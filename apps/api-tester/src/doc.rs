//! Automerge document model for the API workspace.
//!
//! The workspace — collections, environments, the selected environment,
//! the request currently being edited, and the response history — lives in
//! the shared CRDT so two peers can edit the same set of requests and
//! converge, and so that `ui::persist` can carry all of it across a
//! restart. The document shape is:
//!
//! ```text
//! root
//! ├── "url"          (Text)   # the active request's URL (live-collaborative)
//! ├── "body"         (Text)   # the active request's body editor buffer
//! ├── "api_tester_collection_fields" (Map) # one native map per collection UUID
//! ├── "api_tester_collection_tombstones" (Map) # delete-wins collection UUIDs
//! ├── "api_tester_saved_request_fields" (Map) # one native map per request UUID
//! ├── "api_tester_environment_fields" (Map) # one native map per environment UUID
//! ├── "api_tester_environment_tombstones" (Map) # delete-wins environment UUIDs
//! ├── "api_tester_environment_variables" (Map) # one native map per stable variable UUID
//! ├── "active_env"   (Str)    # selected environment id, or ""
//! ├── "api_tester_request" (Map) # the single active-request record
//! └── "api_tester_history" (Map) # one immutable record per history UUID
//! ```
//!
//! The URL and body are first-class `Text` objects so character-level edits
//! merge between collaborators. Collections, saved requests, environments,
//! variables, and history use keyed records: editing one durable child never
//! overwrites a sibling. The active request uses individual keyed fields for
//! its structured fields, while URL/body remain text CRDTs. "Which response
//! I'm viewing" is intentionally not stored — it's local to each peer.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{Collection, Environment, HistoryEntry, KeyValue, Request};

/// Stateless namespace for the workspace document helpers.
pub struct ApiDoc;

const COLLECTION_FIELDS_KEY: &str = "api_tester_collection_fields";
const COLLECTION_TOMBSTONES_KEY: &str = "api_tester_collection_tombstones";
const REQUEST_TOMBSTONES_KEY: &str = "api_tester_saved_request_tombstones";
const SAVED_REQUEST_FIELDS_KEY: &str = "api_tester_saved_request_fields";
const ENVIRONMENT_FIELDS_KEY: &str = "api_tester_environment_fields";
const ENVIRONMENT_TOMBSTONES_KEY: &str = "api_tester_environment_tombstones";
const VARIABLES_KEY: &str = "api_tester_environment_variables";
const VARIABLE_TOMBSTONES_KEY: &str = "api_tester_environment_variable_tombstones";
const REQUEST_KEY: &str = "api_tester_request";
const ACTIVE_KEY_VALUES_KEY: &str = "api_tester_request_key_values";
const ACTIVE_KEY_VALUE_TOMBSTONES_KEY: &str = "api_tester_request_key_value_tombstones";
const HISTORY_KEY: &str = "api_tester_history";
const HISTORY_TOMBSTONES_KEY: &str = "api_tester_history_tombstones";
const HISTORY_ORDER_KEY: &str = "api_tester_history_order";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SavedRequestRecord {
    collection_id: String,
    request: Request,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct VariableRecord {
    environment_id: String,
    variable: KeyValue,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ActiveKeyValueRecord {
    section: String,
    value: KeyValue,
}

impl ApiDoc {
    /// Native per-record map used by all collection/environment writes.
    fn native_record_map(
        doc: &mut AutoCommit,
        root: &ObjId,
        store: &str,
        id: &str,
    ) -> Option<ObjId> {
        let store = Self::record_roots(doc, root, store)
            .into_iter()
            .next()
            .or_else(|| doc.put_object(root, store, ObjType::Map).ok())?;
        match doc.get(&store, id).ok().flatten() {
            Some((Value::Object(ObjType::Map), map)) => Some(map),
            _ => doc.put_object(&store, id, ObjType::Map).ok(),
        }
    }

    fn write_tombstone(doc: &mut AutoCommit, root: &ObjId, store: &str, id: &str) {
        let map = Self::record_roots(doc, root, store)
            .into_iter()
            .next()
            .or_else(|| doc.put_object(root, store, ObjType::Map).ok());
        if let Some(map) = map {
            Self::put_bool_if_changed(doc, &map, id, true);
        }
    }

    pub fn create_collection(doc: &mut AutoCommit, root: &ObjId, id: &str, name: &str) {
        if Self::is_tombstoned(doc, root, COLLECTION_TOMBSTONES_KEY, id) {
            return;
        }
        if let Some(map) = Self::native_record_map(doc, root, COLLECTION_FIELDS_KEY, id) {
            Self::put_string_if_changed(doc, &map, "name", name);
        }
    }

    pub fn rename_collection(doc: &mut AutoCommit, root: &ObjId, id: &str, name: &str) {
        Self::create_collection(doc, root, id, name);
    }

    pub fn delete_collection(doc: &mut AutoCommit, root: &ObjId, id: &str) {
        Self::write_tombstone(doc, root, COLLECTION_TOMBSTONES_KEY, id);
    }

    pub fn create_environment(doc: &mut AutoCommit, root: &ObjId, id: &str, name: &str) {
        if Self::is_tombstoned(doc, root, ENVIRONMENT_TOMBSTONES_KEY, id) {
            return;
        }
        if let Some(map) = Self::native_record_map(doc, root, ENVIRONMENT_FIELDS_KEY, id) {
            Self::put_string_if_changed(doc, &map, "name", name);
        }
    }

    pub fn rename_environment(doc: &mut AutoCommit, root: &ObjId, id: &str, name: &str) {
        Self::create_environment(doc, root, id, name);
    }

    pub fn delete_environment(doc: &mut AutoCommit, root: &ObjId, id: &str) {
        Self::write_tombstone(doc, root, ENVIRONMENT_TOMBSTONES_KEY, id);
    }

    /// Write one variable directly at its stable row address.  Variable values
    /// deliberately are not a JSON blob: concurrent writes to different rows
    /// (and tombstones for one row) therefore cannot replace siblings.
    pub fn set_variable(
        doc: &mut AutoCommit,
        root: &ObjId,
        env_id: &str,
        variable_id: &str,
        key: &str,
        value: &str,
    ) {
        if Self::is_tombstoned(doc, root, ENVIRONMENT_TOMBSTONES_KEY, env_id)
            || Self::is_tombstoned(
                doc,
                root,
                VARIABLE_TOMBSTONES_KEY,
                &format!("{env_id}:{variable_id}"),
            )
        {
            return;
        }
        let address = format!("{env_id}:{variable_id}");
        let Some(map) = Self::native_record_map(doc, root, VARIABLES_KEY, &address) else {
            return;
        };
        Self::put_string_if_changed(doc, &map, "environment_id", env_id);
        Self::put_string_if_changed(doc, &map, "id", variable_id);
        Self::put_string_if_changed(doc, &map, "key", key);
        Self::put_string_if_changed(doc, &map, "value", value);
        Self::put_bool_if_changed(doc, &map, "enabled", true);
    }

    /// Update only the public variable key at its stable address. The value
    /// remains an independent CRDT scalar, so renaming never reads or replaces
    /// a concurrent secret-value update.
    pub fn rename_variable(
        doc: &mut AutoCommit,
        root: &ObjId,
        env_id: &str,
        variable_id: &str,
        key: &str,
    ) {
        let address = format!("{env_id}:{variable_id}");
        if Self::is_tombstoned(doc, root, ENVIRONMENT_TOMBSTONES_KEY, env_id)
            || Self::is_tombstoned(doc, root, VARIABLE_TOMBSTONES_KEY, &address)
        {
            return;
        }
        if let Some(map) = Self::native_record_map(doc, root, VARIABLES_KEY, &address) {
            Self::put_string_if_changed(doc, &map, "key", key);
        }
    }

    /// Tombstone one stable variable row.  The environment id is part of the
    /// address, allowing two environments to use the same visible key.
    pub fn delete_variable(doc: &mut AutoCommit, root: &ObjId, env_id: &str, variable_id: &str) {
        Self::write_tombstone(
            doc,
            root,
            VARIABLE_TOMBSTONES_KEY,
            &format!("{env_id}:{variable_id}"),
        );
    }
    /// Get-or-create a top-level `Text` object at `key`.
    fn text_obj(doc: &mut AutoCommit, root: &ObjId, key: &str) -> ObjId {
        match doc.get(root, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            // `put_object` on a fresh key of a live document does not fail in
            // practice; falling back to `root` itself (rather than panicking)
            // keeps a write against a genuinely corrupt document non-fatal.
            _ => doc
                .put_object(root, key, ObjType::Text)
                .unwrap_or_else(|_| root.clone()),
        }
    }

    /// Read a top-level `Text` object's contents (empty if absent).
    pub fn read_text(doc: &AutoCommit, root: &ObjId, key: &str) -> String {
        ui::doc::read_str_or_text(doc, root, key).unwrap_or_default()
    }

    /// Splice `value` into the `Text` object at `key`, but only when it
    /// actually differs — avoids churn that would defeat incremental sync.
    pub fn set_text(doc: &mut AutoCommit, root: &ObjId, key: &str, value: &str) {
        let obj = Self::text_obj(doc, root, key);
        ui::doc::splice_text_diffed(doc, &obj, value);
    }

    /// Set one scalar string field without turning a durable record into a
    /// monolithic JSON core.  This is used for record metadata where a
    /// concurrent whole-name winner is preferable to interleaving characters.
    fn put_string_if_changed(doc: &mut AutoCommit, map: &ObjId, key: &str, value: &str) {
        // `get` is Automerge's materialized deterministic winner. `get_all`
        // includes losing concurrent values, so treating *any* matching value
        // as current could incorrectly suppress a requested visible update.
        let current = doc
            .get(map, key)
            .ok()
            .flatten()
            .and_then(|(stored, _)| match stored {
                Value::Scalar(stored) => stored.as_str().map(str::to_owned),
                Value::Object(_) => None,
            });
        if current.as_deref() != Some(value) {
            let _ = doc.put(map, key, value);
        }
    }

    /// Store one boolean only when its observable value changes.  Tombstones
    /// and the enabled marker are deliberately scalar fields, but reapplying
    /// their idempotent actions must not create an ever-growing CRDT history.
    fn put_bool_if_changed(doc: &mut AutoCommit, map: &ObjId, key: &str, value: bool) {
        let current = doc
            .get(map, key)
            .ok()
            .flatten()
            .and_then(|(stored, _)| match stored {
                Value::Scalar(stored) => match stored.as_ref() {
                    ScalarValue::Boolean(stored) => Some(*stored),
                    _ => None,
                },
                Value::Object(_) => None,
            });
        if current != Some(value) {
            let _ = doc.put(map, key, value);
        }
    }

    /// Persist collections as keyed records. A deleted UUID is never revived by
    /// a stale whole-workspace flush; creating a genuinely new collection uses
    /// a fresh UUID.
    pub fn set_collections(doc: &mut AutoCommit, root: &ObjId, collections: &[Collection]) {
        let before = Self::collections(doc, root);
        let live: std::collections::HashSet<_> = collections
            .iter()
            .map(|collection| collection.id.to_string())
            .collect();
        for old in &before {
            if !live.contains(&old.id.to_string()) {
                Self::delete_collection(doc, root, &old.id.to_string());
            }
        }
        for collection in collections {
            let id = collection.id.to_string();
            Self::create_collection(doc, root, &id, &collection.name);
            let previous = before.iter().find(|old| old.id == collection.id);
            for request in &collection.requests {
                let old =
                    previous.and_then(|old| old.requests.iter().find(|item| item.id == request.id));
                Self::save_request(doc, root, &id, old, request);
            }
            if let Some(previous) = previous {
                for request in &previous.requests {
                    if !collection.requests.iter().any(|item| item.id == request.id) {
                        Self::delete_request(doc, root, &request.id.to_string());
                    }
                }
            }
        }
    }

    /// Read collections from every concurrently-created root. Any live
    /// tombstone wins over an old record from another root.
    pub fn collections(doc: &AutoCommit, root: &ObjId) -> Vec<Collection> {
        let mut records: Vec<Collection> = Vec::new();
        for fields in Self::record_roots(doc, root, COLLECTION_FIELDS_KEY) {
            for id in doc.keys(&fields) {
                if Self::is_tombstoned(doc, root, COLLECTION_TOMBSTONES_KEY, &id) {
                    continue;
                }
                let Ok(uuid) = id.parse() else { continue };
                let Some((Value::Object(ObjType::Map), map)) = doc.get(&fields, &id).ok().flatten()
                else {
                    continue;
                };
                let name = Self::read_text(doc, &map, "name");
                if let Some(record) = records.iter_mut().find(|record| record.id == uuid) {
                    if !name.is_empty() {
                        record.name = name;
                    }
                } else {
                    records.push(Collection {
                        id: uuid,
                        name,
                        requests: Vec::new(),
                        expanded: true,
                    });
                }
            }
        }
        let mut by_collection: std::collections::BTreeMap<String, Vec<Request>> =
            std::collections::BTreeMap::new();
        for fields in Self::record_roots(doc, root, SAVED_REQUEST_FIELDS_KEY) {
            for id in doc.keys(&fields) {
                if by_collection
                    .values()
                    .flatten()
                    .any(|request| request.id.to_string() == id)
                {
                    continue;
                }
                let Ok(uuid) = id.parse() else { continue };
                let request = Request {
                    id: uuid,
                    ..Request::new("Saved request")
                };
                let (collection_id, request) = Self::saved_request_fields(doc, root, "", request);
                if !collection_id.is_empty()
                    && !Self::is_tombstoned(doc, root, REQUEST_TOMBSTONES_KEY, &id)
                {
                    by_collection
                        .entry(collection_id)
                        .or_default()
                        .push(request);
                }
            }
        }
        for collection in &mut records {
            collection.requests = by_collection
                .remove(&collection.id.to_string())
                .unwrap_or_default();
        }
        records
    }

    pub fn set_environments(doc: &mut AutoCommit, root: &ObjId, envs: &[Environment]) {
        let before = Self::environments(doc, root);
        let live: std::collections::HashSet<_> = envs
            .iter()
            .map(|environment| environment.id.to_string())
            .collect();
        for old in &before {
            if !live.contains(&old.id.to_string()) {
                Self::delete_environment(doc, root, &old.id.to_string());
            }
        }
        for environment in envs {
            let env_id = environment.id.to_string();
            Self::create_environment(doc, root, &env_id, &environment.name);
            let previous = before.iter().find(|old| old.id == environment.id);
            for variable in &environment.variables {
                Self::set_variable(
                    doc,
                    root,
                    &env_id,
                    &variable.id,
                    &variable.key,
                    &variable.value,
                );
            }
            if let Some(previous) = previous {
                for variable in &previous.variables {
                    if !environment
                        .variables
                        .iter()
                        .any(|item| item.id == variable.id)
                    {
                        Self::delete_variable(doc, root, &env_id, &variable.id);
                    }
                }
            }
        }
    }

    pub fn environments(doc: &AutoCommit, root: &ObjId) -> Vec<Environment> {
        let mut records: Vec<Environment> = Vec::new();
        for fields in Self::record_roots(doc, root, ENVIRONMENT_FIELDS_KEY) {
            for id in doc.keys(&fields) {
                if Self::is_tombstoned(doc, root, ENVIRONMENT_TOMBSTONES_KEY, &id) {
                    continue;
                }
                let Ok(uuid) = id.parse() else { continue };
                let Some((Value::Object(ObjType::Map), map)) = doc.get(&fields, &id).ok().flatten()
                else {
                    continue;
                };
                let name = Self::read_text(doc, &map, "name");
                if let Some(record) = records.iter_mut().find(|record| record.id == uuid) {
                    if !name.is_empty() {
                        record.name = name;
                    }
                } else {
                    records.push(Environment {
                        id: uuid,
                        name,
                        variables: Vec::new(),
                    });
                }
            }
        }
        let mut variables: Vec<VariableRecord> = Vec::new();
        for fields in Self::record_roots(doc, root, VARIABLES_KEY) {
            for address in doc.keys(&fields) {
                if Self::is_tombstoned(doc, root, VARIABLE_TOMBSTONES_KEY, &address) {
                    continue;
                }
                let Some((Value::Object(ObjType::Map), map)) =
                    doc.get(&fields, &address).ok().flatten()
                else {
                    continue;
                };
                let environment_id = Self::read_text(doc, &map, "environment_id");
                let id = Self::read_text(doc, &map, "id");
                let key = Self::read_text(doc, &map, "key");
                if environment_id.is_empty() || id.is_empty() || key.is_empty() {
                    continue;
                }
                let value = Self::read_text(doc, &map, "value");
                let enabled = doc.get(&map, "enabled").ok().flatten().is_none_or(|(value, _)| {
                    matches!(value, Value::Scalar(value) if matches!(value.as_ref(), ScalarValue::Boolean(true)))
                });
                let row = VariableRecord {
                    environment_id,
                    variable: KeyValue {
                        id,
                        key,
                        value,
                        enabled,
                    },
                };
                if let Some(old) = variables.iter_mut().find(|old| {
                    old.environment_id == row.environment_id && old.variable.id == row.variable.id
                }) {
                    *old = row;
                } else {
                    variables.push(row);
                }
            }
        }
        let mut by_environment: std::collections::BTreeMap<String, Vec<KeyValue>> =
            std::collections::BTreeMap::new();
        for record in variables {
            by_environment
                .entry(record.environment_id)
                .or_default()
                .push(record.variable);
        }
        for environment in &mut records {
            environment.variables = by_environment
                .remove(&environment.id.to_string())
                .unwrap_or_default();
        }
        records
    }

    pub fn set_active_env(doc: &mut AutoCommit, root: &ObjId, id: Option<&str>) {
        Self::put_string_if_changed(doc, root, "active_env", id.unwrap_or(""));
    }

    pub fn active_env(doc: &AutoCommit, root: &ObjId) -> Option<String> {
        match doc.get(root, "active_env") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Persist the active request as one explicit record. Its URL/body fields
    /// remain independently live `Text` objects for editor collaboration.
    pub fn set_request(doc: &mut AutoCommit, root: &ObjId, req: &Request) {
        let before = Self::request(doc, root).unwrap_or_else(|| req.clone());
        let map = Self::ensure_single_record_map(doc, root, REQUEST_KEY);
        let Some(map) = map else { return };
        // URL and body remain character-granular Automerge Text objects; the
        // rest are independent keyed fields, so a concurrent header edit does
        // not replace another peer's method or auth edit.
        Self::set_text(doc, root, "url", &req.url);
        Self::set_text(doc, root, "body", &req.body_text);
        Self::put_json_if_changed(doc, &map, "id", &req.id);
        Self::put_json_if_changed(doc, &map, "name", &req.name);
        Self::put_json_if_changed(doc, &map, "method", &req.method);
        Self::put_json_if_changed(doc, &map, "auth", &req.auth);
        Self::put_json_if_changed(doc, &map, "body_mode", &req.body_mode);
        Self::sync_active_key_values(doc, root, &before, req);
    }

    /// Read the active request snapshot, if present.
    pub fn request(doc: &AutoCommit, root: &ObjId) -> Option<Request> {
        let roots = Self::record_roots(doc, root, REQUEST_KEY);
        let mut request = roots
            .iter()
            .find_map(|map| Self::record_at(doc, map, "active"))
            .or_else(|| (!roots.is_empty()).then(|| Request::new("Untitled request")))?;
        for map in &roots {
            if let Some(id) = Self::record_at(doc, map, "id") {
                request.id = id;
            }
            if let Some(name) = Self::record_at(doc, map, "name") {
                request.name = name;
            }
            if let Some(method) = Self::record_at(doc, map, "method") {
                request.method = method;
            }
            if let Some(auth) = Self::record_at(doc, map, "auth") {
                request.auth = auth;
            }
            if let Some(body_mode) = Self::record_at(doc, map, "body_mode") {
                request.body_mode = body_mode;
            }
        }
        let values = Self::read_record_store::<ActiveKeyValueRecord>(
            doc,
            root,
            ACTIVE_KEY_VALUES_KEY,
            ACTIVE_KEY_VALUE_TOMBSTONES_KEY,
        );
        if !values.is_empty() || !Self::record_roots(doc, root, ACTIVE_KEY_VALUES_KEY).is_empty() {
            request.params = Self::active_key_values(&values, "params");
            request.headers = Self::active_key_values(&values, "headers");
            request.form = Self::active_key_values(&values, "form");
        }
        let url = Self::read_text(doc, root, "url");
        if !url.is_empty() {
            request.url = url;
        }
        let body = Self::read_text(doc, root, "body");
        if !body.is_empty() {
            request.body_text = body;
        }
        Some(request)
    }

    pub fn set_history(doc: &mut AutoCommit, root: &ObjId, history: &[HistoryEntry]) {
        let before = Self::history(doc, root);
        let Some((records, tombstones)) = Self::ensure_record_store(
            doc,
            root,
            HISTORY_KEY,
            HISTORY_TOMBSTONES_KEY,
            &before,
            |entry| entry.id.to_string(),
        ) else {
            return;
        };
        Self::sync_record_store(doc, &records, &tombstones, &before, history, |entry| {
            entry.id.to_string()
        });
        if let Some(order) = Self::ensure_history_order(doc, root, history) {
            Self::sync_history_order(doc, &order, history);
        }
    }

    pub fn history(doc: &AutoCommit, root: &ObjId) -> Vec<HistoryEntry> {
        let records = Self::read_record_store(doc, root, HISTORY_KEY, HISTORY_TOMBSTONES_KEY);
        let mut by_id: std::collections::BTreeMap<_, _> = records
            .into_iter()
            .map(|entry: HistoryEntry| (entry.id.to_string(), entry))
            .collect();
        let mut ordered = Vec::new();
        for id in Self::history_order(doc, root) {
            if let Some(entry) = by_id.remove(&id) {
                ordered.push(entry);
            }
        }
        // A concurrent peer can add a record before its order-list insertion
        // has replicated. Keep it visible until the list catches up.
        ordered.extend(by_id.into_values());
        ordered
    }

    fn record_roots(doc: &AutoCommit, root: &ObjId, key: &str) -> Vec<ObjId> {
        doc.get_all(root, key)
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|(value, id)| {
                        matches!(value, Value::Object(ObjType::Map)).then_some(id)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn record_at<T: serde::de::DeserializeOwned>(
        doc: &AutoCommit,
        map: &ObjId,
        id: &str,
    ) -> Option<T> {
        doc.get(map, id).ok()?.and_then(|(value, _)| {
            if let Value::Scalar(value) = value {
                value
                    .as_str()
                    .and_then(|json| serde_json::from_str(json).ok())
            } else {
                None
            }
        })
    }

    fn put_json_if_changed<T: serde::Serialize>(
        doc: &mut AutoCommit,
        map: &ObjId,
        key: &str,
        value: &T,
    ) {
        let Ok(json) = serde_json::to_string(value) else {
            return;
        };
        let already_current = doc.get(map, key).ok().flatten().is_some_and(|(stored, _)| {
            matches!(stored, Value::Scalar(stored) if stored.as_str() == Some(json.as_str()))
        });
        if !already_current {
            let _ = doc.put(map, key, json);
        }
    }

    fn tombstone_ids(doc: &AutoCommit, root: &ObjId, key: &str) -> Vec<String> {
        let mut ids = std::collections::BTreeSet::new();
        for map in Self::record_roots(doc, root, key) {
            ids.extend(doc.keys(&map));
        }
        ids.into_iter().collect()
    }

    fn is_tombstoned(doc: &AutoCommit, root: &ObjId, key: &str, id: &str) -> bool {
        Self::record_roots(doc, root, key).into_iter().any(|map| {
            doc.get_all(&map, id).is_ok_and(|values| {
                values.into_iter().any(|(value, _)| {
                    matches!(value, Value::Scalar(value) if matches!(value.as_ref(), ScalarValue::Boolean(true)))
                })
            })
        })
    }

    fn read_record_store<T: serde::de::DeserializeOwned>(
        doc: &AutoCommit,
        root: &ObjId,
        records_key: &str,
        tombstones_key: &str,
    ) -> Vec<T> {
        let mut values = std::collections::BTreeMap::new();
        for map in Self::record_roots(doc, root, records_key) {
            for id in doc.keys(&map) {
                if Self::is_tombstoned(doc, root, tombstones_key, &id) {
                    continue;
                }
                if let Some(value) = Self::record_at(doc, &map, &id) {
                    // `record_at` intentionally follows Automerge's
                    // materialized winner, the same explicit policy used by
                    // scalar writes above.
                    values.entry(id).or_insert(value);
                }
            }
        }
        values.into_values().collect()
    }

    /// Ensure a single normal map root. When two first-run peers each created
    /// a root, project all live records and tombstones into one replacement map
    /// before writing. Until that observed consolidation, readers see both.
    fn ensure_record_store<T, F>(
        doc: &mut AutoCommit,
        root: &ObjId,
        records_key: &str,
        tombstones_key: &str,
        inherited: &[T],
        id_of: F,
    ) -> Option<(ObjId, ObjId)>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Clone,
        F: Fn(&T) -> String + Copy,
    {
        let roots = Self::record_roots(doc, root, records_key);
        let tombstone_roots = Self::record_roots(doc, root, tombstones_key);
        if let [records] = roots.as_slice()
            && let [tombstones] = tombstone_roots.as_slice()
        {
            return Some((records.clone(), tombstones.clone()));
        }

        let tombstones = Self::tombstone_ids(doc, root, tombstones_key);
        if !roots.is_empty() {
            let _ = doc.delete(root, records_key);
        }
        if !tombstone_roots.is_empty() {
            let _ = doc.delete(root, tombstones_key);
        }
        let records = doc.put_object(root, records_key, ObjType::Map).ok()?;
        let tombstone_map = doc.put_object(root, tombstones_key, ObjType::Map).ok()?;
        for record in inherited {
            if let Ok(json) = serde_json::to_string(record) {
                let _ = doc.put(&records, id_of(record), json);
            }
        }
        for id in tombstones {
            let _ = doc.put(&tombstone_map, id, true);
        }
        Some((records, tombstone_map))
    }

    fn sync_record_store<T, F>(
        doc: &mut AutoCommit,
        records: &ObjId,
        tombstones: &ObjId,
        before: &[T],
        after: &[T],
        id_of: F,
    ) where
        T: serde::Serialize,
        F: Fn(&T) -> String,
    {
        let live: std::collections::HashSet<_> = after.iter().map(&id_of).collect();
        let known: std::collections::HashSet<_> = before.iter().map(&id_of).collect();
        let existing: Vec<_> = doc.keys(records).collect();
        for id in existing {
            if known.contains(&id) && !live.contains(&id) {
                let _ = doc.put(tombstones, id, true);
            }
        }
        for record in after {
            if let Ok(json) = serde_json::to_string(record) {
                let id = id_of(record);
                let already_current = doc.get_all(records, &id).is_ok_and(|values| {
                    values.into_iter().any(|(value, _)| {
                        matches!(value, Value::Scalar(value) if value.as_str() == Some(json.as_str()))
                    })
                });
                if !already_current {
                    let _ = doc.put(records, id, json);
                }
            }
        }
    }

    fn saved_fields_root(doc: &mut AutoCommit, root: &ObjId) -> Option<ObjId> {
        if let Some(root) = Self::record_roots(doc, root, SAVED_REQUEST_FIELDS_KEY)
            .into_iter()
            .next()
        {
            Some(root)
        } else {
            doc.put_object(root, SAVED_REQUEST_FIELDS_KEY, ObjType::Map)
                .ok()
        }
    }

    fn saved_request_map(doc: &mut AutoCommit, root: &ObjId, id: &str) -> Option<ObjId> {
        let fields = Self::saved_fields_root(doc, root)?;
        match doc.get(&fields, id).ok().flatten() {
            Some((Value::Object(ObjType::Map), map)) => Some(map),
            _ => doc.put_object(&fields, id, ObjType::Map).ok(),
        }
    }

    fn sync_saved_request_fields(
        doc: &mut AutoCommit,
        root: &ObjId,
        before: &[SavedRequestRecord],
        after: &[SavedRequestRecord],
    ) {
        let mut prior = std::collections::HashMap::new();
        for record in before {
            prior.insert(record.request.id.to_string(), record.request.clone());
        }
        for record in after {
            let id = record.request.id.to_string();
            let Some(map) = Self::saved_request_map(doc, root, &id) else {
                continue;
            };
            Self::put_json_if_changed(doc, &map, "collection_id", &record.collection_id);
            Self::put_json_if_changed(doc, &map, "name", &record.request.name);
            Self::put_json_if_changed(doc, &map, "method", &record.request.method);
            Self::put_json_if_changed(doc, &map, "auth", &record.request.auth);
            Self::put_json_if_changed(doc, &map, "body_mode", &record.request.body_mode);
            Self::set_text(doc, &map, "url", &record.request.url);
            Self::set_text(doc, &map, "body", &record.request.body_text);
            let previous = prior.remove(&id).unwrap_or_else(|| record.request.clone());
            Self::sync_request_rows(doc, &map, &previous, &record.request);
        }
    }

    /// Direct native write used by canonical Save/Rename actions.
    pub fn save_request(
        doc: &mut AutoCommit,
        root: &ObjId,
        collection_id: &str,
        before: Option<&Request>,
        request: &Request,
    ) {
        let old = before.cloned().unwrap_or_else(|| request.clone());
        let record = SavedRequestRecord {
            collection_id: collection_id.to_owned(),
            request: request.clone(),
        };
        let previous = SavedRequestRecord {
            collection_id: collection_id.to_owned(),
            request: old,
        };
        Self::sync_saved_request_fields(doc, root, &[previous], &[record]);
    }

    pub fn delete_request(doc: &mut AutoCommit, root: &ObjId, id: &str) {
        Self::write_tombstone(doc, root, REQUEST_TOMBSTONES_KEY, id);
    }

    fn saved_request_fields(
        doc: &AutoCommit,
        root: &ObjId,
        collection_id: &str,
        mut request: Request,
    ) -> (String, Request) {
        let Some(fields) = Self::record_roots(doc, root, SAVED_REQUEST_FIELDS_KEY)
            .into_iter()
            .next()
        else {
            return (collection_id.to_owned(), request);
        };
        let Some((Value::Object(ObjType::Map), map)) =
            doc.get(&fields, request.id.to_string()).ok().flatten()
        else {
            return (collection_id.to_owned(), request);
        };
        let collection_id =
            Self::record_at(doc, &map, "collection_id").unwrap_or_else(|| collection_id.to_owned());
        if let Some(value) = Self::record_at(doc, &map, "name") {
            request.name = value;
        }
        if let Some(value) = Self::record_at(doc, &map, "method") {
            request.method = value;
        }
        if let Some(value) = Self::record_at(doc, &map, "auth") {
            request.auth = value;
        }
        if let Some(value) = Self::record_at(doc, &map, "body_mode") {
            request.body_mode = value;
        }
        let url = Self::read_text(doc, &map, "url");
        if !url.is_empty() {
            request.url = url;
        }
        let body = Self::read_text(doc, &map, "body");
        if !body.is_empty() {
            request.body_text = body;
        }
        Self::read_request_rows(doc, &map, &mut request);
        (collection_id, request)
    }

    fn sync_request_rows(doc: &mut AutoCommit, map: &ObjId, before: &Request, after: &Request) {
        for (section, old, new) in [
            ("params", &before.params, &after.params),
            ("headers", &before.headers, &after.headers),
            ("form", &before.form, &after.form),
        ] {
            let tombstones = format!("{section}_tombstones");
            if let Some((records, removed)) =
                Self::ensure_record_store(doc, map, section, &tombstones, old, |row: &KeyValue| {
                    row.id.clone()
                })
            {
                Self::sync_record_store(doc, &records, &removed, old, new, |row| row.id.clone());
            }
        }
    }

    fn read_request_rows(doc: &AutoCommit, map: &ObjId, request: &mut Request) {
        for (section, target) in [
            ("params", &mut request.params),
            ("headers", &mut request.headers),
            ("form", &mut request.form),
        ] {
            let tombstones = format!("{section}_tombstones");
            let rows = Self::read_record_store(doc, map, section, &tombstones);
            if !rows.is_empty() || !Self::record_roots(doc, map, section).is_empty() {
                *target = rows;
            }
        }
    }

    /// Parameter, header, and form rows are durable children of the active
    /// request. Their minted row ids remain stable if a visible key is renamed
    /// or duplicated, so independent header/parameter rows never share a
    /// CRDT record.
    fn sync_active_key_values(
        doc: &mut AutoCommit,
        root: &ObjId,
        before: &Request,
        after: &Request,
    ) {
        let flatten = |request: &Request| {
            [
                ("params", &request.params),
                ("headers", &request.headers),
                ("form", &request.form),
            ]
            .into_iter()
            .flat_map(|(section, rows)| {
                rows.iter()
                    .filter(|row| !row.is_blank())
                    .cloned()
                    .map(move |value| ActiveKeyValueRecord {
                        section: section.to_string(),
                        value,
                    })
            })
            .collect::<Vec<_>>()
        };
        let before = flatten(before);
        let after = flatten(after);
        let id_of =
            |record: &ActiveKeyValueRecord| format!("{}:{}", record.section, record.value.id);
        let Some((records, tombstones)) = Self::ensure_record_store(
            doc,
            root,
            ACTIVE_KEY_VALUES_KEY,
            ACTIVE_KEY_VALUE_TOMBSTONES_KEY,
            &before,
            id_of,
        ) else {
            return;
        };
        Self::sync_record_store(doc, &records, &tombstones, &before, &after, id_of);
    }

    fn active_key_values(values: &[ActiveKeyValueRecord], section: &str) -> Vec<KeyValue> {
        values
            .iter()
            .filter(|record| record.section == section)
            .map(|record| record.value.clone())
            .collect()
    }

    fn ensure_single_record_map(doc: &mut AutoCommit, root: &ObjId, key: &str) -> Option<ObjId> {
        let roots = Self::record_roots(doc, root, key);
        if let [map] = roots.as_slice() {
            return Some(map.clone());
        }
        let inherited: Option<Request> = roots
            .iter()
            .find_map(|map| Self::record_at(doc, map, "active"));
        if !roots.is_empty() {
            let _ = doc.delete(root, key);
        }
        let map = doc.put_object(root, key, ObjType::Map).ok()?;
        if let Some(request) = inherited
            && let Ok(json) = serde_json::to_string(&request)
        {
            let _ = doc.put(&map, "active", json);
        }
        Some(map)
    }

    fn history_order_roots(doc: &AutoCommit, root: &ObjId) -> Vec<ObjId> {
        doc.get_all(root, HISTORY_ORDER_KEY)
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|(value, id)| {
                        matches!(value, Value::Object(ObjType::List)).then_some(id)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn string_list(doc: &AutoCommit, list: &ObjId) -> Vec<String> {
        (0..doc.length(list))
            .filter_map(|index| match doc.get(list, index).ok().flatten() {
                Some((Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
                _ => None,
            })
            .collect()
    }

    fn history_order(doc: &AutoCommit, root: &ObjId) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        Self::history_order_roots(doc, root)
            .into_iter()
            .flat_map(|list| Self::string_list(doc, &list))
            .filter(|id| seen.insert(id.clone()))
            .collect()
    }

    /// The app caps history at 50 rows, making an explicit CRDT order list a
    /// small bounded companion to the independently mergeable entry records.
    fn ensure_history_order(
        doc: &mut AutoCommit,
        root: &ObjId,
        inherited: &[HistoryEntry],
    ) -> Option<ObjId> {
        let roots = Self::history_order_roots(doc, root);
        if let [list] = roots.as_slice() {
            return Some(list.clone());
        }
        let existing = Self::history_order(doc, root);
        let order: Vec<_> = if existing.is_empty() {
            inherited.iter().map(|entry| entry.id.to_string()).collect()
        } else {
            existing
        };
        if !roots.is_empty() {
            let _ = doc.delete(root, HISTORY_ORDER_KEY);
        }
        let list = doc
            .put_object(root, HISTORY_ORDER_KEY, ObjType::List)
            .ok()?;
        for (index, id) in order.iter().enumerate() {
            let _ = doc.insert(&list, index, id);
        }
        Some(list)
    }

    fn sync_history_order(doc: &mut AutoCommit, list: &ObjId, history: &[HistoryEntry]) {
        let desired: Vec<_> = history.iter().map(|entry| entry.id.to_string()).collect();
        if Self::string_list(doc, list) == desired {
            return;
        }
        for index in (0..doc.length(list)).rev() {
            let _ = doc.delete(list, index);
        }
        for (index, id) in desired.iter().enumerate() {
            let _ = doc.insert(list, index, id);
        }
    }

    /// Whether the document has been seeded with any workspace content yet.
    pub fn is_empty(doc: &AutoCommit, root: &ObjId) -> bool {
        Self::collections(doc, root).is_empty() && Self::request(doc, root).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HttpMethod, KeyValue};
    use tilo_automerge::ApplyIncoming;

    fn doc() -> (AutoCommit, ObjId) {
        (AutoCommit::new(), automerge::ROOT)
    }

    /// Each record family written through `ApiDoc` reads back unchanged, from a
    /// fresh document per case.
    #[test]
    fn records_round_trip_through_the_document() {
        collections_round_trip();
        environments_and_active_round_trip();
        request_snapshot_round_trips();
        url_text_is_live_and_editable();
        history_keeps_the_newest_first_order_while_records_stay_keyed();
    }

    /// Case of `records_round_trip_through_the_document`.
    fn collections_round_trip() {
        let (mut d, root) = doc();
        let mut c = Collection::new("Sample");
        c.requests.push(Request::new("List users"));
        ApiDoc::set_collections(&mut d, &root, &[c.clone()]);
        let back = ApiDoc::collections(&d, &root);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Sample");
        assert_eq!(back[0].requests[0].name, "List users");
    }

    /// Case of `records_round_trip_through_the_document`.
    fn environments_and_active_round_trip() {
        let (mut d, root) = doc();
        let mut e = Environment::new("Production");
        e.variables
            .push(KeyValue::new("base_url", "https://api.example.com"));
        ApiDoc::set_environments(&mut d, &root, &[e.clone()]);
        ApiDoc::set_active_env(&mut d, &root, Some(&e.id.to_string()));
        assert_eq!(ApiDoc::environments(&d, &root)[0].name, "Production");
        assert_eq!(ApiDoc::active_env(&d, &root), Some(e.id.to_string()));
    }

    /// Case of `records_round_trip_through_the_document`.
    fn request_snapshot_round_trips() {
        let (mut d, root) = doc();
        let mut r = Request::new("Create user");
        r.method = HttpMethod::Post;
        r.headers.push(KeyValue::new("Accept", "application/json"));
        ApiDoc::set_request(&mut d, &root, &r);
        let back = ApiDoc::request(&d, &root).unwrap();
        assert_eq!(back.method, HttpMethod::Post);
        assert_eq!(back.headers[0].key, "Accept");
    }

    /// Case of `records_round_trip_through_the_document`.
    fn url_text_is_live_and_editable() {
        let (mut d, root) = doc();
        let obj = ApiDoc::text_obj(&mut d, &root, "url");
        let _ = d.splice_text(&obj, 0, 0, "https://api.example.com/users");
        assert_eq!(
            ApiDoc::read_text(&d, &root, "url"),
            "https://api.example.com/users"
        );
    }

    /// Anti-churn: equal writes never advance the document and a one-character
    /// edit splices one character, from a fresh document per case.
    #[test]
    fn unchanged_writes_emit_no_ops_and_edits_are_bounded_splices() {
        set_text_is_idempotent();
        repeated_native_scalar_writes_are_noops();
        appending_one_char_is_a_bounded_splice();
        repeated_set_text_keeps_doc_bounded();
    }

    /// Case of `unchanged_writes_emit_no_ops_and_edits_are_bounded_splices`.
    fn set_text_is_idempotent() {
        let (mut d, root) = doc();
        ApiDoc::set_text(&mut d, &root, "request", "{}");
        let heads_before = d.get_heads();
        ApiDoc::set_text(&mut d, &root, "request", "{}");
        // No-op write must not advance the document.
        assert_eq!(heads_before, d.get_heads());
    }

    /// Case of `unchanged_writes_emit_no_ops_and_edits_are_bounded_splices`.
    fn repeated_native_scalar_writes_are_noops() {
        let (mut d, root) = doc();
        let environment = Environment::new("Production");
        let environment_id = environment.id.to_string();
        let variable_id = uuid::Uuid::new_v4().to_string();
        ApiDoc::create_environment(&mut d, &root, &environment_id, &environment.name);
        ApiDoc::set_variable(
            &mut d,
            &root,
            &environment_id,
            &variable_id,
            "base_url",
            "https://api.example.test",
        );
        ApiDoc::set_active_env(&mut d, &root, Some(&environment_id));
        let heads_before = d.get_heads();

        ApiDoc::set_variable(
            &mut d,
            &root,
            &environment_id,
            &variable_id,
            "base_url",
            "https://api.example.test",
        );
        ApiDoc::set_active_env(&mut d, &root, Some(&environment_id));

        assert_eq!(
            heads_before,
            d.get_heads(),
            "replaying equal variable and environment selections must not grow the CRDT"
        );
    }

    /// Scalar and JSON `*_if_changed` writes compare against the visible
    /// winner, not any conflicting loser.
    #[test]
    fn if_changed_writes_replace_a_visible_conflict_winner_even_if_loser_matches() {
        scalar_update_replaces_a_visible_conflict_winner_even_if_loser_matches();
        json_update_replaces_a_visible_conflict_winner_even_if_loser_matches();
    }

    /// Case of `if_changed_writes_replace_a_visible_conflict_winner_even_if_loser_matches`.
    fn scalar_update_replaces_a_visible_conflict_winner_even_if_loser_matches() {
        let (mut base, root) = doc();
        let map = base
            .put_object(&root, "scalars", ObjType::Map)
            .expect("test map");
        base.put(&map, "name", "base").expect("initial scalar");
        let bytes = base.save();
        let mut left = AutoCommit::load(&bytes).expect("left peer");
        let mut right = AutoCommit::load(&bytes).expect("right peer");
        left.put(&map, "name", "left").expect("left write");
        right.put(&map, "name", "right").expect("right write");
        left.merge_incoming(&mut right)
            .expect("merge concurrent writes");

        let visible = left
            .get(&map, "name")
            .expect("read winner")
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
            .expect("string winner");
        let losing = left
            .get_all(&map, "name")
            .expect("read conflicts")
            .into_iter()
            .filter_map(|(value, _)| value.as_str().map(str::to_owned))
            .find(|value| value != &visible)
            .expect("a concurrent losing value");

        ApiDoc::put_string_if_changed(&mut left, &map, "name", &losing);
        assert_eq!(
            left.get(&map, "name")
                .expect("read requested winner")
                .and_then(|(value, _)| value.as_str().map(str::to_owned)),
            Some(losing),
            "requesting a value held only by a losing conflict updates the visible result"
        );
    }

    /// Case of `if_changed_writes_replace_a_visible_conflict_winner_even_if_loser_matches`.
    fn json_update_replaces_a_visible_conflict_winner_even_if_loser_matches() {
        let (mut base, root) = doc();
        let map = base
            .put_object(&root, "records", ObjType::Map)
            .expect("test map");
        ApiDoc::put_json_if_changed(
            &mut base,
            &map,
            "record",
            &serde_json::json!({"name": "base"}),
        );
        let bytes = base.save();
        let mut left = AutoCommit::load(&bytes).expect("left peer");
        let mut right = AutoCommit::load(&bytes).expect("right peer");
        ApiDoc::put_json_if_changed(
            &mut left,
            &map,
            "record",
            &serde_json::json!({"name": "left"}),
        );
        ApiDoc::put_json_if_changed(
            &mut right,
            &map,
            "record",
            &serde_json::json!({"name": "right"}),
        );
        left.merge_incoming(&mut right)
            .expect("merge concurrent records");

        let visible = left
            .get(&map, "record")
            .expect("read winner")
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
            .expect("JSON winner");
        let losing = left
            .get_all(&map, "record")
            .expect("read conflicts")
            .into_iter()
            .filter_map(|(value, _)| value.as_str().map(str::to_owned))
            .find(|value| value != &visible)
            .expect("a concurrent losing record");
        let requested: serde_json::Value = serde_json::from_str(&losing).expect("valid JSON");

        ApiDoc::put_json_if_changed(&mut left, &map, "record", &requested);
        assert_eq!(
            left.get(&map, "record")
                .expect("read requested winner")
                .and_then(|(value, _)| value.as_str().map(str::to_owned)),
            Some(losing),
            "a requested losing JSON record replaces the materialized winner"
        );
    }

    /// Case of `unchanged_writes_emit_no_ops_and_edits_are_bounded_splices`.
    fn appending_one_char_is_a_bounded_splice() {
        // Anti-churn: growing a long Text field by one char must splice only
        // the new char, not rewrite the whole value.
        let (mut d, root) = doc();
        let long = "x".repeat(300);
        ApiDoc::set_text(&mut d, &root, "body", &long);
        let before = d.save().len();
        let mut longer = long.clone();
        longer.push('!');
        ApiDoc::set_text(&mut d, &root, "body", &longer);
        let grew = d.save().len() - before;
        assert_eq!(
            ApiDoc::read_text(&d, &root, "body"),
            longer,
            "value round-trips"
        );
        assert!(
            grew < 40,
            "appending one char grew the doc by {grew} bytes (expected a single-char splice)"
        );
    }

    /// Case of `unchanged_writes_emit_no_ops_and_edits_are_bounded_splices`.
    fn repeated_set_text_keeps_doc_bounded() {
        // Idempotent store: re-writing the same text must emit no ops, so the
        // serialized document does not grow at all.
        let (mut d, root) = doc();
        let long = "y".repeat(300);
        ApiDoc::set_text(&mut d, &root, "body", &long);
        let a = d.save().len();
        for _ in 0..50 {
            ApiDoc::set_text(&mut d, &root, "body", &long);
        }
        let b = d.save().len();
        assert_eq!(
            a, b,
            "re-storing the same text grew the document {a} -> {b}"
        );
    }

    /// Peers that each write whole records (or a first native root) merge into
    /// one deterministic, consolidatable projection.
    #[test]
    fn concurrent_record_roots_merge_and_converge() {
        two_docs_merge_collections();
        concurrent_first_native_record_roots_keep_independent_records();
        same_collection_record_conflict_is_stable_in_both_merge_directions();
        concurrent_active_request_roots_are_read_then_consolidated();
    }

    /// Case of `concurrent_record_roots_merge_and_converge`.
    fn two_docs_merge_collections() {
        let (mut a, root) = doc();
        ApiDoc::set_collections(&mut a, &root, &[Collection::new("A")]);
        let mut b = AutoCommit::load(&a.save()).unwrap();
        ApiDoc::set_environments(&mut b, &root, &[Environment::new("Env-B")]);
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge_incoming(&mut b_loaded).unwrap();
        assert_eq!(ApiDoc::collections(&a, &root)[0].name, "A");
        assert_eq!(ApiDoc::environments(&a, &root)[0].name, "Env-B");
    }

    /// Case of `concurrent_record_roots_merge_and_converge`.
    fn concurrent_first_native_record_roots_keep_independent_records() {
        let (mut left, root) = doc();
        let (mut right, _) = doc();
        left.set_actor(automerge::ActorId::from([1; 16].as_slice()));
        right.set_actor(automerge::ActorId::from([2; 16].as_slice()));
        let left_collection = Collection::new("Left collection");
        let right_collection = Collection::new("Right collection");
        ApiDoc::set_collections(&mut left, &root, std::slice::from_ref(&left_collection));
        ApiDoc::set_collections(&mut right, &root, std::slice::from_ref(&right_collection));

        left.merge_incoming(&mut right).unwrap();
        let merged = ApiDoc::collections(&left, &root);
        assert!(merged.iter().any(|item| item.id == left_collection.id));
        assert!(merged.iter().any(|item| item.id == right_collection.id));
        ApiDoc::set_collections(&mut left, &root, &merged);
        let consolidated = ApiDoc::collections(&left, &root);
        assert_eq!(consolidated.len(), 2);
    }

    /// A record or row tombstone prevents a concurrent stale edit from reviving
    /// it, for every record family.
    #[test]
    fn deletes_win_over_concurrent_stale_edits() {
        collection_and_environment_deletes_win_over_concurrent_stale_edits();
        history_delete_wins_over_a_concurrent_stale_record_update();
        saved_request_delete_wins_over_a_concurrent_stale_edit();
        active_request_row_delete_wins_over_a_concurrent_stale_edit();
    }

    /// Case of `deletes_win_over_concurrent_stale_edits`.
    fn collection_and_environment_deletes_win_over_concurrent_stale_edits() {
        let (mut base, root) = doc();
        let collection = Collection::new("To remove");
        let environment = Environment::new("To remove");
        ApiDoc::set_collections(&mut base, &root, std::slice::from_ref(&collection));
        ApiDoc::set_environments(&mut base, &root, std::slice::from_ref(&environment));

        let mut delete = AutoCommit::load(&base.save()).unwrap();
        delete.set_actor(automerge::ActorId::from([2; 16].as_slice()));
        let mut stale = AutoCommit::load(&base.save()).unwrap();
        stale.set_actor(automerge::ActorId::from([3; 16].as_slice()));
        ApiDoc::set_collections(&mut delete, &root, &[]);
        ApiDoc::set_environments(&mut delete, &root, &[]);
        let mut stale_collection = collection;
        stale_collection.name = "Stale edit".into();
        let mut stale_environment = environment;
        stale_environment.name = "Stale edit".into();
        ApiDoc::set_collections(&mut stale, &root, &[stale_collection]);
        ApiDoc::set_environments(&mut stale, &root, &[stale_environment]);

        delete.merge_incoming(&mut stale).unwrap();
        assert!(ApiDoc::collections(&delete, &root).is_empty());
        assert!(ApiDoc::environments(&delete, &root).is_empty());
    }

    /// Case of `concurrent_record_roots_merge_and_converge`.
    fn same_collection_record_conflict_is_stable_in_both_merge_directions() {
        let (mut base, root) = doc();
        let collection = Collection::new("Original");
        ApiDoc::set_collections(&mut base, &root, std::slice::from_ref(&collection));
        let mut left = AutoCommit::load(&base.save()).unwrap();
        left.set_actor(automerge::ActorId::from([4; 16].as_slice()));
        let mut right = AutoCommit::load(&base.save()).unwrap();
        right.set_actor(automerge::ActorId::from([5; 16].as_slice()));
        let mut left_value = collection.clone();
        left_value.name = "Left change".into();
        let mut right_value = collection;
        right_value.name = "Right change".into();
        ApiDoc::set_collections(&mut left, &root, &[left_value]);
        ApiDoc::set_collections(&mut right, &root, &[right_value]);

        let mut forward = AutoCommit::load(&left.save()).unwrap();
        let mut forward_other = AutoCommit::load(&right.save()).unwrap();
        forward.merge_incoming(&mut forward_other).unwrap();
        let mut reverse = AutoCommit::load(&right.save()).unwrap();
        let mut reverse_other = AutoCommit::load(&left.save()).unwrap();
        reverse.merge_incoming(&mut reverse_other).unwrap();
        let forward_name = ApiDoc::collections(&forward, &root)[0].name.clone();
        let reverse_name = ApiDoc::collections(&reverse, &root)[0].name.clone();
        assert_eq!(forward_name, reverse_name);
        assert!(matches!(
            forward_name.as_str(),
            "Left change" | "Right change"
        ));
    }

    /// Case of `deletes_win_over_concurrent_stale_edits`.
    fn history_delete_wins_over_a_concurrent_stale_record_update() {
        let (mut base, root) = doc();
        let entry = HistoryEntry {
            id: uuid::Uuid::new_v4(),
            method: HttpMethod::Get,
            url: "https://example.test/old".into(),
            status: 200,
            duration_ms: 10,
            when: "just now".into(),
        };
        ApiDoc::set_history(&mut base, &root, std::slice::from_ref(&entry));
        let mut delete = AutoCommit::load(&base.save()).unwrap();
        delete.set_actor(automerge::ActorId::from([6; 16].as_slice()));
        let mut stale = AutoCommit::load(&base.save()).unwrap();
        stale.set_actor(automerge::ActorId::from([7; 16].as_slice()));
        ApiDoc::set_history(&mut delete, &root, &[]);
        let mut stale_entry = entry;
        stale_entry.status = 503;
        ApiDoc::set_history(&mut stale, &root, &[stale_entry]);

        delete.merge_incoming(&mut stale).unwrap();
        assert!(ApiDoc::history(&delete, &root).is_empty());
    }

    /// Case of `records_round_trip_through_the_document`.
    fn history_keeps_the_newest_first_order_while_records_stay_keyed() {
        let (mut d, root) = doc();
        let older = HistoryEntry {
            id: uuid::Uuid::from_u128(1),
            method: HttpMethod::Get,
            url: "https://example.test/older".into(),
            status: 200,
            duration_ms: 10,
            when: "1m ago".into(),
        };
        let newer = HistoryEntry {
            id: uuid::Uuid::from_u128(2),
            method: HttpMethod::Post,
            url: "https://example.test/newer".into(),
            status: 201,
            duration_ms: 12,
            when: "just now".into(),
        };
        ApiDoc::set_history(&mut d, &root, &[newer.clone(), older.clone()]);
        let round_trip = ApiDoc::history(&d, &root);
        assert_eq!(
            round_trip.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [newer.id, older.id]
        );
        assert_eq!(ApiDoc::history_order_roots(&d, &root).len(), 1);
    }

    /// Case of `concurrent_record_roots_merge_and_converge`.
    fn concurrent_active_request_roots_are_read_then_consolidated() {
        let (mut left, root) = doc();
        let (mut right, _) = doc();
        left.set_actor(automerge::ActorId::from([8; 16].as_slice()));
        right.set_actor(automerge::ActorId::from([9; 16].as_slice()));
        let left_request = Request::new("Left request");
        let right_request = Request::new("Right request");
        ApiDoc::set_request(&mut left, &root, &left_request);
        ApiDoc::set_request(&mut right, &root, &right_request);
        left.merge_incoming(&mut right).unwrap();

        let projected = ApiDoc::request(&left, &root).expect("one stable conflict projection");
        assert!(matches!(
            projected.name.as_str(),
            "Left request" | "Right request"
        ));
        assert_eq!(ApiDoc::record_roots(&left, &root, REQUEST_KEY).len(), 2);
        ApiDoc::set_request(&mut left, &root, &projected);
        assert_eq!(ApiDoc::record_roots(&left, &root, REQUEST_KEY).len(), 1);
        assert_eq!(ApiDoc::request(&left, &root), Some(projected));
    }

    /// Independent edits to different fields or rows of the same record survive
    /// a merge, each from fresh peers forked from their own base.
    #[test]
    fn concurrent_field_and_row_edits_merge_without_clobbering() {
        different_saved_requests_in_one_collection_merge_without_clobbering();
        same_saved_request_independent_fields_and_rows_merge();
        different_variables_in_one_environment_merge_without_clobbering();
        native_variable_rows_merge_and_tombstones_win();
        independent_active_request_fields_and_rows_merge_without_clobbering();
        different_active_header_rows_merge_without_clobbering();
    }

    /// Case of `concurrent_field_and_row_edits_merge_without_clobbering`.
    fn different_saved_requests_in_one_collection_merge_without_clobbering() {
        let (mut base, root) = doc();
        let mut collection = Collection::new("Shared");
        collection.requests = vec![Request::new("First"), Request::new("Second")];
        ApiDoc::set_collections(&mut base, &root, std::slice::from_ref(&collection));
        let mut left = AutoCommit::load(&base.save()).unwrap();
        left.set_actor(automerge::ActorId::from([10; 16].as_slice()));
        let mut right = AutoCommit::load(&base.save()).unwrap();
        right.set_actor(automerge::ActorId::from([11; 16].as_slice()));
        let mut left_collections = ApiDoc::collections(&left, &root);
        left_collections[0].requests[0].name = "First edited".into();
        ApiDoc::set_collections(&mut left, &root, &left_collections);
        let mut right_collections = ApiDoc::collections(&right, &root);
        right_collections[0].requests[1].name = "Second edited".into();
        ApiDoc::set_collections(&mut right, &root, &right_collections);

        left.merge_incoming(&mut right).unwrap();
        let requests = &ApiDoc::collections(&left, &root)[0].requests;
        assert!(
            requests
                .iter()
                .any(|request| request.name == "First edited"),
            "{requests:?}"
        );
        assert!(
            requests
                .iter()
                .any(|request| request.name == "Second edited"),
            "{requests:?}"
        );
        assert_eq!(requests.len(), 2, "each saved request has one keyed record");
    }

    /// Case of `concurrent_field_and_row_edits_merge_without_clobbering`.
    fn same_saved_request_independent_fields_and_rows_merge() {
        let (mut base, root) = doc();
        let mut collection = Collection::new("Shared");
        collection.requests = vec![Request::new("Shared request")];
        ApiDoc::set_collections(&mut base, &root, std::slice::from_ref(&collection));
        let mut left = AutoCommit::load(&base.save()).unwrap();
        left.set_actor(automerge::ActorId::from([30; 16].as_slice()));
        let mut right = AutoCommit::load(&base.save()).unwrap();
        right.set_actor(automerge::ActorId::from([31; 16].as_slice()));
        let mut left_collections = ApiDoc::collections(&left, &root);
        left_collections[0].requests[0].method = HttpMethod::Post;
        left_collections[0].requests[0]
            .params
            .push(KeyValue::new("page", "1"));
        ApiDoc::set_collections(&mut left, &root, &left_collections);
        let mut right_collections = ApiDoc::collections(&right, &root);
        right_collections[0].requests[0]
            .headers
            .push(KeyValue::new("Accept", "application/json"));
        ApiDoc::set_collections(&mut right, &root, &right_collections);

        left.merge_incoming(&mut right).unwrap();
        let request = &ApiDoc::collections(&left, &root)[0].requests[0];
        assert_eq!(request.method, HttpMethod::Post);
        assert!(request.params.iter().any(|row| row.key == "page"));
        assert!(request.headers.iter().any(|row| row.key == "Accept"));
    }

    /// Case of `deletes_win_over_concurrent_stale_edits`.
    fn saved_request_delete_wins_over_a_concurrent_stale_edit() {
        let (mut base, root) = doc();
        let mut collection = Collection::new("Shared");
        collection.requests = vec![Request::new("Delete me")];
        ApiDoc::set_collections(&mut base, &root, std::slice::from_ref(&collection));
        let mut delete = AutoCommit::load(&base.save()).unwrap();
        delete.set_actor(automerge::ActorId::from([18; 16].as_slice()));
        let mut stale = AutoCommit::load(&base.save()).unwrap();
        stale.set_actor(automerge::ActorId::from([19; 16].as_slice()));

        let mut removed = ApiDoc::collections(&delete, &root);
        removed[0].requests.clear();
        ApiDoc::set_collections(&mut delete, &root, &removed);
        let mut edited = ApiDoc::collections(&stale, &root);
        edited[0].requests[0].name = "Stale rename".into();
        ApiDoc::set_collections(&mut stale, &root, &edited);

        delete.merge_incoming(&mut stale).unwrap();
        assert!(
            ApiDoc::collections(&delete, &root)[0].requests.is_empty(),
            "the request tombstone prevents a concurrent record write from reviving it"
        );
    }

    /// Case of `concurrent_field_and_row_edits_merge_without_clobbering`.
    fn different_variables_in_one_environment_merge_without_clobbering() {
        let (mut base, root) = doc();
        let mut environment = Environment::new("Shared");
        environment.variables = vec![KeyValue::new("host", "one"), KeyValue::new("token", "two")];
        ApiDoc::set_environments(&mut base, &root, std::slice::from_ref(&environment));
        let mut left = AutoCommit::load(&base.save()).unwrap();
        left.set_actor(automerge::ActorId::from([12; 16].as_slice()));
        let mut right = AutoCommit::load(&base.save()).unwrap();
        right.set_actor(automerge::ActorId::from([13; 16].as_slice()));
        let mut left_environments = ApiDoc::environments(&left, &root);
        left_environments[0]
            .variables
            .iter_mut()
            .find(|variable| variable.key == "host")
            .unwrap()
            .value = "left".into();
        ApiDoc::set_environments(&mut left, &root, &left_environments);
        let mut right_environments = ApiDoc::environments(&right, &root);
        right_environments[0]
            .variables
            .iter_mut()
            .find(|variable| variable.key == "token")
            .unwrap()
            .value = "right".into();
        ApiDoc::set_environments(&mut right, &root, &right_environments);

        left.merge_incoming(&mut right).unwrap();
        let variables = &ApiDoc::environments(&left, &root)[0].variables;
        assert!(
            variables
                .iter()
                .any(|variable| variable.key == "host" && variable.value == "left"),
            "{variables:?}"
        );
        assert!(
            variables
                .iter()
                .any(|variable| variable.key == "token" && variable.value == "right"),
            "{variables:?}"
        );
    }

    /// Case of `concurrent_field_and_row_edits_merge_without_clobbering`.
    fn native_variable_rows_merge_and_tombstones_win() {
        let (mut base, root) = doc();
        let env = Environment::new("Production");
        ApiDoc::set_environments(&mut base, &root, std::slice::from_ref(&env));
        let mut left = AutoCommit::load(&base.save()).unwrap();
        left.set_actor(automerge::ActorId::from([41; 16].as_slice()));
        let mut right = AutoCommit::load(&base.save()).unwrap();
        right.set_actor(automerge::ActorId::from([42; 16].as_slice()));
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        ApiDoc::set_variable(
            &mut left,
            &root,
            &env.id.to_string(),
            &first,
            "region",
            "eu",
        );
        ApiDoc::set_variable(
            &mut right,
            &root,
            &env.id.to_string(),
            &second,
            "token",
            "abc",
        );
        left.merge_incoming(&mut right).unwrap();
        let vars = &ApiDoc::environments(&left, &root)[0].variables;
        assert!(
            vars.iter()
                .any(|row| row.id == first && row.key == "region")
        );
        assert!(
            vars.iter()
                .any(|row| row.id == second && row.key == "token")
        );
        assert!(
            ApiDoc::record_roots(&left, &root, VARIABLES_KEY)
                .iter()
                .any(|rows| left.keys(rows).next().is_some()),
            "native maps, not JSON cores"
        );

        let mut stale = AutoCommit::load(&left.save()).unwrap();
        stale.set_actor(automerge::ActorId::from([43; 16].as_slice()));
        ApiDoc::delete_variable(&mut left, &root, &env.id.to_string(), &first);
        ApiDoc::set_variable(
            &mut stale,
            &root,
            &env.id.to_string(),
            &first,
            "region",
            "stale",
        );
        left.merge_incoming(&mut stale).unwrap();
        assert!(
            !ApiDoc::environments(&left, &root)[0]
                .variables
                .iter()
                .any(|row| row.id == first),
            "a stable-row tombstone wins"
        );
    }

    /// Case of `concurrent_field_and_row_edits_merge_without_clobbering`.
    fn independent_active_request_fields_and_rows_merge_without_clobbering() {
        let (mut base, root) = doc();
        let request = Request::new("Shared request");
        ApiDoc::set_request(&mut base, &root, &request);
        let mut left = AutoCommit::load(&base.save()).unwrap();
        left.set_actor(automerge::ActorId::from([14; 16].as_slice()));
        let mut right = AutoCommit::load(&base.save()).unwrap();
        right.set_actor(automerge::ActorId::from([15; 16].as_slice()));
        let mut left_request = ApiDoc::request(&left, &root).unwrap();
        left_request.method = HttpMethod::Post;
        left_request.params.push(KeyValue::new("page", "1"));
        ApiDoc::set_request(&mut left, &root, &left_request);
        let mut right_request = ApiDoc::request(&right, &root).unwrap();
        right_request
            .headers
            .push(KeyValue::new("Accept", "application/json"));
        ApiDoc::set_request(&mut right, &root, &right_request);

        left.merge_incoming(&mut right).unwrap();
        let merged = ApiDoc::request(&left, &root).unwrap();
        assert_eq!(merged.method, HttpMethod::Post);
        assert_eq!(merged.params[0].key, "page");
        assert_eq!(merged.headers[0].key, "Accept");
    }

    /// Case of `concurrent_field_and_row_edits_merge_without_clobbering`.
    fn different_active_header_rows_merge_without_clobbering() {
        let (mut base, root) = doc();
        let mut request = Request::new("Shared request");
        request.headers = vec![
            KeyValue::new("Accept", "old"),
            KeyValue::new("X-Trace", "old"),
        ];
        ApiDoc::set_request(&mut base, &root, &request);
        let mut left = AutoCommit::load(&base.save()).unwrap();
        left.set_actor(automerge::ActorId::from([16; 16].as_slice()));
        let mut right = AutoCommit::load(&base.save()).unwrap();
        right.set_actor(automerge::ActorId::from([17; 16].as_slice()));
        let mut left_request = ApiDoc::request(&left, &root).unwrap();
        left_request
            .headers
            .iter_mut()
            .find(|row| row.key == "Accept")
            .unwrap()
            .value = "application/json".into();
        ApiDoc::set_request(&mut left, &root, &left_request);
        let mut right_request = ApiDoc::request(&right, &root).unwrap();
        right_request
            .headers
            .iter_mut()
            .find(|row| row.key == "X-Trace")
            .unwrap()
            .value = "run-42".into();
        ApiDoc::set_request(&mut right, &root, &right_request);

        left.merge_incoming(&mut right).unwrap();
        let merged = ApiDoc::request(&left, &root).unwrap();
        assert!(
            merged
                .headers
                .iter()
                .any(|row| row.key == "Accept" && row.value == "application/json")
        );
        assert!(
            merged
                .headers
                .iter()
                .any(|row| row.key == "X-Trace" && row.value == "run-42")
        );
    }

    /// Case of `deletes_win_over_concurrent_stale_edits`.
    fn active_request_row_delete_wins_over_a_concurrent_stale_edit() {
        let (mut base, root) = doc();
        let mut request = Request::new("Shared request");
        request.headers.push(KeyValue::new("Accept", "old"));
        ApiDoc::set_request(&mut base, &root, &request);
        let mut delete = AutoCommit::load(&base.save()).unwrap();
        delete.set_actor(automerge::ActorId::from([23; 16].as_slice()));
        let mut stale = AutoCommit::load(&base.save()).unwrap();
        stale.set_actor(automerge::ActorId::from([24; 16].as_slice()));

        let mut removed = ApiDoc::request(&delete, &root).unwrap();
        removed.headers.clear();
        ApiDoc::set_request(&mut delete, &root, &removed);
        let mut edited = ApiDoc::request(&stale, &root).unwrap();
        edited.headers[0].value = "application/json".into();
        ApiDoc::set_request(&mut stale, &root, &edited);

        delete.merge_incoming(&mut stale).unwrap();
        assert!(
            ApiDoc::request(&delete, &root).unwrap().headers.is_empty(),
            "the keyed header tombstone prevents a stale row edit from reviving it"
        );
    }
}
