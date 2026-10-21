use serde_json::json;
use tilo_automation::{Binding, Step};
use ui::tilo_actions::{ActionParam, ParamType};

use super::*;
use crate::model::{Draft, Program};

fn step(value: serde_json::Value) -> Step {
    serde_json::from_value(value).unwrap()
}

fn draft(steps: Vec<Step>) -> Draft {
    let program = Program {
        inputs: vec![
            ActionParam::required("who", ParamType::Text, "Who"),
            ActionParam::required("count", ParamType::Integer, "How many"),
        ],
        steps,
        output: None,
        ..Program::default()
    };
    let mut minted = 0_u32;
    Draft::new(
        "Digest".into(),
        "digest".into(),
        &program,
        false,
        &mut || {
            minted += 1;
            ui::prelude::Id::from_raw(format!("item{minted}"))
        },
    )
}

fn list() -> Step {
    step(
        json!({"id": "list", "step": "invoke", "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
                "resource": {"fixed": "notes.folder:inbox"}}),
    )
}

fn create(args: &serde_json::Value) -> Step {
    step(
        json!({"id": "create", "step": "invoke", "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "create"},
                "resource": {"fixed": "notes.folder:inbox"},
                "args": args}),
    )
}

#[test]
fn a_parameter_is_offered_only_type_compatible_values_in_scope() {
    let catalog = crate::samples::catalog();
    let draft = draft(vec![list(), create(&json!({}))]);
    let title = ActionParam::required("title", ParamType::Text, "Title");
    let offered: Vec<Binding> = choices(&draft, &catalog, "1", &title)
        .into_iter()
        .map(|choice| choice.binding)
        .collect();
    assert_eq!(
        offered,
        vec![Binding::Input("who".into())],
        "the integer input and the list's array are not text"
    );
    let pinned = ActionParam::required("pinned", ParamType::Boolean, "Pin");
    assert!(choices(&draft, &catalog, "1", &pinned).is_empty());
    let items = ActionParam::required("items", ParamType::Array, "Items");
    let arrays: Vec<Binding> = choices(&draft, &catalog, "1", &items)
        .into_iter()
        .map(|choice| choice.binding)
        .collect();
    assert_eq!(arrays, vec![Binding::step("list", "/titles")]);
    // The first step cannot use its own result.
    assert!(
        choices(&draft, &catalog, "0", &items).is_empty(),
        "nothing earlier than the first step"
    );
}

#[test]
fn problems_name_the_offending_step() {
    let catalog = crate::samples::catalog();
    let broken = draft(vec![
        list(),
        create(&json!({"title": {"step_output": {"step": "list", "pointer": "/titles"}}})),
    ]);
    let found = problems(&broken, &catalog, &[]);
    let at_create = found.get("1").expect("the create step is wrong");
    assert!(
        at_create.iter().any(|reason| reason.contains("title")),
        "{found:?}"
    );
    assert!(
        at_create
            .iter()
            .any(|reason| reason == "Pin it needs a value"),
        "the required `pinned` is unbound: {found:?}"
    );
    assert!(!found.contains_key("0"), "the list step is fine: {found:?}");

    let fixed = draft(vec![
        list(),
        create(&json!({
            "title": {"input": "who"},
            "pinned": {"literal": false}
        })),
    ]);
    assert!(problems(&fixed, &catalog, &[]).is_empty());
}

#[test]
fn the_proposed_grant_is_the_least_each_step_needs() {
    let catalog = crate::samples::catalog();
    let steps = vec![
        list(),
        create(&json!({})),
        step(json!({"id": "ai", "step": "ai", "transform": "summarise",
                    "input": {"literal": "x"}})),
        list(),
    ];
    let permissions = grant::least_privilege(&steps, &catalog);
    let named: Vec<(String, String, tilo_action_policy::ActionKind)> = permissions
        .iter()
        .map(|permission| {
            (
                permission.app_id.clone(),
                permission.action.clone(),
                permission.kind,
            )
        })
        .collect();
    assert_eq!(
        named,
        vec![
            (
                "so.tilo.notes".into(),
                "list".into(),
                tilo_action_policy::ActionKind::Read
            ),
            (
                "so.tilo.notes".into(),
                "create".into(),
                tilo_action_policy::ActionKind::Undoable
            ),
            (
                tilo_automation::AGENT_APP_ID.into(),
                "summarise".into(),
                tilo_action_policy::ActionKind::Read
            ),
        ]
    );
    // The draft checks against exactly that grant.
    let draft = draft(vec![
        list(),
        create(&json!({"title": {"input": "who"}, "pinned": {"literal": true}})),
    ]);
    assert!(problems(&draft, &catalog, &[]).is_empty());
    assert_eq!(candidate(&draft, &catalog).grant.len(), 2);
}

#[test]
fn typed_text_becomes_a_typed_literal() {
    let param = |ty| ActionParam::required("p", ty, "p");
    assert_eq!(literal("42", &param(ParamType::Integer)), json!(42));
    assert_eq!(literal("4.5", &param(ParamType::Number)), json!(4.5));
    assert_eq!(literal("yes", &param(ParamType::Boolean)), json!(true));
    assert_eq!(literal("[1, 2]", &param(ParamType::Array)), json!([1, 2]));
    assert_eq!(literal(" hi ", &param(ParamType::Text)), json!(" hi "));
    // Not a number stays text, so the checker names the problem.
    assert_eq!(literal("many", &param(ParamType::Integer)), json!("many"));
}

#[test]
fn the_picker_groups_the_catalogue_by_app_then_kind() {
    let groups = picker(&crate::samples::catalog());
    let apps: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
    // Ordered by application id.
    assert_eq!(apps, ["Agent", "Mail", "Notes", "Shortcuts"]);
    let notes = groups
        .iter()
        .find(|group| group.app_id == "so.tilo.notes")
        .unwrap();
    assert_eq!(notes.kinds.len(), 1);
    let create = notes.kinds[0]
        .offers
        .iter()
        .find(|offer| offer.action == "create")
        .unwrap();
    assert_eq!(create.params, ["title: text", "pinned: boolean"]);
    assert_eq!(app_label("so.tilo.speed-test"), "Speed Test");
}

#[test]
fn steps_are_numbered_in_reading_order_and_bindings_name_them_so() {
    let branch = step(json!({
        "id": "branch", "step": "if", "cond": {"truthy": {"input": "who"}},
        "then": [{"id": "create", "step": "invoke", "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "create"},
                  "resource": {"fixed": "notes.folder:inbox"}}]
    }));
    let steps = vec![list(), branch];
    assert_eq!(step_number(&steps, "list"), Some(1));
    assert_eq!(step_number(&steps, "branch"), Some(2));
    assert_eq!(
        step_number(&steps, "create"),
        Some(3),
        "a step inside a branch follows the step that holds it"
    );
    assert_eq!(step_number(&steps, "missing"), None);

    let said = |binding: Binding| describe(&binding, &steps);
    assert_eq!(said(Binding::Input("who".into())), "input who");
    assert_eq!(said(Binding::step("list", "")), "result of step 1");
    assert_eq!(said(Binding::step("list", "/titles")), "titles from step 1");
    assert_eq!(
        said(Binding::step("list", "/titles/0")),
        "item 1 of titles from step 1"
    );
    assert_eq!(
        said(Binding::Template("{{step.list/titles}}".into())),
        "titles from step 1",
        "a template that is only a placeholder reads as the value itself"
    );
    assert_eq!(
        said(Binding::Template("Re: {{input.who}} ({{var.n}})".into())),
        "text “Re: [input who] ([variable n])”"
    );
}

#[test]
fn a_step_is_titled_by_what_it_does_and_placed_by_where_it_acts() {
    let catalog = crate::samples::catalog();
    let steps = vec![list()];
    let listing = &steps[0];
    assert_eq!(title(listing, &catalog, &steps), "List notes");
    assert_eq!(
        context(listing, &catalog).as_deref(),
        Some("Inbox in Notes")
    );
    let unknown = step(
        json!({"id": "x", "step": "invoke", "action": {"app": "so.tilo.gone", "kind": "gone", "action": "vanish"},
                              "resource": {"fixed": "gone"}}),
    );
    assert_eq!(title(&unknown, &catalog, &steps), "vanish");
    assert_eq!(
        context(&unknown, &catalog).as_deref(),
        Some("Gone no longer offers this action")
    );
}

/// An empty draft is not "ready to save", and neither is one whose steps'
/// actions could not be read: the editor says what is missing instead.
#[test]
fn the_save_status_says_what_is_missing() {
    use crate::door::Loaded;
    use crate::view::{EditorInput, save_blocker};
    let id = ui::prelude::Id::from_raw("d");
    let input = |steps: Vec<Step>, catalogue: Loaded<()>| EditorInput {
        draft: Some((id.clone(), draft(steps))),
        catalogue,
        ..EditorInput::default()
    };
    assert_eq!(
        save_blocker(&input(Vec::new(), Loaded::Ready(()))).as_deref(),
        Some("Add a step to save this shortcut"),
        "an empty draft does nothing"
    );
    assert!(
        save_blocker(&input(vec![list()], Loaded::Loading))
            .is_some_and(|why| why.starts_with("Reading")),
        "steps wait for the actions they choose from"
    );
    assert!(
        save_blocker(&input(vec![list()], Loaded::Failed("x".into())))
            .is_some_and(|why| why.starts_with("Can't save here")),
        "an unreadable catalogue is said plainly"
    );
    assert_eq!(save_blocker(&input(vec![list()], Loaded::Ready(()))), None);
}

/// A catalogue read nothing answered is explained in the person's words,
/// not as the client's refusal.
#[test]
fn an_unanswered_catalogue_is_explained_plainly() {
    let unanswered = ui::tilo_actions::remote::ActionHostError::Unavailable {
        reason: ui::remote_actions::CATALOGUE_UNANSWERED.into(),
    };
    let message = crate::door::catalogue_refusal(&unanswered);
    assert!(
        message.starts_with("Nothing in this workspace answered"),
        "{message}"
    );
    assert!(!message.contains("remote actions"), "{message}");
}
