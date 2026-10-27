use std::sync::{Arc, Mutex};

use ui_core::background::Pumped;
use ui_core::time::Duration;

use super::*;

const SONG: &str = "test.song";
const ALBUM: &str = "test.album";

fn kinds() -> Vec<KindDecl> {
    vec![
        KindDecl::new(SONG, "Song", "Songs", Form::Row)
            .field(FieldDecl::title("title", "Title"))
            .field(
                FieldDecl::new("artist", "Artist")
                    .role(FieldRole::Subtitle)
                    .searchable()
                    .filter(FilterKind::Value),
            )
            .field(
                FieldDecl::new("year", "Year")
                    .role(FieldRole::Date)
                    .filter(FilterKind::Value),
            )
            .field(FieldDecl::new("liked", "Liked").filter(FilterKind::Toggle)),
        KindDecl::new(ALBUM, "Album", "Albums", Form::Tile)
            .field(FieldDecl::title("title", "Title"))
            .field(
                FieldDecl::new("artist", "Artist")
                    .role(FieldRole::Subtitle)
                    .searchable()
                    .filter(FilterKind::Value),
            ),
    ]
}

fn song(id: &str, title: &str, artist: &str, liked: bool) -> Item {
    Item::new(SONG, id)
        .with("title", title)
        .with("artist", artist)
        .flag("liked", liked)
}

fn album(id: &str, title: &str, artist: &str) -> Item {
    Item::new(ALBUM, id)
        .with("title", title)
        .with("artist", artist)
}

fn library() -> Vec<Item> {
    vec![
        song("s1", "Runaway", "Aurora", true),
        song("s2", "Running Up That Hill", "Kate Bush", false),
        song("s3", "Murder Song", "Aurora", false),
        album("a1", "All My Demons Greeting Me as a Friend", "Aurora"),
    ]
}

#[test]
fn typed_tokens_are_the_same_chips_as_picked_ones() {
    let kinds = kinds();
    let typed = QuerySpec::parse("run artist:aurora", &[], &kinds);
    assert_eq!(typed.text, "run");
    assert_eq!(typed.chips.len(), 1);
    assert_eq!(typed.chips[0].key, "artist");
    assert_eq!(typed.chips[0].value.as_deref(), Some("aurora"));
    assert!(typed.chips[0].typed);
    let picked = QuerySpec::text("run").toggled(&Chip::value("artist", "aurora"));
    assert_eq!(typed.key(), picked.key(), "both ask the same question");

    let quoted = QuerySpec::parse(r#"artist:"kate bush" liked:yes"#, &[], &kinds);
    assert_eq!(quoted.text, "");
    assert_eq!(quoted.chips[0].value.as_deref(), Some("kate bush"));
    assert_eq!(
        quoted.chips[1],
        Chip {
            typed: true,
            ..Chip::toggle("liked")
        }
    );

    // A key by its label works too; an unknown key, or no value yet, is text.
    let by_label = QuerySpec::parse("Artist:aurora", &[], &kinds);
    assert_eq!(by_label.chips.len(), 1);
    let plain = QuerySpec::parse("feat:x artist:", &[], &kinds);
    assert_eq!(plain.text, "feat:x artist:");
    assert!(plain.chips.is_empty());
}

#[test]
fn a_value_chip_matches_at_the_start_of_a_word() {
    let chip = Chip::value("artist", "bush");
    assert!(chip.admits(&song("x", "t", "Kate Bush", false)));
    assert!(!chip.admits(&song("x", "t", "Ambush", false)));
    assert!(
        !chip.admits(&album("y", "t", "")),
        "an item without the field does not pass"
    );
    assert!(Chip::toggle("liked").admits(&song("x", "t", "a", true)));
    assert!(!Chip::toggle("liked").admits(&song("x", "t", "a", false)));
}

#[test]
fn a_query_refines_an_earlier_one_only_with_the_same_chips() {
    let earlier = QuerySpec::text("auro");
    assert!(QuerySpec::text("aurora").refines(&earlier));
    assert!(!QuerySpec::text("auro").refines(&earlier));
    assert!(!QuerySpec::text("bush").refines(&earlier));
    let narrowed = QuerySpec::text("aurora").toggled(&Chip::toggle("liked"));
    assert!(!narrowed.refines(&earlier));
}

#[derive(Clone, Debug, PartialEq)]
enum TestView {
    Library,
    Album(&'static str),
    Search(String),
}

#[test]
fn back_returns_to_the_view_before_as_it_was_left() {
    let mut stack = ViewStack::new(TestView::Library);
    let left = ViewPlace {
        scroll: 1240.0,
        selection: vec!["s2".into(), "s3".into()],
        focus: Some("s3".into()),
    };
    assert!(stack.open(TestView::Search("a".into()), left.clone()));
    // Typing changes the query without steps to go back through.
    stack.replace(TestView::Search("aurora".into()));
    assert_eq!(stack.previous(), Some(&TestView::Library));
    let back = stack
        .back(ViewPlace::default())
        .expect("the library is behind");
    assert_eq!(back.view, TestView::Library);
    assert_eq!(back.place, left);
    assert!(stack.can_go_forward());
    let forward = stack.forward(left).expect("the search is ahead").clone();
    assert_eq!(forward.view, TestView::Search("aurora".into()));
    // Opening something new drops what Forward held.
    assert!(stack.open(TestView::Album("a1"), ViewPlace::default()));
    assert!(!stack.can_go_forward());
    assert!(!stack.open(TestView::Album("a1"), ViewPlace::default()));
    stack.forget(|view| matches!(view, TestView::Search(_)));
    assert_eq!(stack.previous(), Some(&TestView::Library));
}

#[test]
fn local_results_are_there_when_the_query_is_set_grouped_by_type() {
    let mut engine = QueryEngine::new(kinds());
    engine.sync_local(library());
    assert!(engine.set_query(QuerySpec::text("aurora")));
    let results = engine.results();
    let groups: Vec<_> = results.local.iter().map(|group| group.kind).collect();
    assert_eq!(groups, [SONG, ALBUM], "groups follow the types' order");
    assert_eq!(results.local[0].label, "Songs");
    assert_eq!(results.local[1].form, Form::Tile);
    assert!(engine.timings().local.is_some());
    assert!(
        !engine.set_query(QuerySpec::text("aurora")),
        "asked already"
    );
}

#[test]
fn chips_narrow_and_a_query_of_chips_alone_lists_what_they_admit() {
    let mut engine = QueryEngine::new(kinds());
    engine.sync_local(library());
    engine.set_query(QuerySpec::text("").toggled(&Chip::toggle("liked")));
    let ids: Vec<_> = engine
        .results()
        .order()
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert_eq!(ids, ["s1"]);
    engine.set_query(QuerySpec::parse("song artist:aurora", &[], &kinds()));
    let ids: Vec<_> = engine
        .results()
        .order()
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert_eq!(ids, ["s3"]);
}

#[test]
fn a_change_to_the_local_items_changes_the_rows_where_they_stand() {
    let mut engine = QueryEngine::new(kinds());
    engine.sync_local(library());
    engine.set_query(QuerySpec::text("aurora"));
    let before = engine.results().order();
    // Liked elsewhere: the row shows it liked in its place.
    let mut changed = library();
    changed[2] = song("s3", "Murder Song", "Aurora", true);
    engine.sync_local(changed.clone());
    assert_eq!(engine.results().order(), before);
    let s3 = engine.results().local[0]
        .items
        .iter()
        .find(|item| item.id == "s3")
        .expect("s3 still shown");
    assert!(s3.is_on("liked"));
    // Something new that matches waits for the next query; something gone
    // goes.
    changed.push(song("s9", "Aurora Borealis", "Aurora", false));
    changed.remove(0);
    engine.sync_local(changed);
    let ids: Vec<_> = engine
        .results()
        .order()
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert!(!ids.contains(&"s1".to_owned()) && !ids.contains(&"s9".to_owned()));
    engine.set_query(QuerySpec::text("aurora borealis"));
    assert!(
        engine
            .results()
            .order()
            .into_iter()
            .any(|(_, id)| id == "s9")
    );
}

/// A remote source that answers from a script and records what it was asked.
#[derive(Default)]
struct Scripted {
    pages: Mutex<Vec<(String, Vec<Vec<Item>>)>>,
    asked: Mutex<Vec<(String, bool)>>,
    warmed: Mutex<usize>,
    fail: Mutex<Option<String>>,
}

impl Scripted {
    fn answer(&self, text: &str, pages: Vec<Vec<Item>>) {
        self.pages.lock().unwrap().push((text.to_owned(), pages));
    }

    fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|(q, _)| q.clone())
            .collect()
    }
}

impl RemoteSource for Arc<Scripted> {
    fn id(&self) -> &'static str {
        "catalogue"
    }

    fn label(&self) -> String {
        "Catalogue".into()
    }

    fn warm(&self) {
        let mut warmed = self.warmed.lock().unwrap();
        *warmed = warmed.saturating_add(1);
    }

    fn search(
        &self,
        query: &QuerySpec,
        cancel: &Cancel,
        page: &mut dyn FnMut(Vec<Item>),
    ) -> Result<(), RemoteError> {
        self.asked
            .lock()
            .unwrap()
            .push((query.text.clone(), cancel.is_cancelled()));
        let fail = self.fail.lock().unwrap().clone();
        if let Some(message) = fail {
            return Err(RemoteError(message));
        }
        let pages = self
            .pages
            .lock()
            .unwrap()
            .iter()
            .find(|(text, _)| *text == query.text)
            .map(|(_, pages)| pages.clone())
            .unwrap_or_default();
        for items in pages {
            page(items);
        }
        Ok(())
    }
}

fn remote_engine() -> (QueryEngine, Arc<Scripted>, Pumped) {
    let pumped = Pumped::new();
    let source = Arc::new(Scripted::default());
    let engine = QueryEngine::new(kinds())
        .with_background(pumped.background())
        .with_remote(Arc::new(Arc::clone(&source)));
    (engine, source, pumped)
}

fn remote_ids(results: &QueryResults) -> Vec<String> {
    results.remote[0]
        .rows()
        .map(|item| item.id.clone())
        .collect()
}

#[test]
fn remote_rows_come_in_their_own_section_below_the_local_ones() {
    let (mut engine, source, pumped) = remote_engine();
    engine.sync_local(library());
    source.answer(
        "aurora",
        vec![vec![
            song("s1", "Runaway", "Aurora", false),
            song("r1", "Cure For Me", "Aurora", false),
        ]],
    );
    engine.set_query(QuerySpec::text("aurora"));
    let local_before = engine.results().local.clone();
    assert!(engine.results().remote[0].is_waiting());
    assert!(engine.results().pending(), "the loading line shows");
    engine.poll();
    pumped.run_pending();
    assert!(engine.poll());
    let results = engine.results();
    assert_eq!(
        results.local, local_before,
        "an answer never touches a local row"
    );
    assert_eq!(remote_ids(results), ["r1"], "s1 is local already");
    assert!(results.remote[0].is_answered());
    assert!(!results.pending());
    assert!(engine.timings().remote_first[0].is_some());
}

#[test]
fn a_newer_query_cancels_the_one_before_and_its_answer_never_shows() {
    let (mut engine, source, pumped) = remote_engine();
    source.answer("au", vec![vec![song("r-au", "Autumn", "X", false)]]);
    source.answer("aur", vec![vec![song("r-aur", "Aurelia", "Y", false)]]);
    engine.set_query(QuerySpec::text("au"));
    engine.poll();
    // Typed again within the throttle: the first is cancelled, and the new
    // query waits for its moment.
    engine.set_query(QuerySpec::text("aur"));
    engine.poll();
    pumped.run_pending();
    engine.poll();
    assert_eq!(
        source.asked.lock().unwrap().clone(),
        [("au".to_owned(), true)]
    );
    assert!(
        remote_ids(engine.results()).is_empty(),
        "the cancelled answer is not shown"
    );
    pumped.advance(THROTTLE);
    engine.poll();
    pumped.run_pending();
    engine.poll();
    assert_eq!(source.asked().last().map(String::as_str), Some("aur"));
    assert_eq!(remote_ids(engine.results()), ["r-aur"]);
}

#[test]
fn a_refined_query_shows_what_the_earlier_answer_still_matches_at_once() {
    let (mut engine, source, pumped) = remote_engine();
    source.answer(
        "auro",
        vec![vec![
            song("r1", "Aurora Borealis", "X", false),
            song("r2", "Auroch", "Y", false),
        ]],
    );
    engine.set_query(QuerySpec::text("auro"));
    engine.poll();
    pumped.run_pending();
    engine.poll();
    pumped.advance(Duration::from_secs(1));
    engine.set_query(QuerySpec::text("aurora"));
    assert_eq!(
        remote_ids(engine.results()),
        ["r1"],
        "shown before anything is asked"
    );
    assert!(engine.results().remote[0].is_waiting());
    // Going back to a query answered before shows its answer at once.
    engine.set_query(QuerySpec::text("auro"));
    assert!(engine.results().remote[0].is_answered());
    assert_eq!(remote_ids(engine.results()), ["r1", "r2"]);
    assert_eq!(engine.timings().remote_first[0], Some(Duration::ZERO));
}

#[test]
fn no_row_moves_under_the_pointer_while_remote_results_stream() {
    let (mut engine, source, pumped) = remote_engine();
    engine.sync_local(library());
    // An earlier answer leaves rows on screen for the refined query, then the
    // refined answer arrives in two pages, songs and albums interleaved.
    source.answer(
        "run",
        vec![vec![
            song("r1", "Runaway Love", "C", false),
            album("ra", "Runaround Album", "D"),
        ]],
    );
    source.answer(
        "runa",
        vec![
            vec![
                song("r2", "Runaround", "E", false),
                album("rb", "Runaround Sue", "F"),
            ],
            vec![
                song("r1", "Runaway Love", "C", false),
                song("r3", "Runaway Train", "G", false),
            ],
        ],
    );
    engine.set_query(QuerySpec::text("run"));
    engine.poll();
    pumped.run_pending();
    engine.poll();
    pumped.advance(Duration::from_secs(1));
    engine.set_query(QuerySpec::text("runa"));
    // The person points at the remote section.
    engine.hold(true);
    let mut shown = engine.results().order();
    let local = engine.results().local.clone();
    engine.poll();
    pumped.run_pending();
    while engine.poll() {}
    let now = engine.results().order();
    assert!(
        now.starts_with(&shown),
        "every row shown stays in its place: {shown:?} then {now:?}"
    );
    assert_eq!(engine.results().local, local);
    shown = now;
    // Let go: the finished answer is laid out in its own order, and the
    // earlier answer's rows it did not repeat leave.
    engine.hold(false);
    let laid_out = engine.results().order();
    assert_ne!(laid_out, shown);
    assert!(!laid_out.iter().any(|(_, id)| id == "ra"));
}

#[test]
fn a_remote_row_that_joins_the_library_stays_where_it_is() {
    let (mut engine, source, pumped) = remote_engine();
    engine.sync_local(library());
    source.answer(
        "aurora",
        vec![vec![song("r1", "Cure For Me", "Aurora", false)]],
    );
    engine.set_query(QuerySpec::text("aurora"));
    engine.poll();
    pumped.run_pending();
    engine.poll();
    let before = engine.results().order();
    let mut liked = library();
    liked.push(song("r1", "Cure For Me", "Aurora", true));
    engine.sync_local(liked);
    assert_eq!(
        engine.results().order(),
        before,
        "no row moved and none repeats"
    );
}

#[test]
fn an_unreachable_source_says_so_in_its_section() {
    let (mut engine, source, pumped) = remote_engine();
    *source.fail.lock().unwrap() = Some("No connection".into());
    engine.set_query(QuerySpec::text("aurora"));
    engine.poll();
    pumped.run_pending();
    engine.poll();
    assert_eq!(
        engine.results().remote[0].failure(),
        Some(&RemoteError("No connection".into()))
    );
    assert!(!engine.results().pending());

    // Asked again, it waits again and answers.
    *source.fail.lock().unwrap() = None;
    source.answer(
        "aurora",
        vec![vec![song("r1", "Cure For Me", "Aurora", false)]],
    );
    assert!(!engine.retry("elsewhere"), "only a source that failed");
    assert!(engine.retry("catalogue"));
    assert!(engine.results().remote[0].is_waiting());
    pumped.advance(THROTTLE);
    engine.poll();
    pumped.run_pending();
    engine.poll();
    assert!(engine.results().remote[0].is_answered());
    assert_eq!(remote_ids(engine.results()), ["r1"]);
}

#[test]
fn a_waiting_section_says_where_its_answer_comes_from_and_when_it_was_asked() {
    use ui_core::arriving::{Arrival, Source};
    let (mut engine, source, pumped) = remote_engine();
    source.answer(
        "aurora",
        vec![vec![song("r1", "Cure For Me", "Aurora", false)]],
    );
    engine.set_query(QuerySpec::text("aurora"));
    let section = &engine.results().remote[0];
    assert_eq!(
        section.state,
        Some(Arrival::Pending {
            from: Source::host("Catalogue")
        })
    );
    let clock = section.clock.clone().expect("timed from when it was asked");
    assert!(!clock.has_arrived());
    engine.poll();
    pumped.run_pending();
    engine.poll();
    let section = &engine.results().remote[0];
    assert!(section.is_answered());
    assert!(
        section
            .clock
            .as_ref()
            .is_some_and(ui_core::arriving::ArrivalClock::has_arrived),
        "its placeholder knows the answer landed"
    );
}

#[test]
fn focusing_the_field_warms_the_remote_sources_once() {
    let (mut engine, source, pumped) = remote_engine();
    engine.warm();
    engine.warm();
    pumped.run_pending();
    assert_eq!(*source.warmed.lock().unwrap(), 1);
    assert!(source.asked().is_empty(), "warming asks nothing");
}

#[test]
fn a_query_a_source_does_not_answer_leaves_its_section_idle() {
    let (mut engine, source, pumped) = remote_engine();
    engine.set_query(QuerySpec::text("").toggled(&Chip::toggle("liked")));
    engine.poll();
    pumped.run_pending();
    assert_eq!(engine.results().remote[0].state, None);
    assert!(source.asked().is_empty());
    engine.close();
    assert!(!engine.is_open());
    assert!(engine.results().remote.is_empty());
}

#[test]
fn one_chip_per_facet_reading_its_choice() {
    let mut engine = QueryEngine::new(kinds());
    engine.sync_local(library());
    engine.set_query(QuerySpec::text("r"));
    let offers = facet_offers(engine.kinds(), &engine.results().query, engine.results());
    let names: Vec<_> = offers.iter().map(|offer| offer.label.as_str()).collect();
    assert_eq!(
        names,
        ["Artist", "Year", "Liked"],
        "one chip per field, in order"
    );
    let artist = &offers[0];
    assert!(
        artist.values.len() >= 2,
        "every value is in the list: {:?}",
        artist.values
    );
    assert!(
        artist
            .values
            .windows(2)
            .all(|pair| pair[0].count >= pair[1].count),
        "the most shared first"
    );
    // Two artists chosen: either passes.
    let first = artist.values[0].value.clone();
    let second = artist.values[1].value.clone();
    let query = artist.toggled(&artist.toggled(&engine.results().query, &first), &second);
    engine.set_query(query.clone());
    let offers = facet_offers(engine.kinds(), &query, engine.results());
    assert_eq!(offers[0].label, format!("Artist: {first}, {second}"));
    assert!(offers[0].active && offers[0].clearable);
    assert_eq!(
        offers[0].values.iter().filter(|value| value.chosen).count(),
        2
    );
    // The values to add are still listed, counted without the filter's own
    // choice, the chosen ones checked.
    let offers = engine.facet_offers();
    assert!(offers[0].values.len() >= 2);
    assert_eq!(
        offers[0].values.iter().filter(|value| value.chosen).count(),
        2
    );
    let cleared = query.cleared("artist");
    assert!(
        cleared.chips.is_empty(),
        "the clear control takes the choice away"
    );
    let one = artist_only(&engine, &first);
    engine.set_query(one);
    let listed = engine.facet_offers();
    assert!(
        listed[0]
            .values
            .iter()
            .any(|value| value.value == second && !value.chosen),
        "with one artist chosen, the others are still there to add"
    );
    // A typed token is the same choice, cleared by editing the field.
    engine.set_query(QuerySpec::text("r"));
    let typed = QuerySpec::parse("r artist:aurora", &[], &kinds());
    let offers = facet_offers(engine.kinds(), &typed, engine.results());
    assert_eq!(
        offers[0].label, "Artist: Aurora",
        "named as the results name it"
    );
    assert!(offers[0].active && !offers[0].clearable);
}

fn artist_only(engine: &QueryEngine, artist: &str) -> QuerySpec {
    QuerySpec::text(engine.results().query.text.clone()).toggled(&Chip::value("artist", artist))
}

#[test]
fn a_kept_query_is_kept_once_and_named_after_what_was_searched() {
    let kinds = kinds();
    let query = QuerySpec::parse("run liked:yes", &[], &kinds);
    let name = KeptViews::default_name(&query, &kinds);
    assert_eq!(name, "run, Liked");
    let mut kept = KeptViews::default();
    let id = kept.keep(&query, name);
    assert_eq!(kept.keep(&query.clone(), "again"), id);
    assert_eq!(kept.kept_views.len(), 1);
    assert!(kept.rename(&id, "Running"));
    assert_eq!(
        kept.get(&id).map(|view| view.name.as_str()),
        Some("Running")
    );
    assert!(kept.forget(&id));
    assert!(kept.kept_views.is_empty());
}

#[test]
fn matching_actions_come_as_their_controls() {
    #[derive(Debug, serde::Serialize)]
    struct Noop;
    impl tilo_actions::Action for Noop {
        fn apply(&self, _: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            Ok(())
        }
    }
    let spec = |id: &str, label: &str| {
        tilo_actions::ActionSpec::operation(
            id,
            label,
            tilo_actions::OperationKind::Effect,
            "test.action",
            &Noop,
        )
        .unwrap()
    };
    let specs = vec![
        spec("shuffle", "Shuffle"),
        spec("new-playlist", "New playlist"),
        spec("agent-only", "Shuffle the index").for_audience(tilo_actions::Audience::Agents),
    ];
    let controls = vec![ControlDecl::switch("shuffle", true).home("player")];
    let hits = action_hits(&specs, &controls, "shuf", 5);
    assert_eq!(
        hits.len(),
        1,
        "agent-only actions are not a person's controls"
    );
    assert_eq!(hits[0].control.kind, ControlKind::Switch { on: true });
    assert_eq!(hits[0].control.home.as_deref(), Some("player"));
    let hits = action_hits(&specs, &controls, "playlist", 5);
    assert_eq!(hits[0].control, ControlDecl::button("new-playlist"));
    assert!(action_hits(&specs, &controls, "", 5).is_empty());
}

const BOOK: &str = "test.book";
const PASSAGE: &str = "test.passage";

fn book_kinds() -> Vec<KindDecl> {
    vec![
        KindDecl::new(BOOK, "Book", "Books", Form::Tile)
            .field(FieldDecl::title("title", "Title"))
            .field(
                FieldDecl::new("author", "Author")
                    .role(FieldRole::Subtitle)
                    .searchable()
                    .filter(FilterKind::Value),
            )
            .field(FieldDecl::new("book", "Book").filter(FilterKind::Value)),
        KindDecl::new(PASSAGE, "Passage", "Passages", Form::Row)
            .field(FieldDecl::title("title", "Where"))
            .field(FieldDecl::new("where", "In").role(FieldRole::Subtitle))
            .field(FieldDecl::new("excerpt", "Passage").role(FieldRole::Excerpt))
            .field(FieldDecl::new("book", "Book").filter(FilterKind::Value)),
    ]
}

/// A book's prose: a paragraph per segment, each a location of its own.
struct Prose {
    book: &'static str,
    paragraphs: Vec<&'static str>,
}

impl tilo_resource::SearchText for Prose {
    fn segment_count(&self) -> usize {
        self.paragraphs.len()
    }

    fn piece_count(&self, segment: usize) -> usize {
        usize::from(segment < self.paragraphs.len())
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<tilo_resource::SearchPiece<'_>> {
        (piece == 0).then_some(tilo_resource::SearchPiece {
            text: self.paragraphs.get(segment)?,
            space_before: segment > 0,
        })
    }

    fn locates(&self, segment: usize) -> bool {
        segment < self.paragraphs.len()
    }

    fn location(&self, segment: usize) -> Option<tilo_resource::SearchEntry> {
        Some(
            tilo_resource::SearchEntry::new(format!("Paragraph {}", segment.saturating_add(1)), "")
                .anchored(format!("{}#{segment}", self.book))
                .grouped("Passages")
                .subtitled(self.book),
        )
    }
}

fn book_entry(id: &'static str, title: &str, author: &str, prose: Vec<&'static str>) -> Item {
    let entry = tilo_resource::SearchEntry::new(title, "")
        .anchored(id)
        .grouped("Books")
        .subtitled(author)
        .facet("author", "Author", author)
        .facet("book", "Book", title)
        .field(
            "Whaling; Sea stories",
            tilo_resource::SearchWeight::Metadata,
        )
        .with_body(tilo_resource::SearchBody::new(Arc::new(Prose {
            book: id,
            paragraphs: prose,
        })));
    Item::from_entry(&book_kinds(), &entry).expect("a book entry is a book")
}

fn shelf() -> Vec<Item> {
    vec![
        book_entry(
            "moby",
            "Moby-Dick",
            "Herman Melville",
            vec![
                "Call me Ishmael. Some years ago, never mind how long precisely.",
                "There now is your insular city of the Manhattoes, belted round by wharves.",
            ],
        ),
        book_entry(
            "bartleby",
            "Bartleby",
            "Herman Melville",
            vec!["I would prefer not to, said Bartleby, at his desk by the wharves."],
        ),
    ]
}

#[test]
fn an_entry_is_an_item_of_the_type_its_group_names() {
    let item = &shelf()[0];
    assert_eq!(item.kind, BOOK);
    assert_eq!(item.id, "moby");
    assert_eq!(item.value("title"), Some("Moby-Dick"));
    assert_eq!(item.value("author"), Some("Herman Melville"));
    assert_eq!(item.value("book"), Some("Moby-Dick"));
    assert!(item.body.is_some());
    assert_eq!(
        item.entry().and_then(|entry| entry.anchor.as_deref()),
        Some("moby")
    );
    let mut engine = QueryEngine::new(book_kinds());
    engine.sync_local(shelf());
    engine.set_query(QuerySpec::text("sea stories"));
    assert!(
        engine
            .results()
            .order()
            .contains(&(BOOK, "moby".to_owned())),
        "what an entry is found by finds its item"
    );
}

#[test]
fn a_match_in_prose_is_a_hit_at_its_passage() {
    let pumped = Pumped::new();
    let mut engine = QueryEngine::new(book_kinds()).with_background(pumped.background());
    engine.sync_local(shelf());
    engine.set_query(QuerySpec::text("manhattoes"));
    assert!(
        engine.results().is_empty(),
        "the prose is indexed off the UI thread"
    );
    pumped.run_pending();
    assert!(
        engine.poll(),
        "its matches join the open query when it lands"
    );
    let results = engine.results();
    let passages = results
        .local
        .iter()
        .find(|group| group.kind == PASSAGE)
        .expect("a passage group");
    assert_eq!(passages.label, "Passages");
    let hit = &passages.items[0];
    assert_eq!(hit.id, "moby#1");
    assert_eq!(hit.value("title"), Some("Paragraph 2"));
    assert_eq!(
        hit.value("book"),
        Some("Moby-Dick"),
        "it carries its book's filters"
    );
    let excerpt = hit.value("excerpt").expect("the passage around the match");
    assert!(excerpt.contains("Manhattoes"), "{excerpt}");
    assert_eq!(
        hit.entry().and_then(|entry| entry.anchor.as_deref()),
        Some("moby#1"),
        "opening it opens the location"
    );

    // Narrowed to one book, the other book's passages leave.
    engine.set_query(QuerySpec::text("wharves"));
    assert_eq!(engine.results().order().len(), 2);
    engine.set_query(QuerySpec::text("wharves").toggled(&Chip::value("book", "Bartleby")));
    let ids: Vec<_> = engine
        .results()
        .order()
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert_eq!(ids, ["bartleby#0"]);
}

#[test]
fn an_excerpt_shows_the_words_around_the_match_cut_at_words() {
    let passage = "It was the best of times, it was the worst of times, it was the age of \
                   wisdom, it was the age of foolishness, it was the epoch of belief, it was \
                   the epoch of incredulity, it was the season of Light, it was the season of \
                   Darkness, it was the spring of hope, it was the winter of despair.";
    let shown = excerpt(passage, "darkness");
    assert!(shown.starts_with('\u{2026}'), "{shown}");
    assert!(shown.contains("Darkness"), "{shown}");
    assert!(!shown.contains("best of times"), "{shown}");
    let words = shown.trim_matches('\u{2026}');
    assert!(passage.contains(words), "cut at word boundaries: {shown}");
    assert_eq!(excerpt("Short.", "short"), "Short.");
    let shown = excerpt(passage, "foolishness");
    let at = shown.find("foolishness").expect("the match is shown");
    let after = shown.len() - at - "foolishness".len();
    assert!(
        at.abs_diff(after) < 24,
        "the match in the middle: {at} before, {after} after: {shown}"
    );
}

#[test]
fn prose_is_indexed_again_only_when_the_bodies_change() {
    let pumped = Pumped::new();
    let mut engine = QueryEngine::new(book_kinds()).with_background(pumped.background());
    let books = shelf();
    engine.sync_local(books.clone());
    assert_eq!(pumped.run_pending(), 1, "one prose index for the shelf");
    // A book renamed keeps its prose: only its fields are indexed again.
    let mut renamed = books.clone();
    renamed[1] = renamed[1].clone().with("title", "Bartleby, the Scrivener");
    engine.sync_local(renamed);
    assert_eq!(
        pumped.run_pending(),
        0,
        "the same bodies are not read again"
    );
    // A book gone changes the prose.
    engine.sync_local(books.into_iter().take(1));
    assert_eq!(pumped.run_pending(), 1);
    engine.set_query(QuerySpec::text("wharves"));
    let ids: Vec<_> = engine
        .results()
        .order()
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert_eq!(ids, ["moby#1"]);
}

/// A synced index of books whose text this device does not hold: each hit
/// a passage of a book, found by any word of it.
struct Elsewhere(Vec<(&'static str, &'static str, &'static str)>);

impl super::LocalSource for Elsewhere {
    fn search(&self, query: &QuerySpec, limit: usize) -> Vec<Item> {
        let word = tilo_search::fold(query.text.trim());
        self.0
            .iter()
            .filter(|(_, _, text)| tilo_search::fold(text).contains(&word))
            .take(limit)
            .map(|(id, book, _)| {
                Item::new(PASSAGE, *id)
                    .with("title", "Chapter 3")
                    .with("where", *book)
                    .with("book", *book)
            })
            .collect()
    }
}

#[test]
fn an_index_of_the_apps_own_answers_with_the_local_items_at_once() {
    let mut engine = QueryEngine::new(book_kinds());
    engine.sync_local(shelf());
    engine.set_local_source(Some(Arc::new(Elsewhere(vec![(
        "typee#3",
        "Typee",
        "the wharves of Nukuheva",
    )]))));
    engine.set_query(QuerySpec::text("wharves"));
    let passages: Vec<String> = engine
        .results()
        .local
        .iter()
        .filter(|group| group.kind == PASSAGE)
        .flat_map(|group| group.items.iter().map(|item| item.id.clone()))
        .collect();
    assert!(
        passages.contains(&"typee#3".to_owned()),
        "the index's passage is among the local results: {passages:?}"
    );
    assert!(engine.timings().local.is_some(), "with the local answer");
    // Its hits narrow like the items do.
    engine.set_query(QuerySpec::text("wharves").toggled(&Chip::value("book", "Moby-Dick")));
    assert!(
        engine
            .results()
            .local
            .iter()
            .flat_map(|group| &group.items)
            .all(|item| item.value("book") == Some("Moby-Dick")),
        "a chip narrows the index's hits too"
    );
}

#[test]
fn an_index_arriving_later_adds_its_hits_after_the_rows_shown() {
    let mut engine = QueryEngine::new(book_kinds());
    engine.sync_local(shelf());
    engine.set_query(QuerySpec::text("moby"));
    let before: Vec<String> = engine
        .results()
        .local
        .iter()
        .flat_map(|group| group.items.iter().map(|item| item.id.clone()))
        .collect();
    assert!(!before.is_empty());
    engine.set_local_source(Some(Arc::new(Elsewhere(vec![(
        "omoo#1",
        "Omoo",
        "a moby of a whale",
    )]))));
    let after: Vec<String> = engine
        .results()
        .local
        .iter()
        .flat_map(|group| group.items.iter().map(|item| item.id.clone()))
        .collect();
    assert_eq!(
        after.get(..before.len()),
        Some(before.as_slice()),
        "rows shown stay"
    );
    assert!(after.contains(&"omoo#1".to_owned()));
}
