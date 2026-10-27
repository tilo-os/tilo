//! The authority gate for the headless action API.
//!
//! [`ActionCatalog`](super::action_api::ActionCatalog) applies an
//! [`ActionVisibility`] at build time, so a hidden resource is absent from
//! enumeration *and* selection.
//!
//! The rule: showing a
//! resource needs no permission, taking an action on one does. So notes,
//! records, files and folders are **never** hidden — what disappears for a
//! caller who may not act is the *affordance*, not the content. Opening an
//! overlay is showing; spawning a terminal, launching an app, or minting an
//! invite are acts, and are decided by the room's policy at the action's node.
//! Anything unclassifiable is treated as an undoable act rather than waved
//! through.

use tilo_action_policy::{ActionKind, Answer, Authority, Subject, TreePath, roles};

use crate::shell::state::{Actor, Principal, Workspace};

use super::ResourceKind;
use super::action_api::{ActionVisibility, ResourceRef};

/// The app id chrome-level actions are attributed to — not exported by any
/// packaged app, so they need a stable name to be addressed by. The chrome
/// is also the one resource those actions are on.
pub const SHELL_APP_ID: &str = "shell";

/// The node of the chrome action `id`, below the room.
fn chrome_node(id: &str) -> TreePath {
    TreePath::root().app(SHELL_APP_ID).action(id)
}

/// The room governance action a chrome action performs, if it governs.
fn chrome_governance(id: &str) -> Option<&'static str> {
    Some(match id {
        "member.role.set" => roles::governance::SET_ROLE,
        "member.remove" => roles::governance::REMOVE_MEMBER,
        "agent.invite" => roles::governance::GRANT,
        "agent.remove" => roles::governance::REVOKE,
        "peer.invite" | "peer.invite.named" | "peer.connect" => roles::governance::INVITE,
        "workspace.publish" | "workspace.unpublish" => roles::governance::SETTINGS,
        "bot.add" => roles::governance::INSTALL_BOT,
        "bot.remove" => roles::governance::UNINSTALL_BOT,
        id if id.starts_with("invitation.") => roles::governance::INVITE,
        _ => return None,
    })
}

/// What invoking `resource` is, as the node below the room its action sits
/// at and the kind of action it is, or `None` when it only shows something.
/// Split out from the [`ActionVisibility`] impl so the policy can be read,
/// tested and argued about without a live workspace in hand.
pub fn required_action(
    resource: &ResourceRef,
    kind: &ResourceKind,
) -> Option<(TreePath, ActionKind)> {
    if resource.provider == super::providers::permission_prompts::PROVIDER_ID
        || resource.provider == super::providers::terminal_device_shell::PROVIDER_ID
        || resource.provider == super::providers::file_picker::PROVIDER_ID
        || resource.provider == "onboarding"
        || resource.provider == super::providers::authentication_prompts::PROVIDER_ID
    {
        return Some((chrome_node(&resource.id), ActionKind::Outside));
    }
    if resource.provider == super::providers::live_app_actions::PROVIDER_ID
        && let Some((node, kind)) =
            super::providers::live_app_actions::LiveAppActionsProvider::action_target(&resource.id)
    {
        return (kind != ActionKind::Read).then_some((node, kind));
    }
    match kind {
        // Showing is free — the larger half of the surface.
        ResourceKind::Note
        | ResourceKind::Record
        | ResourceKind::File
        | ResourceKind::Folder
        | ResourceKind::Creator
        | ResourceKind::Video
        | ResourceKind::Playlist => None,

        // Launching puts a tab in the workspace: an undoable change of that
        // app.
        ResourceKind::Application => Some((
            TreePath::root().app(resource.id.clone()),
            ActionKind::Undoable,
        )),

        ResourceKind::Action => {
            if let Some(action) = chrome_governance(&resource.id) {
                return Some((
                    TreePath::root().kind(roles::ROOM_KIND).action(action),
                    roles::GOVERNANCE_KIND,
                ));
            }
            chrome_action_kind(&resource.id).map(|kind| (chrome_node(&resource.id), kind))
        }

        // Window actions and unclassified results: treat as an act, fail
        // closed.
        ResourceKind::Tab | ResourceKind::WindowGroup | ResourceKind::Custom(_) => {
            Some((chrome_node(&resource.id), ActionKind::Undoable))
        }
    }
}

/// The kind of a chrome action, from the same canonical contract exported to
/// discovery, or `None` when it only reads. Dynamic ownership actions retain
/// the contract attached to their workspace resource and reach outside.
fn chrome_action_kind(id: &str) -> Option<ActionKind> {
    let kind = if id.contains("#ownership.") {
        ActionKind::Outside
    } else {
        super::providers::actions::shell_action_contract(id).kind
    };
    (kind != ActionKind::Read).then_some(kind)
}

/// The authority `actor` holds in `workspace`: a person's own, a delegation
/// (an app, an agent, a token) bounded by the person it acts for, or the
/// delegation's alone when it acts for nobody. `None` for an actor naming no
/// one.
fn authority_of<'w>(workspace: &'w Workspace, actor: &Actor) -> Option<Authority<'w>> {
    let person = actor
        .principals
        .iter()
        .find_map(|principal| match principal {
            Principal::Person(person) => Some(person.as_str()),
            _ => None,
        });
    let delegate = actor
        .principals
        .iter()
        .find_map(|principal| match principal {
            Principal::App(app) => Some(Subject::App(app.clone())),
            Principal::Agent(agent) => Some(Subject::Agent(agent.clone())),
            Principal::Token(token) => Some(Subject::Task(token.clone())),
            Principal::Person(_) | Principal::Room(_) => None,
        });
    match (person, delegate) {
        (Some(person), None) => Some(workspace.person_authority(person)),
        (Some(person), Some(delegate)) => {
            Some(workspace.delegated_authority(person, delegate, &[]))
        }
        (None, Some(delegate)) => Some(workspace.subject_authority(delegate)),
        (None, None) => None,
    }
}

/// What `actor` gets for an action of `kind` at `node` (below the room) in
/// `workspace`, now.
#[must_use]
pub fn answer_for(
    workspace: &Workspace,
    actor: &Actor,
    node: &TreePath,
    kind: ActionKind,
) -> Answer {
    authority_of(workspace, actor).map_or(Answer::Deny, |authority| {
        authority
            .decide(
                &workspace.node().join(node),
                kind,
                &tilo_desktop_model::context_at(tilo_clock::unix_now()),
            )
            .answer
    })
}

/// Whether `actor` may invoke an action of `kind` at `node` now: allowed, or
/// asking first of a person acting as themselves, whose own gesture is the
/// answer.
#[must_use]
pub fn permits(workspace: &Workspace, actor: &Actor, node: &TreePath, kind: ActionKind) -> bool {
    match answer_for(workspace, actor, node, kind) {
        Answer::Allow => true,
        Answer::Ask => is_person_alone(actor),
        Answer::Deny => false,
    }
}

/// Whether `actor` is a person acting as themselves: the only actor whose
/// own gesture answers a grant that asks first, and the only one who decides
/// personal consent or a device shell.
fn is_person_alone(actor: &Actor) -> bool {
    !actor.principals.is_empty()
        && actor
            .principals
            .iter()
            .all(|principal| matches!(principal, Principal::Person(_) | Principal::Room(_)))
        && actor.principals.iter().any(Principal::is_person)
}

/// An [`ActionVisibility`] backed by a real workspace's policy. Fail-closed
/// by construction: no grant is Deny, and an actor naming nobody holds
/// nothing. An action that asks first is listed; it asks when invoked.
pub struct PolicyVisibility<'a> {
    workspace: &'a Workspace,
    actor: Actor,
}

impl<'a> PolicyVisibility<'a> {
    pub fn new(workspace: &'a Workspace, actor: Actor) -> Self {
        Self { workspace, actor }
    }
}

impl ActionVisibility for PolicyVisibility<'_> {
    fn can_see(&self, resource: &ResourceRef, kind: &ResourceKind) -> bool {
        // Identity setup, authentication and personal file handoff belong to
        // the local owner alone: never an admitted person, app or agent,
        // whatever their grants.
        if resource.provider == "onboarding"
            || resource.provider == super::providers::authentication_prompts::PROVIDER_ID
            || resource.provider == super::providers::file_picker::PROVIDER_ID
        {
            return is_person_alone(&self.actor)
                && self.workspace.owner().is_some_and(|owner| {
                    self.actor.principals.iter().any(
                        |principal| matches!(principal, Principal::Person(user) if user == owner),
                    )
                });
        }
        // Personal portal consent and a device shell are decided only by a
        // person allowed them outright, never by an app or an agent, whatever
        // they were granted.
        if resource.provider == super::providers::permission_prompts::PROVIDER_ID
            || resource.provider == super::providers::terminal_device_shell::PROVIDER_ID
        {
            return is_person_alone(&self.actor)
                && required_action(resource, kind).is_some_and(|(node, kind)| {
                    answer_for(self.workspace, &self.actor, &node, kind) == Answer::Allow
                });
        }
        match required_action(resource, kind) {
            None => true,
            Some((node, kind)) => {
                answer_for(self.workspace, &self.actor, &node, kind) != Answer::Deny
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{UserId, WorkspaceAuthority as _, WorkspaceId};
    use tilo_action_policy::{Change, Grant, KindSet, Role};

    fn resource(provider: &str, id: &str) -> ResourceRef {
        ResourceRef::new(provider, id)
    }

    fn owned(name: &str, owner: &UserId) -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::new(), name);
        ws.set_owner(owner.clone());
        ws
    }

    fn grant_room(ws: &mut Workspace, subject: Subject, kinds: KindSet) {
        let owner = ws.owner().cloned().expect("owned");
        let grant = Grant::new(
            format!("test:{subject:?}"),
            subject,
            ws.node(),
            kinds,
            Answer::Allow,
            Subject::person(owner.clone()),
        );
        ws.govern_as_at(&owner, Change::Grant(grant), tilo_clock::unix_now())
            .expect("the owner grants");
    }

    #[test]
    fn permission_decisions_require_a_person_allowed_outright_even_with_agent_grants() {
        let owner = "owner".to_string();
        let writer = "writer".to_string();
        let mut ws = owned("permission-gate", &owner);
        let decision = resource(
            super::super::providers::permission_prompts::PROVIDER_ID,
            "allow-always@00000000-0000-0000-0000-000000000000",
        );
        assert_eq!(
            required_action(&decision, &ResourceKind::Action)
                .expect("gated")
                .1,
            ActionKind::Outside
        );
        assert!(
            PolicyVisibility::new(&ws, ws.actor_for_person(&owner))
                .can_see(&decision, &ResourceKind::Action)
        );
        ws.admit(writer.clone(), Role::Editor);
        assert!(
            !PolicyVisibility::new(&ws, ws.actor_for_person(&writer))
                .can_see(&decision, &ResourceKind::Action),
            "an editor is asked about outside effects, so does not decide consent"
        );
        grant_room(&mut ws, Subject::agent("helper"), KindSet::ALL);
        assert!(
            !PolicyVisibility::new(
                &ws,
                Actor {
                    principals: vec![Principal::Agent("helper".into())],
                    ..Actor::default()
                }
            )
            .can_see(&decision, &ResourceKind::Action)
        );
    }

    #[test]
    fn device_shell_selection_is_never_an_agents() {
        let owner = "owner".to_string();
        let mut workspace = owned("host-gate", &owner);
        let resource = resource(
            super::super::providers::terminal_device_shell::PROVIDER_ID,
            "request@9",
        );
        assert!(
            PolicyVisibility::new(&workspace, workspace.actor_for_person(&owner))
                .can_see(&resource, &ResourceKind::Action)
        );
        grant_room(&mut workspace, Subject::agent("helper"), KindSet::ALL);
        assert!(
            !PolicyVisibility::new(
                &workspace,
                Actor {
                    principals: vec![Principal::Agent("helper".into())],
                    ..Actor::default()
                }
            )
            .can_see(&resource, &ResourceKind::Action)
        );
    }

    #[test]
    fn an_agent_cannot_attach_the_owner_principal_to_a_device_shell_request() {
        let owner = "owner".to_string();
        let workspace = owned("host-gate", &owner);
        let resource = resource(
            super::super::providers::terminal_device_shell::PROVIDER_ID,
            "request@9",
        );
        let mut actor = workspace.actor_for_person(&owner);
        actor.principals.push(Principal::Agent("helper".into()));
        assert!(
            !PolicyVisibility::new(&workspace, actor).can_see(&resource, &ResourceKind::Action)
        );
    }

    #[test]
    fn onboarding_identity_actions_are_the_owners_alone() {
        let owner = "owner".to_string();
        let mut ws = owned("identity setup", &owner);
        let selected = resource("onboarding", "onboarding.create");
        assert!(
            PolicyVisibility::new(&ws, ws.actor_for_person(&owner))
                .can_see(&selected, &ResourceKind::Action)
        );
        for principal in [
            Principal::Agent("helper".into()),
            Principal::App("so.tilo.store".into()),
        ] {
            let subject = match &principal {
                Principal::Agent(agent) => Subject::agent(agent.clone()),
                _ => Subject::app("so.tilo.store"),
            };
            grant_room(&mut ws, subject, KindSet::ALL);
            let mut mixed = ws.actor_for_person(&owner);
            mixed.principals.push(principal.clone());
            assert!(!PolicyVisibility::new(&ws, mixed).can_see(&selected, &ResourceKind::Action));
            assert!(
                !PolicyVisibility::new(
                    &ws,
                    Actor {
                        principals: vec![principal],
                        ..Actor::default()
                    }
                )
                .can_see(&selected, &ResourceKind::Action)
            );
        }
        let admin = "admin".to_string();
        ws.admit(admin.clone(), Role::Editor);
        grant_room(&mut ws, Subject::person(admin.clone()), KindSet::ALL);
        assert!(
            !PolicyVisibility::new(&ws, ws.actor_for_person(&admin))
                .can_see(&selected, &ResourceKind::Action),
            "a person the owner granted everything does not set up the owner's identity"
        );
    }

    #[test]
    fn file_picker_personal_handoff_is_the_owners_alone() {
        let owner = "owner".to_string();
        let mut ws = owned("picker", &owner);
        let selected = resource(
            super::super::providers::file_picker::PROVIDER_ID,
            "confirm@request",
        );
        assert!(
            PolicyVisibility::new(&ws, ws.actor_for_person(&owner))
                .can_see(&selected, &ResourceKind::Action)
        );
        for principal in [
            Principal::Agent("helper".into()),
            Principal::App("so.tilo.store".into()),
            Principal::Token("token".into()),
        ] {
            let mut mixed = ws.actor_for_person(&owner);
            mixed.principals.push(principal.clone());
            assert!(!PolicyVisibility::new(&ws, mixed).can_see(&selected, &ResourceKind::Action));
            assert!(
                !PolicyVisibility::new(
                    &ws,
                    Actor {
                        principals: vec![principal],
                        ..Actor::default()
                    }
                )
                .can_see(&selected, &ResourceKind::Action)
            );
        }
        let admitted = "admitted".to_string();
        ws.admit(admitted.clone(), Role::Editor);
        grant_room(&mut ws, Subject::person(admitted.clone()), KindSet::ALL);
        assert!(
            !PolicyVisibility::new(&ws, ws.actor_for_person(&admitted))
                .can_see(&selected, &ResourceKind::Action)
        );
    }

    #[test]
    fn showing_needs_no_authority_but_launching_is_decided_on_the_app() {
        // Phase `showing_a_document_requires_no_authority`.
        // A document-shaped resource is never gated.
        {
            for kind in [
                ResourceKind::Note,
                ResourceKind::Record,
                ResourceKind::File,
                ResourceKind::Folder,
            ] {
                assert_eq!(
                    required_action(&resource("notes", "abc"), &kind),
                    None,
                    "{kind:?} must not require authority to be shown"
                );
            }
        }

        // Phase `opening_an_overlay_is_showing_not_acting`.
        // Reveals a view and changes nothing; not an act.
        {
            for id in [
                "shortcut-help.toggle",
                "overview.toggle",
                "settings.open",
                "quick-settings.open",
                "quick-settings.clipboard",
                "quick-settings.devices",
                "projection.open",
                "collaborators.open",
                "room.activity.open",
            ] {
                assert_eq!(
                    required_action(&resource("actions", id), &ResourceKind::Action),
                    None,
                    "{id} opens a view and must not be gated"
                );
            }
        }

        // Phase `launching_an_app_is_decided_on_that_app`.
        // Launching an app is decided at the app being launched, so a grant
        // can name one app without opening the whole catalogue.
        {
            let (node, kind) = required_action(
                &resource("applications", "so.tilo.terminal"),
                &ResourceKind::Application,
            )
            .expect("launching must be gated");
            assert_eq!(kind, ActionKind::Undoable);
            assert_eq!(node, TreePath::root().app("so.tilo.terminal"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn acting_needs_the_matching_answer_and_unclassified_acts_fail_closed() {
        {
            for id in [
                "tab.new.terminal.here",
                "window.new.terminal",
                "tab.duplicate",
                "notifications.dnd.toggle",
            ] {
                let (_, kind) = required_action(&resource("actions", id), &ResourceKind::Action)
                    .unwrap_or_else(|| panic!("{id} must be gated"));
                assert_eq!(kind, ActionKind::Undoable, "{id}");
            }

            // Inviting, managing who is a member, and minting or revoking the
            // credentials that admit someone, govern the room.
            for (id, action) in [
                ("peer.invite", roles::governance::INVITE),
                ("peer.invite.named", roles::governance::INVITE),
                ("peer.connect", roles::governance::INVITE),
                ("agent.invite", roles::governance::GRANT),
                ("agent.remove", roles::governance::REVOKE),
                ("member.role.set", roles::governance::SET_ROLE),
                ("member.remove", roles::governance::REMOVE_MEMBER),
                ("invitation.issue", roles::governance::INVITE),
                ("invitation.list", roles::governance::INVITE),
                ("invitation.resend", roles::governance::INVITE),
                ("invitation.cancel", roles::governance::INVITE),
            ] {
                let (node, kind) = required_action(&resource("actions", id), &ResourceKind::Action)
                    .unwrap_or_else(|| panic!("{id} must be gated"));
                assert_eq!(kind, roles::GOVERNANCE_KIND, "{id}");
                assert_eq!(
                    node,
                    TreePath::root().kind(roles::ROOM_KIND).action(action),
                    "{id}"
                );
            }
        }

        {
            let (node, kind) =
                required_action(&resource("actions", "record.bring"), &ResourceKind::Action)
                    .expect("writing into a document must be gated");
            assert_eq!(kind, ActionKind::Undoable);
            assert_eq!(
                node,
                TreePath::root().app(SHELL_APP_ID).action("record.bring")
            );
        }

        // Phase `an_unauthorised_caller_cannot_see_the_document_edit`.
        // Hides the edit from someone holding nothing while still showing
        // them the document itself, against a real `Workspace`.
        {
            let owner: UserId = "owner".to_string();
            let stranger: UserId = "stranger".to_string();
            let mut ws = owned("edit-gate", &owner);

            let edit = resource("actions", "record.bring");
            let note = resource("notes", "15");

            let stranger_view = PolicyVisibility::new(&ws, ws.actor_for_person(&stranger));
            assert!(
                !stranger_view.can_see(&edit, &ResourceKind::Action),
                "someone holding nothing must not be offered the edit"
            );
            assert!(
                stranger_view.can_see(&note, &ResourceKind::Note),
                "…but the document itself stays visible: showing is free"
            );

            let owner_view = PolicyVisibility::new(&ws, ws.actor_for_person(&owner));
            assert!(
                owner_view.can_see(&edit, &ResourceKind::Action),
                "the room's owner may edit its documents"
            );

            grant_room(
                &mut ws,
                Subject::person(stranger.clone()),
                KindSet::READ_UNDOABLE,
            );
            let granted = PolicyVisibility::new(&ws, ws.actor_for_person(&stranger));
            assert!(
                granted.can_see(&edit, &ResourceKind::Action),
                "granting undoable changes must make the edit reachable"
            );
        }

        // Phase `an_unclassified_action_fails_closed`.
        // A new provider landing without a mapping should go dark, not open.
        {
            let (_, kind) = required_action(
                &resource("actions", "some.provider.landed.without.a.mapping"),
                &ResourceKind::Action,
            )
            .expect("an unmapped action must be gated, not waved through");
            assert_eq!(kind, ActionKind::Undoable);
        }

        // Phase `the_gate_hides_acts_from_someone_holding_nothing`.
        // End-to-end: an owner sees gated actions, someone holding nothing
        // does not, both still see the ungated ones.
        {
            let owner: UserId = "owner".to_string();
            let stranger: UserId = "stranger".to_string();
            let mut ws = owned("gate-test", &owner);

            let owner_view = PolicyVisibility::new(&ws, ws.actor_for_person(&owner));
            let stranger_view = PolicyVisibility::new(&ws, ws.actor_for_person(&stranger));

            let gated = resource("actions", "window.new.terminal");
            let ungated = resource("actions", "settings.open");
            let document = resource("notes", "note-1");

            assert!(
                owner_view.can_see(&gated, &ResourceKind::Action),
                "the room's owner must see an act they may perform"
            );
            assert!(
                !stranger_view.can_see(&gated, &ResourceKind::Action),
                "someone holding nothing must not see the act"
            );

            // …but showing is never gated, for either of them.
            for view in [&owner_view, &stranger_view] {
                assert!(view.can_see(&ungated, &ResourceKind::Action));
                assert!(view.can_see(&document, &ResourceKind::Note));
            }

            // A grant makes exactly that act appear, without opening
            // anything else.
            grant_room(
                &mut ws,
                Subject::person(stranger.clone()),
                KindSet::READ_UNDOABLE,
            );
            let granted_view = PolicyVisibility::new(&ws, ws.actor_for_person(&stranger));
            assert!(
                granted_view.can_see(&gated, &ResourceKind::Action),
                "granting undoable changes must make the act visible"
            );
            assert!(
                !granted_view.can_see(&resource("actions", "peer.invite"), &ResourceKind::Action),
                "undoable changes must not confer inviting"
            );
        }
    }
}
