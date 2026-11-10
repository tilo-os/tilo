//! Which records a device is sent when it joins a workspace, and which it asks
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
//! A record the platform splits into *chunks* keeps one of them — chunk `0`,
//! the record itself — eager and declares the rest
//! (`derived:chunk.<n>@<base>`, `n` from `1`) on-demand: the record's manifest
//! says which chunks exist, and a device is sent the ones it opens. A write
//! therefore moves only the chunk it changed to the devices holding it.
//!
//! The declaration is the record id itself, because every host can read the
//! route a change travels on, sealed or not.
//!
//! A workspace can also send all of its records on demand: a public one,
//! which anyone may walk into, replays a newcomer only its governance, its
//! shared desktop and the heads of its chains, whatever its records' ids say.
//! Its host decides that for the workspace and says so in the `Ready` that
//! ends a device's replay; the device then asks for every record it opens,
//! and takes one as new only once the host answered it holds none of it.

/// Role of a part of a chained record, in `derived:part.<n>@<base>`.
const PART_ROLE: &str = "part.";
/// Role of a chunk of a split record, in `derived:chunk.<n>@<base>`.
const CHUNK_ROLE: &str = "chunk.";
/// Role of a record declared on-demand, in `derived:on-demand@<base>`.
const ON_DEMAND_ROLE: &str = "on-demand";

/// How a record reaches the devices of a workspace.
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
    /// Chunk `number` of the split record `base`, its `0` being the record
    /// itself; always on-demand.
    Chunk {
        /// The record the chunk belongs to.
        base: &'a str,
        /// The chunk's number, from `1`.
        number: u64,
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
    if let Some(index) = role.strip_prefix(PART_ROLE) {
        return match numbered(index) {
            Some(index) => RecordSync::Part { base, index },
            None => RecordSync::Eager,
        };
    }
    let Some(number) = role.strip_prefix(CHUNK_ROLE) else {
        return RecordSync::Eager;
    };
    // Chunks are numbered from `1`: chunk `0` is the record itself.
    match numbered(number) {
        Some(number) if number > 0 => RecordSync::Chunk { base, number },
        _ => RecordSync::Eager,
    }
}

/// The number a `part.`/`chunk.` role carries, when it is canonical: decimal
/// digits and no leading zero, so one part or chunk has one id.
fn numbered(index: &str) -> Option<u64> {
    let canonical = !index.is_empty()
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && (index == "0" || !index.starts_with('0'));
    canonical
        // ignored-failure: a probe of the id's shape; digits only, so only a number past u64 fails, which is no part or chunk
        .then(|| index.parse().ok())
        .flatten()
}

/// The id of part `index` of the chained record `base`.
#[must_use]
pub fn record_part(base: &str, index: u64) -> String {
    format!("derived:{PART_ROLE}{index}@{base}")
}

/// The id of chunk `number` of the split record `base` (`number` from `1`;
/// chunk `0` is `base`).
#[must_use]
pub fn record_chunk(base: &str, number: u64) -> String {
    format!("derived:{CHUNK_ROLE}{number}@{base}")
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

    #[test]
    fn a_chunk_is_a_numbered_record_sent_when_opened() {
        assert_eq!(
            record_sync(&record_chunk("general", 3)),
            RecordSync::Chunk {
                base: "general",
                number: 3
            }
        );
        // Chunk 0 is the record itself, which a join replays like any record.
        for other in [
            "general",
            "derived:chunk.0@general",
            "derived:chunk.01@general",
            "derived:chunk.@general",
            "derived:chunk.1x@general",
        ] {
            assert_eq!(record_sync(other), RecordSync::Eager, "{other}");
        }
    }
}
