//! HTTP API routes for room lifecycle.

use crate::{
    rate_limit::KeyedLimiter,
    rooms::{PeerInfo, Registry, Room},
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use tracing::{info, warn};

// ── Shared application state ──────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub base_url: String,
    pub relay_addr: String,
    pub room_limiter: KeyedLimiter,
    pub join_limiter: KeyedLimiter,
    pub poll_limiter: KeyedLimiter,
}

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateRoomReq {
    pub host_pubkey: String,
    pub host_stun: String,
    pub cert_fingerprint: String,
}

#[derive(Serialize)]
pub struct CreateRoomResp {
    pub room_id: String,
    pub host_token: String,
    pub relay_token: String,
    pub relay_addr: String,
    pub share_url: String,
}

#[derive(Deserialize)]
pub struct JoinRoomReq {
    pub join_pubkey: String,
    pub join_stun: String,
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
pub struct PeerResp {
    pub join_pubkey: String,
    pub join_stun: String,
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
        warn!("Rate limit hit for room creation from {}", ip);
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
        peer: None,
        created_at: std::time::Instant::now(),
    };

    state.registry.insert(room);
    info!("Room {} created from {}", room_id, ip);

    let share_url = format!("{}/{}", state.base_url.trim_end_matches('/'), room_id);

    Json(CreateRoomResp {
        room_id,
        host_token,
        relay_token,
        relay_addr: state.relay_addr.clone(),
        share_url,
    })
    .into_response()
}

/// GET /api/v1/rooms/:room_id/peer
/// 200 with PeerResp when a joiner has registered, 204 if not yet.
pub async fn poll_peer(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
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

    match room.peer {
        Some(peer) => Json(PeerResp {
            join_pubkey: peer.join_pubkey,
            join_stun: peer.join_stun,
        })
        .into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// POST /api/v1/rooms/:room_id/join
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

    if room.peer.is_some() {
        return (StatusCode::CONFLICT, "room already has a joiner").into_response();
    }

    let peer = PeerInfo {
        join_pubkey: req.join_pubkey,
        join_stun: req.join_stun,
    };
    state.registry.set_peer(&room_id, peer);
    info!("Room {} joined from {}", room_id, ip);

    Json(JoinRoomResp {
        host_pubkey: room.host_pubkey,
        host_stun: room.host_stun,
        cert_fingerprint: room.cert_fingerprint,
        relay_token: room.relay_token,
        relay_addr: state.relay_addr.clone(),
    })
    .into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extract_ip(headers: &HeaderMap) -> IpAddr {
    // Trust X-Forwarded-For when behind a reverse proxy.
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
