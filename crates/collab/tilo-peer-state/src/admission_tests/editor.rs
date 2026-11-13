//! A workspace owned by another person, with the fixture's person its editor.
use super::*;

/// Sign `changes` into `doc` as the owner's device `by`.
pub(super) fn owner_signs(
    doc: &mut AutoCommit,
    by: &(SigningKey, DeviceCert),
    now: SystemTime,
    changes: Vec<Change>,
) {
    let at = crate::state::now_ns(now);
    crate::members::op::tests_support::sign_into(doc, by, at, changes);
    doc.commit();
}
/// The device of the person [`seeded_editor`]'s workspace is owned by.
pub(crate) fn other_owner() -> (SigningKey, DeviceCert) {
    crate::members::op::tests_support::device(200)
}
/// [`seeded`], with the workspace owned by another person ([`other_owner`],
/// whose device the returned governor key is) and the fixture's person an
/// Editor of it: a member whose grants a governor can narrow.
pub(crate) fn seeded_editor() -> (StatePeer, SigningKey, SigningKey, DeviceCert, SystemTime) {
    let (_, _, b, bc, now) = fixtures();
    let (owner_key, owner) = other_owner();
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let mut doc = crate::workspace::candidate(
        None,
        &seed_workspace(workspace, &owner, &[owner.clone(), bc.clone()]).unwrap(),
    )
    .unwrap();
    let person = hex::encode(bc.user_public_key);
    let role = collab_types::Role::Editor;
    let signer = (owner_key.clone(), owner.clone());
    owner_signs(
        &mut doc,
        &signer,
        now,
        vec![Change::AddMember { person, role }],
    );
    let workspace_changes = doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let mut native = StatePeer::new(
        workspace,
        owner.clone(),
        vec![owner, bc.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    native
        .install_seed(
            SeedState {
                workspace_changes,
                documents: vec![],
            },
            &owner_key,
            now,
        )
        .unwrap();
    (native, owner_key, b, bc, now)
}
