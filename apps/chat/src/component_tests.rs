use crate::{ChatApp, doc::ChatDoc, identity::LocalIdentity};

/// The chat surface over `doc` for `identity`, editing as them, as a host
/// without native services mounts it, after its first sync.
fn projection(doc: &AutoCommit, identity: LocalIdentity) -> ChatApp {
    let me = identity.id.clone();
    let mut app = ChatApp::new(
        identity,
        crate::app::ChatRuntime::Unavailable,
        tilo_content::ContentStore::memory(),
    );
    app.attach_framework(ui::framework::Dispatcher::detached("so.tilo.chat", &me));
    app.sync(doc, &ROOT);
    app
}
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ui::Widget;

#[test]
fn browser_media_reply_enables_voice_without_native_attachment_access() {
    use ui::native_media::{Command, Snapshot, State};
    use ui::widget::Collaborative;
    let identity = LocalIdentity::demo();
    let mut doc = AutoCommit::new();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    let channel = ChatDoc::list_channels(&doc)
        .into_iter()
        .find(|channel| channel.kind == crate::model::ChannelKind::Voice)
        .unwrap();
    let mut app = projection(&doc, identity);
    assert!(!app.call_allows(crate::call::CallAction::Join));
    let request_id = app
        .take_collab_requests()
        .into_iter()
        .find_map(|request| match request {
            ui::collaboration::CollabRequest::NativeMediaCommand {
                request_id,
                command,
            } if matches!(command.command, Command::Status) => Some(request_id),
            _ => None,
        })
        .expect("the portable Chat surface polls its real media host");
    let mut idle = Snapshot::unavailable();
    idle.state = State::Idle;
    idle.issue = None;
    app.handle_collab_event(&ui::collaboration::CollabEvent::NativeMediaCommandResult {
        request_id,
        result: Ok(idle),
    });
    assert!(
        app.call_allows(crate::call::CallAction::Join),
        "a validated browser media reply enables voice without native file access"
    );
    let unavailable = app.effect_statuses();
    assert!(
        !unavailable.contains(&ui::framework::EffectStatus::unavailable(
            crate::effects::ChatEffect::JOIN_CALL,
            "no_services",
            crate::i18n::t("chat-voice-unavailable"),
        ))
    );
    assert!(
        unavailable.contains(&ui::framework::EffectStatus::unavailable(
            crate::effects::ChatEffect::ATTACH_FILE,
            "no_services",
            crate::i18n::t("chat-attachment-import-unavailable"),
        ))
    );
    app.native_voice_command(
        &channel.id,
        Command::Join {
            channel_id: channel.id.clone(),
        },
    )
    .unwrap();
    assert!(
        !app.external_services,
        "a media reply must not grant native paths"
    );
    assert!(!app.begin_attachment_import(std::path::Path::new("/fixture/private-file")));
    app.perms = crate::permissions::ChatPermissions::from_context(
        ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Viewer), "reader"),
            vec![],
        ),
    );
    assert!(app.call_allows(crate::call::CallAction::Join));
    assert!(
        !app.call_allows(crate::call::CallAction::Speak),
        "the live room role still gates publication"
    );
}

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
    let mut app = projection(&doc, identity);
    for _ in 0..3 {
        app.sync(&doc, &ROOT);
    }
    assert_eq!((doc.base_heads(), doc.pending_ops()), before);
}
#[test]
fn canonical_chat_repaints_only_for_document_changes_and_consumed_input() {
    use winit::event::{ElementState, MouseButton, WindowEvent};
    let identity = LocalIdentity::demo();
    let mut doc = AutoCommit::new();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let mut app = projection(&doc, identity);
    app.sync(&doc, &ROOT);
    assert!(app.needs_redraw(), "the first sync paints");
    app.mark_drawn();
    for _ in 0..3 {
        app.sync(&doc, &ROOT);
    }
    assert!(
        !app.needs_redraw(),
        "an unchanged document must not repaint the canonical app"
    );

    let device_id = winit::event::DeviceId::dummy();
    let handled = ui::testing::Dispatch::new()
        .doc(&mut doc)
        .send(
            &mut app,
            &WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(5.0, 5.0),
            },
        )
        .handled;
    assert!(
        !handled && !app.needs_redraw(),
        "unconsumed pointer motion must not repaint"
    );

    doc.put(ROOT, "remote-change", "newer").unwrap();
    doc.commit();
    app.sync(&doc, &ROOT);
    assert!(app.needs_redraw(), "a document change repaints");
    app.mark_drawn();
    app.sync(&doc, &ROOT);
    assert!(
        !app.needs_redraw(),
        "the change is absorbed after one paint"
    );

    ui::testing::Dispatch::new().doc(&mut doc).send(
        &mut app,
        &WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
    assert!(app.needs_redraw(), "a press repaints");
}
#[test]
fn component_local_view_restores_presentation_without_reverting_document() {
    use ui::application_view::ApplicationViewState;
    let mut doc = AutoCommit::new();
    let identity = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &identity.id, &identity.name, &identity.initials);
    doc.commit();
    let mut app = projection(&doc, identity.clone());
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
    let mut restored = projection(&doc, identity);
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
    let mut app = projection(&doc, me);
    app.sync(&doc, &ROOT);
    let _ = app.update(&doc, &ROOT);
    app.mark_drawn();
    assert!(!app.update(&doc, &ROOT), "an idle frame is not a change");
    assert!(
        !app.needs_animation_frame(),
        "idle Chat must not keep requesting frames"
    );
    assert!(!app.needs_redraw());
    doc.put(ROOT, "remote-change", "newer").unwrap();
    doc.commit();
    app.sync(&doc, &ROOT);
    assert!(
        app.needs_redraw(),
        "a conversation change still requests one paint"
    );
}
#[test]
fn canonical_composer_exports_ime_purpose_through_app_root() {
    let mut doc = AutoCommit::new();
    let me = LocalIdentity::demo();
    ChatDoc::seed_starter(&mut doc, &me.id, &me.name, &me.initials);
    doc.commit();
    let app = projection(&doc, me);
    assert!(app.is_focusable());
    assert!(app.ime_purpose().is_some());
}

#[test]
fn canonical_composer_semantics_follow_the_actual_editor_focus() {
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
    let (mut frame, _records, _checkout) = semantic_component_harness("chat-focused-composer");

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
    let cache = ui::portable_storage::PortableStorage::new(profile, "device").unwrap();
    component_mount(&cache, "author", 1)
}

/// One mount of the room's Chat, as the browser runtime makes it: the first
/// creates the workspace, a later one opens it.
fn component_mount(
    cache: &ui::portable_storage::PortableStorage,
    author: &str,
    instance: u64,
) -> (
    ui::headless::FrameHarness,
    ui::record::RecordStore,
    ui::record::RecordCheckout,
) {
    component_mount_in_room(cache, author, instance, "room")
}

fn component_mount_in_room(
    cache: &ui::portable_storage::PortableStorage,
    author: &str,
    instance: u64,
    room: &str,
) -> (
    ui::headless::FrameHarness,
    ui::record::RecordStore,
    ui::record::RecordCheckout,
) {
    use std::{cell::RefCell, rc::Rc};
    use ui::application::{
        ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    };

    let cache = cache.clone();
    let definition = crate::definition();
    let records = ui::record::RecordOwners::portable(cache.clone())
        .open(definition.app_id, room)
        .unwrap();
    let views = Rc::new(ui::application_view::InstanceViewStore::portable(cache, room).unwrap());
    let permissions = Rc::new(RefCell::new(
        ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), author),
            vec![],
        ),
    ));
    let primary = ui::record::RecordCheckout::package_primary(definition.record_kind.clone());
    let target = if records.try_open_record(&primary).is_ok() {
        LaunchTarget::Record {
            checkout: primary,
            reveal: None,
        }
    } else {
        LaunchTarget::PrimaryNew
    };
    let context = HostContext::prepare(
        definition,
        HostContextInputs {
            records: records.clone(),
            views,
            device_id: "device".into(),
            instance_id: instance,
            permissions,
            author: ui::permissions::UserId(author.into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .unwrap();
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    let checkout = component.record().clone();
    let font_root = ui::font_catalog::checkout_root();
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
        application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput},
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
    doc.put(ROOT, "tilo:schema_version", 1u64).unwrap();
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
            HostContextInputs {
                records: records.clone(),
                views: views.clone(),
                device_id: "component-device".into(),
                instance_id: instance,
                permissions: permissions.clone(),
                author: ui::permissions::UserId("author".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
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
    fn find<'a>(node: &'a ui::a11y::AccessNode, target: &str) -> Option<&'a ui::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    match node
        .semantics
        .as_ref()
        .and_then(|semantic| find(semantic, target))
    {
        Some(found) => found.value.clone(),
        None => node
            .children
            .iter()
            .find_map(|child| semantic_value(child, target)),
    }
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

/// The browser demo: a visitor and an agent mount of one room. The agent
/// finds the visitor's message among the channel's resources, reads it, and
/// replies and records its work through the published actions; its reply is
/// an agent's, by name.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one conversation between a visitor and an agent, asserted end to end"
)]
fn an_agent_mount_reads_the_visitors_message_and_replies_as_an_agent() {
    use tilo_actions::ParticipantId;
    let cache = ui::portable_storage::PortableStorage::new("chat-agent-mount", "device").unwrap();
    let (mut visitor, records, checkout) = component_mount(&cache, "Visitor", 1);
    let channel = "chat.channel:c_general";
    let invoke = |frame: &mut ui::headless::FrameHarness,
                  resource: &str,
                  action: &str,
                  args: serde_json::Value| {
        let address = ui::bound_dispatch::BoundActionAddress {
            instance_id: frame.component_instance_id().unwrap(),
            address: tilo_actions::ActionAddress::new(resource, action),
        };
        let started = frame
            .invoke_bound_action(&address, args.as_object().unwrap())
            .unwrap();
        // The published tree follows the record on the next frame.
        frame.relayout();
        started
            .completed()
            .unwrap_or_else(|| panic!("{action} completes"))
    };
    invoke(
        &mut visitor,
        channel,
        "post",
        serde_json::json!({"value": "Where does Odysseus wake up?"}),
    );

    let (mut agent, _, _) = component_mount(&cache, "agent-task:reading", 2);
    agent.join_participant(
        &ParticipantId::new("agent-task:reading").unwrap(),
        "Reading agent",
    );
    // The host names the agent for every member on its next tick.
    agent.wait_until(ui::time::Duration::ZERO, |_| true);
    let messages = |frame: &ui::headless::FrameHarness| {
        let mut ids = frame
            .bound_resources(Some(channel))
            .unwrap()
            .iter()
            .flat_map(|resource| resource.children.iter())
            .filter(|child| child.kind == "chat.message")
            .map(|child| child.id.as_str().to_owned())
            .collect::<Vec<_>>();
        ids.dedup();
        ids
    };
    let listed = messages(&agent);
    let question = listed.last().unwrap().clone();
    let read = invoke(&mut agent, &question, "read-message", serde_json::json!({}));
    let read = read.value.unwrap();
    assert_eq!(read["text"], "Where does Odysseus wake up?");
    assert_eq!(read["author"], "Visitor");
    assert_eq!(read["agent"], false);
    assert_eq!(read["mine"], false);

    invoke(
        &mut agent,
        channel,
        "post",
        serde_json::json!({"value": "On Scheria, on the shore of the Phaeacians."}),
    );
    let reply = messages(&agent).last().unwrap().clone();
    assert_ne!(reply, question);
    invoke(
        &mut agent,
        &reply,
        "set-work",
        serde_json::json!({"state": "done", "did": "Add note", "steps": [
            {"resource": "ebook.book:book:0123", "label": "Add note", "outcome": "applied"}
        ]}),
    );
    let read = invoke(&mut agent, &reply, "read-message", serde_json::json!({}))
        .value
        .unwrap();
    assert_eq!(read["author"], "agent-task:reading");
    assert_eq!(read["authorName"], "Reading agent");
    assert_eq!(read["agent"], true);
    assert_eq!(read["work"]["state"]["state"], "done");
    assert_eq!(read["work"]["trace"][0]["label"], "Add note");

    let (author, agents) = records
        .read(&checkout.id, |doc| {
            let reply = ChatDoc::list_messages(doc, "c_general").pop().unwrap();
            (reply.author, ChatDoc::agent_authors(doc))
        })
        .unwrap();
    assert_eq!(author, "agent-task:reading");
    assert_eq!(
        agents.get("agent-task:reading").map(String::as_str),
        Some("Reading agent")
    );
    // Only the message's own author records work on it.
    visitor.wait_until(ui::time::Duration::ZERO, |_| true);
    let visitor_actions = visitor.bound_resources(Some(&reply)).unwrap();
    assert!(visitor_actions[0].label.starts_with("Reading agent: "));
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: visitor.component_instance_id().unwrap(),
        address: tilo_actions::ActionAddress::new(reply.as_str(), "set-work"),
    };
    assert!(
        visitor
            .invoke_bound_action(
                &address,
                serde_json::json!({"state": "refused"}).as_object().unwrap()
            )
            .is_err(),
        "a person cannot record an agent's work"
    );
    visitor.close_components();
    agent.close_components();
}

#[test]
fn browser_media_lane_routes_shared_chat_actions_and_retires_closed_owners() {
    use ui::headless::host_media::MediaLane;
    use ui::native_media::{Command, Snapshot, State};
    let cache =
        ui::portable_storage::PortableStorage::new("browser-media-controls", "device").unwrap();
    let room = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    let (mut frame, records, checkout) = component_mount_in_room(&cache, "author", 1, room);
    let mut lane = MediaLane::default();
    let before = records.replication_snapshot(&checkout.id).unwrap().0;
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: 1,
        address: tilo_actions::ActionAddress::new("chat.call:c_lounge", "join-call"),
    };
    frame
        .invoke_bound_action(&address, &serde_json::Map::new())
        .unwrap();
    let requests = lane.take(&mut frame);
    let joining = requests
        .iter()
        .find(|request| matches!(request.command.command, Command::Join { .. }))
        .expect("the actual shared action enters the browser media lane");
    assert_eq!(joining.app, "so.tilo.chat");
    assert_eq!(joining.room, room);
    assert_eq!(joining.record, "", "Chat uses its package-primary route");
    assert_eq!(joining.instance, "1");
    let mut live = Snapshot::unavailable();
    live.state = State::Live;
    live.issue = None;
    assert!(lane.complete(&mut frame, joining.ticket, Ok(live.clone())));
    // A live call is what a continuation carries.
    let continuation =
        ui::widget::capability::capability::<dyn ui::widget::Continuable>(frame.root_widget())
            .and_then(|app| app.continuation_metadata(&frame.state().doc, &frame.state().root));
    assert!(
        continuation.is_some(),
        "the completed join is live in the app"
    );
    assert!(
        !lane.complete(&mut frame, joining.ticket, Ok(live.clone())),
        "a completion is single-use"
    );
    assert_eq!(
        records.replication_snapshot(&checkout.id).unwrap().0,
        before,
        "voice effects never write the Chat CRDT"
    );
    frame
        .invoke_bound_action(&address, &serde_json::Map::new())
        .unwrap();
    let pending = lane.take(&mut frame);
    assert!(
        !pending.is_empty(),
        "the closed-owner assertion must observe a pending command"
    );
    frame.close_components();
    let retired = lane.take(&mut frame);
    assert!(
        retired
            .iter()
            .any(|request| request.retired && matches!(request.command.command, Command::Leave)),
        "closing the component retires its browser call even after completions have drained"
    );
    for request in pending {
        assert!(
            !lane.complete(&mut frame, request.ticket, Ok(live.clone())),
            "a closed owner cannot accept a late result"
        );
    }
}

#[test]
fn message_range_navigation_preserves_unicode_and_refuses_stale_offsets() {
    use crate::effects::ChatEffect;
    let identity = LocalIdentity::demo();
    let mut doc = AutoCommit::new();
    ChatDoc::seed_demo(&mut doc, &identity.id);
    doc.commit();
    let message = ChatDoc::list_messages(&doc, "c_general")
        .first()
        .expect("seeded message")
        .clone();
    ChatDoc::edit_message(
        &mut doc,
        &message.channel,
        &message.id,
        "café notes",
        &[],
        1,
    );
    doc.commit();
    let mut app = projection(&doc, identity);
    for (start, end, unit) in [(3, 4, "Chars"), (3, 5, "Bytes")] {
        app.answer_effect(ChatEffect::RevealMessageRange {
            message: message.id.clone(),
            start,
            end,
            unit: unit.into(),
        })
        .unwrap();
        assert_eq!(
            app.revealed_range.as_ref().map(|r| (&r.0, r.2, r.3)),
            Some((&message.id, 3, 5))
        );
    }
    let before = app.revealed_range.clone();
    for (start, end, unit) in [(3, 4, "Bytes"), (0, 99, "Chars"), (4, 3, "Chars")] {
        assert!(
            app.answer_effect(ChatEffect::RevealMessageRange {
                message: message.id.clone(),
                start,
                end,
                unit: unit.into()
            })
            .is_err()
        );
        assert_eq!(app.revealed_range, before);
    }
    app.answer_effect(ChatEffect::RevealMessage {
        message: message.id,
    })
    .unwrap();
    assert!(app.revealed_range.is_none());
}

#[test]
fn browser_catalogue_and_media_polls_preserve_each_others_requests() {
    use tilo_actions::remote::RemoteActionMessage;
    use ui::bound_dispatch::BoundActionAddress;
    use ui::headless::host_media::MediaLane;
    use ui::native_media::Command;

    for catalogue_first in [true, false] {
        let cache = ui::portable_storage::PortableStorage::new(
            &format!("browser-request-routing-{catalogue_first}"),
            "device",
        )
        .unwrap();
        let (mut frame, records, checkout) =
            component_mount_in_room(&cache, "author", 1, "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71");
        let share = BoundActionAddress {
            instance_id: 1,
            address: tilo_actions::ActionAddress::new("chat.channel:c_general", "share-resource"),
        };
        frame
            .invoke_bound_action(
                &share,
                serde_json::json!({
                    "reference": {"id":"chat.message:target", "kind":"chat.message", "label":"Target"},
                    "sourceProvider":"record:so.tilo.chat:"
                })
                .as_object()
                .unwrap(),
            )
            .unwrap();
        frame.persist_flush();
        frame.relayout();
        let bytes = records.replication_snapshot(&checkout.id).unwrap().0;
        let doc = AutoCommit::load(&bytes).unwrap();
        let shared = ChatDoc::list_messages(&doc, "c_general")
            .into_iter()
            .find(|message| message.reference.is_some())
            .unwrap();
        let copy = BoundActionAddress {
            instance_id: 1,
            address: tilo_actions::ActionAddress::new(
                format!("chat.message:{}", shared.id),
                "copy-resource-link",
            ),
        };
        let join = BoundActionAddress {
            instance_id: 1,
            address: tilo_actions::ActionAddress::new("chat.call:c_lounge", "join-call"),
        };
        frame
            .invoke_bound_action(&copy, &serde_json::Map::new())
            .unwrap();
        frame
            .invoke_bound_action(&join, &serde_json::Map::new())
            .unwrap();
        let mut lane = MediaLane::default();
        let (catalogues, media) = if catalogue_first {
            let catalogues = lane.take_catalogue_requests(&mut frame);
            (catalogues, lane.take(&mut frame))
        } else {
            let media = lane.take(&mut frame);
            (lane.take_catalogue_requests(&mut frame), media)
        };
        assert!(catalogues.iter().any(|message| matches!(message,
            RemoteActionMessage::CatalogueRequest {query, ..}
                if query.provider.as_deref() == Some("record:so.tilo.chat:")
                    && query.view_resource.as_ref().is_some_and(|resource| resource.id == "chat.message:target"))));
        assert!(
            media
                .iter()
                .any(|request| matches!(request.command.command, Command::Join { .. })),
            "catalogue polling must preserve the actual shared join action"
        );
        assert!(lane.take_catalogue_requests(&mut frame).is_empty());
        frame.close_components();
    }
}

#[test]
fn catalogue_poll_cannot_deliver_queued_media_after_its_owner_closes() {
    use ui::bound_dispatch::BoundActionAddress;
    use ui::headless::host_media::MediaLane;
    use ui::native_media::Command;
    let cache =
        ui::portable_storage::PortableStorage::new("catalogue-media-retirement", "device").unwrap();
    let (mut frame, _, _) =
        component_mount_in_room(&cache, "author", 1, "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71");
    frame
        .invoke_bound_action(
            &BoundActionAddress {
                instance_id: 1,
                address: tilo_actions::ActionAddress::new("chat.call:c_lounge", "join-call"),
            },
            &serde_json::Map::new(),
        )
        .unwrap();
    let mut lane = MediaLane::default();
    assert!(lane.take_catalogue_requests(&mut frame).is_empty());
    frame.close_components();
    let media = lane.take(&mut frame);
    assert!(
        media
            .iter()
            .any(|request| request.retired && matches!(request.command.command, Command::Leave))
    );
    assert!(
        media.iter().all(|request| request.retired),
        "queued commands cannot outlive their bound component"
    );
}
