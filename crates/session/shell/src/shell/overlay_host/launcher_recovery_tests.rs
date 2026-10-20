use super::*;
use std::cell::Cell;

struct RecoveryProvider {
    attempts: Rc<Cell<usize>>,
    failing: Rc<Cell<bool>>,
}

impl crate::shell::search::SearchProvider for RecoveryProvider {
    fn id(&self) -> &'static str {
        "launcher-recovery-fixture"
    }
    fn name(&self) -> &'static str {
        "Applications"
    }
    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }
    fn query(
        &self,
        context: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        if !context.needle.is_empty() && !"recovery fixture".contains(context.needle) {
            return Vec::new();
        }
        let attempts = self.attempts.clone();
        let failing = self.failing.clone();
        vec![crate::shell::search::SearchResult::nullary_fallible(
            crate::shell::search::ResourceKind::Application,
            self.id(),
            "recovery",
            "Open Recovery Fixture",
            Some("A local fixture application".into()),
            100,
            move |_, _, _| {
                attempts.set(
                    attempts
                        .get()
                        .checked_add(1)
                        .expect("bounded test attempts"),
                );
                if failing.get() {
                    Err(crate::shell::search::action_api::ActionError::Explained(
                        "The application is temporarily unavailable. Try again.".into(),
                    ))
                } else {
                    Ok(())
                }
            },
        )]
    }
}

fn recovery_fixture() -> (AbsorbHarness, Rc<Cell<usize>>, Rc<Cell<bool>>) {
    let mut h = build_harness_sized(1280.0, 800.0);
    let attempts = Rc::new(Cell::new(0));
    let failing = Rc::new(Cell::new(true));
    h.host.search_registry.register(Rc::new(RecoveryProvider {
        attempts: attempts.clone(),
        failing: failing.clone(),
    }));
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("recovery fixture");
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    let rows = h.host.palette.visible_entries();
    let index = rows
        .iter()
        .position(|entry| entry.label == "Open Recovery Fixture")
        .expect("production registry must supply the fixture");
    h.host.palette.set_cursor(index);
    (h, attempts, failing)
}

fn assert_recovery_context(h: &AbsorbHarness) {
    assert!(
        h.host.palette_visible(),
        "failed invocation must keep search open"
    );
    assert!(h.host.palette.is_focused(), "search retains keyboard focus");
    assert_eq!(h.host.palette.query(), "recovery fixture");
    let selected = h
        .host
        .palette
        .visible_entries()
        .get(h.host.palette.cursor())
        .copied()
        .unwrap();
    assert_eq!(selected.label, "Open Recovery Fixture");
    assert_eq!(selected.badge.as_deref(), Some("Couldn't run"));
    let semantics = h.host.palette.accessibility().expect("palette semantics");
    let row = semantics
        .children
        .get(1)
        .unwrap()
        .children
        .get(h.host.palette.cursor())
        .unwrap();
    assert!(
        row.description
            .as_deref()
            .unwrap()
            .contains("temporarily unavailable")
    );
}

#[test]
fn failed_palette_keyboard_invocation_keeps_context_for_retry_and_escape() {
    let (mut h, attempts, failing) = recovery_fixture();
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert_eq!(attempts.get(), 1);
    assert_recovery_context(&h);
    let mut held_enter = key_pressed(Key::Named(NamedKey::Enter));
    if let WindowEvent::KeyboardInput { event, .. } = &mut held_enter {
        event.repeat = true;
    }
    assert!(pump(&mut h, &held_enter));
    assert_eq!(
        attempts.get(),
        1,
        "held Enter must not flood failed activations"
    );
    assert_recovery_context(&h);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert_eq!(attempts.get(), 2);
    assert_recovery_context(&h);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(
        h.host.palette_visible(),
        "the first Escape clears the query, keeping search open"
    );
    assert_eq!(h.host.palette_query(), "");
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.host.palette_visible());
    assert_eq!(attempts.get(), 2, "cancel must not retry");
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.set_palette_query("recovery fixture");
    h.host.last_search_query.clear();
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    failing.set(false);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert_eq!(attempts.get(), 3);
    assert!(!h.host.palette_visible(), "successful retry closes search");
}

#[test]
fn failed_palette_pointer_invocation_keeps_context_across_resize_and_edit() {
    let (mut h, attempts, failing) = recovery_fixture();
    paint_then_click(&mut h, |host| {
        host.palette
            .row_rect(host.palette.cursor())
            .expect("selected row has a real painted target")
            .center()
    });
    assert_eq!(attempts.get(), 1);
    assert_recovery_context(&h);
    settle(&mut h, 800.0, 600.0);
    assert_recovery_context(&h);
    // A query edit routes through the shared input and rebuilds provider rows,
    // replacing old feedback rather than carrying a failure to another result.
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Backspace))));
    assert_eq!(h.host.palette.query(), "recovery fixtur");
    assert!(
        h.host
            .palette
            .visible_entries()
            .iter()
            .all(|entry| entry.badge.is_none())
    );
    failing.set(false);
    paint_then_click(&mut h, |host| {
        host.palette
            .row_rect(host.palette.cursor())
            .expect("resized result is clickable")
            .center()
    });
    assert_eq!(attempts.get(), 2);
    assert!(!h.host.palette_visible());
    assert!(
        h.log.borrow().is_empty(),
        "palette input must not reach app content"
    );
}

#[test]
fn palette_recovery_production_render_matrix() {
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/launcher-recovery-review");
    std::fs::create_dir_all(&output).expect("capture output");
    let mut visible_variants = Vec::new();
    for (name, width, height, theme) in vec![
        ("dark", 1280, 800, Theme::dark()),
        ("light", 1280, 800, Theme::light()),
        ("high-contrast", 1280, 800, Theme::high_contrast()),
        ("large-text", 800, 700, Theme::dark().with_text_scale(2.0)),
        (
            "ui-text-scaled",
            1280,
            800,
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
        ),
        ("narrow", 480, 600, Theme::dark()),
    ] {
        let (mut h, attempts, _) = recovery_fixture();
        settle_with_theme(&mut h, f64::from(width), f64::from(height), &theme);
        assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
        assert_eq!(
            attempts.get(),
            1,
            "capture must invoke the production route"
        );
        h.host.force_palette_settled();
        for _ in 0..4 {
            h.host.take_structure_dirty();
            settle_with_theme(&mut h, f64::from(width), f64::from(height), &theme);
            if !h.host.structure_dirty() {
                break;
            }
        }
        assert!(!h.host.structure_dirty(), "capture layout must converge");
        visible_variants.push((name, h.host.palette_visible()));
        let mut selected_bounds = None;
        if h.host.palette_visible() {
            let semantics = h
                .host
                .palette
                .accessibility()
                .expect("visible palette semantics");
            let list = semantics.children.get(1).expect("visible result list");
            let row = list
                .children
                .iter()
                .find(|row| row.name.as_deref() == Some("Open Recovery Fixture"))
                .expect("the failed selected row must remain visibly reachable");
            let bounds = row.bounds.expect("selected row bounds");
            selected_bounds = Some(bounds);
            let list_bounds = list.bounds.expect("painted list bounds");
            assert!(
                bounds.y0 >= list_bounds.y0 && bounds.y1 <= list_bounds.y1,
                "failed result must fit the actual list area at {name}: {bounds:?} in {list_bounds:?}"
            );
        }
        let scene = paint_scene(&mut h, &theme);
        let path = output.join(format!("{name}.png"));
        let mut renderer = offscreen::OffscreenRenderer::new(width, height);
        renderer.render_png(&scene, vello::peniko::Color::BLACK, &path);
        assert!(std::fs::metadata(&path).expect("capture exists").len() > 1000);
        if let Some(bounds) = selected_bounds {
            assert_rendered_title_contrast(
                &path,
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            );
        }
    }
    assert_eq!(visible_variants.len(), 6);
    assert!(
        visible_variants.iter().all(|(_, visible)| *visible),
        "failed activation must leave every captured palette open: {visible_variants:?}"
    );
}

// Observe the actual painted title, not the ink the material helper promises.
// The upper half of this two-line fixture row excludes its accessible detail,
// leading icon and trailing badge, so those cannot mask unreadable title ink.
fn assert_rendered_title_contrast(path: &std::path::Path, row: Rect) {
    let pixels = image::open(path).expect("read rendered capture").to_rgb8();
    let coordinate = |value: f64| ui::num::u32_of_usize(ui::num::usize_of_f64(value));
    let sample = pixels.get_pixel(coordinate(row.x0 + 6.0), coordinate(row.center().y));
    let ground = vello::peniko::Color::from_rgb8(sample[0], sample[1], sample[2]);
    let mut legible_pixels = 0usize;
    for y in coordinate(row.y0 + 2.0)..coordinate(row.center().y) {
        for x in coordinate(row.x0 + 40.0)..coordinate(row.x0 + 200.0) {
            let pixel = pixels.get_pixel(x, y);
            let ink = vello::peniko::Color::from_rgb8(pixel[0], pixel[1], pixel[2]);
            if ui::a11y::contrast_ratio(ink, ground) >= ui::tokens::colors::ON_FILL_MIN_CONTRAST {
                legible_pixels = legible_pixels
                    .checked_add(1)
                    .expect("bounded capture pixels");
            }
        }
    }
    assert!(
        legible_pixels >= 5,
        "painted selected title must clear the shared text floor: {path:?}, {legible_pixels} legible pixels"
    );
}

struct OwnedNoticeProvider {
    progress: bool,
}

impl crate::shell::search::SearchProvider for OwnedNoticeProvider {
    fn id(&self) -> &'static str {
        "owned-notice-fixture"
    }
    fn name(&self) -> &'static str {
        "Applications"
    }
    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }
    fn query(
        &self,
        _: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        let progress = self.progress;
        vec![crate::shell::search::SearchResult::nullary_fallible(
            crate::shell::search::ResourceKind::Application,
            self.id(),
            "owned-notice",
            "Open Notice Fixture",
            None,
            100,
            move |_, _, chrome| {
                if progress {
                    chrome
                        .borrow_mut()
                        .push_notification("Opening Notice Fixture", "Preparing…");
                } else {
                    chrome.borrow_mut().push_error_notification(
                        "Couldn't open Notice Fixture",
                        "The application is unavailable. Open Applications and try again.",
                    );
                }
                Err(crate::shell::search::action_api::ActionError::Refused(
                    "The application is unavailable; backend detail is for diagnostics".into(),
                ))
            },
        )]
    }
}

#[test]
fn palette_reuses_callback_owned_recovery_notice_without_a_duplicate() {
    let mut h = build_harness_sized(1280.0, 800.0);
    h.host
        .search_registry
        .register(Rc::new(OwnedNoticeProvider { progress: false }));
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("notice fixture");
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert!(h.host.palette_visible());
    let chrome = h.chrome.borrow();
    let notices = chrome.notifications();
    assert_eq!(
        notices.len(),
        1,
        "one refusal has one callback-owned recovery card"
    );
    let entry = h.host.palette.visible_entries().first().copied().unwrap();
    assert_eq!(
        entry.description.as_deref(),
        Some(notices.first().unwrap().body.as_str())
    );
    assert!(
        !entry
            .description
            .as_deref()
            .unwrap()
            .contains("backend detail")
    );
}

#[test]
fn palette_progress_notice_does_not_mask_a_failed_activation() {
    let mut h = build_harness_sized(1280.0, 800.0);
    h.host
        .search_registry
        .register(Rc::new(OwnedNoticeProvider { progress: true }));
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("notice fixture");
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert!(h.host.palette_visible());
    let entry = h.host.palette.visible_entries().first().copied().unwrap();
    // A refusal's diagnostic detail is for logs and agents; the row says
    // what the person can do.
    assert_eq!(
        entry.description.as_deref(),
        Some(crate::shell::i18n::t("host-action-try-again").as_str())
    );
    assert!(!entry.description.as_deref().unwrap().contains("Preparing"));
    // The failure is told once, on the row that failed (above): the
    // progress card stays the only notice, and never stands in for it.
    assert_eq!(
        h.chrome.borrow().notifications().len(),
        1,
        "the palette posts no second notice for a failure its row tells"
    );
}
