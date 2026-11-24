//! An agent task's reads of a workspace's apps, outside their apps
//! (`docs/designs/app-language/agents.md`, "Reading documents outside their app").
//!
//! The task reads each app's resources itself, from the workspace's store: every read goes to
//! the API as the task ([`StoreSource`]), which decides it for the task, and is answered by the
//! store; the task reads the answers with the app's own package, the same reads an app's host
//! answers: a resource's fields ([`Host::read_values`]) and a program's `grep`, `find`, `read`
//! and `describe` ([`tilolang_runtime::Runtime::answer_read`]). No app runs for it, so whether an
//! app's window is open never matters.
//!
//! What one program read is kept for that program only ([`TaskReads::forget`]): the next asks
//! the store again, so each read is decided anew and a grant revoked in the middle of a task cuts
//! what follows. What the task changed is in the store as soon as its invocation is answered, so
//! the next read holds it.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang_runtime::Actor;
use tilolang_runtime::reads::ReadRequest;

use crate::bundle::Bundle;
use crate::host::Host;
use crate::resources::ResourceStore;
use tilo_resource_store::Port;

/// Where a task reads an app's resources: the store, asked through the API as the task.
pub trait StoreSource {
    /// The port the reads of the resources the app `app` keeps in its record `record` (empty for
    /// the workspace's) go out on.
    fn port(&self, app: &str, record: &str) -> Rc<dyn Port>;
}

/// The apps a task reads, each with its package.
pub struct TaskReads {
    person: Option<String>,
    /// Each app's package, by its id.
    bundles: HashMap<String, Bundle>,
    /// The id whose resources an app keeps, when it is not its own: an app renamed keeps those of
    /// its earliest id.
    documents_under: BTreeMap<String, String>,
    source: Rc<dyn StoreSource>,
    /// The apps this program read, each by a host of its own.
    open: RefCell<HashMap<String, Host>>,
}

impl TaskReads {
    /// Reads of the apps `bundles` are the packages of, for `person`, asked of `source`.
    #[must_use]
    pub fn new(
        bundles: &[Bundle],
        documents_under: BTreeMap<String, String>,
        person: Option<&str>,
        source: Rc<dyn StoreSource>,
    ) -> Self {
        // The ids the session believes each app had before are the ones a task honours: a
        // package's own `previously:` is a claim the installed catalogue decides on.
        for (app, under) in &documents_under {
            crate::schema::register_previously(app, std::slice::from_ref(under));
        }
        Self {
            person: person.map(str::to_owned),
            bundles: bundles
                .iter()
                .map(|bundle| (bundle.meta.app.clone(), bundle.clone()))
                .collect(),
            documents_under,
            source,
            open: RefCell::default(),
        }
    }

    /// Where the app `app` keeps the resources a task reads: the id they are under and the
    /// workspace's record of the app.
    #[must_use]
    pub fn address(&self, app: &str) -> Option<(String, String)> {
        self.bundles.get(app)?;
        let under = self
            .documents_under
            .get(app)
            .cloned()
            .unwrap_or_else(|| app.to_owned());
        Some((under, String::new()))
    }

    /// Forget everything read: a new program asks the store again.
    pub fn forget(&self) {
        self.open.borrow_mut().clear();
    }

    /// Forget what was read of the app `app`: the program changed it.
    pub fn forget_app(&self, app: &str) {
        self.open.borrow_mut().remove(app);
    }

    /// The fields of the resource `item` of the app `app` ([`Host::read_values`]).
    ///
    /// # Errors
    /// The store cannot be read, or the item names nothing in it.
    pub fn read(&self, app: &str, item: &str) -> Result<Json, String> {
        self.with_host(app, |host| host.read_values(item))
    }

    /// What the app `app`'s resources answer to a program's `request`.
    ///
    /// # Errors
    /// The request is not one a program makes, or the store cannot be read.
    pub fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        let request: ReadRequest = serde_json::from_value(request.clone())
            .map_err(|error| format!("the request is not one a program makes: {error}"))?;
        self.with_host(app, |host| host.runtime().answer_read(&request))
    }

    /// What the action `call` (`{app, resource, action, arguments}`) would write, planned with
    /// the app's package over its resources as this program read them, for `caller`: the preview
    /// the app's origin compares its own run with. The origin runs the action again against the
    /// resources as they are then, so this decides nothing.
    ///
    /// # Errors
    /// The call names no action the app publishes, or it does not plan.
    pub fn plan(
        &self,
        call: &Json,
        caller: Option<&crate::publish::Caller>,
    ) -> Result<Json, String> {
        let app = call["app"].as_str().ok_or("a call names its app")?;
        let action = call["action"].as_str().ok_or("a call names its action")?;
        let prefix = crate::publish::resource_id(app, "");
        let resource = call["resource"]
            .as_str()
            .and_then(|id| id.strip_prefix(&prefix));
        let empty = serde_json::Map::new();
        let args = call["arguments"].as_object().unwrap_or(&empty);
        let published = call["resource"].as_str().unwrap_or_default();
        self.with_host(app, |host| {
            let action = host
                .published_action(published, action)
                .ok_or_else(|| format!("{app} publishes no action {action}"))?;
            let planned = host.plan(&crate::publish::PlanRequest {
                app,
                kind: "",
                action_id: &action.id,
                resource,
                args,
                caller,
                from: None,
            })?;
            serde_json::to_value(&planned).map_err(|error| error.to_string())
        })
    }

    fn with_host<T>(
        &self,
        app: &str,
        read: impl FnOnce(&Host) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(host) = self.open.borrow().get(app) {
            return read(host);
        }
        let host = self.open_host(app)?;
        let answer = read(&host);
        self.open.borrow_mut().insert(app.to_owned(), host);
        answer
    }

    /// A host of the app `app`'s package over its resources, as the store answers them now.
    fn open_host(&self, app: &str) -> Result<Host, String> {
        let (under, record) = self
            .address(app)
            .ok_or_else(|| format!("{app} is not installed"))?;
        let bundle = self
            .bundles
            .get(app)
            .ok_or_else(|| format!("{app} is not installed"))?;
        let store = ResourceStore::new(self.source.port(&under, &record));
        let viewer = Actor::Member(crate::runtime::ids::MemberId::person(
            self.person.clone().unwrap_or_default(),
        ));
        let host = Host::open(store, viewer, None, || None).map_err(|error| error.to_string())?;
        self.load(&host, bundle, &mut Vec::new())?;
        Ok(host)
    }

    /// Load `bundle` into `host`, after the installed packages it uses that it does not carry
    /// itself; `loaded` names those already in.
    fn load(&self, host: &Host, bundle: &Bundle, loaded: &mut Vec<String>) -> Result<(), String> {
        if loaded.contains(&bundle.meta.app) {
            return Ok(());
        }
        loaded.push(bundle.meta.app.clone());
        for used in &bundle.meta.uses {
            let carried = bundle.imports.iter().any(|import| import.meta.app == *used);
            if let Some(dependency) = self.bundles.get(used).filter(|_| !carried) {
                self.load(host, dependency, loaded)?;
            }
        }
        host.load_bundle(bundle)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}
