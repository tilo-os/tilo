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
    let mut failed = None;
    each_published(tree, &mut |spec| {
        let Some((_, path)) = split_published(spec.id.as_str()) else {
            return;
        };
        let Some(ty) = store
            .kinds()
            .of_kind(&spec.kind)
            .map(|entry| entry.id.clone())
        else {
            return;
        };
        let Some(at) = ResourceRef::parse_id(path, ty) else {
            return;
        };
        match store.versions(&at) {
            Ok(held) => versions.extend(held),
            Err(error) => failed = Some(error.to_string()),
        }
    });
    if let Some(error) = failed {
        return Err(error);
    }
    versions.sort_by(|a, b| a.order().cmp(&b.order()));
    Ok(versions)
}

/// Call `visit` with `spec` and every resource published in it, depth first.
pub fn each_published(spec: &ResourceSpec, visit: &mut dyn FnMut(&ResourceSpec)) {
    visit(spec);
    for child in &spec.children {
        each_published(child, visit);
    }
}
