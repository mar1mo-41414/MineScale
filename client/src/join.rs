use crate::{cli::JoinArgs, coord, crypto, diag, lan, relay, stun, telemetry, tunnel};
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::time::Duration;
use tracing::info;

// ── Public config ─────────────────────────────────────────────────────────────

pub struct JoinConfig {
    pub target: String,
    pub local_port: u16,
    pub coord_url: String,
    pub stun_server: String,
    /// Called once with the local proxy port when the tunnel is ready.
    pub on_connected: Option<Box<dyn FnOnce(u16) + Send>>,
    pub cancel: tokio_util::sync::CancellationToken,
    /// Opt-in connection diagnostics. Off by default.
    pub telemetry: bool,
    pub app_kind: telemetry::AppKind,
}

impl From<JoinArgs> for JoinConfig {
    fn from(a: JoinArgs) -> Self {
        Self {
            target: a.target,
            local_port: a.port,
            coord_url: a.coord_url,
            stun_server: a.stun_server,
            on_connected: None,
            cancel: tokio_util::sync::CancellationToken::new(),
            telemetry: telemetry::enabled(a.telemetry),
            app_kind: telemetry::AppKind::Cli,
        }
    }
}

// ── Entry points ──────────────────────────────────────────────────────────────

pub async fn run(args: JoinArgs) -> Result<()> {
    run_with_config(args.into()).await
}

pub async fn run_with_config(config: JoinConfig) -> Result<()> {
    let mut report = telemetry::Reporter::new(
        config.telemetry,
        config.coord_url.clone(),
        telemetry::Role::Join,
        config.app_kind,
    );
    // Join already knows the room_id (it's in the share URL), so we set
    // it BEFORE the start event — every join event carries the pairing key.
    report.set_room(&coord::parse_room_id(&config.target));
    if config.telemetry {
        let d = diag::run().await;
        report.set_network(d.nat_type.label(), d.ipv6_available);
        report.send_start().await;
    }

    let result = run_inner(config, &mut report).await;

    match &result {
        Ok(()) => report.send_result(telemetry::Outcome::Success, None).await,
        Err(e) => {
            let msg = format!("{:#}", e);
            let outcome = classify_error(&msg);
            report.send_result(outcome, Some(short(&msg))).await;
        }
    }
    result
}

async fn run_inner(
    mut config: JoinConfig,
    report: &mut telemetry::Reporter,
) -> Result<()> {
    let room_id = coord::parse_room_id(&config.target);

    // ── 1. Keypair ────────────────────────────────────────────────────────────
    let keypair    = crypto::Keypair::generate();
    let pubkey_b64 = STANDARD.encode(keypair.public_bytes());

    // ── 2. STUN ───────────────────────────────────────────────────────────────
    println!("  … Discovering external address…");
    let (udp_socket, external_addr) = stun::get_external_addr(&config.stun_server).await?;
    info!("External address: {}", external_addr);

    // ── 3. Join the room ──────────────────────────────────────────────────────
    println!("  … Contacting coordination server…");
    let coord = coord::Client::new(config.coord_url.clone());
    let room = coord.join_room(
        &room_id,
        coord::JoinRoomRequest { join_pubkey: pubkey_b64, join_stun: external_addr.to_string() },
    ).await?;
    report.send_event(telemetry::Phase::Registered).await;

    // ── 4. Hole punch ─────────────────────────────────────────────────────────
    println!("  … Establishing P2P connection…");
    let host_addr: std::net::SocketAddr = room.host_stun.parse()?;
    tunnel::punch_holes(&udp_socket, host_addr).await?;

    // ── 5. Bind local proxy ───────────────────────────────────────────────────
    let local_port = resolve_local_port(config.local_port).await?;
    let local_addr: std::net::SocketAddr = format!("0.0.0.0:{}", local_port).parse()?;

    // ── 6. LAN world announcement ─────────────────────────────────────────────
    tokio::spawn(lan::announce_lan_world("MineScale World", local_port));

    print_connected(local_port);

    // ── 7. QUIC tunnel (with relay fallback) ──────────────────────────────────
    // on_connected fires INSIDE run_join, after a usable transport is selected
    // (QUIC or relay) AND the local TCP listener is bound — guaranteeing
    // Minecraft won't see Connection refused. The telemetry checkpoint is
    // chained so it fires the moment the tunnel is usable.
    let cert_fingerprint = STANDARD.decode(&room.cert_fingerprint)?;
    let user_on_connected = config.on_connected.take();
    let report_for_cb = report.clone();
    let on_connected: Option<Box<dyn FnOnce(u16) + Send>> =
        Some(Box::new(move |port: u16| {
            if let Some(cb) = user_on_connected { cb(port); }
            tokio::spawn(async move {
                report_for_cb.send_event(telemetry::Phase::Connected).await;
            });
        }));

    let report_for_transport = report.clone();
    let on_transport: Option<Box<dyn FnOnce(&str) + Send>> =
        Some(Box::new(move |t: &str| {
            let mut r = report_for_transport;
            r.set_transport(t);
        }));

    let relay_fallback = relay::parse_relay_addr(&room.relay_addr).ok().map(|addr| {
        tunnel::RelayFallback {
            addr,
            room_id: room_id.clone(),
            token: room.relay_token.clone(),
        }
    });

    let cancel = config.cancel.clone();

    tokio::select! {
        r = tunnel::run_join(
            udp_socket, host_addr, cert_fingerprint, local_addr,
            relay_fallback, on_connected, on_transport,
        ) => r?,
        _ = cancel.cancelled() => {}
    }

    Ok(())
}

async fn resolve_local_port(preferred: u16) -> Result<u16> {
    if preferred != 0 {
        if let Ok(l) = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", preferred)).await {
            return Ok(l.local_addr()?.port());
        }
    }
    let l = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    Ok(l.local_addr()?.port())
}

fn classify_error(msg: &str) -> telemetry::Outcome {
    let m = msg.to_ascii_lowercase();
    if m.contains("stun") { telemetry::Outcome::StunFailed }
    else if m.contains("coord") || m.contains("room") || m.contains("http") {
        telemetry::Outcome::CoordFailed
    }
    else if m.contains("punch") || m.contains("hole") { telemetry::Outcome::PunchFailed }
    else if m.contains("tls") || m.contains("certificate") { telemetry::Outcome::TlsFailed }
    else if m.contains("quic") || m.contains("connect")    { telemetry::Outcome::QuicFailed }
    else if m.contains("cancel") { telemetry::Outcome::Cancelled }
    else { telemetry::Outcome::Other }
}

fn short(msg: &str) -> &str {
    let line = msg.lines().next().unwrap_or("");
    if line.len() <= 120 { line } else { &line[..120] }
}

fn print_connected(port: u16) {
    println!();
    println!("  ┌──────────────────────────────────────────────────────────┐");
    println!("  │  Connected!                                               │");
    println!("  │                                                            │");
    println!("  │  Open Minecraft → Multiplayer.                            │");
    println!("  │  The world should appear automatically in the list.       │");
    if port != 25565 {
        println!("  │                                                            │");
        println!("  │  Or connect directly to:  127.0.0.1:{}                │", port);
    }
    println!("  └──────────────────────────────────────────────────────────┘");
    println!();
}
