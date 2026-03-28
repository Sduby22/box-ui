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

    /// Ensure the helper is running. If not, try to kickstart it via the OS service manager.
    pub fn ensure_running() -> Result<(), String> {
        if Self::is_available() {
            return Ok(());
        }

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
