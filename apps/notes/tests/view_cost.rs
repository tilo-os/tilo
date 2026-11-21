//! What reading a long note's view costs where no window is open: the
//! workspace's server holds the notebook in its app view, and an agent asks
//! for the view of one note (`outline` with the note as its anchor), as
//! `tilo view` and an agent's `tilo_view` do.
//!
//! The answer carries what the view shows and counts the rest, the note's
//! lines are laid out where the window shows them, and publishing what the
//! view offers does not walk every paragraph's controls. Time budgets apply
//! to optimised builds only; a debug build prints the numbers.
//!
//! `cargo test -p notes --release --test it view_cost -- --nocapture`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "a cost test reports on stderr and fails loudly"
)]

use std::time::{Duration, Instant};

use notes::{NoteList, NotesShell, PageId};
use ui::collaboration::DocumentMutation;
use ui::framework::testing::TestApp;
use ui::framework::{AttachedView, definition};
use ui::tilo_resource::Resource as _;
use ui::tilo_resource::profiling::Sample;

/// Paragraphs of the long note: thousands of lines of outline, far more
/// than one answer carries.
const PARAGRAPHS: usize = 1_500;

/// A long note: paragraphs of about 270 characters, a heading every fifty.
fn long_body(paragraphs: usize) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    for n in 0..paragraphs {
        if n % 50 == 0 {
            let _ = write!(body, "## Section {n}\n\n");
        }
        let _ = write!(body, "Paragraph {n}: ");
        for word in 0..40 {
            body.push_str(["server ", "workspace ", "record ", "sync "][(n + word) % 4]);
        }
        body.push_str("\n\n");
    }
    body
}

/// The notebook's view as a workspace's server holds it, over what `app`
/// wrote: the notebook and every part of its notes handed over.
fn held(app: &TestApp<NotesShell>) -> AttachedView {
    let definition = definition::<NotesShell>();
    let mut view = AttachedView::open(
        definition,
        ui::framework::testing::TEST_WORKSPACE,
        ui::resource::DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
        app.read_document(|doc| doc.clone().save()),
        &ui::permissions::MemberId::person("ada"),
        ui::permissions::PermissionContext::permissive(),
        false,
    )
    .unwrap();
    for (index, _) in app.chain_parts::<NoteList>() {
        let part = notes::note_list::chain().part(index);
        let bytes = app
            .read_document_of(&part, |doc| doc.clone().save())
            .unwrap();
        view.merge_resource(&part, &DocumentMutation::Snapshot(bytes))
            .unwrap();
    }
    view
}

/// The view of note `page`, as an agent reads it, how long that and the
/// listing after it took, and what they did.
fn view_of(view: &mut AttachedView, page: PageId) -> (serde_json::Value, Duration, Vec<Sample>) {
    let address = tilo_actions::ActionAddress::new(notes::Notebook::KIND, "outline");
    let mut arguments = serde_json::Map::new();
    let anchor = ui::tilo_resource::items::item_resource_id::<NoteList>(
        "pages",
        &notes::notebook::page_id(page),
    );
    arguments.insert("anchor".into(), serde_json::json!(anchor));
    let started = Instant::now();
    let (output, phases) = ui::tilo_resource::profiling::observe(|| {
        let output = view.invoke(&address, &arguments, false).unwrap();
        // Whoever reads the view next lays out what the read moved, and
        // lists what it offers.
        let _ = view.resources();
        output
    });
    (output.value.unwrap(), started.elapsed(), phases)
}

/// How long the scope `name` took in `phases`.
fn spent(phases: &[Sample], name: &str) -> Duration {
    phases
        .iter()
        .filter(|phase| phase.phase == name)
        .map(|phase| Duration::from_nanos(u64::try_from(phase.elapsed_ns).unwrap_or(u64::MAX)))
        .sum()
}

fn report(label: &str, value: &serde_json::Value, took: Duration, phases: &[Sample]) {
    eprintln!(
        "perf view {label}: {took:?}, {} lines shown, {} counted, {} bytes",
        value["nodes"],
        value["omitted"],
        value.to_string().len()
    );
    let mut phases: Vec<&Sample> = phases.iter().collect();
    phases.sort_by_key(|phase| std::cmp::Reverse(phase.elapsed_ns));
    for phase in phases.iter().take(8) {
        eprintln!(
            "  {:36} {:>8.1} ms  x{}",
            phase.phase,
            phase.elapsed_ns as f64 / 1e6,
            phase.calls
        );
    }
}

/// The lines of an outline's tree, in order.
fn lines(tree: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut all = Vec::new();
    for line in tree.as_array().into_iter().flatten() {
        all.push(line);
        all.extend(lines(&line["children"]));
    }
    all
}

#[test]
fn a_long_notes_view_answers_what_it_shows() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let small = crate::framework_support::add_note(&mut app, "Small", "A short note.");
    let long = crate::framework_support::add_note(&mut app, "Long", &long_body(PARAGRAPHS));
    app.settle();
    let mut view = held(&app);
    let _ = view.resources();

    let _ = view_of(&mut view, small);
    let (small_view, small_took, small_phases) = view_of(&mut view, small);
    report("small", &small_view, small_took, &small_phases);
    let (first, first_took, first_phases) = view_of(&mut view, long);
    report("long, first", &first, first_took, &first_phases);
    let (again, took, phases) = view_of(&mut view, long);
    report("long, again", &again, took, &phases);

    for read in [&first, &again] {
        // The answer is what the text shows: the lines past it are counted.
        let bytes = read.to_string().len();
        assert!(
            bytes < 64 * 1024,
            "a long note's view answers in {bytes} bytes"
        );
        let shown = lines(&read["tree"]);
        assert_eq!(
            read["nodes"].as_u64(),
            Some(shown.len() as u64),
            "the lines counted are the tree's"
        );
        assert!(
            read["omitted"].as_u64().unwrap_or(0) > PARAGRAPHS as u64,
            "the rest of the note is counted: {}",
            read["omitted"]
        );
        // The note's first paragraph stands where the window shows it.
        let paragraph = shown
            .iter()
            .find(|line| {
                line["value"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Paragraph 0:"))
            })
            .unwrap_or_else(|| panic!("the first paragraph's line: {}", read["outline"]));
        let width = paragraph["rect"][2].as_f64().unwrap_or(0.0);
        let height = paragraph["rect"][3].as_f64().unwrap_or(0.0);
        assert!(
            width > 0.0 && height > 0.0,
            "the paragraph is laid out: {paragraph}"
        );
    }

    if !cfg!(debug_assertions) {
        // Publishing what the view offers builds the nodes controls are
        // published from, not a row for every paragraph: no dearer beside a
        // long note than beside a short one, within a few milliseconds.
        let controls = spent(&phases, "ui.resources.controls");
        let small_controls = spent(&small_phases, "ui.resources.controls");
        assert!(
            controls < small_controls * 4 + Duration::from_millis(5),
            "publishing controls took {controls:?} beside a long note, {small_controls:?} beside a short one"
        );
        // A warm view of the long note is a small multiple of a short one's.
        assert!(
            took < small_took * 10 + Duration::from_millis(150),
            "a long note's view took {took:?}, a short one's {small_took:?}"
        );
    }
}
