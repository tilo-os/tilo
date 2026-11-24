//! The workspaces a host governs, administered from the page: one call turns
//! the page's arguments into the shared protocol's own typed request
//! ([`tilo_server_protocol::workspaces`]) and answers in the shape the page
//! draws and its apps read as an action's output — times in milliseconds,
//! roles as the workspace settings name them, keys in hex.
//!
//! Nothing is decided here: the host admits every request by the person's
//! member access, and its refusal is the answer.

use serde::Deserialize;
use serde_json::{Value, json};
use tilo_server_protocol::workspaces::{
    Member, PendingInvite, WaitListEntry, Workspace, WorkspacesReply, WorkspacesRequest,
};
use tilo_workspace_join::encode_invitation_text;
use wasm_bindgen::prelude::*;

use crate::peer::{WasmPeer, device_key, js_error};
use tilo_action_policy::Role;

/// How long an invitation stays redeemable unless the page says.
const INVITATION_SECONDS: u64 = 7 * 24 * 60 * 60;
/// The longest a host keeps an invitation redeemable.
const MAX_INVITATION_SECONDS: u64 = 30 * 24 * 60 * 60;

/// What one call carries, as the page sends it: `workspace` is a workspace
/// UUID, keys and invitation ids are hex, and the invitation's own arguments
/// are the sheet's.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WorkspacesArgs {
    workspace: String,
    name: String,
    role: String,
    expires_in_seconds: Option<u64>,
    inviter_name: Option<String>,
    wait_list_entry: Option<String>,
    user_public_key: Option<String>,
    invite_id: Option<String>,
    request_id: Option<String>,
    link: String,
}

/// The role a host is asked for, under the name the workspace settings give it.
pub(crate) fn wire_role(name: &str) -> Result<Role, JsError> {
    Role::parse(name)
        .filter(|role| role.assignable())
        .ok_or_else(|| js_error(format!("no such role: {name}")))
}

/// The key a request names as `what`, refused when it names none.
fn named_key(key: Option<&String>, what: &str) -> Result<[u8; 32], JsError> {
    let key = key.ok_or_else(|| js_error(format!("the request names no {what}")))?;
    device_key(key)
}

impl WorkspacesArgs {
    fn role(&self) -> Result<Role, JsError> {
        wire_role(&self.role)
    }

    /// The invitation to `workspace` these arguments ask for: an editor's
    /// unless they name a role, or `administrator`.
    fn invitation(&self, workspace: String) -> Result<WorkspacesRequest, JsError> {
        Ok(WorkspacesRequest::Invite {
            workspace,
            request_id: new_request_id()?,
            // An administrator joins as an Editor the invitation also makes
            // an administrator.
            role: match self.role.as_str() {
                "" | "administrator" => Role::Editor,
                _ => self.role()?,
            },
            expires_at_ns: tilo_clock::unix_now_ns()
                .saturating_add(seconds(self.expires_in_seconds)?.saturating_mul(1_000_000_000)),
            inviter_name: self.inviter_name.clone(),
            wait_list_entry: self
                .wait_list_entry
                .as_deref()
                .map(device_key)
                .transpose()?,
            recipient: None,
            administrator: self.role == "administrator",
        })
    }

    /// The change to the person these arguments name that `op` asks for: their role set,
    /// or removing, banning or unbanning them.
    fn about_member(&self, op: &str) -> Result<tilo_action_policy::Change, JsError> {
        use tilo_action_policy::Change;
        let person = hex::encode(named_key(self.user_public_key.as_ref(), "person")?);
        Ok(match op {
            "setRole" => Change::SetRole {
                person,
                role: self.role()?,
            },
            "remove" => Change::RemoveMember { person },
            "ban" => Change::Ban { person },
            _ => Change::Unban { person },
        })
    }
}

/// How long an invitation lasts, `asked` or [`INVITATION_SECONDS`]: a second
/// at least, [`MAX_INVITATION_SECONDS`] at most.
pub(crate) fn seconds(asked: Option<u64>) -> Result<u64, JsError> {
    let seconds = asked.unwrap_or(INVITATION_SECONDS);
    if seconds == 0 || seconds > MAX_INVITATION_SECONDS {
        return Err(js_error(format!(
            "an invitation lasts 1 to {MAX_INVITATION_SECONDS} seconds"
        )));
    }
    Ok(seconds)
}

/// Nanoseconds as the page counts time.
fn millis(nanoseconds: u64) -> u64 {
    nanoseconds / 1_000_000
}

fn member(value: &Member) -> Value {
    json!({"member": value.id, "name": value.name,
        "role": value.role.map(Role::key), "removed": value.removed,
        // What the workspace made them, and who reads this row: the page's
        // People list offers removal where the workspace's rules allow it.
        "administrator": value.administrator, "grants": value.grants, "self": value.is_self})
}

fn workspace(value: &Workspace) -> Value {
    json!({"workspace": value.workspace, "name": value.name, "generation": value.generation,
        "members": value.members.iter().map(member).collect::<Vec<_>>()})
}

fn invitation(value: &PendingInvite) -> Value {
    json!({"inviteId": hex::encode(value.invite_id), "role": value.role.key(),
        "expiresAtMs": millis(value.expires_at_ns)})
}

fn listed(value: &WaitListEntry) -> Value {
    json!({"requestId": hex::encode(value.request_id), "contact": value.contact,
        "addedAtMs": millis(value.added_at_ns),
        "invitedAtMs": value.invited_at_ns.map(millis)})
}

/// What `reply` answered, which must be what the call asked for.
fn answered<T>(
    reply: WorkspacesReply,
    of: impl FnOnce(WorkspacesReply) -> Option<T>,
) -> Result<T, JsError> {
    match reply {
        WorkspacesReply::Refused(reason) => Err(js_error(reason)),
        other => of(other)
            .ok_or_else(|| js_error("the host answered something else than what was asked")),
    }
}

/// The reply's entries of one kind, in the page's shape.
fn entries(reply: WorkspacesReply, wanted: &str) -> Result<Value, JsError> {
    Ok(Value::Array(match reply {
        WorkspacesReply::Refused(reason) => return Err(js_error(reason)),
        WorkspacesReply::Pending(list) => list.iter().map(invitation).collect(),
        WorkspacesReply::WaitList(list) => list.iter().map(listed).collect(),
        _ => {
            return Err(js_error(format!(
                "the host answered something else than its {wanted}"
            )));
        }
    }))
}

/// The workspaces the host governs.
fn governed(reply: WorkspacesReply) -> Result<Vec<Workspace>, JsError> {
    answered(reply, |reply| match reply {
        WorkspacesReply::Workspaces(list) => Some(list),
        _ => None,
    })
}

/// The workspace `reply` answers with, in the page's shape.
fn one(reply: WorkspacesReply) -> Result<Value, JsError> {
    answered(reply, |reply| match reply {
        WorkspacesReply::Workspace(workspace) => Some(workspace),
        _ => None,
    })
    .map(|found| workspace(&found))
}

/// The invitation `reply` issued, as the text people pass around.
pub(crate) fn invitation_text(reply: WorkspacesReply) -> Result<String, JsError> {
    let signed = answered(reply, |reply| match reply {
        WorkspacesReply::Invitation {
            signed_invitation, ..
        } => Some(signed_invitation),
        _ => None,
    })?;
    Ok(encode_invitation_text(&signed))
}

fn new_request_id() -> Result<[u8; 32], JsError> {
    let mut id = [0; 32];
    getrandom::fill(&mut id).map_err(js_error)?;
    Ok(id)
}

impl WasmPeer {
    /// The entries the server answers `waitList`, `unlist`, `mailInvite` and `cancel` with: the
    /// wait list each is about, or the invitations a cancellation comes back with.
    async fn entries_asked(
        &self,
        op: &str,
        args: &WorkspacesArgs,
        id: &str,
    ) -> Result<Value, JsError> {
        let (request, wanted) = match op {
            "waitList" => (
                WorkspacesRequest::WaitList {
                    workspace: id.to_owned(),
                },
                "wait list",
            ),
            "unlist" => (
                WorkspacesRequest::Unlist {
                    workspace: id.to_owned(),
                    request_id: named_key(args.request_id.as_ref(), "wait-list entry")?,
                },
                "wait list",
            ),
            "mailInvite" => (
                WorkspacesRequest::MailInvite {
                    workspace: id.to_owned(),
                    wait_list_entry: named_key(args.request_id.as_ref(), "wait-list entry")?,
                    link: args.link.clone(),
                },
                "wait list",
            ),
            _ => (
                WorkspacesRequest::Cancel {
                    workspace: id.to_owned(),
                    request_id: new_request_id()?,
                    invite_id: named_key(args.invite_id.as_ref(), "invitation")?,
                },
                "invitations",
            ),
        };
        entries(self.workspaces_request(request).await?, wanted)
    }
}

impl WasmPeer {
    /// Write `change` to the people of the workspace `id` names as this page's device, and wait
    /// for its host to hold it.
    async fn govern(&self, id: &str, change: tilo_action_policy::Change) -> Result<(), JsError> {
        let workspace = tilo_desktop_model::WorkspaceId::parse(id)
            .ok_or_else(|| js_error(format!("{id} names no workspace")))?;
        let membership =
            tilo_session_client::membership::Membership::of(self.handle.engine(), workspace);
        membership.commit(&change).await.map_err(js_error)?;
        membership.settled(&self.handle).await.map_err(js_error)
    }
}

#[wasm_bindgen]
impl WasmPeer {
    /// Administer the host's workspaces as this device: `op` names the call and
    /// `args` is its JSON ([`WorkspacesArgs`]); resolves to the page's shape as JSON.
    ///
    /// # Errors
    ///
    /// Throws for malformed arguments, when the host refuses, or when the
    /// session closed before it answered.
    pub async fn workspaces(&self, op: &str, args: String) -> Result<String, JsError> {
        let args: WorkspacesArgs =
            serde_json::from_str(if args.is_empty() { "{}" } else { &args }).map_err(js_error)?;
        let id = args.workspace.clone();
        let answer = match op {
            "workspaces" => Value::Array(
                governed(self.workspaces_request(WorkspacesRequest::List).await?)?
                    .iter()
                    .map(workspace)
                    .collect(),
            ),
            "create" => one(self
                .workspaces_request(WorkspacesRequest::Create {
                    name: args.name.clone(),
                })
                .await?)?,
            // A workspace made under the id the page chose, as `create` makes one
            // under an id the server chooses: the same id asked again places the one
            // already made and admits this device, so a page that asks again after
            // no answer makes no second workspace.
            "place" => one(self
                .workspaces_request(WorkspacesRequest::Place {
                    workspace: id.clone(),
                    name: args.name.clone(),
                })
                .await?)?,
            "enter" => one(self
                .workspaces_request(WorkspacesRequest::Enter {
                    workspace: id.clone(),
                })
                .await?)?,
            // The person's workspaces as their host lists them, the same question the CLI's peer
            // asks: what the page offers besides the workspaces this browser remembered.
            "personal" => self.personal_workspaces().await?,
            "invite" => {
                let reply = self
                    .workspaces_request(args.invitation(id.clone())?)
                    .await?;
                Value::String(invitation_text(reply)?)
            }
            "list" => {
                let list = governed(self.workspaces_request(WorkspacesRequest::List).await?)?;
                match list.iter().find(|held| held.workspace == id) {
                    Some(held) => workspace(held),
                    None => return Err(js_error("the server does not hold this workspace")),
                }
            }
            // Whether the workspace lets this page's person invite here: the question its
            // governance decides inviting by, asked before anything only an inviter may see.
            "mayInvite" => {
                let node = tilo_action_policy::roles::governance_action(
                    &id,
                    tilo_action_policy::roles::governance::ADD_MEMBER,
                );
                Value::Bool(answered(
                    self.workspaces_request(WorkspacesRequest::Permits {
                        workspace: id.clone(),
                        node: node.to_string(),
                    })
                    .await?,
                    |reply| match reply {
                        WorkspacesReply::Permits(may) => Some(may),
                        _ => None,
                    },
                )?)
            }
            "waitList" | "unlist" | "mailInvite" | "cancel" => {
                self.entries_asked(op, &args, &id).await?
            }
            // A change to the workspace's people is this page's own, signed as its person's
            // device as the desktop signs it, and relayed by the workspace's host: answered with
            // the workspace as the host lists it once it holds the change.
            "setRole" | "remove" | "ban" | "unban" => {
                self.govern(&id, args.about_member(op)?).await?;
                let list = governed(self.workspaces_request(WorkspacesRequest::List).await?)?;
                match list.iter().find(|held| held.workspace == id) {
                    Some(held) => workspace(held),
                    None => return Err(js_error("the server does not hold this workspace")),
                }
            }
            other => return Err(js_error(format!("no such request: {other}"))),
        };
        serde_json::to_string(&answer).map_err(js_error)
    }
}
