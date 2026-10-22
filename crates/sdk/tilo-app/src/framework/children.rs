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
//! - **Made from an item.** A child type that says how one of the parent's
//!   items becomes a child ([`ChildRecord::split`]) gets `take` too: the item
//!   leaves the parent and starts a child of its own under its id, in one
//!   step, so every reference to it stays.
//! - **Kept to its readers.** A child type the room keeps to some readers
//!   ([`ChildRecord::KEPT_TO_READERS`]) is made only once the room keeps its
//!   record so; `take` naming the readers asks the room for that first, then
//!   makes it.
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
use ui_core::permissions::{PermissionScope, TreePath, policy};
use ui_core::record::{RecordCheckout, RecordId, RecordKind, RecordOpenError};
use ui_core::widget::Edits;

use super::chain::{CHAIN_ACTION_TYPE, RoomRecords};

mod held;
mod readers;
mod take;
use super::live::Live;
use super::record::RecordDoor;
pub use held::ChildOf;

/// The operation type a child's own actions are carried out under on the
/// container's door: the action and the child it changes.
pub(crate) const CHILD_ACTION_TYPE: &str = "tilo.app.child-action";
/// The operation type the container's lifecycle is carried out under.
pub(crate) const LIFECYCLE_TYPE: &str = "tilo.app.child-lifecycle";

/// A resource whose records are children a record lists. See the
/// [module docs](self).
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

    /// Whether each of these is kept by the room to its readers (one Deny
    /// for everyone but them on its record, `roles::readers_grant`): one is
    /// made only once the room keeps its record so, so its first change
    /// reaches no one else.
    const KEPT_TO_READERS: bool = false;

    /// What `take` reads as where these are made from the parent's items
    /// ([`Self::split`]); `None`, and no `take`, where they are not.
    const TAKE_LABEL: Option<&'static str> = None;

    /// The parent's item `item` as a new child of its own: the edits that
    /// give the child its content, and those that take the item out of the
    /// parent. `take` applies both in one step, under the item's id.
    ///
    /// # Errors
    /// Why the item cannot be taken: by default, these are not made from
    /// items.
    #[expect(clippy::type_complexity, reason = "the child's edits, the parent's")]
    fn split(
        parent: &<Self::Parent as Resource>::State,
        item: &Id,
    ) -> Result<(Vec<Edit<Self>>, Vec<Edit<Self::Parent>>), String> {
        let _ = (parent, item);
        Err(format!("{} are not made from items", Self::LABEL))
    }

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
    /// The parent's item `item` taken into a child of its own, kept to
    /// `readers` when they are named.
    Take {
        item: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        readers: Option<Vec<policy::Subject>>,
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
            Self::Take { item, readers } => {
                let mut arguments = serde_json::Map::from_iter([("item".to_owned(), text(item))]);
                if let Some(readers) = readers {
                    arguments.insert(
                        "readers".into(),
                        serde_json::to_value(readers).unwrap_or_default(),
                    );
                }
                (door.to_owned(), lifecycle::TAKE, item.clone(), arguments)
            }
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
    /// A child before anything was written to it.
    empty: Rc<C::State>,
    revision: Cell<u64>,
    published: RefCell<Option<((u64, u64), tilo_actions::Resource)>>,
    /// Steps waiting for the room to keep their child to its readers.
    keeping: take::Keeping,
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
            empty,
            revision: Cell::new(0),
            published: RefCell::new(None),
            keeping: take::Keeping::default(),
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

    /// Every child outside the trash with its state, in the container's
    /// order; one whose record has not arrived yet reads as a new one.
    pub(crate) fn children(&self) -> Vec<(Id, ChildRef, Rc<C::State>)> {
        self.refresh();
        let held = self.held.borrow();
        self.listed()
            .into_iter()
            .filter(|(_, child)| child.trashed.is_none())
            .map(|(id, child)| {
                let state = held
                    .get(&id)
                    .map_or_else(|| Rc::clone(&self.empty), Live::get);
                (id, child, state)
            })
            .collect()
    }

    /// Every child outside the trash whose record this device reads, with
    /// its state, in the container's order: one kept from this person never
    /// arrives, so it is not among them.
    pub(crate) fn readable(&self) -> Vec<(Id, Rc<C::State>)> {
        self.refresh();
        let held = self.held.borrow();
        self.listed()
            .into_iter()
            .filter(|(_, child)| child.trashed.is_none())
            .filter_map(|(id, _)| held.get(&id).map(|live| (id, live.get())))
            .collect()
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
            moved |= self.hold(&id).is_ok();
        }
        if moved {
            self.moved();
        }
        // A parent the window shows is written in its pass ([`RecordDoor::
        // settle`]); one in the store, now.
        if self.parent_door.borrow().is_none() {
            self.settle_kept(None);
        }
        moved
    }

    /// Hold child `id`: read its record from this device, which asks the
    /// room for it when it has not got it yet.
    fn hold(&self, id: &Id) -> Result<(), String> {
        if self.held.borrow().contains_key(id) {
            return Ok(());
        }
        let checkout = RecordCheckout::new(Self::record(id)?, self.kind.clone());
        if !self.room.store.is_ready(&checkout.id) {
            if !self.room.store.takes_writes() {
                return Err("opened once the store takes writes".into());
            }
            self.room
                .store
                .try_open_record(&checkout)
                .map_err(|error| error.to_string())?;
        }
        self.follow(id)
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
            Lifecycle::Take { .. } => lifecycle::TAKE,
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
            Lifecycle::Take { item, readers } => {
                self.take(&parent, item, readers.as_deref(), invoker, live)
            }
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
        self.check_kept(id)?;
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
                    at,
                    value: tilo_resource::Model::to_json(&reference),
                },
            )],
            invoker,
            live,
        )?;
        Ok(id.clone())
    }

    /// Refuses a child of a kind the room keeps to its readers whose record
    /// it does not keep so yet.
    fn check_kept(&self, id: &Id) -> Result<(), ActionError> {
        if !C::KEPT_TO_READERS || self.kept(id)? {
            return Ok(());
        }
        Err(ActionError::Refused(format!(
            "choose who may read this {} before it is made",
            C::LABEL.to_lowercase()
        )))
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
                    at,
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
        self.hold(child).map_err(ActionError::Refused)?;
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
        if let Some(label) = C::TAKE_LABEL {
            resource = resource.with_action(self.route(take::spec(label)));
        }
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
                    .with_action(
                        self.route(step_spec(lifecycle::DELETE, "Delete", &id).destructive()),
                    )
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

    fn settle(&self, live: &mut Edits<'_>) {
        self.settle_kept(Some(live));
    }

    fn remote(&self) -> Option<ui_core::RemoteActions> {
        self.keeping.door()
    }

    /// Read as last refreshed: the window refreshes its records every sync,
    /// and search asks on every frame.
    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        let held = self.held.borrow();
        self.listed()
            .into_iter()
            .filter(|(_, child)| child.trashed.is_none())
            .filter_map(|(id, _)| held.get(&id))
            .flat_map(|live| C::search(&live.get()))
            .collect()
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
/// [module docs](self).
pub struct Children<C: ChildRecord> {
    state: Rc<ChildrenState<C>>,
    me: String,
    /// The door of the parent the view shows, which the lifecycle a view
    /// asks for goes through, and the door it submits to one child through.
    dispatch: Option<(super::Dispatcher<C::Parent>, ChildDispatch<C>)>,
}

type ChildDispatch<C> = Rc<dyn Fn(&Id) -> super::Dispatcher<C>>;

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
        let to_parent = parent.clone();
        let (parent, state, me) = (parent.clone(), Rc::downgrade(&self.state), self.me.clone());
        let to_child: ChildDispatch<C> = Rc::new(move |id: &Id| {
            let (id, state, me) = (id.clone(), state.clone(), me.clone());
            parent.for_room_record::<C>(
                &child_resource_id::<C>(&id),
                parent.read_only(),
                move |action| {
                    state
                        .upgrade()
                        .ok_or_else(|| ActionError::Refused("the container is closed".into()))?
                        .apply_to(&id, action, &ActionInvoker::actor(&me))
                        .map(drop)
                },
            )
        });
        self.dispatch = Some((to_parent, to_child));
        self
    }

    /// The door to child `id` for the view holding these: what it submits
    /// is checked, applied and refused like the view's own actions. `None`
    /// outside a view.
    #[must_use]
    pub fn dispatcher(&self, id: &Id) -> Option<super::Dispatcher<C>> {
        self.dispatch.as_ref().map(|(_, to_child)| to_child(id))
    }

    /// The address the container is published under.
    #[must_use]
    pub fn id(&self) -> &str {
        self.state.id()
    }

    /// Every child outside the trash, with its place and state, in order.
    #[must_use]
    pub fn children(&self) -> Vec<(Id, ChildRef, Rc<C::State>)> {
        self.state.children()
    }

    /// Every child outside the trash whose record this device reads, in
    /// order: what a view shows beside the parent.
    #[must_use]
    pub fn readable(&self) -> Vec<(Id, Rc<C::State>)> {
        self.state.readable()
    }

    /// Child `id`, listed or in the trash, when its record is here.
    #[must_use]
    pub fn child(&self, id: &Id) -> Option<Rc<C::State>> {
        self.state.child(id)
    }

    /// Child `id` as a record a part of the view edits like the app's own
    /// ([`ViewCx::record`](super::ViewCx::record)): its controls bind its
    /// fields and verbs, carried out on its record through the container's
    /// door.
    #[must_use]
    pub fn record(&self, id: &Id) -> ChildOf<C> {
        ChildOf {
            children: self.clone(),
            id: id.clone(),
        }
    }

    /// Take the parent's item `item` into a child of its own, as the
    /// window's person ([`ChildRecord::split`]); for a kind the room keeps
    /// to its readers, kept to `readers` first: the room is asked to keep
    /// its record to them, and the item is taken once it does.
    ///
    /// # Errors
    /// As the door refuses `take`.
    pub fn take(
        &self,
        item: &Id,
        readers: Option<Vec<policy::Subject>>,
    ) -> Result<(), ActionError> {
        self.step(&Lifecycle::Take {
            item: item.clone(),
            readers,
        })
        .map(drop)
    }

    /// Whether `item` waits for the room to keep it to its readers.
    #[must_use]
    pub fn keeping(&self, item: &Id) -> bool {
        self.state.keeping.waits(item)
    }

    /// Why the last `take` waiting on the room did not happen, until the
    /// next one is asked.
    #[must_use]
    pub fn refused(&self) -> ui_core::LocalState<Option<String>> {
        self.state.keeping.refused()
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
        let Some((parent, _)) = &self.dispatch else {
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

    /// [`Self::detached_in`], edited as the participant `permissions` name
    /// and held to what those let them do as they change.
    #[must_use]
    pub fn detached_as(
        store: ui_core::record::RecordStore,
        app_id: &'static str,
        permissions: ui_core::permissions::LivePermissions,
        parent: impl Fn() -> Rc<<C::Parent as Resource>::State> + 'static,
    ) -> Option<Self> {
        let me = permissions.current().user().0.clone();
        let room = RoomRecords {
            store,
            me: me.clone(),
            permissions: Some(permissions),
            app_id,
            app_title: app_id,
        };
        let state = Rc::new(ChildrenState::<C>::open(room, Rc::new(parent))?);
        Some(Self::new(state, me))
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

    /// A desk of notes, any of which can be taken into a secret of its own.
    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Desk {
        pub notes: tilo_resource::Collection<Note>,
        pub kept: Container,
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Note {
        pub text: String,
    }

    #[resource(kind = "test.desk", label = "Desk")]
    impl Desk {
        /// Write a note.
        #[action]
        pub fn write(&mut self, cx: &mut tilo_resource::ActionCx, text: String) {
            let id = cx.id();
            self.notes.push(id, Note { text });
        }
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Secret {
        pub note: String,
    }

    #[resource(kind = "test.secret", label = "Secrets")]
    impl Secret {
        /// Note.
        #[action]
        pub fn jot(&mut self, text: String) {
            self.note = text;
        }
    }

    impl ChildRecord for Secret {
        type Parent = Desk;
        const KEPT_TO_READERS: bool = true;
        const TAKE_LABEL: Option<&'static str> = Some("Keep to its readers");
        fn container(parent: &Desk) -> &Container {
            &parent.kept
        }
        fn container_path() -> Vec<Segment> {
            vec![Segment::Field("kept".into())]
        }
        fn split(desk: &Desk, item: &Id) -> Result<(Vec<Edit<Self>>, Vec<Edit<Desk>>), String> {
            let note = desk.notes.get(item).ok_or("there is no such note")?;
            Ok((
                vec![Edit::new(
                    vec![Segment::Field("note".into())],
                    EditOp::Set {
                        value: note.text.clone().into(),
                    },
                )],
                vec![Edit::new(
                    vec![Segment::Field("notes".into())],
                    EditOp::RemoveItem { id: item.clone() },
                )],
            ))
        }
    }

    /// The room's door: records each grant asked and answers `answer`.
    struct Door {
        answer: Result<(), tilo_actions::remote::ActionHostError>,
        asked: std::sync::Mutex<Vec<tilo_actions::remote::InvokeRequest>>,
    }

    impl tilo_actions::remote::ActionHost for Door {
        fn query(
            &self,
            _: &tilo_actions::remote::Caller,
            _: tilo_actions::remote::CatalogueQuery,
        ) -> tilo_actions::remote::Pending<
            Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
        > {
            tilo_actions::remote::Pending::ready(Err(
                tilo_actions::remote::ActionHostError::NotPermitted,
            ))
        }

        fn invoke(
            &self,
            _: &tilo_actions::remote::Caller,
            request: tilo_actions::remote::InvokeRequest,
        ) -> tilo_actions::remote::Pending<
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
        > {
            let invocation = request.invocation;
            self.asked.lock().unwrap().push(request);
            tilo_actions::remote::Pending::ready(self.answer.clone().map(|()| {
                tilo_actions::remote::InvocationReceipt {
                    invocation,
                    status: tilo_actions::remote::ReceiptStatus::Completed {
                        output: tilo_actions::ActionOutput::default(),
                    },
                }
            }))
        }

        fn observe(
            &self,
            _: &tilo_actions::remote::Caller,
            _: &tilo_actions::InvocationId,
        ) -> Option<tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &tilo_actions::remote::Caller,
            _: &tilo_actions::InvocationId,
        ) -> Result<(), tilo_actions::remote::ActionHostError> {
            Ok(())
        }
    }

    const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a91";

    /// Ana's desk with one note, its secrets held as Ana under `permissions`.
    fn desk(
        permissions: ui_core::permissions::LivePermissions,
    ) -> (RecordStore, Children<Secret>, Id) {
        let store = RecordStore::in_memory();
        let parent = RecordCheckout::new(RecordId::room_default(), RecordKind::new(Desk::KIND));
        store.create_record(&parent).unwrap();
        let mut ana = tilo_resource::Replica::<Desk>::new().as_author("ana");
        ana.apply(Desk::WRITE.with("salaries".to_owned())).unwrap();
        let note = ana.state().unwrap().notes.ids().next().cloned().unwrap();
        let mut written = ui_core::automerge::AutoCommit::load(&ana.save()).unwrap();
        store
            .with(&parent.id, |doc| {
                tilo_automerge::ApplyIncoming::merge_incoming(doc, &mut written).map(drop)
            })
            .unwrap()
            .unwrap();
        let read = store.clone();
        let secrets = Children::<Secret>::detached_as(
            store.clone(),
            "so.tilo.test",
            permissions,
            move || {
                read.read(&RecordId::room_default(), |doc| {
                    Reader::<Desk>::read(doc).unwrap().state().clone()
                })
                .unwrap()
            },
        )
        .unwrap();
        (store, secrets, note)
    }

    fn read_desk(store: &RecordStore) -> Desk {
        store
            .read(&RecordId::room_default(), |doc| {
                Reader::<Desk>::read(doc).unwrap().state().as_ref().clone()
            })
            .unwrap()
    }

    #[test]
    fn take_moves_an_item_whole_into_a_child_under_its_id_once_the_room_keeps_it() {
        use ui_core::permissions::policy::{Role, Subject, roles};
        use ui_core::permissions::{LivePermissions, PermissionContext};
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ID", ROOM);
        let door = std::sync::Arc::new(Door {
            answer: Ok(()),
            asked: std::sync::Mutex::default(),
        });
        let _served = ui_core::RemoteActions::enter(
            door.clone(),
            tilo_actions::remote::Caller::local_person("ana"),
        );
        let ana = PermissionContext::member_of(ROOM, "ana", Role::Editor);
        let permissions = LivePermissions::fixed(ana.clone());
        let (store, secrets, note) = desk(permissions.clone());
        let record = RecordId::room_default().child(note.as_str()).unwrap();

        // Not kept yet, and no readers named: refused, nothing written.
        let refused = secrets.take(&note, None).unwrap_err();
        assert!(
            refused.to_string().contains("choose who may read"),
            "{refused}"
        );
        assert!(store.read(&record, |_| ()).is_none());

        // Naming them asks the room to keep the record, for Ana too.
        let bea = Subject::person("bea");
        secrets.take(&note, Some(vec![bea.clone()])).unwrap();
        assert!(secrets.keeping(&note));
        let asked = door.asked.lock().unwrap().pop().unwrap();
        assert_eq!(
            asked.target,
            tilo_actions::remote::RemoteActionTarget::room_governance(
                ROOM,
                roles::governance::GRANT
            )
        );
        let tilo_actions::remote::InvokeArgument::Json(arguments) = asked.argument else {
            panic!("the grant's arguments");
        };
        assert_eq!(
            arguments["subject"],
            serde_json::to_value(Subject::all_but([Subject::person("ana"), bea.clone()])).unwrap()
        );
        assert_eq!(arguments["answer"], "deny");
        assert!(
            store.read(&record, |_| ()).is_none(),
            "nothing before it is kept"
        );
        assert_eq!(read_desk(&store).notes.len(), 1);

        // Once the room keeps it, the note is taken in one step.
        permissions.replace(ana.with_grant(roles::readers_grant(
            ROOM,
            "so.tilo.test",
            record.route(),
            [Subject::person("ana"), bea],
            Subject::person("ana"),
        )));
        let _ = secrets.revision();
        assert!(!secrets.keeping(&note));
        assert_eq!(secrets.child(&note).unwrap().note, "salaries");
        let desk = read_desk(&store);
        assert!(desk.notes.is_empty(), "the note left the desk");
        assert!(desk.kept.get(&note).is_some(), "listed under the note's id");
        assert_eq!(secrets.readable().len(), 1);
        assert!(secrets.take(&note, None).is_err(), "taken once");
    }

    #[test]
    fn a_room_that_refuses_to_keep_it_leaves_everything_as_it_was() {
        use ui_core::permissions::policy::{Role, Subject};
        use ui_core::permissions::{LivePermissions, PermissionContext};
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ID", ROOM);
        let door = std::sync::Arc::new(Door {
            answer: Err(tilo_actions::remote::ActionHostError::NotPermitted),
            asked: std::sync::Mutex::default(),
        });
        let _served =
            ui_core::RemoteActions::enter(door, tilo_actions::remote::Caller::local_person("ana"));
        let ana = PermissionContext::member_of(ROOM, "ana", Role::Editor);
        let (store, secrets, note) = desk(LivePermissions::fixed(ana));
        secrets
            .take(&note, Some(vec![Subject::person("bea")]))
            .unwrap();
        let _ = secrets.revision();
        assert!(!secrets.keeping(&note));
        assert!(secrets.refused().get().is_some(), "the refusal is told");
        assert_eq!(read_desk(&store).notes.len(), 1);
        assert!(secrets.readable().is_empty());
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
                (state.name.clone(), group)
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
}
