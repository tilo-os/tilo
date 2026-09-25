#![allow(refining_impl_trait)]
//! Adapters from the shared broker to the native session's existing services.
use super::state::{WorkspaceTaskStatus, WorkspaceTreeHandle};
mod committed_document;
use qos_action_broker::services::*;
use std::sync::Arc;

/// The same state.json owner used by canonical enrollment and ingress. Network
/// availability never supplies or removes this durable admission boundary.
pub(crate) struct NativeOwnerCommitService {
    tree: WorkspaceTreeHandle,
    owner: Option<super::native_workspace_owner::NativeWorkspaceOwner>,
    network: Option<Arc<super::network::ShellNetwork>>,
}
impl NativeOwnerCommitService {
    pub(crate) fn new(
        tree: WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
    ) -> Self {
        let opened = match network.as_ref() {
            Some(network) => network.native_owner(),
            None => super::native_workspace_owner::NativeWorkspaceOwner::open(tree.clone()),
        };
        let owner = match opened {
            Ok(owner) => Some(owner),
            Err(error) => {
                tracing::warn!(%error,"native durable owner unavailable");
                None
            }
        };
        Self {
            tree,
            owner,
            network,
        }
    }
}
impl qos_peer_state::owner_commit::OwnerCommitService for NativeOwnerCommitService {
    fn requires_room(&self, room: &str) -> bool {
        super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().requires_native_owner(room))
    }
    fn health(&self) -> Result<(), String> {
        self.owner
            .as_ref()
            .ok_or("native durable owner unavailable")?
            .health()
    }
    fn commit(
        &self,
        request: qos_peer_state::owner_commit::OwnerCommitRequest,
    ) -> qos_peer_state::owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner.ok_or("native durable owner unavailable")?;
            let room = super::state::RoomId::parse(&request.room).ok_or("invalid native room")?;
            if let Some(network) = network {
                network.check_native_custody(&owner, room)?;
            }
            owner.commit_document(request, std::time::SystemTime::now())
        })
    }
}

pub struct NativeWorkspaceAuthority(pub WorkspaceTreeHandle);
impl WorkspaceAuthority for NativeWorkspaceAuthority {
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        Ok(self.0.read().desktop.clone())
    }
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        self.0.read().persist_path.as_ref().map(|path| {
            let mut name = path.clone().into_os_string();
            name.push(".invocations");
            name.into()
        })
    }
}
impl LegacyWorkspaceProjection for NativeWorkspaceAuthority {
    fn record_effect(
        &self,
        room: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String> {
        let mut tree = self.0.write();
        if super::state::RoomId::parse(room).is_some_and(|room| tree.requires_native_owner(room)) {
            return Err("native enrolled mutation requires its typed durable owner".into());
        }
        let id = tree
            .workspaces
            .iter()
            .find(|w| w.room.to_string() == room)
            .map(|w| w.id)
            .ok_or("effect workspace no longer hosted")?;
        // Preserve native projection semantics; this void flush is not OwnerCommitService.
        let _ = tree.append_workspace_effect_receipt(id, invocation, actor, context, status);
        Ok(())
    }
    fn record_task_update(
        &self,
        room: &str,
        task: &str,
        actor: &str,
        status: Option<WorkspaceTaskStatus>,
        summary: String,
    ) -> Result<(), String> {
        let mut tree = self.0.write();
        if super::state::RoomId::parse(room).is_some_and(|room| tree.requires_native_owner(room)) {
            return Err("native enrolled mutation requires its typed durable owner".into());
        }
        let id = tree
            .workspaces
            .iter()
            .find(|w| w.room.to_string() == room && w.tasks.iter().any(|t| t.id == task))
            .map(|w| w.id)
            .ok_or("task workspace or task no longer hosted")?;
        if let Some(status) = status {
            let _ = tree.set_workspace_task_status(id, task, status, actor);
        }
        let _ = tree.append_workspace_task_audit(id, task, actor, summary);
        Ok(())
    }
}

pub struct NativeLauncher;
impl LauncherServices for NativeLauncher {
    fn applications(&self) -> Vec<LaunchableApplication> {
        super::launcher::gui_apps()
            .into_iter()
            .map(|app| LaunchableApplication {
                package_id: app.package_id,
                display_name: app.display_name,
            })
            .collect()
    }
    fn launch(&self, app: &str, workspace: &qos_desktop::Workspace) -> Result<(), String> {
        super::launcher::launch_in_workspace(app, workspace)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        super::launcher::attest_trusted_service_launch_for_pid(pid, app, room).is_some()
    }
}
pub struct NativeContinuations;
impl ContinuationServices for NativeContinuations {
    fn report(
        &self,
        room: qos_desktop::RoomId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String> {
        super::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .report(
                room,
                app,
                super::personal_continuity::AppInstanceId(instance),
                schema,
                metadata,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn clear(&self, room: qos_desktop::RoomId, app: &str, instance: u64) -> Result<(), String> {
        super::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .clear(
                room,
                app,
                super::personal_continuity::AppInstanceId(instance),
            );
        Ok(())
    }
}

/// The native identity selector and fleet remain native service dependencies.
pub struct NativeCertificates;
impl qos_action_broker::peer_actions::CertificateAuthority for NativeCertificates {
    fn authorize(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
            .map_err(|e| e.to_string())?
            .ok_or("workspace identity unavailable")?;
        let fleet = crate::device_fleet::DeviceFleetStore::from_env()
            .and_then(|store| store.load(&profile.id))
            .map_err(|e| e.to_string())?;
        if cert.user_public_key != profile.user_public_key
            || fleet.is_revoked(&cert.device_public_key)
        {
            return Err("device revoked or identity mismatched".into());
        }
        Ok(())
    }
    fn authorize_join(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.authorize(room, cert)?;
        let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
            .map_err(|e| e.to_string())?
            .ok_or("workspace identity unavailable")?;
        let fleet = crate::device_fleet::DeviceFleetStore::from_env()
            .and_then(|store| store.load(&profile.id))
            .map_err(|e| e.to_string())?;
        if !fleet
            .certificates
            .iter()
            .any(|known| known.device_public_key == cert.device_public_key)
        {
            return Err("certified collaboration device is not enrolled".into());
        }
        Ok(())
    }
}
pub fn native_services(
    membership: Option<WorkspaceTreeHandle>,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> HostServices {
    let committed_documents = membership.as_ref().map(|tree| {
        Arc::new(NativeOwnerCommitService::new(tree.clone(), network.clone()))
            as Arc<dyn NativeDocumentReads>
    });
    HostServices {
        committed_documents,
        desktop_commands: None,
        native_certificates: Some(Arc::new(NativeCertificates)),
        legacy_workspace: membership.map(|tree| {
            Arc::new(NativeWorkspaceAuthority(tree)) as Arc<dyn LegacyWorkspaceProjection>
        }),
        launcher: Some(Arc::new(NativeLauncher)),
        continuations: Some(Arc::new(NativeContinuations)),
        workspace_control: network.map(|network| {
            Arc::new(NativeWorkspaceControl(network)) as Arc<dyn WorkspaceControlServices>
        }),
        ..Default::default()
    }
}

use capnp::capability::Promise;
use runtime_collab::raw::workspace_control_handle;
use std::rc::Rc;
use tokio::sync::Mutex as AsyncMutex;
pub struct NativeWorkspaceControl(pub Arc<super::network::ShellNetwork>);
impl WorkspaceControlServices for NativeWorkspaceControl {
    fn open(
        &self,
        room: qos_desktop::RoomId,
        target: [u8; 32],
        revalidate: Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>,
    ) -> Result<workspace_control_handle::Client, capnp::Error> {
        revalidate()?;
        let target = iroh::EndpointId::from_bytes(&target)
            .map_err(|_| capnp::Error::failed("invalid target device identity".into()))?;
        let network = self.0.clone();
        let session = network
            .open_workspace_control(super::network::room_topic_id(room), target)
            .map_err(capnp::Error::failed)?;
        let initial_route = match session.route() {
            crate::network::WorkspaceControlRoute::Direct { .. } => {
                runtime_collab::WorkspaceControlRoute::Direct
            }
            crate::network::WorkspaceControlRoute::Relayed { via } => {
                runtime_collab::WorkspaceControlRoute::Relayed {
                    via: *via.as_bytes(),
                }
            }
        };
        Ok(capnp_rpc::new_client(CapnpWorkspaceControlHandle {
            revalidate,
            network: network.clone(),
            session: Rc::new(AsyncMutex::new(session)),
            inbound: Rc::new(AsyncMutex::new(network.subscribe_workspace_control())),
            initial: Rc::new(AsyncMutex::new(Some(
                runtime_collab::WorkspaceControlEvent::WaitingForConsent {
                    route: initial_route,
                },
            ))),
        }))
    }
}
struct CapnpWorkspaceControlHandle {
    revalidate: Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>,
    network: Arc<crate::shell::network::ShellNetwork>,
    session: Rc<AsyncMutex<crate::network::WorkspaceControlSession>>,
    inbound: Rc<
        AsyncMutex<
            tokio::sync::mpsc::UnboundedReceiver<
                crate::shell::network::WorkspaceControlNetworkEvent,
            >,
        >,
    >,
    initial: Rc<AsyncMutex<Option<runtime_collab::WorkspaceControlEvent>>>,
}

fn workspace_control_event(
    update: crate::network::WorkspaceControlUpdate,
) -> runtime_collab::WorkspaceControlEvent {
    match update {
        crate::network::WorkspaceControlUpdate::ConsentGranted {
            surface,
            width,
            height,
        } => runtime_collab::WorkspaceControlEvent::StreamStarted {
            surface,
            width,
            height,
        },
        crate::network::WorkspaceControlUpdate::Frame {
            surface,
            encoded_h264,
            timestamp,
        } => runtime_collab::WorkspaceControlEvent::Frame {
            surface,
            encoded_h264,
            timestamp,
        },
        crate::network::WorkspaceControlUpdate::Closed(reason) => {
            let reason = match reason {
                crate::network::WorkspaceControlCloseReason::Denied(message) => {
                    runtime_collab::WorkspaceControlCloseReason::Denied(message)
                }
                crate::network::WorkspaceControlCloseReason::Revoked => {
                    runtime_collab::WorkspaceControlCloseReason::Revoked
                }
                crate::network::WorkspaceControlCloseReason::TargetDisconnected => {
                    runtime_collab::WorkspaceControlCloseReason::TargetDisconnected
                }
                crate::network::WorkspaceControlCloseReason::RelayDisconnected => {
                    runtime_collab::WorkspaceControlCloseReason::RelayDisconnected
                }
                crate::network::WorkspaceControlCloseReason::LocalDisconnect => {
                    runtime_collab::WorkspaceControlCloseReason::LocalDisconnect
                }
            };
            runtime_collab::WorkspaceControlEvent::Closed(reason)
        }
    }
}

fn closed_workspace_control_event(
    session: &crate::network::WorkspaceControlSession,
) -> Option<runtime_collab::WorkspaceControlEvent> {
    let crate::network::WorkspaceControlStatus::Closed(reason) = session.status() else {
        return None;
    };
    Some(workspace_control_event(
        crate::network::WorkspaceControlUpdate::Closed(reason.clone()),
    ))
}

impl workspace_control_handle::Server for CapnpWorkspaceControlHandle {
    fn read(
        self: Rc<Self>,
        _params: workspace_control_handle::ReadParams,
        mut results: workspace_control_handle::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let this = self;
        Promise::from_future(async move {
            let initial_event = {
                let mut initial = this.initial.lock().await;
                initial.take()
            };
            if let Some(event) = initial_event {
                (this.revalidate)()?;
                let bytes = postcard::to_allocvec(&event)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                results.get().set_event(&bytes);
                return Ok(());
            }
            loop {
                let closed = {
                    let session = this.session.lock().await;
                    closed_workspace_control_event(&session)
                };
                if let Some(event) = closed {
                    let bytes = postcard::to_allocvec(&event)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
                if let Err(error) = (this.revalidate)() {
                    let event = {
                        let mut session = this.session.lock().await;
                        workspace_control_event(session.deny(error.to_string()))
                    };
                    let bytes = postcard::to_allocvec(&event)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
                let network_event =
                    match tokio::time::timeout(std::time::Duration::from_millis(250), async {
                        this.inbound.lock().await.recv().await
                    })
                    .await
                    {
                        Ok(Some(event)) => event,
                        Ok(None) => {
                            return Err(capnp::Error::disconnected(
                                "workspace-control observer closed".into(),
                            ));
                        }
                        Err(_) => continue,
                    };
                let mut session = this.session.lock().await;
                (this.revalidate)()?;
                let update = match network_event {
                    crate::shell::network::WorkspaceControlNetworkEvent::Message(event)
                        if event.workspace_topic == session.workspace() =>
                    {
                        session.observe_from(event.transport_peer, &event.message)
                    }
                    crate::shell::network::WorkspaceControlNetworkEvent::PeerLeft {
                        workspace_topic,
                        peer,
                    } if workspace_topic == session.workspace() => session.peer_departed(peer),
                    crate::shell::network::WorkspaceControlNetworkEvent::PeerRevoked {
                        workspace_topic,
                        peer,
                    } if workspace_topic == session.workspace() && peer == session.target() => {
                        Some(session.deny("target device certificate was revoked"))
                    }
                    _ => None,
                };
                if let Some(update) = update {
                    let bytes = postcard::to_allocvec(&workspace_control_event(update))
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
            }
        })
    }

    fn send_input(
        self: Rc<Self>,
        params: workspace_control_handle::SendInputParams,
        _results: workspace_control_handle::SendInputResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = (self.revalidate)() {
            return Promise::err(error);
        }
        let bytes = match params.get().and_then(
            runtime_collab::collab_capnp::workspace_control_handle::send_input_params::Reader::get_event,
        ) {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(error),
        };
        let input: runtime_collab::WorkspaceControlInput = match postcard::from_bytes(bytes) {
            Ok(input) => input,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let this = self;
        Promise::from_future(async move {
            {
                let session = this.session.lock().await;
                (this.revalidate)()?;
                let input = match input {
                    runtime_collab::WorkspaceControlInput::PointerMotion { x, y } => {
                        crate::network::InputEvent::PointerMotion { x, y }
                    }
                    runtime_collab::WorkspaceControlInput::PointerButton { button, pressed } => {
                        crate::network::InputEvent::PointerButton { button, pressed }
                    }
                    runtime_collab::WorkspaceControlInput::Key {
                        keycode,
                        pressed,
                        modifiers,
                    } => crate::network::InputEvent::Key {
                        keysym: keycode,
                        pressed,
                        modifiers,
                    },
                    runtime_collab::WorkspaceControlInput::KeyboardFocus { focused } => {
                        let surface = match session.status() {
                            crate::network::WorkspaceControlStatus::Active { surface, .. } => {
                                *surface
                            }
                            _ => {
                                return Err(capnp::Error::failed(
                                    "target has not granted control".into(),
                                ));
                            }
                        };
                        crate::network::InputEvent::KeyboardFocus { surface, focused }
                    }
                };
                let published = this
                    .network
                    .publish_workspace_control_input(&session, &input)
                    .map_err(capnp::Error::failed);
                drop(session);
                published
            }
        })
    }

    fn close(
        self: Rc<Self>,
        _params: workspace_control_handle::CloseParams,
        _results: workspace_control_handle::CloseResults,
    ) -> Promise<(), capnp::Error> {
        let session = self.session.clone();
        Promise::from_future(async move {
            session.lock().await.disconnect();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_workspace_control_session_is_terminal_for_rpc_readers() {
        let target = iroh::SecretKey::generate().public();
        let mut session = crate::network::WorkspaceControlSession::new(
            7,
            11,
            target,
            crate::network::WorkspaceControlRoute::Direct { target },
        );
        assert!(closed_workspace_control_event(&session).is_none());
        session.disconnect();
        assert_eq!(
            closed_workspace_control_event(&session),
            Some(runtime_collab::WorkspaceControlEvent::Closed(
                runtime_collab::WorkspaceControlCloseReason::LocalDisconnect,
            ))
        );
    }
}

#[cfg(test)]
#[path = "broker_services/native_owner_tests.rs"]
mod native_owner_tests;
