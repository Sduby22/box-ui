use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use uuid::Uuid;

use crate::app::{push_toast, BoxApp, ToastKind};
use crate::core::download;
use crate::core::settings::ConfigSource;

const MAX_TRAFFIC_POINTS: usize = 180;

#[derive(Clone)]
pub struct TrafficPoint {
    pub time: f64,
    pub upload: f64,
    pub download: f64,
}

/// Info about a kernel that was just installed by an async task.
pub struct PendingKernelInstall {
    pub tag: String,
    pub path: std::path::PathBuf,
}

/// Info about a remote config that was successfully downloaded by an async task.
pub struct PendingRemoteConfig {
    pub name: String,
    pub url: String,
    pub refresh_interval_minutes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddConfigType {
    Local,
    Remote,
}

pub struct DashboardState {
    pub traffic_history: Arc<Mutex<VecDeque<TrafficPoint>>>,
    pub traffic_polling: bool,
    /// Shared with async task so it can signal when the stream ends
    pub polling_flag: Arc<AtomicBool>,
    /// Releases fetched from GitHub
    pub releases: Arc<Mutex<Vec<download::Release>>>,
    /// Download progress: 0 = idle, 1..=1000 = permille progress
    pub download_progress: Arc<AtomicU32>,
    /// Whether the releases window is open
    pub show_releases_window: bool,
    /// Pending kernel install info from async download task
    pub pending_kernel_install: Arc<Mutex<Option<PendingKernelInstall>>>,
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
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            traffic_history: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_TRAFFIC_POINTS))),
            traffic_polling: false,
            polling_flag: Arc::new(AtomicBool::new(false)),
            releases: Arc::new(Mutex::new(Vec::new())),
            download_progress: Arc::new(AtomicU32::new(0)),
            show_releases_window: false,
            pending_kernel_install: Arc::new(Mutex::new(None)),
            show_add_config_window: false,
            add_config_type: AddConfigType::Local,
            add_config_name_input: String::new(),
            add_config_path_input: String::new(),
            add_config_url_input: String::new(),
            add_config_interval_input: "60".to_string(),
            pending_remote_config: Arc::new(Mutex::new(Vec::new())),
            remote_config_downloading: Arc::new(AtomicBool::new(false)),
            show_edit_config_window: false,
            edit_config_id: None,
            edit_config_type: AddConfigType::Local,
            edit_config_name_input: String::new(),
            edit_config_url_input: String::new(),
            edit_config_interval_input: "60".to_string(),
        }
    }
}

impl DashboardState {
    pub fn current_speed(&self) -> (f64, f64) {
        let history = self.traffic_history.lock().unwrap();
        history
            .back()
            .map(|p| (p.upload, p.download))
            .unwrap_or((0.0, 0.0))
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.heading("Dashboard");
    ui.add_space(8.0);

    // Traffic chart
    ui.group(|ui| {
        ui.label("Traffic Speed");
        let history = app.dashboard_state.traffic_history.lock().unwrap();
        let upload_points: PlotPoints = history
            .iter()
            .map(|p| [p.time, p.upload])
            .collect();
        let download_points: PlotPoints = history
            .iter()
            .map(|p| [p.time, p.download])
            .collect();
        let history_snapshot: Vec<TrafficPoint> = history.iter().cloned().collect();
        drop(history);

        let upload_line = Line::new("Upload", upload_points)
            .color(egui::Color32::from_rgb(100, 180, 255));
        let download_line = Line::new("Download", download_points)
            .color(egui::Color32::from_rgb(100, 255, 150));

        Plot::new("traffic_plot")
            .height(200.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .show_axes(true)
            .y_axis_formatter(|mark, _range| format_speed_axis(mark.value))
            .label_formatter(move |_name, value| {
                // Find the closest point by time (x axis)
                let closest = history_snapshot
                    .iter()
                    .min_by(|a, b| {
                        (a.time - value.x)
                            .abs()
                            .partial_cmp(&(b.time - value.x).abs())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                if let Some(p) = closest {
                    format!(
                        "↑ {}\n↓ {}",
                        crate::core::format_speed(p.upload),
                        crate::core::format_speed(p.download),
                    )
                } else {
                    String::new()
                }
            })
            .legend(egui_plot::Legend::default())
            .show(ui, |plot_ui| {
                plot_ui.line(upload_line);
                plot_ui.line(download_line);
            });
    });

    ui.add_space(12.0);

    // Configuration + Core management side by side
    ui.columns(2, |columns| {
        // Left column: Configuration
        columns[0].group(|ui| {
            ui.horizontal(|ui| {
                ui.label("Configuration");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("+ Add").clicked() {
                        app.dashboard_state.show_add_config_window = true;
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
                            app.dashboard_state.edit_config_id = Some(config.id);
                            app.dashboard_state.edit_config_name_input =
                                config.name.clone();
                            match &config.source {
                                ConfigSource::Local => {
                                    app.dashboard_state.edit_config_type = AddConfigType::Local;
                                    app.dashboard_state.edit_config_url_input.clear();
                                    app.dashboard_state.edit_config_interval_input =
                                        "60".to_string();
                                }
                                ConfigSource::Remote {
                                    url,
                                    refresh_interval_minutes,
                                } => {
                                    app.dashboard_state.edit_config_type = AddConfigType::Remote;
                                    app.dashboard_state.edit_config_url_input = url.clone();
                                    app.dashboard_state.edit_config_interval_input =
                                        refresh_interval_minutes.to_string();
                                }
                            }
                            app.dashboard_state.show_edit_config_window = true;
                        }
                        if ui.small_button("📄").clicked() {
                            let path = config.path.clone();
                            std::thread::spawn(move || {
                                open::that(&path).ok();
                            });
                        }
                        if let ConfigSource::Remote { .. } = &config.source
                            && let Some(ago_text) = file_age_text(&config.path)
                        {
                            ui.weak(format!("🕐 {ago_text}"));
                        }
                    });
                });
            }

            match action {
                Some(ConfigAction::SetActive(id)) => {
                    app.settings_manager.set_active_config(id);
                    app.refresh_clash_api_base();
                }
                Some(ConfigAction::Remove(id)) => app.settings_manager.remove_config(id),
                None => {}
            }
        });

        // Right column: Core Management
        columns[1].group(|ui| {
            ui.label("Core Management");
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("Version:");
                let mut kernel_names: Vec<String> = app
                    .settings_manager
                    .kernels()
                    .iter()
                    .map(|k| k.name.clone())
                    .collect();
                kernel_names.sort_by(|a, b| b.cmp(a));
                let mut selected = app
                    .settings_manager
                    .active_kernel()
                    .map(|k| k.name.clone())
                    .unwrap_or_default();
                let prev_selected = selected.clone();
                let available_width = ui.available_width() - 90.0; // reserve space for "+ Download" button
                egui::ComboBox::from_id_salt("kernel_version")
                    .selected_text(if selected.is_empty() {
                        "None"
                    } else {
                        &selected
                    })
                    .width(available_width.max(100.0))
                    .show_ui(ui, |ui| {
                        for name in &kernel_names {
                            ui.selectable_value(&mut selected, name.clone(), name);
                        }
                    });
                if selected != prev_selected {
                    app.settings_manager.set_active_kernel(&selected);
                    let path = app
                        .settings_manager
                        .active_kernel_path()
                        .map(|p| p.to_path_buf());
                    app.kernel_manager.set_kernel_path(path);
                }

                if ui.small_button("+ Download").clicked() {
                    app.dashboard_state.show_releases_window = true;
                    fetch_releases(app);
                }
            });

            ui.horizontal(|ui| {
                let is_running = app.cached_is_running;
                let elevated = app.settings_manager.run_elevated();

                if !is_running {
                    if ui.button("▶ Start").clicked() {
                        if let Some(config) = app.settings_manager.active_config() {
                            let path = config.path.clone();
                            let working_dir = app.settings_manager.working_dir();
                            if let Err(e) = app.kernel_manager.start(
                                &path,
                                working_dir,
                                elevated,
                            ) {
                                push_toast(
                                    &app.toasts,
                                    ToastKind::Error,
                                    format!("Start failed: {e}"),
                                );
                            } else {
                                app.refresh_clash_api_base();
                            }
                        } else {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                "No active config selected".to_string(),
                            );
                        }
                    }
                } else if ui.button("⏹ Stop").clicked()
                    && let Err(e) = app.kernel_manager.stop()
                {
                    push_toast(&app.toasts, ToastKind::Error, format!("Stop failed: {e}"));
                }

                if ui.button("🔄 Restart").clicked() {
                    if let Some(config) = app.settings_manager.active_config() {
                        let path = config.path.clone();
                        let working_dir = app.settings_manager.working_dir();
                        if let Err(e) = app.kernel_manager.restart(
                            &path,
                            working_dir,
                            elevated,
                        ) {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                format!("Restart failed: {e}"),
                            );
                        } else {
                            app.refresh_clash_api_base();
                        }
                    } else {
                        push_toast(
                            &app.toasts,
                            ToastKind::Error,
                            "No active config selected".to_string(),
                        );
                    }
                }
            });

            let mut run_elevated = app.settings_manager.run_elevated();
            if ui
                .checkbox(&mut run_elevated, "Run as Admin")
                .on_hover_text("Run the kernel with elevated privileges (sudo/admin)")
                .changed()
            {
                app.settings_manager.set_run_elevated(run_elevated);
            }

            // Show helper install prompt when elevated mode is on
            if run_elevated {
                let installed = crate::core::helper_install::is_installed();
                if !installed {
                    ui.horizontal(|ui| {
                        ui.weak("Helper not installed.");
                        if ui.small_button("Install Helper").clicked() {
                            match crate::core::helper_install::install_helper() {
                                Ok(()) => {
                                    push_toast(
                                        &app.toasts,
                                        ToastKind::Success,
                                        "Helper daemon installed successfully".to_string(),
                                    );
                                }
                                Err(e) => {
                                    push_toast(
                                        &app.toasts,
                                        ToastKind::Error,
                                        format!("Helper install failed: {e}"),
                                    );
                                }
                            }
                        }
                    });
                } else {
                    ui.weak("Helper installed (no password needed)");
                }
            }
        });
    });

    // Persist and activate kernel if async install completed
    if let Some(install) = app.dashboard_state.pending_kernel_install.lock().unwrap().take() {
        app.settings_manager
            .add_kernel_remote(install.tag, install.path.clone());
        app.kernel_manager.set_kernel_path(Some(install.path));
    }

    // Persist remote configs that were successfully downloaded
    let mut pending_configs = app.dashboard_state.pending_remote_config.lock().unwrap();
    if !pending_configs.is_empty() {
        for pending in pending_configs.drain(..) {
            app.settings_manager.add_remote_config(
                pending.name,
                pending.url,
                pending.refresh_interval_minutes,
            );
        }
        // Close the dialog and clear inputs on successful download
        app.dashboard_state.show_add_config_window = false;
        app.dashboard_state.add_config_name_input.clear();
        app.dashboard_state.add_config_url_input.clear();
        app.dashboard_state.add_config_interval_input = "60".to_string();
    }
    drop(pending_configs);

    // Modal windows
    show_releases_window(ui.ctx(), app);
    show_add_config_window(ui.ctx(), app);
    show_edit_config_window(ui.ctx(), app);

    // Sync polling state from async task
    app.dashboard_state.traffic_polling =
        app.dashboard_state.polling_flag.load(Ordering::Relaxed);

    // Start traffic polling if core is running, window visible, and not already polling
    if app.cached_is_running
        && app.window_visible.load(std::sync::atomic::Ordering::Relaxed)
        && !app.dashboard_state.traffic_polling
    {
        start_traffic_polling(app);
    }
}

enum ConfigAction {
    SetActive(Uuid),
    Remove(Uuid),
}

fn show_releases_window(ctx: &egui::Context, app: &mut BoxApp) {
    let mut open = app.dashboard_state.show_releases_window;
    if !open {
        return;
    }

    let progress_val = app.dashboard_state.download_progress.load(Ordering::Relaxed);
    let is_downloading = progress_val > 0;

    let installed_versions: Vec<String> = app
        .settings_manager
        .installed_kernel_versions()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    egui::Window::new("Download Kernel")
        .open(&mut open)
        .resizable(true)
        .default_width(400.0)
        .show(ctx, |ui| {
            ui.label(format!("Platform: {}", current_platform_keyword()));
            ui.add_space(4.0);

            if ui.button("Refresh").clicked() {
                fetch_releases(app);
            }

            ui.add_space(8.0);

            if is_downloading {
                let fraction = progress_val as f32 / 1000.0;
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(format!("Downloading... {:.1}%", fraction * 100.0))
                        .animate(true),
                );
                ui.add_space(4.0);
            }

            let release_items: Vec<(String, String, String)> = {
                let releases = app.dashboard_state.releases.lock().unwrap();
                let platform_keyword = current_platform_keyword();
                releases
                    .iter()
                    .filter_map(|release| {
                        release
                            .assets
                            .iter()
                            .find(|a| {
                                let name = a.name.to_lowercase();
                                name.contains(&platform_keyword) && !name.ends_with(".sha256")
                            })
                            .map(|asset| {
                                (
                                    release.tag_name.clone(),
                                    asset.browser_download_url.clone(),
                                    asset.name.clone(),
                                )
                            })
                    })
                    .collect()
            };

            if release_items.is_empty() {
                let has_releases = !app.dashboard_state.releases.lock().unwrap().is_empty();
                if has_releases {
                    ui.label("No matching releases for this platform.");
                } else {
                    ui.label("Loading...");
                }
            }

            egui::ScrollArea::vertical().max_width(f32::INFINITY).show(ui, |ui| {
                for (tag, url, asset_name) in &release_items {
                    ui.horizontal(|ui| {
                        ui.label(tag);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let is_installed = installed_versions.iter().any(|v| v == tag);
                            if is_installed {
                                ui.add_enabled(false, egui::Button::new("Installed"));
                            } else if ui
                                .add_enabled(!is_downloading, egui::Button::new("Install"))
                                .clicked()
                            {
                                download_and_install_kernel(app, url, tag, asset_name);
                            }
                        });
                    });
                }
            });
        });

    app.dashboard_state.show_releases_window = open;
}

fn show_add_config_window(ctx: &egui::Context, app: &mut BoxApp) {
    let mut open = app.dashboard_state.show_add_config_window;
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
                    &mut app.dashboard_state.add_config_type,
                    AddConfigType::Local,
                    "Local",
                );
                ui.radio_value(
                    &mut app.dashboard_state.add_config_type,
                    AddConfigType::Remote,
                    "Remote",
                );
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut app.dashboard_state.add_config_name_input);
            });

            ui.add_space(4.0);

            match app.dashboard_state.add_config_type {
                AddConfigType::Local => {
                    ui.horizontal(|ui| {
                        ui.label("Path:");
                        ui.text_edit_singleline(&mut app.dashboard_state.add_config_path_input);
                        if ui.button("Browse").clicked()
                            && let Some(path) = rfd_pick_file("Select config file")
                        {
                            app.dashboard_state.add_config_path_input =
                                path.to_string_lossy().to_string();
                        }
                    });

                    ui.add_space(8.0);

                    if ui.button("Add").clicked() {
                        let name = app.dashboard_state.add_config_name_input.trim().to_string();
                        let path_str = app.dashboard_state.add_config_path_input.trim();
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
                                    app.dashboard_state.add_config_name_input.clear();
                                    app.dashboard_state.add_config_path_input.clear();
                                    app.dashboard_state.show_add_config_window = false;
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
                        ui.text_edit_singleline(&mut app.dashboard_state.add_config_url_input);
                    });

                    ui.horizontal(|ui| {
                        ui.label("Update Interval (min):");
                        ui.text_edit_singleline(
                            &mut app.dashboard_state.add_config_interval_input,
                        );
                    });

                    ui.add_space(8.0);

                    let is_downloading = app
                        .dashboard_state
                        .remote_config_downloading
                        .load(Ordering::Relaxed);

                    if ui
                        .add_enabled(!is_downloading, egui::Button::new("Add"))
                        .clicked()
                    {
                        let name = app.dashboard_state.add_config_name_input.trim().to_string();
                        let url = app.dashboard_state.add_config_url_input.trim().to_string();
                        let interval_str = app.dashboard_state.add_config_interval_input.trim();

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
                            let dest = app.settings_manager.config_path(&name);

                            let client = app.http_client.clone();
                            let toasts = app.toasts.clone();
                            let pending = app.dashboard_state.pending_remote_config.clone();
                            let downloading_flag =
                                app.dashboard_state.remote_config_downloading.clone();
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

    app.dashboard_state.show_add_config_window = open;
}

fn show_edit_config_window(ctx: &egui::Context, app: &mut BoxApp) {
    let mut open = app.dashboard_state.show_edit_config_window;
    if !open {
        return;
    }

    egui::Window::new("Edit Configuration")
        .open(&mut open)
        .resizable(false)
        .default_width(350.0)
        .show(ctx, |ui| {
            let is_remote = app.dashboard_state.edit_config_type == AddConfigType::Remote;

            ui.horizontal(|ui| {
                ui.label("Type:");
                let type_label = if is_remote { "Remote" } else { "Local" };
                ui.label(type_label);
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut app.dashboard_state.edit_config_name_input);
            });

            ui.add_space(4.0);

            if is_remote {
                ui.horizontal(|ui| {
                    ui.label("URL:");
                    ui.text_edit_singleline(&mut app.dashboard_state.edit_config_url_input);
                });

                ui.horizontal(|ui| {
                    ui.label("Update Interval (min):");
                    ui.text_edit_singleline(
                        &mut app.dashboard_state.edit_config_interval_input,
                    );
                });
            }

            ui.add_space(8.0);

            if ui.button("Save").clicked() {
                let name = app.dashboard_state.edit_config_name_input.trim().to_string();
                if name.is_empty() {
                    push_toast(
                        &app.toasts,
                        ToastKind::Error,
                        "Name cannot be empty".to_string(),
                    );
                } else if let Some(id) = app.dashboard_state.edit_config_id {
                    let source = if is_remote {
                        let url = app.dashboard_state.edit_config_url_input.trim().to_string();
                        let interval_str =
                            app.dashboard_state.edit_config_interval_input.trim();
                        match interval_str.parse::<u32>() {
                            Ok(interval) => ConfigSource::Remote {
                                url,
                                refresh_interval_minutes: interval,
                            },
                            Err(_) => {
                                push_toast(
                                    &app.toasts,
                                    ToastKind::Error,
                                    "Invalid interval — enter a number in minutes".to_string(),
                                );
                                return;
                            }
                        }
                    } else {
                        ConfigSource::Local
                    };
                    app.settings_manager.update_config(id, name.clone(), source);
                    push_toast(
                        &app.toasts,
                        ToastKind::Success,
                        format!("Config \"{name}\" updated"),
                    );
                    app.dashboard_state.show_edit_config_window = false;
                }
            }
        });

    app.dashboard_state.show_edit_config_window = open;
}

fn fetch_releases(app: &mut BoxApp) {
    let releases = app.dashboard_state.releases.clone();
    let client = app.http_client.clone();
    let toasts = app.toasts.clone();

    app.runtime.spawn(async move {
        match download::fetch_releases(&client).await {
            Ok(list) => {
                *releases.lock().unwrap() = list;
            }
            Err(e) => {
                push_toast(&toasts, ToastKind::Error, format!("Fetch releases failed: {e}"));
            }
        }
    });
}

fn download_and_install_kernel(app: &mut BoxApp, url: &str, tag: &str, asset_name: &str) {
    let progress = app.dashboard_state.download_progress.clone();
    progress.store(1, Ordering::Relaxed);

    let client = app.http_client.clone();
    let toasts = app.toasts.clone();
    let url = url.to_string();
    let tag = tag.to_string();
    let asset_name = asset_name.to_string();
    let kernels_dir = app.settings_manager.kernels_dir().to_path_buf();
    let pending_install = app.dashboard_state.pending_kernel_install.clone();

    app.runtime.spawn(async move {
        std::fs::create_dir_all(&kernels_dir).ok();
        let dest = kernels_dir.join(&asset_name);

        match download::download_asset_with_progress(&client, &url, &dest, &progress).await {
            Ok(()) => {
                match download::extract_kernel(&dest, &kernels_dir, &tag) {
                    Ok(kernel_path) => {
                        *pending_install.lock().unwrap() = Some(PendingKernelInstall {
                            tag: tag.clone(),
                            path: kernel_path,
                        });
                        push_toast(
                            &toasts,
                            ToastKind::Success,
                            format!("Kernel {tag} installed"),
                        );
                    }
                    Err(e) => {
                        push_toast(
                            &toasts,
                            ToastKind::Error,
                            format!("Extraction failed: {e}"),
                        );
                    }
                }
            }
            Err(e) => {
                push_toast(
                    &toasts,
                    ToastKind::Error,
                    format!("Kernel download failed: {e}"),
                );
                std::fs::remove_file(&dest).ok();
            }
        }

        progress.store(0, Ordering::Relaxed);
    });
}

fn start_traffic_polling(app: &mut BoxApp) {
    app.dashboard_state.traffic_polling = true;
    let history = app.dashboard_state.traffic_history.clone();
    let polling_flag = app.dashboard_state.polling_flag.clone();
    let base_url = app.clash_api_base.clone();
    let secret = app.clash_api_secret.clone();

    polling_flag.store(true, Ordering::Relaxed);

    app.runtime.spawn(async move {
        let mut ws_url = format!(
            "{}/traffic",
            base_url.replacen("http", "ws", 1)
        );
        if !secret.is_empty() {
            ws_url.push_str(&format!("?token={secret}"));
        }

        let (ws_stream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to connect traffic WebSocket: {e}");
                polling_flag.store(false, Ordering::Relaxed);
                return;
            }
        };

        use futures_util::StreamExt;
        let (_, mut read) = ws_stream.split();
        let mut counter = 0.0f64;

        while polling_flag.load(Ordering::Relaxed) {
            match read.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    if let Ok(traffic) = serde_json::from_str::<serde_json::Value>(&text) {
                        let up = traffic["up"].as_f64().unwrap_or(0.0);
                        let down = traffic["down"].as_f64().unwrap_or(0.0);
                        let mut h = history.lock().unwrap();
                        if h.len() >= MAX_TRAFFIC_POINTS {
                            h.pop_front();
                        }
                        counter += 1.0;
                        h.push_back(TrafficPoint {
                            time: counter,
                            upload: up,
                            download: down,
                        });
                    }
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                Some(Err(e)) => {
                    tracing::error!("Traffic WebSocket error: {e}");
                    break;
                }
                _ => {}
            }
        }

        polling_flag.store(false, Ordering::Relaxed);
    });
}

fn current_platform_keyword() -> String {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };

    format!("{os}-{arch}")
}

fn rfd_pick_file(title: &str) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_file()
}

/// Format a Y-axis value as KB/s or MB/s.
fn format_speed_axis(bytes_per_sec: f64) -> String {
    let abs = bytes_per_sec.abs();
    if abs < 1024.0 * 1024.0 {
        format!("{:.0} KB/s", bytes_per_sec / 1024.0)
    } else {
        format!("{:.1} MB/s", bytes_per_sec / (1024.0 * 1024.0))
    }
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
