#![recursion_limit = "256"]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::string_slice,
    clippy::significant_drop_tightening,
    clippy::many_single_char_names,
    clippy::type_complexity,
    clippy::unused_self,
    clippy::unreachable,
    clippy::future_not_send,
    clippy::arithmetic_side_effects,
    clippy::while_immutable_condition,
    reason = "in a test these are the assertion, not a hazard; one scenario reads in order, and its names are its domain's"
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

#[path = "actions_host.rs"]
mod actions_host;
#[path = "device_actions.rs"]
mod device_actions;
#[path = "device_host.rs"]
mod device_host;
