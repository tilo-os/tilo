//! The workspace's own folder, as this device serves it: a file put into it, and Files' verbs on
//! it by paths in it ([`device_files`](tilo_action_services::device_files)).
//!
//! This device holds the tree, so it puts the file through the one writer every change of the
//! tree goes through ([`OpenedFiles`]) and carries a verb out as the tree's own verbs it changes.
//! The dispatch admits the call under the workspace's grants for the application that asks; the
//! provider checks the tree's records and each verb again, when the content was read and the tree
//! is about to be touched. An agent asks under a key its person's root certified for it, and one
//! that names none is refused before anything is written.

use std::sync::Arc;

use fs_protocol::tree::{self, FILES_APP};
use serde_json::{Map, Value};
use tilo_action_policy::{ActionKind, Answer, TreePath};
use tilo_action_services::device_files as wire;
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{
    Admits, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
};
use tilo_actions::{ActionAddress, ActionDescriptor, ActionOutput, InputSpec, OperationKind};
use tilo_app_host::services::FilesTreePuts;
use tilo_broker_core::services::{FilesPutCall, KEYLESS_AGENT};
use tilo_desktop_model::Workspace;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_service::files::OpenedFiles;

use super::workspaces::ServiceWorkspaces;

/// The longest id the host keeps, as the API bounds the one it reads.
const MAX_INVOCATION_ID: usize = 96;

/// What decides a call on the workspace's folder.
pub(crate) trait Folders: Send + Sync {
    /// The workspace `id` as this device holds it now, `None` while it does not.
    fn workspace(&self, id: &str) -> Option<Workspace>;
    /// The policy clock's time, in Unix seconds.
    fn now(&self) -> u64;
    /// What puts files into a workspace's tree, where this device keeps one.
    fn puts(&self) -> Option<Arc<dyn FilesTreePuts>>;
}

impl Folders for ServiceWorkspaces {
    fn workspace(&self, id: &str) -> Option<Workspace> {
        ServiceWorkspaces::workspace(self, WorkspaceId::parse(id)?)
    }

    fn now(&self) -> u64 {
        tilo_clock::unix_now()
    }

    fn puts(&self) -> Option<Arc<dyn FilesTreePuts>> {
        self.content_shelf().map(|shelf| {
            Arc::new(OpenedFiles::new(self.copies().clone(), shelf)) as Arc<dyn FilesTreePuts>
        })
    }
}

/// The workspace's own folder, on this device.
#[derive(Clone)]
pub(crate) struct LocalFiles {
    folders: Arc<dyn Folders>,
}

impl LocalFiles {
    pub(crate) fn new(folders: Arc<dyn Folders>) -> Self {
        Self { folders }
    }

    /// Take the folder's calls from now on, on this device alone.
    pub(crate) fn register(
        self: &Arc<Self>,
        providers: &tilo_workspace_service::workspaces::providers::Providers,
    ) {
        use tilo_workspace_service::workspaces::providers::Reach;
        let answers: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> =
            Arc::new(self.clone());
        providers.provide(wire::PROVIDER, wire::APP, Reach::ThisDevice, answers);
    }

    fn descriptor(action: &str) -> ActionDescriptor {
        let params = if action == wire::OPEN {
            vec![
                tilo_actions::ActionParam::optional(
                    wire::CONTENT,
                    tilo_actions::ParamType::Object,
                    "The file's content: bytes, text, or content the workspace keeps",
                ),
                tilo_actions::ActionParam::optional(
                    wire::FILES,
                    tilo_actions::ParamType::Array,
                    "Many files at once, each with its content and name",
                ),
                tilo_actions::ActionParam::optional(
                    wire::NAME,
                    tilo_actions::ParamType::Text,
                    "The name the file is",
                ),
                tilo_actions::ActionParam::optional(
                    wire::DIR,
                    tilo_actions::ParamType::Text,
                    "The folder of the workspace the file goes under",
                ),
            ]
        } else {
            Vec::new()
        };
        let (label, description) = if action == wire::OPEN {
            (
                "Put in Files",
                "Put a file, or files, into the workspace's own folder",
            )
        } else {
            (
                match action {
                    "new-folder" => "Make a folder",
                    "rename" => "Rename",
                    "move" => "Move",
                    "copy-into" => "Copy",
                    "trash" => "Move to Trash",
                    "restore" => "Put back",
                    "write-file" => "Write a file",
                    "undo" => "Take back",
                    _ => "Change the workspace's folder",
                },
                "Change the workspace's own folder by a path in it",
            )
        };
        ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(wire::FOLDER, action),
            resource_kind: wire::FOLDER.into(),
            resource_label: "Files".into(),
            action_label: label.into(),
            keywords: Vec::new(),
            icon: None,
            description: Some(description.into()),
            locus: tilo_actions::ExecutionLocus::default(),
            kind: OperationKind::Effect,
            contract: tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: false },
                result: tilo_actions::ResultSpec::Json {
                    schema: serde_json::json!({"type": "object"}),
                },
                ..tilo_actions::ActionContract::default()
            },
            input: InputSpec::None,
            params,
            view: None,
        }
    }

    fn arguments(request: &InvokeRequest) -> Map<String, Value> {
        match &request.argument {
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(text) => {
                let mut arguments = Map::new();
                arguments.insert(tilo_actions::PROMPTED_PARAM.into(), text.clone().into());
                arguments
            }
            InvokeArgument::Json(arguments) => arguments.clone(),
        }
    }

    /// Whether `caller` may change the files of `workspace` now as the tree's `file_verbs`.
    fn may_change_tree(
        workspace: &Workspace,
        caller: &Caller,
        now: u64,
        file_verbs: &[&str],
    ) -> bool {
        let id = workspace.id().to_string();
        let actor = tilo_broker_core::BrokerActor::for_caller(caller);
        let allows =
            |node: &TreePath, kind| actor.answer_in(workspace, node, kind, now) == Answer::Allow;
        let records = collab_types::document_node(&id, FILES_APP, &tree::document_id());
        if !allows(&records, ActionKind::Read) || !allows(&records, ActionKind::Undoable) {
            return false;
        }
        file_verbs.iter().all(|verb| {
            let node =
                TreePath::workspace(&id).joined(&fs_protocol::file_action::verb_node(None, verb));
            allows(&node, ActionKind::Undoable)
        })
    }

    fn file_verbs_of(verb: &str) -> &'static [&'static str] {
        match collab_types::files_call_verbs(verb) {
            // Taking a call back may undo any of what the tree takes.
            [] => &fs_protocol::file_action::verbs::ALL,
            carried_out => carried_out,
        }
    }

    /// What the caller said of the invocation it makes this call the work of, and the context
    /// the tree's change is made in. The group is the host's to say; the request itself is kept
    /// where the tree is changed ([`FilesPutCall`]).
    fn caller_context(caller: &Caller, request: &InvokeRequest) -> tilo_actions::ActionContext {
        let said = request.context.as_ref();
        let group = match caller.agent_session() {
            Some(mut group) => {
                group.session = request.said_session_name();
                if let Some(label) =
                    said_invocation_title(request).or_else(|| group.session.clone())
                {
                    group.label = Some(label);
                }
                group.declared_id = said_invocation_id(request);
                Some(group)
            }
            None => said
                .and_then(|context| context.group.clone())
                .filter(|group| group.kind != tilo_actions::context::AGENT_SESSION_GROUP),
        };
        tilo_actions::ActionContext {
            group,
            purpose: said.and_then(|context| context.purpose.clone()),
            gesture: said.and_then(|context| context.gesture),
            source_invocation: Some(request.invocation),
            ..tilo_actions::ActionContext::default()
        }
    }

    /// The workspace `caller` asks in, and this device holds.
    fn workspace(&self, caller: &Caller) -> Result<(String, Workspace, WorkspaceId), String> {
        let id = caller
            .workspace()
            .ok_or("a call on a workspace's folder is made in a workspace")?;
        let workspace = self
            .folders
            .workspace(id)
            .ok_or("this device does not keep that workspace")?;
        let parsed = WorkspaceId::parse(id).ok_or("that workspace has no id this device reads")?;
        Ok((id.to_owned(), workspace, parsed))
    }

    fn confirm(&self, caller: &Caller, id: &str, file_verbs: &'static [&str]) -> bool {
        let Some(workspace) = self.folders.workspace(id) else {
            return false;
        };
        Self::may_change_tree(&workspace, caller, self.folders.now(), file_verbs)
    }

    fn open(&self, caller: &Caller, request: &InvokeRequest) -> Result<Outcome, String> {
        let (id, workspace, parsed) = self.workspace(caller)?;
        let arguments = Self::arguments(request);
        if !arguments.contains_key(wire::CONTENT) && !arguments.contains_key(wire::FILES) {
            return Err("a put names the file's `content`, or files as `files`".into());
        }
        if !Self::may_change_tree(
            &workspace,
            caller,
            self.folders.now(),
            &collab_types::FILES_OPEN_VERBS,
        ) {
            tracing::warn!(
                caller = ?caller.acting(),
                "putting a file into a workspace was refused: the workspace does not let the caller write its files"
            );
            return Err(
                "You can't put files into this workspace's folder: it doesn't let you write its \
                 files."
                    .into(),
            );
        }
        Self::signed_for_agent(caller, request)?;
        let puts = self
            .folders
            .puts()
            .ok_or("The workspace's folder is not served on this computer.")?;
        let context = Self::caller_context(caller, request);
        let call = FilesPutCall { context, request };
        let confirm = || self.confirm(caller, &id, &collab_types::FILES_OPEN_VERBS);
        puts.put(parsed, &arguments, Some(call), &confirm)
            .map(|value| Ok(Self::output(value)))
    }

    fn change(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        verb: &str,
    ) -> Result<Outcome, String> {
        let (id, workspace, parsed) = self.workspace(caller)?;
        let file_verbs = Self::file_verbs_of(verb);
        if !Self::may_change_tree(&workspace, caller, self.folders.now(), file_verbs) {
            return Err(format!(
                "You can't change this workspace's folder: it doesn't let you {verb} its files."
            ));
        }
        let puts = self
            .folders
            .puts()
            .ok_or("The workspace's folder is not served on this computer.")?;
        let arguments = Self::arguments(request);
        let context = Self::caller_context(caller, request);
        let call = FilesPutCall { context, request };
        let confirm = || self.confirm(caller, &id, file_verbs);
        puts.change(parsed, verb, &arguments, Some(call), &confirm)
            .map(|value| Ok(Self::output(value)))
    }

    /// Refuse a call an agent makes under no key of its own.
    fn signed_for_agent(caller: &Caller, request: &InvokeRequest) -> Result<(), String> {
        if caller.acting().is_agent()
            && request.signed.as_deref().is_none_or(|signed| {
                !tilo_server_protocol::invocation::signs_for(caller.acting(), signed)
            })
        {
            return Err(KEYLESS_AGENT.to_owned());
        }
        Ok(())
    }

    /// What the tree answered as the call's output.
    fn output(value: Value) -> ActionOutput {
        ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        }
    }
}

/// The agent-session group `request` says it is a step of, as the client wrote it.
fn said_group(request: &InvokeRequest) -> Option<&tilo_actions::WorkGroup> {
    request
        .context
        .as_ref()?
        .group
        .as_ref()
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)
}

/// What the agent called the group `request` says it is a step of, as History names the
/// invocation's changes: words for people to read, never the group itself, which is the host's
/// to say.
fn said_invocation_title(request: &InvokeRequest) -> Option<String> {
    let title = tilo_actions::context::one_line(
        said_group(request)?.label.as_deref()?,
        tilo_actions::context::MAX_LABEL_CHARS,
    )?;
    (request.said_session_name().as_deref() != Some(title.as_str())).then_some(title)
}

/// The id the agent gave the group `request` says it is a step of, as History keeps it with each
/// change.
fn said_invocation_id(request: &InvokeRequest) -> Option<String> {
    let id = said_group(request)?.declared_id.as_deref()?;
    ((1..=MAX_INVOCATION_ID).contains(&id.len()) && id.chars().all(|c| c.is_ascii_graphic()))
        .then(|| id.to_owned())
}

impl Admits for LocalFiles {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = wire::APP;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        let mut actions = vec![wire::OPEN];
        actions.extend_from_slice(&tree::TREE_VERBS);
        actions.push(tree::WRITE_FILE);
        top_level(actions.into_iter().map(Self::descriptor).collect())
    }

    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        // The folder answers where the tree's sync runs: a change waits for the workspace's
        // records, driven by the device's own runtime, so it is made on a thread of its own
        // rather than parked on a runtime the caller may be inside. A thread that is no
        // runtime's makes it here.
        if tokio::runtime::Handle::try_current().is_ok() {
            // The folder's writer is made here, where a runtime is current, so the watch of the
            // trees deleted meanwhile runs on that runtime; the call itself is made off it.
            drop(self.folders.puts());
            let (answered, heard) = std::sync::mpsc::sync_channel(1);
            let (this, caller, request, admitted) = (
                self.clone(),
                caller.clone(),
                request.clone(),
                admitted.clone(),
            );
            std::thread::Builder::new()
                .name("tilo-files-call".to_owned())
                .spawn(move || {
                    let _ = answered.send(this.start_here(&caller, &request, &admitted));
                })
                .map_err(|error| error.to_string())?;
            return heard
                .recv()
                .map_err(|_| "the folder's call ended without an answer".to_owned())?;
        }
        self.start_here(caller, request, admitted)
    }
}

impl LocalFiles {
    /// [`Admits::start`] as it is made, on a thread that drives the device's runtime.
    fn start_here(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != wire::FOLDER {
            return Err("the device's folder answers no such resource".into());
        }
        let action = address.action.as_str();
        if action == wire::OPEN {
            return self.open(caller, request).map(Pending::ready);
        }
        if tree::is_tree_verb(action) || action == tree::WRITE_FILE {
            return self.change(caller, request, action).map(Pending::ready);
        }
        Err(format!(
            "`{action}` is no verb of the workspace's own folder"
        ))
    }
}

#[cfg(test)]
#[path = "local_files_tests.rs"]
mod tests;
