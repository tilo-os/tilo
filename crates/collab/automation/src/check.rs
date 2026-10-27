//! Save-time checking of a shortcut against the descriptors it will run on.
//!
//! The checker answers "could this run at all?" before anything runs: every
//! step names an action the invoking person is offered, every argument is
//! bound to a value whose type can satisfy the parameter, every step output
//! pointer exists in the declared result schema where the schema says, every
//! action lies inside the shortcut's grant, and no chain of shortcuts leads
//! back to the one being saved. Errors name the step path they are about.
//!
//! What cannot be known at save time (a value typed only as "any", an open
//! schema) is left to the run, which binds every argument strictly against
//! the live descriptor before invoking.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::Value;
use tilo_actions::ActionKey;
use tilo_actions::door::ActionDescriptor;
use tilo_actions::params::{ActionParam, ParamType};

use crate::catalog::{ActionCatalog, ResourceTarget};
use crate::expression::{Expr, Op, field_tokens};
use crate::grant::{EDIT_PARAM, FIELD_EDIT, edit_reach, grant_answer, step_node, step_node_at};
use crate::path::StepPath;
use crate::shortcut::{AiTransform, Binding, Condition, Selector, Shortcut, Step, StepKind};
use crate::template::{self, Piece, is_identifier};
use crate::types::{Ty, assignable, literal_fits, pointer_tokens, schema_at};
use crate::{AGENT_TRANSFORM_KIND, SHORTCUT_RUN_ACTION, SHORTCUTS_APP_ID, shortcut_id_of};
use tilo_action_policy::{Answer, Rule};

/// One reason a shortcut cannot be saved as written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {kind}")]
pub struct CheckError {
    /// The step (or `output`) the problem is in.
    pub path: StepPath,
    pub kind: CheckErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CheckErrorKind {
    #[error("a bot's grant names `{0}`, which none of its steps or triggers use")]
    UnusedRule(String),
    #[error("a bot's grant leaves out `{0}`, which its steps or triggers need")]
    MissingRule(String),
    #[error("{what} `{value}` must be non-empty ASCII letters, digits, `-` or `_`")]
    InvalidName { what: &'static str, value: String },
    #[error("the shortcut needs a name")]
    MissingName,
    #[error("step id `{0}` is used more than once")]
    DuplicateStep(String),
    #[error("input `{0}` is declared more than once")]
    DuplicateInput(String),
    #[error("parameter `{name}` is malformed: {reason}")]
    InvalidParam { name: String, reason: String },
    #[error("there is no input `{0}`")]
    UnknownInput(String),
    #[error("variable `{0}` is not set by any earlier step in scope")]
    UnknownVariable(String),
    #[error("no earlier step `{0}` is in scope")]
    UnknownStep(String),
    #[error("step `{0}` produces no value")]
    NoOutput(String),
    #[error("`{app_id}` offers no action `{action}` on {resource}")]
    UnknownAction {
        app_id: String,
        resource: String,
        action: String,
    },
    #[error("`{name}` is not a parameter of this action")]
    UnknownArgument { name: String },
    #[error("required argument `{name}` is not bound")]
    MissingArgument { name: String },
    #[error("{0}")]
    Incompatible(String),
    #[error("pointer `{pointer}` into step `{step}`: {reason}")]
    BadPointer {
        step: String,
        pointer: String,
        reason: String,
    },
    #[error("template: {0}")]
    BadTemplate(String),
    #[error("a template may not include {0}")]
    SecretInTemplate(String),
    #[error("a loop needs an array, not {0}")]
    NotIterable(String),
    #[error("a loop needs a whole number of times, not {0}")]
    NotCountable(String),
    #[error("a loop must run at most once per element: raise its limit to at least 1")]
    UnboundedLoop,
    #[error("expression: {0}")]
    Expression(String),
    #[error("the shortcut's grant does not cover `{action}` on `{app_id}`")]
    OutsideGrant { app_id: String, action: String },
    #[error("a shortcut run must name the shortcut exactly, so a cycle can be ruled out")]
    UnresolvableShortcut,
    #[error("the shortcut would run itself: {}", chain.join(" → "))]
    Recursion { chain: Vec<String> },
}

/// Looks up other saved shortcuts by id, so the checker can follow a chain
/// of shortcuts running shortcuts.
pub trait ShortcutResolver {
    fn shortcut(&self, id: &str) -> Option<Shortcut>;
}

/// A resolver that knows no other shortcuts.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoShortcuts;

impl ShortcutResolver for NoShortcuts {
    fn shortcut(&self, _id: &str) -> Option<Shortcut> {
        None
    }
}

impl<F: Fn(&str) -> Option<Shortcut>> ShortcutResolver for F {
    fn shortcut(&self, id: &str) -> Option<Shortcut> {
        self(id)
    }
}

/// The checks a bot program passes on top of [`check`]: its grant is exactly
/// what its steps and triggers derive against `catalog`, no more and no
/// less. A person's Shortcut may carry a hand-widened grant; a bot acts
/// unattended under its own identity, so nobody may hand it more than its
/// program shows.
pub fn check_bot(shortcut: &Shortcut, catalog: &ActionCatalog) -> Result<(), Vec<CheckError>> {
    let mut errors = check(shortcut, catalog, &NoShortcuts)
        .err()
        .unwrap_or_default();
    let derived = crate::grant::derived_rules(shortcut, catalog);
    for rule in &shortcut.grant {
        if !derived.contains(rule) {
            errors.push(CheckError {
                path: StepPath::root(),
                kind: CheckErrorKind::UnusedRule(rule.node.to_string()),
            });
        }
    }
    for rule in &derived {
        if !shortcut.grant.contains(rule) {
            errors.push(CheckError {
                path: StepPath::root(),
                kind: CheckErrorKind::MissingRule(rule.node.to_string()),
            });
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Check `shortcut` against the descriptors in `catalog`.
pub fn check(
    shortcut: &Shortcut,
    catalog: &ActionCatalog,
    resolver: &impl ShortcutResolver,
) -> Result<(), Vec<CheckError>> {
    let mut checker = Checker {
        shortcut,
        catalog,
        inputs: BTreeMap::new(),
        step_ids: HashSet::new(),
        errors: Vec::new(),
        probe: None,
    };
    checker.header();
    let mut scope = Scope::default();
    checker.block(&shortcut.steps, &StepPath::root(), &mut scope);
    if let Some(output) = &shortcut.output {
        checker.value(output, &scope, &StepPath::output());
    }
    checker.errors.extend(recursion(shortcut, resolver));
    if checker.errors.is_empty() {
        Ok(())
    } else {
        Err(checker.errors)
    }
}

/// A step's value, as later steps may refer to it: its type, and the schema
/// a pointer into it walks when one is declared. `None` for no value.
type Output = Option<(Ty, Option<Value>)>;

/// What is visible at one point of the program. Steps and variables are
/// lexically scoped: whatever a branch or loop body introduces is gone after
/// it, because that block may not have run.
#[derive(Clone, Default)]
struct Scope {
    steps: BTreeMap<String, Output>,
    vars: BTreeMap<String, Ty>,
}

impl Scope {
    /// Carry assignments a nested block made to variables that already
    /// existed here. A variable may not have been reassigned (the block may
    /// not have run), so the type is the join of both.
    fn merge_assignments(&mut self, inner: &Self) {
        for (name, ty) in &mut self.vars {
            if let Some(assigned) = inner.vars.get(name)
                && assigned != ty
            {
                *ty = Ty::Any;
            }
        }
    }
}

/// A step's resource as far as it is known at save time.
enum Target {
    Id(String),
    Kind(String),
    Any,
}

impl Target {
    fn as_ref(&self) -> ResourceTarget<'_> {
        match self {
            Self::Id(id) => ResourceTarget::Id(id),
            Self::Kind(kind) => ResourceTarget::Kind(kind),
            Self::Any => ResourceTarget::Any,
        }
    }
}

struct Checker<'a> {
    shortcut: &'a Shortcut,
    catalog: &'a ActionCatalog,
    inputs: BTreeMap<String, &'a ActionParam>,
    step_ids: HashSet<String>,
    errors: Vec<CheckError>,
    /// A position to record the scope at, and the scope once reached.
    probe: Option<(StepPath, Option<Scope>)>,
}

impl Checker<'_> {
    fn fail(&mut self, path: &StepPath, kind: CheckErrorKind) {
        self.errors.push(CheckError {
            path: path.clone(),
            kind,
        });
    }

    fn header(&mut self) {
        let root = StepPath::root();
        if !is_identifier(&self.shortcut.id) {
            self.fail(
                &root,
                CheckErrorKind::InvalidName {
                    what: "shortcut id",
                    value: self.shortcut.id.clone(),
                },
            );
        }
        if self.shortcut.name.trim().is_empty() {
            self.fail(&root, CheckErrorKind::MissingName);
        }
        for input in &self.shortcut.inputs {
            let name = input.name.to_string();
            if !is_identifier(&name) {
                self.fail(
                    &root,
                    CheckErrorKind::InvalidName {
                        what: "input",
                        value: name.clone(),
                    },
                );
            }
            for reason in input.validate_declaration() {
                self.fail(
                    &root,
                    CheckErrorKind::InvalidParam {
                        name: name.clone(),
                        reason,
                    },
                );
            }
            if self.inputs.insert(name.clone(), input).is_some() {
                self.fail(&root, CheckErrorKind::DuplicateInput(name));
            }
        }
    }

    fn observe(&mut self, at: &StepPath, scope: &Scope) {
        if let Some((target, seen @ None)) = &mut self.probe
            && target == at
        {
            *seen = Some(scope.clone());
        }
    }

    fn block(&mut self, steps: &[Step], path: &StepPath, scope: &mut Scope) {
        for (index, step) in steps.iter().enumerate() {
            let path = path.child(index);
            self.observe(&path, scope);
            if !is_identifier(&step.id) {
                self.fail(
                    &path,
                    CheckErrorKind::InvalidName {
                        what: "step id",
                        value: step.id.clone(),
                    },
                );
            }
            if !self.step_ids.insert(step.id.clone()) {
                self.fail(&path, CheckErrorKind::DuplicateStep(step.id.clone()));
            }
            let output = self.step(step, &path, scope);
            scope.steps.insert(step.id.clone(), output);
        }
        self.observe(&path.child(steps.len()), scope);
    }

    fn step(&mut self, step: &Step, path: &StepPath, scope: &mut Scope) -> Output {
        match &step.kind {
            StepKind::Invoke {
                action,
                resource,
                args,
                appearance,
            } => {
                if let Some(appearance) = appearance {
                    self.value(appearance, scope, path);
                }
                self.invoke(path, scope, action, resource, args)
            }
            StepKind::Ai {
                prompt,
                output,
                input,
            } => Some(self.ai(path, scope, prompt, output.as_ref(), input.as_ref())),
            StepKind::Task {
                instruction,
                allow,
                output,
            } => Some(self.task(path, scope, instruction, allow, output.as_ref())),
            StepKind::If {
                cond,
                then,
                otherwise,
            } => {
                self.condition(cond, scope, path);
                for (branch, steps) in [(path.then_branch(), then), (path.else_branch(), otherwise)]
                {
                    let mut inner = scope.clone();
                    self.block(steps, &branch, &mut inner);
                    scope.merge_assignments(&inner);
                }
                None
            }
            StepKind::ForEach { items, item, body } => {
                self.for_each(path, scope, items, item, body);
                None
            }
            StepKind::SetVar { name, value } => {
                if !is_identifier(name) {
                    self.fail(
                        path,
                        CheckErrorKind::InvalidName {
                            what: "variable",
                            value: name.clone(),
                        },
                    );
                }
                let ty = self.value(value, scope, path).unwrap_or(Ty::Any);
                let ty = match scope.vars.get(name) {
                    Some(existing) if *existing != ty => Ty::Any,
                    _ => ty,
                };
                scope.vars.insert(name.clone(), ty);
                None
            }
            StepKind::Compute { expr } => Some((self.expression(expr, scope, path), expr.schema())),
            StepKind::Repeat { count, index, body } => {
                self.repeat(path, scope, count, index, body);
                None
            }
            StepKind::While { cond, max, body } => {
                if *max == 0 {
                    self.fail(path, CheckErrorKind::UnboundedLoop);
                }
                self.condition(cond, scope, path);
                let mut inner = scope.clone();
                self.block(body, &path.while_loop(None), &mut inner);
                scope.merge_assignments(&inner);
                None
            }
            StepKind::Return { value } => {
                if let Some(value) = value {
                    self.value(value, scope, path);
                }
                None
            }
            StepKind::Ask { param } => {
                for reason in param.validate_declaration() {
                    self.fail(
                        path,
                        CheckErrorKind::InvalidParam {
                            name: param.name.to_string(),
                            reason,
                        },
                    );
                }
                Some((Ty::of_param(param), param.schema.as_deref().cloned()))
            }
        }
    }

    fn for_each(
        &mut self,
        path: &StepPath,
        scope: &mut Scope,
        items: &Binding,
        item: &str,
        body: &[Step],
    ) {
        if !is_identifier(item) {
            self.fail(
                path,
                CheckErrorKind::InvalidName {
                    what: "loop variable",
                    value: item.to_string(),
                },
            );
        }
        let element = match self.value(items, scope, path) {
            Some(Ty::Array(schema)) => schema.as_ref().map_or(Ty::Any, Ty::of_schema),
            Some(Ty::Any) | None => Ty::Any,
            Some(other) => {
                self.fail(path, CheckErrorKind::NotIterable(other.to_string()));
                Ty::Any
            }
        };
        let mut inner = scope.clone();
        inner.vars.insert(item.to_string(), element);
        self.block(body, &path.each(None), &mut inner);
        if let Some(shadowed) = scope.vars.get(item) {
            inner.vars.insert(item.to_string(), shadowed.clone());
        }
        scope.merge_assignments(&inner);
    }

    /// A counted repeat: its body runs `count` times, with a whole number from
    /// 0 below `count` bound to `index` — a number the loop bound knows.
    fn repeat(
        &mut self,
        path: &StepPath,
        scope: &mut Scope,
        count: &Binding,
        index: &str,
        body: &[Step],
    ) {
        if !is_identifier(index) {
            self.fail(
                path,
                CheckErrorKind::InvalidName {
                    what: "loop variable",
                    value: index.to_string(),
                },
            );
        }
        let known = match self.value(count, scope, path) {
            Some(Ty::Integer) => {
                if let Binding::Literal(value) = count
                    && value.as_u64() == Some(0)
                {
                    self.fail(path, CheckErrorKind::UnboundedLoop);
                }
                true
            }
            // Anything else the check cannot see through may still be a whole
            // number at run time, which the run holds it to.
            Some(Ty::Any) | None => false,
            Some(other) => {
                self.fail(path, CheckErrorKind::NotCountable(other.to_string()));
                false
            }
        };
        let mut inner = scope.clone();
        inner
            .vars
            .insert(index.to_string(), if known { Ty::Integer } else { Ty::Any });
        self.block(body, &path.repeat(None), &mut inner);
        if let Some(shadowed) = scope.vars.get(index) {
            inner.vars.insert(index.to_string(), shadowed.clone());
        }
        scope.merge_assignments(&inner);
    }

    /// The type of `expr` here, reporting anything about it that could not
    /// hold: an operand of the wrong kind, a comparison of values of which
    /// neither is the other's kind, a `map` over a field the elements do not
    /// all have.
    fn expression(&mut self, expr: &Expr, scope: &Scope, path: &StepPath) -> Ty {
        let report = |checker: &mut Self, reason: String| {
            checker.fail(path, CheckErrorKind::Expression(reason));
        };
        match expr {
            Expr::Value { binding } => self.value(binding, scope, path).unwrap_or(Ty::Any),
            Expr::Compare { op, left, right } => {
                let (left_ty, right_ty) = (
                    self.expression(left, scope, path),
                    self.expression(right, scope, path),
                );
                if let Err(reason) = comparable(op, &left_ty, &right_ty) {
                    report(
                        self,
                        format!("{left_ty} is not comparable with {right_ty}: {reason}"),
                    );
                }
                Ty::Boolean
            }
            Expr::Logic { op, left, right } => {
                for side in [left, right] {
                    let ty = self.expression(side, scope, path);
                    if let Err(reason) = truth_value(&ty) {
                        report(
                            self,
                            format!("`{}` needs truth values: {reason}", op.as_str()),
                        );
                    }
                }
                Ty::Boolean
            }
            Expr::Math { op, left, right } => {
                let (left_ty, right_ty) = (
                    self.expression(left, scope, path),
                    self.expression(right, scope, path),
                );
                if let Err(reason) = arithmetic_operands(*op, &left_ty, &right_ty) {
                    report(
                        self,
                        format!("`{}` of {left_ty} and {right_ty}: {reason}", op.as_str()),
                    );
                }
                match op {
                    Op::Concat => Ty::Text,
                    // A whole division of two whole numbers may still not be
                    // whole, so every arithmetic answer is a number.
                    _ => Ty::Number,
                }
            }
            Expr::Negate { value } => {
                let ty = self.expression(value, scope, path);
                if let Err(reason) = arithmetic_operands(Op::Subtract, &Ty::zero(), &ty) {
                    report(self, reason);
                }
                Ty::Number
            }
            Expr::Truthy { value } | Expr::Not { value } => {
                self.expression(value, scope, path);
                Ty::Boolean
            }
            Expr::Length { value } => {
                let ty = self.expression(value, scope, path);
                if let Err(reason) = countable(&ty) {
                    report(self, reason);
                }
                Ty::Integer
            }
            Expr::Replace {
                text,
                find,
                replace,
            } => {
                for side in [text, find, replace] {
                    let ty = self.expression(side, scope, path);
                    if let Err(reason) = textual(&ty) {
                        report(self, reason);
                    }
                }
                Ty::Text
            }
            Expr::Split { text, separator } => {
                for side in std::iter::once(text.as_ref()).chain(separator.as_deref()) {
                    let ty = self.expression(side, scope, path);
                    if let Err(reason) = textual(&ty) {
                        report(self, reason);
                    }
                }
                Ty::Array(Some(serde_json::json!({"type": "string"})))
            }
            Expr::Take { items, count } => {
                let ty = self.expression(items, scope, path);
                let element = match ty {
                    Ty::Array(items) => Ty::Array(items),
                    Ty::Any | Ty::Null => Ty::Array(None),
                    other => {
                        report(self, format!("take needs a list, and it is given {other}"));
                        Ty::Array(None)
                    }
                };
                let count_ty = self.expression(count, scope, path);
                if let Err(reason) = whole_number(&count_ty) {
                    report(self, reason);
                }
                element
            }
            Expr::Field { value, field } => {
                let ty = self.expression(value, scope, path);
                match field_ty(&ty, field) {
                    Ok(field) => field,
                    Err(reason) => {
                        report(self, reason);
                        Ty::Any
                    }
                }
            }
            Expr::Object { members } => {
                for member in members {
                    self.expression(&member.value, scope, path);
                }
                Ty::Object(expr.schema())
            }
            Expr::Filter { items, test } => {
                let ty = self.expression(items, scope, path);
                let (items, element) = match ty {
                    Ty::Array(items) => {
                        let element = items.as_ref().map_or(Ty::Any, Ty::of_schema);
                        (Ty::Array(items), element)
                    }
                    Ty::Any | Ty::Null => (Ty::Array(None), Ty::Any),
                    other => {
                        report(
                            self,
                            format!("filter needs a list, and it is given {other}"),
                        );
                        (Ty::Array(None), Ty::Any)
                    }
                };
                // The test reads the element it judges as `item`, as a loop
                // body reads its loop variable.
                let mut inner = scope.clone();
                inner.vars.insert(FILTER_ITEM.to_owned(), element);
                let test_ty = self.expression(test, &inner, path);
                if let Err(reason) = truth_value(&test_ty) {
                    report(self, reason);
                }
                items
            }
            Expr::Map { items, field } => {
                let ty = self.expression(items, scope, path);
                let element = match &ty {
                    Ty::Array(items) => items.as_ref().map_or(Ty::Any, Ty::of_schema),
                    Ty::Any | Ty::Null => Ty::Any,
                    other => {
                        report(self, format!("map needs a list, and it is given {other}"));
                        Ty::Any
                    }
                };
                match field_ty(&element, field) {
                    Ok(Ty::Any) if element == Ty::Any => Ty::Array(None),
                    Ok(field) => Ty::Array(Some(field.schema())),
                    Err(reason) => {
                        report(self, reason);
                        Ty::Array(None)
                    }
                }
            }
            Expr::Sort { items } => {
                let ty = self.expression(items, scope, path);
                match ty {
                    Ty::Array(_) | Ty::Any | Ty::Null => ty,
                    other => {
                        report(self, format!("sort needs a list, and it is given {other}"));
                        Ty::Array(None)
                    }
                }
            }
            Expr::First { items } => {
                let ty = self.expression(items, scope, path);
                match ty {
                    Ty::Array(items) => items.as_ref().map_or(Ty::Any, Ty::of_schema),
                    Ty::Any | Ty::Null => Ty::Any,
                    other => {
                        report(self, format!("first needs a list, and it is given {other}"));
                        Ty::Any
                    }
                }
            }
        }
    }

    fn ai(
        &mut self,
        path: &StepPath,
        scope: &Scope,
        prompt: &str,
        output: Option<&Value>,
        input: Option<&Binding>,
    ) -> (Ty, Option<Value>) {
        if let Some(input) = input {
            self.value(input, scope, path);
        }
        if let Some(schema) = output
            && let Err(reason) = Self::output_schema(schema)
        {
            self.fail(path, CheckErrorKind::Incompatible(reason));
        }
        let transform = AiTransform::Freeform {
            prompt: prompt.to_string(),
            output: output.cloned(),
        };
        self.invoke(
            path,
            scope,
            &transform.key(),
            &Selector::FirstOfKind(AGENT_TRANSFORM_KIND.into()),
            &transform.arguments(input),
        );
        declared(output)
    }

    /// A [`Task`](StepKind::Task) step is one invocation of the agent's task
    /// action, answered by the task's summary in the shape the step declares.
    fn task(
        &mut self,
        path: &StepPath,
        scope: &Scope,
        instruction: &str,
        allow: &[Rule],
        output: Option<&Value>,
    ) -> (Ty, Option<Value>) {
        if instruction.trim().is_empty() {
            self.fail(
                path,
                CheckErrorKind::Incompatible("a task needs an instruction".into()),
            );
        }
        if let Some(schema) = output
            && let Err(reason) = Self::output_schema(schema)
        {
            self.fail(path, CheckErrorKind::Incompatible(reason));
        }
        let transform = AiTransform::Task {
            instruction: instruction.to_string(),
            allow: allow.to_vec(),
            output: output.cloned(),
        };
        self.invoke(
            path,
            scope,
            &transform.key(),
            &Selector::FirstOfKind(AGENT_TRANSFORM_KIND.into()),
            &transform.arguments(None),
        );
        declared(output)
    }

    /// Whether `schema` is a shape the agent could hold its answer to.
    fn output_schema(schema: &Value) -> Result<(), String> {
        let errors = ActionParam::optional("output", ParamType::Object, "The answer's shape")
            .with_schema(schema.clone())
            .validate_declaration();
        match errors.first() {
            Some(error) => Err(format!("the declared output cannot be checked: {error}")),
            None => Ok(()),
        }
    }

    fn invoke(
        &mut self,
        path: &StepPath,
        scope: &Scope,
        key: &ActionKey,
        selector: &Selector,
        args: &BTreeMap<String, Binding>,
    ) -> Output {
        let (app_id, action) = (key.app.as_str(), key.action.as_str());
        let unknown = Some((Ty::Any, None));
        let target = self.selector(selector, scope, path);
        if app_id == SHORTCUTS_APP_ID
            && action == SHORTCUT_RUN_ACTION
            && !matches!(target, Some(Target::Id(_)))
        {
            self.fail(path, CheckErrorKind::UnresolvableShortcut);
        }
        let target = target.unwrap_or(Target::Any);
        let row = self.catalog.find(key, target.as_ref());
        let Some(descriptor) = row.and_then(|row| row.action.as_ref()) else {
            let resource = match &target {
                Target::Id(id) => format!("`{id}`"),
                Target::Kind(kind) => format!("a `{kind}` resource"),
                Target::Any => "any resource".into(),
            };
            self.fail(
                path,
                CheckErrorKind::UnknownAction {
                    app_id: app_id.to_string(),
                    resource,
                    action: action.to_string(),
                },
            );
            return unknown;
        };
        let at = (action == FIELD_EDIT)
            .then(|| args.get(EDIT_PARAM).and_then(edit_reach))
            .flatten();
        let node = match &at {
            Some(at) => step_node_at(
                None,
                app_id,
                &descriptor.resource_kind,
                &descriptor.address,
                at,
            ),
            None => step_node(None, app_id, &descriptor.resource_kind, &descriptor.address),
        };
        if grant_answer(&self.shortcut.grant, &node, descriptor.contract.need().kind).answer
            == Answer::Deny
        {
            self.fail(
                path,
                CheckErrorKind::OutsideGrant {
                    app_id: app_id.to_string(),
                    action: action.to_string(),
                },
            );
        }
        self.arguments(descriptor, args, scope, path);
        Ty::of_result(&descriptor.contract.result)
    }

    fn selector(&mut self, selector: &Selector, scope: &Scope, path: &StepPath) -> Option<Target> {
        match selector {
            Selector::Fixed(id) | Selector::FromBinding(Binding::Literal(Value::String(id))) => {
                Some(Target::Id(id.clone()))
            }
            Selector::FirstOfKind(kind) => Some(Target::Kind(kind.clone())),
            Selector::FromBinding(binding) => match self.value(binding, scope, path)? {
                // Rows of the bound kind or a sub-kind of it.
                Ty::Resource(Some(kind)) => Some(Target::Kind(kind)),
                Ty::Resource(None) | Ty::Any => Some(Target::Any),
                other => {
                    self.fail(
                        path,
                        CheckErrorKind::Incompatible(format!(
                            "the resource is bound to {other}, not a resource address"
                        )),
                    );
                    None
                }
            },
        }
    }

    fn arguments(
        &mut self,
        descriptor: &ActionDescriptor,
        args: &BTreeMap<String, Binding>,
        scope: &Scope,
        path: &StepPath,
    ) {
        for (name, binding) in args {
            let Some(param) = descriptor
                .params
                .iter()
                .find(|param| param.name.as_ref() == name.as_str())
            else {
                self.fail(path, CheckErrorKind::UnknownArgument { name: name.clone() });
                continue;
            };
            let fits = match binding {
                Binding::Literal(Value::Null) if !param.required => Ok(()),
                Binding::Literal(value) => literal_fits(value, param),
                other => match self.value(other, scope, path) {
                    Some(ty) => assignable(&ty, param),
                    None => Ok(()),
                },
            };
            if let Err(reason) = fits {
                self.fail(path, CheckErrorKind::Incompatible(reason));
            }
        }
        for param in descriptor.params.iter().filter(|param| param.required) {
            if !args.contains_key(param.name.as_ref()) {
                self.fail(
                    path,
                    CheckErrorKind::MissingArgument {
                        name: param.name.to_string(),
                    },
                );
            }
        }
    }

    fn condition(&mut self, condition: &Condition, scope: &Scope, path: &StepPath) {
        match condition {
            Condition::Truthy(value) => {
                self.value(value, scope, path);
            }
            Condition::Equals { left, right } => {
                self.value(left, scope, path);
                self.value(right, scope, path);
            }
            Condition::Holds(expr) => {
                let ty = self.expression(expr, scope, path);
                if let Err(reason) = truth_value(&ty) {
                    self.fail(path, CheckErrorKind::Expression(reason));
                }
            }
            Condition::Not(inner) => self.condition(inner, scope, path),
            Condition::All(all) | Condition::Any(all) => {
                for inner in all {
                    self.condition(inner, scope, path);
                }
            }
        }
    }

    /// The type of `binding` here, or `None` after reporting why it has none.
    fn value(&mut self, binding: &Binding, scope: &Scope, path: &StepPath) -> Option<Ty> {
        match binding {
            Binding::Literal(value) => Some(literal_ty(value)),
            Binding::Input(name) => {
                let (base, pointer) = Binding::named_part(name);
                let found = self.inputs.get(base).map(|param| Ty::of_param(param));
                if found.is_none() {
                    self.fail(path, CheckErrorKind::UnknownInput(base.to_owned()));
                }
                found.map(|ty| part_ty(ty, &pointer))
            }
            Binding::Var(name) => {
                let (base, pointer) = Binding::named_part(name);
                let found = scope.vars.get(base).cloned();
                if found.is_none() {
                    self.fail(path, CheckErrorKind::UnknownVariable(base.to_owned()));
                }
                found.map(|ty| part_ty(ty, &pointer))
            }
            Binding::StepOutput { step, pointer } => self.step_output(step, pointer, scope, path),
            Binding::Template(text) => {
                let pieces = match template::parse(text) {
                    Ok(pieces) => pieces,
                    Err(reason) => {
                        self.fail(path, CheckErrorKind::BadTemplate(reason));
                        return None;
                    }
                };
                for piece in pieces {
                    if let Piece::Value(inner) = piece
                        && let Some(ty) = self.value(&inner, scope, path)
                        && !ty.renders_as_text()
                    {
                        self.fail(path, CheckErrorKind::SecretInTemplate(ty.to_string()));
                    }
                }
                Some(Ty::Text)
            }
            Binding::Object(members) => {
                for member in members.values() {
                    self.value(member, scope, path);
                }
                Some(Ty::Object(None))
            }
            Binding::List(items) => {
                for item in items {
                    self.value(item, scope, path);
                }
                Some(Ty::Array(None))
            }
            Binding::Expression(expr) => Some(self.expression(expr, scope, path)),
        }
    }

    fn step_output(
        &mut self,
        step: &str,
        pointer: &str,
        scope: &Scope,
        path: &StepPath,
    ) -> Option<Ty> {
        let bad_pointer = |reason: String| CheckErrorKind::BadPointer {
            step: step.to_string(),
            pointer: pointer.to_string(),
            reason,
        };
        let Some(output) = scope.steps.get(step) else {
            self.fail(path, CheckErrorKind::UnknownStep(step.to_string()));
            return None;
        };
        let Some((ty, schema)) = output else {
            self.fail(path, CheckErrorKind::NoOutput(step.to_string()));
            return None;
        };
        if let Err(reason) = pointer_tokens(pointer) {
            self.fail(path, bad_pointer(reason));
            return None;
        }
        if pointer.is_empty() {
            return Some(ty.clone());
        }
        let result = match (schema, ty) {
            (Some(schema), _) => schema_at(schema, pointer),
            (None, Ty::Any) => Ok(None),
            (None, other) => Err(format!("the result is {other}, which has no fields")),
        };
        match result {
            Ok(Some(schema)) => Some(Ty::of_schema(&schema)),
            Ok(None) => Some(Ty::Any),
            Err(reason) => {
                self.fail(path, bad_pointer(reason));
                None
            }
        }
    }
}

/// A value available to bind at one point of a shortcut, with what the
/// checker knows about its type.
#[derive(Clone, Debug, PartialEq)]
pub struct BindingOption {
    pub binding: Binding,
    /// The value's type in words: "text", "an array", "a `notes.note`
    /// resource".
    pub describes: String,
}

/// The inputs, variables and earlier step outputs in scope just before the
/// step at `at` runs that could satisfy `param` — exactly the bindings the
/// checker accepts there. A step's output is offered whole and, where its
/// declared schema names them, field by field. `at` may also be the
/// position just past a block's last step, where a new step would go.
#[must_use]
pub fn binding_options(
    shortcut: &Shortcut,
    catalog: &ActionCatalog,
    at: &StepPath,
    param: &ActionParam,
) -> Vec<BindingOption> {
    values_in_scope(shortcut, catalog, at)
        .into_iter()
        .filter(|(_, ty)| assignable(ty, param).is_ok())
        .map(|(binding, ty)| BindingOption {
            binding,
            describes: ty.to_string(),
        })
        .collect()
}

/// Every input, variable and earlier step output in scope just before the
/// step at `at` runs, whatever its type.
#[must_use]
pub fn values_at(
    shortcut: &Shortcut,
    catalog: &ActionCatalog,
    at: &StepPath,
) -> Vec<BindingOption> {
    values_in_scope(shortcut, catalog, at)
        .into_iter()
        .map(|(binding, ty)| BindingOption {
            binding,
            describes: ty.to_string(),
        })
        .collect()
}

fn values_in_scope(
    shortcut: &Shortcut,
    catalog: &ActionCatalog,
    at: &StepPath,
) -> Vec<(Binding, Ty)> {
    let mut checker = Checker {
        shortcut,
        catalog,
        inputs: BTreeMap::new(),
        step_ids: HashSet::new(),
        errors: Vec::new(),
        probe: Some((at.clone(), None)),
    };
    checker.header();
    let mut scope = Scope::default();
    checker.block(&shortcut.steps, &StepPath::root(), &mut scope);
    let Some((_, Some(seen))) = checker.probe.take() else {
        return Vec::new();
    };
    let mut values: Vec<(Binding, Ty)> = checker
        .inputs
        .iter()
        .map(|(name, param)| (Binding::Input(name.clone()), Ty::of_param(param)))
        .collect();
    values.extend(
        seen.vars
            .iter()
            .map(|(name, ty)| (Binding::Var(name.clone()), ty.clone())),
    );
    for (step, output) in &seen.steps {
        let Some((ty, schema)) = output else { continue };
        values.push((Binding::step(step.clone(), ""), ty.clone()));
        let properties = schema
            .as_ref()
            .and_then(|schema| schema.get("properties"))
            .and_then(Value::as_object);
        for (field, property) in properties.into_iter().flatten() {
            let pointer = format!("/{}", field.replace('~', "~0").replace('/', "~1"));
            values.push((
                Binding::step(step.clone(), pointer),
                Ty::of_schema(property),
            ));
        }
    }
    values
}

fn literal_ty(value: &Value) -> Ty {
    match value {
        Value::Null => Ty::Null,
        Value::Bool(_) => Ty::Boolean,
        Value::Number(number) if number.is_i64() || number.is_u64() => Ty::Integer,
        Value::Number(_) => Ty::Number,
        Value::String(_) => Ty::Text,
        Value::Object(_) => Ty::Object(None),
        Value::Array(_) => Ty::Array(None),
    }
}

/// The value an AI or task step's answer has, as later steps may refer to
/// it: the declared shape, or text when the step declares none.
fn declared(output: Option<&Value>) -> (Ty, Option<Value>) {
    match output {
        Some(schema) => (Ty::of_schema(schema), Some(schema.clone())),
        None => (Ty::Text, None),
    }
}

/// Every step of `steps` that runs a shortcut named exactly, with the id.
fn shortcut_calls(steps: &[Step], path: &StepPath, out: &mut Vec<(StepPath, String)>) {
    for (index, step) in steps.iter().enumerate() {
        let path = path.child(index);
        match &step.kind {
            StepKind::Invoke {
                action,
                resource: Selector::Fixed(resource),
                ..
            } if action.app == SHORTCUTS_APP_ID && action.action == SHORTCUT_RUN_ACTION => {
                if let Some(id) = shortcut_id_of(resource) {
                    out.push((path, id.to_string()));
                }
            }
            StepKind::If {
                then, otherwise, ..
            } => {
                shortcut_calls(then, &path.then_branch(), out);
                shortcut_calls(otherwise, &path.else_branch(), out);
            }
            StepKind::ForEach { body, .. } => shortcut_calls(body, &path.each(None), out),
            StepKind::Repeat { body, .. } => shortcut_calls(body, &path.repeat(None), out),
            StepKind::While { body, .. } => shortcut_calls(body, &path.while_loop(None), out),
            StepKind::Invoke { .. }
            | StepKind::SetVar { .. }
            | StepKind::Compute { .. }
            | StepKind::Return { .. }
            | StepKind::Ask { .. }
            | StepKind::Ai { .. }
            | StepKind::Task { .. } => {}
        }
    }
}

/// Every chain of shortcut runs, starting at one of `root`'s steps, that
/// leads back to `root`.
fn recursion(root: &Shortcut, resolver: &impl ShortcutResolver) -> Vec<CheckError> {
    let mut direct = Vec::new();
    shortcut_calls(&root.steps, &StepPath::root(), &mut direct);
    let mut errors = Vec::new();
    for (path, callee) in direct {
        let mut visited = BTreeSet::new();
        let mut pending = vec![vec![root.id.clone(), callee]];
        while let Some(chain) = pending.pop() {
            let Some(last) = chain.last() else { continue };
            if *last == root.id {
                errors.push(CheckError {
                    path: path.clone(),
                    kind: CheckErrorKind::Recursion { chain },
                });
                break;
            }
            if !visited.insert(last.clone()) {
                continue;
            }
            let Some(next) = resolver.shortcut(last) else {
                continue;
            };
            let mut calls = Vec::new();
            shortcut_calls(&next.steps, &StepPath::root(), &mut calls);
            for (_, id) in calls {
                let mut longer = chain.clone();
                longer.push(id);
                pending.push(longer);
            }
        }
    }
    errors
}

/// The type of the part of a `ty` value a pointer addresses: the value
/// itself for an empty pointer, else whatever it holds there.
fn part_ty(ty: Ty, pointer: &str) -> Ty {
    if pointer.is_empty() { ty } else { Ty::Any }
}

/// The variable a [`Filter`](Expr::Filter)'s test reads each element as, so a
/// filter reads like a loop body.
pub(crate) const FILTER_ITEM: &str = "item";

impl Ty {
    /// A zero of this type, for reading a unary operation as a binary one.
    fn zero() -> Self {
        Self::Integer
    }

    /// The JSON Schema describing this type, as far as the checker knows it.
    /// Used to describe what an [`Object`](Expr::Object) or a
    /// [`Map`](Expr::Map) produces, so a step after it can reach into it.
    fn schema(&self) -> Value {
        let typed = |name: &str| serde_json::json!({"type": name});
        match self {
            Self::Any => serde_json::json!({}),
            Self::Null => typed("null"),
            Self::Text | Self::Secret => typed("string"),
            Self::Integer => typed("integer"),
            Self::Number => typed("number"),
            Self::Boolean => typed("boolean"),
            Self::DateTime => serde_json::json!({"type": "string", "format": "date-time"}),
            Self::Resource(_) => serde_json::json!({"type": "string", "format": "resource"}),
            Self::Object(Some(schema)) => schema.clone(),
            Self::Object(None) => typed("object"),
            Self::Array(Some(items)) => serde_json::json!({"type": "array", "items": items}),
            Self::Array(None) => typed("array"),
        }
    }

    /// Whether a value of this type could be a number, or might be one.
    fn numeric(&self) -> bool {
        matches!(self, Self::Integer | Self::Number | Self::Any)
    }

    /// Whether a value of this type could be text, or might be one.
    fn textual(&self) -> bool {
        matches!(self, Self::Text | Self::Secret | Self::Any)
    }

    /// Whether a value of this type could be a whole number, or might be.
    fn whole(&self) -> bool {
        matches!(self, Self::Integer | Self::Any)
    }
}

/// Whether `ty` may stand as a truth value: a boolean, a comparison, or a
/// value judged by its truthiness (`Any`, whose run-time value decides).
fn truth_value(ty: &Ty) -> Result<(), String> {
    match ty {
        Ty::Boolean | Ty::Any => Ok(()),
        other => Err(format!(
            "a condition needs a yes-or-no answer, and this is {other}"
        )),
    }
}

/// Whether `op` may compare a value of type `left` with one of type `right`.
///
/// Equality holds between any two values of the same kind. Order holds
/// between numbers, between texts, and between dates — a date is written as
/// text, so the two meet as text at run time — and never across them.
fn comparable(op: &Op, left: &Ty, right: &Ty) -> Result<(), String> {
    if !op.is_comparison() {
        return Err(format!("`{}` is not a comparison", op.as_str()));
    }
    if *op == Op::Equals || *op == Op::NotEquals {
        // A null is what an optional value has when it was not given, so it
        // compares with anything.
        if matches!(left, Ty::Null) || matches!(right, Ty::Null) {
            return Ok(());
        }
        let overlapping_types = left.numeric() && right.numeric()
            || left.textual() && right.textual()
            || matches!(left, Ty::Boolean) && matches!(right, Ty::Boolean)
            || matches!(left, Ty::DateTime) && matches!(right, Ty::DateTime | Ty::Text)
            || matches!(left, Ty::Text) && matches!(right, Ty::DateTime)
            || both_lists_or_objects(left, right);
        return if overlapping_types {
            Ok(())
        } else {
            Err("they are values of different kinds".into())
        };
    }
    let ordered = (left.numeric() && right.numeric())
        || (left.textual() && right.textual())
        || (matches!(left, Ty::DateTime | Ty::Text) && matches!(right, Ty::DateTime | Ty::Text));
    if ordered {
        Ok(())
    } else {
        Err("only numbers with numbers, or texts with texts, have an order".into())
    }
}

fn both_lists_or_objects(left: &Ty, right: &Ty) -> bool {
    let same = |a: &Ty, b: &Ty| {
        matches!(
            (a, b),
            (Ty::Array(_), Ty::Array(_)) | (Ty::Object(_), Ty::Object(_))
        )
    };
    same(left, right) || (matches!(left, Ty::Any) || matches!(right, Ty::Any))
}

/// Whether `op` may combine a value of type `left` with one of type `right`.
fn arithmetic_operands(op: Op, left: &Ty, right: &Ty) -> Result<(), String> {
    match op {
        Op::Concat => {
            if left.textual() && right.textual() {
                Ok(())
            } else {
                Err("only texts join into text".into())
            }
        }
        Op::Add | Op::Subtract | Op::Multiply | Op::Divide | Op::Modulo => {
            if left.numeric() && right.numeric() {
                Ok(())
            } else {
                Err("arithmetic needs numbers".into())
            }
        }
        other => Err(format!("`{}` is not arithmetic", other.as_str())),
    }
}

fn countable(ty: &Ty) -> Result<(), String> {
    match ty {
        Ty::Text | Ty::Array(_) | Ty::Object(_) | Ty::Secret | Ty::Any => Ok(()),
        other => Err(format!(
            "only a text, a list or a dictionary has a length, and this is {other}"
        )),
    }
}

fn textual(ty: &Ty) -> Result<(), String> {
    if ty.textual() {
        Ok(())
    } else {
        Err(format!("this needs text, and it is given {ty}"))
    }
}

fn whole_number(ty: &Ty) -> Result<(), String> {
    if ty.whole() {
        Ok(())
    } else {
        Err(format!("this needs a whole number, and it is given {ty}"))
    }
}

/// The type of the part of `ty` that `field` names.
///
/// `Err` when the type has no such part. `Ok(Ty::Any)` when it may have one
/// that the checker cannot type (an `Any`, or a dictionary whose members are
/// not all declared).
fn field_ty(ty: &Ty, field: &str) -> Result<Ty, String> {
    let mut current = ty.clone();
    for token in field_tokens(field) {
        current = match (&current, token) {
            (Ty::Any, _) => return Ok(Ty::Any),
            (Ty::Object(Some(schema)), token) => {
                match crate::types::schema_at(schema, &format!("/{token}")) {
                    Ok(Some(property)) => Ty::of_schema(&property),
                    Ok(None) => return Ok(Ty::Any),
                    Err(reason) => return Err(format!("`{field}`: {reason}")),
                }
            }
            (Ty::Object(None), _) => return Ok(Ty::Any),
            (Ty::Array(items), token) => {
                if token.parse::<usize>().is_err() {
                    return Err(format!("a list has positions, not a field `{token}`"));
                }
                items.as_ref().map_or(Ty::Any, |items| Ty::of_schema(items))
            }
            (other, _) => {
                return Err(format!("{other} has no field `{token}`"));
            }
        };
    }
    Ok(current)
}
