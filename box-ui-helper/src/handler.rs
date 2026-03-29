//! Shared business logic for the helper daemon.
//! All functions are generic over AsyncRead/AsyncWrite so they work with
//! both Unix sockets (macOS/Linux) and Named pipes (Windows).

use std::path::Path;
use std::sync::Arc;

use box_ui_ipc::Response;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};
use tokio::time::{self, Duration};

pub struct HelperState {
    pub child: Option<Child>,
    pub child_pid: Option<u32>,
    /// The GUI process ID this helper is bound to.
    pub bound_gui_pid: Option<u32>,
}

impl HelperState {
    pub fn new() -> Self {
        Self {
            child: None,
            child_pid: None,
            bound_gui_pid: None,
        }
    }
}

/// Waits up to 10 seconds for a GUI process to bind. Returns when the timeout
/// expires without a bind, signaling the helper should shut down.
pub async fn bind_timeout(state: &Arc<Mutex<HelperState>>) {
    time::sleep(Duration::from_secs(10)).await;

    if state.lock().await.bound_gui_pid.is_some() {
        // Already bound — pend forever (liveness check handles the rest)
        std::future::pending::<()>().await;
    }

    tracing::warn!("No GUI process bound within 10 seconds");
}

/// Periodically checks if the bound GUI process is still alive.
/// Returns when the GUI process has exited, signaling the helper should shut down.
pub async fn gui_liveness_check(state: &Arc<Mutex<HelperState>>) {
    // Wait a bit before starting checks
    time::sleep(Duration::from_secs(2)).await;

    loop {
        time::sleep(Duration::from_secs(2)).await;

        let bound_pid = state.lock().await.bound_gui_pid;
        if let Some(pid) = bound_pid
            && !is_process_alive(pid)
        {
            tracing::info!("Bound GUI process (PID {pid}) is no longer alive");
            return;
        }
    }
}

/// Check if a process with the given PID is still alive.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks existence without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        _ = pid;
        true
    }
}

/// Handle a single client connection. Generic over the read/write halves.
pub async fn handle_client<R, W>(
    mut reader: R,
    mut writer: W,
    state: Arc<Mutex<HelperState>>,
    shutdown: Arc<Notify>,
) where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    loop {
        // Read length prefix
        let mut len_buf = [0u8; 4];
        if reader.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 16 * 1024 * 1024 {
            break;
        }

        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).await.is_err() {
            break;
        }

        let request: box_ui_ipc::Request = match serde_json::from_slice(&buf) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("Invalid request: {e}"),
                };
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
                continue;
            }
        };

        match request {
            box_ui_ipc::Request::Ping => {
                if send_response(&mut writer, &Response::Pong).await.is_err() {
                    break;
                }
            }
            box_ui_ipc::Request::Version => {
                let resp = Response::Version {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                };
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
            }
            box_ui_ipc::Request::Status => {
                let mut st = state.lock().await;
                let running = is_child_running(&mut st);
                let pid = if running { st.child_pid } else { None };
                let resp = Response::Status { running, pid };
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
            }
            box_ui_ipc::Request::Start {
                kernel_path,
                config_path,
                working_dir,
                uid,
                gid,
            } => {
                let resp =
                    handle_start(&state, &kernel_path, &config_path, &working_dir, uid, gid).await;
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
            }
            box_ui_ipc::Request::Stop => {
                let resp = handle_stop(&state).await;
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
            }
            box_ui_ipc::Request::Bind { pid } => {
                let mut st = state.lock().await;
                let resp = if let Some(existing) = st.bound_gui_pid {
                    if existing == pid {
                        // Same PID re-binding — idempotent, just acknowledge
                        Response::Ok
                    } else {
                        Response::Error {
                            message: format!(
                                "Helper is already bound to GUI process PID {existing}"
                            ),
                        }
                    }
                } else {
                    st.bound_gui_pid = Some(pid);
                    tracing::info!("Bound to GUI process PID {pid}");
                    Response::Ok
                };
                drop(st);
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
            }
            box_ui_ipc::Request::Upgrade { binary_path } => {
                let resp = handle_upgrade(&binary_path).await;
                let should_shutdown = matches!(resp, Response::Ok);
                if send_response(&mut writer, &resp).await.is_err() {
                    break;
                }
                if should_shutdown {
                    shutdown.notify_one();
                    break;
                }
            }
            box_ui_ipc::Request::Shutdown => {
                let _ = send_response(&mut writer, &Response::Ok).await;
                shutdown.notify_one();
                break;
            }
        }
    }
}

async fn handle_upgrade(binary_path: &str) -> Response {
    let src = Path::new(binary_path);
    if !src.is_file() {
        return Response::Error {
            message: format!("Source binary not found: {binary_path}"),
        };
    }

    let dest = Path::new(box_ui_ipc::HELPER_BINARY_PATH);

    if let Err(e) = std::fs::copy(src, dest) {
        return Response::Error {
            message: format!("Failed to copy binary: {e}"),
        };
    }

    // Ensure correct permissions on the new binary
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755)) {
            tracing::warn!("Failed to set permissions on updated binary: {e}");
        }
    }

    tracing::info!("Helper binary upgraded from {binary_path}");
    Response::Ok
}

async fn handle_start(
    state: &Arc<Mutex<HelperState>>,
    kernel_path: &str,
    config_path: &str,
    working_dir: &str,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Response {
    let mut st = state.lock().await;

    if is_child_running(&mut st) {
        return Response::Error {
            message: "Kernel is already running".to_string(),
        };
    }

    if !Path::new(kernel_path).is_file() {
        return Response::Error {
            message: format!("Kernel binary not found: {kernel_path}"),
        };
    }

    if !Path::new(config_path).is_file() {
        return Response::Error {
            message: format!("Config file not found: {config_path}"),
        };
    }

    let work_dir = Path::new(working_dir);
    if !work_dir.is_dir() {
        return Response::Error {
            message: format!("Working directory not found: {working_dir}"),
        };
    }

    let mut cmd = Command::new(kernel_path);
    cmd.arg("run")
        .arg("-c")
        .arg(config_path)
        .current_dir(work_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            st.child_pid = pid;
            st.child = Some(child);
            tracing::info!("sing-box started (pid: {pid:?}, working_dir: {working_dir})");

            // Chown files back to calling user (Unix only)
            #[cfg(unix)]
            if let (Some(uid), Some(gid)) = (uid, gid) {
                let wd = working_dir.to_string();
                tokio::spawn(async move {
                    time::sleep(Duration::from_secs(2)).await;
                    super::unix::chown_dir_contents(&wd, uid, gid);
                });
            }

            #[cfg(not(unix))]
            {
                _ = uid;
                _ = gid;
            }

            Response::Ok
        }
        Err(e) => Response::Error {
            message: format!("Failed to start kernel: {e}"),
        },
    }
}

async fn handle_stop(state: &Arc<Mutex<HelperState>>) -> Response {
    let mut st = state.lock().await;
    if let Some(ref mut child) = st.child {
        stop_child(child).await;
        st.child = None;
        st.child_pid = None;
        tracing::info!("sing-box stopped");
        Response::Ok
    } else {
        Response::Error {
            message: "Kernel is not running".to_string(),
        }
    }
}

fn is_child_running(st: &mut HelperState) -> bool {
    if let Some(ref mut child) = st.child {
        match child.try_wait() {
            Ok(Some(_)) => {
                st.child = None;
                st.child_pid = None;
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    } else {
        false
    }
}

pub async fn stop_child(child: &mut Child) {
    #[cfg(unix)]
    {
        // Try SIGTERM first for graceful shutdown
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }

    #[cfg(not(unix))]
    {
        // On Windows, just kill directly
        child.kill().await.ok();
    }

    // Wait up to 5 seconds for graceful exit
    match time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            child.kill().await.ok();
            child.wait().await.ok();
        }
    }
}

pub async fn send_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    resp: &Response,
) -> std::io::Result<()> {
    let data = serde_json::to_vec(resp)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = data.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&data).await?;
    writer.flush().await
}
