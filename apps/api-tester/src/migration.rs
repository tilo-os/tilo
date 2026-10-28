//! Device-custody import, deliberately separate from deterministic schema evolution.
//! No typed state (including an empty replacement) is published until every
//! credential has received durable acknowledgement. The original remains the
//! retry source on failure. Removing current fields does not erase CRDT history:
//! previously shared credentials must be rotated and old copies retired.

use std::cell::RefCell;
use std::collections::HashMap;

use automerge::{AutoCommit, ROOT, ReadDoc};
use ui::application::HostContext;
use ui::secrets::SecretStore;
use ui::tilo_resource::{Atom, Fields, Id};

#[cfg(test)]
mod tests;

use crate::credentials::Migration;
use crate::legacy::ApiDoc;
use crate::model::{Auth, Request};
use crate::record::{Environment, RequestRecord, SavedCollection, Sent, Variable, Workspace};

thread_local! {
    // Browser acknowledgements are consumed once. Keep completed seals while
    // another field is pending, including when the person reopens to retry.
    static IMPORTS: RefCell<HashMap<String, Migration>> = RefCell::new(HashMap::new());
}

pub(super) fn open(context: &HostContext) -> Result<(), String> {
    let needed = context
        .read_record(needs_import)
        .map_err(|error| error.to_string())?;
    if !needed {
        return Ok(());
    }
    let key = format!("{}:{}", context.device_id(), context.record().id);
    let result = IMPORTS.with(|imports| {
        let mut imports = imports.borrow_mut();
        let migration = imports.entry(key.clone()).or_default();
        let store = context.secrets();
        context.migrate_record(|doc| import(doc, &store, migration))?;
        imports.remove(&key);
        Ok(())
    });
    if result.as_ref().err().map(String::as_str) == Some(crate::credentials::PENDING) {
        // The page backend is writing asynchronously. Ask the host to retry
        // on its next turn, not recursively while this record is borrowed.
        // Each attempt consumes acknowledgements before publishing any typed
        // state. A real failure stops here with the original draft intact.
        let record = context.record().id.clone();
        context.reopener().replace_with(move |reopener| {
            let definition = crate::definition();
            let (context, target) = reopener.prepare(definition, record)?;
            let component =
                definition.instantiate(context, target, ui::application::RestoreInput::None)?;
            Ok(ui::application::ComponentReplacement::new(
                component,
                Vec::new(),
            ))
        });
    }
    result
}

fn needs_import(doc: &AutoCommit) -> bool {
    doc.get(ROOT, "request").ok().flatten().is_none()
        && doc.keys(ROOT).any(|key| key.starts_with("api_tester_"))
}

fn request(
    original: &Request,
    store: &SecretStore,
    migration: &mut Migration,
) -> Result<RequestRecord, String> {
    let mut record = RequestRecord::of(original);
    // An already sealed old document still keeps basic/API-key public metadata
    // inside its local envelope. Restore that metadata before moving the ref.
    if matches!(original.auth, Auth::SecretRef { .. }) {
        let auth = crate::credentials::resolve(&original.auth, store).map_err(str::to_owned)?;
        record.auth = crate::record::Credentials::of(&auth);
    }
    if let Auth::SecretRef { id, .. } = migration
        .seal(&original.auth, store)
        .map_err(str::to_owned)?
    {
        record.auth.reference = id;
    }
    Ok(record)
}

/// The same transaction used by startup, also exercised over saved old bytes.
fn import(
    doc: &mut AutoCommit,
    store: &SecretStore,
    migration: &mut Migration,
) -> Result<bool, String> {
    if !needs_import(doc) {
        return Ok(false);
    }
    let workspace = project(doc, store, migration)?;
    // Perform all document work on a fork: quota failure or pending browser
    // acknowledgement leaves both the durable draft and its history unchanged.
    let mut next = doc.clone();
    migration
        .migrate_document(&mut next, &ROOT, store)
        .map_err(str::to_owned)?;
    workspace
        .write_fields(&mut next, &ROOT)
        .map_err(|error| error.to_string())?;
    *doc = next;
    Ok(true)
}

fn project(
    doc: &AutoCommit,
    store: &SecretStore,
    migration: &mut Migration,
) -> Result<Workspace, String> {
    let mut workspace = Workspace::default();
    if let Some(active) = ApiDoc::request(doc, &ROOT) {
        workspace.request = request(&active, store, migration)?;
        workspace.opened = Atom(active.id.to_string());
    }
    for collection in ApiDoc::collections(doc, &ROOT) {
        let mut saved = SavedCollection {
            name: collection.name,
            ..SavedCollection::default()
        };
        for old in collection.requests {
            saved
                .requests
                .push(old.id.clone(), request(&old, store, migration)?);
        }
        workspace.collections.push(collection.id, saved);
    }
    for environment in ApiDoc::environments(doc, &ROOT) {
        let mut imported = Environment {
            name: environment.name,
            ..Environment::default()
        };
        for variable in environment.variables {
            // Never silently discard a credential from an unsupported old
            // variable envelope, nor move it into the typed shared record.
            if variable.secret && !variable.value.is_empty() {
                return Err(
                    "A legacy secret variable needs device-local storage before import".into(),
                );
            }
            imported.variables.push(
                Id::from_raw(variable.id),
                Variable {
                    key: variable.key,
                    value: variable.value,
                    enabled: variable.enabled,
                    secret: variable.secret,
                },
            );
        }
        workspace.environments.push(environment.id, imported);
    }
    workspace.active_environment = ApiDoc::active_env(doc, &ROOT).map(Id::from_raw);
    for old in ApiDoc::history(doc, &ROOT) {
        workspace.history.push(
            old.id,
            Sent::new(old.method, &old.url, old.status, old.duration_ms, 0, None),
        );
    }
    Ok(workspace)
}

/// Import a deliberately supplied old snapshot without copying its plaintext
/// CRDT history into the destination. The caller retains the original bytes
/// until this succeeds and the destination is saved.
pub(super) async fn snapshot(bytes: &[u8], store: &SecretStore) -> Result<Workspace, String> {
    let doc =
        ui::tilo_resource::Doc::load(bytes).map_err(|_| "Invalid legacy Automerge snapshot")?;
    if !needs_import(&doc) {
        return Err("This is not a legacy API workspace snapshot".into());
    }
    let mut migration = Migration::default();
    let requests = ApiDoc::request(&doc, &ROOT).into_iter().chain(
        ApiDoc::collections(&doc, &ROOT)
            .into_iter()
            .flat_map(|collection| collection.requests),
    );
    for request in requests {
        migration
            .seal_async(&request.auth, store)
            .await
            .map_err(str::to_owned)?;
    }
    project(&doc, store, &mut migration)
}
