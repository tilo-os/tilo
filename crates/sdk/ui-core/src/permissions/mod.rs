//! The toolkit's permission gate.
//!
//! There is exactly **one** permission model in Tilo: the capability
//! model in [`collab_types::caps`] — `(subject × scope × verb)`
//! evaluated by [`decide_capability`], deny-biased and fail-closed. This
//! module is the toolkit-side *adapter*, not a second model:
//!
//! - a widget declares "acting on me needs verb `V` on resource `R`"
//!   by returning a [`PermissionScope`] from `Widget::permission_scope`;
//! - the host installs the [`Actor`] and [`CapOverride`] set those
//!   declarations are decided against, via
//!   [`PermissionContext::from_capabilities`];
//! - [`PermissionContext::check`] answers by calling
//!   [`decide_capability`]. Nothing here decides anything itself.
//!
//! Every vocabulary word — subject, scope, verb, expiry, effect — is
//! re-exported from `collab-types` rather than redefined, so a
//! capability minted, persisted, or replicated is the *same value* the
//! toolkit evaluates.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
struct PolicyTimeSampler(Arc<dyn Fn() -> Result<u64, String> + Send + Sync>);

impl std::fmt::Debug for PolicyTimeSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PolicyTimeSampler(..)")
    }
}

impl PartialEq for PolicyTimeSampler {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PolicyTimeSampler {}

pub use collab_types::caps::{
    Actor, CapDecision, CapOverride, Effect, Expiry, Principal, ResourceRef, Scope, Subject, Verb,
    decide_capability,
};
pub use collab_types::role::Role;

/// The id the local, unrestricted user acts as under
/// [`PermissionContext::permissive`].
pub const LOCAL_USER: &str = "local";

/// Which system app a presence key or a permission scope refers to.
///
/// This is an *identity*, not a policy: it names an app so a presence
/// cursor can be scoped to it. The stable string [`AppKind::app_id`]
/// returns is the same app id the compositor's portal and a
/// `Principal::App` subject use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}

impl AppKind {
    /// The stable app id — the one vocabulary `Scope::App`,
    /// `ResourceRef::App` and `Principal::App` are all phrased in.
    pub fn app_id(self) -> &'static str {
        match self {
            AppKind::Terminal => "terminal",
            AppKind::FileExplorer => "file-explorer",
            AppKind::Browser => "browser",
            AppKind::Settings => "settings",
            AppKind::Unknown => "unknown",
        }
    }
}

/// A person's stable id, as apps and presence spell it.
///
/// Interchangeable with [`Principal::Person`] — see
/// [`UserId::principal`]. Kept as a distinct newtype since presence and
/// app code pass a bare user string around and gain nothing from a
/// tagged enum.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

impl UserId {
    pub fn principal(&self) -> Principal {
        Principal::Person(self.0.clone())
    }
}

/// What a widget asks permission for: a resource, and the verb needed
/// to act on it. Both halves are the capability model's own types —
/// there is no toolkit-local resource or op enum to keep in sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionScope {
    pub resource: ResourceRef,
    pub op: Verb,
}

impl PermissionScope {
    pub fn workspace(op: Verb) -> Self {
        Self {
            resource: ResourceRef::Workspace,
            op,
        }
    }

    pub fn app(app_id: impl Into<String>, op: Verb) -> Self {
        Self {
            resource: ResourceRef::App(app_id.into()),
            op,
        }
    }

    /// One exported operation on one resource within an app. An app-wide
    /// capability still covers this through [`Scope::App`], while
    /// [`Scope::Action`] can grant or deny the individual operation on that
    /// resource without widening the rest of the app, or the same action on
    /// its other resources.
    pub fn action(
        app_id: impl Into<String>,
        resource_id: impl Into<String>,
        action_id: impl Into<String>,
        op: Verb,
    ) -> Self {
        Self {
            resource: ResourceRef::Action {
                app: app_id.into(),
                resource: resource_id.into(),
                action: action_id.into(),
            },
            op,
        }
    }

    pub fn app_kind(kind: AppKind, op: Verb) -> Self {
        Self::app(kind.app_id(), op)
    }

    /// Acting on a filesystem path.
    ///
    /// Roles carry **no** base authority over the filesystem
    /// (`collab_types::caps::role_base_applies`), so this always needs
    /// an explicit capability — even for an Owner.
    pub fn path(path: impl Into<String>, op: Verb) -> Self {
        Self {
            resource: ResourceRef::FsPath(path.into()),
            op,
        }
    }
}

/// The actor + capability set a frame's permission checks are decided
/// against. Held by `App`, consulted by `app::dispatch_gated_event`
/// before a gated widget ever sees an event. Construct with
/// [`PermissionContext::permissive`] (the local unrestricted user) or
/// [`PermissionContext::from_capabilities`] (a real actor + capabilities).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionContext {
    /// The acting person's id, when the actor names one. Empty for an
    /// app / agent / token actor, which is not a person.
    user: UserId,
    actor: Actor,
    caps: Vec<CapOverride>,
    /// The instant expiry is evaluated against (seconds since the Unix
    /// epoch) for legacy contexts without a retained sampler. `None` disables
    /// expiry evaluation and is only for deliberately unclocked local contexts.
    now: Option<u64>,
    policy_time: Option<PolicyTimeSampler>,
    action_authority: Option<ActionAuthoritySampler>,
}

/// Inert directory cache identity. It cannot make a permission decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryPermissionWitness {
    user: UserId,
    actor: Actor,
    caps: Vec<CapOverride>,
    live: Vec<bool>,
    action_authority: Option<(String, Box<DirectoryPermissionWitness>)>,
}

impl PermissionContext {
    /// The **unrestricted local user**: one person holding one explicit
    /// workspace-wide [`Verb::Admin`] capability.
    ///
    /// Not a bypass flag — the permissiveness is a real [`CapOverride`]
    /// that [`decide_capability`] evaluates like any other, and can be
    /// inspected, printed, or revoked. `Scope::Workspace` covers every
    /// resource including filesystem paths, devices and network hosts.
    ///
    /// Right for a solo install, `--shots`, and any session the
    /// compositor has not handed a narrower one. **Wrong** for anything
    /// acting on a remote peer's behalf.
    pub fn permissive() -> Self {
        Self::from_capabilities(
            Actor::person(Some(Role::Owner), LOCAL_USER),
            vec![
                CapOverride::grant(Subject::person(LOCAL_USER), Scope::Workspace, Verb::Admin)
                    .with_id("local-user-unrestricted"),
            ],
        )
    }

    /// Build a context from a real actor and the capabilities it holds.
    /// Expiry is evaluated against the wall clock at construction time;
    /// use [`PermissionContext::at_unix_time`] to pin it.
    pub fn from_capabilities(actor: Actor, caps: Vec<CapOverride>) -> Self {
        let user = UserId(
            actor
                .principals
                .iter()
                .find_map(|p| match p {
                    Principal::Person(id) => Some(id.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
        );
        Self {
            user,
            actor,
            caps,
            now: crate::time::try_unix_now(),
            policy_time: None,
            action_authority: None,
        }
    }

    /// Retain the actual host's fallible policy-time sampler (Unix seconds).
    /// This supplies time only, not authority. The host must validate its exact
    /// clock/origin and current guarded policy, including the captured role.
    /// Every decision resamples; failure denies without consulting the wall
    /// clock or a numeric pin. Clones retain this same sampler.
    #[must_use]
    pub fn with_policy_time_sampler(
        mut self,
        sampler: Arc<dyn Fn() -> Result<u64, String> + Send + Sync>,
    ) -> Self {
        self.policy_time = Some(PolicyTimeSampler(sampler));
        self
    }

    /// Pin a legacy context's expiry time. An installed host sampler always
    /// takes precedence, including when it fails.
    #[must_use]
    pub fn at_unix_time(mut self, now: u64) -> Self {
        self.now = Some(now);
        self
    }

    /// Compare the current policy meaning, not callback allocation or elapsed
    /// seconds. Sampling is fallible; no cached/empty witness replaces failure.
    pub(crate) fn directory_witness(&self) -> Result<DirectoryPermissionWitness, String> {
        let now = match &self.policy_time {
            Some(sampler) => Some((sampler.0)()?),
            None => self.now,
        };
        let action_authority = self
            .action_authority
            .as_ref()
            .map(|authority| {
                let current = authority.sample_for(&self.user, &self.actor)?;
                Ok::<_, String>((
                    authority.app_id.clone(),
                    Box::new(current.directory_witness()?),
                ))
            })
            .transpose()?;
        Ok(DirectoryPermissionWitness {
            user: self.user.clone(),
            actor: self.actor.clone(),
            caps: self.caps.clone(),
            live: self
                .caps
                .iter()
                .map(|cap| cap.expiry.is_live_at(now))
                .collect(),
            action_authority,
        })
    }

    /// Refresh an execution check, preserving the actual host's retained clock.
    pub(crate) fn at_current_time(&self) -> Result<Self, String> {
        let now = match &self.policy_time {
            Some(sampler) => (sampler.0)()?,
            None => crate::time::try_unix_now().ok_or("execution time unavailable")?,
        };
        Ok(self.clone().at_unix_time(now))
    }

    #[must_use]
    pub fn granting(mut self, cap: CapOverride) -> Self {
        self.caps.push(cap);
        self
    }

    /// The acting person's id. Empty when the actor is not a person.
    pub fn user(&self) -> &UserId {
        &self.user
    }

    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// The role the actor holds, if any. Apps, agents and bearer
    /// tokens hold none and act only through explicit capabilities.
    pub fn role(&self) -> Option<Role> {
        self.actor.role
    }

    /// The capabilities in force, for a share list or an audit view.
    pub fn capabilities(&self) -> &[CapOverride] {
        &self.caps
    }

    pub fn decide(&self, resource: &ResourceRef, op: Verb) -> CapDecision {
        let now = match &self.policy_time {
            Some(sampler) => match (sampler.0)() {
                Ok(now) => Some(now),
                Err(_) => return CapDecision::DeniedByRole,
            },
            None => self.now,
        };
        if op == Verb::Admin
            && let Some(authority) = &self.action_authority
            && authority.matches(resource)
        {
            return authority
                .sample_for(&self.user, &self.actor)
                .map_or(CapDecision::DeniedByRole, |current| {
                    current.decide(resource, op)
                });
        }
        decide_capability(&self.actor, resource, op, &self.caps, now)
    }

    /// May the actor do this? Fail-closed: anything not affirmatively
    /// allowed is denied.
    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.decide(&scope.resource, scope.op) == CapDecision::Allow
    }

    /// [`PermissionContext::check`] with the reason, so a caller can
    /// distinguish "your role doesn't allow this" from "a restriction on
    /// this scope blocks it". `None` means allowed.
    pub fn check_with_denied_feedback(&self, scope: &PermissionScope) -> Option<CapDecision> {
        match self.decide(&scope.resource, scope.op) {
            CapDecision::Allow => None,
            denied => Some(denied),
        }
    }

    /// May the actor perform `action` on the resource `resource_id`,
    /// published by the app `app_id`? The verb is the one the action
    /// declares it needs; a grant on the app, or on this one action of this
    /// resource, allows it.
    pub fn permits_action(
        &self,
        app_id: &str,
        resource_id: &str,
        action: &tilo_actions::ActionSpec,
    ) -> bool {
        let verb = match action.contract.permission {
            tilo_actions::PermissionRequirement::None => return true,
            tilo_actions::PermissionRequirement::Read => Verb::Read,
            tilo_actions::PermissionRequirement::Comment => Verb::Comment,
            tilo_actions::PermissionRequirement::Write => Verb::Write,
            tilo_actions::PermissionRequirement::Invite => Verb::Invite,
            tilo_actions::PermissionRequirement::Admin => Verb::Admin,
        };
        self.check(&PermissionScope::action(
            app_id,
            resource_id,
            action.id.clone(),
            verb,
        ))
    }

    /// May the actor perform this one invocation of `action`, published by
    /// the app `app_id`: what its arguments ask
    /// ([`ActionSpec::permission_for_json`](tilo_actions::ActionSpec::permission_for_json))
    /// rather than the least the action ever needs — a generic edit that
    /// replies needs commenting, one that deletes writing. `typed` are the
    /// JSON arguments, else `argument` the palette's text.
    pub fn permits_invocation(
        &self,
        app_id: &str,
        resource_id: &str,
        action: &tilo_actions::ActionSpec,
        typed: Option<&serde_json::Map<String, serde_json::Value>>,
        argument: Option<&str>,
    ) -> bool {
        let requirement = match typed {
            Some(arguments) => action.permission_for_json(arguments),
            None => action.permission_for_argument(argument),
        };
        let verb = match requirement {
            tilo_actions::PermissionRequirement::None => return true,
            tilo_actions::PermissionRequirement::Read => Verb::Read,
            tilo_actions::PermissionRequirement::Comment => Verb::Comment,
            tilo_actions::PermissionRequirement::Write => Verb::Write,
            tilo_actions::PermissionRequirement::Invite => Verb::Invite,
            tilo_actions::PermissionRequirement::Admin => Verb::Admin,
        };
        self.check(&PermissionScope::action(
            app_id,
            resource_id,
            action.id.clone(),
            verb,
        ))
    }

    /// `resource`, published by the app `app_id`, as the actor may reach
    /// it: the actions it may perform, and the nested resources that keep
    /// any. Denied is absent, never merely marked, so nothing downstream can
    /// decide to perform what it may not. `None` when nothing is left.
    pub fn permitted_resource(
        &self,
        app_id: &str,
        mut resource: tilo_actions::Resource,
    ) -> Option<tilo_actions::Resource> {
        let resource_id = resource.id.as_str().to_owned();
        resource
            .actions
            .retain(|action| self.permits_action(app_id, &resource_id, action));
        resource.children = resource
            .children
            .into_iter()
            .filter_map(|child| self.permitted_resource(app_id, child))
            .collect();
        (!resource.actions.is_empty() || !resource.children.is_empty()).then_some(resource)
    }
}

/// A participant's permissions as they stand *now*, following every change the
/// host makes while the app runs: a role changed by the record's owner, a share
/// revoked, a capability granted. Read-only: only the host changes them.
///
/// A [`PermissionContext`] is a snapshot; code that decides once at launch
/// what a participant may do goes stale the moment their role changes. Hold
/// one of these instead and ask again when it matters (every frame is cheap).
#[derive(Clone)]
pub struct LivePermissions(std::rc::Rc<std::cell::RefCell<PermissionContext>>);

impl LivePermissions {
    /// Follow the context the host shares with its components.
    pub(crate) fn shared(context: std::rc::Rc<std::cell::RefCell<PermissionContext>>) -> Self {
        Self(context)
    }

    /// Permissions that never change: a preview of a past version, a capture,
    /// a test fixture.
    pub fn fixed(context: PermissionContext) -> Self {
        Self(std::rc::Rc::new(std::cell::RefCell::new(context)))
    }

    /// May the participant do this, as of now?
    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.0.borrow().check(scope)
    }

    /// The permissions in force now.
    pub fn current(&self) -> PermissionContext {
        self.0.borrow().clone()
    }
}

impl std::fmt::Debug for LivePermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LivePermissions")
            .field(&*self.0.borrow())
            .finish()
    }
}

mod action_authority;
pub mod capability;
use action_authority::ActionAuthoritySampler;

#[cfg(test)]
mod policy_time_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_grant_exactly_their_scope_while_live() {
        // phase: permissive_is_a_capability_not_a_bypass
        // The permissive context must be permissive *because of a
        // capability*, not a special case in the checker: drop the grant
        // and the same actor is denied.
        {
            let ctx = PermissionContext::permissive();
            assert!(ctx.check(&PermissionScope::path(
                "/var/workspace/secret.png",
                Verb::Read
            )));
            assert_eq!(ctx.capabilities().len(), 1);
            assert_eq!(ctx.capabilities()[0].verb, Verb::Admin);
            assert_eq!(ctx.capabilities()[0].scope, Scope::Workspace);

            // Same actor, no capability: a role confers no base authority
            // over paths.
            let stripped = PermissionContext::from_capabilities(
                Actor::person(Some(Role::Owner), LOCAL_USER),
                vec![],
            );
            assert!(!stripped.check(&PermissionScope::path(
                "/var/workspace/secret.png",
                Verb::Read
            )));
        }
        // phase: action_grant_is_narrow_while_app_grant_covers_every_action
        {
            let actor = Actor::agent("scribe");
            let narrow = PermissionContext::from_capabilities(
                actor.clone(),
                vec![CapOverride::grant(
                    Subject::agent("scribe"),
                    Scope::Action {
                        app: "notes".into(),
                        resource: "notes.page:1".into(),
                        action: "note.open".into(),
                    },
                    Verb::Read,
                )],
            );
            assert!(narrow.check(&PermissionScope::action(
                "notes",
                "notes.page:1",
                "note.open",
                Verb::Read
            )));
            assert!(!narrow.check(&PermissionScope::action(
                "notes",
                "notes.page:1",
                "note.delete",
                Verb::Read
            )));
            assert!(
                !narrow.check(&PermissionScope::action(
                    "notes",
                    "notes.page:2",
                    "note.open",
                    Verb::Read
                )),
                "the same action on another page is not granted"
            );

            let broad = PermissionContext::from_capabilities(
                actor,
                vec![CapOverride::grant(
                    Subject::agent("scribe"),
                    Scope::App("notes".into()),
                    Verb::Read,
                )],
            );
            assert!(broad.check(&PermissionScope::action(
                "notes",
                "notes.page:2",
                "note.delete",
                Verb::Read
            )));
        }
        // phase: lapsed_capability_no_longer_grants
        // NEGATIVE: a capability that has lapsed is not a capability.
        // Same context, same actor, same grant — only the clock differs.
        {
            let cap = CapOverride::grant(
                Subject::person("u_guest"),
                Scope::FsPath("/var/workspace/shared".into()),
                Verb::Write,
            )
            .with_expiry(1_000);
            let actor = Actor::person(Some(Role::Viewer), "u_guest");
            let scope = PermissionScope::path("/var/workspace/shared/notes.md", Verb::Write);

            let live = PermissionContext::from_capabilities(actor.clone(), vec![cap.clone()])
                .at_unix_time(999);
            assert!(live.check(&scope), "before the deadline the grant holds");

            let lapsed = PermissionContext::from_capabilities(actor, vec![cap]).at_unix_time(1_001);
            assert!(
                !lapsed.check(&scope),
                "after the deadline the grant is gone"
            );
            assert_eq!(
                lapsed.check_with_denied_feedback(&scope),
                Some(CapDecision::DeniedByRole),
                "an expired capability is invisible — the actor falls back to its role, which \
             confers no authority over the filesystem"
            );
        }
    }

    #[test]
    fn invocation_grants_keep_resource_identity_and_argument_permission() {
        use tilo_actions::{ActionSpec, OperationKind, PermissionRequirement};

        let mut action =
            ActionSpec::operation("edit", "Edit", OperationKind::Effect, "test.edit", &())
                .unwrap()
                .with_permission_of(|_| PermissionRequirement::Write);
        action.contract.permission = PermissionRequirement::Read;
        let context = |verb| {
            PermissionContext::from_capabilities(
                Actor::agent("scribe"),
                vec![CapOverride::grant(
                    Subject::agent("scribe"),
                    Scope::Action {
                        app: "notes".into(),
                        resource: "notes.page:1".into(),
                        action: "edit".into(),
                    },
                    verb,
                )],
            )
        };
        let arguments = serde_json::Map::new();
        let write = context(Verb::Write);
        assert!(write.permits_invocation("notes", "notes.page:1", &action, Some(&arguments), None));
        assert!(!write.permits_invocation(
            "notes",
            "notes.page:2",
            &action,
            Some(&arguments),
            None
        ));
        assert!(!write.permits_invocation(
            "other",
            "notes.page:1",
            &action,
            Some(&arguments),
            None
        ));
        assert!(!context(Verb::Read).permits_invocation(
            "notes",
            "notes.page:1",
            &action,
            Some(&arguments),
            None,
        ));
        assert!(write.permits_invocation("notes", "notes.page:1", &action, None, None));
        assert!(!write.permits_invocation("notes", "notes.page:2", &action, None, None));
    }

    #[test]
    fn denials_are_default_and_attributed() {
        // phase: viewer_is_denied_write_on_an_artifact
        // NEGATIVE: a Viewer may read an artifact but must not write it.
        {
            use collab_types::channel::ArtifactId;
            use collab_types::role::ArtifactKind;

            let ctx = PermissionContext::from_capabilities(
                Actor::person(Some(Role::Viewer), "u_guest"),
                vec![],
            );
            let artifact = ResourceRef::Artifact {
                app: Some("notes".into()),
                kind: ArtifactKind::Note,
                instance: ArtifactId::new(),
            };

            let write = PermissionScope {
                resource: artifact.clone(),
                op: Verb::Write,
            };
            assert!(
                !ctx.check(&write),
                "a Viewer must not be able to write an artifact"
            );
            assert_eq!(
                ctx.check_with_denied_feedback(&write),
                Some(CapDecision::DeniedByRole),
                "the denial must be attributed to the role, not to a restriction"
            );

            // …reading is exactly what a Viewer may do.
            let read = PermissionScope {
                resource: artifact,
                op: Verb::Read,
            };
            assert!(ctx.check(&read), "a Viewer may read the same artifact");
        }
        // phase: restriction_denies_and_is_attributed_to_the_restriction
        // A `Restrict` capability caps authority below the role baseline.
        {
            let ctx = PermissionContext::from_capabilities(
                Actor::person(Some(Role::Editor), "u_editor"),
                vec![CapOverride::restrict_role_in_app(
                    Role::Editor,
                    "terminal",
                    Verb::Comment,
                )],
            );
            let scope = PermissionScope::app_kind(AppKind::Terminal, Verb::Write);
            assert!(!ctx.check(&scope));
            assert_eq!(
                ctx.check_with_denied_feedback(&scope),
                Some(CapDecision::DeniedByRestriction)
            );
        }
        // phase: actor_without_role_or_capability_is_denied_everything
        // An actor with no role and no capability — an app, an agent, a
        // stranger — is denied everything. Fail-closed by construction.
        {
            let ctx = PermissionContext::from_capabilities(Actor::app("dev.example.thing"), vec![]);
            assert!(!ctx.check(&PermissionScope::workspace(Verb::Read)));
            assert!(!ctx.check(&PermissionScope::app_kind(
                AppKind::FileExplorer,
                Verb::Read
            )));
            assert!(!ctx.check(&PermissionScope::path("/etc/shadow", Verb::Read)));
            assert!(ctx.user().0.is_empty(), "an app is not a person");
        }
    }
}
