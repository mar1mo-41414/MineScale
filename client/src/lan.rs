use anyhow::Result;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;

const MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
const MULTICAST_PORT: u16 = 4445;
/// Minecraft expects announcements roughly every second.
const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1500);

/// Broadcast a fake LAN world so Minecraft's multiplayer screen shows it.
///
/// By default the announce stays **on this machine**: it is sent on every
/// local IPv4 interface (IP_MULTICAST_IF set explicitly) with TTL = 0, so it
/// is delivered to local listeners through multicast loop but never reaches
/// other devices on the LAN. Whichever interface Minecraft is listening on
/// gets a copy, independent of what the route table (VPN tunnels!) says.
///
/// History, so we don't re-learn it:
///   - TTL=0 on the *kernel-default* interface, and loopback-only sending,
///     were both unreliable on macOS (a VPN tunnel can own 224.0.0.0/4).
///   - Sending on every interface with TTL=1 (v1.2.11) was reliable but also
///     showed the entry to everyone on the LAN.
///   - Every interface + explicit IF + TTL=0 is reliable on macOS and Linux
///     (measured), and keeps the entry off the LAN.
///
/// Windows has not been measured, so before relying on TTL=0 we check that
/// our own packet actually comes back locally; if it doesn't we fall back
/// to TTL=1 (visible on the LAN) rather than risk the entry not showing up.
///
/// `allow_lan = true` forces the old TTL=1 behaviour.
pub async fn announce_lan_world(motd: &str, port: u16, allow_lan: bool) -> Result<()> {
    let announcement = format!("[MOTD]{motd}[/MOTD][AD]{port}[/AD]");
    let target = SocketAddrV4::new(MULTICAST_ADDR, MULTICAST_PORT);

    let iface_ips = local_ipv4_addrs();

    let ttl: u32 = if allow_lan {
        1
    } else if local_delivery_works(&iface_ips).await {
        0
    } else {
        tracing::warn!(
            "LAN announce: could not confirm host-local multicast delivery on this system;              falling back to TTL=1 (other devices on the network may see the entry)."
        );
        1
    };

    let mut sockets: Vec<(Ipv4Addr, UdpSocket)> = Vec::new();
    for ip in &iface_ips {
        match build_announce_socket(*ip, ttl).await {
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
        "Announcing LAN world \"{}\" on multicast {}:{} via {} interface(s) — {}",
        motd,
        MULTICAST_ADDR,
        MULTICAST_PORT,
        sockets.len(),
        if ttl == 0 { "this computer only" } else { "visible on the local network" },
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

/// Every local IPv4 address (loopback included), link-local excluded.
pub fn local_ipv4_addrs() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .map(|list| {
            list.into_iter()
                .filter_map(|i| match i.addr {
                    if_addrs::IfAddr::V4(a) if !a.ip.is_link_local() => Some(a.ip),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Is `ip` this very computer (loopback, or the address of one of our own
/// interfaces)? A connection from the machine itself to its own LAN address
/// shows up with that LAN address as its source.
pub fn is_this_machine(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || local_ipv4_addrs().contains(&v4),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

async fn build_announce_socket(iface_ip: Ipv4Addr, ttl: u32) -> Result<UdpSocket> {
    let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?;
    sock.set_multicast_loop_v4(true)?;
    sock.set_multicast_ttl_v4(ttl)?;
    socket2::SockRef::from(&sock).set_multicast_if_v4(&iface_ip)?;
    Ok(sock)
}

/// A listener shaped like Minecraft's: port 4445, address reuse on, joined
/// to the group on every local interface.
fn bind_probe_listener(ips: &[Ipv4Addr]) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    sock.set_reuse_port(true)?;
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MULTICAST_PORT).into())?;
    sock.set_nonblocking(true)?;
    for ip in ips {
        let _ = sock.join_multicast_v4(&MULTICAST_ADDR, ip);
    }
    Ok(UdpSocket::from_std(sock.into())?)
}

/// Does a TTL=0 multicast packet sent on our interfaces come back to a
/// local listener? (If it does, Minecraft on this machine will see it too.)
async fn local_delivery_works(ips: &[Ipv4Addr]) -> bool {
    let Ok(listener) = bind_probe_listener(ips) else { return false };
    let probe = format!("[MSCALE-PROBE]{:016x}", rand::random::<u64>());
    let target = SocketAddrV4::new(MULTICAST_ADDR, MULTICAST_PORT);

    for ip in ips {
        if let Ok(s) = build_announce_socket(*ip, 0).await {
            let _ = s.send_to(probe.as_bytes(), target).await;
        }
    }

    let mut buf = [0u8; 128];
    tokio::time::timeout(Duration::from_millis(1500), async {
        loop {
            match listener.recv_from(&mut buf).await {
                Ok((n, _)) if &buf[..n] == probe.as_bytes() => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_this_machine() {
        assert!(is_this_machine("127.0.0.1".parse().unwrap()));
        assert!(is_this_machine("::1".parse().unwrap()));
    }

    #[test]
    fn foreign_address_is_not_this_machine() {
        assert!(!is_this_machine("203.0.113.7".parse().unwrap()));
    }

    #[test]
    fn own_interface_address_is_this_machine() {
        for ip in local_ipv4_addrs() {
            assert!(is_this_machine(std::net::IpAddr::V4(ip)));
        }
    }

    #[test]
    fn parses_announcement() {
        assert_eq!(parse_announcement(b"[MOTD]x[/MOTD][AD]25565[/AD]"), Some(25565));
        assert_eq!(parse_announcement(b"[MSCALE-PROBE]00"), None);
    }
}
