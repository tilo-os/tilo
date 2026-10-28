//! Reading the values a shortcut's text writes.
//!
//! A binding, a condition and an input's declaration are read by the same
//! rules that wrote them ([`super::text`]), so `parse(render(program))` is the
//! program: every rule a person can read in a `TextParseError` is a rule this
//! module states once. Text the module cannot read is refused with the words
//! for what to write instead, never guessed at.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Number, Value};
use tilo_actions::params::{ActionParam, ParamType};

use crate::shortcut::{Binding, Condition, Selector, Shortcut, Step, StepKind};
use crate::template::is_identifier;

use super::text::{MAX_TEXT_STEPS, Shared, TYPES, Targets, prefixes, text_at, type_of};

/// A value read from the start of `text`, and what follows it.
pub(crate) fn binding_and_rest(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Binding, String), String> {
    let text = text.trim_start();
    if text.starts_with('"') {
        let (words, rest) = text_at(text)?;
        return Ok((template_of(shared, targets, words)?, rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix('[') {
        return list_at(shared, rest, targets);
    }
    if let Some(rest) = text.strip_prefix('{') {
        // A dictionary's entries are `"name": value`, so a brace that opens
        // anything else holds one value — the text a template's placeholder
        // stands for — and that value is what the braces enclose.
        if rest.trim_start().starts_with('"') {
            return object_at(shared, rest, targets);
        }
        let (value, after) = binding_and_rest(shared, rest, targets)?;
        let after = after
            .trim_start()
            .strip_prefix('}')
            .ok_or_else(|| "close the value's brace".to_owned())?;
        return Ok((value, after.to_owned()));
    }
    for word in prefixes(text).into_iter().rev() {
        if let Some(binding) = named_binding(word, targets) {
            let after = text.get(word.len()..).unwrap_or_default();
            return Ok((binding, after.to_owned()));
        }
    }
    if let Some(rest) = text.strip_prefix("nothing") {
        return Ok((Binding::Literal(Value::Null), rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix("yes") {
        return Ok((Binding::Literal(Value::Bool(true)), rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix("no") {
        return Ok((Binding::Literal(Value::Bool(false)), rest.to_owned()));
    }
    if let Some((value, rest)) = number_at(text) {
        return Ok((Binding::Literal(value), rest.to_owned()));
    }
    Err(format!(
        "`{}` is not a value: write a quoted text, a number, `yes`, `no`, `nothing`, a list, a dictionary, or a name the steps above set",
        text.trim()
    ))
}

/// A value read from a whole line.
pub(crate) fn binding_of(text: &str, targets: &Targets) -> Result<Binding, String> {
    let (binding, rest) = binding_and_rest(&Shared::empty(), text, targets)?;
    if !rest.trim().is_empty() {
        return Err(format!("`{}` follows the value", rest.trim()));
    }
    Ok(binding)
}

/// A name a step above bound: `input NAME`, `variable NAME`, or the phrase a
/// step reads as, which names its result.
pub(crate) fn named_binding(word: &str, targets: &Targets) -> Option<Binding> {
    for (at, ch) in word.char_indices().rev() {
        let after = at.saturating_add(ch.len_utf8());
        let Some((name, pointer)) = word.get(..after).zip(word.get(after..)) else {
            continue;
        };
        let found = targets
            .value_of(name)
            .cloned()
            .or_else(|| named_value(name));
        let Some(without) = found else {
            continue;
        };
        if pointer.is_empty() {
            return Some(without);
        }
        let Some(rest) = pointer_of(pointer) else {
            continue;
        };
        return Some(match without {
            Binding::StepOutput { step, .. } => Binding::StepOutput {
                step,
                pointer: rest,
            },
            Binding::Var(name) => Binding::Var(format!("{name}{rest}")),
            other => other,
        });
    }
    None
}

/// The value an `input` or a variable name on its own stands for.
pub(crate) fn named_value(name: &str) -> Option<Binding> {
    if let Some(given) = name.strip_prefix("input ") {
        let (base, pointer) = Binding::named_part(given);
        return is_identifier(base).then(|| Binding::Input(format!("{base}{pointer}")));
    }
    let given = name.strip_prefix("variable ")?;
    let (base, pointer) = Binding::named_part(given);
    is_identifier(base).then(|| Binding::Var(format!("{base}{pointer}")))
}

/// A pointer back from the tokens the text writes it as.
pub(crate) fn pointer_of(text: &str) -> Option<String> {
    let mut pointer = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', ' ']).unwrap_or(after.len());
            let (field, after) = after.split_at(end);
            if field.is_empty() {
                return None;
            }
            pointer.push('/');
            pointer.push_str(&field.replace('~', "~0").replace('/', "~1"));
            rest = after;
        } else {
            let after = rest.strip_prefix(" item ")?;
            let end = after.find(['.', ' ']).unwrap_or(after.len());
            let (index, after) = after.split_at(end);
            pointer.push('/');
            pointer.push_str(index);
            rest = after;
        }
    }
    Some(pointer)
}

pub(crate) fn number_at(text: &str) -> Option<(Value, &str)> {
    let end = text
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E'))
        .map(|(index, ch)| index.saturating_add(ch.len_utf8()))
        .last()?;
    let (head, rest) = text.split_at(end);
    let number: Number = head.parse().ok()?;
    Some((Value::Number(number), rest))
}

pub(crate) fn list_at(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Binding, String), String> {
    let mut items = Vec::new();
    let mut owned = text.trim_start().to_owned();
    loop {
        owned = owned.trim_start().to_owned();
        let rest = owned.as_str();
        if let Some(after) = rest.strip_prefix(']') {
            // A list of values the text states is the value it states: the
            // document holds it as one, so reading one back does not rewrite
            // what it is to say the same thing.
            return Ok((folded_list(items), after.to_owned()));
        }
        let (item, after) = binding_and_rest(shared, rest, targets)?;
        items.push(item);
        // The last value is followed by the bracket that closes the list, not
        // by a comma.
        let tail = after.trim_start();
        let next = match tail.strip_prefix(',') {
            Some(next) => next,
            None if tail.starts_with(']') => tail,
            None => return Err("separate a list's values with commas".to_owned()),
        };
        owned.clear();
        owned.push_str(next);
    }
}

/// A list of values as the binding it is: one literal value where every member
/// is written out, and a list of bindings where any member is not.
fn folded_list(items: Vec<Binding>) -> Binding {
    let members: Option<Vec<Value>> = items
        .iter()
        .map(|item| match item {
            Binding::Literal(value) => Some(value.clone()),
            _ => None,
        })
        .collect();
    members.map_or(Binding::List(items), |members| {
        Binding::Literal(Value::Array(members))
    })
}

/// A dictionary of values as the binding it is, on the same rule as a list.
fn folded_object(members: BTreeMap<String, Binding>) -> Binding {
    let stated: Option<serde_json::Map<String, Value>> = members
        .iter()
        .map(|(name, member)| match member {
            Binding::Literal(value) => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect();
    stated.map_or(Binding::Object(members), |stated| {
        Binding::Literal(Value::Object(stated))
    })
}

pub(crate) fn object_at(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Binding, String), String> {
    let mut members: BTreeMap<String, Binding> = BTreeMap::new();
    let mut owned = text.trim_start().to_owned();
    loop {
        owned = owned.trim_start().to_owned();
        let rest = owned.as_str();
        if let Some(after) = rest.strip_prefix('}') {
            return Ok((folded_object(members), after.to_owned()));
        }
        let (name, after) = text_at(rest)?;
        let after = after
            .trim_start()
            .strip_prefix(':')
            .ok_or_else(|| {
                format!("write a dictionary's entries as `\"name\": value`, at `{rest}`")
            })?
            .trim_start();
        let (value, after) = binding_and_rest(shared, after, targets)?;
        members.insert(name, value);
        // The last entry is followed by the brace that closes the dictionary,
        // not by a comma.
        let tail = after.trim_start();
        let next = match tail.strip_prefix(',') {
            Some(next) => next,
            None if tail.starts_with('}') => tail,
            None => return Err("separate a dictionary's entries with commas".to_owned()),
        };
        owned.clear();
        owned.push_str(next);
    }
}

/// A quoted text as the binding it is: a plain text, or a template when it
/// holds `{...}` placeholders.
fn template_of(shared: &Shared<'_>, targets: &Targets, words: String) -> Result<Binding, String> {
    if !words.contains('{') {
        return Ok(Binding::Literal(Value::String(words)));
    }
    Ok(Binding::Template(placeholders_of(shared, targets, &words)?))
}

/// A text a template holds, with each `{{value}}` written as the placeholder
/// the document holds: `{{step.BASE/pointer}}`, `{{input NAME}}`,
/// `{{var NAME}}`. A step is named by the phrase its line reads with, and
/// [`rebind`] points the placeholder at the step's id.
fn placeholders_of(shared: &Shared<'_>, targets: &Targets, text: &str) -> Result<String, String> {
    let mut written = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("{{") {
        written.push_str(before);
        let Some((inner, tail)) = after.split_once("}}") else {
            return Err("close the value's `{{` with `}}`".to_owned());
        };
        let (binding, following) = binding_and_rest(shared, inner.trim(), targets)?;
        if !following.trim().is_empty() {
            return Err(format!("`{}` follows the value", following.trim()));
        }
        let placeholder = match &binding {
            Binding::Input(name) => format!("{{{{input {name}}}}}"),
            Binding::Var(name) => format!("{{{{var {name}}}}}"),
            Binding::StepOutput { step, pointer } => {
                // A template holds the JSON pointer as it is, so the
                // placeholder reads back as the same value.
                format!("{{{{step.{step}{pointer}}}}}")
            }
            // A placeholder stands for a value a step above gave, which is the
            // only thing a template's braces hold.
            _ => {
                return Err(format!(
                    "`{}` is not something a text can insert: write an input, a variable or a step's result",
                    inner.trim()
                ));
            }
        };
        written.push_str(&placeholder);
        rest = tail;
    }
    written.push_str(rest);
    Ok(written)
}

/// A condition read from a whole line.
pub(crate) fn condition_of(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<Condition, String> {
    let (condition, rest) = condition_or_rest(shared, text, targets, 0)?;
    if !rest.trim().is_empty() {
        return Err(format!("`{}` follows the condition", rest.trim()));
    }
    Ok(condition)
}

/// A condition read from the start of `text`, and what follows it. `and`
/// binds tighter than `or`, and parentheses group.
pub(crate) fn condition_and_rest(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
    depth: usize,
) -> Result<(Condition, String), String> {
    if depth > MAX_TEXT_STEPS {
        return Err("this condition nests too deeply".to_owned());
    }
    let text = text.trim_start();
    let (condition, rest) = if let Some(inner) = text.strip_prefix("not ") {
        let below = depth.saturating_add(1);
        let (inner, rest) = condition_and_rest(shared, inner, targets, below)?;
        (Condition::Not(Box::new(inner)), rest)
    } else if let Some(inner) = text.strip_prefix('(') {
        let below = depth.saturating_add(1);
        let (inner, rest) = condition_or_rest(shared, inner, targets, below)?;
        let rest = rest
            .trim_start()
            .strip_prefix(')')
            .ok_or_else(|| "close the condition's parenthesis".to_owned())?
            .to_owned();
        (inner, rest)
    } else {
        reading_of(shared, text, targets)?
    };
    let mut all = vec![condition];
    let mut rest = rest;
    while let Some(after) = rest.trim_start().strip_prefix("and ") {
        let (next, tail) = reading_of(shared, after, targets)?;
        all.push(next);
        rest = tail;
    }
    if all.len() == 1 {
        return Ok((all.remove(0), rest));
    }
    Ok((Condition::All(all), rest))
}

pub(crate) fn condition_or_rest(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
    depth: usize,
) -> Result<(Condition, String), String> {
    let (first, mut rest) = condition_and_rest(shared, text, targets, depth)?;
    let mut any = vec![first];
    while let Some(after) = rest.trim_start().strip_prefix("or ") {
        let (next, tail) = condition_and_rest(shared, after, targets, depth)?;
        any.push(next);
        rest = tail;
    }
    if any.len() == 1 {
        return Ok((any.remove(0), rest));
    }
    Ok((Condition::Any(any), rest))
}

/// The reading a condition states: a value, then how it reads.
pub(crate) fn reading_of(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Condition, String), String> {
    let (value, rest) = binding_and_rest(shared, text, targets)?;
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix("is set") {
        return Ok((Condition::Truthy(value), after.to_owned()));
    }
    if let Some(after) = rest.strip_prefix("is not ") {
        let (other, tail) = binding_and_rest(shared, after, targets)?;
        return Ok((
            Condition::Not(Box::new(Condition::Equals {
                left: value,
                right: other,
            })),
            tail,
        ));
    }
    if let Some(after) = rest.strip_prefix("is ") {
        if after.starts_with("greater than ") || after.starts_with("less than ") {
            return Err("this reading is not one the text writes yet".to_owned());
        }
        let (other, tail) = binding_and_rest(shared, after, targets)?;
        return Ok((
            Condition::Equals {
                left: value,
                right: other,
            },
            tail,
        ));
    }
    Err("a condition reads `value is value`, `value is set`, `not`, `and` or `or`".to_owned())
}

/// An input's declaration.
pub(crate) fn input_of(text: &str) -> Result<ActionParam, String> {
    let (name, rest) = text_at(text)?;
    let rest = rest.trim_start().strip_prefix("is ").ok_or_else(|| {
        "write an input as `input \"name\" is text, not nothing, called \"what it is for\"`"
            .to_owned()
    })?;
    let mut words = rest.trim_start();
    let mut ty: Option<&str> = None;
    for kind in TYPES {
        if let Some(after) = words.strip_prefix(kind)
            && (after.is_empty() || after.starts_with(' ') || after.starts_with(','))
            && ty.is_none_or(|chosen| kind.len() > chosen.len())
        {
            ty = Some(kind);
        }
    }
    let Some(kind) = ty else {
        return Err(format!(
            "`{}` is not a type: a value is one of {}",
            words.split(' ').next().unwrap_or_default(),
            TYPES.join(", ")
        ));
    };
    words = words.get(kind.len()..).unwrap_or_default().trim_start();
    // Two facts a value has beside its type, in the order the line writes
    // them: whether the value may be nothing, and whether the shortcut needs
    // one at all.
    let mut nullable = true;
    let mut required = true;
    if let Some(after) = words.strip_prefix(',') {
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix("not nothing") {
            nullable = false;
            words = after.trim_start();
        } else if let Some(after) = after.strip_prefix("may be nothing") {
            words = after.trim_start();
        }
    }
    if let Some(after) = words.strip_prefix(',') {
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix("always") {
            words = after.trim_start();
        } else if let Some(after) = after.strip_prefix("if given") {
            required = false;
            words = after.trim_start();
        }
    }
    let mut param = ActionParam::required_owned(name, type_of(kind).unwrap_or(ParamType::Text), "");
    param.nullable = nullable;
    param.required = required;
    if words.is_empty() {
        return Ok(param);
    }
    let words = words
        .strip_prefix(',')
        .map(str::trim_start)
        .ok_or_else(|| {
            format!("say what the input is for after a comma, as `called \"…\"`, not `{words}`")
        })?;
    let words = words.strip_prefix("called ").unwrap_or(words);
    let (description, choices) = match words.split_once(", one of ") {
        Some((description, listed)) => (description, choices_of(listed)?),
        None => (words, Vec::new()),
    };
    // The words are what the person wrote on the line, quotation marks and
    // all: what the input is called is what they hold inside them.
    let (description, after) = text_at(description.trim())?;
    if !after.trim().is_empty() {
        return Err(format!(
            "`{}` follows what the input is called",
            after.trim()
        ));
    }
    param.description = description.into();
    param.choices = choices.into_iter().map(Into::into).collect();
    param.nullable = nullable;
    Ok(param)
}

/// The values a declaration lists as the only ones it accepts.
pub(crate) fn choices_of(text: &str) -> Result<Vec<String>, String> {
    let mut choices = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let (choice, after) = text_at(rest)?;
        choices.push(choice);
        rest = after.trim_start();
        rest = rest
            .strip_prefix(" or ")
            .or_else(|| rest.strip_prefix(','))
            .unwrap_or(rest)
            .trim_start();
    }
    Ok(choices)
}

// ------------------------------------------------------------- rebinding ---

/// Point every reference a program holds at the step it names.
///
/// The text names a step by the line it reads with, because a program written
/// by hand has no ids to write. The document names a step by its id, so a
/// reference the text wrote as a phrase is rewritten to the id of the step that
/// phrase reads as. A reference to a step the text no longer states keeps its
/// phrase: it reaches no step, which reports itself as a broken link rather
/// than quietly acting on a different one.
pub(crate) fn rebind(shortcut: &mut Shortcut, names: &BTreeMap<String, String>) {
    let bound = bound_names(&shortcut.steps);
    if let Some(output) = &mut shortcut.output {
        retarget(output, names, &bound);
    }
    for binding in bindings_of(&mut shortcut.steps) {
        retarget(binding, names, &bound);
    }
}

/// Every name a step in the program makes a variable out of: a set step's
/// variable and a loop's item.
fn bound_names(steps: &[Step]) -> BTreeSet<String> {
    let mut bound = BTreeSet::new();
    for step in steps {
        match &step.kind {
            StepKind::SetVar { name, .. } => {
                bound.insert(name.clone());
            }
            StepKind::ForEach { item, body, .. } => {
                bound.insert(item.clone());
                bound.extend(bound_names(body));
            }
            StepKind::If {
                then, otherwise, ..
            } => {
                bound.extend(bound_names(then));
                bound.extend(bound_names(otherwise));
            }
            StepKind::Invoke { .. }
            | StepKind::Ask { .. }
            | StepKind::Ai { .. }
            | StepKind::Task { .. } => {}
        }
    }
    bound
}

/// A template's text, with each placeholder's step named by its id.
fn pointed(text: &str, names: &BTreeMap<String, String>) -> String {
    let mut written = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("{{") {
        written.push_str(before);
        let Some((inner, tail)) = after.split_once("}}") else {
            written.push_str(rest);
            return written;
        };
        if let Some(name) = inner.strip_prefix("step.") {
            let (base, pointer) = Binding::named_part(name);
            written.push_str("{{step.");
            written.push_str(names.get(base).map_or(base, String::as_str));
            written.push_str(&pointer);
            written.push_str("}}");
        } else {
            // An input's or a variable's placeholder is already the name the
            // document holds it under.
            written.push_str("{{");
            written.push_str(inner);
            written.push_str("}}");
        }
        rest = tail;
    }
    written.push_str(rest);
    written
}

/// One binding, with the step it names pointed at that step's id.
fn retarget(
    binding: &mut Binding,
    names: &BTreeMap<String, String>,
    bound_names: &BTreeSet<String>,
) {
    match binding {
        Binding::StepOutput { step, .. } => {
            if let Some(id) = names.get(step.as_str()) {
                id.clone_into(step);
            }
        }
        Binding::Object(members) => {
            for member in members.values_mut() {
                retarget(member, names, bound_names);
            }
        }
        Binding::List(items) => {
            for item in items {
                retarget(item, names, bound_names);
            }
        }
        // A variable a set step or a loop's item makes is the program's own,
        // so it stays the variable the text wrote; a name that no such step
        // makes is the words a step's result is called by, and what the text
        // named by those words points at the step that holds the result.
        Binding::Var(name) => {
            let (base, pointer) = Binding::named_part(name);
            if bound_names.contains(base) {
                return;
            }
            if let Some(id) = names.get(base) {
                *binding = Binding::StepOutput {
                    step: id.clone(),
                    pointer,
                };
            }
        }
        // A template's placeholders name their values as the rest of the line
        // does, so each one is pointed at the step it means.
        Binding::Template(text) => *text = pointed(text, names),
        Binding::Literal(_) | Binding::Input(_) => {}
    }
}

/// Every binding a step and its blocks hold.
fn bindings_of(steps: &mut [Step]) -> Vec<&mut Binding> {
    let mut out = Vec::new();
    for step in steps {
        match &mut step.kind {
            StepKind::Invoke { args, resource, .. } => {
                if let Selector::FromBinding(binding) = resource {
                    out.push(binding);
                }
                out.extend(args.values_mut());
            }
            StepKind::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                out.append(&mut condition_bindings(cond));
                out.append(&mut bindings_of(then));
                out.append(&mut bindings_of(otherwise));
            }
            StepKind::ForEach { items, body, .. } => {
                out.push(items);
                out.append(&mut bindings_of(body));
            }
            StepKind::SetVar { value, .. } => out.push(value),
            StepKind::Ai { input, .. } => out.extend(input.as_mut()),
            StepKind::Ask { .. } | StepKind::Task { .. } => {}
        }
    }
    out
}

fn condition_bindings(cond: &mut Condition) -> Vec<&mut Binding> {
    let mut out = Vec::new();
    match cond {
        Condition::Truthy(binding) => out.push(binding),
        Condition::Equals { left, right } => {
            out.push(left);
            out.push(right);
        }
        Condition::Not(inner) => out.append(&mut condition_bindings(inner)),
        Condition::All(parts) | Condition::Any(parts) => {
            for part in parts {
                out.append(&mut condition_bindings(part));
            }
        }
    }
    out
}
