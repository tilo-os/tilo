//! Automerge document model for the saved-connection list.
//!
//! The connection store is the durable, collaborative part of the app,
//! so it lives in the shared CRDT rather than in widget fields. Two
//! devices that share this document converge on one machine list:
//! adding a host on a laptop makes it appear on the desktop, renames
//! merge, deletions propagate. Only [`RemoteAction`](crate::actions::RemoteAction)
//! writes it; this module reads it.
//!
//! Document shape:
//!
//! ```text
//! root
//! └── "connections" (Map, keyed by ConnectionId string)
//!     └── <id> (Map)
//!         ├── "name"         (Text)   # collaborative display name
//!         ├── "host"         (Str)
//!         ├── "port"         (Int)
//!         ├── "user"         (Str)
//!         ├── "kind"         (Str)    # SSH / VNC / RDP / Tilo
//!         ├── "auth"         (Str)    # auth method label
//!         ├── "last_used_ms" (Int)    # when a session was last opened
//!         ├── "group"        (Str)
//!         └── "favorite"     (Bool)
//! ```
//!
//! View state — current screen, selection, the in-progress form — and what
//! this device observes live (whether a machine is reachable) are
//! deliberately *not* in the document, and neither are credentials: they
//! stay in this device's secret store, keyed by the connection's id.

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_i64_coerced, read_str};

use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind};

/// The document key of the saved-connection map.
pub const CONNECTIONS: &str = "connections";

/// Stateless namespace for reading the connection document.
pub struct RemoteDoc;

impl RemoteDoc {
    /// Read every connection out of the document.
    pub fn list(doc: &AutoCommit) -> Vec<Connection> {
        let Ok(Some((Value::Object(ObjType::Map), conns))) = doc.get(&ROOT, CONNECTIONS) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for key in doc.keys(&conns) {
            let Ok(id) = key.parse::<ConnectionId>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), conn))) = doc.get(&conns, key.as_str())
            else {
                continue;
            };

            let name = match doc.get(&conn, "name") {
                Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).unwrap_or_default(),
                _ => String::new(),
            };

            let host = read_str(doc, &conn, "host").unwrap_or_default();
            let user = read_str(doc, &conn, "user").unwrap_or_default();
            let group = read_str(doc, &conn, "group").unwrap_or_default();
            let last_used_ms = read_i64_coerced(doc, &conn, "last_used_ms");
            let kind = ConnectionKind::from_tag(&read_str(doc, &conn, "kind").unwrap_or_default());
            let auth = AuthMethod::from_label(&read_str(doc, &conn, "auth").unwrap_or_default());
            let port = read_i64_coerced(doc, &conn, "port").unwrap_or(0) as u16;
            let favorite = read_bool(doc, &conn, "favorite").unwrap_or(false);

            out.push(Connection {
                id,
                name,
                host,
                port,
                user,
                kind,
                auth,
                last_used_ms,
                group,
                favorite,
            });
        }
        out
    }
}
