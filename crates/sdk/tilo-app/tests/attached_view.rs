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

/// What the view did and has not sent yet, merged into the person's window.
fn merge(app: &mut TestApp<TallyApp>, view: &mut AttachedView) {
    let Some(DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) =
        view.take_changes()
    else {
        return;
    };
    let mut peer = app.peer_document();
    tilo_automerge::ApplyIncoming::load_incoming(&mut peer, &bytes).unwrap();
    app.merge_peer(&mut peer).unwrap();
}

fn count(view: &AttachedView) -> i64 {
    view.read(|doc| <Tally as tilo_resource::Resource>::read(doc).unwrap().count)
        .unwrap()
}

#[test]
fn history_names_the_agent_its_app_and_the_run_it_acts_in() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    app.invoke(Tally::RENAME.with("visitors")).unwrap();
    let mut view = attached(&app, writer());
    view.set_context(Some(tilo_actions::ActionContext::default().with_group(
        "run-1",
        "Count",
        "agent-run",
    )));
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    view.set_context(None);
    assert_eq!(count(&view), 1);

    merge(&mut app, &mut view);
    assert_eq!(app.state().count, 1);
    let version = app.versions().into_iter().last().unwrap();
    let context = version.context.unwrap();
    let Some(tilo_actions::Did::Verb(verb)) = &context.what else {
        panic!("the agent's edit records its verb: {context:?}");
    };
    assert_eq!(verb.action.action, "bump");
    assert_eq!(context.invoker.unwrap().actor, AGENT);
    assert_eq!(context.app.unwrap().id, TallyApp::ID);
    assert_eq!(context.group.unwrap().id, "run-1");
}

#[test]
fn an_agent_acting_for_a_person_is_recorded_as_such() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let definition = definition::<TallyApp>();
    let agent = tilo_actions::ActionInvoker::actor(AGENT)
        .with_kind(tilo_actions::ActorKind::Agent)
        .acting_for("ada");
    let mut view = AttachedView::open_for(
        definition,
        "test-workspace",
        RecordCheckout::new(app.record().clone(), definition.record_kind.clone()),
        app.read_document(|doc| doc.clone().save()),
        &agent,
        writer(),
        false,
    )
    .unwrap();
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    merge(&mut app, &mut view);
    let invoker = app
        .versions()
        .into_iter()
        .last()
        .and_then(|version| version.context)
        .and_then(|context| context.invoker)
        .unwrap();
    assert_eq!(invoker, agent, "the agent, for the person who asked");
}

#[test]
fn several_typed_edits_apply_as_one_step() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let mut view = attached(&app, writer());
    let before = app.versions().len();
    view.invoke_typed(
        &Invoke::group([Tally::RENAME.with("guests"), Tally::RENAME.with("members")]),
        false,
    )
    .unwrap();
    merge(&mut app, &mut view);
    assert_eq!(app.state().name, "members");
    assert_eq!(app.versions().len(), before + 1, "a group is one version");
}

#[test]
fn a_reader_is_offered_no_edit() {
    let app = TestApp::<TallyApp>::new().unwrap();
    let reader =
        PermissionContext::agent_granted(AGENT, &TreePath::root().app(TallyApp::ID), KindSet::READ);
    let mut view = attached(&app, reader);
    assert!(view.operations().is_empty());
    assert!(matches!(
        view.invoke_typed(&Tally::BUMP.invoke(), false),
        Err(tilo_actions::door::DoorError::Unknown(_))
    ));
    assert_eq!(count(&view), 0);
}

#[test]
fn changes_of_another_record_are_left_alone() {
    use tilo_app::automerge::transaction::Transactable as _;
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let mut view = attached(&app, writer());
    let mut other = tilo_resource::Doc::new();
    other
        .put(tilo_app::automerge::ROOT, "name", "elsewhere")
        .unwrap();
    other.commit();
    let first = other.get_heads();
    other
        .put(tilo_app::automerge::ROOT, "count", 9_i64)
        .unwrap();
    other.commit();
    let unrelated = other.save_after(&first);
    assert!(
        !view
            .merge(&DocumentMutation::Incremental(unrelated))
            .unwrap()
    );
    assert_eq!(count(&view), 0);

    app.invoke(Tally::BUMP.invoke()).unwrap();
    let ours = app.read_document(|doc| doc.clone().save());
    assert!(view.merge(&DocumentMutation::Snapshot(ours)).unwrap());
    assert_eq!(count(&view), 1);
}

#[test]
fn a_run_is_recorded_in_the_coordinating_record_by_the_agent() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let mut view = attached(&app, writer());
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    let last = view
        .read(|doc| tilo_resource::history::versions(&mut doc.clone()))
        .unwrap()
        .into_iter()
        .last()
        .unwrap();
    let end = tilo_app::history_source::entry_id_from_hash(&last.hash.to_string());
    let steps = [tilo_app::RunStep {
        app: TallyApp::ID.into(),
        record: String::new(),
        run: tilo_app::ChangeRun::single(end),
        what: vec![tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(TallyApp::ID, "document", "bump"),
            [],
        ))],
    }];
    assert!(view.record_run("Count", None, None, &steps));
    assert!(!view.record_run("Nothing", None, None, &[]));

    merge(&mut app, &mut view);
    let runs = app.read_document(tilo_app::read_runs);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs.first().unwrap().label, "Count");
}

#[test]
fn a_grant_of_one_action_reaches_that_action_only() {
    use tilo_app::permissions::policy::Level;
    let app = TestApp::<TallyApp>::new().unwrap();
    let only_bump = PermissionContext::agent_granted(
        AGENT,
        &TreePath::root()
            .app(TallyApp::ID)
            .any(Level::Kind)
            .resource("test.tally")
            .action("bump"),
        KindSet::READ_UNDOABLE,
    );
    let mut view = attached(&app, only_bump);
    let actions: Vec<String> = view
        .operations()
        .into_iter()
        .map(|operation| operation.address.action)
        .collect();
    assert_eq!(actions, ["bump"]);
    view.invoke_typed(&Tally::BUMP.invoke(), false).unwrap();
    assert_eq!(count(&view), 1);
    assert!(
        view.invoke_typed(&Tally::RENAME.with("guests"), false)
            .is_err()
    );
    assert_eq!(count(&view), 1);
}
