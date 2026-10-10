#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A participant without a window joins the running app's record by its
//! kind and edits it through the same door a person uses: permissions,
//! attribution, history and one step per gesture.

use tilo_app::framework::{Participant, resource_kind};
use tilo_app::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
use tilo_app::prelude::*;

use crate::kinds::{Tally, TallyApp};

const AGENT: &str = "scribe";

fn granted(verb: Verb) -> PermissionContext {
    PermissionContext::from_capabilities(
        Actor::agent(AGENT),
        vec![CapOverride::grant(
            Subject::agent(AGENT),
            Scope::App(TallyApp::ID.to_owned()),
            verb,
        )],
    )
}

/// The running app and a participant joined from its record.
fn joined(verb: Verb) -> (TestApp<TallyApp>, Participant) {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    app.invoke(Tally::RENAME.with("visitors".into())).unwrap();
    let snapshot = app.read_document(|doc| doc.clone().save());
    let participant =
        Participant::from_snapshot(resource_kind::<TallyApp>(), &snapshot, AGENT, granted(verb))
            .unwrap();
    (app, participant)
}

fn merge(app: &mut TestApp<TallyApp>, participant: &Participant) {
    let mut peer = participant.doc().clone();
    app.merge_peer(&mut peer).unwrap();
}

#[test]
fn a_participant_edits_through_the_door_and_history_names_it() {
    let (mut app, mut participant) = joined(Verb::Write);
    assert!(
        participant
            .operations()
            .iter()
            .any(|operation| operation.address.action == "bump")
    );
    participant.set_context(Some(tilo_actions::ActionContext::default().with_group(
        "run-1",
        "Count",
        "agent-run",
    )));
    let changes = participant
        .invoke(
            &tilo_actions::ActionAddress::new("test.tally", "bump"),
            &serde_json::Map::new(),
        )
        .unwrap();
    assert!(
        changes.is_some(),
        "an edit answers with the changes to send"
    );
    participant.set_context(None);
    assert_eq!(participant.state::<Tally>().unwrap().count, 1);

    merge(&mut app, &participant);
    assert_eq!(app.state().count, 1);
    let version = app.versions().into_iter().last().unwrap();
    let context = version.context.unwrap();
    assert_eq!(context.action_label, "Count one more");
    assert_eq!(context.invoker.unwrap().actor, AGENT);
    assert_eq!(context.app.unwrap().id, TallyApp::ID);
    assert_eq!(context.group.unwrap().id, "run-1");
}

#[test]
fn typed_edits_apply_as_one_step() {
    let (mut app, mut participant) = joined(Verb::Write);
    let before = app.versions().len();
    let rename = |name: &str| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("name".into(), serde_json::json!(name));
        tilo_resource::build_action::<Tally>("rename", &arguments, 0).unwrap()
    };
    participant
        .apply_all_typed::<Tally>(&[rename("guests"), rename("members")])
        .unwrap()
        .unwrap();
    merge(&mut app, &participant);
    assert_eq!(app.state().name, "members");
    assert_eq!(app.versions().len(), before + 1, "a group is one version");
}

#[test]
fn a_participant_reaches_nothing_it_is_not_granted() {
    let (_app, mut participant) = joined(Verb::Read);
    assert!(participant.operations().is_empty());
    let refused = participant.invoke(
        &tilo_actions::ActionAddress::new("test.tally", "bump"),
        &serde_json::Map::new(),
    );
    assert!(matches!(
        refused,
        Err(tilo_actions::door::DoorError::Unknown(_))
    ));
    assert_eq!(participant.state::<Tally>().unwrap().count, 0);
}

#[test]
fn changes_of_another_record_are_left_alone() {
    use tilo_app::automerge::transaction::Transactable as _;
    let (mut app, mut participant) = joined(Verb::Write);
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
    assert!(!participant.merge_changes(&unrelated));
    assert_eq!(participant.missing_changes(), 0, "nothing is left waiting");

    app.invoke(Tally::BUMP.invoke()).unwrap();
    let ours = app.read_document(|doc| doc.clone().save());
    assert!(participant.merge_snapshot(&ours));
    assert_eq!(participant.state::<Tally>().unwrap().count, 1);
}

#[test]
fn a_run_is_recorded_in_the_coordinating_record_by_the_participant() {
    let (mut app, mut participant) = joined(Verb::Write);
    participant
        .invoke(
            &tilo_actions::ActionAddress::new("test.tally", "bump"),
            &serde_json::Map::new(),
        )
        .unwrap();
    let last = participant.versions().into_iter().last().unwrap();
    let end = tilo_app::history_source::entry_id_from_hash(&last.hash.to_string());
    let steps = [tilo_app::RunStep {
        app: TallyApp::ID.into(),
        record: String::new(),
        run: tilo_app::ChangeRun::single(end),
        summary: "Counted one more".into(),
    }];
    assert!(
        participant
            .record_run("Count", None, None, &steps)
            .is_some()
    );
    assert!(participant.record_run("Nothing", None, None, &[]).is_none());

    merge(&mut app, &participant);
    let runs = app.read_document(tilo_app::read_runs);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs.first().unwrap().label, "Count");
}

#[test]
fn a_grant_of_one_action_reaches_that_action_only() {
    let app = TestApp::<TallyApp>::new().unwrap();
    let snapshot = app.read_document(|doc| doc.clone().save());
    let only_bump = PermissionContext::from_capabilities(
        Actor::agent(AGENT),
        vec![CapOverride::grant(
            Subject::agent(AGENT),
            Scope::Action {
                app: TallyApp::ID.to_owned(),
                resource: "test.tally".to_owned(),
                action: "bump".to_owned(),
            },
            Verb::Write,
        )],
    );
    let mut participant =
        Participant::from_snapshot(resource_kind::<TallyApp>(), &snapshot, AGENT, only_bump)
            .unwrap();
    let actions: Vec<String> = participant
        .operations()
        .into_iter()
        .map(|operation| operation.address.action)
        .collect();
    assert_eq!(actions, ["bump"]);
    participant
        .invoke(
            &tilo_actions::ActionAddress::new("test.tally", "bump"),
            &serde_json::Map::new(),
        )
        .unwrap();
    assert_eq!(participant.state::<Tally>().unwrap().count, 1);
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), serde_json::json!("guests"));
    assert!(
        participant
            .invoke(
                &tilo_actions::ActionAddress::new("test.tally", "rename"),
                &arguments
            )
            .is_err()
    );
    assert_eq!(participant.state::<Tally>().unwrap().count, 1);
}
