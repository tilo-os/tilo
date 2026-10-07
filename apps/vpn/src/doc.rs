//! Automerge document model for the VPN vault.
//!
//! Shape:
//!
//! ```text
//! root
//! └── "connections" (Map, keyed by ConnId string)
//!     └── <id> (Map)
//!         ├── "name" / "kind" (Str)
//!         ├── "tor" (Bool)
//! ```
//!
//! Only secret-free, shareable profile identity lives here. Credentials,
//! endpoint/routing details, config paths, live connection state, handshakes
//! and traffic counters remain machine-local. This is intentional: a
//! collaborative profile tells another device *which* profile to provision,
//! never enough information to reconstruct a tunnel.
//! Storing connections as individual Automerge fields (rather than a JSON
//! blob) is what lets two peers concurrently edit the fleet and converge.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{read_bool, read_str};

use crate::model::{ConnId, ConnKind, ConnStatus, Connection, Vault};

pub struct VpnDoc;

impl VpnDoc {
    #[allow(
        clippy::expect_used,
        reason = "put_object on ROOT is infallible; see VpnDoc::write_connection's # Panics doc"
    )]
    fn connections_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "connections") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "connections", ObjType::Map)
                .expect("put_object connections never fails"),
        }
    }

    /// True when no connections have been written yet (fresh document).
    #[cfg(test)]
    pub fn is_empty(doc: &AutoCommit) -> bool {
        match doc.get(&ROOT, "connections") {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.keys(&id).next().is_none(),
            _ => true,
        }
    }

    /// Write the whole vault into the document (idempotent upsert).
    #[cfg(test)]
    pub fn write_vault(doc: &mut AutoCommit, vault: &Vault) {
        for c in &vault.connections {
            Self::write_connection(doc, c);
        }
    }

    /// Upsert one connection's fields.
    ///
    /// # Panics
    /// Never in practice: `put_object` only fails when the target object id
    /// doesn't resolve to a document object, and both `ROOT` and the freshly
    /// resolved `connections` map always do.
    #[allow(
        clippy::expect_used,
        reason = "put_object on an existing map object is infallible; see this function's # Panics doc"
    )]
    pub fn write_connection(doc: &mut AutoCommit, c: &Connection) {
        let conns = Self::connections_map(doc);
        let key = c.id.to_string();
        let obj = match doc.get(&conns, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&conns, key.as_str(), ObjType::Map)
                .expect("put_object connection never fails"),
        };
        let _ = doc.put(&obj, "name", c.name.clone());
        let _ = doc.put(&obj, "kind", c.kind.as_str());
        let _ = doc.put(&obj, "tor", ScalarValue::Boolean(c.tor));
    }

    /// Rename one saved connection (config field only — never touches live
    /// status/telemetry). No-op if the connection isn't present.
    pub fn set_name(doc: &mut AutoCommit, id: ConnId, name: String) {
        let conns = Self::connections_map(doc);
        if let Ok(Some((Value::Object(ObjType::Map), obj))) =
            doc.get(&conns, id.to_string().as_str())
        {
            let _ = doc.put(&obj, "name", name);
        }
    }

    pub fn set_conn_tor(doc: &mut AutoCommit, id: ConnId, on: bool) {
        let conns = Self::connections_map(doc);
        if let Ok(Some((Value::Object(ObjType::Map), obj))) =
            doc.get(&conns, id.to_string().as_str())
        {
            let _ = doc.put(&obj, "tor", ScalarValue::Boolean(on));
        }
    }

    pub fn remove_connection(doc: &mut AutoCommit, id: ConnId) {
        let conns = Self::connections_map(doc);
        let _ = doc.delete(&conns, id.to_string().as_str());
    }

    /// Read the whole vault back out of the document.
    pub fn read_vault(doc: &AutoCommit) -> Vault {
        let mut connections = Vec::new();
        if let Ok(Some((Value::Object(ObjType::Map), conns))) = doc.get(&ROOT, "connections") {
            for key in doc.keys(&conns) {
                let Ok(id) = key.parse::<ConnId>() else {
                    continue;
                };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&conns, key.as_str())
                else {
                    continue;
                };
                let s = |k: &str| read_str(doc, &obj, k).unwrap_or_default();
                connections.push(Connection {
                    id,
                    name: s("name"),
                    kind: ConnKind::parse(&s("kind")),
                    status: ConnStatus::Disconnected,
                    last_handshake_secs: None,
                    last_used: "Never on this device".into(),
                    tor: read_bool(doc, &obj, "tor").unwrap_or(false),
                });
            }
        }
        // Stable order: connected first, then by name.
        connections.sort_by(|a, b| {
            b.is_connected()
                .cmp(&a.is_connected())
                .then_with(|| a.name.cmp(&b.name))
        });
        Vault {
            connections,
            selected: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_vault;
    use tilo_automerge::ApplyIncoming;

    #[test]
    fn vault_document_round_trips_merges_and_excludes_live_state() {
        // Case `vault_round_trips_through_crdt`.
        {
            let mut doc = AutoCommit::new();
            let vault = sample_vault();
            VpnDoc::write_vault(&mut doc, &vault);
            let back = VpnDoc::read_vault(&doc);
            assert_eq!(back.connections.len(), vault.connections.len());
            assert_eq!(back.selected, None, "window selection is not synchronized");
            let home = back.connections.iter().find(|c| c.name == "Home").unwrap();
            assert_eq!(home.status, ConnStatus::Disconnected);
            assert_eq!(home.kind, ConnKind::WireGuard);
        }
        // Case `machine_live_state_is_not_persisted`.
        {
            let mut doc = AutoCommit::new();
            let vault = sample_vault();
            VpnDoc::write_vault(&mut doc, &vault);
            let back = VpnDoc::read_vault(&doc);
            assert!(back.connections.iter().all(|connection| {
                connection.status == ConnStatus::Disconnected
                    && connection.last_handshake_secs.is_none()
            }));
        }
        // Case `empty_detection_and_two_doc_merge`.
        {
            let mut a = AutoCommit::new();
            assert!(VpnDoc::is_empty(&a));
            VpnDoc::write_vault(&mut a, &sample_vault());
            assert!(!VpnDoc::is_empty(&a));

            // Peer B joins from A's state, adds one, A pulls it back.
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let extra = Connection::new("Travel", ConnKind::WireGuard);
            VpnDoc::write_connection(&mut b, &extra);
            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();
            let names: Vec<String> = VpnDoc::read_vault(&a)
                .connections
                .into_iter()
                .map(|c| c.name)
                .collect();
            assert!(names.contains(&"Travel".to_string()));
            assert!(names.contains(&"Home".to_string()));
        }
    }
}
