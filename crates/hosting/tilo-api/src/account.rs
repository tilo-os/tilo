//! The account: the resource that owns a person's workspaces and keys, as
//! an agent or program reaches it through the server with a grant reaching all
//! of them.
//!
//! It is addressed like any resource, by path, through the same tools:
//!
//! - `account/workspaces#list`, `#create {name, host?}`, `#delete {workspace}`;
//! - `account/keys#list`, `#create {name, workspace?, expires_in?}`,
//!   `#mint {workspace, name?, expires_in?}`, `#revoke {grant}`.
//!
//! `tilo_ls account` lists them, `tilo_read` reads the lists, `tilo_do` runs
//! an action, and `tilo_status` follows one that waits for the person. Making
//! a workspace is the relay's own operation ([`crate::relay`]). Minting a
//! key is a certificate the grant's own key signs for one workspace
//! ([`tilo_user_identity::prepare_minted_grant`]): narrower than the grant,
//! ending no later than it and within 90 days, and revoked with it. Creating
//! a key asks the person, whose device certifies it ([`crate::keys`]); one
//! reaching all their workspaces lasts until revoked unless `expires_in`
//! says otherwise. The run waits for them, and `tilo_status` answers the key
//! once they approve. Deleting a workspace is the relay's own operation too:
//! final, it takes everything the host keeps of it, tells whoever is in it,
//! and revokes every key that reached only it. The call is the confirmation:
//! nothing asks the person again, as revoking a key asks nothing.
//!
//! Answers are the agent surface's text (`ok …`, `failed: …`,
//! `pending <id>: …`), or, for a caller that asks for typed answers, the
//! shapes `docs/developers/sdk-wire.md` gives every tool.

use crate::grant_key::GrantKey;
use crate::relay::{Connection, RelayError};
use crate::store::{Grant, GrantKind, Scope};
use crate::{Server, now_ms};
use serde_json::{Value, json};
use std::fmt::Write as _;
use tilo_user_identity::AgentScope;

/// The account's root path.
pub const ROOT: &str = "account";
/// An invocation that waits for the person to approve a key, followed with
/// `tilo_status`: this prefix and the poll secret.
const KEY_INVOCATION: &str = "account-key-";

/// Whether a tool call with `arguments` and no workspace is for the account.
#[must_use]
pub fn addresses(tool: &str, arguments: &Value) -> bool {
    if tool == "tilo_status" {
        return arguments
            .get("invocation_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(KEY_INVOCATION));
    }
    let path = arguments.get("path").and_then(Value::as_str).unwrap_or("");
    let first = path.split(['/', '#']).next().unwrap_or("");
    match tool {
        "tilo_ls" => path.is_empty() || first == ROOT,
        "tilo_read" | "tilo_do" => first == ROOT,
        _ => false,
    }
}

/// The answer to a call of a key for one workspace that addresses the
/// account: it reaches none, and is told so rather than that the workspace
/// runs no app called `account`. `None` for a call in its workspace.
pub(crate) fn beyond_workspace(tool: &str, arguments: &Value, structured: bool) -> Option<Value> {
    let path = arguments.get("path").and_then(Value::as_str).unwrap_or("");
    (!path.is_empty() && addresses(tool, arguments)).then(|| {
        answer(
            structured,
            path,
            failed(
                "forbidden",
                "this key reaches one workspace and no account",
                "account paths need a key reaching all your workspaces",
            ),
        )
    })
}

/// What an account action answered.
pub(crate) enum Outcome {
    Ok(Value),
    Pending {
        invocation: String,
        ask: String,
    },
    Failed {
        code: &'static str,
        message: String,
        next: String,
        /// How long to wait before the same call works, when the host said.
        retry_after_ms: Option<u64>,
    },
}

/// The code of an answer that ran nothing because the host is busy with the
/// person's changes: the same call works after its `retry_after_ms`.
pub(crate) const RATE_LIMITED: &str = "rate_limited";

pub(crate) fn failed(
    code: &'static str,
    message: impl Into<String>,
    next: impl Into<String>,
) -> Outcome {
    Outcome::Failed {
        code,
        message: message.into(),
        next: next.into(),
        retry_after_ms: None,
    }
}

pub(crate) fn refused(error: &RelayError) -> Outcome {
    match error {
        RelayError::NotFound => failed(
            "not_found",
            "no such workspace of yours",
            "tilo_do account/workspaces#list names your workspaces",
        ),
        RelayError::Refused(reason) => failed(
            "refused",
            format!("the host refused: {reason}"),
            "the reason says what the host needs (a free workspace, payment); try again once it holds",
        ),
        RelayError::Unavailable(reason) => {
            failed("unavailable", reason.clone(), "try again shortly")
        }
        RelayError::RateLimited { retry_after_ms } => Outcome::Failed {
            code: RATE_LIMITED,
            message: "the host takes your changes more slowly; nothing ran".to_owned(),
            next: format!("wait {retry_after_ms} ms and retry the same call"),
            retry_after_ms: Some(*retry_after_ms),
        },
    }
}

/// A tool's result: the typed value as JSON text when the caller asked for
/// typed answers (`structured`), else the agents' `text`.
fn tool_result(structured: bool, text: &str, typed: &Value, is_error: bool) -> Value {
    if structured {
        json!({
            "content": [{"type": "text", "text": typed.to_string()}],
            "structuredContent": typed,
            "isError": is_error,
        })
    } else {
        json!({"content": [{"type": "text", "text": text}], "isError": is_error})
    }
}

/// The result of running `path`, as `tilo_do` and `tilo_status` answer it.
pub(crate) fn answer(structured: bool, path: &str, outcome: Outcome) -> Value {
    match outcome {
        Outcome::Ok(result) => tool_result(
            structured,
            &format!("ok\n{result}"),
            &json!({"status": "ok", "path": path, "result": result}),
            false,
        ),
        Outcome::Pending { invocation, ask } => tool_result(
            structured,
            &format!(
                "pending {invocation}: the person approves it at {ask}; tilo_status {{invocation_id}} answers once they did"
            ),
            &json!({"status": "pending", "path": path, "invocation_id": invocation,
                   "ask": {"invocation_id": invocation, "url": ask}}),
            false,
        ),
        Outcome::Failed {
            code,
            message,
            next,
            retry_after_ms,
        } => {
            let mut error = json!({"code": code, "message": message, "next_steps": [next]});
            if let (Some(fields), Some(wait)) = (error.as_object_mut(), retry_after_ms) {
                fields.insert("retry_after_ms".into(), json!(wait));
            }
            tool_result(
                structured,
                &format!("failed: {code}: {message}. Next: {next}"),
                &json!({"status": "failed", "path": path, "error": error}),
                true,
            )
        }
    }
}

/// A parameter of an account action, as `tilo_ls` lists parameters.
struct Param {
    name: &'static str,
    required: bool,
    ty: &'static str,
}

const fn param(name: &'static str, required: bool, ty: &'static str) -> Param {
    Param { name, required, ty }
}

/// The account's actions: (resource, action, label, kind, parameters).
const ACTIONS: [(&str, &str, &str, &str, &[Param]); 7] = [
    (
        "workspaces",
        "list",
        "Your workspaces, and the hosts you can create on",
        "read",
        &[],
    ),
    (
        "workspaces",
        "create",
        "Create a workspace; the host may refuse (quota, payment)",
        "outside",
        &[param("name", true, "Text"), param("host", false, "Text")],
    ),
    (
        "workspaces",
        "delete",
        "Delete a workspace of yours and everything in it, for good; its keys are revoked",
        "outside",
        &[param("workspace", true, "Text")],
    ),
    (
        "keys",
        "list",
        "This key and the keys it minted",
        "read",
        &[],
    ),
    (
        "keys",
        "create",
        "Ask the person for a new key; it waits for them",
        "outside",
        &[
            param("name", true, "Text"),
            param("workspace", false, "Text"),
            param("expires_in", false, "Integer"),
            param("browser_pages", false, "Boolean"),
        ],
    ),
    (
        "keys",
        "mint",
        "A key for one workspace, for an agent there",
        "outside",
        &[
            param("workspace", true, "Text"),
            param("name", false, "Text"),
            param("expires_in", false, "Integer"),
        ],
    ),
    (
        "keys",
        "revoke",
        "Revoke this key or one it minted, and every key it minted",
        "outside",
        &[param("grant", true, "Text")],
    ),
];

/// The action rows of the account's `resource`, as `tilo_ls` types them,
/// and the same as text lines.
fn actions_of(resource: &str) -> (Vec<Value>, String) {
    let mut text = String::new();
    let rows = ACTIONS
        .iter()
        .filter(|row| row.0 == resource)
        .map(|(_, action, label, kind, params)| {
            let names: Vec<String> = params
                .iter()
                .map(|param| format!("{}{}", param.name, if param.required { "" } else { "?" }))
                .collect();
            let _ = write!(text, "\n  #{action}({})  {label}", names.join(", "));
            json!({
                "id": action,
                "label": label,
                "kind": kind,
                "params": params.iter().map(|param| json!({
                    "name": param.name, "ty": param.ty, "required": param.required,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    (rows, text)
}

pub(crate) fn grant_row(grant: &Grant) -> Value {
    json!({
        "grant": grant.id,
        "name": grant.client_name,
        "scope": grant.scope.name(),
        "kind": match grant.kind {
            GrantKind::Agent => "agent",
            GrantKind::Key => "key",
            GrantKind::Minted { .. } => "minted",
        },
        "created_at_ms": grant.created_ms,
        "created_by": grant.created_by,
        "browser_pages": grant.browser_pages,
        "last_used_at_ms": grant.last_used_ms,
        "expires_at_ms": grant.expires_ms,
    })
}

impl Server {
    /// Answer `tool` with `arguments` on the account, as the grant `grant`
    /// in its run `session`, typed when `structured`.
    pub(crate) async fn account_call(
        &self,
        grant: &str,
        session: &crate::session::Session,
        tool: &str,
        arguments: &Value,
        structured: bool,
    ) -> Value {
        let path = arguments.get("path").and_then(Value::as_str).unwrap_or("");
        let Some(grant) = self.store.grant(grant) else {
            return answer(
                structured,
                path,
                failed(
                    "key_revoked",
                    "this key was revoked",
                    "ask the person for a new key",
                ),
            );
        };
        let key = session.connection();
        match tool {
            "tilo_ls" => self.account_ls(key, path, structured).await,
            "tilo_read" => self.account_read(&grant, key, path, structured).await,
            "tilo_status" => {
                let id = arguments
                    .get("invocation_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                answer(structured, "account/keys#create", self.key_status(id))
            }
            _ => {
                let args = arguments.get("args").cloned().unwrap_or_else(|| json!({}));
                let outcome = self.account_do(&grant, key, path, &args).await;
                answer(structured, path, outcome)
            }
        }
    }

    /// `tilo_ls` of the account, or of its workspaces.
    async fn account_ls(&self, key: &Connection, path: &str, structured: bool) -> Value {
        match path.trim_end_matches('/') {
            "" | ROOT => {
                let mut text = String::from("account: your workspaces and keys");
                let mut rows = Vec::new();
                for (resource, label) in [
                    ("workspaces", "Your workspaces"),
                    ("keys", "Keys to your workspaces"),
                    ("usage", "What your workspaces keep and ran"),
                ] {
                    let (actions, lines) = actions_of(resource);
                    let _ = write!(text, "\n{ROOT}/{resource}  {label}{lines}");
                    rows.push(json!({
                        "path": format!("{ROOT}/{resource}"), "name": resource, "label": label,
                        "kind": format!("account.{resource}"), "has_children": resource == "workspaces",
                        "actions": actions,
                    }));
                }
                tool_result(
                    structured,
                    &text,
                    &json!({"path": ROOT, "kind": "account", "rows": rows, "actions": []}),
                    false,
                )
            }
            "account/workspaces" => match self.account_workspaces(key).await {
                Outcome::Ok(listed) => {
                    let mut text = String::from("account/workspaces:");
                    let mut rows = Vec::new();
                    for workspace in listed
                        .get("workspaces")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let (id, name) = (
                            workspace["id"].as_str().unwrap_or_default(),
                            workspace["name"].as_str().unwrap_or_default(),
                        );
                        let _ = write!(
                            text,
                            "\n{name} (id {id}, on {})",
                            workspace["host"].as_str().unwrap_or_default()
                        );
                        rows.push(json!({
                            "path": format!("account/workspaces/{id}"), "name": id, "label": name,
                            "kind": "account.workspace", "has_children": false, "actions": [],
                        }));
                    }
                    let (actions, lines) = actions_of("workspaces");
                    text.push_str(&lines);
                    tool_result(
                        structured,
                        &text,
                        &json!({"path": "account/workspaces", "kind": "account.workspaces",
                               "rows": rows, "actions": actions}),
                        false,
                    )
                }
                other => answer(structured, path, other),
            },
            "account/keys" => {
                let (actions, lines) = actions_of("keys");
                tool_result(
                    structured,
                    &format!("account/keys:{lines}"),
                    &json!({"path": "account/keys", "kind": "account.keys", "rows": [], "actions": actions}),
                    false,
                )
            }
            other => answer(
                structured,
                other,
                failed(
                    "not_found",
                    format!("no {other} in the account"),
                    "tilo_ls account lists what it holds",
                ),
            ),
        }
    }

    /// `tilo_read` of the account's workspaces or keys.
    async fn account_read(
        &self,
        grant: &Grant,
        key: &Connection,
        path: &str,
        structured: bool,
    ) -> Value {
        let path = path.trim_end_matches('/');
        let (kind, fields) = match path {
            "account/workspaces" => match self.account_workspaces(key).await {
                Outcome::Ok(listed) => ("account.workspaces", listed),
                other => return answer(structured, path, other),
            },
            "account/keys" => match self.account_keys(grant) {
                Outcome::Ok(listed) => ("account.keys", listed),
                other => return answer(structured, path, other),
            },
            "account/usage" => match self.relay.account(key).await {
                Ok(account) => ("account.usage", account),
                Err(error) => return answer(structured, path, refused(&error)),
            },
            other => {
                return answer(
                    structured,
                    other,
                    failed(
                        "not_found",
                        format!("{other} is nothing to read"),
                        "tilo_read account/workspaces, account/keys or account/usage",
                    ),
                );
            }
        };
        let mut text = format!("{path}:");
        for row in fields
            .pointer("/standing/usage/workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let _ = write!(
                text,
                "\n{}: {} s of VM time{}, {} bytes kept",
                row["workspace"].as_str().unwrap_or_default(),
                row["vm_seconds"].as_u64().unwrap_or_default(),
                if row["running"] == true {
                    ", running"
                } else {
                    ""
                },
                row["stored_bytes"].as_u64().unwrap_or_default(),
            );
        }
        for row in fields
            .get("workspaces")
            .or_else(|| fields.get("keys"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let when = |field: &str, none: &str| {
                row[field]
                    .as_u64()
                    .map_or_else(|| none.to_owned(), crate::pages::date)
            };
            let _ = match row.get("grant") {
                Some(grant) => write!(
                    text,
                    "\n{} (grant {}, reaches {}, {}, last used {}, expires {})",
                    row["name"].as_str().unwrap_or_default(),
                    grant.as_str().unwrap_or_default(),
                    row["scope"].as_str().unwrap_or_default(),
                    row["kind"].as_str().unwrap_or_default(),
                    when("last_used_at_ms", "never"),
                    when("expires_at_ms", "never"),
                ),
                None => write!(
                    text,
                    "\n{} (id {}, on {})",
                    row["name"].as_str().unwrap_or_default(),
                    row["id"].as_str().unwrap_or_default(),
                    row["host"].as_str().unwrap_or_default(),
                ),
            };
        }
        tool_result(
            structured,
            &text,
            &json!({"path": path, "kind": kind, "fields": fields, "children": []}),
            false,
        )
    }

    async fn account_do(
        &self,
        grant: &Grant,
        key: &Connection,
        path: &str,
        args: &Value,
    ) -> Outcome {
        let field = |name: &str| {
            args.get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        match path {
            "account/workspaces#list" => self.account_workspaces(key).await,
            "account/workspaces#create" => self.create_workspace(key, args).await,
            "account/workspaces#delete" => {
                let Some(workspace) = field("workspace") else {
                    return failed(
                        "invalid_arguments",
                        "`workspace` is required",
                        "pass args {workspace: \"<id>\"}; account/workspaces#list names them",
                    );
                };
                if let Err(error) = self.relay.delete(key, workspace).await {
                    return refused(&error);
                }
                // Every key that reached only that workspace goes with it.
                let revoked = self.revoke_keys_for(&grant.person, workspace).await;
                Outcome::Ok(json!({"deleted": workspace, "revoked_keys": revoked}))
            }
            "account/keys#list" => self.account_keys(grant),
            "account/keys#create" => {
                let asked = crate::keys::expires_in_of(args.get("expires_in"))
                    .and_then(|expires_in| {
                        crate::keys::browser_pages_of(args.get("browser_pages"))
                            .map(|browser_pages| (expires_in, browser_pages))
                    })
                    .and_then(|(expires_in, browser_pages)| {
                        self.ask_for_key(
                            field("name"),
                            field("workspace"),
                            expires_in,
                            browser_pages,
                        )
                    });
                match asked {
                    Ok((invocation, ask)) => Outcome::Pending { invocation, ask },
                    Err(why) => failed(
                        "invalid_arguments",
                        why,
                        "pass args {name, workspace?, expires_in?, browser_pages?}",
                    ),
                }
            }
            "account/keys#mint" => self.mint(grant, key, args).await,
            "account/keys#revoke" => {
                let Some(id) = field("grant") else {
                    return failed(
                        "invalid_arguments",
                        "`grant` is required",
                        "account/keys#list names them",
                    );
                };
                let own = id == grant.id
                    || self
                        .store
                        .grant(id)
                        .is_some_and(|found| found.parent() == Some(grant.id.as_str()));
                if !own {
                    return failed(
                        "not_found",
                        "no such key of this key",
                        "account/keys#list names them",
                    );
                }
                match self.revoke_grant(&grant.person, id).await {
                    Ok(_) => Outcome::Ok(json!({"revoked": id})),
                    Err(_) => failed(
                        "unavailable",
                        "revocation is unavailable",
                        "try again shortly",
                    ),
                }
            }
            other => failed(
                "not_found",
                format!("the account has no action {other}"),
                "tilo_ls account lists its actions",
            ),
        }
    }

    /// Make the workspace `args` names (`name`, and `id` when the caller chose
    /// one) as `key`.
    async fn create_workspace(&self, key: &Connection, args: &Value) -> Outcome {
        let Some(name) = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            return failed(
                "invalid_arguments",
                "`name` is required",
                "pass args {name: \"Applicant: Jane Doe\"}",
            );
        };
        let id = match chosen_id(args) {
            Ok(id) => id,
            Err(refused) => return refused,
        };
        match self.relay.create(key, name, id).await {
            Ok(created) => Outcome::Ok(json!({"workspace": created})),
            Err(error) => refused(&error),
        }
    }

    /// Revoke every live key of `person` that reaches only `workspace`,
    /// deleted: their sessions end now. Answers the grants revoked.
    async fn revoke_keys_for(&self, person: &str, workspace: &str) -> Vec<String> {
        let mut revoked = Vec::new();
        for grant in self.store.grants_of(person) {
            if grant.scope != Scope::Workspace(workspace.to_owned()) {
                continue;
            }
            match self.revoke_grant(person, &grant.id).await {
                Ok(true) => revoked.push(grant.id),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, grant = %grant.id, "a key of a deleted workspace stays");
                }
            }
        }
        revoked
    }

    async fn account_workspaces(&self, key: &Connection) -> Outcome {
        match self.relay.workspaces(key).await {
            Ok(workspaces) => Outcome::Ok(json!({
                "workspaces": workspaces,
                "hosts": [{"name": self.relay.name(), "default": true}],
            })),
            Err(error) => refused(&error),
        }
    }

    fn account_keys(&self, grant: &Grant) -> Outcome {
        let mut keys = vec![grant_row(grant)];
        keys.extend(self.store.minted_by(&grant.id).iter().map(grant_row));
        Outcome::Ok(json!({"keys": keys}))
    }

    /// How a key the person was asked for went.
    fn key_status(&self, invocation: &str) -> Outcome {
        let poll = invocation.strip_prefix(KEY_INVOCATION).unwrap_or_default();
        match self.key_answer(poll) {
            crate::keys::Answer::Approved { token, grant } => Outcome::Ok(json!({
                "key": token,
                "grant": grant.id,
                "name": grant.client_name,
                "scope": grant.scope.name(),
                "expires_at_ms": grant.expires_ms,
            })),
            crate::keys::Answer::Declined => failed(
                "declined",
                "the person declined the key",
                "ask again only if they want it",
            ),
            crate::keys::Answer::Waiting { ask } => Outcome::Pending {
                invocation: invocation.to_owned(),
                ask,
            },
            crate::keys::Answer::Expired => failed(
                "expired",
                "nothing waits under this id: it expired, or the key was already answered",
                "ask again with account/keys#create",
            ),
        }
    }

    /// Ask the person for a key: the invocation that waits for them, and
    /// where they approve it.
    fn ask_for_key(
        &self,
        name: Option<&str>,
        workspace: Option<&str>,
        expires_in: Option<u64>,
        browser_pages: bool,
    ) -> Result<(String, String), &'static str> {
        let (poll, ask) = self.request_key(name, workspace, expires_in, browser_pages)?;
        Ok((format!("{KEY_INVOCATION}{poll}"), ask))
    }

    /// Mint a key reaching one workspace, for an agent to act there.
    async fn mint(&self, grant: &Grant, connection: &Connection, args: &Value) -> Outcome {
        if grant.scope != Scope::Account {
            return failed(
                "forbidden",
                "only a key reaching all your workspaces mints keys",
                "mint with the key you created",
            );
        }
        let Some(workspace) = args.get("workspace").and_then(Value::as_str) else {
            return failed(
                "invalid_arguments",
                "`workspace` is required",
                "account/workspaces#list names them",
            );
        };
        let Some(AgentScope::Workspace(id)) = AgentScope::parse(workspace) else {
            return failed(
                "invalid_arguments",
                "`workspace` is a workspace's id",
                "account/workspaces#list names them",
            );
        };
        let found = match self.relay.workspaces(connection).await {
            Ok(listed) => listed.into_iter().find(|listed| listed.id == workspace),
            Err(error) => return refused(&error),
        };
        let Some(found) = found else {
            return refused(&RelayError::NotFound);
        };
        let key = &connection.key;
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(
                || format!("{}: {}", grant.client_name, found.name),
                str::to_owned,
            );
        let name: String = name
            .chars()
            .filter(|character| !character.is_control())
            .take(crate::keys::MAX_NAME_CHARS)
            .collect();
        let now = now_ms();
        let minted = match mint_key(
            key,
            id,
            &name,
            args.get("expires_in").and_then(Value::as_u64),
            now,
        ) {
            Ok(minted) => minted,
            Err(outcome) => return outcome,
        };
        let kind = GrantKind::Minted {
            parent: grant.id.clone(),
        };
        match self.store.record_key(&minted, &name, kind, None, now) {
            Ok((minted, token)) => Outcome::Ok(json!({
                "grant": minted.id,
                "name": minted.client_name,
                "token": token,
                "workspace": workspace,
                "mcp_url": format!("{}/w/{workspace}/mcp", self.config.issuer),
                "expires_at_ms": minted.expires_ms,
            })),
            Err(_) => failed(
                "unavailable",
                "the key could not be kept",
                "try again shortly",
            ),
        }
    }
}

/// The id a caller chose for a workspace it makes (`id`), so that a retry
/// asks for the same one: none when it chose none.
fn chosen_id(args: &Value) -> Result<Option<uuid::Uuid>, Outcome> {
    args.get("id")
        .and_then(Value::as_str)
        .map(uuid::Uuid::parse_str)
        .transpose()
        .map_err(|_| {
            failed(
                "invalid_arguments",
                "`id` is a workspace id (a UUID)",
                "leave `id` out to make a new workspace",
            )
        })
}

/// A key for the workspace `id` alone, named `name`, certified by `key`: a
/// grant for one workspace that ends after `expires_in` seconds, with `key`,
/// or after the most a grant may last, whichever is soonest. A server key
/// that never expires still mints keys that do.
fn mint_key(
    key: &GrantKey,
    id: [u8; 16],
    name: &str,
    expires_in: Option<u64>,
    now: u64,
) -> Result<GrantKey, Outcome> {
    let now_ns = now.saturating_mul(1_000_000);
    let most = now_ns.saturating_add(tilo_user_identity::MAX_AGENT_GRANT_LIFETIME_NS);
    let limit = key
        .cert()
        .expires_at_ns
        .map_or(most, |parent| parent.min(most));
    let expires_ns = expires_in.map_or(limit, |seconds| {
        now_ns
            .saturating_add(seconds.saturating_mul(1_000_000_000))
            .min(limit)
    });
    if expires_ns <= now_ns {
        return Err(failed(
            "expired",
            "this key has expired",
            "ask the person for a new key",
        ));
    }
    let minted_key = GrantKey::generate().map_err(|_| {
        failed(
            "unavailable",
            "no randomness is available",
            "try again shortly",
        )
    })?;
    let mut cert = tilo_user_identity::prepare_minted_grant(
        key.cert(),
        minted_key.verifying_key().to_bytes(),
        id,
        name,
        now_ns,
        expires_ns,
    );
    tilo_user_identity::sign_device_cert(&mut cert, key.key()).map_err(|error| {
        failed(
            "forbidden",
            format!("this key cannot mint: {error}"),
            "ask the person for a new key",
        )
    })?;
    let browser_pages = cert.capabilities.browser_pages;
    GrantKey::new(
        minted_key,
        cert,
        AgentScope::Workspace(id),
        false,
        browser_pages,
        now_ns,
    )
    .map_err(|error| failed("forbidden", error.0, "ask the person for a new key"))
}
