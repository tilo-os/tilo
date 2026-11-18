//! The workspace's people, as the path `workspace/members` of every workspace
//! a key acts in: who is in it, bringing someone else in, and the name the
//! key's own person goes by.
//!
//! - `workspace/members#list` (or `tilo_read workspace/members`) answers the
//!   members, each with their role, and the invitations still open;
//! - `workspace/members#invite {role, email?, waiting?}` answers a link that
//!   brings one person in as `role`, an administrator or an owner included;
//!   with `waiting`, for that address on the workspace's wait list, mailed
//!   to it where the workspace's entry sends mail;
//! - `workspace/members#add {person, role, name?}` holds the person whose
//!   user public key is `person` in the workspace at `role` (`administrator`
//!   and `owner` included) at once: their entry lets them in, and a key of
//!   theirs reaches the workspace before any device of theirs has joined;
//! - `workspace/members#role {person, role}` moves a member to another role,
//!   as far as the key's person may give it;
//! - `workspace/members#remove {person}` removes a member and every device of
//!   theirs, as the workspace's governance lets the key's person: the same
//!   request, decided the same way, as removing them on the person's page;
//! - `workspace/members#wait-list` (or `tilo_read workspace/members/wait-list`)
//!   answers the addresses left at the workspace's entry while it takes
//!   people by invitation, and `workspace/members#unlist {entry}` takes one
//!   off;
//! - `workspace/members#name {name}` names the key's own person in the
//!   workspace: what its member list and the invitations they make say for
//!   them.
//!
//! All are the workspace's own governance actions (`tilo.workspace`'s
//! `invite`, `member.add`, `member.list`, `invitation.list`, `member.name`
//! and `wait-list.*`), asked of its host over the key's session, and the host
//! decides who may, and keeps who asked: a person's server key does what its
//! person may, and an agent what its grants there let it, asking its person
//! first unless they allowed it. A person names themself, from any key of
//! theirs, in a workspace they are a member of. The link is the one a person's shell hands over: the invitation
//! sealed onto the site's shelf (`https://<site>/i/<handle>#<key>`), or, where
//! the site keeps nothing, the invitation's own text after the workspace's
//! route. Following it opens the site's sign-in, with a passkey or by
//! approving an agent, and lands the person in the workspace with the role.
//!
//! The server sends no mail: with `email`, the invitation names that address
//! and the answer says `sent: false`, with the link to hand over.

use super::Session;
use crate::account::{Outcome, answer, failed, refused};
use crate::server::Administration;
use base64::Engine as _;
use serde_json::{Map, Value, json};
use tilo_actions::remote::RemoteActionTarget;
use tilo_session_client::device::DeviceRefusal;

/// The path of a workspace's people.
pub const ROOT: &str = "workspace/members";
/// The path of the addresses left at a workspace's entry.
const WAIT_LIST_ROOT: &str = "workspace/members/wait-list";
/// The roles a key invites anyone as: never above an Editor, and
/// `administrator` or `owner` only where its person may make one.
const ROLES: [&str; 5] = ["viewer", "commenter", "editor", "administrator", "owner"];
/// The places a key adds a person at: what it invites as, and a Moderator.
const PLACES: [&str; 6] = [
    "viewer",
    "commenter",
    "editor",
    "moderator",
    "administrator",
    "owner",
];
/// The workspace's governance actions this path asks.
const INVITE: &str = "invite";
const ADD: &str = "member.add";
const WAIT_LIST: &str = "wait-list.list";
const UNLIST: &str = "wait-list.remove";
const MAIL: &str = "wait-list.mail";
const MEMBERS: &str = "member.list";
const INVITATIONS: &str = "invitation.list";
const NAME: &str = "member.name";

/// Whether `tool` with `arguments` is about the workspace's people.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    matches!(tool, "tilo_do" | "tilo_read")
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| matches!(path.split('#').next(), Some(ROOT | WAIT_LIST_ROOT)))
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
            ("tilo_do", "workspace/members#add") => self.add(workspace, &args).await,
            ("tilo_do", "workspace/members#role") => self.role(workspace, &args).await,
            ("tilo_do", "workspace/members#remove") => self.remove(workspace, &args).await,
            ("tilo_do", "workspace/members#name") => self.name(workspace, &args).await,
            ("tilo_do", "workspace/members#wait-list") | ("tilo_read", WAIT_LIST_ROOT) => {
                self.wait_list(workspace, WAIT_LIST, Map::new()).await
            }
            ("tilo_do", "workspace/members#unlist") => match entry(&args) {
                Ok(entry) => {
                    let mut asked = Map::new();
                    asked.insert("entry".into(), json!(entry));
                    self.wait_list(workspace, UNLIST, asked).await
                }
                Err(outcome) => outcome,
            },
            ("tilo_do", "workspace/members#list") | ("tilo_read", ROOT) => {
                self.list(workspace).await
            }
            (_, other) => failed(
                "not_found",
                format!("the workspace has no action {other}"),
                "workspace/members#list, workspace/members#invite {role, email?, waiting?}, \
                 workspace/members#add {person, role, name?}, workspace/members#role {person, role}, \
                 workspace/members#remove {person}, workspace/members#name {name}, \
                 workspace/members#wait-list and workspace/members#unlist {entry} are its actions",
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
        self.govern_or(
            workspace,
            verb,
            arguments,
            "a key you created acts as you, as far as your role in the workspace goes; an \
             agent asks you first unless you allowed it in the workspace's settings, under Agents",
        )
        .await
    }

    /// [`Self::govern`], with `next` the step a refusal names.
    pub(super) async fn govern_or(
        &self,
        workspace: &str,
        verb: &str,
        arguments: Map<String, Value>,
        next: &str,
    ) -> Result<Value, Outcome> {
        let connection = self.connection().map_err(|error| refused(&error))?;
        let id = connection
            .server()
            .enter(connection, workspace)
            .await
            .map_err(|error| refused(&error))?;
        let target = RemoteActionTarget::workspace_governance(workspace, verb);
        connection
            .device
            .ask_host(id, target, arguments)
            .await
            .map_err(|error| {
                if error.code == DeviceRefusal::UNANSWERED {
                    failed("unavailable", error.message, "try again shortly")
                } else if error.code == DeviceRefusal::AWAITING_APPROVAL {
                    failed(
                        crate::v1::AWAITING_APPROVAL,
                        error.message,
                        "tell the person to approve it in their Tilo; it is done once they allow it",
                    )
                } else {
                    failed(
                        "not_permitted",
                        format!("the workspace refused: {}", error.message),
                        next,
                    )
                }
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
                "`role` is viewer, commenter, editor, administrator or owner",
                "pass args {role: \"editor\", email?: \"ops@example.com\"}",
            );
        };
        let waiting = match args.get("waiting") {
            None | Some(Value::Null) => None,
            Some(_) => match entry(&json!({"entry": args.get("waiting")})) {
                Ok(entry) => Some(entry),
                Err(outcome) => return outcome,
            },
        };
        let email = match text("email").filter(|email| !email.is_empty()) {
            Some(email) => match tilo_server_protocol::workspaces::contact_address(email) {
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
        if let Some(waiting) = &waiting {
            asked.insert("wait_list_entry".into(), json!(waiting));
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
        let Some(server) = self.server.upgrade() else {
            return failed(
                "unavailable",
                "the server is shutting down",
                "try again shortly",
            );
        };
        let link = server
            .invitation_link(workspace, &invitation, expires_at_ns)
            .await;
        let mut answered = json!({
            "workspace": workspace,
            "invite": issued.get("invite"),
            "role": role,
            "link": link,
            "expires_at_ns": expires_at_ns,
        });
        if let Some(waiting) = waiting {
            return self.mail_waiting(workspace, &waiting, answered).await;
        }
        if let (Some(email), Some(fields)) = (email, answered.as_object_mut()) {
            fields.insert(
                "next".into(),
                json!(format!(
                    "Tilo sends no email: give the person this link to send to {email}"
                )),
            );
            fields.insert("email".into(), json!(email));
            fields.insert("sent".into(), json!(false));
        }
        Outcome::Ok(answered)
    }

    /// Mail the invitation `answered` holds to the address `waiting` on
    /// `workspace`'s wait list, where its entry sends mail; where it sends
    /// none, the answer says so, with the link to hand over.
    async fn mail_waiting(&self, workspace: &str, waiting: &str, mut answered: Value) -> Outcome {
        let link = answered
            .get("link")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut asked = Map::new();
        asked.insert("entry".into(), json!(waiting));
        asked.insert("link".into(), json!(link));
        let (sent, next) = match self.govern(workspace, MAIL, asked).await {
            Ok(_) => (true, "the invitation is mailed to the address".to_owned()),
            Err(Outcome::Failed { message, .. }) => (
                false,
                format!("the invitation was not mailed ({message}): hand the link over yourself"),
            ),
            Err(outcome) => return outcome,
        };
        if let Some(fields) = answered.as_object_mut() {
            fields.insert("waiting".into(), json!(waiting));
            fields.insert("sent".into(), json!(sent));
            fields.insert("next".into(), json!(next));
        }
        Outcome::Ok(answered)
    }

    /// Hold the person `args.person` names in `workspace` at `args.role`:
    /// answers the people as the workspace holds them then.
    async fn add(&self, workspace: &str, args: &Value) -> Outcome {
        let text = |name: &str| args.get(name).and_then(Value::as_str).map(str::trim);
        let Some(person) = text("person").filter(|person| {
            person.len() == 64 && person.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) else {
            return failed(
                "invalid_arguments",
                "`person` is the person's user public key: 64 hex characters",
                "they read it with `tilo login --new-identity --json` (user_public_key)",
            );
        };
        let Some(role) = text("role").filter(|role| PLACES.contains(role)) else {
            return failed(
                "invalid_arguments",
                "`role` is viewer, commenter, editor, moderator, administrator or owner",
                "pass args {person: \"<64 hex>\", role: \"owner\", name?: \"Ada\"}",
            );
        };
        let mut asked = Map::new();
        asked.insert("person".into(), json!(person.to_ascii_lowercase()));
        asked.insert("role".into(), json!(role));
        if let Some(name) = text("name").filter(|name| !name.is_empty()) {
            if tilo_user_identity::display_name(name).is_err() {
                return failed(
                    "invalid_arguments",
                    "`name` is at most 128 bytes, no control characters",
                    "pass args {person, role, name: \"Ada Lovelace\"}",
                );
            }
            asked.insert("name".into(), json!(name));
        }
        match self.govern(workspace, ADD, asked).await {
            Ok(_) => self.list(workspace).await,
            Err(outcome) => outcome,
        }
    }

    /// Move the member `args.person` names to `args.role`: what adding them
    /// does to someone in already, said as what it is.
    async fn role(&self, workspace: &str, args: &Value) -> Outcome {
        let mut moved = Map::new();
        for name in ["person", "role"] {
            if let Some(value) = args.get(name) {
                moved.insert(name.into(), value.clone());
            }
        }
        self.add(workspace, &Value::Object(moved)).await
    }

    /// Remove the member `args.person` names, and every device of theirs:
    /// answers the people as the workspace holds them then.
    async fn remove(&self, workspace: &str, args: &Value) -> Outcome {
        let Some(person) = person_key(args) else {
            return failed(
                "invalid_arguments",
                "`person` is the member's user public key: 64 hex characters",
                "workspace/members#list names each member's key",
            );
        };
        let connection = match self.connection() {
            Ok(connection) => connection,
            Err(error) => return refused(&error),
        };
        match connection
            .server()
            .remove_member(connection, workspace, person)
            .await
        {
            Ok(()) => self.list(workspace).await,
            Err(Administration::Refused(why)) => failed(
                "not_permitted",
                format!("the workspace refused: {why}"),
                "its owners remove people, and a moderator removes those ranked below them",
            ),
            Err(Administration::Unanswered(why)) => failed("unavailable", why, "try again shortly"),
        }
    }

    /// The workspace's wait list, after `verb` with `asked`.
    async fn wait_list(&self, workspace: &str, verb: &str, asked: Map<String, Value>) -> Outcome {
        match self.govern(workspace, verb, asked).await {
            Ok(listed) => {
                let mut listed = listed;
                if let Some(fields) = listed.as_object_mut() {
                    fields.insert("workspace".into(), json!(workspace));
                }
                Outcome::Ok(listed)
            }
            Err(outcome) => outcome,
        }
    }

    /// How the key's person is named in `workspace` from now on, whatever
    /// device or key of theirs said it: the name the workspace's member list
    /// and the invitations they make show for them.
    async fn name(&self, workspace: &str, args: &Value) -> Outcome {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if tilo_user_identity::display_name(name).is_err() {
            return failed(
                "invalid_arguments",
                "`name` is the name the workspace shows for you: at most 128 bytes, no control characters",
                "pass args {name: \"Ada Lovelace\"}",
            );
        }
        let mut asked = Map::new();
        asked.insert("name".into(), json!(name));
        match self.govern(workspace, NAME, asked).await {
            // The people as the workspace names them now, which is how a
            // caller sees the name it just set take.
            Ok(_) => self.list(workspace).await,
            Err(outcome) => outcome,
        }
    }
}

/// The user public key `args.person` names.
fn person_key(args: &Value) -> Option<[u8; 32]> {
    let person = args.get("person").and_then(Value::as_str)?.trim();
    hex::decode(person).ok()?.try_into().ok()
}

/// The wait-list entry `args.entry` names, lowercase hex.
fn entry(args: &Value) -> Result<String, Outcome> {
    args.get("entry")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|entry| entry.len() == 64 && entry.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            failed(
                "invalid_arguments",
                "an address on the wait list is named by its entry: 64 hex characters",
                "workspace/members#wait-list names each address's entry",
            )
        })
}

impl crate::Server {
    /// The link that brings whoever follows it into `workspace` with
    /// `invitation` (its signed bytes), lasting until `expires_at_ns`: sealed
    /// onto the site's shelf, as a person's shell hands one over, or the
    /// invitation's own text after the workspace's route where the site
    /// keeps nothing. Either opens the site's sign-in and joins.
    pub(crate) async fn invitation_link(
        &self,
        workspace: &str,
        invitation: &[u8],
        expires_at_ns: u64,
    ) -> String {
        // A server reached only over iroh has no site a stranger can fetch
        // from: the link carries the signed invitation itself, whose
        // addresses are the server's own, signed by it, and the page or
        // `tilo login` that follows it dials the server with them.
        if let Some(entry) = &self.config.invite_entry {
            let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(invitation);
            return format!(
                "{}/#/w/{workspace}?invite={text}",
                entry.trim_end_matches('/')
            );
        }
        let entry = self.config.web_entry.trim_end_matches('/');
        // The site's shelf keeps the origin the invitation admits into, never a
        // bare name: a shelf refuses what is not an origin.
        let (host, origin) = self.hosts.first_named();
        let shelved = match (
            tilo_workspace_directory::client(),
            tilo_workspace_directory::web_origin(&format!("{entry}/")),
        ) {
            (Ok(client), Ok(site)) => {
                tilo_workspace_directory::shelve(
                    &client,
                    &site,
                    origin.as_deref(),
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
                // The route names the server, as the site's own workspace
                // links do: without it the page asks the site itself for
                // the workspace's host, which it is not.
                let host = host.unwrap_or_default();
                format!("{entry}/#/@{host}/w/{workspace}?invite={text}")
            },
            |shelved| shelved.link,
        )
    }
}
