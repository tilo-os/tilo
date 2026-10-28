//! Every valid program a shortcut can hold reads back as itself.
//!
//! A program is written as text for a person to read and edit, and that text
//! is read back into the document: the two must not disagree about what the
//! shortcut does. The programs here are generated over the same vocabulary the
//! stored program has — every selector, every step kind, nested blocks, loops,
//! conditions, triggers and a grant — so a shape that loses a field on the way
//! through the text is found without someone thinking to write it by hand.
//!
//! The generator is a small deterministic one: a seed walks a fixed sequence,
//! so a failure names the seed that produced it and the same program comes back
//! on the next run.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tilo_action_policy::{Answer, KindSet, Rule};
use tilo_actions::params::{ActionParam, ParamType};

use crate::action_rule;
use crate::shortcut::{
    Binding, CatchUp, Condition, Schedule, Selector, Shortcut, Step, StepKind, TriggerSpec,
};
use crate::text::{Shared, render};

use super::{NOTES, bare, catalog, step as invoke_step};

/// The words the text writes a folder argument with, in the test catalogue.
const FOLDER_ARGUMENT: &str = "Folder input folder";

/// How many programs each seed space walks.
const RUNS: usize = 40;

/// A tiny deterministic generator: a seed in, the next number out.
struct Seed(u64);

impl Seed {
    fn next(&mut self) -> u64 {
        // xorshift64*, chosen because it is a few lines and its output does
        // not depend on the standard library's own hashing.
        let mut at = self.0;
        at ^= at >> 12;
        at ^= at << 25;
        at ^= at >> 27;
        self.0 = at;
        at.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        usize::try_from(self.next().checked_rem(u64::try_from(bound).unwrap_or(1)).unwrap_or(0))
            .unwrap_or(0)
    }

    fn pick<'a, T>(&mut self, choices: &'a [T]) -> &'a T {
        let at = self.below(choices.len());
        &choices[at]
    }

    fn chance(&mut self) -> bool {
        self.next().is_multiple_of(2)
    }
}

/// A program the generator wrote, rendered, and read back into a document.
///
/// The text is written from the program and then edited back into that same
/// program — the document path, not a fresh parse — so every field the text
/// states has to come back exactly as it went in.
fn round_trip(shortcut: &Shortcut) {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let (text, _) = match render(&shared, shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}\n{shortcut:?}"),
    };
    let edit = match crate::text::edit(&shared, shortcut, &text) {
        Ok(edit) => edit,
        Err(error) => panic!("line {}: {error}\n{text}\n{shortcut:?}", error.line),
    };
    assert!(
        edit.removed.is_empty(),
        "{:?} left a program whose text still names it\n{text}",
        edit.removed
    );
    let after = &edit.shortcut;
    assert_eq!(
        unnamed(&after.steps),
        unnamed(&shortcut.steps),
        "{}",
        difference(shortcut, after, &text)
    );
    // A name is written on a line only when something reads the step's
    // result, so a step nothing reads may come back without one; every name
    // the program does read comes back spelled the same, which is what keeps a
    // reference landing on the step that wrote it.
    let referred = referred_names(&shortcut.steps);
    for step in &shortcut.steps {
        if referred.contains(&step.id) {
            assert!(
                after.steps.iter().any(|back| back.id == step.id),
                "`{}` is read, so the text names it\n{text}",
                step.id
            );
        }
    }
    assert_eq!(after.name, shortcut.name);
    assert_eq!(after.inputs, shortcut.inputs);
    assert_eq!(after.output, shortcut.output, "{text}");
    assert_eq!(after.grant, shortcut.grant, "{text}");
    assert_eq!(after.triggers, shortcut.triggers, "{text}");
    assert_eq!(after.step_timeout_secs, shortcut.step_timeout_secs);
    // The words a person edits are the words the document writes back: a
    // program that has not changed has text that has not changed.
    let (again, _) = match render(&shared, after, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}\n{after:?}"),
    };
    assert_eq!(again, text, "a program renders as the text it read from");
}

/// The steps with their names taken off.
///
/// A name is on the line only when something reads the step's result, so it is
/// a property of the program rather than a field the text loses; everything
/// else about a step is the text's to keep.
fn unnamed(steps: &[Step]) -> Vec<Step> {
    steps
        .iter()
        .map(|step| {
            let mut step = step.clone();
            step.id = String::new();
            match &mut step.kind {
                StepKind::If {
                    then, otherwise, ..
                } => {
                    *then = unnamed(then);
                    *otherwise = unnamed(otherwise);
                }
                StepKind::ForEach { body, .. } => *body = unnamed(body),
                _ => {}
            }
            step
        })
        .collect()
}

/// The step names the program reads a result from.
fn referred_names(steps: &[Step]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    crate::text::collect_references(steps, &mut names);
    names
}

/// What differs, beside the text that states it, for a failure to read.
fn difference(before: &Shortcut, after: &Shortcut, text: &str) -> String {
    let had: Vec<String> = before.steps.iter().map(shape_of).collect();
    let now: Vec<String> = after.steps.iter().map(shape_of).collect();
    format!("{text}\nbefore: {had:?}\nafter: {now:?}")
}

/// One step in a line, so a difference between two programs reads at a glance.
fn shape_of(step: &Step) -> String {
    format!("{} {:?}", step.id, step.kind)
}

/// A program the generator writes, every field of it chosen from the whole
/// vocabulary a stored shortcut has.
fn generate(seed: &mut Seed) -> Shortcut {
    let steps = block(seed, 0);
    Shortcut {
        id: "generated".into(),
        name: seed
            .pick(&["File the notes", "Tidy", "Send \"it\""])
            .to_string(),
        inputs: inputs(seed),
        steps: steps.clone(),
        step_timeout_secs: seed
            .chance()
            .then(|| u64::try_from(seed.below(300)).unwrap_or(0).saturating_add(5)),
        output: output_for(seed, &steps),
        grant: grant(seed),
        triggers: triggers(seed),
        saved_by: None,
        saved_proof: None,
    }
}

fn inputs(seed: &mut Seed) -> Vec<ActionParam> {
    let mut inputs = vec![ActionParam::required(
        "folder",
        ParamType::Text,
        "Where they go",
    )];
    if seed.chance() {
        inputs.push(ActionParam::optional(
            "count",
            ParamType::Number,
            "How many",
        ));
    }
    if seed.chance() {
        inputs.push(ActionParam::optional("yes", ParamType::Boolean, "Ask them"));
    }
    inputs
}

/// The steps of a block. A block's depth bounds the nesting, so a program is
/// always finite.
fn block(seed: &mut Seed, depth: usize) -> Vec<Step> {
    let count = seed.below(3).saturating_add(1);
    let mut steps = Vec::new();
    for index in 0..count {
        let name = format!("step{index}");
        steps.push(one(seed, depth, &name));
    }
    steps
}

fn one(seed: &mut Seed, depth: usize, name: &str) -> Step {
    let nested = depth < 2;
    let kind = match seed.below(if nested { 6 } else { 4 }) {
        0 => StepKind::SetVar {
            name: format!("{name}_value"),
            value: value(seed),
        },
        1 => StepKind::Ask {
            param: ActionParam::required("answer", ParamType::Text, "What to write"),
        },
        2 => StepKind::Ai {
            prompt: "a sentence".into(),
            output: None,
            input: None,
        },
        3 => StepKind::Task {
            instruction: "do the thing".into(),
            allow: Vec::new(),
            output: None,
        },
        4 => StepKind::ForEach {
            items: value(seed),
            item: format!("{name}_item"),
            body: block(seed, depth.saturating_add(1)),
        },
        _ => StepKind::If {
            cond: condition(seed),
            then: block(seed, depth.saturating_add(1)),
            otherwise: if seed.chance() {
                block(seed, depth.saturating_add(1))
            } else {
                Vec::new()
            },
        },
    };
    match seed.below(4) {
        // An invoke is the shape a person writes most, so it is asked for
        // more often than the others together.
        0..=2 => {
            // The shortcut helper the tests share builds a `file` step on a
            // note, which is the catalogue row that declares a parameter: what
            // varies here is the resource it acts on and what it passes it.
            invoke_step(name, "file", selector(seed), args(seed))
        }
        _ => Step::new(name, kind),
    }
}

/// Every selector a step may act on: the one resource it names, whatever of a
/// kind is to hand, and the resource a binding yields.
fn selector(seed: &mut Seed) -> Selector {
    match seed.below(4) {
        0 => Selector::Fixed("notes.note:1".into()),
        1 => Selector::FirstOfKind(seed.pick(&["notes.note", "notes.folder"]).to_string()),
        2 => Selector::FromBinding(Binding::Input("folder".into())),
        // The text names one resource the same way whether the document holds
        // it as a resource or as the value a binding yields, so the generated
        // programs use the spelling the text reads back.
        _ => Selector::Fixed("notes.note:2".into()),
    }
}

fn args(seed: &mut Seed) -> BTreeMap<String, Binding> {
    let mut args = BTreeMap::new();
    if seed.chance() {
        args.insert("folder".to_owned(), value(seed));
    }
    args
}

/// Every kind of value the vocabulary has.
fn value(seed: &mut Seed) -> Binding {
    match seed.below(8) {
        0 => Binding::Literal(serde_json::json!("a text, with commas, and \"quotes\"")),
        1 => Binding::Literal(serde_json::json!(42)),
        2 => Binding::Literal(serde_json::Value::Bool(true)),
        3 => Binding::Literal(serde_json::Value::Null),
        4 => Binding::Input("folder".into()),
        5 => Binding::Literal(serde_json::json!(["one", "two"])),
        // A variable no line in a generated program sets: a variable that
        // names a step's own result reads as that result, and the vocabulary
        // this test holds a program to is the text's, not the document's.
        6 => Binding::Var("a_variable".into()),
        _ => Binding::Literal(serde_json::json!({"path": ["a", "b"], "kind": "text"})),
    }
}

fn condition(seed: &mut Seed) -> Condition {
    match seed.below(3) {
        0 => Condition::Truthy(value(seed)),
        1 => Condition::Equals {
            left: value(seed),
            right: value(seed),
        },
        _ => Condition::Not(Box::new(Condition::Truthy(value(seed)))),
    }
}

/// What the shortcut gives back: the result of a step it holds, when it has
/// one, so the line names a step the text states.
fn output_for(seed: &mut Seed, steps: &[Step]) -> Option<Binding> {
    let step = steps.get(seed.below(steps.len()))?;
    seed.chance().then(|| Binding::step(step.id.clone(), ""))
}

fn grant(seed: &mut Seed) -> Vec<Rule> {
    if !seed.chance() {
        return Vec::new();
    }
    vec![
        action_rule(
            NOTES,
            "notes.note",
            None,
            "list",
            KindSet::READ_UNDOABLE,
            Answer::Allow,
        ),
        action_rule(
            NOTES,
            "notes.note",
            Some("notes.note:1"),
            "file",
            KindSet::READ_UNDOABLE,
            Answer::Ask,
        ),
    ]
}

fn triggers(seed: &mut Seed) -> Vec<TriggerSpec> {
    let mut triggers = Vec::new();
    if seed.chance() {
        triggers.push(TriggerSpec::Keyboard {
            chord: "super+shift+f".into(),
        });
    }
    if seed.chance() {
        triggers.push(TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds: 60 },
            catch_up: CatchUp::Skip,
        });
    }
    triggers
}

#[test]
fn every_generated_program_reads_back_as_itself() {
    for start in 1..=RUNS {
        let mut seed = Seed(
            u64::try_from(start)
                .unwrap_or(1)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        let shortcut = generate(&mut seed);
        round_trip(&shortcut);
    }
}

#[test]
fn a_program_a_person_edited_reads_back_as_the_program_they_wrote() {
    // An edit lands as the same text a fresh render writes, so the two views
    // agree on the document rather than parting on the first keystroke.
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    // A seed whose program names the folder argument, so there is an argument
    // in the text to change.
    let mut seed = Seed(0x5EED);
    let mut shortcut = None;
    for _ in 0..RUNS {
        let program = generate(&mut seed);
        let (text, _) = match render(&shared, &program, &BTreeMap::new()) {
            Ok(rendered) => rendered,
            Err(error) => panic!("{error}"),
        };
        if text.contains(FOLDER_ARGUMENT) {
            shortcut = Some(program);
            break;
        }
    }
    let Some(shortcut) = shortcut else {
        panic!("no generated program names its folder argument");
    };
    let (text, _) = match render(&shared, &shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    // One argument changed in the text: the program reads back with that
    // argument changed and nothing else.
    let edited = text.replacen(FOLDER_ARGUMENT, "Folder \"elsewhere\"", 1);
    assert_ne!(edited, text, "the program has the argument to change");
    let edit = match crate::text::edit(&shared, &shortcut, &edited) {
        Ok(edit) => edit,
        Err(error) => panic!("line {}: {error}\n{edited}", error.line),
    };
    let (again, _) = match render(&shared, &edit.shortcut, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    assert_eq!(again, edited, "a program's text is the text it reads from");
}

#[test]
fn one_changed_argument_changes_one_field_of_one_step() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let before = super::program();
    let (text, _) = match render(&shared, &before, &BTreeMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => panic!("{error}"),
    };
    let edited = text.replacen("Folder input folder", "Folder \"elsewhere\"", 1);
    assert_ne!(edited, text);
    let edit = match crate::text::edit(&shared, &before, &edited) {
        Ok(edit) => edit,
        Err(error) => panic!("{error}\n{edited}"),
    };
    let listed = &edit.shortcut.steps[0];
    let StepKind::Invoke { args, resource, .. } = &listed.kind else {
        panic!("the first step invokes");
    };
    assert_eq!(
        args.get("folder"),
        Some(&Binding::Literal(serde_json::json!("elsewhere")))
    );
    assert_eq!(*resource, Selector::FirstOfKind("notes.note".into()));
    // Every other step is the step the document already held.
    assert_eq!(listed.id, "listed");
    assert_eq!(edit.shortcut.steps[1].id, "each");
    assert!(edit.removed.is_empty(), "{:?}", edit.removed);
}

#[test]
fn a_dictionary_holding_a_list_reads_as_one_value() {
    let targets = super::super::Targets::default();
    for text in [
        r#"{"kind": "text", "path": ["a", "b"]}"#,
        r#"{ "kind": "text", "path": ["a", "b"] }"#,
    ] {
        let read = crate::text_read::binding_of(text, &targets)
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        assert!(
            matches!(read, Binding::Literal(Value::Object(_))),
            "{text}: {read:?}"
        );
    }
}

#[test]
fn a_name_with_a_number_reads_as_the_step_it_names() {
    let mut targets = super::super::Targets::default();
    targets.note_name("file in notes");
    targets.note_name("file in notes 2");
    let read = crate::text_read::binding_of("file in notes 2", &targets)
        .expect("a numbered name is a name");
    assert_eq!(read, Binding::step("file in notes 2", ""), "{read:?}");
}

#[test]
fn a_result_named_after_a_numbered_step_reads_back() {
    let catalog = catalog();
    let shared = Shared {
        catalog: &catalog,
        app_label: &bare,
    };
    let text = "shortcut \"Tidy\"\n\
                input \"folder\" is text, not nothing, always, called \"Where they go\"\n\
                file in notes on input folder\n\
                when: not nothing is set\n\
                \x20 set total to 42\n\
                otherwise:\n\
                \x20 file on any Notes.note in notes\n\
                file in notes on input folder\n\
                result is file in notes 2\n";
    let parsed = crate::text::parse(&shared, text).expect("this text is a program");
    let output = parsed
        .shortcut
        .output
        .expect("the program gives something back");
    // The text names the last of the two lines that read alike, so the result
    // has to be that step's, by the name the numbered phrase gives it.
    assert_eq!(output, Binding::step("file in notes 2", ""), "{output:?}");
}
