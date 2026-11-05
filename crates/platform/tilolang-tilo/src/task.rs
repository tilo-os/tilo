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
use std::sync::Arc;

use serde_json::{Value as Json, json};
use tilolang_runtime::catalogue::SignatureKind;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::flow::Journal;
use tilolang_runtime::ids::{DocId, RunId, TypeId};
use tilolang_runtime::meta::{Meta, PackageMeta, parse_ty};
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
    /// Where each of them takes the step back (`{app, resource, published}`), in the same
    /// order.
    history: Vec<Json>,
}

/// The task's session: the runtime with every installed app's package, and its `let`s.
///
/// The session belongs to the task, not to one writer: the agent's programs and the lines its
/// person types run in the same session with the same `let`s, each as its own author
/// ([`TaskSession::run`]).
pub struct TaskSession {
    runtime: Runtime,
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
    /// The session of a task reaching the workspace through `door`, with the packages `bundles`
    /// (every tilolang app the session runs, each after the packages it uses), the native apps
    /// `native` (what they publish, from their surfaces: [`crate::catalogue::NativeSurface`])
    /// and the platform's own (the device's and the session's resources, Settings':
    /// [`crate::platform_apps`]), whose actions a program invokes where the session publishes
    /// them. A program names and type-checks every app of them; one it reaches nothing of yet
    /// refuses in one place ([`Unreachable`]).
    ///
    /// # Errors
    /// The runtime cannot start or a package does not load.
    pub fn new(
        person: Option<&str>,
        door: &Rc<dyn TaskDoor>,
        bundles: &[Bundle],
        native: &[PackageMeta],
    ) -> Result<Self, String> {
        let reads = Rc::new(RefCell::new(Reads::default()));
        let packages = Rc::new(RefCell::new(HashMap::new()));
        let schemas = Arc::new(crate::schema::Schemas::new());
        let unreachable = Rc::new(unreachable(bundles, native));
        let store = TaskStore {
            door: Rc::clone(door),
            reads: Rc::clone(&reads),
            packages: Rc::clone(&packages),
            schemas: Arc::clone(&schemas),
            unreachable: Rc::clone(&unreachable),
            journals: HashMap::new(),
            fresh: 0,
        };
        let runtime = Runtime::new(store, DecidesNothing).map_err(|error| error.to_string())?;
        let mut apps: HashMap<String, Meta> = HashMap::new();
        // Each app's loaded module, by the app: the id an action records as its authorship.
        let mut modules: HashMap<String, ModuleHash> = HashMap::new();
        let _installed = schemas.enter();
        for bundle in bundles {
            for import in &bundle.imports {
                let module = runtime
                    .reload_package(&import.wasm, import.meta.clone())
                    .map_err(|error| error.to_string())?;
                modules.insert(import.meta.app.clone(), module);
                import.meta.register_schemas();
                if let Some(module) = modules.get(&import.meta.app) {
                    crate::schema::register_package(&import.meta, module);
                }
                apps.insert(import.meta.app.clone(), import.meta.clone());
            }
            let module = runtime
                .reload_package(&bundle.wasm, bundle.meta.clone())
                .map_err(|error| error.to_string())?;
            modules.insert(bundle.meta.app.clone(), module);
            bundle.meta.register_schemas();
            if let Some(module) = modules.get(&bundle.meta.app) {
                crate::schema::register_package(&bundle.meta, module);
            }
            apps.insert(bundle.meta.app.clone(), bundle.meta.clone());
        }
        // The native apps, after the ones with code: what each publishes, and not how it runs.
        load_apps(&runtime, &mut apps, native.iter().cloned())?;
        reads.borrow_mut().apps = apps.keys().cloned().collect();
        // The platform's packages, the session's own among them: no record holds their
        // resources, so they are read by no record read; their actions are invoked on the
        // resource the session publishes.
        let platform = crate::platform_apps::platform_packages()?
            .into_iter()
            .chain(std::iter::once(crate::platform_apps::session_package()?));
        let published: std::collections::HashSet<String> =
            load_apps(&runtime, &mut apps, platform)?
                .into_iter()
                .collect();
        *packages.borrow_mut() = modules
            .iter()
            .map(|(app, module)| (app.clone(), module.0.clone()))
            .collect();
        let steps = Rc::new(RefCell::new(Steps::default()));
        runtime.set_invoker(DoorInvoker {
            door: Rc::clone(door),
            apps,
            published,
            addresses: crate::platform_apps::session_addresses()?,
            steps: Rc::clone(&steps),
            unreachable: Rc::clone(&unreachable),
        });
        let asked = Rc::clone(door);
        let reached = Rc::clone(&unreachable);
        runtime.set_reader(move |request| {
            if let Some(why) = reached.why(request.app()) {
                return Err(why);
            }
            let encoded = serde_json::to_value(request).map_err(|error| error.to_string())?;
            asked.query(request.app(), &encoded)
        });
        runtime.set_clock(crate::host::system_time);
        Ok(Self {
            runtime,
            person: person.map(Principal::new),
            session: RefCell::new(Session::new()),
            steps,
            reads,
            door: Rc::clone(door),
        })
    }

    /// Every installed app the task's programs may name, by the name they name it with: its
    /// id and its declarations (`{alias: {app, signatures: [{name, id, kind, signature,
    /// call}]}}`), read from the packages alone, whether or not any app runs. `call` is how a
    /// program writes it: an action called (`session.place(resource: host.Resource, x:
    /// Option<Int> = .none)`), a type by its name (`notes.Note`), every type in it named as a
    /// program names it. An action says what it acts on, so a listing tells it from another
    /// app's action of the same id: `on`, the package's own type its first parameter takes (a
    /// resource of that type), or `at`, the resource the session publishes a loose one on.
    #[must_use]
    pub fn catalogue(&self) -> Json {
        let signatures = self.runtime.catalogue();
        let mut named: Vec<String> = signatures.iter().map(|s| s.app.clone()).collect();
        named.sort_by_key(|app| std::cmp::Reverse(app.len()));
        named.dedup();
        let in_programs = |text: &str| {
            let mut text = text.replace("enum ", "");
            for app in &named {
                let alias = tilolang::resolve::default_alias(app);
                text = text.replace(&format!("{app}."), &format!("{alias}."));
            }
            text
        };
        let own_types: std::collections::HashSet<(String, String)> = signatures
            .iter()
            .filter(|s| s.kind == SignatureKind::Type)
            .map(|s| (s.app.clone(), s.name.clone()))
            .collect();
        // ignored-failure: a session without its description lists its actions by their ids
        let addresses = crate::platform_apps::session_addresses().unwrap_or_default();
        let mut apps = serde_json::Map::new();
        for signature in signatures {
            if signature.app == tilolang::HOST_APP {
                continue;
            }
            let alias = tilolang::resolve::default_alias(&signature.app);
            let (kind, call) = match signature.kind {
                SignatureKind::Action => (
                    "action",
                    signature.text.strip_prefix("action ").map(&in_programs),
                ),
                SignatureKind::Type => ("type", Some(format!("{alias}.{}", signature.name))),
                SignatureKind::Enum => ("enum", Some(format!("{alias}.{}", signature.name))),
                SignatureKind::View => ("view", None),
                SignatureKind::Flow => ("flow", None),
                SignatureKind::Fn => ("fn", None),
            };
            let action = signature.kind == SignatureKind::Action;
            let on = action
                .then(|| first_param_type(&signature.text))
                .flatten()
                .filter(|ty| own_types.contains(&(signature.app.clone(), (*ty).to_owned())));
            let at = (action && on.is_none() && signature.app == crate::platform_apps::SESSION_APP)
                .then(|| addresses.get(&signature.id))
                .flatten();
            let app = apps
                .entry(alias)
                .or_insert_with(|| json!({"app": signature.app, "signatures": []}));
            if let Some(listed) = app["signatures"].as_array_mut() {
                listed.push(json!({
                    "name": signature.name,
                    "id": signature.id,
                    "kind": kind,
                    "signature": signature.render(),
                    "call": call,
                    "on": on,
                    "at": at,
                }));
            }
        }
        Json::Object(apps)
    }

    /// Read the time from `clock` (ISO-8601 UTC), for `now` and the times filters write.
    pub fn set_clock(&self, clock: impl Fn() -> String + 'static) {
        self.runtime.set_clock(clock);
    }

    /// Run `source`, written by `author`, as one step of the task; see the module docs for
    /// `confirm`. The program runs as its author: the agent's as the agent, a line its person
    /// typed as the person. The answer: `{state, output, detail, lets, steps, changed, apps}`,
    /// `state` one of `done`, `preview`, `suspended`, `rejected`, `failed`.
    pub fn run(&self, source: &str, confirm: bool, author: &Principal) -> Json {
        if !confirm {
            let saved = self.session.borrow().clone();
            let (answer, steps) = self.run_once(source, true, author);
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
        self.run_once(source, false, author).0
    }

    fn run_once(&self, source: &str, provisional: bool, author: &Principal) -> (Json, Steps) {
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
            .run_with(&self.runtime, author, source, inputs);
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
            // which the history verbs are published on; for a resource the session publishes,
            // that resource itself, whose own `undo` the session publishes beside its actions.
            "history": steps.history,
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

/// The type of the first parameter of the action declared as `text` (`action a.b(w: Window,
/// ...)` is `Window`).
fn first_param_type(text: &str) -> Option<&str> {
    let (_, params) = text.split_once('(')?;
    let first = params.split([',', ')']).next()?;
    let (_, ty) = first.split_once(':')?;
    let ty = ty.split(" =").next()?.trim();
    (!ty.is_empty()).then_some(ty)
}

/// Sends a program's invocations through the door, or in a provisional run lists them.
struct DoorInvoker {
    door: Rc<dyn TaskDoor>,
    apps: HashMap<String, Meta>,
    /// The native apps among them, which a program names and invokes nothing of yet.
    unreachable: Rc<Unreachable>,
    /// The platform's apps, whose resources the session publishes under their own ids.
    published: std::collections::HashSet<String>,
    /// The resource each of the session's loose actions is published on, by the action's id.
    addresses: std::collections::BTreeMap<String, String>,
    steps: Rc<RefCell<Steps>>,
}

impl DoorInvoker {
    /// The id the session publishes the resource `r` under, as a published action takes it: a
    /// resource of one of the platform's apps (a window) by its own id, a tilolang app's by its
    /// app and its id in its record (`tl:so.tilo.lang.notes:<id>`). `r`'s type names the app:
    /// a type of a package this task holds by its id, any other as `<app>#<Type>`.
    fn published_id(&self, r: &ResourceRef) -> String {
        let id = r.id_string();
        let app = match r.ty.as_str().split_once('#') {
            Some((app, _)) => Some(app.to_owned()),
            None => self
                .apps
                .values()
                .find(|meta| meta.types.iter().any(|ty| ty.id == r.ty.as_str()))
                .map(|meta| meta.app.clone()),
        };
        match app {
            Some(app) if !self.published.contains(&app) => resource_id(&app, &id),
            _ => id,
        }
    }
}

impl Invoker for DoorInvoker {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let app = invocation.app();
        if let Some(why) = self.unreachable.why(app) {
            return Invoked::Refused(why);
        }
        let Some(meta) = self.apps.get(app) else {
            return Invoked::Refused(format!("this task may use no app {app}"));
        };
        let published = self.published.contains(app);
        let call = if published {
            published_call(meta, invocation, &self.addresses, |r| self.published_id(r))
        } else {
            call_of(&self.apps, meta, invocation)
        };
        let call = match call {
            Ok(call) => call,
            Err(why) => return Invoked::Failed(why),
        };
        let resources: Vec<String> = invocation
            .resources()
            .iter()
            .map(|r| {
                if published {
                    r.id_string()
                } else {
                    resource_id(app, &r.id_string())
                }
            })
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
            // An action that only reads or shows (`overview`, `search`) changes nothing of the
            // workspace: it is no step, and nothing of it is taken back. What it showed is
            // closed as it was opened, never undone.
            Answered::Done(output) if reads_only(invocation) => {
                match result_of(meta, invocation, &output) {
                    Ok(result) => Invoked::Done(result),
                    Err(why) => Invoked::Failed(why),
                }
            }
            Answered::Done(output) => {
                let mut steps = self.steps.borrow_mut();
                steps.done = steps.done.saturating_add(1);
                if !steps.apps.iter().any(|known| known == app) {
                    steps.apps.push(app.to_owned());
                    let resource = if published {
                        call.get("resource")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    } else {
                        package_resource_id(app)
                    };
                    steps
                        .history
                        .push(json!({"app": app, "resource": resource, "published": published}));
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

/// Whether the parameter `name` of the action `action` of `meta` is a flag (`Bool`, or an
/// optional one).
fn declared_flag(meta: &Meta, action: &str, name: &str) -> bool {
    meta.actions
        .iter()
        .find(|declared| declared.id == action)
        .and_then(|declared| declared.params.iter().find(|param| param.name == name))
        // ignored-failure: a type this cannot read is no flag; the action still checks the value
        .and_then(|param| parse_ty(&param.ty, &meta.app).ok())
        .is_some_and(|ty| match ty {
            tilolang_runtime::meta::Ty::Bool => true,
            tilolang_runtime::meta::Ty::Option(inner) => {
                matches!(*inner, tilolang_runtime::meta::Ty::Bool)
            }
            _ => false,
        })
}

/// Whether `invocation` only reads or shows: an action declared `read` (a surface opened or
/// closed, a search run), which changes no resource.
fn reads_only(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Action {
            class: tilolang_runtime::meta::EffectClass::Read,
            ..
        }
    )
}

/// The door's call for `invocation` of the platform's package `meta`: its declared verb on the
/// resource the session publishes it as (`shell:arrangement`), with every other argument as the
/// published action takes it, by name, a resource by the id `published_id` gives it. The call
/// says it is the session's (`published`), which the door sends to the session's own provider. A
/// loose action (the session's `place`) goes to the resource `addresses` says it is published on.
fn published_call(
    meta: &Meta,
    invocation: &Invocation,
    addresses: &std::collections::BTreeMap<String, String>,
    published_id: impl Fn(&ResourceRef) -> String,
) -> Result<Json, String> {
    let Invocation::Action {
        app,
        action,
        args,
        subject,
        ..
    } = invocation
    else {
        return Err(format!(
            "{} publishes its resources' fields read-only; change them through its actions",
            meta.title
        ));
    };
    // A platform package's action is its published verb, by the same id (`show-surface`).
    let resource = match subject {
        Some(subject) => subject.id_string(),
        None => addresses
            .get(action)
            .cloned()
            .filter(|_| app == crate::platform_apps::SESSION_APP)
            .ok_or_else(|| format!("`{action}` of {app} names no resource it is published on"))?,
    };
    let mut arguments = serde_json::Map::new();
    for (name, value) in args {
        if matches!(value, Value::Ref(r) if r.target.id_string() == resource) {
            continue;
        }
        // An optional argument given as `.none` is not sent: the published action's own default
        // holds. One given as `.some(x)` is sent as `x`, as the published action takes it.
        let value = match value {
            Value::Opt(None) => continue,
            Value::Opt(Some(inner)) => inner.as_ref(),
            other => other,
        };
        let json = match value {
            Value::Ref(r) => json!(published_id(&r.target)),
            // A flag travels as the integer the guest boxed it as; the published action takes
            // it as the flag its parameter declares.
            Value::Int(n) if declared_flag(meta, action, name) => json!(*n != 0),
            other => tilolang_runtime::value::to_json(other)
                .map_err(|error| format!("`{name}` does not send: {error}"))?,
        };
        arguments.insert(name.clone(), json);
    }
    Ok(json!({
        "app": app,
        "resource": resource,
        "action": action,
        "arguments": arguments,
        "published": true,
    }))
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

/// The apps a task names and reaches nothing of yet: native apps, whose records and actions
/// stay in the app, which a program does not run ([`crate::catalogue::described`] reads what
/// they publish, not how it runs). A program type-checks against one, and every read and
/// invocation of it answers this one refusal.
struct Unreachable(std::collections::HashSet<String>);

/// The native apps among `native` that a program reaches nothing of yet: each one the bundles
/// here do not carry, whose own package is the one that counts.
fn unreachable(bundles: &[Bundle], native: &[PackageMeta]) -> Unreachable {
    let carried: std::collections::HashSet<&str> = bundles
        .iter()
        .flat_map(|bundle| {
            bundle
                .imports
                .iter()
                .map(|import| import.meta.app.as_str())
                .chain(std::iter::once(bundle.meta.app.as_str()))
        })
        .collect();
    Unreachable(
        native
            .iter()
            .map(|meta| meta.app.clone())
            .filter(|app| !carried.contains(app.as_str()))
            .collect(),
    )
}

/// Load the packages of apps this runtime does not run ([`Runtime::load_foreign`]), passing over
/// the ones an app here already has: what they publish, and not how they run. The apps it
/// loaded.
fn load_apps(
    runtime: &Runtime,
    apps: &mut HashMap<String, Meta>,
    metas: impl IntoIterator<Item = PackageMeta>,
) -> Result<Vec<String>, String> {
    let mut loaded = Vec::new();
    for meta in metas {
        if apps.contains_key(&meta.app) {
            continue;
        }
        runtime
            .load_foreign(meta.clone())
            .map_err(|error| error.to_string())?;
        meta.register_schemas();
        loaded.push(meta.app.clone());
        apps.insert(meta.app.clone(), meta);
    }
    Ok(loaded)
}

impl Unreachable {
    /// Why a program cannot reach `app` yet, when it is a native one.
    fn why(&self, app: &str) -> Option<String> {
        self.0.contains(app).then(|| {
            format!("{app} is not yet reachable from a program: a native app's records and actions stay in the app")
        })
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
    /// The module hash of each app's package, by the app: which package version's schemas a
    /// record of it resolves by. Filled as the packages load, after the store is in the runtime.
    packages: Rc<RefCell<HashMap<String, String>>>,
    /// Every installed app's package schemas, as the runtime loaded them: what a read of a
    /// record resolves its type by when the app the record's type names is not the one being
    /// read (a field holding another app's record).
    schemas: Arc<crate::schema::Schemas>,
    /// The native apps among them, which a program reads nothing of yet.
    unreachable: Rc<Unreachable>,
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
        if let Some(why) = self.unreachable.why(&app) {
            return Err(StoreError::Backend(why));
        }
        // The app's package table installed: what the record's type id names is that package
        // version's schema ([`crate::schema::for_module`]), and not another version of the same
        // app that loaded here for another task or host.
        let _installed = self.schemas.enter();
        let _package = self
            .packages
            .borrow()
            .get(&app)
            .and_then(|module| crate::schema::for_module(&app, module))
            .map(|package| package.enter());
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
        if let Some(why) = self.unreachable.why(&app) {
            return Err(StoreError::Backend(why));
        }
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

    fn outbox_next_held(&self) -> Option<String> {
        None
    }

    fn outbox_withdraw(&mut self, _: &DocId, _: &str, _: &ChangeNote) -> Result<usize, StoreError> {
        Err(StoreError::Invalid(WRITES_NOTHING.into()))
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
    /// every field as stored, by field id, or `{"missing": true}` when the workspace holds no such
    /// resource. The app's half of a task's read; what the task may read the door decided
    /// before it asked.
    ///
    /// # Errors
    /// The item names nothing in this workspace.
    pub fn read_values(&self, item: &str) -> Result<Json, String> {
        let unknown = || format!("no resource `{item}` in this workspace");
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

    /// A door that answers nothing: what the test asserts is never reached.
    struct Unasked;

    impl TaskDoor for Unasked {
        fn read(&self, _: &str, _: &str) -> Result<Json, String> {
            Err("a refused invocation reads nothing".to_owned())
        }

        fn invoke(&self, _: &Json) -> Answered {
            Answered::Failed("a refused invocation is not sent".to_owned())
        }

        fn query(&self, _: &str, _: &Json) -> Result<Json, String> {
            Err("a refused invocation asks nothing".to_owned())
        }
    }

    #[test]
    fn a_catalogue_of_apps_names_each_ones_verbs_by_its_app() {
        // Chat and the session both publish a `share-window` verb: in one catalogue they are
        // two actions, and each app's own door takes its verb back.
        let chat = crate::catalogue::NativeSurface {
            app: "so.tilo.chat".to_owned(),
            title: "Chat".to_owned(),
            surface: include_str!("../../../../apps/chat/surface.toml").to_owned(),
        }
        .package()
        .expect("Chat's surface reads");
        let session = crate::platform_apps::session_package().expect("the session's reads");
        assert!(
            chat.actions
                .iter()
                .any(|action| action.id == "so.tilo.chat#share-window"),
            "{:?}",
            chat.actions.iter().map(|a| &a.id).collect::<Vec<_>>()
        );
        assert!(
            session
                .actions
                .iter()
                .any(|action| action.id == "share-window"),
            "{:?}",
            session.actions.iter().map(|a| &a.id).collect::<Vec<_>>()
        );
        for (app, id, verb) in [
            ("so.tilo.chat", "so.tilo.chat#share-window", "share-window"),
            ("so.tilo.chat", "create-channel", "create-channel"),
            ("so.tilo.chat", "a1b2c3", "a1b2c3"),
        ] {
            assert_eq!(crate::catalogue::published_verb(app, id), verb);
        }
    }

    #[test]
    fn a_native_apps_action_is_refused_from_one_place() {
        let meta = crate::catalogue::described(
            "so.tilo.chat",
            "Chat",
            include_str!("../../../../apps/chat/surface.toml"),
        )
        .expect("Chat's surface reads");
        let app = meta.app.clone();
        let invoker = DoorInvoker {
            door: Rc::new(Unasked) as Rc<dyn TaskDoor>,
            apps: HashMap::from([(app.clone(), meta)]),
            published: std::collections::HashSet::new(),
            addresses: std::collections::BTreeMap::new(),
            steps: Rc::new(RefCell::new(Steps::default())),
            unreachable: Rc::new(Unreachable([app.clone()].into_iter().collect())),
        };
        let invocation = Invocation::Action {
            app,
            action: "create-channel".to_owned(),
            name: "create_channel".to_owned(),
            args: Vec::new(),
            class: tilolang_runtime::meta::EffectClass::Undoable,
            destructive: false,
            author: None,
            subject: None,
        };
        match invoker.invoke(&invocation) {
            Invoked::Refused(why) => {
                assert!(why.contains("so.tilo.chat"), "{why}");
                assert!(why.contains("is not yet reachable from a program"), "{why}");
            }
            other => panic!("a native app's action is refused, not {other:?}"),
        }
    }
}
