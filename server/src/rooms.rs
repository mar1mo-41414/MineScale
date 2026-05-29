use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;
use tracing::info;

const ROOM_ID_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const ROOM_EXPIRY: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub struct Room {
    pub room_id: String,
    pub host_token: String,
    pub relay_token: String,
    pub host_pubkey: String,
    pub host_stun: String,
    pub cert_fingerprint: String,
    /// All joiners so far; index = position in this vec.
    pub peers: Vec<PeerInfo>,
    pub created_at: Instant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerInfo {
    pub join_pubkey: String,
    pub join_stun: String,
}

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

    /// Append a new joiner; returns the 0-based index of the new peer.
    pub fn add_peer(&self, room_id: &str, peer: PeerInfo) -> Option<usize> {
        self.0.get_mut(room_id).map(|mut entry| {
            let idx = entry.peers.len();
            entry.peers.push(peer);
            idx
        })
    }

    /// Return peers whose index is >= from_idx.
    pub fn get_peers_from(&self, room_id: &str, from_idx: usize) -> Option<Vec<(usize, PeerInfo)>> {
        self.0.get(room_id).map(|r| {
            r.peers
                .iter()
                .enumerate()
                .filter(|(i, _)| *i >= from_idx)
                .map(|(i, p)| (i, p.clone()))
                .collect()
        })
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

pub fn generate_room_id() -> String {
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| ROOM_ID_CHARS[rng.gen_range(0..ROOM_ID_CHARS.len())] as char)
        .collect()
}

pub fn generate_token() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let bytes: [u8; 16] = rand::thread_rng().gen();
    URL_SAFE_NO_PAD.encode(bytes)
}
