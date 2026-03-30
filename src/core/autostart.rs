fn current_exe_str() -> Result<String, String> {
    std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {e}"))?
        .to_str()
        .ok_or("Exe path is not valid UTF-8".to_string())
        .map(|s| s.to_string())
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn register() -> Result<(), String> {
        let autostart_dir = dirs::config_dir()
            .ok_or("Cannot find config directory")?
            .join("autostart");
        std::fs::create_dir_all(&autostart_dir)
            .map_err(|e| format!("Failed to create autostart dir: {e}"))?;

        let desktop_entry = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Box UI\n\
             Exec={}\n\
             X-GNOME-Autostart-enabled=true\n",
            super::current_exe_str()?
        );

        let path = autostart_dir.join("box-ui.desktop");
        std::fs::write(&path, desktop_entry)
            .map_err(|e| format!("Failed to write desktop entry: {e}"))?;
        Ok(())
    }

    pub fn unregister() -> Result<(), String> {
        let path = dirs::config_dir()
            .ok_or("Cannot find config directory")?
            .join("autostart")
            .join("box-ui.desktop");
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("Failed to remove desktop entry: {e}"))?;
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        let path = dirs::config_dir()
            .map(|d| d.join("autostart").join("box-ui.desktop"));
        let Some(path) = path else { return false };
        let Ok(content) = std::fs::read_to_string(&path) else {
            return false;
        };
        // Verify Exec= line points to the current exe
        let Ok(current) = super::current_exe_str() else {
            return false;
        };
        content
            .lines()
            .any(|line| line.starts_with("Exec=") && line[5..] == current)
    }

    pub fn repair_if_needed() {
        let Some(path) = dirs::config_dir().map(|d| d.join("autostart").join("box-ui.desktop"))
        else {
            return;
        };
        if !path.exists() {
            return;
        }
        // If registered but path is stale, re-register
        if !is_registered() {
            register().ok();
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;

    /// Derive the `.app` bundle path from the running binary.
    /// e.g. `/Applications/Box UI.app/Contents/MacOS/box-ui` → `/Applications/Box UI.app`
    fn app_bundle_path() -> Result<PathBuf, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to get current exe path: {e}"))?;
        // Walk up looking for a directory ending in `.app`
        let mut path = exe.as_path();
        while let Some(parent) = path.parent() {
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
                && path.is_dir()
            {
                return Ok(path.to_path_buf());
            }
            path = parent;
        }
        Err("Not running from a .app bundle".to_string())
    }

    const APP_NAME: &str = "Box UI";

    /// Also clean up old LaunchAgent plist from previous versions.
    fn remove_legacy_plist() {
        if let Some(home) = dirs::home_dir() {
            let plist = home
                .join("Library")
                .join("LaunchAgents")
                .join("com.boxui.app.plist");
            if plist.exists() {
                std::fs::remove_file(&plist).ok();
            }
        }
    }

    pub fn register() -> Result<(), String> {
        remove_legacy_plist();

        let bundle = app_bundle_path()?;
        let bundle_str = bundle
            .to_str()
            .ok_or("App bundle path is not valid UTF-8")?;

        let script = format!(
            r#"tell application "System Events" to make login item at end with properties {{path:"{bundle_str}", hidden:false, name:"{APP_NAME}"}}"#
        );
        let output = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| format!("Failed to run osascript: {e}"))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("Failed to add login item: {}", stderr.trim()))
        }
    }

    pub fn unregister() -> Result<(), String> {
        remove_legacy_plist();

        let script = format!(
            r#"tell application "System Events" to delete login item "{APP_NAME}""#
        );
        let output = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| format!("Failed to run osascript: {e}"))?;

        // Ignore "not found" errors — item may already be removed
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("Can't get login item") {
                return Err(format!("Failed to remove login item: {}", stderr.trim()));
            }
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        let script = format!(
            r#"tell application "System Events" to get the name of every login item"#
        );
        std::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .is_some_and(|out| out.contains(APP_NAME))
    }

    /// macOS Login Items are name-based, no path repair needed.
    pub fn repair_if_needed() {}
}

#[cfg(target_os = "windows")]
mod imp {
    const TASK_NAME: &str = "BoxUI";

    fn startup_dir() -> Result<std::path::PathBuf, String> {
        let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA not set".to_string())?;
        Ok(std::path::PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup"))
    }

    fn remove_bat() {
        if let Ok(dir) = startup_dir() {
            let bat = dir.join("box-ui.bat");
            if bat.exists() {
                std::fs::remove_file(&bat).ok();
            }
        }
    }

    fn remove_schtask() {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("schtasks")
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok();
    }

    fn has_schtask() -> bool {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("schtasks")
            .args(["/query", "/tn", TASK_NAME])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Get the exe path registered in the scheduled task, if any.
    fn schtask_exe_path() -> Option<String> {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("schtasks")
            .args(["/query", "/tn", TASK_NAME, "/xml"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let xml = String::from_utf8_lossy(&output.stdout);
        // Extract <Command>...</Command> value
        let start = xml.find("<Command>")? + "<Command>".len();
        let end = xml[start..].find("</Command>")? + start;
        Some(xml[start..end].to_string())
    }

    /// Get the exe path registered in the startup .bat, if any.
    fn bat_exe_path() -> Option<String> {
        let bat = startup_dir().ok()?.join("box-ui.bat");
        let content = std::fs::read_to_string(bat).ok()?;
        // Format: start "" "C:\path\to\box-ui.exe"
        let start = content.find("start \"\" \"")? + "start \"\" \"".len();
        let end = content[start..].find('"')? + start;
        Some(content[start..end].to_string())
    }

    /// Check if the registered path matches the current exe (case-insensitive on Windows).
    fn path_matches_current(registered: &str) -> bool {
        super::current_exe_str()
            .is_ok_and(|current| current.eq_ignore_ascii_case(registered))
    }

    pub fn register() -> Result<(), String> {
        let exe = super::current_exe_str()?;

        if crate::core::permissions::is_elevated() {
            // Running as admin — use Task Scheduler with HighestAvailable
            // so subsequent logins start the app elevated without UAC.
            remove_bat();

            let xml = format!(
                r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions>
    <Exec>
      <Command>{exe}</Command>
    </Exec>
  </Actions>
</Task>"#
            );

            let temp = std::env::temp_dir().join("box-ui-task.xml");
            // Write as UTF-16 LE with BOM for schtasks compatibility
            let mut bytes = vec![0xFF, 0xFE]; // UTF-16 LE BOM
            for code_unit in xml.encode_utf16() {
                bytes.extend_from_slice(&code_unit.to_le_bytes());
            }
            std::fs::write(&temp, &bytes)
                .map_err(|e| format!("Failed to write task XML: {e}"))?;

            let output = {
                use std::os::windows::process::CommandExt;
                std::process::Command::new("schtasks")
                    .args([
                        "/create",
                        "/xml",
                        &temp.to_string_lossy(),
                        "/tn",
                        TASK_NAME,
                        "/f",
                    ])
                    .creation_flags(0x08000000) // CREATE_NO_WINDOW
                    .output()
            }
                .map_err(|e| format!("Failed to run schtasks: {e}"))?;

            std::fs::remove_file(&temp).ok();

            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("Failed to create scheduled task: {}", stderr.trim()))
            }
        } else {
            // Not elevated — fall back to Startup folder .bat
            remove_schtask();

            let startup_dir = startup_dir()?;
            std::fs::create_dir_all(&startup_dir)
                .map_err(|e| format!("Failed to create startup dir: {e}"))?;

            let bat_path = startup_dir.join("box-ui.bat");
            let content = format!("@echo off\nstart \"\" \"{exe}\"");
            std::fs::write(&bat_path, content)
                .map_err(|e| format!("Failed to write startup script: {e}"))?;
            Ok(())
        }
    }

    pub fn unregister() -> Result<(), String> {
        // Remove both methods to be thorough
        remove_schtask();
        remove_bat();
        Ok(())
    }

    pub fn is_registered() -> bool {
        // Entry must exist AND point to the current exe path
        if has_schtask() {
            return schtask_exe_path().is_some_and(|p| path_matches_current(&p));
        }
        bat_exe_path().is_some_and(|p| path_matches_current(&p))
    }

    /// If autostart is registered but points to a stale path, re-register
    /// with the current exe. Called at app startup for seamless repair.
    pub fn repair_if_needed() {
        let has_task = has_schtask();
        let has_bat = startup_dir()
            .map(|d| d.join("box-ui.bat").exists())
            .unwrap_or(false);

        if !has_task && !has_bat {
            return; // Not registered at all — nothing to repair
        }

        // Check if any existing entry already points to current exe
        if has_task && schtask_exe_path().is_some_and(|p| path_matches_current(&p)) {
            return;
        }
        if has_bat && bat_exe_path().is_some_and(|p| path_matches_current(&p)) {
            return;
        }

        // Stale registration detected — silently re-register
        register().ok();
    }
}

pub use imp::*;
