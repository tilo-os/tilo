//! A shortcut as text people can read and type.
//!
//! The document holds the program; this is a rendering of it. One line is one
//! step, nested blocks are indented, and a step reads as the phrase its action
//! has in the catalogue followed by named arguments — the same words the
//! visual editor shows, in the same order, so a step is in one place in both:
//!
//! ```text
//! shortcut "File the invoices"
//! input "folder" is text, not nothing, called "Where the invoices arrived"
//! each step may take 30 seconds
//! file on any Invoice in Invoices Folder input folder
//! for each title in file on any Invoice in Invoices.titles
//!   file on any Invoice in Invoices Folder variable title
//! ```
//!
//! Values are ordinary Tilo resources, named the way a Rust app developer names
//! them: [`ParamType::label`] in an input's declaration, the parameter label of
//! the action in an argument. The language adds only what a shortcut needs of
//! its own — text, whole numbers, numbers, lists and dictionaries — as literals
//! typed where a value is expected.
//!
//! Nothing in the file format depends on this rendering: a saved program is
//! still canonical JSON ([`crate::program`]), and that is what the checker, the
//! runner and a peer read. Text is how the same program reaches a person and an
//! agent that would rather write it than click it. Editing runs a structural
//! diff ([`edit`]) rather than replacement, so the document keeps every step the
//! change did not touch.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Number, Value};
use tilo_action_policy::{ActionKind, Answer, KindSet, Level, Rule, Segment, TreePath};
use tilo_actions::ActionKey;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::CatalogueRow;

use crate::catalog::{ActionCatalog, ResourceTarget, kind_matches};
use crate::path::StepPath;
use crate::shortcut::{
    Binding, Condition, Schedule, Selector, Shortcut, Step, StepKind, TriggerSpec,
};
use crate::template::is_identifier;
use crate::{AGENT_APP_ID, AGENT_TRANSFORM_KIND};
use tilo_actions::contract::ResultSpec;

/// Most bytes a rendering of a shortcut may have, so a pathologically nested
/// program cannot be turned into text without bound.
pub const MAX_TEXT_BYTES: usize = 262_144;

/// Most steps one program may have, so a deeply nested text is refused before
/// anything recurses over it.
pub const MAX_TEXT_STEPS: usize = 1_024;

/// The types a value of the language has a literal for.
const TYPES: [&str; 9] = [
    "text",
    "whole number",
    "number",
    "yes or no",
    "date and time",
    "secret",
    "item",
    "list",
    "dictionary",
];

/// Where a step sits: the index of each block around it, outermost first.
/// The root block has an empty shape.
pub type Shape = Vec<usize>;

/// The shape of the step at `index` in the block whose own shape is `parent`.
fn under(parent: &Shape, index: usize) -> Shape {
    let mut shape = parent.clone();
    shape.push(index);
    shape
}

/// A program that cannot be written as text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error("the program is too large to show as text")]
    TooLarge,
    #[error("the program has no step {0}")]
    NoStep(String),
}

/// A text that is not a program.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct TextParseError {
    pub line: usize,
    pub message: String,
}

impl TextParseError {
    fn at(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

/// One step's place in a program: the index of its block among the blocks the
/// rendering walked, and its own index in that block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    pub block: usize,
    pub index: usize,
}

/// The step a block holds the steps of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// The shortcut's own steps.
    Root,
    /// The steps run when the condition holds.
    Then,
    /// The steps run when it does not.
    Otherwise,
    /// The steps a loop runs for each item.
    Body,
}

/// One block the rendering walked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// The name a value refers to the step by — its phrase — when the program
    /// refers to the step's result at all. `None` for a step nothing refers to
    /// and for the shortcut's own block.
    pub refer: Option<String>,
}

/// What a rendering knows about the program it rendered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outline {
    pub blocks: Vec<Block>,
    /// Every step, and where its line starts.
    pub steps: BTreeMap<Shape, Location>,
    /// The step each line of the text states, by the line's number, so a
    /// diagnostic a checker reports at a step is shown on the line that
    /// states it. The lines that state no step — the shortcut's name, its
    /// inputs, what it gives back — are absent.
    pub lines: BTreeMap<usize, Shape>,
}

impl Outline {
    /// The line that states the step a checker reports a problem in, so a
    /// diagnostic carries the line a person can go to.
    ///
    /// `None` for the shortcut itself (`(shortcut)`), for a path that names no
    /// step this text states, and for `output`, which the text writes as a
    /// line that is not a step.
    #[must_use]
    pub fn line_of(&self, path: &StepPath) -> Option<usize> {
        let mut shape = Shape::new();
        for segment in path.as_str().split('.') {
            match segment {
                "then" | "else" | "each" => {}
                "output" => return None,
                index => shape.push(index.parse().ok()?),
            }
        }
        self.lines
            .iter()
            .find(|(_, stated)| **stated == shape)
            .map(|(line, _)| *line)
    }
}

/// What the rendering could not learn from the program alone: the phrase an
/// action has in the catalogue, and which action an AI or task step lowers to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hint {
    /// The declared result of the action the step invokes.
    pub result: Option<ResultSpec>,
    /// The catalogue row the step invokes.
    pub row: Option<CatalogueRow>,
}

/// How a step is written.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Naming {
    /// The line's first part, without a condition or a block's colon.
    phrase: String,
    /// The name a value refers to the step's result by, when the program
    /// refers to it at all.
    refer: Option<String>,
}

/// A change to a program, worked out from a text a person edited.
///
/// The document is the truth and a step it holds must stay a step it holds:
/// re-reading a whole program from text would replace every step, giving each
/// new identity and losing the history, the collaboration and the runs
/// hanging off the ones the person did not touch. So the text is read as a
/// program and *diffed* against the one the document has — a step whose line
/// reads the same is the same step, and only the lines that changed become
/// steps of their own.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    /// The program the text described, with every step whose line reads the
    /// same as it did keeping its identity.
    pub shortcut: Shortcut,
    /// The steps that left the program, by id, so a caller can let go of what
    /// hung off them.
    pub removed: Vec<String>,
}

/// Read `text` as a change to `before`.
///
/// # Errors
/// [`TextParseError`] naming the line that does not read as a program.
pub fn edit(shared: &Shared<'_>, before: &Shortcut, text: &str) -> Result<Edit, TextParseError> {
    let parsed = parse(shared, text)?;
    let mut removed = Vec::new();
    let steps = diff_steps(shared, &before.steps, parsed.shortcut.steps, &mut removed);
    Ok(Edit {
        shortcut: Shortcut {
            // Identity, and everything the text does not write, is the
            // document's: only the steps and what the text states change.
            id: before.id.clone(),
            saved_by: before.saved_by.clone(),
            steps,
            ..parsed.shortcut
        },
        removed,
    })
}

/// The steps `after` describes, each one the earlier step whose line reads the
/// same when there is one.
fn diff_steps(
    shared: &Shared<'_>,
    before: &[Step],
    after: Vec<Step>,
    removed: &mut Vec<String>,
) -> Vec<Step> {
    let earlier: Vec<String> = before.iter().map(|step| reads_as(shared, step)).collect();
    // Which earlier step each line states, found by the line it reads as.
    let mut taken: Vec<bool> = vec![false; earlier.len()];
    let mut identity: Vec<Option<usize>> = Vec::with_capacity(after.len());
    for step in &after {
        let line = reads_as(shared, step);
        let found = earlier
            .iter()
            .zip(&mut taken)
            .position(|(read, taken)| !*taken && *read == line);
        if let Some(at) = found
            && let Some(flag) = taken.get_mut(at)
        {
            *flag = true;
        }
        identity.push(found);
    }
    let mut kept: Vec<Step> = Vec::with_capacity(after.len());
    for (step, at) in after.into_iter().zip(identity) {
        let Some(there) = at.and_then(|at| before.get(at)) else {
            kept.push(step);
            continue;
        };
        let mut step = step;
        step.id.clone_from(&there.id);
        match (&mut step.kind, &there.kind) {
            (
                StepKind::If {
                    then, otherwise, ..
                },
                StepKind::If {
                    then: had_then,
                    otherwise: had_otherwise,
                    ..
                },
            ) => {
                let read = std::mem::take(then);
                *then = diff_steps(shared, had_then, read, removed);
                let read = std::mem::take(otherwise);
                *otherwise = diff_steps(shared, had_otherwise, read, removed);
            }
            (StepKind::ForEach { body, .. }, StepKind::ForEach { body: had, .. }) => {
                let read = std::mem::take(body);
                *body = diff_steps(shared, had, read, removed);
            }
            _ => {}
        }
        kept.push(step);
    }
    for (step, taken) in before.iter().zip(&taken) {
        if !*taken {
            removed.push(step.id.clone());
        }
    }
    kept
}

/// What a step reads as, which is what tells a step in the text from the step
/// the document holds.
fn reads_as(shared: &Shared<'_>, step: &Step) -> String {
    title(shared, step).trim_end_matches(':').to_owned()
}

// Everything below is one compiler pass over the same vocabulary: a step is
// written and read by the same rules, so `parse(render(program))` is the
// program, and every rule a person can read in a `ParseError` is the rule that
// wrote the line.

/// A program as text.
///
/// # Errors
/// [`TextError::TooLarge`] when the rendering would exceed [`MAX_TEXT_BYTES`].
pub fn render(
    shared: &Shared<'_>,
    shortcut: &Shortcut,
    hints: &BTreeMap<Shape, Hint>,
) -> Result<(String, Outline), TextError> {
    let mut walk = Walk {
        shared,
        hints,
        refer: referred(shortcut),
        namings: Vec::new(),
        every: BTreeMap::new(),
        text: String::new(),
        blocks: Vec::new(),
        steps: BTreeMap::new(),
        lines: BTreeMap::new(),
    };
    walk.header(shortcut)?;
    walk.block(&shortcut.steps, &Shape::new())?;
    if let Some(output) = &shortcut.output {
        let every: Vec<Naming> = walk
            .every
            .iter()
            .map(|(refer, phrase)| Naming {
                phrase: phrase.clone(),
                refer: Some(refer.clone()),
            })
            .collect();
        let line = format!("result is {}", value_text(output, &every));
        walk.line(0, &line)?;
    }
    for trigger in &shortcut.triggers {
        let line = trigger_text(trigger);
        walk.line(0, &line)?;
    }
    let text = std::mem::take(&mut walk.text);
    Ok((
        text,
        Outline {
            blocks: walk.blocks,
            steps: walk.steps,
            lines: walk.lines,
        },
    ))
}

/// What a rendering needs to know to write an action's phrase: the catalogue
/// it reads the phrase from, and what each application is called (the
/// surface's own naming, so the text says "Notes" where the person does).
pub struct Shared<'a> {
    pub catalog: &'a ActionCatalog,
    pub app_label: &'a dyn Fn(&str) -> String,
}

struct Walk<'a> {
    shared: &'a Shared<'a>,
    hints: &'a BTreeMap<Shape, Hint>,
    /// The step names the program refers to.
    refer: BTreeSet<String>,
    /// The names the block being written gave its steps.
    namings: Vec<Naming>,
    /// Every step's name, by the step it names, for the lines that come after
    /// the steps and may name a result from any block.
    every: BTreeMap<String, String>,
    text: String,
    blocks: Vec<Block>,
    steps: BTreeMap<Shape, Location>,
    /// The shape each line states, by its number.
    lines: BTreeMap<usize, Shape>,
}

impl Walk<'_> {
    fn line(&mut self, depth: usize, line: &str) -> Result<(), TextError> {
        let written = depth
            .saturating_mul(2)
            .saturating_add(line.len())
            .saturating_add(1);
        if self.text.len().saturating_add(written) > MAX_TEXT_BYTES {
            return Err(TextError::TooLarge);
        }
        let indent = "  ".repeat(depth);
        let _ = writeln!(self.text, "{indent}{line}");
        Ok(())
    }

    /// The number of the line just written.
    fn number(&self) -> usize {
        self.text.lines().count()
    }

    fn header(&mut self, shortcut: &Shortcut) -> Result<(), TextError> {
        let line = format!("shortcut {}", quoted(&shortcut.name));
        self.line(0, &line)?;
        for input in &shortcut.inputs {
            self.line(0, input_text(input).trim_start())?;
        }
        if let Some(seconds) = shortcut.step_timeout_secs {
            self.line(0, &format!("each step may take {seconds} seconds"))?;
        }
        for rule in &shortcut.grant {
            self.line(0, &format!("may {}", rule_text(rule)))?;
        }
        Ok(())
    }

    fn block(&mut self, steps: &[Step], shape: &Shape) -> Result<(), TextError> {
        let block = self.blocks.len();
        let namings = name_block(self.shared, steps);
        for naming in &namings {
            if let Some(refer) = &naming.refer {
                self.every.insert(refer.clone(), naming.phrase.clone());
            }
        }
        let outer = std::mem::take(&mut self.namings);
        self.namings.clone_from(&namings);
        for (index, step) in steps.iter().enumerate() {
            let here = under(shape, index);
            let Some(naming) = namings.get(index).cloned() else {
                continue;
            };
            let (phrase, notes) = step_line(self.shared, step, &namings, self.hints.get(&here));
            let mut line = phrase.clone();
            if let Some(condition) = condition_clause(step) {
                let _ = write!(line, ": {}", condition_render(condition, &namings));
            }
            for note in notes {
                let _ = write!(line, " {note}");
            }
            let depth = shape.len();
            self.line(depth, &line)?;
            self.lines.insert(self.number(), here.clone());
            self.steps.insert(here.clone(), Location { block, index });
            match &step.kind {
                StepKind::If {
                    then, otherwise, ..
                } => {
                    self.widget_block(then, &here, BlockKind::Then, &naming)?;
                    if !otherwise.is_empty() {
                        self.line(depth, "otherwise:")?;
                        self.widget_block_otherwise(otherwise, &here, &naming)?;
                    }
                }
                StepKind::ForEach { body, .. } => {
                    self.widget_block(body, &here, BlockKind::Body, &naming)?;
                }
                StepKind::Invoke { .. }
                | StepKind::SetVar { .. }
                | StepKind::Ask { .. }
                | StepKind::Ai { .. }
                | StepKind::Task { .. } => {}
            }
        }
        self.namings = outer;
        Ok(())
    }

    fn widget_block(
        &mut self,
        steps: &[Step],
        owner: &Shape,
        kind: BlockKind,
        naming: &Naming,
    ) -> Result<(), TextError> {
        let refer = naming
            .refer
            .clone()
            .filter(|name| self.refer.contains(name));
        self.blocks.push(Block { kind, refer });
        self.block(steps, owner)
    }

    /// A branch's other block: the same shape, written under `otherwise:`.
    fn widget_block_otherwise(
        &mut self,
        steps: &[Step],
        owner: &Shape,
        naming: &Naming,
    ) -> Result<(), TextError> {
        self.widget_block(steps, owner, BlockKind::Otherwise, naming)
    }
}

// ---------------------------------------------------------------- naming ---

/// What each step of one block is called in its text.
fn name_block(shared: &Shared<'_>, steps: &[Step]) -> Vec<Naming> {
    let phrase = |step: &Step| title(shared, step).trim_end_matches(':').to_owned();
    // Two steps of one block may read alike; the one the program refers to its
    // result of then takes a number, so each name means one step.
    let mut used: BTreeSet<String> = BTreeSet::new();
    steps
        .iter()
        .map(|step| {
            let Some(wanted) = refer_name_step(shared, step) else {
                return Naming {
                    phrase: phrase(step),
                    refer: None,
                };
            };
            let mut name = wanted.clone();
            let mut number = 1usize;
            while !used.insert(name.clone()) {
                number = number.saturating_add(1);
                name = format!("{wanted} {number}");
            }
            Naming {
                phrase: name,
                refer: (!step.id.is_empty()).then(|| step.id.clone()),
            }
        })
        .collect()
}

/// Whether a line states what the shortcut gives back or what starts it,
/// which the text writes after the steps.
fn trailing(text: &str) -> bool {
    text.starts_with("result is ")
        || text.starts_with("each step may take ")
        || text.starts_with("may ")
        || is_trigger(text)
}

/// The name a step's result is referred to by — the phrase the step reads
/// with — when the program refers to that result at all.
fn refer_name_step(shared: &Shared<'_>, step: &Step) -> Option<String> {
    match &step.kind {
        StepKind::Invoke { action, args, .. } => {
            if action.action == crate::grant::FIELD_EDIT
                && let Some(field) = edit_field(args)
            {
                return Some(field);
            }
            Some(title(shared, step).trim_end_matches(':').to_owned())
        }
        StepKind::If { .. } => None,
        StepKind::ForEach { item, .. } | StepKind::SetVar { name: item, .. } => Some(item.clone()),
        StepKind::Ask { param } => Some(param.name.to_string()),
        StepKind::Ai { .. } => Some("its answer".to_owned()),
        StepKind::Task { .. } => Some("its summary".to_owned()),
    }
}

/// The field a field edit changes, when the program names it.
fn edit_field(args: &BTreeMap<String, Binding>) -> Option<String> {
    let edit = args.get(crate::grant::EDIT_PARAM)?;
    let path = match edit {
        Binding::Literal(edit) => edit.get("path")?,
        Binding::Object(members) => members.get("path")?.as_literal()?,
        _ => return None,
    };
    let steps = path.as_array()?;
    let mut named = None;
    for step in steps {
        let (kind, id) = step.as_object()?.iter().next()?;
        if kind == "field" {
            named = id.as_str().map(str::to_owned);
        }
    }
    named.or_else(|| Some("its field".to_owned()))
}

trait AsLiteral {
    fn as_literal(&self) -> Option<&Value>;
}

impl AsLiteral for Binding {
    fn as_literal(&self) -> Option<&Value> {
        match self {
            Binding::Literal(value) => Some(value),
            _ => None,
        }
    }
}

/// A step's heading: what it does, in the words the catalogue uses for it.
fn title(shared: &Shared<'_>, step: &Step) -> String {
    match &step.kind {
        StepKind::Invoke {
            resource, action, ..
        } => invoke_title(shared, action, resource),
        StepKind::If { .. } => "when".to_owned(),
        StepKind::ForEach { item, .. } => format!("for each {item} in"),
        StepKind::SetVar { name, .. } => format!("set {name} to"),
        StepKind::Ask { param } => format!("ask for {}", param.description),
        StepKind::Ai { prompt, .. } => format!("ask AI {prompt}"),
        StepKind::Task { instruction, .. } => format!("tell SaaSy {instruction}"),
    }
}

fn invoke_title(shared: &Shared<'_>, action: &ActionKey, resource: &Selector) -> String {
    if action.app == AGENT_APP_ID && kind_matches(AGENT_TRANSFORM_KIND, &action.kind) {
        let agent = (shared.app_label)(AGENT_APP_ID);
        return match action.action.as_str() {
            crate::AGENT_SUMMARISE_ACTION => "summarise".to_owned(),
            crate::AGENT_EXTRACT_ACTION => "extract".to_owned(),
            crate::AGENT_CLASSIFY_ACTION => "classify".to_owned(),
            crate::AGENT_FREEFORM_ACTION => "ask AI".to_owned(),
            crate::AGENT_TASK_ACTION => format!("tell {agent}"),
            _ => format!("{} {agent}", action.action),
        };
    }
    let app = (shared.app_label)(&action.app);
    let target = match resource {
        Selector::Fixed(id) | Selector::FromBinding(Binding::Literal(Value::String(id))) => {
            ResourceTarget::Id(id)
        }
        Selector::FirstOfKind(kind) => ResourceTarget::Kind(kind),
        Selector::FromBinding(_) => ResourceTarget::Any,
    };
    if let Some(row) = shared.catalog.find(action, target) {
        return reads_any(row, shared.app_label)
            .unwrap_or_else(|| fallback_title(action, target, &app));
    }
    fallback_title(action, target, &app)
}

/// What a step reads as when the catalogue has no words for it.
fn fallback_title(action: &ActionKey, target: ResourceTarget<'_>, app: &str) -> String {
    match target {
        ResourceTarget::Id(id) => format!("{} on {id} in {app}", action.action),
        ResourceTarget::Kind(kind) => format!("{} on any {kind} in {app}", action.action),
        ResourceTarget::Any => format!("{} on the item in {app}", action.action),
    }
}

// -------------------------------------------------------------- the line ---

/// The condition a branch step holds.
fn condition_clause(step: &Step) -> Option<&Condition> {
    match &step.kind {
        StepKind::If { cond, .. } => Some(cond),
        _ => None,
    }
}

/// A step as its line, then the notes that name what the phrase cannot: the
/// step's own fields and the arguments its action takes.
fn step_line(
    shared: &Shared<'_>,
    step: &Step,
    namings: &[Naming],
    hint: Option<&Hint>,
) -> (String, Vec<String>) {
    let phrase = title(shared, step);
    match &step.kind {
        StepKind::ForEach { items, .. } => {
            return (
                format!("{phrase} {}", value_text(items, namings)),
                Vec::new(),
            );
        }
        StepKind::SetVar { value, .. } => {
            return (
                format!("{phrase} to {}", value_text(value, namings)),
                Vec::new(),
            );
        }
        StepKind::If { .. } | StepKind::Ask { .. } => return (phrase, Vec::new()),
        StepKind::Invoke { .. } | StepKind::Ai { .. } | StepKind::Task { .. } => {}
    }
    let mut notes = Vec::new();
    match &step.kind {
        StepKind::Invoke {
            args,
            appearance,
            resource,
            ..
        } => {
            if let Some(params) = params_of(shared, step, hint) {
                for (name, binding) in args {
                    let label = params
                        .iter()
                        .find(|param| param.name.as_ref() == name)
                        .map_or_else(|| readable(name), label_of);
                    notes.push(format!("{label} {}", value_text(binding, namings)));
                }
            } else {
                for (name, binding) in args {
                    notes.push(format!("{name}: {}", value_text(binding, namings)));
                }
            }
            if let Selector::FromBinding(binding) = resource {
                notes.push(format!("on {}", value_text(binding, namings)));
            }
            if appearance.is_some() {
                notes.push("shown as its author".to_owned());
            }
        }
        StepKind::If { .. }
        | StepKind::ForEach { .. }
        | StepKind::SetVar { .. }
        | StepKind::Ask { .. } => {}
        StepKind::Ai { input, .. } => {
            if let Some(input) = input {
                notes.push(format!("of {}", value_text(input, namings)));
            }
        }
        StepKind::Task { allow, .. } => {
            for rule in allow {
                notes.push(format!("may {}", rule_text(rule)));
            }
        }
    }
    (phrase, notes)
}

/// The parameters of the action a step invokes, from the catalogue the
/// rendering was given or the row it was handed.
fn params_of(shared: &Shared<'_>, step: &Step, hint: Option<&Hint>) -> Option<Vec<ActionParam>> {
    if let Some(hint) = hint {
        return Some(hint.row.as_ref()?.action.as_ref()?.params.clone());
    }
    let StepKind::Invoke {
        action, resource, ..
    } = &step.kind
    else {
        return None;
    };
    let row = shared.catalog.find(action, target_of(resource))?;
    Some(row.action.as_ref()?.params.clone())
}

fn target_of(resource: &Selector) -> ResourceTarget<'_> {
    match resource {
        Selector::Fixed(id) | Selector::FromBinding(Binding::Literal(Value::String(id))) => {
            ResourceTarget::Id(id)
        }
        Selector::FirstOfKind(kind) => ResourceTarget::Kind(kind),
        Selector::FromBinding(_) => ResourceTarget::Any,
    }
}

/// The label an argument is written under: the parameter's own label when it
/// published one, otherwise a readable form of its name.
fn label_of(param: &ActionParam) -> String {
    if param.description.trim().is_empty() {
        readable(&param.name)
    } else {
        param.description.to_string()
    }
}

/// `sender_id` as `Sender id`.
fn readable(name: &str) -> String {
    let mut out = String::new();
    for (index, word) in name
        .split(['_', '-'])
        .filter(|word| !word.is_empty())
        .enumerate()
    {
        if index > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        let Some(first) = chars.next() else { continue };
        if index == 0 {
            out.extend(first.to_uppercase());
        } else {
            out.push(first);
        }
        out.push_str(chars.as_str());
    }
    if out.is_empty() { name.to_owned() } else { out }
}

/// A binding as the text writes it: a literal where one is written, and
/// otherwise the name the program refers to the value by.
fn value_text(binding: &Binding, namings: &[Naming]) -> String {
    match binding {
        Binding::Literal(value) => literal_text(value),
        Binding::Input(name) => named_text("input", name),
        Binding::Var(name) => {
            let (base, pointer) = Binding::named_part(name);
            let Some(phrase) = namings.iter().find_map(|naming| {
                naming
                    .refer
                    .as_deref()
                    .filter(|refer| *refer == base)
                    .map(|_| naming.phrase.clone())
            }) else {
                return named_text("variable", name);
            };
            with_pointer(&phrase, &pointer)
        }
        Binding::StepOutput { step, pointer } => {
            let Some(phrase) = namings.iter().find_map(|naming| {
                naming
                    .refer
                    .as_deref()
                    .filter(|refer| *refer == step)
                    .map(|_| naming.phrase.clone())
            }) else {
                return format!("the result of {step}");
            };
            with_pointer(&phrase, pointer)
        }
        Binding::Template(text) => template_text(text, namings),
        Binding::Object(members) => {
            let written: Vec<String> = members
                .iter()
                .map(|(name, member)| format!("{}: {}", quoted(name), value_text(member, namings)))
                .collect();
            format!("{{{}}}", written.join(", "))
        }
        Binding::List(items) => {
            let written: Vec<String> = items.iter().map(|item| value_text(item, namings)).collect();
            format!("[{}]", written.join(", "))
        }
    }
}

fn named_text(kind: &str, name: &str) -> String {
    let (base, pointer) = Binding::named_part(name);
    with_pointer(&format!("{kind} {base}"), &pointer)
}

fn with_pointer(name: &str, pointer: &str) -> String {
    let tokens: Vec<String> = pointer
        .split('/')
        .filter(|token| !token.is_empty())
        .map(|token| {
            token.parse::<usize>().map_or_else(
                |_| format!(".{}", token.replace("~1", "/").replace("~0", "~")),
                |index| format!(" item {index}"),
            )
        })
        .collect();
    format!("{name}{}", tokens.concat())
}

/// A JSON value as the language writes it.
fn literal_text(value: &Value) -> String {
    match value {
        Value::String(text) => quoted(text),
        Value::Number(number) => number.to_string(),
        Value::Bool(true) => "yes".to_owned(),
        Value::Bool(false) => "no".to_owned(),
        Value::Null => "nothing".to_owned(),
        Value::Array(items) => {
            let written: Vec<String> = items.iter().map(literal_text).collect();
            format!("[{}]", written.join(", "))
        }
        Value::Object(members) => {
            let written: Vec<String> = members
                .iter()
                .map(|(name, member)| format!("{}: {}", quoted(name), literal_text(member)))
                .collect();
            format!("{{{}}}", written.join(", "))
        }
    }
}

/// A template: the text as it reads, with its placeholders written as the
/// names a person sees.
fn template_text(text: &str, namings: &[Naming]) -> String {
    match placeholder_bindings(text) {
        Some(bindings) if bindings.is_empty() => quoted(text),
        Some(bindings) => {
            let mut written = String::new();
            let mut rest = text;
            let mut next = bindings.iter();
            while let Some((before, after)) = rest.split_once("{{") {
                written.push_str(before);
                let Some((_, tail)) = after.split_once("}}") else {
                    break;
                };
                if let Some(binding) = next.next() {
                    let _ = write!(written, "{{{}}}", value_text(binding, namings));
                }
                rest = tail;
            }
            written.push_str(rest);
            quoted(&written)
        }
        None => {
            // A template the program holds and the parser will not read back:
            // it is written as the text it is, and reading it back refuses it.
            quoted(text)
        }
    }
}

/// The bindings a template's placeholders name, `Some` for a template without
/// placeholders too; `None` when a placeholder is not one the language reads.
fn placeholder_bindings(text: &str) -> Option<Vec<Binding>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some((_, after)) = rest.split_once("{{") {
        let (inner, tail) = after.split_once("}}")?;
        out.push(placeholder_binding(inner.trim())?);
        rest = tail;
    }
    Some(out)
}

fn placeholder_binding(inner: &str) -> Option<Binding> {
    let (source, name) = inner.split_once('.')?;
    let binding = match source {
        "input" => Binding::Input(name.to_owned()),
        "var" => Binding::Var(name.to_owned()),
        "step" => match name.split_once('/') {
            Some((step, pointer)) => Binding::step(step, format!("/{pointer}")),
            None => Binding::step(name, ""),
        },
        _ => return None,
    };
    let named = match &binding {
        Binding::Input(name) | Binding::Var(name) => Binding::named_part(name).0,
        Binding::StepOutput { step, .. } => step.as_str(),
        _ => return None,
    };
    is_identifier(named).then_some(binding)
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}

// ----------------------------------------------------------- declarations ---

fn input_text(param: &ActionParam) -> String {
    let mut line = format!(
        "input {} is {}, {}",
        quoted(param.name.as_ref()),
        type_name(param.ty),
        if param.nullable {
            "may be nothing"
        } else {
            "not nothing"
        },
    );
    if !param.description.is_empty() {
        line.push_str(", called ");
        line.push_str(&quoted(&param.description));
    }
    if !param.choices.is_empty() {
        let choices: Vec<String> = param.choices.iter().map(|choice| quoted(choice)).collect();
        line.push_str(", one of ");
        line.push_str(&choices.join(" or "));
    }
    line
}

fn type_name(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Text => "text",
        ParamType::Integer => "whole number",
        ParamType::Number => "number",
        ParamType::Boolean => "yes or no",
        ParamType::DateTime => "date and time",
        ParamType::Secret => "secret",
        ParamType::Resource => "item",
        ParamType::Array => "list",
        ParamType::Object => "dictionary",
    }
}

fn type_of(name: &str) -> Option<ParamType> {
    Some(match name {
        "text" => ParamType::Text,
        "whole number" => ParamType::Integer,
        "number" => ParamType::Number,
        "yes or no" => ParamType::Boolean,
        "date and time" => ParamType::DateTime,
        "secret" => ParamType::Secret,
        "item" => ParamType::Resource,
        "list" => ParamType::Array,
        "dictionary" => ParamType::Object,
        _ => return None,
    })
}

fn rule_text(rule: &Rule) -> String {
    let place = rule
        .node
        .segments()
        .iter()
        .filter(|segment| !matches!(segment, Segment::Rest))
        .map(segment_text)
        .collect::<Vec<_>>()
        .join("/");
    let place = if place.is_empty() {
        "anything".to_owned()
    } else {
        place
    };
    let answer = match rule.answer {
        Answer::Allow => "act",
        Answer::Ask => "ask before it acts",
        Answer::Deny => "never act",
    };
    let kinds: Vec<&str> = ActionKind::ALL
        .into_iter()
        .filter(|kind| rule.kinds.contains(*kind))
        .map(|kind| match kind {
            ActionKind::Read => "reading",
            ActionKind::Undoable => "changes",
            ActionKind::Outside => "outward",
        })
        .collect();
    if kinds.is_empty() {
        format!("{answer} on {place}")
    } else {
        format!("{answer} on {place} for {}", kinds.join(", "))
    }
}

fn segment_text(segment: &Segment) -> String {
    match segment {
        Segment::Rest => "anything".to_owned(),
        Segment::Any(level) => format!("any {}", level_noun(*level)),
        Segment::Name(level, id) => format!("{}:{id}", level.tag()),
    }
}

fn level_noun(level: Level) -> &'static str {
    level.noun()
}

fn trigger_text(trigger: &TriggerSpec) -> String {
    match trigger {
        TriggerSpec::Keyboard { chord } => format!("when someone presses {}", quoted(chord)),
        TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds },
            catch_up,
        } => format!("every {seconds} seconds{}", catch_up_text(*catch_up)),
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar { at, weekdays },
            catch_up,
        } => calendar_text(*at, weekdays, *catch_up),
        TriggerSpec::AppOpened { app_id } => {
            format!("when the app opens {}", quoted(app_id))
        }
        TriggerSpec::WorkspaceJoined { room } => match room {
            Some(room) => format!("when someone joins workspace {}", quoted(room)),
            None => "when someone joins a workspace".to_owned(),
        },
        TriggerSpec::FileChanged { capability } => {
            format!("when a file changes in {}", quoted(capability))
        }
        TriggerSpec::FocusMode { mode, transition } => format!(
            "when focus {mode} is {}",
            match transition {
                crate::shortcut::FocusTransition::Enter => "entered",
                crate::shortcut::FocusTransition::Leave => "left",
            }
        ),
        TriggerSpec::NetworkChanged { network, connected } => {
            let mut line = String::from("when the network changes");
            if let Some(network) = network {
                let _ = write!(line, " to {}", quoted(network));
            }
            if let Some(connected) = connected {
                line.push_str(if *connected {
                    " connected"
                } else {
                    " disconnected"
                });
            }
            line
        }
        TriggerSpec::DeviceConnected { device_class } => {
            format!("when a {} connects", quoted(device_class))
        }
        TriggerSpec::RecordChanged {
            kind,
            actions,
            fields,
            by,
        } => {
            let mut line = format!("when a {} changes", quoted(kind));
            if !actions.is_empty() {
                let _ = write!(line, " by {}", quoted_list(actions));
            }
            if !fields.is_empty() {
                let _ = write!(line, " in {}", quoted_list(fields));
            }
            line.push_str(match by {
                crate::shortcut::ActorFilter::Anyone => "",
                crate::shortcut::ActorFilter::People => " by people",
                crate::shortcut::ActorFilter::NotBots => " by anyone but a bot",
            });
            line
        }
        TriggerSpec::Connector {
            app_id,
            source,
            filter,
        } => {
            let mut line = format!("when {} sends {}", quoted(app_id), quoted(source));
            for (field, value) in filter {
                let _ = write!(line, " where {field} is {}", literal_text(value));
            }
            line
        }
        TriggerSpec::IncomingMessage { app_id, from } => {
            let mut line = String::from("when a message arrives");
            if let Some(app_id) = app_id {
                let _ = write!(line, " in {}", quoted(app_id));
            }
            if let Some(from) = from {
                let _ = write!(line, " from {}", quoted(from));
            }
            line
        }
        TriggerSpec::NearbyDevice { device } => match device {
            Some(device) => format!("when {} comes near", quoted(device)),
            None => "when a device comes near".to_owned(),
        },
    }
}

/// A daily schedule, on the days it names.
fn calendar_text(
    at: crate::shortcut::TimeOfDay,
    weekdays: &[crate::shortcut::Weekday],
    catch_up: crate::shortcut::CatchUp,
) -> String {
    let days: Vec<&str> = weekdays.iter().map(|day| weekday_name(*day)).collect();
    let when = if days.is_empty() {
        "day".to_owned()
    } else {
        days.join(", ")
    };
    format!(
        "every {when} at {:02}:{:02}{}",
        at.hour,
        at.minute,
        catch_up_text(catch_up)
    )
}

/// Several names as the text writes them: each in quotation marks, joined by
/// `or`.
fn quoted_list(names: &[String]) -> String {
    names
        .iter()
        .map(|name| quoted(name))
        .collect::<Vec<String>>()
        .join(" or ")
}

fn catch_up_text(catch_up: crate::shortcut::CatchUp) -> &'static str {
    match catch_up {
        crate::shortcut::CatchUp::Skip => "",
        crate::shortcut::CatchUp::RunOnce => ", running once for what was missed",
    }
}

fn weekday_name(day: crate::shortcut::Weekday) -> &'static str {
    use crate::shortcut::Weekday::{
        Friday, Monday, Saturday, Sunday, Thursday, Tuesday, Wednesday,
    };
    match day {
        Monday => "Monday",
        Tuesday => "Tuesday",
        Wednesday => "Wednesday",
        Thursday => "Thursday",
        Friday => "Friday",
        Saturday => "Saturday",
        Sunday => "Sunday",
    }
}

/// The `on any <kind>` form of the same action: what a step reads when it
/// takes the first resource of a kind rather than one resource.
fn reads_any(row: &CatalogueRow, app_label: &dyn Fn(&str) -> String) -> Option<String> {
    let descriptor = row.action.as_ref()?;
    if descriptor.action_label.is_empty() {
        return None;
    }
    let app = app_label(&row.app_id);
    Some(format!(
        "{} on any {} in {app}",
        descriptor.action_label,
        readable(&row.resource.kind)
    ))
}

// ---------------------------------------------------------------- parsing ---

/// One line of a program's text.
struct Row<'t> {
    number: usize,
    indent: usize,
    text: &'t str,
}

/// A program read from text: the shortcut, and the phrase each step's line
/// began with, so a naming scheme can refer to a step it did not name itself.
pub struct Parsed {
    pub shortcut: Shortcut,
    pub phrases: BTreeMap<Shape, String>,
}

/// The steps under a line that began a block, and the steps of its
/// `otherwise:`. A block that states neither is `(None, None)`.
type Blocks = (Option<Vec<Step>>, Option<Vec<Step>>);

struct Reader<'a> {
    shared: &'a Shared<'a>,
    rows: Vec<Row<'a>>,
    phrases: BTreeMap<Shape, String>,
    count: usize,
}

/// A line read as a step: the step still empty of children, and whether the
/// line opened a block.
struct Read {
    step: Step,
    open: bool,
}

/// What a block's lines may refer back to: the inputs, and the name each
/// earlier line gave its result.
#[derive(Clone, Default)]
struct Targets {
    bindings: Vec<Binding>,
    phrases: Vec<String>,
}

impl Targets {
    fn value_of(&self, phrase: &str) -> Option<&Binding> {
        let at = self.phrases.iter().position(|name| name == phrase)?;
        self.bindings.get(at)
    }

    /// Note what a line's result is called, for the lines after it.
    fn note(&mut self, phrase: &str, step: &Step) {
        let binding = match &step.kind {
            StepKind::If { .. } => return,
            StepKind::ForEach { item, .. } => Binding::Var(item.clone()),
            StepKind::SetVar { name, .. } => Binding::Var(name.clone()),
            StepKind::Ask { param } => Binding::Var(param.name.to_string()),
            StepKind::Invoke { .. } | StepKind::Ai { .. } | StepKind::Task { .. } => {
                Binding::step(phrase, "")
            }
        };
        self.bindings.push(binding);
        self.phrases.push(phrase.to_owned());
    }
}

/// Read a shortcut from its text.
///
/// # Errors
/// [`TextParseError`] naming the line that does not read as a program.
pub fn parse(shared: &Shared<'_>, text: &str) -> Result<Parsed, TextParseError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(TextParseError::at(
            1,
            "the text is too long to be a shortcut",
        ));
    }
    let mut rows = Vec::new();
    let mut previous = 1;
    for (index, line) in text.lines().enumerate() {
        let number = index.saturating_add(1);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len().saturating_sub(trimmed.len());
        if trimmed.starts_with('\t') {
            return Err(TextParseError::at(
                number,
                "indent with spaces, not tabs, so a block lines up",
            ));
        }
        if indent % 2 != 0 {
            return Err(TextParseError::at(
                previous,
                "indent each block by two spaces",
            ));
        }
        previous = number;
        rows.push(Row {
            number,
            indent: indent / 2,
            text: trimmed.trim_end(),
        });
    }
    let Some(first) = rows.first() else {
        return Err(TextParseError::at(1, "a shortcut starts with its name"));
    };
    if first.indent != 0 {
        return Err(TextParseError::at(
            first.number,
            "the first line is not indented",
        ));
    }
    if rows.len() > MAX_TEXT_STEPS {
        let last = rows
            .get(MAX_TEXT_STEPS.saturating_sub(1))
            .map_or(1, |row| row.number);
        return Err(TextParseError::at(
            last,
            format!("a shortcut has at most {MAX_TEXT_STEPS} steps"),
        ));
    }
    let mut reader = Reader {
        shared,
        rows,
        phrases: BTreeMap::new(),
        count: 0,
    };
    reader.resource()
}

/// Where a line's colon sits, when the colon is outside a quoted text.
fn split_colon(text: &str) -> Option<(&str, &str)> {
    let mut quoted_text = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if quoted_text {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted_text = false;
            }
            continue;
        }
        match ch {
            '"' => quoted_text = true,
            ':' if text
                .get(index.saturating_add(1)..)
                .is_some_and(|after| after.starts_with(' ')) =>
            {
                let head = text.get(..index)?;
                let tail = text.get(index.saturating_add(1)..)?.trim_start();
                return Some((head, tail));
            }
            _ => {}
        }
    }
    None
}

/// Whether `at` in `text` sits inside a quoted text.
fn inside_quote(text: &str, at: usize) -> bool {
    let mut quoted_text = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if index >= at {
            return quoted_text;
        }
        if quoted_text {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted_text = false;
            }
        } else if ch == '"' {
            quoted_text = true;
        }
    }
    quoted_text
}

/// `text` cut at each space outside a quoted text, shortest first, so the
/// longest one that names something is the reading a line is given.
fn prefixes(text: &str) -> Vec<&str> {
    let mut cuts: Vec<usize> = text
        .char_indices()
        .filter(|(index, ch)| *ch == ' ' && !inside_quote(text, *index))
        .map(|(index, _)| index)
        .collect();
    cuts.push(text.len());
    cuts.into_iter().filter_map(|end| text.get(..end)).collect()
}

/// A quoted text, and what follows the closing quote.
fn text_at(text: &str) -> Result<(String, &str), String> {
    let text = text.trim_start();
    let Some(rest) = text.strip_prefix('"') else {
        return Err("write the words inside quotation marks".to_owned());
    };
    let mut out = String::new();
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped {
            out.push(match ch {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => {
                let after = rest.get(index.saturating_add(1)..).unwrap_or_default();
                return Ok((out, after));
            }
            other => out.push(other),
        }
    }
    Err("this text has no closing quotation mark".to_owned())
}

/// A whole line that must be one quoted text.
fn text_of(text: &str) -> Result<String, String> {
    let (words, rest) = text_at(text)?;
    if !rest.trim().is_empty() {
        return Err("write the name as one quoted text".to_owned());
    }
    Ok(words)
}

impl Reader<'_> {
    fn error(&self, index: usize, message: impl Into<String>) -> TextParseError {
        let line = self.rows.get(index).map_or(1, |row| row.number);
        TextParseError::at(line, message)
    }

    /// Read the shortcut: its header lines, then its steps.
    fn resource(&mut self) -> Result<Parsed, TextParseError> {
        let shared = self.shared;
        let mut shortcut = Shortcut {
            id: String::new(),
            name: String::new(),
            inputs: Vec::new(),
            steps: Vec::new(),
            step_timeout_secs: None,
            output: None,
            grant: Vec::new(),
            triggers: Vec::new(),
            saved_by: None,
        };
        let mut named = false;
        let mut index = 0;
        // The shortcut's headers and its steps share the left edge, so read
        // down it once: a line the reader knows as a header declares something
        // about the shortcut, and the first line it does not know is a step.
        while self.at_left_edge(index) {
            if self
                .rows
                .get(index)
                .is_some_and(|row| row.text.starts_with("shortcut "))
                && named
            {
                let number = self.rows.get(index).map_or(1, |row| row.number);
                return Err(TextParseError::at(number, "a shortcut is named once"));
            }
            let Some(declared) = self.declaration(&mut shortcut, &Targets::default(), index)?
            else {
                break;
            };
            named |= declared;
            index = index.saturating_add(1);
        }
        if !named {
            return Err(TextParseError::at(
                1,
                "a shortcut starts with `shortcut \"its name\"`",
            ));
        }
        if self.rows.get(index).is_some_and(|row| row.indent > 0) {
            return Err(self.error(
                index,
                "this line sits under no step: write it under the step that runs it",
            ));
        }
        let root: Shape = Shape::new();
        let (steps, mut next) = self.block(shared, index, 0, &root, &Targets::default())?;
        // What the shortcut gives back, and what starts it, read after the
        // steps: both name results the steps above them have.
        let mut targets = Targets::default();
        for step in &steps {
            let heading = title(shared, step).trim_end_matches(':').to_owned();
            targets.note(&heading, step);
        }
        while self.at_left_edge(next) {
            if self.declaration(&mut shortcut, &targets, next)?.is_none() {
                let number = self.rows.get(next).map_or(1, |row| row.number);
                return Err(TextParseError::at(
                    number,
                    "this is not a step: begin it with what it does, or with `when`, `for each` or `set`",
                ));
            }
            next = next.saturating_add(1);
        }
        if let Some(row) = self.rows.get(next) {
            return Err(TextParseError::at(
                row.number,
                "this line sits under no step: write it under the step that runs it",
            ));
        }
        shortcut.steps = steps;
        Ok(Parsed {
            shortcut,
            phrases: std::mem::take(&mut self.phrases),
        })
    }

    /// Whether the line at `index` sits at the left edge of the text.
    fn at_left_edge(&self, index: usize) -> bool {
        self.rows.get(index).is_some_and(|row| row.indent == 0)
    }

    /// Read the line at `index` as a statement about the whole shortcut.
    ///
    /// `Some(named)` when the line was one — `named` says whether that line is
    /// the one that names the shortcut — and `None` when it was not.
    fn declaration(
        &self,
        shortcut: &mut Shortcut,
        targets: &Targets,
        index: usize,
    ) -> Result<Option<bool>, TextParseError> {
        let Some(row) = self.rows.get(index).filter(|row| row.indent == 0) else {
            return Ok(None);
        };
        let text = row.text;
        let number = row.number;
        if let Some(rest) = text.strip_prefix("shortcut ") {
            shortcut.name = text_of(rest).map_err(|m| TextParseError::at(number, m))?;
            return Ok(Some(true));
        }
        if let Some(rest) = text.strip_prefix("input ") {
            shortcut
                .inputs
                .push(input_of(rest).map_err(|m| TextParseError::at(number, m))?);
            return Ok(Some(false));
        }
        if let Some(rest) = text.strip_prefix("result is ") {
            shortcut.output =
                Some(binding_of(rest, targets).map_err(|m| TextParseError::at(number, m))?);
            return Ok(Some(false));
        }
        if let Some(rest) = text.strip_prefix("each step may take ") {
            shortcut.step_timeout_secs =
                Some(seconds_of(rest).map_err(|m| TextParseError::at(number, m))?);
            return Ok(Some(false));
        }
        if let Some(rest) = text.strip_prefix("may ") {
            shortcut
                .grant
                .push(rule_of(rest).map_err(|m| TextParseError::at(number, m))?);
            return Ok(Some(false));
        }
        if is_trigger(text) {
            shortcut
                .triggers
                .push(trigger_of(text).map_err(|m| TextParseError::at(number, m))?);
            return Ok(Some(false));
        }
        Ok(None)
    }

    /// Read the block starting at `from`, whose lines are indented `depth`.
    fn block(
        &mut self,
        shared: &Shared<'_>,
        from: usize,
        depth: usize,
        shape: &Shape,
        outer: &Targets,
    ) -> Result<(Vec<Step>, usize), TextParseError> {
        let mut steps: Vec<Step> = Vec::new();
        let mut targets = outer.clone();
        let mut index = from;
        while let Some(row) = self.rows.get(index).filter(|row| row.indent == depth) {
            // `result is` and what starts the shortcut come after the steps.
            if depth == 0 && trailing(row.text) {
                break;
            }
            self.count = steps.len();
            let (step, next) = self.line(shared, index, depth, shape, &targets)?;
            // The line's phrase is the full heading; what the text refers to a
            // result by can be shorter (`file` for a named field's edit), so
            // the heading itself is what a later line names.
            let heading = title(shared, &step).trim_end_matches(':').to_owned();
            targets.note(&heading, &step);
            steps.push(step);
            index = next;
        }
        Ok((steps, index))
    }

    /// One step at `index`, and the index of the line after it. A line that
    /// begins a block takes the block under it, and its `otherwise:`.
    fn line(
        &mut self,
        shared: &Shared<'_>,
        index: usize,
        depth: usize,
        shape: &Shape,
        targets: &Targets,
    ) -> Result<(Step, usize), TextParseError> {
        let read = self.step(shared, index, targets)?;
        let here = under(shape, self.count);
        let below = index.saturating_add(1);
        let mut next = below;
        let (children, otherwise) = if read.open {
            self.open_block(shared, &mut next, below, depth, &here, targets)?
        } else {
            (None, None)
        };
        Ok((read.finish(children, otherwise), next))
    }

    /// The steps under a line that began a block, and its `otherwise:` when
    /// it has one. `next` walks past both.
    fn open_block(
        &mut self,
        shared: &Shared<'_>,
        next: &mut usize,
        under_line: usize,
        depth: usize,
        here: &Shape,
        targets: &Targets,
    ) -> Result<Blocks, TextParseError> {
        let under = depth.saturating_add(1);
        if self
            .rows
            .get(under_line)
            .is_none_or(|row| row.indent != under)
        {
            return Err(self.error(
                under_line.saturating_sub(1),
                "this line begins a block: write its steps under it, indented two more spaces",
            ));
        }
        let (children, after) = self.block(shared, under_line, under, here, targets)?;
        *next = after;
        let other =
            if self.rows.get(after).is_some_and(|row| {
                row.indent == depth && row.text.trim_end_matches(':') == "otherwise"
            }) {
                let header = after.saturating_add(1);
                if self.rows.get(header).is_none_or(|row| row.indent != under) {
                    return Err(self.error(
                        header,
                        "`otherwise:` begins a block: write its steps under it",
                    ));
                }
                let (steps, end) = self.block(shared, header, under, here, targets)?;
                *next = end;
                Some(steps)
            } else {
                None
            };
        Ok((Some(children), other))
    }

    /// Read one line as a step, given what its block has bound so far.
    fn step(
        &self,
        shared: &Shared<'_>,
        index: usize,
        targets: &Targets,
    ) -> Result<Read, TextParseError> {
        let row = self
            .rows
            .get(index)
            .ok_or_else(|| self.error(index, "that line is not there"))?;
        let (body, condition) = match split_colon(row.text) {
            Some((body, condition)) => (body, Some(condition)),
            None => (row.text, None),
        };
        if body == "otherwise" {
            return Err(self.error(
                index,
                "`otherwise` follows the steps of the block it is the other side of",
            ));
        }
        for phrase in prefixes(body).into_iter().rev() {
            if let Some(param) = ask_param(shared, phrase) {
                let step = Step::new(String::new(), StepKind::Ask { param });
                if condition.is_some() {
                    return Err(self.error(index, "a step that asks has no condition"));
                }
                return Ok(Read { step, open: false });
            }
        }
        for phrase in prefixes(body).into_iter().rev() {
            if let Some(step) = self.control(shared, phrase, body, condition, targets, index)? {
                return Ok(Read {
                    step,
                    open: opens_block(phrase),
                });
            }
        }
        for phrase in prefixes(body).into_iter().rev() {
            if let Some(step) = self.invocation(shared, phrase, body, condition, targets, index)? {
                return Ok(Read { step, open: false });
            }
        }
        Err(self.error(
            index,
            "this is not a step: begin it with what it does, or with `when`, `for each` or `set`",
        ))
    }
}

impl Read {
    /// The step, with the children its line opened.
    fn finish(mut self, children: Option<Vec<Step>>, otherwise: Option<Vec<Step>>) -> Step {
        let children = children.unwrap_or_default();
        match &mut self.step.kind {
            StepKind::If {
                then,
                otherwise: alt,
                ..
            } => {
                *then = children;
                *alt = otherwise.unwrap_or_default();
            }
            StepKind::ForEach { body, .. } => *body = children,
            _ => {}
        }
        self.step
    }
}

/// Whether a phrase heads a step that its own block follows.
fn opens_block(phrase: &str) -> bool {
    phrase.starts_with("for each ") || phrase.starts_with("tell ") || phrase == "when"
}

impl Reader<'_> {
    /// Read a line that heads a block: `when`, `for each`, `set`, and the
    /// steps that ask the agent.
    fn control(
        &self,
        shared: &Shared<'_>,
        phrase: &str,
        body: &str,
        condition: Option<&str>,
        targets: &Targets,
        index: usize,
    ) -> Result<Option<Step>, TextParseError> {
        let rest = body.get(phrase.len()..).unwrap_or_default().trim_start();
        let number = self.rows.get(index).map_or(1, |row| row.number);
        let kind = if phrase == "when" {
            let Some(condition) = condition else {
                return Err(self.error(
                    index,
                    "`when` heads a block: write its condition after a colon",
                ));
            };
            StepKind::If {
                cond: condition_of(shared, condition, targets)
                    .map_err(|message| TextParseError::at(number, message))?,
                then: Vec::new(),
                otherwise: Vec::new(),
            }
        } else if phrase.starts_with("for each ") {
            // The phrase stops at a space, so a longer one still carries the
            // `in` the loop's items follow; read the item name off the start.
            let after_phrase = body.get("for each ".len()..).unwrap_or_default();
            let (name, after) = split_word(after_phrase).ok_or_else(|| {
                self.error(
                    index,
                    "a loop's item is a word like `line`, after `for each`",
                )
            })?;
            let name = read_name(name).ok_or_else(|| {
                self.error(
                    index,
                    "a loop's item is a word like `line`, after `for each`",
                )
            })?;
            let rest = after
                .trim_start()
                .strip_prefix("in ")
                .ok_or_else(|| {
                    self.error(
                        index,
                        "say what the loop goes over, as `for each line in …`",
                    )
                })?
                .trim_start();
            let items =
                binding_of(rest, targets).map_err(|message| TextParseError::at(number, message))?;
            StepKind::ForEach {
                items,
                item: name,
                body: Vec::new(),
            }
        } else if let Some(name) = phrase.strip_prefix("set ") {
            let name = read_name(name).ok_or_else(|| {
                self.error(index, "a variable is a word like `total`, after `set`")
            })?;
            let value = binding_of(rest.strip_prefix("to ").unwrap_or(rest), targets)
                .map_err(|message| TextParseError::at(number, message))?;
            StepKind::SetVar { name, value }
        } else if phrase.starts_with("ask AI ") {
            let words = text_of(body.get(phrase.len()..).unwrap_or_default())
                .map_err(|message| TextParseError::at(number, message))?;
            StepKind::Ai {
                prompt: words,
                output: None,
                input: None,
            }
        } else if opens_block(phrase) {
            let words = text_of(body.get(phrase.len()..).unwrap_or_default())
                .map_err(|message| TextParseError::at(number, message))?;
            StepKind::Task {
                instruction: words,
                allow: Vec::new(),
                output: None,
            }
        } else {
            return Ok(None);
        };
        Ok(Some(Step::new(String::new(), kind)))
    }
}

/// The step an action's phrase heads, read from the catalogue row.
fn action_step(row: &CatalogueRow) -> Option<Step> {
    let descriptor = row.action.as_ref()?;
    if descriptor.action_label.is_empty() {
        return None;
    }
    let key = ActionKey::new(
        row.app_id.clone(),
        descriptor.resource_kind.clone(),
        descriptor.address.action.clone(),
    );
    // The text names a kind — "on any note" — so the step takes whatever item
    // of that kind is to hand, not one particular item's id.
    let selector = Selector::FirstOfKind(row.resource.kind.clone());
    Some(Step::new(
        String::new(),
        StepKind::Invoke {
            action: key,
            resource: selector,
            args: BTreeMap::new(),
            appearance: None,
        },
    ))
}

/// Read a line that invokes an action: the phrase the catalogue gives it,
/// then its arguments as `label value` notes.
fn invocation(
    shared: &Shared<'_>,
    row: &CatalogueRow,
    phrase: &str,
    body: &str,
    targets: &Targets,
    number: usize,
) -> Result<Step, TextParseError> {
    let Some(mut step) = action_step(row) else {
        return Err(TextParseError::at(number, "this step names no action"));
    };
    let Some(descriptor) = row.action.as_ref() else {
        return Err(TextParseError::at(number, "this step names no action"));
    };
    let mut rest = body
        .get(phrase.len()..)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut remaining = descriptor.params.clone();
    let mut args: BTreeMap<String, Binding> = BTreeMap::new();
    let mut appearance = None;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("shown as its author") {
            appearance = Some(Binding::Input(AUTHOR.to_owned()));
            rest = after.trim_start().to_string();
            continue;
        }
        let Some((param, after)) = longest_param(&rest, &remaining) else {
            return Err(TextParseError::at(
                number,
                format!("`{}` is not something this step takes", rest.trim()),
            ));
        };
        let (value, after) = binding_and_rest(shared, after.trim_start(), targets)
            .map_err(|message| TextParseError::at(number, message))?;
        remaining.retain(|other| other.name != param.name);
        args.insert(param.name.to_string(), value);
        rest = after.trim_start().to_string();
    }
    if let StepKind::Invoke {
        args: bound,
        appearance: shown,
        ..
    } = &mut step.kind
    {
        *bound = args;
        *shown = appearance;
    }
    Ok(step)
}

/// The input whose value is shown as a bot's name and avatar.
const AUTHOR: &str = "author";

// ----------------------------------------------------------- the reader's ---

/// The first word of `text`, and what follows it.
fn split_word(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    let end = text.find(' ')?;
    let (word, rest) = text.split_at(end);
    (!word.is_empty()).then_some((word, rest))
}

/// A name the text writes: an identifier, which is what a program holds.
fn read_name(text: &str) -> Option<String> {
    let name = text.trim();
    is_identifier(name).then(|| name.to_owned())
}

/// A value read from the start of `text`, and what follows it.
fn binding_and_rest(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Binding, String), String> {
    let text = text.trim_start();
    if text.starts_with('"') {
        let (words, rest) = text_at(text)?;
        return Ok((template_of(words), rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix('[') {
        return list_at(shared, rest, targets);
    }
    if let Some(rest) = text.strip_prefix('{') {
        return object_at(shared, rest, targets);
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
fn binding_of(text: &str, targets: &Targets) -> Result<Binding, String> {
    let (binding, rest) = binding_and_rest(&Shared::empty(), text, targets)?;
    if !rest.trim().is_empty() {
        return Err(format!("`{}` follows the value", rest.trim()));
    }
    Ok(binding)
}

/// A name a step above bound: `input NAME`, `variable NAME`, or the phrase a
/// step reads as, which names its result.
fn named_binding(word: &str, targets: &Targets) -> Option<Binding> {
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
fn named_value(name: &str) -> Option<Binding> {
    if let Some(given) = name.strip_prefix("input ") {
        let (base, pointer) = Binding::named_part(given);
        return is_identifier(base).then(|| Binding::Input(format!("{base}{pointer}")));
    }
    let given = name.strip_prefix("variable ")?;
    let (base, pointer) = Binding::named_part(given);
    is_identifier(base).then(|| Binding::Var(format!("{base}{pointer}")))
}

/// A pointer back from the tokens the text writes it as.
fn pointer_of(text: &str) -> Option<String> {
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

fn number_at(text: &str) -> Option<(Value, &str)> {
    let end = text
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E'))
        .map(|(index, ch)| index.saturating_add(ch.len_utf8()))
        .last()?;
    let (head, rest) = text.split_at(end);
    let number: Number = head.parse().ok()?;
    Some((Value::Number(number), rest))
}

fn list_at(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Binding, String), String> {
    let mut items = Vec::new();
    let mut owned = text.trim_start().to_owned();
    loop {
        let rest = owned.as_str();
        if let Some(after) = rest.strip_prefix(']') {
            return Ok((Binding::List(items), after.to_owned()));
        }
        let (item, after) = binding_and_rest(shared, rest, targets)?;
        items.push(item);
        let next = after
            .trim_start()
            .strip_prefix(',')
            .ok_or_else(|| "separate a list's values with commas".to_owned())?;
        owned.clear();
        owned.push_str(next);
    }
}

fn object_at(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Binding, String), String> {
    let mut members: BTreeMap<String, Binding> = BTreeMap::new();
    let mut owned = text.trim_start().to_owned();
    loop {
        let rest = owned.as_str();
        if let Some(after) = rest.strip_prefix('}') {
            return Ok((Binding::Object(members), after.to_owned()));
        }
        let (name, after) = text_at(rest)?;
        let after = after
            .trim_start()
            .strip_prefix(':')
            .ok_or_else(|| "write a dictionary's entries as `\"name\": value`".to_owned())?
            .trim_start();
        let (value, after) = binding_and_rest(shared, after, targets)?;
        members.insert(name, value);
        let next = after
            .trim_start()
            .strip_prefix(',')
            .ok_or_else(|| "separate a dictionary's entries with commas".to_owned())?;
        owned.clear();
        owned.push_str(next);
    }
}

/// A quoted text as the binding it is: a plain text, or a template when it
/// holds `{...}` placeholders.
fn template_of(words: String) -> Binding {
    if !words.contains('{') {
        return Binding::Literal(Value::String(words));
    }
    Binding::Template(words)
}

/// A condition read from a whole line.
fn condition_of(shared: &Shared<'_>, text: &str, targets: &Targets) -> Result<Condition, String> {
    let (condition, rest) = condition_and_rest(shared, text, targets, 0)?;
    if !rest.trim().is_empty() {
        return Err(format!("`{}` follows the condition", rest.trim()));
    }
    Ok(condition)
}

/// A condition read from the start of `text`, and what follows it. `and`
/// binds tighter than `or`, and parentheses group.
fn condition_and_rest(
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

fn condition_or_rest(
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
fn reading_of(
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
fn input_of(text: &str) -> Result<ActionParam, String> {
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
    let mut nullable = true;
    if let Some(after) = words.strip_prefix(',') {
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix("not nothing") {
            nullable = false;
            words = after.trim_start();
        } else if let Some(after) = after.strip_prefix("may be nothing") {
            words = after.trim_start();
        }
    }
    let mut param = ActionParam::required_owned(name, type_of(kind).unwrap_or(ParamType::Text), "");
    if !nullable {
        param.nullable = false;
    }
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
    param.description = description.to_owned().into();
    param.choices = choices.into_iter().map(Into::into).collect();
    param.nullable = nullable;
    Ok(param)
}

/// The values a declaration lists as the only ones it accepts.
fn choices_of(text: &str) -> Result<Vec<String>, String> {
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

fn seconds_of(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let seconds = text
        .strip_suffix(" seconds")
        .or_else(|| text.strip_suffix(" second"))
        .ok_or_else(|| "write how long as `N seconds`".to_owned())?;
    seconds
        .trim()
        .parse()
        .map_err(|_| "that is not a number of seconds".to_owned())
}

/// Whether a line declares a trigger.
fn is_trigger(text: &str) -> bool {
    [
        "when someone presses ",
        "every ",
        "when the app opens ",
        "when the network changes",
        "when a message arrives",
        "when a device comes near",
        "when focus ",
        "when someone joins ",
        "when a file changes in ",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

fn trigger_of(text: &str) -> Result<TriggerSpec, String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("when someone presses ") {
        let (chord, after) = text_at(rest)?;
        if !after.trim().is_empty() {
            return Err(format!("`{}` follows the keys", after.trim()));
        }
        return Ok(TriggerSpec::Keyboard { chord });
    }
    if let Some(rest) = text.strip_prefix("when the app opens ") {
        let (app_id, after) = text_at(rest)?;
        if !after.trim().is_empty() {
            return Err(format!("`{}` follows the app", after.trim()));
        }
        return Ok(TriggerSpec::AppOpened { app_id });
    }
    if let Some(rest) = text.strip_prefix("when focus ") {
        let Some((mode, after)) = rest.split_once(" is ") else {
            return Err("write `when focus MODE is entered` or `is left`".to_owned());
        };
        let transition = match after.trim() {
            "entered" => crate::shortcut::FocusTransition::Enter,
            "left" => crate::shortcut::FocusTransition::Leave,
            other => return Err(format!("`{other}` is not `entered` or `left`")),
        };
        return Ok(TriggerSpec::FocusMode {
            mode: mode.trim().to_owned(),
            transition,
        });
    }
    if let Some(rest) = text.strip_prefix("when someone joins ") {
        if rest.trim() == "a workspace" {
            return Ok(TriggerSpec::WorkspaceJoined { room: None });
        }
        let (room, after) = text_at(rest)?;
        if !after.trim().is_empty() {
            return Err(format!("`{}` follows the workspace", after.trim()));
        }
        return Ok(TriggerSpec::WorkspaceJoined { room: Some(room) });
    }
    if let Some(rest) = text.strip_prefix("when a file changes in ") {
        let (capability, after) = text_at(rest)?;
        if !after.trim().is_empty() {
            return Err(format!("`{}` follows the folder", after.trim()));
        }
        return Ok(TriggerSpec::FileChanged { capability });
    }
    if text.starts_with("when the network changes") || text.starts_with("when a message arrives") {
        return Err(format!(
            "`{text}` says what changes but not what to match, which the text does not write yet"
        ));
    }
    if let Some(rest) = text.strip_prefix("when a device comes near") {
        let _ = rest;
        return Ok(TriggerSpec::NearbyDevice { device: None });
    }
    Err(format!("`{text}` is not a trigger"))
}

fn rule_of(text: &str) -> Result<Rule, String> {
    let (answer, rest) = if let Some(rest) = text.strip_prefix("ask before it acts on ") {
        (Answer::Ask, rest)
    } else if let Some(rest) = text.strip_prefix("act on ") {
        (Answer::Allow, rest)
    } else if let Some(rest) = text.strip_prefix("never act on ") {
        (Answer::Deny, rest)
    } else {
        return Err(
            "write what the shortcut may do as `act on PLACE`, `ask before it acts on PLACE` or `never act on PLACE`"
                .to_owned(),
        );
    };
    let (place, kinds) = match rest.split_once(" for ") {
        Some((place, kinds)) => (place, Some(kinds)),
        None => (rest, None),
    };
    let kinds = match kinds {
        None => KindSet::ALL,
        Some(words) => {
            let mut set = KindSet::NONE;
            for word in words.split(", ") {
                set = set.with(match word.trim() {
                    "reading" => ActionKind::Read,
                    "changes" => ActionKind::Undoable,
                    "outward" => ActionKind::Outside,
                    other => return Err(format!("`{other}` is not a kind of action")),
                });
            }
            set
        }
    };
    let node = place
        .parse::<TreePath>()
        .map_err(|error| error.to_string())?;
    Ok(Rule::new(node, kinds, answer))
}

/// Every reading of a `when` line's condition, checked against the program's
/// own vocabulary.
fn ask_param(shared: &Shared<'_>, phrase: &str) -> Option<ActionParam> {
    let asked = phrase.strip_prefix("ask for ")?;
    let _ = shared;
    Some(ActionParam::required_owned(
        asked.to_owned(),
        ParamType::Text,
        asked.to_owned(),
    ))
}

// ---------------------------------------------------------------- naming ---

/// The step names a program refers to its own results by, so a rendering can
/// tell a name it must write from one nothing reads.
fn referred(shortcut: &Shortcut) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_references(&shortcut.steps, &mut names);
    if let Some(output) = &shortcut.output {
        binding_names(output, &mut names);
    }
    names
}

fn collect_references(steps: &[Step], names: &mut BTreeSet<String>) {
    for step in steps {
        let mut args = Vec::new();
        match &step.kind {
            StepKind::Invoke {
                args: bound,
                resource,
                appearance,
                ..
            } => {
                args.extend(bound.values().cloned());
                args.push(resource_binding(resource));
                args.extend(appearance.iter().cloned());
            }
            StepKind::If {
                cond,
                then,
                otherwise,
            } => {
                condition_names(cond, names);
                collect_references(then, names);
                collect_references(otherwise, names);
            }
            StepKind::ForEach { items, body, .. } => {
                args.push(items.clone());
                collect_references(body, names);
            }
            StepKind::SetVar { value, .. } => args.push(value.clone()),
            StepKind::Ai { input, .. } => args.extend(input.iter().cloned()),
            StepKind::Ask { .. } | StepKind::Task { .. } => {}
        }
        for arg in args {
            binding_names(&arg, names);
        }
    }
}

fn resource_binding(resource: &Selector) -> Binding {
    match resource {
        Selector::Fixed(id) => Binding::Literal(Value::String(id.clone())),
        Selector::FirstOfKind(kind) => Binding::Literal(Value::String(kind.clone())),
        Selector::FromBinding(binding) => binding.clone(),
    }
}

fn binding_names(binding: &Binding, names: &mut BTreeSet<String>) {
    match binding {
        Binding::StepOutput { step, .. } => {
            names.insert(step.clone());
        }
        Binding::Template(text) => {
            if let Some(bindings) = placeholder_bindings(text) {
                for binding in bindings {
                    binding_names(&binding, names);
                }
            }
        }
        Binding::Object(members) => {
            for member in members.values() {
                binding_names(member, names);
            }
        }
        Binding::List(items) => {
            for item in items {
                binding_names(item, names);
            }
        }
        Binding::Literal(_) | Binding::Input(_) | Binding::Var(_) => {}
    }
}

fn condition_names(condition: &Condition, names: &mut BTreeSet<String>) {
    match condition {
        Condition::Truthy(binding) => binding_names(binding, names),
        Condition::Equals { left, right } => {
            binding_names(left, names);
            binding_names(right, names);
        }
        Condition::Not(inner) => condition_names(inner, names),
        Condition::All(all) | Condition::Any(all) => {
            for inner in all {
                condition_names(inner, names);
            }
        }
    }
}

// -------------------------------------------------------------- conditions ---

/// A condition as its line reads it.
fn condition_render(condition: &Condition, namings: &[Naming]) -> String {
    match condition {
        Condition::Truthy(binding) => format!("{} is set", value_text(binding, namings)),
        Condition::Equals { left, right } => format!(
            "{} is {}",
            value_text(left, namings),
            value_text(right, namings)
        ),
        Condition::Not(inner) => {
            let inner = condition_render(inner, namings);
            if inner.contains(" and ") || inner.contains(" or ") {
                format!("not ({inner})")
            } else {
                format!("not {inner}")
            }
        }
        Condition::All(all) => all
            .iter()
            .map(|inner| {
                let text = condition_render(inner, namings);
                if text.contains(" or ") {
                    format!("({text})")
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
            .join(" and "),
        Condition::Any(all) => all
            .iter()
            .map(|inner| condition_render(inner, namings))
            .collect::<Vec<_>>()
            .join(" or "),
    }
}

// ----------------------------------------------------------- the reader's ---

impl Shared<'_> {
    /// A rendering that reads no catalogue: enough to read the values of a
    /// line that names no action.
    #[must_use]
    pub fn empty() -> Self {
        static CATALOG: std::sync::OnceLock<ActionCatalog> = std::sync::OnceLock::new();
        fn bare(app_id: &str) -> String {
            app_id.rsplit('.').next().unwrap_or(app_id).to_owned()
        }
        Self {
            catalog: CATALOG.get_or_init(ActionCatalog::default),
            app_label: &bare,
        }
    }
}

/// The arguments a line's notes name, longest label first, so one label that
/// begins another's words does not swallow it.
fn longest_param<'t>(text: &'t str, params: &[ActionParam]) -> Option<(ActionParam, &'t str)> {
    let mut best: Option<(ActionParam, &str)> = None;
    for param in params {
        let label = label_of(param);
        let Some(rest) = text.strip_prefix(&label) else {
            continue;
        };
        if !rest.starts_with(' ') {
            continue;
        }
        let longer = best
            .as_ref()
            .is_none_or(|(chosen, _)| label.len() > label_of(chosen).len());
        if longer {
            best = Some((param.clone(), rest));
        }
    }
    best
}

impl Reader<'_> {
    /// Read a line that names an action: the phrase the catalogue gives it,
    /// then its arguments as `label value` notes.
    fn invocation(
        &self,
        shared: &Shared<'_>,
        phrase: &str,
        body: &str,
        _condition: Option<&str>,
        targets: &Targets,
        index: usize,
    ) -> Result<Option<Step>, TextParseError> {
        let Some(row) = shared
            .catalog
            .rows()
            .iter()
            .find(|row| reads_any(row, &*shared.app_label).as_deref() == Some(phrase))
        else {
            return Ok(None);
        };
        let number = self.rows.get(index).map_or(1, |row| row.number);
        invocation(shared, row, phrase, body, targets, number).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
