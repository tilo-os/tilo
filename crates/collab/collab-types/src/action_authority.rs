//! A host's read-only, app-scoped view of a person's authority in one room:
//! the room's owner, the person's role and the grants that can reach this
//! app. Its provenance is the launcher-owned read-only namespace, not a
//! caller-supplied value.
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Authority, Party, RoomPolicy, Subject, TreePath, roles};

pub const PROJECTION_PATH: &str = "/run/tilo/action-authority/current.json";
pub const MAX_PROJECTION_BYTES: usize = 64 * 1024;
pub const SCHEMA_VERSION: u16 = 2;
const MAX_GRANTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppActionAuthorityProjection {
    pub schema_version: u16,
    pub room: String,
    pub app_id: String,
    pub person: String,
    /// The room's owner and members, and the grants that can reach this app:
    /// those on the room itself and on the app's subtree.
    pub policy: RoomPolicy,
    /// The person works from a device that is not theirs, in a room they do
    /// not govern.
    #[serde(default)]
    pub foreign: bool,
    pub issued_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
}

impl AppActionAuthorityProjection {
    /// The projection for `app_id` of `person`'s authority in `policy`: the
    /// grants are narrowed to those that can decide one of the app's actions.
    #[must_use]
    pub fn project(
        policy: &RoomPolicy,
        app_id: &str,
        person: &str,
        foreign: bool,
        issued_at_unix_ms: u64,
        valid_until_unix_ms: u64,
    ) -> Self {
        let app = policy.node().app(app_id);
        let mut narrowed = policy.clone();
        narrowed.grants.retain(|grant| grant.node.overlaps(&app));
        Self {
            schema_version: SCHEMA_VERSION,
            room: policy.room.clone(),
            app_id: app_id.to_owned(),
            person: person.to_owned(),
            policy: narrowed,
            foreign,
            issued_at_unix_ms,
            valid_until_unix_ms,
        }
    }

    /// Refuse a projection that names another room or app, or reaches
    /// further than one app.
    ///
    /// # Errors
    /// Refuses malformed, foreign-scoped or excessively long-lived policy.
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid_name = |value: &str| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        };
        if self.schema_version != SCHEMA_VERSION
            || !valid_name(&self.room)
            || !valid_name(&self.app_id)
            || !valid_name(&self.person)
            || self.policy.room != self.room
            || self
                .policy
                .owner
                .as_deref()
                .is_some_and(|owner| !valid_name(owner))
            || self.policy.grants.len() > MAX_GRANTS
            || self.issued_at_unix_ms == 0
            || self.valid_until_unix_ms <= self.issued_at_unix_ms
            || self
                .valid_until_unix_ms
                .saturating_sub(self.issued_at_unix_ms)
                > 3_000
        {
            return Err("invalid host action policy projection");
        }
        let app = self.policy.node().app(&self.app_id);
        for grant in &self.policy.grants {
            if !grant.node.overlaps(&app) || grant.node.room_id() != Some(self.room.as_str()) {
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
            && self.policy == other.policy
            && self.foreign == other.foreign
    }

    /// The node of this app in the room.
    #[must_use]
    pub fn app_node(&self) -> TreePath {
        self.policy.node().app(&self.app_id)
    }

    /// The person's authority, bounded on a foreign device.
    #[must_use]
    pub fn authority<'a>(
        &'a self,
        foreign_bound: &'a [tilo_action_policy::Grant],
    ) -> Authority<'a> {
        let authority = Authority::person(self.policy.party(&self.person));
        if self.foreign {
            authority.bounded_by(
                Party::new(vec![Subject::ForeignDevice], foreign_bound)
                    .with_grants(self.policy.grants_of(&Subject::ForeignDevice)),
            )
        } else {
            authority
        }
    }
}

/// The grants every foreign device is held to, before a room narrows them.
#[must_use]
pub fn foreign_device_bound() -> Vec<tilo_action_policy::Grant> {
    roles::foreign_device_bound()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{ActionKind, Answer, Context, Grant, KindSet, Role};

    fn policy() -> AppActionAuthorityProjection {
        let mut room = RoomPolicy::new("room-a", "person-a");
        room.members.insert("person-b".into(), Role::Editor);
        AppActionAuthorityProjection::project(
            &room,
            "so.tilo.store",
            "person-b",
            false,
            10_000,
            13_000,
        )
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
        for node in [
            TreePath::room("room-a").app("so.tilo.settings"),
            TreePath::room("room-b"),
            TreePath::root(),
        ] {
            let mut invalid = original.clone();
            invalid.policy.grants.push(Grant::new(
                "foreign",
                Subject::person("person-b"),
                node,
                KindSet::ALL,
                Answer::Allow,
                Subject::person("person-a"),
            ));
            assert!(invalid.validate().is_err());
        }
        let mut renewed = original.clone();
        renewed.issued_at_unix_ms += 1;
        renewed.valid_until_unix_ms += 1;
        assert!(original.same_policy(&renewed));
        renewed.policy.owner = None;
        assert!(!original.same_policy(&renewed));
    }

    #[test]
    fn projection_keeps_only_grants_that_reach_the_app() {
        let mut room = RoomPolicy::new("room-a", "person-a");
        room.members.insert("person-b".into(), Role::Viewer);
        for (id, app) in [("store", "so.tilo.store"), ("settings", "so.tilo.settings")] {
            room.grants.push(Grant::new(
                id,
                Subject::person("person-b"),
                TreePath::room("room-a").app(app),
                KindSet::UNDOABLE,
                Answer::Allow,
                Subject::person("person-a"),
            ));
        }
        let projection =
            AppActionAuthorityProjection::project(&room, "so.tilo.store", "person-b", false, 1, 2);
        projection.validate().unwrap();
        assert!(
            projection
                .policy
                .grants
                .iter()
                .any(|grant| grant.id.as_str() == "store")
        );
        assert!(
            !projection
                .policy
                .grants
                .iter()
                .any(|grant| grant.id.as_str() == "settings")
        );
        let bound = foreign_device_bound();
        let node = projection.app_node().action("buy");
        let cx = Context::at(0);
        assert_eq!(
            projection
                .authority(&bound)
                .decide(&node, ActionKind::Undoable, &cx)
                .answer,
            Answer::Allow
        );
        assert_eq!(
            projection
                .authority(&bound)
                .decide(&node, ActionKind::Outside, &cx)
                .answer,
            Answer::Deny
        );
    }

    #[test]
    fn a_foreign_device_never_governs() {
        let room = RoomPolicy::new("room-a", "person-a");
        let mut projection =
            AppActionAuthorityProjection::project(&room, "so.tilo.store", "person-a", true, 1, 2);
        projection.policy.grants.push(Grant::new(
            "governance",
            Subject::person("person-a"),
            roles::room_resource("room-a"),
            KindSet::ALL,
            Answer::Allow,
            Subject::person("person-a"),
        ));
        let bound = foreign_device_bound();
        let cx = Context::at(0);
        let invite = roles::governance_action("room-a", roles::governance::INVITE);
        assert_eq!(
            projection
                .authority(&bound)
                .decide(&invite, ActionKind::Undoable, &cx)
                .answer,
            Answer::Deny
        );
        let content = projection.app_node().action("buy");
        assert_eq!(
            projection
                .authority(&bound)
                .decide(&content, ActionKind::Undoable, &cx)
                .answer,
            Answer::Allow,
            "content changes go ahead"
        );
        assert_eq!(
            projection
                .authority(&bound)
                .decide(&content, ActionKind::Outside, &cx)
                .answer,
            Answer::Ask,
            "an outside effect from a device not theirs asks first"
        );
    }
}
