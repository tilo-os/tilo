//! The recorded run, as the draft of a Shortcut.
//!
//! A recorded invocation names the page element it reached by a ref of the
//! page it ran on (`d1e5`); a replay runs on a page nobody has looked at
//! yet, so every step that reached an element becomes a find step: it names
//! the element the way the Browser's [`FIND_ELEMENT`] names one (role, name,
//! and which of the equals), and its answer is what the action that follows
//! acts on ([`Selector::FromBinding`] and a binding of the find's output).
//!
//! Three rules keep the draft honest:
//!
//! - Nothing is invented. A step is the invocation that succeeded, the
//!   element description the action's own answer gave, or the value a
//!   recorded argument held — never a guess at what a page "should" say.
//! - A value equal to a named input becomes [`Binding::Input`], so the
//!   same shortcut replays on other data. What no input names stays the
//!   constant it was, which is what lets the person see it before saving.
//! - What was only looked at is dropped: an action that reads and whose
//!   answer nothing used is exploration, not work.
//!
//! The draft ends on a check: the last recorded look at the page, kept as
//! its own step, so a replay that reaches a page unlike the one recorded
//! says so instead of reporting success for a run of actions that each
//! answered.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use tilo_automation::{Binding, Selector, Shortcut, Step, StepKind};

use super::Recorded;

/// The app whose published action opens a draft: the same one the person
/// uses, so nothing enters the library but through Shortcuts' own review.
pub const SHORTCUTS_APP: &str = tilo_automation::SHORTCUTS_APP_ID;
/// The Shortcuts resource that opens a program as an unsaved draft.
pub const DRAFT_RESOURCE: &str = "shortcuts.workbench";
/// The action on [`DRAFT_RESOURCE`] that opens a draft.
pub const DRAFT_ACTION: &str = "draft-shortcut";

/// The Browser's app id, whose page actions act on elements a replay must
/// find again.
pub const BROWSER_APP: &str = "so.tilo.browser";
/// The Browser resource actions act on: the page.
pub const BROWSER_PAGE: &str = "browser.page";
/// The Browser's action that names an element of the page, so a step can
/// act on it after a reload replaced every ref of the old one.
pub const FIND_ELEMENT: &str = "find-page-element";
/// Why `entries` cannot become a Shortcut: nothing in them does anything a
/// replay would repeat.
pub const NOTHING_DONE: &str = "nothing in this run changed anything a shortcut could replay";

/// The recorded run as a Shortcut program, ready to be opened as a draft.
///
/// `inputs` names the values the person supplies on each replay: an argument
/// whose value equals an input's value is bound to that input, so the same
/// run repeats on other data. `name` is the person's name for it, in their
/// words.
///
/// # Errors
///
/// When the run recorded nothing that changes anything: a run of reads
/// replays as nothing, and a shortcut that does nothing is not what the
/// person asked for.
pub fn draft(name: &str, entries: &[Recorded], inputs: &[Value]) -> Result<Shortcut, String> {
    let mut builder = Builder::default();
    let mut work = false;
    for entry in entries {
        work |= builder.add(entry, inputs)?;
    }
    if !work {
        return Err(NOTHING_DONE.to_owned());
    }
    // The replay ends on the run's own last look at where it got to: a step
    // that fails there says the page is not the one this run recorded. A
    // read that a later step needed is already a step of its own.
    if let Some(last) = entries
        .last()
        .filter(|entry| entry.reads() && entry.value.is_some())
    {
        builder.keep(last, inputs)?;
    }
    builder.into_shortcut(name, inputs)
}

/// The draft as the argument of the Shortcuts action that opens it.
#[must_use]
pub fn argument(shortcut: &Shortcut) -> Value {
    json!({"shortcut": shortcut})
}

/// What the draft does, in the order the run recorded it, for the person to
/// read before saving.
#[must_use]
pub fn summary(entries: &[Recorded]) -> String {
    let mut lines = Vec::new();
    for entry in entries.iter().filter(|entry| !entry.reads()) {
        match entry.element() {
            Some(element) => lines.push(format!("{} {}", entry.described(), describe(&element))),
            None => lines.push(entry.described()),
        }
    }
    lines.join("; ")
}

/// An element description as a person reads it.
fn describe(element: &Value) -> String {
    let named = |key: &str| {
        element
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    };
    match (named("role"), named("name")) {
        (Some(role), Some(name)) => format!("`{name}` ({role})"),
        (Some(role), None) => format!("the {role}"),
        (None, Some(name)) => format!("`{name}`"),
        (None, None) => "an element the page named".to_owned(),
    }
}

/// An element a step has to find before it acts: the step whose output is
/// the find's answer.
#[derive(Clone, Debug)]
struct Found {
    step: String,
}

/// The steps, built one recorded invocation at a time.
#[derive(Default)]
struct Builder {
    steps: Vec<Step>,
    /// The element found so far, by the resource it is on and the element a
    /// step described: a run that fills a field twice finds it once.
    found: BTreeMap<(String, String), Found>,
    /// Step ids taken.
    ids: BTreeSet<String>,
}

impl Builder {
    /// Add `entry` as a step, finding the element it acts on first.
    ///
    /// Returns whether the step does something a replay repeats: a read is
    /// exploration and adds no step, unless [`Self::keep`] is asked for it
    /// directly, as the run's last look.
    fn add(&mut self, entry: &Recorded, inputs: &[Value]) -> Result<bool, String> {
        if entry.reads() {
            // Exploration: what it read is worth keeping as the run's last
            // look, and nothing else.
            return Ok(false);
        }
        self.keep(entry, inputs)
    }

    /// [`Self::add`], without dropping a read: the step is in the program.
    fn keep(&mut self, entry: &Recorded, inputs: &[Value]) -> Result<bool, String> {
        let Some(key) = entry.key() else {
            // An action whose app the session did not learn cannot be
            // invoked from a program: its key names the app.
            return Ok(false);
        };
        if entry.reads() && entry.value.is_none() {
            // A look that said nothing is no check either.
            return Ok(false);
        }
        let mut resource = Selector::Fixed(entry.resource.clone());
        if let (Some((_element, frame)), Some(described)) = (reach(entry), entry.element()) {
            let identified = (entry.resource.clone(), described.to_string());
            let found = if let Some(found) = self.found.get(&identified) {
                found.clone()
            } else {
                let found = self.find_step(entry, &described, frame, inputs)?;
                self.found.insert(identified, found.clone());
                found
            };
            resource = Selector::FromBinding(Binding::step(found.step, ""));
        }
        // Every element ref the invocation used is the find's answer: the
        // page's own refs mean nothing on the page a replay opens.
        let mut args = BTreeMap::new();
        for (name, value) in &entry.arguments {
            if value.as_str().is_some_and(is_ref) {
                continue;
            }
            args.insert(name.to_owned(), bind(value, inputs));
        }
        let id = self.id_for(entry);
        self.steps.push(Step::new(
            id,
            StepKind::Invoke {
                action: key,
                resource,
                args,
                appearance: None,
            },
        ));
        Ok(true)
    }

    /// The step that names the element `entry` acted on, added before it.
    fn find_step(
        &mut self,
        entry: &Recorded,
        described: &Value,
        frame: Option<Binding>,
        inputs: &[Value],
    ) -> Result<Found, String> {
        let fields = described
            .as_object()
            .ok_or("the Browser named no element it acted on")?;
        let mut args = BTreeMap::new();
        for name in ["role", "name", "nth"] {
            if let Some(value) = fields.get(name).filter(|value| !value.is_null()) {
                args.insert(name.to_owned(), bind(value, inputs));
            }
        }
        // A ref read in a frame of its own is found in that frame: a find
        // of any element there takes the frame's id, or the outline's
        // `[frame=1]` for the one the page itself is.
        if let Some(frame) = frame {
            args.insert("frame".to_owned(), frame);
        }
        let id = self.id_for(entry);
        self.steps.push(Step::new(
            id.clone(),
            StepKind::Invoke {
                action: tilo_actions::ActionKey::new(BROWSER_APP, BROWSER_PAGE, FIND_ELEMENT),
                resource: Selector::Fixed(entry.resource.clone()),
                args,
                appearance: None,
            },
        ));
        Ok(Found { step: id })
    }

    /// A step id for `entry` that no other step has: the action's own name,
    /// then `-2`, `-3`, … for a run that took the same action twice.
    fn id_for(&mut self, entry: &Recorded) -> String {
        let base: String = entry
            .action
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let base = if base.is_empty() {
            "step".to_owned()
        } else {
            base
        };
        let mut candidate = base.clone();
        let mut n: usize = 2;
        while !self.ids.insert(candidate.clone()) {
            candidate = format!("{base}-{n}");
            n = n.saturating_add(1);
        }
        candidate
    }

    /// The program, with `name` and `inputs`.
    fn into_shortcut(self, name: &str, inputs: &[Value]) -> Result<Shortcut, String> {
        let mut declared = Vec::new();
        for input in inputs {
            let Some(fields) = input.as_object() else {
                return Err("an input is named by its value".to_owned());
            };
            let Some(name) = fields.get("name").and_then(Value::as_str) else {
                return Err("an input has no name".to_owned());
            };
            let ty = fields.get("type").and_then(Value::as_str).unwrap_or("text");
            let ty = [
                tilo_actions::params::ParamType::Text,
                tilo_actions::params::ParamType::Integer,
                tilo_actions::params::ParamType::Number,
                tilo_actions::params::ParamType::Boolean,
                tilo_actions::params::ParamType::DateTime,
                tilo_actions::params::ParamType::Resource,
            ]
            .into_iter()
            .find(|kind| kind.label() == ty)
            .ok_or_else(|| format!("`{ty}` is not a parameter type an input may hold"))?;
            declared.push(tilo_actions::params::ActionParam::optional_owned(
                name.to_owned(),
                ty,
                name.to_owned(),
            ));
        }
        Ok(Shortcut {
            id: String::new(),
            name: name.to_owned(),
            inputs: declared,
            steps: self.steps,
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
            saved_proof: None,
        })
    }
}

/// The element ref one recorded invocation's arguments hold, with the frame
/// it was read in.
fn reach(entry: &Recorded) -> Option<(String, Option<Binding>)> {
    let frame = entry.arguments.get("frame").and_then(Value::as_str);
    let frame = frame.map(|frame| Binding::Literal(Value::String(frame.to_owned())));
    entry
        .arguments
        .iter()
        .filter(|(name, _)| name.as_str() != "frame")
        .find_map(|(name, value)| {
            let value = value.as_str()?;
            is_ref(value).then(|| (name.clone(), frame.clone()))
        })
}

/// Whether `value` is a ref the page's outline issued, such as `d1e5`: the
/// page (`d`, then the document's number) and the element (`e`, then its
/// number there).
#[must_use]
pub fn is_ref(value: &str) -> bool {
    let Some((document, element)) = value.split_once('e') else {
        return false;
    };
    let numbered = |part: &str| {
        !part.is_empty() && part.len() <= 6 && part.chars().all(|c| c.is_ascii_digit())
    };
    document.strip_prefix('d').is_some_and(numbered) && numbered(element)
}

/// The value `value` becomes in the draft: the input it equals, else the
/// constant it is.
fn bind(value: &Value, inputs: &[Value]) -> Binding {
    let named = inputs
        .iter()
        .filter_map(Value::as_object)
        .find(|input| input.get("value") == Some(value))
        .and_then(|input| input.get("name"))
        .and_then(Value::as_str);
    match named {
        Some(name) => Binding::Input(name.to_owned()),
        None => Binding::Literal(value.clone()),
    }
}

#[cfg(test)]
#[path = "export/tests.rs"]
mod tests;
