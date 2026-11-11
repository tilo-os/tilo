//! An English window reads English, on any device.
//! A window speaks the language of its own catalogue. The toolkit's widget labels and the Settings
//! index are in every window's binary, in every language they ship, and ride along in the language
//! the window selected: a German machine never reads a German word inside an English window (the
//! search panel's "Alle 13 zeigen", an editor's "Etwas schreiben…").
//! Each surface is read twice on a German device: with the catalogues it ships as its own (its
//! source alone, as a device with no language pack installed has it) and with the same window on an
//! English device. A word from a catalogue the window does not speak shows as a difference between
//! the two.

use std::collections::BTreeMap;

use tilo_copy_lint::catalog;
use ui::i18n::{ArgValue, EmbeddedCatalog, I18nManager};

/// The toolkit's widget labels, in the languages every binary carries them.
const TOOLKIT_EN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../sdk/ui-core/locales/en-US.ftl"
));

/// The Settings index: every window offers the same settings, in the same words.
const INDEX_EN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../system/settings-store/locales/en-US.ftl"
));
const INDEX_DE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../system/settings-store/locales/de-DE.ftl"
));

/// The index as the desktop loads it.
static INDEX: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", &[("en-US", INDEX_EN), ("de-DE", INDEX_DE)])
        .with_system_locale()
        .as_shared();

/// A surface a person uses: its own catalogue's source, and the same catalogue in the language a
/// pack carries when the window has one installed.
struct Window {
    name: &'static str,
    source: &'static str,
    /// The language pack's tag and text: `None` for a window translated by its own binary only
    /// (none here), or one whose pack is not installed.
    german: Option<(&'static str, &'static str)>,
}

macro_rules! catalogue {
    ($path:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), $path))
    };
}

/// The windows the person works in day to day.
const WINDOWS: &[Window] = &[
    Window {
        name: "the session's menus and notices",
        source: catalogue!("/locales/en-US.ftl"),
        german: Some(("de-DE", catalogue!("/locales/de-DE.ftl"))),
    },
    Window {
        name: "search",
        source: catalogue!("/../desktop-search/locales/en-US.ftl"),
        german: Some(("de-DE", catalogue!("/../desktop-search/locales/de-DE.ftl"))),
    },
    Window {
        name: "the desktop surface's menus",
        source: catalogue!("/../desktop-surface/locales/en-US.ftl"),
        german: Some(("de-DE", catalogue!("/../desktop-surface/locales/de-DE.ftl"))),
    },
    Window {
        name: "quick settings",
        source: catalogue!("/../quick-settings/locales/en-US.ftl"),
        german: Some(("de-DE", catalogue!("/../quick-settings/locales/de-DE.ftl"))),
    },
    Window {
        name: "Tasks",
        source: catalogue!("/../../../apps/tasks/locales/en-US.ftl"),
        german: None,
    },
    Window {
        name: "Notes",
        source: catalogue!("/../../../apps/notes/locales/en-US.ftl"),
        german: Some(("de", catalogue!("/../../../apps/notes/locales/de.ftl"))),
    },
    Window {
        name: "Settings",
        source: catalogue!("/../../../apps/settings/locales/en-US.ftl"),
        german: Some((
            "de-DE",
            catalogue!("/../../../apps/settings/locales/de-DE.ftl"),
        )),
    },
    Window {
        name: "Settings' providers",
        source: catalogue!("/../../sdk/provider-settings-ui/locales/en-US.ftl"),
        german: Some((
            "de-DE",
            catalogue!("/../../sdk/provider-settings-ui/locales/de-DE.ftl"),
        )),
    },
];

/// Every key a window can draw: its own words and the shared ones (the toolkit's labels, the
/// index).
fn keys(window: &Window) -> Vec<String> {
    let mut keys: Vec<String> = catalog::messages(window.source).into_keys().collect();
    for shared in [TOOLKIT_EN, INDEX_EN] {
        for key in catalog::messages(shared).into_keys() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

/// Every key of `window` resolved on a thread `device` speaks, with the window's `catalogues`
/// loaded and the index beside them (the toolkit's labels join any manager by themselves).
fn resolved(
    window: &Window,
    catalogues: &[(&str, &str)],
    device: &str,
) -> BTreeMap<String, String> {
    let installed = I18nManager::install("en-US", device, catalogues);
    assert!(
        installed.is_ok(),
        "{}'s catalogue installs: {installed:?}",
        window.name
    );
    INDEX.ensure();
    keys(window)
        .into_iter()
        .map(|key| {
            let said = I18nManager::lookup(&key, None);
            (key, said)
        })
        .collect()
}

/// The window ships its source only: the device's language pack for it is not installed.
fn shipped_source(window: &Window) -> Vec<(&str, &str)> {
    vec![("en-US", window.source)]
}

/// The window ships its source and the pack beside it.
fn with_pack(window: &Window) -> Vec<(&str, &str)> {
    let mut catalogues = shipped_source(window);
    catalogues.extend(window.german);
    catalogues
}

#[test]
fn every_surface_reads_the_same_on_a_german_and_an_english_device() {
    for window in WINDOWS {
        let german_device = resolved(window, &shipped_source(window), "de-DE");
        let english_device = resolved(window, &shipped_source(window), "en-US");
        for (key, english) in &english_device {
            assert_eq!(
                german_device.get(key),
                Some(english),
                "{} reads {key:?} differently on a German device: the window ships its source \
                 only, so this word comes from a catalogue it does not speak",
                window.name,
            );
        }
    }
}

/// A window that has the pack installed still reads German on a German device: the rule is the
/// window's own language, not dropping translations.
#[test]
fn a_window_with_its_pack_installed_reads_its_language() {
    for window in WINDOWS {
        let Some(german) = window.german else {
            continue;
        };
        let german_device = resolved(window, &with_pack(window), "de-DE");
        let english_device = resolved(window, &shipped_source(window), "en-US");
        let own = catalog::messages(window.source);
        assert!(
            own.keys()
                .any(|key| german_device[key] != english_device[key]),
            "{} has {} and read English on a German device",
            window.name,
            german.0,
        );
    }
}

/// A package the session reads for someone else says nothing about the language the session draws
/// in: seeding a demo app that ships German (Notes does) leaves the session's own words — the
/// toolkit's — English on a German device. The same words are the package's own when the process is
/// that package's window, and then they are German.
#[test]
fn a_seeded_packages_words_do_not_make_the_session_speak_its_language() {
    let notes = WINDOWS
        .iter()
        .find(|window| window.name == "Notes")
        .expect("Notes is one of the windows");
    let (locale, german) = notes.german.expect("Notes ships German");
    let seed = [(locale.to_owned(), german.to_owned())];
    let session = WINDOWS
        .iter()
        .find(|window| window.name == "the session's menus and notices")
        .expect("the session is one of the windows");
    let show_all = || INDEX.t_args("search-show-all", &[("count", ArgValue::from(13))]);

    I18nManager::install("en-US", "de-DE", &shipped_source(session)).expect("the session installs");
    I18nManager::add_shared_resource(&seed[0].0, seed[0].1.clone());
    assert_eq!(show_all(), "Show all 13");
    assert_eq!(
        I18nManager::lookup("block-editor-empty", None),
        "Write something…"
    );

    // The same package in a process that is its own window: its language is the window's.
    I18nManager::install("en-US", "de-DE", &shipped_source(notes)).expect("Notes installs");
    I18nManager::add_resource(&seed[0].0, seed[0].1.clone());
    assert_eq!(show_all(), "Alle 13 zeigen");
}

/// The two strings a person read in German inside an English session, read where they were drawn.
#[test]
fn toolkit_words_read_in_the_windows_language() {
    let show_all = || INDEX.t_args("search-show-all", &[("count", ArgValue::from(13))]);
    let window = WINDOWS
        .iter()
        .find(|window| window.name == "search")
        .expect("search is one of the windows");

    I18nManager::install("en-US", "de-DE", &shipped_source(window)).expect("search installs");
    assert_eq!(show_all(), "Show all 13");
    assert_eq!(
        I18nManager::lookup("block-editor-empty", None),
        "Write something…"
    );

    // The same search window with its German pack installed reads German.
    I18nManager::install("en-US", "de-DE", &with_pack(window)).expect("search installs in both");
    assert_eq!(show_all(), "Alle 13 zeigen");
    assert_eq!(
        I18nManager::lookup("block-editor-empty", None),
        "Etwas schreiben…"
    );
}
