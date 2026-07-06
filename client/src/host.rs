use crate::{cli::HostArgs, coord, crypto, diag, lan, relay, stun, telemetry, tunnel};
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::time::Duration;
use tracing::info;

// ── Public config (used by GUI as well as CLI) ────────────────────────────────

pub struct HostConfig {
    pub mc_port: u16,
    pub coord_url: String,
    pub stun_server: String,
    /// Called once when the share URL is ready (e.g. to show it in a GUI).
    pub on_share_url: Option<Box<dyn FnOnce(String) + Send>>,
    /// Signals cancellation.  Drop the sender to cancel.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Opt-in connection diagnostics. Off by default.
    pub telemetry: bool,
    /// `cli` for the bare binary, `gui` from the GUI front-end.
    pub app_kind: telemetry::AppKind,
}

impl From<HostArgs> for HostConfig {
    fn from(a: HostArgs) -> Self {
        Self {
            mc_port: a.port,
            coord_url: a.coord_url,
            stun_server: a.stun_server,
            on_share_url: None,
            cancel: tokio_util::sync::CancellationToken::new(),
            telemetry: telemetry::enabled(a.telemetry),
            app_kind: telemetry::AppKind::Cli,
        }
    }
}

// ── Entry points ──────────────────────────────────────────────────────────────

pub async fn run(args: HostArgs) -> Result<()> {
    run_with_config(args.into()).await
}

pub async fn run_with_config(mut config: HostConfig) -> Result<()> {
    let mut report = telemetry::Reporter::new(
        config.telemetry,
        config.coord_url.clone(),
        telemetry::Role::Host,
        config.app_kind,
    );
    if config.telemetry {
        let d = diag::run().await;
        report.set_network(d.nat_type.label(), d.ipv6_available);
        report.send_start().await;
    }

    let result = run_inner(&mut config, &mut report).await;

    // Classify error → outcome
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
    config: &mut HostConfig,
    report: &mut telemetry::Reporter,
) -> Result<()> {
    // ── 1. Detect or use Minecraft port ──────────────────────────────────────
    let mc_port = if config.mc_port != 0 {
        config.mc_port
    } else {
        println!("  … Looking for a running LAN world…");
        match lan::detect_lan_world(Duration::from_secs(3)).await {
            Some(p) => { info!("Found LAN world on port {}", p); p }
            None    => { info!("No LAN world detected — using default 25565"); 25565 }
        }
    };

    // ── 2. Ephemeral keypair + TLS cert ───────────────────────────────────────
    let keypair   = crypto::Keypair::generate();
    let cert_key  = crypto::generate_self_signed_cert()?;
    let pubkey_b64 = STANDARD.encode(keypair.public_bytes());
    let fp_b64     = STANDARD.encode(crypto::cert_fingerprint(cert_key.cert.der().as_ref()));

    // ── 3. STUN ───────────────────────────────────────────────────────────────
    println!("  … Discovering external address via STUN…");
    let (udp_socket, external_addr) = stun::get_external_addr(&config.stun_server).await?;
    info!("External address: {}", external_addr);

    // ── 4. Register room ──────────────────────────────────────────────────────
    println!("  … Registering room on coordination server…");
    let coord = coord::Client::new(config.coord_url.clone());
    let room = coord.create_room(coord::CreateRoomRequest {
        host_pubkey: pubkey_b64,
        host_stun: external_addr.to_string(),
        cert_fingerprint: fp_b64,
    }).await?;

    report.set_room(&room.room_id);
    report.send_event(telemetry::Phase::Registered).await;
    print_share_link(&room.share_url);
    if let Some(cb) = config.on_share_url.take() {
        cb(room.share_url.clone());
    }

    // ── 5. Wait for first joiner ──────────────────────────────────────────────
    println!("  … Waiting for someone to join…");
    let first_peer = coord.wait_for_peer(
        &room.room_id, &room.host_token, 0, Duration::from_secs(900),
    ).await?;
    info!("First joiner at {}", first_peer.join_stun);

    // ── 6. Hole punch with first joiner ───────────────────────────────────────
    let first_addr: std::net::SocketAddr = first_peer.join_stun.parse()?;
    tunnel::punch_holes(&udp_socket, first_addr).await?;

    // ── 7. Start QUIC server + relay park pool ────────────────────────────────
    let mc_addr: std::net::SocketAddr = format!("127.0.0.1:{}", mc_port).parse()?;
    info!("Forwarding to Minecraft at {}", mc_addr);
    println!("\n  Friend connected! Tunnelling to 127.0.0.1:{} …\n", mc_port);
    println!("  (The same link can be shared with more friends)\n");

    // The `Connected` checkpoint should fire only when a joiner has
    // actually reached us — either over QUIC or over relay. Whichever
    // side wins fires it first; subsequent fires are idempotent.
    let connected_flag = report.connected_flag.clone();
    let report_for_quic = report.clone();
    let report_for_relay = report.clone();

    let on_quic_connect: Box<dyn Fn() + Send + Sync> = {
        let cf = connected_flag.clone();
        Box::new(move || {
            if !cf.swap(true, std::sync::atomic::Ordering::Relaxed) {
                let r = report_for_quic.clone();
                tokio::spawn(async move {
                    r.set_transport("quic");
                    r.send_event(telemetry::Phase::Connected).await;
                });
            }
        })
    };

    // A room-expired signal shared between the joiner poller (which
    // detects the 404 from the coord server) and the relay pool
    // (which should stop spawning new host-side parks once the room
    // is dead — otherwise it hammers the relay server for the rest
    // of the session, one connect per free slot). Existing paired
    // pipes are unaffected: they own their own TcpStreams and pipe
    // until game traffic actually stops.
    let stop_new_parks = tokio_util::sync::CancellationToken::new();

    // Spawn the relay park pool if the coordination server gave us a
    // reachable relay address. The pool maintains 4 parked connections
    // so several joiners' simultaneous MC connections (status pings +
    // world joins) all find a ready park.
    let cancel = config.cancel.clone();
    if let Ok(relay_addr) = relay::parse_relay_addr(&room.relay_addr) {
        let cf = connected_flag.clone();
        let report_relay = report_for_relay.clone();
        let room_id = room.room_id.clone();
        let token = room.relay_token.clone();
        // Pool stops when either the main cancel fires OR the room
        // expires. Both are represented by the same CancellationToken
        // API, so we build a combined token: cancel any one of them
        // and the pool exits its parking loop.
        let cancel_relay = cancel.clone();
        let stop_parks_for_pool = stop_new_parks.clone();
        let combined = tokio_util::sync::CancellationToken::new();
        let combined_child = combined.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel_relay.cancelled() => {}
                _ = stop_parks_for_pool.cancelled() => {}
            }
            combined_child.cancel();
        });
        tokio::spawn(async move {
            relay::host_pool(
                relay_addr,
                room_id,
                token,
                mc_addr,
                4,
                combined,
                move || {
                    if !cf.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        let r = report_relay.clone();
                        tokio::spawn(async move {
                            r.set_transport("relay");
                            r.send_event(telemetry::Phase::Connected).await;
                        });
                    }
                },
            )
            .await;
        });
    } else {
        tracing::warn!("relay disabled: bad relay address {:?}", room.relay_addr);
    }

    // Channel: poll_more_joiners → run_host, so run_host can poke new joiners
    // from the QUIC port (required for Port-Restricted Cone NAT).
    let (joiner_tx, joiner_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn the joiner poller as a *detached* task. Previously this was
    // in the same tokio::select! as run_host, which meant the poller
    // returning (either because the room expired on the coord server or
    // because it hit its own deadline) would cancel run_host — killing
    // every existing joiner's tunnel. The room's coord-side lifetime
    // (~15 min) is completely independent of how long we serve joiners
    // that already connected; those should keep flowing until the user
    // stops the host explicitly.
    let poller_cancel = cancel.clone();
    let poller = tokio::spawn(poll_more_joiners(
        coord,
        room.room_id.clone(),
        room.host_token.clone(),
        first_peer.idx + 1,
        poller_cancel,
        joiner_tx,
        stop_new_parks.clone(),
    ));

    let result = tokio::select! {
        r = tunnel::run_host(udp_socket, first_addr, cert_key, mc_addr, joiner_rx, Some(on_quic_connect)) => r,
        _ = cancel.cancelled() => Ok(()),
    };

    // Clean up: stop the poller when we exit.
    poller.abort();
    result?;
    Ok(())
}

/// Background task: poll for new joiners and forward their addresses to
/// `run_host` so it can poke them from the QUIC port.
async fn poll_more_joiners(
    coord: coord::Client,
    room_id: String,
    host_token: String,
    mut next_idx: usize,
    cancel: tokio_util::sync::CancellationToken,
    joiner_tx: tokio::sync::mpsc::UnboundedSender<std::net::SocketAddr>,
    stop_new_parks: tokio_util::sync::CancellationToken,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(14 * 60 + 30);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => { return; }
            _ = tick.tick() => {}
        }
        if tokio::time::Instant::now() >= deadline {
            // We won't poll further, and there's no point in the relay
            // pool trying to park either — the room is (or will very
            // soon be) gone from the coord's registry.
            stop_new_parks.cancel();
            return;
        }

        match coord.poll_peers(&room_id, &host_token, next_idx).await {
            Ok(Some(peers)) => {
                for peer in peers {
                    if let Ok(addr) = peer.join_stun.parse() {
                        info!("New joiner #{} detected at {} — sending to QUIC poke task", peer.idx, addr);
                        // run_host receives this and pokes the joiner from the
                        // QUIC port, opening the host's NAT for the joiner.
                        let _ = joiner_tx.send(addr);
                        next_idx = peer.idx + 1;
                    }
                }
            }
            Ok(None) => {
                // 404 — the coord server expired the room (15 min lifetime).
                // Stop polling AND signal the relay pool to stop parking:
                // continued re-parks would just hammer the relay server
                // with "room not found" for the rest of the session.
                info!("Room {} has expired on the coordination server — \
                      no more joiners can connect (the share link is now dead). \
                      Existing connections remain active.", room_id);
                stop_new_parks.cancel();
                return;
            }
            Err(e) => tracing::warn!("poll_more_joiners: {}", e),
        }
    }
}

// ── UI helpers ────────────────────────────────────────────────────────────────

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
    // First line, capped at 120 bytes — no IPs or external addresses get
    // printed by our error types, so this is safe to forward.
    let line = msg.lines().next().unwrap_or("");
    if line.len() <= 120 { line } else { &line[..120] }
}

fn print_share_link(url: &str) {
    println!();
    println!("  ┌────────────────────────────────────────────────┐");
    println!("  │  World shared! Send this link to your friend:  │");
    println!("  │                                                  │");
    println!("  │  {}  │", url);
    println!("  └────────────────────────────────────────────────┘");
    println!();
}
