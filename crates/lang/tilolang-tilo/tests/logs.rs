//! Logs end to end: the window over the device's `journal` reading, the shared filter, the
//! live tail and the export.

use crate::fixtures::Pane;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};

const SOURCE: &str = include_str!("../../../../apps/tl/logs/app.tl");

/// What this device's journal publishes: three records over two boots, an error among them.
const READING: &str = r#"{
  "serving": true,
  "read_error": null,
  "problem": null,
  "entries": [
    {"realtime_us": 1760000000000000, "unit": "kernel", "identifier": "kernel", "priority": 6,
     "message": "Linux version 7.0.9", "pid": null, "hostname": "tilo-station",
     "boot_id": "boot-a", "transport": "kernel",
     "extra": [{"name": "CODE_FILE", "value": "init/main.c"}],
     "time": "09:00:00.000", "timestamp": "2026-11-24 09:00:00.000"},
    {"realtime_us": 1760000001000000, "unit": "shell.service", "identifier": "shell", "priority": 3,
     "message": "renderer stalled 2 frames", "pid": 820, "hostname": "tilo-station",
     "boot_id": "boot-a", "transport": "stdout",
     "extra": [{"name": "CODE_FILE", "value": "crates/desktop/session/src/render/frame.rs"}],
     "time": "09:00:01.000", "timestamp": "2026-11-24 09:00:01.000"},
    {"realtime_us": 1760000002000000, "unit": "sshd.service", "identifier": "sshd", "priority": 4,
     "message": "Invalid user admin from 203.0.113.7", "pid": 1022, "hostname": "tilo-station",
     "boot_id": "boot-b", "transport": "syslog", "extra": [],
     "time": "09:00:02.000", "timestamp": "2026-11-24 09:00:02.000"}
  ],
  "cursor": "s=a;i=3",
  "following": false,
  "read_at_ms": 1760000000000,
  "pending": []
}"#;

/// A computer whose journal cannot be read.
const MISSING: &str = r#"{
  "serving": false,
  "read_error": "The system journal is not available on this device.",
  "problem": null,
  "entries": [],
  "cursor": "",
  "following": false,
  "read_at_ms": 0,
  "pending": []
}"#;

fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

fn reading() -> ResourceRef {
    ResourceRef::parse_id("journal", "so.tilo.device#Journal").expect("a resource id")
}

fn installed(reading_json: &str) -> Pane {
    Pane::install(&me(), SOURCE)
        .delegating()
        .reading(&reading(), reading_json)
        .rooted("Logs")
}

/// Every text, title, badge and label under `node`, in order.
fn words(node: &UiNode, into: &mut Vec<String>) {
    for key in [
        "value", "title", "text", "label", "action", "primary", "message",
    ] {
        if let Some(Value::Text(text)) = node.props.get(key) {
            into.push(text.clone());
        }
    }
    for child in &node.children {
        words(child, into);
    }
}

fn shown(view: &ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    words(view.tree(), &mut out);
    out
}

fn has(view: &ViewInstance, wanted: &str) -> bool {
    shown(view).iter().any(|text| text == wanted)
        || shown(view).iter().any(|text| text.contains(wanted))
}

#[test]
fn the_window_lists_the_journal_with_its_severity_and_unit() {
    let app = installed(READING);
    let window = app.window();
    let words = shown(&window);
    for wanted in [
        "Linux version 7.0.9",
        "renderer stalled 2 frames",
        "Invalid user admin from 203.0.113.7",
        "Error",
        "Warning",
        "Info",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
}

#[test]
fn the_filter_hides_records_below_its_severity() {
    let app = installed(READING);
    app.try_call("set_severity", vec![app.root_value(), Value::text("err")])
        .expect("the severity is set");
    let window = app.window();
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text == "renderer stalled 2 frames"),
        "the error shows: {words:?}"
    );
    assert!(
        !words.iter().any(|text| text == "Linux version 7.0.9"),
        "the information is hidden: {words:?}"
    );
}

#[test]
fn the_filter_narrows_to_one_unit() {
    let app = installed(READING);
    app.try_call(
        "set_unit",
        vec![app.root_value(), Value::text("sshd.service")],
    )
    .expect("the unit is set");
    let window = app.window();
    let words = shown(&window);
    assert!(
        words
            .iter()
            .any(|text| text == "Invalid user admin from 203.0.113.7")
    );
    assert!(!words.iter().any(|text| text == "renderer stalled 2 frames"));
}

#[test]
fn the_filter_narrows_to_one_boot() {
    let app = installed(READING);
    app.try_call("set_boot", vec![app.root_value(), Value::text("boot-b")])
        .expect("the boot is set");
    let window = app.window();
    let words = shown(&window);
    assert!(
        words
            .iter()
            .any(|text| text == "Invalid user admin from 203.0.113.7")
    );
    assert!(!words.iter().any(|text| text == "Linux version 7.0.9"));
}

/// The window's search narrows the records: the words searched for in the window's bar, with no
/// search field of the app's own.
#[test]
fn the_search_narrows_the_records() {
    fn shell(node: &UiNode) -> Option<String> {
        if node.kind == "ui.NavShell" {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(shell)
    }
    fn fields(node: &UiNode) -> usize {
        let searching = node.kind == "ui.TextInput"
            && matches!(node.props.get("placeholder"), Some(Value::Text(text)) if text.starts_with("Search"));
        usize::from(searching) + node.children.iter().map(fields).sum::<usize>()
    }
    let app = installed(READING);
    let mut window = app.window();
    assert_eq!(
        fields(window.tree()),
        0,
        "the window draws no search field of its own"
    );
    let shell = shell(window.tree()).expect("the window is a shell");
    window
        .fire_with(&shell, "search", Some(&Value::text("stalled")))
        .expect("the window's search reaches the app");
    window.poll_patches().unwrap();
    let words = shown(&window);
    assert!(words.iter().any(|text| text == "renderer stalled 2 frames"));
    assert!(
        !words
            .iter()
            .any(|text| text == "Invalid user admin from 203.0.113.7")
    );
}

#[test]
fn clearing_the_filters_shows_every_record() {
    let app = installed(READING);
    app.try_call("set_severity", vec![app.root_value(), Value::text("err")])
        .unwrap();
    app.try_call("clear_filters", vec![app.root_value()])
        .expect("the filters are cleared");
    let window = app.window();
    assert!(has(&window, "Linux version 7.0.9"));
}

#[test]
fn a_refresh_reads_the_most_recent_records() {
    let app = installed(READING);
    app.try_call("refresh", vec![app.root_value()])
        .expect("the read starts");
    let asked = app.sent.asked("refresh");
    match &asked[0] {
        Value::Ref(r) => assert_eq!(r.target.as_ref(), &reading(), "the verb is the reading's"),
        other => panic!("the verb's resource is a reference, not {other:?}"),
    }
    assert_eq!(asked[1], Value::Int(500));
}

#[test]
fn following_reads_the_records_after_the_cursor() {
    let app = installed(READING);
    app.try_call("follow", vec![app.root_value(), Value::Bool(true)])
        .expect("following starts");
    assert_eq!(
        app.sent.asked("set-following")[1],
        Value::Bool(true),
        "the device follows"
    );
    app.try_call("poll_tail", vec![app.root_value()])
        .expect("a tail read runs");
    let asked = app.sent.asked("read-after");
    assert_eq!(
        asked[1],
        Value::text("s=a;i=3"),
        "it reads after the cursor"
    );
    assert_eq!(asked[2], Value::Int(200));
}

#[test]
fn the_export_writes_one_json_object_a_line() {
    let app = installed(READING);
    let made = app
        .try_call("made_export", vec![app.root_value()])
        .expect("the export is made");
    let text = match made {
        Value::Text(text) => text,
        other => panic!("an export is text, not {other:?}"),
    };
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(
        text.contains(r#""message":"renderer stalled 2 frames""#),
        "{text}"
    );
    assert!(text.contains(r#""unit":"shell.service""#), "{text}");
    assert!(text.contains(r#""priority":3"#), "{text}");
}

#[test]
fn the_records_detail_shows_its_fields() {
    let app = installed(READING);
    app.try_call(
        "choose",
        vec![app.root_value(), Value::text("1760000000000000:kernel")],
    )
    .expect("the kernel's record is chosen");
    let window = app.window();
    let words = shown(&window);
    for wanted in [
        "Linux version 7.0.9",
        "kernel",
        "CODE_FILE",
        "init/main.c",
        "tilo-station",
        "boot-a",
    ] {
        assert!(
            words.iter().any(|text| text == wanted),
            "{wanted} shows: {words:?}"
        );
    }
}
#[test]
fn the_severity_filter_offers_the_threshold_it_keeps() {
    let app = installed(READING);
    let window = app.window();
    let (options, values) = {
        let severity = of_kind(window.tree(), "ui.Dropdown")
            .into_iter()
            .find(|node| node.props.get("caption") == Some(&Value::text("Severity")))
            .expect("the severity filter is a dropdown");
        let Some(Value::List(options)) = severity.props.get("options") else {
            panic!("the filter offers its options: {:?}", severity.props);
        };
        let Some(Value::List(values)) = severity.props.get("values") else {
            panic!("each option stands for its level: {:?}", severity.props);
        };
        (options.clone(), values.clone())
    };
    let words: Vec<&str> = options
        .iter()
        .filter_map(Value::as_text)
        .collect::<Vec<_>>();
    assert_eq!(
        words,
        [
            "Emergency only",
            "Alert and above",
            "Critical and above",
            "Error and above",
            "Warning and above",
            "Notice and above",
            "Info and above",
            "All levels",
        ],
        "each option says what it keeps, not the level's key"
    );
    // The words stand for the levels the filter reads: choosing one sets its level, and the
    // records below it go.
    assert_eq!(
        values.iter().filter_map(Value::as_text).collect::<Vec<_>>(),
        [
            "emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"
        ],
        "the option for errors sets `err`"
    );
    // Choosing the option for errors sets `err`: the records below it go.
    app.try_call("set_severity", vec![app.root_value(), Value::text("err")])
        .expect("the severity is set");
    let window = app.window();
    let words = shown(&window);
    assert!(
        words.iter().any(|text| text == "renderer stalled 2 frames"),
        "the error stays: {words:?}"
    );
    assert!(
        !words.iter().any(|text| text == "Linux version 7.0.9"),
        "the information it keeps above goes: {words:?}"
    );
}

#[test]
fn the_list_names_a_service_by_its_own_name() {
    let app = installed(READING);
    let window = app.window();
    let words = shown(&window);
    assert!(
        words
            .iter()
            .any(|text| text == "09:00:01.000 · shell · shell"),
        "the row names the service short: {words:?}"
    );
    assert!(
        words.iter().any(|text| text == "shell.service"),
        "the record's detail keeps the unit's whole name: {words:?}"
    );
}
#[test]
fn a_computer_without_a_readable_journal_says_so() {
    let app = installed(MISSING);
    let window = app.window();
    let words = shown(&window);
    assert!(
        words
            .iter()
            .any(|text| text == "The journal is not available"),
        "{words:?}"
    );
    assert!(
        words
            .iter()
            .any(|text| text == "The system journal is not available on this device."),
        "{words:?}"
    );
}

/// How many times `wanted` is shown.
fn count(view: &ViewInstance, wanted: &str) -> usize {
    shown(view)
        .iter()
        .filter(|text| text.as_str() == wanted)
        .count()
}

/// The nodes of `kind`, in order.
fn of_kind<'a>(tree: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    fn walk<'a>(node: &'a UiNode, kind: &str, found: &mut Vec<&'a UiNode>) {
        if node.kind == kind {
            found.push(node);
        }
        for child in &node.children {
            walk(child, kind, found);
        }
    }
    let mut found = Vec::new();
    walk(tree, kind, &mut found);
    found
}

/// The first button under `node` whose words satisfy `wanted`.
fn button<'a>(node: &'a UiNode, wanted: &dyn Fn(&str) -> bool) -> Option<&'a UiNode> {
    if node.kind == "ui.Button" {
        let mut said = Vec::new();
        words(node, &mut said);
        if said.iter().any(|text| wanted(text)) {
            return Some(node);
        }
    }
    node.children.iter().find_map(|child| button(child, wanted))
}

#[test]
fn selection_auto_lands_on_errors_and_moves_through_the_document() {
    let app = installed(READING);
    // The list shows each record once; the inspector's record is shown a second time.
    let window = app.window();
    assert_eq!(
        count(&window, "renderer stalled 2 frames"),
        2,
        "the newest error is what the inspector lands on: {:?}",
        shown(&window)
    );
    // Choosing another record writes it to the shared document.
    let chosen = window
        .tree()
        .children
        .iter()
        .find_map(|node| button(node, &|text| text == "Show this record"))
        .expect("a row's way into its detail");
    window
        .fire(&chosen.key, "press")
        .expect("the record is chosen");
    let selected = app
        .host
        .runtime()
        .read(app.root(), "selected")
        .expect("the selection reads");
    assert_ne!(selected, Value::text(""), "a record is written as chosen");
    // A second window shows the same record: the selection is the document's, not a window's.
    let second = app.window();
    assert_eq!(
        count(&second, "Linux version 7.0.9"),
        2,
        "the chosen record is the inspector's in every window: {:?}",
        shown(&second)
    );
    assert_eq!(count(&second, "renderer stalled 2 frames"), 1);
}

#[test]
fn the_live_tail_reads_in_the_window_not_the_workspaces_origin() {
    let app = installed(READING);
    assert!(
        of_kind(app.window().tree(), "ui.Timer").is_empty(),
        "a window that follows nothing asks for nothing"
    );
    app.try_call("follow", vec![app.root_value(), Value::Bool(true)])
        .expect("the journal is followed");
    let window = app.window();
    let timers = of_kind(window.tree(), "ui.Timer");
    assert_eq!(timers.len(), 1, "the window carries the tail's clock");
    window
        .fire(&timers[0].key, "tick")
        .expect("the tail's clock strikes");
    let read_after = app.sent.asked("read-after");
    let Value::Ref(on) = &read_after[0] else {
        panic!("the verb is called on a resource: {read_after:?}")
    };
    assert_eq!(
        &*on.target,
        &reading(),
        "the window's own device reads its journal"
    );
}
