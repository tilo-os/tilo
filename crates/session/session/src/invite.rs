//! Inviting people into the workspace in front: the sheet a link is made
//! from, the links that still work, and taking one back.
//!
//! The sheet shows and asks; the desk's own published actions make, list and
//! cancel the invitations, so a link the person hands over from here is the
//! same invitation an app or an agent makes, admitted by the workspace's
//! governance the same way. What is the session's here is saying whether the
//! person may invite at all, keeping the sheet current while it is up, and
//! putting the link on the clipboard long enough to hand it over.

use std::time::{Duration, SystemTime};

use tilo_desktop::workspace_settings as domain;
use tilo_desktop_model::role_from_id;
use tilo_sheets::Seat as _;
use tilo_sheets::invite as sheet;
use tilo_sheets::{word, words};
use tilo_surface_host::shell_part::DESK_RESOURCE;

use crate::Shared;
use crate::mount::clipboard::Ask;

/// How long a link the sheet copied stays on the clipboard: long enough to
/// paste it into a message to whoever it is for, and no longer, because the
/// link is what admits them.
const ON_CLIPBOARD_FOR: Duration = Duration::from_secs(300);

impl Shared {
    /// Open the invite sheet on the workspace in front.
    pub(crate) fn open_invite(&self) {
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        self.invite_made.borrow_mut().opening();
        self.show_invite();
        seat.presentation().open(false);
    }

    /// Whether the person may invite someone into the workspace in front: a
    /// workspace this device holds, whose own governance lets them invite into
    /// it, with a transport a link can name. The write path asks the
    /// workspace's record again, so this only decides what is offered.
    pub(crate) fn may_invite_here(&self) -> bool {
        self.sync.is_some()
            && self
                .handle
                .read(|tree| {
                    tree.active_workspace()
                        .map(|workspace| workspace.id().to_string())
                })
                .is_some_and(|workspace| {
                    may_invite(self.engine.as_deref(), &workspace, &self.person)
                })
    }

    /// Answer for the surfaces that offer inviting whether the person may,
    /// every pass: the bar keeps its invite button, and the roster its footer.
    pub(crate) fn follow_invitable(&self) {
        self.invitable.set(self.may_invite_here());
    }

    /// Keep the sheet current while it is up: who is in the workspace, the
    /// parts this person may hand out and the links that still work all move
    /// under it.
    pub(crate) fn refresh_invite(&self) {
        self.directory_store
            .follow_shelf(&mut self.invite_made.borrow_mut());
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        if !seat.presentation().is_open() {
            return;
        }
        self.show_invite();
    }

    /// Carry out what the person asked for in the sheet, and show what came of
    /// it.
    pub(crate) fn follow_invite(&self) {
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        if sheet::drive(seat, &DeskInvites(self)) == 0 {
            return;
        }
        self.show_invite();
    }

    /// Show the sheet what the workspace stands as, for the person asking.
    fn show_invite(&self) {
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        seat.show(sheet::view(&DeskInvites(self)));
    }

    /// Make a link into the workspace in front, through the desk's own action:
    /// the part it brings someone in at, and, for a person the workspace
    /// knows, a link only their device can redeem.
    fn make_invite(&self, role: &str, recipient: &str) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("role".to_owned(), serde_json::Value::from(role));
        if !recipient.is_empty() {
            arguments.insert("recipient".to_owned(), serde_json::Value::from(recipient));
        }
        let Some(started) =
            self.services
                .borrow_mut()
                .invoke(DESK_RESOURCE, "issue-invitation", &arguments)
        else {
            self.say(word("invite-no-answer"), true);
            return;
        };
        let value = match started {
            Ok(tilo_service::Started::Done(output)) => output.value,
            Ok(tilo_service::Started::Accepted) => {
                self.say(word("invite-no-answer"), true);
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "the invitation was not made");
                self.refused("invite-failed", &error.to_string());
                return;
            }
        };
        let Some(link) = value
            .as_ref()
            .and_then(|value| value.get("link"))
            .and_then(serde_json::Value::as_str)
        else {
            self.say(word("invite-no-answer"), true);
            return;
        };
        let invite = value
            .as_ref()
            .and_then(|value| value.get("invite"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let expires_at_ns = value
            .as_ref()
            .and_then(|value| value.get("expires_at_ns"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let role = role_from_id(role).map_or_else(|| role.to_owned(), domain::role_label);
        let brings = if recipient.is_empty() {
            sheet::brings(&role, None)
        } else {
            let name = self.name_of(recipient);
            let name = name.unwrap_or_else(|| recipient.chars().take(8).collect());
            sheet::brings(&role, Some(&name))
        };
        // The person hands over a link, not an invitation's kilobytes: the
        // signed bytes are sealed under a fresh key and wait on the site this
        // computer is configured with, under a handle, and the link carries
        // the key in its fragment. A computer with no site, or one that will
        // not keep it, hands over the invitation's own text instead.
        if let Err(reason) = self.directory_store.hand_over(
            &mut self.invite_made.borrow_mut(),
            &invite,
            link,
            expires_at_ns,
            brings,
            crate::directory::community_site().as_ref(),
            None,
        ) {
            tracing::warn!(%reason, "the invitation's text is not the invitation's bytes");
            self.refused("invite-failed", &reason);
        }
    }

    /// The name the workspace in front knows `person` by, when it knows one.
    fn name_of(&self, person: &str) -> Option<String> {
        let workspace = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })?;
        let key = recipient_key(person)?;
        self.engine.as_deref()?.name_of(&workspace, key)
    }

    /// Take back a link into the workspace in front, through the desk's own
    /// action: whoever holds it admits nobody from now on.
    fn revoke_invite(&self, invite: &str) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("invite".to_owned(), serde_json::Value::from(invite));
        match self
            .services
            .borrow_mut()
            .invoke(DESK_RESOURCE, "cancel-invitation", &arguments)
        {
            Some(Ok(_)) => {
                // The site's copy goes too: a link handed out before is not
                // one that waits there to be fetched, and the one this
                // opening showed goes with it.
                self.directory_store
                    .take_back(&mut self.invite_made.borrow_mut(), invite);
                self.say(word("invite-revoked"), false);
            }
            Some(Err(error)) => {
                tracing::warn!(%error, "the invitation was not cancelled");
                self.refused("invite-revoke-failed", &error.to_string());
            }
            None => self.say(word("invite-no-answer"), true),
        }
    }

    /// Say in the sheet what went wrong, and why.
    fn refused(&self, key: &str, reason: &str) {
        self.say(words(key, &[("reason", reason)]), true);
    }

    /// Say in the sheet what became of the last ask.
    fn say(&self, text: String, bad: bool) {
        self.invite_made.borrow_mut().say(text, bad);
    }
}

/// The session's side of the invite sheet: the workspace in front's people,
/// the parts this person may hand out, the links that still work, and the
/// presses carried out through the desk's own actions.
struct DeskInvites<'a>(&'a Shared);

impl sheet::Invitations for DeskInvites<'_> {
    fn may_invite(&self) -> bool {
        self.0.may_invite_here()
    }

    fn roles(&self) -> Vec<sheet::Role> {
        let Some(engine) = self.0.engine.as_deref() else {
            return Vec::new();
        };
        let workspace = self
            .0
            .handle
            .read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
            .unwrap_or_default();
        engine
            .invitable_roles(&workspace)
            .into_iter()
            .map(|role| {
                let role = role.into();
                sheet::Role {
                    key: tilo_desktop_model::role_id(role).to_owned(),
                    label: domain::role_label(role),
                    brings: domain::role_capability_preview(role),
                }
            })
            .collect()
    }

    fn people(&self) -> Vec<sheet::Person> {
        let Some(engine) = self.0.engine.as_deref() else {
            return Vec::new();
        };
        let Some(workspace) = self.0.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        }) else {
            return Vec::new();
        };
        let own = hex::encode(engine.identity().device_cert.user_public_key);
        // Everyone this device knows by name, from every workspace it is
        // part of, the workspace in front first: a link can be made for
        // someone the person works with elsewhere too, and the same people
        // a send-to picker would offer.
        let mut seen = std::collections::HashSet::new();
        let mut people = Vec::new();
        let workspaces = std::iter::once(workspace)
            .chain(engine.workspaces().into_iter().map(|held| held.to_string()));
        for held in workspaces {
            for member in engine.people(&held).unwrap_or_default() {
                if member.removed || member.person == own || !seen.insert(member.person.clone()) {
                    continue;
                }
                people.push(sheet::Person {
                    name: if member.name.is_empty() {
                        member.person.chars().take(8).collect()
                    } else {
                        member.name
                    },
                    key: member.person,
                });
            }
        }
        people
    }

    fn pending(&self) -> Vec<sheet::Pending> {
        let Some(engine) = self.0.engine.as_deref() else {
            return Vec::new();
        };
        let Some(workspace) = self.0.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        }) else {
            return Vec::new();
        };
        engine
            .invitations(&workspace, SystemTime::now())
            .unwrap_or_default()
            .into_iter()
            .map(|invite| {
                sheet::pending(
                    &hex::encode(invite.invite_id),
                    &domain::role_label(invite.role.into()),
                    invite.invitee_name.as_deref(),
                    invite.recipient_user_public_key.is_some(),
                    invite.expires_at_ns,
                )
            })
            .collect()
    }

    fn made(&self) -> Option<sheet::Created> {
        self.0.invite_made.borrow().made()
    }

    fn making(&self) -> bool {
        self.0.invite_made.borrow().making()
    }

    fn note(&self) -> Option<sheet::Note> {
        self.0.invite_made.borrow().note()
    }

    fn create(&self, role: &str, recipient: &str) {
        self.0.make_invite(role, recipient);
    }

    fn revoke(&self, invite: &str) {
        self.0.revoke_invite(invite);
    }

    fn copy(&self, link: &str) {
        self.0.clipboard.ask(Ask::Copy {
            mime: tilo_compositor::clipboard_history::TEXT_MIME.to_owned(),
            bytes: link.as_bytes().to_vec(),
            kept_for: ON_CLIPBOARD_FOR,
        });
        self.0.say(word("invite-copied"), false);
    }

    fn clear_note(&self) {
        self.0.invite_made.borrow_mut().clear();
    }
}

/// Whether `person` may invite someone into `workspace` from this device, as
/// the workspace's own governance answers it: a current member whose part
/// there grants inviting, on the device that signs the link. What the
/// surfaces offering inviting are told; the write path asks again.
pub(crate) fn may_invite(
    engine: Option<&crate::engine::Engine>,
    workspace: &str,
    person: &str,
) -> bool {
    engine.is_some_and(|engine| {
        hex::encode(engine.identity().device_cert.user_public_key) == person
            && engine.may_invite(workspace)
    })
}

/// The person a link names, by their key in hex.
fn recipient_key(recipient: &str) -> Option<[u8; 32]> {
    let key = hex::decode(recipient).ok()?;
    <[u8; 32]>::try_from(key).ok()
}
