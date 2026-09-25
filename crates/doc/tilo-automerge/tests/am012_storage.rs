//! Runs the vendored automerge `resource_limits` storage tests (bounded
//! reconstruct, save and history). `vendor/automerge` is outside the
//! workspace, so its integration tests run through this member.
#[path = "../../../../vendor/automerge/tests/resource_limits_storage.rs"]
mod resource_limits_storage;
