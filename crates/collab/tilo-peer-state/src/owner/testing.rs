//! Fixtures and internals for owner tests, the desktop session's included.
#![allow(
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the test failing"
)]
use crate::owner::{
    NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner,
    VerifiedNativeEnrollmentIntent, WorkspaceId, publication,
};
use crate::owner_commit::OwnerCommitRequest;
use crate::{
    SeedDocument, SeedState, StatePeer, TransportOrigin, join, seed_workspace, workspace::media,
};
use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::{collections::BTreeMap, time::SystemTime};
use tilo_automation::Shortcut;
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

pub mod governor;

/// The record at `path` as the disk holds it: the index and one file per
/// workspace, sorted by path.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn record_on_disk(path: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    crate::owner::file_store::record_files(path).unwrap()
}

pub const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
pub const APP: &str = "so.tilo.chat";

/// Where `certificate`'s device keeps a workspace it founds on itself:
/// `workspace`'s own host, reached at `endpoint`, signed by the device.
pub fn own_host_hint(
    workspace: WorkspaceId,
    endpoint: iroh_base::EndpointAddr,
    certificate: &DeviceCert,
    key: &SigningKey,
) -> join::TransportHint {
    let room = workspace.to_string();
    IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        workspace_id: join::room_workspace_id(&room).unwrap(),
        room,
        endpoint_addr: endpoint,
        host_user_public_key: certificate.user_public_key,
        host_device_certificate: certificate.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(key)
    .unwrap()
}

/// [`own_host_hint`] for a device reached at its own endpoint id.
pub fn own_host_hint_here(
    workspace: WorkspaceId,
    certificate: &DeviceCert,
    key: &SigningKey,
) -> join::TransportHint {
    own_host_hint(
        workspace,
        iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&certificate.device_public_key).unwrap(),
        ),
        certificate,
        key,
    )
}

/// Found `name` on `owner` as `certificate`'s device, naming its own host.
pub fn founded(
    owner: &NativeWorkspaceOwner,
    name: &str,
    certificate: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> Result<WorkspaceId, String> {
    let workspace = WorkspaceId::new();
    owner.create_workspace(
        workspace,
        name,
        certificate,
        key,
        Some(own_host_hint_here(workspace, certificate, key)),
        now,
    )?;
    Ok(workspace)
}

/// Enroll an empty interactive workspace through the real signed join protocol.
/// This helper neither seeds application records nor grants capabilities.
pub fn enroll_interactive_governor(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    certificate: &DeviceCert,
) -> Result<governor::TestGovernor, String> {
    enroll_interactive_governor_in(owner, key, certificate, WORKSPACE)
}

/// [`enroll_interactive_governor`] into `workspace`.
pub fn enroll_interactive_governor_in(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    certificate: &DeviceCert,
    workspace: &str,
) -> Result<governor::TestGovernor, String> {
    let governor =
        governor::TestGovernor::empty_interactive(certificate.clone(), key.clone(), workspace)?;
    enroll_into(governor, owner, key, certificate)
}

/// Enroll `certificate`'s device into the workspace `governor` already holds, as
/// the workspace's devices [`enroll_interactive_governor`] make one for: what a
/// workspace whose other devices are already in it admits this one into.
pub fn enroll_into(
    mut governor: governor::TestGovernor,
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    certificate: &DeviceCert,
) -> Result<governor::TestGovernor, String> {
    let endpoint = iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&certificate.device_public_key).unwrap(),
    );
    let hint = governor.hint(endpoint);
    let now = SystemTime::now();
    let now_ns = ns(now).unwrap();
    // One workspace admits more than one device: each asks with an id of its own.
    let mut request_id = [0_u8; 32];
    request_id[..8].copy_from_slice(&certificate.device_public_key[..8]);
    let invitation = governor.issue(
        vec![hint.clone()],
        join::MemberRole::Editor,
        Some(certificate.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(certificate.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let response = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id,
        }),
    );
    let join::JoinTransportResponse::Challenge(response) = response else {
        return Err("actual governor challenge refused".into());
    };
    let complete = join::JoinCompleteRequest {
        proof: join::sign_join_proof(response.challenge, key).unwrap(),
    };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        certificate.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let response = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Complete(complete),
    );
    let join::JoinTransportResponse::Complete(bootstrap) = response else {
        return Err("actual governor enrollment refused".into());
    };
    if !bootstrap.records.is_empty() {
        return Err("empty governor supplied seeded application records".into());
    }
    owner
        .install(&finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    Ok(governor)
}
pub fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    certificate_with(root, key, label, DeviceCapabilities::primary())
}

/// `key`'s ordinary member certificate for `root`'s person.
pub fn member_certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    certificate_with(root, key, label, DeviceCapabilities::member())
}

/// `key`'s certificate for `root`'s person, with `capabilities` and no expiry.
pub fn certificate_with(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

pub fn raw_fixture() -> (
    VerifiedNativeEnrollmentIntent,
    join::JoinBootstrap,
    Vec<NeighbourMsg>,
    [u8; 32],
    SystemTime,
) {
    raw_fixture_at(SystemTime::now())
}
/// The joining device's own key the raw fixture enrolls.
#[must_use]
pub fn member_key() -> SigningKey {
    SigningKey::from_bytes(&[63; 32])
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear signed-enrollment fixture; splitting it would scatter the protocol \
              steps each test depends on"
)]
pub fn raw_fixture_at(
    now: SystemTime,
) -> (
    VerifiedNativeEnrollmentIntent,
    join::JoinBootstrap,
    Vec<NeighbourMsg>,
    [u8; 32],
    SystemTime,
) {
    let ns = u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let governor = certificate(
        &SigningKey::from_bytes(&[62; 32]),
        &governor_key,
        "Governor",
    );
    let device_key = member_key();
    let device = certificate(&SigningKey::from_bytes(&[64; 32]), &device_key, "Member");
    let workspace_id = tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: WORKSPACE.into(),
        workspace_id,
        endpoint_addr: iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&governor.device_public_key).unwrap(),
        ),
        host_user_public_key: governor.user_public_key,
        host_device_certificate: governor.clone(),
        packet_relay_url: "https://relay.invalid".into(),
    }
    .transport_hint(&governor_key)
    .unwrap();
    let signed = join::issue_invitation(
        join::InviteGrant {
            workspace_id,
            room: WORKSPACE.into(),
            issuer: governor.clone(),
            role: join::MemberRole::Editor,
            nonce: [65; 16],
            recipient_user_public_key: Some(device.user_public_key),
            invitee_name: Some("Member".into()),
            expires_at_ns: ns.checked_add(60_000_000_000).unwrap(),
            transport: vec![hint.clone()],
        },
        &governor_key,
    )
    .unwrap();
    let verified = join::decode_and_verify_invitation(
        &signed,
        Some(device.user_public_key),
        &RevocationList::new(),
        ns,
    )
    .unwrap();
    let challenge = join::JoinChallenge::new(
        &verified,
        &device,
        [66; 32],
        [67; 32],
        ns.checked_add(30_000_000_000).unwrap(),
        ns,
    )
    .unwrap();
    let proof = join::sign_join_proof(challenge.clone(), &device_key).unwrap();
    let redemption =
        join::verify_join_proof(&proof, &challenge, &device, &RevocationList::new(), ns).unwrap();
    let mut peer = StatePeer::new(
        WORKSPACE,
        governor.clone(),
        vec![governor.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let channel = {
        let entry = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
        chat.put_object(&entry, "v", ObjType::Map).unwrap()
    };
    chat.put(&channel, "kind", "voice").unwrap();
    chat.put(&channel, "workspace", "home").unwrap();
    collab_types::CommitNamed::commit_edit(&mut chat, "chat", "channels");
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(
                WORKSPACE,
                &governor,
                std::slice::from_ref(&governor),
            )
            .unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: String::new(),
                kind: "chat".into(),
                bytes: chat.save(),
            }],
        },
        &governor_key,
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, &governor_key, now)
            .unwrap()
    );
    let policy = media::WorkspaceMediaV1 {
        version: 1,
        workspace: *uuid::Uuid::parse_str(WORKSPACE).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(
            peer.admitted_workspace().unwrap(),
            &peer.revocations(),
        )
        .unwrap(),
        calls: BTreeMap::from([(
            [68; 32],
            media::CallV1 {
                binding: peer
                    .media()
                    .resolve_media_binding(APP, "", "voice")
                    .unwrap(),
                epoch: 1,
                phase: media::CallPhaseV1::Active,
                allowed: media::DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::new(),
            },
        )]),
    };
    peer.media_mut()
        .author_media_policy(&policy, &governor_key, now)
        .unwrap();
    let messages = peer
        .publication_mut()
        .durable_bootstrap_messages(&governor_key, now, 1)
        .unwrap();
    assert!(peer.validated_media_policy().unwrap().is_some());
    let bootstrap = join::JoinBootstrap {
        version: 1,
        invite_id: verified.invite_id(),
        device_cert: device.clone(),
        role: join::MemberRole::Editor,
        room: WORKSPACE.into(),
        workspace_id,
        host: governor.clone(),
        certified_roster: peer.certificates(),
        revocations: peer.revocations(),
        records: peer.rows(),
        governance_messages: vec![postcard::to_stdvec(&messages[0]).unwrap()],
        generation: 1,
    };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        join::JoinCompleteRequest { proof },
        device,
        hint,
        now,
    )
    .unwrap();
    (intent, bootstrap, messages, governor.device_public_key, now)
}
pub fn finalize(
    intent: VerifiedNativeEnrollmentIntent,
    pending: crate::owner::PendingEnrollmentId,
    bootstrap: join::JoinBootstrap,
) -> Result<crate::owner::VerifiedNativeEnrollment, String> {
    crate::owner::VerifiedNativeEnrollment::finalize(intent, pending, bootstrap)
}
/// An enrollment attempt into `workspace` at `generation` that no store
/// published, for a test naming another attempt than the retained one.
#[must_use]
pub fn pending_id(
    workspace: crate::owner::WorkspaceId,
    generation: u64,
) -> crate::owner::PendingEnrollmentId {
    crate::owner::PendingEnrollmentId::new(workspace, generation, [0; 32])
}
/// # Errors
/// `now` is before the epoch or beyond `u64` nanoseconds.
pub fn ns(now: SystemTime) -> Result<u64, String> {
    crate::owner::enrollment::ns(now)
}
/// The exact bytes of every original outgoing entry.
pub fn entries(view: &NativeCommittedView) -> Result<Vec<Vec<u8>>, String> {
    Ok(publication::state(view)?
        .entries
        .iter()
        .map(|entry| entry.bytes.clone())
        .collect())
}
pub fn prepare(request: &OwnerCommitRequest) -> Result<Vec<u8>, String> {
    publication::prepare(request)
}
pub fn request(message: &NeighbourMsg) -> Result<OwnerCommitRequest, String> {
    publication::request(message)
}
/// `(timestamp_ns, channel, incremental)` as the owner records them.
pub fn metadata(message: &NeighbourMsg) -> Result<(u64, [u8; 32], bool), String> {
    publication::metadata(message)
}
pub fn append_local(view: &mut NativeCommittedView, bytes: Vec<u8>) -> Result<(), String> {
    publication::append_local(view, bytes)
}
/// Record the live peer as the view's durable peer.
pub fn seal(view: &mut NativeCommittedView) -> Result<(), String> {
    view.record.peer = view.peer.durable()?;
    Ok(())
}
pub fn bump_generation(view: &mut NativeCommittedView) {
    view.record.generation = view.record.generation.saturating_add(1);
}
/// What a record keeps of one workspace.
pub struct Stored {
    pub peer: Vec<u8>,
    pub governor: DeviceCert,
    pub device: DeviceCert,
    pub projection: Vec<Vec<u8>>,
}
#[must_use]
pub fn stored(storage: &NativeAuthorityStorage, workspace: WorkspaceId) -> Option<Stored> {
    let record = storage.workspaces.get(&workspace)?;
    Some(Stored {
        peer: record.peer.clone(),
        governor: record.workspace_host.clone(),
        device: record.device.clone(),
        projection: record
            .publication
            .as_ref()?
            .projection
            .iter()
            .map(|entry| entry.bytes.clone())
            .collect(),
    })
}

/// The instant these fixtures write at, so every message two peers exchange
/// carries the same clock.
#[must_use]
pub fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(2000))
        .unwrap()
}

/// `message` arriving from `guest` as a relay delta at [`now`].
pub fn receive(
    peer: &mut StatePeer,
    guest: &DeviceCert,
    message: NeighbourMsg,
) -> Result<(), String> {
    peer.receive_from(
        TransportOrigin::RelayDelta(guest.device_public_key),
        message,
        now(),
    )
    .map(|_| ())
}

/// A shortcut named after its id, with nothing else set.
#[must_use]
pub fn shortcut(id: &str) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: Vec::new(),
        steps: Vec::new(),
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    }
}
