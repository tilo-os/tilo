//! `files#open` naming content: the call that puts files into the workspace's own folder.
//!
//! A workspace has one folder — its Files app record, the tree its members mount — and the open
//! that names a file's `content`, or many files as `files`, writes them into it instead of
//! starting an app. The files are put where this device keeps the tree, through the one writer
//! every change of it goes through ([`tilo_workspace_service::files::OpenedFiles`]); what the open answers is
//! where each file is, or how many went in.
//!
//! The call is the person's own (§ [`Caller`]), or a device agent's where the workspace's grants
//! let it write the tree. An open naming no content is Files' own open, which starts the app:
//! this answers nothing of it.
//!
//! Files' other verbs on the folder by paths in it (`new-folder`, `rename`, `move`, `copy-into`,
//! `trash`, `restore`, `write-file`, and `undo` of a call) go through the same writer, each
//! decided by the tree's verbs it is carried out as.

use std::sync::Arc;

use fs_protocol::file_action::verbs;
use fs_protocol::tree::{self, FILES_APP};
use serde_json::{Map, Value};
use tilo_action_policy::{ActionKind, Answer, TreePath};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionCallError, Caller, InvokeArgument,
    InvokeRequest,
};
use tilo_actions::{
    ActionAddress, ActionDescriptor, ActionFailure, ActionOutput, InvocationUpdate,
};
use tilo_desktop_model::Workspace;
use tilo_server_protocol::WorkspaceId;

use super::action_provider::refusal;
use super::host_action_admission::HostActionAdmission;
use super::{AppBrokerState, BrokerActor, InvocationCaller};
use crate::services::KEYLESS_AGENT;

/// The descriptor a put into the workspace's folder is admitted under: Files' `open` of a
/// workspace's own folder, which takes the file's `content` and the `name` and `dir` it goes
/// under, or many at once as `files`.
fn open_descriptor() -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{FILES_APP}"),
            APPLICATION_OPEN_ACTION,
        ),
        resource_kind: "files.folder".into(),
        resource_label: "Files".into(),
        action_label: "Open in Files".into(),
        keywords: Vec::new(),
        icon: None,
        description: Some("Put a file the workspace keeps into the workspace's own folder".into()),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract {
            execution: tilo_actions::ExecutionMode::Background { cancellable: false },
            result: tilo_actions::ResultSpec::None,
            ..tilo_actions::ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// The arguments an invocation carries, as the API binds them.
fn invoke_arguments(argument: &InvokeArgument) -> Map<String, Value> {
    match argument {
        InvokeArgument::None => Map::new(),
        InvokeArgument::Text(text) => {
            let mut arguments = Map::new();
            arguments.insert(tilo_actions::PROMPTED_PARAM.into(), text.clone().into());
            arguments
        }
        InvokeArgument::Json(arguments) => arguments.clone(),
    }
}

/// The longest id the host keeps, as the API bounds the one it reads.
const MAX_INVOCATION_ID: usize = 96;

/// The agent-session group `request` says it is a step of, as the client wrote it: what the
/// agent said about its work, never the group the host stamps.
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

/// The id the agent gave the group `request` says it is a step of, as History keeps it with
/// each change.
fn said_invocation_id(request: &InvokeRequest) -> Option<String> {
    let id = said_group(request)?.declared_id.as_deref()?;
    ((1..=MAX_INVOCATION_ID).contains(&id.len()) && id.chars().all(|c| c.is_ascii_graphic()))
        .then(|| id.to_owned())
}

/// The context the host stamps on what `caller`'s `request` changes: the invocation id the host
/// admitted it under, the purpose and gesture the caller said, and the work it is part of. The
/// group is the host's to say. The request itself is kept where the tree is changed
/// ([`crate::services::FilesPutCall`]).
fn caller_context(caller: &Caller, request: &InvokeRequest) -> tilo_actions::ActionContext {
    let said = request.context.as_ref();
    let group = match caller.agent_session() {
        Some(mut group) => {
            group.session = request.said_session_name();
            if let Some(label) = said_invocation_title(request).or_else(|| group.session.clone()) {
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

/// Whether `actor` may change the files of `workspace` now as the tree's `file_verbs`: the
/// tree's own records read and undone, and each of those verbs allowed. An agent gets no more
/// than what the workspace grants it on its turn, as for every other action.
fn may_change_tree(
    workspace: &Workspace,
    actor: &BrokerActor,
    now: u64,
    file_verbs: &[&str],
) -> bool {
    let id = workspace.id().to_string();
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

/// Whether `actor` may put files into `workspace`'s tree now.
fn may_put_into_tree(workspace: &Workspace, actor: &BrokerActor, now: u64) -> bool {
    may_change_tree(workspace, actor, now, &collab_types::FILES_OPEN_VERBS)
}

/// The tree's verbs Files' `verb` on a path is carried out as: what its caller is decided by.
fn file_verbs_of(verb: &str) -> &'static [&'static str] {
    match collab_types::files_call_verbs(verb) {
        // Taking a call back may undo any of what the tree takes.
        [] => &verbs::ALL,
        carried_out => carried_out,
    }
}

impl AppBrokerState {
    /// The workspace's `open` of Files given content: put the file, or the files, into the
    /// workspace's own folder and answer where they are. `None` when the target is another
    /// action, or Files' open of nothing, which starts the app.
    ///
    /// # Errors
    /// A caller the workspace does not let write its files, a caller naming no workspace, an
    /// admission that fails, or a host that serves no folder.
    pub(super) fn start_files_open(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
    ) -> Result<Option<u64>, ActionCallError> {
        let address = &request.target.address;
        if address.action != APPLICATION_OPEN_ACTION {
            return Ok(None);
        }
        let Some(app) = address.resource.strip_prefix(APPLICATION_RESOURCE_PREFIX) else {
            return Ok(None);
        };
        let arguments = invoke_arguments(&request.argument);
        let many = arguments.contains_key("files");
        if app != FILES_APP {
            // Many files are Files' own: a folder goes into the workspace's one folder, not into
            // an app's record.
            return if many {
                Err(refusal(
                    "`files` puts many files into Files' folder: open them with `files#open`"
                        .into(),
                ))
            } else {
                Ok(None)
            };
        }
        if !many && !arguments.contains_key("content") {
            return Ok(None);
        }
        let workspace_id = workspace.id().to_string();
        if caller.workspace() != Some(workspace_id.as_str()) {
            tracing::warn!(
                "putting a file into a workspace was refused: the caller names no such workspace"
            );
            return Err(ActionCallError::UnknownAction);
        }
        if !may_put_into_tree(workspace, policy, now) {
            tracing::warn!(
                caller = ?policy.member_id(),
                owner = workspace.policy.owner().unwrap_or("none"),
                "putting a file into a workspace was refused: the workspace does not let the caller write its files"
            );
            return Err(ActionCallError::NotPermitted);
        }
        // Every replica of the tree checks who asked against the signature the change keeps, so
        // an agent's put is the call it signed with the key its person's root certified for it.
        // One asking under no key of its own is refused before anything is put.
        if caller.acting().is_agent()
            && request.signed.as_deref().is_none_or(|signed| {
                !tilo_server_protocol::invocation::signs_for(caller.acting(), signed)
            })
        {
            return Err(refusal(KEYLESS_AGENT.into()));
        }
        let puts =
            self.services.files_open.clone().ok_or_else(|| {
                refusal("the workspace's folder is not served on this host".into())
            })?;
        let Some(workspace_id) = WorkspaceId::parse(&workspace_id) else {
            return Err(ActionCallError::UnknownAction);
        };
        // The tree's change waits on the workspace carrying it, and the call answers what it
        // answered then, as the writes a file into a mounted folder are answered: the caller
        // reads where its file is, not an invocation to follow.
        let descriptor = open_descriptor();
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                workspace: &workspace_id.to_string(),
                app_id: FILES_APP,
                descriptor: &descriptor,
                request,
            },
        )?;
        let id = admitted.id;
        // Who made the change, in which run, under which invocation: as every change of the
        // workspace's records is made, the caller who asked, an agent named as itself.
        let invoker = tilo_actions::ActionInvoker::admitted(caller.acting())
            .named(caller.agent_name().map(str::to_owned));
        let acting = caller_context(caller, request).with_invoker(invoker);
        // A grant taken back while the content is read refuses the change, decided as the call
        // was.
        let confirm = {
            let state = Arc::clone(self);
            let caller = caller.clone();
            let workspace = workspace_id.to_string();
            move || {
                state
                    .decide_for(&caller, &workspace, may_put_into_tree)
                    .unwrap_or(false)
            }
        };
        // The tree's change keeps the request, so each device that takes the change in checks
        // who asked and decides it by the caller's own grants on the tree's verbs the put is
        // carried out as, rather than taking this device's word for it.
        let call = crate::services::FilesPutCall {
            context: acting,
            request,
        };
        let update = match puts.put(workspace_id, &arguments, Some(call), &confirm) {
            Ok(value) => InvocationUpdate::Succeeded {
                invocation: admitted.invocation,
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
            // What the tree refused is the call's own answer, in words its caller reads.
            Err(reason) => failed(admitted.invocation, reason),
        };
        self.complete_host_action(&admitted, update)?;
        Ok(Some(id))
    }
}

/// The descriptor Files' `verb` on the workspace's own folder is admitted under.
fn change_descriptor(verb: &str) -> ActionDescriptor {
    let mut descriptor = open_descriptor();
    verb.clone_into(&mut descriptor.address.action);
    verb.clone_into(&mut descriptor.action_label);
    descriptor.description =
        Some("Change the workspace's own folder by paths in it; `undo` takes a call back".into());
    descriptor.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    descriptor
}

impl AppBrokerState {
    /// Files' verb on the workspace's own folder by paths in it (`new-folder`, `rename`, `move`,
    /// `copy-into`, `trash`, `restore`, `write-file`, and `undo` of one of them): carried out where
    /// this device holds the tree and answered with what changed and the call it was. `None`
    /// when the target is another action or no folder of the workspace.
    ///
    /// # Errors
    /// A caller naming no workspace, one the workspace does not let change its files as the
    /// verb does, an admission that fails, or a host that serves no folder.
    pub(super) fn start_files_change(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
    ) -> Result<Option<u64>, ActionCallError> {
        let address = &request.target.address;
        let verb = address.action.as_str();
        if !(tree::is_tree_verb(verb) || verb == tree::WRITE_FILE)
            || address.resource != format!("{APPLICATION_RESOURCE_PREFIX}{FILES_APP}")
        {
            return Ok(None);
        }
        let workspace_id = workspace.id().to_string();
        if caller.workspace() != Some(workspace_id.as_str()) {
            tracing::warn!(
                "changing a workspace's files was refused: the caller names no such workspace"
            );
            return Err(ActionCallError::UnknownAction);
        }
        let file_verbs = file_verbs_of(verb);
        if !may_change_tree(workspace, policy, now, file_verbs) {
            return Err(ActionCallError::NotPermitted);
        }
        let puts =
            self.services.files_open.clone().ok_or_else(|| {
                refusal("the workspace's folder is not served on this host".into())
            })?;
        let Some(id) = WorkspaceId::parse(&workspace_id) else {
            return Err(ActionCallError::UnknownAction);
        };
        let descriptor = change_descriptor(verb);
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                workspace: &workspace_id,
                app_id: FILES_APP,
                descriptor: &descriptor,
                request,
            },
        )?;
        let call = admitted.id;
        let invoker = tilo_actions::ActionInvoker::admitted(caller.acting())
            .named(caller.agent_name().map(str::to_owned));
        let acting = caller_context(caller, request).with_invoker(invoker);
        // A grant taken back meanwhile refuses the change, decided as the call was.
        let confirm = {
            let state = Arc::clone(self);
            let caller = caller.clone();
            move || {
                state
                    .decide_for(&caller, &workspace_id, |workspace, actor, now| {
                        may_change_tree(workspace, actor, now, file_verbs)
                    })
                    .unwrap_or(false)
            }
        };
        let arguments = invoke_arguments(&request.argument);
        let put_call = crate::services::FilesPutCall {
            context: acting,
            request,
        };
        let update = match puts.change(id, verb, &arguments, Some(put_call), &confirm) {
            Ok(value) => InvocationUpdate::Succeeded {
                invocation: admitted.invocation,
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
            Err(reason) => failed(admitted.invocation, reason),
        };
        self.complete_host_action(&admitted, update)?;
        Ok(Some(call))
    }
}

/// The outcome of a put the tree refused.
fn failed(invocation: tilo_actions::InvocationId, reason: String) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: "files_open_failed".into(),
            message: reason,
            details: None,
            retryable: false,
        },
    }
}

#[cfg(test)]
#[path = "files_open_tests.rs"]
mod tests;
