#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod core;
mod fonts;
mod ui;

use eframe::egui;

fn main() -> eframe::Result<()> {
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

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let _guard = rt.enter();

    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/1024.png"))
            .expect("failed to decode app icon");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([700.0, 450.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    eframe::run_native(
        "Box UI",
        options,
        Box::new(|cc| Ok(Box::new(app::BoxApp::new(cc)))),
    )
}
