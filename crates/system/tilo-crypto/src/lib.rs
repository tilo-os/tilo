//! Narrow cryptographic operations needed by privileged OS tooling.
//!
//! Keeping these operations in Rust removes password material from subprocess
//! pipes and prevents general-purpose OpenSSL parsing from becoming part of an
//! application's privilege boundary.

use sha_crypt::{PasswordHasher, ShaCrypt};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

#[cfg(feature = "srp")]
pub mod srp_apple;
#[cfg(feature = "tls-fixture")]
mod tls_fixture;
#[cfg(feature = "tls-fixture")]
pub use tls_fixture::{TlsFixture, generate_tls_fixture};

/// Display metadata extracted from a bounded X.509 certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateInfo {
    pub subject: String,
    pub expires: String,
    pub sha256_fingerprint: String,
}

/// A PEM-encoded RSA key and self-signed certificate suitable for UEFI image
/// signing and owner enrollment.
pub struct RsaCertificate {
    pub private_key_pem: String,
    pub certificate_pem: String,
}

/// A portable RSA authority key for TPM policy authorization.
///
/// TPM implementations do not uniformly accept the 4096-bit keys used for
/// Secure Boot signing. Tilo's production PCR authority is RSA-2048, so keep
/// development and test authorities on that same interoperable path.
pub struct TpmAuthorityKey {
    private_key_pem: String,
    public_key_pem: String,
}

impl TpmAuthorityKey {
    pub fn private_key_pem(&self) -> &str {
        &self.private_key_pem
    }

    pub fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }
}

impl Drop for RsaCertificate {
    fn drop(&mut self) {
        self.private_key_pem.zeroize();
    }
}

impl Drop for TpmAuthorityKey {
    fn drop(&mut self) {
        self.private_key_pem.zeroize();
    }
}

/// Perform an RSA PKCS#1 signature with ring's blinded private-key operation.
///
/// `RustCrypto`'s RSA parser remains useful for accepting both PKCS#1 and PKCS#8
/// keys, but its private operation is affected by RUSTSEC-2023-0071. Keep key
/// parsing separate and hand canonical PKCS#8 to ring for every Tilo-owned
/// signing path.
fn sign_rsa_pkcs1_ring(
    private_key_pem: &str,
    message: &[u8],
    algorithm: &'static dyn ring::signature::RsaEncoding,
) -> Result<Vec<u8>, String> {
    use ring::{rand::SystemRandom, signature::RsaKeyPair};
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::{RsaPrivateKey, pkcs1::DecodeRsaPrivateKey, pkcs8::DecodePrivateKey};

    let key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem))
        .map_err(|_| "invalid RSA private key")?;
    let pkcs8 = key
        .to_pkcs8_der()
        .map_err(|_| "cannot normalize RSA private key")?;
    let pair =
        RsaKeyPair::from_pkcs8(pkcs8.as_bytes()).map_err(|_| "ring rejected RSA private key")?;
    let mut signature = vec![0_u8; pair.public().modulus_len()];
    pair.sign(algorithm, &SystemRandom::new(), message, &mut signature)
        .map_err(|_| "RSA signing failed")?;
    Ok(signature)
}

/// Create a salted SHA-512-crypt record accepted by Linux account tooling.
///
/// The caller's buffer is scrubbed on every return path.
///
/// # Errors
///
/// Returns an error when the password is outside the account-policy bounds or
/// the operating system cannot provide cryptographic randomness.
pub fn hash_account_password(password: &mut [u8]) -> Result<String, String> {
    if password.len() < 10 || password.len() > 1024 || password.contains(&b'\n') {
        password.zeroize();
        return Err("password must contain 10–1024 bytes and no newline".into());
    }

    // `PasswordHasher::hash_password` generates 16 random bytes, which this
    // crate serializes as a 22-character salt. Linux crypt(5) limits SHA-crypt
    // salts to 16 characters. Twelve random bytes encode to exactly that limit
    // while retaining 96 bits of entropy.
    let mut salt = [0_u8; 12];
    if let Err(error) = getrandom::fill(&mut salt) {
        salt.zeroize();
        password.zeroize();
        return Err(format!("cannot generate account password salt: {error}"));
    }
    let result = ShaCrypt::default()
        .hash_password_with_salt(password, &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| format!("cannot hash account password: {error}"));
    salt.zeroize();
    password.zeroize();
    result
}

/// Decode exactly one PEM or DER certificate, returning its DER bytes and the
/// parsed structure. PEM input must hold a single `CERTIFICATE` block followed
/// by nothing but whitespace.
fn decode_single_certificate(bytes: &[u8]) -> Result<(Vec<u8>, x509_cert::Certificate), String> {
    use der::Decode;

    let der = if bytes.starts_with(b"-----BEGIN") {
        let (label, der) = pem_certificate_block(bytes).ok_or("invalid PEM certificate")?;
        if label != "CERTIFICATE" {
            return Err("certificate contains trailing data or has the wrong PEM label".into());
        }
        der
    } else {
        bytes.to_vec()
    };
    let certificate = x509_cert::Certificate::from_der(&der).map_err(|error| {
        if matches!(error.kind(), der::ErrorKind::TrailingData { .. }) {
            "certificate contains trailing DER data"
        } else {
            "invalid X.509 certificate"
        }
    })?;
    Ok((der, certificate))
}

/// Split one RFC 7468 block into its label and decoded body.
///
/// Line width is not enforced, so certificates wrapped by tools other than
/// OpenSSL are accepted; anything but whitespace after the block is not.
fn pem_certificate_block(bytes: &[u8]) -> Option<(String, Vec<u8>)> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    let label = lines
        .next()?
        .trim_end()
        .strip_prefix("-----BEGIN ")?
        .strip_suffix("-----")?;
    let end_line = format!("-----END {label}-----");
    let mut body = String::new();
    loop {
        let line = lines.next()?.trim();
        if line == end_line {
            break;
        }
        if line.contains(':') || line.starts_with("-----") {
            return None;
        }
        body.push_str(line);
    }
    if !lines.all(|line| line.trim().is_empty()) {
        return None;
    }
    Some((label.to_owned(), STANDARD.decode(&body).ok()?))
}

/// Format a distinguished name as comma-separated `TYPE=value` pairs in
/// encoded order, joining multi-valued RDNs with ` + `.
fn display_name(name: &x509_cert::name::Name) -> String {
    name.0
        .iter()
        .map(|rdn| {
            rdn.0
                .iter()
                .map(|attribute| {
                    format!(
                        "{}={}",
                        attribute_type_abbreviation(&attribute.oid),
                        attribute_value_text(&attribute.value)
                    )
                })
                .collect::<Vec<_>>()
                .join(" + ")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn attribute_type_abbreviation(oid: &const_oid::ObjectIdentifier) -> String {
    use const_oid::db::{DB, rfc3280, rfc4519};

    let known = [
        (rfc4519::CN, "CN"),
        (rfc4519::C, "C"),
        (rfc4519::L, "L"),
        (rfc4519::ST, "ST"),
        (rfc4519::O, "O"),
        (rfc4519::OU, "OU"),
        (rfc4519::DC, "DC"),
        (rfc3280::EMAIL_ADDRESS, "Email"),
    ];
    known
        .iter()
        .find(|(known, _)| known == oid)
        .map(|(_, name)| (*name).to_owned())
        .or_else(|| DB.by_oid(oid).map(str::to_owned))
        .unwrap_or_else(|| oid.to_string())
}

fn attribute_value_text(value: &der::Any) -> String {
    use der::{Tag, Tagged as _};

    match value.tag() {
        Tag::NumericString
        | Tag::VisibleString
        | Tag::PrintableString
        | Tag::TeletexString
        | Tag::VideotexString
        | Tag::Utf8String
        | Tag::Ia5String => std::str::from_utf8(value.value())
            .map_or_else(|_| hex::encode_upper(value.value()), str::to_owned),
        Tag::BmpString => {
            let units = value
                .value()
                .chunks_exact(2)
                .filter_map(|pair| pair.try_into().ok().map(u16::from_be_bytes));
            char::decode_utf16(units)
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        }
        _ => hex::encode_upper(value.value()),
    }
}

/// Render an X.509 time as `Mon DD HH:MM:SS YYYY +00:00`.
fn display_time(time: x509_cert::time::Time) -> String {
    let format = time::macros::format_description!(
        "[month repr:short] [day padding:space] [hour]:[minute]:[second] [year padding:none] [offset_hour sign:mandatory]:[offset_minute]"
    );
    i64::try_from(time.to_unix_duration().as_secs())
        .ok()
        .and_then(|seconds| time::OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|moment| moment.format(format).ok())
        .unwrap_or_else(|| "invalid date".to_owned())
}

/// Parse PEM or DER certificate bytes without invoking a general-purpose CLI.
///
/// # Errors
///
/// Returns an error for malformed, trailing, non-certificate, or expired data.
pub fn inspect_certificate(bytes: &[u8]) -> Result<CertificateInfo, String> {
    let (der, certificate) = decode_single_certificate(bytes)?;
    let validity = &certificate.tbs_certificate.validity;
    let expires = display_time(validity.not_after);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch")?;
    if validity.not_after.to_unix_duration().as_secs() <= now.as_secs() {
        return Err(format!("certificate expired at {expires}"));
    }

    let fingerprint = Sha256::digest(&der)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    Ok(CertificateInfo {
        subject: display_name(&certificate.tbs_certificate.subject),
        expires,
        sha256_fingerprint: fingerprint,
    })
}

/// Return the lowercase SHA-256 digest of a certificate's canonical DER.
///
/// # Errors
///
/// Returns an error for malformed, trailing, or non-certificate input.
pub fn certificate_sha256(bytes: &[u8]) -> Result<String, String> {
    let (der, _) = decode_single_certificate(bytes)?;
    Ok(hex::encode(Sha256::digest(&der)))
}

/// Decode one PEM or DER X.509 certificate to its canonical DER form.
///
/// # Errors
///
/// Returns an error for malformed input, the wrong PEM label, or trailing data.
pub fn certificate_der(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use x509_cert::der::{Decode, DecodePem, Encode};

    let certificate = if bytes.starts_with(b"-----BEGIN") {
        x509_cert::Certificate::from_pem(bytes)
            .map_err(|error| format!("invalid PEM certificate: {error}"))?
    } else {
        x509_cert::Certificate::from_der(bytes)
            .map_err(|error| format!("invalid DER certificate: {error}"))?
    };
    certificate
        .to_der()
        .map_err(|error| format!("cannot encode certificate DER: {error}"))
}

/// Verify that an EFI Signature List contains one exact X.509 certificate.
///
/// Signature owners and list ordering are deliberately ignored; firmware may
/// rewrite either while the certificate DER remains the trust identity.
///
/// # Errors
///
/// Returns an error for malformed/truncated ESL data or when the certificate
/// is absent.
pub fn verify_efi_signature_list_contains_certificate(
    certificate_bytes: &[u8],
    signature_list: &[u8],
) -> Result<(), String> {
    let (found, _) = inspect_efi_signature_list(Some(certificate_bytes), signature_list)?;
    if found {
        Ok(())
    } else {
        Err("EFI signature list does not contain the supplied certificate".into())
    }
}

/// Verify that an EFI Signature List trusts exactly one entry: this certificate.
///
/// This is used before sealing an online signing key to PCR 7. Merely finding
/// the owner certificate is insufficient: any additional certificate or image
/// hash could boot another OS under the same PCR 7 value and expose that key.
///
/// # Errors
///
/// Returns an error for malformed/truncated ESL data, a missing certificate,
/// or any additional trusted entry.
pub fn verify_efi_signature_list_exclusively_contains_certificate(
    certificate_bytes: &[u8],
    signature_list: &[u8],
) -> Result<(), String> {
    let (found, entries) = inspect_efi_signature_list(Some(certificate_bytes), signature_list)?;
    if found && entries == 1 {
        Ok(())
    } else {
        Err(format!(
            "EFI signature list must contain only the supplied certificate; found {entries} entries"
        ))
    }
}

/// Validate the framing and dimensions of every list and entry in an EFI
/// Signature List blob.
///
/// # Errors
///
/// Returns an error for empty, malformed, truncated, or partially framed data.
pub fn validate_efi_signature_list(signature_list: &[u8]) -> Result<usize, String> {
    let (_, entries) = inspect_efi_signature_list(None, signature_list)?;
    if entries == 0 {
        Err("EFI signature list contains no entries".into())
    } else {
        Ok(entries)
    }
}

fn inspect_efi_signature_list(
    certificate_bytes: Option<&[u8]>,
    signature_list: &[u8],
) -> Result<(bool, usize), String> {
    const LIST_HEADER_BYTES: usize = 16 + 4 + 4 + 4;
    const SIGNATURE_OWNER_BYTES: usize = 16;
    // EFI_CERT_X509_GUID, encoded using EFI GUID byte order.
    const EFI_CERT_X509_GUID: [u8; 16] = [
        0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b, 0xf0,
        0x72,
    ];

    let certificate = certificate_bytes.map(certificate_der).transpose()?;
    let mut offset = 0_usize;
    let mut found = false;
    let mut entry_count = 0_usize;
    while offset < signature_list.len() {
        let header_end = offset
            .checked_add(LIST_HEADER_BYTES)
            .ok_or("EFI signature-list offset overflow")?;
        let header = signature_list
            .get(offset..header_end)
            .ok_or("truncated EFI signature-list header")?;
        let is_x509_list = header.get(..16) == Some(EFI_CERT_X509_GUID.as_slice());
        let read_size = |start: usize, name: &str| -> Result<usize, String> {
            let end = start
                .checked_add(4)
                .ok_or_else(|| format!("{name} offset overflow"))?;
            let encoded: [u8; 4] = header
                .get(start..end)
                .ok_or_else(|| format!("invalid {name}"))?
                .try_into()
                .map_err(|_| format!("invalid {name}"))?;
            Ok(u32::from_le_bytes(encoded) as usize)
        };
        let list_size = read_size(16, "EFI signature-list size")?;
        let signature_header_size = read_size(20, "EFI signature header size")?;
        let signature_size = read_size(24, "EFI signature size")?;
        if list_size < LIST_HEADER_BYTES
            || signature_size < SIGNATURE_OWNER_BYTES
            || LIST_HEADER_BYTES
                .checked_add(signature_header_size)
                .is_none_or(|minimum| list_size < minimum)
        {
            return Err("invalid EFI signature-list dimensions".into());
        }
        let list_end = offset
            .checked_add(list_size)
            .filter(|end| *end <= signature_list.len())
            .ok_or("truncated EFI signature list")?;
        let entries_start = header_end
            .checked_add(signature_header_size)
            .ok_or("EFI signature-list header overflow")?;
        let entries_bytes = list_end
            .checked_sub(entries_start)
            .ok_or("EFI signature entries precede their header")?;
        if entries_bytes == 0 || entries_bytes.checked_rem(signature_size) != Some(0) {
            return Err("EFI signature list has partial or no entries".into());
        }
        let entries = signature_list
            .get(entries_start..list_end)
            .ok_or("invalid EFI signature entry range")?;
        for entry in entries.chunks_exact(signature_size) {
            entry_count = entry_count
                .checked_add(1)
                .ok_or("EFI signature entry count overflow")?;
            if is_x509_list
                && certificate.as_deref().is_some_and(|certificate| {
                    entry.get(SIGNATURE_OWNER_BYTES..) == Some(certificate)
                })
            {
                found = true;
            }
        }
        offset = list_end;
    }
    Ok((found, entry_count))
}

/// Encode one X.509 certificate as an EFI Signature List suitable for db,
/// KEK, or PK enrollment. The signature-owner value is an opaque random GUID;
/// firmware uses the X.509 signature-type GUID to interpret the payload.
///
/// # Errors
///
/// Returns an error when the certificate is invalid or too large for the EFI
/// structure's 32-bit length fields, or when the operating system cannot
/// provide randomness for the owner GUID.
pub fn certificate_efi_signature_list(certificate: &[u8]) -> Result<Vec<u8>, String> {
    let der = certificate_der(certificate)?;
    let signature_size = 16_usize
        .checked_add(der.len())
        .ok_or("EFI signature entry size overflow")?;
    let list_size = 28_usize
        .checked_add(signature_size)
        .ok_or("EFI signature list size overflow")?;
    let signature_size = u32::try_from(signature_size)
        .map_err(|_| "certificate is too large for an EFI signature entry")?;
    let list_size = u32::try_from(list_size)
        .map_err(|_| "certificate is too large for an EFI signature list")?;

    let mut output = Vec::with_capacity(list_size as usize);
    // EFI_CERT_X509_GUID, encoded using EFI GUID byte order.
    output.extend_from_slice(&[
        0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b, 0xf0,
        0x72,
    ]);
    output.extend_from_slice(&list_size.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&signature_size.to_le_bytes());
    let mut owner = [0_u8; 16];
    getrandom::fill(&mut owner).map_err(|_| "operating-system random generator failed")?;
    output.extend_from_slice(&owner);
    output.extend_from_slice(&der);
    Ok(output)
}

/// Generate a 4096-bit RSA/SHA-256 self-signed certificate valid for 20 years.
///
/// # Errors
///
/// Returns an error if the OS random source or certificate encoder fails.
pub fn generate_secure_boot_certificate(common_name: &str) -> Result<RsaCertificate, String> {
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_RSA_SHA256};
    use rsa::{RsaPrivateKey, pkcs8::EncodePrivateKey};

    if common_name.is_empty()
        || common_name.len() > 200
        || common_name.chars().any(char::is_control)
    {
        return Err("certificate common name must be 1–200 printable characters".into());
    }
    let generated_key = RsaPrivateKey::new(&mut UnwrapErr(SysRng), 4096)
        .map_err(|error| format!("cannot generate RSA key: {error}"))?;
    let private_key_pem = generated_key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .map_err(|error| format!("cannot encode RSA key: {error}"))?;
    let key = KeyPair::from_pkcs8_pem_and_sign_algo(&private_key_pem, &PKCS_RSA_SHA256)
        .map_err(|error| format!("cannot load generated RSA key: {error}"))?;
    let now = time::OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    params.not_before = now
        .checked_sub(time::Duration::days(1))
        .ok_or("certificate start date overflow")?;
    params.not_after = now
        .checked_add(time::Duration::days(7300))
        .ok_or("certificate expiry date overflow")?;
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    let certificate = params
        .self_signed(&key)
        .map_err(|error| format!("cannot create certificate: {error}"))?;
    Ok(RsaCertificate {
        private_key_pem: private_key_pem.to_string(),
        certificate_pem: certificate.pem(),
    })
}

/// Generate an RSA-2048 key pair suitable for TPM policy authorization.
///
/// # Errors
///
/// Returns an error if the operating-system random source fails or either PEM
/// representation cannot be encoded.
pub fn generate_tpm_authority_key() -> Result<TpmAuthorityKey, String> {
    use getrandom::{SysRng, rand_core::UnwrapErr};
    use rsa::{
        RsaPrivateKey, RsaPublicKey,
        pkcs8::{EncodePrivateKey, EncodePublicKey},
    };

    let private = RsaPrivateKey::new(&mut UnwrapErr(SysRng), 2048)
        .map_err(|error| format!("cannot generate TPM authority key: {error}"))?;
    let public_key_pem = RsaPublicKey::from(&private)
        .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
        .map_err(|error| format!("cannot encode TPM authority public key: {error}"))?;
    let private_key_pem = private
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .map_err(|error| format!("cannot encode TPM authority private key: {error}"))?;
    Ok(TpmAuthorityKey {
        private_key_pem: private_key_pem.to_string(),
        public_key_pem,
    })
}

/// Create an RSA PKCS#1 v1.5 SHA-256 signature from a PEM private key.
///
/// # Errors
///
/// Returns an error if the key is malformed or signing fails.
pub fn sign_rsa_sha256(private_key_pem: &str, message: &[u8]) -> Result<Vec<u8>, String> {
    sign_rsa_pkcs1_ring(private_key_pem, message, &ring::signature::RSA_PKCS1_SHA256)
}

/// Derive a PKCS#8 PEM public key from a PKCS#1 or PKCS#8 RSA private key.
///
/// # Errors
///
/// Returns an error if the private key is malformed or encoding fails.
pub fn rsa_public_key_pem(private_key_pem: &str) -> Result<String, String> {
    use rsa::{
        RsaPrivateKey, RsaPublicKey, pkcs1::DecodeRsaPrivateKey, pkcs8::DecodePrivateKey,
        pkcs8::EncodePublicKey,
    };

    let private = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem))
        .map_err(|_| "invalid RSA private key")?;
    RsaPublicKey::from(&private)
        .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
        .map_err(|error| format!("cannot encode RSA public key: {error}"))
}

/// Verify that an RSA private key is the private half of an X.509 certificate.
///
/// # Errors
///
/// Returns an error for malformed inputs, a non-RSA certificate, or a mismatch.
pub fn rsa_private_key_matches_certificate(
    private_key_pem: &str,
    certificate_bytes: &[u8],
) -> Result<(), String> {
    use der::{Decode, Encode};
    use rsa::{
        RsaPrivateKey, RsaPublicKey, pkcs1::DecodeRsaPrivateKey, pkcs8::DecodePrivateKey,
        pkcs8::DecodePublicKey,
    };

    let private = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem))
        .map_err(|_| "invalid RSA private key")?;
    let certificate = x509_cert::Certificate::from_der(&certificate_der(certificate_bytes)?)
        .map_err(|error| format!("invalid X.509 certificate: {error}"))?;
    let public_der = certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|error| format!("cannot encode certificate public key: {error}"))?;
    let certificate_public = RsaPublicKey::from_public_key_der(&public_der)
        .map_err(|_| "certificate does not contain a supported RSA key")?;
    if certificate_public != RsaPublicKey::from(&private) {
        return Err("private key does not match certificate".into());
    }
    Ok(())
}

/// Verify an RSA PKCS#1 v1.5 SHA-256 signature using a PEM public key.
///
/// # Errors
///
/// Returns an error if the key/signature is malformed or verification fails.
pub fn verify_rsa_sha256(
    public_key_pem: &str,
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    use rsa::{
        RsaPublicKey,
        pkcs1::DecodeRsaPublicKey,
        pkcs1v15::{Signature, VerifyingKey},
        pkcs8::DecodePublicKey,
        sha2::Sha256,
        signature::Verifier,
    };

    let key = RsaPublicKey::from_public_key_pem(public_key_pem)
        .or_else(|_| RsaPublicKey::from_pkcs1_pem(public_key_pem))
        .map_err(|_| "invalid RSA public key")?;
    let signature = Signature::try_from(signature_bytes).map_err(|_| "invalid RSA signature")?;
    VerifyingKey::<Sha256>::new(key)
        .verify(message, &signature)
        .map_err(|_| "RSA signature verification failed".into())
}

/// Validate an RSA `SubjectPublicKeyInfo` PEM and return its modulus size and
/// canonical DER SHA-256 identity.
///
/// # Errors
///
/// Returns an error if the PEM is not a valid RSA `SubjectPublicKeyInfo` or
/// the parsed key cannot be re-encoded canonically.
pub fn rsa_public_key_info(public_key_pem: &str) -> Result<(u32, String), String> {
    use rsa::{
        RsaPublicKey,
        pkcs8::{DecodePublicKey, EncodePublicKey},
        traits::PublicKeyParts,
    };
    use sha2::{Digest as _, Sha256};

    let key = RsaPublicKey::from_public_key_pem(public_key_pem)
        .map_err(|_| "invalid RSA SubjectPublicKeyInfo")?;
    let der = key
        .to_public_key_der()
        .map_err(|_| "cannot canonicalize RSA public key")?;
    Ok((key.n().bits(), hex::encode(Sha256::digest(der.as_bytes()))))
}

/// Validate a P-256 `SubjectPublicKeyInfo` PEM and return its canonical DER
/// SHA-256 identity.
///
/// # Errors
///
/// Returns an error if the PEM is not a valid P-256 `SubjectPublicKeyInfo` or
/// the parsed key cannot be re-encoded canonically.
pub fn p256_public_key_sha256(public_key_pem: &str) -> Result<String, String> {
    use p256::{
        ecdsa::VerifyingKey,
        pkcs8::{DecodePublicKey, EncodePublicKey},
    };
    use sha2::{Digest as _, Sha256};

    let key = VerifyingKey::from_public_key_pem(public_key_pem)
        .map_err(|_| "invalid P-256 SubjectPublicKeyInfo")?;
    let der = key
        .to_public_key_der()
        .map_err(|_| "cannot canonicalize P-256 public key")?;
    Ok(hex::encode(Sha256::digest(der.as_bytes())))
}

/// Verify a detached ECDSA P-256 / SHA-256 signature over `message`, in the
/// form `cosign sign-blob` writes: base64 of the DER signature, surrounding
/// whitespace allowed.
///
/// A signature with a high `s` is accepted in its normalized form: a KMS may
/// produce either, and both prove the same signer over the same bytes.
///
/// # Errors
///
/// Returns an error if the key is not a P-256 `SubjectPublicKeyInfo` PEM, the
/// signature is not base64 DER, or the signature does not verify.
pub fn verify_p256_sha256(
    public_key_pem: &str,
    message: &[u8],
    signature_base64: &[u8],
) -> Result<(), String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use p256::{
        ecdsa::{Signature, VerifyingKey, signature::Verifier as _},
        pkcs8::DecodePublicKey,
    };

    let key = VerifyingKey::from_public_key_pem(public_key_pem)
        .map_err(|_| "invalid P-256 SubjectPublicKeyInfo")?;
    let text = std::str::from_utf8(signature_base64)
        .map_err(|_| "P-256 signature is not base64 text")?
        .trim();
    let der = STANDARD
        .decode(text)
        .map_err(|_| "P-256 signature is not base64 text")?;
    let signature = Signature::from_der(&der).map_err(|_| "invalid P-256 DER signature")?;
    let signature = signature.normalize_s();
    key.verify(message, &signature)
        .map_err(|_| "P-256 signature verification failed".into())
}

/// Append a Linux kernel PKCS#7 module signature made with RSA/SHA-512.
///
/// The envelope intentionally omits certificates, content, and signed
/// attributes, matching `scripts/sign-file` with `CMS_NOCERTS`,
/// `CMS_DETACHED`, and `CMS_NOATTR`.  The kernel resolves the signer from its
/// built-in trusted keyring using the certificate issuer and serial number.
///
/// # Errors
///
/// Returns an error for malformed key/certificate material or DER encoding
/// failure, and refuses input that is already module-signed.
pub fn sign_kernel_module(
    private_key_pem: &str,
    certificate_bytes: &[u8],
    module: &[u8],
) -> Result<Vec<u8>, String> {
    use cms::{
        cert::IssuerAndSerialNumber,
        content_info::{CmsVersion, ContentInfo},
        signed_data::{
            EncapsulatedContentInfo, SignatureValue, SignedData, SignerIdentifier, SignerInfo,
            SignerInfos,
        },
    };
    use der::{Any, AnyRef, Decode, Encode, asn1::SetOfVec};
    use spki::AlgorithmIdentifierOwned;

    const MODULE_SIGNATURE_MAGIC: &[u8] = b"~Module signature appended~\n";
    if module.ends_with(MODULE_SIGNATURE_MAGIC) {
        return Err("kernel module is already signed".into());
    }
    let certificate_der = certificate_der(certificate_bytes)?;
    let certificate = x509_cert::Certificate::from_der(&certificate_der)
        .map_err(|error| format!("invalid X.509 certificate: {error}"))?;
    let digest_algorithm = AlgorithmIdentifierOwned {
        oid: const_oid::db::rfc5912::ID_SHA_512,
        parameters: None,
    };
    let signature_algorithm = AlgorithmIdentifierOwned {
        oid: const_oid::db::rfc5912::RSA_ENCRYPTION,
        parameters: Some(Any::null()),
    };
    let signature =
        sign_rsa_pkcs1_ring(private_key_pem, module, &ring::signature::RSA_PKCS1_SHA512)?;
    let issuer = certificate.tbs_certificate.issuer;
    let serial_number = certificate.tbs_certificate.serial_number;
    let signer_info = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
            issuer,
            serial_number,
        }),
        digest_alg: digest_algorithm.clone(),
        signed_attrs: None,
        signature_algorithm,
        signature: SignatureValue::new(signature)
            .map_err(|error| format!("cannot encode module signature: {error}"))?,
        unsigned_attrs: None,
    };
    let signed_data = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: SetOfVec::try_from(vec![digest_algorithm])
            .map_err(|error| format!("cannot encode digest set: {error}"))?,
        encap_content_info: EncapsulatedContentInfo {
            econtent_type: const_oid::db::rfc5911::ID_DATA,
            econtent: None,
        },
        certificates: None,
        crls: None,
        signer_infos: SignerInfos(
            SetOfVec::try_from(vec![signer_info])
                .map_err(|error| format!("cannot encode signer set: {error}"))?,
        ),
    };
    let signed_data_der = signed_data
        .to_der()
        .map_err(|error| format!("cannot encode signed data: {error}"))?;
    let content = AnyRef::try_from(signed_data_der.as_slice())
        .map_err(|error| format!("cannot wrap signed data: {error}"))?;
    let envelope = ContentInfo {
        content_type: const_oid::db::rfc5911::ID_SIGNED_DATA,
        content: Any::from(content),
    }
    .to_der()
    .map_err(|error| format!("cannot encode PKCS#7 envelope: {error}"))?;
    let signature_len =
        u32::try_from(envelope.len()).map_err(|_| "module signature exceeds kernel format")?;

    let capacity = module
        .len()
        .checked_add(envelope.len())
        .and_then(|size| size.checked_add(40))
        .ok_or("signed module size overflow")?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(module);
    output.extend_from_slice(&envelope);
    output.extend_from_slice(&[0, 0, 2, 0, 0, 0, 0, 0]);
    output.extend_from_slice(&signature_len.to_be_bytes());
    output.extend_from_slice(MODULE_SIGNATURE_MAGIC);
    Ok(output)
}

/// Create the detached PKCS#7 `SignedData` used by authenticated UEFI variable updates.
///
/// # Errors
///
/// Returns an error if the key or certificate is malformed, if they do not
/// match, or if the signature or CMS structure cannot be encoded.
pub fn sign_efi_variable_payload(
    private_key_pem: &str,
    certificate_bytes: &[u8],
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    use cms::{
        cert::{CertificateChoices, IssuerAndSerialNumber},
        content_info::CmsVersion,
        signed_data::{
            CertificateSet, EncapsulatedContentInfo, SignatureValue, SignedData, SignerIdentifier,
            SignerInfo, SignerInfos,
        },
    };
    use der::{Any, Decode, Encode, asn1::SetOfVec};
    use spki::AlgorithmIdentifierOwned;

    rsa_private_key_matches_certificate(private_key_pem, certificate_bytes)?;
    let certificate_der = certificate_der(certificate_bytes)?;
    let certificate = x509_cert::Certificate::from_der(&certificate_der)
        .map_err(|error| format!("invalid X.509 certificate: {error}"))?;
    let digest_algorithm = AlgorithmIdentifierOwned {
        oid: const_oid::db::rfc5912::ID_SHA_256,
        parameters: None,
    };
    let signature_algorithm = AlgorithmIdentifierOwned {
        oid: const_oid::db::rfc5912::RSA_ENCRYPTION,
        parameters: Some(Any::null()),
    };
    let signature =
        sign_rsa_pkcs1_ring(private_key_pem, payload, &ring::signature::RSA_PKCS1_SHA256)?;
    let signer_info = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
            issuer: certificate.tbs_certificate.issuer.clone(),
            serial_number: certificate.tbs_certificate.serial_number.clone(),
        }),
        digest_alg: digest_algorithm.clone(),
        signed_attrs: None,
        signature_algorithm,
        signature: SignatureValue::new(signature)
            .map_err(|error| format!("cannot encode EFI variable signature: {error}"))?,
        unsigned_attrs: None,
    };
    let signed_data = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: SetOfVec::try_from(vec![digest_algorithm])
            .map_err(|error| format!("cannot encode digest set: {error}"))?,
        encap_content_info: EncapsulatedContentInfo {
            econtent_type: const_oid::db::rfc5911::ID_DATA,
            econtent: None,
        },
        certificates: Some(CertificateSet(
            SetOfVec::try_from(vec![CertificateChoices::Certificate(certificate)])
                .map_err(|error| format!("cannot encode certificate set: {error}"))?,
        )),
        crls: None,
        signer_infos: SignerInfos(
            SetOfVec::try_from(vec![signer_info])
                .map_err(|error| format!("cannot encode signer set: {error}"))?,
        ),
    };
    signed_data
        .to_der()
        .map_err(|error| format!("cannot encode EFI variable SignedData: {error}"))
}

/// Verify the exact Linux module-signature envelope emitted by
/// [`sign_kernel_module`] against the supplied X.509 certificate.
///
/// # Errors
///
/// Returns an error for malformed trailers/CMS, unexpected algorithms or
/// attributes, a mismatched signer identity, or an invalid signature.
pub fn verify_kernel_module(certificate_bytes: &[u8], signed_module: &[u8]) -> Result<(), String> {
    use cms::{
        content_info::ContentInfo,
        signed_data::{SignedData, SignerIdentifier},
    };
    use der::{Decode, Encode};
    use rsa::{RsaPublicKey, pkcs1v15, pkcs8::DecodePublicKey, sha2::Sha512, signature::Verifier};

    const MAGIC: &[u8] = b"~Module signature appended~\n";
    const HEADER_LEN: usize = 12;
    if !signed_module.ends_with(MAGIC) {
        return Err("kernel module signature trailer is missing".into());
    }
    let header_start = signed_module
        .len()
        .checked_sub(MAGIC.len())
        .and_then(|offset| offset.checked_sub(HEADER_LEN))
        .ok_or("kernel module signature trailer is truncated")?;
    let header_end = header_start
        .checked_add(HEADER_LEN)
        .ok_or("module signature header overflow")?;
    let header = signed_module
        .get(header_start..header_end)
        .ok_or("kernel module signature header is truncated")?;
    if header.get(..8) != Some(&[0, 0, 2, 0, 0, 0, 0, 0]) {
        return Err("kernel module signature header is unsupported".into());
    }
    let envelope_len = u32::from_be_bytes(
        header
            .get(8..12)
            .ok_or("invalid module signature length")?
            .try_into()
            .map_err(|_| "invalid module signature length")?,
    ) as usize;
    let envelope_start = header_start
        .checked_sub(envelope_len)
        .ok_or("module signature length exceeds input")?;
    let module = signed_module
        .get(..envelope_start)
        .ok_or("module signature offset exceeds input")?;
    let envelope = signed_module
        .get(envelope_start..header_start)
        .ok_or("module signature envelope exceeds input")?;
    let content_info = ContentInfo::from_der(envelope)
        .map_err(|error| format!("invalid PKCS#7 envelope: {error}"))?;
    if content_info.content_type != const_oid::db::rfc5911::ID_SIGNED_DATA {
        return Err("module signature is not CMS SignedData".into());
    }
    let signed_data: SignedData = content_info
        .content
        .decode_as()
        .map_err(|error| format!("invalid CMS SignedData: {error}"))?;
    if signed_data.encap_content_info.econtent_type != const_oid::db::rfc5911::ID_DATA
        || signed_data.encap_content_info.econtent.is_some()
        || signed_data.certificates.is_some()
        || signed_data.crls.is_some()
        || signed_data.signer_infos.0.len() != 1
    {
        return Err("module signature has an unexpected CMS shape".into());
    }
    let signer = signed_data
        .signer_infos
        .0
        .get(0)
        .ok_or("module signature has no signer")?;
    if signer.digest_alg.oid != const_oid::db::rfc5912::ID_SHA_512
        || signer.signature_algorithm.oid != const_oid::db::rfc5912::RSA_ENCRYPTION
        || signer.signed_attrs.is_some()
        || signer.unsigned_attrs.is_some()
    {
        return Err("module signature algorithms or attributes are unsupported".into());
    }
    let certificate = x509_cert::Certificate::from_der(&certificate_der(certificate_bytes)?)
        .map_err(|error| format!("invalid X.509 certificate: {error}"))?;
    let SignerIdentifier::IssuerAndSerialNumber(identity) = &signer.sid else {
        return Err("module signature does not use issuer/serial identity".into());
    };
    if identity.issuer != certificate.tbs_certificate.issuer
        || identity.serial_number != certificate.tbs_certificate.serial_number
    {
        return Err("module signature names a different certificate".into());
    }
    let public_der = certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|error| format!("cannot encode certificate public key: {error}"))?;
    let public = RsaPublicKey::from_public_key_der(&public_der)
        .map_err(|_| "certificate does not contain a supported RSA key")?;
    let signature = pkcs1v15::Signature::try_from(signer.signature.as_bytes())
        .map_err(|_| "invalid RSA module signature length")?;
    pkcs1v15::VerifyingKey::<Sha512>::new(public)
        .verify(module, &signature)
        .map_err(|_| "kernel module signature verification failed".into())
}

fn with_pe<T>(
    image: &[u8],
    mut operation: impl FnMut(&dyn authenticode::PeTrait) -> Result<T, String>,
) -> Result<T, String> {
    use object::read::pe::{PeFile32, PeFile64};

    if let Ok(pe) = PeFile64::parse(image) {
        return operation(&pe);
    }
    if let Ok(pe) = PeFile32::parse(image) {
        return operation(&pe);
    }
    Err("input is not a valid PE32/PE32+ image".into())
}

fn pe_checksum(image: &[u8], checksum_offset: usize) -> Result<u32, String> {
    let checksum_end = checksum_offset
        .checked_add(4)
        .ok_or("PE checksum offset overflow")?;
    if checksum_end > image.len() {
        return Err("PE checksum field is outside the image".into());
    }
    let mut checksum = 0u64;
    for (index, chunk) in image.chunks(2).enumerate() {
        let offset = index.checked_mul(2).ok_or("PE checksum offset overflow")?;
        if (checksum_offset..checksum_end).contains(&offset) {
            continue;
        }
        let first = chunk.first().copied().ok_or("empty PE checksum chunk")?;
        let word = u16::from_le_bytes([first, chunk.get(1).copied().unwrap_or(0)]);
        checksum = (checksum & 0xffff)
            .wrapping_add(checksum >> 16)
            .wrapping_add(u64::from(word));
    }
    checksum = (checksum & 0xffff).wrapping_add(checksum >> 16);
    checksum = (checksum & 0xffff).wrapping_add(checksum >> 16);
    let folded = u32::try_from(checksum & 0xffff).map_err(|_| "PE checksum overflow")?;
    let image_len = u32::try_from(image.len()).map_err(|_| "PE image exceeds 4 GiB")?;
    Ok(folded.wrapping_add(image_len))
}

/// `"<<<Obsolete>>>"` as a `BMPString`, the conventional placeholder Authenticode
/// signers put wherever the format still demands an `SpcLink` file name.
const SPC_OBSOLETE_FILE_LINK: [u8; 32] = [
    0xa2, 0x1e, // [2] EXPLICIT SpcString (SpcLink.file)
    0x80, 0x1c, // [0] IMPLICIT BMPString (SpcString.unicode)
    0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x4f, 0x00, 0x62, 0x00, 0x73, 0x00, 0x6f, 0x00, 0x6c,
    0x00, 0x65, 0x00, 0x74, 0x00, 0x65, 0x00, 0x3e, 0x00, 0x3e, 0x00, 0x3e,
];

/// DER `SpcPeImageData { flags includeResources, file <<<Obsolete>>> }`.
fn spc_pe_image_data() -> Vec<u8> {
    let mut encoded = vec![
        0x30, 0x26, // SEQUENCE
        0x03, 0x02, 0x05, 0x80, // BIT STRING, 3 bits: includeResources only
        0xa0, 0x20, // [0] EXPLICIT SpcLink
    ];
    encoded.extend_from_slice(&SPC_OBSOLETE_FILE_LINK);
    encoded
}

/// DER `SpcSpOpusInfo { moreInfo <<<Obsolete>>> }` without a program name.
fn spc_sp_opus_info() -> Vec<u8> {
    let mut encoded = vec![
        0x30, 0x22, // SEQUENCE
        0xa1, 0x20, // [1] EXPLICIT SpcLink (moreInfo)
    ];
    encoded.extend_from_slice(&SPC_OBSOLETE_FILE_LINK);
    encoded
}

const SPC_PE_IMAGE_DATA_OBJID: const_oid::ObjectIdentifier =
    const_oid::ObjectIdentifier::new_unwrap("1.3.6.1.4.1.311.2.1.15");
const SPC_SP_OPUS_INFO_OBJID: const_oid::ObjectIdentifier =
    const_oid::ObjectIdentifier::new_unwrap("1.3.6.1.4.1.311.2.1.12");
const ID_KP_CODE_SIGNING: const_oid::ObjectIdentifier =
    const_oid::ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.3");

fn sha256_algorithm() -> spki::AlgorithmIdentifierOwned {
    spki::AlgorithmIdentifierOwned {
        oid: const_oid::db::rfc5912::ID_SHA_256,
        parameters: Some(der::Any::null()),
    }
}

/// Build one PKCS#7 `WIN_CERTIFICATE` table entry carrying an RSA/SHA-256
/// Authenticode signature over `digest`.
///
/// The envelope is assembled here so that ring, not `RustCrypto`'s RSA
/// private operation (RUSTSEC-2023-0071), produces the signature.
#[allow(
    clippy::too_many_lines,
    reason = "Keep Authenticode DER field construction beside the exact attributes signed by ring"
)]
fn efi_authenticode_entry(
    private_key_pem: &str,
    certificate_pem: &str,
    digest: Vec<u8>,
) -> Result<Vec<u8>, String> {
    use authenticode::{
        DigestInfo, SPC_INDIRECT_DATA_OBJID, SpcAttributeTypeAndOptionalValue,
        SpcIndirectDataContent, WIN_CERT_REVISION_2_0, WIN_CERT_TYPE_PKCS_SIGNED_DATA,
    };
    use cms::{
        cert::{CertificateChoices, IssuerAndSerialNumber},
        content_info::{CmsVersion, ContentInfo},
        revocation::RevocationInfoChoices,
        signed_data::{
            CertificateSet, EncapsulatedContentInfo, SignatureValue, SignedData, SignerIdentifier,
            SignerInfo, SignerInfos,
        },
    };
    use der::{
        Any, Decode, Encode,
        asn1::{OctetString, SetOfVec},
    };
    use x509_cert::attr::Attribute;

    fn encode_error(what: &'static str) -> impl Fn(der::Error) -> String {
        move |error| format!("cannot encode {what}: {error}")
    }
    let single_value = |value: Any| {
        SetOfVec::try_from(vec![value]).map_err(encode_error("Authenticode attribute"))
    };

    let certificate_der = certificate_der(certificate_pem.as_bytes())?;
    let certificate = x509_cert::Certificate::from_der(&certificate_der)
        .map_err(|error| format!("invalid EFI certificate: {error}"))?;
    let indirect_data = SpcIndirectDataContent {
        data: SpcAttributeTypeAndOptionalValue {
            value_type: SPC_PE_IMAGE_DATA_OBJID,
            value: Any::from_der(&spc_pe_image_data())
                .map_err(encode_error("Authenticode image data"))?,
        },
        message_digest: DigestInfo {
            digest_algorithm: sha256_algorithm(),
            digest: OctetString::new(digest).map_err(encode_error("Authenticode digest"))?,
        },
    };
    // The messageDigest attribute covers the content octets of the
    // SpcIndirectDataContent, not its outer SEQUENCE header.
    let mut signed_content = indirect_data
        .data
        .to_der()
        .map_err(encode_error("Authenticode content"))?;
    signed_content.extend(
        indirect_data
            .message_digest
            .to_der()
            .map_err(encode_error("Authenticode digest"))?,
    );
    let signed_attributes = SetOfVec::try_from(vec![
        Attribute {
            oid: const_oid::db::rfc5911::ID_CONTENT_TYPE,
            values: single_value(
                Any::encode_from(&SPC_INDIRECT_DATA_OBJID)
                    .map_err(encode_error("Authenticode content type"))?,
            )?,
        },
        Attribute {
            oid: SPC_SP_OPUS_INFO_OBJID,
            values: single_value(
                Any::from_der(&spc_sp_opus_info())
                    .map_err(encode_error("Authenticode opus info"))?,
            )?,
        },
        Attribute {
            oid: const_oid::db::rfc5911::ID_MESSAGE_DIGEST,
            values: single_value(
                Any::encode_from(
                    &OctetString::new(Sha256::digest(&signed_content).to_vec())
                        .map_err(encode_error("Authenticode message digest"))?,
                )
                .map_err(encode_error("Authenticode message digest"))?,
            )?,
        },
    ])
    .map_err(encode_error("Authenticode attributes"))?;
    // RFC 5652 §5.4: the signature covers the attributes under a SET OF tag.
    let attributes_der = signed_attributes
        .to_der()
        .map_err(encode_error("Authenticode attributes"))?;
    let rsa_signature = sign_rsa_pkcs1_ring(
        private_key_pem,
        &attributes_der,
        &ring::signature::RSA_PKCS1_SHA256,
    )?;

    let signer_info = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
            issuer: certificate.tbs_certificate.issuer.clone(),
            serial_number: certificate.tbs_certificate.serial_number.clone(),
        }),
        digest_alg: sha256_algorithm(),
        signed_attrs: Some(signed_attributes),
        signature_algorithm: spki::AlgorithmIdentifierOwned {
            oid: const_oid::db::rfc5912::RSA_ENCRYPTION,
            parameters: Some(Any::null()),
        },
        signature: SignatureValue::new(rsa_signature)
            .map_err(encode_error("Authenticode signature"))?,
        unsigned_attrs: None,
    };
    let signed_data = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: SetOfVec::try_from(vec![sha256_algorithm()])
            .map_err(encode_error("digest set"))?,
        encap_content_info: EncapsulatedContentInfo {
            econtent_type: SPC_INDIRECT_DATA_OBJID,
            econtent: Some(
                Any::encode_from(&indirect_data).map_err(encode_error("Authenticode content"))?,
            ),
        },
        certificates: Some(CertificateSet(
            SetOfVec::try_from(vec![CertificateChoices::Certificate(certificate)])
                .map_err(encode_error("certificate set"))?,
        )),
        crls: Some(RevocationInfoChoices(SetOfVec::new())),
        signer_infos: SignerInfos(
            SetOfVec::try_from(vec![signer_info]).map_err(encode_error("signer set"))?,
        ),
    };
    let signature_der = ContentInfo {
        content_type: const_oid::db::rfc5911::ID_SIGNED_DATA,
        content: Any::encode_from(&signed_data).map_err(encode_error("signed data"))?,
    }
    .to_der()
    .map_err(encode_error("EFI Authenticode signature"))?;

    // WIN_CERTIFICATE: dwLength covers the 8-byte header, the signature and
    // the zero padding that keeps the next entry 8-byte aligned.
    let padded_len = signature_der
        .len()
        .checked_add(8)
        .map(|length| length.next_multiple_of(8))
        .ok_or("EFI certificate table entry size overflow")?;
    let length =
        u32::try_from(padded_len).map_err(|_| "EFI certificate table entry exceeds PE format")?;
    let mut entry = Vec::with_capacity(padded_len);
    entry.extend_from_slice(&length.to_le_bytes());
    entry.extend_from_slice(&WIN_CERT_REVISION_2_0.to_le_bytes());
    entry.extend_from_slice(&WIN_CERT_TYPE_PKCS_SIGNED_DATA.to_le_bytes());
    entry.extend_from_slice(&signature_der);
    entry.resize(padded_len, 0);
    Ok(entry)
}

/// Return the exact signed-attribute bytes of the only `SignerInfo` in a
/// PKCS#7 envelope, retagged as the SET OF the signature covers.
///
/// Decoding into typed structures would re-sort the attributes into DER
/// order, which breaks signatures from signers that emitted them unsorted.
fn authenticode_signed_attributes_der(envelope: &[u8]) -> Result<Vec<u8>, der::Error> {
    use der::{AnyRef, Decode, Reader, SliceReader, Tag, TagNumber};

    // ContentInfo; trailing WIN_CERTIFICATE padding is not part of it.
    let content_info = AnyRef::decode(&mut SliceReader::new(envelope)?)?;
    let mut reader = SliceReader::new(content_info.value())?;
    const_oid::ObjectIdentifier::decode(&mut reader)?;
    let explicit = AnyRef::decode(&mut reader)?;
    let signed_data = AnyRef::from_der(explicit.value())?;
    let mut reader = SliceReader::new(signed_data.value())?;
    // version, digestAlgorithms, encapContentInfo
    for _ in 0..3 {
        AnyRef::decode(&mut reader)?;
    }
    // certificates [0] and crls [1] are optional.
    while matches!(reader.peek_tag()?, Tag::ContextSpecific { .. }) {
        AnyRef::decode(&mut reader)?;
    }
    let signer_infos = AnyRef::decode(&mut reader)?;
    let signer_info = AnyRef::decode(&mut SliceReader::new(signer_infos.value())?)?;
    let mut reader = SliceReader::new(signer_info.value())?;
    // version, sid, digestAlgorithm
    for _ in 0..3 {
        AnyRef::decode(&mut reader)?;
    }
    let signed_attributes_tag = Tag::ContextSpecific {
        constructed: true,
        number: TagNumber::N0,
    };
    let actual = reader.peek_tag()?;
    if actual != signed_attributes_tag {
        return Err(der::ErrorKind::TagUnexpected {
            expected: Some(signed_attributes_tag),
            actual,
        }
        .into());
    }
    let mut attributes = reader.tlv_bytes()?.to_vec();
    if let Some(tag) = attributes.first_mut() {
        *tag = 0x31;
    }
    Ok(attributes)
}

/// Verify one Authenticode `WIN_CERTIFICATE` payload against the image digest,
/// provided its signer is exactly `expected_der`.
///
/// Returns `Ok(false)` when the entry is not a parseable Authenticode
/// signature or is signed by another certificate, so the caller can move on
/// to the next entry; returns an error when the named signer's signature
/// does not hold.
#[allow(
    clippy::too_many_lines,
    reason = "Keep every Authenticode acceptance check in one auditable sequence"
)]
fn verify_authenticode_entry(
    entry: &[u8],
    expected_der: &[u8],
    image_digest: &[u8],
) -> Result<bool, String> {
    use authenticode::AuthenticodeSignature;
    use cms::{
        cert::CertificateChoices,
        content_info::ContentInfo,
        signed_data::{SignedData, SignerIdentifier},
    };
    use der::{Decode, Encode, SliceReader, asn1::OctetStringRef};
    use rsa::{RsaPublicKey, pkcs1v15, pkcs8::DecodePublicKey, signature::Verifier};
    use x509_cert::{Certificate, ext::pkix::ExtendedKeyUsage};

    fn fail(reason: &str) -> Result<bool, String> {
        Err(format!("EFI Authenticode verification failed: {reason}"))
    }

    // Structural rules: SignedData v1 with one digest algorithm that the
    // signer repeats, SpcIndirectDataContent, one v1 SignerInfo and a
    // messageDigest attribute whenever attributes are present.
    let Ok(signature) = AuthenticodeSignature::from_bytes(entry) else {
        return Ok(false);
    };
    let Ok(signed_data) = SliceReader::new(entry)
        .and_then(|mut reader| ContentInfo::decode(&mut reader))
        .and_then(|content_info| content_info.content.decode_as::<SignedData>())
    else {
        return Ok(false);
    };
    let signer = signature.signer_info();
    let SignerIdentifier::IssuerAndSerialNumber(identity) = &signer.sid else {
        return Ok(false);
    };
    let certificates: Vec<&Certificate> = signed_data
        .certificates
        .as_ref()
        .map(|set| {
            set.0
                .iter()
                .filter_map(|choice| match choice {
                    CertificateChoices::Certificate(certificate) => Some(certificate),
                    CertificateChoices::Other(_) => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let Some(signing_certificate) = certificates.iter().copied().find(|certificate| {
        certificate.tbs_certificate.issuer == identity.issuer
            && certificate.tbs_certificate.serial_number == identity.serial_number
    }) else {
        return Ok(false);
    };
    let signer_der = signing_certificate
        .to_der()
        .map_err(|error| format!("cannot encode embedded EFI certificate: {error}"))?;
    if signer_der != expected_der {
        return Ok(false);
    }

    if signature.digest_algorithm() != &signer.digest_alg {
        return fail("image digest algorithm does not match the signer");
    }
    if signer.digest_alg.oid != const_oid::db::rfc5912::ID_SHA_256 {
        return fail("only SHA-256 Authenticode digests are supported");
    }
    if signature.digest() != image_digest {
        return fail("image digest does not match the signed digest");
    }

    let Some(attributes) = signer.signed_attrs.as_ref() else {
        return fail("signature has no signed attributes");
    };
    let attributes_der = authenticode_signed_attributes_der(entry)
        .map_err(|error| format!("EFI Authenticode verification failed: {error}"))?;
    let public_der = signing_certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|error| format!("cannot encode certificate public key: {error}"))?;
    let Ok(public) = RsaPublicKey::from_public_key_der(&public_der) else {
        return fail("signer key is not RSA");
    };
    let Ok(rsa_signature) = pkcs1v15::Signature::try_from(signature.signature()) else {
        return fail("invalid RSA signature");
    };
    if pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(public)
        .verify(&attributes_der, &rsa_signature)
        .is_err()
    {
        return fail("signature does not match");
    }

    let Some(content) = signature.encapsulated_content() else {
        return fail("signature has no content");
    };
    let content_digest = Sha256::digest(content);
    let signed_digest = attributes
        .iter()
        .find(|attribute| attribute.oid == const_oid::db::rfc5911::ID_MESSAGE_DIGEST)
        .and_then(|attribute| attribute.values.iter().next())
        .and_then(|value| value.decode_as::<OctetStringRef<'_>>().ok());
    if signed_digest.as_ref().map(OctetStringRef::as_bytes) != Some(content_digest.as_slice()) {
        return fail("messageDigest does not cover the signed content");
    }

    // Code-signing purpose: if the signer or any issuer in its embedded chain
    // restricts key usage, the signer must allow code signing.
    let extended_key_usage = |certificate: &Certificate| {
        certificate
            .tbs_certificate
            .extensions
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|extension| extension.extn_id == const_oid::db::rfc5280::ID_CE_EXT_KEY_USAGE)
            .map(|extension| ExtendedKeyUsage::from_der(extension.extn_value.as_bytes()))
    };
    let mut next_subject = &signing_certificate.tbs_certificate.subject;
    let mut chain_restricts_usage = false;
    for certificate in &certificates {
        if &certificate.tbs_certificate.subject == next_subject {
            chain_restricts_usage |= extended_key_usage(certificate).is_some();
            next_subject = &certificate.tbs_certificate.issuer;
        }
    }
    if chain_restricts_usage {
        let allows_code_signing = extended_key_usage(signing_certificate)
            .and_then(Result::ok)
            .is_some_and(|usage| usage.0.contains(&ID_KP_CODE_SIGNING));
        if !allows_code_signing {
            return fail("signer certificate is not valid for code signing");
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch")?;
    let validity = &signing_certificate.tbs_certificate.validity;
    if validity.not_before.to_unix_duration() > now {
        return fail("signer certificate is not yet valid");
    }
    if validity.not_after.to_unix_duration() < now {
        return fail("signer certificate has expired");
    }
    Ok(true)
}

/// Add an RSA/SHA-256 Authenticode signature to a PE/COFF EFI image.
///
/// Existing signatures are retained. The certificate table must be at the end
/// of the image, as required for an unambiguous Authenticode digest.
///
/// # Errors
///
/// Returns an error for malformed keys, certificates, PE images, certificate
/// tables, or signing/encoding failures.
pub fn sign_efi_image(
    private_key_pem: &str,
    certificate_pem: &str,
    image: &[u8],
) -> Result<Vec<u8>, String> {
    let signed = embed_efi_signature(private_key_pem, certificate_pem, image)?;
    verify_efi_image(certificate_pem, &signed)
        .map_err(|error| format!("new EFI signature failed self-verification: {error}"))?;
    Ok(signed)
}

/// Append an Authenticode entry to `image` and fix up the security directory
/// and checksum, without verifying the result.
fn embed_efi_signature(
    private_key_pem: &str,
    certificate_pem: &str,
    image: &[u8],
) -> Result<Vec<u8>, String> {
    use authenticode::authenticode_digest;
    use authenticode_sha2::Digest as _;

    let mut prepared = image.to_vec();
    let (security_dir_offset, checksum_offset, certificate_offset, old_size) =
        with_pe(&prepared, |pe| {
            let offsets = pe.offsets().map_err(|error| error.to_string())?;
            let range = pe
                .certificate_table_range()
                .map_err(|error| error.to_string())?;
            let (certificate_offset, old_size) = match range {
                Some(range) if !range.is_empty() => {
                    if range.end != pe.data().len() {
                        return Err("PE certificate table is not at the end of the image".into());
                    }
                    let size = range
                        .end
                        .checked_sub(range.start)
                        .ok_or("invalid PE certificate table range")?;
                    (range.start, size)
                }
                _ => (pe.data().len(), 0),
            };
            Ok((
                offsets.security_data_dir,
                offsets.check_sum,
                certificate_offset,
                old_size,
            ))
        })?;

    if old_size == 0 {
        prepared.resize(prepared.len().next_multiple_of(8), 0);
    }
    let certificate_offset = if old_size == 0 {
        prepared.len()
    } else {
        certificate_offset
    };

    let mut hasher = authenticode_sha2::Sha256::new();
    with_pe(&prepared, |pe| {
        authenticode_digest(pe, &mut hasher).map_err(|error| error.to_string())
    })?;
    let encoded =
        efi_authenticode_entry(private_key_pem, certificate_pem, hasher.finalize().to_vec())?;

    let new_size = old_size
        .checked_add(encoded.len())
        .ok_or("EFI certificate table size overflow")?;
    let certificate_offset_u32 = u32::try_from(certificate_offset)
        .map_err(|_| "EFI certificate table offset exceeds PE format")?;
    let new_size_u32 =
        u32::try_from(new_size).map_err(|_| "EFI certificate table exceeds PE format")?;
    prepared.extend_from_slice(&encoded);
    let security_dir_end = security_dir_offset
        .checked_add(8)
        .ok_or("PE security directory offset overflow")?;
    prepared
        .get_mut(security_dir_offset..security_dir_end)
        .ok_or("PE security directory is outside the image")?
        .copy_from_slice(
            &[
                certificate_offset_u32.to_le_bytes(),
                new_size_u32.to_le_bytes(),
            ]
            .concat(),
        );
    let checksum = pe_checksum(&prepared, checksum_offset)?;
    let checksum_end = checksum_offset
        .checked_add(4)
        .ok_or("PE checksum offset overflow")?;
    prepared
        .get_mut(checksum_offset..checksum_end)
        .ok_or("PE checksum field is outside the image")?
        .copy_from_slice(&checksum.to_le_bytes());
    Ok(prepared)
}

/// Verify that a PE/COFF EFI image has a valid Authenticode signature made by
/// the exact certificate supplied by the caller.
///
/// The supplied certificate is the trust anchor: certificates embedded beside
/// it only matter for the code-signing purpose check, not as a chain.
///
/// # Errors
///
/// Returns an error for malformed input, when the image digest or signature is
/// invalid, or when no signature is bound to the supplied certificate.
pub fn verify_efi_image(certificate_pem: &str, image: &[u8]) -> Result<(), String> {
    use authenticode::{AttributeCertificateIterator, authenticode_digest};
    use authenticode_sha2::Digest as _;

    let expected_der = certificate_der(certificate_pem.as_bytes())
        .map_err(|error| format!("invalid EFI certificate: {error}"))?;

    with_pe(image, |pe| {
        let mut hasher = authenticode_sha2::Sha256::new();
        authenticode_digest(pe, &mut hasher).map_err(|error| error.to_string())?;
        let digest = hasher.finalize();
        let certificates = AttributeCertificateIterator::new(pe)
            .map_err(|error| format!("invalid EFI certificate table: {error}"))?
            .ok_or("EFI image is unsigned")?;
        for entry in certificates {
            let entry = entry.map_err(|error| format!("invalid EFI signature entry: {error}"))?;
            if verify_authenticode_entry(entry.data, &expected_der, &digest)? {
                return Ok(());
            }
        }
        Err("EFI image has no signature from the supplied certificate".into())
    })
}

/// Count structurally valid Authenticode entries in a PE/COFF EFI image.
///
/// This proves only that signature envelopes exist. Call [`verify_efi_image`]
/// when a particular signing identity must be authenticated.
///
/// # Errors
///
/// Returns an error for malformed PE images or certificate tables.
pub fn efi_signature_count(image: &[u8]) -> Result<usize, String> {
    use authenticode::{AttributeCertificateIterator, AuthenticodeSignature};

    with_pe(image, |pe| {
        let Some(certificates) = AttributeCertificateIterator::new(pe)
            .map_err(|error| format!("invalid EFI certificate table: {error}"))?
        else {
            return Ok(0);
        };
        let mut count = 0usize;
        for entry in certificates {
            let entry = entry.map_err(|error| format!("invalid EFI signature entry: {error}"))?;
            if AuthenticodeSignature::from_bytes(entry.data).is_ok() {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha_crypt::{PasswordHashRef, PasswordVerifier};

    #[test]
    fn account_hash_is_compatible_and_scrubs_input() {
        let original = b"correct horse battery staple";
        let mut password = original.to_vec();
        let hash = hash_account_password(&mut password).expect("hash password");

        assert!(hash.starts_with("$6$"));
        assert!(password.iter().all(|byte| *byte == 0));
        let fields: Vec<_> = hash.split('$').collect();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[2], "rounds=5000");
        assert_eq!(fields[3].len(), 16);
        assert_eq!(fields[4].len(), 86);
        let parsed = PasswordHashRef::new(&hash).expect("parse generated hash");
        ShaCrypt::default()
            .verify_password(original, parsed)
            .expect("verify generated hash");
    }

    #[test]
    fn invalid_input_is_also_scrubbed() {
        let mut password = b"short".to_vec();
        assert!(hash_account_password(&mut password).is_err());
        assert!(password.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn rejects_non_certificates() {
        assert!(inspect_certificate(b"not a certificate").is_err());
    }

    #[cfg(feature = "tls-fixture")]
    #[test]
    fn certificate_inspection_reports_subject_expiry_and_bounds_pem_input() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let fixture = generate_tls_fixture("127.0.0.1".parse().unwrap()).expect("TLS fixture");
        let pem = fixture.ca_certificate_pem.as_str();
        let info = inspect_certificate(pem.as_bytes()).expect("inspect CA certificate");
        assert_eq!(info.subject, "CN=Tilo Disposable Registry CA");
        let (month, rest) = info.expires.split_once(' ').expect("month");
        assert_eq!(month.len(), 3, "{}", info.expires);
        assert!(rest.ends_with(" +00:00"), "{}", info.expires);
        assert_eq!(info.sha256_fingerprint.len(), 32 * 3 - 1);

        let der = certificate_der(pem.as_bytes()).expect("certificate DER");
        let digest = certificate_sha256(pem.as_bytes()).expect("PEM digest");
        assert_eq!(certificate_sha256(&der), Ok(digest.clone()));
        assert_eq!(
            info.sha256_fingerprint.replace(':', "").to_lowercase(),
            digest
        );

        let body = STANDARD.encode(&der);
        let rewrapped = format!(
            "-----BEGIN CERTIFICATE-----\r\n{}\r\n-----END CERTIFICATE-----\r\n\r\n  \n",
            body.as_bytes()
                .chunks(76)
                .map(|line| std::str::from_utf8(line).unwrap())
                .collect::<Vec<_>>()
                .join("\r\n")
        );
        assert_eq!(
            certificate_sha256(rewrapped.as_bytes()),
            Ok(digest),
            "line width and trailing whitespace are not part of the certificate"
        );

        let bundle = format!("{pem}{pem}");
        assert!(inspect_certificate(bundle.as_bytes()).is_err());
        assert!(certificate_sha256(format!("{pem}trailing").as_bytes()).is_err());
        let relabeled = pem.replace("CERTIFICATE", "PUBLIC KEY");
        assert!(certificate_sha256(relabeled.as_bytes()).is_err());
        let mut trailing_der = der.clone();
        trailing_der.push(0);
        assert_eq!(
            certificate_sha256(&trailing_der),
            Err("certificate contains trailing DER data".into())
        );
        assert!(certificate_sha256(&der[..der.len() - 1]).is_err());
    }

    #[test]
    fn generated_secure_boot_certificate_round_trips() {
        let generated =
            generate_secure_boot_certificate("Tilo test db").expect("generate RSA cert");
        assert!(generated.private_key_pem.contains("PRIVATE KEY"));
        let info = inspect_certificate(generated.certificate_pem.as_bytes()).expect("inspect cert");
        assert_eq!(info.subject, "CN=Tilo test db");
        assert_eq!(
            certificate_sha256(generated.certificate_pem.as_bytes())
                .unwrap()
                .len(),
            64
        );
        rsa_private_key_matches_certificate(
            &generated.private_key_pem,
            generated.certificate_pem.as_bytes(),
        )
        .expect("generated key matches certificate");
        let other = generate_secure_boot_certificate("other key").expect("second certificate");
        assert!(
            rsa_private_key_matches_certificate(
                &generated.private_key_pem,
                other.certificate_pem.as_bytes()
            )
            .is_err()
        );

        let mut esl = certificate_efi_signature_list(generated.certificate_pem.as_bytes())
            .expect("encode certificate as ESL");
        assert_eq!(
            &esl[..16],
            &[
                0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b,
                0xf0, 0x72,
            ]
        );
        verify_efi_signature_list_contains_certificate(generated.certificate_pem.as_bytes(), &esl)
            .expect("ESL contains generated certificate");
        verify_efi_signature_list_exclusively_contains_certificate(
            generated.certificate_pem.as_bytes(),
            &esl,
        )
        .expect("single-entry ESL is owner-exclusive");
        assert_eq!(validate_efi_signature_list(&esl), Ok(1));
        let mut wrong_signature_type = esl.clone();
        wrong_signature_type[..16].fill(0x5a);
        assert!(
            verify_efi_signature_list_contains_certificate(
                generated.certificate_pem.as_bytes(),
                &wrong_signature_type
            )
            .is_err(),
            "certificate bytes under a non-X.509 signature type must not establish trust"
        );
        assert!(
            verify_efi_signature_list_exclusively_contains_certificate(
                generated.certificate_pem.as_bytes(),
                &wrong_signature_type
            )
            .is_err(),
            "a wrong-type entry must not satisfy owner-exclusive db verification"
        );
        let mut combined = esl.clone();
        combined.extend(
            certificate_efi_signature_list(other.certificate_pem.as_bytes())
                .expect("encode second certificate"),
        );
        verify_efi_signature_list_contains_certificate(
            generated.certificate_pem.as_bytes(),
            &combined,
        )
        .expect("combined ESL still contains owner");
        assert_eq!(validate_efi_signature_list(&combined), Ok(2));
        assert!(
            verify_efi_signature_list_exclusively_contains_certificate(
                generated.certificate_pem.as_bytes(),
                &combined,
            )
            .is_err(),
            "additional boot authority must defeat owner-exclusive verification"
        );
        assert!(
            verify_efi_signature_list_contains_certificate(other.certificate_pem.as_bytes(), &esl)
                .is_err()
        );
        let mut trailing_corruption = esl.clone();
        trailing_corruption.push(0);
        assert!(validate_efi_signature_list(&trailing_corruption).is_err());
        assert!(
            verify_efi_signature_list_contains_certificate(
                generated.certificate_pem.as_bytes(),
                &trailing_corruption
            )
            .is_err()
        );
        esl.pop();
        assert!(
            verify_efi_signature_list_contains_certificate(
                generated.certificate_pem.as_bytes(),
                &esl
            )
            .is_err()
        );
    }

    #[test]
    fn rsa_signature_round_trips_and_rejects_tampering() {
        let generated = generate_secure_boot_certificate("Tilo signer").expect("generate RSA cert");
        let public = rsa_public_key_pem(&generated.private_key_pem).expect("derive public key");
        let (bits, identity) = rsa_public_key_info(&public).expect("inspect public key");
        assert!(bits >= 2048);
        assert_eq!(identity.len(), 64);
        let signature =
            sign_rsa_sha256(&generated.private_key_pem, b"measured image").expect("sign message");
        verify_rsa_sha256(&public, b"measured image", &signature).expect("verify signature");
        assert!(verify_rsa_sha256(&public, b"tampered image", &signature).is_err());
    }

    #[test]
    fn tpm_authority_keys_match_the_portable_production_profile() {
        let generated = generate_tpm_authority_key().expect("generate TPM authority key");
        let (bits, identity) =
            rsa_public_key_info(generated.public_key_pem()).expect("inspect public key");
        assert_eq!(bits, 2048);
        assert_eq!(identity.len(), 64);
        let signature =
            sign_rsa_sha256(generated.private_key_pem(), b"PCR policy").expect("sign policy");
        verify_rsa_sha256(generated.public_key_pem(), b"PCR policy", &signature)
            .expect("verify policy signature");
    }

    #[test]
    fn p256_public_identity_validates_curve_point_and_is_stable() {
        use p256::pkcs8::{EncodePublicKey as _, LineEnding};
        let signing = p256::ecdsa::SigningKey::from_slice(&[7_u8; 32]).expect("P-256 key");
        let public = signing
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .expect("public PEM");
        let identity = p256_public_key_sha256(&public).expect("P-256 identity");
        // SHA-256 over the canonical SubjectPublicKeyInfo DER; the identity
        // is persisted, so its bytes are pinned.
        assert_eq!(
            identity,
            "ce97ec20d43661a19b215cb2e376a698d0b4aa2aa5b4c7854e916bd05082fcee"
        );
        assert!(
            p256_public_key_sha256("-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n")
                .is_err()
        );
    }

    #[test]
    fn p256_detached_signatures_verify_only_the_signed_bytes_and_key() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use p256::ecdsa::{Signature, SigningKey, signature::Signer as _};
        use p256::pkcs8::{EncodePublicKey as _, LineEnding};

        let signing = SigningKey::from_slice(&[7_u8; 32]).expect("P-256 key");
        let public = signing
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .expect("public PEM");
        let message = br#"{"type":"tilo.system-extension.sysext-manifest"}"#;
        let signature: Signature = signing.sign(message);
        let encoded = format!("{}\n", STANDARD.encode(signature.to_der().as_bytes()));

        verify_p256_sha256(&public, message, encoded.as_bytes()).expect("valid signature");

        let mut tampered = message.to_vec();
        tampered[3] ^= 1;
        assert!(verify_p256_sha256(&public, &tampered, encoded.as_bytes()).is_err());

        let other = SigningKey::from_slice(&[9_u8; 32])
            .expect("P-256 key")
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .expect("public PEM");
        assert!(verify_p256_sha256(&other, message, encoded.as_bytes()).is_err());

        // The same signature with s replaced by n - s still proves the signer.
        let (r, s) = (signature.r(), signature.s());
        let high = Signature::from_scalars(r, -*s).expect("high-s signature");
        let high = STANDARD.encode(high.to_der().as_bytes());
        verify_p256_sha256(&public, message, high.as_bytes()).expect("high-s signature");

        assert!(verify_p256_sha256(&public, message, b"not base64!").is_err());
        assert!(verify_p256_sha256(&public, message, STANDARD.encode(b"junk").as_bytes()).is_err());
    }

    #[test]
    fn pure_rust_kernel_module_signature_round_trips() {
        let certificate = generate_secure_boot_certificate("Tilo kernel test").expect("signer");
        let module = b"synthetic kernel module bytes";
        let signed_module = sign_kernel_module(
            &certificate.private_key_pem,
            certificate.certificate_pem.as_bytes(),
            module,
        )
        .expect("sign module");
        verify_kernel_module(certificate.certificate_pem.as_bytes(), &signed_module)
            .expect("verify module");

        let mut tampered = signed_module.clone();
        *tampered.first_mut().expect("nonempty signed module") ^= 1;
        assert!(verify_kernel_module(certificate.certificate_pem.as_bytes(), &tampered).is_err());
        assert!(
            sign_kernel_module(
                &certificate.private_key_pem,
                certificate.certificate_pem.as_bytes(),
                &signed_module,
            )
            .is_err()
        );
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed offsets construct a bounded synthetic PE fixture"
    )]
    fn minimal_efi_image() -> Vec<u8> {
        let mut image = vec![0u8; 1024];
        image[0..2].copy_from_slice(b"MZ");
        image[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        image[0x80..0x84].copy_from_slice(b"PE\0\0");
        let coff = 0x84;
        image[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        image[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
        image[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
        image[coff + 18..coff + 20].copy_from_slice(&0x2022u16.to_le_bytes());
        let optional = coff + 20;
        image[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        image[optional + 16..optional + 20].copy_from_slice(&0x1000u32.to_le_bytes());
        image[optional + 20..optional + 24].copy_from_slice(&0x1000u32.to_le_bytes());
        image[optional + 24..optional + 32].copy_from_slice(&0x0040_0000_u64.to_le_bytes());
        image[optional + 32..optional + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        image[optional + 36..optional + 40].copy_from_slice(&0x200u32.to_le_bytes());
        image[optional + 56..optional + 60].copy_from_slice(&0x2000u32.to_le_bytes());
        image[optional + 60..optional + 64].copy_from_slice(&0x200u32.to_le_bytes());
        image[optional + 68..optional + 70].copy_from_slice(&10u16.to_le_bytes());
        image[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
        let section = optional + 240;
        image[section..section + 8].copy_from_slice(b".text\0\0\0");
        image[section + 8..section + 12].copy_from_slice(&0x200u32.to_le_bytes());
        image[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        image[section + 16..section + 20].copy_from_slice(&0x200u32.to_le_bytes());
        image[section + 20..section + 24].copy_from_slice(&0x200u32.to_le_bytes());
        image[section + 36..section + 40].copy_from_slice(&0x6000_0020u32.to_le_bytes());
        image[0x200..0x209].copy_from_slice(b"Tilo EFI\0");
        image
    }

    #[test]
    fn pure_rust_efi_signing_verifies_and_rejects_tampering() {
        let signer = generate_secure_boot_certificate("Tilo EFI signer").expect("signer");
        let wrong = generate_secure_boot_certificate("wrong EFI signer").expect("wrong signer");
        let image = minimal_efi_image();
        assert_eq!(efi_signature_count(&image).expect("unsigned count"), 0);
        let signed_image = sign_efi_image(&signer.private_key_pem, &signer.certificate_pem, &image)
            .expect("sign EFI image");
        assert_eq!(efi_signature_count(&signed_image).expect("signed count"), 1);
        verify_efi_image(&signer.certificate_pem, &signed_image).expect("verify EFI image");
        assert!(verify_efi_image(&wrong.certificate_pem, &signed_image).is_err());
        assert!(sign_efi_image(&wrong.private_key_pem, &signer.certificate_pem, &image).is_err());
        let variable_signature = sign_efi_variable_payload(
            &signer.private_key_pem,
            signer.certificate_pem.as_bytes(),
            b"authenticated EFI variable payload",
        )
        .expect("sign EFI variable payload");
        assert!(!variable_signature.is_empty());
        assert!(
            sign_efi_variable_payload(
                &wrong.private_key_pem,
                signer.certificate_pem.as_bytes(),
                b"authenticated EFI variable payload",
            )
            .is_err()
        );

        let mut tampered = signed_image.clone();
        tampered[0x200] ^= 1;
        assert!(verify_efi_image(&signer.certificate_pem, &tampered).is_err());

        let dual_signed = sign_efi_image(
            &wrong.private_key_pem,
            &wrong.certificate_pem,
            &signed_image,
        )
        .expect("append second EFI signature");
        assert_eq!(efi_signature_count(&dual_signed).expect("dual count"), 2);
        verify_efi_image(&signer.certificate_pem, &dual_signed).expect("verify first signer");
        verify_efi_image(&wrong.certificate_pem, &dual_signed).expect("verify second signer");
    }

    fn find(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("subsequence present")
    }

    /// Rewrite the signed attributes of the only signature in `signed` and
    /// re-sign them with `private_key_pem`, as a signer with different
    /// encoding habits (or a forger holding the key) would.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "offsets come from locating the envelope's own fields"
    )]
    fn resign_attributes(
        signed: &[u8],
        private_key_pem: &str,
        rewrite: impl FnOnce(&[u8]) -> Vec<u8>,
    ) -> Vec<u8> {
        let range = with_pe(signed, |pe| {
            Ok(pe
                .certificate_table_range()
                .expect("certificate table")
                .expect("signed image"))
        })
        .expect("PE image");
        let envelope = &signed[range.start + 8..range.end];
        let attributes = authenticode_signed_attributes_der(envelope).expect("signed attributes");
        let old_signature = authenticode::AuthenticodeSignature::from_bytes(envelope)
            .expect("Authenticode signature")
            .signature()
            .to_vec();

        let mut tagged = attributes.clone();
        tagged[0] = 0xa0;
        let attributes_at = find(signed, &tagged);
        let body_len = <der::AnyRef<'_> as der::Decode<'_>>::from_der(&attributes)
            .expect("attribute set")
            .value()
            .len();
        let header_len = attributes.len() - body_len;
        let body = rewrite(&attributes[header_len..]);
        assert_eq!(body.len(), attributes.len() - header_len, "same length");
        let mut rewritten = attributes[..header_len].to_vec();
        rewritten.extend_from_slice(&body);
        let signature = sign_rsa_pkcs1_ring(
            private_key_pem,
            &rewritten,
            &ring::signature::RSA_PKCS1_SHA256,
        )
        .expect("re-sign attributes");
        assert_eq!(signature.len(), old_signature.len());

        let mut output = signed.to_vec();
        rewritten[0] = 0xa0;
        output[attributes_at..attributes_at + rewritten.len()].copy_from_slice(&rewritten);
        let signature_at = find(&output, &old_signature);
        output[signature_at..signature_at + signature.len()].copy_from_slice(&signature);
        output
    }

    fn attribute_elements(body: &[u8]) -> Vec<Vec<u8>> {
        use der::{Reader as _, SliceReader};
        let mut reader = SliceReader::new(body).expect("attribute body");
        let mut elements = Vec::new();
        while !reader.is_finished() {
            elements.push(reader.tlv_bytes().expect("attribute").to_vec());
        }
        elements
    }

    #[test]
    fn efi_verification_checks_every_signed_field() {
        let signer = generate_secure_boot_certificate("Tilo EFI signer").expect("signer");
        let image = minimal_efi_image();
        let signed_image = sign_efi_image(&signer.private_key_pem, &signer.certificate_pem, &image)
            .expect("sign EFI image");
        let range = with_pe(&signed_image, |pe| {
            Ok(pe
                .certificate_table_range()
                .expect("certificate table")
                .expect("signed_image image"))
        })
        .expect("PE image");
        let envelope = signed_image[range.start + 8..range.end].to_vec();
        let parsed = authenticode::AuthenticodeSignature::from_bytes(&envelope).expect("parse");

        let mut forged_signature = signed_image.clone();
        let signature_at = find(&forged_signature, parsed.signature());
        forged_signature[signature_at + 10] ^= 1;
        let error = verify_efi_image(&signer.certificate_pem, &forged_signature).unwrap_err();
        assert!(error.contains("signature does not match"), "{error}");

        let mut forged_digest = signed_image.clone();
        let digest_at = find(&forged_digest, parsed.digest());
        forged_digest[digest_at] ^= 1;
        let error = verify_efi_image(&signer.certificate_pem, &forged_digest).unwrap_err();
        assert!(error.contains("image digest does not match"), "{error}");

        // A correctly signed_image attribute set whose messageDigest names other
        // content must not vouch for the embedded SpcIndirectDataContent.
        let content_digest = Sha256::digest(parsed.encapsulated_content().expect("content"));
        let stale_digest = resign_attributes(&signed_image, &signer.private_key_pem, |body| {
            let mut body = body.to_vec();
            let at = find(&body, &content_digest);
            body[at] ^= 1;
            body
        });
        let error = verify_efi_image(&signer.certificate_pem, &stale_digest).unwrap_err();
        assert!(error.contains("messageDigest"), "{error}");

        // The signature covers the attributes as transmitted; signers that
        // do not sort them into DER order must remain verifiable.
        let unsorted = resign_attributes(&signed_image, &signer.private_key_pem, |body| {
            let mut elements = attribute_elements(body);
            elements.reverse();
            elements.concat()
        });
        assert_ne!(unsorted, signed_image);
        verify_efi_image(&signer.certificate_pem, &unsorted).expect("unsorted attributes");
        assert_eq!(efi_signature_count(&unsorted).expect("count"), 1);
    }

    fn rsa_code_signing_certificate(
        private_key_pem: &str,
        usages: Vec<rcgen::ExtendedKeyUsagePurpose>,
        not_before: time::OffsetDateTime,
        not_after: time::OffsetDateTime,
    ) -> String {
        use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_RSA_SHA256};

        let key = KeyPair::from_pkcs8_pem_and_sign_algo(private_key_pem, &PKCS_RSA_SHA256)
            .expect("RSA key pair");
        let mut params = CertificateParams::default();
        params.not_before = not_before;
        params.not_after = not_after;
        params.extended_key_usages = usages;
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "Tilo EFI purpose test");
        params.self_signed(&key).expect("certificate").pem()
    }

    #[test]
    fn efi_verification_enforces_signer_validity_and_code_signing_purpose() {
        use rcgen::ExtendedKeyUsagePurpose::{CodeSigning, ServerAuth};

        let key = generate_tpm_authority_key().expect("RSA key");
        let now = time::OffsetDateTime::now_utc();
        let day = time::Duration::days(1);
        let image = minimal_efi_image();
        let sign = |certificate: &str| {
            embed_efi_signature(key.private_key_pem(), certificate, &image).expect("embed")
        };

        let code_signing = rsa_code_signing_certificate(
            key.private_key_pem(),
            vec![CodeSigning],
            now - day,
            now + day,
        );
        verify_efi_image(&code_signing, &sign(&code_signing)).expect("code-signing EKU");

        let server_only = rsa_code_signing_certificate(
            key.private_key_pem(),
            vec![ServerAuth],
            now - day,
            now + day,
        );
        let error = verify_efi_image(&server_only, &sign(&server_only)).unwrap_err();
        assert!(error.contains("code signing"), "{error}");

        let expired = rsa_code_signing_certificate(
            key.private_key_pem(),
            Vec::new(),
            now - day * 10,
            now - day,
        );
        let error = verify_efi_image(&expired, &sign(&expired)).unwrap_err();
        assert!(error.contains("expired"), "{error}");
        assert!(sign_efi_image(key.private_key_pem(), &expired, &image).is_err());

        let future = rsa_code_signing_certificate(
            key.private_key_pem(),
            Vec::new(),
            now + day,
            now + day * 10,
        );
        let error = verify_efi_image(&future, &sign(&future)).unwrap_err();
        assert!(error.contains("not yet valid"), "{error}");
    }
}
