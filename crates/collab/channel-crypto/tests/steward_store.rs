//! A steward kept in memory and exported, as a browser keeps it, signing
//! through a key that answers later.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use channel_crypto::{
    Access, ChannelMember, ChannelSteward, Error, MemoryStewardStore, SignFuture, StewardRecord,
    StewardSigner, StewardStore, VaultKey,
};
use collab_types::{ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey};
use ed25519_dalek::SigningKey;

fn vault() -> VaultKey {
    VaultKey::new([5; 32])
}

fn public(key: &SigningKey) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes())
}

/// Answers each signature on the second poll, as a promise does.
struct LaterSigner(SigningKey);

struct Later(Option<[u8; 64]>, bool);

impl Future for Later {
    type Output = channel_crypto::Result<[u8; 64]>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if std::mem::replace(&mut self.1, true) {
            Poll::Ready(
                self.0
                    .take()
                    .ok_or_else(|| Error::Signer("polled twice".into())),
            )
        } else {
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

impl StewardSigner for LaterSigner {
    fn device(&self) -> Ed25519PublicKey {
        public(&self.0)
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.0, &message).to_bytes();
        Box::pin(Later(Some(signature), false))
    }
}

/// A memory store that keeps an export after every write: each one is what
/// a page that died right then would reload.
struct Recording {
    inner: MemoryStewardStore,
    exports: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl StewardStore for Recording {
    fn read(&self, record: StewardRecord) -> channel_crypto::Result<Option<Vec<u8>>> {
        self.inner.read(record)
    }

    fn write(&mut self, record: StewardRecord, bytes: &[u8]) -> channel_crypto::Result<()> {
        self.inner.write(record, bytes)?;
        self.exports.lock().unwrap().push(self.inner.export()?);
        Ok(())
    }
}

fn open<S: StewardStore>(store: S, channel: ChannelId, key: &SigningKey) -> ChannelSteward<S> {
    pollster::block_on(ChannelSteward::open(
        store,
        channel,
        Box::new(LaterSigner(key.clone())),
        &vault(),
    ))
    .unwrap()
}

fn waiting(channel: ChannelId, device: &SigningKey, steward: &SigningKey) -> ChannelMember {
    ChannelMember::in_memory(channel, public(device), public(steward)).unwrap()
}

fn asked(
    member: &mut ChannelMember,
    device: &SigningKey,
) -> BTreeMap<Ed25519PublicKey, collab_types::MlsAccessRequestV1> {
    let request = member
        .access_request(device, 1, &vault())
        .unwrap()
        .expect("a waiting member asks");
    BTreeMap::from([(public(device), request)])
}

fn deliver<S: StewardStore>(steward: &ChannelSteward<S>, member: &mut ChannelMember) {
    for outgoing in steward.outbox() {
        if outgoing.recipient == member.device() {
            for frame in outgoing.frames {
                member.receive(frame, &vault()).unwrap();
            }
        }
    }
}

#[test]
fn a_signer_that_answers_later_signs_every_frame() {
    let channel = ChannelId::new();
    let browser = SigningKey::from_bytes(&[1; 32]);
    let laptop = SigningKey::from_bytes(&[2; 32]);
    let mut steward = open(MemoryStewardStore::new(), channel, &browser);
    assert!(steward.journal().encryption_required());

    let mut member = waiting(channel, &laptop, &browser);
    let requests = asked(&mut member, &laptop);
    let allowed = BTreeSet::from([public(&laptop)]);
    // The synchronous call cannot wait for this signer, and leaves nothing
    // half done behind.
    assert!(matches!(
        steward.reconcile(&allowed, &requests, &vault()),
        Err(Error::Signer(_))
    ));
    assert_eq!(steward.epoch(), 0);

    let outcome =
        pollster::block_on(steward.reconcile_async(&allowed, &requests, &vault())).unwrap();
    assert_eq!(outcome.admitted, vec![public(&laptop)]);
    deliver(&steward, &mut member);
    assert_eq!(member.access(), Access::Ready { epoch: 1 });

    let artifact = ArtifactId::new();
    let (metadata, ciphertext) = steward
        .content_keys()
        .unwrap()
        .seal_content(artifact, ContentRetention::Delta, b"from the browser")
        .unwrap();
    assert_eq!(
        member
            .content_keys()
            .unwrap()
            .open_content(artifact, &metadata, &ciphertext)
            .unwrap()
            .as_slice(),
        b"from the browser"
    );
}

#[test]
fn an_export_reopens_as_the_same_steward() {
    let channel = ChannelId::new();
    let browser = SigningKey::from_bytes(&[3; 32]);
    let laptop = SigningKey::from_bytes(&[4; 32]);
    let mut steward = open(MemoryStewardStore::new(), channel, &browser);
    let mut member = waiting(channel, &laptop, &browser);
    let requests = asked(&mut member, &laptop);
    pollster::block_on(steward.reconcile_async(
        &BTreeSet::from([public(&laptop)]),
        &requests,
        &vault(),
    ))
    .unwrap();

    let exported = steward.store().export().unwrap();
    let reopened = open(
        MemoryStewardStore::import(&exported).unwrap(),
        channel,
        &browser,
    );
    assert_eq!(reopened.epoch(), steward.epoch());
    assert_eq!(reopened.members(), steward.members());
    assert_eq!(reopened.outbox(), steward.outbox());

    // Another vault or another device cannot open it.
    assert!(
        pollster::block_on(ChannelSteward::open(
            MemoryStewardStore::import(&exported).unwrap(),
            channel,
            Box::new(browser),
            &VaultKey::new([6; 32]),
        ))
        .is_err()
    );
    assert!(
        pollster::block_on(ChannelSteward::open(
            MemoryStewardStore::import(&exported).unwrap(),
            channel,
            Box::new(laptop),
            &vault(),
        ))
        .is_err()
    );
}

#[test]
fn every_point_a_change_can_stop_at_reopens_whole() {
    let channel = ChannelId::new();
    let browser = SigningKey::from_bytes(&[7; 32]);
    let laptop = SigningKey::from_bytes(&[8; 32]);
    let exports = Arc::new(Mutex::new(Vec::new()));
    let mut steward = open(
        Recording {
            inner: MemoryStewardStore::new(),
            exports: Arc::clone(&exports),
        },
        channel,
        &browser,
    );
    let before = exports.lock().unwrap().len();
    let mut member = waiting(channel, &laptop, &browser);
    let requests = asked(&mut member, &laptop);
    let allowed = BTreeSet::from([public(&laptop)]);
    pollster::block_on(steward.reconcile_async(&allowed, &requests, &vault())).unwrap();
    let during: Vec<Vec<u8>> = exports.lock().unwrap().split_off(before);
    assert_eq!(
        during.len(),
        4,
        "staged group, ledger, journal, merged group"
    );

    for (step, export) in during.iter().enumerate() {
        let mut reopened = open(
            MemoryStewardStore::import(export).unwrap(),
            channel,
            &browser,
        );
        let frames = reopened.journal().frames().len();
        let recorded = step >= 2;
        assert_eq!(frames, if recorded { 2 } else { 1 }, "step {step}");
        assert_eq!(reopened.epoch(), u64::from(recorded), "step {step}");
        assert_eq!(
            reopened.members().contains(&public(&laptop)),
            recorded,
            "step {step}"
        );
        // Whatever it reopened as, it finishes the change, and the member
        // whose request it answers joins.
        pollster::block_on(reopened.reconcile_async(&allowed, &requests, &vault())).unwrap();
        assert_eq!(reopened.epoch(), 1, "step {step}");
        let mut joiner = member_from(&member);
        deliver(&reopened, &mut joiner);
        assert!(
            matches!(joiner.access(), Access::Ready { .. }),
            "step {step}"
        );
    }
}

/// An independent copy of a waiting member, through its export.
fn member_from(member: &ChannelMember) -> ChannelMember {
    ChannelMember::restore_in_memory(
        member.channel_id(),
        member.device(),
        member.steward(),
        &member.export_sealed(&vault()).unwrap(),
        &vault(),
    )
    .unwrap()
}
