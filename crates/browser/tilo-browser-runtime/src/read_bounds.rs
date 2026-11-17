//! Who may read what a page holds of another app's documents.
//!
//! A window reads another app's document through its session, and the page answers from what it
//! holds (`browser::session_asks`): for a desktop's windows, and for the window of a page that
//! shows one app alone. Whoever asks, the same bounds decide before anything is read, as the
//! workspace's broker decides a read: the person at the page and nobody else, in the workspace
//! the page shows, never an agent; nothing on a guess before the workspace said what that
//! person may do; and then the permission at the document's own node.

use tilo_actions::remote::{
    ActionCallError, Caller, RESOURCE_READ_REFUSED, RESOURCE_READS_FOR_AGENTS, ResourceAddress,
};
use ui::permissions::SharedPermissions;

/// The person at a page, and what the page knows of what they may read in its workspace.
pub(crate) struct Reader<'a> {
    /// The person at the page: a read is theirs or nobody's.
    pub(crate) person: &'a str,
    /// The workspace the page shows.
    pub(crate) workspace: &'a str,
    /// Whether the page joined a workspace whose server says what this person may do there. A
    /// page that joined none shows its person's own, where nobody else decides.
    pub(crate) joined: bool,
    /// The role the workspace answered this person holds, as `desktop::SharedRole::code` counts
    /// it: 0 until it has.
    pub(crate) role: u8,
    /// What the person may do here, as the workspace answered it.
    pub(crate) permissions: &'a SharedPermissions,
}

impl Reader<'_> {
    /// Whether `caller` may read the document `address` now.
    ///
    /// # Errors
    ///
    /// [`ActionCallError::Refused`] for every agent, for anyone but the person at the page asking
    /// in its workspace, and for a document that person may not read;
    /// [`ActionCallError::Unavailable`] while the workspace has not said what they may do.
    pub(crate) fn may_read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<(), ActionCallError> {
        let refused = |reason: &str| ActionCallError::Refused {
            reason: reason.into(),
        };
        // As the broker: a device hands its agents nothing here, and a read is its person's, in
        // the workspace this page shows. Nobody else asks a page's session.
        if caller.acting().is_agent() {
            return Err(refused(RESOURCE_READS_FOR_AGENTS));
        }
        let person = caller
            .acting()
            .person_id()
            .or_else(|| caller.acting_for_person());
        if person != Some(self.person) || caller.workspace() != Some(self.workspace) {
            return Err(refused(RESOURCE_READ_REFUSED));
        }
        // What this person may read is the workspace's to say: until it has, nothing is read on a
        // guess, and the read waits as for a document still arriving.
        if self.joined && self.role == 0 {
            return Err(ActionCallError::Unavailable {
                reason: "the workspace has not said yet what you may read here".into(),
            });
        }
        // Decided at the record's node, as the broker decides it: allowed outright, a member's own
        // record theirs alone.
        let node = ui::permissions::TreePath::root()
            .app(&address.app)
            .resource(&address.record);
        if self
            .permissions
            .borrow()
            .answer(&node, ui::permissions::ActionKind::Read)
            != ui::permissions::Answer::Allow
        {
            return Err(refused(RESOURCE_READ_REFUSED));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::{
        ActionCallError, Caller, RESOURCE_READ_REFUSED, RESOURCE_READS_FOR_AGENTS, ResourceAddress,
    };
    use ui::permissions::{
        Acting, Grantee, MemberAccess, MemberId, PermissionContext, Role, SharedPermissions, policy,
    };

    use super::Reader;

    const ADA: &str = "ada";
    const HOME: &str = "w1";

    /// What `person` may do as a member holding `role`.
    fn member(person: &str, role: Role) -> SharedPermissions {
        SharedPermissions::new(PermissionContext::member(&MemberId::person(person), role))
    }

    /// What `person` may do in a workspace they are no member of.
    fn no_member(person: &str) -> SharedPermissions {
        let policy = policy::WorkspacePolicy::unowned(HOME, &Grantee::role(Role::Owner));
        SharedPermissions::new(PermissionContext::new(
            Acting::person(person.to_owned()),
            Vec::new(),
            MemberAccess::person(policy, person.to_owned()),
        ))
    }

    /// What Tasks keeps for the workspace, as a view asks for it.
    fn tasks() -> ResourceAddress {
        ResourceAddress {
            app: "so.tilo.tasks".into(),
            record: String::new(),
        }
    }

    /// Ada at her page, which shows the workspace `HOME`.
    fn reader(permissions: &SharedPermissions, joined: bool, role: u8) -> Reader<'_> {
        Reader {
            person: ADA,
            workspace: HOME,
            joined,
            role,
            permissions,
        }
    }

    /// What a read was answered, in words a failed comparison shows.
    fn said(answer: Result<(), ActionCallError>) -> String {
        match answer {
            Ok(()) => "read".into(),
            Err(ActionCallError::Refused { reason }) => format!("refused: {reason}"),
            Err(ActionCallError::Unavailable { reason }) => format!("unavailable: {reason}"),
            Err(other) => format!("{other:?}"),
        }
    }

    #[test]
    fn the_person_at_the_page_reads_in_its_workspace_and_nobody_else_does() {
        let permissions = member(ADA, Role::Viewer);
        let reader = reader(&permissions, true, 1);
        let refused = format!("refused: {RESOURCE_READ_REFUSED}");
        let ada = Caller::local_person(ADA).in_workspace(HOME);
        assert_eq!(said(reader.may_read(&ada, &tasks())), "read");
        let another = Caller::local_person("bo").in_workspace(HOME);
        assert_eq!(said(reader.may_read(&another, &tasks())), refused);
        let elsewhere = Caller::local_person(ADA).in_workspace("w2");
        assert_eq!(said(reader.may_read(&elsewhere, &tasks())), refused);
        let nowhere = Caller::local_person(ADA);
        assert_eq!(said(reader.may_read(&nowhere, &tasks())), refused);
    }

    #[test]
    fn an_agent_reads_nothing_here_even_for_the_person_at_the_page() {
        // Where the person themselves may read everything: their own page, joined to nothing.
        let permissions = member(ADA, Role::Owner);
        let reader = reader(&permissions, false, 0);
        let ada = Caller::local_person(ADA).in_workspace(HOME);
        assert_eq!(said(reader.may_read(&ada, &tasks())), "read");
        let refused = format!("refused: {RESOURCE_READS_FOR_AGENTS}");
        let agent = Caller::local_agent("helper").in_workspace(HOME);
        assert_eq!(said(reader.may_read(&agent, &tasks())), refused);
        let for_ada = Caller::local_agent("helper")
            .acting_for(ADA)
            .in_workspace(HOME);
        assert_eq!(said(reader.may_read(&for_ada, &tasks())), refused);
    }

    #[test]
    fn nothing_is_read_before_the_workspace_said_what_the_person_may_do() {
        let permissions = member(ADA, Role::Viewer);
        let ada = Caller::local_person(ADA).in_workspace(HOME);
        assert_eq!(
            said(reader(&permissions, true, 0).may_read(&ada, &tasks())),
            "unavailable: the workspace has not said yet what you may read here"
        );
        // Once it has, the permission decides; so it does where no workspace was joined.
        assert_eq!(
            said(reader(&permissions, true, 1).may_read(&ada, &tasks())),
            "read"
        );
        assert_eq!(
            said(reader(&permissions, false, 0).may_read(&ada, &tasks())),
            "read"
        );
    }

    #[test]
    fn the_permission_at_the_documents_node_decides_a_read() {
        let ada = Caller::local_person(ADA).in_workspace(HOME);
        let refused = format!("refused: {RESOURCE_READ_REFUSED}");
        // Whatever role the page was told: the workspace's permissions hold nothing for her.
        let outside = no_member(ADA);
        assert_eq!(
            said(reader(&outside, true, 3).may_read(&ada, &tasks())),
            refused
        );
        assert_eq!(
            said(reader(&outside, false, 0).may_read(&ada, &tasks())),
            refused
        );
        let inside = member(ADA, Role::Viewer);
        assert_eq!(
            said(reader(&inside, true, 3).may_read(&ada, &tasks())),
            "read"
        );
    }
}
