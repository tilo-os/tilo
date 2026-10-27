//! Child records: resources a record contains, each a record of its own
//! (`tilo_resource::children`).
//!
//! A [`ChildRecord`] type names the record that lists it (its parent, the
//! app's own record by default) and the container field there. The
//! framework does the rest:
//!
//! - **Held where the parent is.** Every child the container lists (of this
//!   kind) is a record of the app's store beside the parent
//!   ([`RecordId::child`]), opened as it is listed, read as it changes
//!   ([`Children::children`]), synced on its own route like every record of
//!   the app.
//! - **One door.** The container is published beside the app's resource
//!   ([`ui_core::record::children_resource_id`]): every listed child with
//!   its own actions, carried out on its record, checked against the
//!   participant's roles in the room as the app's own actions are,
//!   attributed to whoever invokes them and written into the child's
//!   history.
//! - **One lifecycle.** The container's `create` (a child with its first
//!   fields, under a group, before a sibling), and on each child `delete`
//!   (to the room's trash: its record and id kept, restorable), `restore`
//!   and `move` (to another group or place: its reference moves, the child
//!   and every reference to it stay). The app writes none of these verbs.
//!
//! An app declares a child type in
//! [`ResourceApp::records`](super::ResourceApp::records)
//! (`records.children::<Channel>()`) and reaches it from a view or surface
//! with [`RecordEnv::children`](super::RecordEnv::children).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContext, ActionError, ActionInvoker, ActionParam, ActionSpec, EncodedAction,
    ExecutionLocus, OperationKind, ParamType,
};
use tilo_resource::children::{self as lifecycle, ChildList as _, ChildRef, Children as Container};
use tilo_resource::{Atom, Edit, EditOp, Id, Reader, Resource, Segment};
use ui_core::arriving::{Arrival, Arriving, Source};
use ui_core::permissions::{PermissionScope, TreePath};
use ui_core::record::{RecordCheckout, RecordId, RecordKind, RecordOpenError};
use ui_core::widget::Edits;

use super::chain::{CHAIN_ACTION_TYPE, RoomRecords};
use super::live::Live;
use super::record::RecordDoor;

/// The operation type a child's own actions are carried out under on the
/// container's door: the action and the child it changes.
pub(crate) const CHILD_ACTION_TYPE: &str = "tilo.app.child-action";
/// The operation type the container's lifecycle is carried out under.
pub(crate) const LIFECYCLE_TYPE: &str = "tilo.app.child-lifecycle";

/// A listed child: its id, its place in the container and its state as a
/// read, pending while its record is on its way from the room.
pub type Listed<C> = (Id, ChildRef, Arriving<Rc<<C as Resource>::State>>);

/// Why a listed child is not held.
enum Unheld {
    /// Its record is not on this device yet.
    Missing,
    /// It is here and does not read.
    Unreadable(String),
}

impl From<String> for Unheld {
    fn from(reason: String) -> Self {
        Self::Unreadable(reason)
    }
}

/// A resource whose records are children a record lists. See the
/// module docs.
pub trait ChildRecord: Resource<State: tilo_resource::Model + Default> {
    /// The resource whose record lists these children.
    type Parent: Resource;

    /// The record that lists them: the app's own record in its room.
    #[must_use]
    fn parent_record() -> RecordId {
        RecordId::room_default()
    }

    /// The container of the parent that lists them.
    fn container(parent: &<Self::Parent as Resource>::State) -> &Container;

    /// Where the container is in the parent's document: its field.
    fn container_path() -> Vec<Segment>;

    /// Refuse a place the parent does not have for one of these: under a
    /// group of the wrong kind (a channel inside a channel). Allows any
    /// listed group by default.
    ///
    /// # Errors
    /// Why the parent refuses the place, as the lifecycle says it.
    fn check_group(
        parent: &<Self::Parent as Resource>::State,
        group: Option<&Id>,
    ) -> Result<(), String> {
        let _ = (parent, group);
        Ok(())
    }
}

/// One lifecycle step, as the container's published actions carry it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Lifecycle {
    /// A child with its first `fields` (its model's JSON form, partial),
    /// under `group`, before the sibling `before`.
    Create {
        id: Id,
        group: Option<Id>,
        before: Option<Id>,
        fields: serde_json::Value,
    },
    Delete {
        child: Id,
    },
    Restore {
        child: Id,
    },
    Move {
        child: Id,
        group: Option<Id>,
        before: Option<Id>,
    },
}

impl Lifecycle {
    /// The published action carrying this step out on `door`, the
    /// container's address: the resource it is on, its verb, the child it
    /// concerns and its arguments.
    fn invocation<C: Resource>(
        &self,
        door: &str,
    ) -> (
        String,
        &'static str,
        Id,
        serde_json::Map<String, serde_json::Value>,
    ) {
        let text = |id: &Id| serde_json::Value::String(id.to_string());
        let place = |group: &Option<Id>, before: &Option<Id>| {
            let mut arguments = serde_json::Map::new();
            if let Some(group) = group {
                arguments.insert("group".into(), text(group));
            }
            if let Some(before) = before {
                arguments.insert("before".into(), text(before));
            }
            arguments
        };
        match self {
            Self::Create {
                id,
                group,
                before,
                fields,
            } => {
                let mut arguments = place(group, before);
                arguments.insert("id".into(), text(id));
                if !fields.is_null() {
                    arguments.insert("fields".into(), fields.clone());
                }
                (door.to_owned(), lifecycle::CREATE, id.clone(), arguments)
            }
            Self::Delete { child } => (
                child_resource_id::<C>(child),
                lifecycle::DELETE,
                child.clone(),
                serde_json::Map::new(),
            ),
            Self::Restore { child } => (
                child_resource_id::<C>(child),
                lifecycle::RESTORE,
                child.clone(),
                serde_json::Map::new(),
            ),
            Self::Move {
                child,
                group,
                before,
            } => (
                child_resource_id::<C>(child),
                lifecycle::MOVE,
                child.clone(),
                place(group, before),
            ),
        }
    }
}

/// One of a child's own actions, as the container's door carries it.
#[derive(Serialize, Deserialize)]
struct ChildAction {
    child: Id,
    action: EncodedAction,
}

/// The parent's state, as the lifecycle reads it where it is invoked.
/// A field of the container's item `id`, in the parent's document.
fn item_path<C: ChildRecord>(id: &Id, field: &str) -> Vec<Segment> {
    let mut path = C::container_path();
    path.push(Segment::Item(id.clone()));
    path.push(Segment::Field(field.to_owned()));
    path
}

/// How the container lists `child`, if it lists it as one of `C`.
fn listed<'a, C: ChildRecord>(
    parent: &'a <C::Parent as Resource>::State,
    child: &Id,
) -> Result<&'a ChildRef, ActionError> {
    C::container(parent)
        .get(child)
        .filter(|listed| listed.kind.0 == C::KIND)
        .ok_or_else(|| ActionError::Refused(format!("there is no such {}", C::LABEL)))
}

/// Whether `child` may sit under `group`: the container's rule, then `C`'s.
fn check_place<C: ChildRecord>(
    parent: &<C::Parent as Resource>::State,
    child: &Id,
    group: Option<&Id>,
) -> Result<(), ActionError> {
    lifecycle::check_group(C::container(parent), child, group).map_err(ActionError::Refused)?;
    C::check_group(parent, group).map_err(ActionError::Refused)
}

type Parent<C> = Rc<dyn Fn() -> Rc<<<C as ChildRecord>::Parent as Resource>::State>>;

/// The children of one kind a container lists, held for a window. See the
/// [module docs](self).
pub(crate) struct ChildrenState<C: ChildRecord> {
    room: RoomRecords,
    parent: Parent<C>,
    id: String,
    kind: RecordKind,
    held: RefCell<BTreeMap<Id, Live<C>>>,
    /// Each listed child as a read: pending while its record is on its way
    /// from the room, its state once it is here.
    arrivals: RefCell<BTreeMap<Id, Arriving<Rc<C::State>>>>,
    /// Why a listed child that is here does not read, by id.
    unreadable: RefCell<BTreeMap<Id, String>>,
    /// A child before anything was written to it: what the container
    /// publishes for one whose record has not arrived.
    empty: Rc<C::State>,
    revision: Cell<u64>,
    published: RefCell<Option<((u64, u64), tilo_actions::Resource)>>,
    /// The door of the parent the window shows, which the lifecycle writes
    /// through in the pass that lends it; `None` where the parent is held
    /// in the store (a tool's, a test's).
    parent_door: RefCell<Option<super::Dispatcher<C::Parent>>>,
}

impl<C: ChildRecord> ChildrenState<C> {
    pub(crate) fn open(room: RoomRecords, parent: Parent<C>) -> Option<Self> {
        let mut doc = ui_core::automerge::AutoCommit::new();
        C::initialize(&mut doc)
            .map_err(|error| tracing::warn!(kind = C::KIND, %error, "child has no genesis"))
            .ok()?;
        let empty = Reader::<C>::read(&doc)
            .map_err(|error| tracing::warn!(kind = C::KIND, %error, "child unreadable"))
            .ok()?
            .state()
            .clone();
        let state = Self {
            room,
            parent,
            id: ui_core::record::children_resource_id(&C::parent_record(), C::KIND),
            kind: RecordKind::new(C::KIND),
            held: RefCell::default(),
            arrivals: RefCell::default(),
            unreadable: RefCell::default(),
            empty,
            revision: Cell::new(0),
            published: RefCell::new(None),
            parent_door: RefCell::new(None),
        };
        state.refresh();
        Some(state)
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Write the parent through `door`, the window's own: the parent is the
    /// record it shows.
    pub(crate) fn write_parent_through(&self, door: super::Dispatcher<C::Parent>) {
        *self.parent_door.borrow_mut() = Some(door);
    }

    fn record(child: &Id) -> Result<RecordId, String> {
        C::parent_record()
            .child(child.as_str())
            .ok_or_else(|| format!("`{child}` cannot name a record"))
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// The children of this kind the container lists, in order, the
    /// trashed among them.
    fn listed(&self) -> Vec<(Id, ChildRef)> {
        let parent = (self.parent)();
        C::container(&parent)
            .iter()
            .filter(|(_, child)| child.kind.0 == C::KIND)
            .map(|(id, child)| (id.clone(), child.clone()))
            .collect()
    }

    /// Every child outside the trash with its state as a read, in the
    /// container's order: pending while its record is on its way from the
    /// room.
    pub(crate) fn children(&self) -> Vec<Listed<C>> {
        self.refresh();
        let arrivals = self.arrivals.borrow();
        self.listed()
            .into_iter()
            .filter(|(_, child)| child.trashed.is_none())
            .filter_map(|(id, child)| {
                let arriving = arrivals.get(&id)?.clone();
                Some((id, child, arriving))
            })
            .collect()
    }

    /// Bring each listed child's read to where its record stands: its
    /// state once it is here, why it does not read, or on its way from the
    /// room. Children no longer listed are let go.
    fn settle(&self) {
        let listed = self.listed();
        let held = self.held.borrow();
        let unreadable = self.unreadable.borrow();
        let mut arrivals = self.arrivals.borrow_mut();
        arrivals.retain(|id, _| listed.iter().any(|(listed, _)| listed == id));
        for (id, _) in listed {
            let now = match (held.get(&id), unreadable.get(&id)) {
                (Some(live), _) => Arrival::Ready(live.get()),
                (None, Some(reason)) => Arrival::Failed(reason.clone()),
                (None, None) => Arrival::Pending {
                    from: ui_core::room_sync::source(),
                },
            };
            match arrivals.get(&id) {
                None => {
                    let arriving = Arriving::pending(Source::Local);
                    arriving.stand(now);
                    arrivals.insert(id, arriving);
                }
                Some(arriving) => {
                    let same = arriving.with(|was| match (was, &now) {
                        (Arrival::Ready(was), Arrival::Ready(now)) => Rc::ptr_eq(was, now),
                        (Arrival::Failed(was), Arrival::Failed(now)) => was == now,
                        (Arrival::Pending { from: was }, Arrival::Pending { from: now }) => {
                            was == now
                        }
                        _ => false,
                    });
                    if !same {
                        arriving.stand(now);
                    }
                }
            }
        }
    }

    /// The child `id`, listed or in the trash, when its record is here.
    pub(crate) fn child(&self, id: &Id) -> Option<Rc<C::State>> {
        self.refresh();
        self.held.borrow().get(id).map(Live::get)
    }

    pub(crate) fn revision(&self) -> u64 {
        self.refresh();
        self.revision.get()
    }

    /// Follow every held child that changed and hold every child the
    /// container lists that is here now. Whether anything moved.
    pub(crate) fn refresh(&self) -> bool {
        let mut moved = false;
        for (id, live) in self.held.borrow().iter() {
            let Ok(record) = Self::record(id) else {
                continue;
            };
            match self.room.store.read(&record, |doc| live.sync(doc)) {
                Some(Ok(true)) => moved = true,
                Some(Err(error)) => {
                    tracing::warn!(kind = C::KIND, %id, %error, "child unreadable");
                }
                _ => {}
            }
            let _ = live.take_change();
        }
        let missing: Vec<Id> = {
            let held = self.held.borrow();
            self.listed()
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| !held.contains_key(id))
                .collect()
        };
        for id in missing {
            match self.hold(&id) {
                Ok(()) => {
                    self.unreadable.borrow_mut().remove(&id);
                    moved = true;
                }
                // Not here yet: it is on its way from the room.
                Err(Unheld::Missing) => {}
                Err(Unheld::Unreadable(reason)) => {
                    self.unreadable.borrow_mut().insert(id, reason);
                }
            }
        }
        if moved {
            self.moved();
        }
        self.settle();
        moved
    }

    /// Hold child `id`: read its record from this device, which asks the
    /// room for it when it has not got it yet.
    fn hold(&self, id: &Id) -> Result<(), Unheld> {
        if self.held.borrow().contains_key(id) {
            return Ok(());
        }
        let checkout = RecordCheckout::new(Self::record(id)?, self.kind.clone());
        if !self.room.store.is_ready(&checkout.id) {
            if !self.room.store.takes_writes() {
                return Err(Unheld::Missing);
            }
            match self.room.store.try_open_record(&checkout) {
                Ok(_) => {}
                Err(RecordOpenError::MissingRecord | RecordOpenError::Busy) => {
                    return Err(Unheld::Missing);
                }
                Err(error) => return Err(Unheld::Unreadable(error.to_string())),
            }
        }
        Ok(self.follow(id)?)
    }

    fn follow(&self, id: &Id) -> Result<(), String> {
        let record = Self::record(id)?;
        let stored = self
            .room
            .store
            .read(&record, tilo_resource::evolution::stored_version)
            .ok_or("the child is not open here")?;
        if stored < <C as Resource>::VERSION {
            if !self.room.store.takes_writes() {
                return Err("migrated once the store takes writes".into());
            }
            self.room
                .store
                .with(&record, tilo_resource::evolution::migrate::<C>)
                .ok_or("the child is not open here")?
                .map_err(|error| error.to_string())?;
        }
        let reader = self
            .room
            .store
            .read(&record, Reader::<C>::read)
            .ok_or("the child is not open here")?
            .map_err(|error| error.to_string())?;
        self.held.borrow_mut().insert(id.clone(), Live::new(reader));
        self.moved();
        Ok(())
    }

    /// Make child `id`'s record with the child type's genesis.
    fn start(&self, id: &Id) -> Result<(), String> {
        let checkout = RecordCheckout::new(Self::record(id)?, self.kind.clone());
        if !self.room.store.is_ready(&checkout.id) {
            let initialize = |doc: &mut ui_core::automerge::AutoCommit| {
                C::initialize(doc).map_err(|error| error.to_string())
            };
            match self.room.store.reserve_record(&checkout) {
                Ok(mut reservation) => reservation
                    .initialize(initialize)
                    .map_err(|error| error.to_string())?,
                // A store that keeps no canonical records (a test's)
                // creates it the way it creates every record.
                Err(RecordOpenError::IdentityMismatch) => {
                    self.room
                        .store
                        .create_record(&checkout)
                        .map_err(|error| error.to_string())?;
                    self.room
                        .store
                        .with(&checkout.id, |doc| {
                            initialize(doc)?;
                            doc.commit();
                            Ok::<(), String>(())
                        })
                        .ok_or("the new child is not open")??;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        self.follow(id)
    }

    /// Whether `invoker` may take a step `kind` on `verb` of this
    /// container, as the room's roles say.
    fn may(&self, verb: &str, kind: tilo_actions::ActionKind) -> bool {
        self.room.permissions.as_ref().is_none_or(|permissions| {
            permissions.check(&PermissionScope::Action {
                node: tilo_actions::action_node(
                    &TreePath::root(),
                    self.room.app_id,
                    C::KIND,
                    &self.id,
                    None,
                    verb,
                ),
                kind,
            })
        })
    }

    /// Write `edits` into the parent, as `invoker`, recorded in its
    /// history: the container's lifecycle. The parent the window shows is
    /// written through its own door in the pass that lends it (`live`);
    /// one held in the store, as a tool holds it, there.
    fn edit_parent(
        &self,
        verb: &str,
        edits: Vec<Edit<C::Parent>>,
        invoker: &ActionInvoker,
        mut live: Option<&mut Edits<'_>>,
    ) -> Result<(), ActionError> {
        let record = C::parent_record();
        let door = self.parent_door.borrow().clone();
        for edit in edits {
            let action = <C::Parent as Resource>::edit_action(edit)
                .ok_or_else(|| ActionError::Refused(format!("{} take no field edits", C::LABEL)))?;
            if let Some(door) = &door {
                let live = live.as_mut().ok_or_else(|| {
                    ActionError::Refused(format!("the record listing {verb} is not lent here"))
                })?;
                door.apply_now_for(live, &action, invoker)?;
                continue;
            }
            let encoded = tilo_resource::encode::<C::Parent>(&action)?;
            let context = ActionContext::for_resource(&self.id, C::KIND, C::LABEL)
                .with_app(self.room.app_id, self.room.app_title)
                .with_invoker(invoker.clone())
                .with_change(tilo_actions::Action::change_kind(&action));
            self.room
                .store
                .with_history(&record, |doc, history| {
                    let writer = ui_core::bound_dispatch::document_writer(&invoker.actor, doc);
                    let applied = tilo_actions::apply_local_with_context(
                        doc,
                        &action,
                        encoded,
                        Some(&writer),
                        context,
                    )
                    .map(drop)
                    .map_err(|error| ActionError::Refused(error.to_string()));
                    history.rebase(doc);
                    applied
                })
                .ok_or_else(|| {
                    ActionError::Refused(format!("the record listing {verb} is not open here"))
                })??;
            self.room.store.schedule_committed_history(&record);
        }
        self.refresh();
        self.moved();
        Ok(())
    }

    /// Carry out one lifecycle step for `invoker`. The child it concerns.
    ///
    /// # Errors
    /// The participant may not, the step names no child of this kind, the
    /// place is refused, or a record could not be written.
    pub(crate) fn lifecycle(
        &self,
        step: &Lifecycle,
        invoker: &ActionInvoker,
        now_ms: i64,
        live: Option<&mut Edits<'_>>,
    ) -> Result<Id, ActionError> {
        use tilo_actions::ActionKind;
        let verb = match step {
            Lifecycle::Create { .. } => lifecycle::CREATE,
            Lifecycle::Delete { .. } => lifecycle::DELETE,
            Lifecycle::Restore { .. } => lifecycle::RESTORE,
            Lifecycle::Move { .. } => lifecycle::MOVE,
        };
        if !self.may(verb, ActionKind::Undoable) {
            return Err(ActionError::Refused(format!(
                "you may not {verb} these {}",
                C::LABEL.to_lowercase()
            )));
        }
        let parent = (self.parent)();
        match step {
            Lifecycle::Create {
                id,
                group,
                before,
                fields,
            } => self.create_child(
                &parent,
                id,
                group.as_ref(),
                before.as_ref(),
                fields,
                invoker,
                live,
            ),
            Lifecycle::Delete { child } => self.delete_child(&parent, child, invoker, now_ms, live),
            Lifecycle::Restore { child } => self.restore_child(&parent, child, invoker, live),
            Lifecycle::Move {
                child,
                group,
                before,
            } => self.move_child(
                &parent,
                child,
                group.as_ref(),
                before.as_ref(),
                invoker,
                live,
            ),
        }
    }

    /// A new child `id` under `group`, before `before`, with its first fields.
    fn create_child(
        &self,
        parent: &<C::Parent as Resource>::State,
        id: &Id,
        group: Option<&Id>,
        before: Option<&Id>,
        fields: &serde_json::Value,
        invoker: &ActionInvoker,
        live: Option<&mut Edits<'_>>,
    ) -> Result<Id, ActionError> {
        let container = C::container(parent);
        if container.contains(id) {
            return Err(ActionError::Refused("that child exists already".into()));
        }
        check_place::<C>(parent, id, group)?;
        let first = first_fields::<C>(fields).map_err(ActionError::Refused)?;
        self.start(id).map_err(ActionError::Refused)?;
        if !first.is_empty() {
            self.edit_child(id, first, invoker)?;
        }
        let at = lifecycle::place(container, before);
        let reference = ChildRef {
            kind: Atom(C::KIND.to_owned()),
            group: group.cloned(),
            trashed: None,
        };
        self.edit_parent(
            lifecycle::CREATE,
            vec![Edit::new(
                C::container_path(),
                EditOp::InsertItem {
                    id: Some(id.clone()),
                    at: at.into(),
                    value: tilo_resource::Model::to_json(&reference),
                },
            )],
            invoker,
            live,
        )?;
        Ok(id.clone())
    }

    /// `child` to the trash, whole.
    fn delete_child(
        &self,
        parent: &<C::Parent as Resource>::State,
        child: &Id,
        invoker: &ActionInvoker,
        now_ms: i64,
        live: Option<&mut Edits<'_>>,
    ) -> Result<Id, ActionError> {
        if listed::<C>(parent, child)?.trashed.is_some() {
            return Ok(child.clone());
        }
        let trashed = lifecycle::Trashed { at: now_ms };
        self.edit_parent(
            lifecycle::DELETE,
            vec![Edit::new(
                item_path::<C>(child, "trashed"),
                EditOp::Set {
                    value: tilo_resource::Model::to_json(&Some(trashed)),
                },
            )],
            invoker,
            live,
        )?;
        Ok(child.clone())
    }

    /// `child` back from the trash.
    fn restore_child(
        &self,
        parent: &<C::Parent as Resource>::State,
        child: &Id,
        invoker: &ActionInvoker,
        live: Option<&mut Edits<'_>>,
    ) -> Result<Id, ActionError> {
        let listed = listed::<C>(parent, child)?;
        if listed.trashed.is_none() {
            return Ok(child.clone());
        }
        // Its group may have gone to the trash meanwhile: it comes back at
        // the top then.
        let mut edits = vec![Edit::new(
            item_path::<C>(child, "trashed"),
            EditOp::Set {
                value: serde_json::Value::Null,
            },
        )];
        if listed
            .group
            .as_ref()
            .is_some_and(|group| !C::container(parent).lists(group))
        {
            edits.push(Edit::new(
                item_path::<C>(child, "group"),
                EditOp::Set {
                    value: serde_json::Value::Null,
                },
            ));
        }
        self.edit_parent(lifecycle::RESTORE, edits, invoker, live)?;
        Ok(child.clone())
    }

    /// `child` under `group`, before `before`.
    fn move_child(
        &self,
        parent: &<C::Parent as Resource>::State,
        child: &Id,
        group: Option<&Id>,
        before: Option<&Id>,
        invoker: &ActionInvoker,
        live: Option<&mut Edits<'_>>,
    ) -> Result<Id, ActionError> {
        listed::<C>(parent, child)?;
        check_place::<C>(parent, child, group)?;
        let container = C::container(parent);
        let at = lifecycle::place(container, before);
        let from = container.position(child).unwrap_or_default();
        // Taken out first, the place after it is one less.
        let at = if at > from { at.saturating_sub(1) } else { at };
        let edits = vec![
            Edit::new(
                item_path::<C>(child, "group"),
                EditOp::Set {
                    value: tilo_resource::Model::to_json(&group.cloned()),
                },
            ),
            Edit::new(
                C::container_path(),
                EditOp::MoveTo {
                    id: child.clone(),
                    at: at.into(),
                },
            ),
        ];
        self.edit_parent(lifecycle::MOVE, edits, invoker, live)?;
        Ok(child.clone())
    }

    /// Write the first fields of a new child into its record.
    fn edit_child(
        &self,
        child: &Id,
        edits: Vec<Edit<C>>,
        invoker: &ActionInvoker,
    ) -> Result<(), ActionError> {
        for edit in edits {
            let action = C::edit_action(edit)
                .ok_or_else(|| ActionError::Refused(format!("{} take no field edits", C::LABEL)))?;
            self.write(child, &action, invoker)?;
        }
        Ok(())
    }

    /// Apply `action`, one of child `child`'s own, for `invoker`: checked
    /// against what they may do in the room, attributed to them and
    /// written into the child's history.
    ///
    /// # Errors
    /// The participant may not, the child is not here, or it refused.
    pub(crate) fn apply_to(
        &self,
        child: &Id,
        action: &C::Action,
        invoker: &ActionInvoker,
    ) -> Result<Vec<String>, ActionError> {
        let need = tilo_resource::need_of::<C>(action);
        if let Some(permissions) = &self.room.permissions {
            let node = TreePath::root()
                .app(self.room.app_id)
                .kind(C::KIND)
                .resource(self.id.clone())
                .field(need.field.as_deref().unwrap_or_default());
            if !permissions.check(&PermissionScope::Action {
                node,
                kind: need.kind,
            }) {
                return Err(ActionError::Refused(format!(
                    "you may not do this to these {}",
                    C::LABEL.to_lowercase()
                )));
            }
        }
        self.hold(child).map_err(|unheld| {
            ActionError::Refused(match unheld {
                Unheld::Missing => "it has not arrived on this device yet".to_owned(),
                Unheld::Unreadable(reason) => reason,
            })
        })?;
        self.write(child, action, invoker)
    }

    fn write(
        &self,
        child: &Id,
        action: &C::Action,
        invoker: &ActionInvoker,
    ) -> Result<Vec<String>, ActionError> {
        let encoded = tilo_resource::encode::<C>(action)?;
        let context = ActionContext::for_resource(child_resource_id::<C>(child), C::KIND, C::LABEL)
            .with_app(self.room.app_id, self.room.app_title)
            .with_invoker(invoker.clone())
            .with_change(tilo_actions::Action::change_kind(action));
        let record = Self::record(child).map_err(ActionError::Refused)?;
        let created = self
            .room
            .store
            .with_history(&record, |doc, history| {
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
                    ui_core::bound_dispatch::minted_items(doc, C::KIND, &envelope, &before)
                })
                .map_err(|error| ActionError::Refused(error.to_string()));
                history.rebase(doc);
                applied
            })
            .ok_or_else(|| ActionError::Refused("the child is not open here".into()))??;
        self.room.store.schedule_committed_history(&record);
        self.refresh();
        self.moved();
        Ok(created)
    }

    /// The container as it is published: `create`, and every child with
    /// its own actions, `delete` and `move` (a trashed one with `restore`),
    /// carried out on the container's door.
    pub(crate) fn publish(&self) -> tilo_actions::Resource {
        let revision = (self.revision(), ui_core::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let mut resource =
            tilo_actions::Resource::new(self.id.clone(), format!("{}.children", C::KIND), C::LABEL)
                .with_action(self.route(create_spec::<C>()));
        let held = self.held.borrow();
        for (id, child) in self.listed() {
            let state = held
                .get(&id)
                .map_or_else(|| Rc::clone(&self.empty), Live::get);
            let address = child_resource_id::<C>(&id);
            let label = tilo_resource::Model::label(&*state).unwrap_or_else(|| id.to_string());
            let mut published = if child.trashed.is_some() {
                tilo_actions::Resource::new(address, C::KIND, label)
                    .with_action(self.route(step_spec(lifecycle::RESTORE, "Restore", &id)))
            } else {
                let own = tilo_resource::publish::<C>(&address, &label, &state);
                route_child(own, &self.id, &id)
                    .with_action(self.route(step_spec(lifecycle::DELETE, "Move to Trash", &id)))
                    .with_action(self.route(move_spec(&id)))
            };
            published.actions.dedup_by(|a, b| a.id == b.id);
            resource = resource.with_child(published);
        }
        drop(held);
        *self.published.borrow_mut() = Some((revision, resource.clone()));
        resource
    }

    /// `spec` carried out on this container's door.
    fn route(&self, spec: ActionSpec) -> ActionSpec {
        super::record::route_operation(spec, CHAIN_ACTION_TYPE, &self.id)
    }
}

/// The address child `id` of `C` is published under: its kind and its id,
/// wherever it is listed, so moving it keeps every reference to it.
#[must_use]
pub fn child_resource_id<C: Resource>(id: &Id) -> String {
    format!("{}/{id}", C::KIND)
}

/// The edits that give a new `C` its first `fields` (its JSON form, any of
/// its fields), each set on its own.
fn first_fields<C: ChildRecord>(fields: &serde_json::Value) -> Result<Vec<Edit<C>>, String> {
    let fields = match fields {
        serde_json::Value::Null => return Ok(Vec::new()),
        serde_json::Value::Object(fields) => fields,
        _ => return Err("a new child's fields are an object".into()),
    };
    // The whole value has to read as one of these, so nothing unknown or
    // ill-typed is written.
    let mut whole = tilo_resource::Model::to_json(&C::State::default());
    if let serde_json::Value::Object(defaults) = &mut whole {
        for (key, value) in fields {
            if !defaults.contains_key(key) {
                return Err(format!("{} have no `{key}`", C::LABEL));
            }
            defaults.insert(key.clone(), value.clone());
        }
    }
    <C::State as tilo_resource::Model>::from_json(&whole).map_err(|error| error.to_string())?;
    Ok(fields
        .iter()
        .map(|(key, value)| {
            Edit::new(
                vec![Segment::Field(key.clone())],
                EditOp::Set {
                    value: value.clone(),
                },
            )
        })
        .collect())
}

/// `resource`'s document actions, and those of its children, carried out on
/// child `child`'s record through the container's door `door`.
fn route_child(
    mut resource: tilo_actions::Resource,
    door: &str,
    child: &Id,
) -> tilo_actions::Resource {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .filter(|spec| spec.kind == OperationKind::Document)
        .map(|spec| wrap_child(spec, door, child))
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|nested| route_child(nested, door, child))
        .collect();
    resource
}

/// `spec` carried out on child `child` through the container's door: one
/// routing, whose operation names the door and whose action the child.
fn wrap_child(spec: ActionSpec, door: &str, child: &Id) -> ActionSpec {
    let child = child.clone();
    let door = door.to_owned();
    spec.routed(CHAIN_ACTION_TYPE, move |action| {
        let routed = EncodedAction::encode_operation(
            CHILD_ACTION_TYPE,
            &ChildAction {
                child: child.clone(),
                action,
            },
        )
        .and_then(|action| {
            EncodedAction::encode_operation(
                CHAIN_ACTION_TYPE,
                &super::record::RecordAction {
                    record: door.clone(),
                    action,
                },
            )
        });
        routed.unwrap_or_else(|error| invalid(&error.to_string()))
    })
}

fn invalid(error: &str) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
        payload: error.as_bytes().to_vec(),
    }
}

fn encode_step(step: &Lifecycle) -> EncodedAction {
    EncodedAction::encode_operation(LIFECYCLE_TYPE, step)
        .unwrap_or_else(|error| invalid(&error.to_string()))
}

fn text_arg(args: &tilo_actions::ActionArgs, name: &str) -> Option<Id> {
    match args.get(name) {
        Some(tilo_actions::ArgValue::Text(text) | tilo_actions::ArgValue::Resource(text))
            if !text.is_empty() =>
        {
            // A resource address names the child by its last segment.
            Some(Id::from_raw(text.rsplit('/').next().unwrap_or(text)))
        }
        _ => None,
    }
}

/// The container's `create`: a child with its first fields, under a group,
/// before a sibling.
fn create_spec<C: Resource>() -> ActionSpec {
    ActionSpec::typed(
        lifecycle::CREATE,
        format!("New {}", C::LABEL.to_lowercase()),
        "fields",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::optional("fields", ParamType::Object, "Its first fields"),
            ActionParam::optional("group", ParamType::Text, "The child to list it under"),
            ActionParam::optional("before", ParamType::Text, "The child to place it before"),
            ActionParam::optional(
                "id",
                ParamType::Text,
                "The id it is made under; a new one without",
            ),
        ],
        |args| {
            let fields = match args.get("fields") {
                Some(tilo_actions::ArgValue::Object(fields)) => {
                    serde_json::Value::Object(fields.clone())
                }
                _ => serde_json::Value::Null,
            };
            encode_step(&Lifecycle::Create {
                id: text_arg(args, "id")
                    .unwrap_or_else(|| Id::from_raw(tilo_resource::fresh_uuid().to_string())),
                group: text_arg(args, "group"),
                before: text_arg(args, "before"),
                fields,
            })
        },
    )
}

/// A child's `delete` or `restore`.
fn step_spec(verb: &'static str, label: &str, child: &Id) -> ActionSpec {
    let child = child.clone();
    ActionSpec::typed(
        verb,
        label,
        "",
        ExecutionLocus::ClientLocal,
        Vec::new(),
        move |_| {
            let child = child.clone();
            encode_step(&match verb {
                lifecycle::DELETE => Lifecycle::Delete { child },
                _ => Lifecycle::Restore { child },
            })
        },
    )
}

/// A child's `move`: under another group, before another sibling.
fn move_spec(child: &Id) -> ActionSpec {
    let child = child.clone();
    ActionSpec::typed(
        lifecycle::MOVE,
        "Move",
        "group",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::optional("group", ParamType::Text, "The child to list it under"),
            ActionParam::optional("before", ParamType::Text, "The child to place it before"),
        ],
        move |args| {
            encode_step(&Lifecycle::Move {
                child: child.clone(),
                group: text_arg(args, "group"),
                before: text_arg(args, "before"),
            })
        },
    )
}

impl<C: ChildRecord> RecordDoor for ChildrenState<C> {
    fn id(&self) -> &str {
        &self.id
    }

    fn editable(&self) -> bool {
        true
    }

    fn revision(&self) -> u64 {
        ChildrenState::revision(self)
    }

    fn publish(&self) -> tilo_actions::Resource {
        ChildrenState::publish(self)
    }

    fn apply_encoded(&self, action: &EncodedAction, invoker: &ActionInvoker) -> Result<(), String> {
        self.apply_answering(action, invoker).map(drop)
    }

    fn apply_in(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
        live: &mut Edits<'_>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.answer(action, invoker, Some(live))
    }

    fn apply_answering(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.answer(action, invoker, None)
    }
}

impl<C: ChildRecord> ChildrenState<C> {
    /// Carry out one published action of the container for `invoker`, the
    /// window lending the parent it shows as `live`.
    fn answer(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
        live: Option<&mut Edits<'_>>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        match action.type_id.as_str() {
            LIFECYCLE_TYPE => {
                let step: Lifecycle = tilo_actions::decode_json(&action.payload)
                    .map_err(|error| error.to_string())?;
                let child = self
                    .lifecycle(&step, invoker, tilo_resource::origin_now_ms(), live)
                    .map_err(|error| error.to_string())?;
                Ok(tilo_actions::ActionOutput {
                    value: None,
                    resources: vec![child_resource_id::<C>(&child)],
                })
            }
            CHILD_ACTION_TYPE => {
                let request: ChildAction = tilo_actions::decode_json(&action.payload)
                    .map_err(|error| error.to_string())?;
                if request.action.type_id != C::ACTION_TYPE {
                    return Err(format!(
                        "{} take no `{}` action",
                        C::LABEL,
                        request.action.type_id
                    ));
                }
                let typed: C::Action = tilo_actions::decode_json(&request.action.payload)
                    .map_err(|error| error.to_string())?;
                let resources = self
                    .apply_to(&request.child, &typed, invoker)
                    .map_err(|error| error.to_string())?;
                Ok(tilo_actions::ActionOutput {
                    value: None,
                    resources,
                })
            }
            other => Err(format!("{} take no `{other}` action", C::LABEL)),
        }
    }
}

/// The children of `C` a record lists, held for a view or surface. See the
/// module docs.
pub struct Children<C: ChildRecord> {
    state: Rc<ChildrenState<C>>,
    me: String,
    /// The door of the parent the view shows, which the lifecycle a view
    /// asks for goes through.
    dispatch: Option<super::Dispatcher<C::Parent>>,
}

impl<C: ChildRecord> Clone for Children<C> {
    fn clone(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
            me: self.me.clone(),
            dispatch: self.dispatch.clone(),
        }
    }
}

impl<C: ChildRecord> Children<C> {
    pub(crate) fn new(state: Rc<ChildrenState<C>>, me: String) -> Self {
        Self {
            state,
            me,
            dispatch: None,
        }
    }

    /// These, submitting through `parent`'s door for the view that holds
    /// them: the door of the parent it shows.
    pub(crate) fn dispatching(mut self, parent: &super::Dispatcher<C::Parent>) -> Self {
        self.state.write_parent_through(parent.clone());
        self.dispatch = Some(parent.clone());
        self
    }

    /// The address the container is published under.
    #[must_use]
    pub fn id(&self) -> &str {
        self.state.id()
    }

    /// Every child outside the trash, with its place and its state as a
    /// read, in order: a child whose record is on its way from the room is
    /// pending. Show it with [`ViewCx::arriving`](super::ViewCx::arriving).
    #[must_use]
    pub fn children(&self) -> Vec<Listed<C>> {
        self.state.children()
    }

    /// Child `id`, listed or in the trash, when its record is here.
    #[must_use]
    pub fn child(&self, id: &Id) -> Option<Rc<C::State>> {
        self.state.child(id)
    }

    /// Increases whenever a child or the container changed.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.state.revision()
    }

    /// Make a child with its first `fields` (its JSON form, partial) under
    /// `group`, before `before`, as the window's person. Its id.
    ///
    /// # Errors
    /// As the door refuses `create`.
    pub fn create(
        &self,
        fields: serde_json::Value,
        group: Option<&Id>,
        before: Option<&Id>,
    ) -> Result<Id, ActionError> {
        self.step(&Lifecycle::Create {
            id: Id::from_raw(tilo_resource::fresh_uuid().to_string()),
            group: group.cloned(),
            before: before.cloned(),
            fields,
        })
    }

    /// Put child `id` in the room's trash, as the window's person.
    ///
    /// # Errors
    /// As the door refuses `delete`.
    pub fn delete(&self, id: &Id) -> Result<(), ActionError> {
        self.step(&Lifecycle::Delete { child: id.clone() })
            .map(drop)
    }

    /// Take child `id` out of the trash, as the window's person.
    ///
    /// # Errors
    /// As the door refuses `restore`.
    pub fn restore(&self, id: &Id) -> Result<(), ActionError> {
        self.step(&Lifecycle::Restore { child: id.clone() })
            .map(drop)
    }

    /// Move child `id` under `group`, before `before`, as the window's
    /// person.
    ///
    /// # Errors
    /// As the door refuses `move`.
    pub fn move_to(
        &self,
        id: &Id,
        group: Option<&Id>,
        before: Option<&Id>,
    ) -> Result<(), ActionError> {
        self.step(&Lifecycle::Move {
            child: id.clone(),
            group: group.cloned(),
            before: before.cloned(),
        })
        .map(drop)
    }

    /// Apply one of child `id`'s own actions, as the window's person.
    ///
    /// # Errors
    /// As the door refuses it.
    pub fn apply(&self, id: &Id, action: &C::Action) -> Result<(), ActionError> {
        self.state
            .apply_to(id, action, &ActionInvoker::actor(&self.me))
            .map(drop)
    }

    /// Carry out `step` as the window's person: in a view, through the
    /// door, which writes the parent the window shows in the pass that
    /// lends it (a refusal is the view's dispatcher's to tell); held
    /// outside a window, now.
    fn step(&self, step: &Lifecycle) -> Result<Id, ActionError> {
        let Some(parent) = &self.dispatch else {
            return self.state.lifecycle(
                step,
                &ActionInvoker::actor(&self.me),
                tilo_resource::origin_now_ms(),
                None,
            );
        };
        let (resource, verb, child, arguments) = step.invocation::<C>(self.state.id());
        parent.invoke_at(
            tilo_actions::door::ActionAddress::new(resource, verb),
            arguments,
        );
        Ok(child)
    }

    /// The container published as the door publishes it, for a test.
    #[must_use]
    pub fn published(&self) -> tilo_actions::Resource {
        self.state.publish()
    }

    /// Carry out a published action of the container, as the door does:
    /// what an agent invoking it reaches.
    ///
    /// # Errors
    /// As the door refuses it.
    pub fn invoke_published(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let request: super::record::RecordAction =
            tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
        if request.record != self.state.id() {
            return Err(format!("`{}` is another record", request.record));
        }
        self.state.apply_answering(&request.action, invoker)
    }

    /// Children held outside any host, for a tool or a test: in `store`,
    /// which holds the parent's record, edited as `me`, who may do
    /// anything, the parent read from `parent`. `None` when `C` does not
    /// read as a child.
    #[must_use]
    pub fn detached_in(
        store: ui_core::record::RecordStore,
        app_id: &'static str,
        me: &str,
        parent: impl Fn() -> Rc<<C::Parent as Resource>::State> + 'static,
    ) -> Option<Self> {
        let room = RoomRecords {
            store,
            me: me.to_owned(),
            permissions: None,
            app_id,
            app_title: app_id,
        };
        let state = Rc::new(ChildrenState::<C>::open(room, Rc::new(parent))?);
        Some(Self::new(state, me.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use tilo_resource::{Model, resource};
    use ui_core::record::RecordStore;

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Room {
        pub entries: Container,
    }

    #[resource(kind = "test.room", label = "Room")]
    impl Room {
        /// Rename nothing.
        #[action]
        #[expect(
            clippy::unused_self,
            clippy::needless_pass_by_ref_mut,
            reason = "an action takes the state it acts on"
        )]
        pub fn touch(&mut self) {}
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Topic {
        #[model(label)]
        pub name: String,
        pub note: String,
    }

    #[resource(kind = "test.topic", label = "Topics")]
    impl Topic {
        /// Note.
        #[action]
        pub fn jot(&mut self, text: String) {
            self.note = text;
        }
    }

    impl ChildRecord for Topic {
        type Parent = Room;
        fn container(parent: &Room) -> &Container {
            &parent.entries
        }
        fn container_path() -> Vec<Segment> {
            vec![Segment::Field("entries".into())]
        }
    }

    /// A room's record in a store of its own, and its topics held over it.
    fn room() -> (RecordStore, Children<Topic>) {
        let store = RecordStore::in_memory();
        let parent = RecordCheckout::new(RecordId::room_default(), RecordKind::new(Room::KIND));
        store.create_record(&parent).unwrap();
        store
            .with(&parent.id, |doc| {
                Room::initialize(doc).unwrap();
                doc.commit();
            })
            .unwrap();
        let read = store.clone();
        let children =
            Children::<Topic>::detached_in(store.clone(), "so.tilo.test", "ana", move || {
                read.read(&RecordId::room_default(), |doc| {
                    Reader::<Room>::read(doc).unwrap().state().clone()
                })
                .unwrap()
            })
            .unwrap();
        (store, children)
    }

    fn order(children: &Children<Topic>) -> Vec<(String, Option<String>)> {
        children
            .children()
            .into_iter()
            .map(|(_, child, state)| {
                let group = child.group.map(|group| {
                    children
                        .child(&group)
                        .map_or_else(|| group.to_string(), |topic| topic.name.clone())
                });
                let name = state.with(|state| state.ready().map(|topic| topic.name.clone()));
                (name.expect("every child here"), group)
            })
            .collect()
    }

    #[test]
    fn a_child_is_a_record_of_its_own_and_moving_it_keeps_its_identity() {
        let (store, children) = room();
        let design = children
            .create(serde_json::json!({ "name": "design" }), None, None)
            .unwrap();
        let ui = children
            .create(serde_json::json!({ "name": "ui" }), Some(&design), None)
            .unwrap();
        let general = children
            .create(serde_json::json!({ "name": "general" }), None, None)
            .unwrap();
        let record = RecordId::room_default().child(ui.as_str()).unwrap();
        assert_eq!(
            store.read(&record, |doc| Reader::<Topic>::read(doc)
                .unwrap()
                .state()
                .name
                .clone()),
            Some("ui".to_owned())
        );
        assert_eq!(
            order(&children),
            [
                ("design".to_owned(), None),
                ("ui".to_owned(), Some("design".to_owned())),
                ("general".to_owned(), None),
            ]
        );
        // Moved out of its group and to the front: the same record, the
        // same address, only its reference moved.
        children.move_to(&ui, None, Some(&design)).unwrap();
        assert_eq!(
            order(&children),
            [
                ("ui".to_owned(), None),
                ("design".to_owned(), None),
                ("general".to_owned(), None),
            ]
        );
        assert_eq!(children.child(&ui).unwrap().name, "ui");
        assert!(store.read(&record, |_| ()).is_some());
        children.move_to(&general, Some(&design), None).unwrap();
        assert_eq!(
            order(&children)[2],
            ("general".to_owned(), Some("design".to_owned()))
        );
        // Nothing is grouped under itself, nor under what is not there.
        assert!(children.move_to(&design, Some(&design), None).is_err());
        assert!(
            children
                .move_to(&design, Some(&Id::from_raw("absent")), None)
                .is_err()
        );
    }

    #[test]
    fn delete_puts_a_child_in_the_trash_whole_and_restore_brings_it_back() {
        let (store, children) = room();
        let design = children
            .create(
                serde_json::json!({ "name": "design", "note": "kept" }),
                None,
                None,
            )
            .unwrap();
        let ui = children
            .create(serde_json::json!({ "name": "ui" }), Some(&design), None)
            .unwrap();
        children.delete(&design).unwrap();
        assert_eq!(
            order(&children),
            [("ui".to_owned(), Some("design".to_owned()))]
        );
        let record = RecordId::room_default().child(design.as_str()).unwrap();
        assert_eq!(
            store.read(&record, |doc| Reader::<Topic>::read(doc)
                .unwrap()
                .state()
                .note
                .clone()),
            Some("kept".to_owned())
        );
        assert_eq!(children.child(&design).unwrap().note, "kept");
        children.delete(&ui).unwrap();
        children.restore(&ui).unwrap();
        assert_eq!(order(&children), [("ui".to_owned(), None)]);
        children.restore(&design).unwrap();
        assert_eq!(
            order(&children),
            [("design".to_owned(), None), ("ui".to_owned(), None)]
        );
        // A child is made only with fields its kind has.
        assert!(
            children
                .create(serde_json::json!({ "colour": 3 }), None, None)
                .is_err()
        );
        assert!(
            children
                .create(serde_json::json!({ "name": 3 }), None, None)
                .is_err()
        );
    }

    #[test]
    fn the_door_offers_one_lifecycle_on_every_child_and_carries_each_childs_own_actions() {
        let (_, children) = room();
        let agent = ActionInvoker::actor("agent-1");
        let invoke = |resource: &tilo_actions::Resource, action: &str, args: serde_json::Value| {
            let spec = resource
                .actions
                .iter()
                .find(|spec| spec.id == action)
                .unwrap_or_else(|| panic!("`{action}` on {}", resource.id.as_str()));
            let encoded = spec.build_json(args.as_object().unwrap()).unwrap();
            children.invoke_published(&encoded, &agent)
        };
        let made = invoke(
            &children.published(),
            lifecycle::CREATE,
            serde_json::json!({ "fields": { "name": "design" } }),
        )
        .unwrap();
        let address = made.resources[0].clone();
        assert!(address.starts_with("test.topic/"));
        let published = children.published();
        let child = published
            .children
            .iter()
            .find(|child| child.id.as_str() == address)
            .unwrap();
        for verb in [lifecycle::DELETE, lifecycle::MOVE, "jot"] {
            assert!(
                child.actions.iter().any(|spec| spec.id == verb),
                "{verb} on the child"
            );
        }
        invoke(child, "jot", serde_json::json!({ "text": "agenda" })).unwrap();
        let id = Id::from_raw(address.rsplit('/').next().unwrap());
        assert_eq!(children.child(&id).unwrap().note, "agenda");
        // In the trash, it offers `restore` only.
        invoke(child, lifecycle::DELETE, serde_json::json!({})).unwrap();
        let trashed = children.published();
        let child = trashed
            .children
            .iter()
            .find(|child| child.id.as_str() == address)
            .unwrap();
        assert_eq!(
            child
                .actions
                .iter()
                .map(|spec| spec.id.as_str())
                .collect::<Vec<_>>(),
            [lifecycle::RESTORE]
        );
        invoke(child, lifecycle::RESTORE, serde_json::json!({})).unwrap();
        assert_eq!(children.children().len(), 1);
    }

    /// The saved record `id` of `store`, as another device receives it.
    fn copy(from: &RecordStore, to: &RecordStore, id: &RecordId, kind: &str) {
        let saved = from.read(id, |doc| doc.clone().save()).unwrap();
        to.create_record(&RecordCheckout::new(id.clone(), RecordKind::new(kind)))
            .unwrap();
        to.with(id, |doc| {
            *doc = ui_core::automerge::AutoCommit::load(&saved).unwrap();
        })
        .unwrap();
    }

    /// A device's store of room `lounge`'s records, kept as it keeps them:
    /// a record it has not got reads as missing, not as an error.
    fn device() -> RecordStore {
        crate::record::RecordOwners::with_store(Rc::new(crate::feedback::MemoryStore::new()))
            .open("so.tilo.test", "lounge")
            .unwrap()
    }

    /// The children of the room `store` holds, as `author` sees them.
    fn children_of(store: &RecordStore) -> Children<Topic> {
        let read = store.clone();
        Children::<Topic>::detached_in(store.clone(), "so.tilo.test", "ana", move || {
            read.read(&RecordId::room_default(), |doc| {
                Reader::<Room>::read(doc).unwrap().state().clone()
            })
            .unwrap()
        })
        .unwrap()
    }

    #[test]
    fn a_child_whose_record_is_on_its_way_reads_pending_until_it_arrives() {
        let here = device();
        let parent = RecordCheckout::new(RecordId::room_default(), RecordKind::new(Room::KIND));
        here.create_record(&parent).unwrap();
        here.with(&parent.id, |doc| {
            Room::initialize(doc).unwrap();
            doc.commit();
        })
        .unwrap();
        let design = children_of(&here)
            .create(serde_json::json!({ "name": "design" }), None, None)
            .unwrap();
        // Another device has the room's listing, not yet the child's record.
        let there = device();
        copy(&here, &there, &RecordId::room_default(), Room::KIND);
        let theirs = children_of(&there);
        let listed = theirs.children();
        let [(id, _, arriving)] = listed.as_slice() else {
            panic!("the one listed child: {}", listed.len());
        };
        assert_eq!(*id, design);
        assert!(
            arriving.get().is_pending(),
            "listed and not here is on its way, not a new child: {:?}",
            arriving.with(|arrival| arrival.clone().map(|_| ()))
        );
        assert!(theirs.child(&design).is_none());

        copy(
            &here,
            &there,
            &RecordId::room_default().child(design.as_str()).unwrap(),
            Topic::KIND,
        );
        let _ = theirs.revision();
        assert_eq!(
            arriving.with(|arrival| arrival.ready().map(|topic| topic.name.clone())),
            Some("design".to_owned()),
            "the same read answers once the record arrived"
        );
    }
}
