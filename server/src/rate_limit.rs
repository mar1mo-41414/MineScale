//! IP-keyed rate limiters backed by `governor`.

use governor::{clock::DefaultClock, state::keyed::DefaultKeyedStateStore, Quota, RateLimiter};
use std::{net::IpAddr, num::NonZeroU32, sync::Arc};

pub type KeyedLimiter = Arc<RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>>;

/// 10 room creations per IP per minute.
pub fn room_creation_limiter() -> KeyedLimiter {
    Arc::new(RateLimiter::keyed(Quota::per_minute(
        NonZeroU32::new(10).unwrap(),
    )))
}

/// 5 join attempts per IP per second.
pub fn join_attempt_limiter() -> KeyedLimiter {
    Arc::new(RateLimiter::keyed(Quota::per_second(
        NonZeroU32::new(5).unwrap(),
    )))
}

/// Peer-poll rate limit.
///
/// The host polls every 3 s while waiting for joiners, so a 20/min cap
/// (the old value) was hit almost exactly after 130 s of waiting.
/// Allow 2/s steady state with a burst of 5 — comfortably above the
/// 0.33/s the host actually produces, while still rejecting flood.
pub fn poll_limiter() -> KeyedLimiter {
    Arc::new(RateLimiter::keyed(
        Quota::per_second(NonZeroU32::new(2).unwrap())
            .allow_burst(NonZeroU32::new(5).unwrap()),
    ))
}
