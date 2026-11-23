#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of four: each tests/*.rs linked this whole crate as a
// binary of its own. #[path] keeps the files where they are, and nextest runs
// each test in a process of its own, so the isolation separate binaries gave
// them stays. A test that starts itself again as a helper process selects
// itself by its name here, which carries its module.

// The hosts and brokers these tests run decode peer bytes in worker processes
// started from this executable, which is the test binary here: the one worker
// entry every module's tests start.
tilo_automerge_isolate::test_worker!();

#[path = "device_actions.rs"]
mod device_actions;
#[path = "device_host.rs"]
mod device_host;
#[path = "root_holder_login.rs"]
mod root_holder_login;
#[path = "runtime_daemon_process.rs"]
mod runtime_daemon_process;
#[path = "session_api_connect.rs"]
mod session_api_connect;
