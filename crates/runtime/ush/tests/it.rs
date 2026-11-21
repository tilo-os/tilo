#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "host_serve.rs"]
pub mod host_serve;
#[path = "hosted_terminal.rs"]
pub mod hosted_terminal;
#[path = "network.rs"]
pub mod network;
#[path = "runtime_dir.rs"]
pub mod runtime_dir;
#[path = "serve_handoff.rs"]
pub mod serve_handoff;
#[path = "socket_activation.rs"]
pub mod socket_activation;

#[path = "helper_runtime.rs"]
pub mod helper_runtime;

#[path = "shell.rs"]
pub mod shell;

#[path = "support/control.rs"]
pub mod control;
