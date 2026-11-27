//! A member given one app of a workspace reads that app's records, is handed the keys of them
//! alone, and keeps no change of another app a peer offers it: the one place a device's reach
//! comes from narrows to the parts its person holds.
use super::*;
use crate::fixture::{
    Founding, WORKSPACE, at, caught_up, certify, key, origin, record, relay, sealed,
};
use collab_types::CommitNamed as _;
use tilo_action_policy::Change;
use tilo_user_identity::DeviceCapabilities;

const FILES: &str = "so.tilo.files";
const NOTES: &str = "so.tilo.notes";
const TREE: &str = "tree";
const NOTE: &str = "minutes";
/// The folder's words, and the note's: the note's are what a device outside its part must never
/// keep.
const FILE_WORDS: &str = "Plan.md";
const NOTE_WORDS: &str = "minutes the member may not read";

/// What the owner offers the member's device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Offer {
    Folder,
    /// A note of another app, sealed under the note's own group.
    Note,
    /// A file written into the folder after the share was taken back.
    FileAfterTheShare,
}

/// What the member's device decides of an offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Keeps,
    Drops,
}

/// One case: whether the share has been taken back, what is offered, and what the device decides.
const CASES: [(bool, Offer, Verdict); 3] = [
    (false, Offer::Folder, Verdict::Keeps),
    (false, Offer::Note, Verdict::Drops),
    (true, Offer::FileAfterTheShare, Verdict::Drops),
];

/// The part of the workspace the Files are, as the share and its end both name it.
fn files_share() -> TreePath {
    TreePath::workspace(WORKSPACE).app(FILES).rest()
}

/// `grantee` given `role` over `scope`, or its role taken back.
fn grant(grantee: Grantee, scope: TreePath, role: Option<Role>) -> Change {
    Change::SetScopedRole {
        grantee,
        scope,
        role,
    }
}

/// The changes `doc` holds since `before`, encoded.
fn changes(doc: &mut AutoCommit, before: &[automerge::ChangeHash]) -> Vec<Vec<u8>> {
    doc.get_changes(before)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// The owner's and the member's devices, holding the workspace and the folder, before or after
/// the share is taken back.
struct Scoped {
    host: StatePeer,
    editor: StatePeer,
    owner: SigningKey,
    editor_key: SigningKey,
    editor_cert: DeviceCert,
    now: SystemTime,
}

fn scoped(taken_back: bool) -> Scoped {
    let (owner, owner_cert) = certify(&key(1), 2, DeviceCapabilities::member());
    let (editor_key, editor_cert) = certify(&key(110), 111, DeviceCapabilities::member());
    let by = (owner.clone(), owner_cert.clone());
    let founding = Founding::by(&(owner.clone(), owner_cert.clone()))
        .named(WORKSPACE)
        .with([&editor_cert])
        .seeding_in(FILES, TREE, record(FILE_WORDS));
    // The workspace the share makes it, and, taken back, the change that ends it.
    let mut workspace = workspace::candidate(None, &founding.seed().workspace_changes).unwrap();
    let person = hex::encode(editor_cert.user_public_key);
    let shares = files_share();
    // The share, and another app's part given to everyone: a share with everyone widens nobody.
    let parts = vec![
        grant(
            Grantee::person(person.clone()),
            shares.clone(),
            Some(Role::Editor),
        ),
        grant(
            Grantee::role(Role::Viewer),
            TreePath::workspace(WORKSPACE).app(NOTES).rest(),
            Some(Role::Editor),
        ),
    ];
    crate::members::op::tests_support::sign_into(&mut workspace, &by, 1_000, parts);
    let mut seed = founding.seed();
    seed.workspace_changes = changes(&mut workspace, &[]);
    let removal = taken_back.then(|| {
        let before = workspace.get_heads();
        crate::members::op::tests_support::sign_into(
            &mut workspace,
            &by,
            2_000,
            vec![grant(Grantee::person(person), shares, None)],
        );
        changes(&mut workspace, &before)
    });
    let mut host = founding.empty();
    let now = at();
    host.install_seed(seed, &owner, now)
        .expect("the workspace's host holds it");
    host.write_as_device(owner_cert.device_public_key);
    let mut editor = caught_up(&mut host, &founding);
    editor.write_as_device(editor_cert.device_public_key);
    if let Some(changes) = removal {
        let taken = NeighbourMsg::SyncResp {
            changes,
            workspace: WORKSPACE.into(),
        };
        for peer in [&mut host, &mut editor] {
            relay(peer, &owner, std::slice::from_ref(&taken), now);
        }
    }
    Scoped {
        host,
        editor,
        owner,
        editor_key,
        editor_cert,
        now,
    }
}

/// The note the owner writes, sealed under its own group: what a device outside the note's part
/// is offered, and drops before either opens it.
fn sealed_note(f: &Scoped) -> NeighbourMsg {
    let scope = neighbour_wire::KeyScope::document(
        collab_types::WorkspaceId::parse(WORKSPACE).unwrap(),
        NOTES,
        NOTE,
        None,
    )
    .unwrap();
    let group = key_group::KeyGroup::create(
        scope.group(),
        key_group::MemberKey::generate("owner").unwrap(),
    )
    .expect("the note's own group");
    sealed(
        &group.content_keys().unwrap(),
        (WORKSPACE, NOTES, NOTE),
        &f.owner,
        &record(NOTE_WORDS),
        1,
        f.now,
    )
}

/// The owner offers `offer` to the member's device, and answers what it decided.
fn offered(f: &mut Scoped, offer: Offer) -> AdmissionReport {
    let message = match offer {
        Offer::Note => sealed_note(f),
        Offer::Folder | Offer::FileAfterTheShare => {
            let mut document = AutoCommit::load(&f.host.document(FILES, TREE).unwrap()).unwrap();
            let heads = document.get_heads();
            document.put(ROOT, "also", FILE_WORDS).unwrap();
            document.commit_edit(
                "fixture",
                "also",
                &crate::fixture::person(&f.host, &f.owner),
            );
            f.host
                .publication_mut()
                .publish_incremental(FILES, TREE, document.save_after(&heads), &f.owner, f.now)
                .expect("the change is the owner's to make")
        }
    };
    f.editor
        .receive_from_report(origin(&f.owner), message, f.now)
        .expect("an offer is no error of the device's own")
}

/// Whether `bytes`, what a device keeps, holds `words` anywhere: an app document's content is
/// part of what a device persists once it keeps a change of it.
fn keeps(bytes: &[u8], words: &str) -> bool {
    bytes
        .windows(words.len())
        .any(|window| window == words.as_bytes())
}

#[test]
fn a_member_given_one_app_reads_and_is_handed_the_keys_of_that_app_alone() {
    let f = scoped(false);
    let editor = f.editor_key.verifying_key().to_bytes();
    let policy = f.host.governance().member_policy().unwrap();
    assert_eq!(
        policy.member_scope(&hex::encode(f.editor_cert.user_public_key)),
        Some(vec![&files_share()]),
        "the member holds the Files app alone of the workspace"
    );
    // Each record, and whether the member's device reads it and is handed its keys: its part's,
    // and neither for another app's record shared with everyone.
    for ((app, id), reads) in [((FILES, TREE), true), ((NOTES, NOTE), false)] {
        assert_eq!(
            f.host.check_document_read(editor, app, id, f.now).is_ok(),
            reads,
            "the member reads {app}/{id} or not, as its part says"
        );
        assert_eq!(
            f.host
                .document_key_holders(app, id, f.now)
                .unwrap()
                .contains(&editor),
            reads,
            "the member's device is handed {app}/{id}'s keys or not, as its part says"
        );
    }
}

#[test]
fn a_members_device_keeps_its_part_and_drops_a_change_outside_it() {
    for (taken_back, offer, verdict) in CASES {
        let mut f = scoped(taken_back);
        let report = offered(&mut f, offer);
        let kept = verdict == Verdict::Keeps;
        assert_eq!(
            matches!(report.primary(), crate::InputDisposition::Accepted(_)),
            kept,
            "{offer:?}: taken in only for the part it holds"
        );
        if kept {
            let held = f.editor.document(FILES, TREE).expect("the folder it holds");
            assert!(keeps(&held, FILE_WORDS), "{offer:?}: it keeps the file");
            continue;
        }
        assert_eq!(
            report.primary(),
            &crate::InputDisposition::OutsidePart,
            "{offer:?}: nothing of a record outside its person's part"
        );
        let [dropped] = report.summary().dropped.as_slice() else {
            panic!(
                "{offer:?}: the dropped change is named: {:?}",
                report.summary()
            );
        };
        assert_eq!(
            dropped.reason,
            crate::admission::DropReason::OutsidePart,
            "{offer:?}: it says why it kept none of it"
        );
        assert!(
            dropped.to_string().contains("does not read"),
            "{offer:?}: the reason as a person reads it: {dropped}"
        );
        if offer == Offer::Note {
            assert!(
                f.editor.document(NOTES, NOTE).is_none(),
                "{offer:?}: it keeps no record of a note"
            );
        }
        let kept_bytes = f.editor.durable().unwrap();
        assert!(
            !keeps(&kept_bytes, NOTE_WORDS),
            "{offer:?}: none of the note's words are among what it keeps"
        );
        assert!(
            keeps(&kept_bytes, FILE_WORDS),
            "{offer:?}: the folder's file is among what it keeps"
        );
    }
}

#[test]
fn taking_the_last_part_back_takes_the_members_device_out_of_the_keys() {
    let f = scoped(true);
    let editor = f.editor_key.verifying_key().to_bytes();
    let policy = f.host.governance().member_policy().unwrap();
    assert!(
        policy
            .member_scope(&hex::encode(f.editor_cert.user_public_key))
            .is_some_and(|scope| scope.is_empty()),
        "the member holds nothing once their last part is taken back"
    );
    // Losing the last part is leaving: the device leaves every group of the workspace, and the
    // commit that follows rotates the Files group's keys it was in.
    assert!(
        !f.host
            .governance()
            .key_holders(f.now)
            .unwrap()
            .contains(&editor),
        "the member's device holds none of the workspace's keys"
    );
    assert!(
        !f.host
            .document_key_holders(FILES, TREE, f.now)
            .unwrap()
            .contains(&editor),
        "the Files group's keys move past the member's device"
    );
    // Opening the folder is an error now, not a record it lacks.
    assert!(
        f.editor
            .document_opens(editor, (FILES, TREE), f.now, false)
            .is_err(),
        "the folder is no longer the member's to open"
    );
}
