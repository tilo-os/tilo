//! Who a window is for, and what the device's daemon says of the actions it shows.
//!
//! A window decides no permission. The daemon's dispatch decides every action the window
//! publishes, in the workspace holding its record, and sends each verdict as it changes
//! ([`runtime_collab::shown_actions`]). A control shows its action's verdict: enabled, asking
//! first, or refused with the reason the person reads. A press runs as its verdict admits it, and
//! waits for the verdict when it has not come yet. The changes it makes are admitted again when
//! they reach the workspace.
//!
//! A harness, a shot or a test has no daemon: its host stands in for one with
//! [`PermissionContext::hosted`], deciding with the workspace's own policy function as the
//! dispatch does ([`HostGrants`]).

use std::sync::Arc;

pub use collab_types::presence::AppKind;
pub use collab_types::role::Role;
pub use runtime_collab::shown_actions::{ActionDecision, ActionVerdict, ShownAction};
pub use tilo_action_policy as policy;
pub use tilo_action_policy::{
    Acting, ActionKind, Answer, Grantee, KindSet, MemberAccess, MemberId, TreePath,
};

mod host;
pub mod shown;
pub use host::HostGrants;
pub use shown::ShownVerdicts;

/// The person a window acts for when no host names one.
pub const LOCAL_USER: &str = "local";

/// The workspace a harness host decides in when no host names one.
pub const LOCAL_WORKSPACE: &str = "local";

/// Who a window is for, and the verdicts its host sent of the actions it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionContext {
    user: MemberId,
    shown: Arc<ShownVerdicts>,
    /// What stands in for the daemon where there is none (a harness, a shot, a test).
    host: Option<Arc<HostGrants>>,
    /// The record moved on, or this is a past version of it: nothing here changes it.
    read_only: bool,
}

/// What a person reads at a control of a record that moved on or is a past version.
pub const READ_ONLY: &str = "This is history now, so it can't be changed here.";

impl PermissionContext {
    /// A window acting for `user`, its verdicts sent by the device's daemon.
    #[must_use]
    pub fn new(user: MemberId) -> Self {
        Self {
            user,
            shown: Arc::default(),
            host: None,
            read_only: false,
        }
    }

    /// A window acting for the local person ([`LOCAL_USER`]).
    #[must_use]
    pub fn local() -> Self {
        Self::new(MemberId::person(LOCAL_USER))
    }

    /// A window whose host has no daemon and decides with `grants` in its place, as the dispatch
    /// decides: a harness, a shot, a test.
    #[must_use]
    pub fn hosted(grants: HostGrants) -> Self {
        Self {
            user: MemberId::person(grants.person()),
            shown: Arc::default(),
            host: Some(Arc::new(grants)),
            read_only: false,
        }
    }

    /// The same window on a record that moved on, or a past version of it: whoever holds it may
    /// still read it, and nobody changes it here.
    #[must_use]
    pub fn read_only(&self) -> Self {
        Self {
            read_only: true,
            ..self.clone()
        }
    }

    /// Whether nothing here changes the record ([`Self::read_only`]).
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// The owner of [`LOCAL_WORKSPACE`], decided by a harness host.
    #[must_use]
    pub fn owner() -> Self {
        Self::hosted(HostGrants::owner())
    }

    /// `person` holding `role` in [`LOCAL_WORKSPACE`], decided by a harness host.
    #[must_use]
    pub fn member(person: &MemberId, role: Role) -> Self {
        Self::hosted(HostGrants::member(person, role))
    }

    /// `person` holding `role` in `workspace`, decided by a harness host.
    #[must_use]
    pub fn member_of(workspace: &str, person: &MemberId, role: Role) -> Self {
        Self::hosted(HostGrants::member_of(workspace, person, role))
    }

    /// The person `person`, who is no member of [`LOCAL_WORKSPACE`], decided by a harness host.
    #[must_use]
    pub fn stranger(person: &MemberId) -> Self {
        Self::hosted(HostGrants::stranger(person))
    }

    /// The person `person`, no member of [`LOCAL_WORKSPACE`], granted exactly `kinds` at
    /// `relative`, until Unix second `until` if given, decided by a harness host.
    #[must_use]
    pub fn granted(
        person: &MemberId,
        relative: &TreePath,
        kinds: KindSet,
        until: Option<u64>,
    ) -> Self {
        Self::hosted(HostGrants::granted(person, relative, kinds, until))
    }

    /// The owner of [`LOCAL_WORKSPACE`] acting through the agent `agent`, granted exactly `kinds`
    /// at `relative`, decided by a harness host.
    #[must_use]
    pub fn agent_granted(agent: &str, relative: &TreePath, kinds: KindSet) -> Self {
        Self::hosted(HostGrants::agent_granted(agent, relative, kinds))
    }

    /// The member access `access` a host built, decided by a harness host.
    #[must_use]
    pub fn from_access(access: MemberAccess) -> Self {
        Self::hosted(HostGrants::new(access))
    }

    /// The same context, its harness host judging grants at Unix second `now` instead of the
    /// clock.
    #[must_use]
    pub fn at_unix_time(mut self, now: u64) -> Self {
        if let Some(host) = &mut self.host {
            *host = Arc::new((**host).clone().at_unix_time(now));
        }
        self
    }

    /// The acting person.
    pub fn user(&self) -> &MemberId {
        &self.user
    }

    /// What the host said of the actions this window shows.
    #[must_use]
    pub fn shown(&self) -> &ShownVerdicts {
        &self.shown
    }

    /// The harness host deciding in place of a daemon, when there is one.
    #[must_use]
    pub fn host(&self) -> Option<&HostGrants> {
        self.host.as_deref()
    }

    /// Take the verdicts the host sent.
    pub fn decide_shown(&mut self, decisions: &[ActionDecision]) {
        if decisions.is_empty() {
            return;
        }
        Arc::make_mut(&mut self.shown).decide(decisions);
    }

    /// The verdict of `action`: what the host last sent, or what a harness host decides now.
    /// `None` until the host says.
    #[must_use]
    pub fn verdict(&self, action: &ShownAction) -> Option<ActionVerdict> {
        if self.read_only && action.kind_needed != ActionKind::Read {
            return Some(ActionVerdict::Refused(READ_ONLY.into()));
        }
        self.shown
            .verdict(action)
            .cloned()
            .or_else(|| self.host.as_ref()?.decide(action))
    }

    /// The verdict of `action` on `resource`, published by the app `app_id`.
    #[must_use]
    pub fn verdict_of(
        &self,
        app_id: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
    ) -> Option<ActionVerdict> {
        self.verdict(&shown_action(app_id, resource, action))
    }

    /// Whether the verdict of `action` lets the person take it now, asking first or not.
    #[must_use]
    pub fn allows(&self, action: &ShownAction) -> bool {
        matches!(
            self.verdict(action),
            Some(ActionVerdict::Allowed | ActionVerdict::AsksFirst)
        )
    }

    /// Whether the person may now do what `need` is to the resource `resource` of kind `kind` the
    /// app `app_id` publishes: as the verdict of its field edit `field_edit` at `need`'s field
    /// where `need` names one, else of the app's records at `need`'s kind.
    #[must_use]
    pub fn may(
        &self,
        app_id: &str,
        (kind, resource, field_edit): (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
    ) -> bool {
        if need.field.is_some() {
            let edit = ShownAction::new(None, app_id, (kind, resource, field_edit), need);
            if let Some(verdict) = self.verdict(&edit) {
                return !matches!(verdict, ActionVerdict::Refused(_));
            }
        }
        self.allows(&ShownAction::of_app(app_id, need.kind))
    }

    /// Whether the person may take the action `action` of some record of the app `app_id` of
    /// kind `kind` it shows, decided at what `need` says: as the host's verdict of it on any of
    /// them, and before the host said, as a harness host decides it.
    #[must_use]
    pub fn may_on_some(
        &self,
        app_id: &str,
        (kind, action): (&str, &str),
        need: &tilo_actions::ActionNeed,
    ) -> bool {
        if self.read_only && need.kind != ActionKind::Read {
            return false;
        }
        let mut said = self
            .shown
            .verdicts_of(action, need.field.as_deref())
            .peekable();
        if said.peek().is_some() {
            return said.any(|verdict| !matches!(verdict, ActionVerdict::Refused(_)));
        }
        self.host.as_ref().is_some_and(|host| {
            matches!(
                host.decide(&ShownAction::new(None, app_id, (kind, "", action), need)),
                Some(ActionVerdict::Allowed | ActionVerdict::AsksFirst)
            )
        })
    }

    /// Whether the person may take the workspace's governance action `action` (inviting
    /// someone, its settings) through any window.
    #[must_use]
    pub fn may_govern(&self, action: &str) -> bool {
        self.allows(&ShownAction::of_workspace(
            action,
            policy::roles::GOVERNANCE_KIND,
        ))
    }

    /// Whether a control of `action` on `resource`, published by the app `app_id`, is shown:
    /// unless its verdict refuses it. One still being decided is shown, and its press waits.
    #[must_use]
    pub fn shows(
        &self,
        app_id: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
    ) -> bool {
        !matches!(
            self.verdict_of(app_id, resource, action),
            Some(ActionVerdict::Refused(_))
        )
    }

    /// `resource`, published by the app `app_id`, as its controls show it: without the actions
    /// whose verdict refuses them, and the nested resources that keep none. Those still being
    /// decided stay, and a press of one waits for its verdict. An agent is told what it may not do
    /// on a resource it may otherwise act on, rather than left to read an absence as there being
    /// no such action. `None` when nothing is left.
    #[must_use]
    pub fn shown_resource(
        &self,
        app_id: &str,
        mut resource: tilo_actions::ResourceSpec,
    ) -> Option<tilo_actions::ResourceSpec> {
        let shown: Vec<bool> = resource
            .actions
            .iter()
            .map(|action| self.shows_on(app_id, &resource, action))
            .collect();
        let refused: Vec<String> = resource
            .actions
            .iter()
            .zip(&shown)
            .filter(|(_, shown)| !**shown)
            .map(|(action, _)| action.id.clone())
            .collect();
        if !refused.is_empty() {
            if self.lists_refused() && shown.contains(&true) {
                // Listed as not allowed, and still named by the carriers beside them: a call of
                // one through a carrier is refused for the value it carries, as a permission.
                for (action, shown) in resource.actions.iter_mut().zip(&shown) {
                    if !shown {
                        action.type_mut().contract.availability =
                            tilo_actions::Availability::not_allowed(None);
                    }
                }
            } else {
                resource.retain_actions(|action| !refused.contains(&action.id));
                let refused: Vec<&str> = refused.iter().map(String::as_str).collect();
                for action in &mut resource.actions {
                    action.without_carried(&refused);
                }
            }
        }
        resource.children = std::mem::take(&mut resource.children)
            .into_iter()
            .filter_map(|child| self.shown_resource(app_id, child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }

    /// Whether what the person may not do is listed to them as not allowed: an agent's.
    fn lists_refused(&self) -> bool {
        self.user.is_agent()
            || self
                .host
                .as_ref()
                .is_some_and(|host| host.access().acting.is_agent())
    }

    /// Whether `action` is shown on `resource`: as its verdict says, and a carrier (a typed edit,
    /// a group, a field edit) also wherever an edit it carries is, as each value it carries is
    /// decided when it is invoked.
    fn shows_on(
        &self,
        app_id: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
    ) -> bool {
        self.shows(app_id, resource, action)
            || (action.carries_actions() && self.carries_a_shown_edit(app_id, resource))
    }

    /// Whether `resource` shows an edit a carrier beside it carries: an action of its own that is
    /// no carrier and no read.
    fn carries_a_shown_edit(&self, app_id: &str, resource: &tilo_actions::ResourceSpec) -> bool {
        resource.actions.iter().any(|edit| {
            !edit.carries_actions()
                && edit.contract.need().kind != ActionKind::Read
                && self.shows(app_id, resource, edit)
        })
    }

    /// Whether a press of `action` on `resource`, published by `app_id`, runs now as its verdict
    /// admits it: allowed, or asking first and `confirmed` by the person. The verbs a screen
    /// reader publishes for a drawn control are the person's own pointer and keyboard: what the
    /// control then does is admitted as its own action.
    ///
    /// # Errors
    /// Until the verdict comes, that it is not ready ([`tilo_actions::api::NOT_READY`]): a
    /// person's press waits and is asked again. Otherwise why it does not run:
    /// [`tilo_actions::ApiError::ConfirmationRequired`] until the person confirmed what asks
    /// first, or the verdict's refusal. An invocation whose need depends on the value it carries
    /// (a typed edit, a group) is decided at what that value needs (`value`: its typed arguments,
    /// or its text argument).
    pub fn admit(
        &self,
        app_id: &str,
        resource: &tilo_actions::ResourceSpec,
        action: &tilo_actions::ActionSpec,
        value: (
            Option<&serde_json::Map<String, serde_json::Value>>,
            Option<&str>,
        ),
        confirmed: bool,
    ) -> Result<(), tilo_actions::ApiError> {
        if tilo_actions::kind_is_a_drawn_control(&resource.kind)
            && action.operation_type.as_deref() == Some(tilo_actions::SEMANTIC_OPERATION_TYPE)
        {
            return Ok(());
        }
        let declared = shown_action(app_id, resource, action);
        let need = action.need_for(value.0, value.1);
        let verdict = if need == action.contract.need() {
            self.verdict(&declared)
        } else {
            let carried = ShownAction::new(
                resource.workspace.as_deref(),
                app_id,
                (&resource.kind, resource.id.as_str(), &action.id),
                &need,
            );
            self.verdict(&carried).or_else(|| self.verdict(&declared))
        };
        match verdict {
            None => Err(tilo_actions::ApiError::Unavailable {
                code: tilo_actions::api::NOT_READY.into(),
                reason: shown::CHECKING.into(),
            }),
            Some(ActionVerdict::Allowed) => Ok(()),
            Some(ActionVerdict::AsksFirst) if confirmed => Ok(()),
            Some(ActionVerdict::AsksFirst) => Err(tilo_actions::ApiError::ConfirmationRequired),
            Some(ActionVerdict::Refused(reason)) => Err(tilo_actions::ApiError::Unavailable {
                code: "permission_denied".into(),
                reason,
            }),
        }
    }
}

/// The action `action` on `resource`, published by the app `app_id`, as its verdict names it.
#[must_use]
pub fn shown_action(
    app_id: &str,
    resource: &tilo_actions::ResourceSpec,
    action: &tilo_actions::ActionSpec,
) -> ShownAction {
    ShownAction::new(
        resource.workspace.as_deref(),
        app_id,
        (&resource.kind, resource.id.as_str(), &action.id),
        &action.contract.need(),
    )
}

/// The window's verdicts its host and components share, which change as the host sends others.
/// Every change goes through [`Self::set`] or [`Self::update`], which moves [`shared_revision`]
/// on, so whatever derives something from them (which controls may run) derives it again only
/// after a change, not on every frame.
#[derive(Clone)]
pub struct SharedPermissions(std::rc::Rc<std::cell::RefCell<PermissionContext>>);

thread_local! {
    static SHARED_REVISION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Moves on with every change of any [`SharedPermissions`] on this thread.
#[must_use]
pub fn shared_revision() -> u64 {
    SHARED_REVISION.get()
}

fn moved() {
    SHARED_REVISION.set(SHARED_REVISION.get().wrapping_add(1));
}

impl SharedPermissions {
    /// Share `context`.
    #[must_use]
    pub fn new(context: PermissionContext) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(context)))
    }

    /// The context in force now, to read.
    ///
    /// # Panics
    /// While a [`Self::set`] of the same permissions runs.
    pub fn borrow(&self) -> std::cell::Ref<'_, PermissionContext> {
        self.0.borrow()
    }

    /// [`Self::borrow`], or an error while a change of them runs.
    ///
    /// # Errors
    /// While a [`Self::set`] or [`Self::update`] of the same permissions runs.
    pub fn try_borrow(
        &self,
    ) -> Result<std::cell::Ref<'_, PermissionContext>, std::cell::BorrowError> {
        self.0.try_borrow()
    }

    /// Change the context in place (the host's verdicts arrived).
    pub fn update<R>(&self, change: impl FnOnce(&mut PermissionContext) -> R) -> R {
        let (result, changed) = {
            let mut live = self.0.borrow_mut();
            let before = live.clone();
            let result = change(&mut live);
            (result, *live != before)
        };
        if changed {
            moved();
        }
        result
    }

    /// The context in force now.
    #[must_use]
    pub fn get(&self) -> PermissionContext {
        self.0.borrow().clone()
    }

    /// Put `context` in force for everyone sharing it. What the host said of the actions shown
    /// stays: only the host changes it.
    pub fn set(&self, context: PermissionContext) {
        {
            let mut live = self.0.borrow_mut();
            let mut context = context;
            context.shown = Arc::clone(&live.shown);
            if *live == context {
                return;
            }
            *live = context;
        }
        moved();
    }

    /// Take the verdicts the host sent ([`PermissionContext::decide_shown`]).
    pub fn decide_shown(&self, decisions: &[ActionDecision]) {
        self.update(|context| context.decide_shown(decisions));
    }

    /// Whether `self` and `other` are the same shared permissions.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }
}

impl From<PermissionContext> for SharedPermissions {
    fn from(context: PermissionContext) -> Self {
        Self::new(context)
    }
}

impl std::fmt::Debug for SharedPermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedPermissions")
            .field(&*self.0.borrow())
            .finish()
    }
}

/// The verdicts a window's host shares with a record it opens, as they stand *now*: the host
/// changes them as grants change. Read-only: only the host changes them.
#[derive(Clone)]
pub struct LivePermissions(SharedPermissions);

impl LivePermissions {
    /// Follow the verdicts the host shares with its components.
    #[must_use]
    pub fn shared(context: SharedPermissions) -> Self {
        Self(context)
    }

    /// The verdict of `action` as of now ([`PermissionContext::verdict`]).
    #[must_use]
    pub fn verdict(&self, action: &ShownAction) -> Option<ActionVerdict> {
        self.0.borrow().verdict(action)
    }

    /// Whether the verdict of `action` lets the person take it now
    /// ([`PermissionContext::allows`]).
    #[must_use]
    pub fn allows(&self, action: &ShownAction) -> bool {
        self.0.borrow().allows(action)
    }

    /// Whether the person may now do what `need` is ([`PermissionContext::may`]).
    #[must_use]
    pub fn may(
        &self,
        app_id: &str,
        at: (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
    ) -> bool {
        self.0.borrow().may(app_id, at, need)
    }

    /// Whether the person may take the governance action `action`
    /// ([`PermissionContext::may_govern`]).
    #[must_use]
    pub fn may_govern(&self, action: &str) -> bool {
        self.0.borrow().may_govern(action)
    }

    /// The context in force now.
    #[must_use]
    pub fn current(&self) -> PermissionContext {
        self.0.get()
    }

    /// Take the host's verdicts ([`PermissionContext::decide_shown`]).
    pub fn decide_shown(&self, decisions: &[ActionDecision]) {
        self.0.decide_shown(decisions);
    }
}

impl std::fmt::Debug for LivePermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LivePermissions")
            .field(&*self.0.borrow())
            .finish()
    }
}
