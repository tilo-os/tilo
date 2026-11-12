//! The workspaces a host governs, administered from the page: one call turns
//! the page's arguments into the shared protocol's own typed request
//! ([`tilo_relay_protocol::workspaces`]) and answers in the shape the page
//! draws and its apps read as an action's output — times in milliseconds,
//! roles as the workspace settings name them, keys in hex.
//!
//! Nothing is decided here: the host admits every request by the person's
//! standing, and its refusal is the answer.

use serde::Deserialize;
use serde_json::{Value, json};
use tilo_relay_protocol::workspaces::{
    PendingInvite, WaitListEntry, Workspace, WorkspaceMember, WorkspaceRole, WorkspacesReply,
    WorkspacesRequest,
};
use tilo_workspace_join::encode_invitation_text;
use wasm_bindgen::prelude::*;

use crate::invite::{role_from_id, role_id, role_of, workspace_role};
use crate::peer::{WasmPeer, device_key, js_error};

/// How long an invitation stays redeemable unless the page says.
const INVITATION_SECONDS: u64 = 7 * 24 * 60 * 60;
/// The longest a host keeps an invitation redeemable.
const MAX_INVITATION_SECONDS: u64 = 30 * 24 * 60 * 60;

/// What one call carries, as the page sends it: `workspace` is a workspace
/// UUID, keys and invitation ids are hex, and the invitation's own arguments
/// are the sheet's.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RoomsArgs {
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
pub(crate) fn wire_role(name: &str) -> Result<WorkspaceRole, JsError> {
    role_from_id(name)
        .and_then(workspace_role)
        .ok_or_else(|| js_error(format!("no such role: {name}")))
}

/// The key a request names as `what`, refused when it names none.
fn named_key(key: Option<&String>, what: &str) -> Result<[u8; 32], JsError> {
    let key = key.ok_or_else(|| js_error(format!("the request names no {what}")))?;
    device_key(key)
}

impl RoomsArgs {
    fn role(&self) -> Result<WorkspaceRole, JsError> {
        wire_role(&self.role)
    }

    /// The invitation to `workspace` these arguments ask for: an editor's
    /// unless they name a role.
    fn invitation(&self, workspace: String) -> Result<WorkspacesRequest, JsError> {
        Ok(WorkspacesRequest::Invite {
            workspace,
            request_id: new_request_id()?,
            role: match self.role.as_str() {
                "" => WorkspaceRole::Editor,
                _ => self.role()?,
            },
            expires_at_ns: ui::time::unix_now_ns()
                .saturating_add(seconds(self.expires_in_seconds)?.saturating_mul(1_000_000_000)),
            inviter_name: self.inviter_name.clone(),
            wait_list_entry: self
                .wait_list_entry
                .as_deref()
                .map(device_key)
                .transpose()?,
            recipient: None,
        })
    }

    /// Removing, banning or unbanning the person these arguments name.
    fn about_member(&self, op: &str, workspace: String) -> Result<WorkspacesRequest, JsError> {
        let user_public_key = named_key(self.user_public_key.as_ref(), "person")?;
        let request_id = new_request_id()?;
        Ok(match op {
            "remove" => WorkspacesRequest::Remove {
                workspace,
                request_id,
                user_public_key,
            },
            "ban" => WorkspacesRequest::Ban {
                workspace,
                request_id,
                user_public_key,
            },
            _ => WorkspacesRequest::Unban {
                workspace,
                request_id,
                user_public_key,
            },
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

fn member(value: &WorkspaceMember) -> Value {
    json!({"userPublicKey": hex::encode(value.user_public_key), "name": value.name,
        "role": role_id(role_of(value.role)), "owner": value.owner, "removed": value.removed})
}

fn workspace(value: &Workspace) -> Value {
    json!({"room": value.workspace, "name": value.name, "generation": value.generation,
        "members": value.members.iter().map(member).collect::<Vec<_>>()})
}

fn invitation(value: &PendingInvite) -> Value {
    json!({"inviteId": hex::encode(value.invite_id), "role": role_id(role_of(value.role)),
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

#[wasm_bindgen]
impl WasmPeer {
    /// Administer the host's workspaces as this device: `op` names the call and
    /// `args` is its JSON ([`RoomsArgs`]); resolves to the page's shape as JSON.
    ///
    /// # Errors
    ///
    /// Throws for malformed arguments, when the host refuses, or when the
    /// session closed before it answered.
    pub async fn rooms(&self, op: &str, args: String) -> Result<String, JsError> {
        let args: RoomsArgs =
            serde_json::from_str(if args.is_empty() { "{}" } else { &args }).map_err(js_error)?;
        let room = args.workspace.clone();
        let answer = match op {
            "workspaces" => Value::Array(
                governed(self.rooms_request(WorkspacesRequest::List).await?)?
                    .iter()
                    .map(workspace)
                    .collect(),
            ),
            "create" => one(self
                .rooms_request(WorkspacesRequest::Create {
                    name: args.name.clone(),
                })
                .await?)?,
            "enter" => one(self
                .rooms_request(WorkspacesRequest::Enter {
                    workspace: room.clone(),
                })
                .await?)?,
            "invite" => {
                let reply = self.rooms_request(args.invitation(room.clone())?).await?;
                Value::String(invitation_text(reply)?)
            }
            "list" => {
                let list = governed(self.rooms_request(WorkspacesRequest::List).await?)?;
                match list.iter().find(|held| held.workspace == room) {
                    Some(held) => workspace(held),
                    None => return Err(js_error("the host does not govern this workspace")),
                }
            }
            // Whether the workspace lets this page's person invite here: the question its
            // governance decides inviting by, asked before anything only an inviter may see.
            "mayInvite" => {
                let node = tilo_action_policy::roles::governance_action(
                    &room,
                    tilo_action_policy::roles::governance::INVITE,
                );
                Value::Bool(answered(
                    self.rooms_request(WorkspacesRequest::Permits {
                        workspace: room.clone(),
                        node: node.to_string(),
                    })
                    .await?,
                    |reply| match reply {
                        WorkspacesReply::Permits(may) => Some(may),
                        _ => None,
                    },
                )?)
            }
            "waitList" => entries(
                self.rooms_request(WorkspacesRequest::WaitList { workspace: room })
                    .await?,
                "wait list",
            )?,
            "unlist" => entries(
                self.rooms_request(WorkspacesRequest::Unlist {
                    workspace: room.clone(),
                    request_id: named_key(args.request_id.as_ref(), "wait-list entry")?,
                })
                .await?,
                "wait list",
            )?,
            "mailInvite" => entries(
                self.rooms_request(WorkspacesRequest::MailInvite {
                    workspace: room.clone(),
                    wait_list_entry: named_key(args.request_id.as_ref(), "wait-list entry")?,
                    link: args.link.clone(),
                })
                .await?,
                "wait list",
            )?,
            "setRole" => one(self
                .rooms_request(WorkspacesRequest::SetRole {
                    workspace: room.clone(),
                    request_id: new_request_id()?,
                    user_public_key: named_key(args.user_public_key.as_ref(), "person")?,
                    role: args.role()?,
                })
                .await?)?,
            "remove" | "ban" | "unban" => one(self
                .rooms_request(args.about_member(op, room.clone())?)
                .await?)?,
            "cancel" => entries(
                self.rooms_request(WorkspacesRequest::Cancel {
                    workspace: room.clone(),
                    request_id: new_request_id()?,
                    invite_id: named_key(args.invite_id.as_ref(), "invitation")?,
                })
                .await?,
                "invitations",
            )?,
            other => return Err(js_error(format!("no such request: {other}"))),
        };
        serde_json::to_string(&answer).map_err(js_error)
    }
}
