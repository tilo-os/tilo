//! The capability list as written before [`super::FsAuthority`] existed: a
//! bare JSON array of capabilities phrased in the old `verb` vocabulary
//! (`Read`/`Comment`/`Write`/`Invite`/`Admin`), with role subjects and
//! op-level scopes that no longer exist.
//!
//! That file is durable. tmpfiles seeds `/var/lib/tilo/fs-capabilities.json`
//! once and never overwrites it, so a machine installed before the schema
//! changed keeps the old file across every image update, and an
//! administrator's edits to it must survive too. Reading it is therefore
//! part of the format, not a one-off repair.
//!
//! The conversion keeps exactly what the old file meant to its readers (the
//! filesystem and the relays) and nothing more:
//!
//! - `Workspace` covered every resource, filesystem paths included, so it
//!   becomes `FsPath("/")`. `FsPath`, `Device`, `NetHost` and `FsRecord`
//!   carry over unchanged.
//! - `App`, `ResourceType`, `ResourceInstance`, `Action`, `ActionOnKind` and
//!   `Room` scopes never matched a path, device, host or record, and a
//!   `Role` subject never matched a filesystem actor (it holds no role).
//!   They could not grant or restrict anything here and are dropped.
//! - A verb maps to the access it implied on a path: `Write` and `Admin`
//!   reached writing, `Read`, `Comment` and `Invite` only reading. That
//!   holds for `Restrict` too: `Restrict { Comment }` took writing away,
//!   which is `Restrict { Read }`.
//!
//! So a migrated list grants neither more nor less than the old one did.

use serde::Deserialize;

use super::{Access, CapOverride, Effect, Expiry, Principal, Scope, Subject};
use crate::crypto::Ed25519PublicKey;

/// One capability in the old file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CapOverrideV1 {
    subject: SubjectV1,
    scope: ScopeV1,
    effect: Effect,
    verb: VerbV1,
    id: String,
    expiry: Expiry,
    granted_by: Option<Principal>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum SubjectV1 {
    Role(serde::de::IgnoredAny),
    User(Ed25519PublicKey),
    Principal(Principal),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum ScopeV1 {
    Workspace,
    App(serde::de::IgnoredAny),
    ResourceType(serde::de::IgnoredAny),
    ResourceInstance(serde::de::IgnoredAny),
    FsPath(String),
    Device(String),
    NetHost(String),
    Action(serde::de::IgnoredAny),
    ActionOnKind(serde::de::IgnoredAny),
    Room(serde::de::IgnoredAny),
    FsRecord(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum VerbV1 {
    Read,
    Comment,
    Write,
    Invite,
    Admin,
}

impl CapOverrideV1 {
    /// The capability this one is today, or `None` when it could not reach
    /// anything the readers of this file enforce.
    fn migrate(self) -> Option<CapOverride> {
        let subject = match self.subject {
            SubjectV1::Role(_) => return None,
            SubjectV1::User(key) => Subject::User(key),
            SubjectV1::Principal(principal) => Subject::Principal(principal),
        };
        let scope = match self.scope {
            ScopeV1::Workspace => Scope::FsPath("/".into()),
            ScopeV1::FsPath(path) => Scope::FsPath(path),
            ScopeV1::Device(device) => Scope::Device(device),
            ScopeV1::NetHost(host) => Scope::NetHost(host),
            ScopeV1::FsRecord(record) => Scope::FsRecord(record),
            ScopeV1::App(_)
            | ScopeV1::ResourceType(_)
            | ScopeV1::ResourceInstance(_)
            | ScopeV1::Action(_)
            | ScopeV1::ActionOnKind(_)
            | ScopeV1::Room(_) => return None,
        };
        let access = match self.verb {
            VerbV1::Write | VerbV1::Admin => Access::Write,
            VerbV1::Read | VerbV1::Comment | VerbV1::Invite => Access::Read,
        };
        Some(CapOverride {
            subject,
            scope,
            effect: self.effect,
            access,
            id: self.id,
            expiry: self.expiry,
            granted_by: self.granted_by,
        })
    }
}

/// Today's capabilities for an old list, in order.
pub(super) fn migrate(old: Vec<CapOverrideV1>) -> Vec<CapOverride> {
    old.into_iter().filter_map(CapOverrideV1::migrate).collect()
}

#[cfg(test)]
mod tests {
    use super::super::FsAuthority;
    use super::*;

    /// `/var/lib/tilo/fs-capabilities.json` as tmpfiles seeded it on a machine
    /// installed on 2026-10-02, byte for byte. After the image update that
    /// introduced [`FsAuthority`], fs.service refused it on every start and
    /// `/var/workspace` never mounted.
    const SEEDED_2026_10_02: &str = r#"[
  {
    "subject": { "Principal": { "Person": "local" } },
    "scope": { "FsPath": "/" },
    "effect": "Grant",
    "verb": "Write",
    "id": "local-session-workspace",
    "expiry": "Never",
    "granted_by": null
  }
]
"#;

    #[test]
    fn the_list_seeded_before_the_file_authority_reads_as_the_same_grant() {
        let authority: FsAuthority = serde_json::from_str(SEEDED_2026_10_02).unwrap();
        let shipped = CapOverride::grant(
            Subject::person("local"),
            Scope::FsPath("/".into()),
            Access::Write,
        )
        .with_id("local-session-workspace");
        assert_eq!(authority, FsAuthority::from(vec![shipped]));
    }

    #[test]
    fn every_old_verb_and_scope_keeps_what_it_reached_on_a_path() {
        let old = r#"[
          {"subject":{"Principal":{"Person":"p"}},"scope":"Workspace","effect":"Grant","verb":"Admin","id":"a","expiry":"Never","granted_by":null},
          {"subject":{"Principal":{"Person":"p"}},"scope":{"FsPath":"/d"},"effect":"Restrict","verb":"Comment","id":"b","expiry":{"AtUnix":5},"granted_by":{"App":"x"}},
          {"subject":{"Principal":{"Person":"p"}},"scope":{"FsPath":"/i"},"effect":"Grant","verb":"Invite","id":"c","expiry":"Never","granted_by":null},
          {"subject":{"Role":"Editor"},"scope":{"FsPath":"/"},"effect":"Grant","verb":"Write","id":"d","expiry":"Never","granted_by":null},
          {"subject":{"Principal":{"Person":"p"}},"scope":{"App":"notes"},"effect":"Grant","verb":"Write","id":"e","expiry":"Never","granted_by":null},
          {"subject":{"Principal":{"Person":"p"}},"scope":{"Action":{"app":"a","resource":"r","action":"x"}},"effect":"Grant","verb":"Write","id":"f","expiry":"Never","granted_by":null},
          {"subject":{"Principal":{"Person":"p"}},"scope":{"Room":"r"},"effect":"Grant","verb":"Read","id":"g","expiry":"Never","granted_by":null}
        ]"#;
        let FsAuthority {
            capabilities,
            room_members,
        } = serde_json::from_str(old).unwrap();
        assert!(room_members.is_empty());
        let got: Vec<_> = capabilities
            .iter()
            .map(|c| (c.id.as_str(), c.scope.clone(), c.effect, c.access))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", Scope::FsPath("/".into()), Effect::Grant, Access::Write),
                (
                    "b",
                    Scope::FsPath("/d".into()),
                    Effect::Restrict,
                    Access::Read
                ),
                ("c", Scope::FsPath("/i".into()), Effect::Grant, Access::Read),
            ],
            "a role subject and op-level scopes never reached a path and must not start to"
        );
        assert_eq!(capabilities[1].expiry, Expiry::AtUnix(5));
        assert_eq!(capabilities[1].granted_by, Some(Principal::App("x".into())));
    }

    #[test]
    fn a_wrong_file_is_reported_in_the_shape_it_is_in() {
        let err = serde_json::from_str::<FsAuthority>("7")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("file authority object") && err.contains("array written before it"),
            "{err}"
        );
        let err = serde_json::from_str::<FsAuthority>(r#"{"capabilities":[{"verb":"Write"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("verb"),
            "an object names its own bad field: {err}"
        );
        assert!(!err.contains("expected a sequence"), "{err}");
    }
}
