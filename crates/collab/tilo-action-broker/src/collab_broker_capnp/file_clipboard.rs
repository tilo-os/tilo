//! Local Files offers use actual launcher custody and the existing action policy.
use super::{Arc, CapnpChannelHandle, Rc};
use crate::services::FileClipboardContext;
use runtime_collab::file_clipboard::{Command, MAX_WIRE_BYTES};
use tilo_actions::{ActionKind, InvocationId};

impl CapnpChannelHandle {
    fn clipboard_original(
        &self,
        parent: Option<InvocationId>,
    ) -> Result<Option<super::PendingInvocation>, String> {
        let Some(parent) = parent else {
            return Ok(None);
        };
        let registry = self
            .state
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let matched = registry
            .values()
            .filter(|p| p.peer_id == self.peer_id && p.update.invocation() == parent)
            .take(2)
            .cloned()
            .collect::<Vec<_>>();
        drop(registry);
        match matched.as_slice() {
            [pending] if pending.cancellation_requested_at.is_none() => Ok(Some(pending.clone())),
            _ => Err("original clipboard invocation cancelled, absent or ambiguous".into()),
        }
    }

    fn file_clipboard_context(
        &self,
        parent: Option<InvocationId>,
        action: &str,
        command: &Command,
    ) -> Result<FileClipboardContext, String> {
        let local = self
            .native_media_context(false)
            .map_err(|error| error.to_string())?;
        if local.app_id() != "so.tilo.file-explorer" {
            return Err("local file offers belong only to admitted Files windows".into());
        }
        let original = self.clipboard_original(parent)?;
        if original.as_ref().is_some_and(|pending| {
            pending
                .original
                .as_ref()
                .is_none_or(|data| !data.permits_clipboard_command(command))
        }) {
            return Err("original invocation does not authorize this clipboard command".into());
        }
        let history_compensation = original
            .as_ref()
            .and_then(|pending| pending.original.as_ref())
            .is_some_and(|data| data.clipboard_compensation());
        let route = self.route();
        let workspace = route.cluster_id.clone();
        let window = route.channel_key();
        let duplicate = self.clipboard_descriptor_source()?;
        let owner = if let Some(pending) = &original {
            pending
                .caller
                .as_ref()
                .and_then(|caller| caller.current(&self.state))
                .map(|caller| self.state.caller_actor(&caller, &workspace).id().to_owned())
                .ok_or("original clipboard caller unavailable")?
        } else {
            self.verified_identity.broker_actor().id().to_owned()
        };
        let local_action = action.to_owned();
        let peer = self.peer_id;
        let admitted = self.verified_identity.clone();
        let state = Arc::downgrade(&self.state);
        let check = Arc::new(move |needed: ActionKind| {
            local.revalidate()?;
            let state = state.upgrade().ok_or("clipboard broker retired")?;
            if !state.is_accepting() {
                return Err("clipboard broker retired".into());
            }
            let actor =
                clipboard_actor(&state, &route, &admitted, original.as_ref(), needed, peer)?;
            let now = state
                .checked_policy_time(original.as_ref().and_then(|p| p.policy_time.as_ref()))?
                .1
                / 1_000_000_000;
            let membership = state
                .membership
                .clone()
                .ok_or("clipboard workspace authority unavailable")?;
            drop(state);
            let action = original
                .as_ref()
                .and_then(|p| p.original.as_ref())
                .map_or(local_action.as_str(), |data| data.clipboard_action());
            // Local Files controls operate on the framework root resource.
            // Continued actions retain their originally addressed resource.
            let resource = original
                .as_ref()
                .and_then(|p| p.original.as_ref())
                .map_or("files.folder", |data| data.clipboard_resource());
            let kind = original
                .as_ref()
                .and_then(|p| p.original.as_ref())
                .map_or("files.folder", |data| data.clipboard_resource_kind());
            if !matches!(needed, ActionKind::Read | ActionKind::Undoable) {
                return Err("unsupported clipboard permission".into());
            }
            membership
                .decide_in_room(&route.cluster_id, |space| {
                    let space = space.ok_or("clipboard workspace ended")?;
                    let node = tilo_actions::action_node(
                        &space.node(),
                        &route.app_id,
                        kind,
                        resource,
                        None,
                        action,
                    );
                    if !actor.may_in(space, &node, needed, now) {
                        return Err("clipboard caller no longer has the required permission");
                    }
                    Ok(())
                })?
                .map_err(str::to_owned)
        });
        Ok(FileClipboardContext {
            peer: self.peer_id,
            window,
            duplicate,
            workspace,
            owner,
            history_compensation,
            check,
        })
    }

    pub(super) fn file_clipboard_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
        parent: &[u8],
    ) -> Result<Vec<u8>, capnp::Error> {
        let failed = |message: String| capnp::Error::failed(message);
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(failed("clipboard command exceeds bound".into()));
        }
        let command: Command =
            serde_json::from_slice(bytes).map_err(|error| failed(error.to_string()))?;
        if !command.valid() {
            return Err(failed("invalid clipboard command".into()));
        }
        let parent = super::observed_launch::launch_parent(parent)?;
        let action = match &command {
            Command::Publish { cut: false, .. }
            | Command::Capture { .. }
            | Command::Release { .. } => "copy-selection",
            Command::Publish { cut: true, .. } => "cut-selection",
            Command::Undo { .. } => "undo",
            Command::Redo { .. } => "redo",
            Command::Recover { .. } => "recover-clipboard-transfers",
            Command::RestoreTrash { .. } => "restore",
            Command::Paste { .. }
            | Command::Snapshot
            | Command::Observe { .. }
            | Command::Cancel { .. } => "paste",
        };
        let context = self
            .file_clipboard_context(parent, action, &command)
            .map_err(failed)?;
        let service = self
            .state
            .services
            .file_clipboard
            .as_ref()
            .ok_or_else(|| failed("local Files clipboard unavailable".into()))?;
        let reply = service.command(&context, command).map_err(failed)?;
        let reply = serde_json::to_vec(&reply).map_err(|error| failed(error.to_string()))?;
        if reply.len() > MAX_WIRE_BYTES {
            return Err(failed("clipboard reply exceeds bound".into()));
        }
        Ok(reply)
    }
}

fn clipboard_actor(
    state: &super::CapnpBrokerState,
    route: &super::CrdtRoute,
    admitted: &super::AdmittedJoinIdentity,
    original: Option<&super::PendingInvocation>,
    needed: ActionKind,
    peer: u64,
) -> Result<super::BrokerActor, String> {
    let actor = if let Some(pending) = original {
        let data = pending
            .original
            .as_ref()
            .ok_or("original clipboard custody unavailable")?;
        data.revalidate_clipboard_origin(state)?;
        let registry = state
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let cancelled = registry.values().any(|current| {
            current.peer_id == peer
                && current.update.invocation() == pending.update.invocation()
                && current.cancellation_requested_at.is_some()
        });
        drop(registry);
        if cancelled || pending.cancellation_fence.load(super::Ordering::Acquire) {
            return Err("original clipboard invocation cancelled".into());
        }
        if !data.permits_clipboard(needed) {
            return Err("original clipboard authority lapsed".into());
        }
        let caller = pending
            .caller
            .as_ref()
            .and_then(|caller| caller.current(state))
            .ok_or("original clipboard caller no longer admitted")?;
        if caller.execution_id().is_some() && !caller.execution_is_live() {
            return Err("original clipboard execution ended".into());
        }
        state.caller_actor(&caller, &route.cluster_id)
    } else {
        state
            .revalidate_admission(route, admitted)
            .map_err(|error| error.to_string())?
            .broker_actor()
    };
    Ok(actor)
}
