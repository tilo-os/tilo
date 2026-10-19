#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "pixel geometry over bounded fixture canvases"
)]

//! Composition and data-display widgets: renders the immediate-mode data
//! widgets through the shared `FrameHarness` at 1x and 2x text scale and
//! checks the alignment contract each widget paints against, on the pixels
//! it actually produced.
//!
//! With `TILO_UI_SCREENSHOT_DIR` set, each frame is also written under
//! `data_widget_text_alignment/` there as visual evidence; the assertions
//! are the regression.
//!
//! Two properties are checked per widget:
//! - at 1x, a single line of descender-free text sits on the centre line of
//!   its band (its ink centre is within a pixel of the band centre, which the
//!   old `(band - font_size) / 2` placement missed by ~1.5 px);
//! - the ink height grows by ~2x between 1x and 2x text scale. The widgets
//!   used to grow by 4x because they fed already-scaled theme sizes to the
//!   renderer, which scales once more.

use std::collections::HashMap;

use tilo_ui::State;
use tilo_ui::TextRole;
use tilo_ui::headless::{FrameHarness, InspectionNode};
use tilo_ui::icons::Icon;
use tilo_ui::search::{SearchGroup, SearchResult};
use tilo_ui::theme::Theme;
use tilo_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use tilo_ui::widget::{
    Banner, Button, Chart, Column, Element, SearchResultsView, Section, SectionHeader, Series,
    StatGroup, StatTile, Text, TreeNode, TreeView, Trend,
};
use vello::kurbo::Rect;

// Pinned widget defaults the regions below are built from.
const TABLE_HEADER_H: f64 = 32.0;
const TABLE_ROW_H: f64 = 24.0;
const TREE_ROW_H: f64 = 22.0;
const TREE_DISCLOSURE_W: f64 = 16.0;
const SEARCH_PAD: f64 = 8.0;
const SEARCH_HEADER_H: f64 = 26.0;
const SEARCH_ROW_ONE_LINE: f64 = 38.0;
const SEARCH_ROW_TWO_LINE: f64 = 50.0;
const SEARCH_TEXT_X: f64 = 8.0 + 12.0 + 30.0;
const CHART_HEADER_H: f64 = 22.0;

// The production formulas the widgets size their bands from — one line of
// type as the renderer lays it out, and a pinned floor grown by content plus
// padding — so the expectations below cannot drift from the widgets.
use tilo_ui::widget::data_table::{text_band as band, type_line_height as line};

struct Capture {
    harness: FrameHarness,
    image: image::RgbaImage,
}

fn capture(name: &str, root: Element, viewport: (u32, u32), scale: f32) -> Capture {
    capture_with_theme(name, root, viewport, Theme::dark().with_text_scale(scale))
}

/// [`capture`] under an explicit theme. Evidence for the dark preset is named
/// `<name>-<scale>x`; any other theme adds its name, `<name>-<theme>-<scale>x`.
fn capture_with_theme(name: &str, root: Element, viewport: (u32, u32), theme: Theme) -> Capture {
    let scale = theme.text_scale;
    let stem = if theme.name == Theme::dark().name {
        format!("{name}-{scale}x")
    } else {
        format!("{name}-{}-{scale}x", theme.name)
    };
    let harness = FrameHarness::new_with_theme(State::new(), root, viewport, theme);
    let rgba = harness.render_rgba();
    crate::support::evidence::save_png(
        &format!("data_widget_text_alignment/{stem}"),
        viewport.0,
        viewport.1,
        &rgba,
    );
    let image =
        image::RgbaImage::from_raw(viewport.0, viewport.1, rgba).expect("viewport-sized frame");
    Capture { harness, image }
}

fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

fn find<'a>(node: &'a InspectionNode, widget: &str) -> &'a InspectionNode {
    fn walk<'a>(node: &'a InspectionNode, widget: &str) -> Option<&'a InspectionNode> {
        if node.widget == widget {
            return Some(node);
        }
        node.children.iter().find_map(|child| walk(child, widget))
    }
    walk(node, widget).unwrap_or_else(|| panic!("no {widget} in the inspection tree"))
}

fn luma(p: [u8; 4]) -> f64 {
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

fn pixels(
    image: &image::RgbaImage,
    region: Rect,
) -> impl Iterator<Item = (u32, u32, [u8; 4])> + '_ {
    let x0 = region.x0.max(0.0).floor() as u32;
    let y0 = region.y0.max(0.0).floor() as u32;
    let x1 = (region.x1.ceil() as u32).min(image.width());
    let y1 = (region.y1.ceil() as u32).min(image.height());
    (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y, image.get_pixel(x, y).0)))
}

/// The region's fill: the most frequent colour inside it.
fn dominant(image: &image::RgbaImage, region: Rect) -> [u8; 4] {
    let mut counts: HashMap<[u8; 4], usize> = HashMap::new();
    for (_, _, p) in pixels(image, region) {
        *counts.entry(p).or_default() += 1;
    }
    // Ties break on the colour bytes so the threshold is run-to-run stable.
    counts
        .into_iter()
        .max_by_key(|(p, n)| (*n, *p))
        .map(|(p, _)| p)
        .expect("non-empty region")
}

/// Bounding box of the pixels clearly brighter than the region's fill: text
/// and icon strokes on the dark theme, but not borders, zebra fills or grid
/// rules (which stay within the threshold of their surface).
fn ink(image: &image::RgbaImage, region: Rect) -> Rect {
    let floor = luma(dominant(image, region)) + 60.0;
    let mut bbox: Option<Rect> = None;
    for (x, y, p) in pixels(image, region) {
        if luma(p) > floor {
            let px = Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + 1),
                f64::from(y + 1),
            );
            bbox = Some(bbox.map_or(px, |b| b.union(px)));
        }
    }
    bbox.unwrap_or_else(|| panic!("no ink in {region:?}"))
}

fn assert_centered(what: &str, ink: Rect, band: Rect, tolerance: f64) {
    let offset = ink.center().y - band.center().y;
    assert!(
        offset.abs() <= tolerance,
        "{what}: ink {ink:?} sits {offset:+.2} px off the centre of {band:?}"
    );
    assert!(
        ink.y0 >= band.y0 && ink.y1 <= band.y1,
        "{what}: ink {ink:?} leaves its band {band:?}"
    );
}

/// The band is taller than its text: ink keeps at least a pixel of air at
/// the top and bottom. Text that outgrew a fixed band fills the probe edge to
/// edge instead, so this is what tells a grown band from a clipped one.
fn assert_has_air(what: &str, ink: Rect, band: Rect) {
    assert!(
        ink.y0 > band.y0 + 0.5 && ink.y1 < band.y1 - 0.5,
        "{what}: ink {ink:?} touches the edge of its band {band:?}"
    );
}

/// The renderer scales once: ink grows ~2x between 1x and 2x, not 4x.
fn assert_scaled_once(what: &str, base: Rect, large: Rect) {
    let ratio = large.height() / base.height();
    assert!(
        (1.6..=2.6).contains(&ratio),
        "{what}: ink height {:.1} -> {:.1} (x{ratio:.2}) must follow the text scale once",
        base.height(),
        large.height()
    );
}

#[derive(Clone)]
struct Process {
    name: String,
    cpu: f32,
    mem: u32,
}

fn processes() -> Vec<Process> {
    (0..14)
        .map(|index| Process {
            name: format!("service-daemon-{index:02}"),
            cpu: (index as f32 * 1.1) % 10.0,
            mem: 40 + index * 7,
        })
        .collect()
}

fn data_table() -> Element {
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Process", ColumnWidth::Fixed(220.0))
            .sortable(true)
            .with_text(|row: &Process| row.name.clone()),
        ColumnDef::new("cpu", "CPU%", ColumnWidth::Fixed(90.0))
            .sortable(true)
            .with_text(|row: &Process| format!("{:.1}%", row.cpu)),
        ColumnDef::new("mem", "Mem", ColumnWidth::Fixed(90.0))
            .with_text(|row: &Process| format!("{} MB", row.mem)),
    ])
    .with_id("processes")
    .with_viewport(220.0, 24.0);
    table.set_rows(processes());
    table.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
    Box::new(table)
}

fn tree_view() -> Element {
    let root = TreeNode::branch(
        1,
        "crates",
        (),
        vec![
            // Cap-height, descender-free label: the second-row probe centres
            // on its ink, and x-height-only text ("src") sits visibly low.
            TreeNode::branch(
                2,
                "Sources",
                (),
                vec![
                    TreeNode::leaf(3, "main.rs", ()),
                    TreeNode::leaf(4, "lib.rs", ()),
                ],
            ),
            TreeNode::leaf(5, "README.md", ()),
            TreeNode::branch(6, "tests", (), vec![]),
        ],
    );
    Box::new(
        TreeView::new(root)
            .with_id("tree")
            .with_expanded([1_u64, 2_u64])
            .with_selected(Some(3))
            .with_viewport(200.0, 22.0),
    )
}

fn search_results() -> Element {
    let mut view = SearchResultsView::new().with_test_id("results");
    view.set_groups(vec![
        SearchGroup {
            kind: "Contacts".into(),
            results: vec![
                SearchResult::new("Contacts", "Alice Ng", "a")
                    .with_subtitle("alice@example.com")
                    .with_icon(Icon::Mail),
                // Descender-free title and subtitle: the two-line probe
                // centres on their ink.
                SearchResult::new("Contacts", "Ada Lovelace", "b")
                    .with_subtitle("Mathematics")
                    .with_icon(Icon::Mail),
            ],
        },
        SearchGroup {
            kind: "Apps".into(),
            results: vec![
                SearchResult::new("Apps", "Calculator", "c").with_icon(Icon::Terminal),
                SearchResult::new("Apps", "Calendar", "d").with_icon(Icon::Calendar),
                SearchResult::new("Apps", "Camera", "e"),
            ],
        },
    ]);
    Box::new(view)
}

fn banners() -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0)
            .push(Box::new(
                Banner::info("A new version is available.").with_message("Restart to finish."),
            ))
            .push(Box::new(Banner::success("All set.")))
            .push(Box::new(
                Banner::warning("You're offline.")
                    .with_message("Edits will sync when you reconnect.")
                    .dismissible(),
            ))
            .push(Box::new(Banner::error("Upload failed.").dismissible())),
    )
}

fn stat_tiles() -> Element {
    Box::new(StatGroup::new(vec![
        Box::new(StatTile::new("Sessions", "1,284").change("12%", Trend::Up)),
        Box::new(StatTile::new("Errors", "3").change("2", Trend::Down)),
        Box::new(StatTile::new("Latency", "48").unit("ms")),
        Box::new(StatTile::new("Uptime", "99.9").unit("%")),
    ]))
}

fn chart() -> Element {
    let theme = Theme::dark();
    Box::new(
        Chart::new(vec![
            Series::new(
                vec![1.0, 3.0, 2.0, 5.0, 4.0, 6.0, 5.5],
                theme.tokens.colors.intent.accent,
            )
            .with_legend("Sessions"),
            Series::new(
                vec![0.5, 1.0, 0.8, 1.5, 1.2, 1.1, 1.4],
                theme.status_success,
            )
            .with_legend("Errors"),
        ])
        .with_title("Sessions over time")
        .with_axis_labels("6.0", "0")
        .with_height(180.0),
    )
}

fn legacy_cards() -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(12.0)
            .push(Box::new(Section::titled(
                "Region formats",
                vec![Box::new(Text::new(
                    "Dates, times and numbers follow the selected region.",
                ))],
            )))
            .push(Box::new(
                Section::new(vec![Box::new(Text::new("No email accounts yet."))]).with_header(
                    SectionHeader::heading("Email")
                        .action(Button::new("add-account", "Add account")),
                ),
            )),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn data_rows_centre_their_text_and_scale_once() {
    // Case `data_table_header_and_rows_centre_text_and_scale_once`.
    {
        let mut header_ink = Vec::new();
        for scale in [1.0, 2.0] {
            let cap = capture("data_table", data_table(), (440, 260), scale);
            let table = rect(find(&cap.harness.inspection().root, "DataTable"));
            // The bands the table resolves from the theme: the header holds the
            // `sm` label line plus `s3`; the fixture's authored 24 px row floors
            // at the line plus `s1`. Both are the pinned constants at 1x.
            let theme = Theme::dark().with_text_scale(scale);
            let sm = line(&theme.tokens.typography, TextRole::Caption);
            let header_h = band(TABLE_HEADER_H, sm, f64::from(theme.tokens.spacing.s3));
            let row_h = band(TABLE_ROW_H, sm, f64::from(theme.tokens.spacing.s1));
            assert!(
                header_h >= TABLE_HEADER_H && row_h >= TABLE_ROW_H,
                "the bands never undercut the pinned defaults"
            );
            if scale > 1.0 {
                assert!(header_h > TABLE_HEADER_H && row_h > TABLE_ROW_H);
            }
            // First column only: the label and the sort icon beside it share
            // the header's centre line.
            let header = Rect::new(
                table.x0 + 4.0,
                table.y0,
                table.x0 + 216.0,
                table.y0 + header_h,
            );
            let header_label = ink(&cap.image, header);
            header_ink.push(header_label);
            assert_centered("header label", header_label, header, 1.0);
            assert_has_air("header label", header_label, header);
            // The first two rows: if the rows had stayed 24 px at 2x, the second
            // probe would straddle two rows' text and neither would centre.
            for index in 0..2 {
                let y0 = header.y1 + f64::from(index) * row_h;
                let row = Rect::new(header.x0, y0, header.x1, y0 + row_h);
                let cell = ink(&cap.image, row);
                assert_centered(&format!("row {index} cell"), cell, row, 1.0);
                assert_has_air(&format!("row {index} cell"), cell, row);
            }
        }
        assert_scaled_once("data table header", header_ink[0], header_ink[1]);
    }

    // Case `tree_view_rows_centre_labels_and_scale_once`.
    {
        let mut label_ink = Vec::new();
        for scale in [1.0, 2.0] {
            let cap = capture("tree_view", tree_view(), (320, 220), scale);
            let tree = rect(find(&cap.harness.inspection().root, "TreeView"));
            // The fixture's authored 22 px row floors at the `sm` line plus `s1`;
            // the disclosure column grows to the line so the glyph clears the
            // label. Both are the pinned constants at 1x.
            let theme = Theme::dark().with_text_scale(scale);
            let sm = line(&theme.tokens.typography, TextRole::Body);
            let row_h = band(TREE_ROW_H, sm, f64::from(theme.tokens.spacing.s1));
            let disclosure_w = band(TREE_DISCLOSURE_W, sm, 0.0);
            let indent = f64::from(theme.tokens.spacing.s4);
            assert!(row_h >= TREE_ROW_H && disclosure_w >= TREE_DISCLOSURE_W);
            if scale > 1.0 {
                assert!(row_h > TREE_ROW_H && disclosure_w > TREE_DISCLOSURE_W);
            }
            let row = Rect::new(
                tree.x0 + disclosure_w,
                tree.y0,
                tree.x0 + 200.0,
                tree.y0 + row_h,
            );
            let root_label = ink(&cap.image, row);
            label_ink.push(root_label);
            // The shaped line box is exactly centred; the rasterized lowercase
            // glyphs can carry up to ~2.5 px more ink below their optical centre at
            // the 2× body size.
            assert_centered("root label", root_label, row, 2.5);
            assert_has_air("root label", root_label, row);
            let glyph = Rect::new(tree.x0, tree.y0, tree.x0 + disclosure_w, tree.y0 + row_h);
            let glyph_ink = ink(&cap.image, glyph);
            // The down-triangle's rasterized ink is optically bottom-heavy at
            // 2x even though it shares the label's centred line box.
            assert_centered("disclosure glyph", glyph_ink, glyph, 3.0);
            assert!(
                glyph_ink.x1 <= row.x0,
                "disclosure glyph {glyph_ink:?} must clear the label column starting at {}",
                row.x0
            );
            // Second row (`src`, depth 1): centred in its own band, which only
            // holds if the first row really is `row_h` tall.
            let second = Rect::new(
                tree.x0 + indent + disclosure_w,
                tree.y0 + row_h,
                tree.x0 + 200.0,
                tree.y0 + 2.0 * row_h,
            );
            let second_label = ink(&cap.image, second);
            assert_centered("second row label", second_label, second, 2.5);
            assert_has_air("second row label", second_label, second);
        }
        assert_scaled_once("tree label", label_ink[0], label_ink[1]);
    }

    // Case `search_results_centre_single_line_titles_and_scale_once`.
    {
        let mut title_ink = Vec::new();
        for scale in [1.0, 2.0] {
            let cap = capture("search_results", search_results(), (420, 360), scale);
            let view = rect(find(&cap.harness.inspection().root, "SearchResultsView"));
            // The bands the view resolves from the theme: a one-line row holds
            // the `base` title line plus `s3`, a two-line row the title, `s1` and
            // the `sm` subtitle plus `s3`, the section header its `Subheading`
            // line plus two `s1` insets. All three are the pinned constants at 1x.
            let theme = Theme::dark().with_text_scale(scale);
            let typography = &theme.tokens.typography;
            let s1 = f64::from(theme.tokens.spacing.s1);
            let s3 = f64::from(theme.tokens.spacing.s3);
            let header_h = band(
                SEARCH_HEADER_H,
                line(typography, TextRole::Subheading),
                2.0 * s1,
            );
            let one_line = band(SEARCH_ROW_ONE_LINE, line(typography, TextRole::Body), s3);
            let two_line = band(
                SEARCH_ROW_TWO_LINE,
                line(typography, TextRole::Body) + s1 + line(typography, TextRole::Caption),
                s3,
            );
            assert!(
                header_h >= SEARCH_HEADER_H
                    && one_line >= SEARCH_ROW_ONE_LINE
                    && two_line >= SEARCH_ROW_TWO_LINE
            );
            if scale > 1.0 {
                assert!(two_line > SEARCH_ROW_TWO_LINE && one_line > SEARCH_ROW_ONE_LINE);
            }
            // Second row: "Alan Turing / Computing", a two-line block.
            let second_y0 = view.y0 + SEARCH_PAD + header_h + two_line;
            let second = Rect::new(
                view.x0 + SEARCH_TEXT_X,
                second_y0,
                view.x0 + 300.0,
                second_y0 + two_line,
            );
            let block = ink(&cap.image, second);
            // The block's ink (cap height to descender) sits within the
            // face's own ascent/descent, so its tolerance grows with the text.
            assert_centered("two-line block", block, second, 1.5 * f64::from(scale));
            assert_has_air("two-line block", block, second);
            // Third row: "Calculator", after two two-line rows and two headers.
            let row_y0 = second_y0 + two_line + header_h;
            let row = Rect::new(
                view.x0 + SEARCH_TEXT_X,
                row_y0,
                view.x0 + 300.0,
                row_y0 + one_line,
            );
            let title = ink(&cap.image, row);
            title_ink.push(title);
            assert_centered("single-line title", title, row, 1.0);
            assert_has_air("single-line title", title, row);
            let icon = Rect::new(
                view.x0 + 20.0,
                row_y0,
                view.x0 + SEARCH_TEXT_X,
                row_y0 + one_line,
            );
            assert_centered("row icon", ink(&cap.image, icon), icon, 1.5);
        }
        assert_scaled_once("search title", title_ink[0], title_ink[1]);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn card_headers_centre_on_their_icon_line_and_scale_once() {
    // Case `banner_text_block_centres_on_the_icon_line_and_scales_once`.
    {
        let mut text_ink = Vec::new();
        for scale in [1.0, 2.0] {
            let cap = capture("banners", banners(), (520, 360), scale);
            let snapshot = cap.harness.inspection();
            let banner = rect(&snapshot.root.children[0]);
            let text = Rect::new(
                banner.x0 + 44.0,
                banner.y0 + 1.0,
                banner.x1 - 40.0,
                banner.y1 - 1.0,
            );
            let block = ink(&cap.image, text);
            text_ink.push(block);
            // The band grows with the measured block, so the block keeps one
            // `s2` of air above and below at every scale.
            let air = f64::from(Theme::dark().tokens.spacing.s2) - 1.0;
            assert!(
                block.y0 - banner.y0 >= air && banner.y1 - block.y1 >= air,
                "{scale}x: banner text {block:?} needs {air} px of air inside {banner:?}"
            );
            if scale == 1.0 {
                assert_centered("title + message block", block, banner, 1.0);
                let icon = Rect::new(
                    banner.x0 + 12.0,
                    banner.y0 + 1.0,
                    banner.x0 + 36.0,
                    banner.y1 - 1.0,
                );
                assert_centered("banner icon", ink(&cap.image, icon), banner, 1.5);
            }
        }
        assert_scaled_once("banner text", text_ink[0], text_ink[1]);
    }

    // Case `stat_tile_label_sits_on_its_line_and_scales_once`.
    {
        let mut caption_ink = Vec::new();
        for scale in [1.0, 2.0] {
            let cap = capture("stat_tiles", stat_tiles(), (760, 160), scale);
            let tile = rect(find(&cap.harness.inspection().root, "StatTile"));
            let theme = Theme::dark().with_text_scale(scale);
            // The label is the tile's first line: a Caption line box at its top.
            let line = f64::from(theme.text(TextRole::Caption).line_height_px());
            let band = Rect::new(tile.x0, tile.y0, tile.x0 + 120.0, tile.y0 + line);
            let label = ink(&cap.image, band);
            assert_has_air("tile label", label, band);
            caption_ink.push(label);
        }
        assert_scaled_once("tile caption", caption_ink[0], caption_ink[1]);
    }

    // Case `chart_header_centres_title_and_legend_and_scales_once`.
    {
        let mut title_ink = Vec::new();
        for scale in [1.0, 2.0] {
            let cap = capture("chart", chart(), (480, 220), scale);
            let chart = rect(find(&cap.harness.inspection().root, "Chart"));
            // The header band holds the `sm` title line plus `s1`; the pinned
            // 22 px at 1x.
            let theme = Theme::dark().with_text_scale(scale);
            let header_h = band(
                CHART_HEADER_H,
                line(&theme.tokens.typography, TextRole::Caption),
                f64::from(theme.tokens.spacing.s1),
            );
            assert!(header_h >= CHART_HEADER_H);
            if scale > 1.0 {
                assert!(header_h > CHART_HEADER_H);
            }
            let header = Rect::new(chart.x0, chart.y0, chart.x0 + 160.0, chart.y0 + header_h);
            let title = ink(&cap.image, header);
            title_ink.push(title);
            assert_centered("chart title", title, header, 1.0);
            assert_has_air("chart title", title, header);
            let legend = Rect::new(chart.x1 - 60.0, chart.y0, chart.x1, chart.y0 + header_h);
            assert_centered("legend label", ink(&cap.image, legend), legend, 1.0);
        }
        assert_scaled_once("chart title", title_ink[0], title_ink[1]);
    }

    // Case `section_titles_use_the_heading_role_and_the_related_gap`.
    {
        let mut title_heights = Vec::new();
        for scale in [1.0_f32, 2.0] {
            let cap = capture("legacy_cards", legacy_cards(), (520, 320), scale);
            let snapshot = cap.harness.inspection();
            // Section -> [header row, body]; the Text inside the header row is
            // the heading.
            let section = &snapshot.root.children[0];
            let title_row = rect(&section.children[0]);
            let body = rect(&section.children[1]);
            let title = rect(find(&section.children[0], "Text"));
            title_heights.push(title.height());
            let theme = Theme::dark().with_text_scale(scale);
            let expected = f64::from(theme.text(TextRole::Heading).line_height_px());
            assert!(
                (title.height() - expected).abs() < 1.0,
                "{scale}x: title line box {} must be the Heading role's {expected}",
                title.height()
            );
            let related = f64::from(theme.tokens.spacing.rhythm.related);
            assert!(
                (body.y0 - title_row.y1 - related).abs() < 0.5,
                "{scale}x: title/body gap {} must be rhythm.related {related}",
                body.y0 - title_row.y1,
            );
        }
        assert!(
            (title_heights[1] - 2.0 * title_heights[0]).abs() < 1.5,
            "title {} -> {} must follow the text scale",
            title_heights[0],
            title_heights[1]
        );
    }
}
