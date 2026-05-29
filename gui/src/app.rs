use eframe::egui::{self, Color32, FontId, RichText, ScrollArea, Ui};
use mc_share::{
    host::{HostConfig, run_with_config as host_run},
    join::{JoinConfig, run_with_config as join_run},
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const DEFAULT_COORD: &str = "https://mcs.markund.f5.si";
const DEFAULT_STUN:  &str = "stun.l.google.com:19302";

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Mode { Host, Join }

#[derive(PartialEq, Clone, Copy)]
enum RunState { Idle, Running, Done }

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    rt: Arc<tokio::runtime::Runtime>,

    mode: Mode,
    run_state: RunState,

    // ── inputs ──
    coord_url:   String,
    mc_port:     String,   // host: Minecraft server port; join: local proxy port
    share_input: String,   // join: URL / room code to connect to

    // ── outputs ──
    share_url:    String,   // host: generated share URL
    local_port:   u16,      // join: local proxy port

    // ── log ──
    log: Arc<Mutex<Vec<(LogLevel, String)>>>,
    scroll_to_bottom: bool,

    // ── cancellation ──
    cancel: Option<CancellationToken>,
}

#[derive(Clone, Copy)]
enum LogLevel { Info, Warn, Error }

impl App {
    pub fn new(rt: Arc<tokio::runtime::Runtime>) -> Self {
        Self {
            rt,
            mode: Mode::Host,
            run_state: RunState::Idle,
            coord_url: DEFAULT_COORD.to_string(),
            mc_port: String::new(),
            share_input: String::new(),
            share_url: String::new(),
            local_port: 0,
            log: Arc::new(Mutex::new(Vec::new())),
            scroll_to_bottom: false,
            cancel: None,
        }
    }

    // ── Log helpers ───────────────────────────────────────────────────────────

    fn push_log(log: &Arc<Mutex<Vec<(LogLevel, String)>>>, level: LogLevel, msg: String) {
        if let Ok(mut v) = log.lock() {
            v.push((level, msg));
            if v.len() > 2000 { v.drain(0..200); }
        }
    }

    fn log_info(&self, msg: impl Into<String>) {
        Self::push_log(&self.log, LogLevel::Info, msg.into());
    }

    // ── Start / stop ──────────────────────────────────────────────────────────

    fn start_host(&mut self, ctx: egui::Context) {
        let mc_port: u16 = self.mc_port.trim().parse().unwrap_or(0);
        let coord_url   = self.coord_url.trim().to_string();
        let log         = Arc::clone(&self.log);
        let cancel      = CancellationToken::new();
        self.cancel     = Some(cancel.clone());
        self.share_url  = String::new();
        self.run_state  = RunState::Running;

        // share_url channel
        let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
        let share_url_store = Arc::clone(&self.log); // we'll piggyback via log
        let ctx2  = ctx.clone();
        let log2  = Arc::clone(&log);

        // Capture share URL → store in App via a shared cell
        let url_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let url_cell2 = Arc::clone(&url_cell);

        let rt   = Arc::clone(&self.rt);
        let log3 = Arc::clone(&log);
        let ctx3 = ctx.clone();
        let cancel2 = cancel.clone();
        let url_cell3 = Arc::clone(&url_cell);

        // We store url_cell on self so update() can read it.
        // Hack: encode the URL into the log with a sentinel prefix.
        let url_log = Arc::clone(&log);

        rt.spawn(async move {
            Self::push_log(&log3, LogLevel::Info, "Starting host…".into());
            ctx3.request_repaint();

            let config = HostConfig {
                mc_port,
                coord_url,
                stun_server: DEFAULT_STUN.to_string(),
                on_share_url: Some(Box::new(move |url| {
                    Self::push_log(&url_log, LogLevel::Info,
                        format!("__SHARE_URL__{}", url));
                })),
                cancel: cancel2,
            };

            match host_run(config).await {
                Ok(_)  => Self::push_log(&log3, LogLevel::Info, "Host session ended.".into()),
                Err(e) => Self::push_log(&log3, LogLevel::Error, format!("Error: {}", e)),
            }
            ctx3.request_repaint();
        });

        // tracing → log  (best-effort: capture println output is hard;
        // important messages are already in the log via push_log above)
    }

    fn start_join(&mut self, ctx: egui::Context) {
        let target     = self.share_input.trim().to_string();
        let local_port: u16 = self.mc_port.trim().parse().unwrap_or(25565);
        let coord_url  = self.coord_url.trim().to_string();
        let log        = Arc::clone(&self.log);
        let cancel     = CancellationToken::new();
        self.cancel    = Some(cancel.clone());
        self.run_state = RunState::Running;

        let rt    = Arc::clone(&self.rt);
        let log2  = Arc::clone(&log);
        let ctx2  = ctx.clone();
        let cancel2 = cancel.clone();
        let local_port_shared: Arc<Mutex<u16>> = Arc::new(Mutex::new(0));
        let lps = Arc::clone(&local_port_shared);
        let lps_log = Arc::clone(&log);

        rt.spawn(async move {
            Self::push_log(&log2, LogLevel::Info, "Joining…".into());
            ctx2.request_repaint();

            let config = JoinConfig {
                target,
                local_port,
                coord_url,
                stun_server: DEFAULT_STUN.to_string(),
                on_connected: Some(Box::new(move |port| {
                    Self::push_log(&lps_log, LogLevel::Info,
                        format!("__LOCAL_PORT__{}", port));
                })),
                cancel: cancel2,
            };

            match join_run(config).await {
                Ok(_)  => Self::push_log(&log2, LogLevel::Info, "Disconnected.".into()),
                Err(e) => Self::push_log(&log2, LogLevel::Error, format!("Error: {}", e)),
            }
            ctx2.request_repaint();
        });
    }

    fn stop(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        self.run_state = RunState::Idle;
        self.log_info("Stopped.");
    }

    // ── UI sections ───────────────────────────────────────────────────────────

    fn show_host_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.add_space(4.0);
        egui::Grid::new("host_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Coordination Server:");
                ui.add_enabled(
                    self.run_state == RunState::Idle,
                    egui::TextEdit::singleline(&mut self.coord_url).desired_width(360.0),
                );
                ui.end_row();

                ui.label("Minecraft Port:");
                ui.add_enabled(
                    self.run_state == RunState::Idle,
                    egui::TextEdit::singleline(&mut self.mc_port)
                        .desired_width(80.0)
                        .hint_text("auto"),
                );
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            match self.run_state {
                RunState::Idle => {
                    if ui.button(RichText::new("  Share World  ").size(16.0)).clicked() {
                        self.start_host(ctx.clone());
                    }
                }
                RunState::Running | RunState::Done => {
                    if ui.button("  Stop  ").clicked() { self.stop(); }
                }
            }
        });

        // Extract share URL from sentinel log entries
        if self.share_url.is_empty() {
            if let Ok(mut v) = self.log.lock() {
                for (_, msg) in v.iter_mut() {
                    if let Some(url) = msg.strip_prefix("__SHARE_URL__") {
                        self.share_url = url.to_string();
                        *msg = format!("Share URL ready: {}", self.share_url);
                        break;
                    }
                }
            }
        }

        if !self.share_url.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("Share this link:").strong());
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.share_url.clone())
                        .desired_width(380.0)
                        .interactive(false),
                );
                if ui.button("📋 Copy").clicked() {
                    ui.output_mut(|o| o.copied_text = self.share_url.clone());
                }
            });
        }
    }

    fn show_join_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.add_space(4.0);
        egui::Grid::new("join_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Coordination Server:");
                ui.add_enabled(
                    self.run_state == RunState::Idle,
                    egui::TextEdit::singleline(&mut self.coord_url).desired_width(360.0),
                );
                ui.end_row();

                ui.label("Share URL / Code:");
                ui.add_enabled(
                    self.run_state == RunState::Idle,
                    egui::TextEdit::singleline(&mut self.share_input)
                        .desired_width(360.0)
                        .hint_text("https://mcs.markund.f5.si/xxxxxx"),
                );
                ui.end_row();

                ui.label("Local Port:");
                ui.add_enabled(
                    self.run_state == RunState::Idle,
                    egui::TextEdit::singleline(&mut self.mc_port)
                        .desired_width(80.0)
                        .hint_text("25565"),
                );
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            match self.run_state {
                RunState::Idle => {
                    let can_join = !self.share_input.trim().is_empty();
                    if ui.add_enabled(can_join, egui::Button::new(
                        RichText::new("  Join World  ").size(16.0),
                    )).clicked() {
                        self.start_join(ctx.clone());
                    }
                    if ui.button("📋 Paste").clicked() {
                        self.share_input = ui.input(|i| i.raw.clone())
                            .events
                            .iter()
                            .find_map(|e| {
                                if let egui::Event::Paste(s) = e { Some(s.clone()) } else { None }
                            })
                            .unwrap_or_default();
                        // Fallback: read clipboard via output
                        if self.share_input.is_empty() {
                            ui.output_mut(|o| o.copied_text = String::new()); // trigger clipboard access
                        }
                    }
                }
                RunState::Running | RunState::Done => {
                    if ui.button("  Disconnect  ").clicked() { self.stop(); }
                }
            }
        });

        // Show local port once connected
        if self.run_state == RunState::Running && self.local_port == 0 {
            if let Ok(mut v) = self.log.lock() {
                for (_, msg) in v.iter_mut() {
                    if let Some(port_str) = msg.strip_prefix("__LOCAL_PORT__") {
                        if let Ok(p) = port_str.parse::<u16>() {
                            self.local_port = p;
                            *msg = format!("Proxy listening on 127.0.0.1:{}", p);
                        }
                        break;
                    }
                }
            }
        }

        if self.local_port != 0 && self.run_state == RunState::Running {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("Connected!").color(Color32::GREEN).strong());
            ui.label("Open Minecraft → Multiplayer — the world appears automatically.");
            if self.local_port != 25565 {
                ui.label(format!("Or connect directly to:  127.0.0.1:{}", self.local_port));
            }
        }
    }

    fn show_log(&mut self, ui: &mut Ui) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Log").strong());
            if ui.small_button("Clear").clicked() {
                if let Ok(mut v) = self.log.lock() { v.clear(); }
            }
        });

        let log_snapshot: Vec<(LogLevel, String)> = self
            .log
            .lock()
            .map(|v| v.iter()
                .filter(|(_, m)| !m.starts_with("__"))
                .cloned()
                .collect())
            .unwrap_or_default();

        let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        let visible = (ui.available_height() / row_h).floor() as usize;

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, row_h, log_snapshot.len(), |ui, range| {
                for (level, msg) in &log_snapshot[range] {
                    let color = match level {
                        LogLevel::Info  => Color32::from_gray(220),
                        LogLevel::Warn  => Color32::from_rgb(255, 200, 80),
                        LogLevel::Error => Color32::from_rgb(255, 100, 100),
                    };
                    ui.label(RichText::new(msg).monospace().color(color).size(12.0));
                }
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for repaint so log updates feel live
        ctx.request_repaint_after(std::time::Duration::from_millis(200));

        egui::CentralPanel::default().show(ctx, |ui| {
            // ── Title + mode tabs ─────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.heading("MineScale-Java");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let idle = self.run_state == RunState::Idle;
                    ui.add_enabled_ui(idle, |ui| {
                        ui.selectable_value(&mut self.mode, Mode::Join, "Join");
                        ui.selectable_value(&mut self.mode, Mode::Host, "Host");
                    });
                });
            });
            ui.separator();

            // ── Mode panel ────────────────────────────────────────────────────
            match self.mode {
                Mode::Host => self.show_host_panel(ui, ctx),
                Mode::Join => self.show_join_panel(ui, ctx),
            }

            // ── Log ───────────────────────────────────────────────────────────
            ui.add_space(8.0);
            self.show_log(ui);
        });
    }
}
