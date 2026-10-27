//! Architecture checks: the window's public surface stays answerable and its
//! dependencies keep pointing one way.

use super::*;

#[test]
fn public_inventory_cannot_answer_from_the_initial_render_cache() {
    let mut widget = BrowserWidget::new_with_mode(BrowsingMode::Normal);
    widget.accept_service_catalog(Vec::new());
    let resource = widget.published();
    let inventory = resource
        .children
        .iter()
        .find(|row| row.id.0 == "browser.service-sessions")
        .unwrap();
    let action = inventory
        .actions
        .iter()
        .find(|action| action.id == "list-service-sessions")
        .unwrap();
    let encoded = action.build_json(&serde_json::Map::new()).unwrap();
    assert!(matches!(
        effect_operation(&encoded.payload),
        BrowserOperation::ListServiceSessions
    ));
    assert!(
        widget
            .admit(
                ui::tilo_actions::InvocationId::new(),
                BrowserOperation::ListServiceSessions
            )
            .is_err()
    );
}

#[test]
fn pending_revocation_and_stale_selection_remain_publicly_readable() {
    let mut widget = BrowserWidget::new_with_mode(BrowsingMode::Normal);
    for status in [
        "forgetting",
        "forget-incomplete",
        "forgotten",
        "needs-reconnect",
    ] {
        widget.service_observation = Some(
            serde_json::json!({"session_id": "selected", "status": status,
                "custody_revoked": status == "forgotten", "engine_shutdown_confirmed": status == "forgotten",
                "custody_backend_needed": status != "forgotten", "local_only": false,
                "requires_reconnect": status == "needs-reconnect"}),
        );
        let resource = widget.published();
        let row = resource
            .children
            .iter()
            .find(|row| row.id.0 == "browser.service-status")
            .unwrap();
        let action = row
            .actions
            .iter()
            .find(|action| action.id == "read-service-session")
            .unwrap();
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        let ui::framework::Effected::Done(output) = widget
            .admit(
                ui::tilo_actions::InvocationId::new(),
                effect_operation(&encoded.payload),
            )
            .unwrap()
        else {
            panic!("observation must be immediately readable")
        };
        let value = output.value.unwrap();
        assert_eq!(value["session_id"], "selected");
        assert_eq!(value["status"], status);
        assert_eq!(value["local_only"], false);
    }
}

use ui::framework::HostDelivery as _;
use ui::tilo_actions::OperationKind;
use ui::widget::MenuButton;

/// What the window does for the effect an encoded invocation names.
fn effect_operation(payload: &[u8]) -> BrowserOperation {
    use ui::tilo_resource::Effect as _;
    crate::effects::BrowserEffect::from_payload(payload)
        .expect("a browser effect")
        .operation()
}
use ui::webview::WebViewEngine;

pub(super) fn compose(widget: &mut BrowserWidget) {
    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    widget.sync(&doc, &root);
    let mut tree = TaffyTree::<()>::new();
    let mut nodes = std::collections::HashMap::new();
    widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
}

pub(super) fn activate_overflow_command(widget: &mut BrowserWidget, label: &str) {
    fn menu(widget: &mut dyn Widget) -> Option<&mut MenuButton> {
        if widget.test_id() == Some(toolbar::OVERFLOW_ID) {
            return widget.as_any_mut()?.downcast_mut::<MenuButton>();
        }
        widget.children_mut().into_iter().find_map(menu)
    }
    fn target(node: ui::a11y::AccessNode, label: &str) -> Option<String> {
        if node.name.as_deref() == Some(label) {
            return node.action_target;
        }
        node.children
            .into_iter()
            .find_map(|child| target(child, label))
    }
    let menu = menu(widget.composed.element_mut().expect("actual composition"))
        .expect("retained overflow menu");
    menu.open(true);
    let target = target(menu.accessibility().expect("exported menu"), label)
        .expect("the command has its real semantic target");
    assert!(menu.perform_semantic_action(
        &target,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT
    ));
}

fn assert_no_document_control_paths(widget: &BrowserWidget) {
    assert!(
        ui::widget::controls::control_action_bindings(widget)
            .iter()
            .all(|binding| {
                !matches!(
                    binding.kind,
                    ui::widget::ControlBindingKind::CollaborativeDocument(_)
                )
            })
    );
}

pub(super) fn assert_action_binding(
    widget: &BrowserWidget,
    control_id: &str,
    resource: &str,
    action: &str,
) {
    assert!(
        ui::widget::controls::control_action_bindings(widget)
            .iter()
            .any(|binding| {
                binding.control_id == control_id
                    && matches!(
                        &binding.kind,
                        ui::widget::ControlBindingKind::Action(address)
                            if address.resource == resource && address.action == action
                    )
            }),
        "{control_id} must retain its canonical {resource}/{action} address"
    );
}

fn dispatch_back_event(widget: &mut BrowserWidget, event: &WindowEvent) -> bool {
    ui::testing::dispatch(widget, event)
}

fn mouse(button: MouseButton, state: ElementState) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, button)
}

pub(super) fn key(named: WNamedKey) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        WKey::Named(named),
        None,
        ElementState::Pressed,
    ))
}

#[test]
fn delayed_address_paste_survives_layout() {
    use std::sync::{Arc, Mutex, mpsc};
    use ui::clipboard::{ClipboardBackend, ClipboardTarget};

    #[derive(Default)]
    struct Delayed {
        reply: Mutex<Option<mpsc::Sender<Option<String>>>>,
    }
    impl ClipboardBackend for Delayed {
        fn set_text(&self, _: ClipboardTarget, _: &str) -> bool {
            false
        }
        fn text(&self, _: ClipboardTarget) -> Option<String> {
            None
        }
        fn request_text(&self, _: ClipboardTarget) -> mpsc::Receiver<Option<String>> {
            let (reply, result) = mpsc::channel();
            *self.reply.lock().unwrap() = Some(reply);
            result
        }
        fn name(&self) -> &'static str {
            "delayed-address-paste"
        }
    }
    let clipboard = Arc::new(Delayed::default());
    let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = ui::host::enter(&services);
    let mut widget = BrowserWidget::with_engine(TestEngine::new(), true);
    let (tree, node) = super::tests::layout_keeping_tree(&mut widget, 900.0, 700.0, &Theme::dark());
    assert!(widget.focus_address_input());
    let mut event = key(WNamedKey::Enter);
    if let WindowEvent::KeyboardInput { event, .. } = &mut event {
        event.logical_key = WKey::Character("v".into());
    }
    let mut document = AutoCommit::new();
    let handled = ui::testing::Dispatch::new()
        .doc(&mut document)
        .layout(&tree, node)
        .modifiers(winit::keyboard::ModifiersState::CONTROL)
        .send(&mut widget, &event)
        .handled;
    assert!(handled);
    let reply = clipboard
        .reply
        .lock()
        .unwrap()
        .take()
        .expect("native paste requested");
    super::tests::layout_at(&mut widget, 900.0, 700.0);
    reply
        .send(Some("https://example.test/pasted".into()))
        .expect("layout retains the pending paste");
    widget.sync(&document, &automerge::ROOT);
    assert_eq!(
        widget.address.with(Clone::clone),
        "https://example.test/pasted"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn find_and_daily_shortcuts_route_focus_keys_and_dpi() {
    {
        let mut widget = BrowserWidget::new_headless();
        compose(&mut widget);
        assert!(widget.focus_control("browser.surface"));
        compose(&mut widget);
        assert!(widget.control_focused("browser.surface"));
        widget
            .daily
            .act(crate::daily_controls::Action::Open, &widget.engine);
        compose(&mut widget);
        assert!(widget.control_focused(crate::daily_controls::FIND_INPUT));
        widget
            .daily
            .act(crate::daily_controls::Action::Close, &widget.engine);
        compose(&mut widget);
        assert!(widget.control_focused("browser.surface"));
    }

    {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        // The window's density is what it draws at; input never sets it.
        super::tests::draw_at(&mut widget, 2.0);
        super::tests::layout_at(&mut widget, 800.0, 600.0);
        widget
            .daily
            .act(crate::daily_controls::Action::Open, &widget.engine);
        super::tests::layout_at(&mut widget, 800.0, 600.0);
        widget
            .daily
            .act(crate::daily_controls::Action::Close, &widget.engine);
        super::tests::layout_at(&mut widget, 800.0, 600.0);
        let sizes: Vec<_> = engine
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::Resize {
                    width,
                    height,
                    scale_factor,
                } => Some((width, height, scale_factor)),
                _ => None,
            })
            .collect();
        assert!(sizes.len() >= 3, "{sizes:?}");
        assert!(
            sizes
                .iter()
                .all(|&(width, _, scale)| width > 1000 && (scale - 2.0).abs() < f32::EPSILON),
            "{sizes:?}"
        );
        assert!(
            sizes[1].1 < sizes[0].1,
            "Find must shrink the page viewport"
        );
        assert_eq!(sizes.first(), sizes.last());
    }

    {
        let capture = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(capture.clone(), true);
        compose(&mut widget);
        widget
            .daily
            .act(crate::daily_controls::Action::Open, &widget.engine);
        let mut tree = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let node = widget.build_layout(&mut tree, &Theme::dark(), &mut nodes);
        // The host restores its previous focus after rebuilding. The next
        // event must reapply the pending find focus before dispatching text.
        widget.focus_control("browser.surface");
        let mut doc = AutoCommit::new();
        let mut target = None;
        for letter in ["N", "e", "e", "d", "l", "e"] {
            let mut event = key(WNamedKey::Enter);
            if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                event.logical_key = WKey::Character(letter.into());
                event.text = Some(letter.into());
            }
            target = ui::testing::Dispatch::new()
                .doc(&mut doc)
                .layout(&tree, node)
                .focus_target(target)
                .send(&mut widget, &event)
                .focus_target;
            widget.daily.sync(&widget.engine);
        }
        widget.daily.sync(&widget.engine);
        assert!(widget.control_focused(crate::daily_controls::FIND_INPUT));
        assert!(
            capture.commands().iter().any(|command| matches!(command,
                WebViewCommand::Find { text, .. } if text == "Needle")),
            "{:?}",
            capture.commands()
        );
    }

    {
        let mut widget = BrowserWidget::new_headless();
        for letter in ["f", "+", "-", "0", "d"] {
            let mut event = key(WNamedKey::Enter);
            if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                event.logical_key = WKey::Character(letter.into());
            }
            assert!(!widget.handle_browser_shortcut(&event, winit::event::Modifiers::default()));
            assert!(
                widget.handle_browser_shortcut(
                    &event,
                    (if cfg!(target_os = "macos") {
                        winit::keyboard::ModifiersState::SUPER
                    } else {
                        winit::keyboard::ModifiersState::CONTROL
                    })
                    .into()
                ),
                "{letter}"
            );
        }
    }
}

#[test]
fn owned_navigation_rejects_unrelated_completion_and_preserves_redirect_output() {
    use ui::tilo_resource::Effect as _;

    struct Events(
        std::sync::Mutex<Vec<WebViewEvent>>,
        std::sync::Arc<TestEngine>,
    );
    impl WebViewEngine for Events {
        fn send(&self, command: WebViewCommand) {
            self.1.send(command);
        }
        fn navigate_owned(
            &self,
            request_id: u32,
            command: NavigationCommand,
        ) -> Result<(), String> {
            self.1.navigate_owned(request_id, command)
        }
        fn frames(&self) -> ui::webview::FrameSlot {
            self.1.frames()
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }
    let engine = std::sync::Arc::new(Events(std::sync::Mutex::new(Vec::new()), TestEngine::new()));
    let mut widget = BrowserWidget::with_engine(engine.clone(), false);
    let invocation = ui::tilo_actions::InvocationId::new();
    assert!(matches!(
        widget
            .admit(
                invocation,
                BrowserOperation::Navigate {
                    url: "https://request.test/".into()
                }
            )
            .unwrap(),
        ui::framework::Effected::Pending
    ));
    engine.0.lock().unwrap().extend([
        WebViewEvent::UriChanged("about:blank".into()),
        WebViewEvent::LoadFinished,
        WebViewEvent::LoadFailed {
            uri: "about:blank".into(),
            error: "stale startup".into(),
        },
        WebViewEvent::NavigationOutcome {
            request_id: 999,
            uri: "about:blank".into(),
            error: None,
        },
    ]);
    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    widget.sync(&doc, &root);
    assert!(widget.poll_invocation(invocation).is_none());
    assert!(
        !engine
            .1
            .commands()
            .iter()
            .any(|command| matches!(command, WebViewCommand::Navigate(_))),
        "late default initialization must not supersede owned navigation"
    );
    let request_id = widget.resource_navigation_request.unwrap();
    engine
        .0
        .lock()
        .unwrap()
        .push(WebViewEvent::NavigationOutcome {
            request_id,
            uri: "https://redirect.test/".into(),
            error: None,
        });
    widget.apply_engine_events();
    let Some(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) =
        widget.poll_invocation(invocation)
    else {
        panic!("matching owned completion must succeed");
    };
    assert_eq!(
        output.value,
        Some(serde_json::json!("https://redirect.test/"))
    );
    let contract = crate::effects::BrowserEffect::Navigate {
        url: "https://request.test/".into(),
    }
    .bound()
    .contract
    .clone();
    assert!(matches!(
        contract.execution,
        ui::tilo_actions::ExecutionMode::Background { cancellable: false }
    ));
    assert!(contract.validate_declaration().is_empty());
    contract.result.validate(&output).unwrap();
    let failed = ui::tilo_actions::InvocationId::new();
    widget.admit(failed, BrowserOperation::Reload).unwrap();
    let request_id = widget.resource_navigation_request.unwrap();
    engine
        .0
        .lock()
        .unwrap()
        .push(WebViewEvent::NavigationOutcome {
            request_id,
            uri: "https://redirect.test/".into(),
            error: Some("ERR_CONNECTION_REFUSED".into()),
        });
    widget.apply_engine_events();
    let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) =
        widget.poll_invocation(failed)
    else {
        panic!("matching owned failure must fail");
    };
    assert_eq!(error.code, "browser.navigation_failed");
    assert_eq!(error.message, "ERR_CONNECTION_REFUSED");
}

/// Loading state: stop contract, animation scheduling, and when loading starts and ends.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn loading_state_starts_stops_and_animates_only_while_live() {
    {
        struct QueuedEngine {
            events: std::sync::Mutex<Vec<WebViewEvent>>,
            capture: std::sync::Arc<TestEngine>,
        }
        impl WebViewEngine for QueuedEngine {
            fn navigate_owned(
                &self,
                request_id: u32,
                command: NavigationCommand,
            ) -> Result<(), String> {
                self.capture.navigate_owned(request_id, command)
            }
            fn send(&self, command: WebViewCommand) {
                self.capture.send(command);
            }
            fn frames(&self) -> ui::webview::FrameSlot {
                self.capture.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                std::mem::take(&mut *self.events.lock().unwrap())
            }
        }
        let capture = TestEngine::new();
        let engine = std::sync::Arc::new(QueuedEngine {
            events: std::sync::Mutex::new(Vec::new()),
            capture: capture.clone(),
        });
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let invocation = ui::tilo_actions::InvocationId::new();
        assert!(matches!(
            widget
                .admit(
                    invocation,
                    BrowserOperation::Navigate {
                        url: "https://stop.test/".into()
                    }
                )
                .unwrap(),
            ui::framework::Effected::Pending
        ));
        widget.set_loading_progress(0.2);
        let resources = [widget.published()];
        let stop = resources[0]
            .children
            .iter()
            .find(|r| r.id.0 == "browser.page")
            .unwrap()
            .actions
            .iter()
            .find(|a| a.id == "stop")
            .unwrap();
        let encoded = stop.build_json(&serde_json::Map::new()).unwrap();
        let result = widget
            .admit(
                ui::tilo_actions::InvocationId::new(),
                effect_operation(&encoded.payload),
            )
            .map(Some)
            .unwrap();
        let Some(ui::framework::Effected::Done(output)) = result else {
            panic!("Stop must complete immediately");
        };
        assert!(stop.contract.result.validate(&output).is_ok());
        assert!(matches!(
            widget.resource_navigation_result,
            Some(ui::tilo_actions::InvocationUpdate::Failed { .. })
        ));
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::LoadProgress(0.4));
        widget.apply_engine_events();
        assert!((widget.progress - 1.0).abs() < f64::EPSILON);
        assert!(capture.commands().contains(&WebViewCommand::StopLoading));
        widget
            .perform_browser_operation(BrowserOperation::Reload)
            .unwrap();
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::LoadProgress(0.6));
        widget.apply_engine_events();
        assert!((widget.progress - 0.6).abs() < f64::EPSILON);

        // A failed request can leave Chromium on its previous document.
        // Late progress and that document's completion must not conceal
        // the failed address or restart the spinner; retry owns the URL.
        let failed_uri = "http://127.0.0.1:9/";
        engine.events.lock().unwrap().extend([
            WebViewEvent::LoadFailed {
                uri: failed_uri.into(),
                error: "ERR_UNSAFE_PORT".into(),
            },
            WebViewEvent::LoadProgress(0.3),
            WebViewEvent::LoadFinishedAt {
                uri: "https://stop.test/".into(),
            },
            WebViewEvent::LoadFinished,
        ]);
        widget.apply_engine_events();
        assert_eq!(widget.address.get(), failed_uri);
        assert_eq!(widget.current_uri, failed_uri);
        // The person reads what happened; the engine's error is logged.
        assert_eq!(
            widget.load_error.as_deref(),
            Some(load_failure::page_failed_message("ERR_UNSAFE_PORT").as_str())
        );
        assert_eq!(widget.title, load_failure::page_failed_title());
        assert!((widget.progress - 1.0).abs() < f64::EPSILON);
        widget
            .perform_browser_operation(BrowserOperation::Reload)
            .unwrap();
        assert!(
            capture
                .commands()
                .contains(&WebViewCommand::Navigate(failed_uri.into()))
        );
        assert!(widget.load_error.is_none());
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::LoadProgress(0.4));
        widget.apply_engine_events();
        assert!((widget.progress - 0.4).abs() < f64::EPSILON);
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::LoadFailed {
                uri: "file:///outside-workspace.html".into(),
                error: "blocked".into(),
            });
        widget.apply_engine_events();
        widget
            .perform_browser_operation(BrowserOperation::Reload)
            .unwrap();
        assert_eq!(capture.commands().last(), Some(&WebViewCommand::Reload));
    }

    {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        widget
            .perform_browser_operation(BrowserOperation::Navigate {
                url: "https://a.test".into(),
            })
            .unwrap();
        assert!(widget.needs_animation_frame(), "the loading line sweeps");
        widget.set_motion_reduced(true);
        assert!(
            !widget.needs_animation_frame(),
            "under reduced motion the loading line holds still"
        );
        widget
            .perform_browser_operation(BrowserOperation::Stop)
            .unwrap();
        assert!((widget.progress - 1.0).abs() < f64::EPSILON);
        assert!(engine.commands().contains(&WebViewCommand::StopLoading));
        assert!(widget.take_structure_dirty());
    }

    {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine, true);
        widget
            .perform_browser_operation(BrowserOperation::Navigate {
                url: "https://a.test".into(),
            })
            .unwrap();
        assert!(
            widget.progress < 1.0,
            "navigation must paint loading before callbacks"
        );
        assert!(
            widget.needs_animation_frame(),
            "loading indicator animates immediately"
        );
        widget.set_loading_progress(0.4);
        assert!(
            !widget.needs_animation_frame(),
            "a load with reported progress repaints only when progress moves"
        );
        widget.apply_engine_events();
        assert!((widget.progress - 1.0).abs() < f64::EPSILON);
        assert!(widget.loading_face().is_none());
        assert!(!widget.needs_animation_frame());
    }

    {
        let engine = ui::webview::chromium::UnavailableEngine::new("offline".into());
        let mut widget = BrowserWidget::with_engine(engine, false);
        assert!(
            widget.progress < 1.0,
            "new tab is visibly loading before bootstrap"
        );
        widget
            .engine
            .send(WebViewCommand::Navigate("https://a.test".into()));
        widget.apply_engine_events();
        assert!((widget.progress - 1.0).abs() < f64::EPSILON);
        assert!(
            !widget
                .build_composition(&Theme::dark())
                .needs_animation_frame()
        );
    }
}

/// Every hosted affordance is a canonical action; presentation stays peer-local.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn hosted_browser_affordances_are_canonical_peer_local_actions() {
    {
        let capture = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(capture.clone(), true);
        widget
            .capabilities
            .apply(WebViewEvent::NativeDownloadRequested {
                id: 1,
                url: "https://test.example/file".into(),
                name: "file.txt".into(),
            });
        compose(&mut widget);
        let result = widget
            .start_encoded(
                OperationKind::Effect,
                "tilo.ui.control.invoke",
                &serde_json::to_vec("browser.decision.false.1.true").unwrap(),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            )
            .unwrap();
        assert!(result.is_some());
        assert!(
            capture
                .commands()
                .contains(&WebViewCommand::DownloadDecision { id: 1, allow: true })
        );
    }

    {
        let mut left = BrowserWidget::new_headless();
        let right = BrowserWidget::new_headless();
        let mut doc = AutoCommit::new();
        let before = doc.save();
        left.perform_browser_operation(BrowserOperation::ShowLibrary {
            section: LibrarySection::Bookmarks,
        })
        .unwrap();
        assert!(left.show_library);
        assert!(!right.show_library);
        assert_eq!(
            doc.save(),
            before,
            "showing a local pane is not a shared mutation"
        );
    }

    {
        let mut page = BrowserWidget::new_headless();
        compose(&mut page);
        assert_no_document_control_paths(&page);
        for (control, resource, action) in [
            ("browser.back", "browser.page", "back"),
            ("browser.forward", "browser.page", "forward"),
            ("browser.reload", "browser.page", "reload"),
            ("browser.library", "browser.history", "show-history"),
        ] {
            assert_action_binding(&page, control, resource, action);
        }
        page.debug_show_library("history");
        compose(&mut page);
        assert_action_binding(
            &page,
            "browser.library.history-pause",
            "browser.session",
            "toggle-pause",
        );
        // The overflow menu's commands queue the same canonical actions.
        for (label, resource, action) in [
            ("Pause history", "browser.session", "toggle-pause"),
            ("Picture in Picture", "browser.page", "picture-in-picture"),
            ("Settings", "browser.settings", "show-settings"),
        ] {
            let mut page = BrowserWidget::new_headless();
            compose(&mut page);
            activate_overflow_command(&mut page, label);
            assert!(page.run_pending_commands());
            assert!(
                page.pending_controls.iter().any(|invocation| {
                    invocation.address.resource == resource && invocation.address.action == action
                }),
                "{resource}/{action} reaches the action host from the overflow menu"
            );
        }

        let mut library = BrowserWidget::new_headless();
        library.debug_show_library("history");
        library
            .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                query: "history".into(),
            })
            .unwrap();
        compose(&mut library);
        assert_no_document_control_paths(&library);
        assert_action_binding(
            &library,
            "browser.library.clear-filter",
            "browser.library",
            "clear-library-filter",
        );

        let mut settings = BrowserWidget::new_headless();
        settings.show_settings = true;
        compose(&mut settings);
        assert_no_document_control_paths(&settings);
        for (control, resource, action) in [
            (
                "settings.close",
                crate::resource::Browser::KIND,
                "show-page",
            ),
            ("settings.clear_history", "browser.history", "clear-history"),
            (
                "settings.clear_cookies",
                "browser.settings",
                "request-site-data-clear",
            ),
        ] {
            assert_action_binding(&settings, control, resource, action);
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn session_history_menus_request_real_entries_and_ignore_stale_replies() {
    {
        let engine = TestEngine::new();
        for url in ["https://a.test", "https://b.test", "https://c.test"] {
            engine.send(WebViewCommand::Navigate(url.into()));
        }
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        widget.cursor = widget.find_rect("browser.back").unwrap().center();
        assert!(dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Pressed)
        ));
        assert_eq!(engine.current_uri().as_deref(), Some("https://c.test"));
        widget.tick_back_hold(ui::time::now() + std::time::Duration::from_millis(451));
        widget.apply_engine_events();
        assert!(widget.back_menu.is_open());
        assert_eq!(
            widget
                .back_entries
                .iter()
                .map(|e| e.url.as_str())
                .collect::<Vec<_>>(),
            ["https://b.test", "https://a.test"]
        );
        assert!(dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Released)
        ));
        assert!(widget.pending_controls.is_empty());
        assert_eq!(engine.current_uri().as_deref(), Some("https://c.test"));
        let accessible = widget.back_menu.accessibility().unwrap();
        assert_eq!(accessible.children.len(), 2);
        assert!(dispatch_back_event(&mut widget, &key(WNamedKey::ArrowDown)));
        assert!(dispatch_back_event(&mut widget, &key(WNamedKey::ArrowDown)));
        assert!(dispatch_back_event(&mut widget, &key(WNamedKey::Enter)));
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        engine.send(WebViewCommand::GoForward);
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
    }

    {
        let engine = TestEngine::new();
        for url in ["https://a.test", "https://b.test"] {
            engine.send(WebViewCommand::Navigate(url.into()));
        }
        engine.send(WebViewCommand::GoBack);
        let mut widget = BrowserWidget::with_engine(engine, true);
        widget.debug_set_viewport_width(360);
        super::tests::layout_at(&mut widget, 360.0, 700.0);
        widget.apply_engine_events();
        let forward = widget
            .find_rect("browser.forward")
            .expect("compact Forward must remain visible");
        assert!(forward.width() >= 30.0 && forward.x0 >= 0.0 && forward.x1 <= 360.0);
        widget.cursor = forward.center();
        assert!(dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Right, ElementState::Pressed)
        ));
        widget.apply_engine_events();
        assert!(widget.back_menu.is_open());
        assert_eq!(
            widget
                .back_entries
                .iter()
                .map(|entry| entry.url.as_str())
                .collect::<Vec<_>>(),
            ["https://b.test"]
        );
    }

    {
        let engine = TestEngine::new();
        for url in ["https://a.test", "https://b.test", "https://c.test"] {
            engine.send(WebViewCommand::Navigate(url.into()));
        }
        engine.send(WebViewCommand::GoBack);
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        widget.apply_engine_events();
        widget.cursor = widget.find_rect("browser.back").unwrap().center();
        assert!(dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Right, ElementState::Pressed)
        ));
        widget.apply_engine_events();
        assert_eq!(
            widget
                .back_entries
                .iter()
                .map(|entry| (entry.id, entry.url.as_str()))
                .collect::<Vec<_>>(),
            [(1, "https://a.test")]
        );
        assert!(dispatch_back_event(&mut widget, &key(WNamedKey::Escape)));
        widget.cursor = widget.find_rect("browser.forward").unwrap().center();

        assert!(dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Pressed)
        ));
        widget.tick_back_hold(ui::time::now() + std::time::Duration::from_millis(451));
        widget.apply_engine_events();

        assert!(widget.back_menu.is_open());
        assert_eq!(
            widget
                .back_entries
                .iter()
                .map(|entry| (entry.id, entry.url.as_str()))
                .collect::<Vec<_>>(),
            [(3, "https://c.test")]
        );
        let request_count = engine
            .commands()
            .iter()
            .filter(|command| matches!(command, WebViewCommand::RequestNavigationHistory { .. }))
            .count();
        widget.tick_back_hold(ui::time::now() + std::time::Duration::from_millis(900));
        assert_eq!(
            engine
                .commands()
                .iter()
                .filter(|command| matches!(
                    command,
                    WebViewCommand::RequestNavigationHistory { .. }
                ))
                .count(),
            request_count
        );
        assert!(dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Released)
        ));
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
    }

    {
        struct DelayedEngine {
            events: std::sync::Mutex<Vec<WebViewEvent>>,
            capture: std::sync::Arc<TestEngine>,
        }
        impl WebViewEngine for DelayedEngine {
            fn send(&self, command: WebViewCommand) {
                self.capture.send(command);
            }
            fn frames(&self) -> ui::webview::FrameSlot {
                self.capture.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                std::mem::take(&mut *self.events.lock().unwrap())
            }
        }

        let capture = TestEngine::new();
        let engine = std::sync::Arc::new(DelayedEngine {
            events: std::sync::Mutex::new(Vec::new()),
            capture,
        });
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        widget.can_go_back = true;
        widget.can_go_forward = true;
        widget.current_uri = "https://b.test".into();

        widget.request_navigation_history(ui::webview::NavigationDirection::Back);
        widget.request_navigation_history(ui::webview::NavigationDirection::Forward);
        let stale_direction_request = widget.history_request;
        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::NavigationHistory {
                request_id: stale_direction_request,
                direction: ui::webview::NavigationDirection::Back,
                entries: vec![ui::webview::NavigationEntry {
                    id: 11,
                    url: "https://a.test".into(),
                    title: "A".into(),
                }],
            });
        widget.apply_engine_events();
        assert!(!widget.back_menu.is_open());

        let stale_page_request = widget.history_request;
        engine.events.lock().unwrap().extend([
            WebViewEvent::UriChanged("https://new.test".into()),
            WebViewEvent::NavigationHistory {
                request_id: stale_page_request,
                direction: ui::webview::NavigationDirection::Forward,
                entries: vec![ui::webview::NavigationEntry {
                    id: 33,
                    url: "https://c.test".into(),
                    title: "C".into(),
                }],
            },
        ]);
        widget.apply_engine_events();
        assert!(!widget.back_menu.is_open());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn media_fullscreen_and_renderer_events_drive_tab_state() {
    {
        struct EventEngine {
            events: std::sync::Mutex<Vec<WebViewEvent>>,
            capture: std::sync::Arc<TestEngine>,
        }
        impl WebViewEngine for EventEngine {
            fn send(&self, command: WebViewCommand) {
                self.capture.send(command);
            }
            fn frames(&self) -> ui::webview::FrameSlot {
                self.capture.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                std::mem::take(&mut *self.events.lock().unwrap())
            }
        }
        let engine = std::sync::Arc::new(EventEngine {
            events: std::sync::Mutex::new(vec![
                WebViewEvent::UriChanged("https://media.test/".into()),
                WebViewEvent::MediaPlayingChanged {
                    page_url: "https://media.test/".into(),
                    document_generation: 8,
                    playing: true,
                },
                WebViewEvent::MediaPlayingChanged {
                    page_url: "https://media.test/".into(),
                    document_generation: 7,
                    playing: false,
                },
            ]),
            capture: TestEngine::new(),
        });
        let mut widget = BrowserWidget::with_engine(engine, true);
        widget.apply_engine_events();
        assert!(widget.media_playing);
        assert_eq!(widget.media_document_generation, Some(8));
    }

    {
        struct FullscreenEngine {
            events: std::sync::Mutex<Vec<WebViewEvent>>,
            capture: std::sync::Arc<TestEngine>,
        }
        impl WebViewEngine for FullscreenEngine {
            fn send(&self, command: WebViewCommand) {
                self.capture.send(command);
            }
            fn frames(&self) -> ui::webview::FrameSlot {
                self.capture.frames()
            }
            fn drain_events(&self) -> Vec<WebViewEvent> {
                std::mem::take(&mut *self.events.lock().unwrap())
            }
        }
        let capture = TestEngine::new();
        let engine = std::sync::Arc::new(FullscreenEngine {
            events: std::sync::Mutex::new(vec![WebViewEvent::FullscreenChanged(true)]),
            capture: capture,
        });
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);

        widget.apply_engine_events();
        assert!(widget.content_fullscreen());
        assert!(widget.take_structure_dirty());

        engine
            .events
            .lock()
            .unwrap()
            .push(WebViewEvent::RendererTerminated);
        widget.apply_engine_events();
        assert!(!widget.content_fullscreen());
        assert!(widget.take_structure_dirty());

        engine.events.lock().unwrap().extend([
            WebViewEvent::FullscreenChanged(true),
            WebViewEvent::FullscreenChanged(false),
        ]);
        widget.apply_engine_events();
        assert!(!widget.content_fullscreen());
    }

    {
        let engine = TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        widget.content_fullscreen = true;

        assert!(widget.exit_content_fullscreen());
        assert!(!widget.exit_content_fullscreen());
        assert!(!widget.content_fullscreen());
        assert_eq!(
            engine
                .commands()
                .iter()
                .filter(|command| matches!(command, WebViewCommand::ExitFullscreen))
                .count(),
            1
        );
    }
}

/// Back/forward click, cancel, keyboard and accessible activation paths.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn back_forward_controls_open_session_entries_without_accidental_navigation() {
    {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        let mut widget = BrowserWidget::with_engine(engine, true);
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        widget.cursor = widget.find_rect("browser.back").unwrap().center();
        dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Pressed),
        );
        dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Released),
        );
        assert_eq!(widget.pending_controls.len(), 1);
        widget.pending_controls.clear();
        dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Pressed),
        );
        dispatch_back_event(&mut widget, &WindowEvent::Focused(false));
        dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Released),
        );
        assert!(widget.pending_controls.is_empty());
        assert!(!widget.navigation_hold.waiting());
        dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Right, ElementState::Pressed),
        );
        widget.apply_engine_events();
        assert!(widget.back_menu.is_open());
        assert!(dispatch_back_event(&mut widget, &key(WNamedKey::Escape)));
        assert!(!widget.back_menu.is_open());
        dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Right, ElementState::Pressed),
        );
        widget.apply_engine_events();
        assert!(widget.back_menu.is_open());
        widget.cursor = Point::new(850.0, 650.0);
        dispatch_back_event(
            &mut widget,
            &ui::synthetic_input::cursor_moved(850.0, 650.0),
        );
        assert!(dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Pressed)
        ));
        assert!(!widget.back_menu.is_open());
    }

    {
        fn focus_back(widget: &mut dyn Widget) {
            widget.set_focus(widget.test_id() == Some("browser.back"));
            for child in widget.children_mut() {
                focus_back(child);
            }
        }
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        focus_back(&mut widget);
        assert!(dispatch_back_event(
            &mut widget,
            &key(WNamedKey::ContextMenu)
        ));
        widget.apply_engine_events();
        assert!(widget.back_menu.is_open());
        let node = widget.accessibility().unwrap();
        let target = node.children[0].children[0].action_target.clone().unwrap();
        assert!(widget.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT
        ));
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        assert!(!widget.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT
        ));
    }

    {
        fn focus_forward(widget: &mut dyn Widget) {
            widget.set_focus(widget.test_id() == Some("browser.forward"));
            for child in widget.children_mut() {
                focus_forward(child);
            }
        }
        let engine = TestEngine::new();
        for url in ["https://a.test", "https://b.test", "https://c.test"] {
            engine.send(WebViewCommand::Navigate(url.into()));
        }
        engine.send(WebViewCommand::GoBack);
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        widget.apply_engine_events();
        focus_forward(&mut widget);

        assert!(dispatch_back_event(
            &mut widget,
            &key(WNamedKey::ContextMenu)
        ));
        widget.apply_engine_events();
        let node = widget.accessibility().unwrap();
        let target = node.children[0].children[0].action_target.clone().unwrap();
        assert!(target.contains(":forward:"));
        assert!(widget.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT
        ));
        assert_eq!(engine.current_uri().as_deref(), Some("https://c.test"));
    }

    {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        let mut widget = BrowserWidget::with_engine(engine, true);
        super::tests::layout_at(&mut widget, 900.0, 700.0);
        widget.cursor = widget.find_rect("browser.back").unwrap().center();
        dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Pressed),
        );
        dispatch_back_event(&mut widget, &WindowEvent::Focused(false));
        widget.cursor = Point::new(800.0, 600.0);
        assert!(!dispatch_back_event(
            &mut widget,
            &mouse(MouseButton::Left, ElementState::Released)
        ));
    }
}

#[test]
fn library_controls_are_bound_canonical_actions_with_stable_addresses() {
    {
        let mut widget = BrowserWidget::new_headless();
        widget
            .history
            .record_at("https://first.example", "First", 1);
        widget
            .history
            .record_at("https://second.example", "Second", 2);
        widget.show_library = true;
        compose(&mut widget);
        let bindings = ui::widget::controls::control_action_bindings(&widget);
        for url in ["https://first.example", "https://second.example"] {
            let resource = history_resource_id(url);
            assert!(bindings.iter().any(|binding| {
                binding.control_id == format!("{resource}.open")
                    && matches!(
                        &binding.kind,
                        ui::widget::ControlBindingKind::Action(address)
                            if address.resource == resource && address.action == "open"
                    )
            }));
        }
    }

    {
        let mut widget = BrowserWidget::new_headless();
        widget.debug_show_library("bookmarks");
        widget
            .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                query: "rust".into(),
            })
            .unwrap();
        compose(&mut widget);
        assert_action_binding(
            &widget,
            "browser.library.clear-filter",
            "browser.library",
            "clear-library-filter",
        );

        widget
            .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                query: "rust learn".into(),
            })
            .unwrap();
        assert_eq!(widget.library_filter.get(), "rust learn");
        assert!(widget.show_library);
        assert!(widget.take_structure_dirty());
        assert!(
            widget
                .perform_browser_operation(BrowserOperation::SetLibraryFilter {
                    query: "x".repeat(MAX_LIBRARY_FILTER_BYTES + 1),
                })
                .is_err()
        );
    }
}
