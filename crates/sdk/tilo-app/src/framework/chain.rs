//! Records an app keeps beside its own as a chain of parts: what grows
//! without end — a chat's messages, a log — kept as numbered records of
//! another resource type ([`RecordId::part`]) in the room the app's record
//! lives in.
//!
//! One record cannot hold everything such an app ever wrote: a record's
//! snapshot has a size limit, and a newcomer would be sent all of it. A
//! chain keeps it in parts. The part with the highest number a device holds
//! is the chain's head; a room's host replays a joining device the head
//! only, and an earlier part reaches a device when it opens it
//! ([`Chain::hold`]). Two writers that start the same new part start the
//! same record (each part's genesis is the same everywhere), so their
//! changes meet there.
//!
//! The resource of the parts says how they are used ([`Chained`]): which
//! part an action changes (the one holding the item it names, or for a new
//! item the head or the part after it), and what an action reads of the
//! app's own record where it is invoked. Everything else is the framework's:
//!
//! - **One door.** The chain is published beside the app's resource as one
//!   resource ([`Chain::id`]): the head's actions, and every held part's
//!   items with theirs, each carried out on the part [`Chained::part_for`]
//!   names when it is invoked. The palette, agents, scripts and the app's
//!   own surface reach the parts through it, checked against the
//!   participant's roles in the room like the app's own actions (the parts
//!   carry the app record's grants), attributed to whoever invokes them and
//!   written into the part's history.
//! - **Held where the app is.** The parts are records of the app's own
//!   store, so whatever syncs the app's records — the browser's room, an
//!   attached view — syncs them on their own routes.
//! - **Read as they change.** [`Chain::parts`] is every part the window
//!   holds, oldest first, followed change by change; [`Chain::revision`]
//!   moves whenever any of them changed, or a new head arrived.
//!
//! An app declares its chain in [`ResourceApp::records`](super::ResourceApp::records)
//! (`records.chain::<Part>()`), so it is opened and published with the
//! window, and reaches it from a view or surface with
//! [`RecordEnv::chain`](super::RecordEnv::chain). Edits of a part are not
//! undone with the app's own: they are recorded in the part's history, and
//! the chain publishes the history verbs (`undo`, `redo`, `restore`,
//! `checkpoint`) naming the part they change, so a person, an agent or the
//! room's moderation takes back steps in a part through the same door.
//!
//! A part opened in the app's store by anyone — a peer's merged into an
//! attached view, one this device asked the room for — is held from the
//! next read on.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::{ActionContext, ActionError, ActionInvoker, OperationKind};
use tilo_resource::{Invoke, Reader, Resource};
use ui_core::permissions::{LivePermissions, PermissionScope, TreePath};
use ui_core::record::{RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordStore};

use super::dispatch::Dispatcher;
use super::history::{PART_EDIT_TYPE, PartEdit};
use super::live::Live;
use super::record::RecordDoor;

/// The operation type a chain's published actions are carried out under:
/// the host applies the action to the part of the chain it names.
pub(crate) const CHAIN_ACTION_TYPE: &str = "tilo.app.chain-action";

/// How often a window looks for parts that arrived on this device without
/// being opened (a new head synced in) when nothing told its store one did
/// ([`RecordStore::note_kept`]): listing the store reads its index.
const LOOK_EVERY: ui_core::time::Duration = ui_core::time::Duration::from_secs(1);

/// A resource whose records are the parts of a chain kept beside the app's
/// own record ([`RecordId::part`]). See the [module docs](self).
pub trait Chained: Resource {
    /// The app's own resource: what [`Self::prepare`] reads where an action
    /// is invoked.
    type Base: Resource;

    /// The chain the parts belong to: part `n` is `chain().part(n)`.
    fn chain() -> RecordId;

    /// The part `action` changes, given the parts this device holds, oldest
    /// first: the one holding the item it names, or for a new item the head
    /// (or the next part, once the head is full).
    ///
    /// # Errors
    /// Why the action changes no part held here (it names an item none of
    /// them holds).
    fn part_for(action: &Self::Action, parts: &[(u64, Rc<Self::State>)]) -> Result<u64, String>;

    /// Fill in, where the action is invoked, what it records from the app's
    /// own record (who a mention names), and refuse what that record does
    /// not allow (a post to a channel it has not got). A part's action
    /// itself reads only its part, so it replays the same on every replica.
    ///
    /// # Errors
    /// Why the app's record refuses the action.
    fn prepare(
        action: &mut Self::Action,
        base: &<Self::Base as Resource>::State,
    ) -> Result<(), String> {
        let _ = (action, base);
        Ok(())
    }
}

/// What a window holds a chain with: the store of the app's records, who
/// edits, and what they may do in the room.
#[derive(Clone)]
pub(crate) struct RoomRecords {
    pub(crate) store: RecordStore,
    pub(crate) me: String,
    pub(crate) permissions: Option<LivePermissions>,
    pub(crate) app_id: &'static str,
    pub(crate) app_title: &'static str,
}

/// The app's record as a part's [`Chained::prepare`] reads it.
type Base<P> = Rc<dyn Fn() -> Rc<<<P as Chained>::Base as Resource>::State>>;

/// A chain held for a window: its parts, read as they change. See the
/// [module docs](self).
pub(crate) struct ChainState<P: Chained> {
    room: RoomRecords,
    base: Base<P>,
    id: String,
    kind: RecordKind,
    parts: RefCell<BTreeMap<u64, Live<P>>>,
    /// What a part holds before anything was written to it: what the chain
    /// publishes before it has a head.
    empty: Rc<P::State>,
    revision: Cell<u64>,
    /// When the window last looked for a newer head, with the store's
    /// [`RecordStore::kept_revision`] then.
    looked: Cell<Option<(ui_core::time::Instant, u64)>>,
    published: RefCell<Option<((u64, u64), tilo_actions::Resource)>>,
}

impl<P: Chained> ChainState<P> {
    /// Hold the chain in `room`, with its head when this device has one.
    pub(crate) fn open(room: RoomRecords, base: Base<P>) -> Option<Self> {
        let mut doc = ui_core::automerge::AutoCommit::new();
        P::initialize(&mut doc)
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "chain part has no genesis"))
            .ok()?;
        let empty = Reader::<P>::read(&doc)
            .map_err(|error| tracing::warn!(kind = P::KIND, %error, "chain part unreadable"))
            .ok()?
            .state()
            .clone();
        let state = Self {
            room,
            base,
            id: ui_core::record::chain_resource_id(&P::chain()),
            kind: RecordKind::new(P::KIND),
            parts: RefCell::default(),
            empty,
            revision: Cell::new(0),
            looked: Cell::new(None),
            published: RefCell::new(None),
        };
        state.look();
        Some(state)
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    fn checkout(&self, index: u64) -> RecordCheckout {
        RecordCheckout::new(P::chain().part(index), self.kind.clone())
    }

    /// The parts held, oldest first.
    pub(crate) fn parts(&self) -> Vec<(u64, Rc<P::State>)> {
        self.refresh();
        self.parts
            .borrow()
            .iter()
            .map(|(index, live)| (*index, live.get()))
            .collect()
    }

    pub(crate) fn head(&self) -> Option<u64> {
        self.refresh();
        self.parts.borrow().keys().next_back().copied()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.refresh();
        self.revision.get()
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Follow every held part that changed, and take up a newer head that
    /// arrived on this device. Whether anything moved.
    pub(crate) fn refresh(&self) -> bool {
        let mut moved = false;
        for (index, live) in self.parts.borrow().iter() {
            match self
                .room
                .store
                .read(&P::chain().part(*index), |doc| live.sync(doc))
            {
                Some(Ok(true)) => moved = true,
                Some(Err(error)) => {
                    tracing::warn!(kind = P::KIND, part = index, %error, "chain part unreadable");
                }
                _ => {}
            }
            let _ = live.take_change();
        }
        // A part opened in the store since (a peer's merged in, one this
        // device asked for and received) is held at once.
        let opened: Vec<u64> = {
            let parts = self.parts.borrow();
            self.room
                .store
                .open_parts(&P::chain())
                .into_iter()
                .filter(|index| !parts.contains_key(index))
                .collect()
        };
        for index in opened {
            moved |= self.follow(index).is_ok();
        }
        if self.due() {
            moved |= self.look();
        }
        if moved {
            self.moved();
        }
        moved
    }

    /// Whether to look for a newer head now: the store was told a record
    /// arrived, or the last look is a while ago. Not while a frame reads the
    /// store (a view's sync): opening a part takes the store's writer, so
    /// the look waits for the frame's settle, where the window takes up what
    /// it was sent, and the view's sync reads the part it opened.
    fn due(&self) -> bool {
        if !self.room.store.takes_writes() {
            return false;
        }
        self.looked.get().is_none_or(|(at, kept)| {
            kept != self.room.store.kept_revision()
                || ui_core::time::now().saturating_duration_since(at) >= LOOK_EVERY
        })
    }

    /// Open every part newer than the head that this device holds. Whether
    /// one was.
    fn look(&self) -> bool {
        self.looked.set(Some((
            ui_core::time::now(),
            self.room.store.kept_revision(),
        )));
        let head = self.parts.borrow().keys().next_back().copied();
        // Only the newest is the head; the parts between are opened when
        // someone reads back to them.
        let kept = self.room.store.held_parts(&P::chain()).last().copied();
        // A part a peer started is the same record here (each part's genesis
        // is the same everywhere): started here, it syncs on its route.
        let started = self.room.store.announced_parts(&P::chain()).last().copied();
        let Some(newest) = kept.max(started) else {
            return false;
        };
        if head.is_some_and(|head| newest <= head) {
            return false;
        }
        let opened = if kept.is_some_and(|kept| kept >= newest) {
            self.hold(newest)
        } else {
            self.start(newest)
        };
        opened
            .map_err(|error| {
                tracing::debug!(kind = P::KIND, part = newest, %error, "chain head not opened");
            })
            .is_ok()
    }

    /// Hold part `index`, which this device has or asks its room for.
    pub(crate) fn hold(&self, index: u64) -> Result<(), String> {
        if self.parts.borrow().contains_key(&index) {
            return Ok(());
        }
        let checkout = self.checkout(index);
        if !self.room.store.is_ready(&checkout.id) {
            self.room
                .store
                .try_open_record(&checkout)
                .map_err(|error| error.to_string())?;
        }
        self.follow(index)
    }

    /// Hold part `index`, starting it when nobody has.
    fn start(&self, index: u64) -> Result<(), String> {
        if self.parts.borrow().contains_key(&index) {
            return Ok(());
        }
        let checkout = self.checkout(index);
        if !self.room.store.is_ready(&checkout.id) {
            match self.room.store.try_open_record(&checkout) {
                Ok(_) => {}
                // A store that keeps no canonical records (a test's) has no
                // record it could open: it is started as every record is.
                Err(RecordOpenError::MissingRecord | RecordOpenError::IdentityMismatch) => {
                    self.create(&checkout)?;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        self.follow(index)
    }

    /// Create the part `checkout` names, with the part resource's genesis:
    /// the same on every device, so two that start it converge.
    fn create(&self, checkout: &RecordCheckout) -> Result<(), String> {
        let initialize = |doc: &mut ui_core::automerge::AutoCommit| {
            P::initialize(doc).map_err(|error| error.to_string())
        };
        match self.room.store.reserve_record(checkout) {
            Ok(mut reservation) => reservation
                .initialize(initialize)
                .map_err(|error| error.to_string()),
            // A store that keeps no canonical records (a test's) creates it
            // the way it creates every record.
            Err(RecordOpenError::IdentityMismatch) => {
                self.room
                    .store
                    .create_record(checkout)
                    .map_err(|error| error.to_string())?;
                self.room
                    .store
                    .with(&checkout.id, |doc| {
                        initialize(doc)?;
                        doc.commit();
                        Ok::<(), String>(())
                    })
                    .ok_or("the new part is not open")?
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn follow(&self, index: u64) -> Result<(), String> {
        let part = P::chain().part(index);
        // An older part is brought up to the part's storage version before
        // it is read, as the app's record is when it opens: once, as the
        // same housekeeping change on every device.
        let stored = self
            .room
            .store
            .read(&part, tilo_resource::evolution::stored_version)
            .ok_or("the part is not open here")?;
        if stored < <P as Resource>::VERSION {
            // Not while a frame reads the store: the part is held at the
            // next refresh, as a part this device asked for is.
            if !self.room.store.takes_writes() {
                return Err("the part is migrated once the store takes writes".into());
            }
            self.room
                .store
                .with(&part, tilo_resource::evolution::migrate::<P>)
                .ok_or("the part is not open here")?
                .map_err(|error| error.to_string())?;
        }
        let reader = self
            .room
            .store
            .read(&part, Reader::<P>::read)
            .ok_or("the part is not open here")?
            .map_err(|error| error.to_string())?;
        self.parts.borrow_mut().insert(index, Live::new(reader));
        self.moved();
        Ok(())
    }

    /// Apply `action`, invoked by `invoker`, to the part it changes: read
    /// against the app's record ([`Chained::prepare`]), checked against
    /// what they may do in the room, attributed to them and written into
    /// the part's history. The part it was applied to.
    ///
    /// # Errors
    /// The participant may not, the app's record or the part refused it, or
    /// the part could not be opened or started.
    pub(crate) fn apply_as(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
    ) -> Result<u64, ActionError> {
        self.apply_answering(action, invoker)
            .map(|(index, _)| index)
    }

    /// [`Self::apply_as`], with the item resources the action created: what
    /// its invoker is answered, as for the app's own actions.
    fn apply_answering(
        &self,
        action: &P::Action,
        invoker: &ActionInvoker,
    ) -> Result<(u64, Vec<String>), ActionError> {
        let mut action = action.clone();
        P::prepare(&mut action, &(self.base)()).map_err(ActionError::Refused)?;
        let index = P::part_for(&action, &self.parts()).map_err(ActionError::Refused)?;
        if let Some(permissions) = &self.room.permissions {
            // Decided where the chain publishes the action, as the door
            // decides it: a grant on the app, the parts' kind or the verb
            // covers it.
            let need = tilo_resource::need_of::<P>(&action);
            let node = if let Some(verb) = verb_of::<P>(&action) {
                tilo_actions::action_node(
                    &TreePath::root(),
                    self.room.app_id,
                    P::KIND,
                    &self.id,
                    need.field.as_deref(),
                    &verb,
                )
            } else {
                TreePath::root()
                    .app(self.room.app_id)
                    .kind(P::KIND)
                    .resource(self.id.clone())
                    .field(need.field.as_deref().unwrap_or_default())
            };
            if !permissions.check(&PermissionScope::Action {
                node,
                kind: need.kind,
            }) {
                return Err(ActionError::Refused(format!(
                    "you may not do this to these {}",
                    P::LABEL.to_lowercase()
                )));
            }
        }
        self.start(index).map_err(ActionError::Refused)?;
        let encoded = tilo_resource::encode::<P>(&action)?;
        let created = self.write(index, &action, encoded, invoker)?;
        Ok((index, created))
    }

    /// Apply a history verb to the part it names ([`PartEdit`]), invoked by
    /// `invoker`: checked as an undoable change where the chain publishes
    /// the verb, and written into the part's history as any of its steps.
    /// Answers the part and its version after.
    ///
    /// # Errors
    /// The participant may not, the part is neither here nor known to the
    /// room, or history refused the edit.
    fn apply_history(
        &self,
        edit: &PartEdit,
        invoker: &ActionInvoker,
    ) -> Result<serde_json::Value, ActionError> {
        if let Some(permissions) = &self.room.permissions
            && !permissions.check(&PermissionScope::Action {
                node: tilo_actions::action_node(
                    &TreePath::root(),
                    self.room.app_id,
                    P::KIND,
                    &self.id,
                    None,
                    edit.verb(),
                ),
                kind: tilo_actions::ActionKind::Undoable,
            })
        {
            return Err(ActionError::Refused(format!(
                "you may not change the history of these {}",
                P::LABEL.to_lowercase()
            )));
        }
        self.hold(edit.part).map_err(ActionError::Refused)?;
        let encoded = edit.encoded().map_err(ActionError::Refused)?;
        self.write(edit.part, &edit.action::<P>(), encoded, invoker)?;
        let now = self
            .room
            .store
            .with(&P::chain().part(edit.part), |doc| {
                super::history::version_name(&doc.get_heads())
            })
            .unwrap_or_default();
        Ok(serde_json::json!({ "part": edit.part, "now": now }))
    }

    /// Write `action` into part `index`, which is open here, attributed to
    /// `invoker` and recorded in the part's history. The item resources it
    /// created.
    fn write(
        &self,
        index: u64,
        action: &dyn tilo_actions::Action,
        encoded: tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<Vec<String>, ActionError> {
        let context = ActionContext::for_resource(&self.id, P::KIND, P::LABEL)
            .with_app(self.room.app_id, self.room.app_title)
            .with_invoker(invoker.clone())
            .with_change(action.change_kind());
        let id = P::chain().part(index);
        let created = self
            .room
            .store
            .with_history(&id, |doc, history| {
                let before = doc.get_heads();
                let writer = ui_core::bound_dispatch::document_writer(&invoker.actor, doc);
                let applied = tilo_actions::apply_local_with_context(
                    doc,
                    action,
                    encoded,
                    Some(&writer),
                    context,
                )
                .map(|envelope| {
                    ui_core::bound_dispatch::minted_items(doc, P::KIND, &envelope, &before)
                })
                .map_err(|error| ActionError::Refused(error.to_string()));
                // Nobody's undo step: a part's edits are kept in its
                // history, not the app's undo.
                history.rebase(doc);
                applied
            })
            .ok_or_else(|| ActionError::Refused("the part is not open here".into()))??;
        self.room.store.schedule_committed_history(&id);
        self.refresh();
        self.moved();
        Ok(created)
    }

    /// The chain as it is published: the head's actions and every held
    /// part's items, each carried out through [`CHAIN_ACTION_TYPE`] on the
    /// part it changes; checked against the participant's roles in the room
    /// as the app's own actions are.
    pub(crate) fn publish(&self) -> tilo_actions::Resource {
        let revision = (self.revision(), ui_core::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let parts = self.parts();
        let head = parts
            .last()
            .map_or_else(|| Rc::clone(&self.empty), |(_, state)| Rc::clone(state));
        let mut resource = tilo_resource::publish::<P>(&self.id, P::LABEL, &head);
        let mut seen: std::collections::HashSet<tilo_actions::ResourceId> = resource
            .children
            .iter()
            .map(|child| child.id.clone())
            .collect();
        for (_, state) in parts.iter().rev().skip(1) {
            let earlier = tilo_resource::publish::<P>(&self.id, P::LABEL, state);
            resource.children.extend(
                earlier
                    .children
                    .into_iter()
                    .filter(|child| seen.insert(child.id.clone())),
            );
        }
        let mut published = route(resource, &self.id);
        let own: std::collections::HashSet<String> = published
            .actions
            .iter()
            .map(|spec| spec.id.clone())
            .collect();
        published.actions.extend(
            super::history::part_edit_specs()
                .into_iter()
                .filter(|spec| !own.contains(&spec.id))
                .map(|spec| super::record::route_operation(spec, CHAIN_ACTION_TYPE, &self.id)),
        );
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }
}

/// The verb `action` runs, as history names it: what a grant on one verb
/// covers. `None` for a field edit.
fn verb_of<P: Chained>(action: &P::Action) -> Option<String> {
    use tilo_actions::Action as _;
    let mut doc = ui_core::automerge::AutoCommit::new();
    P::initialize(&mut doc).ok()?;
    match action.describe(&doc)?.what {
        tilo_actions::Did::Verb(call) => Some(call.action.action),
        _ => None,
    }
}

/// `resource`'s document actions, and those of its children, carried out on
/// the chain `chain`, each keeping the need it declares.
fn route(mut resource: tilo_actions::Resource, chain: &str) -> tilo_actions::Resource {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .filter(|spec| spec.kind == OperationKind::Document)
        .map(|spec| super::record::route_operation(spec, CHAIN_ACTION_TYPE, chain))
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| route(child, chain))
        .collect();
    resource
}

impl<P: Chained> RecordDoor for ChainState<P> {
    fn id(&self) -> &str {
        &self.id
    }

    fn editable(&self) -> bool {
        true
    }

    fn revision(&self) -> u64 {
        ChainState::revision(self)
    }

    fn publish(&self) -> tilo_actions::Resource {
        ChainState::publish(self)
    }

    fn apply_encoded(
        &self,
        action: &tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<(), String> {
        if action.type_id == PART_EDIT_TYPE {
            let edit: PartEdit =
                tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
            return self
                .apply_history(&edit, invoker)
                .map(drop)
                .map_err(|error| error.to_string());
        }
        if action.type_id != P::ACTION_TYPE {
            return Err(format!("{} take no `{}` action", P::LABEL, action.type_id));
        }
        let action: P::Action =
            tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
        self.apply_as(&action, invoker)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn apply_answering(
        &self,
        action: &tilo_actions::EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, String> {
        if action.type_id == PART_EDIT_TYPE {
            let edit: PartEdit =
                tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
            let value = self
                .apply_history(&edit, invoker)
                .map_err(|error| error.to_string())?;
            return Ok(tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            });
        }
        if action.type_id != P::ACTION_TYPE {
            return Err(format!("{} take no `{}` action", P::LABEL, action.type_id));
        }
        let action: P::Action =
            tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
        let (_, resources) = ChainState::apply_answering(self, &action, invoker)
            .map_err(|error| error.to_string())?;
        Ok(tilo_actions::ActionOutput {
            value: None,
            resources,
        })
    }
}

/// A chain of records the app keeps beside its own, held for a view or
/// surface. See the [module docs](self).
pub struct Chain<P: Chained> {
    state: Rc<ChainState<P>>,
    dispatch: Dispatcher<P>,
}

impl<P: Chained> Clone for Chain<P> {
    fn clone(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
            dispatch: self.dispatch.clone(),
        }
    }
}

impl<P: Chained> Chain<P> {
    pub(crate) fn new(state: Rc<ChainState<P>>, dispatch: Dispatcher<P>) -> Self {
        Self { state, dispatch }
    }

    /// The address the chain is published under: what an agent or a
    /// script invokes its actions on.
    #[must_use]
    pub fn id(&self) -> &str {
        self.state.id()
    }

    /// Every part this window holds, oldest first, as it stands now.
    #[must_use]
    pub fn parts(&self) -> Vec<(u64, Rc<P::State>)> {
        self.state.parts()
    }

    /// The number of the chain's head: the newest part held. `None` while
    /// nothing was written to the chain.
    #[must_use]
    pub fn head(&self) -> Option<u64> {
        self.state.head()
    }

    /// Increases whenever a held part changed or a newer head arrived.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.state.revision()
    }

    /// Hold the earlier part `index` too: read it from this device, which
    /// asks the room for it when it has not got it yet (its changes arrive
    /// and [`Self::revision`] moves).
    ///
    /// # Errors
    /// The part is neither here nor known to the room yet.
    pub fn hold(&self, index: u64) -> Result<(), String> {
        self.state.hold(index)
    }

    /// Apply `action` now, as the window's person, to the part it changes.
    /// The part's number.
    ///
    /// # Errors
    /// As the door refuses it: the person may not, the app's record or the
    /// part refused it.
    pub fn apply(&self, action: &P::Action) -> Result<u64, ActionError> {
        let me = self.dispatch.me().unwrap_or(&self.state.room.me);
        self.state.apply_as(action, &ActionInvoker::actor(me))
    }

    /// Apply what `invoke` names now, as [`Self::apply`].
    ///
    /// # Errors
    /// As [`Self::apply`], or the arguments do not build the action.
    pub fn invoke(&self, invoke: &Invoke<P>) -> Result<u64, ActionError> {
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        self.apply(&action)
    }

    /// Apply what `invoke` names as `author` wrote it: a screenshot
    /// scenario staging what other people wrote, or a test acting as
    /// someone else. Checked and recorded as [`Self::apply`] is, attributed
    /// to `author`.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn stage_as(&self, invoke: &Invoke<P>, author: &str) -> Result<u64, ActionError> {
        self.stage_at(invoke, author, tilo_resource::origin_now_ms())
    }

    /// [`Self::stage_as`] at `now_ms`, the action's time: what someone
    /// wrote at another time.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn stage_at(
        &self,
        invoke: &Invoke<P>,
        author: &str,
        now_ms: i64,
    ) -> Result<u64, ActionError> {
        let action = invoke.build(now_ms).map_err(ActionError::Refused)?;
        self.state.apply_as(&action, &ActionInvoker::actor(author))
    }

    /// Write `state` into part `index` directly, as what the part starts
    /// with: a capture's or a test's fixture (items under fixed ids,
    /// written by several people) in fields the parts keep to their own
    /// actions (`#[model(readonly)]`), which refuse every generic edit.
    /// Not an action: nothing checks who may, nobody is its author, and it
    /// is committed as the part's genesis, so only a fixture writes it.
    ///
    /// # Errors
    /// The part could not be opened or started, or `state` does not fit it.
    pub fn write_start(&self, index: u64, state: &P::State) -> Result<(), String>
    where
        P::State: tilo_resource::Fields,
    {
        use tilo_resource::Fields as _;
        self.state.start(index)?;
        let id = P::chain().part(index);
        self.state
            .room
            .store
            .with_history(&id, |doc, history| {
                let root = ui_core::automerge::ROOT;
                let written = <P::State as tilo_resource::Fields>::read_fields(doc, &root)
                    .and_then(|before| {
                        state.write_changed_fields(
                            &before,
                            doc,
                            &root,
                            &tilo_resource::Delta::default(),
                        )
                    })
                    .map_err(|error| error.to_string());
                doc.commit_with(
                    ui_core::automerge::transaction::CommitOptions::default()
                        .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
                );
                history.rebase(doc);
                written
            })
            .ok_or("the part is not open here")??;
        self.state.room.store.schedule_committed_history(&id);
        self.state.refresh();
        self.state.moved();
        Ok(())
    }

    /// Whether the chain takes a change now: not while a frame reads the
    /// app's records (a view's sync), when [`Self::stage_as`] waits for a
    /// later pass (an event, a settle pass).
    #[must_use]
    pub fn takes_changes(&self) -> bool {
        self.state.room.store.takes_writes()
    }

    /// The chain's dispatcher: what it submits goes through the host's door
    /// as a click on a control does (a destructive verb asks first), and
    /// [`apply_now`](Dispatcher::apply_now) applies at once.
    #[must_use]
    pub fn dispatcher(&self) -> Dispatcher<P> {
        self.dispatch.clone()
    }

    /// A chain outside any host, for a tool or a test that holds the app's
    /// records without a window: held in a store of its own, edited as `me`
    /// (or whoever [`Self::stage_as`] names), who may do anything, its
    /// actions reading the app's record from `base`. `None` when `P` does
    /// not read as a part.
    #[must_use]
    pub fn detached(
        app_id: &'static str,
        me: &str,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Self> {
        Self::detached_in(RecordStore::in_memory(), app_id, me, base)
    }

    /// [`Self::detached`], held in `store`: one device's records of the
    /// app in a room, for a test of what reaches that device's store from
    /// another one.
    #[must_use]
    pub fn detached_in(
        store: RecordStore,
        app_id: &'static str,
        me: &str,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Self> {
        let room = RoomRecords {
            store,
            me: me.to_owned(),
            permissions: None,
            app_id,
            app_title: app_id,
        };
        let state = Rc::new(ChainState::<P>::open(room, Rc::new(base))?);
        let dispatch = detached_dispatch(app_id, &state, me);
        Some(Self { state, dispatch })
    }

    /// The same detached chain edited as `me`: another person's window
    /// over it, in a test.
    #[must_use]
    pub fn as_reader(&self, me: &str) -> Self {
        Self {
            state: Rc::clone(&self.state),
            dispatch: detached_dispatch(self.state.room.app_id, &self.state, me),
        }
    }

    /// Part `index` as a document's bytes, for a tool that carries it to
    /// another device itself; `None` when the chain holds no such part.
    #[must_use]
    pub fn part_bytes(&self, index: u64) -> Option<Vec<u8>> {
        self.state
            .room
            .store
            .read(&P::chain().part(index), |doc| doc.clone().save())
    }

    /// What this chain's dispatcher handed to the door and nothing took yet
    /// (a destructive verb waiting for confirmation), applied as the door
    /// does once the person agreed. Taking them empties the queue.
    ///
    /// # Errors
    /// The first that did not build or apply.
    #[cfg(any(test, feature = "test-support"))]
    pub fn confirm_asked(&self) -> Result<(), ActionError> {
        for action in self.dispatch.take_asked().map_err(ActionError::Refused)? {
            self.apply(&action)?;
        }
        Ok(())
    }
}

/// The dispatcher of a chain outside any host, applying as `me`.
fn detached_dispatch<P: Chained>(
    app_id: &'static str,
    state: &Rc<ChainState<P>>,
    me: &str,
) -> Dispatcher<P> {
    let applying = Rc::downgrade(state);
    let invoker = ActionInvoker::actor(me);
    Dispatcher::<P>::new(app_id, state.id(), false, Some(me)).for_room_record::<P>(
        state.id(),
        false,
        move |action| {
            let state = applying
                .upgrade()
                .ok_or_else(|| ActionError::Refused("the chain is closed".into()))?;
            state.apply_as(action, &invoker).map(drop)
        },
    )
}

/// The chains a window's views opened, by part type: one per type; and the
/// child records they opened, by container.
#[derive(Default)]
pub(crate) struct Chains {
    held: RefCell<BTreeMap<&'static str, Rc<dyn Any>>>,
    /// The child records held, by their container's address.
    children: RefCell<BTreeMap<String, Rc<dyn Any>>>,
    doors: RefCell<Vec<Rc<dyn RecordDoor>>>,
}

impl Chains {
    /// The chain of `P`, opened in `room` on first use.
    pub(crate) fn open<P: Chained>(
        &self,
        room: Option<&RoomRecords>,
        base: impl Fn() -> Rc<<P::Base as Resource>::State> + 'static,
    ) -> Option<Rc<ChainState<P>>> {
        if let Some(held) = self.held.borrow().get(P::KIND) {
            return Rc::clone(held).downcast::<ChainState<P>>().ok();
        }
        let state = Rc::new(ChainState::<P>::open(room?.clone(), Rc::new(base))?);
        self.held
            .borrow_mut()
            .insert(P::KIND, Rc::clone(&state) as Rc<dyn Any>);
        self.doors
            .borrow_mut()
            .push(Rc::clone(&state) as Rc<dyn RecordDoor>);
        Some(state)
    }

    /// The children of `C` the parent lists, held in `room` on first use:
    /// published and answered beside the chains, through the same door.
    pub(crate) fn open_children<C: super::children::ChildRecord>(
        &self,
        room: Option<&RoomRecords>,
        parent: impl Fn() -> Rc<<C::Parent as Resource>::State> + 'static,
    ) -> Option<Rc<super::children::ChildrenState<C>>> {
        let key = ui_core::record::children_resource_id(&C::parent_record(), C::KIND);
        if let Some(held) = self.children.borrow().get(&key) {
            return Rc::clone(held)
                .downcast::<super::children::ChildrenState<C>>()
                .ok();
        }
        let state = Rc::new(super::children::ChildrenState::<C>::open(
            room?.clone(),
            Rc::new(parent),
        )?);
        self.children
            .borrow_mut()
            .insert(key, Rc::clone(&state) as Rc<dyn Any>);
        self.doors
            .borrow_mut()
            .push(Rc::clone(&state) as Rc<dyn RecordDoor>);
        Some(state)
    }

    pub(crate) fn doors(&self) -> Vec<Rc<dyn RecordDoor>> {
        self.doors.borrow().clone()
    }

    /// Answer one published action of a chain: apply it to the part it
    /// changes, for whoever the host admitted to invoke it.
    pub(crate) fn answer(
        &self,
        me: Option<&str>,
        payload: &[u8],
    ) -> Result<tilo_actions::ActionOutput, String> {
        let request: super::record::RecordAction =
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
        let door = self
            .doors
            .borrow()
            .iter()
            .find(|door| door.id() == request.record)
            .cloned()
            .ok_or_else(|| format!("no chain `{}` is open here", request.record))?;
        let invoker = ui_core::invocation_scope::current_invoker()
            .or_else(|| me.map(ActionInvoker::actor))
            .ok_or("nobody changes a past version")?;
        door.apply_answering(&request.action, &invoker)
    }
}
