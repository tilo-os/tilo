//! What this page's windows read of the records other apps keep.
//!
//! A window reads another app's record through its session's record API, as on a native desktop,
//! where the workspace broker decides each read and captures the record. A page hosts its windows
//! itself and holds the records they read: the ones its replication took in from the workspace, or
//! the ones of a desktop of the person's own. So the page answers
//! ([`tilo_desktop_surface::session::RecordAnswers`]), from what it holds, as the same policy
//! decides it: the person's standing at the record's node, a member's own record theirs alone
//! ([`ui::permissions::PermissionContext::answer`]). A read the broker would refuse is refused
//! here; one of a record still arriving, or asked before the workspace said what this person may
//! do, waits for it.
//!
//! A read held for a write costs a frame one comparison of the record's heads and is read again
//! only once they moved. A page none of whose windows reads another app's records does nothing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::remote::{
    ActionHostError, Caller, RESOURCE_READ_REFUSED, RESOURCE_READS_FOR_AGENTS, ResourceAddress,
    ResourceCopy,
};
use tilo_desktop_surface::session::{HeldRecords, Openings, RecordAnswers};
use ui::automerge::AutoCommit;
use ui::automerge::transaction::Transactable as _;

use super::super::Desktop;

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
        let due = answers.answer(&Held { desktop: self }, now);
        drop(answers);
        let Some(due) = due else {
            return;
        };
        if self
            .asks
            .records
            .due
            .get()
            .is_some_and(|armed| armed > now && armed <= due)
        {
            return;
        }
        self.asks.records.due.set(Some(due));
        let wait = due.saturating_duration_since(now).as_millis();
        let wait = u32::try_from(wait).unwrap_or(u32::MAX).saturating_add(1);
        wasm_bindgen_futures::spawn_local(async move {
            super::super::frame_loop::sleep_ms(wait).await;
            // No frame is asked for: the pass draws nothing, and a reader it answers with
            // something new asks for its own.
            let _ = super::super::with_mount_quietly(id, |mount| {
                if let Some(desktop) = &mount.desktop {
                    desktop.answer_record_reads(id);
                }
                Ok(())
            });
        });
    }
}

/// What this page holds of its workspace's records, as its person may read them.
struct Held<'a> {
    desktop: &'a Desktop,
}

impl Held<'_> {
    /// `read` of the record `address` as a window of its app holds it open, if one does.
    fn open<R>(&self, address: &ResourceAddress, read: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        let id = ui::resource::DocumentId::from_route(&address.record)?;
        let stores = self.desktop.shared.stores.borrow();
        stores
            .iter()
            .find(|store| {
                let (app, home) = store.app_and_home();
                app == address.app && home == self.desktop.workspace
            })?
            .read(&id, read)
    }

    /// The record `address` as its app last committed it where this page keeps it, when no window
    /// holds it open and no workspace host sends it.
    fn stored(&self, address: &ResourceAddress) -> Option<AutoCommit> {
        tilo_session::platform::records::document(
            &*self.desktop.asks.records.store,
            &address.app,
            &self.desktop.workspace,
            &address.record,
        )
    }

    /// The record `address` as this page holds it: what the workspace's replication holds, on a
    /// page with a workspace host, and else what a window has open or the page's store keeps.
    /// Without what is still being typed, as a read answers committed content.
    fn committed(&self, address: &ResourceAddress) -> Option<Vec<u8>> {
        let mut record = match self.desktop.asks.peer() {
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
    ) -> Result<ResourceCopy, ActionHostError> {
        let desktop = self.desktop;
        let refused = |reason: &str| ActionHostError::Refused {
            reason: reason.into(),
        };
        // As the broker: a device hands its agents nothing here, and a read is its person's, in
        // the workspace this page shows. Nobody else asks a page's session.
        if caller.acting().is_agent() {
            return Err(refused(RESOURCE_READS_FOR_AGENTS));
        }
        let person = caller
            .acting()
            .person_id()
            .or_else(|| caller.acting_for_person());
        if person != Some(desktop.author.as_str())
            || caller.workspace() != Some(desktop.workspace.as_str())
        {
            return Err(refused(RESOURCE_READ_REFUSED));
        }
        let hosted = desktop.asks.peer().is_some();
        // What this person may read is the workspace's to say: until it has, nothing is read on a
        // guess, and the read waits as for a record still arriving.
        if hosted && desktop.shared.standing.role().is_none() {
            return Err(ActionHostError::Unavailable {
                reason: "the workspace has not said yet what you may read here".into(),
            });
        }
        // Decided at the record's node, as the broker decides it: allowed outright, a member's own
        // record theirs alone.
        let node = ui::permissions::TreePath::root()
            .app(&address.app)
            .resource(&address.record);
        if desktop
            .permissions
            .borrow()
            .answer(&node, ui::permissions::ActionKind::Read)
            != tilo_action_policy::Answer::Allow
        {
            return Err(refused(RESOURCE_READ_REFUSED));
        }
        match self.committed(address) {
            Some(record) => Ok(ResourceCopy::Document(record)),
            None if hosted => Err(ActionHostError::Unavailable {
                reason: "the record is still arriving from the workspace".into(),
            }),
            None => Ok(ResourceCopy::Absent),
        }
    }

    fn mark(&self, address: &ResourceAddress) -> Option<Vec<u8>> {
        // A record a window holds open moves with its committed heads, whoever wrote it.
        let heads = self.open(address, AutoCommit::base_heads).or_else(|| {
            // One no window holds moves only as the workspace delivers it.
            crate::replicated::record_heads(
                self.desktop.asks.peer()?,
                &address.app,
                &address.record,
            )
        });
        if let Some(heads) = heads {
            return Some(heads.iter().flat_map(|head| head.0).collect());
        }
        if self.desktop.asks.peer().is_some() {
            return None;
        }
        let record = (address.app.clone(), address.record.clone());
        let kept = *self
            .desktop
            .asks
            .records
            .kept
            .borrow_mut()
            .entry(record)
            .or_insert_with(|| self.stored(address).is_some());
        kept.then(Vec::new)
    }

    fn authority(&self) -> u64 {
        ui::permissions::shared_revision()
            .wrapping_mul(8)
            .wrapping_add(u64::from(self.desktop.shared.standing.code()))
    }

    fn moved(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut moved = std::collections::hash_map::DefaultHasher::new();
        for store in self.desktop.shared.stores.borrow().iter() {
            store.kept_revision().hash(&mut moved);
            store.content_revision().hash(&mut moved);
        }
        crate::replicated::changes()
            .moved_in(&self.desktop.workspace)
            .hash(&mut moved);
        moved.finish()
    }
}
