//! Privileged helper daemon installation and uninstallation.
//!
//! On macOS, the helper binary is installed to /Library/PrivilegedHelperTools/ and
//! registered as a launchd daemon. On Linux, it will use systemd. On Windows, it
//! will use Windows Service Manager. Installation requires one-time administrator
//! authorization via a native OS dialog.

/// Check if the installed helper is outdated and needs updating.
/// Compares the running helper's version against the GUI's version.
/// Returns false if the helper is actively managing a running kernel to avoid disruption.
pub fn needs_update() -> bool {
    if !is_installed() {
        return false;
    }
    let gui_version = env!("CARGO_PKG_VERSION");
    match super::helper_client::HelperClient::connect() {
        Ok(mut client) => {
            let version_outdated = match client.version() {
                Ok(helper_version) => helper_version != gui_version,
                Err(_) => true,
            };
            if !version_outdated {
                return false;
            }
            // Don't update if the helper is actively managing a running kernel
            if let Ok((true, _)) = client.status() {
                tracing::info!(
                    "Helper is outdated but managing a running kernel, deferring update"
                );
                return false;
            }
            true
        }
        Err(_) => false,
    }
}

/// Update the helper daemon: shutdown the old one, reinstall, kickstart the new one.
pub fn update_helper() -> Result<(), String> {
    if let Ok(mut client) = super::helper_client::HelperClient::connect() {
        client.shutdown().ok();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    install_helper()
}

#[cfg(target_os = "macos")]
mod imp {
    use box_ui_ipc::{HELPER_BINARY_PATH, HELPER_LABEL, PLIST_PATH};

    pub fn is_installed() -> bool {
        std::path::Path::new(HELPER_BINARY_PATH).exists()
            && std::path::Path::new(PLIST_PATH).exists()
    }

    pub fn install_helper() -> Result<(), String> {
        let gui_exe = std::env::current_exe()
            .map_err(|e| format!("Cannot determine executable path: {e}"))?;
        let helper_src = gui_exe
            .parent()
            .ok_or("Cannot determine executable directory")?
            .join("box-ui-helper");

        if !helper_src.exists() {
            return Err(format!(
                "Helper binary not found at {}. Build it first with `cargo build -p box-ui-helper`.",
                helper_src.display()
            ));
        }

        let plist_content = launchd_plist();

        let script = format!(
            r#"
mkdir -p /Library/PrivilegedHelperTools
cp '{helper_src}' '{HELPER_BINARY_PATH}'
chmod 755 '{HELPER_BINARY_PATH}'
chown root:wheel '{HELPER_BINARY_PATH}'
cat > '{PLIST_PATH}' << 'PLISTEOF'
{plist_content}
PLISTEOF
chmod 644 '{PLIST_PATH}'
chown root:wheel '{PLIST_PATH}'
launchctl bootstrap system '{PLIST_PATH}'
"#,
            helper_src = helper_src.display(),
        );

        run_privileged_script(&script)
    }

    pub fn uninstall_helper() -> Result<(), String> {
        let script = format!(
            r#"
launchctl bootout system/{HELPER_LABEL} 2>/dev/null || true
rm -f '{PLIST_PATH}'
rm -f '{HELPER_BINARY_PATH}'
rm -f '{}'
"#,
            box_ui_ipc::SOCKET_PATH,
        );

        run_privileged_script(&script)
    }

    pub fn kickstart_helper() -> Result<(), String> {
        let output = std::process::Command::new("launchctl")
            .arg("kickstart")
            .arg(format!("system/{HELPER_LABEL}"))
            .output()
            .map_err(|e| format!("Failed to run launchctl: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("already running") || stderr.contains("already active") {
                Ok(())
            } else {
                Err(format!("launchctl kickstart failed: {}", stderr.trim()))
            }
        }
    }

    fn run_privileged_script(script: &str) -> Result<(), String> {
        let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");

        let output = std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!(
                r#"do shell script "{escaped}" with administrator privileges"#
            ))
            .output()
            .map_err(|e| format!("Failed to run osascript: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("User canceled") || stderr.contains("user canceled") {
                Err("Authorization canceled by user".to_string())
            } else {
                Err(format!("Installation failed: {}", stderr.trim()))
            }
        }
    }

    fn launchd_plist() -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{HELPER_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{HELPER_BINARY_PATH}</string>
    </array>
    <key>RunAtLoad</key>
    <false/>
    <key>KeepAlive</key>
    <false/>
    <key>StandardErrorPath</key>
    <string>/var/log/box-ui-helper.log</string>
    <key>StandardOutPath</key>
    <string>/var/log/box-ui-helper.log</string>
</dict>
</plist>"#
        )
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn is_installed() -> bool {
        false
    }

    pub fn install_helper() -> Result<(), String> {
        Err("Helper daemon is not yet implemented for Linux".to_string())
    }

    pub fn uninstall_helper() -> Result<(), String> {
        Err("Helper daemon is not yet implemented for Linux".to_string())
    }

    pub fn kickstart_helper() -> Result<(), String> {
        Err("Helper daemon is not yet implemented for Linux".to_string())
    }
}

#[cfg(target_os = "windows")]
mod imp {
    pub fn is_installed() -> bool {
        false
    }

    pub fn install_helper() -> Result<(), String> {
        Err("Helper daemon is not yet implemented for Windows".to_string())
    }

    pub fn uninstall_helper() -> Result<(), String> {
        Err("Helper daemon is not yet implemented for Windows".to_string())
    }

    pub fn kickstart_helper() -> Result<(), String> {
        Err("Helper daemon is not yet implemented for Windows".to_string())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    pub fn is_installed() -> bool {
        false
    }

    pub fn install_helper() -> Result<(), String> {
        Err("Helper daemon is not supported on this platform".to_string())
    }

    pub fn uninstall_helper() -> Result<(), String> {
        Err("Helper daemon is not supported on this platform".to_string())
    }

    pub fn kickstart_helper() -> Result<(), String> {
        Err("Helper daemon is not supported on this platform".to_string())
    }
}

pub use imp::*;
