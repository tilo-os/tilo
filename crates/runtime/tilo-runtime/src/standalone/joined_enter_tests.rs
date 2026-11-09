//! What a member remembers of the workspaces its relay would not enter it
//! into, so nothing that names a workspace makes it ask again and again.
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;

#[test]
fn a_refusal_is_answered_from_what_the_member_remembers() {
    let refusals = Refusals::default();
    let workspace = WorkspaceId::new();
    assert_eq!(refusals.reason(workspace), None);
    let at = Instant::now();
    refusals.remember_at(workspace, at, "the relay does not govern it".into());
    assert_eq!(
        refusals.reason_at(workspace, at),
        Some("the relay does not govern it".into())
    );
    assert_eq!(
        refusals.reason_at(workspace, at + REFUSED_FOR),
        None,
        "a refusal is asked about again once it is old"
    );
}

#[test]
fn each_workspace_keeps_the_answer_the_relay_gave_for_it() {
    let refusals = Refusals::default();
    let (first, second) = (WorkspaceId::new(), WorkspaceId::new());
    let at = Instant::now();
    refusals.remember_at(first, at, "the relay does not govern it".into());
    refusals.remember_at(second, at, "no such workspace".into());
    refusals.remember_at(first, at, "the relay does not govern it".into());
    assert_eq!(
        refusals.reason_at(first, at).unwrap(),
        "the relay does not govern it"
    );
    assert_eq!(refusals.reason_at(second, at).unwrap(), "no such workspace");
    assert_eq!(
        refusals.0.lock().unwrap().len(),
        2,
        "one answer per workspace, however often it is named"
    );
}

#[test]
fn what_the_member_remembers_is_bounded() {
    let refusals = Refusals::default();
    let start = Instant::now();
    let newest = WorkspaceId::new();
    let mut at = start;
    for step in 0..u32::try_from(KEPT_REFUSALS + 8).unwrap() {
        at = start + std::time::Duration::from_millis(u64::from(step));
        let named = if step == 0 {
            newest
        } else {
            WorkspaceId::new()
        };
        refusals.remember_at(named, at, format!("refused {step}"));
    }
    assert!(
        refusals.reason_at(newest, at).is_none(),
        "the oldest refusal is forgotten, not the newest"
    );
    assert!(
        refusals.0.lock().unwrap().len() <= KEPT_REFUSALS,
        "a request that names many workspaces grows nothing here"
    );
}
