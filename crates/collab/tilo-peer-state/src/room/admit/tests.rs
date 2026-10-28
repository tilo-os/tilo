//! One case per check of [`admit_room_changes`], each admitted and refused on
//! the production function — the same call both receivers make.
use super::*;
use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue};
use std::collections::BTreeMap;
use tilo_action_policy::{Role, Subject, roles};

const ROOM: &str = "0b5c3f1e-7d2a-4c1b-9e8f-6a5d4c3b2a19";
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

/// A room the owner governs, with an editor and a viewer.
fn governed() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", ROOM).unwrap();
    doc.put(ROOT, "name", "Room").unwrap();
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
    for grant in roles::room_role_grants(ROOM, &Subject::Person(person_id(OWNER))) {
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
    let mut after = before.clone();
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
    authority: AuthorityPin<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedRoom, RoomRefusal> {
    admit_room_changes(
        RoomAdmission {
            before,
            room: ROOM,
            source,
            authority,
            now_ns: crate::room::saved_shortcut::ns_at(NOW),
        },
        changes,
    )
}

/// A record the room holds, as any member writes it.
fn hold_record(doc: &mut AutoCommit, record: &str) {
    tilo_desktop_model::projection::project_room_records(
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
    let admitted = admit(
        None,
        member(&authority, OWNER),
        AuthorityPin::Device(&authority),
        &snapshot,
    )
    .expect("the invite's authority supplies the first governance");
    assert_eq!(
        super::super::governance_of(Some(&admitted.candidate)).owner,
        Some(person_id(OWNER))
    );
    let stranger = key(0xB0);
    assert_eq!(
        admit(
            None,
            member(&stranger, EDITOR),
            AuthorityPin::Device(&authority),
            &snapshot,
        )
        .unwrap_err(),
        RoomRefusal::GovernanceMissing
    );
}

/// A room started over from a base holds the authority it held, so a member
/// writes on the base as before it. An editor writes by the grants every role
/// holds in the room, not by the role alone: without them, the role is held
/// and the write is still refused.
#[test]
fn a_base_keeps_the_room_s_authority_and_an_editor_writes_on_it() {
    let room = governed();
    let based = super::super::base::rebase(&room, automerge::ActorId::random()).unwrap();
    assert_eq!(
        tilo_desktop_model::projection::room_policy_of(&based, ROOM),
        tilo_desktop_model::projection::room_policy_of(&room, ROOM)
    );
    let authority = key(0xA0);
    let writer = key(0x02);
    let edit = changes_after(&based, |doc| hold_record(doc, "file:/plan.md"));
    assert!(
        admit(
            Some(&based),
            member(&writer, EDITOR),
            AuthorityPin::Device(&authority),
            &edit,
        )
        .is_ok()
    );

    let mut ungranted = room;
    ungranted.delete(ROOT, "grants").unwrap();
    ungranted.commit();
    let policy = tilo_desktop_model::projection::room_policy_of(&ungranted, ROOM);
    assert_eq!(policy.active_role(&person_id(EDITOR)), Some(Role::Editor));
    let edit = changes_after(&ungranted, |doc| hold_record(doc, "file:/plan.md"));
    assert_eq!(
        admit(
            Some(&ungranted),
            member(&writer, EDITOR),
            AuthorityPin::Device(&authority),
            &edit,
        )
        .unwrap_err(),
        RoomRefusal::WriteForbidden
    );
}

#[test]
fn sender_needs_write_on_the_room_it_already_governs() {
    let before = governed();
    let edit = changes_after(&before, |doc| hold_record(doc, "file:/plan.md"));
    let authority = key(0xA0);
    let writer = key(0x02);
    assert_eq!(
        admit(
            Some(&before),
            member(&writer, VIEWER),
            AuthorityPin::Device(&authority),
            &edit,
        )
        .unwrap_err(),
        RoomRefusal::WriteForbidden,
        "a viewer cannot rewrite room contents"
    );
    assert_eq!(
        admit(
            Some(&before),
            member(&writer, [0x44; 32]),
            AuthorityPin::Device(&authority),
            &edit,
        )
        .unwrap_err(),
        RoomRefusal::WriteForbidden,
        "nor can someone who is not a member"
    );
    assert!(
        admit(
            Some(&before),
            member(&writer, EDITOR),
            AuthorityPin::Device(&authority),
            &edit,
        )
        .is_ok()
    );
    let added = changes_after(&before, |doc| hold_record(doc, "file:/notes.md"));
    assert!(
        admit(
            Some(&before),
            Writer {
                key: &writer,
                standing: Standing::TransportAuthorized,
            },
            AuthorityPin::Device(&authority),
            &added,
        )
        .is_ok(),
        "a transport that already decided Write is not asked again"
    );
}

#[test]
fn malformed_batch_and_foreign_room_are_refused() {
    let before = governed();
    let authority = key(0xA0);
    assert!(matches!(
        admit(
            Some(&before),
            member(&authority, OWNER),
            AuthorityPin::Device(&authority),
            &[vec![0x85, 0x6f, 0x4a, 0x83, 0xff]],
        ),
        Err(RoomRefusal::Malformed(_))
    ));
    let moved = changes_after(&before, |doc| {
        doc.put(ROOT, "room", "5b0e8c47-26a1-4f3e-8d9c-1a2b3c4d5e6f")
            .unwrap();
    });
    assert_eq!(
        admit(
            Some(&before),
            member(&authority, OWNER),
            AuthorityPin::Device(&authority),
            &moved,
        )
        .unwrap_err(),
        RoomRefusal::DeclaredRoomMismatch
    );
}

#[test]
fn governance_changes_only_from_the_pinned_writer() {
    let before = governed();
    let seize = changes_after(&before, |doc| take_ownership(doc, EDITOR));
    let authority = key(0xA0);
    let editor = key(0x02);
    let as_editor = member(&editor, EDITOR);
    for pin in [
        AuthorityPin::Device(&authority),
        AuthorityPin::Person(&OWNER),
        AuthorityPin::Local,
        AuthorityPin::Unpinned,
    ] {
        assert_eq!(
            admit(Some(&before), as_editor, pin, &seize).unwrap_err(),
            RoomRefusal::GovernanceWriter,
            "{pin:?} must refuse an editor's governance change"
        );
    }
    assert!(
        admit(
            Some(&before),
            member(&authority, OWNER),
            AuthorityPin::Device(&authority),
            &seize,
        )
        .is_ok(),
        "the pinned device may change governance"
    );
    assert!(
        admit(
            Some(&before),
            member(&key(0x05), OWNER),
            AuthorityPin::Person(&OWNER),
            &seize,
        )
        .is_ok(),
        "any device of the pinned person may"
    );
    // An echo re-states held governance under the sender's own actor.
    let echo = changes_after(&before, |doc| {
        doc.put(ROOT, "owner", person_id(OWNER)).unwrap();
    });
    assert!(admit(Some(&before), as_editor, AuthorityPin::Unpinned, &echo).is_ok());
}

#[test]
fn unpinned_room_refuses_the_first_governance_from_anyone() {
    let snapshot = all_changes(&governed());
    let first = key(0xB0);
    assert_eq!(
        admit(
            None,
            Writer {
                key: &first,
                standing: Standing::TransportAuthorized,
            },
            AuthorityPin::Unpinned,
            &snapshot,
        )
        .unwrap_err(),
        RoomRefusal::GovernanceWriter,
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
        admit(
            Some(&before),
            member(&editor, EDITOR),
            AuthorityPin::Local,
            &receipts,
        )
        .unwrap_err(),
        RoomRefusal::GovernanceWriter
    );
}

#[test]
fn effect_receipts_are_immutable_even_for_the_governor() {
    let mut before = governed();
    let receipts = before
        .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
        .unwrap();
    before
        .put(
            &receipts,
            "invocation",
            ScalarValue::Bytes(br#"{"outcome":"done"}"#.to_vec()),
        )
        .unwrap();
    before.commit();
    let rewrite = changes_after(&before, |doc| {
        let (_, receipts) = doc.get(ROOT, "effect_receipts_v1").unwrap().unwrap();
        doc.put(
            &receipts,
            "invocation",
            ScalarValue::Bytes(br#"{"outcome":"undone"}"#.to_vec()),
        )
        .unwrap();
    });
    let authority = key(0xA0);
    assert_eq!(
        admit(
            Some(&before),
            member(&authority, OWNER),
            AuthorityPin::Device(&authority),
            &rewrite,
        )
        .unwrap_err(),
        RoomRefusal::ReceiptImmutable
    );
}

fn coverage(sequence: u64) -> crate::coverage::Coverage {
    crate::coverage::Coverage {
        room: ROOM.into(),
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
        admit(
            Some(&before),
            member(&authority, OWNER),
            AuthorityPin::Device(&authority),
            &rewind,
        ),
        Err(RoomRefusal::Coverage(_))
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
        admit(
            Some(&before),
            member(&authority, OWNER),
            AuthorityPin::Device(&authority),
            &rewind,
        ),
        Err(RoomRefusal::Durability(_))
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
        admit(
            Some(&before),
            member(&authority, OWNER),
            AuthorityPin::Device(&authority),
            &malformed,
        ),
        Err(RoomRefusal::Media(_))
    ));
}

#[test]
fn refusals_keep_the_peer_state_messages() {
    for (refusal, message) in [
        (RoomRefusal::GovernanceMissing, "room governance missing"),
        (RoomRefusal::WriteForbidden, "room write forbidden"),
        (RoomRefusal::DeclaredRoomMismatch, "declared room mismatch"),
        (
            RoomRefusal::GovernanceWriter,
            "governance writer is not pinned authority",
        ),
        (RoomRefusal::ReceiptImmutable, "effect receipt is immutable"),
    ] {
        assert_eq!(refusal.to_string(), message);
    }
}

/// The changes saving `shortcut` makes on top of `before`.
fn saving(before: &AutoCommit, shortcut: tilo_automation::Shortcut) -> Vec<Vec<u8>> {
    changes_after(before, |doc| {
        tilo_desktop_model::projection::project_workspace_shortcuts(doc, &[shortcut]).unwrap();
    })
}

/// A shortcut saved from a device whose certificate was issued earlier in
/// the second it arrives in is proven: admission judges at the instant, not
/// at the start of its second.
#[test]
fn a_shortcut_saved_with_a_certificate_issued_this_second_is_proven() {
    use crate::room::saved_shortcut::tests::{NOW_NS, device, shortcut};
    let before = governed();
    let authority = key(0xA0);
    let writer = key(0x02);
    let mut editor = device(0x22, 0x02);
    editor.cert.issued_at_ns = NOW_NS + 400_000_000;
    tilo_user_identity::sign_device_cert(&mut editor.cert, &editor.root).unwrap();
    let batch = saving(&before, editor.saves(ROOM, shortcut("water")));
    admit_room_changes(
        RoomAdmission {
            before: Some(&before),
            room: ROOM,
            source: member(&writer, EDITOR),
            authority: AuthorityPin::Device(&authority),
            now_ns: NOW_NS + 700_000_000,
        },
        &batch,
    )
    .expect("the certificate is valid at the instant the save arrives");
}

#[test]
fn a_shortcut_names_as_its_saver_only_whose_device_signed_it() {
    use crate::room::saved_shortcut::tests::{device, shortcut};
    assert_eq!(
        crate::room::saved_shortcut::ns_at(NOW),
        crate::room::saved_shortcut::tests::NOW_NS
    );
    let before = governed();
    let authority = key(0xA0);
    let writer = key(0x02);
    let owner = device(0x11, 0xA0);
    let editor = device(0x22, 0x02);
    let governor = AuthorityPin::Device(&authority);
    let as_editor = || member(&writer, EDITOR);
    let as_governor = || member(&authority, OWNER);

    // The editor's device names the owner as who saved it.
    let mut forged = editor.saves(ROOM, shortcut("water"));
    forged.saved_by = Some(owner.person());
    let mut unproven = shortcut("water");
    unproven.saved_by = Some(owner.person());
    for batch in [saving(&before, forged), saving(&before, unproven)] {
        assert_eq!(
            admit(Some(&before), as_editor(), governor, &batch).unwrap_err(),
            RoomRefusal::ShortcutAuthorship("water".into()),
            "a member cannot save a shortcut as another person"
        );
    }

    // The governor is no exception: it names the editor without the
    // editor's device having signed.
    let mut forged = owner.saves(ROOM, shortcut("ferns"));
    forged.saved_by = Some(editor.person());
    let mut unproven = shortcut("ferns");
    unproven.saved_by = Some(editor.person());
    for batch in [saving(&before, forged), saving(&before, unproven)] {
        assert_eq!(
            admit(Some(&before), as_governor(), governor, &batch).unwrap_err(),
            RoomRefusal::ShortcutAuthorship("ferns".into()),
            "the governor cannot save a shortcut as a member"
        );
    }

    // A proof from another room does not move with the shortcut.
    let copied = saving(
        &before,
        editor.saves("5e1d9a40-3c2b-4f6e-8a7d-1b2c3d4e5f60", shortcut("lamp")),
    );
    assert_eq!(
        admit(Some(&before), as_editor(), governor, &copied).unwrap_err(),
        RoomRefusal::ShortcutAuthorship("lamp".into()),
    );

    // The person's own device saves it; the governor relaying that change is
    // judged by the device that signed it, not by itself.
    let own = saving(&before, editor.saves(ROOM, shortcut("water")));
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
            &saving(&before, shortcut("nobody")),
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
        &[editor.saves(ROOM, shortcut("water")), legacy],
    )
    .unwrap();
    held.commit();
    let edit = changes_after(&held, |doc| hold_record(doc, "file:/plan.md"));
    assert!(
        admit(Some(&held), as_editor(), governor, &edit).is_ok(),
        "a shortcut held unchanged is not a write"
    );
}
