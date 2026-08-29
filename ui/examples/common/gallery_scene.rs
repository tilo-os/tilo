//! Shared builder for the comprehensive component showcase used by both the
//! interactive `gallery` example (live window) and the `gallery_shots` example
//! (offscreen PNGs, light + dark). Kept in `examples/common/` so Cargo does not
//! auto-build it as its own example; each example `#[path]`-includes it.
//!
//! The single entry point, [`build_showcase`], seeds the CRDT document with the
//! state the interactive controls read (toggles, checkbox, slider, text field,
//! dropdown, table) and returns the root widget: a vertical column of titled
//! cards, one per component family.

#![allow(dead_code)]

use automerge::transaction::Transactable;

use ui::State;
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::table;
use ui::widget::toggle_group::Segment;
use ui::widget::toolbar::Orientation;
use ui::widget::{
    Avatar, Badge, Banner, BarGauge, Breadcrumb, Button, Chart, Checkbox, Chip, Column, Container,
    Divider, Dropdown, EmptyState, NumberInput, ProgressBar, RingGauge, Row, SearchField, Series,
    Slider, Spacer, Sparkline, StatTile, TabBar, TabDescriptor, Table, Text, TextInput, Toggle,
    ToggleGroup, Toolbar, Trend, Widget,
};

const CARD_W: f32 = 560.0;

fn title(t: &str) -> Box<dyn Widget> {
    Box::new(Text::new(t).with_size(20.0))
}

fn label(t: &str) -> Box<dyn Widget> {
    Box::new(Text::new(t).with_size(14.0))
}

fn dim(t: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(t).with_size(13.0).with_color(theme.text_dim))
}

/// A titled card: heading + divider + body, on a raised background.
fn card(width: Option<f32>, heading: &str, body: Box<dyn Widget>) -> Box<dyn Widget> {
    let inner = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(12.0)
        .push(Box::new(Text::new(heading).with_size(15.0)))
        .push(Box::new(Divider::horizontal()))
        .push(body);
    let mut c = Container::new()
        .with_background()
        .with_child(Box::new(inner));
    if let Some(w) = width {
        c = c.with_width(w);
    }
    Box::new(c)
}

fn col(gap: f32) -> Column {
    Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(gap)
}

/// Build the whole showcase, seeding `state.doc`. `theme` selects dimmed-text
/// colours so a per-theme rebuild renders correctly in both light and dark.
pub fn build_showcase(state: &mut State, theme: &Theme) -> Box<dyn Widget> {
    // ---- Seed interactive state -------------------------------------------
    let root = state.root.clone();
    let _ = state.doc.put(&root, "sw_wifi", true);
    let _ = state.doc.put(&root, "sw_bt", false);
    let _ = state.doc.put(&root, "cb_updates", true);
    let _ = state.doc.put(&root, "sld_vol", 0.65);
    let _ = state.doc.put(&root, "dd_quality", "High");
    let ti = state
        .doc
        .put_object(&root, "ti_name", automerge::ObjType::Text)
        .unwrap();
    let _ = state.doc.splice_text(&ti, 0, 0, "Ada Lovelace");

    // Editable table model (lives directly under root, like the old gallery).
    for _ in 0..3 {
        table::insert_column(&mut state.doc, &root, usize::MAX);
    }
    for _ in 0..4 {
        table::insert_row(&mut state.doc, &root, usize::MAX);
    }
    table::set_header(&mut state.doc, &root, true);
    let rows = [
        ["Task", "Owner", "Status"],
        ["Design", "Ada", "Done"],
        ["Build", "Grace", "Active"],
        ["Ship", "Linus", "Queued"],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            table::set_cell_text(&mut state.doc, &root, r, c, cell);
        }
    }

    let accent = theme.accent_primary;

    // ---- Header -----------------------------------------------------------
    let header = col(4.0).push(title("qOS UI Toolkit")).push(dim(
        "Component gallery — one showcase of every general-purpose widget, light & dark",
        theme,
    ));

    // ---- Row 1: controls | inputs -----------------------------------------
    let controls = card(
        Some(CARD_W),
        "Buttons & Controls",
        Box::new(
            col(12.0)
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Button::new("g_save", "Save", None)))
                        .push(Box::new(Button::new("g_cancel", "Discard", None))),
                ))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Toggle::new("sw_wifi", "sw_wifi")))
                        .push(label("Wi-Fi"))
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(Toggle::new("sw_bt", "sw_bt")))
                        .push(label("Bluetooth")),
                ))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Checkbox::new("cb_updates", "cb_updates")))
                        .push(label("Install updates automatically")),
                ))
                .push(dim("Volume", theme))
                .push(Box::new(Slider::new("sld_vol", "sld_vol")))
                .push(Box::new(ToggleGroup::new(
                    vec![
                        Segment::new("Day"),
                        Segment::new("Week"),
                        Segment::new("Month"),
                    ],
                    1,
                ))),
        ),
    );

    let inputs = card(
        Some(CARD_W),
        "Text & Selection",
        Box::new(
            col(12.0)
                .push(dim("Text field", theme))
                .push(Box::new(
                    TextInput::new(vec![])
                        .with_id("g_name")
                        .with_on_change_path("ti_name"),
                ))
                .push(dim("Search field", theme))
                .push(Box::new(
                    SearchField::new().with_placeholder("Search components"),
                ))
                .push(dim("Dropdown", theme))
                .push(Box::new(
                    Dropdown::new(
                        vec!["Low".into(), "Medium".into(), "High".into()],
                        "dd_quality",
                    )
                    .with_placeholder("Quality"),
                ))
                .push(dim("Stepper & breadcrumb", theme))
                .push(Box::new(
                    NumberInput::new(12.0)
                        .with_range(0.0, 100.0)
                        .with_suffix(" px"),
                ))
                .push(Box::new(Breadcrumb::new(vec![
                    "Home".into(),
                    "Projects".into(),
                    "qOS".into(),
                    "ui".into(),
                ]))),
        ),
    );

    // ---- Row 2: tags | feedback -------------------------------------------
    let tags = card(
        Some(CARD_W),
        "Badges, Chips & Avatars",
        Box::new(
            col(12.0)
                .push(dim("Badges", theme))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Badge::pill("NEW", accent)))
                        .push(Box::new(Badge::text("β")))
                        .push(Box::new(Badge::count(3)))
                        .push(Box::new(Badge::count(128)))
                        .push(Box::new(Badge::dot())),
                ))
                .push(dim("Chips — plain, selected, selectable, removable", theme))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Chip::new("Design").with_dot(theme.tokens.colors.semantic.info),
                        ))
                        .push(Box::new(Chip::new("Rust").selected(true)))
                        .push(Box::new(Chip::new("Filter").selectable()))
                        .push(Box::new(Chip::new("alice@example.com").removable())),
                ))
                .push(dim("Avatars", theme))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Avatar::from_initials("AL", 34.0)))
                        .push(Box::new(Avatar::from_initials("GR", 34.0)))
                        .push(Box::new(Avatar::from_initials("LT", 34.0)))
                        .push(Box::new(Avatar::from_initials("DB", 34.0))),
                )),
        ),
    );

    let feedback = card(
        Some(CARD_W),
        "Feedback & Loading",
        Box::new(
            col(14.0)
                .push(dim("Determinate progress", theme))
                .push(Box::new(
                    ProgressBar::new(0.65).with_label("Downloading update"),
                ))
                .push(dim("Indeterminate progress", theme))
                .push(Box::new(ProgressBar::indeterminate()))
                .push(dim("Spinner", theme))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(ui::widget::Spinner::with_size(28.0)))
                        .push(label("Loading…")),
                )),
        ),
    );

    // ---- Metrics (full width) ---------------------------------------------
    let metrics = card(
        None,
        "Metrics",
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(
                    StatTile::new("Requests", "1,284")
                        .with_icon(Icon::BarChart)
                        .with_delta("12%", Trend::Up),
                ))
                .push(Box::new(
                    StatTile::new("Errors", "3")
                        .with_icon(Icon::Ban)
                        .with_value_color(theme.tokens.colors.semantic.danger)
                        .with_delta("2", Trend::Down),
                ))
                .push(Box::new(
                    StatTile::new("Latency", "48 ms")
                        .with_icon(Icon::Refresh)
                        .with_delta("0%", Trend::Flat),
                ))
                .push(Box::new(
                    StatTile::new("Uptime", "99.9%")
                        .with_icon(Icon::Check)
                        .with_value_color(theme.tokens.colors.semantic.success),
                )),
        ),
    );

    // ---- Row 3: charts | navigation ---------------------------------------
    let charts = card(
        Some(CARD_W),
        "Charts & Gauges",
        Box::new(
            col(12.0)
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            RingGauge::new(0.72)
                                .with_label("72%", "CPU")
                                .with_size(110.0),
                        ))
                        .push(Box::new(
                            col(8.0).push(dim("Throughput", theme)).push(Box::new(
                                Sparkline::new(
                                    vec![4.0, 7.0, 3.0, 8.0, 6.0, 9.0, 5.0, 11.0, 8.0, 12.0],
                                    accent,
                                )
                                .with_size(200.0, 52.0),
                            )),
                        )),
                ))
                .push(Box::new(BarGauge::new(0.4).with_label("Disk", "40%")))
                .push(Box::new(
                    Chart::new(vec![Series::new(
                        vec![2.0, 5.0, 4.0, 7.0, 6.0, 9.0, 8.0, 11.0],
                        accent,
                    )])
                    .with_area(true)
                    .with_height(96.0),
                )),
        ),
    );

    let navigation = card(
        Some(CARD_W),
        "Navigation & Toolbar",
        Box::new(
            col(12.0)
                .push(dim("Tabs", theme))
                .push(Box::new(TabBar::new(
                    "g_tabs",
                    vec![
                        TabDescriptor::new(1, "Overview"),
                        TabDescriptor::new(2, "Activity"),
                        TabDescriptor::new(3, "Settings"),
                    ],
                    1,
                )))
                .push(dim("Toolbar", theme))
                .push(Box::new(
                    Toolbar::new(Orientation::Horizontal)
                        .push_button("Bold", || {})
                        .push_button("Italic", || {})
                        .push_separator()
                        .push_button("Link", || {})
                        .push_button("Code", || {}),
                )),
        ),
    );

    // ---- Banners (full width) ---------------------------------------------
    let banners = card(
        None,
        "Inline Banners",
        Box::new(
            col(10.0)
                .push(Box::new(
                    Banner::info("A new version of qOS is available.")
                        .with_message("Restart to finish updating."),
                ))
                .push(Box::new(Banner::success("Your changes were saved.")))
                .push(Box::new(
                    Banner::warning("You're offline.")
                        .with_message("Edits will sync when you reconnect.")
                        .dismissible(),
                ))
                .push(Box::new(
                    Banner::error("Upload failed.")
                        .with_message("Check your connection and try again.")
                        .dismissible(),
                )),
        ),
    );

    // ---- Empty state (full width, fixed height) ---------------------------
    let empty = card(
        None,
        "Empty State",
        Box::new(
            Container::new()
                .with_no_padding()
                .with_height(210.0)
                .with_child(Box::new(
                    EmptyState::new("No messages yet")
                        .with_icon(Icon::Inbox)
                        .with_message("When you receive mail, it will show up here.")
                        .with_action("Compose"),
                )),
        ),
    );

    // ---- Data table (full width) ------------------------------------------
    let table_card = card(
        None,
        "Editable Table",
        Box::new(Table::new("gallery_table")),
    );

    // ---- Assemble ---------------------------------------------------------
    Box::new(
        Column::new()
            .with_gap(16.0)
            .push(Box::new(header))
            .push(Box::new(Row::new().push(controls).push(inputs)))
            .push(Box::new(Row::new().push(tags).push(feedback)))
            .push(metrics)
            .push(Box::new(Row::new().push(charts).push(navigation)))
            .push(banners)
            .push(empty)
            .push(table_card),
    )
}
