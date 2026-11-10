//! Inviting people into the workspace a page is in: the session's own role
//! ([`tilo_session::invite`]) and sheet, over the page. Who is in the workspace
//! is the membership the page follows, the parts this person may hand out are
//! their standing here, and the workspace's host makes, lists and cancels the
//! links, which open this page.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use tilo_desktop::here::Here;
use tilo_relay_protocol::workspaces::{
    PendingInvite, WorkspaceRole, WorkspacesReply, WorkspacesRequest,
};
use tilo_session::invite::{Answer, Invite, Issued, Refused, Workspace};
use tilo_sheets::Presentation;
use tilo_sheets::invite::{self as sheet, Choosing, InviteEffect};
use tilo_workspace_directory::Store;

use ui::LocalState;

use crate::over_page::OverSheet;
use crate::shell::{HostRequest, HostRequests};

/// How long a link the sheet makes works, as the session's desk makes one.
const LINK_SECONDS: u64 = 3_600;

/// What the page tells its desktop about invitations (`mountDesktop({invite})`).
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct PageInviteConfig {
    /// The page's entry (`https://tilo.sh/`): the link the sheet hands over opens it.
    #[serde(default)]
    pub base: String,
    /// The workspace's host, which the link names.
    #[serde(default)]
    pub origin: String,
    /// The workspace, by its uuid.
    #[serde(default)]
    pub workspace: String,
    /// How this person is named: the invitation says who made it.
    #[serde(default)]
    pub name: String,
    /// This person's root key, hex: the one member a link is never made for.
    #[serde(default)]
    pub user: String,
}

/// What the host answered, from its own thread.
type Reply = Result<WorkspacesReply, String>;

/// Who hears a host's answer.
type Then = Box<dyn FnOnce(Reply)>;

/// The sheet over the page, while it is up.
pub(crate) struct PageInvite {
    invite: Invite,
    workspace: Rc<PageWorkspace>,
    /// Where an invitation is left for the link that names it.
    store: Rc<Store>,
    presentation: Presentation,
    ask: sheet::Ask,
    asked: Rc<RefCell<Vec<InviteEffect>>>,
    choosing: LocalState<Choosing>,
    link: LocalState<String>,
    /// What the built sheet shows, and what it was built with, so it is built
    /// again only when one of them moved.
    built: Option<(sheet::Invite, u64, u64)>,
}

impl PageInvite {
    pub(crate) fn new(
        requests: HostRequests,
        config: Option<&PageInviteConfig>,
        shared: crate::desktop::Shared,
        here: Here,
        peer: Option<u32>,
        wake: &Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let store = Rc::new(Store::default());
        store.waking(Arc::clone(wake));
        let asked: Rc<RefCell<Vec<InviteEffect>>> = Rc::default();
        let pressing = Rc::clone(&asked);
        let ask: sheet::Ask = Rc::new(move |effect: InviteEffect| {
            pressing.borrow_mut().push(effect);
        });
        let workspace = Rc::new(PageWorkspace {
            config: config.cloned(),
            peer,
            person: config.and_then(|config| hex::decode(&config.user).ok()?.try_into().ok()),
            shared,
            here,
            pending: Rc::default(),
            replies: Arc::default(),
            waiting: RefCell::default(),
            asks: Cell::new(0),
        });
        Self {
            invite: Invite::new(
                Rc::clone(&workspace) as Rc<dyn Workspace>,
                Rc::new(PageClipboard(requests)),
                Rc::clone(&store),
            ),
            workspace,
            store,
            presentation: Presentation::default(),
            ask,
            asked,
            choosing: LocalState::new(Choosing::default()),
            link: LocalState::new(String::new()),
            built: None,
        }
    }

    /// Put the sheet up on the workspace this page is in.
    pub(crate) fn open(&self) {
        self.choosing.set(Choosing::default());
        self.link.set(String::new());
        self.invite.opening();
        self.presentation.open(false);
    }

    pub(crate) fn is_open(&self) -> bool {
        self.presentation.is_open()
    }

    /// Take what the host and the shelf answered, show the sheet as the
    /// workspace stands, and carry out what the person pressed since the last
    /// pass. Whether anything changed.
    fn follow(&mut self, show: &mut dyn FnMut(Option<ui::widget::Element>), missing: bool) -> bool {
        let mut changed = self.workspace.take_replies();
        changed |= self.store.answered();
        changed |= self.invite.follow_shelf();
        if !self.presentation.is_open() {
            show(None);
            return changed;
        }
        let asked: Vec<InviteEffect> = std::mem::take(&mut *self.asked.borrow_mut());
        if !asked.is_empty() {
            changed = true;
            if asked.contains(&InviteEffect::Done) {
                self.presentation.close();
            }
            sheet::answer(&asked, &self.invite);
        }
        if !self.presentation.is_open() {
            show(None);
            return changed;
        }
        let shown = sheet::view(&self.invite);
        let built = (
            shown.clone(),
            self.choosing.revision(),
            self.link.revision(),
        );
        if missing || self.built.as_ref() != Some(&built) {
            self.built = Some(built);
            show(Some(sheet::sheet(
                &shown,
                &self.choosing,
                &self.link,
                &self.ask,
                Some(&self.presentation),
            )));
            changed = true;
        }
        changed
    }
}

/// The page's clipboard: a link is written from the press that asked for it.
struct PageClipboard(HostRequests);

impl tilo_session::platform::clipboard::Clipboard for PageClipboard {
    fn copy(&self, text: String, _kept_for: std::time::Duration) {
        self.0.borrow_mut().push(HostRequest::Copy(text));
    }
}

/// The workspace this page is in, as its host answers for it.
struct PageWorkspace {
    config: Option<PageInviteConfig>,
    peer: Option<u32>,
    shared: crate::desktop::Shared,
    /// Who is here, as the workspace's presence names them.
    here: Here,
    /// The page's person, as they are here.
    person: Option<[u8; 32]>,
    /// The links that still work, as the host last answered.
    pending: Rc<RefCell<Vec<PendingInvite>>>,
    /// What the host answered, by the ask, from its own thread.
    replies: Arc<Mutex<Vec<(u64, Reply)>>>,
    /// Who hears each answer, by the ask.
    waiting: RefCell<BTreeMap<u64, Then>>,
    asks: Cell<u64>,
}

impl PageWorkspace {
    /// Ask this page's workspace host `request`; `then` hears its answer on a
    /// later pass, or why it was not asked.
    fn ask(
        &self,
        request: impl FnOnce(&str) -> WorkspacesRequest,
        then: impl FnOnce(Reply) + 'static,
    ) {
        let Some(peer) = self.peer else {
            return then(Err(NO_HOST.to_owned()));
        };
        let id = self.asks.replace(self.asks.get().wrapping_add(1));
        let (replies, woken) = (Arc::clone(&self.replies), Arc::clone(&self.shared.woken));
        let asked = crate::replication::host_then(
            peer,
            request,
            Box::new(move |reply| {
                replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((id, reply));
                woken.store(true, Ordering::Release);
            }),
        );
        match asked {
            Ok(()) => drop(self.waiting.borrow_mut().insert(id, Box::new(then))),
            Err(error) => then(Err(error)),
        }
    }

    /// Hand what the host answered since the last pass to whoever asked.
    /// Whether anything arrived.
    fn take_replies(&self) -> bool {
        let replies: Vec<(u64, Reply)> = std::mem::take(
            &mut *self
                .replies
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let arrived = !replies.is_empty();
        for (id, reply) in replies {
            let then = self.waiting.borrow_mut().remove(&id);
            if let Some(then) = then {
                then(reply);
            }
        }
        arrived
    }

    /// Keep the links that still work from the host's answer.
    fn keeping(&self) -> impl FnOnce(Reply) -> Result<(), Refused> + 'static {
        let pending = Rc::clone(&self.pending);
        move |reply| match reply {
            Ok(WorkspacesReply::Pending(listed)) => {
                *pending.borrow_mut() = listed;
                Ok(())
            }
            other => Err(Refused::Because(refused(other))),
        }
    }

    fn workspace(&self) -> Option<String> {
        self.config
            .as_ref()
            .map(|config| config.workspace.clone())
            .filter(|workspace| !workspace.is_empty())
    }
}

/// What the workspace said no to, and why.
fn refuse<T>(reason: &str) -> Result<T, Refused> {
    Err(Refused::Because(reason.to_owned()))
}

/// Why nothing was asked: a page with no workspace host makes no links.
const NO_HOST: &str = "this page is in no workspace to invite anyone into";

/// Why nothing was asked: a page that is in no workspace.
const NO_WORKSPACE: &str = "this page is in no workspace";

impl Workspace for PageWorkspace {
    fn may_invite(&self) -> bool {
        // Whether this person may invite here is the workspace's last answer, which
        // [`crate::replication`] asks with the rest of their standing.
        self.peer.is_some()
            && self.workspace().is_some()
            && self.shared.invitable.load(Ordering::Relaxed)
    }

    fn opened(&self, listed: Answer<()>) {
        self.pending.borrow_mut().clear();
        let Some(workspace) = self.workspace() else {
            return;
        };
        let keeping = self.keeping();
        self.ask(
            move |_| WorkspacesRequest::Pending { workspace },
            move |reply| listed(keeping(reply)),
        );
    }

    fn roles(&self) -> Vec<sheet::Role> {
        let ceiling = self.shared.standing.role().unwrap_or(Role::Editor);
        Role::ALL
            .into_iter()
            .filter(|role| *role != Role::Owner && ceiling.includes(*role))
            .map(|role| sheet::Role {
                key: role_id(role).to_owned(),
                label: role_label(role),
                brings: role_preview(role),
            })
            .collect()
    }

    fn people(&self) -> Vec<sheet::Person> {
        let Some(members) = self.shared.members.borrow().clone() else {
            return Vec::new();
        };
        let own = self.person.map(hex::encode);
        let here = self.here.borrow();
        members
            .into_iter()
            .filter(|member| Some(member) != own.as_ref())
            .map(|member| sheet::Person {
                name: here
                    .person(&member)
                    .map(|(name, _)| name)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| member.chars().take(8).collect()),
                key: member,
            })
            .collect()
    }

    fn pending(&self) -> Vec<sheet::Pending> {
        self.pending
            .borrow()
            .iter()
            .map(|invite| {
                sheet::pending(
                    &hex::encode(invite.invite_id),
                    &role_label(role_of(invite.role)),
                    invite.invitee_name.as_deref(),
                    invite.recipient.is_some(),
                    invite.expires_at_ns,
                )
            })
            .collect()
    }

    fn issue(&self, role: &str, recipient: &str, answer: Answer<Issued>) {
        let Some(asked) = role_from_id(role).and_then(workspace_role) else {
            return answer(refuse("that is not a part a link can bring someone in at"));
        };
        let Some(workspace) = self.workspace() else {
            return answer(refuse(NO_WORKSPACE));
        };
        let recipient = match recipient {
            "" => None,
            key => {
                let Some(key) = hex::decode(key).ok().and_then(|key| key.try_into().ok()) else {
                    return answer(refuse("that is not a person's key"));
                };
                Some(key)
            }
        };
        let mut request_id = [0u8; 32];
        if let Err(error) = getrandom::fill(&mut request_id) {
            return answer(refuse(&error.to_string()));
        }
        let expires_at_ns =
            ui::time::unix_now_ns().saturating_add(LINK_SECONDS.saturating_mul(1_000_000_000));
        let name = self
            .config
            .as_ref()
            .map(|config| config.name.trim().to_owned())
            .filter(|name| !name.is_empty());
        self.ask(
            move |_| WorkspacesRequest::Invite {
                workspace,
                request_id,
                role: asked,
                expires_at_ns,
                inviter_name: name,
                retired_wait_list_entry: None,
                recipient,
            },
            move |reply| {
                answer(match reply {
                    Ok(WorkspacesReply::Invitation {
                        invite_id,
                        signed_invitation,
                    }) => Ok(Issued {
                        invite: hex::encode(invite_id),
                        text: tilo_workspace_join::encode_invitation_text(&signed_invitation),
                        expires_at_ns,
                    }),
                    other => Err(Refused::Because(refused(other))),
                });
            },
        );
    }

    fn cancel(&self, invite: &str, answer: Answer<()>) {
        let Some(invite_id) = hex::decode(invite)
            .ok()
            .and_then(|id| <[u8; 32]>::try_from(id).ok())
        else {
            return answer(refuse("that is not an invitation"));
        };
        let Some(workspace) = self.workspace() else {
            return answer(refuse(NO_WORKSPACE));
        };
        let mut request_id = [0u8; 32];
        if let Err(error) = getrandom::fill(&mut request_id) {
            return answer(refuse(&error.to_string()));
        }
        let keeping = self.keeping();
        self.ask(
            move |_| WorkspacesRequest::Cancel {
                workspace,
                request_id,
                invite_id,
            },
            move |reply| answer(keeping(reply)),
        );
    }

    /// The site whose shelf keeps the invitation is the one this page comes
    /// from, so the link opens it; the link names the workspace's host.
    fn shelf(&self) -> (Option<url::Url>, Option<String>) {
        let Some(config) = self.config.as_ref() else {
            return (None, None);
        };
        let origin = config.base.trim().trim_end_matches('/');
        let site = tilo_workspace_directory::web_origin(&format!("{origin}/")).ok();
        (site, Some(config.origin.clone()))
    }
}

/// A part a link may bring someone in at, as the workspace's governance names
/// one.
pub(crate) type Role = tilo_action_policy::Role;

/// The role a link's part names, when it is one.
pub(crate) fn role_from_id(id: &str) -> Option<Role> {
    tilo_desktop_model::role_from_id(id)
}

/// What a role is called, as the workspace settings call it.
fn role_label(role: Role) -> String {
    tilo_desktop::workspace_settings::role_label(role)
}

/// What a role lets whoever joins with a link do here.
fn role_preview(role: Role) -> String {
    tilo_desktop::workspace_settings::role_capability_preview(role)
}

/// A role's id in the sheet and in the host's requests.
pub(crate) fn role_id(role: Role) -> &'static str {
    tilo_desktop_model::role_id(role)
}

/// The part a host's invitation grants, as the sheet names it.
pub(crate) fn role_of(role: WorkspaceRole) -> Role {
    match role {
        WorkspaceRole::Viewer => Role::Viewer,
        WorkspaceRole::Commenter => Role::Commenter,
        WorkspaceRole::Editor => Role::Editor,
        WorkspaceRole::Moderator => Role::Moderator,
    }
}

/// The role a host is asked for, when a link may grant it.
pub(crate) fn workspace_role(role: Role) -> Option<WorkspaceRole> {
    match role {
        Role::Viewer => Some(WorkspaceRole::Viewer),
        Role::Commenter => Some(WorkspaceRole::Commenter),
        Role::Editor => Some(WorkspaceRole::Editor),
        Role::Moderator => Some(WorkspaceRole::Moderator),
        Role::Owner => None,
    }
}

/// What the host refused, in its own words, or what it answered instead of
/// what was asked.
fn refused(reply: Result<WorkspacesReply, String>) -> String {
    match reply {
        Ok(WorkspacesReply::Refused(reason)) => reason,
        Ok(_) => "the workspace answered something else than what was asked".to_owned(),
        Err(error) => error,
    }
}

impl OverSheet for PageInvite {
    fn follow_sheet(
        &mut self,
        show: &mut dyn FnMut(Option<ui::widget::Element>),
        missing: bool,
    ) -> bool {
        self.follow(show, missing)
    }

    fn drawn(&self) -> bool {
        self.presentation.is_drawn()
    }

    fn sheet_name(&self) -> &'static str {
        "PageInvite"
    }
}
