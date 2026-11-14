// One test binary instead of five: each tests/*.rs linked this whole crate as a
// binary of its own. #[path] keeps the files where they are, and nextest runs
// each test in a process of its own, so the isolation separate binaries gave
// them stays. A test that starts itself again as a helper process selects
// itself by its name here, which carries its module.

// The hosts and brokers these tests run decode peer bytes in worker processes
// started from this executable, which is the test binary here: the one worker
// entry every module's tests start.
tilo_automerge_isolate::test_worker!();

#[path = "admission.rs"]
mod admission;
#[path = "device_host.rs"]
mod device_host;
#[path = "remote_door.rs"]
mod remote_door;
#[path = "runtime_daemon_process.rs"]
mod runtime_daemon_process;
#[path = "session_door_connect.rs"]
mod session_door_connect;
