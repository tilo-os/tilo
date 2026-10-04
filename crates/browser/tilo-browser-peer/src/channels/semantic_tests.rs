//! Isolated real-MLS fixtures; no model, UI input or external identity grants.
use super::*;
use channel_crypto::ChannelSteward;
use ed25519_dalek::{Signer as _, SigningKey};
use std::collections::{BTreeMap, BTreeSet};
use tilo_relay_protocol::live::{LiveFrame, LiveRecord, OpsFrame, SealedLiveFrame, SemanticFrame};
use tilo_user_identity::{DeviceCapabilities, RevocationReason};

pub(crate) struct Fixture {
    pub(crate) channels: Channels,
    pub(crate) route: DocumentRoute,
    pub(crate) root: SigningKey,
    pub(crate) publisher: SigningKey,
    pub(crate) cipher: KeysCipher,
    steward: ChannelSteward,
    vault: VaultKey,
    _directory: tempfile::TempDir,
}
pub(crate) fn fixture() -> Fixture {
    let root = SigningKey::from_bytes(&[71; 32]);
    let publisher = SigningKey::from_bytes(&[72; 32]);
    let browser = SigningKey::from_bytes(&[73; 32]);
    let route = tests::route("semantic-test");
    let vault = VaultKey::new([74; 32]);
    let directory = tempfile::tempdir().unwrap();
    let mut steward = ChannelSteward::open_or_create(
        directory.path(),
        Channels::channel(&route),
        publisher.clone(),
        &vault,
    )
    .unwrap();
    let mut channels = Channels::new(tests::device(&browser), tests::device(&root), [75; 32]);
    channels.open(&route, 0);
    channels.note_cert(
        tests::device(&publisher),
        tests::cert(&root, &publisher, DeviceCapabilities::primary()),
        0,
    );
    let changes = channels.receive_frame(
        tests::device(&publisher),
        &route.cluster_id,
        steward.journal().frames()[0].clone(),
        0,
    );
    let mut request = changes
        .outgoing
        .into_iter()
        .find_map(|out| match out {
            Outgoing::Request { request, .. } => Some(request),
            Outgoing::Ack { .. } => None,
        })
        .unwrap();
    request.signature = collab_types::Signature::from_bytes(
        browser.sign(&request.signing_bytes().unwrap()).to_bytes(),
    );
    let allowed = BTreeSet::from([
        Ed25519PublicKey::from_bytes(tests::device(&publisher)),
        request.device,
    ]);
    steward
        .reconcile(
            &allowed,
            &BTreeMap::from([(request.device, request)]),
            &vault,
        )
        .unwrap();
    let mut cipher = None;
    for outgoing in steward.outbox() {
        for frame in outgoing.frames {
            let changes =
                channels.receive_frame(tests::device(&publisher), &route.cluster_id, frame, 0);
            for (_, keys) in changes.keys {
                cipher = Some(KeysCipher(keys));
            }
        }
    }
    Fixture {
        channels,
        route,
        root,
        publisher,
        cipher: cipher.unwrap(),
        steward,
        vault,
        _directory: directory,
    }
}
impl Fixture {
    pub(crate) fn payload(&self) -> serde_json::Value {
        serde_json::json!({"version":1,"invocation":tilo_actions::InvocationId::new(),"sequence":1,
            "actor":{"id":hex::encode(tests::device(&self.root)),"kind":"person"},
            "phase":{"phase":"applying"}, "action":{"id":"update","label":"Update"},
            "resource":{"id":"test:record","kind":"test.record","label":"Record"}, "anchor":{"kind":"resource"}})
    }
    pub(crate) fn seal(&self, payload: &serde_json::Value) -> Vec<u8> {
        let frame = LiveFrame::Ops(OpsFrame::Semantic {
            record: LiveRecord {
                app: self.route.app_id.clone(),
                record: self.route.record.clone(),
            },
            event: SemanticFrame {
                version: 1,
                seq: 1,
                sent_at_ms: 1,
                execution: tilo_actions::InvocationId::new().0,
                peer: 7,
                interaction_json: payload.to_string(),
            },
        });
        let keys = self.steward.content_keys().unwrap();
        SealedLiveFrame::seal(&frame, |artifact, retention, bytes| {
            keys.seal_content(artifact, retention, bytes)
        })
        .unwrap()
    }
    fn open(&mut self, bytes: &[u8]) -> Option<crate::session::VerifiedSemanticLive> {
        self.channels.open_semantic(
            &self.route,
            tests::device(&self.publisher),
            bytes,
            10,
            &mut self.cipher,
        )
    }
}

#[test]
fn semantic_open_binds_current_publisher_record_and_canonical_actor() {
    let mut f = fixture();
    let bytes = f.seal(&f.payload());
    let verified = f.open(&bytes).unwrap();
    assert_eq!(verified.user, tests::device(&f.root));
    assert_eq!(verified.publisher, tests::device(&f.publisher));
    let mut other = f.route.clone();
    other.record = "other".into();
    assert!(
        f.channels
            .open_semantic(&other, verified.publisher, &bytes, 10, &mut f.cipher)
            .is_none()
    );
    assert!(
        f.channels
            .open_semantic(&f.route, [99; 32], &bytes, 10, &mut f.cipher)
            .is_none()
    );
    for (field, value) in [
        ("actor", serde_json::json!({"id":"forged","kind":"person"})),
        ("sequence", serde_json::json!(0)),
        (
            "invocation",
            serde_json::json!("00000000-0000-0000-0000-000000000000"),
        ),
    ] {
        let mut payload = f.payload();
        payload[field] = value;
        let bytes = f.seal(&payload);
        assert!(f.open(&bytes).is_none());
    }
}

#[test]
fn semantic_revocations_are_signed_root_scoped_and_additive() {
    let mut f = fixture();
    let bytes = f.seal(&f.payload());
    let foreign_root = SigningKey::from_bytes(&[88; 32]);
    let mut entry = RevocationEntry {
        user_public_key: tests::device(&foreign_root),
        revoked_device_public_key: tests::device(&f.publisher),
        revoked_at_ns: 2,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &foreign_root).unwrap();
    f.channels.note_revocations([&entry]);
    assert!(
        f.open(&bytes).is_some(),
        "a different root cannot revoke this certificate"
    );
    entry.user_public_key = tests::device(&f.root);
    f.channels.note_revocations([&entry]);
    assert!(f.open(&bytes).is_some(), "wrong signature is not authority");
    tilo_user_identity::sign_revocation_entry(&mut entry, &f.root).unwrap();
    f.channels.note_revocations([&entry]);
    assert!(f.open(&bytes).is_none());
    f.channels.note_revocations(std::iter::empty());
    assert!(
        f.open(&bytes).is_none(),
        "omission never restores a revoked publisher"
    );
}

#[test]
fn semantic_open_refuses_missing_expired_and_replaced_certificates() {
    let mut f = fixture();
    let bytes = f.seal(&f.payload());
    let publisher = tests::device(&f.publisher);
    let certificate = f.channels.certs.remove(&publisher).unwrap();
    assert!(f.open(&bytes).is_none());
    let mut expired = certificate.clone();
    expired.expires_at_ns = Some(5);
    tilo_user_identity::sign_device_cert(&mut expired, &f.root).unwrap();
    f.channels.note_cert(publisher, expired, 0);
    assert!(f.open(&bytes).is_none());
    let mut mismatch = certificate.clone();
    mismatch.device_public_key = [99; 32];
    tilo_user_identity::sign_device_cert(&mut mismatch, &f.root).unwrap();
    f.channels.note_cert(publisher, mismatch, 0);
    assert!(f.open(&bytes).is_none());
    f.channels.note_cert(publisher, certificate, 0);
    assert!(f.open(&bytes).is_some());
    f.channels.pin_governor(&f.route.cluster_id, [99; 32], 0);
    assert!(
        f.open(&bytes).is_none(),
        "cached keys do not replace current governor membership"
    );
}

#[test]
fn semantic_open_refuses_removed_members_despite_retained_decryption_keys() {
    let mut f = fixture();
    let bytes = f.seal(&f.payload());
    assert!(f.open(&bytes).is_some());
    f.steward
        .reconcile(
            &BTreeSet::from([Ed25519PublicKey::from_bytes(tests::device(&f.publisher))]),
            &BTreeMap::new(),
            &f.vault,
        )
        .unwrap();
    for outgoing in f.steward.outbox() {
        for frame in outgoing.frames {
            f.channels
                .receive_frame(tests::device(&f.publisher), &f.route.cluster_id, frame, 0);
        }
    }
    let sealed = SealedLiveFrame::decode(&bytes).unwrap();
    assert!(
        sealed
            .open_in_record(
                &f.route.cluster_id,
                &LiveRecord {
                    app: f.route.app_id.clone(),
                    record: f.route.record.clone()
                },
                |artifact, metadata, payload| f.cipher.open(artifact, metadata, payload)
            )
            .is_ok()
    );
    assert!(
        f.open(&bytes).is_none(),
        "historical decryptability is not current authority"
    );
}

#[test]
fn semantic_open_does_not_create_routes_or_dirty_membership() {
    let mut f = fixture();
    let bytes = f.seal(&f.payload());
    let _ = f.channels.export_if_changed();
    let routes = f.channels.routes.len();
    assert!(f.open(&bytes).is_some());
    let mut unknown = f.route.clone();
    unknown.record = "unopened".into();
    assert!(
        f.channels
            .open_semantic(
                &unknown,
                tests::device(&f.publisher),
                &bytes,
                10,
                &mut f.cipher
            )
            .is_none()
    );
    assert_eq!(f.channels.routes.len(), routes);
    assert!(f.channels.export_if_changed().is_none());
}

#[test]
fn semantic_open_rejects_old_epochs_with_a_still_admitted_publisher() {
    let mut f = fixture();
    let bytes = f.seal(&f.payload());
    let extra = SigningKey::from_bytes(&[87; 32]);
    let mut member = ChannelMember::in_memory(
        Channels::channel(&f.route),
        Ed25519PublicKey::from_bytes(tests::device(&extra)),
        Ed25519PublicKey::from_bytes(tests::device(&f.publisher)),
    )
    .unwrap();
    let request = member.access_request(&extra, 1, &f.vault).unwrap().unwrap();
    let mut allowed = f.steward.members();
    allowed.insert(request.device);
    f.steward
        .reconcile(
            &allowed,
            &BTreeMap::from([(request.device, request)]),
            &f.vault,
        )
        .unwrap();
    for outgoing in f.steward.outbox() {
        for frame in outgoing.frames {
            f.channels
                .receive_frame(tests::device(&f.publisher), &f.route.cluster_id, frame, 0);
        }
    }
    assert!(
        f.channels
            .semantic_authority(&f.route, tests::device(&f.publisher), 10)
            .is_some()
    );
    assert!(
        f.open(&bytes).is_none(),
        "an admitted publisher cannot replay a historical key epoch"
    );
}

#[test]
fn semantic_revocation_and_overflow_survive_reload_without_reannouncement() {
    for overflow in [false, true] {
        let mut f = fixture();
        let bytes = f.seal(&f.payload());
        let _ = f.channels.export_if_changed();
        let mut entry = RevocationEntry {
            user_public_key: tests::device(&f.root),
            revoked_device_public_key: tests::device(&f.publisher),
            revoked_at_ns: 2,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &f.root).unwrap();
        f.channels.note_revocations([&entry]);
        if overflow {
            f.channels.revocation_overflow = true;
        }
        let saved = f
            .channels
            .export_if_changed()
            .expect("revocation is durable channel state");
        let encoded = postcard::to_allocvec(&saved).unwrap();
        let saved = postcard::from_bytes(&encoded).unwrap();
        let browser = SigningKey::from_bytes(&[73; 32]);
        let mut reloaded = Channels::new(tests::device(&browser), tests::device(&f.root), [75; 32]);
        reloaded.restore(saved);
        reloaded.open(&f.route, 0);
        reloaded.note_cert(
            tests::device(&f.publisher),
            tests::cert(&f.root, &f.publisher, DeviceCapabilities::primary()),
            0,
        );
        reloaded.note_revocations(std::iter::empty());
        assert!(
            reloaded
                .open_semantic(
                    &f.route,
                    tests::device(&f.publisher),
                    &bytes,
                    10,
                    &mut f.cipher
                )
                .is_none()
        );
        assert_eq!(reloaded.revocation_overflow, overflow);
    }
}

#[test]
fn semantic_invalid_persisted_revocation_fails_closed() {
    let mut f = fixture();
    let bytes = f.seal(&f.payload());
    let mut saved = f.channels.export_if_changed().unwrap();
    saved.revocations.push(RevocationEntry {
        user_public_key: tests::device(&f.root),
        revoked_device_public_key: tests::device(&f.publisher),
        revoked_at_ns: 2,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    });
    f.channels.restore(saved);
    assert!(f.channels.revocation_overflow);
    assert!(f.open(&bytes).is_none());
}
