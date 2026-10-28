use std::collections::BTreeMap;

use serde_json::{Value, json};
use tilo_actions::params::{ActionParam, ParamType};

use super::{Recorded, Recorder, SECRET_HELD, compact, recorded_arguments};

fn param(name: &str, ty: ParamType) -> ActionParam {
    ActionParam::required_owned(name.to_owned(), ty, name.to_owned())
}

fn entry(action: &str) -> Recorded {
    Recorded {
        app: Some("so.example".into()),
        provider: None,
        resource: "example.thing:1".into(),
        kind: "example.thing".into(),
        action: action.into(),
        label: None,
        need: Some("undoable changes".into()),
        arguments: BTreeMap::new(),
        secrets: Vec::new(),
        value: None,
        data: BTreeMap::new(),
        message: None,
        url: None,
    }
}

#[test]
fn the_log_keeps_what_was_done_in_order() {
    let recorder = Recorder::new();
    for action in ["open", "rename", "read_content"] {
        recorder.record(entry(action));
    }
    let actions: Vec<String> = recorder
        .entries()
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert_eq!(actions, ["open", "rename", "read_content"]);
    assert_eq!(recorder.len(), 3);
}

#[test]
fn the_log_drops_the_oldest_step_when_it_is_full() {
    let recorder = Recorder::new();
    for index in 0..=super::MAX_RECORDED {
        let mut one = entry("open");
        one.message = Some(index.to_string());
        recorder.record(one);
    }
    let kept = recorder.entries();
    assert_eq!(kept.len(), super::MAX_RECORDED);
    assert_eq!(kept[0].message.as_deref(), Some("1"));
}

#[test]
fn a_secret_parameter_is_kept_as_a_placeholder_never_its_value() {
    let params = [
        param("element", ParamType::Text),
        param("text", ParamType::Secret),
    ];
    let arguments = serde_json::Map::from_iter([
        ("element".to_owned(), json!("d1e5")),
        ("text".to_owned(), json!("hunter2")),
    ]);
    let (kept, secrets) = recorded_arguments(&params, &arguments);
    assert_eq!(kept["element"], json!("d1e5"));
    assert_eq!(kept["text"], json!(SECRET_HELD));
    assert!(!kept.values().any(|value| value == &json!("hunter2")));
    assert_eq!(secrets, ["text"]);
}

#[test]
fn an_absent_argument_is_not_recorded() {
    let params = [param("frame", ParamType::Text)];
    let arguments = serde_json::Map::from_iter([("frame".to_owned(), Value::Null)]);
    let (kept, secrets) = recorded_arguments(&params, &arguments);
    assert!(kept.is_empty());
    assert!(secrets.is_empty());
}

#[test]
fn a_long_answer_is_kept_whole_up_to_the_limit_and_counted_after_it() {
    let short = "a".repeat(super::MAX_RECORDED_TEXT);
    assert_eq!(compact(&json!(short.clone())), json!(short));
    let long = "a".repeat(super::MAX_RECORDED_TEXT + 10);
    let kept = compact(&json!(long));
    assert_eq!(kept["cut"], json!(true));
    assert_eq!(kept["characters"], json!(super::MAX_RECORDED_TEXT + 10));
    assert_eq!(
        kept["text"].as_str().map(str::len),
        Some(super::MAX_RECORDED_TEXT)
    );
}

#[test]
fn a_long_list_is_kept_to_its_first_members() {
    let items: Vec<Value> = (0..super::MAX_RECORDED_ITEMS + 5)
        .map(|index| json!(index))
        .collect();
    let kept = compact(&Value::Array(items));
    assert_eq!(
        kept.as_array().map(Vec::len),
        Some(super::MAX_RECORDED_ITEMS)
    );
}

#[test]
fn the_acted_on_element_is_the_description_a_find_accepts() {
    let mut recorded = entry("fill-page");
    recorded.data = BTreeMap::from([(
        "acted".into(),
        json!({"role": "textbox", "name": "Email", "nth": 0, "frame": "1", "element": "d1e5"}),
    )]);
    assert_eq!(
        recorded.element(),
        Some(json!({"role": "textbox", "name": "Email", "nth": 0, "frame": "1"}))
    );
}

#[test]
fn a_find_answer_names_the_element_it_found() {
    let mut recorded = entry("find-page-element");
    recorded.data = BTreeMap::from([
        ("role".into(), json!("button")),
        ("name".into(), json!("Buy")),
        ("element".into(), json!("d1e2")),
    ]);
    assert_eq!(
        recorded.element(),
        Some(json!({"role": "button", "name": "Buy"}))
    );
}

#[test]
fn an_answer_that_named_no_element_has_none() {
    let recorded = entry("open");
    assert_eq!(recorded.element(), None);
}
