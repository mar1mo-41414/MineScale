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
/// The announce is host-local-only by use of TTL = 0:
///   - packet is not forwarded out of the host
///   - multicast loopback (enabled by default) delivers it to local
///     listeners — including Minecraft running on the same machine
///
/// We do NOT set the outgoing interface to loopback. Earlier versions
/// tried that to make the source IP be 127.0.0.1 (so the LAN-entry
/// click would handshake with "127.0.0.1" and bypass host online-auth),
/// but on macOS the loopback multicast path was found to be unreliable
/// — the local Minecraft sometimes didn't receive the announce at all,
/// and the LAN entry would intermittently fail to appear.
///
/// Sending via the default interface with TTL = 0 reliably reaches the
/// local Minecraft on every supported platform while still preventing
/// other devices on the physical LAN from seeing the entry.
///
/// If the LAN-entry click still doesn't let an offline-mode account
/// join (host Minecraft sometimes requires online auth depending on
/// version / settings), the joiner can use Add Server with the Direct
/// address mc-share-gui displays — that path is always offline-friendly.
pub async fn announce_lan_world(motd: &str, port: u16) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(0)?;

    let announcement = format!("[MOTD]{motd}[/MOTD][AD]{port}[/AD]");
    let target = SocketAddrV4::new(MULTICAST_ADDR, MULTICAST_PORT);

    tracing::info!(
        "Announcing LAN world \"{}\" on multicast {}:{} (host-local, TTL=0)",
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
