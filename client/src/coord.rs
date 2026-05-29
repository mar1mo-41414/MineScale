//! HTTP client for the coordination server API.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

// ── Request / Response types (shared with server) ────────────────────────────

#[derive(Serialize)]
pub struct CreateRoomRequest {
    pub host_pubkey: String,        // base64(X25519 pubkey)
    pub host_stun: String,          // "ip:port"
    pub cert_fingerprint: String,   // base64(SHA-256 of DER cert)
}

#[derive(Deserialize)]
pub struct CreateRoomResponse {
    pub room_id: String,
    pub host_token: String,
    pub relay_token: String,
    pub relay_addr: String,
    pub share_url: String,
}

#[derive(Serialize)]
pub struct JoinRoomRequest {
    pub join_pubkey: String,
    pub join_stun: String,
}

#[derive(Deserialize)]
pub struct JoinRoomResponse {
    pub host_pubkey: String,
    pub host_stun: String,
    pub cert_fingerprint: String,
    pub relay_token: String,
    pub relay_addr: String,
}

/// Returned by `wait_for_peer` when a joiner has registered.
#[derive(Deserialize)]
pub struct PeerInfo {
    pub join_pubkey: String,
    pub join_stun: String,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct Client {
    base_url: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn create_room(&self, req: CreateRoomRequest) -> Result<CreateRoomResponse> {
        let url = format!("{}/api/v1/rooms", self.base_url);
        let resp = self.http.post(&url).json(&req).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("create_room failed {}: {}", status, body));
        }
        Ok(resp.json().await?)
    }

    /// Long-poll: waits up to `timeout` for a joiner to appear, retrying every 2s.
    pub async fn wait_for_peer(
        &self,
        room_id: &str,
        host_token: &str,
        timeout: std::time::Duration,
    ) -> Result<PeerInfo> {
        let url = format!("{}/api/v1/rooms/{}/peer", self.base_url, room_id);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!("Timed out waiting for someone to join"));
            }

            let resp = self
                .http
                .get(&url)
                .bearer_auth(host_token)
                .send()
                .await?;

            match resp.status() {
                s if s == 200 => return Ok(resp.json().await?),
                s if s == 204 => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
                s => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(anyhow!("wait_for_peer failed {}: {}", s, body));
                }
            }
        }
    }

    pub async fn join_room(&self, room_id: &str, req: JoinRoomRequest) -> Result<JoinRoomResponse> {
        let url = format!("{}/api/v1/rooms/{}/join", self.base_url, room_id);
        let resp = self.http.post(&url).json(&req).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("join_room failed {}: {}", status, body));
        }
        Ok(resp.json().await?)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the room code from a full share URL or pass it through as-is.
pub fn parse_room_id(target: &str) -> String {
    target
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(target)
        .to_string()
}
