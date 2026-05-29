use crate::{cli::JoinArgs, coord, crypto, lan, stun, tunnel};
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::time::Duration;
use tracing::info;

pub async fn run(args: JoinArgs) -> Result<()> {
    let room_id = coord::parse_room_id(&args.target);

    // ── 1. Generate ephemeral keypair ─────────────────────────────────────────
    let keypair = crypto::Keypair::generate();
    let pubkey_b64 = STANDARD.encode(keypair.public_bytes());

    // ── 2. STUN ───────────────────────────────────────────────────────────────
    println!("  … Discovering external address…");
    let (udp_socket, external_addr) = stun::get_external_addr(&args.stun_server).await?;
    info!("External address: {}", external_addr);

    // ── 3. Join the room on the coordination server ───────────────────────────
    println!("  … Contacting coordination server…");
    let coord = coord::Client::new(args.coord_url.clone());
    let room = coord
        .join_room(
            &room_id,
            coord::JoinRoomRequest {
                join_pubkey: pubkey_b64,
                join_stun: external_addr.to_string(),
            },
        )
        .await?;

    // ── 4. UDP hole punching ──────────────────────────────────────────────────
    println!("  … Establishing P2P connection…");
    let host_addr: std::net::SocketAddr = room.host_stun.parse()?;
    tunnel::punch_holes(&udp_socket, host_addr).await?;

    // ── 5. Resolve local port ─────────────────────────────────────────────────
    let local_port = resolve_local_port(args.port).await?;
    let local_addr: std::net::SocketAddr = format!("127.0.0.1:{}", local_port).parse()?;

    // ── 6. Announce as LAN world ──────────────────────────────────────────────
    let motd = "MineScale World";
    tokio::spawn(lan::announce_lan_world(motd, local_port));

    print_connected(local_port);

    // ── 7. Run QUIC client tunnel ─────────────────────────────────────────────
    let cert_fingerprint = STANDARD.decode(&room.cert_fingerprint)?;
    tunnel::run_join(udp_socket, host_addr, cert_fingerprint, local_addr).await
}

async fn resolve_local_port(preferred: u16) -> Result<u16> {
    if preferred != 0 {
        // Try the preferred port first
        if let Ok(l) = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", preferred)).await {
            return Ok(l.local_addr()?.port());
        }
    }
    // Fall back to a random port
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
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
