//! One case per check of [`admit_workspace_changes`], each admitted and refused on
//! the production function — the same call both receivers make.
use super::*;
use crate::workspace::direct_pair::{DIRECT_PAIRS_KEY, DirectPair, MAX_DIRECT_PAIRS};
use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue};
use ed25519_dalek::SigningKey;
use std::collections::BTreeMap;
use tilo_action_policy::{Role, Subject, roles};
use tilo_user_identity::{DeviceCert, sign_device_cert};

const WORKSPACE: &str = "0b5c3f1e-7d2a-4c1b-9e8f-6a5d4c3b2a19";
const OWNER: [u8; 32] = [0x11; 32];
const EDITOR: [u8; 32] = [0x22; 32];
const VIEWER: [u8; 32] = [0x33; 32];
const NOW: u64 = 1_000;

fn key(byte: u8) -> String {
    hex::encode([byte; 32])
}

fn person_id(root: [u8; 32]) -> String {
    hex::encode(root)
}

/// A workspace the owner governs, with an editor and a viewer.
fn governed() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", WORKSPACE).unwrap();
    doc.put(ROOT, "name", "Workspace").unwrap();
    doc.put(ROOT, "owner", person_id(OWNER)).unwrap();
    let members = doc.put_object(ROOT, "members", ObjType::Map).unwrap();
    for (root, role) in [
        (OWNER, Role::Owner),
        (EDITOR, Role::Editor),
        (VIEWER, Role::Viewer),
    ] {
        doc.put(
            &members,
            person_id(root),
            ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
        )
        .unwrap();
    }
    let grants = doc.put_object(ROOT, "grants", ObjType::Map).unwrap();
    for grant in roles::workspace_role_grants(WORKSPACE, &Subject::Person(person_id(OWNER))) {
        doc.put(
            &grants,
            grant.id.as_str(),
            ScalarValue::Bytes(serde_json::to_vec(&grant).unwrap()),
        )
        .unwrap();
    }
    doc.commit();
    doc
}

/// The changes `edit` makes on top of `before`.
fn changes_after(before: &AutoCommit, edit: impl FnOnce(&mut AutoCommit)) -> Vec<Vec<u8>> {
    // Its own writer: two batches made on one workspace are concurrent changes.
    let mut after = before.clone().with_actor(automerge::ActorId::random());
    let heads = after.get_heads();
    edit(&mut after);
    after.commit();
    after
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

fn all_changes(doc: &AutoCommit) -> Vec<Vec<u8>> {
    doc.clone()
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

fn member(writer_key: &str, root: [u8; 32]) -> Writer<'_> {
    Writer {
        key: writer_key,
        standing: Standing::Member {
            person: root,
            foreign: false,
        },
    }
}

fn admit(
    before: Option<&AutoCommit>,
    source: Writer<'_>,
    owner: &[u8; 32],
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, WorkspaceRefusal> {
    admit_workspace_changes(
        WorkspaceAdmission {
            before,
            workspace: WORKSPACE,
            source,
            owner,
            now_ns: crate::workspace::saved_shortcut::ns_at(NOW),
        },
        changes,
    )
}

/// A record the workspace holds, as any member writes it.
fn hold_record(doc: &mut AutoCommit, record: &str) {
    tilo_desktop_model::projection::project_workspace_records(
        doc,
        &BTreeMap::from([(record.to_owned(), "document".to_owned())]),
    )
    .unwrap();
}

fn take_ownership(doc: &mut AutoCommit, root: [u8; 32]) {
    doc.put(ROOT, "owner", person_id(root)).unwrap();
}

#[test]
fn authority_bootstrap_needs_no_roster_and_others_need_held_governance() {
    let authority = key(0xA0);
    let snapshot = all_changes(&governed());
    let admitted = admit(None, member(&authority, OWNER), &OWNER, &snapshot)
        .expect("the invite's authority supplies the first governance");
    assert_eq!(
        super::super::governance_of(Some(&admitted.candidate)).owner,
        Some(person_id(OWNER))
    );
    let stranger = key(0xB0);
    assert_eq!(
        admit(None, member(&stranger, EDITOR), &OWNER, &snapshot,).unwrap_err(),
        WorkspaceRefusal::GovernanceMissing
    );
}

/// A workspace started over from a base holds the authority it held, so a member
/// writes on the base as before it. An editor writes by the grants every role
/// holds in the workspace, not by the role alone: without them, the role is held
/// and the write is still refused.
#[test]
fn a_base_keeps_the_workspace_s_authority_and_an_editor_writes_on_it() {
    let workspace = governed();
    let based = super::super::base::rebase(&workspace, automerge::ActorId::random()).unwrap();
    assert_eq!(
        tilo_desktop_model::projection::workspace_policy_of(&based, WORKSPACE),
        tilo_desktop_model::projection::workspace_policy_of(&workspace, WORKSPACE)
    );
    let writer = key(0x02);
    let edit = changes_after(&based, |doc| hold_record(doc, "file:/plan.md"));
    assert!(admit(Some(&based), member(&writer, EDITOR), &OWNER, &edit,).is_ok());

    let mut ungranted = workspace;
    ungranted.delete(ROOT, "grants").unwrap();
    ungranted.commit();
    let policy = tilo_desktop_model::projection::workspace_policy_of(&ungranted, WORKSPACE);
    assert_eq!(policy.active_role(&person_id(EDITOR)), Some(Role::Editor));
    let edit = changes_after(&ungranted, |doc| hold_record(doc, "file:/plan.md"));
    assert_eq!(
        admit(Some(&ungranted), member(&writer, EDITOR), &OWNER, &edit,).unwrap_err(),
        WorkspaceRefusal::WriteForbidden
    );
}

#[test]
fn sender_needs_write_on_the_workspace_it_already_governs() {
    let before = governed();
    let edit = changes_after(&before, |doc| hold_record(doc, "file:/plan.md"));
    let writer = key(0x02);
    assert_eq!(
        admit(Some(&before), member(&writer, VIEWER), &OWNER, &edit,).unwrap_err(),
        WorkspaceRefusal::WriteForbidden,
        "a viewer cannot rewrite workspace contents"
    );
    assert_eq!(
        admit(Some(&before), member(&writer, [0x44; 32]), &OWNER, &edit,).unwrap_err(),
        WorkspaceRefusal::WriteForbidden,
        "nor can someone who is not a member"
    );
    assert!(admit(Some(&before), member(&writer, EDITOR), &OWNER, &edit,).is_ok());
    let added = changes_after(&before, |doc| hold_record(doc, "file:/notes.md"));
    assert!(
        admit(
            Some(&before),
            Writer {
                key: &writer,
                standing: Standing::TransportAuthorized,
            },
            &OWNER,
            &added,
        )
        .is_ok(),
        "a transport that already decided Write is not asked again"
    );
}

#[test]
fn malformed_batch_and_foreign_workspace_are_refused() {
    let before = governed();
    let authority = key(0xA0);
    assert!(matches!(
        admit(
            Some(&before),
            member(&authority, OWNER),
            &OWNER,
            &[vec![0x85, 0x6f, 0x4a, 0x83, 0xff]],
        ),
        Err(WorkspaceRefusal::Malformed(_))
    ));
    let moved = changes_after(&before, |doc| {
        doc.put(ROOT, "room", "5b0e8c47-26a1-4f3e-8d9c-1a2b3c4d5e6f")
            .unwrap();
    });
    assert_eq!(
        admit(Some(&before), member(&authority, OWNER), &OWNER, &moved,).unwrap_err(),
        WorkspaceRefusal::DeclaredWorkspaceMismatch
    );
}

#[test]
fn governance_changes_only_from_the_owners_devices() {
    let before = governed();
    let seize = changes_after(&before, |doc| take_ownership(doc, EDITOR));
    let editor = key(0x02);
    let as_editor = member(&editor, EDITOR);
    assert_eq!(
        admit(Some(&before), as_editor, &OWNER, &seize).unwrap_err(),
        WorkspaceRefusal::GovernanceWriter,
        "an editor's governance change is refused"
    );
    for device in [0xA0, 0x05] {
        assert!(
            admit(Some(&before), member(&key(device), OWNER), &OWNER, &seize).is_ok(),
            "any device of the owner may change governance"
        );
    }
    // An echo re-states held governance under the sender's own actor.
    let echo = changes_after(&before, |doc| {
        doc.put(ROOT, "owner", person_id(OWNER)).unwrap();
    });
    assert!(admit(Some(&before), as_editor, &OWNER, &echo).is_ok());
}

#[test]
fn a_first_answer_from_someone_not_the_owner_is_refused() {
    let snapshot = all_changes(&governed());
    let first = key(0xB0);
    assert_eq!(
        admit(
            None,
            Writer {
                key: &first,
                standing: Standing::TransportAuthorized,
            },
            &OWNER,
            &snapshot,
        )
        .unwrap_err(),
        WorkspaceRefusal::GovernanceWriter,
        "answering first does not make a peer the governor"
    );
}

#[test]
fn durable_and_import_governance_count_as_governance() {
    let before = governed();
    let receipts = changes_after(&before, |doc| {
        crate::durability::put(
            doc,
            BTreeMap::from([(
                crate::coverage::route_key("app", "record"),
                durable_receipt(1),
            )]),
        )
        .unwrap();
    });
    let editor = key(0x02);
    assert_eq!(
        admit(Some(&before), member(&editor, EDITOR), &OWNER, &receipts,).unwrap_err(),
        WorkspaceRefusal::GovernanceWriter
    );
}

/// A receipt `actor` recorded for invocation `invocation`, as a member's
/// device records it.
fn effect(invocation: u128, actor: &str) -> tilo_desktop_model::WorkspaceEffectReceipt {
    tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(uuid::Uuid::from_u128(invocation)),
        actor,
        tilo_actions::ActionContext::for_resource("resource", "kind", "Resource"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        1,
    )
}

/// The changes recording `receipts` in `person`'s slice make on `before`.
fn recording(
    before: &AutoCommit,
    person: [u8; 32],
    receipts: &[tilo_desktop_model::WorkspaceEffectReceipt],
) -> Vec<Vec<u8>> {
    changes_after(before, |doc| {
        tilo_desktop_model::projection::project_effect_receipts(doc, &person_id(person), receipts)
            .unwrap();
    })
}

#[test]
fn effect_receipts_are_immutable_even_for_the_governor() {
    let mut before = governed();
    tilo_desktop_model::projection::project_effect_receipts(
        &mut before,
        &person_id(OWNER),
        &[effect(1, &person_id(OWNER))],
    )
    .unwrap();
    before.commit();
    let mut rewritten = effect(1, &person_id(OWNER));
    rewritten.status = tilo_actions::EffectReceiptStatus::Cancelled;
    let rewrite = recording(&before, OWNER, &[rewritten]);
    let authority = key(0xA0);
    assert_eq!(
        admit(Some(&before), member(&authority, OWNER), &OWNER, &rewrite,).unwrap_err(),
        WorkspaceRefusal::ReceiptImmutable
    );
}

#[test]
fn a_member_records_only_their_own_effect_receipts() {
    let before = governed();
    let editor = key(0x02);
    let own = recording(
        &before,
        EDITOR,
        &[effect(1, &person_id(EDITOR)), effect(2, "agent:scribe")],
    );
    assert!(
        admit(Some(&before), member(&editor, EDITOR), &OWNER, &own).is_ok(),
        "their own receipts and their agents', in their own slice"
    );
    for (slice, actor) in [(VIEWER, person_id(EDITOR)), (EDITOR, person_id(VIEWER))] {
        let foreign = recording(&before, slice, &[effect(3, &actor)]);
        assert_eq!(
            admit(Some(&before), member(&editor, EDITOR), &OWNER, &foreign).unwrap_err(),
            WorkspaceRefusal::ReceiptWriter,
            "another's slice, or another's actor"
        );
    }
    let anchor = |person: [u8; 32]| {
        changes_after(&before, |doc| {
            let history = doc
                .put_object(ROOT, "effect_history_root_v1", ObjType::Map)
                .unwrap();
            let slice = doc
                .put_object(&history, person_id(person), ObjType::Map)
                .unwrap();
            doc.put(&slice, key(0x02), ScalarValue::Bytes(vec![7; 32]))
                .unwrap();
        })
    };
    assert!(
        admit(
            Some(&before),
            member(&editor, EDITOR),
            &OWNER,
            &anchor(EDITOR)
        )
        .is_ok()
    );
    assert_eq!(
        admit(
            Some(&before),
            member(&editor, EDITOR),
            &OWNER,
            &anchor(VIEWER)
        )
        .unwrap_err(),
        WorkspaceRefusal::ReceiptWriter,
        "only their own archive's anchor"
    );
    let viewer = key(0x03);
    assert!(
        admit(
            Some(&before),
            member(&viewer, VIEWER),
            &OWNER,
            &recording(&before, VIEWER, &[effect(5, &person_id(VIEWER))]),
        )
        .is_ok(),
        "one who only reads the workspace records their own receipts"
    );
    let relayed = recording(&before, EDITOR, &[effect(4, &person_id(EDITOR))]);
    let authority = key(0xA0);
    assert!(
        admit(Some(&before), member(&authority, OWNER), &OWNER, &relayed).is_ok(),
        "the owner's devices relay what members recorded"
    );
}

/// The grants of `person`'s share `m1` of a note, giving `access`.
fn share_of_note(
    person: [u8; 32],
    access: tilo_action_policy::share::ShareAccess,
) -> Vec<tilo_action_policy::Grant> {
    let note = tilo_action_policy::TreePath::workspace(WORKSPACE)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .any(tilo_action_policy::Level::Resource)
        .item("p1");
    tilo_action_policy::share::share_grants(WORKSPACE, &person_id(person), "m1", &note, access)
        .unwrap()
}

/// The changes writing `grants` into the workspace, and taking `ended` out,
/// make on `before`.
fn regranting(
    before: &AutoCommit,
    grants: &[tilo_action_policy::Grant],
    ended: &[tilo_action_policy::Grant],
) -> Vec<Vec<u8>> {
    changes_after(before, |doc| {
        let (_, map) = doc.get(ROOT, "grants").unwrap().unwrap();
        for grant in grants {
            doc.put(
                &map,
                grant.id.as_str(),
                ScalarValue::Bytes(serde_json::to_vec(grant).unwrap()),
            )
            .unwrap();
        }
        for grant in ended {
            doc.delete(&map, grant.id.as_str()).unwrap();
        }
    })
}

#[test]
fn a_member_shares_only_what_they_hold_and_ends_only_their_own_shares() {
    use tilo_action_policy::share::ShareAccess;
    let before = governed();
    let (editor, viewer) = (key(0x02), key(0x03));
    let edit = share_of_note(EDITOR, ShareAccess::Edit);
    assert!(
        admit(
            Some(&before),
            member(&editor, EDITOR),
            &OWNER,
            &regranting(&before, &edit, &[])
        )
        .is_ok(),
        "an Editor shares a note to edit as theirs"
    );
    assert!(
        matches!(
            admit(
                Some(&before),
                member(&viewer, VIEWER),
                &OWNER,
                &regranting(&before, &share_of_note(VIEWER, ShareAccess::Edit), &[]),
            ),
            Err(WorkspaceRefusal::Membership(_))
        ),
        "a Viewer hands out no edit"
    );
    assert!(
        admit(
            Some(&before),
            member(&viewer, VIEWER),
            &OWNER,
            &regranting(&before, &share_of_note(VIEWER, ShareAccess::View), &[]),
        )
        .is_ok(),
        "but shares what they see"
    );
    assert!(
        matches!(
            admit(
                Some(&before),
                member(&viewer, VIEWER),
                &OWNER,
                &regranting(&before, &edit, &[])
            ),
            Err(WorkspaceRefusal::Membership(_))
        ),
        "nobody shares as someone else"
    );

    let mut shared = before.clone();
    let changes: Vec<_> = regranting(&before, &edit, &[])
        .into_iter()
        .map(|bytes| automerge::Change::from_bytes(bytes).unwrap())
        .collect();
    tilo_automerge::ApplyIncoming::apply_incoming(&mut shared, changes).unwrap();
    assert!(
        matches!(
            admit(
                Some(&shared),
                member(&viewer, VIEWER),
                &OWNER,
                &regranting(&shared, &[], &edit)
            ),
            Err(WorkspaceRefusal::Membership(_))
        ),
        "only its sharer ends a share"
    );
    assert!(
        admit(
            Some(&shared),
            member(&editor, EDITOR),
            &OWNER,
            &regranting(&shared, &[], &edit)
        )
        .is_ok(),
        "its sharer ends it"
    );
}

fn coverage(sequence: u64) -> crate::coverage::Coverage {
    crate::coverage::Coverage {
        workspace: WORKSPACE.into(),
        app: "app".into(),
        record: "record".into(),
        payload_hash: [3; 32],
        heads: vec!["head".into()],
        author: [1; 32],
        sequence,
        watermarks: BTreeMap::new(),
    }
}

fn durable_receipt(generation: u64) -> crate::durability::DurableReceipt {
    crate::durability::DurableReceipt::new(coverage(2), generation).unwrap()
}

#[test]
fn coverage_never_rewinds_even_from_the_governor() {
    let mut before = governed();
    crate::coverage::put(&mut before, coverage(5)).unwrap();
    before.commit();
    let rewind = changes_after(&before, |doc| {
        crate::coverage::put(doc, coverage(4)).unwrap();
    });
    let authority = key(0xA0);
    assert!(matches!(
        admit(Some(&before), member(&authority, OWNER), &OWNER, &rewind,),
        Err(WorkspaceRefusal::Coverage(_))
    ));
}

#[test]
fn durability_never_rewinds_even_from_the_governor() {
    let mut before = governed();
    let route = crate::coverage::route_key("app", "record");
    crate::durability::put(
        &mut before,
        BTreeMap::from([(route.clone(), durable_receipt(7))]),
    )
    .unwrap();
    before.commit();
    let rewind = changes_after(&before, |doc| {
        crate::durability::put(doc, BTreeMap::from([(route, durable_receipt(6))])).unwrap();
    });
    let authority = key(0xA0);
    assert!(matches!(
        admit(Some(&before), member(&authority, OWNER), &OWNER, &rewind,),
        Err(WorkspaceRefusal::Durability(_))
    ));
}

#[test]
fn malformed_media_policy_is_refused_even_from_the_governor() {
    let before = governed();
    let malformed = changes_after(&before, |doc| {
        doc.put(ROOT, media::FIELD, "not bytes").unwrap();
    });
    let authority = key(0xA0);
    assert!(matches!(
        admit(Some(&before), member(&authority, OWNER), &OWNER, &malformed,),
        Err(WorkspaceRefusal::Media(_))
    ));
}

#[test]
fn refusals_keep_the_peer_state_messages() {
    for (refusal, message) in [
        (
            WorkspaceRefusal::GovernanceMissing,
            "workspace governance missing",
        ),
        (
            WorkspaceRefusal::WriteForbidden,
            "workspace write forbidden",
        ),
        (
            WorkspaceRefusal::DeclaredWorkspaceMismatch,
            "declared workspace mismatch",
        ),
        (
            WorkspaceRefusal::GovernanceWriter,
            "governance writer is not the owner's device",
        ),
        (
            WorkspaceRefusal::ReceiptImmutable,
            "effect receipt is immutable",
        ),
        (
            WorkspaceRefusal::ReceiptWriter,
            "effect receipt is not its writer's own",
        ),
    ] {
        assert_eq!(refusal.to_string(), message);
    }
}

/// The changes saving `shortcut` makes on top of `before`.
fn saving(before: &AutoCommit, shortcut: &tilo_automation::Shortcut) -> Vec<Vec<u8>> {
    changes_after(before, |doc| {
        tilo_desktop_model::projection::project_workspace_shortcuts(
            doc,
            &[tilo_automation::SavedShortcut::encode(shortcut)],
        )
        .unwrap();
    })
}

/// A shortcut saved from a device whose certificate was issued earlier in
/// the second it arrives in is proven: admission judges at the instant, not
/// at the start of its second.
#[test]
fn a_shortcut_saved_with_a_certificate_issued_this_second_is_proven() {
    use crate::workspace::saved_shortcut::tests::{NOW_NS, device, shortcut};
    let before = governed();
    let writer = key(0x02);
    let mut editor = device(0x22, 0x02);
    editor.cert.issued_at_ns = NOW_NS + 400_000_000;
    tilo_user_identity::sign_device_cert(&mut editor.cert, &editor.root).unwrap();
    let batch = saving(&before, &editor.saves(WORKSPACE, shortcut("water")));
    admit_workspace_changes(
        WorkspaceAdmission {
            before: Some(&before),
            workspace: WORKSPACE,
            source: member(&writer, EDITOR),
            owner: &OWNER,
            now_ns: NOW_NS + 700_000_000,
        },
        &batch,
    )
    .expect("the certificate is valid at the instant the save arrives");
}

#[test]
fn a_shortcut_names_as_its_saver_only_whose_device_signed_it() {
    use crate::workspace::saved_shortcut::tests::{device, shortcut};
    assert_eq!(
        crate::workspace::saved_shortcut::ns_at(NOW),
        crate::workspace::saved_shortcut::tests::NOW_NS
    );
    let before = governed();
    let authority = key(0xA0);
    let writer = key(0x02);
    let owner = device(0x11, 0xA0);
    let editor = device(0x22, 0x02);
    let governor = &OWNER;
    let as_editor = || member(&writer, EDITOR);
    let as_governor = || member(&authority, OWNER);

    // The editor's device names the owner as who saved it.
    let mut forged = editor.saves(WORKSPACE, shortcut("water"));
    forged.saved_by = Some(owner.person());
    let mut unproven = shortcut("water");
    unproven.saved_by = Some(owner.person());
    for batch in [saving(&before, &forged), saving(&before, &unproven)] {
        assert_eq!(
            admit(Some(&before), as_editor(), governor, &batch).unwrap_err(),
            WorkspaceRefusal::ShortcutAuthorship("water".into()),
            "a member cannot save a shortcut as another person"
        );
    }

    // The governor is no exception: it names the editor without the
    // editor's device having signed.
    let mut forged = owner.saves(WORKSPACE, shortcut("ferns"));
    forged.saved_by = Some(editor.person());
    let mut unproven = shortcut("ferns");
    unproven.saved_by = Some(editor.person());
    for batch in [saving(&before, &forged), saving(&before, &unproven)] {
        assert_eq!(
            admit(Some(&before), as_governor(), governor, &batch).unwrap_err(),
            WorkspaceRefusal::ShortcutAuthorship("ferns".into()),
            "the governor cannot save a shortcut as a member"
        );
    }

    // A proof from another workspace does not move with the shortcut.
    let copied = saving(
        &before,
        &editor.saves("5e1d9a40-3c2b-4f6e-8a7d-1b2c3d4e5f60", shortcut("lamp")),
    );
    assert_eq!(
        admit(Some(&before), as_editor(), governor, &copied).unwrap_err(),
        WorkspaceRefusal::ShortcutAuthorship("lamp".into()),
    );

    // The person's own device saves it; the governor relaying that change is
    // judged by the device that signed it, not by itself.
    let own = saving(&before, &editor.saves(WORKSPACE, shortcut("water")));
    admit(Some(&before), as_editor(), governor, &own)
        .expect("the editor's own device saves as the editor");
    let relayed = admit(Some(&before), as_governor(), governor, &own)
        .expect("a relayed member change is judged by its signer")
        .candidate;
    assert!(
        admit(
            Some(&before),
            as_editor(),
            governor,
            &saving(&before, &shortcut("nobody")),
        )
        .is_ok(),
        "a shortcut that names no saver arms nowhere"
    );

    // A shortcut held unchanged, even one saved before proofs existed, is
    // not written by an unrelated edit.
    let mut legacy = shortcut("lights");
    legacy.saved_by = Some(owner.person());
    let mut held = relayed;
    tilo_desktop_model::projection::project_workspace_shortcuts(
        &mut held,
        &[
            tilo_automation::SavedShortcut::encode(&editor.saves(WORKSPACE, shortcut("water"))),
            tilo_automation::SavedShortcut::encode(&legacy),
        ],
    )
    .unwrap();
    held.commit();
    let edit = changes_after(&held, |doc| hold_record(doc, "file:/plan.md"));
    assert!(
        admit(Some(&held), as_editor(), governor, &edit).is_ok(),
        "a shortcut held unchanged is not a write"
    );
}

const ADMIN: [u8; 32] = [0x55; 32];
const NEWCOMER: [u8; 32] = [0x66; 32];

fn put_role(doc: &mut AutoCommit, person: [u8; 32], role: Role) {
    let (_, members) = doc.get(ROOT, "members").unwrap().unwrap();
    doc.put(
        &members,
        person_id(person),
        ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
    )
    .unwrap();
}

fn put_grant(doc: &mut AutoCommit, grant: &tilo_action_policy::Grant) {
    let (_, grants) = doc.get(ROOT, "grants").unwrap().unwrap();
    doc.put(
        &grants,
        grant.id.as_str(),
        ScalarValue::Bytes(serde_json::to_vec(grant).unwrap()),
    )
    .unwrap();
}

fn drop_grant(doc: &mut AutoCommit, id: &str) {
    let (_, grants) = doc.get(ROOT, "grants").unwrap().unwrap();
    doc.delete(&grants, id).unwrap();
}

/// [`governed`] with an editor the owner made an administrator.
fn with_administrator() -> AutoCommit {
    let mut doc = governed();
    put_role(&mut doc, ADMIN, Role::Editor);
    put_grant(
        &mut doc,
        &roles::administrator_grant(
            WORKSPACE,
            &person_id(ADMIN),
            Subject::Person(person_id(OWNER)),
        ),
    );
    doc.commit();
    doc
}

#[test]
fn an_administrator_admits_a_member_and_an_editor_cannot() {
    let before = with_administrator();
    let admit_newcomer = changes_after(&before, |doc| put_role(doc, NEWCOMER, Role::Commenter));
    let admitted = admit(
        Some(&before),
        member(&key(0x05), ADMIN),
        &OWNER,
        &admit_newcomer,
    )
    .expect("an administrator admits a member");
    assert_eq!(
        tilo_desktop_model::projection::workspace_policy_of(&admitted.candidate, WORKSPACE)
            .role_of(&person_id(NEWCOMER)),
        Some(Role::Commenter)
    );
    assert!(matches!(
        admit(
            Some(&before),
            member(&key(0x02), EDITOR),
            &OWNER,
            &admit_newcomer
        ),
        Err(WorkspaceRefusal::Membership(_))
    ));
}

#[test]
fn an_administrator_writes_membership_only() {
    let before = with_administrator();
    let rename = changes_after(&before, |doc| take_ownership(doc, ADMIN));
    assert_eq!(
        admit(Some(&before), member(&key(0x05), ADMIN), &OWNER, &rename).unwrap_err(),
        WorkspaceRefusal::GovernanceWriter
    );
    let above = changes_after(&before, |doc| put_role(doc, NEWCOMER, Role::Owner));
    assert!(admit(Some(&before), member(&key(0x05), ADMIN), &OWNER, &above).is_err());
}

/// A change is judged where its writer made it: one made before the
/// writer's grant was taken back stands wherever it arrives, one made after
/// does not.
#[test]
fn a_membership_change_is_judged_where_its_writer_made_it() {
    let base = with_administrator();
    let admin_id = roles::administrator_grant_id(&person_id(ADMIN));
    let revoked = {
        let mut doc = base.clone();
        drop_grant(&mut doc, admin_id.as_str());
        doc.commit();
        doc
    };
    // Made on the workspace before the revocation, received after it.
    let concurrent = changes_after(&base, |doc| put_role(doc, NEWCOMER, Role::Viewer));
    assert!(
        admit(
            Some(&revoked),
            member(&key(0x05), ADMIN),
            &OWNER,
            &concurrent
        )
        .is_ok(),
        "made while the writer held the grant"
    );
    // Made after the revocation reached the writer.
    let later = changes_after(&revoked, |doc| put_role(doc, NEWCOMER, Role::Viewer));
    assert!(matches!(
        admit(Some(&revoked), member(&key(0x05), ADMIN), &OWNER, &later),
        Err(WorkspaceRefusal::Membership(_))
    ));
}

/// A person's root key and the id their people are known by.
fn person(seed: u8) -> (SigningKey, String) {
    let root = SigningKey::from_bytes(&[seed; 32]);
    let id = hex::encode(root.verifying_key().to_bytes());
    (root, id)
}

/// A device of `root`, signing with `seed`.
fn device(root: &SigningKey, seed: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// [`governed`], with `members` admitted beside the owner, the owner's role
/// grants as its only grants.
fn governed_by(members: &[(String, Role)]) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", WORKSPACE).unwrap();
    doc.put(ROOT, "name", "Workspace").unwrap();
    doc.put(ROOT, "owner", person_id(OWNER)).unwrap();
    let map = doc.put_object(ROOT, "members", ObjType::Map).unwrap();
    doc.put(
        &map,
        person_id(OWNER),
        ScalarValue::Bytes(serde_json::to_vec(&Role::Owner).unwrap()),
    )
    .unwrap();
    for (person, role) in members {
        doc.put(
            &map,
            person.as_str(),
            ScalarValue::Bytes(serde_json::to_vec(role).unwrap()),
        )
        .unwrap();
    }
    let grants = doc.put_object(ROOT, "grants", ObjType::Map).unwrap();
    for grant in roles::workspace_role_grants(WORKSPACE, &Subject::Person(person_id(OWNER))) {
        doc.put(
            &grants,
            grant.id.as_str(),
            ScalarValue::Bytes(serde_json::to_vec(&grant).unwrap()),
        )
        .unwrap();
    }
    doc.commit();
    doc
}

/// `doc`'s pair map, created when it holds none.
fn pair_map(doc: &mut AutoCommit) -> automerge::ObjId {
    match doc.get(ROOT, DIRECT_PAIRS_KEY).unwrap() {
        Some((automerge::Value::Object(ObjType::Map), map)) => map,
        Some(_) => panic!("the fixture's pairs are a map"),
        None => doc
            .put_object(ROOT, DIRECT_PAIRS_KEY, ObjType::Map)
            .unwrap(),
    }
}

fn hold_pair(doc: &mut AutoCommit, pair: &str, announcement: &DirectPair) {
    let map = pair_map(doc);
    doc.put(
        &map,
        pair,
        ScalarValue::Bytes(serde_json::to_vec(announcement).unwrap()),
    )
    .unwrap();
}

/// The editor's and viewer's roots, their ids, and their devices.
fn two_people() -> (
    (SigningKey, String),
    (SigningKey, String),
    (SigningKey, DeviceCert),
) {
    let editor = person(0x44);
    let viewer = person(0x55);
    let (editor_key, editor_cert) = device(&editor.0, 0x46);
    (editor, viewer, (editor_key, editor_cert))
}

#[test]
fn a_pair_a_device_inside_it_proved_is_admitted() {
    let (editor, viewer, (editor_key, editor_cert)) = two_people();
    let before = governed_by(&[
        (editor.1.clone(), Role::Editor),
        (viewer.1.clone(), Role::Viewer),
    ]);
    let (pair, announcement) = DirectPair::signed(
        WORKSPACE,
        [editor.1.clone(), viewer.1],
        &editor_cert,
        &editor_key,
    )
    .unwrap();
    let added = changes_after(&before, |doc| hold_pair(doc, &pair, &announcement));
    let admitted = admit(
        Some(&before),
        member(&editor.1, editor.0.verifying_key().to_bytes()),
        &OWNER,
        &added,
    )
    .expect("a pair one of its two people's devices proved is admitted");
    assert!(
        admitted
            .candidate
            .get(ROOT, DIRECT_PAIRS_KEY)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_pair_no_one_in_it_proved_is_refused() {
    let (editor, viewer, (editor_key, editor_cert)) = two_people();
    let before = governed_by(&[
        (editor.1.clone(), Role::Editor),
        (viewer.1.clone(), Role::Viewer),
    ]);
    let writer = || member(&editor.1, editor.0.verifying_key().to_bytes());
    // The device that signs belongs to the owner, who is not in the pair: the
    // proof chains to a root the pair is not between.
    let (owner_key, owner_cert) = device(&SigningKey::from_bytes(&[0x11; 32]), 0x12);
    let (pair, forged) = DirectPair::signed(
        WORKSPACE,
        [editor.1.clone(), viewer.1.clone()],
        &owner_cert,
        &owner_key,
    )
    .unwrap();
    let changes = changes_after(&before, |doc| hold_pair(doc, &pair, &forged));
    assert_eq!(
        admit(Some(&before), writer(), &OWNER, &changes).unwrap_err(),
        WorkspaceRefusal::DirectPairProof(pair.clone()),
        "a pair is proven by a device of one of its two people"
    );
    // A pair its two people's device signed, stored under another pair's id.
    let (_, on_another) = DirectPair::signed(
        WORKSPACE,
        [editor.1.clone(), person_id(OWNER)],
        &editor_cert,
        &editor_key,
    )
    .unwrap();
    let changes = changes_after(&before, |doc| hold_pair(doc, &pair, &on_another));
    assert_eq!(
        admit(Some(&before), writer(), &OWNER, &changes).unwrap_err(),
        WorkspaceRefusal::DirectPairProof(pair.clone()),
        "a pair's id is the hash of its people"
    );
    let (_, announcement) = DirectPair::signed(
        WORKSPACE,
        [editor.1.clone(), viewer.1.clone()],
        &editor_cert,
        &editor_key,
    )
    .unwrap();
    let mut held = before;
    hold_pair(&mut held, &pair, &announcement);
    held.commit();
    // Removed, a pair the workspace holds is no pair any more: refused.
    let removal = changes_after(&held, |doc| {
        let map = pair_map(doc);
        doc.delete(&map, pair.as_str()).unwrap();
    });
    assert_eq!(
        admit(Some(&held), writer(), &OWNER, &removal).unwrap_err(),
        WorkspaceRefusal::DirectPairImmutable(pair.clone())
    );
    // Changed, its proof is no longer the one the workspace holds: refused.
    let (_, other_device) = device(&editor.0, 0x47);
    let (_, resigned) = DirectPair::signed(
        WORKSPACE,
        [editor.1.clone(), viewer.1],
        &other_device,
        &SigningKey::from_bytes(&[0x47; 32]),
    )
    .unwrap();
    let rewrite = changes_after(&held, |doc| hold_pair(doc, &pair, &resigned));
    assert_eq!(
        admit(Some(&held), writer(), &OWNER, &rewrite).unwrap_err(),
        WorkspaceRefusal::DirectPairImmutable(pair.clone())
    );
    // Left as it is, it passes.
    let untouched = changes_after(&held, |doc| hold_pair(doc, &pair, &announcement));
    admit(Some(&held), writer(), &OWNER, &untouched)
        .expect("a pair the workspace holds is relayed unchanged");
}

#[test]
fn a_workspace_holds_no_more_pairs_than_it_can() {
    let (editor, viewer, (editor_key, editor_cert)) = two_people();
    let mut before = governed_by(&[
        (editor.1.clone(), Role::Editor),
        (viewer.1.clone(), Role::Viewer),
    ]);
    let map = pair_map(&mut before);
    for index in 0..MAX_DIRECT_PAIRS {
        before
            .put(
                &map,
                format!("direct_{index:064x}"),
                ScalarValue::Bytes(b"held".to_vec()),
            )
            .unwrap();
    }
    before.commit();
    let (pair, announcement) = DirectPair::signed(
        WORKSPACE,
        [editor.1.clone(), viewer.1],
        &editor_cert,
        &editor_key,
    )
    .unwrap();
    let changes = changes_after(&before, |doc| hold_pair(doc, &pair, &announcement));
    assert!(matches!(
        admit(
            Some(&before),
            member(&editor.1, editor.0.verifying_key().to_bytes()),
            &OWNER,
            &changes
        ),
        Err(WorkspaceRefusal::DirectPairProof(why)) if why.contains("as many pairs")
    ));
}
