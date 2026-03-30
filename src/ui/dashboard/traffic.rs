use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::BoxApp;

const MAX_TRAFFIC_POINTS: usize = 180;

#[derive(Clone)]
pub struct TrafficPoint {
    pub upload: f64,
    pub download: f64,
}

pub struct TrafficState {
    pub traffic_history: Arc<Mutex<VecDeque<TrafficPoint>>>,
    pub traffic_polling: bool,
    /// Shared with async task so it can signal when the stream ends
    pub polling_flag: Arc<AtomicBool>,
    /// Handle to the traffic polling task; aborted before starting a new one
    /// to prevent duplicate streams after window hide/show cycles.
    pub polling_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Default for TrafficState {
    fn default() -> Self {
        Self {
            traffic_history: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_TRAFFIC_POINTS))),
            traffic_polling: false,
            polling_flag: Arc::new(AtomicBool::new(false)),
            polling_handle: None,
        }
    }
}

impl TrafficState {
    pub fn current_speed(&self) -> (f64, f64) {
        let history = self.traffic_history.lock().unwrap();
        history
            .back()
            .map(|p| (p.upload, p.download))
            .unwrap_or((0.0, 0.0))
    }
}

pub fn show_traffic_chart(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.group(|ui| {
        ui.label("Traffic Speed");
        let history = app.dashboard_state.traffic.traffic_history.lock().unwrap();
        let upload_points: PlotPoints = history
            .iter()
            .enumerate()
            .map(|(i, p)| [i as f64, p.upload])
            .collect();
        let download_points: PlotPoints = history
            .iter()
            .enumerate()
            .map(|(i, p)| [i as f64, p.download])
            .collect();
        // Cheap Arc clone for the label_formatter closure instead of copying all points
        let history_ref = app.dashboard_state.traffic.traffic_history.clone();
        drop(history);

        let upload_line =
            Line::new("Upload", upload_points).color(egui::Color32::from_rgb(100, 180, 255));
        let download_line =
            Line::new("Download", download_points).color(egui::Color32::from_rgb(100, 255, 150));

        Plot::new("traffic_plot")
            .height(200.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .show_axes([false, true])
            .include_x(0.0)
            .include_x(MAX_TRAFFIC_POINTS as f64)
            .y_axis_formatter(|mark, _range| format_speed_axis(mark.value))
            .label_formatter(move |_name, value| {
                let idx = value.x.round() as usize;
                let h = history_ref.lock().unwrap();
                if let Some(p) = h.get(idx) {
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
}

pub fn start_traffic_polling(app: &mut BoxApp) {
    // Abort any previous task to prevent duplicate streams
    if let Some(h) = app.dashboard_state.traffic.polling_handle.take() {
        h.abort();
    }

    app.dashboard_state.traffic.traffic_polling = true;
    let history = app.dashboard_state.traffic.traffic_history.clone();
    let polling_flag = app.dashboard_state.traffic.polling_flag.clone();
    let base_url = app.clash_api_base.clone();
    let secret = app.clash_api_secret.clone();

    polling_flag.store(true, Ordering::Relaxed);
    history.lock().unwrap().clear();

    let handle = app.runtime.spawn(async move {
        let mut ws_url = format!("{}/traffic", base_url.replacen("http", "ws", 1));
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
                        h.push_back(TrafficPoint {
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
    app.dashboard_state.traffic.polling_handle = Some(handle);
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
