//! A host's read-only, app-scoped view of a person's authority in one room:
//! the room's owner, the person's role and the grants that can reach this
//! app. Its provenance is the launcher-owned read-only namespace, not a
//! caller-supplied value, and it is current only while the host that wrote
//! it holds it (`tilo_files::native::held`): it carries no lifetime of its own.
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Authority, TreePath, WorkspacePolicy, roles};

pub const PROJECTION_PATH: &str = "/run/tilo/action-authority/current.json";
pub const MAX_PROJECTION_BYTES: usize = 64 * 1024;
pub const SCHEMA_VERSION: u16 = 3;
const MAX_GRANTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, from = "StoredProjection")]
pub struct AppActionAuthorityProjection {
    pub schema_version: u16,
    pub room: String,
    pub app_id: String,
    pub person: String,
    /// The room's owner and members, and the grants that can reach this app:
    /// those on the room itself and on the app's subtree.
    pub policy: WorkspacePolicy,
    /// The person works from a device that is not theirs, in a room they do
    /// not govern.
    #[serde(default)]
    pub foreign: bool,
}

/// An [`AppActionAuthorityProjection`] as written. Its policy loads with the
/// grants the code defines as it defines them now (the built-in roles' and
/// the room's make-up), which gives back those the projection left out;
/// [`From`] narrows them again.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredProjection {
    schema_version: u16,
    room: String,
    app_id: String,
    person: String,
    policy: WorkspacePolicy,
    #[serde(default)]
    foreign: bool,
}

impl From<StoredProjection> for AppActionAuthorityProjection {
    /// The grants the code defines are the code's, so they are narrowed to
    /// the app here as [`AppActionAuthorityProjection::project`] narrowed
    /// them: a projection reads back as the projection of the current policy.
    /// Grants given in the room stay as written, for
    /// [`AppActionAuthorityProjection::validate`] to judge.
    fn from(stored: StoredProjection) -> Self {
        let mut policy = stored.policy;
        let app = policy.node().app(&stored.app_id);
        policy
            .grants
            .retain(|grant| !roles::is_code_defined(grant) || grant.node.overlaps(&app));
        Self {
            schema_version: stored.schema_version,
            room: stored.room,
            app_id: stored.app_id,
            person: stored.person,
            policy,
            foreign: stored.foreign,
        }
    }
}

impl AppActionAuthorityProjection {
    /// The projection for `app_id` of `person`'s authority in `policy`: the
    /// grants are narrowed to those that can decide one of the app's actions.
    #[must_use]
    pub fn project(policy: &WorkspacePolicy, app_id: &str, person: &str, foreign: bool) -> Self {
        let app = policy.node().app(app_id);
        let mut narrowed = policy.clone();
        narrowed.grants.retain(|grant| grant.node.overlaps(&app));
        Self {
            schema_version: SCHEMA_VERSION,
            room: policy.workspace.clone(),
            app_id: app_id.to_owned(),
            person: person.to_owned(),
            policy: narrowed,
            foreign,
        }
    }

    /// Refuse a projection that names another room or app, or reaches
    /// further than one app.
    ///
    /// # Errors
    /// Refuses malformed or foreign-scoped policy.
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid_name = |value: &str| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        };
        if self.schema_version != SCHEMA_VERSION {
            return Err("invalid host action policy projection: another schema");
        }
        if !valid_name(&self.room)
            || !valid_name(&self.app_id)
            || !valid_name(&self.person)
            || self
                .policy
                .owner
                .as_deref()
                .is_some_and(|owner| !valid_name(owner))
        {
            return Err("invalid host action policy projection: a malformed name");
        }
        if self.policy.workspace != self.room {
            return Err("invalid host action policy projection: the policy is another room's");
        }
        if self.policy.grants.len() > MAX_GRANTS {
            return Err("invalid host action policy projection: too many grants");
        }
        let app = self.policy.node().app(&self.app_id);
        for grant in &self.policy.grants {
            if !grant.node.overlaps(&app) {
                return Err("invalid host action policy projection: a grant reaches past the app");
            }
            if grant.node.workspace_id() != Some(self.room.as_str()) {
                return Err("invalid host action policy projection: a grant is another room's");
            }
        }
        Ok(())
    }

    pub fn matches(&self, room: &str, app: &str, person: &str) -> bool {
        self.room == room && self.app_id == app && self.person == person
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
        self.policy
            .person_on_device(&self.person, self.foreign, foreign_bound)
    }
}

/// The grants every foreign device is held to, before a room narrows them.
#[must_use]
pub fn foreign_device_bound() -> Vec<tilo_action_policy::Grant> {
    roles::foreign_device_bound().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{ActionKind, Answer, Context, Grant, Grantee, KindSet, Role};

    fn policy() -> AppActionAuthorityProjection {
        let mut room = WorkspacePolicy::new("room-a", "person-a");
        room.members.insert("person-b".into(), Role::Editor);
        AppActionAuthorityProjection::project(&room, "so.tilo.store", "person-b", false)
    }

    #[test]
    fn action_authority_scope_is_bounded() {
        let original = policy();
        original.validate().unwrap();
        let mut invalid = original.clone();
        invalid.schema_version = 2;
        assert!(invalid.validate().is_err(), "another schema is refused");
        for node in [
            TreePath::workspace("room-a").app("so.tilo.settings"),
            TreePath::workspace("room-b"),
            TreePath::root(),
        ] {
            let mut invalid = original.clone();
            invalid.policy.grants.push(Grant::new(
                "foreign",
                Grantee::person("person-b"),
                node,
                KindSet::ALL,
                Answer::Allow,
                Grantee::person("person-a"),
            ));
            assert!(invalid.validate().is_err());
        }
    }

    /// What the launcher and the app read is what the host projected: the
    /// built-in roles loading as the code defines them adds none of the
    /// grants the projection left out.
    #[test]
    fn a_projection_reads_back_as_written() {
        let original = policy();
        assert!(
            original.policy.grants.len() < WorkspacePolicy::new("room-a", "person-a").grants.len(),
            "the room holds role grants that reach no action of the app"
        );
        let read: AppActionAuthorityProjection =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        read.validate().unwrap();
        assert_eq!(read, original);
    }

    /// Every grant a read restores (the roles', the session app's, the
    /// device panel's) is narrowed again, so an app's launch reads back
    /// the projection its host wrote, also for a room no one owns yet.
    #[test]
    fn every_restored_grant_is_narrowed_on_read() {
        let owned = WorkspacePolicy::new("room-a", "person-a");
        let unowned = WorkspacePolicy::unowned("room-a", &Grantee::role(Role::Owner));
        for room in [owned, unowned.with_current_roles()] {
            for app in ["so.tilo.notes", roles::SESSION_APP, roles::DEVICE_PANEL] {
                let written = AppActionAuthorityProjection::project(&room, app, "person-a", false);
                written.validate().unwrap();
                let read: AppActionAuthorityProjection =
                    serde_json::from_slice(&serde_json::to_vec(&written).unwrap()).unwrap();
                read.validate()
                    .unwrap_or_else(|error| panic!("{app}'s projection read back: {error}"));
                assert_eq!(read, written, "{app}");
            }
        }
    }

    /// A projection written while its policy named the workspace for what it
    /// means still reads.
    #[test]
    fn a_projection_written_with_a_workspace_policy_still_reads() {
        let written = serde_json::to_string(&policy()).unwrap();
        assert_eq!(
            written.matches("\"room\":\"room-a\"").count(),
            2,
            "{written}"
        );
        let stored = written.replace("\"policy\":{\"room\":", "\"policy\":{\"workspace\":");
        assert_eq!(
            stored.matches("\"workspace\":\"room-a\"").count(),
            1,
            "{stored}"
        );
        let read: AppActionAuthorityProjection = serde_json::from_str(&stored).unwrap();
        read.validate().unwrap();
        assert_eq!(read, policy());
    }

    /// The grants the code makes for the session's own app reach it, so a
    /// projection of that app keeps them and reads back as it was written,
    /// in the order it wrote them.
    #[test]
    fn a_projection_of_the_sessions_own_app_reads_back_as_written() {
        let projection = AppActionAuthorityProjection::project(
            &WorkspacePolicy::new("room-a", "person-a"),
            tilo_action_policy::roles::SESSION_APP,
            "person-a",
            false,
        );
        let read: AppActionAuthorityProjection =
            serde_json::from_slice(&serde_json::to_vec(&projection).unwrap()).unwrap();
        read.validate().unwrap();
        assert_eq!(read, projection);
    }

    /// A grant the room gave that reaches beyond the app is still refused
    /// when read, not dropped.
    #[test]
    fn a_read_projection_keeps_room_grants_for_validation() {
        let mut written = policy();
        written.policy.grants.push(Grant::new(
            "foreign",
            Grantee::person("person-b"),
            TreePath::workspace("room-a").app("so.tilo.settings"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person("person-a"),
        ));
        let read: AppActionAuthorityProjection =
            serde_json::from_slice(&serde_json::to_vec(&written).unwrap()).unwrap();
        assert!(read.validate().is_err());
    }

    #[test]
    fn projection_keeps_only_grants_that_reach_the_app() {
        let mut room = WorkspacePolicy::new("room-a", "person-a");
        room.members.insert("person-b".into(), Role::Viewer);
        for (id, app) in [("store", "so.tilo.store"), ("settings", "so.tilo.settings")] {
            room.grants.push(Grant::new(
                id,
                Grantee::person("person-b"),
                TreePath::workspace("room-a").app(app),
                KindSet::UNDOABLE,
                Answer::Allow,
                Grantee::person("person-a"),
            ));
        }
        let projection =
            AppActionAuthorityProjection::project(&room, "so.tilo.store", "person-b", false);
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
        let room = WorkspacePolicy::new("room-a", "person-a");
        let mut projection =
            AppActionAuthorityProjection::project(&room, "so.tilo.store", "person-a", true);
        projection.policy.grants.push(Grant::new(
            "governance",
            Grantee::person("person-a"),
            roles::workspace_resource("room-a"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person("person-a"),
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
