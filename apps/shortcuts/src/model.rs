//! Shortcuts being written: the app's own resource.
//!
//! A draft holds one shortcut program while the person shapes it. Every edit
//! is an action, so the palette, an agent or a script builds a draft exactly
//! as the editor does. A draft becomes a saved shortcut only through the
//! door's `shortcuts#create` / `shortcuts#update`, which the save effect
//! calls after the person has reviewed what the shortcut may do.

use serde::{Deserialize, Serialize};
use tilo_automation::{Binding, Shortcut, Step, StepKind, TriggerSpec, UnattendedConsent};
use ui::prelude::*;
use ui::tilo_actions::{ActionParam, ParamType};

/// The program a draft holds: everything of a shortcut but its identity,
/// name and grant, which the draft and the save step supply.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Program {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ActionParam>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Binding>,
    /// Carried unchanged from the saved shortcut, so editing its steps
    /// keeps when it runs on its own and what it may do then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<TriggerSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consents: Vec<UnattendedConsent>,
}

/// One shortcut being written.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Draft {
    pub name: String,
    /// The id the shortcut is saved under.
    pub shortcut_id: String,
    pub program: Json<Program>,
    /// Whether saving replaces a saved shortcut rather than creating one.
    pub replaces_saved: bool,
}

impl Draft {
    /// The shortcut this draft saves as, with `grant`.
    #[must_use]
    pub fn shortcut(&self, grant: Vec<collab_types::PermissionProfileRule>) -> Shortcut {
        let program = &self.program.0;
        Shortcut {
            id: self.shortcut_id.clone(),
            name: self.name.trim().to_owned(),
            inputs: program.inputs.clone(),
            steps: program.steps.clone(),
            output: program.output.clone(),
            grant,
            triggers: program.triggers.clone(),
            consents: program.consents.clone(),
            saved_by: None,
        }
    }
}

/// Every draft on this device's workbench.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Workbench {
    pub drafts: Collection<Draft>,
}

fn draft_mut<'a>(workbench: &'a mut Workbench, draft: &Id) -> Result<&'a mut Draft, Refusal> {
    workbench
        .drafts
        .get_mut(draft)
        .ok_or_else(|| Refusal::from("there is no such draft"))
}

/// Replace the program of `draft` with `edit` applied to a copy of it.
fn edit_program(
    workbench: &mut Workbench,
    draft: &Id,
    edit: impl FnOnce(&mut Program) -> Result<(), String>,
) -> Result<(), Refusal> {
    let draft = draft_mut(workbench, draft)?;
    let mut program = draft.program.0.clone();
    edit(&mut program).map_err(Refusal::from)?;
    draft.program = Json(program);
    Ok(())
}

#[resource(kind = "shortcuts.workbench", label = "Shortcut drafts")]
impl Workbench {
    /// Start a new shortcut.
    #[action]
    pub fn new_draft(&mut self, cx: &mut ActionCx, #[param("Name of the shortcut")] name: String) {
        let id = cx.id();
        let shortcut_id = format!("s{}", id.as_str().get(..16).unwrap_or(id.as_str()));
        self.drafts.push(
            id,
            Draft {
                name,
                shortcut_id,
                program: Json(Program::default()),
                replaces_saved: false,
            },
        );
    }

    /// Open a saved shortcut as a draft to change it.
    #[action]
    pub fn open_shortcut(
        &mut self,
        cx: &mut ActionCx,
        #[param("The saved shortcut, as JSON")] shortcut: serde_json::Value,
    ) -> Result<(), Refusal> {
        let shortcut: Shortcut = serde_json::from_value(shortcut)
            .map_err(|error| Refusal::new(format!("not a shortcut: {error}")))?;
        self.drafts.push(
            cx.id(),
            Draft {
                name: shortcut.name,
                shortcut_id: shortcut.id,
                program: Json(Program {
                    inputs: shortcut.inputs,
                    steps: shortcut.steps,
                    output: shortcut.output,
                    triggers: shortcut.triggers,
                    consents: shortcut.consents,
                }),
                replaces_saved: true,
            },
        );
        Ok(())
    }

    /// Rename a draft.
    #[action]
    pub fn rename(&mut self, draft: Id, #[param("New name")] name: String) -> Result<(), Refusal> {
        draft_mut(self, &draft)?.name = name;
        Ok(())
    }

    /// Discard a draft. A saved shortcut it was editing stays as it was.
    #[action]
    pub fn discard(&mut self, draft: Id) -> Result<(), Refusal> {
        self.drafts
            .remove(&draft)
            .map(drop)
            .ok_or_else(|| Refusal::from("there is no such draft"))
    }

    /// Add a step at the end of a block: `""` for the top level, or the
    /// branch of a step such as `2.then`, `2.else` or `3.each`.
    #[action]
    pub fn add_step(
        &mut self,
        draft: Id,
        #[param("Block to add to: empty for the top level, or like 2.then")] block: String,
        #[param("The step, as JSON with a `step` kind")] step: serde_json::Value,
    ) -> Result<(), Refusal> {
        let kind: StepKind = serde_json::from_value(step)
            .map_err(|error| Refusal::new(format!("not a step: {error}")))?;
        edit_program(self, &draft, |program| {
            let id = fresh_step_id(&program.steps, &kind);
            block_mut(&mut program.steps, &block)
                .ok_or_else(|| format!("there is no block `{block}`"))?
                .push(Step::new(id, kind));
            Ok(())
        })
    }

    /// Replace what a step does, keeping its id.
    #[action]
    pub fn set_step(
        &mut self,
        draft: Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("What the step does, as JSON with a `step` kind")] step: serde_json::Value,
    ) -> Result<(), Refusal> {
        let kind: StepKind = serde_json::from_value(step)
            .map_err(|error| Refusal::new(format!("not a step: {error}")))?;
        edit_program(self, &draft, |program| {
            step_mut(&mut program.steps, &path)
                .ok_or_else(|| format!("there is no step `{path}`"))?
                .kind = kind;
            Ok(())
        })
    }

    /// Bind one argument of an action step, or clear it.
    #[action]
    pub fn set_argument(
        &mut self,
        draft: Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("The parameter to bind")] param: String,
        #[param("Where the value comes from, as binding JSON; empty clears it")] binding: String,
    ) -> Result<(), Refusal> {
        let binding = parse_binding(&binding)?;
        edit_program(self, &draft, |program| {
            let step = step_mut(&mut program.steps, &path)
                .ok_or_else(|| format!("there is no step `{path}`"))?;
            let StepKind::Invoke { args, .. } = &mut step.kind else {
                return Err(format!("step `{path}` is not an action"));
            };
            match binding {
                Some(binding) => {
                    args.insert(param, binding);
                }
                None => {
                    args.remove(&param);
                }
            }
            Ok(())
        })
    }

    /// Remove a step, and everything inside it.
    #[action]
    pub fn remove_step(
        &mut self,
        draft: Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
    ) -> Result<(), Refusal> {
        edit_program(self, &draft, |program| {
            let (block, index) = split_path(&path).ok_or_else(|| format!("no step `{path}`"))?;
            let steps = block_mut(&mut program.steps, block)
                .ok_or_else(|| format!("there is no step `{path}`"))?;
            if index >= steps.len() {
                return Err(format!("there is no step `{path}`"));
            }
            steps.remove(index);
            Ok(())
        })
    }

    /// Move a step up or down within its block.
    #[action]
    pub fn move_step(
        &mut self,
        draft: Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("True to move it earlier, false to move it later")] earlier: bool,
    ) -> Result<(), Refusal> {
        edit_program(self, &draft, |program| {
            let (block, index) = split_path(&path).ok_or_else(|| format!("no step `{path}`"))?;
            let steps = block_mut(&mut program.steps, block)
                .ok_or_else(|| format!("there is no step `{path}`"))?;
            let other = if earlier {
                index.checked_sub(1)
            } else {
                index.checked_add(1).filter(|next| *next < steps.len())
            };
            match other {
                Some(other) if index < steps.len() => {
                    steps.swap(index, other);
                    Ok(())
                }
                _ => Err(format!("step `{path}` cannot move that way")),
            }
        })
    }

    /// Declare a value the shortcut is started with.
    #[action]
    pub fn add_input(
        &mut self,
        draft: Id,
        #[param("Name of the input: letters, digits, - or _")] name: String,
        #[param("text, number, integer, boolean, date-time or resource")] kind: String,
    ) -> Result<(), Refusal> {
        let ty = param_type(&kind).ok_or_else(|| Refusal::new(format!("unknown type `{kind}`")))?;
        edit_program(self, &draft, |program| {
            if program
                .inputs
                .iter()
                .any(|input| input.name == name.as_str())
            {
                return Err(format!("there is already an input `{name}`"));
            }
            program
                .inputs
                .push(ActionParam::required_owned(name.clone(), ty, name.clone()));
            Ok(())
        })
    }

    /// Remove an input.
    #[action]
    pub fn remove_input(&mut self, draft: Id, name: String) -> Result<(), Refusal> {
        edit_program(self, &draft, |program| {
            let before = program.inputs.len();
            program.inputs.retain(|input| input.name != name.as_str());
            if program.inputs.len() == before {
                return Err(format!("there is no input `{name}`"));
            }
            Ok(())
        })
    }

    /// Choose what a finished run reports back, or nothing.
    #[action]
    pub fn set_output(
        &mut self,
        draft: Id,
        #[param("Where the result comes from, as binding JSON; empty for none")] binding: String,
    ) -> Result<(), Refusal> {
        let binding = parse_binding(&binding)?;
        edit_program(self, &draft, |program| {
            program.output = binding;
            Ok(())
        })
    }
}

/// A binding given as JSON text; empty for none.
fn parse_binding(text: &str) -> Result<Option<Binding>, Refusal> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(text)
        .map(Some)
        .map_err(|error| Refusal::new(format!("not a binding: {error}")))
}

/// The input types the editor offers, by name.
pub const INPUT_TYPES: [&str; 6] = [
    "text",
    "number",
    "integer",
    "boolean",
    "date-time",
    "resource",
];

fn param_type(kind: &str) -> Option<ParamType> {
    Some(match kind {
        "text" => ParamType::Text,
        "number" => ParamType::Number,
        "integer" => ParamType::Integer,
        "boolean" => ParamType::Boolean,
        "date-time" => ParamType::DateTime,
        "resource" => ParamType::Resource,
        _ => return None,
    })
}

/// A readable id no other step in `steps` (at any depth) has.
fn fresh_step_id(steps: &[Step], kind: &StepKind) -> String {
    let base: String = match kind {
        StepKind::Invoke { action, .. } => action
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect(),
        StepKind::If { .. } => "if".into(),
        StepKind::ForEach { .. } => "each".into(),
        StepKind::SetVar { .. } => "set".into(),
        StepKind::Ask { .. } => "ask".into(),
        StepKind::Ai { .. } => "ai".into(),
    };
    let base = if base.is_empty() { "step".into() } else { base };
    let mut taken = Vec::new();
    collect_ids(steps, &mut taken);
    if !taken.contains(&base) {
        return base;
    }
    (2_u32..=u32::MAX)
        .map(|n| format!("{base}{n}"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(base)
}

fn collect_ids(steps: &[Step], out: &mut Vec<String>) {
    for step in steps {
        out.push(step.id.clone());
        match &step.kind {
            StepKind::If {
                then, otherwise, ..
            } => {
                collect_ids(then, out);
                collect_ids(otherwise, out);
            }
            StepKind::ForEach { body, .. } => collect_ids(body, out),
            StepKind::Invoke { .. }
            | StepKind::SetVar { .. }
            | StepKind::Ask { .. }
            | StepKind::Ai { .. } => {}
        }
    }
}

/// The block and index of a step path: `2.then.1` is step 1 of block
/// `2.then`; `0` is step 0 of the top level.
#[must_use]
pub fn split_path(path: &str) -> Option<(&str, usize)> {
    let (block, index) = match path.rsplit_once('.') {
        Some((block, index)) => (block, index),
        None => ("", path),
    };
    Some((block, index.parse().ok()?))
}

/// The block a block address names: `""`, `2.then`, `2.else`, `3.each`.
pub fn block_mut<'a>(steps: &'a mut Vec<Step>, block: &str) -> Option<&'a mut Vec<Step>> {
    if block.is_empty() {
        return Some(steps);
    }
    let (owner, branch) = block.rsplit_once('.')?;
    let step = step_mut(steps, owner)?;
    match (&mut step.kind, branch) {
        (StepKind::If { then, .. }, "then") => Some(then),
        (StepKind::If { otherwise, .. }, "else") => Some(otherwise),
        (StepKind::ForEach { body, .. }, "each") => Some(body),
        _ => None,
    }
}

/// The step at `path`.
pub fn step_mut<'a>(steps: &'a mut Vec<Step>, path: &str) -> Option<&'a mut Step> {
    let (block, index) = split_path(path)?;
    block_mut(steps, block)?.get_mut(index)
}

/// The step at `path`, read-only.
#[must_use]
pub fn step_at<'a>(steps: &'a [Step], path: &str) -> Option<&'a Step> {
    let (block, index) = split_path(path)?;
    block_at(steps, block)?.get(index)
}

/// The block a block address names, read-only.
#[must_use]
pub fn block_at<'a>(steps: &'a [Step], block: &str) -> Option<&'a [Step]> {
    if block.is_empty() {
        return Some(steps);
    }
    let (owner, branch) = block.rsplit_once('.')?;
    let step = step_at(steps, owner)?;
    match (&step.kind, branch) {
        (StepKind::If { then, .. }, "then") => Some(then),
        (StepKind::If { otherwise, .. }, "else") => Some(otherwise),
        (StepKind::ForEach { body, .. }, "each") => Some(body),
        _ => None,
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
