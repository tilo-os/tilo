//! A shortcut as text people can read and type.
//!
//! The document holds the program; this is a rendering of it. One line is one
//! step, nested blocks are indented, and a step reads as the phrase its action
//! has in the catalogue followed by named arguments — the same words the
//! visual editor shows, in the same order, so a step is in one place in both:
//!
//! ```text
//! shortcut "File the invoices"
//! input "folder" is text, not nothing, always, called "Where the invoices arrived"
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

use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, KindSet, Rule, Segment, TreePath};
use tilo_actions::ActionKey;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::CatalogueRow;

use crate::catalog::{ActionCatalog, ResourceTarget, kind_matches};
use crate::path::StepPath;
use crate::shortcut::{Binding, Condition, Selector, Shortcut, Step, StepKind};
use crate::template::is_identifier;
use crate::{AGENT_APP_ID, AGENT_TRANSFORM_KIND};
use tilo_actions::contract::ResultSpec;

#[cfg(test)]
mod tests;
mod trigger;

pub(super) use crate::text_read::number_at;
use crate::text_read::{binding_and_rest, binding_of, condition_of, input_of};

/// Most bytes a rendering of a shortcut may have, so a pathologically nested
/// program cannot be turned into text without bound.
pub const MAX_TEXT_BYTES: usize = 262_144;

/// Most steps one program may have, so a deeply nested text is refused before
/// anything recurses over it.
pub(crate) const MAX_TEXT_STEPS: usize = 1_024;

/// The types a value of the language has a literal for.
pub(crate) const TYPES: [&str; 9] = [
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
    // The text names a step by the line it reads with, because a program
    // written by hand has no ids to write; the document names it by its id.
    // Every reference is pointed at the step the text meant, so an edited
    // program reads a result from the step that now holds it.
    let mut names = BTreeMap::new();
    note_phrases(&steps, shared, &mut names);
    let mut shortcut = Shortcut {
        // Identity, and everything the text does not write, is the document's:
        // only the steps and what the text states change.
        id: before.id.clone(),
        saved_by: before.saved_by.clone(),
        saved_proof: before.saved_proof.clone(),
        steps,
        ..parsed.shortcut
    };
    crate::text_read::rebind(&mut shortcut, &names);
    Ok(Edit { shortcut, removed })
}

/// What each step in the program reads as, and the id it now holds.
fn note_phrases(steps: &[Step], shared: &Shared<'_>, names: &mut BTreeMap<String, String>) {
    // The names are the ones [`name_block`] writes, numbering and all, so a
    // line that refers to the second of two steps that read alike finds the
    // step it meant.
    for naming in name_block(shared, steps) {
        if let Some(refer) = naming.refer {
            names.insert(naming.phrase.clone(), refer);
        }
    }
    for step in steps {
        // A step that asks holds its result in a variable the text names by
        // that variable, so the name points at the step as well.
        if !step.id.is_empty()
            && matches!(step.kind, StepKind::Ask { .. })
            && let StepKind::Ask { param } = &step.kind
        {
            names.insert(param.name.to_string(), step.id.clone());
        }
        match &step.kind {
            StepKind::If {
                then, otherwise, ..
            } => {
                note_phrases(then, shared, names);
                note_phrases(otherwise, shared, names);
            }
            StepKind::ForEach { body, .. } => note_phrases(body, shared, names),
            _ => {}
        }
    }
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
        let line = trigger::trigger_text(trigger);
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
        || trigger::is_trigger(text)
}

/// The name a step's result is referred to by — the phrase the step reads
/// with — when the program refers to that result at all.
///
/// An invocation is always named: its line carries what the action acts on,
/// and the text writes a resource the program names by the name a step above
/// gave it, so the step has to have one whether or not anything reads it.
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
        StepKind::SetVar { name, .. } => format!("set {name}"),
        StepKind::Ask { param } => {
            let words = &param.description;
            // The name is only written where it is not the one the question
            // reads as, so a plain question stays a plain question.
            let words = quoted(words);
            if param.name == identifier_of(param.description.trim()) {
                format!("ask for {words}")
            } else {
                format!("ask for {words} as {}", quoted(&param.name))
            }
        }
        // A prompt and an instruction are words the person wrote, quoted the
        // way any text the program holds is, so one that reads like the rest
        // of the line — with a colon, a comma, `on` — still reads back.
        StepKind::Ai { prompt, .. } => format!("ask AI {}", quoted(prompt)),
        StepKind::Task { instruction, .. } => format!("tell SaaSy {}", quoted(instruction)),
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
    let target = target_of(resource);
    if let Some(row) = shared.catalog.find(action, target) {
        return reads_target(row, resource, shared.app_label)
            .unwrap_or_else(|| fallback_title(action, target, &app));
    }
    fallback_title(action, target, &app)
}

/// What a step reads as when the catalogue has no words for it.
///
/// The fallback carries what the step acts on, so the line names the
/// resource the step holds and reads back as that step rather than as the one
/// that acts on whatever is to hand. The kind is in the same readable form the
/// catalogue's own words use, which is the form the reader turns back into a
/// kind, so a step reads the same whether or not the catalogue knows it.
fn fallback_title(action: &ActionKey, target: ResourceTarget<'_>, app: &str) -> String {
    match target {
        // A resource id is quoted wherever the text writes one, so an id that
        // holds a space reads back as the one id it is.
        ResourceTarget::Id(id) => format!("{} on {} in {app}", action.action, quoted(id)),
        ResourceTarget::Kind(kind) => {
            format!("{} on any {} in {app}", action.action, readable(kind))
        }
        // What the step acts on is a value, and the note that follows says
        // what it holds: the phrase names only the action and the app.
        ResourceTarget::Any => format!("{} in {app}", action.action),
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
            // What the program names is written out, so the line reads back
            // as the step the document holds and not as one that acts on
            // whatever is to hand: the one resource by its id, the kind a step
            // takes whatever item of, and the item a binding yields.
            //
            // The note is only for the resource the phrase leaves out. The
            // phrase says it whenever the catalogue has words for the step —
            // `file on any Notes.note in Notes` — and the fallback says it too,
            // in the program's own spelling. A note that repeats what the line
            // already says reads as a step acting on something else.
            let note = match resource {
                Selector::Fixed(id) => Some(format!("on {}", quoted(id))),
                Selector::FirstOfKind(kind) => Some(format!("on any {}", readable(kind))),
                Selector::FromBinding(binding) => {
                    Some(format!("on {}", value_text(binding, namings)))
                }
            };
            let stated = note
                .as_ref()
                .is_some_and(|note| format!(" {phrase} ").contains(&format!(" {note} ")));
            if let (false, Some(note)) = (stated, note) {
                notes.push(note);
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
    // A hint is the row the catalogue gave this step, and it speaks for the
    // step only where it has one: a step whose resource the catalogue has no
    // row for still takes the arguments the action states.
    let hinted = hint
        .and_then(|hint| hint.row.as_ref())
        .and_then(|row| row.action.as_ref())
        .map(|action| action.params.clone());
    if let Some(params) = hinted {
        return Some(params);
    }
    let StepKind::Invoke {
        action, resource, ..
    } = &step.kind
    else {
        return None;
    };
    // The parameters of the action as the catalogue states them for what the
    // step acts on, and — for a resource the catalogue has no row for — as it
    // states them for the kind, because the action takes the same arguments
    // whichever of its resources it runs on.
    let row = shared
        .catalog
        .find(action, target_of(resource))
        .or_else(|| {
            shared
                .catalog
                .find(action, ResourceTarget::Kind(&action.kind))
        })?;
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

/// A resource kind as the text writes it back: [`readable`]'s words, in lower
/// case.
///
/// The text names a kind by the words a person reads — `Notes.note` — and each
/// kind's own tail is a word of the kind (`files.entry.image`). A kind whose
/// words do not read back as itself — one whose `_` or `-` would come back as
/// a space, or one with a capital inside a word — is written as the id it is,
/// so the step keeps the kind the program holds.
fn readable_back(words: &str) -> String {
    let kind = words.trim().to_lowercase();
    if readable(&kind) == words.trim() {
        kind
    } else {
        words.trim().to_owned()
    }
}

/// A binding as the text writes it: a literal where one is written, and
/// otherwise the name the program refers to the value by.
fn value_text(binding: &Binding, namings: &[Naming]) -> String {
    match binding {
        // A list or a dictionary the program holds as a literal is written,
        // and read back, as the list or dictionary it is: the text has one
        // spelling for it, so the shape a value came in never changes it.
        Binding::Literal(Value::Array(items)) => value_text(
            &Binding::List(
                items
                    .iter()
                    .map(|item| Binding::Literal(item.clone()))
                    .collect(),
            ),
            namings,
        ),
        Binding::Literal(Value::Object(members)) => value_text(
            &Binding::Object(
                members
                    .iter()
                    .map(|(name, member)| (name.clone(), Binding::Literal(member.clone())))
                    .collect(),
            ),
            namings,
        ),
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
pub(super) fn literal_text(value: &Value) -> String {
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
                    // A placeholder is the value it stands for, said the way
                    // the rest of the line says a value: `{{List notes …}}`
                    // for a step's result, `{{input folder}}` for an input.
                    let _ = write!(written, "{{{{{}}}}}", value_text(binding, namings));
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

pub(super) fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}

// ----------------------------------------------------------- declarations ---

fn input_text(param: &ActionParam) -> String {
    let mut line = format!(
        "input {} is {}, {}, {}",
        quoted(param.name.as_ref()),
        type_name(param.ty),
        if param.nullable {
            "may be nothing"
        } else {
            "not nothing"
        },
        // Whether the shortcut needs a value for it: an input the person may
        // leave out is a different shortcut from one they must fill in.
        if param.required { "always" } else { "if given" },
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

pub(crate) fn type_of(name: &str) -> Option<ParamType> {
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

/// A segment of a grant's place, written the way a place is read back:
/// `app:so.tilo.notes` names one app, `kind:*` any kind, and `**` any depth.
fn segment_text(segment: &Segment) -> String {
    match segment {
        Segment::Rest => "**".to_owned(),
        Segment::Any(level) => format!("{}:*", level.tag()),
        Segment::Name(level, id) => format!("{}:{id}", level.tag()),
    }
}

/// The `on any <kind>` form of the same action: what a step reads when it
/// takes the first resource of a kind rather than one resource.
/// The line a catalogue row's action reads as, naming the resource the step
/// acts on: a step that takes whatever item of the kind is to hand, or one
/// that takes the very item the program names.
fn reads_target(
    row: &CatalogueRow,
    resource: &Selector,
    app_label: &dyn Fn(&str) -> String,
) -> Option<String> {
    let descriptor = row.action.as_ref()?;
    if descriptor.action_label.is_empty() {
        return None;
    }
    let app = app_label(&row.app_id);
    let action = &descriptor.action_label;
    // A step that acts on what a binding yields says the action and the app
    // and nothing about the resource: the value is written as the note that
    // follows, which is the only place a program says what it acts on.
    Some(match resource {
        Selector::Fixed(id) => format!("{action} on {} in {app}", quoted(id)),
        Selector::FirstOfKind(kind) => format!("{action} on any {} in {app}", readable(kind)),
        Selector::FromBinding(_) => format!("{action} in {app}"),
    })
}

/// The line the reader takes a phrase back to a step by: the same words
/// [`reads_any`] shows, which name no particular resource. Only a step that
/// takes whatever item of the kind is to hand reads this way; a step that
/// names one resource reads every line with [`reads_target`], so the line it
/// states and the phrase the reader matches carry what the step acts on.
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
pub(crate) struct Targets {
    bindings: Vec<Binding>,
    phrases: Vec<String>,
}

impl Targets {
    pub(crate) fn value_of(&self, phrase: &str) -> Option<&Binding> {
        let at = self.phrases.iter().position(|name| name == phrase)?;
        self.bindings.get(at)
    }

    /// Note a step's name, for a test that cares how two names that read
    /// alike are told apart rather than about the step behind either.
    #[cfg(test)]
    fn note_name(&mut self, phrase: &str) {
        self.bindings.push(Binding::step(phrase, ""));
        self.phrases.push(phrase.to_owned());
    }

    /// Note what a line's result is called, for the lines after it.
    ///
    /// Two lines that read alike are told apart by a number after the second,
    /// which is how [`name_block`] names them and so how the text writes the
    /// one a later line refers to.
    fn note(&mut self, phrase: &str, step: &Step) {
        let mut binding = match &step.kind {
            StepKind::If { .. } => return,
            StepKind::ForEach { item, .. } => Binding::Var(item.clone()),
            StepKind::SetVar { name, .. } => Binding::Var(name.clone()),
            StepKind::Ask { param } => Binding::Var(param.name.to_string()),
            // A step that asks the agent names its result by the words the
            // text writes it with, which is also what the line after it refers
            // to, so both names reach the same step.
            StepKind::Ai { .. } | StepKind::Task { .. } => {
                match refer_name_step(&Shared::empty(), step) {
                    Some(named) => {
                        if !self.phrases.contains(&named) {
                            self.bindings.push(Binding::Var(named.clone()));
                            self.phrases.push(named.clone());
                        }
                        Binding::Var(named)
                    }
                    None => Binding::step(phrase, ""),
                }
            }
            StepKind::Invoke { .. } => Binding::step(phrase, ""),
        };
        let mut name = phrase.to_owned();
        let mut number = 1usize;
        while self.phrases.contains(&name) {
            number = number.saturating_add(1);
            name = format!("{phrase} {number}");
        }
        // A step referred to by the line it reads with is referred to by the
        // name the text writes, numbering and all, so a later line that names
        // the second of two lines that read alike still means that step when
        // the program is read back without the ids the document gives it.
        if name != phrase
            && let Binding::StepOutput { step: referred, .. } = &mut binding
        {
            name.clone_into(referred);
        }
        self.bindings.push(binding);
        self.phrases.push(name);
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
    let mut brackets = 0usize;
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
            // A dictionary's entry separates its name from its value with the
            // same colon a block's condition follows, so only a colon outside
            // every list and dictionary is the condition's.
            '{' | '[' => brackets = brackets.saturating_add(1),
            '}' | ']' => brackets = brackets.saturating_sub(1),
            ':' if brackets == 0
                && text
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

/// `text` cut at each place one value can end outside a quoted text, shortest
/// first, so the longest one that names something is the reading it is given.
pub(crate) fn prefixes(text: &str) -> Vec<&str> {
    let spaces = text
        .char_indices()
        .filter(|(index, ch)| ends_a_value(*ch) && !inside_quote(text, *index))
        .map(|(index, _)| index);
    spaces
        .chain(std::iter::once(text.len()))
        .filter_map(|end| text.get(..end))
        .collect()
}

/// Whether a value may end just before `ch`: at a space, where the next word
/// starts, and where a list's or a dictionary's members are separated.
fn ends_a_value(ch: char) -> bool {
    matches!(ch, ' ' | ',' | ']' | '}')
}

/// A quoted text, and what follows the closing quote.
pub(super) fn text_at(text: &str) -> Result<(String, &str), String> {
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
pub(crate) fn text_of(text: &str) -> Result<String, String> {
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
            saved_proof: None,
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
        let mut targets = Targets::default();
        let (steps, mut next) =
            self.block(shared, index, 0, &root, &Targets::default(), &mut targets)?;
        // What the shortcut gives back, and what starts it, read after the
        // steps: both name results the steps above them have, by the names the
        // block they are written in gave them.
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
            // A text a person cut a step out of still says what the shortcut
            // gives back, and the step it names is the one that left: keeping
            // the name lets the cut be reported as a step leaving.
            let given = match binding_of(rest, targets) {
                Ok(given) => given,
                // Only a whole line that names a step, and nothing else, is
                // kept as the result of the step the text no longer states.
                Err(message) => match unstated_step(rest) {
                    Some(step) => step,
                    None => return Err(TextParseError::at(number, message)),
                },
            };
            shortcut.output = Some(given);
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
        if trigger::is_trigger(text) {
            shortcut
                .triggers
                .push(trigger::trigger_of(text).map_err(|m| TextParseError::at(number, m))?);
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
        after: &mut Targets,
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
        // The lines after the block refer to what it set, as the lines inside
        // it did, so what a top-level block named carries out of it.
        *after = targets;
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
        let mut unused = Targets::default();
        let (children, after) =
            self.block(shared, under_line, under, here, targets, &mut unused)?;
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
                let (steps, end) = self.block(shared, header, under, here, targets, &mut unused)?;
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

/// Whether a phrase heads a step that its own block follows. What an agent is
/// told is quoted on its own line, so it has no block under it.
fn opens_block(phrase: &str) -> bool {
    phrase.starts_with("for each ") || phrase == "when"
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
        } else if let Some(named) = phrase.strip_prefix("set ") {
            // The line's phrase runs to the end of the value, because a value
            // the program states is part of what the step says: the variable
            // is the word after `set` and the value follows the one after it.
            let (word, tail) = named.split_once(char::is_whitespace).unwrap_or((named, ""));
            let name = read_name(word).ok_or_else(|| {
                self.error(index, "a variable is a word like `total`, after `set`")
            })?;
            let tail = tail.trim_start();
            let value = binding_of(tail.strip_prefix("to ").unwrap_or(tail), targets)
                .map_err(|message| TextParseError::at(number, message))?;
            StepKind::SetVar { name, value }
        } else if phrase.starts_with("ask AI ") {
            // What the agent is asked is quoted, so the whole line is read
            // from the head rather than from the phrase: a phrase that stops
            // inside the words would not find the closing quotation mark.
            let asked = body.strip_prefix("ask AI ").unwrap_or_default();
            let (prompt, after) =
                text_at(asked).map_err(|message| TextParseError::at(number, message))?;
            let input = match after.trim_start() {
                "" => None,
                after => Some(
                    binding_of(
                        after.strip_prefix("of ").ok_or_else(|| {
                            TextParseError::at(
                                number,
                                format!("`{}` follows the prompt", after.trim()),
                            )
                        })?,
                        targets,
                    )
                    .map_err(|message| TextParseError::at(number, message))?,
                ),
            };
            StepKind::Ai {
                prompt,
                output: None,
                input,
            }
        } else if phrase.starts_with("tell SaaSy ") {
            let told = body.strip_prefix("tell SaaSy ").unwrap_or_default();
            let (instruction, after) =
                text_at(told).map_err(|message| TextParseError::at(number, message))?;
            let allow = rules_of(after).map_err(|message| TextParseError::at(number, message))?;
            StepKind::Task {
                instruction,
                allow,
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
    // The phrase names no particular resource, so until the line's `on …` note
    // says which, the step takes whatever item of the kind is to hand.
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

/// Read a line that invokes an action: what it acts on, then its arguments as
/// `label value` notes.
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
    // The resource the phrase leaves out is the first note of the line, so it
    // is read before any argument: `on "notes.note:1"` for the one resource
    // the program names, `on any note` for whatever of the kind is to hand.
    // The app the resource is named in follows it, as the phrase writes it.
    let app = (shared.app_label)(&row.app_id);
    let mut selector = None;
    if let Some(after) = rest.strip_prefix("on ") {
        let (read, tail) = selector_of(shared, after, targets)
            .map_err(|message| TextParseError::at(number, message))?;
        selector = Some(read);
        after_the_app(&tail, app.as_str()).clone_into(&mut rest);
    }
    let mut remaining = descriptor.params.clone();
    let mut args: BTreeMap<String, Binding> = BTreeMap::new();
    let mut appearance = None;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("shown as its author") {
            appearance = Some(Binding::Input(AUTHOR.to_owned()));
            rest = after.trim_start().to_string();
            continue;
        }
        // The resource note follows the arguments the phrase did not name, so
        // a line may state it here as well as before them.
        if let Some(after) = rest.strip_prefix("on ") {
            let (read, tail) = selector_of(shared, after, targets)
                .map_err(|message| TextParseError::at(number, message))?;
            selector = Some(read);
            after_the_app(&tail, app.as_str()).clone_into(&mut rest);
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
        resource,
        ..
    } = &mut step.kind
    {
        *bound = args;
        *shown = appearance;
        if let Some(selector) = selector {
            *resource = selector;
        }
    }
    Ok(step)
}

/// What follows a resource the line names: the app it is named in, when the
/// phrase states it, and then the rest of the line.
fn after_the_app<'t>(tail: &'t str, app: &str) -> &'t str {
    let tail = tail.trim_start();
    tail.strip_prefix("in ")
        .and_then(|named| named.strip_prefix(app))
        .unwrap_or(tail)
        .trim_start()
}

/// The resource a line's `on …` note names, as the selector the step holds.
///
/// A quoted resource id names that one resource; a binding names the resource
/// the run binds; and `any KIND` names the kind, so the step takes the first
/// resource of it that offers the action.
fn selector_of(
    shared: &Shared<'_>,
    text: &str,
    targets: &Targets,
) -> Result<(Selector, String), String> {
    if let Some(rest) = text.strip_prefix("any ") {
        // The kind is the words up to whatever the line says next: a step's
        // arguments follow it, and nothing after it belongs to the kind.
        let end = rest.find(' ').unwrap_or(rest.len());
        let (kind, after) = rest.split_at(end);
        return Ok((Selector::FirstOfKind(readable_back(kind)), after.to_owned()));
    }
    let (binding, rest) = binding_and_rest(shared, text, targets)?;
    Ok((
        // A literal string the program writes is a resource the program names,
        // so it is read back as the one spelling of that resource rather than
        // as a binding that happens to hold it. Everything else stays a
        // binding: it is what the value is at run time that decides.
        match binding {
            Binding::Literal(Value::String(id)) => Selector::Fixed(id),
            other => Selector::FromBinding(other),
        },
        rest,
    ))
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

/// The rules that follow what an agent is told, each introduced by `may`. A
/// rule's own words never contain `may`, so that is where one ends and the
/// next begins.
fn rules_of(text: &str) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let Some(after) = rest.strip_prefix("may ") else {
            return Err(format!("`{rest}` is not a rule: write `may act on PLACE`"));
        };
        let (next, tail) = match after.find(" may ") {
            Some(at) => (
                after.get(..at).unwrap_or_default(),
                after.get(at.saturating_add(1)..).unwrap_or_default(),
            ),
            None => (after, ""),
        };
        rules.push(rule_of(next.trim())?);
        rest = tail.trim();
    }
    Ok(rules)
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
    // The step is named by what it asks for, and the name the answer is held
    // under follows the question; where the program holds it under a name of
    // its own, the line says so. The question is read first, so a ` as ` inside
    // the words asked for is part of them rather than the name.
    let (words, after) = text_at(asked).ok()?;
    let name = match after.trim() {
        "" => identifier_of(&words),
        after => text_at(after.strip_prefix("as ")?).ok()?.0,
    };
    Some(ActionParam::required_owned(name, ParamType::Text, words))
}

/// The name a question is held under when the program does not name it: the
/// words it reads as, joined up as an identifier.
fn identifier_of(words: &str) -> String {
    let name: String = words
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = name.trim_matches('_');
    if trimmed.is_empty() {
        "answer".to_owned()
    } else {
        trimmed.to_owned()
    }
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

pub(crate) fn collect_references(steps: &[Step], names: &mut BTreeSet<String>) {
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

/// The input whose value is shown as a bot's name and avatar.
const AUTHOR: &str = "author";

/// The first word of `text`, and what follows it.
pub(super) fn split_word(text: &str) -> Option<(&str, &str)> {
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

/// The step a line names when the text states no such step.
///
/// A step's result is referred to by the words the step reads as, so a line
/// that gives a result back and names a step the text does not hold is the one
/// a person cut out. The words are kept as the step's name, which is what the
/// diff reports as leaving.
fn unstated_step(text: &str) -> Option<Binding> {
    let name = text.trim();
    // Only words: a value that starts with a bracket, a quote or a digit is a
    // value the reader refused, and stays refused.
    let mut words = name.split(' ').filter(|word| !word.is_empty());
    let first = words.next()?;
    if !first.starts_with(|c: char| c.is_alphabetic()) {
        return None;
    }
    if !words.all(|word| word.starts_with(|c: char| c.is_alphabetic() || c == '"')) {
        return None;
    }
    Some(Binding::step(name, ""))
}

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
    /// then what it acts on and its arguments as notes.
    ///
    /// A step that names one resource reads as the phrase with that resource
    /// said, so the phrase is one of the row's readable forms — any way it
    /// writes the resource — up to the `on ` the resource follows.
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
            .find(|row| names_an_action(row, phrase, &*shared.app_label))
        else {
            return Ok(None);
        };
        let number = self.rows.get(index).map_or(1, |row| row.number);
        invocation(shared, row, phrase, body, targets, number).map(Some)
    }
}

/// Whether `phrase` is a line of `row`'s action: the words the catalogue gives
/// it, and then what it acts on — `on any <kind>` for a kind, `on <resource>`
/// for one resource the program names.
fn names_an_action(row: &CatalogueRow, phrase: &str, app_label: &dyn Fn(&str) -> String) -> bool {
    let Some(any) = reads_any(row, app_label) else {
        return false;
    };
    // The phrase reads `<action> on any <kind> in <app>`. A line that names
    // one resource, or takes whatever item of one, says the same words up to
    // `on any` and then its own resource, so the action's own words are what
    // decide whether the row is this line's action.
    let Some(head) = any.split_once(" on any ").map(|(head, _)| head) else {
        return false;
    };
    // The row's own words; just its action, for a step whose resource follows
    // as a note; or the action and the app, for the step that acts on what a
    // binding yields. A phrase that runs past `on` would take the resource the
    // reader has to read for itself.
    let phrase = phrase.trim_end();
    let app = app_label(&row.app_id);
    phrase == any || phrase == head || phrase == format!("{head} in {app}")
}
