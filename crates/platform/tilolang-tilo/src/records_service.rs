//! A workspace's tilolang records as a session service: every record and item a loaded package
//! holds is a session resource, read through [`Service::read`] and changed through its actions.
//!
//! Nothing here is per app. The catalogue is the workspace's published trees, kept by the
//! incremental republisher ([`Publications`]), with each package's own actions beside them,
//! exactly as an app's instance publishes them on its connection. A resource's reading is its
//! fields as the runtime reads them, each by its field id, encoded as the runtime encodes a
//! value ([`tilolang_runtime::value::to_json`]); a reference in it names the resource it points
//! at by its published id (`tl:<app>:<id>`) and its type as `<app>#<Type>`. That is what a
//! runtime that loads the package as another app's
//! ([`tilolang_runtime::Runtime::load_foreign_package`]) reads back
//! ([`tilolang_runtime::Runtime::set_reading`]): the shell composes a note's own view over the
//! note's reading, and the reading is the record the notes app shows.
//!
//! An invocation is the published action's own: the door builds it through the action's spec,
//! which plans it in the runtime, and it is committed to the record as a step of the person
//! seated here.

use std::sync::Arc;

use serde_json::{Map, Value as Json};
use tilo_actions::{ActionAddress, ActionContext, ActionOutput, DoorError, ResourceSpec};
use tilo_service::{Service, Started};
use tilolang_runtime::value::ResourceRef;

use crate::action::{ACTION_TYPE, DynAction, EDIT_FLAG_ACTION, EDIT_TEXT_ACTION};
use crate::host::Host;
use crate::publish::{Planner, Publications, edit_action, publish_package, resource_id};

/// The workspace record's tilolang records, published to the session.
pub struct RecordService {
    record: tilo_resource::Doc,
    host: Host,
    planner: Arc<dyn Planner>,
    publications: Publications,
    /// Every field change since the last poll: a reading moves with any field of its resource,
    /// also one no published tree shows (a note's body).
    watch: crate::store::Watch,
    /// Who the changes made here are steps of.
    person: String,
    /// The resources whose readings moved since [`Service::moved`] was last asked, by their
    /// published ids, and whether something moved that names no one resource (the record was
    /// taken in anew, a document changed as a whole).
    moved: std::collections::BTreeSet<String>,
    moved_unsaid: bool,
}

impl RecordService {
    /// The records `host` runs over `record`, its packages loaded, published for `person`.
    #[must_use]
    pub fn new(record: tilo_resource::Doc, host: Host, person: impl Into<String>) -> Self {
        host.sync_from(&record);
        let planner: Arc<dyn Planner> = Arc::new(host.handle());
        let mut publications = Publications::new(&host);
        let watch = host.store().watch();
        publications.settle(&host, &planner);
        let _ = watch.take();
        Self {
            record,
            host,
            planner,
            publications,
            watch,
            person: person.into(),
            moved: std::collections::BTreeSet::new(),
            moved_unsaid: false,
        }
    }

    /// The workspace record as it is here.
    #[must_use]
    pub fn record(&self) -> &tilo_resource::Doc {
        &self.record
    }

    /// The runtime the records are read through.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Take in what another replica of the workspace record holds.
    ///
    /// # Errors
    /// The changes do not apply to the record.
    pub fn take_in(&mut self, other: &mut tilo_resource::Doc) -> Result<(), String> {
        use tilo_automerge::ApplyIncoming as _;
        self.record
            .merge_incoming(other)
            .map_err(|error| error.to_string())?;
        self.host.sync_from(&self.record);
        Ok(())
    }

    /// Have `wake` called (from another thread) when records built off the frame are ready for
    /// the next [`Service::poll`] to publish.
    pub fn set_waker(&mut self, wake: crate::background::Waker) {
        self.publications.set_waker(wake);
    }

    /// Whether a field of any record changed since the last call. What changed is kept for
    /// [`Service::moved`]: each resource a changed field is of, by its published id.
    fn watched(&mut self) -> bool {
        use tilolang_runtime::store::Store as _;
        let changes = self.watch.take();
        let any = changes.everything || !changes.whole.is_empty() || !changes.docs.is_empty();
        self.moved_unsaid |= changes.everything || !changes.whole.is_empty();
        // A resource that is gone has no app to name it by: what listed it moved too, and
        // names it no more.
        for key in changes.docs.values().flatten() {
            if let Some(app) = ResourceRef::parse_id(&key.resource, String::new())
                .and_then(|probe| self.host.store().type_of(&probe).ok())
                .and_then(|ty| crate::schema::lookup(&ty))
                .map(|schema| schema.app.clone())
            {
                self.moved.insert(resource_id(&app, &key.resource));
            }
        }
        any
    }

    /// The reading of the record or item `path` of `app`.
    fn reading(&self, app: &str, path: &str) -> Option<Json> {
        use tilolang_runtime::store::Store as _;
        let probe = ResourceRef::parse_id(path, String::new())?;
        let ty = self.host.store().type_of(&probe).ok()?;
        let schema = crate::schema::lookup(&ty)?;
        if schema.app != app {
            return None;
        }
        let at = ResourceRef::parse_id(path, ty)?;
        let runtime = self.host.runtime();
        let mut fields = Map::new();
        for field in &schema.fields {
            let Ok(value) = runtime.read(&at, &field.id) else {
                continue;
            };
            let Ok(mut json) = tilolang_runtime::value::to_json(&value) else {
                continue;
            };
            published_refs(&mut json);
            fields.insert(field.id.clone(), json);
        }
        Some(Json::Object(fields))
    }

    /// The direct edit (a bound control's) of the field `arguments` name in the record or item
    /// `resource`: what a view composed over its reading elsewhere sends when the person types
    /// into it. A record publishes these edits; an item is edited in its record by its id there
    /// ([`edit_action`]), as its own published resource carries only its type's actions.
    /// `None` for a resource that is not one of these records'.
    /// A flag (`flag`) may come as its text, as a door that knows no parameter type for it
    /// hands it on.
    fn direct_edit(
        &self,
        resource: &str,
        flag: bool,
        arguments: &Map<String, Json>,
    ) -> Option<Result<DynAction, String>> {
        use tilolang_runtime::store::Store as _;
        let (app, path) = resource.strip_prefix("tl:")?.split_once(':')?;
        let package = self.host.package(app)?;
        let probe = ResourceRef::parse_id(path, String::new())?;
        let ty = self.host.store().type_of(&probe).ok()?;
        let schema = crate::schema::lookup(&ty)?;
        let field = arguments
            .get("field")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let mut value = arguments.get("value").cloned().unwrap_or(Json::Null);
        if flag && let Some(said) = value.as_str() {
            match said.parse::<bool>() {
                Ok(set) => value = Json::Bool(set),
                Err(_) => return Some(Err(format!("`{said}` is not a flag"))),
            }
        }
        Some(
            self.host
                .with_schemas(|| edit_action(&package.meta, &schema.name, path, field, &value)),
        )
    }

    /// Commit `action` to the record as a step of the person seated here.
    fn commit(&mut self, action: &DynAction) -> Result<(), String> {
        let encoded = action.encode().map_err(|error| error.to_string())?;
        let context = ActionContext::for_resource(
            // The workspace root's path, as a grant's tree path names it (`room/app:todo/**`),
            // for an action that wrote no resource of its own.
            action.resources().first().copied().unwrap_or("room"),
            &action.kind,
            &action.kind,
        )
        .with_invoker(tilo_actions::ActionInvoker::actor(self.person.clone()));
        let record = &mut self.record;
        let person = self.person.as_bytes();
        self.host
            .with_schemas(|| {
                tilo_actions::apply_local_with_context(
                    record,
                    action,
                    encoded,
                    Some(person),
                    context,
                )
            })
            .map_err(|error| error.to_string())?;
        self.record.commit();
        self.host.sync_from(&self.record);
        Ok(())
    }
}

/// Each reference in `json` named as the session publishes it: by its published id, and its
/// type as `<app>#<Type>`.
fn published_refs(json: &mut Json) {
    match json {
        Json::Object(fields) => {
            let named = fields
                .get("ref")
                .and_then(Json::as_str)
                .zip(fields.get("ty").and_then(Json::as_str))
                .and_then(|(id, ty)| {
                    let schema = crate::schema::lookup(ty)?;
                    Some((
                        resource_id(&schema.app, id),
                        format!("{}#{}", schema.app, schema.name),
                    ))
                });
            if let Some((id, ty)) = named {
                fields.insert("ref".into(), Json::String(id));
                fields.insert("ty".into(), Json::String(ty));
            }
            for nested in fields.values_mut() {
                published_refs(nested);
            }
        }
        Json::Array(items) => items.iter_mut().for_each(published_refs),
        _ => {}
    }
}

impl Service for RecordService {
    fn seated(&mut self, person: &str, _earlier: &[String]) {
        person.clone_into(&mut self.person);
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        // Each record is its package's, as an app's connection says of what it publishes: what
        // places it on the canvas shows it through that app.
        let mut out: Vec<ResourceSpec> = self
            .publications
            .trees()
            .values()
            .map(|tree| {
                let mut tree = (**tree).clone();
                kept_by_its_app(&mut tree);
                tree
            })
            .collect();
        for package in self.host.packages() {
            let resource = publish_package(&package.meta, &self.planner);
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
    }

    fn read(&self, resource: &str) -> Option<Json> {
        let (app, path) = resource.strip_prefix("tl:")?.split_once(':')?;
        self.host.package(app)?;
        self.reading(app, path)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<Started, DoorError> {
        let address = ActionAddress::new(resource, action);
        let planned = if action == EDIT_TEXT_ACTION || action == EDIT_FLAG_ACTION {
            self.direct_edit(resource, action == EDIT_FLAG_ACTION, arguments)
                .ok_or_else(|| DoorError::Unknown(address.clone()))?
                .map_err(DoorError::InvalidArguments)?
        } else {
            let encoded = tilo_actions::door::encode_json(&self.resources(), &address, arguments)?;
            if encoded.type_id != ACTION_TYPE {
                return Err(DoorError::Unknown(address));
            }
            serde_json::from_slice(&encoded.payload)
                .map_err(|error| DoorError::InvalidArguments(error.to_string()))?
        };
        self.commit(&planned).map_err(DoorError::Failed)?;
        // What it made is published before it is answered, so the caller's next invocation
        // reaches it: a record changed as a whole is built now, not off the frame.
        self.publications.settle(&self.host, &self.planner);
        let made = self
            .host
            .take_returned(&planned.action_id)
            .map(|made| resource_id(&planned.app, &made.id_string()));
        Ok(Started::Done(ActionOutput {
            value: None,
            resources: made.into_iter().collect(),
        }))
    }

    fn poll(&mut self) -> bool {
        let touched = self.publications.update(&self.host, &self.planner);
        self.watched() || !touched.is_empty()
    }

    fn moved(&mut self) -> Option<Vec<String>> {
        self.watched();
        let moved = std::mem::take(&mut self.moved);
        if std::mem::take(&mut self.moved_unsaid) {
            return None;
        }
        Some(moved.into_iter().collect())
    }

    fn module(&self, app: &str) -> Option<std::sync::Arc<[u8]>> {
        self.host.package(app).map(|package| package.wasm)
    }

    fn settle(&mut self) {
        self.publications.settle(&self.host, &self.planner);
    }
}

/// Name the app that keeps `resource` and each of its children, by its published id
/// (`tl:<app>:<id>`), where it names none yet.
fn kept_by_its_app(resource: &mut ResourceSpec) {
    if resource.app_id.is_none()
        && let Some((app, _)) = resource
            .id
            .as_str()
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
    {
        resource.app_id = Some(app.to_owned());
    }
    for child in &mut resource.children {
        kept_by_its_app(child);
    }
}

/// The type a resource of the kind `kind` (`tl.<app>.<Type>`, as [`RecordService`] publishes
/// it) is, as a runtime that loads its package as another app's names it: `<app>#<Type>`.
/// `None` for a kind no tilolang record has.
#[must_use]
pub fn reading_type(kind: &str) -> Option<String> {
    let (app, ty) = kind.strip_prefix("tl.")?.rsplit_once('.')?;
    Some(format!("{app}#{ty}"))
}
