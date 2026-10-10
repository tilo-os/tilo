//! Owner-neutral transactions over the canonical room governance document.

use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use collab_types::caps::{CapOverride, Principal, Scope, Subject};
use ed25519_dalek::SigningKey;
use web_time::SystemTime;

use crate::state::{GovernedIdentity, RedemptionReceipt, put_member_capability};

/// How many later redemption receipts one governed identity row keeps beside
/// its first (`GovernedIdentity::admission`).
///
/// The row is a single JSON blob rewritten on every redemption, so an
/// unbounded list makes each later admission, roster read and room sync pay
/// for the device's whole redemption history. The first receipt is kept for
/// good: it is the admission evidence membership reads (`has_admission`, the
/// governed member list). Only the oldest of the later receipts are dropped,
/// and nothing they prove is lost: [`admit_invited_member`] copies every
/// receipt it has seen into the per-invite `invitation_receipts_v1` ledger in
/// the same transaction that drops one, and that ledger is what refuses a
/// second redemption of the same invitation. See "Retention" in
/// `docs/architecture/collaboration.md`.
pub const MAX_RETAINED_IDENTITY_ADMISSIONS: usize = 8;

/// Keep the latest [`MAX_RETAINED_IDENTITY_ADMISSIONS`] receipts, in order.
fn retain_latest_admissions(admissions: &mut Vec<RedemptionReceipt>) {
    let excess = admissions
        .len()
        .saturating_sub(MAX_RETAINED_IDENTITY_ADMISSIONS);
    admissions.drain(..excess);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Applied { changes: Vec<Vec<u8>> },
    AlreadyApplied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GovernanceOutcome {
    Applied { changes: Vec<Vec<u8>> },
    Unchanged,
}

/// Apply certificate, role and immutable redemption receipt to the caller's
/// existing canonical room document as one transaction. On error `document`
/// remains byte-for-byte unchanged.
///
/// # Errors
///
/// Returns an error if the redemption conflicts with recorded governance or the
/// document cannot be updated.
#[expect(
    clippy::too_many_lines,
    reason = "one atomic governance transaction; on error the document stays unchanged"
)]
pub fn admit_invited_member(
    document: &mut AutoCommit,
    redemption: &crate::join::VerifiedRedemption,
    authority_key: &SigningKey,
    now: SystemTime,
) -> Result<AdmissionOutcome, String> {
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX);
    let grant = redemption.join().grant();
    if authority_key.verifying_key().to_bytes() != grant.issuer.device_public_key {
        return Err("governance writer is not invitation issuer".into());
    }
    tilo_user_identity::verify_device_cert_at(&grant.authority, now_ns)
        .map_err(|error| error.to_string())?;
    tilo_user_identity::verify_device_cert_at(redemption.certificate(), now_ns)
        .map_err(|error| error.to_string())?;
    if now_ns >= grant.expires_at_ns {
        return Err("invitation has expired".into());
    }
    let declared_room = document
        .get(ROOT, "room")
        .map_err(|error| error.to_string())?
        .and_then(|(value, _)| value.as_str().map(ToOwned::to_owned))
        .ok_or("governed room id is missing")?;
    if declared_room != grant.room {
        return Err("invitation room does not match governed document".into());
    }
    let governance = crate::room::governance_of(Some(document));
    if governance.owner.as_deref() != Some(&hex::encode(grant.authority.user_public_key)) {
        return Err("invitation authority is not the structural owner".into());
    }
    if let Some(delegation) = &grant.delegation {
        crate::join::verify_workspace_delegation(
            delegation,
            &grant.authority,
            &tilo_user_identity::RevocationList::new(),
            now_ns,
        )?;
        let persisted = governance.identities.values().any(|bytes| {
            serde_json::from_slice::<GovernedIdentity>(bytes).is_ok_and(|identity| {
                identity
                    .workspace_delegations
                    .iter()
                    .any(|known| known == delegation)
            })
        });
        if !persisted
            || delegation.delegate != grant.issuer
            || !delegation.permissions.admit_invited_members
        {
            return Err("invitation issuer lacks persisted room admission delegation".into());
        }
    } else if grant.issuer != grant.authority {
        return Err("invitation issuer lacks room delegation".into());
    }
    let receipt = RedemptionReceipt {
        invite_id: redemption.join().invite_id(),
        user_public_key: redemption.certificate().user_public_key,
        device_public_key: redemption.certificate().device_public_key,
        request_id: redemption.request_id(),
        role: grant.role,
    };
    let invitation_receipts = crate::room::invitation_receipts(document)?;
    let mut exact_redemption = false;
    for (key, values) in &invitation_receipts {
        for bytes in values {
            let existing: RedemptionReceipt =
                serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
            if *key != hex::encode(existing.invite_id) {
                return Err("governed invitation receipt key mismatch".into());
            }
            if existing.invite_id == receipt.invite_id {
                if existing != receipt {
                    return Err(
                        "invitation was already redeemed by another request or device".into(),
                    );
                }
                exact_redemption = true;
            }
        }
    }
    let mut legacy_receipts = Vec::new();
    for bytes in governance.identities.values() {
        let identity: GovernedIdentity =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        for existing in identity.admission.iter().chain(&identity.admissions) {
            if existing.invite_id == receipt.invite_id {
                if *existing != receipt {
                    return Err(
                        "invitation was already redeemed by another request or device".into(),
                    );
                }
                exact_redemption = true;
            }
            legacy_receipts.push(existing.clone());
        }
    }
    if exact_redemption {
        return Ok(AdmissionOutcome::AlreadyApplied);
    }
    let cert = redemption.certificate();
    let id = hex::encode(cert.device_public_key);
    let prior = governance
        .identities
        .get(&id)
        .map(|bytes| serde_json::from_slice::<GovernedIdentity>(bytes))
        .transpose()
        .map_err(|error| error.to_string())?;
    if prior.as_ref().is_some_and(|old| old.device_cert != *cert) {
        return Err("governed certificate conflict".into());
    }
    let mut candidate = document.clone();
    let before = candidate.get_heads();
    let identities = candidate
        .get(ROOT, "peer_identities_v1")
        .map_err(|error| error.to_string())?
        .ok_or("governed identities missing")?
        .1;
    let names = candidate
        .get(ROOT, "peers")
        .map_err(|error| error.to_string())?
        .ok_or("governed peers missing")?
        .1;
    let caps = candidate
        .get(ROOT, "capabilities")
        .map_err(|error| error.to_string())?
        .ok_or("governed capabilities missing")?
        .1;
    candidate.set_actor(automerge::ActorId::from(
        authority_key.verifying_key().to_bytes().to_vec(),
    ));
    let invitation_receipts = match candidate
        .get(ROOT, "invitation_receipts_v1")
        .map_err(|error| error.to_string())?
    {
        Some((_, object)) => object,
        None => candidate
            .put_object(ROOT, "invitation_receipts_v1", automerge::ObjType::Map)
            .map_err(|error| error.to_string())?,
    };
    for legacy in legacy_receipts.iter().chain([&receipt]) {
        candidate
            .put(
                &invitation_receipts,
                hex::encode(legacy.invite_id),
                automerge::ScalarValue::Bytes(
                    serde_json::to_vec(legacy).map_err(|error| error.to_string())?,
                ),
            )
            .map_err(|error| error.to_string())?;
    }
    let (revocations, mut admission, mut admissions, workspace_delegations) = prior.map_or_else(
        || (Vec::new(), None, Vec::new(), Vec::new()),
        |identity| {
            (
                identity.revocations,
                identity.admission,
                identity.admissions,
                identity.workspace_delegations,
            )
        },
    );
    if admission.is_none() {
        admission = Some(receipt);
    } else {
        admissions.push(receipt);
        retain_latest_admissions(&mut admissions);
    }
    let identity = GovernedIdentity {
        device_cert: cert.clone(),
        revocations,
        admission,
        admissions,
        workspace_delegations,
    };
    candidate
        .put(
            &identities,
            &id,
            automerge::ScalarValue::Bytes(
                serde_json::to_vec(&identity).map_err(|error| error.to_string())?,
            ),
        )
        .map_err(|error| error.to_string())?;
    candidate
        .put(&names, &id, member_display_name(grant, cert))
        .map_err(|error| error.to_string())?;
    put_member_capability(&mut candidate, &caps, cert.user_public_key, grant.role)?;
    candidate.commit();
    crate::room::media::refresh_authorization(document, &mut candidate)
        .map_err(|error| error.to_string())?;
    let changes = candidate
        .get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    *document = candidate;
    Ok(AdmissionOutcome::Applied { changes })
}

/// Display bound for a governed member name: self-asserted text, never authority.
const MAX_DISPLAY_NAME_CHARS: usize = 80;
const MAX_DISPLAY_NAME_BYTES: usize = 128;

fn bounded_display_name(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= MAX_DISPLAY_NAME_BYTES
        && value.chars().count() <= MAX_DISPLAY_NAME_CHARS
        && !value.chars().any(char::is_control))
    .then_some(value)
}

/// The name other members see. A bearer invitation is redeemed by a person the
/// issuer does not know yet, so the name that person chose (the label on their
/// own signed device certificate) wins; the issuer's `invitee_name` is only a
/// fallback. A recipient-bound invitation names a known person, so the issuer's
/// name wins there. Either way the value is bounded display text; roles and
/// access come only from the receipt and capability rows.
fn member_display_name<'a>(
    grant: &'a crate::join::InviteGrant,
    cert: &'a tilo_user_identity::DeviceCert,
) -> &'a str {
    let chosen = bounded_display_name(&cert.device_label);
    let issued = grant.invitee_name.as_deref().and_then(bounded_display_name);
    let preferred = if grant.recipient_user_public_key.is_none() {
        chosen.or(issued)
    } else {
        issued.or(chosen)
    };
    preferred.unwrap_or("Guest")
}

/// Record a verified workspace delegation in the governed room document.
///
/// # Errors
///
/// Returns an error if the delegation does not verify against `authority` or the
/// document cannot be updated.
pub fn install_workspace_delegation(
    document: &mut AutoCommit,
    authority: &tilo_user_identity::DeviceCert,
    delegation: crate::join::WorkspaceDelegation,
    authority_key: &SigningKey,
    now: SystemTime,
) -> Result<GovernanceOutcome, String> {
    validate_authority(document, authority, authority_key)?;
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX);
    crate::join::verify_workspace_delegation(
        &delegation,
        authority,
        &tilo_user_identity::RevocationList::new(),
        now_ns,
    )?;
    let mut candidate = document.clone();
    let before = candidate.get_heads();
    let (_, identities) = candidate
        .get(ROOT, "peer_identities_v1")
        .map_err(|error| error.to_string())?
        .ok_or("governed identities missing")?;
    let authority_id = hex::encode(authority.device_public_key);
    let bytes = candidate
        .get(&identities, &authority_id)
        .map_err(|error| error.to_string())?
        .and_then(|(value, _)| match value {
            automerge::Value::Scalar(value) => match value.as_ref() {
                automerge::ScalarValue::Bytes(bytes) => Some(bytes.clone()),
                _ => None,
            },
            automerge::Value::Object(_) => None,
        })
        .ok_or("authority identity is absent from governed roster")?;
    let mut identity: GovernedIdentity =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if identity.device_cert != *authority {
        return Err("governed authority certificate mismatch".into());
    }
    if identity.workspace_delegations.contains(&delegation) {
        return Ok(GovernanceOutcome::Unchanged);
    }
    identity.workspace_delegations.push(delegation);
    identity
        .workspace_delegations
        .sort_by_key(|item| (item.delegate.device_public_key, item.nonce));
    candidate.set_actor(automerge::ActorId::from(
        authority_key.verifying_key().to_bytes().to_vec(),
    ));
    candidate
        .put(
            &identities,
            authority_id,
            automerge::ScalarValue::Bytes(
                serde_json::to_vec(&identity).map_err(|error| error.to_string())?,
            ),
        )
        .map_err(|error| error.to_string())?;
    candidate.commit();
    finish(document, candidate, &before)
}

/// Change a governed collaborator's role in the room document.
///
/// # Errors
///
/// Returns an error if the user is the owner or not a governed member, or the document
/// cannot be updated.
pub fn set_member_role(
    document: &mut AutoCommit,
    authority: &tilo_user_identity::DeviceCert,
    user_public_key: [u8; 32],
    role: crate::join::MemberRole,
    authority_key: &SigningKey,
) -> Result<GovernanceOutcome, String> {
    validate_authority(document, authority, authority_key)?;
    if user_public_key == authority.user_public_key {
        return Err("owner role cannot be changed through member governance".into());
    }
    let governance = crate::room::governance_of(Some(document));
    if !governance.identities.values().any(|bytes| {
        serde_json::from_slice::<GovernedIdentity>(bytes).is_ok_and(|identity| {
            identity.device_cert.user_public_key == user_public_key && identity.has_admission()
        })
    }) {
        return Err("member is absent from governed admission history".into());
    }
    let mut candidate = document.clone();
    let before = candidate.get_heads();
    let capabilities = candidate
        .get(ROOT, "capabilities")
        .map_err(|error| error.to_string())?
        .ok_or("governed capabilities missing")?
        .1;
    candidate.set_actor(automerge::ActorId::from(
        authority_key.verifying_key().to_bytes().to_vec(),
    ));
    put_member_capability(&mut candidate, &capabilities, user_public_key, role)?;
    candidate.commit();
    finish(document, candidate, &before)
}

/// Remove a governed collaborator from the room document.
///
/// # Errors
///
/// Returns an error if the user is the owner or not a governed member, or the document
/// cannot be updated.
pub fn remove_member(
    document: &mut AutoCommit,
    authority: &tilo_user_identity::DeviceCert,
    user_public_key: [u8; 32],
    authority_key: &SigningKey,
) -> Result<GovernanceOutcome, String> {
    validate_authority(document, authority, authority_key)?;
    if user_public_key == authority.user_public_key {
        return Err("workspace owner cannot be removed".into());
    }
    let mut candidate = document.clone();
    let (_, capabilities) = candidate
        .get(ROOT, "capabilities")
        .map_err(|error| error.to_string())?
        .ok_or("governed capabilities missing")?;
    let person = hex::encode(user_public_key);
    let matching = candidate
        .keys(&capabilities)
        .filter(|id| {
            candidate
                .get(&capabilities, id)
                .ok()
                .flatten()
                .is_some_and(|(value, _)| {
                    let automerge::Value::Scalar(value) = value else {
                        return false;
                    };
                    let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
                        return false;
                    };
                    serde_json::from_slice::<CapOverride>(bytes).is_ok_and(|cap| {
                        subject_is_person(&cap.subject, user_public_key, &person)
                            && matches!(cap.scope, Scope::Room(_) | Scope::Workspace)
                    })
                })
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(GovernanceOutcome::Unchanged);
    }
    let before = candidate.get_heads();
    candidate.set_actor(automerge::ActorId::from(
        authority_key.verifying_key().to_bytes().to_vec(),
    ));
    for id in matching {
        candidate
            .delete(&capabilities, id)
            .map_err(|error| error.to_string())?;
    }
    candidate.commit();
    finish(document, candidate, &before)
}

fn validate_authority(
    document: &AutoCommit,
    authority: &tilo_user_identity::DeviceCert,
    authority_key: &SigningKey,
) -> Result<(), String> {
    if authority_key.verifying_key().to_bytes() != authority.device_public_key {
        return Err("governance writer is not pinned authority".into());
    }
    if crate::room::governance_of(Some(document)).owner.as_deref()
        != Some(&hex::encode(authority.user_public_key))
    {
        return Err("governance authority is not the structural owner".into());
    }
    Ok(())
}

fn finish(
    document: &mut AutoCommit,
    mut candidate: AutoCommit,
    before: &[automerge::ChangeHash],
) -> Result<GovernanceOutcome, String> {
    crate::room::media::refresh_authorization(document, &mut candidate)
        .map_err(|error| error.to_string())?;
    let changes = candidate
        .get_changes(before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect::<Vec<_>>();
    if changes.is_empty() {
        return Ok(GovernanceOutcome::Unchanged);
    }
    *document = candidate;
    Ok(GovernanceOutcome::Applied { changes })
}

fn subject_is_person(subject: &Subject, root: [u8; 32], canonical: &str) -> bool {
    match subject {
        Subject::User(key) => key.as_bytes() == &root,
        Subject::Principal(Principal::Person(person)) => person == canonical,
        _ => false,
    }
}

#[cfg(test)]
mod display_name_tests {
    use super::*;
    use crate::join::{
        InviteGrant, JoinChallenge, MemberRole, issue_invitation, sign_join_proof,
        verify_join_proof,
    };
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

    fn cert(label: &str) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        }
    }

    fn grant(recipient: Option<[u8; 32]>, invitee_name: Option<&str>) -> InviteGrant {
        let authority = cert("owner");
        InviteGrant {
            workspace_id: 7,
            room: "room".into(),
            authority: authority.clone(),
            issuer: authority,
            delegation: None,
            role: MemberRole::Editor,
            nonce: [0; 16],
            recipient_user_public_key: recipient,
            invitee_name: invitee_name.map(Into::into),
            expires_at_ns: u64::MAX,
            transport: vec![],
        }
    }

    #[test]
    fn bearer_redeemer_is_shown_by_the_name_they_chose() {
        let guest = cert("  Gina Guest ");
        assert_eq!(
            member_display_name(&grant(None, Some("Guest Tester")), &guest),
            "Gina Guest"
        );
        assert_eq!(
            member_display_name(&grant(None, None), &guest),
            "Gina Guest"
        );
    }

    #[test]
    fn recipient_bound_invitation_keeps_the_issuers_name() {
        assert_eq!(
            member_display_name(&grant(Some([1; 32]), Some("Ada")), &cert("MacBook")),
            "Ada"
        );
        assert_eq!(
            member_display_name(&grant(Some([1; 32]), None), &cert("Ada's laptop")),
            "Ada's laptop"
        );
    }

    #[test]
    fn unbounded_or_control_text_falls_back_and_never_widens_the_bound() {
        let issued = grant(None, Some("Guest Tester"));
        for label in [
            String::new(),
            "   ".to_owned(),
            "x".repeat(81),
            "界".repeat(43),
            "Gina\nGuest".to_owned(),
            "Gina\u{7}Guest".to_owned(),
        ] {
            let redeemer = cert(&label);
            assert_eq!(
                member_display_name(&issued, &redeemer),
                "Guest Tester",
                "{label:?}"
            );
        }
        let longest = cert(&"x".repeat(80));
        assert_eq!(member_display_name(&issued, &longest), "x".repeat(80));
        let unnamed = cert("");
        assert_eq!(
            member_display_name(&grant(None, Some(&"y".repeat(200))), &unnamed),
            "Guest"
        );
    }

    #[test]
    fn admitted_bearer_guest_is_listed_under_the_chosen_name() {
        let (mut peer, authority_key, _, _, now) = crate::admission_tests::seeded();
        let now_ns: u64 = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .unwrap();
        let guest_root = SigningKey::from_bytes(&[51; 32]);
        let guest_key = SigningKey::from_bytes(&[52; 32]);
        let mut guest_cert = DeviceCert {
            user_public_key: guest_root.verifying_key().to_bytes(),
            device_public_key: guest_key.verifying_key().to_bytes(),
            device_label: "Gina Guest".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut guest_cert, &guest_root).unwrap();
        let signed = issue_invitation(
            InviteGrant {
                workspace_id: 7,
                room: peer.room().into(),
                authority: peer.authority().clone(),
                issuer: peer.authority().clone(),
                delegation: None,
                role: MemberRole::Editor,
                nonce: [53; 16],
                recipient_user_public_key: None,
                invitee_name: Some("Guest Tester".into()),
                expires_at_ns: now_ns.checked_add(10_000_000_000).unwrap(),
                transport: vec![],
            },
            &authority_key,
        )
        .unwrap();
        let join = crate::join::decode_and_verify_invitation(
            &signed,
            None,
            &RevocationList::new(),
            now_ns,
        )
        .unwrap();
        let challenge = JoinChallenge::new(
            &join,
            &guest_cert,
            [54; 32],
            [55; 32],
            now_ns.checked_add(1_000_000_000).unwrap(),
            now_ns,
        )
        .unwrap();
        let proof = sign_join_proof(challenge.clone(), &guest_key).unwrap();
        let redemption = verify_join_proof(
            &proof,
            &challenge,
            &guest_cert,
            &RevocationList::new(),
            now_ns,
        )
        .unwrap();
        assert!(
            peer.admit_invited_member(&redemption, &authority_key, now)
                .unwrap()
        );
        let member = peer
            .governed_members(now)
            .unwrap()
            .into_iter()
            .find(|member| member.user_public_key == guest_cert.user_public_key)
            .expect("admitted guest is a governed member");
        assert_eq!(member.display_name.as_deref(), Some("Gina Guest"));
        assert_eq!(member.role, MemberRole::Editor);
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use crate::join::{
        InviteGrant, JoinChallenge, MemberRole, VerifiedRedemption, issue_invitation,
        sign_join_proof, verify_join_proof,
    };
    use crate::{NeighbourMsg, StatePeer};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

    fn device(root: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("guest-{seed}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    /// Invitation `nonce` from the room authority, redeemed by `device`.
    fn redemption(
        peer: &StatePeer,
        authority: &SigningKey,
        nonce: u8,
        (key, cert): &(SigningKey, DeviceCert),
        now_ns: u64,
    ) -> VerifiedRedemption {
        let signed = issue_invitation(
            InviteGrant {
                workspace_id: 7,
                room: peer.room().into(),
                authority: peer.authority().clone(),
                issuer: peer.authority().clone(),
                delegation: None,
                role: MemberRole::Editor,
                nonce: [nonce; 16],
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: Some("Guest".into()),
                expires_at_ns: now_ns.checked_add(10_000_000_000).unwrap(),
                transport: vec![],
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
        let challenge = JoinChallenge::new(
            &join,
            cert,
            request,
            [nonce; 32],
            now_ns.checked_add(1_000_000_000).unwrap(),
            now_ns,
        )
        .unwrap();
        let proof = sign_join_proof(challenge.clone(), key).unwrap();
        verify_join_proof(&proof, &challenge, cert, &RevocationList::new(), now_ns).unwrap()
    }

    /// The room governance document the peer sends.
    fn governance(peer: &StatePeer) -> crate::room::RoomGovernance {
        let NeighbourMsg::SyncResp { changes, .. } = peer.room_message() else {
            panic!("room message is a SyncResp")
        };
        crate::room::governance_of(Some(&crate::room::candidate(None, &changes).unwrap()))
    }

    #[test]
    fn identity_admissions_are_bounded_and_a_pruned_admission_cannot_readmit() {
        let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
        let now_ns: u64 = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .unwrap();
        let root = SigningKey::from_bytes(&[61; 32]);
        let guest = device(&root, 62);
        let total = MAX_RETAINED_IDENTITY_ADMISSIONS + 3;
        let redemptions: Vec<_> = (0..total)
            .map(|n| {
                let nonce = 100 + u8::try_from(n).unwrap();
                redemption(&peer, &authority, nonce, &guest, now_ns)
            })
            .collect();
        for redemption in &redemptions {
            assert!(
                peer.admit_invited_member(redemption, &authority, now)
                    .unwrap()
            );
        }
        let invite = |r: &VerifiedRedemption| r.join().invite_id();
        let governed = governance(&peer);
        let row: GovernedIdentity =
            serde_json::from_slice(&governed.identities[&hex::encode(guest.1.device_public_key)])
                .unwrap();
        assert_eq!(
            row.admission.as_ref().map(|r| r.invite_id),
            Some(invite(&redemptions[0])),
            "the first admission is kept as membership evidence"
        );
        assert_eq!(
            row.admissions
                .iter()
                .map(|r| r.invite_id)
                .collect::<Vec<_>>(),
            redemptions[total - MAX_RETAINED_IDENTITY_ADMISSIONS..]
                .iter()
                .map(invite)
                .collect::<Vec<_>>(),
            "only the latest later receipts stay on the identity row, in order"
        );
        for redemption in &redemptions {
            assert!(
                governed
                    .invitation_receipts
                    .contains_key(&hex::encode(invite(redemption))),
                "every receipt, pruned or not, stays in the per-invite ledger"
            );
        }

        let pruned = &redemptions[1];
        assert!(
            !row.admissions.iter().any(|r| r.invite_id == invite(pruned)),
            "fixture: this receipt was pruned from the identity row"
        );
        assert!(
            peer.remove_member(guest.1.user_public_key, &authority, now)
                .unwrap()
        );
        assert!(peer.permissions(guest.1.device_public_key, now).is_err());
        assert!(
            !peer.admit_invited_member(pruned, &authority, now).unwrap(),
            "replaying a pruned redemption is already applied, not a new admission"
        );
        assert!(
            peer.permissions(guest.1.device_public_key, now).is_err(),
            "the removed member stays removed"
        );
        let other = device(&root, 63);
        let replay = redemption(&peer, &authority, 101, &other, now_ns);
        assert_eq!(replay.join().invite_id(), invite(pruned));
        assert!(
            peer.admit_invited_member(&replay, &authority, now)
                .unwrap_err()
                .contains("already redeemed"),
            "a pruned invitation stays spent for every other device"
        );
    }
}
