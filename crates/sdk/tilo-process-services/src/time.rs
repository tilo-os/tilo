//! The toolkit's clock: `tilo-clock`, under the toolkit's module path.
//!
//! Every monotonic and wall-clock reading in `ui` goes through [`now`] and
//! [`wall`], and every clock type is named through this module rather than
//! `std::time`. A process that has no process clock of its own (or one that must
//! be virtualised, such as a deterministic replay) then has exactly one boundary to
//! provide, instead of a hundred scattered `Instant::now()` calls.
//!
//! On targets with a native clock the re-exports are `std::time`'s own types,
//! so values cross freely into crates that name `std::time` directly.
//!
//! `crates/ui/ui-core/clippy.toml` rejects `Instant::now` and
//! `SystemTime::now` everywhere else in the crate.

pub use tilo_clock::time::*;
