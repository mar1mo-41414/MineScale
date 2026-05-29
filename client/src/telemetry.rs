//! Opt-in telemetry submission.
//!
//! Disabled by default. Enabled via:
//!   - CLI flag `--telemetry`
//!   - env var `MC_SHARE_TELEMETRY=1`
//!
//! Privacy guarantees:
//!   - No external IP, hostname, user name, or installed Minecraft version
//!     list is included.
//!   - OS version is recorded only at the level documented in README
//!     (family + major version).
//!   - A random `session_id` correlates events within a single run; it is
//!     not persisted across runs.
//!   - The coordination server explicitly drops the source IP before
//!     writing to the log.

use serde::Serialize;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role { Host, Join }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind { Cli, Gui }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase { Start, Result }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    StunFailed,
    CoordFailed,
    PunchFailed,
    QuicFailed,
    TlsFailed,
    Cancelled,
    Other,
}

#[derive(Clone)]
pub struct Reporter {
    pub enabled: bool,
    pub base_url: String,
    pub session_id: String,
    pub role: Role,
    pub app_kind: AppKind,
    pub started: Instant,

    // Network snapshot — filled in once at start.
    pub nat_type: Option<String>,
    pub ipv6_available: Option<bool>,

    // Pairing key (room ID) — set as soon as it is known.
    pub room_id: Option<String>,
}

impl Reporter {
    pub fn new(enabled: bool, base_url: String, role: Role, app_kind: AppKind) -> Self {
        Self {
            enabled,
            base_url,
            session_id: random_session_id(),
            role,
            app_kind,
            started: Instant::now(),
            nat_type: None,
            ipv6_available: None,
            room_id: None,
        }
    }

    pub fn set_room(&mut self, room_id: &str) {
        self.room_id = Some(room_id.to_string());
    }

    pub fn set_network(&mut self, nat: &str, ipv6: bool) {
        self.nat_type = Some(nat.to_string());
        self.ipv6_available = Some(ipv6);
    }

    pub async fn send_start(&self) {
        if !self.enabled { return; }
        let body = self.base_payload(Phase::Start, None, None);
        Self::post(&self.base_url, body).await;
    }

    pub async fn send_result(&self, outcome: Outcome, detail: Option<&str>) {
        if !self.enabled { return; }
        let body = self.base_payload(Phase::Result, Some(outcome), detail);
        Self::post(&self.base_url, body).await;
    }

    fn base_payload(
        &self,
        phase: Phase,
        outcome: Option<Outcome>,
        detail: Option<&str>,
    ) -> serde_json::Value {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        serde_json::json!({
            "schema": SCHEMA,
            "ts_ms": ts_ms,
            "session_id": self.session_id,
            "room_id": self.room_id,
            "role": self.role,
            "phase": phase,
            "outcome": outcome,
            "outcome_detail": detail,
            "duration_ms": self.started.elapsed().as_millis() as u64,
            "nat_type": self.nat_type,
            "ipv6_available": self.ipv6_available,
            "os": os_family(),
            "os_detail": os_major(),
            "arch": std::env::consts::ARCH,
            "app_version": env!("CARGO_PKG_VERSION"),
            "app_kind": self.app_kind,
        })
    }

    async fn post(base_url: &str, body: serde_json::Value) {
        // Fire-and-forget: short timeout, no retry, log only.
        let url = format!("{}/api/v1/telemetry", base_url.trim_end_matches('/'));
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Err(e) = client.post(&url).json(&body).send().await {
            tracing::debug!("telemetry post failed: {}", e);
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn random_session_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn os_family() -> &'static str {
    if cfg!(target_os = "macos") { "macos" }
    else if cfg!(target_os = "linux") { "linux" }
    else if cfg!(target_os = "windows") { "windows" }
    else { "other" }
}

/// Coarse OS version label (major only — no minor/build numbers).
fn os_major() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(c) = std::fs::read_to_string(
            "/System/Library/CoreServices/SystemVersion.plist",
        ) {
            let mut lines = c.lines().peekable();
            while let Some(line) = lines.next() {
                if line.trim() == "<key>ProductVersion</key>" {
                    if let Some(val) = lines.next() {
                        let raw = val.trim()
                            .trim_start_matches("<string>")
                            .trim_end_matches("</string>");
                        // Keep only the major: "14.5.1" → "14"
                        if let Some(major) = raw.split('.').next() {
                            return format!("macOS {}", major);
                        }
                    }
                }
            }
        }
        "macOS".into()
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(c) = std::fs::read_to_string("/etc/os-release") {
            let mut id = None;
            let mut ver = None;
            for line in c.lines() {
                if let Some(r) = line.strip_prefix("ID=") {
                    id = Some(r.trim_matches('"').to_string());
                }
                if let Some(r) = line.strip_prefix("VERSION_ID=") {
                    // Take major part only
                    let v = r.trim_matches('"');
                    let major = v.split('.').next().unwrap_or(v);
                    ver = Some(major.to_string());
                }
            }
            return match (id, ver) {
                (Some(i), Some(v)) => format!("{} {}", i, v),
                (Some(i), None)    => i,
                _ => "Linux".into(),
            };
        }
        "Linux".into()
    }
    #[cfg(target_os = "windows")]
    {
        "Windows".into()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        std::env::consts::OS.into()
    }
}

/// Resolve effective telemetry opt-in from CLI flag and env var.
pub fn enabled(cli_flag: bool) -> bool {
    if cli_flag { return true; }
    matches!(
        std::env::var("MC_SHARE_TELEMETRY").as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on"),
    )
}
