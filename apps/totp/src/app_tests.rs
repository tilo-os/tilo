use super::*;
use crate::model::demo_accounts;
use crate::test_events::{char_press, ctrl, named_press, no_mods};
use ui::headless::Framed;

// The demo root is used so tests get a deterministic sample vault and never
// read or write the real on-disk vault.
fn app_with_state() -> (AuthenticatorApp, ui::State) {
    let mut app = AuthenticatorApp::demo();
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    (app, state)
}

/// Mount `app` over `state` in the shared frame harness and run its first
/// frame at `w`x`h`: the production sync, layout and draw passes.
fn framed((app, state): (AuthenticatorApp, ui::State), w: u32, h: u32) -> Framed<AuthenticatorApp> {
    Framed::new(state, app, (w, h), Theme::dark())
}

/// Run frames until a sheet that just opened or changed shape has settled.
fn settle(app: &mut Framed<AuthenticatorApp>) {
    for _ in 0..3 {
        Framed::harness_mut(app).relayout();
    }
}

/// Run the next frame at a concrete window size.
fn layout_at(app: &mut Framed<AuthenticatorApp>, w: u32, h: u32) {
    Framed::harness_mut(app)
        .relayout_at((w, h))
        .expect("layout settles");
}

/// Route one event through the app's real `handle_event`, returning whether it
/// was marked handled.
fn dispatch(
    app: &mut Framed<AuthenticatorApp>,
    ev: &WindowEvent,
    mods: winit::event::Modifiers,
) -> bool {
    Framed::harness_mut(app).dispatch_to_root_with(ev, &mods)
}

/// Reduced motion wakes on second boundaries, and an unchanged vault skips the
/// document mirror.
#[test]
fn idle_frames_do_no_redundant_work() {
    // Case `reduced_motion_replaces_vsync_with_a_second_boundary_wake`.
    {
        let mut app = AuthenticatorApp::demo();
        assert!(app.needs_animation_frame());
        assert!(app.next_wake_deadline().is_none());

        Widget::set_motion_reduced(&mut app, true);

        assert!(!app.needs_animation_frame());
        let deadline = app
            .next_wake_deadline()
            .expect("the discrete countdown schedules its next second");
        let delay = deadline.saturating_duration_since(ui::time::now());
        assert!(delay > Duration::ZERO);
        assert!(delay <= Duration::from_secs(1));
    }

    // Case `doc_mirror_is_skipped_when_nothing_changed`.
    {
        // After the initial seed+mirror, a second sync with no vault change
        // must not re-dirty or rewrite the document.
        let (mut app, mut state) = app_with_state();
        assert!(!app.doc_dirty, "sync clears the doc-dirty flag");
        app.sync(&mut state.doc, &state.root);
        assert!(!app.doc_dirty, "idle sync leaves the doc-dirty flag clear");
    }
}

/// A fresh vault is published by account actions, not a raw document write.
#[test]
fn a_fresh_vault_is_published_by_account_actions() {
    let mut app = AuthenticatorApp::demo();
    let mut state = ui::State::new();
    Widget::sync(&mut app, &mut state.doc, &state.root);
    let published = AuthDoc::list_accounts(&state.doc);
    let local = app.vault.ordered();
    assert!(!local.is_empty());
    assert_eq!(published.len(), local.len());
    assert!(
        published.iter().all(|account| account.secret.is_empty()),
        "the collaborative document contains metadata only"
    );
    assert!(
        app.actions.take_committed().is_some(),
        "the seed mirror is an AddAccount on the action host"
    );
}

/// Resource, accessibility, event and Enter copies share the row's clipboard
/// semantics; toasts distinguish failures.
#[test]
fn copy_paths_share_row_semantics_and_toast_severity() {
    // Case `resource_copy_invokes_the_same_clipboard_semantics_as_the_row`.
    {
        let (mut app, mut state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        let payload =
            serde_json::to_vec(&crate::actions::TotpOperation::CopyCode { id: id.to_string() })
                .unwrap();
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    OperationKind::Effect,
                    crate::actions::TOTP_OPERATION_TYPE,
                    &payload
                ),
                &mut state.doc,
                &state.root
            )
            .map(|started| started.is_some())
            .unwrap()
        );
        assert_eq!(app.selected, Some(id));
        assert!(app.last_copied().is_some());
    }

    // Case `accessibility_copy_uses_the_existing_list_event_path`.
    {
        let mut framed = framed(app_with_state(), 640, 700);
        let (app, state) = Framed::split(&mut framed);
        let id = app.vault.ordered()[0].id;
        assert!(app.perform_semantic_action(
            &format!("totp.copy:{id}"),
            &A11yAction::Activate,
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(app.selected_account(), Some(id));
        assert!(app.last_copied().is_some());
    }

    // Case `copy_event_selects_the_account_and_confirms_with_a_toast`.
    {
        let (mut app, mut state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        let acc = app.vault.get(id).cloned().unwrap();
        let before = totp::current_code(&acc);
        app.apply_list_event(ListEvent::Copy(id), &mut state.doc);
        let after = totp::current_code(&acc);
        assert_eq!(app.selected, Some(id), "the copied row becomes selected");
        let (msg, _, kind) = app
            .toast
            .clone()
            .expect("copying shows a confirmation toast");
        assert!(msg.starts_with("Copied"), "toast confirms the copy: {msg}");
        assert_eq!(kind, ToastKind::Success);
        // The copied string is the account's live TOTP code (not its title or
        // an empty string). It equals the code at some instant during the copy,
        // so it must match the value just before or just after, which holds
        // even across a period rollover.
        let copied = app.last_copied().expect("a copy records the copied code");
        assert!(
            copied == before || copied == after,
            "copied {copied:?} must equal the account's live code ({before:?}/{after:?})"
        );
    }

    // Case `enter_copies_the_selected_accounts_live_code`.
    {
        let mut app = framed(app_with_state(), 1000, 800);
        let id = app
            .selected
            .expect("the demo vault selects the first account");
        let acc = app.vault.get(id).cloned().unwrap();

        let before = totp::current_code(&acc);
        dispatch(&mut app, &named_press(NamedKey::Enter), no_mods());
        let after = totp::current_code(&acc);

        let copied = app.last_copied().expect("Enter copies the selected code");
        assert!(
            copied == before || copied == after,
            "Enter copies the account's live TOTP code ({copied:?} vs {before:?}/{after:?})"
        );
        let (msg, _, kind) = app.toast.clone().expect("Enter shows a copy toast");
        assert!(msg.starts_with("Copied"), "toast confirms the copy: {msg}");
        assert_eq!(kind, ToastKind::Success);
    }

    // Case `a_row_without_a_key_refuses_to_copy`.
    {
        let (mut app, mut state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        app.vault.accounts[0].secret.clear();
        app.apply_list_event(ListEvent::Copy(id), &mut state.doc);
        assert!(app.last_copied().is_none(), "no key, nothing to copy");
        assert_eq!(app.toast.as_ref().map(|t| t.2), Some(ToastKind::Error));
    }
}

/// The accessibility tree names accounts without codes, and the open sheet is
/// the only thing a screen reader reaches.
#[test]
fn accessibility_names_accounts_and_the_sheet_owns_the_window() {
    // Case `accessibility_tree_names_accounts_without_exposing_codes`.
    {
        let app = framed(app_with_state(), 640, 700);
        let tree = app.accessibility().expect("root has a semantic group");
        assert_eq!(tree.name.as_deref(), Some("Authenticator"));
        let list = tree
            .children
            .iter()
            .find(|node| node.role == Role::List)
            .expect("named account list");
        assert!(!list.children.is_empty(), "demo accounts are exported");
        let account = &list.children[0];
        assert_eq!(account.role, Role::ListItem);
        assert!(account.name.as_deref().is_some_and(|name| !name.is_empty()));
        assert!(
            account.value.is_none(),
            "a TOTP credential is never a value"
        );
        let names: Vec<_> = account
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        for expected in ["Copy code for", "Edit", "Move", "Delete"] {
            assert!(
                names.iter().any(|name| name.starts_with(expected)),
                "{expected} is a named control of the row: {names:?}"
            );
        }
    }

    // Case `the_open_sheet_is_a_modal_dialog_with_named_fields`.
    {
        let mut app = framed(app_with_state(), 900, 800);
        app.open_add();
        settle(&mut app);
        let nodes = Framed::harness(&app).a11y_nodes();
        let named = |role: &str, name: &str| {
            nodes
                .iter()
                .any(|(_, node)| format!("{:?}", node.role()) == role && node.label() == Some(name))
        };
        assert!(named("Dialog", "Add account"));
        assert!(named("TextInput", "Issuer"));
        assert!(named("TextInput", "Account name"));
        assert!(named("TextInput", "Secret key"));
        assert!(named("Button", "Cancel"));
        assert!(named("Button", "Show"), "Advanced options");
        let tree = app.accessibility().expect("the root stays described");
        assert!(
            tree.state.hidden,
            "the list behind the sheet is not reachable while it is open"
        );
    }

    // Case `the_secret_field_is_never_read_out`.
    {
        let mut app = framed(app_with_state(), 900, 800);
        app.add_panel.open();
        app.add_panel
            .apply_uri("otpauth://totp/Acme:ada?secret=JBSWY3DPEHPK3PXP");
        app.structure_dirty = true;
        settle(&mut app);
        let nodes = Framed::harness(&app).a11y_nodes();
        let secret = nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Secret key"))
            .expect("secret field is named");
        assert!(
            secret
                .1
                .value()
                .is_none_or(|value| !value.contains("JBSWY")),
            "a secret key is not exported as the field's value"
        );
    }
}

/// The list stands in one readable column that tracks the window.
#[test]
fn the_list_stands_in_one_centred_readable_column() {
    // Case `a_wide_window_bounds_the_column_and_centres_it`.
    let mut app = framed(app_with_state(), 1920, 1080);
    let theme = Theme::dark();
    let gutter = f64::from(theme.tokens.spacing.s6);
    let width = f64::from(theme.tokens.metrics.panel_width_sheet) + 2.0 * gutter;
    let column = app.column;
    assert!((column.width() - width).abs() < 0.5);
    assert!(
        (column.center().x - app.rect.center().x).abs() < 0.5,
        "the column is centred in the window"
    );

    // Case `a_narrow_window_keeps_its_gutters_and_recovers_when_widened`.
    layout_at(&mut app, 560, 720);
    assert!((app.column.width() - (560.0 - 2.0 * gutter)).abs() < 0.5);
    assert!(app.column.x0 >= app.rect.x0 && app.column.x1 <= app.rect.x1);
    layout_at(&mut app, 1920, 1080);
    assert!((app.column.width() - width).abs() < 0.5);

    // Case `a_phone_width_still_shows_every_row_whole`.
    let mut app = framed(app_with_state(), 320, 640);
    assert!(app.column.width() > 200.0);
    let id = app.vault.ordered()[0].id;
    let bounds = app.list.account_bounds(id).expect("first row is on screen");
    assert!(bounds.x0 >= app.rect.x0 && bounds.x1 <= app.rect.x1);
    layout_at(&mut app, 320, 640);
}

/// Only the demo root seeds sample accounts, and a capture never reads the
/// real vault.
#[test]
fn demo_root_seeds_samples_but_live_root_starts_empty() {
    // Case `demo_root_seeds_sample_accounts_into_vault_and_doc`.
    {
        let (app, state) = app_with_state();
        assert!(app.vault.len() >= 3, "demo vault should populate");
        let in_doc = AuthDoc::list_accounts(&state.doc);
        assert_eq!(in_doc.len(), app.vault.len(), "vault mirrored to CRDT");
    }

    // Case `live_root_starts_empty_without_fabricating_accounts`.
    {
        // The live app must never auto-populate sample content; a fresh
        // install with no persisted vault shows an empty list. (This test
        // is only meaningful when no vault file exists, which is the case
        // in the hermetic build/CI environment.)
        let mut app = AuthenticatorApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        if crate::storage::vault_path().is_none_or(|p| !p.exists()) {
            assert_eq!(app.vault.len(), 0, "live app must not seed demo accounts");
        }
    }
}

/// Deletion, reordering and selection through the list's events.
#[test]
fn vault_events_delete_reorder_and_select_accounts() {
    // Case `delete_event_removes_account`.
    {
        let (mut app, mut state) = app_with_state();
        let before = app.vault.len();
        let id = app.vault.ordered()[0].id;
        app.apply_list_event(ListEvent::Delete(id), &mut state.doc);
        assert_eq!(app.vault.len(), before - 1);
        assert!(app.vault.get(id).is_none());
        assert_ne!(
            app.selected,
            Some(id),
            "the selection moves off a deleted row"
        );
        // The delete went through the action, so it left the shared document too.
        assert!(
            AuthDoc::list_accounts(&state.doc)
                .iter()
                .all(|a| a.id != id),
            "the delete action mutated the shared document"
        );
    }

    // Case `reorder_event_moves_account_down`.
    {
        let (mut app, mut state) = app_with_state();
        let first = app.vault.ordered()[0].id;
        let second = app.vault.ordered()[1].id;
        app.apply_list_event(ListEvent::MoveDown(first), &mut state.doc);
        assert_eq!(app.vault.ordered()[0].id, second);
        assert_eq!(app.vault.ordered()[1].id, first);
        // The reorder is reflected in the document (the source of truth).
        assert_eq!(AuthDoc::list_accounts(&state.doc)[0].id, second);
    }

    // Case `arrow_keys_move_the_selection_through_the_list`.
    {
        let mut app = framed(app_with_state(), 1000, 800);
        let ids: Vec<_> = app.vault.ordered().iter().map(|a| a.id).collect();
        assert_eq!(app.selected, Some(ids[0]));
        assert!(dispatch(
            &mut app,
            &named_press(NamedKey::ArrowDown),
            no_mods()
        ));
        assert_eq!(app.selected, Some(ids[1]));
        assert!(dispatch(
            &mut app,
            &named_press(NamedKey::ArrowUp),
            no_mods()
        ));
        assert_eq!(app.selected, Some(ids[0]));
        assert!(dispatch(
            &mut app,
            &named_press(NamedKey::ArrowUp),
            no_mods()
        ));
        assert_eq!(app.selected, Some(ids[0]), "the selection stops at the top");
    }

    // Case `edit_and_set_up_events_open_the_sheet_for_that_account`.
    {
        let (mut app, mut state) = app_with_state();
        let id = app.vault.ordered()[1].id;
        app.apply_list_event(ListEvent::Edit(id), &mut state.doc);
        assert!(app.add_panel.is_open());
        assert_eq!(app.selected, Some(id));
        assert_eq!(app.add_panel.field_snapshot().0, "Google");
    }
}

#[test]
fn hosted_account_lookup_returns_the_published_account() {
    let (mut app, state) = app_with_state();
    let id = app.vault.ordered()[0].id;
    let address = crate::actions::account_resource_id(id);
    let standalone =
        ui::resources::resource(&app, &state.doc, &address).expect("standalone account resolves");
    app.actions.mark_hosted_for_test();
    let hosted = ui::resources::resource(&app, &state.doc, &address)
        .expect("hosted account resolves from the document");
    assert_eq!(hosted.kind, "totp.account");
    assert_eq!(
        hosted
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        standalone
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        "a hosted account is the published row"
    );
    assert_eq!(
        hosted
            .presentation
            .as_ref()
            .and_then(|presentation| presentation.badge.as_deref()),
        standalone
            .presentation
            .as_ref()
            .and_then(|presentation| presentation.badge.as_deref()),
        "the local secret still marks the published account"
    );
}

/// Adding through the sheet and attaching a missing local secret to the same id.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn adding_and_attaching_accounts_grows_the_vault() {
    // Case `adding_an_account_through_the_sheet_grows_the_vault`.
    {
        let (mut app, mut state) = app_with_state();
        let before = app.vault.len();
        app.add_panel.open();
        // Fill via the paste seam, then run the production save path.
        app.add_panel
            .apply_uri("otpauth://totp/Proton:ada@pm.me?secret=JBSWY3DPEHPK3PXP");
        app.add_panel.save_for_test();
        assert!(!app.add_panel.is_open(), "a valid save closes the sheet");
        let ev = app.add_panel.take_event().expect("save emits an event");
        app.apply_add_panel_event(ev, &mut state.doc);
        assert_eq!(
            app.vault.len(),
            before + 1,
            "the account is added to the vault"
        );
        // The save broadcasts only metadata. The secret stays in the local,
        // encrypted vault even though the account remains collaborative.
        let in_doc = AuthDoc::list_accounts(&state.doc);
        assert_eq!(
            in_doc.len(),
            before + 1,
            "the add action mutated the shared document"
        );
        assert!(
            in_doc.iter().any(|a| a.issuer == "Proton"),
            "the added account is present in the document by issuer"
        );
        assert!(
            in_doc.iter().all(|account| account.secret.is_empty()),
            "the shared document never receives the setup secret"
        );
        // And the host has one undo boundary to record for it.
        assert!(
            ui::undo::take_committed_action(&mut app).is_some(),
            "the add action registered exactly one undo boundary for the host"
        );
    }

    // Case `missing_local_secret_exposes_honest_setup_then_attaches_to_the_same_id`.
    {
        use tilo_actions::Availability;

        let mut app = AuthenticatorApp::empty();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let remote = demo_accounts().remove(0);
        let id = remote.id;
        let secret = remote.secret.clone();
        AuthDoc::upsert_account(&mut state.doc, &remote);
        app.reload_from_doc(&state.doc);
        let mut framed = Framed::new(state, app, (900, 700), Theme::dark());
        let (app, state) = Framed::split(&mut framed);

        assert!(!app.has_local_secret(id));
        let resource =
            ui::resources::resource(app, &state.doc, &crate::actions::account_resource_id(id))
                .expect("the shared account resolves locally");
        let copy = resource
            .actions
            .iter()
            .find(|action| action.id == "copy-code")
            .expect("copy action");
        assert!(matches!(
            copy.contract.availability,
            Availability::Unavailable { ref code, .. } if code == "local_secret_required"
        ));
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == "set-up-local")
        );

        let encoded =
            serde_json::to_vec(&crate::actions::TotpOperation::SetUpLocal { id: id.to_string() })
                .unwrap();
        assert!(
            !encoded
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "the exact-account setup route carries an id, never a secret"
        );
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    OperationKind::View,
                    crate::actions::TOTP_OPERATION_TYPE,
                    &encoded
                ),
                &mut state.doc,
                &state.root
            )
            .map(|started| started.is_some())
            .unwrap()
        );
        assert!(app.add_panel.is_open(), "setup opens the local form");
        app.add_panel.apply_uri(
            "otpauth://totp/GitHub:ada@example.com?secret=JBSWY3DPEHPK3PXP&issuer=GitHub",
        );
        app.add_panel.save_for_test();
        let event = app.add_panel.take_event().expect("setup save event");
        assert!(matches!(event, AddPanelEvent::Provision(ref account) if account.id == id));
        app.apply_add_panel_event(event, &mut state.doc);
        assert!(app.has_local_secret(id));
        assert_eq!(
            app.vault.get(id).map(|account| account.secret.as_str()),
            Some(secret.as_str())
        );

        // A subsequent metadata projection keeps the provisioned local key,
        // while the CRDT remains deliberately credential-free.
        app.reload_from_doc(&state.doc);
        assert!(app.has_local_secret(id));
        let bytes = state.doc.save();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "setup never puts a bearer credential in collaboration/history"
        );
    }
}

/// Failed loads/saves keep the vault dirty and expose only bounded, user-triggered retries.
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn persistence_failures_keep_the_vault_dirty_with_bounded_retry() {
    // Case `failed_initial_load_cannot_be_cleared_by_saving_the_empty_view`.
    {
        let mut app = AuthenticatorApp::demo_persistence_error();
        app.vault_recovery_required = true;
        let message = app.persistence_message.clone();
        assert!(!app.persist_vault());
        assert!(!app.retry_persist());
        assert!(app.persistence_pending);
        assert_eq!(app.persistence_message, message);
    }

    // Case `failed_persistence_exposes_a_bounded_retry_effect`.
    {
        let mut app = AuthenticatorApp::demo_persistence_error();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let root = ui::resources::resources(&app, &state.doc).remove(0);
        let retry = root
            .actions
            .iter()
            .find(|action| action.id == "retry-save")
            .expect("unsaved local vault exposes one retry effect");
        assert_eq!(retry.kind, OperationKind::Effect);
        let encoded = match &retry.input {
            tilo_actions::ActionInput::Immediate(encoded) => encoded.clone(),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => {
                panic!("retry is immediate")
            }
        };
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    OperationKind::Effect,
                    &encoded.type_id,
                    &encoded.payload
                ),
                &mut state.doc,
                &state.root
            )
            .map(|started| started.is_some())
            .unwrap()
        );
        assert!(
            !app.persistence_pending,
            "retry is user-triggered, never a frame loop"
        );
    }

    // Case `persistence_banner_has_its_own_bounded_click_and_a11y_retry`.
    {
        let mut app = AuthenticatorApp::demo_persistence_error();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let mut framed = Framed::new(state, app, (390, 720), Theme::dark());
        let (app, state) = Framed::split(&mut framed);
        let selected = app.vault.ordered()[0].id;
        app.selected = Some(selected);
        assert!(
            app.toast.is_none(),
            "the durable banner replaces a duplicate toast"
        );

        assert!(app.retry_rect.x0 >= app.rect.x0);
        assert!(app.retry_rect.x1 <= app.rect.x1);
        assert!(
            app.banner_rect.union(app.retry_rect) == app.banner_rect
                || app.retry_rect.y0 >= app.banner_rect.y1,
            "Retry stands inside the banner's box, or under it, never beside it"
        );
        assert!(
            app.retry_rect.x1 <= app.banner_rect.x1,
            "Retry ends at or inside the banner's trailing edge"
        );
        assert!(app.retry_rect.y0 >= app.rect.y0);

        let tree = app
            .accessibility()
            .expect("persistence state has semantics");
        let status = tree
            .children
            .iter()
            .find(|node| node.role == Role::Toolbar)
            .expect("the vault status is described");
        let retry = status
            .children
            .iter()
            .find(|node| node.semantic_key.as_deref() == Some("totp.retry-persist"))
            .expect("retry is a separate semantic control");
        assert_eq!(
            retry.bounds.map(ui::a11y::AccessBounds::to_rect),
            Some(app.retry_rect)
        );
        assert!(retry.state.focusable);

        assert!(app.perform_semantic_action(
            "totp.retry-persist",
            &A11yAction::Activate,
            &mut state.doc,
            &state.root,
        ));
        assert!(!app.persistence_pending);
    }

    // Case `the_list_starts_below_the_banner`.
    {
        let mut app = framed(
            {
                let mut app = AuthenticatorApp::demo_persistence_error();
                let mut state = ui::State::new();
                app.sync(&mut state.doc, &state.root);
                (app, state)
            },
            1000,
            800,
        );
        let first = app.vault.ordered()[0].id;
        let row = app.list.account_bounds(first).expect("first row on screen");
        assert!(
            row.y0 >= app.banner_rect.y1 && row.y0 >= app.retry_rect.y1,
            "banner reserves its band above the list"
        );
        layout_at(&mut app, 1000, 800);
    }

    // Case `injected_save_failure_keeps_the_vault_dirty_and_advertises_retry`.
    {
        let mut app = AuthenticatorApp::demo();
        app.seed();
        app.seeded = true;
        // Avoid touching a user's live vault: the injected failure returns
        // before resolving a data directory or key custody.
        app.seed_mode = SeedMode::Live;
        crate::storage::force_save_failure_for_test(true);
        assert!(!app.persist_vault());
        crate::storage::force_save_failure_for_test(false);
        assert!(app.persistence_pending);

        let state = ui::State::new();
        let root = ui::resources::resources(&app, &state.doc).remove(0);
        assert!(root.actions.iter().any(|action| action.id == "retry-save"));
    }
}

/// QR import: host portal routing, frame drain, cancel and file decoding.
#[test]
fn qr_import_routes_through_the_host_portal_and_fills_the_sheet() {
    // Case `qr_cta_routes_to_the_host_portal`.
    // The primary "Scan a QR image" action must reach the **host** file
    // chooser (the XDG portal), not an in-app picker or a dead-end toast. A
    // recording host answers the request so no real dialog opens; the
    // in-flight guard keeps a second tap from stacking another dialog.
    {
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let (mut app, _state) = app_with_state();
        app.add_panel.open();
        app.request_qr_via_portal();
        assert!(
            app.portal_rx.is_some(),
            "the QR action routes through the host portal (not an in-app picker)"
        );
        app.request_qr_via_portal();
        let requests = recorder.pick_requests();
        assert_eq!(requests.len(), 1, "an in-flight request is not stacked");
        assert_eq!(requests[0].title(), "Choose a QR image");
    }

    // Case `portal_chosen_qr_image_fills_the_sheet_via_frame_drain`.
    // End-to-end proof of the import seam on the real per-frame drain: a QR
    // image the host portal returned is decoded and fills the sheet. A
    // recording host answers the request (no real dialog) and the next
    // `sync` loads it.
    {
        let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
        let dir = std::env::temp_dir().join(format!("totp-portal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("acme-qr.png");
        render_qr_png(uri, &img);

        let (mut app, mut state) = app_with_state();
        app.add_panel.open();

        // The host answers the real QR request and the per-frame sync drains
        // it, the exact channel the off-thread portal worker feeds.
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Opened(img));
        ui::host::scoped(&services, || app.request_qr_via_portal());
        app.sync(&mut state.doc, &state.root);

        assert!(
            app.portal_rx.is_none(),
            "the portal channel clears after delivering"
        );
        let (issuer, label, secret) = app.add_panel.field_snapshot();
        assert_eq!(issuer, "Acme");
        assert_eq!(label, "bob@acme.io");
        assert_eq!(secret, "JBSWY3DPEHPK3PXP");
        std::fs::remove_dir_all(&dir).ok();
    }

    // Case `portal_cancel_leaves_the_sheet_untouched`.
    // A cancelled portal interaction is a no-op: the sheet's fields are left
    // exactly as they were and the channel clears.
    {
        let (mut app, mut state) = app_with_state();
        app.add_panel.open();
        let before = app.add_panel.field_snapshot();
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(PortalOutcome::Cancelled);
        ui::host::scoped(&services, || app.request_qr_via_portal());
        app.sync(&mut state.doc, &state.root);
        assert!(
            app.portal_rx.is_none(),
            "the portal channel clears after a cancel"
        );
        assert_eq!(
            app.add_panel.field_snapshot(),
            before,
            "cancel changes nothing"
        );
    }

    // Case `qr_image_file_decodes_and_fills_the_sheet`.
    // End-to-end proof of the QR-image import seam: a real QR PNG on disk is
    // decoded and its otpauth payload fills the sheet.
    {
        let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
        let path = std::env::temp_dir().join(format!("totp-qr-{}.png", uuid::Uuid::new_v4()));
        render_qr_png(uri, &path);
        let decoded = crate::qr::decode_image_file(&path).expect("decode a real QR png file");
        let mut panel = AddAccountPanel::new();
        panel.open();
        panel.apply_uri(&decoded);
        let (issuer, label, secret) = panel.field_snapshot();
        assert_eq!(issuer, "Acme");
        assert_eq!(label, "bob@acme.io");
        assert_eq!(secret, "JBSWY3DPEHPK3PXP");
        std::fs::remove_file(&path).ok();
    }
}

/// The sheet through the window's real input path: clicks put the caret in a
/// field, typing fills it, Tab moves on, Enter saves, Escape cancels.
#[test]
fn the_sheet_takes_clicks_typing_tab_enter_and_escape() {
    let mut app = framed(app_with_state(), 900, 800);
    let before = app.vault.len();

    // Case `ctrl_n_opens_the_sheet`.
    assert!(!app.add_panel.is_open(), "starts on the list, sheet closed");
    assert!(
        dispatch(&mut app, &char_press("n"), ctrl()),
        "Ctrl+N is consumed"
    );
    assert!(app.add_panel.is_open(), "Ctrl+N opens the add sheet");
    settle(&mut app);

    // Case `a_click_puts_the_caret_in_the_field_under_it_and_typing_fills_it`.
    let fields: Vec<Rect> = {
        let targets = Framed::harness(&app).targets();
        let mut rects: Vec<(String, Rect)> = targets
            .into_iter()
            .filter(|(name, _)| name.ends_with("/TextInput"))
            .collect();
        rects.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
        rects.into_iter().map(|(_, rect)| rect).collect()
    };
    assert_eq!(fields.len(), 3, "issuer, account name and secret key");
    let harness = Framed::harness_mut(&mut app);
    harness.click_at(fields[0].center().x, fields[0].center().y);
    assert_eq!(
        app.add_panel.caret(),
        Some((3, Some(0))),
        "the click gave the issuer the caret"
    );
    let harness = Framed::harness_mut(&mut app);
    harness.type_text("Acme");
    assert_eq!(app.add_panel.field_snapshot().0, "Acme");

    // Case `tab_moves_the_caret_to_the_next_field`.
    let harness = Framed::harness_mut(&mut app);
    harness.key("Tab");
    harness.type_text("bob");
    assert_eq!(app.add_panel.field_snapshot().1, "bob");

    // Case `enter_with_no_secret_explains_instead_of_closing`.
    Framed::harness_mut(&mut app).key("Enter");
    app.drain_add_panel(&mut ui::State::new().doc);
    settle(&mut app);
    assert!(app.add_panel.is_open(), "an invalid form stays open");
    assert_eq!(
        app.add_panel.problem().as_deref(),
        Some("Enter a secret key"),
        "the problem is named where it is"
    );

    // Case `a_secret_and_enter_add_the_account`.
    let harness = Framed::harness_mut(&mut app);
    harness.key("Tab");
    harness.type_text("JBSWY3DPEHPK3PXP");
    assert_eq!(app.add_panel.field_snapshot().2, "JBSWY3DPEHPK3PXP");
    Framed::harness_mut(&mut app).key("Enter");
    settle(&mut app);
    assert!(!app.add_panel.is_open(), "a valid form closes the sheet");
    assert_eq!(app.vault.len(), before + 1, "the account was added");
    assert!(
        app.vault
            .ordered()
            .iter()
            .any(|a| a.issuer == "Acme" && a.label == "bob"),
        "with the typed details"
    );
    assert_eq!(
        app.toast.as_ref().map(|t| t.0.as_str()),
        Some("Account added")
    );

    // Case `escape_cancels_and_forgets_what_was_typed`.
    assert!(dispatch(&mut app, &char_press("n"), ctrl()));
    settle(&mut app);
    assert!(app.add_panel.is_open());
    assert!(
        app.add_panel.field_snapshot().0.is_empty(),
        "each visit starts from an empty form"
    );
    Framed::harness_mut(&mut app).key("Escape");
    settle(&mut app);
    assert!(!app.add_panel.is_open(), "Escape closes the sheet");
    assert_eq!(app.vault.len(), before + 1, "nothing was added");

    // Case `the_open_sheet_gates_every_list_shortcut`.
    assert!(dispatch(&mut app, &char_press("n"), ctrl()));
    settle(&mut app);
    let selected = app.selected;
    dispatch(&mut app, &named_press(NamedKey::ArrowDown), no_mods());
    assert_eq!(app.selected, selected, "the sheet owns the arrow keys");
    assert!(
        app.last_copied().is_none(),
        "and Enter does not copy behind it"
    );
}

/// The vault's add operation and the empty list's action open the sheet.
#[test]
fn the_add_operation_and_the_empty_action_open_the_sheet() {
    // Case `the_add_operation_the_header_button_is_bound_to_opens_the_sheet`.
    {
        let (mut app, mut state) = app_with_state();
        assert!(!app.add_panel.is_open());
        let payload = serde_json::to_vec(&crate::actions::TotpOperation::OpenAddAccount).unwrap();
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    OperationKind::View,
                    crate::actions::TOTP_OPERATION_TYPE,
                    &payload
                ),
                &mut state.doc,
                &state.root
            )
            .map(|started| started.is_some())
            .unwrap()
        );
        assert!(app.add_panel.is_open(), "the operation opens the sheet");
    }

    // Case `the_empty_state_action_opens_the_sheet`.
    {
        let mut app = AuthenticatorApp::empty();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let mut framed = Framed::new(state, app, (1000, 800), Theme::dark());
        assert!(framed.list.is_empty());
        let action = framed.list.empty_action_rect();
        assert!(action.area() > 0.0, "the empty state offers one action");
        Framed::harness_mut(&mut framed).click_at(action.center().x, action.center().y);
        assert!(framed.add_panel.is_open(), "its action opens the add sheet");
    }
}

/// Every capture scenario stages the state it names.
#[test]
fn every_capture_scenario_stages_its_state() {
    for scenario in [
        "list",
        "hover",
        "copied",
        "empty",
        "add",
        "add-advanced",
        "provision",
        "missing-secret",
        "persistence-error",
        "expiring",
    ] {
        let mut app = AuthenticatorApp::new();
        app.stage_scenario(Some(scenario));
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let mut framed = Framed::new(state, app, (1280, 800), Theme::dark());
        settle(&mut framed);
        assert_eq!(
            framed.seed_mode,
            if scenario == "empty" {
                SeedMode::Empty
            } else {
                SeedMode::Demo
            },
            "{scenario}: a capture never reads the real vault"
        );
        match scenario {
            "empty" => assert!(framed.list.is_empty(), "{scenario}"),
            "add" | "add-advanced" | "provision" => {
                assert!(framed.add_panel.is_open(), "{scenario}");
            }
            "missing-secret" => {
                let first = framed.vault.ordered()[0].id;
                assert!(!framed.has_local_secret(first), "{scenario}");
            }
            "persistence-error" => {
                assert!(framed.persistence_pending, "{scenario}");
                assert!(framed.banner_rect.area() > 0.0, "{scenario}");
            }
            "hover" => assert_eq!(framed.list.hovered_row(), Some(2), "{scenario}"),
            "copied" => {
                assert!(framed.last_copied().is_some(), "{scenario}");
                assert!(
                    framed
                        .toast
                        .as_ref()
                        .is_some_and(|(_, shown, _)| shown.elapsed() == Duration::ZERO),
                    "{scenario}: the toast waits for the capture"
                );
            }
            _ => assert_eq!(framed.list.len(), 5, "{scenario}"),
        }
        assert!(
            Framed::harness(&framed).accessibility_audit().is_clean(),
            "{scenario}: the screen is accessible"
        );
    }
}

/// The app as the framework runs it: the shell's header buttons, the staged
/// vault and the sheet, driven through the real host.
#[test]
fn the_hosted_app_opens_the_sheet_from_the_header_and_copies_from_the_keyboard() {
    use crate::resource::TotpShell;
    let mut hosted =
        ui::framework::testing::HeadlessApp::<TotpShell>::new().expect("the app mounts");
    hosted.stage(Some("list")).expect("the list stages");
    let harness = hosted.harness();
    harness.relayout();
    let names = |harness: &ui::headless::FrameHarness| -> Vec<(String, String)> {
        harness
            .a11y_nodes()
            .into_iter()
            .map(|(_, node)| {
                (
                    format!("{:?}", node.role()),
                    node.label().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    };

    // Case `the_staged_vault_is_listed_under_the_shell_header`.
    let listed = names(harness);
    assert!(
        listed
            .iter()
            .any(|(role, name)| role == "ListItem" && name.starts_with("GitHub")),
        "the demo accounts are listed: {listed:?}"
    );
    assert!(
        listed
            .iter()
            .any(|(role, name)| role == "Button" && name == "Add account"),
        "the header carries the add button"
    );

    // Case `the_header_button_opens_the_sheet`.
    harness
        .click_named("totp-add")
        .expect("the header button is a target");
    for _ in 0..3 {
        harness.relayout();
    }
    let opened = names(harness);
    assert!(
        opened
            .iter()
            .any(|(role, name)| role == "Dialog" && name == "Add account"),
        "pressing it opens the sheet: {opened:?}"
    );
    assert!(
        opened.iter().any(|(_, name)| name == "Secret key"),
        "with its fields"
    );

    // Case `a_click_in_a_field_of_the_hosted_sheet_puts_the_caret_there`.
    harness
        .click_named("totp-issuer")
        .expect("the issuer field is a target");
    harness.type_text("Acme");
    harness.tab(false);
    harness.type_text("bob");
    let typed: Vec<_> = harness
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| format!("{:?}", node.role()) == "TextInput")
        .map(|(_, node)| node.value().unwrap_or_default().to_owned())
        .collect();
    assert!(
        typed.iter().any(|value| value == "Acme") && typed.iter().any(|value| value == "bob"),
        "the click and Tab each put the caret in a field: {typed:?}"
    );

    // Case `escape_closes_the_sheet_and_enter_copies_the_selected_code`.
    harness.key("Escape");
    for _ in 0..3 {
        harness.relayout();
    }
    assert!(
        !names(harness)
            .iter()
            .any(|(role, name)| role == "Dialog" && name == "Add account"),
        "Escape closes it"
    );
    harness.key("Enter");
    harness.relayout();
    let copied = names(harness);
    assert!(
        copied.iter().any(|(_, name)| name == "Copied GitHub code"),
        "Enter copies the selected account and says so: {copied:?}"
    );
}

/// Rasterise `data` as a QR code into a greyscale PNG on disk, scaling
/// modules up with a quiet zone so the decoder has clean finder patterns.
fn render_qr_png(data: &str, path: &std::path::Path) {
    use image::{GrayImage, Luma};
    use qrcode::{EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).unwrap();
    let modules = code.to_colors();
    let dim = (modules.len() as f64).sqrt() as usize;
    let scale = 6usize;
    let quiet = 4usize;
    let side = ((dim + quiet * 2) * scale) as u32;
    let mut img = GrayImage::from_pixel(side, side, Luma([255]));
    for y in 0..dim {
        for x in 0..dim {
            if modules[y * dim + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = ((x + quiet) * scale + dx) as u32;
                        let py = ((y + quiet) * scale + dy) as u32;
                        img.put_pixel(px, py, Luma([0]));
                    }
                }
            }
        }
    }
    img.save(path).unwrap();
}

/// The capture steps leave every ring part-way round, and the expiring one
/// inside the warning window.
#[test]
fn the_capture_steps_show_the_countdown_part_way_and_nearly_out() {
    let account = &crate::model::demo_accounts()[0];
    let period = f64::from(account.period);
    let at = |elapsed: u64| 1_000.0 * period + elapsed as f64;
    let remaining = |elapsed| crate::totp::seconds_remaining(account, at(elapsed));
    let part_way = remaining(CAPTURE_ELAPSED);
    assert!(
        part_way > crate::list::EXPIRING_SECONDS && part_way < account.period,
        "the ordinary captures show a ring part-way round, not full and not warning"
    );
    assert!(
        remaining(CAPTURE_EXPIRING_ELAPSED) <= crate::list::EXPIRING_SECONDS,
        "the expiring capture is inside the warning window"
    );
}
