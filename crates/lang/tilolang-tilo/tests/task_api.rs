//! A workspace of tilolang apps as an agent task's api reaches them: each app's host (a
//! [`Replica`]) answers the task's reads and queries and runs what the task invokes, as the
//! broker delivers it; every invocation is recorded. The task's programs run in a
//! [`TaskSession`] of their own over it.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_runtime::Actor;
use tilolang_runtime::reads::ReadRequest;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{PlanRequest, edit_action, resource_id};
use tilolang_tilo::task::{Answered, TaskApi, TaskSession};

use crate::fixtures::{Replica, Workspace};

/// The apps of a workspace, each group of packages run by one host.
pub struct Apps {
    replicas: Vec<RefCell<Replica>>,
    by_app: HashMap<String, usize>,
    bundles: Vec<Bundle>,
    /// What the task invoked, in order.
    pub invoked: RefCell<Vec<Json>>,
    /// Actions whose invocations the policy refuses.
    pub refused: Vec<&'static str>,
    /// Apps the API cannot reach (not open), and why.
    pub unreachable: Vec<(&'static str, &'static str)>,
    /// The readings the session publishes of its own resources, by the resource's id (the
    /// device's styles, the session's windows): what a program reads within a call, as the
    /// session's services answer it.
    pub readings: RefCell<HashMap<String, Json>>,
    /// The readings a program asked the API for, in order, by the app and resource it named.
    pub read_for: RefCell<Vec<(String, String)>>,
}

impl Apps {
    /// Hosts for `groups` of packages (source and ids lock), each package compiled once, after
    /// the ones before it, and its root made.
    pub fn new(groups: &[&[(&str, &str)]]) -> Self {
        let mut replicas = Vec::new();
        let mut by_app = HashMap::new();
        let mut bundles = Vec::new();
        // Each package compiles once, after the ones before it, whichever group runs it.
        let mut metas: Vec<tilolang::PackageMeta> = Vec::new();
        for group in groups {
            let doc = Workspace::new();
            let host = tilolang_tilo::host::Host::open(
                doc.store(),
                Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                None,
                || None,
            )
            .unwrap();
            for (source, ids) in *group {
                let lock = tilolang::IdsLock::parse(ids).unwrap();
                // A package compiles against the platform packages its `use`s name, as the
                // host's own built-ins do.
                let mut imports = tilolang_tilo::platform_apps::named_platform_packages(source)
                    .expect("the platform's packages read");
                imports.extend(metas.iter().cloned());
                let compiled = tilolang::compile_package(source, &lock, &imports).unwrap();
                host.load_compiled(&compiled).unwrap();
                host.root_of(&compiled.meta.app).unwrap();
                by_app.insert(compiled.meta.app.clone(), replicas.len());
                bundles.push(Bundle {
                    wasm: compiled.wasm.clone(),
                    meta: compiled.meta.clone(),
                    ids: String::new(),
                    imports: Vec::new(),
                    assets: Vec::new(),
                });
                if !metas.iter().any(|meta| meta.app == compiled.meta.app) {
                    metas.push(compiled.meta.clone());
                }
            }
            replicas.push(RefCell::new(Replica::with_host("ada", doc, host)));
        }
        Self {
            replicas,
            by_app,
            bundles,
            invoked: RefCell::default(),
            refused: Vec::new(),
            unreachable: Vec::new(),
            readings: RefCell::default(),
            read_for: RefCell::default(),
        }
    }

    /// The host of `app`.
    pub fn replica(&self, app: &str) -> RefMut<'_, Replica> {
        self.replicas[self.by_app[app]].borrow_mut()
    }

    /// A task of these apps, acting for ada.
    pub fn task(self: &Rc<Self>) -> TaskSession {
        TaskSession::new(
            Some("ada"),
            &(Rc::clone(self) as Rc<dyn TaskApi>),
            &self.bundles,
            Rc::new(tilolang_tilo::native::Native::of(&self.bundles, &[])),
        )
        .unwrap()
    }
}

impl TaskApi for Apps {
    fn reading(
        &self,
        target: &tilolang_runtime::value::ResourceRef,
    ) -> Result<Option<Json>, String> {
        let (app, _) = target
            .ty
            .split_once('#')
            .unwrap_or((target.ty.as_str(), ""));
        self.read_for
            .borrow_mut()
            .push((app.to_owned(), target.id_string()));
        if let Some(reading) = self.readings.borrow().get(&target.id_string()) {
            return Ok(Some(reading.clone()));
        }
        // The session publishes a platform root no service of this device serves (no battery, no
        // Bluetooth) empty, as the shell's own parts are handed it: a program reads its fields as
        // absent and goes on.
        let platform =
            tilolang_tilo::platform_apps::platform_roots()
                .iter()
                .any(|(published, ty)| {
                    tilolang_runtime::value::flat_id(published) == target.id_string()
                        && *ty == target.ty
                });
        Ok(platform.then(|| json!({})))
    }

    fn read(&self, app: &str, item: &str, _: &str) -> Result<Json, String> {
        if let Some((_, why)) = self.unreachable.iter().find(|(gone, _)| *gone == app) {
            return Err((*why).to_owned());
        }
        self.replica(app).host.read_values(item)
    }

    fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        let request: ReadRequest =
            serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
        // An app no host here runs (the device's own resources) has nothing to answer from.
        if !self.by_app.contains_key(app) {
            return Err("no such app".to_owned());
        }
        self.replica(app).host.runtime().answer_read(&request)
    }

    fn invoke(&self, call: &Json) -> Answered {
        self.invoked.borrow_mut().push(call.clone());
        let app = call["app"].as_str().unwrap();
        // The API publishes an action by its name, which the policy names it by too; what is
        // planned and stored of it keeps its id.
        let named = call["action"].as_str().unwrap();
        let action = self
            .replica(app)
            .host
            .package(app)
            .and_then(|package| {
                package
                    .meta
                    .action_published_as(named)
                    .map(|a| a.id.clone())
            })
            .unwrap_or_else(|| named.to_owned());
        let action = action.as_str();
        if self.refused.contains(&named) {
            return Answered::Refused(format!("{named}: denied by the workspace's policy"));
        }
        let replica = self.replica(app);
        let prefix = resource_id(app, "");
        let resource = call["resource"].as_str().unwrap().strip_prefix(&prefix);
        let arguments = call["arguments"].as_object().unwrap();
        // The broker binds the arguments to the action's published parameters before it
        // delivers: what does not bind never reaches the app.
        if let Some(package) = replica.host.package(app)
            && let Some(declared) = package.meta.actions.iter().find(|a| a.id == action)
        {
            let params: Vec<_> = package
                .meta
                .arguments(&declared.params)
                .iter()
                .map(|param| tilolang_tilo::surface::published_param(&package.meta, param))
                .collect();
            if let Err(error) = tilo_actions::ActionArgs::bind_json(&params, arguments) {
                return Answered::Failed(format!("invoke: {action}: {error}"));
            }
        }
        let caller = task_caller();
        let planned = if action == "edit-text" || action == "edit-flag" {
            let package = replica.host.package(app).unwrap();
            let id = resource.unwrap();
            let ty = replica.host.read_values(id).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned();
            let declared = package.meta.types.iter().find(|t| t.id == ty).unwrap();
            edit_action(
                replica.host.store(),
                &package.meta,
                &declared.name,
                id,
                arguments["field"].as_str().unwrap(),
                &arguments["value"],
                Some(&caller.actor),
            )
        } else {
            let request = PlanRequest {
                app,
                kind: "",
                action_id: action,
                resource,
                args: arguments,
                caller: Some(&caller),
                from: None,
            };
            replica.carry_out(&request)
        };
        match planned {
            Ok(_) => {
                let returned = replica
                    .host
                    .take_returned(action)
                    .map(|value| replica.host.action_output(app, action, &value))
                    .transpose();
                match returned {
                    Ok(returned) => {
                        let output = returned.unwrap_or_default();
                        Answered::Done(json!({
                            "value": output.value,
                            "resources": output.resources,
                        }))
                    }
                    Err(why) => Answered::Failed(why),
                }
            }
            Err(why) => Answered::Failed(why),
        }
    }
}

/// The task as the broker delivers its invocations: an agent acting for ada.
fn task_caller() -> tilolang_tilo::publish::Caller {
    tilolang_tilo::publish::Caller {
        actor: tilolang_tilo::runtime::ids::MemberId::agent("task-1", "ada").into(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
        handed: false,
        app: None,
    }
}
