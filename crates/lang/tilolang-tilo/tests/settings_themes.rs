//! Settings' Themes page: the person's own themes, the editor that changes their roles, and the
//! words that lead a search here.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{Actor, Value};

use crate::fixtures::invoker::{Answered as _, Sent};
use crate::fixtures::{control_under, texts};
use crate::settings_panes::{invoke, open_window, settings};

const APP: &str = "so.tilo.settings";

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// One of the person's themes, as the appearance resource keeps it: every role an edit of the
/// editor can carry.
const MINE: &str = r#"{
  "id": "mine",
  "name": "Mine",
  "dark": true,
  "high_contrast": false,
  "hue": 220.0,
  "chroma": 0.02,
  "surfaces": {"base": 0.12, "content": 0.16, "sidebar": 0.14, "inspector": 0.15,
               "raised": 0.2, "overlay": 0.24},
  "ink": {"primary": 0.95, "secondary": 0.75, "tertiary": 0.6},
  "status": {
    "danger": {"lightness": 0.6, "chroma": 0.15},
    "warning": {"lightness": 0.75, "chroma": 0.14},
    "success": {"lightness": 0.7, "chroma": 0.12},
    "info": {"lightness": 0.7, "chroma": 0.1}
  },
  "tint": null,
  "monochrome": false,
  "presence": {"lightness": 0.6, "chroma": 0.1},
  "window_radius": 14.0,
  "density": 1.0,
  "face": "Inter",
  "base_size": 14.0,
  "distinct_letters": null,
  "motion": 1.0,
  "blur": 16.0,
  "tint_opacity": 0.4,
  "frosted_strip": true,
  "rim_span": 0.5,
  "rim_alpha": 0.5,
  "hover_alpha": 0.08,
  "refraction": 0.0,
  "origin": null,
  "findings": []
}"#;

/// The person's appearance: one own theme, shown in dark mode.
fn appearance(host: &tilolang_tilo::host::Host) {
    let value: serde_json::Value = serde_json::from_str(MINE).expect("the theme reads");
    let reading = serde_json::json!({
        "mode": "dark",
        "dark_theme": "custom:mine",
        "light_theme": "tilo-light",
        "themes": [value],
    });
    crate::fixtures::set_reading(host, "settings/appearance", &reading);
}

/// The Themes page opens a theme's editor, and the editor changes the theme's roles through the
/// appearance resource: a role typed and a polarity switched are the same edits an agent's call
/// and the shell's style panel make.
#[test]
fn the_themes_page_edits_a_theme_of_the_persons() {
    let (host, _) = settings();
    appearance(&host);
    invoke(&host, "open_pane", vec![Value::text("themes")]);
    let mut view = open_window(&host);
    let shown = texts(&view);
    for words in ["Your themes", "Mine", "Shown in dark mode"] {
        assert!(
            shown.iter().any(|text| text == words),
            "{words} shows on the Themes page: {shown:?}"
        );
    }

    // The theme's row opens its editor.
    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());
    let row = view
        .tree()
        .of_kind("ui.ListRow")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text("Mine")))
        .unwrap_or_else(|| {
            panic!(
                "the theme's row; the page drew {}",
                crate::fixtures::drawn(view.tree())
            )
        })
        .key
        .clone();
    view.fire_answering(&host, &me(), &row, "activate")
        .expect("the theme's editor opens");
    view.poll_patches().expect("the editor draws");
    let editor = texts(&view);
    for words in [
        "Name and polarity",
        "Surfaces",
        "Primary ink",
        "Window corners",
    ] {
        assert!(
            editor.iter().any(|text| text == words),
            "{words} shows in the editor: {editor:?}"
        );
    }

    // The theme's name is its own field of the appearance, edited where it is kept.
    let name = control_under(view.tree(), "Name", "ui.TextInput");
    view.edit(&name, &Value::text("Harbour"))
        .expect("the name is typed");
    let Invocation::Edit {
        path, field, value, ..
    } = sent.last()
    else {
        panic!("an edit of the theme: {:?}", sent.last());
    };
    assert_eq!(
        path,
        ["themes".to_owned(), "0".to_owned()],
        "the theme's own entry of the person's themes, as the reading stands it"
    );
    assert_eq!(field.as_str(), "name");
    assert_eq!(&value, &Value::text("Harbour"));

    // Its polarity is the appearance's own verb, so every device draws the change.
    let dark = control_under(view.tree(), "Dark", "ui.Toggle");
    view.fire_with_answering(&host, &me(), &dark, "check", Some(&Value::Bool(false)))
        .expect("the polarity is switched");
    let asked = sent.last_args_on("so.tilo.settings.sections", "switch-polarity");
    let dark = asked
        .iter()
        .find(|(named, _)| named == "dark")
        .map(|(_, value)| value.clone());
    assert_eq!(
        dark,
        Some(Value::Opt(Some(Box::new(Value::Bool(false))))),
        "the polarity the person chose: {asked:?}"
    );

    // A slider carries the role it is bound to: the desktop's lightness.
    let desktop = control_under(view.tree(), "Desktop", "ui.Slider");
    view.fire_with_answering(&host, &me(), &desktop, "change", Some(&Value::Float(0.2)))
        .expect("the desktop's lightness moves");
    let asked = sent.last_args_on("so.tilo.settings.sections", "review-theme");
    let role = asked
        .iter()
        .find(|(named, _)| named == "role")
        .map(|(_, value)| value.clone());
    assert_eq!(
        role,
        Some(Value::text("surfaces.base")),
        "the role the slider tunes, as the appearance names it: {asked:?}"
    );
}

/// A search for the words a person knows themes by reaches Settings, in English and in German:
/// the page is Settings' own, so the words are its manifest's and its German catalogue's.
#[test]
fn the_words_a_person_knows_themes_by_lead_to_settings() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let (_, compiled) = settings();
    for word in [
        "theme",
        "colors",
        "appearance",
        "wallpaper",
        "fonts",
        "accent",
        "icons",
    ] {
        assert!(
            compiled.meta.keywords.iter().any(|keyword| keyword == word),
            "Settings is found by {word}: {:?}",
            compiled.meta.keywords
        );
    }

    let german = std::fs::read_to_string(dir.join("locales/de-DE.ftl")).expect("the catalogue");
    let words = tilolang::messages::keywords_word(&german, APP).expect("the German words");
    for word in [
        "design",
        "farben",
        "aussehen",
        "hintergrund",
        "anpassen",
        "schriftarten",
        "farbschema",
    ] {
        assert!(
            words.iter().any(|keyword| keyword == word),
            "Settings is found by {word}: {words:?}"
        );
    }

    // The catalogue the fleet reads carries both, so search finds the app before it is installed.
    let fleet: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("fleet.json")).expect("the fleet entry"),
    )
    .expect("the fleet entry reads");
    let fleet_words: Vec<String> = fleet["keywords"]
        .as_array()
        .expect("the fleet entry's words")
        .iter()
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect();
    for word in ["theme", "wallpaper", "farbschema"] {
        assert!(
            fleet_words.iter().any(|keyword| keyword == word),
            "the fleet knows Settings by {word}: {fleet_words:?}"
        );
    }
}
