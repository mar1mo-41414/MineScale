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
///   - v1.2.6 used TTL=1 on the kernel-default interface. Reliable
///     on a vanilla home network, but LAN-mates also see the entry.
///   - v1.2.7 routed via the loopback interface to force source IP =
///     127.0.0.1. macOS lost the local-delivery path.
///   - v1.2.8 tried default interface with TTL=0. macOS lost it again
///     when an active VPN tunnel claims part of 224.0.0.0/4 in the
///     kernel route table — packets vanish into the tunnel and never
///     reach the local Minecraft.
///   - v1.2.10 went back to TTL=1 on the kernel-default interface.
///     Still broken on Mac+VPN, since the *kernel default* is the
///     wrong choice when a tunnel is hijacking the multicast route.
///   - v1.2.11 (this): enumerate every local IPv4 interface and send
///     the announce on each one *explicitly*. Whichever interface
///     Minecraft's MulticastSocket is reading from receives a copy.
///     Works regardless of whatever the route table is doing.
pub async fn announce_lan_world(motd: &str, port: u16) -> Result<()> {
    let announcement = format!("[MOTD]{motd}[/MOTD][AD]{port}[/AD]");
    let target = SocketAddrV4::new(MULTICAST_ADDR, MULTICAST_PORT);

    // Build one socket per local IPv4 interface, each pinned to send
    // outgoing multicast via that interface (IP_MULTICAST_IF).
    let mut sockets: Vec<(Ipv4Addr, UdpSocket)> = Vec::new();
    let iface_ips: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .map(|list| {
            list.into_iter()
                .filter_map(|i| match i.addr {
                    if_addrs::IfAddr::V4(a) => Some(a.ip),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    for ip in &iface_ips {
        match build_announce_socket(*ip).await {
            Ok(s) => sockets.push((*ip, s)),
            Err(e) => tracing::debug!("LAN announce: skip {} ({})", ip, e),
        }
    }

    // Fall back to a default socket if enumeration failed entirely.
    if sockets.is_empty() {
        let s = UdpSocket::bind("0.0.0.0:0").await?;
        s.set_multicast_loop_v4(true)?;
        s.set_multicast_ttl_v4(1)?;
        sockets.push((Ipv4Addr::UNSPECIFIED, s));
    }

    tracing::info!(
        "Announcing LAN world \"{}\" on multicast {}:{} via {} interface(s): {}",
        motd,
        MULTICAST_ADDR,
        MULTICAST_PORT,
        sockets.len(),
        sockets
            .iter()
            .map(|(ip, _)| ip.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    );

    loop {
        for (ip, sock) in &sockets {
            if let Err(e) = sock.send_to(announcement.as_bytes(), target).await {
                tracing::debug!("LAN announce send failed via {}: {}", ip, e);
            }
        }
        tokio::time::sleep(ANNOUNCE_INTERVAL).await;
    }
}

async fn build_announce_socket(iface_ip: Ipv4Addr) -> Result<UdpSocket> {
    let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?;
    sock.set_multicast_loop_v4(true)?;
    sock.set_multicast_ttl_v4(1)?;
    socket2::SockRef::from(&sock).set_multicast_if_v4(&iface_ip)?;
    Ok(sock)
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
