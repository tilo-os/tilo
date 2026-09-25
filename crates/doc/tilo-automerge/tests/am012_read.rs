//! Runs the vendored automerge bounded-read tests (vendor/automerge is outside
//! the workspace, so its own tests cannot run with `cargo test -p automerge`).
#[path = "../../../../vendor/automerge/tests/resource_limits_read.rs"]
mod resource_limits_read;
