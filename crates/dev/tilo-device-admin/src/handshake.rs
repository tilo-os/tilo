//! The device-admin handshake.
//!
//! ```text
//! dialer → device:  Hello { cert, nonce }
//! device:           admits the dialer's certificate, or answers Refused
//! device → dialer:  Answer::Challenge { cert, nonce, proof over the dialer's nonce }
//! dialer:           checks the device's certificate and proof
//! dialer → device:  Proof { proof over the device's nonce }
//! device → dialer:  Verdict::Admitted { session } | Verdict::Refused
//! ```
//!
//! Each proof is an Ed25519 signature by a device key over a digest of the
//! signer's role, [`crate::ALPN`], the nonce, and both endpoint ids of the
//! QUIC connection as the transport authenticated them. A proof made for one
//! connection proves nothing on another, so a peer in the middle cannot pass
//! one side's proof on to the other. After `Admitted` the stream carries
//! tilo-vmd's request lines unchanged.

use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use tilo_user_identity::{DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at};

/// Version carried in [`Hello`]; the ALPN already names the major version.
pub const VERSION: u32 = 1;

/// Domain tag bound into every proof.
const PROOF_TAG: &[u8] = b"tilo-device-admin-proof-v1\0";

/// No handshake frame comes near this; a certificate is a few hundred bytes.
pub const MAX_FRAME: usize = 16 * 1024;

/// First frame: dialer → device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub cert: DeviceCert,
    pub nonce: [u8; 32],
}

/// Second frame: device → dialer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Answer {
    Challenge(Box<Challenge>),
    Refused(Refusal),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Challenge {
    pub cert: DeviceCert,
    pub nonce: [u8; 32],
    /// 64 bytes: the device key's signature over [`Role::Device`]'s digest.
    pub proof: Vec<u8>,
}

/// Third frame: dialer → device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proof {
    /// 64 bytes: the dialer's device key over [`Role::Administrator`]'s digest.
    pub proof: Vec<u8>,
}

/// Last frame: device → dialer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Verdict {
    /// The stream now carries tilo-vmd's requests; `session` names this
    /// session in the device's journal.
    Admitted {
        session: u64,
    },
    Refused(Refusal),
}

/// Why a device refused the route. Every reason says how to fix it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The certificate presented does not verify: forged, damaged, or not
    /// signed by the root it names.
    NotCertified,
    /// The certificate has expired or is not valid yet.
    OutsideValidity,
    /// The certificate belongs to another identity than the device's.
    OtherIdentity,
    /// The person removed this machine from their devices.
    Revoked,
    /// The certificate is the person's, but does not grant administration.
    NotAdministrator,
    /// The dialer did not prove it holds the certificate's key.
    BadProof,
    /// The device belongs to no identity yet, so nobody can administer it.
    DeviceNotEnrolled,
    /// The device admitted the dialer but could not start tilo-vmd.
    DaemonUnavailable,
    /// The dialer speaks another handshake version.
    UnsupportedVersion,
}

impl Refusal {
    /// What the refusal means and what fixes it, for the person at the
    /// dialing machine.
    pub fn explain(self) -> &'static str {
        match self {
            Self::NotCertified => {
                "this machine's certificate does not verify; enrol it again (tilo-vm vm enrol-admin)"
            }
            Self::OutsideValidity => {
                "this machine's certificate has expired or is not valid yet (check both clocks); enrol it again (tilo-vm vm enrol-admin)"
            }
            Self::OtherIdentity => {
                "this machine is enrolled in another identity than the device's; enrol it with the device owner's identity (tilo-vm vm enrol-admin)"
            }
            Self::Revoked => {
                "the device owner removed this machine from their devices; enrol it again (tilo-vm vm enrol-admin)"
            }
            Self::NotAdministrator => {
                "this machine is one of the owner's devices but its certificate does not grant administration; enrol it for administration (tilo-vm vm enrol-admin)"
            }
            Self::BadProof => "this machine did not prove it holds its certificate's key",
            Self::DeviceNotEnrolled => {
                "the device belongs to no identity yet; pair it with its owner's identity first"
            }
            Self::DaemonUnavailable => {
                "the device admitted this machine but could not start tilo-vmd; see `journalctl -u tilo-device-admin` on the device"
            }
            Self::UnsupportedVersion => {
                "the device speaks another version of the route; update tilo-device-admin here or the device's image"
            }
        }
    }
}

/// Which side signs a proof. Distinct roles keep a device's proof from
/// standing in for a dialer's and the other way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    /// The device answering, over the dialer's nonce.
    Device = 1,
    /// The dialer, over the device's nonce.
    Administrator = 2,
}

/// The two endpoint ids of one QUIC connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub dialer: [u8; 32],
    pub device: [u8; 32],
}

/// The digest a proof signs.
pub fn proof_digest(role: Role, nonce: &[u8; 32], binding: &Binding) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PROOF_TAG);
    hasher.update(&[role as u8]);
    hasher.update(crate::ALPN);
    hasher.update(nonce);
    hasher.update(&binding.dialer);
    hasher.update(&binding.device);
    *hasher.finalize().as_bytes()
}

/// Sign `role`'s proof.
pub fn prove(
    key: &ed25519_dalek::SigningKey,
    role: Role,
    nonce: &[u8; 32],
    binding: &Binding,
) -> Vec<u8> {
    key.sign(&proof_digest(role, nonce, binding))
        .to_bytes()
        .to_vec()
}

/// Whether `proof` is `device_public_key`'s proof for `role`.
pub fn proof_holds(
    device_public_key: &[u8; 32],
    proof: &[u8],
    role: Role,
    nonce: &[u8; 32],
    binding: &Binding,
) -> bool {
    let Ok(signature) = <[u8; 64]>::try_from(proof) else {
        return false;
    };
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(device_public_key) else {
        return false;
    };
    key.verify(
        &proof_digest(role, nonce, binding),
        &ed25519_dalek::Signature::from_bytes(&signature),
    )
    .is_ok()
}

/// The device's admission rule for a dialer's certificate: the device's own
/// identity, valid now, not revoked, and granting administration.
///
/// # Errors
/// The [`Refusal`] that names the first rule the certificate breaks.
pub fn admit(
    cert: &DeviceCert,
    own_user_public_key: &[u8; 32],
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), Refusal> {
    match verify_device_cert_at(cert, now_ns) {
        Ok(()) => {}
        Err(
            DeviceCertError::Expired
            | DeviceCertError::NotYetValid
            | DeviceCertError::InvalidValidity,
        ) => return Err(Refusal::OutsideValidity),
        Err(_) => return Err(Refusal::NotCertified),
    }
    if &cert.user_public_key != own_user_public_key {
        return Err(Refusal::OtherIdentity);
    }
    if revocations.is_revoked_at(&cert.device_public_key, now_ns) {
        return Err(Refusal::Revoked);
    }
    if !cert.capabilities.can_administer_devices {
        return Err(Refusal::NotAdministrator);
    }
    Ok(())
}

/// Why the dialer does not trust the device that answered.
#[derive(Debug, PartialEq, Eq)]
pub enum Untrusted {
    /// Its certificate does not verify or is not valid now.
    Certificate(String),
    /// It is a device of another identity.
    OtherIdentity,
    /// It is a device of the person, but not the one asked for.
    OtherDevice { answered: [u8; 32] },
    /// It did not prove it holds its certificate's key on this connection.
    BadProof,
}

impl std::fmt::Display for Untrusted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Certificate(detail) => write!(f, "its certificate does not verify: {detail}"),
            Self::OtherIdentity => f.write_str("it belongs to another identity"),
            Self::OtherDevice { answered } => {
                write!(f, "device {} answered instead", hex::encode(answered))
            }
            Self::BadProof => f.write_str("it did not prove it holds its device key"),
        }
    }
}

/// The dialer's check of the device's [`Challenge`].
///
/// # Errors
/// What makes the answering device untrustworthy.
pub fn trust_device(
    challenge: &Challenge,
    own_user_public_key: &[u8; 32],
    expected_device: Option<&[u8; 32]>,
    client_nonce: &[u8; 32],
    binding: &Binding,
    now_ns: u64,
) -> Result<(), Untrusted> {
    verify_device_cert_at(&challenge.cert, now_ns)
        .map_err(|error| Untrusted::Certificate(error.to_string()))?;
    if &challenge.cert.user_public_key != own_user_public_key {
        return Err(Untrusted::OtherIdentity);
    }
    if let Some(expected) = expected_device
        && &challenge.cert.device_public_key != expected
    {
        return Err(Untrusted::OtherDevice {
            answered: challenge.cert.device_public_key,
        });
    }
    if !proof_holds(
        &challenge.cert.device_public_key,
        &challenge.proof,
        Role::Device,
        client_nonce,
        binding,
    ) {
        return Err(Untrusted::BadProof);
    }
    Ok(())
}

/// A fresh challenge nonce.
///
/// # Errors
/// When the system has no randomness to give.
pub fn nonce() -> std::io::Result<[u8; 32]> {
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(nonce)
}

/// Write one length-prefixed postcard frame.
///
/// # Errors
/// Encoding and transport failures.
pub async fn write_frame<T: Serialize + Sync>(
    writer: &mut (impl AsyncWrite + Unpin + Send),
    frame: &T,
) -> std::io::Result<()> {
    let body = postcard::to_stdvec(frame).map_err(std::io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(std::io::Error::other("handshake frame too large"));
    }
    let len = u32::try_from(body.len()).map_err(std::io::Error::other)?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

/// Read one length-prefixed postcard frame.
///
/// # Errors
/// Transport failures, oversized frames and undecodable ones.
pub async fn read_frame<T: for<'de> Deserialize<'de>>(
    reader: &mut (impl AsyncRead + Unpin + Send),
) -> std::io::Result<T> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).await?;
    let len = usize::try_from(u32::from_be_bytes(prefix)).map_err(std::io::Error::other)?;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "handshake frame too large",
        ));
    }
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body).await?;
    postcard::from_bytes(&body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, RevocationEntry, RevocationReason, revocation_signing_digest,
        sign_device_cert,
    };

    struct Person {
        root: ed25519_dalek::SigningKey,
    }

    impl Person {
        fn new(seed: u8) -> Self {
            Self {
                root: ed25519_dalek::SigningKey::from_bytes(&[seed; 32]),
            }
        }

        fn user(&self) -> [u8; 32] {
            self.root.verifying_key().to_bytes()
        }

        fn certify(
            &self,
            device: &ed25519_dalek::SigningKey,
            capabilities: DeviceCapabilities,
        ) -> DeviceCert {
            let mut cert = DeviceCert {
                user_public_key: self.user(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "test".into(),
                capabilities,
                issued_at_ns: 10,
                expires_at_ns: Some(1_000),
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &self.root).unwrap();
            cert
        }

        fn revoke(&self, device: [u8; 32]) -> RevocationList {
            let mut entry = RevocationEntry {
                user_public_key: self.user(),
                revoked_device_public_key: device,
                revoked_at_ns: 20,
                reason: RevocationReason::Lost,
                signature: [0; 64],
            };
            entry.signature = self
                .root
                .sign(&revocation_signing_digest(&entry).unwrap())
                .to_bytes();
            let mut list = RevocationList::new();
            list.insert(entry).unwrap();
            list
        }
    }

    /// The device admits only an unrevoked, valid administrator of its own
    /// identity, and names the first rule any other certificate breaks.
    #[test]
    fn admission_takes_only_the_owners_administrators() {
        let person = Person::new(1);
        let stranger = Person::new(2);
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let none = RevocationList::new();
        let admin = person.certify(&key, DeviceCapabilities::administrator());
        assert_eq!(admit(&admin, &person.user(), &none, 50), Ok(()));
        // Every preset without the grant is refused, the primary too.
        for preset in [
            DeviceCapabilities::primary(),
            DeviceCapabilities::member(),
            DeviceCapabilities::host(),
        ] {
            let cert = person.certify(&key, preset);
            assert_eq!(
                admit(&cert, &person.user(), &none, 50),
                Err(Refusal::NotAdministrator)
            );
        }
        let theirs = stranger.certify(&key, DeviceCapabilities::administrator());
        assert_eq!(
            admit(&theirs, &person.user(), &none, 50),
            Err(Refusal::OtherIdentity)
        );
        // A certificate that claims the person but was not signed by them.
        let mut forged = theirs;
        forged.user_public_key = person.user();
        assert_eq!(
            admit(&forged, &person.user(), &none, 50),
            Err(Refusal::NotCertified)
        );
        let mut damaged = admin.clone();
        damaged.signature[0] ^= 1;
        assert_eq!(
            admit(&damaged, &person.user(), &none, 50),
            Err(Refusal::NotCertified)
        );
        assert_eq!(
            admit(&admin, &person.user(), &none, 5_000),
            Err(Refusal::OutsideValidity)
        );
        let revoked = person.revoke(admin.device_public_key);
        assert_eq!(
            admit(&admin, &person.user(), &revoked, 50),
            Err(Refusal::Revoked)
        );
    }

    /// A proof holds only for its own role, nonce and connection.
    #[test]
    fn proofs_bind_role_nonce_and_connection() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let public = key.verifying_key().to_bytes();
        let binding = Binding {
            dialer: [5; 32],
            device: [6; 32],
        };
        let nonce = [7; 32];
        let proof = prove(&key, Role::Device, &nonce, &binding);
        assert!(proof_holds(&public, &proof, Role::Device, &nonce, &binding));
        assert!(!proof_holds(
            &public,
            &proof,
            Role::Administrator,
            &nonce,
            &binding
        ));
        assert!(!proof_holds(
            &public,
            &proof,
            Role::Device,
            &[8; 32],
            &binding
        ));
        let relayed = Binding {
            dialer: [9; 32],
            ..binding
        };
        assert!(!proof_holds(
            &public,
            &proof,
            Role::Device,
            &nonce,
            &relayed
        ));
        assert!(!proof_holds(
            &public,
            &proof[..63],
            Role::Device,
            &nonce,
            &binding
        ));
    }

    /// The dialer trusts only the device it asked for, of its own identity,
    /// proving its key on this connection.
    #[test]
    fn the_dialer_trusts_only_the_device_it_asked_for() {
        let person = Person::new(1);
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[10; 32]);
        let device = person.certify(&device_key, DeviceCapabilities::member());
        let binding = Binding {
            dialer: [11; 32],
            device: [12; 32],
        };
        let client_nonce = [13; 32];
        let challenge = Challenge {
            cert: device.clone(),
            nonce: [14; 32],
            proof: prove(&device_key, Role::Device, &client_nonce, &binding),
        };
        let expected = device.device_public_key;
        let trust = |challenge: &Challenge, user: &[u8; 32], expected: Option<&[u8; 32]>| {
            trust_device(challenge, user, expected, &client_nonce, &binding, 50)
        };
        assert_eq!(trust(&challenge, &person.user(), Some(&expected)), Ok(()));
        assert_eq!(trust(&challenge, &person.user(), None), Ok(()));
        assert_eq!(
            trust(&challenge, &Person::new(2).user(), Some(&expected)),
            Err(Untrusted::OtherIdentity)
        );
        assert!(matches!(
            trust(&challenge, &person.user(), Some(&[0; 32])),
            Err(Untrusted::OtherDevice { .. })
        ));
        let replayed = Challenge {
            proof: prove(&device_key, Role::Device, &[99; 32], &binding),
            ..challenge
        };
        assert_eq!(
            trust(&replayed, &person.user(), Some(&expected)),
            Err(Untrusted::BadProof)
        );
    }

    #[tokio::test]
    async fn frames_round_trip_and_oversized_ones_are_refused() {
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        write_frame(&mut a, &Verdict::Refused(Refusal::Revoked))
            .await
            .unwrap();
        let verdict: Verdict = read_frame(&mut b).await.unwrap();
        assert!(matches!(verdict, Verdict::Refused(Refusal::Revoked)));
        let len = u32::try_from(MAX_FRAME + 1).unwrap();
        a.write_all(&len.to_be_bytes()).await.unwrap();
        assert!(read_frame::<Verdict>(&mut b).await.is_err());
    }
}
