use eframe::egui;
use egui_extras::{Column, TableBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::app::BoxApp;

#[derive(Debug, Clone, Deserialize)]
struct ConnectionsResponse {
    connections: Option<Vec<Connection>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Connection {
    pub id: String,
    pub metadata: ConnectionMetadata,
    pub upload: u64,
    pub download: u64,
    pub chains: Vec<String>,
    pub rule: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionMetadata {
    pub process: Option<String>,
    #[serde(rename = "processPath")]
    pub process_path: Option<String>,
    pub host: Option<String>,
    #[serde(rename = "destinationIP")]
    pub destination_ip: Option<String>,
}

impl ConnectionMetadata {
    pub fn display_process(&self) -> &str {
        if let Some(ref p) = self.process {
            return p;
        }
        if let Some(ref path) = self.process_path
            && let Some(name) = std::path::Path::new(path).file_name().and_then(|n| n.to_str())
        {
            return name;
        }
        "unknown"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortColumn {
    Process,
    Host,
    Chain,
    Rule,
    Upload,
    Download,
    UploadSpeed,
    DownloadSpeed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

/// Row data with precomputed speeds for display and sorting.
struct ConnRow {
    process: String,
    host: String,
    chain: String,
    rule: String,
    upload: u64,
    download: u64,
    upload_speed: f64,
    download_speed: f64,
}

pub struct ConnectionsState {
    pub connections: Arc<Mutex<Vec<Connection>>>,
    pub streaming: bool,
    pub streaming_flag: Arc<AtomicBool>,
    pub sort_column: Option<SortColumn>,
    pub sort_order: SortOrder,
    /// Previous snapshot for speed calculation: id -> (upload, download, timestamp).
    prev_bytes: HashMap<String, (u64, u64, Instant)>,
    /// Computed speeds: id -> (upload_speed, download_speed) in bytes/sec.
    speeds: HashMap<String, (f64, f64)>,
    /// Snapshot of the last connections data used for speed calculation,
    /// so we only recalculate when the WebSocket delivers new data.
    last_snapshot_len: usize,
    last_snapshot_bytes: u64,
}

impl Default for ConnectionsState {
    fn default() -> Self {
        Self {
            connections: Arc::new(Mutex::new(Vec::new())),
            streaming: false,
            streaming_flag: Arc::new(AtomicBool::new(false)),
            sort_column: None,
            sort_order: SortOrder::Ascending,
            prev_bytes: HashMap::new(),
            speeds: HashMap::new(),
            last_snapshot_len: 0,
            last_snapshot_bytes: 0,
        }
    }
}

impl ConnectionsState {
    /// Update speed calculations from current connections snapshot.
    /// Only recalculates when the underlying data has actually changed
    /// (new WebSocket message), not on every UI repaint.
    fn update_speeds(&mut self, connections: &[Connection]) {
        // Quick fingerprint: number of connections + total bytes
        let total_bytes: u64 = connections
            .iter()
            .map(|c| c.upload + c.download)
            .sum();
        if connections.len() == self.last_snapshot_len && total_bytes == self.last_snapshot_bytes {
            return; // Data unchanged since last update, keep existing speeds
        }
        self.last_snapshot_len = connections.len();
        self.last_snapshot_bytes = total_bytes;

        let now = Instant::now();
        let mut new_prev = HashMap::with_capacity(connections.len());

        for conn in connections {
            if let Some(&(prev_up, prev_down, prev_time)) = self.prev_bytes.get(&conn.id) {
                let dt = now.duration_since(prev_time).as_secs_f64();
                if dt > 0.0 {
                    let up_speed = (conn.upload.saturating_sub(prev_up)) as f64 / dt;
                    let down_speed = (conn.download.saturating_sub(prev_down)) as f64 / dt;
                    self.speeds.insert(conn.id.clone(), (up_speed, down_speed));
                }
            } else {
                self.speeds.insert(conn.id.clone(), (0.0, 0.0));
            }
            new_prev.insert(conn.id.clone(), (conn.upload, conn.download, now));
        }

        // Remove stale entries
        self.speeds.retain(|id, _| new_prev.contains_key(id));
        self.prev_bytes = new_prev;
    }

    fn toggle_sort(&mut self, column: SortColumn) {
        if self.sort_column == Some(column) {
            match self.sort_order {
                SortOrder::Ascending => self.sort_order = SortOrder::Descending,
                SortOrder::Descending => {
                    self.sort_column = None;
                    self.sort_order = SortOrder::Ascending;
                }
            }
        } else {
            self.sort_column = Some(column);
            self.sort_order = SortOrder::Ascending;
        }
    }

}

pub fn show(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.heading("Connections");
    ui.add_space(8.0);

    // Sync streaming state from async task
    app.connections_state.streaming =
        app.connections_state.streaming_flag.load(Ordering::Relaxed);

    // Start WebSocket streaming if core is running and not already streaming
    if app.cached_is_running && !app.connections_state.streaming {
        start_connections_streaming(app);
    }

    // Build row data under lock, then drop lock before UI rendering
    let rows: Vec<ConnRow> = {
        let connections = app.connections_state.connections.lock().unwrap().clone();
        app.connections_state.update_speeds(&connections);

        connections
            .iter()
            .map(|conn| {
                let (up_speed, down_speed) = app
                    .connections_state
                    .speeds
                    .get(&conn.id)
                    .copied()
                    .unwrap_or((0.0, 0.0));
                ConnRow {
                    process: conn.metadata.display_process().to_owned(),
                    host: conn
                        .metadata
                        .host
                        .as_deref()
                        .or(conn.metadata.destination_ip.as_deref())
                        .unwrap_or("-")
                        .to_owned(),
                    chain: conn.chains.join(" → "),
                    rule: conn.rule.clone(),
                    upload: conn.upload,
                    download: conn.download,
                    upload_speed: up_speed,
                    download_speed: down_speed,
                }
            })
            .collect()
    };

    if rows.is_empty() {
        ui.label("No active connections");
        return;
    }

    // Sort rows
    let mut rows = rows;
    if let Some(col) = app.connections_state.sort_column {
        let order = app.connections_state.sort_order;
        rows.sort_by(|a, b| {
            let cmp = match col {
                SortColumn::Process => a.process.to_lowercase().cmp(&b.process.to_lowercase()),
                SortColumn::Host => a.host.to_lowercase().cmp(&b.host.to_lowercase()),
                SortColumn::Chain => a.chain.cmp(&b.chain),
                SortColumn::Rule => a.rule.cmp(&b.rule),
                SortColumn::Upload => a.upload.cmp(&b.upload),
                SortColumn::Download => a.download.cmp(&b.download),
                SortColumn::UploadSpeed => a
                    .upload_speed
                    .partial_cmp(&b.upload_speed)
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::DownloadSpeed => a
                    .download_speed
                    .partial_cmp(&b.download_speed)
                    .unwrap_or(std::cmp::Ordering::Equal),
            };
            match order {
                SortOrder::Ascending => cmp,
                SortOrder::Descending => cmp.reverse(),
            }
        });
    }

    // Column definitions: (label, SortColumn variant)
    let columns: &[(&str, SortColumn)] = &[
        ("Process", SortColumn::Process),
        ("Host", SortColumn::Host),
        ("Chain", SortColumn::Chain),
        ("Rule", SortColumn::Rule),
        ("Upload", SortColumn::Upload),
        ("Download", SortColumn::Download),
        ("UL Speed", SortColumn::UploadSpeed),
        ("DL Speed", SortColumn::DownloadSpeed),
    ];

    let text_height = ui.text_style_height(&egui::TextStyle::Body);

    let available_width = ui.available_width();
    let num_cols = columns.len() as f32;
    let initial_col_width = (available_width / num_cols).max(80.0);

    let mut clicked_column: Option<SortColumn> = None;

    egui::ScrollArea::horizontal().show(ui, |ui| {
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(
                egui::Layout::left_to_right(egui::Align::Center)
                    .with_main_wrap(false),
            );

        for _ in columns {
            table = table.column(Column::initial(initial_col_width).clip(true));
        }

        table
            .header(text_height + 8.0, |mut header| {
                for &(label, col) in columns {
                    header.col(|ui| {
                        let indicator = match app.connections_state.sort_column {
                            Some(c) if c == col => match app.connections_state.sort_order {
                                SortOrder::Ascending => " ▲",
                                SortOrder::Descending => " ▼",
                            },
                            _ => "",
                        };
                        let text = format!("{label}{indicator}");
                        // Make the entire cell clickable
                        let response = ui.interact(
                            ui.max_rect(),
                            ui.id().with(col),
                            egui::Sense::click(),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).strong())
                                .selectable(false),
                        );
                        if response.clicked() {
                            clicked_column = Some(col);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(text_height + 4.0, rows.len(), |mut row| {
                    let r = &rows[row.index()];
                    row.col(|ui| { ui.label(&r.process); });
                    row.col(|ui| { ui.label(&r.host); });
                    row.col(|ui| { ui.label(&r.chain); });
                    row.col(|ui| { ui.label(&r.rule); });
                    row.col(|ui| { ui.label(format_bytes(r.upload)); });
                    row.col(|ui| { ui.label(format_bytes(r.download)); });
                    row.col(|ui| { ui.label(format_speed(r.upload_speed)); });
                    row.col(|ui| { ui.label(format_speed(r.download_speed)); });
                });
            });
    });

    if let Some(col) = clicked_column {
        app.connections_state.toggle_sort(col);
    }
}

fn start_connections_streaming(app: &mut BoxApp) {
    app.connections_state.streaming = true;
    let connections = app.connections_state.connections.clone();
    let streaming_flag = app.connections_state.streaming_flag.clone();
    let base_url = app.clash_api_base.clone();
    let secret = app.clash_api_secret.clone();

    streaming_flag.store(true, Ordering::Relaxed);

    app.runtime.spawn(async move {
        let mut ws_url = format!(
            "{}/connections",
            base_url.replacen("http", "ws", 1)
        );
        if !secret.is_empty() {
            ws_url.push_str(&format!("?token={secret}"));
        }

        let (ws_stream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to connect connections WebSocket: {e}");
                streaming_flag.store(false, Ordering::Relaxed);
                return;
            }
        };

        use futures_util::StreamExt;
        let (_, mut read) = ws_stream.split();

        while streaming_flag.load(Ordering::Relaxed) {
            match read.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    if let Ok(data) = serde_json::from_str::<ConnectionsResponse>(&text) {
                        *connections.lock().unwrap() = data.connections.unwrap_or_default();
                    }
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                Some(Err(e)) => {
                    tracing::error!("Connections WebSocket error: {e}");
                    break;
                }
                _ => {}
            }
        }

        // Clear connections when stream ends
        connections.lock().unwrap().clear();
        streaming_flag.store(false, Ordering::Relaxed);
    });
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_speed(bytes_per_sec: f64) -> String {
    crate::core::format_speed(bytes_per_sec)
}
