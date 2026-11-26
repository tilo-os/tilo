use fluent::FluentArgs;
use fluent::FluentResource;
use fluent::FluentValue;
use std::cell::Cell;
use std::sync::Arc;
use tilo_ui_base::i18n::*;
use unic_langid::LanguageIdentifier;

/// One line Fluent cannot read refuses the whole catalogue, and every toolkit label then
/// shows as its key: each language the toolkit ships parses whole.
#[test]
fn every_toolkit_catalogue_parses_whole() {
    for (locale, text) in [
        ("en-US", include_str!("../../../ui-base/locales/en-US.ftl")),
        ("de-DE", include_str!("../../../ui-base/locales/de-DE.ftl")),
    ] {
        if let Err((_, errors)) = FluentResource::try_new(text.to_owned()) {
            panic!("the {locale} toolkit catalogue does not parse: {errors:?}");
        }
    }
}

/// The languages the desktop speaks are the ones its own catalogue names, each in its own
/// words, so a picker offers exactly what the desktop can show.
#[test]
fn the_desktop_speaks_the_languages_its_catalogue_names() {
    let spoken = spoken_languages();
    assert!(
        spoken
            .iter()
            .any(|(tag, name)| tag == "en" && name == "English"),
        "{spoken:?}"
    );
    assert!(
        spoken
            .iter()
            .any(|(tag, name)| tag == "de" && name == "Deutsch"),
        "{spoken:?}"
    );
}

/// A catalogue embeds only its source; its other languages come from the language packs, here the
/// crate's own `locales/` directory.
#[test]
fn the_interface_language_is_the_one_the_settings_name() {
    let _restore = RestoreLookupState::capture();
    let document = serde_json::json!({"appearance": {"language": "en-US"}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let _settings = tilo_ui_base::settings_source::scoped_source(Some(std::sync::Arc::new(
        tilo_ui_base::settings_source::HostDocument::new(document, |_| Ok(())),
    )));
    assert_eq!(preferred_language().as_deref(), Some("en-US"));
    I18nManager::install_system(
        "en-US",
        &[
            ("en-US", "greeting = Hello"),
            ("de-DE", "greeting = Guten Tag"),
        ],
    )
    .unwrap();
    assert_eq!(
        I18nManager::current_locale(),
        "en-US",
        "the settings' language, not the machine's"
    );
    assert_eq!(I18nManager::lookup_plain("greeting"), "Hello");
    let day = jiff::civil::Date::new(2025, 11, 5).unwrap();
    assert_eq!(
        tilo_ui_base::format::day_heading(day, false),
        "Wednesday, November 5"
    );
    I18nManager::set_locale("de-DE");
    assert_eq!(
        tilo_ui_base::format::day_heading(day, false),
        "Mittwoch, 5. November"
    );
}

tilo_test_env::scenario! {
    fn a_window_speaks_the_language_selected_for_it;

    phase an_english_only_app_leaves_the_window_english;
    // A window speaks one language: an app that ships English only leaves the toolkit's words and the
    // platform's formats English too, whatever the person's preference, while the person's own
    // language stays German for typed input without bumping the display revision.
    {
        let _restore = RestoreLookupState::capture();
        I18nManager::install(
            "en-US",
            "de-DE",
            &[
                ("en-US", "en-only = English"),
                ("de-DE", "de-only = Deutsch"),
            ],
        )
        .unwrap();
        let catalog_rev = || I18nManager::revision().wrapping_sub(tilo_ui_base::format::region::revision());
        let rev_before = catalog_rev();
        assert_eq!(
            set_person_locale(Some("de-DE")),
            None,
            "previous override is None when unset"
        );
        assert_eq!(
            catalog_rev(),
            rev_before,
            "setting only the person's language does not bump the display revision"
        );
        assert_eq!(
            window_language(&["en-US", "de-DE"]),
            "de-DE",
            "window_language and person_locale read the same single source"
        );
        assert_eq!(I18nManager::lookup_plain("de-only"), "Deutsch");
        let language = speak_window_language(&[("en-US".to_owned(), "d-new = New".to_owned())]);
        assert_eq!(
            language, "en-US",
            "the app ships English only, so its window does"
        );
        assert_eq!(I18nManager::current_locale(), "en-US");
        assert_eq!(
            person_locale().as_deref(),
            Some("de-DE"),
            "narrowing the window to English leaves the person's language German"
        );
        // A subsequent `I18nManager::set_locale("en-US")` (e.g. from ToUi::Speak) leaves
        // `person_locale` untouched as well.
        I18nManager::set_locale("en-US");
        assert_eq!(person_locale().as_deref(), Some("de-DE"));
        assert_eq!(set_person_locale(None).as_deref(), Some("de-DE"));
        assert_eq!(
            set_person_locale(None),
            None,
            "clearing restores None rather than a resolved setting"
        );
        assert_eq!(I18nManager::lookup_plain("d-new"), "New");
        assert_eq!(
            I18nManager::lookup_plain("de-only"),
            "de-only",
            "the window stopped speaking German"
        );
    }

    phase a_window_speaks_its_language_over_the_toolkits_own_words;
    {
        let _restore = RestoreLookupState::capture();
        // A thread that has drawn nothing yet: its first catalogue is the toolkit's, installed
        // in the shell's language, and the window's language must win over it.
        let language = speak_window_language(&[("en-US".to_owned(), "d-new = New".to_owned())]);
        assert_eq!(language, "en-US");
        assert_eq!(I18nManager::current_locale(), "en-US");
        assert_eq!(I18nManager::lookup_plain("d-new"), "New");
        assert_eq!(
            I18nManager::lookup_plain("date-field-add"),
            "Add date",
            "the toolkit's own words follow the window's language"
        );
    }

    phase a_refused_edit_is_said_in_the_windows_language;
    // An edit an app declined is said in the person's own language, with the limit it is held
    // to: the one word set a native and a hosted window both read.
    {
        let _restore = RestoreLookupState::capture();
        I18nManager::register_widget_messages();
        I18nManager::set_locale("en-US");
        assert_eq!(
            declined_text(tilo_actions::Declined::TooLong),
            "This paste is too long to keep (limit 8 MB). Paste it in smaller parts."
        );
        assert_eq!(
            declined_text(tilo_actions::Declined::Changed),
            "This paragraph changed while you were typing. Your edit wasn't kept."
        );
        I18nManager::set_locale("de-DE");
        assert_eq!(
            declined_text(tilo_actions::Declined::TooLong),
            "Der eingefügte Text ist zu lang (höchstens 8 MB). Füge ihn in kleineren Stücken ein."
        );
        assert_eq!(
            declined_text(tilo_actions::Declined::Changed),
            "Dieser Absatz hat sich beim Tippen geändert. Deine Änderung wurde nicht übernommen."
        );
    }

    phase a_window_with_no_catalogue_speaks_its_selected_language;
    // A window with no catalogue of its own — a system menu, a sheet of frames — has no language of
    // its own to hold: the toolkit's labels read in the language selected for it, which is the
    // person's.
    {
        let _restore = RestoreLookupState::capture();
        I18nManager::register_widget_messages();
        I18nManager::set_locale("de-DE");
        assert_eq!(
            I18nManager::lookup_plain("menu-query-placeholder"),
            "Tippen zum Filtern",
            "the filter field's hint is in the person's language"
        );
        assert_eq!(
            I18nManager::lookup_plain("date-field-add"),
            "Datum hinzufügen"
        );
        I18nManager::add_shared_resource("de-DE", "host-sort = Sortieren".to_owned());
        assert_eq!(I18nManager::lookup_plain("date-field-add"), "Add date");
    }

    phase a_word_from_another_languages_catalogue_is_recorded;
    // A word a window shows from a catalogue in another language is recorded, so a check can hold a
    // window to one language: a translation the app lacks (its source language in its place), or a
    // catalogue of used package read in the wrong language.
    {
        let _restore = RestoreLookupState::capture();
        install_plain(
            "en-US",
            "de-DE",
            &[
                (
                    "en-US",
                    "probe = Probe\nprobe-only-english = English only\n",
                ),
                ("de-DE", "probe = Sonde\n"),
            ],
        );
        I18nManager::clear_foreign_words();
        assert_eq!(I18nManager::window_locale(), "de-DE");
        assert_eq!(I18nManager::lookup_plain("probe"), "Sonde");
        assert_eq!(
            I18nManager::foreign_words(),
            Vec::<(String, String)>::new(),
            "a word the window's own language holds is no foreign word"
        );
        assert_eq!(
            I18nManager::lookup_plain("probe-only-english"),
            "English only"
        );
        assert_eq!(
            I18nManager::foreign_words(),
            vec![("probe-only-english".to_owned(), "en-US".to_owned())],
            "the source language stood in for the missing translation"
        );
        assert_eq!(
            I18nManager::lookup_plain("probe-only-english"),
            "English only"
        );
        assert_eq!(
            I18nManager::foreign_words().len(),
            1,
            "each word is recorded once"
        );
        I18nManager::clear_foreign_words();
        assert!(I18nManager::foreign_words().is_empty());
    }

    phase locale_selection_parses_posix_values_and_revises_only_on_change;
    {
        let _restore = RestoreLookupState::capture();
        let parsed = |raw: &str| parse_locale(raw).ok().map(|locale| locale.to_string());
        for (raw, want) in [
            ("C", None),
            ("POSIX", None),
            ("", None),
            ("de_DE.UTF-8", Some("de-DE")),
            ("en_US", Some("en-US")),
            ("de:en_US", Some("de")),
        ] {
            assert_eq!(parsed(raw).as_deref(), want, "parse {raw:?}");
        }

        // The revision changes only for an effective locale change.
        I18nManager::install("en-US", "en-US", &[("en-US", EN), ("de", DE)]).unwrap();
        let initial = I18nManager::revision();
        I18nManager::set_locale("en_US.UTF-8");
        assert_eq!(I18nManager::revision(), initial);
        I18nManager::set_locale("de_DE.UTF-8");
        assert_ne!(I18nManager::revision(), initial);
        assert_eq!(I18nManager::lookup("greeting", None), "Guten Tag");

        I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
        for (raw, want) in [
            ("C", "en-US"),
            ("de_DE.UTF-8", "de-DE"),
            ("not a locale!", "en-US"),
        ] {
            I18nManager::set_locale_or_source(raw);
            assert_eq!(
                I18nManager::current_locale(),
                want,
                "set_locale_or_source({raw:?})"
            );
        }
        // `set_locale` itself keeps the selection on a value it cannot parse.
        I18nManager::set_locale("de");
        I18nManager::set_locale("C");
        assert_eq!(I18nManager::current_locale(), "de");
    }

    phase the_window_speaks_its_own_language_not_the_toolkits;
    // A window speaks the language of its own words.
    {
        static GERMAN_WINDOW: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "german-window-probe = a window"),
                ("de", "german-window-probe = ein Fenster"),
            ],
        );
        let show_all = || widget_format("search-show-all", &[("count", ArgValue::from(13))]);
        let _restore = RestoreLookupState::capture();

        // A German device and a window that ships English only.
        I18nManager::install("en-US", "de", &[("en-US", EN)]).unwrap();
        assert_eq!(I18nManager::current_locale(), "de");
        assert_eq!(show_all(), "Show all 13");

        GERMAN_WINDOW.ensure();
        assert_eq!(show_all(), "Alle 13 zeigen");
    }
}

tilo_test_env::scenario! {
    fn embedded_catalogues_load_share_and_read_their_languages;

    phase a_catalogue_reads_its_other_languages_from_the_language_packs;
    {
        static PACKED: EmbeddedCatalog =
            EmbeddedCatalog::new("en-US", &[("en-US", "packed-greeting = Hello\n")])
                .with_language_pack(LanguagePack {
                    id: "tilo-ui-pack-test",
                    locales: &["en-US", "de-DE"],
                    source_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/../ui-base/locales"),
                });
        let sources = PACKED.every_source();
        assert_eq!(sources.first().map(|(locale, _)| *locale), Some("en-US"));
        let german = sources
            .iter()
            .find(|(locale, _)| *locale == "de-DE")
            .expect("the German catalogue is read from the crate's locales");
        assert_eq!(
            german.1,
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../ui-base/locales/de-DE.ftl"))
                .unwrap()
        );
        let missing = LanguagePack {
            id: "tilo-ui-pack-test",
            locales: &["xx-XX"],
            source_dir: "/nonexistent",
        };
        assert_eq!(missing.read("xx-XX"), None);
        assert!(!missing.has("xx-XX"));
        assert_eq!(PACKED.languages(), ["en-US", "de-DE"]);
    }

    phase a_messages_words_come_back_in_every_language_the_catalogue_ships;
    {
        static SPELLED: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "spelled-name = Default\nspelled-only = English\n"),
                ("de-DE", "spelled-name = Standard\n"),
            ],
        );
        assert_eq!(SPELLED.every_value("spelled-name"), ["Default", "Standard"]);
        assert_eq!(SPELLED.every_value("spelled-only"), ["English"]);
        assert!(SPELLED.every_value("spelled-missing").is_empty());
    }

    phase embedded_catalogs_install_join_or_pin_their_manager;
    {
        static PINNED: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "pinned-probe = Pinned"),
                ("de", "pinned-probe = Festgelegt"),
            ],
        )
        .with_pinned_locale("en-US");

        {
            let _restore = RestoreLookupState::capture();
            assert_eq!(manager_id(), None);
            assert_eq!(EMBEDDED.t("embedded-probe"), "Probe");
            assert_eq!(EMBEDDED.current_locale(), "en-US");
            assert_eq!(
                EMBEDDED.t_args("embedded-greeting", &[("name", ArgValue::from("Ada"))]),
                "Hello Ada"
            );
        }

        {
            let _restore = RestoreLookupState::capture();
            I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
            let host = manager_id();
            assert_eq!(EMBEDDED.t("embedded-probe"), "Probe");
            assert_eq!(manager_id(), host);
            assert_eq!(I18nManager::current_locale(), "de");
            assert_eq!(I18nManager::lookup_plain("greeting"), "Guten Tag");

            // A manager installed later starts without the catalogue; the next
            // lookup loads it again rather than trusting a stale flag.
            I18nManager::install("en-US", "en-US", &[("en-US", EN)]).unwrap();
            assert_eq!(EMBEDDED.t("embedded-probe"), "Probe");
        }

        {
            let _restore = RestoreLookupState::capture();
            I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
            assert_eq!(PINNED.t("pinned-probe"), "Pinned");
            assert_eq!(I18nManager::current_locale(), "en-US");
        }
    }

    phase only_the_shown_language_and_the_source_are_parsed;
    // Only the language shown and the source are parsed; choosing another language parses it then, and
    // lets go of the one it replaced.
    {
        static SWITCHED: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "switch-probe = Hello\nswitch-only-source = Source"),
                ("de-DE", "switch-probe = Hallo"),
                ("fr", "switch-probe = Bonjour"),
            ],
        );
        let _restore = RestoreLookupState::capture();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(SWITCHED.t("switch-probe"), "Hello");
        assert_eq!(parsed_locales(), ["en-US"]);

        let revision = I18nManager::revision();
        SWITCHED.set_locale("de-DE");
        assert_ne!(I18nManager::revision(), revision);
        assert_eq!(SWITCHED.t("switch-probe"), "Hallo");
        assert_eq!(SWITCHED.t("switch-only-source"), "Source");
        assert_eq!(SWITCHED.t("switch-missing"), "switch-missing");
        assert_eq!(parsed_locales(), ["de-DE", "en-US"]);

        SWITCHED.set_locale("fr-CA");
        assert_eq!(SWITCHED.t("switch-probe"), "Bonjour");
        assert_eq!(parsed_locales(), ["en-US", "fr"]);

        SWITCHED.set_locale("en-US");
        assert_eq!(SWITCHED.t("switch-probe"), "Hello");
        assert_eq!(parsed_locales(), ["en-US"]);
    }

    phase threads_share_one_parse_of_a_catalogue;
    {
        static SHARED: EmbeddedCatalog =
            EmbeddedCatalog::new("en-US", &[("en-US", "shared-probe = Shared\n")]);
        let source = || {
            SHARED.ensure();
            MANAGER.with(|manager| {
                let manager = manager.borrow();
                let loaded = manager.as_ref()?.bundles.values().next()?;
                Some(Arc::as_ptr(loaded.resources.first()?).addr())
            })
        };
        let _restore = RestoreLookupState::capture();
        let here = source();
        assert!(here.is_some());
        let there = std::thread::spawn(source).join().unwrap();
        assert_eq!(here, there);
    }
}

struct RestoreLookupState {
    manager: Option<I18nManager>,
    person_locale: Option<LanguageIdentifier>,
    pseudo: PseudoLocale,
    pseudo_revision: u64,
}

impl RestoreLookupState {
    fn capture() -> Self {
        Self {
            manager: MANAGER.with(|manager| manager.borrow_mut().take()),
            person_locale: PERSON_LOCALE.with(|held| held.borrow_mut().take()),
            pseudo: PSEUDO_LOCALE.with(Cell::get),
            pseudo_revision: PSEUDO_REVISION.with(Cell::get),
        }
    }
}

impl Drop for RestoreLookupState {
    fn drop(&mut self) {
        MANAGER.with(|manager| *manager.borrow_mut() = self.manager.take());
        PERSON_LOCALE.with(|held| *held.borrow_mut() = self.person_locale.take());
        PSEUDO_LOCALE.with(|mode| mode.set(self.pseudo));
        PSEUDO_REVISION.with(|revision| revision.set(self.pseudo_revision));
    }
}

const EN: &str = "greeting = Hello\nonly-source = Source fallback";
const DE: &str = "greeting = Guten Tag";

fn install_plain(source: &str, locale: &str, catalogs: &[(&str, &str)]) {
    I18nManager::install(source, locale, catalogs).unwrap();
    I18nManager::set_pseudo_locale(PseudoLocale::Off);
}

tilo_test_env::scenario! {
    fn lookups_resolve_fall_back_and_degrade_to_copy_that_works;

    phase a_message_only_a_value_completes_is_not_said_without_one;
    // A message a value completes is said with it, and is not said at all where the caller has none:
    // the words a step title can stand on.
    {
        let _restore = RestoreLookupState::capture();
        install_plain(
            "en-US",
            "en-US",
            &[(
                "en-US",
                "sheets-workbook-title = Named the workbook { $new }\nbare = Post a message\n",
            )],
        );
        assert_eq!(
            I18nManager::lookup_complete("sheets-workbook-title", None),
            None,
            "a placeable left unfilled is no sentence"
        );
        let mut args = FluentArgs::new();
        args.set("new", "Q3");
        assert_eq!(
            I18nManager::lookup_complete("sheets-workbook-title", Some(&args)).as_deref(),
            Some("Named the workbook Q3")
        );
        assert_eq!(
            I18nManager::lookup_complete("bare", None).as_deref(),
            Some("Post a message"),
            "a message that names no variable is said as it stands"
        );
        assert_eq!(I18nManager::lookup_complete("no-such-message", None), None);
    }

    phase a_parameter_is_not_titled_by_a_sentence_that_needs_a_value;
    // A parameter's title is looked up by convention (`<kind>-<param>`), a key a history sentence
    // for the field it sets can hold too: a message only a value completes is not its title, and
    // looking it up reports nothing.
    {
        let _restore = RestoreLookupState::capture();
        install_plain(
            "en-US",
            "en-US",
            &[(
                "en-US",
                "sheets-workbook-title = Named the workbook { $new }\n\
                 sheets-workbook-set-title = Name the workbook\n\
                 sheets-workbook-name = Name\n",
            )],
        );
        let word = tilo_resource::label::Word {
            key: "sheets-workbook-set-title",
            source: "Name the workbook",
            catalogue: None,
            arguments: Vec::new(),
        };
        let params = tilo_resource::label::titled(
            &word,
            "sheets.workbook",
            vec![
                tilo_actions::ActionParam::required("title", tilo_actions::ParamType::Text, ""),
                tilo_actions::ActionParam::required("name", tilo_actions::ParamType::Text, ""),
            ],
        );
        let title = |param: &tilo_actions::ActionParam| {
            param
                .schema
                .as_deref()
                .and_then(|schema| schema.get("title"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        assert_eq!(title(&params[0]), None, "a sentence that needs a value is no title");
        assert_eq!(title(&params[1]).as_deref(), Some("Name"));
    }

    phase a_kept_plain_lookup_follows_catalogue_locale_and_pseudo_locale;
    // A plain lookup is resolved once per catalogue, locale and pseudo-localisation, and resolved
    // again as soon as any of them changes.
    {
        let _restore = RestoreLookupState::capture();
        install_plain(
            "en-US",
            "en-US",
            &[
                ("en-US", "kept-probe = One\n"),
                ("de-DE", "kept-probe = Eins\n"),
            ],
        );
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "One");
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "One");
        I18nManager::set_locale("de-DE");
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "Eins");
        I18nManager::add_resource("de-DE", "kept-other = Zwei\n".to_owned());
        assert_eq!(I18nManager::lookup_plain("kept-other"), "Zwei");
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        assert_ne!(I18nManager::lookup_plain("kept-probe"), "Eins");
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "Eins");
        install_plain("en-US", "en-US", &[("en-US", "kept-probe = Other\n")]);
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "Other");
    }

    phase lookup_falls_back_through_language_catalogues_then_source;
    {
        let _restore = RestoreLookupState::capture();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        TOOLKIT_SOURCE.with(|source| assert!(source.is_ok(), "bundled toolkit catalog must parse"));
        assert_eq!(I18nManager::lookup_plain("history-mode-title"), "History");
        assert_eq!(
            I18nManager::lookup_plain("standalone-search-title"),
            "Search"
        );

        type Catalogs = &'static [(&'static str, &'static str)];
        let cases: &[(&str, &str, Catalogs, Catalogs)] = &[
            // The active language answers before the source does, the
            // toolkit's own words included: its German catalogue is part of
            // every manager, so a key the host only has in its source reads
            // in the toolkit's German rather than in English.
            (
                "the active language answers before the source, toolkit words included",
                "de-DE",
                &[
                    (
                        "en-US",
                        "history-mode-title = App history\nstandalone-search-title = App search",
                    ),
                    ("de", "history-mode-title = Verlauf"),
                ],
                &[
                    ("history-mode-title", "Verlauf"),
                    ("standalone-search-title", "Suchen"),
                    ("proposal-accept", "Übernehmen"),
                ],
            ),
            (
                "language match, then source fallback, then the key itself",
                "de-DE",
                &[("en-US", EN), ("de", DE)],
                &[
                    ("greeting", "Guten Tag"),
                    ("only-source", "Source fallback"),
                    ("missing", "missing"),
                ],
            ),
            (
                "the generic language wins over another region",
                "de-AT",
                &[("en-US", EN), ("de-CH", "greeting = Grüezi"), ("de", DE)],
                &[("greeting", "Guten Tag")],
            ),
            (
                "without a generic language, registration order decides",
                "de-AT",
                &[("en-US", EN), ("de-CH", "greeting = Grüezi"), ("de-DE", DE)],
                &[("greeting", "Grüezi")],
            ),
            (
                "every catalogue of the active language is consulted before source",
                "de-DE",
                &[
                    ("en-US", "greeting = Hello\ntool = Tool\napp = App"),
                    ("de-DE", "tool = Werkzeug"),
                    ("de", "app = Anwendung"),
                ],
                &[
                    ("tool", "Werkzeug"),
                    ("app", "Anwendung"),
                    ("greeting", "Hello"),
                ],
            ),
        ];
        for (case, locale, catalogs, expected) in cases {
            install_plain("en-US", locale, catalogs);
            assert_eq!(I18nManager::current_locale(), *locale, "case {case}");
            for (key, want) in *expected {
                assert_eq!(
                    I18nManager::lookup_plain(key),
                    *want,
                    "case {case}: key {key}"
                );
            }
        }
    }

    phase broken_resources_and_messages_degrade_to_one_that_works;
    {
        let _restore = RestoreLookupState::capture();
        I18nManager::install("en-US", "en-US", &[("en-US", EN)]).unwrap();
        let revision = I18nManager::revision();
        let error = I18nManager::install("en-US", "de", &[("en-US", "broken = {"), ("de", DE)])
            .unwrap_err();
        assert!(matches!(error, I18nError::InvalidResource { .. }));
        assert_eq!(I18nManager::revision(), revision);
        assert_eq!(I18nManager::lookup("greeting", None), "Hello");

        install_plain(
            "en-US",
            "de",
            &[
                ("en-US", "share = Shared with { $person }"),
                ("de", "share = Geteilt mit { $persn }"),
            ],
        );
        assert_eq!(
            I18nManager::format("share", &[("person", ArgValue::from("Zoë"))]),
            "Shared with Zoë"
        );
        assert_eq!(I18nManager::format("share", &[]), "Geteilt mit {$persn}");

        const SOURCE: &str =
            "items = { $count ->\n    [one] { $count } item\n   *[other] { $count } items\n}";
        install_plain("en-US", "en-US", &[("en-US", SOURCE)]);
        let items = || I18nManager::format("items", &[("count", ArgValue::from(21))]);
        assert_eq!(items(), "21 items");
        // A window that speaks Russian reads its English fallback under
        // Russian's rules, where 21 is `one`.
        install_plain(
            "en-US",
            "ru-RU",
            &[("en-US", SOURCE), ("ru-RU", "greeting = Здравствуйте")],
        );
        assert_eq!(items(), "21 item");
        I18nManager::set_locale("xx-ZZ");
        assert_eq!(items(), "21 items");
    }
}

tilo_test_env::scenario! {
    fn pseudo_locales_mark_copy_once_and_leave_data_alone;

    phase pseudo_locale_expansion_is_idempotent;
    {
        // Copy that is localised twice must still carry one pair of markers:
        // the doubling a reviewer sees is the string, not the transform
        // (`T3` in the UX audit).
        let once = pseudo_expand("Wi-Fi");
        assert_eq!(pseudo_expand(&once), once);
        assert_eq!(once.matches(PSEUDO_OPEN).count(), 1);
        assert_eq!(once.matches(PSEUDO_CLOSE).count(), 1);
        // A string that merely starts and ends with the markers is left alone
        // rather than wrapped a second time.
        assert_eq!(pseudo_expand("⟦⟧"), "⟦⟧");
    }

    phase a_message_of_localised_fragments_is_not_marked_twice;
    // Copy made of copy already localised keeps the one pair of marks it has: a date heading reads
    // `⟦Tue⟧, ⟦Jun⟧ 30`, never `⟦⟦Tue⟧, ⟦Jun⟧ 30⟧`.
    {
        let _restore = RestoreLookupState::capture();
        install_plain("en-US", "en-US", &[("en-US", EN)]);
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let weekday = I18nManager::lookup("greeting", None);
        let composed =
            I18nManager::format("share", &[("person", ArgValue::from(weekday.as_str()))]);
        assert_eq!(
            composed.matches(PSEUDO_OPEN).count(),
            1,
            "one pair of markers: {composed}"
        );
    }

    phase an_empty_message_stays_empty_under_the_pseudo_locale;
    // Nothing says nothing: an empty message is not two markers, which a review would read as copy
    // that is too long.
    {
        let _restore = RestoreLookupState::capture();
        install_plain("en-US", "en-US", &[("en-US", EN)]);
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        assert_eq!(I18nManager::lookup("", None), "");
        assert_eq!(I18nManager::format("", &[]), "");
    }

    phase pseudo_locales_expand_copy_but_not_data_and_invalidate_widgets;
    {
        let _restore = RestoreLookupState::capture();
        // Expanded pseudo-localisation lengthens text and bumps the revision
        // so retained widgets re-resolve.
        install_plain("en-US", "en-US", &[("en-US", EN)]);
        let initial = I18nManager::revision();
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let expanded = I18nManager::lookup("greeting", None);
        assert!(expanded.starts_with('⟦') && expanded.ends_with('⟧'));
        assert!(expanded.chars().count() > "Hello".chars().count());
        assert_ne!(I18nManager::revision(), initial);
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(I18nManager::lookup("greeting", None), "Hello");

        install_plain(
            "en-US",
            "en-US",
            &[("en-US", "share = Shared with { $person }")],
        );
        let initial = I18nManager::revision();
        I18nManager::set_locale("qps-ploc");
        assert_ne!(I18nManager::revision(), initial);
        let rendered = I18nManager::format("share", &[("person", ArgValue::from("Zoë <a>"))]);
        assert!(
            rendered.starts_with('⟦') && rendered.ends_with('⟧'),
            "{rendered}"
        );
        assert!(rendered.contains("Zoë <a>"), "{rendered}");
        assert!(!rendered.contains("Shared"), "{rendered}");
    }
}

static EMBEDDED: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "embedded-probe = Probe\nembedded-greeting = Hello { $name }",
    )],
);

fn parsed_locales() -> Vec<String> {
    MANAGER.with(|manager| {
        let mut locales: Vec<String> = manager
            .borrow()
            .as_ref()
            .map(|manager| manager.bundles.keys().map(ToString::to_string).collect())
            .unwrap_or_default();
        locales.sort();
        locales
    })
}

tilo_test_env::scenario! {
    fn relative_times_are_said_in_the_host_language;

    phase relative_times_follow_the_host_locale_and_its_plural_rules;
    {
        use tilo_ui_base::format::relative;
        use crate::time::{Duration, SystemTime};
        use crate::widget::format_relative_time;
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago = |secs| format_relative_time(base, base + Duration::from_secs(secs));

        {
            let _restore = RestoreLookupState::capture();
            assert_eq!(ago(10), "just now");
            assert_eq!(ago(120), "2 min ago");
            assert_eq!(manager_id(), None);
        }

        {
            let _restore = RestoreLookupState::capture();
            install_plain("en-US", "de-DE", &[("en-US", EN), ("de-DE", DE)]);
            assert_eq!(ago(10), "gerade eben");
            assert_eq!(ago(120), "vor 2 Min.");
            assert_eq!(ago(2 * 3600 + 5), "vor 2 Std.");
            assert_eq!(ago(86_400), "gestern");
            assert_eq!(ago(259_200), "vor 3 Tagen");

            assert_eq!(relative(0), "gerade eben");
            assert_eq!(relative(90_000), "gestern");
            assert_eq!(relative(3 * 86_400), "vor 3 Tagen");
            assert_eq!(relative(8 * 86_400), "vor 1 Woche");
            assert_eq!(relative(90 * 86_400), "vor 3 Monaten");
            assert_eq!(relative(400 * 86_400), "vor 1 Jahr");
        }
    }

    phase relative_time_messages_are_translated;
    // A toolkit key only English defines would show English on a German shell; every relative-time
    // message is translated with the same variables.
    {
        const EN_FTL: &str = include_str!("../../../ui-base/locales/en-US.ftl");
        const DE_FTL: &str = include_str!("../../../ui-base/locales/de-DE.ftl");
        let en = make_bundle(&parse_locale_or_default("en-US"), EN_FTL).unwrap();
        let de = make_bundle(&parse_locale_or_default("de-DE"), DE_FTL).unwrap();
        for key in [
            "relative-ago-just-now",
            "relative-ago-minutes",
            "relative-ago-hours",
            "relative-ago-yesterday",
            "relative-ago-days",
            "relative-ago-weeks",
            "relative-ago-months",
            "relative-ago-years",
            "member-shortcut",
        ] {
            let mut args = FluentArgs::new();
            args.set("count", FluentValue::from(2));
            args.set("name", FluentValue::from("scribe"));
            args.set("person", FluentValue::from("Ada"));
            for (locale, bundle) in [("en-US", &en), ("de-DE", &de)] {
                let (value, clean) = tilo_history::format_message(bundle, key, Some(&args))
                    .unwrap_or_else(|| panic!("{locale}.ftl lacks {key}"));
                assert!(clean, "{locale}.ftl formats {key} with errors: {value}");
            }
        }
    }
}
