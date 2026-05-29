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
            .arg(format!(
                "chown root:root '{path_str}' && chmod u+s '{path_str}'"
            ))
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
    use std::os::windows::ffi::OsStrExt;
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
        use windows::Win32::Security::{
            GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        };
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

        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_NORMAL;
        use windows::core::PCWSTR;

        let operation = wide_str("runas");
        let file = wide_os(exe.as_os_str());
        let params = command_line_args(std::env::args_os().skip(1));
        let params_wide = if params.is_empty() {
            None
        } else {
            Some(wide_str(&params))
        };
        let params_pcwstr = params_wide
            .as_ref()
            .map(|p| PCWSTR::from_raw(p.as_ptr()))
            .unwrap_or_else(PCWSTR::null);

        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR::from_raw(operation.as_ptr()),
                PCWSTR::from_raw(file.as_ptr()),
                params_pcwstr,
                PCWSTR::null(),
                SW_NORMAL,
            )
        };

        let code = result.0 as usize;
        if code > 32 {
            std::process::exit(0);
        }

        Err(format!(
            "Failed to relaunch as administrator: {}",
            shell_execute_error(code)
        ))
    }

    fn wide_os(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn wide_str(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn command_line_args(args: impl Iterator<Item = std::ffi::OsString>) -> String {
        args.map(|arg| quote_windows_arg(&arg.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn quote_windows_arg(arg: &str) -> String {
        if arg.is_empty() {
            return "\"\"".to_string();
        }

        if !arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
            return arg.to_string();
        }

        let mut quoted = String::with_capacity(arg.len() + 2);
        quoted.push('"');

        let mut backslashes = 0usize;
        for ch in arg.chars() {
            match ch {
                '\\' => {
                    backslashes += 1;
                }
                '"' => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                _ => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                    quoted.push(ch);
                }
            }
        }

        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        quoted
    }

    fn shell_execute_error(code: usize) -> &'static str {
        match code {
            0 => "out of memory or resources",
            2 => "file not found",
            3 => "path not found",
            5 => "access denied",
            8 => "out of memory",
            26 => "sharing violation",
            27 => "file association incomplete",
            28 => "DDE timeout",
            29 => "DDE transaction failed",
            30 => "DDE busy",
            31 => "no application association",
            32 => "DLL not found",
            1223 => "UAC prompt was cancelled",
            _ => "ShellExecuteW failed",
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
