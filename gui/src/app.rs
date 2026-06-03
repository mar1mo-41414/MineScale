use eframe::egui::{self, Color32, RichText, ScrollArea, Ui};
use mc_share::{
    diag::{DiagResult, NatType},
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
enum Mode { Host, Join, Diag }

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

    /// When the host room expires (15 min after share URL appears).
    room_expires_at: Option<std::time::Instant>,

    // ── diagnostics ──────────────────────────────────────────────────────────
    diag_result:  Arc<Mutex<Option<DiagResult>>>,
    diag_running: bool,

    // ── log ──────────────────────────────────────────────────────────────────
    log: Arc<Mutex<Vec<LogEntry>>>,

    // ── cancellation ─────────────────────────────────────────────────────────
    cancel: Option<CancellationToken>,

    // ── opt-in telemetry flag (set via CLI flag or env var at startup) ──────
    telemetry_enabled: bool,
}

impl App {
    pub fn new(
        rt: Arc<tokio::runtime::Runtime>,
        log: Arc<Mutex<Vec<LogEntry>>>,
        telemetry_cli_flag: bool,
    ) -> Self {
        // CLI flag wins; otherwise fall back to MC_SHARE_TELEMETRY env var.
        let telemetry_enabled = mc_share::telemetry::enabled(telemetry_cli_flag);
        if telemetry_enabled {
            tracing::info!(
                "Telemetry enabled (anonymous connection diagnostics — see README)"
            );
        }
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
            room_expires_at: None,
            diag_result:  Arc::new(Mutex::new(None)),
            diag_running: false,
            log,
            cancel: None,
            telemetry_enabled,
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn push_log(&self, level: LogLevel, msg: impl Into<String>) {
        if let Ok(mut v) = self.log.lock() {
            v.push((level, msg.into()));
        }
    }

    fn reset_outputs(&mut self) {
        self.share_url       = String::new();
        self.local_port      = 0;
        self.room_expires_at = None;
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
        let telemetry_enabled = self.telemetry_enabled;

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
                telemetry: telemetry_enabled,
                app_kind: mc_share::telemetry::AppKind::Gui,
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
        let telemetry_enabled = self.telemetry_enabled;

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
                telemetry: telemetry_enabled,
                app_kind: mc_share::telemetry::AppKind::Gui,
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
                if let Some(u) = c.as_ref() {
                    self.share_url = u.clone();
                    // Start the 15-minute room expiry countdown.
                    self.room_expires_at = Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_secs(15 * 60),
                    );
                }
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

            // ── Room expiry countdown ────────────────────────────────────────
            if let Some(expires_at) = self.room_expires_at {
                let now = std::time::Instant::now();
                if now < expires_at {
                    let rem = expires_at - now;
                    let mins = rem.as_secs() / 60;
                    let secs = rem.as_secs() % 60;
                    let color = if rem.as_secs() < 60 {
                        Color32::from_rgb(255, 160, 50)  // orange: last minute
                    } else {
                        Color32::from_gray(160)
                    };
                    ui.horizontal(|ui| {
                        ui.colored_label(color,
                            format!("⏱  New joiners accepted for {:02}:{:02}", mins, secs));
                    });
                } else {
                    ui.colored_label(
                        Color32::from_rgb(200, 80, 80),
                        "⏱  Room expired — new joiners cannot connect (existing players stay connected)",
                    );
                }
            }
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
                    ui.label(RichText::new(
                        "  If the world doesn't appear or refuses to join,\n  \
                         use \"Add Server\" with the direct address below:"
                    ).color(Color32::GRAY).size(12.0));
                    ui.label(RichText::new(
                        format!("    127.0.0.1:{}", self.local_port)
                    ).monospace());
                    ui.add_space(4.0);
                } else {
                    ui.label(RichText::new("  Connecting…").italics().color(Color32::GRAY));
                }
                if ui.button("  Disconnect  ").clicked() { self.stop(); }
            }
        }
    }

    // ── Diagnostics ───────────────────────────────────────────────────────────

    fn start_diag(&mut self, ctx: egui::Context) {
        self.diag_running = true;
        if let Ok(mut r) = self.diag_result.lock() { *r = None; }

        let cell = Arc::clone(&self.diag_result);
        let rt   = Arc::clone(&self.rt);
        let ctx2 = ctx.clone();

        rt.spawn(async move {
            let result = mc_share::diag::run().await;
            if let Ok(mut c) = cell.lock() { *c = Some(result); }
            ctx2.request_repaint();
        });
    }

    fn show_diag_panel(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.add_space(4.0);

        let running = self.diag_running
            && self.diag_result.lock().map(|r| r.is_none()).unwrap_or(true);

        ui.horizontal(|ui| {
            let btn = egui::Button::new(
                RichText::new(if running { "  Running...  " } else { "  Run Diagnostics  " }).size(15.0),
            );
            if ui.add_enabled(!running, btn).clicked() {
                self.start_diag(ctx.clone());
            }
            if running {
                ui.spinner();
            }
        });

        let result_opt: Option<DiagResult> = self
            .diag_result
            .lock()
            .ok()
            .and_then(|r| r.clone());

        // Update running flag when result arrives
        if result_opt.is_some() { self.diag_running = false; }

        let Some(r) = result_opt else {
            if !running {
                ui.add_space(8.0);
                ui.label(RichText::new("Press the button to start diagnostics.").color(Color32::GRAY));
            }
            return;
        };

        ui.add_space(8.0);

        // ── Network ──────────────────────────────────────────────────────────
        ui.label(RichText::new("Network").strong());
        ui.separator();

        egui::Grid::new("diag_net")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                // External IPv4 (primary)
                ui.label("External Address (IPv4)");
                match r.ext_v4_primary {
                    Some(a) => { ui.label(RichText::new(a.to_string()).monospace()); }
                    None    => { ui.colored_label(Color32::from_rgb(255,90,90), "Failed"); }
                }
                ui.end_row();

                // External IPv4 (secondary — for NAT comparison)
                if let Some(a2) = r.ext_v4_secondary {
                    ui.label("External Address (STUN2)");
                    ui.label(RichText::new(a2.to_string()).monospace());
                    ui.end_row();
                }

                // NAT type
                ui.label("NAT Type");
                let (icon, color) = match r.nat_type {
                    NatType::Cone          => ("✅", Color32::from_rgb(100, 220, 100)),
                    NatType::Indeterminate => ("⚠️", Color32::from_rgb(255, 210, 60)),
                    NatType::UdpBlocked    => ("❌", Color32::from_rgb(255,  90,  90)),
                    NatType::Symmetric     => ("⚠️", Color32::from_rgb(255, 160,  50)),
                };
                ui.colored_label(color, format!("{} {}", icon, r.nat_type.label()));
                ui.end_row();

                // UDP
                ui.label("UDP");
                let udp_ok = r.ext_v4_primary.is_some();
                if udp_ok {
                    ui.colored_label(Color32::from_rgb(100, 220, 100), "✅ Available");
                } else {
                    ui.colored_label(Color32::from_rgb(255,  90,  90), "❌ Blocked / Failed");
                }
                ui.end_row();

                // IPv6
                ui.label("IPv6");
                if r.ipv6_available {
                    ui.colored_label(Color32::from_rgb(100, 220, 100), "✅ Available");
                } else {
                    ui.colored_label(Color32::GRAY, "— Unavailable");
                }
                ui.end_row();
            });

        // NAT type hint
        ui.add_space(4.0);
        match r.nat_type {
            NatType::Symmetric => {
                ui.colored_label(
                    Color32::from_rgb(255, 160, 50),
                    "⚠ Symmetric NAT — P2P is difficult.\n\
                     Relay will be used automatically when connecting.",
                );
            }
            NatType::UdpBlocked => {
                ui.colored_label(
                    Color32::from_rgb(255, 90, 90),
                    "❌ UDP is blocked. Check your firewall settings.",
                );
            }
            _ => {}
        }

        ui.add_space(12.0);

        // ── System ───────────────────────────────────────────────────────────
        ui.label(RichText::new("System").strong());
        ui.separator();
        egui::Grid::new("diag_sys")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label("OS");
                ui.label(&r.os_detail);
                ui.end_row();

                ui.label("Architecture");
                ui.label(RichText::new(&r.arch).monospace());
                ui.end_row();
            });

        ui.add_space(12.0);

        // ── Minecraft ────────────────────────────────────────────────────────
        ui.label(RichText::new("Minecraft Java Edition").strong());
        ui.separator();
        egui::Grid::new("diag_mc")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label("Data folder");
                match &r.mc_dir {
                    Some(d) => {
                        ui.label(RichText::new(d.display().to_string()).monospace().size(11.0));
                    }
                    None => {
                        ui.colored_label(Color32::GRAY, "— Not found");
                    }
                }
                ui.end_row();

                ui.label("Installed versions");
                if r.mc_versions.is_empty() {
                    ui.colored_label(Color32::GRAY, "— None detected");
                } else {
                    ui.label(r.mc_versions.join(", "));
                }
                ui.end_row();
            });
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
                    ui.selectable_value(&mut self.mode, Mode::Diag, "Diag");
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
                Mode::Diag => {
                    ScrollArea::vertical()
                        .max_height(ui.available_height() - 8.0)
                        .show(ui, |ui| self.show_diag_panel(ui, ctx));
                }
            }

            // ── Log ───────────────────────────────────────────────────────────
            ui.add_space(8.0);
            self.show_log_panel(ui);
        });
    }
}
