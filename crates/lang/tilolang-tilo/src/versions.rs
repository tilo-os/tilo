//! The versions of a published record: every version of a published resource and of the
//! resources published in it, each as the app's history lists it.

use serde_json::Value as Json;
use tilo_actions::ResourceSpec;
use tilolang_runtime::store::{Store as _, Version};
use tilolang_runtime::value::ResourceRef;

use crate::host::Host;
use crate::publish::{resource_id, split_published};

/// `version`, of a resource of `app` that `host` reads, as its history lists it: the version,
/// its resource by the id it is published under, the label the app's history calls it by, and
/// what it did to the resource.
#[must_use]
pub fn version_entry(host: &Host, app: &str, version: &Version) -> Json {
    let published = resource_id(app, &version.resource);
    let label = history_label(host, &version.resource);
    let mut entry = serde_json::to_value(version).unwrap_or_default();
    if let Some(entry) = entry.as_object_mut() {
        entry.insert("resource".to_owned(), Json::String(published));
        entry.insert("label".to_owned(), Json::String(label));
        // What the version did to the resource it is a version of, read from the store's own
        // kind, beside the change's id.
        entry.insert(
            "happened".to_owned(),
            Json::String(version.kind.what().word().to_owned()),
        );
    }
    entry
}

/// What the app's history calls the resource `id` (as [`ResourceRef::id_string`] writes it): an
/// item by its label as published, a package's root by its type ("Notebook"), as the app's name
/// it is published under says nothing in the app's own history.
fn history_label(host: &Host, id: &str) -> String {
    let Some(at) = host
        .store()
        .type_of_id(id)
        .and_then(|ty| ResourceRef::parse_id(id, ty))
    else {
        return String::new();
    };
    match (&at.item, crate::schema::lookup(&at.ty)) {
        (None, Some(schema)) => schema.spoken_name(),
        _ => host.label_at(&at),
    }
}

/// The versions of the published resource `tree`, which `host` reads, and of every resource
/// published in it, oldest first.
///
/// # Errors
/// The store did not read a resource's versions.
pub fn versions(host: &Host, tree: &ResourceSpec) -> Result<Vec<Version>, String> {
    let store = host.store();
    let mut versions = Vec::new();
    for at in published_refs(host, tree) {
        versions.extend(store.versions(&at).map_err(|error| error.to_string())?);
    }
    versions.sort_by(|a, b| a.order().cmp(&b.order()));
    Ok(versions)
}

/// The published resource `tree`, which `host` reads, and every resource published in it, as
/// the store names them; those of no type loaded here left out.
#[must_use]
pub fn published_refs(host: &Host, tree: &ResourceSpec) -> Vec<ResourceRef> {
    let mut refs = Vec::new();
    each_published(tree, &mut |spec| refs.extend(store_ref(host, spec)));
    refs
}

/// The published resource `spec`, as the store names it; `None` for one of no type loaded here.
#[must_use]
pub fn store_ref(host: &Host, spec: &ResourceSpec) -> Option<ResourceRef> {
    let (_, path) = split_published(spec.id.as_str())?;
    let ty = host
        .store()
        .kinds()
        .of_kind(&spec.kind)
        .map(|entry| entry.id.clone())?;
    ResourceRef::parse_id(path, ty)
}

/// Call `visit` with `spec` and every resource published in it, depth first.
pub fn each_published(spec: &ResourceSpec, visit: &mut dyn FnMut(&ResourceSpec)) {
    visit(spec);
    for child in &spec.children {
        each_published(child, visit);
    }
}
