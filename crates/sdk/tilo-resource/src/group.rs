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
use crate::vocabulary::{Built, Vocabulary, vocabulary};

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

type DeferredCheck = Box<dyn Fn(&Doc) -> Result<(), crate::Refusal>>;

thread_local! {
    /// The record checks deferred to the end of the group applying on this
    /// thread, by what they check; `None` outside a group.
    static DEFERRED: std::cell::RefCell<Option<Vec<(String, DeferredCheck)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run a record's `#[model(check)]` now, or, inside a group, once at its
/// end: a group valid as a whole may pass through a state that is not (two
/// dangling references cleared one after the other). `key` names what is
/// checked, so it runs once however many members touched it.
///
/// # Errors
/// The check's refusal, when it runs now.
#[doc(hidden)]
pub fn check_or_defer(
    key: String,
    doc: &Doc,
    check: impl Fn(&Doc) -> Result<(), crate::Refusal> + 'static,
) -> Result<(), crate::Refusal> {
    let grouping = DEFERRED.with(|deferred| deferred.borrow().is_some());
    if !grouping {
        return check(doc);
    }
    DEFERRED.with(|deferred| {
        if let Some(checks) = deferred.borrow_mut().as_mut()
            && !checks.iter().any(|(existing, _)| *existing == key)
        {
            checks.push((key, Box::new(check)));
        }
    });
    Ok(())
}

/// Whether a group is applying on this thread: record checks wait for its
/// end.
pub(crate) fn grouping() -> bool {
    DEFERRED.with(|deferred| deferred.borrow().is_some())
}

/// Runs the checks deferred in a group when the outermost group ends.
struct Deferring(bool);

impl Deferring {
    fn begin() -> Self {
        Self(DEFERRED.with(|deferred| {
            let mut deferred = deferred.borrow_mut();
            if deferred.is_some() {
                return false;
            }
            *deferred = Some(Vec::new());
            true
        }))
    }

    /// The deferred checks, run once on the group's result.
    fn finish(&self, doc: &Doc) -> Result<(), ActionError> {
        if !self.0 {
            return Ok(());
        }
        let checks = DEFERRED.with(|deferred| deferred.borrow_mut().take().unwrap_or_default());
        for (_, check) in checks {
            check(doc).map_err(|refusal| ActionError::Refused(refusal.0))?;
        }
        Ok(())
    }
}

impl Drop for Deferring {
    fn drop(&mut self) {
        if self.0 {
            DEFERRED.with(|deferred| deferred.borrow_mut().take());
        }
    }
}

/// Restores the enclosing member (for a group applied inside a group) even
/// when a member unwinds.
struct MemberScope(Option<([u8; 32], u64)>);

impl Drop for MemberScope {
    fn drop(&mut self) {
        MEMBER.with(|member| member.set(self.0));
    }
}

impl<A> ActionGroup<A>
where
    A: tilo_actions::Action,
{
    /// The members, of whichever type.
    fn members(&self) -> Vec<&dyn tilo_actions::Action> {
        let mut members: Vec<&dyn tilo_actions::Action> = Vec::with_capacity(self.actions.len());
        for action in &self.actions {
            members.push(action);
        }
        members
    }
}

impl<A> tilo_actions::Action for ActionGroup<A>
where
    A: tilo_actions::Action + Serialize,
{
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let payload = serde_json::to_vec(&self.actions).map_err(ActionError::Encode)?;
        apply_members(doc, &payload, &self.members())
    }

    /// A group is as consequential as its most consequential member: one
    /// deletion in a gesture makes the gesture ask for confirmation.
    fn risk(&self) -> RiskLevel {
        group_risk(self.members().into_iter().map(tilo_actions::Action::risk))
    }

    /// A gesture records each of its members, and the item they share.
    fn describe(&self, doc: &Doc) -> Option<tilo_actions::Described> {
        describe_members(doc, &self.members(), &|index| {
            crate::__private::json_via_bytes(self.actions.get(index)?).ok()
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
        group_change_kind(&self.members())
    }
}

/// Apply `members`, a group encoded as `payload`, in order as one.
fn apply_members(
    doc: &mut Doc,
    payload: &[u8],
    members: &[&dyn tilo_actions::Action],
) -> Result<(), ActionError> {
    let seed = crate::action::seed(doc, b"tilo-resource/group-seed/v1", payload);
    if current_member().is_none() {
        drop(crate::action::take_minted());
    }
    let deferring = Deferring::begin();
    {
        let _restore = MemberScope(current_member());
        let mut position = 0_u64;
        for action in members {
            MEMBER.with(|member| member.set(Some((seed, position))));
            action.apply(doc)?;
            position = position.saturating_add(1);
        }
    }
    deferring.finish(doc)
}

/// What a group of `members` did: each member's description, or its JSON
/// form (`value(index)`) where it has none, and the item they share.
fn describe_members(
    doc: &Doc,
    members: &[&dyn tilo_actions::Action],
    value: &dyn Fn(usize) -> Option<serde_json::Value>,
) -> Option<tilo_actions::Described> {
    let mut described: Vec<tilo_actions::Described> = Vec::with_capacity(members.len());
    for (index, action) in members.iter().enumerate() {
        let member = match action.describe(doc) {
            Some(member) => member,
            None => tilo_actions::Described::from_value("", &value(index)?, None)?,
        };
        described.push(member);
    }
    let item = described.first().and_then(|first| first.item.clone());
    let shared = described.iter().all(|member| member.item == item);
    Some(tilo_actions::Described {
        what: tilo_actions::Did::group(described.into_iter().map(|member| member.what)),
        item: if shared { item } else { None },
    })
}

/// The change kind of a group of `members`.
fn group_change_kind(members: &[&dyn tilo_actions::Action]) -> tilo_actions::ChangeKind {
    use tilo_actions::ChangeKind;
    let mut kinds = members.iter().map(|member| member.change_kind());
    let Some(first) = kinds.next() else {
        return ChangeKind::Content;
    };
    kinds.fold(first, |group, member| match (group, member) {
        (ChangeKind::Content, _) | (_, ChangeKind::Content) => ChangeKind::Content,
        (ChangeKind::External, _) | (_, ChangeKind::External) => ChangeKind::External,
        (ChangeKind::Housekeeping, ChangeKind::Housekeeping) => ChangeKind::Housekeeping,
    })
}

/// How consequential a group of actions as consequential as `risks` is: as
/// its most consequential member. One deletion in a gesture makes the
/// gesture ask for confirmation.
pub(crate) fn group_risk(risks: impl IntoIterator<Item = RiskLevel>) -> RiskLevel {
    let mut risk = RiskLevel::None;
    for member in risks {
        match member {
            consequential if consequential.requires_confirmation() => return consequential,
            RiskLevel::Low => risk = RiskLevel::Low,
            _ => {}
        }
    }
    risk
}

/// The registry type id groups of `R`'s actions are replayed under.
pub fn group_type<R: Resource>() -> String {
    group_type_of(R::ACTION_TYPE)
}

/// The registry type id groups of the actions of the type `action_type`
/// are replayed under.
pub(crate) fn group_type_of(action_type: &str) -> String {
    format!("{action_type}+group")
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
    let members = build_members(vocabulary::<R>(), arguments, now_ms)?;
    let mut actions = Vec::with_capacity(members.len());
    for member in members {
        match member.into_action::<R>() {
            Some(action) => actions.push(action),
            None => return Err(format!("not an action of a {}", R::LABEL)),
        }
    }
    Ok(ActionGroup { actions })
}

/// [`build_group`]'s members, of the resource `vocabulary`.
pub(crate) fn build_members(
    vocabulary: &Vocabulary,
    arguments: &serde_json::Map<String, serde_json::Value>,
    now_ms: i64,
) -> Result<Vec<Built>, String> {
    let Some(serde_json::Value::Array(members)) = arguments.get(GROUP_PARAM) else {
        return Err(format!("`{GROUP_PARAM}` must be an array of invocations"));
    };
    let mut built = Vec::with_capacity(members.len());
    for (index, member) in members.iter().enumerate() {
        let Some(id) = member.get("action").and_then(serde_json::Value::as_str) else {
            return Err(format!("member {index} names no `action`"));
        };
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
        match crate::resource::build_action_of(vocabulary, id, &args, now_ms) {
            Ok(action) => built.push(action),
            Err(reason) => return Err(format!("member {index} ({id}): {reason}")),
        }
    }
    Ok(built)
}
