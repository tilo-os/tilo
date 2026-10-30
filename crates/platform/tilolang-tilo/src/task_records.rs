//! An agent task's reads of a room's records, outside their apps
//! (`docs/designs/app-language/agents.md`, "Reading records outside their app").
//!
//! The task reads each app's record itself: the door hands it the record's committed content
//! ([`RecordSource`]), read-only and decided for the task by the broker, and it reads that
//! content with the app's own package, the same reads an app's host answers: a resource's
//! fields ([`Host::read_values`]) and a program's `grep`, `find`, `read` and `describe`
//! ([`tilolang_runtime::Runtime::answer_read`]) over the record's index, built here. No app runs
//! for it, so whether an app's window is open never matters.
//!
//! A record is fetched once per program and kept for that program only: the next program
//! fetches it again, so each read is decided anew and a grant revoked in the middle of a task
//! cuts what follows. What the record's origin applied for the task is taken into the record
//! it read ([`RecordReads::take_in`]) and kept for the task's life: the program reads its own
//! changes at once, and every later program of the task reads them too, whether or not the copy
//! the door hands over holds them yet.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang_runtime::Principal;
use tilolang_runtime::reads::ReadRequest;

use crate::bundle::Bundle;
use crate::host::Host;

/// Where a task gets a record's committed content: the door, as the task.
pub trait RecordSource {
    /// The record `record` (empty for the app's record of the room) of the app `app`: its
    /// content (an Automerge document), or `None` when the room holds no such record.
    ///
    /// # Errors
    /// Why it cannot be read: the task may not, or the room's records are not here.
    fn record(&self, app: &str, record: &str) -> Result<Option<Vec<u8>>, String>;
}

/// The records a task reads, each with the package of its app.
pub struct RecordReads {
    person: Option<String>,
    /// Each app's package, by its id.
    bundles: HashMap<String, Bundle>,
    /// The id whose records an app keeps, when it is not its own: an app renamed keeps the
    /// record of its earliest id.
    records_under: BTreeMap<String, String>,
    source: Rc<dyn RecordSource>,
    /// The records this program read, by app, each with a host of its own over it.
    open: RefCell<HashMap<String, (tilo_resource::Doc, Host)>>,
    /// The apps whose record this program read as none: what the program saw of it (its root,
    /// say) was made by the read alone, never written anywhere.
    read_none: RefCell<std::collections::HashSet<String>>,
    /// What the records' origins applied for this task, by app, in order: kept for the task's
    /// life and put into every record the task reads, so each of its programs reads its own
    /// earlier changes while the copy the door hands over still lags the origin.
    written: RefCell<HashMap<String, Vec<Vec<u8>>>>,
}

impl RecordReads {
    /// Reads of the records of the apps `bundles` are the packages of, for `person`, fetched
    /// from `source`.
    #[must_use]
    pub fn new(
        bundles: &[Bundle],
        records_under: BTreeMap<String, String>,
        person: Option<&str>,
        source: Rc<dyn RecordSource>,
    ) -> Self {
        Self {
            person: person.map(str::to_owned),
            bundles: bundles
                .iter()
                .map(|bundle| (bundle.meta.app.clone(), bundle.clone()))
                .collect(),
            records_under,
            source,
            open: RefCell::default(),
            read_none: RefCell::default(),
            written: RefCell::default(),
        }
    }

    /// Where the app `app` keeps the records a task reads: the id they are under and the
    /// record, the person's own for an app whose records are each member's own.
    #[must_use]
    pub fn address(&self, app: &str) -> Option<(String, String)> {
        let bundle = self.bundles.get(app)?;
        let under = self
            .records_under
            .get(app)
            .cloned()
            .unwrap_or_else(|| app.to_owned());
        let record = self
            .person
            .as_deref()
            .and_then(|person| crate::own_record(&bundle.meta, person))
            .unwrap_or_default();
        Some((under, record))
    }

    /// Forget every record read: a new program reads them again, with what the task wrote.
    pub fn forget(&self) {
        self.open.borrow_mut().clear();
        self.read_none.borrow_mut().clear();
    }

    /// Forget the record of the app `app`: the program changed it.
    pub fn forget_app(&self, app: &str) {
        self.open.borrow_mut().remove(app);
        self.read_none.borrow_mut().remove(app);
    }

    /// Take in `change`, what the record's origin applied for this task to the record of the
    /// app `app`: what this program and every later one of the task reads holds it, without
    /// waiting for the room. A record this program has not read is read whole when it is next
    /// read, with the change in it.
    ///
    /// # Errors
    /// The change does not apply to the record.
    pub fn take_in(&self, app: &str, change: &[u8]) -> Result<(), String> {
        self.written
            .borrow_mut()
            .entry(app.to_owned())
            .or_default()
            .push(change.to_vec());
        // A record read as none holds only what the read made of it; the origin, which made the
        // record for the change, answers it whole, and that is the record from now on.
        if self.read_none.borrow_mut().remove(app) {
            let opened = self.host_over(app, Some(change))?;
            self.open.borrow_mut().insert(app.to_owned(), opened);
            return Ok(());
        }
        let mut open = self.open.borrow_mut();
        let Some((doc, host)) = open.get_mut(app) else {
            return Ok(());
        };
        tilo_automerge::ApplyIncoming::load_incoming(doc, change)
            .map_err(|error| format!("{app}'s change does not apply to its record: {error}"))?;
        host.sync_from(doc);
        Ok(())
    }

    /// The fields of the resource `item` of the app `app` ([`Host::read_values`]).
    ///
    /// # Errors
    /// The record cannot be read, or the item names nothing in it.
    pub fn read(&self, app: &str, item: &str) -> Result<Json, String> {
        self.with_host(app, |host| host.read_values(item))
    }

    /// What the app `app`'s record answers to a program's `request`.
    ///
    /// # Errors
    /// The record cannot be read, or the request is none a program makes.
    pub fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        let request: ReadRequest = serde_json::from_value(request.clone())
            .map_err(|error| format!("the request is not one a program makes: {error}"))?;
        self.with_host(app, |host| host.runtime().answer_read(&request))
    }

    /// What the action `call` (`{app, resource, action, arguments}`) would write, planned with
    /// the app's package over its record as this program read it, for `caller`: the preview the
    /// record's origin compares its own run with. The origin runs the action again against
    /// the record as it is then, so this decides nothing.
    ///
    /// # Errors
    /// The record cannot be read, or the action does not run on it (a requirement fails).
    pub fn plan(&self, call: &Json, caller: &crate::publish::Caller) -> Result<Json, String> {
        let app = call["app"].as_str().ok_or("a call names its app")?;
        let action = call["action"].as_str().ok_or("a call names its action")?;
        let prefix = crate::publish::resource_id(app, "");
        let resource = call["resource"]
            .as_str()
            .and_then(|id| id.strip_prefix(&prefix));
        let empty = serde_json::Map::new();
        let args = call["arguments"].as_object().unwrap_or(&empty);
        self.with_host(app, |host| {
            let planned = host.plan(&crate::publish::PlanRequest {
                app,
                kind: "",
                action_id: action,
                resource,
                args,
                caller: Some(caller),
            })?;
            serde_json::to_value(&planned).map_err(|error| error.to_string())
        })
    }

    fn with_host<T>(
        &self,
        app: &str,
        read: impl FnOnce(&Host) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some((_, host)) = self.open.borrow().get(app) {
            return read(host);
        }
        let (doc, host) = self.open_host(app)?;
        let answer = read(&host);
        self.open.borrow_mut().insert(app.to_owned(), (doc, host));
        answer
    }

    /// A host of the app `app`'s package over its record, as the door hands it now.
    fn open_host(&self, app: &str) -> Result<(tilo_resource::Doc, Host), String> {
        let (under, record) = self
            .address(app)
            .ok_or_else(|| format!("{app} is not installed"))?;
        let bytes = self.source.record(&under, &record)?;
        let written = self.written.borrow();
        let wrote = written.get(app).map_or(&[][..], Vec::as_slice);
        if bytes.is_none() && wrote.is_empty() {
            self.read_none.borrow_mut().insert(app.to_owned());
        }
        self.host_over_with(app, bytes.as_deref(), wrote)
    }

    /// A host of the app `app`'s package over its record `bytes` (none: an empty record).
    fn host_over(
        &self,
        app: &str,
        bytes: Option<&[u8]>,
    ) -> Result<(tilo_resource::Doc, Host), String> {
        self.host_over_with(app, bytes, &[])
    }

    /// [`Self::host_over`], the record holding what the task wrote to it (`wrote`, the changes
    /// its origin answered, in order) whether or not `bytes` holds it yet.
    fn host_over_with(
        &self,
        app: &str,
        bytes: Option<&[u8]>,
        wrote: &[Vec<u8>],
    ) -> Result<(tilo_resource::Doc, Host), String> {
        let bundle = self
            .bundles
            .get(app)
            .ok_or_else(|| format!("{app} is not installed"))?;
        let mut doc = match bytes {
            Some(bytes) => tilo_resource::Doc::load(bytes)
                .map_err(|error| format!("{app}'s record does not read: {error}"))?,
            None => tilo_resource::Doc::new(),
        };
        for change in wrote {
            tilo_automerge::ApplyIncoming::load_incoming(&mut doc, change).map_err(|error| {
                format!("what the task wrote to {app}'s record does not apply to it: {error}")
            })?;
        }
        let viewer = Principal::new(self.person.clone().unwrap_or_default());
        let host = Host::new(&doc, viewer, || None).map_err(|error| error.to_string())?;
        self.load(&host, bundle, &mut Vec::new())?;
        host.wait_index();
        Ok((doc, host))
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
