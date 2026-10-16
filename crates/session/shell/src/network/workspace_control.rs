//! Route-aware state machine for Tilo workspace control.
//!
//! This deliberately reuses the shell's existing surface stream and remote
//! input messages.  A session starts in `WaitingForConsent`; only an
//! authenticated `SurfaceStreamStart` from the exact target opens the input
//! gate.  Stream end, target/relay departure, revocation, and local close all
//! shut it again before any subsequent input can be published.
//!
//! The frames are relayed to the Remote app, whose decoder takes H.264 only
//! (the broker's stream events carry no codec). While a session exists this
//! peer therefore advertises H.264-only decode (see
//! [`crate::network::media_caps::H264OnlyViewer`]), so the target's share
//! negotiates H.264; frames of any other codec are not relayed.

use iroh::EndpointId;

use crate::network::actor::WorkspaceControlRoute;
use crate::network::behaviour::peer_liveness_handle;
use crate::network::protocol::{NeighbourMsg, SurfaceId};
use crate::network::streaming::InputEvent;
pub use runtime_collab::WorkspaceControlCloseReason;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceControlStatus {
    WaitingForConsent,
    Active {
        surface: SurfaceId,
        width: u32,
        height: u32,
    },
    Closed(WorkspaceControlCloseReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceControlUpdate {
    ConsentGranted {
        surface: SurfaceId,
        width: u32,
        height: u32,
    },
    Frame {
        surface: SurfaceId,
        encoded_h264: Vec<u8>,
        timestamp: u64,
    },
    Closed(WorkspaceControlCloseReason),
}

/// One selected mesh route plus its consent/input lifecycle.
pub struct WorkspaceControlSession {
    workspace: u64,
    local_peer: u64,
    target: EndpointId,
    target_peer: u64,
    route: WorkspaceControlRoute,
    status: WorkspaceControlStatus,
    granted_surface: Option<SurfaceId>,
    pending_start: Option<(SurfaceId, u32, u32)>,
    /// Whether the target's current stream is H.264, the only codec the
    /// Remote app decodes.
    stream_is_h264: bool,
    _h264_only: crate::network::media_caps::H264OnlyViewer,
}

impl WorkspaceControlSession {
    pub fn new(
        workspace: u64,
        local_peer: u64,
        target: EndpointId,
        route: WorkspaceControlRoute,
    ) -> Self {
        Self {
            workspace,
            local_peer,
            target,
            target_peer: peer_liveness_handle(&target),
            route,
            status: WorkspaceControlStatus::WaitingForConsent,
            granted_surface: None,
            pending_start: None,
            stream_is_h264: true,
            _h264_only: crate::network::media_caps::H264OnlyViewer::new(),
        }
    }

    pub fn workspace(&self) -> u64 {
        self.workspace
    }

    pub fn target(&self) -> EndpointId {
        self.target
    }

    pub fn route(&self) -> WorkspaceControlRoute {
        self.route
    }

    pub fn status(&self) -> &WorkspaceControlStatus {
        &self.status
    }

    /// Accept one already-authenticated legacy stream message. Messages from
    /// other publishers/surfaces are ignored, so a room peer cannot grant
    /// consent on the target's behalf.
    pub fn observe(&mut self, message: &NeighbourMsg) -> Option<WorkspaceControlUpdate> {
        self.observe_from(Some(self.target), message)
    }

    pub fn observe_from(
        &mut self,
        transport_peer: Option<EndpointId>,
        message: &NeighbourMsg,
    ) -> Option<WorkspaceControlUpdate> {
        if matches!(self.status, WorkspaceControlStatus::Closed(_)) {
            return None;
        }
        match message {
            NeighbourMsg::PermissionGrant { peer, capability }
                if transport_peer == Some(self.target) && *peer == self.local_peer =>
            {
                let (surface, granted) = parse_control_capability(capability)?;
                if !granted {
                    if self.granted_surface == Some(surface)
                        || self
                            .pending_start
                            .is_some_and(|pending| pending.0 == surface)
                    {
                        return Some(self.close(WorkspaceControlCloseReason::Revoked));
                    }
                    return None;
                }
                self.granted_surface = Some(surface);
                let (pending_surface, width, height) = self.pending_start?;
                if pending_surface == surface {
                    self.status = WorkspaceControlStatus::Active {
                        surface,
                        width,
                        height,
                    };
                    return Some(WorkspaceControlUpdate::ConsentGranted {
                        surface,
                        width,
                        height,
                    });
                }
                None
            }
            NeighbourMsg::SurfaceStreamStart {
                surface,
                source_peer,
                width,
                height,
                codec,
                ..
            } if transport_peer == Some(self.target) && *source_peer == self.target_peer => {
                self.stream_is_h264 = codec == "h264";
                self.pending_start = Some((*surface, *width, *height));
                if self.granted_surface != Some(*surface) {
                    return None;
                }
                self.status = WorkspaceControlStatus::Active {
                    surface: *surface,
                    width: *width,
                    height: *height,
                };
                Some(WorkspaceControlUpdate::ConsentGranted {
                    surface: *surface,
                    width: *width,
                    height: *height,
                })
            }
            NeighbourMsg::SurfaceFrame {
                surface,
                source_peer,
                encoded_h264,
                ts,
                ..
            } if transport_peer == Some(self.target)
                && *source_peer == self.target_peer
                && self.stream_is_h264
                && self.active_surface() == Some(*surface) =>
            {
                Some(WorkspaceControlUpdate::Frame {
                    surface: *surface,
                    encoded_h264: encoded_h264.clone(),
                    timestamp: *ts,
                })
            }
            NeighbourMsg::SurfaceStreamEnd {
                surface,
                source_peer,
                ..
            } if transport_peer == Some(self.target)
                && *source_peer == self.target_peer
                && self.active_surface() == Some(*surface) =>
            {
                Some(self.close(WorkspaceControlCloseReason::Revoked))
            }
            _ => None,
        }
    }

    /// Turn an app input event into the existing shell wire message. This is
    /// the sole gate: before target consent and after every close edge it
    /// returns an honest error and produces no message.
    ///
    /// # Errors
    ///
    /// Returns an error when no consented live surface is active or when the
    /// input event cannot be encoded for the wire protocol.
    pub fn input_message(&self, event: &InputEvent) -> Result<NeighbourMsg, &'static str> {
        let Some(surface) = self.active_surface() else {
            return Err("target has not granted control for a live surface");
        };
        let event = postcard::to_allocvec(event).map_err(|_| "input event could not be encoded")?;
        Ok(NeighbourMsg::RemoteInput {
            peer: self.local_peer,
            surface,
            event,
        })
    }

    pub fn peer_departed(&mut self, peer: EndpointId) -> Option<WorkspaceControlUpdate> {
        let reason = if peer == self.target {
            Some(WorkspaceControlCloseReason::TargetDisconnected)
        } else if matches!(self.route, WorkspaceControlRoute::Relayed { via } if via == peer) {
            Some(WorkspaceControlCloseReason::RelayDisconnected)
        } else {
            None
        }?;
        Some(self.close(reason))
    }

    pub fn deny(&mut self, message: impl Into<String>) -> WorkspaceControlUpdate {
        self.close(WorkspaceControlCloseReason::Denied(message.into()))
    }

    pub fn disconnect(&mut self) -> WorkspaceControlUpdate {
        self.close(WorkspaceControlCloseReason::LocalDisconnect)
    }

    fn active_surface(&self) -> Option<SurfaceId> {
        match self.status {
            WorkspaceControlStatus::Active { surface, .. } => Some(surface),
            WorkspaceControlStatus::WaitingForConsent | WorkspaceControlStatus::Closed(_) => None,
        }
    }

    fn close(&mut self, reason: WorkspaceControlCloseReason) -> WorkspaceControlUpdate {
        self.status = WorkspaceControlStatus::Closed(reason.clone());
        WorkspaceControlUpdate::Closed(reason)
    }
}

fn parse_control_capability(capability: &str) -> Option<(SurfaceId, bool)> {
    let mut parts = capability.split(':');
    if parts.next()? != "workspace-control" {
        return None;
    }
    let surface = parts.next()?.parse().ok()?;
    let granted = match parts.next()? {
        "granted" => true,
        "revoked" => false,
        _ => return None,
    };
    parts.next().is_none().then_some((surface, granted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(byte: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[byte; 32]).public()
    }

    fn start(target: EndpointId, surface: u64) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamStart {
            surface,
            source_peer: peer_liveness_handle(&target),
            width: 1280,
            height: 720,
            codec: "h264".into(),
            ts: 1,
            sender_monotonic_ns_at_send: 1,
            colorimetry: None,
            hdr_static_metadata: None,
            bit_depth: 8,
            app_id: "word-processor".into(),
            tab: 0,
        }
    }

    fn grant(local_peer: u64, surface: u64, granted: bool) -> NeighbourMsg {
        NeighbourMsg::PermissionGrant {
            peer: local_peer,
            capability: format!(
                "workspace-control:{surface}:{}",
                if granted { "granted" } else { "revoked" }
            ),
        }
    }

    #[test]
    fn direct_input_opens_only_after_exact_target_consent() {
        // Phase `no_input_before_target_consent`.
        {
            let target = endpoint(2);
            let session = WorkspaceControlSession::new(
                7,
                11,
                target,
                WorkspaceControlRoute::Direct { target },
            );
            assert!(
                session
                    .input_message(&InputEvent::PointerMotion { x: 1.0, y: 2.0 })
                    .is_err()
            );
        }
        // Phase `exact_target_start_opens_direct_input_path`.
        {
            let target = endpoint(2);
            let attacker = endpoint(9);
            let mut session = WorkspaceControlSession::new(
                7,
                11,
                target,
                WorkspaceControlRoute::Direct { target },
            );
            assert!(
                session
                    .observe_from(Some(attacker), &start(attacker, 44))
                    .is_none()
            );
            assert!(session.observe(&start(target, 44)).is_none());
            assert!(
                session
                    .observe_from(Some(attacker), &grant(11, 44, true))
                    .is_none()
            );
            assert!(
                session
                    .input_message(&InputEvent::PointerMotion { x: 1.0, y: 2.0 })
                    .is_err()
            );
            assert!(matches!(
                session.observe_from(Some(target), &grant(11, 44, true)),
                Some(WorkspaceControlUpdate::ConsentGranted { surface: 44, .. })
            ));
            assert!(matches!(
                session.input_message(&InputEvent::PointerButton {
                    button: 1,
                    pressed: true
                }),
                Ok(NeighbourMsg::RemoteInput { surface: 44, .. })
            ));
        }
    }

    #[test]
    fn lost_revoked_or_denied_sessions_keep_the_input_gate_closed() {
        // Phase `relay_loss_closes_relay_session`.
        {
            let target = endpoint(2);
            let relay = endpoint(3);
            let mut session = WorkspaceControlSession::new(
                7,
                11,
                target,
                WorkspaceControlRoute::Relayed { via: relay },
            );
            session.observe(&grant(11, 44, true));
            session.observe(&start(target, 44));
            assert_eq!(
                session.peer_departed(relay),
                Some(WorkspaceControlUpdate::Closed(
                    WorkspaceControlCloseReason::RelayDisconnected
                ))
            );
            assert!(
                session
                    .input_message(&InputEvent::PointerMotion { x: 1.0, y: 2.0 })
                    .is_err()
            );
        }
        // Phase `target_loss_closes_direct_session`.
        {
            let target = endpoint(2);
            let mut session = WorkspaceControlSession::new(
                7,
                11,
                target,
                WorkspaceControlRoute::Direct { target },
            );
            session.observe(&grant(11, 44, true));
            session.observe(&start(target, 44));
            assert_eq!(
                session.peer_departed(target),
                Some(WorkspaceControlUpdate::Closed(
                    WorkspaceControlCloseReason::TargetDisconnected
                ))
            );
            assert!(
                session
                    .input_message(&InputEvent::PointerMotion { x: 1.0, y: 2.0 })
                    .is_err()
            );
        }
        // Phase `revocation_disconnect_and_denial_keep_gate_closed`.
        {
            let target = endpoint(2);
            let mut revoked = WorkspaceControlSession::new(
                7,
                11,
                target,
                WorkspaceControlRoute::Direct { target },
            );
            revoked.observe(&grant(11, 44, true));
            revoked.observe(&start(target, 44));
            let end = NeighbourMsg::SurfaceStreamEnd {
                surface: 44,
                source_peer: peer_liveness_handle(&target),
                ts: 2,
            };
            assert!(matches!(
                revoked.observe(&end),
                Some(WorkspaceControlUpdate::Closed(
                    WorkspaceControlCloseReason::Revoked
                ))
            ));
            assert!(
                revoked
                    .input_message(&InputEvent::PointerMotion { x: 1.0, y: 2.0 })
                    .is_err()
            );

            let mut disconnected = WorkspaceControlSession::new(
                7,
                11,
                target,
                WorkspaceControlRoute::Direct { target },
            );
            disconnected.disconnect();
            assert!(
                disconnected
                    .input_message(&InputEvent::PointerMotion { x: 1.0, y: 2.0 })
                    .is_err()
            );

            let mut denied = WorkspaceControlSession::new(
                7,
                11,
                target,
                WorkspaceControlRoute::Direct { target },
            );
            denied.deny("not shared with this viewer");
            assert!(
                denied
                    .input_message(&InputEvent::PointerMotion { x: 1.0, y: 2.0 })
                    .is_err()
            );
        }
    }
}
