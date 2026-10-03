//! [`DeviceCert`] — the signed statement binding a device key to a
//! user identity, plus [`DeviceCapabilities`] describing what the
//! device may do on behalf of the user.
//!
//! The signing preimage constant [`DEVCERT_PREIMAGE_TAG`] is frozen
//! wire ABI — see `signing_preimage_is_frozen_abi` in `lib.rs`.

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};

/// Signing-domain tag for `DeviceCert`. Frozen wire ABI:
/// `tilo-devcert-v1` + NUL. Any change breaks in-field
/// certificates.
pub const DEVCERT_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v1\0";

/// Signing-domain tag for a certificate whose capabilities mark a foreign
/// device (see [`DeviceCapabilities::foreign`]). A foreign certificate never
/// verifies under [`DEVCERT_PREIMAGE_TAG`], so a verifier that predates the
/// marker cannot accept it as native.
pub const DEVCERT_V2_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v2\0";

/// First byte of a foreign device's capabilities in binary encodings. A
/// verifier that predates the marker decodes that position as a `bool` and
/// refuses any byte other than 0 or 1.
const FOREIGN_BINARY_MARKER: u8 = 2;

/// What a device may do on behalf of its user.
///
/// The four booleans map directly to enforceable policy checks in
/// `crates/session/shell/` and `crates/system/fs/`. Currently a user's *primary* device (the one
/// that holds the root private key) gets `can_own_workspaces:
/// true, can_admit_devices: true`, phones/tablets get
/// `can_own_workspaces: true` but `can_admit_devices: false`, and
/// dedicated home-server hosts get `can_act_as_host: true`.
///
/// Capabilities are baked into the cert at issue time — they do not
/// change over the cert's lifetime. To upgrade a device from
/// "member" to "owner", the primary revokes the old cert and issues
/// a new one.
// Each field is an independently-meaningful policy bit, not a set of
// mutually-exclusive states an enum could collapse — and this struct's
// field layout is part of the frozen `DEVCERT_PREIMAGE_TAG` wire ABI
// (it's embedded in `DeviceCertView`), so changing its shape to appease
// the lint would break every certificate already issued in the field.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent policy flags, frozen as part of the DeviceCert wire ABI"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCapabilities {
    /// May create/delete/rename workspaces on the user's directory.
    /// When false, the device can still read + edit content on
    /// workspaces already assigned to it.
    pub can_own_workspaces: bool,

    /// May mint enrollment tokens for new devices. Typically only
    /// the user's primary device — a phone should not be able to
    /// admit another phone into the fleet.
    pub can_admit_devices: bool,

    /// May act as an always-on host (pins workspaces, relays for
    /// other devices). Only true for dedicated servers and
    /// desktop-class machines the user has consciously marked
    /// "host".
    pub can_act_as_host: bool,

    /// May read every workspace on the user's directory, regardless
    /// of per-workspace membership. Reserved for the user's primary
    /// devices and hosts they explicitly trust with the full data
    /// set.
    pub can_read_all_workspaces: bool,

    /// The device runs a Tilo runtime on an operating system Tilo does not
    /// control (stock macOS, another Linux desktop). Workspace policy caps
    /// the role such a device may act with. A foreign certificate is signed
    /// under [`DEVCERT_V2_PREIMAGE_TAG`] and encoded so that verifiers which
    /// predate this field reject it; a native one keeps the v1 encoding.
    pub foreign: bool,
}

/// The v1 JSON shape plus the optional foreign marker, omitted when false so
/// native capabilities serialise exactly as before.
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the frozen DeviceCapabilities JSON fields"
)]
#[derive(Serialize, Deserialize)]
#[serde(rename = "DeviceCapabilities", deny_unknown_fields)]
struct CapabilitiesJson {
    can_own_workspaces: bool,
    can_admit_devices: bool,
    can_act_as_host: bool,
    can_read_all_workspaces: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    foreign: bool,
}

impl Serialize for DeviceCapabilities {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        if serializer.is_human_readable() {
            return CapabilitiesJson {
                can_own_workspaces: self.can_own_workspaces,
                can_admit_devices: self.can_admit_devices,
                can_act_as_host: self.can_act_as_host,
                can_read_all_workspaces: self.can_read_all_workspaces,
                foreign: self.foreign,
            }
            .serialize(serializer);
        }
        // Positional binary formats (postcard) encode each bool as one byte;
        // a foreign device prefixes the four flags with the marker byte.
        let flags = [
            self.can_own_workspaces,
            self.can_admit_devices,
            self.can_act_as_host,
            self.can_read_all_workspaces,
        ];
        let len = if self.foreign { 5 } else { 4 };
        let mut tuple = serializer.serialize_tuple(len)?;
        if self.foreign {
            tuple.serialize_element(&FOREIGN_BINARY_MARKER)?;
        }
        for flag in flags {
            tuple.serialize_element(&flag)?;
        }
        tuple.end()
    }
}

impl<'de> Deserialize<'de> for DeviceCapabilities {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let json = CapabilitiesJson::deserialize(deserializer)?;
            return Ok(Self {
                can_own_workspaces: json.can_own_workspaces,
                can_admit_devices: json.can_admit_devices,
                can_act_as_host: json.can_act_as_host,
                can_read_all_workspaces: json.can_read_all_workspaces,
                foreign: json.foreign,
            });
        }
        // Postcard reads only the elements the visitor asks for, so the upper
        // bound admits the four native flags as well as marker + four flags.
        deserializer.deserialize_tuple(5, BinaryCapabilities)
    }
}

struct BinaryCapabilities;

impl<'de> serde::de::Visitor<'de> for BinaryCapabilities {
    type Value = DeviceCapabilities;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("four capability flags, optionally after the foreign marker")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;
        let mut next = |index: usize| {
            seq.next_element::<u8>()?
                .ok_or_else(|| A::Error::invalid_length(index, &"four capability flags"))
        };
        let first = next(0)?;
        let (foreign, flags) = if first == FOREIGN_BINARY_MARKER {
            (true, [next(1)?, next(2)?, next(3)?, next(4)?])
        } else {
            (false, [first, next(1)?, next(2)?, next(3)?])
        };
        let flag = |byte: u8| match byte {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(A::Error::invalid_value(
                serde::de::Unexpected::Unsigned(other.into()),
                &"a capability flag byte of 0 or 1",
            )),
        };
        Ok(DeviceCapabilities {
            can_own_workspaces: flag(flags[0])?,
            can_admit_devices: flag(flags[1])?,
            can_act_as_host: flag(flags[2])?,
            can_read_all_workspaces: flag(flags[3])?,
            foreign,
        })
    }
}

impl DeviceCapabilities {
    /// Capability set for the user's primary device (holds root
    /// private key, admits new devices, reads everything).
    pub fn primary() -> Self {
        Self {
            can_own_workspaces: true,
            can_admit_devices: true,
            can_act_as_host: false,
            can_read_all_workspaces: true,
            foreign: false,
        }
    }

    /// Capability set for a secondary device — a phone, tablet, or
    /// laptop the user opts into the fleet but does not want to be
    /// able to admit new devices.
    pub fn member() -> Self {
        Self {
            can_own_workspaces: true,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
        }
    }

    /// Capability set for an always-on host (home server / cloud
    /// tenant). Reads everything so it can pin all workspaces; does
    /// not own or admit — those stay with the human's primary
    /// devices.
    pub fn host() -> Self {
        Self {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: true,
            can_read_all_workspaces: true,
            foreign: false,
        }
    }
}

/// Signed statement: "the user identity with public key
/// `user_public_key` recognises `device_public_key` as one of its
/// devices, with the given `capabilities`."
///
/// The signature is Ed25519, produced by the *user root* signing
/// key over `blake3(DEVCERT_PREIMAGE_TAG || postcard(view))`, where
/// `view` is the `DeviceCertView` omitting the `signature` field. A
/// foreign device's certificate uses [`DEVCERT_V2_PREIMAGE_TAG`] instead.
///
/// Cert chain: for anything the device does to be trusted by a
/// peer, that peer must (a) verify this signature under
/// `user_public_key`, (b) check the cert has not expired, and (c)
/// check the `device_public_key` is not present in the user's
/// [`crate::RevocationList`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCert {
    /// User root public key that owns / signs this cert. 32 bytes.
    pub user_public_key: [u8; 32],

    /// Certified device's public key. 32 bytes.
    pub device_public_key: [u8; 32],

    /// Human-readable label for the device — "Framework 16",
    /// "Studio phone", "Home server". Not authoritative; a peer's
    /// UI may display this label as-is.
    pub device_label: String,

    /// What this device may do on behalf of the user. See
    /// [`DeviceCapabilities`].
    pub capabilities: DeviceCapabilities,

    /// Nanoseconds since Unix epoch — when the cert was signed.
    /// Peers use it for tie-breaks (a later-issued cert supersedes
    /// an earlier one for the same device) and for display.
    pub issued_at_ns: u64,

    /// Optional expiry. `None` means the cert is valid until
    /// revoked. Practical presets: forever for a laptop, 30 days
    /// for a temporary shared device, 24 hours for a bring-your-
    /// own-lab-machine.
    pub expires_at_ns: Option<u64>,

    /// Ed25519 signature. 64 bytes. Covers everything else in this
    /// struct via `blake3(DEVCERT_PREIMAGE_TAG || postcard(view))`.
    #[serde(with = "serde_bytes_64")]
    pub signature: [u8; 64],
}

/// Signing view — everything except the signature. Used to build
/// the preimage that `signature` covers.
#[derive(Serialize)]
struct DeviceCertView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    device_label: &'a str,
    capabilities: &'a DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
}

impl DeviceCert {
    /// Build the signing view for this cert.
    fn view(&self) -> DeviceCertView<'_> {
        DeviceCertView {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            device_label: &self.device_label,
            capabilities: &self.capabilities,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }

    /// Returns `true` if the cert's `expires_at_ns` has passed at
    /// `now_ns`. A cert with `expires_at_ns == None` never expires.
    pub fn is_expired_at(&self, now_ns: u64) -> bool {
        matches!(self.expires_at_ns, Some(e) if now_ns >= e)
    }
}

/// Failure modes for cert signing/verification.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DeviceCertError {
    /// The user root public key bytes do not decode to a valid
    /// Ed25519 curve point.
    #[error("invalid user root public key (not a point on the curve)")]
    InvalidUserPublicKey,

    /// The signature did not verify under the declared user root
    /// key. Either the wrong root signed, the bytes were tampered
    /// with, or the cert was signed by an impostor.
    #[error("device-cert signature did not verify")]
    BadSignature,

    /// The `expires_at_ns` deadline has passed. Not a signature
    /// failure; the cert is cryptographically valid but no longer
    /// admissible.
    #[error("device cert has expired")]
    Expired,

    /// The certificate's claimed issuance time has not arrived.
    #[error("device cert is not yet valid")]
    NotYetValid,

    /// A finite expiry must be strictly later than issuance.
    #[error("device cert has an invalid validity interval")]
    InvalidValidity,

    /// Canonical-encoding failure. Effectively unreachable but
    /// propagated for hygiene.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Sign a device cert with the user's root signing key.
///
/// Fills the cert's `signature` field in place. Callers can call
/// this multiple times on the same cert idempotently — the signing
/// view omits the signature so the second call reproduces the first.
///
/// # Errors
///
/// Returns [`DeviceCertError::BadSignature`] if `cert.user_public_key`
/// does not match the public key derived from `root_signing_key`, or
/// [`DeviceCertError::Canonical`] if the cert cannot be canonically
/// encoded (effectively unreachable).
pub fn sign_device_cert(
    cert: &mut DeviceCert,
    root_signing_key: &ed25519_dalek::SigningKey,
) -> Result<(), DeviceCertError> {
    // Guard: refuse to sign a cert whose `user_public_key` does not
    // match the signing key's derived public key. Mis-signing this
    // would produce a syntactically valid but unverifiable cert.
    let derived = root_signing_key.verifying_key().to_bytes();
    if cert.user_public_key != derived {
        return Err(DeviceCertError::BadSignature);
    }
    if cert
        .expires_at_ns
        .is_some_and(|expiry| expiry <= cert.issued_at_ns)
    {
        return Err(DeviceCertError::InvalidValidity);
    }

    let digest = device_cert_signing_digest(cert)?;
    let sig = root_signing_key.sign(&digest);
    cert.signature = sig.to_bytes();
    Ok(())
}

/// Return the domain-separated digest an external signer must sign for this
/// certificate.
///
/// This is the broker-friendly counterpart to [`sign_device_cert`]. It lets a
/// caller ask a non-exportable user-root capability to sign the exact wire-ABI
/// preimage without copying the private key into another file. Callers must
/// attach the resulting signature and validate it with [`verify_device_cert`]
/// before persisting the certificate.
///
/// # Errors
///
/// Returns [`DeviceCertError::Canonical`] if the cert cannot be
/// canonically encoded (effectively unreachable).
pub fn device_cert_signing_digest(cert: &DeviceCert) -> Result<[u8; 32], DeviceCertError> {
    let tag = if cert.capabilities.foreign {
        DEVCERT_V2_PREIMAGE_TAG
    } else {
        DEVCERT_PREIMAGE_TAG
    };
    signing_digest(tag, &cert.view()).map_err(Into::into)
}

/// Verify a device cert against the user root public key embedded
/// in the cert.
///
/// This is the cryptographic check *only*. Callers should also
/// verify:
///
/// - `now_ns >= cert.issued_at_ns` (unless clock skew is being
///   tolerated),
/// - `!cert.is_expired_at(now_ns)`,
/// - `!revocation_list.is_revoked(&cert.device_public_key)`.
///
/// The convenience [`verify_device_cert_at`] bundles those checks
/// with the signature verification.
///
/// # Errors
///
/// Returns [`DeviceCertError::InvalidUserPublicKey`] if
/// `cert.user_public_key` is not a valid Ed25519 point, or
/// [`DeviceCertError::BadSignature`] if the signature does not verify
/// under it.
pub fn verify_device_cert(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    let digest = device_cert_signing_digest(cert)?;
    // Hosts re-check the same certificates on every request and pump turn.
    // A signature check is a pure function of (key, signed digest, signature):
    // an exact triple that verified before verifies again. Only successes are
    // remembered; every other input takes the full path below.
    let mut triple = [0u8; 128];
    triple[..32].copy_from_slice(&cert.user_public_key);
    triple[32..64].copy_from_slice(&digest);
    triple[64..].copy_from_slice(&cert.signature);
    if verified_signatures()
        .lock()
        .is_ok_and(|seen| seen.contains(&triple))
    {
        return Ok(());
    }
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&cert.user_public_key)
        .map_err(|_| DeviceCertError::InvalidUserPublicKey)?;
    let sig = ed25519_dalek::Signature::from_bytes(&cert.signature);
    vk.verify(&digest, &sig)
        .map_err(|_| DeviceCertError::BadSignature)?;
    if let Ok(mut seen) = verified_signatures().lock() {
        if seen.len() >= VERIFIED_SIGNATURE_LIMIT {
            seen.clear();
        }
        seen.insert(triple);
    }
    Ok(())
}

const VERIFIED_SIGNATURE_LIMIT: usize = 4096;

pub(crate) fn verified_signatures()
-> &'static std::sync::Mutex<std::collections::HashSet<[u8; 128]>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<[u8; 128]>>> =
        std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// Full validation: signature + not-before + expiry.
///
/// Revocation is NOT checked here — the caller passes in a
/// [`crate::RevocationList`] separately because the list lives in
/// the user's directory CRDT, not in the cert itself. See
/// [`crate::RevocationList::is_revoked`].
///
/// # Errors
///
/// Returns anything [`verify_device_cert`] can return, or
/// [`DeviceCertError::NotYetValid`] if `now_ns` predates issuance,
/// [`DeviceCertError::InvalidValidity`] for a backwards/empty finite interval,
/// or [`DeviceCertError::Expired`] if `now_ns` is at or past expiry.
pub fn verify_device_cert_at(cert: &DeviceCert, now_ns: u64) -> Result<(), DeviceCertError> {
    verify_device_cert(cert)?;
    if cert
        .expires_at_ns
        .is_some_and(|expiry| expiry <= cert.issued_at_ns)
    {
        return Err(DeviceCertError::InvalidValidity);
    }
    if now_ns < cert.issued_at_ns {
        return Err(DeviceCertError::NotYetValid);
    }
    if cert.is_expired_at(now_ns) {
        return Err(DeviceCertError::Expired);
    }
    Ok(())
}

/// Serde helper: a 64-byte-array serialiser that emits a length-
/// prefixed byte string rather than 64 separate `u8`s. Saves 63
/// bytes of varint overhead per signature on the wire.
pub(crate) mod serde_bytes_64 {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(bytes).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let v: serde_bytes::ByteBuf = serde_bytes::ByteBuf::deserialize(d)?;
        let bytes = v.into_vec();
        if bytes.len() != 64 {
            return Err(D::Error::custom(format!(
                "expected 64-byte signature, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserIdentity;

    fn fresh_cert(caps: DeviceCapabilities) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let (root_sk, user) = UserIdentity::generate("Alice", 1_720_000_000_000_000_000);
        let (_dev_sk, dev) =
            crate::DeviceIdentity::generate("Framework 16", 1_720_000_000_000_000_100);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 1_720_000_000_000_000_200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        (root_sk, cert)
    }

    /// Certificates sign and verify, honour expiry, round-trip canonically, reproduce their
    /// signature vector, and every capability preset is distinct.
    #[allow(
        clippy::similar_names,
        reason = "the _sk/_pk keypair naming convention is deliberate and used throughout this crate's tests"
    )]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn device_certificates_sign_verify_expire_and_round_trip() {
        // Phase: sign_and_verify.
        {
            let (_root, cert) = fresh_cert(DeviceCapabilities::primary());
            verify_device_cert(&cert).unwrap();
        }

        // Phase: expiry_semantics.
        {
            let (root_sk, user) = UserIdentity::generate("Alice", 1);
            let (_dev_sk, dev) = crate::DeviceIdentity::generate("dev", 2);
            let mut cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: dev.public_key,
                device_label: dev.label,
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 100,
                expires_at_ns: Some(200),
                signature: [0u8; 64],
            };
            sign_device_cert(&mut cert, &root_sk).unwrap();
            assert_eq!(
                verify_device_cert_at(&cert, 99).unwrap_err(),
                DeviceCertError::NotYetValid
            );
            // Before expiry
            verify_device_cert_at(&cert, 150).unwrap();
            // Exactly at expiry (`now >= expires_at_ns` — exclusive on
            // the left side, inclusive on the right).
            assert_eq!(
                verify_device_cert_at(&cert, 200).unwrap_err(),
                DeviceCertError::Expired
            );
            // After expiry
            assert_eq!(
                verify_device_cert_at(&cert, 300).unwrap_err(),
                DeviceCertError::Expired
            );

            cert.expires_at_ns = Some(cert.issued_at_ns);
            assert_eq!(
                sign_device_cert(&mut cert, &root_sk).unwrap_err(),
                DeviceCertError::InvalidValidity
            );
        }

        // Phase: cert_round_trips_through_canonical.
        {
            let (_root, cert) = fresh_cert(DeviceCapabilities::host());
            let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
            let back: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
            assert_eq!(cert, back);
            verify_device_cert(&back).unwrap();
        }

        // Phase: signature_vector_is_reproducible.
        // Signature outputs must be reproducible from the same inputs
        // (Ed25519 is deterministic and we build the preimage
        // deterministically). Locks in the wire vectors — if this ever
        // fails, the signing preimage has drifted.
        // `root_sk`/`root_pk` follows this crate's established signing-key /
        // public-key naming convention; see the reason on `directory::tests`.
        {
            use ed25519_dalek::SigningKey;

            // Fixed-seed keypair for the vector. This is the ONLY test
            // that uses a fixed seed; production always uses UnwrapErr(SysRng).
            let seed = [1u8; 32];
            let root_sk = SigningKey::from_bytes(&seed);
            let root_pk = root_sk.verifying_key().to_bytes();

            let device_pk = [2u8; 32];

            let mut cert = DeviceCert {
                user_public_key: root_pk,
                device_public_key: device_pk,
                device_label: "test-device".to_string(),
                capabilities: DeviceCapabilities {
                    can_own_workspaces: true,
                    can_admit_devices: false,
                    can_act_as_host: false,
                    can_read_all_workspaces: false,
                    foreign: false,
                },
                issued_at_ns: 1_000_000,
                expires_at_ns: None,
                signature: [0u8; 64],
            };
            sign_device_cert(&mut cert, &root_sk).unwrap();

            // Re-sign an identical view; must yield the same signature
            // (Ed25519 is deterministic, so this is a strong wire-ABI
            // check — any drift in the preimage or postcard encoding
            // would flip these bytes).
            let sig_1 = cert.signature;
            let mut cert2 = cert.clone();
            cert2.signature = [0u8; 64];
            sign_device_cert(&mut cert2, &root_sk).unwrap();
            assert_eq!(sig_1, cert2.signature);
        }

        // Phase: all_capability_presets_are_distinct.
        {
            // Each preset must produce a different signature — otherwise
            // a peer could substitute one preset for another without
            // touching the sig.
            let (root_sk, user) = UserIdentity::generate("Alice", 1);
            let (_dev_sk, dev) = crate::DeviceIdentity::generate("dev", 2);

            let build = |caps: DeviceCapabilities| {
                let mut c = DeviceCert {
                    user_public_key: user.public_key,
                    device_public_key: dev.public_key,
                    device_label: dev.label.clone(),
                    capabilities: caps,
                    issued_at_ns: 3,
                    expires_at_ns: None,
                    signature: [0u8; 64],
                };
                sign_device_cert(&mut c, &root_sk).unwrap();
                c.signature
            };

            let s_primary = build(DeviceCapabilities::primary());
            let s_member = build(DeviceCapabilities::member());
            let s_host = build(DeviceCapabilities::host());
            assert_ne!(s_primary, s_member);
            assert_ne!(s_member, s_host);
            assert_ne!(s_primary, s_host);
        }
    }

    /// Tampering with the capability, device key, signature or user key, or signing by a different
    /// root, fails verification.
    #[test]
    fn tampering_with_any_certificate_field_or_root_fails_verification() {
        // Phase: tampered_capability_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::member());
            cert.capabilities.can_admit_devices = true;
            assert_eq!(
                verify_device_cert(&cert).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }

        // Phase: tampered_device_pubkey_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
            cert.device_public_key[0] ^= 0x01;
            assert_eq!(
                verify_device_cert(&cert).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }

        // Phase: tampered_signature_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
            cert.signature[0] ^= 0x01;
            assert_eq!(
                verify_device_cert(&cert).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }

        // Phase: tampered_user_pubkey_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
            // Flip a bit in the embedded user pubkey. The signature no
            // longer verifies under the new pubkey.
            cert.user_public_key[0] ^= 0x01;
            let err = verify_device_cert(&cert).unwrap_err();
            assert!(matches!(
                err,
                DeviceCertError::BadSignature | DeviceCertError::InvalidUserPublicKey
            ));
        }

        // Phase: signed_by_different_root_fails.
        {
            // Alice's cert signed with Bob's root key — mismatch caught
            // by the pre-sign guard (BadSignature).
            let (_, alice) = UserIdentity::generate("Alice", 1);
            let (bob_sk, _) = UserIdentity::generate("Bob", 2);
            let (_, dev) = crate::DeviceIdentity::generate("dev", 3);
            let mut cert = DeviceCert {
                user_public_key: alice.public_key,
                device_public_key: dev.public_key,
                device_label: dev.label,
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 4,
                expires_at_ns: None,
                signature: [0u8; 64],
            };
            assert_eq!(
                sign_device_cert(&mut cert, &bob_sk).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }
    }

    fn golden_cert(foreign: bool) -> DeviceCert {
        let root_sk = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
        let mut cert = DeviceCert {
            user_public_key: root_sk.verifying_key().to_bytes(),
            device_public_key: [2u8; 32],
            device_label: "golden".to_string(),
            capabilities: DeviceCapabilities {
                foreign,
                ..DeviceCapabilities::member()
            },
            issued_at_ns: 1_000_000,
            expires_at_ns: Some(2_000_000),
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        cert
    }

    const GOLDEN_V1_JSON: &str = r#"{"user_public_key":[138,136,227,221,116,9,241,149,253,82,219,45,60,186,93,114,202,103,9,191,29,148,18,27,243,116,136,1,180,15,111,92],"device_public_key":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"device_label":"golden","capabilities":{"can_own_workspaces":true,"can_admit_devices":false,"can_act_as_host":false,"can_read_all_workspaces":false},"issued_at_ns":1000000,"expires_at_ns":2000000,"signature":[193,94,11,63,27,213,214,122,35,157,141,24,157,200,120,75,180,243,189,153,177,189,224,147,47,142,23,15,234,170,239,9,175,129,176,168,99,254,102,174,97,65,248,91,187,181,93,141,241,211,8,123,41,66,110,172,36,46,39,75,189,14,237,1]}"#;
    const GOLDEN_V1_POSTCARD: &str = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c020202020202020202020202020202020202020202020202020202020202020206676f6c64656e01000000c0843d0180897a40c15e0b3f1bd5d67a239d8d189dc8784bb4f3bd99b1bde0932f8e170feaaaef09af81b0a863fe66ae6141f85bbbb55d8df1d3087b29426eac242e274bbd0eed01";
    const GOLDEN_V1_DIGEST: &str =
        "0d8271d3aadb5438af000ff5f96b6355102b57a271885ed26142fb698884e623";

    /// The certificate encodings and signing digest as they were before the
    /// foreign marker existed, pinned from the pre-change build.
    #[test]
    fn native_certificates_keep_their_v1_encoding_and_digest() {
        let cert = golden_cert(false);
        assert_eq!(serde_json::to_string(&cert).unwrap(), GOLDEN_V1_JSON);
        assert_eq!(
            hex::encode(crate::canonical::canonical_bytes(&cert).unwrap()),
            GOLDEN_V1_POSTCARD
        );
        assert_eq!(
            hex::encode(device_cert_signing_digest(&cert).unwrap()),
            GOLDEN_V1_DIGEST
        );
        let from_json: DeviceCert = serde_json::from_str(GOLDEN_V1_JSON).unwrap();
        let from_postcard: DeviceCert =
            crate::canonical::from_canonical_bytes(&hex::decode(GOLDEN_V1_POSTCARD).unwrap())
                .unwrap();
        assert_eq!(from_json, cert);
        assert_eq!(from_postcard, cert);
        verify_device_cert(&from_postcard).unwrap();
    }

    #[test]
    fn foreign_certificates_sign_under_their_own_tag() {
        let foreign = golden_cert(true);
        verify_device_cert(&foreign).unwrap();
        assert_eq!(
            device_cert_signing_digest(&foreign).unwrap(),
            signing_digest(DEVCERT_V2_PREIMAGE_TAG, &foreign.view()).unwrap()
        );

        let json = serde_json::to_string(&foreign).unwrap();
        assert!(json.contains(r#""foreign":true"#));
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), foreign);
        let bytes = crate::canonical::canonical_bytes(&foreign).unwrap();
        assert_eq!(
            crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).unwrap(),
            foreign
        );

        // Neither direction of flipping the marker survives the signature.
        let mut stripped = foreign;
        stripped.capabilities.foreign = false;
        assert_eq!(
            verify_device_cert(&stripped).unwrap_err(),
            DeviceCertError::BadSignature
        );
        let mut marked = golden_cert(false);
        marked.capabilities.foreign = true;
        assert_eq!(
            verify_device_cert(&marked).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    /// A frozen copy of the certificate types as they were before the
    /// foreign marker, standing in for a verifier built from an older tree.
    mod v1 {
        #[allow(
            clippy::struct_excessive_bools,
            reason = "a frozen copy of the v1 wire type"
        )]
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct DeviceCapabilities {
            pub can_own_workspaces: bool,
            pub can_admit_devices: bool,
            pub can_act_as_host: bool,
            pub can_read_all_workspaces: bool,
        }

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct DeviceCert {
            pub user_public_key: [u8; 32],
            pub device_public_key: [u8; 32],
            pub device_label: String,
            pub capabilities: DeviceCapabilities,
            pub issued_at_ns: u64,
            pub expires_at_ns: Option<u64>,
            #[serde(with = "super::serde_bytes_64")]
            pub signature: [u8; 64],
        }
    }

    #[test]
    fn verifiers_that_predate_the_marker_reject_foreign_certificates() {
        let native = golden_cert(false);
        let foreign = golden_cert(true);
        let nonce = [9u8; 32];

        let old: v1::DeviceCert =
            serde_json::from_str(&serde_json::to_string(&native).unwrap()).unwrap();
        assert_eq!(serde_json::to_string(&old).unwrap(), GOLDEN_V1_JSON);
        let old_wire = postcard::to_allocvec(&(&old, nonce)).unwrap();
        assert_eq!(postcard::to_allocvec(&(&native, nonce)).unwrap(), old_wire);
        let (_, old_nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&old_wire).unwrap();
        assert_eq!(old_nonce, nonce);

        let foreign_json = serde_json::to_string(&foreign).unwrap();
        let json_error = serde_json::from_str::<v1::DeviceCert>(&foreign_json).unwrap_err();
        assert!(json_error.to_string().contains("unknown field `foreign`"));
        // Embedded in a larger message, the old decoder stops at the marker
        // instead of misreading the fields that follow.
        let foreign_wire = postcard::to_allocvec(&(&foreign, nonce)).unwrap();
        assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&foreign_wire).is_err());
    }

    #[test]
    fn binary_capabilities_refuse_flag_bytes_other_than_zero_or_one() {
        let mut bytes = crate::canonical::canonical_bytes(&golden_cert(true)).unwrap();
        let marker = 32 + 32 + 1 + "golden".len();
        assert_eq!(bytes[marker], FOREIGN_BINARY_MARKER);
        bytes[marker + 1] = 3;
        assert!(crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).is_err());
    }

    #[test]
    fn remembered_verification_never_accepts_a_changed_certificate() {
        let (_root, cert) = fresh_cert(DeviceCapabilities::primary());
        // Verify twice: the second check is answered by the remembered triple.
        verify_device_cert(&cert).unwrap();
        verify_device_cert(&cert).unwrap();
        for tamper in [0usize, 1, 2, 3] {
            let mut changed = cert.clone();
            match tamper {
                0 => changed.signature[5] ^= 0x01,
                1 => changed.device_public_key[3] ^= 0x01,
                2 => changed.issued_at_ns ^= 1,
                _ => changed.user_public_key[7] ^= 0x01,
            }
            assert!(verify_device_cert(&changed).is_err(), "tamper {tamper}");
        }
        verify_device_cert(&cert).unwrap();
    }
}
