use std::collections::BTreeMap;

use serde_json::json;
use tilo_automation::{Binding, Selector, StepKind};

use super::Recorded;
use super::{BROWSER_APP, FIND_ELEMENT, NOTHING_DONE, draft, is_ref, summary};

fn recorded(action: &str, kind: &str, resource: &str) -> Recorded {
    Recorded {
        app: Some(BROWSER_APP.into()),
        provider: None,
        resource: resource.into(),
        kind: kind.into(),
        action: action.into(),
        label: None,
        need: Some("outside".into()),
        arguments: BTreeMap::new(),
        secrets: Vec::new(),
        value: None,
        data: BTreeMap::new(),
        message: None,
        url: None,
    }
}

/// A page fill that reached the element the answer names.
fn fill(resource: &str, element: &str, text: &str) -> Recorded {
    let mut entry = recorded("fill-page", "browser.page", resource);
    entry.arguments = BTreeMap::from([
        ("element".into(), json!(element)),
        ("text".into(), json!(text)),
    ]);
    entry.data = BTreeMap::from([(
        "acted".into(),
        json!({"role": "textbox", "name": "Email", "element": element}),
    )]);
    entry
}

/// A page read that saw the page.
fn read(resource: &str) -> Recorded {
    let mut entry = recorded("read-page-changes", "browser.page", resource);
    entry.need = Some("read".into());
    entry.value = Some(json!({"changed": true}));
    entry
}

#[test]
fn a_step_reaching_an_element_finds_it_first_and_acts_on_the_answer() {
    let entries = [fill("browser.page:1", "d1e5", "ada@example.test")];
    let shortcut = draft("Apply", &entries, &[]).expect("a draft");
    assert_eq!(shortcut.steps.len(), 2);
    let find = &shortcut.steps[0];
    let StepKind::Invoke {
        action,
        resource,
        args,
        ..
    } = &find.kind
    else {
        panic!("the first step finds the element");
    };
    assert_eq!(action.app, BROWSER_APP);
    assert_eq!(action.action, FIND_ELEMENT);
    assert_eq!(resource, &Selector::Fixed("browser.page:1".to_owned()));
    assert_eq!(args["role"], Binding::Literal(json!("textbox")));
    assert_eq!(args["name"], Binding::Literal(json!("Email")));
    let fill = &shortcut.steps[1];
    let StepKind::Invoke {
        action,
        resource,
        args,
        ..
    } = &fill.kind
    else {
        panic!("the second step fills");
    };
    assert_eq!(action.action, "fill-page");
    assert_eq!(
        resource,
        &Selector::FromBinding(Binding::step(find.id.clone(), ""))
    );
    assert_eq!(args["text"], Binding::Literal(json!("ada@example.test")));
    // The old page's ref is not carried: a replay's page has none of them.
    assert!(!args.contains_key("element"));
}

#[test]
fn a_value_equal_to_a_named_input_is_bound_to_it() {
    let entries = [fill("browser.page:1", "d1e5", "ada@example.test")];
    let inputs = [json!({"name": "email", "value": "ada@example.test"})];
    let shortcut = draft("Apply", &entries, &inputs).expect("a draft");
    let StepKind::Invoke { args, .. } = &shortcut.steps[1].kind else {
        panic!("a fill step");
    };
    assert_eq!(args["text"], Binding::Input("email".into()));
    assert_eq!(shortcut.inputs.len(), 1);
    assert_eq!(shortcut.inputs[0].name.as_ref(), "email");
}

#[test]
fn one_element_filled_twice_is_found_once() {
    let entries = [
        fill("browser.page:1", "d1e5", "ada@example.test"),
        fill("browser.page:1", "d1e5", "ada@example.test"),
    ];
    let shortcut = draft("Apply", &entries, &[]).expect("a draft");
    assert_eq!(shortcut.steps.len(), 3);
    let found = shortcut.steps[0].id.clone();
    for step in &shortcut.steps[1..] {
        let StepKind::Invoke { resource, .. } = &step.kind else {
            panic!("a fill step");
        };
        assert_eq!(
            resource,
            &Selector::FromBinding(Binding::step(found.clone(), ""))
        );
    }
}

#[test]
fn what_was_only_looked_at_is_dropped() {
    let entries = [read("browser.page:1"), fill("browser.page:1", "d1e5", "x")];
    let shortcut = draft("Apply", &entries, &[]).expect("a draft");
    let actions: Vec<&str> = shortcut
        .steps
        .iter()
        .filter_map(|step| match &step.kind {
            StepKind::Invoke { action, .. } => Some(action.action.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(actions, [FIND_ELEMENT, "fill-page"]);
}

#[test]
fn the_run_ends_on_the_last_look_at_where_it_got_to() {
    let entries = [fill("browser.page:1", "d1e5", "x"), read("browser.page:2")];
    let shortcut = draft("Apply", &entries, &[]).expect("a draft");
    let last = shortcut.steps.last().expect("a last step");
    let StepKind::Invoke { action, .. } = &last.kind else {
        panic!("a page read");
    };
    assert_eq!(action.action, "read-page-changes");
}

#[test]
fn a_run_that_changed_nothing_is_no_shortcut() {
    let entries = [read("browser.page:1")];
    assert_eq!(draft("Apply", &entries, &[]).unwrap_err(), NOTHING_DONE);
    assert_eq!(draft("Apply", &[], &[]).unwrap_err(), NOTHING_DONE);
}

#[test]
fn a_draft_claims_no_authority_of_its_own() {
    let entries = [fill("browser.page:1", "d1e5", "x")];
    let shortcut = draft("Apply", &entries, &[]).expect("a draft");
    assert!(shortcut.grant.is_empty());
    assert!(shortcut.triggers.is_empty());
    assert_eq!(shortcut.saved_by, None);
    assert!(shortcut.id.is_empty(), "Shortcuts names the draft");
}

#[test]
fn an_action_on_no_element_keeps_the_resource_it_ran_on() {
    let mut entry = recorded("open", "notes.note", "notes.note:1");
    entry.app = Some("so.tilo.notes".into());
    entry.arguments = BTreeMap::from([("path".into(), json!("Inbox"))]);
    let shortcut = draft("Open", &[entry], &[]).expect("a draft");
    assert_eq!(shortcut.steps.len(), 1);
    let StepKind::Invoke {
        action, resource, ..
    } = &shortcut.steps[0].kind
    else {
        panic!("an invoke step");
    };
    assert_eq!(action.app, "so.tilo.notes");
    assert_eq!(action.kind, "notes.note");
    assert_eq!(resource, &Selector::Fixed("notes.note:1".to_owned()));
}

#[test]
fn an_element_read_in_a_frame_is_found_in_that_frame() {
    let mut entry = fill("browser.page:1", "d2e5", "ada@example.test");
    entry.arguments.insert("frame".into(), json!("1"));
    entry.data.insert(
        "acted".into(),
        json!({"role": "textbox", "name": "Email", "frame": "1", "element": "d2e5"}),
    );
    let shortcut = draft("Apply", &[entry], &[]).expect("a draft");
    let StepKind::Invoke { args, .. } = &shortcut.steps[0].kind else {
        panic!("a find step");
    };
    assert_eq!(args["frame"], Binding::Literal(json!("1")));
    // The fill's own frame is the find's: it acted on what the find answered.
    let StepKind::Invoke {
        args: fill_args, ..
    } = &shortcut.steps[1].kind
    else {
        panic!("a fill step");
    };
    assert_eq!(fill_args["frame"], Binding::Literal(json!("1")));
}

#[test]
fn a_ref_is_read_as_the_outline_issued_it() {
    for ref_ in ["d1e5", "d12e345", "d1e1"] {
        assert!(is_ref(ref_), "{ref_}");
    }
    for not in [
        "",
        "Email",
        "e5",
        "d1",
        "d1x5",
        "ada@example.test",
        "d1e1234567",
    ] {
        assert!(!is_ref(not), "{not}");
    }
}

#[test]
fn the_draft_reads_as_what_it_will_do() {
    let entries = [fill("browser.page:1", "d1e5", "x")];
    assert_eq!(summary(&entries), "fill-page `Email` (textbox)");
}
