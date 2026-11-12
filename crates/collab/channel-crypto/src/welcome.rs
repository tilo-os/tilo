//! What a device joining a channel reads its Welcome with.
//!
//! A Welcome carries the group's whole ratchet tree, one leaf per member
//! device, and RFC 9420 (12.4.3.1) has the joiner validate every leaf, its
//! signature included. In a channel of hundreds that is one Ed25519 check
//! per member, three quarters of what joining costs, and the newcomer waits
//! for it before it holds the channel's keys.
//!
//! Those leaf signatures are the one check left to the committer here; the
//! joiner still checks everything else the RFC lists. The trust argument:
//!
//! - The tree is bound to the `GroupInfo`: its tree hash covers every leaf,
//!   signature and all, and the joiner checks that hash against the
//!   `GroupInfo` and the `GroupInfo`'s signature against the committer's leaf.
//!   The frame carrying the Welcome is signed by the committer's device, and
//!   the channel's membership must name that device and every device the
//!   commit adds ([`crate::backs`]). Nobody but that committer can hand the
//!   joiner a tree.
//! - Every leaf in the committer's tree was checked, signature included,
//!   when the commit that added or updated it was taken in: openmls verifies
//!   an added key package's leaf and every update path's leaf, on the
//!   committer as on every member. The committer attests a tree it checked.
//! - A leaf's signature proves only that whoever made the leaf holds the
//!   leaf's own signature key. A committer that lies about the tree makes
//!   any leaf it likes with a valid signature by generating that key
//!   itself, so the check refuses no tree a dishonest committer could not
//!   otherwise send. Which device a leaf belongs to is never read from its
//!   signature: Tilo binds it with frames and access requests signed by the
//!   device's own key.
//!
//! So skipping that one check on a Welcome's tree is exactly as strong as
//! making it. Every other signature (the `GroupInfo`'s, every commit's and
//! every update path's leaf after the join) is verified as before, and the
//! tree hash, parent hashes, unique keys, unmerged leaves, capabilities and
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
    /// until the returned guard is dropped to its committer.
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

/// The leaf signatures left to the committer and the signatures checked in
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
    /// the committer's, which checked it when the leaf came in.
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
                "every leaf of the tree, the joiner's own among them, was left to the committer"
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

    /// The party the joiner leaves leaf signatures to checks them: a key
    /// package whose leaf signature was tampered with is refused before any
    /// committer could add it.
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
