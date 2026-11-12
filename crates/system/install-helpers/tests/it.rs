// One test binary instead of one per file; #[path] keeps the files in place.
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
