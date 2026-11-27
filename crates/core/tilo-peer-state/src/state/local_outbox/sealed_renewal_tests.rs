use super::super::sealed_compaction_tests::held;
use super::*;
use crate::{InputDisposition, TransportOrigin};
use automerge::transaction::Transactable as _;
use collab_types::{EncryptionMetadata, SignedOp};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::Duration;

const APP: &str = "test.app";
const RECORD: &str = "tab:1";

struct Fixture {
    owner_copy: StatePeer,
    member: StatePeer,
    owner_copy_key: SigningKey,
    key: SigningKey,
    cert: DeviceCert,
    now: SystemTime,
    write: NeighbourMsg,
}

/// A member holding its own sealed write, which never reached the host.
fn fixture() -> Fixture {
    let (mut owner_copy, owner_copy_key, key, cert, now) = crate::admission_tests::seeded();
    owner_copy.select_exact_admissions().unwrap();
    let mut member = StatePeer::new(
        owner_copy.workspace_id(),
        owner_copy.owner_certificates()[0].clone(),
        vec![cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    member.select_exact_admissions().unwrap();
    for message in owner_copy
        .publication_mut()
        .bootstrap_messages(&owner_copy_key, now)
        .unwrap()
    {
        member
            .receive_from(origin(&owner_copy_key), message, now)
            .unwrap();
    }
    let write = sealed(&member, &key, &cert, now, "sealed by this member's app");
    member
        .admit_new_local_document(APP, RECORD, envelope(&write).clone(), now)
        .unwrap();
    Fixture {
        owner_copy,
        member,
        owner_copy_key,
        key,
        cert,
        now,
        write,
    }
}

fn origin(key: &SigningKey) -> TransportOrigin {
    crate::fixture::origin(key)
}

fn metadata(epoch: u64) -> EncryptionMetadata {
    EncryptionMetadata::content_key(epoch, collab_types::ContentRetention::Delta)
}

/// The member's app write to [`RECORD`], its `value` set to `text`: `cert`'s device drafts it
/// through the production writer, which seals it under the workspace's member keys, and `key`,
/// that device's key, signs it. The device's own edit names the person its certificate is for, as
/// every change a device signs names who made it.
fn sealed(
    member: &StatePeer,
    key: &SigningKey,
    cert: &DeviceCert,
    at: SystemTime,
    text: &str,
) -> NeighbourMsg {
    let keys = crate::sealed_records::members_keys(member.workspace_id());
    let mut document = automerge::AutoCommit::load(&member.document(APP, RECORD).unwrap()).unwrap();
    let heads = document.get_heads();
    document.put(automerge::ROOT, "value", text).unwrap();
    document.commit_with(
        automerge::transaction::CommitOptions::default().with_message(
            tilo_actions::ActionContext::whole_edit_by(
                &tilo_actions::Acting::person(hex::encode(cert.user_public_key)),
                &document,
            ),
        ),
    );
    let changes = document.save_after(&heads);
    let (envelopes, _) = crate::owner::draft_local_write(
        member,
        member.workspace_id(),
        &cert.device_public_key,
        crate::owner::LocalWrite {
            app: APP,
            document: RECORD,
            readers: None,
            changes: &changes,
            founding: false,
            snapshot: false,
            sealed: Some(&keys),
            commit: None,
            now: at,
        },
    )
    .unwrap();
    let [mut envelope] = <[_; 1]>::try_from(envelopes).expect("one frame");
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: APP.into(),
        cluster_id: member.workspace_id().into(),
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

fn envelope(message: &NeighbourMsg) -> &SignedOp {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a document write")
    };
    envelope
}

/// The key holder's answer while the write's epoch is current.
fn identical(op: &SignedOp) -> Result<(EncryptionMetadata, Vec<u8>), String> {
    Ok((op.encryption.ok_or("not sealed")?, op.payload.clone()))
}

fn expired(now: SystemTime) -> SystemTime {
    now.checked_add(Duration::from_secs(301)).unwrap()
}

/// The host admits `message` and records, in governance `member`
/// admits, its signed durable receipt over the record's watermarks. The
/// member holds no key for the record, so that receipt, not a cleartext
/// checkpoint, is what acknowledges its write.
///
/// A renewal is numbered past the write it renews, so an owner copy that never saw
/// that write holds the renewal for the history before it and asks its sender for
/// it. The sender still keeps the original write, unretired, and answers with it
/// as retained history; the renewal is taken in with it.
fn acknowledge(fixture: &mut Fixture, message: NeighbourMsg, at: SystemTime) {
    let payload = envelope(&message).payload.clone();
    let signature = envelope(&message).signature;
    let report = fixture
        .owner_copy
        .receive_from_report(origin(&fixture.key), message, at)
        .unwrap();
    let primary = report.primary().clone();
    let mut taken = matches!(primary, InputDisposition::Accepted(_));
    if !taken {
        let original = fixture.write.clone();
        let history = fixture
            .owner_copy
            .receive_from_report(
                TransportOrigin::RetainedServerDelta(fixture.key.verifying_key().to_bytes()),
                original,
                at,
            )
            .unwrap();
        assert!(
            matches!(history.primary(), InputDisposition::Accepted(_)),
            "the host takes the write before the renewal as history: {:?}",
            history.primary()
        );
        taken = history.released().iter().any(|released| {
            matches!(
                &released.message,
                NeighbourMsg::AppDocument { envelope: held, .. } if held.signature == signature
            )
        });
    }
    assert!(taken, "the host takes the renewal: {primary:?}");
    let owner_copy = &mut fixture.owner_copy;
    let authority = owner_copy.owner_certificates()[0].device_public_key;
    let document = &owner_copy.core.documents[&Route {
        app: APP.into(),
        record: RECORD.into(),
    }];
    let mut watermarks: BTreeMap<String, u64> = document
        .channel
        .last_seq_per_author
        .iter()
        .map(|(author, seq)| (hex::encode(author.as_bytes()), *seq))
        .collect();
    let sequence = watermarks[&hex::encode(authority)].checked_add(1).unwrap();
    watermarks.insert(hex::encode(authority), sequence);
    let coverage = crate::coverage::Coverage {
        workspace: owner_copy.workspace_id().into(),
        app: APP.into(),
        record: RECORD.into(),
        payload_hash: *blake3::hash(&payload).as_bytes(),
        heads: document
            .doc
            .base_heads()
            .iter()
            .map(ToString::to_string)
            .collect(),
        author: authority,
        sequence,
        watermarks,
    };
    let workspace = owner_copy.core.workspace_doc.as_deref_mut().unwrap();
    let generation = crate::durability::entries(workspace)
        .unwrap()
        .values()
        .map(|receipt| receipt.generation)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .unwrap();
    crate::coverage::put_all_with_receipts(workspace, vec![coverage], |rows| {
        rows.iter()
            .map(|(route, row)| {
                Ok((
                    route.clone(),
                    crate::durability::DurableReceipt::new(row.clone(), generation)?,
                ))
            })
            .collect()
    })
    .unwrap();
    let governance = owner_copy.workspace_message();
    fixture
        .member
        .receive_from(origin(&fixture.owner_copy_key), governance, at)
        .unwrap();
}

tilo_test_env::scenario! {
    fn a_sealed_write_is_renewed_only_for_its_current_author_by_the_key_holder;

    phase an_expired_sealed_write_renews_its_ciphertext_under_the_next_sequence_and_retires_with_it;
    {
        let mut fixture = fixture();
        let author = fixture.key.verifying_key().to_bytes();
        let later = expired(fixture.now);
        let renewal = fixture
            .renew(&fixture.write.clone(), &mut |_, _, op| identical(op), later)
            .unwrap()
            .expect("an expired unretired sealed write renews");
        let (original, renewed) = (envelope(&fixture.write), envelope(&renewal));
        collab_types::verify_op(renewed).unwrap();
        assert_eq!(
            renewed.payload, original.payload,
            "the ciphertext is identical"
        );
        assert_eq!(renewed.encryption, original.encryption);
        assert_eq!(
            (
                renewed.artifact,
                renewed.attestation_id,
                renewed.kind,
                renewed.author
            ),
            (
                original.artifact,
                original.attestation_id,
                original.kind,
                original.author
            )
        );
        assert_eq!(renewed.seq, original.seq.checked_add(1).unwrap());
        assert_eq!(renewed.timestamp, later);
        assert!(crate::outbox::fresh(&renewal, later));
        assert!(
            fixture
                .member
                .lookup_exact_local_document(APP, RECORD, renewed, later)
                .unwrap()
                .is_some(),
            "the renewal is admitted exactly, as a new local sealed write"
        );
        assert_eq!(held(&fixture.member).last(), Some(renewed));
        assert_eq!(
            fixture.renew(&renewal, &mut |_, _, op| identical(op), later).unwrap(),
            None,
            "a fresh renewal is not renewed again"
        );
        assert!(
            !fixture
                .member
                .outbox()
                .retained_local_document_retired(&renewal, author)
                .unwrap()
        );

        acknowledge(&mut fixture, renewal.clone(), later);
        for message in [&renewal, &fixture.write] {
            assert!(
                fixture
                    .member
                    .outbox()
                    .retained_local_document_retired(message, author)
                    .unwrap(),
                "the host's watermark over the renewal retires both"
            );
        }
        assert_eq!(
            fixture
                .renew(&fixture.write.clone(), &mut |_, _, op| identical(op), later)
                .unwrap(),
            None,
            "a retired write is not renewed"
        );
    }

    phase a_write_from_an_older_epoch_renews_with_what_the_key_holder_resealed;
    {
        let mut fixture = fixture();
        let later = expired(fixture.now);
        let current = metadata(2);
        let renewal = fixture
            .renew(
                &fixture.write.clone(),
                &mut |_, _, _| Ok((current, b"resealed under epoch 2".to_vec())),
                later,
            )
            .unwrap()
            .unwrap();
        assert_eq!(envelope(&renewal).encryption, Some(current));
        assert_eq!(envelope(&renewal).payload, b"resealed under epoch 2");
        collab_types::verify_op(envelope(&renewal)).unwrap();
    }

    phase a_refused_reseal_or_a_fresh_write_changes_nothing;
    {
        let mut fixture = fixture();
        let before = fixture.member.durable().unwrap();
        let (mut asked, write) = (0, fixture.write.clone());
        assert_eq!(
            fixture
                .renew(
                    &write,
                    &mut |_, _, _| {
                        asked += 1;
                        identical(envelope(&write))
                    },
                    fixture.now
                )
                .unwrap(),
            None,
            "a fresh write is not renewed"
        );
        assert_eq!(asked, 0);
        assert!(
            fixture
                .renew(
                    &fixture.write.clone(),
                    &mut |_, _, _| Err("this device holds no current key".into()),
                    expired(fixture.now)
                )
                .is_err()
        );
        assert_eq!(
            fixture.member.durable().unwrap(),
            before,
            "a refusal leaves the write exactly as it was"
        );
    }

    phase a_revoked_author_renews_nothing;
    {
        let mut fixture = fixture();
        let later = expired(fixture.now);
        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: fixture.cert.user_public_key,
            revoked_device_public_key: fixture.cert.device_public_key,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32]))
            .unwrap();
        fixture
            .member
            .receive_from(
                TransportOrigin::UntrustedPresence(fixture.owner_copy_key.verifying_key().to_bytes()),
                NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(fixture.owner_copy.owner_certificates()[0].clone()),
                    revocations: vec![revocation],
                },
                later,
            )
            .unwrap();
        let before = fixture.member.durable().unwrap();
        let mut asked = 0;
        assert!(
            fixture
                .renew(
                    &fixture.write.clone(),
                    &mut |_, _, op| {
                        asked += 1;
                        identical(op)
                    },
                    later
                )
                .is_err()
        );
        assert_eq!(asked, 0, "nothing is resealed for a revoked author");
        assert_eq!(fixture.member.durable().unwrap(), before);
    }

    phase a_renewal_survives_restart_as_exact_ownership;
    {
        let mut fixture = fixture();
        let later = expired(fixture.now);
        let renewal = fixture
            .renew(&fixture.write.clone(), &mut |_, _, op| identical(op), later)
            .unwrap()
            .unwrap();
        let restored = StatePeer::restore(
            &fixture.member.durable().unwrap(),
            fixture.member.workspace_id(),
            fixture.member.owner_certificates()[0].clone(),
            vec![fixture.cert.clone()],
            RevocationList::new(),
            later,
        )
        .unwrap();
        let author = fixture.key.verifying_key().to_bytes();
        for message in [&fixture.write, &renewal] {
            assert!(
                restored
                    .outbox()
                    .lookup_retained_local_document(message, author)
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(held(&restored), held(&fixture.member));
        fixture.member = restored;
        assert_eq!(
            fixture.renew(&renewal, &mut |_, _, op| identical(op), later).unwrap(),
            None,
            "the restored renewal is still fresh"
        );
        acknowledge(&mut fixture, renewal.clone(), later);
        assert!(
            fixture
                .member
                .outbox()
                .retained_local_document_retired(&renewal, author)
                .unwrap()
        );
    }
}

impl Fixture {
    /// `original` renewed as its owner renews it: drafted, signed by its author and admitted.
    fn renew(
        &mut self,
        original: &NeighbourMsg,
        reseal: &mut crate::Reseal<'_>,
        now: SystemTime,
    ) -> Result<Option<NeighbourMsg>, String> {
        let author = self.key.verifying_key().to_bytes();
        let drafted = self.member.outbox().draft_sealed_renewals(
            std::slice::from_ref(original),
            reseal,
            author,
            now,
        );
        let Some(mut renewal) = drafted.into_iter().next().expect("one outcome")? else {
            return Ok(None);
        };
        collab_types::sign_op(&mut renewal, &self.key).map_err(|error| error.to_string())?;
        self.member
            .outbox_mut()
            .admit_sealed_renewal(original, renewal, now)
            .map(Some)
    }
}
