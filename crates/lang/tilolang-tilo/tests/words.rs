//! A package's words are read from its catalogue in the reader's language: its nouns by count,
//! its actions' and views' labels, and what search matches — so "notebooks" finds Notes.

use std::path::Path;
use std::sync::Arc;

use tilolang_runtime::{Actor, Words};
use tilolang_tilo::action::DynAction;
use tilolang_tilo::host::{Host, Package};
use tilolang_tilo::publish::{
    ActionTypes, PlanRequest, Planner, publish_package, publish_resource,
};

const NOTES: &str = "so.tilo.notes";

/// What the toolkit reads: a package's catalogues join the person's, so what it declares reads
/// in their language.
struct Toolkit;

impl Words for Toolkit {
    fn add(&self, catalogues: &[(String, String)]) {
        tilo_app_window::i18n::add_catalogues(catalogues);
    }

    fn add_used(&self, catalogues: &[(String, String)]) {
        tilo_app_window::i18n::add_shared_catalogues(catalogues);
    }

    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        tilo_app_window::i18n::word(key, args, source)
    }

    fn revision(&self) -> u64 {
        tilo_app_window::i18n::revision()
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
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
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
    publish_resource(
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
    assert!(
        package
            .meta
            .actions
            .iter()
            .any(|action| action.name == name),
        "the notes app declares it"
    );
    spec.actions
        .iter()
        .find(|action| action.id == name)
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
    // The app calls a note a notebook: a noun its own, which the catalogue says, not the derived
    // one ("note", or "notes" as a plural name would read).
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

/// Every catalogue a package ships is carried by its module and loads into the toolkit, which is
/// all `Host::set_words` does with them. What a start pays for that is measured and printed here,
/// not asserted: it is machine and load dependent, as `budgets.rs` says.
#[test]
fn a_packages_catalogues_all_load() {
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
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    let mut catalogues = Vec::new();
    catalogue_directories(&apps, &mut catalogues);
    let mut read = 0;
    for locales in catalogues {
        for file in std::fs::read_dir(&locales).expect("the catalogues read") {
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
    assert!(read >= 100, "an app ships a catalogue ({read} read)");
}

/// The `locales` directories under `dir`, wherever an app keeps them.
fn catalogue_directories(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("the apps read") {
        let path = entry.expect("an app directory").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "locales") {
                found.push(path);
            } else {
                catalogue_directories(&path, found);
            }
        }
    }
}

/// The words a drawn window shows, in the order the tree holds them: every text of a node and of
/// its children, whatever prop carries it.
fn drawn(node: &tilolang_runtime::UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            tilolang_runtime::Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A German desktop's counter window draws German: its catalogue reaches what the window shows,
/// not only the labels it publishes, so the window's title, its section's name and its two
/// buttons read in the person's language and nothing of them in English.
#[test]
fn a_german_counter_window_draws_german() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/counter");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the counter package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the counter package's ids"),
    )
    .expect("its ids parse");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &[]).expect("the counter package compiles");
    ui::i18n::I18nManager::install("en-US", "de-DE", &[("en-US", "")]).expect("the German words");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.set_words(Toolkit);
    host.load_compiled(&compiled).expect("the counter loads");
    let view = host
        .open_main("so.tilo.lang.counter")
        .expect("its window opens");
    let words = drawn(view.tree());
    for word in ["Zähler", "Anzahl", "Erhöhen", "Verringern"] {
        assert!(
            words.iter().any(|said| said == word),
            "the window says {word}: {words:?}"
        );
    }
    for english in ["Counter", "Count", "Increment", "Decrement"] {
        assert!(
            !words.iter().any(|said| said == english),
            "and nothing of it in English ({english}): {words:?}"
        );
    }
}

#[test]
fn a_packages_words_are_published_without_a_document() {
    let (host, package) = notes("de-DE");
    let planner: Arc<dyn Planner> = Arc::new(Unasked);
    let words = host.runtime().words();
    let published = publish_package(&package.meta, &planner, &*words);
    assert_eq!(
        published.label, "Notizen",
        "the package is called by the app's name in the reader's words"
    );
    assert!(
        published
            .actions
            .iter()
            .all(|action| !action.label.is_empty()),
        "and every action it offers without a document says what it does"
    );
}

/// A notebook is called by the app's name in the reader's words, whether it holds notes or none:
/// a note's title names the note, never the notebook.
#[test]
fn a_root_reads_as_the_apps_name_not_what_it_holds() {
    let (host, package) = notes("de-DE");
    let notebook = host.root_of(NOTES).expect("the notebook");
    assert_eq!(host.label_at(&notebook), "Notizen", "an empty notebook");
    let scenario = package
        .meta
        .scenarios
        .first()
        .expect("Notes declares a scenario");
    host.runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.action.as_str(), vec![]),
        )
        .expect("the scenario runs");
    assert_eq!(
        host.label_at(&notebook),
        "Notizen",
        "a notebook whose first note says Q3 plan"
    );
}

/// A German desktop's palette lists a native app's actions and a tilolang app's in German: each
/// declares its label as a key of its own catalogue, read in the person's language when it is
/// published.
#[test]
fn the_palette_reads_native_and_tilolang_actions_in_german() {
    use tilo_desktop_search::model::Source as _;
    use tilo_desktop_search::providers::{Published, PublishedActions};
    use tilo_resource::effect::{Effect as _, EffectDecl};

    let (host, package) = notes("de-DE");
    let notebook = notebook(&host, &package);
    let contacts = contacts::actions::ContactsEffect::declarations()
        .iter()
        .map(EffectDecl::spec_of::<contacts::actions::ContactsEffect>)
        .fold(
            tilo_actions::ResourceSpec::new("contacts", "contacts.effect", "Kontakte"),
            tilo_actions::ResourceSpec::with_action,
        );
    let palette = Published(vec![
        PublishedActions::of("so.tilo.contacts", "workspace", &[contacts]),
        PublishedActions::of(NOTES, "workspace", &[notebook]),
    ]);
    let rows = |query: &str| -> Vec<String> {
        palette
            .query(query)
            .into_iter()
            .map(|hit| hit.title)
            .collect()
    };
    let native = rows("Gruppe");
    assert!(
        native.iter().any(|title| title == "Gruppe öffnen"),
        "the contacts app's action reads in German: {native:?}"
    );
    let notes = rows("Notiz");
    assert!(
        notes.iter().any(|title| title == "Neue Notiz"),
        "the notes app's action reads in German: {notes:?}"
    );
    assert!(
        rows("open group").is_empty() && rows("new note").is_empty(),
        "and neither reads in English"
    );
}

/// A German reader asked to let an agent file a note reads what it fills in in German: the
/// consent sheet names each argument by the package's catalogue (`d-<action>-<param>`).
#[test]
fn the_consent_sheet_names_file_notes_arguments_in_german() {
    let (host, package) = notes("de-DE");
    let notebook = notebook(&host, &package);
    let id = "file_note";
    let descriptor = tilo_actions::api::survey(std::slice::from_ref(&notebook))
        .into_iter()
        .find(|descriptor| descriptor.address.action == id)
        .expect("filing a note is offered on the notebook");
    let arguments = serde_json::json!({"title": "Einkauf", "tag": "Haushalt"});
    let sheet = tilo_actions::Draft::render(
        &descriptor,
        arguments.as_object().expect("arguments are an object"),
    );
    let labels: Vec<&str> = sheet
        .fields
        .iter()
        .map(|field| field.label.as_str())
        .collect();
    assert_eq!(labels, ["Titel", "Schlagwort"], "{sheet:?}");
}
