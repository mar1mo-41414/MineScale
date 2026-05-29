//! In-memory room registry with automatic expiry.

use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;
use tracing::info;

const ROOM_ID_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const ROOM_EXPIRY: Duration = Duration::from_secs(15 * 60); // 15 minutes

// ── Data model ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Room {
    pub room_id: String,
    pub host_token: String,
    pub relay_token: String,
    /// Host X25519 public key (base64)
    pub host_pubkey: String,
    /// Host external IP:port (STUN result)
    pub host_stun: String,
    /// SHA-256 fingerprint of host's QUIC TLS cert (base64)
    pub cert_fingerprint: String,
    /// Joiner info — populated when a joiner calls /join
    pub peer: Option<PeerInfo>,
    pub created_at: Instant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerInfo {
    pub join_pubkey: String,
    pub join_stun: String,
}

// ── Registry ──────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct Registry(Arc<DashMap<String, Room>>);

impl Registry {
    pub fn new() -> Self {
        let reg = Self::default();
        reg.start_cleanup_task();
        reg
    }

    pub fn insert(&self, room: Room) {
        self.0.insert(room.room_id.clone(), room);
    }

    pub fn get(&self, room_id: &str) -> Option<Room> {
        self.0.get(room_id).map(|r| r.clone())
    }

    /// Set the joiner peer info; returns false if room not found.
    pub fn set_peer(&self, room_id: &str, peer: PeerInfo) -> bool {
        if let Some(mut entry) = self.0.get_mut(room_id) {
            entry.peer = Some(peer);
            true
        } else {
            false
        }
    }

    pub fn remove(&self, room_id: &str) {
        self.0.remove(room_id);
    }

    fn start_cleanup_task(&self) {
        let map = Arc::clone(&self.0);
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                let now = Instant::now();
                map.retain(|_id, room| {
                    let keep = now.duration_since(room.created_at) < ROOM_EXPIRY;
                    if !keep {
                        info!("Expired room {}", room.room_id);
                    }
                    keep
                });
            }
        });
    }
}

// ── Token / ID generation ─────────────────────────────────────────────────────

pub fn generate_room_id() -> String {
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| ROOM_ID_CHARS[rng.gen_range(0..ROOM_ID_CHARS.len())] as char)
        .collect()
}

/// 128-bit URL-safe token, no padding.
pub fn generate_token() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let bytes: [u8; 16] = rand::thread_rng().gen();
    URL_SAFE_NO_PAD.encode(bytes)
}
