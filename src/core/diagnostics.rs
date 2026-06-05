use std::io::Write;

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
    }));
}
