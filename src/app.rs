use eframe::egui;
use std::collections::VecDeque;
use std::process::Child;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::TrayState;
use crate::core::kernel::KernelManager;
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
}

impl BoxApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        kernel_backend: Arc<Mutex<Option<Child>>>,
        tray_state: Arc<TrayState>,
    ) -> Self {
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

        // Reuse the persistent kernel backend so a running kernel survives window cycles.
        let kernel_manager = KernelManager::with_backend(kernel_path, kernel_backend);

        // Store egui context so the tray thread can focus this window.
        *tray_state.egui_ctx.lock().unwrap() = Some(cc.egui_ctx.clone());

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

        // Single lock: remove expired toasts and render in one pass
        let mut toasts = self.toasts.lock().unwrap();
        toasts.retain(|t| t.expires_at > now);

        if toasts.is_empty() {
            return;
        }

        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -20.0])
            .show(ctx, |ui| {
                for toast in toasts.iter() {
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

        // Handle window close: always cancel and hide instead of destroying.
        // On macOS, letting eframe::run_native return terminates the
        // NSApplication event loop, which makes the tray icon unresponsive.
        // When release_memory_on_hide is enabled, we additionally clear heavy
        // application state and egui caches to reduce memory while hidden.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));

            // Always stop background streams to save CPU while hidden
            self.dashboard_state
                .polling_flag
                .store(false, Ordering::Relaxed);
            self.dashboard_state.traffic_polling = false;
            self.connections_state
                .streaming_flag
                .store(false, Ordering::Relaxed);
            self.logs_state
                .streaming_flag
                .store(false, Ordering::Relaxed);

            if self.settings_manager.release_memory_on_hide() {
                // Clear heavy application state to free memory while hidden.
                self.dashboard_state.traffic_history.lock().unwrap().clear();
                self.connections_state.connections.lock().unwrap().clear();
                self.connections_state.clear_speed_cache();
                self.outbounds_state.groups.lock().unwrap().clear();
                self.outbounds_state.expanded.clear();
                self.outbounds_state.last_fetch = None;
                self.logs_state.entries.lock().unwrap().clear();
                self.toasts.lock().unwrap().clear();
                // Clear egui's internal layout/paint caches.
                ctx.memory_mut(|m| *m = Default::default());

                // Platform-specific: return freed memory to the OS immediately.
                #[cfg(target_os = "windows")]
                {
                    // Evict all pages from the working set; accessed pages will
                    // soft-fault back in on demand.
                    use windows_sys::Win32::System::ProcessStatus::EmptyWorkingSet;
                    use windows_sys::Win32::System::Threading::GetCurrentProcess;
                    unsafe { EmptyWorkingSet(GetCurrentProcess()); }
                }
                #[cfg(target_os = "linux")]
                {
                    // glibc: release free heap pages back to the kernel.
                    unsafe { libc::malloc_trim(0); }
                }
            }
        }

        // Cache is_running once per frame (avoids repeated Mutex lock + try_wait syscall)
        self.cached_is_running = self.kernel_manager.is_running();

        // Sync & start traffic polling globally (sidebar needs live speed data)
        self.dashboard_state.traffic_polling =
            self.dashboard_state.polling_flag.load(Ordering::Relaxed);
        if self.cached_is_running && !self.dashboard_state.traffic_polling {
            ui::dashboard::start_traffic_polling(self);
        }

        // Show error toast if the kernel exited unexpectedly
        if let Some(error_msg) = self.kernel_manager.take_unexpected_exit() {
            push_toast(&self.toasts, ToastKind::Error, error_msg);
        }

        // Handle configs refreshed by the background auto-refresh task.
        // Runs at app level so it works regardless of active tab or window visibility.
        let refreshed: Vec<uuid::Uuid> = self
            .dashboard_state
            .refreshed_config_ids
            .lock()
            .unwrap()
            .drain(..)
            .collect();
        if !refreshed.is_empty() {
            let active_id = self.settings_manager.active_config_id();
            let active_refreshed = refreshed.iter().any(|id| Some(*id) == active_id);
            for id in &refreshed {
                if let Some(config) = self.settings_manager.configs().iter().find(|c| c.id == *id) {
                    push_toast(
                        &self.toasts,
                        ToastKind::Success,
                        format!("Config \"{}\" refreshed", config.name),
                    );
                }
            }
            if active_refreshed {
                self.refresh_clash_api_base();
                ui::dashboard::restart_kernel_if_running(self);
            }
        }

        // Start subscription auto-refresh task if not already running
        if !self.dashboard_state.refresh_task_running.load(Ordering::Relaxed) {
            ui::dashboard::start_config_refresh_task(self);
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
