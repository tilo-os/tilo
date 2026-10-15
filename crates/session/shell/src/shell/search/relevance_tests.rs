//! How good the palette's answers are, measured against the real providers
//! and the real apps.
//!
//! The fixture is what a device has: every app under `apps/` read from its
//! manifest, the shell's actions with the radios present, the typed system
//! settings, the settings pages and the session actions, all behind the
//! registry's own ranking and merge.
//! [`the_palette_finds_what_people_type`] is a table of what a person types
//! and the row that must lead; [`a_query_against_the_full_index_is_fast`]
//! bounds what one keystroke costs.

use std::fmt::Write as _;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use ui::search::system::SearchScope;

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::launcher::InstalledApp;
use crate::shell::quick_settings::actions::{QsActionAvailability, UnavailableQsActions};
use crate::shell::search::providers::actions::SharedQsDispatcher;
use crate::shell::search::providers::applications::ApplicationsTestSource;
use crate::shell::search::providers::{
    ActionsProvider, ApplicationsProvider, SessionActionPolicy, SessionActionsProvider,
    SettingsPagesProvider, SystemSettingsProvider,
};
use crate::shell::search::{QueryArgs, SearchRegistry, SearchResult};

/// Every language's words of `search/*.ftl`, as the packer joins them.
fn tilo_app_package_keywords(dir: &Path) -> Vec<String> {
    let mut words = Vec::new();
    let Ok(files) = std::fs::read_dir(dir.join("search")) else {
        return words;
    };
    for file in files.filter_map(Result::ok) {
        let text = std::fs::read_to_string(file.path()).expect("a keywords file");
        for line in text.lines() {
            if let Some(list) = line.strip_prefix("search-keywords = ") {
                words.extend(list.split(',').map(|word| word.trim().to_owned()));
            }
        }
    }
    words
}

/// The apps a device has: every `apps/*/tilo-app.toml` that is something a
/// person opens, read the way the launcher reads a manifest.
fn installed_apps() -> Vec<InstalledApp> {
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    let mut dirs: Vec<_> = std::fs::read_dir(&apps)
        .expect("the apps directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    let mut installed = Vec::new();
    for dir in dirs {
        let Ok(text) = std::fs::read_to_string(dir.join("tilo-app.toml")) else {
            continue;
        };
        let manifest: toml::Table = text.parse().expect("a manifest is TOML");
        let package = manifest["package"].as_table().expect("[package]");
        // An entrypoint without a type is a GUI one, as the manifest reads it.
        let gui = manifest
            .get("entrypoint")
            .and_then(|entry| entry.get("type"))
            .and_then(toml::Value::as_str)
            .is_none_or(|kind| kind == "gui");
        if package.get("kind").and_then(toml::Value::as_str) != Some("app") || !gui {
            continue;
        }
        // The words come from search/*.ftl, as the packer reads them.
        let keywords: Vec<String> = tilo_app_package_keywords(&dir);
        let id = package["id"].as_str().expect("package id").to_owned();
        installed.push(InstalledApp {
            display_name: package
                .get("name")
                .and_then(toml::Value::as_str)
                .map_or_else(|| id.clone(), str::to_owned),
            localized_names: package
                .get("names")
                .and_then(toml::Value::as_table)
                .into_iter()
                .flatten()
                .filter_map(|(tag, name)| Some((tag.clone(), name.as_str()?.to_owned())))
                .collect(),
            keywords,
            category: package
                .get("category")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            package_id: id,
            version: "0.1.0".into(),
            manifest_path: dir.join("tilo-app.toml"),
            payload_root: dir,
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        });
    }
    installed
}

struct Palette {
    registry: SearchRegistry,
    chrome: Chrome,
    document: AutoCommit,
}

impl Palette {
    fn new() -> Self {
        let mut registry = SearchRegistry::new();
        let apps =
            ApplicationsTestSource::new(installed_apps(), Vec::new(), Vec::new(), Vec::new());
        registry.register(Rc::new(ApplicationsProvider::with_test_source(apps)));
        // A device with every radio, so the switches are offered.
        let qs = Rc::new(SharedQsDispatcher::new(
            Rc::new(UnavailableQsActions),
            QsActionAvailability::all(),
        ));
        registry.register(Rc::new(ActionsProvider::with_shared_qs_actions(qs)));
        registry.register(Rc::new(SystemSettingsProvider::new()));
        registry.register(Rc::new(SettingsPagesProvider::new()));
        let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new());
        policy.set_user_switching(true);
        registry.register(Rc::new(SessionActionsProvider::new(policy)));
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let palette = Self {
            registry,
            chrome,
            document: AutoCommit::new(),
        };
        // The apps index builds off the keystroke; wait for it as a person
        // would, by it being there when they type.
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("a deadline in range");
        while palette.with_args("", |args| {
            palette.registry.refresh_indexes(args);
            palette.registry.indexes_settling(args)
        }) {
            assert!(
                Instant::now() < deadline,
                "the application index never settled"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        palette
    }

    fn with_args<T>(&self, needle: &str, run: impl FnOnce(&QueryArgs<'_>) -> T) -> T {
        let state = self.chrome.borrow();
        run(&QueryArgs {
            needle,
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &state,
            doc: &self.document,
            doc_root: &automerge::ROOT,
        })
    }

    /// What the palette does on a key: the grouped, budgeted answer.
    fn keystroke(&self, needle: &str) -> usize {
        self.with_args(needle, |args| {
            let _ = self.registry.query_revision(args);
            self.registry
                .query_grouped(args, ui::search::system::PALETTE_ROW_BUDGET)
                .iter()
                .map(|group| group.results.len())
                .sum()
        })
    }

    fn ask(&self, needle: &str) -> Vec<SearchResult> {
        self.with_args(needle, |args| self.registry.query(args))
    }
}

/// Which row must lead.
enum Expect {
    /// This row, first.
    Top(&'static str, &'static str),
    /// One of these rows, first: the query names several things equally well.
    OneOf(&'static [(&'static str, &'static str)]),
    /// Anything but this row: the query names something the app is not.
    Not(&'static str, &'static str),
}

/// CPU time this thread has used. A keystroke's cost is the work it does, not
/// the wall time it waited for a core while the machine was building.
fn thread_cpu() -> Duration {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid timespec for the call to fill in.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    assert_eq!(status, 0, "the thread CPU clock is readable");
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or(0),
        u32::try_from(now.tv_nsec).unwrap_or(0),
    )
}

/// How much slower the unoptimised test profile runs this code than the
/// optimised build the compositor ships. The budgets below are the optimised
/// ones; a build with debug assertions is held to them times this factor, so
/// the gate runs, and bites, in the profile the suite is built in. Set from
/// measurement, with twice the observed headroom: the slowest key (a long
/// German word) takes 4.8 times its optimised budget here.
const UNOPTIMISED_SLOWDOWN: u32 = 10;

fn budget(optimised: Duration) -> Duration {
    if cfg!(debug_assertions) {
        optimised.saturating_mul(UNOPTIMISED_SLOWDOWN)
    } else {
        optimised
    }
}

const APP: &str = "applications";
const ACTION: &str = "actions";
const PAGE: &str = "settings-pages";
const SESSION: &str = "session";

const fn app(id: &'static str) -> Expect {
    Expect::Top(APP, id)
}
const fn action(id: &'static str) -> Expect {
    Expect::Top(ACTION, id)
}
const fn page(id: &'static str) -> Expect {
    Expect::Top(PAGE, id)
}
const fn not_app(id: &'static str) -> Expect {
    Expect::Not(APP, id)
}
const fn session(id: &'static str) -> Expect {
    Expect::Top(SESSION, id)
}

/// What a person types, and what must lead. Names, the words people use
/// instead, the other language, the products an app stands in for, typos and
/// initials; then actions by intent, switches, settings pages and the
/// session.
fn table() -> Vec<(&'static str, Expect)> {
    let mut rows = app_names();
    rows.extend(typos_actions_pages_and_session());
    rows
}

/// Apps by name, by the words people use instead, in the other language, and
/// by names they are not.
fn app_names() -> Vec<(&'static str, Expect)> {
    vec![
        // The names themselves.
        ("tube", app("so.tilo.tube")),
        ("mail", app("so.tilo.mail")),
        ("calendar", app("so.tilo.calendar")),
        ("terminal", app("so.tilo.terminal")),
        ("git", app("so.tilo.git-client")),
        ("pdf", app("so.tilo.pdf-viewer")),
        ("word", app("so.tilo.word-processor")),
        ("files", app("so.tilo.file-explorer")),
        ("calc", app("so.tilo.calculator")),
        ("vpn", app("so.tilo.vpn")),
        ("theme", app("so.tilo.themes")),
        ("shortcuts", app("so.tilo.shortcuts")),
        // The product or the plain word an app stands in for.
        ("youtube", app("so.tilo.tube")),
        ("Youtube", app("so.tilo.tube")),
        ("YOUTUBE", app("so.tilo.tube")),
        ("email", app("so.tilo.mail")),
        ("e-mail", app("so.tilo.mail")),
        ("gmail", app("so.tilo.mail")),
        ("inbox", app("so.tilo.mail")),
        ("internet", app("so.tilo.browser")),
        ("excel", app("so.tilo.sheets")),
        ("powerpoint", app("so.tilo.slides")),
        ("webcam", app("so.tilo.camera")),
        ("docker", app("so.tilo.containers")),
        ("memo", app("so.tilo.notes")),
        ("task manager", app("so.tilo.monitor")),
        ("backup", app("so.tilo.backups")),
        ("stopwatch", app("so.tilo.clocks")),
        ("forecast", app("so.tilo.weather")),
        ("openstreetmap", app("so.tilo.maps")),
        // Where apps come from: generic words must not be swallowed by
        // category matching of every installed app.
        ("apps", app("so.tilo.store")),
        ("app store", app("so.tilo.store")),
        ("store", app("so.tilo.store")),
        ("install", app("so.tilo.store")),
        ("programs", app("so.tilo.store")),
        ("programme", app("so.tilo.store")),
        ("app-store", app("so.tilo.store")),
        ("programme installieren", app("so.tilo.store")),
        ("apps installieren", app("so.tilo.store")),
        // The other language: names and words.
        ("einstellungen", app("so.tilo.settings")),
        ("posteingang", app("so.tilo.mail")),
        ("kalender", app("so.tilo.calendar")),
        ("rechner", app("so.tilo.calculator")),
        ("taschenrechner", app("so.tilo.calculator")),
        ("dateien", app("so.tilo.file-explorer")),
        ("notizen", app("so.tilo.notes")),
        ("wetter", app("so.tilo.weather")),
        ("musik", app("so.tilo.music")),
        ("fotos", app("so.tilo.photos")),
        ("konsole", app("so.tilo.terminal")),
        ("tabelle", app("so.tilo.sheets")),
        ("textverarbeitung", app("so.tilo.word-processor")),
        ("passwort", app("so.tilo.passwords")),
        ("karten", app("so.tilo.maps")),
        ("sicherung", app("so.tilo.backups")),
        ("wecker", app("so.tilo.clocks")),
        ("praesentation", app("so.tilo.slides")),
        // Names an app is not: another service, another product. Nothing
        // that merely sits near an app's category may find it.
        ("netflix", not_app("so.tilo.tube")),
        ("twitch", not_app("so.tilo.tube")),
        ("tiktok", not_app("so.tilo.tube")),
        ("vimeo", not_app("so.tilo.tube")),
        ("slack", not_app("so.tilo.chat")),
        ("discord", not_app("so.tilo.chat")),
        ("whatsapp", not_app("so.tilo.chat")),
        ("firefox", not_app("so.tilo.browser")),
        ("chrome", not_app("so.tilo.browser")),
        ("photoshop", not_app("so.tilo.canvas")),
        ("spotify", not_app("so.tilo.music")),
        ("notion", not_app("so.tilo.notes")),
        ("vscode", not_app("so.tilo.code-editor")),
        ("github", not_app("so.tilo.git-client")),
        ("outlook", not_app("so.tilo.mail")),
        ("zoom", not_app("so.tilo.remote")),
        ("nordvpn", not_app("so.tilo.vpn")),
    ]
}

/// Typos and initials, then actions by intent, switches, settings pages and
/// the session.
fn typos_actions_pages_and_session() -> Vec<(&'static str, Expect)> {
    vec![
        // Typos.
        ("setings", app("so.tilo.settings")),
        ("emial", app("so.tilo.mail")),
        ("yotube", app("so.tilo.tube")),
        ("calender", app("so.tilo.calendar")),
        ("terminl", app("so.tilo.terminal")),
        ("kalendar", app("so.tilo.calendar")),
        // Initials.
        ("wp", app("so.tilo.word-processor")),
        ("pv", app("so.tilo.pdf-viewer")),
        // A word several apps are known by: any of them, the best first.
        (
            "video",
            Expect::OneOf(&[
                (APP, "so.tilo.video-player"),
                (APP, "so.tilo.video-editor"),
                (APP, "so.tilo.tube"),
            ]),
        ),
        // Actions by intent.
        ("screenshot", action("capture.open")),
        ("bildschirmfoto", action("capture.open")),
        ("dark mode", action("appearance.theme.dark")),
        ("dunkelmodus", action("appearance.theme.dark")),
        ("light mode", action("appearance.theme.light")),
        ("do not disturb", action("notifications.dnd.toggle")),
        ("nicht stören", action("notifications.dnd.toggle")),
        ("dnd", action("notifications.dnd.toggle")),
        ("volume", action("quick-settings.volume.set")),
        ("lautstärke", action("quick-settings.volume.set")),
        ("brightness", action("quick-settings.brightness.set")),
        ("helligkeit", action("quick-settings.brightness.set")),
        ("dictate", action("dictation.toggle")),
        ("reopen closed tab", action("tab.reopen")),
        ("overview", action("overview.toggle")),
        // The switches.
        ("wifi", action("quick-settings.wifi.toggle")),
        ("wi-fi", action("quick-settings.wifi.toggle")),
        ("wlan", action("quick-settings.wifi.toggle")),
        ("wireless", action("quick-settings.wifi.toggle")),
        (
            "airplane mode",
            action("quick-settings.airplane-mode.toggle"),
        ),
        ("flugmodus", action("quick-settings.airplane-mode.toggle")),
        // Settings pages: by name and by what is on them.
        ("bluetooth", page("page:bluetooth")),
        ("keyboard layout", page("page:keyboard")),
        ("tastatur", page("page:keyboard")),
        ("touchpad", page("page:pointing")),
        ("night light", page("page:displays")),
        ("time zone", page("page:time_date")),
        ("zeitzone", page("page:time_date")),
        ("default browser", page("page:default-apps")),
        ("firmware", page("page:updates")),
        ("battery", page("page:power")),
        // The session.
        ("lock", session("lock")),
        ("sperren", session("lock")),
        ("restart", session("restart")),
        ("reboot", session("restart")),
        ("neustart", session("restart")),
        ("shutdown", session("shut-down")),
        ("herunterfahren", session("shut-down")),
        ("log out", session("logout")),
        ("abmelden", session("logout")),
        ("sleep", session("suspend")),
    ]
}

#[test]
fn the_palette_finds_what_people_type() {
    let palette = Palette::new();
    let mut failures = Vec::new();
    let mut report = String::new();
    for (query, expect) in table() {
        let rows = palette.ask(query);
        let top: Vec<String> = rows
            .iter()
            .take(3)
            .map(|row| format!("{}/{} ({})", row.provider_id, row.id, row.score))
            .collect();
        let passed = rows.first().is_some_and(|first| match &expect {
            Expect::Top(provider, id) => first.provider_id == *provider && first.id == *id,
            Expect::OneOf(accepted) => accepted
                .iter()
                .any(|(provider, id)| first.provider_id == *provider && first.id == *id),
            Expect::Not(provider, id) => !(first.provider_id == *provider && first.id == *id),
        }) || (matches!(expect, Expect::Not(..)) && rows.is_empty());
        let wanted = match &expect {
            Expect::Top(provider, id) => format!("{provider}/{id}"),
            Expect::OneOf(accepted) => accepted
                .iter()
                .map(|(provider, id)| format!("{provider}/{id}"))
                .collect::<Vec<_>>()
                .join(" | "),
            Expect::Not(provider, id) => format!("not {provider}/{id}"),
        };
        let _ = writeln!(
            report,
            "{} {query:<20} -> {wanted:<46} got {}",
            if passed { "ok  " } else { "FAIL" },
            top.join(", ")
        );
        if !passed {
            failures.push(query);
        }
    }
    println!("{report}");
    assert!(
        failures.is_empty(),
        "{} of {} queries did not lead with the expected row:\n{report}",
        failures.len(),
        table().len()
    );
}

#[test]
fn an_exact_keyword_leads_the_notes_and_files_that_merely_contain_it() {
    // Provider-independent merge: the same word as a note's exact title must
    // not push the app it names below the note.
    let palette = Palette::new();
    let rows = palette.ask("youtube");
    assert_eq!(
        rows.first().map(|row| row.id.as_str()),
        Some("so.tilo.tube")
    );
}

/// The palette refreshes on the compositor thread for every key, and the
/// providers are single-threaded, so the answer is incremental instead: the
/// apps' index and every list's index are retained and only searched, and
/// the pages' copy is built once per language. This holds a keystroke at
/// full index size, revision check, search, merge and grouping included, to
/// well under the 1 ms the compositor can spare.
#[test]
fn a_keystroke_against_the_full_index_costs_under_a_millisecond() {
    let palette = Palette::new();
    let words = [
        "youtube",
        "email",
        "einstellungen",
        "wifi",
        "dark mode",
        "setings",
        "wp",
        "tastatur",
        "screenshot",
        "lock",
    ];
    // Typing is every prefix of the word, one key at a time.
    let mut keystrokes: Vec<String> = Vec::new();
    for word in words {
        let letters: Vec<char> = word.chars().collect();
        for end in 1..=letters.len() {
            keystrokes.push(letters[..end].iter().collect());
        }
    }
    // The first query warms the retained indexes, as opening the palette does.
    for key in &keystrokes {
        let _ = palette.keystroke(key);
    }
    // What a key costs is the least it took of a few tries: the rest is the
    // machine's other work (a build, a neighbour thread) landing in the
    // timing, which says nothing about the code.
    let mut samples: Vec<Duration> = keystrokes
        .iter()
        .map(|key| {
            (0..5)
                .map(|_| {
                    let started = thread_cpu();
                    let _ = palette.keystroke(key);
                    thread_cpu().saturating_sub(started)
                })
                .min()
                .unwrap_or_default()
        })
        .collect();
    samples.sort();
    let median = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    println!(
        "{} keystrokes: median {median:?}, p95 {p95:?}, max {:?}",
        samples.len(),
        samples[samples.len() - 1]
    );
    assert!(
        median < budget(Duration::from_millis(1)),
        "a keystroke took {median:?} at the median; the budget is {:?}",
        budget(Duration::from_millis(1))
    );
    assert!(
        p95 < budget(Duration::from_millis(2)),
        "a slow keystroke took {p95:?} (p95); the budget is {:?}",
        budget(Duration::from_millis(2))
    );
}

#[test]
fn a_query_against_the_full_index_is_fast() {
    let palette = Palette::new();
    let queries = [
        "youtube",
        "email",
        "einstellungen",
        "wifi",
        "dark mode",
        "setings",
        "wp",
        "tastatur",
        "screenshot",
        "lock",
    ];
    // The first query of each warms the retained indexes, as the first
    // keystroke after the palette opens does.
    for query in queries {
        let _ = palette.ask(query);
    }
    let mut worst = Duration::ZERO;
    for query in queries {
        let best = (0..25)
            .map(|_| {
                let started = thread_cpu();
                let rows = palette.ask(query);
                let took = thread_cpu().saturating_sub(started);
                assert!(!rows.is_empty(), "{query:?} found nothing");
                took
            })
            .min()
            .unwrap_or_default();
        println!("{query:<14} best of 25 {best:?}");
        worst = worst.max(best);
    }
    assert!(
        worst < budget(Duration::from_millis(2)),
        "a keystroke against the full index took {worst:?}; the budget is {:?}",
        budget(Duration::from_millis(2))
    );
}
