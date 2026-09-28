include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
fn malformed_fixture(doc: &mut AutoCommit) {
    let themes = doc.get(ROOT, "themes").unwrap().unwrap().1;
    let row = doc
        .put_object(&themes, "broken", automerge::ObjType::Map)
        .unwrap();
    doc.put(&row, "unexpected_effect", "bad").unwrap();
}
fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    let mut s = crate::model::seed_custom();
    s.name = "Selected theme".into();
    s.accent = [12, 34, 56];
    crate::doc::save_catalog(&mut doc, &ROOT, &[s.clone()]);
    let selected = doc.get_heads()[0];
    crate::doc::set_name(&mut doc, &ROOT, &s.id, "Live theme");
    crate::doc::set_token(
        &mut doc,
        &ROOT,
        &s.id,
        crate::model::TokenId::Accent,
        [200, 100, 1],
    );
    doc.put(ROOT, "active", "saved-applied-identity").unwrap();
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "Selected theme".into())
}
fn verify_restored(doc: &AutoCommit) {
    let themes = crate::doc::load_catalog(doc, &ROOT).unwrap();
    let s = themes.iter().find(|s| s.id == "sunset").unwrap();
    assert_eq!(s.name, "Selected theme");
    assert_eq!(s.accent, [12, 34, 56]);
    assert_eq!(
        ui::doc::read_str(doc, &ROOT, "active").as_deref(),
        Some("saved-applied-identity")
    );
}

/// Edits, resets and deletes touch only the addressed record, with one undo.
#[test]
fn custom_theme_edits_touch_one_record_with_one_undo() {
    // Case `valid_edit_changes_only_addressed_record_and_invalid_edit_does_nothing`.
    {
        use tilo_actions::Action;
        let mut doc = AutoCommit::new();
        let one = crate::model::seed_custom();
        let mut two = one.clone();
        two.id = "second".into();
        two.name = "Second".into();
        crate::doc::save_catalog(&mut doc, &ROOT, &[one, two]);
        let before = doc.get_heads();
        for action in [
            crate::actions::set_custom_theme_name_action("sunset", ""),
            crate::actions::set_custom_theme_font_action("sunset", "unknown"),
            crate::actions::set_custom_theme_radius_action("sunset", f32::NAN),
        ] {
            action.apply(&mut doc).unwrap();
        }
        assert_eq!(doc.get_heads(), before);
        let action = crate::actions::set_custom_theme_name_action("sunset", "Explicit edit");
        tilo_actions::apply_local(&mut doc, &action, crate::actions::encode(&action)).unwrap();
        let catalog = crate::doc::load_catalog(&doc, &ROOT).unwrap();
        assert_eq!(
            catalog.iter().find(|s| s.id == "sunset").unwrap().name,
            "Explicit edit"
        );
        assert_eq!(
            catalog.iter().find(|s| s.id == "second").unwrap().name,
            "Second"
        );
        assert_eq!(doc.get_changes(&before).len(), 1);
    }

    // Case `reset_and_delete_keep_other_records_and_one_undo`.
    {
        for action in [
            crate::actions::reset_custom_theme_action("sunset"),
            crate::actions::delete_custom_theme_action("sunset"),
        ] {
            let mut state = ui::State::new();
            let mut one = crate::model::seed_custom();
            one.accent = [1, 2, 3];
            let mut two = one.clone();
            two.id = "second".into();
            crate::doc::save_catalog(&mut state.doc, &ROOT, &[one, two]);
            let before = state.doc.get_heads();
            tilo_actions::apply_local(&mut state.doc, &action, crate::actions::encode(&action))
                .unwrap();
            state.record_committed_action(before.clone(), None, None);
            assert_eq!(state.doc.get_changes(&before).len(), 1);
            let catalog = crate::doc::load_catalog(&state.doc, &ROOT).unwrap();
            assert!(catalog.iter().any(|s| s.id == "second"));
            match action {
                crate::actions::ThemesAction::DeleteCustomTheme { .. } => {
                    assert!(!catalog.iter().any(|s| s.id == "sunset"));
                    let heads = state.doc.base_heads();
                    assert!(!crate::doc::set_name(
                        &mut state.doc,
                        &ROOT,
                        "sunset",
                        "Resurrect"
                    ));
                    assert_eq!(state.doc.get_heads(), heads);
                }
                _ => assert_ne!(
                    catalog.iter().find(|s| s.id == "sunset").unwrap().accent,
                    [1, 2, 3]
                ),
            }
            assert!(state.undo(None, None).is_some());
            assert_eq!(
                crate::doc::load_catalog(&state.doc, &ROOT)
                    .unwrap()
                    .iter()
                    .find(|s| s.id == "sunset")
                    .unwrap()
                    .accent,
                [1, 2, 3]
            );
        }
    }
}

fn test_root() -> Box<dyn ui::widget::Widget> {
    crate::app_root()
}

#[test]
fn restore_actual_storage_reopen_never_applies_os_theme() {
    if let Ok(stage) = std::env::var("TILO_SLICE5_THEMES_STAGE") {
        let (mut state, mut saver) = ui::persist::try_open("so.tilo.themes").unwrap();
        let store = crate::store::store_path().unwrap();
        if stage == "write" {
            crate::store::save_to(&store, &crate::model::seed_custom()).unwrap();
            let store_bytes = std::fs::read(&store).unwrap();
            let mut spec = crate::model::seed_custom();
            spec.name = "Stored theme".into();
            crate::doc::save_catalog(&mut state.doc, &ROOT, &[spec.clone()]);
            state.doc.put(ROOT, "active", "preserved-applied").unwrap();
            let selected = state.doc.get_heads()[0];
            crate::doc::set_name(&mut state.doc, &ROOT, &spec.id, "Live theme");
            state.doc.get_heads();
            let mut d = managed_driver(state.doc, selected);
            d.click_named("history-restore").unwrap();
            d.key("Enter");
            d.click_named("history-back-live").unwrap();
            assert_eq!(std::fs::read(&store).unwrap(), store_bytes);
            saver.request(&mut d.state_mut().doc);
            saver.try_flush().unwrap();
            std::fs::write(
                saver.document_path().with_extension("selected"),
                selected.to_string(),
            )
            .unwrap();
        } else {
            let bytes = std::fs::read(&store).unwrap();
            let heads = state.doc.base_heads();
            crate::app_root().sync(&mut state.doc, &ROOT);
            assert_eq!(state.doc.base_heads(), heads);
            assert_eq!(state.doc.pending_ops(), 0);
            let selected =
                std::fs::read_to_string(saver.document_path().with_extension("selected"))
                    .unwrap()
                    .parse()
                    .unwrap();
            let target = state.doc.fork_at(&[selected]).unwrap();
            assert_eq!(
                project_content(&state.doc, &crate::framework_history::SCHEMA).unwrap(),
                project_content(&target, &crate::framework_history::SCHEMA).unwrap()
            );
            assert!(state.doc.get_change_by_hash(&selected).is_some());
            assert_eq!(std::fs::read(&store).unwrap(), bytes);
            assert_eq!(
                ui::doc::read_str(&state.doc, &ROOT, "active").as_deref(),
                Some("preserved-applied")
            );
        }
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("tilo-slice5-themes-restore-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for stage in ["write", "read"] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "framework_history_tests::restore_actual_storage_reopen_never_applies_os_theme",
                "--nocapture",
            ])
            .env("TILO_SLICE5_THEMES_STAGE", stage)
            .env("XDG_DATA_HOME", &dir)
            .env("XDG_CONFIG_HOME", &dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{stage}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("test result: ok. 1 passed"),
            "{stage}: the child ran no test: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// The frozen library panel is a raised surface on the history host's
/// ground; HIG §9.5 asks every such surface for a visible rest edge in dark
/// and light alike. Render the real preview over the host ground with the
/// offscreen renderer, then read the panel's outermost pixel column against
/// the fill one step inside it: the edge must move relative luminance by at
/// least the toolkit's hairline floor on both presets. The frames are written
/// under `target/ux1x/a3/themes/after/<theme>/history-panel.png` as the
/// visual evidence (the launcher has no `--shots` route into history mode).
#[test]
fn library_panel_paints_a_visible_derived_edge_in_dark_and_light() {
    use ui::a11y::relative_luminance;
    use ui::offscreen::OffscreenRenderer;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    let (doc, selected, _) = fixture();
    let (w, h) = (640_u32, 960_u32);
    let bounds = Rect::new(24.0, 24.0, f64::from(w) - 24.0, f64::from(h) - 24.0);
    for theme in [ui::Theme::dark(), ui::Theme::light()] {
        let mut panel = preview(&doc, selected, PreviewSide::After);
        let mut tr = ui::TextRenderer::new();
        panel.layout(bounds, &mut tr, &theme).unwrap();
        let mut scene = vello::Scene::new();
        // The host ground the panel sits on.
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.bg_app,
            None,
            &Rect::new(0.0, 0.0, f64::from(w), f64::from(h)),
        );
        panel.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme);

        let mut renderer = OffscreenRenderer::new(w, h);
        let rgba = renderer.render_rgba(&scene);
        let px = |x: f64, y: f64| -> Color {
            let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
            Color::new([
                f32::from(rgba[i]) / 255.0,
                f32::from(rgba[i + 1]) / 255.0,
                f32::from(rgba[i + 2]) / 255.0,
                1.0,
            ])
        };
        // Sample on the panel's left edge, halfway down: the outermost pixel
        // column carries the derived hairline; six pixels in is the panel's
        // own fill (the content is inset `s3` = 12 from the edge).
        let y = bounds.center().y;
        let edge = relative_luminance(px(bounds.x0, y));
        let fill = relative_luminance(px(bounds.x0 + 6.0, y));
        assert!(
            (edge - fill).abs() >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
            "{}: the library panel's edge must register against its fill \
             (edge {edge:.4} vs fill {fill:.4})",
            theme.name
        );
        let ground = relative_luminance(px(bounds.x0 - 6.0, y));
        assert!(
            (fill - ground).abs() >= ui::tokens::colors::HAIRLINE_MIN_DELTA
                || (edge - ground).abs() >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
            "{}: the panel must step off the host ground (fill {fill:.4}, edge {edge:.4}, ground {ground:.4})",
            theme.name
        );

        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/ux1x/a3/themes/after")
            .join(if theme.name.to_ascii_lowercase().contains("light") {
                "light"
            } else {
                "dark"
            })
            .join("history-panel.png");
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        renderer.render_png(&scene, theme.bg_app, &out);
    }
}
