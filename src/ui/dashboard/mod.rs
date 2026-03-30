mod config;
mod kernel;
mod traffic;

pub use config::{restart_kernel_if_running, start_config_refresh_task};
pub use traffic::start_traffic_polling;

use eframe::egui;

use crate::app::BoxApp;

#[derive(Default)]
pub struct DashboardState {
    pub traffic: traffic::TrafficState,
    pub kernel: kernel::KernelState,
    pub config: config::ConfigState,
}

impl DashboardState {
    pub fn current_speed(&self) -> (f64, f64) {
        self.traffic.current_speed()
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.heading("Dashboard");
    ui.add_space(8.0);

    traffic::show_traffic_chart(ui, app);

    ui.add_space(12.0);

    // Configuration + Core management side by side
    ui.columns(2, |columns| {
        columns[0].group(|ui| {
            config::show_config_manager(ui, app);
        });
        columns[1].group(|ui| {
            kernel::show_kernel_manager(ui, app);
        });
    });

    // Process async completions
    kernel::process_pending_kernel(app);
    config::process_pending_configs(app);

    // Modal windows
    kernel::show_releases_window(ui.ctx(), app);
    config::show_add_config_window(ui.ctx(), app);
    config::show_edit_config_window(ui.ctx(), app);
}
