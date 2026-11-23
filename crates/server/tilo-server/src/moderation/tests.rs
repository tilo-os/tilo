#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;

#[test]
fn only_a_report_wakes_the_agent() {
    let report = Did::Verb(tilo_actions::entry::VerbCall::new(
        tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "report-person"),
        [],
    ));
    assert!(files_a_report(&report));
    let post = Did::Verb(tilo_actions::entry::VerbCall::new(
        tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
        [],
    ));
    assert!(!files_a_report(&post));
}
