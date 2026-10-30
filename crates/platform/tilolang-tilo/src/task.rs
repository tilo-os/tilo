//! An agent task's programs (`docs/designs/app-language/agents.md`, "How a program runs").
//!
//! A task's programs run in a tilolang host of their own, whose code author is the task. The
//! host holds the task's session (the `let`s its programs bound) and no records: it compiles each
//! program against every installed app's package and runs it over a [`TaskStore`], which reads
//! through the task's door, and sends every change the program makes back through the door as an
//! invocation of the action's own app ([`tilolang_runtime::delegate`]), decided there by the
//! workspace's one policy as the task's. Nothing here decides anything or writes a record.
//!
//! The door ([`TaskDoor`]) is the task's connection: in the session the program connection to
//! the agent door (`crates/platform/tilolang-host/src/task.rs`), whose caller is the task.
//!
//! **Reads** go to the door ([`TaskDoor::read`], [`TaskDoor::query`]). In the session the task's
//! host answers them itself from each app's record as the door hands it over, read with the
//! app's package ([`crate::task_records::RecordReads`]): no app runs for them. One resource is
//! read once per program.
//!
//! **Steps.** One program is one step. Run without `confirm`, it first runs provisionally: its
//! invocations are listed, not sent, and nothing it bound is kept. One that changed nothing has
//! answered; one that would change many resources or destroy something answers with that list as
//! its preview; any other runs again for real at once. With `confirm` it runs for real straight
//! away.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::flow::Journal;
use tilolang_runtime::ids::{DocId, RunId, TypeId};
use tilolang_runtime::meta::{Meta, parse_ty};
use tilolang_runtime::program::{ProgramOutcome, Session};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::store::{
    Applied, ChangeNote, ChangeSummary, FieldSpec, FieldValue, Mutation, NewRecord, OutboxEntry,
    Scalar, Store, StoreError, SubId, Txn,
};
use tilolang_runtime::value::{ResourceRef, Value, from_json};
use tilolang_runtime::{ActionInfo, Authority, Decision, ModuleHash, Principal, Runtime};

use crate::bundle::Bundle;
use crate::host::{Host, argument_json};
use crate::meta::MetaExt as _;
use crate::publish::{package_resource_id, resource_id};

/// More resources than this changed by one program are many: it answers with a preview first.
pub const MANY_RESOURCES: usize = 3;

/// What a task's host asks of its door. Each call waits for the door's answer.
pub trait TaskDoor {
    /// The fields of the resource `item` (its id in its record) of the app `app`, as
    /// [`Host::read_values`] answers them.
    ///
    /// # Errors
    /// Why it cannot be read: the task may not, the app does not run, the resource is gone.
    fn read(&self, app: &str, item: &str) -> Result<Json, String>;

    /// Invoke `call` (`{app, resource, action, arguments}`) as the task.
    fn invoke(&self, call: &Json) -> Answered;

    /// What the app `app` answers to a program's `request` to read it (`grep`, `find`, `read`,
    /// `describe`: a [`tilolang_runtime::reads::ReadRequest`]), read as the task.
    ///
    /// # Errors
    /// Why it cannot be read.
    fn query(&self, app: &str, request: &Json) -> Result<Json, String>;

    /// A program starts: what was read for an earlier one is read again
    /// ([`crate::task_records::RecordReads::forget`]).
    fn begin_program(&self) {}
}

/// How an invocation the door made ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Answered {
    /// It ran: its output, `{value, resources}`.
    Done(Json),
    /// It ran and failed.
    Failed(String),
    /// The policy refused it.
    Refused(String),
    /// It waits on the person's answer.
    Asks(String),
}

/// The step a program made: what it invoked, or in a provisional run would have.
#[derive(Clone, Debug, Default)]
struct Steps {
    /// Invocations not sent: the program runs provisionally.
    provisional: bool,
    /// What it invoked, in order, as a preview lists it.
    planned: Vec<Json>,
    /// How many of them ran.
    done: usize,
    /// The apps whose resources it changed, in order.
    apps: Vec<String>,
}

/// The task's session: the runtime with every installed app's package, and its `let`s.
pub struct TaskSession {
    runtime: Runtime,
    task: Principal,
    /// The person the task acts for: `me`.
    person: Option<Principal>,
    session: RefCell<Session>,
    steps: Rc<RefCell<Steps>>,
    reads: Rc<RefCell<Reads>>,
    /// Where the task reaches the workspace.
    door: Rc<dyn TaskDoor>,
}

/// What a runtime of a task decides itself: nothing. Every change it makes goes to its
/// invoker; anything that would run here is refused.
struct DecidesNothing;

impl Authority for DecidesNothing {
    fn decide(&self, _: &Principal, _: &[ModuleHash], _: &ActionInfo) -> Decision {
        Decision::Deny
    }
}

impl TaskSession {
    /// The session of `task` (the agent run's id) reaching the workspace through `door`, with
    /// the packages `bundles` (every installed app's, each after the packages it uses).
    ///
    /// # Errors
    /// The runtime cannot start or a package does not load.
    pub fn new(
        task: &str,
        person: Option<&str>,
        door: &Rc<dyn TaskDoor>,
        bundles: &[Bundle],
    ) -> Result<Self, String> {
        let reads = Rc::new(RefCell::new(Reads::default()));
        let store = TaskStore {
            door: Rc::clone(door),
            reads: Rc::clone(&reads),
            journals: HashMap::new(),
            fresh: 0,
        };
        let runtime = Runtime::new(store, DecidesNothing).map_err(|error| error.to_string())?;
        let mut apps: HashMap<String, Meta> = HashMap::new();
        for bundle in bundles {
            for import in &bundle.imports {
                runtime
                    .reload_package(&import.wasm, import.meta.clone())
                    .map_err(|error| error.to_string())?;
                import.meta.register_schemas();
                apps.insert(import.meta.app.clone(), import.meta.clone());
            }
            runtime
                .reload_package(&bundle.wasm, bundle.meta.clone())
                .map_err(|error| error.to_string())?;
            bundle.meta.register_schemas();
            apps.insert(bundle.meta.app.clone(), bundle.meta.clone());
        }
        reads.borrow_mut().apps = apps.keys().cloned().collect();
        let steps = Rc::new(RefCell::new(Steps::default()));
        runtime.set_invoker(DoorInvoker {
            door: Rc::clone(door),
            apps,
            steps: Rc::clone(&steps),
        });
        let asked = Rc::clone(door);
        runtime.set_reader(move |request| {
            let encoded = serde_json::to_value(request).map_err(|error| error.to_string())?;
            asked.query(request.app(), &encoded)
        });
        runtime.set_clock(crate::host::system_time);
        Ok(Self {
            runtime,
            task: Principal::new(format!("agent:{task}")),
            person: person.map(Principal::new),
            session: RefCell::new(Session::new()),
            steps,
            reads,
            door: Rc::clone(door),
        })
    }

    /// Every installed app the task's programs may name, by the name they name it with: its
    /// id and its declarations (`{alias: {app, signatures: [{name, signature}]}}`), read from
    /// the packages alone, whether or not any app runs.
    #[must_use]
    pub fn catalogue(&self) -> Json {
        let mut apps = serde_json::Map::new();
        for signature in self.runtime.catalogue() {
            if signature.app == tilolang::HOST_APP {
                continue;
            }
            let alias = tilolang::resolve::default_alias(&signature.app);
            let app = apps
                .entry(alias)
                .or_insert_with(|| json!({"app": signature.app, "signatures": []}));
            if let Some(signatures) = app["signatures"].as_array_mut() {
                signatures.push(json!({"name": signature.name, "signature": signature.render()}));
            }
        }
        Json::Object(apps)
    }

    /// Read the time from `clock` (ISO-8601 UTC), for `now` and the times filters write.
    pub fn set_clock(&self, clock: impl Fn() -> String + 'static) {
        self.runtime.set_clock(clock);
    }

    /// Run `source` as one step of the task; see the module docs for `confirm`. The answer:
    /// `{state, output, detail, lets, steps, changed, apps}`, `state` one of `done`, `preview`,
    /// `suspended`, `rejected`, `failed`.
    pub fn run(&self, source: &str, confirm: bool) -> Json {
        if !confirm {
            let saved = self.session.borrow().clone();
            let (answer, steps) = self.run_once(source, true);
            if steps.planned.is_empty() {
                // It changed nothing: it has answered.
                return answer;
            }
            *self.session.borrow_mut() = saved;
            let ran = answer.get("state") == Some(&json!("done"));
            if !ran || wants_preview(&steps.planned) {
                // A provisional run that stopped before its end stopped on what an invocation
                // it did not send would have answered: what follows is not known, so what is
                // known is shown first.
                let mut preview = answer;
                if let Some(fields) = preview.as_object_mut() {
                    fields.insert("state".to_owned(), json!("preview"));
                    fields.insert("lets".to_owned(), json!([]));
                    fields.insert("complete".to_owned(), json!(ran));
                }
                return preview;
            }
        }
        self.run_once(source, false).0
    }

    fn run_once(&self, source: &str, provisional: bool) -> (Json, Steps) {
        *self.steps.borrow_mut() = Steps {
            provisional,
            ..Steps::default()
        };
        self.reads.borrow_mut().known.clear();
        self.door.begin_program();
        let inputs = tilolang_runtime::reads::ProgramInputs {
            me: self.person.clone(),
            ..Default::default()
        };
        let reply = self
            .session
            .borrow_mut()
            .run_with(&self.runtime, &self.task, source, inputs);
        let steps = std::mem::take(&mut *self.steps.borrow_mut());
        let lets: Vec<Json> = self
            .session
            .borrow()
            .bindings()
            .iter()
            .map(|binding| json!({"name": binding.name, "type": binding.ty}))
            .collect();
        let (state, detail) = match reply.outcome {
            ProgramOutcome::Done => ("done", Json::Null),
            ProgramOutcome::Rejected(lines) => ("rejected", json!(lines)),
            ProgramOutcome::Suspended(suggestion) => {
                ("suspended", json!({"action": suggestion.action}))
            }
            ProgramOutcome::Failed(reason) => ("failed", json!(reason)),
            // Every read is answered while the program runs; one left over is a fault.
            ProgramOutcome::NeedsReads(asked) => (
                "failed",
                json!(format!("{} reads were left unanswered", asked.len())),
            ),
        };
        let answer = json!({
            "kind": "program",
            "state": state,
            "output": reply.output,
            "detail": detail,
            "lets": lets,
            "steps": steps.planned,
            "changed": steps.done,
            "apps": steps.apps,
            // Where each app it changed takes a step back: the app's own resource, by its id,
            // which the history verbs are published on.
            "history": steps
                .apps
                .iter()
                .map(|app| json!({"app": app, "resource": package_resource_id(app)}))
                .collect::<Vec<_>>(),
        });
        (answer, steps)
    }
}

/// Whether a program whose invocations are `planned` is previewed before it runs: it changes
/// many resources (it names many, or makes many changes, each of which may make one), or
/// destroys something.
#[must_use]
pub fn wants_preview(planned: &[Json]) -> bool {
    let mut resources: Vec<&str> = planned
        .iter()
        .flat_map(|step| step["resources"].as_array().into_iter().flatten())
        .filter_map(Json::as_str)
        .collect();
    resources.sort_unstable();
    resources.dedup();
    planned.len() > MANY_RESOURCES
        || resources.len() > MANY_RESOURCES
        || planned.iter().any(|step| step["destructive"] == true)
}

/// Sends a program's invocations through the door, or in a provisional run lists them.
struct DoorInvoker {
    door: Rc<dyn TaskDoor>,
    apps: HashMap<String, Meta>,
    steps: Rc<RefCell<Steps>>,
}

impl Invoker for DoorInvoker {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let app = invocation.app();
        let Some(meta) = self.apps.get(app) else {
            return Invoked::Refused(format!("this task may use no app {app}"));
        };
        let call = match call_of(&self.apps, meta, invocation) {
            Ok(call) => call,
            Err(why) => return Invoked::Failed(why),
        };
        let resources: Vec<String> = invocation
            .resources()
            .iter()
            .map(|r| resource_id(app, &r.id_string()))
            .collect();
        let provisional = {
            let mut steps = self.steps.borrow_mut();
            steps.planned.push(json!({
                "app": app,
                "action": invocation.name(),
                "resources": resources,
                "destructive": invocation.destroys(),
            }));
            steps.provisional
        };
        if provisional {
            return Invoked::Done(Value::Unit);
        }
        match self.door.invoke(&call) {
            Answered::Done(output) => {
                let mut steps = self.steps.borrow_mut();
                steps.done = steps.done.saturating_add(1);
                if !steps.apps.iter().any(|known| known == app) {
                    steps.apps.push(app.to_owned());
                }
                drop(steps);
                match result_of(meta, invocation, &output) {
                    Ok(result) => Invoked::Done(result),
                    // It ran, and the program cannot go on with what it answered: said so,
                    // rather than going on with nothing in its place.
                    Err(why) => Invoked::Failed(why),
                }
            }
            Answered::Failed(why) => Invoked::Failed(why),
            Answered::Refused(why) => Invoked::Refused(why),
            Answered::Asks(why) => Invoked::Asks(why),
        }
    }
}

/// The door's call for `invocation` of the package `meta`: a declared action on its subject
/// (its first parameter, when it is one of the package's types) or on the package, its other
/// arguments by name; a field edit on its resource.
fn call_of(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    invocation: &Invocation,
) -> Result<Json, String> {
    match invocation {
        Invocation::Action {
            app, action, args, ..
        } => {
            let declared = meta
                .actions
                .iter()
                .find(|declared| declared.id == *action)
                .ok_or_else(|| format!("{app} declares no action {action}"))?;
            let subject = meta.subject_of(&declared.params).and_then(|_| args.first());
            let resource = match subject {
                Some((_, Value::Ref(r))) => resource_id(app, &r.target.id_string()),
                _ => package_resource_id(app),
            };
            let mut arguments = serde_json::Map::new();
            for (name, value) in args.iter().skip(usize::from(subject.is_some())) {
                let param = declared
                    .params
                    .iter()
                    .find(|param| param.name == *name)
                    .ok_or_else(|| format!("{action} takes no argument `{name}`"))?;
                let ty = parse_ty(&param.ty, app)?;
                let owner = |other: &str| {
                    apps.get(other)
                        .cloned()
                        .ok_or_else(|| format!("this task may use no app {other}"))
                };
                arguments.insert(name.clone(), argument_json(&owner, meta, value, &ty)?);
            }
            Ok(json!({
                "app": app,
                "resource": resource,
                "action": action,
                "arguments": arguments,
            }))
        }
        Invocation::Edit {
            app,
            target,
            field,
            field_id,
            value,
        } => {
            let (action, value) = match value {
                Value::Text(text) => (crate::action::EDIT_TEXT_ACTION, json!(text)),
                Value::Bool(flag) => (crate::action::EDIT_FLAG_ACTION, json!(flag)),
                _ => {
                    return Err(format!(
                        "a program sets text and flag fields; `{field}` changes through {}'s actions",
                        app.rsplit('.').next().unwrap_or(app)
                    ));
                }
            };
            Ok(json!({
                "app": app,
                "resource": resource_id(app, &target.id_string()),
                "action": action,
                "arguments": {"field": field_id, "value": value},
            }))
        }
    }
}

/// What an invocation of `meta`'s action answered (`{value, resources}`), as the program gets
/// it: a record result as the resource the app says it returned, any other as its value.
fn result_of(meta: &Meta, invocation: &Invocation, output: &Json) -> Result<Value, String> {
    let Invocation::Action { action, .. } = invocation else {
        return Ok(Value::Unit);
    };
    let Some(result) = meta
        .actions
        .iter()
        .find(|declared| declared.id == *action)
        .and_then(|declared| declared.result.clone())
    else {
        return Ok(Value::Unit);
    };
    if let Some(declared) = meta.type_named(&result) {
        let prefix = resource_id(&meta.app, "");
        return output["resources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .find_map(|id| ResourceRef::parse_id(id.strip_prefix(&prefix)?, declared.id.clone()))
            .map(|at| Value::resource(&at))
            .ok_or_else(|| {
                format!(
                    "the action ran but did not answer the {} it returns: {output}",
                    declared.name
                )
            });
    }
    match output.get("value") {
        None | Some(Json::Null) => Ok(Value::Unit),
        Some(value) => from_json(value)
            .map_err(|error| format!("the action ran but its answer does not decode: {error}")),
    }
}

/// What the door read of resources, per program.
#[derive(Default)]
struct Reads {
    /// Every app the task's session holds a package of.
    apps: Vec<String>,
    /// Each resource read so far, by its id: its type and fields.
    known: HashMap<String, Rc<Read>>,
}

/// One resource as its app read it.
#[derive(Debug)]
struct Read {
    ty: TypeId,
    fields: HashMap<String, FieldValue>,
}

/// The store of a task's runtime: reads through the door, writes nothing.
struct TaskStore {
    door: Rc<dyn TaskDoor>,
    reads: Rc<RefCell<Reads>>,
    /// The journals of the task's program runs: the task's own, kept in memory.
    journals: HashMap<RunId, Journal>,
    fresh: u64,
}

const WRITES_NOTHING: &str =
    "a program changes nothing directly: it invokes its apps' actions and field edits";

impl TaskStore {
    /// The resource `at` as its app reads it, read once per program.
    fn resource(&self, at: &ResourceRef) -> Result<Rc<Read>, StoreError> {
        let id = at.id_string();
        if let Some(known) = self.reads.borrow().known.get(&id) {
            return Ok(Rc::clone(known));
        }
        let app = self.app_of(at)?;
        // A read that did not reach the app fails the program; one the app answered that it
        // holds no such resource is a resource that is not there.
        let answer = self.door.read(&app, &id).map_err(StoreError::Backend)?;
        if answer.get("missing").is_some() {
            return Err(StoreError::NotFound(format!("{app} holds no {id}")));
        }
        let read = Rc::new(read_of(&answer).map_err(StoreError::Backend)?);
        self.reads.borrow_mut().known.insert(id, Rc::clone(&read));
        Ok(read)
    }

    /// The app that holds `at`: the one whose package declares its type, else the one its
    /// root's id names (`root-<type id>`).
    fn app_of(&self, at: &ResourceRef) -> Result<String, StoreError> {
        let declared = |ty: &str| crate::schema::lookup(ty).map(|schema| schema.app.clone());
        let ty = if at.ty.is_empty() {
            at.doc.strip_prefix("root-").unwrap_or_default().to_owned()
        } else {
            at.ty.clone()
        };
        declared(&ty)
            .filter(|app| self.reads.borrow().apps.contains(app))
            .ok_or_else(|| {
                StoreError::NotFound(format!(
                    "no installed app holds {}; the task may read the apps it lists",
                    at.id_string()
                ))
            })
    }
}

impl Store for TaskStore {
    fn fresh_id(&mut self, prefix: &str) -> String {
        self.fresh = self.fresh.saturating_add(1);
        format!("{prefix}task-{}", self.fresh)
    }

    fn create(&mut self, _: &NewRecord) -> Result<ResourceRef, StoreError> {
        Err(StoreError::Invalid(WRITES_NOTHING.into()))
    }

    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError> {
        Ok(self.resource(at)?.ty.clone())
    }

    fn container(&self, at: &ResourceRef) -> Result<Option<(ResourceRef, String)>, StoreError> {
        Err(StoreError::Invalid(format!(
            "a task reads what holds {} from the resource that holds it",
            at.id_string()
        )))
    }

    fn begin(&mut self) -> Result<Txn, StoreError> {
        Err(StoreError::Invalid(WRITES_NOTHING.into()))
    }

    fn apply(&mut self, _: Txn, _: &ResourceRef, _: &Mutation) -> Result<Applied, StoreError> {
        Err(StoreError::Invalid(WRITES_NOTHING.into()))
    }

    fn commit(&mut self, _: Txn, _: &ChangeNote) -> Result<(), StoreError> {
        Err(StoreError::Invalid(WRITES_NOTHING.into()))
    }

    fn rollback(&mut self, _: Txn) {}

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        Ok(self
            .resource(at)?
            .fields
            .get(&field.id)
            .cloned()
            .unwrap_or(FieldValue::Absent))
    }

    fn select(
        &self,
        ty: &TypeId,
        pred: &tilolang_runtime::store::Pred,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Vec<ResourceRef>, StoreError> {
        let app = self.app_of(&ResourceRef {
            doc: String::new(),
            item: None,
            ty: ty.clone(),
        })?;
        let request = tilolang_runtime::reads::ReadRequest::Select {
            app: app.clone(),
            ty: ty.clone(),
            pred: pred.clone(),
            order: order.cloned(),
        };
        let encoded = serde_json::to_value(&request)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let found = self
            .door
            .query(&app, &encoded)
            .map_err(StoreError::Backend)?;
        Ok(found
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .filter_map(|id| ResourceRef::parse_id(id, ty.clone()))
            .collect())
    }

    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError> {
        Err(StoreError::Invalid(format!(
            "a task reads no history of {doc} yet"
        )))
    }

    fn subscribe(&mut self, _: &DocId, _: Box<dyn FnMut(&ChangeSummary)>) -> SubId {
        SubId(0)
    }

    fn unsubscribe(&mut self, _: SubId) {}

    fn outbox_pending(&self) -> Vec<OutboxEntry> {
        Vec::new()
    }

    fn outbox_mark(
        &mut self,
        _: &OutboxEntry,
        _: &tilolang_runtime::store::OutboxState,
        _: &ChangeNote,
    ) -> Result<(), StoreError> {
        Err(StoreError::Invalid(WRITES_NOTHING.into()))
    }

    fn outbox_settle(
        &mut self,
        _: &OutboxEntry,
        _: &tilolang_runtime::store::Settled,
        _: &ChangeNote,
    ) -> Result<(), StoreError> {
        Err(StoreError::Invalid(WRITES_NOTHING.into()))
    }

    fn outbox_states(&self) -> Vec<(DocId, tilolang_runtime::store::DeliveryState)> {
        Vec::new()
    }

    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError> {
        self.journals.insert(run.clone(), journal.clone());
        Ok(())
    }

    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError> {
        Ok(self.journals.get(run).cloned())
    }
}

// ---- the read, on both ends -----------------------------------------------------------------

/// A field as `read-values` carries it.
fn field_json(value: &FieldValue) -> Json {
    match value {
        FieldValue::Absent => json!({"absent": true}),
        FieldValue::Scalar(Scalar::Bool(flag)) => json!({"bool": flag}),
        FieldValue::Scalar(Scalar::Int(int)) => json!({"int": int}),
        FieldValue::Scalar(Scalar::Float(float)) => json!({"float": float}),
        FieldValue::Scalar(Scalar::Text(text)) => json!({"str": text}),
        FieldValue::Scalar(Scalar::Json(value)) => json!({"json": value}),
        FieldValue::Counter(count) => json!({"counter": count}),
        FieldValue::Text(text) => json!({"text": text}),
        FieldValue::Rich(rich) => json!({"rich": rich}),
        FieldValue::Items(items) => json!({"items": items}),
        FieldValue::Keys(keys) => json!({"keys": keys}),
        FieldValue::Entries(entries) => json!({
            "entries": entries
                .iter()
                .map(|(key, value)| json!([key, field_json(&FieldValue::Scalar(value.clone()))]))
                .collect::<Vec<_>>()
        }),
    }
}

fn field_of(json: &Json) -> Result<FieldValue, String> {
    let bad = || format!("a field read as {json} is no field");
    let (tag, value) = json
        .as_object()
        .and_then(|fields| fields.iter().next())
        .ok_or_else(bad)?;
    Ok(match tag.as_str() {
        "absent" => FieldValue::Absent,
        "bool" => FieldValue::Scalar(Scalar::Bool(value.as_bool().ok_or_else(bad)?)),
        "int" => FieldValue::Scalar(Scalar::Int(value.as_i64().ok_or_else(bad)?)),
        "float" => FieldValue::Scalar(Scalar::Float(value.as_f64().ok_or_else(bad)?)),
        "str" => FieldValue::Scalar(Scalar::Text(value.as_str().ok_or_else(bad)?.to_owned())),
        "json" => FieldValue::Scalar(Scalar::Json(value.clone())),
        "counter" => FieldValue::Counter(value.as_i64().ok_or_else(bad)?),
        "text" => FieldValue::Text(value.as_str().ok_or_else(bad)?.to_owned()),
        "rich" => {
            FieldValue::Rich(serde_json::from_value::<RichValue>(value.clone()).map_err(|_| bad())?)
        }
        "items" | "keys" => {
            let texts = value
                .as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|item| item.as_str().map(str::to_owned).ok_or_else(bad))
                .collect::<Result<Vec<_>, _>>()?;
            if tag == "items" {
                FieldValue::Items(texts)
            } else {
                FieldValue::Keys(texts)
            }
        }
        "entries" => FieldValue::Entries(
            value
                .as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|entry| {
                    let key = entry[0].as_str().ok_or_else(bad)?.to_owned();
                    match field_of(&entry[1])? {
                        FieldValue::Scalar(scalar) => Ok((key, scalar)),
                        _ => Err(bad()),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        _ => return Err(bad()),
    })
}

/// A resource as `read-values` answered it.
fn read_of(answer: &Json) -> Result<Read, String> {
    let ty = answer["type"]
        .as_str()
        .ok_or("a read names the resource's type")?
        .to_owned();
    let fields = answer["fields"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, value)| Ok((id.clone(), field_of(value)?)))
        .collect::<Result<_, String>>()?;
    Ok(Read { ty, fields })
}

impl Host {
    /// The resource `item` (its id in its record), as a task's host reads it: its type and
    /// every field as stored, by field id, or `{"missing": true}` when the room holds no such
    /// resource. The app's half of a task's read; what the task may read the door decided
    /// before it asked.
    ///
    /// # Errors
    /// The item names nothing in this room.
    pub fn read_values(&self, item: &str) -> Result<Json, String> {
        let unknown = || format!("no resource `{item}` in this room");
        let probe = ResourceRef::parse_id(item, String::new()).ok_or_else(unknown)?;
        let Ok(ty) = self.store().type_of(&probe) else {
            // Not there (a root nobody made yet reads as empty): said, not failed.
            return Ok(json!({"missing": true}));
        };
        let at = ResourceRef { ty, ..probe };
        let specs = self.runtime().field_specs(&at.ty).ok_or_else(unknown)?;
        let mut fields = serde_json::Map::new();
        for spec in specs {
            let value = self
                .store()
                .read(&at, &spec)
                .map_err(|error| error.to_string())?;
            fields.insert(spec.id.clone(), field_json(&value));
        }
        Ok(json!({"type": at.ty, "fields": fields}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stored_field_reads_back_as_it_was() {
        let values = [
            FieldValue::Absent,
            FieldValue::Scalar(Scalar::Bool(true)),
            FieldValue::Scalar(Scalar::Int(-3)),
            FieldValue::Scalar(Scalar::Float(1.5)),
            FieldValue::Scalar(Scalar::Text("a".into())),
            FieldValue::Scalar(Scalar::Json(json!({"case": "x"}))),
            FieldValue::Counter(7),
            FieldValue::Text("plain".into()),
            FieldValue::Rich(RichValue::from_text("one\ntwo")),
            FieldValue::Items(vec!["i1".into(), "i2".into()]),
            FieldValue::Keys(vec!["k".into()]),
            FieldValue::Entries(vec![("k".into(), Scalar::Int(1))]),
        ];
        for value in values {
            assert_eq!(field_of(&field_json(&value)).unwrap(), value);
        }
    }

    #[test]
    fn many_resources_or_a_destructive_one_is_previewed() {
        let few = vec![json!({"resources": ["a", "b"], "destructive": false})];
        assert!(!wants_preview(&few));
        let many: Vec<Json> = ["a", "b", "c", "d"]
            .iter()
            .map(|r| json!({"resources": [r], "destructive": false}))
            .collect();
        assert!(wants_preview(&many));
        assert!(wants_preview(&[
            json!({"resources": ["a"], "destructive": true})
        ]));
    }
}
