//! A device whose certificate expires while it runs keeps its membership:
//! its person's root renews the certificate, the device proves its
//! membership with the renewal, and the governor and the device's own
//! record take it in place of the expired one.
use crate::testing::{self, ROOM};
use crate::{RoomId, VerifiedNativeEnrollmentIntent, open_file};
use ed25519_dalek::SigningKey;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_governor::{Governor, GovernorStore, RoomTxn};
use tilo_peer_state::join::invitations::{IssueInvitationRequest, IssuedInvitation};
use tilo_peer_state::join::{
    self, JoinTransportRequest, JoinTransportResponse, MemberRole, TransportHint,
};
use tilo_peer_state::join_ledger::{GovernanceMessages, Governed, JoinError, JoinLedger, Signer};
use tilo_peer_state::{SeedState, StatePeer, seed_room};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

/// The room's admitted state, which also gives the governance a joining
/// member admits.
struct Admitted(StatePeer);

impl GovernanceMessages for Admitted {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        postcard::to_stdvec(&self.0.room_message())
            .map(|bytes| vec![bytes])
            .unwrap_or_default()
    }
}

/// One room as its governor holds it in memory.
struct HeldRoom {
    peer: Admitted,
    ledger: JoinLedger,
    generation: u64,
}

impl HeldRoom {
    fn governed(&self) -> Governed<'_> {
        Governed {
            peer: &self.peer.0,
            workspace_id: join::room_workspace_id(ROOM).unwrap(),
            generation: self.generation,
            governance_messages: &self.peer,
            hosts: &[],
        }
    }
}

impl RoomTxn for HeldRoom {
    fn room(&self) -> &str {
        ROOM
    }

    fn governed(&self) -> Governed<'_> {
        HeldRoom::governed(self)
    }

    fn ledger(&self) -> &JoinLedger {
        &self.ledger
    }

    fn stage_ledger(&mut self, ledger: JoinLedger) {
        self.ledger = ledger;
    }

    fn commit_governance(
        &mut self,
        peer: StatePeer,
        _now: u64,
        ledger: &mut dyn FnMut(&Governed<'_>) -> Result<JoinLedger, JoinError>,
    ) -> Result<(), JoinError> {
        let next = HeldRoom {
            peer: Admitted(peer),
            ledger: JoinLedger::default(),
            generation: self.generation.saturating_add(1),
        };
        let answered = ledger(&next.governed())?;
        *self = HeldRoom {
            ledger: answered,
            ..next
        };
        Ok(())
    }
}

/// A governor of one room, kept in memory: the exchange every host
/// answers ([`Governor`]) over the simplest store.
struct MemoryGovernor {
    key: SigningKey,
    nonce: Mutex<u8>,
    room: Mutex<HeldRoom>,
}

impl GovernorStore for MemoryGovernor {
    fn key(&self) -> &SigningKey {
        &self.key
    }

    fn nonce(&self) -> Result<[u8; 32], JoinError> {
        let mut nonce = self.nonce.lock().unwrap();
        *nonce = nonce.wrapping_add(1);
        Ok([*nonce; 32])
    }

    fn with_room<T: Send>(
        &self,
        room: &str,
        decide: impl FnOnce(&mut dyn RoomTxn) -> Result<T, JoinError> + Send,
    ) -> impl std::future::Future<Output = Result<T, JoinError>> + Send {
        std::future::ready(if room == ROOM {
            decide(&mut *self.room.lock().unwrap())
        } else {
            Err(JoinError::new("this governor holds no such room"))
        })
    }
}

impl MemoryGovernor {
    /// A governor of `ROOM` signing as `device`, whose key `key` is.
    fn new(device: &DeviceCert, key: &SigningKey, now: SystemTime) -> Self {
        let mut peer = StatePeer::new(
            ROOM,
            device.clone(),
            vec![device.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.select_admission_v5().unwrap();
        peer.install_seed(
            SeedState {
                room_changes: seed_room(ROOM, device, std::slice::from_ref(device)).unwrap(),
                documents: Vec::new(),
            },
            key,
            now,
        )
        .unwrap();
        Self {
            key: key.clone(),
            nonce: Mutex::new(0),
            room: Mutex::new(HeldRoom {
                peer: Admitted(peer),
                ledger: JoinLedger::default(),
                generation: 1,
            }),
        }
    }

    fn issue(
        &self,
        request: IssueInvitationRequest,
        transport: TransportHint,
        now: SystemTime,
    ) -> IssuedInvitation {
        let mut room = self.room.lock().unwrap();
        let mut ledger = room.ledger.clone();
        let administrator = room.peer.0.authority().clone();
        let issued = ledger
            .issue(
                &room.governed(),
                &administrator,
                request,
                vec![transport],
                Signer {
                    key: &self.key,
                    nonce: [1; 16],
                },
                testing::ns(now).unwrap(),
            )
            .unwrap();
        room.ledger = ledger;
        issued
    }

    fn with_peer<T>(&self, read: impl FnOnce(&StatePeer) -> T) -> T {
        read(&self.room.lock().unwrap().peer.0)
    }
}

/// `governor`'s answer to `request` from `remote` at `now`. Every step of
/// a memory governor is decided before its future is first polled.
fn answer(
    governor: &Governor<MemoryGovernor>,
    remote: [u8; 32],
    request: JoinTransportRequest,
    now: SystemTime,
) -> JoinTransportResponse {
    let answer = std::pin::pin!(governor.answer(remote, request, testing::ns(now).unwrap()));
    match answer.poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(answer) => answer,
        std::task::Poll::Pending => panic!("a memory governor answers at once"),
    }
}

const HOUR: Duration = Duration::from_secs(60 * 60);

/// A certificate of `key` from `root`, valid from `issued` for `lifetime`.
fn expiring(
    root: &SigningKey,
    key: &SigningKey,
    issued: SystemTime,
    lifetime: Duration,
) -> DeviceCert {
    let issued_at_ns = testing::ns(issued).unwrap();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Test slot".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns,
        expires_at_ns: Some(
            issued_at_ns.saturating_add(u64::try_from(lifetime.as_nanos()).unwrap()),
        ),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one device's life read top to bottom: join, expiry, renewal, restart"
)]
fn a_device_whose_certificate_expires_while_it_runs_keeps_its_membership() {
    let directory = tempfile::tempdir().unwrap();
    let person = SigningKey::from_bytes(&[91; 32]);
    let key = SigningKey::from_bytes(&[92; 32]);
    let device = testing::certificate(&person, &key, "Desktop");
    let room = RoomId::parse(ROOM).unwrap();
    let joined_at = SystemTime::now();
    let joined_ns = testing::ns(joined_at).unwrap();
    let governor = Governor(MemoryGovernor::new(&device, &key, joined_at));
    let workspace_id = join::room_workspace_id(ROOM).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: ROOM.into(),
        workspace_id,
        governing_authority: device.clone(),
        endpoint_addr: iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&device.device_public_key).unwrap(),
        ),
        host_user_public_key: device.user_public_key,
        host_device_certificate: device.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(&key)
    .unwrap();

    // A test slot: its throwaway root certifies its device for a day.
    let slot_root = SigningKey::from_bytes(&[81; 32]);
    let slot_key = SigningKey::from_bytes(&[82; 32]);
    let first = expiring(
        &slot_root,
        &slot_key,
        joined_at.checked_sub(HOUR).unwrap(),
        24 * HOUR,
    );
    let issued = governor.0.issue(
        IssueInvitationRequest {
            request_id: [1; 32],
            role: MemberRole::Editor,
            recipient_user_public_key: Some(first.user_public_key),
            invitee_name: None,
            expires_at_ns: joined_ns.saturating_add(60_000_000_000),
            inviter_name: None,
        },
        hint.clone(),
        joined_at,
    );
    let verified = join::decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(first.user_public_key),
        &RevocationList::new(),
        joined_ns,
    )
    .unwrap();
    let JoinTransportResponse::Challenge(challenge) = answer(
        &governor,
        first.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: issued.signed_invitation,
            device_cert: first.clone(),
            request_id: [95; 32],
        }),
        joined_at,
    ) else {
        panic!("the governor challenges the slot");
    };
    let complete = join::JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, &slot_key).unwrap(),
    };
    let member_path = directory.path().join("member.json");
    let member = open_file(&member_path).unwrap();
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        first.clone(),
        hint,
        joined_at,
    )
    .unwrap();
    let pending = member
        .prepare_enrollment_into(&intent, None, joined_at)
        .unwrap();
    let JoinTransportResponse::Complete(bootstrap) = answer(
        &governor,
        first.device_public_key,
        JoinTransportRequest::Complete(complete),
        joined_at,
    ) else {
        panic!("the governor admits the slot");
    };
    member
        .install(
            &testing::finalize(intent, pending, bootstrap).unwrap(),
            joined_at,
        )
        .unwrap();

    // Two days on, the first certificate has expired; the slot's root
    // renewed it when it was due.
    let later = joined_at.checked_add(48 * HOUR).unwrap();
    let later_ns = testing::ns(later).unwrap();
    let renewed = expiring(
        &slot_root,
        &slot_key,
        later.checked_sub(HOUR).unwrap(),
        24 * HOUR,
    );
    assert!(first.is_expired_at(later_ns));
    let membership = |device_cert: &DeviceCert, seed: u8| {
        answer(
            &governor,
            slot_key.verifying_key().to_bytes(),
            JoinTransportRequest::MembershipChallenge(join::MembershipChallengeRequest {
                room: ROOM.into(),
                workspace_id,
                device_cert: device_cert.clone(),
                request_id: [seed; 32],
            }),
            later,
        )
    };
    assert!(
        matches!(membership(&first, 10), JoinTransportResponse::Error { .. }),
        "the expired certificate proves nothing"
    );
    let JoinTransportResponse::MembershipChallenge(challenge) = membership(&renewed, 11) else {
        panic!("the governor challenges the renewed certificate as the device it renews");
    };
    let JoinTransportResponse::MembershipComplete(renewal) = answer(
        &governor,
        slot_key.verifying_key().to_bytes(),
        JoinTransportRequest::MembershipComplete(join::MembershipCompleteRequest {
            proof: join::sign_membership_proof(challenge.challenge, &slot_key).unwrap(),
        }),
        later,
    ) else {
        panic!("the governor answers the slot's renewed membership");
    };
    assert_eq!(renewal.device_cert, renewed);
    assert_eq!(
        renewal.role,
        MemberRole::Editor,
        "the same member, with its role"
    );
    member
        .renew_membership(room, &renewed, &renewal, later)
        .unwrap();

    governor
        .0
        .with_peer(|peer| {
            assert!(peer.certificates().contains(&renewed));
            assert!(!peer.certificates().contains(&first));
            peer.permissions(renewed.device_public_key, later)
                .map(|_| ())
        })
        .unwrap();
    member
        .with_current(room, |view| {
            assert_eq!(view.device(), &renewed);
            assert!(view.peer().certificates().contains(&renewed));
            Ok(())
        })
        .unwrap();
    // A stale certificate is refused after the renewal.
    assert!(matches!(
        membership(&first, 13),
        JoinTransportResponse::Error { .. }
    ));

    // The renewed membership is what the device starts with next time.
    drop(member);
    let member = open_file(&member_path).unwrap();
    member
        .with_current(room, |view| {
            assert_eq!(view.device(), &renewed);
            Ok(())
        })
        .unwrap();
}
