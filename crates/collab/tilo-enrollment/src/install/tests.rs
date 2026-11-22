use super::*;
#[cfg(target_os = "linux")]
use crate::public_identity_document;
#[cfg(target_os = "linux")]
use tilo_server_protocol::{HomePeerRegistration, WorkspaceId, personal_device_peer_path};
#[cfg(target_os = "linux")]
use tilo_test_env::SpawnUnderEnvLock as _;
#[cfg(target_os = "linux")]
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

#[cfg(unix)]
#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test asserts what it restored before it returns"
)]
fn recovery_words_restore_the_person_on_a_headless_device() -> anyhow::Result<()> {
    use tilo_device_identity::persist::read_device_cert;
    use tilo_identity::{provisioning, store};

    let source = tempfile::tempdir()?;
    let identity = source.path().join("identity");
    let source_paths = IdentityPaths::under(source.path().join("device"));
    let storage = provisioning::IdentityStorage::Files;
    let user = provisioning::create_person(&identity, &source_paths, "Ada", "Original", storage)?;
    let words = store::recovery_words(identity.join(provisioning::PRIMARY_IDENTITY_FILE))?;
    let original = read_device_cert(&source_paths.device_cert)?;

    let target = tempfile::tempdir()?;
    let identity = target.path().join("identity");
    let target_paths = IdentityPaths::under(target.path().join("device"));
    let profile =
        restore_from_recovery_words(&identity, &target_paths, &words, Some("Headless"), storage)?;
    let cert = read_device_cert(&target_paths.device_cert)?;
    assert_eq!(profile.user_public_key, user);
    assert_eq!(cert.user_public_key, user);
    assert_eq!(cert.device_label, "Headless");
    assert_ne!(cert.device_public_key, original.device_public_key);
    provisioning::verify_collaboration_identity(&identity, &target_paths, now_ns()?)?;

    let repeated = restore_from_recovery_words(&identity, &target_paths, &words, None, storage)?;
    assert_eq!(repeated, profile);
    assert_eq!(read_device_cert(&target_paths.device_cert)?, cert);
    let wrong = restore_from_recovery_words(
        source.path().join("identity").as_path(),
        &source_paths,
        "not a valid recovery key",
        None,
        storage,
    )
    .expect_err("invalid words");
    assert!(store::recovery_words_error(&wrong).is_some());
    assert_eq!(read_device_cert(&source_paths.device_cert)?, original);
    Ok(())
}

#[cfg(target_os = "linux")]
fn personal_payload(
    root: &ed25519_dalek::SigningKey,
    device: &ed25519_dalek::SigningKey,
    nonce: [u8; 16],
) -> anyhow::Result<ResponsePayload> {
    let user = root.verifying_key().to_bytes();
    let mut cert = DeviceCert {
        user_public_key: user,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "New laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root)?;
    let mut fleet = DeviceFleet::new(hex::encode(user));
    fleet.certificates.push(cert.clone());
    Ok(ResponsePayload {
        nonce,
        recipient_device_public_key: cert.device_public_key,
        server_node_id: [8; 32],
        cert,
        home_peer_toml: None,
        public_identity_json: Some(public_identity_document(user, "My Tilo identity")?),
        fleet_json: Some(serde_json::to_vec_pretty(&fleet)?),
        home_peers_json: Some(b"[]".to_vec()),
    })
}

#[cfg(target_os = "linux")]
tilo_test_env::scenario! {
    fn personal_projection_publishes_only_live_valid_identity_state;

    phase an_unconfirmed_enrollment_preserves_an_existing_device_and_a_confirmed_one_recovers;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let member = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let old_root = ed25519_dalek::SigningKey::from_bytes(&[53; 32]);
        let old_device = ed25519_dalek::SigningKey::from_bytes(&[54; 32]);
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        paths.ensure_directories().unwrap();
        tilo_device_identity::persist::write_public_key(
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
            issuer: None,
        };
        sign_device_cert(&mut old_cert, &old_root).unwrap();
        tilo_device_identity::persist::write_device_cert(&paths.device_cert, &old_cert).unwrap();
        let before_key = std::fs::read(&paths.user_pubkey).unwrap();
        let before_cert = std::fs::read(&paths.device_cert).unwrap();

        // Keep one live requester on its stdin pipe. Killing a shell loop
        // leaves its timer child holding the test's output handles.
        let mut requester = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        let lease = ProcessLease::capture(requester.id()).unwrap();
        let payload = personal_payload(&root, &member, [9; 16]).unwrap();
        let install = PersonalInstall::new(identity_root.path(), identity_dir.path(), None);
        let error = install
            .install(&payload, Some(lease), || bail!("approval timed out"))
            .unwrap_err();
        assert_eq!(error.to_string(), "approval timed out");
        assert_eq!(std::fs::read(&paths.user_pubkey).unwrap(), before_key);
        assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
        let markers = enrollment_commit_dir(identity_root.path());
        assert_eq!(std::fs::read_dir(&markers).unwrap().count(), 1);
        // Even a new live requester cannot adopt a merely prepared response.
        let retry_lease = ProcessLease::capture(std::process::id()).unwrap();
        install.recover(Some(retry_lease)).unwrap();
        assert_eq!(std::fs::read(&paths.user_pubkey).unwrap(), before_key);
        assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
        let personal_peers = personal_device_peer_path(identity_root.path());
        assert!(
            !personal_peers.exists(),
            "an unconfirmed enrollment records no personal device"
        );

        let error = install
            .install_with_checkpoint(
                &payload,
                Some(lease),
                || Ok(()),
                |checkpoint| {
                    if checkpoint == PersonalCommitCheckpoint::DefaultCert {
                        bail!("simulated crash before default certificate");
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "simulated crash before default certificate");
        assert_eq!(std::fs::read(&paths.device_cert).unwrap(), before_cert);
        // First use reads a selected joined identity as a finished join;
        // an interrupted commit has selected nothing yet.
        assert_eq!(
            tilo_identity_client::resolve_active_identity(identity_dir.path()),
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
        // A root-run commit may run under a strict umask (the pairing
        // helper's, or a script's); the public files stay readable.
        // SAFETY: umask has no preconditions.
        let umask = unsafe { libc::umask(0o077) };
        let recovered = install.recover(Some(retry_lease));
        // SAFETY: as above, restoring the previous mask.
        unsafe { libc::umask(umask) };
        recovered.unwrap();
        assert_eq!(std::fs::read_dir(markers).unwrap().count(), 1);
        for public in [
            paths.user_pubkey.clone(),
            paths.device_cert.clone(),
            paths.device_cert_for_user(&root.verifying_key().to_bytes()),
            personal_peers.clone(),
        ] {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&public).unwrap().permissions().mode() & 0o777,
                tilo_device_identity::persist::PUBLIC_FILE_MODE,
                "{} is not readable to the session",
                public.display()
            );
        }
        assert_eq!(
            std::fs::read(&paths.user_pubkey).unwrap(),
            root.verifying_key().to_bytes()
        );
        assert_eq!(
            tilo_device_identity::persist::read_device_cert(&paths.device_cert)
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
            tilo_identity_client::resolve_active_identity(identity_dir.path()).is_some(),
            "the finished join selects its identity"
        );
        // What the runtime starts the identity from is the registry this
        // wrote, read by the code that reads it: a field it does not know
        // keeps the member it holds from starting at all.
        let registry = tilo_identity::registry::IdentityRegistryStore::at(identity_dir.path())
            .load()
            .expect("the registry this join wrote is one the runtime reads");
        let (id, _) = registry
            .identities
            .iter()
            .next()
            .expect("the join registered the identity it selected");
        assert_eq!(*id, hex::encode(root.verifying_key().to_bytes()));
    }
    phase personal_projection_loads_without_root_signing_material;
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
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let payload = ResponsePayload {
            nonce: [1; 16],
            recipient_device_public_key: cert.device_public_key,
            server_node_id: [8; 32],
            cert: cert.clone(),
            home_peer_toml: None,
            public_identity_json: Some(public_identity_document(user, "My Tilo identity").unwrap()),
            fleet_json: Some({
                let mut fleet = DeviceFleet::new(hex::encode(user));
                fleet.certificates.push(cert);
                serde_json::to_vec_pretty(&fleet).unwrap()
            }),
            home_peers_json: Some(b"[]".to_vec()),
        };
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let record = PersonalInstall::new(identity_root.path(), identity_dir.path(), None)
            .install(&payload, None, || Ok(()))
            .unwrap();
        // The joined identity is what this device acts for from the next
        // login on, not an absent default that asks to create one.
        assert_eq!(
            tilo_identity_client::resolve_active_identity(identity_dir.path()),
            Some(record.clone())
        );
        let loaded = tilo_identity::store::load_from_path(record).unwrap();
        assert_eq!(loaded.group_public_key(), user);
        assert!(!loaded.is_single_device());
        assert!(loaded.sign(b"root-only operation").is_err());
        assert_eq!(
            std::fs::read(tilo_server_protocol::home_peer_registry_path(identity_root.path())).unwrap(),
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
                std::fs::metadata(
                    fleet_store(identity_root.path(), identity_dir.path(), None)
                        .fleet_file(&hex::encode(user))
                )
                .unwrap()
                .uid(),
                uid
            );
        }
    }
    phase personal_projection_merges_and_preserves_unrelated_home_peers;
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
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut other_cert, &other_root).unwrap();
        let other = HomePeerRegistration {
            schema_version: HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(other_cert.device_public_key),
            server_node_id: hex::encode([65; 32]),
            server_endpoint: None,
            label: other_cert.device_label.clone(),
            user_public_key: hex::encode(other_cert.user_public_key),
            device_cert: Some(other_cert),
            server_workspace_ids: vec![WorkspaceId::from_u128(7)],
        };
        tilo_files::native::atomic::write_creating_parent(
            &tilo_server_protocol::home_peer_registry_path(identity_root.path()),
            &serde_json::to_vec_pretty(&vec![other.clone()]).unwrap(),
        )
        .unwrap();
        PersonalInstall::new(identity_root.path(), identity_dir.path(), None)
            .install(&personal_payload(&root, &member, [10; 16]).unwrap(), None, || Ok(()))
            .unwrap();
        let peers: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(tilo_server_protocol::home_peer_registry_path(identity_root.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(peers, vec![other]);
    }
    phase home_peer_projection_rejects_future_schemas_and_forged_device_ids;
    {
        let root = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Verified home peer".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let mut registration = HomePeerRegistration {
            schema_version: HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(cert.device_public_key),
            server_node_id: hex::encode([83; 32]),
            server_endpoint: None,
            label: cert.device_label.clone(),
            user_public_key: hex::encode(cert.user_public_key),
            device_cert: Some(cert),
            server_workspace_ids: vec![WorkspaceId::from_u128(7)],
        };
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let install = PersonalInstall::new(identity_root.path(), identity_dir.path(), None);
        let mut payload = personal_payload(&root, &device, [11; 16]).unwrap();
        payload.home_peers_json = Some(serde_json::to_vec(&[&registration]).unwrap());
        install.install(&payload, None, || Ok(())).unwrap();
        let mut rejected = |registration: &HomePeerRegistration| {
            payload.home_peers_json = Some(serde_json::to_vec(&[registration]).unwrap());
            let error = install.install(&payload, None, || panic!("invalid projection reached confirmation")).unwrap_err();
            assert_eq!(
                error.to_string(),
                "the approval's home peers are not a registry this build accepts"
            );
        };

        registration.schema_version = HomePeerRegistration::CURRENT_SCHEMA.saturating_add(1);
        rejected(&registration);

        registration.schema_version = HomePeerRegistration::CURRENT_SCHEMA;
        registration.device_node_id = hex::encode([84; 32]);
        rejected(&registration);
    }
    phase expired_requester_lease_blocks_personal_identity_publication;
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
            .spawn_locked()
            .unwrap();
        let lease = ProcessLease::capture(requester.id()).unwrap();
        let payload = personal_payload(&root, &member, [12; 16]).unwrap();
        let install = PersonalInstall::new(identity_root.path(), &identity_dir, None);
        let error = install
            .install(&payload, Some(lease), || {
                requester.kill().unwrap();
                requester.wait().unwrap();
                Ok(())
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "the first-use requester exited; nearby pairing was revoked"
        );
        let markers = enrollment_commit_dir(identity_root.path());
        let entry = std::fs::read_dir(&markers).unwrap().next().unwrap().unwrap();
        let mut marker: PersonalEnrollmentMarker =
            serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
        assert!(!marker.commit_ready, "a dead requester cannot authorize commit");
        // A previous requester may have crossed the durable boundary. The
        // retry still needs a live lease before publishing even the record.
        marker.commit_ready = true;
        std::fs::write(entry.path(), serde_json::to_vec(&marker).unwrap()).unwrap();
        assert_eq!(
            install.recover(Some(lease)).unwrap_err().to_string(),
            "the first-use requester exited; nearby pairing was revoked"
        );
        assert!(!identity_dir.exists());
        assert!(!paths.user_pubkey.exists());
        assert!(!paths.device_cert.exists());
        assert!(entry.path().exists(), "staging remains retryable");
    }
    phase explicit_uid_scopes_the_identity_directory_writes;
    {
        use std::os::unix::fs::MetadataExt as _;

        let root = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
        let member = ed25519_dalek::SigningKey::from_bytes(&[74; 32]);
        let identity_root = tempfile::tempdir().unwrap();
        let identity_dir = tempfile::tempdir().unwrap();
        let payload = personal_payload(&root, &member, [13; 16]).unwrap();
        let paths = IdentityPaths::under(identity_root.path());
        let uid = rustix::process::geteuid().as_raw();
        let install = |directory, uid| {
            PersonalInstall::new(identity_root.path(), directory, Some(uid))
                .install(&payload, None, || Ok(()))
        };
        let error = install(identity_dir.path(), uid.wrapping_add(1)).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );

        let link = identity_root.path().join("identities");
        std::os::unix::fs::symlink(identity_dir.path(), &link).unwrap();
        let error = install(&link, uid).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read_dir(identity_dir.path()).unwrap().count(), 0);
        assert!(!paths.user_pubkey.exists());
        assert!(!paths.device_cert.exists());

        let record = install(identity_dir.path(), uid).unwrap();
        assert_eq!(std::fs::metadata(record).unwrap().uid(), uid);
        assert!(paths.device_cert.exists());
        assert_eq!(tilo_identity_client::resolve_active_identity(identity_dir.path()), None);
    }

}

/// A name planted in the shared staging area, as a link to somewhere
/// else, is never staged into: the join takes another, private name, and
/// a commit refuses staging that is not its own private directory.
#[test]
fn staging_never_follows_a_planted_name() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let identity_root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let staging = enrollment_stage_dir(identity_root.path());
    std::fs::create_dir_all(&staging).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), staging.join("personal-planted")).unwrap();

    let mut names = ["personal-planted", "personal-fresh"].into_iter();
    let (name, stage_dir) = create_stage_dir(identity_root.path(), || {
        Ok(names.next().unwrap().to_owned())
    })
    .unwrap();
    assert_eq!(name, "personal-fresh");
    let metadata = std::fs::symlink_metadata(&stage_dir).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.mode() & 0o777, 0o700);
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    private_stage_dir(identity_root.path(), &name).unwrap();

    let taken = create_stage_dir(identity_root.path(), || Ok("personal-planted".to_owned()));
    assert!(taken.is_err());
    assert!(private_stage_dir(identity_root.path(), "personal-planted").is_err());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);

    std::fs::set_permissions(&stage_dir, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(private_stage_dir(identity_root.path(), &name).is_err());
    assert!(private_stage_dir(identity_root.path(), "..").is_err());
}
