//! What this page's windows read of the records other apps keep.
//!
//! A window reads another app's record through its session's record API, as on a native desktop,
//! where the workspace broker decides each read and captures the record. A page hosts its windows
//! itself and holds the records they read: the ones its replication took in from the workspace, or
//! the ones of a desktop of the person's own. So the page answers
//! ([`tilo_desktop_surface::session::RecordAnswers`]), from what it holds, as the same policy
//! decides it: the person's access at the record's node, a member's own record theirs alone
//! ([`ui::permissions::PermissionContext::answer`]). A read the broker would refuse is refused
//! here; one of a record still arriving, or asked before the workspace said what this person may
//! do, waits for it.
//!
//! A read held for a write costs a frame one comparison of the record's heads and is read again
//! only once they moved. A page none of whose windows reads another app's records does nothing.
//!
//! A page that shows one app alone has no desktop, and its window asks through the page's own
//! connection, which carries no read. Such a mount answers the same way ([`MountReads`]): the
//! same session takes the reads, and the same policy ([`Held`]) decides each from what the page
//! holds, for the person at the page and nobody else. The peer's code makes it when the window
//! first reads, and the start module reaches it through [`crate::replicated`] alone, so the
//! session and what a reading is made of load after the start.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::remote::{
    ActionCallError, Caller, RemoteActionMessage, ResourceAddress, ResourceCopy,
};
use tilo_desktop_surface::session::{HeldRecords, Openings, RecordAnswers};
use ui::automerge::AutoCommit;
use ui::automerge::transaction::Transactable as _;
use ui::permissions::SharedPermissions;
use ui::resource::ResourceStore;

use super::super::{Desktop, Mount};

/// The page's end of its windows' record reads.
pub(super) struct Reads {
    answers: RefCell<RecordAnswers>,
    /// Where the page keeps the records no window has open.
    store: Rc<dyn durable_store::DurableStore>,
    /// Whether the store keeps a record no window has open, by app and route, on a page with no
    /// workspace host: nothing writes such a record until a window opens it, so it is looked up
    /// once.
    kept: RefCell<HashMap<(String, String), bool>>,
    /// When the timer that takes the next pass fires, while a read's wait is still to end.
    due: Cell<Option<ui::time::Instant>>,
}

impl Reads {
    /// Answer the record reads the windows ask of the session whose desktop end is `openings`,
    /// from the records in `store` and the ones the windows hold.
    pub(super) fn new(openings: &Openings, store: Rc<dyn durable_store::DurableStore>) -> Self {
        Self {
            answers: RefCell::new(openings.answer_records()),
            store,
            kept: RefCell::default(),
            due: Cell::new(None),
        }
    }
}

impl Desktop {
    /// Answer what the windows asked to read since the last pass, and every read held for a write
    /// whose record moved or whose wait ended. Called on every frame of mount `id` and when a wait
    /// ends with no frame in between.
    pub(super) fn answer_record_reads(&self, id: u64) {
        // A reader asks again as it is answered: that ask is taken by the pass that answers it.
        let Ok(mut answers) = self.asks.records.answers.try_borrow_mut() else {
            return;
        };
        if !answers.has_work() {
            return;
        }
        let now = ui::time::now();
        let due = answers.answer(&self.held(), now);
        drop(answers);
        if let Some(due) = due {
            arm(&self.asks.records, id, due, now);
        }
    }

    /// What this desktop's page holds, as its person may read it.
    fn held(&self) -> Held<'_> {
        Held {
            person: &self.author,
            workspace: &self.workspace,
            peer: self.asks.peer(),
            role: self.shared.role.code(),
            permissions: &self.permissions,
            stores: &self.shared.stores,
            reads: &self.asks.records,
        }
    }
}

/// Take mount `id`'s next pass when `due` comes with no frame in between, unless a timer armed
/// before fires by then.
fn arm(reads: &Reads, id: u64, due: ui::time::Instant, now: ui::time::Instant) {
    if reads
        .due
        .get()
        .is_some_and(|armed| armed > now && armed <= due)
    {
        return;
    }
    reads.due.set(Some(due));
    let wait = due.saturating_duration_since(now).as_millis();
    let wait = u32::try_from(wait).unwrap_or(u32::MAX).saturating_add(1);
    wasm_bindgen_futures::spawn_local(async move {
        super::super::frame_loop::sleep_ms(wait).await;
        // A desktop's pass asks for no frame: it draws nothing, and a reader it answers with
        // something new asks for its own. A mount of one app asks for one when it answered.
        let _ = super::super::with_mount_quietly(id, |mount| {
            if let Some(desktop) = &mount.desktop {
                desktop.answer_record_reads(id);
                return Ok(());
            }
            // Through the table the peer's code filled, as every call from the start module.
            crate::replicated::answer_reads_alone(mount);
            Ok(())
        });
    });
}

/// What a mount of one app alone answers its window that reads another app's document.
///
/// The window asks through the page's own connection, which carries actions to the workspace and
/// no read. The mount takes each read out of what the window asked, puts it to a session of its
/// own as a desktop's windows put theirs, and answers it on its own thread from what the page
/// holds, as [`Held`] decides it: for the person at the page, in the workspace the page shows.
/// The answer goes back to the window as the collaboration event its client waits for.
///
/// The peer's code makes one when a mount's window first reads ([`Mount::answer_reads_alone`]),
/// and the mount keeps it as a value whose type the start module never names. So the session,
/// what a reading is made of and everything else below are reached through
/// [`crate::replicated`] alone and load with the peer's code: a page whose window reads nothing
/// of another app makes none, and what a page loads first holds none of it.
struct MountReads {
    /// The session the reads are put to. It serves nothing else here.
    session: std::sync::Arc<tilo_desktop_surface::session::DesktopSession>,
    reads: Reads,
    /// The person at the page.
    person: String,
    workspace: String,
    /// The peer replicating the workspace.
    peer: u32,
    /// What the person may do here, as the workspace answered it for this mount.
    permissions: SharedPermissions,
    /// The stores this mount's window holds open.
    stores: Rc<RefCell<Vec<ResourceStore>>>,
    /// The answers made since the window was last handed them.
    answered: std::sync::Arc<std::sync::Mutex<Vec<RemoteActionMessage>>>,
}

impl MountReads {
    /// The reads of `mount`, for the person the workspace's replication holds it for and with
    /// what the workspace lets them do there. `None` for a mount that replicates no workspace.
    fn of(mount: &Mount) -> Option<Self> {
        let peer = mount.peer?;
        let (person, permissions) = crate::replication::reader(peer, mount.id)?;
        // The mount takes its window's reads itself, so nothing waits on the session's own wake.
        let (session, openings) = tilo_desktop_surface::session::serve(
            mount.workspace.clone(),
            std::sync::Arc::default(),
        );
        let store: Rc<dyn durable_store::DurableStore> = mount.store.clone();
        Some(Self {
            reads: Reads::new(&openings, store),
            session,
            person,
            workspace: mount.workspace.clone(),
            peer,
            permissions,
            stores: Rc::clone(&mount.stores),
            answered: std::sync::Arc::default(),
        })
    }

    /// What this mount's page holds, as its person may read it.
    fn held(&self) -> Held<'_> {
        Held {
            person: &self.person,
            workspace: &self.workspace,
            peer: Some(self.peer),
            role: crate::replication::role_code(self.peer),
            permissions: &self.permissions,
            stores: &self.stores,
            reads: &self.reads,
        }
    }

    /// Put `asked`, the reads the window asked since the last pass, to the session, and answer
    /// them and every read held for a write whose document moved or whose wait ended. The
    /// answers made, and when the next pass is due though nothing moves.
    fn answer(
        &self,
        asked: Vec<RemoteActionMessage>,
        now: ui::time::Instant,
    ) -> (Vec<RemoteActionMessage>, Option<ui::time::Instant>) {
        if !asked.is_empty() {
            // As a desktop's windows are served: the person at this page, in its workspace.
            let caller =
                Caller::local_person(self.person.clone()).in_workspace(self.workspace.clone());
            let api = tilo_actions::remote::ActionProvider::resource_reads(&*self.session);
            for message in asked {
                let RemoteActionMessage::ResourceRead {
                    correlation, read, ..
                } = message
                else {
                    continue;
                };
                let Some(api) = api.clone() else {
                    continue;
                };
                let answered = std::sync::Arc::clone(&self.answered);
                tilo_actions::remote::ResourceReads::answer_read(api, &caller, correlation, read)
                    .then(move |answer| {
                        if let Some(answer) = answer {
                            answered
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(answer);
                        }
                    });
            }
        }
        let due = match self.reads.answers.try_borrow_mut() {
            Ok(mut answers) if answers.has_work() => answers.answer(&self.held(), now),
            _ => None,
        };
        let answers = std::mem::take(
            &mut *self
                .answered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        (answers, due)
    }
}

impl Mount {
    /// Answer what this mount's one app asked to read of other apps' documents since the last
    /// pass, and every read held for a write that landed since. Called when the page took what
    /// the window asked, on every frame the mount draws, and when a wait ends with no frame in
    /// between.
    ///
    /// The start module calls it through [`crate::replicated::answer_reads_alone`] only, and
    /// only for a mount that shows one app alone: this and all it reaches is the peer's code.
    pub(crate) fn answer_reads_alone(&mut self) {
        let asked = self.media.take_resource_reads();
        if self.reads.is_none() {
            if asked.is_empty() {
                return;
            }
            // The window reads for the first time: what answers it is made now.
            let Some(reads) = MountReads::of(self) else {
                self.hear_unread(asked);
                return;
            };
            self.reads = Some(Box::new(reads));
        }
        let Some(reads) = self
            .reads
            .as_deref()
            .and_then(|reads| reads.downcast_ref::<MountReads>())
        else {
            return;
        };
        let now = ui::time::now();
        let (answers, due) = reads.answer(asked, now);
        if let Some(due) = due {
            arm(&reads.reads, self.id, due, now);
        }
        if answers.is_empty() {
            return;
        }
        for message in answers {
            self.frame
                .handle_collaboration_event(&ui::collaboration::CollabEvent::RemoteAction {
                    message,
                });
        }
        // What the window read shows on its next frame, whose status says what changed.
        self.entries = self.entries.wrapping_add(1);
        self.pending = true;
        self.window.request_redraw();
    }

    /// Tell the window that nothing here answers `asked`, its reads of other apps' documents:
    /// the workspace's replication does not hold this mount.
    fn hear_unread(&mut self, asked: Vec<RemoteActionMessage>) {
        for message in asked {
            let RemoteActionMessage::ResourceRead { correlation, .. } = message else {
                continue;
            };
            self.frame
                .handle_collaboration_event(&ui::collaboration::CollabEvent::RemoteAction {
                    message: RemoteActionMessage::ResourceReading {
                        correlation,
                        result: Err(ActionCallError::Unavailable {
                            reason: "another app's data cannot be read here".into(),
                        }),
                    },
                });
        }
        self.pending = true;
        self.window.request_redraw();
    }
}

/// What this page holds of its workspace's records, as its person may read them: the same for a
/// desktop and for a mount of one app, so one policy decides both.
struct Held<'a> {
    /// The person at this page: a read is theirs or nobody's.
    person: &'a str,
    workspace: &'a str,
    /// The peer replicating the workspace, if the page has one.
    peer: Option<u32>,
    /// The role the workspace answered this person holds, as
    /// [`crate::desktop::SharedRole::code`] counts it: 0 until it has.
    role: u8,
    permissions: &'a SharedPermissions,
    /// The stores the page's windows hold open.
    stores: &'a RefCell<Vec<ResourceStore>>,
    reads: &'a Reads,
}

impl Held<'_> {
    /// `read` of the record `address` as a window of its app holds it open, if one does.
    fn open<R>(&self, address: &ResourceAddress, read: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        let id = ui::resource::DocumentId::from_route(&address.record)?;
        let stores = self.stores.borrow();
        stores
            .iter()
            .find(|store| {
                let (app, home) = store.app_and_home();
                app == address.app && home == self.workspace
            })?
            .read(&id, read)
    }

    /// The record `address` as its app last committed it where this page keeps it, when no window
    /// holds it open and no workspace host sends it.
    fn stored(&self, address: &ResourceAddress) -> Option<AutoCommit> {
        tilo_session::platform::records::document(
            &*self.reads.store,
            &address.app,
            self.workspace,
            &address.record,
        )
    }

    /// The record `address` as this page holds it: what the workspace's replication holds, on a
    /// page with a workspace host, and else what a window has open or the page's store keeps.
    /// Without what is still being typed, as a read answers committed content.
    fn committed(&self, address: &ResourceAddress) -> Option<Vec<u8>> {
        let mut record = match self.peer {
            Some(peer) => {
                crate::replicated::resource_document(peer, &address.app, &address.record)?
            }
            None => self
                .open(address, Clone::clone)
                .or_else(|| self.stored(address))?,
        };
        if record.pending_ops() > 0 {
            record.rollback();
        }
        Some(record.save())
    }
}

impl HeldRecords for Held<'_> {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let hosted = self.peer.is_some();
        // Who may read is decided before anything is read ([`crate::read_bounds`]).
        crate::read_bounds::Reader {
            person: self.person,
            workspace: self.workspace,
            joined: hosted,
            role: self.role,
            permissions: self.permissions,
        }
        .may_read(caller, address)?;
        match self.committed(address) {
            Some(record) => Ok(ResourceCopy::Document(record)),
            None if hosted => Err(ActionCallError::Unavailable {
                reason: "the record is still arriving from the workspace".into(),
            }),
            None => Ok(ResourceCopy::Absent),
        }
    }

    fn mark(&self, address: &ResourceAddress) -> Option<Vec<u8>> {
        // A record a window holds open moves with its committed heads, whoever wrote it.
        let heads = self.open(address, AutoCommit::base_heads).or_else(|| {
            // One no window holds moves only as the workspace delivers it.
            crate::replicated::record_heads(self.peer?, &address.app, &address.record)
        });
        if let Some(heads) = heads {
            return Some(heads.iter().flat_map(|head| head.0).collect());
        }
        if self.peer.is_some() {
            return None;
        }
        let record = (address.app.clone(), address.record.clone());
        let kept = *self
            .reads
            .kept
            .borrow_mut()
            .entry(record)
            .or_insert_with(|| self.stored(address).is_some());
        kept.then(Vec::new)
    }

    fn authority(&self) -> u64 {
        ui::permissions::shared_revision()
            .wrapping_mul(8)
            .wrapping_add(u64::from(self.role))
    }

    fn moved(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut moved = std::collections::hash_map::DefaultHasher::new();
        for store in self.stores.borrow().iter() {
            store.kept_revision().hash(&mut moved);
            store.content_revision().hash(&mut moved);
        }
        crate::replicated::changes()
            .moved_in(self.workspace)
            .hash(&mut moved);
        moved.finish()
    }
}
