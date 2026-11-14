//! The Linux desk's side of inviting: the workspace in front as this device holds it, its links
//! made and cancelled through the desk's own published actions.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use tilo_actions::remote::session_actions::ISSUE_INVITATION;
use tilo_desktop::workspace_settings as domain;
use tilo_session_core::shell::DESK_RESOURCE;
use tilo_sheets::invite as sheet;

use crate::invite::{Answer, Issued, Refused, Workspace};

/// The workspace in front of this desk, as this device holds it: its governance says who may
/// invite whom, and the desk's own published actions make and cancel the links.
pub(crate) struct DeskWorkspace {
    /// Where a link the device's signer signs is answered once it landed.
    pub(crate) landings: Rc<crate::landing::Landings>,
    pub(crate) handle: tilo_desktop_surface::DesktopHandle,
    pub(crate) person: String,
    pub(crate) sync: bool,
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    pub(crate) services: Rc<RefCell<tilo_service::Services>>,
    /// The community site this computer is configured with, read when a link is made.
    pub(crate) site: fn() -> Option<url::Url>,
}

impl DeskWorkspace {
    /// The workspace in front, by its id.
    fn in_front(&self) -> Option<String> {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    }

    /// Ask the desk's own action `action` with `arguments`, and hand what it answered to
    /// `answer`: at once, or on the pass after a link the device's signer signs landed.
    fn invoke(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        answer: impl FnOnce(Result<Option<serde_json::Value>, Refused>) + 'static,
    ) {
        let refused = move |error: tilo_actions::DoorError| {
            tracing::warn!(%error, "the desk refused");
            Refused::Because(error.to_string())
        };
        match self
            .services
            .borrow_mut()
            .invoke(DESK_RESOURCE, action, arguments)
        {
            Some(Ok(tilo_service::Started::Done(output))) => answer(Ok(output.value)),
            Some(Ok(tilo_service::Started::Later(later))) => {
                self.landings.then(later, move |landed| {
                    answer(match landed {
                        Some(Ok(output)) => Ok(output.value),
                        Some(Err(error)) => Err(refused(error)),
                        None => Err(Refused::Unanswered),
                    });
                });
            }
            Some(Ok(tilo_service::Started::Accepted)) | None => answer(Err(Refused::Unanswered)),
            Some(Err(error)) => answer(Err(refused(error))),
        }
    }
}

impl Workspace for DeskWorkspace {
    /// A workspace this device holds, whose own governance lets the person invite into it, with a
    /// transport a link can name. The write path asks the workspace's record again, so this only
    /// decides what is offered.
    fn may_invite(&self) -> bool {
        self.sync
            && self.in_front().is_some_and(|workspace| {
                may_invite(self.engine.as_deref(), &workspace, &self.person)
            })
    }

    fn name(&self) -> Option<String> {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| tilo_desktop_surface::names::workspace(&workspace.name))
        })
    }

    fn roles(&self) -> Vec<sheet::Role> {
        let Some(engine) = self.engine.as_deref() else {
            return Vec::new();
        };
        engine
            .invitable_roles(&self.in_front().unwrap_or_default())
            .into_iter()
            .map(|role| sheet::Role {
                key: role.key().to_owned(),
                label: domain::role_label(role),
                brings: domain::role_capability_preview(role),
            })
            .collect()
    }

    fn people(&self) -> Vec<sheet::Person> {
        let (Some(engine), Some(workspace)) = (self.engine.as_deref(), self.in_front()) else {
            return Vec::new();
        };
        let own = hex::encode(engine.identity().device_cert.user_public_key);
        // Everyone this device knows by name, from every workspace it is part of, the workspace in
        // front first: a link can be made for someone the person works with elsewhere too, and the
        // same people a send-to picker would offer.
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
                        sheet::unnamed_person()
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
        let (Some(engine), Some(workspace)) = (self.engine.as_deref(), self.in_front()) else {
            return Vec::new();
        };
        // An invitation for the person themselves is no link they handed
        // out: it brings one of their own devices in, and the sheet lists
        // only what they gave others.
        let own = engine.identity().device_cert.user_public_key;
        engine
            .invitations(&workspace, SystemTime::now())
            .unwrap_or_default()
            .into_iter()
            .filter(|invite| invite.recipient_user_public_key != Some(own))
            .map(|invite| {
                sheet::pending(
                    &hex::encode(invite.invite_id),
                    &domain::role_label(invite.role),
                    invite.invitee_name.as_deref(),
                    invite.recipient_user_public_key.is_some(),
                    invite.expires_at_ns,
                )
            })
            .collect()
    }

    fn issue(&self, role: &str, recipient: &str, answer: Answer<Issued>) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("role".to_owned(), serde_json::Value::from(role));
        if !recipient.is_empty() {
            arguments.insert("recipient".to_owned(), serde_json::Value::from(recipient));
        }
        self.invoke(ISSUE_INVITATION, &arguments, move |value| {
            answer(value.and_then(|value| {
                let field = |name: &str| value.as_ref().and_then(|value| value.get(name));
                let text = field("link").and_then(serde_json::Value::as_str);
                Ok(Issued {
                    text: text.ok_or(Refused::Unanswered)?.to_owned(),
                    invite: field("invite")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    expires_at_ns: field("expires_at_ns")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default(),
                })
            }));
        });
    }

    fn cancel(&self, invite: &str, answer: Answer<()>) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("invite".to_owned(), serde_json::Value::from(invite));
        self.invoke("cancel-invitation", &arguments, move |value| {
            answer(value.map(drop));
        });
    }

    fn shelf(&self) -> (Option<url::Url>, Option<String>) {
        ((self.site)(), None)
    }
}

/// Whether `person` may invite someone into `workspace` from this device, as the workspace's own
/// governance answers it: a current member whose part there grants inviting, on the device that
/// signs the link. What the surfaces offering inviting are told; the write path asks again.
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
