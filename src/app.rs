use eframe::egui;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

use crate::core::kernel::{KernelManager, shutdown_backend};
use crate::core::settings::SettingsManager;
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Outbounds,
    Connections,
    Logs,
    Settings,
}

/// A toast notification shown at the bottom of the UI.
#[derive(Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub expires_at: std::time::Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    #[allow(dead_code)]
    Info,
    Success,
    Error,
}

/// Thread-safe toast queue, shared with async tasks.
pub type Toasts = Arc<Mutex<VecDeque<Toast>>>;

pub fn push_toast(toasts: &Toasts, kind: ToastKind, message: String) {
    let toast = Toast {
        message,
        kind,
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(5),
    };
    toasts.lock().unwrap().push_back(toast);
}

pub struct BoxApp {
    pub current_tab: Tab,
    pub kernel_manager: KernelManager,
    pub settings_manager: SettingsManager,
    pub clash_api_base: String,
    pub clash_api_secret: String,
    pub http_client: reqwest::Client,
    pub toasts: Toasts,
    pub dashboard_state: ui::dashboard::DashboardState,
    pub outbounds_state: ui::outbounds::OutboundsState,
    pub connections_state: ui::connections::ConnectionsState,
    pub logs_state: ui::logs::LogsState,
    pub settings_state: ui::settings::SettingsState,
    pub runtime: tokio::runtime::Handle,
    /// Cached per-frame to avoid repeated Mutex lock + syscall
    pub cached_is_running: bool,
    /// Whether the window is currently visible (shared with tray thread)
    pub window_visible: Arc<AtomicBool>,
    /// Keep the tray icon alive for the lifetime of the app
    _tray_icon: tray_icon::TrayIcon,
}

impl BoxApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let runtime = tokio::runtime::Handle::current();
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("box-ui");
        std::fs::create_dir_all(&data_dir).ok();

        let settings_manager = SettingsManager::new(data_dir);

        crate::fonts::setup_fonts(&cc.egui_ctx);

        let kernel_path = settings_manager.active_kernel_path();
        let (clash_api_base, clash_api_secret) = {
            let (addr, secret) = settings_manager.active_clash_api_info();
            (addr.unwrap_or_default(), secret.unwrap_or_default())
        };

        let max_log_lines = settings_manager.max_log_lines();

        let logs_state = ui::logs::LogsState::new(max_log_lines);

        let settings_state = ui::settings::SettingsState::default();

        let toasts: Toasts = Arc::new(Mutex::new(VecDeque::new()));

        let mut kernel_manager = KernelManager::new(kernel_path);

        // Auto-elevate: if both launch-core-on-start and run-as-admin are enabled
        // but the process is not elevated, relaunch immediately with admin privileges.
        #[cfg(target_os = "windows")]
        if settings_manager.launch_core_on_start()
            && settings_manager.run_elevated()
            && !crate::core::permissions::is_elevated()
            && let Err(e) = crate::core::permissions::relaunch_elevated()
        {
            push_toast(
                &toasts,
                ToastKind::Error,
                format!("Failed to relaunch as admin: {e}"),
            );
        }

        // Auto-start kernel if configured
        if settings_manager.launch_core_on_start()
            && let Some(config) = settings_manager.active_config()
        {
            let path = config.path.clone();
            let working_dir = settings_manager.working_dir();
            let elevated = settings_manager.run_elevated();
            if let Err(e) = kernel_manager.start(&path, working_dir, elevated) {
                tracing::warn!("Auto-start kernel failed: {e}");
            }
        }

        // Build system tray
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

        // Decode the app icon PNG and resize for tray use
        let tray_icon_image = image::load_from_memory(include_bytes!("../assets/icons/1024.png"))
            .expect("failed to decode tray icon PNG");
        let tray_size = 32u32;
        let resized =
            tray_icon_image.resize_exact(tray_size, tray_size, image::imageops::FilterType::Lanczos3);
        let rgba = resized.into_rgba8().into_raw();
        let icon = tray_icon::Icon::from_rgba(rgba, tray_size, tray_size)
            .expect("failed to create tray icon");

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_icon(icon)
            .with_tooltip("Box UI")
            .build()
            .expect("failed to build tray icon");

        let window_visible = Arc::new(AtomicBool::new(true));

        // Background thread to handle tray events even when the window is hidden.
        {
            let ctx = cc.egui_ctx.clone();
            let backend_for_tray = kernel_manager.backend.clone();
            let visible_flag = window_visible.clone();
            std::thread::spawn(move || {
                let menu_rx = MenuEvent::receiver();
                let icon_rx = TrayIconEvent::receiver();
                loop {
                    if let Ok(event) = menu_rx.try_recv() {
                        if event.id() == &show_id {
                            visible_flag.store(true, Ordering::Relaxed);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                            ctx.request_repaint();
                        } else if event.id() == &quit_id {
                            shutdown_backend(&backend_for_tray);
                            std::process::exit(0);
                        }
                    }
                    // Drain icon events (hover, click, etc.) without acting on them;
                    // all actions go through the menu.
                    while icon_rx.try_recv().is_ok() {}

                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });
        }

        Self {
            current_tab: Tab::Dashboard,
            kernel_manager,
            settings_manager,
            clash_api_base,
            clash_api_secret,
            http_client: reqwest::Client::new(),
            toasts,
            dashboard_state: ui::dashboard::DashboardState::default(),
            outbounds_state: ui::outbounds::OutboundsState::default(),
            connections_state: ui::connections::ConnectionsState::default(),
            logs_state,
            settings_state,
            runtime,
            cached_is_running: false,
            window_visible,
            _tray_icon: tray_icon,
        }
    }

    /// Re-derive the Clash API base URL and secret from the active config.
    pub fn refresh_clash_api_base(&mut self) {
        let (addr, secret) = self.settings_manager.active_clash_api_info();
        self.clash_api_base = addr.unwrap_or_default();
        self.clash_api_secret = secret.unwrap_or_default();
    }

    /// Show toast notifications at the bottom of the screen.
    fn show_toasts(&self, ctx: &egui::Context) {
        let now = std::time::Instant::now();

        // Remove expired toasts
        self.toasts.lock().unwrap().retain(|t| t.expires_at > now);

        let toasts = self.toasts.lock().unwrap().clone();
        if toasts.is_empty() {
            return;
        }

        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -20.0])
            .show(ctx, |ui| {
                for toast in &toasts {
                    let (bg, text_color) = match toast.kind {
                        ToastKind::Info => (
                            egui::Color32::from_rgba_premultiplied(50, 50, 50, 220),
                            egui::Color32::WHITE,
                        ),
                        ToastKind::Success => (
                            egui::Color32::from_rgba_premultiplied(20, 80, 40, 220),
                            egui::Color32::from_rgb(150, 255, 150),
                        ),
                        ToastKind::Error => (
                            egui::Color32::from_rgba_premultiplied(100, 20, 20, 220),
                            egui::Color32::from_rgb(255, 150, 150),
                        ),
                    };
                    egui::Frame::new()
                        .fill(bg)
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::same(8))
                        .outer_margin(egui::Margin::same(2))
                        .show(ui, |ui| {
                            ui.colored_label(text_color, &toast.message);
                        });
                }
            });
    }
}

impl eframe::App for BoxApp {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();

        // Handle window close: hide to tray instead of quitting
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_visible.store(false, Ordering::Relaxed);
            // Disconnect Clash API WebSockets while hidden
            self.dashboard_state
                .polling_flag
                .store(false, Ordering::Relaxed);
            self.dashboard_state.traffic_polling = false;
            self.connections_state
                .streaming_flag
                .store(false, Ordering::Relaxed);
            self.connections_state.streaming = false;
            self.logs_state
                .streaming_flag
                .store(false, Ordering::Relaxed);
            self.logs_state.streaming = false;
        }

        // Cache is_running once per frame (avoids repeated Mutex lock + try_wait syscall)
        self.cached_is_running = self.kernel_manager.is_running();

        // Show error toast if the kernel exited unexpectedly
        if let Some(error_msg) = self.kernel_manager.take_unexpected_exit() {
            push_toast(&self.toasts, ToastKind::Error, error_msg);
        }

        // Request repaint for real-time updates
        ctx.request_repaint_after(std::time::Duration::from_secs(1));

        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(140.0)
            .show_inside(root_ui, |ui| {
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    ui.heading("Box UI");
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(4.0);

                    let tabs = [
                        (Tab::Dashboard, "📊 Dashboard"),
                        (Tab::Outbounds, "🔀 Outbounds"),
                        (Tab::Connections, "🔗 Connections"),
                        (Tab::Logs, "📋 Logs"),
                        (Tab::Settings, "⚙ Settings"),
                    ];

                    for (tab, label) in tabs {
                        let selected = self.current_tab == tab;
                        if ui.selectable_label(selected, label).clicked() {
                            self.current_tab = tab;
                        }
                    }

                    // Push status to bottom
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        ui.add_space(8.0);

                        // Traffic speed (bottom_up: render download first so it appears below upload)
                        let (up, down) = self.dashboard_state.current_speed();
                        ui.label(format!("↓{}", crate::core::format_speed(down)));
                        ui.label(format!("↑{}", crate::core::format_speed(up)));

                        // Core status
                        let (status_text, status_color) = if self.cached_is_running {
                            ("● Running", egui::Color32::from_rgb(80, 200, 120))
                        } else {
                            ("○ Stopped", egui::Color32::GRAY)
                        };
                        ui.colored_label(status_color, status_text);

                        ui.separator();
                    });
                });
            });

        egui::CentralPanel::default().show_inside(root_ui, |ui| match self.current_tab {
            Tab::Dashboard => ui::dashboard::show(ui, self),
            Tab::Outbounds => ui::outbounds::show(ui, self),
            Tab::Connections => ui::connections::show(ui, self),
            Tab::Logs => ui::logs::show(ui, self),
            Tab::Settings => ui::settings::show(ui, self),
        });

        // Render toasts on top
        self.show_toasts(&ctx);
    }
}
