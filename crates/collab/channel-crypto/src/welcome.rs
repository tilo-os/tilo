//! What a device joining a channel reads its Welcome with.
//!
//! A Welcome carries the group's whole ratchet tree, one leaf per member
//! device, and RFC 9420 (12.4.3.1) has the joiner validate every leaf, its
//! signature included. In a channel of hundreds that is one Ed25519 check
//! per member, three quarters of what joining costs, and the newcomer waits
//! for it before it holds the channel's keys.
//!
//! The joiner leaves those leaf signatures unchecked and checks everything
//! else the RFC lists. Why that gives up nothing:
//!
//! - **Only the committer writes the tree.** The tree travels inside the
//!   Welcome's group information, sealed under a key derived from the joiner
//!   secret, which only the committer and the joiner hold. The joiner checks
//!   the group information's signature against the leaf it names as signer,
//!   and that this leaf names the device that signed the frame carrying the
//!   Welcome; the channel's membership must allow that device, every device
//!   the commit adds, and every device a leaf of the tree names
//!   ([`crate::ChannelPeer::receive`]). So the tree is the committer's word,
//!   whatever it holds.
//! - **The committer already holds every secret the joiner gets.** It made
//!   the joiner secret and the epoch's key schedule. A committer that lies
//!   learns nothing by lying, and a leaf signature it would have to forge
//!   proves only that whoever made the leaf holds that leaf's signature key:
//!   a committer making up a tree makes up such keys as it likes.
//! - **A tree other than the real one only splits the joiner off.** The tree
//!   hash covers every leaf, signature and all, and goes into the group
//!   context every key of the epoch is derived from. A joiner given any other
//!   tree than the members hold derives other keys, and no real commit after
//!   it applies: it is cut off from the channel, as a committer that sent it
//!   nothing at all would leave it. Even a tree that names only allowed
//!   devices and keeps their real signature keys cannot do more.
//!
//! The committer never checked every leaf it hands on: one that joined by
//! Welcome itself inherited leaves it never checked. That does not matter
//! here, because the argument above does not rest on it.
//!
//! Every other signature (the group information's, every commit's and every
//! update path's leaf after the join) is verified in full, and the tree
//! hash, parent hashes, unique keys, unmerged leaves, capabilities and
//! lifetimes are all still checked.

use std::sync::atomic::{AtomicBool, Ordering};

use openmls::prelude::tls_codec::SecretVLBytes;
use openmls_rust_crypto::{MemoryStorage, RustCrypto};
use openmls_traits::OpenMlsProvider;
use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::types::{
    AeadType, Ciphersuite, CryptoError, ExporterSecret, HashType, HpkeCiphertext, HpkeConfig,
    HpkeKeyPair, KemOutput, SignatureScheme,
};

/// What a leaf node's signature signs starts with: `SignWithLabel`'s
/// content (RFC 9420, 5.1.2) opens with its label, a one-byte length and
/// then "MLS 1.0 " and the label leaf nodes are signed under (7.2).
const LEAF_NODE_SIGNED: &[u8] = b"\x13MLS 1.0 LeafNodeTBS";

/// The provider a member key's MLS state lives with: openmls's Rust crypto
/// and memory storage, as `OpenMlsRustCrypto` puts them together, except
/// that while a Welcome is read ([`Self::reading_welcome`]) a leaf node's
/// signature passes unchecked (see the module docs). Every MLS operation
/// goes through this one type, so openmls is built once for it.
#[derive(Debug, Default)]
pub(crate) struct Provider {
    crypto: TreeAttested,
    storage: MemoryStorage,
}

impl Provider {
    /// Leave the leaf signatures of the Welcome read with this provider
    /// unchecked until the returned guard is dropped (see the module docs).
    pub(crate) fn reading_welcome(&self) -> ReadingWelcome<'_> {
        self.crypto.welcome.store(true, Ordering::Relaxed);
        ReadingWelcome(&self.crypto)
    }
}

/// While it lives, its provider reads a Welcome.
pub(crate) struct ReadingWelcome<'a>(&'a TreeAttested);

impl Drop for ReadingWelcome<'_> {
    fn drop(&mut self) {
        self.0.welcome.store(false, Ordering::Relaxed);
    }
}

impl OpenMlsProvider for Provider {
    type CryptoProvider = TreeAttested;
    type RandProvider = RustCrypto;
    type StorageProvider = MemoryStorage;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto.inner
    }
}

thread_local! {
    static LEAF_SIGNATURES_LEFT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static SIGNATURES_CHECKED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The leaf signatures left unchecked and the signatures checked in
/// full while reading Welcomes on this thread since the last call: what a
/// test that joining does not grow with the group counts.
#[doc(hidden)]
#[must_use]
pub fn take_welcome_signatures() -> (u64, u64) {
    (
        LEAF_SIGNATURES_LEFT.with(|count| count.replace(0)),
        SIGNATURES_CHECKED.with(|count| count.replace(0)),
    )
}

/// The crypto of [`Provider`].
#[derive(Debug, Default)]
pub(crate) struct TreeAttested {
    inner: RustCrypto,
    /// A Welcome is being read.
    welcome: AtomicBool,
}

impl OpenMlsCrypto for TreeAttested {
    fn supports(&self, ciphersuite: Ciphersuite) -> Result<(), CryptoError> {
        self.inner.supports(ciphersuite)
    }

    fn supported_ciphersuites(&self) -> Vec<Ciphersuite> {
        self.inner.supported_ciphersuites()
    }

    fn hkdf_extract(
        &self,
        hash_type: HashType,
        salt: &[u8],
        ikm: &[u8],
    ) -> Result<SecretVLBytes, CryptoError> {
        self.inner.hkdf_extract(hash_type, salt, ikm)
    }

    fn hmac(
        &self,
        hash_type: HashType,
        key: &[u8],
        message: &[u8],
    ) -> Result<SecretVLBytes, CryptoError> {
        self.inner.hmac(hash_type, key, message)
    }

    fn hkdf_expand(
        &self,
        hash_type: HashType,
        prk: &[u8],
        info: &[u8],
        okm_len: usize,
    ) -> Result<SecretVLBytes, CryptoError> {
        self.inner.hkdf_expand(hash_type, prk, info, okm_len)
    }

    fn hash(&self, hash_type: HashType, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.inner.hash(hash_type, data)
    }

    fn aead_encrypt(
        &self,
        alg: AeadType,
        key: &[u8],
        data: &[u8],
        nonce: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        self.inner.aead_encrypt(alg, key, data, nonce, aad)
    }

    fn aead_decrypt(
        &self,
        alg: AeadType,
        key: &[u8],
        ct_tag: &[u8],
        nonce: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        self.inner.aead_decrypt(alg, key, ct_tag, nonce, aad)
    }

    fn signature_key_gen(&self, alg: SignatureScheme) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        self.inner.signature_key_gen(alg)
    }

    fn verify_signature(
        &self,
        alg: SignatureScheme,
        data: &[u8],
        pk: &[u8],
        signature: &[u8],
    ) -> Result<(), CryptoError> {
        if self.welcome.load(Ordering::Relaxed) {
            if data.starts_with(LEAF_NODE_SIGNED) {
                LEAF_SIGNATURES_LEFT.with(|count| count.set(count.get().saturating_add(1)));
                return Ok(());
            }
            SIGNATURES_CHECKED.with(|count| count.set(count.get().saturating_add(1)));
        }
        self.inner.verify_signature(alg, data, pk, signature)
    }

    fn sign(&self, alg: SignatureScheme, data: &[u8], key: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.inner.sign(alg, data, key)
    }

    fn hpke_seal(
        &self,
        config: HpkeConfig,
        pk_r: &[u8],
        info: &[u8],
        aad: &[u8],
        ptxt: &[u8],
    ) -> Result<HpkeCiphertext, CryptoError> {
        self.inner.hpke_seal(config, pk_r, info, aad, ptxt)
    }

    fn hpke_open(
        &self,
        config: HpkeConfig,
        input: &HpkeCiphertext,
        sk_r: &[u8],
        info: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        self.inner.hpke_open(config, input, sk_r, info, aad)
    }

    fn hpke_setup_sender_and_export(
        &self,
        config: HpkeConfig,
        pk_r: &[u8],
        info: &[u8],
        exporter_context: &[u8],
        exporter_length: usize,
    ) -> Result<(KemOutput, ExporterSecret), CryptoError> {
        self.inner.hpke_setup_sender_and_export(
            config,
            pk_r,
            info,
            exporter_context,
            exporter_length,
        )
    }

    fn hpke_setup_receiver_and_export(
        &self,
        config: HpkeConfig,
        enc: &[u8],
        sk_r: &[u8],
        info: &[u8],
        exporter_context: &[u8],
        exporter_length: usize,
    ) -> Result<ExporterSecret, CryptoError> {
        self.inner.hpke_setup_receiver_and_export(
            config,
            enc,
            sk_r,
            info,
            exporter_context,
            exporter_length,
        )
    }

    fn derive_hpke_keypair(
        &self,
        config: HpkeConfig,
        ikm: &[u8],
    ) -> Result<HpkeKeyPair, CryptoError> {
        self.inner.derive_hpke_keypair(config, ikm)
    }
}

#[cfg(test)]
mod tests {
    use crate::{ChannelGroup, MemberKey};
    use collab_types::ChannelId;
    use openmls_traits::types::SignatureScheme;

    /// A channel of `members` and the Welcome admitting one more, with the
    /// key that joins with it.
    fn welcome_into(members: usize) -> (ChannelId, ChannelGroup, Vec<u8>, MemberKey) {
        let channel = ChannelId::new();
        let mut group =
            ChannelGroup::create(channel, MemberKey::generate("founder").unwrap()).unwrap();
        let packages: Vec<_> = (1..members)
            .map(|n| {
                MemberKey::generate(format!("member {n}"))
                    .unwrap()
                    .fresh_key_package()
                    .unwrap()
            })
            .collect();
        if !packages.is_empty() {
            group.stage_add(&packages).unwrap();
            group.merge_pending_transaction().unwrap();
        }
        let joiner = MemberKey::generate("joiner").unwrap();
        let (welcome, _) = group
            .add_member_with_commit(joiner.fresh_key_package().unwrap())
            .unwrap();
        (channel, group, welcome, joiner)
    }

    /// Joining a channel of 30 checks as many signatures in full as joining
    /// one of 3: the `GroupInfo`'s, which binds the whole tree. Each leaf's is
    /// left unchecked: the tree is the committer's word either way (see the
    /// module docs).
    #[test]
    fn joining_checks_the_same_signatures_however_large_the_channel() {
        let mut checked = Vec::new();
        for members in [3, 30] {
            let (channel, mut founder, welcome, joiner) = welcome_into(members);
            let _ = super::take_welcome_signatures();
            let mut newcomer = ChannelGroup::process_welcome(channel, &welcome, joiner).unwrap();
            let (left, full) = super::take_welcome_signatures();
            assert_eq!(
                left,
                u64::try_from(members + 1).unwrap(),
                "every leaf of the tree, the joiner's own among them, was left unchecked"
            );
            checked.push(full);
            assert_eq!(newcomer.member_count(), members + 1);
            let sealed = founder.encrypt(b"hello").unwrap();
            assert_eq!(newcomer.decrypt(&sealed).unwrap(), b"hello");
        }
        assert_eq!(checked[0], checked[1], "{checked:?}");
        assert!(checked[0] >= 1, "the GroupInfo's signature is checked");
    }

    /// A Welcome changed on its way is refused, wherever it was changed:
    /// the tree it carries is bound to the `GroupInfo` the committer signed and
    /// sealed to the joiner.
    #[test]
    fn a_welcome_changed_on_its_way_is_refused() {
        let (channel, _founder, welcome, joiner) = welcome_into(8);
        let mut refused = 0;
        let offsets: Vec<usize> = (0..welcome.len()).step_by(welcome.len() / 40 + 1).collect();
        for &at in &offsets {
            let mut changed = welcome.clone();
            changed[at] ^= 0x5a;
            if ChannelGroup::process_welcome(channel, &changed, joiner.duplicate().unwrap())
                .is_err()
            {
                refused += 1;
            }
        }
        assert_eq!(refused, offsets.len(), "every changed Welcome is refused");
        assert!(ChannelGroup::process_welcome(channel, &welcome, joiner).is_ok());
    }

    /// A provider leaves leaf signatures unchecked only while it reads a
    /// Welcome: everything after the join is checked in full.
    #[test]
    fn leaf_signatures_are_left_only_while_a_welcome_is_read() {
        let provider = super::Provider::default();
        let reading = provider.reading_welcome();
        assert!(provider.crypto.welcome.load(super::Ordering::Relaxed));
        drop(reading);
        assert!(!provider.crypto.welcome.load(super::Ordering::Relaxed));
    }

    /// A member's signer that breaks the signature of every leaf node it
    /// signs, and signs everything else as the member.
    struct BreaksLeaves<'a>(&'a openmls_basic_credential::SignatureKeyPair);

    impl openmls_traits::signatures::Signer for BreaksLeaves<'_> {
        fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, openmls_traits::signatures::SignerError> {
            let mut signature = self.0.sign(payload)?;
            if payload.starts_with(super::LEAF_NODE_SIGNED) {
                signature[0] ^= 1;
            }
            Ok(signature)
        }

        fn signature_scheme(&self) -> SignatureScheme {
            self.0.signature_scheme()
        }
    }

    /// A member that joined by Welcome checks every leaf signature of every
    /// commit after it in full: a commit whose update path carries a leaf
    /// with a broken signature is refused, and the same commit signed
    /// honestly is taken. Leaving leaf signatures unchecked ends with the
    /// Welcome.
    #[test]
    fn a_commit_after_the_join_with_a_broken_leaf_signature_is_refused() {
        let (channel, mut founder, welcome, joiner) = welcome_into(4);
        let mut newcomer = crate::ChannelGroup::process_welcome(channel, &welcome, joiner).unwrap();
        let _ = super::take_welcome_signatures();
        let forged = {
            use openmls::prelude::tls_codec::Serialize as _;
            let signer = BreaksLeaves(&founder.member.signer);
            let bundle = founder
                .group
                .self_update(
                    &founder.member.provider,
                    &signer,
                    openmls::prelude::LeafNodeParameters::default(),
                )
                .unwrap();
            bundle.into_messages().0.tls_serialize_detached().unwrap()
        };
        founder.discard_pending_transaction().unwrap();
        let epoch = newcomer.epoch();
        assert!(
            newcomer.process_commit(&forged).is_err(),
            "a self-update whose leaf signature is broken is refused"
        );
        assert_eq!(newcomer.epoch(), epoch);
        assert_eq!(
            super::take_welcome_signatures(),
            (0, 0),
            "nothing after the Welcome is read as one"
        );
        let honest = founder
            .stage_commit(&[], &[], &crate::ContentKeyring::new())
            .unwrap();
        founder.merge_pending_transaction().unwrap();
        newcomer.process_commit(&honest.commit).unwrap();
        assert_eq!(newcomer.epoch(), epoch + 1);
        let sealed = founder.encrypt(b"after").unwrap();
        assert_eq!(newcomer.decrypt(&sealed).unwrap(), b"after");
    }

    /// A key package whose leaf signature was tampered with is refused before
    /// any member could add it: a device's own leaf reaches an honest group
    /// only with its signature checked.
    #[test]
    fn a_leaf_with_a_broken_signature_never_reaches_a_tree() {
        let package = MemberKey::generate("someone")
            .unwrap()
            .fresh_key_package()
            .unwrap();
        let bytes = crate::encode_key_package(&package).unwrap();
        assert!(crate::decode_key_package(&bytes).is_ok());
        let mut tampered = bytes;
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(crate::decode_key_package(&tampered).is_err());
    }
}
