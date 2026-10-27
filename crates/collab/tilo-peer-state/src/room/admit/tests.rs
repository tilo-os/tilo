//! One case per check of [`admit_room_changes`], each admitted and refused on
//! the production function — the same call both receivers make.
use super::*;
use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue};
use std::collections::BTreeMap;
use tilo_action_policy::{Role, Subject, roles};
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::{DesktopTree, Strip, StripId, Workspace, WorkspaceId, shared_record};

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

/// A room the owner governs, with an editor and a viewer, one strip the
/// owner made and one the editor made.
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
    let mut workspace = Workspace::new(WorkspaceId(1), "Room");
    workspace.strips = vec![
        Strip::new(StripId(OWNERS_STRIP), "Strip"),
        Strip::new(StripId(EDITORS_STRIP), "Strip"),
    ];
    shared_record::write(
        &mut doc,
        &SharedDesktop {
            tree: DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            authors: BTreeMap::from([
                (OWNERS_STRIP, person_id(OWNER)),
                (EDITORS_STRIP, person_id(EDITOR)),
            ]),
            ..SharedDesktop::default()
        },
    )
    .unwrap();
    doc.commit();
    doc
}

const OWNERS_STRIP: u64 = 0x0001_0000_0000_0001;
const EDITORS_STRIP: u64 = 0x0002_0000_0000_0001;

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
            caps_now: NOW,
        },
        changes,
    )
}

fn strip_name(doc: &mut AutoCommit, id: u64, name: &str) {
    let mut desktop = shared_record::read(&*doc, None).unwrap();
    for strip in desktop
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|w| &mut w.strips)
    {
        if strip.id.0 == id {
            strip.name = name.into();
        }
    }
    shared_record::write(doc, &desktop).unwrap();
}

/// A strip nobody is named as having made.
fn add_strip(doc: &mut AutoCommit, id: u64) {
    let mut desktop = shared_record::read(&*doc, None).unwrap();
    desktop.tree.workspaces[0]
        .strips
        .push(Strip::new(StripId(id), "New"));
    shared_record::write(doc, &desktop).unwrap();
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

#[test]
fn sender_needs_write_on_the_room_it_already_governs() {
    let before = governed();
    let edit = changes_after(&before, |doc| strip_name(doc, EDITORS_STRIP, "Edited"));
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
    let added = changes_after(&before, |doc| add_strip(doc, 0x0003_0000_0000_0001));
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
fn what_someone_else_made_changes_only_with_the_rooms_leave() {
    let before = governed();
    let authority = key(0xA0);
    let editor = key(0x02);
    let theirs = changes_after(&before, |doc| strip_name(doc, OWNERS_STRIP, "Stolen"));
    for pin in [AuthorityPin::Device(&authority), AuthorityPin::Local] {
        let refused = admit(Some(&before), member(&editor, EDITOR), pin, &theirs);
        assert!(
            matches!(refused, Err(RoomRefusal::NotTheirs(_))),
            "{pin:?}: {refused:?}"
        );
    }
    let own = changes_after(&before, |doc| strip_name(doc, EDITORS_STRIP, "Mine"));
    assert!(
        admit(
            Some(&before),
            member(&editor, EDITOR),
            AuthorityPin::Device(&authority),
            &own,
        )
        .is_ok(),
        "a person's own strip is theirs from any device"
    );
    // The owner, from a device that does not govern the room, moderates it.
    let moderated = changes_after(&before, |doc| strip_name(doc, EDITORS_STRIP, "Tidied"));
    assert!(
        admit(
            Some(&before),
            member(&key(0x07), OWNER),
            AuthorityPin::Device(&authority),
            &moderated,
        )
        .is_ok()
    );
    // A sender whose person is not known may change nothing a person made.
    assert!(matches!(
        admit(
            Some(&before),
            Writer {
                key: &editor,
                standing: Standing::TransportAuthorized,
            },
            AuthorityPin::Local,
            &own,
        ),
        Err(RoomRefusal::NotTheirs(_))
    ));
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
