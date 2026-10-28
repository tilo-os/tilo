//! What a room's people change about the room itself, applied by its
//! governor: its name, its saved grant sets and their assignments, the
//! actions its apps may use of each other, its policy grants and its apps'
//! sandbox capabilities.
//!
//! A member's device asks; the governor decides with the asker's own
//! standing in the room and writes the result into the room's governance,
//! which every member reads. The request of the person's own device is
//! their confirmation, and nothing here acts for anyone but the asker:
//! authority follows authorship. The rules are the ones a workspace has
//! always applied to these changes ([`tilo_desktop_model`]'s policy and
//! grant sets), now decided where the room is governed.

use collab_types::caps::{CapOverride, Principal};
use tilo_action_policy::roles::{self, governance};
use tilo_action_policy::{
    Answer, Context, Grant, GrantId, KindSet, Lifetime, Preset, Subject, TreePath,
};
use tilo_desktop_model::commands::{DesktopCommand, Standing, may_command};
use tilo_desktop_model::ids::{RoomId, WorkspaceId};
use tilo_desktop_model::{WORKSPACE_GOVERNANCE_APP_ID, Workspace};
use tilo_lock::RwLockRecover as _;
use tilo_peer_state::MemberAuthority;
use tilo_user_identity::DeviceCert;

use super::collaboration::{GovernanceCompletion, request_digest};
use super::{CollaborationError, GovernedRoom, Rooms, SystemTime, system_time, valid_name};

const SECOND_NS: u64 = 1_000_000_000;

/// One of the room's own changes, as a member asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerCommand {
    /// Name the workspace `name`.
    Rename { name: String },
    /// Save a named grant set, replacing the one with the same name.
    SavePreset { preset: Preset },
    /// Give `subject` the grants the saved set `preset` expands to, until
    /// `lifetime` ends.
    AssignPreset {
        preset: String,
        subject: Subject,
        lifetime: Lifetime,
    },
    /// Let app `app` use `action` on `target_app`'s `resource` here.
    GrantAppAction(AppAction),
    /// Take back every grant letting `app` use that action.
    RevokeAppAction(AppAction),
    /// Record `grant` in the room's policy, given by the asker.
    Grant { grant: Grant },
    /// Take back the grant `grant`.
    Revoke { grant: GrantId },
    /// Record a sandbox capability for the room's apps.
    GrantSandbox { capability: CapOverride },
    /// Take back the sandbox capability with this id.
    RevokeSandbox { capability: String },
}

/// The action an app-action grant is about: `action` on `target_app`'s
/// resource `resource`, wherever in the app that resource sits, used by
/// `app`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AppAction {
    pub app: String,
    pub target_app: String,
    pub resource: String,
    pub action: String,
}

impl AppAction {
    /// The action named, each part trimmed; refused when a part is empty.
    ///
    /// # Errors
    ///
    /// A part is empty.
    pub fn new(
        app: &str,
        target_app: &str,
        resource: &str,
        action: &str,
    ) -> Result<Self, CollaborationError> {
        let named = Self {
            app: app.trim().to_owned(),
            target_app: target_app.trim().to_owned(),
            resource: resource.trim().to_owned(),
            action: action.trim().to_owned(),
        };
        if named.app.is_empty()
            || named.target_app.is_empty()
            || named.resource.is_empty()
            || named.action.is_empty()
        {
            return Err(CollaborationError::new(
                "name the application, the other application, its resource and the action",
            ));
        }
        Ok(named)
    }

    /// The node the grant names, inside `room`'s node.
    fn node(&self, room: &TreePath) -> TreePath {
        room.join(
            &TreePath::root()
                .app(self.target_app.as_str())
                .rest()
                .resource(self.resource.as_str())
                .rest()
                .action(self.action.as_str()),
        )
    }
}

/// How long an app-action grant lasts, from the word a person picked.
///
/// # Errors
///
/// A `session` grant, which a room's governor has no session to end with,
/// and any other word but `always`.
pub fn app_action_lifetime(lifetime: &str) -> Result<Lifetime, CollaborationError> {
    match lifetime.trim().to_ascii_lowercase().as_str() {
        "always" => Ok(Lifetime::UntilRevoked),
        "session" => Err(CollaborationError::new(
            "the workspace's host has no session to end a session grant with: grant it always",
        )),
        _ => Err(CollaborationError::new("lifetime must be always")),
    }
}

impl OwnerCommand {
    /// What the command is called in the room's request records: two
    /// requests with one id and different commands are a conflict.
    fn digest(&self) -> Result<[u8; 32], CollaborationError> {
        match self {
            Self::Rename { name } => request_digest(&("owner.rename", name)),
            Self::SavePreset { preset } => request_digest(&("owner.preset.save", preset)),
            Self::AssignPreset {
                preset,
                subject,
                lifetime,
            } => request_digest(&("owner.preset.assign", preset, subject, lifetime)),
            Self::GrantAppAction(action) => request_digest(&("owner.app-action.grant", action)),
            Self::RevokeAppAction(action) => request_digest(&("owner.app-action.revoke", action)),
            Self::Grant { grant } => request_digest(&("owner.grant", grant)),
            Self::Revoke { grant } => request_digest(&("owner.revoke", grant)),
            Self::GrantSandbox { capability } => {
                request_digest(&("owner.sandbox.grant", capability))
            }
            Self::RevokeSandbox { capability } => {
                request_digest(&("owner.sandbox.revoke", capability))
            }
        }
    }

    /// The room's governance action the command exercises, named in the
    /// log line of a refusal.
    #[must_use]
    pub const fn gate(&self) -> &'static str {
        match self {
            Self::Rename { .. } | Self::SavePreset { .. } => governance::SETTINGS,
            Self::AssignPreset { .. }
            | Self::GrantAppAction(_)
            | Self::Grant { .. }
            | Self::GrantSandbox { .. } => governance::GRANT,
            Self::RevokeAppAction(_) | Self::Revoke { .. } | Self::RevokeSandbox { .. } => {
                governance::REVOKE
            }
        }
    }
}

/// The room's governance as a workspace reads it: its name, policy,
/// sandbox and grant sets, from the governor's committed document.
fn workspace_of(room: &GovernedRoom) -> Result<Workspace, CollaborationError> {
    let doc = room
        .peer()
        .admitted_room()
        .ok_or_else(|| CollaborationError::new("the room has no governance"))?;
    let id = RoomId::parse(room.room())
        .ok_or_else(|| CollaborationError::new("the room is not a UUID"))?;
    let mut workspace = Workspace::in_room(
        WorkspaceId(room.workspace_id()),
        id,
        room.record.name.clone(),
    );
    tilo_peer_state::room::adopt_room(&mut workspace, doc, &[]);
    Ok(workspace)
}

/// The grant letting `action`'s app use it, as `person` gives it.
fn app_action_grant(
    action: &AppAction,
    room_node: &TreePath,
    request_id: [u8; 32],
    person: &str,
) -> Result<Change, CollaborationError> {
    if action.target_app == WORKSPACE_GOVERNANCE_APP_ID {
        return Err(CollaborationError::new(
            "governing the workspace is granted to people, not apps",
        ));
    }
    // Enough for any action that at most changes the room undoably: an
    // outside effect still asks.
    let grant = Grant::new(
        format!("app-action:{}", hex::encode(&request_id[..16])),
        Subject::App(action.app.clone()),
        action.node(room_node),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Subject::person(person),
    )
    .with_lifetime(Lifetime::UntilRevoked);
    Ok(Change::Policy(vec![tilo_action_policy::Change::Grant(
        grant,
    )]))
}

/// Taking back every grant given for `action`, whether through
/// [`app_action_grant`] or by answering the app's ask in place.
fn app_action_revoke(action: &AppAction, room_node: &TreePath, workspace: &Workspace) -> Change {
    let subject = Subject::App(action.app.clone());
    let node = action.node(room_node);
    Change::Policy(
        workspace
            .policy
            .grants
            .iter()
            .filter(|grant| {
                grant.subject == subject && (grant.node == node || node.covers(&grant.node))
            })
            .map(|grant| tilo_action_policy::Change::Revoke(grant.id.clone()))
            .collect(),
    )
}

fn refusal(reason: &impl ToString) -> CollaborationError {
    CollaborationError::new(reason.to_string())
}

impl Rooms {
    /// Apply `command` to `room` for the device `device`, as its person
    /// asks: decided by their standing in the room now, committed as the
    /// room's next generation and published before this answers. A retry
    /// of `request_id` answers with what the first attempt did.
    ///
    /// # Errors
    ///
    /// Why the room refused the command, or why it could not be committed.
    pub async fn apply_owner_command(
        &self,
        room: &str,
        device: &DeviceCert,
        request_id: [u8; 32],
        command: OwnerCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        if request_id == [0; 32] {
            return Err(CollaborationError::new(
                "request identifier must be nonzero",
            ));
        }
        let now = tilo_clock::unix_now_ns();
        let digest = command.digest()?;
        let governed = self.room(room).await?;
        let mut governed = governed.lock().await;
        let member = self
            .standing(&governed, device, now)
            .inspect_err(|reason| {
                tracing::warn!(
                    device = %hex::encode(device.device_public_key),
                    operation = command.gate(),
                    %reason,
                    "room change denied"
                );
            })?;
        let key = self.inner.host.key.clone();
        let at = system_time(now);
        let decided =
            self.decide_owner_command(&governed, device, &member, request_id, command, now);
        let change = decided.inspect_err(|reason| {
            tracing::warn!(
                room = %governed.room(),
                device = %hex::encode(device.device_public_key),
                %reason,
                "room change refused"
            );
        })?;
        // Policy changes are checked once more by the room's own grant rule
        // as each is written: the asker gives no more than they hold.
        if let Change::Policy(changes) = &change {
            let bound = roles::foreign_device_bound();
            let authority = member.authority(&bound);
            let cx = Context::at_time(at);
            let mut checked = member.policy.clone();
            for change in changes {
                checked
                    .apply(&member.person, &authority, change.clone(), &cx, false)
                    .map_err(|error| refusal(&error))?;
            }
        }
        let renamed = match &change {
            Change::Rename(name) => Some(name.clone()),
            _ => None,
        };
        let generation = governed.record.generation;
        let prior_name = renamed
            .as_ref()
            .map(|name| std::mem::replace(&mut governed.record.name, name.clone()));
        let caller = device.clone();
        let done = self
            .administer(
                &mut governed,
                request_id,
                digest,
                |peer| change.apply(peer, &caller, &key, at),
                now,
            )
            .await;
        if let Some(prior) = prior_name
            && governed.record.generation == generation
        {
            // Nothing was committed: the room keeps the name it has.
            governed.record.name = prior;
        }
        done
    }

    /// What `command` changes in `room`, decided by `member`'s standing on
    /// `device` at `now`.
    fn decide_owner_command(
        &self,
        governed: &GovernedRoom,
        device: &DeviceCert,
        member: &MemberAuthority,
        request_id: [u8; 32],
        command: OwnerCommand,
        now: u64,
    ) -> Result<Change, CollaborationError> {
        let at = system_time(now);
        let seconds = now / SECOND_NS;
        let bound = roles::foreign_device_bound();
        let authority = member.authority(&bound);
        let cx = Context::at_time(at);
        let mut workspace = workspace_of(governed)?;
        let room_node = workspace.node();
        match command {
            OwnerCommand::Rename { name } => {
                let name = valid_name(&name)
                    .map_err(|error| refusal(&error))?
                    .to_owned();
                self.may_rename(governed, device, member, &workspace)?;
                Ok(Change::Rename(name))
            }
            OwnerCommand::SavePreset { preset } => workspace
                .save_preset_with_at(&authority, preset, seconds)
                .map(|()| Change::Presets(workspace.presets.clone()))
                .map_err(|error| refusal(&error)),
            OwnerCommand::AssignPreset {
                preset,
                subject,
                lifetime,
            } => {
                let assigned = workspace
                    .assign_preset_with_at(
                        &member.person,
                        &authority,
                        &preset,
                        &subject,
                        &lifetime,
                        seconds,
                    )
                    .map_err(|error| refusal(&error))?;
                Ok(Change::Grants(
                    workspace
                        .policy
                        .grants
                        .iter()
                        .filter(|grant| assigned.contains(&grant.id))
                        .cloned()
                        .collect(),
                ))
            }
            OwnerCommand::GrantAppAction(action) => {
                app_action_grant(&action, &room_node, request_id, &member.person)
            }
            OwnerCommand::RevokeAppAction(action) => {
                Ok(app_action_revoke(&action, &room_node, &workspace))
            }
            OwnerCommand::Grant { grant } => {
                if grant.node.room_id() == Some(governed.room()) {
                    Ok(Change::Policy(vec![tilo_action_policy::Change::Grant(
                        grant,
                    )]))
                } else {
                    Err(CollaborationError::new(
                        "a grant names a place in this room",
                    ))
                }
            }
            OwnerCommand::Revoke { grant } => {
                Ok(Change::Policy(vec![tilo_action_policy::Change::Revoke(
                    grant,
                )]))
            }
            OwnerCommand::GrantSandbox { mut capability } => {
                if capability.persists_across_restart() {
                    member
                        .policy
                        .check_action(&authority, governance::GRANT, &cx, false)
                        .map_err(|error| refusal(&error))?;
                    if capability.id.trim().is_empty() {
                        capability.id = format!("cap-{}", hex::encode(&request_id[..16]));
                    }
                    workspace.grant_sandbox(
                        capability.granted_by(Principal::Person(member.person.clone())),
                    );
                    Ok(Change::Sandbox(workspace.sandbox.clone()))
                } else {
                    Err(CollaborationError::new(
                        "a capability that ends with a session or a single use is never \
                         written into the room",
                    ))
                }
            }
            OwnerCommand::RevokeSandbox { capability } => {
                member
                    .policy
                    .check_action(&authority, governance::REVOKE, &cx, false)
                    .map_err(|error| refusal(&error))?;
                workspace.revoke_sandbox(&capability);
                Ok(Change::Sandbox(workspace.sandbox.clone()))
            }
        }
    }

    /// The asker's standing in `room`: a person's current device, not one
    /// of this host's agents, admitted to the room under the person its
    /// certificate names.
    fn standing(
        &self,
        room: &GovernedRoom,
        device: &DeviceCert,
        now: u64,
    ) -> Result<MemberAuthority, CollaborationError> {
        if device.user_public_key == self.inner.host.cert.user_public_key
            && self
                .inner
                .host
                .agents
                .read_recover()
                .contains_key(&device.device_public_key)
        {
            return Err(CollaborationError::new(
                "room changes require a person's device, not an agent's",
            ));
        }
        if self.revocations().is_revoked(&device.device_public_key) {
            return Err(CollaborationError::new("device revoked"));
        }
        tilo_user_identity::verify_device_cert_at(device, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let member = room
            .peer()
            .permissions(device.device_public_key, system_time(now))
            .map_err(|reason| {
                CollaborationError::new(format!("not a member of the room: {reason}"))
            })?;
        if member.person != hex::encode(device.user_public_key) {
            return Err(CollaborationError::new(
                "the device's certificate names another person than the room admitted",
            ));
        }
        Ok(member)
    }

    /// Whether `member`, on `device`, may rename the workspace: its owner
    /// always, and otherwise someone who governs it (a moderator or above,
    /// not blocked) in a room that shares its members' layout. A room that
    /// shares none is named by its host.
    fn may_rename(
        &self,
        room: &GovernedRoom,
        device: &DeviceCert,
        member: &MemberAuthority,
        workspace: &Workspace,
    ) -> Result<(), CollaborationError> {
        if device.user_public_key == room.peer().authority().user_public_key {
            return Ok(());
        }
        let standing = Standing {
            person: &member.person,
            role: (!workspace.is_blocked(&member.person)).then_some(member.role),
            layout_shared: !self.layout(room.room()).is_personal(),
            members: None,
        };
        may_command(
            &standing,
            &tilo_desktop_model::ownership::Authors::new(),
            workspace,
            &DesktopCommand::RenameWorkspace {
                name: String::new(),
            },
        )
        .map_err(|error| refusal(&error))
    }

    /// Whether the device `device` may ask for what `offer` names in
    /// `room` now: the decision [`Self::apply_owner_command`] makes before
    /// it looks at the command's arguments, for a catalogue that offers
    /// only what its caller may do and an invocation that treats a
    /// forbidden action as an unknown one.
    pub(super) fn may_offer(
        &self,
        room: &GovernedRoom,
        device: &DeviceCert,
        offer: Offer,
        now: u64,
    ) -> bool {
        let Ok(member) = self.standing(room, device, now) else {
            return false;
        };
        let bound = roles::foreign_device_bound();
        let authority = member.authority(&bound);
        let cx = Context::at_time(system_time(now));
        let may = |action: &str| {
            member
                .policy
                .check_action(&authority, action, &cx, false)
                .is_ok()
        };
        match offer {
            Offer::Rename => workspace_of(room)
                .and_then(|workspace| self.may_rename(room, device, &member, &workspace))
                .is_ok(),
            // Assigning is granting, and a grant on one set's assign node
            // lets someone assign that one set.
            Offer::AssignPreset => {
                may(governance::GRANT)
                    || workspace_of(room).is_ok_and(|workspace| {
                        workspace.presets.iter().any(|preset| {
                            authority
                                .decide(
                                    &tilo_desktop_model::preset_assign_node(
                                        room.room(),
                                        &preset.name,
                                    ),
                                    roles::GOVERNANCE_KIND,
                                    &cx,
                                )
                                .answer
                                == Answer::Allow
                        })
                    })
            }
            Offer::Govern(action) => may(action),
        }
    }
}

/// What a room change is offered by: the decision before its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Offer {
    /// Renaming, decided by role ([`Rooms::may_rename`]).
    Rename,
    /// Assigning a grant set: the room's grant action, or a grant on one
    /// set's assign node.
    AssignPreset,
    /// One of the room's governance actions.
    Govern(&'static str),
}

/// What a decided command writes into the room's governance.
enum Change {
    Rename(String),
    Presets(Vec<Preset>),
    /// Grants already decided and written as they are: an assigned set's.
    Grants(Vec<Grant>),
    /// Policy changes the room's grant rule decides as each is written.
    Policy(Vec<tilo_action_policy::Change>),
    Sandbox(Vec<CapOverride>),
}

impl Change {
    fn apply(
        &self,
        peer: &mut tilo_peer_state::StatePeer,
        caller: &DeviceCert,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        match self {
            Self::Rename(name) => {
                peer.governance_mut().author_room_name(name, key, now)?;
                Ok(true)
            }
            Self::Presets(presets) => peer
                .governance_mut()
                .put_governed_presets(presets, key, now),
            Self::Grants(grants) => peer.governance_mut().put_governed_grants(grants, key, now),
            Self::Policy(changes) => {
                let mut wrote = false;
                for change in changes {
                    let command = match change {
                        tilo_action_policy::Change::Grant(grant) => Command::Grant {
                            grant: grant.clone(),
                        },
                        tilo_action_policy::Change::Revoke(grant) => Command::Revoke {
                            grant: grant.clone(),
                        },
                        _ => {
                            return Err("only grants and revocations change the policy here".into());
                        }
                    };
                    wrote |= peer
                        .workspace_mut()
                        .apply_workspace_membership_command(caller, &command, key, now)?;
                }
                Ok(wrote)
            }
            Self::Sandbox(sandbox) => peer
                .governance_mut()
                .put_governed_sandbox(sandbox, key, now),
        }
    }
}
