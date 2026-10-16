//! Which records a device is sent when it joins a room, and which it asks
//! for when it needs them.
//!
//! Most records are *eager*: a joining device is replayed them and follows
//! their changes live. A record can instead be declared *on-demand* by its
//! id: a joining device is not replayed it, and gets its history and its
//! live changes only once it asks to open it. A big workspace keeps the
//! documents nobody has open out of every join that way.
//!
//! A record that grows without end is kept as a *chain* of numbered parts
//! instead, each a record of its own: `derived:part.<n>@<base>`. The part
//! with the highest number a host holds is the chain's head, which takes the
//! new writes and is replayed like an eager record; every earlier part is
//! on-demand. A part's id is its base and its number, so two writers that
//! both start part `n + 1` start the same record, and nothing has to decide
//! which of them did.
//!
//! The declaration is the record id itself, because every host can read the
//! route a change travels on, sealed or not.

/// Role of a part of a chained record, in `derived:part.<n>@<base>`.
const PART_ROLE: &str = "part.";
/// Role of a record declared on-demand, in `derived:on-demand@<base>`.
const ON_DEMAND_ROLE: &str = "on-demand";

/// How a record reaches the devices of a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordSync<'a> {
    /// Replayed to every joining device and followed live.
    Eager,
    /// Sent to a device only once it opens the record.
    OnDemand,
    /// Part `index` of the chain `base`: eager while it is the chain's head,
    /// on-demand once a later part exists.
    Part {
        /// The record the chain belongs to.
        base: &'a str,
        /// The part's number, from `0`.
        index: u64,
    },
}

/// How `record` (a route's record id) is synced.
#[must_use]
pub fn record_sync(record: &str) -> RecordSync<'_> {
    let Some((role, base)) = record
        .strip_prefix("derived:")
        .and_then(|derived| derived.split_once('@'))
    else {
        return RecordSync::Eager;
    };
    if role == ON_DEMAND_ROLE {
        return RecordSync::OnDemand;
    }
    let Some(index) = role.strip_prefix(PART_ROLE) else {
        return RecordSync::Eager;
    };
    // Canonical decimal only, so one part has one id.
    let canonical = !index.is_empty()
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && (index == "0" || !index.starts_with('0'));
    match index.parse() {
        Ok(index) if canonical => RecordSync::Part { base, index },
        _ => RecordSync::Eager,
    }
}

/// The id of part `index` of the chained record `base`.
#[must_use]
pub fn record_part(base: &str, index: u64) -> String {
    format!("derived:{PART_ROLE}{index}@{base}")
}

/// The id of `base` declared on-demand.
#[must_use]
pub fn on_demand_record(base: &str) -> String {
    format!("derived:{ON_DEMAND_ROLE}@{base}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_id_declares_how_it_syncs() {
        assert_eq!(record_sync("general"), RecordSync::Eager);
        assert_eq!(
            record_sync("derived:default@tab:0000000000000000"),
            RecordSync::Eager
        );
        assert_eq!(
            record_sync(&on_demand_record("uuid:1")),
            RecordSync::OnDemand
        );
        assert_eq!(
            record_sync(&record_part("chat", 12)),
            RecordSync::Part {
                base: "chat",
                index: 12
            }
        );
        assert_eq!(
            record_sync(&record_part("", 0)),
            RecordSync::Part { base: "", index: 0 }
        );
        // One part, one id: anything else is an ordinary record.
        for other in [
            "derived:part.01@chat",
            "derived:part.@chat",
            "derived:part.1x@chat",
            "derived:part.-1@chat",
        ] {
            assert_eq!(record_sync(other), RecordSync::Eager, "{other}");
        }
    }
}
