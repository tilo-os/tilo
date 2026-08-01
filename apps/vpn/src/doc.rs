//! Automerge document model for the VPN vault.
//!
//! Shape:
//!
//! ```text
//! root
//! ├── "tor_global" (Bool)
//! ├── "selected"   (Str)   # ConnId string, "" for none
//! └── "connections" (Map, keyed by ConnId string)
//!     └── <id> (Map)
//!         ├── "name" / "kind" / "endpoint" / "address" / "dns"
//!         │     "public_key" / "allowed_ips" / "status" / "last_used"   (Str)
//!         ├── "tor" (Bool)
//!         ├── "last_handshake" (Int, -1 == none)
//!         ├── "rx" / "tx" (Int)
//!         └── "base_down" / "base_up" (F64)
//! ```
//!
//! State lives here — the widget tree only caches a read-back [`Vault`].
//! Storing connections as individual Automerge fields (rather than a JSON
//! blob) is what lets two peers concurrently edit the fleet and converge.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ROOT, ScalarValue, Value};

use crate::model::{ConnId, ConnKind, ConnStatus, Connection, Vault};

pub struct VpnDoc;

impl VpnDoc {
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
        let sel = vault.selected.map(|s| s.to_string()).unwrap_or_default();
        let _ = doc.put(&ROOT, "selected", sel);
        for c in &vault.connections {
            Self::write_connection(doc, c);
        }
    }

    /// Upsert one connection's fields.
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
        let _ = doc.put(&obj, "status", c.status.as_str());
        let _ = doc.put(&obj, "last_used", c.last_used.clone());
        let _ = doc.put(&obj, "tor", ScalarValue::Boolean(c.tor));
        let _ = doc.put(
            &obj,
            "last_handshake",
            c.last_handshake_secs.map(|s| s as i64).unwrap_or(-1),
        );
        let _ = doc.put(&obj, "rx", c.rx_bytes as i64);
        let _ = doc.put(&obj, "tx", c.tx_bytes as i64);
        let _ = doc.put(&obj, "base_down", c.base_down_mbps);
        let _ = doc.put(&obj, "base_up", c.base_up_mbps);
        let _ = doc.put(&obj, "config_path", c.config_path.clone().unwrap_or_default());
    }

    /// Update just the live status of a connection (connect/disconnect).
    pub fn set_status(doc: &mut AutoCommit, id: ConnId, status: ConnStatus) {
        let conns = Self::connections_map(doc);
        if let Ok(Some((Value::Object(ObjType::Map), obj))) =
            doc.get(&conns, id.to_string().as_str())
        {
            let _ = doc.put(&obj, "status", status.as_str());
            if status == ConnStatus::Connected {
                let _ = doc.put(&obj, "last_handshake", 0i64);
                let _ = doc.put(&obj, "last_used", "Active now".to_string());
            }
        }
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

    pub fn set_selected(doc: &mut AutoCommit, id: Option<ConnId>) {
        let _ = doc.put(&ROOT, "selected", id.map(|s| s.to_string()).unwrap_or_default());
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
                let Ok(id) = key.parse::<ConnId>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), obj))) =
                    doc.get(&conns, key.as_str())
                else {
                    continue;
                };
                let s = |k: &str| get_str(doc, &obj, k);
                let last_hs = get_int(doc, &obj, "last_handshake");
                connections.push(Connection {
                    id,
                    name: s("name"),
                    kind: ConnKind::from_str(&s("kind")),
                    endpoint: s("endpoint"),
                    address: s("address"),
                    dns: s("dns"),
                    public_key: s("public_key"),
                    allowed_ips: s("allowed_ips"),
                    status: ConnStatus::from_str(&s("status")),
                    last_handshake_secs: if last_hs < 0 { None } else { Some(last_hs as u64) },
                    last_used: s("last_used"),
                    tor: get_bool(doc, &obj, "tor"),
                    rx_bytes: get_int(doc, &obj, "rx").max(0) as u64,
                    tx_bytes: get_int(doc, &obj, "tx").max(0) as u64,
                    base_down_mbps: get_f64(doc, &obj, "base_down"),
                    base_up_mbps: get_f64(doc, &obj, "base_up"),
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
        let selected = match doc.get(&ROOT, "selected") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) if !s.is_empty() => s.parse::<ConnId>().ok(),
                _ => None,
            },
            _ => None,
        };
        let tor_global = get_bool(doc, &ROOT, "tor_global");
        Vault { connections, selected, tor_global }
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

fn get_int(doc: &AutoCommit, obj: &ObjId, key: &str) -> i64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64().unwrap_or(0),
        _ => 0,
    }
}

fn get_f64(doc: &AutoCommit, obj: &ObjId, key: &str) -> f64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::F64(f) => *f,
            ScalarValue::Int(i) => *i as f64,
            ScalarValue::Uint(u) => *u as f64,
            _ => 0.0,
        },
        _ => 0.0,
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
        assert_eq!(back.selected, vault.selected);
        let home = back.connections.iter().find(|c| c.name == "Home").unwrap();
        assert_eq!(home.status, ConnStatus::Connected);
        assert_eq!(home.kind, ConnKind::WireGuard);
        assert_eq!(home.endpoint, "home.aurora.net:51820");
        assert!((home.base_down_mbps - 6.4).abs() < 1e-9);
    }

    #[test]
    fn set_status_flips_connection() {
        let mut doc = AutoCommit::new();
        let vault = sample_vault();
        VpnDoc::write_vault(&mut doc, &vault);
        let work = vault.connections.iter().find(|c| c.name == "Work").unwrap();
        VpnDoc::set_status(&mut doc, work.id, ConnStatus::Connected);
        let back = VpnDoc::read_vault(&doc);
        let work2 = back.connections.iter().find(|c| c.name == "Work").unwrap();
        assert_eq!(work2.status, ConnStatus::Connected);
        assert_eq!(work2.last_handshake_secs, Some(0));
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
        let names: Vec<String> =
            VpnDoc::read_vault(&a).connections.into_iter().map(|c| c.name).collect();
        assert!(names.contains(&"Travel".to_string()));
        assert!(names.contains(&"Home".to_string()));
    }
}
