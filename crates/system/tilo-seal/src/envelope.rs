//! At-rest envelopes: XChaCha20-Poly1305 under a symmetric key or a
//! password, with a self-describing, versioned header.
//!
//! Layout (version 1, all integers little-endian):
//!
//! ```text
//! 0..5    magic  "TSEAL"
//! 5       format version = 0x01
//! 6       algorithm      = 0x01 (XChaCha20-Poly1305)
//! 7       key source     = 0x01 raw key | 0x02 Argon2id password
//! ..      key source parameters:
//!           raw key:  key id (8 bytes, see `SymmetricKey::key_id`)
//!           Argon2id: m_cost KiB (u32), t_cost (u32), p_cost (u32),
//!                     salt length (u8), salt
//! ..      nonce (24 random bytes)
//! ..      ciphertext + 16-byte tag
//! ```
//!
//! The AEAD associated data is the whole header (magic through nonce),
//! then the [`Label`] string and caller context, each length-prefixed.
//! Every header field is therefore authenticated, and an envelope moved to
//! another purpose or record (a different [`Context`]) fails to open.
//! Nonces are 192-bit random values, so no counter state is needed and
//! collisions are negligible for any realistic number of writes per key.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keys::{KEY_ID_LEN, KEY_LEN, KeyId, SymmetricKey, fill_random};
use crate::label::Label;

/// Magic prefix of every at-rest envelope.
pub const MAGIC: &[u8; 5] = b"TSEAL";
/// Current envelope format version.
pub const FORMAT_VERSION: u8 = 1;
const ALG_XCHACHA20POLY1305: u8 = 1;
const SOURCE_RAW_KEY: u8 = 1;
const SOURCE_ARGON2ID: u8 = 2;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

/// What an envelope is bound to: a registered purpose plus caller bytes
/// naming the record (a path, record id, channel id…). Both are mandatory
/// in the sense that they are always authenticated; pass `&[]` only when
/// the label alone identifies the record.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    label: Label,
    record: &'a [u8],
}

impl<'a> Context<'a> {
    /// Bind to `label` and the record identifier `record`.
    pub const fn new(label: Label, record: &'a [u8]) -> Self {
        Self { label, record }
    }

    /// The purpose this context names.
    pub const fn label(&self) -> Label {
        self.label
    }

    pub(crate) fn aad(&self, header: &[u8]) -> Result<Vec<u8>> {
        let label = self.label.as_bytes();
        let label_len = u16::try_from(label.len()).map_err(|_| Error::Malformed("label"))?;
        let record_len =
            u32::try_from(self.record.len()).map_err(|_| Error::Malformed("context too long"))?;
        let mut aad = Vec::with_capacity(
            header
                .len()
                .saturating_add(label.len())
                .saturating_add(self.record.len())
                .saturating_add(6),
        );
        aad.extend_from_slice(header);
        aad.extend_from_slice(&label_len.to_le_bytes());
        aad.extend_from_slice(label);
        aad.extend_from_slice(&record_len.to_le_bytes());
        aad.extend_from_slice(self.record);
        Ok(aad)
    }
}

/// Argon2id cost parameters, stored in every password envelope so they can
/// be raised later without breaking old files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasswordParams {
    /// Memory cost in KiB.
    pub m_cost_kib: u32,
    /// Number of passes.
    pub t_cost: u32,
    /// Degree of parallelism.
    pub p_cost: u32,
}

impl PasswordParams {
    /// The default for new password envelopes (64 MiB, 3 passes, 1 lane).
    pub const DEFAULT: Self = Self {
        m_cost_kib: 64 * 1024,
        t_cost: 3,
        p_cost: 1,
    };
    /// Upper bounds accepted when *opening*, so a crafted header cannot
    /// make the reader allocate or spin without limit.
    pub const MAX_M_COST_KIB: u32 = 4 * 1024 * 1024;
    /// Maximum passes accepted when opening.
    pub const MAX_T_COST: u32 = 64;
    /// Maximum lanes accepted when opening.
    pub const MAX_P_COST: u32 = 64;

    fn to_argon2(self) -> Result<Params> {
        if self.m_cost_kib > Self::MAX_M_COST_KIB {
            return Err(Error::PasswordParams("memory cost too high"));
        }
        if self.t_cost > Self::MAX_T_COST || self.t_cost == 0 {
            return Err(Error::PasswordParams("pass count out of range"));
        }
        if self.p_cost > Self::MAX_P_COST || self.p_cost == 0 {
            return Err(Error::PasswordParams("lane count out of range"));
        }
        Params::new(self.m_cost_kib, self.t_cost, self.p_cost, Some(KEY_LEN))
            .map_err(|_| Error::PasswordParams("rejected by Argon2"))
    }
}

impl Default for PasswordParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Minimum and default salt length for password envelopes.
pub const SALT_LEN: usize = 16;
const MAX_SALT_LEN: usize = 64;

/// Derive a 256-bit key from a password with Argon2id v1.3.
///
/// # Errors
///
/// [`Error::PasswordParams`] for out-of-range parameters or salts.
pub fn derive_password_key(
    password: &[u8],
    salt: &[u8],
    params: PasswordParams,
) -> Result<SymmetricKey> {
    if salt.len() < 8 || salt.len() > MAX_SALT_LEN {
        return Err(Error::PasswordParams("salt length out of range"));
    }
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.to_argon2()?);
    let mut out = Zeroizing::new([0_u8; KEY_LEN]);
    argon
        .hash_password_into(password, salt, out.as_mut())
        .map_err(|_| Error::PasswordParams("derivation failed"))?;
    Ok(SymmetricKey::from_bytes(*out))
}

fn cipher(key: &SymmetricKey) -> Result<XChaCha20Poly1305> {
    XChaCha20Poly1305::new_from_slice(key.expose_secret()).map_err(|_| Error::InvalidKey)
}

fn seal_with_header(
    key: &SymmetricKey,
    mut header: Vec<u8>,
    context: Context<'_>,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let mut nonce = [0_u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    header.extend_from_slice(&nonce);
    let aad = context.aad(&header)?;
    let ciphertext = cipher(key)?
        .encrypt(
            <&XNonce>::from(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| Error::Seal)?;
    header.extend_from_slice(&ciphertext);
    Ok(header)
}

fn header_prefix(source: u8) -> Vec<u8> {
    let mut header = Vec::with_capacity(64);
    header.extend_from_slice(MAGIC);
    header.push(FORMAT_VERSION);
    header.push(ALG_XCHACHA20POLY1305);
    header.push(source);
    header
}

/// Seal `plaintext` at rest under `key`, bound to `context`.
///
/// # Errors
///
/// [`Error::Randomness`] when no nonce can be drawn.
pub fn seal(key: &SymmetricKey, context: Context<'_>, plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut header = header_prefix(SOURCE_RAW_KEY);
    header.extend_from_slice(&key.key_id().0);
    seal_with_header(key, header, context, plaintext)
}

/// Seal `plaintext` under a key derived from `password` with Argon2id and
/// `params`; a fresh random salt is stored in the header.
///
/// # Errors
///
/// [`Error::PasswordParams`] for unacceptable parameters,
/// [`Error::Randomness`] when no salt/nonce can be drawn.
pub fn seal_with_password(
    password: &[u8],
    params: PasswordParams,
    context: Context<'_>,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let mut salt = [0_u8; SALT_LEN];
    fill_random(&mut salt)?;
    let key = derive_password_key(password, &salt, params)?;
    let mut header = header_prefix(SOURCE_ARGON2ID);
    header.extend_from_slice(&params.m_cost_kib.to_le_bytes());
    header.extend_from_slice(&params.t_cost.to_le_bytes());
    header.extend_from_slice(&params.p_cost.to_le_bytes());
    header.push(u8::try_from(SALT_LEN).map_err(|_| Error::Seal)?);
    header.extend_from_slice(&salt);
    seal_with_header(&key, header, context, plaintext)
}

/// How a parsed envelope's key is obtained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// A raw symmetric key with this id.
    Key(KeyId),
    /// A password stretched with these parameters and salt.
    Password {
        /// Argon2id parameters.
        params: PasswordParams,
        /// Salt bytes.
        salt: Vec<u8>,
    },
}

/// A parsed, not yet authenticated envelope.
#[derive(Debug)]
pub struct Envelope<'a> {
    header: &'a [u8],
    nonce: [u8; NONCE_LEN],
    ciphertext: &'a [u8],
    /// Where the key comes from.
    pub source: KeySource,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Error::Malformed("length overflow"))?;
        let out = self
            .bytes
            .get(self.pos..end)
            .ok_or(Error::Malformed("truncated envelope"))?;
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(Error::Malformed("truncated envelope"))
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(
            <[u8; 4]>::try_from(bytes).map_err(|_| Error::Malformed("truncated envelope"))?,
        ))
    }
}

/// Whether `bytes` starts like an envelope of this crate (used by readers
/// that must also accept older formats).
pub fn is_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

impl<'a> Envelope<'a> {
    /// Parse the header without authenticating anything.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`], [`Error::UnsupportedVersion`] or
    /// [`Error::UnsupportedAlgorithm`] for bytes this build cannot read.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let mut reader = Reader { bytes, pos: 0 };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(Error::Malformed("bad magic"));
        }
        let version = reader.u8()?;
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let algorithm = reader.u8()?;
        if algorithm != ALG_XCHACHA20POLY1305 {
            return Err(Error::UnsupportedAlgorithm(algorithm));
        }
        let source = match reader.u8()? {
            SOURCE_RAW_KEY => KeySource::Key(KeyId(
                <[u8; KEY_ID_LEN]>::try_from(reader.take(KEY_ID_LEN)?)
                    .map_err(|_| Error::Malformed("key id"))?,
            )),
            SOURCE_ARGON2ID => {
                let params = PasswordParams {
                    m_cost_kib: reader.u32()?,
                    t_cost: reader.u32()?,
                    p_cost: reader.u32()?,
                };
                let salt_len = usize::from(reader.u8()?);
                KeySource::Password {
                    params,
                    salt: reader.take(salt_len)?.to_vec(),
                }
            }
            other => return Err(Error::UnsupportedAlgorithm(other)),
        };
        let nonce = <[u8; NONCE_LEN]>::try_from(reader.take(NONCE_LEN)?)
            .map_err(|_| Error::Malformed("nonce"))?;
        let header = bytes
            .get(..reader.pos)
            .ok_or(Error::Malformed("truncated envelope"))?;
        let ciphertext = bytes
            .get(reader.pos..)
            .ok_or(Error::Malformed("truncated envelope"))?;
        if ciphertext.len() < TAG_LEN {
            return Err(Error::Malformed("truncated envelope"));
        }
        Ok(Self {
            header,
            nonce,
            ciphertext,
            source,
        })
    }

    fn decrypt(&self, key: &SymmetricKey, context: Context<'_>) -> Result<Zeroizing<Vec<u8>>> {
        let aad = context.aad(self.header)?;
        cipher(key)?
            .decrypt(
                <&XNonce>::from(&self.nonce),
                Payload {
                    msg: self.ciphertext,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::Authentication)
    }
}

/// Open an envelope sealed by [`seal`].
///
/// # Errors
///
/// [`Error::WrongKey`] when the key id differs, [`Error::Authentication`]
/// when the context or bytes differ, or a parse error.
pub fn open(key: &SymmetricKey, context: Context<'_>, bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let envelope = Envelope::parse(bytes)?;
    match &envelope.source {
        KeySource::Key(id) if *id == key.key_id() => envelope.decrypt(key, context),
        KeySource::Key(_) | KeySource::Password { .. } => Err(Error::WrongKey),
    }
}

/// Open an envelope sealed by [`seal_with_password`], using the parameters
/// stored in its header (bounded by [`PasswordParams`]'s maxima).
///
/// # Errors
///
/// [`Error::Authentication`] for a wrong password or modified bytes,
/// [`Error::PasswordParams`] for out-of-bounds stored parameters.
pub fn open_with_password(
    password: &[u8],
    context: Context<'_>,
    bytes: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let envelope = Envelope::parse(bytes)?;
    match &envelope.source {
        KeySource::Password { params, salt } => {
            let key = derive_password_key(password, salt, *params)?;
            envelope.decrypt(&key, context)
        }
        KeySource::Key(_) => Err(Error::WrongKey),
    }
}

/// Wrap `key` under the key-encryption key `kek` for `context`.
///
/// # Errors
///
/// As [`seal`].
pub fn wrap_key(kek: &SymmetricKey, context: Context<'_>, key: &SymmetricKey) -> Result<Vec<u8>> {
    seal(kek, context, key.expose_secret())
}

/// Unwrap a key wrapped by [`wrap_key`].
///
/// # Errors
///
/// As [`open`], plus [`Error::InvalidKey`] if the plaintext is not a key.
pub fn unwrap_key(
    kek: &SymmetricKey,
    context: Context<'_>,
    wrapped: &[u8],
) -> Result<SymmetricKey> {
    SymmetricKey::from_slice(&open(kek, context, wrapped)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHEAP: PasswordParams = PasswordParams {
        m_cost_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };

    #[test]
    fn key_and_password_envelopes_round_trip_bound_with_fresh_nonces() {
        // Phase `key_envelope_round_trip_and_binding`.
        {
            let key = SymmetricKey::generate().unwrap();
            let context = Context::new(Label::SecretVault, b"record-1");
            let sealed = seal(&key, context, b"payload").unwrap();
            assert!(is_envelope(&sealed));
            assert_eq!(open(&key, context, &sealed).unwrap().as_slice(), b"payload");
            assert_eq!(
                open(&key, Context::new(Label::SecretVault, b"record-2"), &sealed),
                Err(Error::Authentication)
            );
            assert_eq!(
                open(
                    &key,
                    Context::new(Label::PasswordBackup, b"record-1"),
                    &sealed
                ),
                Err(Error::Authentication)
            );
            assert_eq!(
                open(&SymmetricKey::generate().unwrap(), context, &sealed),
                Err(Error::WrongKey)
            );
            // Every header byte is authenticated.
            for index in 0..sealed.len() {
                let mut tampered = sealed.clone();
                tampered[index] ^= 0x01;
                assert!(open(&key, context, &tampered).is_err(), "byte {index}");
            }
            assert!(open(&key, context, &sealed[..sealed.len() - 1]).is_err());
        }
        // Phase `nonces_are_fresh`.
        {
            let key = SymmetricKey::generate().unwrap();
            let context = Context::new(Label::SecretVault, b"");
            assert_ne!(
                seal(&key, context, b"same").unwrap(),
                seal(&key, context, b"same").unwrap()
            );
        }
        // Phase `password_envelope_stores_params`.
        {
            let context = Context::new(Label::IdentityBackup, b"backup");
            let sealed = seal_with_password(b"hunter2", CHEAP, context, b"identity").unwrap();
            match Envelope::parse(&sealed).unwrap().source {
                KeySource::Password { params, salt } => {
                    assert_eq!(params, CHEAP);
                    assert_eq!(salt.len(), SALT_LEN);
                }
                KeySource::Key(_) => panic!("expected a password envelope"),
            }
            assert_eq!(
                open_with_password(b"hunter2", context, &sealed)
                    .unwrap()
                    .as_slice(),
                b"identity"
            );
            assert_eq!(
                open_with_password(b"hunter3", context, &sealed),
                Err(Error::Authentication)
            );
        }
        // Phase `wrap_and_unwrap`.
        {
            let kek = SymmetricKey::generate().unwrap();
            let key = SymmetricKey::generate().unwrap();
            let context = Context::new(Label::KeyWrap, b"slot");
            let wrapped = wrap_key(&kek, context, &key).unwrap();
            assert_eq!(unwrap_key(&kek, context, &wrapped).unwrap(), key);
        }
    }

    #[test]
    fn hostile_or_unknown_envelopes_are_refused_explicitly() {
        // Phase `hostile_password_params_are_refused_before_derivation`.
        {
            let context = Context::new(Label::IdentityBackup, b"");
            let mut sealed = seal_with_password(b"pw", CHEAP, context, b"x").unwrap();
            // m_cost lives right after the 8-byte prefix.
            sealed[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(matches!(
                open_with_password(b"pw", context, &sealed),
                Err(Error::PasswordParams(_))
            ));
        }
        // Phase `unknown_version_and_algorithm_are_explicit`.
        {
            let key = SymmetricKey::generate().unwrap();
            let context = Context::new(Label::KeyWrap, b"");
            let mut sealed = seal(&key, context, b"x").unwrap();
            sealed[5] = 9;
            assert_eq!(
                open(&key, context, &sealed),
                Err(Error::UnsupportedVersion(9))
            );
            sealed[5] = FORMAT_VERSION;
            sealed[6] = 7;
            assert_eq!(
                open(&key, context, &sealed),
                Err(Error::UnsupportedAlgorithm(7))
            );
        }
    }
}
