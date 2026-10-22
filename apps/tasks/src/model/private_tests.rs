#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "in a test these are the assertion"
)]

use std::rc::Rc;

use tilo_automerge::ApplyIncoming as _;

use ui::framework::Children;
use ui::permissions::policy::{
    ActionKind, Answer, Authority, Context, Role, RoomPolicy, Subject, TreePath, roles,
};
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordStore};
use ui::tilo_resource::{Id, Invoke, Model as _, Reader, Replica, Resource as _, build_action};

use super::{private_record, readers};
use crate::model::{APP_ID, Tracker, tracker};

const ADA: &str = "ada";

fn action(invoke: &Invoke<Tracker>) -> <Tracker as ui::tilo_resource::Resource>::Action {
    build_action::<Tracker>(invoke.id, &invoke.arguments, 0).unwrap()
}

/// Ada's tracker, with one issue she wrote, kept in a record store as the
/// app keeps it; the issue's id.
fn tracker() -> (RecordStore, Id) {
    let mut ada = Replica::<Tracker>::new().as_author(ADA);
    ada.apply(Tracker::CREATE.with(tracker::Create {
        title: "Salary review".into(),
        parent: None,
        body: Some("Bea's numbers".into()),
        status: None,
        assignee: Some("person:bea".into()),
    }))
    .unwrap();
    let issue = ada.state().unwrap().issues.ids().next().cloned().unwrap();
    ada.apply(Tracker::ADD_LABEL.with(tracker::AddLabel {
        issue: issue.clone(),
        label: "people".into(),
    }))
    .unwrap();
    let store = RecordStore::in_memory();
    let record = RecordCheckout::new(RecordId::room_default(), RecordKind::new(Tracker::KIND));
    store.create_record(&record).unwrap();
    let mut written = ui::automerge::AutoCommit::load(&ada.save()).unwrap();
    store
        .with(&record.id, |doc| doc.merge_incoming(&mut written).map(drop))
        .unwrap()
        .unwrap();
    (store, issue)
}

fn read(store: &RecordStore, record: &RecordId) -> Tracker {
    store
        .read(record, |doc| {
            Reader::<Tracker>::read(doc)
                .unwrap()
                .state()
                .as_ref()
                .clone()
        })
        .unwrap()
}

fn apply(store: &RecordStore, invoke: &Invoke<Tracker>) {
    let action = action(invoke);
    store
        .with(&RecordId::room_default(), |doc| {
            let mut replica = Replica::<Tracker>::from_doc(doc.fork()).as_author(ADA);
            replica.apply_action(&action).unwrap();
            let mut changed = replica.doc().clone();
            doc.merge_incoming(&mut changed).map(drop)
        })
        .unwrap()
        .unwrap();
}

fn private(store: &RecordStore) -> Children<Tracker> {
    let parent = store.clone();
    Children::<Tracker>::detached_in(store.clone(), APP_ID, ADA, move || {
        Rc::new(read(&parent, &RecordId::room_default()))
    })
    .unwrap()
}

#[test]
fn a_private_issue_moves_whole_into_its_record_and_the_tracker_keeps_its_id_alone() {
    let (store, issue) = tracker();
    let before = read(&store, &RecordId::room_default());
    let held = before.issues.get(&issue).unwrap().clone();
    let private = private(&store);

    // One step: its record listed under the issue's own id, the issue
    // written into it and gone from the tracker.
    private.take(&issue, None).unwrap();

    let kept = read(&store, &private_record(&issue).unwrap());
    let moved = kept.issues.get(&issue).unwrap();
    let whole = |issue: &crate::model::Issue| {
        (
            issue.number,
            issue.title.clone(),
            issue.body.text(),
            issue.assignee.clone(),
            issue.labels.clone(),
            issue.author.clone(),
        )
    };
    assert_eq!(
        whole(moved),
        whole(&held),
        "the issue is kept whole, its number too"
    );
    let tracker = read(&store, &RecordId::room_default());
    assert!(tracker.issues.is_empty());
    // What the tracker says of it: that a private issue of this id is there.
    let listed = tracker.private.get(&issue).unwrap();
    assert_eq!(listed.kind.0, Tracker::KIND);
    assert_eq!((listed.group.as_ref(), listed.trashed), (None, None));
    let json = tracker.to_json().to_string();
    for leak in ["Salary", "Bea", "people", "person:bea"] {
        assert!(!json.contains(leak), "the tracker reads {leak}: {json}");
    }
    assert_eq!(private.readable().len(), 1);
    // A sub-issue made there numbers on from it.
    private
        .apply(
            &issue,
            &action(&Tracker::CREATE.with(tracker::Create {
                title: "Bea's part".into(),
                parent: Some(issue.clone()),
                body: None,
                status: None,
                assignee: None,
            })),
        )
        .unwrap();
    let kept = read(&store, &private_record(&issue).unwrap());
    assert!(
        kept.issues
            .values()
            .any(|sub| sub.number == held.number + 1)
    );
}

#[test]
fn an_issue_with_a_run_going_or_sub_issues_stays_and_a_finished_run_stays_behind() {
    let (store, issue) = tracker();
    apply(
        &store,
        &Tracker::CREATE.with(tracker::Create {
            title: "Bea's part".into(),
            parent: Some(issue.clone()),
            body: None,
            status: None,
            assignee: None,
        }),
    );
    let private = private(&store);
    let refused = private.take(&issue, None).unwrap_err();
    assert!(refused.to_string().contains("sub-issues"), "{refused}");

    let mut ada = Replica::<Tracker>::new().as_author(ADA);
    ada.apply(Tracker::CREATE.with(tracker::Create {
        title: "Audit".into(),
        parent: None,
        body: None,
        status: None,
        assignee: Some("agent:claude".into()),
    }))
    .unwrap();
    let audit = ada.state().unwrap().issues.ids().next().cloned().unwrap();
    ada.apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
        issue: audit.clone(),
    }))
    .unwrap();
    let going = ada.state().unwrap();
    let refused = <Tracker as ui::framework::ChildRecord>::split(&going, &audit).unwrap_err();
    assert!(refused.contains("Stop the run"), "{refused}");
    ada.apply(Tracker::STOP_RUN.with(tracker::StopRun {
        issue: audit.clone(),
    }))
    .unwrap();
    let stopped = ada.state().unwrap();
    assert!(stopped.issues.get(&audit).unwrap().run.is_some());
    let (content, _) = <Tracker as ui::framework::ChildRecord>::split(&stopped, &audit).unwrap();
    let edit = format!("{content:?}");
    assert!(edit.contains("\"run\": Null"), "no run comes along: {edit}");
}

#[test]
fn readers_are_named_as_assignees_are() {
    assert_eq!(
        readers("person:bea, agent:scout").unwrap(),
        [Subject::person("bea"), Subject::agent("scout")]
    );
    assert_eq!(readers("").unwrap(), []);
    assert!(readers("bea").is_none());
}

/// The room of the tracker: Ada and Cal Editors, an agent of Ada's.
fn room(issue: &Id, readers: &[String]) -> RoomPolicy {
    let mut room = RoomPolicy::new("r", "owner");
    room.members.insert(ADA.into(), Role::Editor);
    room.members.insert("cal".into(), Role::Editor);
    room.grants.push(kept("r", issue, readers));
    room
}

/// The grant keeping `issue` of room `room` to Ada and `readers`.
fn kept(room: &str, issue: &Id, readers: &[String]) -> ui::permissions::policy::Grant {
    let mut subjects = vec![Subject::person(ADA)];
    subjects.extend(readers.iter().map(|name| super::reader(name).unwrap()));
    roles::readers_grant(
        room,
        APP_ID,
        private_record(issue).unwrap().route(),
        subjects,
        Subject::person(ADA),
    )
}

fn reads(room: &RoomPolicy, person: &str, agent: Option<&str>, record: &str) -> bool {
    let node = TreePath::room("r").app(APP_ID).resource(record);
    let cx = Context::at(1);
    let mut authority = Authority::person(room.party(person));
    let delegation;
    if let Some(agent) = agent {
        delegation = roles::agent_delegation("r", agent, person);
        authority = authority.bounded_by(delegation.party(&room.grants));
    }
    authority.decide(&node, ActionKind::Read, &cx).answer == Answer::Allow
}

#[test]
fn only_its_author_and_the_named_read_a_private_issue() {
    let issue = Id::from_raw("i1");
    let record = private_record(&issue).unwrap();
    let record = record.route();
    let tracker = RecordId::room_default();
    let room = room(&issue, &["agent:claude".into()]);
    assert!(reads(&room, ADA, None, record));
    assert!(!reads(&room, "cal", None, record));
    assert!(
        reads(&room, "cal", None, tracker.route()),
        "the tracker reads as before"
    );
    // An agent's run reads it only when the agent and its person do.
    assert!(reads(&room, ADA, Some("claude"), record));
    assert!(!reads(&room, ADA, Some("scout"), record));
    assert!(!reads(&room, "cal", Some("claude"), record));
    assert_eq!(
        kept("r", &issue, &["person:bea".into()]).subject,
        Subject::all_but([Subject::person(ADA), Subject::person("bea")])
    );
}
