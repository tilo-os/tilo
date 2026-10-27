use super::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn context_swipe_and_bulk_purge_share_confirmed_actions() {
    fn row_menu(app: &mut MailApp, id: MessageId) -> Vec<CtxAction> {
        app.shared.borrow_mut().push_intent(Intent::ContextMenu(
            id,
            Point::new(400.0, 300.0),
            Rect::new(280.0, 280.0, 620.0, 320.0),
        ));
        app.process_intents();
        assert!(app.menu.is_open(), "menu opened");
        app.menu_target
            .as_ref()
            .map(|target| target.items.clone())
            .unwrap_or_default()
    }

    {
        use ui::a11y::Role;

        let (mut app, mut doc) = app();
        let ids: Vec<MessageId> = app
            .shared
            .borrow()
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .take(2)
            .map(|message| message.id)
            .collect();
        {
            let mut mail = app.shared.borrow_mut();
            mail.set_bulk_selecting(true);
            for id in &ids {
                mail.mailbox.move_to(*id, Folder::Trash);
                mail.toggle_bulk_selection(*id);
            }
            mail.push_intent(Intent::ConfirmBulkPurge);
        }
        app.process_intents();
        assert!(app.bulk_purge_confirmation.is_some());
        assert!(
            ids.iter()
                .all(|id| app.shared.borrow().mailbox.get(*id).is_some()),
            "opening confirmation never performs a purge"
        );
        let dialog = app.accessibility().expect("modal accessibility");
        assert_eq!(dialog.role, Role::Dialog);

        assert!(dispatch(&mut app, &esc_event(), &mut doc));
        assert!(app.bulk_purge_confirmation.is_none());
        assert!(
            ids.iter()
                .all(|id| app.shared.borrow().mailbox.get(*id).is_some())
        );

        app.shared
            .borrow_mut()
            .push_intent(Intent::ConfirmBulkPurge);
        app.process_intents();
        // Return is the safe answer in a destructive confirmation.
        assert!(dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc));
        assert!(app.bulk_purge_confirmation.is_none());
        assert!(
            ids.iter()
                .all(|id| app.shared.borrow().mailbox.get(*id).is_some()),
            "Enter cancels a permanent delete"
        );

        app.shared
            .borrow_mut()
            .push_intent(Intent::ConfirmBulkPurge);
        app.process_intents();
        let confirmation = app
            .bulk_purge_confirmation
            .as_mut()
            .expect("the confirmation is open");
        assert!(Widget::perform_semantic_action(
            &mut confirmation.dialog,
            "modal.primary",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        app.settle_purge_confirmation(&mut ui::widget::Edits::lend(&mut doc));
        assert!(app.bulk_purge_confirmation.is_none());
        assert!(
            ids.iter()
                .all(|id| app.shared.borrow().mailbox.get(*id).is_none())
        );
    }

    {
        let (app, _doc) = app();
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        let menu = app.shared.borrow().context_items_for(id);
        assert!(
            menu.contains(&CtxAction::Archive),
            "the swipe's action is in the menu"
        );
        assert!(menu.contains(&CtxAction::Delete));
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        let items = row_menu(&mut app, id);
        let idx = items.iter().position(|a| *a == CtxAction::Archive).unwrap();
        let rect = app.menu.item_rect(idx).expect("archive item rect");
        let before = app.shared.borrow().mailbox.count(Folder::Archive);
        assert!(
            click_at(&mut app, &mut doc, rect.center()),
            "menu item click consumed"
        );
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Archive),
            before + 1,
            "clicking Archive archived the message"
        );
        assert!(!app.menu.is_open(), "menu dismissed after the action");
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        let items = row_menu(&mut app, id);
        let archive_index = items
            .iter()
            .position(|item| *item == CtxAction::Archive)
            .expect("archive is an offered menu command");
        for _ in 0..archive_index {
            assert!(dispatch(
                &mut app,
                &named_key(NamedKey::ArrowDown),
                &mut doc
            ));
        }
        let before = app.shared.borrow().mailbox.count(Folder::Archive);
        assert!(dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc));
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Archive),
            before + 1
        );
        assert!(!app.menu.is_open());
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        let _ = row_menu(&mut app, id);
        assert!(dispatch(&mut app, &char_key("e"), &mut doc));
        assert_eq!(
            app.shared.borrow().mailbox.get(id).map(|m| m.folder),
            Some(Folder::Archive)
        );
        assert!(!app.menu.is_open());
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn keyboard_shortcuts_drive_the_visible_triage_actions() {
    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        assert!(dispatch(&mut app, &char_key("?"), &mut doc));
        assert!(app.shortcuts.visible());
        let overlay = app.accessibility().expect("shortcut sheet semantics");
        assert_eq!(overlay.name.as_deref(), Some("Keyboard shortcuts"));
        let rows: Vec<&str> = overlay
            .children
            .iter()
            .filter_map(|row| row.name.as_deref())
            .collect();
        assert!(rows.contains(&"Open message: Enter"), "{rows:?}");
        assert!(
            rows.contains(&"Preview without marking read: P"),
            "{rows:?}"
        );
        assert!(dispatch(&mut app, &esc_event(), &mut doc));
        assert!(!app.shortcuts.visible(), "Escape dismisses the modal sheet");
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let before = app.shared.borrow().mailbox.count(Folder::Archive);
        let id = app.shared.borrow().selected.expect("a selected message");
        dispatch(&mut app, &char_key("e"), &mut doc);
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Archive),
            before + 1,
            "'e' archived the selection"
        );
        assert_eq!(
            TestMail::get_folder_for(&doc, &id.to_string()),
            Some(Folder::Archive)
        );
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let id = app.shared.borrow().selected.expect("selected message");

        assert!(dispatch(&mut app, &char_key("r"), &mut doc));
        assert!(app.compose.open, "R opens the visible reply composer");
        app.compose.close();

        let was_starred = app.shared.borrow().mailbox.get(id).unwrap().starred;
        assert!(dispatch(&mut app, &char_key("s"), &mut doc));
        assert_eq!(
            app.shared.borrow().mailbox.get(id).unwrap().starred,
            !was_starred
        );
        assert_eq!(
            TestMail::get_starred(&doc, &id.to_string()),
            Some(!was_starred)
        );

        assert!(dispatch(&mut app, &char_key("u"), &mut doc));
        assert!(!app.shared.borrow().mailbox.get(id).unwrap().read);
        assert_eq!(TestMail::get_read(&doc, &id.to_string()), Some(false));
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let first = app.shared.borrow().selected;
        dispatch(&mut app, &named_key(NamedKey::ArrowDown), &mut doc);
        assert_ne!(
            app.shared.borrow().selected,
            first,
            "ArrowDown moved the selection"
        );
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let before = app.shared.borrow().mailbox.count(Folder::Trash);
        let id = app.shared.borrow().selected.expect("a selected message");
        dispatch(&mut app, &named_key(NamedKey::Delete), &mut doc);
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Trash),
            before + 1,
            "Delete moved the selection to Trash"
        );
        assert_eq!(
            TestMail::get_folder_for(&doc, &id.to_string()),
            Some(Folder::Trash)
        );
    }
}
