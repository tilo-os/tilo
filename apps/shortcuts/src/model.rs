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
    Binding, Condition, Selector, Shortcut, Step, StepKind, StepPolicy, TriggerSpec,
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
    /// What the run does when the step fails; [`StepPolicy::Fail`], the
    /// default, stops it.
    pub on_error: StepPolicy,
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
        /// What to do, in the person's words.
        prompt: String,
        /// The step's own input; the step before it when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Binding>,
        /// The shape the answer must have, as a JSON Schema; text when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
    },
    Task {
        /// What the task is to do, in the person's words.
        instruction: String,
        /// What it may use, as the grant node of each action. It narrows the
        /// shortcut's own grant and never widens it. Held as the values the
        /// policy layer reads, the shape the agent's task action takes them in.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allow: Vec<serde_json::Value>,
        /// The shape the summary must have, as a JSON Schema; text when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
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
            StepKind::Ai {
                prompt,
                output,
                input,
            } => (
                Self::Ai {
                    prompt: prompt.clone(),
                    output: output.clone(),
                    input: input.clone(),
                },
                BTreeMap::new(),
                Vec::new(),
            ),
            StepKind::Task {
                instruction,
                allow,
                output,
            } => (
                Self::Task {
                    instruction: instruction.clone(),
                    allow: allow
                        .iter()
                        .filter_map(|rule| serde_json::to_value(rule).ok())
                        .collect(),
                    output: output.clone(),
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
            Self::Ai {
                prompt,
                output,
                input,
            } => StepKind::Ai {
                prompt,
                output,
                input,
            },
            Self::Task {
                instruction,
                allow,
                output,
            } => StepKind::Task {
                instruction,
                allow: allow
                    .iter()
                    .filter_map(|rule| serde_json::from_value(rule.clone()).ok())
                    .collect(),
                output,
            },
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
                Some(Step::new(node.id.0.clone(), kind).with_policy(node.on_error.clone()))
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
                on_error: step.on_error.clone(),
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
            saved_proof: None,
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
    /// The repeated work the person said no to: the sequence of actions
    /// ([`crate::proposal::Proposal::key`]), so it is not offered again. It
    /// says nothing about what the person did, only which proposals were
    /// turned down.
    pub ignored: Vec<String>,
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
/// one JSON string each; they are stored as their values now. Every draft,
/// step and binding is kept; a value that was JSON `null` (no input, no head,
/// no binding) is left absent, as it read before. Versions 1 to 3 stored a
/// step's head with its fields flat, beside its variant's name; they are
/// stored by their variant now.
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
    // Version 3 added the proposals the person turned down; a workbench that
    // predates it has turned none down.
    if !(1..=3).contains(&from) {
        return Err(ui::tilo_resource::ModelError::invalid(format!(
            "no Shortcuts workbench migration from {from}"
        )));
    }
    for draft in collection_items(doc, &ui::automerge::ROOT, "drafts")? {
        if from == 1 {
            retype_json::<Option<Binding>>(doc, &draft, "output")?;
            retype_json::<Vec<TriggerSpec>>(doc, &draft, "triggers")?;
            retype_json_items::<Option<ActionParam>>(doc, &draft, "inputs")?;
        }
        for step in collection_items(doc, &draft, "steps")? {
            retype_json::<Option<Head>>(doc, &step, "head")?;
            retype_variant_fields::<Option<Head>>(doc, &step, "head")?;
            if from != 1 {
                continue;
            }
            let Some((Value::Object(ObjType::Map), args)) = doc.get(&step, "args")? else {
                continue;
            };
            for name in doc.keys(&args).collect::<Vec<_>>() {
                retype_json::<Option<Binding>>(doc, &args, &name)?;
            }
        }
    }
    Ok(())
}

#[resource(
    kind = "shortcuts.workbench",
    label = "Shortcuts",
    version = 4,
    migrate = upgrade
)]
impl Workbench {
    /// Open a shortcut the app proposed, named after the work it noticed, as
    /// an unsaved draft for review. Never arms a trigger.
    #[action]
    pub fn draft_proposal(
        &mut self,
        cx: &mut ActionCx,
        #[param("The shortcut that does the repeated work, as JSON")] shortcut: serde_json::Value,
    ) -> Result<(), Refusal> {
        if shortcut.to_string().len() > 16 * 1024 {
            return Err(Refusal::new("Shortcut draft is too large"));
        }
        let shortcut: Shortcut = serde_json::from_value(shortcut)
            .map_err(|error| Refusal::new(format!("not a shortcut: {error}")))?;
        if self
            .drafts
            .iter()
            .any(|(_, draft)| draft.name.trim() == shortcut.name.trim())
        {
            return Err(Refusal::new(format!(
                "A draft called “{}” is already open",
                shortcut.name.trim()
            )));
        }
        let id = cx.id();
        let draft_id = format!("s{}", id.as_str().get(..16).unwrap_or(id.as_str()));
        let program = Program {
            inputs: shortcut.inputs,
            steps: shortcut.steps,
            output: shortcut.output,
            triggers: Vec::new(),
            step_timeout_secs: shortcut.step_timeout_secs,
        };
        let draft = Draft::new(shortcut.name, draft_id, &program, false, &mut || cx.id());
        self.drafts.push(id, draft);
        Ok(())
    }

    /// Do not offer this repeated work again.
    #[action]
    pub fn ignore_proposal(
        &mut self,
        #[param("The steps of the proposal, as its key")] key: String,
    ) {
        if key.trim().is_empty() {
            return;
        }
        if !self.ignored.contains(&key) {
            self.ignored.push(key);
        }
    }

    /// Offer this repeated work again after all.
    #[action]
    pub fn allow_proposal(&mut self, #[param("The proposal's key")] key: String) {
        self.ignored.retain(|ignored| *ignored != key);
    }
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

    /// Copy a draft, its steps, inputs and triggers, into a new unsaved
    /// draft named after it. A variant starts from the whole automation
    /// instead of being rebuilt step by step.
    #[action]
    pub fn duplicate_draft(&mut self, cx: &mut ActionCx, draft: &Id) -> Result<(), Refusal> {
        let original = self
            .drafts
            .get(draft)
            .cloned()
            .ok_or_else(|| Refusal::from("there is no such draft"))?;
        let program = original.program();
        let name = if original.name.trim().is_empty() {
            String::from("Copy")
        } else {
            format!("{} copy", original.name.trim())
        };
        let name = unused_name(self, &name);
        let id = cx.id();
        let shortcut_id = format!("s{}", id.as_str().get(..16).unwrap_or(id.as_str()));
        let copy = Draft::new(name, shortcut_id, &program, false, &mut || cx.id());
        self.drafts.push(id, copy);
        Ok(())
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

    /// Copy a saved shortcut into a new unsaved draft for review. Its
    /// triggers are carried, so a duplicate that runs on its own is reviewed
    /// in the save sheet before it is armed.
    #[action]
    pub fn duplicate_shortcut(
        &mut self,
        cx: &mut ActionCx,
        #[param("The saved shortcut, as JSON")] shortcut: serde_json::Value,
    ) -> Result<(), Refusal> {
        let shortcut: Shortcut = serde_json::from_value(shortcut)
            .map_err(|error| Refusal::new(format!("not a shortcut: {error}")))?;
        let program = Program {
            inputs: shortcut.inputs,
            steps: shortcut.steps,
            output: shortcut.output,
            triggers: shortcut.triggers,
            step_timeout_secs: shortcut.step_timeout_secs,
        };
        let name = if shortcut.name.trim().is_empty() {
            String::from("Copy")
        } else {
            format!("{} copy", shortcut.name.trim())
        };
        let name = unused_name(self, &name);
        let id = cx.id();
        let shortcut_id = format!("s{}", id.as_str().get(..16).unwrap_or(id.as_str()));
        let copy = Draft::new(name, shortcut_id, &program, false, &mut || cx.id());
        self.drafts.push(id, copy);
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

    /// Say what a run does when a step fails: stop (the default), try it
    /// again a few times, do something else instead, or go on with a value
    /// of the shortcut's own.
    #[action]
    pub fn set_step_policy(
        &mut self,
        cx: &mut ActionCx,
        draft: &Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("What to do when it fails, as JSON; empty stops the run")] policy: String,
    ) -> Result<(), Refusal> {
        let policy: StepPolicy = if policy.trim().is_empty() {
            StepPolicy::Fail
        } else {
            serde_json::from_str(&policy)
                .map_err(|error| Refusal::new(format!("not a policy: {error}")))?
        };
        edit_program(self, cx, draft, |program| {
            step_mut(&mut program.steps, &path)
                .ok_or_else(|| format!("there is no step `{path}`"))?
                .on_error = policy;
            Ok(())
        })
    }

    /// Move a step into another block of the same draft: `""` for the top
    /// level, or a block of a step such as `2.then`, `2.else` or `3.each`.
    /// It lands at `index` there, counted from 0 (past the block's last
    /// step puts it last), and the steps that follow it make room. A step
    /// never moves inside its own blocks.
    #[action]
    pub fn set_nested(
        &mut self,
        cx: &mut ActionCx,
        draft: &Id,
        #[param("The step's position, like 0 or 2.then.1")] path: String,
        #[param("The block it moves into: empty for the top level, or like 2.then")] block: String,
        #[param("Where it lands there: 0 first, or past its last step")] index: usize,
    ) -> Result<(), Refusal> {
        draft_mut(self, draft)?
            .step_item(&path)
            .ok_or_else(|| Refusal::new(format!("there is no step `{path}`")))?;
        edit_program(self, cx, draft, |program| {
            let (from, at) = split_path(&path).ok_or_else(|| format!("no step `{path}`"))?;
            let moved = block_at(&program.steps, from)
                .and_then(|steps| steps.get(at))
                .cloned()
                .ok_or_else(|| format!("there is no step `{path}`"))?;
            if block == from || block.starts_with(&format!("{from}.")) {
                return Err(format!("step `{path}` cannot move into its own block"));
            }
            let count = block_at(&program.steps, &block)
                .ok_or_else(|| format!("there is no block `{block}`"))?
                .len();
            let from_block = from.to_owned();
            block_mut(&mut program.steps, &from_block)
                .ok_or_else(|| format!("there is no step `{path}`"))?
                .remove(at);
            let destination = block_mut(&mut program.steps, &block)
                .ok_or_else(|| format!("there is no block `{block}`"))?;
            destination.insert(index.min(count), moved);
            Ok(())
        })
    }

    /// Say what the shortcut reports when it finishes, or that it reports
    /// nothing.
    #[action]
    pub fn set_output(
        &mut self,
        cx: &mut ActionCx,
        draft: &Id,
        #[param("Where the value comes from, as binding JSON; empty reports nothing")]
        binding: String,
    ) -> Result<(), Refusal> {
        let binding = parse_binding(&binding)?;
        edit_program(self, cx, draft, |program| {
            program.output = binding;
            Ok(())
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
        StepKind::Task { .. } => "task".into(),
    };
    let base = if base.is_empty() { "step".into() } else { base };
    unique_step_id(steps, &base, "step")
}

/// `base`, made readable ("letters, digits, - or _"), and numbered past every
/// step id `steps` (at any depth) already has; `fallback` when it has nothing
/// left.
pub(crate) fn unique_step_id(steps: &[Step], base: &str, fallback: &str) -> String {
    let base: String = base
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
        fallback.to_owned()
    } else {
        base
    };
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
            | StepKind::Ai { .. }
            | StepKind::Task { .. } => {}
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

impl Draft {
    /// The item holding the step at `path` (`2.then.1`), which
    /// [`Workbench::set_nested`] moves in the stored draft itself.
    #[must_use]
    pub fn step_item(&self, path: &str) -> Option<Id> {
        let (block, index) = path.rsplit_once('.').unwrap_or(("", path));
        let block = segments(block)?;
        self.item_at(&block, index.parse().ok()?)
    }

    /// The item of the step at `index` of `block`.
    fn item_at(&self, block: &[Segment], index: usize) -> Option<Id> {
        let (parent, branch) = self.block_owner(block);
        self.steps
            .iter()
            .filter(|(_, node)| node.parent.0 == parent && node.branch.0 == branch)
            .map(|(key, _)| key.clone())
            .nth(index)
    }

    /// The item id of the step whose block `block` names, and which of its
    /// branches that block is.
    fn block_owner(&self, block: &[Segment]) -> (String, String) {
        let Some((Segment::Branch(branch), addressing)) = block.split_last() else {
            return (String::new(), String::new());
        };
        if addressing.is_empty() {
            return (String::new(), (*branch).to_owned());
        }
        let owner = self
            .item_at(addressing, index_of(addressing).unwrap_or_default())
            .map_or_else(String::new, |owner| owner.as_str().to_owned());
        (owner, (*branch).to_owned())
    }
}

/// A block address (`2.then`) as its segments: step indices, then the
/// branches they lead into.
type Block = Vec<Segment>;

/// One segment of a block address: where a step sits in its block, or which
/// of a step's blocks it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Segment {
    At(usize),
    Branch(&'static str),
}

/// The segments of `block`, in order; `None` when one is neither a step
/// index nor a branch.
fn segments(block: &str) -> Option<Block> {
    block
        .split('.')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            if let Some(branch) = [THEN, ELSE, EACH]
                .into_iter()
                .find(|branch| *branch == segment)
            {
                return Some(Segment::Branch(branch));
            }
            segment.parse().ok().map(Segment::At)
        })
        .collect()
}

/// The index `addressing` ends with, when it ends with one.
fn index_of(addressing: &[Segment]) -> Option<usize> {
    match addressing.last() {
        Some(Segment::At(index)) => Some(*index),
        _ => None,
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
