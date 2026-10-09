//! Several actions applied as one: one change, one version in history, one
//! undo step, one permission decision.
//!
//! One action is one undo step, and that is the right default: a control, a
//! palette entry or an agent call is one thing the user did. A *gesture* can
//! be several — splitting a linked video and audio clip, deleting a
//! selection, a paste — and the user expects one Ctrl+Z to take all of it
//! back. [`ActionGroup`] carries such a gesture as one replayable action; the
//! framework's `Dispatcher::group` and [`Invoke::group`](crate::Invoke::group)
//! build it from ordinary invocations.

use std::cell::Cell;

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, ActionRegistry, ExecutionLocus, RiskLevel};

use crate::model::Doc;
use crate::resource::Resource;

/// The id of the action every resource publishes that applies several of its
/// actions, in order, as one.
pub const GROUP: &str = "group";
/// The parameter of [`GROUP`]: an array of `{ "action": id, "args": {..} }`
/// invocations of the resource's other actions.
pub const GROUP_PARAM: &str = "actions";

/// Several actions of one resource, applied in order as one change.
///
/// Replay is exact: members apply inside one transaction, and the ids a
/// member mints from its [`ActionCx`](crate::ActionCx) are seeded by the
/// group and the member's position, so every replica mints the same ones.
/// A member that refuses refuses the whole group, and nothing is applied.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(transparent)]
pub struct ActionGroup<A> {
    pub actions: Vec<A>,
}

thread_local! {
    /// The seed and position of the group member being applied, if any.
    static MEMBER: Cell<Option<([u8; 32], u64)>> = const { Cell::new(None) };
}

/// Inside a group member's `apply`: the group's seed and the member's
/// position. [`ActionCx::for_apply`](crate::ActionCx::for_apply) uses it in
/// place of the document's heads, which a member cannot read without
/// committing its predecessors' changes separately.
pub(crate) fn current_member() -> Option<([u8; 32], u64)> {
    MEMBER.with(Cell::get)
}

/// Restores the enclosing member (for a group applied inside a group) even
/// when a member unwinds.
struct MemberScope(Option<([u8; 32], u64)>);

impl Drop for MemberScope {
    fn drop(&mut self) {
        MEMBER.with(|member| member.set(self.0));
    }
}

impl<A> tilo_actions::Action for ActionGroup<A>
where
    A: tilo_actions::Action + Serialize,
{
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let payload = serde_json::to_vec(&self.actions).map_err(ActionError::Encode)?;
        let seed = crate::action::seed(doc, b"tilo-resource/group-seed/v1", &payload);
        let _restore = MemberScope(current_member());
        for (position, action) in (0_u64..).zip(&self.actions) {
            MEMBER.with(|member| member.set(Some((seed, position))));
            action.apply(doc)?;
        }
        Ok(())
    }

    /// A group is as consequential as its most consequential member: one
    /// deletion in a gesture makes the gesture ask for confirmation.
    fn risk(&self) -> RiskLevel {
        let mut risk = RiskLevel::None;
        for action in &self.actions {
            match action.risk() {
                consequential if consequential.requires_confirmation() => return consequential,
                RiskLevel::Low => risk = RiskLevel::Low,
                _ => {}
            }
        }
        risk
    }

    /// A gesture records each of its members, and the item they share.
    fn describe(&self, doc: &Doc) -> Option<tilo_actions::Described> {
        let members: Vec<tilo_actions::Described> = self
            .actions
            .iter()
            .map(|action| {
                action.describe(doc).or_else(|| {
                    let value = serde_json::to_value(action).ok()?;
                    tilo_actions::Described::from_value("", &value, None)
                })
            })
            .collect::<Option<_>>()?;
        let item = members.first().and_then(|first| first.item.clone());
        let shared = members.iter().all(|member| member.item == item);
        Some(tilo_actions::Described {
            what: tilo_actions::Did::group(members.into_iter().map(|member| member.what)),
            item: if shared { item } else { None },
        })
    }

    fn locus(&self) -> ExecutionLocus {
        self.actions
            .first()
            .map_or(ExecutionLocus::ClientLocal, tilo_actions::Action::locus)
    }

    /// A gesture is someone's step when any member is: one content change
    /// makes it content. Otherwise it is external when any member is, and
    /// housekeeping when every member is.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        use tilo_actions::ChangeKind;
        let mut kinds = self.actions.iter().map(tilo_actions::Action::change_kind);
        let Some(first) = kinds.next() else {
            return ChangeKind::Content;
        };
        kinds.fold(first, |group, member| match (group, member) {
            (ChangeKind::Content, _) | (_, ChangeKind::Content) => ChangeKind::Content,
            (ChangeKind::External, _) | (_, ChangeKind::External) => ChangeKind::External,
            (ChangeKind::Housekeeping, ChangeKind::Housekeeping) => ChangeKind::Housekeeping,
        })
    }
}

/// The registry type id groups of `R`'s actions are replayed under.
pub fn group_type<R: Resource>() -> String {
    format!("{}+group", R::ACTION_TYPE)
}

/// Register everything a replica of `R` replays: the resource's own actions
/// ([`Resource::register`]) and groups of them.
pub fn register<R: Resource>(registry: &mut ActionRegistry) {
    R::register(registry);
    registry.register::<ActionGroup<R::Action>>(group_type::<R>());
}

/// Build the group [`GROUP`] invocations describe: each member is built from
/// its own declaration exactly as if it were invoked alone.
///
/// # Errors
/// Returns why a member is unknown, does not bind or is itself a group.
pub fn build_group<R: Resource>(
    arguments: &serde_json::Map<String, serde_json::Value>,
    now_ms: i64,
) -> Result<ActionGroup<R::Action>, String> {
    let Some(serde_json::Value::Array(members)) = arguments.get(GROUP_PARAM) else {
        return Err(format!("`{GROUP_PARAM}` must be an array of invocations"));
    };
    members
        .iter()
        .enumerate()
        .map(|(index, member)| {
            let id = member
                .get("action")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("member {index} names no `action`"))?;
            if id == GROUP {
                return Err(format!("member {index}: a group member cannot be a group"));
            }
            let args = match member.get("args") {
                None | Some(serde_json::Value::Null) => serde_json::Map::new(),
                Some(serde_json::Value::Object(args)) => args.clone(),
                Some(_) => {
                    return Err(format!(
                        "member {index}: the arguments of `{id}` must be an object"
                    ));
                }
            };
            crate::resource::build_action::<R>(id, &args, now_ms)
                .map_err(|reason| format!("member {index} ({id}): {reason}"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|actions| ActionGroup { actions })
}
