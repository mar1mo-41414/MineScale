//! TCP relay fallback for when P2P hole-punching fails.
//!
//! Protocol (after TCP connect):
//!   Client → Server: "RELAY {room_id} {role:host|join} {relay_token}\n"
//!   Server → Client: "OK\n"  or  "ERROR {reason}\n"
//!   After OK: raw TCP bytes piped to the matched peer.
//!
//! Pairing rule:
//!   - The first arriver of a (room_id, role) is parked.
//!   - The next arriver of the opposite role pops the parked stream
//!     and the two are spliced together.
//!   - Multiple host parks are queued in FIFO order to support several
//!     concurrent joiners.
//!
//! The first 3 bytes from the join side are inspected to gate on a
//! plausible Minecraft Java handshake (packet id 0x00) before relaying.

use crate::rooms::Registry;
use anyhow::{anyhow, Result};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};
use tracing::{info, warn};

const RELAY_AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const RELAY_PARK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_AUTH_LINE: usize = 256;
const MC_HANDSHAKE_PACKET_ID: u8 = 0x00;

// Each parked stream carries a unique id so the per-stream timeout
// task can remove that *specific* stream — popping from the front of
// the queue (as we used to) would mistakenly drop a still-valid
// later-arriving stream once the queue had rotated.
type Parkers = Arc<Mutex<HashMap<String, VecDeque<(u64, TcpStream)>>>>;

pub async fn run_relay(listener: TcpListener, registry: Registry) {
    info!("Relay server listening on {}", listener.local_addr().unwrap());
    let parkers: Parkers = Arc::new(Mutex::new(HashMap::new()));

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                tune(&stream);
                let registry = registry.clone();
                let parkers = Arc::clone(&parkers);
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, registry, parkers).await {
                        warn!("relay {} error: {}", addr, e);
                    }
                });
            }
            Err(e) => warn!("relay accept error: {}", e),
        }
    }
}

/// Tune an accepted TCP stream:
///   - TCP_NODELAY so Minecraft's small packets aren't Nagle-buffered
///   - TCP keepalive so middlebox-dropped parked sockets are reaped
///     within ~2 minutes instead of the 2-hour kernel default
fn tune(stream: &TcpStream) {
    use socket2::{SockRef, TcpKeepalive};
    let _ = stream.set_nodelay(true);
    let ka = TcpKeepalive::new()
        .with_time(Duration::from_secs(45))
        .with_interval(Duration::from_secs(15))
        .with_retries(4);
    let _ = SockRef::from(stream).set_tcp_keepalive(&ka);
}

async fn handle(mut stream: TcpStream, registry: Registry, parkers: Parkers) -> Result<()> {
    // ── Auth ─────────────────────────────────────────────────────────────────
    let line = tokio::time::timeout(RELAY_AUTH_TIMEOUT, read_line(&mut stream))
        .await
        .map_err(|_| anyhow!("auth timeout"))??;

    let parts: Vec<&str> = line.trim().split_whitespace().collect();
    if parts.len() != 4 || parts[0] != "RELAY" {
        let _ = stream.write_all(b"ERROR bad auth\n").await;
        return Err(anyhow!("bad auth format"));
    }
    let (room_id, role, token) = (parts[1].to_string(), parts[2].to_string(), parts[3]);
    if role != "host" && role != "join" {
        let _ = stream.write_all(b"ERROR invalid role\n").await;
        return Err(anyhow!("invalid role"));
    }
    let room = registry
        .get(&room_id)
        .ok_or_else(|| anyhow!("room not found"))?;
    if room.relay_token != token {
        let _ = stream.write_all(b"ERROR invalid token\n").await;
        return Err(anyhow!("invalid relay token"));
    }
    stream.write_all(b"OK\n").await?;
    stream.flush().await?;

    // ── Pair ─────────────────────────────────────────────────────────────────
    let opposite = if role == "host" { "join" } else { "host" };
    let opposite_key = format!("{}:{}", room_id, opposite);
    let my_key = format!("{}:{}", room_id, role);

    // Try to pop a parked partner first (oldest first — FIFO).
    let partner = {
        let mut map = parkers.lock().await;
        map.get_mut(&opposite_key)
            .and_then(|q| q.pop_front())
            .map(|(_id, s)| s)
    };

    if let Some(partner) = partner {
        // We are the second arriver — pair now.
        info!("relay paired room={} ({} arrived second)", room_id, role);
        let (host_stream, join_stream) = if role == "host" {
            (stream, partner)
        } else {
            (partner, stream)
        };
        return pipe_host_join(host_stream, join_stream).await;
    }

    // We are first — park ourselves until the partner arrives or we time out.
    let park_id: u64 = rand::random();
    {
        let mut map = parkers.lock().await;
        map.entry(my_key.clone()).or_default().push_back((park_id, stream));
    }
    info!("relay parked room={} role={} id={:016x}", room_id, role, park_id);

    // Schedule a cleanup so a never-paired stream is dropped eventually.
    // We remove by id, not by position — popping from the front is wrong
    // once other parks have arrived after us.
    let parkers2 = parkers.clone();
    let my_key2 = my_key.clone();
    tokio::spawn(async move {
        tokio::time::sleep(RELAY_PARK_TIMEOUT).await;
        let mut map = parkers2.lock().await;
        if let Some(q) = map.get_mut(&my_key2) {
            let before = q.len();
            q.retain(|(id, _)| *id != park_id);
            if q.len() < before {
                warn!("relay park timeout: {} id={:016x}", my_key2, park_id);
            }
            if q.is_empty() {
                map.remove(&my_key2);
            }
        }
    });
    Ok(())
}

async fn read_line(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::with_capacity(64);
    let mut b = [0u8; 1];
    loop {
        let n = stream.read(&mut b).await?;
        if n == 0 {
            return Err(anyhow!("eof during auth"));
        }
        if b[0] == b'\n' {
            break;
        }
        if buf.len() >= MAX_AUTH_LINE {
            return Err(anyhow!("auth line too long"));
        }
        buf.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

async fn pipe_host_join(host: TcpStream, join: TcpStream) -> Result<()> {
    let (host_r, mut host_w) = host.into_split();
    let (mut join_r, join_w) = join.into_split();

    // Sniff the first 3 bytes from the join side and validate that they
    // look like a Minecraft Java handshake. Forward them along untouched.
    let mut hdr = [0u8; 3];
    let mut got = 0;
    while got < hdr.len() {
        let n = join_r.read(&mut hdr[got..]).await?;
        if n == 0 {
            return Err(anyhow!("join closed before handshake"));
        }
        got += n;
    }
    if hdr[1] != MC_HANDSHAKE_PACKET_ID {
        return Err(anyhow!(
            "relay rejected non-MC traffic (pkt id 0x{:02x})",
            hdr[1]
        ));
    }
    host_w.write_all(&hdr).await?;

    let mut host_r = host_r;
    let mut join_w = join_w;
    let j_to_h = tokio::io::copy(&mut join_r, &mut host_w);
    let h_to_j = tokio::io::copy(&mut host_r, &mut join_w);
    tokio::select! {
        r = j_to_h => { let _ = r; }
        r = h_to_j => { let _ = r; }
    }
    Ok(())
}
