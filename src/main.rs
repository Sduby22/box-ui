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

#[cfg(feature = "heap-profile")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Global profiler handle so the tray quit handler can flush data before exit.
#[cfg(feature = "heap-profile")]
pub static HEAP_PROFILER: std::sync::Mutex<Option<dhat::Profiler>> = std::sync::Mutex::new(None);

/// Shared state between the tray event thread and the main/window loops.
pub struct TrayState {
    /// Signal from tray "Show" action to create a new window.
    pub show_requested: AtomicBool,
    /// egui context when a window is active (allows tray thread to focus an existing window).
    pub egui_ctx: Mutex<Option<egui::Context>>,
}

fn main() -> eframe::Result<()> {
    // dhat records every heap allocation. The profiler is stored globally so
    // the tray quit handler (which calls std::process::exit) can drop it first,
    // ensuring dhat-heap.json is written to the data directory.
    #[cfg(feature = "heap-profile")]
    {
        let output_path = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("box-ui")
            .join("dhat-heap.json");
        let profiler = dhat::Profiler::builder()
            .file_name(output_path.clone())
            .trim_backtraces(None)
            .build();
        *HEAP_PROFILER.lock().unwrap() = Some(profiler);
        eprintln!("dhat: will write to {}", output_path.display());
    }
    #[cfg(feature = "profile")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        tracing_subscriber::registry()
            .with(tracing_tracy::TracyLayer::default())
            .init();
    }
    #[cfg(not(feature = "profile"))]
    tracing_subscriber::fmt::init();

    core::platform::setup_child_process_cleanup();

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

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("box-ui");
    std::fs::create_dir_all(&data_dir).ok();

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

    // Build tray icon once — persists for the entire process lifetime.
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::TrayIconBuilder;

    let tray_menu = Menu::new();
    let show_item = MenuItem::new("Show", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    tray_menu
        .append(&show_item)
        .expect("failed to add tray menu item");
    tray_menu
        .append(&quit_item)
        .expect("failed to add tray menu item");

    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();

    // Reuse the shared 128x128 PNG for the tray icon (already small enough).
    let icon_data =
        eframe::icon_data::from_png_bytes(APP_ICON_PNG).expect("failed to decode tray icon PNG");
    let tray_icon_image =
        tray_icon::Icon::from_rgba(icon_data.rgba, icon_data.width, icon_data.height)
            .expect("failed to create tray icon");

    // Dropping the tray icon removes it from the system tray — keep it alive.
    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_icon(tray_icon_image)
        .with_tooltip("Box UI")
        .build()
        .expect("failed to build tray icon");

    let tray_state = Arc::new(TrayState {
        show_requested: AtomicBool::new(true), // show window on first launch
        egui_ctx: Mutex::new(None),
    });

    // Background thread to handle tray menu events.
    {
        let tray = tray_state.clone();
        let backend = kernel_backend.clone();
        std::thread::Builder::new()
            .name("tray-events".into())
            .spawn(move || {
                let menu_rx = MenuEvent::receiver();
                loop {
                    if let Ok(event) = menu_rx.try_recv() {
                        if event.id() == &show_id {
                            let ctx_guard = tray.egui_ctx.lock().unwrap();
                            if let Some(ctx) = ctx_guard.as_ref() {
                                // Window already exists — just focus it.
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                                ctx.request_repaint();
                            } else {
                                drop(ctx_guard);
                                tray.show_requested.store(true, Ordering::Relaxed);
                            }
                        } else if event.id() == &quit_id {
                            core::kernel::shutdown_backend(&backend);
                            #[cfg(feature = "heap-profile")]
                            {
                                let _ = crate::HEAP_PROFILER.lock().unwrap().take();
                            }
                            std::process::exit(0);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            })
            .expect("failed to spawn tray event thread");
    }

    // Main window lifecycle loop.
    // Each iteration creates an eframe window; when the user closes it, eframe
    // returns and ALL egui/eframe memory is freed. Only the tray icon and the
    // kernel child process survive. On tray "Show", a fresh window is created.
    loop {
        // Wait for a show signal (first iteration starts immediately).
        while !tray_state.show_requested.swap(false, Ordering::Relaxed) {
            pump_platform_messages();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let icon =
            eframe::icon_data::from_png_bytes(APP_ICON_PNG).expect("failed to decode app icon");
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([900.0, 600.0])
                .with_min_inner_size([700.0, 450.0])
                .with_icon(std::sync::Arc::new(icon)),
            ..Default::default()
        };

        let backend = kernel_backend.clone();
        let tray = tray_state.clone();
        if let Err(e) = eframe::run_native(
            "Box UI",
            options,
            Box::new(move |cc| Ok(Box::new(app::BoxApp::new(cc, backend, tray)))),
        ) {
            tracing::error!("eframe error: {e}");
        }

        // Window was destroyed — clear egui context so the tray thread knows
        // to signal a new window instead of trying to focus a dead one.
        *tray_state.egui_ctx.lock().unwrap() = None;
    }
}

/// Pump platform messages to keep the tray icon responsive between eframe sessions.
/// On Windows, tray-icon uses a hidden HWND whose messages must be dispatched.
/// On other platforms this is a no-op (tray-icon uses D-Bus / platform APIs
/// that handle events internally).
#[inline]
fn pump_platform_messages() {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
