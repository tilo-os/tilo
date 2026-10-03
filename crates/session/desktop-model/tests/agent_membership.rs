//! An agent is admitted to a room the same way anybody is — by a
//! capability naming it. Asserts on the production readers, because the
//! failure worth catching is a roster showing or hiding the wrong people.

use collab_types::caps::{CapOverride, ResourceRef, Scope, Subject, Verb};
use collab_types::role::Role as CapRole;
use tilo_desktop_model::*;

/// The clock these fixtures run at, Unix seconds.
const NOW: u64 = 1_800_000_000;

fn room() -> Workspace {
    let mut ws = Workspace::new(WorkspaceId(1), "Demo");
    ws.set_owner("ada");
    ws
}

#[test]
fn only_a_live_room_grant_makes_an_agent_a_member() {
    // Phase `a_room_with_no_grant_has_no_agents`.
    // The floor, and the one that matters most: a room nobody has
    // admitted an agent to has no agents in it. A default agent, a
    // placeholder, or a row that appears because the feature exists
    // would all show up here.
    {
        assert!(room().agent_members_at(NOW).is_empty());
    }
    // Phase `a_room_grant_admits_the_agent_and_a_revoke_removes_it`.
    {
        let mut ws = room();
        let id = ws
            .grant_as_at(
                &"ada".to_string(),
                CapOverride::membership(
                    Subject::agent("scribe"),
                    ws.id.raw().to_string(),
                    Verb::Write,
                ),
                NOW,
            )
            .expect("the owner may administer the room");

        assert_eq!(
            ws.agent_members_at(NOW),
            vec![("scribe".to_string(), CapRole::Editor)],
            "the verb on the membership capability is the role",
        );
        assert_eq!(ws.role_of_agent_at("scribe", NOW), Some(CapRole::Editor));

        assert!(ws.revoke_as_at(&"ada".to_string(), &id, NOW));
        assert!(
            ws.agent_members_at(NOW).is_empty(),
            "revoking the capability is the whole of taking the agent out",
        );
        assert_eq!(ws.role_of_agent_at("scribe", NOW), None);
    }
    // Phase `a_grant_on_one_app_is_not_membership`.
    // An agent that holds a capability on one *app* and nothing else is
    // not a member — the same rule that keeps a person who was shared
    // one file out of the roster. Without this an agent granted access
    // to a single document would be presented as being in the room.
    {
        let mut ws = room();
        ws.grant_as_at(
            &"ada".to_string(),
            CapOverride::grant(
                Subject::agent("scribe"),
                Scope::App("so.tilo.word-processor".into()),
                Verb::Write,
            ),
            NOW,
        )
        .expect("owner grants");
        assert!(
            ws.agent_members_at(NOW).is_empty(),
            "a scope naming one app is a share, not membership",
        );
    }
    // Phase `an_expired_grant_admits_nobody`.
    // An expired capability is not a quieter membership, it is none.
    {
        let mut ws = room();
        ws.grant_as_at(
            &"ada".to_string(),
            CapOverride::membership(
                Subject::agent("scribe"),
                ws.id.raw().to_string(),
                Verb::Write,
            )
            .with_expiry(1),
            NOW,
        )
        .expect("owner grants");
        assert!(ws.agent_members_at(NOW).is_empty());
    }
}

#[test]
fn membership_is_replicated_and_people_and_agents_stay_separate() {
    // Phase `replicated_membership_does_not_depend_on_the_local_workspace_handle`.
    {
        let mut ws = room();
        ws.grant_capability(CapOverride::membership(
            Subject::person("remote-editor"),
            "a-different-peers-local-id",
            Verb::Write,
        ));
        assert_eq!(
            ws.role_of_at(&"remote-editor".to_string(), NOW),
            Some(CapRole::Editor)
        );
        assert!(ws.allows_at(
            &ws.actor_for_person_at(&"remote-editor".to_string(), NOW),
            &ResourceRef::App("so.tilo.notes".into()),
            Verb::Write,
            NOW,
        ));
    }
    // Phase `people_and_agents_do_not_leak_into_each_others_lists`.
    // The two lists are disjoint. A person in the roster must never
    // arrive in the agent list, and an agent must never arrive in the
    // roster — that second one is the OS claiming a person is present
    // when none is.
    {
        let mut ws = room();
        ws.grant_as_at(
            &"ada".to_string(),
            CapOverride::membership(
                Subject::person("bob"),
                ws.id.raw().to_string(),
                Verb::Write,
            ),
            NOW,
        )
        .expect("owner grants");
        ws.grant_as_at(
            &"ada".to_string(),
            CapOverride::membership(
                Subject::agent("scribe"),
                ws.id.raw().to_string(),
                Verb::Write,
            ),
            NOW,
        )
        .expect("owner grants");

        let people: Vec<String> = ws.roster_at(NOW).into_iter().map(|(u, _)| u).collect();
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
