use super::*;
use crate::model::demo_accounts;
use crate::test_events::{char_press, ctrl, named_press, no_mods};
use ui::headless::Framed;

/// The surface over the demo accounts, every key on this device, as the
/// framework presents them: for tests of layout, input and semantics, which
/// change nothing in the document.
fn app_with_state() -> (AuthenticatorApp, ui::State) {
    app_with_seeds(0)
}

/// As [`app_with_state`], without keys for the first `missing` accounts.
fn app_with_seeds(missing: usize) -> (AuthenticatorApp, ui::State) {
    let mut app = AuthenticatorApp::new();
    let mut seeds = Seeds::new(SecretStore::memory("so.tilo.totp"), true);
    for account in demo_accounts().iter().skip(missing) {
        seeds.put(account.id, &account.secret);
    }
    app.seeds = seeds;
    let metadata: Vec<Account> = demo_accounts()
        .into_iter()
        .map(|mut account| {
            account.secret.clear();
            account
        })
        .collect();
    app.present_accounts(&Rc::new(metadata));
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

/// Reduced motion wakes on second boundaries.
#[test]
fn reduced_motion_replaces_vsync_with_a_second_boundary_wake() {
    {
        let mut app = AuthenticatorApp::new();
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
}

/// Effect, accessibility, event and Enter copies share the row's clipboard
/// semantics; toasts distinguish failures.
#[test]
fn copy_paths_share_row_semantics_and_toast_severity() {
    // Case `the_copy_effect_invokes_the_same_clipboard_semantics_as_the_row`.
    {
        let (mut app, _state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        app.perform_effect(TotpEffect::CopyCode {
            account: id.to_string(),
        })
        .unwrap();
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
        let (mut app, _state) = app_with_state();
        let id = app.vault.ordered()[0].id;
        let acc = app.vault.get(id).cloned().unwrap();
        let before = totp::current_code(&acc);
        app.apply_list_event(ListEvent::Copy(id));
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
        let (mut app, _state) = app_with_seeds(1);
        let id = app.vault.ordered()[0].id;
        app.apply_list_event(ListEvent::Copy(id));
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
        assert!(named("Button", "Advanced options"));
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

/// Selection and the edit sheet through the list's events.
#[test]
fn list_events_select_accounts_and_open_the_sheet() {
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
        let (mut app, _state) = app_with_state();
        let id = app.vault.ordered()[1].id;
        app.apply_list_event(ListEvent::Edit(id));
        assert!(app.add_panel.is_open());
        assert_eq!(app.selected, Some(id));
        assert_eq!(app.add_panel.field_snapshot().0, "Google");
    }
}

/// A key the store refused keeps a bounded, user-triggered retry.
#[test]
fn persistence_failures_offer_a_bounded_retry() {
    // Case `persistence_banner_has_its_own_bounded_click_and_a11y_retry`.
    {
        let (mut app, state) = app_with_state();
        app.stage_scenario(Some("persistence-error"));
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
            .expect("the key status is described");
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
        assert!(
            !app.persistence_pending(),
            "retry is user-triggered, never a frame loop"
        );
    }

    // Case `the_list_starts_below_the_banner`.
    {
        let mut app = framed(
            {
                let (mut app, state) = app_with_state();
                app.stage_scenario(Some("persistence-error"));
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
    app.drain_add_panel();
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
    assert_eq!(app.vault.len(), before, "nothing was added");

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

/// The vault's add effect and the empty list's action open the sheet.
#[test]
fn the_add_effect_and_the_empty_action_open_the_sheet() {
    // Case `the_add_effect_the_header_button_is_bound_to_opens_the_sheet`.
    {
        let (mut app, _state) = app_with_state();
        assert!(!app.add_panel.is_open());
        app.perform_effect(TotpEffect::AddAccount).unwrap();
        assert!(app.add_panel.is_open(), "the effect opens the sheet");
    }

    // Case `the_empty_state_action_opens_the_sheet`.
    {
        let mut app = AuthenticatorApp::new();
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
    for scenario in crate::resource::SCENARIOS.iter().copied() {
        let (app, _state) = match scenario {
            "empty" => {
                let mut app = AuthenticatorApp::new();
                app.present_accounts(&Rc::default());
                (app, ())
            }
            "provision" | "missing-secret" => (app_with_seeds(1).0, ()),
            _ => (app_with_state().0, ()),
        };
        let mut app = app;
        app.stage_scenario(Some(scenario));
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let mut framed = Framed::new(state, app, (1280, 800), Theme::dark());
        settle(&mut framed);
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
                assert!(framed.persistence_pending(), "{scenario}");
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
