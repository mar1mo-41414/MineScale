//! TCP relay client.
//!
//! Used as a fallback when P2P (QUIC over UDP hole-punching) cannot
//! be established — typically because the joiner sits behind Symmetric
//! NAT, where the host cannot predict the joiner's outbound port.
//!
//! Protocol matches `server/src/relay.rs`:
//!   Client → Server: "RELAY {room_id} {role:host|join} {relay_token}\n"
//!   Server → Client: "OK\n"
//!   Then: raw TCP pipe to the matched peer.

use anyhow::{anyhow, Result};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const AUTH_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve a relay endpoint string like "1.2.3.4:9090" to a SocketAddr.
pub fn parse_relay_addr(s: &str) -> Result<SocketAddr> {
    s.parse::<SocketAddr>()
        .map_err(|e| anyhow!("invalid relay address {:?}: {}", s, e))
}

// ── Common: dial + auth ──────────────────────────────────────────────────────

async fn dial_and_auth(
    relay_addr: SocketAddr,
    room_id: &str,
    role: &str,
    token: &str,
) -> Result<TcpStream> {
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(relay_addr))
        .await
        .map_err(|_| anyhow!("relay connect timeout"))??;
    stream.set_nodelay(true).ok();

    let line = format!("RELAY {} {} {}\n", room_id, role, token);
    stream.write_all(line.as_bytes()).await?;
    stream.flush().await?;

    // Read "OK\n" (3 bytes) or an "ERROR …" line.
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(AUTH_READ_TIMEOUT, stream.read(&mut buf))
        .await
        .map_err(|_| anyhow!("relay auth read timeout"))??;
    if n == 0 {
        return Err(anyhow!("relay closed before auth response"));
    }
    let resp = String::from_utf8_lossy(&buf[..n]).trim().to_string();
    if !resp.starts_with("OK") {
        return Err(anyhow!("relay auth failed: {}", resp));
    }
    Ok(stream)
}

// ── Host side: maintain a pool of parked connections ─────────────────────────

/// Maintain `pool_size` parked relay connections at all times.
///
/// Whenever a parked connection is consumed by a joiner (signalled by
/// first byte of activity), it transitions to a pipe to the local
/// Minecraft server, and a fresh connection is parked to replace it.
///
/// `on_first_pair` is called exactly once, the first time a parked
/// connection is successfully consumed by a peer. Used by telemetry
/// to fire the `connected` checkpoint.
pub async fn host_pool(
    relay_addr: SocketAddr,
    room_id: String,
    token: String,
    mc_addr: SocketAddr,
    pool_size: usize,
    cancel: CancellationToken,
    on_first_pair: impl Fn() + Send + Sync + 'static,
) {
    let sem = Arc::new(Semaphore::new(pool_size));
    let on_first_pair = Arc::new(on_first_pair);

    loop {
        if cancel.is_cancelled() {
            break;
        }
        // Block until a park slot is free.
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break, // semaphore closed
        };

        let cancel2 = cancel.clone();
        let room_id = room_id.clone();
        let token = token.clone();
        let on_first_pair = on_first_pair.clone();

        tokio::spawn(async move {
            let permit = permit; // moved here; dropped on first activity or error
            match park_one(relay_addr, &room_id, &token, cancel2.clone()).await {
                Ok(Some(stream)) => {
                    on_first_pair();
                    drop(permit); // free slot now that we transitioned to active
                    if let Err(e) = host_pipe_to_mc(stream, mc_addr).await {
                        debug!("relay host pipe ended: {}", e);
                    }
                }
                Ok(None) => {
                    drop(permit);
                }
                Err(e) => {
                    drop(permit);
                    debug!("relay park error: {}", e);
                    // Soft backoff so we don't hammer a broken relay.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        });
    }
}

/// Establish a parked connection. Returns `Some(stream)` once the peer
/// has paired and started sending bytes. Returns `None` if cancelled.
async fn park_one(
    relay_addr: SocketAddr,
    room_id: &str,
    token: &str,
    cancel: CancellationToken,
) -> Result<Option<TcpStream>> {
    let stream = dial_and_auth(relay_addr, room_id, "host", token).await?;
    debug!("host relay parked");

    // Wait for the first byte of activity, which means the joiner paired.
    // We can't read it (it must go to Minecraft) — peek instead.
    let mut peek = [0u8; 1];
    tokio::select! {
        r = stream.peek(&mut peek) => {
            let n = r?;
            if n == 0 { return Err(anyhow!("relay closed before pair")); }
            Ok(Some(stream))
        }
        _ = cancel.cancelled() => Ok(None),
    }
}

/// Pipe a paired relay stream to the local Minecraft server.
async fn host_pipe_to_mc(relay_stream: TcpStream, mc_addr: SocketAddr) -> Result<()> {
    let mc = TcpStream::connect(mc_addr).await?;
    info!("Relay session active → Minecraft at {}", mc_addr);
    let (mut r_r, mut r_w) = relay_stream.into_split();
    let (mut m_r, mut m_w) = mc.into_split();
    let r_to_m = tokio::io::copy(&mut r_r, &mut m_w);
    let m_to_r = tokio::io::copy(&mut m_r, &mut r_w);
    tokio::select! {
        _ = r_to_m => {}
        _ = m_to_r => {}
    }
    Ok(())
}

// ── Join side: dial per Minecraft connection ─────────────────────────────────

/// Dial the relay as the join side and pipe the given Minecraft TCP stream
/// through it. Returns when either side disconnects.
pub async fn join_forward(
    mc_tcp: TcpStream,
    relay_addr: SocketAddr,
    room_id: &str,
    token: &str,
) -> Result<()> {
    let relay_stream = dial_and_auth(relay_addr, room_id, "join", token).await?;
    info!("Relay session active for Minecraft client");
    let (mut mc_r, mut mc_w) = mc_tcp.into_split();
    let (mut r_r, mut r_w) = relay_stream.into_split();
    let mc_to_r = tokio::io::copy(&mut mc_r, &mut r_w);
    let r_to_mc = tokio::io::copy(&mut r_r, &mut mc_w);
    tokio::select! {
        r = mc_to_r => { let _ = r; }
        r = r_to_mc => { let _ = r; }
    }
    Ok(())
}

// ── Convenience: log a one-shot warning when relay isn't reachable ───────────

#[allow(dead_code)]
pub fn warn_unreachable(relay_addr: SocketAddr, err: &str) {
    warn!("relay {} unreachable: {}", relay_addr, err);
}
