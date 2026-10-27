//! Shortcuts being written: the app's own resource.
//!
//! A draft holds one shortcut program while the person shapes it. Every edit
//! is an action, so the palette, an agent or a script builds a draft exactly
//! as the editor does. A draft becomes a saved shortcut only through the
//! door's `shortcuts#create` / `shortcuts#update`, which the save effect
//! calls after the person has reviewed what the shortcut may do.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_automation::{
    AiTransform, Binding, Condition, Selector, Shortcut, Step, StepKind, TriggerSpec,
};
use ui::prelude::*;
use ui::tilo_actions::{ActionKey, ActionParam, ParamType};

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
    /// How long each step of a run may take before it is given up on
    /// (`Shortcut::step_timeout_secs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timeout_secs: Option<u64>,
}

/// One shortcut being written.
///
/// The program is stored piece by piece rather than as one value, so two
/// people editing different steps, arguments or inputs of the same draft
/// both keep their edit: every step is an item of [`Self::steps`] with its
/// arguments under their own keys, every input an item of [`Self::inputs`].
/// [`Self::program`] reads it back as one [`Program`].
///
/// Each unit (an input's declaration, an argument's binding, a step's head,
/// the output, the triggers) is written whole and kept as the automation
/// value the run engine reads; the record stores each as its maps, lists and
/// scalars.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Draft {
    /// Name
    #[model(label)]
    pub name: String,
    /// The id the shortcut is saved under.
    pub shortcut_id: String,
    /// The shortcut's inputs, keyed by name.
    pub inputs: Collection<ActionParam>,
    /// Every step at every depth, in program order; a step names the block
    /// it is in ([`StepNode::parent`], [`StepNode::branch`]).
    pub steps: Collection<StepNode>,
    /// What a finished run reports back.
    pub output: Option<Binding>,
    /// Carried unchanged from the saved shortcut, so editing its steps
    /// keeps when it runs on its own and what it may do then.
    pub triggers: Vec<TriggerSpec>,
    /// Carried unchanged like the triggers: how long each step of a run may
    /// take, or `None` for the engine's own patience.
    pub step_timeout_secs: Option<u64>,
    /// Whether saving replaces a saved shortcut rather than creating one.
    pub replaces_saved: bool,
}

/// One step of a draft, wherever it sits in the program.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct StepNode {
    /// The item id of the step whose block holds this one; empty at the top
    /// level.
    pub parent: Atom,
    /// Which block of the parent: `then`, `else` or `each`; empty at the top
    /// level.
    pub branch: Atom,
    /// The step's own id, which bindings to its output name.
    pub id: Atom,
    /// What the step does, apart from its arguments and blocks.
    pub head: Option<Head>,
    /// An action step's arguments, each bound on its own.
    pub args: BTreeMap<String, Binding>,
}

/// A [`StepKind`] without the parts a [`StepNode`] stores apart: an action's
/// arguments and the steps inside a block.
#[derive(Model, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Head {
    Invoke {
        action: ActionKey,
        resource: Selector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        appearance: Option<Binding>,
    },
    If {
        cond: Condition,
    },
    ForEach {
        items: Binding,
        item: String,
    },
    SetVar {
        name: String,
        value: Binding,
    },
    Ask {
        param: ActionParam,
    },
    Ai {
        transform: AiTransform,
        input: Binding,
    },
}

const THEN: &str = "then";
const ELSE: &str = "else";
const EACH: &str = "each";

/// The blocks of a step, by branch name.
type Blocks<'a> = Vec<(&'static str, &'a [Step])>;

impl Head {
    /// The head of `kind`, its arguments, and its blocks by branch name.
    fn split(kind: &StepKind) -> (Self, BTreeMap<String, Binding>, Blocks<'_>) {
        match kind {
            StepKind::Invoke {
                action,
                resource,
                args,
                appearance,
            } => (
                Self::Invoke {
                    action: action.clone(),
                    resource: resource.clone(),
                    appearance: appearance.clone(),
                },
                args.clone(),
                Vec::new(),
            ),
            StepKind::If {
                cond,
                then,
                otherwise,
            } => (
                Self::If { cond: cond.clone() },
                BTreeMap::new(),
                vec![(THEN, then.as_slice()), (ELSE, otherwise.as_slice())],
            ),
            StepKind::ForEach { items, item, body } => (
                Self::ForEach {
                    items: items.clone(),
                    item: item.clone(),
                },
                BTreeMap::new(),
                vec![(EACH, body.as_slice())],
            ),
            StepKind::SetVar { name, value } => (
                Self::SetVar {
                    name: name.clone(),
                    value: value.clone(),
                },
                BTreeMap::new(),
                Vec::new(),
            ),
            StepKind::Ask { param } => (
                Self::Ask {
                    param: param.clone(),
                },
                BTreeMap::new(),
                Vec::new(),
            ),
            StepKind::Ai { transform, input } => (
                Self::Ai {
                    transform: transform.clone(),
                    input: input.clone(),
                },
                BTreeMap::new(),
                Vec::new(),
            ),
        }
    }

    /// The step kind this head makes with `args` and the blocks `block`
    /// reads by branch name.
    fn join(
        self,
        args: BTreeMap<String, Binding>,
        mut block: impl FnMut(&str) -> Vec<Step>,
    ) -> StepKind {
        match self {
            Self::Invoke {
                action,
                resource,
                appearance,
            } => StepKind::Invoke {
                action,
                resource,
                args,
                appearance,
            },
            Self::If { cond } => StepKind::If {
                cond,
                then: block(THEN),
                otherwise: block(ELSE),
            },
            Self::ForEach { items, item } => StepKind::ForEach {
                items,
                item,
                body: block(EACH),
            },
            Self::SetVar { name, value } => StepKind::SetVar { name, value },
            Self::Ask { param } => StepKind::Ask { param },
            Self::Ai { transform, input } => StepKind::Ai { transform, input },
        }
    }
}

impl Draft {
    /// A draft holding `program`, its steps' item ids minted by `mint`.
    pub fn new(
        name: String,
        shortcut_id: String,
        program: &Program,
        replaces_saved: bool,
        mint: &mut dyn FnMut() -> Id,
    ) -> Self {
        let mut draft = Self {
            name,
            shortcut_id,
            replaces_saved,
            ..Self::default()
        };
        draft.set_program(program, mint);
        draft
    }

    /// The program the draft holds.
    #[must_use]
    pub fn program(&self) -> Program {
        Program {
            inputs: self.inputs.values().cloned().collect(),
            steps: self.block("", ""),
            output: self.output.clone(),
            triggers: self.triggers.clone(),
            step_timeout_secs: self.step_timeout_secs,
        }
    }

    /// The steps of block `branch` of the step with item id `parent`.
    fn block(&self, parent: &str, branch: &str) -> Vec<Step> {
        self.steps
            .iter()
            .filter(|(_, node)| node.parent.0 == parent && node.branch.0 == branch)
            .filter_map(|(key, node)| {
                let head = node.head.clone()?;
                let args = node.args.clone();
                let kind = head.join(args, |branch| self.block(key.as_str(), branch));
                Some(Step::new(node.id.0.clone(), kind))
            })
            .collect()
    }

    /// Store `program`, changing only what differs from what the draft
    /// holds: a step keeps its item while it stays in the same block under
    /// the same id, so an edit to it merges with edits to every other step.
    pub fn set_program(&mut self, program: &Program, mint: &mut dyn FnMut() -> Id) {
        let names: Vec<Id> = program
            .inputs
            .iter()
            .map(|input| Id::from_raw(input.name.as_ref()))
            .collect();
        self.inputs.retain(|name, _| names.contains(name));
        for (index, (name, input)) in names.iter().zip(&program.inputs).enumerate() {
            match self.inputs.get_mut(name) {
                Some(stored) => {
                    if stored != input {
                        *stored = input.clone();
                    }
                }
                None => {
                    self.inputs.insert(index, name.clone(), input.clone());
                }
            }
        }
        place(&mut self.inputs, &names);

        let mut order = Vec::new();
        self.store_block(&program.steps, "", "", &mut order, mint);
        self.steps.retain(|key, _| order.contains(key));
        place(&mut self.steps, &order);

        if self.output != program.output {
            self.output.clone_from(&program.output);
        }
        if self.triggers != program.triggers {
            self.triggers.clone_from(&program.triggers);
        }
    }

    /// Store the steps of block `branch` of the step with item id `parent`,
    /// and the blocks inside them, appending their item ids to `order` in
    /// program order.
    fn store_block(
        &mut self,
        steps: &[Step],
        parent: &str,
        branch: &str,
        order: &mut Vec<Id>,
        mint: &mut dyn FnMut() -> Id,
    ) {
        for step in steps {
            let (head, args, blocks) = Head::split(&step.kind);
            let existing = self
                .steps
                .iter()
                .find(|(key, node)| {
                    node.parent.0 == parent
                        && node.branch.0 == branch
                        && node.id.0 == step.id
                        && !order.contains(key)
                })
                .map(|(key, _)| key.clone());
            let key = existing.unwrap_or_else(&mut *mint);
            let node = StepNode {
                parent: Atom(parent.to_owned()),
                branch: Atom(branch.to_owned()),
                id: Atom(step.id.clone()),
                head: Some(head),
                args,
            };
            match self.steps.get_mut(&key) {
                Some(stored) => {
                    if *stored != node {
                        *stored = node;
                    }
                }
                None => {
                    self.steps.push(key.clone(), node);
                }
            }
            order.push(key.clone());
            for (branch, block) in blocks {
                self.store_block(block, key.as_str(), branch, order, mint);
            }
        }
    }

    /// The shortcut this draft saves as, with `grant`.
    #[must_use]
    pub fn shortcut(&self, grant: Vec<tilo_action_policy::Rule>) -> Shortcut {
        let program = self.program();
        Shortcut {
            id: self.shortcut_id.clone(),
            name: self.name.trim().to_owned(),
            inputs: program.inputs,
            steps: program.steps,
            output: program.output,
            grant,
            triggers: program.triggers,
            step_timeout_secs: program.step_timeout_secs,
            saved_by: None,
        }
    }
}

/// Bring `collection` into the order `order` names, moving only the items
/// out of place.
fn place<T>(collection: &mut Collection<T>, order: &[Id]) {
    for (index, key) in order.iter().enumerate() {
        if collection.position(key) != Some(index) {
            collection.move_to(key, index);
        }
    }
}

/// Every draft on this device's workbench.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Workbench {
    pub drafts: Collection<Draft>,
}

/// `name`, numbered past the drafts that already go by it: "New shortcut",
/// then "New shortcut 2", "New shortcut 3".
fn unused_name(workbench: &Workbench, name: &str) -> String {
    let base = name.trim();
    let number = |taken: &str| -> Option<u32> {
        let rest = taken.trim().strip_prefix(base)?;
        if rest.is_empty() {
            return Some(1);
        }
        rest.strip_prefix(' ')?.parse().ok()
    };
    let highest = workbench
        .drafts
        .iter()
        .filter_map(|(_, draft)| number(&draft.name))
        .max();
    match highest {
        Some(highest) if !base.is_empty() => format!("{base} {}", highest.saturating_add(1)),
        _ => name.to_owned(),
    }
}

fn draft_mut<'a>(workbench: &'a mut Workbench, draft: &Id) -> Result<&'a mut Draft, Refusal> {
    workbench
        .drafts
        .get_mut(draft)
        .ok_or_else(|| Refusal::from("there is no such draft"))
}

/// Apply `edit` to the program of `draft` and store what changed.
fn edit_program(
    workbench: &mut Workbench,
    cx: &mut ActionCx,
    draft: &Id,
    edit: impl FnOnce(&mut Program) -> Result<(), String>,
) -> Result<(), Refusal> {
    let draft = draft_mut(workbench, draft)?;
    let mut program = draft.program();
    edit(&mut program).map_err(Refusal::from)?;
    draft.set_program(&program, &mut || cx.id());
    Ok(())
}

/// Bring a workbench of an older version up to this one. Version 1 stored a
/// draft's inputs, output and triggers and each step's head and arguments as
/// one JSON string each; they are stored as their values now. Version 2
/// stored a step's head with its fields flat beside its variant's name; a
/// head's fields are stored by their variant now, so that a field one variant
/// names is never read where another wrote it. Every draft, step and binding
/// is kept; a value that was JSON `null` (no input, no head, no binding) is
/// left absent, as it read before.
///
/// # Errors
/// Returns why a stored value does not read as its type.
pub fn upgrade(
    doc: &mut ui::tilo_resource::Doc,
    from: u32,
) -> Result<(), ui::tilo_resource::ModelError> {
    use ui::automerge::{ObjType, ReadDoc as _, Value};
    use ui::tilo_resource::evolution::{
        collection_items, retype_json, retype_json_items, retype_variant_fields,
    };
    if !(1..=2).contains(&from) {
        return Err(ui::tilo_resource::ModelError::invalid(format!(
            "no Shortcuts workbench migration from {from}"
        )));
    }
    for draft in collection_items(doc, &ui::automerge::ROOT, "drafts")? {
        if from == 1 {
            // Version 1 stored a step's head as one JSON string.
            retype_json::<Option<Binding>>(doc, &draft, "output")?;
            retype_json::<Vec<TriggerSpec>>(doc, &draft, "triggers")?;
            retype_json_items::<Option<ActionParam>>(doc, &draft, "inputs")?;
        }
        for step in collection_items(doc, &draft, "steps")? {
            if from == 1 {
                retype_json::<Option<Head>>(doc, &step, "head")?;
            }
            // Version 2 stored a head's fields flat, beside its variant's
            // name; whatever version stored it, a head's fields are stored by
            // its variant now, so a field one variant names is never read as
            // another's.
            retype_variant_fields::<Option<Head>>(doc, &step, "head")?;
            let Some((Value::Object(ObjType::Map), args)) = doc.get(&step, "args")? else {
                continue;
            };
            if from == 1 {
                for name in doc.keys(&args).collect::<Vec<_>>() {
                    retype_json::<Option<Binding>>(doc, &args, &name)?;
                }
            }
        }
    }
    Ok(())
}

#[resource(
    kind = "shortcuts.workbench",
    label = "Shortcuts",
    version = 3,
    migrate = upgrade
)]
impl Workbench {
    /// Start a new shortcut.
    ///
    /// A name another draft already has is numbered ("New shortcut 2"), so
    /// the drafts can be told apart in the list.
    #[action]
    pub fn new_draft(&mut self, cx: &mut ActionCx, #[param("Name of the shortcut")] name: String) {
        let name = unused_name(self, &name);
        let id = cx.id();
        let shortcut_id = format!("s{}", id.as_str().get(..16).unwrap_or(id.as_str()));
        let draft = Draft::new(name, shortcut_id, &Program::default(), false, &mut || {
            cx.id()
        });
        self.drafts.push(id, draft);
    }

    /// Import a shortcut as an unsaved draft for review. Never arms a trigger.
    #[action]
    pub fn draft_shortcut(
        &mut self,
        cx: &mut ActionCx,
        #[param("The shortcut program, as JSON")] shortcut: serde_json::Value,
    ) -> Result<(), Refusal> {
        if shortcut.to_string().len() > 16 * 1024 {
            return Err(Refusal::new("Shortcut draft is too large"));
        }
        let mut shortcut: Shortcut = serde_json::from_value(shortcut)
            .map_err(|error| Refusal::new(format!("not a shortcut: {error}")))?;
        let id = cx.id();
        shortcut.id = format!("s{}", id.as_str().get(..16).unwrap_or(id.as_str()));
        let program = Program {
            inputs: shortcut.inputs,
            steps: shortcut.steps,
            output: shortcut.output,
            triggers: Vec::new(),
            step_timeout_secs: shortcut.step_timeout_secs,
        };
        let draft = Draft::new(shortcut.name, shortcut.id, &program, false, &mut || cx.id());
        self.drafts.push(id, draft);
        Ok(())
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
        let id = cx.id();
        let program = Program {
            inputs: shortcut.inputs,
            steps: shortcut.steps,
            output: shortcut.output,
            triggers: shortcut.triggers,
            step_timeout_secs: shortcut.step_timeout_secs,
        };
        let draft = Draft::new(shortcut.name, shortcut.id, &program, true, &mut || cx.id());
        self.drafts.push(id, draft);
        Ok(())
    }

    /// Choose or disable a local morning refresh schedule in this unsaved draft.
    #[action]
    pub fn set_schedule(
        &mut self,
        cx: &mut ActionCx,
        draft: Id,
        #[param("Enable the daily local-time schedule")] enabled: bool,
        #[param("Local hour, 0 to 23")] hour: i64,
        #[param("Local minute, 0 to 59")] minute: i64,
    ) -> Result<(), Refusal> {
        let hour = u8::try_from(hour)
            .ok()
            .filter(|hour| *hour < 24)
            .ok_or_else(|| Refusal::new("Hour must be 0 to 23"))?;
        let minute = u8::try_from(minute)
            .ok()
            .filter(|minute| *minute < 60)
            .ok_or_else(|| Refusal::new("Minute must be 0 to 59"))?;
        edit_program(self, cx, &draft, |program| {
            program.triggers.retain(|trigger| !matches!(trigger,
                TriggerSpec::Schedule { schedule: tilo_automation::Schedule::Calendar { weekdays, .. }, .. } if weekdays.is_empty()
            ));
            if enabled {
                program.triggers.push(TriggerSpec::Schedule {
                    schedule: tilo_automation::Schedule::Calendar {
                        at: tilo_automation::TimeOfDay { hour, minute },
                        weekdays: Vec::new(),
                    },
                    catch_up: tilo_automation::CatchUp::RunOnce,
                });
            }
            Ok(())
        })
    }

    /// Discard a draft. A saved shortcut it was editing stays as it was.
    #[action]
    pub fn discard(&mut self, draft: &Id) -> Result<(), Refusal> {
        self.drafts
            .remove(draft)
            .map(drop)
            .ok_or_else(|| Refusal::from("there is no such draft"))
    }

    /// Add a step at the end of a block: `""` for the top level, or the
    /// branch of a step such as `2.then`, `2.else` or `3.each`.
    #[action]
    pub fn add_step(
        &mut self,
        cx: &mut ActionCx,
        draft: &Id,
        #[param("Block to add to: empty for the top level, or like 2.then")] block: String,
        #[param("The step, as JSON with a `step` kind")] step: serde_json::Value,
    ) -> Result<(), Refusal> {
        let kind: StepKind = serde_json::from_value(step)
            .map_err(|error| Refusal::new(format!("not a step: {error}")))?;
        edit_program(self, cx, draft, |program| {
            let id = fresh_step_id(&program.steps, &kind);
            block_mut(&mut program.steps, &block)
                .ok_or_else(|| format!("there is no block `{block}`"))?
                .push(Step::new(id, kind));
            Ok(())
        })
    }

    /// Replace what a step does, keeping its id.
    #[action]
    pub fn replace_step(
        &mut self,
        cx: &mut ActionCx,
        draft: &Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("What the step does, as JSON with a `step` kind")] step: serde_json::Value,
    ) -> Result<(), Refusal> {
        let kind: StepKind = serde_json::from_value(step)
            .map_err(|error| Refusal::new(format!("not a step: {error}")))?;
        edit_program(self, cx, draft, |program| {
            step_mut(&mut program.steps, &path)
                .ok_or_else(|| format!("there is no step `{path}`"))?
                .kind = kind;
            Ok(())
        })
    }

    /// Bind one argument of an action step, or clear it.
    #[action]
    pub fn bind_argument(
        &mut self,
        cx: &mut ActionCx,
        draft: &Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("The parameter to bind")] param: String,
        #[param("Where the value comes from, as binding JSON; empty clears it")] binding: String,
    ) -> Result<(), Refusal> {
        let binding = parse_binding(&binding)?;
        edit_program(self, cx, draft, |program| {
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
        cx: &mut ActionCx,
        draft: &Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
    ) -> Result<(), Refusal> {
        edit_program(self, cx, draft, |program| {
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
        cx: &mut ActionCx,
        draft: &Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("True to move it earlier, false to move it later")] earlier: bool,
    ) -> Result<(), Refusal> {
        edit_program(self, cx, draft, |program| {
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
        cx: &mut ActionCx,
        draft: &Id,
        #[param("Name of the input: letters, digits, - or _")] name: String,
        #[param("text, number, integer, boolean, date-time or resource")] kind: String,
    ) -> Result<(), Refusal> {
        let ty = param_type(&kind).ok_or_else(|| Refusal::new(format!("unknown type `{kind}`")))?;
        edit_program(self, cx, draft, |program| {
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
    pub fn remove_input(
        &mut self,
        cx: &mut ActionCx,
        draft: &Id,
        name: String,
    ) -> Result<(), Refusal> {
        edit_program(self, cx, draft, |program| {
            let before = program.inputs.len();
            program.inputs.retain(|input| input.name != name.as_str());
            if program.inputs.len() == before {
                return Err(format!("there is no input `{name}`"));
            }
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

pub(crate) fn param_type(kind: &str) -> Option<ParamType> {
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
            .action
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
