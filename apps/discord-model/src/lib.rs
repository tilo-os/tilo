//! The Discord connector's vocabulary, shared by the app (which queues ops
//! and reads events), the outside worker (which performs ops and reports
//! events) and the bridge program's tests. Plain serde types: nothing here
//! talks to Discord or to a room.

pub mod event;
pub mod op;
pub mod perm;

/// Discord's epoch, the zero of a snowflake's timestamp.
pub const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

/// The creation time of a snowflake (any Discord id), in Unix milliseconds.
#[must_use]
pub fn snowflake_ms(id: &str) -> i64 {
    id.parse::<u64>()
        .ok()
        .and_then(|id| i64::try_from(id >> 22).ok())
        .map_or(0, |ms| ms.saturating_add(DISCORD_EPOCH_MS))
}
