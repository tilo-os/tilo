//! Remote-capture coordination inside the device-fabric pump.
//!
//! The capture runtime owns protocol/session authority. This adapter owns the
//! compositor-facing concerns around it: transient lock contention, route
//! freshness, peer presence, policy teardown, and locally verified app focus.

use std::collections::{BTreeSet, VecDeque};
use std::time::Instant;

use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::device_fabric::PersonalFabricAction;
use crate::shell::network::{ReceivedPersonalFabricAction, ShellNetwork};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::physical_gamepad::PhysicalGamepadBinding;
use crate::shell::remote_capture::device::MAX_REMOTE_CAPTURE_SESSIONS;
use crate::shell::remote_capture::runtime::SharedRemoteCaptureRuntime;

#[derive(Default)]
pub(super) struct RemoteCaptureIntegration {
    runtime: Option<SharedRemoteCaptureRuntime>,
    online_peers: BTreeSet<DeviceId>,
    pending_actions: VecDeque<ReceivedPersonalFabricAction>,
    stale_focus: BTreeSet<(String, String)>,
    teardown_all_pending: bool,
}

impl RemoteCaptureIntegration {
    pub(super) fn install(&mut self, runtime: SharedRemoteCaptureRuntime) {
        self.runtime = Some(runtime);
    }

    pub(super) fn note_focus_change(
        &mut self,
        previous: Option<&PhysicalGamepadBinding>,
        current: Option<&PhysicalGamepadBinding>,
    ) {
        if let Some(owner) = stale_owner(previous, current) {
            self.stale_focus.insert(owner);
        }
    }

    pub(super) fn drain_pending_actions(&mut self, network: &ShellNetwork) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(mut runtime) = runtime.try_lock() else {
            return;
        };
        for received in self.pending_actions.drain(..) {
            if network.personal_route_is_current(&received.route) {
                runtime.handle_action(&received.envelope, Instant::now());
            } else {
                network.revoke_queued_personal_action_authority(&received.envelope);
            }
        }
    }

    pub(super) fn handles(action: &PersonalFabricAction) -> bool {
        matches!(
            action,
            PersonalFabricAction::RemoteCaptureSignal { .. }
                | PersonalFabricAction::RemoteCaptureChunk { .. }
        )
    }

    pub(super) fn handle_action(&mut self, received: ReceivedPersonalFabricAction) {
        if let Some(runtime) = self.runtime.as_ref()
            && let Ok(mut runtime) = runtime.try_lock()
        {
            runtime.handle_action(&received.envelope, Instant::now());
        } else if self.pending_actions.len() < MAX_REMOTE_CAPTURE_SESSIONS * 4 {
            self.pending_actions.push_back(received);
        }
    }

    pub(super) fn request_teardown_all(&mut self) {
        self.teardown_all_pending = true;
        self.online_peers.clear();
        self.pending_actions.clear();
        self.stale_focus.clear();
        self.try_teardown_all();
    }

    pub(super) fn pump(&mut self, network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(mut runtime) = runtime.try_lock() else {
            return;
        };
        if self.teardown_all_pending || !policy.permits(ContinuityCapability::Media) {
            runtime.teardown_all();
            self.teardown_all_pending = false;
            self.online_peers.clear();
            self.pending_actions.clear();
            self.stale_focus.clear();
            return;
        }
        for (app_id, workspace_id) in std::mem::take(&mut self.stale_focus) {
            runtime.teardown_target_app(&app_id, &workspace_id);
        }

        let online = network
            .device_fabric()
            .snapshots()
            .values()
            .filter(|snapshot| snapshot.presence.online)
            .map(|snapshot| snapshot.owner.clone())
            .collect::<BTreeSet<_>>();
        for peer in self.online_peers.difference(&online) {
            runtime.teardown_peer(peer);
        }
        self.online_peers = online;
        runtime.pump(Instant::now());
        let outgoing = runtime.drain_outgoing();
        drop(runtime);
        for (peer, action) in outgoing {
            if network
                .send_personal_fabric_action(&peer, action.clone())
                .is_err()
                && let Some(runtime) = self.runtime.as_ref()
                && let Ok(mut runtime) = runtime.try_lock()
            {
                runtime.outgoing_failed(&peer, &action);
            }
        }
    }

    fn try_teardown_all(&mut self) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(mut runtime) = runtime.try_lock() else {
            return;
        };
        runtime.teardown_all();
        self.teardown_all_pending = false;
    }
}

fn stale_owner(
    previous: Option<&PhysicalGamepadBinding>,
    current: Option<&PhysicalGamepadBinding>,
) -> Option<(String, String)> {
    previous.and_then(|previous| {
        let same_owner = current.is_some_and(|current| {
            current.app_id == previous.app_id && current.workspace_id == previous.workspace_id
        });
        (!same_owner).then(|| (previous.app_id.clone(), previous.workspace_id.clone()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::portal::CaptureBroker;
    use crate::shell::realtime_session::VerifiedAppRegistration;
    use crate::shell::remote_capture::device::{RemoteCaptureProfile, RemoteVideoFormat};
    use crate::shell::remote_capture::runtime::RemoteCaptureFabricRuntime;

    #[test]
    fn remote_capture_owner_follows_focus_and_teardown_survives_contention() {
        fn owner_changes_only_across_app_or_workspace_focus() {
            let previous = PhysicalGamepadBinding {
                app_id: "so.tilo.Camera".into(),
                workspace_id: "room-a".into(),
                session_id: "tab-a".into(),
            };
            let same_owner = PhysicalGamepadBinding {
                session_id: "tab-b".into(),
                ..previous.clone()
            };
            let other_app = PhysicalGamepadBinding {
                app_id: "so.tilo.Calls".into(),
                ..previous.clone()
            };

            assert_eq!(stale_owner(Some(&previous), Some(&same_owner)), None);
            assert_eq!(
                stale_owner(Some(&previous), Some(&other_app)),
                Some(("so.tilo.Camera".into(), "room-a".into()))
            );
            assert_eq!(
                stale_owner(Some(&previous), None),
                Some(("so.tilo.Camera".into(), "room-a".into()))
            );
            assert_eq!(stale_owner(None, Some(&previous)), None);
        }

        fn policy_teardown_survives_runtime_lock_contention() {
            let runtime = Arc::new(Mutex::new(RemoteCaptureFabricRuntime::production(
                Arc::new(Mutex::new(CaptureBroker::new())),
            )));
            let mut integration = RemoteCaptureIntegration::default();
            integration.install(Arc::clone(&runtime));
            let mut locked = runtime.lock().unwrap();
            locked
                .request(
                    VerifiedAppRegistration::new_verified("so.tilo.Camera", "room-a").unwrap(),
                    DeviceId::new("phone").unwrap(),
                    DeviceId::new("laptop").unwrap(),
                    "Camera".into(),
                    RemoteCaptureProfile::Camera {
                        format: RemoteVideoFormat::Nv12,
                        width: 640,
                        height: 480,
                        frames_per_second: 30,
                    },
                )
                .unwrap();

            integration.request_teardown_all();
            assert!(integration.teardown_all_pending);
            drop(locked);
            integration.try_teardown_all();

            assert!(!integration.teardown_all_pending);
            assert!(runtime.lock().unwrap().view().sessions.is_empty());
        }

        owner_changes_only_across_app_or_workspace_focus();
        policy_teardown_survives_runtime_lock_contention();
    }
}
