use crate::{cli::HostArgs, coord, crypto, lan, stun, tunnel};
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::time::Duration;
use tracing::info;

pub async fn run(args: HostArgs) -> Result<()> {
    // ── 1. Detect or use Minecraft server port ────────────────────────────────
    let mc_port = if args.port != 0 {
        args.port
    } else {
        print_waiting("Looking for a running LAN world…");
        match lan::detect_lan_world(Duration::from_secs(3)).await {
            Some(p) => {
                info!("Found LAN world on port {}", p);
                p
            }
            None => {
                info!("No LAN world detected — using default port 25565");
                25565
            }
        }
    };

    // ── 2. Generate ephemeral keypair + TLS cert ──────────────────────────────
    let keypair = crypto::Keypair::generate();
    let cert_key = crypto::generate_self_signed_cert()?;
    let pubkey_b64 = STANDARD.encode(keypair.public_bytes());
    let fp_b64 = STANDARD.encode(crypto::cert_fingerprint(cert_key.cert.der().as_ref()));

    // ── 3. STUN — discover our external address ───────────────────────────────
    print_waiting("Discovering external address via STUN…");
    let (udp_socket, external_addr) = stun::get_external_addr(&args.stun_server).await?;
    info!("External address: {}", external_addr);

    // ── 4. Register room on coordination server ───────────────────────────────
    print_waiting("Registering room on coordination server…");
    let coord = coord::Client::new(args.coord_url.clone());
    let room = coord
        .create_room(coord::CreateRoomRequest {
            host_pubkey: pubkey_b64,
            host_stun: external_addr.to_string(),
            cert_fingerprint: fp_b64,
        })
        .await?;

    print_share_link(&room.share_url);

    // ── 5. Wait for a joiner to appear ────────────────────────────────────────
    print_waiting("Waiting for someone to join…");
    let peer = coord
        .wait_for_peer(&room.room_id, &room.host_token, Duration::from_secs(900))
        .await?;
    info!("Joiner appeared at {}", peer.join_stun);

    // ── 6. UDP hole punching ──────────────────────────────────────────────────
    let peer_addr: std::net::SocketAddr = peer.join_stun.parse()?;
    tunnel::punch_holes(&udp_socket, peer_addr).await?;

    // ── 7. Start QUIC server tunnel ───────────────────────────────────────────
    let mc_addr: std::net::SocketAddr = format!("127.0.0.1:{}", mc_port).parse()?;
    info!("Forwarding to Minecraft server at {}", mc_addr);
    println!("\n  Friend connected! Tunnelling to 127.0.0.1:{} …\n", mc_port);

    tunnel::run_host(udp_socket, peer_addr, cert_key, mc_addr).await
}

fn print_waiting(msg: &str) {
    println!("  … {}", msg);
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
