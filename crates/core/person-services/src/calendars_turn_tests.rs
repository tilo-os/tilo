//! A workspace's calendar accounts and a turn this server carries out for a member: the turn is
//! offered the sign-in on a provider's own page, the finding of an account's calendars and the
//! choice of which show; the choice reaches the account through the same path Settings takes, and
//! no secret is in the call or the answer. A turn the workspace's policy does not let reach its
//! calendars, and a caller that is no turn, reach none of it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use serde_json::{Value, json};
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_workspace_records::accounts as workspace_accounts;

use crate::fixtures::asking;
use crate::fixtures::{hosting_device, test_host as test_server};
use crate::hosted_use::HostedTurn;
use crate::hosted_use::turn_use_tests::{dispatched, narrowed_elsewhere, narrowing, on_turn};
use tilo_server_protocol::WorkspaceId;

const ACCOUNT: &str = "caldav:ada@calendar.example.test";
const SCOUT: &str = "scout";

/// Keep the calendar account Settings keeps for a person whose server holds two calendars, as
/// the bridge reads it: one shows and one is left out.
async fn keep_the_calendar(workspaces: &crate::PersonServices, workspace: &str) {
    let mut account = accounts::Account::new(
        accounts::AccountId::from(ACCOUNT),
        accounts::calendar::TYPE_ID,
    );
    account.display_name = "Work".into();
    account.config = json!({
        "display_name": "Work",
        "server_url": "https://calendar.example.test/dav/",
        "username": "ada@example.test",
        "calendars": [
            {"id": "work", "name": "Work", "color": 0, "visible": true, "read_only": false},
            {"id": "home", "name": "Home", "color": 1, "visible": false, "read_only": false},
        ],
        "collapsed": false,
    });
    workspaces
        .keep_workspace_account((workspace, &workspaces.acting_or_own(None), None), &account)
        .await
        .expect("the calendar account is kept");
}

/// What the turn `caller` works in `workspace_id`, with `policy` narrowing its agent.
const fn turn<'a>(
    workspace_id: WorkspaceId,
    caller: &'a Caller,
    policy: Option<&'a tilo_action_policy::WorkspacePolicy>,
) -> HostedTurn<'a> {
    HostedTurn {
        workspace: workspace_id,
        caller,
        policy,
    }
}

/// The calendar actions `turn` is offered, by their ids.
async fn offered(workspaces: &crate::PersonServices, turn: &HostedTurn<'_>) -> Vec<String> {
    workspaces
        .calendars_offered(turn)
        .await
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

/// What a turn is answered when it asks `action` with `argument`.
async fn asks(
    workspaces: &crate::PersonServices,
    turn: &HostedTurn<'_>,
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    dispatched(workspaces, turn, &asking(action, argument)).await
}

/// The calendars the account keeps on show, by id, once `result` answered.
async fn on_show(workspaces: &crate::PersonServices, workspace: &str) -> Vec<String> {
    let held = workspaces.workspace_accounts(workspace).await.unwrap();
    let config = &held.account(ACCOUNT).expect("the account is kept").config;
    config["calendars"]
        .as_array()
        .expect("its calendars are kept")
        .iter()
        .filter(|calendar| calendar["visible"] == true)
        .filter_map(|calendar| calendar["id"].as_str().map(str::to_owned))
        .collect()
}

/// The turn of a member its workspace's policy lets reach its calendars is offered the sign-in,
/// the finding of an account's calendars and the choice of which show; the choice is kept on the
/// account, and it carries no sign-in. An agent the policy narrows is offered none and answered
/// none, and a caller that is no turn reaches nothing.
#[tokio::test]
async fn a_members_turn_chooses_the_calendars_their_account_reads() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_server(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, _service) = crate::fixtures::hosted(workspaces).await;
    let ada = crate::fixtures::admit(workspaces, &workspace, &owner, 1).await;
    keep_the_calendar(workspaces, &workspace).await;

    let scout = on_turn(&ada, &workspace, SCOUT);
    let asking_turn = turn(workspace_id, &scout, None);
    assert_eq!(
        offered(workspaces, &asking_turn).await,
        [
            workspace_accounts::BEGIN_SIGN_IN,
            workspace_accounts::DISCOVER_CALENDARS,
            workspace_accounts::SET_CALENDARS,
        ]
    );

    // The turn chooses the other calendar to show, and it is kept on the account the bridge reads.
    let asked = json!({"account": ACCOUNT, "calendars": ["home"]});
    let answered = asks(
        workspaces,
        &asking_turn,
        workspace_accounts::SET_CALENDARS,
        asked.clone(),
    )
    .await
    .expect("the turn chooses the calendars");
    assert!(
        answered.is_null() || answered.get("accounts").is_some(),
        "the choice is answered: {answered}"
    );
    assert_eq!(on_show(workspaces, &workspace).await, ["home"]);
    assert!(
        !asked.to_string().to_lowercase().contains("password"),
        "the call names no sign-in"
    );

    // Asking its server for its calendars is answered by the calendar path: the account keeps no
    // sign-in here, which is what it is refused for, and not for being an action nobody knows.
    let refused = asks(
        workspaces,
        &asking_turn,
        workspace_accounts::DISCOVER_CALENDARS,
        json!({"account": ACCOUNT}),
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.to_lowercase().contains("sign")),
        "{refused:?}"
    );

    // An agent the workspace's answers narrow to another app reaches none of it.
    let narrowed = narrowing(&workspace, &scout, &narrowed_elsewhere(&workspace));
    let narrowed_turn = turn(workspace_id, &scout, Some(&narrowed));
    assert!(offered(workspaces, &narrowed_turn).await.is_empty());
    assert!(matches!(
        asks(
            workspaces,
            &narrowed_turn,
            workspace_accounts::SET_CALENDARS,
            asked
        )
        .await,
        Err(ActionCallError::NotPermitted | ActionCallError::UnknownAction)
    ));

    // A caller that is no turn this server carries out is answered nothing. Ada is a member of
    // somebody else's workspace asking this device, which is not hers: she is refused for what
    // is not hers to ask, rather than told the calendars are not there.
    let herself =
        Caller::authenticated(ada.user_public_key, ada.device_public_key).in_workspace(&workspace);
    let no_turn = turn(workspace_id, &herself, None);
    assert!(offered(workspaces, &no_turn).await.is_empty());
    assert!(matches!(
        asks(
            workspaces,
            &no_turn,
            workspace_accounts::SET_CALENDARS,
            json!({"account": ACCOUNT, "calendars": ["work"]})
        )
        .await,
        Err(ActionCallError::NotPermitted)
    ));
}

/// Only a turn of a member's own agent is offered the connecting of a calendar account: another
/// member's agent's turn connects for the workspace too, and every caller that is no turn this
/// server carries out for a member (the person themselves, an agent outside a turn, an agent
/// admitted as a member, a bot, an app, a turn confined elsewhere, an agent a person's own device
/// hands its session to) is offered none of it.
#[tokio::test]
async fn only_a_turn_of_a_members_own_agent_is_offered_the_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_server(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, _service) = crate::fixtures::hosted(workspaces).await;
    let ada = crate::fixtures::admit(workspaces, &workspace, &owner, 1).await;
    let bo = crate::fixtures::admit(workspaces, &workspace, &owner, 2).await;

    let bos_caller = on_turn(&bo, &workspace, SCOUT);
    let bos_turn = turn(workspace_id, &bos_caller, None);
    assert!(
        offered(workspaces, &bos_turn)
            .await
            .contains(&workspace_accounts::BEGIN_SIGN_IN.to_owned()),
        "another member's agent connects the workspace's calendars"
    );
    for (who, caller) in crate::hosted_use::turn_use_tests::not_hosted_turns(&ada, &workspace) {
        let not_a_turn = turn(workspace_id, &caller, None);
        assert!(
            offered(workspaces, &not_a_turn).await.is_empty(),
            "{who} is offered no connecting"
        );
    }

    // A turn connects an account for its calendars alone: the person's mail and files are not
    // reached with the calendars' grant.
    let scout = on_turn(&ada, &workspace, SCOUT);
    let hers = turn(workspace_id, &scout, None);
    let with_mail = json!({"provider": "google", "calendar": true, "mail": true});
    assert!(matches!(
        asks(
            workspaces,
            &hers,
            workspace_accounts::BEGIN_SIGN_IN,
            with_mail
        )
        .await,
        Err(ActionCallError::UnknownAction)
    ));
}
