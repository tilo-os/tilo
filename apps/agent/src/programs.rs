//! The `tilo` tool: the task works through tilolang programs (docs/designs/app-language/
//! agents.md, "One tool, programs, sessions").
//!
//! The model sends a program; what comes back is tilolang, and output pastes back as input. Each
//! call is one step of the task, recorded on its issue as a [`ProgramStep`] that the composer and
//! Tasks draw: the program as code, the resources the answer names as references that open, what
//! it changed as rows that undo, and what it would change as a preview.
//!
//! The session's door runs every task's programs ([`tilo_actions::remote::task_programs`]): the
//! loop hands each program to it as a step of its run, and the door runs it in the task's own
//! tilolang host, as the agent for its person, the same way it runs an MCP client's `tilo` calls.
//! This module only says what the model is offered and reads back what the door answered.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use agent_provider_config::protocol::ToolDefinition;
use serde_json::{Value, json};
use ui::tilo_reference::{
    ProgramStep, ResourceAt, ResourceRef, StepChange, StepOutcome, StepPreview,
};

use tilo_actions::remote::task_programs::ProgramAnswer;

use crate::surface::{Catalogue, Offer};

/// The tool's name.
pub const TILO: &str = "tilo";
/// The resource every tilolang host publishes its agent operations on.
pub const HOST_RESOURCE: &str = "tl:host";
/// The host operation a program runs through.
pub const RUN_BLOCK: &str = "run-block";
/// The host operation that lists a host's declarations.
pub const CATALOGUE: &str = "catalogue";
/// The kind a suggested invocation is named by in a [`StepPreview`]: its id is the host's own,
/// which its `resolve` takes.
pub const INVOCATION_KIND: &str = "tl.invocation";
/// The most characters of a program the tool takes.
const MOST_PROGRAM_CHARS: usize = 4_000;

/// The tool's definition. Byte-stable across turns and tasks, like every loop tool.
#[must_use]
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: TILO.into(),
        description: ("Run a tilolang program over the apps listed under \"Apps\" and get its \
             answer, in tilolang. Each call is one step of the task that the person sees and \
             can undo. An answer names each resource by a short reference at the start of its \
             line (`t3`), which later programs take as it is: `tasks.check(t3)`. `this` is \
             what the person pointed at. Read with `find`, `grep`, `describe` and fields; \
             change things by calling actions or assigning fields. A program that would \
             change many things or destroy something answers with a preview; send it again \
             with `confirm` once the person's request asks for it. `undo` takes back the last \
             step, `undo task` every step, `history task` lists them. A program that does not \
             compile answers with what is valid.")
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "program": {
                    "type": "string",
                    "description": "The program: tilolang statements, one per line, such \
                        as `find(tasks.Task, done: false)`."
                },
                "confirm": {
                    "type": "boolean",
                    "description": "true only to run again a program whose answer was a \
                        preview, unchanged, once the person's request asks for it."
                }
            },
            "required": ["program"],
            "additionalProperties": false
        }),
    }
}

/// A `tilo` call's input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub source: String,
    pub confirm: bool,
}

/// Read a `tilo` call.
///
/// # Errors
/// In words for the model: no program, or one too long.
pub fn read_program(input: &Value) -> Result<Program, String> {
    let source = input
        .get("program")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .ok_or("`program` is required: the tilolang program to run")?;
    if source.chars().count() > MOST_PROGRAM_CHARS {
        return Err(format!(
            "the program is longer than {MOST_PROGRAM_CHARS} characters; split it into steps"
        ));
    }
    Ok(Program {
        source: source.to_owned(),
        confirm: input.get("confirm").and_then(Value::as_bool) == Some(true),
    })
}

/// A tilolang app the task can reach: its host's operations as the catalogue offers them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mounted {
    /// The app's id (`so.tilo.lang.board`).
    pub app: String,
    /// How programs name it (`board`).
    pub alias: String,
    /// Its title, as its application offer names it.
    pub title: String,
    /// Its host's `run-block`, by operation id.
    pub run_block: String,
    /// Its host's `catalogue`, by operation id, when offered.
    pub catalogue: Option<String>,
}

/// How a program names an app: the last part of its id, as the compiler imports it.
#[must_use]
pub fn alias_of(app: &str) -> &str {
    app.rsplit('.').next().unwrap_or(app)
}

/// Every tilolang app of `catalogue` whose host the task may run programs on.
#[must_use]
pub fn mounted(catalogue: &Catalogue) -> Vec<Mounted> {
    let host_op = |offer: &Offer, action: &str| {
        offer.descriptor.address.resource == HOST_RESOURCE
            && offer.descriptor.address.action == action
    };
    let mut apps: BTreeMap<String, Mounted> = BTreeMap::new();
    for offer in catalogue.offers.iter().filter(|o| host_op(o, RUN_BLOCK)) {
        apps.entry(offer.app.clone()).or_insert_with(|| Mounted {
            app: offer.app.clone(),
            alias: alias_of(&offer.app).to_owned(),
            title: title_of(catalogue, &offer.app),
            run_block: offer.id(),
            catalogue: catalogue
                .offers
                .iter()
                .find(|o| o.app == offer.app && host_op(o, CATALOGUE))
                .map(Offer::id),
        });
    }
    apps.into_values().collect()
}

fn title_of(catalogue: &Catalogue, app: &str) -> String {
    catalogue
        .offers
        .iter()
        .find(|offer| {
            offer.app == app && offer.descriptor.resource_kind == crate::surface::APPLICATION_KIND
        })
        .map_or_else(
            || {
                let alias = alias_of(app);
                ui::format::capitalize_first(alias)
            },
            |offer| offer.descriptor.resource_label.clone(),
        )
}

/// The types each app declares, from a host's `catalogue` answer: what a projected record's
/// type name is looked up in, to name the app that keeps it.
#[must_use]
pub fn types_of(catalogue: &Value) -> BTreeMap<String, String> {
    let mut types = BTreeMap::new();
    for signature in catalogue.as_array().into_iter().flatten() {
        let (Some(app), Some(name), Some(text)) = (
            signature.get("app").and_then(Value::as_str),
            signature.get("name").and_then(Value::as_str),
            signature.get("signature").and_then(Value::as_str),
        ) else {
            continue;
        };
        if text.trim_start().starts_with("type ") {
            types
                .entry(name.to_owned())
                .or_insert_with(|| app.to_owned());
        }
    }
    types
}

/// One line per app for the session's first message: the app, how programs name it, and its
/// types, roots and actions as its host declares them.
#[must_use]
pub fn app_lines(mounted: &[Mounted], catalogue: &Value) -> String {
    let mut out = String::new();
    for app in mounted {
        let mut types = Vec::new();
        let mut actions = Vec::new();
        let mut roots = Vec::new();
        for signature in catalogue.as_array().into_iter().flatten() {
            if signature.get("app").and_then(Value::as_str) != Some(app.app.as_str()) {
                continue;
            }
            let name = signature.get("name").and_then(Value::as_str).unwrap_or("");
            let text = signature
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_start();
            if text.starts_with("type ") {
                types.push(name.to_owned());
            } else if text.starts_with("root ") {
                roots.push(text.lines().next().unwrap_or(text).to_owned());
            } else if text.contains("action ") {
                actions.push(name.to_owned());
            }
        }
        let _ = write!(out, "- `{}`: {}", app.alias, app.title);
        if !roots.is_empty() {
            let _ = write!(out, "; {}", roots.join(", "));
        }
        if !types.is_empty() {
            let _ = write!(out, "; types {}", types.join(", "));
        }
        if !actions.is_empty() {
            let _ = write!(out, "; actions {}", actions.join(", "));
        }
        out.push('\n');
    }
    out
}

/// What naming a resource of `app` in an answer needs: its type name's app (a host mounting
/// several apps answers records of each), so the reference opens in the app that keeps it.
pub struct Naming<'a> {
    /// The app whose host answered.
    pub host: &'a Mounted,
    pub mounted: &'a [Mounted],
    /// Type name to app, as the hosts declare them.
    pub types: &'a BTreeMap<String, String>,
}

impl Naming<'_> {
    fn app_of(&self, type_name: &str) -> &str {
        self.types
            .get(type_name)
            .map_or(self.host.app.as_str(), String::as_str)
    }

    fn alias_of<'s>(&'s self, app: &'s str) -> &'s str {
        self.mounted
            .iter()
            .find(|m| m.app == app)
            .map_or_else(|| alias_of(app), |m| m.alias.as_str())
    }

    /// A projected record as a reference that opens it, and as its literal.
    fn reference(&self, record: &Value) -> Option<(ResourceAt, String)> {
        let type_name = record.get("type")?.as_str()?;
        let id = record.get("id")?.as_str()?;
        let label = record
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(type_name);
        let app = self.app_of(type_name);
        let literal = format!("@{}.{type_name}:{id}", self.alias_of(app));
        Some((
            ResourceAt {
                app: app.to_owned(),
                record: None,
                reference: ResourceRef::whole(
                    format!("tl.{app}.{type_name}"),
                    format!("tl:{app}:{id}"),
                    label,
                ),
            },
            literal,
        ))
    }
}

/// The step a program was, from what its host answered (`Err`: the invocation failed, with the
/// host's words).
#[must_use]
pub fn step_of(source: &str, naming: &Naming<'_>, answered: Result<&Value, &str>) -> ProgramStep {
    let mut step = ProgramStep {
        program: source.to_owned(),
        ..ProgramStep::default()
    };
    let value = match answered {
        Ok(value) => value,
        Err(error) => {
            let (outcome, lines) = match error.split_once("the block does not compile:") {
                Some((_, diagnostics)) => (StepOutcome::Rejected, diagnostics.trim()),
                None => (StepOutcome::Failed, error.trim()),
            };
            step.outcome = outcome;
            step.answer = lines.lines().map(|line| format!("// {line}")).collect();
            return step.bounded();
        }
    };
    match value.get("kind").and_then(Value::as_str) {
        Some("data") => {
            let shown = value.get("value").cloned().unwrap_or(Value::Null);
            show(&shown, naming, &mut step, 0);
        }
        Some("invocation") => invocation(value, naming, &mut step),
        Some("view") => {
            step.answer
                .push("// a view: answer with it by finishing with `view`".to_owned());
            if let Some(shows) = value.pointer("/definition/shows").and_then(Value::as_str) {
                step.answer.push(format!("// it shows a {shows}"));
            }
        }
        _ => {
            step.answer.push(format!("// {value}"));
        }
    }
    if step.answer.is_empty() {
        step.answer.push("none".to_owned());
    }
    step.bounded()
}

fn invocation(value: &Value, naming: &Naming<'_>, step: &mut ProgramStep) {
    let state = value.get("state").and_then(Value::as_str).unwrap_or("");
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let app = value
        .get("app")
        .and_then(Value::as_str)
        .unwrap_or(naming.host.app.as_str())
        .to_owned();
    let result = value.get("result").cloned().unwrap_or(Value::Null);
    match state {
        "done" => {
            step.answer.push(format!("// {action}: done"));
            let before = step.refs.len();
            show(&result, naming, step, 0);
            let changed: Vec<ResourceAt> = step.refs.iter().skip(before).cloned().collect();
            if changed.is_empty() {
                // An action that returns nothing changed the resources it was given; the
                // program names them.
                step.changes.push(StepChange {
                    resource: ResourceAt {
                        app: app.clone(),
                        record: None,
                        reference: ResourceRef::whole(
                            format!("tl.{app}"),
                            format!("tl:{app}"),
                            &action,
                        ),
                    },
                    action,
                });
            } else {
                for resource in changed {
                    step.changes.push(StepChange {
                        resource,
                        action: action.clone(),
                    });
                }
            }
        }
        "suggested" | "pending" => {
            step.outcome = StepOutcome::Asked;
            step.answer
                .push(format!("// {action}: waits for the person to approve it"));
            if let Some(id) = value.get("invocation").and_then(Value::as_str) {
                step.previews.push(StepPreview {
                    invocation: ResourceAt {
                        app,
                        record: None,
                        reference: ResourceRef::whole(INVOCATION_KIND, id, &action),
                    },
                    action,
                    summary: String::new(),
                });
            }
        }
        "declined" | "denied" => {
            step.outcome = StepOutcome::Failed;
            step.answer
                .push(format!("// {action}: {state}; do not try it again"));
        }
        other => {
            step.outcome = StepOutcome::Failed;
            step.answer.push(format!("// {action}: {other}"));
            show(&result, naming, step, 0);
        }
    }
}

/// `value`, a host's projection, as answer lines: a record as its reference literal, label and
/// fields; a list as one line per item after its count; anything else as itself.
fn show(value: &Value, naming: &Naming<'_>, step: &mut ProgramStep, depth: usize) {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        let count = value
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(items.len() as u64);
        if depth == 0 {
            let more = if value.get("truncated").and_then(Value::as_bool) == Some(true) {
                format!(", the first {} shown", items.len())
            } else {
                String::new()
            };
            step.answer.push(format!("// {count} items{more}"));
        }
        for item in items {
            show(item, naming, step, depth + 1);
        }
        return;
    }
    if let Some((resource, literal)) = naming.reference(value) {
        let mut line = literal;
        let label = &resource.reference.label;
        let _ = write!(line, " {label:?}");
        if let Some(fields) = value.get("fields").and_then(Value::as_object) {
            for (name, field) in fields {
                if let Some(inline) = inline(field, naming) {
                    let _ = write!(line, " {name}: {inline}");
                }
            }
        }
        if !step.refs.contains(&resource) {
            step.refs.push(resource);
        }
        step.answer.push(line);
        return;
    }
    step.answer
        .push(inline(value, naming).unwrap_or_else(|| value.to_string()));
}

/// A field's value on one line, or `None` for one not worth a line's room (a collection's
/// count, a label already shown).
fn inline(value: &Value, naming: &Naming<'_>) -> Option<String> {
    Some(match value {
        Value::Null => "none".to_owned(),
        Value::String(text) => format!("{text:?}"),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Object(map) if map.contains_key("type") && map.contains_key("id") => {
            let (resource, literal) = naming.reference(value)?;
            format!("{literal} {:?}", resource.reference.label)
        }
        Value::Object(map) if map.len() == 1 && map.contains_key("count") => return None,
        Value::Object(map) if map.contains_key("variant") => map
            .get("variant")
            .and_then(Value::as_str)
            .map(|variant| format!(".{variant}"))?,
        other => other.to_string(),
    })
}

/// The resource a literal names (`@tasks.Task:root/items:x`), labelled `label`: its app by how
/// programs name it in `mounted`.
fn literal_resource(literal: &str, label: &str, mounted: &[Mounted]) -> Option<ResourceAt> {
    let (path, id) = literal.strip_prefix('@')?.split_once(':')?;
    let (alias, type_name) = path.rsplit_once('.')?;
    let app = mounted
        .iter()
        .find(|m| m.alias == alias)
        .map_or_else(|| format!("so.tilo.lang.{alias}"), |m| m.app.clone());
    Some(ResourceAt {
        reference: ResourceRef::whole(
            format!("tl.{app}.{type_name}"),
            format!("tl:{app}:{id}"),
            if label.is_empty() { type_name } else { label },
        ),
        app,
        record: None,
    })
}

/// The title the answer shows after the reference `shown` at the start of a line
/// (`t3 "Record the demo video" ...`), if it shows one.
fn shown_title(text: &str, shown: &str) -> String {
    text.lines()
        .find_map(|line| {
            let rest = line.trim_start().strip_prefix(shown)?;
            let rest = rest.strip_prefix(' ')?.strip_prefix('"')?;
            let mut title = String::new();
            let mut escaped = false;
            for c in rest.chars() {
                match c {
                    _ if escaped => {
                        title.push(c);
                        escaped = false;
                    }
                    '\\' => escaped = true,
                    '"' => return Some(title),
                    _ => title.push(c),
                }
            }
            None
        })
        .unwrap_or_default()
}

/// The step a program was, from what the door answered: the door's text as its answer, the
/// resources it shows as references, what it changed as rows that undo, and what a preview
/// would change once it is confirmed.
#[must_use]
pub fn door_step(source: &str, answered: &ProgramAnswer, mounted: &[Mounted]) -> ProgramStep {
    let mut step = ProgramStep {
        program: source.to_owned(),
        answer: answered.text.lines().map(str::to_owned).collect(),
        ..ProgramStep::default()
    };
    for (shown, literal) in &answered.refs {
        let title = shown_title(&answered.text, shown);
        if let Some(resource) = literal_resource(literal, &title, mounted)
            && !step.refs.contains(&resource)
        {
            step.refs.push(resource);
        }
    }
    let answer = answered.answer.as_ref();
    let state = answer
        .and_then(|answer| answer.get("state"))
        .and_then(Value::as_str);
    step.outcome = match state {
        Some("done") | None if !answered.failed => StepOutcome::Done,
        Some("preview") => StepOutcome::Preview,
        Some("suspended") => StepOutcome::Asked,
        Some("rejected") => StepOutcome::Rejected,
        _ => StepOutcome::Failed,
    };
    let planned = answer
        .and_then(|answer| answer.get("steps"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let changed = answer
        .and_then(|answer| answer.get("changed"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let known = step.refs.clone();
    let resource_of = |app: &str, id: &str, action: &str| {
        known
            .iter()
            .find(|known| known.reference.id == id)
            .cloned()
            .unwrap_or_else(|| ResourceAt {
                app: app.to_owned(),
                record: None,
                reference: ResourceRef::whole(format!("tl.{app}"), id, action),
            })
    };
    for planned in &planned {
        let app = planned
            .get("app")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let action = planned
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // The resource an action is about is the first it names.
        let Some(id) = planned
            .get("resources")
            .and_then(Value::as_array)
            .and_then(|resources| resources.first())
            .and_then(Value::as_str)
        else {
            continue;
        };
        let resource = resource_of(app, id, &action);
        match step.outcome {
            StepOutcome::Done if changed > 0 => step.changes.push(StepChange { resource, action }),
            StepOutcome::Preview => step.planned.push(StepChange { resource, action }),
            _ => {}
        }
    }
    step.bounded()
}

/// What the model reads of a step: its answer as tilolang, led by how it ended when it did not
/// simply run.
#[must_use]
pub fn observation(step: &ProgramStep) -> String {
    let mut out = String::new();
    match step.outcome {
        StepOutcome::Done => {}
        StepOutcome::Rejected => out.push_str("// did not compile; nothing ran\n"),
        StepOutcome::Preview => out.push_str(
            "// a preview: nothing ran; the person confirms, or you send the same program with \
             `confirm` when the request asks for it\n",
        ),
        StepOutcome::Asked => out.push_str("// asked the person; their answer comes later\n"),
        StepOutcome::Failed => out.push_str("// failed\n"),
    }
    out.push_str(&step.answer.join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Vec<Mounted> {
        [
            "so.tilo.lang.board",
            "so.tilo.lang.todo",
            "so.tilo.lang.views",
        ]
        .iter()
        .map(|app| Mounted {
            app: (*app).to_owned(),
            alias: alias_of(app).to_owned(),
            title: ui::format::capitalize_first(alias_of(app)),
            run_block: format!("broker:1:{app}/run-block"),
            catalogue: None,
        })
        .collect()
    }

    #[test]
    fn a_list_answers_one_line_per_record_led_by_its_reference() {
        let mounted = board();
        let types = BTreeMap::from([("Card".to_owned(), "so.tilo.lang.board".to_owned())]);
        let naming = Naming {
            host: &mounted[0],
            mounted: &mounted,
            types: &types,
        };
        let answered = json!({"kind": "data", "value": {"count": 2, "items": [
            {"type": "Card", "id": "root/cards:c1", "label": "Write the release notes",
             "fields": {"title": "Write the release notes",
                        "column": {"type": "Column", "id": "root/columns:k1", "label": "To do"}}},
            {"type": "Card", "id": "root/cards:c2", "label": "Screenshots for the docs",
             "fields": {"title": "Screenshots for the docs"}}
        ]}});
        let step = step_of("board.cards", &naming, Ok(&answered));
        assert_eq!(step.outcome, StepOutcome::Done);
        assert_eq!(step.answer[0], "// 2 items");
        assert_eq!(
            step.answer[1],
            "@board.Card:root/cards:c1 \"Write the release notes\" title: \"Write the release \
             notes\" column: @board.Column:root/columns:k1 \"To do\""
        );
        assert_eq!(step.refs.len(), 2);
        assert_eq!(
            step.refs[0].reference.id,
            "tl:so.tilo.lang.board:root/cards:c1"
        );
        assert_eq!(step.refs[0].reference.kind, "tl.so.tilo.lang.board.Card");
    }

    #[test]
    fn a_block_that_does_not_compile_answers_its_diagnostics_and_an_ask_previews() {
        let mounted = board();
        let types = BTreeMap::new();
        let naming = Naming {
            host: &mounted[1],
            mounted: &mounted,
            types: &types,
        };
        let step = step_of(
            "todo.nope",
            &naming,
            Err(
                "the block does not compile:\n1:6: E0101 `todo` has no `nope`; did you mean `todos`?",
            ),
        );
        assert_eq!(step.outcome, StepOutcome::Rejected);
        assert!(observation(&step).contains("did you mean `todos`"));
        let asked = json!({"kind": "invocation", "state": "suggested", "action": "Remove",
            "app": "so.tilo.lang.todo", "result": null, "invocation": "root/invocations:i1"});
        let step = step_of("todo.remove(...)", &naming, Ok(&asked));
        assert_eq!(step.outcome, StepOutcome::Asked);
        assert_eq!(
            step.previews[0].invocation.reference.id,
            "root/invocations:i1"
        );
        assert_eq!(step.previews[0].action, "Remove");
    }
}
