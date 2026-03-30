/// Platform-specific privilege management for elevated kernel execution.
///
/// - macOS/Linux: setuid bit on the kernel binary (one-time password prompt)
/// - Windows: GUI self-elevation check and re-launch

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    pub fn has_kernel_permissions(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o4000 != 0)
            .unwrap_or(false)
    }

    pub fn grant_kernel_permissions(path: &Path) -> Result<(), String> {
        let path_str = path.to_string_lossy();
        let script = format!("chown root:admin '{path_str}' && chmod u+s '{path_str}'");
        run_privileged_script(&script)
    }

    pub fn revoke_kernel_permissions(path: &Path) -> Result<(), String> {
        let path_str = path.to_string_lossy();
        let script = format!("chmod u-s '{path_str}'");
        run_privileged_script(&script)
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
                Err(format!("Failed to set permissions: {}", stderr.trim()))
            }
        }
    }

    pub fn is_elevated() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    pub fn relaunch_elevated() -> Result<(), String> {
        Err("Use setuid on the kernel binary instead of relaunching on macOS".to_string())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;

    pub fn has_kernel_permissions(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o4000 != 0)
            .unwrap_or(false)
    }

    pub fn grant_kernel_permissions(path: &Path) -> Result<(), String> {
        let path_str = path.to_string_lossy();

        let output = std::process::Command::new("pkexec")
            .arg("sh")
            .arg("-c")
            .arg(format!("chown root:root '{path_str}' && chmod u+s '{path_str}'"))
            .output()
            .map_err(|e| format!("Failed to run pkexec: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("dismissed") || stderr.contains("Not authorized") {
                Err("Authorization canceled by user".to_string())
            } else {
                Err(format!("Failed to set permissions: {}", stderr.trim()))
            }
        }
    }

    pub fn revoke_kernel_permissions(path: &Path) -> Result<(), String> {
        let path_str = path.to_string_lossy();

        let output = std::process::Command::new("pkexec")
            .arg("chmod")
            .arg("u-s")
            .arg(path_str.as_ref())
            .output()
            .map_err(|e| format!("Failed to run pkexec: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("Failed to revoke permissions: {}", stderr.trim()))
        }
    }

    pub fn is_elevated() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    pub fn relaunch_elevated() -> Result<(), String> {
        Err("Use setuid on the kernel binary instead of relaunching on Linux".to_string())
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::path::Path;

    pub fn has_kernel_permissions(_path: &Path) -> bool {
        // On Windows, elevation is at the process level, not the binary level.
        // The kernel inherits the GUI's elevation status.
        is_elevated()
    }

    #[allow(dead_code)]
    pub fn grant_kernel_permissions(_path: &Path) -> Result<(), String> {
        // No-op on Windows — elevation is handled by relaunching the GUI as admin.
        Ok(())
    }

    #[allow(dead_code)]
    pub fn revoke_kernel_permissions(_path: &Path) -> Result<(), String> {
        // No-op on Windows.
        Ok(())
    }

    pub fn is_elevated() -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        unsafe {
            let mut token = Default::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }

            let mut elevation = TOKEN_ELEVATION::default();
            let mut ret_len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&raw mut elevation as *mut _),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret_len,
            );

            let _ = CloseHandle(token);
            ok.is_ok() && elevation.TokenIsElevated != 0
        }
    }

    pub fn relaunch_elevated() -> Result<(), String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("Cannot determine executable path: {e}"))?;

        let exe_str = exe.to_string_lossy().replace('\'', "''");
        let args: Vec<String> = std::env::args()
            .skip(1)
            .map(|a| a.replace('\'', "''"))
            .collect();

        let command = if args.is_empty() {
            format!(
                "Start-Process -FilePath '{exe_str}' -Verb RunAs"
            )
        } else {
            let args_str = args.join("', '");
            format!(
                "Start-Process -FilePath '{exe_str}' -ArgumentList '{args_str}' -Verb RunAs"
            )
        };

        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;

        let result = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &command])
            .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
            .spawn();

        match result {
            Ok(_) => std::process::exit(0),
            Err(e) => Err(format!("Failed to relaunch as administrator: {e}")),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    use std::path::Path;

    pub fn has_kernel_permissions(_path: &Path) -> bool {
        false
    }

    pub fn grant_kernel_permissions(_path: &Path) -> Result<(), String> {
        Err("Not supported on this platform".to_string())
    }

    pub fn revoke_kernel_permissions(_path: &Path) -> Result<(), String> {
        Err("Not supported on this platform".to_string())
    }

    pub fn is_elevated() -> bool {
        false
    }

    pub fn relaunch_elevated() -> Result<(), String> {
        Err("Not supported on this platform".to_string())
    }
}

pub use imp::*;
