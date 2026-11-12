// One test binary instead of one per file; #[path] keeps the files in place.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::string_slice,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "accounts_native.rs"]
mod accounts_native;
#[path = "backend_commands.rs"]
mod backend_commands;
#[path = "bootc_copy_cache.rs"]
mod bootc_copy_cache;
#[path = "code_refresh.rs"]
mod code_refresh;
#[path = "code_refresh_probe.rs"]
mod code_refresh_probe;
#[path = "dev_ssh.rs"]
mod dev_ssh;
#[path = "dev_target_uki.rs"]
mod dev_target_uki;
#[path = "handoff_copy.rs"]
mod handoff_copy;
#[path = "install_proof.rs"]
mod install_proof;
#[path = "native_python.rs"]
mod native_python;
#[path = "offline_target.rs"]
mod offline_target;
#[path = "staging_handoff.rs"]
mod staging_handoff;
