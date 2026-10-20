//! Publishing tilolang resources on the action door (implementation design 7.4).
//!
//! Every resource in the room record is published as a `tilo_actions::Resource` of kind
//! `tl.<app>.<Type>`, with one typed action per tilolang action whose subject is its type. The
//! desktop agent, the runtime door, shortcuts and grants therefore reach tilolang actions with
//! no agent-side code. The build closure of each action runs the wasm **planner** and encodes
//! the resulting [`DynAction`].

use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::resource::INVALID_ARGUMENTS_TYPE;
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, EncodedAction, ExecutionLocus, ParamType, Resource,
};

use crate::action::DynAction;
use tilolang_runtime::meta::{ActionMeta, Meta, ParamMeta};

use crate::meta::{MetaExt as _, action_kind};
use crate::surface::{arguments, schema_of_type};

/// Who is invoking: what the door admitted of the caller, for the runtime's principal.
///
/// An agent is a task of a person under that person's narrowed rights, and the door tells the
/// host which task (the delegate's id) and for whom. A person at the window is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated principal: a person, or an agent task's delegate.
    pub actor: String,
    /// Whether the actor is software acting for a person.
    pub agent: bool,
    /// The person an agent acts for.
    pub on_behalf_of: Option<String>,
}

/// Where the planner reads the caller of the invocation being built, on the thread that builds it.
pub type CallerSource = dyn Fn() -> Option<Caller> + Send + Sync;

/// What the planner is asked to run.
#[derive(Clone, Debug)]
pub struct PlanRequest<'a> {
    pub app: &'a str,
    /// The kind the action was invoked on.
    pub kind: &'a str,
    pub action_id: &'a str,
    /// The subject resource's id, when the action has one.
    pub resource: Option<&'a str>,
    /// The arguments beyond the subject, by parameter name.
    pub args: &'a serde_json::Map<String, Json>,
    /// Who invokes, when it is not the person at the window.
    pub caller: Option<&'a Caller>,
}

/// Runs a tilolang action as a planner and records what it wrote.
///
/// The implementation is the host's runtime: a transaction over a copy of the room record
/// collects the module's mutations as edits, and nothing is committed here.
pub trait Planner: Send + Sync {
    /// # Errors
    /// Why the action cannot run: a trap, a refused argument, an unknown action.
    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String>;

    /// The caller of the invocation being built now, as the door admitted it.
    fn caller(&self) -> Option<Caller> {
        None
    }
}

/// Hands out an action the runtime already committed, for the executor to commit.
pub trait Committer: Send + Sync {
    /// The encoded action queued under `ticket`.
    ///
    /// # Errors
    /// Nothing is queued under the ticket.
    fn encoded(&self, ticket: u64) -> Result<EncodedAction, String>;
}

/// The action id of the host action that commits a queued action.
pub const COMMIT: &str = "commit";

/// The id of the published resource for the tilolang resource `id` of `app`.
#[must_use]
pub fn resource_id(app: &str, id: &str) -> String {
    format!("tl:{app}:{id}")
}

/// The id of a package's own resource, for its record-less actions.
#[must_use]
pub fn package_resource_id(app: &str) -> String {
    format!("tl:{app}")
}

/// The id of the host resource.
pub const HOST_RESOURCE: &str = "tl:host";

fn param_type(meta: &Meta, ty: &str) -> ParamType {
    let ty = ty.trim();
    let ty = ty
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .map_or(ty, str::trim);
    match ty {
        "Int" => ParamType::Integer,
        "Float" => ParamType::Number,
        "Bool" => ParamType::Boolean,
        "Text" => ParamType::Text,
        other if meta.type_named(other).is_some() => ParamType::Text,
        other if other.starts_with("Map<") => ParamType::Object,
        _ => ParamType::Array,
    }
}

fn action_param(meta: &Meta, param: &ParamMeta) -> ActionParam {
    let ty = param_type(meta, &param.ty);
    let description = format!("{}: {}", param.name, param.ty);
    let optional = param.ty.trim().starts_with("Option<");
    let built = if optional {
        ActionParam::optional_owned(param.name.clone(), ty, description)
    } else {
        ActionParam::required_owned(param.name.clone(), ty, description)
    };
    built.with_schema(schema_of_type(meta, &param.ty))
}

/// The typed action spec of `action`, whose build runs `planner`.
///
/// `subject` is the id of the resource the action is published on, when it has one.
#[must_use]
pub fn action_spec(
    meta: &Meta,
    kind: &str,
    subject: Option<&str>,
    action: &ActionMeta,
    planner: &Arc<dyn Planner>,
) -> ActionSpec {
    let params: Vec<ActionParam> = arguments(meta, &action.params)
        .iter()
        .map(|param| action_param(meta, param))
        .collect();
    let (app, kind, id) = (meta.app.clone(), kind.to_owned(), action.id.clone());
    let subject = subject.map(str::to_owned);
    let planner = Arc::clone(planner);
    let build = move |args: &ActionArgs| {
        let json = args.to_json();
        let caller = planner.caller();
        let request = PlanRequest {
            app: &app,
            kind: &kind,
            action_id: &id,
            resource: subject.as_deref(),
            args: &json,
            caller: caller.as_ref(),
        };
        planner
            .plan(&request)
            .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
            .unwrap_or_else(|message| EncodedAction {
                type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: message.into_bytes(),
            })
    };
    let placeholder = if action.doc.is_empty() {
        action.name.clone()
    } else {
        action.doc.clone()
    };
    let mut spec = ActionSpec::typed(
        action.id.clone(),
        action.name.clone(),
        placeholder,
        ExecutionLocus::ClientLocal,
        params,
        build,
    )
    .of_kind(action_kind(action.class));
    if !action.doc.is_empty() {
        spec = spec.with_description(action.doc.clone());
    }
    spec
}

/// The resource `id` of the type `type_name`, published with the actions of that type.
#[must_use]
pub fn publish_record(
    meta: &Meta,
    type_name: &str,
    id: &str,
    label: &str,
    planner: &Arc<dyn Planner>,
) -> Resource {
    let kind = meta.kind_of(type_name);
    let mut resource =
        Resource::new(resource_id(&meta.app, id), kind.clone(), label).with_action(open_spec(id));
    for spec in edit_specs(meta, type_name, id) {
        resource = resource.with_action(spec);
    }
    for action in &meta.actions {
        let on_type = meta
            .subject_of(&action.params)
            .is_some_and(|subject| subject.name == type_name);
        if on_type {
            resource = resource.with_action(action_spec(meta, &kind, Some(id), action, planner));
        }
    }
    resource
}

/// The `open` view action of the published resource `anchor`: what a search hit invokes.
fn open_spec(anchor: &str) -> ActionSpec {
    use tilo_resource::Effect as _;
    crate::effects::OpenEffect::Open {
        anchor: anchor.to_owned(),
    }
    .bound()
}

/// The resource of a root record with the items of its collections as child resources, each
/// carrying the actions whose subject is its type (a draft's `send`, a note's `toggle_pin`).
///
/// An item's resource id is the root's with the path to the item (`doc/field:item`), the same
/// form an action takes an item as an argument in, so an agent that surveys the tree calls an
/// item's verbs directly.
#[must_use]
pub fn publish_tree(
    meta: &Meta,
    record: &crate::record::DynRecord,
    type_name: &str,
    doc: &str,
    label: &str,
    planner: &Arc<dyn Planner>,
) -> Resource {
    let mut resource = publish_record(meta, type_name, doc, label, planner);
    for child in item_resources(meta, record, doc, planner) {
        resource = resource.with_child(child);
    }
    resource
}

fn item_resources(
    meta: &Meta,
    record: &crate::record::DynRecord,
    parent: &str,
    planner: &Arc<dyn Planner>,
) -> Vec<Resource> {
    use crate::record::DynValue;
    let mut out = Vec::new();
    for field in &record.schema().fields {
        let Some(DynValue::Collection(items)) = record.get(&field.id) else {
            continue;
        };
        for (item_id, item) in items.iter() {
            let path = format!("{parent}/{}:{}", field.id, item_id.as_str());
            out.push(item_resource(meta, item, &path, planner));
        }
    }
    out
}

/// The item `item` at `path`, published with its type's actions and its own items.
fn item_resource(
    meta: &Meta,
    item: &crate::record::DynRecord,
    path: &str,
    planner: &Arc<dyn Planner>,
) -> Resource {
    let type_name = item.schema().name.clone();
    let kind = meta.kind_of(&type_name);
    let label = item_label(item).unwrap_or_else(|| type_name.clone());
    let mut resource = Resource::new(resource_id(&meta.app, path), kind.clone(), label)
        .with_action(open_spec(path));
    for action in &meta.actions {
        let on_type = meta
            .subject_of(&action.params)
            .is_some_and(|subject| subject.name == type_name);
        if on_type {
            resource = resource.with_action(action_spec(meta, &kind, Some(path), action, planner));
        }
    }
    for child in item_resources(meta, item, path, planner) {
        resource = resource.with_child(child);
    }
    resource
}

/// What a room publishes of its records, kept up to date by republishing only what changed.
///
/// A record whose item changed is republished by rebuilding that item (its top-level item, which
/// names it) in the record's cached tree; items added or removed are added to or removed from it.
/// Only a record that appeared, or one that changed as a whole, is read and built in full.
pub struct Publications {
    watch: crate::store::Watch,
    trees: std::collections::BTreeMap<String, Resource>,
    /// Per record, the resource whose text names it now.
    named_by: std::collections::BTreeMap<String, Option<String>>,
}

impl Publications {
    /// Nothing published yet; the first [`Self::update`] publishes every record.
    #[must_use]
    pub fn new(host: &crate::host::Host) -> Self {
        Self {
            watch: host.store().watch(),
            trees: std::collections::BTreeMap::new(),
            named_by: std::collections::BTreeMap::new(),
        }
    }

    /// The published records, by document id.
    #[must_use]
    pub fn trees(&self) -> &std::collections::BTreeMap<String, Resource> {
        &self.trees
    }

    /// Republish what changed since the last update. Returns the documents whose published tree
    /// changed (or went away).
    pub fn update(&mut self, host: &crate::host::Host, planner: &Arc<dyn Planner>) -> Vec<String> {
        use std::collections::BTreeSet;

        use tilolang_runtime::store::ANY_FIELD;

        let changes = self.watch.take();
        let mut touched = Vec::new();
        let records = host.records();
        let now: BTreeSet<&str> = records.iter().map(|(_, _, doc)| doc.as_str()).collect();
        let gone: Vec<String> = self
            .trees
            .keys()
            .filter(|doc| !now.contains(doc.as_str()))
            .cloned()
            .collect();
        for doc in gone {
            self.trees.remove(&doc);
            self.named_by.remove(&doc);
            touched.push(doc);
        }
        for (package, type_name, doc) in &records {
            let Some(schema) = package
                .meta
                .types
                .iter()
                .find(|t| &t.name == type_name)
                .and_then(|t| crate::schema::lookup(&t.id))
            else {
                continue;
            };
            let at = Published {
                store: host.store(),
                meta: &package.meta,
                schema,
                doc,
                planner,
            };
            let whole = changes.everything
                || changes.whole.contains(doc)
                || !self.trees.contains_key(doc)
                || changes.docs.get(doc).is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|key| key.resource == *doc && key.field == ANY_FIELD)
                });
            if whole {
                let (label, named_by) = at.label();
                self.trees.insert(doc.clone(), at.whole(type_name, &label));
                self.named_by.insert(doc.clone(), named_by);
                touched.push(doc.clone());
                continue;
            }
            let (Some(fields), Some(tree)) = (changes.docs.get(doc), self.trees.get_mut(doc))
            else {
                continue;
            };
            let named_by = self.named_by.get(doc).cloned().flatten();
            let mut rework = at.rework(fields, named_by.as_deref());
            for field in &rework.collections {
                at.membership(tree, field, &mut rework.items);
            }
            at.rebuild(tree, &rework.items);
            if rework.relabel {
                let (label, named_by) = at.label();
                tree.label = label;
                self.named_by.insert(doc.clone(), named_by);
            }
            touched.push(doc.clone());
        }
        touched
    }
}

/// What to rebuild of a published record.
struct Rework {
    /// The top-level items that changed.
    items: std::collections::BTreeSet<String>,
    /// The record's collections whose membership or order changed.
    collections: std::collections::BTreeSet<String>,
    /// Whether the text that names the record may have changed.
    relabel: bool,
}

/// One record being republished.
struct Published<'a> {
    store: &'a crate::store::TiloStore,
    meta: &'a Meta,
    schema: &'static crate::schema::TypeSchema,
    doc: &'a str,
    planner: &'a Arc<dyn Planner>,
}

impl Published<'_> {
    fn root(&self) -> tilolang_runtime::value::ResourceRef {
        tilolang_runtime::value::ResourceRef::root(self.doc, self.schema.id.clone())
    }

    /// The text that names the record, and the resource that holds it.
    fn label(&self) -> (String, Option<String>) {
        let named = self.store.first_text(&self.root());
        let label = named
            .as_ref()
            .map_or_else(|| self.doc.to_owned(), |(text, _)| text.clone());
        (label, named.map(|(_, at)| at))
    }

    /// The record published in full.
    fn whole(&self, type_name: &str, label: &str) -> Resource {
        match self.store.record(self.doc) {
            Ok(record) => {
                publish_tree(self.meta, &record, type_name, self.doc, label, self.planner)
            }
            Err(_) => publish_record(self.meta, type_name, self.doc, label, self.planner),
        }
    }

    fn rework(
        &self,
        fields: &std::collections::BTreeSet<tilolang_runtime::store::FieldKey>,
        named_by: Option<&str>,
    ) -> Rework {
        let mut rework = Rework {
            items: std::collections::BTreeSet::new(),
            collections: std::collections::BTreeSet::new(),
            relabel: false,
        };
        for key in fields {
            if key.resource == self.doc {
                let kind = self.schema.field(&key.field).map(|f| f.kind);
                if kind == Some(crate::schema::MergeKind::Collection) {
                    rework.collections.insert(key.field.clone());
                }
                rework.relabel = true;
            } else if let Some(top) = top_item(self.doc, &key.resource) {
                if named_by.is_some_and(|at| at == top || at.starts_with(&format!("{top}/"))) {
                    rework.relabel = true;
                }
                rework.items.insert(top);
            }
        }
        rework
    }

    /// Bring the children of `tree` from the collection `field` in line with its items: keep the
    /// unchanged ones, build the new and the changed ones (taking them out of `items`).
    fn membership(
        &self,
        tree: &mut Resource,
        field: &str,
        items: &mut std::collections::BTreeSet<String>,
    ) {
        use tilolang_runtime::store::Store as _;

        let (doc, app) = (self.doc, &self.meta.app);
        let spec = tilolang_runtime::store::FieldSpec {
            id: field.to_owned(),
            kind: tilolang_runtime::meta::FieldKind::Collection,
        };
        let ids = match self.store.read(&self.root(), &spec) {
            Ok(tilolang_runtime::store::FieldValue::Items(ids)) => ids,
            _ => Vec::new(),
        };
        let prefix = resource_id(app, &format!("{doc}/{field}:"));
        let mut held = std::collections::BTreeMap::new();
        let mut others = Vec::new();
        for child in std::mem::take(&mut tree.children) {
            if child.id.as_str().starts_with(&prefix) {
                held.insert(child.id.as_str().to_owned(), child);
            } else {
                others.push(child);
            }
        }
        let mut members = Vec::with_capacity(ids.len());
        for id in ids {
            let path = format!("{doc}/{field}:{id}");
            match held.remove(&resource_id(app, &path)) {
                Some(child) if !items.contains(&path) => members.push(child),
                _ => {
                    items.remove(&path);
                    members.extend(self.item(&path));
                }
            }
        }
        // The record's collections in its fields' order, as `publish_tree` has them.
        let mut ordered = Vec::with_capacity(others.len().saturating_add(members.len()));
        let mut members = Some(members);
        for f in &self.schema.fields {
            if f.kind != crate::schema::MergeKind::Collection {
                continue;
            }
            if f.id == field {
                ordered.extend(members.take().unwrap_or_default());
            } else {
                let p = resource_id(app, &format!("{doc}/{}:", f.id));
                ordered.extend(
                    others
                        .iter()
                        .filter(|c| c.id.as_str().starts_with(&p))
                        .cloned(),
                );
            }
        }
        tree.children = ordered;
    }

    /// Rebuild the top-level items at `paths` in place; drop those gone.
    fn rebuild(&self, tree: &mut Resource, paths: &std::collections::BTreeSet<String>) {
        for path in paths {
            let published = resource_id(&self.meta.app, path);
            let at = tree
                .children
                .iter()
                .position(|child| child.id.as_str() == published);
            match (at, self.item(path)) {
                (Some(i), Some(child)) => {
                    if let Some(slot) = tree.children.get_mut(i) {
                        *slot = child;
                    }
                }
                (Some(i), None) => {
                    tree.children.remove(i);
                }
                // A new item arrives with its collection's membership.
                (None, _) => {}
            }
        }
    }

    /// The item at `path`, read from the store and published.
    fn item(&self, path: &str) -> Option<Resource> {
        let at = tilolang_runtime::value::ResourceRef::parse_id(path, self.schema.id.clone())?;
        let item = self.store.record_at(&at).ok()?;
        Some(item_resource(self.meta, &item, path, self.planner))
    }
}

/// The top-level item an item path is in: `doc/field:item`.
fn top_item(doc: &str, path: &str) -> Option<String> {
    let rest = path.strip_prefix(doc)?.strip_prefix('/')?;
    let first = rest.split('/').next()?;
    Some(format!("{doc}/{first}"))
}

/// What names an item: the first of its texts.
fn item_label(item: &crate::record::DynRecord) -> Option<String> {
    let mut text = Vec::new();
    tilo_resource::Model::collect_text(item, &mut text);
    text.into_iter().next()
}

/// The package's own resource, carrying the actions that take no record.
#[must_use]
pub fn publish_package(meta: &Meta, planner: &Arc<dyn Planner>) -> Resource {
    let kind = meta.package_kind();
    let mut resource = Resource::new(package_resource_id(&meta.app), kind.clone(), &meta.title);
    for action in &meta.actions {
        if meta.subject_of(&action.params).is_none() {
            resource = resource.with_action(action_spec(meta, &kind, None, action, planner));
        }
    }
    resource
}

/// The host's own resource: the commit of an action the runtime planned while a window handled a
/// press. It is not offered in the palette; the window invokes it.
///
/// It also carries the [`HostEffect`] vocabulary an agent uses: run a block, run a flow, read
/// the catalogue. The window answers them with `Host::answer`.
#[must_use]
pub fn host_resource(committer: &Arc<dyn Committer>) -> Resource {
    let committer = Arc::clone(committer);
    let commit = ActionSpec::typed(
        COMMIT,
        "Commit a planned action",
        "ticket",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "ticket",
            ParamType::Integer,
            "The ticket of the planned action",
        )],
        move |args| {
            args.integer("ticket")
                .and_then(|ticket| u64::try_from(ticket).ok())
                .ok_or_else(|| "the ticket is not a number".to_owned())
                .and_then(|ticket| committer.encoded(ticket))
                .unwrap_or_else(|message| EncodedAction {
                    type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                    payload: message.into_bytes(),
                })
        },
    )
    .programmatic();
    let mut host =
        Resource::new(HOST_RESOURCE, crate::surface::HOST_KIND, "tilolang").with_action(commit);
    for spec in <crate::effects::HostEffect as tilo_resource::Effect>::specs() {
        host = host.with_action(spec);
    }
    host
}

/// The plain edit a bound control makes: set `value` into the field `field` of the resource
/// `id` of the type `type_name`. It needs no planner, since it is not module code.
///
/// # Errors
/// The type or field is not registered, or the value does not fit the field.
pub fn edit_action(
    meta: &Meta,
    type_name: &str,
    id: &str,
    field: &str,
    value: &Json,
) -> Result<DynAction, String> {
    use crate::schema::{MergeKind, lookup};
    use tilo_resource::{Edit, EditOp, Segment};

    let declared = meta
        .type_named(type_name)
        .ok_or_else(|| format!("the package declares no type `{type_name}`"))?;
    let schema =
        lookup(&declared.id).ok_or_else(|| format!("the type `{type_name}` is not registered"))?;
    let schema_field = schema
        .field(field)
        .ok_or_else(|| format!("the type `{type_name}` has no field `{field}`"))?;
    let op = match (schema_field.kind, value) {
        (MergeKind::Text, Json::String(text)) => EditOp::Replace { text: text.clone() },
        (MergeKind::Register, value) => EditOp::Set {
            value: value.clone(),
        },
        _ => return Err(format!("the field `{field}` cannot be set to that value")),
    };
    let action_id = if value.is_boolean() {
        crate::action::EDIT_FLAG_ACTION
    } else {
        crate::action::EDIT_TEXT_ACTION
    };
    Ok(DynAction {
        app: meta.app.clone(),
        kind: meta.kind_of(type_name),
        action_id: action_id.to_owned(),
        args: serde_json::json!({"field": field, "value": value}),
        authorship: crate::action::Authorship {
            module: meta.module_hash.clone(),
            ..crate::action::Authorship::default()
        },
        edits: vec![crate::action::DynEdit::Edit {
            id: id.to_owned(),
            edit: Edit::new(vec![Segment::Field(field.to_owned())], op),
        }],
    })
}

/// The two direct edit actions of the resource `id`: a bound text input and a bound checkbox.
#[must_use]
pub fn edit_specs(meta: &Meta, type_name: &str, id: &str) -> [ActionSpec; 2] {
    let make = |action: &'static str, label: &'static str, ty: ParamType| {
        let (meta, type_name, id) = (meta.clone(), type_name.to_owned(), id.to_owned());
        ActionSpec::typed(
            action,
            label,
            label,
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("field", ParamType::Text, "The field id"),
                ActionParam::required("value", ty, "The new value"),
            ],
            move |args: &ActionArgs| {
                let json = args.to_json();
                let field = json.get("field").and_then(Json::as_str).unwrap_or_default();
                let value = json.get("value").cloned().unwrap_or(Json::Null);
                edit_action(&meta, &type_name, &id, field, &value)
                    .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
                    .unwrap_or_else(|message| EncodedAction {
                        type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                        payload: message.into_bytes(),
                    })
            },
        )
    };
    [
        make(
            crate::action::EDIT_TEXT_ACTION,
            "Edit a text field",
            ParamType::Text,
        ),
        make(
            crate::action::EDIT_FLAG_ACTION,
            "Set a flag field",
            ParamType::Boolean,
        ),
    ]
}
