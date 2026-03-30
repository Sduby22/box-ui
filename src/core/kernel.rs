use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};

pub struct KernelManager {
    kernel_path: Option<PathBuf>,
    pub(crate) backend: Arc<Mutex<Option<Child>>>,
    /// Stores an error message when the kernel exits unexpectedly.
    last_unexpected_exit: Mutex<Option<String>>,
    /// Captured stderr handle (only for elevated wrapper processes).
    stderr: Mutex<Option<std::process::ChildStderr>>,
}

impl KernelManager {
    #[allow(dead_code)]
    pub fn new(kernel_path: Option<PathBuf>) -> Self {
        Self {
            kernel_path,
            backend: Arc::new(Mutex::new(None)),
            last_unexpected_exit: Mutex::new(None),
            stderr: Mutex::new(None),
        }
    }

    /// Create a KernelManager that reuses an existing shared backend.
    /// Used to preserve a running kernel process across window destroy/recreate cycles.
    pub fn with_backend(kernel_path: Option<PathBuf>, backend: Arc<Mutex<Option<Child>>>) -> Self {
        Self {
            kernel_path,
            backend,
            last_unexpected_exit: Mutex::new(None),
            stderr: Mutex::new(None),
        }
    }

    pub fn set_kernel_path(&mut self, path: Option<PathBuf>) {
        self.kernel_path = path;
    }

    pub fn start(
        &mut self,
        config_path: &PathBuf,
        working_dir: &std::path::Path,
        elevated: bool,
    ) -> Result<(), String> {
        if self.is_running() {
            return Err("Kernel is already running".to_string());
        }

        let kernel_path = self
            .kernel_path
            .as_ref()
            .ok_or("No kernel binary configured")?
            .clone();

        // On first elevated start, try to grant persistent permissions (setuid / elevation)
        // so subsequent starts don't need a password prompt.
        if elevated && !super::permissions::has_kernel_permissions(&kernel_path) {
            super::permissions::grant_kernel_permissions(&kernel_path)?;
        }

        let needs_elevation_wrapper = elevated && !super::permissions::has_kernel_permissions(&kernel_path);

        let mut cmd = if needs_elevation_wrapper {
            build_elevated_command(&kernel_path, config_path, working_dir)?
        } else {
            let mut c = std::process::Command::new(&kernel_path);
            c.arg("run").arg("-c").arg(config_path).current_dir(working_dir);
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                c.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            c
        };

        // Pipe stderr for elevated wrapper processes (short-lived, safe to buffer).
        // For non-elevated direct starts, discard stderr to avoid pipe buffer filling
        // up from a long-running kernel.
        let stderr_mode = if needs_elevation_wrapper {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        };

        let mut child = cmd
            .stdout(std::process::Stdio::null())
            .stderr(stderr_mode)
            .spawn()
            .map_err(|e| format!("Failed to start kernel: {e}"))?;

        *self.stderr.lock().unwrap() = if needs_elevation_wrapper {
            child.stderr.take()
        } else {
            None
        };
        *self.backend.lock().unwrap() = Some(child);
        *self.last_unexpected_exit.lock().unwrap() = None;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        let mut backend = self.backend.lock().unwrap();
        match backend.take() {
            Some(mut child) => {
                child
                    .kill()
                    .map_err(|e| format!("Failed to stop kernel: {e}"))?;
                child.wait().ok();
                Ok(())
            }
            None => Err("Kernel is not running".to_string()),
        }
    }

    pub fn restart(
        &mut self,
        config_path: &PathBuf,
        working_dir: &std::path::Path,
        elevated: bool,
    ) -> Result<(), String> {
        self.stop().ok();
        self.start(config_path, working_dir, elevated)
    }

    pub fn is_running(&self) -> bool {
        let mut backend = self.backend.lock().unwrap();
        if let Some(child) = &mut *backend {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut error_msg = format!("Kernel exited unexpectedly ({status})");
                    if let Some(mut err) = self.stderr.lock().unwrap().take() {
                        use std::io::Read;
                        let mut buf = vec![0u8; 4096];
                        if let Ok(n) = err.read(&mut buf) {
                            let text = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                            if !text.is_empty() {
                                error_msg = format!("{error_msg}: {text}");
                            }
                        }
                    }
                    *self.last_unexpected_exit.lock().unwrap() = Some(error_msg);
                    *backend = None;
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Drain the last unexpected exit error, if any.
    /// Called per frame by the UI to show error toasts.
    pub fn take_unexpected_exit(&self) -> Option<String> {
        self.last_unexpected_exit.lock().unwrap().take()
    }
}

/// Stop kernel given only the shared backend Arc.
/// Used by the tray quit handler.
pub fn shutdown_backend(backend: &Arc<Mutex<Option<Child>>>) {
    if let Some(mut child) = backend.lock().unwrap().take() {
        child.kill().ok();
        child.wait().ok();
    }
}

/// Build a `Command` that runs the kernel binary with elevated (root/admin) privileges.
/// Used as fallback when the kernel binary does not have setuid (macOS/Linux only).
fn build_elevated_command(
    kernel_path: &std::path::Path,
    config_path: &std::path::Path,
    working_dir: &std::path::Path,
) -> Result<std::process::Command, String> {
    #[cfg(target_os = "macos")]
    {
        let askpass_dir = std::env::temp_dir().join("box-ui");
        std::fs::create_dir_all(&askpass_dir)
            .map_err(|e| format!("Failed to create temp dir: {e}"))?;
        let askpass_path = askpass_dir.join("askpass.sh");
        std::fs::write(
            &askpass_path,
            "#!/bin/bash\nosascript -e 'display dialog \"Box UI needs administrator privileges to start sing-box kernel.\" default answer \"\" with hidden answer with title \"Box UI\"' -e 'text returned of result' 2>/dev/null\n",
        )
        .map_err(|e| format!("Failed to write askpass helper: {e}"))?;

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&askpass_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to set askpass permissions: {e}"))?;

        let mut cmd = std::process::Command::new("sudo");
        cmd.env("SUDO_ASKPASS", &askpass_path)
            .arg("-A")
            .arg(kernel_path)
            .arg("run")
            .arg("-c")
            .arg(config_path)
            .current_dir(working_dir);
        Ok(cmd)
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = std::process::Command::new("pkexec");
        cmd.arg(kernel_path)
            .arg("run")
            .arg("-c")
            .arg(config_path)
            .current_dir(working_dir);
        Ok(cmd)
    }

    #[cfg(target_os = "windows")]
    {
        _ = (kernel_path, config_path, working_dir);
        // On Windows, the GUI should already be running elevated.
        // If we reach here, the user hasn't relaunched as admin.
        Err("Please relaunch Box UI as administrator to run the kernel with elevated privileges".to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        _ = (kernel_path, config_path, working_dir);
        Err("Elevated execution is not supported on this platform".to_string())
    }
}
