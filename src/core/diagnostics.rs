use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// Marker file left behind when a hardware-graphics session loses the GPU
/// device (driver update/reset). Its presence makes the next launch use the
/// stable (software) graphics mode once, then it is consumed.
const GPU_CRASH_MARKER: &str = "gpu-crash-marker";

/// True while the current session renders on a hardware adapter. Read by the
/// panic hook and the wgpu device-lost callback to decide whether a crash
/// should arm the stable-graphics fallback for the next launch.
static HARDWARE_GRAPHICS_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_hardware_graphics_active(active: bool) {
    HARDWARE_GRAPHICS_ACTIVE.store(active, Ordering::Relaxed);
}

pub fn hardware_graphics_active() -> bool {
    HARDWARE_GRAPHICS_ACTIVE.load(Ordering::Relaxed)
}

/// Arm the stable-graphics fallback for the next launch.
pub fn write_gpu_crash_marker(data_dir: &std::path::Path) {
    if let Err(e) = std::fs::write(data_dir.join(GPU_CRASH_MARKER), b"") {
        tracing::warn!("Failed to write GPU crash marker: {e}");
    }
}

/// Consume the crash marker: returns true if the previous hardware session
/// died from a GPU device loss. The marker is deleted so a single healthy
/// session restores hardware rendering afterwards.
pub fn take_gpu_crash_marker(data_dir: &std::path::Path) -> bool {
    let path = data_dir.join(GPU_CRASH_MARKER);
    if path.exists() {
        std::fs::remove_file(&path).ok();
        return true;
    }
    false
}

/// Best-effort append-only diagnostics for GUI-subsystem builds where stderr is hidden.
pub fn append_line(data_dir: &std::path::Path, message: &str) {
    let path = data_dir.join("diagnostics.log");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "[{ts}] {message}");
}

pub fn install_panic_hook(data_dir: std::path::PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");
        let backtrace = std::backtrace::Backtrace::force_capture();
        append_line(
            &data_dir,
            &format!("panic on thread '{thread_name}': {info}\n{backtrace}"),
        );

        // Backup for the device-lost callback: with panic=abort this hook is
        // the last code guaranteed to run, so a renderer panic in a hardware
        // session also arms the stable-graphics fallback for the next launch.
        if hardware_graphics_active()
            && info
                .location()
                .is_some_and(|loc| loc.file().contains("wgpu"))
        {
            write_gpu_crash_marker(&data_dir);
        }
    }));
}
