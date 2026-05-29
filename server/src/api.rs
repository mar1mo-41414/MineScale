use crate::{
    rate_limit::KeyedLimiter,
    rooms::{PeerInfo, Registry, Room},
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use tracing::{info, warn};

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub base_url: String,
    pub relay_addr: String,
    pub room_limiter: KeyedLimiter,
    pub join_limiter: KeyedLimiter,
    pub poll_limiter: KeyedLimiter,
}

// ── Requests ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateRoomReq {
    pub host_pubkey: String,
    pub host_stun: String,
    pub cert_fingerprint: String,
}

#[derive(Deserialize)]
pub struct JoinRoomReq {
    pub join_pubkey: String,
    pub join_stun: String,
}

#[derive(Deserialize)]
pub struct PollQuery {
    #[serde(default)]
    pub after: usize,
}

// ── Responses ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct CreateRoomResp {
    pub room_id: String,
    pub host_token: String,
    pub relay_token: String,
    pub relay_addr: String,
    pub share_url: String,
}

#[derive(Serialize)]
pub struct JoinRoomResp {
    pub host_pubkey: String,
    pub host_stun: String,
    pub cert_fingerprint: String,
    pub relay_token: String,
    pub relay_addr: String,
}

#[derive(Serialize)]
pub struct IndexedPeer {
    pub idx: usize,
    pub join_pubkey: String,
    pub join_stun: String,
}

#[derive(Serialize)]
pub struct PollPeersResp {
    pub peers: Vec<IndexedPeer>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/v1/rooms
pub async fn create_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRoomReq>,
) -> impl IntoResponse {
    let ip = extract_ip(&headers);
    if state.room_limiter.check_key(&ip).is_err() {
        warn!("Rate limit: room creation from {}", ip);
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }

    let room_id = crate::rooms::generate_room_id();
    let host_token = crate::rooms::generate_token();
    let relay_token = crate::rooms::generate_token();

    let room = Room {
        room_id: room_id.clone(),
        host_token: host_token.clone(),
        relay_token: relay_token.clone(),
        host_pubkey: req.host_pubkey,
        host_stun: req.host_stun,
        cert_fingerprint: req.cert_fingerprint,
        peers: Vec::new(),
        created_at: std::time::Instant::now(),
    };
    state.registry.insert(room);
    info!("Room {} created from {}", room_id, ip);

    let share_url = format!("{}/{}", state.base_url.trim_end_matches('/'), room_id);
    Json(CreateRoomResp { room_id, host_token, relay_token, relay_addr: state.relay_addr, share_url })
        .into_response()
}

/// GET /api/v1/rooms/:room_id/peers?after=<idx>
/// Returns peers whose index >= after (empty list = no new joiners since last poll).
pub async fn poll_peers(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(q): Query<PollQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let ip = extract_ip(&headers);
    if state.poll_limiter.check_key(&ip).is_err() {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }

    let token = match bearer_token(&headers) {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response(),
    };
    let room = match state.registry.get(&room_id) {
        Some(r) => r,
        None => return (StatusCode::NOT_FOUND, "room not found").into_response(),
    };
    if room.host_token != token {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }

    let peers = state
        .registry
        .get_peers_from(&room_id, q.after)
        .unwrap_or_default()
        .into_iter()
        .map(|(idx, p)| IndexedPeer { idx, join_pubkey: p.join_pubkey, join_stun: p.join_stun })
        .collect();

    Json(PollPeersResp { peers }).into_response()
}

/// POST /api/v1/rooms/:room_id/join
/// Multiple joiners are allowed — no conflict check.
pub async fn join_room(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<JoinRoomReq>,
) -> impl IntoResponse {
    let ip = extract_ip(&headers);
    if state.join_limiter.check_key(&ip).is_err() {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }

    let room = match state.registry.get(&room_id) {
        Some(r) => r,
        None => return (StatusCode::NOT_FOUND, "room not found or expired").into_response(),
    };

    let peer = PeerInfo { join_pubkey: req.join_pubkey, join_stun: req.join_stun };
    let idx = state.registry.add_peer(&room_id, peer).unwrap_or(0);
    info!("Room {} joined by {} (peer #{})", room_id, ip, idx);

    Json(JoinRoomResp {
        host_pubkey: room.host_pubkey,
        host_stun: room.host_stun,
        cert_fingerprint: room.cert_fingerprint,
        relay_token: room.relay_token,
        relay_addr: state.relay_addr,
    })
    .into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extract_ip(headers: &HeaderMap) -> IpAddr {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("authorization")?.to_str().ok()?;
    v.strip_prefix("Bearer ").map(|s| s.to_string())
}
