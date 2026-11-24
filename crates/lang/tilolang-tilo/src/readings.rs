//! The readings of published resources: what a runtime that loads a package as another app's
//! ([`tilolang_runtime::Runtime::load_foreign_package`]) reads back
//! ([`tilolang_runtime::Runtime::set_reading`]).

use serde_json::{Map, Value as Json};
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::ResourceRef;

use crate::host::Host;
use crate::publish::{resource_id, split_published};

/// The pseudo-field of a reading of an item that names what holds it, as a link names what links
/// to it (`{"ref": <published id>, "ty": "<app>#<Type>"}`): the conversation a message is in, the
/// notebook a note is in. An item moved elsewhere reads as held where it is now.
pub const HELD_IN: &str = "$in";

/// The reading of the root or item `resource` (its published id, `tl:<app>:<id>`) that `host`
/// reads: its fields as the runtime reads them, each by its field id, what holds it ([`HELD_IN`]),
/// and what links to it. What a runtime that loads its package as another app's reads back
/// ([`tilolang_runtime::Runtime::set_reading`]). `None` for a resource `host` holds none of.
#[must_use]
pub fn reading_of(host: &Host, resource: &str) -> Option<Json> {
    let (app, path) = split_published(resource)?;
    host.package(app)?;
    let at = resource_at(host, app, path)?;
    let schema = crate::schema::lookup(&at.ty)?;
    let runtime = host.runtime();
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
    // What the person it is read for has not seen of its items: a view composed elsewhere
    // counts it as this app's own views do ([`tilolang_runtime::store::UNSEEN`]).
    if let Some(unseen) = runtime.unseen_reading(host.viewer(), &at) {
        fields.insert(tilolang_runtime::store::UNSEEN.to_owned(), unseen);
    }
    if let Ok(Some((holder, _))) = host.store().container(&at)
        && let Some(schema) = crate::schema::lookup(&holder.ty)
    {
        fields.insert(
            HELD_IN.to_owned(),
            serde_json::json!({
                "ref": resource_id(&schema.app, &holder.id_string()),
                "ty": format!("{}#{}", schema.app, schema.name),
            }),
        );
    }
    // What links to it, each with how: a view composed elsewhere draws them as this app's
    // own views do ([`tilolang_runtime::store::LINKS`]), and runs again when they move.
    if let Ok(links) = host.store().linked(&at) {
        let links: Vec<Json> = links
            .into_iter()
            .filter_map(|link| {
                let schema = crate::schema::lookup(&link.of.ty)?;
                Some(Json::Object(Map::from_iter([
                    (
                        "ref".to_owned(),
                        Json::String(resource_id(&schema.app, &link.of.id_string())),
                    ),
                    (
                        "ty".to_owned(),
                        Json::String(format!("{}#{}", schema.app, schema.name)),
                    ),
                    ("how".to_owned(), Json::String(link.how.word().to_owned())),
                ])))
            })
            .collect();
        if !links.is_empty() {
            fields.insert(
                tilolang_runtime::store::LINKS.to_owned(),
                Json::Array(links),
            );
        }
    }
    Some(Json::Object(fields))
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

/// The resource `path` of the app `app` that `host`'s store holds, typed as the type of `app`
/// it is of: a package's root not made yet counts as held, as it reads as it would start.
/// `None` for a path that names none of the app's resources.
pub fn resource_at(host: &Host, app: &str, path: &str) -> Option<ResourceRef> {
    let kinds = host.store().kinds();
    let mut types = kinds
        .types()
        .filter(|entry| entry.app == app)
        .map(|entry| entry.id.clone());
    types.find_map(|ty| {
        let at = ResourceRef::parse_id(path, ty)?;
        let unmade_root = at.is_unmade_root() && at.doc == format!("root-{}", at.ty);
        (unmade_root || host.store().contains(&at)).then_some(at)
    })
}

/// The app of a resource whose type the shell's runtime names `<app>#<Type>`.
#[must_use]
pub fn app_of(ty: &str) -> Option<&str> {
    ty.split_once('#').map(|(app, _)| app)
}
