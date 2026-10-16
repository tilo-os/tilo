//! Freshness of a signed outgoing publication. An expired durable incremental
//! is renewed only after fresh governed coverage; its original signed evidence
//! stays in the outbox until the governor covers it.
use crate::NeighbourMsg;
use web_time::{Duration, SystemTime};
pub fn fresh(message: &NeighbourMsg, now: SystemTime) -> bool {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return false;
    };
    let earliest = now
        .checked_sub(Duration::from_secs(300))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let latest = now.checked_add(Duration::from_secs(300)).unwrap_or(now);
    envelope.timestamp >= earliest && envelope.timestamp <= latest
}
