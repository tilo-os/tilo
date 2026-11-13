#![expect(
    clippy::significant_drop_tightening,
    reason = "the host a test binds has to outlive every step it drives, and its shutdown takes it by value, so there is nothing left to drop early"
)]
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
use crate::workspaces::fixtures::test_host_taking_part;
use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use tilo_automerge::ApplyIncoming as _;
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

pub(super) const APP: &str = "so.tilo.notes";
const RECORD: &str = "welcome";

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
            document.commit_edit("fixture", "title");
        })
        .await;
}

async fn readable(workspaces: &Workspaces, workspace: &str) -> Option<String> {
    title(
        &workspaces
            .record_document(workspace, APP, RECORD)
            .await
            .unwrap()?,
    )
}

/// Wait until the node reads `wanted` as [`RECORD`]'s title: every write that got there is
/// followed.
async fn follows(workspaces: &Workspaces, workspace: &str, wanted: &str) {
    tilo_test_env::within("the node follows the record's writes", async {
        while readable(workspaces, workspace).await.as_deref() != Some(wanted) {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await;
}

/// Wait until the node's copy of [`RECORD`] holds `wanted` at `field`: a write that changes
/// no title is followed too, and a check made before it lands reads a copy without it.
async fn follows_field(workspaces: &Workspaces, workspace: &str, field: &str, wanted: &str) {
    tilo_test_env::within("the node follows the record's writes", async {
        loop {
            let held = workspaces
                .record_document(workspace, APP, RECORD)
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
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await;
}

/// A node governing a "Lounge" workspace, the workspace's record of [`APP`] it holds, and
/// a member device that writes it sealed under the keys the node takes part in.
pub(super) struct Lounge {
    pub(super) _dir: tempfile::TempDir,
    pub(super) relay: crate::workspaces::fixtures::TestHost,
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
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    let relay = test_host_taking_part(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    if capacity > 0 {
        workspaces.bound_link_readers(capacity);
    }
    let created = workspaces.create("Lounge").await.unwrap();
    let (member_key, member_cert) =
        member_device(&workspaces, &created.workspace, (&key, &cert), 3).await;
    workspaces
        .author_room_record(&created.workspace, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member = SealingMember::join(
        &workspaces,
        &created.workspace,
        (APP, RECORD),
        member_key.clone(),
    )
    .await;
    Lounge {
        _dir: dir,
        relay,
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

/// [`lounge`] with its member's own subscription to the node's host, as the member's phone
/// or laptop reaches it.
pub(super) async fn lounge_with_client() -> (Lounge, crate::RelayClient) {
    lounge_reading_with_client(0).await
}

/// [`lounge_with_client`] over [`lounge_reading`].
async fn lounge_reading_with_client(capacity: usize) -> (Lounge, crate::RelayClient) {
    // The host is heap-held from the moment it exists: a node's state is far larger than a
    // test's future wants to keep in its frame while the member's subscription is set up.
    let lounge = Box::new(lounge_reading(capacity).await);
    let client = Box::pin(subscribed_member(
        &lounge.relay,
        &lounge.member_key,
        lounge.member_cert.clone(),
        lounge.workspace_id,
    ))
    .await;
    (*lounge, client)
}

/// A node governs a workspace whose record its members write sealed. It reads
/// the record through the channel keys it holds as a member and resets it
/// with a write of its own, sealed like theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_reads_and_resets_a_record_its_members_seal() {
    let Lounge {
        _dir,
        relay,
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
        .author_room_record(&workspace, &governor, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .expect("the governor writes a sealed record sealed");
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("Welcome")
    );
    let own = governor_published(&workspaces, &workspace, RECORD, &governor)
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
    relay.shutdown().await;
}

/// A public workspace's chat takes thousands of writes within hours. Past three
/// quarters of a sealed history's bound the node compacts it into one sealed
/// checkpoint, so its copy goes on following the record, and reads it whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_compacts_a_sealed_history_before_it_fills() {
    let Lounge {
        _dir,
        relay,
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
    let due = state
        .lock()
        .await
        .peer()
        .publication()
        .sealed_records_to_compact();
    assert!(due.is_empty(), "the history was compacted: {due:?}");
    // Members hold every write it covers: the compaction stays with the
    // governor's copy and is never pushed to them.
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
    assert_eq!(published, 0, "a compaction is not published to members");
    drop((state, workspaces));
    relay.shutdown().await;
}

/// Every write of `record` the relay retains for `workspace_id`, in replay
/// order: what a newcomer is sent.
async fn retained_writes(
    relay: &crate::workspaces::fixtures::TestHost,
    workspace_id: WorkspaceId,
    record: &str,
) -> Vec<SignedOp> {
    relay
        .workspace_records()
        .retained(workspace_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(
            |retained| match postcard::from_bytes::<NeighbourMsg>(&retained.body) {
                Ok(NeighbourMsg::AppDocument {
                    record_id,
                    envelope,
                    ..
                }) if record_id == record => Some(*envelope),
                _ => None,
            },
        )
        .collect()
}

/// A member's own client to `relay`'s host, subscribed to `workspace_id`'s
/// workspace, as the member's phone or laptop reaches it.
pub(super) async fn subscribed_member(
    relay: &crate::workspaces::fixtures::TestHost,
    key: &ed25519_dalek::SigningKey,
    cert: DeviceCert,
    workspace_id: WorkspaceId,
) -> crate::RelayClient {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let client = crate::RelayClient::connect_to_pinned_host(
        endpoint,
        relay.node_addr(),
        cert,
        key.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(crate::Envelope::Subscribe { workspace_id })
        .unwrap();
    tilo_test_env::within("the member's subscription", async {
        while !matches!(client.recv().await, Some(crate::Envelope::Ready { .. })) {}
    })
    .await;
    client
}

/// Send `delta`, `member`'s write, to the host `client` reaches.
fn send_members_delta(
    client: &crate::RelayClient,
    workspace_id: WorkspaceId,
    member: &ed25519_dalek::SigningKey,
    delta: &crate::CoeditPayload,
) {
    client
        .send(crate::Envelope::Delta {
            workspace_id,
            author_device_public_key: member.verifying_key().to_bytes(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: delta.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap();
}

/// Send `message`, a sealed write of the member `key` signs, to the host `client` reaches,
/// as the member's own writes travel.
fn send_sealed(
    client: &crate::RelayClient,
    workspace_id: WorkspaceId,
    key: &ed25519_dalek::SigningKey,
    message: &NeighbourMsg,
) {
    let (channel, _) = neighbour_wire::retention(message, false).unwrap();
    send_members_delta(
        client,
        workspace_id,
        key,
        &crate::CoeditPayload::RetainedDelta {
            channel,
            body: postcard::to_stdvec(message).unwrap(),
        },
    );
}

/// `member` writes `line` as the record's title and sends it to the host
/// sealed, as the member's own writes travel.
pub(super) fn seal_and_send_title(
    member: &SealingMember,
    client: &crate::RelayClient,
    workspace: &str,
    workspace_id: WorkspaceId,
    key: &ed25519_dalek::SigningKey,
    line: &str,
) {
    let ((), message) = member.seal(workspace, |doc| {
        doc.put(ROOT, "title", line).unwrap();
        doc.commit_edit("fixture", "title");
    });
    send_sealed(client, workspace_id, key, &message);
}

/// A newcomer opening `held` in order: the record as a device joining now reads it.
fn newcomer_opens(member: &SealingMember, held: &[SignedOp]) -> automerge::AutoCommit {
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
            relay,
            workspaces,
            workspace,
            workspace_id,
            member_key,
            member,
            ..
        },
        client,
    ) = lounge_with_client().await;
    for index in 0..WRITES {
        seal_and_send_title(
            &member,
            &client,
            &workspace,
            workspace_id,
            &member_key,
            &format!("line {index}"),
        );
    }
    follows(&workspaces, &workspace, "line 399").await;
    // The member's engine folds the history it holds into one checkpoint
    // and publishes it in place of every write it covers.
    let [compaction]: [NeighbourMsg; 1] = member
        .compact(&workspaces, &workspace)
        .await
        .try_into()
        .unwrap();
    let (channel, _) = neighbour_wire::retention(&compaction, false).unwrap();
    send_members_delta(
        &client,
        workspace_id,
        &member_key,
        &crate::CoeditPayload::Checkpoint {
            channel,
            body: postcard::to_stdvec(&compaction).unwrap(),
        },
    );
    // What a newcomer is sent past the checkpoint travels as itself.
    for index in WRITES..WRITES + 3 {
        seal_and_send_title(
            &member,
            &client,
            &workspace,
            workspace_id,
            &member_key,
            &format!("line {index}"),
        );
    }
    // What a newcomer is sent, opened: the member's checkpoint, then the
    // writes after it.
    let newest = format!("line {}", WRITES + 2);
    let held = tilo_test_env::within("the node retains the member's checkpoint", async {
        loop {
            let held = retained_writes(&relay, workspace_id, RECORD).await;
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
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(held.iter().all(holds_nothing_in_the_clear));
    drop(workspaces);
    relay.shutdown().await;
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
/// before anything is replayed, already holds them: replaying the relay's
/// copy at a start may not bring a write back (the app surface it is
/// decided against is not there yet), and a later write building on it
/// would leave the record with a gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_writes_the_node_took_in_survive_a_hard_stop() {
    let Lounge {
        _dir,
        relay,
        workspaces,
        workspace,
        governor,
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
        let held = match GovernedWorkspace::open(copy, &governor, device.as_ref()) {
            Ok(reopened) => reopened.readable_document(APP, RECORD).await.unwrap(),
            // Copied while the workspace wrote its next state.
            Err(_) => None,
        };
        if held.as_deref().and_then(title).as_deref() == Some("more graffiti") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the members' writes never reached the room's own file"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    drop(workspaces);
    relay.shutdown().await;
}

/// App surfaces as a relay has them while it fetches one from the registry:
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
        _: &collab_types::Record<'_>,
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
/// governor replays what the relay retains and takes in every write, those
/// it let go included, without waiting for another write to land.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_held_for_a_late_surface_are_all_taken_in_once_it_arrives() {
    const WRITES: usize = tilo_peer_state::MAX_HELD_INPUTS + 16;
    let Lounge {
        _dir,
        relay,
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
    let client = subscribed_member(&relay, &member_key, member_cert, workspace_id).await;
    let mut followed = workspaces.record_writes();
    for index in 0..WRITES {
        let ((), message) = member.seal(&workspace, |doc| {
            doc.put(ROOT, "title", format!("line {index}")).unwrap();
            doc.commit_edit("fixture", "title");
        });
        let (channel, _) = neighbour_wire::retention(&message, false).unwrap();
        client
            .send(crate::Envelope::Delta {
                workspace_id,
                author_device_public_key: member_key.verifying_key().to_bytes(),
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta: crate::CoeditPayload::RetainedDelta {
                    channel,
                    body: postcard::to_stdvec(&message).unwrap(),
                }
                .encode()
                .unwrap(),
                signature: [0; 64],
            })
            .unwrap();
    }
    tilo_test_env::within("the governor follows every write", async {
        let mut seen = 0;
        while seen < WRITES {
            if followed.recv().await.unwrap().record == RECORD {
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
    relay.shutdown().await;
}

/// A chat kept as a chain of parts starts each part with a member's sealed
/// write, under keys the node takes part in. The node takes in every part as it
/// starts, however many there are, and reads each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_follows_every_part_its_members_start_sealed() {
    let Lounge {
        _dir,
        relay,
        workspaces,
        workspace,
        member,
        ..
    } = lounge().await;
    for index in 0..40 {
        let record = neighbour_wire::record_part(RECORD, index);
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
            .record_document(&workspace, APP, &record)
            .await
            .unwrap();
        assert_eq!(
            held.as_deref().and_then(title).as_deref(),
            Some(format!("part {index} line 2").as_str()),
            "the node holds part {index}"
        );
    }
    drop(workspaces);
    relay.shutdown().await;
}

/// Members of a hosted workspace seal every Chat write, so the node reads the
/// voice channel a member opens through the keys it holds: a member's
/// sealed write never leaves a workspace's voice channels without calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn a_member_opens_a_call_in_a_chat_its_members_seal() {
    const CHAT: &str = "so.tilo.chat";
    const LOBBY: &str = "lobby";
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    let relay = test_host_taking_part(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let (member_key, member_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    workspaces
        .author_room_record(&workspace, &cert, (CHAT, LOBBY), |doc| {
            let channels = doc
                .put_object(ROOT, "channels", automerge::ObjType::Map)
                .map_err(|e| e.to_string())?;
            let entry = doc
                .put_object(&channels, "voice", automerge::ObjType::Map)
                .map_err(|e| e.to_string())?;
            let voice = doc
                .put_object(&entry, "v", automerge::ObjType::Map)
                .map_err(|e| e.to_string())?;
            doc.put(&voice, "kind", "voice")
                .map_err(|e| e.to_string())?;
            doc.put(&voice, "workspace", "main")
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member = SealingMember::join(&workspaces, &workspace, (CHAT, LOBBY), member_key).await;
    member
        .writes(&workspaces, &workspace, |document| {
            document.put(ROOT, "topic", "hello").unwrap();
            document.commit_edit("fixture", "topic");
        })
        .await;
    let state = workspaces.workspace(&workspace).await.unwrap();
    assert!(
        state
            .lock()
            .await
            .peer()
            .publication()
            .is_sealed_record(CHAT, LOBBY),
        "the member's write is sealed"
    );
    drop(state);

    let open = tilo_relay_protocol::voice::CallOperation::Open {
        app_id: CHAT.into(),
        record_id: LOBBY.into(),
        channel_id: "voice".into(),
    };
    let call = workspaces
        .voice_call_as(&workspace, &member_cert, &open)
        .await
        .expect("a member opens the voice channel's call");
    assert!(call.active);
    let peer = tilo_relay_protocol::AuthenticatedPeer {
        cert: member_cert.clone(),
        user_public_key: member_cert.user_public_key,
        device_public_key: member_cert.device_public_key,
    };
    let (outbound, _inbox) = tokio::sync::mpsc::channel(4);
    let calls = workspaces
        .voice_calls(&workspace, &peer, outbound)
        .await
        .expect("the node still reads the room's calls");
    assert!(calls.iter().any(|listed| listed.state.call == call.call));
    drop(workspaces);
    relay.shutdown().await;
}

/// What the relay retains of `record`, once it retains a write `wanted`.
async fn retained_once(
    relay: &crate::workspaces::fixtures::TestHost,
    workspace_id: WorkspaceId,
    record: &str,
    wanted: impl Fn(&SignedOp) -> bool + Send + Sync,
) -> Vec<SignedOp> {
    tilo_test_env::within("the relay retains the record", async {
        loop {
            let held = retained_writes(relay, workspace_id, record).await;
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
async fn a_record_of_a_sealed_room_is_sealed_from_its_first_write() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    let relay = test_host_taking_part(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    workspaces
        .author_room_record(&workspace, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let held = retained_once(&relay, workspace_id, RECORD, |op| op.encryption.is_some()).await;
    assert!(held.iter().all(holds_nothing_in_the_clear));
    assert_eq!(
        readable(&workspaces, &workspace).await.as_deref(),
        Some("Welcome")
    );
    drop(workspaces);
    relay.shutdown().await;
}

/// A record its workspace wrote in the clear before the node took part in its
/// channel is sealed once its operator decided it: its channel is opened, the
/// relay retains one sealed snapshot in place of the writes in the clear,
/// and a member with the channel's keys reads every change of that history
/// from it, with its author and sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "TestHost::shutdown consumes the host after the assertions"
)]
async fn a_record_written_in_the_clear_is_sealed_with_its_whole_history() {
    const LEGACY: &str = "legacy";
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    let relay = test_host_taking_part(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    let (member_key, _) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    // Two authors' writes, as the workspace's host kept them in the clear before
    // it took part in the record's channel.
    let mut written = automerge::AutoCommit::new();
    written.put(ROOT, "title", "before").unwrap();
    written.commit_edit("fixture", "title");
    let mut other = written.fork();
    other.put(ROOT, "by", "another author").unwrap();
    other.commit_edit("fixture", "by");
    written.put(ROOT, "title", "the channel").unwrap();
    written.commit_edit("fixture", "title");
    written.merge_incoming(&mut other).unwrap();
    {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let mut governed = state.lock().await;
        let mut candidate = governed.peer().clone();
        let now = SystemTime::now();
        for bytes in [automerge::AutoCommit::new().save(), written.save()] {
            candidate
                .publication_mut()
                .publish_document(APP, LEGACY, bytes, &key, now)
                .unwrap();
        }
        let collaboration = governed.collaboration().clone();
        governed
            .commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
        workspaces.settle(&mut governed).await.unwrap();
    }
    retained_once(&relay, workspace_id, LEGACY, |op| {
        !holds_nothing_in_the_clear(op)
    })
    .await;

    // The host's operator decided to seal what its workspaces hold in the clear.
    workspaces.reseal_existing();
    relay.compactor().reseal_existing();
    workspaces.reseal_own_channels(relay.compactor()).await;

    let held = retained_writes(&relay, workspace_id, LEGACY).await;
    assert!(held.iter().all(|op| op.encryption.is_some()));
    workspaces.reseal_own_channels(relay.compactor()).await;
    assert_eq!(retained_writes(&relay, workspace_id, LEGACY).await, held);
    let member = SealingMember::join(&workspaces, &workspace, (APP, LEGACY), member_key).await;
    let mut read = automerge::AutoCommit::new();
    for op in &held {
        member.open_into(op, &mut read);
    }
    let changes =
        |doc: &mut automerge::AutoCommit| -> std::collections::HashSet<(automerge::ActorId, u64)> {
            doc.get_changes(&[])
                .iter()
                .map(|change| (change.actor_id().clone(), change.seq()))
                .collect()
        };
    assert!(changes(&mut written).is_subset(&changes(&mut read)));
    assert_eq!(title(&read.save()).as_deref(), Some("the channel"));
    drop(workspaces);
    relay.shutdown().await;
}

/// A note the workspace's governor (Ada) wrote before someone new was admitted
/// reaches the newcomer. Grace is admitted through the workspace's door after
/// Ada wrote the note twice; her device subscribes to the workspace as a device
/// does, and the replay it is sent holds Ada's writes of the note, which
/// open to the note with both versions in its history.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newcomer_is_replayed_a_note_written_before_they_were_admitted() {
    use super::super::tests::{door_join, guest_device};
    const NOTE: &str = "tab:0000000000000001";
    let dir = tempfile::tempdir().unwrap();
    let (key, ada) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    let relay = test_host_taking_part(dir.path(), &key, &ada).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    for version in ["first", "second"] {
        workspaces
            .author_room_record(&workspace, &ada, (APP, NOTE), |doc| {
                doc.put(ROOT, "title", version).map_err(|e| e.to_string())
            })
            .await
            .unwrap();
    }
    assert!(
        !retained_writes(&relay, workspace_id, NOTE).await.is_empty(),
        "the relay retains Ada's note before Grace is admitted"
    );

    let (grace_key, grace) = guest_device();
    door_join(&workspaces, &ada, &workspace, (&grace_key, &grace), 21).await;

    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&grace_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let client = crate::RelayClient::connect_to_pinned_host(
        endpoint,
        relay.node_addr(),
        grace.clone(),
        grace_key.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(crate::Envelope::Subscribe { workspace_id })
        .unwrap();
    let replayed = tilo_test_env::within("Grace's replay of the room", async {
        let mut replayed = Vec::new();
        loop {
            match client.recv().await {
                Some(crate::Envelope::Ready { .. }) => break replayed,
                Some(crate::Envelope::Delta { delta, .. }) => replayed.push(delta),
                Some(_) => {}
                None => panic!("the relay closed Grace's subscription before Ready"),
            }
        }
    })
    .await;
    let held: Vec<SignedOp> = replayed
        .iter()
        .filter_map(|delta| match crate::CoeditPayload::decode(delta)? {
            crate::CoeditPayload::Checkpoint { body, .. }
            | crate::CoeditPayload::RetainedDelta { body, .. } => {
                match postcard::from_bytes::<NeighbourMsg>(&body).ok()? {
                    NeighbourMsg::AppDocument {
                        record_id,
                        envelope,
                        ..
                    } if record_id == NOTE => Some(*envelope),
                    _ => None,
                }
            }
        })
        .collect();
    assert!(
        !held.is_empty(),
        "Grace's replay holds no write of the note Ada wrote before she was admitted \
         ({} frames replayed)",
        replayed.len()
    );
    assert!(
        held.iter()
            .all(|op| op.author.as_bytes() == &ada.device_public_key),
        "every write of the note Grace is sent is Ada's"
    );
    let grace_member = SealingMember::join(&workspaces, &workspace, (APP, NOTE), grace_key).await;
    let mut read = automerge::AutoCommit::new();
    for op in &held {
        grace_member.open_into(op, &mut read);
    }
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
    drop(workspaces);
    relay.shutdown().await;
}

/// Every retained file of `channel` in `root`, as (path, bytes).
fn retained_files(
    root: &std::path::Path,
    channel: &[u8; 32],
) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let v2 = root.join(crate::store::RETAINED_DIR).join("v2");
    let Ok(lineages) = std::fs::read_dir(&v2) else {
        return Vec::new();
    };
    let suffix = hex::encode(channel);
    let mut files = Vec::new();
    for lineage in lineages.filter_map(Result::ok) {
        if !lineage.file_name().to_string_lossy().ends_with(&suffix) {
            continue;
        }
        let Ok(name) = std::fs::read_to_string(lineage.path().join("CURRENT")) else {
            continue;
        };
        let generation = lineage.path().join(name.trim());
        let Ok(entries) = std::fs::read_dir(&generation) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "delta")
                && let Ok(bytes) = std::fs::read(&path)
            {
                files.push((path, bytes));
            }
        }
    }
    files
}

/// The store the node retains the workspace's channels under:
/// `<state>/workspaces/<id>`, beside the governed records' `rooms` directory.
async fn state_root(
    workspaces: &Workspaces,
    workspace: &str,
    workspace_id: WorkspaceId,
) -> std::path::PathBuf {
    workspaces
        .workspace(workspace)
        .await
        .expect("the workspace is governed")
        .lock()
        .await
        .path
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the workspace's state root")
        .join("workspaces")
        .join(workspace_id.to_string())
}

/// Take the record's founding checkpoint — the one in the lineage of the governor's
/// `governor` — and one change of it out of the store at `root`'s channel, as a volume that
/// lost them. Says how many files the channel retains after.
fn lose_the_records_base(
    root: &std::path::Path,
    channel: &[u8; 32],
    governor: &DeviceCert,
) -> usize {
    let files = retained_files(root, channel);
    let base = files
        .iter()
        .find(|(path, _)| {
            // v2/<author>-<channel>/<generation>/<frame>.delta
            let generation = path.parent().expect("the frame's generation");
            let lineage = generation.parent().expect("the generation's lineage");
            lineage.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with(&hex::encode(governor.device_public_key))
            })
        })
        .map(|(path, _)| path.clone())
        .expect("the record has the governor's base on disk");
    std::fs::remove_file(&base).unwrap();
    let change = files
        .iter()
        .find(|(path, _)| *path != base)
        .map(|(path, _)| path.clone())
        .expect("the record has changes on disk");
    std::fs::remove_file(&change).unwrap();
    let left = retained_files(root, channel).len();
    assert_eq!(
        left,
        files.len().saturating_sub(2),
        "the damage took the base and one change"
    );
    left
}

/// Every entry of the channel's archive under `root`, as its bytes: the history a repair
/// replaced, kept beside the replay.
fn archived_bodies(root: &std::path::Path) -> std::collections::BTreeSet<Vec<u8>> {
    let archive = root
        .join(crate::store::RETAINED_DIR)
        .join(crate::store::compaction::ARCHIVE_DIR);
    let mut bodies = std::collections::BTreeSet::new();
    let Ok(lineages) = std::fs::read_dir(&archive) else {
        return bodies;
    };
    for lineage in lineages.filter_map(Result::ok) {
        for generation in std::fs::read_dir(lineage.path())
            .unwrap()
            .filter_map(Result::ok)
        {
            for frame in std::fs::read_dir(generation.path())
                .unwrap()
                .filter_map(Result::ok)
            {
                bodies.insert(std::fs::read(frame.path()).unwrap());
            }
        }
    }
    bodies
}

/// Several authors write the record over time, past the point its history folds: the shape
/// a compaction folds, and the shape whose base can go missing. Says the newest title, and
/// that a newcomer reads the record whole before the damage.
async fn members_write_over_time(lounge: &Lounge, client: &crate::RelayClient) -> String {
    for author in ["first", "second"] {
        for index in 0..5 {
            seal_and_send_title(
                &lounge.member,
                client,
                &lounge.workspace,
                lounge.workspace_id,
                &lounge.member_key,
                &format!("{author} {index}"),
            );
        }
    }
    let newest = "second 4";
    follows(&lounge.workspaces, &lounge.workspace, newest).await;
    let held = retained_writes(&lounge.relay, lounge.workspace_id, RECORD).await;
    assert!(!held.is_empty(), "the record is retained");
    assert_eq!(
        title(&newcomer_opens(&lounge.member, &held).save()).as_deref(),
        Some(newest),
        "a newcomer reads the record whole before the damage"
    );
    newest.to_owned()
}

/// The governor founds the record again, and archives the history it replaces.
async fn refound_the_record(workspaces: &Workspaces, workspace: &str) {
    let refounded = workspaces
        .refound_record(workspace, APP, RECORD)
        .await
        .expect("the governor founds the record again");
    assert_eq!(
        (refounded.app.as_str(), refounded.record.as_str()),
        (APP, RECORD)
    );
    assert!(
        refounded.archived > 0,
        "the old history went to the archive"
    );
}

/// Nothing the repair replaced was deleted: every entry the channel retained before it is in
/// the archive, and the archive holds every entry the channel had.
fn assert_the_repair_kept_everything(
    root: &std::path::Path,
    kept_before: &std::collections::BTreeSet<Vec<u8>>,
    damaged: usize,
) {
    let archived = archived_bodies(root);
    assert!(!archived.is_empty(), "the archive holds the old entries");
    for kept in kept_before {
        assert!(
            archived.contains(kept),
            "an entry the channel retained before the repair is in neither the replay nor the archive"
        );
    }
    assert!(
        archived.len() >= damaged,
        "the archive holds every entry the channel had: {} of {damaged}",
        archived.len()
    );
}

/// A governed record whose retained history lost the base its deltas were
/// written over — eu1's Lounge — is founded again by the governor, and reads
/// to everyone from then on.
///
/// Several members write the record sealed over time, as the Lounge's shared
/// desktop was written for months. Then the damage is made the way the volume
/// held it: the founding checkpoint and one change are taken out of the
/// store. The governor's own reader reports the writes it is left out, and a
/// device joining now does not reach the record — the state this test asserts
/// before repairing it.
///
/// Then the governor refounds the record. Nothing is deleted: what the
/// channel retained is archived beside the replay. A device joining after
/// reads the whole record the governor reads; two members writing after
/// converge; and a second start naming the same record founds nothing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_whose_base_its_history_lost_is_founded_again() {
    let (lounge, client) = lounge_with_client().await;
    let newest = members_write_over_time(&lounge, &client).await;

    // The damage: this author's founding checkpoint and one change are gone
    // from the store, as eu1's history lost them.
    let channel = neighbour_wire::app_document_channel(APP, RECORD);
    let root = state_root(&lounge.workspaces, &lounge.workspace, lounge.workspace_id).await;
    let damaged = lose_the_records_base(&root, &channel, &lounge.governor);

    // Before the repair: a device joining now does not reach the record.
    let broken = retained_writes(&lounge.relay, lounge.workspace_id, RECORD).await;
    assert_ne!(
        title(&newcomer_opens(&lounge.member, &broken).save()).as_deref(),
        Some(newest.as_str()),
        "a joiner no longer converges: the base it needs is gone"
    );
    // Everything the channel retains at the moment of the repair.
    let kept_before: std::collections::BTreeSet<Vec<u8>> = retained_files(&root, &channel)
        .into_iter()
        .map(|(_, bytes)| bytes)
        .collect();

    // The repair, which keeps what it replaces in the archive beside the replay.
    refound_the_record(&lounge.workspaces, &lounge.workspace).await;
    assert_the_repair_kept_everything(&root, &kept_before, damaged);

    // A device joining now reads the whole record the governor reads.
    let whole = retained_writes(&lounge.relay, lounge.workspace_id, RECORD).await;
    assert_eq!(
        title(&newcomer_opens(&lounge.member, &whole).save()).as_deref(),
        Some(newest.as_str()),
        "a joiner reads the record whole again"
    );
    assert_eq!(
        readable(&lounge.workspaces, &lounge.workspace)
            .await
            .as_deref(),
        Some(newest.as_str())
    );

    // Two members writing after the repair converge on each other's writes.
    for line in ["after, first", "after, second"] {
        seal_and_send_title(
            &lounge.member,
            &client,
            &lounge.workspace,
            lounge.workspace_id,
            &lounge.member_key,
            line,
        );
    }
    follows(&lounge.workspaces, &lounge.workspace, "after, second").await;
    let wrote = retained_writes(&lounge.relay, lounge.workspace_id, RECORD).await;
    assert_eq!(
        newcomer_opens(&lounge.member, &wrote).get_heads(),
        lounge.member.document().get_heads(),
        "the retained record is what the record's author wrote"
    );

    // A device that still holds the old changes writes one the refound's
    // checkpoint cannot contain: written before the repair, delivered after
    // it, as a device that was away while the record was founded again.
    let ((), held_back) = lounge.member.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "written before the repair, sent after it")
            .unwrap();
        doc.commit_edit("fixture", "title");
    });
    send_sealed(&client, lounge.workspace_id, &lounge.member_key, &held_back);
    follows(
        &lounge.workspaces,
        &lounge.workspace,
        "written before the repair, sent after it",
    )
    .await;
    let late = retained_writes(&lounge.relay, lounge.workspace_id, RECORD).await;
    assert_eq!(
        title(&newcomer_opens(&lounge.member, &late).save()).as_deref(),
        Some("written before the repair, sent after it"),
        "a change written before the repair and sent after it still reaches a joiner"
    );

    // A second start with the same flag founds nothing again: the
    // operator's flag is kept in the host's state (`refounded.json`, beside
    // the workspace), so a record already refounded is not founded twice.
    // What the host would do if asked again is found again from the record
    // as it reads, which is not this test's subject.
    drop(lounge.workspaces);
    lounge.relay.shutdown().await;
}

/// A device that still holds changes the refound's checkpoint does not writes
/// after the repair: does a device joining later still read the record?
///
/// This is the case a repair cannot answer for itself. The governor founds the
/// record from what it can read; a device that was away holds more, including
/// changes the governor never got. Its next write is a change over a history
/// the new checkpoint does not carry, and a joiner reaches the record only if
/// that write is decided against — or held until — what the checkpoint holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_holds_changes_the_repair_lacks_still_reaches_a_joiner() {
    let (
        Lounge {
            _dir,
            relay,
            workspaces,
            workspace,
            workspace_id,
            governor,
            member_key,
            member,
            ..
        },
        client,
    ) = lounge_with_client().await;
    for index in 0..4 {
        seal_and_send_title(
            &member,
            &client,
            &workspace,
            workspace_id,
            &member_key,
            &format!("line {index}"),
        );
    }
    follows(&workspaces, &workspace, "line 3").await;

    // The member keeps writing while its connection is down: its page holds
    // these changes, the host has never seen them.
    let ((), offline_one) = member.seal(&workspace, |doc| {
        doc.put(ROOT, "offline", "one").unwrap();
    });
    let ((), offline_two) = member.seal(&workspace, |doc| {
        doc.put(ROOT, "offline", "two").unwrap();
    });

    // The damage, and the repair, while that device is away.
    let channel = neighbour_wire::app_document_channel(APP, RECORD);
    let root = state_root(&workspaces, &workspace, workspace_id).await;
    lose_the_records_base(&root, &channel, &governor);
    workspaces
        .refound_record(&workspace, APP, RECORD)
        .await
        .expect("the governor founds the record again");

    // The away device comes back and sends what it held. The relay keeps a
    // write as it arrives, before the node follows it, so the wait is for the
    // node's own copy to hold what the away device wrote: the record a device
    // joining now is replayed is the node's, and a check made before the node
    // followed the last write would read a record it has yet to hold.
    for message in [offline_one, offline_two] {
        send_sealed(&client, workspace_id, &member_key, &message);
    }
    follows(&workspaces, &workspace, "line 3").await;
    follows_field(&workspaces, &workspace, "offline", "two").await;

    // What a device joining now is replayed: it reads the record the governor
    // reads, with the away device's changes in it.
    let replayed = retained_writes(&relay, workspace_id, RECORD).await;
    let held_by_governor = workspaces
        .record_document(&workspace, APP, RECORD)
        .await
        .unwrap()
        .expect("the governor holds the record");
    assert_eq!(
        newcomer_opens(&member, &replayed).get_heads(),
        automerge::AutoCommit::load(&held_by_governor)
            .unwrap()
            .get_heads(),
        "a joiner reads the record whole, the away device's changes included"
    );
    drop(workspaces);
    relay.shutdown().await;
}

/// The node asks to be let into a channel a member opened, and the member is
/// linked to it directly, as a member device's runtime is: the member hears
/// the node's request, so it can admit the node. Without it the node takes
/// every member's request in and is never let into a channel itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_linked_directly_hears_the_nodes_channel_governance() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    let relay = test_host_taking_part(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
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

    let channel = collab_types::workspace_scope(&created.workspace);
    let request =
        collab_types::MlsAccessRequestV1::signed(channel, vec![1, 2, 3], 1, &key).unwrap();
    workspaces
        .publish_channel_governance(
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
    relay.shutdown().await;
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
        .author_room_record(workspace, &lounge.governor, (APP, "other"), |doc| {
            doc.put(ROOT, "title", "Other").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    // A write the node never gets: the member's next one waits on it.
    let _never_sent = lounge.member.seal(workspace, |document| {
        document.put(ROOT, "title", "Lost").unwrap();
        document.commit_edit("fixture", "title");
    });
    set_title(&lounge.member, workspaces, workspace, "After").await;

    let read = workspaces.whole_records(workspace, None).await.unwrap();
    assert!(!read.is_whole());
    let unread: Vec<_> = read.unread.iter().map(|(route, _)| route.clone()).collect();
    assert_eq!(unread, [(APP.to_owned(), RECORD.to_owned())]);
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
    lounge.relay.shutdown().await;
}

/// The value at `key` in the record as the node reads it.
async fn read_key(workspaces: &Workspaces, workspace: &str, key: &str) -> Option<String> {
    automerge::AutoCommit::load(
        &workspaces
            .record_document(workspace, APP, RECORD)
            .await
            .unwrap()?,
    )
    .ok()?
    .get(ROOT, key)
    .ok()
    .flatten()
    .and_then(|(value, _)| value.into_string().ok())
}

/// A record's repair kept a change it left out, one that builds on a change
/// the node never got. Once a member's write brings that change, the node
/// writes the kept one into the record by itself, and it leaves the archive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_a_repair_kept_is_taken_in_once_what_it_builds_on_arrives() {
    let lounge = Box::new(lounge().await);
    let (workspaces, workspace) = (&lounge.workspaces, lounge.workspace.as_str());
    // The member's write the node never got, and the change it wrote on top
    // of it, which the repair took out of the record and kept.
    let before = lounge.member.document().get_heads();
    let _never_sent = lounge.member.seal(workspace, |document| {
        document.put(ROOT, "lost", "on the way").unwrap();
        document.commit_edit("fixture", "lost");
    });
    let lost = lounge.member.document().save_after(&before);
    let after_lost = lounge.member.document().get_heads();
    let _kept_out = lounge.member.seal(workspace, |document| {
        document.put(ROOT, "kept", "by the repair").unwrap();
        document.commit_edit("fixture", "kept");
    });
    let kept = lounge.member.document().get_changes(&after_lost);
    let [kept] = kept.as_slice() else {
        panic!("one change kept: {kept:?}");
    };
    // The relay's state root, which holds the governed records' `rooms`.
    let root = workspaces
        .workspace(workspace)
        .await
        .unwrap()
        .lock()
        .await
        .path
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .to_path_buf();
    let archive =
        crate::workspaces::repaired_records::repaired_record_dir(&root, workspace, APP, RECORD);
    std::fs::create_dir_all(&archive).unwrap();
    let kept_at = archive.join(format!("{}.change", kept.hash()));
    std::fs::write(&kept_at, kept.raw_bytes()).unwrap();

    // The member's next write carries the change the node never got.
    lounge
        .member
        .sends_changes(workspaces, workspace, lost)
        .await;
    tilo_test_env::within("the kept change is taken in", async {
        while kept_at.exists() || read_key(workspaces, workspace, "kept").await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    assert_eq!(
        read_key(workspaces, workspace, "lost").await.as_deref(),
        Some("on the way")
    );
    assert_eq!(
        read_key(workspaces, workspace, "kept").await.as_deref(),
        Some("by the repair")
    );
    assert!(
        !archive.exists(),
        "a record with nothing kept leaves no directory behind"
    );
    // What the node wrote, a member that lacks the kept change opens, and
    // holds it then.
    let own = governor_published(workspaces, workspace, RECORD, &lounge.governor)
        .await
        .expect("the node published the kept change");
    let mut lacking = lounge.member.document().fork_at(&after_lost).unwrap();
    assert!(lacking.get(ROOT, "kept").unwrap().is_none());
    lounge.member.open_into(&own, &mut lacking);
    assert_eq!(
        lacking
            .get(ROOT, "kept")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .as_deref(),
        Some("by the repair"),
        "a member reads the kept change from the node's write"
    );
    lounge.relay.shutdown().await;
}

/// A device linked to the workspace directly, as a desktop's runtime is,
/// hearing what the link brings it.
struct Desk {
    key: ed25519_dalek::SigningKey,
    link: tilo_peer_state::direct::DirectSync,
    heard: tilo_peer_state::direct::Reader,
    pairing: tokio::task::JoinHandle<Result<(), String>>,
}

impl Desk {
    /// Another device of the lounge's person, admitted and linked to the node, caught up.
    async fn link(lounge: &Lounge, seed: u8) -> Self {
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
        let host = lounge
            .workspaces
            .direct()
            .link(lounge.workspace_id)
            .expect("the node serves the workspace over a direct link");
        let mut link =
            tilo_peer_state::direct::DirectSync::new(host.read(Clone::clone), key.clone());
        if capacity > 0 {
            link = link.with_heard_capacity(capacity);
        }
        let heard = link.reader();
        let mut caught_up = link.caught_up();
        let linked = link.clone();
        let pairing =
            tokio::spawn(
                async move { tilo_peer_state::direct::DirectSync::pair(&linked, &host).await },
            );
        tilo_test_env::within("the desk catches up with the node", async {
            caught_up.wait_for(|caught_up| *caught_up).await.unwrap();
        })
        .await;
        Self {
            key,
            link,
            heard,
            pairing,
        }
    }

    /// The sequence numbers of `author`'s writes of [`RECORD`] this desk hears live, in order,
    /// up to and including `last`: each write the desk took in, as many times as it arrived.
    async fn hears(&mut self, author: [u8; 32], last: u64) -> Vec<u64> {
        let heard = &mut self.heard;
        tilo_test_env::within("the desk hears the writes", async {
            let mut seqs = Vec::new();
            loop {
                let frame = heard.recv().await.unwrap();
                if let Some(seq) = write_of(&frame.message, author) {
                    assert!(
                        frame.report.is_ok(),
                        "the desk takes the write in: {:?}",
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

/// The sequence number of `message`, when it is `author`'s write of [`RECORD`].
fn write_of(message: &NeighbourMsg, author: [u8; 32]) -> Option<u64> {
    match message {
        NeighbourMsg::AppDocument {
            record_id,
            envelope,
            ..
        } if record_id == RECORD && envelope.author.as_bytes() == &author => Some(envelope.seq),
        _ => None,
    }
}

/// The sequence numbers of `author`'s writes of [`RECORD`] the relay sends `client` live, in
/// order, up to and including `last`, as many times as each arrived.
async fn delivered(client: &crate::RelayClient, author: [u8; 32], last: u64) -> Vec<u64> {
    tilo_test_env::within("the session is sent the writes", async {
        let mut seqs = Vec::new();
        loop {
            let Some(crate::Envelope::Delta { delta, .. }) = client.recv().await else {
                continue;
            };
            let Some(
                crate::CoeditPayload::Checkpoint { body, .. }
                | crate::CoeditPayload::RetainedDelta { body, .. },
            ) = crate::CoeditPayload::decode(&delta)
            else {
                continue;
            };
            let Ok(message) = postcard::from_bytes::<NeighbourMsg>(&body) else {
                continue;
            };
            if let Some(seq) = write_of(&message, author) {
                seqs.push(seq);
                if seq == last {
                    return seqs;
                }
            }
        }
    })
    .await
}

/// A workspace's members write on whatever carrier their runtime runs: a page
/// through a relay session, a desktop over the workspace's direct link. What
/// one writes reaches every other member live, once, on either carrier, and
/// never comes back to its own: each linked desk hears each of the page's
/// writes once, and the page and the other desk each get each of a desk's
/// writes once, all sealed as their authors wrote them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_reaches_every_member_once_whatever_carrier_it_came_on() {
    const WRITES: u64 = 6;
    let (lounge, page) = lounge_with_client().await;
    let mut desks = [Desk::link(&lounge, 5).await, Desk::link(&lounge, 6).await];
    let page_author = lounge.member_key.verifying_key().to_bytes();

    let started = std::time::Instant::now();
    for line in 0..WRITES {
        seal_and_send_title(
            &lounge.member,
            &page,
            &lounge.workspace,
            lounge.workspace_id,
            &lounge.member_key,
            &format!("from the page {line}"),
        );
    }
    for desk in &mut desks {
        assert_eq!(
            desk.hears(page_author, WRITES).await,
            (1..=WRITES).collect::<Vec<_>>(),
            "a linked desk hears each of the page's writes once, live"
        );
    }
    eprintln!(
        "{WRITES} writes of a relay session reached two linked desks in {:?}",
        started.elapsed()
    );

    let [writer, other] = &mut desks;
    let sealing = SealingMember::join(
        &lounge.workspaces,
        &lounge.workspace,
        (APP, RECORD),
        writer.key.clone(),
    )
    .await;
    let desk_author = writer.key.verifying_key().to_bytes();
    let started = std::time::Instant::now();
    for line in 0..WRITES {
        let ((), message) = sealing.seal(&lounge.workspace, |doc| {
            doc.put(ROOT, "title", format!("from a desk {line}"))
                .unwrap();
            doc.commit_edit("fixture", "title");
        });
        assert!(
            matches!(&message, NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some()),
            "the desk writes sealed"
        );
        writer.link.send(message).unwrap();
    }
    assert_eq!(
        other.hears(desk_author, WRITES).await,
        (1..=WRITES).collect::<Vec<_>>(),
        "the other linked desk hears each of the desk's writes once"
    );
    assert_eq!(
        delivered(&page, desk_author, WRITES).await,
        (1..=WRITES).collect::<Vec<_>>(),
        "the page's session is sent each of the desk's writes once"
    );
    eprintln!(
        "{WRITES} writes of a linked desk reached the other desk and a relay session in {:?}",
        started.elapsed()
    );

    // Nothing the writer wrote came back to it, and nothing reached it twice:
    // the link carries in order, so an echo of any write would arrive before
    // the page's next one.
    seal_and_send_title(
        &lounge.member,
        &page,
        &lounge.workspace,
        lounge.workspace_id,
        &lounge.member_key,
        "the page again",
    );
    let heard = &mut writer.heard;
    let echoed = tilo_test_env::within("the writer hears the page again", async {
        let mut echoed = Vec::new();
        loop {
            let frame = heard.recv().await.unwrap();
            if write_of(&frame.message, page_author) == Some(WRITES + 1) {
                return echoed;
            }
            if let Some(seq) = write_of(&frame.message, desk_author) {
                echoed.push(("the desk's own", seq, frame.retained));
            }
            if let Some(seq) = write_of(&frame.message, page_author) {
                echoed.push(("the page's", seq, frame.retained));
            }
        }
    })
    .await;
    assert!(
        echoed.is_empty(),
        "no write came back to its author or reached the writer twice: {echoed:?}"
    );
    for desk in desks {
        desk.pairing.abort();
    }
    drop(page);
    let Lounge {
        relay, workspaces, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}

/// Write `value` at `key` into `replica` as its next change, and hand that
/// change to the workspace as the member's write of [`RECORD`].
async fn burst_write(
    member: &SealingMember,
    workspaces: &Workspaces,
    workspace: &str,
    replica: &mut automerge::AutoCommit,
    (key, value): (&str, &str),
) {
    let heads = replica.get_heads();
    replica.put(ROOT, key, value).unwrap();
    replica.commit_edit("fixture", key);
    member
        .sends_changes(workspaces, workspace, replica.save_after(&heads))
        .await;
}

/// A record whose repair keeps a change that waits for another is not read
/// and written again for every write of it: a burst of writes it waits for
/// nothing from leaves its kept change where it is, and the one write that
/// brings what it builds on takes it in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repair_is_tried_for_a_write_only_when_that_write_could_release_what_it_keeps() {
    let lounge = Box::new(lounge().await);
    let (workspaces, workspace) = (&lounge.workspaces, lounge.workspace.as_str());
    let tried = || super::super::repaired_records::tried(workspace, APP, RECORD);
    // The record as the member joined it, the write the node never got, and
    // the change the repair kept on top of it.
    let joined = lounge.member.document().get_heads();
    let _never_sent = lounge.member.seal(workspace, |document| {
        document.put(ROOT, "lost", "on the way").unwrap();
        document.commit_edit("fixture", "lost");
    });
    let lost = lounge.member.document().save_after(&joined);
    let after_lost = lounge.member.document().get_heads();
    let _kept_out = lounge.member.seal(workspace, |document| {
        document.put(ROOT, "kept", "by the repair").unwrap();
        document.commit_edit("fixture", "kept");
    });
    let kept = lounge.member.document().get_changes(&after_lost);
    let [kept] = kept.as_slice() else {
        panic!("one change kept: {kept:?}");
    };
    // The relay's state root, which holds the governed records' `rooms`.
    let root = workspaces
        .workspace(workspace)
        .await
        .unwrap()
        .lock()
        .await
        .path
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .to_path_buf();
    let archive =
        crate::workspaces::repaired_records::repaired_record_dir(&root, workspace, APP, RECORD);
    std::fs::create_dir_all(&archive).unwrap();
    let kept_at = archive.join(format!("{}.change", kept.hash()));
    std::fs::write(&kept_at, kept.raw_bytes()).unwrap();

    // A writer of its own, so none of these writes takes the place in the
    // member's sequence that the kept change waits for.
    let mut burst = lounge.member.document().fork_at(&joined).unwrap();
    burst.set_actor(automerge::ActorId::from(b"burst".as_slice()));
    // The first write is the one that reads the archive: what it keeps is
    // what every write after it is tested against.
    burst_write(
        &lounge.member,
        workspaces,
        workspace,
        &mut burst,
        ("title", "burst 0"),
    )
    .await;
    follows(workspaces, workspace, "burst 0").await;
    let read = tried();
    for index in 1..=4 {
        let value = format!("burst {index}");
        burst_write(
            &lounge.member,
            workspaces,
            workspace,
            &mut burst,
            ("title", &value),
        )
        .await;
    }
    follows(workspaces, workspace, "burst 4").await;
    // What a write costs a record's repair is decided once it landed.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        tried(),
        read,
        "the record's repair is not read for a write it waits for nothing from"
    );
    assert!(
        kept_at.exists(),
        "the kept change stays while what it builds on is not in the record"
    );
    assert_eq!(
        read_key(workspaces, workspace, "title").await.as_deref(),
        Some("burst 4"),
        "the node took every write of the burst in"
    );

    // The write that brings the change the kept one builds on.
    lounge
        .member
        .sends_changes(workspaces, workspace, lost)
        .await;
    tilo_test_env::within("the kept change is taken in", async {
        while kept_at.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    assert_eq!(
        read_key(workspaces, workspace, "kept").await.as_deref(),
        Some("by the repair")
    );
    assert_eq!(
        tried(),
        read + 1,
        "the one write that could release it, and no other"
    );
    lounge.relay.shutdown().await;
}

/// The write of `record` of `app` the host published last, whoever signed it.
async fn last_published(
    workspaces: &Workspaces,
    workspace: &str,
    record: &str,
) -> Option<SignedOp> {
    let state = workspaces.workspace(workspace).await.unwrap();
    let published: Vec<Vec<u8>> = state
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .map(|publication| publication.bytes.to_vec())
        .collect();
    published
        .iter()
        .rev()
        .filter_map(|bytes| postcard::from_bytes::<NeighbourMsg>(bytes).ok())
        .find_map(|message| match message {
            NeighbourMsg::AppDocument {
                record_id,
                envelope,
                ..
            } if record_id == record => Some(*envelope),
            _ => None,
        })
}

/// A record's repair kept a change a bot made showing another name. The
/// record is held sealed, so its writes are decided without holding anyone to
/// a name, and the change goes in as the host's own write, as any kept change
/// does, keeping the name it showed. Once the host runs the bot the change
/// names, its kept changes are tried then ([`Workspaces::keep_bot_device`]).
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kept_change_a_bot_showed_another_name_in_keeps_the_name_it_showed() {
    use crate::workspaces::tests::bots::{bridge_program, install_command};
    use automerge::transaction::Transactable as _;
    use tilo_actions::{ActionContext, ActionInvoker, ActorKind, Appearance};

    let lounge = Box::new(lounge().await);
    let (workspaces, workspace) = (&lounge.workspaces, lounge.workspace.as_str());
    // The bot's own device, installed with personas: only a bot may show
    // another name, and only the bot it names may say so.
    let installation = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";
    let bot_key = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    workspaces
        .govern_bot(
            workspace,
            &lounge.governor,
            install_command(
                workspace,
                &bridge_program(),
                (installation, 8),
                tilo_peer_state::AppearancePolicy::Personas,
            ),
        )
        .await
        .unwrap();
    // The change the bot made on top of the record, showing a name of its own.
    let record = workspaces
        .record_document(workspace, APP, RECORD)
        .await
        .unwrap()
        .unwrap();
    let mut bot = automerge::AutoCommit::load(&record).unwrap();
    bot.set_actor(automerge::ActorId::from(b"bridge".as_slice()));
    bot.put(ROOT, "posted", "for Ada").unwrap();
    let context = ActionContext::default().with_invoker(
        ActionInvoker::actor(format!("bot:{installation}"))
            .with_kind(ActorKind::Bot)
            .with_appearance(Some(Appearance::new("Ada", None, None).unwrap())),
    );
    bot.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
    let kept = bot.get_last_local_change().unwrap();
    // The relay's state root, which holds the governed records' `rooms`.
    let root = workspaces
        .workspace(workspace)
        .await
        .unwrap()
        .lock()
        .await
        .path
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .to_path_buf();
    let archive =
        crate::workspaces::repaired_records::repaired_record_dir(&root, workspace, APP, RECORD);
    std::fs::create_dir_all(&archive).unwrap();
    let kept_at = archive.join(format!("{}.change", kept.hash()));
    std::fs::write(&kept_at, kept.raw_bytes()).unwrap();

    // The bot's device arrives, as it does once the host runs the bot: what it
    // made goes in then, as its own write.
    workspaces.keep_bot_device(workspace, installation, bot_key.clone());
    tilo_test_env::within("the kept change is taken in", async {
        while kept_at.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    let published = last_published(workspaces, workspace, RECORD)
        .await
        .expect("the host published the kept change");
    assert_eq!(
        published.author.as_bytes(),
        &lounge.governor.device_public_key,
        "a sealed record's kept change goes in as the host's write"
    );
    assert_eq!(
        read_key(workspaces, workspace, "posted").await.as_deref(),
        Some("for Ada"),
        "the record holds the change, showing the name it showed"
    );
    lounge.relay.shutdown().await;
}

/// Every write of [`RECORD`] the relay sends `client` live, as `(author, seq)`
/// in the order they arrived, up to and including `author`'s write `last`.
async fn writes_sent(
    client: &crate::RelayClient,
    author: [u8; 32],
    last: u64,
) -> Vec<([u8; 32], u64)> {
    tilo_test_env::within("the session is sent the writes", async {
        let mut writes = Vec::new();
        loop {
            let Some(crate::Envelope::Delta { delta, .. }) = client.recv().await else {
                continue;
            };
            let Some(
                crate::CoeditPayload::Checkpoint { body, .. }
                | crate::CoeditPayload::RetainedDelta { body, .. },
            ) = crate::CoeditPayload::decode(&delta)
            else {
                continue;
            };
            let Ok(NeighbourMsg::AppDocument {
                record_id,
                envelope,
                ..
            }) = postcard::from_bytes::<NeighbourMsg>(&body)
            else {
                continue;
            };
            if record_id != RECORD {
                continue;
            }
            let write = (*envelope.author.as_bytes(), envelope.seq);
            writes.push(write);
            if write == (author, last) {
                return writes;
            }
        }
    })
    .await
}

/// A desktop whose reader of the workspace's link falls behind, and a host
/// whose reader of it does while a page waits on the relay's session, still
/// read every write once after catching up from the workspace's history:
/// the desk each of the page's writes, the page each of the desk's, and
/// neither its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readers_that_fell_behind_read_every_write_once() {
    const WRITES: u64 = 6;
    let (lounge, page) = lounge_reading_with_client(2).await;
    let mut desk = Desk::reading(&lounge, 5, 2).await;
    let page_author = lounge.member_key.verifying_key().to_bytes();

    // The page writes while the desk reads nothing: its link has room for two.
    let started = std::time::Instant::now();
    for line in 0..WRITES {
        seal_and_send_title(
            &lounge.member,
            &page,
            &lounge.workspace,
            lounge.workspace_id,
            &lounge.member_key,
            &format!("from the page {line}"),
        );
    }
    // Every write reached the desk's link before the desk reads one.
    let mut landed = desk.link.changes();
    tilo_test_env::within("the page's writes reach the desk", async {
        landed
            .wait_for(|_| {
                desk.link.cursors().iter().any(|cursor| {
                    cursor.lineage.author == page_author && cursor.after_seq == WRITES
                })
            })
            .await
            .unwrap();
    })
    .await;
    let caught_up = std::time::Instant::now();
    assert_eq!(
        desk.hears(page_author, WRITES).await,
        (1..=WRITES).collect::<Vec<_>>(),
        "a desk that fell behind reads each of the page's writes once"
    );
    eprintln!(
        "a desk that fell behind {WRITES} writes caught up in {:?} ({:?} after the first write)",
        caught_up.elapsed(),
        started.elapsed()
    );

    // The host's reader of the link stalls while the desk writes: it cannot
    // take the workspace while the test holds it.
    let sealing = SealingMember::join(
        &lounge.workspaces,
        &lounge.workspace,
        (APP, RECORD),
        desk.key.clone(),
    )
    .await;
    let desk_author = desk.key.verifying_key().to_bytes();
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
            doc.put(ROOT, "title", format!("from a desk {line}"))
                .unwrap();
            doc.commit_edit("fixture", "title");
        });
        desk.link.send(message).unwrap();
    }
    tilo_test_env::within("the desk's writes reach the host's link", async {
        landed
            .wait_for(|_| {
                host.cursors().iter().any(|cursor| {
                    cursor.lineage.author == desk_author && cursor.after_seq == WRITES
                })
            })
            .await
            .unwrap();
    })
    .await;
    let released = std::time::Instant::now();
    drop(held);
    let sent: Vec<_> = writes_sent(&page, desk_author, WRITES)
        .await
        .into_iter()
        .filter(|(author, _)| [desk_author, page_author].contains(author))
        .collect();
    eprintln!(
        "a host reader that fell behind {WRITES} writes carried them to a session in {:?}",
        released.elapsed()
    );
    assert_eq!(
        sent,
        (1..=WRITES)
            .map(|seq| (desk_author, seq))
            .collect::<Vec<_>>(),
        "the page's session is sent each of the desk's writes once, and none of its own"
    );
    desk.pairing.abort();
    lounge.relay.shutdown().await;
}

/// A page wrote parts of a chained record through its relay session, and the
/// host wrote a record and a part of its own, before the workspace's link kept
/// any of it: the relay retains them, the link's history does not. A relay
/// session replays a joining page only a chain's head and sends an older part
/// once asked; a desktop decides on its link's replay alone, and a record it
/// holds nothing of after that replay is one its app makes anew. The relay's
/// start hands the link what it retains before it serves anyone
/// ([`Workspaces::recall`]): a desk that links holds every one of them once it
/// caught up, the parts behind the head included, and reads them rather than
/// making them again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_desk_linking_later_holds_every_part_a_page_wrote_before_the_link_kept_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    // The host after the restart holds the channel keys the one before it took part with.
    let keys = std::sync::Arc::<crate::workspaces::fixtures::TestChannelKeys>::default();
    let relay = crate::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        std::sync::Arc::clone(&keys),
    )
    .await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    let (member_key, member_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    workspaces
        .author_room_record(&workspace, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member =
        SealingMember::join(&workspaces, &workspace, (APP, RECORD), member_key.clone()).await;
    let page = Box::pin(subscribed_member(
        &relay,
        &member_key,
        member_cert.clone(),
        workspace_id,
    ))
    .await;
    // The page's parts: two behind the chain's head, and the head.
    let mut written = Vec::new();
    for index in [0, 1, 3] {
        let record = neighbour_wire::record_part(RECORD, index);
        let part = member.part(&workspaces, &workspace, &record).await;
        let ((), message) = part.seal(&workspace, |doc| {
            doc.put(ROOT, "title", format!("part {index}")).unwrap();
            doc.commit_edit("fixture", "title");
        });
        send_sealed(&page, workspace_id, &member_key, &message);
        written.push(record);
    }
    // The host's own: a part behind the head, as a host seeds or repairs one.
    let host_part = neighbour_wire::record_part(RECORD, 2);
    workspaces
        .author_room_record(&workspace, &cert, (APP, &host_part), |doc| {
            doc.put(ROOT, "title", "the host's part")
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    written.extend([host_part, RECORD.to_owned()]);
    tilo_test_env::within("the relay retains the page's parts", async {
        for record in written.iter().filter(|record| record.as_str() != RECORD) {
            while retained_writes(&relay, workspace_id, record)
                .await
                .is_empty()
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    })
    .await;
    drop(page);
    drop(workspaces);
    relay.shutdown().await;

    // The link's history as it stood before the relay carried session writes to it.
    std::fs::remove_dir_all(dir.path().join("store")).unwrap();
    let relay = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let workspaces = relay.workspaces().clone();
    let lounge = Lounge {
        _dir: dir,
        relay,
        workspaces,
        workspace,
        workspace_id,
        governor: cert,
        governor_key: key,
        member_key,
        member_cert,
        member,
    };
    let desk = Desk::link(&lounge, 5).await;
    for record in &written {
        assert!(
            desk.link.read(|peer| peer.document(APP, record).is_some()),
            "a desk that caught up holds {record}: its app reads it, and does not make it again"
        );
    }
    drop(desk);
    let Lounge {
        workspaces, relay, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}

/// A desktop wrote a part over the workspace's link that the relay's own
/// retained history lacks (the link kept it before the relay carried each
/// link write to its sessions). A page decides a part is absent when the relay
/// answers its ask with nothing, and its app then makes the part anew: so the
/// relay's history takes what the link keeps, and a page that opens the part
/// is sent it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_opening_a_part_only_the_link_kept_is_sent_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    let keys = std::sync::Arc::<crate::workspaces::fixtures::TestChannelKeys>::default();
    let relay = crate::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        std::sync::Arc::clone(&keys),
    )
    .await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    let (member_key, member_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    workspaces
        .author_room_record(&workspace, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member =
        SealingMember::join(&workspaces, &workspace, (APP, RECORD), member_key.clone()).await;
    // A desktop's part behind the chain's head, kept by the link alone.
    let (desk_key, _) = member_device(&workspaces, &workspace, (&key, &cert), 5).await;
    let older = neighbour_wire::record_part(RECORD, 0);
    let desk = SealingMember::join(&workspaces, &workspace, (APP, RECORD), desk_key)
        .await
        .part(&workspaces, &workspace, &older)
        .await;
    let ((), message) = desk.seal(&workspace, |doc| {
        doc.put(ROOT, "title", "the desk's older part").unwrap();
        doc.commit_edit("fixture", "title");
    });
    let frame = tilo_peer_state::store::Frame::of(&message).unwrap();
    workspaces.inner.store.retain(workspace_id, &frame).unwrap();
    drop(workspaces);
    relay.shutdown().await;

    let relay = crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &cert, keys).await;
    let page = Box::pin(subscribed_member(
        &relay,
        &member_key,
        member_cert,
        workspace_id,
    ))
    .await;
    let channel = neighbour_wire::app_document_channel(APP, &older);
    page.send(crate::Envelope::OpenRecord {
        workspace_id,
        channel,
    })
    .unwrap();
    let frames = tilo_test_env::within("the relay answers the page's ask", async {
        loop {
            if let Some(crate::Envelope::RecordOpened {
                channel: opened,
                frames,
                ..
            }) = page.recv().await
                && opened == channel
            {
                return frames;
            }
        }
    })
    .await;
    assert!(
        frames > 0,
        "the relay sends the page the part the link kept: it reads it, and does not make it again"
    );
    drop(member);
    drop(page);
    relay.shutdown().await;
}

/// What the relay sends `page` when it opens `record`: how many frames, before
/// it says the record was sent whole.
async fn opened_frames(page: &crate::RelayClient, workspace_id: WorkspaceId, record: &str) -> u32 {
    let channel = neighbour_wire::app_document_channel(APP, record);
    page.send(crate::Envelope::OpenRecord {
        workspace_id,
        channel,
    })
    .unwrap();
    tilo_test_env::within("the relay answers the page's ask", async {
        loop {
            if let Some(crate::Envelope::RecordOpened {
                channel: opened,
                frames,
                ..
            }) = page.recv().await
                && opened == channel
            {
                return frames;
            }
        }
    })
    .await
}

/// A linked desk writes a part over the workspace's link while the relay
/// runs, and the relay's copy of it for its sessions fails `failing` times;
/// the part, and the relay's page, wait.
async fn a_desk_writes_while_its_copy_fails(
    failing: usize,
) -> (Lounge, crate::RelayClient, String) {
    let (lounge, page) = lounge_with_client().await;
    lounge
        .workspaces
        .inner
        .session_carriers
        .failing
        .store(failing, std::sync::atomic::Ordering::Release);
    let desk = Desk::link(&lounge, 5).await;
    let record = neighbour_wire::record_part(RECORD, 0);
    let part = SealingMember::join(
        &lounge.workspaces,
        &lounge.workspace,
        (APP, RECORD),
        desk.key.clone(),
    )
    .await
    .part(&lounge.workspaces, &lounge.workspace, &record)
    .await;
    let ((), message) = part.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "title", "the desk's part").unwrap();
        doc.commit_edit("fixture", "title");
    });
    desk.link.send(message).unwrap();
    tilo_test_env::within("the relay tries to copy the desk's write", async {
        while lounge
            .workspaces
            .inner
            .session_carriers
            .failing
            .load(std::sync::atomic::Ordering::Acquire)
            == failing
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    (lounge, page, record)
}

/// The relay's copy of a desk's write for its sessions fails once while it
/// runs: it is tried again until it lands, and a page that opens the record
/// is sent it from the relay's own history.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_desks_write_whose_copy_failed_once_still_reaches_a_page() {
    let (lounge, page, record) = Box::pin(a_desk_writes_while_its_copy_fails(1)).await;
    tilo_test_env::within("the copy is tried again until it lands", async {
        while retained_writes(&lounge.relay, lounge.workspace_id, &record)
            .await
            .is_empty()
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    assert_eq!(
        opened_frames(&page, lounge.workspace_id, &record).await,
        1,
        "the page is sent the desk's write, once"
    );
    drop(page);
    let Lounge {
        workspaces, relay, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}

/// The relay's copy of a desk's write keeps failing: until it lands, a page
/// that opens the record is sent the write from the workspace's link, never
/// told the record is absent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_is_sent_a_desks_write_whose_copy_has_not_landed() {
    let (lounge, page, record) = Box::pin(a_desk_writes_while_its_copy_fails(usize::MAX)).await;
    assert!(
        retained_writes(&lounge.relay, lounge.workspace_id, &record)
            .await
            .is_empty(),
        "the copy has not landed"
    );
    assert_eq!(
        opened_frames(&page, lounge.workspace_id, &record).await,
        1,
        "the page is sent the desk's write from the link"
    );
    drop(page);
    let Lounge {
        workspaces, relay, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}

/// The member's page lets a change go, unsigned and unsent, and sends its next change, built on
/// it, to the relay: the hole a page left in a record before its writes waited for the record.
/// Answers the write it sent, once the relay retains it.
async fn page_leaves_a_hole(lounge: &Lounge, client: &crate::RelayClient) -> SignedOp {
    lounge.member.lets_go(|doc| {
        doc.put(ROOT, "early", "let go").unwrap();
        doc.commit_edit("fixture", "early");
    });
    let ((), stuck) = lounge.member.seal(&lounge.workspace, |doc| {
        doc.put(ROOT, "after", "built on it").unwrap();
        doc.commit_edit("fixture", "after");
    });
    send_sealed(client, lounge.workspace_id, &lounge.member_key, &stuck);
    let NeighbourMsg::AppDocument { envelope, .. } = stuck else {
        panic!("a document write")
    };
    tilo_test_env::within("the relay retains the stuck write", async {
        while !retained_writes(&lounge.relay, lounge.workspace_id, RECORD)
            .await
            .iter()
            .any(|op| op.signature == envelope.signature)
        {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    // As a reader is refused it: the record a newcomer is sent waits for a change no device
    // will ever send.
    let held = retained_writes(&lounge.relay, lounge.workspace_id, RECORD).await;
    assert!(
        !newcomer_opens(&lounge.member, &held)
            .get_missing_deps(&[])
            .is_empty(),
        "a newcomer is sent a record that waits for the change the page let go"
    );
    *envelope
}

/// Wait until what a newcomer is sent of [`RECORD`] holds neither `stuck` nor anything that waits
/// for a change it lacks, and reads `title`: the record whole without the stuck write.
async fn newcomer_reads_whole_without(lounge: &Lounge, stuck: &SignedOp, wanted: &str) {
    tilo_test_env::within("the relay folds the stuck write away", async {
        loop {
            let held = retained_writes(&lounge.relay, lounge.workspace_id, RECORD).await;
            let mut read = automerge::AutoCommit::new();
            let opens = held
                .iter()
                .all(|op| lounge.member.try_open_into(op, &mut read).is_ok());
            if opens
                && !held.iter().any(|op| op.signature == stuck.signature)
                && read.get_missing_deps(&[]).is_empty()
                && title(&read.save()).as_deref() == Some(wanted)
            {
                assert!(read.get(ROOT, "after").unwrap().is_none());
                assert!(read.get(ROOT, "early").unwrap().is_none());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await;
}

/// A page that held a record's keys before the record itself wrote a change in that moment, let it
/// go unsigned and unsent, and sent its next change, built on the first: the record holds a change
/// whose base no device will ever send, and every reader was refused it as incomplete. The
/// relay's fold left that write beside its snapshot, so a newcomer was sent it forever.
///
/// The governor, which every write of the record reaches, finds the write stuck behind a change
/// its writer never sent: the writer's own sequence of changes shows the gap, and every write it
/// signed before is held. The relay's next fold leaves it out of the record for good, keeps it in
/// the store's archive, and a newcomer reads the rest of the record whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_built_on_a_change_its_writer_never_sent_is_folded_away() {
    let (lounge, client) = lounge_with_client().await;
    let (workspaces, workspace) = (&lounge.workspaces, lounge.workspace.as_str());
    let stuck = page_leaves_a_hole(&lounge, &client).await;

    // The governor reads the record without it, and holds it whole so.
    follows_field(workspaces, workspace, "title", "Welcome").await;
    assert!(
        workspaces
            .whole_records(workspace, None)
            .await
            .unwrap()
            .is_whole(),
        "a write no device can complete leaves the record whole without it"
    );

    // The page reloads and writes on, past the point the record's channel folds.
    let reloaded = lounge.member.reloads(workspaces, workspace).await;
    for index in 0..40 {
        seal_and_send_title(
            &reloaded,
            &client,
            workspace,
            lounge.workspace_id,
            &lounge.member_key,
            &format!("line {index}"),
        );
    }
    follows(workspaces, workspace, "line 39").await;
    newcomer_reads_whole_without(&lounge, &stuck, "line 39").await;
    let root = state_root(workspaces, workspace, lounge.workspace_id).await;
    assert!(
        !archived_bodies(&root).is_empty(),
        "the write the record left is kept in the store's archive"
    );
    drop(client);
    let Lounge {
        workspaces, relay, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}

/// A relay's store already holds such a stuck write from a run before writes waited for their
/// record, and nothing the record's writers do brings it a fold: the relay folds it away as it
/// starts, and a newcomer reads the record whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_store_holding_a_stuck_write_is_repaired_when_its_relay_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::host());
    // The relay after the restart holds the channel keys the one before it took part with.
    let keys = std::sync::Arc::<crate::workspaces::fixtures::TestChannelKeys>::default();
    let relay = crate::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        std::sync::Arc::clone(&keys),
    )
    .await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let (workspace, workspace_id) = (created.workspace, created.workspace_id);
    let (member_key, member_cert) = member_device(&workspaces, &workspace, (&key, &cert), 3).await;
    workspaces
        .author_room_record(&workspace, &cert, (APP, RECORD), |doc| {
            doc.put(ROOT, "title", "Welcome").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let member =
        SealingMember::join(&workspaces, &workspace, (APP, RECORD), member_key.clone()).await;
    let client = Box::pin(subscribed_member(
        &relay,
        &member_key,
        member_cert.clone(),
        workspace_id,
    ))
    .await;
    let lounge = Lounge {
        _dir: dir,
        relay,
        workspaces,
        workspace,
        workspace_id,
        governor: cert,
        governor_key: key,
        member_key,
        member_cert,
        member,
    };
    let stuck = page_leaves_a_hole(&lounge, &client).await;
    drop(client);
    let Lounge {
        _dir: dir,
        relay,
        workspaces,
        workspace,
        workspace_id,
        governor,
        governor_key,
        member_key,
        member_cert,
        member,
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;

    let relay =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &governor_key, &governor, keys)
            .await;
    let workspaces = relay.workspaces().clone();
    let lounge = Lounge {
        _dir: dir,
        relay,
        workspaces,
        workspace,
        workspace_id,
        governor,
        governor_key,
        member_key,
        member_cert,
        member,
    };
    newcomer_reads_whole_without(&lounge, &stuck, "Welcome").await;
    let Lounge {
        workspaces, relay, ..
    } = lounge;
    drop(workspaces);
    relay.shutdown().await;
}
