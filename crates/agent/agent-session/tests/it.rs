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
#[path = "../../../session/client/tests/support/mod.rs"]
mod support;

// Agents hold each record in a confined app worker (this binary, started
// again), whose changes are admitted through the Automerge isolate.
tilo_automerge_isolate::test_worker!();
tilo_agent_session::app_worker!();

#[path = "action_cost.rs"]
mod action_cost;
#[path = "ai_transforms.rs"]
mod ai_transforms;
#[path = "app_coverage.rs"]
mod app_coverage;
#[path = "chain_listing.rs"]
mod chain_listing;
#[path = "chain_reads.rs"]
mod chain_reads;
#[path = "empty_state_actions.rs"]
mod empty_state_actions;
#[path = "issue_listing.rs"]
mod issue_listing;
#[path = "issue_reads.rs"]
mod issue_reads;
#[path = "messages_for_agents.rs"]
mod messages_for_agents;
#[path = "page_view.rs"]
mod page_view;
#[path = "part_handover.rs"]
mod part_handover;
#[path = "render_video.rs"]
mod render_video;
#[path = "replicas.rs"]
mod replicas;
#[path = "worker_isolation.rs"]
mod worker_isolation;
#[path = "workspace_actions.rs"]
mod workspace_actions;
