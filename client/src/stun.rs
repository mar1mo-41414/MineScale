use anyhow::{anyhow, Result};
use rand::RngCore;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: [u8; 2] = [0x00, 0x01];
const BINDING_SUCCESS: [u8; 2] = [0x01, 0x01];
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
// Some servers send the deprecated MAPPED-ADDRESS instead
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;

/// Bind a UDP socket, query the STUN server, and return
/// (the bound socket, our public IP:port as seen from the internet).
pub async fn get_external_addr(stun_server: &str) -> Result<(UdpSocket, SocketAddr)> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;

    let stun_addr = tokio::net::lookup_host(stun_server)
        .await?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| anyhow!("Could not resolve STUN server: {}", stun_server))?;

    let mut transaction_id = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut transaction_id);
    let request = build_binding_request(&transaction_id);

    // Retry up to 3 times (UDP is lossy)
    for attempt in 0..3 {
        socket.send_to(&request, stun_addr).await?;

        let mut buf = [0u8; 1024];
        match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            socket.recv_from(&mut buf),
        )
        .await
        {
            Ok(Ok((len, _src))) => {
                if let Ok(addr) = parse_binding_response(&buf[..len], &transaction_id) {
                    tracing::debug!("STUN external address: {}", addr);
                    return Ok((socket, addr));
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                if attempt < 2 {
                    tracing::debug!("STUN timeout, retrying...");
                    continue;
                }
            }
        }
    }

    Err(anyhow!("STUN failed after 3 attempts (server: {})", stun_server))
}

fn build_binding_request(tid: &[u8; 12]) -> [u8; 20] {
    let mut buf = [0u8; 20];
    buf[0..2].copy_from_slice(&BINDING_REQUEST);
    buf[2..4].copy_from_slice(&0u16.to_be_bytes()); // message length = 0
    buf[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    buf[8..20].copy_from_slice(tid);
    buf
}

fn parse_binding_response(data: &[u8], tid: &[u8; 12]) -> Result<SocketAddr> {
    if data.len() < 20 {
        return Err(anyhow!("STUN response too short ({} bytes)", data.len()));
    }
    if data[0..2] != BINDING_SUCCESS {
        return Err(anyhow!("Not a STUN binding success response"));
    }

    let cookie = u32::from_be_bytes(data[4..8].try_into().unwrap());
    if cookie != MAGIC_COOKIE {
        return Err(anyhow!("Invalid STUN magic cookie"));
    }
    if &data[8..20] != tid {
        return Err(anyhow!("STUN transaction ID mismatch"));
    }

    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let end = (20 + msg_len).min(data.len());
    let attrs = &data[20..end];

    let mut i = 0;
    while i + 4 <= attrs.len() {
        let atype = u16::from_be_bytes([attrs[i], attrs[i + 1]]);
        let alen = u16::from_be_bytes([attrs[i + 2], attrs[i + 3]]) as usize;
        i += 4;
        if i + alen > attrs.len() {
            break;
        }
        let value = &attrs[i..i + alen];

        match atype {
            ATTR_XOR_MAPPED_ADDRESS => return parse_xor_mapped(value, tid),
            ATTR_MAPPED_ADDRESS => return parse_mapped(value),
            _ => {}
        }
        // Attributes are padded to 4-byte boundary
        i += (alen + 3) & !3;
    }

    Err(anyhow!("No address attribute in STUN response"))
}

fn parse_xor_mapped(data: &[u8], _tid: &[u8; 12]) -> Result<SocketAddr> {
    if data.len() < 8 {
        return Err(anyhow!("XOR-MAPPED-ADDRESS too short"));
    }
    let family = data[1];
    let xport = u16::from_be_bytes([data[2], data[3]]);
    let port = xport ^ ((MAGIC_COOKIE >> 16) as u16);

    if family == 0x01 {
        let xip = u32::from_be_bytes(data[4..8].try_into().unwrap());
        let ip = xip ^ MAGIC_COOKIE;
        let b = ip.to_be_bytes();
        Ok(SocketAddr::from((
            std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]),
            port,
        )))
    } else {
        Err(anyhow!("IPv6 STUN addresses are not supported yet"))
    }
}

fn parse_mapped(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 8 {
        return Err(anyhow!("MAPPED-ADDRESS too short"));
    }
    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]);
    if family == 0x01 {
        let b: [u8; 4] = data[4..8].try_into().unwrap();
        Ok(SocketAddr::from((std::net::Ipv4Addr::from(b), port)))
    } else {
        Err(anyhow!("IPv6 MAPPED-ADDRESS not supported yet"))
    }
}
