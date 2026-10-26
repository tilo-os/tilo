use super::*;
use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Expiry, Scope, Subject};
use std::sync::Mutex;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, persist, sign_device_cert};

fn files_grant(person: [u8; 32]) -> CapOverride {
    CapOverride {
        id: String::default(),
        subject: Subject::User(Ed25519PublicKey(person)),
        scope: Scope::FsPath("/".into()),
        access: Access::Write,
        effect: Effect::Grant,
        expiry: Expiry::Never,
        granted_by: None,
    }
}

struct SyntheticPeer {
    endpoint: iroh::Endpoint,
    lookup: iroh::address_lookup::memory::MemoryLookup,
    identity: fs::sync::FsTransportIdentity,
    root: PathBuf,
}

async fn peer(root: PathBuf, governor: &ed25519_dalek::SigningKey, seed: u8) -> SyntheticPeer {
    let paths = IdentityPaths::under(&root);
    paths.ensure_directories().unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    persist::write_device_key(&paths.device_privkey, &key).unwrap();
    persist::write_public_key(&paths.user_pubkey, &governor.verifying_key().to_bytes()).unwrap();
    let mut cert = DeviceCert {
        user_public_key: governor.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "isolated native recovery fixture".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, governor).unwrap();
    persist::write_device_cert(&paths.device_cert, &cert).unwrap();
    let certified =
        fs::identity::load_existing_certified_mount_identity(&paths, tilo_clock::unix_now_ns())
            .unwrap();
    assert!(!certified.mount.created);
    let lookup = iroh::address_lookup::memory::MemoryLookup::new();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(certified.mount.secret)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .unwrap();
    let identity = fs::sync::FsTransportIdentity::new(
        certified.credentials,
        Arc::new(RwLock::new(tilo_user_identity::RevocationList::default())),
    );
    SyntheticPeer {
        endpoint,
        lookup,
        identity,
        root,
    }
}

fn records(root: &Path, authority: Arc<WorkspaceAuthority>) -> fs::RecordServices {
    fs::RecordServices {
        authority: Arc::new(
            fs::share_authority::ShareAuthority::open(root.join("shares.json")).unwrap(),
        ),
        repository: Arc::new(
            fs::record_repository::RecordRepository::open(root.join("records")).unwrap(),
        ),
        access_registry: Arc::new(
            fs::record_access::RecordAccessRegistry::new(32, 60_000).unwrap(),
        ),
        mutation_sink: None,
        materializer: None,
        availability: None,
        workspace_authority: Some(authority),
    }
}

fn fixture_state(governor: &ed25519_dalek::SigningKey) -> JoinedNamespace {
    let endpoint = iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[74; 32]).public());
    // Replicas may legitimately differ from the originating ticket endpoint.
    let ticket = fs::Ticket {
        node: iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[75; 32]).public()),
        workspace: [76; 32],
    };
    let authority = WorkspaceAuthorityEnvelope::sign(
        ticket.workspace_id(),
        8,
        vec![files_grant(governor.verifying_key().to_bytes())],
        false,
        governor,
    )
    .unwrap();
    JoinedNamespace {
        registration: tilo_relay::transport::ContentPeerRegistration {
            endpoint,
            ticket: ticket.encode().unwrap(),
            relay_service_id: [77; 32],
        },
        authority,
    }
}

#[test]
fn private_recovery_preserves_replica_binding_without_a_second_namespace_tree() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let governor = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
    let person = governor.verifying_key().to_bytes();
    let joined = fixture_state(&governor);
    let workspace = joined.authority.workspace;
    let cache = temp.path().join("recovered");
    prepare_join(&cache, workspace, person, joined).unwrap();
    let restored = fs::Namespace::from_bytes(
        "restart",
        &std::fs::read(cache.join("namespace.bin")).unwrap(),
    )
    .unwrap();
    // The cache holds an empty local projection. The signed record log is
    // the only durable tree authority and is pulled after the mount opens.
    assert!(restored.entry_at("retained.txt").is_none());
    assert_eq!(
        cache.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in [
        "namespace.bin",
        "content-peers.json",
        "workspace-ticket",
        "workspace-authority.bin",
    ] {
        assert_eq!(
            cache.join(file).metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let peers = fs::workspace_authority::load_persisted_content_peers(&cache).unwrap();
    assert_ne!(
        peers[0].endpoint.id,
        peers[0].decoded_ticket().unwrap().unwrap().node.id
    );
    assert_eq!(
        peers[0].decoded_ticket().unwrap().unwrap().workspace_id(),
        workspace
    );
    let before = std::fs::read(cache.join("namespace.bin")).unwrap();
    assert!(prepare_join(&cache, workspace, person, fixture_state(&governor)).is_err());
    assert_eq!(std::fs::read(cache.join("namespace.bin")).unwrap(), before);
}

#[test]
fn recovery_refusals_leave_no_cache_or_replacement_namespace() {
    let temp = tempfile::tempdir().unwrap();
    let governor = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
    let person = governor.verifying_key().to_bytes();
    let cache = temp.path().join("recovery");
    let joined = fixture_state(&governor);
    assert!(prepare_join(&cache, uuid::Uuid::nil(), person, joined).is_err());
    assert!(!cache.exists());
    let joined = fixture_state(&governor);
    let workspace = joined.authority.workspace;
    assert!(prepare_join(&cache, workspace, [72; 32], joined).is_err());
    assert!(!cache.exists());
    let mut joined = fixture_state(&governor);
    joined.authority =
        WorkspaceAuthorityEnvelope::sign(workspace, 9, Vec::new(), false, &governor).unwrap();
    assert!(prepare_join(&cache, workspace, person, joined).is_err());
    assert!(!cache.exists());
    let options = JoinOptions {
        cache_dir: cache.clone(),
        identity_root: temp.path().join("absent-identity"),
        acl: temp.path().join("absent-acl"),
        device: String::new(),
        service: String::new(),
        workspace_id: 1,
        files_workspace_id: workspace,
        registry: None,
        confirm_existing_files_access: false,
    };
    assert!(join(&options).is_err());
    assert!(!options.identity_root.exists());
    assert!(!cache.exists());
}

#[test]
fn recovery_identity_loader_never_creates_missing_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("absent");
    let paths = IdentityPaths::under(&root);
    assert!(
        fs::identity::load_existing_certified_mount_identity(&paths, tilo_clock::unix_now_ns())
            .is_err()
    );
    assert!(!root.exists());
}

struct NativeHost {
    _hosted: fs::Workspace,
    registration: tilo_relay::transport::ContentPeerRegistration,
    authority: Arc<WorkspaceAuthority>,
}

async fn serve_host(
    host: &SyntheticPeer,
    root: &Path,
    governor: &ed25519_dalek::SigningKey,
) -> NativeHost {
    let person = governor.verifying_key().to_bytes();
    let workspace = fs::Ticket::workspace_id_for_topic(*host.endpoint.id().as_bytes());
    let signed =
        WorkspaceAuthorityEnvelope::sign(workspace, 11, vec![files_grant(person)], false, governor)
            .unwrap();
    let authority = Arc::new(
        WorkspaceAuthority::open(
            root.join("host-authority.bin"),
            workspace,
            person,
            person,
            Vec::new(),
        )
        .unwrap(),
    );
    authority.apply(signed).unwrap();
    let store = Arc::new(fs::ContentStore::memory());
    let hydrator = Arc::new(fs::Hydrator::new(host.endpoint.clone(), Arc::clone(&store)));
    let payload_bytes = b"source-only file payload remains remote";
    let payload = store
        .write_content(payload_bytes)
        .await
        .unwrap()
        .expect("nonempty remote payload has a content hash");
    let mut namespace = fs::Namespace::new("native-host-history").unwrap();
    namespace
        .create_file(
            fs::namespace::ROOT_INO,
            "native.txt",
            Some(&payload),
            u64::try_from(payload_bytes.len()).unwrap(),
            0,
        )
        .unwrap();
    let namespace = Arc::new(Mutex::new(namespace));
    let services = records(root, Arc::clone(&authority));
    let (hosted, ticket) = fs::Workspace::host_with_records(
        host.endpoint.clone(),
        host.lookup.clone(),
        store,
        hydrator,
        namespace,
        host.identity.clone(),
        Some(authority.handle()),
        services,
    )
    .await
    .unwrap();
    let registration = tilo_relay::transport::ContentPeerRegistration {
        endpoint: host.endpoint.addr(),
        ticket: ticket.encode().unwrap(),
        relay_service_id: [84; 32],
    };
    NativeHost {
        _hosted: hosted,
        registration,
        authority,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_native_peers_restore_authority_without_a_second_namespace_tree() {
    let temp = tempfile::tempdir().unwrap();
    let governor = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
    let person = governor.verifying_key().to_bytes();
    let host = peer(temp.path().join("host"), &governor, 82).await;
    let client = peer(temp.path().join("client"), &governor, 83).await;
    let hosted = serve_host(&host, temp.path(), &governor).await;
    let workspace = hosted.authority.workspace();
    let registration = hosted.registration.clone();
    let joined = fetch_joined_namespace(
        &client.endpoint,
        &client.identity,
        workspace,
        registration.clone(),
    )
    .await
    .unwrap();
    let cache = temp.path().join("restored");
    prepare_join(&cache, workspace, person, joined).unwrap();
    super::super::check(&client.root, &cache).expect("existing joined authority is ready");
    let restored = fs::Namespace::from_bytes(
        "after-restart",
        &std::fs::read(cache.join("namespace.bin")).unwrap(),
    )
    .unwrap();
    assert!(restored.entry_at("native.txt").is_none());
    assert!(!cache.join("blobs.db").exists());
    assert!(!cache.join("data").exists());
    hosted
        .authority
        .apply(
            WorkspaceAuthorityEnvelope::sign(workspace, 12, Vec::new(), true, &governor).unwrap(),
        )
        .unwrap();
    let refused =
        fetch_joined_namespace(&client.endpoint, &client.identity, workspace, registration).await;
    let error = refused
        .err()
        .expect("revoked native authority must refuse recovery");
    assert!(error.to_string().contains("authority is revoked"));
    drop(hosted);
    host.endpoint.close().await;
    client.endpoint.close().await;
}
