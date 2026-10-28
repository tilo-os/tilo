//! The row scenes the editor keeps between frames, through the frame it
//! actually draws.
//!
//! A kept row must paint exactly what a frame with nothing kept paints, so
//! every test compares the two: the same view drawn once with the rows it
//! kept and once with [`EditorView::forget_rows_kept`]. A change that the
//! cache does not witness shows up as a kept row painting last frame's
//! pixels, and a frame that reuses nothing shows up in the counts.

use super::super::Caret;
use super::*;
use crate::doc::EditorDoc;
use crate::lsp::{LspDiagnostic, Position, Range};
use crate::model::Language;
use automerge::AutoCommit;
use lsp_types::DiagnosticSeverity;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::Widget as _;
use vello::Scene;
use vello::kurbo::Affine;

/// An editor over `lines` short Rust lines in a 600 by 400 view.
fn view_of(doc: &mut AutoCommit, lines: usize) -> EditorView {
    let source = (0..lines).fold(String::new(), |mut source, line| {
        use std::fmt::Write as _;
        let _ = writeln!(source, "let value_{line} = {line};");
        source
    });
    let text = EditorDoc::ensure_buffer(doc, 0, &source);
    let mut view = EditorView::new();
    view.bind(text, Language::Rust);
    view.sync(doc, &automerge::ROOT);
    view.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
    view
}

/// One frame through the production draw.
fn painted(view: &mut EditorView, renderer: &mut TextRenderer, theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    view.draw(
        &mut scene,
        Affine::IDENTITY,
        renderer,
        theme,
        &PresenceCtx::empty(),
    );
    scene
}

/// The frame a view paints when it keeps no row at all.
fn repainted(view: &mut EditorView, renderer: &mut TextRenderer, theme: &Theme) -> Scene {
    view.forget_rows_kept();
    painted(view, renderer, theme)
}

/// The lines the frame has in view, as the draw counts them.
fn visible_lines(view: &EditorView) -> usize {
    let rows = view.visible_rows();
    (rows.first_line + rows.n_visible).min(view.lines.len()) - rows.first_line
}

fn assert_same_paint(kept: &Scene, fresh: &Scene, what: &str) {
    assert!(
        kept.encoding_eq(fresh),
        "{what}: a kept row must paint what a cold frame paints"
    );
}

#[test]
fn a_repeated_frame_reuses_the_rows_it_painted_and_paints_the_same() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let rows = visible_lines(&view);
    assert!(rows > 8, "the view shows a screen of lines: {rows}");

    let first = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts(),
        (0, rows),
        "with nothing kept, the first frame paints every row"
    );

    let second = painted(&mut view, &mut renderer, &theme);
    let (reused, painted_now) = view.rows_kept_counts();
    assert_eq!(
        painted_now, 1,
        "the primary row carries the caret and is painted fresh"
    );
    assert_eq!(reused, rows - 1, "every other row comes from the kept one");
    assert_same_paint(&second, &first, "a repeated frame");
    assert_eq!(
        view.rows_kept_lines(),
        (1..rows).collect::<Vec<_>>(),
        "the frame keeps every row in view but the primary one"
    );
}

#[test]
fn an_edit_keeps_the_rows_it_did_not_change() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let rows = visible_lines(&view);
    painted(&mut view, &mut renderer, &theme);

    // A line past the screen changes: no row in view paints differently.
    let object = view.text_obj.clone().expect("the view is bound");
    let offscreen = view.char_index(rows + 3, 0);
    EditorDoc::splice(&mut doc, &object, offscreen, 0, "// remote\n");
    doc.commit();
    view.reconcile_from_doc(&doc);
    let after = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts(),
        (rows - 1, 1),
        "an edit off the screen reuses every row in view but the primary one"
    );
    assert_same_paint(
        &after,
        &repainted(&mut view, &mut renderer, &theme),
        "an edit off the screen",
    );

    // A line in view changes: it is painted fresh, and its neighbours are
    // still reused.
    let in_view = view.char_index(rows / 2, 0);
    EditorDoc::splice(&mut doc, &object, in_view, 0, "let");
    doc.commit();
    view.reconcile_from_doc(&doc);
    let edited = painted(&mut view, &mut renderer, &theme);
    let (reused, painted_now) = view.rows_kept_counts();
    assert_eq!(
        painted_now, 2,
        "the edited row and the primary row are painted fresh"
    );
    assert_eq!(
        reused,
        rows - 2,
        "the rows the edit did not touch are reused"
    );
    assert_same_paint(
        &edited,
        &repainted(&mut view, &mut renderer, &theme),
        "an edit in view",
    );
}

#[test]
fn another_theme_or_text_scale_paints_every_row_fresh() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let mut renderer = TextRenderer::new();
    let dark = painted(&mut view, &mut renderer, &Theme::dark());

    let light = Theme::light();
    let relit = painted(&mut view, &mut renderer, &light);
    assert_eq!(
        view.rows_kept_counts().0,
        0,
        "a row painted in another theme is reused by none"
    );
    assert_same_paint(
        &relit,
        &repainted(&mut view, &mut renderer, &light),
        "another theme",
    );
    assert!(
        !relit.encoding_eq(&dark),
        "the theme does change what the rows paint"
    );

    renderer.set_text_scale(1.5);
    let scaled = painted(&mut view, &mut renderer, &light);
    assert_eq!(
        view.rows_kept_counts().0,
        0,
        "a row shaped at another text scale is reused by none"
    );
    assert_same_paint(
        &scaled,
        &repainted(&mut view, &mut renderer, &light),
        "another text scale",
    );
    assert!(
        view.rows_kept_lines().len() + 2 >= visible_lines(&view),
        "the rows are kept again at the new scale: {:?}",
        view.rows_kept_lines()
    );
}

#[test]
fn the_editors_own_text_zoom_paints_every_row_fresh() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    painted(&mut view, &mut renderer, &theme);
    assert!(view.rows_kept_counts().0 > 0, "the rows are kept");

    // The editor's text zoom flows through the theme it draws with, so a row
    // shaped at the old size must not be replayed at the new one.
    view.set_text_zoom(1.25);
    let zoomed = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts().0,
        0,
        "a row shaped at another editor zoom is reused by none"
    );
    assert_same_paint(
        &zoomed,
        &repainted(&mut view, &mut renderer, &theme),
        "the editor's own text zoom",
    );
    let repeated = painted(&mut view, &mut renderer, &theme);
    assert!(
        view.rows_kept_counts().0 > 0,
        "the rows are kept again at the new zoom"
    );
    assert_same_paint(&repeated, &zoomed, "a repeated frame at the new zoom");
}

#[test]
fn another_font_epoch_paints_every_row_fresh() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    painted(&mut view, &mut renderer, &theme);
    assert!(view.rows_kept_counts().0 > 0, "the rows are kept");

    // New fonts shape different glyphs; the rows are kept per font epoch,
    // so none of them may be reused.
    renderer.bump_generation();
    let after = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts().0,
        0,
        "a row shaped with other fonts is reused by none"
    );
    assert_same_paint(
        &after,
        &repainted(&mut view, &mut renderer, &theme),
        "another font epoch",
    );
}

#[test]
fn a_security_surface_does_not_replay_ordinary_letter_forms() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    painted(&mut view, &mut renderer, &theme);
    assert!(view.rows_kept_counts().0 > 0);
    let epoch = renderer.font_epoch();
    renderer.security_surface(|renderer| {
        assert!(renderer.distinguishes_letters());
        assert_eq!(epoch, renderer.font_epoch());
        let secure = painted(&mut view, renderer, &theme);
        assert_eq!(view.rows_kept_counts().0, 0);
        assert_same_paint(
            &secure,
            &repainted(&mut view, renderer, &theme),
            "security-surface letter forms",
        );
    });
    let ordinary = painted(&mut view, &mut renderer, &theme);
    assert_eq!(view.rows_kept_counts().0, 0);
    assert_same_paint(
        &ordinary,
        &repainted(&mut view, &mut renderer, &theme),
        "ordinary letter forms restored",
    );
}

#[test]
fn another_geometry_or_horizontal_scroll_paints_every_row_fresh() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    painted(&mut view, &mut renderer, &theme);
    assert!(view.rows_kept_counts().0 > 0, "the rows are kept");

    view.rect = Rect::new(0.0, 0.0, 420.0, 300.0);
    let resized = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts().0,
        0,
        "a row painted in another box is reused by none"
    );
    assert_same_paint(
        &resized,
        &repainted(&mut view, &mut renderer, &theme),
        "another editor box",
    );

    // The rows are kept where they stand in the buffer, so a scroll that
    // only moves them reuses them: one line down, only the row that came
    // into view is painted. The primary row is above the screen by then.
    let rows = visible_lines(&view);
    view.scroll_y = 1.0;
    let scrolled = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts(),
        (rows - 1, 1),
        "a scroll paints the row that came into view and reuses the rest"
    );
    assert_same_paint(
        &scrolled,
        &repainted(&mut view, &mut renderer, &theme),
        "a scroll",
    );

    view.scroll_x = 24.0;
    let sideways = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts().0,
        0,
        "a row painted at another horizontal scroll is reused by none"
    );
    assert_same_paint(
        &sideways,
        &repainted(&mut view, &mut renderer, &theme),
        "a horizontal scroll",
    );
}

#[test]
fn other_highlight_spans_over_the_same_text_paint_fresh() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let plain = painted(&mut view, &mut renderer, &theme);
    painted(&mut view, &mut renderer, &theme);

    // The same source, resolved to other spans: a rollback, a late
    // highlight or a re-parse must not show the previous paint.
    let line = 4;
    view.spans[line] = vec![ui::widget::code_editor::Span::new(
        0..3,
        ui::widget::code_editor::TokenKind::Keyword,
    )];
    view.dirty = true;
    let highlighted = painted(&mut view, &mut renderer, &theme);
    assert_eq!(
        view.rows_kept_counts().1,
        2,
        "the row whose spans changed is painted fresh beside the primary one"
    );
    assert_same_paint(
        &highlighted,
        &repainted(&mut view, &mut renderer, &theme),
        "other spans over the same text",
    );
    assert!(
        !highlighted.encoding_eq(&plain),
        "the spans do change what the row paints"
    );
}

#[test]
fn a_state_that_must_never_come_from_a_kept_row_is_painted_fresh() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let before = painted(&mut view, &mut renderer, &theme);

    // A diagnostic and a find match first: stepping to the current match
    // moves the caret, and the selection below stays where the test puts it.
    view.set_diagnostics(&[LspDiagnostic {
        range: Range {
            start: Position {
                line: 6,
                character: 0,
            },
            end: Position {
                line: 6,
                character: 3,
            },
        },
        severity: DiagnosticSeverity::ERROR,
        message: "declared but never used".to_owned(),
        source: Some("test".to_owned()),
    }]);
    view.set_find_matches(
        &[crate::find::Match {
            line: 7,
            col: 4,
            len: 5,
        }],
        Some(0),
    );
    // A selection across four rows, the primary one first.
    view.carets[0] = Caret {
        line: 0,
        col: 0,
        anchor: Some((3, 2)),
    };
    let primary = view.primary();
    assert_eq!(primary.line, 0, "the selection starts at the first row");
    assert!(
        view.scroll_y.abs() < 1e-9,
        "stepping to the match did not scroll the view: {}",
        view.scroll_y
    );
    let marked = painted(&mut view, &mut renderer, &theme);
    assert_same_paint(
        &marked,
        &repainted(&mut view, &mut renderer, &theme),
        "a selection, a diagnostic and a find match",
    );
    assert!(
        !marked.encoding_eq(&before),
        "the selection, diagnostic and match do change what the rows paint"
    );
    let kept = view.rows_kept_lines();
    for line in [0, 2, 3, 6, 7] {
        assert!(
            !kept.contains(&line),
            "row {line} moves with the person, so it keeps no scene: {kept:?}"
        );
    }

    // The input method's composition is drawn at the primary caret, whose
    // row is fresh for the same reason.
    let mut handled = false;
    view.handle_ime(
        &winit::event::Ime::Preedit("ka".to_owned(), Some((2, 0))),
        &mut handled,
    );
    assert!(handled, "the editor takes the composition");
    let composing = painted(&mut view, &mut renderer, &theme);
    assert_same_paint(
        &composing,
        &repainted(&mut view, &mut renderer, &theme),
        "a composition",
    );
    assert!(
        !view.rows_kept_lines().contains(&0),
        "the composing row keeps no scene"
    );
}

#[test]
fn the_kept_rows_are_bounded_to_the_rows_in_view() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 400);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let rows = visible_lines(&view);
    let mut seen = std::collections::BTreeSet::new();
    for step in 0..120 {
        view.scroll_y = f64::from(step) * 3.0;
        painted(&mut view, &mut renderer, &theme);
        let kept = view.rows_kept_lines();
        assert!(
            kept.len() <= rows,
            "a frame keeps the rows in view and no others: {} of {rows}",
            kept.len()
        );
        assert!(
            kept.len() <= ROWS_KEPT,
            "a frame keeps at most {ROWS_KEPT} rows: {}",
            kept.len()
        );
        let first = view.visible_rows().first_line;
        assert!(
            kept.iter()
                .all(|line| *line >= first && *line < first + rows),
            "every kept row is in view at scroll {}: {kept:?}",
            view.scroll_y
        );
        assert!(
            view.rows_kept_bytes() <= ROWS_KEPT_BYTES,
            "the kept paint stays within {ROWS_KEPT_BYTES} bytes: {}",
            view.rows_kept_bytes()
        );
        seen.extend(kept);
    }
    assert!(
        seen.len() > rows,
        "the scroll visited rows it did not have to hold at once"
    );
}

#[test]
fn a_long_line_is_painted_fresh_and_never_kept() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 40);
    let long = 3;
    let mut lines: Vec<String> = view.lines.iter().cloned().collect();
    lines[long] = "let very_long_name = 1;".repeat(400);
    let long_len = lines[long].len();
    assert!(long_len > ROW_KEPT_LINE, "the line is past {ROW_KEPT_LINE}");
    view.rebuild(&lines.join("\n"));

    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let first = painted(&mut view, &mut renderer, &theme);
    assert!(
        !view.rows_kept_lines().contains(&long),
        "a row of a {long_len}-byte line keeps no scene: {:?}",
        view.rows_kept_lines()
    );
    assert!(
        view.rows_kept_lines().len() + 2 >= visible_lines(&view),
        "the short rows around it are kept: {:?}",
        view.rows_kept_lines()
    );
    let second = painted(&mut view, &mut renderer, &theme);
    assert_same_paint(&second, &first, "a repeated frame with a long line");
    assert_same_paint(
        &second,
        &repainted(&mut view, &mut renderer, &theme),
        "a long line",
    );
}

#[test]
fn a_row_past_the_whole_line_clip_paints_when_a_scroll_brings_it_in() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    // A viewport that is not a whole number of rows: the last row the frame
    // counts lies past the bottom edge (and past the whole-line clip) and
    // paints nothing this frame.
    view.rect = Rect::new(0.0, 0.0, 600.0, 410.0);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    let rows = visible_lines(&view);
    assert!(
        view.lines.len() > rows,
        "the buffer runs past the screen: {} rows of {}",
        view.lines.len(),
        rows
    );
    assert!(
        (view.line_h * rows as f64) > view.rect.height(),
        "the last counted row stands past the bottom edge"
    );

    // A jump of several rows brings that row onto the screen: it must paint
    // itself, not replay the nothing it kept.
    view.scroll_y = 9.0;
    let jumped = painted(&mut view, &mut renderer, &theme);
    assert_same_paint(
        &jumped,
        &repainted(&mut view, &mut renderer, &theme),
        "a jump past several rows",
    );
}

#[test]
fn an_optimistic_edit_and_its_rollback_never_replay_a_row_from_the_other_text() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    painted(&mut view, &mut renderer, &theme);

    // The person types a line the app has not dispatched yet: the view
    // shows it, so the rows below it hold other lines than they did.
    view.carets = vec![Caret::at(4, 0)];
    view.edit_at_carets(|core, _| core.insert("// refused\n"));
    let optimistic = painted(&mut view, &mut renderer, &theme);
    assert_same_paint(
        &optimistic,
        &repainted(&mut view, &mut renderer, &theme),
        "an optimistic edit",
    );

    // The app refuses the edit: the view rolls back to the document, and no
    // row may replay what it painted of the refused text.
    view.reconcile_from_doc(&doc);
    let rolled = painted(&mut view, &mut renderer, &theme);
    assert_same_paint(
        &rolled,
        &repainted(&mut view, &mut renderer, &theme),
        "a rolled-back edit",
    );
    assert!(
        !rolled.encoding_eq(&optimistic),
        "the rollback takes the refused line off the screen"
    );
}

#[test]
fn moving_a_caret_within_unchanged_text_damages_only_its_stroke() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    view.is_focused = true;
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    view.mark_drawn();

    view.move_caret(1, 0, false);
    assert_eq!(view.primary().col, 1);
    let moved = painted(&mut view, &mut renderer, &theme);
    let mut regions = Vec::new();
    ui::scroll_copy::accepting(|| view.collect_damage(&mut regions));
    assert!(
        !regions.is_empty(),
        "the old stroke must be cleared and the new one drawn"
    );
    let area: f64 = regions.iter().map(Rect::area).sum();
    let old_row_band = view.rect.width() * 2.0 * view.line_h;
    assert!(
        area < old_row_band / 10.0,
        "caret damage {area} versus glyph band {old_row_band}: {regions:?}"
    );
    assert!(
        regions
            .iter()
            .all(|region| region.width() <= view.cell_w + 2.0)
    );
    assert_same_paint(
        &moved,
        &repainted(&mut view, &mut renderer, &theme),
        "caret movement",
    );
}

#[test]
fn typing_still_damages_the_original_conservative_glyph_band() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    view.is_focused = true;
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    painted(&mut view, &mut renderer, &theme);
    view.mark_drawn();
    view.edit_at_carets(|core, _| core.insert("x"));
    let typed = painted(&mut view, &mut renderer, &theme);
    let mut regions = Vec::new();
    ui::scroll_copy::accepting(|| view.collect_damage(&mut regions));
    assert!(
        regions.iter().any(|region| {
            region.x0.to_bits() == view.rect.x0.to_bits()
                && region.x1.to_bits() == view.rect.x1.to_bits()
        }),
        "glyphs retain their full horizontal reach: {regions:?}"
    );
    assert!(
        regions
            .iter()
            .any(|region| region.height() >= 2.0 * view.line_h),
        "the first row retains its clipped three-line reach: {regions:?}"
    );
    assert_same_paint(
        &typed,
        &repainted(&mut view, &mut renderer, &theme),
        "typing",
    );
}

#[test]
fn losing_focus_damages_the_last_caret_stroke_instead_of_leaving_it_behind() {
    let mut doc = AutoCommit::new();
    let mut view = view_of(&mut doc, 80);
    view.is_focused = true;
    let theme = Theme::dark();
    let mut renderer = TextRenderer::new();
    let focused = painted(&mut view, &mut renderer, &theme);
    view.mark_drawn();
    view.is_focused = false;
    view.dirty = true;
    let unfocused = painted(&mut view, &mut renderer, &theme);
    let mut regions = Vec::new();
    ui::scroll_copy::accepting(|| view.collect_damage(&mut regions));
    assert!(
        !regions.is_empty(),
        "the previous caret must be repainted away"
    );
    assert!(!focused.encoding_eq(&unfocused));
    assert_same_paint(
        &unfocused,
        &repainted(&mut view, &mut renderer, &theme),
        "lost focus",
    );
}
