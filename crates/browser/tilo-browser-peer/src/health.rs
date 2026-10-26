//! `PeerSnapshot` and `SessionHealth` v1, the shapes Cloud already codes
//! against, derived from the relay session and the page store's save
//! status.
//!
//! Serialized with camelCase field names so the page can hand the value to
//! JavaScript unchanged.

use serde::Serialize;

use crate::session::{Failure, FailureSubject, LocalState, PeerStatus, TransportPhase};

/// `PeerSnapshot` of a certified peer.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSnapshot {
    /// `authenticating` | `admitting` | `ready` | `incomplete` | `failed` |
    /// `closed`.
    pub phase: &'static str,
    /// This device, hex.
    pub device_id: String,
    /// Newest failure message.
    pub error: Option<String>,
    /// Newest failure code.
    pub error_code: Option<&'static str>,
    /// Commits the host has not acknowledged.
    pub outgoing_pending: usize,
    /// `acknowledged` | `pending` | `blocked`.
    pub synchronization: &'static str,
    /// Full health.
    pub health: SessionHealth,
}

/// `SessionHealth` v1.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHealth {
    /// Always 1.
    pub version: u8,
    /// Changes whenever anything below changes.
    pub generation: String,
    /// Relay session.
    pub transport: TransportHealth,
    /// Room admission.
    pub admission: AdmissionHealth,
    /// One entry per document route this session touched.
    pub records: Vec<RecordHealth>,
    /// Supported controls.
    pub capabilities: Capabilities,
    /// Current failures.
    pub failures: Vec<SessionFailure>,
}

/// `SessionHealth.transport`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportHealth {
    /// `connecting` | `connected` | `reconnecting` | `offline` | `failed` |
    /// `closed`.
    pub phase: &'static str,
    /// The current connection completed the certified handshake.
    pub authenticated: bool,
    pub last_relay_contact_at_ms: Option<u64>,
    /// The host is the session peer on this carrier.
    pub last_session_peer_contact_at_ms: Option<u64>,
    /// Reconnect schedule.
    pub retry: Retry,
}

/// `SessionHealth.transport.retry`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    /// Consecutive failed attempts.
    pub attempt: u32,
    pub next_attempt_at_ms: Option<u64>,
    /// An attempt is running now.
    pub in_flight: bool,
}

/// `SessionHealth.admission`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionHealth {
    /// `waiting` | `syncing` | `ready` | `failed`.
    pub phase: &'static str,
    /// Rooms still waiting for the host's `Ready`.
    pub pending_prerequisites: usize,
}

/// `RecordRef`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordRef {
    /// Application id.
    pub app_id: String,
    pub room: String,
    /// Record wire name.
    pub record_id: String,
}

/// `RecordHealth`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordHealth {
    pub record: RecordRef,
    /// `online` | `offline-queued` | `blocked`.
    pub editing: &'static str,
    /// `unknown` | `pending` | `committing` | `durable` | `failed`, from the
    /// page store.
    pub local: &'static str,
    /// Outgoing commits.
    pub outgoing: Outgoing,
    /// Host acknowledgement: `unknown` | `pending` | `covered`.
    pub acknowledgement: &'static str,
    /// Host order of the latest acknowledged commit.
    pub acknowledged_seq: Option<u64>,
}

/// `RecordHealth.outgoing`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outgoing {
    /// Sent, not yet acknowledged.
    pub pending_operations: usize,
    /// Acknowledged this session.
    pub durable_operations: u64,
    /// Refused by the host this session: withdrawn from the replica, never
    /// shared.
    pub refused_operations: u64,
}

/// `SessionHealth.capabilities`.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent flags of the frozen SessionHealth v1 shape"
)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// The session reconnects by itself.
    pub automatic_recovery: bool,
    /// `retry-saving` is honoured.
    pub retry_saving: bool,
    /// `retry-desktop` is honoured.
    pub recover_renderer: bool,
    /// Acknowledgements mean durable on the host.
    pub durable_peer_acknowledgement: bool,
}

/// `SessionFailure`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFailure {
    /// Stable id.
    pub id: String,
    /// `FailureCode`.
    pub code: &'static str,
    /// `transport` | `identity` | `record` | `permission` | `storage` |
    /// `admission` | `renderer` | `asset`.
    pub scope: &'static str,
    /// The desktop surface of a `renderer` or `asset` failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    /// `notice` | `error`.
    pub severity: &'static str,
    /// Safe product copy.
    pub message: String,
    /// Controls that apply.
    pub recovery: Vec<RecoveryRequest>,
    /// When it happened.
    pub occurred_at_ms: u64,
}

/// `RecoveryRequest`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum RecoveryRequest {
    /// Reconnect now.
    RetryConnection,
    /// Certify this browser again.
    Reauthenticate,
    /// Commit the page store's failed writes again.
    RetrySaving,
    /// Ask the host for the rooms' shared state again.
    RefreshCheckpoint,
    /// Recreate a desktop surface's GPU device, surface and renderer.
    RetryDesktop {
        /// The surface.
        surface_id: String,
    },
    /// Fetch and verify a surface's asset again.
    RetryAsset {
        surface_id: String,
        asset_id: String,
    },
}

fn transport_phase(phase: TransportPhase) -> &'static str {
    match phase {
        TransportPhase::Connecting => "connecting",
        TransportPhase::Connected => "connected",
        TransportPhase::Reconnecting => "reconnecting",
        TransportPhase::Offline => "offline",
        TransportPhase::Failed => "failed",
        TransportPhase::Closed => "closed",
    }
}

fn local_state(state: Option<LocalState>) -> &'static str {
    match state {
        None | Some(LocalState::Unknown) => "unknown",
        Some(LocalState::Pending) => "pending",
        Some(LocalState::Committing) => "committing",
        Some(LocalState::Durable) => "durable",
        Some(LocalState::Failed) => "failed",
    }
}

/// The failure's `SessionFailure` projection: id, scope, severity,
/// recovery, surface and asset.
fn session_failure(failure: &Failure) -> SessionFailure {
    let (id, scope, severity, recovery, surface_id, asset_id) = match &failure.subject {
        FailureSubject::Session => {
            let (scope, severity, recovery) = failure_scope(failure.code);
            (
                format!("{}-{}", failure.code, failure.at_ms),
                scope,
                severity,
                recovery,
                None,
                None,
            )
        }
        FailureSubject::Storage => (
            "storage".to_owned(),
            "storage",
            "error",
            vec![RecoveryRequest::RetrySaving],
            None,
            None,
        ),
        FailureSubject::Admission => (
            "admission".to_owned(),
            "admission",
            "error",
            vec![RecoveryRequest::RefreshCheckpoint],
            None,
            None,
        ),
        FailureSubject::Surface(surface) => (
            format!("surface:{surface}"),
            "renderer",
            "error",
            vec![RecoveryRequest::RetryDesktop {
                surface_id: surface.clone(),
            }],
            Some(surface.clone()),
            None,
        ),
        FailureSubject::Asset { surface, asset } => (
            format!("asset:{surface}:{asset}"),
            "asset",
            "error",
            vec![RecoveryRequest::RetryAsset {
                surface_id: surface.clone(),
                asset_id: asset.clone(),
            }],
            Some(surface.clone()),
            Some(asset.clone()),
        ),
    };
    SessionFailure {
        id,
        code: failure.code,
        scope,
        surface_id,
        asset_id,
        severity,
        message: failure.message.clone(),
        recovery,
        occurred_at_ms: failure.at_ms,
    }
}

fn failure_scope(code: &str) -> (&'static str, &'static str, Vec<RecoveryRequest>) {
    match code {
        "certificate-expired" | "certificate-rejected" => {
            ("identity", "error", vec![RecoveryRequest::Reauthenticate])
        }
        // The host no longer admits this person, or another host answers
        // now: the page joins again through the room's door, which no
        // request to this session can do. A revoked device never comes back.
        "not-admitted" | "host-changed" | "device-revoked" => ("identity", "error", Vec::new()),
        "storage-transaction-failed" => ("record", "error", vec![RecoveryRequest::RetrySaving]),
        "permission-required" => ("permission", "error", Vec::new()),
        "protocol-rejected" | "signature-rejected" => ("record", "error", Vec::new()),
        _ => (
            "transport",
            "notice",
            vec![RecoveryRequest::RetryConnection],
        ),
    }
}

impl PeerSnapshot {
    /// Derive the snapshot from a session status.
    pub fn from_status(status: &PeerStatus) -> Self {
        let waiting_rooms = status.rooms.values().filter(|ready| !**ready).count();
        let connected = status.phase == TransportPhase::Connected;
        let phase = match status.phase {
            TransportPhase::Closed => "closed",
            TransportPhase::Failed => "failed",
            TransportPhase::Connected if waiting_rooms == 0 => "ready",
            TransportPhase::Connected => "admitting",
            TransportPhase::Connecting => "authenticating",
            TransportPhase::Reconnecting | TransportPhase::Offline => "incomplete",
        };
        let failures: Vec<SessionFailure> = status.failures.iter().map(session_failure).collect();
        let blocked = status.phase == TransportPhase::Failed;
        let records = status
            .routes
            .iter()
            .map(|(route, route_status)| RecordHealth {
                record: RecordRef {
                    app_id: route.app_id.clone(),
                    room: route.cluster_id.clone(),
                    record_id: route.record.clone(),
                },
                editing: if blocked {
                    "blocked"
                } else if connected {
                    "online"
                } else {
                    "offline-queued"
                },
                local: local_state(route_status.local),
                outgoing: Outgoing {
                    pending_operations: route_status.pending_operations,
                    durable_operations: route_status.durable_operations,
                    refused_operations: route_status.refused_operations,
                },
                acknowledgement: if route_status.pending_operations > 0 {
                    "pending"
                } else if route_status.durable_operations > 0 {
                    "covered"
                } else {
                    "unknown"
                },
                acknowledged_seq: route_status.last_seq,
            })
            .collect();
        let newest = status.failures.last();
        Self {
            phase,
            device_id: hex::encode(status.device),
            error: newest.map(|failure| failure.message.clone()),
            error_code: newest.map(|failure| failure.code),
            outgoing_pending: status.outgoing_pending,
            synchronization: if blocked && status.outgoing_pending > 0 {
                "blocked"
            } else if status.outgoing_pending > 0 {
                "pending"
            } else {
                "acknowledged"
            },
            health: SessionHealth {
                version: 1,
                generation: status.generation.to_string(),
                transport: TransportHealth {
                    phase: transport_phase(status.phase),
                    authenticated: status.authenticated,
                    last_relay_contact_at_ms: status.last_contact_at_ms,
                    last_session_peer_contact_at_ms: status.last_contact_at_ms,
                    retry: Retry {
                        attempt: status.attempt,
                        next_attempt_at_ms: status.next_attempt_at_ms,
                        in_flight: matches!(
                            status.phase,
                            TransportPhase::Connecting | TransportPhase::Reconnecting
                        ),
                    },
                },
                admission: AdmissionHealth {
                    phase: if blocked {
                        "failed"
                    } else if !connected {
                        "waiting"
                    } else if waiting_rooms > 0 {
                        "syncing"
                    } else {
                        "ready"
                    },
                    pending_prerequisites: waiting_rooms,
                },
                records,
                capabilities: Capabilities {
                    automatic_recovery: true,
                    retry_saving: true,
                    recover_renderer: true,
                    durable_peer_acknowledgement: true,
                },
                failures,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn snapshot(failures: Vec<Failure>) -> serde_json::Value {
        let status = PeerStatus {
            phase: TransportPhase::Connected,
            authenticated: true,
            attempt: 0,
            next_attempt_at_ms: None,
            last_contact_at_ms: None,
            rooms: BTreeMap::new(),
            routes: BTreeMap::new(),
            failures,
            outgoing_pending: 0,
            device: [0; 32],
            generation: 7,
            channels: String::new(),
            personal_rooms: std::collections::BTreeSet::new(),
            room_members: std::collections::BTreeMap::new(),
            host_device: [0; 32],
        };
        serde_json::to_value(PeerSnapshot::from_status(&status).health).unwrap_or_default()
    }

    fn failure(code: &'static str, subject: FailureSubject) -> Failure {
        Failure {
            code,
            message: "copy".into(),
            at_ms: 5,
            subject,
        }
    }

    #[test]
    fn every_recovery_kind_is_offered_for_its_subject() {
        let health = snapshot(vec![
            failure("transport-ended", FailureSubject::Session),
            failure("certificate-rejected", FailureSubject::Session),
            failure("not-admitted", FailureSubject::Session),
            failure("storage-transaction-failed", FailureSubject::Storage),
            failure("admission-timeout", FailureSubject::Admission),
            failure(
                "gpu-device-lost",
                FailureSubject::Surface("tilo-desktop-1".into()),
            ),
            failure(
                "asset-unavailable",
                FailureSubject::Asset {
                    surface: "tilo-desktop-1".into(),
                    asset: "font:Noto.ttf".into(),
                },
            ),
        ]);
        let failures = health["failures"].as_array().cloned().unwrap_or_default();
        let projected: Vec<_> = failures
            .iter()
            .map(|failure| {
                (
                    failure["id"].clone(),
                    failure["scope"].clone(),
                    failure["recovery"].clone(),
                )
            })
            .collect();
        assert_eq!(
            projected,
            [
                (
                    serde_json::json!("transport-ended-5"),
                    serde_json::json!("transport"),
                    serde_json::json!([{"kind": "retry-connection"}])
                ),
                (
                    serde_json::json!("certificate-rejected-5"),
                    serde_json::json!("identity"),
                    serde_json::json!([{"kind": "reauthenticate"}])
                ),
                (
                    serde_json::json!("not-admitted-5"),
                    serde_json::json!("identity"),
                    serde_json::json!([])
                ),
                (
                    serde_json::json!("storage"),
                    serde_json::json!("storage"),
                    serde_json::json!([{"kind": "retry-saving"}])
                ),
                (
                    serde_json::json!("admission"),
                    serde_json::json!("admission"),
                    serde_json::json!([{"kind": "refresh-checkpoint"}])
                ),
                (
                    serde_json::json!("surface:tilo-desktop-1"),
                    serde_json::json!("renderer"),
                    serde_json::json!([{"kind": "retry-desktop", "surfaceId": "tilo-desktop-1"}])
                ),
                (
                    serde_json::json!("asset:tilo-desktop-1:font:Noto.ttf"),
                    serde_json::json!("asset"),
                    serde_json::json!([{"kind": "retry-asset", "surfaceId": "tilo-desktop-1", "assetId": "font:Noto.ttf"}])
                ),
            ]
        );
        assert_eq!(failures[5]["surfaceId"], "tilo-desktop-1");
        assert_eq!(failures[6]["assetId"], "font:Noto.ttf");
        assert!(failures[0].get("surfaceId").is_none());
        assert_eq!(health["capabilities"]["retrySaving"], true);
        assert_eq!(health["capabilities"]["recoverRenderer"], true);
    }
}
