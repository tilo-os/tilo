//! `take`: one of the parent's items moved whole into a child of its own,
//! and, for a kind the room keeps to its readers, kept to them first.
//!
//! The child keeps the item's id, so its address and every reference to it
//! stay. What the child starts with and the item leaving the parent are
//! written in one step ([`ChildRecord::split`]): nothing is half taken.
//!
//! A child the room keeps to its readers ([`ChildRecord::KEPT_TO_READERS`])
//! is made only once the room keeps its record so (one Deny for everyone but
//! them, `roles::readers_grant`), so nobody else is ever given a byte of it.
//! A `take` naming the readers asks the room for that first, with the room's
//! own `grant` on the session's action door, decided there for the person
//! asking; the item is taken as soon as the room keeps the record so. A
//! refusal is told ([`Children::refused`](super::Children::refused)) and
//! changes nothing.

use std::cell::RefCell;

use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};
use tilo_actions::{
    ActionError, ActionInvoker, ActionParam, ActionSpec, ArgValue, ExecutionLocus,
    InvocationUpdate, ParamType,
};
use tilo_resource::children::{self as lifecycle, ChildRef};
use tilo_resource::{Atom, Edit, EditOp, Id, Resource};
use ui_core::permissions::{TreePath, policy};
use ui_core::widget::Edits;
use ui_core::{InvocationCall, LocalState, RemoteActions, RemoteCall};

use super::{ChildRecord, ChildrenState, Lifecycle, encode_step, invalid, text_arg};

/// One `take` waiting for the room to keep its child to its readers.
struct Waiting {
    item: Id,
    invoker: ActionInvoker,
    call: LocalState<InvocationCall>,
}

/// The `take`s of one container waiting on the room, and the client they
/// asked it through.
pub(super) struct Keeping {
    door: RefCell<Option<RemoteActions>>,
    waiting: RefCell<Vec<Waiting>>,
    refused: LocalState<Option<String>>,
}

impl Default for Keeping {
    fn default() -> Self {
        Self {
            door: RefCell::default(),
            waiting: RefCell::default(),
            refused: LocalState::new(None),
        }
    }
}

impl Keeping {
    /// The client, once anything was asked: the window's root carries its
    /// requests and answers.
    pub(super) fn door(&self) -> Option<RemoteActions> {
        self.door.borrow().clone()
    }

    pub(super) fn waits(&self, item: &Id) -> bool {
        self.waiting
            .borrow()
            .iter()
            .any(|waiting| &waiting.item == item)
    }

    pub(super) fn refused(&self) -> LocalState<Option<String>> {
        self.refused.clone()
    }

    fn refuse(&self, reason: String) {
        tracing::info!(%reason, "a take waiting on the room was refused");
        self.refused.set(Some(reason));
    }
}

impl<C: ChildRecord> ChildrenState<C> {
    /// Take `item` into a child of its own for `invoker`, kept to `readers`
    /// first where the room keeps these to their readers and does not keep
    /// this one so yet. The child's id: its item's.
    pub(super) fn take(
        &self,
        parent: &<C::Parent as Resource>::State,
        item: &Id,
        readers: Option<&[policy::Subject]>,
        invoker: &ActionInvoker,
        live: Option<&mut Edits<'_>>,
    ) -> Result<Id, ActionError> {
        if C::container(parent).contains(item) {
            return Err(ActionError::Refused(format!(
                "this is one of these {} already",
                C::LABEL.to_lowercase()
            )));
        }
        // Refused before the room is asked anything.
        C::split(parent, item).map_err(ActionError::Refused)?;
        match readers {
            Some(readers) if C::KEPT_TO_READERS && !self.kept(item)? => {
                self.keep(item, readers, invoker)?;
                Ok(item.clone())
            }
            _ => self.take_now(parent, item, invoker, live),
        }
    }

    /// The child's content and the item leaving the parent, written now.
    fn take_now(
        &self,
        parent: &<C::Parent as Resource>::State,
        item: &Id,
        invoker: &ActionInvoker,
        live: Option<&mut Edits<'_>>,
    ) -> Result<Id, ActionError> {
        self.check_kept(item)?;
        let (content, mut edits) = C::split(parent, item).map_err(ActionError::Refused)?;
        self.start(item).map_err(ActionError::Refused)?;
        self.edit_child(item, content, invoker)?;
        let reference = ChildRef {
            kind: Atom(C::KIND.to_owned()),
            group: None,
            trashed: None,
        };
        edits.push(Edit::new(
            C::container_path(),
            EditOp::InsertItem {
                id: Some(item.clone()),
                at: lifecycle::place(C::container(parent), None),
                value: tilo_resource::Model::to_json(&reference),
            },
        ));
        self.edit_parent(lifecycle::TAKE, edits, invoker, live)?;
        Ok(item.clone())
    }

    /// Whether the room keeps child `id`'s record to its readers. Children
    /// held outside any host, which may do anything, are kept.
    pub(super) fn kept(&self, id: &Id) -> Result<bool, ActionError> {
        let Some(permissions) = &self.room.permissions else {
            return Ok(true);
        };
        let record = Self::record(id).map_err(ActionError::Refused)?;
        let current = permissions.current();
        let standing = current.standing();
        let node = standing.node(
            &TreePath::root()
                .app(self.room.app_id)
                .resource(record.route()),
        );
        Ok(standing.policy.grants.iter().any(|grant| {
            grant.node == node
                && grant.answer == policy::Answer::Deny
                && grant.kinds.contains(policy::ActionKind::Read)
                && matches!(grant.subject, policy::Subject::AllBut(_))
        }))
    }

    /// Ask the room to keep `item`'s record to `readers` and the window's
    /// person, and take it once it does.
    fn keep(
        &self,
        item: &Id,
        readers: &[policy::Subject],
        invoker: &ActionInvoker,
    ) -> Result<(), ActionError> {
        if self.keeping.waits(item) {
            return Ok(());
        }
        let record = Self::record(item).map_err(ActionError::Refused)?;
        let room = super::super::reference::room().map_err(ActionError::Refused)?;
        let me = policy::Subject::person(&self.room.me);
        let mut subjects = vec![me.clone()];
        for reader in readers {
            if !subjects.contains(reader) {
                subjects.push(reader.clone());
            }
        }
        let grant =
            policy::roles::readers_grant(&room, self.room.app_id, record.route(), subjects, me);
        let node = TreePath::root()
            .app(self.room.app_id)
            .resource(record.route());
        let json = |value: serde_json::Result<serde_json::Value>| {
            value.map_err(|error| ActionError::Refused(error.to_string()))
        };
        let arguments = serde_json::Map::from_iter([
            (
                "subject".to_owned(),
                json(serde_json::to_value(&grant.subject))?,
            ),
            ("node".to_owned(), node.to_string().into()),
            ("kinds".to_owned(), json(serde_json::to_value(grant.kinds))?),
            (
                "answer".to_owned(),
                json(serde_json::to_value(grant.answer))?,
            ),
        ]);
        let call = self
            .keeping
            .door
            .borrow_mut()
            .get_or_insert_with(RemoteActions::current)
            .invoke(
                RemoteActionTarget::room_governance(&room, policy::roles::governance::GRANT),
                InvokeArgument::Json(arguments),
                None,
            );
        self.keeping.refused.set(None);
        self.keeping.waiting.borrow_mut().push(Waiting {
            item: item.clone(),
            invoker: invoker.clone(),
            call,
        });
        Ok(())
    }

    /// Take every waiting item whose record the room keeps now, and drop
    /// those the room refused to keep, telling why: in the window's pass
    /// over its own record (`live`), which the take writes.
    pub(super) fn settle_kept(&self, mut live: Option<&mut Edits<'_>>) {
        let waiting = std::mem::take(&mut *self.keeping.waiting.borrow_mut());
        if waiting.is_empty() {
            return;
        }
        if let Some(door) = self.keeping.door() {
            door.poll();
        }
        let mut still = Vec::new();
        for waiting in waiting {
            match self.kept(&waiting.item) {
                Ok(true) => {
                    let parent = (self.parent)();
                    let live = live.as_deref_mut();
                    if let Err(error) =
                        self.take_now(&parent, &waiting.item, &waiting.invoker, live)
                    {
                        self.keeping.refuse(error.to_string());
                    }
                    continue;
                }
                Err(error) => {
                    self.keeping.refuse(error.to_string());
                    continue;
                }
                Ok(false) => {}
            }
            let refused = waiting.call.with(|call| match call {
                RemoteCall::Done(InvocationUpdate::Failed { error, .. }) => {
                    Some(error.message.clone())
                }
                RemoteCall::Done(InvocationUpdate::Cancelled { message, .. }) => Some(
                    message
                        .clone()
                        .unwrap_or_else(|| "the room was not asked".to_owned()),
                ),
                RemoteCall::Refused(error) => Some(error.to_string()),
                _ => None,
            });
            match refused {
                Some(reason) => self.keeping.refuse(reason),
                // Granted or not answered yet: taken once the room's
                // policy here says it keeps the record.
                None => still.push(waiting),
            }
        }
        self.keeping.waiting.borrow_mut().extend(still);
    }
}

/// The container's `take`: an item of the parent into a child of its own,
/// kept to the readers named first.
pub(super) fn spec(label: &str) -> ActionSpec {
    ActionSpec::typed(
        lifecycle::TAKE,
        label,
        "item",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("item", ParamType::Text, "The item it is made from"),
            ActionParam::optional(
                "readers",
                ParamType::Array,
                "Who may read it, each {\"person\": id} or {\"agent\": id}; the room keeps it to them, and you, before it is made",
            ),
        ],
        |args| {
            let Some(item) = text_arg(args, "item") else {
                return invalid("name the item to take");
            };
            let readers = match args.get("readers") {
                Some(ArgValue::Array(readers)) => {
                    match serde_json::from_value(serde_json::Value::Array(readers.clone())) {
                        Ok(readers) => Some(readers),
                        Err(error) => return invalid(&error.to_string()),
                    }
                }
                _ => None,
            };
            encode_step(&Lifecycle::Take { item, readers })
        },
    )
}
