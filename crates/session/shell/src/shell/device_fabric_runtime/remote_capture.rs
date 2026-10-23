//! Remote-capture coordination inside the device-fabric pump.
//!
//! The capture runtime owns protocol/session authority. This adapter owns the
//! compositor-facing concerns around it: transient lock contention, route
//! freshness, peer presence, policy teardown, and locally verified app focus.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use tokio::sync::oneshot;

use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::device_fabric::PersonalFabricAction;
use crate::shell::network::{ReceivedPersonalFabricAction, ShellNetwork};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::physical_gamepad::PhysicalGamepadBinding;
use crate::shell::portal_dialog::{PortalDialogQueue, ReviewDecision, ReviewRequest};
use crate::shell::remote_capture::device::MAX_REMOTE_CAPTURE_SESSIONS;
use crate::shell::remote_capture::runtime::{RemoteCaptureConsentView, SharedRemoteCaptureRuntime};

#[derive(Default)]
pub(super) struct RemoteCaptureIntegration {
    runtime: Option<SharedRemoteCaptureRuntime>,
    online_peers: BTreeSet<DeviceId>,
    pending_actions: VecDeque<ReceivedPersonalFabricAction>,
    stale_focus: BTreeSet<(String, String)>,
    teardown_all_pending: bool,
    consent_sheets: CaptureConsentSheets,
}

/// Requests from another of the person's devices to use this one's camera,
/// microphone or scanner, asked on the session's consent sheet (the trusted
/// sheet every permission uses), one sheet per request. A request that
/// expires or is withdrawn takes its sheet with it; an answer is carried to
/// the capture runtime as the person gave it.
#[derive(Default)]
pub(super) struct CaptureConsentSheets {
    /// The session's sheet queue, taken when the first request arrives.
    queue: Option<PortalDialogQueue>,
    /// Each request asked about: its sheet's handle and the answer to come.
    asked: BTreeMap<[u8; 16], (String, oneshot::Receiver<ReviewDecision>)>,
}

impl CaptureConsentSheets {
    #[cfg(test)]
    fn with_queue(queue: PortalDialogQueue) -> Self {
        Self {
            queue: Some(queue),
            asked: BTreeMap::new(),
        }
    }

    /// Bring the sheets in line with `consents`, the requests waiting now:
    /// withdraw the sheets of requests that are gone, ask about new ones
    /// (`source_label` names the device asking), and return the answers
    /// given since, as `(request, approved)`.
    pub(super) fn sync(
        &mut self,
        consents: &[RemoteCaptureConsentView],
        source_label: impl Fn(&DeviceId) -> String,
    ) -> Vec<([u8; 16], bool)> {
        if consents.is_empty() && self.asked.is_empty() {
            return Vec::new();
        }
        let queue = self.queue.get_or_insert_with(PortalDialogQueue::global);
        self.asked.retain(|request, (handle, _)| {
            let live = consents
                .iter()
                .any(|consent| consent.request_id == *request);
            if !live {
                queue.cancel(handle);
            }
            live
        });
        for consent in consents {
            if self.asked.contains_key(&consent.request_id) {
                continue;
            }
            let handle = consent_handle(consent.request_id);
            let input = crate::shell::i18n::t(crate::shell::overlay_host::remote_capture_kind_key(
                consent.profile.kind(),
            ));
            let request = ReviewRequest {
                handle: handle.clone(),
                app_id: consent.app_label.clone(),
                title: crate::shell::i18n::t_args(
                    "remote-capture-consent-title",
                    &[("input", input.into())],
                ),
                reason: crate::shell::i18n::t_args(
                    "remote-capture-consent-detail",
                    &[
                        ("source", source_label(&consent.peer).into()),
                        ("app", consent.app_label.as_str().into()),
                    ],
                ),
                resource: String::new(),
                confirm_label: crate::shell::i18n::t("remote-capture-consent-allow"),
            };
            match queue.enqueue_review(request) {
                Ok(answer) => {
                    self.asked.insert(consent.request_id, (handle, answer));
                }
                Err(error) => {
                    tracing::warn!(%error, "a remote capture request could not be asked on the consent sheet");
                }
            }
        }
        let mut answers = Vec::new();
        self.asked
            .retain(|request, (_, answer)| match answer.try_recv() {
                Ok(decision) => {
                    answers.push((*request, decision == ReviewDecision::Confirmed));
                    false
                }
                // A sheet that went without an answer declines.
                Err(oneshot::error::TryRecvError::Closed) => {
                    answers.push((*request, false));
                    false
                }
                Err(oneshot::error::TryRecvError::Empty) => true,
            });
        answers
    }
}

fn consent_handle(request: [u8; 16]) -> String {
    format!("remote-capture/{}", hex::encode(request))
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

        let online = network.online_device_ids();
        for peer in self.online_peers.difference(&online) {
            runtime.teardown_peer(peer);
        }
        self.online_peers = online;
        runtime.pump(Instant::now());
        let outgoing = runtime.drain_outgoing();
        let consents = runtime.consents();
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
        let answers = self.consent_sheets.sync(&consents, |peer| {
            network.device_fabric().snapshots().get(peer).map_or_else(
                || crate::shell::i18n::t("qs-devices-another-certified"),
                |snapshot| snapshot.presence.label.clone(),
            )
        });
        for (request, approved) in answers {
            if let Err(error) = crate::shell::remote_capture::runtime::decide_remote_capture(
                network, request, approved,
            ) {
                tracing::warn!(%error, approved, "a remote capture answer could not be delivered");
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

    fn camera_request(id: u8) -> RemoteCaptureConsentView {
        RemoteCaptureConsentView {
            request_id: [id; 16],
            peer: DeviceId::new("phone").unwrap(),
            app_label: "Camera".into(),
            profile: RemoteCaptureProfile::Camera {
                format: RemoteVideoFormat::Nv12,
                width: 1280,
                height: 720,
                frames_per_second: 30,
            },
        }
    }

    #[test]
    fn a_camera_request_is_asked_on_the_consent_sheet_and_answered_from_it() {
        use crate::shell::portal_dialog::{DialogView, PortalDialogController};
        let queue = PortalDialogQueue::default();
        let mut sheets = CaptureConsentSheets::with_queue(queue.clone());
        let mut controller = PortalDialogController::new(queue);
        let label = |_: &DeviceId| "Phone".to_string();

        assert!(sheets.sync(&[camera_request(1)], label).is_empty());
        controller.pump();
        let view = controller.view().expect("the consent sheet is up");
        assert!(matches!(view, DialogView::Review { .. }));
        assert_eq!(view.title(), "Share this device's camera?");
        assert!(view.detail().contains("Phone"), "{}", view.detail());
        // Asked once, however often the runtime is read.
        assert!(sheets.sync(&[camera_request(1)], label).is_empty());

        assert!(controller.confirm());
        assert_eq!(sheets.sync(&[camera_request(1)], label), [([1; 16], true)]);

        // A request that goes away before an answer takes its sheet along.
        assert!(sheets.sync(&[camera_request(2)], label).is_empty());
        controller.pump();
        assert!(controller.view().is_some());
        assert!(sheets.sync(&[], label).is_empty());
        controller.pump();
        assert!(controller.view().is_none(), "the withdrawn sheet is gone");
    }
}
