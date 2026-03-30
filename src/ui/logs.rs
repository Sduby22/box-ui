use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::app::BoxApp;

pub const DEFAULT_MAX_LOG_LINES: usize = 10_000;

pub struct LogEntry {
    pub level: LogLevel,
    pub payload: String,
    /// Pre-formatted display text: "[LEVEL] payload" — built once on creation to avoid
    /// per-frame `format!()` allocations in the UI loop.
    pub formatted: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn from_str(s: &str) -> Self {
        match s {
            "debug" | "DEBUG" => Self::Debug,
            "warn" | "WARN" | "warning" | "WARNING" => Self::Warn,
            "error" | "ERROR" => Self::Error,
            _ => Self::Info,
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Self::Debug => egui::Color32::from_rgb(140, 140, 140),
            Self::Info => egui::Color32::from_rgb(80, 180, 220),
            Self::Warn => egui::Color32::from_rgb(220, 180, 50),
            Self::Error => egui::Color32::from_rgb(220, 50, 47),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Fixed-capacity ring buffer that overwrites the oldest entries when full.
/// The internal buffer is lazily allocated on first push to avoid upfront memory cost.
pub struct LogRingBuffer {
    buf: Vec<Option<LogEntry>>,
    capacity: usize,
    /// Next write position.
    head: usize,
    /// Number of entries currently stored.
    len: usize,
}

impl LogRingBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // Defer allocation until first push
        Self {
            buf: Vec::new(),
            capacity,
            head: 0,
            len: 0,
        }
    }

    /// Ensure internal buffer is allocated to full capacity.
    fn ensure_allocated(&mut self) {
        if self.buf.len() < self.capacity {
            self.buf.clear();
            self.buf.reserve_exact(self.capacity);
            self.buf.resize_with(self.capacity, || None);
        }
    }

    pub fn push(&mut self, entry: LogEntry) {
        self.ensure_allocated();
        self.buf[self.head] = Some(entry);
        self.head = (self.head + 1) % self.capacity;
        if self.len < self.capacity {
            self.len += 1;
        }
    }

    pub fn clear(&mut self) {
        for slot in &mut self.buf {
            *slot = None;
        }
        self.head = 0;
        self.len = 0;
    }

    /// Iterate entries from oldest to newest.
    pub fn iter(&self) -> RingIter<'_> {
        if self.buf.is_empty() || self.len == 0 {
            return RingIter {
                buf: &self.buf,
                pos: 0,
                remaining: 0,
            };
        }
        let start = if self.len < self.capacity {
            0
        } else {
            self.head
        };
        RingIter {
            buf: &self.buf,
            pos: start,
            remaining: self.len,
        }
    }

    /// Resize the buffer. If shrinking, keeps the newest entries.
    pub fn resize(&mut self, new_capacity: usize) {
        let new_capacity = new_capacity.max(1);
        if new_capacity == self.capacity {
            return;
        }

        if self.buf.is_empty() {
            // Not yet allocated, just update capacity
            self.capacity = new_capacity;
            return;
        }

        let mut new_buf = Vec::with_capacity(new_capacity);
        new_buf.resize_with(new_capacity, || None);

        // Copy entries: if shrinking, skip oldest; if growing, copy all
        let keep = self.len.min(new_capacity);
        let skip = self.len - keep;
        for (i, entry) in self.iter().skip(skip).enumerate() {
            new_buf[i] = Some(LogEntry {
                level: entry.level,
                payload: entry.payload.clone(),
                formatted: entry.formatted.clone(),
            });
        }

        self.buf = new_buf;
        self.capacity = new_capacity;
        self.head = keep % new_capacity;
        self.len = keep;
    }
}

pub struct RingIter<'a> {
    buf: &'a [Option<LogEntry>],
    pos: usize,
    remaining: usize,
}

impl<'a> Iterator for RingIter<'a> {
    type Item = &'a LogEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let entry = self.buf[self.pos].as_ref().expect("ring slot must be Some");
        self.pos = (self.pos + 1) % self.buf.len();
        self.remaining -= 1;
        Some(entry)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<'a> ExactSizeIterator for RingIter<'a> {}

pub struct LogsState {
    pub entries: Arc<Mutex<LogRingBuffer>>,
    pub auto_scroll: bool,
    pub max_lines: usize,
    pub search_query: String,
    pub streaming: bool,
    pub streaming_flag: Arc<AtomicBool>,
}

impl LogsState {
    pub fn new(max_lines: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(LogRingBuffer::new(max_lines))),
            auto_scroll: true,
            max_lines,
            search_query: String::new(),
            streaming: false,
            streaming_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Default for LogsState {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LOG_LINES)
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut BoxApp) {
    ui.heading("Logs");
    ui.add_space(8.0);

    // Sync streaming state from async task
    app.logs_state.streaming = app.logs_state.streaming_flag.load(Ordering::Relaxed);

    // Start streaming if core is running, window visible, and not already streaming
    if app.cached_is_running
        && app.window_visible.load(Ordering::Relaxed)
        && !app.logs_state.streaming
    {
        start_log_streaming(app);
    }

    ui.horizontal(|ui| {
        ui.label("Search:");
        ui.add(
            egui::TextEdit::singleline(&mut app.logs_state.search_query)
                .desired_width(200.0)
                .hint_text("Filter logs..."),
        );

        ui.separator();

        ui.label("Max lines:");
        let mut max_str = app.logs_state.max_lines.to_string();
        let response = ui.add(egui::TextEdit::singleline(&mut max_str).desired_width(60.0));
        if response.changed()
            && let Ok(val) = max_str.parse::<usize>()
            && val > 0
        {
            app.logs_state.max_lines = val;
            app.logs_state.entries.lock().unwrap().resize(val);
        }

        ui.separator();

        if ui.button("Clear").clicked() {
            app.logs_state.entries.lock().unwrap().clear();
        }

        ui.checkbox(&mut app.logs_state.auto_scroll, "Auto-scroll");
    });

    ui.add_space(8.0);

    let scroll = egui::ScrollArea::vertical().auto_shrink(false);
    let scroll = if app.logs_state.auto_scroll {
        scroll.stick_to_bottom(true)
    } else {
        scroll
    };

    let entries = app.logs_state.entries.lock().unwrap();
    let query = app.logs_state.search_query.to_lowercase();
    let has_filter = !query.is_empty();

    scroll.show(ui, |ui| {
        for entry in entries.iter() {
            if has_filter && !entry.payload.to_lowercase().contains(&query) {
                continue;
            }
            let text = egui::RichText::new(&entry.formatted)
                .monospace()
                .color(entry.level.color());
            ui.label(text);
        }
    });
}

fn start_log_streaming(app: &mut BoxApp) {
    if app.clash_api_base.is_empty() {
        return;
    }

    app.logs_state.streaming = true;
    let entries = app.logs_state.entries.clone();
    let streaming_flag = app.logs_state.streaming_flag.clone();
    let base_url = app.clash_api_base.clone();
    let secret = app.clash_api_secret.clone();

    streaming_flag.store(true, Ordering::Relaxed);

    app.runtime.spawn(async move {
        // Subscribe to all levels (debug captures everything)
        let mut ws_url = format!("{}/logs", base_url.replacen("http", "ws", 1));
        let mut params = vec!["level=debug".to_string()];
        if !secret.is_empty() {
            params.push(format!("token={secret}"));
        }
        ws_url.push('?');
        ws_url.push_str(&params.join("&"));

        let (ws_stream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to connect log WebSocket: {e}");
                streaming_flag.store(false, Ordering::Relaxed);
                return;
            }
        };

        use futures_util::StreamExt;
        let (_, mut read) = ws_stream.split();

        while streaming_flag.load(Ordering::Relaxed) {
            match read.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    // Clash API log format: {"type":"info","payload":"message"}
                    if let Ok(log) = serde_json::from_str::<serde_json::Value>(&text) {
                        let level_str = log["type"].as_str().unwrap_or("info");
                        let payload = log["payload"].as_str().unwrap_or("").to_string();
                        let level = LogLevel::from_str(level_str);
                        let formatted = format!("[{}] {}", level.label(), payload);
                        let entry = LogEntry {
                            level,
                            payload,
                            formatted,
                        };

                        entries.lock().unwrap().push(entry);
                    }
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                Some(Err(e)) => {
                    tracing::error!("Log WebSocket error: {e}");
                    break;
                }
                _ => {}
            }
        }

        streaming_flag.store(false, Ordering::Relaxed);
    });
}
