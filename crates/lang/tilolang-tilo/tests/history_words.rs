//! History says what an action did in the words its app declares for it (`did:`), read with the
//! step's own arguments and in the reader's language; an action that declares none reads by its
//! label and the resource it changed, never by an id.

use crate::fixtures::Pane;
use tilolang_runtime::{Actor, Value};

const LAMP: &str = r#"
app so.tilo.lang.lamp "Lamp" (icon: .sun, category: .utility)

/// How warm a lamp's light is.
enum Tint {
    warm
    cold
}

/// A lamp people turn on and off.
type Lamp {
    on: Bool
    mode: Text
    tint: Option<Tint>
}

root Lamp

default view lamp for Lamp

/// The lamp's window.
view lamp(l: Lamp) {
    std.page(l) {
        ui.Text("Lamp")
    }
}

/// Turns the lamp on or off.
action switch(l: Lamp, on: Bool) (
    label: "Switch the lamp",
    did: if on { "turned the lamp on" } else { "turned the lamp off" },
) {
    l.on = on
}

/// Sets how the lamp shines.
action shine(l: Lamp, mode: Text, note: Text) (label: "Set the light", did: "set the light to {mode}") {
    l.mode = mode
}

/// Makes the light warm or cold.
action tint(l: Lamp, tint: Tint) (
    label: "Tint the light",
    did: match tint { .warm => "made the light warm", .cold => "made the light cold" },
) {
    l.tint = .some(tint)
}

/// Turns the lamp on, dimmed.
action dim(l: Lamp) (label: "Dim the lamp") {
    l.mode = "dim"
    l.on = true
}
"#;

/// The ids the German words are keyed by.
const IDS: &str = "switch@Lamp = swtchlmp\nshine@Lamp = shinelmp\ntint@Lamp = tintlamp\n";

/// The app's German, as a translator writes it beside the source: its own selectors.
const GERMAN: &str = "
d-swtchlmp = Lampe schalten
    .did = { $on ->
        [true] schaltete die Lampe ein
       *[other] schaltete die Lampe aus
    }

d-shinelmp = Licht einstellen
    .did = stellte das Licht auf { $mode }

d-tintlamp = Licht tönen
    .did = { $tint ->
        [warm] machte das Licht warm
       *[other] machte das Licht kalt
    }
";

fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The lamp, switched on and off, set bright, made cold, and dimmed.
fn used() -> Pane {
    let app = Pane::install_with_ids(&ada(), LAMP, IDS).rooted("Lamp");
    let lamp = app.root_value();
    app.call("switch", vec![lamp.clone(), Value::Bool(true)]);
    app.call("switch", vec![lamp.clone(), Value::Bool(false)]);
    app.call(
        "shine",
        vec![
            lamp.clone(),
            Value::Text("bright".into()),
            Value::Text("the code is 4417".into()),
        ],
    );
    let meta = app
        .host
        .runtime()
        .meta_of("so.tilo.lang.lamp")
        .expect("the lamp's package");
    let tint = meta
        .enums
        .iter()
        .find(|en| en.name == "Tint")
        .expect("the Tint enum");
    let cold = tint
        .variants
        .iter()
        .find(|case| case.name == "cold")
        .expect("its cold case");
    let cold = tilolang_runtime::value::EnumValue {
        ty: tint.id.clone(),
        variant: cold.id.clone(),
        fields: std::collections::BTreeMap::new(),
    };
    app.call("tint", vec![lamp.clone(), Value::Enum(cold)]);
    app.call("dim", vec![lamp]);
    app
}

/// The sentences after the one that made the lamp, oldest first.
fn said(app: &Pane, locale: &str, translation: &str) -> Vec<String> {
    let mut said = app.history(app.root(), locale, translation);
    assert!(
        said.len() > 5,
        "the lamp's history lists what made it and the five changes: {said:?}"
    );
    said.split_off(said.len().saturating_sub(5))
}

#[test]
fn history_says_each_change_in_the_words_its_action_declares_with_its_arguments() {
    let app = used();
    let english = said(&app, "en-US", "");
    let starts = [
        "Turned the lamp on",
        "Turned the lamp off",
        "Set the light to bright",
        "Made the light cold",
    ];
    for (sentence, wanted) in english.iter().zip(starts) {
        assert!(
            sentence.starts_with(wanted),
            "{wanted:?} is what the change did, the branch its argument picks: {english:?}"
        );
    }

    // A German reader reads the same entries in the app's German, its selectors picking by the
    // same arguments.
    let german = said(&app, "de-DE", GERMAN);
    let starts = [
        "Schaltete die Lampe ein",
        "Schaltete die Lampe aus",
        "Stellte das Licht auf bright",
        "Machte das Licht kalt",
    ];
    for (sentence, wanted) in german.iter().zip(starts) {
        assert!(
            sentence.starts_with(wanted),
            "{wanted:?} in German: {german:?}"
        );
    }
}

#[test]
fn an_action_that_declares_no_words_reads_by_its_label_and_never_by_an_id() {
    let app = used();
    let english = said(&app, "en-US", "");
    let dimmed = english.last().expect("the dim");
    assert_eq!(
        dimmed, "Ran “Dim the lamp” on Lamp",
        "the action's label and the resource it changed: {english:?}"
    );
    let root = app.root().id_string();
    for sentence in &english {
        for id in [root.as_str(), "swtchlmp", "shinelmp", "tintlamp", "$", "{"] {
            assert!(!sentence.contains(id), "no id in history: {sentence:?}");
        }
    }
}

#[test]
fn an_entry_keeps_only_the_arguments_its_words_read() {
    let app = used();
    let entries: Vec<serde_json::Value> = app
        .host
        .runtime()
        .versions(app.root())
        .expect("the versions read")
        .iter()
        .map(|version| {
            tilolang_tilo::versions::version_entry(&app.host, "so.tilo.lang.lamp", version, None)
        })
        .collect();
    let args: Vec<&serde_json::Value> = entries.iter().map(|entry| &entry["args"]).collect();
    let tail = &args[args.len() - 5..];
    assert_eq!(*tail[0], serde_json::json!({ "on": true }));
    assert_eq!(*tail[1], serde_json::json!({ "on": false }));
    // The note the words do not read is never listed beside the change.
    assert_eq!(*tail[2], serde_json::json!({ "mode": "bright" }));
    // A case is named by the name its enum gives it.
    assert_eq!(*tail[3], serde_json::json!({ "tint": "cold" }));
    assert!(
        tail[4].is_null(),
        "an action with no words keeps none: {entries:#?}"
    );
}
