//! Which path the game traffic actually takes.
//!
//! Shown in the CLI / GUI so users can tell a direct P2P tunnel from a
//! relayed one, and used as the `transport` field of the telemetry events.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Direct QUIC tunnel between the two peers (TLS 1.3, cert-pinned).
    Quic,
    /// TCP relay through the coordination server.
    Relay,
}

impl Transport {
    /// Stable identifier for telemetry (`"quic"` / `"relay"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quic  => "quic",
            Self::Relay => "relay",
        }
    }

    /// Short human label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Quic  => "Direct (P2P)",
            Self::Relay => "Relay (via server)",
        }
    }

    /// One-line explanation, including what is and isn't protected.
    ///
    /// Be precise here: on the relay path MineScale itself does not
    /// encrypt the stream — the server forwards the bytes as-is.
    pub fn detail(self) -> &'static str {
        match self {
            Self::Quic  => "Direct connection, encrypted end-to-end by MineScale.",
            Self::Relay => "Traffic passes through the relay server and is NOT \
                            encrypted by MineScale (Minecraft's own protection still applies).",
        }
    }
}
