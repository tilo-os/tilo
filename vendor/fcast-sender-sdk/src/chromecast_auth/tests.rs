//! Synthetic private authorities exist only in tests; no root store or device is
//! modified. RSA keys are ephemeral OS-random values generated once in memory.
use super::super::test_clock as time;
use super::super::{auth_crypto, cert_parser, certificates, rsa_crypto, tls};
use super::*;
use auth_crypto::{Digest as _, Sha256};
use certificates::{
    BasicConstraints, CertificateParams, CustomExtension, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rsa_crypto::{
    RsaPrivateKey,
    pkcs8::EncodePrivateKey as _,
    signature::{SignatureEncoding as _, Signer as _},
};
use std::sync::OnceLock;
use time::OffsetDateTime;

fn keys() -> &'static Vec<RsaPrivateKey> {
    static KEYS: OnceLock<Vec<RsaPrivateKey>> = OnceLock::new();
    KEYS.get_or_init(|| {
        (0..5)
            .map(|_| {
                RsaPrivateKey::new(&mut auth_rng::UnwrapErr(auth_random::SysRng), 2048)
                    .expect("isolated RSA key")
            })
            .collect()
    })
}
fn key(index: usize) -> KeyPair {
    let der = keys()[index].to_pkcs8_der().expect("ephemeral PKCS8");
    KeyPair::from_pkcs8_der_and_sign_algo(
        &pki::PrivatePkcs8KeyDer::from(der.as_bytes()),
        &certificates::PKCS_RSA_SHA256,
    )
    .expect("fixture RSA signing key")
}
fn sign(index: usize, data: &[u8]) -> Vec<u8> {
    rsa_crypto::pkcs1v15::SigningKey::<Sha256>::new(keys()[index].clone())
        .try_sign(data)
        .expect("fixture proof")
        .to_vec()
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    Root,
    Intermediate,
    Device,
    CrlRoot,
    CrlSigner,
    Tls,
}
fn params(part: Part, now: UnixTime) -> CertificateParams {
    let mut params = CertificateParams::new(vec!["isolated-cast-fixture".into()])
        .expect("certificate parameters");
    params
        .distinguished_name
        .push(DnType::CommonName, format!("isolated {part:?}"));
    params.not_before = OffsetDateTime::from_unix_timestamp(
        i64::try_from(now.as_secs()).unwrap().saturating_sub(60),
    )
    .expect("fixture time");
    params.not_after = OffsetDateTime::from_unix_timestamp(
        i64::try_from(now.as_secs())
            .unwrap()
            .saturating_add(172_800),
    )
    .expect("fixture time");
    params.serial_number = Some(
        match part {
            Part::Root => 3,
            Part::Intermediate => 13,
            Part::Device => 23,
            Part::CrlRoot => 33,
            Part::CrlSigner => 43,
            Part::Tls => 53,
        }
        .into(),
    );
    if matches!(part, Part::Root | Part::Intermediate | Part::CrlRoot) {
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(u8::from(
            part != Part::Intermediate,
        )));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    } else {
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    }
    params
}
struct Fixture {
    root: Vec<u8>,
    intermediate: Vec<u8>,
    device: Vec<u8>,
    crl_root: Vec<u8>,
    crl_signer: Vec<u8>,
    tls_leaf: Vec<u8>,
    tls_key: KeyPair,
    now: UnixTime,
}
impl Fixture {
    fn new() -> Self {
        Self::with(|_, _| {})
    }
    fn with(change: impl Fn(Part, &mut CertificateParams)) -> Self {
        let now = UnixTime::now();
        let parameters = |part| {
            let mut p = params(part, now);
            change(part, &mut p);
            p
        };
        let root_params = parameters(Part::Root);
        let root_key = key(0);
        let root = root_params
            .self_signed(&root_key)
            .expect("fixture root")
            .der()
            .to_vec();
        let intermediate_params = parameters(Part::Intermediate);
        let intermediate_key = key(1);
        let intermediate = intermediate_params
            .signed_by(
                &intermediate_key,
                &Issuer::from_params(&root_params, &root_key),
            )
            .expect("fixture intermediate")
            .der()
            .to_vec();
        let device = parameters(Part::Device)
            .signed_by(
                &key(2),
                &Issuer::from_params(&intermediate_params, &intermediate_key),
            )
            .expect("fixture device")
            .der()
            .to_vec();
        let crl_root_params = parameters(Part::CrlRoot);
        let crl_root_key = key(3);
        let crl_root = crl_root_params
            .self_signed(&crl_root_key)
            .expect("fixture CRL root")
            .der()
            .to_vec();
        let crl_signer = parameters(Part::CrlSigner)
            .signed_by(
                &key(4),
                &Issuer::from_params(&crl_root_params, &crl_root_key),
            )
            .expect("fixture CRL signer")
            .der()
            .to_vec();
        let tls_key = KeyPair::generate().expect("fixture TLS key");
        let tls_leaf = parameters(Part::Tls)
            .self_signed(&tls_key)
            .expect("fixture TLS leaf")
            .der()
            .to_vec();
        Self {
            root,
            intermediate,
            device,
            crl_root,
            crl_signer,
            tls_leaf,
            tls_key,
            now,
        }
    }
    fn tbs(&self) -> proto::TbsCrl {
        proto::TbsCrl {
            version: Some(0),
            not_before_seconds: Some(self.now.as_secs().saturating_sub(10)),
            not_after_seconds: Some(self.now.as_secs().saturating_add(86400)),
            revoked_public_key_hashes: vec![],
            revoked_serial_number_ranges: vec![],
        }
    }
    fn crl(&self, tbs: &proto::TbsCrl) -> Vec<u8> {
        let raw = tbs.encode_to_vec();
        proto::CrlBundle {
            crls: vec![proto::Crl {
                tbs: Some(raw.clone()),
                signer_cert: Some(self.crl_signer.clone()),
                signature: Some(sign(4, &raw)),
            }],
        }
        .encode_to_vec()
    }
    fn response(&self, pending: &Pending) -> proto::Response {
        let mut proof = pending.nonce.to_vec();
        proof.extend_from_slice(&pending.tls_leaf);
        proto::Response {
            signature: sign(2, &proof),
            client_auth_certificate: self.device.clone(),
            intermediate_certificate: vec![self.intermediate.clone()],
            signature_algorithm: Some(1),
            sender_nonce: Some(pending.nonce.to_vec()),
            hash_algorithm: Some(1),
            crl: Some(self.crl(&self.tbs())),
        }
    }
    fn verify(&self, pending: Pending, response: proto::Response) -> io::Result<()> {
        pending
            .verify(
                envelope(response),
                &Authorities {
                    device: &[&self.root],
                    revocation: &[&self.crl_root],
                },
                self.now,
            )
            .map(|_| ())
    }
}
fn envelope(response: proto::Response) -> CastMessage {
    CastMessage {
        protocol_version: 0,
        source_id: "receiver-0".into(),
        destination_id: "sender-0".into(),
        namespace: NAMESPACE.into(),
        payload_type: 1,
        payload_utf8: None,
        payload_binary: Some(
            proto::DeviceAuth {
                response: Some(response),
                challenge: None,
                error: None,
            }
            .encode_to_vec(),
        ),
    }
}
fn rejected(fixture: &Fixture, change: impl FnOnce(&mut proto::Response)) {
    let pending = Pending::new(&fixture.tls_leaf).expect("nonce");
    let mut response = fixture.response(&pending);
    change(&mut response);
    assert!(
        fixture.verify(pending, response).is_err(),
        "adversarial response admitted"
    );
}
#[test]
fn valid_chain_nonce_proof_and_current_crl() {
    let fixture = Fixture::new();
    let pending = Pending::new(&fixture.tls_leaf).unwrap();
    let response = fixture.response(&pending);
    fixture
        .verify(pending, response)
        .expect("valid synthetic identity");
    let pending = Pending::new(&fixture.tls_leaf).unwrap();
    let mut response = fixture.response(&pending);
    response.signature_algorithm = None;
    fixture
        .verify(pending, response)
        .expect("wire PKCS1 default retains SHA256 proof");
}
#[test]
fn fresh_nonce_rejects_replay_and_tls_leaf_substitution() {
    let fixture = Fixture::new();
    let pending = Pending::new(&fixture.tls_leaf).unwrap();
    let response = fixture.response(&pending);
    fixture.verify(pending, response.clone()).unwrap();
    let next = Pending::new(&fixture.tls_leaf).unwrap();
    assert_ne!(
        response.sender_nonce.as_deref(),
        Some(next.nonce.as_slice())
    );
    assert!(fixture.verify(next, response.clone()).is_err());
    let another = Fixture::new();
    let mut pending = Pending::new(&another.tls_leaf).unwrap();
    pending
        .nonce
        .copy_from_slice(response.sender_nonce.as_ref().unwrap());
    assert!(
        fixture.verify(pending, response).is_err(),
        "proof cannot move between TLS connections"
    );
}
#[test]
fn missing_nonce_sha1_and_algorithm_substitution_rejected() {
    let fixture = Fixture::new();
    rejected(&fixture, |r| r.sender_nonce = None);
    rejected(&fixture, |r| r.sender_nonce = Some(vec![0; 16]));
    rejected(&fixture, |r| r.hash_algorithm = Some(0));
    rejected(&fixture, |r| r.hash_algorithm = None);
    rejected(&fixture, |r| r.signature_algorithm = Some(2));
    rejected(&fixture, |r| r.signature_algorithm = Some(999));
}
#[test]
fn wrong_proof_and_auth_certificate_key_rejected() {
    let fixture = Fixture::new();
    rejected(&fixture, |r| r.signature[0] ^= 1);
    rejected(&fixture, |r| {
        r.client_auth_certificate = fixture.crl_signer.clone();
    });
}
#[test]
fn untrusted_issuer_and_signature_failure_rejected() {
    let fixture = Fixture::new();
    let pending = Pending::new(&fixture.tls_leaf).unwrap();
    let response = fixture.response(&pending);
    assert!(
        pending
            .verify(
                envelope(response),
                &Authorities {
                    device: &[&fixture.crl_root],
                    revocation: &[&fixture.crl_root]
                },
                fixture.now
            )
            .is_err()
    );
    rejected(&fixture, |r| {
        let last = r.client_auth_certificate.len() - 1;
        r.client_auth_certificate[last] ^= 1;
    });
    rejected(&fixture, |r| r.intermediate_certificate.clear());
}
#[test]
fn certificate_times_and_four_day_tls_bound_rejected() {
    for part in [
        Part::Root,
        Part::Intermediate,
        Part::Device,
        Part::CrlRoot,
        Part::CrlSigner,
        Part::Tls,
    ] {
        for expired in [true, false] {
            let fixture = Fixture::with(|p, params| {
                if p == part {
                    if expired {
                        params.not_before -= time::Duration::days(3);
                        params.not_after -= time::Duration::days(3);
                    } else {
                        params.not_before += time::Duration::days(3);
                        params.not_after += time::Duration::days(3);
                    }
                }
            });
            rejected(&fixture, |_| {});
        }
    }
    let fixture = Fixture::with(|p, params| {
        if p == Part::Tls {
            params.not_after += time::Duration::days(3);
        }
    });
    rejected(&fixture, |_| {});
}
#[test]
fn key_usage_ca_path_and_critical_extensions_rejected() {
    for part in [
        Part::Device,
        Part::Intermediate,
        Part::Root,
        Part::CrlSigner,
    ] {
        let fixture = Fixture::with(|p, params| {
            if p == part {
                params.key_usages.clear();
            }
        });
        rejected(&fixture, |_| {});
    }
    let fixture = Fixture::with(|p, params| {
        if p == Part::Root {
            params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        }
    });
    rejected(&fixture, |_| {});
    let fixture = Fixture::with(|p, params| {
        if p == Part::Intermediate {
            params.is_ca = IsCa::NoCa;
        }
    });
    rejected(&fixture, |_| {});
    let fixture = Fixture::with(|p, params| {
        if p == Part::Device {
            let mut ext = CustomExtension::from_oid_content(&[1, 2, 3, 4], vec![5, 0]);
            ext.set_criticality(true);
            params.custom_extensions.push(ext);
        }
    });
    rejected(&fixture, |_| {});
}
#[test]
fn audio_only_on_leaf_intermediate_or_anchor_rejected() {
    for part in [Part::Device, Part::Intermediate, Part::Root] {
        let fixture = Fixture::with(|p, params| {
            if p == part {
                params
                    .custom_extensions
                    .push(CustomExtension::from_oid_content(
                        &[2, 5, 29, 32],
                        vec![
                            0x30, 14, 0x30, 12, 6, 10, 0x2b, 6, 1, 4, 1, 0xd6, 0x79, 2, 5, 2,
                        ],
                    ));
            }
        });
        rejected(&fixture, |_| {});
    }
}
#[test]
fn missing_stale_future_or_invalid_crl_rejected() {
    let fixture = Fixture::new();
    rejected(&fixture, |r| r.crl = None);
    rejected(&fixture, |r| r.crl = Some(vec![0xff]));
    for future in [false, true] {
        let mut tbs = fixture.tbs();
        if future {
            tbs.not_before_seconds = Some(fixture.now.as_secs() + 1);
        } else {
            tbs.not_after_seconds = Some(fixture.now.as_secs() - 1);
        }
        rejected(&fixture, |r| r.crl = Some(fixture.crl(&tbs)));
    }
    let mut bundle = proto::CrlBundle::decode(fixture.crl(&fixture.tbs()).as_slice()).unwrap();
    bundle.crls[0].signature.as_mut().unwrap()[0] ^= 1;
    rejected(&fixture, |r| r.crl = Some(bundle.encode_to_vec()));
    let mut tbs = fixture.tbs();
    tbs.version = Some(1);
    rejected(&fixture, |r| r.crl = Some(fixture.crl(&tbs)));
}
#[test]
fn revoked_spki_and_issuer_serial_ranges_rejected() {
    let fixture = Fixture::new();
    for der in [&fixture.device, &fixture.intermediate, &fixture.root] {
        let (_, cert) = cert_parser::parse_x509_certificate(der).unwrap();
        let mut tbs = fixture.tbs();
        tbs.revoked_public_key_hashes
            .push(Sha256::digest(cert.public_key().raw).to_vec());
        rejected(&fixture, |r| r.crl = Some(fixture.crl(&tbs)));
    }
    let (_, issuer) = cert_parser::parse_x509_certificate(&fixture.intermediate).unwrap();
    let mut tbs = fixture.tbs();
    tbs.revoked_serial_number_ranges.push(proto::SerialRange {
        issuer_public_key_hash: Some(Sha256::digest(issuer.public_key().raw).to_vec()),
        first_serial_number: Some(23),
        last_serial_number: Some(23),
    });
    rejected(&fixture, |r| r.crl = Some(fixture.crl(&tbs)));
}
#[test]
fn protocol_routes_shapes_and_bounds_rejected() {
    let fixture = Fixture::new();
    for case in 0..7 {
        let pending = Pending::new(&fixture.tls_leaf).unwrap();
        let mut message = envelope(fixture.response(&pending));
        match case {
            0 => message.namespace = "wrong".into(),
            1 => message.source_id = "sender-0".into(),
            2 => message.destination_id = "*".into(),
            3 => message.payload_type = 0,
            4 => message.payload_utf8 = Some("{}".into()),
            5 => message.payload_binary = None,
            _ => message.payload_binary = Some(vec![0xff]),
        }
        assert!(
            pending
                .verify(
                    message,
                    &Authorities {
                        device: &[&fixture.root],
                        revocation: &[&fixture.crl_root]
                    },
                    fixture.now
                )
                .is_err()
        );
    }
    rejected(&fixture, |r| {
        r.intermediate_certificate = vec![fixture.intermediate.clone(); MAX_CHAIN + 1];
    });
}
#[test]
fn public_authority_der_pins() {
    for (der, pin) in [
        (
            CAST_ROOT,
            "809af14700b3fe2611ad597eb1584d6354313b64ccdb3390f097fa3e5826d6ca",
        ),
        (
            EUREKA_ROOT,
            "caf6d1e37b532203e01a76bf07187bb731ccd38801565ab2211a2c0ab7f3bc46",
        ),
        (
            CRL_ROOT,
            "8b77c60291f9d62764613083f49750b1c5caac2d9efbe5a28edf301f9d84831d",
        ),
    ] {
        let hash = Sha256::digest(der)
            .iter()
            .fold(String::new(), |mut output, byte| {
                use std::fmt::Write as _;
                write!(output, "{byte:02x}").unwrap();
                output
            });
        assert_eq!(hash, pin);
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Wire {
    Valid,
    WrongProof,
    Stalled,
    Disconnect,
    Oversize,
    Malformed,
    ReceiverError,
    Cancel,
    Closed,
    CancelWithProof,
    ClosedWithProof,
}
async fn wire_receiver(
    acceptor: tls_runtime::TlsAcceptor,
    server: tokio::io::DuplexStream,
    fixture: Fixture,
    case: Wire,
    production_roots: bool,
    mut cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let mut stream = acceptor.accept(server).await.unwrap();
    let size = stream.read_u32().await.unwrap() as usize;
    assert!(size <= MAX_FRAME);
    let mut frame = vec![0; size];
    stream.read_exact(&mut frame).await.unwrap();
    let challenge = CastMessage::decode(frame.as_slice()).unwrap();
    assert_eq!(challenge.namespace, NAMESPACE);
    assert_eq!(challenge.source_id, "sender-0");
    assert_eq!(challenge.destination_id, "receiver-0");
    assert_eq!(challenge.payload_type, 1);
    assert!(challenge.payload_utf8.is_none());
    let auth = proto::DeviceAuth::decode(challenge.payload_binary.unwrap().as_slice()).unwrap();
    assert!(auth.response.is_none());
    assert!(auth.error.is_none());
    let challenge = auth.challenge.unwrap();
    assert_eq!(challenge.hash_algorithm, Some(1));
    assert_eq!(challenge.signature_algorithm, Some(1));
    let nonce = challenge.sender_nonce.unwrap();
    assert_eq!(nonce.len(), 16);
    if case == Wire::Cancel {
        cancel_tx.take().unwrap().send(()).unwrap();
    } else if case == Wire::Closed {
        drop(cancel_tx.take());
    }
    if case == Wire::Disconnect {
        return;
    }
    if case == Wire::Oversize {
        stream
            .write_all(&u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes())
            .await
            .unwrap();
    } else if !matches!(case, Wire::Stalled | Wire::Cancel | Wire::Closed) {
        let pending = Pending {
            nonce: nonce.try_into().unwrap(),
            tls_leaf: fixture.tls_leaf.clone(),
        };
        let mut response = fixture.response(&pending);
        if case == Wire::WrongProof {
            response.signature[0] ^= 1;
        }
        let mut message = envelope(response);
        if case == Wire::ReceiverError {
            message.payload_binary = Some(
                proto::DeviceAuth {
                    challenge: None,
                    response: None,
                    error: Some(proto::AuthError { error_type: 1 }),
                }
                .encode_to_vec(),
            );
        }
        let frame = if case == Wire::Malformed {
            vec![0xff]
        } else {
            message.encode_to_vec()
        };
        stream
            .write_all(&u32::try_from(frame.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        stream.write_all(&frame).await.unwrap();
        stream.flush().await.unwrap();
    }
    if case == Wire::CancelWithProof {
        cancel_tx.take().unwrap().send(()).unwrap();
    } else if case == Wire::ClosedWithProof {
        drop(cancel_tx.take());
    }
    if case == Wire::Valid && !production_roots {
        let size = stream.read_u32().await.unwrap() as usize;
        let mut frame = vec![0; size];
        stream.read_exact(&mut frame).await.unwrap();
        let message = CastMessage::decode(frame.as_slice()).unwrap();
        assert_eq!(message.namespace, protocol::CONNECTION_NAMESPACE);
        assert_eq!(
            message.payload_utf8.as_deref(),
            Some("{\"type\":\"CONNECT\"}")
        );
    } else {
        let result = tokio::time::timeout(Duration::from_secs(2), stream.read_u8())
            .await
            .expect("failed authentication closes the same transport");
        assert!(
            result.is_err(),
            "no control or media byte after authentication failure, cancellation or timeout"
        );
    }
}
async fn wire(case: Wire, production_roots: bool) {
    use tls_runtime::rustls;
    let fixture = Fixture::new();
    let device_root = fixture.root.clone();
    let crl_root = fixture.crl_root.clone();
    let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![pki::CertificateDer::from(fixture.tls_leaf.clone())],
        pki::PrivateKeyDer::Pkcs8(pki::PrivatePkcs8KeyDer::from(
            fixture.tls_key.serialize_der(),
        )),
    )
    .unwrap();
    let acceptor = tls_runtime::TlsAcceptor::from(std::sync::Arc::new(config));
    let (client, server) = tokio::io::duplex(128 * 1024);
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let cancel_tx = Some(cancel_tx);
    let server = tokio::spawn(async move {
        wire_receiver(acceptor, server, fixture, case, production_roots, cancel_tx).await;
    });
    let stream = tls::connect(
        &tls::connector().unwrap(),
        pki::ServerName::try_from("isolated-cast-fixture").unwrap(),
        client,
        std::future::pending(),
        Duration::from_secs(5),
    )
    .await
    .unwrap()
    .unwrap();
    let cancelled = async move {
        if matches!(
            case,
            Wire::Cancel | Wire::Closed | Wire::CancelWithProof | Wire::ClosedWithProof
        ) {
            let _ = cancel_rx.await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let deadline = if case == Wire::Stalled {
        Duration::from_millis(30)
    } else {
        Duration::from_secs(5)
    };
    let result = if production_roots {
        authenticate(stream, cancelled, deadline).await
    } else {
        authenticate_with(
            stream,
            cancelled,
            deadline,
            &Authorities {
                device: &[&device_root],
                revocation: &[&crl_root],
            },
        )
        .await
    };
    match case {
        Wire::Valid if !production_roots => {
            let (mut stream, window) = result.unwrap().unwrap();
            assert!(window.expiry.as_secs() > UnixTime::now().as_secs());
            let message = CastMessage {
                protocol_version: 0,
                source_id: "sender-0".into(),
                destination_id: "receiver-0".into(),
                namespace: protocol::CONNECTION_NAMESPACE.into(),
                payload_type: 0,
                payload_utf8: Some("{\"type\":\"CONNECT\"}".into()),
                payload_binary: None,
            };
            let frame = message.encode_to_vec();
            stream
                .write_all(&u32::try_from(frame.len()).unwrap().to_be_bytes())
                .await
                .unwrap();
            stream.write_all(&frame).await.unwrap();
        }
        Wire::Cancel | Wire::Closed | Wire::CancelWithProof | Wire::ClosedWithProof => {
            assert!(result.unwrap().is_none());
        }
        Wire::Stalled => assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut),
        _ => assert!(
            result.is_err(),
            "unverified identity must not return a controller transport"
        ),
    }
    server.await.expect("independent receiver fixture");
}
#[tokio::test]
async fn same_connection_challenge_precedes_control_and_valid_proof_admits_it() {
    wire(Wire::Valid, false).await;
}
#[tokio::test]
async fn failed_authentication_sends_no_control_bytes() {
    for case in [
        Wire::WrongProof,
        Wire::Oversize,
        Wire::Malformed,
        Wire::ReceiverError,
    ] {
        wire(case, false).await;
    }
}
#[tokio::test]
async fn auth_timeout_cancel_closed_channel_and_disconnect_close_transport() {
    for case in [Wire::Stalled, Wire::Cancel, Wire::Closed, Wire::Disconnect] {
        wire(case, false).await;
    }
}
#[tokio::test]
async fn production_authorities_never_admit_synthetic_roots() {
    wire(Wire::Valid, true).await;
}

#[test]
fn name_constraints_and_wrong_extended_usage_rejected() {
    let fixture = Fixture::with(|part, params| {
        if part == Part::Intermediate {
            params.name_constraints = Some(certificates::NameConstraints {
                permitted_subtrees: vec![certificates::GeneralSubtree::DnsName(
                    "allowed.invalid".into(),
                )],
                excluded_subtrees: vec![],
            });
        }
    });
    rejected(&fixture, |_| {});
    let fixture = Fixture::with(|part, params| {
        if part == Part::Device {
            params.extended_key_usages = vec![certificates::ExtendedKeyUsagePurpose::CodeSigning];
        }
    });
    rejected(&fixture, |_| {});
}
#[test]
fn crl_authority_is_separate_and_malformed_signed_lists_rejected() {
    let fixture = Fixture::new();
    let pending = Pending::new(&fixture.tls_leaf).unwrap();
    let response = fixture.response(&pending);
    assert!(
        pending
            .verify(
                envelope(response),
                &Authorities {
                    device: &[&fixture.root],
                    revocation: &[&fixture.root]
                },
                fixture.now
            )
            .is_err()
    );
    for case in 0..6 {
        let mut tbs = fixture.tbs();
        match case {
            0 => tbs.not_before_seconds = None,
            1 => tbs.not_after_seconds = None,
            2 => tbs.revoked_public_key_hashes.push(vec![0; 31]),
            _ => tbs.revoked_serial_number_ranges.push(proto::SerialRange {
                issuer_public_key_hash: Some(vec![0; if case == 3 { 31 } else { 32 }]),
                first_serial_number: if case == 4 { None } else { Some(9) },
                last_serial_number: Some(8),
            }),
        }
        rejected(&fixture, |r| r.crl = Some(fixture.crl(&tbs)));
    }
    let mut bundle = proto::CrlBundle::decode(fixture.crl(&fixture.tbs()).as_slice()).unwrap();
    bundle.crls.push(bundle.crls[0].clone());
    rejected(&fixture, |r| r.crl = Some(bundle.encode_to_vec()));
}
#[test]
fn wrong_signer_with_a_valid_device_path_and_sha1_chain_marker_rejected() {
    let fixture = Fixture::new();
    rejected(&fixture, |response| {
        let mut data = response.sender_nonce.clone().unwrap();
        data.extend_from_slice(&fixture.tls_leaf);
        response.signature = sign(4, &data);
    });
    rejected(&fixture, |response| {
        let needle = [6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 11];
        let der = &mut response.client_auth_certificate;
        let mut changed = 0;
        for index in 0..=der.len().saturating_sub(needle.len()) {
            if der[index..index + needle.len()] == needle {
                der[index + needle.len() - 1] = 5;
                changed += 1;
            }
        }
        assert_eq!(
            changed, 2,
            "both SHA256 signature algorithm identifiers in the DER were located"
        );
    });
}
#[test]
fn weak_rsa_device_key_rejected() {
    use rsa_crypto::pkcs1::EncodeRsaPublicKey as _;
    struct Public(Vec<u8>);
    impl certificates::PublicKeyData for Public {
        fn der_bytes(&self) -> &[u8] {
            &self.0
        }
        fn algorithm(&self) -> &'static certificates::SignatureAlgorithm {
            &certificates::PKCS_RSA_SHA256
        }
    }
    let fixture = Fixture::new();
    let weak = RsaPrivateKey::new(&mut auth_rng::UnwrapErr(auth_random::SysRng), 1024).unwrap();
    let public = Public(
        weak.to_public_key()
            .to_pkcs1_der()
            .unwrap()
            .as_bytes()
            .to_vec(),
    );
    let leaf = params(Part::Device, fixture.now)
        .signed_by(
            &public,
            &Issuer::from_params(&params(Part::Intermediate, fixture.now), &key(1)),
        )
        .unwrap()
        .der()
        .to_vec();
    let pending = Pending::new(&fixture.tls_leaf).unwrap();
    let mut response = fixture.response(&pending);
    response.client_auth_certificate = leaf;
    let mut data = pending.nonce.to_vec();
    data.extend_from_slice(&pending.tls_leaf);
    response.signature = rsa_crypto::pkcs1v15::SigningKey::<Sha256>::new(weak)
        .try_sign(&data)
        .unwrap()
        .to_vec();
    let error = fixture.verify(pending, response).unwrap_err();
    assert!(
        error.to_string().contains("Cast RSA key size"),
        "weak key must fail its policy check: {error}"
    );
}

#[test]
fn authentication_window_is_bounded_by_the_authenticated_crl() {
    let fixture = Fixture::new();
    let pending = Pending::new(&fixture.tls_leaf).unwrap();
    let response = fixture.response(&pending);
    let expiry = pending
        .verify(
            envelope(response),
            &Authorities {
                device: &[&fixture.root],
                revocation: &[&fixture.crl_root],
            },
            fixture.now,
        )
        .unwrap();
    assert_eq!(expiry.as_secs(), fixture.tbs().not_after_seconds.unwrap());
}
#[tokio::test]
async fn expired_credentials_emit_no_control_or_media_url_and_release_waiters() {
    for clock_expired in [true, false] {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        let window = Window {
            expiry: UnixTime::since_unix_epoch(Duration::from_secs(if clock_expired {
                0
            } else {
                UnixTime::now().as_secs() + 100
            })),
            deadline: tokio::time::Instant::now(),
        };
        assert!(
            window
                .write_frame(&mut writer, b"prefix", b"secret-media-url")
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_millis(50), window.expired())
            .await
            .expect("expired session wakes the controller");
        drop(writer);
        assert!(
            reader.read_u8().await.is_err(),
            "no frame byte after UTC or monotonic expiry"
        );
    }
}

#[tokio::test]
async fn cancellation_or_channel_close_wins_over_an_already_available_valid_proof() {
    wire(Wire::CancelWithProof, false).await;
    wire(Wire::ClosedWithProof, false).await;
}
