//! Type specimens: matched script fallbacks in every role, and the text kinds
//! (numeric columns, codes, identifiers), in the default and Hyperlegible
//! faces, dark and light.
//!
//! ```bash
//! cargo run -p ui-examples --example type-specimen-shots -- \
//!     --shots target/look/type/specimen.png --viewport 1280x800
//! ```
//!
//! Writes `<stem>-<page>-<face>-<theme>.png` next to the `--shots` path for
//! the pages `scripts` and `kinds`, the faces `default` and `hyperlegible`
//! and the themes `dark` and `light`.

use std::path::{Path, PathBuf};

use tilo_ui::font_catalog::FontCatalog;
use tilo_ui::headless::FrameHarness;
use tilo_ui::permissions::UserId;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::{TextRole, UiFace};
use tilo_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use tilo_ui::widget::{Column, Container, Element, Row, Text, TextInput, TextInputKind};
use tilo_ui::{State, TextKind};

const ROLES: [(TextRole, &str); 10] = [
    (TextRole::Display, "Display"),
    (TextRole::LargeTitle, "Large title"),
    (TextRole::Title, "Title"),
    (TextRole::Heading, "Heading"),
    (TextRole::Subheading, "Subheading"),
    (TextRole::Body, "Body"),
    (TextRole::Label, "Label"),
    (TextRole::Caption, "Caption"),
    (TextRole::Reading, "Reading"),
    (TextRole::Code, "Code"),
];

/// One line per role, Latin interleaved with each matched script.
const MIXED: &str = "Report Отчёт Звіт Αναφορά Báo cáo Größe";

const SAMPLES: [&str; 6] = [
    "Russian: Съешь же ещё этих мягких французских булок, да выпей чаю.",
    "Ukrainian: Чуєш їх, доцю, га? Кумедна ж ти, прощайся без ґольфів!",
    "Greek: Ξεσκεπάζω την ψυχοφθόρα βδελυγμία. Ταχίστη αλώπηξ βαφής ψημένη γη.",
    "Vietnamese: Tiếng Việt có dấu — Chào thế giới, hẹn gặp lại ngày mai.",
    "German: Größenverhältnisse über Straßen, Flüsse und Brücken.",
    "Invoice 4821 — Счёт № 4821 — Τιμολόγιο 4821 — Hóa đơn 4821",
];

fn label(text: &str) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(150.0)
            .with_child(Box::new(
                Text::new(text).with_role(TextRole::Caption).dimmed(),
            )),
    )
}

fn line(name: &str, content: Element) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .with_gap(16.0)
            .with_align_items(taffy::AlignItems::Baseline)
            .push(label(name))
            .push(content),
    )
}

fn page(content: Column) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .filling()
            .with_child(Box::new(
                content
                    .with_vertical_padding(32.0)
                    .with_horizontal_padding(40.0, 40.0),
            )),
    )
}

fn scripts() -> Element {
    let mut column = Column::new().with_gap(10.0);
    for (role, name) in ROLES {
        column = column.push(line(name, Box::new(Text::new(MIXED).with_role(role))));
    }
    for sample in SAMPLES {
        column = column.push(line(
            "Body",
            Box::new(Text::new(sample).with_role(TextRole::Body)),
        ));
    }
    column = column.push(line(
        "Body strong",
        Box::new(
            Text::new("Medium Средний Μεσαίο Trung bình")
                .with_role(TextRole::Body)
                .strong(),
        ),
    ));
    column = column.push(line(
        "Body italic",
        Box::new(
            Text::new("Italic Курсив Πλάγια Chữ nghiêng")
                .with_role(TextRole::Body)
                .with_italic(true),
        ),
    ));
    page(column)
}

#[derive(Clone)]
struct Process {
    name: &'static str,
    cpu: &'static str,
    memory: &'static str,
    uptime: &'static str,
}

const PROCESSES: [Process; 6] = [
    Process {
        name: "compositor",
        cpu: "11.8 %",
        memory: "1,111.1 MB",
        uptime: "11:11:11",
    },
    Process {
        name: "sync",
        cpu: "4.0 %",
        memory: "848.0 MB",
        uptime: "08:40:19",
    },
    Process {
        name: "indexer",
        cpu: "71.1 %",
        memory: "100.1 MB",
        uptime: "00:11:01",
    },
    Process {
        name: "mail",
        cpu: "0.9 %",
        memory: "388.8 MB",
        uptime: "23:58:47",
    },
    Process {
        name: "browser",
        cpu: "17.1 %",
        memory: "2,040.7 MB",
        uptime: "01:17:11",
    },
    Process {
        name: "agent",
        cpu: "88.8 %",
        memory: "999.9 MB",
        uptime: "10:10:10",
    },
];

fn table() -> Element {
    let columns = vec![
        ColumnDef::new("name", "Process", ColumnWidth::Fixed(140.0))
            .with_text(|p: &Process| p.name.into()),
        ColumnDef::new("cpu-prose", "CPU as prose", ColumnWidth::Fixed(110.0))
            .with_text(|p: &Process| p.cpu.into()),
        ColumnDef::new("cpu", "CPU", ColumnWidth::Fixed(90.0))
            .with_text(|p: &Process| p.cpu.into())
            .with_kind(TextKind::Numeric),
        ColumnDef::new("memory", "Memory", ColumnWidth::Fixed(110.0))
            .with_text(|p: &Process| p.memory.into())
            .with_kind(TextKind::Numeric),
        ColumnDef::new("uptime", "Uptime", ColumnWidth::Fixed(100.0))
            .with_text(|p: &Process| p.uptime.into())
            .with_kind(TextKind::Numeric),
    ];
    let mut table = DataTable::new(columns).with_id("processes");
    table.set_rows(PROCESSES.to_vec());
    Box::new(
        Container::new()
            .with_no_padding()
            .with_size(560.0, 260.0)
            .with_child(Box::new(table)),
    )
}

fn input(kind: TextInputKind, value: &str, name: &str) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(260.0)
            .with_child(Box::new(
                TextInput::local()
                    .with_kind(kind)
                    .with_initial_value(value)
                    .with_accessible_name(name),
            )),
    )
}

fn pair(name: &str, text: &str, role: TextRole, kind: TextKind) -> Element {
    line(
        name,
        Box::new(
            Row::new()
                .no_wrap()
                .with_gap(32.0)
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_width(300.0)
                        .with_child(Box::new(Text::new(text).with_role(role))),
                ))
                .push(Box::new(Text::new(text).with_role(role).with_kind(kind))),
        ),
    )
}

fn kinds() -> Element {
    let column = Column::new()
        .with_gap(14.0)
        .push(line(
            "",
            Box::new(
                Row::new()
                    .no_wrap()
                    .with_gap(32.0)
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(300.0)
                            .with_child(Box::new(
                                Text::new("As prose").with_role(TextRole::Subheading),
                            )),
                    ))
                    .push(Box::new(
                        Text::new("With its kind").with_role(TextRole::Subheading),
                    )),
            ),
        ))
        .push(pair(
            "TOTP code",
            "482 913",
            TextRole::Display,
            TextKind::Secret,
        ))
        .push(pair(
            "Recovery code",
            "I1l0-O0lI-7fK2-QZ8w",
            TextRole::Body,
            TextKind::Secret,
        ))
        .push(pair(
            "API key",
            "tl_live_0OIl1aB7xQ9z",
            TextRole::Body,
            TextKind::Secret,
        ))
        .push(pair(
            "Username",
            "Ilya_l1",
            TextRole::Body,
            TextKind::Identifier,
        ))
        .push(pair(
            "Look-alikes",
            "I1l0O",
            TextRole::Title,
            TextKind::Identifier,
        ))
        .push(pair(
            "Clock",
            "10:11:08",
            TextRole::LargeTitle,
            TextKind::Numeric,
        ))
        .push(line(
            "Username field",
            Box::new(
                Row::new()
                    .no_wrap()
                    .with_gap(16.0)
                    .push(input(TextInputKind::Username, "Ilya_l1", "Username"))
                    .push(input(
                        TextInputKind::Username,
                        "I1l0O",
                        "Look-alike username",
                    ))
                    .push(input(
                        TextInputKind::Secret,
                        "I1l0-O0lI-7fK2",
                        "Recovery code",
                    )),
            ),
        ))
        .push(line("Numeric columns", table()));
    page(column)
}

fn theme(face: UiFace, light: bool) -> Theme {
    let mut theme = if light { Theme::light() } else { Theme::dark() };
    theme.tokens.typography.set_face(face.name());
    theme
}

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip_while(|a| a != name);
    args.next()?;
    args.next()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let shots =
        PathBuf::from(arg("--shots").unwrap_or_else(|| "target/look/type/specimen.png".into()));
    let viewport = arg("--viewport")
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280, 800));
    let dir = shots.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let stem = shots
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("specimen")
        .to_owned();
    // The checked-in catalog, without host fonts: the shots show the faces
    // Tilo ships, on any machine.
    let catalog = FontCatalog::controlled_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/ui-core/assets/fonts"),
        "en-US",
    );
    for (page_name, build) in [("scripts", scripts as fn() -> Element), ("kinds", kinds)] {
        for face in UiFace::ALL {
            for (theme_name, light) in [("dark", false), ("light", true)] {
                let harness = FrameHarness::new_with_controlled_inputs(
                    State::new(),
                    build(),
                    viewport,
                    theme(face, light),
                    TextRenderer::with_font_catalog(&catalog)?,
                    UserId("specimen".into()),
                );
                let face_name = face.name().to_ascii_lowercase();
                let out = dir.join(format!("{stem}-{page_name}-{face_name}-{theme_name}.png"));
                harness.render_png(&out);
                println!("{}", out.display());
            }
        }
    }
    Ok(())
}
