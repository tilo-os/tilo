//! How good the palette's answers are, measured against the real providers
//! and the real apps.
//!
//! The fixture is what a device has: every app under `apps/` read from its
//! manifest, the shell's actions with the radios present, the typed system
//! settings, the settings pages and the session actions, all behind the
//! registry's own ranking and merge.
//! [`the_palette_finds_what_people_type`] is a table of what a person types
//! and the row that must lead; [`a_keystroke_against_the_full_index_only_searches_it`]
//! bounds what one keystroke costs.

use std::fmt::Write as _;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use automerge::AutoCommit;
use ui::search::system::SearchScope;

use crate::shell::catalogue::{CatalogueApp, SharedCatalogue};
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::host_apps::HostApp;
use crate::shell::launcher::InstalledApp;
use crate::shell::quick_settings::actions::{QsActionAvailability, UnavailableQsActions};
use crate::shell::search::providers::actions::SharedQsDispatcher;
use crate::shell::search::providers::applications::ApplicationsTestSource;
use crate::shell::search::providers::host_applications::HostAppsTestSource;
use crate::shell::search::providers::store_catalogue::{
    StoreCatalogueTestSource, StoreCatalogueTestState,
};
use crate::shell::search::providers::{
    ActionsProvider, ApplicationsProvider, ContinueProvider, FilesProvider, SessionActionPolicy,
    SessionActionsProvider, SettingsPagesProvider, WebProvider,
};
use crate::shell::search::providers::{HostApplicationsProvider, StoreCatalogueProvider};
use crate::shell::search::{
    Availability, QueryArgs, ResourceKind, ResourceKindText as _, SearchRegistry, SearchResult,
};
use std::sync::Arc;

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
            opens_secondary: false,
            offers: Vec::new(),
        });
    }
    installed
}

struct Palette {
    registry: SearchRegistry,
    chrome: Chrome,
    document: AutoCommit,
    applications: Rc<ApplicationsProvider>,
    pages: Rc<SettingsPagesProvider>,
}

impl Palette {
    fn new() -> Self {
        let mut registry = SearchRegistry::new();
        let apps = ApplicationsTestSource::new(installed_apps(), Vec::new());
        let applications = Rc::new(ApplicationsProvider::with_test_source(apps));
        registry.register(applications.clone());
        // A device with every radio, so the switches are offered.
        let qs = Rc::new(SharedQsDispatcher::new(
            Rc::new(UnavailableQsActions),
            QsActionAvailability::all(),
        ));
        registry.register(Rc::new(ActionsProvider::with_shared_qs_actions(qs)));
        let pages = Rc::new(SettingsPagesProvider::new());
        registry.register(pages.clone());
        let policy = SessionActionPolicy::new(tilo_power_mgmt::InhibitRegistry::new());
        policy.set_user_switching(true);
        registry.register(Rc::new(SessionActionsProvider::new(policy)));
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let palette = Self {
            registry,
            chrome,
            document: AutoCommit::new(),
            applications,
            pages,
        };
        palette.settled()
    }

    /// The palette once every index has built: an index builds off the
    /// keystroke, and a person meets it there when they type.
    fn settled(self) -> Self {
        tilo_test_env::wait_until("the search indexes to settle", || {
            self.with_args("", |args| {
                self.registry.refresh_indexes(args);
                !self.registry.indexes_settling(args)
            })
        });
        self
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

    /// The indexes built so far: the apps' and the settings pages'.
    fn index_builds(&self) -> (usize, usize) {
        (self.applications.index_builds(), self.pages.builds())
    }

    fn ask(&self, needle: &str) -> Vec<SearchResult> {
        self.with_args(needle, |args| self.registry.query(args))
    }

    /// The rows the palette shows for `needle`: grouped and budgeted.
    fn shown(&self, needle: &str) -> Vec<SearchResult> {
        self.with_args(needle, |args| {
            self.registry
                .query_grouped(args, ui::search::system::PALETTE_ROW_BUDGET)
                .into_iter()
                .flat_map(|group| group.results)
                .collect()
        })
    }

    /// The palette on a desk whose Notes windows were lately on `notes`
    /// (title, anchor) in the workspace in front, as Continue lists them;
    /// Notes published no entry for any of them.
    fn with_places(mut self, notes: &[(&str, &str)]) -> Self {
        let room = self
            .chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("a workspace in front")
            .room()
            .to_string();
        for (at, (title, anchor)) in (1u64..).zip(notes) {
            crate::shell::search::recent_places::visit_ago(
                "so.tilo.notes",
                &room,
                0,
                title,
                Some(anchor),
                Duration::from_secs(at.saturating_mul(60)),
            );
        }
        self.registry
            .register(Rc::new(ContinueProvider::without_broker(Arc::new(
                crate::shell::search::app_content_index::AppContentIndex::in_memory(),
            ))));
        self
    }

    /// The palette with the web as a provider too, on a device whose apps
    /// are not installed through the launcher, so the web serves every
    /// service.
    fn with_web(mut self) -> Self {
        self.registry
            .register(Rc::new(WebProvider::with_installed(|_| None)));
        self
    }

    /// The palette on a device that also has these host (Flatpak) apps
    /// installed, and the catalogue offering these Tilo apps and Flathub's
    /// apps from `appstream` (a Flathub `AppStream` document).
    fn with_catalogue(
        mut self,
        host: Vec<HostApp>,
        tilo: &[(&str, &str)],
        appstream: &str,
    ) -> Self {
        let installed: Vec<String> = installed_apps()
            .into_iter()
            .map(|app| app.package_id)
            .chain(host.iter().map(|app| app.desktop_id.clone()))
            .collect();
        self.registry
            .register(Rc::new(HostApplicationsProvider::with_test_source(
                HostAppsTestSource::new(host, installed_apps()),
            )));
        let source = app_catalogue::appstream::AppstreamSource {
            remote: "flathub".into(),
            installation: app_catalogue::Installation::System,
            file: "/var/lib/flatpak/appstream/flathub/x86_64/active/appstream.xml".into(),
            icons: "/var/lib/flatpak/appstream/flathub/x86_64/active/icons".into(),
        };
        let flathub = app_catalogue::appstream::parse(appstream.as_bytes(), &source, "en")
            .expect("the Flathub catalogue parses");
        let state = StoreCatalogueTestState {
            installed,
            tilo: tilo
                .iter()
                .map(|(package_id, display_name)| CatalogueApp {
                    package_id: (*package_id).into(),
                    display_name: (*display_name).into(),
                    version: "1.0.0".into(),
                    registry: "Tilo".into(),
                    trust: "publisher-signed".into(),
                    ..CatalogueApp::default()
                })
                .collect(),
            shared: SharedCatalogue {
                flathub: Arc::new(flathub),
                blocks: Arc::new(app_catalogue::BlockList::empty()),
            },
        };
        self.registry
            .register(Rc::new(StoreCatalogueProvider::with_test_source(
                StoreCatalogueTestSource::new(state),
            )));
        self.settled()
    }

    /// The palette on a home holding these `~/…` files, every one changed
    /// just now, so recency favours them as much as it ever does.
    fn with_files(mut self, names: &[&str]) -> Self {
        self.registry
            .register(Rc::new(FilesProvider::with_walked_names(names)));
        self
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
const fn setting(id: &'static str) -> Expect {
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
        ("vpn", action("quick-settings.vpn.set-enabled")),
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
        // The other systems' browsers are names of the Browser while no app
        // of that name is installed (one that is leads by its own name).
        ("firefox", app("so.tilo.browser")),
        ("chrome", app("so.tilo.browser")),
        ("photoshop", not_app("so.tilo.canvas")),
        ("spotify", not_app("so.tilo.music")),
        ("notion", not_app("so.tilo.notes")),
        ("vscode", not_app("so.tilo.code-editor")),
        ("github", not_app("so.tilo.git-client")),
        ("outlook", not_app("so.tilo.mail")),
        ("zoom", not_app("so.tilo.remote")),
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
        // A four-letter word with a letter dropped.
        ("nots", app("so.tilo.notes")),
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
        // The switch itself, its state on the row, or the page.
        (
            "bluetooth",
            Expect::OneOf(&[
                (ACTION, "quick-settings.bluetooth.toggle"),
                (PAGE, "page:bluetooth"),
            ]),
        ),
        // The layout in place, its value on the row; one row, not also a
        // link to the same setting on its page.
        ("keyboard layout", setting("setting:keyboard.layout")),
        ("tastaturbelegung", setting("setting:keyboard.layout")),
        ("qwertz", setting("setting:keyboard.layout")),
        ("tastatur", page("page:keyboard")),
        ("touchpad", page("page:pointing")),
        ("night light", setting("setting:display.night_light")),
        ("dark mode", action("appearance.theme.dark")),
        ("charge cap", setting("setting:power.charge_limit")),
        ("touchpad speed", setting("setting:pointing.touchpad_speed")),
        ("tap to click", setting("setting:pointing.tap_to_click")),
        (
            "disable touchpad while typing",
            setting("setting:pointing.disable_while_typing"),
        ),
        ("24-hour", setting("setting:region.time")),
        ("time zone", setting("setting:time.zone")),
        ("zeitzone", setting("setting:time.zone")),
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
        // A phrase keyword is not its first word: "rechner" is the
        // calculator, never Restart ("rechner neu starten"), and
        // "bildschirm" is the screen, not Lock ("bildschirm sperren").
        ("rechner neu starten", session("restart")),
        ("bildschirm", Expect::Not(SESSION, "lock")),
        // Everyday German for the sound.
        ("lauter", action("quick-settings.volume.up")),
        ("leiser", action("quick-settings.volume.down")),
        ("louder", action("quick-settings.volume.up")),
        ("mute", action("quick-settings.volume.mute")),
        ("stumm", action("quick-settings.volume.mute")),
        ("dimmer", action("quick-settings.brightness.down")),
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
fn an_innocent_word_never_offers_to_restart() {
    let palette = Palette::new();
    for query in ["rechner", "Rechner", "bildschirm"] {
        let rows = palette.ask(query);
        assert!(
            !rows.iter().any(|row| row.provider_id == SESSION
                && matches!(row.id.as_str(), "restart" | "shut-down" | "logout")),
            "{query} offered to end the session: {:?}",
            rows.iter()
                .map(|row| format!("{}/{}", row.provider_id, row.id))
                .collect::<Vec<_>>()
        );
    }
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

/// Words a person types, in either language, typos and abbreviations included.
const TYPED: [&str; 10] = [
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

/// Set by the scheduled performance job to hold the CPU budgets below, which
/// depend on the machine; every run holds the work counts.
const SEARCH_PERF_GATE: &str = "TILO_SEARCH_PERF_GATE";

fn perf_gate() -> bool {
    std::env::var_os(SEARCH_PERF_GATE).is_some()
}

/// The palette refreshes on the compositor thread for every key, and the
/// providers are single-threaded, so the answer is incremental instead: the
/// apps' index and every list's index are retained and only searched, and
/// the pages' copy is built once per language. Every run holds that no key
/// builds an index; the performance job also holds a keystroke at full index
/// size, revision check, search, merge and grouping included, to well under
/// the 1 ms the compositor can spare.
#[test]
fn a_keystroke_against_the_full_index_only_searches_it() {
    let palette = Palette::new();
    // Typing is every prefix of the word, one key at a time.
    let mut keystrokes: Vec<String> = Vec::new();
    for word in TYPED {
        let letters: Vec<char> = word.chars().collect();
        for end in 1..=letters.len() {
            keystrokes.push(letters[..end].iter().collect());
        }
    }
    // The first query warms the retained indexes, as opening the palette does.
    for key in &keystrokes {
        let _ = palette.keystroke(key);
    }
    assert_eq!(
        palette.index_builds(),
        (1, 1),
        "the apps' index and the pages' copy are each built once"
    );
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
    assert_eq!(palette.index_builds(), (1, 1), "a keystroke built an index");
    samples.sort();
    let median = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    println!(
        "{} keystrokes: median {median:?}, p95 {p95:?}, max {:?}",
        samples.len(),
        samples[samples.len() - 1]
    );
    if !perf_gate() {
        return;
    }
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

/// A whole query, ungrouped, builds nothing either; the performance job also
/// holds it to a keystroke's budget.
#[test]
fn a_query_against_the_full_index_only_searches_it() {
    let palette = Palette::new();
    // The first query of each warms the retained indexes, as the first
    // keystroke after the palette opens does.
    for query in TYPED {
        let _ = palette.ask(query);
    }
    assert_eq!(palette.index_builds(), (1, 1));
    let mut worst = Duration::ZERO;
    for query in TYPED {
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
    assert_eq!(palette.index_builds(), (1, 1), "a query built an index");
    if !perf_gate() {
        return;
    }
    assert!(
        worst < budget(Duration::from_millis(2)),
        "a keystroke against the full index took {worst:?}; the budget is {:?}",
        budget(Duration::from_millis(2))
    );
}

fn titles(rows: &[SearchResult]) -> Vec<&str> {
    rows.iter().map(|row| row.title.as_str()).collect()
}

/// Two letters begin an app's name: "pi" is not looking for Speed Test by
/// its keyword "ping", Video Editor by "pitivi" or Camera by "take a
/// picture". Typed further, those words find them again.
#[test]
fn two_letters_find_apps_by_name_not_by_a_keyword_they_begin() {
    let palette = Palette::new();
    let apps = |rows: Vec<SearchResult>| -> Vec<String> {
        rows.into_iter()
            .filter(|row| row.kind == ResourceKind::Application)
            .map(|row| row.title)
            .collect()
    };
    for found in [apps(palette.ask("pi")), apps(palette.shown("pi"))] {
        for app in ["Speed Test", "Video Editor", "Camera"] {
            assert!(!found.iter().any(|title| title == app), "{found:?}");
        }
    }
    assert!(
        apps(palette.ask("ping"))
            .iter()
            .any(|title| title == "Speed Test"),
        "a keyword typed whole still finds its app"
    );
}

/// The busy launcher's desk: "pi" finds the note the person was just in by
/// its name, and nothing whose "pi" only begins a keyword or sits inside a
/// word of a description ("ping", "take a picture", "typing").
#[test]
fn pi_finds_the_pitch_deck_continue_lists_and_nothing_pi_only_begins() {
    let palette = Palette::new().with_places(&[
        ("Pitch deck", "pages/pitch"),
        ("Launch plan", "pages/launch"),
        ("Interview notes", "pages/interviews"),
    ]);
    for rows in [palette.ask("pi"), palette.shown("pi")] {
        let found = titles(&rows);
        assert!(found.contains(&"Pitch deck"), "{found:?}");
        for not_begun in [
            "Speed Test",
            "Video Editor",
            "Camera",
            "Choose an emoji",
            "Launch plan",
            "Interview notes",
        ] {
            assert!(!found.contains(&not_begun), "{not_begun} in {found:?}");
        }
    }
    assert!(
        titles(&palette.ask("pitch")).contains(&"Pitch deck"),
        "and by more of its name"
    );
}

#[test]
fn youtube_cats_offers_search_youtube_for_cats_among_the_first_three_rows() {
    let palette = Palette::new().with_web();
    for rows in [palette.ask("youtube cats"), palette.shown("youtube cats")] {
        assert!(
            titles(&rows)
                .iter()
                .take(3)
                .any(|title| *title == "Search YouTube for cats"),
            "{:?}",
            titles(&rows)
        );
    }
}

#[test]
fn spotify_ends_with_search_the_web_for_spotify() {
    let palette = Palette::new().with_web();
    for rows in [palette.ask("spotify"), palette.shown("spotify")] {
        assert_eq!(
            titles(&rows).last().copied(),
            Some("Search the web for spotify"),
            "{:?}",
            titles(&rows)
        );
    }
}

#[test]
fn a_service_named_alone_offers_its_site_below_the_app_that_serves_it() {
    let palette = Palette::new().with_web();
    let rows = palette.ask("youtube");
    assert_eq!(
        rows.first()
            .map(|row| (row.provider_id.as_str(), row.id.as_str())),
        Some(("applications", "so.tilo.tube")),
        "{:?}",
        titles(&rows)
    );
    assert!(
        rows.iter()
            .any(|row| row.provider_id == "web" && row.id == "service:youtube"),
        "{:?}",
        titles(&rows)
    );
    assert_eq!(
        titles(&rows).last().copied(),
        Some("Search the web for youtube")
    );
}

#[test]
fn finder_and_explorer_lead_with_files_ahead_of_files_named_like_them() {
    let palette = Palette::new().with_files(&[
        "~/finder",
        "~/Documents/finder notes.txt",
        "~/Documents/Finder shortcuts.md",
        "~/explorer",
        "~/Downloads/explorer-setup.exe",
        "~/Documents/Explorer.md",
        "~/explorations.txt",
        "~/projects/explorer/README.md",
    ]);
    for query in ["finder", "Finder", "explorer", "explo", "file explorer"] {
        for rows in [palette.ask(query), palette.shown(query)] {
            assert_eq!(
                rows.first()
                    .map(|row| (row.provider_id.as_str(), row.id.as_str())),
                Some((APP, "so.tilo.file-explorer")),
                "{query:?}: {:?}",
                titles(&rows)
            );
        }
    }
    assert!(
        palette
            .ask("explorer")
            .iter()
            .any(|row| row.provider_id == "files"),
        "the files named like it are still found"
    );
}

/// A Flathub `AppStream` document: the apps whose names, summaries or keywords
/// say "music", as Flathub really lists them, GNOME's own "Music" among them.
const FLATHUB_MUSIC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<components version="0.16" origin="flathub">
  <component type="desktop-application"><id>org.gnome.Music</id><name>Music</name><summary>Play and organize your music collection</summary><keywords><keyword>music</keyword></keywords><bundle type="flatpak">app/org.gnome.Music/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>com.spotify.Client</id><name>Spotify</name><summary>Online music streaming service</summary><keywords><keyword>music</keyword></keywords><bundle type="flatpak">app/com.spotify.Client/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>org.musicbrainz.Picard</id><name>MusicBrainz Picard</name><summary>Tag your music files</summary><bundle type="flatpak">app/org.musicbrainz.Picard/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>org.strawberrymusicplayer.strawberry</id><name>Strawberry Music Player</name><summary>Play and organize your music collection</summary><bundle type="flatpak">app/org.strawberrymusicplayer.strawberry/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>com.github.taiko2k.tauonmb</id><name>Tauon Music Box</name><summary>A modern streamlined music player</summary><bundle type="flatpak">app/com.github.taiko2k.tauonmb/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>app.ytmdesktop.ytmdesktop</id><name>YouTube Music Desktop App</name><summary>A desktop app for YouTube Music</summary><bundle type="flatpak">app/app.ytmdesktop.ytmdesktop/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>org.gnome.Lollypop</id><name>Lollypop</name><summary>Play and organize your music collection</summary><keywords><keyword>music</keyword></keywords><bundle type="flatpak">app/org.gnome.Lollypop/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>io.bassi.Amberol</id><name>Amberol</name><summary>Plays music, and nothing else</summary><keywords><keyword>music</keyword></keywords><bundle type="flatpak">app/io.bassi.Amberol/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>org.musescore.MuseScore</id><name>MuseScore</name><summary>Create, play and print sheet music</summary><keywords><keyword>music</keyword></keywords><bundle type="flatpak">app/org.musescore.MuseScore/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>org.feichtmeier.Musicpod</id><name>Musicpod</name><summary>Music, radio and podcast player</summary><bundle type="flatpak">app/org.feichtmeier.Musicpod/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>org.nickvision.tubeconverter</id><name>Parabolic</name><summary>Download web video and music</summary><keywords><keyword>music</keyword></keywords><bundle type="flatpak">app/org.nickvision.tubeconverter/x86_64/stable</bundle></component>
  <component type="desktop-application"><id>com.rafaelmardojai.Blanket</id><name>Blanket</name><summary>Listen to ambient sounds</summary><bundle type="flatpak">app/com.rafaelmardojai.Blanket/x86_64/stable</bundle></component>
</components>"#;

/// The device of the music tests: Tilo's apps installed, the Musicpod Flatpak
/// installed (so Flathub no longer offers it), Tilo's "Sheet Music" in the
/// catalogue, Flathub's other music apps offered, and a home with music in
/// its file names.
fn music_palette() -> Palette {
    let musicpod = HostApp {
        desktop_id: "org.feichtmeier.Musicpod".into(),
        name: "Musicpod".into(),
        kind: desktop_entry::EntryKind::Flatpak,
        categories: vec!["Audio".into()],
        keywords: vec!["music".into()],
        mime_types: Vec::new(),
        terminal: false,
        icon: None,
        window_identities: vec!["org.feichtmeier.Musicpod".into()],
        downloaded: None,
    };
    Palette::new()
        .with_files(&[
            "~/Documents/music theory notes.md",
            "~/Documents/Music lessons.pdf",
            "~/Downloads/music-festival-tickets.pdf",
        ])
        .with_catalogue(
            vec![musicpod],
            &[("so.tilo.sheet-music", "Sheet Music")],
            FLATHUB_MUSIC,
        )
}

#[test]
fn installed_and_tilo_apps_lead_then_own_content_then_flatpak_store_pages() {
    let palette = music_palette();
    // A Store page of a Flathub app the person does not have.
    let uninstalled_flatpak = |row: &SearchResult| {
        palette.registry.availability(row) == Availability::ThirdPartyCatalogue
    };
    let shown = palette.shown("music");
    let first_shown_file = shown
        .iter()
        .position(|row| row.provider_id == "files")
        .expect("the palette shows the person's music files");
    assert!(
        first_shown_file > 0
            && shown[..first_shown_file]
                .iter()
                .all(|row| row.kind == ResourceKind::Application),
        "the palette leads with the person's apps: {:?}",
        titles(&shown)
    );
    assert!(
        shown
            .iter()
            .skip_while(|row| !uninstalled_flatpak(row))
            .all(&uninstalled_flatpak),
        "the palette lists Store pages last: {:?}",
        titles(&shown)
    );
    {
        let rows = palette.ask("music");
        let at = |provider: &str, id: &str| {
            rows.iter()
                .position(|row| row.provider_id == provider && row.id == id)
                .unwrap_or_else(|| panic!("{provider}:{id} in {:?}", titles(&rows)))
        };
        assert_eq!(
            at(APP, "so.tilo.music"),
            0,
            "the installed Music leads, however exactly GNOME's Music matches: {:?}",
            titles(&rows)
        );
        let musicpod = at("host-applications", "org.feichtmeier.Musicpod");
        let sheet_music = at("store-catalogue", "app:so.tilo.sheet-music");
        let first_file = rows
            .iter()
            .position(|row| row.provider_id == "files")
            .expect("the person's music files are found");
        let first_flatpak = rows
            .iter()
            .position(&uninstalled_flatpak)
            .expect("Flathub's music apps are still listed");
        assert!(
            musicpod < first_file && sheet_music < first_file,
            "the installed Flatpak and the Tilo app lead the person's files: {:?}",
            titles(&rows)
        );
        assert!(
            rows.iter().skip(first_flatpak).all(&uninstalled_flatpak),
            "every Store page of an uninstalled Flatpak follows everything the person \
             has, their files among it: {:?}",
            titles(&rows)
        );
        assert_eq!(
            rows.get(first_flatpak).map(|row| row.id.as_str()),
            Some("flathub:org.gnome.Music"),
            "among themselves, Store pages rank by match: {:?}",
            titles(&rows)
        );
    }
    assert_eq!(
        palette
            .ask("music")
            .iter()
            .filter(|row| uninstalled_flatpak(row))
            .count(),
        10,
        "every Flathub app that matches and is not installed is still found"
    );
}

/// No two groups share a heading: when the person's apps and third-party
/// Store pages both match, the Store pages are their own group, after the
/// person's own content, and Tilo apps that open at once stay under Apps.
#[test]
fn store_pages_have_their_own_heading_after_the_persons_content() {
    let palette = music_palette();
    let groups = palette.with_args("music", |args| {
        palette
            .registry
            .query_grouped(args, ui::search::system::PALETTE_ROW_BUDGET)
    });
    let headings: Vec<String> = groups
        .iter()
        .map(|group| group.kind.localized_label())
        .collect();
    let mut unique = headings.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        headings.len(),
        "a heading twice: {headings:?}"
    );
    let store = crate::shell::search::in_the_store();
    assert_eq!(
        groups.last().map(|group| &group.kind),
        Some(&store),
        "the Store pages come last: {headings:?}"
    );
    for group in &groups {
        let third_party = group
            .results
            .iter()
            .filter(|row| palette.registry.availability(row) == Availability::ThirdPartyCatalogue);
        if group.kind == store {
            assert_eq!(third_party.count(), group.results.len(), "{headings:?}");
        } else {
            assert_eq!(
                third_party.count(),
                0,
                "{:?} holds a Store page",
                group.kind
            );
        }
    }
    // Every row, not the palette's budgeted share: one Apps group holds
    // every kind of app, so a budgeted one shows only its first few.
    let untrimmed = palette.with_args("music", |args| palette.registry.query_grouped(args, 0));
    let applications = untrimmed
        .iter()
        .find(|group| group.kind == ResourceKind::Application)
        .expect("the person's apps");
    assert!(
        applications
            .results
            .iter()
            .any(|row| row.id == "app:so.tilo.sheet-music"),
        "the Tilo app that opens at once stays under Apps"
    );
}

#[test]
fn a_query_only_a_flatpak_matches_still_finds_its_store_page() {
    let palette = music_palette();
    for rows in [palette.ask("picard"), palette.shown("picard")] {
        assert_eq!(
            rows.first().map(|row| row.id.as_str()),
            Some("flathub:org.musicbrainz.Picard"),
            "{:?}",
            titles(&rows)
        );
    }
}
