//! Outbox: entries a verb queues, settled with a typed result by the device
//! that owns them, forgotten as housekeeping, and the undo of a queueing
//! verb queueing its compensation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use tilo_actions::{Action as _, ActionKind, ChangeKind};
use tilo_resource::{
    ActionCx, Collection, Edit, Id, Invoke, Model, Outbox, Refusal, Replica, Resource, Settled,
    Undoing, resource,
};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    #[model(label)]
    pub title: String,
}

/// A filesystem change for the device whose disk it names.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct FileOp {
    pub device: String,
    pub name: String,
    /// The entry this one reverts.
    pub reverts: Option<Id>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Desk {
    pub notes: Collection<Note>,
    /// Settled with where the file went.
    pub ops: Outbox<FileOp, String>,
}

#[resource(kind = "test.desk", label = "Desk", params = desk)]
impl Desk {
    /// Add a note.
    #[action]
    pub fn add_note(&mut self, cx: &mut ActionCx, title: String) {
        self.notes.push(cx.id(), Note { title });
    }

    /// Remove a note.
    #[action]
    pub fn remove_note(&mut self, notes: &Id) {
        self.notes.remove(notes);
    }

    /// Move a file to the trash.
    #[action(undo = Self::untrash)]
    pub fn trash(&mut self, cx: &mut ActionCx, ops: &mut Outbox<FileOp, String>, name: String) {
        ops.push(
            cx.id(),
            FileOp {
                device: "laptop".into(),
                name,
                reverts: None,
            },
        );
    }

    fn untrash(undoing: Undoing<'_, Self, desk::Trash>) -> Option<Invoke<Self>> {
        Some(Self::REVERT.with(undoing.minted.first()?.clone()))
    }

    /// Put back what a file operation changed.
    #[action(item = ops)]
    pub fn revert(
        &mut self,
        cx: &mut ActionCx,
        ops: &mut Outbox<FileOp, String>,
        entry: Id,
    ) -> Result<(), Refusal> {
        let queued = ops.get(&entry).ok_or("no such operation")?;
        let Some(settled) = &queued.settled else {
            // Not carried out yet: it never will be.
            ops.forget(&entry);
            return Ok(());
        };
        let undo = FileOp {
            device: queued.value.device.clone(),
            name: settled.result().clone(),
            reverts: Some(entry),
        };
        ops.push(cx.id(), undo);
        Ok(())
    }

    /// Record that the device found a file already gone.
    #[action(external, item = ops)]
    pub fn found_gone(&mut self, entry: Id) {
        self.ops.settle(&entry, Settled::failed("already gone"));
        self.ops.prune(1);
    }
}

fn edit(replica: &mut Replica<Desk>, edit: Edit<Desk>) -> ChangeKind {
    let action = Desk::edit_action(edit).unwrap();
    let change = action.change_kind();
    replica.apply_action(&action).unwrap();
    change
}

fn trashed(replica: &mut Replica<Desk>, name: &str) -> (Id, tilo_actions::ActionEnvelope) {
    let envelope = replica.apply(Desk::TRASH.with(name)).unwrap().clone();
    let state = replica.state().unwrap();
    let (id, _) = state
        .ops
        .entries()
        .find(|(_, entry)| entry.value.name == name)
        .unwrap();
    (id.clone(), envelope)
}

#[test]
fn an_entry_settles_with_its_result_as_nobodys_step() {
    let mut desk = Replica::<Desk>::new();
    let (id, envelope) = trashed(&mut desk, "a.txt");
    assert_eq!(
        envelope.context.as_ref().unwrap().kind,
        Some(ActionKind::Outside)
    );
    // A pending entry is not forgotten by an edit.
    let action = Desk::edit_action(Desk::paths().ops.forget(&id)).unwrap();
    assert!(desk.apply_action(&action).is_err());

    let settle = Desk::paths()
        .ops
        .settle(&id, &Settled::Done("Trash/a.txt".to_owned()));
    assert_eq!(edit(&mut desk, settle), ChangeKind::External);
    let state = desk.state().unwrap();
    assert_eq!(state.ops.pending().count(), 0);
    assert_eq!(
        state.ops.outcome(&id),
        Some(&Settled::Done("Trash/a.txt".to_owned()))
    );
    let entry = desk.log().last().unwrap().context.clone().unwrap();
    assert_eq!(entry.change, ChangeKind::External);
    let Some(tilo_actions::Described {
        what: tilo_actions::Did::Edit(recorded),
        ..
    }) = entry.described()
    else {
        panic!("a settlement is recorded as an edit of the outbox");
    };
    assert_eq!(recorded.op, tilo_actions::EditOp::Settle);
    assert_eq!(recorded.field, "ops");
    assert_eq!(recorded.args["state"].as_str(), Some("done"));

    assert_eq!(
        edit(&mut desk, Desk::paths().ops.forget(&id)),
        ChangeKind::Housekeeping
    );
    assert!(desk.state().unwrap().ops.is_empty());
}

#[test]
fn undoing_a_queueing_verb_queues_its_compensation_for_the_entry_it_minted() {
    let mut desk = Replica::<Desk>::new();
    let (id, envelope) = trashed(&mut desk, "a.txt");
    let compensation = envelope
        .context
        .as_ref()
        .and_then(|context| context.compensation.clone())
        .expect("a verb with a declared undo records its compensation");
    let undo: <Desk as Resource>::Action = serde_json::from_slice(&compensation.payload).unwrap();
    let revert = Desk::REVERT.with(id.clone()).build(0).unwrap();
    assert_eq!(
        serde_json::to_value(&undo.operation).unwrap(),
        serde_json::to_value(&revert.operation).unwrap(),
        "the compensation names the entry the verb queued"
    );

    // A peer replays the verb and checks its recorded compensation.
    let mut peer = desk.fork();
    let (_, later) = trashed(&mut desk, "b.txt");
    peer.replay(&later).unwrap();
    assert_eq!(peer.state().unwrap().ops.len(), 2);

    // The owning device carried it out; undo queues the compensation,
    // which reads where the file went.
    edit(
        &mut desk,
        Desk::paths()
            .ops
            .settle(&id, &Settled::Done("Trash/a.txt".to_owned())),
    );
    desk.apply_action(&undo).unwrap();
    let state = desk.state().unwrap();
    let restore = state
        .ops
        .pending()
        .find(|(_, op)| op.reverts.as_ref() == Some(&id))
        .map(|(_, op)| op.clone())
        .unwrap();
    assert_eq!(restore.name, "Trash/a.txt");
    assert_eq!(restore.device, "laptop");
}

#[test]
fn undoing_before_the_owner_ran_it_cancels_the_entry() {
    let mut desk = Replica::<Desk>::new();
    let (id, envelope) = trashed(&mut desk, "a.txt");
    let compensation = envelope.context.unwrap().compensation.unwrap();
    let undo: <Desk as Resource>::Action = serde_json::from_slice(&compensation.payload).unwrap();
    desk.apply_action(&undo).unwrap();
    assert!(desk.state().unwrap().ops.get(&id).is_none());
}

#[test]
fn a_verb_settles_and_prunes_and_an_entry_verb_is_no_item_verb() {
    let mut desk = Replica::<Desk>::new();
    desk.apply(Desk::ADD_NOTE.with("first")).unwrap();
    let (a, _) = trashed(&mut desk, "a.txt");
    let (b, _) = trashed(&mut desk, "b.txt");
    edit(
        &mut desk,
        Desk::paths()
            .ops
            .settle(&a, &Settled::Done("Trash/a.txt".into())),
    );
    let envelope = desk.apply(Desk::FOUND_GONE.with(b.clone())).unwrap();
    assert_eq!(
        envelope.context.as_ref().unwrap().change,
        ChangeKind::External
    );
    let state = desk.state().unwrap();
    assert!(state.ops.get(&a).is_none(), "pruned to the newest settled");
    assert_eq!(
        state.ops.outcome(&b).and_then(Settled::reason),
        Some("already gone")
    );

    let tree = desk.published().unwrap();
    let note = tree
        .children
        .iter()
        .find(|child| child.id.as_str().contains("/notes/"))
        .expect("the note is an item resource");
    assert!(
        note.actions
            .iter()
            .all(|spec| spec.id != "revert" && spec.id != "found-gone"),
        "a verb on an outbox entry is no verb of the record's one collection"
    );
    assert!(tree.actions.iter().any(|spec| spec.id == "revert"));
}

/// An event on a calendar a server keeps, or on this device only.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Event {
    #[model(label)]
    pub title: String,
    pub remote: bool,
}

/// An upload of an event as it is now.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Upload {
    pub event: Id,
    pub title: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Planner {
    pub events: Collection<Event>,
    pub uploads: Outbox<Upload>,
}

#[resource(kind = "test.planner", label = "Planner", params = planner, queue = Planner::queue)]
impl Planner {
    /// Add an event.
    #[action]
    pub fn add_event(&mut self, cx: &mut ActionCx, title: String, remote: bool) {
        self.events.push(cx.id(), Event { title, remote });
    }

    /// Import a calendar's events, one undo step.
    #[action]
    pub fn import_events(&mut self, cx: &mut ActionCx, titles: Vec<String>) {
        for title in titles {
            self.events.push(
                cx.id(),
                Event {
                    title,
                    remote: true,
                },
            );
        }
    }

    /// Take the events a server sent.
    #[action(external)]
    pub fn ingest(&mut self, cx: &mut ActionCx, titles: Vec<String>) {
        for title in titles {
            self.events.push(
                cx.id(),
                Event {
                    title,
                    remote: false,
                },
            );
        }
    }

    /// Every change of a remote event goes to its server.
    fn queue(&mut self, before: &Self, cx: &mut ActionCx) {
        let changed: Vec<(Id, String)> = self
            .events
            .iter()
            .filter(|(id, event)| event.remote && before.events.get(id) != Some(*event))
            .map(|(id, event)| (id.clone(), event.title.clone()))
            .collect();
        for (event, title) in changed {
            self.uploads.push(cx.id(), Upload { event, title });
        }
    }
}

fn uploads(planner: &Replica<Planner>) -> Vec<String> {
    planner
        .state()
        .unwrap()
        .uploads
        .entries()
        .map(|(_, entry)| entry.value.title.clone())
        .collect()
}

#[test]
fn every_change_of_a_record_that_queues_reaches_outside_and_undoes_through_apply() {
    let mut planner = Replica::<Planner>::new();
    let added = planner
        .apply(Planner::ADD_EVENT.with(("Standup".to_owned(), true)))
        .unwrap()
        .clone();
    assert_eq!(
        added.context.as_ref().unwrap().kind,
        Some(ActionKind::Outside)
    );
    planner
        .apply(Planner::ADD_EVENT.with(("Lunch".to_owned(), false)))
        .unwrap();
    assert_eq!(uploads(&planner), ["Standup"]);

    // A field edit is a change like any: the hook queues the upload.
    let state = planner.state().unwrap();
    let (standup, _) = state
        .events
        .iter()
        .find(|(_, event)| event.title == "Standup")
        .unwrap();
    let (lunch, _) = state
        .events
        .iter()
        .find(|(_, event)| event.title == "Lunch")
        .unwrap();
    let rename = Planner::edit_action(
        Planner::paths()
            .events
            .item(standup)
            .title
            .replace("Daily standup"),
    )
    .unwrap();
    assert_eq!(Planner::kind_of(&rename), ActionKind::Outside);
    let renamed = planner.apply_action(&rename).unwrap().clone();
    assert_eq!(uploads(&planner), ["Standup", "Daily standup"]);
    // An event kept on this device only queues nothing.
    planner
        .apply_action(
            &Planner::edit_action(Planner::paths().events.item(lunch).title.replace("Brunch"))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(uploads(&planner), ["Standup", "Daily standup"]);

    // Undo is the edit back, through apply, so the server gets it too.
    let undo = renamed.context.unwrap().compensation.unwrap();
    let undo: <Planner as Resource>::Action = serde_json::from_slice(&undo.payload).unwrap();
    planner.apply_action(&undo).unwrap();
    let state = planner.state().unwrap();
    assert_eq!(state.events.get(standup).unwrap().title, "Standup");
    assert_eq!(uploads(&planner), ["Standup", "Daily standup", "Standup"]);

    // A settlement queues nothing and is nobody's step.
    let (first, _) = state.uploads.entries().next().unwrap();
    let settle =
        Planner::edit_action(Planner::paths().uploads.settle(first, &Settled::Done(()))).unwrap();
    assert_eq!(settle.change_kind(), ChangeKind::External);
    let settled = planner.apply_action(&settle).unwrap().clone();
    assert!(settled.context.unwrap().compensation.is_none());
    assert_eq!(uploads(&planner).len(), 3);
}

#[test]
fn content_from_outside_compensates_nothing_however_much_it_wrote() {
    let mut planner = Replica::<Planner>::new();
    let titles: Vec<String> = (0..3000)
        .map(|n| format!("Pulled event number {n}"))
        .collect();
    let ingested = planner
        .apply(Planner::INGEST.with(titles))
        .expect("a large ingest is not held to the history budget")
        .clone();
    let context = ingested.context.as_ref().unwrap();
    assert!(context.compensation.is_none(), "nobody undoes it");
    assert_eq!(planner.state().unwrap().events.len(), 3000);
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Runs {
    /// What the device runs; changed by verbs, settled by the device.
    #[model(readonly)]
    pub queue: Outbox<String>,
}

#[resource(kind = "test.runs", label = "Runs")]
impl Runs {
    /// Run a command.
    #[action]
    pub fn run(&mut self, cx: &mut ActionCx, queue: &mut Outbox<String>, command: String) {
        queue.push(cx.id(), command);
    }
}

#[test]
fn a_read_only_outbox_takes_its_settlement() {
    let mut runs = Replica::<Runs>::new();
    runs.apply(Runs::RUN.with("ls")).unwrap();
    let entry = runs
        .state()
        .unwrap()
        .queue
        .entries()
        .next()
        .unwrap()
        .0
        .clone();
    assert!(
        runs.apply(Runs::paths().queue.forget(&Id::from_raw("nothing")))
            .is_err(),
        "nothing to forget"
    );
    runs.apply(Runs::paths().queue.settle(&entry, &Settled::Done(())))
        .expect("the device settles the entry");
    assert!(
        runs.state()
            .unwrap()
            .queue
            .get(&entry)
            .unwrap()
            .settled
            .is_some()
    );
}

#[test]
fn a_change_too_large_to_record_its_restoring_edits_still_undoes_through_apply() {
    let mut planner = Replica::<Planner>::new();
    let mut peer = planner.fork();
    let titles: Vec<String> = (0..3000)
        .map(|n| format!("Imported event {n:04}"))
        .collect();
    let imported = planner
        .apply(Planner::IMPORT_EVENTS.with(titles))
        .unwrap()
        .clone();
    assert_eq!(planner.state().unwrap().events.len(), 3000);
    assert_eq!(uploads(&planner).len(), 3000);

    // Its 3000 restoring edits do not fit the history: the entry names the
    // change instead, and a peer replaying it computes the same.
    let compensation = imported.context.clone().unwrap().compensation.unwrap();
    assert_eq!(compensation.type_id, "test.planner+revert");
    assert!(serde_json::to_vec(&compensation).unwrap().len() < 1024);
    peer.replay(&imported).unwrap();
    assert_eq!(peer.state().unwrap().events.len(), 3000);

    // Undo derives the edits from the change and runs the record's queue.
    let mut registry = tilo_actions::ActionRegistry::new();
    tilo_resource::group::register::<Planner>(&mut registry);
    let undo = registry.decode(&compensation).unwrap();
    undo.apply(planner.doc_mut()).unwrap();
    planner.doc_mut().commit();
    let state = planner.state().unwrap();
    assert!(state.events.is_empty(), "the import is taken back");
    assert_eq!(
        state.uploads.entries().count(),
        3000,
        "taking events back queues nothing new for them"
    );
}
