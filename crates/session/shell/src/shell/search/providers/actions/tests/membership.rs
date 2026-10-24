//! Workspace membership and invitation actions: roles, removals and the
//! invitation requests the palette queues.

use super::*;

/// [`chrome_with_room`] with `member` admitted as an Editor through the
/// production granting path, and the room document seeded from it.
fn chrome_with_member(member: &str) -> Chrome {
    let chrome = chrome_with_room();
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let ws = tree.desktop.active_workspace_mut().unwrap();
        ws.govern_as_at(
            FIXTURE_USER,
            tilo_action_policy::Change::AddMember {
                person: member.to_owned(),
                role: tilo_action_policy::Role::Editor,
            },
            tilo_clock::unix_now(),
        )
        .expect("the owner may admit a member");
        tree.seed_doc_from_cache();
    }
    chrome
}

/// Invoke one shell action the way the control socket does: resolve its
/// exact address in a catalog, then bind and run its JSON arguments.
/// Every action stays selectable, so the provider's own authority
/// check is the one under test.
pub(super) fn invoke_shell_action(
    chrome: &Chrome,
    id: &str,
    args: &serde_json::Value,
) -> Result<crate::shell::search::InvocationDisposition, ActionError> {
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
    let mut registry = crate::shell::search::SearchRegistry::new();
    registry.register(Rc::new(ActionsProvider::new()));
    let mut doc = AutoCommit::new();
    let resource = ResourceRef::new("actions", id);
    let catalog = ActionCatalog::build_exact(
        &registry,
        &resource,
        crate::shell::search::SearchScope::All,
        None,
        &chrome.borrow(),
        &doc,
        &automerge::ROOT,
        &AllVisible,
    );
    let action = catalog
        .select(&resource)
        .ok_or_else(|| ActionError::Refused(format!("{id} is not published")))?;
    action.invoke_json(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        chrome,
        args.as_object().expect("arguments are an object"),
    )
}

fn role_of(chrome: &Chrome, member: &str) -> Option<crate::shell::state::CapRole> {
    let state = chrome.borrow();
    let tree = state.tree();
    let role = tree
        .desktop
        .active_workspace()
        .and_then(|ws| ws.role_of(member));
    drop(tree);
    role
}

/// The room's governor takes the changes asked of it as `actor`, and
/// the room's projection carries them here: for tests, which have no
/// governor.
pub(super) fn governed(chrome: &Chrome, actor: &str) {
    let state = chrome.borrow();
    let mut tree = state.tree_mut();
    assert!(
        tree.has_governance_requests(),
        "the change is asked of the room's governor"
    );
    tree.govern_requests_here_for_test(&actor.to_owned());
}

/// The active room's whole authority: its policy and its sandbox.
fn capabilities(
    chrome: &Chrome,
) -> Option<(
    tilo_action_policy::RoomPolicy,
    Vec<crate::shell::state::CapOverride>,
)> {
    let state = chrome.borrow();
    let tree = state.tree();
    let caps = tree
        .desktop
        .active_workspace()
        .map(|ws| (ws.policy.clone(), ws.sandbox.clone()));
    drop(tree);
    caps
}

#[test]
fn member_role_set_action_changes_role_through_workspace_caps() {
    use crate::shell::state::CapRole;
    // Phase `the_owner_changes_a_members_role_in_the_cache_and_the_room_document`.
    {
        let chrome = chrome_with_member("bob");
        assert_eq!(role_of(&chrome, "bob"), Some(CapRole::Editor));
        let disposition = invoke_shell_action(
            &chrome,
            MEMBER_ROLE_SET_ID,
            &serde_json::json!({"participant_id": "bob", "role": "viewer", "confirm": true}),
        )
        .expect("the owner may change a member's role");
        assert!(matches!(
            disposition,
            crate::shell::search::InvocationDisposition::Accepted(_)
        ));
        assert_eq!(
            role_of(&chrome, "bob"),
            Some(CapRole::Editor),
            "nothing changes here before the room's governor takes it"
        );
        governed(&chrome, FIXTURE_USER);
        assert_eq!(role_of(&chrome, "bob"), Some(CapRole::Viewer));
    }

    // Phase `an_unconfirmed_or_unknown_change_changes_nothing`.
    {
        let chrome = chrome_with_member("bob");
        let before = capabilities(&chrome);
        assert!(
            invoke_shell_action(
                &chrome,
                MEMBER_ROLE_SET_ID,
                &serde_json::json!({"participant_id": "bob", "role": "viewer"}),
            )
            .is_err(),
            "a privileged change needs explicit confirmation"
        );
        assert!(
            invoke_shell_action(
                &chrome,
                MEMBER_ROLE_SET_ID,
                &serde_json::json!({"participant_id": "bob", "role": "owner", "confirm": true}),
            )
            .is_err(),
            "ownership is not a role this action can hand out"
        );
        assert!(
            invoke_shell_action(
                &chrome,
                MEMBER_ROLE_SET_ID,
                &serde_json::json!({"participant_id": "carol", "role": "editor", "confirm": true}),
            )
            .is_err(),
            "setting a role is not a way to admit someone who was never invited"
        );
        assert_eq!(capabilities(&chrome), before);
    }

    // Phase `a_delegated_administrator_passes_the_same_capability_check`.
    {
        let chrome = chrome_with_member("bob");
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let ws = tree.desktop.active_workspace_mut().unwrap();
            ws.set_owner("foreign-owner");
            let now = tilo_clock::unix_now();
            ws.govern_as_at(
                "foreign-owner",
                tilo_action_policy::Change::AddMember {
                    person: FIXTURE_USER.to_owned(),
                    role: tilo_action_policy::Role::Editor,
                },
                now,
            )
            .expect("the owner admits the delegate");
            let set_role = tilo_action_policy::roles::governance_action(
                &ws.room().to_string(),
                tilo_action_policy::roles::governance::SET_ROLE,
            );
            ws.govern_as_at(
                "foreign-owner",
                tilo_action_policy::Change::Grant(tilo_action_policy::Grant::new(
                    "delegate-set-role",
                    tilo_action_policy::Subject::person(FIXTURE_USER),
                    set_role,
                    tilo_action_policy::KindSet::ALL,
                    tilo_action_policy::Answer::Allow,
                    tilo_action_policy::Subject::person("foreign-owner"),
                )),
                now,
            )
            .expect("the owner delegates administration");
            drop(tree);
        }
        invoke_shell_action(
            &chrome,
            MEMBER_ROLE_SET_ID,
            &serde_json::json!({"participant_id": "bob", "role": "commenter", "confirm": true}),
        )
        .expect("administration delegated as a capability is administration");
        governed(&chrome, FIXTURE_USER);
        assert_eq!(role_of(&chrome, "bob"), Some(CapRole::Commenter));
    }
}

#[test]
fn member_remove_action_refuses_non_admin_and_owner() {
    // Phase `the_owner_removes_a_member`.
    {
        let chrome = chrome_with_member("bob");
        invoke_shell_action(
            &chrome,
            MEMBER_REMOVE_ID,
            &serde_json::json!({"participant_id": "bob", "confirm": true}),
        )
        .expect("the owner may remove a member");
        governed(&chrome, FIXTURE_USER);
        assert_eq!(role_of(&chrome, "bob"), None);
    }

    // Phase `the_recorded_owner_cannot_be_removed`.
    {
        let chrome = chrome_with_member("bob");
        let before = capabilities(&chrome);
        assert!(
            invoke_shell_action(
                &chrome,
                MEMBER_REMOVE_ID,
                &serde_json::json!({"participant_id": FIXTURE_USER, "confirm": true}),
            )
            .is_err()
        );
        assert_eq!(capabilities(&chrome), before);
    }

    // Phase `a_non_administrator_is_refused_by_the_action_and_by_membership`.
    {
        let chrome = chrome_with_member("bob");
        chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .unwrap()
            .set_owner("foreign-owner");
        let before = capabilities(&chrome);
        assert!(matches!(
            invoke_shell_action(
                &chrome,
                MEMBER_REMOVE_ID,
                &serde_json::json!({"participant_id": "bob", "confirm": true}),
            ),
            Err(ActionError::Refused(_))
        ));
        assert_eq!(
            crate::shell::membership::remove_member(&mut chrome.borrow_mut(), &"bob".to_string()),
            Err(crate::shell::membership::MembershipError::Unauthorized(
                MEMBER_REMOVE_ID
            )),
            "the Members tab's own entry point refuses a non-administrator too"
        );
        assert_eq!(
            crate::shell::membership::set_member_role(
                &mut chrome.borrow_mut(),
                &"bob".to_string(),
                crate::shell::state::CapRole::Viewer,
            ),
            Err(crate::shell::membership::MembershipError::Unauthorized(
                MEMBER_ROLE_SET_ID
            ))
        );
        assert_eq!(capabilities(&chrome), before);
    }
}

/// The Agents tab's Remove is an action too: it revokes every grant
/// naming the agent, membership included, and refuses anyone who does
/// not administer the room, through the action and the tab alike.
#[test]
fn agent_remove_action_revokes_everything_the_agent_holds() {
    let apply = |chrome: &Chrome, id: &str, params: &[ActionParam], name: &str| {
        let action = Action::ALL
            .iter()
            .find(|action| action.id == id)
            .expect("registered");
        let mut values = vec![(
            "name".to_string(),
            crate::shell::search::action_api::ArgValue::Text(name.to_string()),
        )];
        if params.iter().any(|param| param.name == "confirm") {
            values.push((
                "confirm".to_string(),
                crate::shell::search::action_api::ArgValue::Boolean(true),
            ));
        }
        let args = ActionArgs::bind(params, values).expect("declared parameters");
        (action.apply)(&mut chrome.borrow_mut(), &args)
    };
    let grants_of = |chrome: &Chrome, agent: &str| {
        let subject = tilo_action_policy::Subject::agent(agent);
        chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture has a room")
            .policy
            .grants
            .iter()
            .filter(|grant| grant.subject == subject)
            .count()
    };

    // Phase `the_owner_removes_an_admitted_agent`.
    {
        let chrome = chrome_with_room();
        apply(&chrome, "agent.invite", AGENT_INVITE_PARAMS, "Scribe").expect("owner admits");
        governed(&chrome, FIXTURE_USER);
        assert!(grants_of(&chrome, "scribe") > 0, "admitted");
        apply(&chrome, "agent.remove", AGENT_REMOVE_PARAMS, "  SCRIBE ")
            .expect("the owner may remove the agent, named in any case");
        governed(&chrome, FIXTURE_USER);
        assert_eq!(
            grants_of(&chrome, "scribe"),
            0,
            "no grant names it any more"
        );
        assert!(
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .unwrap()
                .agent_members_at(tilo_clock::unix_now())
                .is_empty(),
            "the Agents tab no longer lists it"
        );
        assert!(
            matches!(
                apply(&chrome, "agent.remove", AGENT_REMOVE_PARAMS, "scribe"),
                Err(ActionError::Refused(_))
            ),
            "removing an agent that holds nothing says so"
        );
    }

    // Phase `a_non_administrator_is_refused_by_the_action_and_by_the_tab`.
    {
        let chrome = chrome_with_room();
        apply(&chrome, "agent.invite", AGENT_INVITE_PARAMS, "scribe").expect("owner admits");
        governed(&chrome, FIXTURE_USER);
        chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .unwrap()
            .set_owner("foreign-owner");
        let before = capabilities(&chrome);
        assert!(matches!(
            apply(&chrome, "agent.remove", AGENT_REMOVE_PARAMS, "scribe"),
            Err(ActionError::Refused(_))
        ));
        assert_eq!(
            crate::shell::membership::remove_agent(&mut chrome.borrow_mut(), "scribe"),
            Err(crate::shell::membership::MembershipError::Unauthorized(
                crate::shell::membership::AGENT_REMOVE_ACTION
            )),
            "the Agents tab's own entry point refuses a non-administrator too"
        );
        assert_eq!(capabilities(&chrome), before);
    }

    let contract = shell_action_contract("agent.remove");
    assert_eq!(contract.risk, tilo_actions::RiskLevel::Destructive);
    assert_eq!(
        contract.confirmation,
        tilo_actions::ConfirmationPolicy::Confirm
    );
}

#[test]
fn invitation_params_follow_the_canonical_definitions() {
    let definitions = tilo_action_broker::collaboration_admin::action_definitions();
    for (id, canonical) in [
        (MEMBER_ROLE_SET_ID, MEMBER_ROLE_SET_ID),
        (MEMBER_REMOVE_ID, MEMBER_REMOVE_ID),
        (INVITATION_ISSUE_ID, INVITATION_ISSUE_ID),
        (INVITATION_CANCEL_ID, INVITATION_CANCEL_ID),
        (INVITATION_LIST_ID, INVITATION_LIST_ID),
        (INVITATION_RESEND_ID, INVITATION_CANCEL_ID),
    ] {
        let action = Action::ALL
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("{id} is published"));
        let definition = definitions
            .iter()
            .find(|definition| definition.id == canonical)
            .unwrap_or_else(|| panic!("{canonical} is canonical"));
        let shared: Vec<_> = action
            .params
            .iter()
            .filter(|param| !membership_actions::SHELL_ONLY_PARAMS.contains(&&*param.name))
            .cloned()
            .collect();
        assert_eq!(
            shared, definition.params,
            "{id} declares the canonical parameters"
        );
        assert!(
            action.params.iter().any(|param| param.name == "confirm"),
            "{id} carries the confirmation its contract demands"
        );
        let contract = shell_action_contract(id);
        assert_eq!(
            contract.kind,
            tilo_actions::ActionKind::Outside,
            "{id} is administration"
        );
        if id != INVITATION_RESEND_ID {
            assert_eq!(contract.risk, definition.contract.risk);
        }
        assert_eq!(contract.result, definition.contract.result);
        assert_eq!(
            contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert!(
            contract.validate_declaration().is_empty(),
            "{id}: {:?}",
            contract.validate_declaration()
        );
        assert_eq!(shell_action_kind(id), tilo_actions::OperationKind::Effect);
    }
    assert!(
        !membership_actions::INVITATION_ACTION_IDS.contains(&MEMBER_ROLE_SET_ID)
            && !membership_actions::INVITATION_ACTION_IDS.contains(&MEMBER_REMOVE_ID)
    );
}

#[test]
fn membership_actions_are_published_and_invitations_need_peer_sync() {
    let provider = ActionsProvider::new();
    let chrome = chrome_with_room();
    let rows = rows_for(&provider, &chrome, "");
    assert!(row(&rows, MEMBER_ROLE_SET_ID).is_some());
    assert!(row(&rows, MEMBER_REMOVE_ID).is_some());
    for id in membership_actions::INVITATION_ACTION_IDS {
        assert!(row(&rows, id).is_none(), "{id} needs peer sync");
    }
    chrome.borrow_mut().peer_sync_enabled = true;
    let rows = rows_for(&provider, &chrome, "");
    for id in membership_actions::INVITATION_ACTION_IDS {
        assert!(row(&rows, id).is_some(), "{id} is published with peer sync");
    }
}

#[test]
fn invitation_requests_are_validated_before_anything_is_queued() {
    use crate::shell::overlay_host::{InvitationTerms, PeerSyncRequest};
    let bind = |id: &str, args: serde_json::Value| {
        let action = Action::ALL.iter().find(|action| action.id == id).unwrap();
        let args = ActionArgs::bind_json(action.params, args.as_object().unwrap()).unwrap();
        invitation_request(id, &args)
    };
    let recipient = "ab".repeat(32);
    assert_eq!(
        bind(
            INVITATION_ISSUE_ID,
            serde_json::json!({
                "role": "commenter",
                "expires_in_seconds": 600,
                "invitee_name": "  Bob ",
                "recipient": format!("tilo-identity:{recipient}"),
                "confirm": true,
            }),
        )
        .unwrap(),
        PeerSyncRequest::IssueInvitation(InvitationTerms {
            role: crate::network::invite::InviteRole::Commenter,
            expires_in_seconds: 600,
            invitee_name: Some("Bob".into()),
            recipient_user_public_key: Some([0xab; 32]),
        })
    );
    for refused in [
        serde_json::json!({"role": "editor", "expires_in_seconds": 0, "confirm": true}),
        serde_json::json!({
            "role": "editor",
            "expires_in_seconds": membership_actions::MAX_INVITATION_LIFETIME_SECONDS.saturating_add(1),
            "confirm": true,
        }),
        serde_json::json!({"role": "owner", "expires_in_seconds": 60, "confirm": true}),
        serde_json::json!({
            "role": "editor",
            "expires_in_seconds": 60,
            "recipient": "not-a-key",
            "confirm": true,
        }),
    ] {
        assert!(
            bind(INVITATION_ISSUE_ID, refused.clone()).is_err(),
            "{refused} is refused"
        );
    }
    let id = "07".repeat(32);
    assert_eq!(
        bind(
            INVITATION_CANCEL_ID,
            serde_json::json!({"invite_id": id, "confirm": true})
        )
        .unwrap(),
        PeerSyncRequest::CancelInvitation([7; 32])
    );
    assert_eq!(
        bind(
            INVITATION_RESEND_ID,
            serde_json::json!({"invite_id": id, "confirm": true})
        )
        .unwrap(),
        PeerSyncRequest::ResendInvitation([7; 32])
    );
    assert!(
        bind(
            INVITATION_CANCEL_ID,
            serde_json::json!({"invite_id": "07", "confirm": true})
        )
        .is_err()
    );
    assert_eq!(
        bind(INVITATION_LIST_ID, serde_json::json!({"confirm": true})).unwrap(),
        PeerSyncRequest::ListPendingInvitations
    );
}
