use crate::owner::testing::shortcut;
use ed25519_dalek::SigningKey;
use tilo_automation::{Shortcut, ShortcutChange};

fn signed(
    workspace: &str,
    id: &str,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
) -> Shortcut {
    let mut shortcut = shortcut(id);
    crate::workspace::sign_saved_shortcut(workspace, &mut shortcut, cert, key).unwrap();
    shortcut
}

fn saved(peer: &crate::StatePeer) -> Vec<Shortcut> {
    tilo_automation::decode_saved(&tilo_desktop_model::projection::workspace_shortcuts_of(
        peer.admitted_workspace().unwrap(),
    ))
}

tilo_test_env::scenario! {
    fn a_member_saves_shortcuts_only_as_themselves_through_their_own_device;

    phase a_members_own_device_saves_and_removes_its_shortcut_in_the_workspace;
    {
        let (mut owner_copy, _, device, cert, now) = crate::admission_tests::seeded();
        let workspace = owner_copy.core.workspace.clone();
        let water = signed(&workspace, "water", &cert, &device);
        let save = ShortcutChange::Save(Box::new(water.clone()));
        assert_eq!(
            owner_copy
                .workspace_mut()
                .apply_shortcut_change(&cert, &save, &device, now),
            Ok(true)
        );
        assert_eq!(saved(&owner_copy), vec![water]);
        assert_eq!(
            owner_copy
                .workspace_mut()
                .apply_shortcut_change(&cert, &save, &device, now),
            Ok(false),
            "the same save again changes nothing"
        );
        let delete = ShortcutChange::Delete("water".into());
        assert_eq!(
            owner_copy
                .workspace_mut()
                .apply_shortcut_change(&cert, &delete, &device, now),
            Ok(true)
        );
        assert!(saved(&owner_copy).is_empty());
        assert!(
            owner_copy
                .workspace_mut()
                .apply_shortcut_change(&cert, &delete, &device, now)
                .is_err(),
            "nothing to remove"
        );
    }

    phase a_shortcut_change_is_signed_by_its_callers_own_device;
    {
        let (mut owner_copy, authority, device, cert, now) = crate::admission_tests::seeded();
        let workspace = owner_copy.core.workspace.clone();
        let water = signed(&workspace, "water", &cert, &device);
        assert!(
            owner_copy
                .workspace_mut()
                .apply_shortcut_change(
                    &cert,
                    &ShortcutChange::Save(Box::new(water)),
                    &authority,
                    now
                )
                .is_err(),
            "another device, the owner's included, does not write it for the caller"
        );
        assert!(saved(&owner_copy).is_empty());
    }

    phase a_member_saves_only_as_themselves;
    {
        let (mut owner_copy, authority, device, cert, now) = crate::admission_tests::seeded();
        let workspace = owner_copy.core.workspace.clone();
        let (editor, _) = crate::admission_tests::add_editor(&mut owner_copy, &authority, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);

        // The editor names the owner's person, under either device's proof.
        let mut forged = signed(&workspace, "water", &editor, &editor_key);
        forged.saved_by = Some(hex::encode(cert.user_public_key));
        for shortcut in [forged, signed(&workspace, "water", &cert, &device)] {
            assert!(
                owner_copy
                    .workspace_mut()
                    .apply_shortcut_change(
                        &editor,
                        &ShortcutChange::Save(Box::new(shortcut)),
                        &editor_key,
                        now,
                    )
                    .is_err()
            );
        }
        // A shortcut naming the editor without proof.
        let mut unproven = shortcut("water");
        unproven.saved_by = Some(hex::encode(editor.user_public_key));
        assert!(
            owner_copy
                .workspace_mut()
                .apply_shortcut_change(
                    &editor,
                    &ShortcutChange::Save(Box::new(unproven)),
                    &editor_key,
                    now,
                )
                .is_err()
        );
        assert!(saved(&owner_copy).is_empty());

        let own = signed(&workspace, "water", &editor, &editor_key);
        assert_eq!(
            owner_copy.workspace_mut().apply_shortcut_change(
                &editor,
                &ShortcutChange::Save(Box::new(own.clone())),
                &editor_key,
                now,
            ),
            Ok(true)
        );
        assert_eq!(saved(&owner_copy), vec![own]);
    }

    phase a_member_sees_a_saved_shortcut_and_never_the_owners_forgery;
    {
        let (mut owner_copy, authority, _, _, now) = crate::admission_tests::seeded();
        let workspace = owner_copy.core.workspace.clone();
        let (editor, _) = crate::admission_tests::add_editor(&mut owner_copy, &authority, now);
        let editor_key = SigningKey::from_bytes(&[6; 32]);
        let owner_copy_device = authority.verifying_key().to_bytes();
        let mut member = owner_copy.clone();

        // The editor's save, relayed by the owner's device, reaches the member.
        let own = signed(&workspace, "water", &editor, &editor_key);
        owner_copy
            .workspace_mut()
            .apply_shortcut_change(
                &editor,
                &ShortcutChange::Save(Box::new(own.clone())),
                &editor_key,
                now,
            )
            .unwrap();
        member
            .receive(owner_copy_device, owner_copy.workspace_message(), now)
            .unwrap();
        assert_eq!(saved(&member), vec![own.clone()]);

        // The owner's device writing a shortcut that names the editor, with no
        // device of the editor's having signed it, is refused where it arrives.
        let mut forged = shortcut("ferns");
        forged.saved_by = Some(hex::encode(editor.user_public_key));
        let mut document = owner_copy.admitted_workspace().unwrap().clone();
        tilo_desktop_model::projection::project_workspace_shortcuts(
            &mut document,
            &[
                tilo_automation::SavedShortcut::encode(&own),
                tilo_automation::SavedShortcut::encode(&forged),
            ],
        )
        .unwrap();
        document.commit();
        owner_copy.core.workspace_doc = Some(document.into());
        let _ = member.receive(owner_copy_device, owner_copy.workspace_message(), now);
        assert_eq!(saved(&member), vec![own], "the forgery never lands");
    }

    phase a_saved_shortcut_is_written_as_one_of_the_workspaces_settings;
    {
        use crate::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};

        let (mut owner_copy, _, device, cert, now) = crate::admission_tests::seeded();
        let workspace = owner_copy.core.workspace.clone();
        let water = signed(&workspace, "water", &cert, &device);
        // What the session's action API writes when a person saves a shortcut.
        let save = WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
            ShortcutChange::Save(Box::new(water.clone())),
        ));
        assert_eq!(
            owner_copy
                .workspace_mut()
                .apply_workspace_membership_command(&cert, &save, &device, now),
            Ok(true)
        );
        assert_eq!(saved(&owner_copy), vec![water.clone()]);
        assert_eq!(
            owner_copy.workspace().workspace_shortcuts(),
            vec![tilo_automation::SavedShortcut::encode(&water)],
            "the shell and every reader see the shortcut the workspace holds"
        );

        let delete = WorkspaceMembershipCommand::Settings(SettingsChange::Shortcuts(
            ShortcutChange::Delete("water".into()),
        ));
        assert_eq!(
            owner_copy
                .workspace_mut()
                .apply_workspace_membership_command(&cert, &delete, &device, now),
            Ok(true)
        );
        assert!(saved(&owner_copy).is_empty());
    }
}
