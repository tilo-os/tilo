//! The apps and bots a workspace uses, as the path `workspace/apps` of
//! every workspace a key acts in.
//!
//! - `workspace/apps#install-app {app, version, surface, package,
//!   publisher, dev?, file?}` makes that version the one the workspace uses:
//!   every member is to use it, and a change made with another version waits
//!   until its author has. `dev` says a developer's own build, and `file` is
//!   the package the server keeps for members to fetch (`upload-app-file`);
//! - `workspace/apps#uninstall-app {app}` lets every member use their own
//!   version again;
//! - `workspace/apps#upload-app-file {file, offset, total, data}` hands the
//!   server one piece of a package file, by the file's BLAKE3 hash: `install-app`
//!   then names it as the version's `file`;
//! - `workspace/apps#request-bot {entry, version, program?, inputs?}` asks the
//!   workspace's server to start a bot from the directory, or a developer's own
//!   program for that entry, with the bot's settings that are no secret;
//! - `workspace/apps#withdraw-bot-request {entry}` takes the request back.
//!
//! All are the workspace's own governance actions (`tilo.workspace`'s
//! `install-app`, `uninstall-app`, `upload-app-file`, `request-bot` and
//! `withdraw-bot-request`), asked of its server over the key's session, and the
//! server decides who may: a member the workspace grants `install-app` chooses
//! the version it uses, and one it grants `install-bot` asks for a bot.
//! `tilo-app push` is the developer's way to them.

use super::Session;
use crate::account::{Outcome, answer, failed};
use serde_json::{Map, Value};

/// The path of a workspace's apps and bots.
pub const ROOT: &str = "workspace/apps";
/// Choosing the version of an app the workspace uses.
const INSTALL_APP: &str = "workspace/apps#install-app";
/// Letting every member use their own version again.
const UNINSTALL_APP: &str = "workspace/apps#uninstall-app";
/// Handing the server a piece of a package file.
const UPLOAD_APP_FILE: &str = "workspace/apps#upload-app-file";
/// Asking the server to start a bot.
const REQUEST_BOT: &str = "workspace/apps#request-bot";
/// Taking the request for a bot back.
const WITHDRAW_BOT_REQUEST: &str = "workspace/apps#withdraw-bot-request";

/// The verbs the paths name, as the workspace's governance names them.
const INSTALL_APP_ACTION: &str = "install-app";
const UNINSTALL_APP_ACTION: &str = "uninstall-app";
const UPLOAD_APP_FILE_ACTION: &str = "upload-app-file";
const REQUEST_BOT_ACTION: &str = "request-bot";
const WITHDRAW_BOT_REQUEST_ACTION: &str = "withdraw-bot-request";

/// What a refusal names as the next step: the grant the person needs.
const NEXT: &str = "the workspace's owner lets a member choose the version it uses and ask for \
                    bots (`install-app` and `install-bot`); ask an owner for it";

/// Whether `tool` with `arguments` is about the workspace's apps and bots.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    tool == "tilo_do"
        && arguments
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.split('#').next() == Some(ROOT))
}

/// The arguments a verb requires, and those it may take.
type Fields = &'static [&'static str];

/// What the path names: the governance verb, and the arguments it takes.
fn action(path: &str) -> Result<(&'static str, Fields, Fields), Outcome> {
    let found = match path {
        INSTALL_APP => (
            INSTALL_APP_ACTION,
            &["app", "version", "surface", "package", "publisher"][..],
            &["dev", "file"][..],
        ),
        UNINSTALL_APP => (UNINSTALL_APP_ACTION, &["app"][..], &[][..]),
        UPLOAD_APP_FILE => (
            UPLOAD_APP_FILE_ACTION,
            &["file", "offset", "total", "data"][..],
            &[][..],
        ),
        REQUEST_BOT => (
            REQUEST_BOT_ACTION,
            &["entry", "version"][..],
            &["program", "inputs"][..],
        ),
        WITHDRAW_BOT_REQUEST => (WITHDRAW_BOT_REQUEST_ACTION, &["entry"][..], &[][..]),
        other => {
            return Err(failed(
                "not_found",
                format!("the workspace has no action {other}"),
                "its actions are workspace/apps#install-app, workspace/apps#uninstall-app, \
                 workspace/apps#upload-app-file, workspace/apps#request-bot and \
                 workspace/apps#withdraw-bot-request",
            ));
        }
    };
    Ok(found)
}

/// The arguments `args` gives the verb: every required one named, and each
/// optional one it names.
fn fields(
    args: &Value,
    required: &[&str],
    optional: &[&str],
) -> Result<Map<String, Value>, Outcome> {
    let mut asked = Map::new();
    for name in required {
        let Some(value) = args.get(*name).filter(|value| !value.is_null()) else {
            return Err(failed(
                "invalid_arguments",
                format!("`{name}` is required"),
                NEXT,
            ));
        };
        asked.insert((*name).to_owned(), value.clone());
    }
    for name in optional {
        if let Some(value) = args.get(*name).filter(|value| !value.is_null()) {
            asked.insert((*name).to_owned(), value.clone());
        }
    }
    Ok(asked)
}

impl Session {
    /// Answer `tool` with `arguments` on `workspace`'s apps and bots, typed
    /// when `structured`.
    pub(super) async fn apps(
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
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let outcome = match tool {
            "tilo_do" => match action(path) {
                Ok((verb, required, optional)) => match fields(&args, required, optional) {
                    Ok(asked) => match self.govern_or(workspace, verb, asked, NEXT).await {
                        Ok(answer) => Outcome::Ok(answer),
                        Err(outcome) => outcome,
                    },
                    Err(outcome) => outcome,
                },
                Err(outcome) => outcome,
            },
            _ => failed(
                "not_found",
                format!("the workspace has no action {path}"),
                "tilo_do workspace/apps#install-app, #uninstall-app, #upload-app-file, \
                 #request-bot and #withdraw-bot-request act on a workspace's apps and bots",
            ),
        };
        answer(structured, path, outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_do_on_the_apps_path_is_answered_here() {
        assert!(addresses(
            "tilo_do",
            &json!({"path": "workspace/apps#install-app"})
        ));
        assert!(!addresses(
            "tilo_do",
            &json!({"path": "workspace/members#list"})
        ));
        assert!(!addresses("tilo_read", &json!({"path": "workspace/apps"})));
        assert!(!addresses("tilo_do", &json!({})));
    }

    #[test]
    fn the_argumentsare_the_verbs_own_and_a_missing_one_names_itself() {
        let Ok((verb, required, _)) = action(INSTALL_APP) else {
            panic!("install-app is one of its verbs");
        };
        assert_eq!(verb, INSTALL_APP_ACTION);
        let Ok(asked) = fields(
            &json!({"app": "so.tilo.notes", "version": "1.0.0", "surface": "a",
                    "package": "b", "publisher": "c", "dev": true, "unknown": 1}),
            required,
            &["dev", "file"],
        ) else {
            panic!("every required argument is given");
        };
        assert_eq!(asked["app"], "so.tilo.notes");
        assert_eq!(asked["dev"], true);
        assert!(!asked.contains_key("unknown"));

        let Err(Outcome::Failed { message, .. }) =
            fields(&json!({"app": "so.tilo.notes"}), required, &[])
        else {
            panic!("a missing argument refuses the call");
        };
        assert!(message.contains("`version` is required"), "{message}");
    }

    #[test]
    fn an_unknown_action_says_which_ones_there_are() {
        let Err(Outcome::Failed { code, next, .. }) = action("workspace/apps#fly") else {
            panic!("an unknown action refuses");
        };
        assert_eq!(code, "not_found");
        assert!(next.contains("workspace/apps#install-app"), "{next}");
    }
}
