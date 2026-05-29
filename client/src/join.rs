use crate::{cli::JoinArgs, coord, crypto, lan, stun, tunnel};
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
        }
    }
}

// ── Entry points ──────────────────────────────────────────────────────────────

pub async fn run(args: JoinArgs) -> Result<()> {
    run_with_config(args.into()).await
}

pub async fn run_with_config(mut config: JoinConfig) -> Result<()> {
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

    // ── 4. Hole punch ─────────────────────────────────────────────────────────
    println!("  … Establishing P2P connection…");
    let host_addr: std::net::SocketAddr = room.host_stun.parse()?;
    tunnel::punch_holes(&udp_socket, host_addr).await?;

    // ── 5. Bind local proxy ───────────────────────────────────────────────────
    let local_port = resolve_local_port(config.local_port).await?;
    let local_addr: std::net::SocketAddr = format!("0.0.0.0:{}", local_port).parse()?;

    // ── 6. LAN world announcement ─────────────────────────────────────────────
    tokio::spawn(lan::announce_lan_world("MineScale World", local_port));

    if let Some(cb) = config.on_connected.take() { cb(local_port); }
    print_connected(local_port);

    // ── 7. QUIC tunnel ────────────────────────────────────────────────────────
    let cert_fingerprint = STANDARD.decode(&room.cert_fingerprint)?;
    let cancel = config.cancel.clone();

    tokio::select! {
        r = tunnel::run_join(udp_socket, host_addr, cert_fingerprint, local_addr) => r?,
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
