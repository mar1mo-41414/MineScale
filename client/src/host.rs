use crate::{cli::HostArgs, coord, crypto, lan, stun, tunnel};
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
}

impl From<HostArgs> for HostConfig {
    fn from(a: HostArgs) -> Self {
        Self {
            mc_port: a.port,
            coord_url: a.coord_url,
            stun_server: a.stun_server,
            on_share_url: None,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
}

// ── Entry points ──────────────────────────────────────────────────────────────

pub async fn run(args: HostArgs) -> Result<()> {
    run_with_config(args.into()).await
}

pub async fn run_with_config(mut config: HostConfig) -> Result<()> {
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

    // ── 7. Start QUIC server ──────────────────────────────────────────────────
    let mc_addr: std::net::SocketAddr = format!("127.0.0.1:{}", mc_port).parse()?;
    info!("Forwarding to Minecraft at {}", mc_addr);
    println!("\n  Friend connected! Tunnelling to 127.0.0.1:{} …\n", mc_port);
    println!("  (The same link can be shared with more friends)\n");

    // Run QUIC server and multi-joiner poller concurrently.
    let cancel = config.cancel.clone();
    tokio::select! {
        r = tunnel::run_host(udp_socket, first_addr, cert_key, mc_addr) => r?,
        _ = poll_more_joiners(
                coord,
                room.room_id.clone(),
                room.host_token.clone(),
                first_peer.idx + 1,   // next expected index
                cancel,
            ) => {}
    }

    Ok(())
}

/// Background task: poll for more joiners after the first one, warm-up NAT holes.
async fn poll_more_joiners(
    coord: coord::Client,
    room_id: String,
    host_token: String,
    mut next_idx: usize,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    // Poll for up to the room's 15-minute lifetime
    let deadline = tokio::time::Instant::now() + Duration::from_secs(14 * 60 + 30);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => { return; }
            _ = tick.tick() => {}
        }
        if tokio::time::Instant::now() >= deadline { return; }

        match coord.poll_peers(&room_id, &host_token, next_idx).await {
            Ok(peers) => {
                for peer in peers {
                    if let Ok(addr) = peer.join_stun.parse() {
                        info!("New joiner #{} at {} — warming up NAT hole", peer.idx, addr);
                        tokio::spawn(tunnel::warm_up_hole(addr));
                        next_idx = peer.idx + 1;
                    }
                }
            }
            Err(e) => tracing::warn!("poll_more_joiners error: {}", e),
        }
    }
}

// ── UI helpers ────────────────────────────────────────────────────────────────

fn print_share_link(url: &str) {
    println!();
    println!("  ┌────────────────────────────────────────────────┐");
    println!("  │  World shared! Send this link to your friend:  │");
    println!("  │                                                  │");
    println!("  │  {}  │", url);
    println!("  └────────────────────────────────────────────────┘");
    println!();
}
