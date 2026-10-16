#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "test assertions, counters and timings"
)]

//! A large view bound by paths follows a one-field edit by running only the
//! readers of that field, and the framework's work per edit stays bounded.

use tilo_app::prelude::*;
use tilo_app::tilo_resource::profiling as phases;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub books: Collection<Book>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Book {
    pub title: String,
    pub read: bool,
}

#[resource(kind = "test.shelf", label = "Shelf")]
impl Shelf {
    /// Add books.
    #[action]
    pub fn add_many(&mut self, cx: &mut ActionCx, count: usize) {
        for n in 0..count {
            self.books.push(
                cx.id(),
                Book {
                    title: format!("Book {n}"),
                    read: false,
                },
            );
        }
    }

    /// Mark a book read.
    #[action]
    pub fn mark(&mut self, book: &Id, read: bool) -> Result<(), &'static str> {
        self.books.get_mut(book).ok_or("no such book")?.read = read;
        Ok(())
    }
}

struct ShelfApp;

fn row(row: &mut ViewCx<Shelf, Book>) -> Element {
    let p = row.paths();
    let read = row.checkbox("read", p.read);
    let title = row.input(p.title);
    Box::new(tilo_app::row![read, title])
}

/// The same rows bound by reading closures and an item verb.
struct VerbShelfApp;

fn verb_row(row: &mut ViewCx<Shelf, Book>) -> Element {
    let id = row.item().cloned().unwrap();
    let control = format!("mark-{id}");
    let read = row.control_with(
        |shown| {
            Checkbox::from_state(shown)
                .with_id(&control)
                .with_label("read")
        },
        |book: &Book| book.read,
        Shelf::MARK.map(move |read| (id.clone(), read)),
    );
    let title = row.text(|book: &Book| book.title.clone());
    Box::new(tilo_app::row![read, title])
}

impl ResourceApp for VerbShelfApp {
    type Resource = Shelf;
    const ID: &'static str = "so.tilo.test-verb-shelf";
    const TITLE: &'static str = "Shelf";

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        Box::new(Column::new().push(Box::new(cx.each(|shelf| &shelf.books, verb_row))))
    }
}

impl ResourceApp for ShelfApp {
    type Resource = Shelf;
    const ID: &'static str = "so.tilo.test-shelf";
    const TITLE: &'static str = "Shelf";

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        Box::new(Column::new().push(Box::new(cx.each(|shelf| &shelf.books, row))))
    }
}

const BOOKS: usize = 1000;

fn mark(book: &Id, read: bool) -> Invoke<Shelf> {
    Invoke::edit(&Shelf::paths().books.item(book).read.set(read)).unwrap()
}

fn readers(samples: &[phases::Sample]) -> usize {
    samples
        .iter()
        .filter(|sample| sample.phase == "framework.reader")
        .map(|sample| sample.calls)
        .sum()
}

#[test]
fn a_one_field_edit_in_a_large_collection_runs_only_its_readers() {
    let mut app = TestApp::<ShelfApp>::new().unwrap();
    app.invoke(Shelf::ADD_MANY.with(BOOKS)).unwrap();
    let book = app.state().books.ids().nth(BOOKS / 2).cloned().unwrap();

    let ((), samples) = phases::observe(|| app.invoke(mark(&book, true)).unwrap());
    assert!(app.state().books.get(&book).unwrap().read);
    let woken = readers(&samples);
    assert!(
        (1..=4).contains(&woken),
        "one edit woke {woken} readers of {} bound controls",
        2 * BOOKS
    );
    let checkbox = format!("books-{book}-read");
    let inspection = app.harness().inspection_json().unwrap();
    assert!(inspection.contains(&checkbox), "{inspection}");

    // Undo is a change like any other: it wakes the same readers.
    let ((), samples) = phases::observe(|| assert!(app.undo()));
    assert!(!app.state().books.get(&book).unwrap().read);
    assert!(readers(&samples) <= 4, "undo woke {}", readers(&samples));

    // Adding an item changes membership: the list reconciles, and still only
    // a handful of readers run.
    let ((), samples) = phases::observe(|| app.invoke(Shelf::ADD_MANY.with(1)).unwrap());
    assert_eq!(app.state().books.len(), BOOKS + 1);
    assert!(
        readers(&samples) <= 4,
        "an insert woke {}",
        readers(&samples)
    );
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn measure_one_field_edit_in_a_large_bound_view() {
    eprintln!("MEASURE --- bound by path, edited at the path");
    measure::<ShelfApp>(mark);
    eprintln!("MEASURE --- bound by closure, edited by an item verb");
    measure::<VerbShelfApp>(|book, read| Shelf::MARK.with((book.clone(), read)));
}

fn measure<A: ResourceApp<Resource = Shelf>>(edit: impl Fn(&Id, bool) -> Invoke<Shelf>) {
    let mut app = TestApp::<A>::new().unwrap();
    app.invoke(Shelf::ADD_MANY.with(BOOKS)).unwrap();
    let book = app.state().books.ids().nth(BOOKS / 2).cloned().unwrap();
    let mut totals = std::collections::BTreeMap::<&'static str, (u128, usize)>::new();
    let rounds: usize = 20;
    app.harness().begin_profiling();
    let mut frames = Vec::new();
    for round in 0..rounds {
        let start = tilo_app::time::now();
        let ((), samples) = phases::observe(|| {
            app.invoke(edit(&book, round % 2 == 0)).unwrap();
        });
        let entry = totals.entry("invoke").or_default();
        entry.0 += start.elapsed().as_nanos();
        entry.1 += 1;
        frames.extend(app.harness().take_profile_samples());
        for sample in samples {
            let entry = totals.entry(sample.phase).or_default();
            entry.0 += sample.elapsed_ns;
            entry.1 += 1;
        }
    }
    let sum = |pick: fn(&tilo_app::profiling::FrameTiming) -> u64| {
        frames.iter().map(pick).sum::<u64>() as f64 / 1e3 / rounds as f64
    };
    eprintln!(
        "MEASURE frames: {} per edit; sync {:.3} ms, layout {:.3}+{:.3} ms, draw {:.3} ms",
        frames.len() / rounds,
        sum(|f| f.sync_us),
        sum(|f| f.build_layout_us),
        sum(|f| f.apply_layout_us),
        sum(|f| f.draw_us),
    );
    let start = tilo_app::time::now();
    for _ in 0..rounds {
        app.harness().relayout();
    }
    eprintln!(
        "MEASURE idle relayout: {:.3} ms",
        start.elapsed().as_nanos() as f64 / 1e6 / rounds as f64
    );
    let start = tilo_app::time::now();
    for _ in 0..rounds {
        let _ = app.actions();
    }
    eprintln!(
        "MEASURE action survey: {:.3} ms",
        start.elapsed().as_nanos() as f64 / 1e6 / rounds as f64
    );
    for (phase, (ns, count)) in &totals {
        eprintln!(
            "MEASURE {phase}: {:.3} ms per edit, {} scopes per edit",
            *ns as f64 / 1e6 / rounds as f64,
            count / rounds
        );
    }
}
