//! Ordinary production startup: no scoped fixture, alternate app or fake lease.
use super::*;
use crate::{host_signing::SignatureMailbox, owner_commit::OwnerCommitMailbox};
use std::time::Duration;
use ui::collaboration::{CollabEvent, CollabRequest};

struct Fixture {
    host: ActionHost,
    caller: CollaborationManager,
    owner: SessionOwner,
    authority: Authority,
    signatures: SignatureMailbox,
    _commits: OwnerCommitMailbox,
    _network: tokio::sync::mpsc::Receiver<NeighbourMsg>,
    record: String,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
        if !owner
            .peer()
            .rows()
            .iter()
            .any(|row| row.app_id == "so.tilo.notes")
        {
            use qos_peer_state::{
                desktop_command::{BoundCommand, CommandSource, DesktopCommand, TabPlacement},
                owner_commit::{CommitOperation, OwnerCommitRequest},
            };
            let strip = project(owner.peer()).unwrap().workspaces[0].strips[0].id.0;
            owner
                .commit_desktop_command(&OwnerCommitRequest {
                    room: owner.peer().room().into(),
                    operation: CommitOperation::DesktopCommand {
                        invocation: uuid::Uuid::new_v4(),
                        caller: owner.identity.native_cert.clone(),
                        command: BoundCommand {
                            source: CommandSource {
                                room: owner.peer().room().into(),
                                heads: owner
                                    .peer()
                                    .admitted_room()
                                    .unwrap()
                                    .clone()
                                    .get_heads()
                                    .iter()
                                    .map(|h| h.0)
                                    .collect(),
                            },
                            command: DesktopCommand::OpenTab {
                                app: "so.tilo.notes".into(),
                                placement: TabPlacement::NewGroup { strip, after: None },
                            },
                        },
                    },
                })
                .unwrap();
        }
        let record = owner
            .peer()
            .rows()
            .into_iter()
            .find(|row| row.app_id == "so.tilo.notes")
            .unwrap()
            .record_id;
        let authority = Authority::new(CommittedAuthority::new(&owner));
        authority.set_ready(true);
        let (handle, commits) = OwnerCommitMailbox::bounded(16);
        let (signing, signatures) = HostSigning::new(handle);
        let (sender, network) = tokio::sync::mpsc::channel(64);
        // This is exactly the ordinary entry used by ActionRuntime, with its
        // production definition selection and scoped_fixture=false.
        let host =
            ActionHost::start_with_collaboration(&owner, authority.clone(), signing, sender, None)
                .unwrap();
        assert!(!host.scoped_fixture);
        let caller = Self::connect_caller(&host, &owner, &record);
        Self {
            host,
            caller,
            owner,
            authority,
            signatures,
            _commits: commits,
            _network: network,
            record,
            _root: root,
        }
    }
    fn connect_caller(
        host: &ActionHost,
        owner: &SessionOwner,
        record: &str,
    ) -> CollaborationManager {
        let room: uuid::Uuid = owner.peer().room().parse().unwrap();
        let stream = host
            .broker
            .connect_local_worker(
                CrdtRoute::record("so.tilo.notes", room.to_string(), record),
                &owner.identity.native_cert,
            )
            .unwrap();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_string(),
            device_certificate: serde_json::to_vec(&owner.identity.native_cert).unwrap(),
            proof: owner
                .identity
                .native
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &room.to_string(),
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap();
        CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            "so.tilo.notes",
            room,
            Some(record.into()),
            None,
            CertifiedEndpointIdentity::new(presentation, owner.identity.native.clone()),
        )
        .unwrap()
    }
    async fn pump(&mut self) -> Result<(), String> {
        self.host.pump(&self.owner)?;
        while let Ok(pending) = self.signatures.try_recv() {
            // Existing exact startup replay only; a real new mutation keeps its
            // existing final refusal, never an alternate admission/barrier.
            let before = self.owner.committed_generation();
            let disk = std::fs::read(self.owner.root().join("session.bin")).unwrap();
            let update = self
                .authority
                .committed
                .begin(&self.owner)
                .map_err(|e| e.to_string())?;
            let (complete, result) = pending.apply(&mut self.owner);
            update.publish(&self.owner).map_err(|e| e.to_string())?;
            complete();
            result?;
            assert_eq!(self.owner.committed_generation(), before);
            assert_eq!(
                std::fs::read(self.owner.root().join("session.bin")).unwrap(),
                disk
            );
        }
        while self._network.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(())
    }
    async fn ready_notes(&mut self) -> (u64, String) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            // Deliberately pump before looking at a catalogue: the missing
            // production authorizer must fail at the actual worker health gate.
            self.pump()
                .await
                .expect("ordinary production worker must have its original paired authorizer");
            self.caller.poll();
            if let Some(address) = self
                .host
                .broker
                .published_action_surfaces()
                .iter()
                .filter(|surface| {
                    surface.live
                        && surface.app_id == "so.tilo.notes"
                        && surface.route.record_id == self.record
                })
                .find_map(|surface| {
                    surface
                        .actions
                        .iter()
                        .find(|action| {
                            action.resource_kind == "notes.notebook" && action.action == "new-page"
                        })
                        .map(|action| (surface.peer_id, action.resource.clone()))
                })
            {
                // Ordinary mounted components publish opaque resource aliases.
                // Carry the actual broker address; never reconstruct it from
                // the Notes provider's private domain resource name.
                return address;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "actual Notes never published its notebook new-page action"
            );
        }
    }
    async fn query(
        &mut self,
        peer: u64,
        resource: &str,
    ) -> Result<runtime_collab::ActionDirectoryPage, String> {
        let id = 913;
        if !self.caller.send(CollabRequest::QueryActionDirectory {
            request_id: id,
            query: runtime_collab::ActionDirectoryQuery {
                peer_id: peer,
                revision: String::new(),
                cursor: vec![],
                needle: String::new(),
                parent_resource: resource.into(),
                max_results: 32,
                max_bytes: 64 * 1024,
                max_work: 128,
                max_depth: 1,
            },
        }) {
            return Err("actual certified caller refused query".into());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            for event in self.caller.poll() {
                if let CollabEvent::ActionDirectoryPage { request_id, result } = event {
                    if request_id == id {
                        return result;
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("certified directory timed out".into());
            }
            self.pump().await?;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_production_authorizer_ordinary_startup_and_certified_directory() {
    let mut fixture = Fixture::new();
    let (peer, resource) = fixture.ready_notes().await;
    let before = std::fs::read(fixture.owner.root().join("session.bin")).unwrap();
    let page = fixture.query(peer, &resource).await.unwrap();
    assert!(!page.revision.is_empty());
    assert!(
        page.rows.iter().any(|row| row.resource == resource
            && row.resource_kind == "notes.notebook"
            && row.action.as_deref() == Some("new-page")),
        "real Notes action absent from authenticated read-only directory: {page:?}"
    );
    assert_eq!(
        std::fs::read(fixture.owner.root().join("session.bin")).unwrap(),
        before
    );
    fixture.host.close(&fixture.owner).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_production_authorizer_retired_original_owner_denies_certified_read() {
    let mut fixture = Fixture::new();
    let (peer, resource) = fixture.ready_notes().await;
    fixture.query(peer, &resource).await.unwrap();
    let before = std::fs::read(fixture.owner.root().join("session.bin")).unwrap();
    let time = fixture.host.registered_policy_time().clone();
    fixture.authority.committed.invalidate();
    assert!(time.now_ns().is_err());
    assert!(fixture.host.pump(&fixture.owner).is_err());
    // The already admitted actual socket must receive a refusal from the
    // broker's current owner gate, not a new synthetic caller or local timeout.
    assert!(fixture.caller.send(CollabRequest::QueryActionDirectory {
        request_id: 991,
        query: runtime_collab::ActionDirectoryQuery {
            peer_id: peer,
            revision: String::new(),
            cursor: vec![],
            needle: String::new(),
            parent_resource: resource.clone(),
            max_results: 32,
            max_bytes: 64 * 1024,
            max_work: 128,
            max_depth: 1,
        },
    }));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = fixture
            .caller
            .poll()
            .into_iter()
            .find_map(|event| match event {
                CollabEvent::ActionDirectoryPage {
                    request_id: 991,
                    result,
                } => Some(result),
                _ => None,
            })
        {
            let error = result.expect_err("retired original policy must not disclose a page");
            assert!(
                !error.contains("timed out"),
                "a timeout is not an explicit retired-owner refusal: {error}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "actual broker never replied to retired caller"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        std::fs::read(fixture.owner.root().join("session.bin")).unwrap(),
        before
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_production_authorizer_replacement_does_not_revive_original_connection() {
    let mut original = Fixture::new();
    let (peer, resource) = original.ready_notes().await;
    original.query(peer, &resource).await.unwrap();
    for _ in 0..10 {
        original.pump().await.unwrap();
        original.caller.poll();
    }
    let Fixture {
        mut host,
        mut caller,
        owner,
        authority,
        _root,
        record,
        ..
    } = original;
    let old_socket = host.broker.capnp_socket().to_path_buf();
    host.close(&owner).unwrap();
    drop(host); // joins the original broker and drops its actual worker registry
    assert!(!old_socket.exists());
    let (handle, commits) = OwnerCommitMailbox::bounded(16);
    let (signing, signatures) = HostSigning::new(handle);
    let (sender, network) = tokio::sync::mpsc::channel(64);
    let host =
        ActionHost::start_with_collaboration(&owner, authority.clone(), signing, sender, None)
            .unwrap();
    assert!(!host.scoped_fixture);
    assert_ne!(host.broker.capnp_socket(), old_socket);
    let next_caller = Fixture::connect_caller(&host, &owner, &record);
    let mut replacement = Fixture {
        host,
        caller: next_caller,
        owner,
        authority,
        signatures,
        _commits: commits,
        _network: network,
        record,
        _root,
    };
    let (new_peer, new_resource) = replacement.ready_notes().await;
    replacement.query(new_peer, &new_resource).await.unwrap();
    // `is_connected` is the SDK bridge failure latch, not a passive socket
    // probe. Test the actual retained capability against both old and new
    // directory targets after the original broker has shut down and joined.
    for (request_id, target_peer, target_resource) in
        [(993, peer, resource), (994, new_peer, new_resource.clone())]
    {
        assert!(caller.send(CollabRequest::QueryActionDirectory {
            request_id,
            query: runtime_collab::ActionDirectoryQuery {
                peer_id: target_peer,
                revision: String::new(),
                cursor: vec![],
                needle: String::new(),
                parent_resource: target_resource,
                max_results: 32,
                max_bytes: 64 * 1024,
                max_work: 128,
                max_depth: 1,
            },
        }));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = caller.poll().into_iter().find_map(|event| match event {
                CollabEvent::ActionDirectoryPage {
                    request_id: id,
                    result,
                } if id == request_id => Some(result),
                _ => None,
            }) {
                let error =
                    result.expect_err("old connection must not access old or replacement worker");
                assert!(
                    !error.contains("timed out"),
                    "a timeout is not an explicit old-connection refusal: {error}"
                );
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "old original connection did not explicitly refuse directory target {request_id}"
            );
            // Keep the replacement fully serviceable while probing the old
            // connection, so lack of a worker pump cannot explain refusal.
            replacement.pump().await.unwrap();
        }
    }
    replacement.query(new_peer, &new_resource).await.unwrap();
    replacement.host.close(&replacement.owner).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_production_authorizer_real_invocation_keeps_final_mutation_closed() {
    let mut fixture = Fixture::new();
    let (peer, resource) = fixture.ready_notes().await;
    fixture.query(peer, &resource).await.unwrap();
    for _ in 0..10 {
        fixture.pump().await.unwrap();
        fixture.caller.poll();
    }
    let disk = std::fs::read(fixture.owner.root().join("session.bin")).unwrap();
    let generation = fixture.owner.committed_generation();
    let records = fixture
        .host
        .owners
        .open("so.tilo.notes", fixture.owner.peer().room())
        .unwrap();
    let id = if fixture.record.is_empty() {
        RecordId::package_primary()
    } else {
        RecordId::parse(&fixture.record).unwrap()
    };
    let local_before = records.read(&id, |doc| doc.clone().save()).unwrap();
    assert!(fixture.caller.send(CollabRequest::InvokePublishedAction {
        request_id: 941,
        peer_id: peer,
        resource,
        action: "new-page".into(),
        arguments: serde_json::Map::from_iter([(
            "value".into(),
            serde_json::json!("Original production caller")
        )]),
        confirmation: serde_json::json!({}),
    }));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let invocation = loop {
        fixture.host.pump(&fixture.owner).unwrap();
        if let Some(result) = fixture
            .caller
            .poll()
            .into_iter()
            .find_map(|event| match event {
                CollabEvent::PublishedInvocationResult {
                    request_id: 941,
                    result,
                } => Some(result),
                _ => None,
            })
        {
            break result
                .expect("genuine production caller must be admitted")
                .0;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    loop {
        if let Err(error) = fixture.pump().await {
            assert_eq!(
                error,
                "startup replay cannot acknowledge invocation contributors"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "real scoped invocation never reached its unchanged final native refusal: {invocation}"
        );
    }
    assert_ne!(
        records.read(&id, |doc| doc.clone().save()).unwrap(),
        local_before,
        "actual Notes handler must have run under its original invocation"
    );
    assert_eq!(fixture.owner.committed_generation(), generation);
    assert_eq!(
        std::fs::read(fixture.owner.root().join("session.bin")).unwrap(),
        disk
    );
    // Do not claim success or acknowledge the new local bytes. Actual close
    // retains their existing dirty recovery custody instead.
    assert!(fixture.host.close(&fixture.owner).is_err());
    assert!(fixture.owner.root().join("actions/recovery.json").is_file());
}
