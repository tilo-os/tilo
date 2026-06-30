//! Automerge projection of the [`PermissionSet`].
//!
//! Roles, members, and contexts are user-authored and must sync across
//! peers, so they live in the CRDT. The registry of permission
//! *definitions* does **not** live here — definitions are contributed by
//! code at startup (see [`crate::registry`]); only the references to
//! their ids are persisted.
//!
//! Layout under the document root:
//!
//! ```text
//! root
//! └── "permissions" (Map)
//!     ├── "roles" (Map keyed by RoleId)
//!     │   └── <id> (Map { name:Str, color:Int, position:Int,
//!     │                   grants: List<Map{permission:Str, grant:Str,
//!     │                                     context:Str?}> })
//!     ├── "members" (Map keyed by MemberId)
//!     │   └── <id> (Map { name:Str,
//!     │                   roles: List<Str(RoleId)>,
//!     │                   overrides: List<Map{permission,grant,context?}> })
//!     └── "contexts" (List<Map{id:Str, app:Str, kind:Str, label:Str}>)
//! ```
//!
//! Grants and overrides share the same `{permission, grant, context?}`
//! tuple shape, so one pair of read/write helpers serves both.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{
    Context, ContextId, Grant, Member, MemberId, Override, PermissionSet, Role, RoleGrant, RoleId,
};

/// Stateless namespace for the permissions document helpers.
pub struct PermissionsDoc;

impl PermissionsDoc {
    /// Get-or-create the top-level `permissions` Map.
    fn root_map(doc: &mut AutoCommit) -> ObjId {
        map_child(doc, &ROOT, "permissions")
    }

    /// Write the whole [`PermissionSet`] into the document, replacing the
    /// stored roles/members/contexts. Used after structural edits (create
    /// role, delete member, …) where a full rewrite is simplest and the
    /// set is small. Field-level edits (renaming a role, flipping one
    /// grant) can also go through here — Automerge diffs the change.
    pub fn write(doc: &mut AutoCommit, set: &PermissionSet) {
        let root = Self::root_map(doc);

        // ----- roles -----
        let roles = map_child(doc, &root, "roles");
        let keep: Vec<String> = set.roles.iter().map(|r| r.id.to_string()).collect();
        prune_map(doc, &roles, &keep);
        for r in &set.roles {
            let m = map_child(doc, &roles, &r.id.to_string());
            put_str(doc, &m, "name", &r.name);
            let _ = doc.put(&m, "color", r.color as i64);
            let _ = doc.put(&m, "position", r.position);
            write_grant_tuples(doc, &m, "grants", grants_as_tuples(&r.grants));
        }

        // ----- members -----
        let members = map_child(doc, &root, "members");
        let keepm: Vec<String> = set.members.iter().map(|m| m.id.to_string()).collect();
        prune_map(doc, &members, &keepm);
        for mem in &set.members {
            let m = map_child(doc, &members, &mem.id.to_string());
            put_str(doc, &m, "name", &mem.display_name);
            write_str_list(doc, &m, "roles", &mem.role_ids.iter().map(|r| r.to_string()).collect::<Vec<_>>());
            write_grant_tuples(doc, &m, "overrides", overrides_as_tuples(&mem.overrides));
        }

        // ----- contexts -----
        let list = list_child(doc, &root, "contexts");
        clear_list(doc, &list);
        for (i, c) in set.contexts.iter().enumerate() {
            if let Ok(cm) = doc.insert_object(&list, i, ObjType::Map) {
                let _ = doc.put(&cm, "id", c.id.0.clone());
                let _ = doc.put(&cm, "app", c.app.clone());
                let _ = doc.put(&cm, "kind", c.kind.clone());
                let _ = doc.put(&cm, "label", c.label.clone());
            }
        }
    }

    /// Read the whole [`PermissionSet`] back out of the document.
    /// Returns `PermissionSet::default()` when nothing has been written.
    pub fn read(doc: &AutoCommit) -> PermissionSet {
        let root = match doc.get(&ROOT, "permissions") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return PermissionSet::default(),
        };

        let mut set = PermissionSet::default();

        // roles
        if let Ok(Some((Value::Object(ObjType::Map), roles))) = doc.get(&root, "roles") {
            for key in doc.keys(&roles) {
                let Ok(id) = key.parse::<RoleId>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&roles, key.as_str()) else {
                    continue;
                };
                set.roles.push(Role {
                    id,
                    name: read_str(doc, &m, "name"),
                    color: read_int(doc, &m, "color") as u32,
                    position: read_int(doc, &m, "position"),
                    grants: read_grant_tuples(doc, &m, "grants")
                        .into_iter()
                        .map(|(permission, grant, context)| RoleGrant { permission, grant, context })
                        .collect(),
                });
            }
        }

        // members
        if let Ok(Some((Value::Object(ObjType::Map), members))) = doc.get(&root, "members") {
            for key in doc.keys(&members) {
                let Ok(id) = key.parse::<MemberId>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&members, key.as_str()) else {
                    continue;
                };
                set.members.push(Member {
                    id,
                    display_name: read_str(doc, &m, "name"),
                    role_ids: read_str_list(doc, &m, "roles")
                        .into_iter()
                        .filter_map(|s| s.parse::<RoleId>().ok())
                        .collect(),
                    overrides: read_grant_tuples(doc, &m, "overrides")
                        .into_iter()
                        .map(|(permission, grant, context)| Override { permission, grant, context })
                        .collect(),
                });
            }
        }

        // contexts
        if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&root, "contexts") {
            for i in 0..doc.length(&list) {
                if let Ok(Some((Value::Object(ObjType::Map), cm))) = doc.get(&list, i) {
                    set.contexts.push(Context {
                        id: ContextId::new(read_str(doc, &cm, "id")),
                        app: read_str(doc, &cm, "app"),
                        kind: read_str(doc, &cm, "kind"),
                        label: read_str(doc, &cm, "label"),
                    });
                }
            }
        }

        // Keep roles in hierarchy order for stable reads.
        set.roles.sort_by(|a, b| a.position.cmp(&b.position));
        set.members.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        set
    }
}

// ---------------------------------------------------------------------
// Grant/override tuple shape (shared by roles' grants + members' overrides)
// ---------------------------------------------------------------------

type Tuple = (String, Grant, Option<ContextId>);

fn grants_as_tuples(grants: &[RoleGrant]) -> Vec<Tuple> {
    grants
        .iter()
        .map(|g| (g.permission.clone(), g.grant, g.context.clone()))
        .collect()
}

fn overrides_as_tuples(overrides: &[Override]) -> Vec<Tuple> {
    overrides
        .iter()
        .map(|o| (o.permission.clone(), o.grant, o.context.clone()))
        .collect()
}

fn grant_str(g: Grant) -> &'static str {
    match g {
        Grant::Allow => "allow",
        Grant::Deny => "deny",
        Grant::Inherit => "inherit",
    }
}

fn parse_grant(s: &str) -> Grant {
    match s {
        "allow" => Grant::Allow,
        "deny" => Grant::Deny,
        _ => Grant::Inherit,
    }
}

fn write_grant_tuples(doc: &mut AutoCommit, parent: &ObjId, key: &str, tuples: Vec<Tuple>) {
    let list = list_child(doc, parent, key);
    clear_list(doc, &list);
    for (i, (perm, grant, ctx)) in tuples.into_iter().enumerate() {
        if let Ok(m) = doc.insert_object(&list, i, ObjType::Map) {
            let _ = doc.put(&m, "permission", perm);
            let _ = doc.put(&m, "grant", grant_str(grant));
            if let Some(c) = ctx {
                let _ = doc.put(&m, "context", c.0);
            }
        }
    }
}

fn read_grant_tuples(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<Tuple> {
    let list = match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for i in 0..doc.length(&list) {
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) {
            let perm = read_str(doc, &m, "permission");
            if perm.is_empty() {
                continue;
            }
            let grant = parse_grant(&read_str(doc, &m, "grant"));
            let ctx = {
                let s = read_str(doc, &m, "context");
                if s.is_empty() { None } else { Some(ContextId::new(s)) }
            };
            out.push((perm, grant, ctx));
        }
    }
    out
}

// ---------------------------------------------------------------------
// Low-level Automerge helpers
// ---------------------------------------------------------------------

fn map_child(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => id,
        _ => doc
            .put_object(parent, key, ObjType::Map)
            .expect("put_object map never fails"),
    }
}

fn list_child(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => doc
            .put_object(parent, key, ObjType::List)
            .expect("put_object list never fails"),
    }
}

/// Remove every key from `map` that isn't in `keep`.
fn prune_map(doc: &mut AutoCommit, map: &ObjId, keep: &[String]) {
    let stale: Vec<String> = doc
        .keys(map)
        .filter(|k| !keep.iter().any(|x| x == k))
        .collect();
    for k in stale {
        let _ = doc.delete(map, k.as_str());
    }
}

fn clear_list(doc: &mut AutoCommit, list: &ObjId) {
    let n = doc.length(list);
    for _ in 0..n {
        let _ = doc.delete(list, 0);
    }
}

fn put_str(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &str) {
    let _ = doc.put(parent, key, value.to_string());
}

fn write_str_list(doc: &mut AutoCommit, parent: &ObjId, key: &str, values: &[String]) {
    let list = list_child(doc, parent, key);
    clear_list(doc, &list);
    for (i, v) in values.iter().enumerate() {
        let _ = doc.insert(&list, i, v.clone());
    }
}

fn read_str(doc: &AutoCommit, parent: &ObjId, key: &str) -> String {
    match doc.get(parent, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn read_int(doc: &AutoCommit, parent: &ObjId, key: &str) -> i64 {
    match doc.get(parent, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Int(i) => *i,
            ScalarValue::Uint(u) => *u as i64,
            _ => 0,
        },
        _ => 0,
    }
}

fn read_str_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<String> {
    let list = match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for i in 0..doc.length(&list) {
        if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i) {
            if let ScalarValue::Str(s) = v.as_ref() {
                out.push(s.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Grant, Member, Role};
    use crate::sample;

    #[test]
    fn write_then_read_round_trips() {
        let set = sample::sample_set();
        let mut doc = AutoCommit::new();
        PermissionsDoc::write(&mut doc, &set);
        let back = PermissionsDoc::read(&doc);
        assert_eq!(back.roles.len(), set.roles.len());
        assert_eq!(back.members.len(), set.members.len());
        assert_eq!(back.contexts.len(), set.contexts.len());
        // Spot-check a grant survives with its context.
        let admin = back
            .roles
            .iter()
            .find(|r| r.name == "Admin")
            .expect("Admin role round-trips");
        let original = set.roles.iter().find(|r| r.name == "Admin").unwrap();
        assert_eq!(admin.grants, original.grants);
    }

    #[test]
    fn context_scoped_grant_survives_round_trip() {
        let mut set = PermissionSet::default();
        let mut role = Role::new("Mod", 0x4444ff, 1);
        let ctx = ContextId::new("chat:server:eng");
        role.set_grant("chat.manage_channels", Some(ctx.clone()), Grant::Deny);
        let rid = role.id;
        set.roles.push(role);
        set.contexts.push(Context::new("chat:server:eng", "chat", "server", "Engineering"));

        let mut doc = AutoCommit::new();
        PermissionsDoc::write(&mut doc, &set);
        let back = PermissionsDoc::read(&doc);
        let r = back.role(rid).unwrap();
        assert_eq!(r.grant_for("chat.manage_channels", Some(&ctx)), Grant::Deny);
        assert_eq!(
            r.grant_for("chat.manage_channels", None),
            Grant::Inherit,
            "the grant must stay scoped to the eng context"
        );
    }

    #[test]
    fn deleting_a_role_prunes_it_from_the_doc() {
        let mut set = PermissionSet::default();
        let r1 = Role::new("Keep", 0, 1);
        let r2 = Role::new("Drop", 0, 2);
        let drop_id = r2.id;
        set.roles.push(r1);
        set.roles.push(r2);
        let mut doc = AutoCommit::new();
        PermissionsDoc::write(&mut doc, &set);
        // Now remove one and re-write.
        set.remove_role(drop_id);
        PermissionsDoc::write(&mut doc, &set);
        let back = PermissionsDoc::read(&doc);
        assert_eq!(back.roles.len(), 1);
        assert_eq!(back.roles[0].name, "Keep");
    }

    #[test]
    fn member_role_assignment_round_trips() {
        let mut set = PermissionSet::default();
        let role = Role::new("Mod", 0, 1);
        let rid = role.id;
        set.roles.push(role);
        let mut mem = Member::new("Ada");
        mem.role_ids.push(rid);
        mem.set_override("workspace.manage", None, Grant::Deny);
        let mid = mem.id;
        set.members.push(mem);

        let mut doc = AutoCommit::new();
        PermissionsDoc::write(&mut doc, &set);
        let back = PermissionsDoc::read(&doc);
        let m = back.member(mid).unwrap();
        assert!(m.has_role(rid));
        assert_eq!(m.override_for("workspace.manage", None), Grant::Deny);
    }

    #[test]
    fn two_peers_merge_role_edits() {
        let set = sample::sample_set();
        let mut a = AutoCommit::new();
        PermissionsDoc::write(&mut a, &set);

        // Peer B loads A, adds a role, writes back.
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let mut bset = PermissionsDoc::read(&b);
        bset.roles.push(Role::new("Guest", 0x999999, bset.next_position()));
        PermissionsDoc::write(&mut b, &bset);

        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        let merged = PermissionsDoc::read(&a);
        assert!(merged.roles.iter().any(|r| r.name == "Guest"));
    }
}
