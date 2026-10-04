use super::*;
use crate::model::demo_accounts;
use crate::test_events::{char_press, ctrl, named_press, no_mods};
use ui::headless::Framed;

fn app_with_state() -> (AuthenticatorApp, ui::State) {
    app_with_seeds(0)
}

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
    (app, ui::State::new())
}

fn framed((app, state): (AuthenticatorApp, ui::State), w: u32, h: u32) -> Framed<AuthenticatorApp> {
    Framed::new(state, app, (w, h), Theme::dark())
}

fn layout_at(app: &mut Framed<AuthenticatorApp>, w: u32, h: u32) {
    Framed::harness_mut(app)
        .relayout_at((w, h))
        .expect("layout settles");
}

fn dispatch(
    app: &mut Framed<AuthenticatorApp>,
    ev: &WindowEvent,
    mods: winit::event::Modifiers,
) -> bool {
    Framed::harness_mut(app).dispatch_to_root_with(ev, &mods)
}

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

fn settle(app: &mut Framed<AuthenticatorApp>) {
    for _ in 0..3 {
        Framed::harness_mut(app).relayout();
    }
}

#[test]
fn reduced_motion_replaces_vsync_with_a_second_boundary_wake() {
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

#[test]
fn copy_paths_share_row_semantics_and_toast_severity() {
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
        // The copied string is the account's live TOTP code. It equals the
        // code at some instant during the copy, so it must match the value
        // just before or just after — deterministic even across a period
        // rollover.
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
            .expect("the first account is selected when shown");
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

    // Case `copy_without_a_local_secret_is_an_error_toast`.
    {
        let (mut app, _state) = app_with_seeds(demo_accounts().len());
        let id = app.vault.ordered()[0].id;
        app.apply_list_event(ListEvent::Copy(id));
        assert!(app.last_copied().is_none(), "no secret, no code");
        assert_eq!(app.toast.as_ref().map(|t| t.2), Some(ToastKind::Error));
    }
}

#[test]
fn qr_import_routes_through_the_host_portal_and_fills_the_panel() {
    // Case `qr_cta_routes_to_the_host_portal`.
    // The primary "Scan a QR image" action must reach the **host** file
    // chooser (the XDG portal), not an in-app picker or a dead-end toast. A
    // recording host answers the request so no real dialog opens; the
    // in-flight guard keeps a second tap from stacking another dialog.
    {
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let (mut app, _state) = app_with_state();
        app.rect = Rect::new(0.0, 0.0, 1000.0, 800.0);
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

    // Case `portal_chosen_qr_image_fills_the_add_panel_via_frame_drain`.
    {
        let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
        let dir = std::env::temp_dir().join(format!("totp-portal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("acme-qr.png");
        render_qr_png(uri, &img);

        let (mut app, mut state) = app_with_state();
        app.add_panel.open();

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

    // Case `portal_cancel_leaves_the_add_panel_untouched`.
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

    // Case `qr_image_file_decodes_and_fills_the_panel`.
    {
        let uri = "otpauth://totp/Acme:bob@acme.io?secret=JBSWY3DPEHPK3PXP&issuer=Acme";
        let path = std::env::temp_dir().join(format!("totp-qr-{}.png", uuid::Uuid::new_v4()));
        render_qr_png(uri, &path);
        let decoded = crate::qr::decode_image_file(&path).expect("decode a real QR png file");
        let mut panel = crate::add_panel::AddAccountPanel::new();
        panel.open();
        panel.apply_uri(&decoded);
        let (issuer, label, secret) = panel.field_snapshot();
        assert_eq!(issuer, "Acme");
        assert_eq!(label, "bob@acme.io");
        assert_eq!(secret, "JBSWY3DPEHPK3PXP");
        std::fs::remove_file(&path).ok();
    }
}

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

#[test]
fn missing_key_narrow_root_starts_at_top_with_an_accessible_first_row() {
    let app = framed(app_with_seeds(1), 390, 844);
    assert!(app.list.scroll_offset().abs() <= f64::EPSILON);
    let tree = app.accessibility().unwrap();
    let list = tree
        .children
        .iter()
        .find(|node| node.role == Role::List)
        .unwrap();
    assert!(
        list.children
            .first()
            .is_some_and(|row| row.bounds.is_some())
    );
}

#[test]
fn persistence_banner_has_its_own_bounded_retry() {
    let (mut app, state) = app_with_state();
    app.stage_scenario(Some("persistence-error"));
    let mut framed = Framed::new(state, app, (390, 720), Theme::dark());
    let (app, state) = Framed::split(&mut framed);
    assert!(app.toast.is_none());
    assert!(app.retry_rect.x0 >= app.rect.x0 && app.retry_rect.x1 <= app.rect.x1);
    assert!(app.retry_rect.y0 >= app.rect.y0 && app.retry_rect.y1 <= app.rect.y1);
    assert!(
        app.list
            .account_bounds(app.vault.ordered()[0].id)
            .unwrap()
            .y0
            >= app.banner_rect.y1
    );
    let tree = app.accessibility().unwrap();
    let retry = tree
        .children
        .iter()
        .find(|node| node.semantic_key.as_deref() == Some("totp.retry-persist"))
        .unwrap();
    assert_eq!(
        retry.bounds.map(ui::a11y::AccessBounds::to_rect),
        Some(app.retry_rect)
    );
    assert!(app.perform_semantic_action(
        "totp.retry-persist",
        &A11yAction::Activate,
        &mut state.doc,
        &state.root
    ));
    assert!(!app.persistence_pending());
}

#[test]
fn shortcuts_open_the_shared_sheet_and_gate_the_list() {
    let mut app = framed(app_with_state(), 900, 800);
    assert!(dispatch(&mut app, &char_press("n"), ctrl()));
    settle(&mut app);
    assert!(app.add_panel.is_open());
    let selected = app.selected;
    dispatch(&mut app, &named_press(NamedKey::ArrowDown), no_mods());
    assert_eq!(app.selected, selected);
    Framed::harness_mut(&mut app).key("Escape");
    settle(&mut app);
    assert!(!app.add_panel.is_open());
    assert!(dispatch(&mut app, &char_press("e"), ctrl()));
    settle(&mut app);
    assert!(app.add_panel.is_open());
}

#[test]
fn idle_frames_leave_the_document_unchanged() {
    let (mut app, mut state) = app_with_state();
    let before = state.doc.get_heads();
    app.sync(&mut state.doc, &state.root);
    app.sync(&mut state.doc, &state.root);
    assert_eq!(state.doc.get_heads(), before);
}
