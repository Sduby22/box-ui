use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};

/// Tracks how the kernel process is being managed.
pub(crate) enum KernelBackend {
    /// GUI directly owns the child process.
    Direct(Child),
    /// A macOS helper daemon manages the process.
    HelperManaged,
}

pub struct KernelManager {
    kernel_path: Option<PathBuf>,
    pub(crate) backend: Arc<Mutex<Option<KernelBackend>>>,
}

impl KernelManager {
    pub fn new(kernel_path: Option<PathBuf>) -> Self {
        let mgr = Self {
            kernel_path,
            backend: Arc::new(Mutex::new(None)),
        };
        mgr.check_no_stale_helper();
        mgr
    }

    /// On startup, verify no helper daemon is already running.
    /// A running helper means it is bound to a previous GUI process — we must not
    /// take it over. The user should wait for the old helper to exit or kill it manually.
    fn check_no_stale_helper(&self) {
        if !super::helper_install::is_installed() {
            return;
        }
        if super::helper_client::HelperClient::is_available() {
            tracing::error!(
                "A helper daemon is already running (bound to another GUI process). \
                 Cannot start a second instance."
            );
            // Show a native error dialog before exiting
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Box UI")
                .set_description(
                    "A helper daemon is already running and bound to another Box UI instance.\n\
                     Please close the other instance first, or wait for the helper to exit.",
                )
                .show();
            std::process::exit(1);
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

        // When elevated and helper is installed, use helper daemon (all platforms)
        if elevated && super::helper_install::is_installed() {
            return self.start_via_helper(&kernel_path, config_path, working_dir);
        }

        let mut cmd = if elevated {
            build_elevated_command(&kernel_path, config_path, working_dir)?
        } else {
            let mut c = std::process::Command::new(&kernel_path);
            c.arg("run").arg("-c").arg(config_path).current_dir(working_dir);
            c
        };

        let child = cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start kernel: {e}"))?;

        *self.backend.lock().unwrap() = Some(KernelBackend::Direct(child));
        Ok(())
    }

    fn start_via_helper(
        &mut self,
        kernel_path: &std::path::Path,
        config_path: &std::path::Path,
        working_dir: &std::path::Path,
    ) -> Result<(), String> {
        use super::helper_client::HelperClient;

        HelperClient::ensure_running()?;

        let mut client = HelperClient::connect()
            .map_err(|e| format!("Failed to connect to helper: {e}"))?;

        let pid = std::process::id();
        client.bind(pid).map_err(|e| format!("Failed to bind helper: {e}"))?;

        client.start(kernel_path, config_path, working_dir)?;

        *self.backend.lock().unwrap() = Some(KernelBackend::HelperManaged);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        let mut backend = self.backend.lock().unwrap();
        match backend.take() {
            Some(KernelBackend::Direct(mut child)) => {
                child
                    .kill()
                    .map_err(|e| format!("Failed to stop kernel: {e}"))?;
                child.wait().ok();
                Ok(())
            }
            Some(KernelBackend::HelperManaged) => {
                drop(backend); // Release lock before network call
                self.stop_via_helper()
            }
            None => Err("Kernel is not running".to_string()),
        }
    }

    fn stop_via_helper(&mut self) -> Result<(), String> {
        use super::helper_client::HelperClient;
        let mut client = HelperClient::connect()?;
        client.stop()?;
        *self.backend.lock().unwrap() = None;
        Ok(())
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
        match &mut *backend {
            Some(KernelBackend::Direct(child)) => match child.try_wait() {
                Ok(Some(_)) => {
                    *backend = None;
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            },
            Some(KernelBackend::HelperManaged) => {
                // Check via helper client (non-blocking attempt)
                drop(backend); // Release lock before network call
                self.check_helper_status()
            }
            None => false,
        }
    }

    fn check_helper_status(&self) -> bool {
        use super::helper_client::HelperClient;
        match HelperClient::connect() {
            Ok(mut client) => match client.status() {
                Ok((running, _)) => {
                    if !running {
                        *self.backend.lock().unwrap() = None;
                    }
                    running
                }
                Err(_) => {
                    *self.backend.lock().unwrap() = None;
                    false
                }
            },
            Err(_) => {
                *self.backend.lock().unwrap() = None;
                false
            }
        }
    }
}

/// Stop kernel and shut down helper given only the shared backend Arc.
/// Used by both `KernelManager::shutdown_cleanup` and the tray quit handler.
pub fn shutdown_backend(backend: &Arc<Mutex<Option<KernelBackend>>>) {
    match backend.lock().unwrap().take() {
        Some(KernelBackend::Direct(mut child)) => {
            child.kill().ok();
            child.wait().ok();
        }
        Some(KernelBackend::HelperManaged) => {
            if let Ok(mut client) = super::helper_client::HelperClient::connect() {
                client.stop().ok();
                client.shutdown().ok();
            }
        }
        None => {
            if let Ok(mut client) = super::helper_client::HelperClient::connect() {
                client.shutdown().ok();
            }
        }
    }
}

/// Build a `Command` that runs the kernel binary with elevated (root/admin) privileges.
/// Used as fallback when the helper daemon is not installed.
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
        let script = format!(
            "Start-Process -FilePath '{}' -ArgumentList 'run','-c','{}' -Verb RunAs -Wait -NoNewWindow -WorkingDirectory '{}'",
            kernel_path.display(),
            config_path.display(),
            working_dir.display(),
        );
        let mut cmd = std::process::Command::new("powershell");
        cmd.arg("-Command").arg(&script);
        Ok(cmd)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("Elevated execution is not supported on this platform".to_string())
    }
}
