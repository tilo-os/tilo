use super::*;
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert, sign_revocation_entry,
};

#[test]
fn host_record_approval_keeps_admission_and_revocation_checks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tilo");
    let identities = temp.path().join("identities");
    let paths = IdentityPaths::under(&root);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &identities,
        &paths,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    assert!(!paths.user_privkey.exists());
    let options = ApprovalOptions {
        identity_root: &root,
        identity_dir: Some(&identities),
        timeout: 1,
        workspace_cache: temp.path(),
        acl: temp.path(),
        relay_workspace_ids: &[],
        expected_device: None,
        expected_service: None,
        kind: EnrollmentKind::HomePeer,
        port: 0,
        ready: None,
        approved: None,
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
    revocation.signature = identity
        .root
        .as_ref()
        .unwrap()
        .sign_message(&tilo_user_identity::revocation_signing_digest(&revocation).unwrap())
        .unwrap();
    let path = fleet_path(&identities, identity.user_public_key);
    let mut fleet: FleetProjection =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    fleet.revocations.push(revocation);
    std::fs::write(&path, serde_json::to_vec(&fleet).unwrap()).unwrap();
    let error = load_approval_identity(&options).err().unwrap();
    assert!(error.to_string().contains("revoked"));
    fleet.revocations.clear();
    std::fs::write(&path, serde_json::to_vec(&fleet).unwrap()).unwrap();
    let mut issuer = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
    issuer.capabilities = DeviceCapabilities::member();
    sign_root_cert(&mut issuer, identity.root.as_deref().unwrap()).unwrap();
    tilo_user_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&identity.user_public_key),
        &issuer,
    )
    .unwrap();
    let error = load_approval_identity(&options).err().unwrap();
    assert!(error.to_string().contains("not authorised"));
    issuer.capabilities = DeviceCapabilities::primary();
    issuer.issued_at_ns = 1;
    issuer.expires_at_ns = Some(2);
    sign_root_cert(&mut issuer, identity.root.as_deref().unwrap()).unwrap();
    tilo_user_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&identity.user_public_key),
        &issuer,
    )
    .unwrap();
    let error = load_approval_identity(&options).err().unwrap();
    assert!(error.to_string().contains("validity"));
    let unrelated_root = ed25519_dalek::SigningKey::from_bytes(&[76; 32]);
    issuer.expires_at_ns = None;
    issuer.user_public_key = unrelated_root.verifying_key().to_bytes();
    sign_device_cert(&mut issuer, &unrelated_root).unwrap();
    tilo_user_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&identity.user_public_key),
        &issuer,
    )
    .unwrap();
    let error = load_approval_identity(&options).err().unwrap();
    assert!(error.to_string().contains("not authorised"));
    assert!(!paths.user_privkey.exists());
}

/// The root a device holds certifies a developer's machine for
/// administration and lists it in the person's fleet, where Settings can
/// remove it; an unknown capability names the ones there are.
#[test]
fn a_held_root_certifies_an_administrator_into_the_fleet() {
    let identity_root = tempfile::tempdir().unwrap();
    let identity_dir = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(identity_root.path());
    paths.ensure_directories().unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
    tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
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
        home_peer: None,
        out: &out,
    };
    let shell = ["shell".to_owned()];
    let unknown = certify(&options(&shell)).unwrap_err();
    assert!(
        unknown.to_string().contains("read-all or admin"),
        "{unknown}"
    );

    let admin_only = ["admin".to_owned()];
    certify(&options(&admin_only)).unwrap();
    let cert = tilo_user_identity::persist::read_device_cert(&out).unwrap();
    verify_device_cert(&cert).unwrap();
    assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
    assert_eq!(cert.capabilities, DeviceCapabilities::administrator());
    let fleet: FleetProjection = serde_json::from_slice(
        &std::fs::read(fleet_path(identity_dir.path(), cert.user_public_key)).unwrap(),
    )
    .unwrap();
    assert_eq!(fleet.certificates, vec![cert]);
}

/// A host certified out of band becomes one of the person's home peers:
/// the registry names its relay, where that relay listens and the
/// person's continuity shard, so the device shares workspaces on it. A
/// device that may not host is never registered.
#[test]
fn a_host_certified_out_of_band_is_registered_as_a_home_peer() {
    let identity_root = tempfile::tempdir().unwrap();
    let identity_dir = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(identity_root.path());
    paths.ensure_directories().unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[45; 32]);
    tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
    let host = ed25519_dalek::SigningKey::from_bytes(&[46; 32]);
    let device = hex::encode(host.verifying_key().to_bytes());
    let service = tilo_relay::daemon::relay_service_node_id(&host);
    let addr: std::net::SocketAddr = "127.0.0.1:41234".parse().unwrap();
    let out = identity_dir.path().join("host.cert");
    let registry = tilo_relay::home_peer_registry_path(identity_root.path());

    assert!(OutOfBandHomePeer::parse("not hex", &[addr]).is_err());
    let home_peer = OutOfBandHomePeer::parse(&hex::encode(service), &[addr]).unwrap();
    let options = |capabilities| CertifyOptions {
        root_key: None,
        identity_root: identity_root.path(),
        identity_dir: Some(identity_dir.path()),
        device: &device,
        label: "Home relay",
        capabilities,
        home_peer: Some(&home_peer),
        out: &out,
    };
    let member = ["read-all".to_owned()];
    let refused = certify(&options(&member)).unwrap_err();
    assert!(refused.to_string().contains("host capability"), "{refused}");
    assert!(!registry.exists() && !out.exists());

    let host_capabilities = ["host".to_owned(), "read-all".to_owned()];
    certify(&options(&host_capabilities)).unwrap();
    let cert = tilo_user_identity::persist::read_device_cert(&out).unwrap();
    assert_eq!(cert.capabilities, DeviceCapabilities::host());
    let peers: Vec<HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    assert_eq!(peers.len(), 1);
    let peer = &peers[0];
    assert_eq!(peer.device_node_id, device);
    assert_eq!(peer.relay_service_node_id, hex::encode(service));
    assert_eq!(
        peer.user_public_key,
        hex::encode(root.verifying_key().to_bytes())
    );
    assert_eq!(
        peer.relay_workspace_ids,
        vec![tilo_relay::personal_continuity_workspace_id(
            &root.verifying_key().to_bytes()
        )]
    );
    let endpoint = peer.relay_endpoint.as_ref().expect("the dial hint is kept");
    assert_eq!(endpoint.id.as_bytes(), &service);
    assert_eq!(endpoint.ip_addrs().copied().collect::<Vec<_>>(), vec![addr]);
    assert_eq!(peer.device_cert.as_ref(), Some(&cert));
    assert!(valid_home_peer_registration(peer));

    // Certified again (a relay restarted on another port): one entry,
    // the newest hint.
    let moved: std::net::SocketAddr = "127.0.0.1:41235".parse().unwrap();
    let home_peer = OutOfBandHomePeer::parse(&hex::encode(service), &[moved]).unwrap();
    certify(&CertifyOptions {
        home_peer: Some(&home_peer),
        ..options(&host_capabilities)
    })
    .unwrap();
    let peers: Vec<HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    assert_eq!(peers.len(), 1);
    let endpoint = peers[0].relay_endpoint.as_ref().unwrap();
    assert_eq!(
        endpoint.ip_addrs().copied().collect::<Vec<_>>(),
        vec![moved]
    );
}

#[test]
fn an_out_of_band_certificate_joins_its_identity() {
    let identity_root = tempfile::tempdir().unwrap();
    let identity_dir = tempfile::tempdir().unwrap();
    device_key(identity_root.path()).unwrap();
    let paths = IdentityPaths::under(identity_root.path());
    let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey).unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
    let user = root.verifying_key().to_bytes();
    let issue = |device_public_key: [u8; 32], root: &ed25519_dalek::SigningKey| {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key,
            device_label: "Node".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        let path = identity_dir
            .path()
            .join(format!("{}.cert", hex::encode(&cert.signature[..8])));
        tilo_user_identity::persist::write_device_cert(&path, &cert).unwrap();
        path
    };

    let other_device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let foreign = issue(other_device.verifying_key().to_bytes(), &root);
    assert!(install_cert(identity_root.path(), identity_dir.path(), &foreign).is_err());
    assert!(!paths.device_cert.exists());

    let root_key = identity_dir.path().join("root.key");
    tilo_user_identity::persist::write_signing_key(&root_key, &root).unwrap();
    let own = identity_dir.path().join("own.cert");
    let capabilities = ["own", "host", "read-all"].map(String::from);
    certify(&CertifyOptions {
        root_key: Some(&root_key),
        identity_root: identity_root.path(),
        identity_dir: None,
        device: &hex::encode(device.verifying_key().to_bytes()),
        label: "Node",
        capabilities: &capabilities,
        home_peer: None,
        out: &own,
    })
    .unwrap();
    install_cert(identity_root.path(), identity_dir.path(), &own).unwrap();
    // Delivered again (a restart before the launcher noticed): unchanged.
    install_cert(identity_root.path(), identity_dir.path(), &own).unwrap();
    let installed = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
    assert_eq!(installed.user_public_key, user);
    assert!(installed.capabilities.can_act_as_host && !installed.capabilities.can_admit_devices);
    assert_eq!(
        tilo_user_identity::persist::read_public_key(&paths.user_pubkey).unwrap(),
        user
    );
    let record =
        runtime_identity::load_from_path(identity_record_path(identity_dir.path(), user)).unwrap();
    assert_eq!(record.group_public_key(), user);
    assert!(record.sign(b"root-only operation").is_err());

    let stranger = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
    let takeover = issue(device.verifying_key().to_bytes(), &stranger);
    assert!(install_cert(identity_root.path(), identity_dir.path(), &takeover).is_err());
    assert_eq!(
        tilo_user_identity::persist::read_public_key(&paths.user_pubkey).unwrap(),
        user
    );
}

fn personal_payload(
    root: &ed25519_dalek::SigningKey,
    device: &ed25519_dalek::SigningKey,
    nonce: [u8; 16],
) -> ResponsePayload {
    let user = root.verifying_key().to_bytes();
    let mut cert = DeviceCert {
        user_public_key: user,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "New laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    let fleet = FleetProjection {
        schema_version: 1,
        identity_id: hex::encode(user),
        certificates: vec![cert.clone()],
        revocations: Vec::new(),
    };
    ResponsePayload {
        nonce,
        recipient_device_public_key: cert.device_public_key,
        relay_service_node_id: [8; 32],
        cert,
        home_peer_toml: None,
        public_identity_json: Some(public_identity_record(user, "My Tilo identity").unwrap()),
        fleet_json: Some(serde_json::to_vec_pretty(&fleet).unwrap()),
        home_peers_json: Some(b"[]".to_vec()),
    }
}

/// A token `issuer` minted for the identity `user`.
fn token_from(
    user: [u8; 32],
    issuer: &ed25519_dalek::SigningKey,
) -> tilo_user_identity::EnrollmentToken {
    mint_enrollment_token(user, DeviceCapabilities::host(), 10, 1_000, [3; 16], issuer).unwrap()
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
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    ResponsePayload {
        nonce: [3; 16],
        recipient_device_public_key: [7; 32],
        relay_service_node_id: [8; 32],
        cert,
        home_peer_toml: Some("status_file = '/var/lib/tilo/full-replica/status.json'".into()),
        public_identity_json: None,
        fleet_json: None,
        home_peers_json: None,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn enrollment_responses_are_signed_encrypted_and_certificate_bound() {
    // Phase `expired_device_certificate_is_not_projected_into_personal_state`.
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
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let identity = tempfile::tempdir().unwrap();

        let error = project_personal_device(identity.path(), &cert).unwrap_err();

        assert!(error.to_string().contains("certificate validity"));
        assert!(!fleet_path(identity.path(), cert.user_public_key).exists());
    }
    // Phase `revoked_device_cannot_use_approval_capability`.
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
        tilo_files::native::atomic::write_creating_parent(
            &fleet_path(identity.path(), user),
            &serde_json::to_vec(&FleetProjection {
                schema_version: 1,
                identity_id: hex::encode(user),
                certificates: Vec::new(),
                revocations: vec![revocation],
            })
            .unwrap(),
        )
        .unwrap();

        assert!(
            fleet_revokes_device(identity.path(), identity.path(), user, device_public_key)
                .unwrap()
        );
    }
    // Phase `signed_response_rejects_bundle_tampering`.
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
    // Phase `approval_target_requires_a_complete_pair_of_bounded_ids`.
    {
        let mut options = ApprovalOptions {
            identity_root: std::path::Path::new(""),
            identity_dir: None,
            timeout: 1,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            relay_workspace_ids: &[],
            expected_device: None,
            expected_service: None,
            kind: EnrollmentKind::HomePeer,
            port: 0,
            ready: None,
            approved: None,
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
    // Phase `provisioning_bundle_is_encrypted_for_the_recipient_device`.
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
    // Phase `bearer_token_is_encrypted_for_the_issuing_device`.
    {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let request = Request {
            token: "reusable-secret-token".into(),
            device_public_key: [7; 32],
            relay_service_node_id: [8; 32],
            label: "NAS".into(),
            kind: EnrollmentKind::HomePeer,
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
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn personal_projection_publishes_only_live_valid_identity_state() {
    // Phase `personal_enrollment_staging_keeps_an_existing_device_unchanged`.
    fn personal_enrollment_staging_keeps_an_existing_device_unchanged() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let member = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let old_root = ed25519_dalek::SigningKey::from_bytes(&[53; 32]);
        let old_device = ed25519_dalek::SigningKey::from_bytes(&[54; 32]);
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        paths.ensure_directories().unwrap();
        tilo_user_identity::persist::write_public_key(
            &paths.user_pubkey,
            &old_root.verifying_key().to_bytes(),
        )
        .unwrap();
        let mut old_cert = DeviceCert {
            user_public_key: old_root.verifying_key().to_bytes(),
            device_public_key: old_device.verifying_key().to_bytes(),
            device_label: "Existing device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut old_cert, &old_root).unwrap();
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &old_cert).unwrap();
        let before_key = std::fs::read(&paths.user_pubkey).unwrap();
        let before_cert = std::fs::read(&paths.device_cert).unwrap();

        // Keep one live requester on its stdin pipe. Killing a shell loop
        // leaves its timer child holding the test's output handles.
        let mut requester = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
        let payload = personal_payload(&root, &member, [9; 16]);
        let (marker_path, marker) = stage_personal_enrollment(
            identity_root.path(),
            identity_dir.path(),
            &paths,
            &payload,
            Some(lease),
        )
        .unwrap();
        // This models an approval timeout: the recipient has a durable,
        // retryable staging marker but no active identity mutation.
        assert_eq!(std::fs::read(&paths.user_pubkey).unwrap(), before_key);
        assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
        assert!(marker_path.exists());
        // A timeout/restart must not turn a merely prepared response into an
        // active enrollment.
        recover_personal_enrollments(
            identity_root.path(),
            identity_dir.path(),
            &paths,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(std::fs::read(&paths.user_pubkey).unwrap(), before_key);
        assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
        let personal_peers = tilo_relay_protocol::personal_device_peer_path(identity_root.path());
        assert!(
            !personal_peers.exists(),
            "an unconfirmed enrollment records no personal device"
        );

        let marker = authorize_personal_commit(&marker_path, &marker, Some(lease)).unwrap();
        assert!(
            commit_staged_personal_enrollment(
                identity_root.path(),
                &paths,
                &marker_path,
                &marker,
                |checkpoint| {
                    lease.validate()?;
                    if checkpoint == PersonalCommitCheckpoint::DefaultCert {
                        bail!("simulated crash before default certificate")
                    }
                    Ok(())
                },
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
        // First use reads a selected joined identity as a finished join;
        // an interrupted commit has selected nothing yet.
        assert_eq!(
            runtime_identity::resolve_active_identity(identity_dir.path()),
            None,
            "an unfinished join selected its identity"
        );
        requester.kill().unwrap();
        requester.wait().unwrap();
        assert!(lease.validate().is_err());
        // `commit_ready` is the durable boundary: Finalized and the original
        // live lease were authenticated before it was written. A subsequent
        // explicit retry may complete that exact transaction under its own
        // live lease rather than leaving mixed durable state.
        let retry_lease = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
        // A root-run commit may run under a strict umask (the pairing
        // helper's, or a script's); the public files stay readable.
        // SAFETY: umask has no preconditions; this test process is alone.
        let umask = unsafe { libc::umask(0o077) };
        let recovered =
            recover_personal_enrollments(identity_root.path(), identity_dir.path(), &paths, |_| {
                retry_lease.validate().map_err(Into::into)
            });
        // SAFETY: as above, restoring the previous mask.
        unsafe { libc::umask(umask) };
        recovered.unwrap();
        for public in [
            paths.user_pubkey.clone(),
            paths.device_cert.clone(),
            paths.device_cert_for_user(&root.verifying_key().to_bytes()),
            personal_peers.clone(),
        ] {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&public).unwrap().permissions().mode() & 0o777,
                tilo_user_identity::persist::PUBLIC_FILE_MODE,
                "{} is not readable to the session",
                public.display()
            );
        }
        assert_eq!(
            std::fs::read(&paths.user_pubkey).unwrap(),
            root.verifying_key().to_bytes()
        );
        assert_eq!(
            tilo_user_identity::persist::read_device_cert(&paths.device_cert)
                .unwrap()
                .device_public_key,
            member.verifying_key().to_bytes()
        );
        let recorded: Vec<String> =
            serde_json::from_slice(&std::fs::read(&personal_peers).unwrap()).unwrap();
        assert_eq!(
            recorded,
            vec![hex::encode(member.verifying_key().to_bytes())]
        );
        assert!(
            runtime_identity::resolve_active_identity(identity_dir.path()).is_some(),
            "the finished join selects its identity"
        );
    }
    // Phase `personal_projection_loads_without_root_signing_material`.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[19; 32]);
        let user = root.verifying_key().to_bytes();
        let device = ed25519_dalek::SigningKey::from_bytes(&[23; 32]);
        let mut cert = DeviceCert {
            user_public_key: user,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "New laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let fleet = FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(user),
            certificates: vec![cert.clone()],
            revocations: Vec::new(),
        };
        let payload = ResponsePayload {
            nonce: [1; 16],
            recipient_device_public_key: cert.device_public_key,
            relay_service_node_id: [8; 32],
            cert,
            home_peer_toml: None,
            public_identity_json: Some(public_identity_record(user, "My Tilo identity").unwrap()),
            fleet_json: Some(serde_json::to_vec_pretty(&fleet).unwrap()),
            home_peers_json: Some(b"[]".to_vec()),
        };
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let record =
            install_personal_projection(identity_root.path(), identity_dir.path(), &payload)
                .unwrap();
        // The joined identity is what this device acts for from the next
        // login on, not an absent default that asks to create one.
        assert_eq!(
            runtime_identity::resolve_active_identity(identity_dir.path()),
            Some(record.clone())
        );
        let loaded = runtime_identity::load_from_path(record).unwrap();
        assert_eq!(loaded.group_public_key(), user);
        assert!(!loaded.is_single_device());
        assert!(loaded.sign(b"root-only operation").is_err());
        assert_eq!(
            std::fs::read(tilo_relay::home_peer_registry_path(identity_root.path())).unwrap(),
            b"[]"
        );
        let registry: serde_json::Value = serde_json::from_slice(
            &std::fs::read(identity_dir.path().join("registry.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(registry["default_identity"], hex::encode(user));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let uid = rustix::process::geteuid().as_raw();
            assert_eq!(
                std::fs::metadata(identity_dir.path().join("registry.json"))
                    .unwrap()
                    .uid(),
                uid
            );
            assert_eq!(
                std::fs::metadata(fleet_path(identity_dir.path(), user))
                    .unwrap()
                    .uid(),
                uid
            );
        }
    }
    // Phase `personal_projection_merges_and_preserves_unrelated_home_peers`.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let member = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
        let other_root = ed25519_dalek::SigningKey::from_bytes(&[63; 32]);
        let other_device = ed25519_dalek::SigningKey::from_bytes(&[64; 32]);
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let mut other_cert = DeviceCert {
            user_public_key: other_root.verifying_key().to_bytes(),
            device_public_key: other_device.verifying_key().to_bytes(),
            device_label: "Other identity home".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut other_cert, &other_root).unwrap();
        let other = HomePeerRegistration {
            schema_version: HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(other_cert.device_public_key),
            relay_service_node_id: hex::encode([65; 32]),
            relay_endpoint: None,
            label: other_cert.device_label.clone(),
            user_public_key: hex::encode(other_cert.user_public_key),
            device_cert: Some(other_cert),
            relay_workspace_ids: vec![7],
            environments: tilo_relay_protocol::RelayEnvironments::default(),
        };
        tilo_files::native::atomic::write_creating_parent(
            &tilo_relay::home_peer_registry_path(identity_root.path()),
            &serde_json::to_vec_pretty(&vec![other.clone()]).unwrap(),
        )
        .unwrap();
        install_personal_projection(
            identity_root.path(),
            identity_dir.path(),
            &personal_payload(&root, &member, [10; 16]),
        )
        .unwrap();
        let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(tilo_relay::home_peer_registry_path(identity_root.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(peers, vec![other]);
    }
    // Phase `home_peer_projection_rejects_future_schemas_and_forged_device_ids`.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Verified home peer".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let mut registration = HomePeerRegistration {
            schema_version: HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(cert.device_public_key),
            relay_service_node_id: hex::encode([83; 32]),
            relay_endpoint: None,
            label: cert.device_label.clone(),
            user_public_key: hex::encode(cert.user_public_key),
            device_cert: Some(cert),
            relay_workspace_ids: vec![7],
            environments: tilo_relay_protocol::RelayEnvironments::default(),
        };
        assert!(valid_home_peer_registration(&registration));

        registration.schema_version = HomePeerRegistration::CURRENT_SCHEMA.saturating_add(1);
        assert!(!valid_home_peer_registration(&registration));

        registration.schema_version = HomePeerRegistration::CURRENT_SCHEMA;
        registration.device_node_id = hex::encode([84; 32]);
        assert!(!valid_home_peer_registration(&registration));
    }
    // Phase `personal_enrollment_staging_keeps_an_existing_device_unchanged` (defined above).
    personal_enrollment_staging_keeps_an_existing_device_unchanged();
    // Phase `expired_requester_lease_blocks_personal_identity_publication`.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let member = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
        let identity_root = tempfile::tempdir().unwrap();
        let identity_parent = tempfile::tempdir().unwrap();
        let identity_dir = identity_parent.path().join("identities");
        let paths = IdentityPaths::under(identity_root.path());
        paths.ensure_directories().unwrap();
        let mut requester = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
        let payload = personal_payload(&root, &member, [12; 16]);
        let (marker_path, marker) = stage_personal_enrollment(
            identity_root.path(),
            &identity_dir,
            &paths,
            &payload,
            Some(lease),
        )
        .unwrap();
        let unrelated = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
        assert!(
            authorize_personal_commit(&marker_path, &marker, Some(unrelated)).is_err(),
            "another live requester cannot adopt a prepared enrollment"
        );
        requester.kill().unwrap();
        requester.wait().unwrap();

        assert!(
            authorize_personal_commit(&marker_path, &marker, Some(lease)).is_err(),
            "a dead compositor cannot cross the durable commit point"
        );
        let mut simulated_ready = marker;
        simulated_ready.commit_ready = true;
        assert!(
            commit_staged_personal_enrollment(
                identity_root.path(),
                &paths,
                &marker_path,
                &simulated_ready,
                |_| lease.validate().map_err(Into::into),
            )
            .is_err()
        );
        assert!(!identity_dir.exists());
        assert!(!paths.user_pubkey.exists());
        assert!(!paths.device_cert.exists());
        assert!(marker_path.exists(), "staging remains retryable");
    }
    // Phase `pair_device_rejects_an_expired_requester_before_creating_identity_state`.
    {
        let mut requester = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let lease = tilo_relay::ProcessLease::capture(requester.id()).unwrap();
        requester.kill().unwrap();
        requester.wait().unwrap();

        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        assert!(
            pair(
                JoinOptions {
                    identity_root: identity_root.path(),
                    label: "New laptop",
                    timeout: 1,
                    kind: EnrollmentKind::PersonalDevice,
                    home_peer_output: None,
                    identity_dir: Some(identity_dir.path()),
                    requester_lease: Some(lease),
                    port: 0,
                },
                None,
            )
            .is_err()
        );
        let paths = IdentityPaths::under(identity_root.path());
        assert!(!paths.device_privkey.exists());
        assert!(!paths.user_pubkey.exists());
        assert!(!paths.device_cert.exists());
    }
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
    tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root).unwrap();
    tilo_user_identity::persist::write_public_key(
        &paths.user_pubkey,
        &root.verifying_key().to_bytes(),
    )
    .unwrap();
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &issuer).unwrap();
    let mut issuer_cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: issuer.verifying_key().to_bytes(),
        device_label: "Trusted laptop".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut issuer_cert, &root).unwrap();
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &issuer_cert).unwrap();
    tilo_user_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&root.verifying_key().to_bytes()),
        &issuer_cert,
    )
    .unwrap();
    tilo_files::native::atomic::write_creating_parent(
        &fleet_path(approver_identities.path(), root.verifying_key().to_bytes()),
        &serde_json::to_vec_pretty(&FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(root.verifying_key().to_bytes()),
            certificates: vec![issuer_cert],
            revocations: Vec::new(),
        })
        .unwrap(),
    )
    .unwrap();
    let home_device = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let mut home_cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: home_device.verifying_key().to_bytes(),
        device_label: "Home peer".into(),
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut home_cert, &root).unwrap();
    register_home_peer(
        approver_root.path(),
        &home_cert,
        [43; 32],
        &[tilo_relay::personal_continuity_workspace_id(
            &root.verifying_key().to_bytes(),
        )],
        None,
    )
    .unwrap();

    // Seed the new device's key so the approval pins exactly this device:
    // an unpinned approver answers whichever device solicits first.
    let new_root = tempfile::tempdir().unwrap();
    let new_identities = tempfile::tempdir().unwrap();
    let new_device = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
    let new_paths = IdentityPaths::under(new_root.path());
    new_paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_device_key(&new_paths.device_privkey, &new_device).unwrap();
    let expected_device = hex::encode(new_device.verifying_key().to_bytes());
    let expected_service = hex::encode(tilo_relay::daemon::relay_service_node_id(&new_device));

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
            relay_workspace_ids: &[],
            expected_device: Some(&expected_device),
            expected_service: Some(&expected_service),
            kind: EnrollmentKind::PersonalDevice,
            // An OS-assigned port keeps concurrent tests off each other's
            // listeners and off the well-known port.
            port: 0,
            ready: Some(&ready_tx),
            approved: None,
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
    let requester = tilo_relay::ProcessLease::capture(std::process::id()).unwrap();
    pair(
        JoinOptions {
            identity_root: new_root.path(),
            label: "New laptop",
            timeout: 60,
            kind: EnrollmentKind::PersonalDevice,
            home_peer_output: None,
            identity_dir: Some(new_identities.path()),
            requester_lease: Some(requester),
            port,
        },
        None,
    )
    .unwrap();
    approval.join().unwrap().unwrap();

    let enrolled_paths = IdentityPaths::under(new_root.path());
    let cert = tilo_user_identity::persist::read_device_cert(&enrolled_paths.device_cert).unwrap();
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
    assert!(!enrolled_paths.user_privkey.exists());
    let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
        &std::fs::read(tilo_relay::home_peer_registry_path(new_root.path())).unwrap(),
    )
    .unwrap();
    assert_eq!(peers.len(), 1);
    assert!(
        peers[0]
            .relay_workspace_ids
            .contains(&tilo_relay::personal_continuity_workspace_id(
                &root.verifying_key().to_bytes()
            ))
    );
    let record = identity_record_path(new_identities.path(), root.verifying_key().to_bytes());
    let identity = runtime_identity::load_from_path(record).unwrap();
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
            &std::fs::read(tilo_relay_protocol::personal_device_peer_path(root))
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

/// Three devices of one person: a member that holds none of the identity's
/// key and may not admit devices finds a new laptop nearby; the device
/// holding the whole key approves the certificate it proposes
/// (`certification_ceremony`); the member then pairs the laptop with it.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one auditable scenario across the three devices"
)]
fn a_device_holding_no_key_pairs_a_device_its_key_holder_approved() {
    use tilo_identity_host::certification_ceremony::{Gathering, Progress, Signer, check_proposal};

    let seed = [31; 32];
    let root = ed25519_dalek::SigningKey::from_bytes(&seed);
    let user = root.verifying_key().to_bytes();
    let key_holder = runtime_identity::load_from_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "display_name": "Key holder", "avatar_color": [1, 2, 3],
            "created_at": "2026-10-22T00:00:00Z", "schema_version": 1,
            "threshold_identity": {
                "single_device": true, "group_public_key": user, "threshold": 1,
                "total_shares": 1, "share_index": 1, "secret_bytes": seed,
            },
        }))
        .unwrap(),
    )
    .unwrap();

    // The member: its own certified key, the person's public record and
    // fleet, no root.
    let member_root = tempfile::tempdir().unwrap();
    let member_identities = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(member_root.path());
    paths.ensure_directories().unwrap();
    let member = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    tilo_user_identity::persist::write_public_key(&paths.user_pubkey, &user).unwrap();
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &member).unwrap();
    let mut member_cert = DeviceCert {
        user_public_key: user,
        device_public_key: member.verifying_key().to_bytes(),
        device_label: "Orange Pi".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut member_cert, &root).unwrap();
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &member_cert).unwrap();
    tilo_user_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&user),
        &member_cert,
    )
    .unwrap();
    tilo_files::native::atomic::write_creating_parent(
        &fleet_path(member_identities.path(), user),
        &serde_json::to_vec_pretty(&FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(user),
            certificates: vec![member_cert],
            revocations: Vec::new(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(
        load_approval_root(member_root.path(), Some(member_identities.path())).is_err(),
        "the member holds none of the key"
    );

    let new_root = tempfile::tempdir().unwrap();
    let new_identities = tempfile::tempdir().unwrap();
    let new_device = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
    let new_paths = IdentityPaths::under(new_root.path());
    new_paths.ensure_directories().unwrap();
    tilo_user_identity::persist::write_device_key(&new_paths.device_privkey, &new_device).unwrap();
    let service = tilo_relay::daemon::relay_service_node_id(&new_device);

    // What the member found nearby, proposed to the key holder, who asks
    // its person and signs.
    let now = now_ns().unwrap();
    let mut gathering = Gathering::new(
        [5; 16],
        DeviceCert {
            user_public_key: user,
            device_public_key: new_device.verifying_key().to_bytes(),
            device_label: "New laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: validity_start_ns(now),
            expires_at_ns: None,
            signature: [0; 64],
        },
        service,
        1,
        1,
    );
    let ask = gathering.ask().clone();
    check_proposal(&ask, user, &|_| false, now).unwrap();
    assert_eq!(
        ask.code(),
        pairing_code(new_device.verifying_key().to_bytes(), service)
    );
    let reply = Signer::default().approve(&key_holder, &ask, true, now);
    let Progress::Certified(approved) = gathering.answer(
        [1; 32],
        tilo_identity_host::certification_ceremony::Answer {
            session: ask.session,
            reply,
        },
    ) else {
        panic!("the whole key certifies with one approval");
    };

    // A member without the approval may not admit at all.
    let refused = load_approval_identity(&ApprovalOptions {
        identity_root: member_root.path(),
        identity_dir: Some(member_identities.path()),
        timeout: 1,
        workspace_cache: std::path::Path::new(""),
        acl: std::path::Path::new(""),
        relay_workspace_ids: &[],
        expected_device: None,
        expected_service: None,
        kind: EnrollmentKind::PersonalDevice,
        port: 0,
        ready: None,
        approved: None,
    });
    assert!(refused.is_err());

    let expected_device = hex::encode(new_device.verifying_key().to_bytes());
    let expected_service = hex::encode(service);
    let server_root = member_root.path().to_path_buf();
    let server_identities = member_identities.path().to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let approval = std::thread::spawn(move || {
        approve(&ApprovalOptions {
            identity_root: &server_root,
            identity_dir: Some(&server_identities),
            timeout: 60,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            relay_workspace_ids: &[],
            expected_device: Some(&expected_device),
            expected_service: Some(&expected_service),
            kind: EnrollmentKind::PersonalDevice,
            port: 0,
            ready: Some(&ready_tx),
            approved: Some(&approved),
        })
    });
    let port = match ready_rx.recv_timeout(tilo_test_env::hang_bound()) {
        Ok(port) => port,
        Err(error) => panic!("the member did not listen ({error}): {:?}", approval.join()),
    };
    pair(
        JoinOptions {
            identity_root: new_root.path(),
            label: "New laptop",
            timeout: 60,
            kind: EnrollmentKind::PersonalDevice,
            home_peer_output: None,
            identity_dir: Some(new_identities.path()),
            requester_lease: Some(tilo_relay::ProcessLease::capture(std::process::id()).unwrap()),
            port,
        },
        None,
    )
    .unwrap();
    approval.join().unwrap().unwrap();

    let cert = tilo_user_identity::persist::read_device_cert(
        &IdentityPaths::under(new_root.path()).device_cert,
    )
    .unwrap();
    assert_eq!(cert.user_public_key, user);
    assert_eq!(cert.device_label, "New laptop");
    verify_device_cert(&cert).unwrap();
    let fleet: FleetProjection =
        serde_json::from_slice(&std::fs::read(fleet_path(member_identities.path(), user)).unwrap())
            .unwrap();
    assert!(
        fleet
            .certificates
            .iter()
            .any(|listed| listed.device_public_key == cert.device_public_key),
        "the member lists the device it introduced"
    );
}

/// A name planted in the shared staging area, as a link to somewhere
/// else, is never staged into: the join takes another, private name, and
/// a commit refuses staging that is not its own private directory.
#[cfg(unix)]
#[test]
fn staging_never_follows_a_planted_name() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let identity_root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let staging = enrollment_stage_dir(identity_root.path());
    std::fs::create_dir_all(&staging).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), staging.join("personal-planted")).unwrap();

    let mut names = ["personal-planted", "personal-fresh"].into_iter();
    let (name, stage_dir) =
        create_stage_dir(identity_root.path(), || names.next().unwrap().to_owned()).unwrap();
    assert_eq!(name, "personal-fresh");
    let metadata = std::fs::symlink_metadata(&stage_dir).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.mode() & 0o777, 0o700);
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    private_stage_dir(identity_root.path(), &name).unwrap();

    let taken = create_stage_dir(identity_root.path(), || "personal-planted".to_owned());
    assert!(taken.is_err());
    assert!(private_stage_dir(identity_root.path(), "personal-planted").is_err());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);

    std::fs::set_permissions(&stage_dir, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(private_stage_dir(identity_root.path(), &name).is_err());
    assert!(private_stage_dir(identity_root.path(), "..").is_err());
}

/// A joining device records only the personal devices whose certificates
/// verify under the identity it joined, are valid now and are not
/// revoked; a malformed fleet records nothing.
#[test]
fn a_joining_device_records_only_certified_live_personal_devices() {
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
            };
            sign_device_cert(&mut cert, signer).unwrap();
            cert
        };
    let approver = cert(1, DeviceCapabilities::primary(), &root);
    let sibling = cert(2, DeviceCapabilities::member(), &root);
    let revoked = cert(3, DeviceCapabilities::member(), &root);
    let host = cert(4, DeviceCapabilities::host(), &root);
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
    let fleet = |revocations: Vec<RevocationEntry>| {
        serde_json::to_vec(&FleetProjection {
            schema_version: 1,
            identity_id: hex::encode(user),
            certificates: vec![
                approver.clone(),
                sibling.clone(),
                revoked.clone(),
                host.clone(),
                administrator.clone(),
                foreign_root.clone(),
                forged.clone(),
                expired.clone(),
            ],
            revocations,
        })
        .unwrap()
    };
    let now = now_ns().unwrap();
    assert_eq!(
        certified_personal_devices(&fleet(vec![revocation.clone()]), user, [10; 32], now).unwrap(),
        vec![[10; 32], [1; 32], [2; 32]]
    );

    let mut tampered = revocation;
    tampered.revoked_device_public_key = [2; 32];
    assert!(certified_personal_devices(&fleet(vec![tampered]), user, [10; 32], now).is_err());
    let other_identity = serde_json::to_vec(&FleetProjection {
        schema_version: 1,
        identity_id: hex::encode(stranger.verifying_key().to_bytes()),
        certificates: vec![approver],
        revocations: Vec::new(),
    })
    .unwrap();
    assert!(certified_personal_devices(&other_identity, user, [10; 32], now).is_err());
    assert!(certified_personal_devices(b"[]", user, [10; 32], now).is_err());
}

#[test]
fn a_failed_first_use_pairing_reports_its_reason_in_the_status_file() {
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

#[test]
fn a_failed_pairing_names_its_cause() {
    let denied = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EACCES))
        .context("publish the enrollment commit");
    assert_eq!(pairing_failure_cause(&denied), "permission_denied");
    let full = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::ENOSPC));
    assert_eq!(pairing_failure_cause(&full), "storage_full");
    let read_only = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EROFS));
    assert_eq!(pairing_failure_cause(&read_only), "read_only");
    let unreachable = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::ENETUNREACH));
    assert_eq!(pairing_failure_cause(&unreachable), "unreachable");
    let closed = anyhow::Error::from(WindowClosed("the window closed"));
    assert_eq!(pairing_failure_cause(&closed), "timed_out");
    assert_eq!(
        pairing_failure_cause(&anyhow::anyhow!("first-use requester exited")),
        "unknown"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn pairing_exchange_is_private_single_use_and_resumable() {
    // Phase `automatic_pair_offer_is_private_to_the_soliciting_server`.
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
    // Phase `nearby_pairing_code_is_stable_and_six_digits`.
    {
        let code = pairing_code([7; 32], [8; 32]);
        assert_eq!(code, pairing_code([7; 32], [8; 32]));
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
        assert_ne!(code, pairing_code([7; 32], [9; 32]));
    }
    // Phase `unrelated_lan_datagram_is_not_a_pairing_message`.
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
    // Phase `enrollment_nonce_is_consumed_once`.
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
    // Phase `approved_home_peer_registration_exposes_distinct_stable_node_ids`.
    {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let cert = payload(&root).cert;
        register_home_peer(dir.path(), &cert, [8; 32], &[2, 9], None).unwrap();
        let json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tilo_relay::home_peer_registry_path(dir.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(
            json[0]["device_node_id"],
            hex::encode(cert.device_public_key)
        );
        assert_eq!(json[0]["relay_service_node_id"], hex::encode([8; 32]));
        assert_eq!(json[0]["relay_workspace_ids"], serde_json::json!([2, 9]));
        assert_eq!(
            std::fs::metadata(tilo_relay::home_peer_registry_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }
    // Phase `invalid_existing_home_peer_registry_is_never_overwritten`.
    {
        let dir = tempfile::tempdir().unwrap();
        let path = tilo_relay::home_peer_registry_path(dir.path());
        tilo_files::native::atomic::write_creating_parent(&path, b"not json").unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        assert!(register_home_peer(dir.path(), &payload(&root).cert, [8; 32], &[2], None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
    }
    // Phase `dropped_response_can_be_retried_with_the_same_authenticated_ack`.
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
    // Phase `dropped_all_acks_can_resume_after_process_restart`.
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

const KAT_RESPONSE_SIGNATURE: &str = "a645d9aa10752fbf8b19d1ccca48b9cf5405d6ec88eb69fea45df36466edda10\
    e8b1bf1f0d85e9f224b21f75a7251771b1482ce6ceed2ce018d43895124e6f0c";
const KAT_ACK: &str = "0404040404040404040404040404040417cb79fb2b4120f2b1ec65e4198d6e08\
    b28e813feb01e4a400839b85e18080ce0aa39056fd697d25fd6d7afbe84dbb81\
    f52d9e543b4aa167cf6472c602c8079a40440274b6b4d4b42e7fd529bbbdf4f0\
    1cfc5dd55c7f0755cf6d7cc821f1cb7fdb971e77b7e4c0b1cffcadc3b3be2aef\
    423888209b7aab3703f999ab61869f950d";

#[test]
fn pairing_crypto_matches_known_answers() {
    // Phase `known_answer_x25519_and_ed25519_conversion`.
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
    // Phase `known_answer_response_signature_verifies`.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let signed = sign_response(payload(&root), Approver::Root(&root)).unwrap();
        assert_eq!(
            hex::encode(postcard::from_bytes::<Response>(&signed).unwrap().signature),
            KAT_RESPONSE_SIGNATURE
        );
        verify_response(&signed, &token(&root)).unwrap();
    }
    // Phase `known_answer_ack_signature`.
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
}

/// A device holding the identity's key answers with the identity-signed
/// `(payload, signature)` every build decodes and checks against the
/// identity's key; only an introducer, which holds none of it, signs with its
/// certified device key and names its certificate. A joining device accepts
/// both, and each only from its signer.
#[test]
fn a_key_holder_answers_as_every_build_expects_and_an_introducer_with_its_certificate() {
    let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let user = root.verifying_key().to_bytes();
    let reply = sign_response(payload(&root), Approver::Root(&root)).unwrap();
    let signature = root
        .sign(&response_preimage(&payload(&root)).unwrap())
        .to_bytes()
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

fn sample_request() -> Request {
    Request {
        token: "bearer".into(),
        device_public_key: ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes(),
        relay_service_node_id: [8; 32],
        label: "NAS".into(),
        kind: EnrollmentKind::HomePeer,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn pairing_envelopes_are_step_bound() {
    // Phase `v2_request_gets_a_v2_response_only_the_requester_opens`.
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let (bytes, response_secret) =
            encrypt_request(&sample_request(), issuer.verifying_key().to_bytes()).unwrap();
        assert_eq!(&bytes[..8], b"TLOENRL\x02");
        let (request, route) = decrypt_request(&bytes, &issuer).unwrap();
        assert_eq!(request.token, "bearer");
        let signed = sign_response(payload(&root), Approver::Root(&root)).unwrap();
        let response =
            encrypt_response(&signed, [3; 16], device.verifying_key().to_bytes(), route).unwrap();
        assert!(v2_body(&response).is_some());
        let opened = decrypt_response(
            &response,
            [3; 16],
            device.verifying_key().to_bytes(),
            &response_secret,
        )
        .unwrap();
        verify_response(&opened, &token(&root)).unwrap();
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
    // Phase `pair_offers_answer_only_the_current_solicitation_magic`.
    {
        let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut solicitation = PairSolicitation {
            magic: PAIR_MAGIC,
            device_public_key: server.verifying_key().to_bytes(),
            relay_service_node_id: [8; 32],
            label: "NAS".into(),
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
    // Phase `version_prefix_decides_the_parser_and_tampering_fails`.
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
    // Phase `v2_envelopes_do_not_open_as_another_step`.
    {
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let offer = encrypt_pair_offer("token", device.verifying_key().to_bytes()).unwrap();
        // Same recipient key, different step label: the request reader
        // must refuse the pairing offer.
        assert!(decrypt_request(&offer, &device).is_err());
    }
}
