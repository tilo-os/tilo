//! Automerge document model for the saved-connection list.
//!
//! The connection store is the durable, collaborative part of the app,
//! so it lives in the shared CRDT rather than in widget fields. Two
//! devices that share this document converge on one machine list:
//! adding a host on a laptop makes it appear on the desktop, renames
//! merge, deletions propagate.
//!
//! Document shape:
//!
//! ```text
//! root
//! └── "connections" (Map, keyed by ConnectionId string)
//!     └── <id> (Map)
//!         ├── "name"      (Text)   # collaborative display name
//!         ├── "host"      (Str)
//!         ├── "port"      (Int)
//!         ├── "user"      (Str)
//!         ├── "kind"      (Str)    # SSH / VNC / RDP / Tilo
//!         ├── "auth"      (Str)    # auth method label
//!         ├── "last_used" (Str)
//!         ├── "presence"  (Str)    # online / offline / unknown
//!         ├── "group"     (Str)
//!         └── "favorite"  (Bool)
//! ```
//!
//! View state — current screen, selection, the in-progress form — is
//! deliberately *not* in the document: it's per-peer, so collaborators
//! can browse the same list while looking at different machines.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_i64_coerced, read_str};

use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind, Presence};

/// Stateless namespace for the connection-document helpers.
pub struct RemoteDoc;

impl RemoteDoc {
    /// Get-or-create the top-level `connections` Map.
    ///
    /// # Panics
    /// Never in practice: `put_object` on `ROOT` only fails for an invalid or
    /// deleted parent object, and `ROOT` is permanent for the life of the doc.
    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so \
                  put_object on it cannot fail short of a corrupted document \
                  — there is no meaningful ObjId to fall back to"
    )]
    pub fn connections_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "connections") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "connections", ObjType::Map)
                .expect("put_object connections on root never fails"),
        }
    }

    /// Get-or-create the per-connection Map for `id`.
    #[allow(
        clippy::expect_used,
        reason = "conns is a Map this call itself just fetched-or-created via \
                  connections_map, so put_object on it cannot fail short of a \
                  corrupted document — there is no meaningful ObjId to fall back to"
    )]
    fn ensure_conn(doc: &mut AutoCommit, id: ConnectionId) -> ObjId {
        let conns = Self::connections_map(doc);
        let key = id.to_string();
        match doc.get(&conns, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), cid))) => cid,
            _ => doc
                .put_object(&conns, key.as_str(), ObjType::Map)
                .expect("put_object connection map never fails"),
        }
    }

    /// Insert or update a connection. The name is stored as a `Text`
    /// object so concurrent renames merge; the rest are plain scalars
    /// (last-writer-wins is fine for host/port/flags).
    ///
    /// # Panics
    /// Never in practice: `conn` was just fetched-or-created above, so
    /// `put_object` on it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "conn is a Map this call itself just fetched-or-created via \
                  ensure_conn, so put_object on it cannot fail short of a \
                  corrupted document — there is no meaningful ObjId to fall back to"
    )]
    pub fn upsert(doc: &mut AutoCommit, c: &Connection) {
        let conn = Self::ensure_conn(doc, c.id);

        // name (Text — merges on concurrent edit)
        let name_id = match doc.get(&conn, "name") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(&conn, "name", ObjType::Text)
                .expect("put_object name never fails"),
        };
        ui::doc::splice_text_diffed(doc, &name_id, &c.name);

        ui::doc::put_if_changed(doc, &conn, "host", c.host.as_str());
        ui::doc::put_if_changed(doc, &conn, "port", i64::from(c.port));
        ui::doc::put_if_changed(doc, &conn, "user", c.user.as_str());
        ui::doc::put_if_changed(doc, &conn, "kind", c.kind.tag());
        ui::doc::put_if_changed(doc, &conn, "auth", c.auth.label());
        ui::doc::put_if_changed(doc, &conn, "last_used", c.last_used.as_str());
        ui::doc::put_if_changed(doc, &conn, "presence", presence_tag(c.presence));
        ui::doc::put_if_changed(doc, &conn, "group", c.group.as_str());
        ui::doc::put_if_changed(doc, &conn, "favorite", c.favorite);
    }

    /// Remove a connection from the document.
    pub fn remove(doc: &mut AutoCommit, id: ConnectionId) {
        let conns = Self::connections_map(doc);
        let _ = doc.delete(&conns, id.to_string().as_str());
    }

    /// Rename a connection, splicing only the changed run of the collaborative
    /// `name` [`Text`](ObjType::Text) so concurrent renames merge. No-op if the
    /// connection isn't in the document — a rename can't conjure a machine.
    ///
    /// # Panics
    /// Never in practice: `conn` was just looked up above, so `put_object` on
    /// it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "conn is a Map this call itself just looked up above, so \
                  put_object on it cannot fail short of a corrupted document \
                  — there is no meaningful ObjId to fall back to"
    )]
    pub fn rename(doc: &mut AutoCommit, id: ConnectionId, name: &str) {
        let conns = Self::connections_map(doc);
        let Ok(Some((Value::Object(ObjType::Map), conn))) =
            doc.get(&conns, id.to_string().as_str())
        else {
            return;
        };
        let name_id = match doc.get(&conn, "name") {
            Ok(Some((Value::Object(ObjType::Text), tid))) => tid,
            _ => doc
                .put_object(&conn, "name", ObjType::Text)
                .expect("put_object name never fails"),
        };
        ui::doc::splice_text_diffed(doc, &name_id, name);
    }

    /// Set the favorite flag on an existing connection. The absolute target is
    /// stored (never a read-then-flip), so the write is a pure function of its
    /// argument and replays identically. No-op if the connection isn't present.
    pub fn set_favorite(doc: &mut AutoCommit, id: ConnectionId, favorite: bool) {
        let conns = Self::connections_map(doc);
        let Ok(Some((Value::Object(ObjType::Map), conn))) =
            doc.get(&conns, id.to_string().as_str())
        else {
            return;
        };
        ui::doc::put_if_changed(doc, &conn, "favorite", favorite);
    }

    /// Read every connection out of the document.
    pub fn list(doc: &AutoCommit) -> Vec<Connection> {
        let Ok(Some((Value::Object(ObjType::Map), conns))) = doc.get(&ROOT, "connections") else {
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
            let last_used = read_str(doc, &conn, "last_used").unwrap_or_default();
            let kind = ConnectionKind::from_tag(&read_str(doc, &conn, "kind").unwrap_or_default());
            let auth = AuthMethod::from_label(&read_str(doc, &conn, "auth").unwrap_or_default());
            let presence = presence_from_tag(&read_str(doc, &conn, "presence").unwrap_or_default());
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
                last_used,
                presence,
                group,
                favorite,
            });
        }
        out
    }
}

fn presence_tag(p: Presence) -> &'static str {
    match p {
        Presence::Online => "online",
        Presence::Offline => "offline",
        Presence::Unknown => "unknown",
    }
}

fn presence_from_tag(s: &str) -> Presence {
    match s {
        "online" => Presence::Online,
        "offline" => Presence::Offline,
        _ => Presence::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;
    use uuid::Uuid;

    fn conn(name: &str, kind: ConnectionKind) -> Connection {
        Connection {
            id: Uuid::new_v4(),
            name: name.into(),
            host: "10.0.0.5".into(),
            port: kind.default_port(),
            user: "admin".into(),
            kind,
            auth: AuthMethod::PublicKey,
            last_used: "2h ago".into(),
            presence: Presence::Online,
            group: "lab".into(),
            favorite: true,
        }
    }

    #[test]
    fn connection_doc_round_trips_tags_and_merges() {
        eprintln!("phase round_trip_preserves_all_fields");
        {
            let mut doc = AutoCommit::new();
            let c = conn("Build Box", ConnectionKind::Ssh);
            RemoteDoc::upsert(&mut doc, &c);
            let back = RemoteDoc::list(&doc);
            assert_eq!(back.len(), 1);
            assert_eq!(back[0], c);
        }

        eprintln!("phase kind_and_auth_tags_round_trip");
        {
            for kind in ConnectionKind::all() {
                let mut doc = AutoCommit::new();
                RemoteDoc::upsert(&mut doc, &conn("c", kind));
                assert_eq!(RemoteDoc::list(&doc)[0].kind, kind);
            }
        }

        eprintln!("phase two_docs_merge_connections");
        {
            let mut a = AutoCommit::new();
            RemoteDoc::upsert(&mut a, &conn("A", ConnectionKind::Ssh));

            // B joins from A's state, then adds its own.
            let mut b = AutoCommit::load(&a.save()).unwrap();
            RemoteDoc::upsert(&mut b, &conn("B", ConnectionKind::Tilo));

            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();

            let names: Vec<_> = RemoteDoc::list(&a).into_iter().map(|c| c.name).collect();
            assert!(names.contains(&"A".to_string()));
            assert!(names.contains(&"B".to_string()));
        }
    }

    #[test]
    fn upsert_and_remove_update_in_place_with_bounded_growth() {
        eprintln!("phase upsert_updates_in_place");
        {
            let mut doc = AutoCommit::new();
            let mut c = conn("Old", ConnectionKind::Vnc);
            RemoteDoc::upsert(&mut doc, &c);
            c.name = "New".into();
            c.presence = Presence::Offline;
            RemoteDoc::upsert(&mut doc, &c);
            let back = RemoteDoc::list(&doc);
            assert_eq!(back.len(), 1, "update must not create a duplicate");
            assert_eq!(back[0].name, "New");
            assert_eq!(back[0].presence, Presence::Offline);
        }

        eprintln!("phase appending_one_char_to_name_is_bounded");
        {
            // The name is a collaborative Text; appending one char must splice only
            // the new char. The other (scalar) fields are unchanged, so the guarded
            // re-puts emit nothing — the whole upsert grows the doc by ~one char.
            let mut doc = AutoCommit::new();
            let mut c = conn("x", ConnectionKind::Ssh);
            c.name = "n".repeat(300);
            RemoteDoc::upsert(&mut doc, &c);
            let before = doc.save().len();
            c.name.push('!');
            RemoteDoc::upsert(&mut doc, &c);
            let grew = doc.save().len() - before;
            assert_eq!(RemoteDoc::list(&doc)[0].name, c.name, "value round-trips");
            assert!(
                grew < 40,
                "appending one char to the name grew doc by {grew} bytes"
            );
        }

        eprintln!("phase repeated_upsert_keeps_doc_bounded");
        {
            // Re-upserting an unchanged connection must be a no-op: the name diff is
            // empty and every scalar re-put is guarded.
            let mut doc = AutoCommit::new();
            let c = conn("Build Box", ConnectionKind::Ssh);
            RemoteDoc::upsert(&mut doc, &c);
            let a = doc.save().len();
            for _ in 0..50 {
                RemoteDoc::upsert(&mut doc, &c);
            }
            let b = doc.save().len();
            assert_eq!(
                a, b,
                "re-upserting an unchanged connection grew the document {a} -> {b}"
            );
        }

        eprintln!("phase remove_deletes_connection");
        {
            let mut doc = AutoCommit::new();
            let c = conn("X", ConnectionKind::Rdp);
            let id = c.id;
            RemoteDoc::upsert(&mut doc, &c);
            RemoteDoc::remove(&mut doc, id);
            assert!(RemoteDoc::list(&doc).is_empty());
        }
    }

    #[test]
    fn rename_and_favorite_touch_only_known_connections() {
        eprintln!("phase rename_updates_only_the_name");
        {
            let mut doc = AutoCommit::new();
            let c = conn("Before", ConnectionKind::Ssh);
            RemoteDoc::upsert(&mut doc, &c);
            RemoteDoc::rename(&mut doc, c.id, "After");
            let back = RemoteDoc::list(&doc);
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].name, "After");
            assert_eq!(
                back[0].host, c.host,
                "rename leaves the rest of the record intact"
            );
            assert_eq!(back[0].favorite, c.favorite);
        }

        eprintln!("phase rename_of_unknown_connection_is_a_noop");
        {
            let mut doc = AutoCommit::new();
            RemoteDoc::rename(&mut doc, Uuid::new_v4(), "Ghost");
            assert!(
                RemoteDoc::list(&doc).is_empty(),
                "rename must not conjure a machine"
            );
        }

        eprintln!("phase set_favorite_stores_absolute_value");
        {
            let mut doc = AutoCommit::new();
            let mut c = conn("Box", ConnectionKind::Ssh);
            c.favorite = false;
            RemoteDoc::upsert(&mut doc, &c);
            RemoteDoc::set_favorite(&mut doc, c.id, true);
            assert!(RemoteDoc::list(&doc)[0].favorite);
            RemoteDoc::set_favorite(&mut doc, c.id, false);
            assert!(!RemoteDoc::list(&doc)[0].favorite);
        }

        eprintln!("phase set_favorite_of_unknown_connection_is_a_noop");
        {
            let mut doc = AutoCommit::new();
            RemoteDoc::set_favorite(&mut doc, Uuid::new_v4(), true);
            assert!(RemoteDoc::list(&doc).is_empty());
        }
    }
}
