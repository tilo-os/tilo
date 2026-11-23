//! The one path a device's writes take: admitted into its own replica, kept as
//! a frame until a link carries it, and durable across a restart.

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use collab_types::signer::DeviceSigner;
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tilo_peer_state::NeighbourMsg;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::testing::{self, workspace::TestWorkspace};
use tilo_peer_state::owner::{LocalWrite, NativeWorkspaceOwner, open_file};

const APP: &str = "test.app";
const RECORD: &str = "tab:1";

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// A record holding `value`, as a whole snapshot.
fn note(value: &str) -> Vec<u8> {
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", value).unwrap();
    document.commit();
    document.save()
}

/// Write `value` into the test record of `workspace` as `device`.
async fn write(
    owner: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    device: &SigningKey,
) {
    // A record is founded empty; a local publication adds to what the workspace
    // holds and never starts a record with content of its own.
    let founding = !owner
        .with_current(workspace, |view| {
            Ok(view.peer().holds_document(APP, RECORD))
        })
        .expect("the device holds the workspace");
    if founding {
        owner
            .write_local(
                workspace,
                device.verifying_key().to_bytes(),
                Arc::new(device.clone()) as Arc<dyn DeviceSigner>,
                LocalWrite {
                    app: APP,
                    document: RECORD,
                    readers: None,
                    changes: &AutoCommit::new().save(),
                    founding: true,
                    snapshot: true,
                    sealed: None,
                    now: tilo_clock::wall(),
                },
            )
            .await
            .expect("the device founds its record");
    }
    let written = owner
        .write_local(
            workspace,
            device.verifying_key().to_bytes(),
            Arc::new(device.clone()) as Arc<dyn DeviceSigner>,
            LocalWrite {
                app: APP,
                document: RECORD,
                readers: None,
                changes: &note("written"),
                founding: false,
                snapshot: true,
                sealed: None,
                now: tilo_clock::wall(),
            },
        )
        .await;
    written.expect("the device writes its record");
}

/// Everything the fixture's copy of the workspace holds that `owners` do not yet: the governance
/// its enrollments wrote after each device was installed.
fn sync_from_fixture(fixture: &TestWorkspace, owners: &[&NativeWorkspaceOwner]) {
    let message = fixture.peer().workspace_message();
    let device = fixture.device().device_public_key;
    let now = tilo_clock::wall();
    for owner in owners {
        owner
            .receive(
                fixture.workspace_id(),
                TransportOrigin::ServerDelta(device),
                message.clone(),
                now,
            )
            .unwrap();
    }
}

/// A workspace `root`'s person made, with the device `certificate` holding it,
/// and the fixture that holds it beside.
fn held(
    directory: &std::path::Path,
) -> (NativeWorkspaceOwner, TestWorkspace, SigningKey, SigningKey) {
    let root = key(1);
    let a_key = key(2);
    let b_key = key(3);
    let a_cert = testing::certificate(&root, &a_key, "A");
    let b_cert = testing::certificate(&root, &b_key, "B");
    let owner_a = open_file(&directory.join("a.json")).unwrap();
    let owner_b = open_file(&directory.join("b.json")).unwrap();
    let fixture = empty(&a_cert, &a_key);
    let fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    let fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
    sync_from_fixture(&fixture, &[&owner_a, &owner_b]);
    (owner_a, fixture, a_key, b_key)
}

/// An empty workspace under a new id, held by `certificate`'s device: what a
/// person's workspace is before any app writes to it.
fn empty(certificate: &tilo_user_identity::DeviceCert, key: &SigningKey) -> TestWorkspace {
    TestWorkspace::empty_interactive(
        certificate.clone(),
        key.clone(),
        &uuid::Uuid::new_v4().to_string(),
    )
    .expect("the fixture device holds its workspace")
}

/// A write is durable in this device's record before any peer holds it, and a
/// restart with the other device away keeps it until a link carries it there.
#[tokio::test]
async fn a_write_is_durable_before_a_peer_holds_it_and_survives_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (owner_a, fixture, a_key, _b_key) = held(directory.path());
    let workspace = fixture.workspace_id();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    write(&owner_a, workspace, &a_key).await;
    let frames = owner_a.frames(workspace);
    assert!(!frames.is_empty(), "the write is a frame a link carries");
    assert!(
        owner_b.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(false),
        "the other device held nothing of it"
    );
    // The device goes away with the write only in its record, and comes back:
    // the frame is still there to be carried.
    drop(owner_a);
    let owner_a = open_file(&directory.path().join("a.json")).unwrap();
    assert_eq!(
        owner_a.frames(workspace),
        frames,
        "a restart keeps the frame until a peer holds it"
    );
    // A link carries it to the device that was away.
    let author = a_key.verifying_key().to_bytes();
    for frame in &frames {
        let message = NeighbourMsg::decode(frame).unwrap();
        owner_b
            .receive(
                workspace,
                TransportOrigin::ServerDelta(author),
                message,
                tilo_clock::wall(),
            )
            .unwrap();
        owner_a.sent(workspace, frame).unwrap();
    }
    assert!(
        owner_b.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(true),
        "the write reaches the device when it links"
    );
    assert!(
        owner_a.frames(workspace).is_empty(),
        "a frame a peer took is not carried again"
    );
}

/// The first hour with one device and no server: a workspace is made, written
/// to, a person is invited and joins, one device leaves, and the workspace
/// lives on the other.
#[tokio::test]
async fn one_device_holds_a_workspace_through_its_first_use_and_leaving_it() {
    let directory = tempfile::tempdir().unwrap();
    let (owner_a, fixture, a_key, b_key) = held(directory.path());
    let workspace = fixture.workspace_id();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    write(&owner_a, workspace, &a_key).await;
    // The two devices link: what one wrote is carried to the other.
    for frame in owner_a.frames(workspace) {
        let message = NeighbourMsg::decode(&frame).unwrap();
        owner_b
            .receive(
                workspace,
                TransportOrigin::ServerDelta(a_key.verifying_key().to_bytes()),
                message,
                tilo_clock::wall(),
            )
            .unwrap();
    }
    assert!(
        owner_b
            .with_current(workspace, |view| Ok(view
                .peer()
                .holds_document(APP, RECORD)))
            .unwrap_or(false),
        "the joiner holds what the earlier device wrote"
    );
    write(&owner_b, workspace, &b_key).await;
    assert_eq!(
        owner_a.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD))),
        Ok(true)
    );
    assert!(owner_a.release_workspace(workspace).unwrap());
    assert!(!owner_a.contains_workspace(workspace));
    let reopened = open_file(&directory.path().join("a.json")).unwrap();
    assert!(
        !reopened.contains_workspace(workspace),
        "a workspace a device left does not come back"
    );
    assert!(
        owner_b.contains_workspace(workspace),
        "the workspace lives on the device that stayed"
    );
    let _ = fixture;
}

/// A device admitted after another wrote holds the history it is sent and can
/// write at once, and the earlier device holds what it writes.
#[tokio::test]
async fn a_joiner_gets_the_history_and_writes_right_away() {
    let directory = tempfile::tempdir().unwrap();
    let root = key(1);
    let a_key = key(2);
    let b_key = key(3);
    let a_cert = testing::certificate(&root, &a_key, "A");
    let b_cert = testing::certificate(&root, &b_key, "B");
    let owner_a = open_file(&directory.path().join("a.json")).unwrap();
    let mut fixture = empty(&a_cert, &a_key);
    let workspace = fixture.workspace_id();
    fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    write(&owner_a, workspace, &a_key).await;
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    let _fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
    for frame in owner_a.frames(workspace) {
        let message = NeighbourMsg::decode(&frame).unwrap();
        owner_b
            .receive(
                workspace,
                TransportOrigin::ServerDelta(a_key.verifying_key().to_bytes()),
                message,
                tilo_clock::wall(),
            )
            .unwrap();
    }
    assert!(
        owner_b.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(true),
        "the joiner holds what was written before it came"
    );
    write(&owner_b, workspace, &b_key).await;
    for frame in owner_b.frames(workspace) {
        let message = NeighbourMsg::decode(&frame).unwrap();
        owner_a
            .receive(
                workspace,
                TransportOrigin::ServerDelta(b_key.verifying_key().to_bytes()),
                message,
                tilo_clock::wall(),
            )
            .unwrap();
    }
    assert!(
        owner_a.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(true),
        "the earlier device holds what the joiner wrote"
    );
}

/// An effect's terminal receipt is recorded once and reads the same on every
/// device.
#[tokio::test]
async fn an_effect_receipt_is_recorded_once_on_both_devices() {
    let directory = tempfile::tempdir().unwrap();
    let (owner_a, fixture, a_key, _b_key, a_cert) = held_certified(directory.path());
    let workspace = fixture.workspace_id();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    let actor = tilo_actions::MemberId::person(hex::encode(a_cert.user_public_key));
    let receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(uuid::Uuid::from_u128(1)),
        &actor,
        tilo_actions::ActionContext::for_resource("resource", "kind", "Resource"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        1,
    );
    let now = tilo_clock::wall();
    owner_a
        .record_effect(workspace, &a_cert, &receipt, now)
        .unwrap();
    // An exact retry changes nothing.
    owner_a
        .record_effect(workspace, &a_cert, &receipt, now)
        .unwrap();
    let message = owner_a
        .with_current(workspace, |view| Ok(view.peer().workspace_message()))
        .unwrap();
    owner_b
        .receive(
            workspace,
            TransportOrigin::ServerDelta(a_key.verifying_key().to_bytes()),
            message,
            now,
        )
        .unwrap();
    let read = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(workspace, |view| {
                let workspace_doc = view
                    .peer()
                    .admitted_workspace()
                    .ok_or_else(|| "no governance".to_owned())?;
                Ok(tilo_desktop_model::projection::workspace_effect_receipts_of(workspace_doc))
            })
            .unwrap()
    };
    let receipts = read(&owner_a);
    assert_eq!(receipts.len(), 1, "recorded once: {receipts:?}");
    assert_eq!(receipts[0], receipt);
    assert_eq!(read(&owner_b), receipts, "the other device reads the same");
    let mut other = receipt;
    other.status = tilo_actions::EffectReceiptStatus::Cancelled;
    assert!(
        owner_a
            .record_effect(workspace, &a_cert, &other, now)
            .is_err(),
        "an invocation keeps the terminal receipt it was given"
    );
}

/// A workspace held by two certified devices, with A's certificate.
fn held_certified(
    directory: &std::path::Path,
) -> (
    NativeWorkspaceOwner,
    TestWorkspace,
    SigningKey,
    SigningKey,
    tilo_user_identity::DeviceCert,
) {
    let root = key(1);
    let a_key = key(2);
    let b_key = key(3);
    let a_cert = testing::certificate(&root, &a_key, "A");
    let b_cert = testing::certificate(&root, &b_key, "B");
    let owner_a = open_file(&directory.join("a.json")).unwrap();
    let owner_b = open_file(&directory.join("b.json")).unwrap();
    let fixture = empty(&a_cert, &a_key);
    let fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    let fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
    sync_from_fixture(&fixture, &[&owner_a, &owner_b]);
    (owner_a, fixture, a_key, b_key, a_cert)
}

/// Two devices write different role changes while offline, and the same
/// membership is what they read once they exchange them.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: the grant, both devices' offline changes and their exchange"
)]
async fn two_devices_offline_role_changes_converge() {
    let directory = tempfile::tempdir().unwrap();
    let root = key(1);
    let a_key = key(2);
    let b_key = key(3);
    let a_cert = testing::certificate(&root, &a_key, "A");
    let b_cert = testing::certificate(&root, &b_key, "B");
    let owner_a = open_file(&directory.path().join("a.json")).unwrap();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    let fixture = empty(&a_cert, &a_key);
    let workspace = fixture.workspace_id();
    let fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    let fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
    sync_from_fixture(&fixture, &[&owner_a, &owner_b]);
    let now = tilo_clock::wall();
    let grant_action = |id: &str, person: [u8; 32]| {
        tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Grant {
            grant: tilo_action_policy::Grant::new(
                id,
                tilo_action_policy::Grantee::person(hex::encode(person)),
                tilo_action_policy::TreePath::workspace(workspace.to_string()),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Grantee::person(hex::encode(a_cert.user_public_key)),
            ),
        }
    };
    // A lets B grant: every device admits it.
    let to_b = tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Grant {
        grant: tilo_action_policy::Grant::new(
            "fixture:grant-to-b",
            tilo_action_policy::Grantee::person(hex::encode(b_cert.user_public_key)),
            tilo_action_policy::roles::governance_action(
                &workspace.to_string(),
                tilo_action_policy::roles::governance::GRANT,
            ),
            tilo_action_policy::KindSet::OUTSIDE,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(a_cert.user_public_key)),
        ),
    };
    owner_a
        .commit_membership(workspace, &a_cert, &to_b, &a_key, now)
        .await
        .unwrap();
    let change = owner_a
        .with_current(workspace, |view| Ok(view.peer().workspace_message()))
        .unwrap();
    owner_b
        .receive(
            workspace,
            TransportOrigin::ServerDelta(a_key.verifying_key().to_bytes()),
            change,
            now,
        )
        .unwrap();
    // Offline, each grants a different person.
    let x = key(4).verifying_key().to_bytes();
    let y = key(5).verifying_key().to_bytes();
    owner_a
        .commit_membership(
            workspace,
            &a_cert,
            &grant_action("fixture:grant-x", x),
            &a_key,
            now,
        )
        .await
        .unwrap();
    owner_b
        .commit_membership(
            workspace,
            &b_cert,
            &grant_action("fixture:grant-y", y),
            &b_key,
            now,
        )
        .await
        .unwrap();
    // They exchange what each wrote.
    let from_b = owner_b
        .with_current(workspace, |view| Ok(view.peer().workspace_message()))
        .unwrap();
    owner_a
        .receive(
            workspace,
            TransportOrigin::ServerDelta(b_key.verifying_key().to_bytes()),
            from_b,
            now,
        )
        .unwrap();
    let from_a = owner_a
        .with_current(workspace, |view| Ok(view.peer().workspace_message()))
        .unwrap();
    owner_b
        .receive(
            workspace,
            TransportOrigin::ServerDelta(a_key.verifying_key().to_bytes()),
            from_a,
            now,
        )
        .unwrap();
    let grants = |owner: &NativeWorkspaceOwner| {
        let mut ids: Vec<String> = owner
            .with_current(workspace, |view| {
                Ok(view.peer().governance().governed_policy()?.grants)
            })
            .unwrap()
            .into_iter()
            .map(|grant| grant.id.as_str().to_owned())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(grants(&owner_a), grants(&owner_b), "the same membership");
    let held = grants(&owner_a);
    assert!(held.contains(&"fixture:grant-x".to_owned()), "{held:?}");
    assert!(held.contains(&"fixture:grant-y".to_owned()), "{held:?}");
}

/// The keys one device opens a sealed record's history with, and seals its
/// checkpoints under.
struct FixtureKeys {
    keys: key_group::ContentKeys,
    workspace: tilo_desktop_model::WorkspaceId,
}

impl FixtureKeys {
    fn open(
        write: &collab_types::SignedOp,
        keys: &key_group::ContentKeys,
    ) -> Result<Vec<u8>, String> {
        let ciphertext =
            collab_types::sealed_ciphertext(write).map_err(|error| error.to_string())?;
        keys.open_content(
            write.artifact,
            &write.encryption.ok_or("not sealed")?,
            &ciphertext,
        )
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
    }
}

impl tilo_peer_state::owner::SealedHistoryKeys for FixtureKeys {
    fn open(
        &self,
        _: tilo_desktop_model::WorkspaceId,
        write: &collab_types::SignedOp,
    ) -> Result<Vec<u8>, String> {
        FixtureKeys::open(write, &self.keys)
    }

    fn seal_checkpoint(
        &self,
        _: tilo_desktop_model::WorkspaceId,
        app: &str,
        record: &str,
        _named: Option<&str>,
        snapshot: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        self.keys
            .seal_content(
                collab_types::document_artifact_id(app, &self.workspace.to_string(), record),
                collab_types::ContentRetention::of_frame(snapshot),
                snapshot,
            )
            .map_err(|error| error.to_string())
    }
}

/// The record's plaintext as `owner` holds it, opened with `keys`.
fn opened(
    owner: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    keys: &key_group::ContentKeys,
) -> AutoCommit {
    let bytes = owner
        .with_current(workspace, |view| {
            let mut opener = |write: &collab_types::SignedOp| FixtureKeys::open(write, keys);
            let held = view
                .peer()
                .publication()
                .opened_document(APP, RECORD, &mut opener)
                .unwrap()
                .expect("the record is held");
            Ok(held.document)
        })
        .unwrap();
    AutoCommit::load(&bytes).unwrap()
}

/// One sealed change setting `key` on `document`, written by `device` as its person.
fn sealed_change(
    owner: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    device: &SigningKey,
    document: &mut AutoCommit,
    key: &str,
    value: &str,
) -> Vec<u8> {
    let invoker = owner
        .with_current(workspace, |view| {
            Ok(testing::invoker_of(
                view.peer(),
                device.verifying_key().to_bytes(),
            ))
        })
        .unwrap();
    let before = document.get_heads();
    document.put(automerge::ROOT, key, value).unwrap();
    document.commit_edit_as("fixture", "topic", invoker);
    document.save_after(&before)
}

/// Write `changes` sealed under `keys` as `device`, founding the record where `founding`.
async fn sealed_write(
    owner: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    device: &SigningKey,
    keys: &key_group::ContentKeys,
    changes: &[u8],
    founding: bool,
) {
    owner
        .write_local(
            workspace,
            device.verifying_key().to_bytes(),
            Arc::new(device.clone()) as Arc<dyn DeviceSigner>,
            LocalWrite {
                app: APP,
                document: RECORD,
                readers: None,
                changes,
                founding,
                snapshot: founding,
                sealed: Some(keys),
                now: tilo_clock::wall(),
            },
        )
        .await
        .expect("the device writes its sealed record");
}

/// Carry every frame `from` keeps to `to`, as a link does.
fn carry(
    from: &NativeWorkspaceOwner,
    to: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    author: [u8; 32],
) {
    for frame in from.frames(workspace) {
        let message = NeighbourMsg::decode(&frame).unwrap();
        to.receive(
            workspace,
            TransportOrigin::ServerDelta(author),
            message,
            tilo_clock::wall(),
        )
        .unwrap();
        from.sent(workspace, &frame).unwrap();
    }
}

/// What a device reads of the record under `keys`: the keys its plaintext holds
/// and how many writes its history still opens.
fn read(
    owner: &NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    keys: &key_group::ContentKeys,
) -> (std::collections::BTreeSet<String>, usize) {
    let document = opened(owner, workspace, keys);
    let typed: std::collections::BTreeSet<String> = document
        .keys(automerge::ROOT)
        .filter(|key| key.starts_with('a') || key.starts_with('b'))
        .collect();
    let history = owner
        .with_current(workspace, |view| {
            let mut opener = |write: &collab_types::SignedOp| FixtureKeys::open(write, keys);
            let opened = view
                .peer()
                .publication()
                .opened_history(APP, RECORD, &mut opener)
                .expect("the record is held");
            Ok(opened.writes.len())
        })
        .unwrap();
    (typed, history)
}

/// Two devices hold one sealed record: one compacts past its bound (in bytes,
/// which a few large writes cross) while offline and the other writes meanwhile;
/// on reconnecting both hold every write and the range the compaction folded is
/// retired on both.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: the offline compaction, the other device's write and their exchange"
)]
async fn two_devices_converge_on_a_sealed_record_one_compacted_offline() {
    // A build's bound is 4096 writes or 16 MiB of sealed history, and a device
    // compacts at three quarters of either; large writes cross the bytes bound
    // in a few seconds, where the write bound would take minutes and run past
    // the freshness an op is admitted within.
    const OFFLINE_WRITES: usize = 8;
    const BIG: usize = 2 * 1024 * 1024;
    let directory = tempfile::tempdir().unwrap();
    let (owner_a, fixture, a_key, b_key) = held(directory.path());
    let workspace = fixture.workspace_id();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    let group = key_group::KeyGroup::create(
        workspace,
        key_group::MemberKey::generate("fixture").unwrap(),
    )
    .unwrap();
    let content = group.content_keys().unwrap();
    for owner in [&owner_a, &owner_b] {
        owner.keep_sealed_histories(Arc::new(FixtureKeys {
            keys: content.clone(),
            workspace,
        }));
    }
    // A founds the record sealed, and B takes its history.
    sealed_write(
        &owner_a,
        workspace,
        &a_key,
        &content,
        &note("founded"),
        true,
    )
    .await;
    carry(
        &owner_a,
        &owner_b,
        workspace,
        a_key.verifying_key().to_bytes(),
    );
    // Offline, A writes past the bound and compacts what it wrote.
    let mut away = opened(&owner_a, workspace, &content);
    let big = "x".repeat(BIG);
    for step in 0..OFFLINE_WRITES {
        let changes = sealed_change(
            &owner_a,
            workspace,
            &a_key,
            &mut away,
            &format!("a{step}"),
            &big,
        );
        sealed_write(&owner_a, workspace, &a_key, &content, &changes, false).await;
    }
    // Meanwhile B writes on the history it holds.
    let mut here = opened(&owner_b, workspace, &content);
    let b_change = sealed_change(&owner_b, workspace, &b_key, &mut here, "b0", "b0");
    sealed_write(&owner_b, workspace, &b_key, &content, &b_change, false).await;
    // A compacted what it wrote while away.
    let kinds: Vec<collab_types::OpKind> = owner_a
        .frames(workspace)
        .iter()
        .filter_map(|frame| match NeighbourMsg::decode(frame) {
            Ok(NeighbourMsg::AppDocument { envelope, .. }) => Some(envelope.kind),
            _ => None,
        })
        .collect();
    assert!(
        kinds.contains(&collab_types::OpKind::Compaction),
        "the device that went away compacted its history: {kinds:?}"
    );
    // They reconnect: each carries what it wrote to the other.
    carry(
        &owner_a,
        &owner_b,
        workspace,
        a_key.verifying_key().to_bytes(),
    );
    carry(
        &owner_b,
        &owner_a,
        workspace,
        b_key.verifying_key().to_bytes(),
    );
    // Each writes once more, which compacts its own copy of the history it took in.
    let late = sealed_change(&owner_a, workspace, &a_key, &mut away, "a-late", "a-late");
    sealed_write(&owner_a, workspace, &a_key, &content, &late, false).await;
    carry(
        &owner_a,
        &owner_b,
        workspace,
        a_key.verifying_key().to_bytes(),
    );
    let b_late = sealed_change(&owner_b, workspace, &b_key, &mut here, "b-late", "b-late");
    sealed_write(&owner_b, workspace, &b_key, &content, &b_late, false).await;
    carry(
        &owner_b,
        &owner_a,
        workspace,
        b_key.verifying_key().to_bytes(),
    );
    // Both read every write, and the range the compaction folded is retired on both.
    let mut every: std::collections::BTreeSet<String> =
        (0..OFFLINE_WRITES).map(|step| format!("a{step}")).collect();
    every.insert("b0".to_owned());
    every.insert("a-late".to_owned());
    every.insert("b-late".to_owned());
    for owner in [&owner_a, &owner_b] {
        let (typed, history) = read(owner, workspace, &content);
        for key in &every {
            assert!(
                typed.contains(key),
                "{key} is held by both devices: {typed:?}"
            );
        }
        assert!(
            history < OFFLINE_WRITES,
            "the compacted range is retired on both: {history}"
        );
    }
}
