//! The changes to who is in a workspace a key's own device signs: adding a
//! person by their key (`workspace/members#add {person, role, name?}`),
//! moving a member to another role (`workspace/members#role {person, role}`)
//! and removing one (`workspace/members#remove {person}`). Listing the
//! people, inviting and the wait list are the workspace's own resources
//! (`workspace/members`, `workspace/invitations`, `workspace/waiting`),
//! which its host answers as any path.
//!
//! A key that is a device of its person's (a computer `tilo login` made one)
//! signs these changes itself, as their desktop and their page do, once the
//! host says the change is its to make; any other key has the host make them
//! as far as its person's place and its grants let it.

use super::Session;
use crate::account::{Outcome, answer, failed, refused};
use crate::server::Administration;
use serde_json::{Map, Value, json};
use tilo_actions::remote::RemoteActionTarget;
use tilo_session_client::device::DeviceRefusal;
use tilo_session_client::membership::{MemberPlace, Membership};

/// The changes this path makes, at the path of a workspace's people.
const CHANGES: [&str; 3] = [
    "workspace/members#add",
    "workspace/members#role",
    "workspace/members#remove",
];
/// The places a key adds a person at.
const PLACES: [&str; 6] = [
    "viewer",
    "commenter",
    "editor",
    "moderator",
    "administrator",
    "owner",
];
/// The workspace's governance actions this path asks.
const ADD: &str = "member.add";
const MEMBERS: &str = "member.list";
const INVITATIONS: &str = "invitation.list";
/// The workspace's governance verb that removes a member.
const REMOVE: &str = "remove-member";
/// The step a refused change of who is in the workspace names.
const OWN_NEXT: &str = "its owner makes and unmakes its administrators and hands it on, from a \
                        device of their own; an administrator gives every other place";

/// Whether `tool` with `arguments` changes who is in the workspace.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    tool == "tilo_do"
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| CHANGES.contains(&path))
}

impl Session {
    /// Answer the change `arguments` asks of `workspace`'s people, typed when
    /// `structured`.
    pub(super) async fn members(
        &self,
        workspace: &str,
        arguments: &Value,
        structured: bool,
    ) -> Value {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = arguments.get("args").cloned().unwrap_or_else(|| json!({}));
        let outcome = match path {
            "workspace/members#add" => self.add(workspace, &args).await,
            "workspace/members#role" => self.role(workspace, &args).await,
            _ => self.remove(workspace, &args).await,
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
                        "a key you created acts as you, as far as your role in the workspace \
                         goes; an agent asks you first unless you allowed it in the \
                         workspace's settings, under Agents",
                    )
                }
            })
    }

    /// The people in `workspace` and its open invitations, as a change answers them.
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
        let key = hex::decode(person)
            .ok()
            .and_then(|key| <[u8; 32]>::try_from(key).ok());
        if let (Some(place), Some(key)) = (MemberPlace::parse(role), key) {
            let name = asked.get("name").and_then(Value::as_str);
            if let Some(signed) = self
                .sign_own(workspace, OwnChange::Place(key, place, name), &asked)
                .await
            {
                return match signed {
                    Ok(()) => self.list(workspace).await,
                    Err(outcome) => outcome,
                };
            }
        }
        match self.govern(workspace, ADD, asked).await {
            Ok(_) => self.list(workspace).await,
            Err(outcome) => outcome,
        }
    }

    /// Write `change` as the key's own device, when the key is one: a device of its person's,
    /// which signs their changes to the workspace's governance itself, as their desktop and
    /// their page do, and the workspace's host relays it. `None` for a key made to act for them
    /// (an agent's grant, a server key), whose changes the host makes after asking the person
    /// as far as their grants let it: such a key governs no workspace itself.
    ///
    /// The host is asked first, with the same verb and `asked`, whether the change is the
    /// device's to make: by its person's place in the workspace and, as the session of a
    /// computer `tilo login` made a device of is its agent's run, by that agent's grants there,
    /// which ask the person before it where they say so. Only then does the device sign it.
    async fn sign_own(
        &self,
        workspace: &str,
        change: OwnChange<'_>,
        asked: &Map<String, Value>,
    ) -> Option<Result<(), Outcome>> {
        let connection = match self.connection() {
            Ok(connection) => connection,
            Err(error) => return Some(Err(refused(&error))),
        };
        let capabilities = &connection.device.cert().capabilities;
        if capabilities.agent.is_some() || capabilities.stand_in || capabilities.server_key {
            return None;
        }
        let verb = match change {
            OwnChange::Place(..) => ADD,
            OwnChange::Remove(_) => REMOVE,
        };
        let mut asked = asked.clone();
        asked.insert(
            tilo_server_session::workspaces::SIGNED_BY_CALLER.into(),
            json!(true),
        );
        if let Err(outcome) = admitted(connection, workspace, verb, asked).await {
            return Some(Err(outcome));
        }
        Some(sign_as(connection, workspace, change).await)
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
        let mut asked = Map::new();
        asked.insert("person".into(), json!(hex::encode(person)));
        if let Some(signed) = self
            .sign_own(workspace, OwnChange::Remove(person), &asked)
            .await
        {
            return match signed {
                Ok(()) => self.list(workspace).await,
                Err(outcome) => outcome,
            };
        }
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
}

/// A change the key's own device signs ([`Session::sign_own`]).
enum OwnChange<'a> {
    /// The person, the place, and the name the workspace calls them by.
    Place([u8; 32], MemberPlace, Option<&'a str>),
    /// The person to remove.
    Remove([u8; 32]),
}

/// Whether the workspace's host admits `verb` with `asked` (which says the caller signs it,
/// [`tilo_server_session::workspaces::SIGNED_BY_CALLER`]) for `connection`: waited for while the
/// host asks the person, since the device signs the change only once they allowed it. The host
/// answers at once that a question waits on them, and the same invocation asked again is
/// followed until they answer.
async fn admitted(
    connection: &crate::server::Connection,
    workspace: &str,
    verb: &str,
    asked: Map<String, Value>,
) -> Result<(), Outcome> {
    let id = connection
        .server()
        .enter(connection, workspace)
        .await
        .map_err(|error| refused(&error))?;
    let target = RemoteActionTarget::workspace_governance(workspace, verb);
    let invocation = tilo_actions::InvocationId::new();
    let mut asked_again = false;
    loop {
        let answer = connection
            .device
            .ask_server_as(id, target.clone(), asked.clone(), invocation)
            .await;
        match answer {
            Ok(_) => return Ok(()),
            Err(error) if error.code == DeviceRefusal::AWAITING_APPROVAL && !asked_again => {
                asked_again = true;
            }
            Err(error) if error.code == DeviceRefusal::AWAITING_APPROVAL => {
                return Err(failed(
                    crate::v1::AWAITING_APPROVAL,
                    error.message,
                    "the person did not answer in time; ask again once they can",
                ));
            }
            Err(error) if error.code == DeviceRefusal::UNANSWERED => {
                return Err(failed("unavailable", error.message, "try again shortly"));
            }
            Err(error) => {
                return Err(failed(
                    "not_permitted",
                    format!("the workspace refused: {}", error.message),
                    OWN_NEXT,
                ));
            }
        }
    }
}

/// Write `change` to `workspace`'s governance as `connection`'s device, entering the workspace
/// first, and wait for its host to hold it: what the workspace refused, as the refusal.
async fn sign_as(
    connection: &crate::server::Connection,
    workspace: &str,
    change: OwnChange<'_>,
) -> Result<(), Outcome> {
    let refusal = |why: String| {
        failed(
            "not_permitted",
            format!("the workspace refused: {why}"),
            OWN_NEXT,
        )
    };
    let id = connection
        .server()
        .enter(connection, workspace)
        .await
        .map_err(|error| refused(&error))?;
    if !connection.device.admitted(id).await {
        return Err(failed(
            "unavailable",
            "the workspace has not let this device in yet",
            "try again shortly",
        ));
    }
    let link = connection.device.session();
    let membership = Membership::of(link.engine(), id);
    match change {
        OwnChange::Place(person, place, name) => membership.place(person, place, name).await,
        OwnChange::Remove(person) => membership.remove(person).await,
    }
    .map_err(refusal)?;
    membership.settled(link).await.map_err(refusal)
}

/// The user public key `args.person` names.
fn person_key(args: &Value) -> Option<[u8; 32]> {
    let person = args.get("person").and_then(Value::as_str)?.trim();
    hex::decode(person).ok()?.try_into().ok()
}
