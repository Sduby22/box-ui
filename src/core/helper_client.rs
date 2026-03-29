//! Client for communicating with the box-ui-helper daemon over a Unix socket.

use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::time::Duration;

use box_ui_ipc::{Request, Response};

pub struct HelperClient {
    reader: BufReader<Box<dyn std::io::Read + Send>>,
    writer: BufWriter<Box<dyn std::io::Write + Send>>,
}

impl HelperClient {
    /// Connect to the helper daemon.
    pub fn connect() -> Result<Self, String> {
        let (read_half, write_half) = connect_transport(Some(Duration::from_secs(10)))?;
        Ok(Self {
            reader: BufReader::new(read_half),
            writer: BufWriter::new(write_half),
        })
    }

    /// Check if the helper socket exists and is connectable.
    pub fn is_available() -> bool {
        match Self::connect() {
            Ok(mut client) => client.ping().is_ok(),
            Err(_) => false,
        }
    }

    /// Ensure the helper is running, up-to-date, and bound to this GUI process.
    /// If outdated, asks the running helper to self-upgrade (no password needed).
    /// If not running, kickstarts it via the OS service manager.
    /// Returns the helper version string on success.
    pub fn ensure_running() -> Result<String, String> {
        if !Self::is_available() {
            Self::kickstart_and_wait()?;
        }

        // Check version and self-upgrade if needed
        let gui_version = env!("CARGO_PKG_VERSION");
        let mut client = Self::connect()?;
        let needs_upgrade = match client.version() {
            Ok(v) => v != gui_version,
            Err(_) => true,
        };

        if needs_upgrade {
            // Don't upgrade if helper is actively managing a running kernel
            if let Ok((true, _)) = client.status() {
                tracing::info!(
                    "Helper is outdated but managing a running kernel, deferring upgrade"
                );
            } else {
                // Find the new helper binary next to the GUI executable
                let helper_src = std::env::current_exe()
                    .map_err(|e| format!("Cannot determine executable path: {e}"))?
                    .parent()
                    .ok_or("Cannot determine executable directory")?
                    .join("box-ui-helper");

                if helper_src.exists() {
                    tracing::info!("Upgrading helper daemon via IPC");
                    // Try IPC self-upgrade first; fall back to osascript reinstall
                    // (needed for old helpers that don't support the Upgrade command)
                    match client.upgrade(&helper_src) {
                        Ok(()) => {
                            drop(client);
                            std::thread::sleep(Duration::from_millis(500));
                        }
                        Err(e) => {
                            tracing::warn!("IPC upgrade failed ({e}), falling back to reinstall");
                            client.shutdown().ok();
                            drop(client);
                            std::thread::sleep(Duration::from_millis(500));
                            super::helper_install::install_helper()?;
                        }
                    }
                    Self::kickstart_and_wait()?;
                    tracing::info!("Helper daemon upgraded to {gui_version}");
                }
            }
        }

        // Bind this GUI process to the helper (idempotent for same PID)
        let mut client = Self::connect()?;
        let pid = std::process::id();
        client
            .bind(pid)
            .map_err(|e| format!("Failed to bind helper: {e}"))?;

        let version = client.version().unwrap_or_else(|_| "unknown".to_string());
        Ok(version)
    }

    /// Kickstart the helper daemon and wait for it to become available.
    fn kickstart_and_wait() -> Result<(), String> {
        super::helper_install::kickstart_helper()?;

        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if Self::is_available() {
                return Ok(());
            }
        }
        Err("Helper daemon did not start in time".to_string())
    }

    pub fn ping(&mut self) -> Result<(), String> {
        self.send(&Request::Ping)?;
        match self.recv()? {
            Response::Pong => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("Unexpected response: {other:?}")),
        }
    }

    pub fn start(
        &mut self,
        kernel_path: &Path,
        config_path: &Path,
        working_dir: &Path,
    ) -> Result<(), String> {
        self.send(&Request::Start {
            kernel_path: kernel_path.to_string_lossy().to_string(),
            config_path: config_path.to_string_lossy().to_string(),
            working_dir: working_dir.to_string_lossy().to_string(),
            uid: get_uid(),
            gid: get_gid(),
        })?;
        match self.recv()? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("Unexpected response: {other:?}")),
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.send(&Request::Stop)?;
        match self.recv()? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("Unexpected response: {other:?}")),
        }
    }

    pub fn status(&mut self) -> Result<(bool, Option<u32>), String> {
        self.send(&Request::Status)?;
        match self.recv()? {
            Response::Status { running, pid } => Ok((running, pid)),
            Response::Error { message } => Err(message),
            other => Err(format!("Unexpected response: {other:?}")),
        }
    }

    /// Get the helper daemon version.
    pub fn version(&mut self) -> Result<String, String> {
        self.send(&Request::Version)?;
        match self.recv()? {
            Response::Version { version } => Ok(version),
            Response::Error { message } => Err(message),
            other => Err(format!("Unexpected response: {other:?}")),
        }
    }

    /// Bind the helper to the current GUI process PID.
    pub fn bind(&mut self, pid: u32) -> Result<(), String> {
        self.send(&Request::Bind { pid })?;
        match self.recv()? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("Unexpected response: {other:?}")),
        }
    }

    /// Ask the helper to replace its own binary and shutdown for restart.
    pub fn upgrade(&mut self, binary_path: &Path) -> Result<(), String> {
        self.send(&Request::Upgrade {
            binary_path: binary_path.to_string_lossy().to_string(),
        })?;
        match self.recv()? {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(message),
            other => Err(format!("Unexpected response: {other:?}")),
        }
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        self.send(&Request::Shutdown)?;
        match self.recv() {
            Ok(Response::Ok) | Err(_) => Ok(()),
            Ok(Response::Error { message }) => Err(message),
            Ok(other) => Err(format!("Unexpected response: {other:?}")),
        }
    }

    fn send(&mut self, req: &Request) -> Result<(), String> {
        box_ui_ipc::send_request(&mut self.writer, req)
            .map_err(|e| format!("Failed to send request: {e}"))
    }

    fn recv(&mut self) -> Result<Response, String> {
        box_ui_ipc::recv_response(&mut self.reader)
            .map_err(|e| format!("Failed to receive response: {e}"))
    }
}

// --- Platform-specific transport ---

type TransportPair = (
    Box<dyn std::io::Read + Send>,
    Box<dyn std::io::Write + Send>,
);

#[cfg(unix)]
fn connect_transport(read_timeout: Option<Duration>) -> Result<TransportPair, String> {
    use std::os::unix::net::UnixStream;

    let stream =
        UnixStream::connect(box_ui_ipc::SOCKET_PATH).map_err(|e| format!("Cannot connect to helper: {e}"))?;
    stream.set_read_timeout(read_timeout).ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .ok();
    let read_half = stream
        .try_clone()
        .map_err(|e| format!("Clone failed: {e}"))?;
    Ok((Box::new(read_half), Box::new(stream)))
}

#[cfg(windows)]
fn connect_transport(_read_timeout: Option<Duration>) -> Result<TransportPair, String> {
    // TODO: implement Named Pipe client for Windows
    Err("Windows helper client not yet implemented".to_string())
}

#[cfg(not(any(unix, windows)))]
fn connect_transport(_read_timeout: Option<Duration>) -> Result<TransportPair, String> {
    Err("Helper client not supported on this platform".to_string())
}

// --- Platform-specific UID/GID ---

#[cfg(unix)]
fn get_uid() -> Option<u32> {
    Some(unsafe { libc::getuid() })
}

#[cfg(unix)]
fn get_gid() -> Option<u32> {
    Some(unsafe { libc::getgid() })
}

#[cfg(not(unix))]
fn get_uid() -> Option<u32> {
    None
}

#[cfg(not(unix))]
fn get_gid() -> Option<u32> {
    None
}
