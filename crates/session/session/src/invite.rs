//! Inviting people into the workspace in front: the sheet a link is made
//! from, the links that still work, and taking one back.
//!
//! The sheet shows and asks; the desk's own published actions make, list and
//! cancel the invitations, so a link the person hands over from here is the
//! same invitation an app or an agent makes, admitted by the workspace's
//! governance the same way. What is the session's here is saying whether the
//! person may invite at all, keeping the sheet current while it is up, and
//! putting the link on the clipboard long enough to hand it over.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tilo_desktop::workspace_settings as domain;
use tilo_desktop_model::role_from_id;
use tilo_desktop_surface::i18n::{t, t_args};
use tilo_sheets::Seat as _;
use tilo_sheets::invite::{self as sheet};
use tilo_surface_host::shell_part::DESK_RESOURCE;

use crate::Shared;
use crate::mount::clipboard::Ask;

/// How long a link the sheet copied stays on the clipboard: long enough to
/// paste it into a message to whoever it is for, and no longer, because the
/// link is what admits them.
const ON_CLIPBOARD_FOR: Duration = Duration::from_secs(300);

/// What the session keeps of the sheet between passes: the link it last
/// made, and what became of the last ask.
#[derive(Default)]
pub(crate) struct Made {
    created: Option<sheet::Created>,
    note: Option<sheet::Note>,
}

impl Shared {
    /// Open the invite sheet on the workspace in front.
    pub(crate) fn open_invite(&self) {
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        *self.invite_made.borrow_mut() = Made::default();
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
        let asked = seat.take_asked();
        if asked.is_empty() {
            return;
        }
        for effect in asked {
            match effect {
                sheet::InviteEffect::Create { role, recipient } => {
                    self.make_invite(&role, &recipient);
                }
                sheet::InviteEffect::Revoke { invite } => self.revoke_invite(&invite),
                sheet::InviteEffect::Copy { link } => {
                    self.clipboard.ask(Ask::Copy {
                        mime: tilo_compositor::clipboard_history::TEXT_MIME.to_owned(),
                        bytes: link.into_bytes(),
                        kept_for: ON_CLIPBOARD_FOR,
                    });
                    self.say(t("invite-copied"), false);
                }
                sheet::InviteEffect::ClearNote => self.invite_made.borrow_mut().note = None,
                // The sheet put itself away as it asked; the presentation
                // says so (`crate::sheets` sees it out).
                sheet::InviteEffect::Done => {}
            }
        }
        self.show_invite();
    }

    /// Show the sheet what the workspace stands as, for the person asking.
    fn show_invite(&self) {
        let Some(seat) = self.invite.as_ref() else {
            return;
        };
        seat.show(self.invite_sheet());
    }

    /// The sheet's data: the workspace's people and the parts this person may
    /// hand out, the links that still work, and this opening's link.
    fn invite_sheet(&self) -> sheet::Invite {
        let invited = self.invite_made.borrow();
        let may = self.may_invite_here();
        let mut data = sheet::Invite {
            title: t("invite-title"),
            description: t("invite-description"),
            close: t("invite-close"),
            may_invite: may,
            refusal: t("invite-refusal"),
            role_label: t("invite-role-label"),
            for_label: t("invite-for-label"),
            anyone: t("invite-anyone"),
            create: t("invite-create"),
            pending_title: t("invite-pending-title"),
            pending_empty: t("invite-pending-empty"),
            created: invited.created.clone(),
            note: invited.note.clone(),
            ..sheet::Invite::default()
        };
        if !may {
            return data;
        }
        let workspace = self
            .handle
            .read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
            .unwrap_or_default();
        let Some(engine) = self.engine.as_deref() else {
            return data;
        };
        let own = hex::encode(engine.identity().device_cert.user_public_key);
        data.roles = engine
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
            .collect();
        // Everyone this device knows by name, from every workspace it is
        // part of, the workspace in front first: a link can be made for
        // someone the person works with elsewhere too, and the same people
        // a send-to picker would offer.
        let mut seen = std::collections::HashSet::new();
        let mut people = Vec::new();
        let workspaces = std::iter::once(workspace.clone())
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
        data.people = people;
        let now = SystemTime::now();
        data.pending = engine
            .invitations(&workspace, now)
            .unwrap_or_default()
            .into_iter()
            .map(|invite| sheet::Pending {
                key: hex::encode(invite.invite_id),
                title: domain::role_label(invite.role.into()),
                whose: match (&invite.invitee_name, invite.recipient_user_public_key) {
                    (Some(name), _) => {
                        t_args("invite-for-person", &[("name", name.as_str().into())])
                    }
                    (None, Some(_)) => t("invite-for-someone"),
                    (None, None) => t("invite-for-anyone"),
                },
                lasts: lasts(invite.expires_at_ns),
                revoke: t("invite-revoke"),
            })
            .collect();
        data
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
            self.say(t("invite-no-answer"), true);
            return;
        };
        let value = match started {
            Ok(tilo_service::Started::Done(output)) => output.value,
            Ok(tilo_service::Started::Accepted) => {
                self.say(t("invite-no-answer"), true);
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
            self.say(t("invite-no-answer"), true);
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
            t_args("invite-created-anyone", &[("role", role.as_str().into())])
        } else {
            let name = self.name_of(recipient);
            let name = name.unwrap_or_else(|| recipient.chars().take(8).collect());
            t_args(
                "invite-created-for",
                &[("name", name.into()), ("role", role.as_str().into())],
            )
        };
        // The code the sheet draws is the invitation itself: its text spells
        // those bytes out in base64, which is longer than a code fits.
        let code = match tilo_workspace_join::invitation_bytes(link) {
            Ok(code) => code,
            Err(reason) => {
                tracing::warn!(%reason, "the invitation's text is not the invitation's bytes");
                self.refused("invite-failed", &reason);
                return;
            }
        };
        let mut made = self.invite_made.borrow_mut();
        made.created = Some(sheet::Created {
            invite,
            link: link.to_owned(),
            code,
            link_label: t("invite-link-label"),
            brings,
            lasts: t_args(
                "invite-created-lasts",
                &[("minutes", minutes_left(expires_at_ns).into())],
            ),
            copy: t("invite-copy"),
            revoke: t("invite-revoke"),
            alt: t("invite-code-alt"),
        });
        made.note = None;
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
                let mut made = self.invite_made.borrow_mut();
                // A link taken back is not a link to hand over: the one this
                // opening showed goes with it, and another stays.
                made.created = made
                    .created
                    .take()
                    .filter(|created| created.invite != invite);
                drop(made);
                self.say(t("invite-revoked"), false);
            }
            Some(Err(error)) => {
                tracing::warn!(%error, "the invitation was not cancelled");
                self.refused("invite-revoke-failed", &error.to_string());
            }
            None => self.say(t("invite-no-answer"), true),
        }
    }

    /// Say in the sheet what went wrong, and why.
    fn refused(&self, key: &str, reason: &str) {
        self.say(t_args(key, &[("reason", reason.into())]), true);
    }

    /// Say in the sheet what became of the last ask.
    fn say(&self, text: String, bad: bool) {
        self.invite_made.borrow_mut().note = Some(sheet::Note { text, bad });
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

/// How long a link has left, in the whole minutes the sheet says.
fn minutes_left(expires_at_ns: u64) -> usize {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let left = expires_at_ns.saturating_sub(now_ns);
    usize::try_from(left / 60_000_000_000).unwrap_or(usize::MAX)
}

/// What the sheet says of how long a link still works.
fn lasts(expires_at_ns: u64) -> String {
    t_args(
        "invite-lasts",
        &[("minutes", minutes_left(expires_at_ns).into())],
    )
}
