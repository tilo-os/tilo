//! Starting an app from a path: `notes#open` starts an installed app and says what is in it,
//! and `pdf-viewer#open content=…` opens a file the workspace holds in the app that opens it,
//! whether or not the app runs already.

use super::{ActionProvider, Addressed, McpServer, OneCall, Value, json};
use super::{Address, Done, LsArgs, Reply, app_names, app_of, below_app, outcome};
use std::fmt::Write as _;

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// `notes#open` (or `notes#start`, without a window): start an installed app, then say
    /// what is in it.
    pub(super) async fn launch(
        &self,
        parsed: &Address,
        app_id: &str,
        action: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<(Value, Reply), String> {
        if !below_app(parsed).is_empty() || !starts_app(action) {
            return Err(tilo_actions::tree::Unresolved::NotRunning {
                name: app_of(parsed),
                opens: true,
            }
            .to_string());
        }
        let value = self
            .app_in_workspace(app_id, action, arguments, invocation, wait_ms)
            .await?;
        if outcome(&value).failed {
            let reply = outcome(&value);
            return Ok((value, reply));
        }
        let reply = self.opened_reply(app_id, &value).await;
        Ok((value, reply))
    }

    /// [`Self::launch`] as a call's answer: `pdf-viewer#open content=…` opens a file, running or not.
    pub(super) async fn launched(
        &self,
        (parsed, app_id, action): (&Address, &str, &str),
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Done, super::Failure> {
        let (value, reply) =
            Box::pin(self.launch(parsed, app_id, action, arguments, invocation, wait_ms)).await?;
        // `open` puts the app's window in front of the person, on the document it opened; `start`
        // starts it without one. Starting the app is the workspace's open; the window is the
        // layout's, asked for here as for an app that is up already ([`Self::running`]): where the
        // serving device holds no layout, that ask is what the person's page places. A window that
        // cannot be put there is said beside what the open did.
        let not_shown = if action == tilo_actions::remote::APPLICATION_OPEN_ACTION && !reply.failed
        {
            let opened = value
                .pointer("/update/output/value/provider")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.show(opened, app_id).await.err()
        } else {
            None
        };
        Ok(Done::Launched {
            value,
            reply,
            not_shown,
        })
    }

    /// Do the one action `path` names. One of Files' verbs on the workspace's own folder
    /// ([`in_workspace_folder`]) starts no app: a read is asked of the workspace, a put or a
    /// change of the device that keeps the tree. Every other action is done in the app that
    /// offers it. A file read by its bare name is first the entry Files shows by that name where
    /// the agent acts ([`Self::read_where_shown`]).
    pub(super) async fn do_one_value(
        &self,
        path: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Done, super::Failure> {
        // The workspace's search, which no app offers: answered by the host's search, each hit
        // named by the path the next call takes.
        if path.trim().split_once('#')
            == Some((
                tilo_workspace_records::search::RESOURCE,
                tilo_workspace_records::search::SearchAction::FIND.id(),
            ))
        {
            let args: super::FindArgs =
                serde_json::from_value(Value::Object(arguments)).map_err(|error| {
                    format!(
                        "invalid_arguments: search#find takes `query`, `app` and `limit`: {error}"
                    )
                })?;
            let mut reply = Box::pin(self.agent_find(args, true)).await?;
            let found = reply.structured.take();
            let value = json!({"update": {"state": "succeeded", "output": {"value": found}}});
            return Ok(Done::Launched {
                value,
                reply,
                not_shown: None,
            });
        }
        // Only a path that begins with Files' name is read here before it is read where it acts.
        let app = path.split(['#', '/']).next().unwrap_or_default().trim();
        if tilo_actions::tree::names_files(app)
            && let Ok(parsed) = Address::parse(path)
            && let Some(action) = parsed.action.as_deref()
            && in_workspace_folder(&parsed, action, &arguments)
        {
            if reads_by_bare_name(action, &arguments)
                && let Some(read) = self.read_where_shown(path, &arguments, wait_ms).await
            {
                return read;
            }
            // A read is answered by the workspace's listing of its folder; a put or a change is
            // the device's, which keeps the tree.
            let value = if reads_workspace_folder(action, &arguments) {
                let files = fs_protocol::tree::FILES_APP;
                self.app_in_workspace(files, action, arguments, invocation, wait_ms)
                    .await?
            } else {
                self.in_device_folder(action, arguments, invocation, wait_ms)
                    .await?
            };
            let reply = outcome(&value);
            return Ok(Done::Launched {
                value,
                reply,
                not_shown: None,
            });
        }
        Box::pin(self.do_in_app(path, arguments, invocation, wait_ms)).await
    }

    /// The read of a file named by its bare name, done by Files where the agent acts, when
    /// Files is started there and shows an entry by that name: an agent that sees a folder in
    /// Files on its device and names one of its entries means that entry, as it did before a
    /// workspace's own folder was read by a path. `None` where nothing of Files is started (a
    /// turn on a server), or where the folder it shows holds no such entry: the name is then
    /// a path in the workspace's own folder. What Files refuses for another reason (a file
    /// that is no text) is its answer.
    async fn read_where_shown(
        &self,
        path: &str,
        arguments: &serde_json::Map<String, Value>,
        wait_ms: Option<u64>,
    ) -> Option<Result<Done, super::Failure>> {
        let (_, started) = self.tree.providers().await.ok()?;
        // An app's own instance is named by its number; what a holder serves of the
        // workspace's documents is not Files showing a folder.
        let shows = started.iter().any(|(provider, app)| {
            provider.parse::<u64>().is_ok()
                && app["app_id"].as_str() == Some(fs_protocol::tree::FILES_APP)
        });
        if !shows {
            return None;
        }
        let read = Box::pin(self.do_in_app(path, arguments.clone(), None, wait_ms)).await;
        let said = match &read {
            Ok(Done::Invoked { value, .. } | Done::Launched { value, .. }) => outcome(value).text,
            Err(failure) => failure.message.clone(),
        };
        (!said.contains(NOT_SHOWN)).then_some(read)
    }

    /// `action` (`open` or `start`) of the app `resolved` names, which runs already: it is there,
    /// and what is in it. Running is not shown: `open` asks for the app's window on the person's
    /// shell, or brings the one there into view, and says why beside what it did when neither
    /// can be done.
    pub(super) async fn running(
        &self,
        resolved: &super::Resolved,
        action: &str,
    ) -> Result<Done, super::Failure> {
        let app_id = resolved.app_id.as_str();
        let not_shown = if action == tilo_actions::remote::APPLICATION_OPEN_ACTION {
            self.show(&resolved.provider, app_id).await.err()
        } else {
            None
        };
        // What is in the window it answered with: the record the workspace lists
        // it on, so a caller opening an app that runs learns the window it
        // attached to and the record that window shows, as the open that made
        // it said.
        let record = self
            .tree
            .providers()
            .await
            .ok()
            .and_then(|(_, listed)| {
                listed
                    .into_iter()
                    .find(|(provider, _)| *provider == resolved.provider)
                    .map(|(_, entry)| entry)
            })
            .and_then(|entry| {
                entry
                    .get("document_id")
                    .and_then(Value::as_str)
                    .filter(|record| !record.is_empty())
                    .map(str::to_owned)
            });
        let value = match record {
            Some(record) => json!({"update": {
                "state": "succeeded",
                "output": {"value": {
                    "provider": format!("{}:{app_id}:{record}", tilo_actions::remote::RESOURCE_PROVIDER),
                    "record": record,
                }},
            }}),
            None => json!({"update": {"state": "succeeded", "output": {}}}),
        };
        let reply = self.opened_reply(app_id, &value).await;
        Ok(Done::Launched {
            value,
            reply,
            not_shown,
        })
    }

    /// What starting `app_id` answers once it runs: that it does, and what
    /// is in it.
    pub(super) async fn opened_reply(&self, app_id: &str, value: &Value) -> Reply {
        let mut reply = outcome(value);
        let name = app_names(&[app_id.to_owned()]).join("");
        reply.text = format!("ok: {name} is running");
        // A file opened in it: the record it is in, as a path addresses it.
        let opened =
            (value.pointer("/update/output/value")).filter(|it| it.get("action").is_some());
        if let Some(provider) = opened.and_then(|opened| opened["provider"].as_str()) {
            reply.text = format!("ok: opened in {name}@{provider}");
        }
        if let Some(error) = value.pointer("/opened/error").and_then(Value::as_str) {
            let _ = write!(reply.text, "\nnot opened: {error}");
        }
        if let Ok(listing) = self
            .agent_ls(
                LsArgs {
                    path: Some(name.clone()),
                    ..LsArgs::default()
                },
                false,
            )
            .await
        {
            // An app that opened on the person's shell but publishes nothing an
            // agent lists (yet) is not answered as both running and not.
            if listing.text.starts_with(
                &tilo_actions::tree::Unresolved::NotRunning {
                    name: name.clone(),
                    opens: true,
                }
                .to_string(),
            ) {
                if reply.text.ends_with("is running") {
                    reply.text = format!(
                        "ok: {name} opened on the person's shell; it lists nothing to agents"
                    );
                }
                return reply;
            }
            reply.text.push('\n');
            reply.text.push_str(&listing.text);
        }
        reply
    }

    /// Files' `action` on the workspace's own folder (its `open` of content, a verb by paths in
    /// it), as the device that keeps the tree answers it under the workspace's grants for Files.
    async fn in_device_folder(
        &self,
        action: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Value, String> {
        use tilo_action_services::device_files;
        Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: device_files::PROVIDER.to_owned(),
                app_id: None,
            },
            workspace: false,
            revision: None,
            resource: device_files::FOLDER.to_owned(),
            action: action.to_owned(),
            arguments,
            invocation_id: invocation,
            wait_ms,
        }))
        .await
        .map_err(String::from)
    }

    /// The workspace's `action` (its `open` or its `start`) of the app `app_id`, with
    /// `arguments` for the files it opens there: the workspace's answer, which names the
    /// instance it started at [`super::OPENED_PEER`].
    pub(super) async fn app_in_workspace(
        &self,
        app_id: &str,
        action: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Value, String> {
        Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: tilo_actions::remote::workspace_provider(&self.workspace),
                app_id: None,
            },
            workspace: true,
            revision: None,
            resource: format!(
                "{}{app_id}",
                tilo_actions::remote::APPLICATION_RESOURCE_PREFIX
            ),
            action: action.to_owned(),
            arguments,
            invocation_id: invocation,
            wait_ms,
        }))
        .await
        .map_err(String::from)
    }
}

/// Whether `action` is the word that starts an app: its `open` (with its window) or its `start`.
pub(super) fn starts_app(action: &str) -> bool {
    action == tilo_actions::remote::APPLICATION_OPEN_ACTION
        || action == tilo_actions::remote::APPLICATION_START_ACTION
}

/// Whether `action` with `arguments`, asked of the app `parsed` names, is one of Files' verbs on
/// the workspace's own folder, its `write-file` there, its `open` of content
/// ([`puts_into_workspace_folder`]), or one of its reads of it ([`reads_workspace_folder`]): a path in the workspace, and no device whose own disk it would
/// change or read. Those start no app: the workspace reads its tree, and the device that keeps it
/// changes it, as a file put into the workspace is; with a `device` they stay Files' own on that
/// device.
pub(super) fn in_workspace_folder(
    parsed: &Address,
    action: &str,
    arguments: &serde_json::Map<String, Value>,
) -> bool {
    below_app(parsed).is_empty()
        && tilo_actions::tree::names_files(&app_of(parsed))
        && (fs_protocol::tree::is_tree_verb(action)
            || action == fs_protocol::tree::WRITE_FILE
            || puts_into_workspace_folder(action, arguments)
            || reads_workspace_folder(action, arguments))
        && !arguments.contains_key("device")
}

/// Whether `action` with `arguments` is Files' `open` of content (`content`, or `files` for
/// many): a put into the workspace's own folder, which the workspace answers wherever Files
/// runs. Asked of a running Files it would be resolved in that window's own actions, which
/// put nothing into the workspace.
fn puts_into_workspace_folder(action: &str, arguments: &serde_json::Map<String, Value>) -> bool {
    action == tilo_actions::remote::APPLICATION_OPEN_ACTION
        && (arguments.contains_key("content") || arguments.contains_key("files"))
}

/// Whether `action` with `arguments` is one of Files' reads of the workspace's own folder by a
/// path in the workspace: its `list`, its `history`, and its `read-text` of a file whose path starts at the
/// workspace's folder. A path that starts with `/` or `~` is a file on a computer's own disk,
/// which Files on that computer reads.
fn reads_workspace_folder(action: &str, arguments: &serde_json::Map<String, Value>) -> bool {
    use fs_protocol::tree::{HISTORY, LIST, READ_TEXT};
    // Content the workspace keeps is on no one computer either: a read of it by its address
    // (`content`) is the workspace's to answer.
    action == LIST
        || action == HISTORY
        || (action == READ_TEXT
            && (arguments.contains_key("content")
                || arguments
                    .get("file")
                    .and_then(Value::as_str)
                    .is_some_and(|file| !file.trim_start().starts_with(['/', '~']))))
}

/// What Files says of a name that is neither an absolute path nor an entry of the folder it
/// shows (`FilesState::named_file` in the Files app): by these words a bare name it does not
/// show is known, and is then read as a path in the workspace's own folder.
const NOT_SHOWN: &str = "an entry of the folder shown";

/// Whether `action` with `arguments` reads a file named by its bare name, with no folder
/// before it: as much the name of an entry of the folder Files shows as a path in the
/// workspace's own folder.
fn reads_by_bare_name(action: &str, arguments: &serde_json::Map<String, Value>) -> bool {
    action == fs_protocol::tree::READ_TEXT
        && arguments
            .get("file")
            .and_then(Value::as_str)
            .is_some_and(|file| !file.trim().is_empty() && !file.contains('/'))
}

/// Whether `arguments` are those of an app's own `open` of a file: they name the file's
/// content, as the workspace's open reads it.
pub(super) fn opens_a_file(arguments: &serde_json::Map<String, Value>) -> bool {
    arguments.contains_key("content")
}
