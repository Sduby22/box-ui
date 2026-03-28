use eframe::egui;

use crate::app::{push_toast, BoxApp, ToastKind};
use crate::core::autostart;

pub struct SettingsState {
    pub autostart_enabled: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            autostart_enabled: autostart::is_registered(),
        }
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.heading("Settings");
    ui.add_space(8.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        // Autostart
        ui.group(|ui| {
            ui.label("Autostart");
            ui.add_space(4.0);

            let mut autostart = app.settings_state.autostart_enabled;
            if ui
                .checkbox(&mut autostart, "Launch on login")
                .changed()
            {
                let result = if autostart {
                    autostart::register()
                } else {
                    autostart::unregister()
                };
                match result {
                    Ok(()) => {
                        app.settings_state.autostart_enabled = autostart;
                    }
                    Err(e) => {
                        push_toast(&app.toasts, ToastKind::Error, format!("Autostart failed: {e}"));
                    }
                }
            }

            let mut launch_core = app.settings_manager.launch_core_on_start();
            if ui
                .checkbox(&mut launch_core, "Launch core on start")
                .on_hover_text("Automatically start the sing-box core when the app launches")
                .changed()
            {
                app.settings_manager.set_launch_core_on_start(launch_core);
            }
        });

        ui.add_space(8.0);

        // Helper Daemon
        ui.group(|ui| {
            ui.label("Privileged Helper Daemon");
            ui.add_space(4.0);

            let installed = crate::core::helper_install::is_installed();
            let available = if installed {
                crate::core::helper_client::HelperClient::is_available()
            } else {
                false
            };

            ui.horizontal(|ui| {
                ui.label("Status:");
                if !installed {
                    ui.colored_label(egui::Color32::GRAY, "Not installed");
                } else if available {
                    ui.colored_label(
                        egui::Color32::from_rgb(80, 200, 120),
                        "Installed & Running",
                    );
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 200, 50),
                        "Installed (idle)",
                    );
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if !installed {
                    if ui.button("Install Helper").clicked() {
                        match crate::core::helper_install::install_helper() {
                            Ok(()) => {
                                push_toast(
                                    &app.toasts,
                                    ToastKind::Success,
                                    "Helper daemon installed".to_string(),
                                );
                            }
                            Err(e) => {
                                push_toast(
                                    &app.toasts,
                                    ToastKind::Error,
                                    format!("Install failed: {e}"),
                                );
                            }
                        }
                    }
                } else {
                    if ui.button("Uninstall Helper").clicked() {
                        match crate::core::helper_install::uninstall_helper() {
                            Ok(()) => {
                                push_toast(
                                    &app.toasts,
                                    ToastKind::Success,
                                    "Helper daemon uninstalled".to_string(),
                                );
                            }
                            Err(e) => {
                                push_toast(
                                    &app.toasts,
                                    ToastKind::Error,
                                    format!("Uninstall failed: {e}"),
                                );
                            }
                        }
                    }
                }
            });

            ui.add_space(2.0);
            ui.weak("The helper daemon runs with root privileges to start sing-box without requiring a password each time. It only runs when needed and auto-exits when idle.");
        });

        ui.add_space(8.0);

        // About
        ui.group(|ui| {
            ui.label("About");
            ui.add_space(4.0);
            ui.label("Box UI - A lightweight GUI for sing-box");
            ui.label(format!("Version: {}", env!("CARGO_PKG_VERSION")));
        });
    });
}
