//! Local Files offers use actual launcher custody and the existing action policy.
use super::{Arc, CapnpChannelHandle, Rc};
use crate::services::FileClipboardContext;
use runtime_collab::file_clipboard::{Command, MAX_WIRE_BYTES};
use tilo_actions::{ActionKind, InvocationId};

impl CapnpChannelHandle {
    #[cfg(target_os = "linux")]
    pub(super) fn selected_discovery_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
        parent: &[u8],
    ) -> Result<crate::services::SelectedDiscoveryFuture, capnp::Error> {
        let failed = capnp::Error::failed;
        let request = runtime_collab::open_selected::discovery::decode_request(bytes)
            .map_err(|e| failed(e.to_string()))?;
        let parent = super::observed_launch::launch_parent(parent)?;
        if parent.is_some() != request.invocation.is_some() {
            return Err(failed(
                "selected discovery continuation scope differs".into(),
            ));
        }
        let original = if let Some(scope) = &request.invocation {
            let held = self
                .clipboard_original(parent)
                .map_err(failed)?
                .ok_or_else(|| failed("original selected discovery absent".into()))?;
            if held
                .original
                .as_ref()
                .is_none_or(|data| !data.permits_selected_discovery(scope))
            {
                return Err(failed(
                    "original OpenWith does not permit discovered access".into(),
                ));
            }
            Some(
                self.launch_original_context(parent, &scope.app_id, None)
                    .map_err(failed)?
                    .ok_or_else(|| {
                        failed("original selected discovery certificate absent".into())
                    })?,
            )
        } else {
            None
        };
        let clipboard = self
            .file_clipboard_context(parent, "open-with", &Command::Snapshot)
            .map_err(failed)?;
        let executing = self.selected_open_peer().map_err(failed)?;
        let certificate = if let Some(original) = &original {
            original.certificate().map_err(failed)?
        } else {
            self.state
                .revalidate_admission(&self.route(), &self.verified_identity)?
                .certificate
                .ok_or_else(|| failed("native Files certificate unavailable".into()))?
        };
        let needed = if request
            .invocation
            .as_ref()
            .is_some_and(|scope| scope.access.editable())
        {
            ActionKind::Undoable
        } else {
            ActionKind::Read
        };
        let context = crate::services::SelectedDiscoveryContext {
            clipboard,
            executing,
            certificate,
            original,
            needed,
        };
        context.authorize().map_err(failed)?;
        let service = self
            .state
            .services
            .file_clipboard
            .as_ref()
            .ok_or_else(|| failed("selected discovery unavailable".into()))?;
        Ok(service.discover_selected(context, request))
    }

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

    #[cfg(target_os = "linux")]
    pub(super) fn selected_open_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
        parent: &[u8],
    ) -> Result<crate::services::OpenSelectedFuture, capnp::Error> {
        let failed = |message: String| capnp::Error::failed(message);
        let request = runtime_collab::open_selected::decode_request(bytes)
            .map_err(|e| failed(e.to_string()))?;
        let parent = super::observed_launch::launch_parent(parent)?;
        let original = self
            .clipboard_original(parent)
            .map_err(failed)?
            .ok_or_else(|| failed("OpenWith requires its admitted semantic invocation".into()))?;
        if original
            .original
            .as_ref()
            .is_none_or(|data| !data.permits_selected_open(&request))
        {
            return Err(failed(
                "original invocation does not authorize this OpenWith selection".into(),
            ));
        }
        let local = self
            .native_media_context(false)
            .map_err(|e| failed(e.to_string()))?;
        if local.app_id() != "so.tilo.file-explorer" {
            return Err(failed("OpenWith descriptors belong only to Files".into()));
        }
        let launch = self
            .launch_original_context(parent, &request.request.app_id, None)
            .map_err(failed)?
            .ok_or_else(|| {
                failed("OpenWith target requires its certified original launch caller".into())
            })?;
        let executing = self.selected_open_peer().map_err(failed)?;
        let duplicate = self.clipboard_descriptor_source().map_err(failed)?;
        let route = self.route();
        let workspace = route.cluster_id.clone();
        let peer = self.peer_id;
        let expected = request.clone();
        let custody = SelectedOpenCustody {
            local,
            state: Arc::downgrade(&self.state),
            original,
            expected,
            v2: None,
            route,
            peer,
        };
        let check = Arc::new(move || custody.check());
        let context = crate::services::OpenSelectedContext {
            launch,
            workspace,
            duplicate,
            executing,
            check,
        };
        context.authorize().map_err(failed)?;
        let service = self
            .state
            .services
            .file_clipboard
            .as_ref()
            .ok_or_else(|| failed("selected-open transport unavailable".into()))?;
        Ok(service.open_selected(context, request))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn selected_open_v2_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
        parent: &[u8],
    ) -> Result<crate::services::OpenSelectedV2Future, capnp::Error> {
        let failed = |message: String| capnp::Error::failed(message);
        let request = runtime_collab::open_selected::v2::decode_request(bytes)
            .map_err(|e| failed(e.to_string()))?;
        let parent = super::observed_launch::launch_parent(parent)?;
        let original = self
            .clipboard_original(parent)
            .map_err(failed)?
            .ok_or_else(|| failed("OpenWith requires its admitted semantic invocation".into()))?;
        if original
            .original
            .as_ref()
            .is_none_or(|data| !data.permits_selected_open_v2(&request))
        {
            return Err(failed(
                "original invocation does not authorize this OpenWith selection".into(),
            ));
        }
        let local = self
            .native_media_context(false)
            .map_err(|e| failed(e.to_string()))?;
        if local.app_id() != "so.tilo.file-explorer" {
            return Err(failed("OpenWith descriptors belong only to Files".into()));
        }
        let launch = self
            .launch_original_context(parent, &request.request.app_id, None)
            .map_err(failed)?
            .ok_or_else(|| {
                failed("OpenWith target requires its certified original launch caller".into())
            })?;
        let executing = self.selected_open_peer().map_err(failed)?;
        let duplicate = self.clipboard_descriptor_source().map_err(failed)?;
        let route = self.route();
        let workspace = route.cluster_id.clone();
        let peer = self.peer_id;
        let expected = request.clone();
        let custody = SelectedOpenCustody {
            local,
            state: Arc::downgrade(&self.state),
            original,
            expected: expected.navigation(),
            v2: Some(expected),
            route,
            peer,
        };
        let check = Arc::new(move || custody.check());
        let context = crate::services::OpenSelectedContext {
            launch,
            workspace,
            duplicate,
            executing,
            check,
        };
        context.authorize().map_err(failed)?;
        let service = self
            .state
            .services
            .file_clipboard
            .as_ref()
            .ok_or_else(|| failed("selected-open transport unavailable".into()))?;
        Ok(service.open_selected_v2(context, request))
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
        // Whatever this command names and does not take, it releases: a
        // handed-over object lives no longer than the request naming it.
        let _handed = HandedRelease {
            descriptors: self.handed_descriptors(),
            numbers: command
                .selected()
                .map(|selected| selected.descriptor)
                .collect(),
        };
        let parent = super::observed_launch::launch_parent(parent)?;
        let action = match &command {
            Command::Publish { cut: false, .. }
            | Command::Capture { .. }
            | Command::CaptureNamed { .. }
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

struct HandedRelease {
    descriptors: Option<runtime_portal::descriptor_lane::ReceivedDescriptors>,
    numbers: Vec<u32>,
}
impl Drop for HandedRelease {
    fn drop(&mut self) {
        if let Some(descriptors) = &self.descriptors {
            descriptors.discard(self.numbers.drain(..));
        }
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

#[cfg(target_os = "linux")]
struct SelectedOpenCustody {
    local: super::native_media::NativeMediaRequestContext,
    state: std::sync::Weak<super::CapnpBrokerState>,
    original: super::PendingInvocation,
    expected: runtime_collab::open_selected::Request,
    v2: Option<runtime_collab::open_selected::v2::Request>,
    route: super::CrdtRoute,
    peer: u64,
}
#[cfg(target_os = "linux")]
impl SelectedOpenCustody {
    fn check(&self) -> Result<(), String> {
        self.local.revalidate()?;
        let state = self.state.upgrade().ok_or("OpenWith broker retired")?;
        self.check_with(&state)
    }

    /// The remaining custody checks, against the live broker `state`.
    fn check_with(&self, state: &super::CapnpBrokerState) -> Result<(), String> {
        if !state.is_accepting() {
            return Err("OpenWith broker retired".into());
        }
        let data = self
            .original
            .original
            .as_ref()
            .ok_or("original OpenWith custody absent")?;
        data.revalidate_clipboard_origin(state)?;
        if match &self.v2 {
            Some(request) => !data.permits_selected_open_v2(request),
            None => !data.permits_selected_open(&self.expected),
        } {
            return Err("original OpenWith authority lapsed".into());
        }
        let registry = state
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let live = registry.values().any(|pending| {
            pending.peer_id == self.peer
                && pending.update.invocation() == self.original.update.invocation()
                && pending.cancellation_requested_at.is_none()
                && !pending.cancellation_fence.load(super::Ordering::Acquire)
        });
        drop(registry);
        if !live
            || self
                .original
                .cancellation_fence
                .load(super::Ordering::Acquire)
        {
            return Err("original OpenWith invocation ended".into());
        }
        let caller = self
            .original
            .caller
            .as_ref()
            .and_then(|caller| caller.current(state))
            .ok_or("original OpenWith caller unavailable")?;
        if caller.execution_id().is_some() && !caller.execution_is_live() {
            return Err("original OpenWith execution ended".into());
        }
        let actor = state.caller_actor(&caller, &self.route.cluster_id);
        let now = state
            .checked_policy_time(self.original.policy_time.as_ref())?
            .1
            / 1_000_000_000;
        let membership = state
            .membership
            .as_ref()
            .ok_or("OpenWith workspace authority unavailable")?;
        let needed = if self
            .v2
            .as_ref()
            .is_some_and(|request| request.request.access.editable())
        {
            ActionKind::Undoable
        } else {
            ActionKind::Read
        };
        membership
            .decide_in_room(&self.route.cluster_id, |space| {
                let space = space.ok_or("OpenWith workspace ended")?;
                let node = tilo_actions::action_node(
                    &space.node(),
                    &self.route.app_id,
                    data.clipboard_resource_kind(),
                    data.clipboard_resource(),
                    None,
                    data.clipboard_action(),
                );
                if !actor.may_in(space, &node, needed, now) {
                    return Err("OpenWith caller no longer has requested permission");
                }
                Ok(())
            })?
            .map_err(str::to_owned)?;
        Ok(())
    }
}
