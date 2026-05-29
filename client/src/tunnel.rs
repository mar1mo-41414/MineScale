//! QUIC-based encrypted P2P tunnel.
//!
//! Host side  → quinn QUIC server, one bidirectional stream per Minecraft TCP connection.
//! Joiner side → quinn QUIC client, opens a stream for each local TCP connection.
//!
//! Hole punching is done on the same UDP socket before QUIC takes ownership.

use anyhow::{anyhow, Result};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

// ── Hole punching ─────────────────────────────────────────────────────────────

const PROBE_MAGIC: &[u8] = b"MCS\x01";
const HOLE_PUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_INTERVAL: Duration = Duration::from_millis(200);

/// Send probes and wait to receive one back — confirms both NATs have holes punched.
pub async fn punch_holes(socket: &UdpSocket, peer: SocketAddr) -> Result<()> {
    info!("Hole punching to {}…", peer);
    let mut probe_interval = tokio::time::interval(PROBE_INTERVAL);
    let mut recv_buf = [0u8; 64];

    tokio::time::timeout(HOLE_PUNCH_TIMEOUT, async {
        loop {
            tokio::select! {
                _ = probe_interval.tick() => {
                    let _ = socket.send_to(PROBE_MAGIC, peer).await;
                    debug!("Sent hole-punch probe to {}", peer);
                }
                result = socket.recv_from(&mut recv_buf) => {
                    match result {
                        Ok((len, src)) if src == peer && len >= PROBE_MAGIC.len()
                            && recv_buf[..PROBE_MAGIC.len()] == *PROBE_MAGIC =>
                        {
                            info!("Hole punched! Got probe from {}", src);
                            return Ok(());
                        }
                        Ok(_) => continue, // spurious packet
                        Err(e) => return Err(anyhow!(e)),
                    }
                }
            }
        }
    })
    .await
    .map_err(|_| anyhow!("Hole punching timed out after {}s", HOLE_PUNCH_TIMEOUT.as_secs()))?
}

// ── QUIC endpoint builders ────────────────────────────────────────────────────

fn build_server_endpoint(
    std_socket: std::net::UdpSocket,
    cert_key: &rcgen::CertifiedKey,
) -> Result<quinn::Endpoint> {
    use quinn::crypto::rustls::QuicServerConfig;
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

    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server));

    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        std_socket,
        Arc::new(quinn::TokioRuntime),
    )?;

    Ok(endpoint)
}

fn build_client_endpoint(
    std_socket: std::net::UdpSocket,
    expected_fingerprint: Vec<u8>,
) -> Result<quinn::Endpoint> {
    use quinn::crypto::rustls::QuicClientConfig;

    let verifier = Arc::new(PinnedCertVerifier(expected_fingerprint));
    let client_tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    let quic_client = QuicClientConfig::try_from(client_tls)
        .map_err(|e| anyhow!("QuicClientConfig: {}", e))?;

    let mut endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        std_socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_client)));

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
/// For each QUIC bidirectional stream opened by the joiner,
/// connect to the local Minecraft server and pipe traffic.
pub async fn run_host(
    socket: UdpSocket,
    _peer_addr: SocketAddr,
    cert_key: rcgen::CertifiedKey,
    mc_addr: SocketAddr,
) -> Result<()> {
    let std_socket = socket.into_std()?;
    let endpoint = build_server_endpoint(std_socket, &cert_key)?;
    info!("QUIC server listening (waiting for joiner connection)…");

    let conn = endpoint
        .accept()
        .await
        .ok_or_else(|| anyhow!("QUIC endpoint closed before connection"))?
        .await?;
    info!("Joiner connected via QUIC from {}", conn.remote_address());

    loop {
        match conn.accept_bi().await {
            Ok(streams) => {
                let mc_addr = mc_addr;
                tokio::spawn(async move {
                    if let Err(e) = forward_to_minecraft(streams, mc_addr).await {
                        warn!("Host stream error: {}", e);
                    }
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed(_)) => {
                info!("Joiner closed the connection");
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
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

/// Run the joiner side of the tunnel.
/// Accept local TCP connections from Minecraft client, open QUIC streams to host.
pub async fn run_join(
    socket: UdpSocket,
    host_addr: SocketAddr,
    cert_fingerprint: Vec<u8>,
    local_addr: SocketAddr,
) -> Result<()> {
    let std_socket = socket.into_std()?;
    let endpoint = build_client_endpoint(std_socket, cert_fingerprint)?;

    info!("Connecting to host via QUIC at {}…", host_addr);
    let conn = endpoint.connect(host_addr, "minescale")?.await?;
    info!("P2P tunnel established");

    let listener = tokio::net::TcpListener::bind(local_addr).await?;

    loop {
        match listener.accept().await {
            Ok((tcp_stream, peer)) => {
                debug!("Minecraft client connected from {}", peer);
                let conn = conn.clone();
                tokio::spawn(async move {
                    if let Err(e) = forward_from_minecraft(tcp_stream, conn).await {
                        warn!("Join stream error: {}", e);
                    }
                });
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
