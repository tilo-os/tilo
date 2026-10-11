//! An agent is admitted to a room the same way anybody is given authority —
//! by a grant naming it on the room. Asserts on the production readers,
//! because the failure worth catching is a roster showing or hiding the
//! wrong people.

use tilo_action_policy::{
    ActionKind, Answer, Change, Grant, GrantId, KindSet, Lifetime, Role, Subject, TreePath,
};
use tilo_desktop_model::*;

/// The clock these fixtures run at, Unix seconds.
const NOW: u64 = 1_800_000_000;

fn room() -> Workspace {
    let mut ws = Workspace::new(WorkspaceId(1), "Demo");
    ws.set_owner("quinten");
    ws
}

fn agent_grant(ws: &Workspace, id: &str, node: TreePath) -> Grant {
    let _ = ws;
    Grant::new(
        id,
        Subject::agent("scribe"),
        node,
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Subject::person("quinten"),
    )
}

#[test]
fn only_a_live_room_grant_makes_an_agent_a_member() {
    // Phase `a_room_with_no_grant_has_no_agents`.
    // The floor, and the one that matters most: a room nobody has
    // admitted an agent to has no agents in it.
    {
        assert!(room().agent_members_at(NOW).is_empty());
    }
    // Phase `a_room_grant_admits_the_agent_and_a_revoke_removes_it`.
    {
        let mut ws = room();
        let grant = agent_grant(&ws, "scribe-member", ws.node());
        ws.govern_as_at("quinten", Change::Grant(grant), NOW)
            .expect("the owner may grant in the room");

        assert_eq!(
            ws.agent_members_at(NOW),
            vec![("scribe".to_string(), KindSet::READ_UNDOABLE)],
            "the kinds of the room grant are what the agent may do",
        );

        ws.govern_as_at(
            "quinten",
            Change::Revoke(GrantId::new("scribe-member")),
            NOW,
        )
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
        let mut ws = room();
        let grant = agent_grant(&ws, "scribe-app", ws.node().app("so.tilo.word-processor"));
        ws.govern_as_at("quinten", Change::Grant(grant), NOW)
            .expect("owner grants");
        assert!(
            ws.agent_members_at(NOW).is_empty(),
            "a grant naming one app is a share, not membership",
        );
    }
    // Phase `an_expired_grant_admits_nobody`.
    // An expired grant is not a quieter membership, it is none.
    {
        let mut ws = room();
        let grant =
            agent_grant(&ws, "scribe-expired", ws.node()).with_lifetime(Lifetime::Until(1_000));
        ws.govern_as_at("quinten", Change::Grant(grant), NOW)
            .expect("owner grants");
        assert!(ws.agent_members_at(NOW).is_empty());
    }
}

#[test]
fn a_grant_on_another_room_is_not_membership_here() {
    let mut ws = room();
    let here = agent_grant(&ws, "member", ws.node());
    ws.grant(Grant {
        subject: Subject::agent("member"),
        ..here
    });
    ws.grant(Grant {
        id: GrantId::new("other-room"),
        subject: Subject::agent("other-room"),
        node: TreePath::room("a-different-durable-room"),
        ..agent_grant(&ws, "other-room", ws.node())
    });
    assert_eq!(
        ws.agent_members_at(NOW),
        vec![("member".to_string(), KindSet::READ_UNDOABLE)]
    );
}

#[test]
fn the_people_roster_is_the_owner_and_the_members() {
    let mut ws = room();
    ws.admit("member", Role::Viewer);
    ws.grant(Grant::new(
        "elsewhere",
        Subject::person("not-a-member-here"),
        TreePath::room("other-room"),
        KindSet::ALL,
        Answer::Allow,
        Subject::person("quinten"),
    ));
    assert_eq!(
        ws.roster(),
        vec![
            ("quinten".into(), Role::Owner),
            ("member".into(), Role::Viewer),
        ]
    );
    assert_eq!(ws.role_of("member"), Some(Role::Viewer));
    assert_eq!(ws.role_of("not-a-member-here"), None);
}

#[test]
fn membership_is_replicated_and_people_and_agents_stay_separate() {
    // Phase `a_members_role_decides_what_they_may_do`.
    {
        let mut ws = room();
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
        let mut ws = room();
        ws.govern_as_at(
            "quinten",
            Change::AddMember {
                person: "xaver".into(),
                role: Role::Editor,
            },
            NOW,
        )
        .expect("owner admits");
        let grant = agent_grant(&ws, "scribe-member", ws.node());
        ws.govern_as_at("quinten", Change::Grant(grant), NOW)
            .expect("owner grants");

        let people: Vec<String> = ws.roster().into_iter().map(|(u, _)| u).collect();
        assert!(people.contains(&"xaver".to_string()));
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
