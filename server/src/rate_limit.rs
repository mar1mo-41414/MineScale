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

/// 20 peer-poll requests per IP per minute.
pub fn poll_limiter() -> KeyedLimiter {
    Arc::new(RateLimiter::keyed(Quota::per_minute(
        NonZeroU32::new(20).unwrap(),
    )))
}
