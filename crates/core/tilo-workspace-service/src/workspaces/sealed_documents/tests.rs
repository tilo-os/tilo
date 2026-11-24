#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the member's helpers are part of the test's assertions"
)]

use super::super::Workspaces;
use super::super::tests::governor_with;
use super::fixtures::{SealingMember, governor_published, member_device};
use super::*;
use crate::workspaces::fixtures::test_host;
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

pub(super) const APP: &str = "so.tilo.notes";
pub(super) const RESOURCE: &str = "welcome";

pub(super) fn title(bytes: &[u8]) -> Option<String> {
    automerge::AutoCommit::load(bytes)
        .ok()?
        .get(ROOT, "title")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

pub(super) async fn set_title(
    member: &SealingMember,
    workspaces: &Workspaces,
    workspace: &str,
    value: &str,
) {
    member
        .writes(workspaces, workspace, |document| {
            document.put(ROOT, "title", value).unwrap();
            document.commit_edit("fixture", "title", &member.person());
        })
        .await;
}

pub(super) async fn readable(workspaces: &Workspaces, workspace: &str) -> Option<String> {
    title(
        &workspaces
            .resource_document(workspace, APP, RESOURCE)
            .await
            .unwrap()?,
    )
}

/// Wait until the node reads `wanted` as [`RESOURCE`]'s title: every write that got there is
/// followed.
pub(super) async fn follows(workspaces: &Workspaces, workspace: &str, wanted: &str) {
    tilo_test_env::within("the node follows the record's writes", async {
        while readable(workspaces, workspace).await.as_deref() != Some(wanted) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Wait until the node's copy of [`RESOURCE`] holds `wanted` at `field`: a write that changes
/// no title is followed too, and a check made before it lands reads a copy without it.
pub(super) async fn follows_field(
    workspaces: &Workspaces,
    workspace: &str,
    field: &str,
    wanted: &str,
) {
    tilo_test_env::within("the node follows the record's writes", async {
        loop {
            let held = workspaces
                .resource_document(workspace, APP, RESOURCE)
                .await
                .expect("the node reads its record");
            let holds = held
                .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                .and_then(|doc| {
                    doc.get(automerge::ROOT, field)
                        .ok()
                        .flatten()
                        .and_then(|(value, _)| value.into_string().ok())
                });
            if holds.as_deref() == Some(wanted) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A node governing a "Lounge" workspace, the workspace's record of [`APP`] it holds, and
/// a member device that writes it sealed under the keys the node takes part in.
pub(super) struct Lounge {
    pub(super) _dir: tempfile::TempDir,
    pub(super) server: crate::workspaces::fixtures::TestHost,
    pub(super) workspaces: Workspaces,
    pub(super) workspace: String,
    pub(super) workspace_id: WorkspaceId,
    pub(super) governor: DeviceCert,
    pub(super) governor_key: ed25519_dalek::SigningKey,
    pub(super) member_key: ed25519_dalek::SigningKey,
    pub(super) member_cert: DeviceCert,
    pub(super) member: SealingMember,
}

pub(super) async fn lounge() -> Lounge {
    lounge_reading(0).await
}

/// [`lounge`] whose host reads its workspace's link with room for only
/// `capacity` frames it did not read yet; `0` keeps the link's own.
async fn lounge_reading(capacity: usize) -> Lounge {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host(dir.path(), &key, &cert).await;
    lounge_on(dir, server, key, cert, capacity).await
}

/// [`lounge`] whose host keeps `keys`: a device whose keys arrive after it opened, or that keeps
/// them across a restart.
pub(super) async fn lounge_holding(
    keys: std::sync::Arc<crate::workspaces::fixtures::TestGroupKeys>,
) -> Lounge {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    lounge_on(dir, server, key, cert, 0).await
}

async fn lounge_on(
    dir: tempfile::TempDir,
    server: crate::workspaces::fixtures::TestHost,
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    capacity: usize,
) -> Lounge {
    let workspaces = server.workspaces().clone();
    if capacity > 0 {
        workspaces.bound_link_readers(capacity);
    }
    let created = workspaces.create("Lounge").await.unwrap();
    let (member_key, member_cert) =
        member_device(&workspaces, &created.workspace, (&key, &cert), 3).await;
    workspaces
        .author_workspace_document(&created.workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member = SealingMember::join(
        &workspaces,
        &created.workspace,
        (APP, RESOURCE),
        member_key.clone(),
    )
    .await;
    Lounge {
        _dir: dir,
        server,
        workspaces,
        workspace: created.workspace,
        workspace_id: created.workspace_id,
        governor: cert,
        governor_key: key,
        member_key,
        member_cert,
        member,
    }
}

/// [`lounge`] with its member's own link to the node's host, as the member's phone or laptop
/// reaches it.
pub(super) async fn lounge_with_link() -> (Lounge, Session) {
    lounge_reading_with_link(0).await
}

/// [`lounge_with_link`] over [`lounge_reading`].
async fn lounge_reading_with_link(capacity: usize) -> (Lounge, Session) {
    // The host is heap-held from the moment it exists: a node's state is far larger than a
    // test's future wants to keep in its frame while the member's link is set up.
    let lounge = Box::new(lounge_reading(capacity).await);
    let session = Box::pin(member_session(
        &lounge.server,
        &lounge.member_key,
        &lounge.member_cert,
        lounge.workspace_id,
        capacity,
    ))
    .await;
    (*lounge, session)
}

/// A node governs a workspace whose record its members write sealed. It reads
/// the document through the workspace's keys it holds as a member and resets it
/// with a write of its own, sealed like theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_reads_and_resets_a_record_its_members_seal() {
    let Lounge {
        _dir,
        server,
        workspaces,
        workspace,
        governor,
        member,
        ..
    } = lounge().await;
    set_title(&member, &workspaces, &workspace, "graffiti").await;
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("graffiti")
    );

    workspaces
        .author_workspace_document(&workspace, &governor, (APP, RESOURCE), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .expect("the governor writes a sealed record sealed");
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("Welcome")
    );
    let own = governor_published(&workspaces, &workspace, RESOURCE, &governor)
        .await
        .expect("the governor published its write");
    assert!(own.encryption.is_some(), "never in the clear");
    member.take_in(&own);
    assert_eq!(
        title(&member.document().save()).as_deref(),
        Some("Welcome"),
        "a member opens the governor's write"
    );
    drop(workspaces);
    server.shutdown().await;
}

/// A public workspace's chat takes thousands of writes within hours. Past three
/// quarters of a sealed history's bound the node compacts it into one sealed
/// checkpoint, so its copy goes on following the record, and reads it whole.
/// It publishes the compaction as a member does: every member's history then
/// matches the copy, and their next compaction folds what the copy holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_compacts_a_sealed_history_before_it_fills() {
    let Lounge {
        _dir,
        server,
        workspaces,
        workspace,
        member,
        ..
    } = lounge().await;
    // More writes than the history's bound (4096) holds.
    for index in 0..4200 {
        set_title(&member, &workspaces, &workspace, &format!("line {index}")).await;
    }
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("line 4199")
    );
    let state = workspaces.workspace(&workspace).await.unwrap();
    let due = state.lock().await.peer().publication().sealed_to_compact();
    assert!(due.is_empty(), "the history was compacted: {due:?}");
    let published = state
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .filter_map(|publication| postcard::from_bytes::<NeighbourMsg>(&publication.bytes).ok())
        .filter(|message| {
            matches!(message, NeighbourMsg::AppDocument { envelope, .. }
                if envelope.kind == collab_types::OpKind::Compaction)
        })
        .count();
    assert!(published > 0, "the compaction is published to members");
    drop((state, workspaces));
    server.shutdown().await;
}

/// Every retained write of the workspace's resource, for `workspace_id`, in replay
/// order: what a newcomer is sent.
///
/// Read once the workspace's store settled every write its owner took before
/// the call ([`crate::workspace_documents::WorkspaceDocuments::retained_settled`]),
/// as a newcomer's replay is captured in the owner's order. The node's own copy
/// holds a write as soon as the node is handed it, before the store does: a
/// read made beside the owner, the moment the node follows a write, can come
/// before the store holds that write.
pub(super) async fn retained_writes(
    server: &crate::workspaces::fixtures::TestHost,
    workspace_id: WorkspaceId,
    record: &str,
) -> Vec<SignedOp> {
    server
        .workspace_documents()
        .retained_settled(workspace_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(
            |retained| match postcard::from_bytes::<NeighbourMsg>(&retained.body) {
                Ok(NeighbourMsg::AppDocument {
                    document_id,
                    envelope,
                    ..
                }) if document_id == record => Some(*envelope),
                _ => None,
            },
        )
        .collect()
}

/// A member device's link to `host`'s workspace, as the member's phone or laptop reaches it:
/// a dialer to the host, the member handshake on the link's first stream, and the workspace's
/// records dialled over it.
pub(super) struct Session {
    /// The link this device reads the workspace over.
    pub(super) link: tilo_peer_state::direct::DirectSync,
    /// Every frame the link brings this device, missing none of the workspace's frames.
    pub(super) heard: tilo_peer_state::direct::Reader,
    pairing: tokio::task::JoinHandle<Result<(), String>>,
}

impl Session {
    /// Send `message` to the host, as this device's own write travels.
    ///
    /// # Panics
    ///
    /// The frame could not be kept.
    pub(super) fn send(&self, message: NeighbourMsg) {
        self.link.send(message).unwrap();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.pairing.abort();
    }
}

/// `key`'s device, `cert`'s, linked to `host`'s workspace `workspace_id`: the link it reads the
/// workspace over, caught up, with room for only `capacity` frames heard and not read yet when
/// it is above zero.
pub(super) async fn member_session(
    host: &crate::workspaces::fixtures::TestHost,
    key: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
    workspace_id: WorkspaceId,
    capacity: usize,
) -> Session {
    let home = tilo_test_relay::relay().await;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Custom(home.relays()))
        .bind()
        .await
        .unwrap();
    let workspaces = host.workspaces();
    let host_device = workspaces.host_device();
    let host_cert = workspaces.device_cert().clone();
    let dialer = tilo_server_client::link::Dialer::from_endpoint(
        tilo_server_client::ServerAddress::at(host.node_addr()),
        endpoint,
    );
    let mut state = workspaces
        .state(&workspace_id.to_string())
        .await
        .expect("the host holds the workspace");
    // A device holds its own certificate, whether or not the workspace's roster names it yet:
    // a link dialled from that copy proves the device by it, and the host decides its
    // admission.
    if !state
        .certificates()
        .iter()
        .any(|kept| kept.device_public_key == cert.device_public_key)
    {
        let _ = state.receive_from(
            tilo_peer_state::TransportOrigin::ServerDelta(host_device),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(cert.clone()),
                revocations: Vec::new(),
            },
            std::time::SystemTime::now(),
        );
    }
    let mut link = tilo_peer_state::direct::DirectSync::new(state, key.clone());
    if capacity > 0 {
        link = link.with_heard_capacity(capacity);
    }
    let heard = link.reader();
    let mut caught_up = link.caught_up();
    let link_end = dialer
        .link(tilo_server_client::link::Purpose::Records)
        .await
        .expect("the host answers a records link");
    link_end
        .prove_member(cert, key, &host_cert)
        .await
        .expect("the host proves the member");
    let dialed = link
        .dial_over(std::sync::Arc::new(link_end), host_device)
        .await
        .expect("the link opens");
    let pairing = {
        let link = link.clone();
        tokio::spawn(async move { dialed.run(&link).await })
    };
    tilo_test_env::within("the device catches up with the host", async {
        caught_up.wait_for(|set| *set).await.unwrap();
    })
    .await;
    Session {
        link,
        heard,
        pairing,
    }
}

/// `member` writes `line` as the document's title and sends it to the host sealed, as the
/// member's own writes travel.
pub(super) fn seal_and_send_title(
    member: &SealingMember,
    session: &Session,
    workspace: &str,
    line: &str,
) {
    let ((), message) = member.seal(workspace, |doc| {
        doc.put(ROOT, "title", line).unwrap();
        doc.commit_edit("fixture", "title", &member.person());
    });
    session.send(message);
}

/// A newcomer opening `held` in order: the record as a device joining now reads it.
pub(super) fn newcomer_opens(member: &SealingMember, held: &[SignedOp]) -> automerge::AutoCommit {
    let mut newcomer = automerge::AutoCommit::new();
    for op in held {
        member.open_into(op, &mut newcomer);
    }
    newcomer
}

/// What a newcomer opening `held` in order reads as the record's title.
fn newcomer_reads(member: &SealingMember, held: &[SignedOp]) -> Option<String> {
    title(&newcomer_opens(member, held).save())
}

/// The workspace's retained writes, once what a newcomer opens from them is
/// `wanted`: a write the node follows is retained a moment after it, so a
/// read made in between is short of it.
pub(super) async fn retained_until(
    server: &crate::workspaces::fixtures::TestHost,
    workspace_id: WorkspaceId,
    member: &SealingMember,
    wanted: impl Fn(&mut automerge::AutoCommit) -> bool + Sync,
) -> Vec<SignedOp> {
    tilo_test_env::within("the retained writes to follow the node", async {
        loop {
            let held = retained_writes(server, workspace_id, RESOURCE).await;
            if wanted(&mut newcomer_opens(member, &held)) {
                return held;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
}

/// A record its members wrote sealed for a long time is folded by its
/// member device: a newcomer is sent the member's checkpoint and the writes
/// since, not every write ever made, and they open to the record as its
/// members wrote it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_checkpoint_folds_what_a_newcomer_replays() {
    const WRITES: usize = 400;
    let (
        Lounge {
            _dir,
            server,
            workspaces,
            workspace,
            workspace_id,
            member_key,
            member,
            ..
        },
        session,
    ) = lounge_with_link().await;
    for index in 0..WRITES {
        seal_and_send_title(&member, &session, &workspace, &format!("line {index}"));
    }
    follows(&workspaces, &workspace, "line 399").await;
    // The member's engine folds the history it holds into one checkpoint
    // and publishes it in place of every write it covers.
    let [compaction]: [NeighbourMsg; 1] = member
        .compact(&workspaces, &workspace)
        .await
        .try_into()
        .unwrap();
    session.send(compaction);
    // What a newcomer is sent past the checkpoint travels as itself.
    for index in WRITES..WRITES + 3 {
        seal_and_send_title(&member, &session, &workspace, &format!("line {index}"));
    }
    // What a newcomer is sent, opened: the member's checkpoint, then the
    // writes after it.
    let newest = format!("line {}", WRITES + 2);
    let held = tilo_test_env::within("the node retains the member's checkpoint", async {
        loop {
            let held = retained_writes(&server, workspace_id, RESOURCE).await;
            let folded = held.iter().any(|op| {
                op.author.as_bytes() == &member_key.verifying_key().to_bytes()
                    && op.kind == collab_types::OpKind::Compaction
                    && op.encryption.is_some()
            });
            if folded
                && held.len() < WRITES / 4
                && newcomer_reads(&member, &held).as_deref() == Some(newest.as_str())
            {
                break held;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(held.iter().all(holds_nothing_in_the_clear));
    drop(workspaces);
    server.shutdown().await;
}

/// Whether `op` is sealed, or holds nothing: the checkpoint a record is
/// born from.
fn holds_nothing_in_the_clear(op: &SignedOp) -> bool {
    op.encryption.is_some()
        || matches!(
            collab_types::DocumentMutation::decode(&op.payload),
            Ok(collab_types::DocumentMutation::Snapshot(bytes))
                if automerge::AutoCommit::load(&bytes).unwrap().get_heads().is_empty()
        )
}

/// Copy what `from` holds into `to`, keeping files `to` has already.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else if !target.exists() {
            // A file removed since it was listed is not part of this state.
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
}

/// The node is stopped hard a moment after it took in members' sealed
/// writes, with no governance change since. The workspace as it opens from disk,
/// before anything is replayed, already holds them: replaying the server's
/// copy at a start may not bring a write back (the app surface it is
/// decided against is not there yet), and a later write building on it
/// would leave the record with a gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_writes_the_node_took_in_survive_a_hard_stop() {
    let Lounge {
        _dir,
        server,
        workspaces,
        workspace,
        member,
        ..
    } = lounge().await;
    for line in ["graffiti", "more graffiti"] {
        set_title(&member, &workspaces, &workspace, line).await;
    }
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("more graffiti")
    );
    let path = workspaces
        .workspace(&workspace)
        .await
        .unwrap()
        .lock()
        .await
        .path
        .clone();
    // The next start runs on the same device, with the keys it took the writes in under.
    let device = workspaces.device_keys();

    // What a hard stop leaves on disk, copied (the workspace file before the
    // files it names) and opened as the next start opens it.
    let deadline = std::time::Instant::now() + super::super::KEEP_FOLLOWED * 30;
    loop {
        let stopped = tempfile::tempdir().unwrap();
        let copy = stopped.path().join(path.file_name().unwrap());
        std::fs::copy(&path, &copy).unwrap();
        copy_dir(path.parent().unwrap(), stopped.path());
        let held = match GovernedWorkspace::open(copy, device.as_ref()) {
            Ok(reopened) => reopened.readable_document(APP, RESOURCE).await.unwrap(),
            // Copied while the workspace wrote its next state.
            Err(_) => None,
        };
        if held.as_deref().and_then(title).as_deref() == Some("more graffiti") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the members' writes never reached the workspace's own file"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    drop(workspaces);
    server.shutdown().await;
}

/// App surfaces as a server has them while it fetches one from the registry:
/// none until the fetch lands, then one that admits a sealed write.
struct LateSurface(std::sync::Arc<std::sync::atomic::AtomicBool>);

struct AnySurface;

impl collab_types::ReceiptSurface for AnySurface {
    fn kind(&self) -> &'static str {
        ""
    }
    fn verb(&self, _: &str) -> Option<(tilo_action_policy::ActionKind, Option<String>)> {
        None
    }
    fn editable(&self, _: &[tilo_actions::PathStep]) -> bool {
        false
    }
    fn footprint(&self, _: &collab_types::ClaimedAction) -> Vec<collab_types::UnitPath> {
        Vec::new()
    }
}

impl collab_types::SurfaceSource for LateSurface {
    fn surface(
        &self,
        _: &collab_types::WrittenDocument<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<std::sync::Arc<dyn collab_types::ReceiptSurface>> {
        self.0.load(std::sync::atomic::Ordering::SeqCst).then(|| {
            std::sync::Arc::new(AnySurface) as std::sync::Arc<dyn collab_types::ReceiptSurface>
        })
    }
}

/// Members write while the app surface their writes name is still being
/// fetched, as after a restart: the governor holds the writes, more than its
/// held queue keeps, and lets the oldest go. Once the surface arrives the
/// governor replays what the server retains and takes in every write, those
/// it let go included, without waiting for another write to land.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_held_for_a_late_surface_are_all_taken_in_once_it_arrives() {
    const WRITES: usize = tilo_peer_state::MAX_HELD_INPUTS + 16;
    let Lounge {
        _dir,
        server,
        workspaces,
        workspace,
        workspace_id,
        member_key,
        member_cert,
        mut member,
        ..
    } = lounge().await;
    // The member's page names a surface; the governor has not got it yet.
    member.surface = Some([7; 32]);
    let here = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    workspaces
        .workspace(&workspace)
        .await
        .unwrap()
        .lock()
        .await
        .peer()
        .set_surfaces(collab_types::Surfaces::from_source(std::sync::Arc::new(
            LateSurface(std::sync::Arc::clone(&here)),
        )));
    let session = Box::pin(member_session(
        &server,
        &member_key,
        &member_cert,
        workspace_id,
        0,
    ))
    .await;
    let mut followed = workspaces.document_writes();
    for index in 0..WRITES {
        let ((), message) = member.seal(&workspace, |doc| {
            doc.put(ROOT, "title", format!("line {index}")).unwrap();
            doc.commit_edit("fixture", "title", &member.person());
        });
        session.send(message);
    }
    tilo_test_env::within("the governor follows every write", async {
        let mut seen = 0;
        while seen < WRITES {
            if followed.recv().await.unwrap().record == RESOURCE {
                seen += 1;
            }
        }
    })
    .await;
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("Welcome"),
        "every write waits for the surface"
    );

    here.store(true, std::sync::atomic::Ordering::SeqCst);
    crate::surfaces::arrived();
    let newest = format!("line {}", WRITES - 1);
    follows(&workspaces, &workspace, &newest).await;
    drop(workspaces);
    server.shutdown().await;
}

/// A chat kept as a chain of parts starts each part with a member's sealed
/// write, under keys the node takes part in. The node takes in every part as it
/// starts, however many there are, and reads each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_follows_every_part_its_members_start_sealed() {
    let Lounge {
        _dir,
        server,
        workspaces,
        workspace,
        member,
        ..
    } = lounge().await;
    for index in 0..40 {
        let record = neighbour_wire::document_part(RESOURCE, index);
        let part = member.part(&workspaces, &workspace, &record).await;
        for line in 0..3 {
            set_title(
                &part,
                &workspaces,
                &workspace,
                &format!("part {index} line {line}"),
            )
            .await;
        }
        let held = workspaces
            .resource_document(&workspace, APP, &record)
            .await
            .unwrap();
        assert_eq!(
            held.as_deref().and_then(title).as_deref(),
            Some(format!("part {index} line 2").as_str()),
            "the node holds part {index}"
        );
    }
    drop(workspaces);
    server.shutdown().await;
}

/// What the server retains of `record`, once it retains a write `wanted`.
async fn retained_once(
    server: &crate::workspaces::fixtures::TestHost,
    workspace_id: WorkspaceId,
    record: &str,
    wanted: impl Fn(&SignedOp) -> bool + Send + Sync,
) -> Vec<SignedOp> {
    tilo_test_env::within("the server retains the record", async {
        loop {
            let held = retained_writes(server, workspace_id, record).await;
            if held.iter().any(&wanted) {
                break held;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
}

/// A node that takes part in its workspace's channels opens a record's channel
/// before the record's first write: nothing of the record is ever retained
/// in the clear, and the workspace reads it through the channel's keys.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_of_a_sealed_workspace_is_sealed_from_its_first_write() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    workspaces
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let held = retained_once(&server, workspace_id, RESOURCE, |op| {
        op.encryption.is_some()
    })
    .await;
    assert!(held.iter().all(holds_nothing_in_the_clear));
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("Welcome")
    );
    drop(workspaces);
    server.shutdown().await;
}

/// A note the workspace's governor (Ada) wrote before someone new was admitted
/// reaches the newcomer. Grace is admitted through the workspace's entry after
/// Ada wrote the note twice; her device subscribes to the workspace as a device
/// does, and the replay it is sent holds Ada's writes of the note, which
/// open to the note with both versions in its history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newcomer_is_replayed_a_note_written_before_they_were_admitted() {
    use super::super::tests::{entry_join, guest_device};
    const NOTE: &str = "tab:0000000000000001";
    let dir = tempfile::tempdir().unwrap();
    let (key, ada) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host(dir.path(), &key, &ada).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    for version in ["first", "second"] {
        workspaces
            .author_workspace_document(&workspace, &ada, (APP, NOTE), |doc| {
                doc.put(ROOT, "title", version).map_err(|e| e.to_string())
            })
            .await
            .unwrap();
    }
    retained_once(&server, workspace_id, NOTE, |_| true).await;

    let (grace_key, grace) = guest_device();
    entry_join(&workspaces, &ada, &workspace, (&grace_key, &grace), 21).await;

    // Grace's device reads the workspace over its link, as a device does: the
    // history it is replayed holds Ada's writes of the note, which open to the
    // note with both versions in its history.
    let grace_member =
        SealingMember::join(&workspaces, &workspace, (APP, NOTE), grace_key.clone()).await;
    let (_link, mut heard, pairing) = linked(&workspaces, workspace_id, grace_key, 0).await;
    let (held, mut read) = tilo_test_env::within("Grace's link holds the note", async {
        let mut held = Vec::new();
        let mut read = automerge::AutoCommit::new();
        loop {
            let frame = heard.recv().await.unwrap();
            let NeighbourMsg::AppDocument {
                document_id,
                envelope,
                ..
            } = &frame.message
            else {
                continue;
            };
            if document_id != NOTE {
                continue;
            }
            held.push(envelope.clone());
            grace_member.open_into(envelope, &mut read);
            if title(&read.save()).as_deref() == Some("second") {
                return (held, read);
            }
        }
    })
    .await;
    assert!(
        !held.is_empty(),
        "Grace's link holds no write of the note Ada wrote before she was admitted"
    );
    assert!(
        held.iter()
            .all(|op| op.author.as_bytes() == &ada.device_public_key),
        "every write of the note Grace is sent is Ada's"
    );
    assert_eq!(title(&read.save()).as_deref(), Some("second"));
    let versions: std::collections::BTreeSet<String> = read
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .filter_map(|hash| title(&read.fork_at(&[hash]).ok()?.save()))
        .collect();
    assert!(
        versions.contains("first") && versions.contains("second"),
        "the note Grace reads keeps both of Ada's versions: {versions:?}"
    );
    pairing.abort();
    drop(workspaces);
    server.shutdown().await;
}

/// The node asks to be let into a channel a member opened, and the member is
/// linked to it directly, as a member device's runtime is: the member hears
/// the node's request, so it can admit the node. Without it the node takes
/// every member's request in and is never let into a channel itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_linked_directly_hears_the_nodes_channel_governance() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (member_key, _) = member_device(&workspaces, &created.workspace, (&key, &cert), 3).await;

    let host_link = workspaces
        .direct()
        .link(created.workspace_id)
        .expect("the node serves the workspace over a direct link");
    let member = tilo_peer_state::direct::DirectSync::new(host_link.read(Clone::clone), member_key);
    let mut heard = member.heard();
    let mut caught_up = member.caught_up();
    let linked = member.clone();
    let pairing = tokio::spawn(async move {
        tilo_peer_state::direct::DirectSync::pair(&linked, &host_link).await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        caught_up.wait_for(|caught_up| *caught_up),
    )
    .await
    .expect("the member catches up with the node")
    .unwrap();

    let channel = collab_types::WorkspaceId::parse(&created.workspace).unwrap();
    let request =
        collab_types::MlsAccessRequestV1::signed(channel, vec![1, 2, 3], 1, &key).unwrap();
    workspaces
        .publish_governance(
            &created.workspace,
            vec![neighbour_wire::NeighbourMsg::MlsAccessRequest {
                workspace: created.workspace.clone(),
                request: Box::new(request.clone()),
                delivery: 1,
            }],
        )
        .await
        .unwrap();

    let arrived = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let frame = heard.recv().await.unwrap();
            if let neighbour_wire::NeighbourMsg::MlsAccessRequest { request: held, .. } =
                frame.message
            {
                break *held;
            }
        }
    })
    .await
    .expect("the member hears the node's access request");
    assert_eq!(arrived, request);
    assert_eq!(*arrived.device.as_bytes(), cert.device_public_key);

    pairing.abort();
    drop(workspaces);
    server.shutdown().await;
}

/// A record whose sealed writes do not all merge is named with why, and the
/// workspace's other records are read all the same: what a reader that
/// places content by what records name needs from a workspace with one
/// broken record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_that_does_not_read_whole_leaves_the_others_read() {
    let lounge = Box::new(lounge().await);
    let (workspaces, workspace) = (&lounge.workspaces, lounge.workspace.as_str());
    workspaces
        .author_workspace_document(workspace, &lounge.governor, (APP, "other"), |doc| {
            doc.put(ROOT, "title", "Other").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    // A write the node never gets: the member's next one waits on it.
    let _never_sent = lounge.member.seal(workspace, |document| {
        document.put(ROOT, "title", "Lost").unwrap();
        document.commit_edit("fixture", "title", &lounge.member.person());
    });
    set_title(&lounge.member, workspaces, workspace, "After").await;

    let read = workspaces.whole_documents(workspace, None).await.unwrap();
    assert!(!read.is_whole());
    let unread: Vec<_> = read.unread.iter().map(|(route, _)| route.clone()).collect();
    assert_eq!(unread, [(APP.to_owned(), RESOURCE.to_owned())]);
    assert!(
        read.unread[0].1.contains("do not merge"),
        "why it is unread: {}",
        read.unread[0].1
    );
    assert!(
        read.documents
            .iter()
            .any(|(route, _)| *route == (APP.to_owned(), "other".to_owned())),
        "the other record is read"
    );
    lounge.server.shutdown().await;
}

/// A device linked to the workspace directly, as a desktop's runtime is,
/// hearing what the link brings it.
pub(super) struct Peer {
    pub(super) key: ed25519_dalek::SigningKey,
    pub(super) link: tilo_peer_state::direct::DirectSync,
    pub(super) heard: tilo_peer_state::direct::Reader,
    pub(super) pairing: tokio::task::JoinHandle<Result<(), String>>,
}

impl Peer {
    /// Another device of the lounge's person, admitted and linked to the node, caught up.
    pub(super) async fn link(lounge: &Lounge, seed: u8) -> Self {
        Self::reading(lounge, seed, 0).await
    }

    /// [`Self::link`] with room for only `capacity` frames heard and not read
    /// yet; `0` keeps the link's own.
    async fn reading(lounge: &Lounge, seed: u8, capacity: usize) -> Self {
        let (key, _) = member_device(
            &lounge.workspaces,
            &lounge.workspace,
            (&lounge.governor_key, &lounge.governor),
            seed,
        )
        .await;
        Self::with_key(lounge, key, capacity).await
    }

    /// The device whose key is `key`, already admitted, linked to the
    /// workspace directly and caught up: what a member's runtime is.
    async fn with_key(lounge: &Lounge, key: ed25519_dalek::SigningKey, capacity: usize) -> Self {
        let (link, heard, pairing) = linked(
            &lounge.workspaces,
            lounge.workspace_id,
            key.clone(),
            capacity,
        )
        .await;
        Self {
            key,
            link,
            heard,
            pairing,
        }
    }
    /// The sequence numbers of `author`'s writes of [`RESOURCE`] this device hears live, in order,
    /// up to and including `last`: each write the device took in, as many times as it arrived.
    async fn hears(&mut self, author: [u8; 32], last: u64) -> Vec<u64> {
        let heard = &mut self.heard;
        tilo_test_env::within("the device hears the writes", async {
            let mut seqs = Vec::new();
            loop {
                let frame = heard.recv().await.unwrap();
                if let Some(seq) = write_of(&frame.message, author) {
                    assert!(
                        frame.report.is_ok(),
                        "the device takes the write in: {:?}",
                        frame.report.err()
                    );
                    seqs.push(seq);
                    if seq == last {
                        return seqs;
                    }
                }
            }
        })
        .await
    }
}

/// `key`'s device, already admitted, linked to `workspace_id`'s link and caught
/// up: the link it reads the workspace over, its reader, and the pairing task.
async fn linked(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
    key: ed25519_dalek::SigningKey,
    capacity: usize,
) -> (
    tilo_peer_state::direct::DirectSync,
    tilo_peer_state::direct::Reader,
    tokio::task::JoinHandle<Result<(), String>>,
) {
    let host = workspaces
        .direct()
        .link(workspace_id)
        .expect("the node serves the workspace over a direct link");
    let mut link = tilo_peer_state::direct::DirectSync::new(host.read(Clone::clone), key);
    if capacity > 0 {
        link = link.with_heard_capacity(capacity);
    }
    let heard = link.reader();
    let mut caught_up = link.caught_up();
    let paired = link.clone();
    let pairing =
        tokio::spawn(
            async move { tilo_peer_state::direct::DirectSync::pair(&paired, &host).await },
        );
    tilo_test_env::within("the device catches up with the node", async {
        caught_up.wait_for(|caught_up| *caught_up).await.unwrap();
    })
    .await;
    (link, heard, pairing)
}

/// The sequence number of `message`, when it is `author`'s write of [`RESOURCE`].
fn write_of(message: &NeighbourMsg, author: [u8; 32]) -> Option<u64> {
    match message {
        NeighbourMsg::AppDocument {
            document_id,
            envelope,
            ..
        } if document_id == RESOURCE && envelope.author.as_bytes() == &author => Some(envelope.seq),
        _ => None,
    }
}

/// A write from another device over the workspace's link wakes whoever follows
/// the workspace's records through [`Workspaces::document_writes`], once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_from_another_device_over_the_link_wakes_the_write_watch() {
    let lounge = lounge().await;
    let mut followed = lounge.workspaces.document_writes();
    let writer = Peer::link(&lounge, 5).await;
    let sealing = SealingMember::join(
        &lounge.workspaces,
        &lounge.workspace,
        (APP, RESOURCE),
        writer.key.clone(),
    )
    .await;
    let ((), message) = sealing.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "from a device").unwrap();
        doc.commit_edit("fixture", "title", &sealing.person());
    });
    writer.link.send(message).unwrap();
    let write = tilo_test_env::within("the write wakes the workspace's followers", async {
        loop {
            let write = followed.recv().await.unwrap();
            if write.app == APP && write.record == RESOURCE {
                return write;
            }
        }
    })
    .await;
    assert_eq!(
        write.workspace, lounge.workspace,
        "the notice names the workspace the write changed"
    );
    follows(&lounge.workspaces, &lounge.workspace, "from a device").await;
    // One write raises one notice: nothing already carried raises another.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        followed.try_recv().is_err(),
        "one write wakes a follower once"
    );
    writer.pairing.abort();
    let Lounge {
        server, workspaces, ..
    } = lounge;
    drop(workspaces);
    server.shutdown().await;
}

/// A member's write over its link wakes whoever follows the workspace's records
/// once, not once for each admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_write_wakes_the_write_watch_once() {
    let (lounge, session) = lounge_with_link().await;
    let mut followed = lounge.workspaces.document_writes();
    seal_and_send_title(&lounge.member, &session, &lounge.workspace, "once");
    tilo_test_env::within("the write wakes the workspace's followers", async {
        loop {
            let write = followed.recv().await.unwrap();
            if write.app == APP && write.record == RESOURCE {
                return;
            }
        }
    })
    .await;
    follows(&lounge.workspaces, &lounge.workspace, "once").await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        followed.try_recv().is_err(),
        "one write wakes a follower once"
    );
    drop(session);
    let Lounge {
        server, workspaces, ..
    } = lounge;
    drop(workspaces);
    server.shutdown().await;
}

/// A member's write reaches every other member once over the link it is sent on, and never
/// comes back to its own: each linked device hears each of the writer's writes once, live, and
/// the writer hears none of its own, all sealed as their authors wrote them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: both directions, the echo check and the cleanup"
)]
async fn a_write_reaches_every_member_once() {
    const WRITES: u64 = 6;
    let (lounge, mut writer) = lounge_with_link().await;
    let mut peers = [Peer::link(&lounge, 5).await, Peer::link(&lounge, 6).await];
    let writer_author = lounge.member_key.verifying_key().to_bytes();

    let started = std::time::Instant::now();
    for line in 0..WRITES {
        seal_and_send_title(
            &lounge.member,
            &writer,
            &lounge.workspace,
            &format!("from the writer {line}"),
        );
    }
    for peer in &mut peers {
        assert_eq!(
            peer.hears(writer_author, WRITES).await,
            (1..=WRITES).collect::<Vec<_>>(),
            "a linked device hears each of the writer's writes once, live"
        );
    }
    eprintln!(
        "{WRITES} writes of one device reached two linked devices in {:?}",
        started.elapsed()
    );

    // Another device of the workspace writes too, on its own link: what it writes reaches the
    // first device and the writer's, once, and the writer hears no echo of the other's nor of
    // any of its own.
    let [other, reader] = &mut peers;
    let sealing = SealingMember::join(
        &lounge.workspaces,
        &lounge.workspace,
        (APP, RESOURCE),
        other.key.clone(),
    )
    .await;
    let other_author = other.key.verifying_key().to_bytes();
    let started = std::time::Instant::now();
    for line in 0..WRITES {
        let ((), message) = sealing.seal(&lounge.workspace, |doc| {
            doc.put(ROOT, "title", format!("from a device {line}"))
                .unwrap();
            doc.commit_edit("fixture", "title", &sealing.person());
        });
        assert!(
            matches!(&message, NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some()),
            "the device writes sealed"
        );
        other.link.send(message).unwrap();
    }
    assert_eq!(
        reader.hears(other_author, WRITES).await,
        (1..=WRITES).collect::<Vec<_>>(),
        "the other linked device hears each of the device's writes once"
    );
    let heard = &mut writer.heard;
    let echoed = tilo_test_env::within("the writer hears the other device's writes", async {
        let mut echoed = Vec::new();
        loop {
            let frame = heard.recv().await.unwrap();
            if write_of(&frame.message, other_author) == Some(WRITES) {
                return echoed;
            }
            if let Some(seq) = write_of(&frame.message, writer_author) {
                echoed.push(("its own", seq, frame.retained));
            }
            if let Some(seq) = write_of(&frame.message, other_author) {
                echoed.push(("the other device's", seq, frame.retained));
            }
        }
    })
    .await;
    assert!(
        echoed.is_empty(),
        "no write came back to its author or reached the writer twice: {echoed:?}"
    );
    eprintln!(
        "{WRITES} writes of a linked device reached the other device and the writer's link in {:?}",
        started.elapsed()
    );
    for peer in peers {
        peer.pairing.abort();
    }
    drop(writer);
    let Lounge {
        server, workspaces, ..
    } = lounge;
    drop(workspaces);
    server.shutdown().await;
}

/// A device whose reader of the workspace's link falls behind, and a host whose
/// reader of it does, still read every write once after catching up from the
/// workspace's history: the device each of the writer's writes, the host the
/// device's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_that_fell_behind_read_every_write_once() {
    const WRITES: u64 = 6;
    let (lounge, writer) = lounge_reading_with_link(2).await;
    let mut peer = Peer::reading(&lounge, 5, 2).await;
    let writer_author = lounge.member_key.verifying_key().to_bytes();

    // The writer writes while the device reads nothing: its link has room for two.
    let started = std::time::Instant::now();
    for line in 0..WRITES {
        seal_and_send_title(
            &lounge.member,
            &writer,
            &lounge.workspace,
            &format!("from the writer {line}"),
        );
    }
    // Every write reached the device's link before the device reads one.
    let mut landed = peer.link.changes();
    tilo_test_env::within("the writer's writes reach the device", async {
        landed
            .wait_for(|_| {
                peer.link.cursors().iter().any(|cursor| {
                    cursor.lineage.author == writer_author && cursor.after_seq == WRITES
                })
            })
            .await
            .unwrap();
    })
    .await;
    let caught_up = std::time::Instant::now();
    assert_eq!(
        peer.hears(writer_author, WRITES).await,
        (1..=WRITES).collect::<Vec<_>>(),
        "a device that fell behind reads each of the writer's writes once"
    );
    eprintln!(
        "a device that fell behind {WRITES} writes caught up in {:?} ({:?} after the first write)",
        caught_up.elapsed(),
        started.elapsed()
    );

    // The host's reader of the link stalls while the device writes: it cannot
    // take the workspace while the test holds it.
    let sealing = SealingMember::join(
        &lounge.workspaces,
        &lounge.workspace,
        (APP, RESOURCE),
        peer.key.clone(),
    )
    .await;
    let peer_author = peer.key.verifying_key().to_bytes();
    let state = lounge
        .workspaces
        .workspace(&lounge.workspace)
        .await
        .unwrap();
    let host = lounge
        .workspaces
        .direct()
        .link(lounge.workspace_id)
        .expect("the node serves the workspace over a direct link");
    let mut landed = host.changes();
    let held = state.lock().await;
    for line in 0..WRITES {
        let ((), message) = sealing.seal(&lounge.workspace, |doc| {
            doc.put(ROOT, "title", format!("from a device {line}"))
                .unwrap();
            doc.commit_edit("fixture", "title", &sealing.person());
        });
        peer.link.send(message).unwrap();
    }
    tilo_test_env::within("the device's writes reach the host's link", async {
        landed
            .wait_for(|_| {
                host.cursors().iter().any(|cursor| {
                    cursor.lineage.author == peer_author && cursor.after_seq == WRITES
                })
            })
            .await
            .unwrap();
    })
    .await;
    let released = std::time::Instant::now();
    drop(held);
    // The host's reader takes the writes in once the workspace is free: what
    // the link admitted is in the workspace's copy, nothing lost to the wait.
    let newest = format!("from a device {}", WRITES - 1);
    tilo_test_env::within("the host's reader catches up", async {
        while readable(&lounge.workspaces, &lounge.workspace)
            .await
            .as_deref()
            != Some(newest.as_str())
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    eprintln!(
        "a host reader that fell behind {WRITES} writes caught up in {:?}",
        released.elapsed()
    );
    peer.pairing.abort();
    lounge.server.shutdown().await;
}

/// A device wrote parts of a chained record over the workspace's link, and the
/// host wrote a record and a part of its own, before the workspace's store kept
/// any of it: the store keeps them, the link's history does not. The store
/// replays a joining device only a chain's head and sends an older part once
/// asked; a desktop decides on its link's replay alone, and a record it holds
/// nothing of after that replay is one its app makes anew. The server's start
/// hands the link what it keeps before it serves anyone
/// ([`Workspaces::recall`]): a device that links holds every one of them once it
/// caught up, the parts behind the head included, and reads them rather than
/// making them again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_linking_later_holds_every_part_written_before_the_link_kept_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    // The host after the restart holds the channel keys the one before it took part with.
    let keys = std::sync::Arc::<crate::workspaces::fixtures::TestGroupKeys>::default();
    let server = crate::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        std::sync::Arc::clone(&keys),
    )
    .await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    let (member_key, member_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    workspaces
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member =
        SealingMember::join(&workspaces, &workspace, (APP, RESOURCE), member_key.clone()).await;
    let writer = Box::pin(member_session(
        &server,
        &member_key,
        &member_cert,
        workspace_id,
        0,
    ))
    .await;
    // The writer's parts: two behind the chain's head, and the head.
    let mut written = Vec::new();
    for index in [0, 1, 3] {
        let record = neighbour_wire::document_part(RESOURCE, index);
        let part = member.part(&workspaces, &workspace, &record).await;
        let ((), message) = part.seal(&workspace, |doc| {
            doc.put(ROOT, "title", format!("part {index}")).unwrap();
            doc.commit_edit("fixture", "title", &part.person());
        });
        writer.send(message);
        written.push(record);
    }
    // The host's own: a part behind the head, as a host seeds or repairs one.
    let host_part = neighbour_wire::document_part(RESOURCE, 2);
    workspaces
        .author_workspace_document(&workspace, &cert, (APP, &host_part), |doc| {
            doc.put(ROOT, "title", "the host's part")
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    written.extend([host_part, RESOURCE.to_owned()]);
    // The server keeps a write before the governor takes it in, and a start
    // replays what the server keeps only into the records the governor holds:
    // both have to be there before this host stops, or the restart races the
    // write it replays.
    tilo_test_env::within("the store and the host keep the writer's parts", async {
        for record in written.iter().filter(|record| record.as_str() != RESOURCE) {
            while retained_writes(&server, workspace_id, record)
                .await
                .is_empty()
                || !workspaces.records(&workspace).await.is_some_and(|records| {
                    records
                        .iter()
                        .any(|(app, held)| app.as_str() == APP && held.as_str() == record.as_str())
                })
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    })
    .await;
    drop(writer);
    drop(workspaces);
    server.shutdown().await;

    // The link's history as it stood before the store carried the device's writes to it.
    std::fs::remove_dir_all(dir.path().join("store")).unwrap();
    let server =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    let lounge = Lounge {
        _dir: dir,
        server,
        workspaces,
        workspace,
        workspace_id,
        governor: cert,
        governor_key: key,
        member_key,
        member_cert,
        member,
    };
    let peer = Peer::link(&lounge, 5).await;
    for record in &written {
        assert!(
            peer.link.read(|peer| peer.document(APP, record).is_some()),
            "a device that caught up holds {record}: its app reads it, and does not make it again"
        );
    }
    drop(peer);
    let Lounge {
        workspaces, server, ..
    } = lounge;
    drop(workspaces);
    server.shutdown().await;
}

/// A desktop wrote a part over the workspace's link that the server's history
/// lacked: the store kept it, and a device that links after a restart reads it
/// rather than making it anew.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_reads_a_part_only_the_link_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let keys = std::sync::Arc::<crate::workspaces::fixtures::TestGroupKeys>::default();
    let server = crate::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        std::sync::Arc::clone(&keys),
    )
    .await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    let (member_key, _member_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    workspaces
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member =
        SealingMember::join(&workspaces, &workspace, (APP, RESOURCE), member_key.clone()).await;
    // A desktop's part behind the chain's head, kept by the link alone.
    let (device_key, _) = member_device(&workspaces, &workspace, (&key, &cert), 5).await;
    let older = neighbour_wire::document_part(RESOURCE, 0);
    let device = SealingMember::join(&workspaces, &workspace, (APP, RESOURCE), device_key)
        .await
        .part(&workspaces, &workspace, &older)
        .await;
    let ((), message) = device.founds(&workspace, |doc| {
        doc.put(ROOT, "title", "the device's older part").unwrap();
        doc.commit_edit("fixture", "title", &device.person());
    });
    let frame = tilo_peer_state::store::Frame::of(&message).unwrap();
    workspaces.inner.store.retain(workspace_id, &frame).unwrap();
    drop(workspaces);
    server.shutdown().await;

    let server =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    // A device reads the workspace over its link, as a page's runtime does: it
    // holds the part the store kept, and reads it rather than making it again.
    let (link, _heard, pairing) = linked(&workspaces, workspace_id, member_key, 0).await;
    assert!(
        link.read(|held| held.document(APP, &older).is_some()),
        "a device that caught up holds the part the store kept"
    );
    pairing.abort();
    drop(member);
    drop(workspaces);
    server.shutdown().await;
}

/// A device keeps a member's write in its store, and the workspace's copy never follows it
/// because the device stops first: the write reaches the copy when the device starts again,
/// from the store alone, and a newcomer reads the record whole with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_store_holding_a_stuck_write_is_repaired_when_its_server_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    // The server after the restart holds the channel keys the one before it took part with.
    let keys = std::sync::Arc::<crate::workspaces::fixtures::TestGroupKeys>::default();
    let server = crate::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        std::sync::Arc::clone(&keys),
    )
    .await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    let (member_key, _member_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    workspaces
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member =
        SealingMember::join(&workspaces, &workspace, (APP, RESOURCE), member_key.clone()).await;
    let ((), stuck) = member.seal(&workspace, |doc| {
        doc.put(ROOT, "title", "from its store").unwrap();
        doc.commit_edit("fixture", "title", &member.person());
    });
    let NeighbourMsg::AppDocument { envelope, .. } = &stuck else {
        panic!("a document write")
    };
    let envelope = envelope.signature;
    let documents = server.workspace_documents();
    drop(workspaces);
    server.shutdown().await;
    // Kept at the device's store, while its copy is not running to follow it.
    documents
        .carry(
            workspace_id,
            member_key.verifying_key().to_bytes(),
            &crate::workspace_documents::HostCommit {
                body: postcard::to_stdvec(&stuck).unwrap(),
            },
        )
        .unwrap();

    // The device starts again on the same store: its copy reads the write, and a newcomer
    // opening what the store serves reads the record whole with it.
    let server =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces().clone();
    follows(&workspaces, &workspace, "from its store").await;
    let held = retained_writes(&server, workspace_id, RESOURCE).await;
    assert!(
        held.iter().any(|op| op.signature == envelope),
        "the store still keeps the write it gave the copy"
    );
    assert_eq!(
        newcomer_reads(&member, &held).as_deref(),
        Some("from its store"),
        "a newcomer reads the record whole with the write"
    );
    drop(workspaces);
    server.shutdown().await;
}

/// Who an agent's call is and what it reads are answered while a change holds
/// the workspace waiting on something slow, as one waits on the key service
/// settling a newcomer's access: no read waits for the workspace's lock. Every
/// call of an agent asks both, and each waited up to a minute while a person
/// joined.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_answer_while_a_change_holds_the_workspace() {
    use tilo_actions::remote::Caller;
    let Lounge {
        _dir,
        server,
        workspaces,
        workspace,
        workspace_id,
        member_cert,
        ..
    } = lounge().await;
    let caller = Caller::authenticated(member_cert.user_public_key, member_cert.device_public_key);
    let read = || async {
        let started = std::time::Instant::now();
        assert!(workspaces.admits(workspace_id, &caller).await);
        assert!(
            workspaces
                .workspace_policy(workspace_id, &caller)
                .await
                .is_some()
        );
        assert_eq!(
            readable(&workspaces, &workspace).await.as_deref(),
            Some("Welcome")
        );
        started.elapsed()
    };
    let mut normal = std::time::Duration::ZERO;
    for _ in 0..5 {
        normal = normal.max(read().await);
    }
    let governed = workspaces.held(workspace_id).await.unwrap();
    let held = governed.lock().await;
    let during = tokio::time::timeout(normal * 50, read())
        .await
        .expect("a read answers while a change holds the workspace");
    drop(held);
    assert!(
        during <= normal * 50,
        "{during:?} against {normal:?} unheld"
    );
    drop(governed);
    drop(workspaces);
    server.shutdown().await;
}

/// The governor's copy of members' writes is kept on disk without holding
/// the workspace: while the disk is slow to take it, the next write is taken
/// in and read, and the copy is written once the disk takes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_write_is_taken_in_while_the_disk_keeps_the_last() {
    let Lounge {
        _dir,
        server,
        workspaces,
        workspace,
        workspace_id,
        member,
        ..
    } = lounge().await;
    let governed = workspaces.held(workspace_id).await.unwrap();
    let files = governed.lock().await.parts.files();
    // The disk, slow: nothing is written until it is let go.
    let (busy, release) = (
        std::sync::Arc::new(std::sync::Barrier::new(2)),
        std::sync::Arc::new(std::sync::Barrier::new(2)),
    );
    let disk = std::thread::spawn({
        let (busy, release) = (
            std::sync::Arc::clone(&busy),
            std::sync::Arc::clone(&release),
        );
        move || {
            let _writing = files.busy();
            busy.wait();
            release.wait();
        }
    });
    busy.wait();
    set_title(&member, &workspaces, &workspace, "first").await;
    follows(&workspaces, &workspace, "first").await;
    // The copy holding it is named and on its way to the disk.
    tilo_test_env::within("the governor keeps its copy", async {
        loop {
            {
                let held = governed.lock().await;
                if !held.unkept && !held.keeping {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    set_title(&member, &workspaces, &workspace, "second").await;
    follows(&workspaces, &workspace, "second").await;
    release.wait();
    disk.join().unwrap();
    tilo_test_env::within("the kept copy reaches the disk", async {
        while governed.lock().await.parts.pending() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    drop(governed);
    drop(workspaces);
    server.shutdown().await;
}

/// A document can come to need keys of its own before anything was sealed
/// under them: a Deny on reading makes one, and the Mail workspace's retained
/// copies always need them. Its group does not exist yet, and the host serves
/// its members' reads with those keys, so asking for them opens the group
/// rather than refusing the record to everyone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_document_that_needs_keys_of_its_own_opens_its_group_for_a_read() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Demo").await.unwrap();
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.mail".into(),
        cluster_id: created.workspace,
        document: tilo_action_policy::roles::WORKSPACE_MAIL_COPIES_DOCUMENT.into(),
    };
    assert!(
        workspaces
            .state(&route.cluster_id)
            .await
            .expect("the host reads its workspace's governance")
            .requires_document_keys(&route.app_id, &route.document, tilo_clock::wall())
            .expect("the workspace decides the document's keys"),
        "the document needs keys of its own, and none was sealed under them"
    );
    let read = workspaces
        .own_route_keys(&route, false)
        .await
        .expect("the host takes part in the workspace's channels")
        .expect("a read opens the document's group");
    // The read after it finds the group the first opened, rather than a new one.
    let again = workspaces
        .own_route_keys(&route, false)
        .await
        .expect("the host takes part in the workspace's channels")
        .expect("the group the read opened answers again");
    assert!(read.same_write_key(&again));
    drop(workspaces);
    server.shutdown().await;
}
