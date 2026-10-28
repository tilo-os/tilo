use super::*;
use crate::model::CellValue;

#[test]
fn formula_reference_colours_follow_the_theme() {
    let dark = Theme::dark();
    let other = Theme::light();
    let first = reference_color(&dark, 0);
    let second = reference_color(&dark, 1);
    assert_ne!(first, second, "consecutive references stay distinct");
    let wrap = ChartPalette::from_theme(&dark).len();
    assert_eq!(
        reference_color(&dark, wrap),
        first,
        "the series wraps with the theme palette"
    );
    assert_ne!(
        first,
        reference_color(&other, 0),
        "the first slot is grown from the theme, not a fixed hue"
    );
}

#[test]
fn scrollbars_are_thin_and_only_reserve_space_for_overflow() {
    let mut tr = TextRenderer::new();
    for scale in [1.0, 1.5, 2.0] {
        let theme = Theme::dark().with_ui_scale(scale);
        let mut grid = grid_with(&[]);
        grid.set_metrics(GridMetrics::resolve(&mut tr, &theme));
        grid.rect = Rect::new(0.0, 0.0, 260.0, 180.0);
        let body = grid.body_rect();
        let horizontal = grid.scrollbar_thumb(ScrollAxis::Horizontal).unwrap();
        let vertical = grid.scrollbar_thumb(ScrollAxis::Vertical).unwrap();
        assert!((5.0..=6.0).contains(&horizontal.height()));
        assert!((5.0..=6.0).contains(&vertical.width()));
        assert!(horizontal.y0 >= body.y1 && vertical.x0 >= body.x1);
        grid.rect = Rect::new(
            0.0,
            0.0,
            grid.total_width() + grid.metrics.row_header_w() + 1.0,
            grid.total_height() + grid.metrics.col_header_h() + 1.0,
        );
        assert!(grid.scrollbar_thumb(ScrollAxis::Horizontal).is_none());
        assert!(grid.scrollbar_thumb(ScrollAxis::Vertical).is_none());
        assert_eq!(grid.body_rect().x1, grid.rect.x1);
        assert_eq!(grid.body_rect().y1, grid.rect.y1);
        // Only the vertical axis overflows: no horizontal gutter.
        grid.rect.y1 = 180.0;
        grid.rect.x1 += grid.metrics.gutter;
        assert!(grid.scrollbar_thumb(ScrollAxis::Horizontal).is_none());
        assert!(grid.scrollbar_thumb(ScrollAxis::Vertical).is_some());
        assert_eq!(grid.body_rect().y1, grid.rect.y1);
    }
}

/// Measured metrics and draggable scrollbar thumbs.
#[test]
fn grid_metrics_and_scrollbars_follow_the_renderer() {
    // Case `measured_metrics_drive_automatic_geometry_but_never_manual_sizes`.
    // Metrics resolved against a real renderer at 1× reproduce the classic
    // density; at 2× the automatic row, header height and row-header width
    // follow the measured text, the prefix sums are rebuilt from them, and a
    // user's manual row height survives untouched.
    {
        let mut tr = TextRenderer::new();
        let one_x = GridMetrics::resolve(&mut tr, &Theme::dark());
        let control = ControlSize::Small.height(&Theme::dark());
        assert!(
            one_x.default_row_h() >= control && one_x.default_row_h() < control + 2.0,
            "1x row is the small control height or the cell line plus s2"
        );
        assert!(
            (one_x.col_header_h() - control).abs() < 1.0,
            "1x header is the small control height"
        );
        assert!(
            (one_x.row_header_w() - (one_x.s12 + one_x.s2)).abs() < 0.01,
            "1x row header is s12 + s2"
        );

        let mut g = Grid::new();
        g.row_heights.insert(3, 40.0);
        g.rebuild_geom();
        let one_x_top_of_row_2 = g.row_top(2);

        let theme = Theme::dark().with_text_scale(2.0);
        tr.set_text_scale(2.0);
        let two_x = GridMetrics::resolve(&mut tr, &theme);
        // The role scales once, so the measured line box doubled.
        assert!(two_x.cell_line_h > one_x.cell_line_h * 1.9);
        assert!(two_x.default_row_h() >= two_x.cell_line_h + two_x.pad - 0.01);
        assert!(two_x.col_header_h() >= two_x.header_line_h + two_x.header_pad - 0.01);
        assert!(two_x.row_header_w() >= two_x.row_label_w + 2.0 * two_x.pad - 0.01);

        g.set_metrics(two_x);
        assert!(
            (g.row_h(3) - 40.0).abs() < 0.01,
            "manual row height is byte-for-byte"
        );
        assert!(
            (g.row_h(4) - two_x.default_row_h()).abs() < 0.01,
            "automatic row follows text"
        );
        assert!(
            g.row_top(2) > one_x_top_of_row_2,
            "prefix sums were rebuilt"
        );
        assert!((g.row_top(4) - (3.0 * two_x.default_row_h() + 40.0)).abs() < 0.01);
        assert!((g.grid_y0() - g.rect.y0 - two_x.col_header_h()).abs() < 0.01);
        assert!((g.grid_x0() - g.rect.x0 - two_x.row_header_w()).abs() < 0.01);
        // Unchanged metrics are a no-op: no needless geometry rebuild flag.
        g.needs_autofit = false;
        g.set_metrics(two_x);
        assert!(!g.needs_autofit);
    }

    {
        let mut grid = grid_with(&[]);
        // The normal logical grid is larger than this deliberately narrow
        // viewport, so both rails expose a thumb and have scroll range.
        grid.rect = Rect::new(0.0, 0.0, 260.0, 180.0);
        let mut out = Vec::new();
        let horizontal = grid
            .scrollbar_thumb(ScrollAxis::Horizontal)
            .expect("the wide sheet overflows the narrow viewport");
        assert!(grid.pointer_down(
            Point::new(
                horizontal.x0.midpoint(horizontal.x1),
                horizontal.y0.midpoint(horizontal.y1)
            ),
            false,
            false,
            &mut out,
        ));
        assert!(grid.pointer_move(Point::new(
            horizontal.x1 + 40.0,
            horizontal.y0.midpoint(horizontal.y1)
        )));
        grid.pointer_up();
        assert!(grid.scroll_x > 0.0);

        let vertical = grid
            .scrollbar_thumb(ScrollAxis::Vertical)
            .expect("the tall sheet overflows the short viewport");
        assert!(grid.pointer_down(
            Point::new(
                vertical.x0.midpoint(vertical.x1),
                vertical.y0.midpoint(vertical.y1)
            ),
            false,
            false,
            &mut out,
        ));
        assert!(grid.pointer_move(Point::new(
            vertical.x0.midpoint(vertical.x1),
            vertical.y1 + 40.0
        )));
        grid.pointer_up();
        assert!(grid.scroll_y > 0.0);
    }
}

/// A grid with a known geometry: each column is the default width, the
/// body starts after the headers and is wide enough for many columns,
/// with the scroll bar gutters beside it. No scroll.
fn grid_with(values: &[(Addr, CellValue)]) -> Grid {
    let mut g = Grid::new();
    let m = g.metrics();
    // Body roughly 12 columns wide so overflow has room.
    let width = m.row_header_w() + m.default_col_w() * 12.0 + m.gutter;
    let height = m.col_header_h() + m.default_row_h() * 12.0 + m.gutter;
    g.rect = Rect::new(0.0, 0.0, width, height);
    let mut map = HashMap::new();
    for (addr, v) in values {
        map.insert(*addr, v.clone());
    }
    g.values = map;
    g
}

/// A row that appears opens the room the rows below need rather than
/// teleporting into place: the band starts closed, the row after it is
/// drawn where it was, and both come to rest on the standard disclosure.
/// A row that leaves closes its band, and the rows below rise into it.
#[test]
fn an_inserted_row_opens_and_the_rows_after_it_make_room() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut g = grid_with(&[]);
    g.set_metrics(GridMetrics::resolve(&mut tr, &theme));
    let laid = g.cell_screen_rect(0, 3).y0;
    let row_h = g.metrics.default_row_h();

    g.rows_inserted(2, 1);
    // Drawn before its first frame: the band is closed, so the row after
    // it still stands where it did — one row above its new place.
    assert!(g.is_animating(), "the change asks for frames");
    assert!(
        (g.cell_screen_rect(0, 3).y0 - (laid - row_h)).abs() < 0.01,
        "the row after the closed band keeps its old place"
    );
    let shift = g.row_shift.as_ref().expect("the band is on its way");
    assert!((shift.full - row_h).abs() < 0.01, "one row opens");

    // Mid-way: the following row has moved down by what has opened of
    // the band, not by the whole of it.
    g.start_shifts(&theme);
    assert!(
        g.row_shift
            .as_ref()
            .is_some_and(|shift| shift.open_height() > -0.01),
        "the band is opening"
    );
    let opened = g.row_shift.as_ref().map_or(0.0, Shift::open_height);
    let followed = g.cell_screen_rect(0, 3).y0 - laid;
    assert!(
        (-row_h..=0.01).contains(&followed),
        "the row after the band moves from where it was to its new place: {followed}"
    );
    assert!(opened >= 0.0);

    // A row leaving closes its band: the rows below start a band lower
    // and rise into the gap.
    let mut g = grid_with(&[]);
    g.set_metrics(GridMetrics::resolve(&mut tr, &theme));
    let laid = g.cell_screen_rect(0, 2).y0;
    g.rows_removed(2, 1);
    let shift = g.row_shift.as_ref().expect("the band is leaving");
    assert!(shift.leaving);
    assert!(
        (g.cell_screen_rect(0, 2).y0 - (laid + row_h)).abs() < 0.01,
        "the row that closed the gap starts a row lower"
    );
}

#[test]
fn autofit_respects_occupancy_and_manual_sizes() {
    {
        let mut g = grid_with(&[((1, 0), CellValue::Number(5.0))]);
        assert!(g.cell_is_occupied((1, 0)));
        assert!(!g.cell_is_occupied((2, 0)));
        // Empty CellValue is not occupied.
        g.values.insert((3, 0), CellValue::Empty);
        assert!(!g.cell_is_occupied((3, 0)));
        // A raw entry (mid-edit, pre-eval) counts as occupied too.
        g.raw.insert((4, 0), "x".into());
        assert!(g.cell_is_occupied((4, 0)));
    }

    {
        // A0 holds a label wider than a default column; B0 is occupied so the
        // label can't overflow into it. Auto-fit must widen column A to fit the
        // whole label, while the short occupied neighbour keeps the default.
        let label = "Monthly budget total";
        let mut g = grid_with(&[
            ((0, 0), CellValue::Text(label.into())),
            ((1, 0), CellValue::Number(2600.0)),
        ]);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let style = g.cell_style(&theme, (0, 0), false);
        let label_w = f64::from(tr.measure_style(label, None, &style).0);
        let want = label_w + g.metrics.cell_pad() * 2.0;
        let default = g.metrics.default_col_w();
        // Precondition: this label really does overflow a default-width column.
        assert!(want > default, "test label must exceed the default width");

        g.autofit_columns(&mut tr, &theme);

        assert!(
            g.col_w(0) + 0.5 >= want,
            "col A width {} must fit the label (needs {want})",
            g.col_w(0),
        );
        assert!(
            g.col_w(0) <= g.metrics.max_autofit_w() + 0.5,
            "auto-fit stays clamped"
        );
        assert_eq!(
            g.col_w(1),
            default,
            "short occupied neighbour is left alone"
        );
    }

    {
        // A manual resize must win over auto-fit.
        let mut g = grid_with(&[((0, 0), CellValue::Text("Monthly budget total".into()))]);
        g.col_widths.insert(0, 200.0);
        let mut tr = TextRenderer::new();
        g.autofit_columns(&mut tr, &Theme::dark());
        assert_eq!(g.col_w(0), 200.0, "manual width overrides auto-fit");
    }
}

/// In-cell editor overflow and clipping.
#[test]
fn cell_editor_overflows_only_into_empty_neighbours() {
    {
        // A0 is being edited; B0.. are all empty. A wide desired width should
        // grow across several empty columns.
        let g = grid_with(&[]);
        let cell = g.cell_screen_rect(0, 0);
        let body_right = g.body_rect().x1;
        // Desired width spanning ~3 columns.
        let desired = g.metrics.default_col_w() * 3.0;
        let right = g.editor_right_extent((0, 0), desired, body_right);
        // It should reach at least 3 columns wide (within the body).
        assert!(
            right >= cell.x0 + g.metrics.default_col_w() * 3.0 - 0.5,
            "right={right} expected >= {}",
            cell.x0 + g.metrics.default_col_w() * 3.0
        );
        assert!(right <= body_right + 0.5);
    }

    {
        // A0 edited; C0 (two cells over) is occupied. The editor may use B0
        // but must STOP before C0 even though it wants more room.
        let g = grid_with(&[((2, 0), CellValue::Text("blocker".into()))]);
        let _cell = g.cell_screen_rect(0, 0);
        let body_right = g.body_rect().x1;
        let desired = g.metrics.default_col_w() * 6.0; // wants a lot
        let right = g.editor_right_extent((0, 0), desired, body_right);
        // Reaches the end of B0 (= start of C0) but not into C0.
        let c0_left = g.cell_screen_rect(2, 0).x0;
        assert!(
            (right - c0_left).abs() < 0.5,
            "right={right} c0_left={c0_left}"
        );
    }

    {
        // B0 occupied directly to the right of A0 → editor stays cell-wide.
        let g = grid_with(&[((1, 0), CellValue::Number(1.0))]);
        let cell = g.cell_screen_rect(0, 0);
        let body_right = g.body_rect().x1;
        let right = g.editor_right_extent((0, 0), g.metrics.default_col_w() * 4.0, body_right);
        assert!(
            (right - cell.x1).abs() < 0.5,
            "right={right} cell.x1={}",
            cell.x1
        );
    }

    {
        let g = grid_with(&[]);
        let body_right = g.body_rect().x1;
        let right = g.editor_right_extent((10, 0), g.metrics.default_col_w() * 50.0, body_right);
        assert!(
            right <= body_right + 0.5,
            "right={right} body_right={body_right}"
        );
    }
}

/// The screen point at the centre of a cell — where a click would land.
fn cell_point(g: &Grid, c: u32, r: u32) -> Point {
    g.cell_screen_rect(c, r).center()
}

/// Drag, shift/ctrl click/drag multi-ranges, aggregation and arrow collapse.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn pointer_selection_builds_and_collapses_ranges() {
    {
        // Press on B2, move to D5 with the button held, release. This is the
        // exact `pointer_down`/`pointer_move`/`pointer_up` path `handle_event`
        // drives on a live left-drag.
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        assert!(g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out));
        // Live update on each move while the button is held.
        g.pointer_move(cell_point(&g, 2, 3));
        assert_eq!(g.selection_bounds(), ((1, 1), (2, 3)), "mid-drag extent");
        g.pointer_move(cell_point(&g, 3, 4));
        g.pointer_up();
        assert_eq!(g.selection_bounds(), ((1, 1), (3, 4)));
        assert_eq!(g.active(), (3, 4));
        assert!(!g.is_multi_range());
    }

    {
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
        g.pointer_up();
        // Shift+click D4: anchor stays at B2, range grows to B2:D4.
        g.pointer_down(cell_point(&g, 3, 3), true, false, &mut out);
        g.pointer_up();
        assert_eq!(g.selection_bounds(), ((1, 1), (3, 3)));
        assert!(!g.is_multi_range());
    }

    {
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        // First range: drag B2:C3.
        g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
        g.pointer_move(cell_point(&g, 2, 2));
        g.pointer_up();
        // Ctrl+click F6 → adds a second, single-cell range; F6 becomes active.
        g.pointer_down(cell_point(&g, 5, 5), false, true, &mut out);
        g.pointer_up();
        assert!(g.is_multi_range());
        assert_eq!(g.active(), (5, 5));
        let cells = g.selected_cells();
        assert!(
            cells.contains(&(1, 1)) && cells.contains(&(2, 2)),
            "first range kept"
        );
        assert!(cells.contains(&(5, 5)), "second range added");
        assert!(!cells.contains(&(3, 3)), "gap between ranges not selected");
    }

    {
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        // First range: a single cell B2.
        g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
        g.pointer_up();
        // Ctrl+drag F6→G8 adds a new rectangle while keeping B2.
        g.pointer_down(cell_point(&g, 5, 5), false, true, &mut out);
        g.pointer_move(cell_point(&g, 6, 7));
        g.pointer_up();
        assert!(g.is_multi_range());
        let cells = g.selected_cells();
        assert!(cells.contains(&(1, 1)), "original cell kept");
        for c in 5..=6 {
            for r in 5..=7 {
                assert!(cells.contains(&(c, r)), "added rectangle covers ({c},{r})");
            }
        }
    }

    {
        // Two numeric columns with a non-numeric gap between them. Column B
        // (index 1) holds 100/200/300; column D (index 3) holds 10/20/30; the
        // gap column C (index 2) holds a label so it must never contribute.
        let mut g = grid_with(&[
            ((1, 1), CellValue::Number(100.0)),
            ((1, 2), CellValue::Number(200.0)),
            ((1, 3), CellValue::Number(300.0)),
            ((2, 1), CellValue::Text("label".into())),
            ((3, 1), CellValue::Number(10.0)),
            ((3, 2), CellValue::Number(20.0)),
            ((3, 3), CellValue::Number(30.0)),
        ]);
        let mut out = Vec::new();
        // Drag-select B2:B4 (the first rectangle), then Ctrl+drag D2:D4 to add a
        // discontiguous second rectangle — the exact production pointer path.
        g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
        g.pointer_move(cell_point(&g, 1, 3));
        g.pointer_up();
        g.pointer_down(cell_point(&g, 3, 1), false, true, &mut out);
        g.pointer_move(cell_point(&g, 3, 3));
        g.pointer_up();
        assert!(g.is_multi_range(), "two discontiguous ranges are selected");

        // Aggregate over the exact methods the status bar's summary consumes:
        // `selected_cells()` (dedup across ranges) and `cell_value_at()`.
        let mut sum = 0.0;
        let mut count = 0usize;
        for addr in g.selected_cells() {
            if let Some(CellValue::Number(n)) = g.cell_value_at(addr) {
                sum += n;
                count += 1;
            }
        }
        assert_eq!(count, 6, "six numeric cells across both rectangles");
        assert!(
            (sum - 660.0).abs() < 1e-9,
            "sum over both ranges = 660, got {sum}"
        );
        assert!(!g.selected_cells().contains(&(2, 1)), "gap column excluded");
    }

    {
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
        g.pointer_up();
        g.pointer_down(cell_point(&g, 5, 5), false, true, &mut out);
        g.pointer_up();
        assert!(g.is_multi_range());
        g.move_active(1, 0, false);
        assert!(!g.is_multi_range());
        assert_eq!(g.selected_cells(), vec![(6, 5)]);
    }
}

/// Point-mode click/drag insertion, operator append, replacement and off-state.
#[test]
fn formula_point_mode_inserts_references() {
    {
        // Select A5, start a formula (as typing '=' would), then click B1.
        // This drives the exact grid path `handle_event` runs on a click.
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        g.select_only((0, 4));
        g.begin_edit(Some("=".into()));
        assert!(g.pointer_down(cell_point(&g, 1, 0), false, false, &mut out));
        g.pointer_up();
        assert!(
            out.is_empty(),
            "a point-mode click must not commit the edit"
        );
        assert_eq!(g.active_display_raw(), "=B1", "clicked cell's ref inserted");
        assert!(g.is_editing(), "still editing the original cell");
        assert_eq!(g.active(), (0, 4), "selection stayed on the edited cell");
    }

    {
        // Press A1, drag to B4 while editing a formula → inserts A1:B4.
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        g.select_only((0, 6));
        g.begin_edit(Some("=".into()));
        assert!(g.pointer_down(cell_point(&g, 0, 0), false, false, &mut out));
        g.pointer_move(cell_point(&g, 1, 3));
        g.pointer_up();
        assert_eq!(g.active_display_raw(), "=A1:B4");
        assert!(out.is_empty(), "the drag never committed the edit");
    }

    {
        // =A1  then type '+'  then click B2  →  =A1+B2.
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        g.select_only((4, 4));
        g.begin_edit(Some("=".into()));
        g.pointer_down(cell_point(&g, 0, 0), false, false, &mut out);
        g.pointer_up();
        assert_eq!(g.active_display_raw(), "=A1");
        g.insert_text_public("+");
        g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
        g.pointer_up();
        assert_eq!(g.active_display_raw(), "=A1+B2");
    }

    {
        // Click A1, then C3 without typing an operator → the ref is replaced
        // (Excel point-mode behaviour), not appended.
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        g.select_only((4, 4));
        g.begin_edit(Some("=".into()));
        g.pointer_down(cell_point(&g, 0, 0), false, false, &mut out);
        g.pointer_up();
        g.pointer_down(cell_point(&g, 2, 2), false, false, &mut out);
        g.pointer_up();
        assert_eq!(g.active_display_raw(), "=C3");
    }

    {
        // A non-pointable click (nothing typed makes the caret ready) commits
        // the edit and moves the selection instead of inserting a reference.
        let mut g = grid_with(&[]);
        let mut out = Vec::new();
        g.select_only((0, 0));
        g.begin_edit(Some("=A1".into())); // caret after '1', no operator
        g.point_anchor = None; // as if the ref were typed, not clicked
        assert!(g.pointer_down(cell_point(&g, 3, 3), false, false, &mut out));
        g.pointer_up();
        // The edit committed to A1 and the selection moved to the clicked cell.
        assert!(
            out.iter().any(|c| matches!(c, GridCmd::SetCell { .. })),
            "a non-pointable click commits the edit"
        );
        assert!(!g.is_editing());
        assert_eq!(g.active(), (3, 3));
    }
}

// --- formula-bar text editing (caret + selection) ----------------------

/// Edit selection replace/paste, grapheme boundaries, cut and shift-arrow delete.
#[test]
fn cell_edit_selection_replaces_cuts_and_deletes() {
    {
        // The exact methods the formula bar drives: set_edit_caret (click /
        // shift-drag), typing over a selection, select-all, and paste.
        let mut g = grid_with(&[]);
        g.select_only((0, 0));
        g.begin_edit(Some("hello".into()));
        g.set_edit_caret(1, false); // click after 'h'
        g.set_edit_caret(4, true); // shift-drag to select "ell"
        assert_eq!(g.edit_selection(), Some((1, 4)));
        assert_eq!(g.edit_selected_text().as_deref(), Some("ell"));
        g.insert_text_public("ELL");
        assert_eq!(g.active_display_raw(), "hELLo");
        g.select_all_edit();
        g.edit_paste_text("=A1+B2");
        assert_eq!(g.active_display_raw(), "=A1+B2");
    }

    {
        let mut g = Grid::new();
        g.begin_edit(Some("a🙂b".into()));
        g.set_edit_caret(1, false);
        g.set_edit_caret(2, true);
        assert_eq!(g.edit_selected_text().as_deref(), Some("🙂"));
        g.insert_text_public("X");
        assert_eq!(g.edit_selected_text(), None);
        assert_eq!(g.active_display_raw(), "aXb");
    }

    {
        let mut g = grid_with(&[]);
        g.select_only((0, 0));
        g.begin_edit(Some("abcdef".into()));
        g.set_edit_caret(2, false);
        g.set_edit_caret(5, true); // select "cde"
        assert_eq!(g.edit_cut_text().as_deref(), Some("cde"));
        assert_eq!(g.active_display_raw(), "abf");
        assert_eq!(g.edit_caret(), Some(2));
    }

    {
        let mut g = grid_with(&[]);
        g.select_only((0, 0));
        g.begin_edit(Some("abcd".into())); // caret at end (4)
        g.edit_move_caret(CaretMove::Left, true); // select "d"
        g.edit_move_caret(CaretMove::Left, true); // select "cd"
        assert_eq!(g.edit_selection(), Some((2, 4)));
        // Backspace removes the whole selection, not one char.
        assert!(g.delete_selection());
        assert_eq!(g.active_display_raw(), "ab");
    }
}

/// Formula/header highlights and bounded accessibility windows.
#[test]
fn highlights_and_accessibility_windows_track_the_selection() {
    {
        let mut g = grid_with(&[]);
        g.raw.insert((0, 0), "=A2+A3".into());
        g.select_only((0, 0));
        assert!(!g.is_editing());
        // Refs are available for highlighting even when only selecting.
        assert_eq!(g.highlight_formula_body().as_deref(), Some("A2+A3"));
        g.raw.insert((1, 0), "42".into());
        g.select_only((1, 0));
        assert_eq!(g.highlight_formula_body(), None);
    }

    {
        let mut g = grid_with(&[]);
        g.select_range_public((2, 3), (4, 5)); // C4:E6
        assert!(g.col_in_selection(2) && g.col_in_selection(4));
        assert!(!g.col_in_selection(1) && !g.col_in_selection(5));
        assert!(g.row_in_selection(3) && g.row_in_selection(5));
        assert!(!g.row_in_selection(2) && !g.row_in_selection(6));
        g.add_range_public((7, 8), (7, 8)); // H9
        assert!(g.col_in_selection(7) && g.row_in_selection(8));
        assert!(g.col_in_selection(2), "original range headers stay lit");
    }

    {
        let mut g = grid_with(&[((1, 1), CellValue::Number(42.0))]);
        g.raw.insert((1, 1), "42".into());
        g.select_only((1, 1));
        let window = g.accessibility_window();
        assert!(window.columns.len() < g.n_cols as usize);
        assert!(window.rows.len() < g.n_rows as usize);
        let active = window
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .find(|cell| cell.addr == (1, 1))
            .expect("visible active cell");
        assert_eq!(active.display, "42");
        assert!(active.selected);
    }

    {
        let mut g = Grid::new();
        g.select_only((40, 180));
        let window = g.accessibility_window();
        assert!(window.columns.len() <= A11Y_FALLBACK_COLS as usize);
        assert!(window.rows.len() <= A11Y_FALLBACK_ROWS as usize);
        assert!(
            window
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.addr == (40, 180))
        );
    }

    {
        let mut g = Grid::new();
        g.n_cols = 16_384;
        g.n_rows = 1_048_576;
        g.rebuild_geom();
        g.rect = Rect::new(0.0, 0.0, 280.0, 180.0);
        g.scroll_x = g.max_scroll_x();
        g.scroll_y = g.max_scroll_y();

        let columns = g.visible_cols();
        let rows = g.visible_rows();
        assert!(columns.len() < 8 && rows.len() < 12);
        assert_eq!(columns.last().copied(), Some(g.n_cols - 1));
        assert_eq!(rows.last().copied(), Some(g.n_rows - 1));
    }
}
