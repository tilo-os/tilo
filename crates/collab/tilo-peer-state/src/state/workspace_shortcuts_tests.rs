use ed25519_dalek::SigningKey;
use tilo_automation::{Shortcut, ShortcutChange};

fn shortcut(id: &str) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: Vec::new(),
        steps: Vec::new(),
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    }
}

fn signed(
    room: &str,
    id: &str,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
) -> Shortcut {
    let mut shortcut = shortcut(id);
    crate::room::sign_saved_shortcut(room, &mut shortcut, cert, key).unwrap();
    shortcut
}

fn saved(peer: &crate::StatePeer) -> Vec<Shortcut> {
    tilo_desktop_model::projection::workspace_shortcuts_of(peer.admitted_room().unwrap())
}

#[test]
fn a_members_own_device_saves_and_removes_its_shortcut_in_the_room() {
    let (mut governor, authority, device, cert, now) = crate::admission_tests::seeded();
    let room = governor.core.room.clone();
    let water = signed(&room, "water", &cert, &device);
    let save = ShortcutChange::Save(Box::new(water.clone()));
    assert_eq!(
        governor
            .workspace_mut()
            .apply_shortcut_change(&cert, &save, &authority, now),
        Ok(true)
    );
    assert_eq!(saved(&governor), vec![water]);
    assert_eq!(
        governor
            .workspace_mut()
            .apply_shortcut_change(&cert, &save, &authority, now),
        Ok(false),
        "the same save again changes nothing"
    );
    let delete = ShortcutChange::Delete("water".into());
    assert_eq!(
        governor
            .workspace_mut()
            .apply_shortcut_change(&cert, &delete, &authority, now),
        Ok(true)
    );
    assert!(saved(&governor).is_empty());
    assert!(
        governor
            .workspace_mut()
            .apply_shortcut_change(&cert, &delete, &authority, now)
            .is_err(),
        "nothing to remove"
    );
}

#[test]
fn a_member_saves_only_as_themselves() {
    let (mut governor, authority, device, cert, now) = crate::admission_tests::seeded();
    let room = governor.core.room.clone();
    let (editor, _) = crate::admission_tests::add_editor(&mut governor, &authority, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);

    // The editor names the owner's person, under either device's proof.
    let mut forged = signed(&room, "water", &editor, &editor_key);
    forged.saved_by = Some(hex::encode(cert.user_public_key));
    for shortcut in [forged, signed(&room, "water", &cert, &device)] {
        assert!(
            governor
                .workspace_mut()
                .apply_shortcut_change(
                    &editor,
                    &ShortcutChange::Save(Box::new(shortcut)),
                    &authority,
                    now,
                )
                .is_err()
        );
    }
    // A shortcut naming the editor without proof.
    let mut unproven = shortcut("water");
    unproven.saved_by = Some(hex::encode(editor.user_public_key));
    assert!(
        governor
            .workspace_mut()
            .apply_shortcut_change(
                &editor,
                &ShortcutChange::Save(Box::new(unproven)),
                &authority,
                now,
            )
            .is_err()
    );
    assert!(saved(&governor).is_empty());

    let own = signed(&room, "water", &editor, &editor_key);
    assert_eq!(
        governor.workspace_mut().apply_shortcut_change(
            &editor,
            &ShortcutChange::Save(Box::new(own.clone())),
            &authority,
            now,
        ),
        Ok(true)
    );
    assert_eq!(saved(&governor), vec![own]);
}

#[test]
fn a_member_sees_a_saved_shortcut_and_never_the_governors_forgery() {
    let (mut governor, authority, _, _, now) = crate::admission_tests::seeded();
    let room = governor.core.room.clone();
    let (editor, _) = crate::admission_tests::add_editor(&mut governor, &authority, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    let governor_device = authority.verifying_key().to_bytes();
    let mut member = governor.clone();

    // The editor's save, committed by the governor, reaches the member.
    let before = governor.clone();
    let own = signed(&room, "water", &editor, &editor_key);
    governor
        .workspace_mut()
        .apply_shortcut_change(
            &editor,
            &ShortcutChange::Save(Box::new(own.clone())),
            &authority,
            now,
        )
        .unwrap();
    for message in governor
        .publication_mut()
        .durable_mutation_messages(&before, &authority, now, 2)
        .unwrap()
    {
        member.receive(governor_device, message, now).unwrap();
    }
    assert_eq!(saved(&member), vec![own.clone()]);

    // The governor writing a shortcut that names the editor, with no device
    // of the editor's having signed it, is refused where it arrives.
    let before = governor.clone();
    let mut forged = shortcut("ferns");
    forged.saved_by = Some(hex::encode(editor.user_public_key));
    let mut document = governor.admitted_room().unwrap().clone();
    tilo_desktop_model::projection::project_workspace_shortcuts(
        &mut document,
        &[own.clone(), forged],
    )
    .unwrap();
    document.commit();
    governor.core.room_doc = Some(document.into());
    for message in governor
        .publication_mut()
        .durable_mutation_messages(&before, &authority, now, 3)
        .unwrap()
    {
        let _ = member.receive(governor_device, message, now);
    }
    assert_eq!(saved(&member), vec![own], "the forgery never lands");
}
