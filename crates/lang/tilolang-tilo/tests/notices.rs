//! A notice is the workspace's document of a person being told something: every device of theirs
//! shows it and no one else's does, what one of them does with it is what all of them see, and
//! what it opens resolves from any of them.

use crate::fixtures::Workspace;
use serde_json::{Map, Value as Json, json};
use tilo_service::notice::{Closed, Notice};
use tilo_service::{Service, Started};
use tilolang_runtime::{Actor, InvokeOutcome, Meta, Resolution, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::resource_service::ResourceService;

use crate::fixtures::{Replica, action_id};

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const TASKS: (&str, &str) = (
    crate::fixtures::TASKS_SOURCE,
    include_str!("../../../../apps/tl/tasks/app.ids"),
);
const TASKS_APP: &str = "so.tilo.tasks";

/// An app that tells a person about a note and then asks who seals it: the notice is written
/// before the ask, as something that goes out before one is.
const TELLER: (&str, &str) = (
    r#"app so.tilo.test.teller "Teller"

type Note {
    sealed: Bool
}

/// Tells the person about the note, then asks who seals it.
action tell_and_seal(n: Note) uses notify {
    notify(to: invoker, title: "A note of yours", body: "", opens: n)
    seal(n)
}

/// Seals the note: it reaches outside, so a person decides it before it happens.
action seal(n: Note) {
    outside("smtp.send", (n.sealed,))
    n.sealed = true
}

root Note

default view note for Note

view note(n: Note) {
    ui.Text("a note")
}
"#,
    "",
);
const TELLER_APP: &str = "so.tilo.test.teller";
const WORKSPACE: &str = "workspace";

/// What one device showed and took back, and what its platform said of the notifications it
/// closed, as the notifications service would.
#[derive(Clone, Default)]
struct Recorder {
    shown: std::rc::Rc<std::cell::RefCell<Vec<Notice>>>,
    withdrawn: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    closures: std::rc::Rc<std::cell::RefCell<Vec<(String, Closed)>>>,
}

impl Recorder {
    /// The platform closed the notification `tag` for `reason`.
    fn close(&self, tag: &str, reason: Closed) {
        self.closures.borrow_mut().push((tag.to_owned(), reason));
    }
}

impl tilo_service::notice::Notices for Recorder {
    fn show(&self, notice: &Notice) {
        self.shown.borrow_mut().push(notice.clone());
    }

    fn withdraw(&self, tag: &str) {
        self.withdrawn.borrow_mut().push(tag.to_owned());
    }

    fn take_closed(&self) -> Vec<(String, Closed)> {
        std::mem::take(&mut *self.closures.borrow_mut())
    }
}

fn args(value: Json) -> Map<String, Json> {
    value.as_object().cloned().unwrap()
}

/// One device of a workspace: its service over its copy of the workspace's resources.
struct Device {
    service: ResourceService,
    doc: Workspace,
}

impl Device {
    fn poll(&mut self) -> bool {
        self.service.poll()
    }

    fn settle(&mut self) {
        self.service.settle();
    }

    fn host(&self) -> &Host {
        self.service.host()
    }

    fn notice_seen(&mut self, id: &str) {
        self.service.notice_seen(id);
    }

    fn told_kept(&self) -> Vec<String> {
        self.service.told_kept()
    }

    fn take_up_told(&mut self, kept: impl IntoIterator<Item = String>) {
        self.service.take_up_told(kept);
    }

    /// Take in what another device wrote, as sync does, and take the turn that tells the person.
    fn take_in(&mut self, other: &Workspace) {
        if self.doc.take_in(other) {
            self.service.host().store().moved();
        }
        self.poll();
    }
}

/// A session's service over a copy of `base`, for `person`, telling what it shows to a recorder.
/// Its clock stands before the reminder, so its own turn runs nothing until it is told to.
fn service(name: &str, person: &str, base: &Workspace) -> (Device, Recorder) {
    let (replica, _) = Replica::new(
        name,
        base.fork(),
        &[crate::fixtures::AGENT, DISCUSSION, TASKS],
    );
    let (mut service, doc) = replica.into_service_with_workspace(person);
    service.set_clock(|| "2026-10-27T09:00:00Z".to_owned());
    let recorder = Recorder::default();
    service.set_tell(recorder.clone());
    (Device { service, doc }, recorder)
}

/// Ada's workspace with its tasks, as the session runs them, and the clock its app and its
/// triggers read.
fn ada_workspace() -> (Device, Recorder, std::rc::Rc<std::cell::RefCell<String>>) {
    let clock = std::rc::Rc::new(std::cell::RefCell::new("2026-10-27T09:00:00Z".to_owned()));
    let (ada, _) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, DISCUSSION, TASKS],
    );
    ada.host.set_clock({
        let clock = std::rc::Rc::clone(&clock);
        move || clock.borrow().clone()
    });
    ada.host.root_of(TASKS_APP).unwrap();
    ada.commit();
    let (mut service, doc) = ada.into_service_with_workspace("ada");
    service.set_clock({
        let clock = std::rc::Rc::clone(&clock);
        move || clock.borrow().clone()
    });
    let recorder = Recorder::default();
    service.set_tell(recorder.clone());
    (Device { service, doc }, recorder, clock)
}

/// The id of the app's published root: the tasks.
fn tasks_root(service: &ResourceService) -> tilo_actions::ResourceId {
    service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TASKS_APP}.Tasks"))
        .expect("the tasks are published")
        .id
}

/// A task added under `title`: its id as the workspace names it.
fn task(service: &mut Device, title: &str) -> String {
    let root = tasks_root(&service.service);
    let started = service
        .service
        .invoke(root.as_str(), "new_task", &args(json!({ "title": title })))
        .unwrap();
    let Started::Done(output) = started else {
        panic!("a tilolang action is done once committed");
    };
    service.settle();
    output.resources.first().cloned().expect("the task it made")
}

/// Ada makes `call` due today and Bo's, with a reminder in an hour; then the moment comes.
fn remind(service: &mut Device, call: &str, clock: &std::cell::RefCell<String>) {
    for (action, values) in [
        ("assign", json!({ "who": { "person": "bo" } })),
        ("due_today", json!({})),
        ("remind_at", json!({ "when": "2026-10-27T10:00:00Z" })),
    ] {
        service.service.invoke(call, action, &args(values)).unwrap();
    }
    "2026-10-27T10:00:00Z".clone_into(&mut clock.borrow_mut());
    assert!(service.poll(), "the reminder's moment came");
}

/// The document as it is here, to give the next device.
fn document_of(device: &Device) -> Workspace {
    device.doc.fork()
}

/// What the notice's tag names: its id in the document.
fn id_of(tag: &str) -> String {
    tag.strip_prefix("notice:")
        .expect("a notice's tag starts with its kind")
        .split('/')
        .next()
        .expect("a notice's tag names it")
        .to_owned()
}

/// Where choosing the notice lands, as the session makes it.
fn place(notice: &Notice) -> tilo_service::notice::Place {
    notice.place.clone().expect("it opens what it names")
}

/// Invoke the action `name` of `meta`'s package on `args` as the replica's own person, commit
/// what it made, and answer what it returned.
fn invoke(replica: &Replica, meta: &Meta, name: &str, args: Vec<Value>) -> Value {
    let id = action_id(meta, name);
    let InvokeOutcome::Done(value) = replica
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&replica.name)),
            (id.as_str(), args),
        )
        .unwrap()
    else {
        panic!("`{name}` runs");
    };
    replica.commit();
    value
}

/// Ada's tasks on a replica of her own: a task of Bo's, due today, reminded of an hour from
/// nine, with the clock at nine as the actions were made.
fn reminded_workspace() -> (Replica, Meta, std::rc::Rc<std::cell::RefCell<String>>) {
    let clock = std::rc::Rc::new(std::cell::RefCell::new("2026-10-27T09:00:00Z".to_owned()));
    let (ada, packages) = Replica::new(
        "ada",
        Workspace::new(),
        &[crate::fixtures::AGENT, DISCUSSION, TASKS],
    );
    ada.host.set_clock({
        let clock = std::rc::Rc::clone(&clock);
        move || clock.borrow().clone()
    });
    let meta = packages.last().unwrap().meta.clone();
    let tasks = ada.host.root_of(TASKS_APP).unwrap();
    ada.commit();
    let Value::Ref(task) = invoke(
        &ada,
        &meta,
        "new_task",
        vec![
            Value::resource(&tasks),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::none(),
            Value::text("Call the landlord"),
        ],
    ) else {
        panic!("a new task is a task");
    };
    let task = tilolang_runtime::value::ResourceRef::clone(&task.target);
    invoke(
        &ada,
        &meta,
        "assign",
        vec![
            Value::resource(&task),
            Value::some(Value::Actor(Actor::from(
                tilolang_tilo::runtime::ids::MemberId::person("bo"),
            ))),
        ],
    );
    invoke(&ada, &meta, "due_today", vec![Value::resource(&task)]);
    invoke(
        &ada,
        &meta,
        "remind_at",
        vec![Value::resource(&task), Value::text("2026-10-27T10:00:00Z")],
    );
    (ada, meta, clock)
}

/// `who`'s access in the teller workspace: an editor allowed to tell and to reach outside from
/// `tell_and_seal`, so the action runs up to the seal it asks about.
fn teller_access(meta: &Meta, who: &str) -> tilo_action_policy::MemberAccess {
    use tilo_action_policy::{
        Answer, Grant, Grantee, KindSet, MemberAccess, Role, TreePath, WorkspacePolicy,
    };
    let mut policy = WorkspacePolicy::new(WORKSPACE, "sol");
    for member in ["ada", "grace"] {
        policy.members.insert(member.to_owned(), Role::Editor);
    }
    let tell = meta
        .actions
        .iter()
        .find(|action| action.name == "tell_and_seal")
        .expect("the teller tells");
    let node = TreePath::workspace(WORKSPACE)
        .app(TELLER_APP)
        .kind(meta.kind_of("Note"))
        .action(&tell.id);
    let owner = Grantee::person("sol");
    policy.grants.extend([Grant::new(
        "tells",
        Grantee::person(who),
        node,
        KindSet::OUTSIDE,
        Answer::Allow,
        owner,
    )]);
    MemberAccess::person(policy, who)
}

/// Two replicas of a teller workspace: ada told the note's person about it, and sealing the note
/// waits on an answer either of them may give.
fn teller_workspace() -> (Replica, Replica, tilolang_runtime::ResourceRef) {
    let (_, probe) = Replica::new("probe", Workspace::new(), &[TELLER]);
    let meta = probe[0].meta.clone();
    let ids = probe[0].ids.clone();
    let packages = [(TELLER.0, ids.as_str())];
    let (ada, _) = Replica::access(
        "ada",
        Workspace::new(),
        &packages,
        Some(teller_access(&meta, "ada")),
    );
    let note = ada.host.root_of(TELLER_APP).unwrap();
    ada.commit();
    let tell = meta
        .actions
        .iter()
        .find(|action| action.name == "tell_and_seal")
        .expect("the teller tells");
    let args = vec![Value::resource(&note)];
    let InvokeOutcome::Suggested(suggestion) = ada
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (tell.id.as_str(), args.clone()),
        )
        .unwrap()
    else {
        panic!("sealing the note asks first");
    };
    let invocation = ada
        .host
        .runtime()
        .record_suggestion(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            tell.id.as_str(),
            args,
            &suggestion,
        )
        .unwrap();
    ada.commit();
    let grace = Replica::access(
        "grace",
        ada.doc.fork(),
        &packages,
        Some(teller_access(&meta, "grace")),
    )
    .0;
    (ada, grace, invocation)
}

tilo_test_env::scenario! {
    fn a_notice_two_replicas_write_is_told_once;

    phase a_notice_a_run_writes_before_it_asks_is_one_when_both_replicas_run_it;
    {
        let (ada, grace, invocation) = teller_workspace();
        // Ada answers: the run goes on from its journal, and the notice the prefix wrote stays one.
        assert_eq!(
            ada.host
                .runtime()
                .resolve_invocation(&invocation, &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")), Resolution::Approve)
                .unwrap(),
            "done"
        );
        ada.commit();
        assert_eq!(notices(&ada.host).len(), 1, "told once");

        // Grace holds no journal of the run: answering runs the whole action again, telling the
        // person once more before it reaches the ask — under the document ada wrote, so it is one
        // notice — and then answering that seals the note.
        let answer = |grace: &Replica| {
            grace.host.runtime().resolve_invocation(
                &invocation,
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("grace")),
                Resolution::Approve,
            )
        };
        assert_eq!(
            answer(&grace).unwrap(),
            "suggested",
            "it runs the prefix again"
        );
        grace.commit();
        assert_eq!(answer(&grace).unwrap(), "done");
        grace.commit();
        assert_eq!(
            notices(&grace.host).len(),
            1,
            "one notice on her device too"
        );

        ada.sync_with(&grace);
        for replica in [&ada, &grace] {
            assert_eq!(notices(&replica.host).len(), 1, "one notice between them");
        }
    }

    phase a_reminder_its_trigger_runs_on_two_devices_is_told_once;
    {
        let (ada, _, clock) = reminded_workspace();
        let ten = tilolang::calendar::instant("2026-10-27T10:00:00Z").unwrap();

        // Another device of the workspace, which has not heard from ada's yet: both come to run the
        // reminder at its moment, as they do when the origin moves between them.
        let bo = ada.fork("bo", &[crate::fixtures::AGENT, DISCUSSION, TASKS]);
        bo.host.set_clock({
            let clock = std::rc::Rc::clone(&clock);
            move || clock.borrow().clone()
        });
        bo.host.root_of(TASKS_APP).unwrap();

        assert_eq!(ada.host.run_triggers(ten), 1, "the reminder comes due");
        ada.commit();
        assert_eq!(
            bo.host.run_triggers(ten),
            1,
            "and on the other, before it heard"
        );
        bo.commit();

        ada.sync_with(&bo);
        for replica in [&ada, &bo] {
            let told = notices(&replica.host);
            assert_eq!(told.len(), 1, "one notice between them: {told:#?}");
            assert_eq!(told[0].0, "bo", "for the task's assignee");
        }
    }
}

#[test]
fn a_notice_reaches_the_devices_of_the_person_it_names_and_no_one_else() {
    let (mut ada, her_notices, clock) = ada_workspace();
    let call = task(&mut ada, "Call the landlord");

    // Both of Bo's devices are there before its moment, having looked: nothing yet.
    let before = document_of(&ada);
    let (mut laptop, laptop_notices) = service("bo", "bo", &before);
    let (mut phone, phone_notices) = service("bo", "bo", &before);
    laptop.poll();
    phone.poll();

    // The moment comes on the workspace's device; each of Bo's takes in what it wrote, and
    // shows it, saying what the task is and when it is due.
    remind(&mut ada, &call, &clock);
    laptop.take_in(&document_of(&ada));
    phone.take_in(&document_of(&ada));
    for notices in [&laptop_notices, &phone_notices] {
        let shown = notices.shown.borrow();
        assert_eq!(shown.len(), 1, "one notice: {shown:#?}");
        assert_eq!(shown[0].title, "Call the landlord");
        assert_eq!(shown[0].body, "Due 2026-10-27");
        let opens = shown[0].place.as_ref().expect("it opens what it names");
        assert_eq!(opens.app, TASKS_APP);
        assert_eq!(opens.resource, call);
    }

    // Ada's device tells nothing: the notice is Bo's, though her own ran the trigger.
    ada.poll();
    assert!(
        her_notices.shown.borrow().is_empty(),
        "not hers to see: {:#?}",
        her_notices.shown.borrow()
    );

    // The device that ran the trigger is the one that told them once, not each of the three.
    assert_eq!(notices(ada.host()).len(), 1, "written once");
}

#[test]
fn dismissing_a_notice_on_one_device_clears_it_on_the_others() {
    let (mut ada, _, clock) = ada_workspace();
    let call = task(&mut ada, "Call the landlord");
    let before = document_of(&ada);
    let (mut laptop, laptop_notices) = service("bo", "bo", &before);
    let (mut phone, phone_notices) = service("bo", "bo", &before);
    laptop.poll();
    phone.poll();
    remind(&mut ada, &call, &clock);
    laptop.take_in(&document_of(&ada));
    phone.take_in(&document_of(&ada));
    let shown = laptop_notices.shown.borrow().clone();
    assert_eq!(shown.len(), 1);
    assert_eq!(phone_notices.shown.borrow().len(), 1, "both showed it");

    // Bo dismisses it on their laptop: the document says it is seen, as their own change.
    laptop.notice_seen(&id_of(&shown[0].tag));
    phone.take_in(&document_of(&laptop));

    // Their phone takes the notification back, and shows it no more.
    assert_eq!(
        phone_notices.withdrawn.borrow().clone(),
        vec![shown[0].tag.clone()],
        "taken back"
    );
    assert_eq!(
        phone_notices.shown.borrow().len(),
        1,
        "shown once, then cleared"
    );
}

#[test]
fn dismissing_the_banner_marks_the_notice_seen_and_letting_it_go_does_not() {
    let (mut ada, _, clock) = ada_workspace();
    let call = task(&mut ada, "Call the landlord");
    let before = document_of(&ada);
    let (mut laptop, laptop_notices) = service("bo", "bo", &before);
    let (mut phone, phone_notices) = service("bo", "bo", &before);
    laptop.poll();
    phone.poll();
    remind(&mut ada, &call, &clock);
    laptop.take_in(&document_of(&ada));
    phone.take_in(&document_of(&ada));
    let shown = laptop_notices.shown.borrow().clone();
    assert_eq!(shown.len(), 1);
    assert_eq!(phone_notices.shown.borrow().len(), 1, "both showed it");

    // The banner runs out on its own: the person did not answer it, so the notice is still
    // theirs to see, and their phone keeps showing it.
    laptop_notices.close(&shown[0].tag, Closed::Expired);
    laptop.poll();
    phone.take_in(&document_of(&laptop));
    assert!(
        phone_notices.withdrawn.borrow().is_empty(),
        "an expiry answers nothing: {:#?}",
        phone_notices.withdrawn.borrow()
    );

    // Nor does this device taking the banner back, as it does for a notice another of their
    // devices has seen.
    laptop_notices.close(&shown[0].tag, Closed::Withdrawn);
    laptop.poll();
    phone.take_in(&document_of(&laptop));
    assert!(
        phone_notices.withdrawn.borrow().is_empty(),
        "taking one back answers nothing: {:#?}",
        phone_notices.withdrawn.borrow()
    );

    // Bo dismisses the banner themselves: the notice is seen, as their own change, and their
    // phone takes it back.
    laptop_notices.close(&shown[0].tag, Closed::Dismissed);
    laptop.poll();
    phone.take_in(&document_of(&laptop));
    assert_eq!(
        phone_notices.withdrawn.borrow().clone(),
        vec![shown[0].tag.clone()],
        "a dismissal is the person's answer"
    );
    assert_eq!(
        phone_notices.shown.borrow().len(),
        1,
        "shown once, then cleared"
    );
}

#[test]
fn a_replay_or_a_late_joining_device_tells_nothing_again() {
    let (mut ada, _, clock) = ada_workspace();
    let call = task(&mut ada, "Call the landlord");

    // Bo's phone is there before the moment, having looked: nothing yet.
    let early = document_of(&ada);
    let (mut phone, phone_notices) = service("bo", "bo", &early);
    phone.poll();
    assert!(phone_notices.shown.borrow().is_empty());

    // The moment comes, and the phone takes in what the workspace wrote: this is news to it.
    remind(&mut ada, &call, &clock);
    let record = document_of(&ada);
    phone.take_in(&document_of(&ada));
    assert_eq!(phone_notices.shown.borrow().len(), 1, "news to this device");

    // A device that restarts having told it keeps that, and tells nothing again.
    let (mut restarted, again) = service("bo", "bo", &record);
    restarted.take_up_told(phone.told_kept());
    restarted.poll();
    assert!(
        again.shown.borrow().is_empty(),
        "delivered before, and kept"
    );

    // A device that joins after the document holds it tells nothing of it: what the document held
    // when it first looked is not news.
    let (mut joining, late) = service("bo", "bo", &record);
    joining.poll();
    assert!(
        late.shown.borrow().is_empty(),
        "nothing new to a late joiner: {:#?}",
        late.shown.borrow()
    );

    // Nor does another device's turn run the trigger a second time.
    joining.take_in(&document_of(&ada));
    assert_eq!(notices(ada.host()).len(), 1, "one notice in the workspace");
}

#[test]
fn what_a_notice_opens_resolves_from_another_device() {
    let (mut ada, _, clock) = ada_workspace();
    let call = task(&mut ada, "Call the landlord");
    let before = document_of(&ada);
    let (mut phone, phone_notices) = service("bo", "bo", &before);
    phone.poll();
    remind(&mut ada, &call, &clock);
    phone.take_in(&document_of(&ada));
    let shown = phone_notices.shown.borrow().clone();
    let place = place(&shown[0]);
    assert_eq!(place.app, TASKS_APP);
    assert_eq!(place.resource, call);
    assert_eq!(
        tilo_service::notice::Place::from_activation(&place.activation()),
        Some(place),
        "choosing it opens what it names"
    );
}

/// The notices the document holds: each one's person, what it says, and the resource it opens.
fn notices(host: &Host) -> Vec<(String, String, String, String)> {
    host.store()
        .resources_of(tilolang::NOTICE_TYPE)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            let at = tilolang_runtime::ResourceRef::root(id, tilolang::NOTICE_TYPE.to_owned());
            let read = |field| host.runtime().read(&at, field).ok();
            let to = match read(tilolang::NOTICE_TO)? {
                tilolang_runtime::Value::Actor(person) => person.id().to_owned(),
                _ => return None,
            };
            let tilolang_runtime::Value::Text(title) = read(tilolang::NOTICE_TITLE)? else {
                return None;
            };
            let body = match read(tilolang::NOTICE_BODY) {
                Some(tilolang_runtime::Value::Text(text)) => text,
                _ => String::new(),
            };
            let opens = match read(tilolang::NOTICE_OPENS)? {
                tilolang_runtime::Value::Ref(reference) => reference.target.id_string(),
                _ => return None,
            };
            Some((to, title, body, opens))
        })
        .collect()
}
