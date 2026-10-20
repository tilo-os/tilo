//! Authority follows authorship: a stranger's button asks, the app's own view is allowed,
//! and revoking trust changes the next decision.

mod common;

use common::*;
use tilolang_runtime::{FlowOutcome, InvokeOutcome, Resolution, Value};

struct Setup {
    w: World,
    stranger: tilolang_runtime::PackageId,
    c: tilolang_runtime::ResourceRef,
}

fn setup() -> Setup {
    let w = World::new();
    let stranger =
        w.rt.load_package(&stranger_wasm(), stranger_meta())
            .unwrap();
    let c = w.counter();
    Setup { w, stranger, c }
}

#[test]
fn a_stranger_button_asks_and_the_apps_own_view_is_allowed() {
    let Setup { w, stranger, c } = setup();

    let own =
        w.rt.open_view(&w.me, "vc", vec![Value::resource(&c)])
            .unwrap();
    assert!(matches!(own.press("btn").unwrap(), InvokeOutcome::Done(_)));
    assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));

    let theirs =
        w.rt.open_view(&w.me, "vs", vec![Value::resource(&c)])
            .unwrap();
    let InvokeOutcome::Suggested(s) = theirs.press("btn").unwrap() else {
        panic!("a foreign invocation from a stranger's view must ask");
    };
    assert_eq!(s.action, "inc");
    assert_eq!(s.chain, vec![stranger]);
    assert_eq!(
        w.rt.read(&c, "value").unwrap(),
        Value::Int(1),
        "asking changes nothing"
    );
}

#[test]
fn approving_a_suggestion_runs_it_and_records_who_approved() {
    let Setup { w, stranger, c } = setup();
    let theirs =
        w.rt.open_view(&w.me, "vs", vec![Value::resource(&c)])
            .unwrap();
    let InvokeOutcome::Suggested(s) = theirs.press("btn").unwrap() else {
        panic!("expected a suggestion");
    };
    let out =
        w.rt.resolve(
            &s.run,
            &tilolang_runtime::Principal::new("boss"),
            Resolution::Approve,
        )
        .unwrap();
    assert_eq!(out, FlowOutcome::Done(Value::Unit));
    assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));

    let notes = w.rt.history(&c.doc).unwrap();
    let n = notes.iter().find(|n| n.action == "inc").unwrap();
    assert_eq!(n.invoker, "me");
    assert_eq!(n.approved_by.as_deref(), Some("boss"));
    assert_eq!(
        n.chain,
        vec![stranger.0.clone()],
        "history names the whole chain"
    );

    assert!(
        w.rt.resolve(&s.run, &w.me, Resolution::Approve).is_err(),
        "answered once"
    );
}

#[test]
fn declining_a_suggestion_changes_nothing() {
    let Setup { w, c, .. } = setup();
    let theirs =
        w.rt.open_view(&w.me, "vs", vec![Value::resource(&c)])
            .unwrap();
    let InvokeOutcome::Suggested(s) = theirs.press("btn").unwrap() else {
        panic!("expected a suggestion");
    };
    let out = w.rt.resolve(&s.run, &w.me, Resolution::Decline).unwrap();
    assert!(matches!(out, FlowOutcome::Done(Value::Error(_))), "{out:?}");
    assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(0));
}

#[test]
fn revoking_trust_changes_the_decision_at_the_next_invocation() {
    let Setup { w, stranger, c } = setup();
    let theirs =
        w.rt.open_view(&w.me, "vs", vec![Value::resource(&c)])
            .unwrap();

    assert!(matches!(
        theirs.press("btn").unwrap(),
        InvokeOutcome::Suggested(_)
    ));

    w.auth.trust(&stranger);
    assert!(
        matches!(theirs.press("btn").unwrap(), InvokeOutcome::Done(_)),
        "trusted authors are allowed"
    );
    assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));

    w.auth.revoke(&stranger);
    assert!(
        matches!(theirs.press("btn").unwrap(), InvokeOutcome::Suggested(_)),
        "the same open view, the same button: revoked at once"
    );
    assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));

    w.auth.trust(&stranger);
    w.auth.deny_module(&stranger);
    assert_eq!(
        theirs.press("btn").unwrap(),
        InvokeOutcome::Denied,
        "deny wins"
    );
}

#[test]
fn a_direct_call_by_a_person_is_allowed() {
    let Setup { w, c, .. } = setup();
    let out =
        w.rt.invoke_action(&w.me, ("inc", vec![Value::resource(&c)]))
            .unwrap();
    assert_eq!(out, InvokeOutcome::Done(Value::Unit));
    w.auth.deny_action("inc");
    let out =
        w.rt.invoke_action(&w.me, ("inc", vec![Value::resource(&c)]))
            .unwrap();
    assert_eq!(out, InvokeOutcome::Denied);
}
