use super::*;
use tilo_enrollment::approval::{ApprovalOptions, register_home_peer};
use tilo_enrollment::client::PairingCause;
use tilo_enrollment::protocol::{
    Approver, PAIR_MAGIC, Request, Response, ResponseRoute, decrypt_pair_offer, decrypt_request,
    decrypt_response, encrypt_request, encrypt_response, make_ack, make_finalized, v2_body,
    verify_ack, verify_finalized,
};
use tilo_enrollment::request::{install_answer, prepare_join};

use tilo_enrollment::ISSUANCE_CLOCK_ALLOWANCE_NS;
use tilo_enrollment::sign_response;
use tilo_enrollment::{WindowClosed, open_answer, response_statement_digest, verify_response};
use tilo_identity::certification_ceremony::pairing_code;
use tilo_identity::device_fleet::DeviceFleetStore;
use tilo_identity::enrollment::NonceStore;
use tilo_server::HomePeerRegistration;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::verify_device_cert;
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert, sign_revocation_entry,
};

/// The fleet document of `identity_id` a device hands another: `certificates`
/// and `revocations`, as an approving device's fleet holds them.
fn fleet_document(
    identity_id: &str,
    certificates: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
) -> Vec<u8> {
    let mut fleet = DeviceFleet::new(identity_id.to_owned());
    fleet.certificates = certificates;
    fleet.revocations = revocations;
    serde_json::to_vec_pretty(&fleet).unwrap()
}

#[test]
fn a_refused_signature_issues_no_enrollment_response() {
    struct RefusingRoot;

    impl tilo_user_identity::StatementSigner for RefusingRoot {
        fn public_key(&self) -> [u8; 32] {
            [1; 32]
        }

        fn sign_digest(
            &self,
            _digest: &[u8; 32],
        ) -> Result<[u8; 64], tilo_user_identity::SignError> {
            Err(tilo_user_identity::SignError("locked".into()))
        }
    }

    impl tilo_server::root_signer::RootSigner for RefusingRoot {}

    let refusing = RefusingRoot;
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    assert!(
        sign_response(payload(&root), Approver::Root(&refusing)).is_err(),
        "a refusing root issues no enrollment response"
    );
}

tilo_test_env::scenario! {
    fn certificates_and_fleets_admit_only_what_the_identity_signed;

    phase host_record_approval_keeps_admission_and_revocation_checks;
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tilo");
        let identities = temp.path().join("identities");
        let paths = IdentityPaths::under(&root);
        tilo_identity::provisioning::ensure_collaboration_identity(
            &identities,
            &paths,
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let options = ApprovalOptions {
            identity_root: &root,
            identity_dir: Some(&identities),
            timeout: 1,
            workspace_cache: temp.path(),
            acl: temp.path(),
            server_workspace_ids: &[],
            expected_device: None,
            expected_service: None,
            kind: EnrollmentKind::HomePeer,
            port: 0,
            ready: None,
            approved: None,
            person_uid: None,
        };
        let identity = load_approval_identity(&options).unwrap();
        let mut cert = payload(&ed25519_dalek::SigningKey::from_bytes(&[74; 32])).cert;
        cert.user_public_key = identity.user_public_key;
        sign_root_cert(&mut cert, identity.root.as_deref().unwrap()).unwrap();
        verify_device_cert(&cert).unwrap();
        let mut revocation = RevocationEntry {
            user_public_key: identity.user_public_key,
            revoked_device_public_key: identity.device.verifying_key().to_bytes(),
            revoked_at_ns: 1,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, identity.root.as_deref().unwrap()).unwrap();
        let store = fleet_store(&root, &identities, None);
        let identity_id = hex::encode(identity.user_public_key);
        let mut fleet = store.load(&identity_id).unwrap();
        fleet.revocations.push(revocation);
        store.adopt_transferred(&fleet).unwrap();
        let refused = |error: anyhow::Error, why: &str| {
            assert!(format!("{error:#}").contains(why), "{error:#}");
        };
        refused(load_approval_identity(&options).err().unwrap(), "revoked");
        fleet.revocations.clear();
        store.adopt_transferred(&fleet).unwrap();
        let mut issuer = tilo_device_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        issuer.capabilities = DeviceCapabilities::member();
        sign_root_cert(&mut issuer, identity.root.as_deref().unwrap()).unwrap();
        tilo_device_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&identity.user_public_key),
            &issuer,
        )
        .unwrap();
        refused(
            load_approval_identity(&options).err().unwrap(),
            "not authorised",
        );
        issuer.capabilities = DeviceCapabilities::primary();
        issuer.issued_at_ns = 1;
        issuer.expires_at_ns = Some(2);
        sign_root_cert(&mut issuer, identity.root.as_deref().unwrap()).unwrap();
        tilo_device_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&identity.user_public_key),
            &issuer,
        )
        .unwrap();
        refused(load_approval_identity(&options).err().unwrap(), "validity");
        let unrelated_root = ed25519_dalek::SigningKey::from_bytes(&[76; 32]);
        issuer.expires_at_ns = None;
        issuer.user_public_key = unrelated_root.verifying_key().to_bytes();
        sign_device_cert(&mut issuer, &unrelated_root).unwrap();
        tilo_device_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&identity.user_public_key),
            &issuer,
        )
        .unwrap();
        refused(
            load_approval_identity(&options).err().unwrap(),
            "not authorised",
        );
    }

    phase a_held_root_certifies_an_administrator_into_the_fleet;
    // The root a device holds certifies a developer's machine for
    // administration and lists it in the person's fleet, where Settings can
    // remove it; an unknown capability names the ones there are.
    {
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        paths.ensure_directories().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        tilo_identity::provisioning::import_single_device_root(
            identity_dir.path(),
            &root,
            "",
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let admin = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
        let device = hex::encode(admin.verifying_key().to_bytes());
        let out = identity_dir.path().join("admin.cert");
        let options = |capabilities| CertifyOptions {
            root_key: None,
            identity_root: identity_root.path(),
            identity_dir: Some(identity_dir.path()),
            device: &device,
            label: "workstation (tilo-vm)",
            capabilities,
            out: &out,
        };
        let shell = ["shell".to_owned()];
        let unknown = certify(&options(&shell)).unwrap_err();
        assert!(unknown.to_string().contains("admit or admin"), "{unknown}");

        let admin_only = ["admin".to_owned()];
        certify(&options(&admin_only)).unwrap();
        let cert = tilo_device_identity::persist::read_device_cert(&out).unwrap();
        verify_device_cert(&cert).unwrap();
        assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(cert.capabilities, DeviceCapabilities::administrator());
        let fleet = fleet_store(identity_root.path(), identity_dir.path(), None)
            .load(&hex::encode(cert.user_public_key))
            .unwrap();
        assert_eq!(fleet.certificates, vec![cert]);
    }

    phase an_out_of_band_certificate_joins_its_identity;
    {
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        device_key(identity_root.path()).unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        let device = tilo_device_identity::persist::read_device_key(&paths.device_privkey).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let user = root.verifying_key().to_bytes();
        let issue = |device_public_key: [u8; 32], root: &ed25519_dalek::SigningKey| {
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key,
                device_label: "Node".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            sign_device_cert(&mut cert, root).unwrap();
            let path = identity_dir
                .path()
                .join(format!("{}.cert", hex::encode(&cert.signature[..8])));
            tilo_device_identity::persist::write_device_cert(&path, &cert).unwrap();
            path
        };

        let other_device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let foreign = issue(other_device.verifying_key().to_bytes(), &root);
        assert!(install_cert(identity_root.path(), identity_dir.path(), &foreign).is_err());
        assert!(!paths.device_cert.exists());

        let root_key = identity_dir.path().join("root.key");
        tilo_device_identity::persist::write_signing_key(&root_key, &root).unwrap();
        let own = identity_dir.path().join("own.cert");
        let capabilities = ["own"].map(String::from);
        certify(&CertifyOptions {
            root_key: Some(&root_key),
            identity_root: identity_root.path(),
            identity_dir: None,
            device: &hex::encode(device.verifying_key().to_bytes()),
            label: "Node",
            capabilities: &capabilities,
            out: &own,
        })
        .unwrap();
        install_cert(identity_root.path(), identity_dir.path(), &own).unwrap();
        // Delivered again (a restart before the launcher noticed): unchanged.
        install_cert(identity_root.path(), identity_dir.path(), &own).unwrap();
        let installed = tilo_device_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        assert_eq!(installed.user_public_key, user);
        assert!(installed.capabilities.can_own_workspaces && !installed.capabilities.can_admit_devices);
        assert_eq!(
            tilo_device_identity::persist::read_public_key(&paths.user_pubkey).unwrap(),
            user
        );
        let record = tilo_identity::store::resolve_active_identity(identity_dir.path()).unwrap();
        let record = tilo_identity::store::load_from_path(record).unwrap();
        assert_eq!(record.group_public_key(), user);
        assert!(record.sign(b"root-only operation").is_err());

        let stranger = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        let takeover = issue(device.verifying_key().to_bytes(), &stranger);
        assert!(install_cert(identity_root.path(), identity_dir.path(), &takeover).is_err());
        assert_eq!(
            tilo_device_identity::persist::read_public_key(&paths.user_pubkey).unwrap(),
            user
        );
    }

    phase a_joining_device_records_only_certified_live_personal_devices;
    // A joining device records only the personal devices whose certificates
    // verify under the identity it joined, are valid now and are not
    // revoked; a malformed fleet records nothing.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
        let user = root.verifying_key().to_bytes();
        let cert =
            |device: u8, capabilities: DeviceCapabilities, signer: &ed25519_dalek::SigningKey| {
                let mut cert = DeviceCert {
                    user_public_key: signer.verifying_key().to_bytes(),
                    device_public_key: [device; 32],
                    device_label: format!("Device {device}"),
                    capabilities,
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                };
                sign_device_cert(&mut cert, signer).unwrap();
                cert
            };
        let approver = cert(1, DeviceCapabilities::primary(), &root);
        let sibling = cert(2, DeviceCapabilities::member(), &root);
        let revoked = cert(3, DeviceCapabilities::member(), &root);
        let administrator = cert(5, DeviceCapabilities::administrator(), &root);
        let foreign_root = cert(6, DeviceCapabilities::member(), &stranger);
        let mut forged = cert(7, DeviceCapabilities::member(), &root);
        forged.device_public_key = [8; 32];
        let mut expired = cert(9, DeviceCapabilities::member(), &root);
        expired.expires_at_ns = Some(2);
        sign_device_cert(&mut expired, &root).unwrap();
        let mut revocation = RevocationEntry {
            user_public_key: user,
            revoked_device_public_key: revoked.device_public_key,
            revoked_at_ns: 2,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &root).unwrap();
        let fleet = |certificates: Vec<DeviceCert>, revocations: Vec<RevocationEntry>| {
            fleet_document(&hex::encode(user), certificates, revocations)
        };
        let listed = || {
            vec![
                approver.clone(),
                sibling.clone(),
                revoked.clone(),
                administrator.clone(),
                foreign_root.clone(),
                expired.clone(),
            ]
        };
        let identity_root = tempfile::tempdir().unwrap();
        let identities = tempfile::tempdir().unwrap();
        let store = fleet_store(identity_root.path(), identities.path(), None);
        let accepting =
            |document: &[u8], identity: &str| store.accept_transferred(identity, document, [1; 32]);
        let now = now_ns().unwrap();
        let mut devices = vec![[10; 32]];
        devices.extend(
            accepting(
                &fleet(listed(), vec![revocation.clone()]),
                &hex::encode(user),
            )
            .unwrap()
            .personal_devices(&user, now),
        );
        assert_eq!(devices, vec![[10; 32], [1; 32], [2; 32]]);
        // A fleet that does not list the device it was handed to is not that
        // device's.
        assert!(
            store
                .accept_transferred(&hex::encode(user), &fleet(listed(), Vec::new()), [10; 32])
                .is_err()
        );

        // A fleet with a record that does not verify is no fleet this device
        // joins on.
        let mut forged_list = listed();
        forged_list.push(forged);
        assert!(accepting(&fleet(forged_list, Vec::new()), &hex::encode(user)).is_err());
        let mut tampered = revocation;
        tampered.revoked_device_public_key = [2; 32];
        assert!(accepting(&fleet(listed(), vec![tampered]), &hex::encode(user)).is_err());
        let other_identity = fleet_document(
            &hex::encode(stranger.verifying_key().to_bytes()),
            vec![approver],
            Vec::new(),
        );
        assert!(accepting(&other_identity, &hex::encode(user)).is_err());
        assert!(accepting(b"[]", &hex::encode(user)).is_err());
    }
}

/// A token `issuer` minted for the identity `user`.
fn token_from(
    user: [u8; 32],
    issuer: &ed25519_dalek::SigningKey,
) -> tilo_user_identity::EnrollmentToken {
    tilo_user_identity::mint_enrollment_token(
        user,
        DeviceCapabilities::member(),
        10,
        1_000,
        [3; 16],
        vec![],
        issuer,
    )
    .unwrap()
}

/// A token the device holding the identity key `root` minted.
fn token(root: &ed25519_dalek::SigningKey) -> tilo_user_identity::EnrollmentToken {
    token_from(root.verifying_key().to_bytes(), root)
}

fn payload(root: &ed25519_dalek::SigningKey) -> ResponsePayload {
    let user_public_key = root.verifying_key().to_bytes();
    let mut cert = DeviceCert {
        user_public_key,
        device_public_key: [7; 32],
        device_label: "NAS".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    ResponsePayload {
        nonce: [3; 16],
        recipient_device_public_key: [7; 32],
        server_node_id: [8; 32],
        cert,
        home_peer_toml: Some("status_file = '/var/lib/tilo/full-replica/status.json'".into()),
        public_identity_json: None,
        fleet_json: None,
        home_peers_json: None,
    }
}

/// The certificate `root`'s person signs for `device`.
fn root_signed(
    root: &ed25519_dalek::SigningKey,
    device: [u8; 32],
    label: &str,
    capabilities: DeviceCapabilities,
    issued_at_ns: u64,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device,
        device_label: label.into(),
        capabilities,
        issued_at_ns,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// A server's enrollment answers with the person's own store beside its
/// certificate: the machine keeps that list in its identity root, where the
/// devices it may ask at boot come from with no session of theirs running.
#[test]
fn a_home_peer_keeps_the_persons_devices_from_its_enrollment() {
    let dir = tempfile::tempdir().unwrap();
    let system = dir.path().join("system");
    let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let user = root.verifying_key().to_bytes();
    let identity_id = hex::encode(user);
    let issuer = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let issuer_cert = root_signed(
        &root,
        issuer.verifying_key().to_bytes(),
        "The laptop",
        DeviceCapabilities::primary(),
        1,
    );
    let now = now_ns().unwrap();
    let token = tilo_user_identity::mint_enrollment_token(
        user,
        DeviceCapabilities::member(),
        now,
        now + 60 * 60 * 1_000_000_000,
        [3; 16],
        Vec::new(),
        &issuer,
    )
    .unwrap();
    let token_text = tilo_user_identity::encode_enrollment_token(&token).unwrap();
    let home_peer_output = dir.path().join("home-peer.toml");
    let options = JoinOptions {
        identity_root: &system,
        label: "NAS",
        timeout: 60,
        kind: EnrollmentKind::HomePeer,
        home_peer_output: Some(&home_peer_output),
        identity_dir: None,
        requester_lease: None,
        port: 0,
        person_uid: None,
        recovery_key: None,
    };
    let prepared = prepare_join(&token_text, options).unwrap();
    let machine = prepared.device.verifying_key().to_bytes();
    let cert = root_signed(&root, machine, "NAS", DeviceCapabilities::member(), now);
    let home_peer = tilo_fs::full_replica::HomePeerSet {
        status_file: PathBuf::from("/var/lib/tilo/full-replica/status.json"),
        workspaces: Vec::new(),
    };
    let payload = ResponsePayload {
        nonce: token.nonce,
        recipient_device_public_key: machine,
        server_node_id: prepared.request.server_node_id,
        cert: cert.clone(),
        home_peer_toml: Some(toml::to_string_pretty(&home_peer).unwrap()),
        public_identity_json: None,
        fleet_json: Some(fleet_document(
            &identity_id,
            vec![issuer_cert, cert],
            Vec::new(),
        )),
        home_peers_json: Some(b"[]".to_vec()),
    };
    install_answer(&prepared, options, &payload, b"response", &mut |_| Ok(())).unwrap();

    assert!(
        home_peer_output.exists(),
        "the home-peer configuration is written"
    );
    let paths = IdentityPaths::under(&system);
    assert!(
        tilo_device_identity::persist::read_device_cert(&paths.device_cert)
            .is_ok_and(|written| written.device_public_key == machine),
        "the machine holds its certificate"
    );
    let store = DeviceFleetStore::of_device(paths, &identity_id);
    let kept = store
        .load(&identity_id)
        .expect("the person's devices are kept");
    assert!(
        kept.certificates
            .iter()
            .any(|known| known.device_public_key == machine),
        "the machine's own certificate is among them"
    );
    assert_eq!(
        kept.personal_devices(&user, now).len(),
        2,
        "the machine and the device that admitted it"
    );
    assert!(
        store.fleet_file(&identity_id).starts_with(system.as_path()),
        "kept in this machine's identity root, not an account's configuration"
    );
}

tilo_test_env::scenario! {
    fn enrollment_responses_are_signed_encrypted_and_certificate_bound;

    phase expired_device_certificate_is_not_projected_into_personal_state;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[74; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Expired laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: Some(2),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let identity = tempfile::tempdir().unwrap();
        let store = fleet_store(identity.path(), identity.path(), None);

        let error = store
            .transferred(&hex::encode(cert.user_public_key), &cert)
            .unwrap_err();

        assert!(error.to_string().contains("expired"));
        assert!(
            !store
                .fleet_file(&hex::encode(cert.user_public_key))
                .exists()
        );
    }

    phase revoked_device_cannot_use_approval_capability;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[75; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[76; 32]);
        let user = root.verifying_key().to_bytes();
        let device_public_key = device.verifying_key().to_bytes();
        let mut revocation = RevocationEntry {
            user_public_key: user,
            revoked_device_public_key: device_public_key,
            revoked_at_ns: 10,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &root).unwrap();
        let identity = tempfile::tempdir().unwrap();
        let store = fleet_store(identity.path(), identity.path(), None);
        let mut fleet = DeviceFleet::new(hex::encode(user));
        fleet.revocations.push(revocation);
        store.adopt_transferred(&fleet).unwrap();

        assert!(
            store
                .load(&hex::encode(user))
                .unwrap()
                .is_revoked(&device_public_key)
        );
    }

    phase signed_response_rejects_bundle_tampering;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let signed = sign_response(payload(&root), Approver::Root(&root)).unwrap();
        verify_response(&signed, &token(&root)).unwrap();
        let mut response: Response = postcard::from_bytes(&signed).unwrap();
        response
            .payload
            .home_peer_toml
            .as_mut()
            .unwrap()
            .push_str("\n# changed");
        let changed = postcard::to_stdvec(&response).unwrap();
        assert!(verify_response(&changed, &token(&root)).is_err());
    }

    phase approval_target_requires_a_complete_pair_of_bounded_ids;
    {
        let mut options = ApprovalOptions {
            identity_root: std::path::Path::new(""),
            identity_dir: None,
            timeout: 1,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            server_workspace_ids: &[],
            expected_device: None,
            expected_service: None,
            kind: EnrollmentKind::HomePeer,
            port: 0,
            ready: None,
            approved: None,
            person_uid: None,
        };
        assert_eq!(expected_pair(&options).unwrap(), None);

        options.expected_device = Some("00");
        assert!(expected_pair(&options).is_err());

        let device = hex::encode([7; 32]);
        let service = hex::encode([8; 32]);
        options.expected_device = Some(&device);
        options.expected_service = Some(&service);
        assert_eq!(expected_pair(&options).unwrap(), Some(([7; 32], [8; 32])));
    }

    phase provisioning_bundle_is_encrypted_for_the_recipient_device;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut request_seed = [0_u8; 32];
        request_seed[0] = 11;
        let request_ephemeral = tilo_seal::SecretKey::from_bytes(request_seed);
        let request_public = request_ephemeral.public_key().unwrap().to_bytes();
        let mut payload = payload(&root);
        payload.home_peer_toml = Some("ticket = 'secret-ticket-material'".into());
        let signed = sign_response(payload, Approver::Root(&root)).unwrap();
        let encrypted = encrypt_response(
            &signed,
            [3; 16],
            device.verifying_key().to_bytes(),
            ResponseRoute {
                response_public_key: request_public,
            },
        )
        .unwrap();
        assert!(v2_body(&encrypted).is_some());
        assert!(
            !encrypted
                .windows(b"secret-ticket-material".len())
                .any(|window| window == b"secret-ticket-material")
        );
        let decrypted = decrypt_response(
            &encrypted,
            [3; 16],
            device.verifying_key().to_bytes(),
            &request_ephemeral,
        )
        .unwrap();
        verify_response(&decrypted, &token(&root)).unwrap();
        let wrong = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        assert!(
            decrypt_response(
                &encrypted,
                [3; 16],
                wrong.verifying_key().to_bytes(),
                &request_ephemeral,
            )
            .is_err()
        );
    }

    phase bearer_token_is_encrypted_for_the_issuing_device;
    {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let request = Request {
            token: "reusable-secret-token".into(),
            device_public_key: [7; 32],
            server_node_id: [8; 32],
            label: "NAS".into(),
            hardware: "8 cores · 16 GB memory".into(),
            kind: EnrollmentKind::HomePeer,
            recovery_key: None,
        };
        let (encrypted, _) = encrypt_request(&request, issuer.verifying_key().to_bytes()).unwrap();
        assert!(
            !encrypted
                .windows(b"reusable-secret-token".len())
                .any(|window| window == b"reusable-secret-token")
        );
        let (decrypted, _) = decrypt_request(&encrypted, &issuer).unwrap();
        assert_eq!(decrypted.token, request.token);
        let wrong = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        assert!(decrypt_request(&encrypted, &wrong).is_err());
    }
}

#[test]
fn pair_device_rejects_an_expired_requester_before_creating_identity_state() {
    let mut requester = std::process::Command::new("/bin/cat")
        .stdin(std::process::Stdio::piped())
        .spawn_locked()
        .unwrap();
    let lease = tilo_server::ProcessLease::capture(requester.id()).unwrap();
    requester.kill().unwrap();
    requester.wait().unwrap();

    let identity_root = tempfile::tempdir().unwrap();
    let identity_dir = tempfile::tempdir().unwrap();
    assert!(
        pair_device(
            tilo_identity::enrollment::Methods::ALL,
            JoinOptions {
                identity_root: identity_root.path(),
                label: "New laptop",
                timeout: 1,
                kind: EnrollmentKind::PersonalDevice,
                home_peer_output: None,
                identity_dir: Some(identity_dir.path()),
                requester_lease: Some(lease),
                port: 0,
                person_uid: None,
                recovery_key: None,
            },
            |_| Ok(()),
        )
        .is_err()
    );
    let paths = IdentityPaths::under(identity_root.path());
    assert!(!paths.device_privkey.exists());
    assert!(!paths.user_pubkey.exists());
    assert!(!paths.device_cert.exists());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the end-to-end test keeps the approver and joining-device setup in one auditable protocol scenario"
)]
fn nearby_personal_device_pairing_transfers_cert_fleet_and_home_peer() {
    let approver_root = tempfile::tempdir().unwrap();
    let approver_identities = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(approver_root.path());
    paths.ensure_directories().unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let issuer = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    tilo_identity::provisioning::import_single_device_root(
        approver_identities.path(),
        &root,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    tilo_device_identity::persist::write_public_key(
        &paths.user_pubkey,
        &root.verifying_key().to_bytes(),
    )
    .unwrap();
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, &issuer).unwrap();
    let mut issuer_cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: issuer.verifying_key().to_bytes(),
        device_label: "Trusted laptop".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut issuer_cert, &root).unwrap();
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &issuer_cert).unwrap();
    tilo_device_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&root.verifying_key().to_bytes()),
        &issuer_cert,
    )
    .unwrap();
    fleet_store(approver_root.path(), approver_identities.path(), None)
        .remember_certificate(&hex::encode(root.verifying_key().to_bytes()), issuer_cert)
        .unwrap();
    let home_device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut home_cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: home_device.verifying_key().to_bytes(),
        device_label: "Home peer".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut home_cert, &root).unwrap();
    register_home_peer(approver_root.path(), &home_cert, [43; 32], &[], None).unwrap();

    // Seed the new device's key so the approval pins exactly this device:
    // an unpinned approver answers whichever device solicits first.
    let new_root = tempfile::tempdir().unwrap();
    let new_identities = tempfile::tempdir().unwrap();
    let new_device = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
    let new_paths = IdentityPaths::under(new_root.path());
    new_paths.ensure_directories().unwrap();
    tilo_device_identity::persist::write_device_key(&new_paths.device_privkey, &new_device)
        .unwrap();
    let expected_device = hex::encode(new_device.verifying_key().to_bytes());
    let expected_service = hex::encode(tilo_server::daemon::server_node_id(&new_device));

    let server_root = approver_root.path().to_path_buf();
    let server_identities = approver_identities.path().to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let started_ns = now_ns().unwrap();
    let approval = std::thread::spawn(move || {
        approve(&ApprovalOptions {
            identity_root: &server_root,
            identity_dir: Some(&server_identities),
            timeout: 60,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            server_workspace_ids: &[],
            expected_device: Some(&expected_device),
            expected_service: Some(&expected_service),
            kind: EnrollmentKind::PersonalDevice,
            // An OS-assigned port keeps concurrent tests off each other's
            // listeners and off the well-known port.
            port: 0,
            ready: Some(&ready_tx),
            approved: None,
            person_uid: None,
        })
    });
    let port = match ready_rx.recv_timeout(tilo_test_env::hang_bound()) {
        Ok(port) => port,
        // An approver that ended before listening dropped the sender; its
        // own error names why.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the approver ended before listening: {:?}", approval.join())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("the approval listener did not become ready within the hang bound")
        }
    };
    // Other nearby-pairing surfaces on this host (an open discovery
    // view, another approval sheet) share the approver's port. Which of
    // them the kernel hands a unicast datagram must not decide whether
    // the enrollment completes.
    let _other_surfaces: Vec<UdpSocket> =
        (0..3).map(|_| bind_pair_listener(port).unwrap()).collect();
    let requester = tilo_server::ProcessLease::capture(std::process::id()).unwrap();
    pair_device(
        tilo_identity::enrollment::Methods::ALL,
        JoinOptions {
            identity_root: new_root.path(),
            label: "New laptop",
            timeout: 60,
            kind: EnrollmentKind::PersonalDevice,
            home_peer_output: None,
            identity_dir: Some(new_identities.path()),
            requester_lease: Some(requester),
            port,
            person_uid: None,
            recovery_key: None,
        },
        |_| Ok(()),
    )
    .unwrap();
    approval.join().unwrap().unwrap();

    let enrolled_paths = IdentityPaths::under(new_root.path());
    let cert =
        tilo_device_identity::persist::read_device_cert(&enrolled_paths.device_cert).unwrap();
    assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
    assert_eq!(cert.capabilities, DeviceCapabilities::member());
    assert!(verify_device_cert(&cert).is_ok());
    // The new device checks its certificate on its own clock the moment it
    // arrives, and that clock can trail the approver's. One that reads a
    // full allowance behind the end of this pairing still accepts it, and
    // validity begins no further back than the allowance.
    let finished_ns = now_ns().unwrap();
    verify_device_cert_at(&cert, finished_ns - ISSUANCE_CLOCK_ALLOWANCE_NS)
        .expect("a device whose clock trails the approver's accepts its certificate");
    assert!(cert.issued_at_ns >= started_ns - ISSUANCE_CLOCK_ALLOWANCE_NS);
    let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
        &std::fs::read(tilo_server::home_peer_registry_path(new_root.path())).unwrap(),
    )
    .unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].server_node_id, hex::encode([43; 32]));
    let record = tilo_identity::store::resolve_active_identity(new_identities.path()).unwrap();
    let identity = tilo_identity::store::load_from_path(record).unwrap();
    assert!(
        identity
            .sign(b"root/admin signing must stay unavailable")
            .is_err()
    );
    // Each machine now lists the other, and itself, as this person's
    // devices: the list remote actions and wake address. The always-on
    // home peer is a home peer, not a personal device.
    let personal = |root: &std::path::Path| -> Vec<String> {
        serde_json::from_slice(
            &std::fs::read(tilo_server_protocol::personal_device_peer_path(root))
                .expect("pairing recorded the personal devices"),
        )
        .unwrap()
    };
    let approver_key = hex::encode(issuer.verifying_key().to_bytes());
    let new_key = hex::encode(new_device.verifying_key().to_bytes());
    assert_eq!(
        personal(approver_root.path()),
        vec![approver_key.clone(), new_key.clone()]
    );
    assert_eq!(personal(new_root.path()), vec![new_key, approver_key]);
}

tilo_test_env::scenario! {
    fn a_failed_pairing_reports_its_cause;

    phase a_failed_first_use_pairing_reports_its_reason_in_the_status_file;
    {
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let status = identity_dir.path().join(".nearby-enrollment-status.json");
        // A requester that is already gone: the lease check fails before
        // anything is broadcast, as an expired or foreign compositor would.
        let error = pair_device_cli(
            identity_root.path(),
            identity_dir.path(),
            "New laptop",
            1,
            Some(&status),
            false,
            Some(u32::MAX - 1),
            Some(1),
        )
        .unwrap_err();
        let recorded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(recorded["state"], "failed");
        assert_eq!(recorded["error"], format!("{error:#}"));
        assert!(
            recorded["error"]
                .as_str()
                .unwrap()
                .contains("first-use requester exited"),
            "the reason reaches the UI: {recorded}"
        );
    }

    phase a_failed_pairing_names_its_cause;
    {
        let denied = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EACCES))
            .context("publish the enrollment commit");
        assert_eq!(PairingStatus::failed(&denied).cause, Some(PairingCause::PermissionDenied));
        let full = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::ENOSPC));
        assert_eq!(PairingStatus::failed(&full).cause, Some(PairingCause::StorageFull));
        let read_only = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EROFS));
        assert_eq!(PairingStatus::failed(&read_only).cause, Some(PairingCause::ReadOnly));
        let unreachable = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::ENETUNREACH));
        assert_eq!(PairingStatus::failed(&unreachable).cause, Some(PairingCause::Unreachable));
        let closed = anyhow::Error::from(WindowClosed("the window closed"));
        assert_eq!(PairingStatus::failed(&closed).cause, Some(PairingCause::TimedOut));
        assert_eq!(
            PairingStatus::failed(&anyhow::anyhow!("first-use requester exited")).cause, Some(PairingCause::Unknown)
        );
    }
}

tilo_test_env::scenario! {
    fn pairing_exchange_is_private_single_use_and_resumable;

    phase automatic_pair_offer_is_private_to_the_soliciting_server;
    {
        let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let token = "signed-host-enrollment-token";
        let encrypted = encrypt_pair_offer(token, server.verifying_key().to_bytes()).unwrap();
        assert!(v2_body(&encrypted).is_some());
        assert!(
            !encrypted
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        );
        assert_eq!(decrypt_pair_offer(&encrypted, &server).unwrap(), token);
        let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        assert!(decrypt_pair_offer(&encrypted, &other).is_err());
    }

    phase nearby_pairing_code_is_stable_and_six_digits;
    {
        let code = pairing_code([7; 32], [8; 32]);
        assert_eq!(code, pairing_code([7; 32], [8; 32]));
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
        assert_ne!(code, pairing_code([7; 32], [9; 32]));
    }

    phase unrelated_lan_datagram_is_not_a_pairing_message;
    {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        assert!(decrypt_request(b"ambient discovery traffic", &issuer).is_err());
        let mut request_seed = [0_u8; 32];
        request_seed[0] = 11;
        let request_ephemeral = tilo_seal::SecretKey::from_bytes(request_seed);
        assert!(
            decrypt_response(
                b"not an enrollment response",
                [3; 16],
                [7; 32],
                &request_ephemeral,
            )
            .is_err()
        );
    }

    phase enrollment_nonce_is_consumed_once;
    {
        let dir = tempfile::tempdir().unwrap();
        let nonces = NonceStore::under(dir.path());
        nonces.record(&[4; 16], [7; 32]).unwrap();
        nonces.record(&[4; 16], [7; 32]).unwrap();
        assert!(nonces.record(&[4; 16], [8; 32]).is_err());
        assert!(
            dir.path()
                .join("enrollment-nonces")
                .join(hex::encode([4; 16]))
                .exists()
        );
    }

    phase approved_home_peer_registration_exposes_distinct_stable_node_ids;
    {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let cert = payload(&root).cert;
        register_home_peer(
            dir.path(),
            &cert,
            [8_u8; 32],
            &[
                tilo_server_protocol::WorkspaceId::from_u128(2),
                tilo_server_protocol::WorkspaceId::from_u128(9),
            ],
            None,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tilo_server::home_peer_registry_path(dir.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(
            json[0]["device_node_id"],
            hex::encode(cert.device_public_key)
        );
        assert_eq!(json[0]["server_node_id"], hex::encode([8; 32]));
        assert_eq!(
            json[0]["server_workspace_ids"],
            serde_json::json!([
                "00000000-0000-0000-0000-000000000002",
                "00000000-0000-0000-0000-000000000009"
            ])
        );
        assert_eq!(
            std::fs::metadata(tilo_server::home_peer_registry_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    phase invalid_existing_home_peer_registry_is_never_overwritten;
    {
        let dir = tempfile::tempdir().unwrap();
        let path = tilo_server::home_peer_registry_path(dir.path());
        tilo_files::native::atomic::write_creating_parent(&path, b"not json").unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        assert!(
            register_home_peer(
                dir.path(),
                &payload(&root).cert,
                [8_u8; 32],
                &[tilo_server_protocol::WorkspaceId::from_u128(2)],
                None
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
    }

    phase dropped_response_can_be_retried_with_the_same_authenticated_ack;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let response = sign_response(payload(&root), Approver::Root(&root)).unwrap();
        // The first datagram is modeled as dropped. Retransmission uses the
        // exact same signed bytes, and the recipient ACK binds their key,
        // nonce, and that response digest.
        let retransmitted = response.clone();
        assert_eq!(response, retransmitted);
        let ack = make_ack(&retransmitted, [3; 16], &device).unwrap();
        verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
    }

    phase dropped_all_acks_can_resume_after_process_restart;
    {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let response = sign_response(payload(&root), Approver::Root(&root)).unwrap();

        // The approver spent the nonce for this device when it redeemed
        // the token, before the response first left. However many ACKs
        // are lost, the device finishes with that same response and
        // records the nonce idempotently; no other device can.
        let nonces = NonceStore::under(dir.path());
        nonces
            .claim(&[3; 16], device.verifying_key().to_bytes())
            .unwrap();
        let ack = make_ack(&response, [3; 16], &device).unwrap();
        verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
        nonces
            .record(&[3; 16], device.verifying_key().to_bytes())
            .unwrap();
        let finalized = make_finalized(
            &response,
            [3; 16],
            device.verifying_key().to_bytes(),
            Approver::Root(&root),
        )
        .unwrap();
        verify_finalized(
            &finalized,
            &response,
            [3; 16],
            device.verifying_key().to_bytes(),
            root.verifying_key().to_bytes(),
        )
        .unwrap();
        nonces
            .record(&[3; 16], device.verifying_key().to_bytes())
            .unwrap();
        assert!(nonces.record(&[3; 16], [6; 32]).is_err());
    }
}

const KAT_RESPONSE_SIGNATURE: &str = "9082279179d565ac7d05ffae71ab9d648f247f8317d3dded09a53162198f72e9\
    f3ece18c9e83fe35495c6c24192d3ebb9a0e8c6fbc84b24648e51c9c7b375502";
const KAT_ACK: &str = "0404040404040404040404040404040417cb79fb2b4120f2b1ec65e4198d6e08\
    b28e813feb01e4a400839b85e18080ce0aa39056fd697d25fd6d7afbe84dbb81\
    f52d9e543b4aa167cf6472c602c8079a402f82443b34fd9707a575979dc100ce\
    d50e200bbcd54bee4bf90d735aa8eaa0a492230680992d64e4a01ad9815dd391\
    93d225eb9e17599471e0019670b6f72c00";

tilo_test_env::scenario! {
    fn pairing_crypto_matches_known_answers;

    phase known_answer_x25519_and_ed25519_conversion;
    {
        let a = tilo_seal::SecretKey::from_bytes([0x11; 32]);
        assert_eq!(
            hex::encode(a.public_key().unwrap().to_bytes()),
            "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13"
        );
        let recipient = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
        assert_eq!(
            hex::encode(recipient.to_scalar_bytes()),
            "c25a999cf27ed91865db781f67e404f9d541d12e3e761786c409131fe595096d"
        );
        assert_eq!(
            hex::encode(recipient.verifying_key().to_montgomery().to_bytes()),
            "931aad030ad93d3ff8042699324fdb230585be4daf500266eaa4b8f747543f50"
        );
    }

    phase known_answer_response_signature_verifies;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let signed = sign_response(payload(&root), Approver::Root(&root)).unwrap();
        assert_eq!(
            hex::encode(postcard::from_bytes::<Response>(&signed).unwrap().signature),
            KAT_RESPONSE_SIGNATURE
        );
        verify_response(&signed, &token(&root)).unwrap();
    }

    phase known_answer_ack_signature;
    {
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);
        let ack = make_ack(b"kat-response", [4; 16], &device).unwrap();
        assert_eq!(hex::encode(&ack), KAT_ACK);
        verify_ack(
            &ack,
            b"kat-response",
            [4; 16],
            device.verifying_key().to_bytes(),
        )
        .unwrap();
    }

    phase a_key_holder_answers_as_every_build_expects_and_an_introducer_with_its_certificate;
    // A device holding the identity's key answers with the identity-signed
    // `(payload, signature)` every build decodes and checks against the
    // identity's key; only an introducer, which holds none of it, signs with its
    // certified device key and names its certificate. A joining device accepts
    // both, and each only from its signer.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let user = root.verifying_key().to_bytes();
        let reply = sign_response(payload(&root), Approver::Root(&root)).unwrap();
        let signature = tilo_user_identity::StatementSigner::sign_digest(
            &root,
            &response_statement_digest(&payload(&root)).unwrap(),
        )
        .unwrap()
        .to_vec();
        assert_eq!(
            *reply,
            postcard::to_stdvec(&(payload(&root), signature)).unwrap()
        );
        assert_eq!(verify_response(&reply, &token(&root)).unwrap().1, user);
        let finalized = make_finalized(&reply, [3; 16], [7; 32], Approver::Root(&root)).unwrap();
        verify_finalized(&finalized, &reply, [3; 16], [7; 32], user).unwrap();

        let device = ed25519_dalek::SigningKey::from_bytes(&[12; 32]);
        let mut issuer = DeviceCert {
            user_public_key: user,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Orange Pi".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut issuer, &root).unwrap();
        let introducer = Approver::Introducer {
            device: &device,
            issuer: &issuer,
        };
        let signed_by_introducer = sign_response(payload(&root), introducer).unwrap();
        let minted = token_from(user, &device);
        assert_eq!(
            verify_response(&signed_by_introducer, &minted).unwrap().1,
            device.verifying_key().to_bytes()
        );
        let finalized = make_finalized(&signed_by_introducer, [3; 16], [7; 32], introducer).unwrap();
        verify_finalized(
            &finalized,
            &signed_by_introducer,
            [3; 16],
            [7; 32],
            device.verifying_key().to_bytes(),
        )
        .unwrap();
        assert!(
            verify_response(&signed_by_introducer, &token(&root)).is_err(),
            "an introducer answers only its own token"
        );
        let mut uncertified = issuer.clone();
        uncertified.capabilities = DeviceCapabilities::primary();
        assert!(
            verify_response(
                &sign_response(
                    payload(&root),
                    Approver::Introducer {
                        device: &device,
                        issuer: &uncertified,
                    },
                )
                .unwrap(),
                &minted,
            )
            .is_err(),
            "an introducer's certificate must be the identity's"
        );
    }
}

fn sample_request() -> Request {
    Request {
        token: "bearer".into(),
        device_public_key: ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes(),
        server_node_id: [8; 32],
        label: "NAS".into(),
        hardware: "8 cores · 16 GB memory".into(),
        kind: EnrollmentKind::HomePeer,
        recovery_key: None,
    }
}

tilo_test_env::scenario! {
    fn pairing_envelopes_are_step_bound;

    phase v2_request_gets_a_v2_response_only_the_requester_opens;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let (bytes, response_secret) =
            encrypt_request(&sample_request(), issuer.verifying_key().to_bytes()).unwrap();
        assert_eq!(&bytes[..8], b"TLOENRL\x02");
        let (mut request, route) = decrypt_request(&bytes, &issuer).unwrap();
        assert_eq!(request.token, "bearer");
        let token = token(&root);
        let mut payload = payload(&root);
        payload.recipient_device_public_key = request.device_public_key;
        payload.cert = root_signed(
            &root,
            request.device_public_key,
            &request.label,
            token.capabilities.clone(),
            1,
        );
        let signed = sign_response(payload, Approver::Root(&root)).unwrap();
        let response =
            encrypt_response(&signed, token.nonce, request.device_public_key, route).unwrap();
        assert!(v2_body(&response).is_some());
        let (opened, approver) = open_answer(&response, &token, &request, &response_secret).unwrap();
        assert_eq!(opened.cert.device_public_key, request.device_public_key);
        assert_eq!(approver, token.user_public_key);
        request.server_node_id = [13; 32];
        assert!(open_answer(&response, &token, &request, &response_secret).is_err());
        let other = tilo_seal::SecretKey::generate().unwrap();
        assert!(
            decrypt_response(
                &response,
                [3; 16],
                device.verifying_key().to_bytes(),
                &other
            )
            .is_err()
        );
        assert!(
            decrypt_response(
                &response,
                [4; 16],
                device.verifying_key().to_bytes(),
                &response_secret
            )
            .is_err()
        );
        assert!(decrypt_response(&response, [3; 16], [1; 32], &response_secret).is_err());
    }

    phase pair_offers_answer_only_the_current_solicitation_magic;
    {
        let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut solicitation = PairSolicitation {
            magic: PAIR_MAGIC,
            device_public_key: server.verifying_key().to_bytes(),
            server_node_id: [8; 32],
            label: "NAS".into(),
            hardware: "4 cores · 8 GB memory".into(),
            kind: EnrollmentKind::HomePeer,
        };
        assert!(valid_solicitation(&solicitation));
        let offer = encrypt_pair_offer("token", solicitation.device_public_key).unwrap();
        assert!(v2_body(&offer).is_some());
        assert_eq!(decrypt_pair_offer(&offer, &server).unwrap(), "token");

        for magic in [*b"TLOPAIR1", *b"TLOPAIR9"] {
            solicitation.magic = magic;
            assert!(!valid_solicitation(&solicitation));
        }
    }

    phase version_prefix_decides_the_parser_and_tampering_fails;
    {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let (bytes, _) =
            encrypt_request(&sample_request(), issuer.verifying_key().to_bytes()).unwrap();
        // Every byte of an envelope is covered: flipping the version or any
        // sealed byte fails, and a stripped prefix is refused.
        for index in 0..bytes.len() {
            let mut tampered = bytes.clone();
            tampered[index] ^= 1;
            assert!(decrypt_request(&tampered, &issuer).is_err(), "byte {index}");
        }
        assert!(decrypt_request(&bytes[8..], &issuer).is_err());
        let wrong = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        assert!(decrypt_request(&bytes, &wrong).is_err());
    }

    phase v2_envelopes_do_not_open_as_another_step;
    {
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let offer = encrypt_pair_offer("token", device.verifying_key().to_bytes()).unwrap();
        // Same recipient key, different step label: the request reader
        // must refuse the pairing offer.
        assert!(decrypt_request(&offer, &device).is_err());
    }
}
