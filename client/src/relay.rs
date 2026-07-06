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
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const AUTH_READ_TIMEOUT: Duration = Duration::from_secs(10);
// Aggressive keepalive so middlebox-dropped relay sockets are detected
// fast — Linux defaults are 2h, which is far too long for a parked stream
// to silently die without us noticing.
const KEEPALIVE_IDLE: Duration   = Duration::from_secs(45);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_RETRIES: u32      = 4;

/// Validate a relay endpoint string.  Accepts either a literal "host:port"
/// (DNS resolved at connect time) or "ip:port".  Returns the string for
/// later use by `dial_and_auth`.
pub fn parse_relay_addr(s: &str) -> Result<String> {
    if !s.contains(':') {
        return Err(anyhow!("relay address missing :port — {:?}", s));
    }
    Ok(s.to_string())
}

// ── Common: dial + auth ──────────────────────────────────────────────────────

fn tune(stream: &TcpStream) {
    use socket2::{SockRef, TcpKeepalive};
    // Disable Nagle so Minecraft's small packets aren't buffered.
    let _ = stream.set_nodelay(true);
    // Keep-alive: detect dead parked connections quickly.
    // `with_retries` is only available on Linux/macOS/BSD; Windows applies
    // a sensible default count once idle+interval are set.
    let mut ka = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL);
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd",
              target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly"))]
    {
        ka = ka.with_retries(KEEPALIVE_RETRIES);
    }
    let _ = SockRef::from(stream).set_tcp_keepalive(&ka);
}

async fn dial_and_auth(
    relay_addr: &str,
    room_id: &str,
    role: &str,
    token: &str,
) -> Result<TcpStream> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(relay_addr))
        .await
        .map_err(|_| anyhow!("relay connect timeout (addr={})", relay_addr))??;
    tune(&stream);

    let line = format!("RELAY {} {} {}\n", room_id, role, token);
    let mut stream = stream;
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
    relay_addr: String,
    room_id: String,
    token: String,
    mc_addr: std::net::SocketAddr,
    pool_size: usize,
    cancel: CancellationToken,
    on_first_pair: impl Fn() + Send + Sync + 'static,
) {
    let sem = Arc::new(Semaphore::new(pool_size));
    let on_first_pair = Arc::new(on_first_pair);

    // Shared backoff across the whole pool. Every successful park
    // resets it; every failure grows it. Prevents 4 concurrent
    // parkers from re-dialing 60×/sec against a broken relay.
    let backoff = Arc::new(std::sync::atomic::AtomicU64::new(0));
    const BACKOFF_INITIAL_MS: u64 = 500;
    const BACKOFF_MAX_MS: u64 = 30_000;

    loop {
        if cancel.is_cancelled() { break; }

        // Sleep off any pending backoff BEFORE grabbing a permit —
        // otherwise 4 permit-holders race through and each fail fast.
        let wait_ms = backoff.load(std::sync::atomic::Ordering::Relaxed);
        if wait_ms > 0 {
            let _ = tokio::time::timeout(
                Duration::from_millis(wait_ms),
                cancel.cancelled(),
            ).await;
            if cancel.is_cancelled() { break; }
        }

        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };

        let cancel2 = cancel.clone();
        let room_id = room_id.clone();
        let token = token.clone();
        let relay_addr = relay_addr.clone();
        let on_first_pair = on_first_pair.clone();
        let backoff2 = backoff.clone();

        tokio::spawn(async move {
            let permit = permit; // dropped on first activity or error
            match park_one(&relay_addr, &room_id, &token, cancel2.clone()).await {
                Ok(Some(stream)) => {
                    backoff2.store(0, std::sync::atomic::Ordering::Relaxed);
                    on_first_pair();
                    drop(permit);
                    let peer_label = stream
                        .peer_addr()
                        .map(|a| a.to_string())
                        .unwrap_or_else(|_| "?".into());
                    info!("relay: host-side session paired (relay peer={})", peer_label);
                    match host_pipe_to_mc(stream, mc_addr).await {
                        Ok(()) => info!("relay: host-side session ended cleanly"),
                        Err(e) => warn!("relay: host-side session ended: {}", e),
                    }
                }
                Ok(None) => { drop(permit); }
                Err(e) => {
                    drop(permit);
                    // Exponential backoff, shared across the pool. Only
                    // warn on the first failure of a burst so we don't
                    // spam the log during a long stretch of unreachability
                    // (e.g. relay server temporarily down, or room-gone
                    // races before the poller has told us to stop).
                    let prev = backoff2.load(std::sync::atomic::Ordering::Relaxed);
                    let next = if prev == 0 {
                        warn!("relay: park failed: {}", e);
                        BACKOFF_INITIAL_MS
                    } else {
                        (prev * 2).min(BACKOFF_MAX_MS)
                    };
                    backoff2.store(next, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
    }
}

/// Establish a parked connection. Returns `Some(stream)` once the peer
/// has paired and started sending bytes. Returns `None` if cancelled.
async fn park_one(
    relay_addr: &str,
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
async fn host_pipe_to_mc(
    relay_stream: TcpStream,
    mc_addr: std::net::SocketAddr,
) -> Result<()> {
    let started = std::time::Instant::now();
    let mc = TcpStream::connect(mc_addr).await
        .map_err(|e| anyhow!("MC connect at {} failed (is the LAN world still open?): {}", mc_addr, e))?;
    tune(&mc);
    let connect_ms = started.elapsed().as_millis();
    info!("relay: piping host-side session → Minecraft at {} (mc-connect={}ms)", mc_addr, connect_ms);

    let (mut r_r, mut r_w) = relay_stream.into_split();
    let (mut m_r, mut m_w) = mc.into_split();
    let r_to_m = tokio::io::copy(&mut r_r, &mut m_w);
    let m_to_r = tokio::io::copy(&mut m_r, &mut r_w);
    let (which, bytes) = tokio::select! {
        rv = r_to_m => ("relay→MC", rv.unwrap_or(0)),
        rv = m_to_r => ("MC→relay", rv.unwrap_or(0)),
    };
    let elapsed_ms = started.elapsed().as_millis();
    info!(
        "relay: host-side session ended after {}ms — {} reached EOF, total {} bytes one-way",
        elapsed_ms, which, bytes
    );
    Ok(())
}

// ── Join side: dial per Minecraft connection ─────────────────────────────────

/// Dial the relay as the join side and pipe the given Minecraft TCP stream
/// through it. Returns when either side disconnects.
pub async fn join_forward(
    mc_tcp: TcpStream,
    relay_addr: &str,
    room_id: &str,
    token: &str,
) -> Result<()> {
    let started = std::time::Instant::now();
    tune(&mc_tcp);
    let relay_stream = dial_and_auth(relay_addr, room_id, "join", token).await?;
    let dial_ms = started.elapsed().as_millis();
    info!("relay: join-side session opened (dial={}ms) — waiting for Minecraft traffic", dial_ms);

    let (mut mc_r, mut mc_w) = mc_tcp.into_split();
    let (mut r_r, mut r_w) = relay_stream.into_split();
    let mc_to_r = tokio::io::copy(&mut mc_r, &mut r_w);
    let r_to_mc = tokio::io::copy(&mut r_r, &mut mc_w);
    let (which, bytes) = tokio::select! {
        rv = mc_to_r => ("MC→relay", rv.unwrap_or(0)),
        rv = r_to_mc => ("relay→MC", rv.unwrap_or(0)),
    };
    let elapsed_ms = started.elapsed().as_millis();
    info!(
        "relay: join-side session closed after {}ms — {} reached EOF, total {} bytes one-way",
        elapsed_ms, which, bytes
    );
    Ok(())
}
