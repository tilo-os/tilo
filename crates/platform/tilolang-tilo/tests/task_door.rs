//! A workspace of tilolang apps as an agent task's door reaches them: each app's host (a
//! [`Replica`]) answers the task's reads and queries and runs what the task invokes, as the
//! broker delivers it; every invocation is recorded. The task's programs run in a
//! [`TaskSession`] of their own over it.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use automerge::AutoCommit;
use serde_json::{Value as Json, json};
use tilolang_runtime::Principal;
use tilolang_runtime::reads::ReadRequest;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{PlanRequest, edit_action, resource_id};
use tilolang_tilo::task::{Answered, TaskDoor, TaskSession};

use crate::fixtures::Replica;

/// The apps of a workspace, each group of packages run by one host.
pub struct Apps {
    replicas: Vec<RefCell<Replica>>,
    by_app: HashMap<String, usize>,
    bundles: Vec<Bundle>,
    /// What the task invoked, in order.
    pub invoked: RefCell<Vec<Json>>,
    /// Actions whose invocations the policy refuses.
    pub refused: Vec<&'static str>,
    /// Apps the door cannot reach (not open), and why.
    pub unreachable: Vec<(&'static str, &'static str)>,
}

impl Apps {
    /// Hosts for `groups` of packages (source and ids lock), each package compiled once, after
    /// the ones before it, and its root made.
    pub fn new(groups: &[&[(&str, &str)]]) -> Self {
        let mut metas = Vec::new();
        let mut replicas = Vec::new();
        let mut by_app = HashMap::new();
        let mut bundles = Vec::new();
        for group in groups {
            let doc = AutoCommit::new();
            let host = Host::new(&doc, Principal::new("ada"), || None).unwrap();
            for (source, ids) in *group {
                let lock = tilolang::IdsLock::parse(ids).unwrap();
                let compiled = tilolang::compile_package(source, &lock, &metas).unwrap();
                host.load_compiled(&compiled).unwrap();
                host.root_of(&compiled.meta.app).unwrap();
                metas.push(compiled.meta.clone());
                by_app.insert(compiled.meta.app.clone(), replicas.len());
                bundles.push(Bundle {
                    wasm: compiled.wasm.clone(),
                    meta: compiled.meta.clone(),
                    ids: String::new(),
                    imports: Vec::new(),
                });
            }
            let mut replica = Replica {
                name: "ada".to_owned(),
                doc,
                host,
                log: Vec::new(),
            };
            replica.commit();
            replicas.push(RefCell::new(replica));
        }
        Self {
            replicas,
            by_app,
            bundles,
            invoked: RefCell::default(),
            refused: Vec::new(),
            unreachable: Vec::new(),
        }
    }

    /// The host of `app`.
    pub fn replica(&self, app: &str) -> RefMut<'_, Replica> {
        self.replicas[self.by_app[app]].borrow_mut()
    }

    /// A task of these apps, acting for ada.
    pub fn task(self: &Rc<Self>) -> TaskSession {
        TaskSession::new(
            "task-1",
            Some("ada"),
            &(Rc::clone(self) as Rc<dyn TaskDoor>),
            &self.bundles,
        )
        .unwrap()
    }
}

impl TaskDoor for Apps {
    fn read(&self, app: &str, item: &str) -> Result<Json, String> {
        if let Some((_, why)) = self.unreachable.iter().find(|(gone, _)| *gone == app) {
            return Err((*why).to_owned());
        }
        self.replica(app).host.read_values(item)
    }

    fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        let request: ReadRequest =
            serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
        self.replica(app).host.runtime().answer_read(&request)
    }

    fn invoke(&self, call: &Json) -> Answered {
        self.invoked.borrow_mut().push(call.clone());
        let app = call["app"].as_str().unwrap();
        let action = call["action"].as_str().unwrap();
        // The policy names an action by what it is called.
        let named = self
            .replica(app)
            .host
            .package(app)
            .and_then(|package| {
                package
                    .meta
                    .actions
                    .iter()
                    .find(|declared| declared.id == action)
                    .map(|declared| declared.name.clone())
            })
            .unwrap_or_else(|| action.to_owned());
        if self.refused.contains(&named.as_str()) {
            return Answered::Refused(format!("{action}: denied by the workspace's policy"));
        }
        let mut replica = self.replica(app);
        let prefix = resource_id(app, "");
        let resource = call["resource"].as_str().unwrap().strip_prefix(&prefix);
        let arguments = call["arguments"].as_object().unwrap();
        let planned = if action == "edit-text" || action == "edit-flag" {
            let package = replica.host.package(app).unwrap();
            let id = resource.unwrap();
            let ty = replica.host.read_values(id).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned();
            let declared = package.meta.types.iter().find(|t| t.id == ty).unwrap();
            edit_action(
                &package.meta,
                &declared.name,
                id,
                arguments["field"].as_str().unwrap(),
                &arguments["value"],
            )
        } else {
            replica.host.plan(&PlanRequest {
                app,
                kind: "",
                action_id: action,
                resource,
                args: arguments,
                caller: Some(&task_caller()),
            })
        };
        match planned {
            Ok(planned) => {
                // The broker delivers it as the task's.
                let person = std::mem::replace(&mut replica.name, "task-1".to_owned());
                replica.apply(&planned);
                replica.name = person;
                let returned = replica
                    .host
                    .take_returned(action)
                    .map(|made| resource_id(app, &made.id_string()));
                Answered::Done(
                    json!({"value": null, "resources": returned.into_iter().collect::<Vec<_>>()}),
                )
            }
            Err(why) => Answered::Failed(why),
        }
    }
}

/// The task as the broker delivers its invocations: an agent acting for ada.
fn task_caller() -> tilolang_tilo::publish::Caller {
    tilolang_tilo::publish::Caller {
        actor: "task-1".to_owned(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
    }
}
