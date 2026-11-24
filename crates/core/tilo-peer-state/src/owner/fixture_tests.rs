use crate::NeighbourMsg;
use crate::join::{self, JoinTransportRequest, JoinTransportResponse};
use crate::owner::{NativeWorkspaceOwner, VerifiedNativeEnrollmentIntent, open_file, testing};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_identity::policy_change::epoch;
use tilo_user_identity::{DeviceCert, RevocationList};

pub(crate) struct Fixture {
    pub(crate) directory: tempfile::TempDir,
    pub(crate) owner: NativeWorkspaceOwner,
    pub(crate) key: SigningKey,
    pub(crate) certificate: DeviceCert,
    pub(crate) now: SystemTime,
    pub(crate) workspace: testing::workspace::TestWorkspace,
}

pub(crate) fn fixture() -> Fixture {
    let key = SigningKey::from_bytes(&[92; 32]);
    let root = SigningKey::from_bytes(&[91; 32]);
    fixture_with(&root, key.clone(), key, join::Role::Editor, false)
}

/// A device of a second person, enrolled into a workspace another person governs:
/// a key holder that neither governs nor hosts the workspace.
pub(crate) fn member_device_fixture() -> Fixture {
    fixture_with(
        &SigningKey::from_bytes(&[96; 32]),
        SigningKey::from_bytes(&[92; 32]),
        SigningKey::from_bytes(&[94; 32]),
        join::Role::Moderator,
        true,
    )
}

/// A device of `root` at `key`, enrolled into the workspace `governor_key`'s
/// device of the workspace owner's root governs.
fn fixture_with(
    root: &SigningKey,
    key: SigningKey,
    governor_key: SigningKey,
    role: join::Role,
    administers: bool,
) -> Fixture {
    let governor_root = SigningKey::from_bytes(&[91; 32]);
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("owner.json")).unwrap();
    let certificate = testing::certificate(root, &key, "Governor");
    let mut governor = testing::workspace::TestWorkspace::empty_interactive(
        testing::certificate(&governor_root, &governor_key, "Governor"),
        governor_key,
        testing::WORKSPACE,
    )
    .unwrap();
    let endpoint = iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(
            &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
        )
        .unwrap(),
    );
    if administers {
        governor
            .allow_governance(
                &hex::encode(certificate.user_public_key),
                tilo_action_policy::roles::governance::GRANT,
            )
            .unwrap();
    }
    let hint = governor.hint(endpoint);
    let now = SystemTime::now();
    // The owner side runs on `now` throughout; the governor answers on the
    // same instant so invitations and challenges never lapse mid-test on a
    // slow machine.
    governor.pin_clock(now);
    let now_ns = testing::ns(now).unwrap();
    let invitation = governor.issue(
        vec![hint.clone()],
        role,
        Some(certificate.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(certificate.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let response = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation.clone(),
            device_cert: certificate.clone(),
            request_id: [95; 32],
        }),
    );
    let JoinTransportResponse::Challenge(response) = response else {
        panic!("challenge refused")
    };
    let redemption = join::verify_connection_redemption(
        &invitation,
        &certificate,
        [95; 32],
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let admission = crate::own_admission::own_admission(
        testing::WORKSPACE,
        &response.governance,
        &redemption,
        None,
        &key,
        now,
    )
    .unwrap();
    let proof = join::sign_join_proof(response.challenge, &key).unwrap();
    let complete = join::JoinCompleteRequest { proof, admission };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        certificate.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let response = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Complete(complete),
    );
    let JoinTransportResponse::Complete(bootstrap) = response else {
        panic!("completion refused")
    };
    assert!(bootstrap.records.is_empty());
    let enrollment = testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    Fixture {
        directory,
        owner,
        key,
        certificate,
        now,
        workspace: governor,
    }
}

/// Give the member a grant: a change of the workspace's governance this
/// device is allowed to make.
async fn grant(fixture: &Fixture, id: &str) {
    let command = crate::workspace_membership::WorkspaceMembershipCommand::Grant {
        grant: tilo_action_policy::Grant::new(
            id,
            tilo_action_policy::Grantee::person(hex::encode(fixture.certificate.user_public_key)),
            tilo_action_policy::TreePath::workspace(testing::WORKSPACE),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(fixture.certificate.user_public_key)),
        ),
    };
    fixture
        .owner
        .commit_membership(
            crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap(),
            &fixture.certificate,
            &command,
            &fixture.key,
            fixture.now,
        )
        .await
        .unwrap();
}

/// One write of this device's record, committed to its ownership.
fn publish_write(fixture: &Fixture, workspace: crate::owner::WorkspaceId, key: &str) {
    let envelope = signed_write(fixture, workspace, key);
    fixture
        .owner
        .admit_writes(
            workspace,
            vec![crate::owner::WorkspaceDocumentWrite {
                app: "so.tilo.chat".into(),
                document: String::new(),
                readers: None,
                envelope,
            }],
            fixture.now,
        )
        .unwrap();
}

/// The chat record as the owner holds it, loaded as its app holds it: under
/// one actor, as the app's store keeps it across writes and restarts.
fn app_document(fixture: &Fixture, workspace: crate::owner::WorkspaceId) -> automerge::AutoCommit {
    let held = fixture
        .owner
        .with_current(workspace, |view| {
            view.peer()
                .document("so.tilo.chat", "")
                .ok_or_else(|| "no chat record".to_owned())
        })
        .unwrap();
    automerge::AutoCommit::load(&held)
        .unwrap()
        .with_actor(automerge::ActorId::from(b"so.tilo.chat".as_slice()))
}

/// A write of `key` to the chat record as its app signs it, before the
/// owner's turn admits it.
fn signed_write(
    fixture: &Fixture,
    workspace: crate::owner::WorkspaceId,
    key: &str,
) -> collab_types::SignedOp {
    signed_write_to(
        fixture,
        workspace,
        &mut app_document(fixture, workspace),
        key,
    )
}

/// [`signed_write`] by an app that keeps `record` open between its writes:
/// the write is the change `key` makes to `record`, which holds it from then
/// on, as the owner does once it committed the write.
fn signed_write_to(
    fixture: &Fixture,
    workspace: crate::owner::WorkspaceId,
    record: &mut automerge::AutoCommit,
    key: &str,
) -> collab_types::SignedOp {
    use automerge::transaction::Transactable as _;
    let before = record.get_heads();
    record.put(automerge::ROOT, key, key).unwrap();
    record.commit_edit("fixture", key, &fixture.certificate.user_public_key);
    let changes = record.save_after(&before);
    let message = fixture
        .owner
        .with_current(workspace, |view| {
            view.peer()
                .prepare_signed_mutation(
                    "so.tilo.chat",
                    "",
                    crate::DocumentMutation::Incremental(changes),
                    &fixture.key,
                    fixture.now,
                )
                .map(|(message, _)| message)
        })
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a signed write is a document write")
    };
    *envelope
}

/// Admit `envelope`, this device's signed write to `document` of the chat app, and make it
/// durable.
fn admit(
    fixture: &Fixture,
    workspace: crate::owner::WorkspaceId,
    document: &str,
    envelope: collab_types::SignedOp,
) {
    fixture
        .owner
        .admit_writes(
            workspace,
            vec![crate::owner::WorkspaceDocumentWrite {
                app: "so.tilo.chat".into(),
                document: document.into(),
                readers: None,
                envelope,
            }],
            fixture.now,
        )
        .unwrap();
}

/// A member of the workspace, not one of its owner's devices, restarts with
/// its retained frames and publishes them as it signed them: nothing it sends
/// waits for an owner's checkpoint. A peer handed the later frame first asks
/// for what that frame builds on, and converges once it arrives.
#[tokio::test]
async fn a_member_device_republishes_its_retained_frames_and_a_peer_converges() {
    let fixture = member_device_fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let path = fixture.directory.path().join("owner.json");
    grant(&fixture, "chat-read").await;
    testing::start_resource(
        &fixture.owner,
        workspace,
        "so.tilo.chat",
        "",
        &fixture.key,
        fixture.now,
    );
    // Two writes of this device, the second building on the first.
    publish_write(&fixture, workspace, "first");
    publish_write(&fixture, workspace, "second");
    let reopened = open_file(&path).unwrap();
    let frames = reopened.frames(workspace);
    assert_eq!(
        frames.len(),
        3,
        "the record's creation and two writes are frames a peer is sent"
    );
    let frames = frames
        .into_iter()
        .map(|bytes| postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&bytes).unwrap())
        .collect::<Vec<_>>();
    // A peer that never saw the first write receives the second one first.
    let origin = |fixture: &Fixture| {
        crate::TransportOrigin::RetainedServerDelta(fixture.certificate.device_public_key)
    };
    let mut peer = fixture.workspace.peer().clone();
    let held = peer
        .receive_from_report(origin(&fixture), frames[2].clone(), fixture.now)
        .unwrap();
    assert_eq!(
        held.primary(),
        &crate::InputDisposition::RetryLater(crate::MissingPrerequisite::DocumentCheckpoint)
    );
    for frame in &frames[0..=1] {
        peer.receive_from_report(origin(&fixture), frame.clone(), fixture.now)
            .unwrap();
    }
    let landed = peer
        .receive_from_report(origin(&fixture), frames[2].clone(), fixture.now)
        .unwrap();
    assert!(
        matches!(landed.primary(), crate::InputDisposition::Duplicate(_)),
        "the held write landed once what it builds on arrived: {:?}",
        landed.primary()
    );
    let authored = reopened
        .with_current(workspace, |view| {
            Ok(view.peer().document("so.tilo.chat", ""))
        })
        .unwrap();
    assert_eq!(peer.document("so.tilo.chat", ""), authored);
}

/// A write to an app record changes nothing authorization decides on, so its
/// watchers are not told; a change of the workspace's grants is, at once.
///
/// Not announced is read as the process's announcement epoch holding still
/// ([`tilo_identity::policy_change`]), and every owner, device fleet and
/// registry in a process announces into that one epoch: this reads it where
/// no other test owns a workspace that moves it.
#[tokio::test]
async fn only_a_governance_change_is_announced_to_authorization() {
    if !tilo_test_env::in_own_process_here(
        module_path!(),
        "only_a_governance_change_is_announced_to_authorization",
    ) {
        return;
    }
    let fixture = member_device_fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let before = epoch();
    grant(&fixture, "chat-read").await;
    assert_ne!(epoch(), before, "a grant is announced");
    testing::start_resource(
        &fixture.owner,
        workspace,
        "so.tilo.chat",
        "",
        &fixture.key,
        fixture.now,
    );
    let before = epoch();
    publish_write(&fixture, workspace, "first");
    publish_write(&fixture, workspace, "second");
    assert_eq!(epoch(), before, "an app record's writes are not announced");
    grant(&fixture, "notes-read").await;
    assert_ne!(epoch(), before, "the next grant is announced");
}

/// The median of `of` over `range`.
fn median<T: Ord + Copy>(of: &[T], range: std::ops::Range<usize>) -> T {
    let mut window = of[range].to_vec();
    window.sort_unstable();
    window[window.len() / 2]
}

/// What a write costs does not grow with its record's history: a keystroke
/// into a record of 2,000 changes writes about what one into a record of 100
/// does, and reads at most the document it changed (to digest it, store it
/// and publish its checkpoint). After a restart the record is the
/// state it was stored as, with the same digest, and every change reads
/// back.
#[tokio::test]
async fn a_write_costs_what_it_changed_however_long_the_record_history() {
    const WRITES: usize = 2_050;
    let fixture = fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let path = fixture.directory.path().join("owner.json");
    testing::start_resource(
        &fixture.owner,
        workspace,
        "so.tilo.chat",
        "",
        &fixture.key,
        fixture.now,
    );
    let written = || {
        let store: &dyn std::any::Any = &*fixture.owner.0.store;
        store
            .downcast_ref::<crate::owner::file_store::FileStore>()
            .unwrap()
            .written
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    // What a write cost when each one wrote the workspace's file whole.
    let whole = || {
        serde_json::to_vec(&fixture.owner.0.authority.read().workspaces[&workspace])
            .unwrap()
            .len()
    };
    // The app keeps its record open between its keystrokes: read back from
    // the owner before each one, the record's whole history was loaded 2,050
    // times, none of it part of what a write costs the owner.
    let mut record = app_document(&fixture, workspace);
    let mut wholes = Vec::new();
    let mut costs = Vec::with_capacity(WRITES);
    let mut reads = Vec::with_capacity(WRITES);
    let mut times = Vec::with_capacity(WRITES);
    for n in 0..WRITES {
        let request = signed_write_to(&fixture, workspace, &mut record, &format!("k{n}"));
        let before = written();
        let read_from = crate::work::now().bytes_read;
        let started = std::time::Instant::now();
        admit(&fixture, workspace, "", request);
        times.push(started.elapsed());
        reads.push(crate::work::now().bytes_read.saturating_sub(read_from));
        costs.push(written() - before);
        if n == 100 || n == 2_000 {
            wholes.push(whole());
        }
    }
    let (early, late) = (median(&costs, 100..150), median(&costs, 2_000..2_050));
    let (read_early, read_late) = (median(&reads, 100..150), median(&reads, 2_000..2_050));
    println!(
        "a write at 100 changes writes {early} bytes and reads {read_early}, at 2,000 {late} \
         and {read_late}, in {:?} and {:?}; the workspace's file whole is {wholes:?} bytes",
        median(&times, 100..150),
        median(&times, 2_000..2_050),
    );
    assert!(
        late <= early * 2,
        "a write at 2,000 changes costs {late} bytes, at 100 {early}"
    );
    // Rewrites now and then included: a new snapshot once the journal
    // outgrows the last one, a record's base saved anew once its changes
    // outweigh half of it, the settled admissions once the recent ones fill
    // up. Each costs about what the writes since the last one did, so a long
    // run of writes costs about as much late as early on average.
    let average = |of: &[usize], range: std::ops::Range<usize>| {
        of[range.clone()].iter().sum::<usize>() / range.len()
    };
    let (before, after) = (average(&costs, 100..1_000), average(&costs, 1_000..WRITES));
    assert!(
        after <= before * 2,
        "writes past 1,000 changes cost {after} bytes each on average, before {before}"
    );
    // What a write reads is its own document at most: the checkpoint it
    // publishes is of the document it changed, never of the workspace.
    let own = app_document(&fixture, workspace).save().len();
    assert!(
        read_late <= (own as u64) * 2,
        "a write at 2,000 changes reads {read_late} bytes, its document is {own}"
    );
    assert_eq!(
        app_document(&fixture, workspace).get_heads(),
        record.get_heads(),
        "the owner holds the record as the app that wrote it does"
    );
    reopens_as_it_was_stored(fixture.owner, &path, WRITES);
}

/// A workspace holds every document of every app in it, so what it holds is
/// never capped whole: a workspace of 1,500 documents that hold more together
/// than the 32 MiB a workspace's file was once capped at takes each new
/// document's write at what it changed, as one of 300 documents does, and
/// opens again with the next write costing what it changed.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario in order: made, measured, opened again"
)]
async fn a_workspace_past_any_size_takes_writes_at_what_they_change() {
    const DOCUMENTS: usize = 1_500;
    /// Text that does not compress, as a document's own does not.
    const TEXT: usize = 32 * 1024;
    let fixture = fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let written = |owner: &NativeWorkspaceOwner| {
        let store: &dyn std::any::Any = &*owner.0.store;
        store
            .downcast_ref::<crate::owner::file_store::FileStore>()
            .unwrap()
            .written
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    let text = |n: usize| {
        let mut seed = blake3::hash(&n.to_le_bytes());
        let mut text = String::with_capacity(TEXT);
        while text.len() < TEXT {
            text.push_str(&seed.to_hex());
            seed = blake3::hash(seed.as_bytes());
        }
        text
    };
    let mut costs = Vec::with_capacity(DOCUMENTS);
    let mut times = Vec::with_capacity(DOCUMENTS);
    for n in 0..DOCUMENTS {
        let document = format!("r{n}");
        let before = written(&fixture.owner);
        testing::start_resource(
            &fixture.owner,
            workspace,
            "so.tilo.chat",
            &document,
            &fixture.key,
            fixture.now,
        );
        times.push(write_text(
            &fixture.owner,
            &fixture.key,
            &fixture.certificate.user_public_key,
            fixture.now,
            workspace,
            &document,
            &text(n),
        ));
        costs.push(written(&fixture.owner) - before);
        if n % 50 == 49 {}
    }
    let (early, late) = (
        median(&costs, 300..400),
        median(&costs, DOCUMENTS - 100..DOCUMENTS),
    );
    let whole = serde_json::to_vec(&fixture.owner.0.authority.read().workspaces[&workspace])
        .unwrap()
        .len();
    println!(
        "a document made at 300 writes {early} bytes, at {DOCUMENTS} {late}; its write commits \
         in {:?} and {:?}; the workspace's file whole is {whole} bytes",
        median(&times, 300..400),
        median(&times, DOCUMENTS - 100..DOCUMENTS),
    );
    assert!(
        late <= early * 2,
        "a document made at {DOCUMENTS} costs {late} bytes, at 300 {early}"
    );
    // Rewrites now and then included: a snapshot once the journal outgrows
    // the last one costs about what the writes since it did.
    let average =
        |range: std::ops::Range<usize>| costs[range.clone()].iter().sum::<usize>() / range.len();
    let (before, after) = (
        average(300..DOCUMENTS / 2),
        average(DOCUMENTS / 2..DOCUMENTS),
    );
    assert!(
        after <= before * 2,
        "documents in the later half cost {after} bytes each on average, before {before}"
    );

    // Opened again, it holds every document, and its next write appends what
    // it changed rather than writing all the workspace holds first.
    drop(fixture.owner);
    let reopened = open_file(&path).unwrap();
    let held = reopened
        .with_current(workspace, |view| {
            Ok((0..DOCUMENTS)
                .filter(|n| {
                    view.peer()
                        .document("so.tilo.chat", &format!("r{n}"))
                        .is_some()
                })
                .count())
        })
        .unwrap();
    assert_eq!(held, DOCUMENTS, "every document opens again");
    let before = written(&reopened);
    write_text(
        &reopened,
        &fixture.key,
        &fixture.certificate.user_public_key,
        fixture.now,
        workspace,
        "r7",
        "again",
    );
    let after_open = written(&reopened) - before;
    assert!(
        after_open <= early * 2,
        "the first write after opening costs {after_open} bytes, a write before {early}"
    );
    assert!(
        whole > 32 * 1024 * 1024,
        "the workspace holds more than it was once capped at: {whole} bytes"
    );
}

/// Write `text` into the chat app's document `document`, as its app signs it, and
/// answer how long the owner took to commit it.
fn write_text(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    person: &[u8; 32],
    now: SystemTime,
    workspace: crate::owner::WorkspaceId,
    document: &str,
    text: &str,
) -> std::time::Duration {
    use automerge::transaction::Transactable as _;
    let message = owner
        .with_current(workspace, |view| {
            let held = view
                .peer()
                .document("so.tilo.chat", document)
                .ok_or_else(|| "no document".to_owned())?;
            let mut doc = automerge::AutoCommit::load(&held)
                .map_err(|error| error.to_string())?
                .with_actor(automerge::ActorId::from(b"so.tilo.chat".as_slice()));
            let before = doc.get_heads();
            doc.put(automerge::ROOT, "text", text)
                .map_err(|error| error.to_string())?;
            doc.commit_edit("fixture", "text", person);
            view.peer()
                .prepare_signed_mutation(
                    "so.tilo.chat",
                    document,
                    crate::DocumentMutation::Incremental(doc.save_after(&before)),
                    key,
                    now,
                )
                .map(|(message, _)| message)
        })
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a write is a document write")
    };
    let started = std::time::Instant::now();
    owner
        .admit_writes(
            workspace,
            vec![crate::owner::WorkspaceDocumentWrite {
                app: "so.tilo.chat".into(),
                document: document.into(),
                readers: None,
                envelope: *envelope,
            }],
            now,
        )
        .unwrap();
    // A link carries it; only the write's own cost is measured here.
    for frame in owner.frames(workspace) {
        owner.sent(workspace, &frame).unwrap();
    }
    started.elapsed()
}

/// The record reopens from `path` as the state it was stored as, with the
/// same digest, and every one of the `writes` changes reads back.
fn reopens_as_it_was_stored(owner: NativeWorkspaceOwner, path: &std::path::Path, writes: usize) {
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let digest = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(workspace, |view| view.peer().durable_hash())
            .unwrap()
    };
    let before = digest(&owner);
    drop(owner);
    let reopened = open_file(path).unwrap();
    assert_eq!(
        digest(&reopened),
        before,
        "the record reopens as the state it was stored as"
    );
    let document = reopened
        .with_current(workspace, |view| {
            view.peer()
                .document("so.tilo.chat", "")
                .ok_or_else(|| "no record".to_owned())
        })
        .unwrap();
    let document = automerge::AutoCommit::load(&document).unwrap();
    for n in 0..writes {
        let key = format!("k{n}");
        assert!(
            automerge::ReadDoc::get(&document, automerge::ROOT, &key)
                .unwrap()
                .is_some(),
            "{key} reads after a restart"
        );
    }
}

/// Make documents `made` of the chat app, empty, a batch of writes per owner turn.
fn documents(
    fixture: &Fixture,
    workspace: crate::owner::WorkspaceId,
    made: std::ops::Range<usize>,
) {
    const BATCH: usize = 100;
    let mut n = made.start;
    while n < made.end {
        let end = n.saturating_add(BATCH).min(made.end);
        let writes = fixture
            .owner
            .with_current(workspace, |view| {
                (n..end)
                    .map(|n| {
                        let message = view.peer().clone().publication_mut().publish_document(
                            "so.tilo.chat",
                            &format!("r{n}"),
                            automerge::AutoCommit::new().save(),
                            &fixture.key,
                            fixture.now,
                        )?;
                        let NeighbourMsg::AppDocument { envelope, .. } = message else {
                            return Err("a record's founding write is a document write".to_owned());
                        };
                        Ok(crate::owner::WorkspaceDocumentWrite {
                            app: "so.tilo.chat".into(),
                            document: format!("r{n}"),
                            readers: None,
                            envelope: *envelope,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .unwrap();
        fixture
            .owner
            .admit_writes(workspace, writes, fixture.now)
            .unwrap();
        // A link carries what this device wrote; nothing waits in its record.
        for frame in fixture.owner.frames(workspace) {
            fixture.owner.sent(workspace, &frame).unwrap();
        }
        n = end;
    }
}

/// A write costs what it changed, however many documents the workspace
/// holds: its receipt's digest, the durable encoding it publishes and the
/// journal entry it appends each take the document the write changed, not
/// every document, and a commit in a workspace of 3,000 documents costs about
/// what one in a workspace of 300 does. The digest is the one the workspace
/// reopens with, and still changes with every write.
#[tokio::test]
async fn a_write_commits_at_what_it_changed_however_many_documents() {
    // Enough writes per size that the median holds while the machine runs other work: one
    // write takes a few milliseconds, so a few dozen of them can each be slowed by a burst.
    const WRITES: usize = 101;
    const SIZES: [usize; 2] = [300, 3_000];
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let digest = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(workspace, |view| view.peer().durable_hash())
            .unwrap()
    };
    let fixtures = SIZES.map(|size| {
        let fixture = fixture();
        documents(&fixture, workspace, 0..size);
        fixture
    });
    // The two workspaces take their writes in turns, so whatever else the
    // machine does meanwhile slows both alike.
    let mut times = [Vec::new(), Vec::new()];
    for n in 0..WRITES {
        for ((fixture, size), times) in fixtures.iter().zip(SIZES).zip(&mut times) {
            let before = digest(&fixture.owner);
            crate::state::DOCUMENTS_WALKED.with(|walked| walked.set(0));
            times.push(write_text(
                &fixture.owner,
                &fixture.key,
                &fixture.certificate.user_public_key,
                fixture.now,
                workspace,
                &format!("r{n}"),
                &format!("text {n} at {size}"),
            ));
            // Its digest's leaf, and its part of the encoding published.
            let walked = crate::state::DOCUMENTS_WALKED.with(std::cell::Cell::get);
            assert!(
                walked <= 2,
                "a write at {size} documents takes {walked} documents again"
            );
            assert_ne!(digest(&fixture.owner), before, "a write changes the digest");
        }
    }
    // A write at each size is timed right after the one at the other, so a burst of other
    // work slows the pair alike: the median of the pairs' ratios is what the larger workspace
    // costs a write, whatever the machine did meanwhile. It catches what grows with the
    // workspace outside the documents a write walks (an index it looks in, a map it keeps).
    let mut ratios: Vec<f64> = times[0]
        .iter()
        .zip(&times[1])
        .map(|(early, late)| late.as_secs_f64() / early.as_secs_f64().max(f64::MIN_POSITIVE))
        .collect();
    ratios.sort_by(f64::total_cmp);
    let ratio = ratios[WRITES / 2];
    let [early, late] = times.map(|mut times| {
        times.sort();
        times[WRITES / 2]
    });
    println!(
        "a write commits at 300 documents in {early:?}, at 3,000 in {late:?}; \
         a pair's median ratio {ratio:.2}"
    );
    assert!(
        ratio <= 1.5,
        "a write at 3,000 documents costs {ratio:.2} times one at 300 \
         (medians {late:?} and {early:?})"
    );
    let [_, fixture] = fixtures;
    let path = fixture.directory.path().join("owner.json");
    let before = digest(&fixture.owner);
    drop(fixture.owner);
    let reopened = open_file(&path).unwrap();
    assert_eq!(
        digest(&reopened),
        before,
        "the workspace reopens with the digest it was stored with"
    );
}
