//! Network and system diagnostics.

use crate::stun;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::debug;

// ── NAT type ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum NatType {
    /// STUN request failed; UDP is likely blocked.
    UdpBlocked,
    /// Both STUN queries returned the same external port.
    /// Indicates Full-Cone, Address-Restricted, or Port-Restricted Cone NAT.
    /// P2P hole-punching should work.
    Cone,
    /// STUN queries to two different servers returned different external ports.
    /// Indicates Symmetric NAT; P2P is difficult and relay will be used.
    Symmetric,
    /// Only one STUN server responded; cannot determine type.
    Indeterminate,
}

impl NatType {
    pub fn label(&self) -> &str {
        match self {
            Self::UdpBlocked    => "UDP Blocked (P2P unavailable)",
            Self::Cone          => "Cone NAT (P2P ready)",
            Self::Symmetric     => "Symmetric NAT (relay mode)",
            Self::Indeterminate => "Indeterminate (1 server only)",
        }
    }
    pub fn is_ok(&self) -> bool { *self == Self::Cone }
}

// ── Diagnostics result ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DiagResult {
    // Network
    pub ext_v4_primary:   Option<SocketAddr>,  // STUN server 1
    pub ext_v4_secondary: Option<SocketAddr>,  // STUN server 2 (same local port)
    pub nat_type:         NatType,
    pub ipv6_available:   bool,

    // System
    pub os_detail: String,  // e.g. "Ubuntu 22.04 LTS", "macOS", "Windows 11"
    pub arch:      String,  // "x86_64" / "aarch64" / …

    // Minecraft
    pub mc_dir:      Option<PathBuf>,
    pub mc_versions: Vec<String>,
}

// ── Main entry point ──────────────────────────────────────────────────────────

pub async fn run() -> DiagResult {
    tracing::info!("Running diagnostics...");

    // Run network checks in parallel
    let (nat_res, ipv6_res) = tokio::join!(
        check_nat(),
        check_ipv6(),
    );

    let (ext1, ext2, nat_type) = nat_res;
    let ipv6_available = ipv6_res;

    let (mc_dir, mc_versions) = detect_minecraft();

    let result = DiagResult {
        ext_v4_primary:   ext1,
        ext_v4_secondary: ext2,
        nat_type,
        ipv6_available,
        os_detail:  os_detail(),
        arch:       std::env::consts::ARCH.to_string(),
        mc_dir,
        mc_versions,
    };

    tracing::info!("Diagnostics complete: NAT={}", result.nat_type.label());
    result
}

// ── Network checks ────────────────────────────────────────────────────────────

const STUN1: &str = "stun.l.google.com:19302";
const STUN2: &str = "stun.cloudflare.com:3478";

async fn check_nat() -> (Option<SocketAddr>, Option<SocketAddr>, NatType) {
    // Bind one socket and query two different STUN servers from it.
    // If both return the same external port → Cone NAT.
    // If they return different ports         → Symmetric NAT.
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await else {
        return (None, None, NatType::UdpBlocked);
    };

    let addr1 = match stun::query_stun(&socket, STUN1).await {
        Ok(a) => { debug!("STUN1 → {}", a); a }
        Err(e) => {
            debug!("STUN1 failed: {}", e);
            return (None, None, NatType::UdpBlocked);
        }
    };

    let addr2 = match stun::query_stun(&socket, STUN2).await {
        Ok(a) => { debug!("STUN2 → {}", a); a }
        Err(e) => {
            debug!("STUN2 failed: {}", e);
            // Only one result; cannot compare
            return (Some(addr1), None, NatType::Indeterminate);
        }
    };

    let nat = if addr1.port() == addr2.port() {
        NatType::Cone
    } else {
        NatType::Symmetric
    };

    (Some(addr1), Some(addr2), nat)
}

async fn check_ipv6() -> bool {
    // Bind an IPv6 UDP socket and call connect() to a well-known IPv6 address.
    // The OS rejects connect() with ENETUNREACH if there is no IPv6 route.
    let Ok(sock) = UdpSocket::bind("[::]:0").await else {
        return false;
    };
    tokio::time::timeout(
        Duration::from_secs(3),
        sock.connect("[2001:4860:4860::8888]:443"),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

// ── System info ───────────────────────────────────────────────────────────────

fn os_detail() -> String {
    #[cfg(target_os = "linux")]
    {
        // Try to read the human-friendly distro name from /etc/os-release
        if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
                    return rest.trim_matches('"').to_string();
                }
            }
        }
        "Linux".to_string()
    }
    #[cfg(target_os = "macos")]
    {
        // Read the macOS version from the SystemVersion plist
        if let Ok(content) = std::fs::read_to_string(
            "/System/Library/CoreServices/SystemVersion.plist",
        ) {
            // Simple key-value extraction without a full plist parser
            let mut product = None;
            let mut version = None;
            let mut lines = content.lines().peekable();
            while let Some(line) = lines.next() {
                let key = line.trim();
                if key == "<key>ProductName</key>" {
                    if let Some(val) = lines.next() {
                        product = Some(val.trim()
                            .trim_start_matches("<string>")
                            .trim_end_matches("</string>")
                            .to_string());
                    }
                } else if key == "<key>ProductUserVisibleVersion</key>"
                    || key == "<key>ProductVersion</key>"
                {
                    if let Some(val) = lines.next() {
                        version = Some(val.trim()
                            .trim_start_matches("<string>")
                            .trim_end_matches("</string>")
                            .to_string());
                    }
                }
            }
            match (product, version) {
                (Some(p), Some(v)) => return format!("{} {}", p, v),
                (Some(p), None)    => return p,
                _ => {}
            }
        }
        "macOS".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        // Read from the Windows Registry via the registry crate would be ideal,
        // but we avoid extra deps; use the environment variable fallback.
        // PROCESSOR_ARCHITECTURE gives the arch, but OS version is harder.
        // Return a minimal label.
        "Windows".to_string()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        std::env::consts::OS.to_string()
    }
}

// ── Minecraft detection ───────────────────────────────────────────────────────

fn mc_base_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join("Library/Application Support/minecraft"))
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".minecraft"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").ok()?;
        Some(PathBuf::from(appdata).join(".minecraft"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn detect_minecraft() -> (Option<PathBuf>, Vec<String>) {
    let Some(base) = mc_base_dir() else {
        return (None, vec![]);
    };

    if !base.exists() {
        return (None, vec![]);
    }

    let versions_dir = base.join("versions");
    let mut versions: Vec<String> = std::fs::read_dir(&versions_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        // Only include "real" release versions (start with a digit)
        .filter(|name| name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .collect();

    // Sort newest first using semver-style comparison
    versions.sort_by(|a, b| {
        let parse = |s: &str| -> (u32, u32, u32) {
            let parts: Vec<u32> = s.split('.').filter_map(|p| p.parse().ok()).collect();
            (*parts.first().unwrap_or(&0),
             *parts.get(1).unwrap_or(&0),
             *parts.get(2).unwrap_or(&0))
        };
        parse(b).cmp(&parse(a))
    });

    (Some(base), versions)
}
