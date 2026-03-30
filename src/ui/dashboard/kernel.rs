use eframe::egui;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::{BoxApp, ToastKind, push_toast};
use crate::core::download;

/// Info about a kernel that was just installed by an async task.
pub struct PendingKernelInstall {
    pub tag: String,
    pub path: std::path::PathBuf,
}

pub struct KernelState {
    /// Releases fetched from GitHub
    pub releases: Arc<Mutex<Vec<download::Release>>>,
    /// Download progress: 0 = idle, 1..=1000 = permille progress
    pub download_progress: Arc<AtomicU32>,
    /// Whether the releases window is open
    pub show_releases_window: bool,
    /// Pending kernel install info from async download task
    pub pending_kernel_install: Arc<Mutex<Option<PendingKernelInstall>>>,
}

impl Default for KernelState {
    fn default() -> Self {
        Self {
            releases: Arc::new(Mutex::new(Vec::new())),
            download_progress: Arc::new(AtomicU32::new(0)),
            show_releases_window: false,
            pending_kernel_install: Arc::new(Mutex::new(None)),
        }
    }
}

pub fn show_kernel_manager(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.label("Core Management");
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Version:");
        let kernel_names = app.settings_manager.kernel_names();
        let mut selected = app
            .settings_manager
            .active_kernel_name()
            .unwrap_or_default()
            .to_string();
        let prev_selected = selected.clone();
        let available_width = ui.available_width() - 90.0; // reserve space for "+ Download" button
        fn strip_exe(name: &str) -> &str {
            name.strip_suffix(".exe").unwrap_or(name)
        }
        egui::ComboBox::from_id_salt("kernel_version")
            .selected_text(if selected.is_empty() {
                "None"
            } else {
                strip_exe(&selected)
            })
            .width(available_width.max(100.0))
            .show_ui(ui, |ui| {
                for name in &kernel_names {
                    ui.selectable_value(&mut selected, name.clone(), strip_exe(name));
                }
            });
        if selected != prev_selected {
            app.settings_manager.set_active_kernel(&selected);
            app.kernel_manager
                .set_kernel_path(app.settings_manager.active_kernel_path());
        }

        if ui.small_button("+ Download").clicked() {
            app.dashboard_state.kernel.show_releases_window = true;
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
                    match app.kernel_manager.start(&path, working_dir, elevated) {
                        Ok(()) => {
                            push_toast(
                                &app.toasts,
                                ToastKind::Success,
                                "Kernel started".to_string(),
                            );
                            app.refresh_clash_api_base();
                        }
                        Err(e) => {
                            push_toast(
                                &app.toasts,
                                ToastKind::Error,
                                format!("Start failed: {e}"),
                            );
                        }
                    }
                } else {
                    push_toast(
                        &app.toasts,
                        ToastKind::Error,
                        "No active config selected".to_string(),
                    );
                }
            }
        } else if ui.button("⏹ Stop").clicked() {
            match app.kernel_manager.stop() {
                Ok(()) => {
                    push_toast(
                        &app.toasts,
                        ToastKind::Success,
                        "Kernel stopped".to_string(),
                    );
                }
                Err(e) => {
                    push_toast(
                        &app.toasts,
                        ToastKind::Error,
                        format!("Stop failed: {e}"),
                    );
                }
            }
        }

        if ui.button("🔄 Restart").clicked() {
            if let Some(config) = app.settings_manager.active_config() {
                let path = config.path.clone();
                let working_dir = app.settings_manager.working_dir();
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

    if run_elevated {
        if crate::core::permissions::has_kernel_permissions(
            app.settings_manager
                .active_kernel_path()
                .as_deref()
                .unwrap_or(std::path::Path::new("")),
        ) {
            ui.weak("No password needed");
        } else {
            #[cfg(target_os = "windows")]
            if !crate::core::permissions::is_elevated() {
                ui.horizontal(|ui| {
                    ui.weak("Not elevated.");
                    if ui.small_button("Relaunch as Admin").clicked()
                        && let Err(e) = crate::core::permissions::relaunch_elevated()
                    {
                        push_toast(
                            &app.toasts,
                            ToastKind::Error,
                            format!("Relaunch failed: {e}"),
                        );
                    }
                });
            }

            #[cfg(unix)]
            ui.weak("Will prompt for password on first start");
        }
    }
}

pub fn show_releases_window(ctx: &egui::Context, app: &mut BoxApp) {
    let mut open = app.dashboard_state.kernel.show_releases_window;
    if !open {
        return;
    }

    let progress_val = app
        .dashboard_state
        .kernel
        .download_progress
        .load(Ordering::Relaxed);
    let is_downloading = progress_val > 0;

    let installed_versions = app.settings_manager.installed_kernel_versions();

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
                let releases = app.dashboard_state.kernel.releases.lock().unwrap();
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
                let has_releases = !app.dashboard_state.kernel.releases.lock().unwrap().is_empty();
                if has_releases {
                    ui.label("No matching releases for this platform.");
                } else {
                    ui.label("Loading...");
                }
            }

            egui::ScrollArea::vertical()
                .max_width(f32::INFINITY)
                .show(ui, |ui| {
                    let mut delete_tag: Option<String> = None;
                    for (tag, url, asset_name) in &release_items {
                        ui.horizontal(|ui| {
                            ui.label(tag);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let is_installed = installed_versions.iter().any(|v| v == tag);
                                    if is_installed {
                                        ui.add_enabled(false, egui::Button::new("Installed"));
                                        if ui.small_button("🗑").clicked() {
                                            delete_tag = Some(tag.clone());
                                        }
                                    } else if ui
                                        .add_enabled(!is_downloading, egui::Button::new("Install"))
                                        .clicked()
                                    {
                                        download_and_install_kernel(app, url, tag, asset_name);
                                    }
                                },
                            );
                        });
                    }
                    if let Some(tag) = delete_tag {
                        delete_kernel(app, &tag);
                    }
                });
        });

    app.dashboard_state.kernel.show_releases_window = open;
}

/// Persist and activate kernel if async install completed.
pub fn process_pending_kernel(app: &mut BoxApp) {
    if let Some(install) = app
        .dashboard_state
        .kernel
        .pending_kernel_install
        .lock()
        .unwrap()
        .take()
    {
        let name = install
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or(install.tag);
        app.settings_manager.activate_new_kernel(&name);
        app.kernel_manager.set_kernel_path(Some(install.path));
    }
}

fn fetch_releases(app: &mut BoxApp) {
    let releases = app.dashboard_state.kernel.releases.clone();
    let client = app.http_client.clone();
    let toasts = app.toasts.clone();

    app.runtime.spawn(async move {
        match download::fetch_releases(&client).await {
            Ok(list) => {
                *releases.lock().unwrap() = list;
            }
            Err(e) => {
                push_toast(
                    &toasts,
                    ToastKind::Error,
                    format!("Fetch releases failed: {e}"),
                );
            }
        }
    });
}

fn download_and_install_kernel(app: &mut BoxApp, url: &str, tag: &str, asset_name: &str) {
    let progress = app.dashboard_state.kernel.download_progress.clone();
    progress.store(1, Ordering::Relaxed);

    let client = app.http_client.clone();
    let toasts = app.toasts.clone();
    let url = url.to_string();
    let tag = tag.to_string();
    let asset_name = asset_name.to_string();
    let kernels_dir = app.settings_manager.kernels_dir().to_path_buf();
    let pending_install = app.dashboard_state.kernel.pending_kernel_install.clone();

    app.runtime.spawn(async move {
        std::fs::create_dir_all(&kernels_dir).ok();
        let dest = kernels_dir.join(&asset_name);

        match download::download_asset_with_progress(&client, &url, &dest, &progress).await {
            Ok(()) => match download::extract_kernel(&dest, &kernels_dir, &tag) {
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
                    push_toast(&toasts, ToastKind::Error, format!("Extraction failed: {e}"));
                }
            },
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

fn delete_kernel(app: &mut BoxApp, tag: &str) {
    // Resolve the actual filename: on Windows kernels have a `.exe` suffix.
    let filename = if cfg!(target_os = "windows") {
        format!("{tag}.exe")
    } else {
        tag.to_string()
    };
    let is_active = app.settings_manager.active_kernel_name() == Some(&filename);
    if is_active && app.cached_is_running {
        push_toast(
            &app.toasts,
            ToastKind::Error,
            "Cannot delete the running kernel".to_string(),
        );
        return;
    }
    app.settings_manager.remove_kernel(&filename);
    if is_active {
        app.kernel_manager.set_kernel_path(None);
    }
    push_toast(
        &app.toasts,
        ToastKind::Success,
        format!("Kernel {tag} deleted"),
    );
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
