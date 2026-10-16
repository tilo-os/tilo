//! The Writer agent benchmark: one fixed task, done twice against one
//! checked-in reference, then exported and reopened.
//!
//! The task is a short field report — two headings, a paragraph with a bold
//! phrase, a two-item bullet list and a closing paragraph with an italic
//! date ([`REFERENCE`]).
//!
//! - The **typed arm** is an agent. It reads Writer's published actions and
//!   drives the document only through them, by id with JSON arguments, as
//!   the agent [`AGENT`]: the bound-action door that the control socket's
//!   `actions invoke`, the MCP bridge and the palette all reach.
//! - The **input arm** is computer use. It clicks into the page it was shown
//!   and types and presses keys — the control socket's `click`, `type`, `key`
//!   and `modifiers` vocabulary — and reads nothing but the frame.
//!
//! Each arm is graded from the document the system owns, never from what
//! the arm reports. The typed arm's result is then saved as DOCX and as PDF
//! with Writer's own Save As and reopened the way a person opens them: the
//! DOCX launched in Writer, the PDF opened in the PDF viewer's engine. What
//! comes back is inspected for structure, text, formatting and pixels.
//!
//! Every qualification lands in a receipt (written to the path in
//! `TILO_WRITER_BENCHMARK_RECEIPT` when it is set), and any that did not
//! pass fails the test, naming what was observed. Nothing the benchmark did
//! not observe is in the receipt as a pass.

use std::path::Path;

use tilo_doc::DocSnapshot;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActorKind, ParticipantId};
use word_processor::framework::WordProcessorApp;

type App = TestApp<WordProcessorApp>;

/// Who the typed arm acts as, and the person it acts for.
const AGENT: &str = "agent:writer-benchmark";
const PERSON: &str = "owner";
/// The agent's own caret and selection in the document: its presence.
const PARTICIPANT: &str = "writer-benchmark";
const DOCUMENT: &str = "word-processor.document";

/// One paragraph of the reference: its kind, heading level or list kind,
/// text, and the phrases that carry each inline format.
struct Expected {
    kind: &'static str,
    level: Option<i64>,
    ordered: Option<bool>,
    text: &'static str,
    marks: &'static [(&'static str, &'static str)],
}

/// The accepted reference: what both arms are asked to produce.
const REFERENCE: [Expected; 6] = [
    Expected {
        kind: "heading",
        level: Some(1),
        ordered: None,
        text: "Quarterly Field Report",
        marks: &[],
    },
    Expected {
        kind: "paragraph",
        level: None,
        ordered: None,
        text: "Prepared for the steering group by the field team.",
        marks: &[("strong", "steering group")],
    },
    Expected {
        kind: "heading",
        level: Some(2),
        ordered: None,
        text: "Findings",
        marks: &[],
    },
    Expected {
        kind: "list_item",
        level: None,
        ordered: Some(false),
        text: "Sensors stayed online for 41 days.",
        marks: &[],
    },
    Expected {
        kind: "list_item",
        level: None,
        ordered: Some(false),
        text: "Battery drain fell by 18 percent.",
        marks: &[],
    },
    Expected {
        kind: "paragraph",
        level: None,
        ordered: None,
        text: "Next review: 3 November.",
        marks: &[("em", "3 November")],
    },
];

/// The inline formats the reference is graded on; authorship and other
/// bookkeeping marks are not formatting.
const FORMATS: [&str; 4] = ["strong", "em", "underline", "strike"];

/// The share of a frame's pixels two renders of one document may differ in
/// (a channel off by more than [`PIXEL_TOLERANCE`]) and still be one picture.
/// Two renders of the report with the caret in one place are identical; one
/// italic date of ten characters lost is 0.036% (see
/// `a_page_missing_one_format_is_not_the_same_picture`), so 0.01% leaves
/// room for antialiasing and none for a lost format.
const MAX_FRAME_DIFFERENCE: f64 = 0.0001;
const PIXEL_TOLERANCE: u8 = 48;
/// The least share of the frame six paragraphs of text change from a blank
/// document's.
const MIN_DRAWN: f64 = 0.005;

/// One qualification: whether it passed and what was observed.
struct Qualification {
    id: &'static str,
    passed: bool,
    observed: String,
}

#[derive(Default)]
struct Receipt {
    qualifications: Vec<Qualification>,
    measurements: serde_json::Map<String, serde_json::Value>,
}

impl Receipt {
    fn record(&mut self, id: &'static str, outcome: Result<String, String>) {
        let (passed, observed) = match outcome {
            Ok(observed) => (true, observed),
            Err(observed) => (false, observed),
        };
        self.qualifications.push(Qualification {
            id,
            passed,
            observed,
        });
    }

    fn measure(&mut self, name: &str, value: impl Into<serde_json::Value>) {
        self.measurements.insert(name.to_owned(), value.into());
    }

    fn to_json(&self) -> serde_json::Value {
        let passed = self.qualifications.iter().filter(|q| q.passed).count();
        serde_json::json!({
            "benchmark": "writer-agent",
            "reference": REFERENCE.iter().map(|block| block.text).collect::<Vec<_>>(),
            "passed": passed,
            "total": self.qualifications.len(),
            "qualifications": self.qualifications.iter().map(|q| serde_json::json!({
                "id": q.id,
                "passed": q.passed,
                "observed": q.observed,
            })).collect::<Vec<_>>(),
            "measurements": self.measurements,
        })
    }
}

// ---- grading -----------------------------------------------------------

/// The document as the system holds it.
fn snapshot(app: &App) -> DocSnapshot {
    app.read_document(|doc| {
        let containers = word_processor::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &containers)
    })
}

/// One paragraph as graded: everything [`Expected`] names, read back.
#[derive(Debug, PartialEq, Eq)]
struct Graded {
    kind: String,
    level: Option<i64>,
    ordered: Option<bool>,
    text: String,
    marks: Vec<(String, String)>,
}

fn graded(snapshot: &DocSnapshot) -> Vec<Graded> {
    snapshot
        .blocks
        .iter()
        .map(|block| {
            let chars: Vec<char> = block.text.chars().collect();
            let mut marks: Vec<(String, String)> = block
                .marks
                .iter()
                .filter(|mark| FORMATS.contains(&mark.name.as_str()))
                .map(|mark| {
                    let end = mark.end.min(chars.len());
                    let start = mark.start.min(end);
                    (mark.name.clone(), chars[start..end].iter().collect())
                })
                .collect();
            marks.sort();
            marks.dedup();
            Graded {
                kind: block.kind.clone(),
                level: (block.kind == "heading")
                    .then(|| block.int_attr("level"))
                    .flatten(),
                ordered: (block.kind == "list_item")
                    .then(|| block.bool_attr("ordered").or(Some(false)))
                    .flatten(),
                text: block.text.clone(),
                marks,
            }
        })
        .collect()
}

fn expected() -> Vec<Graded> {
    REFERENCE
        .iter()
        .map(|block| {
            let mut marks: Vec<(String, String)> = block
                .marks
                .iter()
                .map(|&(name, phrase)| (name.to_owned(), phrase.to_owned()))
                .collect();
            marks.sort();
            Graded {
                kind: block.kind.to_owned(),
                level: block.level,
                ordered: block.ordered,
                text: block.text.to_owned(),
                marks,
            }
        })
        .collect()
}

/// Whether `snapshot` is the reference, paragraph by paragraph.
fn matches_reference(snapshot: &DocSnapshot) -> Result<String, String> {
    let got = graded(snapshot);
    let want = expected();
    if got == want {
        let marks: usize = want.iter().map(|block| block.marks.len()).sum();
        return Ok(format!(
            "{} paragraphs, kinds, levels, list kinds, text and {marks} formatted phrases match",
            want.len()
        ));
    }
    let first = got
        .iter()
        .zip(&want)
        .position(|(got, want)| got != want)
        .unwrap_or_else(|| got.len().min(want.len()));
    Err(format!(
        "{} paragraphs, reference has {}; first difference at paragraph {first}: got {:?}, want {:?}",
        got.len(),
        want.len(),
        got.get(first),
        want.get(first)
    ))
}

// ---- the typed arm -----------------------------------------------------

fn agent() -> ActionInvoker {
    ActionInvoker::actor(AGENT)
        .with_kind(ActorKind::Agent)
        .acting_for(PERSON)
        .with_participant(ParticipantId::new(PARTICIPANT).unwrap())
}

/// The agent's plan: published action ids and their JSON arguments, in
/// order. It names no paragraph ids and no coordinates — it types at its
/// caret and finds the phrases it formats.
fn plan() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    let paragraph = |kind: &str, extra: serde_json::Value| {
        let mut arguments = json!({ "kind": kind });
        if let (Some(arguments), serde_json::Value::Object(extra)) =
            (arguments.as_object_mut(), extra)
        {
            arguments.extend(extra);
        }
        ("set-paragraph-style", arguments)
    };
    vec![
        ("type-text", json!({"text": REFERENCE[0].text})),
        paragraph("heading", json!({"level": 1})),
        ("new-paragraph", json!({})),
        paragraph("paragraph", json!({})),
        ("type-text", json!({"text": REFERENCE[1].text})),
        ("new-paragraph", json!({})),
        paragraph("heading", json!({"level": 2})),
        ("type-text", json!({"text": REFERENCE[2].text})),
        ("new-paragraph", json!({})),
        paragraph("list_item", json!({"ordered": false})),
        ("type-text", json!({"text": REFERENCE[3].text})),
        ("new-paragraph", json!({})),
        paragraph("list_item", json!({"ordered": false})),
        ("type-text", json!({"text": REFERENCE[4].text})),
        ("new-paragraph", json!({})),
        paragraph("paragraph", json!({})),
        ("type-text", json!({"text": REFERENCE[5].text})),
        ("find-text", json!({"query": "steering group"})),
        ("set-format", json!({"name": "strong", "on": true})),
        ("find-text", json!({"query": "3 November"})),
        ("set-format", json!({"name": "em", "on": true})),
        ("find-text", json!({"query": ""})),
    ]
}

/// Invoke every step of `plan` as the agent, and answer the refusals.
fn run(app: &mut App, plan: &[(&'static str, serde_json::Value)]) -> Vec<String> {
    let mut refused = Vec::new();
    for (id, arguments) in plan {
        if let Err(error) = app.call_by(agent(), DOCUMENT, id, arguments.clone()) {
            refused.push(format!("{id}: {error}"));
        }
    }
    app.settle();
    refused
}

/// The typed arm, start to finish: discover, then act. Answers the app it
/// drove.
fn typed_arm(receipt: &mut Receipt) -> App {
    let mut app = open();
    let surface = app.agent_surface();
    let published: Vec<&str> = surface
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|row| row["resource"] == DOCUMENT)
        .filter_map(|row| row["action"].as_str())
        .collect();
    let plan = plan();
    let mut used: Vec<&str> = plan.iter().map(|(id, _)| *id).collect();
    used.sort_unstable();
    used.dedup();
    let missing: Vec<&str> = used
        .iter()
        .copied()
        .filter(|id| !published.contains(id))
        .collect();
    receipt.record(
        "typed.discovers_every_action",
        if missing.is_empty() {
            Ok(format!(
                "the {} actions the plan uses are among the {} the document publishes",
                used.len(),
                published.len()
            ))
        } else {
            Err(format!("not published: {missing:?}"))
        },
    );

    let before = app.history().len();
    let started = std::time::Instant::now();
    let refused = run(&mut app, &plan);
    let elapsed = started.elapsed();
    receipt.measure("typed_actions", plan.len());
    receipt.measure(
        "typed_elapsed_ms",
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    );
    receipt.record(
        "typed.every_action_admitted",
        if refused.is_empty() {
            Ok(format!("{} invocations, none refused", plan.len()))
        } else {
            Err(format!("refused: {refused:?}"))
        },
    );
    receipt.record(
        "typed.document_matches_reference",
        matches_reference(&snapshot(&app)),
    );

    let entries = app.history();
    let run = entries.get(before..).unwrap_or_default();
    let foreign = run
        .iter()
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_none_or(|invoker| invoker.actor != AGENT)
        })
        .count();
    receipt.record(
        "typed.history_attributed_to_agent",
        if !run.is_empty() && foreign == 0 {
            Ok(format!(
                "{} history entries, every one names {AGENT}",
                run.len()
            ))
        } else {
            Err(format!(
                "{} history entries, {foreign} not attributed to {AGENT}",
                run.len()
            ))
        },
    );
    app
}

// ---- the input arm -----------------------------------------------------

/// Type `text` the way a keyboard does: a space is the space bar, so what
/// Writer does at a word boundary (autoformat, autocorrect) happens here
/// as it does for a person.
fn keyboard(app: &mut App, text: &str) {
    for (index, word) in text.split(' ').enumerate() {
        if index > 0 {
            app.harness().key("Space");
        }
        app.harness().type_text(word);
    }
}

fn press(app: &mut App, key: &str, times: usize) {
    for _ in 0..times {
        app.harness().key(key);
    }
}

/// Select the `phrase` that ends `trailing` characters before the caret,
/// and press `shortcut` with Ctrl held.
fn format_phrase(app: &mut App, phrase: &str, trailing: usize, shortcut: &str) {
    press(app, "Left", trailing);
    app.harness().set_modifiers(&["shift".to_owned()]);
    press(app, "Left", phrase.chars().count());
    app.harness().set_modifiers(&["ctrl".to_owned()]);
    app.harness().key(shortcut);
    app.harness().set_modifiers(&[]);
    app.harness().key("End");
}

/// The input arm: a click into the page, then keys. Markdown prefixes make
/// the headings and the list, Ctrl+B and Ctrl+I the formats.
fn input_arm(receipt: &mut Receipt) -> App {
    let mut app = open();
    let page = app
        .harness()
        .targets()
        .get("word_processor")
        .copied()
        .expect("the editor is on screen");
    let started = std::time::Instant::now();
    // Into the empty body, well below the page header: a click there puts
    // the caret in the document's only paragraph.
    app.harness()
        .click_at(page.center().x, page.y0 + page.height() * 0.6);
    keyboard(&mut app, &format!("# {}", REFERENCE[0].text));
    app.harness().key("Enter");
    keyboard(&mut app, REFERENCE[1].text);
    format_phrase(&mut app, "steering group", " by the field team.".len(), "b");
    app.harness().key("Enter");
    keyboard(&mut app, &format!("## {}", REFERENCE[2].text));
    app.harness().key("Enter");
    keyboard(&mut app, &format!("- {}", REFERENCE[3].text));
    app.harness().key("Enter");
    keyboard(&mut app, REFERENCE[4].text);
    // Enter on an empty list item ends the list.
    press(&mut app, "Enter", 2);
    keyboard(&mut app, REFERENCE[5].text);
    format_phrase(&mut app, "3 November", ".".len(), "i");
    // Back to where a document opens with the caret, so the frame shows
    // what was typed rather than where typing stopped.
    press(&mut app, "Up", REFERENCE.len());
    app.harness().key("Home");
    app.settle();
    receipt.measure(
        "input_elapsed_ms",
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    receipt.record(
        "input.document_matches_reference",
        matches_reference(&snapshot(&app)),
    );
    app
}

// ---- pixels ------------------------------------------------------------

/// The share of pixels that differ between two frames of one size.
fn frame_difference(left: &[u8], right: &[u8]) -> Result<f64, String> {
    if left.len() != right.len() || left.is_empty() {
        return Err(format!(
            "frames of {} and {} bytes cannot be compared",
            left.len(),
            right.len()
        ));
    }
    let differing = left
        .chunks_exact(4)
        .zip(right.chunks_exact(4))
        .filter(|(left, right)| {
            left.iter()
                .zip(right.iter())
                .any(|(left, right)| left.abs_diff(*right) > PIXEL_TOLERANCE)
        })
        .count();
    #[allow(
        clippy::cast_precision_loss,
        reason = "a share of a frame's pixel count"
    )]
    Ok(differing as f64 / (left.len() / 4) as f64)
}

/// The share of pixels that are not the frame's ground (its most common
/// colour): where something — text, a caret, a control — is drawn, in any
/// theme.
fn ink(rgba: &[u8]) -> f64 {
    let mut counts = std::collections::HashMap::<&[u8], usize>::new();
    for pixel in rgba.chunks_exact(4) {
        let count = counts.entry(pixel).or_default();
        *count = count.saturating_add(1);
    }
    let Some(ground) = counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(pixel, _)| pixel)
    else {
        return 0.0;
    };
    let drawn = rgba
        .chunks_exact(4)
        .filter(|pixel| {
            pixel
                .iter()
                .zip(ground)
                .any(|(channel, ground)| channel.abs_diff(*ground) > PIXEL_TOLERANCE)
        })
        .count();
    #[allow(
        clippy::cast_precision_loss,
        reason = "a share of a frame's pixel count"
    )]
    let share = drawn as f64 / (rgba.len() / 4) as f64;
    share
}

/// Keep `app`'s frame as `name.png` in `TILO_WRITER_BENCHMARK_FRAMES`, when
/// it is set, for a person to look at.
fn keep_frame(app: &mut App, name: &str) {
    if let Some(directory) = std::env::var_os("TILO_WRITER_BENCHMARK_FRAMES") {
        let directory = Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        app.harness()
            .render_png(&directory.join(format!("{name}.png")));
    }
}

/// Keep a copy of an exported file in `TILO_WRITER_BENCHMARK_FRAMES`, when
/// it is set.
fn keep_file(path: &Path) {
    if let (Some(directory), Some(name)) = (
        std::env::var_os("TILO_WRITER_BENCHMARK_FRAMES"),
        path.file_name(),
    ) {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::copy(path, Path::new(&directory).join(name)).unwrap();
    }
}

fn same_picture(
    name: &str,
    left: &[u8],
    right: &[u8],
    receipt: &mut Receipt,
) -> Result<String, String> {
    let difference = frame_difference(left, right)?;
    receipt.measure(name, difference);
    if difference <= MAX_FRAME_DIFFERENCE {
        Ok(format!(
            "{:.3}% of pixels differ (at most {:.2}% allowed)",
            difference * 100.0,
            MAX_FRAME_DIFFERENCE * 100.0
        ))
    } else {
        Err(format!(
            "{:.3}% of pixels differ, more than {:.2}%",
            difference * 100.0,
            MAX_FRAME_DIFFERENCE * 100.0
        ))
    }
}

// ---- export and reopen -------------------------------------------------

fn open() -> App {
    ui::i18n::I18nManager::init("en-US");
    App::new().unwrap()
}

/// Save As through the published action, confirmed by the person as the
/// host's prompt for a destructive file write asks them to.
fn save_as(app: &mut App, path: &Path) -> Result<u64, String> {
    app.call_confirmed(
        "save-as",
        serde_json::json!({"path": path.to_string_lossy()}),
    )
    .map_err(|error| format!("Save As refused: {error}"))?;
    for _ in 0..10 {
        if path.exists() {
            break;
        }
        app.harness().relayout();
    }
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("{} was not written: {error}", path.display()))
}

fn docx_export(app: &mut App, path: &Path, receipt: &mut Receipt) -> Result<String, String> {
    let bytes = save_as(app, path)?;
    receipt.measure("docx_bytes", bytes);
    let file = std::fs::read(path).map_err(|error| error.to_string())?;
    let container = file.starts_with(b"PK\x03\x04");
    let body = file
        .windows(b"word/document.xml".len())
        .any(|window| window == b"word/document.xml");
    if container && body {
        Ok(format!(
            "{bytes} bytes, a ZIP container with word/document.xml"
        ))
    } else {
        Err(format!(
            "{bytes} bytes; ZIP container: {container}, word/document.xml: {body}"
        ))
    }
}

/// Every reference paragraph's text in `page`, in order. Whitespace is not
/// compared: a PDF places glyphs, and where a text layer infers a space
/// between two of them (`1 8` for a kerned `18`) is the reader's guess.
fn text_in_order(page: &str) -> Result<String, String> {
    let squashed = |text: &str| text.split_whitespace().collect::<String>();
    let page = squashed(page);
    let mut from = 0;
    for block in &REFERENCE {
        let text = squashed(block.text);
        match page.get(from..).and_then(|rest| rest.find(&text)) {
            Some(at) => from = from.saturating_add(at).saturating_add(text.len()),
            None => {
                return Err(format!(
                    "{:?} is missing or out of order in {page:?}",
                    block.text
                ));
            }
        }
    }
    Ok(format!("all {} paragraphs, in order", REFERENCE.len()))
}

fn pdf_checks(app: &mut App, path: &Path, receipt: &mut Receipt) {
    let exported = save_as(app, path).and_then(|bytes| {
        receipt.measure("pdf_bytes", bytes);
        let file = std::fs::read(path).map_err(|error| error.to_string())?;
        if file.starts_with(b"%PDF") {
            Ok(format!("{bytes} bytes, a PDF"))
        } else {
            Err(format!("{bytes} bytes that are not a PDF"))
        }
    });
    let written = exported.is_ok();
    receipt.record("pdf.exported", exported);
    if !written {
        for id in PDF_READS {
            receipt.record(id, Err("no PDF to reopen".into()));
        }
        return;
    }
    // The PDF viewer's own engine, the one it opens a file with on every
    // platform this runs on.
    let opened = std::fs::read(path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            tilo_pdf::Engine::portable()
                .open_bytes(&bytes)
                .map_err(|error| format!("the PDF viewer cannot open it: {error}"))
        });
    let document = match opened {
        Ok(document) => document,
        Err(error) => {
            for id in PDF_READS {
                receipt.record(id, Err(error.clone()));
            }
            return;
        }
    };
    receipt.measure("pdf_pages", document.page_count());
    let text = document
        .text(0)
        .map_err(|error| error.to_string())
        .and_then(|layer| {
            let page = layer.text_for(&layer.select_range(0..layer.len()));
            text_in_order(&page)
        })
        .and_then(|found| {
            if document.page_count() == 1 {
                Ok(format!("one page; {found}"))
            } else {
                Err(format!(
                    "{} pages for a one-page report",
                    document.page_count()
                ))
            }
        });
    receipt.record("pdf.reopened_text", text);
    let pixels = document
        .page_size(0)
        .ok_or_else(|| "no first page".to_owned())
        .and_then(|size| {
            document
                .render(0, &tilo_pdf::RenderOptions::fit_width(size, 600))
                .map_err(|error| error.to_string())
        })
        .and_then(|bitmap| {
            let pixels = bitmap.pixels();
            let share = ink(pixels);
            receipt.measure("pdf_ink", share);
            let paper = pixels.get(..4) == Some(&[255, 255, 255, 255][..]);
            if paper && (0.001..0.2).contains(&share) {
                Ok(format!(
                    "white paper, {:.2}% of a {}x{} render is text",
                    share * 100.0,
                    bitmap.width(),
                    bitmap.height()
                ))
            } else {
                Err(format!("white paper: {paper}; ink {share:.4}"))
            }
        });
    receipt.record("pdf.page_pixels", pixels);
    let formats = pdf_formats(&document, &snapshot(app), receipt);
    receipt.record("pdf.inline_formats", formats);
}

/// What is read back from the PDF, each a qualification.
const PDF_READS: [&str; 3] = ["pdf.reopened_text", "pdf.page_pixels", "pdf.inline_formats"];

/// The first page of `pdf`, rendered 600 pixels wide.
fn pdf_page(pdf: &tilo_pdf::Document) -> Result<Vec<u8>, String> {
    let size = pdf.page_size(0).ok_or_else(|| "no first page".to_owned())?;
    pdf.render(0, &tilo_pdf::RenderOptions::fit_width(size, 600))
        .map(|bitmap| bitmap.pixels().to_vec())
        .map_err(|error| error.to_string())
}

/// `document` exported to PDF directly and rendered: the control the
/// exported file is compared with.
fn render_pdf(document: &DocSnapshot) -> Result<Vec<u8>, String> {
    let bytes = tilo_doc_pdf::render_pdf(document, &tilo_content::NoContent)
        .map_err(|error| error.to_string())?;
    let pdf = tilo_pdf::Engine::portable()
        .open_bytes(&bytes)
        .map_err(|error| error.to_string())?;
    pdf_page(&pdf)
}

/// Each formatted phrase shows in the PDF: the exported page is the page of
/// the document as it is, and is not the page of the document without that
/// one format.
fn pdf_formats(
    exported: &tilo_pdf::Document,
    document: &DocSnapshot,
    receipt: &mut Receipt,
) -> Result<String, String> {
    let shown = pdf_page(exported)?;
    let control = frame_difference(&shown, &render_pdf(document)?)?;
    if control > 0.0 {
        return Err(format!(
            "the export differs from the document's own page in {:.3}% of pixels",
            control * 100.0
        ));
    }
    let mut seen = Vec::new();
    for (index, expected) in REFERENCE.iter().enumerate() {
        for &(name, phrase) in expected.marks {
            let mut variant = document.clone();
            if let Some(block) = variant.blocks.get_mut(index) {
                block.marks.retain(|mark| mark.name != name);
            }
            let difference = frame_difference(&shown, &render_pdf(&variant)?)?;
            receipt.measure(&format!("pdf_{name}_difference"), difference);
            if difference <= 0.0 {
                return Err(format!("{phrase:?} looks the same without {name}"));
            }
            seen.push(format!(
                "{name} on {phrase:?} ({:.3}% of the page)",
                difference * 100.0
            ));
        }
    }
    Ok(format!("drawn: {}", seen.join(", ")))
}

// ---- the benchmark -----------------------------------------------------

#[test]
fn writer_agent_benchmark_qualifies_against_the_reference() {
    let mut receipt = Receipt::default();
    let scratch = tempfile::tempdir().unwrap();
    let scratch_path = scratch.path().to_owned();

    let mut typed = typed_arm(&mut receipt);
    let typed_frame = typed.harness().render_rgba();
    keep_frame(&mut typed, "typed-arm");
    let blank_frame = open().harness().render_rgba();
    let drawn = frame_difference(&blank_frame, &typed_frame).and_then(|share| {
        receipt.measure("editor_drawn_share", share);
        if share >= MIN_DRAWN {
            Ok(format!(
                "{:.2}% of the frame differs from a blank document's",
                share * 100.0
            ))
        } else {
            Err(format!(
                "only {:.3}% of the frame differs from a blank document's",
                share * 100.0
            ))
        }
    });
    receipt.record("visual.editor_draws_the_document", drawn);

    let mut input = input_arm(&mut receipt);
    let input_frame = input.harness().render_rgba();
    keep_frame(&mut input, "input-arm");
    let agree = same_picture(
        "arms_frame_difference",
        &typed_frame,
        &input_frame,
        &mut receipt,
    );
    receipt.record("visual.arms_draw_the_same_page", agree);

    let docx = scratch_path.join("Quarterly Field Report.docx");
    let exported = docx_export(&mut typed, &docx, &mut receipt);
    let docx_written = exported.is_ok();
    receipt.record("docx.exported", exported);
    if docx_written {
        ui::i18n::I18nManager::init("en-US");
        match App::launched(&[&docx.to_string_lossy()]) {
            Ok(mut reopened) => {
                receipt.record(
                    "docx.reopened_matches_reference",
                    matches_reference(&snapshot(&reopened)),
                );
                let reopened_frame = reopened.harness().render_rgba();
                keep_frame(&mut reopened, "docx-reopened");
                let same = same_picture(
                    "docx_reopen_frame_difference",
                    &typed_frame,
                    &reopened_frame,
                    &mut receipt,
                );
                receipt.record("docx.reopened_draws_the_same_page", same);
            }
            Err(error) => {
                let error = format!("Writer cannot open it: {error:?}");
                receipt.record("docx.reopened_matches_reference", Err(error.clone()));
                receipt.record("docx.reopened_draws_the_same_page", Err(error));
            }
        }
    } else {
        receipt.record(
            "docx.reopened_matches_reference",
            Err("no DOCX to reopen".into()),
        );
        receipt.record(
            "docx.reopened_draws_the_same_page",
            Err("no DOCX to reopen".into()),
        );
    }

    let pdf = scratch_path.join("Quarterly Field Report.pdf");
    pdf_checks(&mut typed, &pdf, &mut receipt);
    keep_file(&docx);
    keep_file(&pdf);

    drop((typed, input));
    let closed = scratch.close();
    receipt.record(
        "cleanup.scratch_removed",
        match closed {
            Ok(()) if !scratch_path.exists() => {
                Ok("the exported files and their directory are gone".into())
            }
            Ok(()) => Err(format!("{} is still there", scratch_path.display())),
            Err(error) => Err(error.to_string()),
        },
    );

    let json = receipt.to_json();
    let rendered = serde_json::to_string_pretty(&json).unwrap();
    println!("{rendered}");
    if let Some(path) = std::env::var_os("TILO_WRITER_BENCHMARK_RECEIPT") {
        std::fs::write(&path, format!("{rendered}\n")).unwrap();
    }
    let failed: Vec<String> = receipt
        .qualifications
        .iter()
        .filter(|q| !q.passed)
        .map(|q| format!("{}: {}", q.id, q.observed))
        .collect();
    assert!(failed.is_empty(), "unqualified:\n{}", failed.join("\n"));
}

#[test]
fn a_document_that_misses_the_reference_is_not_qualified() {
    let mut app = open();
    app.call_by(
        agent(),
        DOCUMENT,
        "type-text",
        serde_json::json!({"text": REFERENCE[0].text}),
    )
    .unwrap();
    let verdict = matches_reference(&snapshot(&app));
    assert!(
        verdict
            .as_ref()
            .is_err_and(|why| why.contains("first difference at paragraph 0")),
        "an unstyled title alone is not the report: {verdict:?}"
    );
}

/// The pixel comparison sees a lost format: the report without its italic
/// date is not the same picture as the report.
#[test]
fn a_page_missing_one_format_is_not_the_same_picture() {
    let mut whole = open();
    assert!(run(&mut whole, &plan()).is_empty());
    let mut unformatted = open();
    let without_italic: Vec<_> = plan()
        .into_iter()
        .filter(|(id, arguments)| !(*id == "set-format" && arguments["name"] == "em"))
        .collect();
    assert!(run(&mut unformatted, &without_italic).is_empty());
    let mut receipt = Receipt::default();
    let verdict = same_picture(
        "difference",
        &whole.harness().render_rgba(),
        &unformatted.harness().render_rgba(),
        &mut receipt,
    );
    assert!(verdict.is_err(), "{verdict:?}");
}
