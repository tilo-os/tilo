//! Which device takes a turn of work that several devices of a workspace could each take.
//!
//! A turn is anything one device should do once for the whole workspace: an agent's answer to
//! an ask, first of all. Every eligible device that sees it claims it with an ordinary change of
//! the resource it is about (an `Ask`'s `claims`), then reads the claims back and asks
//! [`takes_turn`]. The claims sync like any change, so every device decides the same once it
//! holds the same claims:
//!
//! - the earliest live claim wins, and a tie goes to the lowest device key;
//! - a device that works on its turn claims again now and then ([`RENEW`]); a claim whose device
//!   wrote no claim for [`STALE`] is no longer live, so another device may take the turn over by
//!   claiming it anew;
//! - two devices that were offline at once may both have answered: the one [`takes_turn`] does
//!   not name marks its answer superseded once it sees the other's claim.

/// How long a device's claim stays live without the device claiming again.
pub const STALE_MS: i64 = 60_000;

/// [`STALE_MS`], as a duration.
pub const STALE: std::time::Duration = std::time::Duration::from_secs(60);

/// How often a device working on its turn claims it again, well inside [`STALE`].
pub const RENEW: std::time::Duration = std::time::Duration::from_secs(20);

/// One device's claim of a turn: the device, by its key, and when it claimed, in milliseconds
/// since 1970.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub device: String,
    pub at: i64,
}

/// What [`takes_turn`] decides for one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Taking {
    /// This device holds the turn: it works on it.
    Mine,
    /// No device holds it live: this device claims it, then asks again.
    Claim,
    /// Another device holds it live, by its key: this device leaves it.
    Theirs(String),
}

/// Who takes the turn `claims` claim, as the device `me` decides it at `now` (milliseconds
/// since 1970). Every device that holds the same claims decides the same.
///
/// A device's claims are taken together: it holds the turn from its first claim, and keeps it
/// live while its latest one is younger than [`STALE_MS`]. Of the devices that hold it live, the
/// one with the earliest first claim wins, a tie going to the lowest device key.
#[must_use]
pub fn takes_turn(claims: &[Claim], me: &str, now: i64) -> Taking {
    // Each device's first and latest claim.
    let mut devices: std::collections::BTreeMap<&str, (i64, i64)> =
        std::collections::BTreeMap::new();
    for claim in claims {
        let held = devices
            .entry(claim.device.as_str())
            .or_insert((claim.at, claim.at));
        held.0 = held.0.min(claim.at);
        held.1 = held.1.max(claim.at);
    }
    let winner = devices
        .iter()
        .filter(|(_, (_, latest))| now.saturating_sub(*latest) < STALE_MS)
        // The map is ordered by device key, so `min_by_key` keeps the lowest key on a tie.
        .min_by_key(|(_, (first, _))| *first)
        .map(|(device, _)| *device);
    match winner {
        None => Taking::Claim,
        Some(device) if device == me => Taking::Mine,
        Some(device) => Taking::Theirs(device.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Claim, Taking, takes_turn};

    fn claim(device: &str, at: i64) -> Claim {
        Claim {
            device: device.to_owned(),
            at,
        }
    }

    /// Nobody holds an unclaimed turn; the earliest claim wins it, ties going to the lowest key;
    /// a claim nobody renewed for a minute is taken over, and its device then leaves the turn.
    #[test]
    fn the_earliest_live_claim_takes_the_turn() {
        assert_eq!(takes_turn(&[], "b", 1_000), Taking::Claim);
        let both = [claim("b", 1_000), claim("a", 1_500)];
        assert_eq!(takes_turn(&both, "b", 2_000), Taking::Mine);
        assert_eq!(takes_turn(&both, "a", 2_000), Taking::Theirs("b".into()));
        let tied = [claim("b", 1_000), claim("a", 1_000)];
        assert_eq!(takes_turn(&tied, "b", 2_000), Taking::Theirs("a".into()));
        // b went quiet; a claims anew after the minute, and holds it from then on.
        let quiet = [claim("b", 1_000)];
        assert_eq!(takes_turn(&quiet, "a", 61_000), Taking::Claim);
        let over = [claim("b", 1_000), claim("a", 61_000)];
        assert_eq!(takes_turn(&over, "a", 61_500), Taking::Mine);
        assert_eq!(takes_turn(&over, "b", 61_500), Taking::Theirs("a".into()));
        // b renews in time: it keeps the turn it claimed first.
        let renewed = [claim("b", 1_000), claim("b", 40_000), claim("a", 61_000)];
        assert_eq!(takes_turn(&renewed, "a", 61_500), Taking::Theirs("b".into()));
    }
}
