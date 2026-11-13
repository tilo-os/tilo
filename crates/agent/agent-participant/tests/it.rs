// One test binary instead of one per file: each tests/*.rs linked this whole
// crate separately, and every one of them carried the same app-worker fixtures
// and started the same workers again. #[path] keeps the files where they are,
// the fixtures below are the one copy every file shares, and nextest runs each
// test in its own process, so the isolation separate binaries gave them stays.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::future_not_send,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::string_slice,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unreachable,
    clippy::unused_self,
    clippy::unwrap_used,
    dead_code,
    reason = "in a test these are the assertion, not a hazard; each file reads part of the \
              fixtures they share"
)]

#[path = "common/mod.rs"]
mod common;
#[path = "harness/mod.rs"]
mod harness;
#[path = "../../../session/client/tests/support/participant.rs"]
mod participant;
#[path = "../../../session/client/tests/support/mod.rs"]
mod support;

// Agents hold each record in a confined app worker (this binary, started
// again), whose changes are admitted through the Automerge isolate.
tilo_automerge_isolate::test_worker!();
tilo_agent_participant::app_worker!();

#[path = "app_coverage.rs"]
mod app_coverage;
#[path = "asking_first.rs"]
mod asking_first;
#[path = "chain_reads.rs"]
mod chain_reads;
#[path = "check_facts.rs"]
mod check_facts;
#[path = "empty_state_actions.rs"]
mod empty_state_actions;
#[path = "host_agents.rs"]
mod host_agents;
#[path = "mail_for_agents.rs"]
mod mail_for_agents;
#[path = "messages_for_agents.rs"]
mod messages_for_agents;
#[path = "model_planner.rs"]
mod model_planner;
#[path = "public_workspace_records.rs"]
mod public_workspace_records;
#[path = "render_video.rs"]
mod render_video;
#[path = "replicas.rs"]
mod replicas;
#[path = "search_workspace.rs"]
mod search_workspace;
#[path = "session_coverage.rs"]
mod session_coverage;
#[path = "shared_session.rs"]
mod shared_session;
#[path = "worker_isolation.rs"]
mod worker_isolation;
#[path = "workspace_actions.rs"]
mod workspace_actions;
