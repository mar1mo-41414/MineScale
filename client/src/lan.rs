use anyhow::Result;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
const MULTICAST_PORT: u16 = 4445;
/// Minecraft expects announcements roughly every second.
const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1500);

/// Broadcast a fake LAN world to the local network so Minecraft's
/// multiplayer screen shows the world automatically.
///
/// History (so we don't re-learn the same lessons):
///   - v1.2.6 used TTL=1 on the default interface. Reliable everywhere
///     but LAN-mates also see the entry.
///   - v1.2.7 routed via the loopback interface to force source IP =
///     127.0.0.1 (so MC's LAN-click would handshake as a local client
///     and bypass online auth). macOS broke this — the local MC missed
///     packets from loopback multicast.
///   - v1.2.8 tried default interface with TTL=0, hoping multicast
///     loopback would still deliver to the local MC while LAN-mates
///     stayed blind. Empirically, macOS's multicast loopback path is
///     too flaky for this to be reliable: when the Mac is on a
///     network with VPN tunnels, the route table sometimes sends
///     224.0.0.0/4 only out the VPN, and the local MC never sees it.
///
/// We go back to TTL=1 on the default interface. The LAN-mate privacy
/// loss is acceptable (the joiner's own MC always sees the entry; LAN
/// mates can see it but can't actually join unless they have an MC
/// account that the host accepts, and even then they need to know the
/// joiner is using mc-share). What we cannot afford is the announce
/// silently not reaching the joiner's own Minecraft.
pub async fn announce_lan_world(motd: &str, port: u16) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(1)?;

    let announcement = format!("[MOTD]{motd}[/MOTD][AD]{port}[/AD]");
    let target = SocketAddrV4::new(MULTICAST_ADDR, MULTICAST_PORT);

    tracing::info!(
        "Announcing LAN world \"{}\" on multicast {}:{}",
        motd,
        MULTICAST_ADDR,
        MULTICAST_PORT
    );

    loop {
        if let Err(e) = socket.send_to(announcement.as_bytes(), target).await {
            tracing::warn!("LAN announce send failed: {}", e);
        }
        tokio::time::sleep(ANNOUNCE_INTERVAL).await;
    }
}

/// Scan for existing Minecraft LAN world announcements.
/// Returns the first port found within `timeout`, or `None`.
pub async fn detect_lan_world(timeout: Duration) -> Option<u16> {
    let socket =
        UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MULTICAST_PORT)).await.ok()?;
    socket
        .join_multicast_v4(MULTICAST_ADDR, Ipv4Addr::UNSPECIFIED)
        .ok()?;

    let mut buf = [0u8; 512];

    tokio::time::timeout(timeout, async {
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, _)) => {
                    if let Some(port) = parse_announcement(&buf[..len]) {
                        return Some(port);
                    }
                }
                Err(_) => return None,
            }
        }
    })
    .await
    .unwrap_or(None)
}

fn parse_announcement(data: &[u8]) -> Option<u16> {
    let s = std::str::from_utf8(data).ok()?;
    let ad_start = s.find("[AD]")? + 4;
    let ad_end = s.find("[/AD]")?;
    s[ad_start..ad_end].parse().ok()
}
