//! The capability gate for the headless action API.
//!
//! [`ActionCatalog`](super::action_api::ActionCatalog) takes an
//! [`ActionVisibility`] and applies it at build time, so a resource the
//! gate hides is absent from enumeration *and* selection — you cannot
//! invoke what you were never shown. Until now the only implementation
//! was `AllVisible`, which is the honest default for a local human
//! sitting at the machine but says nothing about a programmatic caller.
//!
//! This module supplies the real one: it maps an exported search
//! resource to the capability invoking it would require, then asks the
//! workspace's own capability model.
//!
//! ## What is gated, and what is not
//!
//! The rule comes from [`crate::shell::actions`] and is not re-decided
//! here:
//!
//! > Showing a resource is non-destructive and needs no permission.
//! > Taking an action on one requires permission.
//!
//! So notes, records, files and folders are **never** hidden. A peer who
//! may see a document is still shown it; what disappears for a caller
//! who cannot act is the *affordance*, not the content. Concretely:
//! opening an overlay is showing, and stays visible to everyone;
//! spawning a terminal, duplicating a tab, launching an app or minting
//! an invite are acts, and are gated.
//!
//! Anything this module cannot classify is treated as an act requiring
//! [`Verb::Write`] rather than waved through. A new provider that lands
//! without a mapping should become invisible to a restricted caller, not
//! silently reachable by one.

use crate::shell::state::{Actor, ResourceRef as CapResource, Verb, Workspace};

use super::ResourceKind;
use super::action_api::{ActionVisibility, ResourceRef};

/// The app id chrome-level actions are attributed to in the capability
/// model. They are not exported by any packaged app — they are the
/// shell's own — so they need a stable name to be addressed by.
pub const SHELL_APP_ID: &str = "shell";

/// What invoking `resource` requires, or `None` when invoking it only
/// shows something and so needs no capability.
///
/// Split out from the [`ActionVisibility`] impl so the policy can be
/// read, tested and argued about without a live workspace in hand.
pub fn required_capability(
    resource: &ResourceRef,
    kind: &ResourceKind,
) -> Option<(CapResource, Verb)> {
    match kind {
        // Showing is free. This is the larger half of the surface —
        // every note, file, folder and record a caller can already see.
        ResourceKind::Note
        | ResourceKind::Record
        | ResourceKind::File
        | ResourceKind::Folder
        | ResourceKind::Creator
        | ResourceKind::Video
        | ResourceKind::Playlist => None,

        // Launching an app puts a tab in the workspace, which is a
        // mutation of the room's contents, so it needs `Write` on the
        // app being launched.
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

        // A provider minting its own kind has not been classified here.
        // Treat it as an act: an unmapped action must not be reachable
        // by a caller who would fail every mapped one.
        ResourceKind::Custom(_) => Some((
            CapResource::Action {
                app: SHELL_APP_ID.to_string(),
                action: resource.id.clone(),
            },
            Verb::Write,
        )),
    }
}

/// The verb one of the shell's own chrome actions requires, or `None`
/// when it only opens a view.
///
/// The overlay-opening entries are deliberately ungated: `settings.open`
/// reveals a panel, it does not change a setting. The action that
/// changes something is the one inside the panel, and that is gated
/// where it lives.
fn chrome_action_verb(id: &str) -> Option<Verb> {
    match id {
        // Showing: reveals a view, mutates nothing.
        "overview.toggle"
        | "settings.open"
        | "quick-settings.open"
        | "collaborators.open"
        | "room.activity.open" => None,

        // Membership: admitting a peer, joining someone else's room, or
        // admitting an agent. The third is the same act as the first —
        // it decides who is in this room — so it is gated on the same
        // verb. That an agent is not a person changes what the row
        // writes, not who is allowed to write it.
        "peer.invite" | "peer.invite.named" | "peer.connect" | "agent.invite" => {
            Some(Verb::Invite)
        }

        // Acts on the room's contents.
        "tab.new.terminal.here" | "window.new.terminal" | "tab.duplicate" => Some(Verb::Write),

        // Bringing a record into the room changes what the room holds,
        // and what a room holds is what its members can reach — so this
        // is `Write` for the same reason spawning a tab is, and it is
        // classified here rather than left to the fail-closed default so
        // the choice is a choice. Note the asymmetry the rest of this
        // module states: *seeing* which records a room holds is showing
        // and is never gated; putting one there is an act.
        "record.bring" => Some(Verb::Write),

        // Writing into a document is the plainest case of the rule this
        // module states: reading the note is showing and is free, and
        // changing what it says is an act. It is `Write` on the shell's
        // own action namespace for the same reason spawning a tab is —
        // the actor is editing the room's contents.
        "text.insert" => Some(Verb::Write),

        // Changes a session-wide setting.
        "notifications.dnd.toggle" => Some(Verb::Write),

        // Unknown chrome action — gate it. See the module note on why
        // the unclassified case fails closed.
        _ => Some(Verb::Write),
    }
}

/// An [`ActionVisibility`] backed by a real workspace's capability model.
///
/// Fail-closed by construction: [`Workspace::allows`] is
/// `collab_types::decide_capability`, which denies unless something
/// explicitly permits.
pub struct CapabilityVisibility<'a> {
    workspace: &'a Workspace,
    actor: Actor,
}

impl<'a> CapabilityVisibility<'a> {
    /// Gate against `workspace` on behalf of the person `actor`.
    pub fn new(workspace: &'a Workspace, actor: Actor) -> Self {
        Self { workspace, actor }
    }
}

impl ActionVisibility for CapabilityVisibility<'_> {
    fn can_see(&self, resource: &ResourceRef, kind: &ResourceKind) -> bool {
        match required_capability(resource, kind) {
            None => true,
            Some((cap_resource, verb)) => {
                self.workspace.allows(&self.actor, &cap_resource, verb)
            }
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

    /// Showing is free: a document-shaped resource is never gated, so a
    /// caller with no capabilities at all still sees the content it is
    /// entitled to see.
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

    /// Opening an overlay reveals a view and changes nothing, so it is
    /// not an act. The setting *inside* the panel is gated where it
    /// lives, not at the door.
    #[test]
    fn opening_an_overlay_is_showing_not_acting() {
        for id in [
            "overview.toggle",
            "settings.open",
            "quick-settings.open",
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

    /// Acts on the room's contents need `Write`; membership operations
    /// need `Invite`. These are the entries that actually disappear for
    /// a restricted caller.
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

        for id in ["peer.invite", "peer.invite.named", "peer.connect", "agent.invite"] {
            let (_, verb) = required_capability(&resource("actions", id), &ResourceKind::Action)
                .unwrap_or_else(|| panic!("{id} must be gated"));
            assert_eq!(verb, Verb::Invite, "{id}");
        }
    }

    /// Editing a document is an act, so it is gated — and gated on
    /// `Write`, the same verb spawning a tab needs. The rule this module
    /// states does not bend for the fact that a document looks like
    /// content: *reading* it is showing, *changing* it is acting.
    #[test]
    fn writing_into_a_document_requires_write() {
        let (cap, verb) = required_capability(
            &resource("actions", "text.insert"),
            &ResourceKind::Action,
        )
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

    /// …and the gate actually hides it from someone holding nothing,
    /// while still showing them the document itself. Asserted against a
    /// real `Workspace`, because this is the property `actions_invoke`
    /// relies on to refuse an unauthorised edit: a hidden action is
    /// absent from the catalogue, so `select` finds nothing and the
    /// invocation is refused rather than performed.
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

    /// An action nobody classified must not be reachable by a caller who
    /// would fail every classified one. A new provider landing without a
    /// mapping should go dark for restricted callers, not open.
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

    /// The end-to-end shape: a workspace owner sees the gated actions,
    /// and a member holding nothing does not — while both still see the
    /// ungated ones. This is the property the action API relies on, so
    /// it is asserted against a real `Workspace`, not a stub.
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
            !granted_view.can_see(
                &resource("actions", "peer.invite"),
                &ResourceKind::Action
            ),
            "Write must not confer Invite"
        );
    }
}
