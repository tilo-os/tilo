//! What an agent's certificate gives it: its person's default delegation, which the
//! workspace's own grants naming it then narrow.

use tilo_action_policy::{
    ActionKind, Answer, Authority, Context, Grantee, TreePath, WorkspacePolicy,
    roles::{GOVERNANCE_KIND, certified_agent, workspace_resource},
};

const WORKSPACE: &str = "r";
const PERSON: &str = "ada";
const AGENT: &str = "scout";

/// The certificate's grants say what the policy reads an agent of it may do: reading and
/// undoable changes allowed, outside effects asked first, and never the workspace's own actions.
#[test]
fn a_certificate_gives_its_agent_the_default_delegation_and_the_workspace_is_not_its_to_change() {
    let grants = certified_agent(WORKSPACE, (AGENT, PERSON), PERSON);
    assert!(!grants.is_empty());
    for grant in &grants {
        assert_eq!(grant.grantee, Grantee::agent(AGENT, PERSON), "{grant:?}");
        assert_eq!(grant.granted_by, Grantee::person(PERSON), "{grant:?}");
        assert!(
            grant.node.workspace_id() == Some(WORKSPACE),
            "every grant is of the workspace: {grant:?}"
        );
    }
    let policy = WorkspacePolicy::new(WORKSPACE, PERSON);
    let acting = tilo_member_types::Acting::agent(AGENT, PERSON)
        .acting_for(tilo_member_types::Acting::person(PERSON));
    let authority = Authority::of_acting(&acting, &policy, &grants);
    let cx = Context::at(1_000);
    let root = TreePath::workspace(WORKSPACE);
    assert_eq!(
        authority.decide(&root, ActionKind::Read, &cx).answer,
        Answer::Allow
    );
    assert_eq!(
        authority.decide(&root, ActionKind::Undoable, &cx).answer,
        Answer::Allow
    );
    assert_eq!(
        authority.decide(&root, ActionKind::Outside, &cx).answer,
        Answer::Ask
    );
    let governance = workspace_resource(WORKSPACE);
    assert_ne!(
        authority.decide(&governance, GOVERNANCE_KIND, &cx).answer,
        Answer::Allow,
        "the workspace's own actions are never its to take"
    );
}
