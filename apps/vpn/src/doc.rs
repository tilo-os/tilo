//! Automerge document model for the VPN vault.
//!
//! Shape:
//!
//! ```text
//! root
//! ├── "tor_global" (Bool)
//! └── "connections" (Map, keyed by ConnId string)
//!     └── <id> (Map)
//!         ├── "name" / "kind" / "endpoint" / "address" / "dns"
//!         │     "public_key" / "allowed_ips"   (Str)
//!         ├── "tor" (Bool)
//! ```
//!
//! Only shareable profile configuration lives here. Window selection, live
//! connection state, handshakes and traffic counters remain machine-local.
//! Storing connections as individual Automerge fields (rather than a JSON
//! blob) is what lets two peers concurrently edit the fleet and converge.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

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
    pub fn is_empty(doc: &AutoCommit) -> bool {
        match doc.get(&ROOT, "connections") {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.keys(&id).next().is_none(),
            _ => true,
        }
    }

    /// Write the whole vault into the document (idempotent upsert).
    pub fn write_vault(doc: &mut AutoCommit, vault: &Vault) {
        let _ = doc.put(&ROOT, "tor_global", ScalarValue::Boolean(vault.tor_global));
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
        let _ = doc.put(&obj, "endpoint", c.endpoint.clone());
        let _ = doc.put(&obj, "address", c.address.clone());
        let _ = doc.put(&obj, "dns", c.dns.clone());
        let _ = doc.put(&obj, "public_key", c.public_key.clone());
        let _ = doc.put(&obj, "allowed_ips", c.allowed_ips.clone());
        let _ = doc.put(&obj, "tor", ScalarValue::Boolean(c.tor));
        let _ = doc.put(
            &obj,
            "config_path",
            c.config_path.clone().unwrap_or_default(),
        );
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

    pub fn set_tor_global(doc: &mut AutoCommit, on: bool) {
        let _ = doc.put(&ROOT, "tor_global", ScalarValue::Boolean(on));
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
                let s = |k: &str| get_str(doc, &obj, k);
                connections.push(Connection {
                    id,
                    name: s("name"),
                    kind: ConnKind::parse(&s("kind")),
                    endpoint: s("endpoint"),
                    address: s("address"),
                    dns: s("dns"),
                    public_key: s("public_key"),
                    allowed_ips: s("allowed_ips"),
                    status: ConnStatus::Disconnected,
                    last_handshake_secs: None,
                    last_used: "Never on this device".into(),
                    tor: get_bool(doc, &obj, "tor"),
                    rx_bytes: 0,
                    tx_bytes: 0,
                    base_down_mbps: 0.0,
                    base_up_mbps: 0.0,
                    config_path: {
                        let p = s("config_path");
                        if p.is_empty() { None } else { Some(p) }
                    },
                });
            }
        }
        // Stable order: connected first, then by name.
        connections.sort_by(|a, b| {
            b.is_connected()
                .cmp(&a.is_connected())
                .then_with(|| a.name.cmp(&b.name))
        });
        let tor_global = get_bool(doc, &ROOT, "tor_global");
        Vault {
            connections,
            selected: None,
            tor_global,
        }
    }
}

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> bool {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => matches!(v.as_ref(), ScalarValue::Boolean(true)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_vault;

    #[test]
    fn vault_round_trips_through_crdt() {
        let mut doc = AutoCommit::new();
        let vault = sample_vault();
        VpnDoc::write_vault(&mut doc, &vault);
        let back = VpnDoc::read_vault(&doc);
        assert_eq!(back.connections.len(), vault.connections.len());
        assert_eq!(back.selected, None, "window selection is not synchronized");
        let home = back.connections.iter().find(|c| c.name == "Home").unwrap();
        assert_eq!(home.status, ConnStatus::Disconnected);
        assert_eq!(home.kind, ConnKind::WireGuard);
        assert_eq!(home.endpoint, "home.aurora.net:51820");
        assert_eq!(home.rx_bytes, 0, "traffic counters are not synchronized");
    }

    #[test]
    fn machine_live_state_is_not_persisted() {
        let mut doc = AutoCommit::new();
        let vault = sample_vault();
        VpnDoc::write_vault(&mut doc, &vault);
        let back = VpnDoc::read_vault(&doc);
        assert!(back.connections.iter().all(|connection| {
            connection.status == ConnStatus::Disconnected
                && connection.last_handshake_secs.is_none()
                && connection.rx_bytes == 0
        }));
    }

    #[test]
    fn empty_detection_and_two_doc_merge() {
        let mut a = AutoCommit::new();
        assert!(VpnDoc::is_empty(&a));
        VpnDoc::write_vault(&mut a, &sample_vault());
        assert!(!VpnDoc::is_empty(&a));

        // Peer B joins from A's state, adds one, A pulls it back.
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let extra = Connection::new("Travel", ConnKind::WireGuard);
        VpnDoc::write_connection(&mut b, &extra);
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        let names: Vec<String> = VpnDoc::read_vault(&a)
            .connections
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"Travel".to_string()));
        assert!(names.contains(&"Home".to_string()));
    }
}
