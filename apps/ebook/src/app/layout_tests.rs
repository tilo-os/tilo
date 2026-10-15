//! Reader chrome and page geometry exercised through the production draw pass.

#[test]
fn selection_controls_dock_above_prose_without_reflowing_it() {
    for (scale, width) in [(1.0, 520.0), (1.5, 1440.0), (2.0, 2000.0)] {
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            let theme = theme.with_ui_scale(scale);
            let mut app = reader();
            draw_at(&mut app, &theme, width);
            let before = app.reader_page_geometry(app.rect, ReadMode::Paged);
            app.sel = Some(TextSel {
                anchor: DocPos { block: 1, ch: 0 },
                head: DocPos { block: 1, ch: 50 },
            });
            draw_at(&mut app, &theme, width);
            let after = app.reader_page_geometry(app.rect, ReadMode::Paged);
            assert_eq!(
                before.content_y, after.content_y,
                "selection cannot move text"
            );
            assert_eq!(
                before.content_h, after.content_h,
                "selection cannot repaginate"
            );
            let copy = hit(&app, |action| matches!(action, Action::CopySelection));
            let dock = app
                .hits
                .iter()
                .find(|hit| {
                    matches!(hit.action, Action::Swallow) && hit.rect.contains(copy.center())
                })
                .expect("selection dock is painted")
                .rect;
            contains(
                Rect::new(
                    after.center_x0,
                    after.paper_y,
                    after.center_x1,
                    after.content_y,
                ),
                dock,
            );
            let clearance = f64::from(theme.tokens.spacing.s2);
            assert!(dock.y1 + clearance <= after.content_y + 0.01);
            assert!((dock.height() - ui::SelectionHud::height(&theme)).abs() < 0.01);
            let mut count = 0;
            for swatch in app
                .hits
                .iter()
                .filter(|hit| matches!(hit.action, Action::SetColor(_)))
            {
                contains(dock, swatch.rect);
                assert!((swatch.rect.height() - ControlSize::Small.height(&theme)).abs() < 0.01);
                assert!((swatch.rect.width() - ControlSize::Small.height(&theme)).abs() < 0.01);
                count += 1;
            }
            assert_eq!(count, HiColor::ALL.len());
            contains(dock, hit(&app, |action| matches!(action, Action::AddNote)));
        }
    }
}

use super::*;

fn reader() -> BooksApp {
    BooksApp::with_library(
        crate::resource::library_dispatcher_for_test(),
        crate::content::load_sample_library(),
    )
}

fn draw_at(app: &mut BooksApp, theme: &Theme, width: f64) {
    app.rect = Rect::new(0.0, 0.0, width, 810.0);
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    let mut text = TextRenderer::with_font_catalog(&catalog).expect("checked-in font catalog");
    text.set_text_scale(theme.text_scale);
    app.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        theme,
        &ui::presence::PresenceCtx::empty(),
    );
}

fn hit(app: &BooksApp, matches: impl Fn(&Action) -> bool) -> Rect {
    app.hits
        .iter()
        .find(|hit| matches(&hit.action))
        .expect("reader control is painted")
        .rect
}

fn contains(outer: Rect, inner: Rect) {
    assert!(
        inner.x0 >= outer.x0 - 0.01
            && inner.y0 >= outer.y0 - 0.01
            && inner.x1 <= outer.x1 + 0.01
            && inner.y1 <= outer.y1 + 0.01,
        "{inner:?} escapes {outer:?}"
    );
}

#[test]
fn visible_page_arrows_take_precedence_over_the_progress_track() {
    let mut app = reader();
    let theme = Theme::dark().with_ui_scale(1.5);
    app.arm_reader_bubble_for_shots();
    draw_at(&mut app, &theme, 1440.0);
    assert!(app.cur_page_count > 1, "the fixture has a next page");
    let next = app
        .reader_bubble
        .action_center(ReaderBubbleAction::NextPage)
        .expect("the visible bubble has a next control");
    // Prove this is the overlapping geometry, not a click beside the track.
    assert!(hit(&app, |action| matches!(action, Action::SeekProgress)).contains(next));
    let before = (app.chapter, app.page);
    app.cursor = next;
    let mut doc = crate::resource::doc_with(&[], &[]);
    let mut handled = false;
    app.handle_left_press(
        Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut doc),
        &mut handled,
    );
    assert!(handled);
    assert_eq!((app.chapter, app.page), (before.0, before.1 + 1));
    assert!(!app.progress_dragging);
}

#[test]
fn reader_page_and_progress_follow_ui_scale() {
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let theme = theme.with_ui_scale(1.5);
        let mut app = reader();
        draw_at(&mut app, &theme, 1440.0);
        let page = app.reader_page_geometry(app.rect, ReadMode::Paged);
        assert!(app.toc_drawer.is_docked());
        assert!(app.notes_drawer.is_docked());
        assert!(page.framed);
        assert!((app.header_h - ui::chrome::HeaderRowFace::height(&theme)).abs() < 0.01);
        assert!((page.paper_y - page.body_y0 - f64::from(theme.tokens.spacing.s6)).abs() < 0.01);
        assert!(
            (page.full_content_x - page.paper_x - f64::from(theme.tokens.spacing.s12)).abs() < 0.01
        );
        assert!(page.header_y + app.running_header_h < page.rule_y);
        assert!(page.content_y > page.rule_y);
        let paper_bottom = page.paper_y + page.paper_h;
        let foot = Rect::new(
            page.center_x0,
            paper_bottom - page.slider_band,
            page.center_x1,
            paper_bottom,
        );
        assert!(f64::from(page.content_h) + page.content_y <= foot.y0 + 0.01);
        let progress = hit(&app, |action| matches!(action, Action::SeekProgress));
        contains(foot, progress);
        assert!(progress.height() >= f64::from(theme.tokens.metrics.min_pointer_target));
        assert!((app.progress_slider_track.x0 - page.full_content_x).abs() < 0.01);
        assert!(
            (app.progress_slider_track.x1 - page.full_content_x - f64::from(page.full_content_w))
                .abs()
                < 0.01
        );
        assert!(
            app.progress_fraction_at(app.progress_slider_track.origin())
                .abs()
                < 0.01
        );
        assert!(
            (app.progress_fraction_at(Point::new(
                app.progress_slider_track.x1,
                app.progress_slider_track.y0,
            )) - 1.0)
                .abs()
                < 0.01
        );
    }
}

#[test]
fn docked_inspector_owns_its_toggle_and_keeps_a_readable_page() {
    let theme = Theme::dark().with_ui_scale(1.5);
    let mut app = reader();
    app.shot_drawers = Some((true, true));
    draw_at(&mut app, &theme, 1440.0);
    assert!(app.toc_drawer.is_docked());
    assert!(app.notes_drawer.is_docked());
    let inspector = app.notes_panel_rect().expect("open inspector");
    let toggles: Vec<_> = app
        .hits
        .iter()
        .filter(|hit| matches!(hit.action, Action::ToggleNotes))
        .collect();
    assert_eq!(toggles.len(), 1, "no duplicate in the centre toolbar");
    contains(inspector, toggles[0].rect);
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    let mut text = TextRenderer::with_font_catalog(&catalog).expect("checked-in font catalog");
    let toc = app.toc_panel_rect().expect("open contents");
    let headings = [
        (
            "Contents",
            ui::chrome::HeaderRowFace::new(toc, &theme)
                .with_inset_title()
                .with_toggle(),
        ),
        (
            "Annotations",
            ui::chrome::HeaderRowFace::new(inspector, &theme)
                .with_inset_title()
                .with_trailing(),
        ),
    ];
    for (heading, face) in headings {
        let (width, height) = text.measure_style(heading, None, &theme.text(TextRole::Heading));
        assert!(
            f64::from(width) <= face.title.width(),
            "{heading} fits one line"
        );
        assert!(f64::from(height) <= face.row.height());
        assert!((face.row.y1 - app.header_h).abs() < 0.01);
    }
    let column = app.notes_drawer.content_rect();
    assert!(
        column.width() >= 720.0,
        "three panes retain a readable page"
    );
    let header = Rect::new(column.x0, 0.0, column.x1, app.header_h);
    let controls = [
        hit(&app, |action| matches!(action, Action::Back)),
        app.type_button,
        hit(&app, |action| matches!(action, Action::ToggleBookmark)),
    ];
    for control in controls {
        contains(header, control);
    }
    let gap = f64::from(theme.tokens.spacing.rhythm.related);
    assert!(controls[0].x1 + gap <= controls[1].x0);
    assert!(controls[1].x1 + gap <= controls[2].x0);

    // Below the combined rail and page minimum, the inspector floats
    // rather than squeezing the reading column into a sliver.
    app.shot_drawers = Some((true, true));
    draw_at(&mut app, &theme, 1400.0);
    assert!(app.notes_drawer.is_overlay());
    assert!(app.notes_drawer.content_rect().width() >= 720.0);
}

#[test]
fn page_framing_recomputes_after_ui_scale_changes() {
    let mut app = reader();
    draw_at(&mut app, &Theme::dark(), 1200.0);
    assert!(app.reader_page_geometry(app.rect, ReadMode::Paged).framed);
    draw_at(&mut app, &Theme::dark().with_ui_scale(1.5), 1200.0);
    let page = app.reader_page_geometry(app.rect, ReadMode::Paged);
    assert!(!page.framed, "compact at the current UI scale");
    assert!((page.paper_y - app.header_h).abs() < 0.01);
    draw_at(&mut app, &Theme::dark(), 1200.0);
    assert!(app.reader_page_geometry(app.rect, ReadMode::Paged).framed);
}

#[test]
fn highlights_and_collaborator_note_fit_the_scaled_inspector() {
    let theme = Theme::dark().with_ui_scale(1.5);
    let mut app = reader();
    let book = app.library[0].id.clone();
    let highlight = Annotation::new(&book, AnnoKind::Highlight, 0, 1)
        .with_excerpt("The most honest object in any house is the light switch.");
    let shared = Annotation::new(&book, AnnoKind::Highlight, 0, 2)
        .with_excerpt("Consider what a switch does not do.");
    let mut note = Annotation::new(&book, AnnoKind::Note, 0, 2)
        .with_excerpt("Consider what a switch does not do.");
    note.note = "Ada: A small promise, kept. Worth remembering.".into();
    app.annotations = vec![highlight, shared, note];
    app.shot_drawers = Some((true, true));
    draw_at(&mut app, &theme, 1440.0);
    assert!(app.notes_drawer.is_docked());
    assert_eq!(
        app.notes_max_scroll, 0.0,
        "all three complete cards fit at 1.5×"
    );
    assert_eq!(
        app.hits
            .iter()
            .filter(|hit| matches!(hit.action, Action::JumpTo(_)))
            .count(),
        3
    );
}

#[test]
fn annotation_delete_targets_stay_inside_the_scrolled_list() {
    let theme = Theme::light().with_ui_scale(1.5);
    let mut app = reader();
    let book = app.library[0].id.clone();
    app.annotations = (0..12)
        .map(|block| {
            Annotation::new(&book, AnnoKind::Note, 0, block)
                .with_excerpt("A passage long enough to wrap within the annotation card.")
        })
        .collect();
    app.shot_drawers = Some((true, true));
    draw_at(&mut app, &theme, 1800.0);
    assert!(app.notes_max_scroll > 0.0);
    let first = hit(&app, |action| matches!(action, Action::JumpTo(_)));
    // Cut through the first delete control rather than scrolling a whole
    // card away, so its expanded pointer target straddles the list edge.
    app.notes_scroll = f64::from(theme.tokens.spacing.s6);
    draw_at(&mut app, &theme, 1800.0);
    let inspector = app.notes_panel_rect().expect("open inspector");
    let list = Rect::new(
        inspector.x0,
        first.y0,
        inspector.x1,
        inspector.y1 - f64::from(theme.tokens.spacing.s2),
    );
    let deletes: Vec<_> = app
        .hits
        .iter()
        .filter(|hit| matches!(hit.action, Action::Delete(_)))
        .collect();
    assert!(!deletes.is_empty());
    for delete in deletes {
        contains(list, delete.rect);
    }
}
