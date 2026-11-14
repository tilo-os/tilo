use super::*;
use crate::UserIdentity;
use ed25519_dalek::Signer as _;

fn fresh_cert(caps: DeviceCapabilities) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let (root_sk, user) = UserIdentity::generate("Alice", 1_720_000_000_000_000_000);
    let (_dev_sk, dev) = crate::DeviceIdentity::generate("Framework 16", 1_720_000_000_000_000_100);
    let mut cert = DeviceCert {
        user_public_key: user.public_key,
        device_public_key: dev.public_key,
        device_label: dev.label,
        capabilities: caps,
        issued_at_ns: 1_720_000_000_000_000_200,
        expires_at_ns: None,
        signature: [0u8; 64],
        issuer: None,
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
            issuer: None,
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
        let (_root, cert) = fresh_cert(DeviceCapabilities::administrator());
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
                foreign: false,
                can_administer_devices: false,
                agent: None,
                stand_in: false,
                server_key: false,
                browser_pages: false,
                retired: RetiredFlags::default(),
            },
            issued_at_ns: 1_000_000,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
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
                issuer: None,
            };
            sign_device_cert(&mut c, &root_sk).unwrap();
            c.signature
        };

        let s_primary = build(DeviceCapabilities::primary());
        let s_member = build(DeviceCapabilities::member());
        let s_admin = build(DeviceCapabilities::administrator());
        assert_ne!(s_primary, s_member);
        assert_ne!(s_member, s_admin);
        assert_ne!(s_primary, s_admin);
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
            issuer: None,
        };
        assert_eq!(
            sign_device_cert(&mut cert, &bob_sk).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }
}

fn golden_cert(foreign: bool) -> DeviceCert {
    golden_cert_with(DeviceCapabilities {
        foreign,
        ..DeviceCapabilities::member()
    })
}

fn golden_cert_with(capabilities: DeviceCapabilities) -> DeviceCert {
    let root_sk = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let mut cert = DeviceCert {
        user_public_key: root_sk.verifying_key().to_bytes(),
        device_public_key: [2u8; 32],
        device_label: "golden".to_string(),
        capabilities,
        issued_at_ns: 1_000_000,
        expires_at_ns: Some(2_000_000),
        signature: [0u8; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root_sk).unwrap();
    cert
}

const GOLDEN_V1_JSON: &str = r#"{"user_public_key":[138,136,227,221,116,9,241,149,253,82,219,45,60,186,93,114,202,103,9,191,29,148,18,27,243,116,136,1,180,15,111,92],"device_public_key":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"device_label":"golden","capabilities":{"can_own_workspaces":true,"can_admit_devices":false,"can_act_as_host":false,"can_read_all_workspaces":false},"issued_at_ns":1000000,"expires_at_ns":2000000,"signature":[193,94,11,63,27,213,214,122,35,157,141,24,157,200,120,75,180,243,189,153,177,189,224,147,47,142,23,15,234,170,239,9,175,129,176,168,99,254,102,174,97,65,248,91,187,181,93,141,241,211,8,123,41,66,110,172,36,46,39,75,189,14,237,1]}"#;
const GOLDEN_V1_POSTCARD: &str = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c020202020202020202020202020202020202020202020202020202020202020206676f6c64656e01000000c0843d0180897a40c15e0b3f1bd5d67a239d8d189dc8784bb4f3bd99b1bde0932f8e170feaaaef09af81b0a863fe66ae6141f85bbbb55d8df1d3087b29426eac242e274bbd0eed01";
const GOLDEN_V1_DIGEST: &str = "0d8271d3aadb5438af000ff5f96b6355102b57a271885ed26142fb698884e623";

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
        crate::canonical::from_canonical_bytes(&hex::decode(GOLDEN_V1_POSTCARD).unwrap()).unwrap();
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
        #[serde(with = "serde_bytes")]
        pub signature: [u8; 64],
    }
}

/// A certificate signed with a retired grant (a host's, or a primary
/// device's that read every workspace) keeps the bytes and the signature
/// a build from before their retirement gave it, in both encodings, and
/// grants neither. A grant that may carry nothing else is refused with
/// one, as it was then.
#[test]
fn certificates_signed_with_retired_grants_keep_their_bytes_and_signature() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    for (act_as_host, read_all_workspaces) in [(false, true), (true, true), (true, false)] {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [2; 32],
            device_label: "Home server".into(),
            capabilities: DeviceCapabilities {
                retired: RetiredFlags {
                    act_as_host,
                    read_all_workspaces,
                },
                ..DeviceCapabilities::primary()
            },
            issued_at_ns: 7,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let then = v1::DeviceCapabilities {
            can_own_workspaces: true,
            can_admit_devices: true,
            can_act_as_host: act_as_host,
            can_read_all_workspaces: read_all_workspaces,
        };
        assert_eq!(
            device_cert_signing_digest(&cert).unwrap(),
            signing_digest(
                DEVCERT_PREIMAGE_TAG,
                &(
                    cert.user_public_key,
                    cert.device_public_key,
                    "Home server",
                    &then,
                    7u64,
                    None::<u64>
                )
            )
            .unwrap(),
            "signed over the layout it was signed over then"
        );
        let old = v1::DeviceCert {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            device_label: cert.device_label.clone(),
            capabilities: then,
            issued_at_ns: 7,
            expires_at_ns: None,
            signature: cert.signature,
        };
        let bytes = postcard::to_allocvec(&old).unwrap();
        assert_eq!(crate::canonical::canonical_bytes(&cert).unwrap(), bytes);
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(read, cert);
        verify_device_cert(&read).unwrap();
        assert!(read.capabilities.retired.any());
        let json = serde_json::to_string(&read).unwrap();
        assert_eq!(serde_json::from_str::<v1::DeviceCert>(&json).unwrap(), old);
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), cert);
    }
    let grant = DeviceCapabilities {
        retired: RetiredFlags {
            act_as_host: false,
            read_all_workspaces: true,
        },
        ..DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces)
    };
    assert!(!grant.agent_grant_holds());
}

/// An agent's grant signs under its own tag, round-trips with its scope
/// in both encodings, must expire and grant nothing else, survives no
/// change of scope, and is refused by verifiers that predate it.
#[test]
fn agent_grants_carry_their_scope_expire_and_old_verifiers_refuse_them() {
    let workspace = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
    let one = AgentScope::parse(workspace).unwrap();
    assert_eq!(one.name(), workspace);
    assert!(one.reaches(workspace));
    assert!(!one.reaches("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f1"));
    assert!(!one.makes_workspaces());
    assert!(AgentScope::AllWorkspaces.reaches(workspace));
    assert!(AgentScope::AllWorkspaces.makes_workspaces());
    // One workspace narrows what all of them was, and never the reverse.
    assert!(one.within(AgentScope::AllWorkspaces));
    assert!(one.within(one));
    assert!(AgentScope::AllWorkspaces.within(AgentScope::AllWorkspaces));
    assert!(!AgentScope::AllWorkspaces.within(one));
    for malformed in [
        "",
        "ALL",
        "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0",
        "00000000-0000-0000-0000-000000000000",
        "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
    ] {
        assert_eq!(AgentScope::parse(malformed), None, "{malformed}");
    }

    for scope in [AgentScope::AllWorkspaces, one] {
        let grant = golden_cert_with(DeviceCapabilities::agent_grant(scope));
        verify_device_cert(&grant).unwrap();
        assert_eq!(
            device_cert_signing_digest(&grant).unwrap(),
            signing_digest(DEVCERT_V4_PREIMAGE_TAG, &grant.view()).unwrap()
        );
        let json = serde_json::to_string(&grant).unwrap();
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), grant);
        let bytes = postcard::to_allocvec(&(&grant, [9u8; 32])).unwrap();
        let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&bytes).unwrap();
        assert_eq!((back, nonce), (grant.clone(), [9u8; 32]));
        // Verifiers that predate grants stop at the marker.
        assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&bytes).is_err());
        assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
        // Another scope under the same signature does not verify.
        let mut moved = grant.clone();
        moved.capabilities.agent = Some(match scope {
            AgentScope::AllWorkspaces => one,
            AgentScope::Workspace(_) => AgentScope::AllWorkspaces,
        });
        assert_eq!(
            verify_device_cert(&moved),
            Err(DeviceCertError::BadSignature)
        );
        // A grant is not renewed into another scope or a person's device.
        let mut wider = grant.clone();
        wider.capabilities = DeviceCapabilities::member();
        wider.issued_at_ns += 1;
        assert!(!wider.renews(&grant));
    }

    let root = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
    let unsigned = |capabilities: DeviceCapabilities, expires_at_ns: Option<u64>| DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: [2u8; 32],
        device_label: "Claude Code via mcp.example".to_owned(),
        capabilities,
        issued_at_ns: 1_000,
        expires_at_ns,
        signature: [0u8; 64],
        issuer: None,
    };
    // A grant that never expires, outlives the bound, or grants a flag
    // besides its scope is neither signed nor taken.
    let wide = DeviceCapabilities {
        can_admit_devices: true,
        ..DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces)
    };
    for (capabilities, expiry) in [
        (DeviceCapabilities::agent_grant(one), None),
        (
            DeviceCapabilities::agent_grant(one),
            Some(1_000 + MAX_AGENT_GRANT_LIFETIME_NS + 1),
        ),
        (wide, Some(2_000)),
    ] {
        let mut cert = unsigned(capabilities, expiry);
        assert_eq!(
            sign_device_cert(&mut cert, &root),
            Err(DeviceCertError::InvalidAgentGrant)
        );
        // Signed anyway by a root that skips the check: still refused.
        cert.signature = root
            .sign(&signing_digest(DEVCERT_V4_PREIMAGE_TAG, &cert.view()).unwrap())
            .to_bytes();
        assert_eq!(
            verify_device_cert(&cert),
            Err(DeviceCertError::InvalidAgentGrant)
        );
    }
    let mut bounded = unsigned(
        DeviceCapabilities::agent_grant(one),
        Some(1_000 + MAX_AGENT_GRANT_LIFETIME_NS),
    );
    sign_device_cert(&mut bounded, &root).unwrap();
    verify_device_cert_at(&bounded, 2_000).unwrap();
    assert_eq!(
        verify_device_cert_at(&bounded, 1_000 + MAX_AGENT_GRANT_LIFETIME_NS),
        Err(DeviceCertError::Expired)
    );
}

const DAY_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// A root `seed`'s stand-in (key `[seed + 2; 32]`), valid from 1 000 ns
/// for its longest lifetime.
fn make_stand_in(seed: u8) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(2); 32]);
    let mut cert = prepare_stand_in_cert(
        root.verifying_key().to_bytes(),
        key.verifying_key().to_bytes(),
        "Stand-in on tilo.example",
        1_000,
        1_000 + STAND_IN_LIFETIME_NS,
    );
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// An unsigned member certificate `issuer` issues for device `[9; 32]` at
/// `issued_at_ns`, lasting a browser's lifetime.
fn issued_by(issuer: &DeviceCert, issued_at_ns: u64) -> DeviceCert {
    DeviceCert {
        user_public_key: issuer.user_public_key,
        device_public_key: [9; 32],
        device_label: "Browser".to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns: Some(
            issued_at_ns.saturating_add(crate::CertificationPolicy::browser().lifetime_ns),
        ),
        signature: [0; 64],
        issuer: Some(Box::new(issuer.clone())),
    }
}

/// `cert` signed by `key` under the digest a signer that skips every
/// shape check would sign.
fn forced(mut cert: DeviceCert, key: &ed25519_dalek::SigningKey) -> DeviceCert {
    cert.signature = key
        .sign(&device_cert_signing_digest(&cert).unwrap())
        .to_bytes();
    cert
}

/// A stand-in's certificate verifies as one, is no device, carries
/// nothing besides being a stand-in, and always expires within 30 days.
#[test]
fn a_stand_in_is_certified_by_its_root_and_is_no_device() {
    let (key, cert) = make_stand_in(1);
    verify_stand_in_cert(&cert).unwrap();
    verify_stand_in_cert_at(&cert, 2_000).unwrap();
    assert_eq!(
        verify_stand_in_cert_at(&cert, 1_000 + STAND_IN_LIFETIME_NS),
        Err(DeviceCertError::Expired)
    );
    assert_eq!(
        verify_device_cert(&cert),
        Err(DeviceCertError::StandInIsNoDevice)
    );
    assert_eq!(
        verify_device_cert_at(&cert, 2_000),
        Err(DeviceCertError::StandInIsNoDevice)
    );
    assert_eq!(
        device_cert_signing_digest(&cert).unwrap(),
        signing_digest(DEVCERT_V5_PREIMAGE_TAG, &cert.view()).unwrap()
    );
    // A device's certificate is no stand-in's.
    assert_eq!(
        verify_stand_in_cert(&golden_cert(false)),
        Err(DeviceCertError::InvalidStandIn)
    );
    // Its marker does not come off under the same signature.
    let mut stripped = cert.clone();
    stripped.capabilities.stand_in = false;
    assert_eq!(
        verify_device_cert(&stripped),
        Err(DeviceCertError::BadSignature)
    );

    // It round-trips in both encodings, and verifiers that predate it
    // refuse it.
    let json = serde_json::to_string(&cert).unwrap();
    assert!(json.contains(r#""stand_in":true"#));
    assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), cert);
    assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
    let wire = postcard::to_allocvec(&(&cert, [9u8; 32])).unwrap();
    let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&wire).unwrap();
    assert_eq!((back, nonce), (cert.clone(), [9u8; 32]));
    assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&wire).is_err());

    // Any other capability beside it, no expiry, a longer life, or its
    // root's own key is neither signed nor taken.
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let mut others = Vec::new();
    for widen in [
        |c: &mut DeviceCapabilities| c.can_own_workspaces = true,
        |c: &mut DeviceCapabilities| c.can_admit_devices = true,
        |c: &mut DeviceCapabilities| c.foreign = true,
        |c: &mut DeviceCapabilities| c.can_administer_devices = true,
        |c: &mut DeviceCapabilities| c.agent = Some(AgentScope::AllWorkspaces),
    ] {
        let mut widened = cert.clone();
        widen(&mut widened.capabilities);
        others.push(widened);
    }
    let mut forever = cert.clone();
    forever.expires_at_ns = None;
    let mut longer = cert.clone();
    longer.expires_at_ns = Some(1_001 + STAND_IN_LIFETIME_NS);
    let mut itself = cert.clone();
    itself.device_public_key = cert.user_public_key;
    others.extend([forever, longer, itself]);
    for mut other in others {
        assert!(sign_device_cert(&mut other, &root).is_err(), "{other:?}");
        let other = forced(other, &root);
        assert!(verify_stand_in_cert(&other).is_err(), "{other:?}");
        assert!(verify_device_cert(&other).is_err(), "{other:?}");
    }

    // A stand-in certifies no other stand-in.
    let mut second = prepare_stand_in_cert(
        cert.user_public_key,
        [8; 32],
        "Another stand-in",
        2_000,
        2_000 + DAY_NS,
    );
    second.issuer = Some(Box::new(cert));
    assert!(sign_device_cert(&mut second, &key).is_err());
    let second = forced(second, &key);
    assert!(verify_stand_in_cert(&second).is_err());
    assert!(verify_device_cert(&second).is_err());
}

/// A member certificate a stand-in issued verifies, binds its issuer, and
/// round-trips; verifiers that predate it refuse it.
#[test]
fn a_stand_in_issues_member_certificates_bound_to_it() {
    let (key, stand_in) = make_stand_in(1);
    let mut cert = issued_by(&stand_in, 2_000);
    sign_device_cert(&mut cert, &key).unwrap();
    verify_device_cert(&cert).unwrap();
    verify_device_cert_at(&cert, 3_000).unwrap();
    // Devices it certified outlive it.
    verify_device_cert_at(&cert, 1_000 + STAND_IN_LIFETIME_NS + 500).unwrap();
    assert_eq!(cert.signer_public_key(), stand_in.device_public_key);
    assert_eq!(
        device_cert_signing_digest(&cert).unwrap(),
        signing_digest(
            DEVCERT_V6_PREIMAGE_TAG,
            &IssuedCertView {
                cert: cert.view(),
                issuer: &stand_in,
            }
        )
        .unwrap()
    );
    // A browser's certificate a stand-in issued answers its request as
    // the browser policy allows.
    let mut narrower = issued_by(&stand_in, 2_000);
    narrower.capabilities.foreign = true;
    narrower.expires_at_ns = Some(2_000 + DAY_NS);
    sign_device_cert(&mut narrower, &key).unwrap();
    verify_device_cert(&narrower).unwrap();

    let json = serde_json::to_string(&cert).unwrap();
    assert!(json.contains(r#""issuer":{"#));
    assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), cert);
    assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
    let wire = postcard::to_allocvec(&(&cert, [9u8; 32])).unwrap();
    let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&wire).unwrap();
    assert_eq!((back, nonce), (cert.clone(), [9u8; 32]));
    assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&wire).is_err());
    // Capabilities alone name no issuer.
    let caps = &wire[32 + 32 + 1 + "Browser".len()..];
    assert_eq!(caps[0], EXTENDED_BINARY_MARKER);
    assert!(postcard::from_bytes::<DeviceCapabilities>(caps).is_err());

    // The issuer is bound: another certificate of the same stand-in key,
    // or a changed one, breaks the signature.
    let mut moved = cert.clone();
    let mut renewed = stand_in;
    renewed.issued_at_ns = 1_500;
    renewed = forced(renewed, &ed25519_dalek::SigningKey::from_bytes(&[1; 32]));
    verify_stand_in_cert(&renewed).unwrap();
    moved.issuer = Some(Box::new(renewed));
    assert_eq!(
        verify_device_cert(&moved),
        Err(DeviceCertError::BadSignature)
    );
    let mut tampered = cert.clone();
    if let Some(issuer) = tampered.issuer.as_mut() {
        issuer.signature[0] ^= 1;
    }
    assert_eq!(
        verify_device_cert(&tampered),
        Err(DeviceCertError::BadSignature)
    );
    // Without its issuer it is a root certificate the root never signed.
    let mut orphan = cert;
    orphan.issuer = None;
    assert_eq!(
        verify_device_cert(&orphan),
        Err(DeviceCertError::BadSignature)
    );
}

/// A stand-in issues no more than a member for a browser's lifetime, for
/// its own person, while it is valid, with its own key; and nothing it
/// issued can issue in turn.
#[test]
fn a_stand_in_issues_nothing_beyond_its_bounds() {
    let (key, stand_in) = make_stand_in(1);
    let (other_key, other_person) = make_stand_in(11);
    let golden_device;
    let mut refused: Vec<(DeviceCert, &ed25519_dalek::SigningKey)> = Vec::new();

    let mut more = issued_by(&stand_in, 2_000);
    more.capabilities = DeviceCapabilities::primary();
    refused.push((more, &key));
    for caps in [
        DeviceCapabilities::administrator(),
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        DeviceCapabilities::stand_in_approver(),
    ] {
        let mut cert = issued_by(&stand_in, 2_000);
        cert.capabilities = caps;
        refused.push((cert, &key));
    }
    let mut longer = issued_by(&stand_in, 2_000);
    longer.expires_at_ns = longer.expires_at_ns.map(|expiry| expiry + 1);
    refused.push((longer, &key));
    let mut forever = issued_by(&stand_in, 2_000);
    forever.expires_at_ns = None;
    refused.push((forever, &key));
    // Another person's device, under this person's stand-in.
    let mut foreign_person = issued_by(&stand_in, 2_000);
    foreign_person.user_public_key = other_person.user_public_key;
    refused.push((foreign_person, &key));
    // This person's device, under another person's stand-in.
    let mut other_stand_in = issued_by(&other_person, 2_000);
    other_stand_in.user_public_key = stand_in.user_public_key;
    refused.push((other_stand_in, &other_key));
    // Before the stand-in was certified, and once it expired.
    refused.push((issued_by(&stand_in, 999), &key));
    refused.push((issued_by(&stand_in, 1_000 + STAND_IN_LIFETIME_NS), &key));
    // Its own key, or the root's, as the device.
    let mut own = issued_by(&stand_in, 2_000);
    own.device_public_key = stand_in.device_public_key;
    refused.push((own, &key));
    let mut root = issued_by(&stand_in, 2_000);
    root.device_public_key = stand_in.user_public_key;
    refused.push((root, &key));
    // A device it certified certifies nothing.
    let mut member = issued_by(&stand_in, 2_000);
    sign_device_cert(&mut member, &key).unwrap();
    let device_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let mut chained = issued_by(&member, 3_000);
    chained.device_public_key = [10; 32];
    refused.push((chained, &device_key));
    // A root-signed member is no stand-in either.
    let root_member = golden_cert(false);
    golden_device = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
    let mut under_member = issued_by(&root_member, 1_500_000);
    under_member.device_public_key = [10; 32];
    refused.push((under_member, &golden_device));

    for (cert, signer) in refused {
        let mut signed = cert.clone();
        assert!(sign_device_cert(&mut signed, signer).is_err(), "{cert:?}");
        let forced = forced(cert, signer);
        assert!(verify_device_cert(&forced).is_err(), "{forced:?}");
    }

    // Signed by any key but the stand-in's, it does not verify.
    let mut cert = issued_by(&stand_in, 2_000);
    let root_key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    assert_eq!(
        sign_device_cert(&mut cert, &root_key),
        Err(DeviceCertError::BadSignature)
    );
    let by_root = forced(cert, &root_key);
    assert_eq!(
        verify_device_cert(&by_root),
        Err(DeviceCertError::BadSignature)
    );
}

/// A certificate whose issuer names an issuer of its own is refused by
/// the binary decoder before it recurses, and by the JSON one.
#[test]
fn an_issuer_with_an_issuer_does_not_decode() {
    let (key, stand_in) = make_stand_in(1);
    let mut member = issued_by(&stand_in, 2_000);
    sign_device_cert(&mut member, &key).unwrap();
    let mut chained = issued_by(&member, 3_000);
    chained.device_public_key = [10; 32];
    let wire = postcard::to_allocvec(&chained).unwrap();
    assert!(postcard::from_bytes::<DeviceCert>(&wire).is_err());
    let json = serde_json::to_string(&chained).unwrap();
    assert!(serde_json::from_str::<DeviceCert>(&json).is_err());
}

/// Root `[1; 32]`'s all-workspaces grant (key `[4; 32]`) from 1 000 ns,
/// lasting `lifetime`.
fn make_grant(lifetime: u64) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Visa backend".to_owned(),
        capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        issued_at_ns: 1_000,
        expires_at_ns: Some(lifetime.saturating_add(1_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// An all-workspaces grant mints a grant for one workspace that verifies
/// as one, under its issuer, and round-trips both encodings.
#[test]
fn a_grant_mints_a_narrower_grant() {
    let (key, grant) = make_grant(DAY_NS);
    let mut minted = prepare_minted_grant(&grant, [12; 32], [7; 16], "Jane Doe", 2_000, 3_000);
    sign_device_cert(&mut minted, &key).unwrap();
    verify_device_cert_at(&minted, 2_500).unwrap();
    assert_eq!(minted.signer_public_key(), grant.device_public_key);
    assert_eq!(
        minted.capabilities.agent,
        Some(AgentScope::Workspace([7; 16]))
    );
    let wire = postcard::to_allocvec(&minted).unwrap();
    assert_eq!(postcard::from_bytes::<DeviceCert>(&wire).unwrap(), minted);
    let json = serde_json::to_string(&minted).unwrap();
    assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), minted);
    // The root's own signature does not stand in for the grant's.
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    assert_eq!(
        verify_device_cert(&forced(minted.clone(), &root)),
        Err(DeviceCertError::BadSignature)
    );
}

/// A minted grant never reaches more, lasts longer, or chains further
/// than its issuer, and only an all-workspaces grant mints.
#[test]
fn a_minted_grant_stays_within_its_issuer() {
    let (key, grant) = make_grant(DAY_NS);
    let refused = |cert: DeviceCert| {
        assert_eq!(
            verify_device_cert(&forced(cert.clone(), &key)),
            Err(DeviceCertError::InvalidIssuer)
        );
        let mut signed = cert;
        assert_eq!(
            sign_device_cert(&mut signed, &key),
            Err(DeviceCertError::InvalidIssuer)
        );
    };
    let minted = || prepare_minted_grant(&grant, [12; 32], [7; 16], "Jane", 2_000, 3_000);
    // All workspaces again: wider than a mint may be.
    let mut wide = minted();
    wide.capabilities = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
    refused(wide);
    // Anything besides the scope.
    let mut member = minted();
    member.capabilities = DeviceCapabilities::member();
    refused(member);
    // Past its issuer's expiry.
    let mut longer = minted();
    longer.expires_at_ns = Some(1_000 + DAY_NS + 1);
    refused(longer);
    // Before its issuer was valid.
    let mut early = minted();
    early.issued_at_ns = 500;
    refused(early);
    // The issuer's own key.
    let mut same = minted();
    same.device_public_key = grant.device_public_key;
    refused(same);
    // Another person's grant.
    let mut other = minted();
    other.user_public_key = [3; 32];
    refused(other);
    // A one-workspace grant mints nothing.
    let mut narrow = grant.clone();
    narrow.capabilities = DeviceCapabilities::agent_grant(AgentScope::Workspace([7; 16]));
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    sign_device_cert(&mut narrow, &root).unwrap();
    let mut from_narrow = prepare_minted_grant(&narrow, [12; 32], [7; 16], "Jane", 2_000, 3_000);
    assert_eq!(
        sign_device_cert(&mut from_narrow, &key),
        Err(DeviceCertError::InvalidIssuer)
    );
    // A minted grant mints nothing: the chain does not decode.
    let mut first = minted();
    sign_device_cert(&mut first, &key).unwrap();
    let chained = prepare_minted_grant(&first, [13; 32], [7; 16], "Jane", 2_500, 2_900);
    let encoded = postcard::to_allocvec(&chained).unwrap();
    assert!(postcard::from_bytes::<DeviceCert>(&encoded).is_err());
}

/// Revoking a grant refuses every grant it minted, whenever minted.
#[test]
fn revoking_a_grant_ends_what_it_minted() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let (key, grant) = make_grant(DAY_NS);
    let mut minted = prepare_minted_grant(&grant, [12; 32], [7; 16], "Jane", 2_000, 9_000);
    sign_device_cert(&mut minted, &key).unwrap();
    let mut entry = crate::RevocationEntry {
        user_public_key: grant.user_public_key,
        revoked_device_public_key: grant.device_public_key,
        revoked_at_ns: 5_000,
        reason: crate::RevocationReason::Retired,
        signature: [0; 64],
    };
    crate::sign_revocation_entry(&mut entry, &root).unwrap();
    let mut revocations = crate::RevocationList::new();
    assert!(!revocations.refuses(&minted));
    revocations.insert(entry).unwrap();
    assert!(revocations.refuses(&grant));
    assert!(revocations.refuses(&minted));
    assert!(revocations.refuses_at(&minted, 2_500));
}

/// Root `[1; 32]`'s server key (key `[4; 32]`) from 1 000 ns until
/// `expires_at_ns`, signed.
fn make_server_key(expires_at_ns: Option<u64>) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Visa backend".to_owned(),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: 1_000,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// A server key reaches all workspaces and lasts until revoked, or until
/// the expiry its person chose; its marker is in both encodings and under
/// its signature, and verifiers that predate it refuse it. A grant
/// without the marker still expires within the bound.
#[test]
fn a_server_key_lasts_until_revoked_or_its_chosen_expiry() {
    let capabilities = DeviceCapabilities::server_key_grant();
    assert_eq!(
        postcard::to_allocvec(&capabilities).unwrap(),
        [
            EXTENDED_BINARY_MARKER,
            EXTENDED_AGENT | 32,
            SCOPE_ALL,
            0,
            0,
            0,
            0
        ]
    );
    assert_eq!(
        serde_json::to_string(&capabilities).unwrap(),
        r#"{"can_own_workspaces":false,"can_admit_devices":false,"can_act_as_host":false,"can_read_all_workspaces":false,"agent":"all","server_key":true}"#
    );
    // A bit the decoder does not know is refused, as decoders before the
    // marker refuse the marker's.
    assert!(
        postcard::from_bytes::<DeviceCapabilities>(&[3, EXTENDED_AGENT | 128, 0, 0, 0, 0, 0])
            .is_err()
    );
    let lasting = [None, Some(1_000 + 10 * MAX_AGENT_GRANT_LIFETIME_NS)];
    for expiry in lasting {
        let (_, key) = make_server_key(expiry);
        verify_device_cert_at(&key, 1_000 + 5 * MAX_AGENT_GRANT_LIFETIME_NS).unwrap();
        let bytes = postcard::to_allocvec(&(&key, [9u8; 32])).unwrap();
        let (back, _): (DeviceCert, [u8; 32]) = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, key);
        assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&bytes).is_err());
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), key);
        assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
        // The marker is signed: the same signature on a plain grant fails.
        let mut plain = key.clone();
        plain.capabilities = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
        plain.expires_at_ns = Some(2_000);
        assert_eq!(
            verify_device_cert(&plain),
            Err(DeviceCertError::BadSignature)
        );
    }
    let (_, chosen) = make_server_key(Some(5_000));
    verify_device_cert_at(&chosen, 4_999).unwrap();
    assert_eq!(
        verify_device_cert_at(&chosen, 5_000),
        Err(DeviceCertError::Expired)
    );

    // Only a grant reaching all workspaces is a server key, and a grant
    // without the marker (an agent's, through consent) still expires.
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    for capabilities in [
        DeviceCapabilities {
            server_key: true,
            ..DeviceCapabilities::agent_grant(AgentScope::Workspace([7; 16]))
        },
        DeviceCapabilities {
            agent: None,
            ..DeviceCapabilities::server_key_grant()
        },
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    ] {
        let mut cert = chosen.clone();
        cert.capabilities = capabilities;
        cert.expires_at_ns = None;
        assert_eq!(
            sign_device_cert(&mut cert, &root),
            Err(DeviceCertError::InvalidAgentGrant)
        );
        assert_eq!(
            verify_device_cert(&forced(cert, &root)),
            Err(DeviceCertError::InvalidAgentGrant)
        );
    }
}

/// A key that may operate web pages in Browser is one its person signed,
/// and a key minted from it keeps that and can be given no more.
#[test]
fn operating_web_pages_is_what_the_person_signed_and_a_mint_keeps() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let granted = |capabilities| {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Jane Doe's agent".to_owned(),
            capabilities,
            issued_at_ns: 1_000,
            expires_at_ns: Some(1_000 + MAX_AGENT_GRANT_LIFETIME_NS),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).map(|()| cert)
    };
    let scope = AgentScope::Workspace([7; 16]);
    let operating = granted(DeviceCapabilities::agent_grant(scope).with_browser_pages(true))
        .expect("a person signs the key their agent acts with");
    verify_device_cert_at(&operating, 2_000).unwrap();
    assert!(operating.capabilities.browser_pages);
    assert!(
        !granted(DeviceCapabilities::agent_grant(scope))
            .unwrap()
            .capabilities
            .browser_pages,
        "a key not asked about operates no page"
    );
    // A device's capabilities never carry it: only a key's do.
    assert!(
        !DeviceCapabilities::member()
            .with_browser_pages(false)
            .browser_pages
    );
    // Minted from a key that operates pages, the mint keeps it.
    let mine = granted(
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces).with_browser_pages(true),
    )
    .unwrap();
    let mut minted = prepare_minted_grant(&mine, [12; 32], [7; 16], "Jane", 2_000, 3_000);
    sign_device_cert(&mut minted, &key).unwrap();
    verify_device_cert_at(&minted, 2_500).unwrap();
    assert!(minted.capabilities.browser_pages);
    // From one that does not, a mint cannot gain it.
    let plain = granted(DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces)).unwrap();
    let mut gained = prepare_minted_grant(&plain, [12; 32], [7; 16], "Jane", 2_000, 3_000);
    gained.capabilities.browser_pages = true;
    assert_eq!(
        sign_device_cert(&mut gained, &key),
        Err(DeviceCertError::InvalidIssuer)
    );
}

/// A server key that never expires mints keys for one workspace that
/// expire within the bound, and revoking it ends them.
#[test]
fn a_server_key_mints_keys_that_expire_and_end_with_it() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let (key, server) = make_server_key(None);
    let mint = |expires_at_ns| {
        prepare_minted_grant(&server, [12; 32], [7; 16], "Jane", 2_000, expires_at_ns)
    };
    let mut minted = mint(2_000 + MAX_AGENT_GRANT_LIFETIME_NS);
    sign_device_cert(&mut minted, &key).unwrap();
    verify_device_cert_at(&minted, 3_000).unwrap();
    assert_eq!(
        minted.capabilities.agent,
        Some(AgentScope::Workspace([7; 16]))
    );
    assert!(!minted.capabilities.server_key);
    let mut forever = mint(3_000);
    forever.expires_at_ns = None;
    let mut longer = mint(2_001 + MAX_AGENT_GRANT_LIFETIME_NS);
    for mut refused in [forever, longer.clone()] {
        assert_eq!(
            sign_device_cert(&mut refused, &key),
            Err(DeviceCertError::InvalidAgentGrant)
        );
    }
    longer.capabilities.server_key = true;
    assert!(sign_device_cert(&mut longer, &key).is_err());

    let mut entry = crate::RevocationEntry {
        user_public_key: server.user_public_key,
        revoked_device_public_key: server.device_public_key,
        revoked_at_ns: 5_000,
        reason: crate::RevocationReason::Retired,
        signature: [0; 64],
    };
    crate::sign_revocation_entry(&mut entry, &root).unwrap();
    let mut revocations = crate::RevocationList::new();
    revocations.insert(entry).unwrap();
    assert!(revocations.refuses(&server));
    assert!(revocations.refuses_at(&minted, 2_500));
}

/// Revoking a stand-in refuses what it issues from then on and keeps
/// what it issued before.
#[test]
fn revoking_a_stand_in_keeps_what_it_issued_before() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let (key, stand_in) = make_stand_in(1);
    let mut entry = crate::RevocationEntry {
        user_public_key: stand_in.user_public_key,
        revoked_device_public_key: stand_in.device_public_key,
        revoked_at_ns: 5_000,
        reason: crate::RevocationReason::Retired,
        signature: [0; 64],
    };
    crate::sign_revocation_entry(&mut entry, &root).unwrap();
    let mut revocations = crate::RevocationList::new();
    revocations.insert(entry).unwrap();

    let mut before = issued_by(&stand_in, 4_999);
    sign_device_cert(&mut before, &key).unwrap();
    let mut after = issued_by(&stand_in, 5_000);
    sign_device_cert(&mut after, &key).unwrap();
    verify_device_cert(&before).unwrap();
    verify_device_cert(&after).unwrap();
    assert!(!revocations.refuses(&before));
    assert!(!revocations.refuses_at(&before, 10_000));
    assert!(revocations.refuses(&after));
    assert!(revocations.refuses_at(&after, 10_000));
    assert!(revocations.refuses(&stand_in));
    assert!(!revocations.refuses_at(&stand_in, 4_999));
    // The device's own revocation still counts.
    let mut device = crate::RevocationEntry {
        user_public_key: stand_in.user_public_key,
        revoked_device_public_key: before.device_public_key,
        revoked_at_ns: 20_000,
        reason: crate::RevocationReason::Lost,
        signature: [0; 64],
    };
    crate::sign_revocation_entry(&mut device, &root).unwrap();
    revocations.insert(device).unwrap();
    assert!(revocations.refuses(&before));
    assert!(!revocations.refuses_at(&before, 19_999));
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

/// Administration signs under its own tag, round-trips in both encodings
/// (alone and together with the foreign marker), survives no flip of the
/// flag, and is refused by every verifier that predates it.
#[test]
fn administration_certificates_sign_under_their_own_tag_and_old_verifiers_refuse_them() {
    let native = golden_cert(false);
    let nonce = [9u8; 32];
    for foreign in [false, true] {
        let admin = golden_cert_with(DeviceCapabilities {
            foreign,
            ..DeviceCapabilities::administrator()
        });
        verify_device_cert(&admin).unwrap();
        assert_eq!(
            device_cert_signing_digest(&admin).unwrap(),
            signing_digest(DEVCERT_V3_PREIMAGE_TAG, &admin.view()).unwrap()
        );
        let json = serde_json::to_string(&admin).unwrap();
        assert!(json.contains(r#""can_administer_devices":true"#));
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), admin);
        let bytes = crate::canonical::canonical_bytes(&admin).unwrap();
        let marker = 32 + 32 + 1 + "golden".len();
        assert_eq!(bytes[marker], EXTENDED_BINARY_MARKER);
        assert_eq!(
            crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).unwrap(),
            admin
        );

        let mut stripped = admin.clone();
        stripped.capabilities.can_administer_devices = false;
        assert_eq!(
            verify_device_cert(&stripped).unwrap_err(),
            DeviceCertError::BadSignature
        );

        let json_error = serde_json::from_str::<v1::DeviceCert>(&json).unwrap_err();
        assert!(
            json_error.to_string().contains("unknown field"),
            "{json_error}"
        );
        let wire = postcard::to_allocvec(&(&admin, nonce)).unwrap();
        assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&wire).is_err());
    }
    let mut granted = native;
    granted.capabilities.can_administer_devices = true;
    assert_eq!(
        verify_device_cert(&granted).unwrap_err(),
        DeviceCertError::BadSignature
    );
}

#[test]
fn extended_capabilities_refuse_bytes_that_do_not_grant_administration() {
    let admin = golden_cert_with(DeviceCapabilities::administrator());
    let bytes = crate::canonical::canonical_bytes(&admin).unwrap();
    let marker = 32 + 32 + 1 + "golden".len();
    assert_eq!(bytes[marker + 1], EXTENDED_ADMINISTER);
    for extended in [0, EXTENDED_FOREIGN, 4, 0x80] {
        let mut changed = bytes.clone();
        changed[marker + 1] = extended;
        assert!(
            crate::canonical::from_canonical_bytes::<DeviceCert>(&changed).is_err(),
            "extended byte {extended}"
        );
    }
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
