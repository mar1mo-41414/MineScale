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
/// We deliberately constrain the announce to *the local machine only*:
///   - outgoing multicast interface = loopback (so source IP = 127.0.0.1)
///   - TTL = 0 (packet does not leave the host)
///   - multicast loop enabled (default) — local delivery still works
///
/// Two benefits:
///   1. Other devices on the same physical LAN don't see "MineScale
///      World" in their server list at all. This is a privacy property
///      — only the user running mc-share-gui:join is meant to see it.
///   2. Minecraft on the same machine connects to 127.0.0.1:port (since
///      that's the announce's source IP). The handshake packet carries
///      "127.0.0.1" as the server_address, which makes the host's
///      Minecraft treat the connection as a local LAN client (no online
///      auth) — so even offline-mode accounts can join the LAN entry
///      directly instead of going through "Add Server" with the
///      Direct address.
pub async fn announce_lan_world(motd: &str, port: u16) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(0)?;
    // Loopback as the outgoing multicast interface — source IP becomes
    // 127.0.0.1. tokio's UdpSocket doesn't expose set_multicast_if_v4
    // directly, so reach in via socket2.
    {
        use socket2::SockRef;
        if let Err(e) = SockRef::from(&socket).set_multicast_if_v4(&Ipv4Addr::LOCALHOST) {
            tracing::warn!(
                "LAN announce: could not bind multicast to loopback ({}); \
                 falling back — other devices on the LAN may also see the world.",
                e
            );
        }
    }

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
