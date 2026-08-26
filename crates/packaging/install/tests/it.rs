// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "runtime_cli.rs"]
mod runtime_cli;
#[path = "trust.rs"]
mod trust;
