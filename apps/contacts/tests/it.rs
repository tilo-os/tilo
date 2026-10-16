// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "association.rs"]
mod association;
#[path = "diag_focus.rs"]
mod diag_focus;
