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
use tilo_actions::remote::person_accounts;
use tilo_actions::remote::{ActionCallError, Caller};

use super::accounts_tests::asking;
use super::turn_use_tests::{dispatched, narrowed_elsewhere, narrowing, on_turn};
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::tests::{governor, test_host as test_server};
use crate::workspaces::{WorkspaceId, Workspaces};

const ACCOUNT: &str = "caldav:ada@calendar.example.test";
const SCOUT: &str = "scout";

/// Keep the calendar account Settings keeps for a person whose server holds two calendars, as
/// the bridge reads it: one shows and one is left out.
async fn keep_the_calendar(workspaces: &Workspaces, workspace: &str) {
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
async fn offered(workspaces: &Workspaces, turn: &HostedTurn<'_>) -> Vec<String> {
    workspaces
        .calendars_offered(turn)
        .await
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

/// What a turn is answered when it asks `action` with `argument`.
async fn asks(
    workspaces: &Workspaces,
    turn: &HostedTurn<'_>,
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    dispatched(workspaces, turn, &asking(action, argument)).await
}

/// The calendars the account keeps on show, by id, once `result` answered.
async fn on_show(workspaces: &Workspaces, workspace: &str) -> Vec<String> {
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
    let (key, owner) = governor();
    let server = test_server(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let (workspace, workspace_id, _service) =
        crate::workspaces::actions::tests::governed(&workspaces).await;
    let ada = crate::workspaces::actions::tests::admit(&workspaces, &workspace, &owner, 1).await;
    keep_the_calendar(&workspaces, &workspace).await;

    let scout = on_turn(&ada, &workspace, SCOUT);
    let asking_turn = turn(workspace_id, &scout, None);
    assert_eq!(
        offered(&workspaces, &asking_turn).await,
        [
            person_accounts::BEGIN_SIGN_IN,
            person_accounts::DISCOVER_CALENDARS,
            person_accounts::SET_CALENDARS,
        ]
    );

    // The turn chooses the other calendar to show, and it is kept on the account the bridge reads.
    let asked = json!({"account": ACCOUNT, "calendars": ["home"]});
    let answered = asks(
        &workspaces,
        &asking_turn,
        person_accounts::SET_CALENDARS,
        asked.clone(),
    )
    .await
    .expect("the turn chooses the calendars");
    assert!(
        answered.is_null() || answered.get("accounts").is_some(),
        "the choice is answered: {answered}"
    );
    assert_eq!(on_show(&workspaces, &workspace).await, ["home"]);
    assert!(
        !asked.to_string().to_lowercase().contains("password"),
        "the call names no sign-in"
    );

    // Asking its server for its calendars is answered by the calendar path: the account keeps no
    // sign-in here, which is what it is refused for, and not for being an action nobody knows.
    let refused = asks(
        &workspaces,
        &asking_turn,
        person_accounts::DISCOVER_CALENDARS,
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
    assert!(offered(&workspaces, &narrowed_turn).await.is_empty());
    assert!(matches!(
        asks(
            &workspaces,
            &narrowed_turn,
            person_accounts::SET_CALENDARS,
            asked
        )
        .await,
        Err(ActionCallError::NotPermitted | ActionCallError::UnknownAction)
    ));

    // A caller that is no turn this server carries out is answered nothing.
    let herself =
        Caller::authenticated(ada.user_public_key, ada.device_public_key).in_workspace(&workspace);
    let no_turn = turn(workspace_id, &herself, None);
    assert!(offered(&workspaces, &no_turn).await.is_empty());
    assert!(matches!(
        asks(
            &workspaces,
            &no_turn,
            person_accounts::SET_CALENDARS,
            json!({"account": ACCOUNT, "calendars": ["work"]})
        )
        .await,
        Err(ActionCallError::UnknownAction)
    ));
}
