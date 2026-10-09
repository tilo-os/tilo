#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A host without a screen acts through the app's own view: mounted over the
//! record, never drawn, every operation through the door a click takes, and
//! the change it made flowing back to the person's window.

use tilo_app::collaboration::DocumentMutation;
use tilo_app::framework::{AttachedView, definition};
use tilo_app::permissions::{KindSet, PermissionContext, TreePath};
use tilo_app::prelude::*;
use tilo_app::record::RecordCheckout;

use crate::kinds::{Tally, TallyApp};

const AGENT: &str = "scribe";

fn writer() -> PermissionContext {
    PermissionContext::agent_granted(
        AGENT,
        &TreePath::root().app(TallyApp::ID),
        KindSet::READ_UNDOABLE,
    )
}

fn attached(app: &TestApp<TallyApp>, permissions: PermissionContext) -> AttachedView {
    let definition = definition::<TallyApp>();
    AttachedView::open(
        definition,
        "test-workspace",
        RecordCheckout::new(app.record().clone(), definition.record_kind.clone()),
        app.read_document(|doc| doc.clone().save()),
        AGENT,
        permissions,
        false,
    )
    .unwrap()
}

#[test]
fn an_agent_acts_through_the_view_and_the_window_sees_it() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    app.invoke(Tally::RENAME.with("visitors")).unwrap();
    let mut view = attached(&app, writer());
    assert!(view.take_changes().is_none(), "mounting writes nothing");

    let bump = tilo_actions::ActionAddress::new("test.tally", "bump");
    assert!(
        view.operations()
            .iter()
            .any(|operation| operation.address == bump),
        "the view offers what a person's window offers"
    );
    view.invoke(&bump, &serde_json::Map::new(), false).unwrap();
    assert!(view.landed().is_some(), "the action ran in the view");
    let changes = view.take_changes().expect("the agent's own change");
    assert!(view.take_changes().is_none(), "nothing is taken twice");

    assert!(
        matches!(changes, DocumentMutation::Incremental(_)),
        "an existing record answers with increments"
    );
    let (DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) = changes;
    let mut peer = app.peer_document();
    tilo_automerge::ApplyIncoming::load_incoming(&mut peer, &bytes).unwrap();
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(app.state().count, 1);
    let version = app.versions().into_iter().last().unwrap();
    assert_eq!(version.context.unwrap().invoker.unwrap().actor, AGENT);
}

#[test]
fn the_view_refuses_what_the_participant_may_not_do() {
    let app = TestApp::<TallyApp>::new().unwrap();
    let reader =
        PermissionContext::agent_granted(AGENT, &TreePath::root().app(TallyApp::ID), KindSet::READ);
    let mut view = attached(&app, reader);
    let bump = tilo_actions::ActionAddress::new("test.tally", "bump");
    assert!(view.invoke(&bump, &serde_json::Map::new(), false).is_err());
    assert!(view.take_changes().is_none());
}
