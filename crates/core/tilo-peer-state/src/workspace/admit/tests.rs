//! One case per check of [`admit_workspace_changes`], each admitted and refused on
//! the production function — the same call both receivers make.
use super::*;
use crate::workspace::GovernanceRow;
use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue};
use std::collections::BTreeMap;
use tilo_action_policy::Change;
use tilo_action_policy::{Grantee, Role, roles};
use tilo_user_identity::DeviceCert;

const WORKSPACE: &str = "0b5c3f1e-7d2a-4c1b-9e8f-6a5d4c3b2a19";
const NOW: u64 = 1_000;

fn key(byte: u8) -> String {
    hex::encode([byte; 32])
}

fn person_id(root: [u8; 32]) -> String {
    hex::encode(root)
}

/// The device of each person the tests name: the workspace's owner, an editor,
/// a viewer, an administrator and a newcomer.
fn owner_device() -> (ed25519_dalek::SigningKey, DeviceCert) {
    rooted(0x11)
}
fn editor_device() -> (ed25519_dalek::SigningKey, DeviceCert) {
    rooted(0x23)
}
fn viewer_device() -> (ed25519_dalek::SigningKey, DeviceCert) {
    rooted(0x35)
}
fn admin_device() -> (ed25519_dalek::SigningKey, DeviceCert) {
    rooted(0x57)
}
fn owner_root() -> [u8; 32] {
    owner_device().1.user_public_key
}
fn editor_root() -> [u8; 32] {
    editor_device().1.user_public_key
}
fn viewer_root() -> [u8; 32] {
    viewer_device().1.user_public_key
}
fn admin_root() -> [u8; 32] {
    admin_device().1.user_public_key
}
fn newcomer_root() -> [u8; 32] {
    rooted(0x69).1.user_public_key
}

/// Sign `changes` into `doc` as `by`, on top of every operation it holds.
fn sign(doc: &mut AutoCommit, by: &(ed25519_dalek::SigningKey, DeviceCert), changes: Vec<Change>) {
    crate::members::op::tests_support::sign_into(
        doc,
        by,
        crate::workspace::saved_shortcut::ns_at(NOW),
        changes,
    );
}

/// Sign, as `by`, that `person` holds `role`.
fn put_role(
    doc: &mut AutoCommit,
    by: &(ed25519_dalek::SigningKey, DeviceCert),
    person: [u8; 32],
    role: Role,
) {
    crate::members::op::tests_support::set_member(doc, by, &person_id(person), role);
}

/// The maps a founded workspace holds, which its devices and governors write
/// their own rows into before any of them writes.
fn founded_maps(doc: &mut AutoCommit) {
    use automerge::ObjType;
    for key in [
        crate::coverage::FIELD_V2,
        crate::durability::FIELD_V2,
        crate::removals::FIELD_V1,
        crate::standing::FIELD_V1,
        crate::document_import::WORKSPACE_KEY,
        crate::state::invitation_receipts::RECEIPTS_KEY,
        crate::state::invitation_receipts::PARTS_KEY,
        tilo_desktop_model::projection::WORKSPACE_AUTOMATIONS_KEY,
        tilo_desktop_model::projection::WORKSPACE_PRESETS_KEY,
        tilo_desktop_model::projection::WORKSPACE_SANDBOX_KEY,
        tilo_desktop_model::projection::WORKSPACE_EFFECT_RECEIPTS_KEY,
        tilo_desktop_model::projection::WORKSPACE_EFFECT_HISTORY_KEY,
        tilo_desktop_model::projection::WORKSPACE_WRITE_REFUSALS_KEY,
    ] {
        doc.put_object(ROOT, key, ObjType::Map).unwrap();
    }
}

/// A workspace the owner governs, with an editor and a viewer.
fn governed() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "workspace", WORKSPACE).unwrap();
    doc.put(ROOT, "name", "Workspace").unwrap();
    doc.put(ROOT, "owner", person_id(owner_root())).unwrap();
    founded_maps(&mut doc);
    sign(
        &mut doc,
        &owner_device(),
        vec![
            Change::AddMember {
                person: person_id(editor_root()),
                role: Role::Editor,
            },
            Change::AddMember {
                person: person_id(viewer_root()),
                role: Role::Viewer,
            },
        ],
    );
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
        access: SenderAccess::Member {
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
    admit_anchored(before, source, owner, &[], changes)
}

/// [`admit`] on a replica whose own device, and the device it joined
/// through, are `anchors`.
fn admit_anchored(
    before: Option<&AutoCommit>,
    source: Writer<'_>,
    owner: &[u8; 32],
    anchors: &[[u8; 32]],
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, WorkspaceRefusal> {
    admit_workspace_changes(
        WorkspaceAdmission {
            before,
            workspace: WORKSPACE,
            source,
            owner,
            administrators: &[],
            anchors,
            now_ns: crate::workspace::saved_shortcut::ns_at(NOW),
        },
        changes,
    )
    .map_err(|failure| match failure {
        AdmissionFailure::Refused(refusal) => refusal,
        // These changes are decided in this test's own process; a worker
        // failure would be the test failing, not a case to assert.
        AdmissionFailure::Device(reason) => WorkspaceRefusal::Malformed(reason),
    })
}

/// A record the workspace holds, as any member writes it.
fn hold_document(doc: &mut AutoCommit, record: &str) {
    tilo_desktop_model::projection::project_workspace_documents(
        doc,
        &BTreeMap::from([(record.to_owned(), "document".to_owned())]),
    )
    .unwrap();
}

fn take_ownership(doc: &mut AutoCommit, root: [u8; 32]) {
    doc.put(ROOT, "owner", person_id(root)).unwrap();
}

tilo_test_env::scenario! {
    fn a_batch_is_admitted_only_under_the_workspaces_authority;

    phase a_first_member_list_is_taken_in_where_it_lists_this_device;
    // With nothing held, whoever sends the workspace's governance, it is taken
    // in only where it lists this replica's own device (or the device it
    // joined through) under a person holding a role: no sender is the
    // workspace's governor by answering first.
    {
        let mut listed = governed();
        let (_, editor) = editor_device();
        list_device(&mut listed, &editor);
        let snapshot = all_changes(&listed);
        let stranger = key(0xB0);
        let admitted = admit_anchored(
            None,
            member(&stranger, newcomer_root()),
            &[0; 32],
            &[editor.device_public_key],
            &snapshot,
        )
        .expect("the governance lists the replica's own device");
        assert_eq!(
            super::super::governance_of(Some(&admitted.candidate)).owner,
            Some(person_id(owner_root()))
        );
        let (_, newcomer) = rooted(0x69);
        assert_eq!(
            admit_anchored(
                None,
                member(&key(0xA0), owner_root()),
                &owner_root(),
                &[newcomer.device_public_key],
                &snapshot,
            )
            .unwrap_err(),
            WorkspaceRefusal::WriteForbidden,
            "a governance that lists no anchor of the replica governs nothing here, \
             whoever sends it"
        );
    }

    phase a_base_keeps_the_workspace_s_authority_and_an_editor_writes_on_it;
    // A workspace started over from a base holds the authority it held, so a member
    // writes on the base as before it.
    {
        let workspace = governed();
        let based = super::super::base::rebase(&workspace, automerge::ActorId::random()).unwrap();
        assert_eq!(
            crate::workspace::policy_of(&based, WORKSPACE),
            crate::workspace::policy_of(&workspace, WORKSPACE)
        );
        let writer = key(0x02);
        let edit = changes_after(&based, |doc| hold_document(doc, "file:/plan.md"));
        assert!(
            admit(
                Some(&based),
                member(&writer, editor_root()),
                &owner_root(),
                &edit,
            )
            .is_ok()
        );
    }

    phase sender_needs_write_on_the_workspace_it_already_governs;
    {
        let before = governed();
        let edit = changes_after(&before, |doc| hold_document(doc, "file:/plan.md"));
        let writer = key(0x02);
        assert_eq!(
            admit(
                Some(&before),
                member(&writer, viewer_root()),
                &owner_root(),
                &edit,
            )
            .unwrap_err(),
            WorkspaceRefusal::WriteForbidden,
            "a viewer cannot rewrite workspace contents"
        );
        assert_eq!(
            admit(
                Some(&before),
                member(&writer, [0x44; 32]),
                &owner_root(),
                &edit,
            )
            .unwrap_err(),
            WorkspaceRefusal::WriteForbidden,
            "nor can someone who is not a member"
        );
        assert!(
            admit(
                Some(&before),
                member(&writer, editor_root()),
                &owner_root(),
                &edit,
            )
            .is_ok()
        );
        let added = changes_after(&before, |doc| hold_document(doc, "file:/notes.md"));
        assert!(
            admit(
                Some(&before),
                Writer {
                    key: &writer,
                    access: SenderAccess::TransportAuthorized,
                },
                &owner_root(),
                &added,
            )
            .is_ok(),
            "a transport that already decided Write is not asked again"
        );
    }

    phase malformed_batch_and_foreign_workspace_are_refused;
    {
        let before = governed();
        let authority = key(0xA0);
        assert!(matches!(
            admit(
                Some(&before),
                member(&authority, owner_root()),
                &owner_root(),
                &[vec![0x85, 0x6f, 0x4a, 0x83, 0xff]],
            ),
            Err(WorkspaceRefusal::Malformed(_))
        ));
        let moved = changes_after(&before, |doc| {
            doc.put(ROOT, "workspace", "5b0e8c47-26a1-4f3e-8d9c-1a2b3c4d5e6f")
                .unwrap();
        });
        assert_eq!(
            admit(
                Some(&before),
                member(&authority, owner_root()),
                &owner_root(),
                &moved,
            )
            .unwrap_err(),
            WorkspaceRefusal::DeclaredWorkspaceMismatch
        );
    }

    phase the_member_list_changes_only_through_operations;
    // The owner writes governance as everyone does: through an operation its
    // role permits. A batch that moves governance without one is refused from
    // the owner's devices too.
    {
        let before = governed();
        let seize = changes_after(&before, |doc| take_ownership(doc, editor_root()));
        let editor = key(0x02);
        let as_editor = member(&editor, editor_root());
        assert_eq!(
            admit(Some(&before), as_editor, &owner_root(), &seize).unwrap_err(),
            WorkspaceRefusal::GovernanceWriter,
            "an editor's governance change is refused"
        );
        for device in [0xA0, 0x05] {
            assert_eq!(
                admit(
                    Some(&before),
                    member(&key(device), owner_root()),
                    &owner_root(),
                    &seize
                )
                .unwrap_err(),
                WorkspaceRefusal::GovernanceWriter,
                "no device of the owner writes governance without an operation"
            );
        }
        // An echo re-states held governance under the sender's own actor.
        let echo = changes_after(&before, |doc| {
            doc.put(ROOT, "owner", person_id(owner_root())).unwrap();
        });
        assert!(admit(Some(&before), as_editor, &owner_root(), &echo).is_ok());
    }

    phase a_stored_layout_scope_is_no_governance;
    // A workspace made while the layout could be one member's own stores
    // the scope it was given (`layout_v1`). This build reads no scope: the value
    // is no longer governance, so a member's batch that carries it is admitted as
    // any other change, and the layout a workspace holds is its members'.
    {
        let before = governed();
        let edit = changes_after(&before, |doc| {
            doc.put(ROOT, "layout_v1", "personal").unwrap();
            hold_document(doc, "file:/plan.md");
        });
        assert!(
            admit(
                Some(&before),
                member(&key(0x02), editor_root()),
                &owner_root(),
                &edit
            )
            .is_ok()
        );
    }

    phase a_first_answer_listing_no_device_here_is_refused;
    {
        let snapshot = all_changes(&governed());
        let first = key(0xB0);
        assert_eq!(
            admit(
                None,
                Writer {
                    key: &first,
                    access: SenderAccess::TransportAuthorized,
                },
                &owner_root(),
                &snapshot,
            )
            .unwrap_err(),
            WorkspaceRefusal::WriteForbidden,
            "answering first does not make a peer the governor"
        );
    }

    phase durable_and_import_governance_count_as_governance;
    {
        let before = governed();
        let receipts = changes_after(&before, |doc| {
            crate::durability::put(
                doc,
                BTreeMap::from([(
                    crate::coverage::route_key("app", "note"),
                    durable_receipt(1),
                )]),
            )
            .unwrap();
        });
        let editor = key(0x02);
        assert_eq!(
            admit(
                Some(&before),
                member(&editor, editor_root()),
                &owner_root(),
                &receipts,
            )
            .unwrap_err(),
            WorkspaceRefusal::GovernanceWriter
        );
    }

    phase refusals_keep_the_peer_state_messages;
    {
        for (refusal, message) in [
            (
                WorkspaceRefusal::RulesMissing,
                "the workspace's rules are not here yet",
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
                "governance is not this writer's to change",
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
}

/// A receipt `actor` recorded for invocation `invocation`, as a member's
/// device records it.
fn effect(
    invocation: u128,
    actor: &tilo_actions::Actor,
) -> tilo_desktop_model::WorkspaceEffectReceipt {
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

tilo_test_env::scenario! {
    fn a_member_writes_only_their_own_receipts_shares_and_shortcuts;

    phase a_member_records_only_their_own_effect_receipts;
    {
        let before = governed();
        let editor = key(0x02);
        let own = recording(
            &before,
            editor_root(),
            &[
                effect(1, &tilo_actions::MemberId::person(person_id(editor_root())).into()),
                effect(
                    2,
                    &tilo_actions::MemberId::agent("scribe", person_id(editor_root())).into(),
                ),
                // An app on their device that acted by itself is the author of its own effect.
                effect(6, &tilo_actions::Actor::App("so.tilo.discord".into())),
            ],
        );
        assert!(
            admit(
                Some(&before),
                member(&editor, editor_root()),
                &owner_root(),
                &own
            )
            .is_ok(),
            "their own receipts, their agents' and their apps', in their own slice"
        );
        for (slice, actor) in [
            (viewer_root(), person_id(editor_root())),
            (editor_root(), person_id(viewer_root())),
        ] {
            let foreign = recording(&before, slice, &[effect(3, &tilo_actions::MemberId::person(actor).into())]);
            assert_eq!(
                admit(
                    Some(&before),
                    member(&editor, editor_root()),
                    &owner_root(),
                    &foreign
                )
                .unwrap_err(),
                WorkspaceRefusal::ReceiptWriter,
                "another's slice, or another's actor"
            );
        }
        let anchor = |person: [u8; 32]| {
            changes_after(&before, |doc| {
                let history = doc
                    .get(ROOT, "effect_history_root_v1")
                    .unwrap()
                    .unwrap()
                    .1;
                doc.put(
                    &history,
                    format!("{}/{}", person_id(person), key(0x02)),
                    ScalarValue::Bytes(vec![7; 32]),
                )
                .unwrap();
            })
        };
        assert!(
            admit(
                Some(&before),
                member(&editor, editor_root()),
                &owner_root(),
                &anchor(editor_root())
            )
            .is_ok()
        );
        assert_eq!(
            admit(
                Some(&before),
                member(&editor, editor_root()),
                &owner_root(),
                &anchor(viewer_root())
            )
            .unwrap_err(),
            WorkspaceRefusal::ReceiptWriter,
            "only their own archive's anchor"
        );
        let viewer = key(0x03);
        assert!(
            admit(
                Some(&before),
                member(&viewer, viewer_root()),
                &owner_root(),
                &recording(
                    &before,
                    viewer_root(),
                    &[effect(5, &tilo_actions::MemberId::person(person_id(viewer_root())).into())]
                ),
            )
            .is_ok(),
            "one who only reads the workspace records their own receipts"
        );
        let relayed = recording(
            &before,
            editor_root(),
            &[effect(4, &tilo_actions::MemberId::person(person_id(editor_root())).into())],
        );
        let authority = key(0xA0);
        assert!(
            admit(
                Some(&before),
                member(&authority, owner_root()),
                &owner_root(),
                &relayed
            )
            .is_ok(),
            "the owner's devices relay what members recorded"
        );
    }

    phase a_member_shares_only_what_they_hold_and_ends_only_their_own_shares;
    // A share is its sharer's operation: one they may give takes effect, and one
    // they may not is kept and changes nothing.
    {
        use tilo_action_policy::share::ShareAccess;
        let before = governed();
        let (editor_key, viewer_key) = (key(0x02), key(0x03));
        let (editor_signs, viewer_signs) = (editor_device(), viewer_device());
        let edit = share_of_note(editor_root(), ShareAccess::Edit);
        assert!(
            holds(
                &before,
                member(&editor_key, editor_root()),
                &regranting(&before, &editor_signs, &edit, &[]),
                &edit
            ),
            "an Editor shares a note to edit as theirs"
        );
        let viewer_edit = share_of_note(viewer_root(), ShareAccess::Edit);
        assert!(
            !holds(
                &before,
                member(&viewer_key, viewer_root()),
                &regranting(&before, &viewer_signs, &viewer_edit, &[]),
                &viewer_edit
            ),
            "a Viewer hands out no edit"
        );
        let view = share_of_note(viewer_root(), ShareAccess::View);
        assert!(
            holds(
                &before,
                member(&viewer_key, viewer_root()),
                &regranting(&before, &viewer_signs, &view, &[]),
                &view
            ),
            "but shares what they see"
        );
        assert!(
            !holds(
                &before,
                member(&viewer_key, viewer_root()),
                &regranting(&before, &viewer_signs, &edit, &[]),
                &edit
            ),
            "nobody shares as someone else"
        );

        let mut shared = before.clone();
        let changes: Vec<_> = regranting(&before, &editor_signs, &edit, &[])
            .into_iter()
            .map(|bytes| automerge::Change::from_bytes(bytes).unwrap())
            .collect();
        tilo_automerge::ApplyIncoming::apply_incoming(&mut shared, changes).unwrap();
        assert!(
            holds(
                &shared,
                member(&viewer_key, viewer_root()),
                &regranting(&shared, &viewer_signs, &[], &edit),
                &edit
            ),
            "only its sharer ends a share"
        );
        assert!(
            !holds(
                &shared,
                member(&editor_key, editor_root()),
                &regranting(&shared, &editor_signs, &[], &edit),
                &edit
            ),
            "its sharer ends it"
        );
    }

    phase a_shortcut_saved_with_a_certificate_issued_this_second_is_proven;
    // A shortcut saved from a device whose certificate was issued earlier in
    // the second it arrives in is proven: admission judges at the instant, not
    // at the start of its second.
    {
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
                source: member(&writer, editor_root()),
                owner: &owner_root(),
                administrators: &[],
                anchors: &[],
                now_ns: NOW_NS + 700_000_000,
                },
            &batch,
        )
        .expect("the certificate is valid at the instant the save arrives");
    }

    phase a_shortcut_names_as_its_saver_only_whose_device_signed_it;
    {
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
        let governor = &owner_root();
        let as_editor = || member(&writer, editor_root());
        let as_governor = || member(&authority, owner_root());

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
        let edit = changes_after(&held, |doc| hold_document(doc, "file:/plan.md"));
        assert!(
            admit(Some(&held), as_editor(), governor, &edit).is_ok(),
            "a shortcut held unchanged is not a write"
        );
    }
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

/// The changes `by` writing `grants` into the workspace, and taking `ended`
/// out, make on `before`.
fn regranting(
    before: &AutoCommit,
    by: &(ed25519_dalek::SigningKey, DeviceCert),
    grants: &[tilo_action_policy::Grant],
    ended: &[tilo_action_policy::Grant],
) -> Vec<Vec<u8>> {
    changes_after(before, |doc| {
        let changes = grants
            .iter()
            .cloned()
            .map(Change::Grant)
            .chain(ended.iter().map(|grant| Change::Revoke(grant.id.clone())))
            .collect();
        sign(doc, by, changes);
    })
}

/// Whether the workspace `changes` make of `before` holds every grant in `grants`.
fn holds(
    before: &AutoCommit,
    writer: Writer<'_>,
    changes: &[Vec<u8>],
    grants: &[tilo_action_policy::Grant],
) -> bool {
    let admitted = admit(Some(before), writer, &owner_root(), changes)
        .expect("an operation that proves its writer is kept");
    let policy = crate::workspace::policy_of(&admitted.candidate, WORKSPACE);
    grants.iter().all(|grant| policy.grants.contains(grant))
}

fn coverage(sequence: u64) -> crate::coverage::Coverage {
    crate::coverage::Coverage {
        workspace: WORKSPACE.into(),
        app: "app".into(),
        record: "note".into(),
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

/// What a batch itself forbids is refused whoever wrote it, the governor's
/// own device included: one row per such check, with the refusal it gives.
#[test]
fn a_forbidden_change_is_refused_even_from_the_governor() {
    let mut covered = governed();
    crate::coverage::put(&mut covered, coverage(5)).unwrap();
    covered.commit();
    let coverage_rewind = changes_after(&covered, |doc| {
        crate::coverage::put(doc, coverage(4)).unwrap();
    });

    let route = crate::coverage::route_key("app", "note");
    let mut durable = governed();
    crate::durability::put(
        &mut durable,
        BTreeMap::from([(route.clone(), durable_receipt(7))]),
    )
    .unwrap();
    durable.commit();
    let durability_rewind = changes_after(&durable, |doc| {
        crate::durability::put(doc, BTreeMap::from([(route, durable_receipt(6))])).unwrap();
    });

    let mut recorded = governed();
    tilo_desktop_model::projection::project_effect_receipts(
        &mut recorded,
        &person_id(owner_root()),
        &[effect(
            1,
            &tilo_actions::MemberId::person(person_id(owner_root())).into(),
        )],
    )
    .unwrap();
    recorded.commit();
    let mut rewritten = effect(
        1,
        &tilo_actions::MemberId::person(person_id(owner_root())).into(),
    );
    rewritten.status = tilo_actions::EffectReceiptStatus::Cancelled;
    let receipt_rewrite = recording(&recorded, owner_root(), &[rewritten]);

    let authority = key(0xA0);
    for (case, before, changes, expected) in [
        ("coverage rewound", &covered, &coverage_rewind, "Coverage"),
        (
            "durability rewound",
            &durable,
            &durability_rewind,
            "Durability",
        ),
        (
            "an effect receipt rewritten",
            &recorded,
            &receipt_rewrite,
            "ReceiptImmutable",
        ),
    ] {
        let refusal = admit(
            Some(before),
            member(&authority, owner_root()),
            &owner_root(),
            changes,
        )
        .unwrap_err();
        let refused_as = match &refusal {
            WorkspaceRefusal::Coverage(_) => "Coverage",
            WorkspaceRefusal::Durability(_) => "Durability",
            WorkspaceRefusal::ReceiptImmutable => "ReceiptImmutable",
            _ => "another refusal",
        };
        assert_eq!(refused_as, expected, "{case}: {refusal:?}");
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

/// [`governed`] with an editor the owner made an administrator.
fn with_administrator() -> AutoCommit {
    let mut doc = governed();
    sign(
        &mut doc,
        &owner_device(),
        vec![
            Change::AddMember {
                person: person_id(admin_root()),
                role: Role::Editor,
            },
            Change::Grant(roles::administrator_grant(
                WORKSPACE,
                &person_id(admin_root()),
                Grantee::person(person_id(owner_root())),
            )),
        ],
    );
    doc.commit();
    doc
}

/// The role `changes` leave `person` in, admitted on `before` from `writer`.
fn role_after(
    before: &AutoCommit,
    writer: Writer<'_>,
    changes: &[Vec<u8>],
    person: [u8; 32],
) -> Option<Role> {
    let admitted = admit(Some(before), writer, &owner_root(), changes)
        .expect("an operation that proves its writer is kept");
    crate::workspace::policy_of(&admitted.candidate, WORKSPACE).role_of(&person_id(person))
}

tilo_test_env::scenario! {
    fn a_membership_operation_changes_what_its_writers_role_permits;

    phase an_administrator_admits_a_member_and_an_editors_admission_changes_nothing;
    {
        let before = with_administrator();
        let by_admin = changes_after(&before, |doc| {
            put_role(doc, &admin_device(), newcomer_root(), Role::Commenter);
        });
        assert_eq!(
            role_after(
                &before,
                member(&key(0x05), admin_root()),
                &by_admin,
                newcomer_root()
            ),
            Some(Role::Commenter),
            "an administrator admits a member"
        );
        let by_editor = changes_after(&before, |doc| {
            put_role(doc, &editor_device(), newcomer_root(), Role::Commenter);
        });
        assert_eq!(
            role_after(
                &before,
                member(&key(0x02), editor_root()),
                &by_editor,
                newcomer_root()
            ),
            None,
            "an editor admits no one"
        );
    }

    phase an_administrator_writes_membership_only;
    {
        let before = with_administrator();
        let rename = changes_after(&before, |doc| take_ownership(doc, admin_root()));
        assert_eq!(
            admit(
                Some(&before),
                member(&key(0x05), admin_root()),
                &owner_root(),
                &rename
            )
            .unwrap_err(),
            WorkspaceRefusal::GovernanceWriter
        );
        let above = changes_after(&before, |doc| {
            put_role(doc, &admin_device(), newcomer_root(), Role::Owner);
        });
        assert_eq!(
            role_after(
                &before,
                member(&key(0x05), admin_root()),
                &above,
                newcomer_root()
            ),
            None,
            "an administrator hands out no owner's role"
        );
    }

    phase a_membership_change_stands_where_its_writer_held_the_grant;
    // A change stands when the revocation of its writer's grant was written on
    // top of it; one written after the revocation, or beside it, does not: the
    // owner's operation goes first.
    {
        let mut base = with_administrator();
        let revoke = |doc: &mut AutoCommit| {
            sign(
                doc,
                &owner_device(),
                vec![Change::Revoke(roles::administrator_grant_id(&person_id(
                    admin_root(),
                )))],
            );
            doc.commit();
        };
        let admits = |doc: &mut AutoCommit| {
            put_role(doc, &admin_device(), newcomer_root(), Role::Viewer);
            doc.commit();
        };
        let replayed = |doc: &AutoCommit| {
            crate::workspace::policy_of(doc, WORKSPACE).role_of(&person_id(newcomer_root()))
        };
        let mut earlier = base.fork();
        admits(&mut earlier);
        revoke(&mut earlier);
        assert_eq!(
            replayed(&earlier),
            Some(Role::Viewer),
            "made while the writer held the grant"
        );

        let mut later = base.fork();
        revoke(&mut later);
        admits(&mut later);
        assert_eq!(
            replayed(&later),
            None,
            "made after the revocation reached the writer"
        );

        let mut beside = base.fork();
        revoke(&mut beside);
        let mut concurrent = base.fork();
        admits(&mut concurrent);
        tilo_automerge::ApplyIncoming::merge_incoming(&mut beside, &mut concurrent).unwrap();
        assert_eq!(replayed(&beside), None, "the owner's revocation goes first");
    }

    phase a_role_permits_the_governance_a_members_operation_names;
    // A governance part is changed by a device whose role holds its action: an
    // administrator holds every governance action, so its device renames the
    // workspace with the operation that names the row it changed. Every receiver
    // shares this admission path.
    {
        let workspace = started();
        let before = &workspace.doc;
        let reached = crate::members::heads(&crate::members::ops(before));
        let op = governing(
            &workspace.admin,
            &workspace.admin_key,
            reached,
            &[GovernanceRow::Settings],
        );
        let batch = changes_after(before, |doc| {
            doc.put(ROOT, "name", "Renamed").unwrap();
            crate::members::put_op(doc, &op).unwrap();
        });
        let admin = hex::encode(workspace.admin.device_public_key);
        let admitted = admit(
            Some(before),
            member_of(&admin, &workspace.admin),
            &workspace.owner.user_public_key,
            &batch,
        )
        .expect("an administrator holds the settings action");
        assert_eq!(name_of(&admitted.candidate).as_deref(), Some("Renamed"));

        let reached = crate::members::heads(&crate::members::ops(&admitted.candidate));
        let op = governing(
            &workspace.admin,
            &workspace.admin_key,
            reached,
            &[GovernanceRow::Checkpoints],
        );
        let batch = changes_after(&admitted.candidate, |doc| {
            crate::coverage::put(doc, coverage(1)).unwrap();
            crate::members::put_op(doc, &op).unwrap();
        });
        let admitted = admit(
            Some(&admitted.candidate),
            member_of(&admin, &workspace.admin),
            &workspace.owner.user_public_key,
            &batch,
        )
        .expect("an administrator holds the action its checkpoints take");
        assert_eq!(
            crate::coverage::entries(&admitted.candidate).unwrap().len(),
            1
        );
    }

    phase a_governance_row_its_writers_role_does_not_permit_is_refused;
    // The same operation from a role that does not hold the row's action is
    // refused, and the refusal names the row and the action: an editor holds no
    // governance action, so its coverage write changes nothing.
    {
        let workspace = started();
        let before = &workspace.doc;
        let reached = crate::members::heads(&crate::members::ops(before));
        let op = governing(
            &workspace.editor,
            &workspace.editor_key,
            reached,
            &[GovernanceRow::Checkpoints],
        );
        let batch = changes_after(before, |doc| {
            crate::coverage::put(doc, coverage(1)).unwrap();
            crate::members::put_op(doc, &op).unwrap();
        });
        let editor = hex::encode(workspace.editor.device_public_key);
        match admit(
            Some(before),
            member_of(&editor, &workspace.editor),
            &workspace.owner.user_public_key,
            &batch,
        ) {
            Err(WorkspaceRefusal::Membership(refusal)) => {
                for named in ["the workspace's checkpoints", "settings"] {
                    assert!(
                        refusal.contains(named),
                        "the refusal names {named}: {refusal}"
                    );
                }
            }
            other => panic!("an editor writes no checkpoints: {other:?}"),
        }
    }

    phase an_operation_names_exactly_the_governance_its_batch_changes;
    // The operation names what its batch changed and nothing else: a change no
    // operation names, or a row an operation names that no change of the batch
    // wrote, is refused.
    {
        let workspace = started();
        let before = &workspace.doc;
        let reached = crate::members::heads(&crate::members::ops(before));
        let admin = hex::encode(workspace.admin.device_public_key);
        let unnamed = governing(&workspace.admin, &workspace.admin_key, reached.clone(), &[]);
        let batch = changes_after(before, |doc| {
            doc.put(ROOT, "name", "Renamed").unwrap();
            crate::members::put_op(doc, &unnamed).unwrap();
        });
        assert_eq!(
            admit(
                Some(before),
                member_of(&admin, &workspace.admin),
                &workspace.owner.user_public_key,
                &batch,
            )
            .unwrap_err(),
            WorkspaceRefusal::GovernanceWriter,
            "no operation names the change"
        );
        let spare = governing(
            &workspace.admin,
            &workspace.admin_key,
            reached,
            &[GovernanceRow::Settings, GovernanceRow::Versions],
        );
        let batch = changes_after(before, |doc| {
            doc.put(ROOT, "name", "Renamed").unwrap();
            crate::members::put_op(doc, &spare).unwrap();
        });
        assert_eq!(
            admit(
                Some(before),
                member_of(&admin, &workspace.admin),
                &workspace.owner.user_public_key,
                &batch,
            )
            .unwrap_err(),
            WorkspaceRefusal::GovernanceWriter,
            "an operation names a row the batch did not change"
        );
        // A batch that writes a row and puts it back as it was, as a server
        // detached and named again among the holders, still names it.
        let reached = crate::members::heads(&crate::members::ops(before));
        let put_back = governing(
            &workspace.admin,
            &workspace.admin_key,
            reached,
            &[GovernanceRow::Settings],
        );
        let batch = changes_after(before, |doc| {
            doc.put(ROOT, "name", "Renamed").unwrap();
            doc.put(ROOT, "name", "Workspace").unwrap();
            crate::members::put_op(doc, &put_back).unwrap();
        });
        let admitted = admit(
            Some(before),
            member_of(&admin, &workspace.admin),
            &workspace.owner.user_public_key,
            &batch,
        )
        .expect("an operation names a row its batch wrote and put back");
        assert_eq!(name_of(&admitted.candidate).as_deref(), Some("Workspace"));
    }

    phase a_reader_relays_an_operation_that_proves_its_writer;
    // A device that only reads the workspace relays the operations it holds: the
    // operation proves its writer, so who sent it says nothing about who wrote it.
    {
        let workspace = started();
        let before = &workspace.doc;
        let guest = rooted(0x53).1;
        let reached = crate::members::heads(&crate::members::ops(before));
        let op = admitting(&workspace.admin, &workspace.admin_key, reached, &guest);
        let batch = changes_after(before, |doc| {
            crate::members::put_op(doc, &op).unwrap();
        });
        let server = hex::encode(workspace.viewer.device_public_key);
        let admitted = admit(
            Some(before),
            member_of(&server, &workspace.viewer),
            &workspace.owner.user_public_key,
            &batch,
        )
        .expect("the operation is the administrator's, whoever relayed it");
        assert_eq!(
            crate::members::replay(&admitted.candidate, WORKSPACE).role_of(&person_of(&guest)),
            Some(Role::Commenter)
        );
    }
}

/// A device whose person root `seed` names: its certificate is signed by that
/// root, so a workspace document can name its person in its rows.
fn rooted(seed: u8) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device {seed}"),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

/// Put `cert`'s device among `doc`'s device rows.
fn list_device(doc: &mut AutoCommit, cert: &DeviceCert) {
    let rows = match doc.get(ROOT, "peer_identities_v1").unwrap() {
        Some((_, rows)) => rows,
        None => doc
            .put_object(ROOT, "peer_identities_v1", automerge::ObjType::Map)
            .unwrap(),
    };
    let identity = crate::state::GovernedIdentity {
        device_cert: cert.clone(),
        revocations: Vec::new(),
        admission: None,
        admissions: Vec::new(),
        delegate: None,
        workspace_delegations: Vec::new(),
        removed: false,
    };
    doc.put(
        &rows,
        hex::encode(cert.device_public_key),
        ScalarValue::Bytes(identity.encode().unwrap()),
    )
    .unwrap();
    doc.commit();
}

fn person_of(cert: &DeviceCert) -> String {
    hex::encode(cert.user_public_key)
}

fn member_of<'a>(key: &'a str, cert: &DeviceCert) -> Writer<'a> {
    Writer {
        key,
        access: SenderAccess::Member {
            person: cert.user_public_key,
            foreign: false,
        },
    }
}

/// An operation `cert` writes on top of `parents`, admitting `who` as a
/// commenter.
fn admitting(
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
    parents: Vec<crate::members::OpId>,
    who: &DeviceCert,
) -> crate::members::SignedMembershipOp {
    crate::members::SignedMembershipOp::signed(
        cert,
        key,
        parents,
        crate::workspace::saved_shortcut::ns_at(NOW),
        crate::members::MembershipOpCommand::Project {
            changes: vec![tilo_action_policy::Change::AddMember {
                person: person_of(who),
                role: Role::Commenter,
            }],
        },
    )
    .unwrap()
}

/// A workspace its owner filled with an administrator, an editor and a viewer.
struct Started {
    doc: AutoCommit,
    owner: DeviceCert,
    admin_key: ed25519_dalek::SigningKey,
    admin: DeviceCert,
    editor_key: ed25519_dalek::SigningKey,
    editor: DeviceCert,
    viewer: DeviceCert,
}

fn started() -> Started {
    let (owner_key, owner) = owner_device();
    let (admin_key, admin) = rooted(0x21);
    let (editor_key, editor) = rooted(0x31);
    let (_, viewer) = rooted(0x41);
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "workspace", WORKSPACE).unwrap();
    doc.put(ROOT, "owner", person_of(&owner)).unwrap();
    founded_maps(&mut doc);
    let mut changes: Vec<Change> = [
        (&admin, Role::Moderator),
        (&editor, Role::Editor),
        (&viewer, Role::Viewer),
    ]
    .into_iter()
    .map(|(who, role)| Change::AddMember {
        person: person_of(who),
        role,
    })
    .collect();
    changes.push(Change::Grant(roles::administrator_grant(
        WORKSPACE,
        &person_of(&admin),
        Grantee::person(person_of(&owner)),
    )));
    sign(&mut doc, &(owner_key, owner.clone()), changes);
    doc.commit();
    Started {
        doc,
        owner,
        admin_key,
        admin,
        editor_key,
        editor,
        viewer,
    }
}

/// An operation `cert` writes on top of `parents`, naming the governance rows
/// its batch changes.
fn governing(
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
    parents: Vec<crate::members::OpId>,
    rows: &[GovernanceRow],
) -> crate::members::SignedMembershipOp {
    crate::members::SignedMembershipOp::signed(
        cert,
        key,
        parents,
        crate::workspace::saved_shortcut::ns_at(NOW),
        crate::members::MembershipOpCommand::Governance {
            rows: rows.to_vec(),
        },
    )
    .unwrap()
}

fn name_of(doc: &AutoCommit) -> Option<String> {
    match doc.get(ROOT, "name").ok().flatten() {
        Some((automerge::Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Str(name) => Some(name.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Two devices holding the workspace document its founder seeded: the founder
/// seeds it once, and every other device starts from the changes it sent.
fn two_devices_founded_by_seed() -> (AutoCommit, AutoCommit) {
    let (_, cert) = owner_device();
    let seed = crate::seed_workspace(WORKSPACE, &cert, std::slice::from_ref(&cert)).unwrap();
    let mut a = crate::workspace::candidate(None, &seed).unwrap();
    a.set_actor(automerge::ActorId::from(vec![0x51; 32]));
    let mut b = crate::workspace::candidate(None, &seed).unwrap();
    b.set_actor(automerge::ActorId::from(vec![0x52; 32]));
    (a, b)
}

/// Two devices writing their first checkpoint coverage without seeing each
/// other keep both routes after a merge: they write into the map the workspace
/// is founded with.
#[test]
fn two_devices_first_checkpoint_coverage_both_survive_a_merge() {
    let mut first = coverage(1);
    first.app = "first.app".into();
    let mut second = coverage(1);
    second.app = "second.app".into();
    let (mut a, mut b) = two_devices_founded_by_seed();
    crate::coverage::put(&mut a, first).unwrap();
    crate::coverage::put(&mut b, second).unwrap();
    a.commit();
    b.commit();
    tilo_automerge::ApplyIncoming::merge_incoming(&mut a, &mut b).unwrap();
    let held = crate::coverage::entries(&a).unwrap();
    assert_eq!(held.len(), 2, "both routes' coverage keeps: {held:?}");
}

/// Two devices writing their first durability receipts without seeing each
/// other keep both routes after a merge.
#[test]
fn two_devices_first_durability_receipts_both_survive_a_merge() {
    let mut first = coverage(1);
    first.app = "first.app".into();
    let mut second = coverage(1);
    second.app = "second.app".into();
    let first = crate::durability::DurableReceipt::new(first, 2).unwrap();
    let second = crate::durability::DurableReceipt::new(second, 2).unwrap();
    let (mut a, mut b) = two_devices_founded_by_seed();
    crate::durability::put(
        &mut a,
        BTreeMap::from([(crate::coverage::route_key("first.app", "note"), first)]),
    )
    .unwrap();
    crate::durability::put(
        &mut b,
        BTreeMap::from([(crate::coverage::route_key("second.app", "note"), second)]),
    )
    .unwrap();
    a.commit();
    b.commit();
    tilo_automerge::ApplyIncoming::merge_incoming(&mut a, &mut b).unwrap();
    let held = crate::durability::entries(&a).unwrap();
    assert_eq!(held.len(), 2, "both routes' receipts keep: {held:?}");
}

/// Another device of the owner's: the owner's root certifies a key of its own.
fn owners_other_device(seed: u8) -> DeviceCert {
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device {seed}"),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    cert
}

/// The workspace document `founder` seeds, started over from a base `bases` times, as a
/// governor does at each admission.
fn founded_by(founder: &DeviceCert, bases: u64) -> AutoCommit {
    let seed = crate::seed_workspace(WORKSPACE, founder, std::slice::from_ref(founder)).unwrap();
    let mut doc = crate::workspace::candidate(None, &seed).unwrap();
    for _ in 0..bases {
        doc = super::super::base::rebase(&doc, automerge::ActorId::random()).unwrap();
    }
    doc
}

/// The owner's restored computer, which never held the workspace, founds one of its own under
/// the workspace's id. Neither its founding nor any base of it, however many admissions it went
/// through, replaces what the host holds or merges beside it; the host's own next base still
/// replaces the document as before.
#[test]
fn a_workspace_founded_again_under_its_id_replaces_nothing_its_host_holds() {
    let (host_key, host) = owner_device();
    let mut held = founded_by(&host, 1);
    hold_document(&mut held, "file:/plan.md");
    held.commit();
    let restored = owners_other_device(0x77);
    let restored_key = hex::encode(restored.device_public_key);
    for (bases, what) in [
        (0, "its founding"),
        (1, "a base as old as the host's"),
        (5, "a newer base"),
    ] {
        let refounded = all_changes(&founded_by(&restored, bases));
        assert_eq!(
            admit(
                Some(&held),
                member(&restored_key, owner_root()),
                &owner_root(),
                &refounded,
            )
            .map(|admitted| admitted.rebased),
            Err(WorkspaceRefusal::Refounded),
            "{what} of another founding is refused"
        );
        // A receiver refusing it does not take itself for behind its governor.
        assert!(!matches!(
            super::super::base::lineage(Some(&held), &refounded),
            super::super::base::Lineage::Newer { .. }
        ));
    }
    let unsigned =
        all_changes(&super::super::base::rebase(&held, automerge::ActorId::random()).unwrap());
    assert_eq!(
        admit(
            Some(&held),
            member(&restored_key, owner_root()),
            &owner_root(),
            &unsigned,
        )
        .map(|admitted| admitted.rebased),
        Err(WorkspaceRefusal::Compaction(
            super::super::compaction::CompactionRefusal::Unsigned
        )),
        "a base no device signed replaces nothing"
    );
    let next = all_changes(
        &super::super::compaction::compact(
            &held,
            automerge::ActorId::random(),
            (&host, &host_key),
            WORKSPACE,
            1,
        )
        .unwrap(),
    );
    let admitted = admit(
        Some(&held),
        member(&restored_key, owner_root()),
        &owner_root(),
        &next,
    )
    .expect("the compaction the owner's device signed is the workspace's next base");
    assert!(
        admitted.rebased,
        "a newer base of the same founding replaces the document"
    );
    assert_eq!(
        crate::workspace::policy_of(&admitted.candidate, WORKSPACE),
        crate::workspace::policy_of(&held, WORKSPACE),
        "the base holds the workspace's authority as the document held it"
    );
}

/// The workspace its owner founded and made `admin_root` an administrator of, and the device
/// that keeps its history: `admin_device()`.
fn administrator_keeps() -> (AutoCommit, (ed25519_dalek::SigningKey, DeviceCert), String) {
    let (_, host) = owner_device();
    let mut held = founded_by(&host, 1);
    sign(
        &mut held,
        &owner_device(),
        vec![
            Change::AddMember {
                person: person_id(admin_root()),
                role: Role::Editor,
            },
            Change::Grant(roles::administrator_grant(
                WORKSPACE,
                &person_id(admin_root()),
                Grantee::person(person_id(owner_root())),
            )),
        ],
    );
    held.commit();
    let admin = admin_device();
    let admin_key = hex::encode(admin.1.device_public_key);
    (held, admin, admin_key)
}

/// An administrator keeping the workspace's history makes someone else an administrator, then
/// the workspace's next base: a device keeping it adopts that base, whose governance the
/// operation it adds says it changes, as it would take the operation in a batch. Handing the
/// workspace to someone, which only its owner does, changes nothing in a base as in a batch.
#[test]
fn a_newer_base_an_administrator_made_carries_the_administrators_it_made() {
    let (held, admin, admin_key) = administrator_keeps();
    let rebased_after = |edit: &dyn Fn(&mut AutoCommit)| {
        let mut made = held.clone();
        edit(&mut made);
        made.commit();
        all_changes(
            &super::super::compaction::compact(
                &made,
                automerge::ActorId::random(),
                (&admin.1, &admin.0),
                WORKSPACE,
                1,
            )
            .unwrap(),
        )
    };
    let based = |changes: Vec<Change>| {
        rebased_after(&|made: &mut AutoCommit| sign(made, &admin, changes.clone()))
    };
    let adopted = |changes: &[Vec<u8>]| {
        admit_workspace_changes(
            WorkspaceAdmission {
                before: Some(&held),
                workspace: WORKSPACE,
                source: member(&admin_key, admin_root()),
                owner: &owner_root(),
                administrators: &[admin_root()],
                now_ns: crate::workspace::saved_shortcut::ns_at(NOW),
                anchors: &[],
            },
            changes,
        )
    };

    let made = adopted(&based(vec![
        Change::AddMember {
            person: person_id(newcomer_root()),
            role: Role::Editor,
        },
        Change::Grant(roles::administrator_grant(
            WORKSPACE,
            &person_id(newcomer_root()),
            Grantee::person(person_id(admin_root())),
        )),
    ]))
    .unwrap_or_else(|failure| panic!("the administrator's base is adopted: {failure:?}"));
    assert!(made.rebased, "the base replaces the document");
    assert!(
        crate::workspace::policy_of(&made.candidate, WORKSPACE)
            .is_administrator(&person_id(newcomer_root())),
        "the base carries the administrator it made"
    );

    let handed = adopted(&based(vec![Change::TransferOwnership {
        person: person_id(newcomer_root()),
    }]))
    .unwrap_or_else(|failure| panic!("the administrator's base is kept: {failure:?}"));
    let policy = crate::workspace::policy_of(&handed.candidate, WORKSPACE);
    assert!(
        policy.is_owner(&person_id(owner_root())) && !policy.is_owner(&person_id(newcomer_root())),
        "an administrator's base does not hand the workspace to anyone"
    );

    refuses_a_base_whose_changes_its_author_may_not_make(&held, rebased_after, adopted);
}

/// A base whose operation names a change its author has no grant for is refused whole, as is one
/// changing the workspace's governance with no operation naming the change.
fn refuses_a_base_whose_changes_its_author_may_not_make(
    held: &AutoCommit,
    rebased_after: impl Fn(&dyn Fn(&mut AutoCommit)) -> Vec<Vec<u8>>,
    adopted: impl Fn(&[Vec<u8>]) -> Result<AdmittedWorkspace, AdmissionFailure>,
) {
    let (stranger_key, stranger) = rooted(0x71);
    let reached = crate::members::heads(&crate::members::ops(held));
    let renamed_by_stranger = rebased_after(&|made: &mut AutoCommit| {
        made.put(ROOT, "name", "Renamed").unwrap();
        let op = governing(
            &stranger,
            &stranger_key,
            reached.clone(),
            &[GovernanceRow::Settings],
        );
        crate::members::put_op(made, &op).unwrap();
    });
    match adopted(&renamed_by_stranger) {
        Err(AdmissionFailure::Refused(WorkspaceRefusal::Membership(refusal))) => assert!(
            refusal.contains("is not the writer's"),
            "the refusal names what the stranger changed: {refusal}"
        ),
        other => {
            panic!("a base renamed by someone the workspace grants nothing is refused: {other:?}")
        }
    }
    let renamed_unnamed = rebased_after(&|made: &mut AutoCommit| {
        made.put(ROOT, "name", "Renamed").unwrap();
    });
    assert!(
        matches!(
            adopted(&renamed_unnamed),
            Err(AdmissionFailure::Refused(
                WorkspaceRefusal::GovernanceWriter
            ))
        ),
        "a base renaming the workspace with no operation naming it is refused"
    );
}
