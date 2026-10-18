//! The toolkit's clock: `tilo-clock`, under the toolkit's module path.
//!
//! Every monotonic and wall-clock reading in `ui` goes through [`now`] and
//! [`wall`], and every clock type is named through this module rather than
//! `std::time`. A host that has no process clock of its own (or one that must
//! be virtualised, such as a deterministic replay) then has exactly one seam to
//! provide, instead of a hundred scattered `Instant::now()` calls.
//!
//! On targets with a native clock the re-exports are `std::time`'s own types,
//! so values cross freely into crates that name `std::time` directly.
//!
//! `crates/sdk/ui-core/clippy.toml` rejects `Instant::now` and
//! `SystemTime::now` everywhere else in the crate.

pub use tilo_clock::{
    Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH, freeze_wall, from_std,
    local_zone_name, now, to_std, try_unix_now, try_unix_now_ms, try_unix_now_ns, unix_now,
    unix_now_i64, unix_now_ms, unix_now_ms_i64, unix_now_ns, wall, wall_frozen,
};

/// A file-system timestamp, as `std::fs::Metadata::modified` reports it, in
/// the toolkit's [`SystemTime`].
pub use tilo_clock::from_std as from_file_time;

/// Scoped clock readings used by production-path input and deadline tests.
#[cfg(feature = "test-support")]
pub use tilo_clock::testing;
