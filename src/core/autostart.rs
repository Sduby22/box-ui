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
        dirs::config_dir()
            .map(|d| d.join("autostart").join("box-ui.desktop").exists())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    pub fn register() -> Result<(), String> {
        let exe = super::current_exe_str()?;
        let plist_content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.boxui.app</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>"#
        );

        let launch_agents = dirs::home_dir()
            .ok_or("Cannot find home directory")?
            .join("Library")
            .join("LaunchAgents");
        std::fs::create_dir_all(&launch_agents)
            .map_err(|e| format!("Failed to create LaunchAgents dir: {e}"))?;

        let path = launch_agents.join("com.boxui.app.plist");
        std::fs::write(&path, plist_content)
            .map_err(|e| format!("Failed to write plist: {e}"))?;
        Ok(())
    }

    pub fn unregister() -> Result<(), String> {
        let path = dirs::home_dir()
            .ok_or("Cannot find home directory")?
            .join("Library")
            .join("LaunchAgents")
            .join("com.boxui.app.plist");
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("Failed to remove plist: {e}"))?;
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        dirs::home_dir()
            .map(|d| {
                d.join("Library")
                    .join("LaunchAgents")
                    .join("com.boxui.app.plist")
                    .exists()
            })
            .unwrap_or(false)
    }
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
        has_schtask()
            || startup_dir()
                .map(|d| d.join("box-ui.bat").exists())
                .unwrap_or(false)
    }
}

pub use imp::*;
