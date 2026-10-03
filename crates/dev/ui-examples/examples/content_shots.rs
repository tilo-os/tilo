//! Offscreen renders of the content composites (list rows, property lists,
//! sections, cards, media grids, stat tiles, data tables, empty states,
//! drop zones, skeletons, progress rings, steppers) on the window ground,
//! dark and light, wide and narrow, and at twice the text size.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- content_shots [OUT]`
//! Out:  `OUT` (default `target/look/fw-content`): `<page>-<theme>-<size>.png`
//!       plus an inspection `.json` beside each.

use std::path::{Path, PathBuf};

use tilo_ui::headless::FrameHarness;
use tilo_ui::icons::Icon;
use tilo_ui::text_render::TextKind;
use tilo_ui::theme::Theme;
use tilo_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use tilo_ui::widget::{
    Actions, Button, Card, CardGrid, CardWidth, Column, DropZone, Element, EmptyState, ListRow,
    MediaAspect, MediaCard, MediaGrid, MediaSize, MediaSource, OwnedBadge, ProgressRing,
    ProgressRingSize, Property, PropertyList, Row, Scrollable, Section, SectionHeader, Skeleton,
    StatGroup, StatTile, Stepper, Tone, Trend,
};

fn rows() -> Element {
    Box::new(
        Section::new(vec![
            Box::new(
                ListRow::new("ada", "Ada Lovelace")
                    .avatar("Ada Lovelace", "ada@example.org")
                    .secondary("Notes on the analytical engine, with the corrected Bernoulli table")
                    .two_line_secondary()
                    .meta("09:41")
                    .emphasized(true)
                    .presence("ada@example.org")
                    .on_activate(|| {}),
            ),
            Box::new(
                ListRow::new("grace", "Grace Hopper")
                    .avatar("Grace Hopper", "grace@example.org")
                    .secondary("Re: compiler review on Thursday")
                    .meta("Yesterday")
                    .selected(true)
                    .on_activate(|| {}),
            ),
            Box::new(
                ListRow::new("linus", "Linus Pauling")
                    .avatar("Linus Pauling", "linus@example.org")
                    .secondary("Vitamin C and the common cold, chapter three")
                    .meta("Mon")
                    .on_activate(|| {}),
            ),
            Box::new(
                ListRow::new("get-users", "/v1/users")
                    .badge("GET")
                    .meta("84 ms")
                    .active()
                    .on_activate(|| {}),
            ),
            Box::new(
                ListRow::new("settings", "Storage")
                    .icon(Icon::HardDrive)
                    .meta("412 GB free")
                    .disclosure()
                    .on_activate(|| {}),
            ),
        ])
        .with_header(SectionHeader::heading("Inbox").count(3).action(Button::new(
            "mark-read",
            "Mark all as read",
            None,
        ))),
    )
}

fn properties() -> Element {
    Box::new(Section::titled(
        "Contact",
        vec![Box::new(PropertyList::new(vec![
            Property::new("Email", "ada.lovelace@example.org")
                .with_kind(TextKind::Identifier)
                .copyable(),
            Property::new("Phone", "+44 20 7946 0958").with_kind(TextKind::Numeric),
            Property::new("Address", "12 St James's Square, London SW1Y 4JH"),
            Property::new("Birthday", "10 December 1815"),
        ]))],
    ))
}

fn stepper() -> Element {
    Box::new(
        Stepper::new(
            ["Choose image", "Choose drive", "Write", "Verify"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        )
        .current(2),
    )
}

fn page(children: Vec<Element>) -> Element {
    let mut column = Column::new().with_intrinsic_height().with_gap(32.0);
    for child in children {
        column = column.push(child);
    }
    Box::new(Scrollable::new_vertical().with_child(Box::new(column)))
}

fn lists_page() -> Element {
    page(vec![rows(), properties(), stepper()])
}

fn cards() -> Element {
    Box::new(
        CardGrid::new(vec![
            Box::new(
                Card::new(Box::new(tilo_ui::widget::Text::new(
                    "A terminal that follows you between devices.",
                )))
                .with_title("Terminal")
                .with_footer(Actions::new(Button::new("open-terminal", "Open", None).primary()))
                .on_activate("terminal", || {}),
            ),
            Box::new(
                Card::new(Box::new(tilo_ui::widget::Text::new(
                    "\u{201c}The table on page 12 is off by one row; can we fix it before Friday?\u{201d}",
                )))
                .with_title("Ada Lovelace")
                .leading_stripe(Tone::of_identity("ada@example.org"))
                .with_footer(
                    Actions::new(Button::new("resolve", "Resolve", None))
                        .secondary(Button::new("reply", "Reply", None)),
                ),
            ),
            Box::new(
                Card::new(Box::new(tilo_ui::widget::Text::new(
                    "Backup to the office NAS failed: the share is read-only.",
                )))
                .with_title("Nightly backup")
                .leading_stripe(Tone::Danger)
                .with_footer(Actions::new(Button::new("retry", "Retry backup", None).primary())),
            ),
        ])
        .with_card_width(CardWidth::Regular),
    )
}

fn media() -> Element {
    let albums = [
        ("Kind of Blue", "Miles Davis"),
        ("Blue Train", "John Coltrane"),
        ("Moanin'", "Art Blakey and the Jazz Messengers"),
        ("Mingus Ah Um", "Charles Mingus"),
        ("Time Out", "The Dave Brubeck Quartet"),
    ];
    let grid = MediaGrid::new(
        albums
            .iter()
            .map(|(title, artist)| {
                MediaCard::new(*title, MediaSource::Placeholder)
                    .title(*title)
                    .meta(*artist)
                    .hover_action(Icon::Play, "Play", || {})
                    .on_activate(|| {})
            })
            .collect(),
    )
    .with_size(MediaSize::Small);
    let videos = MediaGrid::new(vec![
        MediaCard::new("talk", MediaSource::Placeholder)
            .aspect(MediaAspect::Wide)
            .title("Building a compositor in Rust")
            .meta("Tilo Engineering")
            .meta("3 days ago")
            .badge(OwnedBadge::Duration("42:17".into()))
            .badge(OwnedBadge::Progress(0.35))
            .on_activate(|| {}),
        MediaCard::new("live", MediaSource::Placeholder)
            .aspect(MediaAspect::Wide)
            .title("Release day stream")
            .meta("Tilo")
            .badge(OwnedBadge::Live)
            .on_activate(|| {}),
        MediaCard::new("offline", MediaSource::Placeholder)
            .aspect(MediaAspect::Wide)
            .title("Designing for presence")
            .meta("Design notes")
            .meta("Last week")
            .badge(OwnedBadge::Duration("18:03".into()))
            .badge(OwnedBadge::Downloaded)
            .on_activate(|| {}),
    ]);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(24.0)
            .push(Box::new(Section::titled("Albums", vec![Box::new(grid)])))
            .push(Box::new(Section::titled("Videos", vec![Box::new(videos)]))),
    )
}

fn stats() -> Element {
    Box::new(Section::titled(
        "Backups",
        vec![Box::new(StatGroup::new(vec![
            Box::new(
                StatTile::new("Protected", "1.2")
                    .unit("TB")
                    .tone(Tone::Success),
            ),
            Box::new(StatTile::new("Snapshots", "128").change("6 this week", Trend::Up)),
            Box::new(StatTile::new("Next run", "In 38 min")),
            Box::new(
                StatTile::new("Last backup", "Failed")
                    .tone(Tone::Danger)
                    .sparkline(vec![3.0, 4.0, 3.5, 5.0, 4.2, 0.0]),
            ),
        ]))],
    ))
}

fn objects_page() -> Element {
    page(vec![stats(), cards(), media()])
}

struct Proc {
    name: &'static str,
    cpu: f64,
    memory: Option<u64>,
}

fn table() -> Element {
    let mut table = DataTable::new(vec![
        ColumnDef::new("name", "Process", ColumnWidth::Flex(2.0))
            .sortable(true)
            .with_text(|p: &Proc| p.name.to_owned()),
        ColumnDef::new("cpu", "CPU %", ColumnWidth::Flex(1.0))
            .with_kind(TextKind::Numeric)
            .sortable(true)
            .with_text(|p: &Proc| format!("{:.1}", p.cpu)),
        ColumnDef::new("mem", "Memory", ColumnWidth::Flex(1.0))
            .with_kind(TextKind::Numeric)
            .with_optional_text(|p: &Proc| p.memory.map(|m| format!("{m} MB")), "NULL"),
    ])
    .selectable(true)
    .ruled();
    table.set_rows(vec![
        Proc {
            name: "tilo-compositor",
            cpu: 12.4,
            memory: Some(412),
        },
        Proc {
            name: "tilo-sync",
            cpu: 3.1,
            memory: Some(96),
        },
        Proc {
            name: "mail",
            cpu: 0.4,
            memory: None,
        },
        Proc {
            name: "photos-indexer",
            cpu: 41.9,
            memory: Some(1_204),
        },
    ]);
    table.sort_by_with(|a: &Proc, b: &Proc| a.cpu.total_cmp(&b.cpu), "cpu", false);
    let mut selection = tilo_ui::selection::SelectionModel::new();
    selection.select_only(1);
    table.set_selection(selection);
    Box::new(table)
}

fn states_page() -> Element {
    page(vec![
        Box::new(
            Row::new()
                .push(Box::new(ProgressRing::new(0.62).value("23").caption("seconds")))
                .push(Box::new(
                    ProgressRing::new(0.84)
                        .value("84%")
                        .caption("Disk")
                        .level(tilo_ui::widget::GaugeThresholds::default()),
                ))
                .push(Box::new(ProgressRing::new(0.3).size(ProgressRingSize::Small))),
        ),
        table(),
        Box::new(
            DropZone::new("import", "Drop books to add them")
                .message("EPUB or PDF, up to 200 MB")
                .with_action(Button::new("choose", "Choose files", None)),
        ),
        Box::new(Skeleton::row()),
        Box::new(Skeleton::text(3)),
        Box::new(
            tilo_ui::widget::Container::new()
                .with_no_padding()
                .with_height(320.0)
                .with_child(Box::new(
                    EmptyState::error(
                        "Can't open this document",
                        "The file ends before its cross-reference table at byte 48 210, so its pages cannot be found.",
                    )
                    .with_icon(Icon::FileText)
                    .with_action("Open another file"),
                )),
        ),
    ])
}

/// A named page and the function that builds it.
type Page = (&'static str, fn() -> Element);

fn shoot(out: &Path, name: &str, build: fn() -> Element, theme: &Theme, viewport: (u32, u32)) {
    let harness =
        FrameHarness::new_with_theme(tilo_ui::State::new(), build(), viewport, theme.clone());
    let path = out.join(format!("{name}.png"));
    harness.render_window_png(&path);
    if let Ok(json) = harness.inspection_json() {
        let _ = std::fs::write(out.join(format!("{name}.json")), json);
    }
    println!("{}", path.display());
}

pub fn run(args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let out = args
        .first()
        .map_or_else(|| PathBuf::from("target/look/fw-content"), PathBuf::from);
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {error}", out.display());
        return;
    }
    let pages: [Page; 3] = [
        ("lists", lists_page),
        ("objects", objects_page),
        ("states", states_page),
    ];
    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for (page, build) in pages {
            shoot(
                &out,
                &format!("{page}-{theme_name}-wide"),
                build,
                &theme,
                (1280, 800),
            );
            shoot(
                &out,
                &format!("{page}-{theme_name}-narrow"),
                build,
                &theme,
                (420, 900),
            );
        }
    }
    let large = Theme::dark().with_text_scale(2.0);
    for (page, build) in pages {
        shoot(
            &out,
            &format!("{page}-dark-large-text"),
            build,
            &large,
            (1280, 900),
        );
    }
}
