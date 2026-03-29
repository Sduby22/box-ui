use eframe::egui;
use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::app::BoxApp;

const AUTO_REFRESH_INTERVAL_SECS: u64 = 2;

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyGroup {
    pub name: String,
    #[serde(rename = "type")]
    pub group_type: String,
    pub now: Option<String>,
    pub all: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProxiesResponse {
    proxies: IndexMap<String, ProxyGroup>,
}

pub struct OutboundsState {
    pub groups: Arc<Mutex<Vec<ProxyGroup>>>,
    pub loading: Arc<AtomicBool>,
    pub last_error: Option<String>,
    pub last_fetch: Option<Instant>,
    /// Groups that are expanded (default is collapsed).
    pub expanded: HashSet<String>,
}

impl Default for OutboundsState {
    fn default() -> Self {
        Self {
            groups: Arc::new(Mutex::new(Vec::new())),
            loading: Arc::new(AtomicBool::new(false)),
            last_error: None,
            last_fetch: None,
            expanded: HashSet::new(),
        }
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.heading("Outbounds");
    ui.add_space(8.0);

    // Auto-refresh (only when window is visible)
    let should_fetch = app.window_visible.load(Ordering::Relaxed)
        && !app.outbounds_state.loading.load(Ordering::Relaxed)
        && app
            .outbounds_state
            .last_fetch
            .is_none_or(|t| t.elapsed().as_secs() >= AUTO_REFRESH_INTERVAL_SECS);
    if should_fetch {
        fetch_proxies(app);
    }

    if let Some(ref err) = app.outbounds_state.last_error {
        ui.colored_label(egui::Color32::RED, err);
    }

    // Hold lock during rendering instead of cloning
    let groups = app.outbounds_state.groups.lock().unwrap();

    // Sort indices so "Global" (exact match) appears last
    let mut sorted_indices: Vec<usize> = (0..groups.len()).collect();
    sorted_indices.sort_by(|&a, &b| {
        let a_global = groups[a].name == "GLOBAL";
        let b_global = groups[b].name == "GLOBAL";
        a_global.cmp(&b_global)
    });

    // Collect group names for collapse all / expand all
    let all_group_names: Vec<String> = groups
        .iter()
        .filter(|g| g.all.as_ref().is_some_and(|n| !n.is_empty()))
        .map(|g| g.name.clone())
        .collect();

    ui.horizontal(|ui| {
        if ui.button("Collapse All").clicked() {
            app.outbounds_state.expanded.clear();
        }
        if ui.button("Expand All").clicked() {
            for name in &all_group_names {
                app.outbounds_state.expanded.insert(name.clone());
            }
        }
    });
    ui.add_space(4.0);

    let mut switch_action: Option<(String, String)> = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for &idx in &sorted_indices {
            let group = &groups[idx];
            let nodes = match &group.all {
                Some(nodes) if !nodes.is_empty() => nodes,
                _ => continue,
            };

            let is_collapsed = !app.outbounds_state.expanded.contains(&group.name);

            ui.group(|ui| {
                let header = ui.horizontal(|ui| {
                    let arrow = if is_collapsed { "▶" } else { "▼" };
                    ui.label(arrow);
                    ui.strong(&group.name);
                    ui.label(format!("({})", group.group_type));
                    if let Some(current) = &group.now {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(current);
                        });
                    }
                });
                // Make the entire header row clickable (including text)
                let header_rect = header.response.rect;
                let header_id = ui.id().with(&group.name).with("header");
                if ui.interact(header_rect, header_id, egui::Sense::click()).clicked() {
                    if is_collapsed {
                        app.outbounds_state.expanded.insert(group.name.clone());
                    } else {
                        app.outbounds_state.expanded.remove(&group.name);
                    }
                }

                if !is_collapsed {
                    let current = group.now.as_deref().unwrap_or("");
                    ui.horizontal_wrapped(|ui| {
                        for node in nodes {
                            let is_selected = node == current;
                            if ui.selectable_label(is_selected, node).clicked() && !is_selected {
                                switch_action = Some((group.name.clone(), node.clone()));
                            }
                        }
                    });
                }
            });
            ui.add_space(4.0);
        }
    });

    drop(groups);

    if let Some((group_name, node_name)) = switch_action {
        switch_proxy(app, &group_name, &node_name);
    }
}

fn fetch_proxies(app: &mut BoxApp) {
    app.outbounds_state.loading.store(true, Ordering::Relaxed);
    app.outbounds_state.last_error = None;
    app.outbounds_state.last_fetch = Some(Instant::now());
    let groups = app.outbounds_state.groups.clone();
    let loading = app.outbounds_state.loading.clone();
    let base_url = app.clash_api_base.clone();
    let secret = app.clash_api_secret.clone();
    let client = app.http_client.clone();

    app.runtime.spawn(async move {
        let url = format!("{base_url}/proxies");

        let mut req = client.get(&url);
        if !secret.is_empty() {
            req = req.bearer_auth(&secret);
        }
        match req.send().await {
            Ok(resp) => match resp.json::<ProxiesResponse>().await {
                Ok(data) => {
                    let group_list: Vec<ProxyGroup> = data
                        .proxies
                        .into_values()
                        .filter(|p| p.all.is_some())
                        .collect();
                    *groups.lock().unwrap() = group_list;
                }
                Err(e) => {
                    tracing::error!("Failed to parse proxies: {e}");
                }
            },
            Err(e) => {
                tracing::error!("Failed to fetch proxies: {e}");
            }
        }
        loading.store(false, Ordering::Relaxed);
    });
}

fn switch_proxy(app: &mut BoxApp, group: &str, node: &str) {
    let base_url = app.clash_api_base.clone();
    let secret = app.clash_api_secret.clone();
    let group = group.to_string();
    let node = node.to_string();
    let groups = app.outbounds_state.groups.clone();
    let client = app.http_client.clone();

    app.runtime.spawn(async move {
        let url = format!(
            "{}/proxies/{}",
            base_url,
            urlencoding::encode(&group)
        );
        let body = serde_json::json!({ "name": node });

        let mut req = client.put(&url).json(&body);
        if !secret.is_empty() {
            req = req.bearer_auth(&secret);
        }
        match req.send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    let mut g = groups.lock().unwrap();
                    if let Some(grp) = g.iter_mut().find(|g| g.name == group) {
                        grp.now = Some(node);
                    }
                } else {
                    tracing::error!("Failed to switch proxy: HTTP {}", resp.status());
                }
            }
            Err(e) => {
                tracing::error!("Failed to switch proxy: {e}");
            }
        }
    });
}
