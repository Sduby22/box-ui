use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

// --- Platform-specific constants ---

#[cfg(target_os = "macos")]
mod platform {
    pub const SOCKET_PATH: &str = "/var/run/box-ui-helper.sock";
    pub const HELPER_BINARY_PATH: &str = "/Library/PrivilegedHelperTools/box-ui-helper";
    pub const HELPER_LABEL: &str = "com.box-ui.helper";
    pub const PLIST_PATH: &str = "/Library/LaunchDaemons/com.box-ui.helper.plist";
}

#[cfg(target_os = "linux")]
mod platform {
    pub const SOCKET_PATH: &str = "/run/box-ui-helper.sock";
    pub const HELPER_BINARY_PATH: &str = "/usr/local/bin/box-ui-helper";
    pub const SYSTEMD_UNIT: &str = "box-ui-helper.service";
}

#[cfg(target_os = "windows")]
mod platform {
    /// Windows uses named pipes instead of Unix sockets.
    pub const PIPE_NAME: &str = r"\\.\pipe\box-ui-helper";
    pub const SERVICE_NAME: &str = "BoxUIHelper";
}

pub use platform::*;

// --- Request / Response types ---

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    Start {
        kernel_path: String,
        config_path: String,
        /// Working directory for the sing-box process.
        working_dir: String,
        /// UID of the calling user (Unix only, None on Windows).
        uid: Option<u32>,
        /// GID of the calling user (Unix only, None on Windows).
        gid: Option<u32>,
    },
    Stop,
    Status,
    Ping,
    /// Get the helper daemon version.
    Version,
    /// Bind the helper to a GUI process. Helper will exit when this PID dies.
    Bind { pid: u32 },
    /// Self-upgrade: copy the new binary from `binary_path` over the installed helper,
    /// then shutdown so the GUI can kickstart the updated version.
    Upgrade { binary_path: String },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    Ok,
    Error {
        message: String,
    },
    Status {
        running: bool,
        pid: Option<u32>,
    },
    Pong,
    /// Helper daemon version.
    Version {
        version: String,
    },
}

// --- Wire protocol: 4-byte big-endian length prefix + JSON payload ---

pub fn send_message<W: Write>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    let len = data.len() as u32;
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(data)?;
    writer.flush()
}

pub fn recv_message<R: Read>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check: reject messages larger than 16 MB
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("message too large: {len} bytes"),
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn send_request<W: Write>(writer: &mut W, req: &Request) -> std::io::Result<()> {
    let data = serde_json::to_vec(req)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    send_message(writer, &data)
}

pub fn recv_request<R: Read>(reader: &mut R) -> std::io::Result<Request> {
    let data = recv_message(reader)?;
    serde_json::from_slice(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn send_response<W: Write>(writer: &mut W, resp: &Response) -> std::io::Result<()> {
    let data = serde_json::to_vec(resp)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    send_message(writer, &data)
}

pub fn recv_response<R: Read>(reader: &mut R) -> std::io::Result<Response> {
    let data = recv_message(reader)?;
    serde_json::from_slice(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
