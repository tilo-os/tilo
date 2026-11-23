//! The Linux shell's side of inviting: the workspace in front as this device holds it, its links
//! made and cancelled through the session's own published actions.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::remote::session_actions::{INVITE_RESOURCE, ISSUE_INVITATION};
use tilo_desktop::workspace_settings as domain;
use tilo_peer_state::join::Role;
use tilo_sheets::invite as sheet;

use crate::invite::{Answer, Issued, Refused, Workspace};

/// The workspace in front of this session, as this device holds it: its governance says who may
/// invite whom, and the session's own published actions make and cancel the links.
#[derive(Clone)]
pub(crate) struct InviteWorkspace {
    /// Where a link the device's signer signs is answered once it landed.
    pub(crate) landings: Rc<crate::landing::Landings>,
    pub(crate) handle: tilo_desktop_surface::DesktopHandle,
    pub(crate) person: String,
    pub(crate) sync: bool,
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    pub(crate) services: Rc<RefCell<tilo_service::Services>>,
    /// The links into the workspace in front that still work, as its holder last listed them.
    pub(crate) listed: Rc<RefCell<Vec<sheet::Pending>>>,
}

/// The links `invitations` names, as the holder lists them, but those for `own` person: an
/// invitation for the person themselves brings one of their own devices in, and the sheet lists
/// only what they gave others.
fn listed_links(invitations: &[serde_json::Value], own: Option<&str>) -> Vec<sheet::Pending> {
    invitations
        .iter()
        .filter(|invite| own.is_none() || invite["recipient"].as_str() != own)
        .filter_map(|invite| {
            // An administrator's invitation joins someone as an Editor and makes them one, so it
            // is listed by what it makes them.
            let label = if invite["administrator"].as_bool() == Some(true) {
                domain::administrator_label()
            } else {
                domain::role_label(Role::parse(invite["role"].as_str()?)?)
            };
            Some(sheet::pending(
                invite["invite"].as_str()?,
                &label,
                invite["invitee_name"].as_str(),
                invite["recipient"].is_string(),
                invite["expires_at_ns"].as_u64().unwrap_or_default(),
            ))
        })
        .collect()
}

impl InviteWorkspace {
    /// The workspace in front, by its id.
    fn in_front(&self) -> Option<String> {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    }

    /// Ask the workspace's holder for the links into the workspace in front that still work, keep
    /// them for the sheet, and tell `listed` whether it answered.
    fn list(&self, listed: Answer<()>) {
        let (kept, own) = (Rc::clone(&self.listed), self.own_person());
        self.invoke("invitations", &serde_json::Map::new(), move |value| {
            listed(value.map(|value| {
                *kept.borrow_mut() = value
                    .as_ref()
                    .and_then(serde_json::Value::as_array)
                    .map(|invitations| listed_links(invitations, own.as_deref()))
                    .unwrap_or_default();
            }));
        });
    }

    /// What asks the list again once a link was made or taken back.
    fn relist(&self) -> impl FnOnce() + 'static {
        let this = self.clone();
        move || this.list(Box::new(drop))
    }

    /// This device's person, by their key in hex.
    fn own_person(&self) -> Option<String> {
        self.engine
            .as_deref()
            .map(|engine| hex::encode(engine.identity().device_cert.user_public_key))
    }

    /// Ask the session's own action `action` with `arguments`, and hand what it answered to
    /// `answer`: at once, or on the pass after a link the device's signer signs landed.
    fn invoke(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        answer: impl FnOnce(Result<Option<serde_json::Value>, Refused>) + 'static,
    ) {
        let refused = move |error: tilo_actions::ApiError| {
            tracing::warn!(%error, "the session refused");
            Refused::Because(error.to_string())
        };
        match self
            .services
            .borrow_mut()
            .invoke(INVITE_RESOURCE, action, arguments)
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

impl Workspace for InviteWorkspace {
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
                .map(|workspace| tilo_session_core::desktop::names::workspace(&workspace.name))
        })
    }

    fn roles(&self) -> Vec<sheet::Role> {
        let Some(engine) = self.engine.as_deref() else {
            return Vec::new();
        };
        let workspace = self.in_front().unwrap_or_default();
        let mut roles: Vec<sheet::Role> = engine
            .invitable_roles(&workspace)
            .into_iter()
            .map(|role| sheet::Role {
                key: role.key().to_owned(),
                label: domain::role_label(role),
                brings: domain::role_capability_preview(role),
            })
            .collect();
        // An administrator is an Editor with the administrator grants: a
        // choice of its own on top of the roles, offered to whoever may make
        // administrators.
        if engine.may_invite_administrator(&workspace) {
            roles.push(sheet::Role {
                key: "administrator".to_owned(),
                label: domain::administrator_label(),
                brings: domain::administrator_capability_preview(),
            });
        }
        roles
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
                let Some(person) = member.id.person_id().map(str::to_owned) else {
                    continue;
                };
                if member.removed || person == own || !seen.insert(person.clone()) {
                    continue;
                }
                people.push(sheet::Person {
                    name: if member.name.is_empty() {
                        ui::presence::unnamed_person_label()
                    } else {
                        member.name
                    },
                    key: person,
                });
            }
        }
        people
    }

    /// The sheet went up: the links that still work are asked of the workspace's holder again.
    fn opened(&self, listed: Answer<()>) {
        self.list(listed);
    }

    fn pending(&self) -> Vec<sheet::Pending> {
        self.listed.borrow().clone()
    }

    fn issue(&self, role: &str, recipient: &str, answer: Answer<Issued>) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("role".to_owned(), serde_json::Value::from(role));
        if !recipient.is_empty() {
            arguments.insert("recipient".to_owned(), serde_json::Value::from(recipient));
        }
        let listing = self.relist();
        self.invoke(ISSUE_INVITATION, &arguments, move |value| {
            listing();
            answer(value.and_then(|value| {
                let field = |name: &str| value.as_ref().and_then(|value| value.get(name));
                let link = field("link").and_then(serde_json::Value::as_str);
                let text = field("invitation")
                    .and_then(serde_json::Value::as_str)
                    .or(link);
                Ok(Issued {
                    text: text.ok_or(Refused::Unanswered)?.to_owned(),
                    link: link.map(str::to_owned),
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
        let listing = self.relist();
        self.invoke("cancel-invitation", &arguments, move |value| {
            listing();
            answer(value.map(drop));
        });
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
