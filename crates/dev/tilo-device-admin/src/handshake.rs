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
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};

use tilo_user_identity::{DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at};

/// Version carried in [`Hello`]; the ALPN already names the major version.
pub const VERSION: u32 = 1;

/// Domain tag bound into every proof.
const PROOF_TAG: &[u8] = b"tilo-device-admin-proof-v1\0";

/// No handshake frame comes near this; a certificate is a few hundred bytes.
pub const MAX_FRAME: usize = 16 * 1024;

const FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(MAX_FRAME);

/// First frame: dialer → device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub cert: DeviceCert,
    pub nonce: [u8; 32],
}

/// Second frame: device → dialer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    Challenge(Box<Challenge>),
    Refused(Refusal),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub cert: DeviceCert,
    pub nonce: [u8; 32],
    /// 64 bytes: the device key's signature over [`Role::Device`]'s digest.
    pub proof: Vec<u8>,
}

/// Third frame: dialer → device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    /// 64 bytes: the dialer's device key over [`Role::Administrator`]'s digest.
    pub proof: Vec<u8>,
}

/// Last frame: device → dialer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
/// in place of a dialer's and the other way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
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
    if revocations.refuses_at(cert, now_ns) {
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
pub fn nonce() -> std::io::Result<[u8; 32]> {
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(nonce)
}

/// Write one length-prefixed postcard frame.
pub async fn write_frame<T: Serialize + Sync>(
    writer: &mut (impl AsyncWrite + Unpin + Send),
    frame: &T,
) -> std::io::Result<()> {
    FRAMING
        .write_value_async(writer, frame)
        .await
        .map_err(std::io::Error::from)?;
    writer.flush().await
}

/// Read one length-prefixed postcard frame.
pub async fn read_frame<T: for<'de> Deserialize<'de>>(
    reader: &mut (impl AsyncRead + Unpin + Send),
) -> std::io::Result<T> {
    FRAMING
        .read_value_async(reader)
        .await
        .map_err(std::io::Error::from)?
        .ok_or_else(|| std::io::Error::from(wire_frame::FrameError::Closed))
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
                issuer: None,
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

    tilo_test_env::scenario! {
        fn the_handshake_admits_proves_and_trusts_only_what_it_should;

        phase admission_takes_only_the_owners_administrators;
        // The device admits only an unrevoked, valid administrator of its own
        // identity, and names the first rule any other certificate breaks.
        {
            let person = Person::new(1);
            let stranger = Person::new(2);
            let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
            let none = RevocationList::new();
            let admin = person.certify(&key, DeviceCapabilities::administrator());
            assert_eq!(admit(&admin, &person.user(), &none, 50), Ok(()));
            for preset in [
                DeviceCapabilities::primary(),
                DeviceCapabilities::member(),
                DeviceCapabilities::member(),
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

        phase proofs_bind_role_nonce_and_connection;
        // A proof holds only for its own role, nonce and connection.
        {
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

        phase the_dialer_trusts_only_the_device_it_asked_for;
        // The dialer trusts only the device it asked for, of its own identity,
        // proving its key on this connection.
        {
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
    }

    #[tokio::test]
    async fn frames_round_trip_flush_and_enforce_eof_and_bounds() {
        let (a, mut b) = tokio::io::duplex(64 * 1024);
        // BufWriter verifies write_frame explicitly flushes the frame.
        let mut buffered = tokio::io::BufWriter::new(a);
        write_frame(&mut buffered, &Verdict::Refused(Refusal::Revoked))
            .await
            .unwrap();
        let verdict: Verdict = read_frame(&mut b).await.unwrap();
        assert_eq!(verdict, Verdict::Refused(Refusal::Revoked));

        // Oversized outgoing payload fails before writing anything.
        let oversized = Proof {
            proof: vec![0_u8; MAX_FRAME + 1],
        };
        assert!(write_frame(&mut buffered, &oversized).await.is_err());

        // Oversized incoming length prefix returns InvalidData.
        let mut a = buffered.into_inner();
        let len = u32::try_from(MAX_FRAME + 1).unwrap();
        a.write_all(&len.to_be_bytes()).await.unwrap();
        let error = read_frame::<Verdict>(&mut b).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        // End of stream before a frame maps to UnexpectedEof.
        let mut empty: &[u8] = &[];
        let eof = read_frame::<Verdict>(&mut empty).await.unwrap_err();
        assert_eq!(eof.kind(), std::io::ErrorKind::UnexpectedEof);

        // Mid-frame truncation maps to UnexpectedEof.
        let mut truncated: &[u8] = &[0, 0, 0, 2, 0];
        let cut = read_frame::<Verdict>(&mut truncated).await.unwrap_err();
        assert_eq!(cut.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    async fn assert_wire_fixture<T>(value: &T, expected_hex: &str)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug + Sync,
    {
        let expected = hex::decode(expected_hex).unwrap();
        let mut wire = Vec::new();
        write_frame(&mut wire, value).await.unwrap();
        assert_eq!(wire, expected);
        let mut slice = expected.as_slice();
        let decoded: T = read_frame(&mut slice).await.unwrap();
        assert_eq!(decoded, *value);
        assert!(slice.is_empty());
    }

    #[tokio::test]
    async fn wire_bytes_match_frozen_fixtures() {
        // Literal bytes captured from main before migrating handshake.rs to wire-frame.
        const HELLO_HEX: &str = concat!(
            "000000b1018a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf37488",
            "01b40f6f5ced4928c628d1c2c6eae90338905995612959273a5c63f93636c146",
            "14ac8737d104746573740302000000000a01e8074021d1dab95870593c82352b",
            "f99c5035a414641ee0db93fb48bb1d2efffaf3b0a8d322166f0e3d839ca80798",
            "75e8ab990d27a63be78f930ccae924b64c4fd66c0b0d0d0d0d0d0d0d0d0d0d0d",
            "0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d",
        );
        const CHALLENGE_HEX: &str = concat!(
            "000000f0008a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf37488",
            "01b40f6f5c43a72e714401762df66b68c26dfbdf2682aaec9f2474eca4613e42",
            "4a0fbafd3c0474657374010000000a01e8074085f98aee2a18c1dfc1ffad3941",
            "db70aac1da9c7c7468edc48f7780a492228297f4232394e3e3df66e3a596eb84",
            "0fb3cfadadcf7187562bd3e96e143c030b68080e0e0e0e0e0e0e0e0e0e0e0e0e",
            "0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e402e169384f142720b59cace8a",
            "ae2a1745e7c1528b2ca4d6892ca30de98cc66fb8c66b2b8223e193035e49730b",
            "74578451f190df509db432950c5821e8782c570f",
        );
        const REFUSED_HEX: &str = "000000020104";
        const PROOF_HEX: &str = concat!(
            "0000004140d9e12b9546fed4b2c4d13880b2024c801f88951c4396ab939f9361",
            "6f166da37471351009c1dff8d52d312b54b22d56d41e0f7c9551962805ef29cb",
            "0430e03d09",
        );
        const ADMITTED_HEX: &str = "00000002002a";
        const VERDICT_REFUSED_HEX: &str = "000000020103";

        let person = Person::new(1);
        let admin_key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[10; 32]);
        let binding = Binding {
            dialer: [11; 32],
            device: [12; 32],
        };
        let client_nonce = [13; 32];
        let device_nonce = [14; 32];

        assert_wire_fixture(
            &Hello {
                version: VERSION,
                cert: person.certify(&admin_key, DeviceCapabilities::administrator()),
                nonce: client_nonce,
            },
            HELLO_HEX,
        )
        .await;
        assert_wire_fixture(
            &Answer::Challenge(Box::new(Challenge {
                cert: person.certify(&device_key, DeviceCapabilities::member()),
                nonce: device_nonce,
                proof: prove(&device_key, Role::Device, &client_nonce, &binding),
            })),
            CHALLENGE_HEX,
        )
        .await;
        assert_wire_fixture(&Answer::Refused(Refusal::NotAdministrator), REFUSED_HEX).await;
        assert_wire_fixture(
            &Proof {
                proof: prove(&admin_key, Role::Administrator, &device_nonce, &binding),
            },
            PROOF_HEX,
        )
        .await;
        assert_wire_fixture(&Verdict::Admitted { session: 42 }, ADMITTED_HEX).await;
        assert_wire_fixture(&Verdict::Refused(Refusal::Revoked), VERDICT_REFUSED_HEX).await;
    }
}
