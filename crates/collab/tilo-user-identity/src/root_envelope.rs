//! A user root a browser holds: the root signing key wrapped under keys only
//! the person has, as a JSON document anyone may store.
//!
//! ```json
//! {"version": 1, "root_public_key": "<hex>", "wraps": [
//!   {"kind": "passkey", "credential_id": "…", "rp_id": "tilo.so",
//!    "prf_salt": "…", "nonce": "…", "ciphertext": "…"},
//!   {"kind": "recovery", "salt": "…", "nonce": "…", "ciphertext": "…"}]}
//! ```
//!
//! Binary fields are unpadded base64url. Each wrap seals the root's PKCS#8
//! (RFC 8410) encoding with [`tilo_seal::web`]: AES-256-GCM under
//! `HKDF-SHA256(ikm, salt)`, so a page unwraps it straight into a
//! non-extractable `WebCrypto` key. A passkey wrap's keying material is the
//! credential's `WebAuthn` PRF output for `prf_salt` (also the HKDF salt); a
//! recovery wrap's is the [`RecoveryKey`]. The associated data binds each
//! wrap to its kind, the root public key and, for a passkey, the credential.
//! Whatever opens a wrap checks the key against `root_public_key`.

use data_encoding::BASE64URL_NOPAD;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, web};
use zeroize::Zeroizing;

/// The only envelope version.
pub const ROOT_ENVELOPE_VERSION: u32 = 1;
/// Length of a PRF input and of a recovery wrap's salt.
pub const SALT_LEN: usize = 32;
/// Random bytes in a [`RecoveryKey`].
pub const RECOVERY_KEY_LEN: usize = 20;
/// PKCS#8 v1 prefix of an Ed25519 private key (RFC 8410), before the seed.
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// Why an envelope did not open or could not be built.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RootEnvelopeError {
    /// Not a version this build reads.
    #[error("unsupported root envelope version {0}")]
    Version(u32),
    /// A field does not decode.
    #[error("malformed root envelope: {0}")]
    Malformed(&'static str),
    /// No wrap of the asked kind (or credential).
    #[error("no matching wrap in the root envelope")]
    NoWrap,
    /// Wrong key, or the envelope was modified.
    #[error("the root envelope did not open")]
    Rejected,
    /// The key does not belong to this envelope's root.
    #[error("the wrapped key is not this envelope's root")]
    WrongRoot,
    /// Randomness unavailable.
    #[error("sealing failed")]
    Seal,
}

type Result<T> = std::result::Result<T, RootEnvelopeError>;

/// One way to open the root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum RootWrap {
    /// Opened by a passkey's PRF output.
    Passkey {
        /// The `WebAuthn` credential id.
        credential_id: String,
        /// The relying party the credential is scoped to.
        rp_id: String,
        /// PRF input; also the HKDF salt.
        prf_salt: String,
        /// AES-GCM nonce.
        nonce: String,
        /// The wrapped root.
        ciphertext: String,
    },
    /// Opened by the recovery key.
    Recovery {
        /// HKDF salt.
        salt: String,
        /// AES-GCM nonce.
        nonce: String,
        /// The wrapped root.
        ciphertext: String,
    },
}

impl RootWrap {
    /// Which secret opens this wrap.
    #[must_use]
    pub const fn kind(&self) -> WrapKind {
        match self {
            Self::Passkey { .. } => WrapKind::Passkey,
            Self::Recovery { .. } => WrapKind::Recovery,
        }
    }

    /// The `WebAuthn` credential a passkey wrap needs, empty for a recovery
    /// wrap.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Malformed`] for a field that does not decode.
    pub fn credential_id(&self) -> Result<Vec<u8>> {
        match self {
            Self::Passkey { credential_id, .. } => decode(credential_id, "credential_id"),
            Self::Recovery { .. } => Ok(Vec::new()),
        }
    }

    /// The relying party a passkey credential is scoped to.
    #[must_use]
    pub fn rp_id(&self) -> Option<&str> {
        match self {
            Self::Passkey { rp_id, .. } => Some(rp_id),
            Self::Recovery { .. } => None,
        }
    }

    /// The HKDF salt: a passkey's PRF input, a recovery wrap's salt.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Malformed`] for a field that does not decode.
    pub fn salt(&self) -> Result<Vec<u8>> {
        match self {
            Self::Passkey { prf_salt, .. } => decode(prf_salt, "prf_salt"),
            Self::Recovery { salt, .. } => decode(salt, "salt"),
        }
    }

    /// The AES-GCM nonce.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Malformed`] for a field that does not decode.
    pub fn nonce(&self) -> Result<Vec<u8>> {
        match self {
            Self::Passkey { nonce, .. } | Self::Recovery { nonce, .. } => decode(nonce, "nonce"),
        }
    }

    /// The wrapped root.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Malformed`] for a field that does not decode.
    pub fn ciphertext(&self) -> Result<Vec<u8>> {
        match self {
            Self::Passkey { ciphertext, .. } | Self::Recovery { ciphertext, .. } => {
                decode(ciphertext, "ciphertext")
            }
        }
    }
}

/// The user root, wrapped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootEnvelope {
    pub version: u32,
    /// The root's Ed25519 public key, hex.
    pub root_public_key: String,
    /// Ways to open it.
    pub wraps: Vec<RootWrap>,
}

/// A random key shown to the person once: 160 bits as 32 Crockford base32
/// symbols in groups of four. Reading it back ignores case, separators and
/// the usual confusions (`O`/`0`, `I`/`L`/`1`).
pub struct RecoveryKey(Zeroizing<[u8; RECOVERY_KEY_LEN]>);

impl RecoveryKey {
    /// A fresh key.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Seal`] when randomness is unavailable.
    pub fn generate() -> Result<Self> {
        let mut bytes = Zeroizing::new([0; RECOVERY_KEY_LEN]);
        tilo_seal::fill_random(bytes.as_mut()).map_err(|_| RootEnvelopeError::Seal)?;
        Ok(Self(bytes))
    }

    /// Read a key the person typed.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Malformed`] for anything but 32 base32 symbols.
    pub fn parse(text: &str) -> Result<Self> {
        let symbols: Zeroizing<String> = Zeroizing::new(
            text.chars()
                .filter(|c| !c.is_whitespace() && *c != '-')
                .collect(),
        );
        let bytes = Zeroizing::new(
            crate::enrollment::CROCKFORD
                .decode(symbols.as_bytes())
                .map_err(|_| RootEnvelopeError::Malformed("recovery key"))?,
        );
        let bytes: [u8; RECOVERY_KEY_LEN] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| RootEnvelopeError::Malformed("recovery key"))?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    /// A key from the [`RECOVERY_KEY_LEN`] random bytes a host drew: a page
    /// takes them from `WebCrypto`'s `getRandomValues`.
    #[must_use]
    pub fn from_bytes(bytes: [u8; RECOVERY_KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The key's bytes, which a wrap seals under.
    #[must_use]
    pub fn bytes(&self) -> Zeroizing<[u8; RECOVERY_KEY_LEN]> {
        self.0.clone()
    }

    /// The key as the person writes it down.
    pub fn display(&self) -> Zeroizing<String> {
        let symbols = Zeroizing::new(crate::enrollment::CROCKFORD.encode(self.0.as_ref()));
        let groups: Vec<&str> = symbols
            .as_bytes()
            .chunks(4)
            .filter_map(|chunk| std::str::from_utf8(chunk).ok())
            .collect();
        Zeroizing::new(groups.join("-"))
    }
}

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKey(..)")
    }
}

fn decode(field: &str, what: &'static str) -> Result<Vec<u8>> {
    BASE64URL_NOPAD
        .decode(field.as_bytes())
        .map_err(|_| RootEnvelopeError::Malformed(what))
}

fn encode(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    tilo_seal::fill_random(&mut bytes).map_err(|_| RootEnvelopeError::Seal)?;
    Ok(bytes)
}

/// Which secret opens a wrap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapKind {
    /// A passkey's `WebAuthn` PRF output.
    Passkey,
    /// The recovery key.
    Recovery,
}

impl WrapKind {
    /// The kind as the envelope's JSON names it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Passkey => "passkey",
            Self::Recovery => "recovery",
        }
    }
}

/// The record a wrap's associated data names: the kind, a zero byte, the
/// root public key and, for a passkey, its credential id.
#[must_use]
pub fn wrap_record(kind: WrapKind, root: &[u8; 32], credential_id: &[u8]) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(kind.name().as_bytes());
    record.push(0);
    record.extend_from_slice(root);
    record.extend_from_slice(credential_id);
    record
}

/// The label a wrap's key derives under: the HKDF `info` of
/// `WebCrypto`'s `deriveKey`, and tilo-seal's label for the same purpose.
#[must_use]
pub const fn wrap_key_info() -> &'static str {
    Label::UserRootWrap.as_str()
}

/// The associated data a wrap's AES-GCM authenticates, which binds it to its
/// kind, the root public key and the credential.
///
/// # Errors
///
/// [`RootEnvelopeError::Malformed`] for a record too long to prefix.
pub fn wrap_aad(kind: WrapKind, root: &[u8; 32], credential_id: &[u8]) -> Result<Vec<u8>> {
    Context::new(Label::UserRootWrap, &wrap_record(kind, root, credential_id))
        .aad(&[])
        .map_err(|_| RootEnvelopeError::Malformed("wrap context"))
}

fn pkcs8(root: &SigningKey) -> Zeroizing<Vec<u8>> {
    let mut der = Zeroizing::new(Vec::with_capacity(48));
    der.extend_from_slice(&PKCS8_PREFIX);
    der.extend_from_slice(root.as_bytes());
    der
}

fn seal(ikm: &[u8], salt: &[u8], record: &[u8], root: &SigningKey) -> Result<(Vec<u8>, Vec<u8>)> {
    let context = Context::new(Label::UserRootWrap, record);
    let key = web::derive_key(ikm, salt, context).map_err(|_| RootEnvelopeError::Seal)?;
    let (nonce, ciphertext) =
        web::seal(&key, context, &pkcs8(root)).map_err(|_| RootEnvelopeError::Seal)?;
    Ok((nonce.to_vec(), ciphertext))
}

impl RootEnvelope {
    /// An envelope of `root` with no wrap yet.
    #[must_use]
    pub fn empty(root: &[u8; 32]) -> Self {
        Self {
            version: ROOT_ENVELOPE_VERSION,
            root_public_key: hex::encode(root),
            wraps: Vec::new(),
        }
    }

    /// Keep a passkey wrap sealed elsewhere: by a page's `WebCrypto`
    /// `wrapKey`, or by [`Self::add_passkey`]'s seal.
    pub fn push_passkey_wrap(
        &mut self,
        credential_id: &[u8],
        rp_id: &str,
        prf_salt: &[u8],
        nonce: &[u8],
        ciphertext: &[u8],
    ) {
        self.wraps.push(RootWrap::Passkey {
            credential_id: encode(credential_id),
            rp_id: rp_id.to_owned(),
            prf_salt: encode(prf_salt),
            nonce: encode(nonce),
            ciphertext: encode(ciphertext),
        });
    }

    /// Keep a recovery wrap sealed elsewhere, replacing any there was: the
    /// recovery key is one the person holds, not one of several.
    pub fn set_recovery_wrap(&mut self, salt: &[u8], nonce: &[u8], ciphertext: &[u8]) {
        self.wraps
            .retain(|wrap| !matches!(wrap, RootWrap::Recovery { .. }));
        self.wraps.push(RootWrap::Recovery {
            salt: encode(salt),
            nonce: encode(nonce),
            ciphertext: encode(ciphertext),
        });
    }

    /// An envelope of `root` opened by `recovery`.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Seal`] when randomness is unavailable.
    pub fn create(root: &SigningKey, recovery: &RecoveryKey) -> Result<Self> {
        let public = root.verifying_key().to_bytes();
        let salt = random::<SALT_LEN>()?;
        let (nonce, ciphertext) = seal(
            recovery.0.as_ref(),
            &salt,
            &wrap_record(WrapKind::Recovery, &public, &[]),
            root,
        )?;
        let mut envelope = Self::empty(&public);
        envelope.set_recovery_wrap(&salt, &nonce, &ciphertext);
        Ok(envelope)
    }

    /// Add a wrap opened by the passkey `credential_id` of `rp_id`, whose
    /// PRF output for `prf_salt` is `prf_output`.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::WrongRoot`] when `root` is another key,
    /// [`RootEnvelopeError::Seal`] for unusable PRF output.
    pub fn add_passkey(
        &mut self,
        root: &SigningKey,
        credential_id: &[u8],
        rp_id: &str,
        prf_salt: &[u8; SALT_LEN],
        prf_output: &[u8],
    ) -> Result<()> {
        let public = self.public_key()?;
        if root.verifying_key().to_bytes() != public {
            return Err(RootEnvelopeError::WrongRoot);
        }
        let (nonce, ciphertext) = seal(
            prf_output,
            prf_salt,
            &wrap_record(WrapKind::Passkey, &public, credential_id),
            root,
        )?;
        self.push_passkey_wrap(credential_id, rp_id, prf_salt, &nonce, &ciphertext);
        Ok(())
    }

    /// Whether this envelope opens the root anywhere else: a passkey or a
    /// recovery key wraps it.
    #[must_use]
    pub fn opens_elsewhere(&self) -> bool {
        self.wraps
            .iter()
            .any(|wrap| matches!(wrap.kind(), WrapKind::Passkey | WrapKind::Recovery))
    }

    /// The root's public key.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Version`] or [`RootEnvelopeError::Malformed`].
    pub fn public_key(&self) -> Result<[u8; 32]> {
        if self.version != ROOT_ENVELOPE_VERSION {
            return Err(RootEnvelopeError::Version(self.version));
        }
        hex::decode(&self.root_public_key)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(RootEnvelopeError::Malformed("root_public_key"))
    }

    /// Open with the recovery key.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::Rejected`] for a wrong key or a modified
    /// envelope, or the errors of [`Self::public_key`].
    pub fn open_with_recovery(&self, recovery: &RecoveryKey) -> Result<SigningKey> {
        let public = self.public_key()?;
        let mut found = false;
        for wrap in &self.wraps {
            if let RootWrap::Recovery {
                salt,
                nonce,
                ciphertext,
            } = wrap
            {
                found = true;
                let opened = open(
                    recovery.0.as_ref(),
                    &decode(salt, "salt")?,
                    &wrap_record(WrapKind::Recovery, &public, &[]),
                    &decode(nonce, "nonce")?,
                    &decode(ciphertext, "ciphertext")?,
                    public,
                );
                if opened.is_ok() {
                    return opened;
                }
            }
        }
        Err(if found {
            RootEnvelopeError::Rejected
        } else {
            RootEnvelopeError::NoWrap
        })
    }

    /// Open with the PRF output of passkey `credential_id`.
    ///
    /// # Errors
    ///
    /// [`RootEnvelopeError::NoWrap`] when no wrap names the credential,
    /// [`RootEnvelopeError::Rejected`] for wrong output or a modified
    /// envelope.
    pub fn open_with_passkey(&self, credential_id: &[u8], prf_output: &[u8]) -> Result<SigningKey> {
        let public = self.public_key()?;
        let wanted = encode(credential_id);
        let Some((prf_salt, nonce, ciphertext)) = self.wraps.iter().find_map(|wrap| match wrap {
            RootWrap::Passkey {
                credential_id,
                prf_salt,
                nonce,
                ciphertext,
                ..
            } if *credential_id == wanted => Some((prf_salt, nonce, ciphertext)),
            _ => None,
        }) else {
            return Err(RootEnvelopeError::NoWrap);
        };
        open(
            prf_output,
            &decode(prf_salt, "prf_salt")?,
            &wrap_record(WrapKind::Passkey, &public, credential_id),
            &decode(nonce, "nonce")?,
            &decode(ciphertext, "ciphertext")?,
            public,
        )
    }
}

fn open(
    ikm: &[u8],
    salt: &[u8],
    record: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    public: [u8; 32],
) -> Result<SigningKey> {
    let context = Context::new(Label::UserRootWrap, record);
    let key = web::derive_key(ikm, salt, context).map_err(|_| RootEnvelopeError::Rejected)?;
    let der =
        web::open(&key, context, nonce, ciphertext).map_err(|_| RootEnvelopeError::Rejected)?;
    let seed: [u8; 32] = der
        .strip_prefix(PKCS8_PREFIX.as_slice())
        .and_then(|seed| seed.try_into().ok())
        .ok_or(RootEnvelopeError::Malformed("wrapped key"))?;
    let root = SigningKey::from_bytes(&seed);
    if root.verifying_key().to_bytes() != public {
        return Err(RootEnvelopeError::WrongRoot);
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[5; 32])
    }

    #[test]
    fn opens_with_either_wrap_and_nothing_else() {
        let recovery = RecoveryKey::generate().unwrap();
        let mut envelope = RootEnvelope::create(&root(), &recovery).unwrap();
        envelope
            .add_passkey(&root(), b"credential", "tilo.so", &[9; 32], &[4; 32])
            .unwrap();
        let envelope: RootEnvelope =
            serde_json::from_str(&serde_json::to_string(&envelope).unwrap()).unwrap();
        let expected = root().verifying_key();
        assert_eq!(
            envelope
                .open_with_recovery(&recovery)
                .unwrap()
                .verifying_key(),
            expected
        );
        let typed = RecoveryKey::parse(&recovery.display().to_lowercase()).unwrap();
        assert_eq!(
            envelope.open_with_recovery(&typed).unwrap().verifying_key(),
            expected
        );
        assert_eq!(
            envelope
                .open_with_passkey(b"credential", &[4; 32])
                .unwrap()
                .verifying_key(),
            expected
        );
        assert_eq!(
            envelope.open_with_passkey(b"credential", &[5; 32]).err(),
            Some(RootEnvelopeError::Rejected)
        );
        assert_eq!(
            envelope.open_with_passkey(b"another", &[4; 32]).err(),
            Some(RootEnvelopeError::NoWrap)
        );
        assert_eq!(
            envelope
                .open_with_recovery(&RecoveryKey::generate().unwrap())
                .err(),
            Some(RootEnvelopeError::Rejected)
        );
        let other = SigningKey::from_bytes(&[6; 32]);
        let mut copy = envelope;
        assert_eq!(
            copy.add_passkey(&other, b"c", "tilo.so", &[1; 32], &[1; 32]),
            Err(RootEnvelopeError::WrongRoot)
        );
    }

    #[test]
    fn a_tampered_envelope_does_not_open() {
        let recovery = RecoveryKey::generate().unwrap();
        let mut envelope = RootEnvelope::create(&root(), &recovery).unwrap();
        envelope
            .add_passkey(&root(), b"credential", "tilo.so", &[9; 32], &[4; 32])
            .unwrap();

        // Another root's public key: the wraps are bound to the original.
        let mut swapped = envelope.clone();
        swapped.root_public_key =
            hex::encode(SigningKey::from_bytes(&[6; 32]).verifying_key().to_bytes());
        assert_eq!(
            swapped.open_with_recovery(&recovery).err(),
            Some(RootEnvelopeError::Rejected)
        );

        let mut flipped = envelope.clone();
        if let RootWrap::Recovery { ciphertext, .. } = &mut flipped.wraps[0] {
            let mut bytes = decode(ciphertext, "").unwrap();
            bytes[3] ^= 0x80;
            *ciphertext = encode(&bytes);
        }
        assert_eq!(
            flipped.open_with_recovery(&recovery).err(),
            Some(RootEnvelopeError::Rejected)
        );
        let mut moved = envelope.clone();
        if let RootWrap::Passkey { credential_id, .. } = &mut moved.wraps[1] {
            *credential_id = encode(b"stolen");
        }
        assert_eq!(
            moved.open_with_passkey(b"stolen", &[4; 32]).err(),
            Some(RootEnvelopeError::Rejected)
        );
        let mut salted = envelope.clone();
        if let RootWrap::Passkey { prf_salt, .. } = &mut salted.wraps[1] {
            *prf_salt = encode(&[8; 32]);
        }
        assert_eq!(
            salted.open_with_passkey(b"credential", &[4; 32]).err(),
            Some(RootEnvelopeError::Rejected)
        );

        let foreign = RootEnvelope::create(&SigningKey::from_bytes(&[6; 32]), &recovery).unwrap();
        let mut pasted = envelope.clone();
        pasted.wraps[0] = foreign.wraps[0].clone();
        assert_eq!(
            pasted.open_with_recovery(&recovery).err(),
            Some(RootEnvelopeError::Rejected)
        );

        let mut future = envelope;
        future.version = 2;
        assert_eq!(
            future.open_with_recovery(&recovery).err(),
            Some(RootEnvelopeError::Version(2))
        );
    }

    #[test]
    fn recovery_keys_read_back_as_typed() {
        let key = RecoveryKey::generate().unwrap();
        let shown = key.display();
        assert_eq!(shown.len(), 39);
        assert_eq!(shown.split('-').count(), 8);
        let sloppy = shown.replace('-', " ").to_lowercase();
        assert_eq!(*RecoveryKey::parse(&sloppy).unwrap().0, *key.0);
        assert_eq!(
            RecoveryKey::parse("0000-OOOO-1111-IIII-LLLL-llll-oooo-0000")
                .unwrap()
                .display()
                .as_str(),
            "0000-0000-1111-1111-1111-1111-0000-0000"
        );
        assert!(RecoveryKey::parse("ABCD").is_err());
        assert!(RecoveryKey::parse(&format!("{}U", shown.as_str())).is_err());
    }

    /// An envelope a page made with `WebCrypto` opens here.
    #[test]
    fn opens_an_envelope_a_page_made() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/root-envelope.json")).unwrap();
        let envelope: RootEnvelope = serde_json::from_value(fixture["envelope"].clone()).unwrap();
        let expected: [u8; 32] = envelope.public_key().unwrap();
        let recovery = RecoveryKey::parse(fixture["recoveryKey"].as_str().unwrap()).unwrap();
        assert_eq!(
            envelope
                .open_with_recovery(&recovery)
                .unwrap()
                .verifying_key()
                .to_bytes(),
            expected
        );
        let credential = decode(fixture["credentialId"].as_str().unwrap(), "").unwrap();
        let output = decode(fixture["prfOutput"].as_str().unwrap(), "").unwrap();
        assert_eq!(
            envelope
                .open_with_passkey(&credential, &output)
                .unwrap()
                .verifying_key()
                .to_bytes(),
            expected
        );
    }
}
