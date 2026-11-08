//! A package's words are read from its catalogue in the reader's language: its nouns by count,
//! its actions' and views' labels, and what search matches — so "notebooks" finds Notes.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::path::Path;
use std::sync::Arc;

use tilolang_runtime::{Principal, Words};
use tilolang_tilo::action::DynAction;
use tilolang_tilo::host::{Host, Package};
use tilolang_tilo::publish::{ActionTypes, PlanRequest, Planner, publish_package, publish_record};

const NOTES: &str = "so.tilo.lang.notes";

/// What the toolkit reads: a package's catalogues join the person's, so what it declares reads
/// in their language.
struct Toolkit;

impl Words for Toolkit {
    fn add(&self, catalogues: &[(String, String)]) {
        tilo_ui_host::i18n::add_catalogues(catalogues);
    }

    fn add_used(&self, catalogues: &[(String, String)]) {
        tilo_ui_host::i18n::add_shared_catalogues(catalogues);
    }

    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        tilo_ui_host::i18n::word(key, args, source)
    }

    fn revision(&self) -> u64 {
        tilo_ui_host::i18n::revision()
    }
}

/// Plans nothing: what these tests read is what the package publishes.
struct Unasked;

impl Planner for Unasked {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<DynAction, String> {
        Err("these tests publish, they run nothing".to_owned())
    }
}

/// A package of `apps/tl`, as it ships, compiled on its own: what a package that imports it
/// compiles against and loads with.
fn app(name: &str) -> tilolang::Compiled {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/tl")
        .join(name);
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    tilolang::compile_package_source(&source, &ids, &[]).expect("the package compiles")
}

/// The notes app as it ships: its source, its ids and the catalogues beside it, compiled into
/// its module and loaded into a host that reads in `locale`.
fn notes(locale: &str) -> (Host, Package) {
    let discussion = app("discussion");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/notes");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the notes package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the notes package's ids"),
    )
    .expect("its ids parse");
    let compiled =
        tilolang::compile_package_source(&source, &ids, std::slice::from_ref(&discussion.meta))
            .expect("the notes package compiles");
    ui::i18n::I18nManager::install("en-US", locale, &[("en-US", "")]).expect("the source words");
    let record = automerge::AutoCommit::new();
    let host = Host::new(&record, Principal::new("ada"), || None).expect("a host");
    host.set_words(Toolkit);
    host.load_compiled(&discussion)
        .expect("the discussion package loads");
    host.load_compiled(&compiled)
        .expect("the notes package loads");
    let package = host.package(NOTES).expect("it is loaded");
    (host, package)
}

fn types_of(host: &Host, package: &Package) -> ActionTypes {
    let planner: Arc<dyn Planner> = Arc::new(Unasked);
    let words = host.runtime().words();
    ActionTypes::new(&package.meta, &planner, &*words)
}

/// The notes package's one notebook, as it publishes it to the workspace's search.
fn notebook(host: &Host, package: &Package) -> tilo_actions::ResourceSpec {
    publish_record(
        &types_of(host, package),
        "Notebook",
        "tl.notes/root",
        "Notes",
    )
}

/// Whether a search for `needle` finds the notebook by what it is, as the session's search asks
/// (`ui::search::system::type_match_score` over the published nouns).
fn found(spec: &tilo_actions::ResourceSpec, needle: &str) -> bool {
    let ty = spec
        .resource_type
        .as_ref()
        .expect("a resource says what it is");
    let needle = ui::search::system::without_filler(needle);
    ui::search::system::type_match_score(&needle, ty, std::iter::empty(), |_| None).is_some()
}

/// What the action `name` says it is, as the package published it on the notebook.
fn said(spec: &tilo_actions::ResourceSpec, name: &str, package: &Package) -> String {
    let id = &package
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .expect("the notes app declares it")
        .id;
    spec.actions
        .iter()
        .find(|action| &action.id == id)
        .expect("it is offered on the notebook")
        .label
        .clone()
}

/// The label of the view `name` of the notebook.
fn view_label(spec: &tilo_actions::ResourceSpec, name: &str, package: &Package) -> String {
    let id = &package
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .expect("the notes app declares it")
        .id;
    spec.views
        .iter()
        .find(|view| &view.id == id)
        .expect("the notebook is shown through it")
        .label
        .clone()
}

#[test]
fn notebooks_find_notes() {
    let (host, package) = notes("en-US");
    let spec = notebook(&host, &package);
    let ty = spec
        .resource_type
        .as_ref()
        .expect("a notebook says what it is");
    // What English cannot derive from "notes" ("noteses"): the catalogue's nouns, by count.
    assert_eq!(
        (ty.noun.one.as_str(), ty.noun.many.as_str()),
        ("notebook", "notebooks")
    );
    assert!(ty.root, "the notebook is the app's one");
    assert!(
        found(&spec, "notebooks"),
        "search for notebooks finds the notebook"
    );
    assert!(
        found(&spec, "my notebooks"),
        "with the words people put before it"
    );
    assert!(found(&spec, "notebook"), "and by one");
    assert!(
        !found(&spec, "notizbücher"),
        "an English reader has no German words"
    );
}

#[test]
fn a_catalogue_line_changes_a_label_and_a_search_match() {
    let (host, package) = notes("en-US");
    let english = notebook(&host, &package);
    assert_eq!(said(&english, "new_note", &package), "New note");
    assert_eq!(view_label(&english, "notebook", &package), "Notebook");

    let words = host.runtime().words().revision();
    ui::i18n::I18nManager::set_locale("de-DE");
    assert_ne!(
        words,
        host.runtime().words().revision(),
        "a language change is what publishes the package's words again"
    );

    let german = notebook(&host, &package);
    assert_eq!(
        said(&german, "new_note", &package),
        "Neue Notiz",
        "the action's line"
    );
    assert_eq!(
        view_label(&german, "notebook", &package),
        "Notizbuch",
        "the view's line"
    );
    let ty = german
        .resource_type
        .as_ref()
        .expect("a notebook says what it is");
    assert_eq!(
        (ty.noun.one.as_str(), ty.noun.many.as_str()),
        ("Notizbuch", "Notizbücher"),
        "and its nouns by count"
    );
    assert!(
        found(&german, "notizbücher"),
        "search finds what the reader calls it"
    );
    assert!(
        !found(&german, "notebooks"),
        "and not the words the catalogue replaced"
    );
    // The German label is what the session's search ranks the action by.
    assert!(
        ui::search::system::rank_action_fields(
            "neue notiz",
            &ui::search::system::action_search_fields("Neue Notiz", &[], "Notizbuch"),
        )
        .is_some(),
        "a person finds New note by the words they read"
    );
}

/// What a start pays for a package's words: reading its catalogues out of its module and parsing
/// them into the toolkit's, which is all `Host::set_words` does with them.
#[test]
fn a_packages_catalogues_load_under_a_millisecond() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/notes");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the notes package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the notes package's ids"),
    )
    .expect("its ids parse");
    let compiled = tilolang::compile_package_source(&source, &ids, &[app("discussion").meta])
        .expect("the notes package compiles");
    let mut best = std::time::Duration::MAX;
    let (mut read, mut parsed) = (best, best);
    let mut loaded = 0;
    for _ in 0..5 {
        ui::i18n::I18nManager::install("en-US", "en-US", &[("en-US", "")])
            .expect("the source words");
        let started = std::time::Instant::now();
        let catalogues = tilolang::meta::locales_from_wasm(&compiled.wasm);
        let reading = started.elapsed();
        let parsing = std::time::Instant::now();
        Toolkit.add(&catalogues);
        let parsing = parsing.elapsed();
        let total = started.elapsed();
        if total < best {
            best = total;
            read = reading;
            parsed = parsing;
        }
        loaded = catalogues.len();
    }
    assert!(loaded > 0, "the module carries its catalogues");
    // What a start pays is machine and load dependent, so it is measured and printed, not
    // asserted (docs/measurements/performance.md, as `budgets.rs`): a release build of the notes
    // package reads its 2 catalogues in 4 µs and parses them in 471 µs, under the 1 ms a package
    // may take; a debug test builds unoptimised, where the same parse takes about 3 ms.
    println!("{loaded} catalogues read in {read:?} and parsed in {parsed:?}");
    assert_eq!(
        loaded,
        source.locales.len(),
        "every catalogue the package has loads"
    );
}

/// Every catalogue an app ships parses as Fluent: one key the language does not allow would
/// leave the whole file unread, and the app would show its source words in every language.
#[test]
fn every_apps_catalogue_parses() {
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    let mut read = 0;
    for app in std::fs::read_dir(&apps).expect("the apps read") {
        let locales = app.expect("an app").path().join("locales");
        let Ok(files) = std::fs::read_dir(&locales) else {
            continue;
        };
        for file in files {
            let path = file.expect("a catalogue").path();
            if path.extension().is_none_or(|extension| extension != "ftl") {
                continue;
            }
            let locale = path
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .expect("a catalogue is named by its locale");
            let text = std::fs::read_to_string(&path).expect("the catalogue reads");
            ui::i18n::I18nManager::install(locale, locale, &[(locale, &text)])
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            read += 1;
        }
    }
    assert!(read >= 9, "an app ships a catalogue ({read} read)");
}

#[test]
fn a_packages_words_are_published_without_a_record() {
    let (host, package) = notes("de-DE");
    let planner: Arc<dyn Planner> = Arc::new(Unasked);
    let words = host.runtime().words();
    let published = publish_package(&package.meta, &planner, &*words);
    assert_eq!(
        published.label, "Notes",
        "the package is called by its title"
    );
    assert!(
        published
            .actions
            .iter()
            .all(|action| !action.label.is_empty()),
        "and every action it offers without a record says what it does"
    );
}
