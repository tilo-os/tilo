//! Shared builder for the comprehensive component showcase used by both the
//! interactive `gallery` example (live window) and the `gallery_shots` example
//! (offscreen PNGs, light + dark). Kept in `examples/common/` so Cargo does not
//! auto-build it as its own example; each example `#[path]`-includes it.
//!
//! The single entry point, [`build_showcase`], seeds the CRDT document with the
//! state the interactive controls read (toggles, checkbox, slider, text field,
//! dropdown, data table) and returns the root widget: a vertical column of titled
//! cards, one per component family.

use automerge::transaction::Transactable;

use tilo_ui::icons::Icon;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::TextRole;
use tilo_ui::widget::table;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::toolbar::Orientation;
use tilo_ui::widget::{
    Avatar, AvatarSize, Badge, Banner, BarGauge, Breadcrumb, Button, ButtonRole, Chart, Checkbox,
    Chip, ChipLead, ChipStyle, Column, Container, Divider, Dropdown, EmptyState, GaugeThresholds,
    NumberInput, ProgressBar, ProgressRing, Row, SearchField, Series, Slider, Spacer, Sparkline,
    StatGroup, StatTile, TabBar, TabDescriptor, Text, TextInput, Toggle, ToggleGroup, Tone,
    Toolbar, Trend, Widget,
};
use tilo_ui::{
    FontChoice, HangingPunctuation, NumericStyle, OpenTypeTag, OpticalSizing, ParagraphAlign, State,
};

const CARD_W: f32 = 560.0;

fn title(t: &str) -> Box<dyn Widget> {
    Box::new(Text::new(t).with_role(TextRole::Title))
}

fn label(t: &str) -> Box<dyn Widget> {
    Box::new(Text::new(t))
}

fn dim(t: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Text::new(t)
            .with_role(TextRole::Caption)
            .with_color(theme.text_dim),
    )
}

/// A titled card: heading + divider + body, on a raised background.
fn card(width: Option<f32>, heading: &str, body: Box<dyn Widget>) -> Box<dyn Widget> {
    let inner = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(12.0)
        .push(Box::new(Text::new(heading).with_role(TextRole::Heading)))
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

fn specimen_cell(caption: &str, specimen: Text, theme: &Theme, width: f32) -> Box<dyn Widget> {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(width)
            .with_child(Box::new(
                col(4.0).push(dim(caption, theme)).push(Box::new(specimen)),
            )),
    )
}

fn specimen_pair(
    theme: &Theme,
    before_caption: &str,
    before: Text,
    after_caption: &str,
    after: Text,
) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(specimen_cell(before_caption, before, theme, 520.0))
            .push(specimen_cell(after_caption, after, theme, 520.0)),
    )
}

/// The gallery's canonical multilingual typography review corpus.
///
/// Kept as its own builder so the offscreen regression test renders exactly
/// the same widget developers inspect in the full component gallery.
/// The role ladder and the serif reading specimens. Split out of
/// `build_typography_showcase` to keep that under the line cap.
fn typography_intro_and_serif(column: Column, _theme: &Theme) -> Column {
    column
        .push(Box::new(
            Text::new("A clear hierarchy makes dense interfaces feel calm.")
                .with_role(TextRole::LargeTitle),
        ))
        .push(Box::new(
            Text::new("Every role derives from one body size").with_role(TextRole::Title),
        ))
        .push(Box::new(
            Text::new("Section heading").with_role(TextRole::Heading),
        ))
        .push(Box::new(
            Text::new("Group label inside a section").with_role(TextRole::Subheading),
        ))
        .push(Box::new(
            Text::new("Body text is the default: what most of the interface says.")
                .with_role(TextRole::Body),
        ))
        .push(Box::new(
            Text::new("Control label, or a row title in a dense list").with_role(TextRole::Label),
        ))
        .push(Box::new(
            Text::new("Caption: a secondary line, a timestamp, metadata.")
                .with_role(TextRole::Caption),
        ))
        .push(Box::new(
            Text::new(
                "Serif reading — AVATAR · WAVE · To · Ta · Yo · office · affinity · ffi · naïve café.",
            )
            .with_role(TextRole::Reading),
        ))
        .push(Box::new(
            Text::new("Italic serif — Typography should feel composed, never merely typeset.")
                .with_role(TextRole::Reading)
                .with_italic(true),
        ))
}

/// BEFORE/AFTER typography-feature specimen pairs (kerning through
/// variable-font optical sizing), all on the reading role. Split out of
/// `build_typography_showcase` to keep that under the line cap.
fn typography_feature_pairs(column: Column, theme: &Theme) -> Column {
    let reading = || TextRole::Reading;
    column
        .push(specimen_pair(
            theme,
            "Before: kerning disabled",
            Text::new("AVATAR  WAVE  To Ta Yo")
                .with_role(reading())
                .with_kerning(false),
            "After: kerning enabled",
            Text::new("AVATAR  WAVE  To Ta Yo")
                .with_role(reading())
                .with_kerning(true),
        ))
        .push(specimen_pair(
            theme,
            "Before: standard forms",
            Text::new("office · affinity · historic 1847").with_role(reading()),
            "After: discretionary ligatures",
            Text::new("office · affinity · historic 1847")
                .with_role(reading())
                .with_discretionary_ligatures(true),
        ))
        .push(specimen_pair(
            theme,
            "Before: proportional lining figures",
            Text::new("1847  2026  €12,408.50")
                .with_role(reading())
                .with_numeric_style(NumericStyle::ProportionalLining),
            "After: tabular old-style figures",
            Text::new("1847  2026  €12,408.50")
                .with_role(reading())
                .with_numeric_style(NumericStyle::TabularOldstyle),
        ))
        .push(specimen_pair(
            theme,
            "Before: Adwaita Sans wght 300 · opsz 14",
            Text::new("Variable fonts adapt continuously")
                .with_role(TextRole::Title)
                .with_font_family(FontChoice::Named("Adwaita Sans".into()))
                .with_font_variation(OpenTypeTag::WEIGHT, 300.0)
                .with_optical_sizing(OpticalSizing::Value(14.0)),
            "After: Adwaita Sans wght 720 · optical sizing auto",
            Text::new("Variable fonts adapt continuously")
                .with_role(TextRole::Title)
                .with_font_family(FontChoice::Named("Adwaita Sans".into()))
                .with_font_variation(OpenTypeTag::WEIGHT, 720.0)
                .with_optical_sizing(OpticalSizing::Auto),
        ))
}

/// The remaining BEFORE/AFTER specimen pairs (wrapping through hanging
/// punctuation), plus the pagination note. Split out of
/// `build_typography_showcase` to keep that under the line cap.
fn typography_wrapping_and_punctuation(column: Column, theme: &Theme) -> Column {
    column
        .push(specimen_pair(
            theme,
            "Before: ordinary wrapping",
            Text::new("A balanced heading wraps its thought into lines of comparable visual weight.")
                .with_role(TextRole::Title),
            "After: balanced wrapping",
            Text::new("A balanced heading wraps its thought into lines of comparable visual weight.")
                .with_role(TextRole::Title)
                .with_balanced_wrap(),
        ))
        .push(specimen_pair(
            theme,
            "Before: ragged start",
            Text::new(
                "Good typography makes a paragraph feel even before it is read. Spacing should support the argument without calling attention to itself.",
            )
            .with_role(TextRole::Reading),
            "After: full justification",
            Text::new(
                "Good typography makes a paragraph feel even before it is read. Spacing should support the argument without calling attention to itself.",
            )
            .with_role(TextRole::Reading)
            .with_align(ParagraphAlign::Justified),
        ))
        .push(specimen_pair(
            theme,
            "Before: punctuation inside the measure",
            Text::new("“A composed edge,” she wrote, “lets the words lead.”")
                .with_role(TextRole::Reading),
            "After: opening and closing punctuation hangs",
            Text::new("“A composed edge,” she wrote, “lets the words lead.”")
                .with_role(TextRole::Reading)
                .with_hanging_punctuation(HangingPunctuation::BOTH),
        ))
        .push(dim(
            "Pagination: RichTextLayout enforces configurable two-line widow and orphan minima at page and column breaks.",
            theme,
        ))
}

/// Code sample, multilingual scripts, hyphenation demo, `BiDi` text and
/// emoji clusters. Split out of `build_typography_showcase` to keep that
/// under the line cap.
fn typography_scripts_and_bidi(column: Column, _theme: &Theme) -> Column {
    column
                .push(Box::new(
                    Text::new("const greeting = \"Hello, 世界\";")
                        .with_role(TextRole::Code),
                ))
                .push(Box::new(
                    Text::new("العربية — مرحبًا بالعالم").with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("עברית — שלום עולם").with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("हिन्दी — नमस्ते दुनिया  ·  বাংলা — নমস্কার বিশ্ব")
                        .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("ไทย — สวัสดีชาวโลก  ·  日本語 東京  ·  简体中文 上海")
                        .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new(
                        "Thai dictionary wrap (no spaces): ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์ทุกสองสัปดาห์",
                    )
                    .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    col(4.0)
                        .push(Box::new(
                            Text::new("English auto hyphenation:")
                                .with_role(TextRole::Body),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_width(190.0).with_child(
                                Box::new(
                                    Text::new("Internationalization demonstrates typography.")
                                        .with_role(TextRole::Reading)
                                        .with_auto_hyphenation("en-US"),
                                ),
                            ),
                        )),
                ))
                .push(Box::new(
                    Text::new("Mixed BiDi: Invoice 123 — חשבון ٤٥٦ — total €78")
                        .with_role(TextRole::Body),
                ))
                .push(Box::new(
                    Text::new("Emoji clusters: 👩🏽‍💻  👨‍👩‍👧‍👦  🏳️‍🌈  🇪🇺")
                        .with_role(TextRole::Body),
                ))
}

pub fn build_typography_showcase(theme: &Theme) -> Box<dyn Widget> {
    let column = typography_intro_and_serif(col(9.0), theme);
    let column = typography_feature_pairs(column, theme);
    let column = typography_wrapping_and_punctuation(column, theme);
    let column = typography_scripts_and_bidi(column, theme);
    card(None, "Typography & Scripts", Box::new(column))
}

/// Seeds `state.doc` with the toggle/checkbox/slider/text/dropdown values
/// the interactive controls read. Split out of `build_showcase` to keep
/// that under the line cap.
fn seed_gallery_state(state: &mut State) {
    let root = state.root.clone();
    let _ = state.doc.put(&root, "sw_wifi", true);
    let _ = state.doc.put(&root, "sw_bt", false);
    let _ = state.doc.put(&root, "cb_updates", true);
    let _ = state.doc.put(&root, "sld_vol", 0.65);
    let _ = state.doc.put(&root, "dd_quality", "High");

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
}

/// Buttons, switches, checkbox, slider and toggle-group. Split out of
/// `build_showcase` to keep that under the line cap.
fn build_controls_card(theme: &Theme) -> Box<dyn Widget> {
    card(
        Some(CARD_W),
        "Buttons & Controls",
        Box::new(
            col(12.0)
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Button::new("g_save", "Save")
                                .role(ButtonRole::Primary)
                                .shortcut_hint(tilo_ui::platform::Shortcut::command("S")),
                        ))
                        .push(Box::new(Button::new("g_cancel", "Discard")))
                        .push(Box::new(
                            Button::new("g_delete", "Delete").role(ButtonRole::Destructive),
                        ))
                        .push(Box::new(
                            Button::new("g_more", "Details").role(ButtonRole::Plain),
                        ))
                        .push(Box::new(Button::icon_only(
                            "g_open",
                            tilo_ui::icons::Icon::ExternalLink,
                            "Open in browser",
                        ))),
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
    )
}

/// Text field, search field, dropdown, stepper and breadcrumb. Split out
/// of `build_showcase` to keep that under the line cap.
fn build_inputs_card(theme: &Theme) -> Box<dyn Widget> {
    card(
        Some(CARD_W),
        "Text & Selection",
        Box::new(
            col(12.0)
                .push(dim("Text field", theme))
                .push(Box::new(
                    TextInput::local()
                        .with_initial_value("Ada Lovelace")
                        .with_id("g_name"),
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
                    "Tilo".into(),
                    "ui".into(),
                ]))),
        ),
    )
}

/// Badges, chips and avatars. Split out of `build_showcase` to keep that
/// under the line cap.
fn build_tags_card(theme: &Theme) -> Box<dyn Widget> {
    card(
        Some(CARD_W),
        "Badges, Chips & Avatars",
        Box::new(
            col(12.0)
                .push(dim("Badges", theme))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Badge::text("New").style(ChipStyle::Tinted(Tone::Info)),
                        ))
                        .push(Box::new(Badge::text("Beta")))
                        .push(Box::new(
                            Badge::text("Failed").style(ChipStyle::Tinted(Tone::Danger)),
                        ))
                        .push(Box::new(Badge::text("Live").style(ChipStyle::Live)))
                        .push(Box::new(Badge::count(3)))
                        .push(Box::new(Badge::count(128)))
                        .push(Box::new(Badge::dot())),
                ))
                .push(dim("Chips — plain, selected, selectable, removable", theme))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Chip::new("Design").with_lead(ChipLead::Dot(
                            theme.tokens.colors.presence.series(0),
                        ))))
                        .push(Box::new(Chip::new("Rust").selected(true)))
                        .push(Box::new(Chip::new("Filter").selectable()))
                        .push(Box::new(Chip::new("alice@example.com").removable())),
                ))
                .push(dim("Avatars", theme))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Avatar::person("Ada Lovelace", "ada.lovelace")
                                .size(AvatarSize::Regular),
                        ))
                        .push(Box::new(
                            Avatar::person("Gwen Rhodes", "gwen.rhodes").size(AvatarSize::Regular),
                        ))
                        .push(Box::new(
                            Avatar::person("Lena Torres", "lena.torres").size(AvatarSize::Regular),
                        ))
                        .push(Box::new(
                            Avatar::person("Dana Brooks", "dana.brooks").size(AvatarSize::Regular),
                        )),
                )),
        ),
    )
}

/// Progress bars and a spinner. Split out of `build_showcase` to keep
/// that under the line cap.
fn build_feedback_card(theme: &Theme) -> Box<dyn Widget> {
    card(
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
                        .push(Box::new(tilo_ui::widget::Spinner::with_size(28.0)))
                        .push(label("Loading…")),
                )),
        ),
    )
}

/// The full-width stat-tile row. Split out of `build_showcase` to keep
/// that under the line cap.
fn build_metrics_card() -> Box<dyn Widget> {
    card(
        None,
        "Metrics",
        Box::new(StatGroup::new(vec![
            Box::new(StatTile::new("Requests", "1,284").change("12% this hour", Trend::Up)),
            Box::new(
                StatTile::new("Errors", "3")
                    .tone(Tone::Danger)
                    .change("2 fewer", Trend::Down),
            ),
            Box::new(
                StatTile::new("Latency", "48")
                    .unit("ms")
                    .change("No change", Trend::Flat),
            ),
            Box::new(
                StatTile::new("Uptime", "99.9")
                    .unit("%")
                    .tone(Tone::Success),
            ),
        ])),
    )
}

/// Ring gauge, sparkline, bar gauge and area chart. Split out of
/// `build_showcase` to keep that under the line cap.
fn build_charts_card(theme: &Theme) -> Box<dyn Widget> {
    let accent = theme.tokens.colors.intent.accent;
    card(
        Some(CARD_W),
        "Charts & Gauges",
        Box::new(
            col(12.0)
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            ProgressRing::new(0.72)
                                .value("72%")
                                .caption("CPU")
                                .level(GaugeThresholds::default()),
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
    )
}

/// Tab bar and toolbar. Split out of `build_showcase` to keep that under
/// the line cap.
fn build_navigation_card(theme: &Theme) -> Box<dyn Widget> {
    card(
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
    )
}

/// The four inline-banner severities. Split out of `build_showcase` to
/// keep that under the line cap.
fn build_banners_card() -> Box<dyn Widget> {
    card(
        None,
        "Inline Banners",
        Box::new(
            col(10.0)
                .push(Box::new(
                    Banner::info("A new version of Tilo is available.")
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
    )
}

/// One row of the gallery's data table.
struct Task {
    name: &'static str,
    owner: &'static str,
    hours: Option<u32>,
}

/// The empty-state and data-table cards. Split out of `build_showcase`
/// to keep that under the line cap.
fn build_empty_and_table_cards() -> (Box<dyn Widget>, Box<dyn Widget>) {
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
    let mut tasks = tilo_ui::widget::data_table::DataTable::new(vec![
        tilo_ui::widget::data_table::ColumnDef::new(
            "task",
            "Task",
            tilo_ui::widget::data_table::ColumnWidth::Flex(2.0),
        )
        .with_text(|t: &Task| t.name.to_owned()),
        tilo_ui::widget::data_table::ColumnDef::new(
            "owner",
            "Owner",
            tilo_ui::widget::data_table::ColumnWidth::Auto,
        )
        .with_text(|t: &Task| t.owner.to_owned()),
        tilo_ui::widget::data_table::ColumnDef::new(
            "hours",
            "Hours",
            tilo_ui::widget::data_table::ColumnWidth::Flex(1.0),
        )
        .with_kind(tilo_ui::text_render::TextKind::Numeric)
        .with_optional_text(|t: &Task| t.hours.map(|h| h.to_string()), "Not estimated"),
    ]);
    tasks.set_rows(vec![
        Task {
            name: "Design the review flow",
            owner: "Ada Lovelace",
            hours: Some(12),
        },
        Task {
            name: "Build the sync engine",
            owner: "Grace Hopper",
            hours: Some(140),
        },
        Task {
            name: "Ship the beta",
            owner: "Linus Pauling",
            hours: None,
        },
    ]);
    let table_card = card(None, "Table", Box::new(tasks));
    (empty, table_card)
}

/// Build the whole showcase, seeding `state.doc`. `theme` selects dimmed-text
/// colours so a per-theme rebuild renders correctly in both light and dark.
pub fn build_showcase(state: &mut State, theme: &Theme) -> Box<dyn Widget> {
    seed_gallery_state(state);

    // ---- Header -----------------------------------------------------------
    let header = col(4.0).push(title("Tilo UI Toolkit")).push(dim(
        "Component gallery — one showcase of every general-purpose widget, light & dark",
        theme,
    ));

    // ---- Row 1: controls | inputs -----------------------------------------
    let controls = build_controls_card(theme);
    let inputs = build_inputs_card(theme);

    // ---- Row 2: tags | feedback -------------------------------------------
    let tags = build_tags_card(theme);
    let feedback = build_feedback_card(theme);

    // ---- Metrics (full width) ---------------------------------------------
    let metrics = build_metrics_card();

    // ---- Row 3: charts | navigation -----------------------------------------
    let charts = build_charts_card(theme);
    let navigation = build_navigation_card(theme);

    // ---- Banners (full width) ---------------------------------------------
    let banners = build_banners_card();

    // ---- Typography conformance (full width) -----------------------------
    // Keep the multilingual corpus visible in the ordinary gallery so font
    // policy, fallback, shaping, metrics and BiDi regressions are reviewable
    // in the same light/dark screenshots as every other component.
    let typography = build_typography_showcase(theme);

    // ---- Empty state + data table (full width) -----------------------------
    let (empty, table_card) = build_empty_and_table_cards();

    // ---- Assemble ---------------------------------------------------------
    Box::new(
        Column::new()
            .with_gap(16.0)
            .push(Box::new(header))
            .push(Box::new(Row::new().push(controls).push(inputs)))
            .push(Box::new(Row::new().push(tags).push(feedback)))
            .push(metrics)
            .push(Box::new(Row::new().push(charts).push(navigation)))
            .push(typography)
            .push(banners)
            .push(empty)
            .push(table_card),
    )
}
