//! An agent is admitted to a workspace the same way anybody is given authority —
//! by a grant naming it on the workspace. Asserts on the production readers,
//! because the failure worth catching is a roster showing or hiding the
//! wrong people.

use tilo_action_policy::{
    ActionKind, Answer, Change, Grant, GrantId, Grantee, KindSet, Lifetime, Role, TreePath,
};
use tilo_desktop_model::*;

/// The clock these fixtures run at, Unix seconds.
const NOW: u64 = 1_800_000_000;

fn workspace() -> Workspace {
    let mut ws = Workspace::new(WorkspaceSlot(1), "Demo");
    ws.set_owner("ada");
    ws
}

fn agent_grant(ws: &Workspace, id: &str, node: TreePath) -> Grant {
    let _ = ws;
    Grant::new(
        id,
        Grantee::agent("scribe"),
        node,
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person("ada"),
    )
}

#[test]
fn only_a_live_workspace_grant_makes_an_agent_a_member() {
    // The workspace has no grant yet.
    // The floor, and the one that matters most: a workspace nobody has
    // admitted an agent to has no agents in it.
    {
        assert!(workspace().agent_members_at(NOW).is_empty());
    }
    // A grant admits the agent and a revoke removes it.
    {
        let mut ws = workspace();
        let grant = agent_grant(&ws, "scribe-member", ws.node());
        ws.govern_as_at("ada", Change::Grant(grant), NOW)
            .expect("the owner may grant in the workspace");

        assert_eq!(
            ws.agent_members_at(NOW),
            vec![("scribe".to_string(), KindSet::READ_UNDOABLE)],
            "the kinds of the workspace grant are what the agent may do",
        );

        ws.govern_as_at("ada", Change::Revoke(GrantId::new("scribe-member")), NOW)
            .expect("the owner may revoke");
        assert!(
            ws.agent_members_at(NOW).is_empty(),
            "revoking the grant is the whole of taking the agent out",
        );
    }
    // Phase `a_grant_on_one_app_is_not_membership`.
    // An agent that holds a grant on one *app* and nothing else is not a
    // member — the same rule that keeps a person who was shared one file out
    // of the roster.
    {
        let mut ws = workspace();
        let grant = agent_grant(&ws, "scribe-app", ws.node().app("so.tilo.word-processor"));
        ws.govern_as_at("ada", Change::Grant(grant), NOW)
            .expect("owner grants");
        assert!(
            ws.agent_members_at(NOW).is_empty(),
            "a grant naming one app is a share, not membership",
        );
    }
    // Phase `an_expired_grant_admits_nobody`.
    // An expired grant is not a quieter membership, it is none.
    {
        let mut ws = workspace();
        let grant =
            agent_grant(&ws, "scribe-expired", ws.node()).with_lifetime(Lifetime::Until(1_000));
        ws.govern_as_at("ada", Change::Grant(grant), NOW)
            .expect("owner grants");
        assert!(ws.agent_members_at(NOW).is_empty());
    }
}

#[test]
fn a_grant_on_another_workspace_is_not_membership_here() {
    let mut ws = workspace();
    let here = agent_grant(&ws, "member", ws.node());
    ws.grant(Grant {
        subject: Grantee::agent("member"),
        ..here
    });
    ws.grant(Grant {
        id: GrantId::new("other-workspace"),
        subject: Grantee::agent("other-workspace"),
        node: TreePath::workspace("a-different-durable-workspace"),
        ..agent_grant(&ws, "other-workspace", ws.node())
    });
    assert_eq!(
        ws.agent_members_at(NOW),
        vec![("member".to_string(), KindSet::READ_UNDOABLE)]
    );
}

#[test]
fn the_people_roster_is_the_owner_and_the_members() {
    let mut ws = workspace();
    ws.admit("member", Role::Viewer);
    ws.grant(Grant::new(
        "elsewhere",
        Grantee::person("not-a-member-here"),
        TreePath::workspace("other-workspace"),
        KindSet::ALL,
        Answer::Allow,
        Grantee::person("ada"),
    ));
    assert_eq!(
        ws.roster(),
        vec![("ada".into(), Role::Owner), ("member".into(), Role::Viewer),]
    );
    assert_eq!(ws.role_of("member"), Some(Role::Viewer));
    assert_eq!(ws.role_of("not-a-member-here"), None);
}

#[test]
fn membership_is_replicated_and_people_and_agents_stay_separate() {
    // Phase `a_members_role_decides_what_they_may_do`.
    {
        let mut ws = workspace();
        ws.admit("remote-editor", Role::Editor);
        assert_eq!(ws.role_of("remote-editor"), Some(Role::Editor));
        assert_eq!(
            ws.answer_at(
                "remote-editor",
                &ws.node().app("so.tilo.notes").action("rename"),
                ActionKind::Undoable,
                NOW,
            ),
            Answer::Allow
        );
        assert_eq!(
            ws.answer_at(
                "remote-editor",
                &ws.node().app("so.tilo.notes").action("publish"),
                ActionKind::Outside,
                NOW,
            ),
            Answer::Ask
        );
    }
    // Phase `people_and_agents_do_not_leak_into_each_others_lists`.
    // The two lists are disjoint. A person in the roster must never arrive
    // in the agent list, and an agent must never arrive in the roster —
    // that second one is the OS claiming a person is present when none is.
    {
        let mut ws = workspace();
        ws.govern_as_at(
            "ada",
            Change::AddMember {
                person: "bob".into(),
                role: Role::Editor,
            },
            NOW,
        )
        .expect("owner admits");
        let grant = agent_grant(&ws, "scribe-member", ws.node());
        ws.govern_as_at("ada", Change::Grant(grant), NOW)
            .expect("owner grants");

        let people: Vec<String> = ws.roster().into_iter().map(|(u, _)| u).collect();
        assert!(people.contains(&"bob".to_string()));
        assert!(
            !people.contains(&"scribe".to_string()),
            "an agent must not be listed as a person: {people:?}",
        );

        let agents: Vec<String> = ws
            .agent_members_at(NOW)
            .into_iter()
            .map(|(a, _)| a)
            .collect();
        assert_eq!(agents, vec!["scribe".to_string()]);
    }
}
