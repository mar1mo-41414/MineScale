use eframe::egui::{self, Color32, RichText, ScrollArea, Ui};
use mc_share::{
    host::{run_with_config as host_run, HostConfig},
    join::{run_with_config as join_run, JoinConfig},
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::Layer;

const DEFAULT_COORD: &str = "https://mcs.markund.f5.si";
const DEFAULT_STUN:  &str = "stun.l.google.com:19302";

// ── Tracing → GUI log layer ───────────────────────────────────────────────────

#[derive(Clone)]
pub enum LogLevel { Info, Warn, Error }
pub type LogEntry = (LogLevel, String);

pub struct GuiLayer(pub Arc<Mutex<Vec<LogEntry>>>);

impl<S: tracing::Subscriber> Layer<S> for GuiLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let meta = event.metadata();

        // Only capture events from our own crates at INFO level or above.
        // This silences internal eframe / winit / egui / quinn debug events.
        if !meta.target().starts_with("mc_share") {
            return;
        }
        if !matches!(*meta.level(),
            tracing::Level::ERROR | tracing::Level::WARN | tracing::Level::INFO)
        {
            return;
        }

        struct V(String);
        impl tracing::field::Visit for V {
            fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                if f.name() == "message" { self.0 = v.to_string(); }
            }
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{:?}", v).trim_matches('"').to_string();
                }
            }
        }
        let mut v = V(String::new());
        event.record(&mut v);
        if v.0.is_empty() { return; }

        let level = match *meta.level() {
            tracing::Level::ERROR => LogLevel::Error,
            tracing::Level::WARN  => LogLevel::Warn,
            _                     => LogLevel::Info,
        };
        if let Ok(mut log) = self.0.lock() {
            if log.len() >= 2000 { log.drain(0..400); }
            log.push((level, v.0));
        }
    }
}

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Mode { Host, Join }

#[derive(PartialEq, Clone, Copy)]
enum RunState { Idle, Running }

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    rt:    Arc<tokio::runtime::Runtime>,
    mode:  Mode,
    state: RunState,

    // ── inputs ──────────────────────────────────────────────────────────────
    coord_url:   String,
    mc_port:     String,
    share_input: String,

    // ── outputs (set by background callbacks) ────────────────────────────────
    share_url_cell:  Arc<Mutex<Option<String>>>,
    local_port_cell: Arc<Mutex<Option<u16>>>,

    // ── derived: updated in update() once cells are populated ────────────────
    share_url:  String,
    local_port: u16,

    // ── log ──────────────────────────────────────────────────────────────────
    log: Arc<Mutex<Vec<LogEntry>>>,

    // ── cancellation ─────────────────────────────────────────────────────────
    cancel: Option<CancellationToken>,
}

impl App {
    pub fn new(rt: Arc<tokio::runtime::Runtime>, log: Arc<Mutex<Vec<LogEntry>>>) -> Self {
        Self {
            rt,
            mode: Mode::Host,
            state: RunState::Idle,
            coord_url:   DEFAULT_COORD.into(),
            mc_port:     String::new(),
            share_input: String::new(),
            share_url_cell:  Arc::new(Mutex::new(None)),
            local_port_cell: Arc::new(Mutex::new(None)),
            share_url:  String::new(),
            local_port: 0,
            log,
            cancel: None,
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn push_log(&self, level: LogLevel, msg: impl Into<String>) {
        if let Ok(mut v) = self.log.lock() {
            v.push((level, msg.into()));
        }
    }

    fn reset_outputs(&mut self) {
        self.share_url  = String::new();
        self.local_port = 0;
        if let Ok(mut c) = self.share_url_cell.lock()  { *c = None; }
        if let Ok(mut c) = self.local_port_cell.lock() { *c = None; }
    }

    // ── Start / stop ──────────────────────────────────────────────────────────

    fn start_host(&mut self, ctx: egui::Context) {
        self.reset_outputs();
        let mc_port: u16 = self.mc_port.trim().parse().unwrap_or(0);
        let coord_url    = self.coord_url.trim().to_string();
        let cancel       = CancellationToken::new();
        self.cancel      = Some(cancel.clone());
        self.state       = RunState::Running;

        let share_cell = Arc::clone(&self.share_url_cell);
        let rt  = Arc::clone(&self.rt);
        let ctx2 = ctx.clone();

        rt.spawn(async move {
            tracing::info!("Starting host…");
            ctx2.request_repaint();

            let config = HostConfig {
                mc_port,
                coord_url,
                stun_server: DEFAULT_STUN.into(),
                on_share_url: Some(Box::new(move |url| {
                    if let Ok(mut c) = share_cell.lock() { *c = Some(url); }
                })),
                cancel,
            };

            match host_run(config).await {
                Ok(_)  => tracing::info!("Host session ended."),
                Err(e) => tracing::error!("{}", e),
            }
            ctx2.request_repaint();
        });
    }

    fn start_join(&mut self, ctx: egui::Context) {
        self.reset_outputs();
        let target     = self.share_input.trim().to_string();
        let local_port: u16 = self.mc_port.trim().parse().unwrap_or(25565);
        let coord_url  = self.coord_url.trim().to_string();
        let cancel     = CancellationToken::new();
        self.cancel    = Some(cancel.clone());
        self.state     = RunState::Running;

        let port_cell = Arc::clone(&self.local_port_cell);
        let rt   = Arc::clone(&self.rt);
        let ctx2 = ctx.clone();

        rt.spawn(async move {
            tracing::info!("Joining room…");
            ctx2.request_repaint();

            let config = JoinConfig {
                target,
                local_port,
                coord_url,
                stun_server: DEFAULT_STUN.into(),
                on_connected: Some(Box::new(move |port| {
                    if let Ok(mut c) = port_cell.lock() { *c = Some(port); }
                })),
                cancel,
            };

            match join_run(config).await {
                Ok(_)  => tracing::info!("Disconnected."),
                Err(e) => tracing::error!("{}", e),
            }
            ctx2.request_repaint();
        });
    }

    fn stop(&mut self) {
        if let Some(c) = self.cancel.take() { c.cancel(); }
        self.state = RunState::Idle;
        self.push_log(LogLevel::Info, "Stopped.");
    }

    // ── Poll callback cells ───────────────────────────────────────────────────

    fn poll_cells(&mut self) {
        if self.share_url.is_empty() {
            if let Ok(c) = self.share_url_cell.lock() {
                if let Some(u) = c.as_ref() { self.share_url = u.clone(); }
            }
        }
        if self.local_port == 0 {
            if let Ok(c) = self.local_port_cell.lock() {
                if let Some(p) = *c { self.local_port = p; }
            }
        }
    }

    // ── UI panels ─────────────────────────────────────────────────────────────

    fn show_host_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let idle = self.state == RunState::Idle;

        egui::Grid::new("host_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Coordination Server:");
                ui.add_enabled(idle,
                    egui::TextEdit::singleline(&mut self.coord_url).desired_width(380.0));
                ui.end_row();

                ui.label("Minecraft Port:");
                ui.add_enabled(idle,
                    egui::TextEdit::singleline(&mut self.mc_port)
                        .desired_width(90.0)
                        .hint_text("auto-detect"));
                ui.end_row();
            });

        ui.add_space(8.0);
        match self.state {
            RunState::Idle => {
                if ui.button(RichText::new("  Share World  ").size(15.0)).clicked() {
                    self.start_host(ctx.clone());
                }
            }
            RunState::Running => {
                if ui.button("  Stop  ").clicked() { self.stop(); }
            }
        }

        if !self.share_url.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("Share this link with your friends:").strong());
            ui.horizontal(|ui| {
                let url = self.share_url.clone();
                ui.add(egui::TextEdit::singleline(&mut self.share_url.clone())
                    .desired_width(400.0)
                    .interactive(false));
                if ui.button("📋 Copy").clicked() {
                    ui.output_mut(|o| o.copied_text = url);
                }
            });
            ui.small("Multiple friends can join with the same link.");
        }
    }

    fn show_join_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let idle = self.state == RunState::Idle;

        egui::Grid::new("join_grid")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Coordination Server:");
                ui.add_enabled(idle,
                    egui::TextEdit::singleline(&mut self.coord_url).desired_width(380.0));
                ui.end_row();

                ui.label("Share URL / Code:");
                ui.horizontal(|ui| {
                    ui.add_enabled(idle,
                        egui::TextEdit::singleline(&mut self.share_input)
                            .desired_width(340.0)
                            .hint_text("https://mcs.markund.f5.si/xxxxxx  or  xxxxxx"));
                    if idle && ui.button("📋 Paste").clicked() {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            if let Ok(text) = cb.get_text() {
                                self.share_input = text.trim().to_string();
                            }
                        }
                    }
                });
                ui.end_row();

                ui.label("Local Port:");
                ui.add_enabled(idle,
                    egui::TextEdit::singleline(&mut self.mc_port)
                        .desired_width(90.0)
                        .hint_text("25565"));
                ui.end_row();
            });

        ui.add_space(8.0);
        match self.state {
            RunState::Idle => {
                let can = !self.share_input.trim().is_empty();
                if ui.add_enabled(can,
                    egui::Button::new(RichText::new("  Join World  ").size(15.0))
                ).clicked() {
                    self.start_join(ctx.clone());
                }
            }
            RunState::Running => {
                if self.local_port != 0 {
                    ui.add_space(6.0);
                    ui.colored_label(Color32::from_rgb(100, 220, 100),
                        "✓  Connected! Open Minecraft → Multiplayer.");
                    if self.local_port != 25565 {
                        ui.label(format!("  Direct address:  127.0.0.1:{}", self.local_port));
                    }
                    ui.add_space(4.0);
                } else {
                    ui.label(RichText::new("  Connecting…").italics().color(Color32::GRAY));
                }
                if ui.button("  Disconnect  ").clicked() { self.stop(); }
            }
        }
    }

    fn show_log_panel(&mut self, ui: &mut Ui) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Log").strong());
            if ui.small_button("Clear").clicked() {
                if let Ok(mut v) = self.log.lock() { v.clear(); }
            }
        });

        let entries: Vec<LogEntry> = self.log.lock()
            .map(|v| v.clone())
            .unwrap_or_default();

        let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, row_h, entries.len(), |ui, range| {
                for (level, msg) in &entries[range] {
                    let color = match level {
                        LogLevel::Info  => Color32::from_gray(210),
                        LogLevel::Warn  => Color32::from_rgb(255, 210, 60),
                        LogLevel::Error => Color32::from_rgb(255, 90, 90),
                    };
                    ui.label(RichText::new(msg).monospace().color(color).size(12.0));
                }
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
        self.poll_cells();

        egui::CentralPanel::default().show(ctx, |ui| {
            // ── Header ────────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.heading("🧱 MineScale-Java");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let idle = self.state == RunState::Idle;
                    ui.add_enabled_ui(idle, |ui| {
                        ui.selectable_value(&mut self.mode, Mode::Join, "Join");
                        ui.selectable_value(&mut self.mode, Mode::Host, "Host");
                    });
                });
            });
            ui.separator();
            ui.add_space(4.0);

            // ── Mode panel ────────────────────────────────────────────────────
            match self.mode {
                Mode::Host => self.show_host_panel(ui, ctx),
                Mode::Join => self.show_join_panel(ui, ctx),
            }

            // ── Log ───────────────────────────────────────────────────────────
            ui.add_space(8.0);
            self.show_log_panel(ui);
        });
    }
}
