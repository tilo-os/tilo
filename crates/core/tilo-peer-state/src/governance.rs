//! Owner-neutral transactions over the canonical workspace governance.

use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use web_time::SystemTime;

use crate::state::{DeviceRow, RedemptionReceipt};

/// How many later redemption receipts one identity row keeps beside
/// its first (`DeviceRow::admission`).
///
/// The row is a single JSON blob rewritten on every redemption, so an
/// unbounded list makes each later admission, roster read and workspace sync pay
/// for the device's whole redemption history. The first receipt is kept for
/// good: it is the admission evidence membership reads (`has_admission`, the
/// member list). Only the oldest of the later receipts are dropped,
/// and nothing they prove is lost: [`admit_invited_member`] copies every
/// receipt it has seen into the per-invite `invitation_receipts_v1` log in
/// the same transaction that drops one, and that log is what refuses a
/// second redemption of the same invitation.
pub const MAX_RETAINED_IDENTITY_ADMISSIONS: usize = 8;

/// The name a device row of a workspace's governance (an entry of its `peer_identities_v1`) gives
/// its device, when the bytes are one: what a history of the workspace calls the device it added.
#[must_use]
pub fn device_row_label(row: &[u8]) -> Option<String> {
    tilo_failure::ignored!(
        "governance bytes that are not a member's identity name no device",
        DeviceRow::decode(row).ok()
    )
    .map(|identity| identity.device_cert.device_label)
}

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

/// Apply certificate, role and immutable redemption receipt to the caller's
/// existing canonical workspace document as one transaction, written by any
/// key holder's device. The invitation is checked on its own: it
/// verifies, has not expired, names this workspace and the joiner when narrowed,
/// was issued by a member who may invite with its role, and was not
/// redeemed before. Whether its writer may admit is decided as every receiver
/// decides it ([`crate::workspace::membership`]). On error `document` remains
/// byte-for-byte unchanged.
///
/// The change is made under `document`'s actor, which the caller sets
/// ([`crate::workspace_host_actor`]).
#[expect(
    clippy::too_many_lines,
    reason = "one atomic governance transaction; on error the document stays unchanged"
)]
pub(crate) fn admit_invited_member(
    document: &mut AutoCommit,
    held: &crate::cached_document::CachedDocument,
    redemption: &crate::join::VerifiedRedemption,
    name: Option<&str>,
    now: SystemTime,
) -> Result<AdmissionOutcome, String> {
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX);
    let cert = redemption.certificate();
    tilo_user_identity::verify_device_cert_at(cert, now_ns).map_err(|error| error.to_string())?;
    let declared_workspace = document
        .get(ROOT, "workspace")
        .map_err(|error| error.to_string())?
        .and_then(|(value, _)| value.as_str().map(ToOwned::to_owned))
        .ok_or("the workspace id is missing")?;
    // Read once per governance state, not once per admission.
    let policy = held.workspace_policy(&declared_workspace);
    // The owner's device comes in on its certificate, as every receiver judges it
    // ([`crate::workspace::membership`]): the invitation's expiry does not bind it.
    let admits = if policy.is_owner(&hex::encode(cert.user_public_key)) {
        crate::workspace::membership::owner_invitation_admits
    } else {
        crate::workspace::membership::invitation_admits
    };
    let grant = admits(
        policy,
        &declared_workspace,
        redemption.join().signed_invitation(),
        cert.user_public_key,
        now_ns,
    )
    .map_err(|refusal| refusal.to_string())?;
    let receipt = RedemptionReceipt {
        invite_id: redemption.join().invite_id(),
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        request_id: redemption.request_id(),
        role: grant.member_role(),
    };
    // The receipts of this invitation: the log's under its key, and the
    // ones device rows keep ([`crate::cached_document::CachedDocument::redemptions`]).
    let mut exact_redemption = false;
    let logged =
        crate::workspace::invitation_receipts_of(document, &hex::encode(receipt.invite_id))?
            .into_iter()
            .map(|bytes| {
                crate::state::StoredReceipt::decode(&bytes)
                    .map(|logged| logged.receipt)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
    let on_rows = held.redemptions().map_err(ToOwned::to_owned)?;
    for existing in logged
        .iter()
        .chain(on_rows.get(&receipt.invite_id).into_iter().flatten())
    {
        if existing.invite_id == receipt.invite_id {
            if *existing != receipt {
                return Err("invitation was already redeemed by another request or device".into());
            }
            exact_redemption = true;
        }
    }
    if exact_redemption {
        return Ok(AdmissionOutcome::AlreadyApplied);
    }
    // A banned person stays out whatever invitation they hold, an older one
    // or a new one, until the workspace unbans them.
    if policy.is_banned(&hex::encode(cert.user_public_key)) {
        return Err("this person is banned from the workspace".into());
    }
    let id = hex::encode(cert.device_public_key);
    let prior = held
        .identities()
        .get(&id)
        .map(|identity| identity.clone().map_err(|error| error.to_string()))
        .transpose()?;
    if prior.as_ref().is_some_and(|old| {
        old.device_cert != *cert && !crate::state::supersedes(&old.device_cert, cert)
    }) {
        return Err("certificate conflict".into());
    }
    let mut candidate = document.clone();
    let before = candidate.get_heads();
    let identities = candidate
        .get(ROOT, "peer_identities_v1")
        .map_err(|error| error.to_string())?
        .ok_or("member identities missing")?
        .1;
    let names = candidate
        .get(ROOT, "peers")
        .map_err(|error| error.to_string())?
        .ok_or("peer names missing")?
        .1;
    let log =
        crate::state::workspace_map(&candidate, crate::state::invitation_receipts::RECEIPTS_KEY)?;
    // The invitation goes into the log with its issuer's certificate and
    // transport details named by digest, kept once for every receipt of an
    // invitation they signed alike ([`crate::state::invitation_receipts`]).
    let signed = redemption.join().signed_invitation();
    let shared = crate::join::invitation_shared_runs(signed);
    let (logged, parts) = crate::state::StoredReceipt::new(
        receipt.clone(),
        signed,
        &shared.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    );
    crate::state::invitation_receipts::put_parts(&mut candidate, &parts)?;
    candidate
        .put(
            &log,
            hex::encode(receipt.invite_id),
            automerge::ScalarValue::Bytes(logged.encode().map_err(|error| error.to_string())?),
        )
        .map_err(|error| error.to_string())?;
    // An agent identity stays bound to its delegation whatever else it
    // redeems: the delegation's window and withdrawal still apply.
    let (revocations, mut admission, mut admissions, delegate) = prior.map_or_else(
        || (Vec::new(), None, Vec::new(), None),
        |identity| {
            (
                identity.revocations,
                identity.admission,
                identity.admissions,
                identity.delegate,
            )
        },
    );
    if admission.is_none() {
        admission = Some(receipt);
    } else {
        admissions.push(receipt);
        retain_latest_admissions(&mut admissions);
    }
    let identity = DeviceRow {
        device_cert: cert.clone(),
        revocations,
        admission,
        admissions,
        delegate,
        workspace_delegations: Vec::new(),
        removed: false,
        entry: None,
    };
    candidate
        .put(
            &identities,
            &id,
            automerge::ScalarValue::Bytes(identity.encode().map_err(|error| error.to_string())?),
        )
        .map_err(|error| error.to_string())?;
    // The person a workspace already names keeps that name as a further device of theirs
    // arrives: the label on the arriving device names the device, and the agent running in it, not
    // its person.
    let known = name
        .map(str::to_owned)
        .or_else(|| known_person_name(held, cert.user_public_key));
    candidate
        .put(
            &names,
            &id,
            known
                .as_deref()
                .unwrap_or_else(|| member_display_name(&grant)),
        )
        .map_err(|error| error.to_string())?;
    // The name the person gave as they joined is theirs on each of their
    // admitted devices' rows, written in the admission's own change: a
    // change of its own cost a second whole governance write per arrival.
    if let Some(name) = name {
        let held_names = &held.policy().names;
        for (other, identity) in held.identities() {
            let Ok(identity) = identity else {
                continue;
            };
            if *other != id
                && identity.device_cert.user_public_key == cert.user_public_key
                && identity.has_admission()
                && held_names.get(other).map(String::as_str) != Some(name)
            {
                candidate
                    .put(&names, other, name)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    crate::state::commit_governance(&mut candidate, crate::state::now_secs(now), None);
    let changes = candidate
        .get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    *document = candidate;
    Ok(AdmissionOutcome::Applied { changes })
}

/// Apply an entry admission to the caller's workspace document as one
/// transaction, written by the joining device itself: `cert` came in under the
/// entry `entry` names, at the role that entry opens. No invitation is
/// checked and no host signed anything for it: the owner's entry is the grant,
/// and the write is the device's own.
///
/// The entry must still be the one the held workspace records, so a close or
/// an invitation-only change since the guest looked refuses it here; replay
/// checks the entry where the admission sits in the log instead.
///
/// # Errors
///
/// `cert` is not current, the entry is not the workspace's now or opens to
/// nobody, the person is banned, the row conflicts, or it cannot be written.
#[expect(
    clippy::too_many_lines,
    reason = "one atomic governance transaction; on error the document stays unchanged"
)]
pub(crate) fn admit_entered_member(
    document: &mut AutoCommit,
    held: &crate::cached_document::CachedDocument,
    cert: &tilo_user_identity::DeviceCert,
    entry: [u8; 32],
    request_id: [u8; 32],
    name: Option<&str>,
    now: SystemTime,
) -> Result<AdmissionOutcome, String> {
    let now_ns: u64 = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX);
    tilo_user_identity::verify_device_cert_at(cert, now_ns).map_err(|error| error.to_string())?;
    let declared_workspace = document
        .get(ROOT, "workspace")
        .map_err(|error| error.to_string())?
        .and_then(|(value, _)| value.as_str().map(ToOwned::to_owned))
        .ok_or("the workspace id is missing")?;
    let policy = held.workspace_policy(&declared_workspace);
    let current = held.workspace_entry();
    if crate::state::entry_digest(&current)? != entry {
        return Err("the workspace's entry is not the one this admission names".into());
    }
    let Some(role) = current.open_role() else {
        return Err("this workspace's entry does not take people who ask".into());
    };
    if policy.is_banned(&hex::encode(cert.user_public_key)) {
        return Err("this person is banned from the workspace".into());
    }
    let id = hex::encode(cert.device_public_key);
    let prior = held
        .identities()
        .get(&id)
        .map(|identity| identity.clone().map_err(|error| error.to_string()))
        .transpose()?;
    if prior.as_ref().is_some_and(|old| {
        old.device_cert != *cert && !crate::state::supersedes(&old.device_cert, cert)
    }) {
        return Err("certificate conflict".into());
    }
    let receipt = crate::state::EntryReceipt {
        entry,
        role,
        request_id,
    };
    if prior
        .as_ref()
        .is_some_and(|old| old.entry.as_ref() == Some(&receipt))
    {
        return Ok(AdmissionOutcome::AlreadyApplied);
    }
    let (revocations, admission, admissions, delegate) = prior.map_or_else(
        || (Vec::new(), None, Vec::new(), None),
        |identity| {
            (
                identity.revocations,
                identity.admission,
                identity.admissions,
                identity.delegate,
            )
        },
    );
    let identity = DeviceRow {
        device_cert: cert.clone(),
        revocations,
        admission,
        admissions,
        delegate,
        workspace_delegations: Vec::new(),
        removed: false,
        entry: Some(receipt),
    };
    let mut candidate = document.clone();
    let before = candidate.get_heads();
    let identities = candidate
        .get(ROOT, "peer_identities_v1")
        .map_err(|error| error.to_string())?
        .ok_or("member identities missing")?
        .1;
    let names = candidate
        .get(ROOT, "peers")
        .map_err(|error| error.to_string())?
        .ok_or("peer names missing")?
        .1;
    candidate
        .put(
            &identities,
            &id,
            automerge::ScalarValue::Bytes(identity.encode().map_err(|error| error.to_string())?),
        )
        .map_err(|error| error.to_string())?;
    let known = name
        .map(str::to_owned)
        .or_else(|| known_person_name(held, cert.user_public_key));
    candidate
        .put(
            &names,
            &id,
            known.as_deref().unwrap_or(cert.device_label.as_str()),
        )
        .map_err(|error| error.to_string())?;
    crate::state::commit_governance(&mut candidate, crate::state::now_secs(now), None);
    let changes = candidate
        .get_changes(&before)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    *document = candidate;
    Ok(AdmissionOutcome::Applied { changes })
}

/// A member name, bounded as every host keeps one
/// ([`tilo_user_identity::display_name`]): self-asserted text, never authority.
fn bounded_display_name(value: &str) -> Option<&str> {
    tilo_user_identity::display_name(value).ok()
}

/// The name the workspace already knows `person` by: kept under their root, else the one their
/// oldest admitted device that is theirs says — the same rule the member list names a person by
/// ([`crate::state::governance::Naming`]). `None` for a person it has not named yet.
fn known_person_name(
    held: &crate::cached_document::CachedDocument,
    person: [u8; 32],
) -> Option<String> {
    let policy = held.policy();
    if let Some(name) = policy
        .names
        .get(&hex::encode(person))
        .and_then(|name| tilo_user_identity::device_name(name))
    {
        return Some(name.to_owned());
    }
    held.identities()
        .iter()
        .filter_map(|(id, identity)| {
            let identity = identity.as_ref().ok()?;
            let cert = &identity.device_cert;
            // A name of their own: not a delegate's name (an agent's), and not the
            // stand-in a device nobody named carries.
            if cert.user_public_key != person
                || identity.delegate.is_some()
                || cert.capabilities.agent.is_some()
                || cert.capabilities.stand_in
            {
                return None;
            }
            let name = policy
                .names
                .get(id)
                .and_then(|name| tilo_user_identity::device_name(name))?;
            Some((cert.issued_at_ns, cert.device_public_key, name.to_owned()))
        })
        .min()
        .map(|(_, _, name)| name)
}

/// The name other members see when the person gave none as they joined: the one the issuer
/// named them by in the invitation, bounded display text, never authority (roles and access
/// come only from the receipt and capability rows). A device's own label is never a person's
/// name: it stays the device's, in its certificate. With no name from either, the person has
/// none yet: nothing is stored, and every surface says so in its own words.
fn member_display_name(grant: &crate::join::InviteGrant) -> &str {
    grant
        .invitee_name
        .as_deref()
        .and_then(bounded_display_name)
        .unwrap_or_default()
}

#[cfg(test)]
mod display_name_tests {
    use super::*;
    use crate::join::{
        InviteGrant, JoinChallenge, Role, WorkspaceId, issue_invitation, sign_join_proof,
        verify_join_proof,
    };
    use ed25519_dalek::SigningKey;
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
            issuer: None,
        }
    }

    fn grant(recipient: Option<[u8; 32]>, invitee_name: Option<&str>) -> InviteGrant {
        let authority = cert("owner");
        InviteGrant {
            workspace_id: WorkspaceId::from_u128(7),
            issuer: authority,
            role: Role::Editor,
            nonce: [0; 16],
            recipient_user_public_key: recipient,
            invitee_name: invitee_name.map(Into::into),
            expires_at_ns: u64::MAX,
            transport: vec![],
        }
    }

    tilo_test_env::scenario! {
        fn a_person_is_shown_by_the_bounded_name_their_invitation_gives;

        phase a_device_label_is_never_the_persons_name;
        {
            assert_eq!(member_display_name(&grant(None, Some("Gina"))), "Gina");
            assert_eq!(member_display_name(&grant(Some([1; 32]), Some("Ada"))), "Ada");
            assert_eq!(member_display_name(&grant(None, None)), "");
            assert_eq!(member_display_name(&grant(Some([1; 32]), None)), "");
        }

        phase unbounded_or_control_text_is_no_name;
        {
            for name in [
                String::new(),
                "   ".to_owned(),
                "x".repeat(129),
                "界".repeat(43),
                "Gina\nGuest".to_owned(),
                "Gina\u{7}Guest".to_owned(),
            ] {
                assert_eq!(member_display_name(&grant(None, Some(&name))), "", "{name:?}");
            }
            assert_eq!(member_display_name(&grant(None, Some(&"x".repeat(128)))), "x".repeat(128));
            // 100 bytes of UTF-8 is a name, whatever it is written in.
            for name in ["😀".repeat(25), "界".repeat(33), "x".repeat(100)] {
                assert_eq!(member_display_name(&grant(Some([1; 32]), Some(&name))), name);
            }
        }

        phase admitted_bearer_guest_is_listed_under_the_issuers_name_not_their_device;
        {
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
                issuer: None,
            };
            sign_device_cert(&mut guest_cert, &guest_root).unwrap();
            let signed = issue_invitation(
                InviteGrant {
                    workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
                    issuer: peer.owner_certificates()[0].clone(),
                    role: Role::Editor,
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
                peer.governance_mut()
                    .admit_invited_member(&redemption, &guest_key, now)
                    .unwrap()
            );
            let member = peer
                .governance()
                .member_list(now)
                .unwrap()
                .members
                .into_iter()
                .find(|member| member.root() == Some(guest_cert.user_public_key))
                .expect("admitted guest is a member");
            assert_eq!(member.name, "Guest Tester", "the invitation's name, never the device label");
            assert_eq!(member.role, Some(Role::Editor));
        }

        phase a_further_device_is_admitted_under_the_name_its_person_has;
        // A computer signed in as someone's agent carries a label naming the agent and the
        // computer ("Your agent on build-box"), never the person. Admitted as another device of
        // theirs, it is named by what the workspace already calls them.
        {
            let (mut peer, authority_key, _, _, now) = crate::admission_tests::seeded();
            let now_ns: u64 = now
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .try_into()
                .unwrap();
            let guest_root = SigningKey::from_bytes(&[61; 32]);
            let guest = |seed: u8, label: &str, issued_at_ns: u64| {
                let key = SigningKey::from_bytes(&[seed; 32]);
                let mut cert = DeviceCert {
                    user_public_key: guest_root.verifying_key().to_bytes(),
                    device_public_key: key.verifying_key().to_bytes(),
                    device_label: label.to_owned(),
                    capabilities: DeviceCapabilities::member(),
                    issued_at_ns,
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                };
                sign_device_cert(&mut cert, &guest_root).unwrap();
                (key, cert)
            };
            let admit = |peer: &mut crate::StatePeer,
                         (key, cert): &(SigningKey, DeviceCert),
                         (nonce, request, named): (u8, u8, Option<&str>)| {
                let signed = issue_invitation(
                    InviteGrant {
                        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
                        issuer: peer.owner_certificates()[0].clone(),
                        role: Role::Editor,
                        nonce: [nonce; 16],
                        recipient_user_public_key: None,
                        invitee_name: named.map(Into::into),
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
                    cert,
                    [request; 32],
                    [request.wrapping_add(1); 32],
                    now_ns.checked_add(1_000_000_000).unwrap(),
                    now_ns,
                )
                .unwrap();
                let proof = sign_join_proof(challenge.clone(), key).unwrap();
                let redemption = verify_join_proof(
                    &proof,
                    &challenge,
                    cert,
                    &RevocationList::new(),
                    now_ns,
                )
                .unwrap();
                assert!(
                    peer.governance_mut()
                        .admit_invited_member(&redemption, key, now)
                        .unwrap()
                );
            };
            let first = guest(62, "Gina Guest", 1);
            admit(&mut peer, &first, (63, 64, Some("Gina Guest")));
            // The computer she signs in with names her agent and the computer, not her.
            let agent_device = guest(65, "Your agent on build-box", 2);
            admit(&mut peer, &agent_device, (66, 67, None));
            let kept = peer.governance().kept_names();
            let row = kept
                .get(&hex::encode(agent_device.1.device_public_key))
                .expect("the admitted device has a row");
            assert_eq!(
                row, "Gina Guest",
                "a further device of a named person is admitted under their name"
            );
        }
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use crate::join::WorkspaceId;
    use crate::join::{
        InviteGrant, JoinChallenge, Role, VerifiedRedemption, issue_invitation, sign_join_proof,
        verify_join_proof,
    };
    use crate::{NeighbourMsg, StatePeer};
    use ed25519_dalek::SigningKey;
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
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    /// Invitation `nonce` from the workspace authority, redeemed by `device`.
    fn redemption(
        peer: &StatePeer,
        authority: &SigningKey,
        nonce: u8,
        (key, cert): &(SigningKey, DeviceCert),
        now_ns: u64,
    ) -> VerifiedRedemption {
        let signed = issue_invitation(
            InviteGrant {
                workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
                issuer: peer.owner_certificates()[0].clone(),
                role: Role::Editor,
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

    /// The workspace's governance the peer sends.
    fn governance(peer: &StatePeer) -> crate::workspace::WorkspaceGovernance {
        let NeighbourMsg::SyncResp { changes, .. } = peer.workspace_message() else {
            panic!("workspace message is a SyncResp")
        };
        crate::workspace::governance_of(Some(&crate::workspace::candidate(None, &changes).unwrap()))
    }

    tilo_test_env::scenario! {
        fn removed_and_pruned_identities_stay_told_apart;

        phase a_removed_members_device_is_told_apart;
        // A device the workspace removed the person of is told apart from one it
        // never named: its host's links end at the removal and refuse it after.
        {
            let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
            let now_ns: u64 = now
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .try_into()
                .unwrap();
            let guest = device(&SigningKey::from_bytes(&[71; 32]), 72);
            let stranger = device(&SigningKey::from_bytes(&[73; 32]), 74);
            let owner = peer.owner_certificates()[0].device_public_key;
            let admitted = redemption(&peer, &authority, 75, &guest, now_ns);
            assert!(
                peer.governance_mut()
                    .admit_invited_member(&admitted, &guest.0, now)
                    .unwrap()
            );
            let device = guest.1.device_public_key;
            assert!(
                !peer.governance().removed_device(device),
                "a member's device"
            );
            assert!(
                peer.governance_mut()
                    .remove_member(guest.1.user_public_key, &authority, now)
                    .unwrap()
            );
            assert!(
                peer.governance().removed_device(device),
                "a removed member's device"
            );
            assert!(
                !peer.governance().removed_device(owner),
                "the owner's device"
            );
            assert!(
                !peer
                    .governance()
                    .removed_device(stranger.1.device_public_key),
                "a device the workspace never named"
            );
        }

        phase identity_admissions_are_bounded_and_a_pruned_admission_cannot_readmit;
        {
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
                    peer.governance_mut()
                        .admit_invited_member(redemption, &guest.0, now)
                        .unwrap()
                );
            }
            let invite = |r: &VerifiedRedemption| r.join().invite_id();
            let hosted = governance(&peer);
            let row =
                DeviceRow::decode(&hosted.identities[&hex::encode(guest.1.device_public_key)])
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
                    hosted
                        .invitation_receipts
                        .contains_key(&hex::encode(invite(redemption))),
                    "every receipt, pruned or not, stays in the per-invite log"
                );
            }

            let pruned = &redemptions[1];
            assert!(
                !row.admissions.iter().any(|r| r.invite_id == invite(pruned)),
                "fixture: this receipt was pruned from the identity row"
            );
            assert!(
                peer.governance_mut()
                    .remove_member(guest.1.user_public_key, &authority, now)
                    .unwrap()
            );
            assert!(peer.permissions(guest.1.device_public_key, now).is_err());
            assert!(
                !peer
                    .governance_mut()
                    .admit_invited_member(pruned, &guest.0, now)
                    .unwrap(),
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
                peer.governance_mut()
                    .admit_invited_member(&replay, &other.0, now)
                    .unwrap_err()
                    .contains("already redeemed"),
                "a pruned invitation stays spent for every other device"
            );
        }
    }
}
