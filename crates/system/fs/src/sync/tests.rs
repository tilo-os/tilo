use super::*;
use crate::acl::WorkspaceAcl;
use crate::history::FileHistoryStore;
use crate::namespace::ROOT_INO;
use crate::record_publisher::RecordPublisher;
use crate::record_replication::{
    RecordEnvelopeV1, RecordOperationV1, test_authorizers::AllowActor,
};
use collab_types::{Access, CapOverride, Ed25519PublicKey, Effect, Scope, Subject};
use fs_protocol::{
    ContentHash, ContentRevisionV1, FILE_BINDING_VERSION, FILE_MUTATION_TRANSACTION_VERSION,
    FileActor, FileBindingV1, FileMetadataV1, FileMutationOperation, FileMutationParticipant,
    FileMutationParticipantRole, FileMutationTransaction, FileMutationTransactionManifest,
    FileNodeKind, FileProvenance, FileRecordId, FileVersionId, FileVersionV1, IndexStatus,
};
use std::time::Duration;

async fn bind_certified_endpoint() -> (Endpoint, MemoryLookup, FsTransportIdentity) {
    bind_certified_test_endpoint().await
}

#[tokio::test]
async fn thumbnail_prefetch_isolated_retries_without_another_namespace_change() {
    let source_endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
    let source_store = ContentStore::memory();
    let ready_thumbnail = source_store.ingest("ready preview").await.unwrap();
    let source_router = Router::builder(source_endpoint.clone())
        .accept(iroh_blobs::ALPN, source_store.blobs_protocol(None))
        .spawn();

    // This hash is valid, but its serving endpoint is intentionally absent
    // during the first pass, modelling an auth warm-up/offline source.
    let late_store = ContentStore::memory();
    let late_thumbnail = late_store.ingest("late preview").await.unwrap();
    let client_endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
    let local_store = Arc::new(ContentStore::memory());
    let hydrator = Hydrator::new(client_endpoint.clone(), Arc::clone(&local_store));
    hydrator.add_peer(source_endpoint.addr());
    let mut namespace = Namespace::new("preview-retry").unwrap();
    let ready = namespace
        .create_file(ROOT_INO, "ready.png", None, 0, 0)
        .unwrap();
    let late = namespace
        .create_file(ROOT_INO, "late.png", None, 0, 0)
        .unwrap();
    assert!(namespace.set_thumb(ready, &ready_thumbnail));
    assert!(namespace.set_thumb(late, &late_thumbnail));
    let namespace = Arc::new(Mutex::new(namespace));
    let mut pending = HashMap::new();

    prefetch_namespace_thumbnails(&namespace, &hydrator, &mut pending, true)
        .await
        .unwrap();
    assert!(local_store.has(ready_thumbnail).await);
    assert!(pending.contains_key(&late_thumbnail));

    let late_endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
    let late_router = Router::builder(late_endpoint.clone())
        .accept(iroh_blobs::ALPN, late_store.blobs_protocol(None))
        .spawn();
    hydrator.add_peer(late_endpoint.addr());
    pending.insert(late_thumbnail, (0, tokio::time::Instant::now()));
    prefetch_namespace_thumbnails(&namespace, &hydrator, &mut pending, false)
        .await
        .unwrap();
    assert!(local_store.has(late_thumbnail).await);
    assert!(pending.is_empty());

    late_router.shutdown().await.unwrap();
    source_router.shutdown().await.unwrap();
    late_endpoint.close().await;
    source_endpoint.close().await;
    client_endpoint.close().await;
}

/// The namespace's current generation: its Automerge heads, which change
/// with every change (reading them commits any open transaction).
fn namespace_generation(namespace: &Arc<Mutex<Namespace>>) -> Vec<automerge::ChangeHash> {
    namespace.lock().unwrap().heads()
}

#[test]
fn tickets_round_trip_and_clean_merges_change_generation() {
    // Phase `ticket_roundtrips`.
    {
        let id = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
        let ticket = Ticket {
            node: EndpointAddr::new(id),
            workspace: [3u8; 32],
        };
        let decoded = Ticket::decode(&ticket.encode().unwrap()).unwrap();
        assert_eq!(decoded.node.id, id);
        assert_eq!(decoded.workspace, [3u8; 32]);
        assert_eq!(decoded.workspace_id(), ticket.workspace_id());
        assert_eq!(ticket.workspace_id(), uuid::Uuid::from_bytes([3; 16]));
    }
    // Phase `clean_remote_merge_changes_generation_even_without_resolution_delta`.
    {
        let mut owner = Namespace::new("owner-preview").unwrap();
        owner
            .create_file(ROOT_INO, "preview.png", None, 0, 1)
            .unwrap();
        let document = owner.save();
        let joined = Arc::new(Mutex::new(Namespace::new("joined-preview").unwrap()));
        let before = namespace_generation(&joined);
        let resolve = joined
            .lock()
            .unwrap()
            .merge_remote_and_resolve(&document)
            .unwrap();
        assert!(resolve.is_empty(), "ordinary merge needs no repair delta");
        assert_ne!(before, namespace_generation(&joined));
    }
}

/// The workspace's policy: `entitled` get the run of the tree, nothing
/// said about anybody else. Direct entries rather than capabilities so
/// the test states its grant outright; `reads_anything` is asked the
/// same question either way (see `acl::tests` for the capability route).
fn policy_granting(entitled: &[IdentityKey]) -> AclHandle {
    let mut acl = WorkspaceAcl::new();
    for id in entitled {
        acl.grant_read(*id, "/");
    }
    AclHandle::new(acl)
}

fn policy_read_write(writers: &[IdentityKey], readers: &[IdentityKey]) -> AclHandle {
    let mut acl = WorkspaceAcl::new();
    for id in writers {
        acl.grant_read_write(*id, "/");
    }
    for id in readers {
        acl.grant_read(*id, "/");
    }
    AclHandle::new(acl)
}

fn signed_read_authority(
    workspace: uuid::Uuid,
    revision: u64,
    owner: IdentityKey,
    collaborator: IdentityKey,
    revoke_collaborator: bool,
    governor: &ed25519_dalek::SigningKey,
) -> crate::workspace_authority::WorkspaceAuthorityEnvelope {
    crate::workspace_authority::WorkspaceAuthorityEnvelope::sign(
        workspace,
        revision,
        vec![
            CapOverride::new(
                Subject::User(Ed25519PublicKey(owner)),
                Scope::FsPath("/".into()),
                Effect::Grant,
                Access::Read,
            ),
            CapOverride::new(
                Subject::User(Ed25519PublicKey(collaborator)),
                Scope::FsPath("/".into()),
                if revoke_collaborator {
                    Effect::Restrict
                } else {
                    Effect::Grant
                },
                Access::Read,
            ),
        ],
        false,
        governor,
    )
    .unwrap()
}

async fn publish_workspace_authority(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    workspace: uuid::Uuid,
    envelope: crate::workspace_authority::WorkspaceAuthorityEnvelope,
) {
    assert_eq!(
        submit_workspace_authority(
            endpoint,
            remote,
            identity,
            workspace,
            Some(envelope.clone())
        )
        .await
        .unwrap(),
        Some(envelope),
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn signed_authority_and_blob_acls_scope_access_to_live_certified_readers() {
    // Phase `blob_authority_is_scoped_to_live_readable_paths`.
    {
        let peer = identity_of(iroh::SecretKey::generate().public());
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(peer, "/shared");
        let handle = AclHandle::new(acl);
        let policy = Some(handle.clone());

        let mut ns = Namespace::new("host").unwrap();
        let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
        let private = ns.create_dir(ROOT_INO, "private", 1).unwrap();
        let store = ContentStore::memory();
        let allowed = store
            .write_content(b"readable bytes")
            .await
            .unwrap()
            .unwrap();
        let allowed_manifest = crate::content::Manifest::decode(
            &store.local(allowed).await.expect("allowed manifest stored"),
        )
        .unwrap();
        let allowed_chunk = iroh_blobs::Hash::from(allowed_manifest.chunks()[0].hash);
        let denied = store
            .write_content(b"private bytes")
            .await
            .unwrap()
            .unwrap();
        let denied_manifest = crate::content::Manifest::decode(
            &store.local(denied).await.expect("private manifest stored"),
        )
        .unwrap();
        let denied_chunk = iroh_blobs::Hash::from(denied_manifest.chunks()[0].hash);
        let thumbnail = iroh_blobs::Hash::from([2; 32]);
        let ino = ns
            .create_file(shared, "allowed.png", Some(&allowed), 1, 1)
            .unwrap();
        assert!(ns.set_thumb(ino, &thumbnail));
        ns.create_file(private, "denied.txt", Some(&denied), 1, 1)
            .unwrap();
        let ns = Arc::new(Mutex::new(ns));

        assert!(peer_may_read_blob(&policy, &ns, &store, &peer, allowed).await);
        assert!(peer_may_read_blob(&policy, &ns, &store, &peer, allowed_chunk).await);
        assert!(peer_may_read_blob(&policy, &ns, &store, &peer, thumbnail).await);
        assert!(!peer_may_read_blob(&policy, &ns, &store, &peer, denied).await);
        assert!(!peer_may_read_blob(&policy, &ns, &store, &peer, denied_chunk).await);
        assert!(
            !peer_may_read_whole_workspace(&policy, &peer),
            "a subtree grant must not unlock the monolithic namespace"
        );

        handle.replace(WorkspaceAcl::new());
        assert!(
            !peer_may_read_blob(&policy, &ns, &store, &peer, allowed_chunk).await,
            "revocation must affect the next request on a live connection"
        );
    }
    // Phase `signed_authority_alpn_updates_nas_and_targeted_revoke_keeps_owner`.
    {
        use crate::workspace_authority::WorkspaceAuthority;
        let (nas_endpoint, _lookup, nas_identity) = bind_certified_endpoint().await;
        let (owner_endpoint, _lookup, owner_identity, owner_root, _owner_device) =
            bind_certified_test_endpoint_with_root().await;
        let (_collab_endpoint, _lookup, collab_identity, collab_root, _collab_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace = uuid::Uuid::new_v4();
        let temp = tempfile::tempdir().unwrap();
        let nas_authority = Arc::new(
            WorkspaceAuthority::open(
                temp.path().join("nas-authority.bin"),
                workspace,
                owner_root.verifying_key().to_bytes(),
                collab_root.verifying_key().to_bytes(),
                Vec::new(),
            )
            .unwrap(),
        );
        let router = Router::builder(nas_endpoint.clone())
            .accept(
                TILO_FS_AUTHORITY_ALPN,
                AuthorityProtocol {
                    endpoint: nas_endpoint.clone(),
                    identity: nas_identity,
                    policy: None,
                    authority: nas_authority.clone(),
                    workspace_id: workspace,
                },
            )
            .spawn();
        let owner = owner_root.verifying_key().to_bytes();
        let collaborator = collab_root.verifying_key().to_bytes();
        let grant = signed_read_authority(workspace, 1, owner, collaborator, false, &owner_root);
        let owner_authority = WorkspaceAuthority::open(
            temp.path().join("owner-authority.bin"),
            workspace,
            owner,
            owner,
            Vec::new(),
        )
        .unwrap();
        owner_authority.apply(grant).unwrap();
        submit_workspace_authority(
            &owner_endpoint,
            nas_endpoint.addr(),
            &owner_identity,
            workspace,
            owner_authority.current(),
        )
        .await
        .unwrap();
        assert_eq!(nas_authority.current().unwrap().revision, 1);
        assert!(
            nas_authority
                .handle()
                .permits(&collaborator, "/shared", crate::Access::Read)
        );

        let revoked = signed_read_authority(workspace, 2, owner, collaborator, true, &owner_root);
        owner_authority.apply(revoked.clone()).unwrap();
        submit_workspace_authority(
            &owner_endpoint,
            nas_endpoint.addr(),
            &owner_identity,
            workspace,
            owner_authority.current(),
        )
        .await
        .unwrap();
        assert!(
            !nas_authority
                .handle()
                .permits(&collaborator, "/shared", crate::Access::Read)
        );
        let owner_view = WorkspaceAuthority::open(
            temp.path().join("owner-view.bin"),
            workspace,
            owner,
            owner,
            Vec::new(),
        )
        .unwrap();
        owner_view.apply(revoked).unwrap();
        assert!(
            owner_view
                .handle()
                .permits(&owner, "/shared", crate::Access::Read)
        );
        router.shutdown().await.unwrap();
        nas_endpoint.close().await;
        owner_endpoint.close().await;
        drop(collab_identity);
    }
    // Phase `blob_acl_resolves_only_a_live_certified_device_to_its_person`.
    {
        use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

        let (_server_ep, _server_lookup, server_identity, _server_root, _server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, _client_lookup, client_identity, client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let (_stranger_ep, _stranger_lookup, stranger_identity) = bind_certified_endpoint().await;

        let client_cert = client_identity.credentials().cert().clone();
        let peer = AuthenticatedFsPeer {
            user_public_key: client_cert.user_public_key,
            device_public_key: client_cert.device_public_key,
            device_label: client_cert.device_label.clone(),
            capabilities: client_cert.capabilities.clone(),
            cert: client_cert,
        };
        let policy = policy_granting(&[peer.user_public_key]);
        let store = ContentStore::memory();
        let manifest = store
            .write_content(b"person-owned blob")
            .await
            .unwrap()
            .unwrap();
        let mut namespace = Namespace::new("host").unwrap();
        namespace
            .create_file(ROOT_INO, "person.txt", Some(&manifest), 17, 1)
            .unwrap();
        let namespace = Arc::new(Mutex::new(namespace));

        assert_eq!(
            server_identity.certified_person_for_device(peer.device_public_key),
            None,
            "a presented but unauthenticated device is never mapped to a person"
        );
        assert_eq!(
            server_identity.certified_person_for_device(stranger_identity.device_public_key()),
            None,
            "an unrelated certified device has not authenticated to this mount"
        );
        assert!(
            !peer_may_read_blob_with_records(
                &Some(policy.clone()),
                &namespace,
                &store,
                &peer.device_public_key,
                None,
                manifest,
                None,
            )
            .await,
            "the ACL intentionally contains no device-key grant"
        );

        // `authenticate_connection` invokes this only after the certificate,
        // challenge signature, endpoint binding, and revocation checks pass.
        server_identity.remember_peer(&peer, client_ep.addr());
        let person = server_identity
            .certified_person_for_device(peer.device_public_key)
            .expect("the authenticated device must resolve to its certified human root");
        assert_eq!(person, peer.user_public_key);
        assert!(
            peer_may_read_blob_with_records(
                &Some(policy.clone()),
                &namespace,
                &store,
                &peer.device_public_key,
                Some(person),
                manifest,
                None,
            )
            .await,
            "a person-only ACL authorizes that person's distinct certified device"
        );

        let mut revocation = RevocationEntry {
            user_public_key: peer.user_public_key,
            revoked_device_public_key: peer.device_public_key,
            revoked_at_ns: unix_now_ns().saturating_sub(1),
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &client_root).unwrap();
        server_identity
            .revocations()
            .write()
            .unwrap()
            .insert(revocation)
            .unwrap();
        assert_eq!(
            server_identity.certified_person_for_device(peer.device_public_key),
            None,
            "revocation is re-evaluated for the next blob request"
        );
        assert!(
            !peer_may_read_blob_with_records(
                &Some(policy.clone()),
                &namespace,
                &store,
                &peer.device_public_key,
                server_identity.certified_person_for_device(peer.device_public_key),
                manifest,
                None,
            )
            .await
        );
    }
}

/// A content request is not trusted merely because its hash is known: the
/// collaborator's blob request is served by the workspace's guarded
/// iroh-blobs handler for as long as the signed authority entitles it.
async fn authorized_collaborator_reads_content(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    hash: Hash,
    expected: &[u8],
) -> Hydrator {
    let hydrator = Hydrator::new(endpoint.clone(), Arc::new(ContentStore::memory()));
    hydrator.add_peer(remote);
    assert_eq!(
        tilo_test_env::within(
            "the authenticated collaborator blob request",
            hydrator.read(hash, 0, expected.len() as u64)
        )
        .await
        .unwrap()
        .as_slice(),
        expected,
    );
    hydrator
}

/// Replacing the signed authority while that certified relationship is live
/// denies the collaborator's *next* request.
async fn assert_revoked_collaborator_is_denied(hydrator: &Hydrator, hash: Hash, bytes: &[u8]) {
    assert!(
        tilo_test_env::within(
            "the revoked collaborator blob request",
            hydrator.read(hash, 0, bytes.len() as u64)
        )
        .await
        .is_err(),
        "an already-authenticated collaborator is denied by the revised authority"
    );
}

async fn assert_owner_retains_content_access(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    hash: Hash,
    expected: &[u8],
) {
    let hydrator = Hydrator::new(endpoint.clone(), Arc::new(ContentStore::memory()));
    hydrator.add_peer(remote);
    assert_eq!(
        tilo_test_env::within(
            "the owner blob request",
            hydrator.read(hash, 0, expected.len() as u64)
        )
        .await
        .unwrap()
        .as_slice(),
        expected,
        "targeted revocation leaves the governor's authenticated content access intact"
    );
}

async fn publish_post_revoke_content(
    store: &ContentStore,
    namespace: &Arc<Mutex<Namespace>>,
    bytes: &[u8],
) -> Hash {
    let hash = store.write_content(bytes).await.unwrap().unwrap();
    let mut namespace = namespace.lock().unwrap();
    namespace
        .create_file(
            ROOT_INO,
            "after-revoke.txt",
            Some(&hash),
            bytes.len() as u64,
            2,
        )
        .unwrap();
    namespace.save_incremental();
    hash
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn revoked_or_foreign_peers_are_denied_and_owned_devices_bootstrap() {
    // Phase `signed_targeted_revoke_denies_an_authenticated_collaborator_blob_request`.
    {
        use crate::workspace_authority::WorkspaceAuthority;

        let temp = tempfile::tempdir().unwrap();
        let (nas_endpoint, nas_lookup, nas_identity) = bind_certified_endpoint().await;
        let (owner_endpoint, _owner_lookup, owner_identity, owner_root, _owner_device) =
            bind_certified_test_endpoint_with_root().await;
        let (collab_endpoint, _collab_lookup, collab_identity, collab_root, _collab_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = Ticket::workspace_id_for_topic(*nas_endpoint.id().as_bytes());
        let (owner, collaborator) = (
            owner_root.verifying_key().to_bytes(),
            collab_root.verifying_key().to_bytes(),
        );
        let authority = Arc::new(
            WorkspaceAuthority::open(
                temp.path().join("nas-authority.bin"),
                workspace_id,
                owner,
                collaborator,
                Vec::new(),
            )
            .unwrap(),
        );
        let mut services = record_services(temp.path());
        services.workspace_authority = Some(authority.clone());
        let nas_store = Arc::new(ContentStore::memory());
        let nas_hydrator = Arc::new(Hydrator::new(nas_endpoint.clone(), nas_store.clone()));
        let nas_namespace = Arc::new(Mutex::new(Namespace::new("nas").unwrap()));
        let first = b"collaborator is initially entitled";
        let first_hash = nas_store.write_content(first).await.unwrap().unwrap();
        nas_namespace
            .lock()
            .unwrap()
            .create_file(
                ROOT_INO,
                "before-revoke.txt",
                Some(&first_hash),
                first.len() as u64,
                1,
            )
            .unwrap();
        let (_nas_workspace, ticket) = Workspace::host_with_records(
            nas_endpoint.clone(),
            nas_lookup,
            nas_store.clone(),
            nas_hydrator,
            nas_namespace.clone(),
            nas_identity,
            Some(authority.handle()),
            services,
        )
        .unwrap();
        let grant = signed_read_authority(workspace_id, 1, owner, collaborator, false, &owner_root);
        publish_workspace_authority(
            &owner_endpoint,
            nas_endpoint.addr(),
            &owner_identity,
            workspace_id,
            grant,
        )
        .await;

        // The collaborator reaches the host on the session plane first, as
        // every peer does: that is where the host attests its certificate.
        submit_workspace_authority(
            &collab_endpoint,
            nas_endpoint.addr(),
            &collab_identity,
            workspace_id,
            None,
        )
        .await
        .expect("the collaborator's session reaches the host");
        let collab_hydrator = authorized_collaborator_reads_content(
            &collab_endpoint,
            ticket.node.clone(),
            first_hash,
            first,
        )
        .await;
        let revoke = signed_read_authority(workspace_id, 2, owner, collaborator, true, &owner_root);
        publish_workspace_authority(
            &owner_endpoint,
            nas_endpoint.addr(),
            &owner_identity,
            workspace_id,
            revoke,
        )
        .await;

        let after = b"this remains available only to the owner";
        let after_hash = publish_post_revoke_content(&nas_store, &nas_namespace, after).await;
        assert_revoked_collaborator_is_denied(&collab_hydrator, after_hash, after).await;
        assert_owner_retains_content_access(
            &owner_endpoint,
            nas_endpoint.addr(),
            after_hash,
            after,
        )
        .await;
    }
    // Phase `certified_human_grant_bootstraps_from_an_owned_device`.
    // A workspace grant names the human root, while the connection arrives
    // from one certified device. This is the identity-layer behavior the
    // endpoint-only prototype could not express.
    {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, auth_b) = bind_certified_endpoint().await;
        let store = Arc::new(ContentStore::memory());
        let manifest = store
            .write_content(b"human-shared content")
            .await
            .unwrap()
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(ep_a.clone(), store.clone()));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        {
            let mut namespace = namespace.lock().unwrap();
            namespace.create_dir(ROOT_INO, "human-shared", 1).unwrap();
            namespace
                .create_file(ROOT_INO, "human.txt", Some(&manifest), 20, 1)
                .unwrap();
        }
        let policy = policy_granting(&[auth_b.user_public_key()]);
        let server = ep_a.clone();
        let (_workspace, _ticket) = Workspace::host(
            ep_a,
            look_a,
            store,
            hydrator,
            namespace,
            auth_a,
            Some(policy),
        )
        .unwrap();

        // The device's session attests its certificate: that is what lets
        // the host resolve the human grant to this connection. The host
        // answers a stream only after recording it, so the round trip makes
        // the grant resolvable before the read.
        let sessions = SessionPool::new(
            ep_b.clone(),
            auth_b.clone(),
            Ticket::workspace_id_for_topic(*server.id().as_bytes()),
        );
        let session = sessions
            .get(&server.addr())
            .await
            .expect("the certified device's session reaches the host");
        session::announce(&sessions, &session)
            .await
            .expect("the host answers the certified device");
        authorized_collaborator_reads_content(
            &ep_b,
            server.addr(),
            manifest,
            b"human-shared content",
        )
        .await;
    }
    // Phase `record_mesh_accepts_same_human_relays_but_rejects_other_people`.
    {
        let server_temp = tempfile::tempdir().unwrap();
        let client_temp = tempfile::tempdir().unwrap();
        let (server_ep, server_lookup, server_identity, server_root, server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, _client_lookup, client_identity, _client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = Ticket::workspace_id_for_topic(*server_ep.id().as_bytes());
        let server_services = record_services(server_temp.path());
        let direct_record = FileRecordId::new();
        let server_actor = FileActor {
            person: Ed25519PublicKey(server_root.verifying_key().to_bytes()),
            device: Ed25519PublicKey(server_identity.device_public_key()),
        };
        publish_record_metadata(
            Arc::clone(&server_services.repository),
            workspace_id,
            direct_record,
            server_actor,
            server_device,
            41,
        );

        // A home peer certified to the same human must relay records signed
        // by that person's other devices. The original device signature stays
        // intact; only a different human remains outside the trust boundary.
        let sibling_record = FileRecordId::new();
        let sibling_device = ed25519_dalek::SigningKey::from_bytes(&[0x60; 32]);
        let sibling_actor = FileActor {
            person: server_actor.person,
            device: Ed25519PublicKey(sibling_device.verifying_key().to_bytes()),
        };
        publish_record_metadata(
            Arc::clone(&server_services.repository),
            workspace_id,
            sibling_record,
            sibling_actor,
            sibling_device,
            73,
        );

        // A peer may also store a different person's authenticated stream,
        // but transport certification does not authorize relaying across
        // people.
        let relayed_record = FileRecordId::new();
        let relayed_person = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
        let relayed_device = ed25519_dalek::SigningKey::from_bytes(&[0x62; 32]);
        let relayed_actor = FileActor {
            person: Ed25519PublicKey(relayed_person.verifying_key().to_bytes()),
            device: Ed25519PublicKey(relayed_device.verifying_key().to_bytes()),
        };
        publish_record_metadata(
            Arc::clone(&server_services.repository),
            workspace_id,
            relayed_record,
            relayed_actor,
            relayed_device,
            99,
        );

        let mut subtree_acl = WorkspaceAcl::new();
        subtree_acl.grant_read(identity(&client_ep), "/shared");
        let server_acl = AclHandle::new(subtree_acl);
        let server_policy = Some(server_acl.clone());
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(server_ep.clone(), Arc::clone(&store)));
        let (_server_workspace, ticket) = Workspace::host_with_records(
            server_ep.clone(),
            server_lookup,
            store,
            hydrator,
            Arc::new(Mutex::new(Namespace::new("server").unwrap())),
            server_identity.clone(),
            server_policy,
            server_services.clone(),
        )
        .unwrap();
        assert_eq!(ticket.workspace_id(), workspace_id);

        let client_policy = Some(policy_read_write(&[identity(&server_ep)], &[]));
        let client_services = record_services(client_temp.path());
        let client_store = Arc::new(ContentStore::memory());
        let client_records = build_record_plane(
            &client_ep,
            &client_identity,
            client_store,
            workspace_id,
            Some(client_services.clone()),
        )
        .unwrap()
        .unwrap();
        assert!(
            pull_over_session(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &client_policy,
                &client_records,
            )
            .await
            .is_err(),
            "a reader of a subtree is denied the whole record plane"
        );

        let mut root_acl = WorkspaceAcl::new();
        root_acl.grant_read(identity(&client_ep), "/");
        server_acl.replace(root_acl);
        pull_over_session(
            &client_ep,
            server_ep.addr(),
            &client_identity,
            &client_policy,
            &client_records,
        )
        .await
        .unwrap();

        let direct_key = RecordRepositoryKey {
            workspace_id,
            record_id: direct_record,
        };
        assert_eq!(
            client_services
                .repository
                .highest_sequence(direct_key, &server_actor),
            Some(1)
        );
        assert_eq!(
            client_services
                .repository
                .reconciliation_batch(direct_key)
                .unwrap()
                .snapshot
                .metadata
                .unwrap()
                .byte_len,
            41
        );
        assert_eq!(
            client_services.repository.highest_sequence(
                RecordRepositoryKey {
                    workspace_id,
                    record_id: sibling_record,
                },
                &sibling_actor,
            ),
            Some(1),
            "a home peer must relay a sibling device's signed stream"
        );
        assert!(
            !client_services.repository.contains(RecordRepositoryKey {
                workspace_id,
                record_id: relayed_record,
            }),
            "the authenticated server must not relay a third actor's stream"
        );
    }
    // Phase `record_session_guards_manifest_chunks_thumbnails_and_get_many_live`.
    {
        use collab_types::Ed25519PublicKey;
        use fs_protocol::{FileInviteSpec, FileInviteSubject, FilePermission, mint_file_invite};
        use iroh_blobs::protocol::{ChunkRangesSeq, GetManyRequest};
        use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};

        let temp = tempfile::tempdir().unwrap();
        let (server_ep, _server_lookup, server_identity, server_root, _server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, _client_lookup, client_identity, client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = uuid::Uuid::new_v4();
        let authority =
            Arc::new(ShareAuthority::open(temp.path().join("share-authority.json")).unwrap());
        let repository =
            Arc::new(RecordRepository::open(temp.path().join("record-repository")).unwrap());
        let access_registry = Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap());
        let services = RecordServices {
            authority: authority.clone(),
            repository,
            access_registry: access_registry.clone(),
            mutation_sink: None,
            materializer: None,
            availability: None,
            workspace_authority: None,
        };
        let store = Arc::new(ContentStore::memory());
        let records = build_record_plane(
            &server_ep,
            &server_identity,
            Arc::clone(&store),
            workspace_id,
            Some(services),
        )
        .unwrap()
        .unwrap();

        let allowed_manifest = store
            .write_content(b"record-scoped content")
            .await
            .unwrap()
            .unwrap();
        let allowed_chunk = iroh_blobs::Hash::from(
            crate::content::Manifest::decode(&store.local(allowed_manifest).await.unwrap())
                .unwrap()
                .chunks()[0]
                .hash,
        );
        let thumbnail = store
            .ingest(bytes::Bytes::from_static(b"record thumbnail"))
            .await
            .unwrap();
        let sibling_manifest = store
            .write_content(b"sibling secret")
            .await
            .unwrap()
            .unwrap();
        let mut namespace = Namespace::new("host").unwrap();
        let allowed_ino = namespace
            .create_file(ROOT_INO, "allowed.png", Some(&allowed_manifest), 21, 1)
            .unwrap();
        assert!(namespace.set_thumb(allowed_ino, &thumbnail));
        let allowed_record = namespace.record_id_for(allowed_ino).unwrap();
        namespace
            .create_file(ROOT_INO, "sibling.txt", Some(&sibling_manifest), 14, 1)
            .unwrap();
        let namespace = Arc::new(Mutex::new(namespace));

        let client_credentials = client_identity.credentials();
        let client_cert = client_credentials.cert().clone();
        let peer = AuthenticatedFsPeer {
            user_public_key: client_cert.user_public_key,
            device_public_key: client_cert.device_public_key,
            device_label: client_cert.device_label.clone(),
            capabilities: client_cert.capabilities.clone(),
            cert: client_cert,
        };
        let issuer = Ed25519PublicKey::from_bytes(server_root.verifying_key().to_bytes());
        let mint_session = || {
            let token = mint_file_invite(
                FileInviteSpec {
                    workspace_id,
                    record_id: allowed_record,
                    subject: FileInviteSubject::Person(Ed25519PublicKey::from_bytes(
                        peer.user_public_key,
                    )),
                    permission: FilePermission::View,
                    expires_at_ms: None,
                    max_redemptions: None,
                },
                &server_root,
            )
            .unwrap();
            authority
                .register(&token, &[issuer], unix_now_ms())
                .unwrap();
            let session = Arc::new(
                crate::share_session::ShareSession::open_named(
                    workspace_id,
                    peer.clone(),
                    token.clone(),
                    authority.clone(),
                    Arc::new(server_identity.clone()),
                )
                .unwrap(),
            );
            access_registry.register(session);
            token
        };
        let token = mint_session();

        // An enforced but empty workspace ACL proves all authority below comes
        // from the live exact-record session, never ambient workspace access.
        let policy: PeerPolicy = Some(AclHandle::new(WorkspaceAcl::new()));
        let peer_key = client_identity.device_public_key();
        assert!(
            admit_blob_caller_with_records(&policy, Some(client_ep.id()), Some(&records)).is_ok()
        );
        for hash in [allowed_manifest, allowed_chunk, thumbnail] {
            assert!(
                peer_may_read_blob_with_records(
                    &policy,
                    &namespace,
                    &store,
                    &peer_key,
                    None,
                    hash,
                    Some(&records),
                )
                .await,
                "record session must reach its manifest, chunks, and thumbnail"
            );
        }
        assert!(
            !peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                sibling_manifest,
                Some(&records),
            )
            .await,
            "a sibling hash is never authority"
        );

        // Exercise the real provider interceptor. Each single authorized root
        // transfers, a chunk only beside its manifest, the sibling is denied,
        // and a mixed get-many is rejected before either member is imported
        // into the client store.
        let _router = Router::builder(server_ep.clone())
            .accept(
                iroh_blobs::ALPN,
                store.blobs_protocol(blob_guard(
                    policy.clone(),
                    namespace.clone(),
                    store.clone(),
                    Some(records.clone()),
                    None,
                )),
            )
            .spawn();
        for hash in [allowed_manifest, thumbnail] {
            let connection = client_ep
                .connect(server_ep.addr(), iroh_blobs::ALPN)
                .await
                .unwrap();
            assert!(
                iroh_blobs::get::request::get_blob(connection, hash)
                    .await
                    .is_ok(),
                "authorized record blob must transfer through the real guard"
            );
        }
        let connection = client_ep
            .connect(server_ep.addr(), iroh_blobs::ALPN)
            .await
            .unwrap();
        assert!(
            iroh_blobs::get::request::get_blob(connection, allowed_chunk)
                .await
                .is_err(),
            "a chunk asked for without its manifest is refused"
        );
        let chunk_client_store = ContentStore::memory();
        let connection = client_ep
            .connect(server_ep.addr(), iroh_blobs::ALPN)
            .await
            .unwrap();
        assert!(
            chunk_client_store
                .store()
                .remote()
                .execute_get_many(
                    connection,
                    GetManyRequest::new(
                        vec![allowed_manifest, allowed_chunk],
                        ChunkRangesSeq::all(),
                    ),
                )
                .await
                .is_ok(),
            "a chunk beside its readable manifest transfers through the real guard"
        );
        assert!(chunk_client_store.has(allowed_chunk).await);
        let connection = client_ep
            .connect(server_ep.addr(), iroh_blobs::ALPN)
            .await
            .unwrap();
        assert!(
            iroh_blobs::get::request::get_blob(connection, sibling_manifest)
                .await
                .is_err(),
            "sibling fetch must fail at the provider"
        );
        let mixed_client_store = ContentStore::memory();
        let connection = client_ep
            .connect(server_ep.addr(), iroh_blobs::ALPN)
            .await
            .unwrap();
        assert!(
            mixed_client_store
                .store()
                .remote()
                .execute_get_many(
                    connection,
                    GetManyRequest::new(
                        vec![allowed_manifest, sibling_manifest],
                        ChunkRangesSeq::all(),
                    ),
                )
                .await
                .is_err(),
            "one unauthorized member must reject the complete get-many request"
        );
        assert!(!mixed_client_store.has(allowed_manifest).await);
        assert!(!mixed_client_store.has(sibling_manifest).await);

        authority.revoke(token.id).unwrap();
        assert!(
            !peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                allowed_manifest,
                Some(&records),
            )
            .await,
            "grant revocation must affect the next request"
        );

        // A fresh grant restores only record authority. Revoking the certified
        // device then removes it again on the immediately following lookup.
        let _fresh_token = mint_session();
        assert!(
            peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                allowed_manifest,
                Some(&records),
            )
            .await
        );
        let mut revocation = RevocationEntry {
            user_public_key: peer.user_public_key,
            revoked_device_public_key: peer.device_public_key,
            revoked_at_ns: unix_now_ns().saturating_sub(1),
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &client_root).unwrap();
        server_identity
            .revocations()
            .write()
            .unwrap()
            .insert(revocation)
            .unwrap();
        assert!(
            !peer_may_read_blob_with_records(
                &policy,
                &namespace,
                &store,
                &peer_key,
                None,
                allowed_manifest,
                Some(&records),
            )
            .await,
            "device revocation must affect the next request"
        );
    }
}

/// The identity an endpoint authenticates as — the same 32 bytes an ACL
/// grant names.
fn identity(endpoint: &Endpoint) -> IdentityKey {
    *endpoint.id().as_bytes()
}

fn record_services(root: &std::path::Path) -> RecordServices {
    RecordServices {
        authority: Arc::new(ShareAuthority::open(root.join("share-authority.json")).unwrap()),
        repository: Arc::new(RecordRepository::open(root.join("records")).unwrap()),
        access_registry: Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap()),
        mutation_sink: None,
        materializer: None,
        availability: None,
        workspace_authority: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn structure_content_and_transactions_propagate_between_peers() {
    // Phase `fresh_record_join_finishes_initial_projection_before_returning`.
    {
        let server_temp = tempfile::tempdir().unwrap();
        let client_temp = tempfile::tempdir().unwrap();
        let (server_ep, server_lookup, server_identity, server_root, server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, client_lookup, client_identity, _client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        let workspace_id = Ticket::workspace_id_for_topic(*server_ep.id().as_bytes());
        let record_id = FileRecordId::new();
        let server_actor = FileActor {
            person: Ed25519PublicKey(server_root.verifying_key().to_bytes()),
            device: Ed25519PublicKey(server_identity.device_public_key()),
        };
        let server_services = record_services(server_temp.path());
        publish_record_metadata(
            Arc::clone(&server_services.repository),
            workspace_id,
            record_id,
            server_actor,
            server_device,
            37,
        );
        let server_store = Arc::new(ContentStore::memory());
        let server_hydrator = Arc::new(Hydrator::new(server_ep.clone(), Arc::clone(&server_store)));
        let server_policy = Some(policy_granting(&[client_identity.user_public_key()]));
        let (_server_workspace, ticket) = Workspace::host_with_records(
            server_ep.clone(),
            server_lookup,
            server_store,
            server_hydrator,
            Arc::new(Mutex::new(Namespace::new("initial-record-server").unwrap())),
            server_identity.clone(),
            server_policy,
            server_services,
        )
        .unwrap();

        let client_services = record_services(client_temp.path());
        let client_repository = Arc::clone(&client_services.repository);
        let client_store = Arc::new(ContentStore::memory());
        let client_hydrator = Arc::new(Hydrator::new(client_ep.clone(), Arc::clone(&client_store)));
        let client_policy = Some(policy_read_write(&[server_identity.user_public_key()], &[]));
        let _client_workspace = Workspace::join_with_records(
            SyncNode {
                endpoint: client_ep,
                lookup: client_lookup,
                store: client_store,
                hydrator: client_hydrator,
                namespace: Arc::new(Mutex::new(Namespace::new("initial-record-client").unwrap())),
                identity: client_identity,
                policy: client_policy,
            },
            ticket,
            client_services,
        )
        .await
        .unwrap();

        assert_eq!(
            client_repository.highest_sequence(
                RecordRepositoryKey {
                    workspace_id,
                    record_id,
                },
                &server_actor,
            ),
            Some(1),
            "join must not return before the bootstrap peer's signed projection is local"
        );
    }
    // Phase `certified_peer_acknowledges_only_a_durable_exact_version_and_replay_is_rejected`.
    {
        let temp = tempfile::tempdir().unwrap();
        let (server_ep, _server_lookup, server_identity, _server_root, server_device) =
            bind_certified_test_endpoint_with_root().await;
        let (client_ep, client_lookup, client_identity, _client_root, _client_device) =
            bind_certified_test_endpoint_with_root().await;
        client_lookup.add_endpoint_info(server_ep.addr());

        let workspace_id = uuid::Uuid::new_v4();
        let record_id = FileRecordId::new();
        let version_id = FileVersionId::new();
        let store = Arc::new(
            ContentStore::open(temp.path().join("durable-blobs"))
                .await
                .unwrap(),
        );
        let bytes = b"durably replicated bytes";
        let manifest = store.write_content(bytes).await.unwrap().unwrap();
        let content_hash = Some(ContentHash(*manifest.as_bytes()));
        let metadata_hash = ContentHash([9; 32]);
        let actor = FileActor {
            person: Ed25519PublicKey(server_identity.user_public_key()),
            device: Ed25519PublicKey(server_identity.device_public_key()),
        };
        let repository =
            Arc::new(RecordRepository::open(temp.path().join("durable-records")).unwrap());
        RecordPublisher::new(Arc::clone(&repository), workspace_id, actor, server_device)
            .unwrap()
            .publish(
                &FileMetadataV1 {
                    schema_version: 1,
                    record_id,
                    content_hash,
                    byte_len: bytes.len() as u64,
                    mime: Some("application/octet-stream".into()),
                    previews: Vec::new(),
                    latest_actor: Some(actor),
                    observed_at_ms: unix_now_ms(),
                    index_status: IndexStatus::Pending,
                    properties: None,
                },
                &[FileVersionV1 {
                    id: version_id,
                    record_id,
                    parents: Vec::new(),
                    actor,
                    timestamp_ms: unix_now_ms(),
                    content_hash,
                    metadata_hash,
                    provenance: FileProvenance::Create,
                }],
            )
            .unwrap();
        let revision = repository
            .reconciliation_batch(RecordRepositoryKey {
                workspace_id,
                record_id,
            })
            .unwrap()
            .snapshot
            .content_revision
            .unwrap();

        let protocol = replica_session_host(
            &server_ep,
            &server_identity,
            None,
            workspace_id,
            &temp.path().join("server-share-authority.json"),
            repository,
            store,
        );
        let _router = Router::builder(server_ep.clone())
            .accept(TILO_FS_SESSION_ALPN, protocol)
            .spawn();

        let request = ReplicaAckRequestV1::new(workspace_id, revision).unwrap();
        let observation =
            replica_ack_over_session(&client_ep, server_ep.addr(), &client_identity, &request)
                .await
                .expect("the host durably holds the exact version");

        let path = temp.path().join("availability.json");
        let registry = Arc::new(Mutex::new(
            crate::availability::AvailabilityRegistry::open(&path).unwrap(),
        ));
        record_replica_observation(
            &mut registry.lock().unwrap(),
            revision,
            Ed25519PublicKey(client_identity.device_public_key()),
            observation.clone(),
            observation.observed_at_ms,
        )
        .unwrap();
        {
            let mut registry = registry.lock().unwrap();
            assert_eq!(
                registry.get(record_id).unwrap().current_version,
                Some(version_id),
                "the shipped whole-workspace ACK path creates the ordinary record row first"
            );
            assert!(matches!(
                registry.observe_replica(
                    record_id,
                    Ed25519PublicKey(client_identity.device_public_key()),
                    observation.clone(),
                    observation.observed_at_ms,
                ),
                Err(crate::availability::AvailabilityError::ReplicaReplay)
            ));
            drop(registry);
        }
        let reopened = crate::availability::AvailabilityRegistry::open(path).unwrap();
        assert_eq!(
            reopened.get(record_id).unwrap().replica_observations.len(),
            1
        );

        let mut wrong_revision = revision;
        wrong_revision.version_id = FileVersionId::new();
        let wrong = ReplicaAckRequestV1::new(workspace_id, wrong_revision).unwrap();
        assert!(
            replica_ack_over_session(&client_ep, server_ep.addr(), &client_identity, &wrong)
                .await
                .is_none()
        );
    }
    // Phase `live_record_mesh_replacement_transaction_survives_staging_restart_in_both_orders`.
    // Exercise replacement transactions through two real authenticated Iroh
    // endpoints. The receiver never reads or mutates the sender repository
    // directly: each asserted boundary follows a completed reconcile/pull
    // exchange over a session.
    {
        for source_first in [true, false] {
            let server_temp = tempfile::tempdir().unwrap();
            let client_temp = tempfile::tempdir().unwrap();
            let (server_ep, server_lookup, server_identity, server_root, server_device) =
                bind_certified_test_endpoint_with_root().await;
            let (client_ep, _client_lookup, client_identity, _client_root, _client_device) =
                bind_certified_test_endpoint_with_root().await;
            assert_ne!(
                server_identity.user_public_key(),
                client_identity.user_public_key(),
                "the live peers must authenticate under separate identity roots"
            );
            let workspace_id = Ticket::workspace_id_for_topic(*server_ep.id().as_bytes());
            let actor = FileActor {
                person: Ed25519PublicKey(server_root.verifying_key().to_bytes()),
                device: Ed25519PublicKey(server_identity.device_public_key()),
            };
            let authorizer = AllowActor(actor);

            let mut server_namespace = Namespace::new("live-record-mesh-server").unwrap();
            let source_ino = server_namespace
                .create_file(ROOT_INO, "source.txt", None, 1, 1)
                .unwrap();
            let replaced_ino = server_namespace
                .create_file(ROOT_INO, "destination.txt", None, 1, 1)
                .unwrap();
            let source_record = server_namespace.record_id_for(source_ino).unwrap();
            let replaced_record = server_namespace.record_id_for(replaced_ino).unwrap();
            let root_record = server_namespace.record_id_for(ROOT_INO).unwrap();
            let source_expected = FileVersionId::new();
            let replaced_expected = FileVersionId::new();
            for (ino, record_id, version_id, hash) in [
                (
                    source_ino,
                    source_record,
                    source_expected,
                    ContentHash([61; 32]),
                ),
                (
                    replaced_ino,
                    replaced_record,
                    replaced_expected,
                    ContentHash([62; 32]),
                ),
            ] {
                assert!(server_namespace.set_content_revision(
                    ino,
                    &ContentRevisionV1 {
                        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                        record_id,
                        version_id,
                        content_hash: Some(hash),
                        byte_len: 1,
                        metadata_hash: ContentHash([63; 32]),
                    },
                    1,
                ));
            }
            let namespace_seed = server_namespace.save();
            let server_namespace = Arc::new(Mutex::new(server_namespace));

            let server_services = record_services(server_temp.path());
            let publisher = RecordPublisher::new(
                Arc::clone(&server_services.repository),
                workspace_id,
                actor,
                server_device.clone(),
            )
            .unwrap();
            for (record_id, version_id, name, hash) in [
                (
                    source_record,
                    source_expected,
                    "source.txt",
                    ContentHash([61; 32]),
                ),
                (
                    replaced_record,
                    replaced_expected,
                    "destination.txt",
                    ContentHash([62; 32]),
                ),
            ] {
                publisher
                    .publish_binding(&FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id,
                        parent: Some(root_record),
                        name: name.into(),
                        kind: FileNodeKind::File,
                        tombstoned: false,
                        actor,
                        observed_at_ms: 1,
                    })
                    .unwrap();
                publisher
                    .publish_structural_version(&FileVersionV1 {
                        id: version_id,
                        record_id,
                        parents: vec![],
                        actor,
                        timestamp_ms: 1,
                        content_hash: Some(hash),
                        metadata_hash: ContentHash([63; 32]),
                        provenance: FileProvenance::Create,
                    })
                    .unwrap();
            }

            let participant = |record_id, expected_version, role, hash| {
                let version = FileVersionV1 {
                    id: FileVersionId::new(),
                    record_id,
                    parents: vec![expected_version],
                    actor,
                    timestamp_ms: 2,
                    content_hash: Some(hash),
                    metadata_hash: ContentHash([63; 32]),
                    provenance: match role {
                        FileMutationParticipantRole::Source => FileProvenance::Rename,
                        FileMutationParticipantRole::Replaced => FileProvenance::Delete,
                        _ => unreachable!("replacement fixture role"),
                    },
                };
                let mut participant = FileMutationParticipant {
                    record_id,
                    role,
                    expected_version,
                    proposed_binding: FileBindingV1 {
                        schema_version: FILE_BINDING_VERSION,
                        record_id,
                        parent: Some(root_record),
                        name: "destination.txt".into(),
                        kind: FileNodeKind::File,
                        tombstoned: role == FileMutationParticipantRole::Replaced,
                        actor,
                        observed_at_ms: 2,
                    },
                    proposed_content_revision: Some(ContentRevisionV1 {
                        schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                        record_id,
                        version_id: version.id,
                        content_hash: version.content_hash,
                        byte_len: 1,
                        metadata_hash: version.metadata_hash,
                    }),
                    proposed_version: version,
                    projection_hash: ContentHash([0; 32]),
                };
                participant.projection_hash = participant.computed_projection_hash().unwrap();
                participant
            };
            let mut participants = vec![
                participant(
                    source_record,
                    source_expected,
                    FileMutationParticipantRole::Source,
                    ContentHash([61; 32]),
                ),
                participant(
                    replaced_record,
                    replaced_expected,
                    FileMutationParticipantRole::Replaced,
                    ContentHash([62; 32]),
                ),
            ];
            participants.sort_by_key(|participant| participant.record_id);
            let transaction = FileMutationTransaction::signed(
                FileMutationTransactionManifest {
                    schema_version: FILE_MUTATION_TRANSACTION_VERSION,
                    transaction_id: uuid::Uuid::new_v4(),
                    workspace_id,
                    actor,
                    timestamp_ms: 2,
                    operation: FileMutationOperation::ReplacementRename {
                        source_parent: root_record,
                        source_name: "source.txt".into(),
                        destination_parent: root_record,
                        destination_name: "destination.txt".into(),
                        source_record,
                        replaced_record,
                    },
                    participants,
                },
                &server_device,
            )
            .unwrap();

            let mut server_acl = WorkspaceAcl::new();
            server_acl.grant_read(identity(&client_ep), "/");
            let server_policy = Some(AclHandle::new(server_acl));
            let server_store = Arc::new(ContentStore::memory());
            let client_store = Arc::new(ContentStore::memory());
            assert!(!Arc::ptr_eq(&server_store, &client_store));
            let server_hydrator =
                Arc::new(Hydrator::new(server_ep.clone(), Arc::clone(&server_store)));
            let (_server_workspace, ticket) = Workspace::host_with_records(
                server_ep.clone(),
                server_lookup,
                Arc::clone(&server_store),
                server_hydrator,
                server_namespace,
                server_identity.clone(),
                server_policy,
                server_services.clone(),
            )
            .unwrap();
            assert_eq!(ticket.workspace_id(), workspace_id);

            let client_policy = Some(policy_read_write(&[identity(&server_ep)], &[]));
            let client_repository_path = client_temp.path().join("records");
            let client_history_path = client_temp.path().join("history.json");
            let client_transaction_journal_path = client_temp.path().join("record-transactions");
            let client_namespace_path = client_temp.path().join("namespace.bin");
            let client_repository =
                Arc::new(RecordRepository::open(&client_repository_path).unwrap());
            let client_history = Arc::new(FileHistoryStore::open(&client_history_path).unwrap());
            let client_namespace = Arc::new(Mutex::new(
                Namespace::from_bytes("live-record-mesh-client", &namespace_seed).unwrap(),
            ));
            let client_materializer = Arc::new(
                crate::record_materializer::RecordMaterializer::new(
                    workspace_id,
                    Arc::clone(&client_namespace),
                    Some(Arc::clone(&client_history)),
                )
                .with_transaction_recovery(
                    Arc::new(Mutex::new(
                        crate::record_transaction::RecordTransactionJournal::open(
                            &client_transaction_journal_path,
                        )
                        .unwrap(),
                    )),
                    client_namespace_path.clone(),
                ),
            );
            let client_services = RecordServices {
                authority: Arc::new(
                    ShareAuthority::open(client_temp.path().join("share-authority.json")).unwrap(),
                ),
                repository: Arc::clone(&client_repository),
                access_registry: Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap()),
                mutation_sink: None,
                materializer: Some(Arc::clone(&client_materializer)),
                availability: None,
                workspace_authority: None,
            };
            let client_records = build_record_plane(
                &client_ep,
                &client_identity,
                Arc::clone(&client_store),
                workspace_id,
                Some(client_services.clone()),
            )
            .unwrap()
            .unwrap();

            // Awaited pull completion is the baseline barrier; no timing loop
            // or arbitrary sleep decides whether both initial streams arrived.
            pull_over_session(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &client_policy,
                &client_records,
            )
            .await
            .unwrap();
            for record_id in [source_record, replaced_record] {
                assert_eq!(
                    client_repository.highest_sequence(
                        RecordRepositoryKey {
                            workspace_id,
                            record_id,
                        },
                        &actor,
                    ),
                    Some(2)
                );
                assert_eq!(
                    client_history.snapshot(record_id).unwrap().versions.len(),
                    1
                );
            }

            let delivery_order = if source_first {
                [source_record, replaced_record]
            } else {
                [replaced_record, source_record]
            };
            let publish_participant = |record_id| {
                let envelopes = [
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        3,
                        RecordOperationV1::PrepareTransaction(transaction.clone()),
                        &server_device,
                    )
                    .unwrap(),
                    RecordEnvelopeV1::signed(
                        record_id,
                        actor,
                        4,
                        RecordOperationV1::CommitTransaction {
                            transaction_id: transaction.manifest.transaction_id,
                        },
                        &server_device,
                    )
                    .unwrap(),
                ];
                server_services
                    .repository
                    .apply_batch(
                        RecordRepositoryKey {
                            workspace_id,
                            record_id,
                        },
                        &envelopes,
                        &authorizer,
                    )
                    .unwrap();
            };

            publish_participant(delivery_order[0]);
            pull_over_session(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &client_policy,
                &client_records,
            )
            .await
            .unwrap();
            assert_eq!(
                client_repository.highest_sequence(
                    RecordRepositoryKey {
                        workspace_id,
                        record_id: delivery_order[0],
                    },
                    &actor,
                ),
                Some(4)
            );
            assert!(
                !client_repository
                    .transaction_status(workspace_id, transaction.manifest.transaction_id)
                    .unwrap()
                    .unwrap()
                    .complete
            );
            {
                let mut namespace = client_namespace.lock().unwrap();
                assert_eq!(
                    namespace
                        .lookup(ROOT_INO, "source.txt")
                        .and_then(|attr| namespace.record_id_for(attr.ino.0)),
                    Some(source_record)
                );
                assert_eq!(
                    namespace
                        .lookup(ROOT_INO, "destination.txt")
                        .and_then(|attr| namespace.record_id_for(attr.ino.0)),
                    Some(replaced_record)
                );
                drop(namespace);
            }

            // Restart every durable receiver-side projection while the first
            // participant is staged. The endpoint/identity remains distinct
            // and the next state change still arrives only over Iroh.
            let restarted_namespace_bytes = client_namespace.lock().unwrap().save();
            drop(client_records);
            drop(client_services);
            drop(client_materializer);
            drop(client_repository);
            drop(client_history);
            let restarted_repository =
                Arc::new(RecordRepository::open(&client_repository_path).unwrap());
            let restarted_history = Arc::new(FileHistoryStore::open(&client_history_path).unwrap());
            let restarted_namespace = Arc::new(Mutex::new(
                Namespace::from_bytes(
                    "live-record-mesh-client-restarted",
                    &restarted_namespace_bytes,
                )
                .unwrap(),
            ));
            let restarted_journal = Arc::new(Mutex::new(
                crate::record_transaction::RecordTransactionJournal::open(
                    &client_transaction_journal_path,
                )
                .unwrap(),
            ));
            let restarted_materializer = Arc::new(
                crate::record_materializer::RecordMaterializer::new(
                    workspace_id,
                    Arc::clone(&restarted_namespace),
                    Some(Arc::clone(&restarted_history)),
                )
                .with_transaction_recovery(
                    Arc::clone(&restarted_journal),
                    client_namespace_path.clone(),
                ),
            );
            let restarted_services = RecordServices {
                authority: Arc::new(
                    ShareAuthority::open(client_temp.path().join("share-authority.json")).unwrap(),
                ),
                repository: Arc::clone(&restarted_repository),
                access_registry: Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap()),
                mutation_sink: None,
                materializer: Some(Arc::clone(&restarted_materializer)),
                availability: None,
                workspace_authority: None,
            };
            let restarted_records = build_record_plane(
                &client_ep,
                &client_identity,
                Arc::clone(&client_store),
                workspace_id,
                Some(restarted_services),
            )
            .unwrap()
            .unwrap();

            publish_participant(delivery_order[1]);
            pull_over_session(
                &client_ep,
                server_ep.addr(),
                &client_identity,
                &client_policy,
                &restarted_records,
            )
            .await
            .unwrap();
            assert!(
                restarted_repository
                    .transaction_status(workspace_id, transaction.manifest.transaction_id)
                    .unwrap()
                    .unwrap()
                    .complete
            );
            let mut namespace = restarted_namespace.lock().unwrap();
            assert!(namespace.lookup(ROOT_INO, "source.txt").is_none());
            assert_eq!(
                namespace
                    .lookup(ROOT_INO, "destination.txt")
                    .and_then(|attr| namespace.record_id_for(attr.ino.0)),
                Some(source_record)
            );
            assert!(namespace.ino_for_record_id(replaced_record).is_none());
            drop(namespace);
            assert_eq!(
                restarted_history
                    .snapshot(source_record)
                    .unwrap()
                    .versions
                    .len(),
                2
            );
            assert_eq!(
                restarted_history
                    .snapshot(replaced_record)
                    .unwrap()
                    .versions
                    .len(),
                2
            );
            assert!(client_namespace_path.is_file());
            assert!(restarted_journal.lock().unwrap().entries().next().is_none());
        }
    }
}

fn publish_record_metadata(
    repository: Arc<RecordRepository>,
    workspace_id: uuid::Uuid,
    record_id: FileRecordId,
    actor: FileActor,
    device: ed25519_dalek::SigningKey,
    byte_len: u64,
) {
    RecordPublisher::new(repository, workspace_id, actor, device)
        .unwrap()
        .publish(
            &FileMetadataV1 {
                schema_version: 1,
                record_id,
                content_hash: None,
                byte_len,
                mime: Some("application/octet-stream".into()),
                previews: vec![],
                latest_actor: Some(actor),
                observed_at_ms: byte_len,
                index_status: IndexStatus::Pending,
                properties: None,
            },
            &[],
        )
        .unwrap();
}

async fn publish_durable_test_revision(
    publisher: &RecordPublisher,
    repository: &RecordRepository,
    store: &ContentStore,
    record_id: FileRecordId,
    version_id: FileVersionId,
    bytes: &[u8],
    metadata_byte: u8,
) -> ContentRevisionV1 {
    let manifest = store.write_content(bytes).await.unwrap().unwrap();
    let metadata_hash = ContentHash([metadata_byte; 32]);
    let workspace_id = publisher.workspace_id();
    let actor = publisher.actor();
    publisher
        .publish(
            &FileMetadataV1 {
                schema_version: fs_protocol::files::FILE_METADATA_VERSION,
                record_id,
                content_hash: Some(ContentHash(*manifest.as_bytes())),
                byte_len: bytes.len() as u64,
                mime: Some("application/octet-stream".into()),
                previews: vec![],
                latest_actor: Some(actor),
                observed_at_ms: 1,
                index_status: IndexStatus::Pending,
                properties: None,
            },
            &[FileVersionV1 {
                id: version_id,
                record_id,
                parents: vec![],
                actor,
                timestamp_ms: 1,
                content_hash: Some(ContentHash(*manifest.as_bytes())),
                metadata_hash,
                provenance: FileProvenance::Create,
            }],
        )
        .unwrap();
    repository
        .reconciliation_batch(RecordRepositoryKey {
            workspace_id,
            record_id,
        })
        .unwrap()
        .snapshot
        .content_revision
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(
    clippy::too_many_lines,
    reason = "the scenario proves one continuous three-peer eviction and rehydration lifecycle"
)]
async fn three_live_peers_require_a_fresh_exact_replica_before_eviction_and_rehydrate_after_gc() {
    use crate::availability::{
        AvailabilityError, AvailabilityRegistry, EvictionBlocker, EvictionDecision,
        EvictionRequirements, RecordAvailability, RecordKind,
    };
    use crate::eviction::{
        EvictionCatalog, EvictionQueue, EvictionRequestState, PlanError, plan_safe_eviction,
    };
    use fs_protocol::Residency;

    let temp = tempfile::tempdir().unwrap();
    let (owner_ep, _owner_lookup, owner_identity, _owner_root, _owner_device) =
        bind_certified_test_endpoint_with_root().await;
    let (durable_ep, _durable_lookup, durable_identity, durable_root, durable_device) =
        bind_certified_test_endpoint_with_root().await;
    let (stale_ep, _stale_lookup, stale_identity, stale_root, stale_device) =
        bind_certified_test_endpoint_with_root().await;
    let roots = [
        owner_identity.user_public_key(),
        durable_identity.user_public_key(),
        stale_identity.user_public_key(),
    ];
    assert!(roots[0] != roots[1] && roots[0] != roots[2] && roots[1] != roots[2]);

    let workspace_id = uuid::Uuid::new_v4();
    let mut durable_namespace = Namespace::new("three-peer-durable-replica").unwrap();
    let durable_ino = durable_namespace
        .create_file(ROOT_INO, "replicated.bin", None, 1, 1)
        .unwrap();
    let record_id = durable_namespace.record_id_for(durable_ino).unwrap();
    let current_version = FileVersionId::new();
    let stale_version = FileVersionId::new();
    let current_bytes = b"current durable collaborative payload";
    let stale_bytes = b"obsolete payload";
    let owner_store_path = temp.path().join("owner-blobs");
    let owner_store = Arc::new(
        ContentStore::open_with_gc_interval(&owner_store_path, Duration::from_millis(10))
            .await
            .unwrap(),
    );
    let durable_store = Arc::new(
        ContentStore::open(temp.path().join("durable-blobs"))
            .await
            .unwrap(),
    );
    let stale_store = Arc::new(
        ContentStore::open(temp.path().join("stale-blobs"))
            .await
            .unwrap(),
    );
    assert!(!Arc::ptr_eq(&owner_store, &durable_store));
    assert!(!Arc::ptr_eq(&owner_store, &stale_store));

    let durable_repository =
        Arc::new(RecordRepository::open(temp.path().join("durable-records")).unwrap());
    let stale_repository =
        Arc::new(RecordRepository::open(temp.path().join("stale-records")).unwrap());
    let durable_actor = FileActor {
        person: Ed25519PublicKey(durable_root.verifying_key().to_bytes()),
        device: Ed25519PublicKey(durable_identity.device_public_key()),
    };
    let stale_actor = FileActor {
        person: Ed25519PublicKey(stale_root.verifying_key().to_bytes()),
        device: Ed25519PublicKey(stale_identity.device_public_key()),
    };
    let current_revision = publish_durable_test_revision(
        &RecordPublisher::new(
            Arc::clone(&durable_repository),
            workspace_id,
            durable_actor,
            durable_device,
        )
        .unwrap(),
        &durable_repository,
        &durable_store,
        record_id,
        current_version,
        current_bytes,
        71,
    )
    .await;
    assert!(durable_namespace.set_content_revision(durable_ino, &current_revision, 1,));
    let durable_namespace = Arc::new(Mutex::new(durable_namespace));
    let stale_revision = publish_durable_test_revision(
        &RecordPublisher::new(
            Arc::clone(&stale_repository),
            workspace_id,
            stale_actor,
            stale_device,
        )
        .unwrap(),
        &stale_repository,
        &stale_store,
        record_id,
        stale_version,
        stale_bytes,
        72,
    )
    .await;
    let owner_manifest = owner_store
        .write_content(current_bytes)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        current_revision.content_hash,
        Some(ContentHash(*owner_manifest.as_bytes()))
    );

    let durable_policy = policy_granting(&[identity(&owner_ep)]);
    let stale_policy = policy_granting(&[identity(&owner_ep)]);
    let durable_ack = replica_session_host(
        &durable_ep,
        &durable_identity,
        Some(durable_policy.clone()),
        workspace_id,
        &temp.path().join("durable-share-authority.json"),
        Arc::clone(&durable_repository),
        Arc::clone(&durable_store),
    );
    let stale_ack = replica_session_host(
        &stale_ep,
        &stale_identity,
        Some(stale_policy),
        workspace_id,
        &temp.path().join("stale-share-authority.json"),
        Arc::clone(&stale_repository),
        Arc::clone(&stale_store),
    );
    let _durable_router = Router::builder(durable_ep.clone())
        .accept(
            iroh_blobs::ALPN,
            durable_store.blobs_protocol(blob_guard(
                Some(durable_policy),
                Arc::clone(&durable_namespace),
                Arc::clone(&durable_store),
                None,
                None,
            )),
        )
        .accept(TILO_FS_SESSION_ALPN, durable_ack)
        .spawn();
    let _stale_router = Router::builder(stale_ep.clone())
        .accept(iroh_blobs::ALPN, stale_store.blobs_protocol(None))
        .accept(TILO_FS_SESSION_ALPN, stale_ack)
        .spawn();

    let current_request = ReplicaAckRequestV1::new(workspace_id, current_revision).unwrap();
    assert!(
        replica_ack_over_session(
            &owner_ep,
            stale_ep.addr(),
            &owner_identity,
            &current_request
        )
        .await
        .is_none(),
        "a peer holding only an obsolete version must not acknowledge the current revision"
    );
    let stale_request = ReplicaAckRequestV1::new(workspace_id, stale_revision).unwrap();
    let stale_observation =
        replica_ack_over_session(&owner_ep, stale_ep.addr(), &owner_identity, &stale_request)
            .await
            .expect("the stale peer durably holds its own version");

    let availability_path = temp.path().join("owner-availability.json");
    let mut availability = AvailabilityRegistry::open(&availability_path).unwrap();
    let mut record = RecordAvailability::new(record_id, RecordKind::File);
    record.current_version = Some(current_version);
    record.residency = Residency::Resident {
        bytes: current_bytes.len() as u64,
    };
    availability.put(record).unwrap();
    assert!(matches!(
        availability.observe_replica(
            record_id,
            Ed25519PublicKey(owner_identity.device_public_key()),
            stale_observation.clone(),
            stale_observation.observed_at_ms,
        ),
        Err(AvailabilityError::ReplicaVersionMismatch)
    ));

    let payload = owner_store
        .inspect_version_payload(record_id, current_version, owner_manifest)
        .await
        .unwrap();
    let catalog = EvictionCatalog::new([payload]).unwrap();
    let requirements = EvictionRequirements {
        minimum_reachable: 1,
        minimum_durable: 1,
        maximum_replica_age_ms: 60_000,
    };
    assert!(matches!(
        plan_safe_eviction(
            &availability,
            &catalog,
            record_id,
            stale_observation.observed_at_ms,
            requirements,
        ),
        Err(PlanError::Denied(_))
    ));

    let fresh_observation = replica_ack_over_session(
        &owner_ep,
        durable_ep.addr(),
        &owner_identity,
        &current_request,
    )
    .await
    .expect("the durable peer holds the current version");
    availability
        .observe_replica(
            record_id,
            Ed25519PublicKey(owner_identity.device_public_key()),
            fresh_observation.clone(),
            fresh_observation.observed_at_ms,
        )
        .unwrap();
    assert_eq!(
        availability.eviction_decision(
            record_id,
            fresh_observation.observed_at_ms,
            requirements,
            false,
        ),
        EvictionDecision::Allowed
    );
    assert_eq!(
        availability.eviction_decision(
            record_id,
            fresh_observation
                .observed_at_ms
                .saturating_add(requirements.maximum_replica_age_ms + 1),
            requirements,
            false,
        ),
        EvictionDecision::Denied(EvictionBlocker::ReplicaAcknowledgementStale)
    );

    let plan = plan_safe_eviction(
        &availability,
        &catalog,
        record_id,
        fresh_observation.observed_at_ms,
        requirements,
    )
    .unwrap();
    assert!(!plan.reclaimable.is_empty());
    let mut queue = EvictionQueue::open(temp.path().join("owner-evictions.json")).unwrap();
    let request_id = queue
        .enqueue(&plan, fresh_observation.observed_at_ms)
        .unwrap()
        .id;
    owner_store
        .release_eviction_to_backend_gc(
            &queue,
            request_id,
            &availability,
            &catalog,
            fresh_observation.observed_at_ms,
            requirements,
        )
        .await
        .unwrap();

    // A clean store restart clears the process cache and lets configured
    // backend GC finish the durable release. Completion is gated by exact
    // backend observation rather than a fixed delay.
    owner_store.store().shutdown().await.unwrap();
    drop(owner_store);
    let owner_store = Arc::new(
        ContentStore::open_with_gc_interval(&owner_store_path, Duration::from_millis(10))
            .await
            .unwrap(),
    );
    tilo_test_env::within("the backend GC absence observation", async {
        loop {
            let observation = owner_store
                .observe_eviction_request(
                    &mut queue,
                    request_id,
                    fresh_observation.observed_at_ms.saturating_add(1),
                )
                .await
                .unwrap();
            if matches!(observation.state, EvictionRequestState::Completed { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(owner_store.local(owner_manifest).await.is_none());

    let owner_hydrator = Hydrator::new(owner_ep.clone(), Arc::clone(&owner_store));
    owner_hydrator.add_peer(durable_ep.addr());
    let recovered = owner_hydrator
        .read(owner_manifest, 0, current_bytes.len() as u64)
        .await
        .unwrap();
    assert_eq!(recovered, current_bytes);
    owner_store
        .verify_durable_revision(&current_revision)
        .await
        .unwrap();
}

#[test]
fn blob_callers_must_be_named_and_currently_granted() {
    // Phase `an_unnamed_blob_caller_is_refused`.
    // A caller the transport could not name is refused — an endpoint
    // arriving without an id isn't served on the strength of arriving.
    {
        use iroh_blobs::provider::events::AbortReason;

        let invited = iroh::SecretKey::generate().public();
        let policy: PeerPolicy = Some(policy_granting(&[identity_of(invited)]));

        assert_eq!(
            admit_blob_caller(&policy, None),
            Err(AbortReason::Permission),
            "an unnamed caller cannot be checked and must not be served"
        );
        assert_eq!(
            admit_blob_caller(&policy, Some(invited)),
            Ok(identity_of(invited)),
            "and a named, entitled caller is admitted as itself"
        );
        assert_eq!(
            admit_blob_caller(&policy, Some(iroh::SecretKey::generate().public())),
            Err(AbortReason::Permission),
            "while a named stranger is refused"
        );
    }
    // Phase `revoking_a_grant_refuses_the_next_blob_connection`.
    // Revocation reaches the blob plane through the live handle: the same
    // caller is admitted, then refused, with no remount in between.
    {
        use iroh_blobs::provider::events::AbortReason;

        let invited = iroh::SecretKey::generate().public();
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(identity_of(invited), "/");
        let handle = AclHandle::new(acl);
        let policy: PeerPolicy = Some(handle.clone());

        assert!(admit_blob_caller(&policy, Some(invited)).is_ok());
        handle.replace(WorkspaceAcl::new());
        assert_eq!(
            admit_blob_caller(&policy, Some(invited)),
            Err(AbortReason::Permission),
            "the next connection must be decided by the ACL bound now"
        );
    }
}

#[tokio::test]
async fn a_long_lived_blob_index_follows_every_namespace_change() {
    let peer = identity_of(iroh::SecretKey::generate().public());
    let mut acl = WorkspaceAcl::new();
    acl.grant_read(peer, "/shared");
    let handle = AclHandle::new(acl);
    let policy = Some(handle.clone());
    let store = ContentStore::memory();
    let chunk_of = |manifest: bytes::Bytes| {
        iroh_blobs::Hash::from(
            crate::content::Manifest::decode(&manifest)
                .unwrap()
                .chunks()[0]
                .hash,
        )
    };
    let first = store.write_content(b"first").await.unwrap().unwrap();
    let first_chunk = chunk_of(store.local(first).await.unwrap());
    let second = store.write_content(b"second").await.unwrap().unwrap();
    let second_chunk = chunk_of(store.local(second).await.unwrap());
    let private = store.write_content(b"private").await.unwrap().unwrap();
    let private_chunk = chunk_of(store.local(private).await.unwrap());

    let mut ns = Namespace::new("host").unwrap();
    let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
    let hidden = ns.create_dir(ROOT_INO, "hidden", 1).unwrap();
    let first_ino = ns.create_file(shared, "first", Some(&first), 5, 1).unwrap();
    ns.create_file(hidden, "private", Some(&private), 7, 1)
        .unwrap();
    let ns = Arc::new(Mutex::new(ns));
    let index = BlobAccessIndex::new(Arc::clone(&ns));
    let may = |wanted: Vec<iroh_blobs::Hash>, context: Vec<iroh_blobs::Hash>| {
        let (policy, index, store) = (&policy, &index, &store);
        async move {
            authorize_blob_request(policy, index, store, None, (peer, None), &wanted, &context)
                .await
        }
    };

    assert!(may(vec![first], vec![]).await);
    assert!(
        may(vec![first_chunk], vec![first]).await,
        "a chunk rides on its readable manifest named in the same request"
    );
    assert!(
        !may(vec![first_chunk], vec![]).await,
        "a chunk asked for without its manifest is refused"
    );
    assert!(
        !may(vec![private_chunk], vec![first]).await,
        "a readable manifest in the request does not authorise another file's chunk"
    );
    assert!(!may(vec![private_chunk], vec![private]).await);
    assert!(
        !may(vec![first_chunk, private_chunk], vec![first]).await,
        "one unreadable blob refuses the whole batch"
    );

    // Deleting the only reference revokes it on the very next request, even
    // though the index was built before.
    assert!(ns.lock().unwrap().remove(first_ino));
    assert!(!may(vec![first], vec![]).await);
    assert!(!may(vec![first_chunk], vec![first]).await);

    // New content is readable at once.
    ns.lock()
        .unwrap()
        .create_file(shared, "second", Some(&second), 6, 2)
        .unwrap();
    assert!(may(vec![second, second_chunk], vec![]).await);

    handle.replace(WorkspaceAcl::new());
    assert!(
        !may(vec![second], vec![]).await,
        "the live ACL decides even when the namespace is unchanged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn content_and_writes_reach_only_entitled_peers() {
    // Phase `a_stranger_cannot_hydrate_content`.
    // The negative on the content plane: a stranger who already knows a
    // blob's hash (easy to learn, impossible to keep secret) still can't
    // fetch the bytes.
    {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, _auth_b) = bind_certified_endpoint().await;

        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        let secret = b"the contents of a workspace nobody invited them to";
        let hash = store_a.write_content(secret).await.unwrap().unwrap();

        let someone_else = identity_of(iroh::SecretKey::generate().public());
        let (_ws_a, _ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a.clone(),
            hyd_a,
            ns_a,
            auth_a,
            Some(policy_granting(&[someone_else])),
        )
        .unwrap();

        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Hydrator::new(ep_b.clone(), store_b.clone());
        hyd_b.add_peer(ep_a.addr());

        let got = hyd_b.read(hash, 0, secret.len() as u64).await;
        assert!(
            got.is_err(),
            "a peer with no grant must not be served a blob it knows the hash of, got {:?}",
            got.map(|b| String::from_utf8_lossy(&b).into_owned()),
        );
        assert!(
            !store_b.has(hash).await,
            "and nothing may have landed in its store"
        );
    }
    // Phase `an_entitled_peer_hydrates_content`.
    // Same host, same policy, an invited peer: the bytes reach it, without
    // which the negative above would be satisfied by a mount that serves
    // nobody.
    {
        let (ep_a, look_a, auth_a) = bind_certified_endpoint().await;
        let (ep_b, _look_b, _auth_b) = bind_certified_endpoint().await;
        let policy = policy_granting(&[identity(&ep_a), identity(&ep_b)]);

        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let content = b"a shared collaborative document body";
        let hash = store_a.write_content(content).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("A").unwrap()));
        namespace
            .lock()
            .unwrap()
            .create_file(
                ROOT_INO,
                "shared.txt",
                Some(&hash),
                u64::try_from(content.len()).unwrap(),
                1,
            )
            .unwrap();
        let _ws_a = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a,
            hyd_a,
            namespace,
            auth_a,
            Some(policy),
        )
        .unwrap();

        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Hydrator::new(ep_b.clone(), store_b.clone());
        hyd_b.add_peer(ep_a.addr());
        let got = hyd_b
            .read(hash, 0, content.len() as u64)
            .await
            .expect("an entitled peer hydrates content");
        assert_eq!(got.as_slice(), content, "and reads the host's real bytes");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one session run covers reconciliation, batched pull, inline content and the settled second pass"
)]
async fn a_session_pulls_many_records_with_small_content_inline_and_then_settles() {
    let server_temp = tempfile::tempdir().unwrap();
    let client_temp = tempfile::tempdir().unwrap();
    let (server_ep, server_lookup, server_identity, server_root, server_device) =
        bind_certified_test_endpoint_with_root().await;
    let (client_ep, _client_lookup, client_identity, _client_root, _client_device) =
        bind_certified_test_endpoint_with_root().await;
    let workspace_id = Ticket::workspace_id_for_topic(*server_ep.id().as_bytes());
    let server_actor = FileActor {
        person: Ed25519PublicKey(server_root.verifying_key().to_bytes()),
        device: Ed25519PublicKey(server_identity.device_public_key()),
    };
    let server_services = record_services(server_temp.path());
    let server_store = Arc::new(ContentStore::memory());
    let mut published = Vec::new();
    for n in 0..40_u8 {
        let record_id = FileRecordId::new();
        let bytes = vec![n; 1000 + usize::from(n)];
        let revision = publish_durable_test_revision(
            &RecordPublisher::new(
                Arc::clone(&server_services.repository),
                workspace_id,
                server_actor,
                server_device.clone(),
            )
            .unwrap(),
            &server_services.repository,
            &server_store,
            record_id,
            FileVersionId::new(),
            &bytes,
            n,
        )
        .await;
        published.push((record_id, revision, bytes));
    }
    // One file above the inline limit travels by content fetch, not inline.
    let large_record = FileRecordId::new();
    let large = vec![7_u8; 64 * 1024];
    let large_revision = publish_durable_test_revision(
        &RecordPublisher::new(
            Arc::clone(&server_services.repository),
            workspace_id,
            server_actor,
            server_device,
        )
        .unwrap(),
        &server_services.repository,
        &server_store,
        large_record,
        FileVersionId::new(),
        &large,
        99,
    )
    .await;
    let server_hydrator = Arc::new(Hydrator::new(server_ep.clone(), Arc::clone(&server_store)));
    let (_server_workspace, ticket) = Workspace::host_with_records(
        server_ep.clone(),
        server_lookup,
        server_store,
        server_hydrator,
        Arc::new(Mutex::new(Namespace::new("session-server").unwrap())),
        server_identity.clone(),
        Some(policy_granting(&[client_identity.user_public_key()])),
        server_services,
    )
    .unwrap();

    let client_services = record_services(client_temp.path());
    let client_store = Arc::new(ContentStore::memory());
    // No peers: whatever content arrives came inline with the records.
    let client_hydrator = Hydrator::new(client_ep.clone(), Arc::clone(&client_store));
    let client_policy = Some(policy_read_write(&[server_identity.user_public_key()], &[]));
    let record_plane = build_record_plane(
        &client_ep,
        &client_identity,
        Arc::clone(&client_store),
        workspace_id,
        Some(client_services),
    )
    .unwrap()
    .unwrap();
    let sessions = SessionPool::new(client_ep.clone(), client_identity.clone(), workspace_id);
    let session = sessions
        .get(&ticket.node)
        .await
        .expect("the host speaks the session protocol");

    assert!(
        session::pull_records(
            &sessions,
            &session,
            &client_identity,
            &client_policy,
            &record_plane,
            &client_hydrator,
            crate::record_materializer::ReconcileScope::Touched,
        )
        .await
        .unwrap(),
        "the first pull applies the host's records"
    );
    for (record_id, revision, bytes) in &published {
        let key = RecordRepositoryKey {
            workspace_id,
            record_id: *record_id,
        };
        assert_pulled(&record_plane, key, &server_actor);
        let manifest = iroh_blobs::Hash::from(revision.content_hash.unwrap().0);
        assert_eq!(
            client_hydrator
                .read_local(manifest, 0, u64::MAX)
                .await
                .as_deref(),
            Some(bytes.as_slice()),
            "a small file's bytes arrive with its record"
        );
    }
    assert!(
        record_plane
            .repository
            .highest_sequence(
                RecordRepositoryKey {
                    workspace_id,
                    record_id: large_record,
                },
                &server_actor
            )
            .is_some()
    );
    assert!(
        !client_store
            .has(iroh_blobs::Hash::from(
                large_revision.content_hash.unwrap().0
            ))
            .await,
        "a file above the inline limit is not sent inline"
    );
    assert!(
        !session::pull_records(
            &sessions,
            &session,
            &client_identity,
            &client_policy,
            &record_plane,
            &client_hydrator,
            crate::record_materializer::ReconcileScope::Touched,
        )
        .await
        .unwrap(),
        "once reconciled, a pass pulls nothing"
    );
}

fn assert_pulled(records: &RecordPlane, key: RecordRepositoryKey, actor: &FileActor) {
    assert!(
        records
            .repository
            .highest_sequence(key, actor)
            .is_some_and(|sequence| sequence > 0),
        "record {key:?} was pulled"
    );
}

#[tokio::test]
async fn a_peer_without_the_session_protocol_is_not_reconciled_with() {
    let (server_ep, _server_lookup, _server_identity) = bind_certified_test_endpoint().await;
    let (client_ep, _client_lookup, client_identity) = bind_certified_test_endpoint().await;
    // A peer at another version: it serves content, but no session.
    let store = ContentStore::memory();
    let _router = Router::builder(server_ep.clone())
        .accept(iroh_blobs::ALPN, store.blobs_protocol(None))
        .spawn();
    let sessions = SessionPool::new(
        client_ep,
        client_identity,
        Ticket::workspace_id_for_topic(*server_ep.id().as_bytes()),
    );
    assert!(
        sessions.get(&server_ep.addr()).await.is_err(),
        "a peer that does not offer the session protocol is an error, not a fallback"
    );
}

/// #4113. The idle timer: three seconds after anything happens, doubling
/// per quiet timer pass to thirty; a change or a pass something started
/// brings it back to three; a failing exchange
/// holds three seconds for a minute, then backs off as well.
#[test]
fn idle_backoff_doubles_to_the_ceiling_and_resets_on_change() {
    let quiet = PassOutcome::default();
    let changed = PassOutcome {
        changed: true,
        failed: false,
    };
    let failed = PassOutcome {
        changed: false,
        failed: true,
    };
    let start = Instant::now();
    let mut backoff = IdleBackoff::default();
    let mut schedule = vec![backoff.interval().as_secs()];
    for _ in 0..6 {
        backoff.after_pass(true, quiet, start);
        schedule.push(backoff.interval().as_secs());
    }
    assert_eq!(schedule, [3, 6, 12, 24, 30, 30, 30]);
    assert_eq!(IDLE_RECONCILE_CEILING, Duration::from_secs(30));

    backoff.after_pass(true, changed, start);
    assert_eq!(backoff.interval(), RECONCILE_INTERVAL, "a change resets");
    backoff.after_pass(true, quiet, start);
    backoff.after_pass(false, quiet, start);
    assert_eq!(
        backoff.interval(),
        RECONCILE_INTERVAL,
        "a pass a local change or an announcement started resets"
    );

    // Phase `a_failing_exchange_holds_the_short_interval_for_a_minute`.
    for _ in 0..3 {
        backoff.after_pass(true, quiet, start);
    }
    let mut at = start;
    while at.duration_since(start) < FAILURE_RETRY_WINDOW {
        backoff.after_pass(true, failed, at);
        assert_eq!(
            backoff.interval(),
            RECONCILE_INTERVAL,
            "at {:?}",
            at - start
        );
        at += RECONCILE_INTERVAL;
    }
    backoff.after_pass(true, failed, at);
    assert_eq!(
        backoff.interval(),
        RECONCILE_INTERVAL * 2,
        "a peer gone for longer is not dialled every 3 s forever"
    );
    backoff.after_pass(true, quiet, at);
    backoff.after_pass(true, failed, at);
    assert_eq!(
        backoff.interval(),
        RECONCILE_INTERVAL,
        "a new failure after a success holds the short interval again"
    );
}

/// A device's publisher over a fresh repository, for the conflict-watch
/// tests: concurrent heads are published from it directly, as a pull from
/// another writer would apply them.
struct ConflictWatchFixture {
    _dir: tempfile::TempDir,
    repository: Arc<RecordRepository>,
    publisher: RecordPublisher,
    workspace_id: uuid::Uuid,
    actor: FileActor,
}

impl ConflictWatchFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repository = Arc::new(RecordRepository::open(dir.path().join("records")).unwrap());
        let (device_key, device) = collab_types::generate_keypair();
        let (_, person) = collab_types::generate_keypair();
        let actor = FileActor { person, device };
        let workspace_id = uuid::Uuid::from_u128(608);
        let publisher =
            RecordPublisher::new(Arc::clone(&repository), workspace_id, actor, device_key).unwrap();
        Self {
            _dir: dir,
            repository,
            publisher,
            workspace_id,
            actor,
        }
    }

    fn version(
        &self,
        record_id: FileRecordId,
        revision: u8,
        parents: &[FileVersionId],
    ) -> FileVersionV1 {
        FileVersionV1 {
            id: FileVersionId::new(),
            record_id,
            parents: parents.to_vec(),
            actor: self.actor,
            timestamp_ms: u64::from(revision),
            content_hash: Some(ContentHash([revision; 32])),
            metadata_hash: ContentHash([revision.wrapping_add(1); 32]),
            provenance: FileProvenance::Edit,
        }
    }

    fn seed(&self) -> (FileRecordId, FileVersionV1) {
        let record_id = FileRecordId::new();
        let base = self.version(record_id, 1, &[]);
        let metadata = FileMetadataV1 {
            schema_version: 1,
            record_id,
            content_hash: base.content_hash,
            byte_len: 1,
            mime: Some("text/plain".into()),
            previews: Vec::new(),
            latest_actor: Some(self.actor),
            observed_at_ms: 1,
            index_status: IndexStatus::Indexed,
            properties: None,
        };
        self.publisher
            .publish(&metadata, std::slice::from_ref(&base))
            .unwrap();
        (record_id, base)
    }

    /// The conflict part of one reconciliation pass at `now`, as
    /// `reconcile_loop` runs it: whether a record conflict was published,
    /// and the namespace conflicts found.
    fn pass(
        &self,
        watch: &mut conflict_watch::ConflictWatch,
        namespace: &Mutex<Namespace>,
        now: Instant,
    ) -> (bool, Vec<crate::conflict::ContentConflict>) {
        watch.begin_pass(now);
        let published =
            watch.publish_record_conflicts(&self.publisher, &self.repository, self.workspace_id);
        (published, watch.namespace_conflicts(namespace))
    }
}

/// Two namespaces holding one file, `doc`, edited concurrently: the second
/// one's change not yet merged into the first.
fn concurrently_edited_namespaces() -> (Namespace, Namespace) {
    let mut namespace = Namespace::new("A").unwrap();
    let ino = namespace
        .create_file(ROOT_INO, "doc", Some(&Hash::from([1; 32])), 1, 1)
        .unwrap();
    let mut other = Namespace::from_bytes("B", &namespace.save()).unwrap();
    let other_ino = other.lookup(ROOT_INO, "doc").unwrap().ino.0;
    other.prime_baseline();
    namespace.set_content(ino, Some(&Hash::from([10; 32])), 10, 5);
    other.set_content(other_ino, Some(&Hash::from([20; 32])), 20, 5);
    (namespace, other)
}

#[test]
fn idle_timer_pass_runs_no_conflict_scan() {
    let fixture = ConflictWatchFixture::new();
    for _ in 0..1_000 {
        fixture.seed();
    }
    let mut namespace = Namespace::new("idle-writer").unwrap();
    let doc = namespace
        .create_file(ROOT_INO, "doc", Some(&Hash::from([1; 32])), 1, 1)
        .unwrap();
    let namespace = Mutex::new(namespace);
    let mut watch = conflict_watch::ConflictWatch::default();
    let start = Instant::now();

    // The first pass after start looks everywhere.
    let (published, conflicts) = fixture.pass(&mut watch, &namespace, start);
    assert!(!published);
    assert!(conflicts.is_empty());
    assert_eq!(fixture.repository.concurrent_head_checks(), 1_000);
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 1);

    // Timer passes that received nothing look nowhere.
    for pass in 1..=3 {
        let (published, conflicts) =
            fixture.pass(&mut watch, &namespace, start + RECONCILE_INTERVAL * pass);
        assert!(!published);
        assert!(conflicts.is_empty());
        assert_eq!(
            watch.namespace_pass().nodes,
            0,
            "an idle pass examines no node"
        );
    }
    assert_eq!(fixture.repository.concurrent_head_checks(), 1_000);
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 1);

    // This device's own namespace writes cannot conflict with each other.
    namespace
        .lock()
        .unwrap()
        .set_content(doc, Some(&Hash::from([2; 32])), 2, 2);
    fixture.pass(&mut watch, &namespace, start + RECONCILE_INTERVAL * 4);
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 1);

    // The slow sweep still looks everywhere, for a signal missed somehow.
    fixture.pass(
        &mut watch,
        &namespace,
        start + conflict_watch::CONFLICT_SWEEP_INTERVAL,
    );
    assert_eq!(fixture.repository.concurrent_head_checks(), 2_000);
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 2);
}

#[test]
fn received_change_is_checked_for_conflict() {
    let fixture = ConflictWatchFixture::new();
    for _ in 0..50 {
        fixture.seed();
    }
    let (record_id, base) = fixture.seed();
    let left = fixture.version(record_id, 2, &[base.id]);
    fixture.publisher.publish_structural_version(&left).unwrap();
    let (namespace, mut other) = concurrently_edited_namespaces();
    let namespace = Mutex::new(namespace);

    let mut watch = conflict_watch::ConflictWatch::default();
    let start = Instant::now();
    fixture.pass(&mut watch, &namespace, start);
    let checks = fixture.repository.concurrent_head_checks();
    assert_eq!(checks, 51);
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 1);

    // What an exchange delivers: the other writer's
    // concurrent edit, and a version concurrent with `left`.
    let delta = other.save_incremental();
    namespace
        .lock()
        .unwrap()
        .apply_remote_and_resolve(&delta)
        .unwrap();
    let right = fixture.version(record_id, 3, &[base.id]);
    fixture
        .publisher
        .publish_structural_version(&right)
        .unwrap();

    let (published, conflicts) = fixture.pass(&mut watch, &namespace, start + RECONCILE_INTERVAL);
    assert!(published, "the concurrent heads become one open conflict");
    assert_eq!(
        fixture.repository.concurrent_head_checks(),
        checks + 1,
        "only the record that was written is checked"
    );
    let snapshot = fixture.publisher.snapshot(record_id).unwrap().unwrap();
    assert_eq!(snapshot.conflicts.len(), 1);
    assert_eq!(
        conflicts.len(),
        1,
        "one conflict copy for the concurrent edit"
    );
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 2);
    assert!(crate::conflict::materialize(
        &mut namespace.lock().unwrap(),
        &conflicts[0],
        10,
        6
    ));

    // Publishing the conflict was itself a write: the next pass checks
    // that one record again and finds it settled; the one after checks
    // nothing. Materialising the copy was a local change: no new scan.
    let (published, conflicts) =
        fixture.pass(&mut watch, &namespace, start + RECONCILE_INTERVAL * 2);
    assert!(!published);
    assert!(conflicts.is_empty());
    let (published, _) = fixture.pass(&mut watch, &namespace, start + RECONCILE_INTERVAL * 3);
    assert!(!published);
    assert_eq!(fixture.repository.concurrent_head_checks(), checks + 2);
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 2);
    let snapshot = fixture.publisher.snapshot(record_id).unwrap().unwrap();
    assert_eq!(snapshot.conflicts.len(), 1);
}

#[test]
fn unmaterialised_namespace_conflict_is_scanned_for_again() {
    let (mut namespace, mut other) = concurrently_edited_namespaces();
    namespace.merge_doc(&mut other).unwrap();
    let namespace = Mutex::new(namespace);

    let mut watch = conflict_watch::ConflictWatch::default();
    let start = Instant::now();
    watch.begin_pass(start);
    assert_eq!(watch.namespace_conflicts(&namespace).len(), 1);
    // Its content was not reachable, so the loop asks again.
    watch.retry_namespace();
    watch.begin_pass(start + RECONCILE_INTERVAL);
    assert_eq!(watch.namespace_conflicts(&namespace).len(), 1);
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 2);
    // Merging what this namespace already holds brings nothing to scan.
    namespace.lock().unwrap().merge_doc(&mut other).unwrap();
    watch.begin_pass(start + RECONCILE_INTERVAL * 2);
    assert!(watch.namespace_conflicts(&namespace).is_empty());
    assert_eq!(namespace.lock().unwrap().conflict_scans(), 2);
}

/// Bytes the endpoints have put on direct paths (IPv4 and IPv6).
fn direct_bytes_sent(endpoints: &[&Endpoint]) -> u64 {
    endpoints
        .iter()
        .map(|endpoint| {
            let socket = &endpoint.metrics().socket;
            socket
                .send_ipv4
                .get()
                .saturating_add(socket.send_ipv6.get())
        })
        .sum()
}

/// A namespace holding `changes` changes, one file per hundred.
fn namespace_with_history(author: &str, changes: u64) -> Namespace {
    let mut namespace = Namespace::new(author).unwrap();
    let mut file = ROOT_INO;
    for change in 0..changes {
        if change.is_multiple_of(100) {
            file = namespace
                .create_file(ROOT_INO, format!("file-{change}"), None, 0, change)
                .unwrap();
        } else {
            let content = Hash::new(change.to_le_bytes());
            assert!(namespace.set_content(file, Some(&content), change, change));
        }
        namespace.heads();
    }
    namespace
}

/// A measurement, not a gate: run with `TILO_FS_IDLE_MEASURE=<seconds>`
/// and `TILO_FS_NATIVE_TRACE=1 … -- --nocapture`. Two writer devices under
/// an ACL, each with a record plane and the production reconciliation
/// loop, nothing changing. Prints the bytes both endpoints sent over the
/// window and marks its bounds, so the trace's `sync pass begins` lines
/// between the marks count the passes. Two endpoints with no workspace are
/// measured first: their traffic (address discovery, relay) is the floor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_workspace_cost_measurement() {
    let Some(window) = std::env::var("TILO_FS_IDLE_MEASURE")
        .ok()
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .map(Duration::from_secs)
    else {
        eprintln!("idle workspace measurement disabled; set TILO_FS_IDLE_MEASURE=<seconds>");
        return;
    };
    let settle = Duration::from_secs(60);

    let (floor_a, _, _) = bind_certified_endpoint().await;
    let (floor_b, _, _) = bind_certified_endpoint().await;
    tokio::time::sleep(settle).await;
    let before = direct_bytes_sent(&[&floor_a, &floor_b]);
    tokio::time::sleep(window).await;
    let floor = direct_bytes_sent(&[&floor_a, &floor_b]) - before;
    drop((floor_a, floor_b));

    let host_temp = tempfile::tempdir().unwrap();
    let device_temp = tempfile::tempdir().unwrap();
    let (host_ep, host_lookup, host_identity) = bind_certified_endpoint().await;
    let (device_ep, device_lookup, device_identity) = bind_certified_endpoint().await;
    let writers = [
        host_identity.user_public_key(),
        device_identity.user_public_key(),
    ];
    let history = namespace_with_history("history", 5_000).save();
    let host_store = Arc::new(ContentStore::memory());
    let (_host, ticket) = Workspace::host_with_records(
        host_ep.clone(),
        host_lookup,
        Arc::clone(&host_store),
        Arc::new(Hydrator::new(host_ep.clone(), host_store)),
        Arc::new(Mutex::new(Namespace::from_bytes("host", &history).unwrap())),
        host_identity,
        Some(policy_read_write(&writers, &[])),
        record_services(host_temp.path()),
    )
    .unwrap();
    let device_store = Arc::new(ContentStore::memory());
    let _device = Workspace::join_with_records(
        SyncNode {
            endpoint: device_ep.clone(),
            lookup: device_lookup,
            store: Arc::clone(&device_store),
            hydrator: Arc::new(Hydrator::new(device_ep.clone(), device_store)),
            namespace: Arc::new(Mutex::new(
                Namespace::from_bytes("device", &history).unwrap(),
            )),
            identity: device_identity,
            policy: Some(policy_read_write(&writers, &[])),
        },
        ticket,
        record_services(device_temp.path()),
    )
    .await
    .unwrap();
    tokio::time::sleep(settle).await;
    eprintln!("MEASURE window begins");
    let before = direct_bytes_sent(&[&host_ep, &device_ep]);
    // Per-thread wakeups of this process (both nodes) over the window, on
    // Linux, where /proc has them.
    let threads_before = tilo_test_env::idle::sample_process(std::process::id());
    tokio::time::sleep(window).await;
    let sent = direct_bytes_sent(&[&host_ep, &device_ep]) - before;
    eprintln!("MEASURE window ends");
    match threads_before.and_then(|threads_before| {
        let threads_after = tilo_test_env::idle::sample_process(std::process::id())?;
        Ok(tilo_test_env::idle::Wakeups::between(
            threads_before,
            threads_after,
            window,
        ))
    }) {
        Ok(wakeups) => eprintln!("MEASURE {wakeups}"),
        Err(error) => eprintln!("MEASURE no per-thread wakeups here: {error}"),
    }
    let per_device_minute = |bytes: u64| bytes * 60 / window.as_secs().max(1) / 2;
    eprintln!(
        "MEASURE idle over {} s: both endpoints sent {sent} B, floor {floor} B; \
         per device per minute {} B, floor {} B",
        window.as_secs(),
        per_device_minute(sent),
        per_device_minute(floor)
    );
}

/// One reconciliation of `records` with the session host at `remote`, on a
/// session of its own, as a pass makes it.
async fn pull_over_session(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    records: &RecordPlane,
) -> Result<bool> {
    let sessions = SessionPool::new(endpoint.clone(), identity.clone(), records.workspace_id);
    let session = sessions.get(&remote).await?;
    // Whatever content arrives inline lands in a store of its own: these
    // tests read records, not bytes.
    let hydrator = Hydrator::new(endpoint.clone(), Arc::new(ContentStore::memory()));
    session::pull_records(
        &sessions,
        &session,
        identity,
        policy,
        records,
        &hydrator,
        crate::record_materializer::ReconcileScope::Everything,
    )
    .await
}

/// A member serving `repository` and `store` on the session protocol, as a
/// device of the workspace does, for the replica receipts it signs.
fn replica_session_host(
    endpoint: &Endpoint,
    identity: &FsTransportIdentity,
    policy: PeerPolicy,
    workspace_id: uuid::Uuid,
    share_authority: &std::path::Path,
    repository: Arc<RecordRepository>,
    store: Arc<ContentStore>,
) -> SessionProtocol {
    let services = RecordServices {
        authority: Arc::new(ShareAuthority::open(share_authority).unwrap()),
        repository,
        access_registry: Arc::new(RecordAccessRegistry::new(32, 60_000).unwrap()),
        mutation_sink: None,
        materializer: None,
        availability: None,
        workspace_authority: None,
    };
    let records = build_record_plane(
        endpoint,
        identity,
        Arc::clone(&store),
        workspace_id,
        Some(services),
    )
    .unwrap()
    .unwrap();
    let (announcements, _) = mpsc::unbounded_channel();
    SessionProtocol {
        endpoint: endpoint.clone(),
        identity: identity.clone(),
        policy,
        store,
        records: Some(records),
        workspace_id,
        announcements,
        pool: std::sync::Weak::new(),
    }
}

/// The peer at `remote`'s verified receipt for `request` over a session, or
/// `None` when it holds no exact durable copy.
async fn replica_ack_over_session(
    endpoint: &Endpoint,
    remote: EndpointAddr,
    identity: &FsTransportIdentity,
    request: &ReplicaAckRequestV1,
) -> Option<crate::availability::ReplicaObservation> {
    let sessions = SessionPool::new(endpoint.clone(), identity.clone(), request.workspace_id);
    let session = sessions.get(&remote).await.unwrap();
    session::replica_acks(&sessions, &session, identity, std::slice::from_ref(request))
        .await
        .unwrap()
        .pop()
        .flatten()
}
