//! Fixtures and internals for owner tests, the desktop session's included.
#![allow(
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the test failing"
)]
use crate::{
    NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, RoomId, Store,
    VerifiedNativeEnrollmentIntent, publication,
};
use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_owner_commit::OwnerCommitRequest;
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, join, room::media, seed_room};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

pub mod governor;

pub const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
pub const APP: &str = "so.tilo.chat";

/// Enroll an empty interactive room through the real signed join protocol.
/// This helper neither seeds application records nor grants capabilities.
///
/// # Errors
/// Returns a refusal when the genuine governor challenge or completion fails.
pub fn enroll_interactive_governor(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    certificate: &DeviceCert,
) -> Result<governor::TestGovernor, String> {
    let mut governor =
        governor::TestGovernor::empty_interactive(certificate.clone(), key.clone(), ROOM).unwrap();
    let endpoint = iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&certificate.device_public_key).unwrap(),
    );
    let hint = governor.hint(endpoint);
    let now = SystemTime::now();
    let now_ns = ns(now).unwrap();
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
            request_id: [95; 32],
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
/// Most outgoing entries one room holds.
pub const MAX_OUTGOING_ENTRIES: usize = publication::MAX_ENTRIES;
/// Largest outgoing publication one room holds.
pub const MAX_OUTGOING_BYTES: usize = publication::MAX_BYTES;

pub fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
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
    let device_key = SigningKey::from_bytes(&[63; 32]);
    let device = certificate(&SigningKey::from_bytes(&[64; 32]), &device_key, "Member");
    let host_key = SigningKey::from_bytes(&[69; 32]);
    let mut host = certificate(
        &SigningKey::from_bytes(&[62; 32]),
        &host_key,
        "Delivery host",
    );
    host.capabilities.can_act_as_host = true;
    sign_device_cert(&mut host, &SigningKey::from_bytes(&[62; 32])).unwrap();
    let locator = tilo_delivery_protocol::workspace_join::DeliveryOriginLocatorV1 {
        version: 1,
        origin: "https://native.example".into(),
    }
    .transport_hint(&host, &host_key)
    .unwrap();
    let workspace_id = tilo_relay_protocol::room_workspace_id(ROOM).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: ROOM.into(),
        workspace_id,
        governing_authority: governor.clone(),
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
            room: ROOM.into(),
            authority: governor.clone(),
            issuer: governor.clone(),
            delegation: None,
            role: join::MemberRole::Editor,
            nonce: [65; 16],
            recipient_user_public_key: Some(device.user_public_key),
            invitee_name: Some("Member".into()),
            expires_at_ns: ns.checked_add(60_000_000_000).unwrap(),
            transport: vec![hint.clone(), locator],
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
        ROOM,
        governor.clone(),
        vec![governor.clone(), host.clone()],
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
            room_changes: seed_room(ROOM, &governor, &[governor.clone(), host.clone()]).unwrap(),
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
    let policy = media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(ROOM).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(
            peer.admitted_room().unwrap(),
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
        .durable_bootstrap_messages(&governor_key, now, 1)
        .unwrap();
    assert!(peer.validated_media_policy().unwrap().is_some());
    let bootstrap = join::JoinBootstrap {
        version: 1,
        invite_id: verified.invite_id(),
        device_cert: device.clone(),
        role: join::MemberRole::Editor,
        room: ROOM.into(),
        workspace_id,
        authority: governor.clone(),
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
/// # Errors
/// As the enrollment a pinned host's Complete response finalizes.
pub fn finalize(
    intent: VerifiedNativeEnrollmentIntent,
    pending: crate::PendingEnrollmentId,
    bootstrap: join::JoinBootstrap,
) -> Result<crate::VerifiedNativeEnrollment, String> {
    crate::VerifiedNativeEnrollment::finalize(intent, pending, bootstrap)
}
/// An enrollment attempt into `room` at `generation` that no store
/// published, for a test naming another attempt than the retained one.
#[must_use]
pub fn pending_id(room: crate::RoomId, generation: u64) -> crate::PendingEnrollmentId {
    crate::PendingEnrollmentId::new(room, generation, [0; 32])
}
/// The store an owner publishes through, for a host test to downcast.
#[must_use]
pub fn store(owner: &NativeWorkspaceOwner) -> Arc<dyn Store> {
    Arc::clone(&owner.0.store)
}
/// # Errors
/// `now` is before the epoch or beyond `u64` nanoseconds.
pub fn ns(now: SystemTime) -> Result<u64, String> {
    crate::enrollment::ns(now)
}
/// The exact bytes of every original outgoing entry.
///
/// # Errors
/// The room has no publication state.
pub fn entries(view: &NativeCommittedView) -> Result<Vec<Vec<u8>>, String> {
    Ok(publication::state(view)?
        .entries
        .iter()
        .map(|entry| entry.bytes.clone())
        .collect())
}
/// The exact bytes of every recovery projection entry.
///
/// # Errors
/// The room has no publication state.
pub fn projection(view: &NativeCommittedView) -> Result<Vec<Vec<u8>>, String> {
    Ok(publication::state(view)?
        .projection
        .iter()
        .map(|entry| entry.bytes.clone())
        .collect())
}
/// # Errors
/// As the owner's own outgoing encoding.
pub fn prepare(request: &OwnerCommitRequest) -> Result<Vec<u8>, String> {
    publication::prepare(request)
}
/// # Errors
/// `message` is not an outgoing document.
pub fn request(message: &NeighbourMsg) -> Result<OwnerCommitRequest, String> {
    publication::request(message)
}
/// `(timestamp_ns, channel, incremental)` as the owner records them.
///
/// # Errors
/// `message` is not an outgoing document.
pub fn metadata(message: &NeighbourMsg) -> Result<(u64, [u8; 32], bool), String> {
    publication::metadata(message)
}
/// # Errors
/// As the owner's own local append.
pub fn append_local(view: &mut NativeCommittedView, bytes: Vec<u8>) -> Result<(), String> {
    publication::append_local(view, bytes)
}
/// # Errors
/// The actual projection outgrew the reserve admitted for it.
pub fn check_actual_projection_bound(view: &NativeCommittedView) -> Result<(), String> {
    publication::check_actual_projection_bound(view)
}
pub fn peer_mut(view: &mut NativeCommittedView) -> &mut StatePeer {
    &mut view.peer
}
#[must_use]
pub fn durable_peer(view: &NativeCommittedView) -> &[u8] {
    &view.record.peer
}
/// Record the live peer as the view's durable peer.
///
/// # Errors
/// The peer cannot be saved.
pub fn seal(view: &mut NativeCommittedView) -> Result<(), String> {
    view.record.peer = view.peer.durable()?;
    Ok(())
}
pub fn bump_generation(view: &mut NativeCommittedView) {
    view.record.generation = view.record.generation.saturating_add(1);
}
pub fn insert_room(storage: &mut NativeAuthorityStorage, room: RoomId, view: NativeCommittedView) {
    storage.rooms.insert(room, view.record);
}
pub fn bump_revision(storage: &mut NativeAuthorityStorage) {
    storage.revision = storage.revision.saturating_add(1);
}
/// What a record keeps of one room.
pub struct Stored {
    pub peer: Vec<u8>,
    pub governor: DeviceCert,
    pub device: DeviceCert,
    pub projection: Vec<Vec<u8>>,
}
#[must_use]
pub fn stored(storage: &NativeAuthorityStorage, room: RoomId) -> Option<Stored> {
    let record = storage.rooms.get(&room)?;
    Some(Stored {
        peer: record.peer.clone(),
        governor: record.governor.clone(),
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
pub fn set_stored_peer(storage: &mut NativeAuthorityStorage, room: RoomId, peer: Vec<u8>) {
    storage.rooms.get_mut(&room).unwrap().peer = peer;
}
