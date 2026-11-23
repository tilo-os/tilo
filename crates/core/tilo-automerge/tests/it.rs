#![allow(
    clippy::expect_used,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file; #[path] keeps the files in place.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "upstream/mod.rs"]
mod upstream;

#[path = "authorship.rs"]
mod authorship;
#[path = "change_hashes.rs"]
mod change_hashes;
#[path = "change_touches.rs"]
mod change_touches;
#[path = "committed_readers.rs"]
mod committed_readers;
#[path = "crafted_changes.rs"]
mod crafted_changes;
#[path = "diff_marks.rs"]
mod diff_marks;
#[path = "diff_walks_what_differs.rs"]
mod diff_walks_what_differs;
#[path = "has_change.rs"]
mod has_change;
#[path = "idle_sync.rs"]
mod idle_sync;
#[path = "known_hashes.rs"]
mod known_hashes;
#[path = "load_hint.rs"]
mod load_hint;
#[path = "load_in_slices.rs"]
mod load_in_slices;
#[path = "map_apply.rs"]
mod map_apply;
#[path = "new_text.rs"]
mod new_text;
#[path = "object_history.rs"]
mod object_history;
#[path = "patch_log_memory.rs"]
mod patch_log_memory;
#[path = "scattered_apply.rs"]
mod scattered_apply;
#[path = "sequence_apply.rs"]
mod sequence_apply;
#[path = "shared_load.rs"]
mod shared_load;
#[path = "speculative_writes.rs"]
mod speculative_writes;
