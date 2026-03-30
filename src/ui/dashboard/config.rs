use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use uuid::Uuid;

use crate::app::{BoxApp, ToastKind, push_toast};
use crate::core::download;
use crate::core::settings::ConfigSource;

/// Info about a remote config that was successfully downloaded by an async task.
pub struct PendingRemoteConfig {
    pub name: String,
    pub url: String,
    pub refresh_interval_minutes: u32,
}

/// Info about a remote config edit that completed its fetch successfully.
pub struct PendingRemoteConfigEdit {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub refresh_interval_minutes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddConfigType {
    Local,
    Remote,
}

pub struct ConfigState {
    /// Whether the add-config window is open
    pub show_add_config_window: bool,
    /// Selected config type in the add-config window
    pub add_config_type: AddConfigType,
    /// Name input for config (both local and remote)
    pub add_config_name_input: String,
    /// Path input for local config
    pub add_config_path_input: String,
    /// URL input for remote config
    pub add_config_url_input: String,
    /// Refresh interval input (minutes) for remote config
    pub add_config_interval_input: String,
    /// Pending remote config from async download task
    pub pending_remote_config: Arc<Mutex<Vec<PendingRemoteConfig>>>,
    /// Whether a remote config download is in progress
    pub remote_config_downloading: Arc<AtomicBool>,
    /// Whether the subscription auto-refresh task is running
    pub refresh_task_running: Arc<AtomicBool>,
    /// Config IDs that were refreshed by the background task (need kernel restart if active)
    pub refreshed_config_ids: Arc<Mutex<Vec<Uuid>>>,
    /// Whether the edit-config window is open
    pub show_edit_config_window: bool,
    /// The config ID being edited
    pub edit_config_id: Option<Uuid>,
    /// Selected config type in the edit-config window
    pub edit_config_type: AddConfigType,
    /// Name input for editing config
    pub edit_config_name_input: String,
    /// URL input for editing remote config
    pub edit_config_url_input: String,
    /// Refresh interval input for editing remote config
    pub edit_config_interval_input: String,
    /// Pending remote config edit from async fetch task
    pub pending_remote_config_edit: Arc<Mutex<Vec<PendingRemoteConfigEdit>>>,
}

impl Default for ConfigState {
    fn default() -> Self {
        Self {
            show_add_config_window: false,
            add_config_type: AddConfigType::Local,
            add_config_name_input: String::new(),
            add_config_path_input: String::new(),
            add_config_url_input: String::new(),
            add_config_interval_input: "60".to_string(),
            pending_remote_config: Arc::new(Mutex::new(Vec::new())),
            remote_config_downloading: Arc::new(AtomicBool::new(false)),
            refresh_task_running: Arc::new(AtomicBool::new(false)),
            refreshed_config_ids: Arc::new(Mutex::new(Vec::new())),
            show_edit_config_window: false,
            edit_config_id: None,
            edit_config_type: AddConfigType::Local,
            edit_config_name_input: String::new(),
            edit_config_url_input: String::new(),
            edit_config_interval_input: "60".to_string(),
            pending_remote_config_edit: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

enum ConfigAction {
    SetActive(Uuid),
    Remove(Uuid),
    Refresh(Uuid),
}

pub fn show_config_manager(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.horizontal(|ui| {
        ui.label("Configuration");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("+ Add").clicked() {
                app.dashboard_state.config.show_add_config_window = true;
            }
        });
    });

    ui.add_space(4.0);

    let configs: Vec<_> = app.settings_manager.configs().to_vec();
    let active_id = app.settings_manager.active_config_id();
    let mut action = None;
    for config in &configs {
        ui.horizontal(|ui| {
            let is_active = active_id == Some(config.id);
            let label = if is_active {
                format!("● {}", config.name)
            } else {
                format!("  {}", config.name)
            };
            if ui.selectable_label(is_active, label).clicked() {
                action = Some(ConfigAction::SetActive(config.id));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("🗑").clicked() {
                    action = Some(ConfigAction::Remove(config.id));
                }
                if ui.small_button("✏").clicked() {
                    app.dashboard_state.config.edit_config_id = Some(config.id);
                    app.dashboard_state.config.edit_config_name_input = config.name.clone();
                    match &config.source {
                        ConfigSource::Local => {
                            app.dashboard_state.config.edit_config_type = AddConfigType::Local;
                            app.dashboard_state.config.edit_config_url_input.clear();
                            app.dashboard_state.config.edit_config_interval_input =
                                "60".to_string();
                        }
                        ConfigSource::Remote {
                            url,
                            refresh_interval_minutes,
                        } => {
                            app.dashboard_state.config.edit_config_type = AddConfigType::Remote;
                            app.dashboard_state.config.edit_config_url_input = url.clone();
                            app.dashboard_state.config.edit_config_interval_input =
                                refresh_interval_minutes.to_string();
                        }
                    }
                    app.dashboard_state.config.show_edit_config_window = true;
                }
                if ui.small_button("📄").clicked() {
                    let path = config.path.clone();
                    std::thread::spawn(move || {
                        open::that(&path).ok();
                    });
                }
                if let ConfigSource::Remote { .. } = &config.source {
                    if ui.small_button("🔄").on_hover_text("Refresh now").clicked() {
                        action = Some(ConfigAction::Refresh(config.id));
                    }
                    if let Some(ago_text) = file_age_text(&config.path) {
                        ui.weak(format!("🕐 {ago_text}"));
                    }
                }
            });
        });
    }

    match action {
        Some(ConfigAction::SetActive(id)) => {
            app.settings_manager.set_active_config(id);
            app.refresh_clash_api_base();
            restart_kernel_if_running(app);
        }
        Some(ConfigAction::Remove(id)) => {
            app.settings_manager.remove_config(id);
            // Signal the refresh task to restart without the removed config
            app.dashboard_state
                .config
                .refresh_task_running
                .store(false, Ordering::Relaxed);
        }
        Some(ConfigAction::Refresh(id)) => {
            if let Some(config) = app.settings_manager.configs().iter().find(|c| c.id == id)
                && let ConfigSource::Remote { url, .. } = &config.source
            {
                let client = app.http_client.clone();
                let toasts = app.toasts.clone();
                let dest = config.path.clone();
                let config_url = url.clone();
                let config_name = config.name.clone();
                let refreshed_ids = app.dashboard_state.config.refreshed_config_ids.clone();
                app.runtime.spawn(async move {
                    match download::fetch_remote_config(&client, &config_url, &dest).await {
                        Ok(()) => {
                            refreshed_ids.lock().unwrap().push(id);
                        }
                        Err(e) => {
                            push_toast(
                                &toasts,
                                ToastKind::Error,
                                format!("Failed to refresh \"{config_name}\": {e}"),
                            );
                        }
                    }
                });
            }
        }
        None => {}
    }
}

/// Persist remote configs that were successfully downloaded, and handle edit completions.
pub fn process_pending_configs(app: &mut BoxApp) {
    let mut pending_configs = app.dashboard_state.config.pending_remote_config.lock().unwrap();
    if !pending_configs.is_empty() {
        for pending in pending_configs.drain(..) {
            app.settings_manager.add_remote_config(
                pending.name,
                pending.url,
                pending.refresh_interval_minutes,
            );
        }
        // Signal the refresh task to restart with the new config list
        app.dashboard_state
            .config
            .refresh_task_running
            .store(false, Ordering::Relaxed);
        // Close the dialog and clear inputs on successful download
        app.dashboard_state.config.show_add_config_window = false;
        app.dashboard_state.config.add_config_name_input.clear();
        app.dashboard_state.config.add_config_url_input.clear();
        app.dashboard_state.config.add_config_interval_input = "60".to_string();
    }
    drop(pending_configs);

    // Persist remote config edits that completed their fetch successfully
    let mut pending_edits = app
        .dashboard_state
        .config
        .pending_remote_config_edit
        .lock()
        .unwrap();
    if !pending_edits.is_empty() {
        for edit in pending_edits.drain(..) {
            let source = ConfigSource::Remote {
                url: edit.url,
                refresh_interval_minutes: edit.refresh_interval_minutes,
            };
            app.settings_manager
                .update_config(edit.id, edit.name, source);
            // Signal kernel restart if this was the active config
            app.dashboard_state
                .config
                .refreshed_config_ids
                .lock()
                .unwrap()
                .push(edit.id);
        }
        // Signal the refresh task to restart with updated config data
        app.dashboard_state
            .config
            .refresh_task_running
            .store(false, Ordering::Relaxed);
        // Close the edit dialog
        app.dashboard_state.config.show_edit_config_window = false;
    }
    drop(pending_edits);
}

pub fn show_add_config_window(ctx: &egui::Context, app: &mut BoxApp) {
    let mut open = app.dashboard_state.config.show_add_config_window;
    if !open {
        return;
    }

    egui::Window::new("Add Configuration")
        .open(&mut open)
        .resizable(false)
        .default_width(350.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Type:");
                ui.radio_value(
                    &mut app.dashboard_state.config.add_config_type,
                    AddConfigType::Local,
                    "Local",
                );
                ui.radio_value(
                    &mut app.dashboard_state.config.add_config_type,
                    AddConfigType::Remote,
                    "Remote",
                );
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut app.dashboard_state.config.add_config_name_input);
            });

            ui.add_space(4.0);

            match app.dashboard_state.config.add_config_type {
                AddConfigType::Local => {
                    ui.horizontal(|ui| {
                        ui.label("Path:");
                        ui.text_edit_singleline(
                            &mut app.dashboard_state.config.add_config_path_input,
                        );
                        if ui.button("Browse").clicked()
                            && let Some(path) = rfd_pick_file("Select config file")
                        {
                            app.dashboard_state.config.add_config_path_input =
                                path.to_string_lossy().to_string();
                        }
                    });

                    ui.add_space(8.0);

                    if ui.button("Add").clicked() {
                        let name =
                            app.dashboard_state.config.add_config_name_input.trim().to_string();
                        let path_str = app.dashboard_state.config.add_config_path_input.trim();
                        if name.is_empty() {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                "Name cannot be empty".to_string(),
                            );
                        } else if path_str.is_empty() {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                "Path cannot be empty".to_string(),
                            );
                        } else {
                            let path = std::path::PathBuf::from(path_str);
                            match app.settings_manager.import_local_config(&name, &path) {
                                Ok(()) => {
                                    push_toast(
                                        &app.toasts,
                                        ToastKind::Success,
                                        format!("Config \"{name}\" imported"),
                                    );
                                    app.dashboard_state.config.add_config_name_input.clear();
                                    app.dashboard_state.config.add_config_path_input.clear();
                                    app.dashboard_state.config.show_add_config_window = false;
                                }
                                Err(e) => {
                                    push_toast(
                                        &app.toasts,
                                        ToastKind::Error,
                                        format!("Import failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }
                AddConfigType::Remote => {
                    ui.horizontal(|ui| {
                        ui.label("URL:");
                        ui.text_edit_singleline(
                            &mut app.dashboard_state.config.add_config_url_input,
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("Update Interval (min):");
                        ui.text_edit_singleline(
                            &mut app.dashboard_state.config.add_config_interval_input,
                        );
                    });

                    ui.add_space(8.0);

                    let is_downloading = app
                        .dashboard_state
                        .config
                        .remote_config_downloading
                        .load(Ordering::Relaxed);

                    if ui
                        .add_enabled(!is_downloading, egui::Button::new("Add"))
                        .clicked()
                    {
                        let name =
                            app.dashboard_state.config.add_config_name_input.trim().to_string();
                        let url =
                            app.dashboard_state.config.add_config_url_input.trim().to_string();
                        let interval_str =
                            app.dashboard_state.config.add_config_interval_input.trim();

                        if name.is_empty() {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                "Name cannot be empty".to_string(),
                            );
                        } else if url.is_empty() {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                "URL cannot be empty".to_string(),
                            );
                        } else if let Ok(interval) = interval_str.parse::<u32>() {
                            let dest = app.settings_manager.new_config_path();

                            let client = app.http_client.clone();
                            let toasts = app.toasts.clone();
                            let pending =
                                app.dashboard_state.config.pending_remote_config.clone();
                            let downloading_flag =
                                app.dashboard_state.config.remote_config_downloading.clone();
                            let config_name = name.clone();
                            let config_url = url.clone();

                            downloading_flag.store(true, Ordering::Relaxed);

                            app.runtime.spawn(async move {
                                match download::fetch_remote_config(&client, &config_url, &dest)
                                    .await
                                {
                                    Ok(()) => {
                                        pending.lock().unwrap().push(PendingRemoteConfig {
                                            name: config_name.clone(),
                                            url: config_url,
                                            refresh_interval_minutes: interval,
                                        });
                                        push_toast(
                                            &toasts,
                                            ToastKind::Success,
                                            format!(
                                                "Remote config \"{config_name}\" downloaded"
                                            ),
                                        );
                                    }
                                    Err(e) => {
                                        push_toast(
                                            &toasts,
                                            ToastKind::Error,
                                            format!(
                                                "Failed to fetch \"{config_name}\": {e}"
                                            ),
                                        );
                                    }
                                }
                                downloading_flag.store(false, Ordering::Relaxed);
                            });
                        } else {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                "Invalid interval — enter a number in minutes".to_string(),
                            );
                        }
                    }
                }
            }
        });

    app.dashboard_state.config.show_add_config_window = open;
}

pub fn show_edit_config_window(ctx: &egui::Context, app: &mut BoxApp) {
    let mut open = app.dashboard_state.config.show_edit_config_window;
    if !open {
        return;
    }

    egui::Window::new("Edit Configuration")
        .open(&mut open)
        .resizable(false)
        .default_width(350.0)
        .show(ctx, |ui| {
            let is_remote = app.dashboard_state.config.edit_config_type == AddConfigType::Remote;

            ui.horizontal(|ui| {
                ui.label("Type:");
                let type_label = if is_remote { "Remote" } else { "Local" };
                ui.label(type_label);
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut app.dashboard_state.config.edit_config_name_input);
            });

            ui.add_space(4.0);

            if is_remote {
                ui.horizontal(|ui| {
                    ui.label("URL:");
                    ui.text_edit_singleline(
                        &mut app.dashboard_state.config.edit_config_url_input,
                    );
                });

                ui.horizontal(|ui| {
                    ui.label("Update Interval (min):");
                    ui.text_edit_singleline(
                        &mut app.dashboard_state.config.edit_config_interval_input,
                    );
                });
            }

            ui.add_space(8.0);

            let is_downloading = is_remote
                && app
                    .dashboard_state
                    .config
                    .remote_config_downloading
                    .load(Ordering::Relaxed);

            if ui
                .add_enabled(!is_downloading, egui::Button::new("Save"))
                .clicked()
            {
                let name = app
                    .dashboard_state
                    .config
                    .edit_config_name_input
                    .trim()
                    .to_string();
                if name.is_empty() {
                    push_toast(
                        &app.toasts,
                        ToastKind::Error,
                        "Name cannot be empty".to_string(),
                    );
                } else if let Some(id) = app.dashboard_state.config.edit_config_id {
                    if is_remote {
                        let url =
                            app.dashboard_state.config.edit_config_url_input.trim().to_string();
                        let interval_str =
                            app.dashboard_state.config.edit_config_interval_input.trim();
                        let interval = match interval_str.parse::<u32>() {
                            Ok(v) => v,
                            Err(_) => {
                                push_toast(
                                    &app.toasts,
                                    ToastKind::Error,
                                    "Invalid interval — enter a number in minutes".to_string(),
                                );
                                return;
                            }
                        };
                        if url.is_empty() {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                "URL cannot be empty".to_string(),
                            );
                            return;
                        }
                        // Fetch first, only persist metadata on success
                        let dest = app
                            .settings_manager
                            .configs()
                            .iter()
                            .find(|c| c.id == id)
                            .map(|c| c.path.clone())
                            .unwrap();
                        let client = app.http_client.clone();
                        let toasts = app.toasts.clone();
                        let downloading_flag =
                            app.dashboard_state.config.remote_config_downloading.clone();
                        let pending_edit =
                            app.dashboard_state.config.pending_remote_config_edit.clone();
                        let config_url = url.clone();
                        let config_name = name.clone();

                        downloading_flag.store(true, Ordering::Relaxed);
                        app.runtime.spawn(async move {
                            match download::fetch_remote_config(&client, &config_url, &dest).await {
                                Ok(()) => {
                                    pending_edit.lock().unwrap().push(PendingRemoteConfigEdit {
                                        id,
                                        name: config_name.clone(),
                                        url: config_url,
                                        refresh_interval_minutes: interval,
                                    });
                                    push_toast(
                                        &toasts,
                                        ToastKind::Success,
                                        format!("Remote config \"{config_name}\" updated"),
                                    );
                                }
                                Err(e) => {
                                    push_toast(
                                        &toasts,
                                        ToastKind::Error,
                                        format!("Failed to fetch \"{config_name}\": {e}"),
                                    );
                                }
                            }
                            downloading_flag.store(false, Ordering::Relaxed);
                        });
                    } else {
                        // Local config: verify file still exists on disk
                        let config_path = app
                            .settings_manager
                            .configs()
                            .iter()
                            .find(|c| c.id == id)
                            .map(|c| c.path.clone());
                        if let Some(path) = config_path
                            && !path.is_file()
                        {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                format!("Config file not found: {}", path.display()),
                            );
                        } else {
                            app.settings_manager
                                .update_config(id, name.clone(), ConfigSource::Local);
                            push_toast(
                                &app.toasts,
                                ToastKind::Success,
                                format!("Config \"{name}\" updated"),
                            );
                            app.dashboard_state.config.show_edit_config_window = false;
                        }
                    }
                }
            }
        });

    // Respect both X button (open=false) and Save button (set inside closure)
    app.dashboard_state.config.show_edit_config_window =
        open && app.dashboard_state.config.show_edit_config_window;
}

/// Restart the kernel if it's currently running (e.g. after config switch or refresh).
pub fn restart_kernel_if_running(app: &mut BoxApp) {
    if !app.cached_is_running {
        return;
    }
    if let Some(config) = app.settings_manager.active_config() {
        let path = config.path.clone();
        let working_dir = app.settings_manager.working_dir();
        let elevated = app.settings_manager.run_elevated();
        match app.kernel_manager.restart(&path, working_dir, elevated) {
            Ok(()) => {
                push_toast(
                    &app.toasts,
                    ToastKind::Success,
                    "Kernel restarted".to_string(),
                );
                app.refresh_clash_api_base();
            }
            Err(e) => {
                push_toast(
                    &app.toasts,
                    ToastKind::Error,
                    format!("Restart failed: {e}"),
                );
            }
        }
    }
}

/// Spawn a long-running background task that periodically checks remote configs
/// and refreshes any that are due. Loops every 60 seconds while the flag is set.
pub fn start_config_refresh_task(app: &mut BoxApp) {
    // Collect remote config info needed by the task
    let remote_configs: Vec<(Uuid, String, std::path::PathBuf, u32)> = app
        .settings_manager
        .configs()
        .iter()
        .filter_map(|c| match &c.source {
            ConfigSource::Remote {
                url,
                refresh_interval_minutes,
            } => Some((c.id, url.clone(), c.path.clone(), *refresh_interval_minutes)),
            _ => None,
        })
        .collect();

    if remote_configs.is_empty() {
        return;
    }

    let running_flag = app.dashboard_state.config.refresh_task_running.clone();
    let refreshed_ids = app.dashboard_state.config.refreshed_config_ids.clone();
    let client = app.http_client.clone();
    let toasts = app.toasts.clone();

    running_flag.store(true, Ordering::Relaxed);

    app.runtime.spawn(async move {
        // Initial delay to avoid hammering on startup
        for _ in 0..60 {
            if !running_flag.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }

        // Main loop: check configs every 60 seconds
        loop {
            if !running_flag.load(Ordering::Relaxed) {
                return;
            }

            for &(id, ref url, ref path, interval_minutes) in &remote_configs {
                if interval_minutes == 0 {
                    continue;
                }

                // Check file age — refresh if older than the interval
                let needs_refresh = match std::fs::metadata(path).and_then(|m| m.modified()) {
                    Ok(modified) => {
                        let elapsed = SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or_default();
                        elapsed.as_secs() >= u64::from(interval_minutes) * 60
                    }
                    Err(_) => true, // file missing, re-download
                };

                if !needs_refresh {
                    continue;
                }

                match download::fetch_remote_config(&client, url, path).await {
                    Ok(()) => {
                        tracing::info!("Auto-refreshed config from {url}");
                        refreshed_ids.lock().unwrap().push(id);
                    }
                    Err(e) => {
                        tracing::warn!("Auto-refresh failed for {url}: {e}");
                        push_toast(
                            &toasts,
                            ToastKind::Error,
                            format!("Auto-refresh failed: {e}"),
                        );
                    }
                }
            }

            // Sleep 60s in 1-second chunks so the task can exit promptly when flagged
            for _ in 0..60 {
                if !running_flag.load(Ordering::Relaxed) {
                    return;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        }
    });
}

fn rfd_pick_file(title: &str) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_file()
}

/// Return a human-readable relative time string for a file's last modification.
/// Format: "1m ago", "30m ago", "2h ago", "1d ago", "365d ago".
/// Minimum unit is minutes, maximum unit is days.
fn file_age_text(path: &std::path::Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = SystemTime::now().duration_since(modified).ok()?;
    let total_minutes = elapsed.as_secs() / 60;

    Some(if total_minutes < 1 {
        "<1m ago".to_string()
    } else if total_minutes < 60 {
        format!("{total_minutes}m ago")
    } else {
        let hours = total_minutes / 60;
        if hours < 24 {
            format!("{hours}h ago")
        } else {
            let days = hours / 24;
            format!("{days}d ago")
        }
    })
}
