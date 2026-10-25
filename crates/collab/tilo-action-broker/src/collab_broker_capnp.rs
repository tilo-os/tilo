#![allow(
    refining_impl_trait,
    reason = "`Promise::from_future(async move {..})` refines capnp-rpc's declared `Promise<.., capnp::Error>` return type, the intended pattern"
)]

//! Native Cap'n Proto server for the Tilo collaboration broker.
//!
//! Answers the `runtime-collab` schema on the shell's collaboration socket.
//!
//! `join_channel` uses the caller's `channel_id` verbatim as the routing
//! key. Document traffic has one path: `send` with a certified signed op.
//! `fetchScrollbackFor` answers from the in-memory op log; an
//! exhausted-but-empty batch triggers no fallback push, callers repeat the
//! fetch instead.

mod approved_checks;
mod external_moderation;
mod local_document_action;
mod subscriptions;
pub use approved_checks::{ApprovedCheckContext, ApprovedCheckServices};
mod execution_activity;
#[cfg(test)]
mod execution_activity_tests;
mod observed_launch;
pub use observed_launch::AppOpen;
mod offload_door;
use execution_activity::{ActivityBus, ActivityTarget, ExecutionScope};
pub use execution_activity::{ExecutionActivity, ExecutionStatus};
#[cfg(test)]
mod app_settings_tests;
#[cfg(test)]
mod authority_sample_tests;
mod canonical_route;
mod committed_document;
mod content_put;
pub use content_put::CONTENT_PUT_REFUSED;
#[cfg(test)]
mod directory_disclosure_tests;
mod directory_pagination;
mod drive_changes;
mod durable_local_invocation;
mod staged_admission;
pub use staged_admission::StagedJob;
use staged_admission::{Admitted, DefiniteOutcome, Delivery};
mod file_clipboard;
#[cfg(test)]
mod history_agents_tests;
mod host_workers;
#[path = "invocation_journal.rs"]
mod invocation_journal;
mod invoker;
mod native_checkout;
#[cfg(test)]
#[path = "collab_broker_capnp/native_effect_recovery_tests.rs"]
mod native_effect_recovery_tests;
mod native_media;
mod native_task_confirmation;
mod native_task_execution;
mod native_task_launch;
mod person_chat;
#[cfg(test)]
mod policy_turn_tests;
mod private_draft;
#[cfg(target_os = "linux")]
mod process_binding;
mod published_invocation;
mod resolved_invocation;
mod route_holds;
mod tracker_runs;
#[path = "workspace_checkpoints.rs"]
mod workspace_checkpoints;
#[path = "workspace_memberships.rs"]
mod workspace_memberships;
pub use crate::services::LaunchScope;
pub use committed_document::{
    DocumentBytes, HeldRevision, NativeDocumentRead, NativeDocumentReadContext,
    NativeDocumentReads, PreparedElsewhere, PreparingElsewhere,
};
use durable_local_invocation::DurableContext;
pub(crate) use invocation_journal::{DurableInvocation, admission_request_digest};
use invocation_journal::{InvocationJournal, InvocationKey, InvocationOwner};

/// A durable local invocation's original caller and its invocation id.
type LocalProvenance = (
    durable_local_invocation::LocalCaller,
    tilo_actions::InvocationId,
);
pub use native_media::{
    AudioOutputFuture, AudioOutputServices, NativeMediaFuture, NativeMediaRequestContext,
    NativeMediaServices,
};
pub use published_invocation::OriginalInvocation;
use published_invocation::{InvocationCaller, OriginalInvocationData};

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use capnp::capability::Promise;
#[cfg(any(test, feature = "test-support"))]
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{mpsc, watch};
#[cfg(any(test, feature = "test-support"))]
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use collab_types::presence::PresencePayload;
use collab_types::{
    ArtifactId, ArtifactKind, RedactedMarker, SignedOp, ValidationError,
    validate_admitted_inbound_op,
};
#[cfg(test)]
use collab_types::{DocumentMutation, document_artifact_id, document_attestation_id};
use runtime_collab::event::CollabEvent;
use runtime_collab::raw::{
    OpAdmission, OpKind, action_catalogue_stream, action_directory, blob_stream, channel_handle,
    collab_service, context_stream, invocation_stream, presence_event, presence_handle,
    presence_payload, presence_stream, realtime_stream, scrollback_batch, stream, task_run_stream,
};
use tilo_action_policy::{
    ActionKind, Answer, Authority, Grant, RoomPolicy, Subject, TreePath, roles,
};
use ui::presence::PresenceKey;

use crate::collab_broker::{
    AuditEntry, BrokerChannels, CrdtRoute, FanoutState, apply_redaction_from_envelope,
};
use crate::collab_outbound::PeerPublisher;
use crate::services::{HostServices, WorkspaceAuthorityHandle as WorkspaceTreeHandle};
use collab_types::{Principal, Role as CapRole};
use tilo_desktop_model::DesktopTree;

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthorizedDocumentRoute {
    artifact: ArtifactId,
    channel: collab_types::ChannelId,
    payload: collab_types::RoutedDocumentPayload,
}

/// How long a peer that began its protocol header has to finish it.
const LAUNCH_MARKER_COMPLETION: Duration = Duration::from_millis(300);

fn invocation_target_peer(requested: u64, current_connection: u64) -> u64 {
    if requested == ui::collaboration::CURRENT_CONNECTION_PEER_ID {
        current_connection
    } else {
        requested
    }
}

#[cfg(test)]
mod realtime_service_boundary_tests;

#[cfg(test)]
mod device_continuity_service_boundary_tests {
    use super::*;
    use crate::services::{
        NearbyTransferRequestContext, NearbyTransferServices, WifiContinuityServices,
    };

    #[derive(Default)]
    struct RecordingWifi {
        calls: Mutex<Vec<runtime_collab::WifiContinuityCommand>>,
    }

    impl WifiContinuityServices for RecordingWifi {
        fn command(
            &self,
            command: runtime_collab::WifiContinuityCommand,
        ) -> runtime_collab::WifiContinuityReply {
            self.calls.lock().expect("calls").push(command);
            runtime_collab::WifiContinuityReply::NotFound
        }
    }

    struct RecordingNearby {
        reply: runtime_collab::NearbyTransferReply,
        calls: Mutex<
            Vec<(
                NearbyTransferRequestContext,
                runtime_collab::NearbyTransferCommand,
            )>,
        >,
    }

    impl RecordingNearby {
        fn answering(reply: runtime_collab::NearbyTransferReply) -> Self {
            Self {
                reply,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl NearbyTransferServices for RecordingNearby {
        fn command(
            &self,
            context: &NearbyTransferRequestContext,
            command: runtime_collab::NearbyTransferCommand,
        ) -> Result<runtime_collab::NearbyTransferReply, String> {
            self.calls
                .lock()
                .expect("calls")
                .push((context.clone(), command));
            Ok(self.reply.clone())
        }

        fn disconnect(&self, _peer_id: u64) {}

        fn changed(
            &self,
            context: &NearbyTransferRequestContext,
            after: u64,
        ) -> Option<crate::services::NearbyTransferChange> {
            self.calls.lock().expect("calls").push((
                context.clone(),
                runtime_collab::NearbyTransferCommand::Snapshot,
            ));
            Some(Box::pin(std::future::ready(after.wrapping_add(1))))
        }
    }

    /// A host that answers commands but cannot say when its state changes.
    struct UnannouncedNearby;

    impl NearbyTransferServices for UnannouncedNearby {
        fn command(
            &self,
            _context: &NearbyTransferRequestContext,
            _command: runtime_collab::NearbyTransferCommand,
        ) -> Result<runtime_collab::NearbyTransferReply, String> {
            Ok(runtime_collab::NearbyTransferReply::Accepted)
        }

        fn disconnect(&self, _peer_id: u64) {}
    }

    fn lookup() -> Vec<u8> {
        serde_json::to_vec(&runtime_collab::WifiContinuityCommand::Lookup {
            ssid: "home".into(),
            security: runtime_collab::PortableWifiSecurity::Wpa2,
        })
        .expect("encode lookup")
    }

    fn snapshot() -> Vec<u8> {
        serde_json::to_vec(&runtime_collab::NearbyTransferCommand::Snapshot)
            .expect("encode snapshot")
    }

    fn files_context() -> NearbyTransferRequestContext {
        NearbyTransferRequestContext::authenticated(
            41,
            NEARBY_TRANSFER_APP_ID,
            "workspace-7",
            CrdtRoute::app(NEARBY_TRANSFER_APP_ID, "workspace-7").channel_key(),
            Some("local-person".into()),
        )
    }

    #[test]
    fn network_wake_uses_only_the_certified_settings_capability() {
        let service = Arc::new(RecordingWifi::default());
        let services = HostServices {
            wifi_continuity: Some(service.clone()),
            ..Default::default()
        };
        let command = runtime_collab::WifiContinuityCommand::NetworkWake {
            profile_uuid: "11111111-1111-4111-8111-111111111111".into(),
            enabled: Some(true),
        };
        let wire = serde_json::to_vec(&command).unwrap();
        delegate_wifi_continuity_command(
            &services,
            WIFI_CONTINUITY_APP_ID,
            WIFI_CONTINUITY_APP_ID,
            &wire,
        )
        .unwrap();
        assert_eq!(service.calls.lock().unwrap().as_slice(), [command]);
        assert!(
            delegate_wifi_continuity_command(
                &services,
                "so.tilo.browser",
                WIFI_CONTINUITY_APP_ID,
                &wire
            )
            .is_err()
        );
        assert!(
            delegate_wifi_continuity_command(
                &services,
                WIFI_CONTINUITY_APP_ID,
                "so.tilo.browser",
                &wire
            )
            .is_err()
        );
        assert!(
            delegate_wifi_continuity_command(
                &services,
                WIFI_CONTINUITY_APP_ID,
                WIFI_CONTINUITY_APP_ID,
                br#"{"command":"network-wake","profile_uuid":"--injection","enabled":true}"#
            )
            .is_err()
        );
        assert_eq!(service.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn wifi_continuity_delegates_only_the_certified_settings_route() {
        let service = Arc::new(RecordingWifi::default());
        let services = HostServices {
            wifi_continuity: Some(service.clone()),
            ..Default::default()
        };
        let reply = delegate_wifi_continuity_command(
            &services,
            WIFI_CONTINUITY_APP_ID,
            WIFI_CONTINUITY_APP_ID,
            &lookup(),
        )
        .expect("certified Settings delegates");
        assert_eq!(
            reply,
            serde_json::to_vec(&runtime_collab::WifiContinuityReply::NotFound).expect("reply")
        );
        assert_eq!(service.calls.lock().expect("calls").len(), 1);

        let other_app = delegate_wifi_continuity_command(
            &services,
            "so.tilo.browser",
            "so.tilo.browser",
            &lookup(),
        )
        .expect_err("only Settings carries Wi-Fi credentials");
        assert!(other_app.to_string().contains("certified Settings"));
        let spoofed = delegate_wifi_continuity_command(
            &services,
            WIFI_CONTINUITY_APP_ID,
            "so.tilo.browser",
            &lookup(),
        )
        .expect_err("caller claim cannot override certified route");
        assert!(spoofed.to_string().contains("certified connection route"));
        let oversized = delegate_wifi_continuity_command(
            &services,
            WIFI_CONTINUITY_APP_ID,
            WIFI_CONTINUITY_APP_ID,
            &vec![b' '; runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES + 1],
        )
        .expect_err("oversized command refused");
        assert!(oversized.to_string().contains("protocol bound"));
        assert_eq!(service.calls.lock().expect("calls").len(), 1);
    }

    #[test]
    fn wifi_continuity_is_unavailable_without_a_host_service() {
        let unsupported = delegate_wifi_continuity_command(
            &HostServices::default(),
            WIFI_CONTINUITY_APP_ID,
            WIFI_CONTINUITY_APP_ID,
            &lookup(),
        )
        .expect_err("host without Wi-Fi fails explicitly");
        assert!(unsupported.to_string().contains("unsupported by this host"));
    }

    #[test]
    fn nearby_transfer_delegates_only_the_certified_files_route() {
        let service = Arc::new(RecordingNearby::answering(
            runtime_collab::NearbyTransferReply::Accepted,
        ));
        let services = HostServices {
            nearby_transfer: Some(service.clone()),
            ..Default::default()
        };
        let context = files_context();
        let reply = delegate_nearby_transfer_command(
            &services,
            &context,
            NEARBY_TRANSFER_APP_ID,
            &snapshot(),
        )
        .expect("certified Files delegates");
        assert_eq!(
            reply,
            serde_json::to_vec(&runtime_collab::NearbyTransferReply::Accepted).expect("reply")
        );
        assert_eq!(
            service.calls.lock().expect("calls").as_slice(),
            &[(context, runtime_collab::NearbyTransferCommand::Snapshot)]
        );

        let other_app = NearbyTransferRequestContext::authenticated(
            41,
            "so.tilo.browser",
            "workspace-7",
            CrdtRoute::app("so.tilo.browser", "workspace-7").channel_key(),
            Some("local-person".into()),
        );
        let refused =
            delegate_nearby_transfer_command(&services, &other_app, "so.tilo.browser", &snapshot())
                .expect_err("only Files sends to nearby devices");
        assert!(refused.to_string().contains("certified Files"));
        let spoofed = delegate_nearby_transfer_command(
            &services,
            &files_context(),
            "so.tilo.browser",
            &snapshot(),
        )
        .expect_err("caller claim cannot override certified route");
        assert!(spoofed.to_string().contains("certified connection route"));
        let invalid = delegate_nearby_transfer_command(
            &services,
            &files_context(),
            NEARBY_TRANSFER_APP_ID,
            br#"{"command":"send"}"#,
        );
        assert!(invalid.is_err(), "malformed command refused");
        assert_eq!(service.calls.lock().expect("calls").len(), 1);
    }

    #[test]
    fn airdrop_delegation_seals_person_and_rejects_application_trust_claims() {
        use runtime_collab::{NearbyTransferCommand as Command, NearbyTransferReply as Reply};
        let service = Arc::new(RecordingNearby::answering(Reply::Accepted));
        let services = HostServices {
            nearby_transfer: Some(service.clone()),
            ..Default::default()
        };
        let context = files_context();
        let command = Command::SendAirDrop {
            source_path: b"/approved/report".to_vec(),
            address: "127.0.0.1".into(),
            port: 443,
        };
        let json = serde_json::to_vec(&command).unwrap();
        delegate_nearby_transfer_command(&services, &context, NEARBY_TRANSFER_APP_ID, &json)
            .unwrap();
        let calls = service.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.person(), Some("local-person"));
        assert_eq!(calls[0].1, command);
        drop(calls);
        let mut spoof = serde_json::from_slice::<serde_json::Value>(&json).unwrap();
        for key in ["person", "certificate_sha256", "verified", "remember"] {
            spoof[key] = "app claim".into();
            assert!(
                delegate_nearby_transfer_command(
                    &services,
                    &context,
                    NEARBY_TRANSFER_APP_ID,
                    &serde_json::to_vec(&spoof).unwrap()
                )
                .is_err()
            );
            spoof.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(
            service.calls.lock().unwrap().len(),
            1,
            "spoofed authority never reaches the service"
        );
    }

    #[test]
    fn nearby_transfer_refuses_an_unbounded_host_reply() {
        let service = Arc::new(RecordingNearby::answering(
            runtime_collab::NearbyTransferReply::Rejected {
                reason: String::new(),
            },
        ));
        let services = HostServices {
            nearby_transfer: Some(service),
            ..Default::default()
        };
        let error = delegate_nearby_transfer_command(
            &services,
            &files_context(),
            NEARBY_TRANSFER_APP_ID,
            &snapshot(),
        )
        .expect_err("invalid reply never reaches the app");
        assert!(error.to_string().contains("invalid bounded reply"));
    }

    #[test]
    fn only_the_certified_files_route_waits_for_nearby_changes() {
        let service = Arc::new(RecordingNearby::answering(
            runtime_collab::NearbyTransferReply::Accepted,
        ));
        let services = HostServices {
            nearby_transfer: Some(service.clone()),
            ..Default::default()
        };
        let change = nearby_transfer_change(&services, &files_context(), 4)
            .expect("Files waits on its own route");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(runtime.block_on(change), 5, "the host's next change");
        assert_eq!(
            service.calls.lock().expect("calls")[0].0,
            files_context(),
            "the host learns which connection waits, for its handles"
        );

        for (app, workspace) in [
            ("so.tilo.browser", "workspace-7"),
            (NEARBY_TRANSFER_APP_ID, ""),
        ] {
            let context = NearbyTransferRequestContext::authenticated(
                41,
                app,
                workspace,
                CrdtRoute::app(app, workspace).channel_key(),
                Some("local-person".into()),
            );
            let refused = nearby_transfer_change(&services, &context, 0)
                .err()
                .expect("only the certified Files route in a workspace waits");
            assert!(refused.to_string().contains("certified Files"));
        }
        assert_eq!(
            service.calls.lock().expect("calls").len(),
            1,
            "a refused route never reaches the host"
        );
        for services in [
            HostServices::default(),
            HostServices {
                nearby_transfer: Some(Arc::new(UnannouncedNearby)),
                ..Default::default()
            },
        ] {
            let unannounced = nearby_transfer_change(&services, &files_context(), 0)
                .err()
                .expect("a host that cannot announce changes says so");
            assert_eq!(unannounced.kind, capnp::ErrorKind::Unimplemented);
        }
    }

    /// Speaker groups that record each wait and answer it with the next
    /// revision, or (`announces: false`) cannot say when they change.
    struct WaitingSpeakers {
        announces: bool,
        waits: Mutex<Vec<u64>>,
    }

    impl crate::services::SpeakerServices for WaitingSpeakers {
        fn command(
            &self,
            _command: runtime_collab::SpeakerCommand,
        ) -> runtime_collab::SpeakerReply {
            runtime_collab::SpeakerReply::Done { group: None }
        }

        fn changed(&self, after: u64) -> Option<crate::services::SpeakerChange> {
            self.waits.lock().expect("waits").push(after);
            self.announces
                .then(|| Box::pin(std::future::ready(after.wrapping_add(1))) as _)
        }
    }

    #[test]
    fn only_the_certified_settings_route_waits_for_speaker_changes() {
        let service = Arc::new(WaitingSpeakers {
            announces: true,
            waits: Mutex::new(Vec::new()),
        });
        let services = HostServices {
            speakers: Some(service.clone()),
            ..Default::default()
        };
        let change =
            speaker_change(&services, "so.tilo.settings", 4).expect("Settings waits on its route");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(runtime.block_on(change), 5, "the host's next change");

        for app in [NEARBY_TRANSFER_APP_ID, "so.tilo.browser", ""] {
            let refused = speaker_change(&services, app, 0)
                .err()
                .expect("only the certified Settings route waits");
            assert!(refused.to_string().contains("certified Settings"));
        }
        assert_eq!(
            *service.waits.lock().expect("waits"),
            [4],
            "a refused route never reaches the host"
        );

        for services in [
            HostServices::default(),
            HostServices {
                speakers: Some(Arc::new(WaitingSpeakers {
                    announces: false,
                    waits: Mutex::new(Vec::new()),
                })),
                ..Default::default()
            },
        ] {
            let unannounced = speaker_change(&services, "so.tilo.settings", 0)
                .err()
                .expect("a host that cannot announce changes says so");
            assert_eq!(unannounced.kind, capnp::ErrorKind::Unimplemented);
        }
    }

    #[test]
    fn nearby_transfer_is_unavailable_without_a_host_service() {
        let unsupported = delegate_nearby_transfer_command(
            &HostServices::default(),
            &files_context(),
            NEARBY_TRANSFER_APP_ID,
            &snapshot(),
        )
        .expect_err("host without nearby radios fails explicitly");
        assert!(unsupported.to_string().contains("unsupported by this host"));
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollabLaunchHandoff {
    protocol: u16,
    app_id: String,
    workspace_id: String,
    #[serde(default)]
    executing_peer: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedJoinIdentity {
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    /// A foreign device in a room it does not govern: its role is capped.
    capped: bool,
}

impl VerifiedJoinIdentity {
    fn of(cert: &tilo_user_identity::DeviceCert, capped: bool) -> Self {
        Self {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            capped,
        }
    }

    /// Whether `cluster_id` is this identity's own person room: the room
    /// every device its root certified owns, and no other device enters.
    fn owns_person_room(&self, cluster_id: &str) -> bool {
        uuid::Uuid::parse_str(cluster_id)
            .is_ok_and(|room| collab_types::is_person_room(&self.user_public_key, &room))
    }
}

/// Whether a device is held to the foreign-device cap in `room`, as far as
/// `authority` knows who governs it; `device` `None` is this host's own.
fn cap_applies_in(
    authority: Option<&dyn super::peer_actions::CertificateAuthority>,
    room: &str,
    foreign: bool,
    device: Option<&[u8; 32]>,
) -> bool {
    let governs = authority
        .zip(uuid::Uuid::parse_str(room).ok())
        .is_some_and(|(authority, room)| authority.governs(room, device));
    // A foreign device is held to the bound only in a room it does not govern.
    foreign && !governs
}

/// [`cap_applies_in`] for `cert`'s device.
fn cert_capped_in(
    authority: Option<&dyn super::peer_actions::CertificateAuthority>,
    room: &str,
    cert: &tilo_user_identity::DeviceCert,
) -> bool {
    cap_applies_in(
        authority,
        room,
        cert.capabilities.foreign,
        Some(&cert.device_public_key),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedJoinIdentity {
    /// Only an actual broker-allocated private descriptor can carry this scope.
    host_scope: Option<LocalHostWorkerScope>,
    worker_registration: Option<PrivateWorkerRegistration>,
    certificate: Option<tilo_user_identity::DeviceCert>,
    verified: VerifiedJoinIdentity,
    principal_id: String,
    /// Informational display label announced by this authenticated session.
    /// Authorization always uses `principal_id`, never this handle.
    display_handle: Option<String>,
    /// Workspace admission role from the live owner/capability roster.
    /// Certified document operations use this directly instead of requiring
    /// a second, channel-local role attestation for the same person. For a
    /// delegated agent it is the agent's role
    /// ([`roles::agent_policy::role`]): its preset's, capped by its person's.
    role: CapRole,
    /// Delegated agent principal for this channel. The certified device still
    /// proves which person launched it, while policy uses this revocable id.
    agent_id: Option<String>,
    /// The agent is one this person's own device handed the session to (an
    /// MCP client): it carries the delegation a device gives its agents,
    /// not grants the room holds for it.
    session_delegated: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct BrokerActor {
    id: String,
    is_agent: bool,
    /// A person acting from a foreign device in a room it does not govern.
    capped: bool,
    /// The person an agent acts for, when one delegated it: the agent then
    /// may do only what both may.
    for_person: Option<String>,
    /// The person's own device delegated the agent for this session: it
    /// carries the delegation a device gives its agents (read and undoable
    /// actions, outside ones asking first, no governance).
    session_delegated: bool,
    /// The issue whose run the agent works in, by its address: grants
    /// naming the issue decide for the run as the agent's own do.
    issue: Option<String>,
}

impl BrokerActor {
    #[must_use]
    pub fn person(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_agent: false,
            capped: false,
            for_person: None,
            session_delegated: false,
            issue: None,
        }
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_agent: true,
            capped: false,
            for_person: None,
            session_delegated: false,
            issue: None,
        }
    }

    /// Installed bot `installation`, acting for the person who installed it:
    /// it may do only what both its grant and that person may.
    #[must_use]
    pub fn bot(installation: &str, installer: impl Into<String>) -> Self {
        Self {
            id: format!("bot:{installation}"),
            is_agent: false,
            capped: false,
            for_person: Some(installer.into()),
            session_delegated: false,
            issue: None,
        }
    }

    /// The same agent, acting for `person`.
    #[must_use]
    pub fn for_person(mut self, person: impl Into<String>) -> Self {
        self.for_person = Some(person.into());
        self
    }

    /// The same actor, held to the foreign-device bound when `capped`: from
    /// a device that is not its person's.
    #[must_use]
    pub fn capped(mut self, capped: bool) -> Self {
        self.capped = capped;
        self
    }

    /// The same agent, delegated by its person's own device for this
    /// session.
    #[must_use]
    pub fn session_delegated(mut self) -> Self {
        self.session_delegated = true;
        self
    }

    /// The same agent, working in a run of the issue at `issue`.
    #[must_use]
    pub fn working_on(mut self, issue: impl Into<String>) -> Self {
        self.issue = Some(issue.into());
        self
    }

    /// The policy principal id (`agent:`-prefixed for an agent).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn is_agent(&self) -> bool {
        self.is_agent
    }

    /// The grant subject this actor is when it is not a person.
    fn delegate(&self) -> Option<Subject> {
        if let Some(app) = self.id.strip_prefix("app:") {
            return Some(Subject::App(app.to_owned()));
        }
        // A person's policy id is lowercase hex, so it never starts with
        // `bot:` and the prefix cannot misread a person.
        if let Some(bot) = self.id.strip_prefix("bot:") {
            return Some(Subject::Bot(bot.to_owned()));
        }
        self.is_agent
            .then(|| Subject::agent(self.id.strip_prefix("agent:").unwrap_or(&self.id)))
    }

    /// What this actor may do in `workspace`: a person's own authority (held
    /// to `foreign_bound` from a device not theirs); an agent's bounded by
    /// the person it acts for; an application, or an agent nobody delegated,
    /// only what the room grants it.
    pub(crate) fn with_authority_in<T>(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        foreign_bound: &[Grant],
        decide: impl FnOnce(&Authority<'_>) -> T,
    ) -> T {
        match (self.delegate(), &self.for_person) {
            (None, _) => {
                decide(&workspace.person_authority_on(&self.id, self.capped, foreign_bound))
            }
            (Some(delegate), Some(person)) => {
                let carried = self.session_grants(&workspace.room().to_string(), person);
                decide(
                    &workspace
                        .person_authority_on(person, self.capped, foreign_bound)
                        .bounded_by(self.party(&workspace.policy.grants, delegate, &carried)),
                )
            }
            // A delegation that names no person still never governs the room.
            (Some(delegate), None) => {
                let room = workspace.room().to_string();
                let launch = roles::agent_policy::launch_bound(&room, delegate.clone(), "");
                let bound = launch.party(&workspace.policy.grants);
                decide(&workspace.subject_authority(delegate).bounded_by(bound))
            }
        }
    }

    /// The delegate `delegate` this actor is, over `grants` and what it
    /// `carried`, and the issue it works on.
    fn party<'g>(
        &self,
        grants: &'g [Grant],
        delegate: Subject,
        carried: &'g [Grant],
    ) -> tilo_action_policy::Party<'g> {
        let party = tilo_action_policy::Party::new(vec![delegate], grants).with_grants(carried);
        match &self.issue {
            Some(issue) => party.also_as(Subject::Task(issue.clone())),
            None => party,
        }
    }

    /// The delegation a session-delegated agent carries in `room`, given by
    /// `person`; nothing for any other actor.
    fn session_grants(&self, room: &str, person: &str) -> Vec<Grant> {
        if !self.session_delegated {
            return Vec::new();
        }
        let agent = self.id.strip_prefix("agent:").unwrap_or(&self.id);
        roles::AgentPreset::Operator.grants(room, agent, &Subject::Person(person.to_owned()))
    }

    /// Whether this actor is a person acting from their own device: the one
    /// whose own gesture answers a grant that asks first.
    pub(crate) fn direct(&self) -> bool {
        self.delegate().is_none() && !self.capped
    }

    /// The person this actor is, when a person acts from their own device:
    /// the only actor a host lets govern a room.
    #[must_use]
    pub fn direct_person(&self) -> Option<&str> {
        self.direct().then_some(self.id.as_str())
    }

    /// The answer for an action of `kind` at `node` in `workspace` at `now`.
    pub(crate) fn answer_in(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        node: &TreePath,
        kind: ActionKind,
        now: u64,
    ) -> Answer {
        self.verdict_in(workspace, node, kind, false, now).0
    }

    /// The answer for the action `action` on `resource` (of kind
    /// `resource_kind`) the app `app` publishes, which `need` describes, in
    /// `workspace` at `now`: decided at its node, and as privileged when
    /// `need` is ([`tilo_action_policy::Verdict::privileged`]).
    pub(crate) fn action_answer_in(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        app: &str,
        (resource_kind, resource, action): (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
        now: u64,
    ) -> Answer {
        self.action_verdict_in(workspace, app, (resource_kind, resource, action), need, now)
            .0
    }

    /// [`Self::action_answer_in`] for running the action: the answer, and the
    /// `Once` grants running it spends ([`tilo_action_policy::Verdict::spent`]).
    pub(crate) fn action_verdict_in(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        app: &str,
        (resource_kind, resource, action): (&str, &str, &str),
        need: &tilo_actions::ActionNeed,
        now: u64,
    ) -> (Answer, Vec<tilo_action_policy::GrantId>) {
        let node = tilo_actions::action_node(
            &workspace.node(),
            app,
            resource_kind,
            resource,
            need.field.as_deref(),
            action,
        );
        self.verdict_in(workspace, &node, need.kind, need.privileged, now)
    }

    /// Admit `action`, published on `surface`, in `workspace` at `now`:
    /// whether the room's grants ask first (`true`), when the invocation
    /// then needs the confirmation of the person it is made for, and the
    /// `Once` grants admitting it spends. Decided on one sample: the action
    /// where it is published, by the run's agent working on its issue when
    /// the call is a run's (`run`); and, for a person, a change of the
    /// route record it acts on, so a person restricted on one record cannot
    /// act on it through an action they may otherwise use.
    ///
    /// # Errors
    /// The refusal: a reserved or malformed route record, another agent
    /// than the run's, or grants that refuse the action or the record.
    pub(crate) fn admit_published_in(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        now: u64,
        run: Option<&crate::services::NativeTaskBinding>,
        (surface, action): (&PublishedSurface, &PublishedAction),
    ) -> Result<(bool, Vec<tilo_action_policy::GrantId>), String> {
        use tilo_action_policy::Refused;
        let refused = || "caller no longer authorized for invocation".to_owned();
        let record = if self.is_agent {
            None
        } else {
            Some(crate::published_authorization::canonical_record(
                &surface.route.record_id,
            )?)
        };
        let need = action.contract.need();
        let node = published_node(workspace, &surface.app_id, action);
        // The run's agent alone acts in it, as the agent working on the issue.
        let working;
        let actor = match run {
            Some(run)
                if self.is_agent
                    && self.id.strip_prefix("agent:").unwrap_or(&self.id) == run.agent_id =>
            {
                working = self.clone().working_on(&run.task_id);
                &working
            }
            Some(_) => return Err("only the run's agent acts in it".into()),
            None => self,
        };
        let (answer, spent) = actor.verdict_in(workspace, &node, need.kind, need.privileged, now);
        let record = record.map(|record| {
            let node = collab_types::validator::document_node(
                &workspace.policy.room,
                &surface.app_id,
                &record,
            );
            self.answer_in(workspace, &node, need.kind, now)
        });
        // The broker reaches the person on their own device to ask them.
        let asks = match tilo_action_policy::admit(
            tilo_action_policy::Invoked {
                answer,
                attended: action.contract.is_attended(),
                record,
            },
            tilo_action_policy::Asker {
                reachable: true,
                delegated: self.delegate().is_some(),
            },
        ) {
            Ok(admission) => admission == tilo_action_policy::Admission::AsksFirst,
            Err(Refused::Action | Refused::Record) => return Err(refused()),
        };
        Ok((asks, spent))
    }

    fn verdict_in(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        now: u64,
    ) -> (Answer, Vec<tilo_action_policy::GrantId>) {
        let bound = roles::foreign_device_bound();
        self.with_authority_in(workspace, &bound, |authority| {
            let mut verdict = authority.decide(node, kind, &tilo_desktop_model::context_at(now));
            if privileged {
                verdict = verdict.privileged();
            }
            (verdict.answer, verdict.spent())
        })
    }

    /// Whether this actor may take an action of `kind` at `node` in
    /// `workspace` at `now` ([`Answer::admits`]).
    pub(crate) fn may_in(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        node: &TreePath,
        kind: ActionKind,
        now: u64,
    ) -> bool {
        self.answer_in(workspace, node, kind, now)
            .admits(self.direct())
    }

    /// Whether this actor takes part in `workspace` at `now`: it may at
    /// least read the room.
    pub(crate) fn reads_in(&self, workspace: &tilo_desktop_model::Workspace, now: u64) -> bool {
        self.answer_in(workspace, &workspace.node(), ActionKind::Read, now) != Answer::Deny
    }
}

/// The node of `action` published by `app` in `workspace`.
pub(crate) fn published_node(
    workspace: &tilo_desktop_model::Workspace,
    app: &str,
    action: &PublishedAction,
) -> TreePath {
    tilo_actions::action_node(
        &workspace.node(),
        app,
        &action.resource_kind,
        &action.resource,
        action.contract.field.as_deref(),
        &action.action,
    )
}

/// The node of opening the application `app` in `workspace`: launching it is
/// an action of the app itself.
pub(crate) fn app_node(workspace: &tilo_desktop_model::Workspace, app: &str) -> TreePath {
    workspace.node().app(app)
}

impl AdmittedJoinIdentity {
    /// This admission handed to `agent` by the person's own device, if any.
    fn delegated_to(mut self, agent: Option<&str>) -> Self {
        if let Some(agent) = agent {
            self.agent_id = Some(agent.to_owned());
            self.session_delegated = true;
        }
        self
    }

    fn broker_actor(&self) -> BrokerActor {
        let actor = match &self.agent_id {
            Some(agent) => {
                let actor = BrokerActor::agent(format!("agent:{agent}"))
                    .for_person(self.principal_id.clone());
                if self.session_delegated {
                    actor.session_delegated()
                } else {
                    actor
                }
            }
            None => BrokerActor::person(self.principal_id.clone()),
        };
        actor.capped(self.verified.capped)
    }

    /// The caller this admitted connection is, confined to its room.
    fn caller(&self, room: &str) -> tilo_actions::remote::Caller {
        let foreign = self
            .certificate
            .as_ref()
            .is_some_and(|cert| cert.capabilities.foreign);
        match &self.agent_id {
            Some(agent) => tilo_actions::remote::Caller::local_agent(agent.clone()),
            None => tilo_actions::remote::Caller::local_person(self.principal_id.clone()),
        }
        .in_room(room)
        .from_foreign_device(foreign)
    }

    /// The caller this certified identity is in `room`, from its own
    /// device: foreign when it was held to the cap there or the host keeps
    /// a stricter restriction for it.
    fn certified_caller(&self, room: &str, restricted: bool) -> tilo_actions::remote::Caller {
        let verified = &self.verified;
        match &self.agent_id {
            Some(agent) => tilo_actions::remote::Caller::delegated_agent(
                agent.clone(),
                verified.user_public_key,
                verified.device_public_key,
            ),
            None => tilo_actions::remote::Caller::authenticated(
                verified.user_public_key,
                verified.device_public_key,
            ),
        }
        .in_room(room)
        .from_foreign_device(verified.capped || restricted)
    }

    /// Run `decide` with what this identity may do in `room` (its person
    /// room, or a workspace in `tree`).
    /// [`Self::with_authority`] over the committed authority of
    /// `membership`, borrowed for the one decision.
    fn with_current_authority<T>(
        &self,
        membership: Option<&WorkspaceTreeHandle>,
        room: &str,
        decide: impl FnOnce(&Authority<'_>) -> T,
    ) -> Result<T, String> {
        match membership {
            Some(membership) => membership
                .decide_in_tree(|tree| self.with_authority(Some(tree), room, decide))
                .and_then(std::convert::identity),
            None => self.with_authority(None, room, decide),
        }
    }

    fn with_authority<T>(
        &self,
        tree: Option<&DesktopTree>,
        room: &str,
        decide: impl FnOnce(&Authority<'_>) -> T,
    ) -> Result<T, String> {
        let bound = roles::foreign_device_bound();
        if (self.agent_id.is_none() || self.session_delegated)
            && self.verified.owns_person_room(room)
        {
            // The person room's devices are its owner's; a foreign one is
            // held to the foreign-device bound there as anywhere, and an
            // agent one of them was handed to, to that delegation too.
            let policy = RoomPolicy::new(room, self.principal_id.clone());
            let authority =
                policy.person_on_device(&self.principal_id, self.verified.capped, &bound);
            let actor = self.broker_actor();
            let carried = actor.session_grants(room, &self.principal_id);
            let authority = match actor.delegate() {
                Some(delegate) => authority.bounded_by(actor.party(&[], delegate, &carried)),
                None => authority,
            };
            return Ok(decide(&authority));
        }
        let workspace = tree
            .ok_or("the identity has no workspace authority")?
            .workspaces
            .iter()
            .find(|workspace| workspace.room().to_string() == room)
            .ok_or("the identity's workspace is no longer hosted")?;
        Ok(self
            .broker_actor()
            .with_authority_in(workspace, &bound, decide))
    }
}

/// Answer a remote-action request with `error`, refusing it.
fn refused(
    message: &tilo_actions::remote::RemoteActionMessage,
    error: tilo_actions::remote::ActionHostError,
    results: channel_handle::RemoteActionResults,
) -> Promise<(), capnp::Error> {
    match tilo_actions::remote::refuse(message, error) {
        Some(refusal) => reply_frame(refusal, results),
        None => Promise::err(capnp::Error::failed(
            "only remote-action requests are answered".into(),
        )),
    }
}

/// Whether `evidence` is what the confirmation `policy` asks for. Minting
/// the grant that binds it to one delivery is
/// [`CapnpBrokerState::confirmation_token`]'s job.
fn confirmation_satisfied(
    policy: &tilo_actions::ConfirmationPolicy,
    evidence: Option<&tilo_actions::remote::Confirmation>,
) -> bool {
    use tilo_actions::remote::Confirmation;
    match (policy, evidence) {
        (tilo_actions::ConfirmationPolicy::None, _)
        | (tilo_actions::ConfirmationPolicy::Confirm, Some(Confirmation::Confirmed)) => true,
        (
            tilo_actions::ConfirmationPolicy::Phrase { phrase: expected },
            Some(Confirmation::Phrase(phrase)),
        ) => phrase.len() <= DIRECTORY_MAX_CONFIRMATION_BYTES && phrase == expected,
        _ => false,
    }
}

#[must_use]
pub fn principal_id_for_root(user_public_key: [u8; 32]) -> String {
    match Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(user_public_key)) {
        Principal::Person(id) => id,
        _ => hex::encode(user_public_key),
    }
}

#[cfg(test)]
fn admit_verified_join(
    tree: &DesktopTree,
    route: &CrdtRoute,
    verified: VerifiedJoinIdentity,
) -> Result<AdmittedJoinIdentity, String> {
    admit_verified_join_at(
        crate::services::room_in(tree, &route.cluster_id),
        route,
        verified,
        tilo_clock::try_unix_now().ok_or("legacy clock unavailable")?,
    )
}
/// Admit a verified identity into `route`'s room. `workspace` is the
/// committed workspace of exactly `route.cluster_id` from one authority
/// sample (`None` when this host has none).
fn admit_verified_join_at(
    workspace: Option<&tilo_desktop_model::Workspace>,
    route: &CrdtRoute,
    verified: VerifiedJoinIdentity,
    now: u64,
) -> Result<AdmittedJoinIdentity, String> {
    let principal_id = principal_id_for_root(verified.user_public_key);
    // The person room has no roster to consult: its members are, by the
    // room id's derivation, exactly the devices this root certified, each
    // acting as the room's owner — a foreign device with the foreign-device
    // cap's role, there as in any room. Any other room id is a workspace.
    if verified.owns_person_room(&route.cluster_id) {
        let role = CapRole::Owner;
        return Ok(AdmittedJoinIdentity {
            host_scope: None,
            worker_registration: None,
            certificate: None,
            verified,
            principal_id,
            role,
            display_handle: None,
            agent_id: None,
            session_delegated: false,
        });
    }
    let workspace = workspace
        .ok_or_else(|| "certified join names a workspace this session does not host".to_string())?;
    // Taking part is being allowed to read the room now: as a member, or
    // by a grant for a while.
    if !BrokerActor::person(principal_id.clone()).reads_in(workspace, now) {
        return Err("certified collaboration identity is not a workspace member".into());
    }
    let role = workspace.role_of(&principal_id).unwrap_or(CapRole::Viewer);
    Ok(AdmittedJoinIdentity {
        host_scope: None,
        worker_registration: None,
        certificate: None,
        verified,
        principal_id,
        role,
        display_handle: None,
        agent_id: None,
        session_delegated: false,
    })
}

#[cfg(test)]
fn admit_agent_delegation(
    tree: &DesktopTree,
    route: &CrdtRoute,
    admitted: &mut AdmittedJoinIdentity,
    agent_id: &str,
) -> Result<(), String> {
    admit_agent_delegation_at(
        crate::services::room_in(tree, &route.cluster_id),
        admitted,
        agent_id,
        tilo_clock::try_unix_now().ok_or("legacy clock unavailable")?,
    )
}
/// Delegate an admitted administrator's session to `agent_id`. `workspace`
/// is the committed workspace of exactly `route.cluster_id` from the same
/// authority sample that admitted the person.
fn admit_agent_delegation_at(
    workspace: Option<&tilo_desktop_model::Workspace>,
    admitted: &mut AdmittedJoinIdentity,
    agent_id: &str,
    now: u64,
) -> Result<(), String> {
    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
        agent: agent_id.to_owned(),
    }
    .validate()?;
    let workspace =
        workspace.ok_or_else(|| "agent delegation names an unavailable workspace".to_string())?;
    if admitted.verified.capped
        || !workspace.may_govern_at(&admitted.principal_id, roles::governance::GRANT, now)
    {
        return Err("only a workspace administrator may launch a delegated agent session".into());
    }
    // The agent takes part with the role its room admission (its preset)
    // grants, never more than its person's: revoking either ends it here,
    // and every revalidation of the session asks again.
    let granted = workspace
        .agent_role_at(agent_id, now)
        .ok_or("the requested agent is not an active workspace member")?;
    admitted.role = roles::agent_policy::role(admitted.role, granted);
    admitted.agent_id = Some(agent_id.to_string());
    admitted.display_handle = Some(agent_id.to_string());
    Ok(())
}

/// Whether the launched app `app` may join `route` in its person's own
/// room: only a record of the person's whose kind its surface declares
/// (its own, or another app's it reads). The person's room holds nothing
/// else for an app.
fn person_record_join(app: &str, route: &CrdtRoute) -> Result<(), String> {
    let kind = collab_types::person_record_kind(&route.record_id)
        .ok_or("an app joins the person's room only for one of their records")?;
    if !ui::app_surface::keeps_person_record(app, kind) {
        return Err(format!(
            "{app} declares no record of the person's of kind {kind}"
        ));
    }
    Ok(())
}

fn verify_session_author(
    admitted: &AdmittedJoinIdentity,
    author: collab_types::Ed25519PublicKey,
) -> Result<(), String> {
    if author.as_bytes() != &admitted.verified.device_public_key {
        return Err("signed op author does not match the certified device for this session".into());
    }
    Ok(())
}

fn verify_join_presentation(
    route: &CrdtRoute,
    presentation: &runtime_collab::CertifiedJoinIdentity,
    expected_user_public_key: &[u8; 32],
    now_ns: u64,
    revoked: bool,
    enrolled: bool,
) -> Result<VerifiedJoinIdentity, String> {
    if presentation.workspace_id != route.cluster_id {
        return Err("certified join is scoped to a different workspace".into());
    }
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    tilo_user_identity::verify_device_cert_at(&cert, now_ns)
        .map_err(|error| format!("device certificate was rejected: {error}"))?;
    if &cert.user_public_key != expected_user_public_key {
        return Err("device certificate belongs to another workspace identity".into());
    }
    if revoked {
        return Err("certified collaboration device has been revoked".into());
    }
    if !enrolled {
        return Err("certified collaboration device is not enrolled".into());
    }
    let proof = <[u8; 64]>::try_from(presentation.proof.as_slice())
        .map_err(|_| "collaboration join proof must be 64 bytes".to_string())?;
    let room = presentation.workspace_id.as_str();
    admission_memo::join_proof_verified(&cert.device_public_key, room, &proof, || {
        VerifyingKey::from_bytes(&cert.device_public_key)
            .map_err(|_| "device certificate contains an invalid device key".to_string())?
            .verify(
                &runtime_identity::collaboration_join_signing_bytes(room),
                &Signature::from_bytes(&proof),
            )
            .map_err(|_| "collaboration join proof was signed by an unknown device".to_string())
    })?;
    // Without an authority to ask who governs the room, every foreign device
    // is held to the cap; the caller refines this where it has one.
    Ok(VerifiedJoinIdentity::of(&cert, cert.capabilities.foreign))
}

fn verify_join_identity(
    route: &CrdtRoute,
    identity_bytes: &[u8],
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn super::peer_actions::CertificateAuthority>,
) -> Result<AdmittedJoinIdentity, String> {
    verify_join_identity_with_host_scope(
        route,
        identity_bytes,
        membership,
        trusted_devices,
        authority,
        None,
    )
}

fn verify_join_identity_with_host_scope(
    route: &CrdtRoute,
    identity_bytes: &[u8],
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn super::peer_actions::CertificateAuthority>,
    host_scope: Option<&LocalHostWorkerScope>,
) -> Result<AdmittedJoinIdentity, String> {
    let Some(presentation) = runtime_collab::CertifiedJoinIdentity::decode(identity_bytes)
        .map_err(|error| format!("certified join presentation is malformed: {error}"))?
    else {
        return Err("collaboration join requires a certified device presentation".into());
    };
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    let (expected_root, revoked, enrolled) = if let Some(scope) = host_scope {
        if !cert.capabilities.can_act_as_host || route != &scope.route || cert != scope.certificate
        {
            return Err("host worker descriptor route or certificate mismatch".into());
        }
        authority
            .ok_or("current host worker authority unavailable")?
            .authorize_host_worker(route, &cert)?;
        (cert.user_public_key, false, true)
    } else if cert.capabilities.can_act_as_host {
        return Err("host certificate requires a private host worker descriptor".into());
    } else if let Some(authority) = authority {
        let room = uuid::Uuid::parse_str(&route.cluster_id).map_err(|_| "invalid exact room")?;
        authority.authorize_join(room, &cert)?;
        (cert.user_public_key, false, true)
    } else if let Some(devices) = trusted_devices {
        (
            cert.user_public_key,
            false,
            devices.contains(&cert.device_public_key),
        )
    } else {
        return Err("no current certificate authority is attached".into());
    };
    let registered = authority.map_or(
        Ok(None),
        super::peer_actions::CertificateAuthority::policy_time,
    )?;
    let now_ns = policy_context::sample(registered.clone(), registered.as_ref())?.1;
    let verified = VerifiedJoinIdentity {
        capped: cert_capped_in(authority, &route.cluster_id, &cert),
        ..verify_join_presentation(
            route,
            &presentation,
            &expected_root,
            now_ns,
            revoked,
            enrolled,
        )?
    };
    let membership = membership
        .ok_or_else(|| "certified join has no workspace membership authority".to_string())?;
    let mut admitted = membership.decide_in_room(&route.cluster_id, |workspace| {
        admit_verified_join_at(workspace, route, verified, now_ns / 1_000_000_000)
    })??;
    admitted.certificate = Some(cert);
    admitted.host_scope = host_scope.cloned();
    Ok(admitted)
}

/// Envelope pushed onto an [`OpStreamServer`]'s queue. `kind` is what
/// makes the queue readable: signed, delta and whole-document share
/// this type and are indistinguishable from the bytes alone.
#[derive(Clone, Debug)]
struct OpEnvelope {
    op_bytes: Vec<u8>,
    from_user_id: String,
    kind: OpKind,
    admission: OpAdmission,
}

/// Envelope pushed onto a [`PresenceStreamServer`]'s queue. Carries
/// every presence variant; the cursor stream filters to pointers.
#[derive(Clone, Debug)]
struct PresenceEnvelope {
    user_id: String,
    key_bytes: Vec<u8>,
    payload: PresencePayload,
}

/// Envelope pushed onto a [`ContextStreamServer`]'s queue.
#[derive(Clone, Debug)]
enum ContextEnvelope {
    Snapshot {
        collaborators: Vec<ui::collaboration::Collaborator>,
        permissions: Vec<String>,
    },
    FollowChanged {
        peer_id: Option<String>,
    },
    SnapshotRequested,
    /// An encoded `collab_types::handoff::HandoffRequest`.
    Handoff(Vec<u8>),
    /// The room's delegated agents the session verified, for history.
    HistoryAgents(Vec<collab_types::history::RosterAgent>),
    /// Records of the application's trees a peer wrote and no window holds.
    RecordsArrived(Vec<String>),
}

/// One inert operation published by a live application connection: the one
/// exported-action shape `runtime-collab` decodes off the wire, typed.
pub use runtime_collab::ExportedAction as PublishedAction;
/// Input contract published for one live application action.
pub use runtime_collab::ExportedInput as PublishedInput;

fn validate_view_action_binding(action: &PublishedAction, role: &str) -> Result<(), capnp::Error> {
    if action.operation_kind != tilo_actions::OperationKind::View {
        return Err(capnp::Error::failed(format!(
            "resource view {role} action must be a view operation"
        )));
    }
    if action.contract.confirmation != tilo_actions::ConfirmationPolicy::None {
        return Err(capnp::Error::failed(format!(
            "resource view {role} action must not require confirmation"
        )));
    }
    if action.params.iter().any(|parameter| parameter.required) {
        return Err(capnp::Error::failed(format!(
            "resource view {role} action must not require caller parameters"
        )));
    }
    Ok(())
}

fn validate_resource_range_binding(
    actions: &[PublishedAction],
    action: &PublishedAction,
    range: &tilo_actions::ResourceViewRangeSpec,
) -> Result<(), capnp::Error> {
    let names = [&range.start_param, &range.end_param, &range.unit_param];
    let target = actions
        .iter()
        .find(|target| target.resource == action.resource && target.action == range.action)
        .ok_or_else(|| capnp::Error::failed("resource range action missing".into()))?;
    if names.iter().any(|name| name.is_empty())
        || names[0] == names[1]
        || names[0] == names[2]
        || names[1] == names[2]
        || target.operation_kind != tilo_actions::OperationKind::View
        || target.contract.confirmation != tilo_actions::ConfirmationPolicy::None
    {
        return Err(capnp::Error::failed(
            "invalid resource range binding".into(),
        ));
    }
    for (name, ty) in [
        (&range.start_param, tilo_actions::ParamType::Integer),
        (&range.end_param, tilo_actions::ParamType::Integer),
        (&range.unit_param, tilo_actions::ParamType::Text),
    ] {
        if !target
            .params
            .iter()
            .any(|p| p.name.as_ref() == name && p.ty == ty)
        {
            return Err(capnp::Error::failed(
                "invalid resource range parameter".into(),
            ));
        }
    }
    if target
        .params
        .iter()
        .any(|p| p.required && !names.iter().any(|n| p.name.as_ref() == *n))
    {
        return Err(capnp::Error::failed(
            "resource range requires unrelated input".into(),
        ));
    }
    Ok(())
}

/// The checks every published surface passes, whichever request carried it:
/// addresses non-empty, unique and separator-free, kinds and labels present,
/// contracts and parameters valid, resource views consistent.
fn validate_published_surface(
    rows: Vec<PublishedAction>,
) -> Result<Vec<PublishedAction>, capnp::Error> {
    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    let mut actions = Vec::with_capacity(rows.len());
    for published in rows {
        if published.resource.is_empty()
            || published.action.is_empty()
            || published.resource.contains(tilo_actions::KEY_SEP)
            || published.action.contains(tilo_actions::KEY_SEP)
            || !seen.insert((published.resource.clone(), published.action.clone()))
        {
            return Err(capnp::Error::failed(
                "exported action addresses must be non-empty, unique, and contain no separator"
                    .into(),
            ));
        }
        if published.resource_kind.trim().is_empty()
            || published.resource_label.trim().is_empty()
            || published.action_label.trim().is_empty()
        {
            return Err(capnp::Error::failed(
                "exported actions require non-empty kind and labels".into(),
            ));
        }
        if let Some(reason) = published.contract.validate_declaration().into_iter().next() {
            return Err(capnp::Error::failed(format!(
                "invalid exported action contract: {reason}"
            )));
        }
        let mut parameter_names = std::collections::HashSet::new();
        for parameter in &published.params {
            if !parameter_names.insert(parameter.name.as_ref()) {
                return Err(capnp::Error::failed(
                    "exported action parameter names must be unique".into(),
                ));
            }
            if let Some(reason) = parameter.validate_declaration().into_iter().next() {
                return Err(capnp::Error::failed(format!(
                    "invalid exported action parameter `{}`: {reason}",
                    parameter.name
                )));
            }
        }
        actions.push(published);
    }
    validate_resource_view_descriptors(&actions)?;
    Ok(actions)
}

/// Recheck the resource-view invariants after decoding an untrusted action
/// surface. A view with a mount must carry that action in the same atomic
/// publication/page so the broker can prove the binding before exposing it.
fn validate_resource_view_descriptors(actions: &[PublishedAction]) -> Result<(), capnp::Error> {
    #[derive(Default)]
    struct ResourceViews<'a> {
        ids: std::collections::HashSet<&'a str>,
        open_actions: std::collections::HashSet<&'a str>,
        primary_count: usize,
    }

    let mut resources = std::collections::HashMap::<&str, ResourceViews<'_>>::new();
    for action in actions {
        let Some(view) = &action.view else {
            continue;
        };
        if view.id.is_empty() || view.id.contains(tilo_actions::KEY_SEP) {
            return Err(capnp::Error::failed(
                "resource view id must be non-empty and contain no separator".into(),
            ));
        }
        if view.label.trim().is_empty() {
            return Err(capnp::Error::failed(
                "resource view label must be non-empty".into(),
            ));
        }
        if view.open_action != action.action {
            return Err(capnp::Error::failed(
                "resource view must be attached to its open action".into(),
            ));
        }
        validate_view_action_binding(action, "open")?;

        let seen = resources.entry(&action.resource).or_default();
        if !seen.ids.insert(&view.id) {
            return Err(capnp::Error::failed(
                "resource view ids must be unique per resource".into(),
            ));
        }
        if !seen.open_actions.insert(&view.open_action) {
            return Err(capnp::Error::failed(
                "at most one resource view may reference an open action".into(),
            ));
        }
        if view.primary {
            seen.primary_count = seen.primary_count.saturating_add(1);
            if seen.primary_count > 1 {
                return Err(capnp::Error::failed(
                    "at most one resource view may be primary".into(),
                ));
            }
        }

        if let Some(range) = &view.range {
            validate_resource_range_binding(actions, action, range)?;
        }

        let Some(mount) = &view.mount else {
            continue;
        };
        if mount.action.trim().is_empty()
            || mount.capability_param.trim().is_empty()
            || mount.view_param.trim().is_empty()
            || mount.capability_param == mount.view_param
        {
            return Err(capnp::Error::failed(
                "resource view mount requires an action and distinct parameter names".into(),
            ));
        }
        let mount_action = actions.iter().find(|candidate| {
            candidate.resource == action.resource && candidate.action == mount.action
        });
        let Some(mount_action) = mount_action else {
            return Err(capnp::Error::failed(
                "resource view mount action is not exported by its resource".into(),
            ));
        };
        validate_view_action_binding(mount_action, "mount")?;
        for (role, name) in [
            ("capability", mount.capability_param.as_str()),
            ("view", mount.view_param.as_str()),
        ] {
            if !mount_action.params.iter().any(|parameter| {
                parameter.name.as_ref() == name
                    && parameter.ty == tilo_actions::ParamType::Text
                    && !parameter.required
            }) {
                return Err(capnp::Error::failed(format!(
                    "resource view mount {role} parameter `{name}` must name an optional text parameter"
                )));
            }
        }
    }
    Ok(())
}

/// A live application's complete, level-triggered action surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSurface {
    pub peer_id: u64,
    pub app_id: String,
    pub route: CrdtRoute,
    pub actions: Vec<PublishedAction>,
    /// False for a permission-filterable catalogue retained after the app
    /// closes: what it last published, offered so invoking it launches the
    /// app.
    pub live: bool,
}

impl PublishedSurface {
    /// Copy only the selected descriptor and its routing metadata.
    fn action_projection(&self, resource: &str, action: &str) -> Option<(Self, PublishedAction)> {
        let descriptor = self
            .actions
            .iter()
            .find(|descriptor| descriptor.resource == resource && descriptor.action == action)?;
        Some((
            Self {
                peer_id: self.peer_id,
                app_id: self.app_id.clone(),
                route: self.route.clone(),
                actions: Vec::new(),
                live: self.live,
            },
            descriptor.clone(),
        ))
    }
}

#[derive(Clone, Debug)]
pub struct InvocationEnvelope {
    id: u64,
    invocation: tilo_actions::InvocationId,
    pub resource: String,
    pub action: String,
    argument: Option<String>,
    pub actor: String,
    context: Option<tilo_actions::ActionContext>,
}

impl InvocationEnvelope {
    /// The context the host set for the invocation: who invoked it, of what
    /// kind, for whom, in which run.
    #[must_use]
    pub fn context(&self) -> Option<&tilo_actions::ActionContext> {
        self.context.as_ref()
    }
}

#[derive(Clone, Debug)]
struct TaskRunEnvelope {
    paused: Option<runtime_collab::TaskRunPausedStep>,
    task_id: String,
    title: String,
    instruction: Option<String>,
    subject: Option<tilo_actions::SelectionSubject>,
    run_id: u64,
    lease: String,
    planning: runtime_collab::TaskRunPlanning,
    execution_budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    action_context: tilo_actions::ActionContext,
    model: Option<runtime_collab::TaskRunModel>,
}

#[derive(Clone, Debug)]
struct TaskSubscription {
    agent_id: String,
    cluster_id: String,
    tx: mpsc::UnboundedSender<TaskRunEnvelope>,
}

#[derive(Clone, Debug)]
struct PendingTaskRun {
    peer_id: u64,
    agent_id: String,
    cluster_id: String,
    task_id: String,
    lease: String,
    action_context: tilo_actions::ActionContext,
    terminal: bool,
    native: crate::services::NativeTaskBinding,
}

/// The application's terminal answer to a directed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationCompletion {
    pub code: String,
    pub message: String,
}

pub struct InvocationJob {
    pub id: u64,
    pub completion: std::sync::mpsc::Receiver<InvocationCompletion>,
}

/// `answer`, waited for here: a test's view of a host's answer.
#[cfg(test)]
#[track_caller]
pub(crate) fn answered<T>(answer: tilo_actions::remote::Pending<T>) -> T {
    answer
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the host answers")
}

#[derive(Clone)]
struct PendingInvocation {
    /// Who it was admitted for, kept so an edit its effect submits is
    /// admitted for them too.
    caller: Option<ContinuingCaller>,
    activity: Option<ActivityTarget>,
    owner: InvocationOwner,
    network_evidence: Option<tilo_relay_session::client::CertifiedAction>,
    original: Option<Arc<OriginalInvocationData>>,
    policy_time: Option<crate::peer_actions::PolicyTime>,
    journal_key: Option<InvocationKey>,
    peer_id: u64,
    tx: std::sync::mpsc::Sender<InvocationCompletion>,
    update: tilo_actions::InvocationUpdate,
    prepared_effect: Arc<Mutex<Option<PreparedEffectCommit>>>,
    last_sequence: u64,
    actor: String,
    cancellable: bool,
    retryable_on_disconnect: bool,
    execution: tilo_actions::ExecutionMode,
    result: tilo_actions::ResultSpec,
    operation_kind: tilo_actions::OperationKind,
    cluster_id: String,
    context: tilo_actions::ActionContext,
    /// Admission/progress heartbeat. A silent application must not leave a
    /// broker-owned job in `Accepted` forever.
    last_activity: std::time::Instant,
    /// Starts the shorter acknowledgement deadline for cooperative cancel.
    cancellation_requested_at: Option<std::time::Instant>,
    /// A retained continuation must observe cancellation after registry removal.
    cancellation_fence: Arc<AtomicBool>,
}

struct PreparedEffectCommit {
    update: tilo_actions::InvocationUpdate,
    request: tilo_owner_commit::OwnerCommitRequest,
}

/// An opaque certified person admission. Its root/device are never accepted
/// from action payload strings; the existing certificate/proof verifier owns
/// construction and every operation rechecks current certificate/membership.
#[derive(Clone)]
pub(crate) struct VerifiedActionCaller {
    room: String,
    presentation: Vec<u8>,
    network: Option<tilo_relay_session::client::CertifiedAction>,
    policy_time: Option<crate::peer_actions::PolicyTime>,
    admitted: AdmittedJoinIdentity,
    /// The agent this person's own device handed the session to, kept
    /// across every re-verification of the person's admission.
    delegated: Option<String>,
    /// A keyless native projection verified against protected seat custody.
    /// Only `certified_native_action_host` sets this; signed/network evidence
    /// alone never establishes physical intent.
    native_projection_verified: bool,
}

impl VerifiedActionCaller {
    /// The same admission, handed by the person's own device to agent
    /// `agent`: it acts for the person with the delegation a device gives
    /// its agents.
    fn delegated_to(mut self, agent: &str) -> Self {
        self.delegated = Some(agent.to_owned());
        self.native_projection_verified = false;
        self.admitted = self.admitted.delegated_to(self.delegated.as_deref());
        self
    }

    fn owner(&self) -> InvocationOwner {
        InvocationOwner::Certified {
            root: self.admitted.verified.user_public_key,
            device: self.admitted.verified.device_public_key,
        }
    }
}

/// Inert transport-neutral request. The production peer entrypoint validates
/// its wire host against the durable broker authority before constructing this
/// request. Tests retain that host to exercise the equivalent direct-admission
/// boundary. Task4c must also bind this to the certified recipient device
/// before entering this API.
#[derive(Clone, Debug)]
pub(crate) struct CallerInvocation {
    pub invocation: tilo_actions::InvocationId,
    #[cfg(test)]
    pub host: uuid::Uuid,
    pub peer_id: u64,
    pub app_id: String,
    pub record_id: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    pub revision: Option<String>,
    pub recipient: Option<[u8; 32]>,
}

impl CallerInvocation {
    fn intent(&self) -> serde_json::Value {
        let mut intent = serde_json::json!({ "peer_id": self.peer_id, "app_id": self.app_id,
            "record_id": self.record_id, "resource": self.resource,
            "action": self.action, "argument": self.argument });
        if let Some(fields) = intent.as_object_mut() {
            if let Some(revision) = &self.revision {
                fields.insert("revision".into(), serde_json::json!(revision));
            }
            if let Some(recipient) = &self.recipient {
                fields.insert("recipient".into(), serde_json::json!(recipient));
            }
        }
        intent
    }
}

impl CapnpBrokerState {
    /// The caller of the invocation `running` that this broker delivered to
    /// the connection `peer_id` and that still runs: who an edit its effect
    /// submits is admitted for. `None` when the connection runs no such
    /// invocation, or its caller was known only by certificate.
    /// Who an invoke continuing a running invocation is admitted for: that
    /// invocation's own caller, with their grants. `None` for any other
    /// request; the refusal to answer with when the connection runs no such
    /// invocation.
    fn continued_caller(
        &self,
        peer_id: u64,
        message: &tilo_actions::remote::RemoteActionMessage,
    ) -> Option<Result<tilo_actions::remote::Caller, Box<tilo_actions::remote::RemoteActionMessage>>>
    {
        let tilo_actions::remote::RemoteActionMessage::Invoke(invoke) = message else {
            return None;
        };
        let running = invoke.on_behalf_of?;
        Some(self.continuing_caller(peer_id, running).ok_or_else(|| {
            Box::new(tilo_actions::remote::RemoteActionMessage::Receipt(
                tilo_actions::remote::InvocationReceipt {
                    invocation: invoke.invocation,
                    status: tilo_actions::remote::ReceiptStatus::Rejected {
                        error: tilo_actions::remote::ActionHostError::NotPermitted,
                    },
                },
            ))
        }))
    }

    pub(crate) fn continuing_caller(
        &self,
        peer_id: u64,
        running: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::Caller> {
        // Doors that take the caller's own invocation id can admit two
        // callers' invocations under one id; such an id names nobody.
        let mut matching = self
            .pending_invocations
            .lock()
            .ok()?
            .values()
            .filter(|pending| pending.peer_id == peer_id && pending.update.invocation() == running)
            .map(|pending| pending.caller.clone())
            .take(2)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return None;
        }
        matching.pop()??.current(self)
    }
}

/// Who a running invocation was admitted for, however the door knew them:
/// the caller an edit its effect submits is admitted as.
#[derive(Clone)]
enum ContinuingCaller {
    /// Host-admitted native person, revalidated for every continuation.
    NativePerson {
        room: String,
        certificate: Box<tilo_user_identity::DeviceCert>,
        time: crate::peer_actions::PolicyTime,
        direct: tilo_actions::remote::Caller,
    },
    /// A caller the door knew directly.
    Caller(tilo_actions::remote::Caller),
    /// A certified caller, its certificate checked again each time.
    Certified {
        caller: Box<VerifiedActionCaller>,
        /// A stricter foreign-device restriction the host kept for them.
        foreign_restriction: bool,
        /// The exact caller the door admitted, with its execution, when it
        /// knew one; it continues only while that execution is live.
        direct: Option<tilo_actions::remote::Caller>,
    },
    /// A person or agent on a connection this host admitted, in a room.
    Admitted {
        room: String,
        admitted: Box<AdmittedJoinIdentity>,
    },
}

impl ContinuingCaller {
    fn from_durable(durable: DurableContext<'_>) -> Self {
        match durable {
            DurableContext::Peer(caller, _) => Self::Certified {
                caller: Box::new(caller.clone()),
                foreign_restriction: false,
                direct: None,
            },
            DurableContext::Local { caller, .. } | DurableContext::Task { caller, .. } => {
                let (room, admitted) = caller.admitted_in();
                Self::Admitted {
                    room,
                    admitted: Box::new(admitted),
                }
            }
        }
    }

    fn from_origin(origin: &InvocationCaller) -> Self {
        match origin {
            InvocationCaller::NativePerson {
                room,
                certificate,
                time,
                ..
            } => Self::NativePerson {
                room: room.clone(),
                certificate: Box::new(certificate.clone()),
                time: time.clone(),
                direct: tilo_actions::remote::Caller::local_person(principal_id_for_root(
                    certificate.user_public_key,
                ))
                .in_room(room.clone()),
            },
            InvocationCaller::Certified {
                caller,
                foreign_restriction,
            } => Self::Certified {
                caller: caller.clone(),
                foreign_restriction: *foreign_restriction,
                direct: None,
            },
            InvocationCaller::Local {
                route, admitted, ..
            } => Self::Admitted {
                room: route.cluster_id.clone(),
                admitted: admitted.clone(),
            },
        }
    }

    /// Who `caller` was admitted as: certified, with the custody its door
    /// holds, when it holds one.
    fn admitted(
        caller: Option<&tilo_actions::remote::Caller>,
        durable: Option<DurableContext<'_>>,
        origin: Option<&InvocationCaller>,
    ) -> Option<Self> {
        match (caller, origin) {
            (
                Some(direct),
                Some(InvocationCaller::NativePerson {
                    room,
                    certificate,
                    time,
                    ..
                }),
            ) => Some(Self::NativePerson {
                room: room.clone(),
                certificate: Box::new(certificate.clone()),
                time: time.clone(),
                direct: direct.clone(),
            }),
            (
                Some(direct),
                Some(InvocationCaller::Certified {
                    caller,
                    foreign_restriction,
                }),
            ) => Some(Self::Certified {
                caller: caller.clone(),
                foreign_restriction: *foreign_restriction,
                direct: Some(direct.clone()),
            }),
            (Some(caller), _) => Some(Self::Caller(caller.clone())),
            (None, _) => durable
                .map(Self::from_durable)
                .or_else(|| origin.map(Self::from_origin)),
        }
    }

    /// The caller as they stand now: a certified caller whose certificate
    /// no longer verifies, who changed, or whose execution ended, is nobody.
    /// A certified caller keeps their own device, so they stay held to the
    /// foreign-device cap wherever that device does not govern the room.
    fn current(&self, state: &CapnpBrokerState) -> Option<tilo_actions::remote::Caller> {
        match self {
            Self::NativePerson {
                room,
                certificate,
                time,
                direct,
            } => {
                state.checked_policy_time(Some(time)).ok()?;
                state.current_native_person(room, certificate).ok()?;
                (direct.execution_id().is_none() || direct.execution_is_live())
                    .then(|| direct.clone())
            }
            Self::Caller(caller) => Some(caller.clone()),
            Self::Certified {
                caller,
                foreign_restriction,
                direct,
            } => {
                let admitted = state.current_action_caller(caller).ok()?;
                match direct {
                    Some(direct) => (direct.execution_id().is_none() || direct.execution_is_live())
                        .then(|| direct.clone()),
                    None => Some(admitted.certified_caller(&caller.room, *foreign_restriction)),
                }
            }
            Self::Admitted { room, admitted } => Some(admitted.caller(room)),
        }
    }
}

impl PendingInvocation {
    fn completion_owner(&self) -> InvocationOwner {
        self.owner.clone()
    }
}

// Remote-action host: the broker as a `tilo_actions::remote::ActionHost`.
mod action_host;
mod admission_memo;
mod app_consent;
mod app_open_door;
mod host_action_admission;
mod host_steps;
mod launchable;
mod live_views;
mod local_action_host;
mod remaining_workspace_actions;
mod run_question;
mod witness;
use witness::requires_store_witness;
mod workspace_open;
pub use local_action_host::{CertifiedActionLifecycle, CertifiedPersonContext};

pub use launchable::launchable_provider;
#[path = "peer_action_broker.rs"]
mod peer_action_broker;
#[path = "policy_context.rs"]
mod policy_context;
#[path = "room_service.rs"]
mod room_service;

impl CapnpBrokerState {
    fn registered_policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
        self.certificate_authority
            .get()
            .map_or(Ok(None), |authority| authority.policy_time())
    }
    fn checked_policy_time(
        &self,
        submitted: Option<&crate::peer_actions::PolicyTime>,
    ) -> Result<(Option<crate::peer_actions::PolicyTime>, u64), String> {
        policy_context::sample(self.registered_policy_time()?, submitted)
    }
    fn policy_now_ns(&self) -> Result<u64, String> {
        let registered = self.registered_policy_time()?;
        Ok(policy_context::sample(registered.clone(), registered.as_ref())?.1)
    }
    pub(crate) fn invocation_host(&self) -> Result<uuid::Uuid, String> {
        if self.durable_owner_required {
            self.owner_health()?;
        }
        self.journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or_else(|| "durable admission unavailable".to_string())
            .and_then(|journal| {
                journal.check()?;
                Ok(journal.host())
            })
    }

    pub(crate) fn verify_action_caller(
        &self,
        room: &str,
        presentation: &[u8],
    ) -> Result<VerifiedActionCaller, String> {
        // The local certified presentation format also binds the exact room.
        // No caller-controlled `is_agent` or principal string enters this door.
        if uuid::Uuid::parse_str(room).is_err() || presentation.len() > 16 * 1024 {
            return Err("invalid certified invocation caller".into());
        }
        #[cfg(test)]
        admission_memo::count_verification();
        let admitted = verify_join_identity(
            &CrdtRoute::cluster(room),
            presentation,
            self.membership.as_ref(),
            self.trusted_devices.as_deref().map(Vec::as_slice),
            self.certificate_authority.get().map(Arc::as_ref),
        )?;
        Ok(VerifiedActionCaller {
            room: room.into(),
            presentation: presentation.into(),
            native_projection_verified: false,
            network: None,
            policy_time: self.registered_policy_time()?,
            admitted,
            delegated: None,
        })
    }

    /// Whether `cert`'s device is held to the foreign-device cap in `room`.
    fn capped_in(&self, room: &str, cert: &tilo_user_identity::DeviceCert) -> bool {
        cert_capped_in(
            self.certificate_authority.get().map(AsRef::as_ref),
            room,
            cert,
        )
    }

    /// The broker actor `caller` is in `room`.
    pub(super) fn caller_actor(
        &self,
        caller: &tilo_actions::remote::Caller,
        room: &str,
    ) -> BrokerActor {
        BrokerActor::for_caller(caller).capped(cap_applies_in(
            self.certificate_authority.get().map(AsRef::as_ref),
            room,
            caller.is_foreign_device(),
            caller.device_public_key(),
        ))
    }

    /// The person `cert` certifies, acting in `room` from that certificate's device.
    fn certified_person(&self, room: &str, cert: &tilo_user_identity::DeviceCert) -> BrokerActor {
        BrokerActor {
            capped: self.capped_in(room, cert),
            ..BrokerActor::person(principal_id_for_root(cert.user_public_key))
        }
    }

    fn current_action_caller(
        &self,
        caller: &VerifiedActionCaller,
    ) -> Result<AdmittedJoinIdentity, String> {
        self.checked_policy_time(caller.policy_time.as_ref())?;
        if let Some(admitted) = admission_memo::admitted_now(caller) {
            return Ok(admitted);
        }
        self.live_action_caller(caller)
    }

    /// Effect barriers must not reuse the synchronous dispatch admission memo.
    fn live_action_caller(
        &self,
        caller: &VerifiedActionCaller,
    ) -> Result<AdmittedJoinIdentity, String> {
        self.checked_policy_time(caller.policy_time.as_ref())?;
        let current = if let Some(evidence) = &caller.network {
            self.verify_network_action_caller_with_policy_time(
                evidence,
                caller.policy_time.as_ref(),
            )?
        } else {
            self.verify_action_caller(&caller.room, &caller.presentation)?
        };
        if current.owner() != caller.owner() {
            return Err("certified caller changed".into());
        }
        Ok(current.admitted.delegated_to(caller.delegated.as_deref()))
    }

    fn caller_key(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<InvocationKey, String> {
        Ok(InvocationKey {
            room: caller.room.clone(),
            host: self.invocation_host()?,
            owner: caller.owner(),
            invocation,
        })
    }

    fn authorize_durable_result(
        &self,
        caller: &VerifiedActionCaller,
        record: &DurableInvocation,
    ) -> Result<(), String> {
        let current = self.current_action_caller(caller)?;
        if record.prepared_effect.is_some()
            && record
                .intent
                .get("actor")
                .and_then(serde_json::Value::as_str)
                != Some(current.broker_actor().id.as_str())
        {
            return Err("prepared invocation actor differs from current caller".into());
        }
        let now = self.checked_policy_time(caller.policy_time.as_ref())?.1 / 1_000_000_000;
        let contract: tilo_actions::ActionContract =
            serde_json::from_value(record.intent.get("contract").cloned().unwrap_or_default())
                .map_err(|_| "journal action contract invalid")?;
        let app = record
            .intent
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal app invalid")?;
        let resource = record
            .intent
            .get("resource")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal resource invalid")?;
        let resource_kind = record
            .intent
            .get("resource_kind")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal resource kind invalid")?;
        let action = record
            .intent
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or("journal action invalid")?;
        self.authorize_invocation_permission(
            &caller.room,
            (app, resource_kind, resource, action),
            &contract.need(),
            &current.broker_actor(),
            now,
        )?;
        self.authorize_checkpoint_result(&caller.room, current.certificate.as_ref(), record)?;
        Ok(())
    }

    pub(crate) fn verified_invocation_status(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<DurableInvocation>, String> {
        self.current_action_caller(caller)?;
        self.expire_stale_invocations();
        let key = self.caller_key(caller, invocation)?;
        let record = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("durable admission unavailable")?
            .lookup(&key)?;
        if let Some(record) = &record {
            self.authorize_durable_result(caller, record)?;
        }
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn start_verified_invocation(
        &self,
        caller: &VerifiedActionCaller,
        request: &CallerInvocation,
        confirmation: Option<&str>,
    ) -> Result<DurableInvocation, String> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        let admitted = self.current_action_caller(caller)?;
        if request.host != self.invocation_host()?
            || request.invocation.0.is_nil()
            || request.peer_id == ui::collaboration::CURRENT_CONNECTION_PEER_ID
            || [
                &request.app_id,
                &request.record_id,
                &request.resource,
                &request.action,
            ]
            .iter()
            .any(|s| s.len() > 512)
            || request
                .argument
                .as_ref()
                .is_some_and(|s| s.len() > 256 * 1024)
        {
            return Err("invalid invocation target or bounds".into());
        }
        if let Some(record) = self.verified_invocation_status(caller, request.invocation)? {
            if !record.admitted_request_is(&request.intent()) {
                return Err("invocation identity reused with different intent".into());
            }
            return Ok(record);
        }
        let offered = self
            .surfaces
            .lock()
            .map_err(|_| "action catalogue unavailable")?
            .get(&request.peer_id)
            .cloned()
            .filter(|surface| {
                surface.live
                    && surface.route.cluster_id == caller.room
                    && surface.app_id == request.app_id
                    && surface.route.record_id == request.record_id
            })
            .and_then(|surface| {
                let action = surface
                    .actions
                    .iter()
                    .find(|a| a.resource == request.resource && a.action == request.action)?
                    .clone();
                Some((surface, action))
            })
            .ok_or("action no longer offered in this exact room/app/record")?;
        self.dispatch_invocation_from_offered(
            request.peer_id,
            offered,
            InvocationInputs {
                original_request: None,
                requested: None,
                caller: None,
                execution: None,
                argument: request.argument.as_deref(),
                actor: &admitted.broker_actor(),
                confirmation_token: confirmation,
                context: None,
            },
            Some((caller, request)),
            None,
        )?
        .start_blocking(self)?;
        self.verified_invocation_status(caller, request.invocation)?
            .ok_or_else(|| "admitted invocation missing".into())
    }

    pub(crate) async fn cancel_verified_invocation(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<DurableInvocation, String> {
        let record = self
            .verified_invocation_status(caller, invocation)?
            .ok_or("unknown invocation")?;
        self.cancel_durable_record(&record)?.wait().await?;
        self.verified_invocation_status(caller, invocation)?
            .ok_or_else(|| "invocation missing".into())
    }

    /// Ask `record`'s application to stop. Answer the asker only once the
    /// returned write resolves; the application hears of it no sooner
    /// either (see `invocation_cancellation`).
    fn cancel_durable_record(
        &self,
        record: &DurableInvocation,
    ) -> Result<invocation_journal::Durable, String> {
        let mut registry = self
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let pending = registry
            .get_mut(&record.wire_id)
            .filter(|p| p.journal_key.as_ref() == Some(&record.key))
            .ok_or("invocation is not running")?;
        if !pending.cancellable {
            return Err("action did not declare cancellation".into());
        }
        let durable = self.persist_cancellation(pending)?;
        pending
            .cancellation_requested_at
            .get_or_insert_with(std::time::Instant::now);
        self.cancellation_requests
            .lock()
            .map_err(|_| "cancellation registry unavailable")?
            .insert(record.wire_id);
        drop(registry);
        Ok(durable)
    }

    /// Stage `pending`'s cancellation request and fence its effects off at
    /// once. The request is durable once the returned write resolves.
    fn persist_cancellation(
        &self,
        pending: &PendingInvocation,
    ) -> Result<invocation_journal::Durable, String> {
        let durable = match &pending.journal_key {
            Some(key) => self
                .journal
                .lock()
                .map_err(|_| "invocation journal unavailable")?
                .as_mut()
                .map_err(|e| e.clone())?
                .as_mut()
                .ok_or("durable admission unavailable")?
                .record_cancellation(key)?,
            None => invocation_journal::Durable::settled(),
        };
        pending.cancellation_fence.store(true, Ordering::Release);
        Ok(durable)
    }

    /// The journal write invocation `id` must wait for before anyone hears
    /// of its last transition.
    fn invocation_in_flight(&self, id: u64) -> invocation_journal::Durable {
        let key = self
            .pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).and_then(|job| job.journal_key.clone()));
        key.map_or_else(invocation_journal::Durable::settled, |key| {
            self.journal_in_flight(&key.invocation)
        })
    }

    /// Stage `pending`'s definite outcome `update`, to be heard only once
    /// it is durable ([`DefiniteOutcome::settle`]). Until then expiry and a
    /// disconnect leave the invocation alone: its outcome is being written.
    fn stage_definite_outcome(
        &self,
        id: u64,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<DefiniteOutcome<'_>, String> {
        let Some(key) = &pending.journal_key else {
            return Ok(DefiniteOutcome {
                state: self,
                id: None,
                durable: invocation_journal::Durable::settled(),
                heard: None,
            });
        };
        let durable = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_mut()
            .map_err(|e| e.clone())?
            .as_mut()
            .ok_or("durable admission unavailable")?
            .record_update(key, update.clone(), false)?;
        // A progress update stages nothing of its own (it waits for what is
        // in flight), so only a terminal outcome holds expiry off.
        let id = update.is_terminal().then_some(id);
        if let Some(id) = id {
            self.committing_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?
                .insert(id);
        }
        Ok(DefiniteOutcome {
            state: self,
            id,
            durable,
            heard: pending
                .network_evidence
                .clone()
                .map(|evidence| (evidence, update.clone())),
        })
    }

    /// Record `pending`'s uncertain outcome `update`. It claims nothing a
    /// restart would not answer anyway (recovery turns every unfinished
    /// invocation uncertain), so it is heard at once, its write in flight.
    fn persist_uncertain_outcome(
        &self,
        pending: &PendingInvocation,
        update: tilo_actions::InvocationUpdate,
    ) -> Result<(), String> {
        if let Some(key) = &pending.journal_key {
            // Not waited for: see above. The write still finishes in order.
            drop(
                self.journal
                    .lock()
                    .map_err(|_| "invocation journal unavailable")?
                    .as_mut()
                    .map_err(|e| e.clone())?
                    .as_mut()
                    .ok_or("durable admission unavailable")?
                    .record_update(key, update.clone(), true)?,
            );
            if let Some(evidence) = &pending.network_evidence {
                self.peer_action_queue.publish(evidence.clone(), update);
            }
        }
        Ok(())
    }

    /// Whether invocation `id`'s definite outcome is being written.
    fn committing(&self, id: u64) -> bool {
        self.committing_invocations
            .lock()
            .is_ok_and(|committing| committing.contains(&id))
    }
}

/// A live application's pull-only directory.  Keeping the certified route
/// beside the capability makes every page/resolve check independent of a
/// stale eager `publishActions` snapshot.
#[derive(Clone)]
struct RegisteredActionDirectory {
    pagination: directory_pagination::DirectoryPagination,
    /// Certified publishing person, never a participant label or room role.
    publishing_person: Option<String>,
    app_id: String,
    route: CrdtRoute,
    directory: action_directory::Client,
    registration: Rc<uuid::Uuid>,
}

/// Inert, enumerable route metadata for one connected pull directory.  It
/// contains no capability and disappears on disconnect, so shell search can
/// select a source without reaching into the broker `LocalSet`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDirectoryRoot {
    pub peer_id: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
}

/// A deterministic, inert page over the broker-retained directory root
/// projection.  Rebuilding the projection is a connect/disconnect operation;
/// palette keystrokes only clone the requested small page.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ActionDirectoryRootPage {
    pub roots: Vec<ActionDirectoryRoot>,
    pub next_after_peer: Option<u64>,
    pub generation: u64,
}

#[derive(Default)]
struct ActionDirectoryRootSnapshot {
    roots: Vec<ActionDirectoryRoot>,
    generation: u64,
}

/// `LocalSet`-only ownership for Rc-backed application capabilities.  The
/// shared broker state deliberately stores neither this map nor its receiver:
/// shell/chrome worker threads must remain Send.
struct LocalActionDirectoryRegistry {
    directories: RefCell<HashMap<u64, RegisteredActionDirectory>>,
    bridge_rx: RefCell<std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>>,
    resolved_permits: Arc<tokio::sync::Semaphore>,
}

impl LocalActionDirectoryRegistry {
    fn new(bridge_rx: std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>) -> Self {
        Self {
            directories: RefCell::new(HashMap::new()),
            bridge_rx: RefCell::new(bridge_rx),
            resolved_permits: Arc::new(tokio::sync::Semaphore::new(64)),
        }
    }

    fn registered(
        &self,
        peer_id: u64,
        cluster_id: &str,
    ) -> Result<RegisteredActionDirectory, capnp::Error> {
        self.directories
            .borrow()
            .get(&peer_id)
            .filter(|entry| entry.route.cluster_id == cluster_id)
            .cloned()
            .ok_or_else(|| capnp::Error::failed("unknown live application directory".into()))
    }

    fn remove(&self, peer_id: u64) {
        // Dropping the removed Cap'n Proto client can synchronously release a
        // connection lease and re-enter this idempotent cleanup. Release the
        // registry borrow before capability drop so the nested removal sees
        // an ordinary absent entry instead of panicking on RefCell reborrow.
        let removed = self.directories.borrow_mut().remove(&peer_id);
        drop(removed);
    }
}

/// Inert result returned from the broker `LocalSet` to shell chrome.  It never
/// carries a Cap'n Proto capability or widget reference across threads.
#[derive(Debug)]
pub enum ActionDirectoryBridgeResponse {
    Page(runtime_collab::ActionDirectoryPage),
    Resolution(runtime_collab::ActionDirectoryResolution),
    StartedDirectoryAction(StartedDirectoryAction),
}

/// Explicit consent supplied with an atomic directory invocation.  It is
/// carried to the broker `LocalSet`, checked against the freshly resolved
/// contract, and never retained in a shell search-result cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryActionConfirmation {
    Confirmed,
    Phrase(String),
}

/// Inert admission receipt for a directory action.  Progress remains
/// available through the existing invocation-status subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct StartedDirectoryAction {
    pub invocation_id: u64,
    pub initial_update: tilo_actions::InvocationUpdate,
}

/// A bounded bridge failure.  `Busy` is deliberate backpressure: palette
/// code must retain its last cached results rather than blocking a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionDirectoryBridgeError {
    Busy,
    InvalidRequest,
    Unavailable,
    TimedOut,
    StaleRevision,
    NotFound,
    ConfirmationRequired,
}

pub type ActionDirectoryBridgeReply =
    Result<ActionDirectoryBridgeResponse, ActionDirectoryBridgeError>;

/// Where a directory bridge answer goes: a oneshot any executor may await,
/// or a remote-action [`tilo_actions::remote::Pending`] completed on the
/// broker `LocalSet` in the same turn the owner answered.
enum DirectoryReply {
    Oneshot(tokio::sync::oneshot::Sender<ActionDirectoryBridgeReply>),
    Pending(tilo_actions::remote::Completer<ActionDirectoryBridgeReply>),
}

impl DirectoryReply {
    /// The longest this asker is waited for. The shell's own surfaces ask
    /// for a person mid-frame and keep their last results past a short
    /// deadline. A remote caller has a deadline of its own and gives up by
    /// dropping its answer, which ends the wait sooner; the bound here only
    /// stops an owner that never answers from pinning the request.
    fn patience(&self) -> Duration {
        match self {
            Self::Oneshot(_) => CapnpBrokerState::DIRECTORY_BRIDGE_TIMEOUT,
            Self::Pending(_) => CapnpBrokerState::DIRECTORY_OWNER_HUNG,
        }
    }

    /// `answer`, awaited for as long as this asker still wants it: the
    /// owning app answers on its next turn, and a turn running a heavy
    /// action or frame is busy, not gone. `Err(TimedOut)` once the asker's
    /// patience ran out, `Err(Unavailable)` once it stopped asking.
    async fn awaited<T>(
        &mut self,
        answer: impl std::future::Future<Output = T>,
    ) -> Result<T, ActionDirectoryBridgeError> {
        let patience = self.patience();
        match self {
            Self::Oneshot(sender) => await_while_asked(patience, sender.closed(), answer).await,
            Self::Pending(completer) => {
                await_while_asked(patience, completer.abandoned(), answer).await
            }
        }
    }

    fn is_closed(&self) -> bool {
        match self {
            Self::Oneshot(sender) => sender.is_closed(),
            Self::Pending(completer) => completer.is_abandoned(),
        }
    }

    fn send(self, reply: ActionDirectoryBridgeReply) {
        match self {
            Self::Oneshot(sender) => {
                let _ = sender.send(reply);
            }
            Self::Pending(completer) => completer.complete(reply),
        }
    }
}

/// `answer`, or why it stopped being awaited: `patience` ran out
/// (`TimedOut`) or the asker went away — `gone` resolved — (`Unavailable`).
/// The asker's departure is an event, so a cancelled request is released
/// at once rather than on the next check.
async fn await_while_asked<T>(
    patience: Duration,
    gone: impl std::future::Future<Output = ()>,
    answer: impl std::future::Future<Output = T>,
) -> Result<T, ActionDirectoryBridgeError> {
    tokio::select! {
        biased;
        value = answer => Ok(value),
        () = gone => Err(ActionDirectoryBridgeError::Unavailable),
        () = tokio::time::sleep(patience) => Err(ActionDirectoryBridgeError::TimedOut),
    }
}

#[cfg(test)]
mod directory_patience_tests {
    use super::{
        ActionDirectoryBridgeError, CapnpBrokerState, DirectoryReply, Duration, await_while_asked,
    };

    /// A remote caller (an agent, another device) waits for an app whose
    /// owner thread is busy with a heavy action or frame for longer than the
    /// shell's own surfaces do; how long is the caller's to say.
    #[test]
    fn a_remote_asker_outwaits_a_busy_owner_and_stops_when_it_gives_up() {
        let (completer, answer) =
            tilo_actions::remote::pending::<super::ActionDirectoryBridgeReply>();
        let remote = DirectoryReply::Pending(tilo_actions::remote::pending().0);
        let (shell, _receiver) = tokio::sync::oneshot::channel();
        let shell = DirectoryReply::Oneshot(shell);
        assert!(remote.patience() > shell.patience());
        assert!(
            CapnpBrokerState::DIRECTORY_OWNER_HUNG
                > CapnpBrokerState::DIRECTORY_BRIDGE_TIMEOUT * 10
        );
        drop(remote);

        // Paused: the owner's busy spell and the patience are timed by the
        // test's clock, never by how promptly a loaded host runs the timers.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let busy = Duration::from_millis(300);
            let answered =
                await_while_asked(Duration::from_secs(5), std::future::pending(), async {
                    tokio::time::sleep(busy).await;
                    7
                })
                .await;
            assert_eq!(answered, Ok(7), "a busy owner is waited for");

            let hung = await_while_asked(
                Duration::from_millis(200),
                std::future::pending(),
                std::future::pending::<()>(),
            )
            .await;
            assert_eq!(hung, Err(ActionDirectoryBridgeError::TimedOut));

            let mut reply = DirectoryReply::Pending(completer);
            drop(answer);
            let abandoned = tilo_test_env::within(
                "an abandoned asker stops the wait",
                reply.awaited(std::future::pending::<()>()),
            )
            .await;
            assert_eq!(abandoned, Err(ActionDirectoryBridgeError::Unavailable));
        });
    }

    /// An asker that goes away — a shell surface dropping its oneshot, a
    /// remote caller dropping its answer — releases the directory request
    /// the moment it does. The clock is paused: any timer the wait armed
    /// to notice would have to advance it, and none may.
    #[tokio::test(start_paused = true)]
    async fn directory_wait_ends_immediately_when_asker_drops() {
        let (shell, receiver) = tokio::sync::oneshot::channel();
        let (completer, answer) =
            tilo_actions::remote::pending::<super::ActionDirectoryBridgeReply>();
        for (mut reply, asker) in [
            (
                DirectoryReply::Oneshot(shell),
                Box::new(receiver) as Box<dyn Send>,
            ),
            (DirectoryReply::Pending(completer), Box::new(answer)),
        ] {
            let started = tokio::time::Instant::now();
            let owner_never_answers = reply.awaited(std::future::pending::<()>());
            tokio::pin!(owner_never_answers);
            assert!(
                futures::poll!(owner_never_answers.as_mut()).is_pending(),
                "the asker is still there"
            );
            drop(asker);
            assert_eq!(
                owner_never_answers.await,
                Err(ActionDirectoryBridgeError::Unavailable)
            );
            assert_eq!(
                started.elapsed(),
                Duration::ZERO,
                "the departure woke the wait; no timer had to notice it"
            );
        }
    }
}

struct EagerActionInvokeRequest {
    caller: tilo_actions::remote::Caller,
    own_peer: Option<u64>,
    launch: Option<LaunchScope>,
    origin: InvocationCaller,
    request: tilo_actions::remote::InvokeRequest,
    reply: tilo_actions::remote::Completer<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
    >,
}

enum ActionDirectoryBridgeRequest {
    InvokeEager(Box<EagerActionInvokeRequest>),
    Query {
        actor: BrokerActor,
        cluster_id: String,
        query: runtime_collab::ActionDirectoryQuery,
        reply: DirectoryReply,
    },
    Resolve {
        actor: BrokerActor,
        cluster_id: String,
        peer_id: u64,
        revision: String,
        resource: String,
        action: String,
        reply: DirectoryReply,
    },
    Start {
        actor: BrokerActor,
        target: DirectoryActionTarget,
        argument: Option<String>,
        confirmation: Option<DirectoryActionConfirmation>,
        reply: DirectoryReply,
    },
}

/// One directory action as the caller chose it: the workspace cluster and
/// live app peer that offered it, the directory revision it was chosen at,
/// and its resource/action address. The broker re-resolves it at start;
/// nothing here is trusted from an earlier page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryActionTarget {
    pub cluster_id: String,
    pub peer_id: u64,
    pub revision: String,
    pub address: tilo_actions::ActionAddress,
}

/// Thread-safe sender for palette/search code.  The receiver is drained only
/// by the broker's `LocalSet`, where its Rc-backed capability clients remain
/// valid.  Callers receive a oneshot they may poll from any executor.
#[derive(Clone)]
pub struct ActionDirectoryRequestQueue {
    sender: std::sync::mpsc::SyncSender<ActionDirectoryBridgeRequest>,
    /// Wakes the accept loop, which is the only drainer of this queue.
    turn: std::sync::Arc<tokio::sync::Notify>,
}

impl ActionDirectoryRequestQueue {
    const CAPACITY: usize = 64;

    pub(crate) fn unavailable() -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel(0);
        drop(receiver);
        Self {
            sender,
            turn: std::sync::Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn deliver(
        &self,
        request: ActionDirectoryBridgeRequest,
    ) -> Result<(), ActionDirectoryBridgeError> {
        match self.sender.try_send(request) {
            Ok(()) => {
                self.turn.notify_one();
                Ok(())
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(ActionDirectoryBridgeError::Busy),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err(ActionDirectoryBridgeError::Unavailable)
            }
        }
    }

    /// # Errors
    ///
    /// Returns an error when the query is invalid, the bounded broker queue is
    /// full, or the broker is unavailable.
    pub fn try_query(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        if validate_directory_query_input(&query).is_err() {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        let reply = DirectoryReply::Oneshot(reply);
        self.deliver(ActionDirectoryBridgeRequest::Query {
            actor,
            cluster_id: cluster_id.into(),
            query,
            reply,
        })
        .map(|()| response)
    }

    /// # Errors
    ///
    /// Returns an error when the target is invalid, the bounded broker queue
    /// is full, or the broker is unavailable.
    pub fn try_resolve(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        peer_id: u64,
        revision: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        let cluster_id = cluster_id.into();
        let revision = revision.into();
        let resource = resource.into();
        let action = action.into();
        if validate_directory_target_input(&cluster_id, &revision, &resource, &action, None, None)
            .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        let reply = DirectoryReply::Oneshot(reply);
        self.deliver(ActionDirectoryBridgeRequest::Resolve {
            actor,
            cluster_id,
            peer_id,
            revision,
            resource,
            action,
            reply,
        })
        .map(|()| response)
    }

    /// [`Self::try_query`] answered as a remote-action pending value.
    ///
    /// # Errors
    ///
    /// Returns an error when the query is invalid, the bounded broker queue is
    /// full, or the broker is unavailable.
    pub(crate) fn try_query_pending(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> Result<tilo_actions::remote::Pending<ActionDirectoryBridgeReply>, ActionDirectoryBridgeError>
    {
        if validate_directory_query_input(&query).is_err() {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tilo_actions::remote::pending();
        self.enqueue(ActionDirectoryBridgeRequest::Query {
            actor,
            cluster_id: cluster_id.into(),
            query,
            reply: DirectoryReply::Pending(reply),
        })
        .map(|()| response)
    }

    /// [`Self::try_resolve`] answered as a remote-action pending value.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is invalid, the bounded broker queue
    /// is full, or the broker is unavailable.
    pub(crate) fn try_resolve_pending(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        peer_id: u64,
        revision: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Result<tilo_actions::remote::Pending<ActionDirectoryBridgeReply>, ActionDirectoryBridgeError>
    {
        let cluster_id = cluster_id.into();
        let revision = revision.into();
        let resource = resource.into();
        let action = action.into();
        if validate_directory_target_input(&cluster_id, &revision, &resource, &action, None, None)
            .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tilo_actions::remote::pending();
        self.enqueue(ActionDirectoryBridgeRequest::Resolve {
            actor,
            cluster_id,
            peer_id,
            revision,
            resource,
            action,
            reply: DirectoryReply::Pending(reply),
        })
        .map(|()| response)
    }

    fn enqueue(
        &self,
        request: ActionDirectoryBridgeRequest,
    ) -> Result<(), ActionDirectoryBridgeError> {
        self.deliver(request)
    }

    /// # Errors
    ///
    /// Returns an error when invocation inputs are invalid, the bounded broker
    /// queue is full, or the broker is unavailable.
    pub fn try_start_resolved(
        &self,
        actor: BrokerActor,
        target: DirectoryActionTarget,
        argument: Option<String>,
        confirmation: Option<DirectoryActionConfirmation>,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        // A start names the revision its target was chosen at: the
        // admission is bound to it, never to the directory as it is now.
        if target.revision.is_empty()
            || validate_directory_target_input(
                &target.cluster_id,
                &target.revision,
                &target.address.resource,
                &target.address.action,
                argument.as_deref(),
                confirmation.as_ref(),
            )
            .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        let reply = DirectoryReply::Oneshot(reply);
        self.deliver(ActionDirectoryBridgeRequest::Start {
            actor,
            target,
            argument,
            confirmation,
            reply,
        })
        .map(|()| response)
    }
}

const DIRECTORY_MAX_IDENTIFIER_BYTES: usize = 512;
const DIRECTORY_MAX_KIND_BYTES: usize = 256;
const DIRECTORY_MAX_LABEL_BYTES: usize = 4 * 1024;
const DIRECTORY_MAX_DESCRIPTION_BYTES: usize = 8 * 1024;
// A directory row's contract, parameter declarations and presentation are
// bounded where they are decoded (`runtime_collab::MAX_DIRECTORY_*_WIRE_BYTES`);
// this bounds the argument an invocation carries.
const DIRECTORY_MAX_PARAMS_BYTES: usize = 32 * 1024;
const DIRECTORY_MAX_QUERY_REVISION_BYTES: usize = 512;
const DIRECTORY_MAX_QUERY_NEEDLE_BYTES: usize = 4 * 1024;
const DIRECTORY_MAX_CURSOR_BYTES: usize = 4 * 1024;
const DIRECTORY_MAX_CONFIRMATION_BYTES: usize = 4 * 1024;

fn validate_directory_text(
    value: &str,
    field: &str,
    maximum: usize,
    required: bool,
) -> Result<(), capnp::Error> {
    if (required && value.is_empty()) || value.len() > maximum || value.contains('\0') {
        return Err(capnp::Error::failed(format!(
            "invalid action directory {field}"
        )));
    }
    Ok(())
}

fn validate_directory_identifier(
    value: &str,
    field: &str,
    maximum: usize,
    required: bool,
) -> Result<(), capnp::Error> {
    validate_directory_text(value, field, maximum, required)?;
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(capnp::Error::failed(format!(
            "invalid action directory {field}"
        )));
    }
    Ok(())
}

fn validate_directory_query_input(
    query: &runtime_collab::ActionDirectoryQuery,
) -> Result<(), capnp::Error> {
    validate_directory_text(
        &query.revision,
        "query revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    validate_directory_text(
        &query.needle,
        "query needle",
        DIRECTORY_MAX_QUERY_NEEDLE_BYTES,
        false,
    )?;
    validate_directory_identifier(
        &query.parent_resource,
        "query parent resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        false,
    )?;
    if query.cursor.len() > DIRECTORY_MAX_CURSOR_BYTES {
        return Err(capnp::Error::failed(
            "invalid action directory query cursor".into(),
        ));
    }
    validate_directory_text(
        &query.resource_name,
        "query resource name",
        DIRECTORY_MAX_LABEL_BYTES,
        false,
    )?;
    // A name lookup is one whole answer about one level (the top, or one
    // parent's children): it neither continues a page nor narrows a needle.
    if !query.resource_name.is_empty() && (!query.cursor.is_empty() || !query.needle.is_empty()) {
        return Err(capnp::Error::failed(
            "a resource name lookup takes no cursor or needle".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod directory_query_input_tests {
    use super::validate_directory_query_input;

    fn named(parent: &str) -> runtime_collab::ActionDirectoryQuery {
        runtime_collab::ActionDirectoryQuery {
            peer_id: 1,
            revision: String::new(),
            cursor: Vec::new(),
            needle: String::new(),
            parent_resource: parent.into(),
            max_results: 8,
            max_bytes: 65536,
            max_work: 1,
            max_depth: 1,
            resource_name: "Todo".into(),
        }
    }

    /// A name is asked at the top level or among one parent's children,
    /// whole: never continued by a cursor or narrowed by a needle.
    #[test]
    fn a_name_is_asked_at_one_level_whole() {
        assert!(validate_directory_query_input(&named("")).is_ok());
        assert!(validate_directory_query_input(&named("notes.notebook")).is_ok());
        let mut continued = named("notes.notebook");
        continued.cursor = b"next".to_vec();
        assert!(validate_directory_query_input(&continued).is_err());
        let mut narrowed = named("");
        narrowed.needle = "milk".into();
        assert!(validate_directory_query_input(&narrowed).is_err());
    }
}

fn validate_directory_target_input(
    cluster_id: &str,
    revision: &str,
    resource: &str,
    action: &str,
    argument: Option<&str>,
    confirmation: Option<&DirectoryActionConfirmation>,
) -> Result<(), capnp::Error> {
    validate_directory_identifier(
        cluster_id,
        "directory cluster",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_text(
        revision,
        "directory revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    validate_directory_identifier(
        resource,
        "directory resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_identifier(
        action,
        "directory action",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    if let Some(argument) = argument {
        validate_directory_text(
            argument,
            "directory argument",
            DIRECTORY_MAX_PARAMS_BYTES,
            false,
        )?;
    }
    if let Some(DirectoryActionConfirmation::Phrase(phrase)) = confirmation {
        validate_directory_text(
            phrase,
            "directory confirmation",
            DIRECTORY_MAX_CONFIRMATION_BYTES,
            true,
        )?;
    }
    Ok(())
}

/// A person's confirmation, given on their own device, of one invocation
/// someone acting for them makes.
struct Witness {
    binding: WitnessBinding,
    expires: std::time::Instant,
}

/// The original invocation and publication the person actually answered.
/// Arguments are hashed so the registry never retains disclosure contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WitnessBinding {
    actor: String,
    provider: String,
    room: String,
    revision: Option<String>,
    publication: (u64, u64),
    invocation: tilo_actions::InvocationId,
    resource: String,
    action: String,
    argument_digest: [u8; 32],
}

#[derive(Clone, Copy)]
enum WitnessPublicationKind {
    Surface,
    Directory,
}

struct ConfirmationGrant {
    /// Only set after consuming a host-issued witness for these exact arguments.
    host_witnessed: Option<WitnessBinding>,
    peer_id: u64,
    resource: String,
    action: String,
    /// Binds the exact arguments without retaining secrets or other raw user
    /// input in the grant registry.
    argument_digest: [u8; 32],
    actor: String,
    expires: std::time::Instant,
}

fn argument_digest(argument: Option<&str>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    match argument {
        Some(value) => {
            hasher.update(b"some\0");
            hasher.update(value.as_bytes());
        }
        None => {
            hasher.update(b"none\0");
        }
    }
    *hasher.finalize().as_bytes()
}

/// One subscription onto the broker fanout. Slot-indexed inside a
/// [`ChannelSubscribers`] table so it can be dropped en masse when
/// the connection tears down.
struct OpSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<OpEnvelope>,
}
struct PresenceSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<PresenceEnvelope>,
}
struct ContextSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<ContextEnvelope>,
    /// Authenticated native membership projection and the exact admission it
    /// came from. Incremental label/presence frames never replace it.
    native: Option<NativeContext>,
    /// Shares the original receiver; membership refresh belongs to its document row.
    private_alias: bool,
    /// The room's verified agents this receiver was last told.
    history_agents: Vec<collab_types::history::RosterAgent>,
}

#[derive(Clone)]
struct NativeContext {
    admitted: AdmittedJoinIdentity,
    roster: Vec<ui::collaboration::Collaborator>,
}

/// Per-channel subscriber registry, keyed by `join_channel(id)`. A
/// `channelId` carries the whole address ([`runtime_collab::route::channel_key`]
/// encodes `(app, cluster, record)`), decoded via
/// [`CrdtRoute::from_channel_key`]; delivery is [`CrdtRoute::delivers_to`].
#[derive(Default)]
struct ChannelSubscribers {
    op: Vec<OpSub>,
    presence: Vec<PresenceSub>,
    context: Vec<ContextSub>,
}

type ActionCatalogueSnapshot = Option<Arc<str>>;
type ActionCatalogueSubscriber = (String, BrokerActor, watch::Sender<ActionCatalogueSnapshot>);

/// Exact pinned host and route, minted only alongside a broker-allocated Unix pair.
/// This scope is never accepted from the wire or exposed as a public constructor.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalHostWorkerScope {
    route: CrdtRoute,
    certificate: tilo_user_identity::DeviceCert,
}

/// A unique actual private connection, independent of device role. A native
/// primary device and a delegated Cloud host retain their existing admission paths.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PrivateWorkerRegistration {
    identity: uuid::Uuid,
    route: CrdtRoute,
    certificate: tilo_user_identity::DeviceCert,
}
/// Host-owned admission for one private SDK descriptor. Never serialized on the wire.
#[derive(Clone)]
struct WorkerBinding {
    host_scope: Option<LocalHostWorkerScope>,
    registration: Option<PrivateWorkerRegistration>,
    route: CrdtRoute,
    root: [u8; 32],
    device: [u8; 32],
}
struct PendingWorker {
    stream: std::os::unix::net::UnixStream,
    binding: WorkerBinding,
}
/// What the caller brings to one invocation: the argument, the actor it runs
/// as, the one-use confirmation token it presents and its ambient context.
/// Admission checks every field; none of them grants authority by itself.
struct InvocationInputs<'a> {
    /// Exact remote request before resolving a provider alias for delivery.
    original_request: Option<&'a tilo_actions::remote::InvokeRequest>,
    /// The caller's lifecycle identity, independent of execution authority.
    requested: Option<tilo_actions::InvocationId>,
    /// Who the invocation is admitted for, when the door knows them as a
    /// caller: kept with the running invocation, so an edit its effect
    /// submits is admitted for them too.
    caller: Option<&'a tilo_actions::remote::Caller>,
    execution: Option<ExecutionScope>,
    argument: Option<&'a str>,
    actor: &'a BrokerActor,
    confirmation_token: Option<&'a str>,
    context: Option<tilo_actions::ActionContext>,
}
/// Who an admission is held for and where it came from: genuine durable
/// caller evidence, the directory revision the target was admitted at (the
/// one the caller chose it at or, when it named none, the one its owner
/// resolved it against; `None` only for an action no directory resolved), a
/// private owner and original caller, the original policy time a retained
/// invocation keeps, and a task-owned durable identity. Each call site names
/// every field, so no custody input is filled implicitly.
struct AdmissionProvenance<'a> {
    durable: Option<DurableContext<'a>>,
    revision: Option<&'a str>,
    origin: Option<InvocationCaller>,
    retained_time: Option<&'a crate::peer_actions::PolicyTime>,
}
/// Shared typed broker state, attached to the transport-neutral fanout core.
pub struct CapnpBrokerState {
    /// Serializes the cloneable shell facade against final shutdown. Broker
    /// `LocalSet` tasks are dropped before shutdown takes the write side.
    lifecycle: RwLock<()>,
    accepting: AtomicBool,
    stopped: AtomicBool,
    /// Keyed by `join_channel`'s `channel_id`; lazily inserted on first join.
    channels: Mutex<HashMap<String, ChannelSubscribers>>,
    /// `Weak` so broker teardown isn't blocked by an outliving connection.
    fanout: Weak<FanoutState>,
    /// Durable validation, audit, redaction, and scrollback state.
    broker_channels: Arc<Mutex<BrokerChannels>>,
    /// A publisher with no network behind it ([`PeerPublisher::disabled`]) is legitimate
    /// (unit tests, a shell started without peer sync).
    publisher: Arc<PeerPublisher>,
    durable_owner_required: bool,
    committed_read_budget: Arc<committed_document::Budget>,
    committed_read_id: AtomicU64,
    owner_commit_failure: Mutex<Option<String>>,
    owner_commit_order: AsyncMutex<()>,
    /// The shell's one replicated workspace owner/capability authority.
    /// `None` exists only for transitional test/diagnostic brokers; a
    /// certified join fails closed when they have no authority to consult.
    membership: Option<WorkspaceTreeHandle>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    certificate_authority: std::sync::OnceLock<Arc<dyn super::peer_actions::CertificateAuthority>>,
    /// Latest complete action surface per live connection.
    surfaces: Mutex<HashMap<u64, PublishedSurface>>,
    /// Per-connection incarnations, independent of unrelated discovery churn.
    witness_publications: Mutex<HashMap<u64, (u64, u64)>>,
    next_witness_publication: AtomicU64,
    /// Each connection's surface as `publishActionSurface` deltas built it:
    /// the shapes and bindings the next delta applies to.
    surface_tables: Mutex<HashMap<u64, runtime_collab::SurfaceTable>>,
    /// Monotonic generation for shell-side retained action projections.
    /// Every change to a published surface moves it; a change to an
    /// application's invocation stream notifies its receivers without
    /// moving it. A launch waiting for an app to publish an action and open
    /// its stream wakes on either.
    surface_changes: tokio::sync::watch::Sender<u64>,
    /// Last published surface per app, workspace and record, kept across
    /// restarts. Catalogue state, never an executable capability by itself.
    launchable: launchable::LaunchableCatalogue,
    /// Workspace `open`s waiting for the instance each started to publish.
    workspace_opens: workspace_open::WorkspaceOpens,
    /// The window instance the shell launched each connected app for, by
    /// peer id: which tab's window the records it holds are shown in.
    window_instances: Mutex<HashMap<u64, u64>>,
    /// Pull-only, revisioned directories owned by currently connected apps.
    /// This intentionally has no durable/archive counterpart: retaining a
    /// capability after disconnect would make a stale live-only row look
    /// executable.
    action_directory_bridge: ActionDirectoryRequestQueue,
    peer_action_queue: super::peer_actions::PeerActionQueue,
    peer_action_rx: Mutex<Option<tokio::sync::mpsc::Receiver<super::peer_actions::Request>>>,
    action_directory_roots: Mutex<HashMap<u64, ActionDirectoryRoot>>,
    canonical_directories: Mutex<canonical_route::CanonicalDirectories>,
    native_checkouts: Mutex<HashMap<u64, native_checkout::NativeCheckout>>,
    action_directory_root_snapshot: Mutex<ActionDirectoryRootSnapshot>,
    /// Directed invocation stream registered by each application connection.
    invocation_subscribers: Mutex<HashMap<u64, mpsc::UnboundedSender<InvocationEnvelope>>>,
    action_catalogue_subscribers: Mutex<HashMap<u64, ActionCatalogueSubscriber>>,
    pending_invocations: Mutex<HashMap<u64, PendingInvocation>>,
    private_drafts: Mutex<private_draft::Drafts>,
    approved_check_jobs: Mutex<approved_checks::Registry>,
    external_moderation_jobs: Mutex<external_moderation::Registry>,
    /// Remote-action invocations admitted through the action host, keyed by
    /// caller and invocation id so a retried id is idempotent.
    remote_invocations: Mutex<tilo_actions::remote::InvocationLedger<u64>>,
    /// Shortcut runs the door finished and agent runs of workspace tasks
    /// that ended, until history records them.
    ended_runs: crate::task_runs::EndedRuns,
    /// What each application declared it may do on the action door under
    /// its own identity.
    app_action_access: RwLock<crate::app_actions::AppActionAccessResolver>,
    /// Who is asked before an application uses another application's
    /// action it holds no grant for. Nobody, until the host installs its
    /// sheet: such a request is then refused.
    app_action_consent: RwLock<Option<Arc<dyn crate::app_actions::AppActionConsentPrompter>>>,
    /// The notices of runs' questions given, by handle, with their room.
    run_question_notices: Mutex<HashMap<String, String>>,
    /// Applications with a grant question on screen, by room: one at a
    /// time each, so an app cannot bury the person in sheets.
    app_action_consent_pending: Mutex<std::collections::HashSet<(String, String)>>,
    /// Steps of interactive shortcut runs waiting for their person.
    step_prompts: crate::automation::StepPrompts,
    /// Bumped whenever a pending or completed invocation changes, so hosts
    /// can refresh retained invocation projections exactly when needed and
    /// a waiter on a running invocation wakes when it may have moved.
    invocation_generation: tokio::sync::watch::Sender<u64>,
    execution_activity: ActivityBus,
    /// Admission serialization covers policy, one-use confirmation, the durable
    /// barrier and the single application delivery. Never held across awaits.
    admission_lock: Mutex<()>,
    journal: Mutex<Result<Option<InvocationJournal>, String>>,
    /// Invocations whose definite outcome is being written: expiry and a
    /// disconnect leave them alone until it is durable and heard.
    committing_invocations: Mutex<HashSet<u64>>,
    local_report_receipts: Mutex<local_document_action::ReceiptCache>,
    stable_invocation_subscribers: Mutex<std::collections::HashSet<u64>>,
    /// Broker-owned governed membership surface per native room.
    workspace_membership_peers: Mutex<HashMap<String, u64>>,
    completed_invocations: Mutex<HashMap<u64, (InvocationOwner, tilo_actions::InvocationUpdate)>>,
    /// Host steps whose result stays current, while they are open.
    live_views: live_views::LiveViews,
    // Retain result authorization even when the host has no durable journal.
    // Entries share the completed invocation cache's bounded lifetime.
    checkpoint_jobs: Mutex<HashMap<u64, (String, String, tilo_actions::ActionContract)>>,
    /// Jobs whose action's contract declares untrusted content, so every
    /// observation of them says so. They share the completed invocation
    /// cache's bounded lifetime.
    untrusted_jobs: Mutex<std::collections::HashSet<u64>>,
    confirmation_grants: Mutex<HashMap<String, ConfirmationGrant>>,
    /// Confirmations people gave on their own device for an invocation
    /// someone acting for them makes; see [`Self::witness_confirmation`].
    witnessed_confirmations: Mutex<HashMap<String, Witness>>,
    cancellation_requests: Mutex<std::collections::HashSet<u64>>,
    task_subscribers: Mutex<HashMap<u64, TaskSubscription>>,
    pending_task_runs: Mutex<HashMap<u64, PendingTaskRun>>,
    invocation_id: AtomicU64,
    services: HostServices,
    workers: Mutex<Vec<PendingWorker>>,
    /// Wakes the broker loop to adopt a newly admitted local worker at once
    /// rather than on its next housekeeping tick.
    worker_admitted: tokio::sync::Notify,
    /// Queued directory work, a test callback, or a new invocation. The
    /// accept loop parks until one of these; a timer used to notice them.
    local_turn: std::sync::Arc<tokio::sync::Notify>,
    /// Admitted offloadable invocations waiting for the turn to ask
    /// [`HostServices::action_offload`] where they run.
    offloads: Mutex<Vec<offload_door::QueuedOffload>>,
    /// Offloadable invocations the route sent back here, by wire id: what
    /// was asked and when the app was handed it.
    offloaded_here:
        Mutex<HashMap<u64, (tilo_actions::offload::OffloadRequest, std::time::Instant)>>,
    /// The worker connections' admitted joins, by connection and route
    /// ([`host_workers`]); never reconstructed from surfaces.
    host_workers: Mutex<host_workers::HostWorkers>,
    /// The connection each further route hold belongs to ([`route_holds`]).
    route_holders: Mutex<route_holds::RouteHolders>,
    /// Peer ids of the broker's own services on its thread's runtime.
    service_peers: Mutex<Vec<u64>>,
    /// How often the broker thread unwound and started again.
    thread_restarts: AtomicU64,
    #[cfg(test)]
    thread_callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl CapnpBrokerState {
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub(crate) fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && !self.is_stopped()
    }

    pub(crate) fn request_stop(&self) {
        self.accepting.store(false, Ordering::Release);
        self.local_turn.notify_one();
        // Held committed-read waits re-check custody, which now refuses.
        tilo_identity_host::policy_change::announce();
    }

    /// Hold the broker lifecycle read side for one complete shell-facade
    /// operation. Once shutdown obtains the write side, no stale facade can
    /// admit or publish new work.
    pub(crate) fn while_live<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let _live = self
            .lifecycle
            .read()
            .map_err(|_| "broker unavailable".to_string())?;
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        let _call =
            crate::collab_broker::BrokerFacadeCallGuard::enter(std::ptr::from_ref(self).addr());
        operation(self)
    }

    /// Like `while_live`, but discard a newly cloned handle if a callback
    /// requested this exact broker's shutdown before the handle escaped.
    pub(crate) fn while_live_fenced<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let result = self.while_live(operation);
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        result
    }

    /// Finalize the shared state after the `LocalSet` and all of its RPC tasks
    /// have been dropped. Surviving Arc-backed facades become inert, and the
    /// journal lock is released before the socket path can be reclaimed.
    pub(crate) fn stop(&self) {
        let _exclusive = match self.lifecycle.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.request_stop();
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }

        macro_rules! clear {
            ($field:expr) => {
                match $field.lock() {
                    Ok(mut value) => value.clear(),
                    Err(poisoned) => poisoned.into_inner().clear(),
                }
            };
        }
        // Host-service callbacks can outlive the LocalSet. Serialize their
        // journal transitions with retirement before releasing the writer.
        let _admission = match self.admission_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.execution_activity.close();
        clear!(self.host_workers);
        clear!(self.route_holders);
        clear!(self.channels);
        clear!(self.surfaces);
        clear!(self.witness_publications);
        clear!(self.surface_tables);
        clear!(self.action_directory_roots);
        clear!(self.native_checkouts);
        clear!(self.invocation_subscribers);
        clear!(self.action_catalogue_subscribers);
        clear!(self.pending_invocations);
        clear!(self.private_drafts);
        clear!(self.stable_invocation_subscribers);
        clear!(self.workspace_membership_peers);
        clear!(self.confirmation_grants);
        clear!(self.cancellation_requests);
        clear!(self.task_subscribers);
        clear!(self.pending_task_runs);
        clear!(self.workers);
        // A launch waiting for an app to publish learns the broker stopped.
        self.invocation_streams_changed();
        #[cfg(test)]
        clear!(self.thread_callbacks);
        match self.peer_action_rx.lock() {
            Ok(mut receiver) => {
                receiver.take();
            }
            Err(poisoned) => {
                poisoned.into_inner().take();
            }
        }
        self.refresh_action_directory_root_snapshot();

        let journal = {
            let mut slot = match self.journal.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::replace(&mut *slot, Ok(None))
        };
        drop(journal);
    }

    #[cfg(test)]
    fn enqueue_thread_callback(&self, callback: Box<dyn FnOnce() + Send>) {
        match self.thread_callbacks.lock() {
            Ok(mut callbacks) => callbacks.push(callback),
            Err(poisoned) => poisoned.into_inner().push(callback),
        }
        self.local_turn.notify_one();
    }

    #[cfg(test)]
    fn callbacks_pending(&self) -> usize {
        match self.thread_callbacks.lock() {
            Ok(callbacks) => callbacks.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Allocate a private connected descriptor for an actual host-mounted worker.
    /// The peer must still prove the exact current certificate on every channel join.
    ///
    /// # Errors
    ///
    /// Returns a message when the owner is unhealthy, the route or certificate is refused, or admission is full.
    pub fn connect_local_worker(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<std::os::unix::net::UnixStream, String> {
        self.connect_local_worker_registered(route, certificate, None)
    }
    fn connect_local_worker_registered(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
        registration: Option<uuid::Uuid>,
    ) -> Result<std::os::unix::net::UnixStream, String> {
        self.owner_health()?;
        if route.app_id.is_empty() || !route.is_publishable() {
            return Err("worker needs an exact app route".into());
        }
        let authority = self
            .certificate_authority
            .get()
            .ok_or("current certificate authority unavailable")?;
        let room = uuid::Uuid::parse_str(&route.cluster_id).map_err(|_| "invalid worker room")?;
        let host_scope = if certificate.capabilities.can_act_as_host {
            authority.authorize_host_worker(&route, certificate)?;
            Some(LocalHostWorkerScope {
                route: route.clone(),
                certificate: certificate.clone(),
            })
        } else {
            authority.authorize_join(room, certificate)?;
            None
        };
        let now = self.policy_now_ns()?;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "worker certificate rejected")?;
        let verified =
            VerifiedJoinIdentity::of(certificate, self.capped_in(&route.cluster_id, certificate));
        self.membership
            .as_ref()
            .ok_or("current workspace authority unavailable")?
            .decide_in_room(&route.cluster_id, |workspace| {
                admit_verified_join_at(workspace, &route, verified, now / 1_000_000_000)
            })??;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "worker admission unavailable")?;
        if workers.len() >= 32 {
            return Err("worker admission queue full".into());
        }
        let (server, client) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
        server.set_nonblocking(true).map_err(|e| e.to_string())?;
        workers.push(PendingWorker {
            stream: server,
            binding: WorkerBinding {
                host_scope,
                registration: registration.map(|identity| PrivateWorkerRegistration {
                    identity,
                    route: route.clone(),
                    certificate: certificate.clone(),
                }),
                route,
                root: certificate.user_public_key,
                device: certificate.device_public_key,
            },
        });
        drop(workers);
        self.worker_admitted.notify_one();
        Ok(client)
    }

    const MAX_RETAINED_INVOCATIONS: usize = 256;
    const INVOCATION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    const CANCELLATION_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
    /// Broker ceilings are deliberately lower than Cap'n Proto list limits:
    /// applications must not turn a palette keystroke into an unbounded RPC.
    const DIRECTORY_MAX_RESULTS: u32 = 256;
    const DIRECTORY_MAX_BYTES: u32 = 256 * 1024;
    const DIRECTORY_MAX_WORK: u32 = 4_096;
    const DIRECTORY_MAX_DEPTH: u16 = 32;
    /// How long the shell's own surfaces (palette, search) wait for an app's
    /// directory before keeping their last results.
    const DIRECTORY_BRIDGE_TIMEOUT: Duration = Duration::from_secs(2);
    /// An app that has not answered its directory in this long is hung, not
    /// busy: its asker hears it timed out. Remote askers normally give up
    /// sooner, on their own deadline.
    const DIRECTORY_OWNER_HUNG: Duration = Duration::from_secs(60);

    fn expire_stale_invocations(&self) {
        let now = std::time::Instant::now();
        self.expire_stale_invocations_at(
            now,
            Self::INVOCATION_IDLE_TIMEOUT,
            Self::CANCELLATION_GRACE,
        );
        self.expire_workspace_opens(now);
        if let Ok(mut drafts) = self.private_drafts.lock() {
            drafts.retain(|_, entry| entry.expires > now);
        }
    }

    /// When the next pending invocation can expire. `None` while nothing is
    /// in flight, so the accept loop parks instead of waking every 100 ms.
    fn next_invocation_deadline(&self) -> Option<std::time::Instant> {
        let opens = self.workspace_opens.next_deadline();
        let drafts = self
            .private_drafts
            .lock()
            .ok()
            .and_then(|drafts| drafts.values().map(|entry| entry.expires).min());
        let pending = self.pending_invocations.lock().ok()?;
        pending
            .values()
            .filter_map(|invocation| {
                let idle_at = invocation
                    .last_activity
                    .checked_add(Self::INVOCATION_IDLE_TIMEOUT)?;
                let cancel_at = invocation
                    .cancellation_requested_at
                    .and_then(|requested| requested.checked_add(Self::CANCELLATION_GRACE));
                Some(match cancel_at {
                    Some(cancel) if cancel < idle_at => cancel,
                    _ => idle_at,
                })
            })
            .chain(opens)
            .chain(drafts)
            .min()
    }

    fn expire_stale_invocations_at(
        &self,
        now: std::time::Instant,
        idle_timeout: std::time::Duration,
        cancellation_grace: std::time::Duration,
    ) {
        let expired = {
            let Ok(mut pending) = self.pending_invocations.lock() else {
                return;
            };
            #[allow(
                clippy::needless_collect,
                reason = "expired ids must be snapshotted before removing from the same pending-invocation map"
            )]
            let ids = pending
                .iter()
                .filter(|(id, _)| !self.committing(**id))
                .filter_map(|(id, invocation)| {
                    let cancellation_expired =
                        invocation
                            .cancellation_requested_at
                            .is_some_and(|requested| {
                                now.saturating_duration_since(requested) >= cancellation_grace
                            });
                    let idle_expired =
                        now.saturating_duration_since(invocation.last_activity) >= idle_timeout;
                    (cancellation_expired || idle_expired).then_some((*id, cancellation_expired))
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|(id, cancellation_expired)| {
                    pending
                        .remove(&id)
                        .map(|invocation| (id, cancellation_expired, invocation))
                })
                .collect::<Vec<_>>()
        };
        for (id, cancellation_expired, pending) in expired {
            pending.cancellation_fence.store(true, Ordering::Release);
            let (code, message) = if cancellation_expired {
                (
                    "action_cancellation_timeout",
                    "application did not acknowledge cancellation before the deadline",
                )
            } else {
                (
                    "action_timeout",
                    "application stopped reporting action progress before the deadline",
                )
            };
            let completion = InvocationCompletion {
                code: code.into(),
                message: message.into(),
            };
            let update = tilo_actions::InvocationUpdate::Failed {
                invocation: pending.update.invocation(),
                error: tilo_actions::ActionFailure {
                    code: completion.code.clone(),
                    message: completion.message.clone(),
                    details: None,
                    retryable: pending.retryable_on_disconnect,
                },
            };
            let (update, completion) = if pending.journal_key.is_some() {
                let update = invocation_journal::unknown(pending.update.invocation());
                let completion = InvocationCompletion {
                    code: "action_outcome_unknown".into(),
                    message: "application outcome unknown after silence".into(),
                };
                (update, completion)
            } else {
                (update, completion)
            };
            if self
                .persist_uncertain_outcome(&pending, update.clone())
                .is_err()
            {
                self.execution_activity
                    .publish_status(pending.activity.as_ref(), ExecutionStatus::OutcomeUnknown);
                continue;
            }
            self.publish_activity(pending.activity.as_ref(), &update);
            self.record_completion(id, pending.completion_owner(), update.clone());
            if let Ok(mut requests) = self.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            self.record_effect_receipt(&pending, &update);
        }
    }

    fn record_completion(
        &self,
        id: u64,
        actor: InvocationOwner,
        update: tilo_actions::InvocationUpdate,
    ) {
        if update.is_terminal() {
            self.offload_answered(id, &update);
        }
        if let Ok(mut completed) = self.completed_invocations.lock() {
            completed.insert(id, (actor, update));
            self.invocation_changed();
            while completed.len() > Self::MAX_RETAINED_INVOCATIONS {
                let Some(oldest) = completed.keys().min().copied() else {
                    break;
                };
                completed.remove(&oldest);
                if let Ok(mut checkpoints) = self.checkpoint_jobs.lock() {
                    checkpoints.remove(&oldest);
                }
                if let Ok(mut untrusted) = self.untrusted_jobs.lock() {
                    untrusted.remove(&oldest);
                }
            }
        }
    }

    fn owner_health(&self) -> Result<(), String> {
        if self.durable_owner_required {
            // A probe: the committed authority must be readable; nothing is copied.
            self.membership
                .as_ref()
                .ok_or("current workspace authority unavailable")?
                .with_tree(&mut |_| {})?;
        }

        if let Some(reason) = self
            .owner_commit_failure
            .lock()
            .map_err(|_| "owner health unavailable")?
            .as_ref()
        {
            return Err(reason.clone());
        }
        self.publisher.owner_commit_health()
    }

    async fn commit_owner_request(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
        record_kind: Option<String>,
    ) -> Result<(), capnp::Error> {
        // Dropping the waiting RPC cannot convert an admitted effect into a
        // refusal or leave the standalone capability apparently healthy.
        struct Waiting<'a> {
            failure: &'a Mutex<Option<String>>,
            complete: bool,
        }
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                if !self.complete
                    && let Ok(mut failure) = self.failure.lock()
                {
                    failure
                        .get_or_insert_with(|| "owner commit interrupted; outcome unknown".into());
                }
            }
        }
        self.owner_health().map_err(capnp::Error::failed)?;
        let mut waiting = Waiting {
            failure: &self.owner_commit_failure,
            complete: false,
        };
        let result = self.publisher.commit_owner(request, record_kind).await;
        waiting.complete = true;
        match result {
            Ok(_) => Ok(()),
            Err(reason) => {
                // The owner answered: retain its actual refusal/failure for
                // later clients instead of labeling this as a dropped RPC.
                if let Ok(mut failure) = self.owner_commit_failure.lock() {
                    failure.get_or_insert_with(|| format!("owner commit failed: {reason}"));
                }
                Err(capnp::Error::failed(reason))
            }
        }
    }

    async fn commit_effect_before_terminal(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<(), capnp::Error> {
        if !(self.durable_owner_required || self.publisher.requires_owner_for(&pending.cluster_id))
            || !update.is_terminal()
        {
            return Ok(());
        }
        self.owner_health().map_err(capnp::Error::failed)?;
        self.checked_policy_time(pending.policy_time.as_ref())
            .map_err(capnp::Error::failed)?;
        if pending.operation_kind != tilo_actions::OperationKind::Effect {
            return Ok(());
        }
        let status = tilo_actions::EffectReceiptStatus::from_terminal(update)
            .ok_or_else(|| capnp::Error::failed("nonterminal effect receipt".into()))?;
        let request = {
            let mut prepared = pending.prepared_effect.lock().map_err(|_| {
                capnp::Error::failed("effect receipt preparation unavailable".into())
            })?;
            if let Some(previous) = prepared.as_ref() {
                if previous.update != *update {
                    return Err(capnp::Error::failed(
                        "effect terminal outcome changed".into(),
                    ));
                }
                previous.request.clone()
            } else {
                let receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
                    update.invocation(),
                    &pending.actor,
                    pending.context.clone(),
                    status,
                    tilo_clock::unix_now_i64(),
                );
                let request = tilo_owner_commit::OwnerCommitRequest {
                    room: pending.cluster_id.clone(),
                    operation: tilo_owner_commit::CommitOperation::EffectReceipt {
                        invocation: update.invocation().0,
                        redacted: serde_json::to_vec(&receipt)
                            .map_err(|error| capnp::Error::failed(error.to_string()))?,
                    },
                };
                *prepared = Some(PreparedEffectCommit {
                    update: update.clone(),
                    request: request.clone(),
                });
                request
            }
        };
        if let Some(key) = &pending.journal_key {
            let (_, durable) = self
                .journal
                .lock()
                .map_err(|_| capnp::Error::failed("invocation journal unavailable".into()))?
                .as_mut()
                .map_err(|error| capnp::Error::failed(error.clone()))?
                .as_mut()
                .ok_or_else(|| capnp::Error::failed("durable admission unavailable".into()))?
                .stage_prepared_effect(key, update.clone(), &request)
                .map_err(capnp::Error::failed)?;
            durable.wait().await.map_err(capnp::Error::failed)?;
        }
        self.commit_owner_request(request, None).await
    }

    /// Reconcile an inert saved receipt, never dispatch or reconstruct action authority.
    async fn reconcile_prepared_effect(
        &self,
        record: DurableInvocation,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<DurableInvocation, String> {
        authorize()?;
        let Some(prepared) = record.prepared_effect.as_ref() else {
            return Ok(record);
        };
        if !record.uncertain || record.update.is_terminal() {
            return Ok(record);
        }
        self.owner_health()?;
        // The normal service supplies actual current governor custody and exact owner retry.
        // A saved request grants no authority, and no unavailable service fallback is allowed.
        if let Err(error) = self.publisher.commit_owner(prepared.request(), None).await {
            tracing::warn!(%error, "prepared effect remains uncertain");
            authorize()?;
            return Ok(record);
        }
        authorize()?;
        let (finished, durable) = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_mut()
            .map_err(|error| error.clone())?
            .as_mut()
            .ok_or("durable admission unavailable")?
            .stage_finished_effect(&record.key, prepared)?;
        durable.wait().await?;
        Ok(finished)
    }

    fn record_effect_receipt(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) {
        if self.durable_owner_required || self.publisher.requires_owner_for(&pending.cluster_id) {
            return;
        }
        if pending.operation_kind != tilo_actions::OperationKind::Effect {
            return;
        }
        let Some(status) = tilo_actions::EffectReceiptStatus::from_terminal(update) else {
            return;
        };
        if let Some(projection) = &self.services.legacy_workspace
            && let Err(error) = projection.record_effect(
                &pending.cluster_id,
                update.invocation(),
                &pending.actor,
                pending.context.clone(),
                status,
            )
        {
            tracing::warn!(%error, "native effect projection unavailable");
        }
    }

    /// So tests can register channel state before pushing signed
    /// envelopes through the capnp path.
    pub fn broker_channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.broker_channels)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn new(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<crate::test_support::WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        let services = membership
            .as_ref()
            .map(crate::test_support::WorkspaceTreeHandle::services)
            .unwrap_or_default();
        Self::new_configured(
            fanout,
            broker_channels,
            publisher,
            membership.map(|tree| tree.authority()),
            trusted_devices,
            None,
            services,
        )
    }

    fn configured_journal(
        config: Option<&super::peer_actions::NativePeerBrokerConfig>,
        membership: Option<&WorkspaceTreeHandle>,
    ) -> Result<Option<InvocationJournal>, String> {
        config
            .map(|config| config.journal_root.clone())
            .or_else(|| membership.and_then(|authority| authority.legacy_journal_root()))
            .map(|path| InvocationJournal::open(&path))
            .transpose()
    }

    fn new_configured(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
        config: Option<&super::peer_actions::NativePeerBrokerConfig>,
        services: HostServices,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        let (bridge_tx, bridge_rx) =
            std::sync::mpsc::sync_channel(ActionDirectoryRequestQueue::CAPACITY);
        let local_turn = std::sync::Arc::new(tokio::sync::Notify::new());
        let (peer_action_queue, peer_action_rx) = super::peer_actions::PeerActionQueue::channel();
        let journal = Self::configured_journal(config, membership.as_ref());
        let next_invocation = journal
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map_or(1, InvocationJournal::next_wire_id);
        (
            Arc::new(Self {
                channels: Mutex::new(HashMap::new()),
                lifecycle: RwLock::new(()),
                accepting: AtomicBool::new(true),
                stopped: AtomicBool::new(false),
                fanout: Arc::downgrade(fanout),
                broker_channels,
                publisher,
                durable_owner_required: config.is_some(),
                committed_read_budget: Arc::new(committed_document::Budget::default()),
                committed_read_id: AtomicU64::new(1),
                owner_commit_failure: Mutex::new(None),
                owner_commit_order: AsyncMutex::new(()),
                membership,
                trusted_devices,
                certificate_authority: config
                    .map(|config| config.certificate_authority.clone())
                    .or_else(|| services.native_certificates.clone())
                    .map_or_else(std::sync::OnceLock::new, std::sync::OnceLock::from),
                surfaces: Mutex::new(HashMap::new()),
                witness_publications: Mutex::new(HashMap::new()),
                next_witness_publication: AtomicU64::new(1),
                surface_tables: Mutex::new(HashMap::new()),
                surface_changes: tokio::sync::watch::Sender::new(0),
                launchable: launchable::LaunchableCatalogue::new(),
                workspace_opens: workspace_open::WorkspaceOpens::default(),
                window_instances: Mutex::new(HashMap::new()),
                action_directory_bridge: ActionDirectoryRequestQueue {
                    sender: bridge_tx,
                    turn: std::sync::Arc::clone(&local_turn),
                },
                peer_action_queue,
                peer_action_rx: Mutex::new(Some(peer_action_rx)),
                action_directory_roots: Mutex::new(HashMap::new()),
                native_checkouts: Mutex::new(HashMap::new()),
                canonical_directories: Mutex::new(canonical_route::CanonicalDirectories::default()),
                action_directory_root_snapshot: Mutex::new(ActionDirectoryRootSnapshot::default()),
                invocation_subscribers: Mutex::new(HashMap::new()),
                action_catalogue_subscribers: Mutex::new(HashMap::new()),
                pending_invocations: Mutex::new(HashMap::new()),
                private_drafts: Mutex::new(HashMap::default()),
                approved_check_jobs: Mutex::default(),
                external_moderation_jobs: Mutex::default(),
                remote_invocations: Mutex::new(tilo_actions::remote::InvocationLedger::default()),
                ended_runs: crate::task_runs::EndedRuns::default(),
                app_action_access: RwLock::new(
                    services
                        .app_action_access
                        .clone()
                        .unwrap_or_else(crate::app_actions::nothing_declared),
                ),
                app_action_consent: RwLock::new(None),
                run_question_notices: Mutex::new(HashMap::new()),
                app_action_consent_pending: Mutex::new(std::collections::HashSet::new()),
                step_prompts: crate::automation::StepPrompts::default(),
                invocation_generation: tokio::sync::watch::Sender::new(0),
                execution_activity: ActivityBus::default(),
                admission_lock: Mutex::new(()),
                journal: Mutex::new(journal),
                committing_invocations: Mutex::new(HashSet::new()),
                local_report_receipts: Mutex::default(),
                stable_invocation_subscribers: Mutex::new(std::collections::HashSet::new()),
                workspace_membership_peers: Mutex::new(HashMap::new()),
                completed_invocations: Mutex::new(HashMap::new()),
                live_views: live_views::LiveViews::default(),
                checkpoint_jobs: Mutex::new(HashMap::new()),
                untrusted_jobs: Mutex::default(),
                confirmation_grants: Mutex::new(HashMap::new()),
                witnessed_confirmations: Mutex::new(HashMap::new()),
                cancellation_requests: Mutex::new(std::collections::HashSet::new()),
                task_subscribers: Mutex::new(HashMap::new()),
                pending_task_runs: Mutex::new(HashMap::new()),
                invocation_id: AtomicU64::new(next_invocation),
                services,
                workers: Mutex::new(Vec::new()),
                worker_admitted: tokio::sync::Notify::new(),
                local_turn,
                offloads: Mutex::default(),
                offloaded_here: Mutex::default(),
                host_workers: Mutex::default(),
                route_holders: Mutex::default(),
                service_peers: Mutex::new(Vec::new()),
                thread_restarts: AtomicU64::new(0),
                #[cfg(test)]
                thread_callbacks: Mutex::new(Vec::new()),
            }),
            bridge_rx,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_broker_with_idle_directory_queue() -> crate::collab_broker::AppActionBroker {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = Self::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        crate::collab_broker::AppActionBroker::from_state_for_test(state)
    }

    /// A broker over a host's own workspace authority and services, with no
    /// transport, for host integration tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_broker_with_authority(
        membership: WorkspaceTreeHandle,
        services: HostServices,
    ) -> crate::collab_broker::AppActionBroker {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = Self::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership),
            None,
            None,
            services,
        );
        crate::collab_broker::AppActionBroker::from_state_for_test(state)
    }

    /// A transport-free fixture with an explicit admission turn. Certified
    /// invocations enqueue inert custody, just as they do on the live broker.
    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::expect_used,
        clippy::panic,
        clippy::missing_panics_doc,
        reason = "a test-support fixture: a turn with nothing to service fails the test that asked for it"
    )]
    pub fn test_broker_with_authority_and_invoke_turn(
        membership: WorkspaceTreeHandle,
        services: HostServices,
    ) -> (crate::collab_broker::AppActionBroker, impl Fn()) {
        let (state, bridge_rx) = Self::new_configured(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership),
            None,
            None,
            services,
        );
        let broker = crate::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&state));
        let turn = move || {
            let first =
                tilo_test_env::recv("the certified invocation at the broker turn", &bridge_rx);
            for request in std::iter::once(first).chain(bridge_rx.try_iter()) {
                let ActionDirectoryBridgeRequest::InvokeEager(invoke) = request else {
                    panic!("the transport-free fixture only services eager invocations");
                };
                if !invoke.reply.is_abandoned() {
                    action_host::answer_with(
                        state.invoke_eager(
                            &invoke.caller,
                            invoke.own_peer,
                            invoke.launch.as_ref(),
                            Some(&invoke.origin),
                            &invoke.request,
                        ),
                        invoke.reply,
                    );
                }
            }
        };
        (broker, turn)
    }

    /// Retain a terminal receipt as if an application had reported it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn record_completion_for_test(
        &self,
        id: u64,
        actor: &str,
        update: tilo_actions::InvocationUpdate,
    ) {
        self.record_completion(
            id,
            InvocationOwner::Local {
                actor: actor.to_owned(),
            },
            update,
        );
    }

    /// Stand in for an application's invocation stream on `peer_id`.
    /// The fixture receives the full envelope, including its stable invocation UUID.
    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_invocations_for_test(
        &self,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<InvocationEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.invocation_subscribers.lock() {
            subscribers.insert(peer_id, tx);
        }
        if let Ok(mut stable) = self.stable_invocation_subscribers.lock() {
            stable.insert(peer_id);
        }
        self.invocation_streams_changed();
        rx
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_action_directory_roots_for_test(&self, roots: Vec<ActionDirectoryRoot>) {
        if let Ok(mut registered) = self.action_directory_roots.lock() {
            for peer in registered
                .keys()
                .copied()
                .chain(roots.iter().map(|root| root.peer_id))
            {
                self.advance_witness_publication(peer, WitnessPublicationKind::Directory);
            }
            registered.clear();
            registered.extend(roots.into_iter().map(|root| (root.peer_id, root)));
        }
        self.refresh_action_directory_root_snapshot();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_published_surfaces_for_test(&self, surfaces: Vec<PublishedSurface>) {
        if let Ok(mut published) = self.surfaces.lock() {
            for peer in published
                .keys()
                .copied()
                .chain(surfaces.iter().map(|surface| surface.peer_id))
            {
                self.advance_witness_publication(peer, WitnessPublicationKind::Surface);
            }
            published.clear();
            published.extend(
                surfaces
                    .into_iter()
                    .map(|surface| (surface.peer_id, surface)),
            );
            self.surfaces_changed();
        }
    }

    /// Snapshot every action surface currently published by a connected app.
    pub fn published_surfaces(&self) -> Vec<PublishedSurface> {
        let live = self
            .surfaces
            .lock()
            .map(|surfaces| surfaces.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let live_keys = live
            .iter()
            .map(|surface| {
                (
                    surface.app_id.clone(),
                    surface.route.cluster_id.clone(),
                    surface.route.record_id.clone(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        let mut result = live;
        result.extend(self.launchable.surfaces().into_iter().filter(|surface| {
            !live_keys.contains(&(
                surface.app_id.clone(),
                surface.route.cluster_id.clone(),
                surface.route.record_id.clone(),
            ))
        }));
        result
    }

    /// Resolve against the current selected source, independently of the
    /// asynchronous search snapshot. A live source shadows its retained
    /// launchable catalogue even when it no longer offers the selected action.
    pub fn published_action(
        &self,
        route: &CrdtRoute,
        resource: &str,
        action: &str,
    ) -> Option<(PublishedSurface, PublishedAction)> {
        let surfaces = self.surfaces.lock().ok()?;
        let mut live_route = false;
        for surface in surfaces
            .values()
            .filter(|surface| surface.app_id == route.app_id && surface.route == *route)
        {
            live_route = true;
            if let Some(selected) = surface.action_projection(resource, action) {
                return Some(selected);
            }
        }
        let selected = if live_route {
            None
        } else {
            self.launchable.action_for(route, resource, action)
        };
        // Keep live-source shadowing coherent with the archive lookup; a
        // publisher must not install a live replacement between those reads.
        drop(surfaces);
        selected
    }

    /// Check one live action route without cloning the published catalogue.
    pub fn has_live_published_action(
        &self,
        app_id: &str,
        cluster_id: &str,
        record_id: &str,
        resource: &str,
        action: &str,
    ) -> bool {
        self.surfaces.lock().is_ok_and(|surfaces| {
            surfaces.values().any(|surface| {
                surface.live
                    && surface.app_id == app_id
                    && surface.route.cluster_id == cluster_id
                    && surface.route.record_id == record_id
                    && surface.actions.iter().any(|descriptor| {
                        descriptor.resource == resource && descriptor.action == action
                    })
            })
        })
    }

    fn published_surfaces_for_scope(
        &self,
        scope: Option<&tilo_delivery_protocol::peer_actions::ExactActionRoute>,
    ) -> Vec<PublishedSurface> {
        let Some(scope) = scope else {
            return self.published_surfaces();
        };
        self.surfaces
            .lock()
            .map(|surfaces| {
                surfaces
                    .values()
                    .filter(|surface| {
                        surface.app_id == scope.app_id && surface.route.record_id == scope.record_id
                    })
                    .map(|surface| PublishedSurface {
                        peer_id: surface.peer_id,
                        app_id: surface.app_id.clone(),
                        route: surface.route.clone(),
                        actions: surface
                            .actions
                            .iter()
                            .filter(|action| {
                                action.resource == scope.resource && action.action == scope.action
                            })
                            .cloned()
                            .collect(),
                        live: surface.live,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Generation of the eager action-surface source. Reading this never
    /// clones the potentially large application action inventory.
    #[must_use]
    pub fn published_surfaces_generation(&self) -> u64 {
        *self.surface_changes.borrow()
    }

    /// Allocate a fresh incarnation even for an identical republication.
    /// The allocator survives disconnect, so reusing a peer id cannot revive
    /// a previously issued witness.
    fn advance_witness_publication(&self, peer: u64, kind: WitnessPublicationKind) {
        let mut publications = self
            .witness_publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let incarnation = self
            .next_witness_publication
            .fetch_add(1, Ordering::Relaxed);
        let publication = publications.entry(peer).or_default();
        match kind {
            WitnessPublicationKind::Surface => publication.0 = incarnation,
            WitnessPublicationKind::Directory => publication.1 = incarnation,
        }
        drop(publications);
    }

    /// Record that a published surface changed: the generation moves and
    /// every launch waiting for its app to publish wakes.
    fn surfaces_changed(&self) {
        self.surface_changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Wake every launch waiting for its app's invocation stream after one
    /// opened or closed. Retained projections do not depend on streams, so
    /// the generation stays.
    fn invocation_streams_changed(&self) {
        self.surface_changes.send_modify(|_| {});
    }

    /// A receiver that sees every later surface or invocation-stream change;
    /// what exists so far counts as seen, so check readiness after taking it.
    pub(crate) fn publisher_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.surface_changes.subscribe()
    }

    /// Clone the nonblocking bridge used by shell search.  Sending only
    /// enqueues inert values; the broker `LocalSet` rechecks authority when
    /// it later services the request.
    #[must_use]
    pub fn action_directory_request_queue(&self) -> ActionDirectoryRequestQueue {
        self.action_directory_bridge.clone()
    }

    fn refresh_action_directory_root_snapshot(&self) {
        let mut roots = self
            .action_directory_roots
            .lock()
            .map(|roots| roots.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        roots.sort_by_key(|root| root.peer_id);
        if let Ok(mut snapshot) = self.action_directory_root_snapshot.lock() {
            snapshot.roots = roots;
            snapshot.generation = snapshot.generation.wrapping_add(1);
        }
    }

    /// Return one bounded page from the retained directory-root projection.
    /// This never clones or sorts the live registry on a palette query.
    #[must_use]
    pub fn action_directory_root_page(
        &self,
        after_peer: Option<u64>,
        max_results: usize,
    ) -> ActionDirectoryRootPage {
        let limit = max_results.min(Self::DIRECTORY_MAX_RESULTS as usize);
        let Ok(snapshot) = self.action_directory_root_snapshot.lock() else {
            return ActionDirectoryRootPage::default();
        };
        let start = after_peer.map_or(0, |peer| {
            snapshot.roots.partition_point(|root| root.peer_id <= peer)
        });
        let end = start.saturating_add(limit).min(snapshot.roots.len());
        let roots = snapshot.roots.get(start..end).unwrap_or_default().to_vec();
        let next_after_peer = (end < snapshot.roots.len())
            .then(|| {
                snapshot
                    .roots
                    .get(end.saturating_sub(1))
                    .map(|root| root.peer_id)
            })
            .flatten();
        ActionDirectoryRootPage {
            roots,
            next_after_peer,
            generation: snapshot.generation,
        }
    }

    /// Compatibility convenience for consumers that need just the first
    /// bounded page. New consumers should retain `next_after_peer` when
    /// presenting deeper root navigation.
    #[must_use]
    pub fn action_directory_roots(&self) -> Vec<ActionDirectoryRoot> {
        self.action_directory_root_page(None, Self::DIRECTORY_MAX_RESULTS as usize)
            .roots
    }

    /// Drain a bounded batch on the broker `LocalSet`.  This function must only
    /// be called by the accept-loop `LocalSet`: it starts the Rc-backed RPCs
    /// there, while the request sender is safe for compositor threads.
    #[allow(
        clippy::too_many_lines,
        reason = "the bounded bridge deliberately keeps request validation, LocalSet dispatch, and reply delivery in one ordered drain turn"
    )]
    fn drain_action_directory_bridge(
        self: &Arc<Self>,
        directories: &Rc<LocalActionDirectoryRegistry>,
    ) {
        const MAX_DRAIN_PER_TICK: usize = ActionDirectoryRequestQueue::CAPACITY;
        for _ in 0..MAX_DRAIN_PER_TICK {
            let request = match directories.bridge_rx.borrow().try_recv() {
                Ok(request) => request,
                Err(
                    std::sync::mpsc::TryRecvError::Empty
                    | std::sync::mpsc::TryRecvError::Disconnected,
                ) => break,
            };
            let state = Arc::clone(self);
            let directories = Rc::clone(directories);
            tokio::task::spawn_local(async move {
                match request {
                    ActionDirectoryBridgeRequest::InvokeEager(invoke) => {
                        let EagerActionInvokeRequest {
                            caller,
                            own_peer,
                            launch,
                            origin,
                            request,
                            reply,
                        } = *invoke;
                        if reply.is_abandoned() {
                            return;
                        }
                        // Enter admission directly, never the routing method that
                        // would enqueue again. Dispatch checks the original custody.
                        action_host::answer_with(
                            state.invoke_eager(
                                &caller,
                                own_peer,
                                launch.as_ref(),
                                Some(&origin),
                                &request,
                            ),
                            reply,
                        );
                    }
                    ActionDirectoryBridgeRequest::Query {
                        actor,
                        cluster_id,
                        query,
                        mut reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = reply
                            .awaited(state.query_action_directory_for_actor(
                                &directories,
                                &actor,
                                &cluster_id,
                                &query,
                            ))
                            .await
                            .and_then(|result| {
                                result
                                    .map(ActionDirectoryBridgeResponse::Page)
                                    .map_err(|_| ActionDirectoryBridgeError::Unavailable)
                            });
                        reply.send(response);
                    }
                    ActionDirectoryBridgeRequest::Resolve {
                        actor,
                        cluster_id,
                        peer_id,
                        revision,
                        resource,
                        action,
                        mut reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = reply
                            .awaited(state.resolve_action_directory_for_actor(
                                &directories,
                                &actor,
                                &cluster_id,
                                peer_id,
                                &revision,
                                &resource,
                                &action,
                            ))
                            .await
                            .and_then(|result| {
                                result
                                    .map(ActionDirectoryBridgeResponse::Resolution)
                                    .map_err(|_| ActionDirectoryBridgeError::Unavailable)
                            });
                        reply.send(response);
                    }
                    ActionDirectoryBridgeRequest::Start {
                        actor,
                        target,
                        argument,
                        confirmation,
                        mut reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = reply
                            .awaited(state.start_action_directory_for_actor(
                                &directories,
                                &actor,
                                &target,
                                argument.as_deref(),
                                confirmation,
                            ))
                            .await
                            .and_then(|result| {
                                result.map(ActionDirectoryBridgeResponse::StartedDirectoryAction)
                            });
                        reply.send(response);
                    }
                }
            });
        }
    }

    /// Shell-local counterpart to the wire `queryActionDirectory` method.
    /// It forwards one selected live directory only, preserving the same
    /// broker budgets and current permission checks without synthesising an
    /// eager cross-app catalogue for every shell keystroke.
    #[allow(
        clippy::future_not_send,
        reason = "the registered Cap'n Proto capability is Rc-backed and is awaited on the shell LocalSet"
    )]
    async fn query_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        query: &runtime_collab::ActionDirectoryQuery,
    ) -> Result<runtime_collab::ActionDirectoryPage, String> {
        if query.peer_id == 0
            || query.max_results == 0
            || query.max_bytes == 0
            || query.max_work == 0
        {
            return Err("directory query requires one peer and non-zero budgets".into());
        }
        validate_directory_query_input(query).map_err(|error| error.to_string())?;
        let registered = directories
            .registered(query.peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let max_results = query.max_results.min(Self::DIRECTORY_MAX_RESULTS);
        let max_bytes = query.max_bytes.min(Self::DIRECTORY_MAX_BYTES);
        let max_work = query.max_work.min(Self::DIRECTORY_MAX_WORK);
        let max_depth = query.max_depth.min(Self::DIRECTORY_MAX_DEPTH);
        let cursor = registered
            .pagination
            .source_cursor(
                actor,
                &query.revision,
                &query.needle,
                &query.parent_resource,
                &query.cursor,
            )
            .map_err(|error| error.to_string())?;
        let mut request = registered.directory.query_request();
        {
            let mut wire = request.get().init_query();
            wire.set_peer_id(query.peer_id);
            wire.set_revision(&query.revision);
            wire.set_cursor(&cursor);
            wire.set_needle(&query.needle);
            wire.set_parent_resource(&query.parent_resource);
            wire.set_max_results(max_results);
            wire.set_max_bytes(max_bytes);
            wire.set_max_work(max_work);
            wire.set_max_depth(max_depth);
            wire.set_resource_name(&query.resource_name);
        }
        let reply = request
            .send()
            .promise
            .await
            .map_err(|error| error.to_string())?;
        let page = reply
            .get()
            .and_then(
                runtime_collab::collab_capnp::action_directory::query_results::Reader::get_page,
            )
            .map_err(|error| error.to_string())?;
        if page
            .total_size()
            .map_err(|error| error.to_string())?
            .word_count
            .saturating_mul(8)
            > u64::from(max_bytes)
        {
            return Err("application exceeded directory byte budget".into());
        }
        let page = runtime_collab::decode_action_directory_page(page)
            .map_err(|error| error.to_string())?;
        let mut page = filter_directory_page(
            self,
            cluster_id,
            actor,
            &registered.app_id,
            registered.publishing_person.as_deref(),
            page,
            max_results,
        )
        .map_err(|error| error.to_string())?;
        if !query.resource_name.is_empty() {
            // A name lookup is answered whole; no cursor continues it.
            page.next_cursor.clear();
        }
        registered
            .pagination
            .shield(actor, &query.needle, &query.parent_resource, &mut page);
        Ok(page)
    }

    /// Shell-local exact re-resolution. A `Found` row is policy-checked at
    /// this instant; callers must never execute from an earlier page cache.
    #[allow(
        clippy::future_not_send,
        reason = "the registered Cap'n Proto capability is Rc-backed and is awaited on the shell LocalSet"
    )]
    async fn resolve_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<runtime_collab::ActionDirectoryResolution, String> {
        validate_directory_target_input(cluster_id, revision, resource, action, None, None)
            .map_err(|error| error.to_string())?;
        let registered = directories
            .registered(peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let mut request = registered.directory.resolve_request();
        {
            let mut wire = request.get();
            wire.set_revision(revision);
            wire.set_resource(resource);
            wire.set_action(action);
        }
        let reply = request
            .send()
            .promise
            .await
            .map_err(|error| error.to_string())?;
        let resolution = reply
            .get()
            .and_then(runtime_collab::collab_capnp::action_directory::resolve_results::Reader::get_resolution)
            .map_err(|error| error.to_string())?;
        if resolution
            .total_size()
            .map_err(|error| error.to_string())?
            .word_count
            .saturating_mul(8)
            > u64::from(Self::DIRECTORY_MAX_BYTES)
        {
            return Err("application exceeded directory resolution byte budget".into());
        }
        let decoded = runtime_collab::decode_action_directory_resolution(resolution)
            .map_err(|error| error.to_string())?;
        match decoded {
            runtime_collab::ActionDirectoryResolution::Found {
                row,
                revision: resolved,
            } if row.resource == resource
                && row.action.as_deref() == Some(action)
                && permits_directory_row(
                    self,
                    cluster_id,
                    actor,
                    &registered.app_id,
                    registered.publishing_person.as_deref(),
                    &row,
                )
                .map_err(|error| error.to_string())? =>
            {
                // A named revision is the one resolved; only an empty one
                // takes the owner's word for which revision it read, and
                // only in the shape a caller could have named itself.
                let resolved = if revision.is_empty() {
                    resolved.filter(|resolved| {
                        validate_directory_identifier(
                            resolved,
                            "resolved revision",
                            DIRECTORY_MAX_QUERY_REVISION_BYTES,
                            true,
                        )
                        .is_ok()
                    })
                } else {
                    Some(revision.to_owned())
                };
                Ok(runtime_collab::ActionDirectoryResolution::Found {
                    row,
                    revision: resolved,
                })
            }
            runtime_collab::ActionDirectoryResolution::Found { .. } => {
                Ok(runtime_collab::ActionDirectoryResolution::NotFound)
            }
            other => Ok(other),
        }
    }

    /// Resolve only a retained pre-input snapshot; never used for delivery.
    #[expect(
        clippy::future_not_send,
        reason = "directory capabilities are Rc-backed on the broker LocalSet"
    )]
    async fn resolve_local_document_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        peer_id: u64,
        target: (&str, &str, &str),
    ) -> Result<runtime_collab::ActionDirectoryResolution, String> {
        let (revision, resource, action) = target;
        validate_directory_target_input(cluster_id, revision, resource, action, None, None)
            .map_err(|error| error.to_string())?;
        if revision.is_empty() {
            return Err("local admission requires an exact retained revision".into());
        }
        let registered = directories
            .registered(peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let decoded = runtime_collab::resolve_local_document_action(
            &registered.directory,
            revision,
            resource,
            action,
            u64::from(Self::DIRECTORY_MAX_BYTES),
        )
        .await
        .map_err(|error| error.to_string())?;
        match decoded {
            runtime_collab::ActionDirectoryResolution::Found {
                row,
                revision: resolved,
            } if resolved.as_deref() == Some(revision)
                && row.resource == resource
                && row.action.as_deref() == Some(action)
                && permits_directory_row(
                    self,
                    cluster_id,
                    actor,
                    &registered.app_id,
                    registered.publishing_person.as_deref(),
                    &row,
                )
                .map_err(|error| error.to_string())? =>
            {
                Ok(runtime_collab::ActionDirectoryResolution::Found {
                    row,
                    revision: resolved,
                })
            }
            runtime_collab::ActionDirectoryResolution::Found { .. } => {
                Ok(runtime_collab::ActionDirectoryResolution::NotFound)
            }
            other => Ok(other),
        }
    }

    /// Atomically resolve and start a directory action on the broker
    /// `LocalSet`.  No cached descriptor, eager surface entry, or compositor
    /// thread participates in admission.
    #[allow(
        clippy::future_not_send,
        reason = "Admission needs the complete authenticated route and runs on the broker LocalSet with non-Send Capnp state."
    )]
    async fn start_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        target: &DirectoryActionTarget,
        argument: Option<&str>,
        confirmation: Option<DirectoryActionConfirmation>,
    ) -> Result<StartedDirectoryAction, ActionDirectoryBridgeError> {
        let unavailable = |_: String| ActionDirectoryBridgeError::Unavailable;
        let DirectoryActionTarget {
            cluster_id,
            peer_id,
            revision,
            address: tilo_actions::ActionAddress { resource, action },
        } = target;
        let peer_id = *peer_id;
        if revision.is_empty() {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        validate_directory_target_input(
            cluster_id,
            revision,
            resource,
            action,
            argument,
            confirmation.as_ref(),
        )
        .map_err(|_| ActionDirectoryBridgeError::InvalidRequest)?;
        let registered = directories
            .registered(peer_id, cluster_id)
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        let resolved = self
            .resolve_action_directory_for_actor(
                directories,
                actor,
                cluster_id,
                peer_id,
                revision,
                resource,
                action,
            )
            .await
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        let runtime_collab::ActionDirectoryResolution::Found { row, .. } = resolved else {
            return Err(match resolved {
                runtime_collab::ActionDirectoryResolution::StaleRevision => {
                    ActionDirectoryBridgeError::StaleRevision
                }
                _ => ActionDirectoryBridgeError::NotFound,
            });
        };
        let published = published_directory_action(&row)
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        // Resolve rechecked the current capability and actor.  Re-run the
        // validation immediately before constructing the invocation surface
        // so a future local caller cannot bypass the same boundary.
        if !permits_directory_row(
            self,
            cluster_id,
            actor,
            &registered.app_id,
            registered.publishing_person.as_deref(),
            &row,
        )
        .map_err(|_| ActionDirectoryBridgeError::Unavailable)?
        {
            return Err(ActionDirectoryBridgeError::NotFound);
        }
        let confirmation_token = self.directory_confirmation_token(
            peer_id,
            resource,
            action,
            argument,
            actor,
            &published.contract,
            confirmation,
        )?;
        let surface = PublishedSurface {
            peer_id,
            app_id: registered.app_id,
            route: registered.route,
            actions: vec![published.clone()],
            live: true,
        };
        let job = self
            .start_invocation_from_offered(
                peer_id,
                (surface, published),
                InvocationInputs {
                    original_request: None,
                    requested: None,
                    caller: None,
                    execution: None,
                    argument,
                    actor,
                    confirmation_token: confirmation_token.as_deref(),
                    context: None,
                },
                Some(revision.as_str()),
            )
            .map_err(unavailable)?;
        let job = job.start(self).await.map_err(unavailable)?;
        let initial_update = self
            .invocation_status(job.id)
            .ok_or(ActionDirectoryBridgeError::Unavailable)?;
        Ok(StartedDirectoryAction {
            invocation_id: job.id,
            initial_update,
        })
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the owned confirmation is intentionally matched without retaining caller-owned approval material"
    )]
    fn directory_confirmation_token(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &BrokerActor,
        contract: &tilo_actions::ActionContract,
        confirmation: Option<DirectoryActionConfirmation>,
    ) -> Result<Option<String>, ActionDirectoryBridgeError> {
        // Only a person acting themselves confirms by their own word.
        let confirmation = confirmation.filter(|_| actor.direct());
        match &contract.confirmation {
            tilo_actions::ConfirmationPolicy::Confirm | tilo_actions::ConfirmationPolicy::None
                if matches!(confirmation, Some(DirectoryActionConfirmation::Confirmed)) =>
            {
                // Without a confirmation policy, a confirmation given anyway
                // answers the room's ask, if the grants ask; admission
                // decides whether they do.
            }
            tilo_actions::ConfirmationPolicy::None => return Ok(None),
            tilo_actions::ConfirmationPolicy::Phrase { phrase } if matches!(&confirmation, Some(DirectoryActionConfirmation::Phrase(value)) if value == phrase) =>
                {}
            _ => return Err(ActionDirectoryBridgeError::ConfirmationRequired),
        }
        let token = uuid::Uuid::new_v4().to_string();
        let mut grants = self
            .confirmation_grants
            .lock()
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        let now = std::time::Instant::now();
        grants.retain(|_, grant| grant.expires >= now);
        grants.insert(
            token.clone(),
            ConfirmationGrant {
                peer_id,
                resource: resource.to_string(),
                action: action.to_string(),
                argument_digest: argument_digest(argument),
                actor: actor.id.clone(),
                host_witnessed: None,
                expires: now
                    .checked_add(std::time::Duration::from_secs(60))
                    .ok_or(ActionDirectoryBridgeError::Unavailable)?,
            },
        );
        // Token ownership is now in the registry; do not retain its mutex
        // while the caller begins the invocation it authorizes.
        drop(grants);
        Ok(Some(token))
    }

    /// Decide validated directory rows against the current workspace
    /// authority, not the authority that existed when a cursor was issued.
    /// Every row of one page is decided on one committed sample, whatever
    /// the page's length; the borrow ends before the reply is serialized.
    fn permits_directory_actions(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
        app_id: &str,
        questions: &[DirectoryQuestion<'_>],
    ) -> Result<Vec<Answer>, capnp::Error> {
        let now = self.policy_now_ns().map_err(capnp::Error::failed)? / 1_000_000_000;
        let membership = self.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("the action broker has no workspace authority".into())
        })?;
        membership
            .decide_in_room(cluster_id, |workspace| {
                let workspace = workspace
                    .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
                Ok::<Vec<Answer>, String>(
                    questions
                        .iter()
                        .map(|question| {
                            let node = tilo_actions::action_node(
                                &workspace.node(),
                                app_id,
                                question.resource_kind,
                                question.resource,
                                question.field,
                                question.action,
                            );
                            actor.answer_in(workspace, &node, question.kind, now)
                        })
                        .collect(),
                )
            })
            .and_then(std::convert::identity)
            .map_err(capnp::Error::failed)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "catalogue projection applies one ordered authorization and serialization pass"
    )]
    #[cfg(test)]
    fn action_catalogue_json(&self, cluster_id: &str, actor: &str) -> Result<String, String> {
        self.action_catalogue_json_for(cluster_id, &BrokerActor::person(actor), None)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "catalogue serialization and policy filtering remain adjacent so a published action cannot bypass the actor check"
    )]
    fn action_catalogue_json_for(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
        scope: Option<&tilo_delivery_protocol::peer_actions::ExactActionRoute>,
    ) -> Result<String, String> {
        let now = self.policy_now_ns()? / 1_000_000_000;
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        // Ordinary directed invocation needs a live publisher. Only native
        // task custody can activate an inert retained route; its catalogue
        // keeps stable providers discoverable without inventing a publisher.
        let live = self
            .published_surfaces_for_scope(scope)
            .into_iter()
            .filter(|surface| surface.live)
            .collect::<Vec<_>>();
        let routes = live
            .iter()
            .map(|surface| surface.route.clone())
            .collect::<Vec<_>>();
        let retained = self.launchable.surfaces().into_iter().filter(|surface| {
            scope.is_none()
                && self.task_admitted_room(cluster_id)
                && !routes.contains(&surface.route)
        });
        let published = live
            .into_iter()
            .chain(retained)
            .filter(|surface| surface.route.cluster_id == cluster_id)
            .map(|mut surface| {
                surface.actions.retain(|action| {
                    scope.is_none_or(|scope| {
                        action.resource == scope.resource && action.action == scope.action
                    })
                });
                surface
            })
            .collect::<Vec<_>>();
        let live_apps = self
            .surfaces
            .lock()
            .map(|surfaces| {
                surfaces
                    .values()
                    .filter(|surface| surface.live && surface.route.cluster_id == cluster_id)
                    .map(|surface| surface.app_id.clone())
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();
        let applications = self
            .services
            .launcher
            .as_ref()
            .map(|service| service.applications())
            .unwrap_or_default()
            .into_iter()
            .filter(|app| {
                scope.is_none_or(|scope| {
                    app.package_id == scope.app_id
                        && scope.record_id.is_empty()
                        && scope.resource == format!("app.{}", app.package_id)
                        && scope.action == "open"
                })
            })
            .filter(|app| !live_apps.contains(&app.package_id))
            .collect::<Vec<_>>();
        // One policy sample decides every row; serialization runs after the
        // committed authority is released.
        let (published, applications) = membership.decide_in_room(cluster_id, |workspace| {
            let workspace = workspace
                .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
            let published = published
                .into_iter()
                .map(|mut surface| {
                    if !surface.live
                        && !actor.may_in(
                            workspace,
                            &app_node(workspace, &surface.app_id),
                            ActionKind::Undoable,
                            now,
                        )
                    {
                        surface.actions.clear();
                    }
                    // Listed when the actor may take it, or would be asked
                    // first.
                    surface.actions.retain(|action| {
                        actor.action_answer_in(
                            workspace,
                            &surface.app_id,
                            (&action.resource_kind, &action.resource, &action.action),
                            &action.contract.need(),
                            now,
                        ) != Answer::Deny
                    });
                    surface
                })
                .collect::<Vec<_>>();
            let applications = applications
                .into_iter()
                .filter(|app| {
                    actor.may_in(
                        workspace,
                        &app_node(workspace, &app.package_id),
                        ActionKind::Undoable,
                        now,
                    )
                })
                .collect::<Vec<_>>();
            Ok::<_, String>((published, applications))
        })??;
        let mut surfaces = published
            .into_iter()
            .filter_map(|surface| {
                let provider = (!surface.live).then(|| launchable::launchable_provider(&surface));
                (!surface.actions.is_empty()).then(|| {
                    serde_json::json!({
                        "peer_id": surface.peer_id,
                        "provider": provider,
                        "app_id": surface.app_id,
                        "cluster_id": surface.route.cluster_id,
                        "record_id": surface.route.record_id,
                        "live": surface.live,
                        "actions": surface.actions,
                    })
                })
            })
            .collect::<Vec<_>>();
        for app in applications {
            let package_id = app.package_id;
            let display_name = app.display_name;
            surfaces.push(serde_json::json!({
                "peer_id": 0,
                "provider": tilo_actions::remote::workspace_provider(cluster_id),
                "app_id": package_id,
                "cluster_id": cluster_id,
                "record_id": "",
                "live": true,
                "actions": [PublishedAction {
                    resource: format!("app.{package_id}"),
                    semantic_identity: None,
                    action: "open".into(),
                    resource_kind: "applications.app".into(),
                    action_label: format!("Open {display_name}"),
                    resource_label: display_name,
                    description: Some(
                        "Launch this application into the current workspace".into(),
                    ),
                    operation_kind: tilo_actions::OperationKind::Effect,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract::default(),
                    view: None,
                    input: PublishedInput::None,
                }],
            }));
        }
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "surfaces": surfaces,
        }))
        .map_err(|error| error.to_string())
    }

    /// Invalidate retained discovery only after a real service-surface mutation.
    fn publish_changed_action_catalogue(&self) {
        self.surfaces_changed();
        self.publish_action_catalogue();
    }

    fn publish_action_catalogue(&self) {
        let subscribers = self
            .action_catalogue_subscribers
            .lock()
            .map(|subscribers| {
                subscribers
                    .iter()
                    .map(|(peer, (cluster, actor, tx))| {
                        (*peer, cluster.clone(), actor.clone(), tx.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut audiences: HashMap<(String, BrokerActor), ActionCatalogueSnapshot> = HashMap::new();
        let mut disconnected = Vec::new();
        for (peer, cluster, actor, tx) in subscribers {
            let snapshot = audiences
                .entry((cluster.clone(), actor.clone()))
                .or_insert_with(|| {
                    self.action_catalogue_json_for(&cluster, &actor, None)
                        .ok()
                        .map(Arc::<str>::from)
                })
                .clone();
            if let Some(snapshot) = snapshot {
                if tx.send(Some(snapshot)).is_err() {
                    disconnected.push(peer);
                }
            } else {
                // Invalidate an unread prior snapshot before removing this
                // audience. A failed current authorization must never let
                // a slow reader consume stale discovery state.
                tx.send_replace(None);
                disconnected.push(peer);
            }
        }
        if !disconnected.is_empty()
            && let Ok(mut subscribers) = self.action_catalogue_subscribers.lock()
        {
            for peer in disconnected {
                subscribers.remove(&peer);
            }
        }
    }

    /// Address one operation back to the exact connection that published it.
    /// The returned receiver resolves only after the application reports a
    /// structured completion; dropping the connection resolves as a refusal.
    ///
    /// # Errors
    ///
    /// Returns an error if the surface or action is unavailable or the
    /// invocation cannot be sent to its owning connection.
    pub fn invoke(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<std::sync::mpsc::Receiver<InvocationCompletion>, String> {
        self.start_invocation(peer_id, resource, action, argument, actor)
            .map(|job| job.completion)
    }

    /// Start an unconfirmed invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is unavailable or cannot be delivered.
    pub fn start_invocation(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<InvocationJob, String> {
        self.start_invocation_confirmed(peer_id, resource, action, argument, actor, None)
    }

    /// Prepare a one-use confirmation token bound to this exact invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the action, phrase, or confirmation registry is invalid.
    pub fn prepare_confirmation(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        phrase: Option<&str>,
    ) -> Result<String, String> {
        let offered =
            self.surfaces
                .lock()
                .ok()
                .and_then(|surfaces| surfaces.get(&peer_id).cloned())
                .and_then(|surface| {
                    surface.actions.into_iter().find(|candidate| {
                        candidate.resource == resource && candidate.action == action
                    })
                })
                .ok_or_else(|| "the application no longer publishes that action".to_string())?;
        match &offered.contract.confirmation {
            tilo_actions::ConfirmationPolicy::None => {
                return Err("the action does not require confirmation".into());
            }
            tilo_actions::ConfirmationPolicy::Confirm => {}
            tilo_actions::ConfirmationPolicy::Phrase { phrase: expected } => {
                if phrase != Some(expected.as_str()) {
                    return Err("the confirmation phrase did not match".into());
                }
            }
        }
        let token = uuid::Uuid::new_v4().to_string();
        let mut grants = self
            .confirmation_grants
            .lock()
            .map_err(|_| "the confirmation registry is unavailable".to_string())?;
        let now = std::time::Instant::now();
        grants.retain(|_, grant| grant.expires >= now);
        grants.insert(
            token.clone(),
            ConfirmationGrant {
                peer_id,
                resource: resource.to_string(),
                action: action.to_string(),
                argument_digest: argument_digest(argument),
                actor: actor.to_string(),
                host_witnessed: None,
                expires: std::time::Instant::now()
                    .checked_add(std::time::Duration::from_secs(60))
                    .ok_or_else(|| "confirmation expiry overflowed".to_string())?,
            },
        );
        drop(grants);
        Ok(token)
    }

    /// Start an invocation with optional broker confirmation evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when admission, authorization, confirmation, or delivery fails.
    ///
    /// Returns once the admission is durable, blocking the calling thread for
    /// that write: never call it on the broker's loop.
    pub fn start_invocation_confirmed(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        confirmation_token: Option<&str>,
    ) -> Result<InvocationJob, String> {
        self.start_invocation_confirmed_for(
            peer_id,
            resource,
            action,
            InvocationInputs {
                original_request: None,
                requested: None,
                caller: None,
                execution: None,
                argument,
                actor: &BrokerActor::person(actor),
                confirmation_token,
                context: None,
            },
        )?
        .start_blocking(self)
    }

    fn start_invocation_confirmed_for(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        inputs: InvocationInputs<'_>,
    ) -> Result<StagedJob, String> {
        self.start_invocation_confirmed_for_origin(peer_id, resource, action, inputs, None)
    }

    fn start_invocation_confirmed_for_origin(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        inputs: InvocationInputs<'_>,
        origin: Option<InvocationCaller>,
    ) -> Result<StagedJob, String> {
        let offered = self
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&peer_id).cloned())
            .and_then(|surface| {
                let action = surface
                    .actions
                    .iter()
                    .find(|candidate| candidate.resource == resource && candidate.action == action)
                    .cloned()?;
                Some((surface, action))
            })
            .ok_or_else(|| "the application no longer publishes that action".to_string())?;
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        self.dispatch_invocation_from_offered_owned(
            peer_id,
            offered,
            inputs,
            AdmissionProvenance {
                durable: None,
                revision: None,
                origin,
                retained_time: None,
            },
        )
    }

    /// Admit an invocation from a just-validated live action.  Directory
    /// actions reach this point without ever being copied into the eager
    /// published surface; the caller must have resolved the row on this
    /// `LocalSet` turn and supplied its certified route.
    fn start_invocation_from_offered(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        inputs: InvocationInputs<'_>,
        revision: Option<&str>,
    ) -> Result<StagedJob, String> {
        self.start_invocation_from_offered_origin(peer_id, offered, inputs, revision, None)
    }

    fn start_invocation_from_offered_origin(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        inputs: InvocationInputs<'_>,
        revision: Option<&str>,
        origin: Option<InvocationCaller>,
    ) -> Result<StagedJob, String> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        self.dispatch_invocation_from_offered_owned(
            peer_id,
            offered,
            inputs,
            AdmissionProvenance {
                durable: None,
                revision,
                origin,
                retained_time: None,
            },
        )
    }

    fn dispatch_invocation_from_offered(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        inputs: InvocationInputs<'_>,
        durable: Option<(&VerifiedActionCaller, &CallerInvocation)>,
        revision: Option<&str>,
    ) -> Result<StagedJob, String> {
        self.dispatch_invocation_from_offered_owned(
            peer_id,
            offered,
            inputs,
            AdmissionProvenance {
                durable: durable.map(|(caller, request)| DurableContext::Peer(caller, request)),
                revision,
                origin: None,
                retained_time: None,
            },
        )
    }

    fn dispatch_invocation_from_offered_owned(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        inputs: InvocationInputs<'_>,
        provenance: AdmissionProvenance<'_>,
    ) -> Result<StagedJob, String> {
        self.admit_invocation_from_offered_owned(peer_id, offered, inputs, provenance, None)
    }

    /// Shared semantic admission; a local mutation report is never delivered.
    #[expect(
        clippy::too_many_lines,
        reason = "a freshly offered live action and its durable identity are consumed into one admission while all checks remain atomic"
    )]
    fn admit_invocation_from_offered_owned(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        inputs: InvocationInputs<'_>,
        provenance: AdmissionProvenance<'_>,
        local_report: Option<&crate::local_document_action::Request>,
    ) -> Result<StagedJob, String> {
        if local_report.is_some()
            && (!matches!(
                offered.1.operation_kind,
                tilo_actions::OperationKind::Document | tilo_actions::OperationKind::View
            ) && offered.1.contract.kind != tilo_actions::ActionKind::Read
                || (offered.1.operation_kind != tilo_actions::OperationKind::Document
                    && local_report.is_some_and(|report| !report.mutation_hashes.is_empty()))
                || provenance.durable.is_some()
                || !matches!(
                    provenance.origin.as_ref(),
                    Some(InvocationCaller::Local { .. })
                ))
        {
            return Err(
                "local action report requires authenticated document or read-only admission".into(),
            );
        }
        if let Some(report) = local_report
            && (report.bound_invocation()? != report.invocation
                || inputs.requested != Some(tilo_actions::InvocationId(report.invocation)))
        {
            return Err("local report invocation does not bind capture".into());
        }
        let typed_local_arguments = local_report
            .map(|report| report.typed_arguments(&offered.1.params))
            .transpose()?;
        let InvocationInputs {
            original_request,
            requested,
            caller,
            execution,
            argument,
            actor,
            confirmation_token,
            context,
        } = inputs;
        let AdmissionProvenance {
            durable,
            revision,
            origin,
            retained_time,
        } = provenance;
        let continuing = ContinuingCaller::admitted(caller, durable, origin.as_ref());
        let owner = match durable {
            Some(context) => context.owner(),
            None => match &origin {
                Some(InvocationCaller::Certified { caller, .. }) => caller.owner(),
                Some(InvocationCaller::NativePerson { certificate, .. }) => {
                    InvocationOwner::AuthenticatedLocal {
                        root: certificate.user_public_key,
                        device: certificate.device_public_key,
                        agent: None,
                    }
                }
                Some(InvocationCaller::Local {
                    route,
                    admitted,
                    time,
                }) => {
                    self.checked_policy_time(time.as_ref())?;
                    let current = self
                        .revalidate_admission(route, admitted)
                        .map_err(|error| error.to_string())?;
                    let certificate = current
                        .certificate
                        .as_ref()
                        .ok_or("original caller certificate missing")?;
                    if route.cluster_id != offered.0.route.cluster_id {
                        return Err("original local caller does not match invocation room".into());
                    }
                    let expected = current.agent_id.as_ref().map_or_else(
                        || principal_id_for_root(certificate.user_public_key),
                        |agent| format!("agent:{}", agent.strip_prefix("agent:").unwrap_or(agent)),
                    );
                    if actor.id == expected && actor.is_agent == current.agent_id.is_some() {
                        InvocationOwner::AuthenticatedLocal {
                            root: certificate.user_public_key,
                            device: certificate.device_public_key,
                            agent: current.agent_id.clone(),
                        }
                    } else {
                        // The connection's custody attests only its own admitted
                        // Person or agent. An application acting under its own
                        // identity, or an effect continuing another caller's
                        // invocation, is decided as that actor and stays
                        // actor-owned: never attested as the connection's.
                        InvocationOwner::Local {
                            actor: actor.id.clone(),
                        }
                    }
                }
                None => InvocationOwner::Local {
                    actor: actor.id.clone(),
                },
            },
        };
        owner.validate()?;
        // In native rooms an agent's action authority comes only from the
        // authenticated task admission callback, never from ambient context
        // or a room grant alone (including after its run has stopped).
        if actor.is_agent && self.task_admitted_room(&offered.0.route.cluster_id) {
            match durable {
                Some(DurableContext::Task { binding, .. })
                    if actor.id.strip_prefix("agent:").unwrap_or(&actor.id) == binding.agent_id => {
                }
                _ => {
                    return Err(
                        "native task agent requires authenticated task action admission".into(),
                    );
                }
            }
        }
        // Never trust an envelope's invoker: the host sets it.
        let context = invoker::host_set_invoker(context, actor, caller, execution.as_ref())?;
        // A certified local origin retains its original registered clock and
        // exact person/room through an asynchronous directory lookup. Never
        // recapture a replacement origin at dispatch or accept a payload actor.
        let original_time = if let Some(InvocationCaller::Certified { caller, .. }) = &origin {
            let admitted = self.current_action_caller(caller)?;
            // The person the admission certifies, or the one agent it admitted
            // for them (an MCP session their device handed over): never
            // another actor.
            let admitted_actor = match &admitted.agent_id {
                Some(agent) => {
                    actor.is_agent
                        && actor.id.strip_prefix("agent:")
                            == Some(agent.strip_prefix("agent:").unwrap_or(agent))
                }
                None => !actor.is_agent && actor.id == admitted.principal_id,
            };
            if caller.room != offered.0.route.cluster_id || !admitted_actor {
                return Err("original caller does not match the invocation route or actor".into());
            }
            Some(
                caller
                    .policy_time
                    .as_ref()
                    .ok_or("original policy clock unavailable")?,
            )
        } else if let Some(InvocationCaller::NativePerson {
            room,
            certificate,
            time,
            ..
        }) = &origin
        {
            self.current_native_person(room, certificate)?;
            if room != &offered.0.route.cluster_id
                || actor.is_agent
                || actor.id != principal_id_for_root(certificate.user_public_key)
            {
                return Err(
                    "original native person does not match the invocation route or actor".into(),
                );
            }
            Some(time)
        } else {
            None
        };
        if original_time.is_some() && (durable.is_some() || retained_time.is_some()) {
            return Err("duplicate original invocation custody".into());
        }
        let local_time = if let Some(InvocationCaller::Local { time, .. }) = &origin {
            self.checked_policy_time(time.as_ref())?;
            time.as_ref()
        } else {
            None
        };
        let retained_time = original_time.or(retained_time);
        let policy_time = match (durable, retained_time) {
            (Some(context), None) => self.checked_policy_time(context.time())?.0,
            (None, Some(original)) => self.checked_policy_time(Some(original))?.0,
            (None, None) => match &origin {
                Some(InvocationCaller::Local { .. }) => local_time.cloned(),
                _ => self.registered_policy_time()?,
            },
            (Some(_), Some(_)) => return Err("duplicate original invocation time custody".into()),
        };
        let now = self.checked_policy_time(policy_time.as_ref())?.1 / 1_000_000_000;

        if offered.0.app_id == tilo_owner_commit::desktop_command::APP_ID
            && self
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?
                .values()
                .filter(|pending| pending.peer_id == peer_id)
                .count()
                >= 32
        {
            return Err("desktop invocation capacity exhausted".into());
        }
        let sender = self
            .invocation_subscribers
            .lock()
            .ok()
            .and_then(|subscribers| subscribers.get(&peer_id).cloned());
        if local_report.is_none() && sender.is_none() {
            return Err("the application has no invocation stream".into());
        }
        if offered.0.route.cluster_id.is_empty() {
            return Err("the application is not attached to a workspace".into());
        }
        if let tilo_actions::Availability::Unavailable { reason, .. } =
            &offered.1.contract.availability
        {
            return Err(format!("the application action is unavailable: {reason}"));
        }
        // Every check of this admission is decided on one committed sample,
        // so a policy change cannot land between the action check and the
        // record check.
        let (asks, spent) = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_room(&offered.0.route.cluster_id, |workspace| {
                let workspace = workspace.ok_or("workspace no longer hosted")?;
                let run = match durable {
                    Some(DurableContext::Task { binding, .. }) => Some(binding),
                    _ => None,
                };
                actor.admit_published_in(workspace, now, run, (&offered.0, &offered.1))
            })??;

        if (durable.is_some() || execution.is_some())
            && !self
                .stable_invocation_subscribers
                .lock()
                .map_err(|_| "application capability unavailable")?
                .contains(&peer_id)
        {
            return Err("application_stable_identity_unavailable".into());
        }
        // A configured but unreadable/corrupt journal disables every new admission.
        let mut journal_guard = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?;
        let journal = journal_guard.as_mut().map_err(|error| error.clone())?;
        if (durable.is_some() || owner.verified_actor().is_some()) && journal.is_none() {
            return Err("action_evidence_unavailable: admission journal unavailable".into());
        }
        // Where the room's grants ask first, the person's confirmation in
        // the host is the answer: the invocation needs a confirmation token
        // even when the action itself asks for none.
        if asks
            || requires_store_witness(&offered.0.app_id, &offered.1.action)
            || !matches!(
                offered.1.contract.confirmation,
                tilo_actions::ConfirmationPolicy::None
            )
        {
            let token = confirmation_token.ok_or_else(|| {
                "action_confirmation_required: request and present a one-use confirmation token"
                    .to_string()
            })?;
            let grant = self
                .confirmation_grants
                .lock()
                .map_err(|_| "the confirmation registry is unavailable".to_string())?
                .remove(token)
                .ok_or_else(|| "the confirmation token is unknown or already used".to_string())?;
            if (requires_store_witness(&offered.0.app_id, &offered.1.action)
                && grant.host_witnessed.is_none())
                || grant.expires < std::time::Instant::now()
                || grant.peer_id != peer_id
                || grant.resource != offered.1.resource
                || grant.action != offered.1.action
                || grant.argument_digest != argument_digest(argument)
                || grant.actor != actor.id
                || grant.host_witnessed.as_ref().is_some_and(|binding| {
                    binding.room != offered.0.route.cluster_id
                        || Some(binding.invocation)
                            != requested
                                .or_else(|| durable.map(DurableContext::invocation))
                                .or_else(|| execution.as_ref().map(ExecutionScope::invocation))
                                .or(match &origin {
                                    Some(InvocationCaller::NativePerson { invocation, .. }) => {
                                        Some(*invocation)
                                    }
                                    _ => None,
                                })
                        || binding.publication != self.witness_publication(&binding.provider)
                        || match original_request {
                            Some(request) => {
                                binding.provider != request.target.provider
                                    || binding.revision != request.revision
                            }
                            None => {
                                binding.provider != peer_id.to_string()
                                    || binding
                                        .revision
                                        .as_deref()
                                        .is_some_and(|held| Some(held) != revision)
                            }
                        }
                })
            {
                return Err(
                    "the confirmation token is expired or does not match this invocation".into(),
                );
            }
        }
        let id = self
            .invocation_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "invocation handle space exhausted")?;
        let authoritative = durable
            .map(DurableContext::invocation)
            .or_else(|| execution.as_ref().map(ExecutionScope::invocation))
            .or(match &origin {
                Some(InvocationCaller::NativePerson { invocation, .. }) => Some(*invocation),
                _ => None,
            });
        if requested.is_some_and(|requested| {
            requested.0.is_nil() || authoritative.is_some_and(|held| held != requested)
        }) {
            return Err("requested invocation does not match admitted custody".into());
        }
        let invocation = match durable {
            Some(context) => context.invocation(),
            None => requested.or(authoritative).unwrap_or_else(|| {
                tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)))
            }),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut receipt_context = tilo_actions::ActionContext::for_resource(
            &offered.1.resource,
            &offered.1.resource_kind,
            &offered.1.resource_label,
        )
        .with_app(&offered.0.app_id, &offered.0.app_id)
        .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(
                &offered.0.app_id,
                &offered.1.resource_kind,
                &offered.1.action,
            ),
            [],
        )));
        if let Some(ambient) = &context {
            receipt_context = receipt_context.enriched_by(ambient);
        }
        let mut admitted = invocation_journal::Durable::settled();
        let journal_key = if let Some(journal) = journal.as_mut() {
            let key = InvocationKey {
                room: offered.0.route.cluster_id.clone(),
                host: journal.host(),
                invocation,
                owner: owner.clone(),
            };
            let request = if offered.0.app_id == "so.tilo.desktop"
                && offered.1.resource == workspace_checkpoints::RESOURCE
            {
                let certificate = match origin.as_ref() {
                    Some(InvocationCaller::Certified { caller, .. }) => self
                        .current_action_caller(caller)?
                        .certificate
                        .ok_or("checkpoint caller certificate absent")?,
                    Some(InvocationCaller::NativePerson { certificate, .. }) => certificate.clone(),
                    _ => return Err("checkpoint requires original certified caller custody".into()),
                };
                let mut intent = workspace_checkpoints::intent(
                    &offered.0.route.cluster_id,
                    &certificate,
                    &InvocationEnvelope {
                        id,
                        invocation,
                        resource: offered.1.resource.clone(),
                        action: offered.1.action.clone(),
                        argument: argument.map(str::to_owned),
                        actor: actor.id.clone(),
                        context: None,
                    },
                )?;
                if let Some(request) = original_request {
                    intent
                        .as_object_mut()
                        .ok_or("checkpoint intent invalid")?
                        .insert(
                            "remote_request".into(),
                            workspace_checkpoints::remote_request(request)?,
                        );
                }
                intent
            } else {
                match durable {
                    Some(context) => context.intent(),
                    None => serde_json::json!({
                        "peer_id": peer_id, "app_id": offered.0.app_id, "record_id": offered.0.route.record_id,
                        "resource": offered.1.resource, "action": offered.1.action, "argument": argument,
                        "original_request": original_request,
                    }),
                }
            };
            let request = match local_report {
                Some(report) => serde_json::json!({
                    "kind": "local-document-action-report-v1",
                    "claim": "semantic-admission-and-app-reported-input-and-hashes-not-execution",
                    "resolved_request": {
                        "app_id": offered.0.app_id,
                        "record_id": offered.0.route.record_id,
                        "resource": offered.1.resource,
                        "action": offered.1.action,
                        "argument": argument,
                        "original_request": original_request,
                    },
                    "report": report,
                    "typed_arguments": typed_local_arguments,
                }),
                None => request,
            };
            let intent = serde_json::json!({ "request": request,
                "contract": offered.1.contract, "operation_kind": offered.1.operation_kind,
                "context": receipt_context, "actor": actor.id, "app_id": offered.0.app_id,
                "record_id": offered.0.route.record_id,
                "resource": offered.1.resource, "resource_kind": offered.1.resource_kind,
                "action": offered.1.action, "revision": revision });
            if let Some(context) = durable {
                context.before_admission(
                    self,
                    journal.host(),
                    peer_id,
                    &offered,
                    sender
                        .as_ref()
                        .ok_or("application invocation stream unavailable")?,
                )?;
            }
            if local_report.is_some() {
                self.local_report_receipts
                    .lock()
                    .map_err(|_| "local receipt cache unavailable")?
                    .admit(key.clone(), &intent, id, peer_id)?;
                Some(key)
            } else {
                let admission = if owner.verified_actor().is_some() {
                    let mut facts = invocation_journal::evidence_facts(&key, &intent)?;
                    if let Some(DurableContext::Task { caller, .. }) = durable {
                        let admitted = caller.current(self)?;
                        let certificate = admitted
                            .certificate
                            .as_ref()
                            .ok_or("task caller certificate missing")?;
                        if let crate::action_evidence::VerifiedActor::NativeTask {
                            root,
                            device,
                            ..
                        } = &mut facts.actor
                        {
                            *root = certificate.user_public_key;
                            *device = certificate.device_public_key;
                        }
                    }
                    Some(crate::action_evidence::mint(facts, &intent).map_err(|_| {
                        "action_evidence_unavailable: host signing identity unavailable".to_string()
                    })?)
                } else {
                    // Actor-only legacy/system work is never upgraded into a certified person.
                    None
                };
                let (fresh, durable) =
                    journal.admit_with_evidence(key.clone(), intent, id, admission)?;
                if !fresh {
                    return Err("invocation already admitted".into());
                }
                admitted = durable;
                Some(key)
            }
        } else {
            None
        };
        drop(journal_guard);
        if local_report.is_some() {
            // The app reports speculative local execution (possibly a read/no-op).
            // Retain only admission, never execution, offload, or success evidence.
            self.spend_once_grants(&offered.0.route.cluster_id, &spent);
            return Ok(StagedJob {
                admitted,
                start: Admitted::Reported(InvocationJob { id, completion: rx }),
            });
        }
        let sender = sender.ok_or("application invocation stream unavailable")?;
        // Spent now, not once durable: a grant used once never admits twice,
        // even should this admission fail.
        self.spend_once_grants(&offered.0.route.cluster_id, &spent);
        let checkpoint = (offered.0.app_id == "so.tilo.desktop"
            && offered.1.resource == workspace_checkpoints::RESOURCE)
            .then(|| {
                (
                    offered.0.route.cluster_id.clone(),
                    offered.1.action.clone(),
                    offered.1.contract.clone(),
                )
            });
        let untrusted = offered.1.contract.carries_untrusted_content();
        // Admitted: an offloadable action may now run on a better-placed
        // device, which the turn asks the route about.
        let offload = self.offload_request(
            (&offered.0.route.cluster_id, &offered.0.app_id),
            &offered.1,
            argument,
        );
        let pending = PendingInvocation {
            caller: continuing,
            activity: ActivityTarget::new(
                execution.clone(),
                (actor, caller, context.as_ref()),
                &offered.0,
                &offered.1,
            ),
            owner,
            network_evidence: durable.and_then(DurableContext::network),
            original: OriginalInvocationData::capture(
                ui::invocation_scope::PublishedInvocationRequest {
                    wire_id: id,
                    invocation,
                    address: tilo_actions::ActionAddress {
                        resource: offered.1.resource.clone(),
                        action: offered.1.action.clone(),
                    },
                    argument: argument.map(str::to_string),
                    actor: actor.id.clone(),
                    participant: invoker::invoker_participant(context.as_ref()),
                },
                offered.0.route.clone(),
                &offered.1,
                revision,
                durable.map(DurableContext::origin).or(origin),
                policy_time.clone(),
                execution.as_ref().map(ExecutionScope::lease),
            ),
            policy_time,
            journal_key,
            peer_id,
            tx,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
            prepared_effect: Arc::default(),
            last_sequence: 0,
            actor: actor.id.clone(),
            cancellable: offered.1.contract.execution.cancellable(),
            retryable_on_disconnect: matches!(
                offered.1.contract.retry,
                tilo_actions::RetryPolicy::Idempotent
                    | tilo_actions::RetryPolicy::RetryWithInvocationId
            ),
            execution: offered.1.contract.execution.clone(),
            result: offered.1.contract.result.clone(),
            operation_kind: offered.1.operation_kind,
            cluster_id: offered.0.route.cluster_id.clone(),
            context: receipt_context,
            last_activity: std::time::Instant::now(),
            cancellation_requested_at: None,
            cancellation_fence: Arc::new(AtomicBool::new(false)),
        };
        let envelope = InvocationEnvelope {
            id,
            invocation,
            resource: offered.1.resource,
            action: offered.1.action,
            argument: argument.map(str::to_string),
            actor: actor.id.clone(),
            context,
        };
        Ok(StagedJob {
            admitted,
            start: Admitted::Delivered(Box::new(Delivery {
                id,
                completion: rx,
                pending,
                envelope,
                sender,
                offload,
                checkpoint,
                untrusted,
            })),
        })
    }

    /// Retire only this run's transient presence, preserving its pending jobs.
    pub fn retire_execution(&self, caller: &tilo_actions::remote::Caller) {
        caller.retire_execution();
        self.live_views.retire(caller);
        if let Ok(mut pending) = self.pending_invocations.lock() {
            for invocation in pending.values_mut() {
                if invocation
                    .activity
                    .as_ref()
                    .is_some_and(|target| target.belongs_to(caller))
                {
                    self.execution_activity
                        .publish_status(invocation.activity.as_ref(), ExecutionStatus::Ended);
                    if let Some(target) = &mut invocation.activity {
                        target.retire_scope();
                    }
                }
            }
        }
    }

    /// Observe subsequent broker execution events without scheduling app work.
    /// Report an invocation's progress on the observation bus, and in the
    /// view it runs in for an actor that view does not already show: an
    /// agent, an app, someone acting for a person, a person on another
    /// device. Nothing is performed out of sight; the view names who acts.
    fn publish_activity(
        &self,
        target: Option<&ActivityTarget>,
        update: &tilo_actions::InvocationUpdate,
    ) {
        self.execution_activity.publish(target, update);
        let Some((target, shown)) = target.and_then(|target| Some((target, target.shown()?)))
        else {
            return;
        };
        let phase = match update {
            tilo_actions::InvocationUpdate::Accepted { .. }
            | tilo_actions::InvocationUpdate::Progress { .. }
            | tilo_actions::InvocationUpdate::Item { .. } => {
                tilo_actions::InteractionPhase::Applying
            }
            tilo_actions::InvocationUpdate::Succeeded { .. } => {
                tilo_actions::InteractionPhase::Committed
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                tilo_actions::InteractionPhase::Failed {
                    code: Some(error.code.clone()),
                    message: error.message.clone(),
                }
            }
            tilo_actions::InvocationUpdate::Cancelled { .. } => {
                tilo_actions::InteractionPhase::Cancelled { message: None }
            }
        };
        self.publish_interaction(target.route(), shown, update.invocation(), phase);
    }

    /// Show `shown` in the view `route` names, in `phase` of `invocation`.
    fn publish_interaction(
        &self,
        route: &CrdtRoute,
        shown: &execution_activity::ShownActor,
        invocation: tilo_actions::InvocationId,
        phase: tilo_actions::InteractionPhase,
    ) {
        let sequence = shown
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        let interaction = tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation,
            sequence,
            actor: shown.actor.clone(),
            phase,
            action: shown.action.clone(),
            resource: shown.resource.clone(),
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: shown.group.clone(),
        };
        if interaction.validate().is_err() {
            return;
        }
        let key = PresenceKey::resource(&route.app_id, &shown.resource.id, &shown.resource.kind);
        let payload = PresencePayload::Interaction(Box::new(interaction));
        // Peers see it too; their ingress keeps the actor only as far as
        // this device's certified person vouches for it.
        self.publish_presence(route, &key, &payload);
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: shown.actor.id.clone(),
            key,
            payload,
        };
        if let Some(fanout) = self.fanout.upgrade() {
            // No connection is peer 0: the view the action runs in sees it too.
            fanout.fanout_to_route(&event, 0, &route.to_cluster_scope());
        }
    }

    /// The fixed 256-event buffer reports `Lagged` rather than concealing loss;
    /// consumers must clear stale presence on lag/closure. Dropping a receiver
    /// changes neither sessions nor app state. This is host-local provenance,
    /// not a substitute for authenticated transport or current room authority.
    #[must_use]
    pub fn subscribe_execution_activity(
        &self,
    ) -> tokio::sync::broadcast::Receiver<Arc<ExecutionActivity>> {
        self.execution_activity.subscribe()
    }

    /// Changes whenever a pending or completed invocation changes.
    #[must_use]
    pub fn invocations_generation(&self) -> u64 {
        *self.invocation_generation.borrow()
    }

    /// Record that a pending or completed invocation changed, waking every
    /// [`Self::invocation_changes`] waiter.
    fn invocation_changed(&self) {
        self.invocation_generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// A receiver that sees every later invocation change; what it has seen
    /// so far counts as seen.
    pub(crate) fn invocation_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.invocation_generation.subscribe()
    }

    /// A receiver that sees every later published-surface change; what it
    /// has seen so far counts as seen.
    pub(crate) fn surface_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.surface_changes.subscribe()
    }

    /// Read one current receipt without surveying or disclosing another
    /// actor's jobs.
    #[must_use]
    pub fn invocation_status_for_actor(
        &self,
        id: u64,
        actor: &str,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.invocation_status_for_owner(
            id,
            &InvocationOwner::Local {
                actor: actor.to_string(),
            },
        )
    }

    /// Check `evidence` against the offered action's confirmation policy and,
    /// when one is required, mint the one-use grant that binds it to this
    /// exact peer, address, argument and actor.
    pub(crate) fn confirmation_token(
        &self,
        peer_id: u64,
        app: &str,
        offered: &PublishedAction,
        argument: Option<&str>,
        actor: &BrokerActor,
        evidence: Option<&tilo_actions::remote::Confirmation>,
        binding: &WitnessBinding,
    ) -> Result<Option<String>, tilo_actions::remote::ActionHostError> {
        use tilo_actions::remote::{ActionHostError, Confirmation};
        // Only a person acting themselves confirms by their own word; anyone
        // acting for them carries the answer the person gave on their device.
        let witnessed = match evidence {
            Some(Confirmation::Witnessed(token)) => {
                binding.actor == actor.id()
                    && binding.resource == offered.resource
                    && binding.action == offered.action
                    && binding.argument_digest == argument_digest(argument)
                    && self.take_witness(token, binding)
            }
            _ => false,
        };
        // Store disclosures must be answered on a trusted host sheet, even
        // when the publisher's connection is admitted as the person themself.
        if requires_store_witness(app, &offered.action) && !witnessed {
            return Err(ActionHostError::ConfirmationRequired);
        }
        let own = actor.direct().then_some(evidence).flatten();
        // Decided from the arguments, without the app's code: a field edit
        // asks only when it removes what a person may want back.
        let policy = offered.contract.confirmation_for_argument(argument);
        if matches!(policy, tilo_actions::ConfirmationPolicy::None) {
            // A confirmation given anyway answers the room's ask, if the
            // grants ask; admission decides whether they do.
            if !witnessed && !matches!(own, Some(Confirmation::Confirmed)) {
                return Ok(None);
            }
        } else if !witnessed && !confirmation_satisfied(&policy, own) {
            return Err(ActionHostError::ConfirmationRequired);
        }
        self.mint_confirmation_grant(peer_id, offered, argument, actor)
            .and_then(|token| {
                if witnessed {
                    self.confirmation_grants
                        .lock()
                        .map_err(|_| "the confirmation registry is unavailable".to_owned())?
                        .get_mut(&token)
                        .ok_or("the confirmation grant disappeared")?
                        .host_witnessed = Some(binding.clone());
                }
                Ok(token)
            })
            .map(Some)
            .map_err(|reason| ActionHostError::Unavailable { reason })
    }

    /// Trusted internal callers only: admission must already have checked
    /// the person's answer for the exact invocation. Untrusted confirmations
    /// must go through `confirmation_token` instead.
    ///
    /// Mint the one-use grant carrying a confirmation already decided to
    /// the dispatch of `actor`'s invocation of `offered` on `peer_id` with
    /// `argument`, and nothing else.
    pub(crate) fn mint_confirmation_grant(
        &self,
        peer_id: u64,
        offered: &PublishedAction,
        argument: Option<&str>,
        actor: &BrokerActor,
    ) -> Result<String, String> {
        let unavailable = || "the confirmation registry is unavailable".to_owned();
        let token = uuid::Uuid::new_v4().to_string();
        let now = std::time::Instant::now();
        let expires = now
            .checked_add(std::time::Duration::from_secs(60))
            .ok_or_else(unavailable)?;
        let mut grants = self.confirmation_grants.lock().map_err(|_| unavailable())?;
        grants.retain(|_, grant| grant.expires >= now);
        grants.insert(
            token.clone(),
            ConfirmationGrant {
                peer_id,
                resource: offered.resource.clone(),
                action: offered.action.clone(),
                argument_digest: argument_digest(argument),
                actor: actor.id.clone(),
                host_witnessed: None,
                expires,
            },
        );
        drop(grants);
        Ok(token)
    }

    /// Record that the person `actor` acts for confirmed `actor`'s invocation
    /// of `action` on `resource` with `argument` on their own device. The
    /// returned token, carried as [`Confirmation::Witnessed`], answers that
    /// one invocation once, within a minute.
    ///
    /// [`Confirmation::Witnessed`]: tilo_actions::remote::Confirmation::Witnessed
    pub(crate) fn witness_confirmation(&self, binding: &WitnessBinding) -> Option<String> {
        let now = std::time::Instant::now();
        let expires = now.checked_add(std::time::Duration::from_secs(60))?;
        let token = uuid::Uuid::new_v4().to_string();
        let mut witnessed = self.witnessed_confirmations.lock().ok()?;
        witnessed.retain(|_, witness| witness.expires >= now);
        witnessed.insert(
            token.clone(),
            Witness {
                binding: binding.clone(),
                expires,
            },
        );
        drop(witnessed);
        Some(token)
    }

    /// Spend the person's answer `token` names, when it was given for this
    /// very invocation.
    fn take_witness(&self, token: &str, binding: &WitnessBinding) -> bool {
        let Ok(mut witnessed) = self.witnessed_confirmations.lock() else {
            return false;
        };
        let matches = witnessed.get(token).is_some_and(|witness| {
            witness.expires >= std::time::Instant::now() && witness.binding == *binding
        });
        if matches {
            witnessed.remove(token);
        }
        matches
    }

    /// Whether job `id`'s action declares untrusted content.
    pub fn untrusted_job(&self, id: u64) -> bool {
        self.untrusted_jobs
            .lock()
            .is_ok_and(|untrusted| untrusted.contains(&id))
    }

    pub fn invocation_status(&self, id: u64) -> Option<tilo_actions::InvocationUpdate> {
        self.expire_stale_invocations();
        if let Some(update) = self.live_views.status(id, None) {
            return Some(update);
        }
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).map(|job| job.update.clone()))
            .or_else(|| {
                self.completed_invocations
                    .lock()
                    .ok()
                    .and_then(|jobs| jobs.get(&id).map(|(_, update)| update.clone()))
            })
    }

    fn invocation_status_for_owner(
        &self,
        id: u64,
        owner: &InvocationOwner,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.expire_stale_invocations();
        if let Some(update) = self.live_views.status(id, Some(owner)) {
            return Some(update);
        }
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|jobs| {
                jobs.get(&id)
                    .filter(|job| &job.owner == owner)
                    .map(|job| job.update.clone())
            })
            .or_else(|| {
                self.completed_invocations.lock().ok().and_then(|jobs| {
                    jobs.get(&id)
                        .filter(|(retained, _)| retained == owner)
                        .map(|(_, update)| update.clone())
                })
            })
    }

    pub fn invocation_statuses(&self, actor: &str) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        self.invocation_statuses_for_owner(&InvocationOwner::Local {
            actor: actor.into(),
        })
    }

    fn invocation_statuses_for_owner(
        &self,
        owner: &InvocationOwner,
    ) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        self.expire_stale_invocations();
        let mut jobs = self
            .pending_invocations
            .lock()
            .map(|pending| {
                pending
                    .iter()
                    .filter(|(_, job)| &job.owner == owner)
                    .map(|(id, job)| (*id, job.update.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Ok(completed) = self.completed_invocations.lock() {
            jobs.extend(
                completed
                    .iter()
                    .filter(|(_, (retained, _))| retained == owner)
                    .map(|(id, (_, update))| (*id, update.clone())),
            );
        }
        jobs.sort_by_key(|(id, _)| *id);
        jobs
    }

    /// Request cooperative cancellation for an actor-owned invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the invocation is unknown, foreign, or non-cancellable.
    pub(crate) fn request_cancellation(
        &self,
        id: u64,
        actor: &str,
    ) -> Result<invocation_journal::Durable, String> {
        self.request_cancellation_for_owner(
            id,
            &InvocationOwner::Local {
                actor: actor.into(),
            },
        )
    }

    /// Ask invocation `id`'s application to stop, for `owner`. The asker is
    /// answered once the returned write resolves.
    fn request_cancellation_for_owner(
        &self,
        id: u64,
        owner: &InvocationOwner,
    ) -> Result<invocation_journal::Durable, String> {
        self.expire_stale_invocations();
        if let Some(cancelled) = self.live_views.cancel(id, Some(owner)) {
            return cancelled.map(|()| invocation_journal::Durable::settled());
        }
        let mut pending = self
            .pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?;
        let job = pending
            .get(&id)
            .ok_or_else(|| "the invocation is not running".to_string())?;
        if &job.owner != owner {
            return Err("only the invocation owner may cancel it".into());
        }
        if !job.cancellable {
            return Err("the action did not declare cooperative cancellation".into());
        }
        let job = pending
            .get_mut(&id)
            .ok_or_else(|| "the invocation is not running".to_string())?;
        let durable = self.persist_cancellation(job)?;
        if job.cancellation_requested_at.is_none() {
            job.cancellation_requested_at = Some(std::time::Instant::now());
            self.execution_activity.publish_status(
                job.activity.as_ref(),
                ExecutionStatus::CancellationRequested,
            );
            self.invocation_changed();
        }
        drop(pending);
        self.cancellation_requests
            .lock()
            .map_err(|_| "the cancellation registry is unavailable".to_string())?
            .insert(id);
        Ok(durable)
    }

    pub fn can_cancel(&self, id: u64, actor: &str) -> bool {
        self.can_cancel_for_owner(
            id,
            &InvocationOwner::Local {
                actor: actor.into(),
            },
        )
    }

    fn can_cancel_for_owner(&self, id: u64, owner: &InvocationOwner) -> bool {
        self.expire_stale_invocations();
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|pending| {
                pending
                    .get(&id)
                    .map(|job| job.cancellable && &job.owner == owner)
            })
            .unwrap_or(false)
    }

    /// Whether this room currently has a subscribed, admitted agent that can
    /// receive a task lease. Presence or membership alone is not sufficient.
    pub fn agent_is_live_task_capable(&self, cluster_id: &str, agent_id: &str) -> bool {
        self.task_subscribers.lock().is_ok_and(|subscribers| {
            subscribers.values().any(|subscriber| {
                subscriber.cluster_id == cluster_id && subscriber.agent_id == agent_id
            })
        })
    }

    fn task_run_for(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
    ) -> Result<PendingTaskRun, String> {
        self.pending_task_runs
            .lock()
            .map_err(|_| "the task run registry is unavailable".to_string())?
            .get(&run_id)
            .filter(|run| {
                run.peer_id == peer_id
                    && run.agent_id == agent_id
                    && runtime_collab::native_task_revision(&run.lease)
                        .zip(runtime_collab::native_task_revision(lease))
                        .is_some_and(|(old, new)| old.0 == new.0)
                    && !run.terminal
            })
            .cloned()
            .ok_or_else(|| {
                "the task run lease is unknown, expired, or belongs to another agent".to_string()
            })
    }

    /// Whether the person stopped this run on its issue. Agents may observe
    /// that; only the issue's own actions change it.
    fn task_run_cancelled(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
    ) -> Result<bool, String> {
        let run = self.task_run_for(peer_id, agent_id, run_id, lease)?;
        Ok(self.native_task_status(&run, lease)?.cancelled)
    }

    fn record_task_run_update(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
        update: &runtime_collab::TaskRunUpdate,
    ) -> Result<(), String> {
        let run = self.task_run_for(peer_id, agent_id, run_id, lease)?;
        self.report_tracker_run(&run, update)
    }

    fn start_task_action(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
        target: runtime_collab::TaskAction<'_>,
        (provenance, confirmed): (Option<&LocalProvenance>, (bool, Option<&str>)),
    ) -> Result<StagedJob, String> {
        let runtime_collab::TaskAction {
            peer_id: target_peer,
            provider: _,
            resource,
            action,
            argument: _,
        } = target;
        let run = self.task_run_for(peer_id, agent_id, run_id, lease)?;
        if self.task_run_cancelled(peer_id, agent_id, run_id, lease)? {
            return Err("the task was cancelled; no further action may start".into());
        }
        let (surface, descriptor) = self
            .surfaces
            .lock()
            .map_err(|_| "task action surface unavailable")?
            .get(&target_peer)
            .filter(|surface| surface.route.cluster_id == run.cluster_id)
            .and_then(|surface| surface.action_projection(resource, action))
            .ok_or("the task action is no longer published in this workspace")?;
        self.admit_task_action(
            &run,
            lease,
            target,
            (provenance, confirmed),
            (surface, descriptor),
        )
    }

    fn admit_task_action(
        &self,
        run: &PendingTaskRun,
        lease: &str,
        target: runtime_collab::TaskAction<'_>,
        (provenance, confirmed): (Option<&LocalProvenance>, (bool, Option<&str>)),
        offered: (PublishedSurface, PublishedAction),
    ) -> Result<StagedJob, String> {
        let runtime_collab::TaskAction {
            peer_id: target_peer,
            provider: _,
            resource,
            action: _,
            argument,
        } = target;
        let (surface, descriptor) = offered;
        if surface.route.cluster_id != run.cluster_id
            || surface.peer_id != target_peer
            || !surface.live
        {
            return Err("task action publisher changed workspace or retired".into());
        }
        // Task transport carries a JSON argument map directly; the contract
        // evaluator also accepts the action door's prefixed map encoding.
        let contract_argument = argument.and_then(|text| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text)
                .ok()
                .map(|_| format!("{}{text}", tilo_actions::ARGUMENTS_JSON_PREFIX))
        });
        let consequential = descriptor
            .contract
            .asks_for_argument(contract_argument.as_deref().or(argument));
        // A run pauses for its person where an action asks first
        // (`native_task_confirmation`).
        let binding = run.native.clone();
        let requirement = crate::services::NativeTaskActionRequirement {
            binding: run.native.clone(),
            app: surface.app_id.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource: resource.into(),
            action: descriptor.action.clone(),
            need: descriptor.contract.need(),
            consequential,
            attended: descriptor.contract.is_attended(),
            confirmed: confirmed.0,
        };
        let (caller, invocation) =
            provenance.ok_or("native task action requires authenticated original caller")?;
        let mut current = binding;
        current.revision = runtime_collab::native_task_revision(lease)
            .ok_or("invalid native lease")?
            .1;
        let mut start = || {
            self.start_native_task_invocation(
                run,
                target,
                caller,
                *invocation,
                (surface.clone(), descriptor.clone()),
                (confirmed.0 || consequential, confirmed.1),
            )
        };
        self.admit_tracker_action(run, &current, &requirement, &mut start)
    }

    fn disconnect_device_media(&self, peer_id: u64) {
        if let Some(media) = self.services.native_media.as_ref() {
            media.disconnect(peer_id);
        }
        if let Some(output) = self.services.audio_output.as_ref() {
            output.disconnect(peer_id);
        }
    }

    /// A connection ended: every route it held is withdrawn, then what the
    /// connection itself owned.
    fn remove_connection(&self, peer_id: u64) {
        for hold in self.connection_holds(peer_id) {
            self.withdraw_route_hold(hold);
        }
        self.launch_disconnected(peer_id);
        if let Ok(mut windows) = self.window_instances.lock() {
            windows.remove(&peer_id);
        }
        self.disconnect_device_media(peer_id);
        if let Some(realtime) = self.services.realtime.as_ref() {
            realtime.disconnect(peer_id);
        }
        self.disconnect_file_transfers(peer_id);
        if let Ok(mut subscribers) = self.action_catalogue_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
    }

    /// One route hold ended ([`route_holds`]): withdraw what it published
    /// and worked as there, and fail what was still pending on it.
    fn withdraw_route_hold(&self, peer_id: u64) {
        self.release_route_hold(peer_id);
        self.witness_publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&peer_id);
        if let Ok(mut directories) = self.canonical_directories.lock() {
            directories.remove(peer_id);
        }
        // Withdraw provenance before any stale surface can be observed during cleanup.
        match self.host_workers.lock() {
            Ok(mut workers) => workers.remove(peer_id),
            Err(poisoned) => poisoned.into_inner().remove(peer_id),
        }
        if let Ok(mut tables) = self.surface_tables.lock() {
            tables.remove(&peer_id);
        }
        if let Ok(mut surfaces) = self.surfaces.lock()
            && surfaces.remove(&peer_id).is_some()
        {
            self.surfaces_changed();
        }
        if let Ok(mut roots) = self.action_directory_roots.lock() {
            roots.remove(&peer_id);
        }
        if let Ok(mut checkouts) = self.native_checkouts.lock() {
            checkouts.remove(&peer_id);
        }
        self.refresh_action_directory_root_snapshot();
        if let Ok(mut capabilities) = self.stable_invocation_subscribers.lock() {
            capabilities.remove(&peer_id);
        }
        if let Ok(mut subscribers) = self.invocation_subscribers.lock()
            && subscribers.remove(&peer_id).is_some()
        {
            drop(subscribers);
            self.invocation_streams_changed();
        }
        self.disconnect_native_task_stream(peer_id);
        self.fail_pending_invocations_of(peer_id);
        self.publish_action_catalogue();
    }

    /// Fail every invocation still pending on `peer_id`, which went away
    /// before completing it: an unjournaled one fails, a journaled one's
    /// outcome is unknown.
    fn fail_pending_invocations_of(&self, peer_id: u64) {
        if let Ok(mut pending) = self.pending_invocations.lock() {
            let ids = pending
                .iter()
                .filter_map(|(id, invocation)| {
                    (invocation.peer_id == peer_id && !self.committing(*id)).then_some(*id)
                })
                .collect::<Vec<_>>();
            for id in ids {
                if let Some(invocation) = pending.remove(&id) {
                    invocation.cancellation_fence.store(true, Ordering::Release);
                    let completion = InvocationCompletion {
                        code: "action_failed".into(),
                        message: "application disconnected before completing the action".into(),
                    };
                    let update = tilo_actions::InvocationUpdate::Failed {
                        invocation: invocation.update.invocation(),
                        error: tilo_actions::ActionFailure {
                            code: completion.code.clone(),
                            message: completion.message.clone(),
                            details: None,
                            retryable: invocation.retryable_on_disconnect,
                        },
                    };
                    let (update, completion) = if invocation.journal_key.is_some() {
                        (
                            invocation_journal::unknown(invocation.update.invocation()),
                            InvocationCompletion {
                                code: "action_outcome_unknown".into(),
                                message: "application outcome unknown after disconnect".into(),
                            },
                        )
                    } else {
                        (update, completion)
                    };
                    if self
                        .persist_uncertain_outcome(&invocation, update.clone())
                        .is_err()
                    {
                        self.execution_activity.publish_status(
                            invocation.activity.as_ref(),
                            ExecutionStatus::OutcomeUnknown,
                        );
                        continue;
                    }
                    self.publish_activity(invocation.activity.as_ref(), &update);
                    self.record_completion(id, invocation.completion_owner(), update.clone());
                    if let Ok(mut requests) = self.cancellation_requests.lock() {
                        requests.remove(&id);
                    }
                    let _ = invocation.tx.send(completion);
                    self.record_effect_receipt(&invocation, &update);
                }
            }
        }
    }

    fn disconnect_file_transfers(&self, peer_id: u64) {
        if let Some(service) = self.services.browser_service_sessions.as_ref() {
            service.disconnect(peer_id);
        }
        if let Some(nearby) = self.services.nearby_transfer.as_ref() {
            nearby.disconnect(peer_id);
        }
        if let Some(clipboard) = self.services.file_clipboard.as_ref() {
            clipboard.disconnect(peer_id);
        }
    }

    fn disconnect_native_task_stream(&self, peer_id: u64) {
        if let Ok(mut subscribers) = self.task_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut runs) = self.pending_task_runs.lock() {
            runs.retain(|_, run| run.peer_id != peer_id);
        }
    }

    /// Stable authenticated actor id for native presence and interactions.
    /// Display handles remain presentation only.
    fn participant_id(admitted: &AdmittedJoinIdentity) -> String {
        admitted.agent_id.as_ref().map_or_else(
            || admitted.principal_id.clone(),
            |agent| format!("agent:{agent}"),
        )
    }

    fn revalidate_admission(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<AdmittedJoinIdentity, capnp::Error> {
        let now = self.policy_now_ns().map_err(capnp::Error::failed)?;
        if let Some(cert) = &admitted.certificate {
            tilo_user_identity::verify_device_cert_at(cert, now)
                .map_err(|_| capnp::Error::failed("certificate no longer valid".into()))?;
            if let Some(scope) = &admitted.host_scope {
                if !self.is_accepting()
                    || !cert.capabilities.can_act_as_host
                    || route != &scope.route
                    || cert != &scope.certificate
                    || admitted.agent_id.is_some()
                {
                    return Err(capnp::Error::failed(
                        "host worker scope no longer valid".into(),
                    ));
                }
                self.certificate_authority
                    .get()
                    .ok_or_else(|| {
                        capnp::Error::failed("current host worker authority unavailable".into())
                    })?
                    .authorize_host_worker(route, cert)
                    .map_err(capnp::Error::failed)?;
            } else {
                if cert.capabilities.can_act_as_host {
                    return Err(capnp::Error::failed(
                        "host certificate requires a private host worker descriptor".into(),
                    ));
                }
                if let Some(authority) = self.certificate_authority.get() {
                    let room = uuid::Uuid::parse_str(&route.cluster_id)
                        .map_err(|_| capnp::Error::failed("invalid exact room".into()))?;
                    authority
                        .authorize_join(room, cert)
                        .map_err(capnp::Error::failed)?;
                }
            }
        } else if admitted.host_scope.is_some() {
            return Err(capnp::Error::failed(
                "host worker certificate missing".into(),
            ));
        }
        let membership = self.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("certified session lost its membership authority".into())
        })?;
        // The certificate was checked above; its connection ceiling must also
        // reflect current governance, not just governance at the original join.
        let verified = match &admitted.certificate {
            Some(cert) => VerifiedJoinIdentity::of(cert, self.capped_in(&route.cluster_id, cert)),
            None => admitted.verified.clone(),
        };
        // The person and any delegated agent are revalidated on one sample.
        membership
            .decide_in_room(&route.cluster_id, |workspace| {
                let mut current =
                    admit_verified_join_at(workspace, route, verified, now / 1_000_000_000)
                        .map_err(|error| {
                            format!(
                                "certified collaboration session is no longer admitted: {error}"
                            )
                        })?;
                current.certificate.clone_from(&admitted.certificate);
                current.host_scope.clone_from(&admitted.host_scope);
                current
                    .worker_registration
                    .clone_from(&admitted.worker_registration);
                if let Some(agent_id) = &admitted.agent_id {
                    admit_agent_delegation_at(
                        workspace,
                        &mut current,
                        agent_id,
                        now / 1_000_000_000,
                    )
                    .map_err(|error| {
                        format!("certified agent session is no longer admitted: {error}")
                    })?;
                } else {
                    current.display_handle.clone_from(&admitted.display_handle);
                }
                Ok::<_, String>(current)
            })
            .and_then(std::convert::identity)
            .map_err(capnp::Error::failed)
    }

    /// Bind a certified operation to the route, typed document protocol and
    /// current workspace capability state. The identity service performs the
    /// same checks before signing; repeating them at receipt prevents a stale
    /// role or capability from remaining effective after launch.
    fn authorize_certified_document(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
        envelope: &SignedOp,
    ) -> Result<AuthorizedDocumentRoute, capnp::Error> {
        let now = self.policy_now_ns().map_err(capnp::Error::failed)? / 1_000_000_000;
        if route.app_id.is_empty() {
            return Err(capnp::Error::failed(
                "certified document operation does not match its joined route".into(),
            ));
        }
        let payload = collab_types::validate_document_payload_route(
            &route.app_id,
            &route.cluster_id,
            &route.record_id,
            envelope,
        )
        .map_err(|error| {
            capnp::Error::failed(format!(
                "certified document operation has an invalid routed payload: {error}"
            ))
        })?;
        let channel = match &payload {
            collab_types::RoutedDocumentPayload::Cleartext(_)
            | collab_types::RoutedDocumentPayload::CleartextSegment(_) => {
                collab_types::document_channel_id(
                    &route.app_id,
                    &route.cluster_id,
                    &route.record_id,
                )
            }
            collab_types::RoutedDocumentPayload::Encrypted { channel, .. } => *channel,
        };
        let broker_channels = self.broker_channels.lock().map_err(|_| {
            capnp::Error::failed("collaboration governance state is unavailable".into())
        })?;
        if !broker_channels.document_encryption_allowed(channel, envelope.encryption.as_ref()) {
            return Err(capnp::Error::failed(
                "plaintext document operation rejected after MLS migration".into(),
            ));
        }
        drop(broker_channels);
        // The change the op makes must be allowed outright: a signed op
        // carries no answer to a grant that would ask first.
        let record = collab_types::Record {
            room: &route.cluster_id,
            app: &route.app_id,
            record: &route.record_id,
        };
        let allowed = admitted
            .with_current_authority(self.membership.as_ref(), &route.cluster_id, |authority| {
                let allows = |node: &tilo_action_policy::TreePath, kind| {
                    authority
                        .decide(node, kind, &tilo_desktop_model::context_at(now))
                        .answer
                        == Answer::Allow
                };
                collab_types::decide_op(
                    envelope,
                    &record,
                    None,
                    None,
                    &collab_types::Surfaces::process(),
                    &allows,
                )
                .is_ok_and(|receipt| receipt == collab_types::Receipt::Admit)
            })
            .map_err(capnp::Error::failed)?;
        if !allowed {
            return Err(capnp::Error::failed(
                "current workspace grants deny this document action".into(),
            ));
        }
        Ok(AuthorizedDocumentRoute {
            artifact: envelope.artifact,
            channel,
            payload,
        })
    }

    fn publisher(&self) -> &Arc<PeerPublisher> {
        &self.publisher
    }

    /// Converts once into the transport-neutral peer presence envelope.
    /// Peer replication does not embed either local RPC dialect.
    fn publish_presence(&self, route: &CrdtRoute, key: &PresenceKey, payload: &PresencePayload) {
        let Some(presence) = crate::collab_presence::to_peer(key, payload) else {
            tracing::warn!(
                channel = %route.channel_key(),
                "capnp broker: presence would not encode; peers will \
                 not see this person move",
            );
            return;
        };
        self.publisher().presence(route, presence);
    }

    /// Delegates to the shared fanout registry for typed connection ids.
    fn next_peer_id(&self) -> u64 {
        // Peer table torn down: racing with shutdown. Hand out a
        // placeholder id; the caller drops it on the next tokio yield.
        self.fanout.upgrade().map_or(u64::MAX, |t| t.next_peer_id())
    }

    /// A peer id for one of the broker's own services, which runs on the
    /// broker thread's runtime and is started again with it.
    fn next_service_peer_id(&self) -> u64 {
        let peer_id = self.next_peer_id();
        match self.service_peers.lock() {
            Ok(mut peers) => peers.push(peer_id),
            Err(poisoned) => poisoned.into_inner().push(peer_id),
        }
        peer_id
    }

    /// Withdraw the services a broker thread that unwound had started, as
    /// if their connections had closed, before a new thread starts them.
    fn forget_service_peers(&self) {
        let peers = match self.service_peers.lock() {
            Ok(mut peers) => std::mem::take(&mut *peers),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for peer_id in peers {
            self.remove_connection(peer_id);
        }
    }

    /// Drive an event onto the subscribed queues `route` addresses,
    /// skipping `except`. Called from [`FanoutState::fanout_to_route`]
    /// Delegates to
    /// [`CrdtRoute::delivers_to`] rather than reimplementing the filter,
    /// so RPC and in-process delivery cannot drift.
    pub fn fanout_to_route(&self, event: &CollabEvent, except: u64, route: &CrdtRoute) {
        if route.cluster_id.is_empty() {
            return;
        }
        let Some(admission) = self.admission_of(event, route) else {
            return;
        };
        let Ok(mut all_channels) = self.channels.lock() else {
            return;
        };
        let channels = all_channels
            .iter_mut()
            .filter(|(k, _)| route.delivers_to(&CrdtRoute::from_channel_key(k)))
            .map(|(_, subs)| subs);
        match event {
            CollabEvent::Document {
                envelope,
                from_user_id,
                ..
            } => {
                Self::fanout_signed_incremental(
                    channels,
                    except,
                    envelope,
                    from_user_id,
                    admission,
                );
            }
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                Self::fanout_cursor_moved(channels, except, user_id, *x, *y);
            }
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            } => Self::fanout_presence_update(channels, except, user_id, key, payload),
            CollabEvent::Context {
                collaborators,
                permissions,
            } => Self::fanout_context(channels, except, collaborators, permissions),
            CollabEvent::FollowChanged { peer_id } => {
                Self::fanout_follow_changed(channels, except, peer_id.as_deref());
            }
            CollabEvent::Handoff { request } => {
                let handoff = ContextEnvelope::Handoff(request.encode());
                subscriptions::context_frame(channels, except, &handoff);
            }
            CollabEvent::RecordsArrived { records } => {
                subscriptions::context_frame(
                    channels,
                    except,
                    &ContextEnvelope::RecordsArrived(records.clone()),
                );
            }
            CollabEvent::ActionArrived { type_id, .. } => {
                // Unreachable today; said out loud, not silently dropped.
                tracing::warn!(
                    %type_id,
                    "capnp fanout: no action stream yet, action dropped"
                );
            }
            CollabEvent::ScrollbackBatch { .. }
            | CollabEvent::HistoryAgents { .. }
            | CollabEvent::DocumentSnapshotRequested { .. }
            | CollabEvent::ChannelAccess { .. }
            | CollabEvent::CommittedDocument { .. }
            | CollabEvent::Invocation { .. }
            | CollabEvent::InvocationCancellation { .. }
            | CollabEvent::ActionCatalogue { .. }
            | CollabEvent::PublishedInvocationResult { .. }
            | CollabEvent::LocalDocumentActionReported { .. }
            | CollabEvent::InstalledApplicationLaunchResult { .. }
            | CollabEvent::ActionDirectoryPage { .. }
            | CollabEvent::PublishedActionResolution { .. }
            | CollabEvent::WorkspaceControl { .. }
            | CollabEvent::TaskRun(_)
            | CollabEvent::TaskActionInvocationResult { .. }
            | CollabEvent::TaskRunCancellation { .. }
            | CollabEvent::TaskRunControl { .. }
            | CollabEvent::TaskRunReportResult { .. }
            | CollabEvent::NativeMediaCommandResult { .. }
            | CollabEvent::RealtimeCommandResult { .. }
            | CollabEvent::NearbyTransferCommandResult { .. }
            | CollabEvent::SelectedDiscoveryResult { .. }
            | CollabEvent::OpenSelectedV2Result { .. }
            | CollabEvent::OpenSelectedResult { .. }
            | CollabEvent::NearbyTransferChanged
            | CollabEvent::SpeakersChanged
            | CollabEvent::DrivesChanged
            | CollabEvent::LocalFileClipboardResult { .. }
            | CollabEvent::Realtime(_)
            | CollabEvent::RecordContent { .. }
            | CollabEvent::RecordContentState { .. }
            | CollabEvent::RemoteAction { .. } => {
                // Invocation delivery and cooperative cancellation are
                // point-to-point and never enter route fanout.
            }
            #[cfg(unix)]
            CollabEvent::ManagedInvocationObservation { .. } => {
                // Funded sender observations remain with their local owner.
            }
        }
    }

    /// One arm of [`Self::fanout_to_route`]: a validated signed op.
    fn fanout_signed_incremental<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        envelope: &SignedOp,
        from_user_id: &str,
        admission: OpAdmission,
    ) {
        let op_bytes = match postcard::to_stdvec(envelope) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "capnp fanout: postcard(SignedOp) failed");
                return;
            }
        };
        let env = OpEnvelope {
            op_bytes,
            from_user_id: from_user_id.to_owned(),
            kind: OpKind::Signed,
            admission,
        };
        for subs in channels {
            subs.op.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                s.tx.send(env.clone()).is_ok()
            });
        }
    }

    /// One arm of [`Self::fanout_to_route`]: a keyless cursor move, translated
    /// onto the sole general presence stream.
    fn fanout_cursor_moved<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        user_id: &str,
        x: f32,
        y: f32,
    ) {
        let key_bytes = default_pointer_key_bytes();
        let presence_env = PresenceEnvelope {
            user_id: user_id.to_owned(),
            key_bytes,
            payload: PresencePayload::Pointer { x, y },
        };
        for subs in channels {
            subs.presence.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                s.tx.send(presence_env.clone()).is_ok()
            });
        }
    }

    /// One arm of [`Self::fanout_to_route`]: a keyed presence update
    /// (caret, selection, arrived, left, ...).
    fn fanout_presence_update<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        user_id: &str,
        key: &PresenceKey,
        payload: &PresencePayload,
    ) {
        // The desktop's own presence (availability, waves) is the shell's
        // to show; an app never receives it.
        if !payload.reaches_apps() {
            return;
        }
        let key_bytes = match postcard::to_stdvec(key) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "capnp fanout: postcard(PresenceKey) failed");
                return;
            }
        };
        let env = PresenceEnvelope {
            user_id: user_id.to_owned(),
            key_bytes,
            payload: payload.clone(),
        };
        for subs in channels {
            subs.presence.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                s.tx.send(env.clone()).is_ok()
            });
        }
    }

    /// One arm of [`Self::fanout_to_route`]: the room's collaborator +
    /// permission context.
    fn fanout_context<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        collaborators: &[ui::collaboration::Collaborator],
        permissions: &[String],
    ) {
        let env = ContextEnvelope::Snapshot {
            collaborators: collaborators.to_vec(),
            permissions: permissions.to_vec(),
        };
        for subs in channels {
            subs.context.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                // App-authored context is not complete membership authority.
                // Native receivers get owner-backed add/remove refreshes from
                // `refresh_workspace_context_services` instead.
                if s.native.is_some() {
                    return true;
                }
                s.tx.send(env.clone()).is_ok()
            });
        }
    }

    /// Session-local view binding. It rides the session context stream but
    /// is explicitly tagged so clients do not replace their roster with an
    /// empty control frame.
    fn fanout_follow_changed<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        peer_id: Option<&str>,
    ) {
        let env = ContextEnvelope::FollowChanged {
            peer_id: peer_id.map(str::to_owned),
        };
        for subs in channels {
            subs.context.retain(|sub| {
                if sub.peer_id == except {
                    return true;
                }
                sub.tx.send(env.clone()).is_ok()
            });
        }
    }

    /// Session-local handoff control for the instance(s) on the addressed
    /// primary route. Like Follow, it rides the context stream tagged so the
    /// client never mistakes it for a roster.
    fn ensure_channel(&self, channel_id: &str) {
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default);
        }
    }

    /// Subscribe to `channel_id`'s ops, after what was admitted before
    /// ([`subscriptions::admitted_ops`]).
    fn subscribe_ops(&self, channel_id: &str, peer_id: u64) -> mpsc::UnboundedReceiver<OpEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        subscriptions::admitted_ops(&self.broker_channels, channel_id, &tx);
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .op
                .push(OpSub { peer_id, tx });
        }
        rx
    }

    fn subscribe_presence(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<PresenceEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .presence
                .push(PresenceSub { peer_id, tx });
        }
        rx
    }

    fn subscribe_context(
        &self,
        channel_id: &str,
        peer_id: u64,
        native: Option<NativeContext>,
    ) -> mpsc::UnboundedReceiver<ContextEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(native) = native.as_ref() {
            let _ = tx.send(ContextEnvelope::Snapshot {
                collaborators: native.roster.clone(),
                permissions: Vec::new(),
            });
        }
        // Who the app's history names for changes an agent's own key signed.
        let history_agents = self.history_agents_of(channel_id);
        if !history_agents.is_empty() {
            let _ = tx.send(ContextEnvelope::HistoryAgents(history_agents.clone()));
        }
        let certified = native.is_some();
        let held = tx.clone();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .context
                .push(ContextSub {
                    peer_id,
                    tx,
                    native,
                    private_alias: false,
                    history_agents,
                });
        }
        // The records of its app's trees this device already holds in the
        // room ([`BrokerChannels::tree_records_held`]), such as those it was
        // replayed when it joined, before any window of the app ran: a
        // window learns those only from here. Read after the subscription
        // is in place, so a record admitted meanwhile is told either here
        // or live (the shell announces a record once it is admitted).
        subscriptions::held_tree_records(&self.broker_channels, channel_id, &held);
        if certified {
            // A new admitted session brings its own certificate expiry: the
            // policy turn re-checks, and from then on wakes at that expiry.
            tilo_identity_host::policy_change::announce();
            let room = CrdtRoute::from_channel_key(channel_id).cluster_id;
            self.publish_certified_people(&room);
        }
        rx
    }

    fn alias_private_context(&self, channel: &str, alias: &str, peer: u64) {
        if let Ok(mut channels) = self.channels.lock() {
            let subscriber = channels
                .get(channel)
                .and_then(|entry| {
                    entry
                        .context
                        .iter()
                        .rev()
                        .find(|subscriber| subscriber.peer_id == peer)
                })
                .map(|subscriber| ContextSub {
                    peer_id: peer,
                    tx: subscriber.tx.clone(),
                    native: subscriber.native.clone(),
                    private_alias: true,
                    history_agents: subscriber.history_agents.clone(),
                });
            if let Some(subscriber) = subscriber {
                let context = &mut channels.entry(alias.to_owned()).or_default().context;
                context.retain(|entry| entry.peer_id != peer && !entry.tx.is_closed());
                context.push(subscriber);
            }
        }
    }

    /// Complete room membership for one retained native receiver. This is
    /// membership metadata, not online presence: it mints no pointer, caret,
    /// arrival frame, or transport permission.
    fn native_membership_roster(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<Vec<ui::collaboration::Collaborator>, String> {
        let now = self.policy_now_ns()? / 1_000_000_000;
        let (agents, people) = self
            .membership
            .as_ref()
            .ok_or("native context has no membership authority")?
            .decide_in_room(&route.cluster_id, |workspace| {
                let workspace = workspace.ok_or("native context workspace is unavailable")?;
                let mut agents = workspace.agent_members_at(now);
                agents.sort_by(|left, right| left.0.cmp(&right.0));
                let mut people: Vec<String> = workspace
                    .roster()
                    .into_iter()
                    .map(|(person, _)| person)
                    .filter(|person| !workspace.is_blocked(person))
                    .collect();
                people.sort();
                // Presence may supply a label, but only owner-backed
                // membership supplies the identity. An offline member
                // remains a member.
                let people = people
                    .into_iter()
                    .map(|person| {
                        let name = workspace
                            .collaborators
                            .iter()
                            .find(|row| row.id == person)
                            .map_or_else(|| person.clone(), |row| row.name.clone());
                        let joined = workspace.policy.joined_at(&person);
                        (person, name, joined)
                    })
                    .collect::<Vec<_>>();
                Ok::<_, &str>((agents, people))
            })??;
        let mut collaborators =
            Vec::with_capacity(agents.len().saturating_add(people.len()).saturating_add(1));
        if admitted.agent_id.is_none() {
            collaborators.push(ui::collaboration::Collaborator {
                id: Self::participant_id(admitted),
                name: admitted
                    .display_handle
                    .clone()
                    .unwrap_or_else(|| admitted.principal_id.clone()),
                colour: None,
                is_agent: false,
                is_self: true,
                joined_at: people
                    .iter()
                    .find(|(person, _, _)| *person == admitted.principal_id)
                    .and_then(|(_, _, joined)| *joined),
            });
        }
        for (person, name, joined_at) in people {
            if admitted.agent_id.is_none() && person == admitted.principal_id {
                continue;
            }
            collaborators.push(ui::collaboration::Collaborator {
                id: person,
                name,
                colour: None,
                is_agent: false,
                is_self: false,
                joined_at,
            });
        }
        for (agent, _) in agents {
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                agent: agent.clone(),
            }
            .validate()
            .map_err(|_| "native context contains a noncanonical agent identity")?;
            let is_self = admitted.agent_id.as_deref() == Some(agent.as_str());
            collaborators.push(ui::collaboration::Collaborator {
                id: format!("agent:{agent}"),
                name: agent,
                colour: None,
                is_agent: true,
                is_self,
                joined_at: None,
            });
        }
        if collaborators.iter().filter(|row| row.is_self).count() != 1 {
            return Err("native context has no exact authenticated self member".into());
        }
        Ok(collaborators)
    }

    /// Complete certified membership currently backing native Context
    /// subscribers, normalized for host chrome (which has no app-local
    /// `is_self`). Conflicting certified snapshots fail closed to empty.
    /// Resolve an exact certified participant to its canonical workspace key.
    /// Both the native roster and current owner-backed membership must agree.
    pub fn certified_agent_identity(
        &self,
        room: &str,
        actor: &str,
    ) -> Option<ui::collaboration::CertifiedAgentIdentity> {
        if !self
            .certified_people_in_room(room)
            .iter()
            .any(|row| row.id == actor && row.is_agent)
        {
            return None;
        }
        let now = self.policy_now_ns().ok()? / 1_000_000_000;
        self.membership
            .as_ref()?
            .decide_in_room(room, |workspace| {
                workspace?
                    .agent_members_at(now)
                    .into_iter()
                    .find_map(|(agent, _)| {
                        let participant_id = format!("agent:{agent}");
                        (participant_id == actor).then_some(
                            ui::collaboration::CertifiedAgentIdentity {
                                participant_id,
                                agent_key: agent,
                            },
                        )
                    })
            })
            .ok()
            .flatten()
    }

    pub(crate) fn certified_people_in_room(
        &self,
        room: &str,
    ) -> Vec<ui::collaboration::Collaborator> {
        let Ok(channels) = self.channels.lock() else {
            return Vec::new();
        };
        let mut people =
            std::collections::BTreeMap::<String, ui::collaboration::Collaborator>::new();
        for (channel, subscribers) in channels.iter() {
            let route = CrdtRoute::from_channel_key(channel);
            if route.cluster_id != room {
                continue;
            }
            for native in subscribers
                .context
                .iter()
                .filter_map(|subscriber| subscriber.native.as_ref())
            {
                for participant in &native.roster {
                    let mut participant = participant.clone();
                    participant.is_self = false;
                    if let Some(held) = people.get(&participant.id) {
                        if held.name != participant.name
                            || held.packed_colour() != participant.packed_colour()
                            || held.is_agent != participant.is_agent
                        {
                            return Vec::new();
                        }
                    } else {
                        people.insert(participant.id.clone(), participant);
                    }
                }
            }
        }
        people.into_values().collect()
    }

    pub(crate) fn certified_people_for_shell(
        &self,
    ) -> Vec<(String, Vec<ui::collaboration::Collaborator>)> {
        let rooms = self
            .channels
            .lock()
            .map(|channels| {
                channels
                    .iter()
                    .filter(|(_, subscribers)| {
                        subscribers
                            .context
                            .iter()
                            .any(|subscriber| subscriber.native.is_some())
                    })
                    .map(|(channel, _)| CrdtRoute::from_channel_key(channel).cluster_id)
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        rooms
            .into_iter()
            .map(|room| {
                let people = self.certified_people_in_room(&room);
                (room, people)
            })
            .collect()
    }

    fn native_roster_for_subscriber(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<Option<NativeContext>, String> {
        let Some(owner) = self.services.workspace_memberships.as_ref() else {
            return Ok(None);
        };
        if !owner.requires_room(&route.cluster_id) {
            return Ok(None);
        }
        let current = self
            .revalidate_admission(route, admitted)
            .map_err(|error| error.to_string())?;
        let roster = self.native_membership_roster(route, &current)?;
        Ok(Some(NativeContext {
            admitted: current,
            roster,
        }))
    }

    fn publish_certified_people(&self, room: &str) {
        if let Some(fanout) = self.fanout.upgrade() {
            fanout.publish_certified_people(room);
        }
    }

    /// The delegated agents of the room `channel` belongs to whose person's
    /// root signed the delegation for exactly that device, as the host's
    /// authority verified them. Presentation only: history names who wrote
    /// from it, and nothing is admitted by it.
    fn history_agents_of(&self, channel: &str) -> Vec<collab_types::history::RosterAgent> {
        let room = CrdtRoute::from_channel_key(channel).cluster_id;
        self.membership
            .as_ref()
            .map(|membership| membership.history_agents(&room))
            .unwrap_or_default()
    }

    /// Tell every context receiver whose room's verified agents changed
    /// since it was last told. The authority is read before the channel
    /// lock is taken, once per room.
    fn refresh_history_agents(&self) {
        let Ok(channels) = self.channels.lock() else {
            return;
        };
        let rooms = channels
            .iter()
            .filter(|(_, subscribers)| !subscribers.context.is_empty())
            .map(|(channel, _)| CrdtRoute::from_channel_key(channel).cluster_id)
            .collect::<std::collections::BTreeSet<_>>();
        drop(channels);
        let Some(membership) = self.membership.as_ref() else {
            return;
        };
        let agents = rooms
            .into_iter()
            .map(|room| {
                let agents = membership.history_agents(&room);
                (room, agents)
            })
            .collect::<HashMap<_, _>>();
        let Ok(mut channels) = self.channels.lock() else {
            return;
        };
        for (channel, subscribers) in channels.iter_mut() {
            let room = CrdtRoute::from_channel_key(channel).cluster_id;
            let Some(current) = agents.get(&room) else {
                continue;
            };
            subscribers.context.retain_mut(|subscriber| {
                if subscriber.history_agents == *current {
                    return true;
                }
                if subscriber
                    .tx
                    .send(ContextEnvelope::HistoryAgents(current.clone()))
                    .is_err()
                {
                    return false;
                }
                subscriber.history_agents.clone_from(current);
                true
            });
        }
    }

    fn refresh_workspace_context_services(&self) -> Result<(), String> {
        fn same_roster(
            left: &[ui::collaboration::Collaborator],
            right: &[ui::collaboration::Collaborator],
        ) -> bool {
            left.len() == right.len()
                && left.iter().zip(right).all(|(left, right)| {
                    left.id == right.id
                        && left.name == right.name
                        && left.packed_colour() == right.packed_colour()
                        && left.is_agent == right.is_agent
                        && left.is_self == right.is_self
                })
        }

        let targets = self
            .channels
            .lock()
            .map_err(|_| "native context subscriptions unavailable")?
            .iter()
            .flat_map(|(channel, subscribers)| {
                subscribers
                    .context
                    .iter()
                    .filter(|subscriber| !subscriber.private_alias)
                    .filter_map(|subscriber| {
                        subscriber.native.as_ref().map(|native| {
                            (channel.clone(), subscriber.peer_id, native.admitted.clone())
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for (channel, peer_id, admitted) in targets {
            let route = CrdtRoute::from_channel_key(&channel);
            let Ok(current) = self.revalidate_admission(&route, &admitted) else {
                let mut channels = self
                    .channels
                    .lock()
                    .map_err(|_| "native context subscriptions unavailable")?;
                if let Some(subscribers) = channels.get_mut(&channel) {
                    subscribers.context.retain(|subscriber| {
                        subscriber.peer_id != peer_id
                            || subscriber
                                .native
                                .as_ref()
                                .is_none_or(|native| native.admitted != admitted)
                    });
                }
                drop(channels);
                self.publish_certified_people(&route.cluster_id);
                continue;
            };
            let roster = self.native_membership_roster(&route, &current)?;
            let mut channels = self
                .channels
                .lock()
                .map_err(|_| "native context subscriptions unavailable")?;
            let mut changed = false;
            if let Some(subscribers) = channels.get_mut(&channel) {
                subscribers.context.retain_mut(|subscriber| {
                    let Some(native) = subscriber.native.as_mut() else {
                        return true;
                    };
                    if subscriber.peer_id != peer_id || native.admitted != admitted {
                        return true;
                    }
                    if same_roster(&native.roster, &roster) {
                        return true;
                    }
                    changed = true;
                    if subscriber
                        .tx
                        .send(ContextEnvelope::Snapshot {
                            collaborators: roster.clone(),
                            permissions: Vec::new(),
                        })
                        .is_err()
                    {
                        return false;
                    }
                    native.roster.clone_from(&roster);
                    true
                });
            }
            drop(channels);
            if changed {
                self.publish_certified_people(&route.cluster_id);
            }
        }
        Ok(())
    }

    /// Re-check every membership surface and native context subscriber,
    /// and register with
    /// [`crate::policy_schedule::announce_at`] the first instant at
    /// which a time-bound input they read (a capability's expiry, a
    /// subscriber certificate's validity) changes the answer. Returns whether
    /// the next re-check must come on the one-second clock instead of on an
    /// announcement: an input nothing announces, or a check that failed.
    fn refresh_policy_surfaces(self: &Arc<Self>) -> bool {
        // A room's verified agents change with its governance, which every
        // host announces; apps' history follows on the same turn.
        self.refresh_history_agents();
        if self.services.workspace_memberships.is_none() || self.membership.is_none() {
            // No membership surface, and no native context subscriber can
            // be admitted: nothing here changes with policy.
            return false;
        }
        // Sampled before anything is read: an expiry between this sample
        // and a read is registered as well, so none is skipped.
        let before = self.policy_now_ns();
        let surface_failed = self
            .refresh_workspace_membership_services()
            .inspect_err(|error| {
                tracing::debug!(%error, "native membership surface could not be refreshed");
            })
            .is_err();
        let context_failed = self
            .refresh_workspace_context_services()
            .inspect_err(|error| {
                tracing::debug!(%error, "native membership context could not be refreshed");
            })
            .is_err();
        let clocked = surface_failed || context_failed;
        let announced = self
            .membership
            .as_ref()
            .is_some_and(|membership| membership.announces_policy_changes())
            && tilo_identity_host::policy_change::timers_reliable();
        let Ok(before) = before else {
            return true;
        };
        if clocked || !announced {
            return true;
        }
        let Ok((next, reads_fleet)) = self.next_policy_change(before) else {
            return true;
        };
        if reads_fleet && !tilo_identity_host::policy_change::identity_files_watched() {
            // Another process may revoke a device in the fleet file; without
            // its watch, the fleet is re-read every second as before.
            return true;
        }
        if let Some(at) = next {
            let Ok(policy_now) = self.policy_now_ns() else {
                return true;
            };
            crate::policy_schedule::announce_at(crate::policy_schedule::wall_instant(
                at,
                policy_now,
                tilo_clock::unix_now_ns(),
            ));
        }
        false
    }

    /// The first policy instant (Unix nanoseconds) after `after` at which a
    /// membership surface or a native context subscriber can be answered
    /// differently with nothing written, and whether a subscriber's
    /// revalidation reads the device fleet.
    fn next_policy_change(&self, after: u64) -> Result<(Option<u64>, bool), String> {
        let capabilities = match self.membership.as_ref() {
            Some(membership) => membership
                .decide_in_tree(|tree| tree.next_grant_expiry_after(after / 1_000_000_000))?
                .map(|seconds| seconds.saturating_mul(1_000_000_000)),
            None => None,
        };
        let fleet_authority = self.certificate_authority.get().is_some();
        let mut reads_fleet = false;
        let certificates = self
            .channels
            .lock()
            .map_err(|_| "native context subscriptions unavailable")?
            .values()
            .flat_map(|subscribers| subscribers.context.iter())
            .filter_map(|subscriber| subscriber.native.as_ref()?.admitted.certificate.as_ref())
            .filter_map(|certificate| {
                reads_fleet |= fleet_authority;
                // Not yet valid until issued; no longer valid once expired.
                [Some(certificate.issued_at_ns), certificate.expires_at_ns]
                    .into_iter()
                    .flatten()
                    .filter(|at| *at > after)
                    .min()
            })
            .min();
        Ok((
            capabilities.into_iter().chain(certificates).min(),
            reads_fleet,
        ))
    }

    /// Ask one local subscriber of every record channel in `cluster_id` for
    /// its document snapshot, on behalf of no local connection: how another
    /// device that just came back catches up on what it missed. Returns how
    /// many channels were asked.
    pub fn request_room_snapshots(&self, cluster_id: &str) -> usize {
        let channels: Vec<String> = self
            .channels
            .lock()
            .map(|channels| {
                channels
                    .keys()
                    .filter(|key| {
                        let route = CrdtRoute::from_channel_key(key);
                        route.cluster_id == cluster_id && !route.record_id.is_empty()
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        channels
            .iter()
            .filter(|channel| self.request_document_snapshot(channel, u64::MAX))
            .count()
    }

    /// Direct a late-join snapshot request to one established subscriber on
    /// the exact route. Lowest broker peer id makes selection deterministic;
    /// the requester is excluded so it never snapshots its own empty state.
    fn request_document_snapshot(&self, channel_id: &str, requester: u64) -> bool {
        let Ok(mut channels) = self.channels.lock() else {
            return false;
        };
        let Some(channel) = channels.get_mut(channel_id) else {
            return false;
        };
        channel.context.sort_by_key(|subscriber| subscriber.peer_id);
        let Some(index) = channel
            .context
            .iter()
            .position(|subscriber| subscriber.peer_id != requester)
        else {
            return false;
        };
        if channel.context.get(index).is_some_and(|subscriber| {
            subscriber
                .tx
                .send(ContextEnvelope::SnapshotRequested)
                .is_ok()
        }) {
            true
        } else {
            channel.context.remove(index);
            false
        }
    }
}

/// Postcard-encode [`PresenceKey::pointer_default`]; cheap enough not
/// to cache.
fn default_pointer_key_bytes() -> Vec<u8> {
    postcard::to_stdvec(&PresenceKey::pointer_default()).unwrap_or_default()
}

// Server implementations

/// Root capability server. One instance per connected RPC session.
struct CapnpCollabService {
    worker: Option<WorkerBinding>,
    /// The worker registration of a connection its launcher authenticated,
    /// minted at connect for the app the shell launched: each route of
    /// that app the connection joins as its person is one of its worker
    /// routes ([`host_workers`]).
    app_worker: Option<(uuid::Uuid, String)>,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    /// Peer id assigned at accept time. Fanout uses it to skip the
    /// author when replaying its own frames back at itself.
    peer_id: u64,
    lease: Rc<ConnectionLease>,
    /// The routes this connection holds ([`route_holds`]).
    holds: route_holds::ConnectionHolds,
}

struct ConnectionLease {
    local_media: Option<Arc<native_media::LocalAppConnection>>,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
}

/// Disconnect even when the `LocalSet` cancels a connection instead of polling it
/// to EOF. capnp-rpc's first disconnect poll releases exported capabilities and
/// pipelines synchronously; dropping `RpcSystem` alone does not release those
/// cycles. No peer response or additional shutdown time is needed here.
struct DisconnectOnDrop<F: std::future::Future>(Option<F>);

impl<F: std::future::Future> Drop for DisconnectOnDrop<F> {
    fn drop(&mut self) {
        if let Some(disconnector) = self.0.take() {
            let _ = futures::FutureExt::now_or_never(disconnector);
        }
    }
}

struct ConnectionExit {
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
}

impl Drop for ConnectionExit {
    fn drop(&mut self) {
        let holds = self.state.connection_holds(self.peer_id);
        self.state.remove_connection(self.peer_id);
        for hold in holds {
            self.directories.remove(hold);
        }
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if let Some(local) = self.local_media.as_ref() {
            local.stop();
        }
        let holds = self.state.connection_holds(self.peer_id);
        self.state.remove_connection(self.peer_id);
        for hold in holds {
            self.directories.remove(hold);
        }
    }
}

impl collab_service::Server for CapnpCollabService {
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered join admission: identity, route, custody and channel handle"
    )]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the route hold moves into the channel handle it lives as long as; the join never drops it"
    )]
    fn join_channel(
        self: Rc<Self>,
        params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let reader = params.get()?;
            let channel_id = reader
                .get_channel_id()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("channelId not utf8: {e}")))?
                .to_owned();
            let identity = reader.get_identity()?;
            let route = CrdtRoute::from_channel_key(&channel_id);
            let mut verified_identity = verify_join_identity_with_host_scope(
                &route,
                identity,
                state.membership.as_ref(),
                state
                    .trusted_devices
                    .as_ref()
                    .map(|devices| devices.as_slice()),
                state.certificate_authority.get().map(Arc::as_ref),
                self.worker
                    .as_ref()
                    .and_then(|worker| worker.host_scope.as_ref()),
            )
            .map_err(|error| {
                capnp::Error::failed(format!("collaboration join identity rejected: {error}"))
            })?;
            if let Some(app) = self.lease.local_media.as_ref()
                && verified_identity
                    .verified
                    .owns_person_room(&route.cluster_id)
            {
                person_record_join(&app.app_id, &route).map_err(capnp::Error::failed)?;
            }
            if let Some(worker) = &self.worker {
                if route != worker.route
                    || verified_identity.verified.user_public_key != worker.root
                    || verified_identity.verified.device_public_key != worker.device
                {
                    return Err(capnp::Error::failed(
                        "worker descriptor route or certificate mismatch".into(),
                    ));
                }
                if let Some(registration) = &worker.registration {
                    if registration.route != route
                        || verified_identity.certificate.as_ref() != Some(&registration.certificate)
                    {
                        return Err(capnp::Error::failed(
                            "registered worker exact certificate changed".into(),
                        ));
                    }
                    verified_identity.worker_registration = Some(registration.clone());
                }
            } else if state.durable_owner_required && !route.app_id.is_empty() {
                return Err(capnp::Error::failed(
                    "application requires host-owned worker descriptor admission".into(),
                ));
            }
            let participant_handle =
                reader.get_participant_handle()?.to_str().map_err(|error| {
                    capnp::Error::failed(format!("participant handle not utf8: {error}"))
                })?;
            let participant_kind = reader.get_participant_kind()?.to_str().map_err(|error| {
                capnp::Error::failed(format!("participant kind not utf8: {error}"))
            })?;
            let record_kind = joined_record_kind(reader.get_record_kind()?)?;
            if (verified_identity.host_scope.is_some()
                || verified_identity.worker_registration.is_some())
                && !matches!(participant_kind, "" | "person")
            {
                return Err(capnp::Error::failed(
                    "host worker cannot delegate an agent identity".into(),
                ));
            }
            match participant_kind {
                "" | "person" => {
                    // A Person handle is presentation, not its certified identity.
                    let participant_handle = participant_handle
                        .trim()
                        .chars()
                        .take(64)
                        .collect::<String>();
                    if !participant_handle.is_empty() {
                        verified_identity.display_handle = Some(participant_handle);
                    }
                }
                "agent" => {
                    // Unlike a display handle, this names the exact workspace
                    // principal. Normalize as enrollment does, then validate
                    // its byte bound; truncation can silently select another
                    // admitted Agent that shares the same display prefix.
                    let participant_handle = participant_handle.trim();
                    // Bound allocation before Unicode case normalization;
                    // every normalized character consumes at least one byte.
                    if participant_handle.chars().take(129).count() > 128 {
                        return Err(capnp::Error::failed(
                            "invalid canonical workspace agent identifier".into(),
                        ));
                    }
                    let participant_handle = participant_handle.to_lowercase();
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                        agent: participant_handle.clone(),
                    }
                    .validate()
                    .map_err(capnp::Error::failed)?;
                    let membership = state.membership.as_ref().ok_or_else(|| {
                        capnp::Error::failed("agent session has no workspace authority".into())
                    })?;
                    let now = state.policy_now_ns().map_err(capnp::Error::failed)? / 1_000_000_000;
                    membership
                        .decide_in_room(&route.cluster_id, |workspace| {
                            admit_agent_delegation_at(
                                workspace,
                                &mut verified_identity,
                                &participant_handle,
                                now,
                            )
                        })
                        .and_then(std::convert::identity)
                        .map_err(|error| {
                            capnp::Error::failed(format!(
                                "agent collaboration join rejected: {error}"
                            ))
                        })?;
                }
                other => {
                    return Err(capnp::Error::failed(format!(
                        "unknown collaboration participant kind `{other}`"
                    )));
                }
            }
            tracing::info!(
                target: "tilo_collab_probe",
                channel = %channel_id,
                peer_id,
                role = ?verified_identity.role,
                principal = %verified_identity.principal_id,
                acting_as = ?verified_identity.agent_id,
                device = ?{
                    collab_types::Ed25519PublicKey::from_bytes(
                        verified_identity.verified.device_public_key,
                    )
                },
                "PROBE join_channel",
            );
            if self.worker.is_none()
                && matches!(participant_kind, "" | "person")
                && let Some(registration) = host_workers::packaged_app_registration(
                    self.app_worker.as_ref(),
                    &route,
                    &verified_identity,
                )
            {
                verified_identity.worker_registration = Some(registration);
            }
            let ensured = channel_id.clone();
            let retained = verified_identity.clone();
            let hold = self.holds.hold(&state, &self.directories, peer_id, &route);
            let hold_id = hold.id();
            let client: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                person_chat_views: RefCell::default(),
                committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                state: state.clone(),
                directories: Rc::clone(&self.directories),
                channel_id,
                peer_id: hold_id,
                connection_id: peer_id,
                verified_identity,
                record_kind,
                lease: Some(Rc::clone(&self.lease)),
                _hold: Some(hold),
            });
            // Refused, the handle and its hold drop here and withdraw again.
            state.retain_host_worker(hold_id, &route, &retained)?;
            state.ensure_channel(&ensured);
            state
                .publisher()
                .local_route(&crate::route::CrdtRoute::from_channel_key(&ensured));
            results.get().set_handle(client);
            Ok(())
        })
    }
}

/// The resource kind a join names for its record: none when empty, and
/// refused unless it is a record kind the toolkit accepts.
fn joined_record_kind(kind: capnp::text::Reader<'_>) -> Result<Option<String>, capnp::Error> {
    let kind = kind
        .to_str()
        .map_err(|error| capnp::Error::failed(format!("record kind not utf8: {error}")))?;
    if kind.is_empty() {
        return Ok(None);
    }
    if !ui::record::RecordKind::new(kind).is_valid() {
        return Err(capnp::Error::failed(format!(
            "invalid record kind `{kind}`"
        )));
    }
    Ok(Some(kind.to_owned()))
}

/// Per-channel handle established by `join_channel`; every subsequent
/// method call inherits its (`channel_id`, `peer_id`, identity) context.
struct CapnpChannelHandle {
    person_chat_views: RefCell<person_chat::SelectedViews>,
    committed_read: Rc<RefCell<committed_document::Slot>>,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    channel_id: String,
    /// The route hold this channel publishes and works as ([`route_holds`]).
    peer_id: u64,
    /// The connection: its transport identity for document fanout,
    /// presence, media and transfers.
    connection_id: u64,
    verified_identity: AdmittedJoinIdentity,
    /// The resource kind the app named for this channel's record, if any.
    record_kind: Option<String>,
    /// Production handles share the root RPC connection lease. Direct unit
    /// fixtures use `None` and retain the legacy one-handle cleanup behavior.
    lease: Option<Rc<ConnectionLease>>,
    /// Production handles share their route's hold, kept for its drop;
    /// fixtures use `None`.
    _hold: Option<Rc<route_holds::RouteHold>>,
}

impl Drop for CapnpChannelHandle {
    fn drop(&mut self) {
        if self.lease.is_none() {
            self.state.remove_connection(self.peer_id);
            self.directories.remove(self.peer_id);
        }
    }
}

impl CapnpChannelHandle {
    /// A record-scoped connection names its record inside the channel id
    /// (the SDK joins `<cluster>#record:<id>`), parsed back out here.
    fn route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn realtime_context(&self) -> Result<crate::services::RealtimeRequestContext, capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(capnp::Error::failed(
                "realtime continuity requires an exact certified app/workspace route".into(),
            ));
        }
        Ok(crate::services::RealtimeRequestContext::authenticated(
            self.connection_id,
            route.app_id,
            route.cluster_id,
        ))
    }
}

/// The `AppSettings` capability of the app `route` certifies: its own
/// entries and the shared sections, in the host's settings source, for as
/// long as the channel stays admitted.
fn app_settings_capability(
    state: &Arc<CapnpBrokerState>,
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
) -> Result<runtime_collab::collab_capnp::app_settings::Client, capnp::Error> {
    if route.app_id.is_empty() {
        return Err(capnp::Error::failed(
            "settings belong to a certified app route".into(),
        ));
    }
    let Some(source) = state.services.settings.clone() else {
        return Err(capnp::Error::failed(
            "this host keeps no settings for apps".into(),
        ));
    };
    let app_id = route.app_id.clone();
    let display = private_display_control(state, &route, &identity);
    // Personal vocabulary stays on this device. Admission to a workspace is
    // insufficient: only the host's private local worker descriptor of the
    // authenticated person receives the read/edit projection.
    let dictionary_owner = (identity.host_scope.is_some()
        && identity.agent_id.is_none()
        && !identity.verified.capped
        && !identity
            .certificate
            .as_ref()
            .is_some_and(|cert| cert.capabilities.foreign))
    .then_some(identity.verified.user_public_key);
    let reach = settings_reach(state, &route, &identity);
    let state = Arc::clone(state);
    let revalidate: runtime_collab::settings::host::Revalidate =
        Arc::new(move || state.revalidate_admission(&route, &identity).map(|_| ()));
    Ok(capnp_rpc::new_client(
        runtime_collab::settings::host::AppSettingsServer::new(app_id, source, revalidate)
            .with_reach(reach)
            .with_dictionary_owner(dictionary_owner)
            .with_display(display),
    ))
}

/// What the app `route` certifies reaches of the settings: every section
/// for the device's settings app, when its signed manifest asks
/// (`settings_store::app_scope::SYSTEM_SETTINGS_CAPABILITY`) and the image
/// grants it ([`settings_store::app_scope::SYSTEM_SETTINGS_APPS`]); its own
/// entries otherwise. An agent working in an app never gets more than the
/// app's own entries.
fn settings_reach(
    state: &Arc<CapnpBrokerState>,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> settings_store::app_scope::Reach {
    use settings_store::app_scope::Reach;
    if identity.agent_id.is_some() {
        return Reach::App;
    }
    state
        .services
        .launcher
        .as_ref()
        .and_then(|launcher| {
            launcher
                .applications()
                .into_iter()
                .find(|app| app.package_id == route.app_id && !app.unconfined)
        })
        .map_or(Reach::App, |app| {
            Reach::of(&route.app_id, &app.capabilities)
        })
}

fn private_display_control(
    state: &Arc<CapnpBrokerState>,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Option<runtime_collab::settings::host::DisplayControl> {
    if route.app_id != "so.tilo.settings" || identity.agent_id.is_some() {
        return None;
    }
    let registration = identity.worker_registration.clone()?;
    if registration.route != *route
        || identity.certificate.as_ref() != Some(&registration.certificate)
    {
        return None;
    }
    let service = state.services.display_control.clone()?;
    let state = state.clone();
    let route = route.clone();
    let identity = identity.clone();
    Some(Arc::new(move |command, origin| {
        if !state.is_accepting() {
            return Err("The display host has closed.".into());
        }
        state
            .revalidate_admission(&route, &identity)
            .map_err(|error| error.to_string())?;
        if !state
            .host_workers
            .lock()
            .map_err(|_| "Settings worker registry is unavailable.")?
            .values()
            .any(|worker| worker.worker_registration.as_ref() == Some(&registration))
        {
            return Err("The admitted Settings worker has closed.".into());
        }
        if command.len() > 64 * 1024 {
            return Err("The display request exceeds its wire bound.".into());
        }
        let certificate = match origin {
            runtime_collab::settings::DisplayOrigin::LocalHost => registration.certificate.clone(),
            runtime_collab::settings::DisplayOrigin::Published(bytes) => {
                let invocation = tilo_actions::InvocationId(uuid::Uuid::from_bytes(bytes));
                let (peer, original) = {
                    let workers = state
                        .host_workers
                        .lock()
                        .map_err(|_| "Settings worker registry is unavailable.")?;
                    let peer = workers
                        .peer_of(&registration)
                        .ok_or("The admitted Settings worker has closed.")?;
                    drop(workers);
                    let pending = state
                        .pending_invocations
                        .lock()
                        .map_err(|_| "Original display invocation registry is unavailable.")?;
                    let mut matching = pending.values().filter(|value| {
                        value.peer_id == peer && value.update.invocation() == invocation
                    });
                    let entry = matching
                        .next()
                        .ok_or("The original display invocation is not retained.")?;
                    if matching.next().is_some() || entry.cancellation_requested_at.is_some() {
                        return Err("The original display invocation is no longer live.".into());
                    }
                    let original = entry
                        .original
                        .clone()
                        .ok_or("Original display authority is unavailable.")?;
                    drop(pending);
                    (peer, original)
                };
                original.display_caller(
                    &state,
                    peer,
                    &registration,
                    serde_json::from_slice::<settings_store::display_control::DisplayCommand>(
                        command,
                    )
                    .map_err(|_| "The display command is invalid.")?
                    .mutates(),
                )?
            }
        };
        service.command(
            &crate::services::DisplayControlContext {
                room: route.cluster_id.clone(),
                root: certificate.user_public_key,
                device: certificate.device_public_key,
            },
            command,
        )
    }))
}

/// The `LiveActivity` capability of the app `route` certifies, over the
/// host's board, for as long as the channel stays admitted.
fn live_activity_capability(
    state: &Arc<CapnpBrokerState>,
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
) -> Result<runtime_collab::collab_capnp::live_activity::Client, capnp::Error> {
    if route.app_id.is_empty() {
        return Err(capnp::Error::failed(
            "a live activity belongs to a certified app route".into(),
        ));
    }
    let Some(board) = state.services.live_activities.clone() else {
        return Err(capnp::Error::failed(
            "this host presents no live activities".into(),
        ));
    };
    let app_id = route.app_id.clone();
    let state = Arc::clone(state);
    let revalidate: runtime_collab::live_activity::Revalidate =
        Arc::new(move || state.revalidate_admission(&route, &identity).map(|_| ()));
    Ok(capnp_rpc::new_client(
        runtime_collab::live_activity::LiveActivityServer::new(app_id, board, revalidate),
    ))
}

fn delegate_realtime_command(
    services: &HostServices,
    context: &crate::services::RealtimeRequestContext,
    claimed_app: &str,
    command: &[u8],
) -> Result<Vec<u8>, capnp::Error> {
    if claimed_app != context.app_id() {
        return Err(capnp::Error::failed(
            "realtime app id must match the certified connection route".into(),
        ));
    }
    let service = services.realtime.as_ref().ok_or_else(|| {
        capnp::Error::failed("realtime continuity is unsupported by this host".into())
    })?;
    service
        .command(context, command)
        .map_err(capnp::Error::failed)
}

/// The only application that may carry Wi-Fi credentials between devices.
const WIFI_CONTINUITY_APP_ID: &str = "so.tilo.settings";
/// The only application that may send files to nearby devices.
const NEARBY_TRANSFER_APP_ID: &str = "so.tilo.file-explorer";

fn delegate_wifi_continuity_command(
    services: &HostServices,
    route_app: &str,
    claimed_app: &str,
    command: &[u8],
) -> Result<Vec<u8>, capnp::Error> {
    if route_app != WIFI_CONTINUITY_APP_ID {
        return Err(capnp::Error::failed(
            "Wi-Fi continuity is restricted to the certified Settings application".into(),
        ));
    }
    if claimed_app != route_app {
        return Err(capnp::Error::failed(
            "Wi-Fi continuity app id must match the certified connection route".into(),
        ));
    }
    if command.len() > runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES {
        return Err(capnp::Error::failed(
            "Wi-Fi continuity command exceeds the local protocol bound".into(),
        ));
    }
    let command = match serde_json::from_slice::<runtime_collab::WifiContinuityCommand>(command) {
        Ok(command) if command.valid() => command,
        Ok(_) => {
            return Err(capnp::Error::failed(
                "Wi-Fi continuity command is structurally invalid".into(),
            ));
        }
        Err(error) => return Err(capnp::Error::failed(error.to_string())),
    };
    let service = services.wifi_continuity.as_ref().ok_or_else(|| {
        capnp::Error::failed("Wi-Fi continuity is unsupported by this host".into())
    })?;
    match serde_json::to_vec(&service.command(command)) {
        Ok(reply) if reply.len() <= runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES => Ok(reply),
        Ok(_) => Err(capnp::Error::failed(
            "Wi-Fi continuity reply exceeds the local protocol bound".into(),
        )),
        Err(error) => Err(capnp::Error::failed(error.to_string())),
    }
}

fn delegate_browser_service_session_command(
    services: &HostServices,
    context: &crate::services::BrowserServiceSessionRequestContext,
    bytes: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, capnp::Error> {
    let refused = || capnp::Error::failed("private service-session request refused".into());
    if context.app_id() != "so.tilo.browser"
        || context.workspace_id().is_empty()
        || context.person().is_empty()
    {
        return Err(refused());
    }
    context.revalidate().map_err(|_| refused())?;
    let command =
        runtime_collab::BrowserServiceSessionCommand::decode(bytes).map_err(|_| refused())?;
    let service = services
        .browser_service_sessions
        .as_ref()
        .ok_or_else(refused)?;
    service
        .authorize_command(context, &command)
        .map_err(|_| refused())?;
    let reply = service
        .command(context, command.clone())
        .map_err(|_| refused())?;
    context.revalidate().map_err(|_| refused())?;
    service
        .authorize_command(context, &command)
        .map_err(|_| refused())?;
    if !reply.matches(&command) {
        return Err(refused());
    }
    if let runtime_collab::BrowserServiceSessionReply::Resolved { session } = &reply
        && session.identity_public_key != context.identity_public_key()
    {
        return Err(refused());
    }
    reply.encode().map_err(|_| refused())
}

/// The wait for Nearby Send's next change past `after` for the connection
/// `context`: only the certified Files route in a workspace may wait, as only
/// it may command.
fn nearby_transfer_change(
    services: &HostServices,
    context: &crate::services::NearbyTransferRequestContext,
    after: u64,
) -> Result<crate::services::NearbyTransferChange, capnp::Error> {
    if context.app_id() != NEARBY_TRANSFER_APP_ID || context.workspace_id().is_empty() {
        return Err(capnp::Error::failed(
            "Nearby Send requires the certified Files application route".into(),
        ));
    }
    services
        .nearby_transfer
        .as_ref()
        .and_then(|service| service.changed(context, after))
        .ok_or_else(|| {
            capnp::Error::unimplemented("this host does not say when Nearby Send changes".into())
        })
}

/// The only application that manages the person's speaker groups.
const SPEAKER_APP_ID: &str = "so.tilo.settings";

/// The wait for the speaker groups' next change past `after` for a
/// connection on `route_app`: only the certified Settings route may wait, as
/// only it may command.
fn speaker_change(
    services: &HostServices,
    route_app: &str,
    after: u64,
) -> Result<crate::services::SpeakerChange, capnp::Error> {
    if route_app != SPEAKER_APP_ID {
        return Err(capnp::Error::failed(
            "speaker groups are managed only by the certified Settings application".into(),
        ));
    }
    services
        .speakers
        .as_ref()
        .and_then(|service| service.changed(after))
        .ok_or_else(|| {
            capnp::Error::unimplemented("this host does not say when speaker groups change".into())
        })
}

fn delegate_nearby_transfer_command(
    services: &HostServices,
    context: &crate::services::NearbyTransferRequestContext,
    claimed_app: &str,
    command: &[u8],
) -> Result<Vec<u8>, capnp::Error> {
    if context.app_id() != NEARBY_TRANSFER_APP_ID || context.workspace_id().is_empty() {
        return Err(capnp::Error::failed(
            "Nearby Send requires the certified Files application route".into(),
        ));
    }
    if claimed_app != context.app_id() {
        return Err(capnp::Error::failed(
            "Nearby Send app id must match the certified connection route".into(),
        ));
    }
    if command.len() > runtime_collab::MAX_NEARBY_TRANSFER_WIRE_BYTES {
        return Err(capnp::Error::failed(
            "Nearby Send command exceeds the local protocol bound".into(),
        ));
    }
    let command = match serde_json::from_slice::<runtime_collab::NearbyTransferCommand>(command) {
        Ok(command) if command.valid() => command,
        Ok(_) => {
            return Err(capnp::Error::failed(
                "Nearby Send command is structurally invalid".into(),
            ));
        }
        Err(error) => return Err(capnp::Error::failed(error.to_string())),
    };
    let service = services
        .nearby_transfer
        .as_ref()
        .ok_or_else(|| capnp::Error::failed("Nearby Send is unsupported by this host".into()))?;
    let reply = service
        .command(context, command)
        .map_err(capnp::Error::failed)?;
    if !reply.valid() {
        return Err(capnp::Error::failed(
            "Nearby Send produced an invalid bounded reply".into(),
        ));
    }
    match serde_json::to_vec(&reply) {
        Ok(bytes) if bytes.len() <= runtime_collab::MAX_NEARBY_TRANSFER_WIRE_BYTES => Ok(bytes),
        Ok(_) => Err(capnp::Error::failed(
            "Nearby Send reply exceeds the local protocol bound".into(),
        )),
        Err(error) => Err(capnp::Error::failed(error.to_string())),
    }
}

/// Answer a remote-action call with `message` at once.
fn reply_frame(
    message: tilo_actions::remote::RemoteActionMessage,
    mut results: channel_handle::RemoteActionResults,
) -> Promise<(), capnp::Error> {
    match tilo_actions::remote::RemoteActionFrame::new(message).encode() {
        Ok(bytes) => {
            results.get().set_frame(&bytes);
            Promise::ok(())
        }
        Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
    }
}

impl CapnpChannelHandle {
    /// The host's record-content service, for a channel still admitted.
    fn record_content(
        &self,
    ) -> Result<(CrdtRoute, Arc<crate::record_content::RouteContent>), capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(capnp::Error::failed(
                "record content belongs to a certified app route".into(),
            ));
        }
        let content = self.state.services.record_content.clone().ok_or_else(|| {
            capnp::Error::unimplemented("this host keeps no record content".into())
        })?;
        Ok((route, content))
    }
}

/// News of content for one channel's record, pulled one per `read`:
/// content that arrived, or where a want of it has got to.
struct BlobStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<crate::record_content::ContentNews>>>,
}

impl blob_stream::Server for BlobStreamServer {
    fn read(
        self: Rc<Self>,
        _params: blob_stream::ReadParams,
        mut results: blob_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut guard = rx.lock().await;
            let Some(arrival) = guard.recv().await else {
                return Err(capnp::Error::disconnected(
                    "record content stream closed".into(),
                ));
            };
            drop(guard);
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut reply = results.get();
            reply.set_hash(&arrival.hash);
            reply.set_bytes(&arrival.bytes);
            reply.set_state(match arrival.state {
                tilo_content::ContentState::Resolved => runtime_collab::BlobState::Arrived,
                tilo_content::ContentState::Pending => runtime_collab::BlobState::Fetching,
                tilo_content::ContentState::Waiting => runtime_collab::BlobState::Waiting,
                tilo_content::ContentState::Unavailable => runtime_collab::BlobState::Unavailable,
            });
            Ok(())
        })
    }
}

impl channel_handle::Server for CapnpChannelHandle {
    /// Resolves once this route's own channel changed since `after`. The
    /// channel is derived from the admitted route, so an app follows only
    /// its own records' channels, and learns nothing but that one changed.
    fn channel_keys_changed(
        self: Rc<Self>,
        params: channel_handle::ChannelKeysChangedParams,
        mut results: channel_handle::ChannelKeysChangedResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Promise::err(capnp::Error::failed(
                "channel keys belong to a certified app route".into(),
            ));
        }
        let Some(keys) = self.state.services.channel_keys.clone() else {
            return Promise::err(capnp::Error::unimplemented(
                "this host has no channel service to follow".into(),
            ));
        };
        let after = match params.get() {
            Ok(params) => params.get_after(),
            Err(error) => return Promise::err(error),
        };
        let channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id);
        Promise::from_future(async move {
            let change = keys.channel_changed(channel, after).await;
            results.get().set_change(change);
            Ok(())
        })
    }

    /// Resolves once what a Nearby Send snapshot would show may have changed
    /// since `after`. Only the certified Files route may wait, the same route
    /// every Nearby command is confined to, and it learns nothing but that
    /// something changed: it asks for a snapshot, which is checked as always.
    fn nearby_transfer_changed(
        self: Rc<Self>,
        params: channel_handle::NearbyTransferChangedParams,
        mut results: channel_handle::NearbyTransferChangedResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let context = crate::services::NearbyTransferRequestContext::authenticated(
            self.connection_id,
            route.app_id.clone(),
            route.cluster_id.clone(),
            route.channel_key(),
            self.verified_identity
                .agent_id
                .is_none()
                .then(|| self.verified_identity.principal_id.clone()),
        );
        let after = match params.get() {
            Ok(params) => params.get_after(),
            Err(error) => return Promise::err(error),
        };
        match nearby_transfer_change(&self.state.services, &context, after) {
            Ok(change) => Promise::from_future(async move {
                results.get().set_change(change.await);
                Ok(())
            }),
            Err(error) => Promise::err(error),
        }
    }

    /// Resolves once the drive list Files is shown may have changed since
    /// `after`. Only the certified Files route may wait, and it learns
    /// nothing but that something changed: it asks the tools service for
    /// the list again.
    fn drives_changed(
        self: Rc<Self>,
        params: channel_handle::DrivesChangedParams,
        mut results: channel_handle::DrivesChangedResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let after = match params.get() {
            Ok(params) => params.get_after(),
            Err(error) => return Promise::err(error),
        };
        match drive_changes::drive_change(&self.state.services, &route.app_id, after) {
            Ok(change) => Promise::from_future(async move {
                results.get().set_change(change.await);
                Ok(())
            }),
            Err(error) => Promise::err(error),
        }
    }

    fn approved_check_reported_progress(
        self: Rc<Self>,
        params: channel_handle::ApprovedCheckReportedProgressParams,
        mut results: channel_handle::ApprovedCheckReportedProgressResults,
    ) -> Promise<(), capnp::Error> {
        let request = params
            .get()
            .and_then(channel_handle::approved_check_reported_progress_params::Reader::get_request)
            .map_err(|error| error.to_string())
            .and_then(runtime_collab::approved_checks::Request::decode);
        let stream = request
            .and_then(|request| approved_checks::context(&self, request))
            .and_then(approved_checks::reported_progress);
        match stream {
            Ok(stream) => {
                results.get().set_stream(stream);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }
    fn approved_check(
        self: Rc<Self>,
        params: channel_handle::ApprovedCheckParams,
        mut results: channel_handle::ApprovedCheckResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params
            .get()
            .and_then(channel_handle::approved_check_params::Reader::get_request)
            .map_err(|error| error.to_string())
            .and_then(runtime_collab::approved_checks::Request::decode)
        {
            Ok(request) => request,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let context = match approved_checks::context(&self, request) {
            Ok(context) => context,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        Promise::from_future(async move {
            let reply = approved_checks::execute(context)
                .await
                .map_err(capnp::Error::failed)?;
            results.get().set_receipt(&reply);
            Ok(())
        })
    }
    fn offer_blob(
        self: Rc<Self>,
        params: channel_handle::OfferBlobParams,
        _results: channel_handle::OfferBlobResults,
    ) -> Promise<(), capnp::Error> {
        let (route, content) = match self.record_content() {
            Ok(found) => found,
            Err(error) => return Promise::err(error),
        };
        Promise::from_future(async move {
            let p = params.get()?;
            let hash = p
                .get_hash()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("offerBlob hash: {e}")))?
                .to_owned();
            let bytes = p.get_bytes()?;
            content
                .offer(&route, &hash, bytes)
                .map_err(capnp::Error::failed)
        })
    }

    fn want_blob(
        self: Rc<Self>,
        params: channel_handle::WantBlobParams,
        _results: channel_handle::WantBlobResults,
    ) -> Promise<(), capnp::Error> {
        let (route, content) = match self.record_content() {
            Ok(found) => found,
            Err(error) => return Promise::err(error),
        };
        Promise::from_future(async move {
            let hash = params
                .get()?
                .get_hash()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("wantBlob hash: {e}")))?
                .to_owned();
            content.want(&route, &hash);
            Ok(())
        })
    }

    fn subscribe_blobs(
        self: Rc<Self>,
        _params: channel_handle::SubscribeBlobsParams,
        mut results: channel_handle::SubscribeBlobsResults,
    ) -> Promise<(), capnp::Error> {
        let (route, content) = match self.record_content() {
            Ok(found) => found,
            Err(error) => return Promise::err(error),
        };
        let client: blob_stream::Client = capnp_rpc::new_client(BlobStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: Rc::new(AsyncMutex::new(content.subscribe(&route))),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    /// The caller-side door to published actions: one remote-action request
    /// frame in, one answer out, as the caller this channel was admitted as.
    #[allow(
        clippy::too_many_lines,
        reason = "one door: admission, an app's grant question and the person's confirmation decide each request in order"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the action host moves into the future that answers the request"
    )]
    fn remote_action(
        self: Rc<Self>,
        params: channel_handle::RemoteActionParams,
        mut results: channel_handle::RemoteActionResults,
    ) -> Promise<(), capnp::Error> {
        use tilo_actions::remote::RemoteActionFrame;
        let original_time = match self.state.registered_policy_time() {
            Ok(time) => time,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let frame = match params
            .get()
            .and_then(channel_handle::remote_action_params::Reader::get_frame)
        {
            Ok(frame) => frame,
            Err(error) => return Promise::err(error),
        };
        let request = match RemoteActionFrame::decode(frame) {
            Ok(request) => request,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let route = self.route();
        let person = current.caller(&route.cluster_id);
        // An app's open of an application is decided as the caller of the
        // invocation it continues, under that invocation's custody, else as
        // the person whose press it follows; an edit an effect submits while
        // running for someone is theirs.
        let (decided, continued_origin) = match current
            .agent_id
            .is_none()
            .then(|| {
                self.state
                    .app_open_caller(self.peer_id, &route.app_id, &person, &request.message)
            })
            .flatten()
        {
            Some(Ok((caller, origin))) => (Some(Ok(caller)), origin),
            Some(Err(refused)) => (Some(Err(refused)), None),
            None => (
                self.state.continued_caller(self.peer_id, &request.message),
                None,
            ),
        };
        let caller = match decided {
            Some(Ok(caller)) => caller,
            Some(Err(refused)) => return reply_frame(*refused, results),
            None if current.agent_id.is_some() => person.clone(),
            None => match self
                .state
                .app_door(&person, &route.app_id, &request.message)
            {
                Ok(caller) => caller,
                Err(refusal) => return reply_frame(*refusal, results),
            },
        };
        if let tilo_actions::remote::RemoteActionMessage::CatalogueRequest { correlation, query } =
            &request.message
            && query.view_resource.is_some()
        {
            return self.resource_view_query(*correlation, query.clone(), results);
        }

        if tilo_actions::remote::refuse(
            &request.message,
            tilo_actions::remote::ActionHostError::UnknownAction,
        )
        .is_none()
        {
            return Promise::err(capnp::Error::failed(
                "only remote-action requests are answered".into(),
            ));
        }
        // An application reaching for another app's action it holds no
        // grant for: the person using it is asked first, or it is refused.
        let consent = match (caller.principal(), &request.message) {
            (
                tilo_actions::remote::CallerPrincipal::App(app_id),
                tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
            ) => match self.state.app_action_consent(&person, app_id, invoke) {
                app_consent::ConsentStep::Serve => None,
                app_consent::ConsentStep::Refuse => {
                    return refused(
                        &request.message,
                        tilo_actions::remote::ActionHostError::NotPermitted,
                        results,
                    );
                }
                app_consent::ConsentStep::Ask(consent) => Some(consent),
            },
            _ => None,
        };
        // A person's answer to what an issue's run asks them is theirs to
        // give on the host's sheet, never the app's.
        let question = match &request.message {
            tilo_actions::remote::RemoteActionMessage::Invoke(invoke) => {
                match self.state.run_question(&route.app_id, &caller, invoke) {
                    run_question::AnswerStep::Serve => None,
                    run_question::AnswerStep::Refuse => {
                        return refused(
                            &request.message,
                            tilo_actions::remote::ActionHostError::NotPermitted,
                            results,
                        );
                    }
                    run_question::AnswerStep::Ask(question) => Some(question),
                }
            }
            _ => None,
        };
        let admitting_person = current.principal_id.clone();
        let host = action_host::ConnectionActionHost {
            state: Arc::clone(&self.state),
            own_peer: self.peer_id,
            origin: continued_origin.unwrap_or_else(|| InvocationCaller::Local {
                route,
                time: original_time,
                admitted: Box::new(current),
            }),
        };
        if let Some(question) = question {
            return run_question::serve_answered(
                Arc::clone(&self.state),
                question,
                (host, caller),
                request.message,
                results,
            );
        }
        if let Some(consent) = consent {
            return app_consent::serve_after_consent(
                Arc::clone(&self.state),
                consent,
                host,
                caller,
                request.message,
                results,
            );
        }
        // An invocation the caller may not confirm itself is put to the
        // person who admitted the connection, on their own device.
        let answer = witness::serve_answering(
            Arc::clone(&self.state),
            host,
            caller,
            admitting_person,
            request.message,
        );
        // Awaited on this local set, which is also where an application's
        // directory answers; the connection's other calls keep flowing.
        Promise::from_future(async move {
            let answer = answer.await.ok_or_else(|| {
                capnp::Error::failed("the action host dropped the request".into())
            })?;
            let bytes = RemoteActionFrame::new(answer)
                .encode()
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            results.get().set_frame(&bytes);
            Ok(())
        })
    }

    fn subscribe_person_chat(
        self: Rc<Self>,
        params: channel_handle::SubscribePersonChatParams,
        results: channel_handle::SubscribePersonChatResults,
    ) -> Promise<(), capnp::Error> {
        self.subscribe_person_chat_dispatch(&params, results)
    }

    fn person_chat(
        self: Rc<Self>,
        params: channel_handle::PersonChatParams,
        mut results: channel_handle::PersonChatResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from((|| {
            let params = params.get()?;
            let reply = self
                .person_chat_dispatch(params.get_command()?, params.get_on_behalf_of()?)
                .map_err(capnp::Error::failed)?;
            results.get().set_reply(&reply);
            Ok(())
        })())
    }

    fn committed_document(
        self: Rc<Self>,
        params: channel_handle::CommittedDocumentParams,
        mut results: channel_handle::CommittedDocumentResults,
    ) -> Promise<(), capnp::Error> {
        use runtime_collab::committed_document::Request;
        let decoded = (|| {
            let reader = params.get()?;
            Request::decode(reader.get_request()?).map_err(|e| capnp::Error::failed(e.into()))
        })();
        let request = match decoded {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        if let Request::Await { known_revision } = request {
            // Held without blocking the channel: answered on this LocalSet
            // when the read may have changed.
            return Promise::from_future(async move {
                let reply = self
                    .await_committed_change(known_revision)
                    .await
                    .map_err(capnp::Error::failed)?;
                results
                    .get()
                    .set_reply(&reply.encode().map_err(|e| capnp::Error::failed(e.into()))?);
                Ok(())
            });
        }
        if let Some(preparing) = self.committed_creation_elsewhere(&request) {
            // The device that creates the record (the room's host) answers
            // over the network: awaited on this LocalSet, so the channel's
            // and every other channel's calls keep flowing meanwhile.
            return Promise::from_future(async move {
                let prepared = preparing.await;
                let reply = self
                    .committed_document_dispatch_prepared(&request, Some(prepared))
                    .map_err(capnp::Error::failed)?;
                results
                    .get()
                    .set_reply(&reply.encode().map_err(|e| capnp::Error::failed(e.into()))?);
                Ok(())
            });
        }
        Promise::from((|| {
            let reply = self
                .committed_document_dispatch(&request)
                .map_err(capnp::Error::failed)?;
            results
                .get()
                .set_reply(&reply.encode().map_err(|e| capnp::Error::failed(e.into()))?);
            Ok(())
        })())
    }

    fn audio_output_command(
        self: Rc<Self>,
        params: channel_handle::AudioOutputCommandParams,
        mut results: channel_handle::AudioOutputCommandResults,
    ) -> Promise<(), capnp::Error> {
        let future = (|| self.audio_output_dispatch(params.get()?.get_command()?))();
        let future = match future {
            Ok(future) => future,
            Err(error) => return Promise::err(error),
        };
        Promise::from_future(async move {
            let snapshot = future.await.map_err(capnp::Error::failed)?;
            let bytes = serde_json::to_vec(&snapshot)
                .map_err(|_| capnp::Error::failed("audio output reply encoding failed".into()))?;
            results.get().set_reply(&bytes);
            Ok(())
        })
    }

    fn native_media_command(
        self: Rc<Self>,
        params: channel_handle::NativeMediaCommandParams,
        mut results: channel_handle::NativeMediaCommandResults,
    ) -> Promise<(), capnp::Error> {
        let future = (|| {
            let request = params.get()?;
            self.native_media_dispatch(request.get_command_json()?)
        })();
        let future = match future {
            Ok(future) => future,
            Err(error) => return Promise::err(error),
        };
        Promise::from_future(async move {
            let snapshot = future.await.map_err(capnp::Error::failed)?;
            let bytes = snapshot
                .encode()
                .map_err(|e| capnp::Error::failed(e.into()))?;
            results.get().set_reply_json(&bytes);
            Ok(())
        })
    }

    fn request_document_snapshot(
        self: Rc<Self>,
        _params: channel_handle::RequestDocumentSnapshotParams,
        _results: channel_handle::RequestDocumentSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let _ = self
            .state
            .request_document_snapshot(&self.channel_id, self.connection_id);
        Promise::ok(())
    }

    fn realtime_command(
        self: Rc<Self>,
        params: channel_handle::RealtimeCommandParams,
        mut results: channel_handle::RealtimeCommandResults,
    ) -> Promise<(), capnp::Error> {
        let context = match self.realtime_context() {
            Ok(context) => context,
            Err(error) => return Promise::err(error),
        };
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let claimed_app = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let command = match request.get_command_json() {
            Ok(bytes) if bytes.len() <= collab_types::realtime::MAX_REALTIME_WIRE_BYTES => bytes,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "realtime command exceeds local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(error),
        };
        match delegate_realtime_command(&self.state.services, &context, claimed_app, command) {
            Ok(reply) if reply.len() <= collab_types::realtime::MAX_REALTIME_WIRE_BYTES => {
                results.get().set_reply_json(&reply);
                Promise::ok(())
            }
            Ok(_) => Promise::err(capnp::Error::failed(
                "realtime reply exceeds local protocol bound".into(),
            )),
            Err(error) => Promise::err(error),
        }
    }

    fn speaker_command(
        self: Rc<Self>,
        params: channel_handle::SpeakerCommandParams,
        mut results: channel_handle::SpeakerCommandResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        if route.app_id != "so.tilo.settings" {
            return Promise::err(capnp::Error::failed(
                "speaker groups are managed only by the certified Settings application".into(),
            ));
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let claimed_app = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        if claimed_app != route.app_id {
            return Promise::err(capnp::Error::failed(
                "speaker command app id must match the certified connection route".into(),
            ));
        }
        let bytes = match request.get_command_json() {
            Ok(bytes) if bytes.len() <= runtime_collab::MAX_SPEAKER_WIRE_BYTES => bytes,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "speaker command exceeds the local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(error),
        };
        let command = match serde_json::from_slice::<runtime_collab::SpeakerCommand>(bytes) {
            Ok(command) if command.valid() => command,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "speaker command is structurally invalid".into(),
                ));
            }
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let Some(speakers) = self.state.services.speakers.as_ref() else {
            return Promise::err(capnp::Error::failed(
                "speaker groups are unsupported by this host".into(),
            ));
        };
        let reply = match serde_json::to_vec(&speakers.command(command)) {
            Ok(reply) if reply.len() <= runtime_collab::MAX_SPEAKER_WIRE_BYTES => reply,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "speaker reply exceeds the local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        results.get().set_reply_json(&reply);
        Promise::ok(())
    }

    /// Resolves once what a speaker snapshot would show may have changed
    /// since `after`. Only the certified Settings route may wait, the same
    /// route every speaker command is confined to, and it learns nothing but
    /// that something changed: it asks for a snapshot, which is checked as
    /// always.
    fn speaker_changed(
        self: Rc<Self>,
        params: channel_handle::SpeakerChangedParams,
        mut results: channel_handle::SpeakerChangedResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let after = match params.get() {
            Ok(params) => params.get_after(),
            Err(error) => return Promise::err(error),
        };
        match speaker_change(&self.state.services, &route.app_id, after) {
            Ok(change) => Promise::from_future(async move {
                results.get().set_change(change.await);
                Ok(())
            }),
            Err(error) => Promise::err(error),
        }
    }

    fn settings(
        self: Rc<Self>,
        _params: channel_handle::SettingsParams,
        mut results: channel_handle::SettingsResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        match app_settings_capability(&self.state, route, self.verified_identity.clone()) {
            Ok(settings) => {
                results.get().set_settings(settings);
                Promise::ok(())
            }
            Err(error) => Promise::err(error),
        }
    }

    fn live_activity(
        self: Rc<Self>,
        _params: channel_handle::LiveActivityParams,
        mut results: channel_handle::LiveActivityResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        match live_activity_capability(&self.state, route, self.verified_identity.clone()) {
            Ok(activity) => {
                results.get().set_activity(activity);
                Promise::ok(())
            }
            Err(error) => Promise::err(error),
        }
    }

    fn wifi_continuity_command(
        self: Rc<Self>,
        params: channel_handle::WifiContinuityCommandParams,
        mut results: channel_handle::WifiContinuityCommandResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let claimed_app = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let command = match request.get_command_json() {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(error),
        };
        if route.app_id != WIFI_CONTINUITY_APP_ID || claimed_app != route.app_id {
            return Promise::err(capnp::Error::failed(
                "Wi-Fi continuity requires the certified Settings connection route".into(),
            ));
        }
        if command.len() > runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES {
            return Promise::err(capnp::Error::failed(
                "Wi-Fi continuity command exceeds the local protocol bound".into(),
            ));
        }
        let services = self.state.services.clone();
        let claimed_app = claimed_app.to_owned();
        let command = command.to_vec();
        Promise::from_future(async move {
            // NetworkManager and nl80211 queries are bounded blocking work.
            // Keep them off the broker's event loop and preserve the same
            // certified Settings validation used by every local command.
            let reply = tokio::task::spawn_blocking(move || {
                delegate_wifi_continuity_command(&services, &route.app_id, &claimed_app, &command)
            })
            .await
            .map_err(|error| capnp::Error::failed(error.to_string()))??;
            results.get().set_reply_json(&reply);
            Ok(())
        })
    }

    fn browser_service_session_command(
        self: Rc<Self>,
        params: channel_handle::BrowserServiceSessionCommandParams,
        mut results: channel_handle::BrowserServiceSessionCommandResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from((|| {
            let refused = || capnp::Error::failed("private service-session request refused".into());
            if self.route().app_id != "so.tilo.browser"
                || self.verified_identity.agent_id.is_some()
                || self.verified_identity.host_scope.is_some()
            {
                return Err(refused());
            }
            let read = self.document_read_context().map_err(|_| refused())?;
            let context = crate::services::BrowserServiceSessionRequestContext::authenticated(
                self.connection_id,
                self.verified_identity.principal_id.clone(),
                read,
            );
            let request = params.get()?;
            let reply = delegate_browser_service_session_command(
                &self.state.services,
                &context,
                request.get_command()?,
            )?;
            results.get().set_reply(&reply);
            Ok(())
        })())
    }

    fn nearby_transfer_command(
        self: Rc<Self>,
        params: channel_handle::NearbyTransferCommandParams,
        mut results: channel_handle::NearbyTransferCommandResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) = self
            .state
            .revalidate_admission(&route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let context = crate::services::NearbyTransferRequestContext::authenticated(
            self.connection_id,
            route.app_id.clone(),
            route.cluster_id.clone(),
            route.channel_key(),
            self.verified_identity
                .agent_id
                .is_none()
                .then(|| self.verified_identity.principal_id.clone()),
        );
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let claimed_app = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let command = match request.get_command_json() {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(error),
        };
        match delegate_nearby_transfer_command(&self.state.services, &context, claimed_app, command)
        {
            Ok(reply) => {
                results.get().set_reply_json(&reply);
                Promise::ok(())
            }
            Err(error) => Promise::err(error),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one RPC: parse, caller custody, clipboard access and the reply, kept in order"
    )]
    fn local_file_clipboard(
        self: Rc<Self>,
        params: channel_handle::LocalFileClipboardParams,
        mut results: channel_handle::LocalFileClipboardResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        #[cfg(target_os = "linux")]
        if request
            .get_command_json()
            .is_ok_and(runtime_collab::open_selected::discovery::is_envelope)
        {
            let future = match request.get_command_json().and_then(|bytes| {
                self.selected_discovery_dispatch(bytes, request.get_on_behalf_of()?)
            }) {
                Ok(value) => value,
                Err(error) => return Promise::err(error),
            };
            return Promise::from_future(async move {
                let value = future.await.map_err(capnp::Error::failed)?;
                if !value.valid() {
                    return Err(capnp::Error::failed("invalid discovery response".into()));
                }
                let bytes =
                    serde_json::to_vec(&value).map_err(|e| capnp::Error::failed(e.to_string()))?;
                if bytes.len() > runtime_collab::open_selected::discovery::MAX_BYTES {
                    return Err(capnp::Error::failed(
                        "discovery response exceeds limit".into(),
                    ));
                }
                results.get().set_reply_json(&bytes);
                Ok(())
            });
        }
        #[cfg(target_os = "linux")]
        if request
            .get_command_json()
            .is_ok_and(runtime_collab::open_selected::v2::is_envelope)
        {
            let future = match request.get_command_json().and_then(|bytes| {
                self.selected_open_v2_dispatch(bytes, request.get_on_behalf_of()?)
            }) {
                Ok(future) => future,
                Err(error) => return Promise::err(error),
            };
            return Promise::from_future(async move {
                let receipt = future.await.map_err(capnp::Error::failed)?;
                let bytes = runtime_collab::open_selected::v2::encode_observed(&receipt)
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
                results.get().set_reply_json(&bytes);
                Ok(())
            });
        }
        #[cfg(target_os = "linux")]
        if request
            .get_command_json()
            .is_ok_and(runtime_collab::open_selected::is_envelope)
        {
            let future = match request
                .get_command_json()
                .and_then(|bytes| self.selected_open_dispatch(bytes, request.get_on_behalf_of()?))
            {
                Ok(future) => future,
                Err(error) => return Promise::err(error),
            };
            return Promise::from_future(async move {
                let receipt = future.await.map_err(capnp::Error::failed)?;
                let bytes = runtime_collab::open_selected::encode_observed(&receipt)
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
                results.get().set_reply_json(&bytes);
                Ok(())
            });
        }
        if request
            .get_command_json()
            .is_ok_and(runtime_collab::write_attribution::is_envelope)
        {
            let answer = request.get_command_json().and_then(|bytes| {
                self.write_attribution_dispatch(bytes, request.get_on_behalf_of()?)
            });
            return match answer {
                Ok(reply) => {
                    results.get().set_reply_json(&reply);
                    Promise::ok(())
                }
                Err(error) => Promise::err(error),
            };
        }
        if request
            .get_command_json()
            .is_ok_and(external_moderation::is_envelope)
        {
            let receive = match request.get_command_json().and_then(|bytes| {
                external_moderation::start(&self, bytes, request.get_on_behalf_of()?)
                    .map_err(capnp::Error::failed)
            }) {
                Ok(receive) => receive,
                Err(error) => return Promise::err(error),
            };
            return Promise::from_future(async move {
                let reply = receive
                    .await
                    .map_err(|_| {
                        capnp::Error::failed("external moderation outcome unobserved".into())
                    })?
                    .map_err(capnp::Error::failed)?;
                let bytes = serde_json::to_vec(&reply)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                results.get().set_reply_json(&bytes);
                Ok(())
            });
        }
        let answer = request
            .get_command_json()
            .and_then(|bytes| self.file_clipboard_dispatch(bytes, request.get_on_behalf_of()?));
        match answer {
            Ok(reply) => {
                results.get().set_reply_json(&reply);
                Promise::ok(())
            }
            Err(error) => Promise::err(error),
        }
    }

    fn subscribe_realtime(
        self: Rc<Self>,
        _params: channel_handle::SubscribeRealtimeParams,
        mut results: channel_handle::SubscribeRealtimeResults,
    ) -> Promise<(), capnp::Error> {
        let context = match self.realtime_context() {
            Ok(context) => context,
            Err(error) => return Promise::err(error),
        };
        let Some(service) = self.state.services.realtime.as_ref() else {
            return Promise::err(capnp::Error::failed(
                "realtime continuity is unsupported by this host".into(),
            ));
        };
        let rx = match service.subscribe(&context) {
            Ok(rx) => rx,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let client: realtime_stream::Client = capnp_rpc::new_client(RealtimeStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "receipt validation and persistence share one RPC boundary so acknowledgement cannot precede durable local state"
    )]
    fn record_local_effect(
        self: Rc<Self>,
        params: channel_handle::RecordLocalEffectParams,
        _results: channel_handle::RecordLocalEffectResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let resource = match request
            .get_resource()
            .map_err(|error| error.to_string())
            .and_then(|value| value.to_str().map_err(|error| error.to_string()))
        {
            Ok(value) => value.to_string(),
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let action = match request
            .get_action()
            .map_err(|error| error.to_string())
            .and_then(|value| value.to_str().map_err(|error| error.to_string()))
        {
            Ok(value) => value.to_string(),
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let update = match request
            .get_update_json()
            .map_err(|error| capnp::Error::failed(error.to_string()))
            .and_then(|value| {
                value
                    .to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))
            })
            .and_then(|json| {
                serde_json::from_str::<tilo_actions::InvocationUpdate>(json)
                    .map_err(|error| capnp::Error::failed(error.to_string()))
            }) {
            Ok(update) => update,
            Err(error) => return Promise::err(error),
        };
        let Some(status) = tilo_actions::EffectReceiptStatus::from_terminal(&update) else {
            return Promise::err(capnp::Error::failed(
                "only a terminal effect may enter workspace activity".into(),
            ));
        };
        let Some((surface, offered)) = self
            .state
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&self.peer_id).cloned())
            .and_then(|surface| {
                let offered = surface
                    .actions
                    .iter()
                    .find(|candidate| candidate.resource == resource && candidate.action == action)
                    .cloned()?;
                Some((surface, offered))
            })
        else {
            return Promise::err(capnp::Error::failed(
                "the application no longer publishes that action".into(),
            ));
        };
        if offered.operation_kind != tilo_actions::OperationKind::Effect {
            return Promise::err(capnp::Error::failed(
                "only Effects produce durable invocation receipts".into(),
            ));
        }
        let what = tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(&surface.app_id, &offered.resource_kind, &offered.action),
            [],
        );
        let mut context = tilo_actions::ActionContext::for_resource(
            offered.resource,
            offered.resource_kind,
            offered.resource_label,
        )
        .with_app(&surface.app_id, &surface.app_id)
        .with_described(tilo_actions::Described::new(what));
        let actor = current.broker_actor().id;
        if self.state.durable_owner_required
            || self
                .state
                .publisher
                .requires_owner_for(&surface.route.cluster_id)
        {
            context.purpose = None;
            let receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
                update.invocation(),
                &actor,
                context,
                status,
                tilo_clock::unix_now_i64(),
            );
            let state = self.state.clone();
            let route = self.route();
            let identity = self.verified_identity.clone();
            return Promise::from_future(async move {
                let _order = state.owner_commit_order.lock().await;
                state.revalidate_admission(&route, &identity)?;
                state
                    .commit_owner_request(
                        tilo_owner_commit::OwnerCommitRequest {
                            room: surface.route.cluster_id,
                            operation: tilo_owner_commit::CommitOperation::EffectReceipt {
                                invocation: update.invocation().0,
                                redacted: serde_json::to_vec(&receipt)
                                    .map_err(|e| capnp::Error::failed(e.to_string()))?,
                            },
                        },
                        None,
                    )
                    .await?;
                state.revalidate_admission(&route, &identity)?;
                Ok(())
            });
        }
        context.purpose = None;
        match self
            .state
            .services
            .legacy_workspace
            .as_ref()
            .ok_or_else(|| "native effect projection unsupported".to_string())
            .and_then(|projection| {
                projection.record_effect(
                    &surface.route.cluster_id,
                    update.invocation(),
                    &actor,
                    context,
                    status,
                )
            }) {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn open_workspace_control(
        self: Rc<Self>,
        params: channel_handle::OpenWorkspaceControlParams,
        mut results: channel_handle::OpenWorkspaceControlResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) =
            revalidate_workspace_control(&self.state, &route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let Some(room) = tilo_desktop_model::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "workspace control requires a workspace-scoped channel".into(),
            ));
        };
        let target_bytes = match params.get().and_then(
            runtime_collab::collab_capnp::channel_handle::open_workspace_control_params::Reader::get_target_device,
        ) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut target = [0_u8; 32];
                target.copy_from_slice(bytes);
                target
            }
            _ => {
                return Promise::err(capnp::Error::failed(
                    "target device must be an exact 32-byte identity".into(),
                ));
            }
        };
        let Some(service) = self.state.services.workspace_control.as_ref() else {
            return Promise::err(capnp::Error::failed("workspace control unsupported".into()));
        };
        let state = self.state.clone();
        let identity = self.verified_identity.clone();
        let revalidate =
            Arc::new(move || revalidate_workspace_control(&state, &route, &identity).map(|_| ()));
        let client = match service.open(room, target_bytes, revalidate) {
            Ok(client) => client,
            Err(error) => return Promise::err(error),
        };
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.connection_id;
        let route = self.route();
        let verified_identity = self.verified_identity.clone();
        let record_kind = self.record_kind.clone();
        Promise::from_future(async move {
            let op_bytes = params.get()?.get_op()?.to_vec();
            let envelope: SignedOp = postcard::from_bytes(&op_bytes).map_err(|e| {
                capnp::Error::failed(format!(
                    "capnp send: op is not a postcard-encoded SignedOp: {e}"
                ))
            })?;
            let current_identity = state.revalidate_admission(&route, &verified_identity)?;
            verify_session_author(&current_identity, envelope.author)
                .map_err(|error| capnp::Error::failed(format!("capnp send: {error}")))?;
            state.authorize_certified_document(&route, &current_identity, &envelope)?;
            let _owner_order = if state.durable_owner_required
                || state.publisher.requires_owner_for(&route.cluster_id)
            {
                Some(state.owner_commit_order.lock().await)
            } else {
                None
            };
            if state.durable_owner_required || state.publisher.requires_owner_for(&route.cluster_id)
            {
                state.revalidate_admission(&route, &verified_identity)?;
                state
                    .commit_owner_request(
                        tilo_owner_commit::OwnerCommitRequest {
                            room: route.cluster_id.clone(),
                            operation: tilo_owner_commit::CommitOperation::Document {
                                app: route.app_id.clone(),
                                record: route.record_id.clone(),
                                envelope: Box::new(envelope.clone()),
                            },
                        },
                        record_kind,
                    )
                    .await?;
            }
            let current_identity = state.revalidate_admission(&route, &verified_identity)?;
            let authorized =
                state.authorize_certified_document(&route, &current_identity, &envelope)?;
            handle_authorized_signed_op_send(
                &state,
                peer_id,
                &route,
                &current_identity,
                &authorized,
                envelope,
            );
            Ok(())
        })
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let rx = self
            .state
            .subscribe_ops(&self.channel_id, self.connection_id);
        let client: stream::Client = capnp_rpc::new_client(OpStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let client: presence_handle::Client = capnp_rpc::new_client(CapnpPresenceHandle {
            state: self.state.clone(),
            channel_id: self.channel_id.clone(),
            peer_id: self.connection_id,
            verified_identity: self.verified_identity.clone(),
            interaction_sequences: Mutex::new(HashMap::new()),
        });
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn retired_unsigned_snapshot(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedSnapshotParams,
        _results: channel_handle::RetiredUnsignedSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn retired_unsigned_incremental(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedIncrementalParams,
        _results: channel_handle::RetiredUnsignedIncrementalResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn subscribe_context(
        self: Rc<Self>,
        _params: channel_handle::SubscribeContextParams,
        mut results: channel_handle::SubscribeContextResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let route = self.route();
        let native = match self
            .state
            .native_roster_for_subscriber(&route, &self.verified_identity)
        {
            Ok(roster) => roster,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let instance = match self.native_private_window_instance() {
            Ok(instance) => instance,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let document = if instance.is_some() {
            match self.document_read_context() {
                Ok(context) => Some(context),
                Err(error) => return Promise::err(capnp::Error::failed(error)),
            }
        } else {
            None
        };
        let rx = self
            .state
            .subscribe_context(&self.channel_id, self.connection_id, native.clone());
        if let Some(instance) = instance {
            let alias = CrdtRoute::record(
                &route.app_id,
                &route.cluster_id,
                format!("continuation:{instance}"),
            )
            .channel_key();
            self.state
                .alias_private_context(&self.channel_id, &alias, self.connection_id);
        }
        let client: context_stream::Client = capnp_rpc::new_client(ContextStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            native_guard: native.map(|native| NativeContextReadGuard {
                state: self.state.clone(),
                route,
                identity: native.admitted,
                document,
            }),
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn fetch_scrollback_for(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackForParams,
        mut results: channel_handle::FetchScrollbackForResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let broker_channels = self.state.broker_channels.clone();
        let publisher = Arc::clone(self.state.publisher());
        let route = self.route();
        Promise::from_future(async move {
            let p = params.get()?;
            let artifact_id_bytes = p.get_artifact_id()?.to_vec();
            let before_seq = p.get_before_seq();
            let max = p.get_max();
            let uuid = uuid::Uuid::from_slice(&artifact_id_bytes).map_err(|e| {
                capnp::Error::failed(format!(
                    "capnp fetch_scrollback_for: artifactId must be 16 bytes: {e}"
                ))
            })?;
            let artifact = ArtifactId(uuid);
            let (ops, mut exhausted) = read_scrollback(&broker_channels, artifact, before_seq, max);
            // Local history ends here: ask the channel's other devices for
            // what lies before it. Their answer lands in durable history, so
            // the page is reported as not exhausted and the endpoint's next
            // request picks it up.
            if exhausted {
                let peer_bound = ops
                    .iter()
                    .filter_map(|bytes| postcard::from_bytes::<SignedOp>(bytes).ok())
                    .map(|op| op.seq)
                    .min()
                    .unwrap_or(before_seq);
                let ask = broker_channels
                    .lock()
                    .is_ok_and(|mut ch| ch.request_peer_history(artifact, peer_bound));
                if ask
                    && publisher.scrollback_request(
                        &route,
                        artifact,
                        peer_bound,
                        u16::try_from(max).unwrap_or(u16::MAX),
                    )
                {
                    exhausted = false;
                }
            }
            let batch = results.get().init_batch();
            fill_scrollback_batch(batch, &ops, exhausted)?;
            Ok(())
        })
    }

    fn publish_actions(
        self: Rc<Self>,
        params: channel_handle::PublishActionsParams,
        _results: channel_handle::PublishActionsResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        Promise::from_future(async move {
            let request = params.get()?;
            let app_id = request.get_app_id()?.to_str()?.to_owned();
            if route.app_id.is_empty() || app_id != route.app_id {
                return Err(capnp::Error::failed(
                    "publishActions appId must match the certified connection route".into(),
                ));
            }
            let mut rows = Vec::with_capacity(request.get_actions()?.len() as usize);
            for row in request.get_actions()? {
                rows.push(runtime_collab::decode_exported_action(row)?);
            }
            let actions = validate_published_surface(rows)?;
            let offers = !actions.is_empty();
            let surface = PublishedSurface {
                peer_id,
                app_id,
                route,
                actions,
                live: true,
            };
            // A whole surface supersedes any delta state: a later delta
            // without `reset` finds no shapes and is refused, so the
            // publisher resets.
            if let Ok(mut tables) = state.surface_tables.lock() {
                tables.remove(&peer_id);
            }
            state.launchable.retain(&surface);
            state
                .surfaces
                .lock()
                .map_err(|_| capnp::Error::failed("action surface registry unavailable".into()))?
                .insert(peer_id, surface);
            state.advance_witness_publication(peer_id, WitnessPublicationKind::Surface);
            state.surfaces_changed();
            state.publish_action_catalogue();
            if offers {
                state.launch_published(peer_id);
            }
            Ok(())
        })
    }

    fn publish_action_surface(
        self: Rc<Self>,
        params: channel_handle::PublishActionSurfaceParams,
        _results: channel_handle::PublishActionSurfaceResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        Promise::from_future(async move {
            let request = params.get()?;
            let app_id = request.get_app_id()?.to_str()?.to_owned();
            if route.app_id.is_empty() || app_id != route.app_id {
                return Err(capnp::Error::failed(
                    "publishActionSurface appId must match the certified connection route".into(),
                ));
            }
            let delta = runtime_collab::decode_surface_delta(request)?;
            // Apply to a copy: a delta that does not apply, or whose result
            // is not a valid surface, changes nothing.
            let mut table = state
                .surface_tables
                .lock()
                .map_err(|_| capnp::Error::failed("action surface registry unavailable".into()))?
                .get(&peer_id)
                .cloned()
                .unwrap_or_default();
            table.apply(&delta).map_err(|reason| {
                capnp::Error::failed(format!("action surface delta refused: {reason}"))
            })?;
            let actions = validate_published_surface(table.actions())?;
            let offers = !actions.is_empty();
            let surface = PublishedSurface {
                peer_id,
                app_id,
                route,
                actions,
                live: true,
            };
            state
                .surface_tables
                .lock()
                .map_err(|_| capnp::Error::failed("action surface registry unavailable".into()))?
                .insert(peer_id, table);
            state.launchable.retain(&surface);
            state
                .surfaces
                .lock()
                .map_err(|_| capnp::Error::failed("action surface registry unavailable".into()))?
                .insert(peer_id, surface);
            state.advance_witness_publication(peer_id, WitnessPublicationKind::Surface);
            state.surfaces_changed();
            state.publish_action_catalogue();
            if offers {
                state.launch_published(peer_id);
            }
            Ok(())
        })
    }

    fn publish_action_directory(
        self: Rc<Self>,
        params: channel_handle::PublishActionDirectoryParams,
        _results: channel_handle::PublishActionDirectoryResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        if let Err(error) =
            validate_directory_identifier(&app_id, "app id", DIRECTORY_MAX_IDENTIFIER_BYTES, true)
        {
            return Promise::err(error);
        }
        if self.route().app_id.is_empty() || app_id != self.route().app_id {
            return Promise::err(capnp::Error::failed(
                "publishActionDirectory appId must match the certified connection route".into(),
            ));
        }
        let directory = match request.get_directory() {
            Ok(directory) => directory,
            Err(error) => return Promise::err(error),
        };
        let route = self.route();
        let registration = Rc::new(uuid::Uuid::new_v4());
        let Ok(_admission) = self.state.admission_lock.lock() else {
            return Promise::err(capnp::Error::failed(
                "directory admission unavailable".into(),
            ));
        };
        if let Err(error) =
            self.state
                .register_canonical_directory(self.peer_id, &route, *registration)
        {
            return Promise::err(capnp::Error::failed(error));
        }
        self.directories.directories.borrow_mut().insert(
            self.peer_id,
            RegisteredActionDirectory {
                registration,
                pagination: directory_pagination::DirectoryPagination::default(),
                publishing_person: self
                    .verified_identity
                    .certificate
                    .as_ref()
                    .map(|cert| principal_id_for_root(cert.user_public_key)),
                app_id: app_id.clone(),
                route: route.clone(),
                directory,
            },
        );
        if let Ok(mut roots) = self.state.action_directory_roots.lock() {
            roots.insert(
                self.peer_id,
                ActionDirectoryRoot {
                    peer_id: self.peer_id,
                    app_id,
                    cluster_id: route.cluster_id.clone(),
                    record_id: route.record_id.clone(),
                },
            );
        }
        self.state
            .advance_witness_publication(self.peer_id, WitnessPublicationKind::Directory);
        self.retain_native_checkout(&route);
        self.state.refresh_action_directory_root_snapshot();
        // The directory can alter remote discoverability without an eager
        // action snapshot changing, so notify legacy catalogue subscribers.
        self.state.publish_action_catalogue();
        self.state.launch_published(self.peer_id);
        Promise::ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one broker boundary must copy bounded input, enforce current policy, and copy a capability page without retaining it"
    )]
    fn query_action_directory(
        self: Rc<Self>,
        params: channel_handle::QueryActionDirectoryParams,
        mut results: channel_handle::QueryActionDirectoryResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get().and_then(
            runtime_collab::collab_capnp::channel_handle::query_action_directory_params::Reader::get_query,
        ) {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let peer_id = request.get_peer_id();
        // A global root is intentionally not synthesised from every app here:
        // that would force a scan of all live directories for each keystroke.
        // Callers discover live app ids from the small authenticated catalogue,
        // then page one source at a time with an opaque source cursor.
        if peer_id == 0 {
            return Promise::err(capnp::Error::failed(
                "a directory page must name one live application peer".into(),
            ));
        }
        let revision = match request.get_revision().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let needle = match request.get_needle().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let parent_resource = match request.get_parent_resource().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let resource_name = match request.get_resource_name().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let cursor = match request.get_cursor() {
            Ok(cursor) => cursor.to_vec(),
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = validate_directory_query_input(&runtime_collab::ActionDirectoryQuery {
            peer_id,
            revision: revision.clone(),
            cursor: cursor.clone(),
            needle: needle.clone(),
            parent_resource: parent_resource.clone(),
            resource_name: resource_name.clone(),
            // Input validation is independent of numeric page budgets; these
            // placeholders avoid delaying it until after a capability lookup.
            max_results: 1,
            max_bytes: 1,
            max_work: 1,
            max_depth: 0,
        }) {
            return Promise::err(error);
        }
        let max_results = request
            .get_max_results()
            .min(CapnpBrokerState::DIRECTORY_MAX_RESULTS);
        let max_bytes = request
            .get_max_bytes()
            .min(CapnpBrokerState::DIRECTORY_MAX_BYTES);
        let max_work = request
            .get_max_work()
            .min(CapnpBrokerState::DIRECTORY_MAX_WORK);
        let max_depth = request
            .get_max_depth()
            .min(CapnpBrokerState::DIRECTORY_MAX_DEPTH);
        if max_results == 0 || max_bytes == 0 || max_work == 0 {
            return Promise::err(capnp::Error::failed(
                "directory query budgets must be non-zero".into(),
            ));
        }
        let cluster = self.route().cluster_id;
        let actor = self.verified_identity.broker_actor();
        let registered = match self.directories.registered(peer_id, &cluster) {
            Ok(registered) => registered,
            Err(error) => return Promise::err(error),
        };
        let state = self.state.clone();
        let host_guard =
            HostWorkerReadGuard::for_channel(&state, self.peer_id, &self.verified_identity);
        Promise::from_future(async move {
            HostWorkerReadGuard::check(host_guard.as_ref())?;
            let cursor = registered.pagination.source_cursor(
                &actor,
                &revision,
                &needle,
                &parent_resource,
                &cursor,
            )?;
            let mut forward = registered.directory.query_request();
            {
                let mut query = forward.get().init_query();
                query.set_peer_id(peer_id);
                query.set_revision(&revision);
                query.set_cursor(&cursor);
                query.set_needle(&needle);
                query.set_parent_resource(&parent_resource);
                query.set_max_results(max_results);
                query.set_max_bytes(max_bytes);
                query.set_max_work(max_work);
                query.set_max_depth(max_depth);
                query.set_resource_name(&resource_name);
            }
            let response = forward.send().promise.await?;
            HostWorkerReadGuard::check(host_guard.as_ref())?;
            let page = response.get()?.get_page()?;
            let bytes = page.total_size()?.word_count.saturating_mul(8);
            if bytes > u64::from(max_bytes) {
                return Err(capnp::Error::failed(
                    "application exceeded directory byte budget".into(),
                ));
            }
            let page = runtime_collab::decode_action_directory_page(page)?;
            let mut page = filter_directory_page(
                &state,
                &cluster,
                &actor,
                &registered.app_id,
                registered.publishing_person.as_deref(),
                page,
                max_results,
            )?;
            if !resource_name.is_empty() {
                page.next_cursor.clear();
            }
            registered
                .pagination
                .shield(&actor, &needle, &parent_resource, &mut page);
            runtime_collab::write_action_directory_page(results.get().init_page(), &page)?;
            Ok(())
        })
    }

    fn resolve_published_action(
        self: Rc<Self>,
        params: channel_handle::ResolvePublishedActionParams,
        results: channel_handle::ResolvePublishedActionResults,
    ) -> Promise<(), capnp::Error> {
        resolved_invocation::resolve(self, &params, results)
    }

    fn subscribe_invocations(
        self: Rc<Self>,
        params: channel_handle::SubscribeInvocationsParams,
        mut results: channel_handle::SubscribeInvocationsResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let stable = match params.get() {
            Ok(request) => request.get_stable_invocation_ids(),
            Err(error) => return Promise::err(error),
        };
        // Stream replacement and resolved dispatch share the admission boundary:
        // an admitted old token must never dispatch to a replacement stream.
        let Ok(admission) = self.state.admission_lock.lock() else {
            return Promise::err(capnp::Error::failed(
                "invocation admission registry unavailable".into(),
            ));
        };
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        if let Ok(mut directories) = self.state.canonical_directories.lock() {
            directories.stream_replaced(self.peer_id);
        } else {
            return Promise::err(capnp::Error::failed(
                "canonical stream registry unavailable".into(),
            ));
        }
        if let Ok(mut capabilities) = self.state.stable_invocation_subscribers.lock() {
            if stable {
                capabilities.insert(self.peer_id);
            } else {
                capabilities.remove(&self.peer_id);
            }
        } else {
            return Promise::err(capnp::Error::failed(
                "application capability registry unavailable".into(),
            ));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.state.invocation_subscribers.lock() {
            subscribers.insert(self.peer_id, tx);
        } else {
            return Promise::err(capnp::Error::failed(
                "invocation subscriber registry unavailable".into(),
            ));
        }
        drop(admission);
        self.state.invocation_streams_changed();
        // The app the launcher attested, else the one this channel names.
        let (app, room) = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .map_or_else(
                || {
                    let route = self.route();
                    (route.app_id, route.cluster_id)
                },
                |app| (app.app_id.clone(), app.workspace_id.clone()),
            );
        let delivered = invocation_delivery_notice(&self.state.services, app, room);
        let client: invocation_stream::Client = capnp_rpc::new_client(InvocationStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: invocation_queue(self.peer_id, rx),
            delivered,
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn subscribe_task_runs(
        self: Rc<Self>,
        _params: channel_handle::SubscribeTaskRunsParams,
        mut results: channel_handle::SubscribeTaskRunsResults,
    ) -> Promise<(), capnp::Error> {
        if self.state.services.legacy_workspace.is_none()
            && !self.state.task_admitted_room(&self.route().cluster_id)
        {
            return Promise::err(capnp::Error::failed(
                "task execution service unsupported".into(),
            ));
        }
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an admitted delegated agent may subscribe to task runs".into(),
            ));
        };
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.state.task_subscribers.lock() {
            subscribers.insert(
                self.peer_id,
                TaskSubscription {
                    agent_id,
                    cluster_id: self.route().cluster_id,
                    tx,
                },
            );
        } else {
            return Promise::err(capnp::Error::failed(
                "task subscription registry unavailable".into(),
            ));
        }
        // A replacement stream needs the canonical approved run again, even
        // when the authenticated connection itself did not change.
        if let Ok(mut runs) = self.state.pending_task_runs.lock() {
            runs.retain(|_, run| run.peer_id != self.peer_id);
        }
        self.state.local_turn.notify_one();
        let client: task_run_stream::Client = capnp_rpc::new_client(TaskRunStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        self.state.refresh_native_task_delivery();
        Promise::ok(())
    }

    fn report_task_run(
        self: Rc<Self>,
        params: channel_handle::ReportTaskRunParams,
        _results: channel_handle::ReportTaskRunResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an agent may update a task run".into(),
            ));
        };
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let request = params.get()?;
            let lease = request.get_lease()?.to_str()?.to_owned();
            let update = serde_json::from_str::<runtime_collab::TaskRunUpdate>(
                request.get_update_json()?.to_str()?,
            )
            .map_err(|error| capnp::Error::failed(format!("invalid task update: {error}")))?;
            state
                .record_task_run_update(peer_id, &agent_id, request.get_run_id(), &lease, &update)
                .map_err(capnp::Error::failed)
        })
    }

    fn invoke_task_action(
        self: Rc<Self>,
        params: channel_handle::InvokeTaskActionParams,
        mut results: channel_handle::InvokeTaskActionResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an agent may invoke a task action".into(),
            ));
        };
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let caller = durable_local_invocation::LocalCaller::capture(&self).ok();
        let route = self.route();
        let identity = self.verified_identity.clone();
        Promise::from_future(async move {
            let request = params.get()?;
            // Interactive confirmations have no safe unattended equivalent.
            if !request.get_confirmation_json()?.to_str()?.trim().is_empty() {
                return Err(capnp::Error::failed(
                    "task action confirmation evidence is not accepted; request human confirmation outside the run".into(),
                ));
            }
            let lease = request.get_lease()?.to_str()?.to_owned();
            let resource = request.get_resource()?.to_str()?.to_owned();
            let action = request.get_action()?.to_str()?.to_owned();
            let argument = request
                .get_has_argument()
                .then(|| {
                    request
                        .get_argument()
                        .and_then(|value| Ok(value.to_str()?.to_owned()))
                })
                .transpose()?;
            let provider = request.get_provider()?.to_str()?.to_owned();
            if !provider.is_empty() && request.get_peer_id() != 0 {
                return Err(capnp::Error::failed(
                    "task target mixes live and stable provider identities".into(),
                ));
            }
            let target = runtime_collab::TaskAction {
                peer_id: request.get_peer_id(),
                provider: &provider,
                resource: &resource,
                action: &action,
                argument: argument.as_deref(),
            };
            let invocation = request.get_invocation()?.to_str()?;
            let provenance = native_task_execution::child_provenance(&lease, invocation, caller)?;
            let (id, update) = if let Some((caller, invocation)) = &provenance {
                let reask = native_task_execution::ChildReask {
                    peer_id,
                    agent_id: &agent_id,
                    run_id: request.get_run_id(),
                    lease: &lease,
                    invocation: *invocation,
                    target,
                    caller,
                };
                let wait = native_task_execution::ChildWait::read(&request)?;
                state
                    .invoke_native_task_child(&reask, wait, (&route, &identity))
                    .await?
            } else {
                if !provider.is_empty() {
                    return Err(capnp::Error::failed(
                        "closed-app activation requires a native task's authenticated custody"
                            .into(),
                    ));
                }
                let job = state
                    .start_task_action(
                        peer_id,
                        &agent_id,
                        request.get_run_id(),
                        &lease,
                        target,
                        (None, (false, None)),
                    )
                    .map_err(capnp::Error::failed)?
                    .start(&state)
                    .await
                    .map_err(capnp::Error::failed)?;
                let update = state.invocation_status(job.id).ok_or_else(|| {
                    capnp::Error::failed("accepted task action has no broker state".into())
                })?;
                (job.id, update)
            };
            results.get().set_id(id);
            results.get().set_update_json(
                &serde_json::to_string(&update)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
            );
            Ok(())
        })
    }

    fn task_run_cancelled(
        self: Rc<Self>,
        params: channel_handle::TaskRunCancelledParams,
        mut results: channel_handle::TaskRunCancelledResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an agent may observe task cancellation".into(),
            ));
        };
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let request = params.get()?;
            let lease = request.get_lease()?.to_str()?.to_owned();
            let cancelled = state
                .task_run_cancelled(peer_id, &agent_id, request.get_run_id(), &lease)
                .map_err(capnp::Error::failed)?;
            results.get().set_cancelled(cancelled);
            let run = state
                .task_run_for(peer_id, &agent_id, request.get_run_id(), &lease)
                .map_err(capnp::Error::failed)?;
            let status = state
                .native_task_status(&run, &lease)
                .map_err(capnp::Error::failed)?;
            if let Some(control) = status.control {
                results.get().set_control_json(
                    &serde_json::to_string(&control)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?,
                );
            }
            Ok(())
        })
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the pending entry borrows the registry until it is staged; the lint misreads the borrow"
    )]
    fn complete_invocation(
        self: Rc<Self>,
        params: channel_handle::CompleteInvocationParams,
        _results: channel_handle::CompleteInvocationResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let identity = self.verified_identity.clone();
        let host_guard = HostWorkerReadGuard::for_channel(&state, peer_id, &identity);
        Promise::from_future(async move {
            let _owner_order = if state.durable_owner_required
                || state.publisher.requires_owner_for(&route.cluster_id)
            {
                Some(state.owner_commit_order.lock().await)
            } else {
                None
            };
            HostWorkerReadGuard::check(host_guard.as_ref())?;
            if state.durable_owner_required || state.publisher.requires_owner_for(&route.cluster_id)
            {
                state.revalidate_admission(&route, &identity)?;
            }
            let request = params.get()?;
            let id = request.get_id();
            let completion = InvocationCompletion {
                code: request.get_code()?.to_str()?.to_owned(),
                message: request.get_message()?.to_str()?.to_owned(),
            };
            if !completion.code.is_empty() && completion.message.trim().is_empty() {
                return Err(capnp::Error::failed(
                    "failed invocation completion requires a message".into(),
                ));
            }
            let (staged, update) = {
                let registry = state
                    .pending_invocations
                    .lock()
                    .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
                let pending = registry
                    .get(&id)
                    .filter(|pending| pending.peer_id == peer_id)
                    .ok_or_else(|| {
                        capnp::Error::failed(
                            "an application may complete only its own invocation".into(),
                        )
                    })?;
                if completion.code.is_empty() {
                    pending
                        .result
                        .validate(&tilo_actions::ActionOutput::default())
                        .map_err(|error| {
                            capnp::Error::failed(format!(
                                "legacy completion violated its published result contract: {error}"
                            ))
                        })?;
                }
                let invocation = pending.update.invocation();
                let update = if completion.code.is_empty() {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: completion.code.clone(),
                            message: completion.message.clone(),
                            details: None,
                            retryable: false,
                        },
                    }
                };
                (pending.clone(), update)
            };
            state
                .commit_effect_before_terminal(&staged, &update)
                .await?;
            HostWorkerReadGuard::check(host_guard.as_ref())?;
            if state.durable_owner_required || state.publisher.requires_owner_for(&route.cluster_id)
            {
                state.revalidate_admission(&route, &identity)?;
            }
            state
                .settle_definite_outcome(peer_id, id, &staged.update, &update)
                .await?;
            let pending = state
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?
                .remove(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            state.publish_activity(pending.activity.as_ref(), &update);
            state.record_completion(id, pending.completion_owner(), update.clone());
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            state.record_effect_receipt(&pending, &update);
            Ok(())
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the RPC validates the complete invocation lifecycle state machine"
    )]
    fn update_invocation(
        self: Rc<Self>,
        params: channel_handle::UpdateInvocationParams,
        _results: channel_handle::UpdateInvocationResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let request = params.get()?;
            let update =
                serde_json::from_str(request.get_update_json()?.to_str()?).map_err(|error| {
                    capnp::Error::failed(format!("invalid invocation update: {error}"))
                })?;
            self.state
                .apply_invocation_update(
                    self.peer_id,
                    request.get_id(),
                    update,
                    Some((self.route(), self.verified_identity.clone())),
                )
                .await
        })
    }

    fn invocation_cancellation(
        self: Rc<Self>,
        params: channel_handle::InvocationCancellationParams,
        mut results: channel_handle::InvocationCancellationResults,
    ) -> Promise<(), capnp::Error> {
        let checked = HostWorkerReadGuard::check(
            HostWorkerReadGuard::for_channel(&self.state, self.peer_id, &self.verified_identity)
                .as_ref(),
        );
        if let Err(error) = checked {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let id = request.get_id();
        let owns = self
            .state
            .pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).map(|job| job.peer_id == self.peer_id))
            .unwrap_or(false);
        if !owns {
            return Promise::err(capnp::Error::failed("unknown invocation id".into()));
        }
        let cancelled = self
            .state
            .cancellation_requests
            .lock()
            .is_ok_and(|requests| requests.contains(&id));
        if !cancelled {
            results.get().set_cancelled(false);
            return Promise::ok(());
        }
        // The application hears of the request once it is durable.
        let durable = self.state.invocation_in_flight(id);
        Promise::from_future(async move {
            durable.wait().await.map_err(capnp::Error::failed)?;
            results.get().set_cancelled(true);
            Ok(())
        })
    }

    fn action_catalogue(
        self: Rc<Self>,
        _params: channel_handle::ActionCatalogueParams,
        mut results: channel_handle::ActionCatalogueResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let snapshot = self.state.action_catalogue_json_for(
            &self.route().cluster_id,
            &self.verified_identity.broker_actor(),
            None,
        );
        match snapshot {
            Ok(snapshot) => {
                results.get().set_catalogue_json(&snapshot);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn subscribe_action_catalogue(
        self: Rc<Self>,
        _params: channel_handle::SubscribeActionCatalogueParams,
        mut results: channel_handle::SubscribeActionCatalogueResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let cluster = self.route().cluster_id;
        let actor = self.verified_identity.broker_actor();
        let snapshot = self.state.action_catalogue_json_for(&cluster, &actor, None);
        let initial = match snapshot {
            Ok(snapshot) => Arc::<str>::from(snapshot),
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        // The session's normal tracing filter passes only errors. Its flight
        // recorder captures stderr; report numeric metadata without changing
        // that filter or exporting any authorized catalogue contents.
        eprintln!(
            "collab: initial authorized action catalogue snapshot event_kind=ActionCatalogue serialized_bytes={}",
            initial.len()
        );
        let (tx, rx) = watch::channel(Some(initial));
        if let Ok(mut subscribers) = self.state.action_catalogue_subscribers.lock() {
            subscribers.insert(self.connection_id, (cluster, actor, tx));
        } else {
            return Promise::err(capnp::Error::failed(
                "action catalogue subscriber registry unavailable".into(),
            ));
        }
        let client: action_catalogue_stream::Client =
            capnp_rpc::new_client(ActionCatalogueStreamServer {
                host_guard: HostWorkerReadGuard::for_channel(
                    &self.state,
                    self.peer_id,
                    &self.verified_identity,
                ),
                reader: Rc::new(AsyncMutex::new(ActionCatalogueReader { rx, initial: true })),
            });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn invoke_resolved_published_action_durable(
        self: Rc<Self>,
        params: channel_handle::InvokeResolvedPublishedActionDurableParams,
        results: channel_handle::InvokeResolvedPublishedActionDurableResults,
    ) -> Promise<(), capnp::Error> {
        durable_local_invocation::invoke(self, &params, results)
    }
    fn published_invocation_observation(
        self: Rc<Self>,
        params: channel_handle::PublishedInvocationObservationParams,
        results: channel_handle::PublishedInvocationObservationResults,
    ) -> Promise<(), capnp::Error> {
        durable_local_invocation::observe(&self, &params, results)
    }
    fn cancel_durable_published_invocation(
        self: Rc<Self>,
        params: channel_handle::CancelDurablePublishedInvocationParams,
        results: channel_handle::CancelDurablePublishedInvocationResults,
    ) -> Promise<(), capnp::Error> {
        durable_local_invocation::cancel(&self, &params, results)
    }

    fn admit_local_document_action(
        self: Rc<Self>,
        params: channel_handle::AdmitLocalDocumentActionParams,
        results: channel_handle::AdmitLocalDocumentActionResults,
    ) -> Promise<(), capnp::Error> {
        local_document_action::admit(self, &params, results)
    }

    fn invoke_resolved_published_action(
        self: Rc<Self>,
        params: channel_handle::InvokeResolvedPublishedActionParams,
        results: channel_handle::InvokeResolvedPublishedActionResults,
    ) -> Promise<(), capnp::Error> {
        resolved_invocation::invoke(self, &params, results)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the RPC performs one complete authorized invocation admission transaction"
    )]
    fn invoke_published_action(
        self: Rc<Self>,
        params: channel_handle::InvokePublishedActionParams,
        mut results: channel_handle::InvokePublishedActionResults,
    ) -> Promise<(), capnp::Error> {
        let original_time = match self.state.registered_policy_time() {
            Ok(time) => time,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let requested_peer_id = request.get_peer_id();
        let peer_id = invocation_target_peer(requested_peer_id, self.peer_id);
        let resource = match request.get_resource().and_then(|s| {
            s.to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let action = match request.get_action().and_then(|s| {
            s.to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let argument = if request.get_has_argument() {
            match request.get_argument().and_then(|s| {
                s.to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))
            }) {
                Ok(value) => Some(value.to_owned()),
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            }
        } else {
            None
        };
        let confirmation = match request.get_confirmation_json().and_then(|s| {
            s.to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok("") => serde_json::Value::Object(serde_json::Map::new()),
            Ok(value) => match serde_json::from_str::<serde_json::Value>(value) {
                Ok(value) => value,
                Err(error) => {
                    return Promise::err(capnp::Error::failed(format!(
                        "invalid confirmation evidence: {error}"
                    )));
                }
            },
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let state = self.state.clone();
        let origin_route = self.route();
        let cluster = origin_route.cluster_id.clone();
        Promise::from_future(async move {
            let publication = state
                .surfaces
                .lock()
                .ok()
                .and_then(|surfaces| surfaces.get(&peer_id).cloned())
                .filter(|surface| surface.route.cluster_id == cluster)
                .and_then(|surface| surface.action_projection(&resource, &action))
                .ok_or_else(|| capnp::Error::failed("unknown application action".into()))?;
            let offered = &publication.1;
            let actor = current.broker_actor();
            let phrase = confirmation
                .get("confirmation_phrase")
                .and_then(serde_json::Value::as_str);
            let job = if requires_store_witness(&publication.0.app_id, &action) {
                // Never treat caller-supplied booleans, phrases or tokens as
                // permission to disclose. The host shows the signed audience
                // and payload, bound to a fresh invocation before it asks.
                let invocation = tilo_actions::InvocationId::new();
                let binding = state.native_witness_binding(
                    actor.id(),
                    &publication,
                    argument.as_deref(),
                    invocation,
                );
                let answer = state
                    .local_store_witness(&current, &publication, argument.as_deref(), &binding)
                    .await
                    .map_err(capnp::Error::failed)?;
                let current = state.revalidate_admission(&origin_route, &current)?;
                state
                    .check_native_store_offer(&publication)
                    .map_err(capnp::Error::failed)?;
                let current_binding = state.native_witness_binding(
                    actor.id(),
                    &publication,
                    argument.as_deref(),
                    invocation,
                );
                let token = state
                    .confirmation_token(
                        peer_id,
                        &publication.0.app_id,
                        offered,
                        argument.as_deref(),
                        &actor,
                        Some(&answer),
                        &current_binding,
                    )
                    .map_err(|error| {
                        capnp::Error::failed(format!("host confirmation refused: {error:?}"))
                    })?;
                state.start_invocation_confirmed_for_origin(
                    peer_id,
                    &resource,
                    &action,
                    InvocationInputs {
                        original_request: None,
                        requested: Some(invocation),
                        caller: None,
                        execution: None,
                        argument: argument.as_deref(),
                        actor: &actor,
                        confirmation_token: token.as_deref(),
                        context: None,
                    },
                    Some(InvocationCaller::Local {
                        route: origin_route.clone(),
                        time: original_time.clone(),
                        admitted: Box::new(current),
                    }),
                )
            } else {
                match &offered.contract.confirmation {
                    tilo_actions::ConfirmationPolicy::None => state
                        .start_invocation_confirmed_for_origin(
                            peer_id,
                            &resource,
                            &action,
                            InvocationInputs {
                                original_request: None,
                                requested: None,
                                caller: None,
                                execution: None,
                                argument: argument.as_deref(),
                                actor: &actor,
                                confirmation_token: None,
                                context: None,
                            },
                            Some(InvocationCaller::Local {
                                route: origin_route.clone(),
                                admitted: Box::new(current.clone()),
                                time: original_time.clone(),
                            }),
                        ),
                    tilo_actions::ConfirmationPolicy::Confirm => {
                        if confirmation
                            .get("confirm")
                            .and_then(serde_json::Value::as_bool)
                            != Some(true)
                        {
                            return Err(capnp::Error::failed(
                                "explicit confirmation is required".into(),
                            ));
                        }
                        let token = state
                            .prepare_confirmation(
                                peer_id,
                                &resource,
                                &action,
                                argument.as_deref(),
                                &actor.id,
                                None,
                            )
                            .map_err(capnp::Error::failed)?;
                        state.start_invocation_confirmed_for_origin(
                            peer_id,
                            &resource,
                            &action,
                            InvocationInputs {
                                original_request: None,
                                requested: None,
                                caller: None,
                                execution: None,
                                argument: argument.as_deref(),
                                actor: &actor,
                                confirmation_token: Some(&token),
                                context: None,
                            },
                            Some(InvocationCaller::Local {
                                route: origin_route.clone(),
                                admitted: Box::new(current.clone()),
                                time: original_time.clone(),
                            }),
                        )
                    }
                    tilo_actions::ConfirmationPolicy::Phrase { .. } => {
                        let token = state
                            .prepare_confirmation(
                                peer_id,
                                &resource,
                                &action,
                                argument.as_deref(),
                                &actor.id,
                                phrase,
                            )
                            .map_err(capnp::Error::failed)?;
                        state.start_invocation_confirmed_for_origin(
                            peer_id,
                            &resource,
                            &action,
                            InvocationInputs {
                                original_request: None,
                                requested: None,
                                caller: None,
                                execution: None,
                                argument: argument.as_deref(),
                                actor: &actor,
                                confirmation_token: Some(&token),
                                context: None,
                            },
                            Some(InvocationCaller::Local {
                                route: origin_route.clone(),
                                admitted: Box::new(current.clone()),
                                time: original_time.clone(),
                            }),
                        )
                    }
                }
            }
            .map_err(capnp::Error::failed)?
            .start(&state)
            .await
            .map_err(capnp::Error::failed)?;
            let update = state.invocation_status(job.id).ok_or_else(|| {
                capnp::Error::failed("accepted invocation has no broker state".into())
            })?;
            let update_json = serde_json::to_string(&update)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            results.get().set_id(job.id);
            results.get().set_update_json(&update_json);
            drop(job.completion);
            Ok(())
        })
    }

    fn published_invocation_status(
        self: Rc<Self>,
        params: channel_handle::PublishedInvocationStatusParams,
        mut results: channel_handle::PublishedInvocationStatusResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let id = match params.get() {
            Ok(request) => request.get_id(),
            Err(error) => return Promise::err(error),
        };
        // The legacy wire-id door observes only legacy actor-owned jobs.
        // Certificate-owned jobs require the host/UUID durable RPC, which
        // binds both the exact device and the authenticated agent.
        let owner = InvocationOwner::Local {
            actor: current.broker_actor().id,
        };
        let update = self.state.invocation_status_for_owner(id, &owner);
        let mut result = results.get();
        result.set_found(update.is_some());
        if let Some(update) = update {
            match serde_json::to_string(&update) {
                Ok(json) => result.set_update_json(&json),
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            }
        }
        Promise::ok(())
    }

    fn cancel_published_invocation(
        self: Rc<Self>,
        params: channel_handle::CancelPublishedInvocationParams,
        mut results: channel_handle::CancelPublishedInvocationResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let id = match params.get() {
            Ok(request) => request.get_id(),
            Err(error) => return Promise::err(error),
        };
        let owner = InvocationOwner::Local {
            actor: current.broker_actor().id,
        };
        if let Err(error) = self.state.request_cancellation_for_owner(id, &owner) {
            return Promise::err(capnp::Error::failed(error));
        }
        let Some(update) = self.state.invocation_status(id) else {
            return Promise::err(capnp::Error::failed("unknown invocation id".into()));
        };
        match serde_json::to_string(&update) {
            Ok(json) => {
                results.get().set_update_json(&json);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn launch_published_application(
        self: Rc<Self>,
        params: channel_handle::LaunchPublishedApplicationParams,
        mut results: channel_handle::LaunchPublishedApplicationResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let parent = match request
            .get_on_behalf_of()
            .and_then(observed_launch::launch_parent)
        {
            Ok(parent) => parent,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id() {
            Ok(value) => match value.to_str() {
                Ok(value) => value.to_owned(),
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            },
            Err(error) => return Promise::err(error),
        };
        let package_root = match request.get_package_root().and_then(|root| {
            root.to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
                .and_then(observed_launch::package_root)
        }) {
            Ok(root) => root,
            Err(error) => return Promise::err(error),
        };
        // An app opens another only for the person's press in it or an
        // invocation delivered to it ([`app_open_door`]); an agent's own
        // connection, by its own grants.
        if self.verified_identity.agent_id.is_none()
            && self
                .state
                .app_open_authority(self.peer_id, &self.route().app_id, parent)
                .is_none()
        {
            return Promise::err(capnp::Error::failed(
                "nothing the person or an invocation asked for is behind this launch".into(),
            ));
        }
        if let Err(error) = self.current_launch_authority(&app_id, parent) {
            return Promise::err(capnp::Error::failed(error));
        }

        let admitted = match self.admit_observed_launch(&app_id, package_root.as_deref(), parent) {
            Ok(admitted) => admitted,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        #[cfg(test)]
        self.observed_launch_effect_barrier();
        // Admission may block on durable storage. Discard the earlier policy
        // snapshot and judge both launch and authorship again at the effect.
        let started =
            self.current_launch_authority(&app_id, parent)
                .and_then(|(workspace, actor, now)| {
                    let author = observed_launch::launch_author(&actor, &workspace, now);
                    self.begin_original_launch(
                        &app_id,
                        package_root.as_deref(),
                        &workspace,
                        author.as_deref(),
                        parent,
                    )
                });
        Promise::from_future(async move {
            let outcome = observed_launch::wait_for_startup(started).await;
            let update = self.finish_observed_launch(&app_id, parent, &admitted, outcome)?;
            let id = admitted.id;
            let json = serde_json::to_string(&update)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            results.get().set_id(id);
            results.get().set_update_json(&json);
            Ok(())
        })
    }

    fn report_continuation(
        self: Rc<Self>,
        params: channel_handle::ReportContinuationParams,
        _results: channel_handle::ReportContinuationResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let route = self.route();
        if route.app_id.is_empty() || app_id != route.app_id {
            return Promise::err(capnp::Error::failed(
                "reportContinuation appId must match the certified connection route".into(),
            ));
        }
        let instance_id = match self.continuation_instance() {
            Ok(instance) => instance,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let Some(room) = tilo_desktop_model::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "reportContinuation route has no durable workspace room".into(),
            ));
        };
        let metadata = match request.get_metadata() {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => return Promise::err(error),
        };
        match self
            .state
            .services
            .continuations
            .as_ref()
            .ok_or_else(|| "continuation reporting unsupported".to_string())
            .and_then(|service| {
                service.report(
                    room,
                    app_id,
                    instance_id,
                    request.get_schema_version(),
                    metadata,
                )
            }) {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn handoff(
        self: Rc<Self>,
        params: channel_handle::HandoffParams,
        _results: channel_handle::HandoffResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        // The same certified primary route a continuation report is bound to:
        // an instance only ever speaks for itself.
        let route = self.route();
        if route.app_id.is_empty() || app_id != route.app_id {
            return Promise::err(capnp::Error::failed(
                "handoff appId must match the certified connection route".into(),
            ));
        }
        let instance_id = match self.continuation_instance() {
            Ok(instance) => instance,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let Some(room) = tilo_desktop_model::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "handoff route has no durable workspace room".into(),
            ));
        };
        let message = match request.get_message() {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => return Promise::err(error),
        };
        match self
            .state
            .services
            .continuations
            .as_ref()
            .ok_or_else(|| "handoff unsupported".to_string())
            .and_then(|service| service.handoff(room, app_id, instance_id, message))
        {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn clear_continuation(
        self: Rc<Self>,
        params: channel_handle::ClearContinuationParams,
        _results: channel_handle::ClearContinuationResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let route = self.route();
        if route.app_id.is_empty() || app_id != route.app_id {
            return Promise::err(capnp::Error::failed(
                "clearContinuation appId must match the certified connection route".into(),
            ));
        }
        let instance_id = match self.continuation_instance() {
            Ok(instance) => instance,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let Some(room) = tilo_desktop_model::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "clearContinuation route has no durable workspace room".into(),
            ));
        };
        match self
            .state
            .services
            .continuations
            .as_ref()
            .ok_or_else(|| "continuation clearing unsupported".to_string())
            .and_then(|service| service.clear(room, &app_id, instance_id))
        {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn publish_search_index(
        self: Rc<Self>,
        params: channel_handle::PublishSearchIndexParams,
        _results: channel_handle::PublishSearchIndexResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        // An application publishes only its own entries, in the workspace
        // and record its certified route names.
        let route = self.route();
        if route.app_id.is_empty() || app_id != route.app_id {
            return Promise::err(capnp::Error::failed(
                "publishSearchIndex appId must match the certified connection route".into(),
            ));
        }
        let Some(room) = tilo_desktop_model::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "publishSearchIndex route has no durable workspace room".into(),
            ));
        };
        let delta = match request.get_delta_json() {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(error),
        };
        if delta.len() > crate::services::MAX_SEARCH_INDEX_DELTA_BYTES {
            return Promise::err(capnp::Error::failed(
                "publishSearchIndex delta exceeds its bound".into(),
            ));
        }
        match self
            .state
            .services
            .search_index
            .as_ref()
            .ok_or_else(|| "search index publication unsupported".to_string())
            .and_then(|service| service.publish(room, &app_id, &route.record_id, delta))
        {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }
}

/// Validate every untrusted field before it reaches shell chrome, then
/// apply current authority. `Ok(false)` means a structurally valid row is
/// simply not visible to this actor; it must not suppress permitted
/// siblings on the same source page.
fn permits_directory_row(
    state: &CapnpBrokerState,
    cluster_id: &str,
    actor: &BrokerActor,
    app_id: &str,
    publishing_person: Option<&str>,
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<bool, capnp::Error> {
    let question = directory_row_question(row)?;
    if !question.disclosed_to(actor, publishing_person) {
        return Ok(false);
    }
    Ok(state
        .permits_directory_actions(cluster_id, actor, app_id, &[question])?
        .first()
        .is_some_and(|answer| *answer != Answer::Deny))
}

/// What current authority must decide for one directory row: the action
/// whose contract governs the row, where it sits and what kind it is.
struct DirectoryQuestion<'a> {
    resource_kind: &'a str,
    resource: &'a str,
    action: &'a str,
    field: Option<&'a str>,
    kind: ActionKind,
    private_view: bool,
}

impl<'a> DirectoryQuestion<'a> {
    /// The question the row's decoded contract asks. Resource-only rows
    /// carry the visibility contract.
    fn of(
        resource_kind: &'a str,
        resource: &'a str,
        action: &'a str,
        contract: &'a tilo_actions::ActionContract,
        operation: Option<tilo_actions::OperationKind>,
    ) -> Self {
        Self {
            resource_kind,
            resource,
            action,
            field: contract.field.as_deref(),
            kind: contract.kind,
            // Read kind alone does not make a private UI view public.
            // A declared document read, however, discloses document state
            // under the caller's ordinary resource policy, not owner UI.
            // That exception requires a known View kind: a tag alone cannot
            // disclose resource-only metadata with an unknown governing kind.
            private_view: (operation == Some(tilo_actions::OperationKind::View)
                || operation.is_none())
                && !contract.is_participant_view()
                && !(operation == Some(tilo_actions::OperationKind::View)
                    && contract.reads_document()),
        }
    }

    fn disclosed_to(&self, actor: &BrokerActor, publishing_person: Option<&str>) -> bool {
        !self.private_view || (!actor.is_agent && publishing_person == Some(actor.id.as_str()))
    }
}

/// Validate every capability-bearing field of one untrusted directory row
/// and name the policy question it asks. No authority is read here.
#[allow(
    clippy::too_many_lines,
    reason = "directory row authorization validates every capability-bearing field before the result reaches shell search"
)]
fn directory_row_question(
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<DirectoryQuestion<'_>, capnp::Error> {
    validate_directory_identifier(
        &row.parent_resource,
        "parent resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        false,
    )?;
    validate_directory_identifier(
        &row.resource,
        "resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_identifier(
        &row.resource_kind,
        "resource kind",
        DIRECTORY_MAX_KIND_BYTES,
        true,
    )?;
    validate_directory_text(
        &row.resource_label,
        "resource label",
        DIRECTORY_MAX_LABEL_BYTES,
        true,
    )?;
    match (&row.action, &row.descriptor) {
        (Some(action), Some(descriptor)) => {
            validate_directory_identifier(action, "action", DIRECTORY_MAX_IDENTIFIER_BYTES, true)?;
            if descriptor.resource != row.resource
                || descriptor.action != *action
                || descriptor.resource_kind != row.resource_kind
                || descriptor.resource_label != row.resource_label
                || row.has_children
                || row.visibility.as_ref().is_none_or(|visibility| {
                    visibility.action != *action
                        || visibility.contract != descriptor.contract
                        || visibility
                            .governing_kind
                            .is_some_and(|kind| kind != descriptor.operation_kind)
                })
            {
                return Err(capnp::Error::failed(
                    "directory action row does not match its descriptor".into(),
                ));
            }
            validate_directory_text(
                &descriptor.action_label,
                "action label",
                DIRECTORY_MAX_LABEL_BYTES,
                true,
            )?;
            if descriptor
                .description
                .as_deref()
                .is_some_and(|description| {
                    description.len() > DIRECTORY_MAX_DESCRIPTION_BYTES
                        || description.contains('\0')
                })
            {
                return Err(capnp::Error::failed(
                    "invalid action directory description".into(),
                ));
            }
            match &descriptor.input {
                runtime_collab::ExportedInput::None => {}
                runtime_collab::ExportedInput::Text {
                    prompt,
                    placeholder,
                    ..
                } => {
                    validate_directory_text(
                        prompt,
                        "input prompt",
                        DIRECTORY_MAX_LABEL_BYTES,
                        true,
                    )?;
                    validate_directory_text(
                        placeholder,
                        "input placeholder",
                        DIRECTORY_MAX_LABEL_BYTES,
                        false,
                    )?;
                }
            }
            let published = published_directory_action(row)?;
            validate_resource_view_descriptors(std::slice::from_ref(&published))?;
            Ok(DirectoryQuestion::of(
                &row.resource_kind,
                &row.resource,
                action,
                &descriptor.contract,
                Some(descriptor.operation_kind),
            ))
        }
        (None, None) => {
            let Some(visibility) = &row.visibility else {
                return Err(capnp::Error::failed(
                    "directory resource row has no visibility contract".into(),
                ));
            };
            validate_directory_identifier(
                &visibility.action,
                "visibility action",
                DIRECTORY_MAX_IDENTIFIER_BYTES,
                true,
            )?;
            Ok(DirectoryQuestion::of(
                &row.resource_kind,
                &row.resource,
                &visibility.action,
                &visibility.contract,
                visibility.governing_kind,
            ))
        }
        _ => Err(capnp::Error::failed(
            "directory row mixes resource and action shapes".into(),
        )),
    }
}

fn filter_directory_page(
    state: &CapnpBrokerState,
    cluster_id: &str,
    actor: &BrokerActor,
    app_id: &str,
    publishing_person: Option<&str>,
    mut page: runtime_collab::ActionDirectoryPage,
    max_results: u32,
) -> Result<runtime_collab::ActionDirectoryPage, capnp::Error> {
    validate_directory_text(
        &page.revision,
        "directory revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    if page.next_cursor.len() > DIRECTORY_MAX_CURSOR_BYTES {
        return Err(capnp::Error::failed(
            "invalid directory continuation".into(),
        ));
    }
    if page.rows.len() > usize::try_from(max_results).unwrap_or(usize::MAX) {
        return Err(capnp::Error::failed(
            "application exceeded directory result budget".into(),
        ));
    }
    let page_actions = page
        .rows
        .iter()
        .filter(|row| row.action.is_some())
        .map(published_directory_action)
        .collect::<Result<Vec<_>, _>>()?;
    validate_resource_view_descriptors(&page_actions)?;
    let questions = page
        .rows
        .iter()
        .map(directory_row_question)
        .collect::<Result<Vec<_>, _>>()?;
    let mut answers = state.permits_directory_actions(cluster_id, actor, app_id, &questions)?;
    for (answer, question) in answers.iter_mut().zip(&questions) {
        if !question.disclosed_to(actor, publishing_person) {
            *answer = Answer::Deny;
        }
    }
    drop(questions);
    // Listed when the actor may take it, or would be asked first; an action
    // asking first asks for the confirmation the person gives on their
    // own device.
    page.rows = page
        .rows
        .into_iter()
        .zip(answers)
        .filter_map(|(mut row, answer)| match answer {
            Answer::Deny => None,
            Answer::Allow => Some(row),
            Answer::Ask => {
                if let Some(descriptor) = row.descriptor.as_mut()
                    && descriptor.contract.confirmation == tilo_actions::ConfirmationPolicy::None
                {
                    descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                }
                Some(row)
            }
        })
        .collect();
    Ok(page)
}

/// Convert the already validated, exact directory row into the inert action
/// shape used by the invocation lifecycle.  This deliberately accepts only
/// an action row: containers are discoverable but never invokable.
fn published_directory_action(
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<PublishedAction, capnp::Error> {
    let action = row
        .action
        .as_deref()
        .ok_or_else(|| capnp::Error::failed("directory container is not invokable".into()))?;
    let descriptor = row
        .descriptor
        .as_ref()
        .ok_or_else(|| capnp::Error::failed("directory action has no descriptor".into()))?;
    if descriptor.resource != row.resource || descriptor.action != action {
        return Err(capnp::Error::failed(
            "directory descriptor address changed during conversion".into(),
        ));
    }
    if descriptor
        .semantic_identity
        .as_ref()
        .is_some_and(|identity| !identity.is_valid() || identity.kind != descriptor.resource_kind)
    {
        return Err(capnp::Error::failed(
            "invalid directory resource semantic identity".into(),
        ));
    }
    Ok(descriptor.clone())
}

fn revalidate_workspace_control(
    state: &CapnpBrokerState,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Result<AdmittedJoinIdentity, capnp::Error> {
    let current = state.revalidate_admission(route, identity)?;
    if !workspace_control_role_allowed(current.role) {
        return Err(capnp::Error::failed(
            "workspace control requires current editor authority".into(),
        ));
    }
    Ok(current)
}

fn workspace_control_role_allowed(role: collab_types::Role) -> bool {
    matches!(role, collab_types::Role::Owner | collab_types::Role::Editor)
}

/// Apply a signed op arriving via capnp `send`: same validation /
/// audit / redaction / compaction pipeline, then
/// fans out via the shared [`FanoutState`].
#[cfg(test)]
fn handle_signed_op_send(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    envelope: SignedOp,
) {
    let artifact = envelope.artifact;
    handle_signed_op_send_for_artifact(
        state,
        peer_id,
        route,
        verified_identity,
        artifact,
        envelope,
    );
}

/// The broker's current policy time as a wall-clock instant.
fn policy_system_time(state: &CapnpBrokerState) -> Option<std::time::SystemTime> {
    let now = state.policy_now_ns().ok()?;
    std::time::UNIX_EPOCH.checked_add(Duration::from_nanos(now))
}

/// `envelope` validated against its author's current authority in the room
/// and the channel's state, or `None` when either is unavailable.
fn validate_signed_op(
    state: &Arc<CapnpBrokerState>,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    artifact: ArtifactId,
    envelope: &SignedOp,
) -> Option<Result<(), ValidationError>> {
    let now = policy_system_time(state)?;
    let validation = verified_identity.with_current_authority(
        state.membership.as_ref(),
        &route.cluster_id,
        |author| {
            let Ok(mut ch) = state.broker_channels.lock() else {
                return None;
            };
            let ch_state = ch.state_for(artifact);
            ch_state.room.clone_from(&route.cluster_id);
            ch_state.artifacts.insert(artifact, ArtifactKind::Note);
            ch_state
                .artifact_apps
                .insert(artifact, route.app_id.clone());
            ch_state
                .artifact_records
                .insert(artifact, route.record_id.clone());
            Some(validate_admitted_inbound_op(
                envelope,
                author,
                ch_state,
                now,
                Duration::from_secs(300),
            ))
        },
    );
    match validation {
        Ok(Some(validation)) => Some(validation),
        Ok(None) => None,
        Err(error) => Some(Err(ValidationError::NotGranted(error))),
    }
}

fn handle_signed_op_send_for_artifact(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    artifact: ArtifactId,
    envelope: SignedOp,
) {
    let Some(validation) = validate_signed_op(state, route, verified_identity, artifact, &envelope)
    else {
        return;
    };
    match validation {
        Ok(()) => {
            if let Ok(mut ch) = state.broker_channels.lock() {
                ch.record_history(route, &envelope);
                let ch_state = ch.state_for(artifact);
                ch_state.record_validated_op(envelope.clone());
                if envelope.kind == collab_types::OpKind::Compaction
                    && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
                {
                    let _ = ch_state.apply_compaction(&cp);
                }
            }
            if envelope.kind == collab_types::OpKind::Redaction {
                apply_redaction_from_envelope(&envelope, artifact, &state.broker_channels);
            }
            // Republishing is not vouching.
            if !state.durable_owner_required {
                state.publisher().signed_document(route, envelope.clone());
            }
            let from_user_id = CapnpBrokerState::participant_id(verified_identity);
            let event = CollabEvent::Document {
                envelope: Box::new(envelope),
                from_user_id,
                decrypted_payload: None,
            };
            if let Some(t) = state.fanout.upgrade() {
                t.fanout_to_route(&event, peer_id, route);
            }
        }
        Err(ValidationError::OpAlreadyRedacted { target }) => {
            tracing::warn!(
                artifact = ?artifact,
                seq = envelope.seq,
                target = ?target,
                "capnp send: late-arriving op was already redacted; storing marker",
            );
            if let Ok(mut ch) = state.broker_channels.lock() {
                let ch_state = ch.state_for(artifact);
                let mut marker_envelope = envelope.clone();
                if let Ok(marker_bytes) = RedactedMarker::v1().canonical() {
                    marker_envelope.payload = marker_bytes;
                }
                ch_state.record_validated_op(marker_envelope);
                ch.audit.record(
                    artifact,
                    AuditEntry {
                        envelope: Box::new(envelope),
                        rejection: Some(ValidationError::OpAlreadyRedacted { target }),
                        at: std::time::SystemTime::now(),
                    },
                );
            }
        }
        Err(reason) => {
            tracing::warn!(
                artifact = ?artifact,
                author = ?envelope.author,
                seq = envelope.seq,
                "capnp send: dropping invalid signed op: {reason}",
            );
            if let Ok(mut ch) = state.broker_channels.lock() {
                ch.audit.record(
                    artifact,
                    AuditEntry {
                        envelope: Box::new(envelope),
                        rejection: Some(reason),
                        at: std::time::SystemTime::now(),
                    },
                );
            }
        }
    }
}

fn handle_authorized_signed_op_send(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    authorized: &AuthorizedDocumentRoute,
    envelope: SignedOp,
) {
    debug_assert_eq!(authorized.artifact, envelope.artifact);
    debug_assert_eq!(
        authorized.channel,
        match &authorized.payload {
            collab_types::RoutedDocumentPayload::Cleartext(_)
            | collab_types::RoutedDocumentPayload::CleartextSegment(_) => {
                collab_types::document_channel_id(
                    &route.app_id,
                    &route.cluster_id,
                    &route.record_id,
                )
            }
            collab_types::RoutedDocumentPayload::Encrypted { channel, .. } => *channel,
        }
    );
    handle_signed_op_send_for_artifact(
        state,
        peer_id,
        route,
        verified_identity,
        authorized.artifact,
        envelope,
    );
}

/// Read a page of scrollback ops for `artifact`, encoded as postcard
/// bytes since the capnp `ScrollbackBatch.ops` is `List(Data)`.
fn read_scrollback(
    broker_channels: &Arc<Mutex<BrokerChannels>>,
    artifact: ArtifactId,
    before_seq: u64,
    max: u32,
) -> (Vec<Vec<u8>>, bool) {
    let Ok(mut ch) = broker_channels.lock() else {
        return (Vec::new(), true);
    };
    // `max` is a peer-supplied page size; clamp rather than wrap so an
    // oversized request degrades to "as many as the u16 cap allows"
    // instead of silently truncating to an unrelated smaller number.
    let capped_max = u16::try_from(max).unwrap_or(u16::MAX);
    let (ops, exhausted) = ch.scrollback_page(artifact, before_seq, capped_max);
    let encoded: Vec<Vec<u8>> = ops
        .into_iter()
        .filter_map(|op| postcard::to_stdvec(&op).ok())
        .collect();
    (encoded, exhausted)
}

/// Encode a scrollback page into the capnp response.
///
/// # Errors
///
/// Returns an error if `ops` holds more entries than fit in a capnp
/// `List` count (`u32`) — unreachable in practice since `ops` is always
/// capped by the `u16` `max` accepted in [`read_scrollback`], but checked
/// rather than assumed since this is peer-request-shaped data.
fn fill_scrollback_batch(
    mut b: scrollback_batch::Builder<'_>,
    ops: &[Vec<u8>],
    exhausted: bool,
) -> Result<(), capnp::Error> {
    let n = u32::try_from(ops.len())
        .map_err(|_| capnp::Error::failed("scrollback batch too large to encode".into()))?;
    let mut list = b.reborrow().init_ops(n);
    for (i, op) in ops.iter().enumerate() {
        let idx = u32::try_from(i)
            .map_err(|_| capnp::Error::failed("scrollback batch index overflow".into()))?;
        list.set(idx, op);
    }
    // Bracket seq isn't tracked; consumers needing bounds decode the ops.
    b.set_earliest_seq(0);
    b.set_latest_seq(0);
    b.set_done(exhausted);
    Ok(())
}

// Presence handle + streams

struct CapnpPresenceHandle {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
    /// Broker-owned monotonic sequence per invocation. Publishers describe
    /// phases and targets but cannot reorder or resurrect their visual state.
    interaction_sequences: Mutex<HashMap<tilo_actions::InvocationId, u64>>,
}

impl CapnpPresenceHandle {
    /// Widened to room scope — presence is room-wide, only document
    /// payloads are confined to a record.
    fn route(&self) -> CrdtRoute {
        self.full_route().to_cluster_scope()
    }

    /// Unwidened, so peer replication carries the full app/cluster route.
    fn full_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn revalidate(&self) -> Result<(), capnp::Error> {
        let route = if self.verified_identity.host_scope.is_some() {
            self.full_route()
        } else {
            self.route()
        };
        self.state
            .revalidate_admission(&route, &self.verified_identity)
            .map(|_| ())
    }

    /// Publish one typed keyed-presence update. A malformed key is isolated
    /// to this update rather than taking down the broker.
    fn publish_keyed(&self, key_bytes: &[u8], payload: PresencePayload) {
        let Ok(key) = postcard::from_bytes::<PresenceKey>(key_bytes) else {
            tracing::debug!(
                len = key_bytes.len(),
                "capnp presence publish: key bytes not decodable — dropping",
            );
            return;
        };
        let route = self.route();
        self.state
            .publish_presence(&self.full_route(), &key, &payload);
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: CapnpBrokerState::participant_id(&self.verified_identity),
            key,
            payload,
        };
        if let Some(fanout) = self.state.fanout.upgrade() {
            fanout.fanout_presence(&event, self.peer_id, &route, &self.full_route());
        }
    }
}

impl presence_handle::Server for CapnpPresenceHandle {
    fn publish_cursor(
        self: Rc<Self>,
        params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let full_route = self.full_route();
        let verified_identity = self.verified_identity.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let (x, y) = (r.get_x(), r.get_y());
            // Cursor stream is keyless; full presence uses pointer_default().
            let event = CollabEvent::RemoteCursorMoved {
                user_id: CapnpBrokerState::participant_id(&verified_identity),
                x,
                y,
            };
            state.publish_presence(
                &full_route,
                &PresenceKey::pointer_default(),
                &PresencePayload::Pointer { x, y },
            );
            if let Some(t) = state.fanout.upgrade() {
                t.fanout_presence(&event, peer_id, &route, &full_route);
            }
            Ok(())
        })
    }

    fn publish_pointer(
        self: Rc<Self>,
        params: presence_handle::PublishPointerParams,
        _results: presence_handle::PublishPointerResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            self.publish_keyed(
                &key_bytes,
                PresencePayload::Pointer {
                    x: r.get_x(),
                    y: r.get_y(),
                },
            );
            Ok(())
        })
    }

    fn publish_caret(
        self: Rc<Self>,
        params: presence_handle::PublishCaretParams,
        _results: presence_handle::PublishCaretResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            // `pos` is a peer-supplied offset; reject rather than wrap it
            // onto an unrelated position on a 32-bit target.
            let pos = usize::try_from(r.get_pos()).map_err(|_| {
                capnp::Error::failed("publishCaret: pos overflows usize on this platform".into())
            })?;
            self.publish_keyed(&key_bytes, PresencePayload::Caret { pos });
            Ok(())
        })
    }

    fn publish_selection(
        self: Rc<Self>,
        params: presence_handle::PublishSelectionParams,
        _results: presence_handle::PublishSelectionResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            // Both are peer-supplied offsets; reject rather than wrap
            // them onto an unrelated range on a 32-bit target.
            let start = usize::try_from(r.get_start()).map_err(|_| {
                capnp::Error::failed(
                    "publishSelection: start overflows usize on this platform".into(),
                )
            })?;
            let end = usize::try_from(r.get_end()).map_err(|_| {
                capnp::Error::failed(
                    "publishSelection: end overflows usize on this platform".into(),
                )
            })?;
            self.publish_keyed(&key_bytes, PresencePayload::Selection { start, end });
            Ok(())
        })
    }

    fn publish_typing(
        self: Rc<Self>,
        params: presence_handle::PublishTypingParams,
        _results: presence_handle::PublishTypingResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            let start = usize::try_from(r.get_start()).map_err(|_| {
                capnp::Error::failed("publishTyping: start overflows usize on this platform".into())
            })?;
            let end = usize::try_from(r.get_end()).map_err(|_| {
                capnp::Error::failed("publishTyping: end overflows usize on this platform".into())
            })?;
            self.publish_keyed(&key_bytes, PresencePayload::Typing { start, end });
            Ok(())
        })
    }

    fn publish_interaction(
        self: Rc<Self>,
        params: presence_handle::PublishInteractionParams,
        _results: presence_handle::PublishInteractionResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            let json = r.get_interaction_json()?.to_str().map_err(|error| {
                capnp::Error::failed(format!("invalid interaction utf8: {error}"))
            })?;
            if json.len() > 64 * 1024 {
                return Err(capnp::Error::failed(
                    "interaction payload exceeds 64 KiB".into(),
                ));
            }
            let mut update: tilo_actions::SemanticInteraction = serde_json::from_str(json)
                .map_err(|error| capnp::Error::failed(format!("invalid interaction: {error}")))?;
            let actor = self.verified_identity.broker_actor();
            let normalized_actor = tilo_actions::InteractionActor::new(
                CapnpBrokerState::participant_id(&self.verified_identity),
                if actor.is_agent {
                    tilo_actions::ActorKind::Agent
                } else {
                    tilo_actions::ActorKind::Person
                },
            );
            let sequence = {
                let mut sequences = self.interaction_sequences.lock().map_err(|_| {
                    capnp::Error::failed("interaction sequence registry unavailable".into())
                })?;
                let current = sequences.entry(update.invocation).or_insert(0);
                let next = current.saturating_add(1);
                *current = next;
                // Persist the monotonic sequence before validating and
                // publishing the interaction outside this mutex.
                drop(sequences);
                next
            };
            update = update.normalized_by_broker(normalized_actor, sequence);
            update
                .validate()
                .map_err(|error| capnp::Error::failed(format!("invalid interaction: {error}")))?;
            self.publish_keyed(&key_bytes, PresencePayload::Interaction(Box::new(update)));
            Ok(())
        })
    }

    fn publish_status(
        self: Rc<Self>,
        params: presence_handle::PublishStatusParams,
        _results: presence_handle::PublishStatusResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let request = params.get()?;
            let key_bytes = request.get_key_bytes()?.to_vec();
            let raw = request
                .get_text()?
                .to_str()
                .map_err(|error| capnp::Error::failed(format!("invalid status utf8: {error}")))?;
            let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.chars().count() > ui::MAX_STATUS_CHARS {
                return Err(capnp::Error::failed(
                    "status exceeds local protocol bound".into(),
                ));
            }
            self.publish_keyed(
                &key_bytes,
                PresencePayload::Status {
                    text,
                    automatic: request.get_automatic(),
                },
            );
            Ok(())
        })
    }

    fn publish_arrived(
        self: Rc<Self>,
        params: presence_handle::PublishArrivedParams,
        _results: presence_handle::PublishArrivedResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            self.publish_keyed(&key_bytes, PresencePayload::Arrived);
            Ok(())
        })
    }

    fn publish_left(
        self: Rc<Self>,
        params: presence_handle::PublishLeftParams,
        _results: presence_handle::PublishLeftResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            self.publish_keyed(&key_bytes, PresencePayload::Left);
            Ok(())
        })
    }

    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        let rx = self
            .state
            .subscribe_presence(&self.channel_id, self.peer_id);
        let client: presence_stream::Client = capnp_rpc::new_client(PresenceStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }
}

/// A returned stream retains the local descriptor's host identity, not a past
/// permission decision. Both sides of every await recheck current authority.
struct HostWorkerReadGuard {
    state: Arc<CapnpBrokerState>,
    peer_id: u64,
    identity: AdmittedJoinIdentity,
}
impl HostWorkerReadGuard {
    fn for_channel(
        state: &Arc<CapnpBrokerState>,
        peer_id: u64,
        identity: &AdmittedJoinIdentity,
    ) -> Option<Self> {
        identity.host_scope.as_ref().map(|_| Self {
            state: state.clone(),
            peer_id,
            identity: identity.clone(),
        })
    }
    fn check(guard: Option<&Self>) -> Result<(), capnp::Error> {
        if let Some(guard) = guard {
            let scope = guard
                .identity
                .host_scope
                .as_ref()
                .ok_or_else(|| capnp::Error::failed("host worker scope missing".into()))?;
            if !guard
                .state
                .is_current_host_worker(guard.peer_id, &scope.route, &scope.certificate)
                .map_err(capnp::Error::failed)?
            {
                return Err(capnp::Error::failed("host worker no longer current".into()));
            }
        }
        Ok(())
    }
}

// Stream servers

// The four stream servers below share a shape: an `Rc<AsyncMutex<mpsc::UnboundedReceiver<_>>>`
// rather than an `Rc<RefCell<_>>`. capnp-rpc will happily dispatch a second
// pipelined `read()` on the same stream before the first one's `recv().await`
// resolves; a `RefCell` would then panic on the second `borrow_mut()` (and
// this crate is `panic = "abort"`, so that panic takes the whole desktop
// session down). The async mutex instead makes the second reader simply wait
// its turn — the two calls serialize instead of racing.

struct OpStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<OpEnvelope>>>,
}

impl stream::Server for OpStreamServer {
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut guard = rx.lock().await;
            let Some(env) = guard.recv().await else {
                return Err(capnp::Error::disconnected("op stream fanout closed".into()));
            };
            drop(guard);
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut r = results.get();
            r.set_op(&env.op_bytes);
            r.set_from_user_id(&env.from_user_id);
            r.set_kind(env.kind);
            r.set_admission(env.admission);
            Ok(())
        })
    }
}

struct PresenceStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<PresenceEnvelope>>>,
}

struct RealtimeStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
}

impl realtime_stream::Server for RealtimeStreamServer {
    fn read(
        self: Rc<Self>,
        _params: realtime_stream::ReadParams,
        mut results: realtime_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut guard = rx.lock().await;
            let event = guard.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("realtime continuity stream closed".into())
            })?;
            drop(guard);
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            results.get().set_event_json(&event);
            Ok(())
        })
    }
}

/// What the host is told each time an invocation is handed to `app` in
/// `room`; nothing when no launcher hosts it.
fn invocation_delivery_notice(
    services: &HostServices,
    app: String,
    room: String,
) -> Option<Box<dyn Fn()>> {
    let launcher = services.launcher.clone().filter(|_| !app.is_empty())?;
    Some(Box::new(move || launcher.note_acted_on(&app, &room)))
}

type InvocationQueue = Rc<AsyncMutex<mpsc::UnboundedReceiver<InvocationEnvelope>>>;

#[cfg(test)]
thread_local! {
    /// Each peer's latest invocation queue, kept for [`unread_invocations`].
    static INVOCATION_QUEUES: std::cell::RefCell<HashMap<u64, std::rc::Weak<AsyncMutex<mpsc::UnboundedReceiver<InvocationEnvelope>>>>> =
        std::cell::RefCell::default();
}

fn invocation_queue(peer: u64, rx: mpsc::UnboundedReceiver<InvocationEnvelope>) -> InvocationQueue {
    let queue = Rc::new(AsyncMutex::new(rx));
    #[cfg(test)]
    INVOCATION_QUEUES.with(|queues| {
        queues.borrow_mut().insert(peer, Rc::downgrade(&queue));
    });
    #[cfg(not(test))]
    let _ = peer;
    queue
}

/// How many invocations were handed to `peer`'s current stream and not read
/// yet. A test asks between reads, on the thread that serves the broker:
/// whatever the broker dispatched by then is counted, however slowly the
/// stream would carry it.
#[cfg(test)]
#[track_caller]
pub(crate) fn unread_invocations(peer: u64) -> usize {
    let queue = INVOCATION_QUEUES
        .with(|queues| queues.borrow().get(&peer).and_then(std::rc::Weak::upgrade))
        .unwrap_or_else(|| panic!("peer {peer} holds no invocation stream on this thread"));
    let queue = queue
        .try_lock()
        .unwrap_or_else(|_| panic!("a read of peer {peer}'s invocations is outstanding"));
    queue.len()
}

struct InvocationStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    rx: InvocationQueue,
    /// Told each time an invocation is handed to the app.
    delivered: Option<Box<dyn Fn()>>,
}

struct TaskRunStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<TaskRunEnvelope>>>,
}

struct ActionCatalogueStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    reader: Rc<AsyncMutex<ActionCatalogueReader>>,
}

struct ActionCatalogueReader {
    rx: watch::Receiver<ActionCatalogueSnapshot>,
    initial: bool,
}

impl ActionCatalogueReader {
    async fn next(&mut self) -> Result<Arc<str>, ()> {
        if self.initial {
            self.initial = false;
        } else {
            self.rx.changed().await.map_err(|_| ())?;
        }
        let snapshot = self.rx.borrow_and_update().clone();
        snapshot.ok_or(())
    }
}

impl action_catalogue_stream::Server for ActionCatalogueStreamServer {
    fn read(
        self: Rc<Self>,
        _params: action_catalogue_stream::ReadParams,
        mut results: action_catalogue_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let reader = self.reader.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let snapshot = reader.lock().await.next().await.map_err(|()| {
                capnp::Error::disconnected("action catalogue stream closed".into())
            })?;
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            results.get().set_catalogue_json(&snapshot);
            Ok(())
        })
    }
}

impl invocation_stream::Server for InvocationStreamServer {
    fn read(
        self: Rc<Self>,
        _params: invocation_stream::ReadParams,
        mut results: invocation_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut guard = rx.lock().await;
            let invocation = guard.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("application invocation stream closed".into())
            })?;
            drop(guard);
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            if let Some(delivered) = &self.delivered {
                delivered();
            }
            let mut result = results.get();
            result.set_id(invocation.id);
            result.set_invocation_uuid(invocation.invocation.0.as_bytes());
            result.set_resource(&invocation.resource);
            result.set_action(&invocation.action);
            result.set_argument(invocation.argument.as_deref().unwrap_or(""));
            result.set_has_argument(invocation.argument.is_some());
            result.set_actor(&invocation.actor);
            let context = invocation
                .context
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| capnp::Error::failed(error.to_string()))?
                .unwrap_or_default();
            result.set_context_json(&context);
            Ok(())
        })
    }
}

impl task_run_stream::Server for TaskRunStreamServer {
    fn read(
        self: Rc<Self>,
        _params: task_run_stream::ReadParams,
        mut results: task_run_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut guard = rx.lock().await;
            let run = guard
                .recv()
                .await
                .ok_or_else(|| capnp::Error::disconnected("agent task stream closed".into()))?;
            drop(guard);
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let payload = runtime_collab::TaskRunEnvelope {
                paused: run.paused,
                version: 1,
                task_id: run.task_id,
                title: run.title,
                instruction: run.instruction,
                subject: run.subject,
                run_id: run.run_id,
                presentation_version: Some(1),
                lease: run.lease,
                planning: run.planning,
                execution_budget: run.execution_budget,
                action_context: serde_json::to_value(run.action_context)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
                model: run.model,
            };
            results.get().set_task_json(
                &serde_json::to_string(&payload)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
            );
            Ok(())
        })
    }
}

impl presence_stream::Server for PresenceStreamServer {
    fn read(
        self: Rc<Self>,
        _params: presence_stream::ReadParams,
        mut results: presence_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut guard = rx.lock().await;
            let Some(env) = guard.recv().await else {
                return Err(capnp::Error::disconnected(
                    "presence stream fanout closed".into(),
                ));
            };
            drop(guard);
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            let mut ev: presence_event::Builder = results.get().init_event();
            ev.set_user_id(&env.user_id);
            ev.set_key_bytes(&env.key_bytes);
            let mut payload: presence_payload::Builder = ev.init_payload();
            match env.payload {
                PresencePayload::Pointer { x, y } => {
                    let mut c = payload.init_pointer();
                    c.set_x(x);
                    c.set_y(y);
                }
                PresencePayload::Caret { pos } => {
                    payload.init_caret().set_pos(pos as u64);
                }
                PresencePayload::Selection { start, end } => {
                    let mut s = payload.init_selection();
                    s.set_start(start as u64);
                    s.set_end(end as u64);
                }
                PresencePayload::Typing { start, end } => {
                    let mut s = payload.init_typing();
                    s.set_start(start as u64);
                    s.set_end(end as u64);
                }
                PresencePayload::Following { target } => {
                    payload.set_following(target.as_deref().unwrap_or(""));
                }
                PresencePayload::Interaction(update) => {
                    let json = serde_json::to_string(&update).map_err(|error| {
                        capnp::Error::failed(format!("encode semantic interaction: {error}"))
                    })?;
                    payload.set_interaction_json(&json);
                }
                PresencePayload::Status { text, automatic } => {
                    let mut status = payload.init_status();
                    status.set_text(&text);
                    status.set_automatic(automatic);
                }
                PresencePayload::Arrived => payload.set_arrived(()),
                PresencePayload::Left => payload.set_left(()),
                PresencePayload::Availability { .. } => {
                    return Err(capnp::Error::failed(
                        "desktop presence is never fanned to an app".into(),
                    ));
                }
            }
            Ok(())
        })
    }
}

struct ContextStreamServer {
    host_guard: Option<HostWorkerReadGuard>,
    native_guard: Option<NativeContextReadGuard>,
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<ContextEnvelope>>>,
}

struct NativeContextReadGuard {
    state: Arc<CapnpBrokerState>,
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
    /// Native private views retain their original document's current Read custody.
    document: Option<NativeDocumentReadContext>,
}

impl NativeContextReadGuard {
    fn check(guard: Option<&Self>) -> Result<(), capnp::Error> {
        if let Some(guard) = guard {
            if !guard
                .state
                .services
                .workspace_memberships
                .as_ref()
                .is_some_and(|owner| owner.requires_room(&guard.route.cluster_id))
            {
                return Err(capnp::Error::failed(
                    "native context owner is no longer current".into(),
                ));
            }
            guard
                .state
                .revalidate_admission(&guard.route, &guard.identity)?;
            if let Some(context) = &guard.document {
                context.revalidate().map_err(capnp::Error::failed)?;
                guard
                    .state
                    .services
                    .committed_documents
                    .as_ref()
                    .ok_or_else(|| {
                        capnp::Error::failed("native private document owner ended".into())
                    })?
                    .authorize(context)
                    .map_err(capnp::Error::failed)?;
                context.revalidate().map_err(capnp::Error::failed)?;
            }
        }
        Ok(())
    }
}

impl context_stream::Server for ContextStreamServer {
    fn read(
        self: Rc<Self>,
        _params: context_stream::ReadParams,
        mut results: context_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            NativeContextReadGuard::check(self.native_guard.as_ref())?;
            let mut guard = rx.lock().await;
            let Some(env) = guard.recv().await else {
                return Err(capnp::Error::disconnected(
                    "context stream fanout closed".into(),
                ));
            };
            drop(guard);
            HostWorkerReadGuard::check(self.host_guard.as_ref())?;
            NativeContextReadGuard::check(self.native_guard.as_ref())?;
            let mut ctx = results.get().init_ctx();
            let (collaborators, permissions) = match env {
                ContextEnvelope::Snapshot {
                    collaborators,
                    permissions,
                } => (collaborators, permissions),
                ContextEnvelope::FollowChanged { peer_id } => {
                    ctx.set_follow_changed(true);
                    ctx.set_follow_target(peer_id.as_deref().unwrap_or(""));
                    return Ok(());
                }
                ContextEnvelope::SnapshotRequested => {
                    ctx.set_snapshot_requested(true);
                    return Ok(());
                }
                ContextEnvelope::Handoff(message) => {
                    ctx.set_handoff(&message);
                    return Ok(());
                }
                ContextEnvelope::HistoryAgents(agents) => {
                    encode_history_agents(&mut ctx, &agents)?;
                    return Ok(());
                }
                ContextEnvelope::RecordsArrived(records) => {
                    let count = u32::try_from(records.len())
                        .map_err(|_| capnp::Error::failed("context: too many records".into()))?;
                    let mut arrived = ctx.init_records_arrived(count);
                    (0..count)
                        .zip(&records)
                        .for_each(|(i, r)| arrived.set(i, r.as_str()));
                    return Ok(());
                }
            };
            let collab_count = u32::try_from(collaborators.len()).map_err(|_| {
                capnp::Error::failed("context: too many collaborators to encode".into())
            })?;
            {
                let mut collabs = ctx.reborrow().init_collaborators(collab_count);
                for (i, c) in collaborators.iter().enumerate() {
                    // `i < env.collaborators.len()`, already proven to fit
                    // in `u32` above; `u32::MAX` is unreachable here.
                    let idx = u32::try_from(i).unwrap_or(u32::MAX);
                    let mut slot = collabs.reborrow().get(idx);
                    slot.set_user_id(&c.id);
                    slot.set_display_name(&c.name);
                    slot.set_color(c.packed_colour());
                    slot.set_is_agent(c.is_agent);
                    slot.set_is_self(c.is_self);
                    slot.set_joined_at_ms(c.packed_joined_at());
                }
            }
            let perm_count = u32::try_from(permissions.len()).map_err(|_| {
                capnp::Error::failed("context: too many permissions to encode".into())
            })?;
            {
                let mut perms = ctx.init_permissions(perm_count);
                for (i, p) in permissions.iter().enumerate() {
                    let idx = u32::try_from(i).unwrap_or(u32::MAX);
                    perms.set(idx, p.as_str());
                }
            }
            Ok(())
        })
    }
}

/// Write a session-local frame naming a room's verified agents: clients
/// leave the roster untouched and replace the room's agents with these.
fn encode_history_agents(
    ctx: &mut runtime_collab::raw::channel_context::Builder<'_>,
    agents: &[collab_types::history::RosterAgent],
) -> Result<(), capnp::Error> {
    let count = u32::try_from(agents.len())
        .map_err(|_| capnp::Error::failed("context: too many history agents to encode".into()))?;
    ctx.set_history_agents_changed(true);
    let mut rows = ctx.reborrow().init_history_agents(count);
    for (index, agent) in (0..count).zip(agents) {
        let mut row = rows.reborrow().get(index);
        row.set_device(&agent.device);
        row.set_person(&agent.person);
        row.set_person_name(&agent.person_name);
        row.set_agent_id(&agent.agent_id);
        row.set_label(&agent.label);
        let [red, green, blue] = agent.color;
        row.set_color(u32::from_be_bytes([0, red, green, blue]));
    }
    Ok(())
}

// Listener wiring

/// Bind `socket_path` and spawn the capnp accept loop. Attaches the
/// created [`CapnpBrokerState`] to `fanout` so every event flowing
/// through the fanout core lands on typed subscribers.
///
/// `publisher` is the other direction — attaching to the peer table lets
/// a capnp app *receive*, `publisher` lets it *send*, same instance
/// [`crate::collab_broker`] uses for peer replication.
///
/// The accept thread runs a `current_thread` tokio runtime + `LocalSet`
/// (capnp-rpc futures are `!Send`), polling `quit` on a 100 ms cadence.
///
/// # Errors
///
/// Returns an error if `socket_path` cannot be bound as a Unix listener
/// (e.g. a stale socket file blocks it, permissions are wrong, or the
/// parent directory does not exist) or if spawning the accept thread
/// fails.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the broker thread takes ownership of the exact shared handles it wires into its isolated LocalSet"
)]
pub fn spawn(
    socket_path: PathBuf,
    fanout: &Arc<FanoutState>,
    broker_channels: Arc<Mutex<BrokerChannels>>,
    publisher: Arc<PeerPublisher>,
    membership: Option<WorkspaceTreeHandle>,
    services: HostServices,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    spawn_configured(
        &socket_path,
        fanout,
        BrokerWiring {
            broker_channels,
            publisher,
            membership,
            trusted_devices: None,
            services,
            config: None,
        },
        quit,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &Path) -> std::io::Result<SocketIdentity> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket path is not a Unix socket",
        ));
    }
    Ok(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn socket_lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    PathBuf::from(lock)
}

fn acquire_socket_path_lease(path: &Path) -> std::io::Result<advisory_lock::FileLock> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(socket_lock_path(path))?;
    if !lock.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket lease is not a regular file",
        ));
    }
    advisory_lock::FileLock::try_exclusive(lock).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket path is owned by another broker",
        )
    })
}

fn remove_socket_if_owned(path: &Path, identity: SocketIdentity) -> std::io::Result<bool> {
    let current = match socket_identity(path) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if current != identity {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

fn private_socket_parent(path: &Path) -> std::io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker socket requires a parent directory",
        )
    })?;
    let metadata = std::fs::symlink_metadata(parent)?;
    let effective_uid = rustix::process::geteuid().as_raw();
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "broker socket parent must be an owner-only directory",
        ));
    }
    Ok(parent)
}

fn socket_stage_path(path: &Path) -> std::io::Result<PathBuf> {
    let parent = private_socket_parent(path)?;
    let leaf_len = path
        .file_name()
        .map(|leaf| leaf.as_bytes().len())
        .filter(|len| *len > 0)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "broker socket requires a filename",
            )
        })?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let mut leaf = String::with_capacity(leaf_len);
    if leaf_len > 1 {
        leaf.push('.');
    }
    while leaf.len() < leaf_len {
        let remaining = leaf_len.saturating_sub(leaf.len());
        // The token is ASCII hex, so each char is one byte.
        leaf.extend(token.chars().take(remaining));
    }
    Ok(parent.join(leaf))
}

#[cfg(test)]
std::thread_local! {
    static FAIL_STAGED_SOCKET_IDENTITY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn staged_socket_identity(path: &Path) -> std::io::Result<SocketIdentity> {
    #[cfg(test)]
    if FAIL_STAGED_SOCKET_IDENTITY.with(|fail| fail.replace(false)) {
        return Err(std::io::Error::other(
            "injected staged socket identity failure",
        ));
    }
    socket_identity(path)
}

fn bind_socket_stage(
    path: &Path,
) -> std::io::Result<(std::os::unix::net::UnixListener, SocketStage)> {
    for _ in 0..8 {
        let socket = socket_stage_path(path)?;
        if socket == path {
            continue;
        }
        let mut stage = SocketStage {
            socket,
            identity: None,
            bound: false,
            published: false,
        };
        match std::os::unix::net::UnixListener::bind(&stage.socket) {
            Ok(listener) => {
                stage.bound = true;
                stage.identity = Some(staged_socket_identity(&stage.socket)?);
                return Ok((listener, stage));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AlreadyExists
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "broker socket staging name collision",
    ))
}

fn rename_socket_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker socket staging path contains NUL",
        )
    })?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker socket destination path contains NUL",
        )
    })?;
    #[cfg(target_os = "linux")]
    // SAFETY: both pointers refer to live NUL-terminated path buffers.
    // RENAME_NOREPLACE atomically refuses an occupied destination.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    // A filesystem without RENAME_NOREPLACE (gVisor's gofer mounts answer
    // EINVAL) claims the name with a hard link, which refuses an occupied
    // destination the same way, then drops the staging name.
    #[cfg(target_os = "linux")]
    let result = if result != 0
        && matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL | libc::ENOSYS)
        ) {
        // SAFETY: both pointers refer to live NUL-terminated path buffers.
        let linked = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                0,
            )
        };
        if linked == 0 {
            // SAFETY: `source` is a live NUL-terminated path buffer.
            unsafe { libc::unlink(source.as_ptr()) };
            0
        } else {
            -1
        }
    } else {
        result
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both pointers refer to live NUL-terminated path buffers.
    // RENAME_EXCL atomically refuses an occupied destination.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result = -1;
    if result == 0 {
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace broker socket publication is unsupported",
    ));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "broker socket destination appeared during publication",
            ));
        }
        Err(error)
    }
}

struct SocketStage {
    socket: PathBuf,
    identity: Option<SocketIdentity>,
    bound: bool,
    published: bool,
}

impl Drop for SocketStage {
    fn drop(&mut self) {
        if !self.published
            && let Some(identity) = self.identity
        {
            let _ = remove_socket_if_owned(&self.socket, identity);
        } else if !self.published
            && self.bound
            && std::fs::symlink_metadata(&self.socket)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
        {
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

/// Remove only a proven stale socket, then bind and retain inode custody.
/// A live socket, symlink, non-socket, or replacement inode is never unlinked.
fn bind_owned_socket(
    path: &Path,
) -> std::io::Result<(std::os::unix::net::UnixListener, SocketCustody)> {
    private_socket_parent(path)?;
    let lease = acquire_socket_path_lease(path)?;
    match socket_identity(path) {
        Ok(stale_candidate) => match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "broker socket is already accepting connections",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                if !remove_socket_if_owned(path, stale_candidate)? {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "broker socket changed during stale-socket recovery",
                    ));
                }
            }
            Err(error) => return Err(error),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let (listener, mut stage) = bind_socket_stage(path)?;
    let identity = stage.identity.ok_or_else(|| {
        std::io::Error::other("bound broker socket stage has no recorded identity")
    })?;
    rename_socket_noreplace(&stage.socket, path)?;
    stage.published = true;
    if socket_identity(path)? != identity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket pathname changed during publication",
        ));
    }
    drop(stage);
    Ok((
        listener,
        SocketCustody {
            path: path.to_path_buf(),
            identity,
            _lease: lease,
        },
    ))
}

struct SocketCustody {
    path: PathBuf,
    identity: SocketIdentity,
    _lease: advisory_lock::FileLock,
}

impl Drop for SocketCustody {
    fn drop(&mut self) {
        let _ = remove_socket_if_owned(&self.path, self.identity);
    }
}

struct BrokerThreadShutdown {
    state: Arc<CapnpBrokerState>,
    fanout: Arc<FanoutState>,
}

/// Every host-owned handle the broker state is built from: durable channel
/// state, the peer publisher, workspace membership, the trusted device keys,
/// host services and the native peer configuration.
pub(super) struct BrokerWiring<'a> {
    pub(super) broker_channels: Arc<Mutex<BrokerChannels>>,
    pub(super) publisher: Arc<PeerPublisher>,
    pub(super) membership: Option<WorkspaceTreeHandle>,
    pub(super) trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    pub(super) services: HostServices,
    pub(super) config: Option<&'a super::peer_actions::NativePeerBrokerConfig>,
}

impl Drop for BrokerThreadShutdown {
    fn drop(&mut self) {
        self.fanout.shutdown_capnp(&self.state);
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the state handles are clones moved into the broker thread"
)]
pub(super) fn spawn_configured(
    socket_path: &Path,
    fanout: &Arc<FanoutState>,
    wiring: BrokerWiring<'_>,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let BrokerWiring {
        broker_channels,
        publisher,
        membership,
        trusted_devices,
        services,
        config,
    } = wiring;
    let wired = publisher.is_wired();
    let (state, bridge_rx) = CapnpBrokerState::new_configured(
        fanout,
        broker_channels,
        publisher,
        membership,
        trusted_devices,
        config,
        services,
    );
    if let Some(path) = state.services.launchable_catalogue.clone() {
        state.launchable.persist_at(&path);
    }
    {
        let journal = state
            .journal
            .lock()
            .map_err(|_| std::io::Error::other("invocation journal unavailable"))?;
        journal
            .as_ref()
            .map_err(|error| std::io::Error::other(error.clone()))?;
    }
    // Bind on the caller's thread: from the worker thread, a bind
    // failure surfaces only via a log line.
    let (std_listener, socket_custody) = bind_owned_socket(socket_path)?;
    std_listener.set_nonblocking(true)?; // tokio's reactor requires this for `from_std`.
    tracing::info!(
        peer_sync = wired,
        "capnp broker: bound {} for native SDK access ({})",
        socket_path.display(),
        if wired {
            "edits from apps on this socket will reach the room"
        } else {
            "LOCAL ONLY — no peer network, so edits from apps on this \
             socket stay on this machine"
        },
    );

    let attached_state = Arc::clone(&state);
    let fanout_for_thread = Arc::clone(fanout);
    let socket_path_for_thread = socket_path.to_path_buf();
    let thread = std::thread::Builder::new()
        .name("tilo-collab-broker-capnp".into())
        .spawn(move || {
            // Declaration order is intentional: shutdown first makes every
            // surviving facade inert and releases the journal, then socket
            // custody removes only this broker's still-owned inode.
            let _socket_custody = socket_custody;
            let _shutdown = BrokerThreadShutdown {
                state: Arc::clone(&state),
                fanout: fanout_for_thread,
            };
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
            loop {
                let listener = match std_listener.try_clone() {
                    Ok(listener) => listener,
                    Err(e) => {
                        tracing::warn!("capnp broker: cannot take up its socket again: {e}");
                        return;
                    }
                };
                let run = {
                    let state = Arc::clone(&state);
                    let directories = Rc::clone(&directories);
                    let quit = Arc::clone(&quit);
                    let socket_path = socket_path_for_thread.clone();
                    tilo_rt::panic_boundary::isolate(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                tracing::warn!("capnp broker: tokio runtime init failed: {e}");
                                return;
                            }
                        };
                        tokio::task::LocalSet::new().block_on(
                            &rt,
                            serve_broker_thread(state, directories, listener, socket_path, quit),
                        );
                    })
                };
                #[cfg(any(test, feature = "test-support"))]
                report_retired_broker_owners(&state, &directories);
                match run {
                    Ok(()) => return,
                    Err(panic) => {
                        if !state.restart_after_unwind(panic.as_ref()) {
                            return;
                        }
                    }
                }
            }
        })?;
    fanout.attach_capnp(attached_state);

    Ok(thread)
}

#[cfg(any(test, feature = "test-support"))]
fn report_retired_broker_owners(
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
) {
    if std::env::var_os("TILO_IN_PROCESS_TEST_ROOT").is_some() {
        eprintln!(
            "native broker retired LocalSet: state refs={}, directory refs={}, registered={:?}",
            Arc::strong_count(state),
            Rc::strong_count(directories),
            directories
                .directories
                .try_borrow()
                .map(|entries| entries.len()),
        );
    }
}

/// Most broker threads a session starts after one unwound, before the
/// broker stays down for the rest of the session.
const BROKER_THREAD_RESTARTS: u64 = 5;

impl CapnpBrokerState {
    /// After the broker thread's runtime unwound: withdraw the services it
    /// ran and say whether to start it again. Its connections closed with
    /// the runtime; apps reconnect to the same socket.
    fn restart_after_unwind(&self, panic: &(dyn std::any::Any + Send)) -> bool {
        let message = tilo_rt::panic_boundary::message(panic);
        let restarts = self
            .thread_restarts
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.forget_service_peers();
        if restarts > BROKER_THREAD_RESTARTS || self.stopped.load(Ordering::Acquire) {
            tracing::error!(
                restarts,
                panic = message,
                "capnp broker: stopped after its thread failed again; apps lose collaboration until the session restarts"
            );
            return false;
        }
        tracing::error!(
            restarts,
            panic = message,
            "capnp broker: its thread failed; starting it again"
        );
        // A thread that fails at once again backs off rather than spin;
        // tests restart at once.
        if !cfg!(test) {
            let _ = tilo_rt::sleep_thread(Duration::from_millis(
                100_u64.saturating_mul(1 << restarts.min(5)),
            ));
        }
        true
    }

    /// How often the broker thread failed and was started again.
    #[must_use]
    pub fn thread_restarts(&self) -> u64 {
        self.thread_restarts.load(Ordering::Relaxed)
    }
}

/// When the broker's turn must wake with nothing arriving: the earlier of
/// its next policy re-check and its next invocation expiry, and never while
/// neither is due.
fn broker_turn_deadline(
    recheck: Option<std::time::Instant>,
    invocation: Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    recheck.into_iter().chain(invocation).min()
}

/// Sleep until `deadline`; only polled while there is one.
async fn sleep_until(deadline: Option<std::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    }
}

/// One run of the broker thread: start the broker's own services, then
/// accept and drive connections until asked to quit.
#[expect(
    clippy::too_many_lines,
    reason = "the broker's turn: service start, worker admission, accept and expiry in one loop"
)]
#[expect(
    clippy::future_not_send,
    reason = "Cap'n Proto RPC and its action-directory registry are Rc-backed and run only on the broker LocalSet"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the state handles are clones moved into the connection tasks"
)]
async fn serve_broker_thread(
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    std_listener: std::os::unix::net::UnixListener,
    socket_path_for_thread: PathBuf,
    quit: Arc<AtomicBool>,
) {
    state.start_peer_action_bridge(Rc::clone(&directories));
    let listener = match tokio::net::UnixListener::from_std(std_listener) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                "capnp broker: adopting {} into the reactor failed: {e}",
                socket_path_for_thread.display()
            );
            return;
        }
    };
    // Membership surfaces and native context subscribers are re-checked when
    // an input announces a change or a registered expiry comes due, not on a
    // clock. The watch wakes this turn for the first announcement
    // after it is armed; the schedule coalesces the rest.
    let policy_turn = Arc::new(tokio::sync::Notify::new());
    let policy_watch = {
        let turn = Arc::clone(&policy_turn);
        tilo_identity_host::policy_change::Watch::new(move || turn.notify_one())
    };
    let mut policy = crate::policy_schedule::PolicySchedule::default();
    // Runs are delivered, and their questions told, as the room's record
    // that holds them announces each publication: the epoch they were last
    // delivered at.
    let mut delivered = None;
    loop {
        let now = std::time::Instant::now();
        let epoch = tilo_identity_host::policy_change::epoch();
        if policy.due(now, epoch).is_some_and(|due| due <= now) {
            let clocked = state.refresh_policy_surfaces();
            policy.checked(now, epoch, clocked);
        }
        if state.has_native_task_subscribers() && delivered != Some(epoch) {
            state.refresh_native_task_delivery();
            delivered = Some(epoch);
        }
        #[cfg(test)]
        {
            let callbacks = match state.thread_callbacks.lock() {
                Ok(mut callbacks) => std::mem::take(&mut *callbacks),
                Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
            };
            for callback in callbacks {
                callback();
            }
        }
        let workers = state
            .workers
            .lock()
            .map(|mut workers| std::mem::take(&mut *workers))
            .unwrap_or_default();
        for worker in workers {
            let state = state.clone();
            let directories = directories.clone();
            let peer_id = state.next_peer_id();
            tokio::task::spawn_local(async move {
                let result = async {
                    let stream = tokio::net::UnixStream::from_std(worker.stream)
                        .map_err(capnp::Error::from)?;
                    drive_connection(stream, state, directories, peer_id, Some(worker.binding))
                        .await
                }
                .await;
                if let Err(error) = result {
                    let error_class = match error.kind {
                        capnp::ErrorKind::Failed => "failed",
                        capnp::ErrorKind::Overloaded => "overloaded",
                        capnp::ErrorKind::Disconnected => "disconnected",
                        capnp::ErrorKind::Unimplemented => "unimplemented",
                        _ => "protocol",
                    };
                    tracing::warn!(
                        peer_id,
                        phase = "local_worker_rpc",
                        error_class,
                        "capnp broker: local worker disconnected"
                    );
                }
            });
        }
        state.drain_action_directory_bridge(&directories);
        state.drain_offloads();
        if quit.load(Ordering::Relaxed) {
            break;
        }
        // Sleep until something here is due: the next policy re-check, or an
        // invocation about to expire. Everything else the turn handles wakes
        // it at once: queued directory work, in-process door calls and quit
        // through `local_turn`, admitted workers through `worker_admitted`,
        // policy announcements, and with them runs to deliver, through
        // `policy_turn`; connected apps run as
        // tasks of this LocalSet.
        let now = std::time::Instant::now();
        let mut recheck = policy.due(now, tilo_identity_host::policy_change::epoch());
        let armed = policy_watch.arm();
        if (recheck.is_none() && armed != policy.epoch())
            || (state.has_native_task_subscribers() && delivered != Some(armed))
        {
            // Announced between the checks above and arming: nothing will
            // wake the turn for it.
            recheck = Some(now);
        }
        let deadline = broker_turn_deadline(recheck, state.next_invocation_deadline());
        let stream = tokio::select! {
            biased;
            () = sleep_until(deadline), if deadline.is_some() => {
                // Expiration must not depend on a caller polling
                // status: completion waiters deserve a terminal
                // answer even after an app goes silent.
                state.expire_stale_invocations();
                continue;
            },
            conn = listener.accept() => conn,
            () = state.local_turn.notified() => continue,
            () = state.worker_admitted.notified() => continue,
            () = policy_turn.notified() => continue,
        };
        let (stream, _) = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("capnp broker: accept failed: {e}; retrying");
                continue;
            }
        };
        let state = Arc::clone(&state);
        let directories = Rc::clone(&directories);
        let peer_id = state.next_peer_id();
        tokio::task::spawn_local(async move {
            if let Err(e) = drive_connection(stream, state, directories, peer_id, None).await {
                tracing::warn!("capnp broker: peer-{peer_id} disconnected: {e}");
            }
        });
    }
}

/// Dial the capnp socket at `path` for tests. Production apps go through
/// `runtime_collab::try_connect` (launcher-passed fd). Public so
/// out-of-tree integration tests can reach it too.
///
/// # Errors
///
/// Returns an error if the socket at `path` does not appear and accept a
/// connection within the shared test hang bound — the accept loop in
/// [`spawn`] may not have bound yet, or the broker never started.
#[cfg(any(test, feature = "test-support"))]
pub async fn dial_capnp_for_test(
    path: PathBuf,
) -> Result<
    (
        runtime_collab::raw::collab_service::Client,
        tokio::task::JoinHandle<()>,
    ),
    std::io::Error,
> {
    // Retry until the accept loop has bound; the bound only catches a broker
    // that never starts, since a loaded machine can delay the bind for seconds.
    let stream = {
        let mut last_err: Option<std::io::Error> = None;
        let mut stream = None;
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        match stream {
            Some(s) => s,
            None => {
                return Err(last_err.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "capnp socket did not appear within the hang bound",
                    )
                }));
            }
        }
    };
    let (r, w) = stream.into_split();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(r.compat()),
        futures::io::BufWriter::new(w.compat_write()),
        rpc_twoparty_capnp::Side::Client,
        capnp::message::ReaderOptions::default(),
    ));
    let mut rpc = RpcSystem::new(network, None);
    let client: runtime_collab::raw::collab_service::Client =
        rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
    let driver = tokio::task::spawn_local(async move {
        let _ = rpc.await;
    });
    Ok((client, driver))
}

// Every `*::Server` impl in this file takes `self: Rc<Self>` (capnp-rpc's
// dispatch convention), so the client/capability chain built here and the
// `RpcSystem` future it drives are inherently `!Send`. That is fine: this
// future is only ever polled via `tokio::task::spawn_local` inside the
// single-threaded `LocalSet` set up in `spawn`, never via `tokio::spawn`,
// so nothing here needs to cross threads.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc's Rc-based server dispatch (self: Rc<Self> throughout this file) makes this future !Send by construction; it is only ever driven via spawn_local on the single-threaded LocalSet in `spawn`, never tokio::spawn"
)]
/// Peek at an incoming descriptor without consuming Cap'n Proto's segment
/// header. Raw endpoint diagnostics intentionally omit the launcher marker;
/// only a marker-bearing connection is subject to shell-process attestation.
async fn has_collab_launch_marker(stream: &tokio::net::UnixStream) -> Result<bool, capnp::Error> {
    let incomplete =
        || capnp::Error::failed("collaboration peer sent an incomplete protocol header".into());
    // A peer that has not begun is waited for as before; one that began has
    // `LAUNCH_MARKER_COMPLETION` to send the rest.
    let mut complete_by = None;
    loop {
        let ready = stream.ready(tokio::io::Interest::READABLE);
        let ready = match complete_by {
            None => ready.await,
            Some(deadline) => tokio::time::timeout_at(deadline, ready)
                .await
                .map_err(|_| incomplete())?,
        }
        .map_err(|error| {
            capnp::Error::failed(format!("collaboration socket unreadable: {error}"))
        })?;
        let mut partial = false;
        // A peek that finds only part of the marker reports `WouldBlock`, so
        // the socket's readiness clears and the next bytes wake this loop.
        match stream.try_io(tokio::io::Interest::READABLE, || {
            peek_launch_marker(stream, &mut partial)
        }) {
            Ok(marker) => return Ok(marker == launcher::services::COLLAB_LAUNCH_MAGIC),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if partial {
                    if ready.is_read_closed() {
                        return Err(capnp::Error::disconnected(
                            "collaboration peer closed mid protocol header".into(),
                        ));
                    }
                    complete_by.get_or_insert_with(|| {
                        tokio::time::Instant::now()
                            .checked_add(LAUNCH_MARKER_COMPLETION)
                            .unwrap_or_else(tokio::time::Instant::now)
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(capnp::Error::disconnected(
                    "collaboration peer closed before its protocol header".into(),
                ));
            }
            Err(error) => {
                return Err(capnp::Error::failed(format!(
                    "could not inspect collaboration protocol header: {error}"
                )));
            }
        }
    }
}

/// Peek at the launch marker without consuming it: the whole marker, or
/// `WouldBlock` — with `partial` set when some of it has arrived — or
/// `UnexpectedEof` when the peer closed without sending anything.
fn peek_launch_marker(
    stream: &tokio::net::UnixStream,
    partial: &mut bool,
) -> std::io::Result<[u8; launcher::services::COLLAB_LAUNCH_MAGIC.len()]> {
    let mut marker = [0_u8; launcher::services::COLLAB_LAUNCH_MAGIC.len()];
    // SAFETY: `marker` is a writable fixed-size buffer and `stream` owns
    // the live socket for this entire call. MSG_PEEK leaves the Cap'n
    // Proto header untouched for legacy/raw clients.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            marker.as_mut_ptr().cast(),
            marker.len(),
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    let Ok(read) = usize::try_from(read) else {
        return Err(std::io::Error::last_os_error());
    };
    if read == 0 {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    if read < marker.len() {
        *partial = true;
        return Err(std::io::ErrorKind::WouldBlock.into());
    }
    Ok(marker)
}

/// A launcher's handoff, read whole: the stream is now where the app's
/// first Cap'n Proto message begins.
struct LauncherHandoff {
    handoff: CollabLaunchHandoff,
    pid: u32,
    media_process: Option<native_media::PeerProcess>,
}

/// Why a connection was refused before its first Cap'n Proto call.
struct LaunchRefusal {
    error: capnp::Error,
    /// What the connection said it was, as far as it got: the process and
    /// the app and room its launcher named.
    peer: String,
    /// The handoff was read whole, so the app can still be told the reason
    /// over Cap'n Proto instead of seeing its connection reset.
    answerable: bool,
}

/// What authenticating a launcher's handoff established about a connection.
#[derive(Default)]
struct AuthenticatedHandoff {
    /// The exact process, where the kernel pins it.
    local_media: Option<Arc<native_media::LocalAppConnection>>,
    /// The launch that started it, where its host keeps a ticket for it.
    launch: Option<workspace_open::ConnectedLaunch>,
}

async fn authenticate_launcher_handoff(
    stream: &mut tokio::net::UnixStream,
    services: &HostServices,
) -> Result<AuthenticatedHandoff, LaunchRefusal> {
    let LauncherHandoff {
        handoff,
        pid,
        media_process,
    } = match read_launcher_handoff(stream).await {
        Ok(Some(read)) => read,
        Ok(None) => return Ok(AuthenticatedHandoff::default()),
        Err(error) => {
            let peer = stream.peer_cred().ok().and_then(|c| c.pid()).map_or_else(
                || "a peer without credentials".to_owned(),
                |pid| format!("pid {pid}"),
            );
            return Err(LaunchRefusal {
                error,
                peer,
                answerable: false,
            });
        }
    };
    let peer = format!(
        "pid {pid} launching {} in room {}",
        handoff.app_id, handoff.workspace_id
    );
    if handoff.protocol != launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION
        || handoff.app_id.trim().is_empty()
        || handoff.workspace_id.trim().is_empty()
    {
        return Err(LaunchRefusal {
            error: capnp::Error::failed(
                "collaboration launch handoff has an unsupported or empty identity".into(),
            ),
            peer,
            answerable: true,
        });
    }

    // Every host records a launch before it releases the launcher's
    // registration gate (`launcher::registration_gate`), so the record exists
    // before the launcher can connect, and one look decides. A direct
    // tilo-launcher command never obtains a record and is refused at once
    // instead of becoming a way around admin/admission checks.
    let launch = services
        .launcher
        .as_ref()
        .map(|service| service.authenticated_launch(pid, &handoff.app_id, &handoff.workspace_id))
        .transpose()
        .map_err(|error| LaunchRefusal {
            error: capnp::Error::failed(error),
            peer: peer.clone(),
            answerable: !handoff.executing_peer,
        })?
        .flatten();
    if let Some(launch) = launch {
        #[cfg(target_os = "linux")]
        let media_process = if handoff.executing_peer {
            Some(
                process_binding::bind(stream, media_process, services, pid, &handoff)
                    .await
                    .map_err(|error| LaunchRefusal {
                        error,
                        peer: peer.clone(),
                        answerable: false,
                    })?,
            )
        } else {
            media_process
        };
        #[cfg(not(target_os = "linux"))]
        if handoff.executing_peer {
            return Err(LaunchRefusal {
                error: capnp::Error::failed("executing custody requires Linux".into()),
                peer,
                answerable: false,
            });
        }
        let opened = launch
            .launch_ticket
            .map(|ticket| workspace_open::ConnectedLaunch {
                ticket,
                app_id: handoff.app_id.clone(),
                room: handoff.workspace_id.clone(),
            });
        return Ok(AuthenticatedHandoff {
            local_media: native_media::LocalAppConnection::from_launch(
                handoff,
                media_process,
                launch,
            ),
            launch: opened,
        });
    }
    let error = if services.launcher.is_none() {
        "collaboration launch cannot be attested: this broker has no launcher service"
    } else {
        "collaboration launch is not an authenticated shell-managed process: the \
         shell holds no live launch record for this process, app and room (an app \
         started by hand through tilo-launcher, or one whose record expired)"
    };
    Err(LaunchRefusal {
        error: capnp::Error::failed(error.into()),
        peer,
        answerable: !handoff.executing_peer,
    })
}

/// Read the launcher's marker and handoff off `stream`; `None` for a raw
/// client that sent no marker.
async fn read_launcher_handoff(
    stream: &mut tokio::net::UnixStream,
) -> Result<Option<LauncherHandoff>, capnp::Error> {
    if !has_collab_launch_marker(stream).await? {
        return Ok(None);
    }
    let credentials = stream.peer_cred().map_err(|error| {
        capnp::Error::failed(format!(
            "could not read collaboration peer credentials: {error}"
        ))
    })?;
    // A socket pathname is not authority. The kernel must identify a process
    // from this user before it can attest the compositor's launch record.
    if credentials.uid() != rustix::process::geteuid().as_raw() {
        return Err(capnp::Error::failed(
            "collaboration service rejected a peer from another user".into(),
        ));
    }
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| capnp::Error::failed("collaboration peer has no usable pid".into()))?;
    // Retain this exact connected task before consulting the expiring launch
    // record. Unsupported kernels keep ordinary collaboration available.
    let media_process = native_media::PeerProcess::from_socket(stream);

    let mut marker = [0_u8; launcher::services::COLLAB_LAUNCH_MAGIC.len()];
    stream.read_exact(&mut marker).await.map_err(|error| {
        capnp::Error::failed(format!(
            "could not read collaboration launch marker: {error}"
        ))
    })?;
    if marker != launcher::services::COLLAB_LAUNCH_MAGIC {
        return Err(capnp::Error::failed(
            "collaboration launch marker changed while being read".into(),
        ));
    }
    let bytes = match launcher::services::COLLAB_LAUNCH_FRAMING
        .read_async(stream)
        .await
    {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Err(capnp::Error::failed(
                "collaboration launch handoff is missing".into(),
            ));
        }
        Err(wire_frame::FrameError::EmptyFrame | wire_frame::FrameError::TooLarge(_)) => {
            return Err(capnp::Error::failed(
                "collaboration launch handoff has an invalid length".into(),
            ));
        }
        Err(error) => {
            return Err(capnp::Error::failed(format!(
                "could not read collaboration launch handoff: {error}"
            )));
        }
    };
    let handoff: CollabLaunchHandoff = serde_json::from_slice(&bytes).map_err(|error| {
        capnp::Error::failed(format!("invalid collaboration launch handoff: {error}"))
    })?;
    Ok(Some(LauncherHandoff {
        handoff,
        pid,
        media_process,
    }))
}

/// Tell a refused app why over the protocol it speaks: every call on the
/// service it bootstraps fails with `error`. Dropping the socket instead
/// leaves its first message unread, and the app only sees its connection
/// reset. Bounded, so a peer that never hangs up holds nothing for long.
#[allow(
    clippy::future_not_send,
    reason = "Cap'n Proto RPC is Rc-backed and runs only on the broker LocalSet"
)]
async fn answer_refused_launch(stream: tokio::net::UnixStream, error: capnp::Error) {
    let refused: collab_service::Client =
        capnp_rpc::new_future_client(std::future::ready(Err(error)));
    let rpc = runtime_portal::serve_connected_io(stream, refused.client);
    let _ = tokio::time::timeout(REFUSED_LAUNCH_ANSWER_WINDOW, rpc).await;
}

/// How long a refused app's connection is kept open to hear why.
const REFUSED_LAUNCH_ANSWER_WINDOW: Duration = Duration::from_secs(5);

#[allow(
    clippy::future_not_send,
    reason = "Cap'n Proto RPC and its action-directory registry are Rc-backed and intentionally run only on the broker LocalSet"
)]
async fn drive_connection(
    stream: tokio::net::UnixStream,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
    worker: Option<WorkerBinding>,
) -> Result<(), capnp::Error> {
    let _exit = ConnectionExit {
        state: Arc::clone(&state),
        directories: Rc::clone(&directories),
        peer_id,
    };
    let mut stream = stream;
    let local_media = if worker.is_none() {
        match authenticate_launcher_handoff(&mut stream, &state.services).await {
            Ok(AuthenticatedHandoff {
                local_media,
                launch,
            }) => {
                if let Some(launch) = launch {
                    state.launch_connected(peer_id, &launch);
                }
                if let Some(instance) = local_media.as_ref().and_then(|l| l.continuation_instance)
                    && let Ok(mut windows) = state.window_instances.lock()
                {
                    windows.insert(peer_id, instance);
                }
                local_media
            }
            Err(refusal) => {
                // Error, not warning: an app the shell launched is about to
                // exit for this, and the default session log keeps errors only.
                tracing::error!(
                    peer_id,
                    peer = %refusal.peer,
                    reason = %refusal.error,
                    "capnp broker: refused a collaboration connection before its first call",
                );
                if refusal.answerable {
                    answer_refused_launch(stream, refusal.error).await;
                }
                return Ok(());
            }
        }
    } else {
        None
    };
    let _media_monitor = native_media::ProcessMonitor::start(local_media.clone(), &state, peer_id);
    let handed_descriptors = local_media
        .as_ref()
        .and_then(|local| local.descriptors.clone());
    let app_worker = local_media
        .as_ref()
        .map(|app| (uuid::Uuid::new_v4(), app.app_id.clone()));
    let client: collab_service::Client = capnp_rpc::new_client(CapnpCollabService {
        worker,
        app_worker,
        lease: Rc::new(ConnectionLease {
            local_media,
            state: Arc::clone(&state),
            directories: Rc::clone(&directories),
            peer_id,
        }),
        state,
        directories,
        peer_id,
        holds: route_holds::ConnectionHolds::default(),
    });
    // Files hands over the objects it selects on its own connection; the
    // broker keeps them for the request naming them. Every other peer's
    // descriptors are closed on receipt.
    let rpc = match handed_descriptors {
        Some(received) => runtime_portal::serve_connected_io(
            runtime_portal::descriptor_lane::DescriptorStream::receiving(stream, received),
            client.client,
        ),
        None => runtime_portal::serve_connected_io(stream, client.client),
    };
    let _disconnect = DisconnectOnDrop(Some(rpc.get_disconnector()));
    rpc.await
}

#[cfg(any(test, feature = "test-support"))]
pub mod tests {
    // Test helpers throughout this module pair `_sk`/`_pk` (secret/public
    // key) and `owner`/`editor` role names — the clearest naming for this
    // domain, and renaming would obscure the keypair relationship rather
    // than clarify it.
    #![allow(
        clippy::similar_names,
        reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
    )]
    // Only test builds and `test-support` fixture builds compile this module;
    // clippy cannot see `cfg(any(test, ..))` as a test context by itself.
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable,
        clippy::panic_in_result_fn,
        clippy::arithmetic_side_effects,
        clippy::missing_errors_doc,
        clippy::missing_panics_doc,
        clippy::wildcard_imports,
        clippy::used_underscore_binding,
        clippy::needless_pass_by_value,
        clippy::too_many_lines,
        clippy::items_after_statements,
        clippy::significant_drop_tightening,
        clippy::future_not_send,
        clippy::large_futures,
        clippy::await_holding_lock,
        clippy::await_holding_refcell_ref,
        reason = "test scenarios and fixtures: a failed unwrap is the assertion, and each scenario keeps its setup, guards and awaits in one readable body"
    )]
    use super::*;
    include!("certified_person_fixture.rs");
    #[cfg(test)]
    include!("eager_bridge_tests.rs");
    #[cfg(test)]
    use crate::test_support::WorkspaceTree;
    use crate::test_support::WorkspaceTreeHandle;
    use std::cell::RefCell;
    #[cfg(test)]
    use std::os::unix::fs::PermissionsExt;
    use std::rc::Rc;
    #[cfg(test)]
    use std::time::SystemTime;

    #[cfg(test)]
    #[cfg(test)]
    use crate::collab_outbound::recording::{RecordingSink, Sent};
    #[cfg(test)]
    use crate::test_support::Grants as _;
    #[cfg(test)]
    use collab_types::{Ed25519PublicKey, Role, sign_op};
    #[cfg(test)]
    use ed25519_dalek::Signer as _;
    #[cfg(test)]
    use ed25519_dalek::SigningKey;

    /// Prepare an owned test directory for the production broker's socket check.
    ///
    /// # Errors
    /// Returns the filesystem error if the directory's mode cannot be set to 0700.
    pub fn secure_socket_parent(parent: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
    }

    #[cfg(test)]
    fn lifecycle_socket(directory: &tempfile::TempDir, name: &str) -> PathBuf {
        secure_socket_parent(directory.path()).expect("secure temporary broker directory");
        directory.path().join(format!("{name}.sock"))
    }

    #[test]
    fn socket_parent_must_be_owner_only_before_any_artifact_is_created() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
            .expect("make parent deliberately non-private");
        let path = directory.path().join("rejected.sock");

        let error = bind_owned_socket(&path)
            .err()
            .expect("non-private parent must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            std::fs::read_dir(directory.path())
                .expect("inspect rejected parent")
                .next()
                .is_none(),
            "parent rejection must precede socket, lease, or staging artifacts"
        );
    }

    #[test]
    fn socket_custody_reclaims_only_stale_owned_inodes() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let live_path = lifecycle_socket(&directory, "live");
        let live = std::os::unix::net::UnixListener::bind(&live_path).expect("live socket");
        let error = bind_owned_socket(&live_path)
            .err()
            .expect("live broker socket must not be reclaimed");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert!(socket_identity(&live_path).is_ok());
        drop(live);

        let stale_path = lifecycle_socket(&directory, "stale");
        let stale = std::os::unix::net::UnixListener::bind(&stale_path).expect("stale socket");
        drop(stale);
        let (replacement, custody) =
            bind_owned_socket(&stale_path).expect("proven stale socket is reclaimed");
        assert!(std::os::unix::net::UnixStream::connect(&stale_path).is_ok());
        let contender_error = bind_owned_socket(&stale_path)
            .err()
            .expect("lifetime lease excludes a competing socket owner");
        assert_eq!(contender_error.kind(), std::io::ErrorKind::AddrInUse);
        drop(replacement);
        drop(custody);
        assert!(
            !stale_path.exists(),
            "owned socket inode is removed on final custody release"
        );

        let replaced_path = lifecycle_socket(&directory, "replacement");
        let original =
            std::os::unix::net::UnixListener::bind(&replaced_path).expect("original socket");
        let original_identity = socket_identity(&replaced_path).expect("original identity");
        // Retain the old inode while replacing its pathname: unlinking its last
        // name would let the filesystem reuse that inode for the new socket.
        let displaced_path = lifecycle_socket(&directory, "displaced");
        std::fs::rename(&replaced_path, &displaced_path).expect("retain original socket inode");
        assert_eq!(
            socket_identity(&displaced_path).expect("retained original identity"),
            original_identity
        );
        let replacement =
            std::os::unix::net::UnixListener::bind(&replaced_path).expect("replacement socket");
        assert_ne!(
            socket_identity(&replaced_path).expect("replacement identity"),
            original_identity,
            "the replacement fixture must exercise a distinct live inode"
        );
        assert!(
            !remove_socket_if_owned(&replaced_path, original_identity)
                .expect("replacement identity check"),
            "an old owner must not unlink a replacement socket"
        );
        assert!(std::os::unix::net::UnixStream::connect(&replaced_path).is_ok());
        drop(replacement);
        drop(original);
        std::fs::remove_file(&displaced_path).expect("remove retained original pathname");

        let racing_path = lifecycle_socket(&directory, "publish-race");
        let (staged, stage) = bind_socket_stage(&racing_path).expect("private socket staging");
        let racing =
            std::os::unix::net::UnixListener::bind(&racing_path).expect("racing destination");
        let racing_identity = socket_identity(&racing_path).expect("racing socket identity");
        let error = rename_socket_noreplace(&stage.socket, &racing_path)
            .expect_err("no-replace publication must preserve a racing destination");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert_eq!(
            socket_identity(&racing_path).expect("racing socket survives"),
            racing_identity
        );
        assert!(std::os::unix::net::UnixStream::connect(&racing_path).is_ok());
        drop(staged);
        drop(stage);
        assert_eq!(
            socket_identity(&racing_path).expect("stage cleanup preserves destination"),
            racing_identity
        );
        drop(racing);

        let entries_before_failure = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        let identity_failure_path = lifecycle_socket(&directory, "identity-failure");
        FAIL_STAGED_SOCKET_IDENTITY.with(|fail| fail.set(true));
        assert!(bind_socket_stage(&identity_failure_path).is_err());
        let entries_after_failure = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            entries_after_failure, entries_before_failure,
            "staged metadata failure leaves no socket artifact"
        );

        #[cfg(target_os = "macos")]
        {
            let parent_len = directory.path().as_os_str().as_bytes().len();
            let separator = usize::from(!directory.path().as_os_str().is_empty());
            let leaf_len = 103usize
                .checked_sub(parent_len + separator)
                .expect("temporary path leaves room for a valid Unix socket");
            let long_path = directory.path().join("x".repeat(leaf_len));
            assert_eq!(long_path.as_os_str().as_bytes().len(), 103);
            let (listener, custody) = bind_owned_socket(&long_path)
                .expect("valid near-limit final path must stage without expansion");
            assert!(std::os::unix::net::UnixStream::connect(&long_path).is_ok());
            drop(listener);
            drop(custody);
        }
    }

    #[cfg(test)]
    struct LifecycleAuthority;

    #[cfg(test)]
    impl crate::peer_actions::CertificateAuthority for LifecycleAuthority {
        fn authorize(
            &self,
            _: uuid::Uuid,
            _: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[cfg(test)]
    struct LifecycleSink;

    #[cfg(test)]
    impl crate::collab_outbound::PeerPublishSink for LifecycleSink {
        fn app_document(&self, _: String, _: String, _: String, _: collab_types::SignedOp) {}

        fn app_presence(&self, _: String, _: String, _: collab_types::PeerPresence) {}

        fn scrollback_request(&self, _: &str, _: ArtifactId, _: u64, _: u16) {}
    }

    #[cfg(test)]
    struct HealthyOwner;

    #[cfg(test)]
    impl tilo_owner_commit::OwnerCommitService for HealthyOwner {
        fn commit(
            &self,
            _: tilo_owner_commit::OwnerCommitRequest,
        ) -> tilo_owner_commit::CommitFuture {
            Box::pin(async { Err("unused lifecycle commit".into()) })
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn a_join_names_a_valid_record_kind_or_none() {
        let joined = |kind: &str| super::joined_record_kind(kind.into());
        assert_eq!(joined("").unwrap(), None);
        assert_eq!(
            joined("notes.notebook").unwrap().as_deref(),
            Some("notes.notebook")
        );
        for invalid in ["1note", "note book", "note/page"] {
            assert!(joined(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn stopped_state_releases_journal_and_makes_surviving_facade_inert() {
        let directory = tempfile::tempdir().expect("temporary journal directory");
        let fanout = Arc::new(FanoutState::new());
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(HealthyOwner)),
            Some(WorkspaceTree::default().shared().authority()),
            None,
            Some(&config),
            HostServices::default(),
        );
        let facade = crate::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&state));
        assert!(
            facade.peer_action_attachment().is_ok(),
            "live facade sees the opened invocation journal"
        );
        let state_for_poison = Arc::clone(&state);
        assert!(
            std::thread::spawn(move || {
                let _journal = state_for_poison.journal.lock().expect("journal lock");
                panic!("poison journal lock for shutdown recovery");
            })
            .join()
            .is_err()
        );

        state.stop();

        assert!(state.is_stopped());
        assert!(facade.peer_action_attachment().is_err());
        assert!(facade.surfaces().is_empty());
        assert!(facade.statuses("owner").is_empty());
        let reopened = InvocationJournal::open(&config.journal_root)
            .expect("stopped broker releases the invocation-journal writer");
        drop(reopened);
    }

    #[test]
    fn admitted_live_operation_keeps_its_identity_when_shutdown_races_after_effect() {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let state_for_operation = Arc::clone(&state);
        let (effect_tx, effect_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let operation = std::thread::spawn(move || {
            state_for_operation.while_live(|_| {
                effect_tx.send(()).expect("effect admitted");
                release_rx.recv().expect("release admitted operation");
                Ok(71_u64)
            })
        });
        tilo_test_env::recv("operation reached its admitted effect", &effect_rx);
        state.request_stop();
        release_tx.send(()).expect("release operation");
        assert_eq!(
            operation.join().expect("operation thread"),
            Ok(71),
            "an operation admitted before shutdown keeps its non-retryable identity"
        );
        state.stop();
    }

    /// An app whose launch the shell holds no record of (one started by hand
    /// through tilo-launcher, or one whose record is gone) is refused. It used
    /// to be refused by dropping its socket with its first call unread, so the
    /// app saw only "Connection reset by peer" and the shell logged nothing it
    /// keeps. The app must hear why, as an ordinary failed call.
    #[test]
    fn an_unattested_launch_is_told_why_it_was_refused() {
        struct NoLaunchRecords;
        impl crate::services::LauncherServices for NoLaunchRecords {
            fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
                Vec::new()
            }
            fn launch(
                &self,
                _: &str,
                _: &tilo_desktop_model::Workspace,
                _: Option<&str>,
                _: &crate::services::LaunchPlace,
            ) -> Result<(), String> {
                Err("this broker launches nothing".into())
            }
            fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
                false
            }
        }
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = lifecycle_socket(&directory, "unattested");
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("unattested-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let _broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices {
                launcher: Some(Arc::new(NoLaunchRecords)),
                ..Default::default()
            },
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("test broker");

        // What tilo-launcher writes before handing the descriptor to the app.
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).expect("connect");
        let handoff = serde_json::to_vec(&serde_json::json!({
            "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
            "app_id": "so.tilo.calculator",
            "workspace_id": "5ba25f73-87c5-447f-a5a6-880dc4249a81",
        }))
        .expect("handoff json");
        std::io::Write::write_all(&mut stream, &launcher::services::COLLAB_LAUNCH_MAGIC)
            .expect("marker");
        std::io::Write::write_all(
            &mut stream,
            &launcher::services::COLLAB_LAUNCH_FRAMING
                .encode(&handoff)
                .expect("framed handoff"),
        )
        .expect("handoff");
        stream.set_nonblocking(true).expect("nonblocking");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let error = tokio::task::LocalSet::new().block_on(&runtime, async move {
            let stream = tokio::net::UnixStream::from_std(stream).expect("tokio stream");
            let client = runtime_collab::CollabClient::from_raw(
                runtime_portal::CapnpClient::from_connected_io(
                    stream,
                    rpc_twoparty_capnp::Side::Client,
                )
                .bootstrap(),
            );
            match client
                .join_channel(
                    "5ba25f73-87c5-447f-a5a6-880dc4249a81#app:so.tilo.calculator",
                    b"",
                )
                .await
            {
                Ok(_) => panic!("an unattested launch joined a channel"),
                Err(error) => error.to_string(),
            }
        });
        assert!(
            error.contains("not an authenticated shell-managed process"),
            "the refused app must learn why, not see its connection reset: {error}",
        );
        assert!(
            !error.contains("reset by peer"),
            "the refusal arrived as a reset: {error}",
        );
    }

    /// What tilo-launcher writes first on its collaboration connection: the
    /// marker, then the framed handoff naming the app and room.
    #[cfg(test)]
    fn launcher_handoff_bytes(app_id: &str, room: &str) -> (Vec<u8>, Vec<u8>) {
        let handoff = serde_json::to_vec(&serde_json::json!({
            "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
            "app_id": app_id,
            "workspace_id": room,
        }))
        .expect("handoff json");
        (
            launcher::services::COLLAB_LAUNCH_MAGIC.to_vec(),
            launcher::services::COLLAB_LAUNCH_FRAMING
                .encode(&handoff)
                .expect("framed handoff"),
        )
    }

    /// Records launches as a host does, before it releases the launcher.
    #[cfg(test)]
    struct RecordedLaunches(Mutex<Vec<(u32, String, String)>>);
    #[cfg(test)]
    impl crate::services::LauncherServices for RecordedLaunches {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("this broker launches nothing".into())
        }
        fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
            self.0
                .lock()
                .expect("records")
                .iter()
                .any(|(recorded, a, r)| *recorded == pid && a == app && r == room)
        }
        /// Every recorded launch was started by the `open` holding ticket 31.
        fn authenticated_launch(
            &self,
            pid: u32,
            app: &str,
            room: &str,
        ) -> Result<Option<crate::services::AuthenticatedApplicationLaunch>, String> {
            Ok(self.attest_process(pid, app, room).then_some(
                crate::services::AuthenticatedApplicationLaunch {
                    creation: None,
                    continuation_instance: None,
                    launch_ticket: Some(31),
                },
            ))
        }
    }

    /// Attestation is one look at the launch record: a launcher whose host
    /// recorded it (before releasing its registration gate) is attested at
    /// once, and a process with the marker but no record — someone running
    /// tilo-launcher by hand — is refused at once, without a retry window.
    /// The clock is paused, so any wait would show as elapsed time.
    #[tokio::test(start_paused = true)]
    async fn launcher_attestation_decides_on_one_look() {
        use tokio::io::AsyncWriteExt as _;
        const APP: &str = "so.tilo.calculator";
        const ROOM: &str = "5ba25f73-87c5-447f-a5a6-880dc4249a81";
        let launches = Arc::new(RecordedLaunches(Mutex::new(Vec::new())));
        let services = HostServices {
            launcher: Some(Arc::clone(&launches) as Arc<dyn crate::services::LauncherServices>),
            ..Default::default()
        };
        let (marker, handoff) = launcher_handoff_bytes(APP, ROOM);
        for recorded in [false, true] {
            let (mut broker_end, mut launcher_end) =
                tokio::net::UnixStream::pair().expect("socket pair");
            if recorded {
                launches.0.lock().expect("records").push((
                    std::process::id(),
                    APP.to_owned(),
                    ROOM.to_owned(),
                ));
            }
            launcher_end.write_all(&marker).await.expect("marker");
            launcher_end.write_all(&handoff).await.expect("handoff");
            let started = tokio::time::Instant::now();
            let attested = authenticate_launcher_handoff(&mut broker_end, &services).await;
            assert_eq!(
                started.elapsed(),
                Duration::ZERO,
                "attestation waited (recorded: {recorded})"
            );
            match attested {
                Ok(handoff) => {
                    assert!(recorded, "an unrecorded launcher was attested");
                    // The connection names the launch that started it, from
                    // the host's record, for the app and room it was
                    // admitted for.
                    let launch = handoff.launch.expect("the recorded launch's ticket");
                    assert_eq!(
                        (launch.ticket, launch.app_id.as_str(), launch.room.as_str()),
                        (31, APP, ROOM)
                    );
                }
                Err(refusal) => {
                    assert!(
                        !recorded,
                        "a recorded launcher was refused: {}",
                        refusal.error
                    );
                    assert!(
                        refusal
                            .error
                            .to_string()
                            .contains("not an authenticated shell-managed process"),
                        "{}",
                        refusal.error
                    );
                }
            }
        }
    }

    /// A launch marker that arrives in pieces is read once it is whole —
    /// the later bytes wake the read — and the stream is left untouched; one
    /// left unfinished is refused after its completion window, and a peer
    /// that hangs up mid-marker is refused for hanging up, rather than
    /// either holding the connection.
    #[tokio::test]
    async fn a_launch_marker_in_pieces_wakes_on_its_bytes() {
        use tokio::io::AsyncWriteExt as _;
        let marker = launcher::services::COLLAB_LAUNCH_MAGIC;
        let (head, tail) = marker.split_at(marker.len() / 2);

        let (broker_end, mut launcher_end) = tokio::net::UnixStream::pair().expect("socket pair");
        launcher_end.write_all(head).await.expect("first half");
        let reading = has_collab_launch_marker(&broker_end);
        tokio::pin!(reading);
        // Let the read see the first half and wait for the rest.
        for _ in 0..4 {
            assert!(futures::poll!(reading.as_mut()).is_pending());
            tokio::task::yield_now().await;
        }
        launcher_end.write_all(tail).await.expect("second half");
        assert!(reading.await.expect("a whole marker"));
        let mut kept = vec![0_u8; marker.len()];
        broker_end.readable().await.expect("readable");
        broker_end
            .try_read(&mut kept)
            .expect("the marker is still unread");
        assert_eq!(kept, marker, "peeking leaves the stream untouched");

        let (broker_end, mut launcher_end) = tokio::net::UnixStream::pair().expect("socket pair");
        launcher_end.write_all(head).await.expect("first half");
        let unfinished = has_collab_launch_marker(&broker_end).await;
        assert!(
            unfinished
                .expect_err("an unfinished marker is refused")
                .to_string()
                .contains("incomplete protocol header"),
            "an unfinished marker is refused at the end of its completion window"
        );
        drop(launcher_end);

        let (broker_end, mut launcher_end) = tokio::net::UnixStream::pair().expect("socket pair");
        launcher_end.write_all(head).await.expect("first half");
        drop(launcher_end);
        assert!(
            has_collab_launch_marker(&broker_end)
                .await
                .expect_err("a peer that hung up mid-marker is refused")
                .to_string()
                .contains("closed mid protocol header"),
            "the hang-up is an event, not the end of the completion window"
        );
    }

    #[test]
    fn public_sink_can_drop_its_broker_without_self_join_or_early_unlink() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = lifecycle_socket(&directory, "self-drop");
        let fanout = Arc::new(FanoutState::new());
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("self-drop-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let surviving_facade = broker.app_action_broker().expect("live broker facade");
        let state = fanout.capnp_state().expect("attached broker state");
        let holder = Arc::new(Mutex::new(Some(broker)));
        let holder_for_sink = Arc::clone(&holder);
        let socket_for_sink = socket.clone();
        let (sink_done, sink_finished) = std::sync::mpsc::sync_channel(1);
        let subscription = holder
            .lock()
            .expect("broker holder")
            .as_ref()
            .expect("broker")
            .subscribe_rooms(
                vec!["self-drop-room".into()],
                Box::new(move |_, _| {
                    let broker = holder_for_sink
                        .lock()
                        .expect("broker holder in sink")
                        .take()
                        .expect("broker dropped once");
                    drop(broker);
                    assert!(
                        std::os::unix::net::UnixListener::bind(&socket_for_sink).is_err(),
                        "self-drop keeps the owned pathname until this callback returns"
                    );
                    sink_done.send(()).expect("signal sink completion");
                }),
            );
        let fanout_for_callback = Arc::clone(&fanout);
        state.enqueue_thread_callback(Box::new(move || {
            fanout_for_callback.fanout_to_route(
                &CollabEvent::FollowChanged { peer_id: None },
                u64::MAX,
                &CrdtRoute::cluster("self-drop-room"),
            );
        }));
        tilo_test_env::recv(
            "broker-thread public sink completed without self-join",
            &sink_finished,
        );
        drop(subscription);

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(
            surviving_facade.peer_action_attachment().is_err(),
            "facade retained across thread exit is inert"
        );
        let reopened_journal = InvocationJournal::open(&config.journal_root)
            .expect("thread exit releases the journal before unlink");
        drop(reopened_journal);
        fanout.fanout_to_route(
            &CollabEvent::FollowChanged {
                peer_id: Some("stale".into()),
            },
            u64::MAX,
            &CrdtRoute::cluster("self-drop-room"),
        );
        let replacement = crate::collab_broker::spawn_with_test_services(
            socket,
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("broker restarts after self-drop thread exits");
        drop(replacement);
    }

    #[test]
    fn off_thread_public_sink_can_drop_broker_without_join_deadlock() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = lifecycle_socket(&directory, "off-thread-drop");
        let fanout = Arc::new(FanoutState::new());
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            None,
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let holder = Arc::new(Mutex::new(Some(broker)));
        let holder_for_sink = Arc::clone(&holder);
        let (sink_done, sink_finished) = std::sync::mpsc::sync_channel(1);
        let subscription = holder
            .lock()
            .expect("broker holder")
            .as_ref()
            .expect("broker")
            .subscribe_rooms(
                vec!["off-thread-room".into()],
                Box::new(move |_, _| {
                    let broker = holder_for_sink
                        .lock()
                        .expect("broker holder in sink")
                        .take()
                        .expect("broker dropped once");
                    drop(broker);
                    sink_done.send(()).expect("signal sink completion");
                }),
            );
        let fanout_for_thread = Arc::clone(&fanout);
        let caller = std::thread::spawn(move || {
            fanout_for_thread.fanout_to_route(
                &CollabEvent::FollowChanged { peer_id: None },
                u64::MAX,
                &CrdtRoute::cluster("off-thread-room"),
            );
        });
        tilo_test_env::recv(
            "off-thread sink drop joined broker without deadlock",
            &sink_finished,
        );
        caller.join().expect("off-thread fanout caller");
        drop(subscription);

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(!socket.exists());
    }

    #[cfg(test)]
    struct DropBrokerOnHealth {
        broker: Arc<Mutex<Option<crate::collab_broker::BrokerHandle>>>,
    }

    #[cfg(test)]
    impl tilo_owner_commit::OwnerCommitService for DropBrokerOnHealth {
        fn commit(
            &self,
            _: tilo_owner_commit::OwnerCommitRequest,
        ) -> tilo_owner_commit::CommitFuture {
            Box::pin(async { Err("unused lifecycle commit".into()) })
        }

        fn health(&self) -> Result<(), String> {
            drop(
                self.broker
                    .lock()
                    .map_err(|_| "broker holder unavailable")?
                    .take(),
            );
            Ok(())
        }
    }

    #[test]
    fn owner_health_callback_can_drop_broker_without_facade_join_deadlock() {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let socket = lifecycle_socket(&directory, "owner-health-drop");
        let journal_root = directory.path().join("owner-health-invocations");
        let holder = Arc::new(Mutex::new(None));
        let publisher = PeerPublisher::with_owner_commit(
            Arc::new(LifecycleSink),
            Arc::new(DropBrokerOnHealth {
                broker: Arc::clone(&holder),
            }),
        );
        let membership = WorkspaceTree::default().shared();
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: journal_root.clone(),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            Some(membership.authority()),
            publisher,
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("test broker");
        let facade = broker.app_action_broker().expect("live broker facade");
        *holder.lock().expect("broker holder") = Some(broker);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            result_tx
                .send(facade.peer_action_attachment())
                .expect("send facade result");
        });
        let result = tilo_test_env::recv(
            "owner health callback returned without join deadlock",
            &result_rx,
        );
        assert!(
            result.is_err(),
            "shutdown requested by the health callback fences its facade result"
        );

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(!socket.exists());
        let reopened = InvocationJournal::open(&journal_root)
            .expect("health callback shutdown releases journal");
        drop(reopened);
    }

    #[cfg(test)]
    struct DropOtherBrokerOnHealth {
        broker: Arc<Mutex<Option<crate::collab_broker::BrokerHandle>>>,
        socket: PathBuf,
        journal_root: PathBuf,
        proof: Arc<Mutex<Option<Result<(), String>>>>,
    }

    #[cfg(test)]
    impl tilo_owner_commit::OwnerCommitService for DropOtherBrokerOnHealth {
        fn commit(
            &self,
            _: tilo_owner_commit::OwnerCommitRequest,
        ) -> tilo_owner_commit::CommitFuture {
            Box::pin(async { Err("unused lifecycle commit".into()) })
        }

        fn health(&self) -> Result<(), String> {
            drop(
                self.broker
                    .lock()
                    .map_err(|_| "other broker holder unavailable")?
                    .take(),
            );
            let result = if self.socket.exists() {
                Err("unrelated broker drop returned before socket cleanup".into())
            } else {
                InvocationJournal::open(&self.journal_root)
                    .map(drop)
                    .map_err(|error| format!("unrelated broker journal remained locked: {error}"))
            };
            *self
                .proof
                .lock()
                .map_err(|_| "other broker proof unavailable")? = Some(result);
            Ok(())
        }
    }

    #[test]
    fn facade_guard_does_not_detach_an_unrelated_broker_drop() {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let other_socket = lifecycle_socket(&directory, "other");
        let other_journal = directory.path().join("other-invocations");
        let other_config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: other_journal.clone(),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let other = crate::collab_broker::spawn_with_test_services(
            other_socket.clone(),
            Some(WorkspaceTree::default().shared().authority()),
            PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(HealthyOwner)),
            Some(&other_config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("other broker");
        let other_holder = Arc::new(Mutex::new(Some(other)));
        let proof = Arc::new(Mutex::new(None));

        let broker_socket = lifecycle_socket(&directory, "guarded");
        let broker_config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("guarded-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::collab_broker::spawn_with_test_services(
            broker_socket,
            Some(WorkspaceTree::default().shared().authority()),
            PeerPublisher::with_owner_commit(
                Arc::new(LifecycleSink),
                Arc::new(DropOtherBrokerOnHealth {
                    broker: Arc::clone(&other_holder),
                    socket: other_socket,
                    journal_root: other_journal,
                    proof: Arc::clone(&proof),
                }),
            ),
            Some(&broker_config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("guarded broker");
        let facade = broker.app_action_broker().expect("guarded broker facade");

        facade
            .peer_action_attachment()
            .expect("unrelated broker drop does not fence this broker");
        proof
            .lock()
            .expect("other broker proof")
            .take()
            .expect("health callback recorded cleanup")
            .expect("unrelated broker drop synchronously released resources");
        drop(broker);
    }

    #[test]
    fn broker_thread_unwind_releases_journal_and_owned_socket() {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let socket = lifecycle_socket(&directory, "unwind");
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("unwind-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let fanout = Arc::new(FanoutState::new());
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let facade = broker.app_action_broker().expect("live broker facade");
        let state = fanout.capnp_state().expect("attached broker state");

        // A failing broker thread is started again on the same socket, and
        // the session's handles to it stay live.
        state.enqueue_thread_callback(Box::new(|| panic!("broker-thread unwind fixture")));
        tilo_test_env::wait_until("the broker thread to start again", || {
            state.thread_restarts() == 1
        });
        let (reached, answered) = std::sync::mpsc::channel();
        state.enqueue_thread_callback(Box::new(move || {
            let _ = reached.send(tilo_rt::panic_boundary::is_isolated());
        }));
        assert!(
            tilo_test_env::recv("the restarted thread's turn", &answered),
            "the restarted thread runs inside its panic boundary"
        );
        assert!(socket.exists(), "a restart keeps the socket");
        assert!(
            !state.stopped.load(Ordering::Acquire),
            "a restart keeps the broker's state live"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("client runtime");
        tokio::task::LocalSet::new().block_on(&runtime, async {
            let (_client, rpc) = dial_capnp_for_test(socket.clone())
                .await
                .expect("a client connects after the restart");
            rpc.abort();
        });

        // One that keeps failing stays down and releases what it held.
        for _ in 0..BROKER_THREAD_RESTARTS {
            state.enqueue_thread_callback(Box::new(|| panic!("broker-thread unwind fixture")));
            tilo_test_env::wait_until("the broker thread to fail", || {
                !socket.exists() || state.callbacks_pending() == 0
            });
        }
        drop(state);

        tilo_test_env::wait_until("the broker to remove its socket", || !socket.exists());
        assert!(!socket.exists(), "unwind releases owned socket custody");
        assert!(
            facade.peer_action_attachment().is_err(),
            "unwind makes surviving broker facades inert"
        );
        let reopened = InvocationJournal::open(&config.journal_root)
            .expect("unwind releases the invocation journal");
        drop(reopened);
        drop(broker);

        let replacement = crate::collab_broker::spawn_with_test_services(
            socket,
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("broker restarts after broker-thread unwind");
        drop(replacement);
    }

    #[test]
    fn connection_exit_cleanup_is_idempotent_and_preserves_survivors_and_replacements() {
        let fixture = local_directory_fixture();
        let survivor = fixture.peer_id + 1;
        let replacement = survivor + 1;
        let survivor_root = ActionDirectoryRoot {
            peer_id: survivor,
            app_id: fixture.route.app_id.clone(),
            cluster_id: fixture.cluster_id.clone(),
            record_id: "survivor".into(),
        };
        fixture
            .state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(survivor, survivor_root);
        fixture.directories.directories.borrow_mut().insert(
            survivor,
            RegisteredActionDirectory {
                registration: Rc::new(uuid::Uuid::new_v4()),
                pagination: directory_pagination::DirectoryPagination::default(),
                publishing_person: Some("fixture-owner".into()),
                app_id: fixture.route.app_id.clone(),
                route: fixture.route.clone(),
                directory: fixture.directory.clone(),
            },
        );
        for peer_id in [fixture.peer_id, survivor] {
            fixture
                .state
                .surfaces
                .lock()
                .expect("published surfaces")
                .insert(
                    peer_id,
                    PublishedSurface {
                        peer_id,
                        app_id: fixture.route.app_id.clone(),
                        route: fixture.route.clone(),
                        actions: Vec::new(),
                        live: true,
                    },
                );
        }

        drop(ConnectionExit {
            state: Arc::clone(&fixture.state),
            directories: Rc::clone(&fixture.directories),
            peer_id: fixture.peer_id,
        });
        drop(ConnectionLease {
            local_media: None,
            state: Arc::clone(&fixture.state),
            directories: Rc::clone(&fixture.directories),
            peer_id: fixture.peer_id,
        });

        assert!(
            !fixture
                .state
                .surfaces
                .lock()
                .expect("published surfaces")
                .contains_key(&fixture.peer_id)
        );
        assert!(
            !fixture
                .directories
                .directories
                .borrow()
                .contains_key(&fixture.peer_id)
        );
        assert!(
            fixture
                .state
                .surfaces
                .lock()
                .expect("published surfaces")
                .contains_key(&survivor)
        );
        assert!(
            fixture
                .directories
                .directories
                .borrow()
                .contains_key(&survivor)
        );

        fixture
            .state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(
                replacement,
                ActionDirectoryRoot {
                    peer_id: replacement,
                    app_id: fixture.route.app_id.clone(),
                    cluster_id: fixture.cluster_id.clone(),
                    record_id: "replacement".into(),
                },
            );
        fixture.state.refresh_action_directory_root_snapshot();
        let peer_ids = fixture
            .state
            .action_directory_roots()
            .into_iter()
            .map(|root| root.peer_id)
            .collect::<std::collections::HashSet<_>>();
        assert!(!peer_ids.contains(&fixture.peer_id));
        assert!(peer_ids.contains(&survivor));
        assert!(peer_ids.contains(&replacement));
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum DirectoryResolveMode {
        Found,
        Stale,
        NotFound,
    }

    /// A real `Cap'n Proto` directory endpoint retained by the `LocalSet` test
    /// below. It answers only bounded inert rows, but is deliberately a
    /// capability client rather than a mock of the shell bridge.
    pub struct TestActionDirectory {
        revision: String,
        pub resolve_mode: DirectoryResolveMode,
        result: tilo_actions::ResultSpec,
        confirmation_required: bool,
        query_count: usize,
        resolve_count: usize,
    }

    impl TestActionDirectory {
        fn contract(&self) -> tilo_actions::ActionContract {
            let mut contract = tilo_actions::ActionContract::view();
            // Opening Atlas changes the invoking participant's view, not the
            // publishing participant's private UI.
            contract
                .tags
                .push(tilo_actions::PARTICIPANT_VIEW_TAG.into());
            contract.result = self.result.clone();
            if self.confirmation_required {
                contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
            contract
        }

        fn row(&self) -> runtime_collab::ActionDirectoryRow {
            let contract = self.contract();
            let descriptor = runtime_collab::ExportedAction {
                semantic_identity: None,
                resource: "workspace:atlas".into(),
                action: "open".into(),
                resource_kind: "workspace".into(),
                resource_label: "Atlas".into(),
                action_label: "Open Atlas".into(),
                description: Some("Open the Atlas workspace".into()),
                operation_kind: tilo_actions::OperationKind::View,
                params: Vec::new(),
                contract: contract.clone(),
                input: runtime_collab::ExportedInput::None,
                view: None,
            };
            runtime_collab::ActionDirectoryRow {
                parent_resource: String::new(),
                resource: descriptor.resource.clone(),
                resource_kind: descriptor.resource_kind.clone(),
                resource_label: descriptor.resource_label.clone(),
                presentation: None,
                action: Some(descriptor.action.clone()),
                descriptor: Some(descriptor),
                has_children: false,
                visibility: Some(runtime_collab::RowVisibility {
                    governing_kind: Some(tilo_actions::OperationKind::View),
                    action: "open".into(),
                    contract,
                }),
            }
        }
    }

    struct TestActionDirectoryServer {
        state: Rc<RefCell<TestActionDirectory>>,
    }

    #[allow(
        refining_impl_trait,
        reason = "the generated Cap'n Proto test endpoint has the same concrete Promise shape as production endpoints"
    )]
    impl action_directory::Server for TestActionDirectoryServer {
        fn query(
            self: Rc<Self>,
            _params: action_directory::QueryParams,
            mut results: action_directory::QueryResults,
        ) -> Promise<(), capnp::Error> {
            let mut state = self.state.borrow_mut();
            state.query_count = state.query_count.saturating_add(1);
            let page = runtime_collab::ActionDirectoryPage {
                revision: state.revision.clone(),
                rows: vec![state.row()],
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            };
            runtime_collab::write_action_directory_page(results.get().init_page(), &page)
                .expect("test directory page encodes");
            Promise::ok(())
        }

        fn resolve(
            self: Rc<Self>,
            _params: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            let mut state = self.state.borrow_mut();
            state.resolve_count = state.resolve_count.saturating_add(1);
            let mut out = results.get().init_resolution();
            match state.resolve_mode {
                DirectoryResolveMode::Found => {
                    runtime_collab::write_action_directory_row(
                        out.reborrow().init_found(),
                        &state.row(),
                    )
                    .expect("test directory row encodes");
                    // As a current owner does: the revision it resolved at.
                    out.set_resolved_revision(state.revision.as_str());
                }
                DirectoryResolveMode::Stale => out.set_stale_revision(()),
                DirectoryResolveMode::NotFound => out.set_not_found(()),
            }
            Promise::ok(())
        }
    }

    #[cfg(test)]
    mod signed_worker {
        include!("signed_worker_test_support.rs");
    }
    #[cfg(test)]
    include!("owner_commit_tests.rs");
    #[cfg(test)]
    include!("resolved_invocation_tests.rs");
    #[cfg(test)]
    include!("approved_check_rpc_tests.rs");
    #[cfg(test)]
    include!("external_moderation_rpc_tests.rs");
    #[cfg(test)]
    include!("approved_check_progress_fixture.rs");
    #[cfg(test)]
    include!("approved_check_progress_rpc_tests.rs");
    #[cfg(test)]
    include!("reliable_resolution_tests.rs");
    #[cfg(test)]
    include!("canonical_route_tests.rs");
    #[cfg(all(test, target_os = "linux"))]
    include!("native_checkout_tests.rs");
    #[cfg(test)]
    include!("durable_local_invocation_matrix_tests.rs");
    #[cfg(test)]
    include!("record_content_tests.rs");
    #[cfg(test)]
    include!("offload_door_tests.rs");

    pub struct LocalDirectoryFixture {
        _fanout: Arc<FanoutState>,
        state: Arc<CapnpBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        directory: action_directory::Client,
        directory_state: Rc<RefCell<TestActionDirectory>>,
        pub broker: crate::collab_broker::AppActionBroker,
        pub membership: Option<WorkspaceTreeHandle>,
        pub cluster_id: String,
        pub route: CrdtRoute,
        pub peer_id: u64,
        invocation_rx: tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>,
        source_identity: Option<AdmittedJoinIdentity>,
    }

    impl LocalDirectoryFixture {
        /// Install the source only after the production certificate, signature,
        /// enrolled-device and current workspace admission checks succeed.
        pub fn authenticate_source(&mut self, presentation: &[u8]) -> Result<CapRole, String> {
            let source = verify_join_identity(
                &self.route,
                presentation,
                self.state.membership.as_ref(),
                self.state.trusted_devices.as_deref().map(Vec::as_slice),
                self.state.services.native_certificates.as_deref(),
            )?;
            let role = source.role;
            self.source_identity = Some(source);
            self.reconnect_directory();
            Ok(role)
        }

        pub fn query_count(&self) -> usize {
            self.directory_state.borrow().query_count
        }
        pub fn disconnect_directory(&self) {
            self.directories.remove(self.peer_id);
        }
        pub fn reconnect_directory(&self) {
            self.directories.directories.borrow_mut().insert(
                self.peer_id,
                RegisteredActionDirectory {
                    registration: Rc::new(uuid::Uuid::new_v4()),
                    pagination: directory_pagination::DirectoryPagination::default(),
                    publishing_person: self.source_identity.as_ref().and_then(|source| {
                        source
                            .certificate
                            .as_ref()
                            .map(|certificate| principal_id_for_root(certificate.user_public_key))
                    }),
                    app_id: self.route.app_id.clone(),
                    route: self.route.clone(),
                    directory: self.directory.clone(),
                },
            );
        }

        pub fn set_confirmation_required(&self, required: bool) {
            self.directory_state.borrow_mut().confirmation_required = required;
        }

        pub fn set_result_spec(&self, result: tilo_actions::ResultSpec) {
            self.directory_state.borrow_mut().result = result;
        }

        fn invocation_channel(&self) -> channel_handle::Client {
            self.invocation_channel_for(self.peer_id, &self.route)
        }

        fn invocation_channel_for(
            &self,
            peer_id: u64,
            route: &CrdtRoute,
        ) -> channel_handle::Client {
            capnp_rpc::new_client(CapnpChannelHandle {
                person_chat_views: RefCell::default(),
                committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                state: Arc::clone(&self.state),
                directories: Rc::clone(&self.directories),
                channel_id: route.channel_key(),
                peer_id,
                connection_id: peer_id,
                verified_identity: self.source_identity.clone().unwrap_or_else(|| {
                    AdmittedJoinIdentity {
                        host_scope: None,
                        worker_registration: None,
                        certificate: None,
                        verified: VerifiedJoinIdentity {
                            user_public_key: [0; 32],
                            device_public_key: [0; 32],
                            capped: false,
                        },
                        principal_id: "fixture-application".into(),
                        display_handle: None,
                        role: CapRole::Editor,
                        agent_id: None,
                        session_delegated: false,
                    }
                }),
                record_kind: None,
                lease: None,
                _hold: None,
            })
        }

        /// Exercise the production peer-bound update RPC, including result-contract validation.
        pub async fn complete_invocation(
            &self,
            id: u64,
            update: &tilo_actions::InvocationUpdate,
        ) -> Result<(), capnp::Error> {
            let client = self.invocation_channel();
            let mut request = client.update_invocation_request();
            request.get().set_id(id);
            request
                .get()
                .set_update_json(serde_json::to_string(update).expect("canonical update"));
            request.send().promise.await?;
            Ok(())
        }

        pub fn set_resolve_mode(&self, mode: DirectoryResolveMode) {
            self.directory_state.borrow_mut().resolve_mode = mode;
        }

        pub fn take_invocation_identity(&mut self) -> Option<(u64, tilo_actions::InvocationId)> {
            self.invocation_rx
                .try_recv()
                .ok()
                .map(|invocation| (invocation.id, invocation.invocation))
        }

        pub fn take_invocation_actor(&mut self) -> Option<String> {
            self.invocation_rx
                .try_recv()
                .ok()
                .map(|invocation| invocation.actor)
        }

        /// The next delivered invocation's actor and the invoker the host
        /// recorded for it.
        pub fn take_invocation_invoker(
            &mut self,
        ) -> Option<(String, Option<tilo_actions::ActionInvoker>)> {
            self.invocation_rx.try_recv().ok().map(|invocation| {
                let invoker = invocation
                    .context
                    .as_ref()
                    .and_then(|context| context.invoker.clone());
                (invocation.actor, invoker)
            })
        }

        pub fn take_invocation_argument(&mut self) -> Option<Option<String>> {
            self.invocation_rx
                .try_recv()
                .ok()
                .map(|invocation| invocation.argument)
        }

        pub fn resolve_count(&self) -> usize {
            self.directory_state.borrow().resolve_count
        }
    }

    #[cfg(test)]
    #[tokio::test(flavor = "current_thread")]
    async fn native_media_raw_admitted_channel_has_no_local_process_authority() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let fixture = local_directory_fixture();
                let channel = fixture.invocation_channel();
                for command in [
                    runtime_collab::native_media::Command::Join {
                        channel_id: "voice".into(),
                    },
                    runtime_collab::native_media::Command::SetMuted { muted: false },
                ] {
                    let mut request = runtime_collab::native_media::Request::new(command);
                    request.intent = 1;
                    let mut rpc = channel.native_media_command_request();
                    rpc.get().set_command_json(&request.encode().unwrap());
                    let error = rpc
                        .send()
                        .promise
                        .await
                        .err()
                        .expect("ordinary admitted channel cannot mint local process custody");
                    assert!(error.to_string().contains("launcher-attested"), "{error}");
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn network_wake_rpc_keeps_host_queries_outside_the_settings_sandbox() {
        struct HostNetwork(Mutex<Vec<std::thread::ThreadId>>);
        impl crate::services::WifiContinuityServices for HostNetwork {
            fn command(
                &self,
                command: runtime_collab::WifiContinuityCommand,
            ) -> runtime_collab::WifiContinuityReply {
                self.0.lock().unwrap().push(std::thread::current().id());
                let runtime_collab::WifiContinuityCommand::NetworkWake { profile_uuid, .. } =
                    command
                else {
                    panic!("the RPC must preserve the typed wake request");
                };
                runtime_collab::WifiContinuityReply::NetworkWake {
                    status: runtime_collab::NetworkWakeStatus {
                        profile: profile_uuid,
                        interface: Some("wlp1s0".into()),
                        configured: true,
                        supported: Some(true),
                        applied: Some(true),
                        problem: None,
                    },
                }
            }
        }
        tokio::task::LocalSet::new().run_until(async {
            let service = Arc::new(HostNetwork(Mutex::new(Vec::new())));
            let base = local_directory_fixture();
            let mut fixture = local_directory_fixture_with_authority(
                base.membership.as_ref().unwrap().authority(),
                HostServices { wifi_continuity: Some(service.clone()), ..Default::default() },
                Some(71), None,
            );
            fixture.route = CrdtRoute::app(WIFI_CONTINUITY_APP_ID, collab_types::person_room(&[0; 32]).to_string());
            let client = fixture.invocation_channel();
            let command = serde_json::to_vec(&runtime_collab::WifiContinuityCommand::NetworkWake {
                profile_uuid: "11111111-1111-4111-8111-111111111111".into(), enabled: Some(true),
            }).unwrap();
            let mut request = client.wifi_continuity_command_request();
            request.get().set_app_id(WIFI_CONTINUITY_APP_ID);
            request.get().set_command_json(&command);
            let response = request.send().promise.await.unwrap();
            let reply: runtime_collab::WifiContinuityReply = serde_json::from_slice(response.get().unwrap().get_reply_json().unwrap()).unwrap();
            assert!(reply.valid());
            assert!(matches!(reply, runtime_collab::WifiContinuityReply::NetworkWake { status } if status.applied == Some(true)));
            assert_ne!(service.0.lock().unwrap().as_slice(), [std::thread::current().id()]);
            let mut spoofed = client.wifi_continuity_command_request();
            spoofed.get().set_app_id("so.tilo.browser");
            spoofed.get().set_command_json(&command);
            assert!(spoofed.send().promise.await.is_err());
            fixture.route.app_id = "so.tilo.browser".into();
            let mut forbidden = fixture.invocation_channel().wifi_continuity_command_request();
            forbidden.get().set_app_id(WIFI_CONTINUITY_APP_ID);
            forbidden.get().set_command_json(&command);
            assert!(forbidden.send().promise.await.is_err());
            assert_eq!(service.0.lock().unwrap().len(), 1);
        }).await;
    }

    #[cfg(test)]
    #[tokio::test(flavor = "current_thread")]
    async fn file_clipboard_raw_admitted_channel_cannot_mint_local_custody() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let fixture = local_directory_fixture();
                let channel = fixture.invocation_channel();
                for command in [
                    runtime_collab::file_clipboard::Command::Snapshot,
                    runtime_collab::file_clipboard::Command::Publish {
                        sources: vec![uuid::Uuid::new_v4().to_string()],
                        cut: true,
                    },
                ] {
                    let mut request = channel.local_file_clipboard_request();
                    request
                        .get()
                        .set_command_json(&serde_json::to_vec(&command).expect("command"));
                    request.get().set_on_behalf_of(&[]);
                    let error = request
                        .send()
                        .promise
                        .await
                        .err()
                        .expect("unattested channel must be refused");
                    assert!(error.to_string().contains("launcher-attested"), "{error}");
                }
            })
            .await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    async fn selected_open_rpc_requires_actual_files_peer_and_original_invocation() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let fixture = local_directory_fixture();
                let channel = fixture.invocation_channel();
                let discovery = runtime_collab::open_selected::discovery::Request {
                    invocation: None,
                    protocol: runtime_collab::open_selected::discovery::PROTOCOL.into(),
                    schema: 1,
                    request_id: uuid::Uuid::new_v4().to_string(),
                    components: vec![b"document.md".to_vec()],
                };
                let mut rpc = channel.local_file_clipboard_request();
                rpc.get()
                    .set_command_json(&serde_json::to_vec(&discovery).unwrap());
                rpc.get().set_on_behalf_of(&[]);
                let error = rpc
                    .send()
                    .promise
                    .await
                    .err()
                    .expect("discovery cannot mint a local Files peer");
                assert!(error.to_string().contains("launcher-attested"), "{error}");
                let selected = runtime_collab::open_selected::v2::Request {
                    protocol: runtime_collab::open_selected::v2::PROTOCOL.into(),
                    resource: "entry:document".into(),
                    request: runtime_collab::open_selected::v2::RootRequest {
                        schema: 2,
                        request_id: uuid::Uuid::new_v4().to_string(),
                        components: vec![b"document.md".to_vec()],
                        app_id: "so.tilo.text-editor".into(),
                        access: runtime_collab::open_selected::v2::SelectedAccess::EditInParent,
                    },
                    selected_descriptor: 3,
                    parent_descriptor: 4,
                    proof_descriptor: 5,
                };
                let mut rpc = channel.local_file_clipboard_request();
                rpc.get()
                    .set_command_json(&serde_json::to_vec(&selected).unwrap());
                rpc.get().set_on_behalf_of(&[]);
                let error = rpc
                    .send()
                    .promise
                    .await
                    .err()
                    .expect("Edit requires its actual semantic invocation");
                assert!(
                    error.to_string().contains("admitted semantic invocation"),
                    "{error}"
                );
                let navigation = selected.navigation();
                let mut rpc = channel.local_file_clipboard_request();
                rpc.get()
                    .set_command_json(&serde_json::to_vec(&navigation).unwrap());
                rpc.get().set_on_behalf_of(&[]);
                let error = rpc
                    .send()
                    .promise
                    .await
                    .err()
                    .expect("Read requires its actual semantic invocation");
                assert!(
                    error.to_string().contains("admitted semantic invocation"),
                    "{error}"
                );
            })
            .await;
    }

    #[cfg(test)]
    #[tokio::test(flavor = "current_thread")]
    async fn audio_output_raw_admitted_channel_cannot_open_or_write_host_pcm() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let fixture = local_directory_fixture();
                let channel = fixture.invocation_channel();
                for command in [
                    runtime_collab::audio_output::Command::Open {},
                    runtime_collab::audio_output::Command::Write {
                        generation: 1,
                        sequence: 1,
                        samples: vec![123; 1920],
                    },
                ] {
                    let mut rpc = channel.audio_output_command_request();
                    rpc.get().set_command(&command.encode().unwrap());
                    let error = rpc.send().promise.await.err().expect(
                        "an admitted remote/raw route cannot manufacture launcher process custody",
                    );
                    assert!(error.to_string().contains("launcher-attested"), "{error}");
                }
            })
            .await;
    }

    #[cfg(test)]
    async fn witness_publish(
        fixture: &LocalDirectoryFixture,
        channel: &channel_handle::Client,
        app: &str,
        directory: bool,
    ) {
        if directory {
            let mut request = channel.publish_action_directory_request();
            request.get().set_app_id(app);
            request.get().set_directory(fixture.directory.clone());
            request.send().promise.await.unwrap();
        } else {
            let mut request = channel.publish_actions_request();
            request.get().set_app_id(app);
            request.get().init_actions(0);
            request.send().promise.await.unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn witness_publication_is_target_scoped_and_rejects_republish_and_peer_reuse() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for directory in [false, true] {
                    let fixture = local_directory_fixture();
                    let person = principal_id_for_root([0; 32]);
                    let mut other_workspace = crate::test_support::Workspace::new(
                        crate::test_support::WorkspaceId(72),
                        "Other publication room",
                    );
                    other_workspace.set_owner(person.clone());
                    let other_room = other_workspace.room().to_string();
                    {
                        let mut tree = fixture.membership.as_ref().unwrap().write();
                        tree.workspaces[0].set_owner(person);
                        tree.workspaces.push(other_workspace);
                    }
                    let target = fixture.invocation_channel();
                    witness_publish(&fixture, &target, &fixture.route.app_id, directory).await;
                    let request = tilo_actions::remote::InvokeRequest {
                        invocation: tilo_actions::InvocationId::new(),
                        target: tilo_actions::remote::RemoteActionTarget {
                            provider: fixture.peer_id.to_string(),
                            address: tilo_actions::ActionAddress::new("document", "open"),
                        },
                        argument: tilo_actions::remote::InvokeArgument::None,
                        confirmation: None,
                        context: None,
                        revision: Some("directory-r1".into()),
                        wait_ms: None,
                        appearance: None,
                        on_behalf_of: None,
                    };
                    let binding = || {
                        fixture
                            .state
                            .witness_binding("person", &fixture.cluster_id, &request)
                    };
                    let original = binding();
                    let token = fixture.state.witness_confirmation(&original).unwrap();
                    // Both another provider in this room and another room may publish,
                    // reregister and disconnect while the person's sheet is open.
                    for room in [fixture.cluster_id.clone(), other_room] {
                        let other_peer = fixture.peer_id + 1;
                        let route = CrdtRoute::record("org.example.unrelated", room, "main");
                        let other = fixture.invocation_channel_for(other_peer, &route);
                        for other_directory in [false, true] {
                            witness_publish(&fixture, &other, &route.app_id, other_directory).await;
                            witness_publish(&fixture, &other, &route.app_id, other_directory).await;
                        }
                        fixture.state.remove_connection(other_peer);
                    }
                    assert!(
                        fixture.state.take_witness(&token, &binding()),
                        "unrelated churn preserves consent"
                    );

                    let token = fixture.state.witness_confirmation(&original).unwrap();
                    witness_publish(&fixture, &target, &fixture.route.app_id, directory).await;
                    assert!(
                        !fixture.state.take_witness(&token, &binding()),
                        "identical target republication revokes consent"
                    );

                    let current = binding();
                    let token = fixture.state.witness_confirmation(&current).unwrap();
                    fixture.state.remove_connection(fixture.peer_id);
                    assert!(
                        !fixture.state.take_witness(&token, &binding()),
                        "disconnect revokes consent"
                    );
                    let token = fixture.state.witness_confirmation(&current).unwrap();
                    witness_publish(&fixture, &target, &fixture.route.app_id, directory).await;
                    assert!(
                        !fixture.state.take_witness(&token, &binding()),
                        "peer reuse cannot restore old consent"
                    );
                }
            })
            .await;
    }

    #[cfg(test)]
    fn local_directory_fixture() -> LocalDirectoryFixture {
        let mut tree = crate::test_support::WorkspaceTree::default();
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(71),
            "Directory test room",
        );
        workspace.set_owner("fixture-owner".to_string());
        tree.workspaces.push(workspace);
        tree.set_active(crate::test_support::WorkspaceId(71));
        local_directory_fixture_with_membership(tree.shared())
    }

    pub fn local_directory_fixture_with_membership(
        membership: crate::test_support::WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_peer(membership, Some(71))
    }

    pub fn local_directory_fixture_with_first_allocated_peer(
        membership: crate::test_support::WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_peer(membership, None)
    }

    fn local_directory_fixture_with_peer(
        membership: crate::test_support::WorkspaceTreeHandle,
        peer_id: Option<u64>,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_devices(membership, peer_id, None)
    }

    fn local_directory_fixture_with_devices(
        membership: WorkspaceTreeHandle,
        peer_id: Option<u64>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> LocalDirectoryFixture {
        let mut fixture = local_directory_fixture_with_authority(
            membership.authority(),
            membership.services(),
            peer_id,
            trusted_devices,
        );
        fixture.membership = Some(membership);
        fixture
    }

    pub fn local_directory_fixture_with_authority(
        membership: crate::services::WorkspaceAuthorityHandle,
        services: HostServices,
        peer_id: Option<u64>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> LocalDirectoryFixture {
        let cluster_id = membership
            .snapshot()
            .unwrap()
            .active_workspace()
            .expect("fixture active room")
            .room()
            .to_string();
        let route = CrdtRoute::record("so.tilo.atlas", cluster_id.clone(), "atlas");
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.clone()),
            trusted_devices,
            None,
            services,
        );
        let peer_id = peer_id.unwrap_or_else(|| state.next_peer_id());
        let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
        let directory_state = Rc::new(RefCell::new(TestActionDirectory {
            revision: "directory-r1".into(),
            resolve_mode: DirectoryResolveMode::Found,
            result: tilo_actions::ResultSpec::None,
            confirmation_required: false,
            query_count: 0,
            resolve_count: 0,
        }));
        let directory: action_directory::Client =
            capnp_rpc::new_client(TestActionDirectoryServer {
                state: Rc::clone(&directory_state),
            });
        directories.directories.borrow_mut().insert(
            peer_id,
            RegisteredActionDirectory {
                registration: Rc::new(uuid::Uuid::new_v4()),
                pagination: directory_pagination::DirectoryPagination::default(),
                publishing_person: None,
                app_id: route.app_id.clone(),
                route: route.clone(),
                directory: directory.clone(),
            },
        );
        state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(
                peer_id,
                ActionDirectoryRoot {
                    peer_id,
                    app_id: route.app_id.clone(),
                    cluster_id: cluster_id.clone(),
                    record_id: route.record_id.clone(),
                },
            );
        state.refresh_action_directory_root_snapshot();
        let invocation_rx = state.capture_invocations_for_test(peer_id);
        fanout.attach_capnp(Arc::clone(&state));
        LocalDirectoryFixture {
            _fanout: fanout,
            broker: crate::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&state)),
            membership: None,
            state,
            directories,
            directory,
            directory_state,
            cluster_id,
            route,
            peer_id,
            invocation_rx,
            source_identity: None,
        }
    }

    #[allow(
        clippy::future_not_send,
        reason = "the directory bridge is deliberately driven on the LocalSet because its Cap'n Proto state contains Rc values"
    )]
    pub async fn drain_directory_bridge(fixture: &LocalDirectoryFixture) {
        fixture
            .state
            .drain_action_directory_bridge(&fixture.directories);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    #[test]
    fn directory_queue_refuses_oversized_query_text_and_cursor_before_contacting_the_app() {
        let fixture = local_directory_fixture();
        let queue = fixture.broker.action_directory_request_queue();
        let mut oversized_text = runtime_collab::ActionDirectoryQuery {
            peer_id: fixture.peer_id,
            revision: String::new(),
            cursor: Vec::new(),
            needle: "x".repeat(DIRECTORY_MAX_QUERY_NEEDLE_BYTES.saturating_add(1)),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 1,
            max_work: 1,
            max_depth: 1,
            resource_name: String::new(),
        };
        assert!(matches!(
            queue.try_query(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                oversized_text.clone()
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        oversized_text.needle.clear();
        oversized_text.cursor = vec![0; DIRECTORY_MAX_CURSOR_BYTES.saturating_add(1)];
        assert!(matches!(
            queue.try_query(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                oversized_text
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert!(matches!(
            queue.try_resolve(
                BrokerActor::person("actor-a"),
                "x".repeat(DIRECTORY_MAX_IDENTIFIER_BYTES.saturating_add(1)),
                fixture.peer_id,
                "directory-r1",
                "workspace:atlas",
                "open",
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        let target = DirectoryActionTarget {
            cluster_id: fixture.cluster_id.clone(),
            peer_id: fixture.peer_id,
            revision: "directory-r1".into(),
            address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
        };
        assert!(matches!(
            queue.try_start_resolved(
                BrokerActor::person("actor-a"),
                target.clone(),
                Some("x".repeat(DIRECTORY_MAX_PARAMS_BYTES.saturating_add(1))),
                None,
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert!(matches!(
            queue.try_start_resolved(
                BrokerActor::person("actor-a"),
                target,
                None,
                Some(DirectoryActionConfirmation::Phrase(
                    "x".repeat(DIRECTORY_MAX_CONFIRMATION_BYTES.saturating_add(1)),
                )),
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert_eq!(
            fixture.directory_state.borrow().query_count,
            0,
            "invalid local requests are rejected before a directory capability is contacted"
        );
    }

    #[test]
    fn published_semantic_identity_retains_exact_directory_address_and_refuses_bad_wire() {
        use runtime_collab::collab_capnp::exported_action;

        let fixture = local_directory_fixture();
        let mut row = fixture.directory_state.borrow().row();
        let identity = runtime_collab::ExportedResourceSemanticIdentity {
            kind: "workspace".into(),
            key: "source-workspace:atlas".into(),
        };
        row.descriptor.as_mut().unwrap().semantic_identity = Some(identity);
        let action = published_directory_action(&row).unwrap();
        assert_eq!(action.resource, row.resource);
        assert_eq!(action.action, row.action.as_deref().unwrap());
        assert_eq!(
            action.semantic_identity,
            tilo_actions::ResourceSemanticIdentity::new("workspace", "source-workspace:atlas")
        );

        let decode = |kind: &str, key: &str| {
            let mut message = capnp::message::Builder::new_default();
            let mut descriptor = message.init_root::<exported_action::Builder<'_>>();
            descriptor.set_resource_kind("chat.agent");
            // A published action names its kind, parameters and contract.
            descriptor.set_operation_kind("effect");
            descriptor.set_params_json("[]");
            descriptor.set_contract_json(
                serde_json::to_string(&tilo_actions::ActionContract::default()).unwrap(),
            );
            let mut identity = descriptor.init_semantic_identity();
            identity.set_kind(kind);
            identity.set_key(key);
            runtime_collab::decode_exported_action(
                message
                    .get_root_as_reader::<exported_action::Reader<'_>>()
                    .unwrap(),
            )
            .map(|action| action.semantic_identity)
        };
        assert_eq!(
            decode("chat.agent", "chat.person:scribe").unwrap(),
            tilo_actions::ResourceSemanticIdentity::new("chat.agent", "chat.person:scribe")
        );
        assert!(decode("chat.person", "chat.person:scribe").is_err());
        assert!(decode("chat.agent", "key\nsecret").is_err());
        assert!(decode("chat.agent", &"x".repeat(4097)).is_err());
        row.descriptor
            .as_mut()
            .unwrap()
            .semantic_identity
            .as_mut()
            .unwrap()
            .kind = "person".into();
        assert!(published_directory_action(&row).is_err());
    }

    /// A directory whose container row carries a visibility contract the
    /// broker cannot read, beside a well-formed action row.
    #[cfg(test)]
    struct UndecodableVisibilityDirectory {
        row: runtime_collab::ActionDirectoryRow,
    }

    #[allow(
        refining_impl_trait,
        reason = "the generated Cap'n Proto test endpoint has the same concrete Promise shape as production endpoints"
    )]
    #[cfg(test)]
    impl action_directory::Server for UndecodableVisibilityDirectory {
        fn query(
            self: Rc<Self>,
            _params: action_directory::QueryParams,
            mut results: action_directory::QueryResults,
        ) -> Promise<(), capnp::Error> {
            let container = runtime_collab::ActionDirectoryRow {
                parent_resource: String::new(),
                resource: "workspace:folder".into(),
                resource_kind: "workspace".into(),
                resource_label: "Folder".into(),
                presentation: None,
                action: None,
                descriptor: None,
                has_children: true,
                visibility: Some(runtime_collab::RowVisibility {
                    governing_kind: Some(tilo_actions::OperationKind::View),
                    action: "open".into(),
                    contract: tilo_actions::ActionContract::view(),
                }),
            };
            let page = runtime_collab::ActionDirectoryPage {
                revision: "directory-r1".into(),
                rows: vec![self.row.clone(), container],
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            };
            let mut out = results.get().init_page();
            runtime_collab::write_action_directory_page(out.reborrow(), &page)
                .expect("test directory page encodes");
            // A permission the broker does not know: the gate is unreadable.
            out.get_rows()
                .unwrap()
                .get(1)
                .set_visibility_contract_json(r#"{"kind":"everyone"}"#);
            Promise::ok(())
        }
    }

    /// The visibility contract is the permission that discloses a container
    /// row. One the broker cannot decode refuses the page: the row is never
    /// listed without its gate, and nothing on that page is listed past it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_row_whose_visibility_cannot_be_decoded_is_not_listed() {
        use tilo_actions::remote::{Caller, CatalogueQuery, PendingPoll};
        tokio::task::LocalSet::new()
            .run_until(async {
                let fixture = local_directory_fixture();
                let caller = Caller::local_person("fixture-owner")
                    .in_room(fixture.cluster_id.clone())
                    .in_new_execution();
                let query = CatalogueQuery {
                    provider: Some(fixture.peer_id.to_string()),
                    max_results: 8,
                    ..CatalogueQuery::default()
                };
                let list = || async {
                    let mut pending = fixture.state.action_query(&caller, &query);
                    tilo_test_env::within("bounded directory query", async {
                        loop {
                            drain_directory_bridge(&fixture).await;
                            if let PendingPoll::Ready(result) = pending.poll_ready() {
                                break result;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                };
                let listed = list().await.expect("a well-formed directory lists");
                assert_eq!(
                    listed.rows.len(),
                    1,
                    "the fixture row is visible to its owner"
                );

                let row = fixture.directory_state.borrow().row();
                fixture
                    .directories
                    .directories
                    .borrow_mut()
                    .get_mut(&fixture.peer_id)
                    .expect("fixture directory")
                    .directory = capnp_rpc::new_client(UndecodableVisibilityDirectory { row });
                let refused = list().await;
                assert!(refused.is_err(), "{refused:?}");
            })
            .await;
    }

    #[test]
    fn directory_wire_rows_refuse_control_framing_in_identifiers_before_policy_or_invocation() {
        let fixture = local_directory_fixture();
        let mut row = fixture.directory_state.borrow().row();
        row.resource = "workspace:atlas\u{1f}admin".into();
        assert!(
            permits_directory_row(
                &fixture.state,
                &fixture.cluster_id,
                &BrokerActor::person("actor-a"),
                &fixture.route.app_id,
                None,
                &row,
            )
            .is_err(),
            "a resource cannot inject the shell result-id separator"
        );
        let mut row = fixture.directory_state.borrow().row();
        row.action = Some("open\nadmin".into());
        assert!(
            permits_directory_row(
                &fixture.state,
                &fixture.cluster_id,
                &BrokerActor::person("actor-a"),
                &fixture.route.app_id,
                None,
                &row,
            )
            .is_err(),
            "control characters cannot turn one wire action into several shell fields"
        );
    }

    #[test]
    fn retained_directory_root_pages_reach_exact_bound_and_tail_without_query_sorting() {
        let fixture = local_directory_fixture();
        {
            let mut roots = fixture
                .state
                .action_directory_roots
                .lock()
                .expect("directory roots");
            for peer_id in 1..=129_u64 {
                roots.insert(
                    peer_id,
                    ActionDirectoryRoot {
                        peer_id,
                        app_id: "so.tilo.test".into(),
                        cluster_id: fixture.cluster_id.clone(),
                        record_id: format!("root-{peer_id}"),
                    },
                );
            }
            drop(roots);
        }
        fixture.state.refresh_action_directory_root_snapshot();
        let first = fixture.state.action_directory_root_page(None, 128);
        assert_eq!(first.roots.len(), 128);
        assert_eq!(first.next_after_peer, Some(128));
        let second = fixture
            .state
            .action_directory_root_page(first.next_after_peer, 128);
        assert_eq!(second.roots.len(), 1);
        assert_eq!(second.roots[0].peer_id, 129);
        assert_eq!(second.next_after_peer, None);
        let repeat = fixture.state.action_directory_root_page(None, 1);
        assert_eq!(repeat.roots[0].peer_id, 1);
        assert_eq!(repeat.generation, second.generation);
        fixture.state.remove_connection(129);
        let after_disconnect = fixture.state.action_directory_root_page(Some(128), 1);
        assert!(after_disconnect.roots.is_empty());
        assert!(after_disconnect.generation > second.generation);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn directory_fixture_preserves_scoped_uuid_through_actual_completion() {
        use tilo_actions::remote::{
            Caller, InvokeArgument, InvokeRequest, PendingPoll, RemoteActionTarget,
        };
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut fixture = local_directory_fixture();
                let caller = Caller::local_person("fixture-owner")
                    .in_room(fixture.cluster_id.clone())
                    .in_new_execution();
                let request = InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: fixture.peer_id.to_string(),
                        address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                    },
                    argument: InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: Some("directory-r1".into()),
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                };
                let mut pending = fixture.state.action_invoke(&caller, None, None, &request);
                let admitted = tilo_test_env::within("bounded actual directory admission", async {
                    loop {
                        drain_directory_bridge(&fixture).await;
                        if let PendingPoll::Ready(result) = pending.poll_ready() {
                            break result;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await;
                admitted.expect("full-envelope consumer supports scoped invocation UUIDs");
                let (job, delivered) = fixture.take_invocation_identity().expect("actual delivery");
                assert_eq!(delivered, request.invocation);
                assert_ne!(delivered.0, uuid::Uuid::from_u128(u128::from(job)));
                fixture
                    .complete_invocation(
                        job,
                        &tilo_actions::InvocationUpdate::Succeeded {
                            invocation: delivered,
                            output: tilo_actions::ActionOutput::default(),
                        },
                    )
                    .await
                    .expect("actual peer-bound completion accepts the delivered UUID");
                assert!(fixture.take_invocation_identity().is_none());
            })
            .await;
    }

    #[test]
    fn invocation_journal_admission_precedes_real_application_delivery() {
        let root = tempfile::tempdir().expect("private test root");
        let base = local_directory_fixture();
        let membership = base.membership.as_ref().unwrap().clone();
        membership.write().persist_path = Some(root.path().join("state.json"));
        drop(base);
        let mut fixture = local_directory_fixture_with_membership(membership);
        let offered = PublishedSurface {
            peer_id: fixture.peer_id,
            app_id: fixture.route.app_id.clone(),
            route: fixture.route.clone(),
            actions: vec![
                published_directory_action(&fixture.directory_state.borrow().row()).unwrap(),
            ],
            live: true,
        };
        fixture
            .state
            .surfaces
            .lock()
            .unwrap()
            .insert(fixture.peer_id, offered);
        let actor = fixture
            .membership
            .as_ref()
            .unwrap()
            .read()
            .active_workspace()
            .unwrap()
            .owner()
            .cloned()
            .unwrap();
        let job = fixture
            .state
            .start_invocation(fixture.peer_id, "workspace:atlas", "open", None, &actor)
            .unwrap();
        let delivered = fixture
            .invocation_rx
            .try_recv()
            .expect("actual app invocation stream");
        assert_eq!(delivered.id, job.id);
        // What a restart would replay: the real broker must persist admission
        // before delivering an application effect.
        let snapshot =
            invocation_journal::durable_snapshot(&root.path().join("state.json.invocations"));
        assert_eq!(snapshot["records"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["records"][0]["wire_id"], job.id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_person_requested_uuid_survives_actual_delivery_and_completion() {
        use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
        tokio::task::LocalSet::new().run_until(async {
            let root = tempfile::tempdir().unwrap();
            let base = local_directory_fixture();
            let membership = base.membership.as_ref().unwrap().clone();
            membership.write().persist_path = Some(root.path().join("state.json"));
            drop(base);
            let mut fixture = local_directory_fixture_with_membership(membership);
            let offered = PublishedSurface {
                peer_id: fixture.peer_id,
                app_id: fixture.route.app_id.clone(),
                route: fixture.route.clone(),
                actions: vec![published_directory_action(&fixture.directory_state.borrow().row()).unwrap()],
                live: true,
            };
            fixture.state.surfaces.lock().unwrap().insert(fixture.peer_id, offered);
            let caller = Caller::local_person("fixture-owner").in_room(fixture.cluster_id.clone());
            assert!(caller.execution_id().is_none(), "no fabricated execution lease");
            let request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: fixture.peer_id.to_string(),
                    address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                },
                argument: InvokeArgument::None, context: None, confirmation: None,
                revision: None, on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            };
            let receipt = fixture
                .state
                .action_invoke(&caller, None, None, &request)
                .await
                .expect("the host answers")
                .expect("published ordinary action must be admitted");
            assert_eq!(receipt.invocation, request.invocation);
            let (job, delivered) = fixture.take_invocation_identity().unwrap();
            assert_eq!(delivered, request.invocation);
            assert_ne!(delivered.0, uuid::Uuid::from_u128(u128::from(job)));
            assert!(matches!(fixture.state.invocation_status(job),
                Some(tilo_actions::InvocationUpdate::Accepted { invocation }) if invocation == request.invocation));
            let path = root.path().join("state.json.invocations");
            let snapshot = invocation_journal::durable_snapshot(&path);
            assert_eq!(snapshot["records"][0]["key"]["invocation"], request.invocation.0.to_string());
            fixture.complete_invocation(job, &tilo_actions::InvocationUpdate::Succeeded {
                invocation: delivered, output: tilo_actions::ActionOutput::default(),
            }).await.unwrap();
            assert!(matches!(fixture.state.invocation_status(job),
                Some(tilo_actions::InvocationUpdate::Succeeded { invocation, .. }) if invocation == request.invocation));
            let snapshot = invocation_journal::durable_snapshot(&path);
            assert_eq!(snapshot["records"][0]["key"]["invocation"], request.invocation.0.to_string());
            assert!(fixture.take_invocation_identity().is_none());
        }).await;
    }

    #[cfg(test)]
    fn certified_join(
        workspace: &str,
        root: &SigningKey,
        device: &SigningKey,
    ) -> runtime_collab::CertifiedJoinIdentity {
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Test device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).expect("root signs device cert");
        runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.into(),
            device_certificate: serde_json::to_vec(&cert).expect("certificate JSON"),
            proof: device
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    workspace,
                ))
                .to_bytes()
                .to_vec(),
        }
    }

    #[test]
    fn certified_join_verifies_root_device_and_workspace_chain() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let device = SigningKey::from_bytes(&[2; 32]);
        let route = CrdtRoute::cluster("workspace-a");
        let presentation = certified_join("workspace-a", &root, &device);
        let verified = verify_join_presentation(
            &route,
            &presentation,
            &root.verifying_key().to_bytes(),
            2,
            false,
            true,
        )
        .expect("valid certified join");
        assert_eq!(verified.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(
            verified.device_public_key,
            device.verifying_key().to_bytes()
        );
    }

    #[cfg(test)]
    struct ParticipantJoinFixture {
        state: Arc<CapnpBrokerState>,
        fanout: Arc<FanoutState>,
        client: runtime_collab::CollabClient,
        presentation: Vec<u8>,
        route: CrdtRoute,
        principal: String,
    }

    #[cfg(test)]
    impl ParticipantJoinFixture {
        fn new(agents: &[String]) -> Self {
            let root = SigningKey::from_bytes(&[101; 32]);
            let device = SigningKey::from_bytes(&[102; 32]);
            let principal = principal_id_for_root(root.verifying_key().to_bytes());
            let mut workspace = crate::test_support::Workspace::new(
                crate::test_support::WorkspaceId(101),
                "Exact Agent identity",
            );
            workspace.set_owner(principal.clone());
            let route = CrdtRoute::cluster(workspace.room().to_string());
            for agent in agents {
                workspace.admit_agent(agent, crate::test_support::KindSet::READ);
            }
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let fanout = Arc::new(FanoutState::new());
            let (state, bridge_rx) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                PeerPublisher::disabled(),
                Some(tree.shared()),
                Some(Arc::new(vec![device.verifying_key().to_bytes()])),
            );
            let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
            let peer_id = fanout.next_peer_id();
            let lease = Rc::new(ConnectionLease {
                local_media: None,
                state: Arc::clone(&state),
                directories: Rc::clone(&directories),
                peer_id,
            });
            let raw: collab_service::Client = capnp_rpc::new_client(CapnpCollabService {
                worker: None,
                app_worker: None,
                state: Arc::clone(&state),
                directories,
                peer_id,
                lease,
                holds: route_holds::ConnectionHolds::default(),
            });
            let presentation = certified_join(&route.cluster_id, &root, &device)
                .encode()
                .unwrap();
            Self {
                state,
                fanout,
                client: runtime_collab::CollabClient::from_raw(raw),
                presentation,
                route,
                principal,
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn signed_agent_join_preserves_exact_ascii_and_multibyte_identity() {
        let ascii = "a".repeat(128);
        let multibyte = "\u{e9}".repeat(64);
        let fixture = ParticipantJoinFixture::new(&[
            ascii.clone(),
            multibyte.clone(),
            // The old display-name truncation would silently select this
            // different, also admitted Agent rather than reject the join.
            "a".repeat(64),
        ]);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let _subscription = fixture.fanout.subscribe_rooms(
            vec![fixture.route.cluster_id.clone()],
            Box::new(move |event, _| {
                event_tx.send(event.clone()).unwrap();
            }),
        );
        let key = PresenceKey::Resource {
            app: "so.tilo.notes".into(),
            id: "note:draft".into(),
            kind: "note".into(),
        };
        let key_bytes = postcard::to_stdvec(&key).unwrap();
        for expected in [ascii, multibyte] {
            let input = format!(" {} ", expected.to_uppercase());
            let channel = fixture
                .client
                .join_channel_as_kind(
                    &fixture.route.channel_key(),
                    &fixture.presentation,
                    &input,
                    "agent",
                    "",
                )
                .await
                .expect("certified join preserves the complete canonical Agent ID");
            channel
                .presence()
                .await
                .unwrap()
                .publish_arrived(&key_bytes)
                .await
                .unwrap();
            let event = event_rx.try_recv().unwrap();
            assert!(matches!(event,
                CollabEvent::RemotePresenceUpdate { user_id, key: delivered, payload: PresencePayload::Arrived }
                    if user_id == format!("agent:{expected}") && delivered == key));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn signed_agent_join_refuses_invalid_ids_without_display_prefix_fallback() {
        let fixture =
            ParticipantJoinFixture::new(&["a".repeat(64), "a".repeat(128), "\u{e9}".repeat(64)]);
        for invalid in [
            "a".repeat(129),
            "\u{e9}".repeat(65),
            // Lowercase expands this input from 86 to 129 UTF-8 bytes.
            "\u{130}".repeat(43),
            format!("{}\nx", "a".repeat(64)),
            String::new(),
        ] {
            assert!(
                fixture
                    .client
                    .join_channel_as_kind(
                        &fixture.route.channel_key(),
                        &fixture.presentation,
                        &invalid,
                        "agent",
                        "",
                    )
                    .await
                    .is_err(),
                "invalid Agent IDs must refuse instead of selecting an enrolled prefix"
            );
            assert!(fixture.state.channels.lock().unwrap().is_empty());
        }
        // The same long string is still a Person's bounded presentation
        // handle: it never becomes that Person's certified principal.
        let person = fixture
            .client
            .join_channel_as_kind(
                &fixture.route.channel_key(),
                &fixture.presentation,
                &"a".repeat(129),
                "person",
                "",
            )
            .await
            .unwrap();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let _subscription = fixture.fanout.subscribe_rooms(
            vec![fixture.route.cluster_id.clone()],
            Box::new(move |event, _| {
                event_tx.send(event.clone()).unwrap();
            }),
        );
        person
            .presence()
            .await
            .unwrap()
            .publish_arrived(&postcard::to_stdvec(&PresenceKey::pointer_default()).unwrap())
            .await
            .unwrap();
        assert!(matches!(event_rx.try_recv().unwrap(),
            CollabEvent::RemotePresenceUpdate { user_id, payload: PresencePayload::Arrived, .. }
                if user_id == fixture.principal));
    }

    #[test]
    fn certified_join_rejects_unknown_proof_signer_and_tampered_cert() {
        let root = SigningKey::from_bytes(&[3; 32]);
        let device = SigningKey::from_bytes(&[4; 32]);
        let stranger = SigningKey::from_bytes(&[5; 32]);
        let route = CrdtRoute::cluster("workspace-a");
        let mut presentation = certified_join("workspace-a", &root, &device);
        presentation.proof = stranger
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                "workspace-a",
            ))
            .to_bytes()
            .to_vec();
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err()
        );

        let mut presentation = certified_join("workspace-a", &root, &device);
        let mut cert: tilo_user_identity::DeviceCert =
            serde_json::from_slice(&presentation.device_certificate).expect("certificate");
        cert.device_label.push_str(" (tampered)");
        presentation.device_certificate = serde_json::to_vec(&cert).expect("certificate JSON");
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn certified_join_rejects_other_workspace_revoked_and_unenrolled_device() {
        let root = SigningKey::from_bytes(&[6; 32]);
        let device = SigningKey::from_bytes(&[7; 32]);
        let presentation = certified_join("workspace-b", &root, &device);
        let expected_root = root.verifying_key().to_bytes();
        assert!(
            verify_join_presentation(
                &CrdtRoute::cluster("workspace-a"),
                &presentation,
                &expected_root,
                2,
                false,
                true,
            )
            .is_err()
        );
        let route = CrdtRoute::cluster("workspace-b");
        let other_root = SigningKey::from_bytes(&[8; 32]);
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &other_root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err(),
            "a certificate selected for another workspace identity is rejected"
        );
        assert!(
            verify_join_presentation(&route, &presentation, &expected_root, 2, true, true).is_err()
        );
        assert!(
            verify_join_presentation(&route, &presentation, &expected_root, 2, false, false)
                .is_err()
        );
    }

    #[test]
    fn workspace_control_requires_editor_authority() {
        assert!(workspace_control_role_allowed(Role::Owner));
        assert!(workspace_control_role_allowed(Role::Editor));
        assert!(!workspace_control_role_allowed(Role::Commenter));
        assert!(!workspace_control_role_allowed(Role::Viewer));
    }

    #[cfg(test)]
    fn verified_device(root: &SigningKey) -> VerifiedJoinIdentity {
        VerifiedJoinIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [42; 32],
            capped: false,
        }
    }

    #[cfg(test)]
    fn downgrade_member_to_viewer(membership: &WorkspaceTreeHandle, principal: &str, room: &str) {
        let mut tree = membership.write();
        let workspace = &mut tree.workspaces[0];
        let _ = room;
        workspace.policy.owner = None;
        workspace.admit(principal, Role::Viewer);
        drop(tree);
    }

    #[test]
    fn certified_join_admission_uses_workspace_membership_and_role() {
        let root = SigningKey::from_bytes(&[20; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(1), "Member room");
        let route = CrdtRoute::cluster(workspace.room().to_string());
        workspace.admit(principal.clone(), crate::test_support::Role::Editor);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let admitted = admit_verified_join(&tree, &route, verified_device(&root))
            .expect("a certified editor is admitted");
        assert_eq!(admitted.principal_id, principal);
        assert_eq!(admitted.role, CapRole::Editor);
        assert!(
            verify_session_author(
                &admitted,
                collab_types::Ed25519PublicKey::from_bytes([42; 32]),
            )
            .is_ok(),
            "signed ops are device-authored under the certificate's device key"
        );
        assert!(
            verify_session_author(
                &admitted,
                collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
            )
            .is_err(),
            "the user root certifies the device but does not author this device's ops"
        );
        assert!(
            verify_session_author(
                &admitted,
                collab_types::Ed25519PublicKey::from_bytes([43; 32]),
            )
            .is_err(),
            "a member cannot submit another device's envelope through this session"
        );
    }

    #[test]
    fn a_foreign_device_is_refused_actions_above_the_workspace_cap() {
        use tilo_actions::ActionKind::{Outside, Undoable};
        let root = SigningKey::from_bytes(&[24; 32]);
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(4), "Actions");
        workspace.set_owner(principal_id_for_root(root.verifying_key().to_bytes()));
        let room = workspace.room().to_string();
        let route = CrdtRoute::cluster(room.clone());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let native = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
        let foreign = VerifiedJoinIdentity {
            capped: true,
            ..verified_device(&root)
        };
        let foreign = admit_verified_join(&tree, &route, foreign).unwrap();
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            None,
        );
        let authorize = |admitted: &AdmittedJoinIdentity, kind| {
            state.authorize_invocation_permission(
                &room,
                (
                    "so.tilo.settings",
                    "settings.workspace",
                    "settings.workspace",
                    "workspace.rename",
                ),
                &tilo_actions::ActionNeed::of(kind),
                &admitted.broker_actor(),
                0,
            )
        };

        for kind in [Outside, Undoable] {
            authorize(&native, kind).unwrap();
        }
        assert!(
            authorize(&foreign, Outside).is_err(),
            "reaching outside asks first on a device not theirs, and nobody answered"
        );
        authorize(&foreign, Undoable).unwrap();
    }

    /// Governs every room for one device; admits every certificate.
    #[cfg(test)]
    struct GovernedBy([u8; 32]);

    #[cfg(test)]
    impl crate::peer_actions::CertificateAuthority for GovernedBy {
        fn authorize(
            &self,
            _room: uuid::Uuid,
            _certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            Ok(())
        }
        fn governs(&self, _room: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
            device == Some(&self.0)
        }
    }

    #[test]
    fn a_foreign_device_is_capped_only_in_rooms_it_does_not_govern() {
        let root = SigningKey::from_bytes(&[25; 32]);
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(5), "Own");
        workspace.set_owner(principal_id_for_root(root.verifying_key().to_bytes()));
        let room = workspace.room().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.clone().shared()),
            None,
        );
        let governor = [7; 32];
        let authority: Arc<dyn crate::peer_actions::CertificateAuthority> =
            Arc::new(GovernedBy(governor));
        assert!(state.certificate_authority.set(authority).is_ok());
        let foreign_cert = |device| tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "mac".into(),
            capabilities: tilo_user_identity::DeviceCapabilities {
                foreign: true,
                ..tilo_user_identity::DeviceCapabilities::primary()
            },
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        let admin = |cert: &tilo_user_identity::DeviceCert| {
            let identity = VerifiedJoinIdentity::of(cert, state.capped_in(&room, cert));
            let admitted =
                admit_verified_join(&tree, &CrdtRoute::cluster(&room), identity).unwrap();
            let actor = state.certified_person(&room, cert);
            let admin = state.authorize_invocation_permission(
                &room,
                (
                    "so.tilo.settings",
                    "settings.workspace",
                    "settings.workspace",
                    "workspace.rename",
                ),
                &tilo_actions::ActionNeed::of(tilo_actions::ActionKind::Outside),
                &actor,
                0,
            );
            (admitted.role, admin.is_ok())
        };

        assert_eq!(admin(&foreign_cert(governor)), (CapRole::Owner, true));
        assert_eq!(admin(&foreign_cert([8; 32])), (CapRole::Owner, false));
    }

    #[test]
    fn a_foreign_device_joins_with_at_most_the_workspace_cap() {
        let root = SigningKey::from_bytes(&[23; 32]);
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(3), "Foreign");
        workspace.set_owner(principal_id_for_root(root.verifying_key().to_bytes()));
        workspace.admit_agent("scribe", crate::test_support::KindSet::READ);
        let route = CrdtRoute::cluster(workspace.room().to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let foreign = VerifiedJoinIdentity {
            capped: true,
            ..verified_device(&root)
        };

        let mut admitted = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
        assert_eq!(admitted.role, CapRole::Owner);
        admit_agent_delegation(&tree, &route, &mut admitted, "scribe").unwrap();

        let mut admitted = admit_verified_join(&tree, &route, foreign.clone()).unwrap();
        assert!(admit_agent_delegation(&tree, &route, &mut admitted, "scribe").is_err());
        let changes = |admitted: &AdmittedJoinIdentity, tree: &WorkspaceTree| {
            admitted
                .with_authority(Some(&tree.desktop), &route.cluster_id, |authority| {
                    authority
                        .decide(
                            &tree.workspaces[0].node().app("so.tilo.notes"),
                            ActionKind::Undoable,
                            &tilo_desktop_model::context_at(0),
                        )
                        .answer
                })
                .unwrap()
        };
        assert_eq!(changes(&admitted, &tree), Answer::Allow);

        tree.workspaces[0].deny(
            crate::test_support::Subject::ForeignDevice,
            &crate::test_support::TreePath::root(),
            crate::test_support::KindSet::UNDOABLE,
        );
        let admitted = admit_verified_join(&tree, &route, foreign).unwrap();
        assert_eq!(
            changes(&admitted, &tree),
            Answer::Deny,
            "the room bounds devices that are not their owner's further"
        );
    }

    #[test]
    fn agent_delegation_uses_agent_caps_and_is_revoked_live() {
        let root = SigningKey::from_bytes(&[44; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(44), "Agent room");
        workspace.set_owner(principal);
        let route = CrdtRoute::cluster(workspace.room().to_string());
        let agent_membership = workspace.admit_agent("scribe", crate::test_support::KindSet::READ);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let mut admitted = admit_verified_join(&tree, &route, verified_device(&root))
            .expect("the owner is admitted before delegation");
        admit_agent_delegation(&tree, &route, &mut admitted, "scribe")
            .expect("the owner may operate an admitted agent");
        assert_eq!(admitted.broker_actor().id, "agent:scribe");
        assert!(admitted.broker_actor().is_agent);
        let answer = |kind| {
            admitted
                .with_authority(Some(&tree.desktop), &route.cluster_id, |authority| {
                    authority
                        .decide(
                            &tree.workspaces[0].node().app("so.tilo.notes"),
                            kind,
                            &tilo_desktop_model::context_at(0),
                        )
                        .answer
                })
                .unwrap()
        };
        assert_eq!(answer(ActionKind::Read), Answer::Allow, "the agent reads");
        assert_eq!(
            answer(ActionKind::Undoable),
            Answer::Deny,
            "and does only what its own grant allows, whoever launched it"
        );

        let membership = tree.shared();
        assert!(membership.write().workspaces[0].revoke_grant(&agent_membership));
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership),
            None,
        );
        assert!(
            state.revalidate_admission(&route, &admitted).is_err(),
            "revoking agent membership invalidates an already-open agent channel"
        );
    }

    /// In a room this desktop hosts itself, a delegated agent takes part
    /// with exactly its preset, capped by its person's role, and revoking
    /// either the person or the agent cuts an open session off.
    #[test]
    fn a_hosted_agent_takes_its_preset_capped_by_its_person_and_ends_with_either() {
        use tilo_action_policy::roles::{AgentPreset, governance};
        let owner_root = SigningKey::from_bytes(&[45; 32]);
        let owner = principal_id_for_root(owner_root.verifying_key().to_bytes());
        let bob_root = SigningKey::from_bytes(&[46; 32]);
        let bob = principal_id_for_root(bob_root.verifying_key().to_bytes());
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(45), "Hosted");
        workspace.set_owner(owner.clone());
        let room = workspace.policy.room.clone();
        // Bob comments, and may launch the room's agents.
        workspace.admit(bob.clone(), Role::Commenter);
        workspace.grant(tilo_action_policy::Grant::new(
            "fixture:bob-launches-agents",
            crate::test_support::Subject::Person(bob.clone()),
            roles::governance_action(&room, governance::GRANT),
            crate::test_support::KindSet::OUTSIDE,
            Answer::Allow,
            crate::test_support::Subject::Person(owner.clone()),
        ));
        let granted_by = crate::test_support::Subject::Person(owner);
        let operator = workspace.admit_agent_preset("scribe", AgentPreset::Operator, &granted_by);
        workspace.admit_agent_preset("reader", AgentPreset::Observer, &granted_by);
        let route = CrdtRoute::cluster(workspace.room().to_string());
        let notes = workspace.node().app("so.tilo.notes");
        let set_role = roles::governance_action(&room, governance::SET_ROLE);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let delegate = |root: &SigningKey, agent: &str, tree: &WorkspaceTree| {
            let mut admitted = admit_verified_join(tree, &route, verified_device(root)).unwrap();
            admit_agent_delegation(tree, &route, &mut admitted, agent).map(|()| admitted)
        };
        let answer = |admitted: &AdmittedJoinIdentity,
                      tree: &WorkspaceTree,
                      node: &TreePath,
                      kind: ActionKind| {
            admitted
                .with_authority(Some(&tree.desktop), &route.cluster_id, |authority| {
                    authority
                        .decide(node, kind, &tilo_desktop_model::context_at(0))
                        .answer
                })
                .unwrap()
        };

        // The owner's Operator agent edits as an Editor and never governs.
        let scribe = delegate(&owner_root, "scribe", &tree).unwrap();
        assert_eq!(scribe.role, Role::Editor, "Operator, below its owner");
        assert!(workspace_control_role_allowed(scribe.role));
        assert_eq!(
            answer(&scribe, &tree, &notes, ActionKind::Undoable),
            Answer::Allow
        );
        assert_eq!(
            answer(&scribe, &tree, &set_role, roles::GOVERNANCE_KIND),
            Answer::Deny,
            "the owner's agent does not govern the room"
        );
        // The owner's Observer agent reads, and is no Editor anywhere.
        let reader = delegate(&owner_root, "reader", &tree).unwrap();
        assert_eq!(reader.role, Role::Viewer, "exactly its preset");
        assert!(!workspace_control_role_allowed(reader.role));
        assert_eq!(
            answer(&reader, &tree, &notes, ActionKind::Read),
            Answer::Allow
        );
        assert_eq!(
            answer(&reader, &tree, &notes, ActionKind::Undoable),
            Answer::Deny
        );
        // Bob's session with the same Operator agent holds Bob's role.
        let bobs = delegate(&bob_root, "scribe", &tree).unwrap();
        assert_eq!(bobs.role, Role::Commenter, "capped by its person's role");
        assert!(!workspace_control_role_allowed(bobs.role));
        assert_eq!(
            answer(&bobs, &tree, &notes, ActionKind::Undoable),
            Answer::Deny,
            "the agent changes nothing its person may not"
        );

        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.clone()),
            None,
        );
        assert_eq!(
            state.revalidate_admission(&route, &bobs).unwrap().role,
            Role::Commenter,
            "revalidation resolves the same role"
        );
        assert!(state.revalidate_admission(&route, &scribe).is_ok());

        // Removing the person ends their agent's session at once.
        assert!(membership.write().workspaces[0].dismiss(&bob));
        assert!(
            state.revalidate_admission(&route, &bobs).is_err(),
            "revoking the person cuts their agent off"
        );
        assert!(state.revalidate_admission(&route, &scribe).is_ok());

        // Revoking the agent's admission ends every session it runs.
        for id in &operator {
            assert!(membership.write().workspaces[0].revoke_grant(id));
        }
        assert!(
            state.revalidate_admission(&route, &scribe).is_err(),
            "revoking the agent cuts it off"
        );
        assert!(state.revalidate_admission(&route, &reader).is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_context_projects_canonical_agents_and_withdraws_exactly() {
        struct NativeRoom(String);
        impl crate::services::WorkspaceMemberships for NativeRoom {
            fn requires_room(&self, room: &str) -> bool {
                room == self.0
            }

            fn execute(
                &self,
                _: tilo_owner_commit::OwnerCommitRequest,
            ) -> tilo_owner_commit::CommitFuture {
                Box::pin(async { Err("roster fixture never commits owner state".into()) })
            }
        }

        let root = SigningKey::from_bytes(&[46; 32]);
        let person = principal_id_for_root(root.verifying_key().to_bytes());
        let tide = "tide";
        let reed = "reed";
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(46), "Roster");
        workspace.set_owner(person.clone());
        let room = workspace.room().to_string();
        let other_person = "other-person";
        workspace.admit(other_person, crate::test_support::Role::Viewer);
        workspace.policy.joined.insert(other_person.into(), 7_000);
        workspace.allow_in_room(
            crate::test_support::Subject::person("person-in-other-room"),
            &uuid::Uuid::new_v4().to_string(),
            crate::test_support::KindSet::READ,
        );
        let collision = workspace.admit_agent(&person, crate::test_support::KindSet::READ);
        workspace.admit_agent(tide, crate::test_support::KindSet::READ);
        workspace.allow_in_room(
            crate::test_support::Subject::agent("wrong-room"),
            &uuid::Uuid::new_v4().to_string(),
            crate::test_support::KindSet::READ,
        );
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let route = CrdtRoute::record("so.tilo.chat", &room, "main");
        let mut admitted = admit_verified_join(&tree, &route, verified_device(&root)).unwrap();
        admitted.display_handle = Some(tide.into());
        assert_eq!(CapnpBrokerState::participant_id(&admitted), person);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let observed = Arc::new(Mutex::new(Vec::<(
            String,
            Vec<ui::collaboration::Collaborator>,
        )>::new()));
        let observed_sink = Arc::clone(&observed);
        let _people = fanout.subscribe_certified_people(Arc::new(move |room, people| {
            observed_sink.lock().unwrap().push((room, people));
        }));
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.authority()),
            None,
            None,
            HostServices {
                workspace_memberships: Some(Arc::new(NativeRoom(room.clone()))),
                ..Default::default()
            },
        );
        fanout.attach_capnp(state.clone());

        let roster = state.native_membership_roster(&route, &admitted).unwrap();
        let legacy_route =
            CrdtRoute::record("so.tilo.chat", uuid::Uuid::new_v4().to_string(), "main");
        assert!(
            state
                .native_roster_for_subscriber(&legacy_route, &admitted)
                .unwrap()
                .is_none(),
            "a room the native owner did not select keeps ordinary app context"
        );
        assert_eq!(roster.iter().filter(|row| row.is_self).count(), 1);
        assert!(roster.iter().any(|row| row.id == person && !row.is_agent));
        assert!(roster.iter().all(|row| row.id != "person-in-other-room"));
        assert!(
            roster
                .iter()
                .any(|row| { row.id == other_person && !row.is_agent && !row.is_self })
        );
        // When each person joined travels with their row, so every replica
        // numbers namesakes alike; the owner holds the room from its start.
        let joined = |id: &str| roster.iter().find(|row| row.id == id).unwrap().joined_at;
        assert_eq!(joined(other_person), Some(7_000));
        assert_eq!(
            roster.iter().find(|row| row.is_self).unwrap().joined_at,
            Some(0)
        );
        assert_eq!(joined(&format!("agent:{tide}")), None);
        assert!(
            roster
                .iter()
                .any(|row| { row.id == format!("agent:{person}") && row.is_agent && !row.is_self })
        );
        assert!(
            roster
                .iter()
                .any(|row| row.id == format!("agent:{tide}") && row.is_agent)
        );
        assert!(
            roster.iter().all(|row| row.id != "agent:wrong-room"),
            "a grant for another room is not native membership here"
        );
        assert!(
            state
                .native_membership_roster(
                    &CrdtRoute::record("so.tilo.chat", uuid::Uuid::new_v4().to_string(), "main",),
                    &admitted,
                )
                .is_err()
        );

        let channel = route.channel_key();
        let native = state
            .native_roster_for_subscriber(&route, &admitted)
            .unwrap()
            .expect("the actual owner selects this native room");
        assert!(state.host_workers.lock().unwrap().is_empty());
        let mut parallel = state.subscribe_context(&channel, 46, Some(native.clone()));
        assert!(matches!(
            parallel.try_recv(),
            Ok(ContextEnvelope::Snapshot { .. })
        ));
        let mut stream = state.subscribe_context(&channel, 46, Some(native.clone()));
        let mut ordinary = state.subscribe_context(&channel, 47, None);
        assert!(matches!(
            stream.try_recv(),
            Ok(ContextEnvelope::Snapshot { .. })
        ));
        assert_eq!(
            state.certified_agent_identity(&room, &format!("agent:{tide}")),
            Some(ui::collaboration::CertifiedAgentIdentity {
                participant_id: format!("agent:{tide}"),
                agent_key: tide.into(),
            }),
        );
        assert!(state.certified_agent_identity(&room, &person).is_none());
        let forged = ui::collaboration::Collaborator {
            id: "agent:forged".into(),
            name: "forged".into(),
            colour: None,
            is_agent: true,
            is_self: false,
            joined_at: None,
        };
        {
            let mut channels = state.channels.lock().unwrap();
            CapnpBrokerState::fanout_context(
                channels.values_mut(),
                u64::MAX,
                std::slice::from_ref(&forged),
                &[],
            );
        }
        assert!(
            stream.try_recv().is_err(),
            "app context must not replace an authenticated native roster"
        );
        assert!(
            parallel.try_recv().is_err(),
            "app context must not replace a parallel authenticated native roster"
        );
        let ContextEnvelope::Snapshot { collaborators, .. } = ordinary.try_recv().unwrap() else {
            panic!("ordinary context subscribers still receive app context");
        };
        assert_eq!(collaborators.len(), 1);
        assert_eq!(collaborators[0].id, forged.id);
        assert_eq!(collaborators[0].name, forged.name);
        assert!(collaborators[0].is_agent);
        assert!(!collaborators[0].is_self);
        assert!(
            observed
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .iter()
                .all(|row| row.id != forged.id),
            "ordinary Context cannot enter the certified People observer"
        );
        assert!(membership.write().workspaces[0].revoke_grant(&collision));
        assert!(membership.write().workspaces[0].dismiss(other_person));
        state.refresh_workspace_context_services().unwrap();
        let ContextEnvelope::Snapshot { collaborators, .. } = stream.try_recv().unwrap() else {
            panic!("membership refresh must be a complete snapshot");
        };
        assert!(
            collaborators
                .iter()
                .all(|row| row.id != format!("agent:{person}"))
        );
        assert!(
            observed
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .iter()
                .all(|row| row.id != format!("agent:{person}")),
            "membership replacement retires the certified agent from shell projection"
        );
        assert!(collaborators.iter().all(|row| row.id != other_person));
        assert!(
            collaborators
                .iter()
                .any(|row| row.id == person && row.is_self)
        );
        let ContextEnvelope::Snapshot { collaborators, .. } = parallel.try_recv().unwrap() else {
            panic!("every mounted root must receive membership removal");
        };
        assert!(
            collaborators
                .iter()
                .all(|row| row.id != format!("agent:{person}"))
        );
        membership.write().workspaces[0].admit_agent(reed, crate::test_support::KindSet::READ);
        state.refresh_workspace_context_services().unwrap();
        let ContextEnvelope::Snapshot { collaborators, .. } = stream.try_recv().unwrap() else {
            panic!("membership addition must be a complete snapshot");
        };
        assert!(
            collaborators
                .iter()
                .any(|row| row.id == format!("agent:{reed}") && row.is_agent)
        );
        let ContextEnvelope::Snapshot { collaborators, .. } = parallel.try_recv().unwrap() else {
            panic!("every mounted root must receive membership addition");
        };
        assert!(
            collaborators
                .iter()
                .any(|row| row.id == "agent:reed" && row.is_agent)
        );
        assert!(
            observed
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .iter()
                .any(|row| row.id == "agent:reed" && row.is_agent),
            "certified kind comes from the owner row, not an id convention"
        );

        let mut queued = state.subscribe_context(&channel, 48, Some(native.clone()));
        assert!(matches!(
            queued.try_recv(),
            Ok(ContextEnvelope::Snapshot { .. })
        ));
        let queued = Rc::new(AsyncMutex::new(queued));
        let client: context_stream::Client = capnp_rpc::new_client(ContextStreamServer {
            host_guard: None,
            native_guard: Some(NativeContextReadGuard {
                state: state.clone(),
                route: route.clone(),
                identity: native.admitted,
                document: None,
            }),
            rx: Rc::clone(&queued),
        });
        let mut pending = Box::pin(client.read_request().send().promise);
        // The actual stream read must be waiting before revocation: it holds
        // the queue while it waits for the next envelope.
        tilo_test_env::within("the stream read to wait for an envelope", async {
            while queued.try_lock().is_ok() {
                assert!(futures::poll!(&mut pending).is_pending());
                tokio::task::yield_now().await;
            }
        })
        .await;
        membership.write().workspaces[0].policy.owner = None;
        state.refresh_workspace_context_services().unwrap();
        assert!(
            observed.lock().unwrap().last().unwrap().1.is_empty(),
            "losing the certified source withdraws the complete shell roster"
        );
        let Err(error) = pending.await else {
            panic!("revocation must terminate an already waiting native read");
        };
        assert!(error.to_string().contains("context stream fanout closed"));
        assert!(
            state
                .certified_agent_identity(&room, &format!("agent:{tide}"))
                .is_none(),
            "an empty certified roster immediately revokes marker activation identity"
        );
        assert!(
            state
                .certified_agent_identity(&room, "agent:forged")
                .is_none()
        );
    }

    #[test]
    fn non_owner_cannot_claim_an_agent_principal() {
        let member = SigningKey::from_bytes(&[45; 32]);
        let member_id = principal_id_for_root(member.verifying_key().to_bytes());
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(45),
            "Delegation room",
        );
        let route = CrdtRoute::cluster(workspace.room().to_string());
        workspace.admit(member_id, crate::test_support::Role::Editor);
        workspace.admit_agent("operator", crate::test_support::KindSet::READ);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let mut admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("the person is a member");
        assert!(admit_agent_delegation(&tree, &route, &mut admitted, "operator").is_err());
    }

    #[test]
    fn certified_join_admission_rejects_wrong_or_removed_root() {
        let member = SigningKey::from_bytes(&[21; 32]);
        let stranger = SigningKey::from_bytes(&[22; 32]);
        let member_id = principal_id_for_root(member.verifying_key().to_bytes());
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(2),
            "Revocation room",
        );
        let route = CrdtRoute::cluster(workspace.room().to_string());
        workspace.admit(member_id.clone(), crate::test_support::Role::Viewer);
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        assert!(admit_verified_join(&tree, &route, verified_device(&stranger)).is_err());
        let admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("member starts admitted");
        let membership_tree = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership_tree.clone()),
            None,
        );
        assert!(membership_tree.write().workspaces[0].dismiss(&member_id));
        assert!(
            state.revalidate_admission(&route, &admitted).is_err(),
            "removing membership rejects the certified session's next operation"
        );
    }

    #[test]
    fn certified_join_admission_rejects_cross_workspace_replay() {
        let root = SigningKey::from_bytes(&[23; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut member_workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(3), "Member room");
        member_workspace.set_owner(&principal);
        let other_workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(4), "Other room");
        let replay_route = CrdtRoute::cluster(other_workspace.room().to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(member_workspace);
        tree.workspaces.push(other_workspace);

        assert!(admit_verified_join(&tree, &replay_route, verified_device(&root)).is_err());
    }

    #[cfg(test)]
    fn assert_replay_and_forgery_are_audited(
        state: &Arc<CapnpBrokerState>,
        broker_channels: &Arc<Mutex<BrokerChannels>>,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
        op: &SignedOp,
    ) {
        let artifact = op.artifact;
        let accepted_before = broker_channels
            .lock()
            .expect("broker channels")
            .state_for(artifact)
            .scrollback_for(artifact, u64::MAX, 10)
            .0;
        handle_signed_op_send(state, 1, route, admitted, op.clone());
        let mut forged = op.clone();
        forged.seq = 2;
        forged.payload = DocumentMutation::Incremental(b"tampered".to_vec())
            .encode()
            .expect("typed incremental");
        handle_signed_op_send(state, 1, route, admitted, forged);

        let audit = state
            .broker_channels
            .lock()
            .expect("broker channels")
            .audit
            .for_artifact(artifact);
        assert_eq!(audit.len(), 2, "replay and forged op are both audited");
        assert!(
            audit
                .iter()
                .any(|entry| matches!(entry.rejection, Some(ValidationError::ReplaySeq { .. })))
        );
        assert!(
            audit
                .iter()
                .any(|entry| matches!(entry.rejection, Some(ValidationError::BadOpSignature(_))))
        );
        assert_eq!(
            broker_channels
                .lock()
                .expect("broker channels")
                .state_for(artifact)
                .scrollback_for(artifact, u64::MAX, 10)
                .0,
            accepted_before,
            "replay and forgery must not change the accepted scrollback",
        );
    }

    #[cfg(test)]
    fn assert_published_snapshot(sink: &RecordingSink, route: &CrdtRoute) {
        let published = sink.snapshot();
        let [
            Sent::Signed {
                app_id,
                cluster_id,
                record_id,
                payload,
            },
        ] = published.as_slice()
        else {
            panic!("the certified document was not published to the peer network");
        };
        assert_eq!(app_id, "so.tilo.notes");
        assert_eq!(cluster_id, &route.cluster_id);
        assert_eq!(record_id, "meeting-notes");
        assert_eq!(
            DocumentMutation::decode(payload).expect("typed network payload"),
            DocumentMutation::Snapshot(b"automerge snapshot".to_vec()),
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the typed document publication test retains its full authorization and payload assertions as one scenario"
    )]
    #[test]
    fn certified_document_send_uses_live_route_role_and_typed_snapshot() {
        let root = SigningKey::from_bytes(&[24; 32]);
        let device = SigningKey::from_bytes(&[25; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(5),
            "Typed document room",
        );
        workspace.set_owner(&principal);
        let route = CrdtRoute::record(
            "so.tilo.notes",
            workspace.room().to_string(),
            "meeting-notes",
        );
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let admitted = admit_verified_join(
            &membership.read(),
            &route,
            VerifiedJoinIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                capped: false,
            },
        )
        .expect("owner is admitted");

        let fanout = Arc::new(FanoutState::new());
        let broker_channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let sink = Arc::new(RecordingSink::default());
        let publisher =
            PeerPublisher::new(sink.clone() as Arc<dyn crate::collab_outbound::PeerPublishSink>);
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::clone(&broker_channels),
            publisher,
            Some(membership.clone()),
            None,
        );
        fanout.attach_capnp(Arc::clone(&state));
        state.ensure_channel(&route.channel_key());
        let mut received = state.subscribe_ops(&route.channel_key(), 2);

        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"automerge snapshot".to_vec())
                .encode()
                .expect("typed snapshot"),
            artifact,
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            collab_types::OpKind::Change,
            SystemTime::now(),
            1,
        );
        sign_op(&mut op, &device).expect("device signs document op");
        let authorized = state
            .authorize_certified_document(&route, &admitted, &op)
            .expect("live owner may edit the joined document");
        handle_authorized_signed_op_send(&state, 1, &route, &admitted, &authorized, op.clone());

        let delivered = received.try_recv().expect("snapshot fanout");
        assert_eq!(delivered.kind, OpKind::Signed);
        assert_eq!(
            postcard::from_bytes::<SignedOp>(&delivered.op_bytes).expect("signed envelope"),
            op
        );
        assert_eq!(
            broker_channels
                .lock()
                .expect("broker channels")
                .state_for(artifact)
                .op_log
                .len(),
            1,
            "the admitted validator records the certified op without a role attestation"
        );
        assert_published_snapshot(&sink, &route);

        let expected_channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id);
        let mut encrypted = SignedOp::new_unsigned_with_encryption(
            b"opaque MLS application frame".to_vec(),
            artifact,
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            collab_types::OpKind::Change,
            SystemTime::now(),
            2,
            collab_types::EncryptionMetadata::mls_application(7, expected_channel),
        );
        sign_op(&mut encrypted, &device).expect("device signs opaque MLS envelope");
        let authorized = state
            .authorize_certified_document(&route, &admitted, &encrypted)
            .expect("broker authorizes signed clear metadata without decrypting");
        assert!(matches!(
            authorized.payload,
            collab_types::RoutedDocumentPayload::Encrypted { channel, epoch: 7 }
                if channel == expected_channel
        ));
        handle_signed_op_send(&state, 1, &route, &admitted, encrypted.clone());
        let delivered = received.try_recv().expect("opaque MLS fanout");
        assert_eq!(
            postcard::from_bytes::<SignedOp>(&delivered.op_bytes).expect("opaque envelope"),
            encrypted,
            "broker must relay ciphertext byte-for-byte"
        );

        let mut wrong_channel = encrypted.clone();
        wrong_channel.encryption.as_mut().expect("metadata").channel =
            collab_types::ChannelId::new();
        sign_op(&mut wrong_channel, &device).expect("re-sign adversarial envelope");
        assert!(
            state
                .authorize_certified_document(&route, &admitted, &wrong_channel)
                .is_err(),
            "validly signed ciphertext for another MLS channel must not be routed"
        );

        assert_replay_and_forgery_are_audited(&state, &broker_channels, &route, &admitted, &op);

        downgrade_member_to_viewer(&membership, &principal, &route.cluster_id);
        let viewer = state
            .revalidate_admission(&route, &admitted)
            .expect("member still admitted");
        assert_eq!(viewer.role, Role::Viewer);
        assert!(
            state
                .authorize_certified_document(&route, &viewer, &op)
                .is_err(),
            "a live downgrade must take effect before the next send"
        );
        let other_record =
            CrdtRoute::record(route.app_id.clone(), route.cluster_id, "other-record");
        assert!(
            state
                .authorize_certified_document(&other_record, &admitted, &op)
                .is_err(),
            "a signed envelope cannot be replayed through another record route"
        );
    }

    /// The person room is admitted from the joiner's own root alone, with no
    /// workspace in the tree, and its documents reach the other local
    /// connections on the record and the peer publisher. Nobody else's root
    /// enters it, and nobody enters another person's room.
    #[test]
    fn person_room_admits_only_its_own_roots_devices_as_owner() {
        let root = SigningKey::from_bytes(&[26; 32]);
        let device = SigningKey::from_bytes(&[27; 32]);
        let stranger = SigningKey::from_bytes(&[28; 32]);
        let own_room = collab_types::person_room(&root.verifying_key().to_bytes());
        let route = CrdtRoute::record("so.tilo.tube", own_room.to_string(), "watch-history");
        let membership = WorkspaceTree::default().shared();
        let own_device = VerifiedJoinIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            capped: false,
        };

        let admitted = admit_verified_join(&membership.read(), &route, own_device.clone())
            .expect("the root's own device joins its person room");
        assert_eq!(admitted.role, CapRole::Owner);
        assert_eq!(
            admitted.principal_id,
            principal_id_for_root(root.verifying_key().to_bytes())
        );
        assert!(
            admit_verified_join(&membership.read(), &route, verified_device(&stranger)).is_err(),
            "another root's device never joins this person's room"
        );
        let foreign = admit_verified_join(
            &membership.read(),
            &route,
            VerifiedJoinIdentity {
                capped: true,
                ..own_device
            },
        )
        .expect("the root's foreign device joins its person room");
        let governs = foreign
            .with_authority(None, &route.cluster_id, |authority| {
                authority
                    .decide(
                        &roles::governance_action(&route.cluster_id, roles::governance::INVITE),
                        roles::GOVERNANCE_KIND,
                        &tilo_desktop_model::context_at(0),
                    )
                    .answer
            })
            .unwrap();
        assert_eq!(
            governs,
            Answer::Deny,
            "a foreign device keeps the foreign-device bound in the person room too"
        );
        let strangers_room = CrdtRoute::record(
            "so.tilo.tube",
            collab_types::person_room(&stranger.verifying_key().to_bytes()).to_string(),
            "watch-history",
        );
        assert!(
            admit_verified_join(&membership.read(), &strangers_room, own_device).is_err(),
            "a device never joins another person's room"
        );

        let fanout = Arc::new(FanoutState::new());
        let broker_channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let sink = Arc::new(RecordingSink::default());
        let publisher =
            PeerPublisher::new(sink.clone() as Arc<dyn crate::collab_outbound::PeerPublishSink>);
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::clone(&broker_channels),
            publisher,
            Some(membership),
            None,
        );
        fanout.attach_capnp(Arc::clone(&state));
        state.ensure_channel(&route.channel_key());
        let mut other_window = state.subscribe_ops(&route.channel_key(), 2);
        let current = state
            .revalidate_admission(&route, &admitted)
            .expect("the person room stays admitted without a workspace");
        assert_eq!(current.role, CapRole::Owner);

        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"watched".to_vec())
                .encode()
                .expect("typed snapshot"),
            document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            collab_types::OpKind::Change,
            SystemTime::now(),
            1,
        );
        sign_op(&mut op, &device).expect("device signs document op");
        let authorized = state
            .authorize_certified_document(&route, &current, &op)
            .expect("the owner writes their own records");
        handle_authorized_signed_op_send(&state, 1, &route, &current, &authorized, op.clone());

        let delivered = other_window.try_recv().expect("local fanout");
        assert_eq!(
            postcard::from_bytes::<SignedOp>(&delivered.op_bytes).expect("signed envelope"),
            op
        );
        assert!(matches!(
            sink.snapshot().as_slice(),
            [Sent::Signed { cluster_id, record_id, .. }]
                if *cluster_id == route.cluster_id && record_id == "watch-history"
        ));
    }

    /// A device that came back is caught up by the local apps holding the
    /// person room's records: each record channel of that room, and only of
    /// that room, is asked for its snapshot.
    #[test]
    fn room_snapshot_requests_reach_only_that_rooms_record_channels() {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        fanout.attach_capnp(Arc::clone(&state));
        let person_room = collab_types::person_room(&[29; 32]).to_string();
        let history = CrdtRoute::record("so.tilo.tube", person_room.as_str(), "watch-history");
        let words = CrdtRoute::record("so.tilo.dict", person_room.as_str(), "learned");
        let unscoped = CrdtRoute::cluster(person_room.as_str());
        let shared = CrdtRoute::record("so.tilo.tube", "room-1", "watch-history");
        let mut receivers: Vec<_> = [&history, &words, &unscoped, &shared]
            .into_iter()
            .zip(2_u64..)
            .map(|(route, peer)| {
                state.ensure_channel(&route.channel_key());
                state.subscribe_context(&route.channel_key(), peer, None)
            })
            .collect();

        assert_eq!(fanout.request_room_snapshots(&person_room), 2);
        let asked: Vec<bool> = receivers
            .iter_mut()
            .map(|rx| matches!(rx.try_recv(), Ok(ContextEnvelope::SnapshotRequested)))
            .collect();
        assert_eq!(asked, [true, true, false, false]);
    }

    #[test]
    fn record_scoped_document_fanout_reaches_only_its_targeted_subscriber() {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        fanout.attach_capnp(Arc::clone(&state));

        let target = CrdtRoute::record("so.tilo.notes", "room-1", "note-a");
        let other_record = CrdtRoute::record("so.tilo.notes", "room-1", "note-b");
        let other_app = CrdtRoute::record("so.tilo.files", "room-1", "note-a");
        let other_room = CrdtRoute::record("so.tilo.notes", "room-2", "note-a");
        let mut target_rx = state.subscribe_ops(&target.channel_key(), 2);
        let mut other_record_rx = state.subscribe_ops(&other_record.channel_key(), 3);
        let mut other_app_rx = state.subscribe_ops(&other_app.channel_key(), 4);
        let mut other_room_rx = state.subscribe_ops(&other_room.channel_key(), 5);

        let (_signing, author) = collab_types::generate_keypair();
        let envelope = SignedOp::new_unsigned(
            DocumentMutation::Incremental(vec![7, 9])
                .encode()
                .expect("typed incremental mutation"),
            document_artifact_id(&target.app_id, &target.cluster_id, &target.record_id),
            author,
            document_attestation_id(&target.app_id, &target.cluster_id, &target.record_id),
            collab_types::OpKind::Change,
            SystemTime::now(),
            1,
        );
        state.fanout_to_route(
            &CollabEvent::Document {
                envelope: Box::new(envelope),
                from_user_id: "peer@42".into(),
                decrypted_payload: None,
            },
            1,
            &target,
        );

        assert!(
            target_rx.try_recv().is_ok(),
            "the addressed record receives the op"
        );
        for (label, received) in [
            ("another record", other_record_rx.try_recv()),
            ("another app", other_app_rx.try_recv()),
            ("another room", other_room_rx.try_recv()),
        ] {
            assert!(
                matches!(received, Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
                "{label} must not receive a record-directed document",
            );
        }
    }

    #[cfg(test)]
    #[derive(Clone)]
    struct CountingCatalogueAuthority {
        tree: Arc<Mutex<tilo_desktop_model::DesktopTree>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        failed: Arc<std::sync::atomic::AtomicBool>,
    }

    #[cfg(test)]
    impl crate::services::WorkspaceAuthority for CountingCatalogueAuthority {
        /// Every sample (`with_tree`, and `with_room`/`snapshot` through it)
        /// counts once.
        fn with_tree(
            &self,
            read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
        ) -> Result<(), String> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.failed.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("catalogue authority unavailable".into());
            }
            let tree = self
                .tree
                .lock()
                .map_err(|_| "catalogue authority unavailable".to_string())?;
            read(&tree);
            Ok(())
        }
    }

    #[cfg(test)]
    fn catalogue_surface(peer_id: u64, cluster: &str, resource: &str) -> PublishedSurface {
        PublishedSurface {
            peer_id,
            app_id: "so.tilo.files".into(),
            route: CrdtRoute::record("so.tilo.files", cluster, format!("record-{peer_id}")),
            actions: vec![PublishedAction {
                semantic_identity: None,
                resource: resource.into(),
                action: "open".into(),
                resource_kind: "files.entry".into(),
                resource_label: resource.into(),
                action_label: "Open".into(),
                description: None,
                operation_kind: tilo_actions::OperationKind::View,
                params: Vec::new(),
                contract: tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Undoable,
                    ..Default::default()
                },
                view: None,
                input: PublishedInput::None,
            }],
            live: true,
        }
    }

    #[cfg(test)]
    struct CountingCatalogueFixture {
        state: Arc<CapnpBrokerState>,
        tree: Arc<Mutex<tilo_desktop_model::DesktopTree>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        failed: Arc<std::sync::atomic::AtomicBool>,
        cluster: String,
    }

    #[cfg(test)]
    fn counting_catalogue_state(owner: &str) -> CountingCatalogueFixture {
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(29),
            "Catalogue room",
        );
        workspace.set_owner(owner);
        let cluster = workspace.room().to_string();
        let mut tree = tilo_desktop_model::DesktopTree::default();
        tree.workspaces.push(workspace);
        let tree = Arc::new(Mutex::new(tree));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(Arc::new(CountingCatalogueAuthority {
                tree: tree.clone(),
                calls: calls.clone(),
                failed: failed.clone(),
            })),
            None,
            None,
            HostServices::default(),
        );
        CountingCatalogueFixture {
            state,
            tree,
            calls,
            failed,
            cluster,
        }
    }

    #[cfg(test)]
    fn catalogue_test_channel() -> (
        watch::Sender<ActionCatalogueSnapshot>,
        watch::Receiver<ActionCatalogueSnapshot>,
    ) {
        watch::channel(Some(Arc::<str>::from("{}")))
    }

    #[cfg(test)]
    fn current_catalogue(
        receiver: &mut watch::Receiver<ActionCatalogueSnapshot>,
    ) -> ActionCatalogueSnapshot {
        receiver.borrow_and_update().clone()
    }

    #[test]
    fn catalogue_publication_computes_once_per_exact_audience_and_keeps_only_latest_state() {
        let CountingCatalogueFixture {
            state,
            calls,
            cluster,
            ..
        } = counting_catalogue_state("owner");
        let actor = BrokerActor::person("owner");
        let (first_tx, mut first_rx) = catalogue_test_channel();
        let (second_tx, mut second_rx) = catalogue_test_channel();
        state.action_catalogue_subscribers.lock().unwrap().extend([
            (1, (cluster.clone(), actor.clone(), first_tx)),
            (2, (cluster.clone(), actor, second_tx)),
        ]);

        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:first"));
        state.publish_action_catalogue();
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(12, catalogue_surface(12, &cluster, "files.entry:second"));
        state.publish_action_catalogue();

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "each synchronous publication computes one snapshot for a shared exact audience"
        );
        for receiver in [&mut first_rx, &mut second_rx] {
            let latest: serde_json::Value =
                serde_json::from_str(&current_catalogue(receiver).unwrap()).unwrap();
            assert_eq!(latest["surfaces"].as_array().unwrap().len(), 2);
            assert!(
                !receiver.has_changed().unwrap(),
                "a slow reader retains no obsolete complete catalogue"
            );
        }
    }

    #[test]
    fn catalogue_publication_separates_person_and_agent_with_the_same_textual_id() {
        let CountingCatalogueFixture {
            state,
            tree,
            calls,
            cluster,
            ..
        } = counting_catalogue_state("same");
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        let mut other = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(30),
            "Other catalogue room",
        );
        other.set_owner("same");
        let other_cluster = other.room().to_string();
        tree.lock().unwrap().workspaces.push(other);
        let (person_tx, mut person_rx) = catalogue_test_channel();
        let (agent_tx, mut agent_rx) = catalogue_test_channel();
        let (other_tx, mut other_rx) = catalogue_test_channel();
        state.action_catalogue_subscribers.lock().unwrap().extend([
            (1, (cluster.clone(), BrokerActor::person("same"), person_tx)),
            (2, (cluster, BrokerActor::agent("same"), agent_tx)),
            (3, (other_cluster, BrokerActor::person("same"), other_tx)),
        ]);

        state.publish_action_catalogue();

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 3);
        let person = current_catalogue(&mut person_rx).unwrap();
        let agent = current_catalogue(&mut agent_rx).unwrap();
        let other = current_catalogue(&mut other_rx).unwrap();
        assert!(person.contains("files.entry:owner"));
        assert!(!agent.contains("files.entry:owner"));
        assert!(!other.contains("files.entry:owner"));
    }

    #[test]
    fn catalogue_publication_rechecks_changed_authority_on_the_next_turn() {
        let CountingCatalogueFixture {
            state,
            tree,
            calls,
            cluster,
            ..
        } = counting_catalogue_state("owner");
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        let (tx, mut rx) = catalogue_test_channel();
        state
            .action_catalogue_subscribers
            .lock()
            .unwrap()
            .insert(1, (cluster, BrokerActor::person("owner"), tx));
        state.publish_action_catalogue();
        assert!(
            current_catalogue(&mut rx)
                .unwrap()
                .contains("files.entry:owner")
        );

        {
            let mut tree = tree.lock().unwrap();
            tree.workspaces[0].set_owner("replacement");
            tree.workspaces[0].dismiss("owner");
        }
        state.publish_action_catalogue();
        assert!(
            !current_catalogue(&mut rx)
                .unwrap()
                .contains("files.entry:owner")
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn catalogue_stream_starts_with_latest_state_and_rejects_invalidated_pending_state() {
        let (initial_tx, initial_rx) = watch::channel(Some(Arc::<str>::from("initial")));
        let mut initial_reader = ActionCatalogueReader {
            rx: initial_rx,
            initial: true,
        };
        assert_eq!(&*initial_reader.next().await.unwrap(), "initial");
        drop(initial_tx);
        assert!(
            initial_reader.next().await.is_err(),
            "a closed stream with no pending state disconnects"
        );

        let (tx, rx) = watch::channel(Some(Arc::<str>::from("initial")));
        tx.send(Some(Arc::<str>::from("newer"))).unwrap();
        let mut reader = ActionCatalogueReader { rx, initial: true };
        assert_eq!(&*reader.next().await.unwrap(), "newer");

        tx.send(Some(Arc::<str>::from("unread"))).unwrap();
        tx.send_replace(None);
        drop(tx);
        assert!(
            reader.next().await.is_err(),
            "projection failure invalidates an unread stale snapshot"
        );
    }

    #[test]
    fn catalogue_projection_failure_invalidates_and_removes_its_audience() {
        let CountingCatalogueFixture {
            state,
            calls,
            failed,
            cluster,
            ..
        } = counting_catalogue_state("owner");
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        let (tx, mut rx) = catalogue_test_channel();
        let (second_tx, mut second_rx) = catalogue_test_channel();
        let (closed_tx, closed_rx) = catalogue_test_channel();
        drop(closed_rx);
        state
            .action_catalogue_subscribers
            .lock()
            .unwrap()
            .insert(1, (cluster.clone(), BrokerActor::person("owner"), tx));
        state.action_catalogue_subscribers.lock().unwrap().insert(
            2,
            (cluster.clone(), BrokerActor::person("owner"), second_tx),
        );
        state
            .action_catalogue_subscribers
            .lock()
            .unwrap()
            .insert(3, (cluster, BrokerActor::person("owner"), closed_tx));
        state.publish_action_catalogue();
        assert!(
            current_catalogue(&mut rx)
                .unwrap()
                .contains("files.entry:owner"),
            "the current authorized catalogue is initially available"
        );
        assert!(
            current_catalogue(&mut second_rx)
                .unwrap()
                .contains("files.entry:owner")
        );
        assert!(
            !state
                .action_catalogue_subscribers
                .lock()
                .unwrap()
                .contains_key(&3),
            "closed catalogue subscribers are removed"
        );

        failed.store(true, std::sync::atomic::Ordering::Relaxed);
        state.publish_action_catalogue();
        assert!(rx.borrow_and_update().is_none());
        assert!(second_rx.borrow_and_update().is_none());
        let subscribers = state.action_catalogue_subscribers.lock().unwrap();
        assert!(!subscribers.contains_key(&1));
        assert!(!subscribers.contains_key(&2));
        assert!(!subscribers.contains_key(&3));
        drop(subscribers);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn exact_published_action_uses_current_live_routes_before_archived_descriptors() {
        let broker = CapnpBrokerState::test_broker_with_idle_directory_queue();
        let state = broker.state();
        let retained = catalogue_surface(7, "room", "notebook");
        state.launchable.retain(&retained);
        let route = retained.route.clone();
        let (archived, descriptor) = state.published_action(&route, "notebook", "open").unwrap();
        assert!(!archived.live);
        assert!(
            archived.actions.is_empty(),
            "lookup must not clone the source inventory"
        );
        assert_eq!(descriptor.resource, "notebook");

        let mut empty = retained.clone();
        empty.actions.clear();
        state.replace_published_surfaces_for_test(vec![empty.clone()]);
        assert!(state.published_action(&route, "notebook", "open").is_none());

        let mut other_peer = retained;
        other_peer.peer_id = 8;
        other_peer.actions[0].action_label = "Current live action".into();
        state.replace_published_surfaces_for_test(vec![empty, other_peer]);
        let (live, descriptor) = state.published_action(&route, "notebook", "open").unwrap();
        assert!(live.live);
        assert_eq!(live.peer_id, 8);
        assert!(live.actions.is_empty());
        assert_eq!(descriptor.action_label, "Current live action");
        let foreign = CrdtRoute::record(&route.app_id, "another-room", &route.record_id);
        assert!(
            state
                .published_action(&foreign, "notebook", "open")
                .is_none()
        );
        state.replace_published_surfaces_for_test(Vec::new());
        assert!(
            !state
                .published_action(&route, "notebook", "open")
                .unwrap()
                .0
                .live
        );
    }

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::unchecked_time_subtraction,
        reason = "the directed-action test keeps capability setup, delivery, and denial assertions in one scenario"
    )]
    #[test]
    fn exact_catalogue_scope_avoids_unrelated_oversized_descriptors() {
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(30),
            "Scoped action room",
        );
        workspace.set_owner("owner");
        let cluster = workspace.room().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        struct Launcher;
        impl crate::services::LauncherServices for Launcher {
            fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
                vec![crate::services::LaunchableApplication {
                    capabilities: Vec::new(),
                    package_id: "so.tilo.word-processor".into(),
                    display_name: "Writer".into(),
                    unconfined: false,
                    offers: Vec::new(),
                }]
            }
            fn launch(
                &self,
                _: &str,
                _: &tilo_desktop_model::Workspace,
                _: Option<&str>,
                _: &crate::services::LaunchPlace,
            ) -> Result<(), String> {
                unreachable!("catalogue projection never launches")
            }
            fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
                false
            }
        }
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.authority()),
            None,
            None,
            crate::services::HostServices {
                launcher: Some(Arc::new(Launcher)),
                ..Default::default()
            },
        );
        let action =
            |resource: String, action: &str, description: Option<String>| PublishedAction {
                semantic_identity: None,
                resource,
                action: action.into(),
                resource_kind: "document".into(),
                resource_label: "Document".into(),
                action_label: "Command".into(),
                description,
                operation_kind: tilo_actions::OperationKind::Effect,
                params: Vec::new(),
                contract: tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Undoable,
                    ..Default::default()
                },
                view: None,
                input: PublishedInput::None,
            };
        let desktop = PublishedSurface {
            peer_id: 1,
            app_id: tilo_owner_commit::desktop_command::APP_ID.into(),
            route: CrdtRoute::record(tilo_owner_commit::desktop_command::APP_ID, &cluster, ""),
            actions: vec![action(
                tilo_owner_commit::desktop_command::RESOURCE.into(),
                tilo_owner_commit::desktop_command::ACTION,
                None,
            )],
            live: true,
        };
        let mut surfaces = state.surfaces.lock().unwrap();
        surfaces.insert(desktop.peer_id, desktop);
        for index in 0..24u64 {
            surfaces.insert(
                index + 2,
                PublishedSurface {
                    peer_id: index + 2,
                    app_id: "so.tilo.word-processor".into(),
                    route: CrdtRoute::record(
                        "so.tilo.word-processor",
                        &cluster,
                        format!("record-{index}"),
                    ),
                    actions: vec![action(
                        format!("document:{index}"),
                        "edit",
                        Some("x".repeat(16 * 1024)),
                    )],
                    live: true,
                },
            );
        }
        drop(surfaces);
        let actor = BrokerActor::person("owner");
        let full: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &actor, None)
                .unwrap(),
        )
        .unwrap();
        let full_message = tilo_delivery_protocol::peer_actions::ActionMessage::BrokerCatalogue {
            host: uuid::Uuid::new_v4(),
            catalogue: full,
        };
        assert!(
            full_message.validate(),
            "the full shape remains structurally valid"
        );
        assert!(
            serde_json::to_vec(&full_message).unwrap().len()
                > tilo_delivery_protocol::peer_actions::MAX_PRIVATE_BYTES
        );
        let scope = tilo_delivery_protocol::peer_actions::ExactActionRoute {
            app_id: tilo_owner_commit::desktop_command::APP_ID.into(),
            record_id: String::new(),
            resource: tilo_owner_commit::desktop_command::RESOURCE.into(),
            action: tilo_owner_commit::desktop_command::ACTION.into(),
        };
        let scoped: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &actor, Some(&scope))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(scoped["surfaces"].as_array().unwrap().len(), 1);
        assert_eq!(
            scoped["surfaces"][0]["actions"].as_array().unwrap().len(),
            1
        );
        let denied = BrokerActor::person("stranger");
        let denied: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &denied, Some(&scope))
                .unwrap(),
        )
        .unwrap();
        assert!(
            denied["surfaces"].as_array().unwrap().is_empty(),
            "scope narrows discovery but never grants authorization"
        );
        let launcher_scope = tilo_delivery_protocol::peer_actions::ExactActionRoute {
            app_id: "so.tilo.word-processor".into(),
            record_id: String::new(),
            resource: "app.so.tilo.word-processor".into(),
            action: "open".into(),
        };
        let launcher: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &actor, Some(&launcher_scope))
                .unwrap(),
        )
        .unwrap();
        assert!(
            launcher["surfaces"].as_array().unwrap().is_empty(),
            "scope cannot add a launcher hidden by another live record of that app"
        );
        let directed = tilo_delivery_protocol::peer_actions::DirectedAction {
            room: uuid::Uuid::parse_str(&cluster).unwrap(),
            request_id: uuid::Uuid::new_v4(),
            source: [1; 32],
            target: [2; 32],
            message: tilo_delivery_protocol::peer_actions::ActionMessage::BrokerCatalogue {
                host: uuid::Uuid::new_v4(),
                catalogue: scoped,
            },
        };
        assert!(
            directed.validate(),
            "the exact desktop reply fits transport bounds"
        );
    }

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::unchecked_time_subtraction,
        reason = "the directed-action test keeps capability setup, delivery, and denial assertions in one scenario"
    )]
    #[test]
    fn live_action_invocation_is_fresh_directed_and_permission_checked() {
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(31),
            "Action room",
        );
        workspace.set_owner("owner");
        let route = CrdtRoute::app("so.tilo.files", workspace.room().to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership),
            None,
        );
        state.surfaces.lock().expect("surfaces").insert(
            7,
            PublishedSurface {
                peer_id: 7,
                app_id: "so.tilo.files".into(),
                route,
                actions: vec![
                    PublishedAction {
                        semantic_identity: None,
                        resource: "files.entry:/tmp/report".into(),
                        action: "reveal".into(),
                        resource_kind: "files.entry".into(),
                        resource_label: "report".into(),
                        action_label: "Reveal".into(),
                        description: None,
                        operation_kind: tilo_actions::OperationKind::View,
                        params: Vec::new(),
                        contract: tilo_actions::ActionContract::view(),
                        view: None,
                        input: PublishedInput::None,
                    },
                    PublishedAction {
                        semantic_identity: None,
                        resource: "files.entry:/tmp/report".into(),
                        action: "erase".into(),
                        resource_kind: "files.entry".into(),
                        resource_label: "report".into(),
                        action_label: "Erase".into(),
                        description: None,
                        operation_kind: tilo_actions::OperationKind::Effect,
                        params: Vec::new(),
                        contract: tilo_actions::ActionContract {
                            risk: tilo_actions::RiskLevel::Destructive,
                            confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                            ..tilo_actions::ActionContract::default()
                        },
                        view: None,
                        input: PublishedInput::None,
                    },
                    PublishedAction {
                        semantic_identity: None,
                        resource: "files.entry:/tmp/report".into(),
                        action: "refresh".into(),
                        resource_kind: "files.entry".into(),
                        resource_label: "report".into(),
                        action_label: "Refresh".into(),
                        description: None,
                        operation_kind: tilo_actions::OperationKind::Effect,
                        params: Vec::new(),
                        contract: tilo_actions::ActionContract {
                            execution: tilo_actions::ExecutionMode::Background {
                                cancellable: true,
                            },
                            result: tilo_actions::ResultSpec::Job,
                            ..tilo_actions::ActionContract::default()
                        },
                        view: None,
                        input: PublishedInput::None,
                    },
                ],
                live: true,
            },
        );
        let (tx, mut invocations) = mpsc::unbounded_channel();
        state
            .invocation_subscribers
            .lock()
            .expect("subscribers")
            .insert(7, tx);

        let action_cluster = state
            .surfaces
            .lock()
            .expect("surfaces")
            .get(&7)
            .expect("surface")
            .route
            .cluster_id
            .clone();
        let owner_catalogue = state
            .action_catalogue_json(&action_cluster, "owner")
            .expect("owner catalogue");
        assert!(owner_catalogue.contains("files.entry:/tmp/report"));
        let denied_catalogue = state
            .action_catalogue_json(&action_cluster, "stranger")
            .expect("filtered catalogue");
        assert!(
            !denied_catalogue.contains("files.entry:/tmp/report"),
            "denied actions must be absent from discovery, not merely refused later"
        );

        let completion = state
            .invoke(7, "files.entry:/tmp/report", "reveal", None, "owner")
            .expect("owner can invoke current offer");
        let invocation = invocations.try_recv().expect("directed invocation");
        assert_eq!(invocation.resource, "files.entry:/tmp/report");
        assert!(
            state
                .start_invocation(7, "files.entry:/tmp/report", "erase", None, "owner")
                .is_err(),
            "a consequential action cannot bypass the confirmation protocol"
        );
        let token = state
            .prepare_confirmation(7, "files.entry:/tmp/report", "erase", None, "owner", None)
            .expect("confirmation grant");
        let erased = state
            .start_invocation_confirmed(
                7,
                "files.entry:/tmp/report",
                "erase",
                None,
                "owner",
                Some(&token),
            )
            .expect("matching one-use grant");
        assert!(
            state
                .start_invocation_confirmed(
                    7,
                    "files.entry:/tmp/report",
                    "erase",
                    None,
                    "owner",
                    Some(&token),
                )
                .is_err(),
            "confirmation grants are single use"
        );
        let erase_invocation = invocations.try_recv().expect("confirmed invocation");
        assert_eq!(erase_invocation.id, erased.id);

        let refresh = state
            .start_invocation(7, "files.entry:/tmp/report", "refresh", None, "owner")
            .expect("cancellable job");
        let refresh_invocation = invocations.try_recv().expect("job invocation");
        assert_eq!(refresh_invocation.id, refresh.id);
        let retained_cancellation = state
            .pending_invocations
            .lock()
            .expect("pending")
            .get(&refresh.id)
            .expect("refresh")
            .cancellation_fence
            .clone();
        assert!(!retained_cancellation.load(Ordering::Acquire));
        assert!(state.request_cancellation(refresh.id, "stranger").is_err());
        state
            .request_cancellation(refresh.id, "owner")
            .expect("owner can request cooperative cancellation")
            .wait_blocking()
            .expect("the request is durable");
        assert!(
            state
                .cancellation_requests
                .lock()
                .expect("cancellation registry")
                .contains(&refresh.id)
        );
        let now = std::time::Instant::now();
        {
            let mut pending = state.pending_invocations.lock().expect("pending");
            pending
                .get_mut(&refresh.id)
                .expect("refresh remains pending")
                .cancellation_requested_at = Some(now - std::time::Duration::from_secs(2));
            pending
                .get_mut(&erased.id)
                .expect("erase remains pending")
                .last_activity = now - std::time::Duration::from_secs(2);
        }
        state.expire_stale_invocations_at(
            now,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        );
        assert!(
            retained_cancellation.load(Ordering::Acquire),
            "the retained worker observes cancellation even after expiry removes the pending job"
        );
        assert!(matches!(
            state.invocation_status(refresh.id),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "action_cancellation_timeout"
        ));
        assert_eq!(
            refresh
                .completion
                .recv()
                .expect("cancel timeout completion")
                .code,
            "action_cancellation_timeout"
        );
        assert!(matches!(
            state.invocation_status(erased.id),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "action_timeout"
        ));
        assert_eq!(
            erased
                .completion
                .recv()
                .expect("idle timeout completion")
                .code,
            "action_timeout"
        );
        assert!(
            state
                .invoke(7, "files.entry:/tmp/report", "gone", None, "owner")
                .is_err(),
            "an address absent from the current level-triggered surface is stale"
        );
        assert!(
            state
                .invoke(7, "files.entry:/tmp/report", "reveal", None, "stranger")
                .is_err(),
            "broker authorization is checked again at delivery time"
        );
        let pending = state
            .pending_invocations
            .lock()
            .expect("pending")
            .remove(&invocation.id)
            .expect("completion slot");
        pending
            .tx
            .send(InvocationCompletion {
                code: String::new(),
                message: String::new(),
            })
            .expect("caller still waiting");
        assert_eq!(
            completion.recv().expect("completion"),
            InvocationCompletion {
                code: String::new(),
                message: String::new(),
            }
        );

        let launchable = PublishedSurface {
            peer_id: 99,
            app_id: "so.tilo.files".into(),
            route: CrdtRoute::app(
                "so.tilo.files",
                state
                    .surfaces
                    .lock()
                    .expect("surfaces")
                    .get(&7)
                    .expect("live surface")
                    .route
                    .cluster_id
                    .clone(),
            ),
            actions: vec![PublishedAction {
                semantic_identity: None,
                resource: "files.app".into(),
                action: "new-window".into(),
                resource_kind: "files.app".into(),
                resource_label: "Files".into(),
                action_label: "New window".into(),
                description: None,
                operation_kind: tilo_actions::OperationKind::View,
                params: Vec::new(),
                contract: tilo_actions::ActionContract::view(),
                view: None,
                input: PublishedInput::None,
            }],
            live: false,
        };
        state.launchable.retain(&launchable);
        state.surfaces.lock().expect("surfaces").remove(&7);
        let archived = state.published_surfaces();
        assert_eq!(archived.len(), 1);
        assert!(!archived[0].live);
        assert_eq!(archived[0].actions[0].action, "new-window");
        assert!(
            !state
                .action_catalogue_json(&archived[0].route.cluster_id, "owner")
                .expect("live-only direct catalogue")
                .contains("new-window"),
            "archived launchable descriptors are not directly invokable through this protocol"
        );
    }

    #[test]
    fn a_handoff_request_reaches_exactly_the_addressed_instance() {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let room = uuid::Uuid::new_v4().to_string();
        let key =
            |app: &str, record: &str| CrdtRoute::record(app, room.clone(), record).channel_key();
        let mut addressed =
            state.subscribe_context(&key("dev.tilo.notes", "continuation:5"), 7, None);
        let mut sibling =
            state.subscribe_context(&key("dev.tilo.notes", "continuation:6"), 8, None);
        let mut other_app =
            state.subscribe_context(&key("dev.tilo.sheets", "continuation:5"), 9, None);
        let request = collab_types::handoff::HandoffRequest::Checkpoint { token: 3 };
        // The route SessionFollowDispatcher::request_handoff addresses.
        state.fanout_to_route(
            &CollabEvent::Handoff {
                request: request.clone(),
            },
            u64::MAX,
            &CrdtRoute::record("dev.tilo.notes", room.clone(), "continuation:5"),
        );
        assert!(matches!(
            addressed.try_recv(),
            Ok(ContextEnvelope::Handoff(message))
                if collab_types::handoff::HandoffRequest::decode(&message) == Some(request)
        ));
        assert!(
            sibling.try_recv().is_err(),
            "another window of the app is not asked"
        );
        assert!(other_app.try_recv().is_err());
    }

    #[test]
    fn private_context_alias_uses_latest_exact_subscriber_without_duplicate_delivery() {
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let room = uuid::Uuid::new_v4().to_string();
        let document = CrdtRoute::record("so.tilo.code-editor", &room, "native-project");
        let private = CrdtRoute::record("so.tilo.code-editor", &room, "continuation:42");
        let mut old = state.subscribe_context(&document.channel_key(), 7, None);
        let mut sibling = state.subscribe_context(&document.channel_key(), 8, None);
        state.alias_private_context(&document.channel_key(), &private.channel_key(), 7);
        let mut current = state.subscribe_context(&document.channel_key(), 7, None);
        state.alias_private_context(&document.channel_key(), &private.channel_key(), 7);
        state.alias_private_context(&document.channel_key(), &private.channel_key(), 7);
        state.alias_private_context(&document.channel_key(), &private.channel_key(), 99);
        let request = collab_types::handoff::HandoffRequest::Checkpoint { token: 3 };
        state.fanout_to_route(
            &CollabEvent::Handoff {
                request: request.clone(),
            },
            u64::MAX,
            &private,
        );
        assert!(
            matches!(current.try_recv(), Ok(ContextEnvelope::Handoff(message))
            if collab_types::handoff::HandoffRequest::decode(&message) == Some(request))
        );
        assert!(
            current.try_recv().is_err(),
            "a renewed stream must receive each private view once"
        );
        assert!(
            old.try_recv().is_err(),
            "superseded context receiver cannot receive private views"
        );
        assert!(
            sibling.try_recv().is_err(),
            "sharing the document does not grant another window's private stream"
        );
    }

    #[test]
    fn late_join_snapshot_request_selects_exactly_one_existing_holder() {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let channel = "dev.tilo.notes#room:test#record:daily";
        let mut first = state.subscribe_context(channel, 7, None);
        let mut second = state.subscribe_context(channel, 9, None);
        let _requester = state.subscribe_context(channel, 12, None);

        assert!(state.request_document_snapshot(channel, 12));
        assert!(matches!(
            first.try_recv(),
            Ok(ContextEnvelope::SnapshotRequested)
        ));
        assert!(second.try_recv().is_err(), "only one holder may respond");
        let requester_only_channel = "dev.tilo.notes#room:test#record:empty";
        let mut requester_only = state.subscribe_context(requester_only_channel, 12, None);
        assert!(
            !state.request_document_snapshot(requester_only_channel, 12),
            "the requester cannot answer from its own empty state"
        );
        assert!(requester_only.try_recv().is_err());
        assert!(
            !state.request_document_snapshot("missing", 12),
            "a route without a holder is retryable, not falsely acknowledged"
        );
    }

    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "the receipt assertions retain the read guard for the complete consistency snapshot"
    )]
    #[test]
    fn remote_agent_effect_terminals_become_redacted_workspace_receipts() {
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(32),
            "Agent effect room",
        );
        workspace.set_owner("owner");
        let cluster_id = workspace.room().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.clone()),
            None,
        );
        let (tx, _rx) = std::sync::mpsc::channel();
        let invocation = tilo_actions::InvocationId::new();
        let pending = PendingInvocation {
            caller: None,
            activity: None,
            owner: InvocationOwner::Local {
                actor: "agent:Scribe".into(),
            },
            network_evidence: None,
            original: None,
            policy_time: None,
            journal_key: None,
            peer_id: 7,
            tx,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
            prepared_effect: Arc::default(),
            last_sequence: 0,
            actor: "agent:Scribe".into(),
            cancellable: false,
            retryable_on_disconnect: false,
            execution: tilo_actions::ExecutionMode::Immediate,
            result: tilo_actions::ResultSpec::None,
            operation_kind: tilo_actions::OperationKind::Effect,
            cluster_id,
            context: tilo_actions::ActionContext::for_resource(
                "document:brief",
                "document",
                "Release brief",
            )
            .with_app("org.tilo.Writer", "Writer")
            .with_group("task:release", "Release", "task"),
            last_activity: std::time::Instant::now(),
            cancellation_requested_at: None,
            cancellation_fence: Arc::new(AtomicBool::new(false)),
        };
        let update = tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "publish_denied".into(),
                message: "authorization=secret-value".into(),
                details: Some(serde_json::json!({"body": "private draft"})),
                retryable: false,
            },
        };

        state.record_effect_receipt(&pending, &update);
        state.record_effect_receipt(&pending, &update);

        let succeeded_invocation = tilo_actions::InvocationId::new();
        let mut succeeded_pending = pending;
        succeeded_pending.update = tilo_actions::InvocationUpdate::Accepted {
            invocation: succeeded_invocation,
        };
        state.record_effect_receipt(
            &succeeded_pending,
            &tilo_actions::InvocationUpdate::Succeeded {
                invocation: succeeded_invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({"token": "output-secret"})),
                    resources: vec!["files.entry:/private/export".into()],
                },
            },
        );
        let mut private_view = succeeded_pending;
        private_view.operation_kind = tilo_actions::OperationKind::View;
        let view_invocation = tilo_actions::InvocationId::new();
        state.record_effect_receipt(
            &private_view,
            &tilo_actions::InvocationUpdate::Succeeded {
                invocation: view_invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        );

        let tree = membership.read();
        let receipts = &tree.workspaces[0].effect_receipts;
        assert_eq!(
            receipts.len(),
            2,
            "invocation identity deduplicates retry delivery"
        );
        assert_eq!(receipts[0].actor, "agent:Scribe");
        assert_eq!(
            receipts[0]
                .context
                .group
                .as_ref()
                .map(|group| group.id.as_str()),
            Some("task:release")
        );
        let durable = serde_json::to_string(receipts).expect("serialize receipts");
        assert!(!durable.contains("secret-value"));
        assert!(!durable.contains("private draft"));
        assert!(!durable.contains("output-secret"));
        assert!(!durable.contains("private/export"));
        assert!(durable.contains("publish_denied"));
        assert!(receipts.iter().any(|receipt| {
            receipt.invocation == succeeded_invocation
                && receipt.status == tilo_actions::EffectReceiptStatus::Succeeded
        }));
    }

    #[test]
    fn current_connection_invocation_sentinel_cannot_target_another_app() {
        assert_eq!(
            invocation_target_peer(ui::collaboration::CURRENT_CONNECTION_PEER_ID, 41),
            41
        );
        assert_eq!(invocation_target_peer(7, 41), 7);
    }
    #[cfg(test)]
    mod durable_admission_tests {
        mod peer_integration {
            include!("peer_action_integration_tests.rs");
        }
        use super::super::invocation_journal::BarrierFailure;
        use super::*;

        async fn update_app(
            client: &channel_handle::Client,
            id: u64,
            update: &tilo_actions::InvocationUpdate,
        ) -> Result<(), capnp::Error> {
            let mut request = client.update_invocation_request();
            request.get().set_id(id);
            request
                .get()
                .set_update_json(serde_json::to_string(update).unwrap());
            request.send().promise.await?;
            Ok(())
        }

        const FULL_ID: u128 = 0xa5a5_a5a5_a5a5_a5a5_0000_0000_0000_002b;

        struct DurableFixture {
            directory: LocalDirectoryFixture,
            _identity_env: tilo_test_env::ScopedEnv,
        }

        impl std::ops::Deref for DurableFixture {
            type Target = LocalDirectoryFixture;

            fn deref(&self) -> &Self::Target {
                &self.directory
            }
        }

        impl std::ops::DerefMut for DurableFixture {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.directory
            }
        }

        fn durable_fixture(
            root: &std::path::Path,
            membership: Option<WorkspaceTreeHandle>,
        ) -> (DurableFixture, VerifiedActionCaller, CallerInvocation) {
            let mut identity_env = tilo_test_env::ScopedEnv::new();
            identity_env
                .set("XDG_CONFIG_HOME", root.join("config"))
                .set("TILO_IDENTITY_ROOT", root.join("identity"))
                .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
            tilo_identity_host::provisioning::ensure_collaboration_identity(
                &root.join("config/tilo/identity"),
                &tilo_user_identity::IdentityPaths::under(root.join("identity")),
                tilo_identity_host::provisioning::IdentityStorage::Files,
            )
            .expect("provision the host admission and terminal receipt signer");
            let root_key = SigningKey::from_bytes(&[51; 32]);
            let device = SigningKey::from_bytes(&[52; 32]);
            let membership = membership.unwrap_or_else(|| {
                let mut tree = WorkspaceTree::default();
                let mut workspace = crate::test_support::Workspace::new(
                    crate::test_support::WorkspaceId(71),
                    "Durable room",
                );
                workspace.set_owner(principal_id_for_root(root_key.verifying_key().to_bytes()));
                tree.workspaces.push(workspace);
                tree.set_active(crate::test_support::WorkspaceId(71));
                tree.persist_path = Some(root.join("state.json"));
                tree.shared()
            });
            let fixture = local_directory_fixture_with_devices(
                membership,
                Some(71),
                Some(Arc::new(vec![
                    device.verifying_key().to_bytes(),
                    SigningKey::from_bytes(&[53; 32]).verifying_key().to_bytes(),
                ])),
            );
            let mut action =
                published_directory_action(&fixture.directory_state.borrow().row()).unwrap();
            action.operation_kind = tilo_actions::OperationKind::Effect;
            action.contract = tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..tilo_actions::ActionContract::default()
            };
            assert!(action.contract.validate_declaration().is_empty());
            fixture.state.surfaces.lock().unwrap().insert(
                71,
                PublishedSurface {
                    peer_id: 71,
                    app_id: fixture.route.app_id.clone(),
                    route: fixture.route.clone(),
                    actions: vec![action],
                    live: true,
                },
            );
            fixture
                .state
                .stable_invocation_subscribers
                .lock()
                .unwrap()
                .insert(71);
            let caller = fixture
                .state
                .verify_action_caller(
                    &fixture.cluster_id,
                    &certified_join(&fixture.cluster_id, &root_key, &device)
                        .encode()
                        .unwrap(),
                )
                .unwrap();
            let request = CallerInvocation {
                recipient: None,
                revision: None,
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID)),
                host: fixture.state.invocation_host().unwrap(),
                peer_id: 71,
                app_id: fixture.route.app_id.clone(),
                record_id: fixture.route.record_id.clone(),
                resource: "workspace:atlas".into(),
                action: "open".into(),
                argument: None,
            };
            (
                DurableFixture {
                    directory: fixture,
                    _identity_env: identity_env,
                },
                caller,
                request,
            )
        }

        fn effect(root: &std::path::Path) {
            use std::io::Write;
            let path = root.join("effect-counter");
            let before = std::fs::read_to_string(&path).map_or(0, |s| s.parse::<u64>().unwrap());
            let mut file = std::fs::File::create(path).unwrap();
            write!(file, "{}", before + 1).unwrap();
            file.sync_all().unwrap();
            std::fs::File::open(root).unwrap().sync_all().unwrap();
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_uuid_collisions_never_reach_application_stream() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    // Both legacy directions matter: a caller can reserve a future
                    // numeric UUID, or attempt to reuse a previously delivered one.
                    for scenario in [
                        "cross-device",
                        "caller-before-legacy",
                        "legacy-before-caller",
                    ] {
                        let root = tempfile::tempdir().unwrap();
                        let (mut fixture, caller, mut request) = durable_fixture(root.path(), None);
                        let other = fixture
                            .state
                            .verify_action_caller(
                                &fixture.cluster_id,
                                &certified_join(
                                    &fixture.cluster_id,
                                    &SigningKey::from_bytes(&[51; 32]),
                                    &SigningKey::from_bytes(&[53; 32]),
                                )
                                .encode()
                                .unwrap(),
                            )
                            .unwrap();
                        if scenario == "caller-before-legacy" {
                            request.invocation =
                                tilo_actions::InvocationId(uuid::Uuid::from_u128(2));
                        }
                        let wire_id = if scenario == "legacy-before-caller" {
                            let job = fixture
                                .state
                                .start_invocation(
                                    71,
                                    &request.resource,
                                    &request.action,
                                    None,
                                    &caller.admitted.principal_id,
                                )
                                .unwrap();
                            request.invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(
                                u128::from(job.id),
                            ));
                            job.id
                        } else {
                            fixture
                                .state
                                .start_verified_invocation(&caller, &request, None)
                                .unwrap()
                                .wire_id
                        };
                        let (_, empty_rx) = mpsc::unbounded_channel();
                        let rx = Rc::new(AsyncMutex::new(std::mem::replace(
                            &mut fixture.invocation_rx,
                            empty_rx,
                        )));
                        let stream: invocation_stream::Client =
                            capnp_rpc::new_client(InvocationStreamServer {
                                host_guard: None,
                                rx: rx.clone(),
                                delivered: None,
                            });
                        let response = stream.read_request().send().promise.await.unwrap();
                        assert_eq!(response.get().unwrap().get_id(), wire_id);
                        assert_eq!(
                            response.get().unwrap().get_invocation_uuid().unwrap(),
                            request.invocation.0.as_bytes()
                        );
                        effect(root.path());
                        let conflict = if scenario == "caller-before-legacy" {
                            fixture
                                .state
                                .start_invocation(
                                    71,
                                    &request.resource,
                                    &request.action,
                                    None,
                                    &caller.admitted.principal_id,
                                )
                                .map(|_| ())
                        } else {
                            fixture
                                .state
                                .start_verified_invocation(&other, &request, None)
                                .map(|_| ())
                        };
                        assert!(
                            conflict
                                .err()
                                .unwrap()
                                .contains("action_invocation_identity_conflict"),
                            "{scenario}: conflicting UUID must fail before app dispatch"
                        );
                        assert!(
                            rx.lock().await.try_recv().is_err(),
                            "only one application delivery"
                        );
                        assert!(
                            fixture
                                .state
                                .verified_invocation_status(&other, request.invocation)
                                .unwrap()
                                .is_none()
                        );
                        assert!(
                            fixture
                                .state
                                .cancel_verified_invocation(&other, request.invocation)
                                .await
                                .is_err()
                        );
                        let channel = fixture.invocation_channel();
                        let terminal = tilo_actions::InvocationUpdate::Succeeded {
                            invocation: request.invocation,
                            output: tilo_actions::ActionOutput::default(),
                        };
                        update_app(&channel, wire_id, &terminal).await.unwrap();
                        if scenario != "legacy-before-caller" {
                            let duplicate = fixture
                                .state
                                .start_verified_invocation(&caller, &request, None)
                                .unwrap();
                            assert_eq!(duplicate.wire_id, wire_id);
                            assert_eq!(duplicate.update, terminal);
                        }
                        let membership = fixture.membership.as_ref().unwrap().clone();
                        drop(channel);
                        drop(fixture);
                        let (mut reopened, _, mut again) =
                            durable_fixture(root.path(), Some(membership));
                        again.invocation = request.invocation;
                        assert!(
                            reopened
                                .state
                                .start_verified_invocation(&other, &again, None)
                                .is_err(),
                            "{scenario}: UUID reservation survives terminal completion and restart"
                        );
                        assert!(reopened.invocation_rx.try_recv().is_err());
                        assert_eq!(
                            std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                            "1"
                        );
                    }
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_real_effect_once_across_concurrent_duplicate_response_loss_and_restart()
         {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let root = tempfile::tempdir().unwrap();
                    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                    fixture
                        .state
                        .surfaces
                        .lock()
                        .unwrap()
                        .get_mut(&71)
                        .unwrap()
                        .actions[0]
                        .contract
                        .result = tilo_actions::ResultSpec::Json {
                        schema: serde_json::json!({"type":"object"}),
                    };
                    // Every thread is spawned before any is joined.
                    #[expect(
                        clippy::needless_collect,
                        reason = "spawning all threads before joining is the concurrency under test"
                    )]
                    let requests = (0..8)
                        .map(|_| {
                            let state = fixture.state.clone();
                            let caller = caller.clone();
                            let request = request.clone();
                            std::thread::spawn(move || {
                                state
                                    .start_verified_invocation(&caller, &request, None)
                                    .unwrap()
                            })
                        })
                        .collect::<Vec<_>>();
                    let admitted = requests
                        .into_iter()
                        .map(|t| t.join().unwrap())
                        .collect::<Vec<_>>();
                    assert!(
                        admitted
                            .iter()
                            .all(|row| row.wire_id == admitted[0].wire_id)
                    );
                    let (_, empty_rx) = mpsc::unbounded_channel();
                    let rx = Rc::new(AsyncMutex::new(std::mem::replace(
                        &mut fixture.invocation_rx,
                        empty_rx,
                    )));
                    let stream: invocation_stream::Client =
                        capnp_rpc::new_client(InvocationStreamServer {
                            host_guard: None,
                            rx: rx.clone(),
                            delivered: None,
                        });
                    let response = stream.read_request().send().promise.await.unwrap();
                    let delivered = response.get().unwrap();
                    let delivered_id = delivered.get_id();
                    assert_eq!(
                        delivered.get_invocation_uuid().unwrap(),
                        request.invocation.0.as_bytes()
                    );
                    assert!(
                        rx.lock().await.try_recv().is_err(),
                        "one actual Cap'n Proto application delivery"
                    );
                    effect(root.path());
                    let terminal = tilo_actions::InvocationUpdate::Succeeded {
                        invocation: request.invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(
                                serde_json::json!({"counter":1,"private":"private-result-token"}),
                            ),
                            resources: vec![],
                        },
                    };
                    // Force receipt persistence to fail independently of the already opened journal.
                    fixture.membership.as_ref().unwrap().write().persist_path =
                        Some(root.path().to_owned());
                    fixture
                        .complete_invocation(delivered_id, &terminal)
                        .await
                        .unwrap();
                    // Discard the original response and retry under freshly verified caller custody.
                    let duplicate = fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    assert_eq!(duplicate.update, terminal);
                    assert!(
                        fixture
                            .state
                            .invocation_statuses(&caller.admitted.principal_id)
                            .is_empty(),
                        "legacy principal-only lookup cannot reveal certified terminal output"
                    );
                    let mut changed = request.clone();
                    changed.argument = Some(String::new());
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &changed, None)
                            .is_err()
                    );
                    let membership = fixture.membership.clone().unwrap();
                    {
                        let mut tree = membership.write();
                        assert_eq!(tree.workspaces[0].effect_receipts.len(), 1);
                        assert!(
                            !serde_json::to_string(&tree.workspaces[0].effect_receipts)
                                .unwrap()
                                .contains("private-result-token")
                        );
                        tree.persist_path = Some(root.path().join("state.json"));
                    }
                    drop(fixture);
                    let (mut restarted, caller, _) = durable_fixture(root.path(), Some(membership));
                    assert_eq!(
                        restarted
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .unwrap()
                            .update,
                        terminal
                    );
                    assert!(restarted.invocation_rx.try_recv().is_err());
                    assert_eq!(
                        std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                        "1"
                    );
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_crash_boundaries_never_repeat_real_effect() {
            tokio::task::LocalSet::new().run_until(async {
                for boundary in [BarrierFailure::File, BarrierFailure::AfterAdmission, BarrierFailure::Terminal] {
                    let root = tempfile::tempdir().unwrap();
                    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                    if boundary == BarrierFailure::Terminal {
                        fixture.state.start_verified_invocation(&caller, &request, None).unwrap();
                        let delivered = fixture.invocation_rx.try_recv().unwrap();
                        effect(root.path());
                        fixture.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at = Some(BarrierFailure::File);
                        assert!(fixture.complete_invocation(delivered.id, &tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput::default() }).await.is_err());
                    } else {
                        fixture.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at = Some(boundary);
                        assert!(fixture.state.start_verified_invocation(&caller, &request, None).is_err());
                        assert!(fixture.invocation_rx.try_recv().is_err());
                    }
                    let membership = fixture.membership.clone().unwrap();
                    drop(fixture);
                    let (mut restarted, caller, _) = durable_fixture(root.path(), Some(membership));
                    if boundary == BarrierFailure::File {
                        assert!(restarted.state.verified_invocation_status(&caller, request.invocation).unwrap().is_none());
                        assert!(!root.path().join("effect-counter").exists());
                    } else {
                        let result = restarted.state.start_verified_invocation(&caller, &request, None).unwrap();
                        assert!(result.uncertain);
                        assert!(matches!(result.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" && !error.retryable));
                        assert!(restarted.invocation_rx.try_recv().is_err());
                        if boundary == BarrierFailure::Terminal {
                            assert_eq!(std::fs::read_to_string(root.path().join("effect-counter")).unwrap(), "1");
                        } else { assert!(!root.path().join("effect-counter").exists()); }
                    }
                }
            }).await;
        }

        #[test]
        fn invocation_journal_owner_is_device_scoped_and_live_authority_precedes_duplicate_disclosure()
         {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            fixture.invocation_rx.try_recv().unwrap();
            let other_device = SigningKey::from_bytes(&[53; 32]);
            let other = fixture
                .state
                .verify_action_caller(
                    &fixture.cluster_id,
                    &certified_join(
                        &fixture.cluster_id,
                        &SigningKey::from_bytes(&[51; 32]),
                        &other_device,
                    )
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            assert!(
                fixture
                    .state
                    .verified_invocation_status(&other, request.invocation)
                    .unwrap()
                    .is_none()
            );
            assert!(
                futures::executor::block_on(
                    fixture
                        .state
                        .cancel_verified_invocation(&other, request.invocation)
                )
                .is_err()
            );
            assert!(
                fixture
                    .state
                    .invocation_statuses(&caller.admitted.principal_id)
                    .is_empty(),
                "legacy root-scoped queries must not disclose another device's private results"
            );
            let mut bad = request.clone();
            bad.invocation = tilo_actions::InvocationId::new();
            bad.record_id = "different-record".into();
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &bad, None)
                    .is_err()
            );
            fixture.membership.as_ref().unwrap().write().workspaces[0].set_owner("new-owner");
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .is_err()
            );
            assert!(
                fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .is_err()
            );
            assert!(fixture.invocation_rx.try_recv().is_err());
        }

        #[test]
        fn invocation_journal_old_apps_wrong_rooms_and_silence_fail_safely() {
            for disconnected in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                fixture
                    .state
                    .stable_invocation_subscribers
                    .lock()
                    .unwrap()
                    .clear();
                assert!(
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap_err()
                        .contains("stable_identity_unavailable")
                );
                assert!(fixture.invocation_rx.try_recv().is_err());
                fixture
                    .state
                    .stable_invocation_subscribers
                    .lock()
                    .unwrap()
                    .insert(71);
                let original_room = fixture
                    .state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get(&71)
                    .unwrap()
                    .route
                    .cluster_id
                    .clone();
                fixture
                    .state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get_mut(&71)
                    .unwrap()
                    .route
                    .cluster_id = uuid::Uuid::new_v4().to_string();
                assert!(
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .is_err()
                );
                fixture
                    .state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get_mut(&71)
                    .unwrap()
                    .route
                    .cluster_id = original_room;
                let admitted = fixture
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .unwrap();
                fixture.invocation_rx.try_recv().unwrap();
                let cancelled = futures::executor::block_on(
                    fixture
                        .state
                        .cancel_verified_invocation(&caller, request.invocation),
                )
                .unwrap();
                assert!(cancelled.cancellation_requested);
                // A durable cancellation request acknowledges receipt only; it
                // does not assert that the running external effect stopped.
                use tilo_delivery_protocol::{
                    peer_actions::{ActionMessage, DirectedAction},
                    transport_v2::{self, EnvelopeV2},
                };
                let cancel = DirectedAction {
                    room: uuid::Uuid::parse_str(&caller.room).unwrap(),
                    request_id: uuid::Uuid::new_v4(),
                    source: [52; 32],
                    target: [51; 32],
                    message: ActionMessage::Cancel {
                        invocation: request.invocation,
                    },
                };
                // The broker's reply path answers exactly this cancel: same
                // room and request, reversed direction, the same invocation.
                let acknowledgement = crate::peer_actions::bounded_reply(
                    &cancel,
                    ActionMessage::CancellationRequested {
                        invocation: request.invocation,
                    },
                );
                assert_eq!(
                    acknowledgement,
                    DirectedAction {
                        room: cancel.room,
                        request_id: cancel.request_id,
                        source: cancel.target,
                        target: cancel.source,
                        message: ActionMessage::CancellationRequested {
                            invocation: request.invocation,
                        },
                    }
                );
                // And it survives the private lane the relay carries.
                assert!(cancel.validate() && acknowledgement.validate());
                let reply = EnvelopeV2::Directed(acknowledgement);
                let encoded = transport_v2::encode(&reply).unwrap();
                assert_eq!(transport_v2::decode(&encoded[4..]).unwrap(), reply);
                if disconnected {
                    fixture.state.remove_connection(71);
                } else {
                    fixture.state.expire_stale_invocations_at(
                        std::time::Instant::now(),
                        Duration::ZERO,
                        Duration::ZERO,
                    );
                }
                let status = fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .unwrap()
                    .unwrap();
                assert!(status.uncertain);
                assert!(
                    matches!(status.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown")
                );
                assert_eq!(
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap()
                        .wire_id,
                    admitted.wire_id
                );
                assert!(fixture.invocation_rx.try_recv().is_err());
            }
        }

        /// An agent, a device admin or another device drives an app through
        /// its actions. The host learns of each invocation the app is handed,
        /// so a window the app closes because of it is not taken for one it
        /// lost.
        #[tokio::test(flavor = "current_thread")]
        async fn the_host_learns_each_invocation_an_app_is_handed() {
            struct Recording(Mutex<Vec<(String, String)>>);
            impl crate::services::LauncherServices for Recording {
                fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
                    Vec::new()
                }
                fn launch(
                    &self,
                    _: &str,
                    _: &tilo_desktop_model::Workspace,
                    _: Option<&str>,
                    _: &crate::services::LaunchPlace,
                ) -> Result<(), String> {
                    Err("this host launches nothing".into())
                }
                fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
                    false
                }
                fn note_acted_on(&self, app: &str, room: &str) {
                    self.0.lock().unwrap().push((app.into(), room.into()));
                }
            }
            tokio::task::LocalSet::new()
                .run_until(async {
                    let recording = Arc::new(Recording(Mutex::new(Vec::new())));
                    let services = HostServices {
                        launcher: Some(recording.clone()),
                        ..Default::default()
                    };
                    let (tx, rx) = mpsc::unbounded_channel();
                    let stream: invocation_stream::Client =
                        capnp_rpc::new_client(InvocationStreamServer {
                            host_guard: None,
                            rx: Rc::new(AsyncMutex::new(rx)),
                            delivered: invocation_delivery_notice(
                                &services,
                                "so.tilo.notes".into(),
                                "room-7".into(),
                            ),
                        });
                    assert!(recording.0.lock().unwrap().is_empty());
                    tx.send(InvocationEnvelope {
                        id: 1,
                        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(1)),
                        resource: "window".into(),
                        action: "close".into(),
                        argument: None,
                        actor: "agent:assistant".into(),
                        context: None,
                    })
                    .unwrap();
                    stream.read_request().send().promise.await.unwrap();
                    assert_eq!(
                        *recording.0.lock().unwrap(),
                        vec![("so.tilo.notes".to_owned(), "room-7".to_owned())]
                    );
                    assert!(
                        invocation_delivery_notice(
                            &HostServices::default(),
                            "a".into(),
                            "r".into()
                        )
                        .is_none(),
                        "a host without a launcher has no one to tell"
                    );
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_native_stream_negotiates_uuid_capability_explicitly() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let root = tempfile::tempdir().unwrap();
                    let (fixture, caller, request) = durable_fixture(root.path(), None);
                    let client: channel_handle::Client =
                        capnp_rpc::new_client(CapnpChannelHandle {
                            person_chat_views: RefCell::default(),
                            committed_read: Rc::new(RefCell::new(
                                committed_document::Slot::default(),
                            )),
                            state: fixture.state.clone(),
                            directories: fixture.directories.clone(),
                            channel_id: fixture.route.channel_key(),
                            peer_id: fixture.peer_id,
                            connection_id: fixture.peer_id,
                            verified_identity: caller.admitted.clone(),
                            record_kind: None,
                            lease: None,
                            _hold: None,
                        });
                    let legacy = client
                        .subscribe_invocations_request()
                        .send()
                        .promise
                        .await
                        .unwrap();
                    let _legacy_stream = legacy.get().unwrap().get_stream().unwrap();
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .unwrap_err()
                            .contains("stable_identity_unavailable")
                    );
                    let mut subscription = client.subscribe_invocations_request();
                    subscription.get().set_stable_invocation_ids(true);
                    let capable = subscription.send().promise.await.unwrap();
                    let stream = capable.get().unwrap().get_stream().unwrap();
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    let reply = stream.read_request().send().promise.await.unwrap();
                    assert_eq!(
                        reply.get().unwrap().get_invocation_uuid().unwrap(),
                        request.invocation.0.as_bytes()
                    );
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_invalid_updates_never_persist_and_legacy_completion_still_works()
         {
            tokio::task::LocalSet::new().run_until(async {
                let root = tempfile::tempdir().unwrap();
                let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                let channel = fixture.invocation_channel();
                let admitted = fixture.state.start_verified_invocation(&caller, &request, None).unwrap();
                fixture.invocation_rx.try_recv().unwrap();
                let foreign: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                    person_chat_views: RefCell::default(),
                    committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
                    state: fixture.state.clone(), directories: fixture.directories.clone(),
                    channel_id: fixture.route.channel_key(), peer_id: 999, connection_id: 999,
                    verified_identity: caller.admitted.clone(),
                    record_kind: None,
                    lease: None, _hold: None,
                });
                assert!(update_app(&foreign, admitted.wire_id, &tilo_actions::InvocationUpdate::Succeeded {
                    invocation: request.invocation, output: tilo_actions::ActionOutput::default(),
                }).await.is_err());
                let invalid = [
                    tilo_actions::InvocationUpdate::Accepted { invocation: request.invocation },
                    tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: String::new(), completed: None, total: None, message: None },
                    tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: "working".into(), completed: Some(2), total: Some(1), message: None },
                    tilo_actions::InvocationUpdate::Item { invocation: request.invocation, sequence: 1, value: serde_json::json!({"private":"value"}) },
                    tilo_actions::InvocationUpdate::Failed { invocation: request.invocation, error: tilo_actions::ActionFailure { code: "bad".into(), message: "failed".into(), details: None, retryable: true } },
                    tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput { value: Some(serde_json::json!("undeclared")), resources: vec![] } },
                ];
                for update in invalid {
                    assert!(update_app(&channel, admitted.wire_id, &update).await.is_err());
                    assert!(matches!(fixture.state.verified_invocation_status(&caller, request.invocation).unwrap().unwrap().update, tilo_actions::InvocationUpdate::Accepted { .. }));
                }
                let mut complete = channel.complete_invocation_request();
                complete.get().set_id(admitted.wire_id);
                complete.get().set_code(""); complete.get().set_message("");
                complete.send().promise.await.unwrap();
                assert!(matches!(fixture.state.verified_invocation_status(&caller, request.invocation).unwrap().unwrap().update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == request.invocation));

                // The legacy numeric local caller still receives and completes its original handle.
                let legacy = fixture.state.start_invocation(71, &request.resource, &request.action, None, &caller.admitted.principal_id).unwrap();
                let delivered = fixture.invocation_rx.try_recv().unwrap();
                assert_eq!(delivered.invocation.0.as_u128(), u128::from(legacy.id));
                let mut complete = channel.complete_invocation_request();
                complete.get().set_id(legacy.id); complete.get().set_code(""); complete.get().set_message("");
                complete.send().promise.await.unwrap();
                assert!(legacy.completion.recv().unwrap().code.is_empty());
            }).await;
        }

        /// The broker answers every application and peer on one thread. A
        /// completion waiting for its journal write, as on a loaded disk whose
        /// syncs take seconds, must leave that thread answering everything
        /// else, and nobody may hear of the outcome before it is durable.
        #[tokio::test(flavor = "current_thread")]
        async fn a_completion_waiting_for_a_slow_disk_leaves_the_broker_loop_answering() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let root = tempfile::tempdir().unwrap();
                    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                    let channel = fixture.invocation_channel();
                    let admitted = fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    fixture.invocation_rx.try_recv().unwrap();
                    // The disk: each journal write waits until the test lets it go.
                    let (writing, at_write) = std::sync::mpsc::channel();
                    let (release, released) = std::sync::mpsc::channel::<()>();
                    let released = Mutex::new(released);
                    fixture
                        .state
                        .journal
                        .lock()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .hold_writes = Some(Arc::new(move || {
                        let _ = writing.send(());
                        let _ = released
                            .lock()
                            .unwrap()
                            .recv_timeout(tilo_test_env::hang_bound());
                    }));
                    let mut complete = channel.complete_invocation_request();
                    complete.get().set_id(admitted.wire_id);
                    complete.get().set_code("");
                    complete.get().set_message("");
                    let completing = tokio::task::spawn_local(complete.send().promise);
                    tilo_test_env::within("the completion to reach its journal write", async {
                        while at_write.try_recv().is_err() {
                            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        }
                    })
                    .await;
                    // Another request the loop answers, as it answers an app.
                    let mut asked = channel.invocation_cancellation_request();
                    asked.get().set_id(admitted.wire_id);
                    tilo_test_env::within(
                        "the loop to answer while the completion writes",
                        asked.send().promise,
                    )
                    .await
                    .unwrap();
                    assert!(
                        !completing.is_finished(),
                        "the answer came only once the completion had written"
                    );
                    assert!(
                        matches!(
                            fixture
                                .state
                                .verified_invocation_status(&caller, request.invocation)
                                .unwrap()
                                .unwrap()
                                .update,
                            tilo_actions::InvocationUpdate::Accepted { .. }
                        ),
                        "nobody hears of the outcome before it is durable"
                    );
                    release.send(()).unwrap();
                    tilo_test_env::within(
                        "the completion to finish once its write goes",
                        completing,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert!(matches!(
                        fixture
                            .state
                            .verified_invocation_status(&caller, request.invocation)
                            .unwrap()
                            .unwrap()
                            .update,
                        tilo_actions::InvocationUpdate::Succeeded { .. }
                    ));
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_immediate_progress_and_malformed_stream_items_are_rejected() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    for streaming in [false, true] {
                        let root = tempfile::tempdir().unwrap();
                        let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                        let channel = fixture.invocation_channel();
                        fixture
                            .state
                            .surfaces
                            .lock()
                            .unwrap()
                            .get_mut(&71)
                            .unwrap()
                            .actions[0]
                            .contract
                            .execution = if streaming {
                            tilo_actions::ExecutionMode::Streaming {
                                cancellable: true,
                                item_schema: serde_json::json!({"type":"integer"}),
                            }
                        } else {
                            tilo_actions::ExecutionMode::Immediate
                        };
                        if !streaming {
                            fixture
                                .state
                                .surfaces
                                .lock()
                                .unwrap()
                                .get_mut(&71)
                                .unwrap()
                                .actions[0]
                                .contract
                                .result = tilo_actions::ResultSpec::None;
                        }
                        let admitted = fixture
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .unwrap();
                        fixture.invocation_rx.try_recv().unwrap();
                        let bad = if streaming {
                            tilo_actions::InvocationUpdate::Item {
                                invocation: request.invocation,
                                sequence: 1,
                                value: serde_json::json!("wrong type"),
                            }
                        } else {
                            tilo_actions::InvocationUpdate::Progress {
                                invocation: request.invocation,
                                sequence: 1,
                                phase: "working".into(),
                                completed: None,
                                total: None,
                                message: None,
                            }
                        };
                        assert!(update_app(&channel, admitted.wire_id, &bad).await.is_err());
                        assert!(matches!(
                            fixture
                                .state
                                .verified_invocation_status(&caller, request.invocation)
                                .unwrap()
                                .unwrap()
                                .update,
                            tilo_actions::InvocationUpdate::Accepted { .. }
                        ));
                        if streaming {
                            let item = tilo_actions::InvocationUpdate::Item {
                                invocation: request.invocation,
                                sequence: 1,
                                value: serde_json::json!(7),
                            };
                            update_app(&channel, admitted.wire_id, &item).await.unwrap();
                            assert!(update_app(&channel, admitted.wire_id, &item).await.is_err());
                            assert_eq!(
                                fixture
                                    .state
                                    .verified_invocation_status(&caller, request.invocation)
                                    .unwrap()
                                    .unwrap()
                                    .update,
                                item
                            );
                        }
                    }
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_confirmation_uuid_and_lifecycle_remain_authoritative() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let root = tempfile::tempdir().unwrap();
                    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                    let channel = fixture.invocation_channel();
                    fixture
                        .state
                        .surfaces
                        .lock()
                        .unwrap()
                        .get_mut(&71)
                        .unwrap()
                        .actions[0]
                        .contract
                        .confirmation = tilo_actions::ConfirmationPolicy::Phrase {
                        phrase: "erase exactly".into(),
                    };
                    let actor = &caller.admitted.principal_id;
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .is_err()
                    );
                    assert!(
                        fixture
                            .state
                            .prepare_confirmation(
                                71,
                                &request.resource,
                                &request.action,
                                None,
                                actor,
                                Some("erase")
                            )
                            .is_err()
                    );
                    let token = fixture
                        .state
                        .prepare_confirmation(
                            71,
                            &request.resource,
                            &request.action,
                            None,
                            actor,
                            Some("erase exactly"),
                        )
                        .unwrap();
                    let admitted = fixture
                        .state
                        .start_verified_invocation(&caller, &request, Some(&token))
                        .unwrap();
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    let delivered = fixture.invocation_rx.try_recv().unwrap();
                    assert_eq!(delivered.invocation, request.invocation);
                    let mut new = request.clone();
                    new.invocation = tilo_actions::InvocationId::new();
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &new, Some(&token))
                            .is_err()
                    );
                    let progress = tilo_actions::InvocationUpdate::Progress {
                        invocation: request.invocation,
                        sequence: 1,
                        phase: "working".into(),
                        completed: Some(0),
                        total: Some(1),
                        message: None,
                    };
                    update_app(&channel, delivered.id, &progress).await.unwrap();
                    assert!(update_app(&channel, delivered.id, &progress).await.is_err());
                    assert!(
                        update_app(
                            &channel,
                            delivered.id,
                            &tilo_actions::InvocationUpdate::Succeeded {
                                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
                                    u128::from(delivered.id)
                                )),
                                output: tilo_actions::ActionOutput::default()
                            }
                        )
                        .await
                        .is_err()
                    );
                    fixture
                        .state
                        .cancel_verified_invocation(&caller, request.invocation)
                        .await
                        .unwrap();
                    assert!(
                        !fixture
                            .state
                            .verified_invocation_status(&caller, request.invocation)
                            .unwrap()
                            .unwrap()
                            .update
                            .is_terminal(),
                        "request is not acknowledgement"
                    );
                    let cancelled = tilo_actions::InvocationUpdate::Cancelled {
                        invocation: request.invocation,
                        message: None,
                    };
                    update_app(&channel, delivered.id, &cancelled)
                        .await
                        .unwrap();
                    assert_eq!(
                        fixture
                            .state
                            .verified_invocation_status(&caller, request.invocation)
                            .unwrap()
                            .unwrap()
                            .update,
                        cancelled
                    );
                    assert_eq!(admitted.wire_id, delivered.id);
                })
                .await;
        }
    }
}

/// Checks one application-reported update against its pending invocation's
/// published contract, returning the sequence it advances to.
fn validate_invocation_update(
    pending: &PendingInvocation,
    update: &tilo_actions::InvocationUpdate,
) -> Result<u64, capnp::Error> {
    if update.invocation() != pending.update.invocation() {
        return Err(capnp::Error::failed(
            "invocation update identity mismatch".into(),
        ));
    }
    let mut next_sequence = pending.last_sequence;
    if matches!(update, tilo_actions::InvocationUpdate::Accepted { .. }) {
        return Err(capnp::Error::failed(
            "invocation acceptance is owned by the broker".into(),
        ));
    }
    if let tilo_actions::InvocationUpdate::Progress {
        sequence,
        phase,
        completed,
        total,
        ..
    } = update
    {
        if matches!(pending.execution, tilo_actions::ExecutionMode::Immediate) {
            return Err(capnp::Error::failed(
                "immediate action reported background progress".into(),
            ));
        }
        if *sequence <= pending.last_sequence {
            return Err(capnp::Error::failed(
                "invocation progress must be monotonic".into(),
            ));
        }
        if phase.trim().is_empty() {
            return Err(capnp::Error::failed(
                "invocation progress phase must be non-empty".into(),
            ));
        }
        if completed
            .zip(*total)
            .is_some_and(|(completed, total)| completed > total)
        {
            return Err(capnp::Error::failed(
                "invocation progress cannot exceed its total".into(),
            ));
        }
        next_sequence = *sequence;
    }
    if let tilo_actions::InvocationUpdate::Item {
        sequence, value, ..
    } = update
    {
        let Some(item_schema) = pending.execution.item_schema() else {
            return Err(capnp::Error::failed(
                "non-streaming action reported a streamed item".into(),
            ));
        };
        if *sequence <= pending.last_sequence {
            return Err(capnp::Error::failed(
                "invocation stream sequence must be monotonic".into(),
            ));
        }
        tilo_actions::validate_json_schema(value, item_schema).map_err(|error| {
            capnp::Error::failed(format!(
                "streamed item violated its published contract: {error}"
            ))
        })?;
        next_sequence = *sequence;
    }
    if let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update {
        pending.result.validate(output).map_err(|error| {
            capnp::Error::failed(format!(
                "invocation result violated its published contract: {error}"
            ))
        })?;
    }
    if let tilo_actions::InvocationUpdate::Failed { error, .. } = update {
        if error.code.trim().is_empty() || error.message.trim().is_empty() {
            return Err(capnp::Error::failed(
                "invocation failure requires a code and message".into(),
            ));
        }
        if error.retryable && !pending.retryable_on_disconnect {
            return Err(capnp::Error::failed(
                "at-most-once action cannot advertise a retryable failure".into(),
            ));
        }
    }
    if matches!(update, tilo_actions::InvocationUpdate::Cancelled { .. }) && !pending.cancellable {
        return Err(capnp::Error::failed(
            "non-cancellable action reported cancellation".into(),
        ));
    }
    Ok(next_sequence)
}

impl CapnpBrokerState {
    fn publish_pending_activity(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) {
        if pending.cancellation_requested_at.is_some() && !update.is_terminal() {
            self.execution_activity.publish_status(
                pending.activity.as_ref(),
                ExecutionStatus::CancellationRequested,
            );
        } else {
            self.publish_activity(pending.activity.as_ref(), update);
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the pending entry borrows the registry until it is staged; the lint misreads the borrow"
    )]
    async fn apply_invocation_update(
        self: &Arc<Self>,
        peer_id: u64,
        id: u64,
        update: tilo_actions::InvocationUpdate,
        identity: Option<(CrdtRoute, AdmittedJoinIdentity)>,
    ) -> Result<(), capnp::Error> {
        let state = self;
        let host_guard = identity
            .as_ref()
            .and_then(|(_, identity)| HostWorkerReadGuard::for_channel(state, peer_id, identity));
        // Copy only the local routing key before awaiting; never hold the registry lock across the owner turn.
        let owner_room = state
            .pending_invocations
            .lock()
            .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?
            .get(&id)
            .map(|pending| pending.cluster_id.clone());
        let requires_owner = state.durable_owner_required
            || owner_room
                .as_ref()
                .is_some_and(|room| state.publisher.requires_owner_for(room));
        let _owner_order = if requires_owner {
            Some(state.owner_commit_order.lock().await)
        } else {
            None
        };
        HostWorkerReadGuard::check(host_guard.as_ref())?;
        if requires_owner && let Some((route, identity)) = &identity {
            state.revalidate_admission(route, identity)?;
        }
        let (staged, next_sequence) = {
            let registry = state
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
            let pending = registry
                .get(&id)
                .filter(|pending| pending.peer_id == peer_id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            let next_sequence = validate_invocation_update(pending, &update)?;
            (pending.clone(), next_sequence)
        };
        state
            .commit_effect_before_terminal(&staged, &update)
            .await?;
        HostWorkerReadGuard::check(host_guard.as_ref())?;
        if requires_owner && let Some((route, identity)) = &identity {
            state.revalidate_admission(route, identity)?;
        }
        state
            .settle_definite_outcome(peer_id, id, &staged.update, &update)
            .await?;
        let mut registry = state
            .pending_invocations
            .lock()
            .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
        let pending = registry
            .get_mut(&id)
            .filter(|pending| pending.peer_id == peer_id && pending.update == staged.update)
            .ok_or_else(|| {
                capnp::Error::failed("invocation changed while committing; outcome unknown".into())
            })?;
        pending.last_activity = std::time::Instant::now();
        pending.last_sequence = next_sequence;
        pending.update = update.clone();
        state.publish_pending_activity(pending, &update);
        state.invocation_changed();
        if update.is_terminal() {
            let completion = match &update {
                tilo_actions::InvocationUpdate::Succeeded { .. } => InvocationCompletion {
                    code: String::new(),
                    message: String::new(),
                },
                tilo_actions::InvocationUpdate::Failed { error, .. } => InvocationCompletion {
                    code: error.code.clone(),
                    message: error.message.clone(),
                },
                tilo_actions::InvocationUpdate::Cancelled { message, .. } => InvocationCompletion {
                    code: "action_cancelled".into(),
                    message: message.clone().unwrap_or_default(),
                },
                _ => {
                    return Err(capnp::Error::failed(
                        "nonterminal invocation reached terminal handling".into(),
                    ));
                }
            };
            let pending = registry
                .remove(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            // Retire it before releasing the registry: a waiter woken by the
            // change above must find it pending or completed, never neither.
            state.record_completion(id, pending.completion_owner(), update.clone());
            drop(registry);
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            state.record_effect_receipt(&pending, &update);
        }
        Ok(())
    }
}

#[cfg(test)]
mod host_worker_capacity_tests {
    #![allow(
        clippy::significant_drop_tightening,
        reason = "each test keeps its broker state for the whole scenario"
    )]
    use super::*;

    fn state() -> Arc<CapnpBrokerState> {
        let fanout = Arc::new(FanoutState::new());
        CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        )
        .0
    }
    fn host_worker(record: usize) -> AdmittedJoinIdentity {
        let root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [92; 32],
            device_label: "host".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
        AdmittedJoinIdentity {
            host_scope: Some(LocalHostWorkerScope {
                route: CrdtRoute::from_channel_key(&format!(
                    "{}#app:so.tilo.notes#record:tab:{record:016x}",
                    uuid::Uuid::from_u128(7)
                )),
                certificate: certificate.clone(),
            }),
            worker_registration: None,
            certificate: Some(certificate),
            verified: VerifiedJoinIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: [92; 32],
                capped: false,
            },
            principal_id: "host".into(),
            display_handle: None,
            role: CapRole::Owner,
            agent_id: None,
            session_delegated: false,
        }
    }
    fn retained(state: &CapnpBrokerState) -> usize {
        state.host_workers.lock().unwrap().len()
    }
    fn retain(state: &CapnpBrokerState, peer: u64, record: usize) -> Result<(), capnp::Error> {
        let worker = host_worker(record);
        let route = worker.host_scope.clone().unwrap().route;
        state.retain_host_worker(peer, &route, &worker)
    }

    /// Repeated worker mounts (connect, then drop every connection) never
    /// grow the provenance registry: each disconnect withdraws its entry.
    #[test]
    fn host_worker_connect_disconnect_cycles_do_not_grow_the_registry() {
        let state = state();
        let mut peer = 0u64;
        for _cycle in 0..20 {
            let first = peer;
            for record in 0..40 {
                peer += 1;
                retain(&state, peer, record).unwrap();
            }
            assert_eq!(retained(&state), 40);
            for connection in first + 1..=peer {
                state.remove_connection(connection);
            }
            assert_eq!(retained(&state), 0);
        }
    }

    /// Workers are counted per connection, never across them: a Cloud host
    /// mounting a worker per record for every app and agent, far past any
    /// former global bound, keeps every one, and closing one withdraws only
    /// its own.
    #[test]
    fn host_workers_are_owned_by_their_connections_not_a_global_count() {
        let state = state();
        let agents = 16;
        let records = 8 * 64; // eight hosted apps with 64 records each
        for peer in 0..(agents + records) {
            retain(&state, peer as u64 + 1, peer).unwrap();
        }
        assert_eq!(retained(&state), agents + records);
        // A reconnect of an already admitted peer with the same scope is idempotent.
        assert!(retain(&state, 1, 0).is_ok());
        assert_eq!(retained(&state), agents + records);
        state.remove_connection(1);
        assert_eq!(retained(&state), agents + records - 1);
        assert!(retain(&state, (agents + records) as u64 + 1, agents + records).is_ok());
    }
}

#[cfg(test)]
#[path = "collab_broker_capnp/resource_view_tests.rs"]
mod resource_view_tests;

#[cfg(test)]
mod person_record_join_tests {
    use super::*;

    fn route(app: &str, record: &str) -> CrdtRoute {
        CrdtRoute {
            app_id: app.into(),
            cluster_id: collab_types::person_room(&[7; 32]).to_string(),
            record_id: record.into(),
        }
    }

    /// An app joins the person's room for the records of theirs its surface
    /// declares, its own or another app's it reads, and for nothing else.
    #[test]
    fn an_app_joins_the_persons_room_only_for_the_records_it_declares() {
        static TERMINAL: &[u8] = b"[[resources]]\nkind = \"test-person.session\"\nlabel = \"Session\"\nversion = 1\n\n[[resources]]\nkind = \"test-person.history\"\nlabel = \"History\"\nperson = true\nversion = 1\n\n[[resources]]\nkind = \"test-person.accounts\"\nlabel = \"Accounts\"\nperson = true\nversion = 1\n";
        ui::app_surface::register_package("test.person-terminal", TERMINAL);
        let history = collab_types::person_record_id("test-person.history");
        let accounts = collab_types::person_record_id("test-person.accounts");
        assert_eq!(
            person_record_join(
                "test.person-terminal",
                &route("test.person-terminal", &history)
            ),
            Ok(())
        );
        assert_eq!(
            person_record_join(
                "test.person-terminal",
                &route("test.person-settings", &accounts)
            ),
            Ok(()),
            "another app's record of the person's it declares, to read"
        );
        let undeclared = collab_types::person_record_id("test-person.bookmarks");
        assert!(
            person_record_join(
                "test.person-terminal",
                &route("test.person-terminal", &undeclared)
            )
            .is_err()
        );
        assert!(
            person_record_join(
                "test.person-terminal",
                &route("test.person-terminal", "tab:01")
            )
            .is_err(),
            "nothing but the person's records"
        );
        assert!(
            person_record_join(
                "test.person-notes",
                &route("test.person-terminal", &history)
            )
            .is_err(),
            "an app that declares none of them is refused"
        );
    }
}
