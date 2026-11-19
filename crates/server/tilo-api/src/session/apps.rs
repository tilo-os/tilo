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
//! `tilo_ls workspace/apps` lists them with what each takes, as any resource
//! lists its actions, so `tilo ls -w <workspace> workspace/apps`, `tilo do
//! -w`, the API's actions route and an agent all find and do them; `tilo-app
//! push` does them in order for a developer's build.

use super::Session;
use crate::account::{
    ActionRow, Outcome, Param, answer, failed, listed_actions, param, tool_result,
};
use serde_json::{Map, Value, json};

/// The path of a workspace's apps and bots.
pub const ROOT: &str = "workspace/apps";

/// What a refusal names as the next step: the grant the person needs.
const NEXT: &str = "the workspace's owner lets a member choose the version it uses and ask for \
                    bots (`install-app` and `install-bot`); ask an owner for it";

/// The workspace's apps and bots actions, each the governance verb of the
/// same name: what `tilo_ls workspace/apps` lists, and what a call is checked
/// against.
const ACTIONS: [ActionRow; 5] = [
    (
        "apps",
        "install-app",
        "Make this version of the app the one every member uses",
        "governance",
        &[
            param("app", true, "Text"),
            param("version", true, "Text"),
            param("surface", true, "Text"),
            param("package", true, "Text"),
            param("publisher", true, "Text"),
            param("dev", false, "Boolean"),
            param("file", false, "Text"),
        ],
    ),
    (
        "apps",
        "uninstall-app",
        "Let every member use their own version of the app again",
        "governance",
        &[param("app", true, "Text")],
    ),
    (
        "apps",
        "upload-app-file",
        "Hand the server one piece of a package file, named by its BLAKE3 hash",
        "governance",
        &[
            param("file", true, "Text"),
            param("offset", true, "Integer"),
            param("total", true, "Integer"),
            param("data", true, "Text"),
        ],
    ),
    (
        "apps",
        "request-bot",
        "Ask the workspace's server to start a bot from the directory, or a program of your own",
        "governance",
        &[
            param("entry", true, "Text"),
            param("version", true, "Text"),
            param("program", false, "Text"),
            param("inputs", false, "Object"),
        ],
    ),
    (
        "apps",
        "withdraw-bot-request",
        "Take the request for a bot back",
        "governance",
        &[param("entry", true, "Text")],
    ),
];

/// Whether `tool` with `arguments` is about the workspace's apps and bots:
/// one of their actions, or the listing of them.
pub(crate) fn addresses(tool: &str, arguments: &Value) -> bool {
    let path = arguments.get("path").and_then(Value::as_str);
    match tool {
        "tilo_do" => path.is_some_and(|path| path.split('#').next() == Some(ROOT)),
        "tilo_ls" => path.is_some_and(|path| path.trim_end_matches('/') == ROOT),
        _ => false,
    }
}

/// What the path names: the governance verb, and the parameters it takes.
fn action(path: &str) -> Result<(&'static str, &'static [Param]), Outcome> {
    path.strip_prefix(ROOT)
        .and_then(|verb| verb.strip_prefix('#'))
        .and_then(|verb| ACTIONS.iter().find(|row| row.1 == verb))
        .map(|row| (row.1, row.4))
        .ok_or_else(|| {
            let names: Vec<String> = ACTIONS
                .iter()
                .map(|row| format!("{ROOT}#{}", row.1))
                .collect();
            failed(
                "not_found",
                format!("the workspace has no action {path}"),
                format!("its actions are {}", names.join(", ")),
            )
        })
}

/// The arguments `args` gives the verb: every required parameter named, and
/// each optional one it names.
fn fields(args: &Value, params: &[Param]) -> Result<Map<String, Value>, Outcome> {
    let mut asked = Map::new();
    for param in params {
        match args.get(param.name).filter(|value| !value.is_null()) {
            Some(value) => {
                asked.insert(param.name.to_owned(), value.clone());
            }
            None if param.required => {
                return Err(failed(
                    "invalid_arguments",
                    format!("`{}` is required", param.name),
                    NEXT,
                ));
            }
            None => {}
        }
    }
    Ok(asked)
}

/// `tilo_ls workspace/apps`: the actions on the workspace's apps and bots.
fn listing(structured: bool) -> Value {
    let (actions, lines) = listed_actions(&ACTIONS, "apps");
    tool_result(
        structured,
        &format!("{ROOT}: the apps and bots the workspace uses{lines}"),
        &json!({"path": ROOT, "kind": "workspace.apps", "rows": [], "actions": actions}),
        false,
    )
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
        if tool == "tilo_ls" {
            return listing(structured);
        }
        let outcome = match action(path) {
            Ok((verb, params)) => match fields(&args, params) {
                Ok(asked) => match self.govern_or(workspace, verb, asked, NEXT).await {
                    Ok(answer) => Outcome::Ok(answer),
                    Err(outcome) => outcome,
                },
                Err(outcome) => outcome,
            },
            Err(outcome) => outcome,
        };
        answer(structured, path, outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn its_actions_and_their_listing_are_answered_here() {
        assert!(addresses(
            "tilo_do",
            &json!({"path": "workspace/apps#install-app"})
        ));
        assert!(addresses("tilo_ls", &json!({"path": "workspace/apps"})));
        assert!(!addresses(
            "tilo_do",
            &json!({"path": "workspace/members#list"})
        ));
        assert!(!addresses("tilo_read", &json!({"path": "workspace/apps"})));
        assert!(!addresses("tilo_ls", &json!({"path": "workspace"})));
        assert!(!addresses("tilo_do", &json!({})));
    }

    #[test]
    fn the_listing_names_every_action_with_what_it_takes() {
        let typed = listing(true)
            .get("structuredContent")
            .cloned()
            .unwrap_or_default();
        let said = listing(false);
        let text = said
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let ids: Vec<&str> = typed["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|action| action["id"].as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "install-app",
                "uninstall-app",
                "upload-app-file",
                "request-bot",
                "withdraw-bot-request"
            ],
            "{typed}"
        );
        assert!(
            text.contains("#request-bot(entry, version, program?, inputs?)"),
            "{text}"
        );
    }

    #[test]
    fn the_arguments_are_the_verbs_own_and_a_missing_one_names_itself() {
        let Ok((verb, params)) = action("workspace/apps#install-app") else {
            panic!("install-app is one of its verbs");
        };
        assert_eq!(verb, "install-app");
        let Ok(asked) = fields(
            &json!({"app": "so.tilo.notes", "version": "1.0.0", "surface": "a",
                    "package": "b", "publisher": "c", "dev": true, "unknown": 1}),
            params,
        ) else {
            panic!("every required argument is given");
        };
        assert_eq!(asked["app"], "so.tilo.notes");
        assert_eq!(asked["dev"], true);
        assert!(!asked.contains_key("unknown"));

        let Err(Outcome::Failed { message, .. }) = fields(&json!({"app": "so.tilo.notes"}), params)
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
