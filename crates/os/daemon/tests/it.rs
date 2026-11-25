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
// One test binary: each file a module of it. A test that starts itself again as a helper process
// selects itself by its name here, which carries its module.

// The daemon a test runs in process decodes peer bytes in worker processes started from this
// executable, which is the test binary here.
tilo_automerge_isolate::test_worker!();

/// The daemon a test reaches, with what the program mounts on it.
mod host;
/// The daemon with the language runtime the program mounts: its records, agents and triggers,
/// driven through its API, its dispatch and the device link.
mod language;
#[path = "runtime_daemon_process.rs"]
mod runtime_daemon_process;
#[path = "session_api_connect.rs"]
mod session_api_connect;
