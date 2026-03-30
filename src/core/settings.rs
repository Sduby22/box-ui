use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigEntry {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    pub source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfigSource {
    Local,
    Remote {
        url: String,
        refresh_interval_minutes: u32,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppSettings {
    pub configs: Vec<ConfigEntry>,
    /// UUID of the active configuration.
    pub active_config: Option<Uuid>,
    /// Filename of the active kernel binary in the kernels directory.
    pub active_kernel: Option<String>,
    /// Maximum number of log lines to keep in the buffer.
    #[serde(default = "default_max_log_lines")]
    pub max_log_lines: usize,
    /// Whether to run the kernel with elevated (root/admin) privileges.
    #[serde(default)]
    pub run_elevated: bool,
    /// Whether to automatically start the kernel when the app launches.
    #[serde(default)]
    pub launch_core_on_start: bool,

}

fn default_max_log_lines() -> usize {
    crate::ui::logs::DEFAULT_MAX_LOG_LINES
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            configs: Vec::new(),
            active_config: None,
            active_kernel: None,
            max_log_lines: default_max_log_lines(),
            run_elevated: false,
            launch_core_on_start: false,
        }
    }
}

pub struct SettingsManager {
    data_dir: PathBuf,
    configs_dir: PathBuf,
    kernels_dir: PathBuf,
    working_dir: PathBuf,
    settings: AppSettings,
}

impl SettingsManager {
    pub fn new(data_dir: PathBuf) -> Self {
        let configs_dir = data_dir.join("configs");
        let kernels_dir = data_dir.join("kernels");
        let working_dir = data_dir.join("pwd");
        std::fs::create_dir_all(&configs_dir).ok();
        std::fs::create_dir_all(&kernels_dir).ok();
        std::fs::create_dir_all(&working_dir).ok();

        let mut settings = Self::load(&data_dir);

        // Ensure every config has a valid (non-nil) UUID
        let mut migrated = false;
        for entry in &mut settings.configs {
            if entry.id.is_nil() {
                entry.id = Uuid::new_v4();
                migrated = true;
            }
        }

        // Auto-select first config if none is active
        if settings.active_config.is_none()
            && let Some(first) = settings.configs.first()
        {
            settings.active_config = Some(first.id);
            migrated = true;
        }

        let mgr = Self {
            data_dir,
            configs_dir,
            kernels_dir,
            working_dir,
            settings,
        };

        if migrated {
            mgr.save();
        }
        mgr
    }

    fn settings_path(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    fn load(data_dir: &Path) -> AppSettings {
        let path = Self::settings_path(data_dir);
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            AppSettings::default()
        }
    }

    fn save(&self) {
        let path = Self::settings_path(&self.data_dir);
        if let Ok(content) = serde_json::to_string_pretty(&self.settings) {
            std::fs::write(path, content).ok();
        }
    }

    // ── Config operations ──

    pub fn import_local_config(&mut self, name: &str, source_path: &Path) -> Result<(), String> {
        let id = Uuid::new_v4();
        let file_name = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config.json");

        let dest = self.configs_dir.join(file_name);
        std::fs::copy(source_path, &dest).map_err(|e| format!("Failed to copy config: {e}"))?;

        let entry = ConfigEntry {
            id,
            name: name.to_string(),
            path: dest,
            source: ConfigSource::Local,
        };
        self.settings.configs.push(entry);
        self.ensure_active_config();
        self.save();
        Ok(())
    }

    pub fn add_remote_config(&mut self, name: String, url: String, refresh_interval_minutes: u32) {
        let id = Uuid::new_v4();
        let dest = self.configs_dir.join(&name);
        let entry = ConfigEntry {
            id,
            name,
            path: dest,
            source: ConfigSource::Remote {
                url,
                refresh_interval_minutes,
            },
        };
        self.settings.configs.push(entry);
        self.ensure_active_config();
        self.save();
    }

    pub fn update_config(&mut self, id: Uuid, name: String, source: ConfigSource) {
        if let Some(entry) = self.settings.configs.iter_mut().find(|c| c.id == id) {
            entry.name = name;
            entry.source = source;
            self.save();
        }
    }

    pub fn remove_config(&mut self, id: Uuid) {
        if let Some(pos) = self.settings.configs.iter().position(|c| c.id == id) {
            let entry = self.settings.configs.remove(pos);
            std::fs::remove_file(&entry.path).ok();
            if self.settings.active_config == Some(id) {
                self.settings.active_config = None;
                self.ensure_active_config();
            }
            self.save();
        }
    }

    pub fn set_active_config(&mut self, id: Uuid) {
        if self.settings.configs.iter().any(|c| c.id == id) {
            self.settings.active_config = Some(id);
            self.save();
        }
    }

    pub fn active_config(&self) -> Option<&ConfigEntry> {
        let active_id = self.settings.active_config?;
        self.settings.configs.iter().find(|c| c.id == active_id)
    }

    pub fn configs(&self) -> &[ConfigEntry] {
        &self.settings.configs
    }

    pub fn active_config_id(&self) -> Option<Uuid> {
        self.settings.active_config
    }

    /// If there are configs but none is active, select the first one.
    fn ensure_active_config(&mut self) {
        if self.settings.active_config.is_none()
            && let Some(first) = self.settings.configs.first()
        {
            self.settings.active_config = Some(first.id);
        }
    }

    pub fn config_path(&self, name: &str) -> PathBuf {
        self.configs_dir.join(name)
    }

    /// Parse the Clash API address and secret from the active config file.
    /// Reads `experimental.clash_api.external_controller` and `experimental.clash_api.secret`
    /// from the sing-box JSON config.
    pub fn active_clash_api_info(&self) -> (Option<String>, Option<String>) {
        let Some(entry) = self.active_config() else {
            return (None, None);
        };
        let Ok(content) = std::fs::read_to_string(&entry.path) else {
            return (None, None);
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
            return (None, None);
        };
        let clash_api = json.get("experimental").and_then(|e| e.get("clash_api"));
        let address = clash_api
            .and_then(|c| c.get("external_controller"))
            .and_then(|v| v.as_str())
            .map(|addr| {
                let addr = addr.replace("0.0.0.0", "127.0.0.1");
                format!("http://{addr}")
            });
        let secret = clash_api
            .and_then(|c| c.get("secret"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        (address, secret)
    }

    // ── Kernel operations ──

    /// List all kernel binaries in the kernels directory.
    /// Returns filenames sorted reverse-alphabetically (newest version first).
    pub fn kernel_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.kernels_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let e = e.ok()?;
                let ft = e.file_type().ok()?;
                if ft.is_file() {
                    Some(e.file_name().to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect();
        names.sort_by(|a, b| b.cmp(a));
        names
    }

    pub fn set_active_kernel(&mut self, name: &str) {
        self.settings.active_kernel = Some(name.to_string());
        self.save();
    }

    pub fn active_kernel_name(&self) -> Option<&str> {
        self.settings.active_kernel.as_deref()
    }

    pub fn active_kernel_path(&self) -> Option<PathBuf> {
        let name = self.settings.active_kernel.as_deref()?;
        let path = self.kernels_dir.join(name);
        if path.is_file() { Some(path) } else { None }
    }

    /// Return the set of installed kernel version tags (e.g. "v1.11.0").
    /// On Windows, kernel filenames have a `.exe` suffix which is stripped here
    /// so they match GitHub release `tag_name` values.
    pub fn installed_kernel_versions(&self) -> Vec<String> {
        self.kernel_names()
            .into_iter()
            .map(|name| name.strip_suffix(".exe").unwrap_or(&name).to_string())
            .collect()
    }

    /// Remove a kernel binary from the kernels directory.
    /// If it was the active kernel, clear the active selection.
    pub fn remove_kernel(&mut self, name: &str) {
        let path = self.kernels_dir.join(name);
        std::fs::remove_file(&path).ok();
        if self.settings.active_kernel.as_deref() == Some(name) {
            self.settings.active_kernel = None;
            self.save();
        }
    }

    /// Activate a newly downloaded kernel by filename.
    pub fn activate_new_kernel(&mut self, name: &str) {
        if self.settings.active_kernel.is_none() {
            self.settings.active_kernel = Some(name.to_string());
            self.save();
        }
    }

    pub fn kernels_dir(&self) -> &Path {
        &self.kernels_dir
    }

    // ── Log settings ──

    pub fn max_log_lines(&self) -> usize {
        self.settings.max_log_lines
    }

    pub fn run_elevated(&self) -> bool {
        self.settings.run_elevated
    }

    pub fn set_run_elevated(&mut self, elevated: bool) {
        self.settings.run_elevated = elevated;
        self.save();
    }

    // ── Working directory ──

    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    // ── Launch core on start ──

    pub fn launch_core_on_start(&self) -> bool {
        self.settings.launch_core_on_start
    }

    pub fn set_launch_core_on_start(&mut self, enabled: bool) {
        self.settings.launch_core_on_start = enabled;
        self.save();
    }
}
