//! The text language's own tests: what a program prints to, and what that
//! text reads back to.
//!
//! A stored program and the text a person reads are two views of one thing:
//! these tests hold each view to the other, and hold an edit made in one to the
//! steps the document already has.

mod roundtrip;

use super::*;
use crate::shortcut::{Schedule, TriggerSpec};

use tilo_actions::action::{ExecutionLocus, OperationKind};
use tilo_actions::contract::ActionContract;
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::params::ActionParam;
use tilo_actions::remote::{CatalogueResource, CatalogueRow};

const NOTES: &str = "so.tilo.notes";

fn row(
    app: &str,
    resource: &str,
    kind: &str,
    action: &str,
    params: Vec<ActionParam>,
) -> CatalogueRow {
    CatalogueRow {
        provider: format!("{app}#1"),
        app_id: app.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: kind.into(),
            label: resource.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: kind.into(),
            resource_label: kind.into(),
            action_label: action.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Document,
            contract: ActionContract::default(),
            input: InputSpec::None,
            params,
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

fn catalog() -> ActionCatalog {
    ActionCatalog::new(vec![
        row(
            NOTES,
            "notes.folder:inbox",
            "notes.folder",
            "list",
            Vec::new(),
        ),
        row(
            NOTES,
            "notes.note:1",
            "notes.note",
            "file",
            vec![ActionParam::required("folder", ParamType::Text, "Folder")],
        ),
    ])
}

fn bare(app_id: &str) -> String {
    app_id.rsplit('.').next().unwrap_or(app_id).to_owned()
}

fn step(name: &str, action: &str, resource: Selector, args: BTreeMap<String, Binding>) -> Step {
    Step::new(
        name,
        StepKind::Invoke {
            action: ActionKey::new(NOTES, "notes.note", action),
            resource,
            args,
            appearance: None,
        },
    )
}

fn program() -> Shortcut {
    Shortcut {
        id: "filing".into(),
        name: "File the notes".into(),
        inputs: vec![ActionParam::required(
            "folder",
            ParamType::Text,
            "Where they go",
        )],
        steps: vec![
            step(
                "listed",
                "file",
                Selector::FirstOfKind("notes.note".into()),
                BTreeMap::from([("folder".to_owned(), Binding::Input("folder".into()))]),
            ),
            Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::step("listed", "/titles"),
                    item: "title".into(),
                    body: vec![step(
                        "filed",
                        "file",
                        Selector::Fixed("notes.note:1".into()),
                        BTreeMap::from([("folder".to_owned(), Binding::Var("title".into()))]),
                    )],
                },
            ),
        ],
        step_timeout_secs: Some(30),
        output: Some(Binding::step("filed", "")),
        grant: Vec::new(),
        triggers: vec![TriggerSpec::Keyboard {
            chord: "super+shift+f".into(),
        }],
        saved_by: None,
        saved_proof: None,
    }
}

#[test]
fn a_program_reads_back_as_the_program_it_was() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let (text, outline) = match render(&shared, &program(), &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    assert!(text.contains("File the notes"), "{text}");
    assert!(!outline.steps.is_empty());
    let parsed = match parse(&shared, &text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{error}\n{text}"),
    };
    assert_eq!(parsed.shortcut.name, "File the notes");
    assert_eq!(parsed.shortcut.steps.len(), 2);
    assert_eq!(parsed.shortcut.step_timeout_secs, Some(30));
    assert_eq!(parsed.shortcut.triggers, program().triggers);
}

#[test]
fn a_nested_block_is_indented_and_parsed_back_nested() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let shortcut = Shortcut {
        id: "branching".into(),
        name: "Maybe file".into(),
        inputs: vec![ActionParam::required(
            "folder",
            ParamType::Text,
            "Where they go",
        )],
        steps: vec![Step::new(
            "checked",
            StepKind::If {
                cond: Condition::Truthy(Binding::Input("folder".into())),
                then: vec![step(
                    "listed",
                    "file",
                    Selector::FirstOfKind("notes.note".into()),
                    BTreeMap::from([("folder".to_owned(), Binding::Input("folder".into()))]),
                )],
                otherwise: Vec::new(),
            },
        )],
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    };
    let (text, _) = match render(&shared, &shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    // One level under the step is one level of the two spaces an indent is.
    assert!(text.contains("\n  file"), "the branch is indented:\n{text}");
    assert!(
        !text.contains("\n    file"),
        "the branch is one level under:\n{text}"
    );
    let parsed = match parse(&shared, &text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{error}\n{text}"),
    };
    let StepKind::If { then, .. } = &parsed.shortcut.steps[0].kind else {
        panic!("the first step is the branch:\n{text}");
    };
    assert_eq!(then.len(), 1);
}

#[test]
fn a_tab_indent_is_refused_by_line() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let Err(error) = parse(&shared, "shortcut \"x\"\n\twhen input \"a\" is set:\n") else {
        panic!("a tab is not an indent the text writes");
    };
    assert_eq!(error.line, 2);
}

#[test]
fn a_line_that_is_not_a_step_names_itself() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let Err(error) = parse(&shared, "shortcut \"x\"\nmow the lawn\n") else {
        panic!("that is not a step");
    };
    assert_eq!(error.line, 2);
}

#[test]
fn an_edited_line_keeps_the_identity_of_the_steps_it_did_not_touch() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let before = program();
    let (text, _) = match render(&shared, &before, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    // File one more note before the loop: the steps it already had are
    // still the steps the document holds.
    let edited = text.replace(
        "for each title in",
        "file on any Notes.note in notes Folder input folder\nfor each title in",
    );
    let edit = match edit(&shared, &before, &edited) {
        Ok(edit) => edit,
        Err(error) => panic!("{error}\n{edited}"),
    };
    assert_eq!(edit.shortcut.id, before.id);
    assert_eq!(edit.shortcut.steps.len(), 3);
    assert_eq!(edit.shortcut.steps[0].id, "listed");
    assert_eq!(edit.shortcut.steps[1].id, "");
    assert_eq!(edit.shortcut.steps[2].id, "each");
    assert!(edit.removed.is_empty(), "{:?}", edit.removed);
}

#[test]
fn a_step_the_text_dropped_is_reported_as_leaving() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let before = program();
    let (text, _) = match render(&shared, &before, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    let cut: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("for each title") && !line.starts_with("  "))
        .collect();
    let edited = cut.join("\n");
    let edit = match edit(&shared, &before, &edited) {
        Ok(edit) => edit,
        Err(error) => panic!("{error}\n{edited}"),
    };
    assert_eq!(edit.shortcut.steps.len(), 1);
    assert_eq!(edit.shortcut.steps[0].id, "listed");
    assert_eq!(edit.removed, vec!["each".to_owned()]);
}

#[test]
fn a_problem_the_checker_reports_lands_on_the_line_that_states_the_step() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    // The loop's body files a note the step above never listed: the
    // checker reads a name nothing set.
    let shortcut = Shortcut {
        id: "broken".into(),
        name: "File the notes".into(),
        inputs: Vec::new(),
        steps: vec![Step::new(
            "each",
            StepKind::ForEach {
                items: Binding::step("listed", "/titles"),
                item: "title".into(),
                body: vec![step(
                    "filed",
                    "file",
                    Selector::FirstOfKind("notes.note".into()),
                    BTreeMap::from([("folder".to_owned(), Binding::Var("title".into()))]),
                )],
            },
        )],
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    };
    let (text, outline) = match render(&shared, &shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    let listed = StepPath::root().child(0).each(None).child(0);
    let Some(line) = outline.line_of(&listed) else {
        panic!("the body's step is on a line:\n{text}");
    };
    let stated = text.lines().nth(line - 1).unwrap_or_default();
    assert!(
        stated.trim_start().starts_with("file"),
        "line {line}: {stated:?}"
    );
    assert!(outline.line_of(&StepPath::root()).is_none());
    assert!(outline.line_of(&StepPath::output()).is_none());
}

/// The name the text gives a step, which is also the identity a program's
/// own values refer to its result by.
const LISTED: &str = "file on any Notes.note in notes";

/// Every kind of value the language writes, so a rendering of this program
/// covers each of them.
fn every_binding(folder: &Binding) -> Vec<Binding> {
    vec![
        Binding::Literal(Value::from(3)),
        Binding::Literal(Value::from("a word")),
        Binding::Literal(Value::Bool(true)),
        Binding::Literal(Value::Null),
        folder.clone(),
        Binding::Var("title".into()),
        Binding::step(LISTED, ""),
        Binding::step(LISTED, "/titles"),
        Binding::Template("hello {folded}".into()),
        Binding::List(vec![folder.clone(), Binding::Literal(Value::from(1))]),
        Binding::Object(BTreeMap::from([("to".to_owned(), folder.clone())])),
    ]
}

#[test]
fn every_kind_of_value_prints_the_same_program_back() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let folder = Binding::Input("folder".into());
    let values = every_binding(&folder);
    let mut steps = vec![step(
        LISTED,
        "file",
        Selector::FirstOfKind("notes.note".into()),
        BTreeMap::from([("folder".to_owned(), folder)]),
    )];
    for (index, value) in values.into_iter().enumerate() {
        steps.push(Step::new(
            format!("set{index}"),
            StepKind::SetVar {
                name: format!("value{index}"),
                value,
            },
        ));
    }
    let shortcut = Shortcut {
        id: "values".into(),
        name: "Every value".into(),
        inputs: vec![ActionParam::required(
            "folder",
            ParamType::Text,
            "Where they go",
        )],
        steps,
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    };
    let (text, _) = match render(&shared, &shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    let parsed = match parse(&shared, &text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{error}\n{text}"),
    };
    let kinds = |shortcut: &Shortcut| -> Vec<StepKind> {
        let mut out = Vec::new();
        for step in &shortcut.steps {
            out.push(step.kind.clone());
            if let StepKind::ForEach { body, .. } | StepKind::If { then: body, .. } = &step.kind {
                out.extend(body.iter().map(|step| step.kind.clone()));
            }
        }
        out
    };
    assert_eq!(kinds(&parsed.shortcut), kinds(&shortcut), "\n{text}");
}

/// A program that holds one of each kind of step, so a rendering of it
/// covers each of them.
fn every_step(folder: Binding) -> Shortcut {
    Shortcut {
        id: "kinds".into(),
        name: "Every step".into(),
        inputs: vec![
            ActionParam::required("folder", ParamType::Text, "Where they go"),
            ActionParam::required("count", ParamType::Integer, "How many"),
        ],
        steps: vec![
            step(
                LISTED,
                "file",
                Selector::FirstOfKind("notes.note".into()),
                BTreeMap::from([("folder".to_owned(), folder.clone())]),
            ),
            // Both steps read alike, so the second's name carries the number
            // that keeps one name per step.
            step(
                "file on any Notes.note in notes 2",
                "file",
                Selector::FromBinding(folder.clone()),
                BTreeMap::from([("folder".to_owned(), folder.clone())]),
            ),
            Step::new(
                "each",
                StepKind::ForEach {
                    items: Binding::step(LISTED, "/titles"),
                    item: "title".into(),
                    body: vec![Step::new(
                        String::new(),
                        StepKind::SetVar {
                            name: "kept".into(),
                            value: Binding::Var("title".into()),
                        },
                    )],
                },
            ),
            Step::new(
                "checked",
                StepKind::If {
                    cond: Condition::Any(vec![
                        Condition::Truthy(folder.clone()),
                        Condition::Not(Box::new(Condition::Equals {
                            left: Binding::Var("kept".into()),
                            right: Binding::Literal(Value::from("x")),
                        })),
                    ]),
                    then: vec![Step::new(
                        String::new(),
                        StepKind::SetVar {
                            name: "first".into(),
                            value: Binding::Literal(Value::from(1)),
                        },
                    )],
                    otherwise: vec![Step::new(
                        String::new(),
                        StepKind::SetVar {
                            name: "second".into(),
                            value: Binding::Literal(Value::from(2)),
                        },
                    )],
                },
            ),
            Step::new(
                "asked",
                // The line reads the parameter's own words, so its name is
                // the words rather than a shorter identifier.
                StepKind::Ask {
                    param: ActionParam::required(
                        "The note's name",
                        ParamType::Text,
                        "The note's name",
                    ),
                },
            ),
            Step::new(
                "written",
                StepKind::Ai {
                    prompt: "Summarise the notes".into(),
                    output: None,
                    input: Some(Binding::step(LISTED, "/titles")),
                },
            ),
            Step::new(
                "tasked",
                StepKind::Task {
                    instruction: "File the notes".into(),
                    allow: Vec::new(),
                    output: None,
                },
            ),
        ],
        step_timeout_secs: Some(45),
        // What the shortcut gives back names an input, which is in scope
        // where the steps' own variables are not.
        output: Some(folder),
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    }
}

#[test]
fn every_kind_of_step_prints_the_same_program_back() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let shortcut = every_step(Binding::Input("folder".into()));
    let (text, _) = match render(&shared, &shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    let parsed = match parse(&shared, &text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{error}\n{text}"),
    };
    assert_eq!(parsed.shortcut.steps.len(), shortcut.steps.len(), "{text}");
    assert_eq!(
        parsed
            .shortcut
            .steps
            .iter()
            .map(|step| step.kind.clone())
            .collect::<Vec<_>>(),
        shortcut
            .steps
            .iter()
            .map(|step| step.kind.clone())
            .collect::<Vec<_>>(),
        "{text}"
    );
    assert_eq!(parsed.shortcut.step_timeout_secs, Some(45));
}

#[test]
fn every_kind_of_trigger_prints_the_same_program_back() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let triggers = vec![
        TriggerSpec::Keyboard {
            chord: "super+shift+f".into(),
        },
        TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds: 600 },
            catch_up: crate::shortcut::CatchUp::Skip,
        },
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar {
                at: crate::shortcut::TimeOfDay { hour: 9, minute: 5 },
                weekdays: vec![
                    crate::shortcut::Weekday::Monday,
                    crate::shortcut::Weekday::Friday,
                ],
            },
            catch_up: crate::shortcut::CatchUp::RunOnce,
        },
        TriggerSpec::AppOpened {
            app_id: "so.tilo.notes".into(),
        },
        TriggerSpec::WorkspaceJoined {
            room: Some("Studio".into()),
        },
        TriggerSpec::WorkspaceJoined { room: None },
        TriggerSpec::FileChanged {
            capability: "notes".into(),
        },
        TriggerSpec::FocusMode {
            mode: "do-not-disturb".into(),
            transition: crate::shortcut::FocusTransition::Enter,
        },
        TriggerSpec::NetworkChanged {
            network: Some("home".into()),
            connected: Some(true),
        },
        TriggerSpec::NetworkChanged {
            network: None,
            connected: None,
        },
        TriggerSpec::DeviceConnected {
            device_class: "camera".into(),
        },
        TriggerSpec::RecordChanged {
            kind: "notes.note".into(),
            actions: vec!["file".into()],
            fields: vec!["title".into()],
            by: crate::shortcut::ActorFilter::NotBots,
        },
        TriggerSpec::Connector {
            app_id: "so.tilo.mail".into(),
            source: "message".into(),
            filter: serde_json::Map::from_iter([(
                "from".to_owned(),
                Value::from("ada@example.com"),
            )]),
        },
        TriggerSpec::IncomingMessage {
            app_id: Some("so.tilo.mail".into()),
            from: Some("ada@example.com".into()),
        },
        TriggerSpec::NearbyDevice {
            device: Some("Ada's phone".into()),
        },
        TriggerSpec::NearbyDevice { device: None },
        TriggerSpec::AppEvent {
            app_id: "so.tilo.mail".into(),
            kind: "mail.message".into(),
        },
    ];
    let shortcut = Shortcut {
        id: "triggered".into(),
        name: "Triggered".into(),
        inputs: Vec::new(),
        steps: Vec::new(),
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: triggers.clone(),
        saved_by: None,
        saved_proof: None,
    };
    let (text, _) = match render(&shared, &shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    let parsed = match parse(&shared, &text) {
        Ok(parsed) => parsed,
        Err(error) => panic!("{error}\n{text}"),
    };
    assert_eq!(parsed.shortcut.triggers, triggers, "\n{text}");
    assert!(
        text.contains(r#"when "so.tilo.mail" announces "mail.message""#),
        "an app's event names the app and the event it declared: {text}"
    );
}
