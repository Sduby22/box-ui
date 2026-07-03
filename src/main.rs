#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod core;
mod fonts;
mod ui;

use eframe::egui;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Shared 128x128 app icon PNG, embedded at compile time.
/// Used by both the window icon and the tray icon to avoid decoding twice.
pub static APP_ICON_PNG: &[u8] = include_bytes!("../assets/icons/128.png");

/// Shared state between the tray event thread and the eframe window.
pub struct TrayState {
    /// egui context for the window (allows tray thread to show/focus it).
    pub egui_ctx: Mutex<Option<egui::Context>>,
    /// Set by tray Quit so the close handler allows a real app shutdown.
    pub quit_requested: AtomicBool,
    /// Set by tray Show so the app clears its hidden state and resumes repaints.
    pub show_requested: AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphicsMode {
    Hardware,
    Stable,
}

impl GraphicsMode {
    fn from_env() -> Option<Self> {
        match std::env::var("BOX_UI_GRAPHICS")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("hardware" | "gpu" | "wgpu") => Some(Self::Hardware),
            Some("stable" | "software" | "cpu" | "warp") => Some(Self::Stable),
            _ => None,
        }
    }

    /// Hardware rendering by default. A GPU device loss (driver update/reset)
    /// in a previous hardware session leaves a crash marker; consume it and
    /// run this launch on the stable software adapter instead so the app is
    /// usable while the driver settles. An explicit env override wins.
    fn resolve(data_dir: &std::path::Path) -> Self {
        if let Some(mode) = Self::from_env() {
            return mode;
        }
        if core::diagnostics::take_gpu_crash_marker(data_dir) {
            core::diagnostics::append_line(
                data_dir,
                "previous hardware session lost the GPU device; using stable graphics for this launch",
            );
            return Self::Stable;
        }
        Self::Hardware
    }
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();

    core::platform::setup_child_process_cleanup();

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("box-ui");
    std::fs::create_dir_all(&data_dir).ok();
    core::diagnostics::install_panic_hook(data_dir.clone());

    // Single-threaded runtime: this GUI app only needs one async worker thread
    // instead of the default N (one per CPU core), saving ~2MB stack per thread.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");
    let handle = rt.handle().clone();
    // Drive the runtime on a dedicated background thread so spawned tasks are polled.
    std::thread::Builder::new()
        .name("tokio-rt".into())
        .spawn(move || rt.block_on(std::future::pending::<()>()))
        .expect("failed to spawn tokio runtime thread");
    let _guard = handle.enter();

    // Kernel child process handle persists across window destroy/recreate cycles.
    let kernel_backend: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

    // Auto-elevate and auto-start kernel before any window is created.
    {
        let settings = core::settings::SettingsManager::new(data_dir.clone());
        #[cfg(target_os = "windows")]
        if settings.launch_core_on_start()
            && settings.run_elevated()
            && !core::permissions::is_elevated()
            && let Err(e) = core::permissions::relaunch_elevated()
        {
            tracing::warn!("Failed to relaunch as admin: {e}");
        }
        if settings.launch_core_on_start()
            && let Some(config) = settings.active_config()
        {
            let path = config.path.clone();
            let working_dir = settings.working_dir().to_path_buf();
            let elevated = settings.run_elevated();
            let mut km = core::kernel::KernelManager::with_backend(
                settings.active_kernel_path(),
                kernel_backend.clone(),
            );
            if let Err(e) = km.start(&path, &working_dir, elevated) {
                tracing::warn!("Auto-start kernel failed: {e}");
            }
        }
    }

    let tray_state = Arc::new(TrayState {
        egui_ctx: Mutex::new(None),
        quit_requested: AtomicBool::new(false),
        show_requested: AtomicBool::new(false),
    });
    let _tray_icon = setup_tray(tray_state.clone(), kernel_backend.clone());
    let tray_enabled = _tray_icon.is_some();

    let graphics_mode = GraphicsMode::resolve(&data_dir);
    let mut result = run_box_ui(
        graphics_mode,
        kernel_backend.clone(),
        tray_state.clone(),
        tray_enabled,
    );
    if result.is_err() && graphics_mode == GraphicsMode::Hardware {
        core::diagnostics::append_line(
            &data_dir,
            "hardware graphics startup failed; retrying with stable graphics mode",
        );
        result = run_box_ui(
            GraphicsMode::Stable,
            kernel_backend.clone(),
            tray_state.clone(),
            tray_enabled,
        );
    }
    result
}

fn run_box_ui(
    graphics_mode: GraphicsMode,
    kernel_backend: Arc<Mutex<Option<Child>>>,
    tray_state: Arc<TrayState>,
    tray_enabled: bool,
) -> eframe::Result<()> {
    // When the tray is available, close events are cancelled by the app and the
    // window is hidden instead. This keeps the platform event loop alive so the
    // tray icon remains responsive.
    let icon = eframe::icon_data::from_png_bytes(APP_ICON_PNG).expect("failed to decode app icon");
    let options = native_options(icon, graphics_mode);
    // Lets the panic hook / device-lost callback arm the stable-graphics
    // fallback for the next launch only when a hardware session crashes.
    core::diagnostics::set_hardware_graphics_active(graphics_mode == GraphicsMode::Hardware);
    tracing::info!("Starting UI with {graphics_mode:?} graphics mode");

    eframe::run_native(
        "Box UI",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::BoxApp::new(
                cc,
                kernel_backend,
                tray_state,
                tray_enabled,
            )))
        }),
    )
}

fn native_options(icon: egui::IconData, graphics_mode: GraphicsMode) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([700.0, 450.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    match graphics_mode {
        GraphicsMode::Hardware => {}
        GraphicsMode::Stable => configure_stable_wgpu(&mut options),
    }

    options
}

fn configure_stable_wgpu(options: &mut eframe::NativeOptions) {
    options.renderer = eframe::Renderer::Wgpu;
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(create_new) =
        &mut options.wgpu_options.wgpu_setup
    {
        create_new.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
            select_stable_adapter(adapters, surface)
        }));
    }
}

fn select_stable_adapter(
    adapters: &[eframe::wgpu::Adapter],
    surface: Option<&eframe::wgpu::Surface<'_>>,
) -> Result<eframe::wgpu::Adapter, String> {
    let Some(adapter) = adapters
        .iter()
        .filter(|adapter| adapter_supports_surface(adapter, surface))
        .min_by_key(|adapter| stable_adapter_rank(&adapter.get_info()))
        .cloned()
    else {
        return Err("no compatible wgpu adapter found".to_string());
    };

    let info = adapter.get_info();
    tracing::info!(
        "Selected stable wgpu adapter: {}",
        eframe::egui_wgpu::adapter_info_summary(&info)
    );
    Ok(adapter)
}

fn adapter_supports_surface(
    adapter: &eframe::wgpu::Adapter,
    surface: Option<&eframe::wgpu::Surface<'_>>,
) -> bool {
    surface
        .map(|surface| !surface.get_capabilities(adapter).formats.is_empty())
        .unwrap_or(true)
}

fn stable_adapter_rank(info: &eframe::wgpu::AdapterInfo) -> u8 {
    match info.device_type {
        eframe::wgpu::DeviceType::Cpu => 0,
        eframe::wgpu::DeviceType::IntegratedGpu => 1,
        eframe::wgpu::DeviceType::VirtualGpu => 2,
        eframe::wgpu::DeviceType::Other => 3,
        eframe::wgpu::DeviceType::DiscreteGpu => 4,
    }
}

fn setup_tray(
    tray_state: Arc<TrayState>,
    kernel_backend: Arc<Mutex<Option<Child>>>,
) -> Option<tray_icon::TrayIcon> {
    use tray_icon::TrayIconBuilder;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};

    let tray_menu = Menu::new();
    let show_item = MenuItem::new("Show", true, None);
    let quit_item = MenuItem::new("Quit", true, None);

    if let Err(e) = tray_menu.append(&show_item) {
        tracing::warn!("Failed to add tray show menu item: {e}");
        return None;
    }
    if let Err(e) = tray_menu.append(&quit_item) {
        tracing::warn!("Failed to add tray quit menu item: {e}");
        return None;
    }

    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();

    let icon_data = match eframe::icon_data::from_png_bytes(APP_ICON_PNG) {
        Ok(icon) => icon,
        Err(e) => {
            tracing::warn!("Failed to decode tray icon: {e}");
            return None;
        }
    };
    let tray_icon_image =
        match tray_icon::Icon::from_rgba(icon_data.rgba, icon_data.width, icon_data.height) {
            Ok(icon) => icon,
            Err(e) => {
                tracing::warn!("Failed to create tray icon: {e}");
                return None;
            }
        };

    let tray_icon = match TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_icon(tray_icon_image)
        .with_tooltip("Box UI")
        .build()
    {
        Ok(icon) => icon,
        Err(e) => {
            tracing::warn!("Failed to build tray icon; close button will quit instead: {e}");
            return None;
        }
    };

    if let Err(e) = std::thread::Builder::new()
        .name("tray-events".into())
        .spawn(move || {
            let menu_rx = MenuEvent::receiver();
            // Block on recv() instead of busy-polling with try_recv + sleep.
            // Zero CPU usage while idle, instant response on menu click.
            while let Ok(event) = menu_rx.recv() {
                if event.id() == &show_id {
                    // Clone ctx and drop the lock before calling egui methods.
                    let ctx_opt = match tray_state.egui_ctx.lock() {
                        Ok(ctx) => ctx.clone(),
                        Err(e) => {
                            tracing::warn!("Tray state lock was poisoned: {e}");
                            None
                        }
                    };
                    if let Some(ctx) = ctx_opt {
                        // Order matters: the flag must be set before the wake
                        // repaint so the frame it triggers observes it.
                        tray_state.show_requested.store(true, Ordering::Relaxed);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.request_repaint();
                    }
                } else if event.id() == &quit_id {
                    tray_state.quit_requested.store(true, Ordering::Relaxed);
                    let ctx_opt = match tray_state.egui_ctx.lock() {
                        Ok(ctx) => ctx.clone(),
                        Err(e) => {
                            tracing::warn!("Tray state lock was poisoned: {e}");
                            None
                        }
                    };
                    if let Some(ctx) = ctx_opt {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ctx.request_repaint();
                    } else {
                        core::kernel::shutdown_backend(&kernel_backend);
                        std::process::exit(0);
                    }
                }
            }
        })
    {
        tracing::warn!("Failed to spawn tray event thread: {e}");
        return None;
    }

    Some(tray_icon)
}
