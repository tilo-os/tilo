//! Staged state for screenshots.
//!
//! Apps start empty, so a capture of an app's screen has to do what a person would have done
//! first: add a few notes, type a list, hold a conversation. A capture names a scenario, and the
//! window plays that scenario's steps as it would play a person's: actions invoked in the
//! runtime as a person or an agent, text typed into the bound inputs, buttons pressed. Every
//! change is committed through the action door like any other. Nothing here runs outside a
//! capture; the live path never reads a scenario.

use std::collections::VecDeque;

use tilolang_tilo::runtime::store::{ChangeNote, Mutation, Store as _};
use tilolang_tilo::runtime::{InvokeOutcome, Principal, ResourceRef, Value};

use super::TilolangPage;
use crate::reconcile::UiEvent;

/// One step of a scenario.
pub(super) type Step = Box<dyn FnOnce(&mut TilolangPage) -> Result<(), String>>;

/// The scenarios a capture can name.
pub const SCENARIOS: &[&str] = &[
    "counter",
    "todo",
    "notes",
    "notes-detail",
    "letters",
    "letters-detail",
    "chat",
    "chat-detail",
    "board",
];

const COUNTER: &str = "so.tilo.counter";
const TODO: &str = "so.tilo.todo";
const NOTES: &str = "so.tilo.notes";
const LETTERS: &str = "so.tilo.letters";
const CHAT: &str = "so.tilo.chat";
const BOARD: &str = "so.tilo.board";

/// An agent task's principal: it posts into conversations like a person does.
const AGENT: &str = "agent:planner";

fn step(run: impl FnOnce(&mut TilolangPage) -> Result<(), String> + 'static) -> Step {
    Box::new(run)
}

fn open(app: &'static str) -> Step {
    step(move |page| {
        page.open(app);
        Ok(())
    })
}

/// Press the first button of the open view with this label.
fn press(label: &'static str) -> Step {
    step(move |page| {
        let tree = page.body_tree.clone().ok_or("no view is open")?;
        let key = find_button(&tree, label).ok_or_else(|| format!("no button `{label}`"))?;
        page.events.borrow_mut().push(UiEvent::Press(key));
        Ok(())
    })
}

fn find_button(node: &tilolang_tilo::runtime::UiNode, label: &str) -> Option<String> {
    if node.kind == "button" && node.props.get("label") == Some(&Value::text(label)) {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| find_button(child, label))
}

/// Run the action `name` of `app` as `who`.
fn call(
    page: &mut TilolangPage,
    who: &str,
    app: &str,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    let host = page.current_host().ok_or("no host")?;
    let package = host
        .package(app)
        .ok_or_else(|| format!("no package {app}"))?;
    let action = package
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .ok_or_else(|| format!("no action {name} in {app}"))?;
    let outcome = host
        .runtime()
        .invoke_action(&Principal::new(who), (action.id.as_str(), args))
        .map_err(|error| error.to_string())?;
    page.commit_planned();
    match outcome {
        InvokeOutcome::Done(value) => Ok(value),
        other => Err(format!("{name} did not run: {other:?}")),
    }
}

fn reference(value: Value) -> Result<ResourceRef, String> {
    match value {
        Value::Ref(reference) => Ok(reference.target),
        other => Err(format!("not a reference: {other:?}")),
    }
}

fn root(page: &TilolangPage, app: &str) -> Result<ResourceRef, String> {
    page.current_host()
        .ok_or("no host")?
        .root_of(app)
        .map_err(|error| error.to_string())
}

fn items(page: &TilolangPage, at: &ResourceRef, field: &str) -> Result<Vec<ResourceRef>, String> {
    let host = page.current_host().ok_or("no host")?;
    match host
        .runtime()
        .read(at, field)
        .map_err(|error| error.to_string())?
    {
        Value::List(values) => values.into_iter().map(reference).collect(),
        other => Err(format!("not a list: {other:?}")),
    }
}

/// Type `text` into the field `field` of `at`, as a person typing in its bound input does.
fn type_into(
    page: &mut TilolangPage,
    at: &ResourceRef,
    field: &str,
    text: &str,
) -> Result<(), String> {
    let host = page.current_host().ok_or("no host")?;
    let package = host
        .packages()
        .into_iter()
        .find(|package| {
            package
                .meta
                .types
                .iter()
                .any(|declared| declared.id == at.ty)
        })
        .ok_or("the resource's package is not loaded")?;
    let id = package
        .meta
        .types
        .iter()
        .find(|declared| declared.id == at.ty)
        .and_then(|declared| declared.fields.iter().find(|f| f.name == field))
        .map(|f| f.id.clone())
        .ok_or_else(|| format!("no field {field}"))?;
    let mut store = host.store().clone();
    let txn = store.begin().map_err(|error| error.to_string())?;
    store
        .apply(
            txn,
            at,
            &Mutation::SetText {
                field: id,
                text: text.to_owned(),
            },
        )
        .map_err(|error| error.to_string())?;
    let note = ChangeNote {
        action: "$edit".to_owned(),
        args: serde_json::Value::Null,
        invoker: page.me.clone(),
        chain: Vec::new(),
        module: String::new(),
        approved_by: None,
    };
    store
        .commit(txn, &note)
        .map_err(|error| error.to_string())?;
    page.commit_planned();
    Ok(())
}

fn say(
    page: &mut TilolangPage,
    who: &str,
    conversation: &ResourceRef,
    text: &str,
    block: &str,
) -> Result<(), String> {
    call(
        page,
        who,
        CHAT,
        "post",
        vec![
            Value::resource(conversation),
            Value::text(text),
            Value::text(block),
        ],
    )
    .map(|_| ())
}

fn note(page: &mut TilolangPage, title: &str, body: &str) -> Result<ResourceRef, String> {
    let notebook = root(page, NOTES)?;
    let note = reference(call(
        page,
        &page.me.clone(),
        NOTES,
        "new_note",
        vec![Value::resource(&notebook)],
    )?)?;
    call(
        page,
        &page.me.clone(),
        NOTES,
        "retitle",
        vec![Value::resource(&note), Value::text(title)],
    )?;
    type_into(page, &note, "body", body)?;
    Ok(note)
}

fn todo_items(page: &mut TilolangPage, titles: &[&str], done: &[usize]) -> Result<(), String> {
    let list = root(page, TODO)?;
    let me = page.me.clone();
    for title in titles {
        call(
            page,
            &me,
            TODO,
            "add",
            vec![Value::resource(&list), Value::text(*title)],
        )?;
    }
    let all = items(page, &list, "items")?;
    for index in done {
        if let Some(item) = all.get(*index) {
            call(page, &me, TODO, "toggle", vec![Value::resource(item)])?;
        }
    }
    Ok(())
}

/// The steps of the scenario `name`.
#[allow(clippy::too_many_lines, reason = "one table of the staged scenarios")]
pub(super) fn script(name: &str) -> VecDeque<Step> {
    let mut steps: Vec<Step> = Vec::new();
    match name {
        "counter" => {
            steps.push(open(COUNTER));
            for _ in 0..7 {
                steps.push(press("+1"));
            }
        }
        "todo" => {
            steps.push(step(|page| {
                let list = root(page, TODO)?;
                let me = page.me.clone();
                call(
                    page,
                    &me,
                    TODO,
                    "rename",
                    vec![Value::resource(&list), Value::text("Saturday errands")],
                )
                .map(|_| ())
            }));
            steps.push(step(|page| {
                todo_items(
                    page,
                    &[
                        "Buy oat milk and bread",
                        "Return the library books",
                        "Call Maria about the studio",
                        "Pick up the printed tickets",
                        "Water the plants",
                    ],
                    &[1, 4],
                )
            }));
            steps.push(open(TODO));
        }
        "notes" | "notes-detail" => {
            steps.push(step(|page| {
                note(
                    page,
                    "Groceries",
                    "Oat milk, bread, lemons, the good olive oil and something green for dinner.",
                )
                .map(|_| ())
            }));
            steps.push(step(|page| {
                note(
                    page,
                    "Launch talk ideas",
                    "Start with the two-minute demo. Keep the slides to a handful of images and finish on the question the audience keeps asking.",
                )
                .map(|_| ())
            }));
            steps.push(step(|page| {
                let first = note(
                    page,
                    "Floor plan feedback",
                    "Ask Maria whether the studio is free on the 14th, and send Tomas the revised plan.",
                )?;
                let me = page.me.clone();
                call(page, &me, NOTES, "toggle_pin", vec![Value::resource(&first)]).map(|_| ())
            }));
            steps.push(open(NOTES));
        }
        "letters" | "letters-detail" => {
            steps.push(step(|page| {
                let desk = root(page, LETTERS)?;
                let me = page.me.clone();
                let draft = reference(call(page, &me, LETTERS, "new_draft", vec![Value::resource(&desk)])?)?;
                type_into(page, &draft, "to", "tomas@example.org")?;
                type_into(page, &draft, "subject", "The revised floor plan")?;
                type_into(
                    page,
                    &draft,
                    "body",
                    "Hi Tomas, the revised plan is attached. The studio is free on the 14th, so we can walk through it together in the morning.",
                )?;
                let older = reference(call(page, &me, LETTERS, "new_draft", vec![Value::resource(&desk)])?)?;
                type_into(page, &older, "to", "maria@example.org")?;
                type_into(page, &older, "subject", "Studio on the 14th")?;
                Ok(())
            }));
            steps.push(open(LETTERS));
        }
        "chat" | "chat-detail" => {
            steps.push(step(|page| {
                let me = page.me.clone();
                // The note and the draft the agent will point at.
                let packing = note(
                    page,
                    "Packing list",
                    "Passport, charger, the blue notebook.",
                )?;
                let desk = root(page, LETTERS)?;
                let draft = reference(call(
                    page,
                    &me,
                    LETTERS,
                    "new_draft",
                    vec![Value::resource(&desk)],
                )?)?;
                type_into(page, &draft, "to", "maria@example.org")?;
                type_into(page, &draft, "subject", "Lisbon on the 14th")?;
                type_into(
                    page,
                    &draft,
                    "body",
                    "Hi Maria, I land at nine. Could you meet me at the station?",
                )?;
                let chat = root(page, CHAT)?;
                let conversation = reference(call(
                    page,
                    &me,
                    CHAT,
                    "start",
                    vec![Value::resource(&chat), Value::text("Trip to Lisbon")],
                )?)?;
                say(
                    page,
                    &me,
                    &conversation,
                    "Can you help me get ready for Lisbon?",
                    "",
                )?;
                say(
                    page,
                    AGENT,
                    &conversation,
                    "Here is the packing list you started.",
                    &format!("notes.note_editor(@notes.Note:{})", packing.id_string()),
                )?;
                say(
                    page,
                    AGENT,
                    &conversation,
                    "I drafted a note to Maria. Shall I send it?",
                    &format!("letters.send(@letters.Draft:{})", draft.id_string()),
                )?;
                say(page, &me, &conversation, "Yes, please.", "")
            }));
            steps.push(open(CHAT));
        }
        "board" => {
            steps.push(step(|page| {
                let me = page.me.clone();
                todo_items(
                    page,
                    &["Review the floor plan", "Book the train", "Send the letter"],
                    &[0],
                )?;
                note(page, "Morning pages", "Three things before coffee.")?;
                note(
                    page,
                    "Reading list",
                    "The Pragmatic Programmer, Designing Data-Intensive Applications.",
                )?;
                let chat = root(page, CHAT)?;
                call(
                    page,
                    &me,
                    CHAT,
                    "start",
                    vec![Value::resource(&chat), Value::text("Trip to Lisbon")],
                )?;
                call(
                    page,
                    &me,
                    CHAT,
                    "start",
                    vec![Value::resource(&chat), Value::text("Studio plans")],
                )?;
                let board = root(page, BOARD)?;
                let notebook = root(page, NOTES)?;
                let list = root(page, TODO)?;
                call(
                    page,
                    &me,
                    BOARD,
                    "attach",
                    vec![
                        Value::resource(&board),
                        Value::resource(&notebook),
                        Value::resource(&list),
                        Value::resource(&chat),
                    ],
                )
                .map(|_| ())
            }));
            steps.push(open(BOARD));
        }
        _ => {}
    }
    // What a person does from a list: open an item, which pushes its own window.
    let shown = match name {
        "notes-detail" => Some((NOTES, "notes", usize::MAX)),
        "letters-detail" => Some((LETTERS, "drafts", 0)),
        "chat-detail" => Some((CHAT, "conversations", 0)),
        _ => None,
    };
    if let Some((app, field, index)) = shown {
        steps.push(step(move |page| {
            let list = root(page, app)?;
            let all = items(page, &list, field)?;
            let item = if index == usize::MAX {
                all.last()
            } else {
                all.get(index)
            };
            let item = item.ok_or("nothing to open")?;
            page.open_resource(item, None);
            Ok(())
        }));
    }
    steps.into()
}

impl TilolangPage {
    /// Play every step of the scenario, in order. The runtime sees each step's changes at once
    /// (its store answers from the changes it planned), and the window commits them through the
    /// door over the frames that follow, in the order they were planned.
    pub(super) fn play_stage(&mut self) -> bool {
        // The host exists once the first frame synced the record.
        if !self.started {
            return !self.stage.is_empty();
        }
        let mut played = false;
        while let Some(next) = self.stage.pop_front() {
            played = true;
            if let Err(message) = next(self) {
                tracing::error!(%message, "a staged step failed");
                self.stage.clear();
            }
        }
        played
    }
}
