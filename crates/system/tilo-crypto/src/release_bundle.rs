//! Verify a blob signed under Tilo's release root.
//!
//! An operational key signs the blob; the offline root certified that key for
//! one purpose and one revocation epoch; the public transparency log (Rekor)
//! recorded the signature and the certificate together. The bundle is the form
//! `tools/signing/sign-blob-certified.sh` writes (Cosign's own): the
//! signature, the certificate and the log entry with Rekor's signed entry
//! timestamp. It is accepted only if all of these hold:
//!
//! * Rekor's key signed the entry, and the entry logs exactly this blob's
//!   SHA-256, this signature and this certificate;
//! * the certificate is issued directly by the root, is not a CA, may sign
//!   code, names `<purpose>@<identity domain>` and the current epoch's issuer
//!   (`keys/release/trust.env`), and both it and the root were valid when Rekor
//!   recorded the entry;
//! * the certificate's key made the signature.
//!
//! Validity is checked at Rekor's time, not now, as containers-policy's
//! `fulcio` requirement does for OS images: a signature made while a
//! short-lived certificate was valid stays valid after it expires, and a
//! signature made after that cannot be backdated past the log.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use const_oid::ObjectIdentifier;
use der::{Decode as _, Encode as _};
use sha2::{Digest as _, Sha256, Sha384};

/// OIDC issuer, as raw bytes (Fulcio's original extension).
const OIDC_ISSUER_V1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.1");
/// OIDC issuer, as a DER `UTF8String`.
const OIDC_ISSUER_V2: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.57264.1.8");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const P256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const P384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
const ECDSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const ECDSA_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");

/// What `keys/release/trust.env` says a certificate must name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseTrust {
    issuer: String,
    identity_domain: String,
}

impl ReleaseTrust {
    /// Parse `trust.env`: `KEY=VALUE` lines, blank lines and `#` comments.
    ///
    /// # Errors
    /// Any other line, a repeated key, or a missing or malformed
    /// `TILO_RELEASE_EPOCH`, `TILO_RELEASE_ISSUER_PREFIX` or
    /// `TILO_RELEASE_IDENTITY_DOMAIN`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut values = std::collections::BTreeMap::new();
        for line in text.lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let (name, value) = line
                .split_once('=')
                .ok_or_else(|| format!("release trust: unexpected line {line:?}"))?;
            let well_formed = name.strip_prefix("TILO_RELEASE_").is_some_and(|rest| {
                !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
            });
            if !well_formed || value.is_empty() || value.trim() != value || value.contains('"') {
                return Err(format!("release trust: unexpected line {line:?}"));
            }
            if values.insert(name, value).is_some() {
                return Err(format!("release trust: {name} is set twice"));
            }
        }
        let get = |name: &str| {
            values
                .get(name)
                .copied()
                .ok_or_else(|| format!("release trust sets no {name}"))
        };
        let epoch = get("TILO_RELEASE_EPOCH")?;
        if epoch.starts_with('0') || epoch.parse::<u32>().is_err() {
            return Err("release trust: TILO_RELEASE_EPOCH must be a positive integer".into());
        }
        let domain = get("TILO_RELEASE_IDENTITY_DOMAIN")?;
        if !domain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        {
            return Err("release trust: TILO_RELEASE_IDENTITY_DOMAIN is not a domain".into());
        }
        Ok(Self {
            issuer: format!("{}{epoch}", get("TILO_RELEASE_ISSUER_PREFIX")?),
            identity_domain: domain.to_owned(),
        })
    }

    /// The OIDC issuer a current certificate names.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The e-mail identity a certificate for `purpose` names.
    ///
    /// # Errors
    /// A purpose that is not lowercase words joined by hyphens.
    pub fn identity(&self, purpose: &str) -> Result<String, String> {
        let well_formed = !purpose.is_empty()
            && !purpose.starts_with('-')
            && !purpose.ends_with('-')
            && purpose.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
        if !well_formed {
            return Err(format!("invalid signing purpose {purpose:?}"));
        }
        Ok(format!("{purpose}@{}", self.identity_domain))
    }
}

/// What a verified bundle proves beyond the signature itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedRelease {
    /// When Rekor recorded the signature (Unix seconds).
    pub integrated_time: u64,
    /// The entry's index in the log.
    pub log_index: u64,
}

struct Bundle {
    signature: Vec<u8>,
    certificate: Vec<u8>,
    set: Vec<u8>,
    body: String,
    integrated_time: u64,
    log_index: u64,
    log_id: String,
}

fn object<'a>(
    value: &'a serde_json::Value,
    what: &str,
    keys: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("{what} is not an object"))?;
    if map.len() != keys.len() || !keys.iter().all(|key| map.contains_key(*key)) {
        return Err(format!("{what} must hold exactly {}", keys.join(", ")));
    }
    Ok(map)
}

fn string<'a>(
    map: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str, String> {
    map[key]
        .as_str()
        .ok_or_else(|| format!("{key} is not a string"))
}

fn base64(text: &str, what: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(text.trim())
        .map_err(|_| format!("{what} is not base64"))
}

fn is_base64(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

fn parse_bundle(bytes: &[u8]) -> Result<Bundle, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "the bundle is not JSON")?;
    let top = object(
        &value,
        "the bundle",
        &["base64Signature", "cert", "rekorBundle"],
    )?;
    let rekor = object(
        &top["rekorBundle"],
        "rekorBundle",
        &["SignedEntryTimestamp", "Payload"],
    )?;
    let payload = object(
        &rekor["Payload"],
        "the log entry",
        &["body", "integratedTime", "logIndex", "logID"],
    )?;
    let body = string(payload, "body")?;
    let log_id = string(payload, "logID")?;
    // The signed timestamp covers these values re-serialized, so restrict them
    // to alphabets that need no JSON escaping.
    if !is_base64(body) {
        return Err("the log entry's body is not base64".into());
    }
    if log_id.len() != 64
        || !log_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("the log entry's logID is not a SHA-256".into());
    }
    let integrated_time = payload["integratedTime"]
        .as_u64()
        .ok_or("the log entry's integratedTime is not a time")?;
    let log_index = payload["logIndex"]
        .as_u64()
        .ok_or("the log entry's logIndex is not an index")?;
    let certificate = base64(string(top, "cert")?, "the bundle's certificate")?;
    Ok(Bundle {
        signature: base64(string(top, "base64Signature")?, "the bundle's signature")?,
        certificate: crate::certificate_der(&certificate)?,
        set: base64(
            string(rekor, "SignedEntryTimestamp")?,
            "the signed entry timestamp",
        )?,
        body: body.to_owned(),
        integrated_time,
        log_index,
        log_id: log_id.to_owned(),
    })
}

/// The DER `SubjectPublicKeyInfo` in a `PUBLIC KEY` PEM block.
fn public_key_der(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .trim()
        .strip_prefix("-----BEGIN PUBLIC KEY-----")
        .and_then(|rest| rest.strip_suffix("-----END PUBLIC KEY-----"))
        .ok_or("the Rekor key is not a PEM public key")?
        .split_whitespace()
        .collect();
    base64(&body, "the Rekor key")
}

/// Rekor's signed entry timestamp over the entry, and the log it names.
fn verify_set(rekor_public_key_pem: &str, bundle: &Bundle) -> Result<(), String> {
    use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier as _};
    use p256::pkcs8::DecodePublicKey as _;

    let der = public_key_der(rekor_public_key_pem)?;
    let key = VerifyingKey::from_public_key_der(&der)
        .map_err(|_| "the Rekor key is not a P-256 public key")?;
    if hex::encode(Sha256::digest(&der)) != bundle.log_id {
        return Err("the log entry is from another log than the trusted Rekor".into());
    }
    // Rekor signs the entry as canonical JSON: sorted keys, no whitespace.
    let canonical = format!(
        "{{\"body\":\"{}\",\"integratedTime\":{},\"logID\":\"{}\",\"logIndex\":{}}}",
        bundle.body, bundle.integrated_time, bundle.log_id, bundle.log_index
    );
    let signature =
        Signature::from_der(&bundle.set).map_err(|_| "the signed entry timestamp is malformed")?;
    key.verify(canonical.as_bytes(), &signature.normalize_s())
        .map_err(|_| "Rekor's signed entry timestamp does not verify".into())
}

/// The logged entry is this blob, this signature and this certificate.
fn verify_body(bundle: &Bundle, input: &[u8]) -> Result<(), String> {
    let body = base64(&bundle.body, "the log entry's body")?;
    let entry: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| "the log entry's body is not JSON")?;
    let field = |path: &[&str]| {
        path.iter()
            .try_fold(&entry, |value, key| value.get(key))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("the log entry has no {}", path.join(".")))
    };
    if field(&["kind"])? != "hashedrekord" || field(&["apiVersion"])? != "0.0.1" {
        return Err("the log entry is not a hashedrekord 0.0.1".into());
    }
    if field(&["spec", "data", "hash", "algorithm"])? != "sha256"
        || field(&["spec", "data", "hash", "value"])? != hex::encode(Sha256::digest(input))
    {
        return Err("the log entry is for other content".into());
    }
    if base64(
        field(&["spec", "signature", "content"])?,
        "the logged signature",
    )? != bundle.signature
    {
        return Err("the log entry holds another signature".into());
    }
    let logged = base64(
        field(&["spec", "signature", "publicKey", "content"])?,
        "the logged certificate",
    )?;
    if crate::certificate_der(&logged)? != bundle.certificate {
        return Err("the log entry holds another certificate".into());
    }
    Ok(())
}

/// The curve OID and SEC1 point of an EC `SubjectPublicKeyInfo`.
fn ec_key(spki: &spki::SubjectPublicKeyInfoOwned) -> Result<(ObjectIdentifier, &[u8]), String> {
    if spki.algorithm.oid != EC_PUBLIC_KEY {
        return Err("not an EC public key".into());
    }
    let curve = spki
        .algorithm
        .parameters
        .as_ref()
        .ok_or("EC public key without a curve")?
        .decode_as::<ObjectIdentifier>()
        .map_err(|_| "EC public key with an unnamed curve")?;
    let point = spki
        .subject_public_key
        .as_bytes()
        .ok_or("EC public key is not whole bytes")?;
    Ok((curve, point))
}

/// `signature` (DER) by `issuer` over `message` hashed as `algorithm` says.
fn verify_ecdsa(
    issuer: &spki::SubjectPublicKeyInfoOwned,
    algorithm: ObjectIdentifier,
    message: &[u8],
    signature: &[u8],
) -> Result<(), String> {
    let prehash = match algorithm {
        a if a == ECDSA_SHA256 => Sha256::digest(message).to_vec(),
        a if a == ECDSA_SHA384 => Sha384::digest(message).to_vec(),
        _ => return Err("the certificate is not signed with ECDSA SHA-256 or SHA-384".into()),
    };
    let (curve, point) = ec_key(issuer)?;
    let verified = if curve == P384 {
        use p384::ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier as _};
        let key = VerifyingKey::from_sec1_bytes(point).map_err(|_| "invalid P-384 key")?;
        let signature = Signature::from_der(signature).map_err(|_| "invalid signature")?;
        key.verify_prehash(&prehash, &signature.normalize_s())
            .is_ok()
    } else if curve == P256 {
        use p256::ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier as _};
        let key = VerifyingKey::from_sec1_bytes(point).map_err(|_| "invalid P-256 key")?;
        let signature = Signature::from_der(signature).map_err(|_| "invalid signature")?;
        key.verify_prehash(&prehash, &signature.normalize_s())
            .is_ok()
    } else {
        return Err("the root key is neither P-256 nor P-384".into());
    };
    if verified {
        Ok(())
    } else {
        Err("the root did not sign this certificate".into())
    }
}

fn valid_at(certificate: &x509_cert::Certificate, time: u64, what: &str) -> Result<(), String> {
    let validity = &certificate.tbs_certificate.validity;
    let not_before = validity.not_before.to_unix_duration().as_secs();
    let not_after = validity.not_after.to_unix_duration().as_secs();
    if time < not_before {
        return Err(format!(
            "the {what} was not yet valid when the log recorded the signature"
        ));
    }
    if time > not_after {
        return Err(format!(
            "the {what} had expired when the log recorded the signature"
        ));
    }
    Ok(())
}

/// The operational certificate is the root's, for this purpose and epoch.
fn verify_certificate(
    root: &x509_cert::Certificate,
    leaf: &x509_cert::Certificate,
    trust: &ReleaseTrust,
    purpose: &str,
    time: u64,
) -> Result<(), String> {
    use x509_cert::ext::pkix::BasicConstraints;

    let root_is_ca = root
        .tbs_certificate
        .extensions
        .iter()
        .flatten()
        .find(|extension| extension.extn_id == const_oid::db::rfc5280::ID_CE_BASIC_CONSTRAINTS)
        .and_then(|extension| BasicConstraints::from_der(extension.extn_value.as_bytes()).ok())
        .is_some_and(|constraints| constraints.ca);
    if !root_is_ca {
        return Err("the root certificate is not a CA".into());
    }
    let tbs = &leaf.tbs_certificate;
    let issuer = tbs.issuer.to_der().map_err(|_| "unencodable issuer")?;
    let subject = root
        .tbs_certificate
        .subject
        .to_der()
        .map_err(|_| "unencodable root subject")?;
    if issuer != subject {
        return Err("the certificate was not issued by the root".into());
    }
    // The algorithm inside the signed part must be the one the signature uses.
    if tbs.signature != leaf.signature_algorithm {
        return Err("the certificate's signature algorithms disagree".into());
    }
    let signed = tbs.to_der().map_err(|_| "unencodable certificate")?;
    let signature = leaf
        .signature
        .as_bytes()
        .ok_or("the certificate's signature is not whole bytes")?;
    verify_ecdsa(
        &root.tbs_certificate.subject_public_key_info,
        leaf.signature_algorithm.oid,
        &signed,
        signature,
    )?;
    valid_at(root, time, "root certificate")?;
    valid_at(leaf, time, "operational certificate")?;
    verify_extensions(tbs, trust, purpose)
}

/// The operational certificate's extensions: a code-signing end entity
/// naming this purpose's identity and the current epoch's issuer, with no
/// critical extension this verifier does not understand.
fn verify_extensions(
    tbs: &x509_cert::TbsCertificate,
    trust: &ReleaseTrust,
    purpose: &str,
) -> Result<(), String> {
    use x509_cert::ext::pkix::{
        BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAltName, name::GeneralName,
    };

    let identity = trust.identity(purpose)?;
    let (mut code_signing, mut named, mut issuers) = (false, false, Vec::new());
    let mut seen = Vec::new();
    for extension in tbs.extensions.iter().flatten() {
        let id = extension.extn_id;
        if seen.contains(&id) {
            return Err(format!("the certificate repeats extension {id}"));
        }
        seen.push(id);
        let value = extension.extn_value.as_bytes();
        let malformed = || format!("the certificate's extension {id} is malformed");
        match id {
            const_oid::db::rfc5280::ID_CE_BASIC_CONSTRAINTS => {
                if BasicConstraints::from_der(value)
                    .map_err(|_| malformed())?
                    .ca
                {
                    return Err("a CA certificate cannot sign a release".into());
                }
            }
            const_oid::db::rfc5280::ID_CE_KEY_USAGE => {
                if !KeyUsage::from_der(value)
                    .map_err(|_| malformed())?
                    .digital_signature()
                {
                    return Err("the certificate's key usage excludes signatures".into());
                }
            }
            const_oid::db::rfc5280::ID_CE_EXT_KEY_USAGE => {
                code_signing = ExtendedKeyUsage::from_der(value)
                    .map_err(|_| malformed())?
                    .0
                    .contains(&const_oid::db::rfc5280::ID_KP_CODE_SIGNING);
            }
            const_oid::db::rfc5280::ID_CE_SUBJECT_ALT_NAME => {
                named = SubjectAltName::from_der(value)
                    .map_err(|_| malformed())?
                    .0
                    .iter()
                    .any(|name| {
                        matches!(name, GeneralName::Rfc822Name(email) if email.to_string() == identity)
                    });
            }
            id if id == OIDC_ISSUER_V1 => {
                issuers.push(String::from_utf8(value.to_vec()).map_err(|_| malformed())?);
            }
            id if id == OIDC_ISSUER_V2 => {
                let text = der::asn1::Utf8StringRef::from_der(value).map_err(|_| malformed())?;
                issuers.push(text.as_str().to_owned());
            }
            _ if extension.critical => {
                return Err(format!(
                    "the certificate has an unknown critical extension {id}"
                ));
            }
            _ => {}
        }
    }
    if !code_signing {
        return Err("the certificate may not sign code".into());
    }
    if !named {
        return Err(format!("the certificate does not name {identity}"));
    }
    match issuers.as_slice() {
        [] => Err("the certificate names no issuer".into()),
        [first, rest @ ..] if rest.iter().any(|other| other != first) => {
            Err("the certificate's issuer extensions disagree".into())
        }
        [first, ..] if first != trust.issuer() => Err(format!(
            "the certificate is of {first}, not the current {}; it was revoked or is from another root",
            trust.issuer()
        )),
        _ => Ok(()),
    }
}

/// Verify `bundle` as a release signature over `input` for `purpose`.
///
/// # Errors
/// Why the bundle does not prove that the root's current operational
/// certificate for `purpose` signed `input` and that Rekor logged it.
pub fn verify_release_bundle(
    root_certificate: &[u8],
    rekor_public_key_pem: &str,
    trust: &ReleaseTrust,
    purpose: &str,
    input: &[u8],
    bundle: &[u8],
) -> Result<VerifiedRelease, String> {
    use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier as _};

    let bundle = parse_bundle(bundle)?;
    verify_set(rekor_public_key_pem, &bundle)?;
    verify_body(&bundle, input)?;
    let root = crate::decode_single_certificate(root_certificate)
        .map_err(|error| format!("root: {error}"))?
        .1;
    let leaf = x509_cert::Certificate::from_der(&bundle.certificate)
        .map_err(|_| "the bundle's certificate is not X.509")?;
    verify_certificate(&root, &leaf, trust, purpose, bundle.integrated_time)?;
    let (curve, point) = ec_key(&leaf.tbs_certificate.subject_public_key_info)?;
    if curve != P256 {
        return Err("the operational key is not P-256".into());
    }
    let key = VerifyingKey::from_sec1_bytes(point).map_err(|_| "invalid operational key")?;
    let signature =
        Signature::from_der(&bundle.signature).map_err(|_| "the signature is malformed")?;
    key.verify(input, &signature.normalize_s())
        .map_err(|_| "the operational key did not sign this content")?;
    Ok(VerifiedRelease {
        integrated_time: bundle.integrated_time,
        log_index: bundle.log_index,
    })
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixture validity windows are offsets from the current time"
)]
mod tests {
    use super::*;
    use rcgen::{
        BasicConstraints, CertificateParams, CustomExtension, DistinguishedName, DnType,
        ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
        PKCS_ECDSA_P384_SHA384, PublicKeyData as _, SanType, SigningKey as _,
    };

    const TRUST: &str = "# release trust\nTILO_RELEASE_EPOCH=1\n\
        TILO_RELEASE_ISSUER_PREFIX=https://signing.tilo.so/release/epoch-\n\
        TILO_RELEASE_IDENTITY_DOMAIN=signing.tilo.so\nTILO_RELEASE_OPERATIONAL_MAX_DAYS=90\n";
    const PURPOSE: &str = "extension-manifests";
    const INPUT: &[u8] = b"{\"schema\":1}\n";
    const DAY: i64 = 86_400;

    fn now() -> i64 {
        time::OffsetDateTime::now_utc().unix_timestamp()
    }

    fn at(seconds: i64) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(seconds).unwrap()
    }

    struct Root {
        params: CertificateParams,
        key: KeyPair,
        pem: String,
    }

    /// A root as tools/signing/release-ca.sh makes one: P-384, CA, path length 0.
    fn root(name: &str) -> Root {
        let mut params = CertificateParams::default();
        params.not_before = at(now() - 365 * DAY);
        params.not_after = at(now() + 3650 * DAY);
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::OrganizationName, "Tilo");
        params.distinguished_name.push(DnType::CommonName, name);
        let key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        Root { params, key, pem }
    }

    /// An operational certificate as release-ca.sh issues one.
    struct Leaf {
        purpose: &'static str,
        issuer_v1: Option<&'static str>,
        issuer_v2: Option<&'static str>,
        not_before: i64,
        not_after: i64,
        ca: bool,
        code_signing: bool,
    }

    impl Default for Leaf {
        fn default() -> Self {
            Self {
                purpose: PURPOSE,
                issuer_v1: Some("https://signing.tilo.so/release/epoch-1"),
                issuer_v2: Some("https://signing.tilo.so/release/epoch-1"),
                not_before: now() - DAY,
                not_after: now() + 30 * DAY,
                ca: false,
                code_signing: true,
            }
        }
    }

    impl Leaf {
        fn issue(&self, root: &Root, key: &KeyPair) -> String {
            let mut params = CertificateParams::default();
            params.not_before = at(self.not_before);
            params.not_after = at(self.not_after);
            params.is_ca = if self.ca {
                IsCa::Ca(BasicConstraints::Constrained(0))
            } else {
                IsCa::ExplicitNoCa
            };
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            if self.code_signing {
                params.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
            } else {
                params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            }
            params.subject_alt_names = vec![SanType::Rfc822Name(
                format!("{}@signing.tilo.so", self.purpose)
                    .try_into()
                    .unwrap(),
            )];
            params.use_authority_key_identifier_extension = true;
            params.distinguished_name = DistinguishedName::new();
            params
                .distinguished_name
                .push(DnType::OrganizationName, "Tilo");
            params
                .distinguished_name
                .push(DnType::CommonName, format!("Tilo {} signing", self.purpose));
            if let Some(issuer) = self.issuer_v1 {
                params
                    .custom_extensions
                    .push(CustomExtension::from_oid_content(
                        &[1, 3, 6, 1, 4, 1, 57264, 1, 1],
                        issuer.as_bytes().to_vec(),
                    ));
            }
            if let Some(issuer) = self.issuer_v2 {
                params
                    .custom_extensions
                    .push(CustomExtension::from_oid_content(
                        &[1, 3, 6, 1, 4, 1, 57264, 1, 8],
                        der::asn1::Utf8StringRef::new(issuer)
                            .unwrap()
                            .to_der()
                            .unwrap(),
                    ));
            }
            let issuer = Issuer::from_params(&root.params, &root.key);
            params.signed_by(key, &issuer).unwrap().pem()
        }
    }

    /// Everything one verification takes, built the way production makes it.
    struct Case {
        root: String,
        rekor: String,
        trust: ReleaseTrust,
        input: Vec<u8>,
        bundle: serde_json::Value,
    }

    impl Case {
        fn verify(&self) -> Result<VerifiedRelease, String> {
            verify_release_bundle(
                self.root.as_bytes(),
                &self.rekor,
                &self.trust,
                PURPOSE,
                &self.input,
                &serde_json::to_vec(&self.bundle).unwrap(),
            )
        }

        fn error(&self) -> String {
            self.verify().expect_err("the bundle must be refused")
        }
    }

    struct Log {
        key: KeyPair,
    }

    impl Log {
        fn new() -> Self {
            Self {
                key: KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap(),
            }
        }

        fn pem(&self) -> String {
            self.key.public_key_pem()
        }

        /// Log `signature` over `input` under `certificate`, as Rekor does.
        fn entry(
            &self,
            input: &[u8],
            signature: &str,
            certificate: &str,
            time: i64,
        ) -> serde_json::Value {
            let body = serde_json::json!({
                "apiVersion": "0.0.1",
                "kind": "hashedrekord",
                "spec": {
                    "data": {"hash": {"algorithm": "sha256",
                                      "value": hex::encode(Sha256::digest(input))}},
                    "signature": {"content": signature,
                                  "publicKey": {"content": STANDARD.encode(certificate)}},
                },
            });
            let body = STANDARD.encode(serde_json::to_vec(&body).unwrap());
            let log_id = hex::encode(Sha256::digest(self.key.subject_public_key_info()));
            let canonical = format!(
                "{{\"body\":\"{body}\",\"integratedTime\":{time},\"logID\":\"{log_id}\",\"logIndex\":7}}"
            );
            let set = STANDARD.encode(self.key.sign(canonical.as_bytes()).unwrap());
            serde_json::json!({
                "SignedEntryTimestamp": set,
                "Payload": {"body": body, "integratedTime": time, "logIndex": 7, "logID": log_id},
            })
        }
    }

    fn case_with(leaf: &Leaf, time: i64) -> Case {
        let root = root("Tilo Release Root");
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = leaf.issue(&root, &key);
        let log = Log::new();
        let signature = STANDARD.encode(key.sign(INPUT).unwrap());
        let entry = log.entry(INPUT, &signature, &certificate, time);
        Case {
            root: root.pem,
            rekor: log.pem(),
            trust: ReleaseTrust::parse(TRUST).unwrap(),
            input: INPUT.to_vec(),
            bundle: serde_json::json!({
                "base64Signature": signature,
                "cert": STANDARD.encode(&certificate),
                "rekorBundle": entry,
            }),
        }
    }

    fn case() -> Case {
        case_with(&Leaf::default(), now())
    }

    #[test]
    fn a_current_certificate_under_the_root_verifies() {
        let case = case();
        let verified = case.verify().unwrap();
        assert_eq!(verified.log_index, 7);
    }

    #[test]
    fn validity_is_judged_at_the_log_time_not_now() {
        // Expired today, valid when logged: still a good signature.
        let leaf = Leaf {
            not_before: now() - 100 * DAY,
            not_after: now() - 10 * DAY,
            ..Leaf::default()
        };
        case_with(&leaf, now() - 50 * DAY).verify().unwrap();
    }

    #[test]
    fn a_certificate_expired_when_logged_is_refused() {
        let leaf = Leaf {
            not_after: now() + DAY,
            ..Leaf::default()
        };
        let error = case_with(&leaf, now() + 2 * DAY).error();
        assert!(error.contains("had expired"), "{error}");
    }

    #[test]
    fn a_certificate_not_yet_valid_when_logged_is_refused() {
        let leaf = Leaf {
            not_before: now() + DAY,
            ..Leaf::default()
        };
        let error = case_with(&leaf, now()).error();
        assert!(error.contains("not yet valid"), "{error}");
    }

    #[test]
    fn another_root_is_refused() {
        let mut case = case();
        case.root = root("Tilo Release Root").pem;
        let error = case.error();
        assert!(
            error.contains("not issued by the root") || error.contains("did not sign"),
            "{error}"
        );
    }

    #[test]
    fn a_revoked_epoch_is_refused() {
        let mut case = case();
        case.trust = ReleaseTrust::parse(&TRUST.replace("EPOCH=1", "EPOCH=2")).unwrap();
        let error = case.error();
        assert!(error.contains("not the current"), "{error}");
    }

    #[test]
    fn another_purpose_is_refused() {
        let leaf = Leaf {
            purpose: "os-images",
            ..Leaf::default()
        };
        let error = case_with(&leaf, now()).error();
        assert!(
            error.contains("does not name extension-manifests@"),
            "{error}"
        );
    }

    #[test]
    fn other_content_is_refused() {
        let mut case = case();
        case.input = b"{\"schema\":2}\n".to_vec();
        let error = case.error();
        assert!(error.contains("other content"), "{error}");
    }

    #[test]
    fn a_timestamp_from_another_log_key_is_refused() {
        let mut case = case();
        case.rekor = Log::new().pem();
        let error = case.error();
        assert!(error.contains("another log"), "{error}");
    }

    #[test]
    fn a_forged_timestamp_is_refused() {
        let mut case = case();
        case.bundle["rekorBundle"]["Payload"]["integratedTime"] = serde_json::json!(now() - DAY);
        let error = case.error();
        assert!(error.contains("does not verify"), "{error}");
    }

    #[test]
    fn a_logged_entry_for_another_signature_or_certificate_is_refused() {
        // A second, valid signature by the same key that the log never saw.
        let root = root("Tilo Release Root");
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = Leaf::default().issue(&root, &key);
        let log = Log::new();
        let logged = STANDARD.encode(key.sign(INPUT).unwrap());
        let unlogged = STANDARD.encode(key.sign(INPUT).unwrap());
        let mut case = Case {
            root: root.pem.clone(),
            rekor: log.pem(),
            trust: ReleaseTrust::parse(TRUST).unwrap(),
            input: INPUT.to_vec(),
            bundle: serde_json::json!({
                "base64Signature": unlogged,
                "cert": STANDARD.encode(&certificate),
                "rekorBundle": log.entry(INPUT, &logged, &certificate, now()),
            }),
        };
        let error = case.error();
        assert!(error.contains("another signature"), "{error}");

        // The same signature, logged under another certificate for the key.
        let other = Leaf::default().issue(&root, &key);
        case.bundle["base64Signature"] = serde_json::json!(logged);
        case.bundle["rekorBundle"] = log.entry(INPUT, &logged, &other, now());
        let error = case.error();
        assert!(error.contains("another certificate"), "{error}");
    }

    #[test]
    fn a_ca_certificate_cannot_sign() {
        let leaf = Leaf {
            ca: true,
            ..Leaf::default()
        };
        let error = case_with(&leaf, now()).error();
        assert!(error.contains("CA certificate"), "{error}");
    }

    #[test]
    fn a_certificate_without_code_signing_is_refused() {
        let leaf = Leaf {
            code_signing: false,
            ..Leaf::default()
        };
        let error = case_with(&leaf, now()).error();
        assert!(error.contains("may not sign code"), "{error}");
    }

    #[test]
    fn issuer_extensions_must_agree_and_exist() {
        let leaf = Leaf {
            issuer_v1: Some("https://signing.tilo.so/release/epoch-2"),
            ..Leaf::default()
        };
        let error = case_with(&leaf, now()).error();
        assert!(error.contains("disagree"), "{error}");

        let leaf = Leaf {
            issuer_v1: None,
            issuer_v2: None,
            ..Leaf::default()
        };
        let error = case_with(&leaf, now()).error();
        assert!(error.contains("names no issuer"), "{error}");

        // Either extension alone carries the issuer.
        for (v1, v2) in [
            (Some("https://signing.tilo.so/release/epoch-1"), None),
            (None, Some("https://signing.tilo.so/release/epoch-1")),
        ] {
            let leaf = Leaf {
                issuer_v1: v1,
                issuer_v2: v2,
                ..Leaf::default()
            };
            case_with(&leaf, now()).verify().unwrap();
        }
    }

    #[test]
    fn a_signature_by_another_key_is_refused() {
        let mut case = case();
        let other = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        case.bundle["base64Signature"] =
            serde_json::json!(STANDARD.encode(other.sign(INPUT).unwrap()));
        // The log entry no longer matches either; the body check names it.
        assert!(case.verify().is_err());
    }

    #[test]
    fn trust_env_is_parsed_strictly() {
        let trust = ReleaseTrust::parse(TRUST).unwrap();
        assert_eq!(trust.issuer(), "https://signing.tilo.so/release/epoch-1");
        assert_eq!(
            trust.identity("os-images").unwrap(),
            "os-images@signing.tilo.so"
        );
        for bad in [
            TRUST.replace("EPOCH=1", "EPOCH=0"),
            TRUST.replace("EPOCH=1", "EPOCH=01"),
            TRUST.replace("EPOCH=1", "EPOCH=\"1\""),
            format!("{TRUST}TILO_RELEASE_EPOCH=2\n"),
            format!("{TRUST}export X=1\n"),
            TRUST.replace("TILO_RELEASE_IDENTITY_DOMAIN=signing.tilo.so\n", ""),
        ] {
            assert!(ReleaseTrust::parse(&bad).is_err(), "{bad}");
        }
        assert!(trust.identity("../x").is_err());
    }

    #[test]
    fn the_repository_trust_env_parses() {
        // Read at run time: the image builders compile this crate from a copy of the
        // repository that does not carry keys/.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../keys/release/trust.env");
        let text = std::fs::read_to_string(path).unwrap();
        let trust = ReleaseTrust::parse(&text).unwrap();
        assert!(trust.issuer().starts_with("https://"));
    }
}
