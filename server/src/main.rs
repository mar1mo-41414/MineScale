mod api;
mod rate_limit;
mod relay;
mod rooms;
mod telemetry;

use anyhow::Result;
use axum::{routing::{get, post}, Router};
use std::net::SocketAddr;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("mc_share_server=info".parse()?)
                .add_directive("tower_http=info".parse()?),
        )
        .init();

    let http_addr: SocketAddr = std::env::var("LISTEN_HTTP")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()?;

    let relay_addr_str = std::env::var("RELAY_ADDR")
        .unwrap_or_else(|_| format!("{}:9090", public_ip_guess()));

    let relay_bind: SocketAddr = std::env::var("LISTEN_RELAY")
        .unwrap_or_else(|_| "0.0.0.0:9090".into())
        .parse()?;

    let base_url = std::env::var("BASE_URL")
        .unwrap_or_else(|_| "https://coord.minescale.example.com".into());

    let registry = rooms::Registry::new();

    // Telemetry sink is opt-in on the server too. Set TELEMETRY_LOG to enable.
    let telemetry = match std::env::var("TELEMETRY_LOG") {
        Ok(p) if !p.is_empty() => match telemetry::TelemetrySink::new(p.into()).await {
            Ok(s) => Some(s),
            Err(e) => { tracing::warn!("telemetry sink disabled: {}", e); None }
        },
        _ => None,
    };

    let state = api::AppState {
        registry: registry.clone(),
        base_url,
        relay_addr: relay_addr_str,
        room_limiter: rate_limit::room_creation_limiter(),
        join_limiter: rate_limit::join_attempt_limiter(),
        poll_limiter: rate_limit::poll_limiter(),
        telemetry,
    };

    let app = Router::new()
        .route("/api/v1/rooms", post(api::create_room))
        .route("/api/v1/rooms/:room_id/peers", get(api::poll_peers))
        .route("/api/v1/rooms/:room_id/join", post(api::join_room))
        .route("/api/v1/telemetry", post(telemetry::ingest))
        .route("/healthz", get(health))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Spawn TCP relay server
    let relay_listener = tokio::net::TcpListener::bind(relay_bind).await?;
    info!("Relay server binding to {}", relay_bind);
    tokio::spawn(relay::run_relay(relay_listener, registry));

    info!("HTTP API listening on {}", http_addr);
    let listener = tokio::net::TcpListener::bind(http_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

fn public_ip_guess() -> String {
    // Best-effort: return localhost for development.
    // In production, set RELAY_ADDR env var to the server's public IP.
    "127.0.0.1".to_string()
}
