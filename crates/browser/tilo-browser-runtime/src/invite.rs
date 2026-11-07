//! Inviting people into the workspace a page is in: the session's own sheet
//! ([`tilo_sheets::invite`]), over the page, made, listed and taken back
//! through the workspace's host.
//!
//! Which parts this person may hand out is their role here; who is in the
//! workspace is the membership the page already follows (with the names the
//! here layer knows), and which links still work is the host's answer. A link
//! the sheet shows opens this page, naming the workspace and the host that keeps
//! it, and carries the handle and key of the invitation sealed on the host's
//! shelf beside it — the same short link the desk hands over, and the same
//! join every build takes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use tilo_desktop::here::Here;
use tilo_relay_protocol::workspaces::{
    PendingInvite, WorkspaceRole, WorkspacesReply, WorkspacesRequest,
};
use tilo_sheets::invite::{self as sheet, Choosing, Invitations, InviteEffect};
use tilo_sheets::{Presentation, word, words};
use tilo_workspace_directory::Store;

use ui::LocalState;

use crate::over_page::OverSheet;
use crate::shell::{HostRequest, HostRequests};

/// How long a link the sheet makes works, as the session's desk makes one.
const LINK_SECONDS: u64 = 3_600;

/// What the page tells its desktop about invitations
/// (`mountDesktop({invite})`): a page has no desk, no identity on disk and no
/// transport of its own to make a link through.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct PageInviteConfig {
    /// The page's entry, as a link names it (`https://tilo.coffee/`): the link
    /// the sheet hands over opens this page.
    #[serde(default)]
    pub base: String,
    /// The workspace's host: where the invitation waits on its shelf, and the
    /// workspace's own address in the link.
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

/// What the host answered since the last pass, from its own thread.
enum Answer {
    /// The invitations that still work.
    Pending(Result<WorkspacesReply, String>),
    /// An invitation was made.
    Made {
        brings: String,
        expires_at_ns: u64,
        reply: Result<WorkspacesReply, String>,
    },
    /// An invitation was taken back, and the ones that still work.
    Cancelled {
        invite: String,
        reply: Result<WorkspacesReply, String>,
    },
}

/// The sheet over the page, while it is up.
pub(crate) struct PageInvite {
    requests: HostRequests,
    /// Where an invitation is left for the link that names it.
    store: Store,
    presentation: Presentation,
    ask: sheet::Ask,
    config: Option<PageInviteConfig>,
    peer: Option<u32>,
    shared: crate::desktop::Shared,
    /// Who is here, as the workspace's presence names them.
    here: Here,
    choosing: LocalState<Choosing>,
    link: LocalState<String>,
    asked: Rc<RefCell<Vec<InviteEffect>>>,
    made: RefCell<sheet::Making>,
    pending: RefCell<Vec<PendingInvite>>,
    /// The page's person, as they are here.
    person: Option<[u8; 32]>,
    /// Whether this person may invite here, as the workspace last answered
    /// ([`crate::replication`] asks it with the rest of their standing).
    invitable: Arc<AtomicBool>,
    /// An invitation is being made: the sheet says so until the host answers.
    creating: Cell<bool>,
    /// The sheet is up and the host has not been asked what stands since.
    asking: Cell<bool>,
    /// What the built sheet shows, and what it was built with, so it is built
    /// again only when one of them moved.
    built: Option<(sheet::Invite, u64, u64)>,
    answers: Arc<Mutex<Vec<Answer>>>,
    woken: Arc<AtomicBool>,
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
        let store = Store::default();
        store.waking(Arc::clone(wake));
        let asked: Rc<RefCell<Vec<InviteEffect>>> = Rc::default();
        let pressing = Rc::clone(&asked);
        let ask: sheet::Ask = Rc::new(move |effect: InviteEffect| {
            pressing.borrow_mut().push(effect);
        });
        let woken = Arc::clone(&shared.woken);
        let invitable = Arc::clone(&shared.invitable);
        Self {
            requests,
            store,
            presentation: Presentation::default(),
            ask,
            config: config.cloned(),
            peer,
            shared,
            here,
            choosing: LocalState::new(Choosing::default()),
            link: LocalState::new(String::new()),
            asked,
            made: RefCell::new(sheet::Making::default()),
            pending: RefCell::new(Vec::new()),
            person: config.and_then(|config| hex::decode(&config.user).ok()?.try_into().ok()),
            invitable,
            creating: Cell::new(false),
            asking: Cell::new(false),
            built: None,
            answers: Arc::default(),
            woken,
        }
    }

    /// Put the sheet up on the workspace this page is in.
    pub(crate) fn open(&mut self) {
        self.choosing.set(Choosing::default());
        self.link.set(String::new());
        self.made.borrow_mut().opening();
        self.pending.borrow_mut().clear();
        self.creating.set(false);
        self.presentation.open(false);
        self.refresh();
    }

    pub(crate) fn is_open(&self) -> bool {
        self.presentation.is_open()
    }

    /// Take what the host and the shelf answered, show the sheet as the
    /// workspace stands, and carry out what the person pressed since the last
    /// pass. Whether anything changed.
    fn follow(&mut self, show: &mut dyn FnMut(Option<ui::widget::Element>), missing: bool) -> bool {
        let mut changed = self.take_answers();
        changed |= self.store.answered();
        changed |= self.store.follow_shelf(&mut self.made.borrow_mut());
        if !self.presentation.is_open() {
            self.close();
            show(None);
            return changed;
        }
        let asked: Vec<InviteEffect> = std::mem::take(&mut *self.asked.borrow_mut());
        if !asked.is_empty() {
            changed = true;
            if asked.contains(&InviteEffect::Done) {
                self.close();
            }
            sheet::answer(&asked, self);
        }
        if !self.presentation.is_open() {
            self.close();
            show(None);
            return changed;
        }
        let shown = sheet::view(self);
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

    /// Ask the host which links still work. Who is in the workspace is the
    /// membership this page already follows, not the host's own listing of
    /// every workspace it keeps.
    fn refresh(&self) {
        if self.asking.replace(true) {
            return;
        }
        let Some(workspace) = self.workspace() else {
            self.asking.set(false);
            return;
        };
        if let Err(error) = self.ask(Answer::Pending, move |_| WorkspacesRequest::Pending {
            workspace,
        }) {
            self.asking.set(false);
            self.refused("invite-failed", &error);
        }
    }

    /// Ask this page's workspace host `request`, and hand its answer to
    /// `answer`.
    fn ask(
        &self,
        answer: impl FnOnce(Result<WorkspacesReply, String>) -> Answer + Send + 'static,
        request: impl FnOnce(&str) -> WorkspacesRequest,
    ) -> Result<(), String> {
        let peer = self.peer.ok_or(NO_HOST)?;
        let answers = Arc::clone(&self.answers);
        let woken = Arc::clone(&self.woken);
        crate::replication::host_then(
            peer,
            request,
            Box::new(move |reply| {
                answers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(answer(reply));
                woken.store(true, Ordering::Release);
            }),
        )
    }

    /// Take what the host answered since the last pass. Whether anything
    /// arrived.
    fn take_answers(&self) -> bool {
        let answers: Vec<Answer> = std::mem::take(
            &mut *self
                .answers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let arrived = !answers.is_empty();
        for answer in answers {
            match answer {
                Answer::Pending(reply) => {
                    self.pending(reply);
                    self.asking.set(false);
                }
                Answer::Made {
                    brings,
                    expires_at_ns,
                    reply,
                } => {
                    self.creating.set(false);
                    self.made(brings, expires_at_ns, reply);
                }
                Answer::Cancelled { invite, reply } => {
                    self.pending(reply);
                    // The site's copy goes too: a link handed out before is
                    // not one that waits there to be fetched, and the one this
                    // opening showed goes with it.
                    self.store.take_back(&mut self.made.borrow_mut(), &invite);
                    self.say(word("invite-revoked"), false);
                }
            }
        }
        arrived
    }

    /// The links that still work, from the host's answer.
    fn pending(&self, reply: Result<WorkspacesReply, String>) {
        match reply {
            Ok(WorkspacesReply::Pending(pending)) => *self.pending.borrow_mut() = pending,
            other => self.refused("invite-failed", &refused(other)),
        }
    }

    /// The invitation the host made: its own bytes sealed onto the shelf of
    /// the site this page's workspace is hosted at, so the link carries a
    /// handle and a key instead of the invitation's kilobytes.
    fn made(&self, brings: String, expires_at_ns: u64, reply: Result<WorkspacesReply, String>) {
        let (invite, invitation) = match reply {
            Ok(WorkspacesReply::Invitation {
                invite_id,
                signed_invitation,
            }) => (hex::encode(invite_id), signed_invitation),
            other => {
                self.refused("invite-failed", &refused(other));
                return;
            }
        };
        // Until the shelf answers, the link is the invitation's own text, as
        // the desk hands it over: what the sheet shows when the site keeps
        // nothing.
        let long = tilo_workspace_join::encode_invitation_text(&invitation);
        let host = self.config.as_ref().map(|config| config.origin.as_str());
        let site = self.site();
        if let Err(reason) = self.store.hand_over(
            &mut self.made.borrow_mut(),
            &invite,
            &long,
            expires_at_ns,
            brings,
            site.as_ref(),
            host,
        ) {
            self.refused("invite-failed", &reason);
        }
    }

    /// Make a link, through the host that governs this workspace.
    fn create(&self, role: &str, recipient: &str) {
        let Some(role) = role_from_id(role).filter(|role| *role != Role::Owner) else {
            self.refused(
                "invite-failed",
                "that is not a part a link can bring someone in at",
            );
            return;
        };
        let Some(workspace) = self.workspace() else {
            self.refused("invite-failed", NO_WORKSPACE);
            return;
        };
        let recipient = match recipient {
            "" => None,
            key => {
                let Some(key) = hex::decode(key).ok().and_then(|key| key.try_into().ok()) else {
                    self.refused("invite-failed", "that is not a person's key");
                    return;
                };
                Some(key)
            }
        };
        let mut request_id = [0u8; 32];
        if let Err(error) = getrandom::fill(&mut request_id) {
            self.refused("invite-failed", &error.to_string());
            return;
        }
        let expires_at_ns =
            ui::time::unix_now_ns().saturating_add(LINK_SECONDS.saturating_mul(1_000_000_000));
        let label = role_label(role);
        let brings = match recipient {
            Some(key) => {
                let name = self
                    .name_of(key)
                    .unwrap_or_else(|| hex::encode(key).chars().take(8).collect());
                sheet::brings(&label, Some(&name))
            }
            None => sheet::brings(&label, None),
        };
        let name = self
            .config
            .as_ref()
            .map(|config| config.name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let Some(asked) = workspace_role(role) else {
            return;
        };
        self.creating.set(true);
        let sent = self.ask(
            move |reply| Answer::Made {
                brings,
                expires_at_ns,
                reply,
            },
            move |_| WorkspacesRequest::Invite {
                workspace: workspace.clone(),
                request_id,
                role: asked,
                expires_at_ns,
                inviter_name: name,
                wait_list_entry: None,
                recipient,
            },
        );
        if let Err(error) = sent {
            self.creating.set(false);
            self.refused("invite-failed", &error);
        }
    }

    /// Take a link back: the host stops redeeming it, and the shelf lets go of
    /// what it keeps.
    fn revoke(&self, invite: &str) {
        let Some(invite_id) = hex::decode(invite)
            .ok()
            .and_then(|id| <[u8; 32]>::try_from(id).ok())
        else {
            self.refused("invite-revoke-failed", "that is not an invitation");
            return;
        };
        let Some(workspace) = self.workspace() else {
            self.refused("invite-revoke-failed", NO_WORKSPACE);
            return;
        };
        let mut request_id = [0u8; 32];
        if let Err(error) = getrandom::fill(&mut request_id) {
            self.refused("invite-revoke-failed", &error.to_string());
            return;
        }
        let cancelled = invite.to_owned();
        let sent = self.ask(
            move |reply| Answer::Cancelled {
                invite: cancelled,
                reply,
            },
            move |_| WorkspacesRequest::Cancel {
                workspace: workspace.clone(),
                request_id,
                invite_id,
            },
        );
        if let Err(error) = sent {
            self.refused("invite-revoke-failed", &error);
        }
    }

    /// Put a link on the clipboard, through the page's own.
    fn copy(&self, link: &str) {
        self.requests
            .borrow_mut()
            .push(HostRequest::Copy(link.to_owned()));
        self.say(word("invite-copied"), false);
    }

    /// The name the workspace knows `person` by, when it knows one.
    fn name_of(&self, person: [u8; 32]) -> Option<String> {
        let user = hex::encode(person);
        self.here
            .borrow()
            .person(&user)
            .map(|(name, _)| name)
            .filter(|name| !name.is_empty())
    }

    /// This page's own person, hex, when the page knows them.
    fn own_user(&self) -> Option<String> {
        self.person.map(hex::encode)
    }

    /// Say in the sheet what went wrong, and why.
    fn refused(&self, key: &str, reason: &str) {
        self.say(words(key, &[("reason", reason)]), true);
    }

    /// Say in the sheet what became of the last ask.
    fn say(&self, text: String, bad: bool) {
        self.made.borrow_mut().say(text, bad);
    }

    fn workspace(&self) -> Option<String> {
        self.config
            .as_ref()
            .map(|config| config.workspace.clone())
            .filter(|workspace| !workspace.is_empty())
    }

    /// The site whose shelf keeps the invitation: the one this page comes
    /// from, so the link opens it.
    fn site(&self) -> Option<url::Url> {
        let config = self.config.as_ref()?;
        let origin = config.base.trim().trim_end_matches('/');
        tilo_workspace_directory::web_origin(&format!("{origin}/")).ok()
    }

    fn close(&mut self) {
        self.presentation.close();
    }
}

/// Why nothing was asked: a page with no workspace host makes no links.
const NO_HOST: &str = "this page is in no workspace to invite anyone into";

/// Why nothing was asked: a page that is in no workspace.
const NO_WORKSPACE: &str = "this page is in no workspace";

impl Invitations for PageInvite {
    fn may_invite(&self) -> bool {
        self.peer.is_some() && self.workspace().is_some() && self.invitable.load(Ordering::Relaxed)
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
        let own = self.own_user();
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

    fn made(&self) -> Option<sheet::Created> {
        self.made.borrow().made()
    }

    fn making(&self) -> bool {
        self.creating.get() || self.made.borrow().making()
    }

    fn note(&self) -> Option<sheet::Note> {
        self.made.borrow().note()
    }

    fn create(&self, role: &str, recipient: &str) {
        self.create(role, recipient);
    }

    fn revoke(&self, invite: &str) {
        self.revoke(invite);
    }

    fn copy(&self, link: &str) {
        self.copy(link);
    }

    fn clear_note(&self) {
        self.made.borrow_mut().clear();
    }
}

/// A part a link may bring someone in at, as the workspace's governance names
/// one.
type Role = tilo_action_policy::Role;

/// The role a link's part names, when it is one.
fn role_from_id(id: &str) -> Option<Role> {
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
fn role_id(role: Role) -> &'static str {
    tilo_desktop_model::role_id(role)
}

/// The part a host's invitation grants, as the sheet names it.
fn role_of(role: WorkspaceRole) -> Role {
    match role {
        WorkspaceRole::Viewer => Role::Viewer,
        WorkspaceRole::Commenter => Role::Commenter,
        WorkspaceRole::Editor => Role::Editor,
        WorkspaceRole::Moderator => Role::Moderator,
    }
}

/// The role a host is asked for, when a link may grant it.
fn workspace_role(role: Role) -> Option<WorkspaceRole> {
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
