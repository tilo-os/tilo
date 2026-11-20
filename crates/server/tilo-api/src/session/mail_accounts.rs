//! The person's mail accounts, as the path `person/mail-accounts` of every
//! workspace a key acts in.
//!
//! - `person/mail-accounts#list` (or `tilo_read person/mail-accounts`) answers
//!   the mail accounts the person's vault keeps on the workspace's server,
//!   each with its servers and whether its sign-in is kept, never the sign-in;
//! - `person/mail-accounts#add {address, displayName, username, incomingHost,
//!   incomingPort, outgoingHost, outgoingPort, password, identities}` has that
//!   server sign in to the account with `password`, and only once its server
//!   took it, keep the account and seal the password in the person's vault.
//!   It answers the account as it is kept, or the server's reason it was not.
//!
//! Both are the person's own (`tilo.accounts`'s `list` and `add-mail`), asked
//! of the workspace's server over the key's session. That server answers them
//! for a device of the person's alone: a key made for a script is their
//! agent's, and is refused. Mail in every window of the person follows the
//! accounts the server keeps, so an account added here shows there at once
//! and syncs from its server.
//!
//! The password goes to the server in the request and nowhere else: it is
//! kept in no answer, no History entry and no log line.

use super::Session;
use crate::account::{Outcome, answer, failed, refused};
use serde_json::{Map, Value};
use tilo_actions::remote::person_accounts;
use tilo_session_client::device::DeviceRefusal;

/// The path of the person's mail accounts.
pub const ROOT: &str = "person/mail-accounts";
/// What a refusal names as the next step.
const PERSON_ONLY: &str = "only the person adds their mail accounts, from a device of theirs: ask \
                           from a computer they logged in to with `tilo login`, not with a key \
                           made for a script";

/// Whether `tool` with `arguments` is about the person's mail accounts.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    matches!(tool, "tilo_do" | "tilo_read")
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.split('#').next() == Some(ROOT))
}

impl Session {
    /// Answer `tool` with `arguments` on the person's mail accounts, asked of
    /// `workspace`'s server, typed when `structured`.
    pub(super) async fn mail_accounts(
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
        let args = arguments
            .get("args")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let outcome = match (tool, path) {
            ("tilo_do", "person/mail-accounts#list") | ("tilo_read", ROOT) => {
                self.ask_accounts(workspace, person_accounts::LIST, Map::new())
                    .await
            }
            ("tilo_do", "person/mail-accounts#add") => {
                // Read as the server reads it, so a request it would refuse
                // for its shape is told here, and the password is in no
                // refusal.
                match serde_json::from_value::<person_accounts::AddMailAccount>(Value::Object(
                    args.clone(),
                )) {
                    Ok(_) => {
                        self.ask_accounts(workspace, person_accounts::ADD_MAIL, args)
                            .await
                    }
                    Err(error) => failed(
                        "invalid_arguments",
                        format!("not an account to add: {}", without_values(&error)),
                        "pass args {address, displayName, username, incomingHost, incomingPort, \
                         outgoingHost, outgoingPort, password, identities}",
                    ),
                }
            }
            (_, other) => failed(
                "not_found",
                format!("the person has no action {other}"),
                "its actions are person/mail-accounts#list and person/mail-accounts#add",
            ),
        };
        answer(structured, path, outcome)
    }

    /// What the person's accounts action `action` answered on `workspace`'s
    /// server.
    async fn ask_accounts(
        &self,
        workspace: &str,
        action: &str,
        asked: Map<String, Value>,
    ) -> Outcome {
        let connection = match self.connection() {
            Ok(connection) => connection,
            Err(error) => return refused(&error),
        };
        let id = match connection.server().enter(connection, workspace).await {
            Ok(id) => id,
            Err(error) => return refused(&error),
        };
        match connection
            .device
            .ask_host(id, person_accounts::target(action), asked)
            .await
        {
            Ok(value) => Outcome::Ok(value),
            Err(error) if error.code == DeviceRefusal::UNANSWERED => {
                failed("unavailable", error.message, "try again shortly")
            }
            // A caller that is not the person is told what a caller asking
            // for something that does not exist is told.
            Err(error) if error.code == "unknown_action" => {
                failed("not_permitted", "this is the person's alone", PERSON_ONLY)
            }
            Err(error) => failed(
                "refused",
                error.message,
                "fix what the server said, then ask again",
            ),
        }
    }
}

/// `error` said without the value it was about, which may be the password.
fn without_values(error: &serde_json::Error) -> String {
    let said = error.to_string();
    // serde names a field it missed between backticks, and quotes a value
    // it could not read (`invalid type: string "…", expected u16`): its
    // words are kept up to the first quoted value.
    match said.split_once('"') {
        Some((before, _)) => before.trim_end().to_owned(),
        None => said,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_its_own_paths_are_answered_here() {
        assert!(addresses(
            "tilo_do",
            &json!({"path": "person/mail-accounts#add"})
        ));
        assert!(addresses(
            "tilo_read",
            &json!({"path": "person/mail-accounts"})
        ));
        assert!(!addresses(
            "tilo_do",
            &json!({"path": "mail/mail.mailbox#add-account"})
        ));
        assert!(!addresses(
            "tilo_ls",
            &json!({"path": "person/mail-accounts"})
        ));
    }

    #[test]
    fn a_request_it_cannot_read_is_refused_without_its_values() {
        let error = serde_json::from_value::<person_accounts::AddMailAccount>(json!({
            "address": "ada@example.test",
            "incomingHost": "imap.example.test",
            "outgoingHost": "smtp.example.test",
            "password": "a-secret-value",
            // A field it does not know is read past, so what it cannot
            // read is a value of the wrong type, which serde quotes.
            "incomingPort": "a-secret-value",
        }))
        .unwrap_err();
        assert!(error.to_string().contains("a-secret-value"), "{error}");
        let said = without_values(&error);
        assert!(!said.contains("a-secret-value"), "{said}");
        assert!(said.contains("invalid type"), "{said}");
    }
}
