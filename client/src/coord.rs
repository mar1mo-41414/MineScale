use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ── Shared types ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct CreateRoomRequest {
    pub host_pubkey: String,
    pub host_stun: String,
    pub cert_fingerprint: String,
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

/// A joiner with its 0-based index in the room's peer list.
#[derive(Deserialize, Clone, Debug)]
pub struct IndexedPeer {
    pub idx: usize,
    pub join_pubkey: String,
    pub join_stun: String,
}

#[derive(Deserialize)]
struct PollPeersResponse {
    pub peers: Vec<IndexedPeer>,
}

// ── Client ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Client {
    base_url: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn create_room(&self, req: CreateRoomRequest) -> Result<CreateRoomResponse> {
        let url = format!("{}/api/v1/rooms", self.base_url);
        let resp = self.http.post(&url).json(&req).send().await?;
        if !resp.status().is_success() {
            let s = resp.status();
            return Err(anyhow!("create_room {}: {}", s, resp.text().await.unwrap_or_default()));
        }
        Ok(resp.json().await?)
    }

    /// Poll for new joiners with index >= after_idx.
    /// Returns immediately with however many new joiners exist (may be empty).
    pub async fn poll_peers(
        &self,
        room_id: &str,
        host_token: &str,
        after_idx: usize,
    ) -> Result<Vec<IndexedPeer>> {
        let url = format!(
            "{}/api/v1/rooms/{}/peers?after={}",
            self.base_url, room_id, after_idx
        );
        let resp = self.http.get(&url).bearer_auth(host_token).send().await?;
        if !resp.status().is_success() {
            let s = resp.status();
            return Err(anyhow!("poll_peers {}: {}", s, resp.text().await.unwrap_or_default()));
        }
        let body: PollPeersResponse = resp.json().await?;
        Ok(body.peers)
    }

    /// Wait until at least one new joiner appears (polls every 2 s until timeout).
    pub async fn wait_for_peer(
        &self,
        room_id: &str,
        host_token: &str,
        after_idx: usize,
        timeout: Duration,
    ) -> Result<IndexedPeer> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!("Timed out waiting for someone to join"));
            }
            let peers = self.poll_peers(room_id, host_token, after_idx).await?;
            if let Some(first) = peers.into_iter().next() {
                return Ok(first);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn join_room(&self, room_id: &str, req: JoinRoomRequest) -> Result<JoinRoomResponse> {
        let url = format!("{}/api/v1/rooms/{}/join", self.base_url, room_id);
        let resp = self.http.post(&url).json(&req).send().await?;
        if !resp.status().is_success() {
            let s = resp.status();
            return Err(anyhow!("join_room {}: {}", s, resp.text().await.unwrap_or_default()));
        }
        Ok(resp.json().await?)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn parse_room_id(target: &str) -> String {
    target
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(target)
        .to_string()
}
