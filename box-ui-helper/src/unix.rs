//! Unix-specific code: socket binding, permissions, chown, peer credential verification.
//! Shared between macOS and Linux.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use box_ui_ipc::SOCKET_PATH;
use tokio::net::UnixListener;
use tokio::sync::{Mutex, Notify};

use crate::handler::{self, HelperState};

pub async fn run() {
    // Clean up stale socket
    if Path::new(SOCKET_PATH).exists() {
        std::fs::remove_file(SOCKET_PATH).ok();
    }

    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind socket at {SOCKET_PATH}: {e}");
            std::process::exit(1);
        }
    };

    // Set socket permissions: rw-rw---- (owner root, group admin/users)
    if let Err(e) = std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(0o660)) {
        tracing::warn!("Failed to set socket permissions: {e}");
    }
    set_socket_group(SOCKET_PATH);

    tracing::info!("Listening on {SOCKET_PATH}");

    let state = Arc::new(Mutex::new(HelperState::new()));
    let shutdown_notify = Arc::new(Notify::new());
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        // Peer credential verification
                        if !verify_peer(&stream) {
                            tracing::warn!("Rejected connection: peer verification failed");
                            continue;
                        }

                        let state = state.clone();
                        let shutdown = shutdown_notify.clone();

                        let (reader, writer) = stream.into_split();
                        tokio::spawn(async move {
                            handler::handle_client(reader, writer, state, shutdown).await;
                        });
                    }
                    Err(e) => {
                        tracing::error!("Accept error: {e}");
                    }
                }
            }
            _ = shutdown_notify.notified() => {
                tracing::info!("Shutdown requested");
                break;
            }
            _ = handler::bind_timeout(&state) => {
                tracing::info!("No bind received within timeout, shutting down");
                break;
            }
            _ = handler::gui_liveness_check(&state) => {
                tracing::info!("Bound GUI process exited, shutting down");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT received, shutting down");
                break;
            }
        }
    }

    // Cleanup: stop sing-box if running
    {
        let mut st = state.lock().await;
        if let Some(ref mut child) = st.child {
            tracing::info!("Stopping sing-box before exit");
            handler::stop_child(child).await;
            st.child = None;
            st.child_pid = None;
        }
    }

    std::fs::remove_file(SOCKET_PATH).ok();
    tracing::info!("box-ui-helper exited");
}

/// Verify the connecting peer is the Box UI GUI process.
/// Returns true if verification passes or cannot be performed (fail-open for now).
fn verify_peer(stream: &tokio::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();

    let Some(pid) = get_peer_pid(fd) else {
        // If we can't get the peer PID, allow the connection but log a warning
        tracing::warn!("Could not determine peer PID, allowing connection");
        return true;
    };

    let Some(exe_path) = get_pid_exe_path(pid) else {
        tracing::warn!("Could not determine executable for PID {pid}, allowing connection");
        return true;
    };

    // Verify the executable name is "box-ui"
    let exe_name = Path::new(&exe_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if exe_name == "box-ui" {
        true
    } else {
        tracing::warn!(
            "Rejected connection from PID {pid}: executable is '{exe_path}', expected 'box-ui'"
        );
        false
    }
}

#[cfg(target_os = "macos")]
fn get_peer_pid(fd: std::os::unix::io::RawFd) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &raw mut pid as *mut libc::c_void,
            &mut len,
        )
    };
    if ret == 0 { Some(pid as u32) } else { None }
}

#[cfg(target_os = "linux")]
fn get_peer_pid(fd: std::os::unix::io::RawFd) -> Option<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &raw mut cred as *mut libc::c_void,
            &mut len,
        )
    };
    if ret == 0 { Some(cred.pid as u32) } else { None }
}

/// Get the executable path for a given PID.
#[cfg(target_os = "macos")]
fn get_pid_exe_path(pid: u32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let ret = unsafe {
        libc::proc_pidpath(
            pid as i32,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if ret > 0 {
        buf.truncate(ret as usize);
        String::from_utf8(buf).ok()
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn get_pid_exe_path(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

fn set_socket_group(path: &str) {
    use std::ffi::CString;
    let c_path = CString::new(path).unwrap();

    #[cfg(target_os = "macos")]
    {
        // 'admin' group GID is 80 on macOS
        unsafe {
            libc::chown(c_path.as_ptr(), 0, 80);
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Use root group (GID 0) on Linux; socket permissions handle access
        unsafe {
            libc::chown(c_path.as_ptr(), 0, 0);
        }
    }
}

/// Recursively chown all files in a directory to the specified uid:gid.
pub fn chown_dir_contents(dir: &str, uid: u32, gid: u32) {
    use std::ffi::CString;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(c_path) = CString::new(path.to_string_lossy().as_bytes()) else {
            continue;
        };
        unsafe {
            libc::chown(c_path.as_ptr(), uid, gid);
        }
        if path.is_dir() {
            chown_dir_contents(&path.to_string_lossy(), uid, gid);
        }
    }
}
