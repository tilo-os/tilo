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
use tilo_actions::door::ActionDescriptor;
use tilo_actions::params::ActionParam;

use crate::catalog::{ActionCatalog, ResourceTarget};
use crate::grant::grant_permits;
use crate::path::StepPath;
use crate::shortcut::{
    AiTransform, Binding, Condition, Selector, Shortcut, Step, StepKind, UnattendedConsent,
};
use crate::template::{self, Piece, is_identifier};
use crate::types::{Ty, assignable, literal_fits, pointer_tokens, schema_at};
use crate::{
    AGENT_APP_ID, AGENT_TRANSFORM_KIND, SHORTCUT_RUN_ACTION, SHORTCUTS_APP_ID, shortcut_id_of,
};

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
    #[error("the shortcut's grant does not cover `{action}` on `{app_id}`")]
    OutsideGrant { app_id: String, action: String },
    #[error("a shortcut run must name the shortcut exactly, so a cycle can be ruled out")]
    UnresolvableShortcut,
    #[error("the shortcut would run itself: {}", chain.join(" → "))]
    Recursion { chain: Vec<String> },
    #[error("consent to `{action}` on `{resource}` of `{app_id}` matches no step")]
    UnusedConsent {
        app_id: String,
        resource: String,
        action: String,
    },
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

/// Check `shortcut` against the descriptors in `catalog`.
///
/// # Errors
///
/// Returns every problem found, each with the step path it concerns.
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
    checker.errors.extend(unused_consents(shortcut));
    checker.errors.extend(recursion(shortcut, resolver));
    if checker.errors.is_empty() {
        Ok(())
    } else {
        Err(checker.errors)
    }
}

/// Consents no step could use: each must name the application and action of
/// an invocation, and its resource when that invocation names one exactly.
fn unused_consents(shortcut: &Shortcut) -> Vec<CheckError> {
    fn reaches(steps: &[Step], consent: &UnattendedConsent) -> bool {
        steps.iter().any(|step| match &step.kind {
            StepKind::Invoke {
                app_id,
                resource,
                action,
                ..
            } => {
                *app_id == consent.app_id
                    && *action == consent.action
                    && match resource {
                        Selector::Fixed(id) => *id == consent.resource,
                        Selector::FromBinding(_) | Selector::FirstOfKind(_) => true,
                    }
            }
            StepKind::If {
                then, otherwise, ..
            } => reaches(then, consent) || reaches(otherwise, consent),
            StepKind::ForEach { body, .. } => reaches(body, consent),
            StepKind::SetVar { .. } | StepKind::Ask { .. } | StepKind::Ai { .. } => false,
        })
    }
    shortcut
        .consents
        .iter()
        .filter(|consent| !reaches(&shortcut.steps, consent))
        .map(|consent| CheckError {
            path: StepPath::root(),
            kind: CheckErrorKind::UnusedConsent {
                app_id: consent.app_id.clone(),
                resource: consent.resource.clone(),
                action: consent.action.clone(),
            },
        })
        .collect()
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
                app_id,
                resource,
                action,
                args,
            } => self.invoke(path, scope, app_id, resource, action, args),
            StepKind::Ai { transform, input } => Some(self.ai(path, scope, transform, input)),
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

    fn ai(
        &mut self,
        path: &StepPath,
        scope: &Scope,
        transform: &AiTransform,
        input: &Binding,
    ) -> (Ty, Option<Value>) {
        self.invoke(
            path,
            scope,
            AGENT_APP_ID,
            &Selector::FirstOfKind(AGENT_TRANSFORM_KIND.into()),
            transform.action(),
            &transform.arguments(input),
        );
        match transform {
            AiTransform::Extract { schema } => (Ty::of_schema(schema), Some(schema.clone())),
            AiTransform::Summarise
            | AiTransform::Classify { .. }
            | AiTransform::Freeform { .. } => (Ty::Text, None),
        }
    }

    fn invoke(
        &mut self,
        path: &StepPath,
        scope: &Scope,
        app_id: &str,
        selector: &Selector,
        action: &str,
        args: &BTreeMap<String, Binding>,
    ) -> Output {
        let unknown = Some((Ty::Any, None));
        let target = self.selector(selector, scope, path);
        if app_id == SHORTCUTS_APP_ID
            && action == SHORTCUT_RUN_ACTION
            && !matches!(target, Some(Target::Id(_)))
        {
            self.fail(path, CheckErrorKind::UnresolvableShortcut);
        }
        let target = target.unwrap_or(Target::Any);
        let row = self.catalog.find(app_id, target.as_ref(), action);
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
        if !grant_permits(
            &self.shortcut.grant,
            app_id,
            &descriptor.address,
            &descriptor.resource_kind,
            descriptor.contract.permission,
        ) {
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
                let found = self.inputs.get(name).map(|param| Ty::of_param(param));
                if found.is_none() {
                    self.fail(path, CheckErrorKind::UnknownInput(name.clone()));
                }
                found
            }
            Binding::Var(name) => {
                let found = scope.vars.get(name).cloned();
                if found.is_none() {
                    self.fail(path, CheckErrorKind::UnknownVariable(name.clone()));
                }
                found
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

/// Every step of `steps` that runs a shortcut named exactly, with the id.
fn shortcut_calls(steps: &[Step], path: &StepPath, out: &mut Vec<(StepPath, String)>) {
    for (index, step) in steps.iter().enumerate() {
        let path = path.child(index);
        match &step.kind {
            StepKind::Invoke {
                app_id,
                resource: Selector::Fixed(resource),
                action,
                ..
            } if app_id == SHORTCUTS_APP_ID && action == SHORTCUT_RUN_ACTION => {
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
            StepKind::Invoke { .. }
            | StepKind::SetVar { .. }
            | StepKind::Ask { .. }
            | StepKind::Ai { .. } => {}
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
