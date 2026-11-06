//! Freshness of a signed outgoing publication. A sealed write expires with the
//! channel epoch it was made under, so its key holder renews it; a cleartext
//! write is re-sent as it was signed, however old it is.
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
