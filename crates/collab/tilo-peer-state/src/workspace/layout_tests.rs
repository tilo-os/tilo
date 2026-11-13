//! A governance stored in the layout before this one comes up as this
//! layout, each value reading back as it held.
use super::*;
use crate::join::{InviteGrant, MemberRole, VerifiedRedemption};
use crate::state::invitation_ledger::{self, Piece};
use crate::{NeighbourMsg, StatePeer};
use automerge::ReadDoc as _;
use ed25519_dalek::SigningKey;
use serde::Serialize;
use tilo_user_identity::{DeviceCapabilities, RevocationList, sign_device_cert};
use web_time::SystemTime;
use crate::join::WorkspaceId;

/// A device row as the earlier layout wrote it.
#[derive(Serialize)]
struct RowJson<'a> {
    device_cert: &'a DeviceCert,
    revocations: &'a [tilo_user_identity::RevocationEntry],
    admission: Option<ReceiptJson>,
    admissions: Vec<ReceiptJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delegate: Option<&'a crate::state::GovernedDelegateRow>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    workspace_delegations: &'a [crate::state::WorkspaceDelegation],
}

/// A receipt as the earlier layout wrote it.
#[derive(Serialize)]
struct ReceiptJson {
    invite_id: [u8; 32],
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    request_id: [u8; 32],
    role: MemberRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    invitation: Option<String>,
}

fn receipt_json(receipt: &RedemptionReceipt, invitation: Option<String>) -> ReceiptJson {
    ReceiptJson {
        invite_id: receipt.invite_id,
        user_public_key: receipt.user_public_key,
        device_public_key: receipt.device_public_key,
        request_id: receipt.request_id,
        role: receipt.role,
        invitation,
    }
}

/// `doc`, a governance in this layout, as the layout before it wrote the
/// same state: what a device holds from a build before this one.
pub(crate) fn earlier(doc: &AutoCommit) -> AutoCommit {
    let parts = invitation_ledger::parts(doc).unwrap();
    super::super::base::rebase_with(doc, automerge::ActorId::random(), |state| {
        if let Some(Value::Map(rows)) = state.get_mut(IDENTITIES_KEY) {
            for entry in rows.values_mut() {
                let Value::Scalar(ScalarValue::Bytes(bytes)) = &mut entry.value else {
                    continue;
                };
                let row = GovernedIdentity::decode(bytes).unwrap();
                *bytes = serde_json::to_vec(&RowJson {
                    device_cert: &row.device_cert,
                    revocations: &row.revocations,
                    admission: row.admission.as_ref().map(|r| receipt_json(r, None)),
                    admissions: row
                        .admissions
                        .iter()
                        .map(|r| receipt_json(r, None))
                        .collect(),
                    delegate: row.delegate.as_ref(),
                    workspace_delegations: &row.workspace_delegations,
                })
                .unwrap();
            }
        }
        if let Some(Value::Map(receipts)) = state.get_mut(RECEIPTS_KEY) {
            for entry in receipts.values_mut() {
                let Value::Scalar(ScalarValue::Bytes(bytes)) = &mut entry.value else {
                    continue;
                };
                let ledgered = LedgerReceipt::decode(bytes).unwrap();
                let signed = ledgered
                    .invitation(|digest| invitation_ledger::part_in(&parts, digest))
                    .unwrap();
                *bytes = serde_json::to_vec(&receipt_json(
                    &ledgered.receipt,
                    Some(crate::join::encode_invitation_text(&signed)),
                ))
                .unwrap();
            }
        }
        state.remove(PARTS_KEY);
    })
    .unwrap()
}

pub(crate) fn device(root: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("guest-{seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// An invitation from the workspace's owner device, as a door hands one
/// out: its transport details signed by the host, redeemed by `device`.
pub(crate) fn door_redemption(
    peer: &StatePeer,
    authority: &SigningKey,
    nonce: u8,
    (key, cert): &(SigningKey, DeviceCert),
    now_ns: u64,
) -> VerifiedRedemption {
    let issuer = peer.owner_certificates()[0].clone();
    let hint = crate::join::sign_transport_hint(
        "iroh",
        r#"{"node":"a host's node and its relays, as a door names them"}"#,
        issuer.clone(),
        [9; 32],
        authority,
    )
    .unwrap();
    let signed = crate::join::issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::from_u128(7),
            issuer,
            role: MemberRole::Editor,
            nonce: [nonce; 16],
            recipient_user_public_key: Some(cert.user_public_key),
            invitee_name: Some(format!("Guest {nonce}")),
            expires_at_ns: now_ns.checked_add(10_000_000_000).unwrap(),
            transport: vec![hint],
        },
        authority,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(cert.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let request = [nonce ^ cert.device_public_key[0]; 32];
    let challenge = crate::join::JoinChallenge::new(
        &join,
        cert,
        request,
        [nonce; 32],
        now_ns.checked_add(1_000_000_000).unwrap(),
        now_ns,
    )
    .unwrap();
    let proof = crate::join::sign_join_proof(challenge.clone(), key).unwrap();
    crate::join::verify_join_proof(&proof, &challenge, cert, &RevocationList::new(), now_ns)
        .unwrap()
}

pub(crate) fn nanos(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

/// The seeded workspace with `guests` people let in through its door.
pub(crate) fn crowded(guests: u8) -> (StatePeer, SigningKey, SystemTime, Vec<DeviceCert>) {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let mut admitted = Vec::new();
    for n in 0..guests {
        let guest = device(
            &SigningKey::from_bytes(&[n.wrapping_add(100); 32]),
            n.wrapping_add(150),
        );
        let redemption = door_redemption(&peer, &authority, n, &guest, nanos(now));
        assert!(
            peer.governance_mut()
                .admit_invited_member(&redemption, &authority, now)
                .unwrap()
        );
        admitted.push(guest.1);
    }
    (peer, authority, now, admitted)
}

pub(crate) fn document(peer: &StatePeer) -> AutoCommit {
    let NeighbourMsg::SyncResp { changes, .. } = peer.workspace_message() else {
        panic!("workspace message is a SyncResp")
    };
    crate::workspace::candidate(None, &changes).unwrap()
}

/// What the values under `key` hold, by entry.
fn values(doc: &AutoCommit, key: &str) -> std::collections::BTreeMap<String, Vec<u8>> {
    let Some((_, map)) = doc.get(automerge::ROOT, key).unwrap() else {
        return std::collections::BTreeMap::new();
    };
    doc.keys(&map)
        .map(|entry| {
            let (value, _) = doc.get(&map, &entry).unwrap().unwrap();
            (entry, value.as_bytes().unwrap().to_vec())
        })
        .collect()
}

#[test]
fn an_earlier_governance_comes_up_holding_what_it_held() {
    let (peer, _, _, guests) = crowded(5);
    let current = document(&peer);
    let before = earlier(&current);
    let brought = bring_up(&before, automerge::ActorId::random()).unwrap();
    for key in [IDENTITIES_KEY, RECEIPTS_KEY, PARTS_KEY] {
        assert_eq!(values(&brought, key), values(&current, key), "{key}");
    }
    assert_eq!(
        super::super::base::generation(&brought),
        super::super::base::generation(&before) + 1,
        "a base of the next generation, which replaces what every device holds"
    );
    // One issuer certificate and one set of transport details, for every
    // receipt.
    assert_eq!(values(&brought, PARTS_KEY).len(), 2);
    for (_, bytes) in values(&brought, RECEIPTS_KEY) {
        let ledgered = LedgerReceipt::decode(&bytes).unwrap();
        assert_eq!(
            ledgered
                .invitation
                .iter()
                .filter(|piece| matches!(piece, Piece::Shared(_)))
                .count(),
            2
        );
    }
    let per_member = |doc: &AutoCommit| {
        [IDENTITIES_KEY, RECEIPTS_KEY]
            .into_iter()
            .flat_map(|key| values(doc, key).into_values())
            .map(|bytes| bytes.len())
            .sum::<usize>()
            / guests.len()
    };
    let (was, is) = (per_member(&before), per_member(&brought));
    eprintln!("governance bytes per member: {was} B before, {is} B in this layout");
    assert!(is * 4 < was, "{is} B a member, from {was} B");
}

#[test]
fn every_admission_reads_back_after_the_governance_comes_up() {
    let (peer, _, now, guests) = crowded(3);
    let workspace = peer.workspace_id().to_owned();
    let brought = bring_up(&earlier(&document(&peer)), automerge::ActorId::random()).unwrap();
    let policy = crate::members::replay::replay(&brought, &workspace);
    let held = crate::members::replay::replay(&document(&peer), &workspace);
    assert_eq!(policy.members, held.members);
    for guest in &guests {
        assert!(
            policy
                .members
                .contains_key(&hex::encode(guest.user_public_key)),
            "a guest the door let in is a member"
        );
    }
    let _ = now;
}
