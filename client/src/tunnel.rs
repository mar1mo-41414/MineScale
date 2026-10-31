//! QUIC-based encrypted P2P tunnel.
//!
//! Host side  → quinn QUIC server, one bidirectional stream per Minecraft TCP connection.
//! Joiner side → quinn QUIC client, opens a stream for each local TCP connection.
//!
//! Hole punching is done on the same UDP socket before QUIC takes ownership.

use anyhow::{anyhow, Result};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::UdpSocket;

use crate::transport::Transport;
use tracing::{debug, info, warn};

// ── Hole punching ─────────────────────────────────────────────────────────────

const PROBE_MAGIC: &[u8] = b"MCS\x01";
const HOLE_PUNCH_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_INTERVAL: Duration = Duration::from_millis(100);

// After receiving the first probe, keep sending for this long before handing
// off to QUIC.  Ensures the remote side receives our probes too.
const GRACE_AFTER_RECEIVE: Duration = Duration::from_millis(2500);

// If no probe is received within this window, proceed anyway.
// Handles 2nd+ joiners where the host is already in QUIC mode and won't
// reply with raw probes — the joiner still needs to open its own NAT hole.
// 5 s gives the host's QUIC-poke task (≈2 s) time to complete before we
// attempt the QUIC connect, opening the host's NAT first.
const BEST_EFFORT_SEND: Duration = Duration::from_secs(5);

/// A UDP error that says nothing about *our* socket being broken.
///
/// When we send to a peer whose NAT answers "port unreachable" (exactly what
/// happens to the first hole-punch probes, which arrive before the peer's own
/// outbound packet has opened its NAT), Windows reports it on the **next
/// `recvfrom` of that socket** as `WSAECONNRESET`. Treating it as fatal ended
/// the whole host session right after the first joiner appeared. Unix reports
/// the equivalent as `ConnectionRefused` on connected sockets. Both mean
/// "that probe went nowhere" — carry on.
pub fn is_transient_udp_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::Interrupted
    )
}

/// UDP hole punching.
///
/// - Normal path: receives a probe back, then sends for GRACE_AFTER_RECEIVE.
/// - Best-effort path: after BEST_EFFORT_SEND without any response, proceeds
///   anyway so that 2nd+ joiners (whose host is in QUIC mode) still work.
pub async fn punch_holes(socket: &UdpSocket, peer: SocketAddr) -> Result<()> {
    info!("Hole punching to {}…", peer);

    let start = tokio::time::Instant::now();
    let mut probe_interval = tokio::time::interval(PROBE_INTERVAL);
    probe_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut recv_buf = [0u8; 64];
    let mut grace_deadline: Option<tokio::time::Instant> = None;

    tokio::time::timeout(HOLE_PUNCH_TIMEOUT, async {
        loop {
            tokio::select! {
                result = socket.recv_from(&mut recv_buf) => {
                    match result {
                        Ok((len, src))
                            if src == peer
                                && len >= PROBE_MAGIC.len()
                                && recv_buf[..PROBE_MAGIC.len()] == *PROBE_MAGIC =>
                        {
                            if grace_deadline.is_none() {
                                info!("Hole punched! Got probe from {} — entering grace period", src);
                                grace_deadline = Some(tokio::time::Instant::now() + GRACE_AFTER_RECEIVE);
                            }
                        }
                        Ok(_) => {}
                        Err(e) if is_transient_udp_error(&e) => {
                            debug!("ignoring transient UDP error while punching: {}", e);
                        }
                        Err(e) => return Err(anyhow!(e)),
                    }
                }
                _ = probe_interval.tick() => {
                    let _ = socket.send_to(PROBE_MAGIC, peer).await;
                    let now = tokio::time::Instant::now();
                    if let Some(dl) = grace_deadline {
                        if now >= dl {
                            info!("Grace period complete — proceeding to QUIC");
                            return Ok(());
                        }
                    } else if now.duration_since(start) >= BEST_EFFORT_SEND {
                        // No response yet but we've been sending long enough to
                        // open our NAT — proceed optimistically.
                        // (2nd+ joiners: host is already in QUIC mode and won't
                        //  reply to raw probes, so this path is normal.)
                        info!("No probe received — proceeding in best-effort mode");
                        return Ok(());
                    }
                }
            }
        }
    })
    .await
    .map_err(|_| anyhow!("Hole punching timed out after {}s", HOLE_PUNCH_TIMEOUT.as_secs()))?
}

/// Poke a new joiner *from the QUIC endpoint* to open the host's NAT.
///
/// warm_up_hole (old approach) used a temporary socket on a random port.
/// For Port-Restricted Cone NAT, only packets from the QUIC port itself
/// (e.g. 33574) open the NAT entry for inbound QUIC from the joiner.
/// This function is called from inside `run_host` which has access to the
/// endpoint; it tries an outbound QUIC connection to the joiner address —
/// the INITIAL packets come from the QUIC port, opening the correct NAT
/// mapping even if the joiner isn't running a QUIC server.
async fn poke_joiner_from_quic(endpoint: quinn::Endpoint, joiner_addr: SocketAddr) {
    info!("Poking joiner {} from QUIC port to open NAT…", joiner_addr);
    for _ in 0..15 {
        if let Ok(connecting) = endpoint.connect(joiner_addr, "minescale") {
            // We only need the INITIAL packets to be sent; ignore the result.
            let _ = tokio::time::timeout(Duration::from_millis(150), connecting).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    debug!("QUIC poke done for {}", joiner_addr);
}

// ── QUIC transport settings ───────────────────────────────────────────────────

/// Ping the peer this often when otherwise idle.
///
/// Without it quinn never sends anything on an idle connection, so (a) the
/// idle timeout below closes it — a player sitting on the "connect" screen
/// for 30 s came back to `Join stream error: timed out` — and (b) the NAT's
/// UDP mapping, which is typically forgotten after 30-60 s of silence, closes
/// and the peer becomes unreachable.
const QUIC_KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Close the connection if the peer has been silent this long. With the
/// keep-alive above this only triggers when the peer is really gone.
const QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

fn transport_config(idle: Duration, keep_alive: Option<Duration>) -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(idle).expect("idle timeout fits in a QUIC varint"),
    ));
    t.keep_alive_interval(keep_alive);
    Arc::new(t)
}

fn default_transport() -> Arc<quinn::TransportConfig> {
    transport_config(QUIC_IDLE_TIMEOUT, Some(QUIC_KEEP_ALIVE))
}

// ── QUIC endpoint builders ────────────────────────────────────────────────────

fn build_server_endpoint(
    std_socket: std::net::UdpSocket,
    cert_key: &rcgen::CertifiedKey,
    transport: Arc<quinn::TransportConfig>,
) -> Result<quinn::Endpoint> {
    use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    let cert_der: CertificateDer = cert_key.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(cert_key.key_pair.serialize_der());

    let mut server_tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], PrivateKeyDer::Pkcs8(key_der))
        .map_err(|e| anyhow!("TLS server config: {}", e))?;
    server_tls.alpn_protocols = vec![b"minescale-1".to_vec()];

    let quic_server = QuicServerConfig::try_from(server_tls)
        .map_err(|e| anyhow!("QuicServerConfig: {}", e))?;

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server));
    server_config.transport_config(transport);

    let mut endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        std_socket,
        Arc::new(quinn::TokioRuntime),
    )?;

    // ── NAT-poke client config ───────────────────────────────────────────────
    // The host occasionally calls endpoint.connect() to send INITIAL packets
    // outbound to a new joiner's address, opening its own Port-Restricted Cone
    // NAT entry for that joiner. Quinn refuses connect() without a default
    // client config — install a permissive one (handshake will fail by design
    // since the joiner is a QUIC server, not a client, but the INITIAL packet
    // *will* leave our NAT, which is the whole point).
    let mut poke_tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    poke_tls.alpn_protocols = vec![b"minescale-1".to_vec()];
    let poke_client = QuicClientConfig::try_from(poke_tls)
        .map_err(|e| anyhow!("poke QuicClientConfig: {}", e))?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(poke_client)));

    Ok(endpoint)
}

/// Certificate verifier used ONLY for the NAT-poke client config in
/// `build_server_endpoint`. We never reach the cert-validation phase of the
/// handshake — the poke just needs the INITIAL packet to leave our NAT —
/// so this verifier's leniency has no security impact.
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn build_client_endpoint(
    std_socket: std::net::UdpSocket,
    expected_fingerprint: Vec<u8>,
    transport: Arc<quinn::TransportConfig>,
) -> Result<quinn::Endpoint> {
    use quinn::crypto::rustls::QuicClientConfig;

    let verifier = Arc::new(PinnedCertVerifier(expected_fingerprint));
    let mut client_tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![b"minescale-1".to_vec()];

    let quic_client = QuicClientConfig::try_from(client_tls)
        .map_err(|e| anyhow!("QuicClientConfig: {}", e))?;

    let mut endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        std_socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_client));
    client_config.transport_config(transport);
    endpoint.set_default_client_config(client_config);

    Ok(endpoint)
}

// ── Certificate pinning verifier ─────────────────────────────────────────────

#[derive(Debug)]
struct PinnedCertVerifier(Vec<u8>);

impl rustls::client::danger::ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use sha2::{Digest, Sha256};
        let fp: Vec<u8> = Sha256::digest(end_entity.as_ref()).to_vec();
        if fp == self.0 {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate fingerprint mismatch".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ── Host tunnel ───────────────────────────────────────────────────────────────

/// Run the host side of the tunnel.
///
/// Accepts an *unlimited* number of QUIC connections (one per joiner).
/// New joiner addresses received on `new_joiners_rx` are poked from the
/// QUIC endpoint to open the host's NAT before the joiner attempts to connect.
///
/// `on_first_connect` fires exactly once when the first joiner completes a
/// QUIC handshake. The host telemetry reporter uses this to record the
/// `Connected` checkpoint at the moment the tunnel is actually usable,
/// rather than optimistically after `punch_holes` returns.
pub async fn run_host(
    socket: UdpSocket,
    cert_key: rcgen::CertifiedKey,
    mc_addr: SocketAddr,
    mut new_joiners_rx: tokio::sync::mpsc::UnboundedReceiver<SocketAddr>,
    on_first_connect: Option<Box<dyn Fn() + Send + Sync>>,
) -> Result<()> {
    let std_socket = socket.into_std()?;
    let endpoint = build_server_endpoint(std_socket, &cert_key, default_transport())?;
    info!("QUIC server ready — accepting connections");

    let on_first_connect = on_first_connect.map(Arc::new);

    loop {
        tokio::select! {
            // ── Accept new QUIC connection from any joiner ────────────────────
            incoming = endpoint.accept() => {
                let connecting = incoming
                    .ok_or_else(|| anyhow!("QUIC endpoint closed"))?;
                let cb = on_first_connect.clone();
                tokio::spawn(async move {
                    match connecting.await {
                        Ok(conn) => {
                            info!("Joiner connected via QUIC from {}", conn.remote_address());
                            if let Some(cb) = cb { cb(); }
                            // Handle all Minecraft streams on this connection.
                            loop {
                                match conn.accept_bi().await {
                                    Ok(streams) => {
                                        tokio::spawn(async move {
                                            if let Err(e) = forward_to_minecraft(streams, mc_addr).await {
                                                warn!("Stream error: {}", e);
                                            }
                                        });
                                    }
                                    Err(quinn::ConnectionError::ApplicationClosed(_)) => {
                                        info!("Joiner {} disconnected", conn.remote_address());
                                        break;
                                    }
                                    Err(e) => {
                                        warn!("Connection error: {}", e);
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("Incoming QUIC handshake failed: {}", e),
                    }
                });
            }

            // ── Poke new joiner from the QUIC port to open host NAT ───────────
            Some(joiner_addr) = new_joiners_rx.recv() => {
                tokio::spawn(poke_joiner_from_quic(endpoint.clone(), joiner_addr));
            }
        }
    }
}

async fn forward_to_minecraft(
    (mut quic_send, mut quic_recv): (quinn::SendStream, quinn::RecvStream),
    mc_addr: SocketAddr,
) -> Result<()> {
    let mc = tokio::net::TcpStream::connect(mc_addr).await?;
    let (mut mc_r, mut mc_w) = mc.into_split();
    let a = tokio::io::copy(&mut quic_recv, &mut mc_w);
    let b = tokio::io::copy(&mut mc_r, &mut quic_send);
    tokio::select! {
        r = a => { r?; }
        r = b => { r?; }
    }
    Ok(())
}

// ── Joiner tunnel ─────────────────────────────────────────────────────────────

/// Fallback parameters when QUIC fails on the join side.
#[derive(Clone)]
pub struct RelayFallback {
    /// Relay endpoint as a "host:port" string. Resolved at connect time so
    /// DNS changes / IPv6 fallback work without restarting.
    pub addr: String,
    pub room_id: String,
    pub token: String,
}

/// Run the joiner side of the tunnel.
///
/// Tries QUIC first. If QUIC handshake fails (most commonly because the
/// joiner sits behind Symmetric NAT and hole punching couldn't predict
/// the remote port), falls back to the TCP relay when `relay_fallback`
/// is supplied.
///
/// `on_connected` fires once when the local TCP listener is bound AND a
/// usable transport (QUIC or relay) is selected — i.e. when it is safe
/// for Minecraft to connect. It receives the bound port and the chosen
/// transport.
pub async fn run_join(
    socket: UdpSocket,
    host_addr: Option<SocketAddr>,
    cert_fingerprint: Vec<u8>,
    local_addr: SocketAddr,
    relay_fallback: Option<RelayFallback>,
    allow_lan: bool,
    on_connected: Option<Box<dyn FnOnce(u16, Transport) + Send>>,
    on_path_change: Option<Arc<dyn Fn(Transport) + Send + Sync>>,
) -> Result<()> {
    const QUIC_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

    // No direct address (STUN failed on either side, e.g. UDP blocked) means
    // there is nothing to hole-punch to: go straight to the relay.
    let endpoint = match host_addr {
        Some(_) => Some(build_client_endpoint(socket.into_std()?, cert_fingerprint, default_transport())?),
        None => {
            info!("No direct address available — skipping QUIC, using the relay");
            None
        }
    };

    let quic_conn = match (&endpoint, host_addr) {
        (Some(endpoint), Some(host_addr)) => {
            info!("Connecting to host via QUIC at {} (timeout {}s)…", host_addr, QUIC_CONNECT_TIMEOUT.as_secs());
            match endpoint.connect(host_addr, "minescale") {
                Ok(connecting) => {
                    match tokio::time::timeout(QUIC_CONNECT_TIMEOUT, connecting).await {
                        Ok(Ok(c)) => {
                            info!("P2P tunnel established (QUIC)");
                            Some(c)
                        }
                        Ok(Err(e)) => {
                            warn!("QUIC handshake failed: {}", e);
                            None
                        }
                        Err(_) => {
                            warn!("QUIC handshake timed out after {}s", QUIC_CONNECT_TIMEOUT.as_secs());
                            None
                        }
                    }
                }
                Err(e) => {
                    warn!("QUIC connect setup failed: {}", e);
                    None
                }
            }
        }
        _ => None,
    };

    let mode = match (&quic_conn, &relay_fallback) {
        (Some(_), _) => Transport::Quic,
        (None, Some(_)) => {
            info!("Falling back to relay");
            Transport::Relay
        }
        (None, None) => {
            return Err(anyhow!("No direct connection and no relay available"));
        }
    };

    let _endpoint_alive = &endpoint; // keep the QUIC endpoint alive for the session

    // If the direct connection dies mid-session, say so right away (and tell
    // the UI), instead of leaving every later Minecraft connection to fail
    // with an unexplained "timed out".
    if let Some(conn) = &quic_conn {
        let conn = conn.clone();
        let notify = on_path_change.clone();
        let relay_available = relay_fallback.is_some();
        tokio::spawn(async move {
            let reason = conn.closed().await;
            if relay_available {
                warn!("Direct connection lost ({}) — new connections will use the relay", reason);
                if let Some(cb) = notify { cb(Transport::Relay); }
            } else {
                warn!("Direct connection lost ({}) and no relay is available — reconnect to continue", reason);
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(local_addr).await?;
    let bound_port = listener.local_addr()?.port();
    info!("Minecraft proxy ready on 0.0.0.0:{} (transport={})", bound_port, mode.as_str());

    if let Some(cb) = on_connected {
        cb(bound_port, mode);
    }

    loop {
        match listener.accept().await {
            Ok((tcp_stream, peer)) => {
                // The proxy listens on 0.0.0.0 (Minecraft connects to the LAN
                // address it read from the announce), so check who is asking:
                // by default only this computer may use the tunnel.
                if !allow_lan && !crate::lan::is_this_machine(peer.ip()) {
                    warn!(
                        "Rejected a connection from {} — only this computer may use the \
                         local proxy (use --allow-lan / the GUI option to change this)",
                        peer.ip()
                    );
                    drop(tcp_stream);
                    continue;
                }
                // Use the direct tunnel only while it is alive; if it has died,
                // new connections go through the relay (when there is one).
                let live_quic = quic_conn.as_ref().filter(|c| c.close_reason().is_none());
                let now = if live_quic.is_some() { Transport::Quic } else { Transport::Relay };
                info!("Minecraft client connected from {} (transport={})", peer, now.as_str());
                match (live_quic, &relay_fallback) {
                    (Some(conn), _) => {
                        let conn = conn.clone();
                        tokio::spawn(async move {
                            if let Err(e) = forward_from_minecraft(tcp_stream, conn).await {
                                warn!("Join stream error: {}", e);
                            }
                        });
                    }
                    (None, Some(r)) => {
                        let r = r.clone();
                        tokio::spawn(async move {
                            if let Err(e) = crate::relay::join_forward(
                                tcp_stream, &r.addr, &r.room_id, &r.token,
                            ).await {
                                warn!("Relay forward error: {}", e);
                            }
                        });
                    }
                    (None, None) => {
                        warn!("The direct connection is down and there is no relay — dropping this connection");
                        drop(tcp_stream);
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}

async fn forward_from_minecraft(
    tcp: tokio::net::TcpStream,
    conn: quinn::Connection,
) -> Result<()> {
    let (mut mc_r, mut mc_w) = tcp.into_split();
    let (mut quic_send, mut quic_recv) = conn.open_bi().await?;
    let a = tokio::io::copy(&mut mc_r, &mut quic_send);
    let b = tokio::io::copy(&mut quic_recv, &mut mc_w);
    tokio::select! {
        r = a => { r?; }
        r = b => { r?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connect a client to a server over loopback using the given transport
    /// settings, and keep the endpoints alive for the duration of the test.
    async fn connected_pair(
        server_tc: Arc<quinn::TransportConfig>,
        client_tc: Arc<quinn::TransportConfig>,
    ) -> (quinn::Connection, quinn::Endpoint, quinn::Endpoint) {
        let cert = crate::crypto::generate_self_signed_cert().unwrap();
        let fp = crate::crypto::cert_fingerprint(cert.cert.der().as_ref());
        let server_sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let server = build_server_endpoint(server_sock, &cert, server_tc).unwrap();
        let accept = server.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accept.accept().await {
                tokio::spawn(async move {
                    if let Ok(c) = incoming.await {
                        let _ = c.closed().await; // hold it open like run_host does
                    }
                });
            }
        });
        let client = build_client_endpoint(
            std::net::UdpSocket::bind("127.0.0.1:0").unwrap(), fp, client_tc,
        ).unwrap();
        let conn = client.connect(server_addr, "minescale").unwrap().await.unwrap();
        (conn, server, client)
    }

    /// The bug: an idle connection with no keep-alive is closed by the idle
    /// timeout ("Join stream error: timed out").
    #[tokio::test]
    async fn idle_connection_without_keepalive_times_out() {
        let tc = || transport_config(Duration::from_millis(800), None);
        let (conn, _s, _c) = connected_pair(tc(), tc()).await;
        tokio::time::sleep(Duration::from_millis(2000)).await;
        assert!(conn.close_reason().is_some(), "expected the idle timeout to close it");
        assert!(conn.open_bi().await.is_err());
    }

    /// The fix: with a keep-alive well inside the idle timeout, an idle
    /// connection stays usable for many multiples of that timeout.
    #[tokio::test]
    async fn idle_connection_with_keepalive_stays_open() {
        let tc = || transport_config(Duration::from_millis(800), Some(Duration::from_millis(200)));
        let (conn, _s, _c) = connected_pair(tc(), tc()).await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(conn.close_reason().is_none(), "closed: {:?}", conn.close_reason());
        assert!(conn.open_bi().await.is_ok());
    }

    #[test]
    fn icmp_style_udp_errors_are_transient() {
        use std::io::{Error, ErrorKind};
        // what Windows returns from recvfrom after an ICMP port-unreachable
        assert!(is_transient_udp_error(&Error::from(ErrorKind::ConnectionReset)));
        // the raw OS code each platform really uses for "connection reset"
        let raw = if cfg!(windows) { 10054 }          // WSAECONNRESET
                  else if cfg!(target_os = "macos") { 54 }  // ECONNRESET (BSD)
                  else { 104 };                              // ECONNRESET (Linux)
        assert!(is_transient_udp_error(&Error::from_raw_os_error(raw)));
        assert!(is_transient_udp_error(&Error::from(ErrorKind::ConnectionRefused)));
        // genuinely broken sockets still surface
        assert!(!is_transient_udp_error(&Error::from(ErrorKind::PermissionDenied)));
        assert!(!is_transient_udp_error(&Error::from(ErrorKind::InvalidInput)));
    }

    #[test]
    fn production_keepalive_is_well_inside_the_idle_timeout() {
        assert!(QUIC_KEEP_ALIVE * 2 < QUIC_IDLE_TIMEOUT);
    }
}
