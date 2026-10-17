//! Read-only compatibility projection of the former API workspace format.
//! Kept at the import boundary; live editing uses the typed record exclusively.

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{Collection, Environment, HistoryEntry, KeyValue, Request};

/// Stateless namespace for the workspace document helpers.
pub(super) struct ApiDoc;

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

#[derive(Clone, serde::Deserialize)]
struct VariableRecord {
    environment_id: String,
    variable: KeyValue,
}

#[derive(Clone, serde::Deserialize)]
struct ActiveKeyValueRecord {
    section: String,
    value: KeyValue,
}

impl ApiDoc {
    pub fn read_text(doc: &AutoCommit, root: &ObjId, key: &str) -> String {
        ui::doc::read_str_or_text(doc, root, key).unwrap_or_default()
    }

    pub fn collections(doc: &AutoCommit, root: &ObjId) -> Vec<Collection> {
        let mut records: Vec<Collection> = Vec::new();
        for fields in Self::record_roots(doc, root, COLLECTION_FIELDS_KEY) {
            for id in doc.keys(&fields) {
                if Self::is_tombstoned(doc, root, COLLECTION_TOMBSTONES_KEY, &id) {
                    continue;
                }
                let uuid = ui::tilo_resource::Id::from_raw(id.clone());
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
                let uuid = ui::tilo_resource::Id::from_raw(id.clone());
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

    pub fn environments(doc: &AutoCommit, root: &ObjId) -> Vec<Environment> {
        let mut records: Vec<Environment> = Vec::new();
        for fields in Self::record_roots(doc, root, ENVIRONMENT_FIELDS_KEY) {
            for id in doc.keys(&fields) {
                if Self::is_tombstoned(doc, root, ENVIRONMENT_TOMBSTONES_KEY, &id) {
                    continue;
                }
                let uuid = ui::tilo_resource::Id::from_raw(id.clone());
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
                        secret: doc.get(&map, "secret").ok().flatten().is_some_and(|(value, _)| {
                            matches!(value, Value::Scalar(value) if matches!(value.as_ref(), ScalarValue::Boolean(true)))
                        }),
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

    pub fn active_env(doc: &AutoCommit, root: &ObjId) -> Option<String> {
        match doc.get(root, "active_env") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

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

    fn saved_request_fields(
        doc: &AutoCommit,
        root: &ObjId,
        collection_id: &str,
        mut request: Request,
    ) -> (String, Request) {
        let mut collection_id = collection_id.to_owned();
        for fields in Self::record_roots(doc, root, SAVED_REQUEST_FIELDS_KEY) {
            let Some((Value::Object(ObjType::Map), map)) =
                doc.get(&fields, request.id.to_string()).ok().flatten()
            else {
                continue;
            };
            collection_id = Self::record_at(doc, &map, "collection_id").unwrap_or(collection_id);
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
        }
        (collection_id, request)
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

    fn active_key_values(values: &[ActiveKeyValueRecord], section: &str) -> Vec<KeyValue> {
        values
            .iter()
            .filter(|record| record.section == section)
            .map(|record| record.value.clone())
            .collect()
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
}
