//! A German window says everything in German: every word the editor paints or
//! speaks is a catalogue message, so no label keeps the source language.
//!
//! The editor is painted rather than built from widgets, so what it says
//! reaches a person through the accessibility tree and through the paint calls
//! alike. This walks the whole window in German — both panes' rows, all ten
//! inspector sections, the export sheet — and fails on a word that still reads
//! in the catalogue's source language.

use ui::accesskit::Role;
use video_editor::resource::CATALOG;

use super::hosted::{Editor, chord, nodes, sample_editor};

/// Every (source, translation) pair in the app's two catalogues, for the
/// messages whose two languages differ and whose words stand on their own.
///
/// A message with a variable is left out — its painted shape depends on the
/// values, so it is checked through the presence checks below instead. A word
/// spelled the same in both languages ("Video", "Audio", "Marker") is not
/// English either, so the pair merely leaves it alone.
fn translated_messages() -> Vec<(String, String)> {
    let source = parse(include_str!("../../locales/en-US.ftl"));
    let german = parse(include_str!("../../locales/de-DE.ftl"));
    source
        .into_iter()
        .filter_map(|(key, english)| {
            let german = german.iter().find(|(k, _)| *k == key)?.1.clone();
            let standalone = !english.contains('{') && !german.contains('{');
            (standalone && english != german).then_some((english, german))
        })
        .collect()
}

/// The `key = value` pairs of one Fluent file, values as a person reads them.
fn parse(catalogue: &'static str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in catalogue.lines() {
        let line = line.trim_end();
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            if let Some((_, value)) = current.as_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some(pair) = current.take() {
            pairs.push(pair);
        }
        if let Some((key, value)) = line.split_once('=') {
            current = Some((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    if let Some(pair) = current {
        pairs.push(pair);
    }
    pairs
}

/// The editor with the sample project open and a clip selected, so every
/// section holding something is listed.
fn german_editor() -> Editor {
    CATALOG.set_locale("de-DE");
    let mut app = sample_editor();
    app.harness().click_named("video_editor_root").unwrap();
    let (clip, _) = nodes(&mut app, |n| {
        n.role() == Role::ListItem
            && n.label() == Some("Drone_Coastline.mov")
            && n.value().is_some_and(|v| v.contains(" auf "))
    })
    .into_iter()
    .next()
    .expect("the sample project places Drone_Coastline.mov on a lane");
    app.harness()
        .perform_a11y_action(clip, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    app
}

/// Every word the window says: its nodes' names, values and descriptions.
fn spoken(app: &mut Editor) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .flat_map(|(_, node)| {
            [
                node.label().map(ToString::to_string),
                node.value().map(ToString::to_string),
                node.description().map(ToString::to_string),
            ]
        })
        .flatten()
        .collect()
}

#[test]
fn no_word_of_the_window_is_left_in_english() {
    let pairs = translated_messages();
    assert!(
        pairs.len() > 100,
        "the catalogues carry {} translated messages",
        pairs.len()
    );
    let mut app = german_editor();
    let said = spoken(&mut app);
    let roles: Vec<String> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .map(|(_, n)| format!("{:?} {:?} {:?}", n.role(), n.label(), n.value()))
        .collect();
    // A label reads in English when it is one of the source catalogue's words
    // and no German message reads the same: "Marker" is the German word for a
    // section while also being an English one elsewhere.
    let german_words: Vec<&str> = pairs.iter().map(|(_, german)| german.as_str()).collect();
    let mut english: Vec<&str> = said
        .iter()
        .filter(|words| !german_words.contains(&words.as_str()))
        .filter_map(|words| {
            pairs
                .iter()
                .find(|(source, _)| source == words)
                .map(|(source, _)| source.as_str())
        })
        .collect();
    english.sort_unstable();
    english.dedup();
    CATALOG.set_locale("en-US");
    if !english.is_empty() {
        let context: Vec<&String> = roles
            .iter()
            .filter(|line| english.iter().any(|word| line.contains(word)))
            .collect();
        CATALOG.set_locale("en-US");
        panic!("a German window says these in English: {english:?} in {context:#?}");
    }
}

#[test]
fn the_window_names_its_panes_sections_and_controls_in_german() {
    let mut app = german_editor();
    let said = spoken(&mut app);
    let says = |words: &str| said.iter().any(|line| line.contains(words));
    for words in [
        // The panes.
        "Medienpool",
        "Wiedergabe",
        "Zeitleiste",
        "Inspektor",
        // The transport buttons.
        "Ein Bild zurück",
        "Ein Bild vor",
        "Zum Anfang",
        "Zum Ende",
        // The inspector's sections.
        "Farbkurven",
        "Transformieren",
        "Tempo",
        "Pegel",
        "Freisteller",
        "Maske",
        "Effekte",
        "Marker",
        "Projekt",
        // The controls at the head of the panel, and inside those sections.
        "Deckkraft",
        "Helligkeit",
        "Kontrast",
        "Sättigung",
        "Einblenden",
        "Ausblenden",
        "Drehung",
        "Skalierung X",
        // What a clip says about itself: its span in words, not "to".
        " lang, auf ",
    ] {
        assert!(says(words), "the German window says {words:?}: {said:#?}");
    }
    CATALOG.set_locale("en-US");
}

#[test]
fn the_export_sheet_reads_in_german() {
    CATALOG.set_locale("de-DE");
    let mut app = super::hosted::sample_editor();
    app.harness().click_named("video_editor_root").unwrap();
    chord(&mut app, "ctrl", "e");
    app.settle();
    let said = spoken(&mut app);
    for words in [
        "Video exportieren",
        "Export starten",
        "Export schließen",
        "Ausgabe",
    ] {
        assert!(
            said.iter().any(|line| line.contains(words)),
            "the export sheet says {words:?}: {said:#?}"
        );
    }
    CATALOG.set_locale("en-US");
}

/// The states only some projects show — a source whose bytes are elsewhere,
/// one whose bytes are not media, a folded header's summary — are each their
/// own message, so each is checked where it is written.
#[test]
fn the_states_only_some_projects_show_read_in_german() {
    CATALOG.set_locale("de-DE");
    for (message, german) in [
        ("video-editor-media-not-local", "Nicht auf diesem Gerät"),
        ("video-editor-media-not-local-short", "Nicht auf Gerät"),
        ("video-editor-media-unreadable", "Medien unlesbar"),
        ("video-editor-media-unreadable-short", "Unlesbar"),
        ("video-editor-summary-cropped", "beschnitten"),
        ("video-editor-summary-animated", "animiert"),
        ("video-editor-summary-collapsed", "eingeklappt"),
        ("video-editor-inspector-empty", "Kein Clip ausgewählt"),
        (
            "video-editor-preview-empty",
            "Kein Clip unter dem Abspielkopf",
        ),
        ("video-editor-import-none", "Keine Medien importiert"),
        ("video-editor-marker-delete", "Löschen"),
        ("video-editor-transport-jump-start", "Zum Anfang"),
        ("video-editor-field-track-gain", "Pegel"),
    ] {
        assert_eq!(CATALOG.t(message), german, "{message}");
    }
    let unreadable = CATALOG.t_args(
        "video-editor-probe-not-media",
        &[(
            "detected",
            ui::i18n::ArgValue::Text("ein PDF-Dokument".to_owned()),
        )],
    );
    assert_eq!(
        unreadable,
        "ist keine Mediendatei, sondern ein PDF-Dokument"
    );
    CATALOG.set_locale("en-US");
}
