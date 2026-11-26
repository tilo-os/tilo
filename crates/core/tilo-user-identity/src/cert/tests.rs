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

tilo_test_env::scenario! {
    /// Certificates sign and verify, honour expiry, round-trip canonically, reproduce their
    /// signature vector, and every capability preset is distinct.
    #[allow(
        clippy::similar_names,
        reason = "the _sk/_pk keypair naming convention is deliberate and used throughout this crate's tests"
    )]
    fn device_certificates_sign_verify_expire_and_round_trip;

    phase sign_and_verify;
    {
        let (_root, cert) = fresh_cert(DeviceCapabilities::primary());
        verify_device_cert(&cert).unwrap();
    }

    phase expiry_semantics;
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

    phase cert_round_trips_through_canonical;
    {
        let (_root, cert) = fresh_cert(DeviceCapabilities::primary());
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        let back: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(cert, back);
        verify_device_cert(&back).unwrap();
    }

    phase signature_vector_is_reproducible;
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
                retired: RetiredFlags::default(),
                can_own_workspaces: true,
                can_admit_devices: false,
                foreign: false,
                agent: None,
                stand_in: false,
                server_key: false,
                browser_pages: false,
                workspace_service: false,
                workspace: None,
                records: None,
                placing: None,
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

    phase all_capability_presets_are_distinct;
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
        let s_foreign = build(DeviceCapabilities {
            foreign: true,
            ..DeviceCapabilities::member()
        });
        assert_ne!(s_primary, s_member);
        assert_ne!(s_member, s_foreign);
        assert_ne!(s_primary, s_foreign);
    }
}

tilo_test_env::scenario! {
    /// Tampering with the capability, device key, signature or user key, or signing by a different
    /// root, fails verification.
    fn tampering_with_any_certificate_field_or_root_fails_verification;

    phase tampered_capability_fails;
    {
        let (_root, mut cert) = fresh_cert(DeviceCapabilities::member());
        cert.capabilities.can_admit_devices = true;
        assert_eq!(
            verify_device_cert(&cert).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    phase tampered_device_pubkey_fails;
    {
        let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
        cert.device_public_key[0] ^= 0x01;
        assert_eq!(
            verify_device_cert(&cert).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    phase tampered_signature_fails;
    {
        let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
        cert.signature[0] ^= 0x01;
        assert_eq!(
            verify_device_cert(&cert).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    phase tampered_user_pubkey_fails;
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

    phase signed_by_different_root_fails;
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

    phase remembered_verification_never_accepts_a_changed_certificate;
    {
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

const GOLDEN_V1_JSON: &str = r#"{"user_public_key":[138,136,227,221,116,9,241,149,253,82,219,45,60,186,93,114,202,103,9,191,29,148,18,27,243,116,136,1,180,15,111,92],"device_public_key":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"device_label":"golden","capabilities":{"can_own_workspaces":true,"can_admit_devices":false},"issued_at_ns":1000000,"expires_at_ns":2000000,"signature":[193,94,11,63,27,213,214,122,35,157,141,24,157,200,120,75,180,243,189,153,177,189,224,147,47,142,23,15,234,170,239,9,175,129,176,168,99,254,102,174,97,65,248,91,187,181,93,141,241,211,8,123,41,66,110,172,36,46,39,75,189,14,237,1]}"#;
const GOLDEN_V1_POSTCARD: &str = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c020202020202020202020202020202020202020202020202020202020202020206676f6c64656e01000000c0843d0180897a40c15e0b3f1bd5d67a239d8d189dc8784bb4f3bd99b1bde0932f8e170feaaaef09af81b0a863fe66ae6141f85bbbb55d8df1d3087b29426eac242e274bbd0eed01";
const GOLDEN_V1_DIGEST: &str = "0d8271d3aadb5438af000ff5f96b6355102b57a271885ed26142fb698884e623";

tilo_test_env::scenario! {
    fn certificate_markers_keep_their_encodings;

    phase native_certificates_keep_their_v1_encoding_and_digest;
    // The certificate encodings and signing digest as they were before the
    // foreign marker existed, pinned from the pre-change build.
    {
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

    phase foreign_certificates_sign_their_marker;
    {
        let foreign = golden_cert(true);
        verify_device_cert(&foreign).unwrap();
        assert_eq!(
            device_cert_signing_digest(&foreign).unwrap(),
            signing_digest(DEVCERT_PREIMAGE_TAG, &foreign.view()).unwrap()
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

    phase extended_capabilities_refuse_bytes_that_need_no_extended_form_or_lack_their_scope;
    {
        let stand_in = golden_cert_with(DeviceCapabilities::stand_in_approver());
        let bytes = crate::canonical::canonical_bytes(&stand_in).unwrap();
        let marker = 32 + 32 + 1 + "golden".len();
        assert_eq!(bytes[marker + 1], EXTENDED_STAND_IN);
        for extended in [0, EXTENDED_FOREIGN, EXTENDED_AGENT] {
            let mut changed = bytes.clone();
            changed[marker + 1] = extended;
            assert!(
                crate::canonical::from_canonical_bytes::<DeviceCert>(&changed).is_err(),
                "extended byte {extended}"
            );
        }
    }

    phase a_certificate_written_with_the_retired_administer_bit_reads_and_writes_back_with_it;
    {
        let stand_in = golden_cert_with(DeviceCapabilities::stand_in_approver());
        let bytes = crate::canonical::canonical_bytes(&stand_in).unwrap();
        let marker = 32 + 32 + 1 + "golden".len();
        let mut retired = bytes.clone();
        retired[marker + 1] = EXTENDED_STAND_IN | 2;
        let read = crate::canonical::from_canonical_bytes::<DeviceCert>(&retired).unwrap();
        assert!(read.capabilities.retired.administer);
        assert!(!read.capabilities.retired.any(), "the bit grants nothing");
        assert_eq!(
            DeviceCapabilities {
                retired: RetiredFlags::default(),
                ..read.capabilities.clone()
            },
            stand_in.capabilities
        );
        assert_eq!(crate::canonical::canonical_bytes(&read).unwrap(), retired);
        let mut only_retired = bytes;
        only_retired[marker + 1] = 2;
        let read = crate::canonical::from_canonical_bytes::<DeviceCert>(&only_retired).unwrap();
        assert_eq!(crate::canonical::canonical_bytes(&read).unwrap(), only_retired);
    }

    phase binary_capabilities_refuse_flag_bytes_other_than_zero_or_one;
    {
        let mut bytes = crate::canonical::canonical_bytes(&golden_cert(true)).unwrap();
        let marker = 32 + 32 + 1 + "golden".len();
        assert_eq!(bytes[marker], FOREIGN_BINARY_MARKER);
        bytes[marker + 1] = 3;
        assert!(crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).is_err());
    }
}

/// A certificate signed while its device was granted the retired grants, as people's
/// devices still keep it, reads from the JSON they stored, verifies, and writes back exactly as it
/// was signed in both encodings: dropping the reader left every such device unable to start an app.
#[test]
fn a_certificate_signed_with_the_retired_grants_still_loads_and_verifies() {
    let cert = golden_cert_with(DeviceCapabilities {
        retired: RetiredFlags {
            act_as_host: true,
            read_all_workspaces: true,
            administer: true,
        },
        foreign: true,
        ..DeviceCapabilities::member()
    });
    let stored = serde_json::to_string(&cert).unwrap();
    assert!(stored.contains(r#""can_act_as_host":true"#), "{stored}");
    assert!(
        stored.contains(r#""can_read_all_workspaces":true"#),
        "{stored}"
    );
    assert!(stored.contains(r#""can_administer":true"#), "{stored}");
    let read: DeviceCert = serde_json::from_str(&stored).unwrap();
    assert_eq!(read, cert);
    verify_device_cert_at(&read, 1_500_000).expect("a certificate signed with them verifies");
    let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
    let decoded = crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).unwrap();
    assert_eq!(decoded, cert);
    assert_eq!(crate::canonical::canonical_bytes(&decoded).unwrap(), bytes);
    // Written by an older build with both spelled out even when clear.
    let older = stored
        .replace(r#""can_act_as_host":true"#, r#""can_act_as_host":false"#)
        .replace(
            r#""can_read_all_workspaces":true"#,
            r#""can_read_all_workspaces":false"#,
        );
    let clear: DeviceCert = serde_json::from_str(&older).unwrap();
    assert!(!clear.capabilities.retired.any());
}

tilo_test_env::scenario! {
    fn agent_grants_keep_their_scope_and_what_they_mint_stays_within_them;

    phase agent_grants_carry_their_scope_and_expire;
    // An agent's grant round-trips with its scope in both encodings, must
    // expire and grant nothing else, and survives no change of scope.
    {
        let workspace = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
        let one = AgentScope::parse(workspace).unwrap();
        assert_eq!(one.name(), workspace);
        assert!(one.reaches(workspace));
        assert!(!one.reaches("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f1"));
        assert!(!one.makes_workspaces());
        assert!(AgentScope::AllWorkspaces.reaches(workspace));
        assert!(AgentScope::AllWorkspaces.makes_workspaces());
        // One workspace narrows what all of them was, and never the reverse.
        let all = AgentScope::AllWorkspaces;
        assert!(one.within(&all));
        assert!(one.within(&one));
        assert!(all.within(&all));
        assert!(!all.within(&one));
        // One agent's key names that agent by its id, in its workspaces, and stands for no other.
        let id = one.workspace().unwrap();
        let scribe = AgentScope::agent_in(id, "scribe");
        assert_eq!(scribe.name(), "agent:scribe");
        assert_eq!(AgentScope::parse(&scribe.name()), None, "no text a person writes");
        assert!(scribe.reaches(workspace) && !scribe.makes_workspaces());
        assert!(scribe.is_agent("scribe") && !scribe.is_agent("critic"));
        assert_eq!(scribe.agent_grant().map(|grant| grant.agent.as_str()), Some("scribe"));
        assert!(!one.is_agent("scribe"));
        assert!(scribe.within(&one) && scribe.within(&all));
        assert!(!one.within(&scribe));
        assert!(!AgentScope::agent_in(id, "critic").within(&scribe));
        // What layout 3 named an agent's key by still names that agent, and reads back from text.
        let hashed = AgentScope::AgentHash {
            workspace: id,
            agent: AgentScope::agent_of("scribe"),
        };
        assert_eq!(AgentScope::parse(&hashed.name()), Some(hashed.clone()));
        assert!(hashed.is_agent("scribe") && !hashed.is_agent("critic"));
        assert_eq!(hashed.agent_grant(), None);
        // An agent's grant reaches the workspaces it names, and acts in the apps it names.
        let other = [0x22; 16];
        let wide = AgentScope::Agent(AgentGrant {
            agent: "scribe".to_owned(),
            workspaces: vec![id, other],
            apps: vec!["notes".to_owned(), "writer".to_owned()],
        });
        assert!(wide.reaches_id(&id) && wide.reaches_id(&other) && !wide.reaches_id(&[0x33; 16]));
        assert_eq!(wide.workspace(), None, "no one workspace");
        assert!(!wide.within(&one));
        let grant = wide.agent_grant().unwrap();
        assert!(grant.permits_app("notes") && !grant.permits_app("mail"));
        for unbounded in [
            AgentGrant { agent: String::new(), ..grant.clone() },
            AgentGrant { agent: "a".repeat(MAX_AGENT_ID_BYTES + 1), ..grant.clone() },
            AgentGrant { workspaces: Vec::new(), ..grant.clone() },
            AgentGrant { workspaces: vec![other, id], ..grant.clone() },
            AgentGrant { workspaces: vec![[0; 16]], ..grant.clone() },
            AgentGrant { apps: vec!["writer".to_owned(), "notes".to_owned()], ..grant.clone() },
        ] {
            assert!(!unbounded.is_well_formed(), "{unbounded:?}");
            let refused = golden_cert_with(DeviceCapabilities::nothing());
            let mut cert = refused.clone();
            cert.capabilities = DeviceCapabilities::agent_grant(AgentScope::Agent(unbounded));
            assert!(postcard::to_allocvec(&cert).is_err(), "never written");
        }
        for malformed in [
            "",
            "ALL",
            "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0",
            "00000000-0000-0000-0000-000000000000",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0/",
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0/ab",
        ] {
            assert_eq!(AgentScope::parse(malformed), None, "{malformed}");
        }

        for scope in [all.clone(), one.clone(), scribe, hashed, wide] {
            let grant = golden_cert_with(DeviceCapabilities::agent_grant(scope.clone()));
            verify_device_cert(&grant).unwrap();
            assert_eq!(
                device_cert_signing_digest(&grant).unwrap(),
                signing_digest(DEVCERT_PREIMAGE_TAG, &grant.view()).unwrap()
            );
            let json = serde_json::to_string(&grant).unwrap();
            assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), grant);
            let bytes = postcard::to_allocvec(&(&grant, [9u8; 32])).unwrap();
            let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&bytes).unwrap();
            assert_eq!((back, nonce), (grant.clone(), [9u8; 32]));
            // Another scope under the same signature does not verify.
            let mut moved = grant.clone();
            moved.capabilities.agent = Some(match scope {
                AgentScope::AllWorkspaces => one.clone(),
                AgentScope::Workspace(_) | AgentScope::Agent(_) | AgentScope::AgentHash { .. } => {
                    all.clone()
                }
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
            (DeviceCapabilities::agent_grant(one.clone()), None),
            (
                DeviceCapabilities::agent_grant(one.clone()),
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
                .sign(&signing_digest(DEVCERT_PREIMAGE_TAG, &cert.view()).unwrap())
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

    phase a_grant_mints_a_narrower_grant;
    // An all-workspaces grant mints a grant for one workspace that verifies
    // as one, under its issuer, and round-trips both encodings.
    {
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

    phase a_minted_grant_stays_within_its_issuer;
    // A minted grant never reaches more, lasts longer, or chains further
    // than its issuer, and only an all-workspaces grant mints.
    {
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

    phase revoking_a_grant_ends_what_it_minted;
    // Revoking a grant refuses every grant it minted, whenever minted.
    {
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

tilo_test_env::scenario! {
    fn a_stand_in_is_bounded_by_its_root_and_its_revocation;

    phase a_stand_in_is_certified_by_its_root_and_is_no_device;
    // A stand-in's certificate verifies as one, is no device, carries
    // nothing besides being a stand-in, and always expires within 30 days.
    {
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
            signing_digest(DEVCERT_PREIMAGE_TAG, &cert.view()).unwrap()
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

        // It round-trips in both encodings.
        let json = serde_json::to_string(&cert).unwrap();
        assert!(json.contains(r#""stand_in":true"#));
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), cert);
        let wire = postcard::to_allocvec(&(&cert, [9u8; 32])).unwrap();
        let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&wire).unwrap();
        assert_eq!((back, nonce), (cert.clone(), [9u8; 32]));

        // Any other capability beside it, no expiry, a longer life, or its
        // root's own key is neither signed nor taken.
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut others = Vec::new();
        for widen in [
            |c: &mut DeviceCapabilities| c.can_own_workspaces = true,
            |c: &mut DeviceCapabilities| c.can_admit_devices = true,
            |c: &mut DeviceCapabilities| c.foreign = true,
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

    phase an_issuer_with_an_issuer_does_not_decode;
    // A certificate whose issuer names an issuer of its own is refused by
    // the binary decoder before it recurses, and by the JSON one.
    {
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

    phase revoking_a_stand_in_keeps_what_it_issued_before;
    // Revoking a stand-in refuses what it issues from then on and keeps
    // what it issued before.
    {
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
}

/// A member certificate a stand-in issued verifies, binds its issuer, and
/// round-trips.
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
            DEVCERT_PREIMAGE_TAG,
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
    let wire = postcard::to_allocvec(&(&cert, [9u8; 32])).unwrap();
    let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&wire).unwrap();
    assert_eq!((back, nonce), (cert.clone(), [9u8; 32]));
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
    let mut refused: Vec<(DeviceCert, &ed25519_dalek::SigningKey)> = Vec::new();

    let mut more = issued_by(&stand_in, 2_000);
    more.capabilities = DeviceCapabilities::primary();
    refused.push((more, &key));
    for caps in [
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

/// A device of the person signs a browser tab in: a member certificate for
/// at most a browser's lifetime, for its own person, while it is valid, with
/// its own key; and nothing it issued can issue in turn.
#[test]
fn a_device_signs_a_browser_tab_in() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let device_key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let mut device = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device_key.verifying_key().to_bytes(),
        device_label: "Claude Code on build-box".to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1_000,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut device, &root).unwrap();
    verify_device_cert(&device).unwrap();
    assert!(device.is_root_signed_device());

    // The tab's certificate verifies, binds the device as its issuer, and
    // round-trips.
    let mut tab = issued_by(&device, 2_000);
    sign_device_cert(&mut tab, &device_key).unwrap();
    verify_device_cert(&tab).unwrap();
    verify_device_cert_at(&tab, 3_000).unwrap();
    assert!(
        !tab.is_root_signed_device(),
        "it answers a tab, it is no device"
    );
    assert_eq!(tab.signer_public_key(), device.device_public_key);
    assert_eq!(tab.issuer.as_deref(), Some(&device));
    let json = serde_json::to_string(&tab).unwrap();
    assert!(json.contains(r#""issuer":{"#));
    assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), tab);
    let wire = postcard::to_allocvec(&(&tab, [9u8; 32])).unwrap();
    let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&wire).unwrap();
    assert_eq!((back, nonce), (tab.clone(), [9u8; 32]));

    // Beyond a browser's bounds, for another person, outside the device's
    // validity, its own key or the root's, or a chain of its own, it is
    // refused.
    let other_root = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let other_device_key = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
    let mut other_device = DeviceCert {
        user_public_key: other_root.verifying_key().to_bytes(),
        device_public_key: other_device_key.verifying_key().to_bytes(),
        device_label: "Another person's laptop".to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1_000,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut other_device, &other_root).unwrap();
    let mut bounded = device.clone();
    bounded.expires_at_ns = Some(5_000);
    sign_device_cert(&mut bounded, &root).unwrap();
    let mut refused: Vec<(DeviceCert, &ed25519_dalek::SigningKey)> = Vec::new();
    let mut more = issued_by(&device, 2_000);
    more.capabilities = DeviceCapabilities::primary();
    refused.push((more, &device_key));
    for caps in [
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        DeviceCapabilities::stand_in_approver(),
    ] {
        let mut cert = issued_by(&device, 2_000);
        cert.capabilities = caps;
        refused.push((cert, &device_key));
    }
    let mut longer = issued_by(&device, 2_000);
    longer.expires_at_ns = longer.expires_at_ns.map(|expiry| expiry + 1);
    refused.push((longer, &device_key));
    let mut forever = issued_by(&device, 2_000);
    forever.expires_at_ns = None;
    refused.push((forever, &device_key));
    let mut other_persons_tab = issued_by(&device, 2_000);
    other_persons_tab.user_public_key = other_device.user_public_key;
    refused.push((other_persons_tab, &device_key));
    // Before the device was certified, and once it expired.
    refused.push((issued_by(&device, 999), &device_key));
    refused.push((issued_by(&bounded, 5_000), &device_key));
    // Its own key, or the root's, as the tab.
    let mut own = issued_by(&device, 2_000);
    own.device_public_key = device.device_public_key;
    refused.push((own, &device_key));
    let mut root_key = issued_by(&device, 2_000);
    root_key.device_public_key = device.user_public_key;
    refused.push((root_key, &device_key));
    // A tab the device signed signs nothing.
    let mut chained = issued_by(&tab, 3_000);
    chained.device_public_key = [10; 32];
    refused.push((chained, &device_key));

    for (cert, signer) in refused {
        let mut signed = cert.clone();
        assert!(sign_device_cert(&mut signed, signer).is_err(), "{cert:?}");
        let forced = forced(cert, signer);
        assert!(verify_device_cert(&forced).is_err(), "{forced:?}");
    }

    // A device that expires a day later signs a tab for a browser's
    // lifetime, beyond its own.
    let mut day = device.clone();
    day.expires_at_ns = Some(1_000 + 24 * 60 * 60 * 1_000_000_000);
    sign_device_cert(&mut day, &root).unwrap();
    let mut beyond = issued_by(&day, 2_000);
    eprintln!(
        "signing beyond: {:?}",
        sign_device_cert(&mut beyond, &device_key)
    );

    // A device that expires signs while it is valid and not at its end.
    let mut before = issued_by(&bounded, 4_999);
    sign_device_cert(&mut before, &device_key).unwrap();
    verify_device_cert(&before).unwrap();
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

tilo_test_env::scenario! {
    fn a_server_key_is_what_its_person_signed;

    phase a_server_key_lasts_until_revoked_or_its_chosen_expiry;
    // A server key reaches all workspaces and lasts until revoked, or until
    // the expiry its person chose; its marker is in both encodings and under
    // its signature. A grant
    // without the marker still expires within the bound.
    {
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
            r#"{"can_own_workspaces":false,"can_admit_devices":false,"agent":"all","server_key":true}"#
        );
        // A flag byte other than 0 or 1 is refused.
        assert!(
            postcard::from_bytes::<DeviceCapabilities>(&[3, EXTENDED_AGENT, 0, 2, 0, 0, 0])
                .is_err()
        );
        let lasting = [None, Some(1_000 + 10 * MAX_AGENT_GRANT_LIFETIME_NS)];
        for expiry in lasting {
            let (_, key) = make_server_key(expiry);
            verify_device_cert_at(&key, 1_000 + 5 * MAX_AGENT_GRANT_LIFETIME_NS).unwrap();
            let bytes = postcard::to_allocvec(&(&key, [9u8; 32])).unwrap();
            let (back, _): (DeviceCert, [u8; 32]) = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(back, key);
            let json = serde_json::to_string(&key).unwrap();
            assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), key);
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

        // A key the person made for one workspace is theirs too and lasts
        // the same; a server key reaches somewhere, and a grant without the
        // marker (an agent's, through consent) still expires.
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut one = chosen.clone();
        one.capabilities = DeviceCapabilities::own_key_grant(AgentScope::Workspace([7; 16]));
        one.expires_at_ns = None;
        sign_device_cert(&mut one, &root).unwrap();
        verify_device_cert_at(&one, 1_000 + 5 * MAX_AGENT_GRANT_LIFETIME_NS).unwrap();
        for capabilities in [
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

    phase operating_web_pages_is_what_the_person_signed_and_a_mint_keeps;
    // A key that may operate web pages in Browser is one its person signed,
    // and a key minted from it keeps that and can be given no more.
    {
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
        let operating = granted(DeviceCapabilities::agent_grant(scope.clone()).with_browser_pages(true))
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
    // A key the person made mints keys of theirs.
    assert!(minted.capabilities.server_key);
    let mut forever = mint(3_000);
    forever.expires_at_ns = None;
    let longer = mint(2_001 + MAX_AGENT_GRANT_LIFETIME_NS);
    // Nor does it lose the marker its issuer gave it.
    let mut unmarked = mint(3_000);
    unmarked.capabilities.server_key = false;
    for (case, mut refused) in [
        ("forever", forever),
        ("longer", longer),
        ("unmarked", unmarked),
    ] {
        assert_eq!(
            sign_device_cert(&mut refused, &key),
            Err(DeviceCertError::InvalidIssuer),
            "{case}"
        );
    }

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

/// A device of the person `root`, root-signed with `capabilities`, valid
/// from 1 000 until `expires_at_ns`.
fn root_signed_device(
    root: &ed25519_dalek::SigningKey,
    key: &ed25519_dalek::SigningKey,
    capabilities: DeviceCapabilities,
    expires_at_ns: Option<u64>,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Laptop".into(),
        capabilities,
        issued_at_ns: 1_000,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

tilo_test_env::scenario! {
    fn a_device_certifies_its_own_workspace_service;

    phase the_service_verifies_under_its_device_and_round_trips;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let device = root_signed_device(&root, &key, DeviceCapabilities::member(), None);
        let mut service = prepare_service_cert(&device, [9; 32], "Laptop's service", 2_000);
        sign_device_cert(&mut service, &key).unwrap();
        verify_device_cert_at(&service, 2_500).unwrap();
        assert_eq!(service.user_public_key, device.user_public_key);
        assert_eq!(service.signer_public_key(), device.device_public_key);
        assert!(service.capabilities.workspace_service);
        assert!(service.capabilities.can_own_workspaces);
        // The service writes for the device that certified it, and that device for itself.
        assert_eq!(service.writes_for(), device.device_public_key);
        assert_eq!(device.writes_for(), device.device_public_key);
        assert!(service.is_of_device(&device.device_public_key));
        assert!(!device.is_of_device(&service.device_public_key));
        let wire = postcard::to_allocvec(&service).unwrap();
        assert_eq!(postcard::from_bytes::<DeviceCert>(&wire).unwrap(), service);
        let json = serde_json::to_string(&service).unwrap();
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), service);
        // The root's signature is not the device's.
        assert_eq!(
            verify_device_cert(&forced(service, &root)),
            Err(DeviceCertError::BadSignature)
        );
    }

    phase the_service_stays_within_its_device;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let device = root_signed_device(&root, &key, DeviceCapabilities::member(), Some(10_000));
        let refused = |cert: DeviceCert| {
            assert_eq!(
                verify_device_cert(&forced(cert, &key)),
                Err(DeviceCertError::InvalidIssuer)
            );
        };
        let service = || prepare_service_cert(&device, [9; 32], "service", 2_000);
        // More than the device may do.
        let mut wider = service();
        wider.capabilities.can_admit_devices = true;
        refused(wider);
        // Longer than the device's certificate.
        let mut longer = service();
        longer.expires_at_ns = Some(10_001);
        refused(longer);
        let mut forever = service();
        forever.expires_at_ns = None;
        refused(forever);
        // Before the device was certified.
        let mut early = service();
        early.issued_at_ns = 500;
        refused(early);
        // The device's own key.
        let mut same = service();
        same.device_public_key = device.device_public_key;
        refused(same);
        // Another person's device.
        let mut other = service();
        other.user_public_key = [3; 32];
        refused(other);
        // No device issued it: the root alone makes no workspace service.
        let mut rootless = service();
        rootless.issuer = None;
        assert_eq!(
            verify_device_cert(&forced(rootless, &root)),
            Err(DeviceCertError::InvalidIssuer)
        );
        // An agent's grant issues no workspace service.
        let grant = root_signed_device(
            &root,
            &key,
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            Some(10_000),
        );
        let mut from_grant = prepare_service_cert(&device, [9; 32], "service", 2_000);
        from_grant.issuer = Some(Box::new(grant));
        assert!(verify_device_cert(&forced(from_grant, &key)).is_err());
    }

    phase revoking_the_device_ends_its_service;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let device = root_signed_device(&root, &key, DeviceCapabilities::member(), None);
        let mut service = prepare_service_cert(&device, [9; 32], "service", 2_000);
        sign_device_cert(&mut service, &key).unwrap();
        let mut revocations = crate::RevocationList::new();
        assert!(!revocations.refuses(&service));
        let mut entry = crate::RevocationEntry {
            user_public_key: device.user_public_key,
            revoked_device_public_key: device.device_public_key,
            revoked_at_ns: 5_000,
            reason: crate::RevocationReason::Lost,
            signature: [0; 64],
        };
        crate::revocation::sign_revocation_entry(&mut entry, &root).unwrap();
        revocations.insert(entry).unwrap();
        // Even what the service signed before the device was revoked.
        assert!(revocations.refuses(&service));
        assert!(revocations.refuses_at(&service, 3_000));
    }
}

/// The workspace a confined device's tests certify it for, and another of its person's.
const VM_WORKSPACE: [u8; 16] = [0x11; 16];
const OTHER_WORKSPACE: [u8; 16] = [0x22; 16];

tilo_test_env::scenario! {
    fn a_device_confined_to_one_workspace_reaches_it_alone;

    phase it_round_trips_and_verifies_in_the_current_layout;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let cert = root_signed_device(
            &root,
            &key,
            DeviceCapabilities::workspace_device(VM_WORKSPACE),
            None,
        );
        verify_device_cert(&cert).unwrap();
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        assert_eq!(binary_layout_of(&bytes).unwrap(), BINARY_LAYOUT);
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(read, cert);
        verify_device_cert(&read).unwrap();
        let json: DeviceCert =
            serde_json::from_str(&serde_json::to_string(&cert).unwrap()).unwrap();
        assert_eq!(json, cert);
        let vm = uuid_text(&VM_WORKSPACE);
        assert_eq!(cert.capabilities.workspace_name().as_deref(), Some(vm.as_str()));
        assert!(cert.capabilities.reaches_workspace(&vm));
        assert!(!cert.capabilities.reaches_workspace(&uuid_text(&OTHER_WORKSPACE)));
        assert!(!cert.capabilities.reaches_workspace("not a workspace"));
        // It is a device of its person's, but none that adds devices, holds a workspace's keys
        // or is a personal device.
        assert!(!cert.is_root_signed_device());
        assert!(!cert.capabilities.holds_workspace_keys());
        assert!(!cert.capabilities.is_personal_device());
    }

    phase the_restriction_cannot_be_stripped_or_moved;
    // The restriction is inside the signed bytes: a certificate that drops it, or names
    // another workspace, no longer verifies.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let cert = root_signed_device(
            &root,
            &key,
            DeviceCapabilities::workspace_device(VM_WORKSPACE),
            None,
        );
        let mut stripped = cert.clone();
        stripped.capabilities.workspace = None;
        assert_eq!(
            verify_device_cert(&stripped),
            Err(DeviceCertError::BadSignature)
        );
        let mut moved = cert.clone();
        moved.capabilities.workspace = Some(OTHER_WORKSPACE);
        assert_eq!(verify_device_cert(&moved), Err(DeviceCertError::BadSignature));
        // The bytes a build of layout 1 would be handed: the capabilities' first byte is the
        // workspace marker, which that build reads as a capability flag of neither 0 nor 1.
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        let (_, rest) =
            postcard::take_from_bytes::<([u8; 32], [u8; 32], String)>(&bytes).unwrap();
        assert_eq!(rest.first(), Some(&WORKSPACE_BINARY_MARKER));
        assert!(rest.first().is_some_and(|byte| *byte > 1));
        assert_eq!(rest.get(1..17), Some(&VM_WORKSPACE[..]));
        // Cutting the workspace out of the bytes reads an unrestricted device whose signature
        // does not hold.
        let mut cut = bytes[..bytes.len() - rest.len()].to_vec();
        cut.extend_from_slice(&rest[17..]);
        let cut: DeviceCert = crate::canonical::from_canonical_bytes(&cut).unwrap();
        assert_eq!(cut.capabilities.workspace, None);
        assert_eq!(verify_device_cert(&cut), Err(DeviceCertError::BadSignature));
    }

    phase it_grants_nothing_else_and_issues_nothing;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        for wider in [
            DeviceCapabilities {
                can_own_workspaces: true,
                ..DeviceCapabilities::workspace_device(VM_WORKSPACE)
            },
            DeviceCapabilities {
                can_admit_devices: true,
                ..DeviceCapabilities::workspace_device(VM_WORKSPACE)
            },
            DeviceCapabilities {
                workspace: Some(VM_WORKSPACE),
                ..DeviceCapabilities::agent_grant(AgentScope::Workspace(VM_WORKSPACE))
            },
        ] {
            let mut cert = root_signed_device(&root, &key, DeviceCapabilities::member(), None);
            cert.capabilities = wider;
            assert!(sign_device_cert(&mut cert, &root).is_err());
            assert!(verify_device_cert(&forced(cert, &root)).is_err());
        }
        // Foreign narrows it further, and is allowed.
        root_signed_device(
            &root,
            &key,
            DeviceCapabilities {
                foreign: true,
                ..DeviceCapabilities::workspace_device(VM_WORKSPACE)
            },
            None,
        );
        // It signs no browser in as a device of its person's.
        let vm = root_signed_device(
            &root,
            &key,
            DeviceCapabilities::workspace_device(VM_WORKSPACE),
            None,
        );
        let mut tab = DeviceCert {
            user_public_key: vm.user_public_key,
            device_public_key: [9; 32],
            device_label: "Browser".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 2_000,
            expires_at_ns: Some(3_000),
            signature: [0; 64],
            issuer: Some(Box::new(vm.clone())),
        };
        assert_eq!(
            sign_device_cert(&mut tab, &key),
            Err(DeviceCertError::InvalidIssuer)
        );
        // Nor a workspace service.
        let service = prepare_service_cert(&vm, [9; 32], "service", 2_000);
        assert!(verify_device_cert(&forced(service, &key)).is_err());
        // A renewal keeps it to its workspace.
        let unconfined = root_signed_device(&root, &key, DeviceCapabilities::nothing(), None);
        assert!(!unconfined.capabilities.grants_no_more_than(&vm.capabilities));
        assert!(!vm.capabilities.grants_no_more_than(&unconfined.capabilities));
    }

    phase a_certificate_the_previous_layout_wrote_reads_verifies_and_writes_as_it_was;
    // Every person's device holds certificates layout 1 wrote: each reads in this layout as the
    // same certificate, verifies, and writes back to the very bytes its signature covers.
    {
        let kept = hex::decode(GOLDEN_V1_POSTCARD).unwrap();
        assert_eq!(binary_layout_of(&kept).unwrap(), BINARY_LAYOUT);
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&kept).unwrap();
        verify_device_cert(&read).unwrap();
        assert_eq!(read.capabilities.workspace, None);
        assert!(read.capabilities.reaches_workspace(&uuid_text(&OTHER_WORKSPACE)));
        assert_eq!(crate::canonical::canonical_bytes(&read).unwrap(), kept);
        let json: DeviceCert = serde_json::from_str(GOLDEN_V1_JSON).unwrap();
        assert_eq!(serde_json::to_string(&json).unwrap(), GOLDEN_V1_JSON);
    }
}

/// The records a placed device's tests certify it for: two of the confined VM's workspace, on
/// channels of their own.
fn placed_records() -> PlacedRecords {
    PlacedRecords::new(VM_WORKSPACE, [[0x44; 32], [0x33; 32]]).unwrap()
}

/// A host that is one of its person's devices: root `[1; 32]` certified its key `[3; 32]` at
/// `1_000` for thirty days.
fn host_device() -> (ed25519_dalek::SigningKey, DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let cert = root_signed_device(
        &root,
        &key,
        DeviceCapabilities::member(),
        Some(1_000 + STAND_IN_LIFETIME_NS),
    );
    (key, cert)
}

/// The records a placement grant's tests give a host: the placed device's two, and one more.
fn granted_records() -> PlacedRecords {
    PlacedRecords::new(VM_WORKSPACE, [[0x33; 32], [0x44; 32], [0x55; 32]]).unwrap()
}

/// A host that is none of its person's devices, key `[5; 32]`, under the placement grant root
/// `[1; 32]` signed it for [`granted_records`] at `1_000`, ending at `expires_at_ns`.
fn granted_host(expires_at_ns: Option<u64>) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
    let mut grant = prepare_placing_grant(
        root.verifying_key().to_bytes(),
        key.verifying_key().to_bytes(),
        granted_records(),
        "tilo.example",
        1_000,
        expires_at_ns,
    );
    sign_device_cert(&mut grant, &root).unwrap();
    (key, grant)
}

/// An unsigned certificate the host whose certificate is `issuer` issues for device `[9; 32]`,
/// reaching `capabilities`, from `issued_at_ns` for `lifetime_ns`.
fn placed_device_of(
    issuer: &DeviceCert,
    capabilities: DeviceCapabilities,
    issued_at_ns: u64,
    lifetime_ns: u64,
) -> DeviceCert {
    DeviceCert {
        capabilities,
        ..prepare_placed_device_cert(
            issuer,
            [9; 32],
            placed_records(),
            "Placed",
            issued_at_ns,
            issued_at_ns.saturating_add(lifetime_ns),
        )
    }
}

tilo_test_env::scenario! {
    fn a_placed_device_reaches_its_records_alone;

    phase its_records_are_sorted_once_each_and_bounded;
    {
        let records = placed_records();
        assert_eq!(records.channels(), &[[0x33; 32], [0x44; 32]]);
        assert_eq!(
            PlacedRecords::new(VM_WORKSPACE, [[0x44; 32], [0x33; 32], [0x44; 32]]).unwrap(),
            records,
            "a channel named twice is one record"
        );
        assert!(records.reaches(&[0x33; 32]) && records.reaches(&[0x44; 32]));
        assert!(!records.reaches(&[0x55; 32]));
        assert_eq!(records.workspace(), &VM_WORKSPACE);
        assert_eq!(records.workspace_name(), uuid_text(&VM_WORKSPACE));
        for nothing in [
            PlacedRecords::new(VM_WORKSPACE, []),
            PlacedRecords::new([0; 16], [[0x33; 32]]),
            PlacedRecords::new(VM_WORKSPACE, [[0x33; 32], [0; 32]]),
            PlacedRecords::new(VM_WORKSPACE, (1..=MAX_PLACED_RECORDS + 1).map(|at| [u8::try_from(at).unwrap(); 32])),
        ] {
            assert_eq!(nothing, Err(DeviceCertError::InvalidPlacedDevice));
        }
        let most = PlacedRecords::new(
            VM_WORKSPACE,
            (1..=MAX_PLACED_RECORDS).map(|at| [u8::try_from(at).unwrap(); 32]),
        )
        .unwrap();
        let (key, host) = host_device();
        let mut cert = placed_device_of(&host, DeviceCapabilities::placed(most), 2_000, 60);
        sign_device_cert(&mut cert, &key).unwrap();
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(read, cert, "the most records a certificate names read back");
        verify_device_cert(&read).unwrap();
    }

    phase it_round_trips_and_verifies_in_the_current_layout;
    {
        let (key, host) = host_device();
        let mut cert = placed_device_of(
            &host,
            DeviceCapabilities::placed(placed_records()),
            2_000,
            MAX_PLACED_DEVICE_LIFETIME_NS,
        );
        sign_device_cert(&mut cert, &key).unwrap();
        verify_device_cert(&cert).unwrap();
        verify_device_cert_at(&cert, 3_000).unwrap();
        assert_eq!(
            verify_device_cert_at(&cert, 2_000 + MAX_PLACED_DEVICE_LIFETIME_NS),
            Err(DeviceCertError::Expired),
            "it lasts no longer than its bound"
        );
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        assert_eq!(binary_layout_of(&bytes).unwrap(), BINARY_LAYOUT);
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(read, cert);
        verify_device_cert(&read).unwrap();
        let text = serde_json::to_string(&cert).unwrap();
        let channels = format!(r#""channels":["{}","{}"]"#, "33".repeat(32), "44".repeat(32));
        assert!(text.contains(&format!(
            r#""records":{{"workspace":"11111111-1111-1111-1111-111111111111",{channels}}}"#
        )));
        let json: DeviceCert = serde_json::from_str(&text).unwrap();
        assert_eq!(json, cert);
        // JSON naming its records out of order, or one twice, is no certificate.
        for wrong in [
            format!(r#""channels":["{}","{}"]"#, "44".repeat(32), "33".repeat(32)),
            format!(r#""channels":["{}","{}"]"#, "33".repeat(32), "33".repeat(32)),
            r#""channels":[]"#.to_owned(),
        ] {
            assert!(serde_json::from_str::<DeviceCert>(&text.replace(&channels, &wrong)).is_err());
        }
        let vm = uuid_text(&VM_WORKSPACE);
        assert!(cert.capabilities.reaches_workspace(&vm));
        assert!(!cert.capabilities.reaches_workspace(&uuid_text(&OTHER_WORKSPACE)));
        assert_eq!(cert.capabilities.workspace_name().as_deref(), Some(vm.as_str()));
        assert_eq!(cert.capabilities.placed_in(&vm), Some(&placed_records()));
        assert_eq!(cert.capabilities.placed_in(&uuid_text(&OTHER_WORKSPACE)), None);
        // No device of its person's: it adds no devices, keeps no workspace's keys, owns none,
        // and a renewal keeps it to its records.
        assert!(!cert.is_root_signed_device());
        assert!(!cert.capabilities.holds_workspace_keys());
        assert!(!cert.capabilities.is_personal_device());
        assert!(!DeviceCapabilities::nothing().grants_no_more_than(&cert.capabilities));
        assert!(!cert.capabilities.grants_no_more_than(&DeviceCapabilities::member()));
        let fewer = DeviceCapabilities::placed(PlacedRecords::new(VM_WORKSPACE, [[0x33; 32]]).unwrap());
        assert!(
            !fewer.grants_no_more_than(&cert.capabilities),
            "a renewal names exactly the records it renews"
        );
    }

    phase the_restriction_cannot_be_stripped_or_moved_and_an_older_layout_refuses_it;
    {
        let (key, host) = host_device();
        let mut cert = placed_device_of(
            &host,
            DeviceCapabilities::placed(placed_records()),
            2_000,
            MAX_PLACED_DEVICE_LIFETIME_NS,
        );
        sign_device_cert(&mut cert, &key).unwrap();
        let mut stripped = cert.clone();
        stripped.capabilities.records = None;
        assert!(verify_device_cert(&stripped).is_err());
        let mut moved = cert.clone();
        moved.capabilities.records =
            Some(PlacedRecords::new(VM_WORKSPACE, [[0x33; 32], [0x55; 32]]).unwrap());
        assert_eq!(verify_device_cert(&moved), Err(DeviceCertError::BadSignature));
        let mut widened = cert.clone();
        widened.capabilities.records =
            Some(PlacedRecords::new(VM_WORKSPACE, [[0x33; 32], [0x44; 32], [0x55; 32]]).unwrap());
        assert_eq!(verify_device_cert(&widened), Err(DeviceCertError::BadSignature));
        // The bytes a build of layout 2 would be handed: the capabilities' first byte is the
        // placed marker, which that build names a later layout and reads as a capability flag of
        // neither 0 nor 1.
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        let (_, rest) =
            postcard::take_from_bytes::<([u8; 32], [u8; 32], String)>(&bytes).unwrap();
        assert_eq!(rest.first(), Some(&PLACED_BINARY_MARKER));
        assert!(rest.first().is_some_and(|byte| *byte > WORKSPACE_BINARY_MARKER));
        assert_eq!(rest.get(1..17), Some(&VM_WORKSPACE[..]));
        assert_eq!(rest.get(17), Some(&2), "a count of its records");
        assert_eq!(rest.get(18..50), Some(&[0x33; 32][..]));
        assert_eq!(rest.get(50..82), Some(&[0x44; 32][..]));
        let at = bytes.len() - rest.len();
        // Cutting the records out of the bytes reads no valid certificate.
        let mut cut = bytes[..at].to_vec();
        cut.extend_from_slice(&rest[82..]);
        let cut = crate::canonical::from_canonical_bytes::<DeviceCert>(&cut);
        assert!(cut.is_err() || verify_device_cert(&cut.unwrap()).is_err());
        // Neither a workspace or channel of nothing, nor records out of order or named twice,
        // nor a count of none or above the bound, reads.
        let edited = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = bytes.clone();
            edit(&mut bytes);
            crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes)
        };
        assert!(edited(&|bytes| bytes[at + 1..at + 17].fill(0)).is_err());
        assert!(edited(&|bytes| bytes[at + 18..at + 50].fill(0)).is_err());
        assert!(edited(&|bytes| bytes[at + 50..at + 82].fill(0x33)).is_err());
        assert!(edited(&|bytes| bytes[at + 18..at + 82].rotate_left(32)).is_err());
        assert!(edited(&|bytes| bytes[at + 17] = 0).is_err());
        assert!(edited(&|bytes| bytes[at + 17] = u8::try_from(MAX_PLACED_RECORDS + 1).unwrap()).is_err());
        // A count that names fewer records than follow leaves bytes no extended form reads.
        assert!(edited(&|bytes| bytes[at + 17] = 1).is_err());
        // Nor does one followed by any form but the extended one.
        assert!(edited(&|bytes| bytes[at + 82] = FOREIGN_BINARY_MARKER).is_err());
    }

    phase it_is_its_hosts_alone_and_grants_nothing_else;
    {
        let (key, host) = host_device();
        let refused = |cert: DeviceCert, signer: &ed25519_dalek::SigningKey| {
            let mut signed = cert.clone();
            let refusal = sign_device_cert(&mut signed, signer).unwrap_err();
            assert!(verify_device_cert(&forced(cert, signer)).is_err());
            refusal
        };
        // Its person's root does not sign one itself: the host that places the device does, with
        // its own key.
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut by_root =
            placed_device_of(&host, DeviceCapabilities::placed(placed_records()), 2_000, 60);
        by_root.issuer = None;
        assert_eq!(refused(by_root, &root), DeviceCertError::InvalidIssuer);
        // No key signs in the person's place: their stand-in places nothing.
        let (tab_key, tab_signer) = make_stand_in(1);
        let by_tab_key =
            placed_device_of(&tab_signer, DeviceCapabilities::placed(placed_records()), 2_000, 60);
        assert_eq!(refused(by_tab_key, &tab_key), DeviceCertError::InvalidPlacedDevice);
        // Nor does an agent's grant, or a device another person's root certified.
        let (grant_key, grant) = make_grant(DAY_NS);
        let by_grant =
            placed_device_of(&grant, DeviceCapabilities::placed(placed_records()), 2_000, 60);
        assert!(matches!(
            refused(by_grant, &grant_key),
            DeviceCertError::InvalidPlacedDevice | DeviceCertError::InvalidIssuer
        ));
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        let theirs = root_signed_device(&stranger, &key, DeviceCapabilities::member(), None);
        let mut by_stranger =
            placed_device_of(&theirs, DeviceCapabilities::placed(placed_records()), 2_000, 60);
        by_stranger.user_public_key = host.user_public_key;
        assert_eq!(refused(by_stranger, &key), DeviceCertError::InvalidPlacedDevice);
        // It grants nothing beside its records.
        for wider in [
            DeviceCapabilities {
                can_own_workspaces: true,
                ..DeviceCapabilities::placed(placed_records())
            },
            DeviceCapabilities {
                foreign: true,
                ..DeviceCapabilities::placed(placed_records())
            },
            DeviceCapabilities {
                agent: Some(AgentScope::Workspace(VM_WORKSPACE)),
                ..DeviceCapabilities::placed(placed_records())
            },
            DeviceCapabilities {
                records: Some(placed_records()),
                ..DeviceCapabilities::member()
            },
        ] {
            refused(placed_device_of(&host, wider, 2_000, 60), &key);
        }
        // A workspace and records together are neither signed nor written.
        let both = placed_device_of(
            &host,
            DeviceCapabilities {
                workspace: Some(VM_WORKSPACE),
                ..DeviceCapabilities::placed(placed_records())
            },
            2_000,
            60,
        );
        assert!(crate::canonical::canonical_bytes(&both).is_err());
        let mut signed = both;
        assert!(sign_device_cert(&mut signed, &key).is_err());
        // Records of nothing, or not in their one order, are none: built past
        // `PlacedRecords::new`, they are neither signed nor written.
        for nothing in [
            PlacedRecords { workspace: [0; 16], channels: vec![[0x33; 32]] },
            PlacedRecords { workspace: VM_WORKSPACE, channels: vec![[0; 32]] },
            PlacedRecords { workspace: VM_WORKSPACE, channels: vec![] },
            PlacedRecords { workspace: VM_WORKSPACE, channels: vec![[0x44; 32], [0x33; 32]] },
            PlacedRecords { workspace: VM_WORKSPACE, channels: vec![[0x33; 32], [0x33; 32]] },
        ] {
            let mut cert =
                placed_device_of(&host, DeviceCapabilities::placed(nothing), 2_000, 60);
            assert!(crate::canonical::canonical_bytes(&cert).is_err());
            assert_eq!(
                sign_device_cert(&mut cert, &key),
                Err(DeviceCertError::InvalidPlacedDevice)
            );
        }
    }

    phase it_always_expires_within_its_bound_and_its_issuer;
    {
        let (key, host) = host_device();
        let placed = DeviceCapabilities::placed(placed_records());
        let mut forever = placed_device_of(&host, placed.clone(), 2_000, 60);
        forever.expires_at_ns = None;
        let mut signed = forever.clone();
        assert_eq!(
            sign_device_cert(&mut signed, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        assert!(verify_device_cert(&forced(forever, &key)).is_err());
        let too_long = placed_device_of(
            &host,
            placed.clone(),
            2_000,
            MAX_PLACED_DEVICE_LIFETIME_NS + 1,
        );
        let mut signed = too_long;
        assert_eq!(
            sign_device_cert(&mut signed, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        // It ends no later than the device that issued it.
        let issuer_ends = host.expires_at_ns.unwrap();
        let mut past_issuer = placed_device_of(&host, placed.clone(), issuer_ends - 10, 60);
        assert_eq!(
            sign_device_cert(&mut past_issuer, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        // Nor is it issued before that device was.
        let mut before = placed_device_of(&host, placed.clone(), 500, 60);
        assert_eq!(
            sign_device_cert(&mut before, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        // Nor for the issuing device's own key, or another person's.
        let mut own = placed_device_of(&host, placed.clone(), 2_000, 60);
        own.device_public_key = host.device_public_key;
        assert_eq!(
            sign_device_cert(&mut own, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        let mut other_person = placed_device_of(&host, placed, 2_000, 60);
        other_person.user_public_key = [7; 32];
        assert!(sign_device_cert(&mut other_person, &key).is_err());
    }
}

tilo_test_env::scenario! {
    fn a_placement_grant_lets_a_host_place_its_records_alone;

    phase the_grant_is_no_device_and_reads_back_as_signed;
    {
        let (_, grant) = granted_host(None);
        verify_placing_grant_at(&grant, 2_000).unwrap();
        assert_eq!(
            verify_device_cert(&grant),
            Err(DeviceCertError::PlacingGrantIsNoDevice),
            "the host it was given to is none of the person's devices"
        );
        assert!(!grant.is_root_signed_device());
        assert!(!grant.capabilities.is_personal_device());
        assert!(!grant.capabilities.holds_workspace_keys());
        let bytes = crate::canonical::canonical_bytes(&grant).unwrap();
        let (_, rest) =
            postcard::take_from_bytes::<([u8; 32], [u8; 32], String)>(&bytes).unwrap();
        assert_eq!(rest.first(), Some(&PLACING_BINARY_MARKER));
        assert_eq!(binary_layout_of(&bytes).unwrap(), BINARY_LAYOUT);
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(read, grant);
        let json: DeviceCert = serde_json::from_str(&serde_json::to_string(&grant).unwrap()).unwrap();
        assert_eq!(json, grant);
        // It grants nothing beside its records, and is never a device's grant to itself.
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut wider = grant.clone();
        wider.capabilities.can_own_workspaces = true;
        assert_eq!(
            sign_device_cert(&mut wider, &root),
            Err(DeviceCertError::InvalidPlacingGrant)
        );
        let mut for_root = grant.clone();
        for_root.device_public_key = grant.user_public_key;
        assert_eq!(
            sign_device_cert(&mut for_root, &root),
            Err(DeviceCertError::InvalidPlacingGrant)
        );
    }

    phase the_host_places_records_within_it_and_no_others;
    {
        let (key, grant) = granted_host(Some(1_000 + DAY_NS));
        let mut cert = placed_device_of(
            &grant,
            DeviceCapabilities::placed(placed_records()),
            2_000,
            MAX_PLACED_DEVICE_LIFETIME_NS,
        );
        sign_device_cert(&mut cert, &key).unwrap();
        verify_device_cert_at(&cert, 3_000).unwrap();
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(read, cert);
        verify_device_cert(&read).unwrap();
        // A record the grant does not name, or another workspace's, is not the host's to place.
        let beyond = PlacedRecords::new(VM_WORKSPACE, [[0x33; 32], [0x66; 32]]).unwrap();
        let mut wider = placed_device_of(&grant, DeviceCapabilities::placed(beyond), 2_000, 60);
        assert_eq!(
            sign_device_cert(&mut wider, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        let elsewhere = PlacedRecords::new(OTHER_WORKSPACE, [[0x33; 32]]).unwrap();
        let mut moved = placed_device_of(&grant, DeviceCapabilities::placed(elsewhere), 2_000, 60);
        assert_eq!(
            sign_device_cert(&mut moved, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        // It ends no later than the grant.
        let mut past_grant =
            placed_device_of(&grant, DeviceCapabilities::placed(placed_records()), 1_000 + DAY_NS - 10, 60);
        assert_eq!(
            sign_device_cert(&mut past_grant, &key),
            Err(DeviceCertError::InvalidPlacedDevice)
        );
        // A grant does not certify a device of the person's, nor grant again.
        let mut member = placed_device_of(&grant, DeviceCapabilities::member(), 2_000, 60);
        assert!(sign_device_cert(&mut member, &key).is_err());
        let mut regranted = prepare_placing_grant(
            grant.user_public_key,
            [9; 32],
            placed_records(),
            "again",
            2_000,
            None,
        );
        regranted.issuer = Some(Box::new(grant));
        assert!(sign_device_cert(&mut regranted, &key).is_err());
    }

    phase a_device_of_the_persons_may_sign_the_grant_for_as_long_as_it_lasts;
    {
        let (laptop_key, laptop) = host_device();
        let host_key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        let laptop_ends = laptop.expires_at_ns.unwrap();
        let mut grant = prepare_placing_grant_by(
            &laptop,
            host_key.verifying_key().to_bytes(),
            granted_records(),
            "tilo.example",
            2_000,
            Some(laptop_ends),
        );
        sign_device_cert(&mut grant, &laptop_key).unwrap();
        verify_placing_grant_at(&grant, 3_000).unwrap();
        let mut cert = placed_device_of(&grant, DeviceCapabilities::placed(placed_records()), 3_000, 60);
        sign_device_cert(&mut cert, &host_key).unwrap();
        verify_device_cert(&cert).unwrap();
        // Two steps read back, as bytes and as JSON.
        let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
        let read: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(read, cert);
        let json: DeviceCert = serde_json::from_str(&serde_json::to_string(&cert).unwrap()).unwrap();
        assert_eq!(json, cert);
        // A grant past the device that signed it, or one a stand-in signed, is none.
        let mut forever = prepare_placing_grant_by(
            &laptop,
            host_key.verifying_key().to_bytes(),
            granted_records(),
            "tilo.example",
            2_000,
            None,
        );
        assert_eq!(
            sign_device_cert(&mut forever, &laptop_key),
            Err(DeviceCertError::InvalidPlacingGrant)
        );
        let (tab_key, tab_signer) = make_stand_in(1);
        let mut by_tab_key = prepare_placing_grant_by(
            &tab_signer,
            host_key.verifying_key().to_bytes(),
            granted_records(),
            "tilo.example",
            2_000,
            Some(3_000),
        );
        assert_eq!(
            sign_device_cert(&mut by_tab_key, &tab_key),
            Err(DeviceCertError::InvalidPlacingGrant)
        );
        // Three steps never read: a device certificate a device issued names no issuer that
        // names one again.
        let mut three = cert.clone();
        if let Some(grant) = three.issuer.as_mut()
            && let Some(device) = grant.issuer.as_mut()
        {
            device.issuer = Some(Box::new(laptop));
        }
        let bytes = crate::canonical::canonical_bytes(&three).unwrap();
        assert!(crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).is_err());
        assert!(serde_json::from_str::<DeviceCert>(&serde_json::to_string(&three).unwrap()).is_err());
    }

    phase revoking_the_grant_or_the_device_that_signed_it_ends_what_it_placed;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let revoked = |key: [u8; 32]| {
            let mut entry = crate::RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: key,
                revoked_at_ns: 5_000,
                reason: crate::RevocationReason::Retired,
                signature: [0; 64],
            };
            crate::sign_revocation_entry(&mut entry, &root).unwrap();
            let mut revocations = crate::RevocationList::new();
            revocations.insert(entry).unwrap();
            revocations
        };
        let (key, grant) = granted_host(None);
        let mut cert = placed_device_of(&grant, DeviceCapabilities::placed(placed_records()), 2_000, 60);
        sign_device_cert(&mut cert, &key).unwrap();
        assert!(!crate::RevocationList::new().refuses(&cert));
        // Whatever it placed before the revocation ends with it.
        assert!(revoked(grant.device_public_key).refuses(&cert));
        let (laptop_key, laptop) = host_device();
        let mut by_laptop = prepare_placing_grant_by(
            &laptop,
            key.verifying_key().to_bytes(),
            granted_records(),
            "tilo.example",
            2_000,
            laptop.expires_at_ns,
        );
        sign_device_cert(&mut by_laptop, &laptop_key).unwrap();
        let mut under = placed_device_of(&by_laptop, DeviceCapabilities::placed(placed_records()), 3_000, 60);
        sign_device_cert(&mut under, &key).unwrap();
        assert!(revoked(laptop.device_public_key).refuses(&by_laptop));
        assert!(revoked(laptop.device_public_key).refuses(&under));
        // A host that is the person's device places nothing once they revoke it.
        let (host_key, host) = host_device();
        let mut placed = placed_device_of(&host, DeviceCapabilities::placed(placed_records()), 2_000, 60);
        sign_device_cert(&mut placed, &host_key).unwrap();
        assert!(revoked(host.device_public_key).refuses(&placed));
    }
}
