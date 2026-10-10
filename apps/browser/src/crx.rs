//! CRX3 package verification.
//!
//! A CRX3 file is `Cr24`, a little-endian version (3), a little-endian header
//! length, a protobuf `CrxFileHeader`, then the zip archive. The header lists
//! public keys with signatures over
//! `"CRX3 SignedData\0" || len(signed_header_data) || signed_header_data || zip`
//! and carries `signed_header_data`, whose `crx_id` is the first sixteen bytes
//! of the SHA-256 of the extension's public key. A package verifies when every
//! proof is valid and one of the proofs belongs to the key the `crx_id` names.
//! The extension identifier is that `crx_id` written with the letters a-p, the
//! same identifier Chromium shows, so installing from a package preserves the
//! identity the Web Store and the extension's own allow-lists expect.

use ring::signature::{ECDSA_P256_SHA256_ASN1, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 4] = b"Cr24";
const SIGNED_DATA_PREFIX: &[u8] = b"CRX3 SignedData\0";
/// Chromium rejects headers above 1 MiB.
const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_PROOFS: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum CrxError {
    NotCrx,
    Unsupported(u32),
    Truncated,
    Header(&'static str),
    BadSignature,
    IdentityNotProven,
}

impl std::fmt::Display for CrxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCrx => f.write_str("not a CRX package"),
            Self::Unsupported(version) => write!(f, "unsupported CRX version {version}"),
            Self::Truncated => f.write_str("truncated CRX package"),
            Self::Header(why) => write!(f, "invalid CRX header: {why}"),
            Self::BadSignature => f.write_str("CRX signature does not verify"),
            Self::IdentityNotProven => {
                f.write_str("CRX is not signed by the key its identifier names")
            }
        }
    }
}

impl std::error::Error for CrxError {}

/// A package whose signatures verified.
#[derive(Debug)]
pub struct VerifiedCrx<'a> {
    /// The Chromium extension identifier, thirty-two letters a-p.
    pub id: String,
    /// The extension's public key (DER `SubjectPublicKeyInfo`), the manifest
    /// `key` that makes an unpacked copy keep this identifier.
    pub public_key: Vec<u8>,
    /// The zip archive behind the header.
    pub archive: &'a [u8],
}

#[derive(Clone, Copy)]
enum Algorithm {
    Rsa,
    Ecdsa,
}

struct Proof<'a> {
    algorithm: Algorithm,
    public_key: &'a [u8],
    signature: &'a [u8],
}

/// Whether `bytes` begins like a CRX package.
pub fn is_crx(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// The identifier Chromium derives from a public key (DER SPKI).
pub fn extension_id(public_key: &[u8]) -> String {
    id_from_crx_id(&Sha256::digest(public_key)[..16])
}

fn id_from_crx_id(crx_id: &[u8]) -> String {
    crx_id
        .iter()
        .flat_map(|byte| {
            [
                char::from(b'a' + (byte >> 4)),
                char::from(b'a' + (byte & 15)),
            ]
        })
        .collect()
}

/// Whether `id` is a well-formed extension identifier.
pub fn is_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
}

/// Verify a CRX3 package.
///
/// # Errors
///
/// Returns an error when the package is not CRX3, its header is malformed, a
/// signature fails, or no proof belongs to the key that the `crx_id` names.
pub fn verify(bytes: &[u8]) -> Result<VerifiedCrx<'_>, CrxError> {
    if !is_crx(bytes) {
        return Err(CrxError::NotCrx);
    }
    let word = |at: usize| -> Result<u32, CrxError> {
        bytes
            .get(at..at + 4)
            .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
            .map(u32::from_le_bytes)
            .ok_or(CrxError::Truncated)
    };
    let version = word(4)?;
    if version != 3 {
        return Err(CrxError::Unsupported(version));
    }
    let header_len = word(8)? as usize;
    if header_len > MAX_HEADER_BYTES {
        return Err(CrxError::Header("header is too large"));
    }
    let header = bytes.get(12..12 + header_len).ok_or(CrxError::Truncated)?;
    let archive = &bytes[12 + header_len..];
    let (proofs, signed_data) = parse_header(header)?;
    let crx_id = parse_signed_data(signed_data)?;

    let mut message = Vec::with_capacity(SIGNED_DATA_PREFIX.len() + 4 + signed_data.len());
    message.extend_from_slice(SIGNED_DATA_PREFIX);
    let signed_len = u32::try_from(signed_data.len()).map_err(|_| CrxError::Truncated)?;
    message.extend_from_slice(&signed_len.to_le_bytes());
    message.extend_from_slice(signed_data);
    message.extend_from_slice(archive);

    let mut identified = None;
    for proof in &proofs {
        verify_proof(proof, &message)?;
        if Sha256::digest(proof.public_key)[..16] == *crx_id {
            identified = Some(proof.public_key.to_vec());
        }
    }
    let public_key = identified.ok_or(CrxError::IdentityNotProven)?;
    Ok(VerifiedCrx {
        id: id_from_crx_id(crx_id),
        public_key,
        archive,
    })
}

fn verify_proof(proof: &Proof<'_>, message: &[u8]) -> Result<(), CrxError> {
    let (_, key) = spki_key(proof.public_key).ok_or(CrxError::Header("malformed public key"))?;
    let result = match proof.algorithm {
        Algorithm::Rsa => UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key)
            .verify(message, proof.signature),
        Algorithm::Ecdsa => {
            UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key).verify(message, proof.signature)
        }
    };
    result.map_err(|_| CrxError::BadSignature)
}

/// Read one DER element with the given tag; returns its contents and the rest.
fn der(tag: u8, input: &[u8]) -> Option<(&[u8], &[u8])> {
    let (&found, rest) = input.split_first()?;
    if found != tag {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let length = rest[..count]
            .iter()
            .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte));
        (length, &rest[count..])
    };
    (rest.len() >= length).then(|| rest.split_at(length))
}

/// The algorithm identifier and the key bits of a DER `SubjectPublicKeyInfo`.
fn spki_key(spki: &[u8]) -> Option<(&[u8], &[u8])> {
    let (body, trailing) = der(0x30, spki)?;
    if !trailing.is_empty() {
        return None;
    }
    let (algorithm, rest) = der(0x30, body)?;
    let (bits, trailing) = der(0x03, rest)?;
    if !trailing.is_empty() {
        return None;
    }
    let (&unused, key) = bits.split_first()?;
    (unused == 0).then_some((algorithm, key))
}

/// Protobuf wire reader over one message.
struct Fields<'a>(&'a [u8]);

enum Value<'a> {
    Varint,
    Bytes(&'a [u8]),
}

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Result<u64, CrxError> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let (&byte, rest) = self
                .0
                .split_first()
                .ok_or(CrxError::Header("truncated varint"))?;
            self.0 = rest;
            if shift < 64 {
                value |= u64::from(byte & 0x7f) << shift;
            }
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(CrxError::Header("varint is too long"))
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), CrxError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        Some((|| {
            let key = self.varint()?;
            let number = key >> 3;
            let value = match key & 7 {
                0 => {
                    self.varint()?;
                    Value::Varint
                }
                1 | 5 => {
                    let size = if key & 7 == 1 { 8 } else { 4 };
                    if self.0.len() < size {
                        return Err(CrxError::Header("truncated fixed field"));
                    }
                    self.0 = &self.0[size..];
                    Value::Varint
                }
                2 => {
                    let size = usize::try_from(self.varint()?)
                        .map_err(|_| CrxError::Header("field is too large"))?;
                    if self.0.len() < size {
                        return Err(CrxError::Header("truncated field"));
                    }
                    let (value, rest) = self.0.split_at(size);
                    self.0 = rest;
                    Value::Bytes(value)
                }
                _ => return Err(CrxError::Header("unsupported wire type")),
            };
            Ok((number, value))
        })())
    }
}

fn parse_header(header: &[u8]) -> Result<(Vec<Proof<'_>>, &[u8]), CrxError> {
    let mut proofs = Vec::new();
    let mut signed_data = None;
    for field in Fields(header) {
        let (number, value) = field?;
        let Value::Bytes(bytes) = value else {
            continue;
        };
        match number {
            2 | 3 => {
                if proofs.len() >= MAX_PROOFS {
                    return Err(CrxError::Header("too many proofs"));
                }
                let mut public_key = None;
                let mut signature = None;
                for inner in Fields(bytes) {
                    match inner? {
                        (1, Value::Bytes(key)) => public_key = Some(key),
                        (2, Value::Bytes(sig)) => signature = Some(sig),
                        _ => {}
                    }
                }
                proofs.push(Proof {
                    algorithm: if number == 2 {
                        Algorithm::Rsa
                    } else {
                        Algorithm::Ecdsa
                    },
                    public_key: public_key.ok_or(CrxError::Header("proof without key"))?,
                    signature: signature.ok_or(CrxError::Header("proof without signature"))?,
                });
            }
            10000 => signed_data = Some(bytes),
            _ => {}
        }
    }
    let signed_data = signed_data.ok_or(CrxError::Header("no signed header data"))?;
    if proofs.is_empty() {
        return Err(CrxError::Header("no proofs"));
    }
    Ok((proofs, signed_data))
}

fn parse_signed_data(signed_data: &[u8]) -> Result<&[u8], CrxError> {
    for field in Fields(signed_data) {
        if let (1, Value::Bytes(id)) = field? {
            return if id.len() == 16 {
                Ok(id)
            } else {
                Err(CrxError::Header("crx_id is not sixteen bytes"))
            };
        }
    }
    Err(CrxError::Header("no crx_id"))
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Build signed CRX3 packages for tests.
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

    pub fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    pub fn field(number: u64, bytes: &[u8]) -> Vec<u8> {
        let mut out = varint(number << 3 | 2);
        out.extend(varint(bytes.len() as u64));
        out.extend_from_slice(bytes);
        out
    }

    fn der_len(length: usize) -> Vec<u8> {
        if length < 0x80 {
            vec![length as u8]
        } else {
            let bytes: Vec<u8> = length
                .to_be_bytes()
                .into_iter()
                .skip_while(|byte| *byte == 0)
                .collect();
            let mut out = vec![0x80 | bytes.len() as u8];
            out.extend(bytes);
            out
        }
    }

    /// DER SPKI for an uncompressed P-256 point.
    pub fn p256_spki(point: &[u8]) -> Vec<u8> {
        // id-ecPublicKey + prime256v1
        let algorithm: &[u8] = &[
            0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
            0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
        ];
        let mut bits = vec![0x00];
        bits.extend_from_slice(point);
        let mut bit_string = vec![0x03];
        bit_string.extend(der_len(bits.len()));
        bit_string.extend(bits);
        let mut body = algorithm.to_vec();
        body.extend(bit_string);
        let mut out = vec![0x30];
        out.extend(der_len(body.len()));
        out.extend(body);
        out
    }

    pub struct Signer {
        pair: EcdsaKeyPair,
        pub spki: Vec<u8>,
    }

    impl Signer {
        pub fn new() -> Self {
            let rng = SystemRandom::new();
            let pkcs8 =
                EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
            let pair =
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
                    .unwrap();
            let spki = p256_spki(pair.public_key().as_ref());
            Self { pair, spki }
        }

        fn sign(&self, message: &[u8]) -> Vec<u8> {
            self.pair
                .sign(&SystemRandom::new(), message)
                .unwrap()
                .as_ref()
                .to_vec()
        }
    }

    /// A CRX3 whose crx_id names `identity`, with each of `signers` proving.
    pub fn crx(identity: &Signer, signers: &[&Signer], archive: &[u8]) -> Vec<u8> {
        let crx_id = &Sha256::digest(&identity.spki)[..16];
        let signed_data = field(1, crx_id);
        let mut message = SIGNED_DATA_PREFIX.to_vec();
        message.extend((signed_data.len() as u32).to_le_bytes());
        message.extend(&signed_data);
        message.extend(archive);
        let mut header = Vec::new();
        for signer in signers {
            let mut proof = field(1, &signer.spki);
            proof.extend(field(2, &signer.sign(&message)));
            header.extend(field(3, &proof));
        }
        header.extend(field(10000, &signed_data));
        let mut out = b"Cr24".to_vec();
        out.extend(3u32.to_le_bytes());
        out.extend((header.len() as u32).to_le_bytes());
        out.extend(header);
        out.extend_from_slice(archive);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{Signer, crx};
    use super::*;

    #[test]
    fn verifies_ids_and_rejects_tampering_and_foreign_identities() {
        let key = Signer::new();
        let publisher = Signer::new();
        let archive = b"PK-pretend-zip-bytes".to_vec();

        // phase: signed_package_verifies_and_names_the_extension
        let package = crx(&key, &[&key, &publisher], &archive);
        let verified = verify(&package).unwrap();
        assert_eq!(verified.archive, &archive[..]);
        assert_eq!(verified.id, extension_id(&key.spki));
        assert!(is_extension_id(&verified.id));
        assert_eq!(verified.public_key, key.spki);

        // phase: tampered_archive_fails
        let mut tampered = package.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(verify(&tampered).unwrap_err(), CrxError::BadSignature);

        // phase: identity_must_be_proven_by_its_own_key
        let impostor = crx(&key, &[&publisher], &archive);
        assert_eq!(verify(&impostor).unwrap_err(), CrxError::IdentityNotProven);

        // phase: malformed_headers_are_rejected
        assert_eq!(verify(b"PK\x03\x04").unwrap_err(), CrxError::NotCrx);
        assert_eq!(verify(&package[..10]).unwrap_err(), CrxError::Truncated);
        let mut v2 = package.clone();
        v2[4] = 2;
        assert_eq!(verify(&v2).unwrap_err(), CrxError::Unsupported(2));
        let mut oversized = package.clone();
        oversized[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(verify(&oversized), Err(CrxError::Header(_))));
        let mut short_header = package;
        short_header[8..12].copy_from_slice(&1000u32.to_le_bytes());
        assert!(verify(&short_header).is_err());
    }

    #[test]
    fn extension_ids_use_letters_a_to_p() {
        // The identifier of a known key hash prefix: 0x00..0x0f map to a..p.
        assert_eq!(id_from_crx_id(&[0x01, 0x23, 0xef, 0xff]), "abcdoppp");
        assert!(!is_extension_id("short"));
        assert!(!is_extension_id(&"q".repeat(32)));
        assert!(is_extension_id(&"a".repeat(32)));
    }

    /// Real Web Store packages verify; set `TILO_TEST_CRX` to one.
    #[test]
    #[ignore = "needs a downloaded Web Store package"]
    fn real_web_store_package_verifies() {
        let path = std::env::var("TILO_TEST_CRX").unwrap();
        let bytes = std::fs::read(path).unwrap();
        let verified = verify(&bytes).unwrap();
        eprintln!("verified {}", verified.id);
        assert!(is_extension_id(&verified.id));
    }
}
