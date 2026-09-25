use crate::{ChatApp, doc::ChatDoc, identity::LocalIdentity};
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ui::Widget;
#[test]
fn component_projection_never_seeds_or_marks_read() {
    let identity = LocalIdentity {
        id: "author".into(),
        name: "Author".into(),
        initials: "A".into(),
        color_idx: 1,
    };
    let mut doc = AutoCommit::new();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let before = (doc.base_heads(), doc.pending_ops());
    let mut app =
        ChatApp::from_document(&doc, identity, crate::app::ChatRuntime::Unavailable).unwrap();
    for _ in 0..3 {
        app.sync(&mut doc, &ROOT);
    }
    assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    assert!(!app.resources(&doc).is_empty());
}
#[test]
fn canonical_chat_repaints_only_for_document_changes_and_consumed_input() {
    use winit::event::{ElementState, MouseButton, WindowEvent};
    let identity = LocalIdentity::demo();
    let mut doc = AutoCommit::new();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let mut app =
        ChatApp::from_document(&doc, identity, crate::app::ChatRuntime::Unavailable).unwrap();
    app.sync(&mut doc, &ROOT);
    assert!(app.needs_redraw(), "the first sync paints");
    app.mark_drawn();
    for _ in 0..3 {
        app.sync(&mut doc, &ROOT);
    }
    assert!(
        !app.needs_redraw(),
        "an unchanged document must not repaint the canonical app"
    );

    let mut taffy = taffy::TaffyTree::<()>::new();
    let node = taffy.new_leaf(taffy::style::Style::default()).unwrap();
    let device_id = winit::event::DeviceId::dummy();
    let (mut handled, mut focus) = (false, None);
    app.handle_event(
        &WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(5.0, 5.0),
        },
        1.0,
        &Default::default(),
        &mut doc,
        &ROOT,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    assert!(
        !handled && !app.needs_redraw(),
        "unconsumed pointer motion must not repaint"
    );

    doc.put(ROOT, "remote-change", "newer").unwrap();
    doc.commit();
    app.sync(&mut doc, &ROOT);
    assert!(app.needs_redraw(), "a document change repaints");
    app.mark_drawn();
    app.sync(&mut doc, &ROOT);
    assert!(
        !app.needs_redraw(),
        "the change is absorbed after one paint"
    );

    let (mut handled, mut focus) = (false, None);
    app.handle_event(
        &WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        1.0,
        &Default::default(),
        &mut doc,
        &ROOT,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    assert!(app.needs_redraw(), "a press repaints");
}
#[test]
fn component_projection_refuses_unseeded_record() {
    assert!(
        ChatApp::from_document(
            &AutoCommit::new(),
            LocalIdentity::demo(),
            crate::app::ChatRuntime::Unavailable
        )
        .is_err()
    );
}

#[test]
fn component_local_view_restores_presentation_without_reverting_document() {
    use ui::widget::AppWidget;
    let mut doc = AutoCommit::new();
    let identity = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let mut app =
        ChatApp::from_document(&doc, identity.clone(), crate::app::ChatRuntime::Unavailable)
            .unwrap();
    app.scroll = 125.;
    app.inbox_open = true;
    app.left_flap.snap(false);
    let saved = app
        .capture_application_view()
        .unwrap()
        .expect("Chat local view");
    doc.put(ROOT, "remote-change", "newer").unwrap();
    doc.commit();
    let before = doc.base_heads();
    let mut restored =
        ChatApp::from_document(&doc, identity, crate::app::ChatRuntime::Unavailable).unwrap();
    restored.restore_application_view(&saved, &doc).unwrap();
    assert_eq!(restored.scroll, 125.);
    assert!(restored.inbox_open);
    assert_eq!(doc.base_heads(), before);
}
#[test]
fn canonical_idle_projection_does_not_request_perpetual_frames() {
    let mut doc = AutoCommit::new();
    let me = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &me.id, &me.name, &me.initials);
    doc.commit();
    let mut app = ChatApp::from_document(&doc, me, crate::app::ChatRuntime::Unavailable).unwrap();
    app.mark_drawn();
    assert!(!app.needs_redraw());
}
#[test]
fn canonical_composer_exports_ime_purpose_through_app_root() {
    let mut doc = AutoCommit::new();
    let me = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &me.id, &me.name, &me.initials);
    doc.commit();
    let app = ChatApp::from_document(&doc, me, crate::app::ChatRuntime::Unavailable).unwrap();
    assert!(app.is_focusable());
    assert!(app.ime_purpose().is_some());
}

#[test]
fn canonical_composer_semantics_follow_the_actual_editor_focus() {
    let (mut frame, _records, _checkout) = semantic_component_harness("chat-focused-composer");
    fn find<'a>(node: &'a ui::a11y::AccessNode, target: &str) -> Option<&'a ui::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    fn inspect<'a>(
        node: &'a ui::headless::InspectionNode,
        target: &str,
    ) -> Option<&'a ui::a11y::AccessNode> {
        node.semantics
            .as_ref()
            .and_then(|semantic| find(semantic, target))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| inspect(child, target))
            })
    }
    fn focused_count(node: &ui::headless::InspectionNode) -> usize {
        fn access(node: &ui::a11y::AccessNode) -> usize {
            usize::from(node.state.focused) + node.children.iter().map(access).sum::<usize>()
        }
        node.semantics.as_ref().map_or(0, access)
            + node.children.iter().map(focused_count).sum::<usize>()
    }

    let inspection = frame.inspection();
    let composer = inspect(&inspection.root, "chat.composer")
        .expect("the real component exports the composer target");
    assert!(composer.state.focusable);
    assert!(
        composer.state.focused,
        "the actual focused TextInput owns semantic focus without a keyboard ring"
    );
    assert_eq!(
        focused_count(&inspection.root),
        1,
        "Chat exports one semantic focus owner"
    );
    frame.close_components();
}

fn semantic_component_harness(
    profile: &str,
) -> (
    ui::headless::FrameHarness,
    ui::record::RecordStore,
    ui::record::RecordCheckout,
) {
    use std::{cell::RefCell, rc::Rc};
    use ui::application::{ComponentRunnerInputs, HostContext, LaunchTarget, RestoreInput};

    let cache = ui::portable_storage::PortableStorage::new(profile, "device").unwrap();
    let definition = crate::definition();
    let records = ui::record::RecordOwners::portable(cache.clone())
        .open(definition.app_id, "room")
        .unwrap();
    let views = Rc::new(ui::application_view::InstanceViewStore::portable(cache, "room").unwrap());
    let permissions = Rc::new(RefCell::new(
        ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
            vec![],
        ),
    ));
    let target = LaunchTarget::PrimaryNew;
    let context = HostContext::prepare(
        definition,
        records.clone(),
        views,
        "device".into(),
        1,
        permissions,
        ui::permissions::UserId("author".into()),
        tilo_actions::ActionContext::default(),
        &target,
    )
    .unwrap();
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    let checkout = component.record().clone();
    let font_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/sdk/ui-core/assets/fonts");
    let fonts = ui::font_catalog::FontCatalog::controlled_fixture(&font_root, "en-US");
    let frame = ui::headless::FrameHarness::from_component(
        component,
        (1280, 720),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::with_font_catalog(&fonts).unwrap(),
        },
    )
    .unwrap();
    (frame, records, checkout)
}

fn semantic_owner_path(node: &ui::headless::InspectionNode, target: &str) -> Option<String> {
    fn has_target(node: &ui::a11y::AccessNode, target: &str) -> bool {
        node.action_target.as_deref() == Some(target)
            || node.children.iter().any(|child| has_target(child, target))
    }
    if node
        .semantics
        .as_ref()
        .is_some_and(|semantic| has_target(semantic, target))
    {
        return Some(node.path.clone());
    }
    node.children
        .iter()
        .find_map(|child| semantic_owner_path(child, target))
}

fn semantic_target_rect(
    node: &ui::headless::InspectionNode,
    target: &str,
) -> Option<vello::kurbo::Rect> {
    fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<vello::kurbo::Rect> {
        if node.action_target.as_deref() == Some(target) {
            return node.bounds.map(ui::a11y::AccessBounds::to_rect);
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    node.semantics
        .as_ref()
        .and_then(|semantic| find(semantic, target))
        .or_else(|| {
            node.children
                .iter()
                .find_map(|child| semantic_target_rect(child, target))
        })
}

#[test]
fn semantic_composer_and_send_mutate_the_canonical_component_once() {
    let (mut frame, records, checkout) = semantic_component_harness("chat-semantic-route");
    let owner = semantic_owner_path(&frame.inspection().root, "chat.composer")
        .expect("the real component exports the composer target");
    let before = records
        .read(&checkout.id, |doc| {
            ChatDoc::top_level_count(doc, "c_general")
        })
        .unwrap();

    frame
        .perform_semantic_named(
            &owner,
            Some("chat.composer"),
            &ui::a11y::Action::SetValue("semantic route".into()),
        )
        .unwrap();
    frame
        .perform_semantic_named(&owner, Some("chat.send"), &ui::a11y::Action::Activate)
        .unwrap();

    let (after, draft, sent) = records
        .read(&checkout.id, |doc| {
            let messages = ChatDoc::list_messages(doc, "c_general");
            (
                messages
                    .iter()
                    .filter(|message| message.parent.is_none())
                    .count(),
                ChatDoc::draft_text(doc, "c_general"),
                messages.last().map(|message| message.text.clone()),
            )
        })
        .unwrap();
    assert_eq!(after, before + 1, "semantic Send appends exactly one row");
    assert_eq!(draft, "", "canonical Send consumes the canonical draft");
    assert_eq!(sent.as_deref(), Some("semantic route"));
    frame.close_components();
}

#[test]
fn assistive_composer_and_send_use_the_same_canonical_component_action() {
    let (mut frame, records, checkout) = semantic_component_harness("chat-assistive-route");
    let before = records
        .read(&checkout.id, |doc| {
            ChatDoc::top_level_count(doc, "c_general")
        })
        .unwrap();
    let composer = frame
        .a11y_node_named("Message composer")
        .expect("the composer is exported to assistive technology");
    frame
        .perform_a11y_action(
            composer,
            &ui::a11y::Action::SetValue("assistive route".into()),
        )
        .unwrap();
    let send = frame
        .a11y_node_named("Send message")
        .expect("Send is exported to assistive technology");
    frame
        .perform_a11y_action(send, &ui::a11y::Action::Activate)
        .unwrap();

    let (after, draft) = records
        .read(&checkout.id, |doc| {
            (
                ChatDoc::top_level_count(doc, "c_general"),
                ChatDoc::draft_text(doc, "c_general"),
            )
        })
        .unwrap();
    assert_eq!(after, before + 1);
    assert_eq!(draft, "");
    frame.close_components();
}

#[test]
fn captured_pointer_send_retry_does_not_duplicate_the_canonical_message() {
    let (mut frame, records, checkout) = semantic_component_harness("chat-pointer-retry");
    let inspection = frame.inspection();
    let owner = semantic_owner_path(&inspection.root, "chat.composer").unwrap();
    let send = semantic_target_rect(&inspection.root, "chat.send")
        .expect("the painted Send command has pointer bounds")
        .center();
    let before = records
        .read(&checkout.id, |doc| {
            ChatDoc::top_level_count(doc, "c_general")
        })
        .unwrap();
    frame
        .perform_semantic_named(
            &owner,
            Some("chat.composer"),
            &ui::a11y::Action::SetValue("one pointer command".into()),
        )
        .unwrap();

    frame.click_at(send.x, send.y);
    frame.click_at(send.x, send.y);

    let after = records
        .read(&checkout.id, |doc| {
            ChatDoc::top_level_count(doc, "c_general")
        })
        .unwrap();
    assert_eq!(
        after,
        before + 1,
        "retrying the captured command is idempotent once the draft is consumed"
    );
    frame.close_components();
}

#[test]
fn two_real_components_open_the_existing_recordless_chat_without_mutation() {
    use std::{cell::RefCell, rc::Rc};
    use ui::{
        application::{HostContext, LaunchTarget, RestoreInput},
        portable_storage::{PortableStorage, StorageKey},
        record::{RecordCheckout, RecordOwners},
    };
    let cache = PortableStorage::new("component-profile", "component-device").unwrap();
    let definition = crate::definition();
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
    let mut doc = AutoCommit::new();
    let identity = LocalIdentity::demo();
    // Existing app-state document envelope; opening it must not migrate or restamp.
    doc.put(ROOT, "qos:schema_version", 1u64).unwrap();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let heads = doc.base_heads();
    let actor = doc.get_actor().clone();
    cache
        .hydrate(
            StorageKey::Primary {
                package: definition.app_id.into(),
                workspace: room.into(),
            },
            doc.save(),
            Some(actor.to_bytes().to_vec()),
        )
        .unwrap();
    let records = RecordOwners::portable(cache.clone())
        .open(definition.app_id, room)
        .unwrap();
    let views = Rc::new(ui::application_view::InstanceViewStore::portable(cache, room).unwrap());
    let permissions = Rc::new(RefCell::new(
        ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
            vec![],
        ),
    ));
    let mut components = Vec::new();
    for instance in [1, 2] {
        let target = LaunchTarget::Record {
            checkout: checkout.clone(),
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            records.clone(),
            views.clone(),
            "component-device".into(),
            instance,
            permissions.clone(),
            ui::permissions::UserId("author".into()),
            tilo_actions::ActionContext::default(),
            &target,
        )
        .unwrap();
        components.push(
            definition
                .instantiate(context, target, RestoreInput::None)
                .unwrap(),
        );
    }
    assert_eq!(records.route(&checkout).unwrap().record, None);
    assert_eq!(
        records
            .read(&checkout.id, |d| (d.base_heads(), d.get_actor().clone()))
            .unwrap(),
        (heads, actor)
    );
    records
        .with(&checkout.id, |doc| {
            doc.put(ROOT, "peer-message", "shared").unwrap();
        })
        .unwrap();
    assert_eq!(components[0].record(), components[1].record());
    for component in &mut components {
        component.close();
    }
}

#[test]
fn semantic_composer_enter_shift_enter_and_empty_clear_use_the_real_input_path() {
    let (mut frame, records, checkout) = semantic_component_harness("chat-semantic-enter");
    let owner = semantic_owner_path(&frame.inspection().root, "chat.composer").unwrap();
    let before = records
        .read(&checkout.id, |doc| {
            ChatDoc::top_level_count(doc, "c_general")
        })
        .unwrap();
    frame
        .perform_semantic_named(&owner, Some("chat.composer"), &ui::a11y::Action::Focus)
        .unwrap();
    frame
        .perform_semantic_named(
            &owner,
            Some("chat.composer"),
            &ui::a11y::Action::SetValue("one message".into()),
        )
        .unwrap();
    frame
        .perform_semantic_named(
            &owner,
            Some("chat.composer"),
            &ui::a11y::Action::SetSelection {
                anchor: 11,
                focus: 11,
            },
        )
        .unwrap();
    frame.key("Enter");
    records
        .read(&checkout.id, |doc| {
            assert_eq!(ChatDoc::top_level_count(doc, "c_general"), before + 1);
            assert_eq!(ChatDoc::draft_text(doc, "c_general"), "");
            assert_eq!(
                ChatDoc::list_messages(doc, "c_general")
                    .last()
                    .unwrap()
                    .text,
                "one message"
            );
        })
        .unwrap();
    frame
        .perform_semantic_named(
            &owner,
            Some("chat.composer"),
            &ui::a11y::Action::SetValue("line".into()),
        )
        .unwrap();
    frame
        .perform_semantic_named(
            &owner,
            Some("chat.composer"),
            &ui::a11y::Action::SetSelection {
                anchor: 4,
                focus: 4,
            },
        )
        .unwrap();
    frame.set_modifiers(&["shift".into()]);
    frame.key("Enter");
    frame.set_modifiers(&[]);
    records
        .read(&checkout.id, |doc| {
            assert_eq!(ChatDoc::top_level_count(doc, "c_general"), before + 1);
            assert_eq!(ChatDoc::draft_text(doc, "c_general"), "line\n");
        })
        .unwrap();
    frame
        .perform_semantic_named(
            &owner,
            Some("chat.composer"),
            &ui::a11y::Action::SetValue(String::new()),
        )
        .unwrap();
    assert_eq!(
        records
            .read(&checkout.id, |doc| ChatDoc::draft_text(doc, "c_general"))
            .unwrap(),
        ""
    );
    frame.close_components();
}

/// The composer's exported value, as a browser bridge or screen reader sees it.
fn semantic_value(node: &ui::headless::InspectionNode, target: &str) -> Option<String> {
    fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<Option<String>> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node.value.clone());
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    node.semantics
        .as_ref()
        .and_then(|semantic| find(semantic, target))
        .or_else(|| {
            node.children
                .iter()
                .find_map(|child| semantic_value(child, target).map(Some))
        })
        .flatten()
}

/// Regression: the product workload sends message after message through the
/// real composer; the second send found a non-empty composer.
#[test]
fn consecutive_semantic_sends_leave_an_empty_composer_each_time() {
    let (mut frame, records, checkout) = semantic_component_harness("chat-consecutive-sends");
    let owner = semantic_owner_path(&frame.inspection().root, "chat.composer").unwrap();
    let before = records
        .read(&checkout.id, |doc| {
            ChatDoc::top_level_count(doc, "c_general")
        })
        .unwrap();
    for (n, text) in ["first message", "second message", "third message"]
        .into_iter()
        .enumerate()
    {
        frame
            .perform_semantic_named(&owner, Some("chat.composer"), &ui::a11y::Action::Focus)
            .unwrap();
        frame
            .perform_semantic_named(
                &owner,
                Some("chat.composer"),
                &ui::a11y::Action::SetValue(text.into()),
            )
            .unwrap();
        frame
            .perform_semantic_named(&owner, Some("chat.send"), &ui::a11y::Action::Activate)
            .unwrap();
        let (count, draft, last) = records
            .read(&checkout.id, |doc| {
                (
                    ChatDoc::top_level_count(doc, "c_general"),
                    ChatDoc::draft_text(doc, "c_general"),
                    ChatDoc::list_messages(doc, "c_general")
                        .last()
                        .map(|m| m.text.clone()),
                )
            })
            .unwrap();
        assert_eq!(count, before + n + 1, "send {n}");
        assert_eq!(draft, "", "send {n} consumes the draft");
        assert_eq!(last.as_deref(), Some(text));
        let shown = semantic_value(&frame.inspection().root, "chat.composer").unwrap_or_default();
        assert_eq!(shown, "", "send {n} leaves an empty composer");
    }
    frame.close_components();
}

/// The same through typing and a pointer click on Send (the browser bridge's
/// route), with the typed text arriving as keyboard input.
#[test]
fn consecutive_typed_sends_leave_an_empty_composer_each_time() {
    let (mut frame, records, checkout) = semantic_component_harness("chat-consecutive-typed");
    let inspection = frame.inspection();
    let composer = semantic_target_rect(&inspection.root, "chat.composer")
        .unwrap()
        .center();
    let send = semantic_target_rect(&inspection.root, "chat.send")
        .unwrap()
        .center();
    let before = records
        .read(&checkout.id, |doc| {
            ChatDoc::top_level_count(doc, "c_general")
        })
        .unwrap();
    for (n, text) in ["first typed", "second typed", "third typed"]
        .into_iter()
        .enumerate()
    {
        frame.click_at(composer.x, composer.y);
        frame.type_text(text);
        assert_eq!(
            records
                .read(&checkout.id, |doc| ChatDoc::draft_text(doc, "c_general"))
                .unwrap(),
            text,
            "typing {n} lands in the draft"
        );
        frame.click_at(send.x, send.y);
        let (count, draft) = records
            .read(&checkout.id, |doc| {
                (
                    ChatDoc::top_level_count(doc, "c_general"),
                    ChatDoc::draft_text(doc, "c_general"),
                )
            })
            .unwrap();
        assert_eq!(count, before + n + 1, "send {n}");
        assert_eq!(draft, "", "send {n} consumes the draft");
        let shown = semantic_value(&frame.inspection().root, "chat.composer").unwrap_or_default();
        assert_eq!(shown, "", "send {n} leaves an empty composer");
    }
    frame.close_components();
}
