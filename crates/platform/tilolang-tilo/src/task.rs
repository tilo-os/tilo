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
//! **Packages that do not load.** A package the session cannot load here (a module that does not
//! link, a description that does not read) leaves only its app unreachable: the session reports
//! it once ([`TaskSession::could_not_load`], which the catalogue lists as `can't be reached:
//! <why>`), and runs every other app as it would without it.
//!
//! **Reads** go to the door ([`TaskDoor::read`], [`TaskDoor::query`]). A tilolang app's the
//! session's host answers itself from the app's record as the door hands it over, read with the
//! app's package ([`crate::task_records::RecordReads`]): no app runs for them. One resource is
//! read once per program. A native app's record only the app knows how to read, so a read of it
//! is the app's own `read` action through the door, as the task ([`crate::native`]).
//!
//! **Native apps.** A native app publishes what it offers, not how it runs: its verbs are actions
//! of its catalogue, a field edit of its record is its own `field-edit` action, and both go to
//! the app through the door as the task's invocation, which the broker decides as it decides
//! `tilo_do`'s and runs the app for when it is not running.
//!
//! **Steps.** One program is one step. Run without `confirm`, it first runs provisionally: its
//! invocations are listed, not sent, and nothing it bound is kept. One that changed nothing has
//! answered; one that would change many resources or destroy something answers with that list as
//! its preview; any other runs again for real at once. With `confirm` it runs for real straight
//! away.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{Value as Json, json};
use tilolang_runtime::catalogue::SignatureKind;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::ids::{DocId, RunId, TypeId};
use tilolang_runtime::meta::{Meta, PackageMeta, parse_ty};
use tilolang_runtime::program::{ProgramOutcome, Session};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::run::Journal;
use tilolang_runtime::store::{
    Applied, ChangeNote, ChangeSummary, FieldSpec, FieldValue, Mutation, NewRecord, OutboxEntry,
    Scalar, Store, StoreError, SubId, Txn, Version,
};
use tilolang_runtime::value::{ResourceRef, Value, from_json};
use tilolang_runtime::{ActionInfo, Authority, Decision, ModuleHash, Principal, Runtime};

use crate::bundle::Bundle;
use crate::host::{Host, argument_json};
use crate::meta::MetaExt as _;
use crate::native::Native;
use crate::publish::{package_resource_id, resource_id};

/// More resources than this changed by one program are many: it answers with a preview first.
pub const MANY_RESOURCES: usize = 3;

/// What a task's host asks of its door. Each call waits for the door's answer.
pub trait TaskDoor {
    /// The fields of the resource `item` (its id in its record) of the app `app`, its type `ty`,
    /// as [`Host::read_values`] answers them. A native app's id names the item alone; `ty` says
    /// which of the app's collections a program holds it by ([`crate::native`]).
    fn read(&self, app: &str, item: &str, ty: &str) -> Result<Json, String>;

    /// Invoke `call` (`{app, resource, action, arguments}`) as the task.
    fn invoke(&self, call: &Json) -> Answered;

    /// What the app `app` answers to a program's `request` to read it (`grep`, `find`, `read`,
    /// `describe`: a [`tilolang_runtime::reads::ReadRequest`]), read as the task.
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

/// An installed app the task's session holds no package of, with why: a program that names it
/// is told it can't be reached, and every other app is reached all the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreachable {
    /// The app's id, as its package declares it.
    pub app: String,
    /// Why its package did not load here.
    pub reason: String,
}

impl Unreachable {
    /// What a program naming the app is told: `can't be reached: <reason>`.
    #[must_use]
    pub fn said(&self) -> String {
        format!("can't be reached: {}", self.reason)
    }
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
    /// The installed apps whose packages did not load here, each reported once: a program that
    /// names one reads that it can't be reached, and every other app is reached.
    unreachable: RefCell<Vec<Unreachable>>,
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
    /// them. A program names and type-checks every app of them; a native one it names the record
    /// of, invokes and assigns fields of, and reads what it holds through the app
    /// ([`crate::native::NativeReads`]), which `door` carries too. A package that does not load
    /// leaves only its app unreachable ([`Self::could_not_load`]): the session starts with every
    /// other app, and the failure is reported once.
    pub fn new(
        person: Option<&str>,
        door: &Rc<dyn TaskDoor>,
        bundles: &[Bundle],
        native: Rc<Native>,
    ) -> Result<Self, String> {
        let reads = Rc::new(RefCell::new(Reads::default()));
        let packages = Rc::new(RefCell::new(HashMap::new()));
        let schemas = Arc::new(crate::schema::Schemas::new());
        let store = TaskStore {
            door: Rc::clone(door),
            reads: Rc::clone(&reads),
            packages: Rc::clone(&packages),
            schemas: Arc::clone(&schemas),
            journals: HashMap::new(),
            fresh: 0,
        };
        let runtime = Runtime::new(store, DecidesNothing).map_err(|error| error.to_string())?;
        let mut apps: HashMap<String, Meta> = HashMap::new();
        // Each app's loaded module, by the app: the id an action records as its authorship.
        let mut modules: HashMap<String, ModuleHash> = HashMap::new();
        // The packages that did not load, by their app and why: each leaves its own app alone
        // unreachable, and every other app is loaded as it would be without it.
        let mut failed: Vec<Unreachable> = Vec::new();
        let _installed = schemas.enter();
        for bundle in bundles {
            for import in &bundle.imports {
                if let Err(reason) = load_module(
                    &runtime,
                    &mut modules,
                    &mut apps,
                    &import.wasm,
                    import.meta.clone(),
                ) {
                    failed.push(Unreachable {
                        app: import.meta.app.clone(),
                        reason,
                    });
                }
            }
            if let Err(reason) = load_module(
                &runtime,
                &mut modules,
                &mut apps,
                &bundle.wasm,
                bundle.meta.clone(),
            ) {
                failed.push(Unreachable {
                    app: bundle.meta.app.clone(),
                    reason,
                });
            }
        }
        // The native apps, after the ones with code: what each publishes, and not how it runs.
        load_apps(&runtime, &mut apps, native.packages().cloned(), &mut failed);
        let loaded: HashSet<String> = apps.keys().cloned().collect();
        reads.borrow_mut().apps = loaded.iter().cloned().collect();
        // The platform's packages, the session's own among them: no record holds their
        // resources, so they are read by no record read; their actions are invoked on the
        // resource the session publishes.
        // The session's own description is one of the platform's (`platform_packages`): its
        // resources are read without a record, and its actions run at the session's door.
        let platform = crate::platform_apps::platform_packages()?;
        let published: std::collections::HashSet<String> =
            load_apps(&runtime, &mut apps, platform, &mut failed)
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
            native,
        });
        let asked = Rc::clone(door);
        runtime.set_reader(move |request| {
            let encoded = serde_json::to_value(request).map_err(|error| error.to_string())?;
            asked.query(request.app(), &encoded)
        });
        runtime.set_clock(crate::host::system_time);
        let session = Self {
            runtime,
            person: person.map(Principal::new),
            session: RefCell::new(Session::new()),
            steps,
            reads,
            door: Rc::clone(door),
            unreachable: RefCell::new(Vec::new()),
        };
        // Each app no package loaded is reported: one that loaded from another package that
        // carried it is reached, and says nothing. The report is what a program naming the app
        // reads, and what the host's log holds, once.
        for gone in failed {
            if !loaded.contains(&gone.app) {
                session.could_not_load(&gone.app, &gone.reason);
            }
        }
        Ok(session)
    }

    /// Report an app whose package did not load, by the id its package declares and why: the
    /// host reports an app whose payload did not read with it, and the session reports every
    /// package of its own that did not load. A program naming the app reads that it can't be
    /// reached ([`Self::catalogue`]), and the rest of the task runs as it would without it. An
    /// app reported once is not reported again.
    pub fn could_not_load(&self, app: &str, why: &str) {
        let mut unreachable = self.unreachable.borrow_mut();
        if unreachable.iter().any(|gone| gone.app == app) {
            return;
        }
        tracing::warn!(app, why, "an installed app can't be reached");
        unreachable.push(Unreachable {
            app: app.to_owned(),
            reason: why.to_owned(),
        });
    }

    /// Every installed app the task's programs may name, by the name they name it with: its
    /// id and its declarations (`{alias: {app, signatures: [{name, id, kind, signature,
    /// call}]}}`), read from the packages alone, whether or not any app runs. `call` is how a
    /// program writes it: an action called (`session.place(resource: host.Resource, x:
    /// Option<Int> = .none)`), a type by its name (`notes.Note`), every type in it named as a
    /// program names it. An action says what it acts on, so a listing tells it from another
    /// app's action of the same id: `on`, the package's own type its first parameter takes (a
    /// resource of that type), or `at`, the resource the session publishes a loose one on. A
    /// `root` is the resource a program names to reach an app the session does not run: the
    /// record whose verbs are that app's.
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
                SignatureKind::Fn => ("fn", None),
                SignatureKind::Root => ("root", Some(format!("{alias}.{}", signature.name))),
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
        // An installed app whose package did not load is listed too, by its name, with what a
        // program naming it is told: the rest of the catalogue is read as it would be without it.
        for gone in self.unreachable.borrow().iter() {
            if gone.app == tilolang::HOST_APP {
                continue;
            }
            let alias = tilolang::resolve::default_alias(&gone.app);
            let app = apps
                .entry(alias)
                .or_insert_with(|| json!({"app": gone.app, "signatures": []}));
            if let Some(fields) = app.as_object_mut() {
                fields.insert("unreachable".to_owned(), json!(gone.said()));
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
    /// The native apps among them, which a program invokes on their own records.
    native: Rc<Native>,
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
        let Some(meta) = self.apps.get(app) else {
            return Invoked::Refused(format!("this task may use no app {app}"));
        };
        let published = self.published.contains(app);
        // The record a native app publishes: where its steps are taken back, and what a
        // program names to invoke it.
        let record = self.native.record(app);
        let call = if published {
            published_call(meta, invocation, &self.addresses, |r| self.published_id(r))
        } else if self.native.is(app) {
            native_call(&self.apps, meta, record, invocation)
        } else {
            call_of(&self.apps, meta, invocation)
        };
        let call = match call {
            Ok(call) => call,
            Err(why) => return Invoked::Failed(why),
        };
        // A native app's resources are the ids its own record holds; a tilolang app's are its
        // record's, under the app.
        let resources: Vec<String> = invocation
            .resources()
            .iter()
            .map(|r| {
                if published || self.native.is(app) {
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
                    // The resource the step is taken back on: the one the session published a
                    // loose action on, the app's own record, or the record the task's change of
                    // a tilolang app applied to.
                    let resource = if published {
                        call.get("resource")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    } else if let Some(record) = record {
                        record.to_owned()
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
        from,
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
            other => tilolang_runtime::value::to_argument(other)
                .map_err(|error| format!("`{name}` does not send: {error}"))?,
        };
        arguments.insert(name.clone(), json);
    }
    made_from(from.as_ref(), &mut arguments, &published_id);
    Ok(json!({
        "app": app,
        "resource": resource,
        "action": action,
        "arguments": arguments,
        "published": true,
    }))
}

/// What the invocation is made from, when it is made from something, among the arguments the
/// door takes: the key [`tilo_actions::params::FROM_ARGUMENT`], as the reference a caller names
/// its source with.
fn made_from(
    from: Option<&ResourceRef>,
    arguments: &mut serde_json::Map<String, Json>,
    published: impl Fn(&ResourceRef) -> String,
) {
    if let Some(from) = from {
        arguments.insert(
            tilo_actions::params::FROM_ARGUMENT.to_owned(),
            json!({"ref": published(from), "ty": from.ty}),
        );
    }
}

/// The `arguments` of the action `action` declared by `meta`: each of `args` (past the first
/// one, when it is the resource the action is on) by its parameter's name, as the door takes it
/// ([`argument_json`]). `apps` holds every package an argument's type may name.
fn arguments_of(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    action: &str,
    args: &[(String, Value)],
    subject: bool,
) -> Result<Json, String> {
    let declared = meta
        .actions
        .iter()
        .find(|declared| declared.id == action)
        .ok_or_else(|| format!("{} declares no action {action}", meta.app))?;
    let mut arguments = serde_json::Map::new();
    for (name, value) in args.iter().skip(usize::from(subject)) {
        let param = declared
            .params
            .iter()
            .find(|param| param.name == *name)
            .ok_or_else(|| format!("{action} takes no argument `{name}`"))?;
        let ty = parse_ty(&param.ty, &meta.app)?;
        let owner = |other: &str| {
            apps.get(other)
                .cloned()
                .ok_or_else(|| format!("this task may use no app {other}"))
        };
        arguments.insert(name.clone(), argument_json(&owner, meta, value, &ty)?);
    }
    Ok(Json::Object(arguments))
}

/// The door's call for `invocation` of the native app `meta`: its declared verb, as the app's
/// own door names it, on the resource the program named (the action's own record, or an item of
/// it), with the arguments the app takes. A field assignment is the app's own `field-edit` of
/// its record. Either is the task's invocation of the app, decided and undone as every other
/// call of its actions: the app keeps the step.
fn native_call(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    record: Option<&str>,
    invocation: &Invocation,
) -> Result<Json, String> {
    let app = meta.app.as_str();
    match invocation {
        Invocation::Action {
            action,
            args,
            subject,
            from,
            ..
        } => {
            let Some(record) = record else {
                return Err(format!(
                    "{app} publishes no record a program names its actions on"
                ));
            };
            // The resource the app's own door takes: the one the program named the action on,
            // else the record the app publishes.
            let resource = subject
                .as_ref()
                .map_or_else(|| record.to_owned(), ResourceRef::id_string);
            let mut arguments = arguments_of(apps, meta, action, args, subject.is_some())?;
            if let Json::Object(arguments) = &mut arguments {
                made_from(from.as_ref(), arguments, ResourceRef::id_string);
            }
            Ok(json!({
                "app": app,
                "resource": resource,
                "action": crate::catalogue::published_verb(app, action),
                "arguments": arguments,
            }))
        }
        Invocation::Edit {
            field,
            field_id,
            value,
            target,
            ..
        } => {
            let Some(record) = record else {
                return Err(format!(
                    "{app} publishes no record a program sets the fields of"
                ));
            };
            if target.item.is_some() {
                return Err(format!(
                    "a program sets {app}'s own record fields; what its items hold changes through its actions"
                ));
            }
            let key = field_id
                .split_once('.')
                .map_or(field_id.as_str(), |(_, key)| key);
            // A text field takes the smallest splice of the text it holds; a flag is set whole.
            let op = match value {
                Value::Text(text) => json!({"op": "replace", "text": text}),
                Value::Bool(flag) => json!({"op": "set", "value": flag}),
                _ => {
                    return Err(format!(
                        "a program sets text and flag fields; `{field}` changes through {app}'s actions"
                    ));
                }
            };
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                tilo_resource::EDIT_PARAM.to_owned(),
                json!({"path": [{"field": key}], "op": op}),
            );
            Ok(json!({
                "app": app,
                "resource": record,
                "action": tilo_resource::FIELD_EDIT,
                "arguments": arguments,
            }))
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
            app,
            action,
            args,
            from,
            ..
        } => {
            let declared = meta
                .actions
                .iter()
                .find(|declared| declared.id == *action)
                .ok_or_else(|| format!("{app} declares no action {action}"))?;
            let subject = meta.subject_of(&declared.params);
            let subject = subject.and_then(|_| args.first());
            let resource = match subject {
                Some((_, Value::Ref(r))) => resource_id(app, &r.target.id_string()),
                _ => package_resource_id(app),
            };
            let mut arguments = arguments_of(apps, meta, action, args, subject.is_some())?;
            if let Json::Object(arguments) = &mut arguments {
                made_from(from.as_ref(), arguments, ResourceRef::id_string);
            }
            Ok(json!({
                "app": app,
                "resource": resource,
                // As the app's door publishes it: by its name.
                "action": declared.name,
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

/// Load the package `wasm`, whose metadata is `meta`, into the session: its module, the schemas
/// its metadata declares, and the app a program names it by. Its app fails the load as a whole,
/// saying why.
fn load_module(
    runtime: &Runtime,
    modules: &mut HashMap<String, ModuleHash>,
    apps: &mut HashMap<String, Meta>,
    wasm: &[u8],
    meta: Meta,
) -> Result<(), String> {
    let module = runtime
        .reload_package(wasm, meta.clone())
        .map_err(|error| error.to_string())?;
    meta.register_schemas();
    crate::schema::register_package(&meta, &module);
    modules.insert(meta.app.clone(), module);
    apps.insert(meta.app.clone(), meta);
    Ok(())
}

/// Load the packages of apps this runtime does not run ([`Runtime::load_foreign`]), passing over
/// the ones an app here already has: what they publish, and not how they run. The apps it
/// loaded; one that does not load is handed back in `failed` and leaves every other app reached.
fn load_apps(
    runtime: &Runtime,
    apps: &mut HashMap<String, Meta>,
    metas: impl IntoIterator<Item = PackageMeta>,
    failed: &mut Vec<Unreachable>,
) -> Vec<String> {
    let mut loaded = Vec::new();
    for meta in metas {
        if apps.contains_key(&meta.app) {
            continue;
        }
        if let Err(error) = runtime.load_foreign(meta.clone()) {
            failed.push(Unreachable {
                app: meta.app,
                reason: error.to_string(),
            });
            continue;
        }
        meta.register_schemas();
        loaded.push(meta.app.clone());
        apps.insert(meta.app.clone(), meta);
    }
    loaded
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
        let answer = self
            .door
            .read(&app, &id, &at.ty)
            .map_err(StoreError::Backend)?;
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

    fn versions(&self, at: &ResourceRef) -> Result<Vec<Version>, StoreError> {
        Err(StoreError::Invalid(format!(
            "a task reads no history of {} yet",
            at.id_string()
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
pub(crate) fn field_json(value: &FieldValue) -> Json {
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

    /// Chat, as its session hands the app's package to a task: its verbs named by the catalogue
    /// (`so.tilo.chat#rename-channel`).
    fn chat() -> Meta {
        crate::catalogue::NativeSurface {
            app: "so.tilo.chat".to_owned(),
            title: "Chat".to_owned(),
            surface: include_str!("../../../../apps/chat/surface.toml").to_owned(),
        }
        .package()
        .expect("Chat's surface reads")
    }

    #[test]
    fn a_catalogue_of_apps_names_each_ones_verbs_by_its_app() {
        // Chat and the session both publish a `share-window` verb: in one catalogue they are
        // two actions, and each app's own door takes its verb back.
        let chat = chat();
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

    /// A door that records the calls it is sent, and answers each as a done action.
    #[derive(Default)]
    struct Asked(RefCell<Vec<Json>>);

    impl TaskDoor for Asked {
        fn read(&self, _: &str, _: &str, _: &str) -> Result<Json, String> {
            Err("no read reaches this door".to_owned())
        }

        fn invoke(&self, call: &Json) -> Answered {
            self.0.borrow_mut().push(call.clone());
            Answered::Done(json!({}))
        }

        fn query(&self, _: &str, _: &Json) -> Result<Json, String> {
            Err("no query reaches this door".to_owned())
        }
    }

    /// The door's invoker for the one native app `meta`, with the door `door`.
    fn invoker_of(meta: Meta, door: Rc<dyn TaskDoor>) -> (DoorInvoker, Rc<RefCell<Steps>>) {
        let steps = Rc::new(RefCell::new(Steps::default()));
        let native = Rc::new(Native::of(&[], std::slice::from_ref(&meta)));
        (
            DoorInvoker {
                door,
                apps: HashMap::from([(meta.app.clone(), meta)]),
                published: std::collections::HashSet::new(),
                addresses: std::collections::BTreeMap::new(),
                steps: Rc::clone(&steps),
                native,
            },
            steps,
        )
    }

    #[test]
    fn a_native_apps_verb_is_its_own_on_the_record_it_publishes() {
        let meta = chat();
        let app = meta.app.clone();
        let door = Rc::new(Asked::default());
        let (invoker, steps) = invoker_of(meta, Rc::clone(&door) as Rc<dyn TaskDoor>);
        let record = ResourceRef {
            doc: "chat.workspace".to_owned(),
            item: None,
            ty: "so.tilo.chat#Workspace".to_owned(),
        };
        let invocation = Invocation::Action {
            app,
            action: "so.tilo.chat#rename-channel".to_owned(),
            name: "rename_channel".to_owned(),
            args: vec![
                ("workspace".to_owned(), Value::resource(&record)),
                ("channel".to_owned(), Value::Text("general".to_owned())),
                ("name".to_owned(), Value::Text("town".to_owned())),
            ],
            class: tilolang_runtime::meta::EffectClass::Undoable,
            destructive: false,
            author: None,
            subject: Some(record),
            from: None,
        };
        let answer = invoker.invoke(&invocation);
        assert!(
            matches!(answer, Invoked::Done(_)),
            "Chat's verb runs: {answer:?}"
        );
        assert_eq!(
            door.0.borrow().as_slice(),
            [json!({
                "app": "so.tilo.chat",
                "resource": "chat.workspace",
                "action": "rename-channel",
                "arguments": {"channel": "general", "name": "town"},
            })],
            "the app's own verb, on the record it publishes, without the record among the arguments"
        );
        let steps = steps.borrow();
        assert_eq!(steps.done, 1);
        assert_eq!(
            steps.history,
            [json!({"app": "so.tilo.chat", "resource": "chat.workspace", "published": false})],
            "the step is taken back on the record the app keeps it in"
        );
    }

    #[test]
    fn a_native_apps_field_is_its_own_field_edit() {
        let meta = chat();
        let door = Rc::new(Asked::default());
        let (invoker, _) = invoker_of(meta, Rc::clone(&door) as Rc<dyn TaskDoor>);
        let invocation = Invocation::Edit {
            app: "so.tilo.chat".to_owned(),
            target: ResourceRef {
                doc: "chat.workspace".to_owned(),
                item: None,
                ty: "so.tilo.chat#Workspace".to_owned(),
            },
            field: "agent_names".to_owned(),
            field_id: "Workspace.agent_names".to_owned(),
            value: Value::Text("Reviewer".to_owned()),
        };
        assert!(matches!(invoker.invoke(&invocation), Invoked::Done(_)));
        assert_eq!(
            door.0.borrow().as_slice(),
            [json!({
                "app": "so.tilo.chat",
                "resource": "chat.workspace",
                "action": "field-edit",
                "arguments": {"edit": {
                    "path": [{"field": "agent_names"}],
                    "op": {"op": "replace", "text": "Reviewer"},
                }},
            })]
        );
    }
}
