use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceTree;
use ui::TextRole;
use ui::widget::Container;

fn host_with_chrome() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
    let tree = WorkspaceTree::bootstrap_demo();
    let chrome = ChromeState::new(tree);
    {
        let mut state = chrome.borrow_mut();
        state.quick_settings.brightness_available = true;
        state.quick_settings.radios.wifi_available = true;
        state.quick_settings.bluetooth_available = true;
    }
    let host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(chrome.clone());
    (host, chrome)
}

fn host_with_launch_polling() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
    let (mut host, chrome) = host_with_chrome();
    host.desktop_controls = DesktopControlsSource::Persisted(
        settings_store::hardware::DesktopControlsConfig::default(),
    );
    (host, chrome)
}

#[test]
fn desktop_controls_refresh_only_replaces_a_persisted_snapshot() {
    let enabled = settings_store::hardware::DesktopControlsConfig { vim_mode: true };
    let (mut persisted, _chrome) = host_with_launch_polling();
    persisted.set_desktop_controls_config(enabled);
    assert!(persisted.desktop_controls.config().vim_mode);

    let (mut explicit, _chrome) = host_with_chrome();
    explicit.set_desktop_controls_config(enabled);
    assert!(
        !explicit.desktop_controls.config().vim_mode,
        "a host given explicit preferences must not follow the settings file"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn reserved_chords_are_owned_by_rename_and_quick_settings_with_help_and_escape_paths() {
    fn workspace_rename_owns_reserved_chords_in_production_dispatch() {
        let (mut host, chrome) = host_with_chrome();
        {
            let mut state = chrome.borrow_mut();
            state.editing_workspace_name = Some("Draft name".into());
            state.editing_workspace_name_selection =
                ui::text_selection::Selection::collapsed("Draft name".len());
        }
        let before = chrome.borrow().tree().clone();
        let modifiers = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER);
        let state = AutoCommit::new();
        for key in ["w", "k", "t"] {
            let mut handled = false;
            assert!(host.handle_shell_keyboard_event(
                &Key::Character(key.into()),
                modifiers,
                false,
                &state,
                &automerge::ROOT,
                &mut handled,
            ));
            assert!(handled);
            assert!(!host.palette_visible, "Super+{key} must not open a palette");
            assert!(chrome.borrow().editing_workspace_name.is_some());
        }
        assert_eq!(
            format!("{:?}", chrome.borrow().tree().desktop.workspaces),
            format!("{:?}", before.desktop.workspaces)
        );
        assert_eq!(
            chrome.borrow().tree().desktop.active_strip,
            before.desktop.active_strip
        );
    }

    fn quick_settings_owns_reserved_chords_but_keeps_f1_and_escape_paths() {
        let (mut host, chrome) = host_with_chrome();
        chrome.borrow_mut().overlays.quick_settings = true;
        let before = chrome.borrow().tree().clone();
        let modifiers = winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER);
        let state = AutoCommit::new();
        for key in ["w", "k", "t"] {
            let mut handled = false;
            assert!(host.handle_shell_keyboard_event(
                &Key::Character(key.into()),
                modifiers,
                false,
                &state,
                &automerge::ROOT,
                &mut handled,
            ));
            assert!(handled);
            assert!(
                !host.palette_visible,
                "Super+{key} must not escape Quick Settings"
            );
            assert!(chrome.borrow().overlays.quick_settings);
        }
        assert_eq!(
            format!("{:?}", chrome.borrow().tree().desktop.workspaces),
            format!("{:?}", before.desktop.workspaces)
        );
        assert_eq!(
            chrome.borrow().tree().desktop.active_strip,
            before.desktop.active_strip
        );

        let mut handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::F1),
            winit::event::Modifiers::default(),
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(chrome.borrow().overlays.quick_settings);
        assert!(chrome.borrow().overlays.shortcut_help);

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Escape),
            winit::event::Modifiers::default(),
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(!chrome.borrow().overlays.shortcut_help);
        assert!(chrome.borrow().overlays.quick_settings);

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Escape),
            winit::event::Modifiers::default(),
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(!chrome.borrow().overlays.quick_settings);
    }

    fn quick_settings_absorbs_text_keys_but_keeps_f1_help_escape_hatch() {
        let (mut host, chrome) = host_with_chrome();
        chrome.borrow_mut().overlays.quick_settings = true;
        let mut handled = false;

        // An ordinary character must stop at the shell instead of reaching
        // the focused client underneath the popover.
        assert!(host.handle_shell_keyboard_event(
            &Key::Character("x".into()),
            Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(!chrome.borrow().overlays.shortcut_help);

        // F1 is the documented desktop-help escape hatch. It opens the guide
        // above QS and therefore never falls through to the client either.
        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::F1),
            Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(chrome.borrow().overlays.shortcut_help);

        // Escape first dismisses the guide, then the underlying QS popover.
        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Escape),
            Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(!chrome.borrow().overlays.shortcut_help);

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Escape),
            Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(!chrome.borrow().overlays.quick_settings);
    }

    fn direct_quick_settings_open_reconciles_focus_before_the_first_draw() {
        let (mut host, chrome) = host_with_chrome();

        // Simulate a retained selection from the previous visit, then open
        // through ChromeState as top-bar, accessibility and deep-link paths
        // do. The first key may arrive before any rendering pass.
        chrome.borrow_mut().overlays.quick_settings = true;
        host.reconcile_quick_settings_open_edge(true);
        assert!(
            host.qs_root
                .focus_next(&chrome.borrow().quick_settings, false)
        );

        let slider = host
            .qs_root
            .adjust_focused_slider(&mut chrome.borrow_mut().quick_settings, 1.0);
        assert!(
            matches!(slider, Some(SliderCommit::Brightness(_))),
            "the first Tab after every direct open must select Brightness"
        );
    }

    fn wifi_drill_in_keyboard_focus_can_activate_back() {
        let (mut host, chrome) = host_with_chrome();
        {
            let mut state = chrome.borrow_mut();
            state.overlays.quick_settings = true;
            state
                .quick_settings
                .push_level(crate::shell::quick_settings::QsLevel::Wifi);
        }

        let mut handled = false;
        assert!(host.handle_quick_settings_keyboard(
            &Key::Named(NamedKey::Tab),
            false,
            false,
            &mut handled,
        ));
        assert!(handled);
        handled = false;
        assert!(host.handle_quick_settings_keyboard(
            &Key::Named(NamedKey::Enter),
            false,
            false,
            &mut handled,
        ));
        assert!(handled);
        assert!(matches!(
            chrome.borrow().quick_settings.current_level(),
            crate::shell::quick_settings::QsLevel::Root
        ));
    }

    workspace_rename_owns_reserved_chords_in_production_dispatch();
    quick_settings_owns_reserved_chords_but_keeps_f1_and_escape_paths();
    quick_settings_absorbs_text_keys_but_keeps_f1_help_escape_hatch();
    direct_quick_settings_open_reconciles_focus_before_the_first_draw();
    wifi_drill_in_keyboard_focus_can_activate_back();
}

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn status_editor_and_presence_publish_live_manual_status_without_leaking_keys() {
    fn inline_status_editor_commits_and_publishes_without_leaking_keys_to_apps() {
        let settings = tempfile::tempdir().expect("isolated settings directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", settings.path());
        let (mut host, _) = host_with_chrome();
        let published = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let capture = std::rc::Rc::clone(&published);
        host.set_status_presence_bridge(std::rc::Rc::new(move |_room, status| {
            capture.borrow_mut().push(status.map(|status| status.text));
        }));
        host.status_editing = true;
        let state = AutoCommit::new();
        let mut handled = false;
        let modifiers = winit::event::Modifiers::default();
        assert!(host.handle_shell_keyboard_event(
            &Key::Character("Preparing Q2 review".into()),
            modifiers,
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Enter),
            modifiers,
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(
            host.pump_presence_requests(),
            "the next production update applies the queued semantic status request"
        );
        assert_eq!(host.local_status.as_deref(), Some("Preparing Q2 review"));
        assert_eq!(
            published.borrow().as_slice(),
            &[Some("Preparing Q2 review".into())]
        );
    }

    fn inline_status_editor_uses_full_selection_and_navigation_contract() {
        let (mut host, _) = host_with_chrome();
        host.status_editing = true;
        host.status_draft = "alpha beta".into();
        host.status_selection = ui::text_selection::Selection::collapsed(host.status_draft.len());
        let state = AutoCommit::new();
        let mut handled = false;
        let ctrl_shift = winit::event::Modifiers::from(
            winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT,
        );

        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::ArrowLeft),
            ctrl_shift,
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert_eq!(
            host.status_selection.range(),
            6..10,
            "Ctrl+Shift+Left selects the previous Unicode word"
        );

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Character("x".into()),
            winit::event::Modifiers::default(),
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert_eq!(host.status_draft, "alpha x");
        assert_eq!(
            host.status_selection,
            ui::text_selection::Selection::collapsed(7)
        );

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Character("a".into()),
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert_eq!(host.status_selection.range(), 0..7);

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Backspace),
            winit::event::Modifiers::default(),
            false,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(host.status_draft.is_empty());
        assert_eq!(
            host.status_selection,
            ui::text_selection::Selection::collapsed(0)
        );
    }

    fn inline_status_editor_repeats_navigation_and_deletion_but_not_text_insertion() {
        let (mut host, _) = host_with_chrome();
        host.status_editing = true;
        host.status_draft = "abcd".into();
        host.status_selection = ui::text_selection::Selection::collapsed(4);
        let state = AutoCommit::new();
        let modifiers = winit::event::Modifiers::default();
        let mut handled = false;

        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Backspace),
            modifiers,
            true,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert_eq!(host.status_draft, "abc");

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Character("z".into()),
            modifiers,
            true,
            &state,
            &automerge::ROOT,
            &mut handled,
        ));
        assert_eq!(
            host.status_draft, "abc",
            "a synthetic repeat press cannot duplicate printable text"
        );
    }

    fn editing_an_inferred_status_starts_a_fresh_manual_sentence() {
        let (mut host, _) = host_with_chrome();
        host.local_status = Some("Summarizing recent file changes".into());
        host.local_status_origin = ui::StatusOrigin::Inferred {
            generator: "local-model".into(),
        };
        let mut handled = false;
        host.apply_collaborators_hit(CollaboratorsHit::EditStatus, &mut handled);

        assert!(handled);
        assert!(host.status_editing);
        assert!(host.status_draft.is_empty());

        host.status_editing = false;
        host.local_status = Some("Closing the Q3 forecast".into());
        host.local_status_origin = ui::StatusOrigin::Manual;
        handled = false;
        host.apply_collaborators_hit(CollaboratorsHit::EditStatus, &mut handled);
        assert_eq!(host.status_draft, "Closing the Q3 forecast");
    }

    fn semantic_status_requests_use_the_live_presence_publisher_and_privacy_state() {
        let settings = tempfile::tempdir().expect("isolated settings directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", settings.path());
        let (mut host, chrome) = host_with_chrome();
        let published = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let capture = std::rc::Rc::clone(&published);
        host.set_status_presence_bridge(std::rc::Rc::new(move |_room, status| {
            capture.borrow_mut().push(status.map(|status| status.text));
        }));
        {
            let mut state = chrome.borrow_mut();
            state.push_presence_request(
                crate::shell::chrome_state::PresenceRequest::SetManualStatus(Some(
                    "Reviewing the release".into(),
                )),
            );
            state.push_presence_request(
                crate::shell::chrome_state::PresenceRequest::SetStatusSharing(
                    ui::StatusSharing::Off,
                ),
            );
        }

        assert!(host.pump_presence_requests());

        assert_eq!(host.local_status.as_deref(), Some("Reviewing the release"));
        assert_eq!(host.status_sharing, ui::StatusSharing::Off);
        assert_eq!(
            published.borrow().as_slice(),
            &[Some("Reviewing the release".into()), None],
            "the same publisher first receives the manual status and then its privacy revocation"
        );
        assert!(chrome.borrow_mut().take_presence_requests().is_empty());
    }

    fn focused_app_presence_is_edge_triggered_and_clears_when_focus_disappears() {
        let (mut host, chrome) = host_with_chrome();
        let published = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let capture = std::rc::Rc::clone(&published);
        host.set_active_tab_presence_bridge(std::rc::Rc::new(move |room, tab, present| {
            capture.borrow_mut().push((room, tab, present));
        }));
        host.refresh_active_tab_presence();
        let first = published.borrow().clone();
        assert_eq!(first.len(), 1);
        assert!(first[0].2, "the focused app is announced as present");
        host.refresh_active_tab_presence();
        assert_eq!(published.borrow().len(), 1, "steady frames publish nothing");

        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let active = tree.desktop.active;
            tree.desktop.active_strip.remove(&active);
        }
        host.refresh_active_tab_presence();
        let events = published.borrow();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1], (first[0].0, first[0].1, false));
    }

    inline_status_editor_commits_and_publishes_without_leaking_keys_to_apps();
    inline_status_editor_uses_full_selection_and_navigation_contract();
    inline_status_editor_repeats_navigation_and_deletion_but_not_text_insertion();
    editing_an_inferred_status_starts_a_fresh_manual_sentence();
    semantic_status_requests_use_the_live_presence_publisher_and_privacy_state();
    focused_app_presence_is_edge_triggered_and_clears_when_focus_disappears();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn palette_confirmations_cannot_forge_identity_or_admin_and_wait_for_the_overlay() {
    fn palette_false_confirmation_cannot_select_an_identity_or_grant_admin() {
        use crate::shell::search::{ResourceKind, SearchContext, SearchProvider};

        let (mut host, chrome) = host_with_chrome();
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let workspace = tree
                .desktop
                .active_workspace_mut()
                .expect("active workspace");
            workspace.set_owner(crate::shell::state::local_user_id_for_room(&workspace.room));
            drop(tree);
        }
        let mut document = AutoCommit::new();
        let action = {
            let state = chrome.borrow();
            crate::shell::search::providers::ActionsProvider::new()
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "use identity",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &document,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id == "identity.select")
                .expect("identity action")
        };
        let before = chrome.borrow().tree().desktop.workspaces[0].sandbox.clone();
        host.pending_palette_action = Some(action);
        host.palette
            .set_query(r#"{"id":"does-not-exist","confirm":false}"#);
        host.activate_selected_result(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        );

        assert!(
            host.pending_palette_action.is_some(),
            "refused palette action remains editable"
        );
        assert_eq!(
            chrome.borrow().tree().desktop.workspaces[0].sandbox,
            before,
            "false confirmation cannot create an Admin grant"
        );
    }

    fn palette_peer_prompt_stays_pending_until_overlay_applies_it() {
        use crate::shell::search::action_api::ActionArgs;
        use crate::shell::search::{
            InvocationDisposition, ResourceKind, SearchContext, SearchProvider,
        };

        let (mut host, chrome) = host_with_chrome();
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let workspace = tree
                .desktop
                .active_workspace_mut()
                .expect("active workspace");
            workspace.set_owner(crate::shell::state::local_user_id_for_room(&workspace.room));
            drop(tree);
        }
        chrome.borrow_mut().peer_sync_enabled = true;
        let mut document = AutoCommit::new();
        let action = {
            let state = chrome.borrow();
            crate::shell::search::providers::ActionsProvider::new()
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "invite collaborator",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &document,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id == "peer.invite")
                .expect("peer prompt action")
        };
        let args = ActionArgs::bind(
            &action.params,
            std::iter::empty::<(String, crate::shell::search::action_api::ArgValue)>(),
        )
        .expect("prompt has no arguments");
        let ticket = match action.on_begin_invoke.expect("prompt owns a lifecycle")(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .expect("prompt admission")
        {
            InvocationDisposition::Pending(ticket) => ticket,
            InvocationDisposition::Accepted(_) => panic!("queued prompt completed too early"),
        };
        assert!(ticket().expect("first poll").is_none());
        assert!(!host.peer_sync_overlay.visible());

        host.pump_peer_sync();

        assert!(host.peer_sync_overlay.visible());
        assert!(ticket().expect("completed poll").is_some());
    }

    palette_false_confirmation_cannot_select_an_identity_or_grant_admin();
    palette_peer_prompt_stays_pending_until_overlay_applies_it();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn configured_shortcuts_match_exactly_capture_safely_and_are_claimed_before_clients() {
    fn configurable_shortcut_matcher_handles_named_keys_and_exact_modifiers() {
        let chord = settings_store::hardware::KeyChord::parse("Super+Shift+Left").unwrap();
        let matching = winit::event::Modifiers::from(
            winit::keyboard::ModifiersState::SUPER | winit::keyboard::ModifiersState::SHIFT,
        );
        assert!(shortcut_chord_matches(
            &Key::Named(NamedKey::ArrowLeft),
            matching,
            &chord
        ));
        assert!(!shortcut_chord_matches(
            &Key::Named(NamedKey::ArrowLeft),
            winit::event::Modifiers::from(winit::keyboard::ModifiersState::SUPER),
            &chord
        ));
    }

    fn shortcut_capture_builds_the_pressed_named_chord() {
        let modifiers = winit::event::Modifiers::from(
            winit::keyboard::ModifiersState::SUPER | winit::keyboard::ModifiersState::SHIFT,
        );
        let chord = shortcut_chord_from_event(&Key::Named(NamedKey::ArrowDown), modifiers)
            .expect("a supported named key becomes a shortcut chord");
        assert_eq!(chord.display(), "Super+Shift+Down");
    }

    fn a_rebound_shortcut_is_claimed_before_a_focused_client() {
        let mut shortcuts = settings_store::hardware::ShortcutConfig::default();
        shortcuts
            .set(
                settings_store::hardware::ShortcutAction::Search,
                settings_store::hardware::KeyChord::parse("Alt+G").unwrap(),
            )
            .unwrap();
        let alt = winit::event::Modifiers::from(ModifiersState::ALT);
        assert!(configured_shortcut_matches(
            &shortcuts,
            &Key::Character("g".into()),
            alt,
        ));
        assert!(!configured_shortcut_matches(
            &shortcuts,
            &Key::Character("g".into()),
            winit::event::Modifiers::default(),
        ));
    }

    fn effective_shortcut_handoff_rejects_reserved_or_ambiguous_tables() {
        let (mut host, _) = host_with_chrome();
        let mut reserved = settings_store::hardware::ShortcutConfig::default();
        reserved.bindings.insert(
            settings_store::hardware::ShortcutAction::Search,
            settings_store::hardware::KeyChord {
                modifiers: std::collections::BTreeSet::from(["Ctrl".into()]),
                key: "f1".into(),
            },
        );
        assert_eq!(
            host.set_shortcut_config(reserved),
            Err(settings_store::hardware::ShortcutConfigError::ReservedSystemKey)
        );

        let mut duplicate = settings_store::hardware::ShortcutConfig::default();
        duplicate.bindings.insert(
            settings_store::hardware::ShortcutAction::Search,
            settings_store::hardware::KeyChord::parse("Ctrl+J").unwrap(),
        );
        duplicate.bindings.insert(
            settings_store::hardware::ShortcutAction::NextTab,
            settings_store::hardware::KeyChord::parse("Ctrl+J").unwrap(),
        );
        assert_eq!(
            host.set_shortcut_config(duplicate),
            Err(settings_store::hardware::ShortcutConfigError::Conflict(
                settings_store::hardware::ShortcutAction::Search
            ))
        );
    }

    fn shortcut_capture_explains_reserved_f1_and_print_screen() {
        use settings_store::hardware::ShortcutAction;

        let (mut host, _) = host_with_chrome();
        for key in [Key::Named(NamedKey::F1), Key::Named(NamedKey::PrintScreen)] {
            host.capture_shortcut_chord(ShortcutAction::Search, &key, Modifiers::default());
            assert_eq!(
                host.shortcut_rebind_message.as_deref(),
                Some("F1 and Print Screen are reserved by the desktop."),
                "{key:?} must not enter the editable shortcut table"
            );
        }
    }

    fn edge_triggered_shell_shortcuts_absorb_repeats_without_reopening() {
        let (mut host, chrome) = host_with_chrome();
        let mut handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::F1),
            winit::event::Modifiers::default(),
            true,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(
            !chrome.borrow().overlays.shortcut_help,
            "a repeated F1 must be consumed without toggling the guide"
        );
    }

    configurable_shortcut_matcher_handles_named_keys_and_exact_modifiers();
    shortcut_capture_builds_the_pressed_named_chord();
    a_rebound_shortcut_is_claimed_before_a_focused_client();
    effective_shortcut_handoff_rejects_reserved_or_ambiguous_tables();
    shortcut_capture_explains_reserved_f1_and_print_screen();
    edge_triggered_shell_shortcuts_absorb_repeats_without_reopening();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn session_power_actions_respect_inhibitors_and_report_failure() {
    fn logout_and_switch_user_respect_their_portal_inhibitors() {
        let (mut host, chrome) = host_with_chrome();
        let registry = tilo_power_mgmt::InhibitRegistry::new();
        host.set_session_inhibitors(registry.clone());

        let _logout_hold = registry.acquire_for(
            "Editor has an unsaved document",
            tilo_power_mgmt::inhibit::INHIBIT_LOGOUT,
        );
        assert!(host.apply_layer_intent(LayerIntent::Session(os_actions::SessionAction::Logout)));
        assert_eq!(chrome.borrow().notifications().len(), 1);
        assert!(
            chrome.borrow().notifications()[0]
                .body
                .contains("Editor has an unsaved document")
        );

        let _switch_hold = registry.acquire_for(
            "Call is sharing the screen",
            tilo_power_mgmt::inhibit::INHIBIT_USER_SWITCH,
        );
        assert!(
            host.apply_layer_intent(LayerIntent::Session(os_actions::SessionAction::SwitchUser))
        );
        assert_eq!(chrome.borrow().notifications().len(), 2);
        assert!(
            chrome.borrow().notifications()[1]
                .body
                .contains("Call is sharing the screen")
        );
    }

    fn logout_power_action_publishes_its_edge_before_dispatch_returns() {
        let (mut host, _chrome) = host_with_chrome();
        let directory = tempfile::tempdir().unwrap();
        let request = directory.path().join("logout-request");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_SESSION_LOGOUT_REQUEST", &request);

        assert!(host.apply_layer_intent(LayerIntent::Session(os_actions::SessionAction::Logout)));
        assert_eq!(std::fs::read_to_string(&request).unwrap(), "logout\n");
        assert!(os_actions::take_logout_request());
        assert!(!os_actions::take_logout_request());
        drop(env);
    }

    fn suspend_power_action_uses_the_coordinated_power_service() {
        let (mut host, _chrome) = host_with_chrome();
        let bridge = tilo_power_mgmt::PowerBridge::new();
        host.set_power_bridge(bridge.clone());

        assert!(host.apply_layer_intent(LayerIntent::Session(os_actions::SessionAction::Suspend)));
        assert_eq!(
            bridge.drain_commands(),
            vec![tilo_power_mgmt::PowerCommand::Suspend]
        );
        assert!(host.session_action_channel.1.try_recv().is_err());
    }

    fn suspend_without_the_power_service_reports_failure() {
        let (mut host, chrome) = host_with_chrome();

        assert!(host.apply_layer_intent(LayerIntent::Session(os_actions::SessionAction::Suspend)));
        assert!(host.drain_session_action_results());
        assert_eq!(chrome.borrow().notifications().len(), 1);
        assert!(chrome.borrow().notifications()[0].body.contains("suspend"));
    }

    fn logout_inhibitor_does_not_cross_block_switch_user() {
        let (mut host, chrome) = host_with_chrome();
        let registry = tilo_power_mgmt::InhibitRegistry::new();
        host.set_session_inhibitors(registry.clone());
        let _logout_hold =
            registry.acquire_for("Unsaved work", tilo_power_mgmt::inhibit::INHIBIT_LOGOUT);

        assert!(!registry.is_inhibited_for(tilo_power_mgmt::inhibit::INHIBIT_USER_SWITCH));
        // Do not apply SwitchUser here: an uninhibited action deliberately
        // reaches the real narrow OS wrapper. The registry assertion proves
        // the scope separation without invoking a host session action.
        assert!(chrome.borrow().notifications().is_empty());
    }

    fn failed_session_action_is_reported_in_chrome() {
        let (host, chrome) = host_with_chrome();
        host.session_action_channel
            .0
            .send(SessionActionResult {
                action: os_actions::SessionAction::Suspend,
                result: Err("logind refused suspend".into()),
            })
            .unwrap();

        assert!(host.drain_session_action_results());
        let notifications = chrome.borrow().notifications().to_vec();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].title, "Session action failed");
        assert!(notifications[0].body.contains("suspend"));
        assert!(!notifications[0].body.contains("logind"));
    }

    logout_and_switch_user_respect_their_portal_inhibitors();
    logout_power_action_publishes_its_edge_before_dispatch_returns();
    suspend_power_action_uses_the_coordinated_power_service();
    suspend_without_the_power_service_reports_failure();
    logout_inhibitor_does_not_cross_block_switch_user();
    failed_session_action_is_reported_in_chrome();
}

/// A killed or crashed app exits with an error after its window was up.
/// It did open, so no "Couldn't open" card; one that never showed a
/// window still gets it, and a relaunch starts unproven.
#[test]
fn an_app_that_showed_its_window_and_then_failed_is_not_reported_as_unopened() {
    let (mut host, chrome) = host_with_launch_polling();
    let package_id = "so.tilo.killed-after-window-fixture";
    let event = |kind| crate::shell::launcher::LaunchEvent {
        package_id: package_id.to_string(),
        display_name: "Fixture".to_string(),
        managed_agent_id: None,
        managed_workspace_id: None,
        kind,
    };
    let failed = || crate::shell::launcher::LaunchEventKind::Failed {
        code: Some(137),
        stderr_tail: String::new(),
    };
    let cards = |chrome: &crate::shell::chrome_state::Chrome| {
        chrome
            .borrow()
            .notifications()
            .iter()
            .filter(|n| n.title.contains("Fixture"))
            .count()
    };
    crate::shell::launcher::push_event(event(crate::shell::launcher::LaunchEventKind::Started {
        pid: 7,
    }));
    host.drain_launch_events();
    crate::shell::launcher::note_launch_presented(package_id);
    crate::shell::launcher::push_event(event(failed()));
    host.drain_launch_events();
    assert_eq!(cards(&chrome), 0, "the app did open");

    crate::shell::launcher::push_event(event(crate::shell::launcher::LaunchEventKind::Started {
        pid: 8,
    }));
    crate::shell::launcher::push_event(event(failed()));
    assert!(host.drain_launch_events());
    assert_eq!(
        cards(&chrome),
        1,
        "a relaunch that never showed a window failed to open"
    );
}

#[test]
fn launch_notices_retire_once_and_escape_dismisses_the_latest_error() {
    /// A launcher parent may exit after daemonising but before its GUI child
    /// maps a toplevel. The opening acknowledgement retires immediately while
    /// the placement promise remains available for the bounded handoff grace.
    fn clean_launch_exit_retires_its_pending_open_notice() {
        let (mut host, chrome) = host_with_launch_polling();
        let package_id = "so.tilo.exit-before-window-fixture";
        crate::shell::launcher::set_pending_placement(
            package_id,
            crate::compositor::tab_binding::Placement::NewGroup,
        );
        let notice = chrome.borrow_mut().push_notification_full(
            tilo_desktop::notifications::Notification::new("Opening Fixture", "Loading…")
                .with_dismiss_after(std::time::Duration::from_secs(120)),
        );
        crate::shell::catalogue::note_pending_notice(package_id, notice);
        crate::shell::launcher::push_event(crate::shell::launcher::LaunchEvent {
            package_id: package_id.to_string(),
            display_name: "Fixture".to_string(),
            managed_agent_id: None,
            managed_workspace_id: None,
            kind: crate::shell::launcher::LaunchEventKind::Exited,
        });

        assert!(host.drain_launch_events());
        assert!(
            chrome
                .borrow()
                .notifications()
                .iter()
                .all(|n| n.id != notice),
            "the terminal launch event must dismiss its opening notice"
        );
        assert!(
            crate::shell::catalogue::pending_notice(package_id).is_none(),
            "the pending id must be consumed along with the card"
        );
        assert_eq!(
            crate::shell::launcher::placement_for_toplevel(package_id, 99_001),
            Some(crate::compositor::tab_binding::Placement::NewGroup),
            "a daemonised GUI child must still inherit the requested placement"
        );
        crate::shell::launcher::clear_pending_placements();
    }

    /// The application callback has already put the short recovery card on
    /// screen when the launchpad adapter receives its refusal. The adapter's
    /// detailed log path must not append a second card with raw `ActionError`
    /// text (which can contain backend paths and embedded newlines).
    fn launchpad_refusal_reporting_does_not_duplicate_callback_notice() {
        let (_host, chrome) = host_with_chrome();
        let notice = chrome.borrow_mut().push_error_notification(
            "Couldn't open Notes",
            "The app launcher is unavailable. Open Applications and try again.",
        );
        let raw_error = crate::shell::search::action_api::ActionError::Refused(
            "spawn failed: /run/user/1000/tilo\nbackend detail".to_string(),
        );

        ShellOverlayHost::report_launchpad_action_failure("Notes", &raw_error);

        let state = chrome.borrow();
        assert_eq!(state.notifications().len(), 1);
        assert_eq!(state.notifications()[0].id, notice);
        assert_eq!(
            state.notifications()[0].body,
            "The app launcher is unavailable. Open Applications and try again."
        );
        assert!(
            state
                .notifications()
                .iter()
                .all(|notification| !notification.body.contains("/run/user")),
            "raw adapter diagnostics must stay out of notification chrome"
        );
    }

    fn escape_dismisses_the_latest_launch_error_without_touching_layout() {
        let (mut host, chrome) = host_with_chrome();
        let notice = chrome.borrow_mut().push_error_notification(
            "Couldn't open Notes",
            "The app stopped before opening. Open Applications and try again.",
        );
        let mut handled = false;
        assert!(host.handle_shell_escape_chord(&mut handled));
        assert!(handled);
        assert!(
            chrome
                .borrow()
                .notifications()
                .iter()
                .all(|n| n.id != notice),
            "Escape must dismiss the visible error"
        );
        assert_eq!(
            chrome.borrow().tree().desktop.workspaces.len(),
            WorkspaceTree::bootstrap_demo().desktop.workspaces.len(),
            "dismissing a notification must not mutate the desktop layout"
        );
    }

    clean_launch_exit_retires_its_pending_open_notice();
    launchpad_refusal_reporting_does_not_duplicate_callback_notice();
    escape_dismisses_the_latest_launch_error_without_touching_layout();
}

#[test]
fn escape_unwinds_the_innermost_overlay_state_first() {
    fn escape_returns_from_quick_settings_drill_in_before_closing_popover() {
        let (mut host, chrome) = host_with_chrome();
        {
            let mut state = chrome.borrow_mut();
            state.overlays.quick_settings = true;
            state
                .quick_settings
                .push_level(crate::shell::quick_settings::QsLevel::Wifi);
        }

        let mut handled = false;
        assert!(host.handle_shell_escape_chord(&mut handled));
        assert!(handled);
        {
            let state = chrome.borrow();
            assert!(
                state.overlays.quick_settings,
                "Back must keep the popover open"
            );
            assert!(matches!(
                state.quick_settings.current_level(),
                crate::shell::quick_settings::QsLevel::Root
            ));
        }

        handled = false;
        assert!(host.handle_shell_escape_chord(&mut handled));
        assert!(handled);
        assert!(
            !chrome.borrow().overlays.quick_settings,
            "Escape on the root page closes Quick Settings"
        );
    }

    fn escape_leaves_resource_search_scope_before_closing_palette() {
        let (mut host, chrome) = host_with_chrome();
        host.palette_visible = true;
        chrome.borrow_mut().search_resource_scope =
            Some(crate::shell::chrome_state::SearchResourceScope {
                peer_id: 9,
                cluster_id: "room".into(),
                app_id: "so.tilo.reader".into(),
                resource: "book:42".into(),
                label: "Book of Tilo".into(),
                kind: "ebook.book".into(),
            });

        let mut handled = false;
        assert!(host.handle_shell_escape_chord(&mut handled));
        assert!(handled);
        assert!(host.palette_visible, "Back keeps search open");
        assert!(chrome.borrow().search_resource_scope.is_none());

        handled = false;
        assert!(host.handle_shell_escape_chord(&mut handled));
        assert!(handled);
        assert!(!host.palette_visible, "Escape at the root closes search");
    }

    fn escape_cancels_wifi_password_before_leaving_wifi_page() {
        let (mut host, chrome) = host_with_chrome();
        {
            let mut state = chrome.borrow_mut();
            state.overlays.quick_settings = true;
            state
                .quick_settings
                .push_level(crate::shell::quick_settings::QsLevel::Wifi);
            state.quick_settings.wifi_password = Some(
                crate::shell::quick_settings::WifiPasswordPrompt::new("Secure network"),
            );
        }

        let mut handled = false;
        assert!(host.handle_shell_escape_chord(&mut handled));
        assert!(handled);
        let state = chrome.borrow();
        assert!(state.overlays.quick_settings);
        assert!(state.quick_settings.wifi_password.is_none());
        assert!(matches!(
            state.quick_settings.current_level(),
            crate::shell::quick_settings::QsLevel::Wifi
        ));
    }

    escape_returns_from_quick_settings_drill_in_before_closing_popover();
    escape_leaves_resource_search_scope_before_closing_palette();
    escape_cancels_wifi_password_before_leaving_wifi_page();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn shortcut_reference_is_complete_truthful_grouped_and_fits_compact_cells() {
    fn shortcut_reference_covers_every_production_essential() {
        let rows = shortcut_reference_rows(&settings_store::hardware::ShortcutConfig::default());
        let visible = rows
            .iter()
            .map(|(label, chord)| format!("{label}:{chord}"))
            .collect::<Vec<_>>();
        for expected in [
            "Screenshot:Print Screen",
            "Switch windows:Alt+Tab",
            "Close tab:Alt+F4  /  Super+W",
            "Reopen closed tab:Ctrl+Shift+T",
            "New window:Super+N",
            "Add tab here:Super+T",
            "Duplicate tab:Super+D",
            "Open terminal:Super+Enter",
            "Search everywhere:Super+Space",
            "Emoji:Super+.",
            "Overview:Super+Tab",
            "Lock screen:Super+L",
            "Quick Settings:Super+Q",
            "Workspace wheel:Super+S",
            "Display layout:Super+P",
            "Capture screenshot:Super+Shift+S",
            "Picture in picture:Super+Shift+P",
            "Save recent replay:Super+Shift+R",
        ] {
            assert!(
                visible.iter().any(|row| row == expected),
                "missing {expected}"
            );
        }
    }

    fn shortcut_reference_never_promises_a_rebound_default() {
        let mut shortcuts = settings_store::hardware::ShortcutConfig::default();
        shortcuts
            .set(
                settings_store::hardware::ShortcutAction::Search,
                settings_store::hardware::KeyChord::parse("Alt+G").unwrap(),
            )
            .unwrap();
        let rows = shortcut_reference_rows(&shortcuts);
        assert!(rows.contains(&("Search everywhere".into(), "Alt+G".into())));
        assert!(!rows.contains(&("Search everywhere".into(), "Super+Space".into())));
    }

    fn shortcut_reference_does_not_repeat_the_open_guide_action() {
        let rows = shortcut_reference_rows(&settings_store::hardware::ShortcutConfig::default());
        assert_eq!(rows.len(), 18);
        assert!(rows.iter().all(|(label, _)| *label != "Shortcut guide"));
    }

    fn shortcut_reference_groups_keep_a_consistent_responsive_rhythm() {
        assert_eq!(shortcut_reference_columns(688.0), 3);
        assert_eq!(shortcut_reference_columns(912.0), 3);
        assert_eq!(shortcut_reference_step(true), 31.0);
        assert_eq!(shortcut_reference_step(false), 34.0);
        assert!(shortcut_reference_group_height(true) >= 6.0 * shortcut_reference_step(true));
        assert!(shortcut_reference_group_height(false) >= 6.0 * shortcut_reference_step(false));
    }

    fn compact_shortcut_reference_keeps_the_complete_action_set_visible() {
        let rows = shortcut_reference_rows(&settings_store::hardware::ShortcutConfig::default());
        let body_height = 426.0;
        let navigation_height = 154.0;
        let essentials_y = navigation_height + 12.0;
        let reference_top = essentials_y + 18.0;
        let final_pill_bottom = reference_top + 29.0 + 5.0 * shortcut_reference_step(true) + 31.0;

        assert_eq!(
            rows.len(),
            18,
            "the guide must retain every documented desktop action without repeating itself"
        );
        assert!(
            final_pill_bottom <= body_height,
            "the final shortcut pill must remain inside an 800×600 guide body"
        );
        for essential in [
            "Screenshot",
            "Switch windows",
            "Close tab",
            "Reopen closed tab",
        ] {
            assert!(rows.iter().any(|(label, _)| *label == essential));
        }
    }

    fn compact_shortcut_recovery_actions_fit_their_cells() {
        let rows = shortcut_reference_rows(&settings_store::hardware::ShortcutConfig::default());
        let mut renderer = TextRenderer::new();
        let chord_size = Theme::dark().tokens.typography.size(TextRole::Caption);
        // An 800-wide viewport yields a 736-wide panel and 688-wide body. The
        // compact guide uses three semantic columns with quiet key pills.
        let columns = shortcut_reference_columns(688.0);
        assert_eq!(columns, 3);
        let available_text_width = (688.0 - 12.0 * 2.0) / 3.0 - 24.0;
        for (_, chord) in rows {
            let (width, _) = renderer.measure(&chord, chord_size, None);
            assert!(
                f64::from(width) + 16.0 <= available_text_width,
                "{chord:?} must not clip inside the compact guide's key pill"
            );
        }
    }

    fn shortcut_reference_columns_stay_semantically_grouped() {
        assert_eq!(shortcut_reference_columns(688.0), 3);
        assert_eq!(shortcut_reference_columns(912.0), 3);
        let rows = shortcut_reference_rows(&settings_store::hardware::ShortcutConfig::default());
        assert_eq!(rows.len(), 18);
        assert!(rows.chunks(6).all(|group| group.len() == 6));
    }

    fn shortcut_accessibility_names_workspace_and_group_navigation() {
        let (host, chrome) = host_with_chrome();
        chrome.borrow_mut().overlays.shortcut_help = true;
        let root = host.accessibility().expect("shell semantics");
        let guide = root
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Desktop keyboard and mouse guide"))
            .expect("guide dialog semantics");
        let description = guide.description.as_deref().expect("guide description");
        assert!(description.contains("workspaces with"));
        assert!(description.contains("move groups with"));
        assert!(description.contains("Screenshot: Print Screen"));
        assert!(!description.contains("Shortcut guide:"));
    }

    shortcut_reference_covers_every_production_essential();
    shortcut_reference_never_promises_a_rebound_default();
    shortcut_reference_does_not_repeat_the_open_guide_action();
    shortcut_reference_groups_keep_a_consistent_responsive_rhythm();
    compact_shortcut_reference_keeps_the_complete_action_set_visible();
    compact_shortcut_recovery_actions_fit_their_cells();
    shortcut_reference_columns_stay_semantically_grouped();
    shortcut_accessibility_names_workspace_and_group_navigation();
}

#[test]
fn shortcut_guide_names_keys_as_the_active_layout_prints_them() {
    use settings_store::hardware::{KeyChord, ShortcutAction};

    let (mut host, chrome) = host_with_chrome();
    host.shortcut_config
        .set(
            ShortcutAction::Search,
            KeyChord::parse("Ctrl+Shift+G").expect("chord"),
        )
        .expect("free chord");
    chrome.borrow_mut().overlays.shortcut_help = true;
    let guide_description = |host: &ShellOverlayHost| {
        host.accessibility()
            .expect("shell semantics")
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Desktop keyboard and mouse guide"))
            .and_then(|guide| guide.description.clone())
            .expect("guide description")
    };

    ui::keycaps::set_active(ui::Keycaps::from_xkb("de", ""));
    let german_rows = shortcut_reference_rows(&host.shortcut_config);
    let german = guide_description(&host);
    ui::keycaps::set_active(ui::Keycaps::US);
    let english = guide_description(&host);

    assert!(german_rows.contains(&("Search everywhere".into(), "Strg+Umschalt+G".into())));
    assert!(german_rows.contains(&("Reopen closed tab".into(), "Strg+Umschalt+T".into())));
    assert!(german.contains("Screenshot: Druck"), "{german}");
    assert!(german.contains("Strg+Umschalt+G"), "{german}");
    // The layout, not the stored chord, changed: switching back restores it.
    assert!(english.contains("Ctrl+Shift+G"), "{english}");
    assert!(english.contains("Screenshot: Print Screen"), "{english}");
    assert_eq!(
        host.shortcut_config.bindings[&ShortcutAction::Search].display(),
        "Ctrl+Shift+g"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn shortcut_guide_opens_from_every_help_route_and_is_keyboard_editable() {
    fn f1_and_configured_global_help_both_open_the_same_desktop_guide() {
        use settings_store::hardware::ShortcutAction;

        let (mut host, chrome) = host_with_chrome();
        let mut handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::F1),
            Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(chrome.borrow().overlays.shortcut_help);

        chrome.borrow_mut().overlays.shortcut_help = false;
        let chord = host
            .shortcut_config
            .bindings
            .get(&ShortcutAction::ShortcutHelp)
            .expect("default global help binding");
        assert_eq!(chord.display(), "Super+Shift+/");
        let key = Key::Character("?".into());
        let modifiers = Modifiers::from(ModifiersState::SUPER | ModifiersState::SHIFT);
        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &key,
            modifiers,
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert!(handled);
        assert!(chrome.borrow().overlays.shortcut_help);
    }

    fn clicking_a_painted_shortcut_starts_capture_and_escape_only_cancels_capture() {
        let (mut host, chrome) = host_with_chrome();
        chrome.borrow_mut().overlays.shortcut_help = true;
        let action = settings_store::hardware::ShortcutAction::FocusPreviousWindow;
        host.shortcut_binding_rects
            .push((action, Rect::new(20.0, 20.0, 80.0, 50.0)));
        host.last_cursor = Some(Point::new(40.0, 35.0));
        let click = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        };
        let mut handled = false;
        assert!(host.handle_shell_mouse_click_event(&click, &mut handled));
        assert!(handled);
        assert_eq!(host.shortcut_rebind, Some(action));

        let mut key_handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Escape),
            winit::event::Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut key_handled,
        ));
        assert!(key_handled);
        assert_eq!(host.shortcut_rebind, None);
        assert!(
            chrome.borrow().overlays.shortcut_help,
            "Escape cancels capture before it closes the guide"
        );
    }

    fn shortcut_guide_controls_are_keyboard_reachable() {
        let (mut host, chrome) = host_with_chrome();
        chrome.borrow_mut().overlays.shortcut_help = true;
        let mut handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Tab),
            winit::event::Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert_eq!(
            host.shortcut_guide_focus,
            Some(ShortcutGuideFocus::Binding(
                settings_store::hardware::ShortcutAction::FocusPreviousWindow
            ))
        );

        handled = false;
        assert!(host.handle_shell_keyboard_event(
            &Key::Named(NamedKey::Enter),
            winit::event::Modifiers::default(),
            false,
            &AutoCommit::new(),
            &automerge::ROOT,
            &mut handled,
        ));
        assert_eq!(
            host.shortcut_rebind,
            Some(settings_store::hardware::ShortcutAction::FocusPreviousWindow)
        );
    }

    f1_and_configured_global_help_both_open_the_same_desktop_guide();
    clicking_a_painted_shortcut_starts_capture_and_escape_only_cancels_capture();
    shortcut_guide_controls_are_keyboard_reachable();
}

#[test]
#[serial_test::serial(process_env)]
fn quick_settings_settings_opens_the_personal_app_or_reports_its_absence() {
    fn quick_settings_settings_opens_personal_settings_app_not_workspace_settings() {
        let (mut host, chrome) = host_with_chrome();
        let launches = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = std::rc::Rc::clone(&launches);
        host.app_launcher = std::rc::Rc::new(move |_, package_id, display_name| {
            recorded
                .borrow_mut()
                .push((package_id.to_string(), display_name.to_string()));
        });
        chrome.borrow_mut().overlays.quick_settings = true;
        host.apply_quick_settings_root_action(RootAction::OpenSettings, &chrome);

        let state = chrome.borrow();
        assert!(!state.overlays.quick_settings, "the source popover closes");
        assert!(
            !state.overlays.ws_settings,
            "device Settings must not be confused with workspace-sharing settings"
        );
        drop(state);
        assert_eq!(
            launches.borrow().as_slice(),
            &[("so.tilo.settings".into(), "Settings".into())],
            "the app-launch service receives the exact Personal Settings identity"
        );
    }

    fn missing_personal_settings_reports_launch_failure_without_opening_workspace_settings() {
        let tmp = tempfile::tempdir().unwrap();
        // An empty registry makes the personal Settings package genuinely
        // unavailable; the ordinary launcher owns wording and notification
        // lifetime for that error.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", tmp.path());
        crate::shell::launcher::invalidate_app_cache();
        crate::shell::launcher::clear_pending_placements();

        // This is deliberately the production constructor: the behavior under
        // test is its real app-launch service and unavailable-package notice.
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let mut host =
            ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome.clone());
        chrome.borrow_mut().overlays.quick_settings = true;
        host.apply_quick_settings_root_action(RootAction::OpenSettings, &chrome);

        let state = chrome.borrow();
        assert!(!state.overlays.quick_settings, "the source popover closes");
        assert!(
            !state.overlays.ws_settings,
            "an unavailable personal Settings app must not redirect into workspace sharing"
        );
        assert!(
            state
                .notifications()
                .iter()
                .any(|notification| notification.title.contains("Settings")),
            "the ordinary launch failure must remain visible to the user"
        );
        drop(state);

        env.remove("TILO_APPS_DATA_DIR");
        crate::shell::launcher::invalidate_app_cache();
        crate::shell::launcher::clear_pending_placements();
    }

    quick_settings_settings_opens_personal_settings_app_not_workspace_settings();
    missing_personal_settings_reports_launch_failure_without_opening_workspace_settings();
}

#[test]
fn dictation_model_setup_uses_semantic_normal_launcher_and_never_grants_capture() {
    use crate::compositor::dictation::{DictationStatus, runtime::RuntimeError};
    let (mut host, _) = host_with_chrome();
    let launches = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = std::rc::Rc::clone(&launches);
    host.app_launcher = std::rc::Rc::new(move |_, package_id, _| {
        recorded.borrow_mut().push(package_id.to_string());
    });
    host.layers
        .dictation
        .status(DictationStatus::Unavailable(RuntimeError::Worker(
            agent_provider_config::speech::SpeechError::ModelUnavailable,
        )));
    let node = host
        .layers
        .dictation
        .accessibility(host.layer_geometry(&[]))
        .unwrap();
    let target = node
        .children
        .first()
        .unwrap()
        .action_target
        .as_ref()
        .unwrap();
    let mut doc = automerge::AutoCommit::new();
    assert!(host.perform_semantic_action(
        target,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(launches.borrow().as_slice(), &["so.tilo.settings"]);
    assert_eq!(
        host.layers.dictation.take_request(),
        Some(dictation_layer::Request::Cancel(None))
    );
    assert!(!host.layers.dictation.is_open());
    assert!(!host.perform_semantic_action(
        target,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(launches.borrow().len(), 1);
}

#[test]
fn peer_sync_and_invite_manager_enable_idempotently_and_fail_closed() {
    fn with_peer_sync_enabled_is_idempotent() {
        let host = ShellOverlayHost::new(Box::new(Container::new()))
            .with_peer_sync_enabled(true)
            .with_peer_sync_enabled(true);
        assert!(host.peer_sync_enabled());
    }

    fn peer_sync_can_be_enabled_after_first_login_host_is_built() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let mut host = ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
            .with_chrome(chrome.clone());
        assert!(!host.peer_sync_enabled());
        assert!(!chrome.borrow().peer_sync_enabled);

        host.set_peer_sync_enabled(true);

        assert!(host.peer_sync_enabled());
        assert!(chrome.borrow().peer_sync_enabled);
    }

    fn invite_manager_fails_closed_without_invite_capability() {
        let (mut h, chrome) = host_with_chrome();
        chrome.borrow_mut().peer_sync_enabled = true;

        h.open_invite_manager();

        assert!(
            h.peer_sync_snapshot().invite_name.is_none(),
            "a user without peer.invite visibility must not reach the composer"
        );
        assert_eq!(
            chrome
                .borrow()
                .notifications()
                .last()
                .map(|n| n.title.as_str()),
            Some("Invite unavailable")
        );
    }

    /// An empty field is the question still unanswered. Minting anyway
    /// is exactly what left the host looking at a number.
    fn the_composer_refuses_to_mint_for_nobody() {
        let (mut h, chrome) = host_with_chrome();
        {
            let state = chrome.borrow();
            state
                .tree_mut()
                .adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
        }
        chrome.borrow_mut().peer_sync_enabled = true;
        h.set_peer_sync_bridge(std::rc::Rc::new(|_| {
            Ok(PeerSyncResponse::Message("a-token".to_string()))
        }));
        chrome
            .borrow_mut()
            .push_peer_sync_request(PeerSyncAction::PromptInvite);
        h.pump_peer_sync();

        let none = Modifiers::default();
        h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Enter)), none);

        assert!(
            h.peer_sync_snapshot().invite_token.is_none(),
            "nothing was minted for nobody"
        );
        assert_eq!(
            h.peer_sync_snapshot().invite_name.as_deref(),
            Some(""),
            "and the card is still asking"
        );
    }

    with_peer_sync_enabled_is_idempotent();
    peer_sync_can_be_enabled_after_first_login_host_is_built();
    invite_manager_fails_closed_without_invite_capability();
    the_composer_refuses_to_mint_for_nobody();
}

// Proves both halves of the invite-token crossing (mint → clipboard
// → paste) against the process clipboard.

use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn key_event(key: Key) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        key,
        None,
        ElementState::Pressed,
    ))
}

const SAMPLE_TOKEN: &str = "eyJ3b3Jrc3BhY2VfaWQiOjEsInJvb21faWQiOiJhYmMifQ";

#[test]
#[serial_test::serial(clipboard)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn invite_tokens_travel_through_the_clipboard_exactly() {
    fn ctrl_c_puts_the_minted_invite_token_on_the_clipboard() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_token: Some(SAMPLE_TOKEN.to_string()),
            ..PeerSyncSnapshot::default()
        });
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Character("c".into())), ctrl),
            "the card owns Ctrl+C while it is showing a token"
        );

        assert_eq!(
            ui::text_selection::clipboard_paste().as_deref(),
            Some(SAMPLE_TOKEN),
            "the token has to reach the clipboard — that is the only way it \
             gets to the other machine without being retyped"
        );
        assert!(
            h.peer_sync_snapshot()
                .status
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains("copied"),
            "a copy the user cannot see happen is a copy they will do twice"
        );
    }

    /// One gesture, one outcome: a token for that person, on the clipboard,
    /// with the card saying so. Splitting mint from Copy is how the copy
    /// gets skipped — nobody thinks to do the second half.
    fn the_composer_mints_for_the_name_it_was_given_and_copies() {
        let (mut h, chrome) = host_with_chrome();
        {
            let state = chrome.borrow();
            state
                .tree_mut()
                .adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
        }
        chrome.borrow_mut().peer_sync_enabled = true;
        h.set_peer_sync_bridge(std::rc::Rc::new(|req| match req {
            PeerSyncRequest::IssueInviteFor { name, .. } => {
                Ok(PeerSyncResponse::Message(format!("token-for-{name}")))
            }
            PeerSyncRequest::IssueInvite | PeerSyncRequest::IssueInvitation(_) => {
                Ok(PeerSyncResponse::Message("token-for-nobody".to_string()))
            }
            PeerSyncRequest::ExportIdentity => Ok(PeerSyncResponse::Message(format!(
                "tilo-identity:{}",
                "42".repeat(32)
            ))),
            PeerSyncRequest::AcceptInvite(_)
            | PeerSyncRequest::PollEnrollment(_)
            | PeerSyncRequest::CancelEnrollment(_) => Err("not here".to_string()),
            PeerSyncRequest::ListPendingInvitations => {
                Ok(PeerSyncResponse::PendingInvitations(Vec::new()))
            }
            PeerSyncRequest::ResendInvitation(_) | PeerSyncRequest::CancelInvitation(_) => {
                Err("not pending".into())
            }
        }));
        chrome
            .borrow_mut()
            .push_peer_sync_request(PeerSyncAction::PromptInvite);
        h.pump_peer_sync();
        assert_eq!(
            h.peer_sync_snapshot().invite_name.as_deref(),
            Some(""),
            "the composer opened with an empty field"
        );

        let none = Modifiers::default();
        for c in ["X", "a", "v", "e", "r"] {
            h.handle_peer_sync_event(&key_event(Key::Character(c.into())), none);
        }
        assert_eq!(
            h.peer_sync_snapshot().invite_name.as_deref(),
            Some("Xaver"),
            "the field takes the name — the same keys that type a token"
        );
        let mut snapshot = h.peer_sync_snapshot();
        snapshot.invite_recipient_key = Some([0x42; 32]);
        h.set_peer_sync_snapshot_for_tests(snapshot);

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Enter)), none),
            "Enter belongs to the composer while it is up"
        );
        assert!(
            h.peer_sync_snapshot().invite_review_open,
            "the first Enter is an explicit review, not a mint"
        );
        assert!(
            h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Enter)), none),
            "Enter confirms after the review is visible"
        );
        assert_eq!(
            h.peer_sync_snapshot().invite_token.as_deref(),
            Some("token-for-Xaver"),
            "the name reached the request, so it can reach the room document"
        );
        assert_eq!(
            h.peer_sync_snapshot().invite_for.as_deref(),
            Some("Xaver"),
            "and the card knows whose invite it is holding"
        );
        assert!(
            h.peer_sync_snapshot().invite_name.is_none(),
            "the question is answered, so the card has moved on from asking it"
        );
        assert_eq!(
            ui::text_selection::clipboard_paste().as_deref(),
            Some("token-for-Xaver"),
            "one gesture, one outcome: the token is on the clipboard"
        );
    }

    fn ctrl_v_fills_the_connect_prompt_from_the_clipboard() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        ui::text_selection::clipboard_copy(SAMPLE_TOKEN);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);

        assert!(
            h.handle_peer_sync_event(&key_event(Key::Character("v".into())), ctrl),
            "the prompt owns Ctrl+V"
        );

        assert_eq!(
            h.peer_sync_snapshot().connect_input.as_deref(),
            Some(SAMPLE_TOKEN),
            "pasting is the receiving half; without it the token still has \
             to be typed in by hand"
        );
    }

    /// A token copied out of a chat window arrives wrapped. The prompt
    /// takes the token, not the line breaks around it.
    fn a_pasted_token_is_stripped_of_the_whitespace_it_travelled_with() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        ui::text_selection::clipboard_copy(&format!("  {SAMPLE_TOKEN}\n"));
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        h.handle_peer_sync_event(&key_event(Key::Character("v".into())), ctrl);
        assert_eq!(
            h.peer_sync_snapshot().connect_input.as_deref(),
            Some(SAMPLE_TOKEN)
        );
    }

    /// The chord must not also type its own letter. `v` is a legal
    /// base64 character, so a paste that left one behind would corrupt
    /// the token in a way the user could not see.
    fn the_paste_chord_does_not_also_type_a_v() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        ui::text_selection::clipboard_copy(SAMPLE_TOKEN);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        h.handle_peer_sync_event(&key_event(Key::Character("v".into())), ctrl);
        let got = h.peer_sync_snapshot().connect_input.unwrap();
        assert!(
            !got.ends_with('v') || SAMPLE_TOKEN.ends_with('v'),
            "Ctrl+V pasted and typed: {got}"
        );
        assert_eq!(got, SAMPLE_TOKEN);
    }

    /// Unmodified characters still type. The chord guard must not have
    /// cost the prompt its ability to accept a token by hand.
    fn plain_characters_still_type_into_the_prompt() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        let none = Modifiers::default();
        for c in ["a", "b", "c"] {
            h.handle_peer_sync_event(&key_event(Key::Character(c.into())), none);
        }
        assert_eq!(h.peer_sync_snapshot().connect_input.as_deref(), Some("abc"));
    }

    ctrl_c_puts_the_minted_invite_token_on_the_clipboard();
    the_composer_mints_for_the_name_it_was_given_and_copies();
    ctrl_v_fills_the_connect_prompt_from_the_clipboard();
    a_pasted_token_is_stripped_of_the_whitespace_it_travelled_with();
    the_paste_chord_does_not_also_type_a_v();
    plain_characters_still_type_into_the_prompt();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn loading_card_offers_its_gestures_and_settles_when_the_window_paints() {
    /// Every face of the card offers its gesture as a button too, for
    /// a user who reaches for the pointer.
    fn each_face_of_the_card_offers_its_own_gesture_as_a_button() {
        let (mut h, _) = host_with_chrome();
        h.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_name: Some("Xaver".to_string()),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"review") && names.contains(&"new-recipient"),
            "the recipient face must offer review and a way to change who, got {names:?}"
        );
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_name: Some("Xaver".to_string()),
            invite_review_open: true,
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"create"),
            "the reviewed invite face must offer its Create gesture, got {names:?}"
        );
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_token: Some(SAMPLE_TOKEN.to_string()),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"copy"),
            "the issuing side must offer Copy, got {names:?}"
        );

        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some(String::new()),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"paste"),
            "the receiving side must offer Paste, got {names:?}"
        );
        assert!(
            names.contains(&"copy-identity"),
            "the recipient must be able to send a bindable identity card, got {names:?}"
        );
        assert!(names.contains(&"join"), "and still Join, got {names:?}");

        // Half a token: what the field holds while typed by hand (a paste
        // arrives whole), exercising `elide_token` — a card that panicked
        // here would take the compositor with it.
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            connect_input: Some("e".repeat(400)),
            ..PeerSyncSnapshot::default()
        });
        let names = draw_and_collect_peer_sync_buttons(&mut h);
        assert!(
            names.contains(&"join") && names.contains(&"paste"),
            "a field holding something unfinished still offers both, got {names:?}"
        );
    }

    /// The loading notice must outlive the fork (nothing on screen yet) but
    /// retire the moment the window's own place says which app is coming —
    /// otherwise it's noise, or a stale "Loading…" under "isn't responding".
    fn the_loading_indication_stands_until_the_window_place_speaks_for_itself() {
        const SURFACE: u64 = 918_273;
        let (h, chrome) = host_with_launch_polling();
        let notice = {
            let mut st = chrome.borrow_mut();
            st.push_notification_full(tilo_desktop::notifications::Notification::new(
                "Opening Ledger",
                "Loading…",
            ))
        };
        crate::shell::catalogue::note_pending_notice("so.tilo.retirefixture", notice);

        assert!(
            !h.retire_notices_for_opened_windows(),
            "nothing has appeared yet, so nothing is settled"
        );
        assert!(
            chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "the user is still waiting; the acknowledgement must still be up"
        );

        // Adoption isn't the end of the wait — for the first moments the
        // tab holds nothing visible, and a blink-fast app must not be
        // preceded by an indicator flashing on and off.
        let store = crate::compositor::foreign::store();
        store.forget(SURFACE);
        {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::adopt_toplevel(
                &mut tree,
                SURFACE,
                Some("so.tilo.retirefixture".to_string()),
            );
        }
        store.backdate_first_seen(SURFACE, std::time::Duration::ZERO);
        assert!(
            !h.retire_notices_for_opened_windows(),
            "a tab with nothing painted in it yet is not somewhere the wait \
             can be handed to; retiring on it hands the user back an empty \
             desktop"
        );
        assert!(
            chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "still waiting — nothing stands in the window's place"
        );

        // Past the delay the window's place names the app; the notice
        // stands down rather than repeating it.
        store.backdate_first_seen(
            SURFACE,
            crate::compositor::foreign::OPENING_DELAY + std::time::Duration::from_millis(50),
        );
        assert!(
            h.retire_notices_for_opened_windows(),
            "the window's own place is naming the app; a second indication \
             in the corner is noise"
        );
        assert!(
            !chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "the wait has moved to where the window will be, and only one \
             thing may be saying it"
        );
        store.forget(SURFACE);
    }

    /// The other way the hand-over can happen: an app fast enough to paint
    /// before the card was ever due still settles the open.
    fn a_window_that_paints_before_the_card_is_due_settles_the_open_too() {
        const SURFACE: u64 = 918_274;
        let (h, chrome) = host_with_launch_polling();
        let notice = {
            let mut st = chrome.borrow_mut();
            st.push_notification_full(tilo_desktop::notifications::Notification::new(
                "Opening Ledger",
                "Loading…",
            ))
        };
        crate::shell::catalogue::note_pending_notice("so.tilo.retirefixture", notice);

        let store = crate::compositor::foreign::store();
        store.forget(SURFACE);
        {
            let st = chrome.borrow();
            let mut tree = st.tree_mut();
            crate::compositor::tab_binding::adopt_toplevel(
                &mut tree,
                SURFACE,
                Some("so.tilo.retirefixture".to_string()),
            );
        }
        store.backdate_first_seen(SURFACE, std::time::Duration::ZERO);
        assert!(
            !h.retire_notices_for_opened_windows(),
            "precondition: nothing stands in the window's place yet"
        );

        // A frame that fits the tab, inside the delay — so the card was
        // never due and the window is simply there.
        let tile = vello::kurbo::Rect::new(0.0, 0.0, 120.0, 80.0);
        store.publish_rgba(SURFACE, &vec![0; 120 * 80 * 4], 120, 80);
        assert!(
            store.mark_presented(SURFACE, tile),
            "precondition: it presented"
        );
        assert!(
            h.retire_notices_for_opened_windows(),
            "the window is on screen, which is the end of the wait however \
             fast it came"
        );
        assert!(
            !chrome
                .borrow()
                .notifications()
                .iter()
                .any(|n| n.id == notice),
            "once the app is on screen the indication has done its job"
        );
        store.forget(SURFACE);
    }

    /// Escape still dismisses. The card gained chords; it must not have
    /// lost the way out.
    fn escape_still_dismisses_the_card() {
        let (mut h, _) = host_with_chrome();
        h.set_peer_sync_snapshot_for_tests(PeerSyncSnapshot {
            invite_token: Some(SAMPLE_TOKEN.to_string()),
            ..PeerSyncSnapshot::default()
        });
        let none = Modifiers::default();
        assert!(h.handle_peer_sync_event(&key_event(Key::Named(NamedKey::Escape)), none));
        assert!(h.peer_sync_snapshot().invite_token.is_none());
    }

    each_face_of_the_card_offers_its_own_gesture_as_a_button();
    the_loading_indication_stands_until_the_window_place_speaks_for_itself();
    a_window_that_paints_before_the_card_is_due_settles_the_open_too();
    escape_still_dismisses_the_card();
}

/// The invitation actions run on the frame pump through the same peer-sync
/// bridge as the invite card, and answer their caller rather than the card.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its shared bridge visible"
)]
fn invitation_actions_return_their_network_output_to_the_caller() {
    use crate::shell::search::InvocationDisposition;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};

    fn begin(
        chrome: &crate::shell::chrome_state::Chrome,
        id: &str,
        args: &serde_json::Value,
    ) -> crate::shell::search::PendingInvocation {
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(std::rc::Rc::new(
            crate::shell::search::providers::actions::ActionsProvider::new(),
        ));
        let mut doc = AutoCommit::new();
        let resource = ResourceRef::new("actions", id);
        let catalog = ActionCatalog::build_exact(
            &registry,
            &resource,
            crate::shell::chrome_state::SearchScope::All,
            None,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        let disposition = catalog
            .select(&resource)
            .unwrap_or_else(|| panic!("{id} is published"))
            .invoke_json(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                chrome,
                args.as_object().unwrap(),
            )
            .unwrap_or_else(|error| panic!("{id} is admitted: {error:?}"));
        let InvocationDisposition::Pending(ticket) = disposition else {
            panic!("{id} waits for the network instead of inventing a result");
        };
        ticket
    }

    fn output(receipt: crate::shell::search::ActionReceipt) -> Option<serde_json::Value> {
        match receipt.update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => output.value,
            other => panic!("expected success, got {other:?}"),
        }
    }

    let (mut h, chrome) = host_with_chrome();
    {
        let state = chrome.borrow();
        state
            .tree_mut()
            .adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
    }
    chrome.borrow_mut().peer_sync_enabled = true;
    let pending_id = [0x42; 32];
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = std::rc::Rc::clone(&seen);
    h.set_peer_sync_bridge(std::rc::Rc::new(move |req| {
        recorded.borrow_mut().push(req.clone());
        match req {
            PeerSyncRequest::IssueInvitation(_) => Ok(PeerSyncResponse::Message("tok".into())),
            PeerSyncRequest::ListPendingInvitations => {
                Ok(PeerSyncResponse::PendingInvitations(vec![
                    PendingInvitationSummary {
                        id: pending_id,
                        name: "Bob".into(),
                        role: crate::network::invite::InviteRole::Viewer,
                        expiry_unix_seconds: 1_900_000_000,
                    },
                ]))
            }
            PeerSyncRequest::CancelInvitation(id) if id == pending_id => {
                Ok(PeerSyncResponse::Message("canceled".into()))
            }
            _ => Err("not here".into()),
        }
    }));

    // Phase `issue_returns_the_minted_token`.
    {
        let ticket = begin(
            &chrome,
            "invitation.issue",
            &serde_json::json!({
                "role": "viewer",
                "expires_in_seconds": 600,
                "invitee_name": "Bob",
                "confirm": true,
            }),
        );
        assert!(ticket().unwrap().is_none(), "nothing ran before the pump");
        h.pump_peer_sync();
        let receipt = ticket().unwrap().expect("the pump completed the request");
        assert_eq!(output(receipt), Some(serde_json::json!({"invite": "tok"})));
        assert!(
            seen.borrow()
                .contains(&PeerSyncRequest::IssueInvitation(InvitationTerms {
                    role: crate::network::invite::InviteRole::Viewer,
                    expires_in_seconds: 600,
                    invitee_name: Some("Bob".into()),
                    recipient_user_public_key: None,
                }))
        );
        assert!(
            h.peer_sync_snapshot().invite_token.is_none(),
            "a token minted for a caller is not put on this screen"
        );
    }

    // Phase `list_returns_the_pending_invitations`.
    {
        let ticket = begin(
            &chrome,
            "invitation.list",
            &serde_json::json!({"confirm": true}),
        );
        h.pump_peer_sync();
        assert_eq!(
            output(ticket().unwrap().unwrap()),
            Some(serde_json::json!({"invitations": [{
                "invite_id": hex::encode(pending_id),
                "name": "Bob",
                "role": "viewer",
                "expires_at": 1_900_000_000_u64,
            }]}))
        );
    }

    // Phase `cancel_of_an_unknown_invitation_is_refused`.
    {
        let ticket = begin(
            &chrome,
            "invitation.cancel",
            &serde_json::json!({"invite_id": "07".repeat(32), "confirm": true}),
        );
        h.pump_peer_sync();
        assert!(matches!(
            ticket(),
            Err(crate::shell::search::action_api::ActionError::Refused(error))
                if error == "invitation is no longer pending"
        ));
        assert!(
            !seen
                .borrow()
                .contains(&PeerSyncRequest::CancelInvitation([7; 32])),
            "an id this workspace does not hold never reaches the network"
        );
    }

    // Phase `cancel_of_a_pending_invitation_succeeds`.
    {
        let ticket = begin(
            &chrome,
            "invitation.cancel",
            &serde_json::json!({"invite_id": hex::encode(pending_id), "confirm": true}),
        );
        h.pump_peer_sync();
        assert_eq!(
            output(ticket().unwrap().unwrap()),
            Some(serde_json::json!({"canceled": true}))
        );
    }
}

fn draw_and_collect_peer_sync_buttons(h: &mut ShellOverlayHost) -> Vec<&'static str> {
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let theme = Theme::dark();
    h.draw_peer_sync(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    h.peer_sync_prompt_rects_for_tests()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

// -----------------------------------------------------------------
// An open the user is waiting on must stay acknowledged until they
// can see how it ended.
// -----------------------------------------------------------------
