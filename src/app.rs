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

/// Upper bound on queued toasts. The queue is only pruned while frames render;
/// when the window is hidden the app runs at 0 FPS, so background tasks must
/// not be able to grow it without limit.
const MAX_TOASTS: usize = 16;

pub fn push_toast(toasts: &Toasts, kind: ToastKind, message: String) {
    let toast = Toast {
        message,
        kind,
        expires_at: std::time::Instant::now() + std::time::Duration::from_secs(5),
    };
    let mut toasts = toasts.lock().unwrap();
    if toasts.len() >= MAX_TOASTS {
        toasts.pop_front();
    }
    toasts.push_back(toast);
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
    pub tray_state: Arc<TrayState>,
    pub tray_enabled: bool,
    /// Cached per-frame to avoid repeated Mutex lock + syscall
    pub cached_is_running: bool,
    /// True while the window is hidden to the tray. eframe on Windows cannot
    /// see `Visible(false)` (ViewportInfo never reflects it), so it keeps
    /// running the full UI + render pipeline for hidden windows; this flag is
    /// how the app knows to stop scheduling repaints and skip ui() entirely.
    pub hidden: bool,
}

impl BoxApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        kernel_backend: Arc<Mutex<Option<Child>>>,
        tray_state: Arc<TrayState>,
        tray_enabled: bool,
    ) -> Self {
        let runtime = tokio::runtime::Handle::current();
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("box-ui");
        std::fs::create_dir_all(&data_dir).ok();

        let settings_manager = SettingsManager::new(data_dir);

        crate::fonts::setup_fonts(&cc.egui_ctx);
        install_wgpu_error_handlers(cc, settings_manager.data_dir());

        let kernel_path = settings_manager.active_kernel_path();
        let (clash_api_base, clash_api_secret) = {
            let (addr, secret) = settings_manager.active_clash_api_info();
            (addr.unwrap_or_default(), secret.unwrap_or_default())
        };

        let max_log_lines = settings_manager.max_log_lines();
        let logs_state = ui::logs::LogsState::new(max_log_lines);
        // Fix autostart if exe was moved since registration
        crate::core::autostart::repair_if_needed();
        let settings_state = ui::settings::SettingsState::default();
        let toasts: Toasts = Arc::new(Mutex::new(VecDeque::new()));
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to build configured HTTP client, using default: {e}");
                reqwest::Client::new()
            });

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
            http_client,
            toasts,
            dashboard_state: ui::dashboard::DashboardState::default(),
            outbounds_state: ui::outbounds::OutboundsState::default(),
            connections_state: ui::connections::ConnectionsState::default(),
            logs_state,
            settings_state,
            runtime,
            tray_state,
            tray_enabled,
            cached_is_running: false,
            hidden: false,
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

    fn update_app_background_services(&mut self) {
        self.cached_is_running = self.kernel_manager.is_running();

        if let Some(error_msg) = self.kernel_manager.take_unexpected_exit() {
            push_toast(&self.toasts, ToastKind::Error, error_msg);
        }

        self.process_config_refresh_events();
        ui::dashboard::ensure_config_refresh_task(self);
    }

    fn process_config_refresh_events(&mut self) {
        let refreshed: Vec<uuid::Uuid> = self
            .dashboard_state
            .config
            .refreshed_config_ids
            .lock()
            .unwrap()
            .drain(..)
            .collect();
        if refreshed.is_empty() {
            return;
        }

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

    fn stop_live_monitoring_streams(&mut self) {
        self.dashboard_state
            .traffic
            .polling_flag
            .store(false, Ordering::Relaxed);
        self.dashboard_state.traffic.traffic_polling = false;
        if let Some(handle) = self.dashboard_state.traffic.polling_handle.take() {
            handle.abort();
        }

        self.connections_state
            .streaming_flag
            .store(false, Ordering::Relaxed);
        self.connections_state.streaming = false;
        if let Some(handle) = self.connections_state.streaming_handle.take() {
            handle.abort();
        }

        self.logs_state
            .streaming_flag
            .store(false, Ordering::Relaxed);
        self.logs_state.streaming = false;
        if let Some(handle) = self.logs_state.streaming_handle.take() {
            handle.abort();
        }
    }

    fn stop_all_background_tasks(&mut self) {
        self.stop_live_monitoring_streams();
        ui::dashboard::stop_config_refresh_task(self);
    }

    fn release_hidden_memory(&mut self, ctx: &egui::Context) {
        if !self.settings_manager.release_memory_on_hide() {
            return;
        }

        // Clear heavy application state to free memory while hidden.
        self.dashboard_state
            .traffic
            .traffic_history
            .lock()
            .unwrap()
            .clear();
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
            unsafe {
                EmptyWorkingSet(GetCurrentProcess());
            }
        }
        #[cfg(target_os = "linux")]
        {
            // glibc: release free heap pages back to the kernel.
            unsafe {
                libc::malloc_trim(0);
            }
        }
    }
}

fn install_wgpu_error_handlers(cc: &eframe::CreationContext<'_>, data_dir: &std::path::Path) {
    let Some(render_state) = cc.wgpu_render_state.as_ref() else {
        return;
    };

    let diagnostics_dir = data_dir.to_path_buf();
    render_state
        .device
        .on_uncaptured_error(Arc::new(move |err| {
            let message = format!("wgpu uncaptured error: {err:?}");
            tracing::warn!("{message}");
            crate::core::diagnostics::append_line(&diagnostics_dir, &message);
        }));

    let diagnostics_dir = data_dir.to_path_buf();
    render_state
        .device
        .set_device_lost_callback(move |reason, message| {
            let message = format!("wgpu device lost: {reason:?}; {message}");
            tracing::warn!("{message}");
            crate::core::diagnostics::append_line(&diagnostics_dir, &message);
            // A real device loss (driver update/reset) in a hardware session is
            // unrecoverable in-process (egui-wgpu panics, release aborts), so
            // arm the stable-graphics fallback for the next launch. `Destroyed`
            // is normal shutdown and must not arm it.
            if matches!(reason, eframe::wgpu::DeviceLostReason::Unknown)
                && crate::core::diagnostics::hardware_graphics_active()
            {
                crate::core::diagnostics::write_gpu_crash_marker(&diagnostics_dir);
            }
        });
}

impl eframe::App for BoxApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.tray_state.quit_requested.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Tray "Show" fired: leave the hidden state before this frame renders.
        // A focused window is also proof we are visible, whatever set the flag.
        if self.tray_state.show_requested.swap(false, Ordering::Relaxed)
            || (self.hidden && ctx.input(|i| i.viewport().focused == Some(true)))
        {
            self.hidden = false;
        }

        self.update_app_background_services();

        // Handle window close here rather than in ui(): eframe 0.35 skips ui()
        // for invisible/minimized/occluded windows, but always runs logic()
        // whenever a repaint is requested. Tray Quit closes an already-hidden
        // window, so the close/CancelClose handling must live where it is
        // guaranteed to run. It runs after update_app_background_services so the
        // real-quit branch's stop_all_background_tasks is not immediately undone
        // by the config refresh task being re-ensured this same frame.
        //
        // On macOS, letting eframe::run_native return terminates the
        // NSApplication event loop, which makes the tray icon unresponsive, so
        // when a tray is available we cancel the close and hide instead. When
        // release_memory_on_hide is enabled we also clear heavy application
        // state and egui caches to reduce memory while hidden.
        if ctx.input(|i| i.viewport().close_requested()) {
            let quit_requested = self
                .tray_state
                .quit_requested
                .swap(false, Ordering::Relaxed);
            self.stop_live_monitoring_streams();

            if self.tray_enabled && !quit_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.hidden = true;
                self.release_hidden_memory(ctx);
            } else {
                self.stop_all_background_tasks();
            }
        }

        // While hidden, schedule nothing: every repaint of a hidden window would
        // still run logic(), and (when visible) the full render pipeline. Tray
        // Show/Quit wake the loop with their own request_repaint.
        if !self.hidden {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Hidden to tray: build nothing. eframe 0.35 already skips ui() for
        // invisible windows; this guard also covers the same frame in which
        // logic() just hid the window (before the Visible(false) command lands)
        // and stops tab panels from reviving the websocket/HTTP streams that the
        // close handler in logic() just tore down.
        if self.hidden {
            return;
        }

        let ctx = root_ui.ctx().clone();

        // Sync & start traffic polling globally (sidebar needs live speed data)
        self.dashboard_state.traffic.traffic_polling = self
            .dashboard_state
            .traffic
            .polling_flag
            .load(Ordering::Relaxed);
        if self.cached_is_running && !self.dashboard_state.traffic.traffic_polling {
            ui::dashboard::start_traffic_polling(self);
        }

        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(140.0)
            .show(root_ui, |ui| {
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
                        ui.label(format!("↓ {}", crate::core::format_speed(down)));
                        ui.label(format!("↑ {}", crate::core::format_speed(up)));

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

        egui::CentralPanel::default().show(root_ui, |ui| match self.current_tab {
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

impl Drop for BoxApp {
    fn drop(&mut self) {
        self.stop_all_background_tasks();
        let _ = self.kernel_manager.stop();
    }
}
