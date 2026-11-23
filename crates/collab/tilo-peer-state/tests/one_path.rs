//! The one path a device's writes take: admitted into its own replica, kept as
//! a frame until a link carries it, and durable across a restart.

use automerge::{AutoCommit, ROOT, transaction::Transactable};
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
    let fixture = TestWorkspace::new();
    let fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    let fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
    (owner_a, fixture, a_key, b_key)
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
    assert!(
        owner_b
            .with_current(workspace, |view| Ok(view
                .peer()
                .holds_document(APP, RECORD)))
            .unwrap_or(false),
        "the joiner holds the workspace it was admitted into"
    );
    write(&owner_b, workspace, &b_key).await;
    assert!(
        owner_a.with_current(workspace, |view| Ok(view
            .peer()
            .holds_document(APP, RECORD)))
            == Ok(true)
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

/// A record an earlier build wrote opens here: its workspaces are listed and
/// their records read, through the layout this build brings up.
#[test]
fn a_record_an_earlier_build_wrote_opens() {
    const FOUNDERS: &str = "40267648-f762-4c8d-89ad-c5ab27aa13b8";
    const GOLDEN: &str = include_str!("../golden/owner-workspace-document/v7.hex");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owner.json");
    let parts = path.with_extension("workspaces");
    std::fs::create_dir_all(&parts).unwrap();
    let bytes: Vec<u8> = GOLDEN
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect();
    std::fs::write(parts.join(format!("{FOUNDERS}.1.json")), &bytes).unwrap();
    std::fs::write(
        parts.join("index.json"),
        format!(r#"{{"version":2,"revision":1,"held":{{"{FOUNDERS}":1}}}}"#),
    )
    .unwrap();
    let owner = open_file(&path).unwrap();
    let workspace = tilo_desktop_model::WorkspaceId::parse(FOUNDERS).unwrap();
    assert!(owner.contains_workspace(workspace) || owner.workspaces().contains(&workspace));
    let read = owner
        .with_current(workspace, |view| {
            Ok((
                view.peer().rows().len(),
                view.peer().certificates().len(),
                view.device().device_public_key,
            ))
        })
        .expect("the record an earlier build wrote reads");
    assert!(read.0 > 0 || read.1 > 0, "its workspace is there");
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
    let mut fixture = TestWorkspace::new();
    let workspace = fixture.workspace_id();
    fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    write(&owner_a, workspace, &a_key).await;
    // The device answering the join has the history the joiner is sent.
    for frame in owner_a.frames(workspace) {
        let message = NeighbourMsg::decode(&frame).unwrap();
        fixture
            .receive(
                TransportOrigin::ServerDelta(a_key.verifying_key().to_bytes()),
                message,
            )
            .unwrap();
    }
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
    let mut other = receipt.clone();
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
    let fixture = TestWorkspace::new();
    let fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    let fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
    (owner_a, fixture, a_key, b_key, a_cert)
}

/// Two devices write different role changes while offline, and the same
/// membership is what they read once they exchange them.
#[tokio::test]
async fn two_devices_offline_role_changes_converge() {
    let directory = tempfile::tempdir().unwrap();
    let root = key(1);
    let a_key = key(2);
    let b_key = key(3);
    let a_cert = testing::certificate(&root, &a_key, "A");
    let b_cert = testing::certificate(&root, &b_key, "B");
    let owner_a = open_file(&directory.path().join("a.json")).unwrap();
    let owner_b = open_file(&directory.path().join("b.json")).unwrap();
    let fixture = TestWorkspace::new();
    let workspace = fixture.workspace_id();
    let fixture = testing::enroll_into(fixture, &owner_a, &a_key, &a_cert).unwrap();
    let _fixture = testing::enroll_into(fixture, &owner_b, &b_key, &b_cert).unwrap();
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
