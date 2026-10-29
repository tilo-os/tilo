//! A shortcut program as structured data in an Automerge document.
//!
//! [`Shortcut`] is a value: a settled program, as the interpreter reads it,
//! as a program file signs it and as one editor hands it to another.
//! [`write_program`] stores one in a document as the maps and lists it is
//! made of, and [`read_program`] reads it back. An input, a step, a step's
//! argument, a trigger and a grant rule are each their own object, and a
//! step's branch is its own object naming the step that holds it. Two people
//! editing different steps of one shortcut therefore both keep their edit,
//! and a document's history shows the step that changed instead of the whole
//! program put in place again.
//!
//! A write changes only what differs from what the object it writes into
//! already holds. The caller projects the same shortcut round after round, so
//! re-writing a step's whole body on every pass would make every pass a
//! change.
//!
//! Every list is a map of objects keyed by a name that does not change when
//! one of them does, each carrying its place among them, rather than an
//! Automerge list whose elements are replaced whenever one grows a member.
//! Re-ordering is then a write of one `position`, and two people adding to
//! different places each keep their addition.
//!
//! Two versions are written and both are read. [`SHORTCUT_SCHEMA_VERSION`] is
//! the current one, which carries [`Shortcut::version`] and [`Step::label`]. A
//! program without `schema` was stored before either existed and reads as
//! [`LEGACY_SHORTCUT_SCHEMA_VERSION`], its steps unlabelled. A slot holding a
//! byte string is a value stored the way an older build stored it, as the JSON
//! of what belonged there; every read here accepts that, so a document written
//! before this schema keeps everything it held.

use std::collections::BTreeMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

/// The document a program is written into.
pub type Doc = AutoCommit;

use crate::{Binding, Selector, Shortcut, Step, StepKind};
use tilo_actions::ActionKey;
use tilo_actions::params::{ActionParam, ParamType};

/// The version of the stored shortcut program this build writes.
pub const SHORTCUT_SCHEMA_VERSION: u64 = 2;

/// The version a stored program without `schema` reads as.
pub const LEGACY_SHORTCUT_SCHEMA_VERSION: u64 = 1;

/// Key of the schema version a stored program carries.
pub const SCHEMA_KEY: &str = "schema";

/// Key of a member's place in the list it belongs to.
pub const POSITION_KEY: &str = "position";

/// Key of a block naming the step whose branch holds it.
pub const OWNER_KEY: &str = "owner";

/// Key of a block naming which branch of that step it is.
pub const BRANCH_KEY: &str = "branch";

/// Key of the blocks nested anywhere in a program.
pub const BLOCKS_KEY: &str = "blocks";

const STEPS_KEY: &str = "steps";
const THEN: &str = "then";
const ELSE: &str = "else";
const EACH: &str = "each";

fn branches(kind: &StepKind) -> Vec<(&'static str, &[Step])> {
    match kind {
        StepKind::If {
            then, otherwise, ..
        } => vec![(THEN, then), (ELSE, otherwise)],
        StepKind::ForEach { body, .. } => vec![(EACH, body)],
        _ => Vec::new(),
    }
}

/// A name no member of the list already holds.
fn unique(taken: &mut Vec<String>, name: &str) -> String {
    let mut key = name.to_owned();
    let mut n = 1;
    while taken.contains(&key) {
        n += 1;
        key = format!("{name}-{n}");
    }
    taken.push(key.clone());
    key
}

// ---- writing --------------------------------------------------------------

/// Store `shortcut` at `map`, changing only what differs from what the map
/// already holds.
///
/// # Errors
/// Returns the document's error when a member cannot be written.
pub fn write_program(
    doc: &mut Doc,
    map: &ObjId,
    shortcut: &Shortcut,
) -> Result<(), automerge::AutomergeError> {
    doc.put(map, SCHEMA_KEY, ScalarValue::Uint(SHORTCUT_SCHEMA_VERSION))?;
    put_text(doc, map, "id", &shortcut.id)?;
    put_text(doc, map, "name", &shortcut.name)?;
    put_optional_text(
        doc,
        map,
        "version",
        Some(shortcut.version.trim()).filter(|v| !v.is_empty()),
    )?;
    put_optional_text(doc, map, "saved_by", shortcut.saved_by.as_deref())?;
    match shortcut.step_timeout_secs {
        Some(seconds) => doc.put(map, "step_timeout_secs", ScalarValue::Uint(seconds))?,
        None => clear(doc, map, "step_timeout_secs")?,
    }

    let inputs = shortcut
        .inputs
        .iter()
        .map(|input| (input.name.to_string(), input))
        .collect::<Vec<_>>();
    write_list(doc, map, "inputs", &inputs, |doc, entry, input| {
        write_input(doc, entry, input)
    })?;

    write_block(doc, map, STEPS_KEY, &shortcut.steps)?;

    match &shortcut.output {
        Some(output) => put_value(doc, map, "output", output)?,
        None => clear(doc, map, "output")?,
    }

    let triggers = shortcut
        .triggers
        .iter()
        .map(|trigger| (trigger.kind().to_owned(), trigger))
        .collect::<Vec<_>>();
    write_list(doc, map, "triggers", &triggers, |doc, entry, trigger| {
        put_value(doc, entry, "spec", trigger)
    })?;

    let grant = shortcut
        .grant
        .iter()
        .map(|rule| (rule.node.to_string(), rule))
        .collect::<Vec<_>>();
    write_list(doc, map, "grant", &grant, |doc, entry, rule| {
        put_value(doc, entry, "rule", rule)
    })
}

/// Store `steps` as the list at `key` of `map`, and every block inside them
/// among `map`'s [`BLOCKS_KEY`].
fn write_block(
    doc: &mut Doc,
    map: &ObjId,
    key: &str,
    steps: &[Step],
) -> Result<(), automerge::AutomergeError> {
    let placed = steps
        .iter()
        .map(|step| (step.id.clone(), step))
        .collect::<Vec<_>>();
    write_list(doc, map, key, &placed, |doc, entry, step| {
        write_step(doc, entry, step)
    })?;
    let nested = steps
        .iter()
        .flat_map(|step| {
            branches(&step.kind)
                .into_iter()
                .flat_map(move |(branch, inner)| {
                    inner
                        .iter()
                        .map(move |child| (step.id.as_str(), branch, child))
                })
        })
        .map(|(owner, branch, child)| (format!("{owner}/{branch}"), (owner, branch, child)))
        .collect::<Vec<_>>();
    write_list(
        doc,
        map,
        BLOCKS_KEY,
        &nested,
        |doc, entry, (owner, branch, step)| {
            put_text(doc, entry, OWNER_KEY, owner)?;
            put_text(doc, entry, BRANCH_KEY, branch)?;
            write_step(doc, entry, step)
        },
    )
}

/// Store `step` at `entry`: what it does, its label and its arguments.
fn write_step(doc: &mut Doc, entry: &ObjId, step: &Step) -> Result<(), automerge::AutomergeError> {
    let label = step
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    put_text(doc, entry, "id", &step.id)?;
    put_optional_text(doc, entry, "label", label)?;
    if step.on_error.is_fail() {
        clear(doc, entry, "on_error")?;
    } else {
        put_value(doc, entry, "on_error", &step.on_error)?;
    }
    match &step.kind {
        StepKind::Invoke {
            action,
            resource,
            args,
            appearance,
        } => {
            put_text(doc, entry, "step", "invoke")?;
            let head = map_at(doc, entry, "head")?;
            put_text(doc, &head, "app", &action.app)?;
            put_text(doc, &head, "kind", &action.kind)?;
            put_text(doc, &head, "action", &action.action)?;
            put_value(doc, &head, "resource", resource)?;
            match appearance {
                Some(appearance) => put_value(doc, &head, "appearance", appearance)?,
                None => clear(doc, &head, "appearance")?,
            }
            let args = args
                .iter()
                .map(|(name, binding)| (name.clone(), (name, binding)))
                .collect::<Vec<_>>();
            write_list(doc, entry, "args", &args, |doc, arg, (name, binding)| {
                put_text(doc, arg, "name", name)?;
                put_value(doc, arg, "binding", binding)
            })
        }
        StepKind::If { cond, .. } => {
            put_text(doc, entry, "step", "if")?;
            put_value(doc, entry, "cond", cond)
        }
        StepKind::ForEach { items, item, .. } => {
            put_text(doc, entry, "step", "for_each")?;
            put_value(doc, entry, "items", items)?;
            put_text(doc, entry, "item", item)
        }
        StepKind::SetVar { name, value } => {
            put_text(doc, entry, "step", "set_var")?;
            put_text(doc, entry, "name", name)?;
            put_value(doc, entry, "value", value)
        }
        StepKind::Ask { param } => {
            put_text(doc, entry, "step", "ask")?;
            write_input(doc, entry, param)
        }
        StepKind::Ai {
            prompt,
            output,
            input,
        } => {
            put_text(doc, entry, "step", "ai")?;
            put_text(doc, entry, "prompt", prompt)?;
            match output {
                Some(output) => put_value(doc, entry, "output", output)?,
                None => clear(doc, entry, "output")?,
            }
            match input {
                Some(input) => put_value(doc, entry, "input", input)?,
                None => clear(doc, entry, "input")?,
            }
            Ok(())
        }
        StepKind::Task {
            instruction,
            allow,
            output,
        } => {
            put_text(doc, entry, "step", "task")?;
            put_text(doc, entry, "instruction", instruction)?;
            let allow = allow
                .iter()
                .map(|rule| (rule.node.to_string(), rule))
                .collect::<Vec<_>>();
            write_list(doc, entry, "allow", &allow, |doc, entry, rule| {
                put_value(doc, entry, "rule", rule)
            })?;
            match output {
                Some(output) => put_value(doc, entry, "output", output),
                None => clear(doc, entry, "output"),
            }
        }
    }
}

/// Store `input` as the fields of `entry` that describe one.
fn write_input(
    doc: &mut Doc,
    entry: &ObjId,
    input: &ActionParam,
) -> Result<(), automerge::AutomergeError> {
    put_text(doc, entry, "name", &input.name)?;
    put_text(doc, entry, "type", input.ty.label())?;
    put_text(doc, entry, "description", &input.description)?;
    doc.put(entry, "required", ScalarValue::Boolean(input.required))?;
    doc.put(entry, "nullable", ScalarValue::Boolean(input.nullable))?;
    put_strings(doc, entry, "choices", &input.choices)?;
    match &input.schema {
        Some(schema) => put_value(doc, entry, "schema", schema.as_ref()),
        None => clear(doc, entry, "schema"),
    }
}

/// Make `map[key]` hold exactly `items`, each at its own entry carrying its
/// place, and delete the entries of members that are gone.
fn write_list<T>(
    doc: &mut Doc,
    map: &ObjId,
    key: &str,
    items: &[(String, T)],
    write: impl Fn(&mut Doc, &ObjId, &T) -> Result<(), automerge::AutomergeError>,
) -> Result<(), automerge::AutomergeError> {
    let list = map_at(doc, map, key)?;
    let mut taken = Vec::new();
    for (place, (name, item)) in items.iter().enumerate() {
        let id = unique(&mut taken, name);
        let entry = map_at(doc, &list, &id)?;
        doc.put(&entry, POSITION_KEY, ScalarValue::Uint(place as u64))?;
        write(doc, &entry, item)?;
    }
    let stale: Vec<String> = doc.keys(&list).filter(|key| !taken.contains(key)).collect();
    for key in stale {
        doc.delete(&list, key.as_str())?;
    }
    Ok(())
}

// ---- reading --------------------------------------------------------------

/// The program stored at `map`.
///
/// A map holding a byte string where a program or a value of one should be
/// was written by a build that kept whole values as JSON; it is read as that
/// JSON, so nothing saved that way is lost.
#[must_use]
pub fn read_program(doc: &Doc, map: &ObjId) -> Option<Shortcut> {
    let schema = uint(doc, map, SCHEMA_KEY).unwrap_or(LEGACY_SHORTCUT_SCHEMA_VERSION);
    let _ = schema;
    Some(Shortcut {
        id: text(doc, map, "id").unwrap_or_default(),
        name: text(doc, map, "name").unwrap_or_default(),
        version: text(doc, map, "version").unwrap_or_default(),
        inputs: read_list(doc, map, "inputs", read_input),
        steps: read_steps(doc, map),
        step_timeout_secs: uint(doc, map, "step_timeout_secs"),
        output: value(doc, map, "output"),
        grant: read_list(doc, map, "grant", |doc, entry| value(doc, entry, "rule")),
        triggers: read_list(doc, map, "triggers", |doc, entry| value(doc, entry, "spec")),
        saved_by: text(doc, map, "saved_by"),
    })
}

/// The steps of the program at `map`, each with the blocks inside it.
fn read_steps(doc: &Doc, map: &ObjId) -> Vec<Step> {
    let blocks = map_at_read(doc, map, BLOCKS_KEY);
    read_list(doc, map, STEPS_KEY, |doc, entry| {
        let step = read_step(doc, entry)?;
        Some(with_blocks(doc, blocks.as_ref(), step))
    })
}

/// `step` with every branch it has filled from the blocks among `blocks`.
fn with_blocks(doc: &Doc, blocks: Option<&ObjId>, mut step: Step) -> Step {
    let Some(blocks) = blocks else {
        return step;
    };
    let mut then = Vec::new();
    let mut otherwise = Vec::new();
    let mut body = Vec::new();
    for (_, entry) in objects(doc, blocks) {
        if text(doc, &entry, OWNER_KEY).as_deref() != Some(step.id.as_str()) {
            continue;
        }
        let Some(inner) = read_step(doc, &entry) else {
            continue;
        };
        let inner = with_blocks(doc, Some(blocks), inner);
        match text(doc, &entry, BRANCH_KEY).as_deref() {
            Some(THEN) => then.push(inner),
            Some(ELSE) => otherwise.push(inner),
            Some(EACH) => body.push(inner),
            _ => {}
        }
    }
    match &mut step.kind {
        StepKind::If {
            then: held_then,
            otherwise: held_otherwise,
            ..
        } => {
            *held_then = then;
            *held_otherwise = otherwise;
        }
        StepKind::ForEach { body: held, .. } => *held = body,
        _ => {}
    }
    step
}

/// The step stored at `entry`, without the blocks inside it.
fn read_step(doc: &Doc, entry: &ObjId) -> Option<Step> {
    let id = text(doc, entry, "id")?;
    let label = text(doc, entry, "label");
    let kind = match text(doc, entry, "step")?.as_str() {
        "invoke" => {
            let head = map_at_read(doc, entry, "head")?;
            let args = read_list(doc, entry, "args", |doc, arg| {
                Some((text(doc, arg, "name")?, value(doc, arg, "binding")?))
            })
            .into_iter()
            .collect::<BTreeMap<String, Binding>>();
            StepKind::Invoke {
                action: ActionKey::new(
                    text(doc, &head, "app").unwrap_or_default(),
                    text(doc, &head, "kind").unwrap_or_default(),
                    text(doc, &head, "action").unwrap_or_default(),
                ),
                resource: value(doc, &head, "resource")
                    .unwrap_or_else(|| Selector::FirstOfKind(String::new())),
                args,
                appearance: value(doc, &head, "appearance"),
            }
        }
        "if" => StepKind::If {
            cond: value(doc, entry, "cond")?,
            then: Vec::new(),
            otherwise: Vec::new(),
        },
        "for_each" => StepKind::ForEach {
            items: value(doc, entry, "items")?,
            item: text(doc, entry, "item").unwrap_or_else(|| "item".to_owned()),
            body: Vec::new(),
        },
        "set_var" => StepKind::SetVar {
            name: text(doc, entry, "name")?,
            value: value(doc, entry, "value")?,
        },
        "ask" => StepKind::Ask {
            param: read_input(doc, entry)?,
        },
        "ai" => StepKind::Ai {
            prompt: text(doc, entry, "prompt").unwrap_or_default(),
            output: value(doc, entry, "output"),
            input: value(doc, entry, "input"),
        },
        "task" => StepKind::Task {
            instruction: text(doc, entry, "instruction").unwrap_or_default(),
            allow: read_list(doc, entry, "allow", |doc, rule| value(doc, rule, "rule"))
                .into_iter()
                .collect(),
            output: value(doc, entry, "output"),
        },
        _ => return None,
    };
    Some(Step {
        id,
        on_error: value(doc, entry, "on_error").unwrap_or_default(),
        label,
        kind,
    })
}

/// The input stored at `entry`.
fn read_input(doc: &Doc, entry: &ObjId) -> Option<ActionParam> {
    let name = text(doc, entry, "name")?;
    let ty = param_type(&text(doc, entry, "type")?)?;
    let description = text(doc, entry, "description").unwrap_or_default();
    let required = flag(doc, entry, "required");
    let mut input = match required {
        true => ActionParam::required_owned(name, ty, description),
        false => ActionParam::optional_owned(name, ty, description),
    };
    input.choices = strings(doc, entry, "choices")
        .into_iter()
        .map(From::from)
        .collect();
    input.schema = value(doc, entry, "schema").map(std::sync::Arc::new);
    input.nullable = flag(doc, entry, "nullable");
    Some(input)
}

/// The type [`ParamType::label`] spells as `label`.
#[must_use]
pub fn param_type(label: &str) -> Option<ParamType> {
    [
        ParamType::Text,
        ParamType::Integer,
        ParamType::Number,
        ParamType::Boolean,
        ParamType::Object,
        ParamType::Array,
        ParamType::Resource,
        ParamType::DateTime,
        ParamType::Secret,
    ]
    .into_iter()
    .find(|ty| ty.label() == label)
}

// ---- reading a stored list ------------------------------------------------

/// Every object entry of `list`, by key.
fn objects(doc: &Doc, list: &ObjId) -> Vec<(String, ObjId)> {
    let mut out = Vec::new();
    for key in doc.keys(list) {
        match doc.get(list, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => out.push((key, id)),
            _ => {}
        }
    }
    out
}

/// The members of the list at `key` of `map`, in their place, each read by
/// `read`.
fn read_list<T>(
    doc: &Doc,
    map: &ObjId,
    key: &str,
    read: impl Fn(&Doc, &ObjId) -> Option<T>,
) -> Vec<T> {
    let Some(list) = map_at_read(doc, map, key) else {
        return Vec::new();
    };
    let mut placed = objects(doc, &list)
        .into_iter()
        .map(|(_, entry)| {
            (
                uint(doc, &entry, POSITION_KEY).unwrap_or(0),
                read(doc, &entry),
            )
        })
        .collect::<Vec<_>>();
    placed.sort_by_key(|(place, _)| *place);
    placed.into_iter().filter_map(|(_, item)| item).collect()
}

/// Store `items` at `key` of `map` as one text element each.
fn put_strings(
    doc: &mut Doc,
    map: &ObjId,
    key: &str,
    items: &[impl AsRef<str>],
) -> Result<(), automerge::AutomergeError> {
    let list = list_at(doc, map, key)?;
    for (index, item) in items.iter().enumerate() {
        let scalar = ScalarValue::Str(item.as_ref().to_owned().into());
        let slot = match index < doc.length(&list) {
            true => Slot::Index(&list, index),
            false => Slot::Insert(&list, index),
        };
        put_unless_same(doc, slot, scalar)?;
    }
    while doc.length(&list) > items.len() {
        doc.delete(&list, doc.length(&list) - 1)?;
    }
    Ok(())
}

/// The text list at `key` of `map`.
fn strings(doc: &Doc, map: &ObjId, key: &str) -> Vec<String> {
    match doc.get(map, key).ok().flatten() {
        Some((Value::Object(ObjType::List), list)) => (0..doc.length(&list))
            .filter_map(|index| match doc.get(&list, index).ok().flatten() {
                Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
                    ScalarValue::Str(text) => Some(text.to_string()),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---- members --------------------------------------------------------------

/// The map at `key` of `map`, made when it is not one.
fn map_at(doc: &mut Doc, map: &ObjId, key: &str) -> Result<ObjId, automerge::AutomergeError> {
    if let Some((Value::Object(ObjType::Map), id)) = doc.get(map, key)? {
        return Ok(id);
    }
    doc.put_object(map, key, ObjType::Map)
}

/// The map at `key` of `map`, when it is one.
fn map_at_read(doc: &Doc, map: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(map, key).ok().flatten() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

/// Store `value` at `key` of `map` as text.
fn put_text(
    doc: &mut Doc,
    map: &ObjId,
    key: &str,
    value: &str,
) -> Result<(), automerge::AutomergeError> {
    doc.put(map, key, ScalarValue::Str(value.to_owned().into()))
}

/// Store `value` at `key` of `map` as text, or remove the member when it has
/// none.
fn put_optional_text(
    doc: &mut Doc,
    map: &ObjId,
    key: &str,
    value: Option<&str>,
) -> Result<(), automerge::AutomergeError> {
    match value {
        Some(value) => put_text(doc, map, key, value),
        None => clear(doc, map, key),
    }
}

/// Remove `key` from `map`.
fn clear(doc: &mut Doc, map: &ObjId, key: &str) -> Result<(), automerge::AutomergeError> {
    if doc.get(map, key)?.is_some() {
        doc.delete(map, key)?;
    }
    Ok(())
}

/// Store `value` at `key` of `map` as the maps and lists it is made of.
///
/// Only what differs from what is already stored is written, so storing an
/// unchanged value again leaves the document unchanged: an object already
/// there is kept and its members re-stored over it, a list keeps the elements
/// that are unchanged, and a member that is gone is deleted.
fn put_value<T: serde::Serialize + ?Sized>(
    doc: &mut Doc,
    map: &ObjId,
    key: &str,
    value: &T,
) -> Result<(), automerge::AutomergeError> {
    let Ok(json) = serde_json::to_value(value) else {
        return clear(doc, map, key);
    };
    put_json(doc, map, key, &json)
}

fn put_json(
    doc: &mut Doc,
    map: &ObjId,
    key: &str,
    json: &serde_json::Value,
) -> Result<(), automerge::AutomergeError> {
    match json {
        serde_json::Value::Object(members) => {
            let entry = map_at(doc, map, key)?;
            for (name, member) in members {
                put_json(doc, &entry, name, member)?;
            }
            let stale: Vec<String> = doc
                .keys(&entry)
                .filter(|name| !members.contains_key(name))
                .collect();
            for name in stale {
                doc.delete(&entry, name.as_str())?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            let entry = list_at(doc, map, key)?;
            for (index, item) in items.iter().enumerate() {
                put_json_at(doc, &entry, index, item)?;
            }
            while doc.length(&entry) > items.len() {
                doc.delete(&entry, doc.length(&entry) - 1)?;
            }
            Ok(())
        }
        scalar => {
            let Some(scalar) = scalar_of(scalar) else {
                return clear(doc, map, key);
            };
            put_unless_same(doc, Slot::Key(map, key), scalar)
        }
    }
}

/// Store `json` as the `index`th element of `list`.
fn put_json_at(
    doc: &mut Doc,
    list: &ObjId,
    index: usize,
    json: &serde_json::Value,
) -> Result<(), automerge::AutomergeError> {
    let kind = match json {
        serde_json::Value::Object(_) => Some(ObjType::Map),
        serde_json::Value::Array(_) => Some(ObjType::List),
        _ => None,
    };
    let Some(kind) = kind else {
        let Some(scalar) = scalar_of(json) else {
            return Ok(());
        };
        let slot = match index < doc.length(list) {
            true => Slot::Index(list, index),
            false => Slot::Insert(list, index),
        };
        return put_unless_same(doc, slot, scalar);
    };
    let entry = match object_in(doc, list, index, kind)? {
        Some(entry) => entry,
        None => return Ok(()),
    };
    match json {
        serde_json::Value::Object(members) => {
            for (name, member) in members {
                put_json(doc, &entry, name, member)?;
            }
            let stale: Vec<String> = doc
                .keys(&entry)
                .filter(|name| !members.contains_key(name))
                .collect();
            for name in stale {
                doc.delete(&entry, name.as_str())?;
            }
        }
        serde_json::Value::Array(items) => {
            for (place, item) in items.iter().enumerate() {
                put_json_at(doc, &entry, place, item)?;
            }
            while doc.length(&entry) > items.len() {
                doc.delete(&entry, doc.length(&entry) - 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The map at `key` of `map`, made when it is not one.
fn list_at(doc: &mut Doc, map: &ObjId, key: &str) -> Result<ObjId, automerge::AutomergeError> {
    if let Some((Value::Object(ObjType::List), id)) = doc.get(map, key)? {
        return Ok(id);
    }
    doc.put_object(map, key, ObjType::List)
}

/// The object of `kind` at `index` of `list`, replaced when it holds another
/// shape.
fn object_in(
    doc: &mut Doc,
    list: &ObjId,
    index: usize,
    kind: ObjType,
) -> Result<Option<ObjId>, automerge::AutomergeError> {
    if let Some((Value::Object(found), id)) = doc.get(list, index)?
        && found == kind
    {
        return Ok(Some(id));
    }
    let id = match index < doc.length(list) {
        true => doc.put_object(list, index, kind)?,
        false => doc.insert_object(list, index, kind)?,
    };
    Ok(Some(id))
}

/// Where a value is written: a key of a map, an element of a list, or a new
/// element of one.
enum Slot<'a> {
    Key(&'a ObjId, &'a str),
    Index(&'a ObjId, usize),
    Insert(&'a ObjId, usize),
}

/// Store `scalar` in `slot` unless it already holds that scalar.
fn put_unless_same(
    doc: &mut Doc,
    slot: Slot<'_>,
    scalar: ScalarValue,
) -> Result<(), automerge::AutomergeError> {
    let held = match slot {
        Slot::Key(map, key) => doc.get(map, key)?.map(|(value, _)| value),
        Slot::Index(list, index) => doc.get(list, index)?.map(|(value, _)| value),
        Slot::Insert(..) => None,
    };
    if let Some(Value::Scalar(current)) = held
        && same(current.as_ref(), &scalar)
    {
        return Ok(());
    }
    match slot {
        Slot::Key(map, key) => doc.put(map, key, scalar)?,
        Slot::Index(list, index) => doc.put(list, index, scalar)?,
        Slot::Insert(list, index) => doc.insert(list, index, scalar)?,
    }
    Ok(())
}

fn same(current: &ScalarValue, next: &ScalarValue) -> bool {
    match (current, next) {
        (ScalarValue::F64(left), ScalarValue::F64(right)) => left.to_bits() == right.to_bits(),
        (left, right) => left == right,
    }
}

/// The scalar `json` is stored as.
fn scalar_of(json: &serde_json::Value) -> Option<ScalarValue> {
    Some(match json {
        serde_json::Value::Null => ScalarValue::Null,
        serde_json::Value::Bool(flag) => ScalarValue::Boolean(*flag),
        serde_json::Value::String(text) => ScalarValue::Str(text.clone().into()),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(int) => ScalarValue::Int(int),
            None => match number.as_u64() {
                Some(uint) => ScalarValue::Uint(uint),
                None => ScalarValue::F64(number.as_f64()?),
            },
        },
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => return None,
    })
}

/// `key` of `map` as text.
fn text(doc: &Doc, map: &ObjId, key: &str) -> Option<String> {
    match doc.get(map, key).ok().flatten() {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Str(value) => Some(value.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// `key` of `map` as a whole number.
fn uint(doc: &Doc, map: &ObjId, key: &str) -> Option<u64> {
    match doc.get(map, key).ok().flatten() {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Uint(value) => Some(*value),
            ScalarValue::Int(value) => u64::try_from(*value).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `key` of `map` holds `true`.
fn flag(doc: &Doc, map: &ObjId, key: &str) -> bool {
    matches!(
        doc.get(map, key).ok().flatten(),
        Some((Value::Scalar(scalar), _)) if matches!(scalar.as_ref(), ScalarValue::Boolean(true))
    )
}

/// `key` of `map` read back as the value it was stored as.
///
/// A member holding a byte string was written by a build that kept whole
/// values as JSON; it is read back as that JSON, so a document written before
/// this schema is not lost.
fn value<T: serde::de::DeserializeOwned>(doc: &Doc, map: &ObjId, key: &str) -> Option<T> {
    let (value, id) = doc.get(map, key).ok().flatten()?;
    serde_json::from_value(json_of(doc, &value, &id)?).ok()
}

/// The JSON `value` (the object `id`) holds.
fn json_of(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Option<serde_json::Value> {
    match value {
        Value::Scalar(scalar) => match scalar.as_ref() {
            ScalarValue::Bytes(bytes) => serde_json::from_slice(bytes).ok(),
            ScalarValue::Null => Some(serde_json::Value::Null),
            ScalarValue::Boolean(flag) => Some(serde_json::Value::Bool(*flag)),
            ScalarValue::Str(text) => Some(serde_json::Value::String(text.to_string())),
            ScalarValue::Int(int) | ScalarValue::Timestamp(int) => Some((*int).into()),
            ScalarValue::Uint(uint) => Some((*uint).into()),
            ScalarValue::Counter(counter) => Some(i64::from(counter.clone()).into()),
            ScalarValue::F64(float) => {
                serde_json::Number::from_f64(*float).map(serde_json::Value::Number)
            }
            ScalarValue::Unknown { .. } => None,
        },
        Value::Object(ObjType::Map | ObjType::Table) => {
            let mut fields = serde_json::Map::new();
            for key in doc.keys(id) {
                let (value, inner) = doc.get(id, key.as_str()).ok().flatten()?;
                fields.insert(key, json_of(doc, &value, &inner)?);
            }
            Some(serde_json::Value::Object(fields))
        }
        Value::Object(ObjType::List) => {
            let mut items = Vec::new();
            for index in 0..doc.length(id) {
                let (value, inner) = doc.get(id, index).ok().flatten()?;
                items.push(json_of(doc, &value, &inner)?);
            }
            Some(serde_json::Value::Array(items))
        }
        Value::Object(_) => None,
    }
}
