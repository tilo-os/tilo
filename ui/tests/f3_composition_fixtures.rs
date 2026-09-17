//! F3 composition/data-display fixtures: renders the immediate-mode data
//! widgets through the shared `FrameHarness` at 1x and 2x text scale and
//! checks the alignment contract each widget paints against, on the pixels
//! it actually produced.
//!
//! PNGs land in `target/ui-review/ui/f3_fixture/<F3_FIXTURE_TAG>/` so the
//! same run doubles as visual evidence; the assertions are the regression.
//!
//! Two properties are checked per widget:
//! - at 1x, a single line of descender-free text sits on the centre line of
//!   its band (its ink centre is within a pixel of the band centre, which the
//!   old `(band - font_size) / 2` placement missed by ~1.5 px);
//! - the ink height grows by ~2x between 1x and 2x text scale. The widgets
//!   used to grow by 4x because they fed already-scaled theme sizes to the
//!   renderer, which scales once more.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ui::State;
use ui::headless::{FrameHarness, InspectionNode};
use ui::icons::Icon;
use ui::search::{SearchGroup, SearchResult};
use ui::theme::Theme;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::{
    Alternative, Banner, Button, Card, Chart, Column, ConflictBanner, ConflictKind, ConflictPicker,
    Container, Element, IconGrid, IconGridItem, Row, SearchResultsView, Series, StatTile, Text,
    TreeNode, TreeView, Trend,
};
use vello::kurbo::Rect;

// Pinned widget defaults the regions below are built from.
const TABLE_HEADER_H: f64 = 28.0;
const TABLE_ROW_H: f64 = 24.0;
const TREE_ROW_H: f64 = 22.0;
const TREE_DISCLOSURE_W: f64 = 16.0;
const SEARCH_PAD: f64 = 8.0;
const SEARCH_HEADER_H: f64 = 26.0;
const SEARCH_ROW_ONE_LINE: f64 = 38.0;
const SEARCH_ROW_TWO_LINE: f64 = 50.0;
const SEARCH_TEXT_X: f64 = 8.0 + 12.0 + 30.0;
const CHART_HEADER_H: f64 = 22.0;
const ICON_CELL: (f64, f64) = (96.0, 88.0);
const TILE_PAD: f64 = 14.0;
const TILE_ICON_D: f64 = 18.0;
const PICKER_PAD: f64 = 16.0;
const PICKER_TITLE_H: f64 = 28.0;

fn out_dir() -> PathBuf {
    let tag = std::env::var("F3_FIXTURE_TAG").unwrap_or_else(|_| "current".into());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-review/ui/f3_fixture")
        .join(tag);
    std::fs::create_dir_all(&dir).expect("fixture output directory");
    dir
}

struct Capture {
    harness: FrameHarness,
    image: image::RgbaImage,
}

fn capture(name: &str, root: Element, viewport: (u32, u32), scale: f32) -> Capture {
    let harness = FrameHarness::new_with_theme(
        State::new(),
        root,
        viewport,
        Theme::dark().with_text_scale(scale),
    );
    let path = out_dir().join(format!("{name}-{scale}x.png"));
    harness.render_window_png(&path);
    let image = image::open(&path)
        .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()))
        .to_rgba8();
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
            TreeNode::branch(
                2,
                "src",
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
                SearchResult::new("Contacts", "Alan Turing", "b")
                    .with_subtitle("Computing")
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
    Box::new(
        Row::new()
            .push(Box::new(
                StatTile::new("Sessions", "1,284")
                    .with_icon(Icon::Search)
                    .with_delta("12%", Trend::Up),
            ))
            .push(Box::new(
                StatTile::new("Errors", "3")
                    .with_icon(Icon::Inbox)
                    .with_delta("2", Trend::Down),
            ))
            .push(Box::new(
                StatTile::new("Latency", "48 ms").with_delta("0%", Trend::Flat),
            ))
            .push(Box::new(
                StatTile::new("Uptime", "99.9%").with_icon(Icon::Refresh),
            )),
    )
}

fn chart() -> Element {
    let theme = Theme::dark();
    Box::new(
        Chart::new(vec![
            Series::new(
                vec![1.0, 3.0, 2.0, 5.0, 4.0, 6.0, 5.5],
                theme.accent_primary,
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

fn icon_grid() -> Element {
    Box::new(IconGrid::new(vec![
        IconGridItem::new(Icon::Folder, "Documents"),
        IconGridItem::new(Icon::Folder, "Downloads"),
        IconGridItem::new(Icon::Image, "holiday-photo-2026.png"),
        IconGridItem::new(Icon::FileText, "notes.md"),
        IconGridItem::new(Icon::Music, "song.flac"),
        IconGridItem::new(Icon::File, "archive.tar"),
    ]))
}

fn conflict() -> Element {
    fn alt(label: &str, summary: &str, author: &str, secs_ago: u64) -> Alternative {
        Alternative {
            label: label.into(),
            summary: summary.into(),
            author: author.into(),
            timestamp: SystemTime::now()
                .checked_sub(Duration::from_secs(secs_ago))
                .unwrap_or_else(SystemTime::now),
            payload: Arc::from(Vec::<u8>::new()),
        }
    }
    let mut banner = ConflictBanner::new("Kanban move");
    banner.set_conflict_count(2);
    let picker = ConflictPicker::new(
        "Kanban move",
        ConflictKind::PickOne {
            alternatives: vec![
                alt(
                    "Move to Done",
                    "Alice marked the card finished",
                    "Alice",
                    120,
                ),
                alt("Move to In Review", "Bob wants Carol to approve", "Bob", 60),
            ],
        },
    );
    Box::new(
        Column::new()
            .with_no_padding()
            .with_gap(8.0)
            .push(Box::new(banner))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_height(320.0)
                    .with_child(Box::new(picker)),
            )),
    )
}

fn legacy_cards() -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(12.0)
            .push(Box::new(Card::titled(
                "Region formats",
                Box::new(Text::new(
                    "Dates, times and numbers follow the selected region.",
                )),
            )))
            .push(Box::new(Card::titled_with_action(
                "Email",
                Box::new(Button::new("add-account", "Add account", None)),
                Box::new(Text::new("No email accounts yet.")),
            ))),
    )
}

#[test]
fn data_table_header_and_rows_centre_text_and_scale_once() {
    let mut header_ink = Vec::new();
    for scale in [1.0, 2.0] {
        let cap = capture("data_table", data_table(), (440, 260), scale);
        let table = rect(find(&cap.harness.inspection().root, "DataTable"));
        // First column only: the sort glyph lives at the column's trailing edge.
        let header = Rect::new(
            table.x0 + 4.0,
            table.y0,
            table.x0 + 180.0,
            table.y0 + TABLE_HEADER_H,
        );
        let probe = Rect::new(
            header.x0,
            header.y0,
            header.x1,
            header.y0 + TABLE_HEADER_H + 4.0,
        );
        header_ink.push(ink(&cap.image, probe));
        if scale == 1.0 {
            assert_centered("header label", ink(&cap.image, header), header, 1.0);
            let arrow = Rect::new(
                table.x0 + 180.0,
                table.y0,
                table.x0 + 220.0,
                table.y0 + TABLE_HEADER_H,
            );
            assert_centered("sort glyph", ink(&cap.image, arrow), arrow, 2.0);
            let row = Rect::new(header.x0, header.y1, header.x1, header.y1 + TABLE_ROW_H);
            assert_centered("first row cell", ink(&cap.image, row), row, 1.0);
        }
    }
    assert_scaled_once("data table header", header_ink[0], header_ink[1]);
}

#[test]
fn tree_view_rows_centre_labels_and_scale_once() {
    let mut label_ink = Vec::new();
    for scale in [1.0, 2.0] {
        let cap = capture("tree_view", tree_view(), (320, 220), scale);
        let tree = rect(find(&cap.harness.inspection().root, "TreeView"));
        let row = Rect::new(
            tree.x0 + TREE_DISCLOSURE_W,
            tree.y0,
            tree.x0 + 200.0,
            tree.y0 + TREE_ROW_H,
        );
        let probe = Rect::new(row.x0, row.y0, row.x1, row.y0 + TREE_ROW_H + 6.0);
        label_ink.push(ink(&cap.image, probe));
        if scale == 1.0 {
            assert_centered("root label", ink(&cap.image, row), row, 1.0);
            let glyph = Rect::new(
                tree.x0,
                tree.y0,
                tree.x0 + TREE_DISCLOSURE_W,
                tree.y0 + TREE_ROW_H,
            );
            assert_centered("disclosure glyph", ink(&cap.image, glyph), glyph, 2.0);
        }
    }
    assert_scaled_once("tree label", label_ink[0], label_ink[1]);
}

#[test]
fn search_results_centre_single_line_titles_and_scale_once() {
    let mut title_ink = Vec::new();
    for scale in [1.0, 2.0] {
        let cap = capture("search_results", search_results(), (420, 360), scale);
        let view = rect(find(&cap.harness.inspection().root, "SearchResultsView"));
        // Third row: "Calculator", after two two-line rows and two headers.
        let row_y0 =
            view.y0 + SEARCH_PAD + SEARCH_HEADER_H + 2.0 * SEARCH_ROW_TWO_LINE + SEARCH_HEADER_H;
        let row = Rect::new(
            view.x0 + SEARCH_TEXT_X,
            row_y0,
            view.x0 + 300.0,
            row_y0 + SEARCH_ROW_ONE_LINE,
        );
        title_ink.push(ink(&cap.image, row));
        if scale == 1.0 {
            assert_centered("single-line title", ink(&cap.image, row), row, 1.0);
            let icon = Rect::new(
                view.x0 + 20.0,
                row_y0,
                view.x0 + SEARCH_TEXT_X,
                row_y0 + SEARCH_ROW_ONE_LINE,
            );
            assert_centered("row icon", ink(&cap.image, icon), icon, 1.5);
        }
    }
    assert_scaled_once("search title", title_ink[0], title_ink[1]);
}

#[test]
fn banner_text_block_centres_on_the_icon_line_and_scales_once() {
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
        text_ink.push(ink(&cap.image, text));
        if scale == 1.0 {
            assert_centered("title + message block", ink(&cap.image, text), banner, 1.0);
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

#[test]
fn stat_tile_icon_sits_on_the_caption_line_and_scales_once() {
    let mut caption_ink = Vec::new();
    for scale in [1.0, 2.0] {
        let cap = capture("stat_tiles", stat_tiles(), (760, 160), scale);
        let tile = rect(find(&cap.harness.inspection().root, "StatTile"));
        // The caption line only: the 24 px value starts at PAD + 12 + 6 at 1x.
        let caption_band = if scale == 1.0 {
            Rect::new(
                tile.x0,
                tile.y0 + TILE_PAD - 2.0,
                tile.x1,
                tile.y0 + TILE_PAD + 17.0,
            )
        } else {
            Rect::new(
                tile.x0,
                tile.y0 + TILE_PAD - 2.0,
                tile.x1,
                tile.y0 + TILE_PAD + 30.0,
            )
        };
        let caption = Rect::new(
            tile.x0 + TILE_PAD,
            caption_band.y0,
            tile.x0 + 100.0,
            caption_band.y1,
        );
        caption_ink.push(ink(&cap.image, caption));
        if scale == 1.0 {
            let icon = Rect::new(
                tile.x1 - TILE_PAD - TILE_ICON_D - 2.0,
                caption_band.y0,
                tile.x1 - TILE_PAD + 2.0,
                caption_band.y1,
            );
            let caption_ink = ink(&cap.image, caption);
            let icon_ink = ink(&cap.image, icon);
            let offset = icon_ink.center().y - caption_ink.center().y;
            assert!(
                offset.abs() <= 1.0,
                "tile icon {icon_ink:?} is {offset:+.2} px off the caption line {caption_ink:?}"
            );
        }
    }
    assert_scaled_once("tile caption", caption_ink[0], caption_ink[1]);
}

#[test]
fn chart_header_centres_title_and_legend_and_scales_once() {
    let mut title_ink = Vec::new();
    for scale in [1.0, 2.0] {
        let cap = capture("chart", chart(), (480, 220), scale);
        let chart = rect(find(&cap.harness.inspection().root, "Chart"));
        let header = Rect::new(
            chart.x0,
            chart.y0,
            chart.x0 + 160.0,
            chart.y0 + CHART_HEADER_H,
        );
        let probe = Rect::new(
            header.x0,
            header.y0,
            header.x1,
            header.y0 + CHART_HEADER_H + 8.0,
        );
        title_ink.push(ink(&cap.image, probe));
        if scale == 1.0 {
            assert_centered("chart title", ink(&cap.image, header), header, 1.0);
            let legend = Rect::new(
                chart.x1 - 60.0,
                chart.y0,
                chart.x1,
                chart.y0 + CHART_HEADER_H,
            );
            assert_centered("legend label", ink(&cap.image, legend), legend, 1.0);
        }
    }
    assert_scaled_once("chart title", title_ink[0], title_ink[1]);
}

#[test]
fn icon_grid_centres_glyph_and_label_block_in_the_cell_and_scales_once() {
    let mut label_ink = Vec::new();
    for scale in [1.0, 2.0] {
        let cap = capture("icon_grid", icon_grid(), (440, 240), scale);
        let grid = rect(find(&cap.harness.inspection().root, "IconGrid"));
        let cell = Rect::new(
            grid.x0,
            grid.y0,
            grid.x0 + ICON_CELL.0,
            grid.y0 + ICON_CELL.1,
        );
        // The label lives under the 40 px glyph, whose bottom edge is at most
        // 52.5 px into the cell once the block is centred; probe below it.
        let label = Rect::new(cell.x0, cell.y0 + 54.0, cell.x1, cell.y1);
        label_ink.push(ink(&cap.image, label));
        if scale == 1.0 {
            assert_centered("glyph + label block", ink(&cap.image, cell), cell, 1.5);
            let glyph_and_label = ink(&cap.image, cell);
            let drift = glyph_and_label.center().x - cell.center().x;
            assert!(
                drift.abs() <= 1.5,
                "cell content {glyph_and_label:?} is {drift:+.2} px off centre"
            );
        }
    }
    assert_scaled_once("icon label", label_ink[0], label_ink[1]);
}

#[test]
fn conflict_surfaces_centre_the_title_and_scale_once() {
    let mut title_ink = Vec::new();
    for scale in [1.0, 2.0] {
        let cap = capture("conflict", conflict(), (820, 380), scale);
        let picker = rect(find(&cap.harness.inspection().root, "ConflictPicker"));
        let title = Rect::new(
            picker.x0 + PICKER_PAD,
            picker.y0 + PICKER_PAD,
            picker.x0 + 320.0,
            picker.y0 + PICKER_PAD + PICKER_TITLE_H,
        );
        let probe = Rect::new(title.x0, title.y0 - 2.0, title.x1, title.y1 + 8.0);
        title_ink.push(ink(&cap.image, probe));
        if scale == 1.0 {
            assert_centered("picker title", ink(&cap.image, title), title, 1.0);
            let banner = rect(find(&cap.harness.inspection().root, "ConflictBanner"));
            let label = Rect::new(banner.x0 + 36.0, banner.y0, banner.x0 + 300.0, banner.y1);
            assert_centered("conflict banner label", ink(&cap.image, label), banner, 1.0);
        }
    }
    assert_scaled_once("picker title", title_ink[0], title_ink[1]);
}

#[test]
fn legacy_card_titles_use_the_title_role_and_the_theme_gap() {
    let mut title_heights = Vec::new();
    for scale in [1.0_f32, 2.0] {
        let cap = capture("legacy_cards", legacy_cards(), (520, 320), scale);
        let snapshot = cap.harness.inspection();
        // Card -> Container -> Column[title Row, body]; the Text inside the
        // title Row is the heading.
        let card = &snapshot.root.children[0];
        let column = &card.children[0].children[0];
        let title_row = rect(&column.children[0]);
        let body = rect(&column.children[1]);
        let title = rect(find(&column.children[0], "Text"));
        title_heights.push(title.height());
        let theme = Theme::dark().with_text_scale(scale);
        let typography = &theme.tokens.typography;
        let expected = f64::from(typography.scale.lg * typography.line_height_tight);
        assert!(
            (title.height() - expected).abs() < 1.0,
            "{scale}x: title line box {} must be the Title role's {expected}",
            title.height()
        );
        assert!(
            (body.y0 - title_row.y1 - f64::from(theme.gap_standard)).abs() < 0.5,
            "{scale}x: title/body gap {} must be theme.gap_standard {}",
            body.y0 - title_row.y1,
            theme.gap_standard
        );
    }
    assert!(
        (title_heights[1] - 2.0 * title_heights[0]).abs() < 1.5,
        "title {} -> {} must follow the text scale",
        title_heights[0],
        title_heights[1]
    );
}
