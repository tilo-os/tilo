//! What an edit does to the text: the splice, replace and repeat
//! primitives, a secret field's masking, what assistive technology and a binding
//! see.
use super::super::*;
use super::draw_to_populate;
use super::drive;
use super::mods;
use crate::testing::Dispatch;
use automerge::ObjType;
use automerge::transaction::Transactable as _;
use tilo_ui_base::{
    synthetic_input::char_key, synthetic_input::cursor_moved, synthetic_input::mouse_press,
    synthetic_input::mouse_release, synthetic_input::named_key,
};

tilo_test_env::scenario! {
    /// Editing a field: its primitives, its accessibility, and its bound text.
    /// Each phase was a test of its own and builds its own fixture.
    fn edits_bindings_and_accessibility;

    phase accessibility_exposes_role_name_value_and_grapheme_actions;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            use tilo_ui_base::{a11y::AccessibleTextSelection, a11y::Role};
            let mut ti = TextInput::new(Vec::new());
            ti.text = "hello".to_string();
            ti.core.set_selection(1, 4);
            let node = Widget::accessibility(&ti).unwrap();
            assert_eq!(node.role, Role::TextInput);
            assert_eq!(node.value.as_deref(), Some("hello"));
            assert!(node.state.focusable, "enabled input is focusable");
            assert!(!node.state.disabled);
            assert!(!node.state.protected);
            assert_eq!(
                node.text_selection,
                Some(AccessibleTextSelection {
                    anchor: 1,
                    focus: 4,
                })
            );
        }

        {
            let input = TextInput::local()
                .with_placeholder("name@example.com")
                .with_accessible_name("Email address");
            let node = Widget::accessibility(&input).unwrap();
            assert_eq!(node.name.as_deref(), Some("Email address"));
            assert_ne!(node.name.as_deref(), Some(input.placeholder()));
        }

        {
            use tilo_ui_base::a11y::Action;

            let family = "👩‍👩‍👧‍👦";
            let original = format!("a{family}b");
            let mut ti = TextInput::local();
            ti.text = original.clone();
            let mut doc = AutoCommit::new();
            let obj = doc
                .put_object(&automerge::ROOT, "body", ObjType::Text)
                .unwrap();
            doc.splice_text(&obj, 0, 0, &original).unwrap();
            ti.bind_text(obj.clone());

            assert!(Widget::perform_action(
                &mut ti,
                &Action::SetSelection {
                    anchor: 1,
                    focus: 2,
                },
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(
                ti.selection_range_public(),
                Some((1, 1 + family.len())),
                "one AccessKit character selects the whole ZWJ family"
            );

            assert!(Widget::perform_action(
                &mut ti,
                &Action::SetValue("new value".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(ti.value(), "new value");
            assert_eq!(doc.text(&obj).unwrap(), original, "the owner writes it");
            assert_eq!(ti.take_dirty().as_deref(), Some("new value"));
        }

        {
            let ml = TextInput::local().multiline(true);
            assert!(
                Widget::accessibility(&ml).unwrap().state.multiline,
                "multiline flagged in a11y"
            );
            let single = TextInput::local();
            assert!(!Widget::accessibility(&single).unwrap().state.multiline);
        }

        // A field bound to a document's text reads it, never writes it, and reports the edit the
        // app is to make.
        {
            use tilo_ui_base::a11y::Action;
            let mut document = AutoCommit::new();
            let title = document
                .put_object(&automerge::ROOT, "title", ObjType::Text)
                .unwrap();
            document.splice_text(&title, 0, 0, "Draft").unwrap();
            let mut input = TextInput::local();
            input.bind_text(title.clone());
            input.sync(&document, &automerge::ROOT);
            assert_eq!(input.value(), "Draft");
            assert!(
                input.take_dirty().is_none(),
                "showing the text is not an edit"
            );

            assert!(Widget::perform_action(
                &mut input,
                &Action::SetSelection {
                    anchor: 0,
                    focus: 0,
                },
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert_eq!(input.value(), "Draft", "moving the caret changes no text");

            assert!(Widget::perform_action(
                &mut input,
                &Action::SetValue("Project Atlas".into()),
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert_eq!(
                document.text(&title).unwrap(),
                "Draft",
                "the input never writes the document"
            );
            assert_eq!(input.take_dirty().as_deref(), Some("Project Atlas"));
        }
    }
}

tilo_test_env::scenario! {
    /// A secret field.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_secret_field_masks_and_keeps_its_value;

    phase secret_input_masks_rendering_and_accessibility_but_keeps_the_value;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let mut ti = TextInput::local().with_secret(true);

            let mut doc = AutoCommit::new();
            let obj_id = doc
                .put_object(&automerge::ROOT, "password", ObjType::Text)
                .unwrap();
            let _ = doc.splice_text(&obj_id, 0, 0, "hunter2");
            ti.bind_text(obj_id);

            ti.sync(&doc, &automerge::ROOT);
            assert_eq!(ti.text, "hunter2", "stored value must be the real secret");

            assert_eq!(ti.display_text(), "•••••••");
            // The masked prefix tracks character count, not bytes.
            assert_eq!(ti.display_prefix(3), "•••");

            let mut plain = TextInput::new(Vec::new());
            plain.text = "hunter2".to_string();
            assert_eq!(plain.display_text(), "hunter2");
            assert_eq!(plain.display_prefix(3), "hun");
        }

        {
            let buffer = Arc::new(Mutex::new(String::new()));
            let mut input = TextInput::local()
                .with_external_buffer(Arc::clone(&buffer))
                .with_secret(true);
            input.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
            input.text = "correct horse battery staple".into();
            input.dirty = true;
            input.layout_dirty = true;
            let document = AutoCommit::new();
            input.sync(&document, &automerge::ROOT);

            assert_eq!(&*buffer.lock().unwrap(), "correct horse battery staple");
            assert!(
                input.layout_needs_measure(),
                "publication must not consume layout invalidation"
            );
            buffer.lock().unwrap().clear();
            input.sync(&document, &automerge::ROOT);
            assert!(
                buffer.lock().unwrap().is_empty(),
                "an acknowledged secret must not be republished"
            );
            input.core.set_caret(input.text.len());
            input.insert_at_cursor("new draft");
            input.sync(&document, &automerge::ROOT);
            assert_eq!(
                &*buffer.lock().unwrap(),
                input.value(),
                "subsequent edits still publish"
            );
            assert!(buffer.lock().unwrap().ends_with("new draft"));
            assert!(document.keys(&automerge::ROOT).next().is_none());
        }

        {
            let mut ti = TextInput::new(Vec::new()).with_secret(true);
            ti.text = "hunter2".to_string();
            let node = Widget::accessibility(&ti).unwrap();
            assert_eq!(
                node.value, None,
                "a secret value must not leak into the a11y tree"
            );
            assert!(node.state.protected, "secret input is marked protected");
            assert_eq!(
                node.text_selection, None,
                "selection offsets must not reveal a secret's length"
            );
        }

        for kind in [TextInputKind::Password, TextInputKind::Secret] {
            let mut ti = TextInput::new(Vec::new()).with_kind(kind);
            ti.text = "JBSWY3DPEHPK3PXP".to_string();
            let node = Widget::accessibility(&ti).unwrap();
            assert_eq!(node.value, None, "{kind:?}: a key is not exported");
            assert!(node.state.protected, "{kind:?}: a key field is protected");
            assert_eq!(node.text_selection, None, "{kind:?}: no selection offsets");
            assert_eq!(
                Widget::ime_content(&ti),
                Some(tilo_ui_base::ime::InputContent::PASSWORD),
                "{kind:?}: no input-method suggestions learn the key"
            );
        }

        {
            let mut ti = TextInput::new(Vec::new()).with_secret(true);
            ti.text = "hunter2".to_string();
            assert_eq!(ti.display_text(), "•••••••", "masked by default");

            ti.toggle_reveal();
            assert!(ti.is_revealed());
            assert_eq!(
                ti.display_text(),
                "hunter2",
                "revealed shows the real glyphs"
            );
            let node = Widget::accessibility(&ti).unwrap();
            assert_eq!(
                node.value, None,
                "revealed value still must not reach the a11y tree"
            );
            assert!(
                node.state.protected,
                "still marked protected while revealed"
            );

            ti.toggle_reveal();
            assert_eq!(ti.display_text(), "•••••••", "toggles back to masked");
        }

        {
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("s3cr3t");
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            ti.set_cursor_pos(0);
            drive(
                &mut ti,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                mods(false, false),
            );
            assert_eq!(ti.cursor_pos(), 1, "Right moved one real char boundary");
        }
    }

    phase secret_field_carries_a_show_hide_control_off_the_tab_order;
    {
        use tilo_ui_base::{a11y::Action, a11y::Role};

        let _settled = tilo_ui_base::motion::testing::settled();
        {
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("hunter2");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
            assert!(!ti.is_revealed(), "a secret starts hidden");
            assert_eq!(ti.display_text(), "•••••••");
            assert!(ti.has_reveal_control());
            let control = ti.reveal_control_rect().unwrap();
            assert_eq!(
                control,
                Rect::new(200.0, 0.0, 240.0, 40.0),
                "trailing square"
            );

            let children = Widget::children(&ti);
            assert_eq!(children.len(), 1, "the control is a real child widget");
            let child = children.first().unwrap();
            assert!(
                !child.is_focusable(),
                "an inline accessory is not a Tab stop"
            );
            let node = child.accessibility().unwrap();
            assert_eq!(node.role, Role::Button);
            assert_eq!(node.name.as_deref(), Some("Show password"));
            assert_eq!(
                Widget::tooltip_target_at(&ti, control.center()).map(|(_, tip)| tip),
                Some("Show password".to_string())
            );
            assert_eq!(
                Widget::cursor_shape(&ti, control.center()),
                Some(winit::window::CursorIcon::Pointer)
            );
        }

        {
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("hunter2");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
            let mut doc = AutoCommit::new();
            let activated = Widget::children_mut(&mut ti)
                .into_iter()
                .next()
                .unwrap()
                .perform_action(
                    &Action::Activate,
                    &[],
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                );
            assert!(activated, "Enter/Space and AT activation toggle");
            assert!(ti.is_revealed());
            assert_eq!(ti.display_text(), "hunter2");
            let child = Widget::children(&ti).into_iter().next().unwrap();
            assert_eq!(
                child.accessibility().unwrap().name.as_deref(),
                Some("Hide password")
            );
            let field = Widget::accessibility(&ti).unwrap();
            assert_eq!(field.value, None, "revealing never exposes the value");
            assert!(field.state.protected);
            assert!(Widget::text_review(&ti).is_none());
            assert!(
                doc.keys(&automerge::ROOT).next().is_none(),
                "nothing persisted"
            );

            let _ = Widget::children_mut(&mut ti)
                .into_iter()
                .next()
                .unwrap()
                .perform_action(
                    &Action::Activate,
                    &[],
                    &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                );
            assert!(!ti.is_revealed(), "a second activation hides it again");
        }

        {
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("hunter2");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
            ti.set_focus(true);
            draw_to_populate(&mut ti);
            let caret = ti.cursor_pos();
            let mut doc = AutoCommit::new();
            let target = ti.reveal_control_rect().unwrap().center();
            drive(
                &mut ti,
                &mut doc,
                &cursor_moved(target.x, target.y),
                mods(false, false),
            );

            let mut sender = Dispatch::new().doc(&mut doc);
            let press = sender.send(&mut ti, &mouse_press());
            let release = sender.send(&mut ti, &mouse_release());
            let handled = press.handled | release.handled;
            let focus_target = release.focus_target.or(press.focus_target);
            assert!(handled);
            assert!(ti.is_revealed(), "a click reveals");
            assert_eq!(focus_target, None, "the click does not move focus");
            assert!(ti.is_focused());
            assert_eq!(ti.cursor_pos(), caret, "the caret stays where it was");
        }

        {
            let mut ti = TextInput::local().with_secret(true);
            let mut taffy = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let root = ti.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let (order, trapped) = crate::focus_navigation::order(root, &ti, &taffy, &node_map);
            assert!(!trapped);
            assert_eq!(order, [root], "the field alone, not its show/hide control");
        }

        {
            let plain = TextInput::local();
            assert!(!plain.has_reveal_control());
            assert!(Widget::children(&plain).is_empty());
            let multiline = TextInput::local().with_secret(true).multiline(true);
            assert!(!multiline.has_reveal_control());
        }

        {
            let mut ti = TextInput::local().with_secret(true).disabled(true);
            ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 40.0));
            let child = Widget::children(&ti).into_iter().next().unwrap();
            assert!(!child.is_focusable());
            assert!(child.accessibility().unwrap().state.disabled);
        }
    }
}

#[test]
fn a_secret_never_reaches_the_clipboard() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let services = crate::process::ProcessServices::null();
    let _host = crate::process::enter(&services);
    let clipboard = || tilo_ui_base::text_selection::clipboard_paste();

    for revealed in [false, true] {
        {
            tilo_ui_base::text_selection::clipboard_copy("before");
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("s3cr3t");
            if revealed {
                ti.toggle_reveal();
            }
            ti.set_focus(true);
            let mut doc = AutoCommit::new();

            drive(&mut ti, &mut doc, &char_key("a"), mods(true, false));
            assert_eq!(ti.selected_str(), Some("s3cr3t"), "Select All still works");
            drive(&mut ti, &mut doc, &char_key("c"), mods(true, false));
            assert_eq!(
                clipboard().as_deref(),
                Some("before"),
                "revealed: {revealed}"
            );
            drive(&mut ti, &mut doc, &char_key("x"), mods(true, false));
            assert_eq!(
                clipboard().as_deref(),
                Some("before"),
                "revealed: {revealed}"
            );
            assert_eq!(ti.value(), "s3cr3t", "a refused cut keeps the value");
            assert_eq!(ti.selected_str(), Some("s3cr3t"), "and the selection");
        }

        {
            tilo_ui_base::text_selection::clipboard_copy("pasted");
            let mut ti = TextInput::local()
                .with_secret(true)
                .with_initial_value("s3cr3t")
                .with_text_action("translate", "Translate");
            if revealed {
                ti.toggle_reveal();
            }
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            ti.select_all();
            ti.open_context_menu([20.0, 18.0]);
            let labels = ti.context_menu_labels();
            assert!(!labels.contains(&"Cut".to_string()), "got {labels:?}");
            assert!(!labels.contains(&"Copy".to_string()), "got {labels:?}");
            assert!(!labels.contains(&"Translate".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Paste".to_string()), "got {labels:?}");
            assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");
        }

        {
            tilo_ui_base::text_selection::clipboard_copy("hunter2");
            let mut ti = TextInput::local().with_secret(true);
            if revealed {
                ti.toggle_reveal();
            }
            ti.set_focus(true);
            let mut doc = AutoCommit::new();
            drive(&mut ti, &mut doc, &char_key("v"), mods(true, false));
            assert_eq!(ti.value(), "hunter2");
        }
    }
}
