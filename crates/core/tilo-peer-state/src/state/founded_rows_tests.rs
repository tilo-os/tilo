//! Rows two devices write into maps the workspace is founded with: neither
//! device sees the other's first row, and a merge keeps both.
//!
//! A map put at one document key by two devices that cannot see each other is
//! two maps, and a merge keeps only one: the other device's rows are gone. The
//! maps these rows sit in therefore exist from the founding, before any device
//! writes into them.

use super::*;
use automerge::{AutoCommit, ROOT, ReadDoc};
use ed25519_dalek::SigningKey;
use tilo_automerge::ApplyIncoming as _;
use tilo_user_identity::DeviceCert;

/// Two peers founded from one workspace, each writing as a different device of
/// its owner, and the signing key of each.
fn pair() -> (
    StatePeer,
    SigningKey,
    DeviceCert,
    StatePeer,
    SigningKey,
    DeviceCert,
    SystemTime,
) {
    let (first, first_key, second_key, second_cert, now) = crate::admission_tests::seeded();
    let first_cert = first
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key != second_cert.device_public_key)
        .expect("the seed names both devices");
    let second = first.clone();
    (
        first,
        first_key,
        first_cert,
        second,
        second_key,
        second_cert,
        now,
    )
}

/// A workspace document as its founding leaves it, with `device`'s changes
/// its own.
fn founded(device: &DeviceCert) -> AutoCommit {
    let mut doc = seeded_doc();
    doc.set_actor(automerge::ActorId::from(device.device_public_key.to_vec()));
    doc
}

/// Two documents founded from one workspace, each a different device's.
fn founded_pair() -> (AutoCommit, AutoCommit) {
    let (_, _, cert_a, _, _, cert_b, _) = pair();
    (founded(&cert_a), founded(&cert_b))
}

/// A workspace document as its founding leaves it, at a fixed time: the two
/// devices of [`founded_pair`] found one history, which only holds if the
/// founding is stamped at the same second on both.
fn seeded_doc() -> AutoCommit {
    let (_, _, _, _, _, cert, _) = pair();
    let seed = crate::seed_workspace_at(
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        &cert,
        std::slice::from_ref(&cert),
        0,
    )
    .expect("a workspace seed");
    crate::workspace::candidate(None, &seed).expect("a founded workspace")
}

/// `a`'s document with `b`'s merged in: the same merge a sync makes.
fn merged(a: &AutoCommit, b: &AutoCommit) -> AutoCommit {
    let mut held = a.clone();
    let mut other = b.clone();
    held.merge_incoming(&mut other).expect("a merge");
    held
}

/// The map at `key`, and the number of maps the key holds: a founded key has
/// exactly one.
fn foundations(doc: &AutoCommit, key: &str) -> (Vec<String>, usize) {
    let values = doc.get_all(ROOT, key).expect("the key reads");
    let maps = values
        .iter()
        .filter(|(value, _)| matches!(value, automerge::Value::Object(_)))
        .count();
    let keys = match values.first() {
        Some((_, id)) => doc.keys(id).collect(),
        None => Vec::new(),
    };
    (keys, maps)
}

/// A receipt of an effect `by`'s device served.
fn receipt(by: &DeviceCert, at: i64) -> tilo_desktop_model::WorkspaceEffectReceipt {
    let invocation = tilo_actions::InvocationId::new();
    let context = tilo_actions::ActionContext::for_resource("workspace", "workspace", "Workspace");
    tilo_desktop_model::WorkspaceEffectReceipt::recorded(
        invocation,
        &tilo_actions::MemberId::person(hex::encode(by.user_public_key)).into(),
        context,
        tilo_actions::EffectReceiptStatus::Succeeded,
        at,
    )
}

fn preset(name: &str) -> tilo_action_policy::Preset {
    tilo_action_policy::Preset {
        name: name.into(),
        description: String::new(),
        rules: vec![tilo_action_policy::Rule::new(
            tilo_action_policy::TreePath::root().app("so.tilo.notes"),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
        )],
    }
}

fn capability(id: &str) -> collab_types::caps::CapOverride {
    collab_types::caps::CapOverride::grant(
        tilo_action_policy::Grantee::person("ada"),
        collab_types::caps::Scope::FsPath("/var/workspace".into()),
        collab_types::caps::Access::Read,
    )
    .with_id(id)
}

fn import(proposal: u8) -> crate::document_import::ImportEntry {
    use collab_types::document_import::{DocumentImportProposalV1, DocumentImportTerms};
    let key = ed25519_dalek::SigningKey::from_bytes(&[proposal; 32]);
    crate::document_import::ImportEntry {
        proposal: DocumentImportProposalV1::new(
            &DocumentImportTerms {
                workspace: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
                proposal_id: [proposal; 32],
                app: "so.tilo.notes".into(),
                document_id: "uuid:0f2b1c00-0000-4000-8000-000000000001".into(),
                title: "Groceries".into(),
                document: b"doc".to_vec(),
                issued_unix: 1,
                expires_unix: 100,
            },
            &key,
        )
        .expect("a signed proposal"),
        decision: None,
    }
}

#[test]
fn two_devices_first_effect_receipts_both_survive_a_merge() {
    let (mut a, _, cert_a, mut b, _, cert_b, now) = pair();
    let first = receipt(&cert_a, 1);
    let second = receipt(&cert_b, 2);
    a.workspace_mut()
        .apply_own_effect_receipt(&cert_a, &first, now)
        .unwrap();
    b.workspace_mut()
        .apply_own_effect_receipt(&cert_b, &second, now)
        .unwrap();
    let both = merged(
        a.core.workspace_doc.as_ref().unwrap(),
        b.core.workspace_doc.as_ref().unwrap(),
    );
    let held = tilo_desktop_model::projection::workspace_effect_receipts_of(&both);
    assert!(
        held.contains(&first) && held.contains(&second),
        "both devices' first receipts are kept: {held:?}"
    );
    let (_, maps) = foundations(
        &both,
        tilo_desktop_model::projection::WORKSPACE_EFFECT_RECEIPTS_KEY,
    );
    assert_eq!(maps, 1, "every device writes into the founded map");
}

#[test]
fn two_devices_first_effect_history_anchors_both_survive_a_merge() {
    let (mut a, _, cert_a, mut b, _, cert_b, now) = pair();
    let first = [11; 32];
    let second = [22; 32];
    a.workspace_mut()
        .set_workspace_effect_history_root(&cert_a, first, now)
        .unwrap();
    b.workspace_mut()
        .set_workspace_effect_history_root(&cert_b, second, now)
        .unwrap();
    let both = merged(
        a.core.workspace_doc.as_ref().unwrap(),
        b.core.workspace_doc.as_ref().unwrap(),
    );
    a.core.workspace_doc = Some(both.into());
    assert_eq!(
        a.workspace()
            .workspace_effect_history_root(&cert_a)
            .unwrap(),
        Some(first),
        "the first device's anchor is kept"
    );
    assert_eq!(
        a.workspace()
            .workspace_effect_history_root(&cert_b)
            .unwrap(),
        Some(second),
        "the second device's anchor is kept"
    );
}

#[test]
fn two_devices_first_invitation_parts_both_survive_a_merge() {
    let mut a = seeded_doc();
    let mut b = a.fork();
    let first = vec![1_u8; 100];
    let second = vec![2_u8; 100];
    a.set_actor(automerge::ActorId::from(vec![3; 32]));
    b.set_actor(automerge::ActorId::from(vec![4; 32]));
    crate::state::invitation_receipts::put_parts(&mut a, std::slice::from_ref(&first)).unwrap();
    crate::state::invitation_receipts::put_parts(&mut b, std::slice::from_ref(&second)).unwrap();
    a.commit();
    b.commit();
    let both = merged(&a, &b);
    let held = crate::state::invitation_receipts::parts(&both).unwrap();
    assert_eq!(
        held.len(),
        2,
        "both devices' first parts are kept: {held:?}"
    );
    let (_, maps) = foundations(&both, crate::state::invitation_receipts::PARTS_KEY);
    assert_eq!(maps, 1, "every device writes into the founded map");
}

#[test]
fn two_devices_first_grant_sets_both_survive_a_merge() {
    let (mut a, mut b) = founded_pair();
    tilo_desktop_model::projection::project_workspace_presets(&mut a, &[preset("admins")]).unwrap();
    tilo_desktop_model::projection::project_workspace_presets(&mut b, &[preset("devs")]).unwrap();
    a.commit();
    b.commit();
    let both = merged(&a, &b);
    let held = tilo_desktop_model::projection::workspace_presets_of(&both);
    let names: Vec<&str> = held.iter().map(|preset| preset.name.as_str()).collect();
    assert_eq!(names, ["admins", "devs"], "both grant sets are kept");
}

#[test]
fn two_devices_first_sandbox_capabilities_both_survive_a_merge() {
    let (mut a, mut b) = founded_pair();
    tilo_desktop_model::projection::project_workspace_sandbox(&mut a, &[capability("one")])
        .unwrap();
    tilo_desktop_model::projection::project_workspace_sandbox(&mut b, &[capability("two")])
        .unwrap();
    a.commit();
    b.commit();
    let both = merged(&a, &b);
    let held = tilo_desktop_model::projection::workspace_sandbox_of(&both);
    let ids: Vec<&str> = held.iter().map(|cap| cap.id.as_str()).collect();
    assert_eq!(ids, ["one", "two"], "both capabilities are kept");
}

#[test]
fn two_devices_first_import_proposals_both_survive_a_merge() {
    let (mut a, mut b) = founded_pair();
    crate::document_import::put_entry(&mut a, &import(1)).unwrap();
    crate::document_import::put_entry(&mut b, &import(2)).unwrap();
    a.commit();
    b.commit();
    let both = merged(&a, &b);
    assert_eq!(
        crate::document_import::entries(&both).len(),
        2,
        "both proposals are kept"
    );
}

#[test]
fn two_devices_first_removal_cuts_both_survive_a_merge() {
    let (mut a, mut b) = founded_pair();
    let first = crate::removals::Held {
        last: 3,
        compactions: Vec::new(),
    };
    let second = crate::removals::Held {
        last: 5,
        compactions: Vec::new(),
    };
    crate::removals::write(
        &mut a,
        &[7; 32],
        &std::collections::BTreeMap::from([(
            crate::coverage::route_key("test.app", "tab:1"),
            first,
        )]),
    )
    .unwrap();
    crate::removals::write(
        &mut b,
        &[8; 32],
        &std::collections::BTreeMap::from([(
            crate::coverage::route_key("test.app", "tab:1"),
            second,
        )]),
    )
    .unwrap();
    a.commit();
    b.commit();
    let both = merged(&a, &b);
    let held = crate::removals::recorded(&both).unwrap();
    assert!(
        held.contains(&[7; 32]) && held.contains(&[8; 32]),
        "both removal cuts are kept: {held:?}"
    );
    let (_, maps) = foundations(&both, crate::removals::FIELD_V1);
    assert_eq!(maps, 1, "every device writes into the founded map");
}
