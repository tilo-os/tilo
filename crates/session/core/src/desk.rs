//! The desk a page keeps: one automerge document, whose own change history is what happened to the
//! desk, who did it and when.
//! A native session keeps no such document: its desk is the workspaces' layout records, which every
//! member shows ([`tilo_desktop_model::shared_record`]).
//! The desk is no second copy anywhere: the tree a host draws is a projection of this document, a
//! pass that finds the desk changed writes one change of it, and a version restored from the
//! history is another change on top. What is written is the form that opens again — a running
//! window's surface id names a buffer of the compositor that mapped it, so a tab is written as the
//! app to launch. Writing is away from the frame that changed it: a drag writes once, when over.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;
use ui::time::Instant;

use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use tilo_actions::ActionContext;
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_model::{Layout, STATE_SCHEMA_VERSION, TabId, desktop_doc, shared_record};
use ui::permissions::UserId;
use ui::state::UndoLog;

/// How long a drag or a burst of switches waits for its last frame before how the desk looks is
/// written.
const DRAG_QUIET: Duration = Duration::from_secs(2);

/// Where a host keeps the desk's document: its bytes, written off the frame that changed them.
pub trait Store {
    /// Keep `bytes` where this device keeps its desk.
    fn keep(&self, bytes: Vec<u8>);
}

/// `desktop` as the document keeps it: every window in the form that opens it again, and no window
/// that opens again in no way.
pub fn relaunchable(desktop: &Layout) -> Layout {
    let mut desktop = desktop.clone();
    desktop.schema_version = STATE_SCHEMA_VERSION;
    for workspace in &mut desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    *tab = tab.relaunchable();
                }
                // A window whose app could not be told apart from any other — the compositor named
                // none — is kept as a tab with no app to open it: nothing would ever come back into
                // it, and the tab would wait on the desk for good. What cannot open again is not
                // kept.
                group.tabs.retain(|tab| {
                    !matches!(&tab.tile, tilo_desktop_model::TileContent::LocalApp { app_id, .. }
                        if app_id.trim().is_empty())
                });
                if !group.tabs.iter().any(|tab| tab.id == group.active_tab)
                    && let Some(first) = group.tabs.first()
                {
                    group.active_tab = first.id;
                }
                let open = &group.tabs;
                group
                    .focus_history
                    .retain(|tab| open.iter().any(|candidate| candidate.id == *tab));
            }
        }
    }
    desktop
}

/// The actor `person` writes the desk's changes as: their handle itself, so a change of the desk
/// reads as theirs here without anything else naming it (`ui::history_source::actor_handle`).
pub fn actor_of_person(person: &str) -> automerge::ActorId {
    automerge::ActorId::from(person.as_bytes())
}

/// The desk's document as this device keeps it, and how the person here changed it.
pub struct Desk {
    doc: Rc<RefCell<AutoCommit>>,
    /// The desk's history as this host drives it: each change of the desk is one step of the
    /// person's, and the next undo takes back all of the last one.
    undo: RefCell<UndoLog>,
    /// The person here, whose the desk is: their changes are theirs to take back.
    person: String,
    /// Where the document is kept; a host without anywhere to keep it keeps it in memory alone.
    store: Option<Rc<dyn Store>>,
    /// When how the desk looks was last written, so a drag or a burst of switches stays one change.
    looks_at: Cell<Option<Instant>>,
}

impl Desk {
    /// The desk `document` holds, whose changes are written by `person` and kept through `store`.
    pub fn new(document: AutoCommit, person: &str, store: Option<Rc<dyn Store>>) -> Self {
        let mut document = document;
        document.set_actor(actor_of_person(person));
        let undo = UndoLog::new(&mut document);
        Self {
            doc: Rc::new(RefCell::new(document)),
            undo: RefCell::new(undo),
            person: person.to_owned(),
            store,
            looks_at: Cell::default(),
        }
    }

    /// The desk the document holds, as this host draws it, and `None` on a document that holds no
    /// workspace yet. The record each window of the desk shows, by tab, as the document keeps it: a
    /// window placed on a record names it there, so the place shows the record it was placed on.
    pub fn records(&self) -> Vec<(TabId, String)> {
        desktop_doc::records(&self.doc.borrow())
    }

    /// The desk the document holds.
    pub fn tree(&self) -> Option<Layout> {
        let doc = self.doc.borrow();
        let mut tree = Layout::default();
        desktop_doc::rebuild(&doc, &mut tree);
        desktop_doc::rebuild_view(&doc, &mut tree);
        (!tree.workspaces.is_empty()).then_some(tree)
    }

    /// Record `desktop` as it stands, at `time` (seconds since the epoch): one change of the desk,
    /// kept. `records` names the record each window shows and `authors` who made each strip and
    /// opened each window, where the host keeps either. Whether the desk changed at all.
    /// Only how the person shows the desk — a drag, a switch — is written once the drag or the
    /// burst of switches is over: a resize writes the view on every frame, and those are not
    /// changes of the desk.
    pub fn record(
        &self,
        desktop: &Layout,
        time: i64,
        records: &[(TabId, String)],
        authors: Option<&Authors>,
    ) -> bool {
        self.write(desktop, time, true, None, records, authors)
    }

    /// [`Self::record`], and write even how the desk looks now: what ends the session, which the
    /// next one starts from.
    pub fn record_now(
        &self,
        desktop: &Layout,
        time: i64,
        records: &[(TabId, String)],
        authors: Option<&Authors>,
    ) -> bool {
        self.write(desktop, time, false, None, records, authors)
    }

    /// Record `desktop` as it stands as `asked`'s arranging action: one change of the desk whose own
    /// entry says who asked and what they did, so the history names it and the step is the person's
    /// to take back. It is a change of the desk and never a frame of a drag: an action asked for is
    /// written as it is made. Whether the desk changed at all.
    pub fn record_action(
        &self,
        desktop: &Layout,
        time: i64,
        asked: &ActionContext,
        records: &[(TabId, String)],
    ) -> bool {
        self.write(desktop, time, false, Some(asked), records, None)
    }

    /// Record how this device shows `desktop` alone: a host whose workspace holds the desk itself
    /// keeps only the view here, so a reload opens the desk as the workspace has it. Whether
    /// anything was written.
    pub fn record_view(&self, desktop: &Layout, time: i64) -> bool {
        let mut doc = self.doc.borrow_mut();
        desktop_doc::seed_of_the_view(&mut doc, &relaunchable(desktop));
        if doc.pending_ops() == 0 {
            return false;
        }
        shared_record::commit_change(&mut doc, &self.person, time, None);
        self.keep(&mut doc);
        true
    }

    /// Record `desktop`, and that the windows in `records` show the records they name.
    fn write(
        &self,
        desktop: &Layout,
        time: i64,
        wait_out_a_drag: bool,
        asked: Option<&ActionContext>,
        records: &[(TabId, String)],
        authors: Option<&Authors>,
    ) -> bool {
        let mut doc = self.doc.borrow_mut();
        let before = doc.get_heads();
        // A document with no desk holds one after this change: the desk is being opened, here or as
        // it arrived from the workspace, and that is no change of the person's to take back.
        let opened = !desktop_doc::holds(&*doc);
        let looks_only = desktop_doc::seed_of_the_view(&mut doc, &relaunchable(desktop));
        // What each window shows is the desk's, not how this device shows it, and it is written like
        // the view is: what a window opened is no step of the person's on its own, and an action
        // that places a resource is a step because it was asked for. A window's record arrives with
        // the window, so a write that knows one binding is no authority over the others.
        for (tab, record) in records {
            let _ = desktop_doc::note_record(&mut doc, *tab, record);
        }
        if let Some(authors) = authors {
            desktop_doc::set_window_authors(&mut doc, &authors_on(desktop, authors));
        }
        if doc.pending_ops() == 0 {
            return false;
        }
        if looks_only && wait_out_a_drag && self.looks_kept_recently() {
            doc.rollback();
            return false;
        }
        shared_record::commit_change(&mut doc, &self.person, time, asked);
        // One change of the desk is one step of the person's: what they did here is theirs to take
        // back, whichever of their agents asked; so is a change asked for that only moves the view,
        // a width or a maximization, which is a step like any other when an action asks for it. How
        // the desk looks alone, unasked, is no step: those changes ride with the change before or
        // after them. Nor is the desk as it was opened.
        if !opened && (asked.is_some() || !looks_only) {
            self.undo.borrow_mut().record_committed_action_authored(
                &mut doc,
                before,
                None,
                None,
                Some(user(&self.person)),
            );
        }
        if looks_only {
            self.looks_at.set(Some(Instant::now()));
        }
        self.keep(&mut doc);
        true
    }

    /// Take the desk `document` holds — the workspace's, as it arrived — for this device's.
    pub fn replace(&self, document: AutoCommit) {
        let mut current = self.doc.borrow_mut();
        *current = document;
        *self.undo.borrow_mut() = UndoLog::new(&mut current);
    }

    /// Write `doc` where this device keeps it, away from the frame.
    fn keep(&self, doc: &mut AutoCommit) {
        if let Some(store) = &self.store {
            store.keep(doc.save());
        }
    }

    /// Take back `person`'s last change of the desk. Whether there was one.
    pub fn undo(&self, person: &str) -> bool {
        self.rewind(|undo, doc| undo.undo_as(doc, Some(&user(person)), None, None).is_some())
    }

    /// Bring back the last change of the desk `person` took back.
    pub fn redo(&self, person: &str) -> bool {
        self.rewind(|undo, doc| undo.redo_as(doc, Some(&user(person)), None, None).is_some())
    }

    fn rewind(&self, step: impl FnOnce(&mut UndoLog, &mut AutoCommit) -> bool) -> bool {
        let mut doc = self.doc.borrow_mut();
        let stepped = {
            let mut undo = self.undo.borrow_mut();
            step(&mut undo, &mut doc)
        };
        if stepped {
            self.keep(&mut doc);
        }
        stepped
    }

    /// The last step of the desk's history, as who made it and what it did: what the history names
    /// it by.
    pub fn last_step(&self, person: &str) -> Option<(String, String)> {
        use tilo_actions::Did;
        let doc = self.doc.borrow();
        let entry = self.undo.borrow().next_undo_as(&doc, Some(&user(person)))?;
        let step = entry.step?;
        // Who asked for it: the invoker the step records, else the person whose desk it is.
        let who = step
            .invoker
            .map_or_else(|| person.to_owned(), |invoker| invoker.key());
        let what = match step.what {
            Some(Did::Verb(call)) => call.action.action,
            _ => String::new(),
        };
        Some((who, what))
    }

    /// Whether how the desk looks was kept so lately that what is written now is still one drag or
    /// one burst of switches.
    fn looks_kept_recently(&self) -> bool {
        self.looks_at
            .get()
            .is_some_and(|at| at.elapsed() < DRAG_QUIET)
    }

    /// The document, for a reader that lists what happened to the desk.
    pub fn document(&self) -> AutoCommit {
        self.doc.borrow().clone()
    }

    /// The document itself, for a host that writes one change of the desk its own way — an
    /// arranging command it received. Whoever holds it commits the change and keeps it.
    pub fn document_mut(&self) -> std::cell::RefMut<'_, AutoCommit> {
        self.doc.borrow_mut()
    }

    /// Write the document as it stands, for a host that took another copy of it: a workspace's,
    /// mirrored into this device's.
    pub fn keep_now(&self) {
        let mut doc = self.doc.borrow_mut();
        self.keep(&mut doc);
    }
}

/// Who made each strip and opened each window still on `desktop`: what a window or a strip no
/// longer there did is no longer anyone's.
#[must_use]
pub fn authors_on(desktop: &Layout, authors: &Authors) -> Authors {
    let live: BTreeSet<u64> = desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| {
            std::iter::once(strip.id.0).chain(
                strip
                    .window_groups
                    .iter()
                    .flat_map(|group| group.tabs.iter().map(|tab| tab.id)),
            )
        })
        .collect();
    authors
        .iter()
        .filter(|(id, _)| live.contains(id))
        .map(|(id, author)| (*id, author.clone()))
        .collect()
}

/// The person `person`, as the history names them.
fn user(person: &str) -> UserId {
    UserId(person.to_owned())
}
