//! The workspace's people, as the path `workspace/members` of every workspace
//! a key acts in: who is in it, and bringing someone else in.
//!
//! - `workspace/members#list` (or `tilo_read workspace/members`) answers the
//!   members, each with their role, and the invitations still open;
//! - `workspace/members#invite {role, email?}` answers a link that brings one
//!   person in as `role`.
//!
//! Both are the workspace's own governance actions (`tilo.workspace`'s
//! `invite`, `member.list` and `invitation.list`), asked of its host over the
//! key's session, and the host decides who may: a person's server key
//! invites into the workspaces its person administers, as an Editor at most;
//! an agent's grant and a key for one workspace do not. The link is the one a
//! person's desk hands over: the invitation sealed onto the site's shelf
//! (`https://<site>/i/<handle>#<key>`), or, where the site keeps nothing, the
//! invitation's own text after the workspace's route. Following it opens the
//! site's sign-in, with a passkey or by approving an agent, and lands the
//! person in the workspace with the role.
//!
//! The door sends no mail: with `email`, the invitation names that address
//! and the answer says `sent: false`, with the link to hand over.

use super::Session;
use crate::account::{Outcome, answer, failed, refused};
use base64::Engine as _;
use serde_json::{Map, Value, json};
use tilo_actions::remote::RemoteActionTarget;
use tilo_relay_client::CertifyError;

/// The path of a workspace's people.
pub const ROOT: &str = "workspace/members";
/// The roles a key invites anyone as: never above an Editor.
const ROLES: [&str; 3] = ["viewer", "commenter", "editor"];
/// The workspace's governance actions this path asks.
const INVITE: &str = "invite";
const MEMBERS: &str = "member.list";
const INVITATIONS: &str = "invitation.list";

/// Whether `tool` with `arguments` is about the workspace's people.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    matches!(tool, "tilo_do" | "tilo_read")
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.split('#').next() == Some(ROOT))
}

impl Session {
    /// Answer `tool` with `arguments` on `workspace`'s people, typed when
    /// `structured`.
    pub(super) async fn members(
        &self,
        workspace: &str,
        tool: &str,
        arguments: &Value,
        structured: bool,
    ) -> Value {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = arguments.get("args").cloned().unwrap_or_else(|| json!({}));
        let outcome = match (tool, path) {
            ("tilo_do", "workspace/members#invite") => self.invite(workspace, &args).await,
            ("tilo_do", "workspace/members#list") | ("tilo_read", ROOT) => {
                self.list(workspace).await
            }
            (_, other) => failed(
                "not_found",
                format!("the workspace has no action {other}"),
                "workspace/members#list and workspace/members#invite {role, email?} are its actions",
            ),
        };
        answer(structured, path, outcome)
    }

    /// Ask `workspace`'s host its governance action `verb` with `arguments`,
    /// entering the workspace the first time.
    async fn govern(
        &self,
        workspace: &str,
        verb: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, Outcome> {
        let id = self
            .relay
            .enter(&self.connection, self.parent.as_deref(), workspace)
            .await
            .map_err(|error| refused(&error))?;
        let target = RemoteActionTarget::workspace_governance(workspace, verb);
        self.connection
            .device
            .ask_host(id, target, arguments, "tilo front door")
            .await
            .map_err(|error| match error {
                CertifyError::Denied(reason) => failed(
                    "forbidden",
                    format!("the workspace refused: {reason}"),
                    "a key you created, reaching all your workspaces, invites into the ones you \
                     administer, as an editor at most",
                ),
                other => failed("unavailable", other.to_string(), "try again shortly"),
            })
    }

    async fn list(&self, workspace: &str) -> Outcome {
        let members = match self.govern(workspace, MEMBERS, Map::new()).await {
            Ok(members) => members,
            Err(outcome) => return outcome,
        };
        match self.govern(workspace, INVITATIONS, Map::new()).await {
            Ok(invitations) => Outcome::Ok(json!({
                "workspace": workspace,
                "members": members,
                "invitations": invitations,
            })),
            Err(outcome) => outcome,
        }
    }

    async fn invite(&self, workspace: &str, args: &Value) -> Outcome {
        let text = |name: &str| args.get(name).and_then(Value::as_str).map(str::trim);
        let Some(role) = text("role").filter(|role| ROLES.contains(role)) else {
            return failed(
                "invalid_arguments",
                "`role` is viewer, commenter or editor",
                "pass args {role: \"editor\", email?: \"ops@example.com\"}",
            );
        };
        let email = match text("email").filter(|email| !email.is_empty()) {
            Some(email) => match tilo_relay_protocol::workspaces::contact_address(email) {
                Ok(email) => Some(email),
                Err(why) => return failed("invalid_arguments", why, "pass one email address"),
            },
            None => None,
        };
        let mut asked = Map::new();
        asked.insert("role".into(), json!(role));
        if let Some(email) = &email {
            asked.insert("invitee_name".into(), json!(email));
        }
        let issued = match self.govern(workspace, INVITE, asked).await {
            Ok(issued) => issued,
            Err(outcome) => return outcome,
        };
        let invitation = issued
            .get("invitation")
            .and_then(Value::as_str)
            .and_then(|text| hex::decode(text).ok());
        let (Some(invitation), Some(expires_at_ns)) = (
            invitation,
            issued.get("expires_at_ns").and_then(Value::as_u64),
        ) else {
            return failed(
                "unavailable",
                "the workspace answered no invitation",
                "try again shortly",
            );
        };
        let Some(door) = self.door.upgrade() else {
            return failed(
                "unavailable",
                "the door is shutting down",
                "try again shortly",
            );
        };
        let link = door
            .invitation_link(workspace, &invitation, expires_at_ns)
            .await;
        let mut answered = json!({
            "workspace": workspace,
            "invite": issued.get("invite"),
            "role": role,
            "link": link,
            "expires_at_ns": expires_at_ns,
        });
        if let (Some(email), Some(fields)) = (email, answered.as_object_mut()) {
            fields.insert(
                "next".into(),
                json!(format!(
                    "this door sends no mail yet: send the link to {email} yourself"
                )),
            );
            fields.insert("email".into(), json!(email));
            fields.insert("sent".into(), json!(false));
        }
        Outcome::Ok(answered)
    }
}

impl crate::Door {
    /// The link that brings whoever follows it into `workspace` with
    /// `invitation` (its signed bytes), lasting until `expires_at_ns`: sealed
    /// onto the site's shelf, as a person's desk hands one over, or the
    /// invitation's own text after the workspace's route where the site
    /// keeps nothing. Either opens the site's sign-in and joins.
    pub(crate) async fn invitation_link(
        &self,
        workspace: &str,
        invitation: &[u8],
        expires_at_ns: u64,
    ) -> String {
        let entry = self.config.web_entry.trim_end_matches('/');
        let shelved = match (
            tilo_workspace_directory::client(),
            tilo_workspace_directory::web_origin(&format!("{entry}/")),
        ) {
            (Ok(client), Ok(site)) => {
                tilo_workspace_directory::shelve(
                    &client,
                    &site,
                    Some(&self.config.relay),
                    invitation,
                    expires_at_ns,
                )
                .await
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        };
        shelved.map_or_else(
            |error| {
                tracing::info!(%error, "the site kept no invitation; handing over its text");
                let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(invitation);
                format!("{entry}/#/w/{workspace}?invite={text}")
            },
            |shelved| shelved.link,
        )
    }
}
