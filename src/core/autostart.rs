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
    fn startup_dir() -> Result<std::path::PathBuf, String> {
        let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA not set".to_string())?;
        Ok(std::path::PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup"))
    }

    pub fn register() -> Result<(), String> {
        let exe = super::current_exe_str()?;
        let startup_dir = startup_dir()?;
        std::fs::create_dir_all(&startup_dir)
            .map_err(|e| format!("Failed to create startup dir: {e}"))?;

        let bat_path = startup_dir.join("box-ui.bat");
        let content = format!("@echo off\nstart \"\" \"{exe}\"");
        std::fs::write(&bat_path, content)
            .map_err(|e| format!("Failed to write startup script: {e}"))?;
        Ok(())
    }

    pub fn unregister() -> Result<(), String> {
        let bat_path = startup_dir()?.join("box-ui.bat");
        if bat_path.exists() {
            std::fs::remove_file(&bat_path)
                .map_err(|e| format!("Failed to remove startup script: {e}"))?;
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        startup_dir()
            .map(|d| d.join("box-ui.bat").exists())
            .unwrap_or(false)
    }
}

pub use imp::*;
