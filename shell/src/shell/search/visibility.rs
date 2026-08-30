//! The capability gate for the headless action API.
//!
//! [`ActionCatalog`](super::action_api::ActionCatalog) applies an
//! [`ActionVisibility`] at build time, so a hidden resource is absent from
//! enumeration *and* selection.
//!
//! The rule, from [`crate::shell::actions`] and not re-decided here: showing a
//! resource needs no permission, taking an action on one does. So notes,
//! records, files and folders are **never** hidden — what disappears for a
//! caller who cannot act is the *affordance*, not the content. Opening an
//! overlay is showing; spawning a terminal, launching an app, or minting an
//! invite are acts, and are gated. Anything unclassifiable is treated as an act
//! requiring [`Verb::Write`] rather than waved through.

use crate::shell::state::{Actor, ResourceRef as CapResource, Verb, Workspace};

use super::ResourceKind;
use super::action_api::{ActionVisibility, ResourceRef};

/// The app id chrome-level actions are attributed to — not exported by any
/// packaged app, so they need a stable name to be addressed by.
pub const SHELL_APP_ID: &str = "shell";

/// What invoking `resource` requires, or `None` when it only shows
/// something. Split out from the [`ActionVisibility`] impl so the policy
/// can be read, tested and argued about without a live workspace in hand.
pub fn required_capability(
    resource: &ResourceRef,
    kind: &ResourceKind,
) -> Option<(CapResource, Verb)> {
    match kind {
        // Showing is free — the larger half of the surface.
        ResourceKind::Note
        | ResourceKind::Record
        | ResourceKind::File
        | ResourceKind::Folder
        | ResourceKind::Creator
        | ResourceKind::Video
        | ResourceKind::Playlist => None,

        // Launching puts a tab in the workspace — `Write` on that app.
        ResourceKind::Application | ResourceKind::HostApplication => {
            Some((CapResource::App(resource.id.clone()), Verb::Write))
        }

        ResourceKind::Action => chrome_action_verb(&resource.id).map(|verb| {
            (
                CapResource::Action {
                    app: SHELL_APP_ID.to_string(),
                    action: resource.id.clone(),
                },
                verb,
            )
        }),

        // Unclassified: treat as an act, fail closed.
        ResourceKind::Custom(_) => Some((
            CapResource::Action {
                app: SHELL_APP_ID.to_string(),
                action: resource.id.clone(),
            },
            Verb::Write,
        )),
    }
}

/// The verb a chrome action requires, or `None` when it only opens a view.
/// Overlay-opening entries are deliberately ungated: `settings.open`
/// reveals a panel; the setting that changes something is gated where it lives.
fn chrome_action_verb(id: &str) -> Option<Verb> {
    match id {
        // Showing: reveals a view, mutates nothing.
        "overview.toggle"
        | "settings.open"
        | "quick-settings.open"
        | "projection.open"
        | "collaborators.open"
        | "room.activity.open" => None,

        // Membership: admitting a peer or agent, or joining a room — all
        // decide who is in this room, so all gate on the same verb.
        "peer.invite" | "peer.invite.named" | "peer.connect" | "agent.invite" => Some(Verb::Invite),

        // Changes what the room holds — `Write`, same as spawning a tab.
        // Seeing which records a room holds stays ungated; adding one is an act.
        "record.bring" => Some(Verb::Write),

        // Reading a note is showing and free; changing what it says is an act.
        "text.insert" => Some(Verb::Write),

        // Changes a session-wide setting.
        "notifications.dnd.toggle" => Some(Verb::Write),

        // Everything else: acts on the room's contents ("tab.new.terminal.here",
        // "window.new.terminal", "tab.duplicate") and any unknown chrome
        // action — all fail closed to the same gate as the arms above.
        _ => Some(Verb::Write),
    }
}

/// An [`ActionVisibility`] backed by a real workspace's capability model.
/// Fail-closed by construction: [`Workspace::allows`] is
/// `collab_types::decide_capability`, denying unless something permits.
pub struct CapabilityVisibility<'a> {
    workspace: &'a Workspace,
    actor: Actor,
}

impl<'a> CapabilityVisibility<'a> {
    pub fn new(workspace: &'a Workspace, actor: Actor) -> Self {
        Self { workspace, actor }
    }
}

impl ActionVisibility for CapabilityVisibility<'_> {
    fn can_see(&self, resource: &ResourceRef, kind: &ResourceKind) -> bool {
        match required_capability(resource, kind) {
            None => true,
            Some((cap_resource, verb)) => self.workspace.allows(&self.actor, &cap_resource, verb),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{CapOverride, Principal, Scope, Subject, UserId, WorkspaceId};

    fn resource(provider: &str, id: &str) -> ResourceRef {
        ResourceRef::new(provider, id)
    }

    /// A document-shaped resource is never gated.
    #[test]
    fn showing_a_document_requires_no_capability() {
        for kind in [
            ResourceKind::Note,
            ResourceKind::Record,
            ResourceKind::File,
            ResourceKind::Folder,
        ] {
            assert_eq!(
                required_capability(&resource("notes", "abc"), &kind),
                None,
                "{kind:?} must not require a capability to be shown"
            );
        }
    }

    /// Reveals a view and changes nothing; not an act.
    #[test]
    fn opening_an_overlay_is_showing_not_acting() {
        for id in [
            "overview.toggle",
            "settings.open",
            "quick-settings.open",
            "projection.open",
            "collaborators.open",
            "room.activity.open",
        ] {
            assert_eq!(
                required_capability(&resource("actions", id), &ResourceKind::Action),
                None,
                "{id} opens a view and must not be gated"
            );
        }
    }

    /// Acts on the room's contents need `Write`; membership needs `Invite`.
    #[test]
    fn acting_on_the_room_requires_the_matching_verb() {
        let write = [
            "tab.new.terminal.here",
            "window.new.terminal",
            "tab.duplicate",
            "notifications.dnd.toggle",
        ];
        for id in write {
            let (_, verb) = required_capability(&resource("actions", id), &ResourceKind::Action)
                .unwrap_or_else(|| panic!("{id} must be gated"));
            assert_eq!(verb, Verb::Write, "{id}");
        }

        for id in [
            "peer.invite",
            "peer.invite.named",
            "peer.connect",
            "agent.invite",
        ] {
            let (_, verb) = required_capability(&resource("actions", id), &ResourceKind::Action)
                .unwrap_or_else(|| panic!("{id} must be gated"));
            assert_eq!(verb, Verb::Invite, "{id}");
        }
    }

    /// Editing a document is an act, gated on `Write` like spawning a tab.
    #[test]
    fn writing_into_a_document_requires_write() {
        let (cap, verb) =
            required_capability(&resource("actions", "text.insert"), &ResourceKind::Action)
                .expect("writing into a document must be gated");
        assert_eq!(verb, Verb::Write);
        assert_eq!(
            cap,
            CapResource::Action {
                app: SHELL_APP_ID.to_string(),
                action: "text.insert".to_string(),
            }
        );
    }

    /// Hides the edit from someone holding nothing while still showing
    /// them the document itself, against a real `Workspace`.
    #[test]
    fn an_unauthorised_caller_cannot_see_the_document_edit() {
        let owner: UserId = "owner".to_string();
        let stranger: UserId = "stranger".to_string();
        let mut ws = Workspace::new(WorkspaceId::new(), "edit-gate");
        ws.owner = Some(owner.clone());

        let edit = resource("actions", "text.insert");
        let note = resource("notes", "15");

        let stranger_view = CapabilityVisibility::new(&ws, ws.actor_for_person(&stranger));
        assert!(
            !stranger_view.can_see(&edit, &ResourceKind::Action),
            "someone holding no capability must not be offered the edit"
        );
        assert!(
            stranger_view.can_see(&note, &ResourceKind::Note),
            "…but the document itself stays visible: showing is free"
        );

        let owner_view = CapabilityVisibility::new(&ws, ws.actor_for_person(&owner));
        assert!(
            owner_view.can_see(&edit, &ResourceKind::Action),
            "the room's owner may edit its documents"
        );

        // A grant makes exactly that act appear.
        let _ = ws.grant_capability(CapOverride::grant(
            Subject::Principal(Principal::Person(stranger.clone())),
            Scope::Workspace,
            Verb::Write,
        ));
        let granted = CapabilityVisibility::new(&ws, ws.actor_for_person(&stranger));
        assert!(
            granted.can_see(&edit, &ResourceKind::Action),
            "granting Write must make the edit reachable"
        );
    }

    /// A new provider landing without a mapping should go dark, not open.
    #[test]
    fn an_unclassified_action_fails_closed() {
        let (_, verb) = required_capability(
            &resource("actions", "some.provider.landed.without.a.mapping"),
            &ResourceKind::Action,
        )
        .expect("an unmapped action must be gated, not waved through");
        assert_eq!(verb, Verb::Write);
    }

    /// Launching an app is gated on the app being launched, so a grant
    /// can name one app without opening the whole catalogue.
    #[test]
    fn launching_an_app_is_gated_on_that_app() {
        let (cap, verb) = required_capability(
            &resource("applications", "dev.qos.terminal"),
            &ResourceKind::Application,
        )
        .expect("launching must be gated");
        assert_eq!(verb, Verb::Write);
        assert_eq!(cap, CapResource::App("dev.qos.terminal".to_string()));
    }

    /// End-to-end: an owner sees gated actions, a member holding nothing
    /// does not, both still see the ungated ones.
    #[test]
    fn the_gate_hides_acts_from_a_member_without_capabilities() {
        let owner: UserId = "owner".to_string();
        let stranger: UserId = "stranger".to_string();
        let mut ws = Workspace::new(WorkspaceId::new(), "gate-test");
        ws.owner = Some(owner.clone());

        let owner_view = CapabilityVisibility::new(&ws, ws.actor_for_person(&owner));
        let stranger_view = CapabilityVisibility::new(&ws, ws.actor_for_person(&stranger));

        let gated = resource("actions", "window.new.terminal");
        let ungated = resource("actions", "settings.open");
        let document = resource("notes", "note-1");

        assert!(
            owner_view.can_see(&gated, &ResourceKind::Action),
            "the room's owner must see an act they may perform"
        );
        assert!(
            !stranger_view.can_see(&gated, &ResourceKind::Action),
            "someone holding no capability must not see the act"
        );

        // …but showing is never gated, for either of them.
        for view in [&owner_view, &stranger_view] {
            assert!(view.can_see(&ungated, &ResourceKind::Action));
            assert!(view.can_see(&document, &ResourceKind::Note));
        }

        // A grant makes exactly that act appear, without opening
        // anything else.
        let _ = ws.grant_capability(CapOverride::grant(
            Subject::Principal(Principal::Person(stranger.clone())),
            Scope::Workspace,
            Verb::Write,
        ));
        let granted_view = CapabilityVisibility::new(&ws, ws.actor_for_person(&stranger));
        assert!(
            granted_view.can_see(&gated, &ResourceKind::Action),
            "granting Write must make the act visible"
        );
        assert!(
            !granted_view.can_see(&resource("actions", "peer.invite"), &ResourceKind::Action),
            "Write must not confer Invite"
        );
    }
}
