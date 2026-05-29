//! Opt-in telemetry sink for connection diagnostics.
//!
//! Privacy model:
//!   - Source IP is never logged.
//!   - Body is bounded (4 KiB) and must parse as a JSON object.
//!   - We only stamp a server-side receive timestamp and append the line.
//!   - File rotation is left to logrotate / external tooling.

use anyhow::Result;
use axum::{extract::State, http::StatusCode, response::IntoResponse};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::warn;

const MAX_BODY: usize = 4 * 1024;

#[derive(Clone)]
pub struct TelemetrySink {
    file: Arc<Mutex<tokio::fs::File>>,
}

impl TelemetrySink {
    pub async fn new(path: PathBuf) -> Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        tracing::info!("telemetry sink open: {}", path.display());
        Ok(Self { file: Arc::new(Mutex::new(file)) })
    }

    async fn write_line(&self, line: &str) -> std::io::Result<()> {
        let mut f = self.file.lock().await;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await
    }
}

/// POST /api/v1/telemetry
pub async fn ingest(
    State(state): State<crate::api::AppState>,
    body: String,
) -> impl IntoResponse {
    if body.len() > MAX_BODY {
        return (StatusCode::PAYLOAD_TOO_LARGE, "too large").into_response();
    }
    let mut value: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid json").into_response(),
    };
    let Some(obj) = value.as_object_mut() else {
        return (StatusCode::BAD_REQUEST, "not an object").into_response();
    };

    // Server-side stamp (privacy: no IP, no headers).
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    obj.insert("server_recv_ts_ms".into(), serde_json::Value::from(now_ms));

    let line = match serde_json::to_string(&value) {
        Ok(s) => s,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "serialize").into_response(),
    };

    if let Some(sink) = &state.telemetry {
        if let Err(e) = sink.write_line(&line).await {
            warn!("telemetry write failed: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "write").into_response();
        }
        StatusCode::NO_CONTENT.into_response()
    } else {
        // Server-side telemetry is disabled — silently accept and drop.
        StatusCode::NO_CONTENT.into_response()
    }
}
