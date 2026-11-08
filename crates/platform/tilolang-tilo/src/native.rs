//! A native app's records, read by the app itself.
//!
//! A native (Rust) app keeps its record in a layout of its own: its model's keys, its unit
//! sections, its tables. Nothing outside the app knows that layout, and nothing here tries to.
//! A read of a native record asks the app: its own `read` action, the
//! one thing that turns its record into typed data, invoked as the task's caller, which the
//! broker decides and for which it starts the app when none of it runs ([`Calls`]). That is the
//! read a person's window and an agent's `tilo_read` take too, so one app code over one record
//! answers all of them, and an indexer filling a task's search from records takes the same
//! path.
//!
//! The app's answer is its model's JSON. Its shape is the app's, not the language's: a set is an
//! object of members or a list, a collection is a list of items without their ids, rich text is
//! markdown. So this module reads each field as the app's surface declares it ([`crate::schema`]:
//! the package [`crate::catalogue`] derived from the app's `surface.toml`), and a program sees a
//! field of the type its package names: `issue.assignee` is the text the issue holds there,
//! `issue.labels` a set, `issue.body` rich text.
//!
//! What an app holds, a query asks the app too: the resources under its record as its directory
//! lists them ([`Calls::children`]), which is the listing `tilo_read` shows. A read addresses a
//! resource by the id the directory lists it by: the resource's own, which answers a read of
//! itself when it offers one ([`Calls::reads`]), else the record's, which takes the resource as
//! its `item` by the key the directory lists it by. That precedence is `tilo_read`'s
//! ([`Calls::reads`] answers what its directory search reads), so both read an app the same
//! way. No byte of a record is read here and no field is guessed from a layout.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value as Json, json};
use tilolang_runtime::foreign::Shown;
use tilolang_runtime::meta::{Meta, PackageMeta, Ty, TypeKey};
use tilolang_runtime::query::Order;
use tilolang_runtime::reads::{ReadAnswer, ReadRequest};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::store::{
    FieldRead, FieldValue, Lit, Pred, Scalar, StoreError, lit_of, ordered_cmp,
};
use tilolang_runtime::value::split_id;

use crate::bundle::Bundle;
use crate::reads::{default_value, element_key, field_of_json, scalar_of_json};
use crate::schema::{FieldSchema, MergeKind, TypeSchema, lookup};
use crate::task::{Answered, field_json};

/// The native apps a task reaches: each app whose code the task does not run
/// ([`crate::catalogue::described`] reads what such an app publishes, not how it runs), with the
/// record it publishes. A program names that record, invokes the app's verbs on it and assigns
/// its fields; what the record holds it reads through the app ([`NativeReads`]).
pub struct Native {
    /// The app's package, by app id: what its surface declares.
    apps: BTreeMap<String, PackageMeta>,
    /// The record each app publishes, by app id (`chat.workspace`): the root its catalogue
    /// names. An app whose surface publishes no root has no record a program reads.
    records: BTreeMap<String, String>,
}

impl Native {
    /// The native apps among `native` that the bundles here do not carry (an app the session
    /// runs is the one that counts), and the record each publishes.
    #[must_use]
    pub fn of(bundles: &[Bundle], native: &[PackageMeta]) -> Self {
        let carried: BTreeSet<&str> = bundles
            .iter()
            .flat_map(|bundle| {
                bundle
                    .imports
                    .iter()
                    .map(|import| import.meta.app.as_str())
                    .chain(std::iter::once(bundle.meta.app.as_str()))
            })
            .collect();
        let mut apps = BTreeMap::new();
        let mut records = BTreeMap::new();
        for meta in native
            .iter()
            .filter(|meta| !carried.contains(meta.app.as_str()))
        {
            if let Some(record) = published_root(meta) {
                records.insert(meta.app.clone(), record);
            }
            apps.insert(meta.app.clone(), meta.clone());
        }
        Self { apps, records }
    }

    /// Whether `app` is a native one.
    #[must_use]
    pub fn is(&self, app: &str) -> bool {
        self.apps.contains_key(app)
    }

    /// The record `app` publishes.
    #[must_use]
    pub fn record(&self, app: &str) -> Option<&str> {
        self.records.get(app).map(String::as_str)
    }

    /// The app's package: what its surface declares.
    #[must_use]
    pub fn meta(&self, app: &str) -> Option<&PackageMeta> {
        self.apps.get(app)
    }

    /// The packages of the native apps, in app order: what a session loads of them.
    pub fn packages(&self) -> impl Iterator<Item = &PackageMeta> {
        self.apps.values()
    }

    /// The type of `app`'s record, as its package names it (`so.tilo.chat.Workspace`).
    fn record_type(&self, app: &str) -> Option<&str> {
        let root = self
            .meta(app)?
            .named_roots
            .iter()
            .find(|root| root.published.is_some())?;
        Some(root.ty.as_str())
    }
}

/// The record `meta` publishes as its root, when it publishes one.
fn published_root(meta: &PackageMeta) -> Option<String> {
    meta.named_roots
        .iter()
        .find_map(|root| root.published.clone())
}

/// What a native app answers through: an invocation of it on its record, as the task's caller,
/// and the resources under one of its resources, as its directory lists them.
pub trait Calls {
    /// Invoke `call` (`{app, resource, action, arguments}`) as the task.
    fn call(&self, call: &Json) -> Answered;

    /// The resources under `resource` of `app`, as the app's directory lists them, each
    /// `{id, kind, key, name}`, `resource` empty at the top of the directory: a resource's `id`
    /// names it under its parent, and its `key` is the app's own id for it, which its `read`
    /// takes as an item (`<record>/<field>/<item>`, the steps its own read parses).
    ///
    /// # Errors
    /// Why the app lists nothing: it is not installed, it does not run, the task may not see it.
    fn children(&self, app: &str, resource: &str) -> Result<Vec<Json>, String>;

    /// Whether the resource `resource` of `app` answers a read of itself: whether the app's
    /// directory offers a read on it. A resource that does not is read by the record, which
    /// takes it as its `item` — the read a window and `tilo_read` take too.
    fn reads(&self, app: &str, resource: &str) -> Result<bool, String>;
}

/// A resource of a native app as its directory lists it, and how a read of it is addressed.
struct Row {
    /// The id a program holds the resource by (`tasks.tracker/abc`).
    held: String,
    /// The id the directory lists the resource as, whose own read answers it when it reads
    /// itself.
    id: String,
    /// The id the directory lists the record as, whose read takes a resource under it as its
    /// `item`.
    under: String,
    /// What that read takes as its `item`: the key the directory lists the resource by;
    /// `None` for the record itself, which no other resource reads.
    key: Option<String>,
}

/// Reads of a native app's records: the app's own read of a resource, and a query over the
/// resources its directory lists.
pub struct NativeReads<'a> {
    native: &'a Native,
    calls: &'a dyn Calls,
}

impl<'a> NativeReads<'a> {
    #[must_use]
    pub const fn new(native: &'a Native, calls: &'a dyn Calls) -> Self {
        Self { native, calls }
    }

    /// The resource `item` (its id as a program writes it, `<record>/<item>`) of the app `app`,
    /// its type `ty`: its fields as the app answers them, typed as the app's package declares
    /// them. Answered as [`crate::task::Host::read_values`] answers a read of an app the session
    /// runs: the type's id, every field by its id, or `{"missing": true}` for a resource the app
    /// does not hold.
    pub fn read(&self, app: &str, item: &str, ty: &str) -> Result<Json, String> {
        let record = self.record(app)?;
        let Some((doc, rest)) = split_id(item) else {
            return Err(format!("`{item}` is no resource id"));
        };
        if doc != record {
            return Err(format!(
                "{app} keeps `{doc}`; the record a program reads is `{record}`"
            ));
        }
        let content = if rest.is_none() {
            let (_, root_ty) = self.root(app)?;
            if ty != root_ty {
                return Err(format!("{record} is a {root_ty}, not a {ty}"));
            }
            self.content(app, &self.record_row(app)?)?
        } else {
            // A program's id names the item, not the collection it sits in: two collections of
            // one type are told apart by asking the app each in turn.
            let rows = self.rows(app, ty)?;
            let mut candidates = rows.iter().filter(|row| row.held == item);
            let Some(first) = candidates.next() else {
                return Ok(json!({"missing": true}));
            };
            let mut found = self.content(app, first);
            for row in candidates {
                if found.is_ok() {
                    break;
                }
                found = self.content(app, row);
            }
            found?
        };
        self.read_values(app, ty, &content)
    }

    /// `content`, the app's answer for a resource of the type `ty`, as a read of it values it.
    fn read_values(&self, app: &str, ty: &str, content: &Json) -> Result<Json, String> {
        if !content.is_object() {
            return Err(format!(
                "{app} answered a {ty} read of no fields: {content}"
            ));
        }
        Ok(json!({
            "type": ty,
            "fields": self
                .fields(ty, content)?
                .into_iter()
                .map(|(id, value)| (id, field_json(&value)))
                .collect::<serde_json::Map<String, Json>>(),
        }))
    }

    /// What the app `app` answers to a program's `request` to read it.
    pub fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        let request: ReadRequest = serde_json::from_value(request.clone())
            .map_err(|error| format!("the request is not one a program makes: {error}"))?;
        match &request {
            ReadRequest::Read { ty, id, .. } => self.answer_read(app, ty, id),
            ReadRequest::Select {
                ty, pred, order, ..
            } => self.answer_select(app, ty, pred, order.as_ref()),
            ReadRequest::Describe { name, field, .. } => {
                self.answer_describe(app, name.as_deref(), field.as_deref())
            }
            ReadRequest::Grep { .. } | ReadRequest::Find { .. } => Err(format!(
                "{app} keeps the text of its record in itself: a program searches the resources \
                 {app} lists by their fields"
            )),
        }
    }

    /// [`ReadRequest::Read`] — one resource, as a program reads it.
    fn answer_read(&self, app: &str, ty: &str, id: &str) -> Result<Json, String> {
        let schema = self.schema_of(app, ty)?;
        let row = match split_id(id) {
            Some((doc, None)) if doc == self.record(app)? && schema.id == self.root(app)?.1 => {
                self.record_row(app)?
            }
            Some((_, Some(_))) => self
                .rows(app, &schema.id)?
                .into_iter()
                .find(|row| row.held == id)
                .ok_or_else(|| format!("{app} lists no {} `{id}`", schema.name))?,
            _ => return Err(format!("{id} names no resource of {}", schema.name)),
        };
        let content = self.content(app, &row)?;
        let mut fields = Vec::new();
        for field in &schema.fields {
            // Text is the substance a read takes in parts; what the record holds is read where
            // the app lists it, and a nested record through the fields of its own type.
            if field.kind.holds_text()
                || matches!(field.kind, MergeKind::Collection | MergeKind::Record)
            {
                continue;
            }
            let Some(held) = content.get(key_of(&field.id)) else {
                continue;
            };
            fields.push((key_of(&field.id).to_owned(), printed(held)));
        }
        let answer = ReadAnswer {
            shown: Shown {
                literal: format!("@{}.{ty}:{id}", alias(app)),
                app: app.to_owned(),
                ty: ty.to_owned(),
                id: id.to_owned(),
                title: self.title(app, schema, &content),
            },
            parts: Vec::new(),
            fields,
        };
        serde_json::to_value(answer).map_err(|error| error.to_string())
    }

    /// [`ReadRequest::Select`] — the resources of `ty` its predicate holds of, by id, in the
    /// order asked.
    fn answer_select(
        &self,
        app: &str,
        ty: &str,
        pred: &Pred,
        order: Option<&Order>,
    ) -> Result<Json, String> {
        let schema = self.schema_of(app, ty)?;
        let rows = self.rows(app, &schema.id)?;
        let levels = order.map(Order::levels).unwrap_or_default();
        let mut keyed = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut held = Held {
                reads: self,
                app,
                schema,
                row,
                content: None,
                fields: HashMap::new(),
            };
            // A resource the query asks no field of is not read at all.
            let holds = pred
                .eval(&mut |read: &FieldRead| held.lit(read))
                .map_err(|error| error.to_string())?;
            if !holds {
                continue;
            }
            let mut keys = Vec::with_capacity(levels.len());
            for level in &levels {
                let value = held.get(&level.field)?;
                keys.push(
                    lit_of(&level.field, value, &Lit::Absent).map_err(|error| error.to_string())?,
                );
            }
            keyed.push((keys, row.held.clone()));
        }
        if let Some(order) = order {
            keyed.sort_by(|(a, one), (b, two)| ordered_cmp(order, a, b).then_with(|| one.cmp(two)));
        }
        Ok(Json::Array(
            keyed.into_iter().map(|(_, id)| Json::String(id)).collect(),
        ))
    }

    /// [`ReadRequest::Describe`] — what the app publishes, from its package.
    fn answer_describe(
        &self,
        app: &str,
        name: Option<&str>,
        field: Option<&str>,
    ) -> Result<Json, String> {
        let meta = self
            .native
            .meta(app)
            .ok_or_else(|| format!("{app} is no app this task reached"))?;
        let short = alias(app);
        let Some(name) = name else {
            return Ok(Json::String(tilolang_runtime::reads::describe_app(
                meta, short,
            )));
        };
        let Some(ty) = meta.types.iter().find(|ty| ty.name == name) else {
            let declared: Vec<&str> = meta.types.iter().map(|ty| ty.name.as_str()).collect();
            return Err(format!(
                "{app} has no type `{name}`; it declares {}",
                declared.join(", ")
            ));
        };
        if let Some(field) = field
            && !ty.fields.iter().any(|found| found.name == field)
        {
            return Err(format!(
                "{app}.{name} has no field `{field}`; fields: {}",
                ty.fields
                    .iter()
                    .map(|found| found.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(Json::String(tilolang_runtime::reads::describe_type(
            meta, ty, short,
        )))
    }

    /// The record `app` publishes.
    fn record(&self, app: &str) -> Result<&str, String> {
        self.native.record(app).ok_or_else(|| {
            format!("{app} publishes no record a program reads: invoke its actions instead")
        })
    }

    /// The resource `app` publishes its record as: the id its directory lists the record by at
    /// the top. A caller addresses the record by that id, which the window publishing it may
    /// bind (`tilo-ui-bound:v1:…`), never by the record's own name, which the directory lists
    /// as the row's key. The record's `read`, where it offers one, takes a resource under it as
    /// its `item`.
    ///
    /// # Errors
    /// Why the app lists no record: it does not run, the task may not see it.
    fn published(&self, app: &str) -> Result<String, String> {
        let record = self.record(app)?;
        self.calls
            .children(app, "")?
            .iter()
            .find(|row| keyed(row) == Some(record))
            .and_then(|row| row.get("id"))
            .and_then(Json::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                format!("{app} lists no `{record}`: a program reads what its directory lists")
            })
    }

    /// The app's record type and its schema.
    fn root(&self, app: &str) -> Result<(&'static TypeSchema, &str), String> {
        let ty = self
            .native
            .record_type(app)
            .ok_or_else(|| format!("{app} publishes no record a program reads"))?;
        let schema = lookup(ty)
            .ok_or_else(|| format!("{app}'s record type {ty} is not one this task knows"))?;
        Ok((schema, ty))
    }

    /// The schema of the type `ty` of `app`, by the name a program writes it or by its id.
    fn schema_of(&self, app: &str, ty: &str) -> Result<&'static TypeSchema, String> {
        let meta = self
            .native
            .meta(app)
            .ok_or_else(|| format!("{app} is no app this task reached"))?;
        let found = meta
            .types
            .iter()
            .find(|found| found.name == ty || found.id == ty)
            .ok_or_else(|| format!("{app} has no type {ty}"))?;
        lookup(&found.id).ok_or_else(|| format!("{app}'s type {ty} is not one this task knows"))
    }

    /// The row of the record itself: the resource the app's directory lists its record as.
    fn record_row(&self, app: &str) -> Result<Row, String> {
        let id = self.published(app)?;
        Ok(Row {
            held: self.record(app)?.to_owned(),
            under: id.clone(),
            id,
            key: None,
        })
    }

    /// The rows of the resources of the type `ty` `app` lists, each by the id a program holds it
    /// by and the id the directory lists it as: the record alone for its own type, and every
    /// resource the app's directory lists under it whose type is `ty`. What an item reads as, the
    /// app says when it is read ([`Self::content`]).
    fn rows(&self, app: &str, ty: &str) -> Result<Vec<Row>, String> {
        let record = self.record(app)?;
        let (root, _) = self.root(app)?;
        if ty == root.id {
            return Ok(vec![self.record_row(app)?]);
        }
        let published = self.published(app)?;
        let mut found = Vec::new();
        let mut seen = BTreeSet::new();
        for row in self.calls.children(app, &published)? {
            let Some(key) = keyed(&row) else { continue };
            if !seen.insert(key.to_owned()) || item_type(record, root, key) != Some(ty) {
                continue;
            }
            let Some(id) = row.get("id").and_then(Json::as_str) else {
                continue;
            };
            found.push(Row {
                held: held_id(record, key),
                id: id.to_owned(),
                under: published.clone(),
                key: Some(key.to_owned()),
            });
        }
        found.sort_by(|one, two| one.held.cmp(&two.held));
        Ok(found)
    }

    /// Every field of a resource of the type `ty` as the app answered it, by field id. What the
    /// app holds that its surface does not declare is the app's own business; a collection is
    /// read where the app lists it, not as one field.
    fn fields(&self, ty: &str, content: &Json) -> Result<HashMap<String, FieldValue>, String> {
        let mut out = HashMap::new();
        let Some(schema) = lookup(ty) else {
            return Ok(out);
        };
        let meta = self.native.meta(&schema.app);
        for field in &schema.fields {
            if matches!(field.kind, MergeKind::Collection | MergeKind::Record) {
                continue;
            }
            let answered = content.get(key_of(&field.id));
            out.insert(field.id.clone(), held(field, answered, meta)?);
        }
        Ok(out)
    }

    /// What the app answers to its own read of the resource `row` names: its typed data. The
    /// resource's own read answers it when the app's directory offers one; else the record's,
    /// which takes the resource as its `item` — the read `tilo_read` performs.
    fn content(&self, app: &str, row: &Row) -> Result<Json, String> {
        let (resource, item) = match &row.key {
            Some(key) if !self.calls.reads(app, &row.id)? => (row.under.clone(), Some(key.clone())),
            _ => (row.id.clone(), None),
        };
        let call = json!({
            "app": app,
            "resource": resource,
            "action": "read",
            "arguments": {"item": item, "version": null},
        });
        match self.calls.call(&call) {
            Answered::Done(output) => Ok(output.get("value").cloned().unwrap_or(Json::Null)),
            Answered::Refused(why) => Err(format!("{app} refused the read: {why}")),
            Answered::Failed(why) => Err(format!("{app} could not be read: {why}")),
            Answered::Asks(why) => Err(format!("{app} asks before it reads: {why}")),
        }
    }

    /// The app's own name for the resource of the type `schema` it answered as `content`: its
    /// first text field, else its `title`, `name` or `label`, else its type's name.
    fn title(&self, app: &str, schema: &TypeSchema, content: &Json) -> Option<String> {
        let named = |field: &FieldSchema| {
            content
                .get(key_of(&field.id))
                .and_then(Json::as_str)
                .map(str::to_owned)
        };
        schema
            .fields
            .iter()
            .find(|field| field.kind.holds_text())
            .and_then(named)
            .or_else(|| {
                ["title", "name", "label"]
                    .iter()
                    .find_map(|name| content.get(*name).and_then(Json::as_str))
                    .map(str::to_owned)
            })
            .or_else(|| {
                self.native
                    .meta(app)
                    .and_then(|meta| meta.types.iter().find(|ty| ty.id == schema.id))
                    .map(|ty| ty.name.clone())
            })
    }
}

/// The name a program writes the app `app` by: `chat` for `so.tilo.chat`.
fn alias(app: &str) -> &str {
    app.rsplit('.').next().unwrap_or(app)
}

/// The key the app stores a field under: the field's own name, as its model writes it.
fn key_of(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

/// The key a directory row names its resource by: the app's own id for it, as its `read` takes
/// an item.
fn keyed(row: &Json) -> Option<&str> {
    row.get("key").and_then(Json::as_str)
}

/// The id a program holds the resource `key` of the record `record` by: the record, then the
/// item's own id, the last step of `key`. A program's id keeps one item, as [`split_id`] reads
/// one, so a resource nested under another is held by the id of the item it sits in.
fn held_id(record: &str, key: &str) -> String {
    format!("{record}/{}", key.rsplit('/').next().unwrap_or(key))
}

/// The type of the resource `key` of the record `record`, walking the collections of `root`:
/// `issues/<id>` is an issue, `issues/<id>/links/<id>` a link.
fn item_type(record: &str, root: &'static TypeSchema, key: &str) -> Option<&'static str> {
    let mut path = key.strip_prefix(record)?.trim_start_matches('/').split('/');
    let mut schema = root;
    let mut ty = None;
    while let Some(field) = path.next() {
        path.next()?;
        let held = schema
            .fields
            .iter()
            .find(|held| held.kind == MergeKind::Collection && key_of(&held.id) == field)?;
        schema = lookup(held.record.as_deref()?)?;
        ty = Some(schema.id.as_str());
    }
    ty
}

/// What `field` reads as, from what the app answered of it; [`default_value`] for a field the
/// answer does not name.
///
/// # Errors
/// What the app answered of a field its surface declares differently: a set that is no set, an
/// option that is neither a value nor nothing, a case no enum of the app declares.
fn held(
    field: &FieldSchema,
    answered: Option<&Json>,
    meta: Option<&Meta>,
) -> Result<FieldValue, String> {
    let Some(answered) = answered.filter(|answered| !answered.is_null()) else {
        return Ok(default_value(field));
    };
    Ok(match field.kind {
        // The app writes rich text as the markdown a person reads: the runtime reads it as the
        // same text.
        MergeKind::RichText => {
            FieldValue::Rich(RichValue::from_text(answered.as_str().unwrap_or_default()))
        }
        MergeKind::Set => FieldValue::Keys(elements(answered)?),
        MergeKind::Map => FieldValue::Entries(
            answered
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, value)| (key.clone(), scalar_of_json(value, false)))
                .collect(),
        ),
        MergeKind::Collection | MergeKind::Record => default_value(field),
        MergeKind::Register if field.composite => {
            let (Some(ty), Some(meta)) = (field.ty.as_ref(), meta) else {
                return Ok(FieldValue::Scalar(Scalar::Json(answered.clone())));
            };
            FieldValue::Scalar(Scalar::Json(
                encoded(ty, answered, meta)
                    .map_err(|why| format!("`{}` of {}: {why}", field.id, meta.app))?,
            ))
        }
        _ => field_of_json(answered, field).unwrap_or_else(|_| default_value(field)),
    })
}

/// The app's `plain` value of the type `ty`, as the runtime holds a value of it: what a program
/// reads a field as and a filter compares it with. An app writes its model's own JSON — an
/// option as the value or `null`, a list as its array, a case of an enum by its name, a record
/// as its id — where the runtime holds `{"some": …}`, `{"list": […]}`,
/// `{"enum": …, "variant": …}` and `{"ref": …}`.
fn encoded(ty: &Ty, plain: &Json, meta: &Meta) -> Result<Json, String> {
    Ok(match ty {
        Ty::Option(inner) => json!({"some": encoded(inner, plain, meta)?}),
        Ty::List(inner) | Ty::Collection(inner) | Ty::Set(inner) => {
            let items = plain
                .as_array()
                .ok_or_else(|| format!("{plain} is no list of {inner}"))?;
            let mut encoded_items = Vec::with_capacity(items.len());
            for item in items {
                encoded_items.push(encoded(inner, item, meta)?);
            }
            json!({"list": encoded_items})
        }
        Ty::Tuple(types) => {
            let items = plain
                .as_array()
                .ok_or_else(|| format!("{plain} is no tuple of {ty}"))?;
            if items.len() != types.len() {
                return Err(format!("{plain} is no tuple of {ty}"));
            }
            let mut encoded_items = Vec::with_capacity(items.len());
            for (item, held) in items.iter().zip(types) {
                encoded_items.push(encoded(held, item, meta)?);
            }
            json!({"tuple": encoded_items})
        }
        Ty::Enum(key) => {
            let case = plain
                .as_str()
                .ok_or_else(|| format!("{plain} is no case of {ty}"))?;
            let declared = meta
                .enums
                .iter()
                .find(|declared| declared.name == key.name && key.app == meta.app)
                .ok_or_else(|| format!("{ty} is no enum of {}", meta.app))?;
            let found = declared
                .variants
                .iter()
                .find(|variant| variant.name == case || variant.id == case)
                .ok_or_else(|| {
                    let cases: Vec<&str> = declared
                        .variants
                        .iter()
                        .map(|variant| variant.name.as_str())
                        .collect();
                    format!(
                        "`.{case}` is no case of {ty}; its cases are {}",
                        cases.join(", ")
                    )
                })?;
            json!({"enum": declared.id, "variant": found.id, "fields": {}})
        }
        Ty::Principal => {
            let who = plain
                .as_str()
                .ok_or_else(|| format!("{plain} names no principal"))?;
            json!({"principal": who})
        }
        Ty::Record(key) => json!({"ref": reference(plain)?, "ty": type_id(key)}),
        // A field of no type the runtime reads as a value of its own: its JSON, as the app
        // wrote it, is all there is to read.
        _ => plain.clone(),
    })
}

/// The id of the resource the app answered as `plain`: its id string, or an object naming one.
fn reference(plain: &Json) -> Result<&str, String> {
    match plain {
        Json::String(id) => Ok(id),
        Json::Object(fields) => fields
            .get("id")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("{plain} names no resource")),
        _ => Err(format!("{plain} names no resource")),
    }
}

/// The id of the type `key`: the one its app registers (`so.tilo.tasks.Issue`), else the
/// qualified name a reference to an app's type is written with (`so.tilo.tasks#Issue`).
fn type_id(key: &TypeKey) -> String {
    crate::schema::lookup_in(&key.app, &key.name).map_or_else(
        || format!("{}#{}", key.app, key.name),
        |schema| schema.id.clone(),
    )
}

/// The elements of a set as the app answered it: its members, sorted as a store keeps them.
fn elements(answered: &Json) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = match answered {
        Json::Array(elements) => elements.iter().map(element_key).collect(),
        Json::Object(elements) => elements.keys().cloned().collect(),
        _ => return Err(format!("{answered} is no set")),
    };
    keys.sort();
    keys.dedup();
    Ok(keys)
}

/// A resource of an app a query reads: the app's answer of it, read when the predicate or the
/// order first asks a field of it, and not at all for a query that asks none.
struct Held<'a> {
    reads: &'a NativeReads<'a>,
    app: &'a str,
    schema: &'static TypeSchema,
    /// The resource, as the app's directory lists it.
    row: &'a Row,
    content: Option<Json>,
    fields: HashMap<String, FieldValue>,
}

impl Held<'_> {
    /// Fill what the app answered of the resource, once.
    fn fill(&mut self) -> Result<(), String> {
        if self.content.is_some() {
            return Ok(());
        }
        let content = self.reads.content(self.app, self.row)?;
        self.fields = self.reads.fields(&self.schema.id, &content)?;
        self.content = Some(content);
        Ok(())
    }

    /// What the app answered a field of the resource holds, or what the field reads as when it
    /// names it not.
    fn get(&mut self, field: &str) -> Result<FieldValue, String> {
        self.fill()?;
        Ok(self
            .fields
            .get(field)
            .cloned()
            .unwrap_or_else(|| field_or_default(self.schema, field)))
    }

    /// The field a predicate reads, as it compares it.
    fn lit(&mut self, read: &FieldRead) -> Result<Lit, StoreError> {
        let value = self.get(&read.spec.id).map_err(StoreError::Backend)?;
        read.lit(value)
    }
}

/// What `field` of `schema` holds when the answer does not name it.
fn field_or_default(schema: &'static TypeSchema, field: &str) -> FieldValue {
    schema
        .fields
        .iter()
        .find(|held| held.id == field)
        .map_or(FieldValue::Absent, default_value)
}

/// A field of a resource as the app answered it, as a program prints it: a collection by how
/// many resources it holds, anything else as the app's text or as its JSON.
fn printed(value: &Json) -> String {
    match value {
        Json::Array(items) => format!(
            "{} {}",
            items.len(),
            if items.len() == 1 { "item" } else { "items" }
        ),
        Json::Null => "none".to_owned(),
        Json::String(text) => text.clone(),
        Json::Object(_) => serde_json::to_string(value).unwrap_or_default(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::MetaExt as _;

    /// Tasks's package, as the session reads it, its schemas registered as loading it registers
    /// them.
    fn tasks() -> PackageMeta {
        let meta = crate::catalogue::NativeSurface {
            app: "so.tilo.tasks".to_owned(),
            title: "Tasks".to_owned(),
            surface: include_str!("../../../../apps/tasks/surface.toml").to_owned(),
        }
        .package()
        .expect("the surface reads");
        meta.register_schemas();
        meta
    }

    /// Chat's package, as the session reads it.
    fn chat() -> PackageMeta {
        let meta = crate::catalogue::NativeSurface {
            app: "so.tilo.chat".to_owned(),
            title: "Chat".to_owned(),
            surface: include_str!("../../../../apps/chat/surface.toml").to_owned(),
        }
        .package()
        .expect("the surface reads");
        meta.register_schemas();
        meta
    }

    /// What answers nothing: these reads resolve the app's own declarations alone.
    struct NoCalls;

    impl Calls for NoCalls {
        fn call(&self, _: &Json) -> Answered {
            Answered::Failed("no app runs here".to_owned())
        }

        fn children(&self, _: &str, _: &str) -> Result<Vec<Json>, String> {
            Ok(Vec::new())
        }

        fn reads(&self, _: &str, _: &str) -> Result<bool, String> {
            Err("no app runs here".to_owned())
        }
    }

    #[test]
    fn the_record_an_app_publishes_is_the_root_its_package_names() {
        let native = Native::of(&[], &[tasks(), chat()]);
        assert_eq!(native.record("so.tilo.tasks"), Some("tasks.tracker"));
        assert_eq!(native.record("so.tilo.chat"), Some("chat.workspace"));
        assert!(native.is("so.tilo.tasks"));
        assert!(!native.is("so.tilo.notes"));
        assert_eq!(
            native.record_type("so.tilo.chat"),
            Some("so.tilo.chat.Workspace")
        );
        assert_eq!(
            native.record_type("so.tilo.tasks"),
            Some("so.tilo.tasks.Tracker")
        );
    }

    #[test]
    fn a_resource_key_names_its_type_by_the_collections_it_sits_in() {
        // The app's schemas register as the session loads its package.
        let _tasks = tasks();
        let root = lookup("so.tilo.tasks.Tracker").expect("the tracker's schema");
        let ty = |key: &str| item_type("tasks.tracker", root, key);
        assert_eq!(ty("tasks.tracker/issues/abc"), Some("so.tilo.tasks.Issue"));
        assert_eq!(
            ty("tasks.tracker/issues/abc/links/one"),
            Some("so.tilo.tasks.Link")
        );
        assert_eq!(
            ty("tasks.tracker/issues/abc/comments/one"),
            Some("so.tilo.tasks.Message")
        );
        // A field of an item is no resource, and neither is a collection.
        assert_eq!(ty("tasks.tracker/issues/abc/title"), None);
        assert_eq!(ty("tasks.tracker/issues"), None);
        assert_eq!(ty("tasks.tracker/nothing/abc"), None);
        // A program's id keeps the item's own id, which [`NativeReads::read`] matches against
        // the rows the app's directory lists.
        assert_eq!(
            held_id("tasks.tracker", "tasks.tracker/issues/abc"),
            "tasks.tracker/abc"
        );
    }

    #[test]
    fn a_field_is_read_as_the_apps_surface_typing_says() {
        let native = Native::of(&[], &[tasks()]);
        let reads = NativeReads::new(&native, &NoCalls);
        let schema = lookup("so.tilo.tasks.Issue").expect("the issue's schema");
        let content = json!({
            "title": "Review Ada's portfolio",
            "assignee": "person:ada",
            "labels": ["urgent", "review"],
            "body": "Look at her GitHub.",
        });
        let fields = reads
            .fields("so.tilo.tasks.Issue", &content)
            .expect("the fields read");
        assert_eq!(
            fields.get("Issue.title"),
            Some(&FieldValue::Text("Review Ada's portfolio".into()))
        );
        // An option the app wrote as the value it holds is the runtime's option.
        assert_eq!(
            fields.get("Issue.assignee"),
            Some(&FieldValue::Scalar(Scalar::Json(
                json!({"some": "person:ada"})
            )))
        );
        assert_eq!(
            fields.get("Issue.labels"),
            Some(&FieldValue::Keys(vec!["review".into(), "urgent".into()]))
        );
        assert_eq!(
            fields.get("Issue.body"),
            Some(&FieldValue::Rich(RichValue::from_text(
                "Look at her GitHub."
            )))
        );
        // A collection is read where the app lists it; a field nobody wrote holds what the
        // app's package says it holds.
        assert!(!fields.contains_key("Issue.links"));
        let due = schema
            .field("Issue.due_ms")
            .expect("the due day is a field");
        assert_eq!(fields.get("Issue.due_ms"), Some(&default_value(due)));
    }
}
