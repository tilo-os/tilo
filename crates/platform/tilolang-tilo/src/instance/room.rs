//! The instance's path to its room, without the toolkit: what the runtime planned is committed
//! to the room record and handed to the room through the collab client (`collaboration`), and
//! what the room's other replicas committed comes back into the record and the instance.
//!
//! An instance queues each planned change as an invocation of the host's `commit` action
//! ([`Instance::take_invocation`]). [`RoomRecord::commit`] applies each to the room record as
//! the recorded action it is ([`DynAction`]), attributed to the person at the window, and sends
//! the change on the record's route. [`RoomRecord::receive`] takes in the changes the route
//! brings, checked against the route they came on, and brings the instance up to date with the
//! record.

use std::rc::Rc;

use app_state::DocWriter;
use automerge::ChangeHash;
use collaboration::document_routes::{decode_routed_mutation, validated_route_payload_with};
use collaboration::history_source::authors::{
    HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME, actor_handle, short_id, stable_hue,
};
use collaboration::history_source::steps::{self, Scope};
use collaboration::history_source::verbs::{self, EDIT_TYPE, HistoryEdit};
use collaboration::history_source::words::HistoryWords;
use collaboration::record::RecordSaveStatus;
use collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, DocumentMutation, RouteSpec,
};
use durable_store::DurableStore;

use serde_json::Value as Json;
use tilo_actions::{ActionContext, ActionInvoker, ActionRegistry, EncodedAction};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::Doc;

use super::Instance;
use crate::action::{ACTION_TYPE, DynAction};
use crate::publish::{COMMIT, HOST_RESOURCE};

/// Why a change did not reach the room.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomError {
    /// The instance has no host over a record yet.
    #[error("the instance holds no room record yet")]
    NoHost,
    /// No committed action is queued under the ticket.
    #[error("no committed action {0}")]
    NoAction(u64),
    /// The action could not be applied to the room record.
    #[error("the action was refused: {0}")]
    Refused(String),
    /// The record could not be opened: no route, an unreadable copy, or a committed read that
    /// failed.
    #[error("the room record could not be opened: {0}")]
    Open(String),
}

/// Where the broker's committed read of a record being opened comes from.
enum Read {
    /// Read on this thread, as the record opens.
    Here,
    /// Read off this thread already; `None` when the route needs none.
    Taken(
        Option<
            Box<(
                collaboration::native_initial::PrimaryRead,
                collaboration::CommittedDocument,
            )>,
        >,
    ),
}

/// The room record an instance commits to, the route it travels on, and its history and save
/// state.
///
/// Its history is the one every app's record has (`collaboration::history_source`): each change
/// is a step of whoever made it, and Undo, Redo, History and the other history verbs are
/// [`verbs`], applied to the record as steps of their own, as a native app applies them.
pub struct RoomRecord {
    doc: Doc,
    manager: CollaborationManager,
    route: RouteSpec,
    person: String,
    /// The heads of what the room was handed: a commit sends what follows them. They move only
    /// once the collab client took the change; what it could not take yet (its queue full, the
    /// route still connecting) stays in the record past them and goes with the next send.
    sent: Vec<ChangeHash>,
    /// Why the record's changes past [`Self::sent`] stay on this device: the collab client
    /// refused them for good (a change above its size limit). They are kept in the record and
    /// its on-disk copy, never dropped.
    unshared: Option<String>,
    /// The words its history verbs speak in.
    words: HostWords,
    /// The writer of the record's on-disk copy; `None` keeps it in memory only.
    writer: Option<DocWriter>,
    /// Whether this device governs the room: the record's origin (convergence.md §1), whose
    /// copy needs no committed read from another host.
    governs: bool,
    /// The person's gesture open now, and its last action: the next action of theirs that goes
    /// on from it ([`DynAction::continues`]) is a change of the same gesture, which undo takes
    /// back with it as one step. Any other step, an undo or a redo closes it.
    gesture: Option<(tilo_actions::ActionId, DynAction)>,
}

impl RoomRecord {
    /// The record `doc`, shared on `route` through `manager`, changed by `person`.
    #[must_use]
    pub fn new(
        doc: Doc,
        manager: CollaborationManager,
        route: RouteSpec,
        person: impl Into<String>,
    ) -> Self {
        Self::holding(doc, None, manager, route, person.into())
    }

    fn holding(
        mut doc: Doc,
        writer: Option<DocWriter>,
        manager: CollaborationManager,
        route: RouteSpec,
        person: String,
    ) -> Self {
        let sent = doc.get_heads();
        Self {
            doc,
            manager,
            route,
            person,
            sent,
            unshared: None,
            words: HostWords::new("en-US"),
            writer,
            governs: true,
            gesture: None,
        }
    }

    /// Whether this device governs the room, as the record's origin: a record another host
    /// governs is read from it first ([`Self::open`]). What the origin alone does (a package's
    /// migrations) runs where this holds.
    #[must_use]
    pub const fn governs(&self) -> bool {
        self.governs
    }

    /// The record, its history verbs speaking `locale` (`en-US`, `de-DE`).
    #[must_use]
    pub fn speaking(mut self, locale: impl Into<String>) -> Self {
        self.words = HostWords::new(locale);
        self
    }

    /// The words the record's history verbs speak in.
    #[must_use]
    pub fn words(&self) -> &HostWords {
        &self.words
    }

    /// Open the room record of the route `manager` joined, changed by `person`, kept in
    /// `documents`: as a native launch opens its room's document. In a room this device governs,
    /// the record is its on-disk copy. In a room another host governs, the broker's committed
    /// read of the room comes first and is merged into that copy, which keeps it from then on.
    ///
    /// # Errors
    /// The manager joined no route, the copy cannot be opened, or the committed read failed.
    pub fn open(
        manager: CollaborationManager,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
    ) -> Result<Self, RoomError> {
        Self::open_with(manager, Read::Here, documents, person)
    }

    /// [`Self::open`], with the broker's committed read taken off this thread: `read` is what
    /// [`PrimaryRead::of`](collaboration::native_initial::PrimaryRead::of) said the manager's
    /// route needs, and what it read. An owner that runs other instances meanwhile waits for
    /// the read on another thread and opens the record with it here.
    ///
    /// # Errors
    /// As [`Self::open`]; and a read that is not of the manager's route.
    pub fn open_read(
        manager: CollaborationManager,
        read: Option<(
            collaboration::native_initial::PrimaryRead,
            collaboration::CommittedDocument,
        )>,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
    ) -> Result<Self, RoomError> {
        Self::open_with(manager, Read::Taken(read.map(Box::new)), documents, person)
    }

    fn open_with(
        manager: CollaborationManager,
        read: Read,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
    ) -> Result<Self, RoomError> {
        use collaboration::native_initial::{self, NativeConstruction};
        use collaboration::record::save::{
            open_stored, open_stored_adopting, open_with_committed_read, record_document,
            room_document,
        };
        let route = manager
            .primary_route()
            .map(|joined| joined.route().clone())
            .ok_or_else(|| RoomError::Open("the connection joined no route".to_owned()))?;
        let room = route.cluster_id.to_string();
        // The workspace's record of the app, or the one the route names: a member's own.
        let store = match &route.record {
            None => room_document(documents, &route.app_id, &room),
            Some(record) => {
                let id = collaboration::record::RecordId::parse(record)
                    .ok_or_else(|| RoomError::Open(format!("`{record}` is no record id")))?;
                record_document(documents, &route.app_id, &room, &id.storage_key())
            }
        }
        .map_err(|error| RoomError::Open(error.to_string()))?;
        // Construction custody reads the committed history before the record is built, as a
        // native launch's does: only a route whose reads are committed has one.
        let construction = match read {
            Read::Taken(read) => NativeConstruction::enter_read(manager, read.map(|read| *read)),
            Read::Here => NativeConstruction::enter(manager),
        }
        .map_err(RoomError::Open)?;
        let primary = native_initial::primary(&route.app_id, Some(&room));
        let governs = primary.is_none();
        // A record read from the room is the room's up to what was read. One opened from this
        // replica's own copy was handed to the room by nothing yet: its first changes (the
        // copy's creation among them) are made here, and a sealed room's channel holds only
        // what was written on it, so the first send hands it all of them.
        let handed_nothing = primary.is_none();
        let opened = match primary {
            Some(read) => read.and_then(|(_, read)| open_with_committed_read(store, &read)),
            // A member's own record of an app that kept the workspace's record before starts
            // from what that record held on this device (`open_stored_adopting`).
            None => Ok(match route.record.as_deref() {
                Some(record) if tilo_action_policy::member_record(record).is_some() => {
                    open_stored_adopting(store, room_document(documents, &route.app_id, &room).ok())
                }
                _ => open_stored(store),
            }),
        };
        let (doc, writer) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                native_initial::fail(&error);
                return Err(RoomError::Open(error));
            }
        };
        let manager = native_initial::transfer_to_app(native_initial::take_manager)
            .map_err(RoomError::Open)?
            .ok_or_else(|| RoomError::Open("the connection was not handed back".to_owned()))?;
        drop(construction);
        let person = person.into();
        let mut record = Self::holding(doc, Some(writer), manager, route, person);
        record.governs = governs;
        if handed_nothing {
            record.sent.clear();
        }
        Ok(record)
    }

    /// The record as this replica holds it.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The collab client the record travels through.
    #[must_use]
    pub fn manager(&self) -> &CollaborationManager {
        &self.manager
    }

    /// The collab client the record travels through, to send what else the instance says on
    /// its connection (actions, presence).
    pub fn manager_mut(&mut self) -> &mut CollaborationManager {
        &mut self.manager
    }

    /// The route the record travels on.
    #[must_use]
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }

    /// Whether the record's changes are stored: in memory only without an on-disk copy.
    #[must_use]
    pub fn save_status(&self) -> RecordSaveStatus {
        self.writer
            .as_ref()
            .map_or(RecordSaveStatus::MemoryOnly, |writer| {
                RecordSaveStatus::of_writer(&self.doc, writer)
            })
    }

    /// Whether the record holds changes the room was not handed yet: they go with the next
    /// send ([`Self::share`]).
    #[must_use]
    pub fn unsent(&mut self) -> bool {
        !heads_equal(&self.doc.get_heads(), &self.sent)
    }

    /// Why the record's changes stay on this device, when the collab client refused them for
    /// good: they are kept, in the record and on disk, but no longer shared.
    #[must_use]
    pub fn unshared(&self) -> Option<&str> {
        self.unshared.as_deref()
    }

    /// Write what the record holds to its on-disk copy now, as a window does before it closes.
    pub fn flush(&mut self) {
        if let Some(writer) = &mut self.writer {
            writer.request(&mut self.doc);
            writer.flush();
        }
    }

    /// Whether the person at the window has a step to undo, or an undo to redo.
    #[must_use]
    pub fn can_undo(&self) -> (bool, bool) {
        self.can_undo_as(&self.person)
    }

    /// Whether `person` has a step to undo, or an undo to redo, in the record: what the
    /// history verbs `undo` and `redo` would take for them.
    #[must_use]
    pub fn can_undo_as(&self, person: &str) -> (bool, bool) {
        let scope = Scope::Invoker(person.to_owned());
        let owns = |version: &tilo_resource::history::Version| scope.owns(version);
        (
            !steps::undo_preview(&self.doc, &scope, 1).is_empty(),
            !steps::undos_in_effect(&self.doc, &owns, 1).is_empty(),
        )
    }

    /// Take back the person's last step through the history verb `undo` on `resource` (a
    /// resource `kind`), as a step of their own that the room is handed like any other; the
    /// instance is brought up to date. Whether there was one.
    ///
    /// # Errors
    /// The undo was refused: somebody else changed what the step touched since.
    pub fn undo(
        &mut self,
        instance: &mut Instance,
        resource: &str,
        kind: &str,
    ) -> Result<bool, RoomError> {
        if !self.can_undo().0 {
            return Ok(false);
        }
        let edit = HistoryEdit::Undo {
            steps: None,
            run: None,
            by: None,
            to: None,
        };
        let me = ActionInvoker::actor(self.person.clone());
        self.history(instance, &edit, resource, kind, me, None)
            .map(|_| true)
    }

    /// Bring back the person's last undo through the history verb `redo`. Whether there was one.
    ///
    /// # Errors
    /// As [`Self::undo`].
    pub fn redo(
        &mut self,
        instance: &mut Instance,
        resource: &str,
        kind: &str,
    ) -> Result<bool, RoomError> {
        if !self.can_undo().1 {
            return Ok(false);
        }
        let edit = HistoryEdit::Redo { steps: None };
        let me = ActionInvoker::actor(self.person.clone());
        self.history(instance, &edit, resource, kind, me, None)
            .map(|_| true)
    }

    /// Apply the history verb `edit`, invoked on `resource` (a resource `kind`) by `invoker` in
    /// `ambient`, to the record as one step of its own, as a native app applies it; send it on
    /// the record's route and bring the instance up to date. What the verb did, as its caller
    /// is answered ([`verbs::result`]).
    ///
    /// # Errors
    /// The verb was refused, said in the record's words.
    pub fn history(
        &mut self,
        instance: &mut Instance,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, RoomError> {
        let (result, unshared) = self.history_step(edit, resource, kind, invoker, ambient)?;
        if let Some(why) = unshared {
            instance.unshared(why);
        }
        instance.sync(Some(&self.doc));
        Ok(result)
    }

    /// [`Self::history`] with no instance: the record's origin applying an agent task's history
    /// verb (its `undo`).
    ///
    /// # Errors
    /// The verb was refused, or the room refuses its change for good.
    pub fn history_at_origin(
        &mut self,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, RoomError> {
        match self.history_step(edit, resource, kind, invoker, ambient)? {
            (_, Some(why)) => Err(RoomError::Refused(why)),
            (result, None) => Ok(result),
        }
    }

    /// Apply the history verb `edit` as one step and send it: what it did, and why the room
    /// refused its change for good, when it did.
    fn history_step(
        &mut self,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(Option<Json>, Option<String>), RoomError> {
        // An undo or redo ends the gesture: what is typed after it is a step of its own.
        self.gesture = None;
        let action = HostHistoryEdit {
            edit: edit.clone(),
            words: self.words.clone(),
        };
        let encoded = EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload: serde_json::to_vec(edit)
                .map_err(|error| RoomError::Refused(error.to_string()))?,
        };
        let before = self.doc.get_heads();
        let context = ActionContext::for_resource(resource, kind, kind);
        self.apply_with(&action, encoded, context, invoker, ambient)?;
        let after = self.doc.get_heads();
        let patches = self.doc.diff(&before, &after);
        let unshared = self.share();
        Ok((
            verbs::result(edit.verb(), &self.doc, &patches, &self.words),
            unshared,
        ))
    }

    /// Answer the history view `payload` asks ([`verbs::VIEW_TYPE`]) of the record.
    ///
    /// # Errors
    /// The payload is no history view, or names a version the record has none of.
    pub fn view_history(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        verbs::answer(&self.doc, payload, &self.words)
    }

    /// Keep the record on disk, and hand the room what it holds past what it was handed, as one
    /// change. What the collab client cannot take yet (its queue is full, or the route is still
    /// connecting) stays in the record and goes with the next send: a frame sends again while
    /// any is left ([`Self::unsent`]), and the client wakes its owner once it has room. A change
    /// it refuses for good (above its size limit) stays on this device, kept in the record and
    /// on disk: the reason is returned the first time, for the window to say so.
    pub fn share(&mut self) -> Option<String> {
        if !self.unsent() {
            return None;
        }
        if let Some(writer) = &mut self.writer {
            writer.request(&mut self.doc);
        }
        if self.unshared.is_some() {
            return None;
        }
        let change = self.doc.save_after(&self.sent);
        if change.is_empty() {
            // The room holds all of it already, by heads named apart.
            self.sent = self.doc.get_heads();
            return None;
        }
        let sent = match self
            .manager
            .try_send(CollabRequest::document_incremental(change))
        {
            Ok(()) => true,
            Err(error @ collaboration::CollabSendError::TooLarge { .. }) => {
                tracing::warn!(%error, "room: the record's changes stay on this device");
                self.unshared = Some(error.to_string());
                return self.unshared.clone();
            }
            Err(error) => {
                tracing::debug!(%error, "room: the change goes with the next send");
                false
            }
        };
        if sent {
            self.sent = self.doc.get_heads();
        }
        None
    }

    /// Hand the room the whole record, as the answer to a late joiner the broker asked this
    /// replica to bring up to date. One the collab client does not take is said: the joiner
    /// asks again while no holder answers.
    fn send_snapshot(&mut self) {
        if let Err(error) = self
            .manager
            .try_send(CollabRequest::document_snapshot(self.doc.save()))
        {
            tracing::warn!(%error, "room: the record's snapshot for a late joiner was not sent");
        }
    }

    /// Commit every change `instance` queued for the room, in order, as a step each, and send
    /// them on the record's route ([`Self::share`]); the instance hears each answered. Returns
    /// the tickets committed.
    ///
    /// A change is the room's once it is in the record: the collab client not taking it yet
    /// never refuses it. An invocation of anything but the host's `commit` is answered refused:
    /// this path runs only what the instance planned itself.
    ///
    /// # Errors
    /// The first change that could not be applied to the record; it is answered refused, and
    /// what is still queued stays queued.
    pub fn commit(&mut self, instance: &mut Instance) -> Result<Vec<u64>, RoomError> {
        let committed = self.commit_queued(instance);
        if let Some(why) = self.share() {
            instance.unshared(why);
        }
        instance.sync(Some(&self.doc));
        committed
    }

    fn commit_queued(&mut self, instance: &mut Instance) -> Result<Vec<u64>, RoomError> {
        let mut committed = Vec::new();
        while let Some((invocation, _origin)) = instance.take_invocation() {
            let address = invocation.address.clone();
            if address.resource != HOST_RESOURCE || address.action != COMMIT {
                instance.answered(
                    &address,
                    Some("the room path runs only the instance's own commits".to_owned()),
                );
                continue;
            }
            let ticket = invocation
                .arguments
                .get("ticket")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            match self.commit_ticket(instance, ticket) {
                Ok(()) => {
                    instance.answered(&address, None);
                    committed.push(ticket);
                }
                Err(error) => {
                    instance.answered(&address, Some(error.to_string()));
                    return Err(error);
                }
            }
        }
        Ok(committed)
    }

    fn commit_ticket(&mut self, instance: &Instance, ticket: u64) -> Result<(), RoomError> {
        let host = instance.host().ok_or(RoomError::NoHost)?;
        let action = host
            .store()
            .committed(ticket)
            .ok_or(RoomError::NoAction(ticket))?;
        let invoker = ActionInvoker::actor(self.person.clone());
        let gesture = match self.gesture.take() {
            Some((gesture, last)) if last.continues(&action) => gesture,
            _ => tilo_actions::ActionId::new(),
        };
        self.apply_in_gesture(&action, invoker, None, Some(gesture))?;
        self.gesture = Some((gesture, action));
        Ok(())
    }

    /// Apply `action`, which the door built for `invoker` (a person or an agent task the broker
    /// admitted), to the room record as one step of its own, and send it on the record's route.
    /// `ambient` is the task or run it was invoked in. The instance is brought up to date.
    ///
    /// # Errors
    /// The action could not be applied.
    pub fn apply_invoked(
        &mut self,
        instance: &mut Instance,
        action: &DynAction,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RoomError> {
        self.apply(action, invoker, ambient)?;
        if let Some(why) = self.share() {
            instance.unshared(why);
        }
        instance.sync(Some(&self.doc));
        Ok(())
    }

    fn apply(
        &mut self,
        action: &DynAction,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RoomError> {
        // A step of its own ends the gesture open before it.
        self.gesture = None;
        self.apply_in_gesture(action, invoker, ambient, None)
    }

    /// As [`Self::apply`], the change a part of `gesture` when there is one: the changes of one
    /// gesture are one undo step.
    fn apply_in_gesture(
        &mut self,
        action: &DynAction,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
        gesture: Option<tilo_actions::ActionId>,
    ) -> Result<(), RoomError> {
        let encoded = action
            .encode()
            .map_err(|error| RoomError::Refused(error.to_string()))?;
        let mut context = ActionContext::for_resource(
            action.resources().first().copied().unwrap_or("room"),
            &action.kind,
            &action.kind,
        );
        context.gesture = gesture;
        self.apply_with(action, encoded, context, invoker, ambient)
    }

    /// Apply `action`, encoded as `encoded`, for `invoker` in `ambient` as one step of its own,
    /// recorded with `context`. The caller sends it on the record's route ([`Self::share`]).
    fn apply_with(
        &mut self,
        action: &dyn tilo_actions::Action,
        encoded: EncodedAction,
        mut context: ActionContext,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RoomError> {
        if let Some(ambient) = ambient {
            context = context.enriched_by(ambient);
        }
        let author = invoker.actor.clone();
        let context = context.with_invoker(invoker);
        let before = self.doc.get_heads();
        tilo_actions::apply_local_with_context(
            &mut self.doc,
            action,
            encoded,
            Some(author.as_bytes()),
            context,
        )
        .map_err(|error| RoomError::Refused(error.to_string()))?;
        // The change is a step of whoever made it, by the invoker its entry names: their Undo
        // takes it back.
        self.doc.commit();
        self.keep_only_if_the_room_takes(&before)
    }

    /// Keep what the record holds past `before` only if the room takes it, as every receiver
    /// decides its claims against the app's surface ([`collaboration::app_surface::refusal`]):
    /// a change the room refuses is taken out of the record again, with all of it, so it is
    /// never kept here, built on and written to disk while the room holds none of it.
    fn keep_only_if_the_room_takes(&mut self, before: &[ChangeHash]) -> Result<(), RoomError> {
        let change = self.doc.save_after(before);
        if change.is_empty() {
            return Ok(());
        }
        let room = self.route.cluster_id.to_string();
        let record = collab_types::Record {
            room: &room,
            app: &self.route.app_id,
            record: self.route.record.as_deref().unwrap_or(""),
        };
        let Some(why) = collaboration::app_surface::refusal(
            &record,
            &collab_types::DocumentMutation::Incremental(change),
        ) else {
            return Ok(());
        };
        tracing::warn!(app = %self.route.app_id, %why, "room: a change the room refuses was not kept");
        let actor = self.doc.get_actor().clone();
        let mut kept = self.doc.fork_at(before).map_err(|error| {
            RoomError::Refused(format!("{why}; and it could not be taken out: {error}"))
        })?;
        kept.set_actor(actor);
        self.doc = kept;
        Err(RoomError::Refused(format!("the room refuses it: {why}")))
    }

    /// Take in what the record's route brought and bring `instance` up to date with the
    /// record. Whether the record changed.
    pub fn receive(&mut self, instance: &mut Instance) -> bool {
        self.receive_and(instance, |_| {})
    }

    /// [`Self::receive`], handing every event that is no change of the record (an invocation,
    /// presence, the route's state) to `other`, in the order they came.
    pub fn receive_and(&mut self, instance: &mut Instance, other: impl FnMut(CollabEvent)) -> bool {
        let changed = self.take_in(other);
        if changed {
            instance.sync(Some(&self.doc));
        }
        changed
    }

    /// The record's heads now: what [`Self::changes_since`] starts from.
    #[must_use]
    pub fn heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// What the record holds past `heads`, as Automerge changes: what a change applied here
    /// wrote, for its invoker to read back without waiting on the room.
    #[must_use]
    pub fn changes_since(&mut self, heads: &[ChangeHash]) -> Vec<u8> {
        self.doc.save_after(heads)
    }

    /// Take in what the record's route brought, with no instance to bring up to date: a record
    /// held only to apply changes at its origin. Events that are no change of the record are
    /// dropped. Whether the record changed.
    pub fn take_in_room(&mut self) -> bool {
        self.take_in(|_| {})
    }

    /// Apply `action`, which the door built for `invoker` (an agent task the broker admitted),
    /// to the room record as one step of its own in `ambient`, and send it on the record's
    /// route, with no instance: the record's origin applying a change.
    ///
    /// # Errors
    /// The action could not be applied, or the room refuses its change for good.
    pub fn apply_change(
        &mut self,
        action: &DynAction,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RoomError> {
        self.apply(action, invoker, ambient)?;
        match self.share() {
            Some(why) => Err(RoomError::Refused(why)),
            None => Ok(()),
        }
    }

    fn take_in(&mut self, mut other: impl FnMut(CollabEvent)) -> bool {
        let mut changed = false;
        let before = self.doc.get_heads();
        for event in self.manager.poll() {
            let (envelope, decrypted_payload) = match event {
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => (envelope, decrypted_payload),
                CollabEvent::DocumentSnapshotRequested { route } if route == self.route => {
                    // The broker chose this connection as the one that answers a late joiner
                    // of the record: nobody else does, so it is answered here.
                    self.send_snapshot();
                    continue;
                }
                event => {
                    other(event);
                    continue;
                }
            };
            let Some(payload) =
                validated_route_payload_with(&self.route, &envelope, decrypted_payload.as_deref())
            else {
                tracing::warn!("room: a change for another route was not taken in");
                continue;
            };
            let mutation =
                match decode_routed_mutation(&self.route, payload, decrypted_payload.as_deref()) {
                    Ok(mutation) => mutation,
                    Err(error) => {
                        tracing::warn!(
                            ?error,
                            "room: a change that does not decode was not taken in"
                        );
                        continue;
                    }
                };
            let taken = match &mutation {
                DocumentMutation::Incremental(bytes) => self.doc.load_incoming(bytes),
                DocumentMutation::Snapshot(bytes) => self.doc.load_incoming_whole(bytes),
            };
            match taken {
                Ok(_) => changed = true,
                Err(error) => tracing::warn!(%error, "room: a change was not taken in"),
            }
        }
        if changed {
            // What arrived is the room's already: nothing of it is sent back. What this replica
            // made and the room was not handed yet still goes with the next send.
            if heads_equal(&before, &self.sent) {
                self.sent = self.doc.get_heads();
            } else {
                for head in self.doc.get_heads() {
                    if !before.contains(&head) && !self.sent.contains(&head) {
                        self.sent.push(head);
                    }
                }
            }
            if let Some(writer) = &mut self.writer {
                writer.request(&mut self.doc);
            }
        }
        changed
    }
}

/// Whether `a` and `b` name the same heads, in whatever order.
fn heads_equal(a: &[ChangeHash], b: &[ChangeHash]) -> bool {
    a.len() == b.len() && a.iter().all(|head| b.contains(head))
}

/// What a tilolang host's history verbs say, without the toolkit: a step as the platform's
/// history sentence in `locale`, a person by the handle their changes carry. The UI half renders
/// History in the reader's own words.
#[derive(Clone, Debug)]
pub struct HostWords {
    locale: String,
}

impl HostWords {
    /// The words of `locale` (`en-US`, `de-DE`).
    #[must_use]
    pub fn new(locale: impl Into<String>) -> Self {
        Self {
            locale: locale.into(),
        }
    }
}

impl ResolveAuthor for HostWords {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        let name = actor_handle(actor_hex).unwrap_or_else(|| UNKNOWN_AUTHOR_NAME.to_owned());
        HistoryAuthor::new(name, stable_hue(actor_hex), short_id(actor_hex))
    }
}

impl HistoryWords for HostWords {
    fn sentence(&self, entry: &ActionContext) -> String {
        tilo_history::Reader::new(&self.locale).sentence(entry)
    }
}

/// A history verb applied to a room record: the edit [`verbs`] applies, decoding the record's
/// recorded actions with the room's registry ([`register`]) and speaking the record's words.
#[derive(Debug)]
pub struct HostHistoryEdit {
    edit: HistoryEdit,
    words: HostWords,
}

impl<'de> serde::Deserialize<'de> for HostHistoryEdit {
    /// A history verb a peer recorded, replayed here: what it says is never shown, so it speaks
    /// the platform's source language.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            edit: HistoryEdit::deserialize(deserializer)?,
            words: HostWords::new("en-US"),
        })
    }
}

impl tilo_actions::Action for HostHistoryEdit {
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        self.edit.change_kind()
    }

    fn describe(&self, doc: &tilo_actions::Doc) -> Option<tilo_actions::Described> {
        Some(self.edit.described(doc))
    }

    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        self.edit.apply_registered(doc, register, &self.words)
    }
}

/// Register what a room record's changes record: the runtime's planned actions and the history
/// verbs.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<DynAction>(ACTION_TYPE);
    registry.register::<HostHistoryEdit>(EDIT_TYPE);
}
