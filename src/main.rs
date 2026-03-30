#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod core;
mod fonts;
mod ui;

use eframe::egui;

/// Shared 128x128 app icon PNG, embedded at compile time.
/// Used by both the window icon and the tray icon to avoid decoding twice.
pub static APP_ICON_PNG: &[u8] = include_bytes!("../assets/icons/128.png");

#[cfg(feature = "heap-profile")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Global profiler handle so the tray quit handler can flush data before exit.
#[cfg(feature = "heap-profile")]
pub static HEAP_PROFILER: std::sync::Mutex<Option<dhat::Profiler>> = std::sync::Mutex::new(None);

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

    let icon =
        eframe::icon_data::from_png_bytes(APP_ICON_PNG)
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
