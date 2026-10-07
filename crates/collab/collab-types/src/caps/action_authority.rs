//! A host's read-only, app-scoped view of accepted person action policy.
//! This is not a sandbox grant or record participation mode. Its provenance
//! is the launcher-owned read-only namespace, not a caller-supplied value.
use super::{Actor, CapOverride, Scope, Subject};
use serde::{Deserialize, Serialize};

pub const PROJECTION_PATH: &str = "/run/tilo/action-authority/current.json";
pub const MAX_PROJECTION_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppActionAuthorityProjection {
    pub schema_version: u16,
    pub room: String,
    pub app_id: String,
    pub person: String,
    pub owner: Option<String>,
    pub capabilities: Vec<CapOverride>,
    pub issued_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
}

impl AppActionAuthorityProjection {
    /// Refuse foreign principals/scopes rather than importing authority over
    /// another app, filesystem paths, devices, network or records.
    ///
    /// # Errors
    /// Refuses malformed, foreign-scoped or excessively long-lived policy.
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid_name = |value: &str| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        };
        if self.schema_version != 1
            || !valid_name(&self.room)
            || !valid_name(&self.app_id)
            || !valid_name(&self.person)
            || self
                .owner
                .as_deref()
                .is_some_and(|owner| !valid_name(owner))
            || self.capabilities.len() > 256
            || self.issued_at_unix_ms == 0
            || self.valid_until_unix_ms <= self.issued_at_unix_ms
            || self
                .valid_until_unix_ms
                .saturating_sub(self.issued_at_unix_ms)
                > 3_000
        {
            return Err("invalid host action policy projection");
        }
        let person = Actor::person(None, self.person.clone()).in_room(self.room.clone());
        for capability in &self.capabilities {
            let subject_matches = matches!(capability.subject, Subject::Role(_))
                || capability.subject.matches_actor(&person);
            let scope_matches = match &capability.scope {
                Scope::Workspace => true,
                Scope::Room(room) => room == &self.room,
                Scope::App(app) | Scope::Action { app, .. } => app == &self.app_id,
                _ => false,
            };
            if !subject_matches || !scope_matches {
                return Err("invalid host action policy projection");
            }
        }
        Ok(())
    }

    pub fn matches(&self, room: &str, app: &str, person: &str) -> bool {
        self.room == room && self.app_id == app && self.person == person
    }

    pub fn is_current_at(&self, now: u64) -> bool {
        self.issued_at_unix_ms <= now && now < self.valid_until_unix_ms
    }

    pub fn same_policy(&self, other: &Self) -> bool {
        self.room == other.room
            && self.app_id == other.app_id
            && self.person == other.person
            && self.owner == other.owner
            && self.capabilities == other.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{Principal, Verb};
    fn policy() -> AppActionAuthorityProjection {
        AppActionAuthorityProjection {
            schema_version: 1,
            room: "room-a".into(),
            app_id: "so.tilo.store".into(),
            person: "person-a".into(),
            owner: Some("person-a".into()),
            capabilities: vec![],
            issued_at_unix_ms: 10_000,
            valid_until_unix_ms: 13_000,
        }
    }
    #[test]
    fn action_authority_lease_and_scope_are_bounded() {
        let original = policy();
        original.validate().unwrap();
        assert!(!original.is_current_at(9_999));
        assert!(original.is_current_at(10_000));
        assert!(original.is_current_at(12_999));
        assert!(!original.is_current_at(13_000));
        let mut invalid = original.clone();
        invalid.valid_until_unix_ms += 1;
        assert!(invalid.validate().is_err());
        for (subject, scope) in [
            (
                Subject::Principal(Principal::Person("person-b".into())),
                Scope::App("so.tilo.store".into()),
            ),
            (
                Subject::Principal(Principal::App("so.tilo.store".into())),
                Scope::Workspace,
            ),
            (
                Subject::Principal(Principal::Person("person-a".into())),
                Scope::App("so.tilo.settings".into()),
            ),
            (
                Subject::Principal(Principal::Person("person-a".into())),
                Scope::Room("room-b".into()),
            ),
        ] {
            let mut invalid = original.clone();
            invalid
                .capabilities
                .push(CapOverride::membership(subject, "room-a", Verb::Admin));
            invalid.capabilities[0].scope = scope;
            assert!(invalid.validate().is_err());
        }
        let mut renewed = original.clone();
        renewed.issued_at_unix_ms += 1;
        renewed.valid_until_unix_ms += 1;
        assert!(original.same_policy(&renewed));
        renewed.owner = None;
        assert!(!original.same_policy(&renewed));
    }
}
