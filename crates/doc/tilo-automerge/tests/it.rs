// One test binary instead of one per file; #[path] keeps the files in place.
#[path = "upstream/mod.rs"]
mod upstream;

#[path = "authorship.rs"]
mod authorship;
#[path = "change_hashes.rs"]
mod change_hashes;
#[path = "committed_readers.rs"]
mod committed_readers;
#[path = "crafted_changes.rs"]
mod crafted_changes;
#[path = "diff_marks.rs"]
mod diff_marks;
#[path = "idle_sync.rs"]
mod idle_sync;
#[path = "known_hashes.rs"]
mod known_hashes;
#[path = "map_apply.rs"]
mod map_apply;
#[path = "patch_log_memory.rs"]
mod patch_log_memory;
#[path = "scattered_apply.rs"]
mod scattered_apply;
#[path = "sequence_apply.rs"]
mod sequence_apply;
#[path = "speculative_writes.rs"]
mod speculative_writes;
