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
    let undo: <Desk as Resource>::Action =
        serde_json::from_slice(&compensation.undo.payload).unwrap();
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
    let undo: <Desk as Resource>::Action =
        serde_json::from_slice(&compensation.undo.payload).unwrap();
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
