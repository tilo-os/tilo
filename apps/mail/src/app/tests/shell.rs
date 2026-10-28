use super::*;

/// The mailbox starts with a selection; wide shows three columns, narrow
/// pushes the message and the drawer, and reflowing restores the columns.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn pane_layout_reflows_between_wide_and_narrow_and_navigates_folders() {
    {
        let (app, _) = app();
        assert!(app.shared.borrow().selected.is_some());
        assert_eq!(app.shared.borrow().folder, Folder::Inbox);
    }

    {
        let (mut app, _doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let geometry = app.nav.geometry();
        assert!(
            geometry.sidebar_column.width() > 1.0,
            "docked sidebar present"
        );
        assert!(
            geometry.overview_column.x0 >= geometry.sidebar_column.x1 - 0.5,
            "list right of sidebar"
        );
        assert!(
            geometry.content_column.x0 >= geometry.overview_column.x1 - 0.5,
            "reader right of list"
        );
        assert!(!app.shared.borrow().narrow);
    }

    // Phase `narrow_mailboxes_open_over_a_settled_layout`: the header
    // rows of the columns behind the mailboxes screen are laid out
    // hidden, and the layout still settles (it panics in
    // `layout_for_test` otherwise).
    {
        let (mut app, _doc) = app();
        app.nav.open_sidebar();
        app.nav.snap();
        app.layout_for_test(560.0, 860.0);
        assert!(app.shared.borrow().narrow);
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(560.0, 860.0);
        assert!(app.shared.borrow().narrow, "560 separates list and message");
        assert_eq!(app.nav_pane(), NavPane::Overview, "starts on the list");
        let row = first_row_centre(&app);
        assert!(click_at(&mut app, &mut doc, row), "row click consumed");
        app.layout_for_test(560.0, 860.0);
        assert_eq!(
            app.nav_pane(),
            NavPane::Content,
            "opening a message pushes it"
        );
        assert!(app.shared.borrow().detail_pushed);
        assert!(app.shared.borrow().selected.is_some());

        // Back is the window bar's slot; Escape goes back too.
        assert!(
            app.nav.can_go_back() && !app.nav.window_toolbar().leading.is_empty(),
            "Back shown in the window's bar while the message is pushed"
        );
        assert!(
            dispatch(&mut app, &esc_event(), &mut doc),
            "Escape consumed"
        );
        assert_eq!(
            app.nav_pane(),
            NavPane::Overview,
            "Escape returns to the list"
        );
    }

    {
        let (mut app, _doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let wide_content_x0 = app.nav.geometry().content_column.x0;
        app.layout_for_test(560.0, 860.0);
        assert!(app.shared.borrow().narrow, "narrow flag set at 560");
        let geometry = app.nav.geometry();
        let front = match app.nav_pane() {
            NavPane::Content => geometry.content_column,
            _ => geometry.overview_column,
        };
        assert!(
            (front.width() - 560.0).abs() < 2.0,
            "the front screen is full-width narrow: {geometry:?}"
        );
        app.layout_for_test(1280.0, 800.0);
        assert!(!app.shared.borrow().narrow, "wide flag re-derived at 1280");
        assert!(
            (app.nav.geometry().content_column.x0 - wide_content_x0).abs() < 2.0,
            "reflows back to three columns"
        );
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        app.shared
            .borrow_mut()
            .push_command(Command::OpenFolder(0, Folder::Sent));
        app.apply_commands(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.shared.borrow().folder, Folder::Sent);
        app.sync(&doc, &automerge::ROOT);
        let state = app.shared.borrow();
        let expected = state
            .selected
            .and_then(|id| state.mailbox.get(id))
            .map_or_else(|| "Sent".to_owned(), |message| message.subject.clone());
        assert_eq!(
            crate::window_bar::location(&state, None),
            expected,
            "the window's location names the open message, else the folder"
        );
    }

    // Phase `narrow_list_reaches_the_mailboxes_by_the_bars_toggle`: too
    // narrow to dock the mailboxes beside the list, they are the push
    // stack's first screen. The list is the top-level page: the window
    // bar's sidebar toggle reaches the mailboxes, with no Back beside it.
    {
        let (mut app, _doc) = app();
        app.layout_for_test(560.0, 860.0);
        let geometry = app.nav.geometry();
        assert_eq!(geometry.sidebar, ui::widget::SidebarPlacement::Screen);
        assert_eq!(app.nav_pane(), NavPane::Overview, "opens on the list");
        let toolbar = app.nav.window_toolbar();
        assert_eq!(toolbar.sidebar, Some(false), "the toggle is the bar's");
        assert!(
            !app.nav.can_go_back() && toolbar.leading.is_empty(),
            "no Back on the top-level list"
        );
        assert!(!app.nav.is_sidebar_open());
        assert!(
            app.nav
                .window_toolbar_event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar),
            "the toggle is answered"
        );
        app.nav.snap();
        app.layout_for_test(560.0, 860.0);
        assert_eq!(
            app.nav_pane(),
            NavPane::Sidebar,
            "the toggle shows the mailboxes"
        );
        assert!(app.nav.is_sidebar_open(), "the mailboxes fill the window");
        let sidebar = app.nav.geometry().sidebar_column;
        assert!(
            sidebar.x0.abs() < 2.0 && (sidebar.width() - 560.0).abs() < 2.0,
            "the mailboxes are the full-width front screen: {sidebar:?}"
        );
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn accessibility_routes_panes_reader_attachments_and_context_menu() {
    {
        use ui::a11y::{Action, Role as A11yRole};

        let (mut app, mut doc) = app();
        app.layout_for_test(560.0, 860.0);

        let overview = app.accessibility().expect("mailbox semantics");
        assert!(
            overview
                .children
                .iter()
                .any(|node| node.role == A11yRole::List),
            "the compact overview exposes its visible message list"
        );
        assert!(
            !overview
                .children
                .iter()
                .any(|node| node.role == A11yRole::Document),
            "the covered reader is omitted in the compact overview"
        );
        let message_target = overview
            .children
            .iter()
            .find(|node| node.role == A11yRole::List)
            .and_then(|list| {
                list.children
                    .iter()
                    .find(|node| node.role == A11yRole::ListItem)
            })
            .and_then(|row| row.action_target.clone())
            .expect("visible message row has a stable activation target");

        assert!(app.perform_semantic_action(
            &message_target,
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        app.layout_for_test(560.0, 860.0);
        assert_eq!(
            app.nav_pane(),
            NavPane::Content,
            "semantic open pushes reader"
        );
        let reader = app.accessibility().expect("reader semantics");
        assert!(
            reader
                .children
                .iter()
                .any(|node| node.role == A11yRole::Document),
            "the pushed compact reader exposes the selected conversation"
        );
        assert!(
            !reader
                .children
                .iter()
                .any(|node| node.role == A11yRole::List),
            "the covered compact overview is absent from AT traversal"
        );

        app.layout_for_test(1280.0, 800.0);
        assert!(app.perform_semantic_action(
            "mail.folder:0:archive",
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            app.shared.borrow().folder,
            Folder::Archive,
            "semantic folder activation uses the same selection state as the sidebar"
        );
    }

    {
        use ui::a11y::{Action, Role as A11yRole};

        let (mut app, mut doc) = app();
        let message_id = {
            let state = app.shared.borrow();
            state
                .mailbox
                .messages
                .values()
                .find(|message| message.attachments.iter().any(Attachment::is_downloadable))
                .map(|message| message.id)
                .expect("sample has a hydrated attachment")
        };
        assert!(app.shared.borrow_mut().preview_message(message_id));
        app.process_intents();
        let target = app
            .accessibility()
            .expect("reader semantics")
            .children
            .iter()
            .find(|node| node.role == A11yRole::Document)
            .and_then(|document| {
                document.children.iter().find_map(|child| {
                    child
                        .action_target
                        .as_ref()
                        .filter(|target| target.contains(":save-attachment:"))
                        .cloned()
                })
            })
            .expect("hydrated attachment is a named screen-reader button");
        assert!(app.perform_semantic_action(
            &target,
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(
            app.shared.borrow().status.is_none(),
            "the reader's button asks to save a downloaded attachment without a refusal"
        );
    }

    {
        use ui::a11y::{Action, Role as A11yRole};

        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let selected = app.shared.borrow().selected.expect("sample selection");
        let before_star = app.shared.borrow().mailbox.get(selected).unwrap().starred;
        let reader = app.accessibility().expect("reader semantics");
        let star_target = reader
            .children
            .iter()
            .find(|node| node.role == A11yRole::Document)
            .and_then(|document| document.children.first())
            .and_then(|toolbar| {
                toolbar
                    .children
                    .iter()
                    .find(|node| node.name.as_deref() == Some("Toggle star"))
            })
            .and_then(|button| button.action_target.clone())
            .expect("reader star control");
        assert!(app.perform_semantic_action(
            &star_target,
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert_eq!(
            app.shared.borrow().mailbox.get(selected).unwrap().starred,
            !before_star
        );
        let other = app
            .shared
            .borrow()
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| *id != selected)
            .expect("second sample message");
        app.shared
            .borrow_mut()
            .open_message(&mut ui::widget::Edits::lend(&mut doc), other);
        app.process_intents();
        assert!(
            !app.perform_semantic_action(
                &star_target,
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ),
            "a stale reader control cannot operate a newly selected message"
        );

        app.compose.open_blank();
        // Beside the list the composer stands where the message is read:
        // a part of the window, the list still published beside it.
        let window = app.accessibility().expect("window semantics");
        assert_eq!(window.role, A11yRole::Window);
        let compose = window
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Compose email"))
            .cloned()
            .expect("the composer is published in the window");
        assert_eq!(compose.role, A11yRole::Group);
        assert!(compose.children.iter().any(|node| {
            node.role == A11yRole::TextInput && node.name.as_deref() == Some("To")
        }));
        assert!(compose.children.iter().any(|node| {
            node.role == A11yRole::MultilineTextInput
                && node.name.as_deref() == Some("Message body")
        }));
        assert!(app.perform_semantic_action(
            "mail.compose:discard",
            &Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(
            !app.compose.open,
            "semantic discard uses compose close path"
        );
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
        app.shared.borrow_mut().push_intent(Intent::ContextMenu(
            id,
            Point::new(400.0, 300.0),
            Rect::new(280.0, 280.0, 620.0, 320.0),
        ));
        app.process_intents();
        let menu = app.accessibility().expect("context menu semantics");
        assert_eq!(menu.role, ui::a11y::Role::Menu);
        let archive = menu
            .children
            .iter()
            .position(|item| item.name.as_deref() == Some("Archive"))
            .expect("Archive is described to assistive technology");
        app.menu.click_item(archive);
        app.take_menu_choice(&mut ui::widget::Edits::lend(&mut doc));
        assert!(!app.menu.is_open());
        assert!(app.menu_target.is_none());
        assert_eq!(
            app.shared
                .borrow()
                .mailbox
                .get(id)
                .map(|message| message.folder),
            Some(Folder::Archive)
        );
    }
}
/// Opening (row, resource, Enter) records the read action; explicit preview (P,
/// resource preview) and the legacy read-only route stay read-neutral.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn opening_marks_read_through_the_document_action_and_preview_stays_neutral() {
    {
        let (app, mut doc) = app();
        let id = {
            let st = app.shared.borrow();
            st.mailbox
                .messages_in(Folder::Inbox)
                .into_iter()
                .find(|m| !m.read)
                .map(|m| m.id)
        }
        .expect("an unread message");
        app.shared
            .borrow_mut()
            .open_message(&mut ui::widget::Edits::lend(&mut doc), id);
        assert!(app.shared.borrow().mailbox.get(id).unwrap().read);
        let mut fresh = app.shared.borrow().mailbox.clone();
        for m in fresh.messages.values_mut() {
            m.read = false;
        }
        TestMail::apply_flags(&doc, &mut fresh);
        assert!(
            fresh.get(id).unwrap().read,
            "read flag persisted to the CRDT"
        );
    }

    {
        let (mut app, mut doc) = app();
        let id = app
            .shared
            .borrow()
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .find(|message| !message.read)
            .map(|message| message.id)
            .expect("sample contains an unread message");
        let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Open {
            target: crate::actions::MailTarget::Message(id.to_string()),
        });
        assert!(matches!(
            app.start_effect(tilo_actions::InvocationId::default(), &payload)
                .unwrap(),
            ui::framework::Effected::Done(_)
        ));
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert!(
            app.shared.borrow().mailbox.get(id).unwrap().read,
            "Open dispatches the canonical mark-read action"
        );
        assert_eq!(
            TestMail::get_read(&doc, &id.to_string()),
            Some(true),
            "resource Open writes the same mark-read every on-screen control does"
        );
        assert!(
            doc.get(&automerge::ROOT, "ui")
                .expect("document remains readable")
                .is_none(),
            "Open records no selection, folder, search, mode, or compose presentation state"
        );
    }

    {
        let (mut app, mut doc) = app();
        let ids: Vec<_> = app
            .shared
            .borrow()
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .filter(|message| !message.read)
            .map(|message| message.id)
            .take(2)
            .collect();
        assert_eq!(ids.len(), 2, "sample contains two unread inbox messages");

        for id in &ids {
            let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Open {
                target: crate::actions::MailTarget::Message(id.to_string()),
            });
            assert!(matches!(
                app.start_effect(tilo_actions::InvocationId::default(), &payload)
                    .unwrap(),
                ui::framework::Effected::Done(_)
            ));
        }
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);

        assert!(ids.iter().all(|id| {
            app.shared
                .borrow()
                .mailbox
                .get(*id)
                .is_some_and(|message| message.read)
        }));
    }

    // The removed read-only operation protocol cannot bypass the authoritative door.
    {
        let mut hosted =
            ui::framework::testing::TestApp::<crate::resource::MailShell>::capture(None).unwrap();
        let snapshot = |app: &mut MailApp| {
            let state = app.shared.borrow();
            (
                state.folder,
                state.selected,
                state.inspection,
                state.status.clone(),
                state
                    .mailbox
                    .messages
                    .values()
                    .map(|m| (m.id, m.read))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            )
        };
        let before = hosted.with_surface(snapshot).unwrap();
        let heads_before = hosted.read_document(|doc| doc.clone().get_heads());
        let error = hosted
            .harness()
            .call_on("mail.mailbox", "mail.operation", serde_json::json!({}))
            .expect_err("the removed protocol is not an admitted mutable operation");
        assert!(
            !error.to_string().is_empty(),
            "the host explains its refusal"
        );
        hosted.settle();
        assert_eq!(
            hosted.read_document(|doc| doc.clone().get_heads()),
            heads_before
        );
        assert_eq!(
            hosted.with_surface(snapshot).unwrap(),
            before,
            "refusal changes neither presentation nor any read flag"
        );
    }

    {
        let (mut app, mut doc) = app();
        let id = app
            .shared
            .borrow()
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .find(|message| !message.read)
            .map(|message| message.id)
            .expect("sample contains an unread message");
        let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Inspect {
            message: id.to_string(),
        });
        let heads_before_preview = doc.get_heads();
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &payload)
                .unwrap(),
            ui::framework::Effected::Done(_)
        ));
        assert_eq!(app.selected_message(), Some(id));
        assert!(
            !app.shared.borrow().mailbox.get(id).unwrap().read,
            "preview is read-state neutral before sync"
        );
        assert_eq!(doc.get_heads(), heads_before_preview);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert!(
            !app.shared.borrow().mailbox.get(id).unwrap().read,
            "unlike Open message, explicit preview never queues mark-read"
        );
        assert_ne!(
            TestMail::get_read(&doc, &id.to_string()),
            Some(true),
            "a read-state-neutral preview writes no read state"
        );
        let semantics = app.accessibility().expect("reader accessibility");
        let reader = semantics
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::Document)
            .expect("reader document");
        assert!(
            reader
                .description
                .as_deref()
                .is_some_and(|text| text.contains("without changing its read state")),
            "assistive technology receives the same explicit preview distinction"
        );
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let unread = app
            .shared
            .borrow()
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| {
                app.shared
                    .borrow()
                    .mailbox
                    .get(*id)
                    .is_some_and(|mail| !mail.read)
            })
            .expect("sample inbox has unread mail");
        app.shared.borrow_mut().selected = Some(unread);

        assert!(dispatch(&mut app, &named_key(NamedKey::Enter), &mut doc));
        assert!(
            app.shared
                .borrow()
                .mailbox
                .get(unread)
                .is_some_and(|mail| mail.read),
            "Enter uses the normal Open boundary, rather than silently previewing"
        );
        assert_eq!(TestMail::get_read(&doc, &unread.to_string()), Some(true));
        assert_ne!(app.shared.borrow().inspection, Some(unread));
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        let unread = app
            .shared
            .borrow()
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| {
                app.shared
                    .borrow()
                    .mailbox
                    .get(*id)
                    .is_some_and(|mail| !mail.read)
            })
            .expect("sample inbox has unread mail");
        app.shared.borrow_mut().selected = Some(unread);
        let heads_before = doc.get_heads();

        assert!(dispatch(&mut app, &char_key("p"), &mut doc));
        let state = app.shared.borrow();
        assert!(state.mailbox.get(unread).is_some_and(|mail| !mail.read));
        assert_eq!(state.inspection, Some(unread));
        assert_eq!(
            doc.get_heads(),
            heads_before,
            "preview does not author a mail action"
        );
    }
}
