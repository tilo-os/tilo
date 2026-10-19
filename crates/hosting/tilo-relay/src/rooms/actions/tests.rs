//! A governed room's administration as `tilo.workspace` actions: offered as
//! far as the room's governance lets the caller, run through the same room
//! methods as the wire doors, and destructive governance held back until the
//! person's own confirmation can be asked for.

use ed25519_dalek::Signer as _;
use serde_json::{Map, Value, json};
use tilo_action_policy::{Answer, Grant, KindSet, Subject, roles, roles::governance};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CatalogueQuery, Confirmation, InvokeArgument,
    InvokeRequest, Pending, ROOM_GOVERNANCE_PROVIDER, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::rooms::{JoinReply, JoinStep, join_proof_digest};
use tilo_user_identity::DeviceCert;

use super::{
    ADMIT, CANCEL_INVITATION, DECLINE, GovernedPlane, HostRoomActions, INVITATIONS, JOIN_REQUESTS,
};
use crate::app_serving::AppServingPlane as _;
use crate::rooms::tests::{governor, guest_device, signed_pre_admission, view_read_relay};
use crate::rooms::{PreAdmitRequest, Rooms, SetMemberRoleRequest};

/// What a pending answer comes to, on this runtime.
pub(super) async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the governance answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// A new person admitted to `room` as an Editor, through the owner's
/// pre-admission: their device's certificate.
pub(super) async fn admit(rooms: &Rooms, room: &str, owner: &DeviceCert, seed: u8) -> DeviceCert {
    let token = [seed; 8];
    let issued = rooms
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(room, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let (key, cert) = guest_device();
    let JoinReply::Challenge { challenge } = rooms
        .page_join(JoinStep::Ask {
            invitation: issued.signed_invitation,
            device_cert: cert.clone(),
            request_id: rand::random(),
        })
        .await
    else {
        panic!("no challenge");
    };
    let signature = key.sign(&join_proof_digest(&challenge)).to_bytes();
    let reply = rooms
        .page_join(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: format!("Person {seed}"),
            token: token.to_vec(),
        })
        .await;
    assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    cert
}

/// The owner grants `person` the governance action `action` of `room`.
async fn grant(rooms: &Rooms, room: &str, owner: &DeviceCert, person: &DeviceCert, action: &str) {
    let who = hex::encode(person.user_public_key);
    let grant = Grant::new(
        format!("test:{action}:{who}"),
        Subject::Person(who),
        roles::governance_action(room, action),
        KindSet::ALL,
        Answer::Allow,
        Subject::Person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(grant.id.as_str().as_bytes()).as_bytes();
    rooms
        .put_room_grants(room, owner, request_id, vec![grant])
        .await
        .unwrap();
}

pub(super) fn caller(device: &DeviceCert) -> Caller {
    Caller::authenticated(device.user_public_key, device.device_public_key)
}

pub(super) fn request(room: &str, verb: &str, argument: Value) -> InvokeRequest {
    let Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::room_governance(room, verb),
        argument: if argument.is_empty() {
            InvokeArgument::None
        } else {
            InvokeArgument::Json(argument)
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// A new room of `rooms`, the workspace carrying it, and its governance
/// as actions.
pub(super) async fn governed(rooms: &Rooms) -> (String, u64, HostRoomActions) {
    let summary = rooms.create("Team").await.unwrap();
    let host = HostRoomActions::new();
    host.attach(rooms.clone());
    (summary.room, summary.workspace_id, host)
}

/// The verbs `caller` is offered on the room `workspace_id` carries.
async fn offered(host: &HostRoomActions, workspace_id: u64, caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(ROOM_GOVERNANCE_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    let page = settled(host.in_workspace(workspace_id).query(caller, query))
        .await
        .unwrap();
    page.rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

/// `request` by `caller`: what it produced, or why not.
async fn invoke(
    host: &HostRoomActions,
    workspace_id: u64,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(host.in_workspace(workspace_id).invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes at once: {other:?}"),
    }
}

const INVITE_FAMILY: [&str; 6] = [
    governance::INVITE,
    INVITATIONS,
    CANCEL_INVITATION,
    JOIN_REQUESTS,
    ADMIT,
    DECLINE,
];

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn the_catalogue_offers_what_the_rooms_governance_lets_each_caller_do() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;

    // The relay's person runs every action the provider serves, on the
    // room's own governance resource, and none it holds back.
    let owners = offered(&host, workspace, &caller(&owner)).await;
    for verb in INVITE_FAMILY.iter().chain(&[
        governance::SET_ROLE,
        governance::RESTRICT,
        governance::UNRESTRICT,
        governance::UNBAN,
        governance::CALL,
    ]) {
        assert!(
            owners.iter().any(|offered| offered == verb),
            "{verb}: {owners:?}"
        );
    }
    assert!(
        !owners
            .iter()
            .any(|verb| verb == governance::BAN || verb == governance::REMOVE_MEMBER)
    );

    // An Editor by role alone is offered nothing.
    assert!(offered(&host, workspace, &caller(&ada)).await.is_empty());

    // Granted invite, Ada is offered inviting and nothing beyond it.
    grant(rooms, &room, &owner, &ada, governance::INVITE).await;
    let granted = offered(&host, workspace, &caller(&ada)).await;
    assert_eq!(granted, INVITE_FAMILY.map(str::to_owned).to_vec());

    // A Moderator is offered the moderation of members below them, but no
    // roles to hand out and no invitations.
    rooms
        .set_member_role(
            &room,
            &owner,
            SetMemberRoleRequest {
                request_id: [30; 32],
                user_public_key: bo.user_public_key,
                role: MemberRole::Moderator,
            },
        )
        .await
        .unwrap();
    let moderator = offered(&host, workspace, &caller(&bo)).await;
    assert_eq!(
        moderator,
        [
            governance::RESTRICT,
            governance::UNRESTRICT,
            governance::UNBAN
        ]
        .map(str::to_owned)
    );

    // An agent governs nothing, even on the relay person's own key, and a
    // workspace that carries no governed room offers nothing.
    let agent = Caller::delegated_agent("writer", owner.user_public_key, owner.device_public_key);
    assert!(offered(&host, workspace, &agent).await.is_empty());
    assert!(
        offered(&host, workspace ^ 1, &caller(&owner))
            .await
            .is_empty()
    );
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn an_invitation_lasts_the_seconds_the_desktop_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let (room, workspace, host) = governed(rooms).await;

    // The desktop's invitation: seconds take precedence over the hours an
    // older relay would read, for exactly the person and name it asked.
    let guest = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
        .verifying_key()
        .to_bytes();
    let before = tilo_clock::unix_now_ns();
    let timed = invoke(
        &host,
        workspace,
        &caller(&owner),
        request(
            &room,
            governance::INVITE,
            json!({
                "role": "viewer",
                "recipient": hex::encode(guest),
                "invitee_name": "Ada",
                "expires_in_seconds": 600,
                "expires_in_hours": 1,
            }),
        ),
    )
    .await
    .unwrap();
    let after = tilo_clock::unix_now_ns();
    let timed_id = timed["invite"].as_str().unwrap();
    assert!(!timed["invitation"].as_str().unwrap().is_empty());
    let timed = rooms
        .pending(&room, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|pending| hex::encode(pending.invite_id) == timed_id)
        .expect("the timed invitation is pending");
    assert_eq!(timed.role, MemberRole::Viewer);
    assert_eq!(timed.recipient_user_public_key, Some(guest));
    assert_eq!(timed.invitee_name.as_deref(), Some("Ada"));
    let lifetime: u64 = 600_000_000_000;
    assert!(
        (before.saturating_add(lifetime)..=after.saturating_add(lifetime))
            .contains(&timed.expires_at_ns),
        "{timed:?}"
    );
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn invitations_run_through_the_rooms_own_methods() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;

    // The owner invites: the room records a pending invitation.
    let invite = request(&room, governance::INVITE, json!({ "role": "editor" }));
    let issued = invoke(&host, workspace, &caller(&owner), invite.clone())
        .await
        .unwrap();
    let invite_id = issued["invite"].as_str().unwrap().to_owned();
    let pending = rooms.pending(&room, &owner).await.unwrap();
    assert!(
        pending
            .iter()
            .any(|pending| hex::encode(pending.invite_id) == invite_id),
        "{pending:?}"
    );
    // A retry of the same invocation is the same invitation.
    let again = invoke(&host, workspace, &caller(&owner), invite)
        .await
        .unwrap();
    assert_eq!(again, issued);
    assert_eq!(
        rooms.pending(&room, &owner).await.unwrap().len(),
        pending.len()
    );
    let listed = invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, INVITATIONS, json!({})),
    )
    .await
    .unwrap();
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|invitation| invitation["invite"] == invite_id.as_str())
    );

    // An Editor may not invite: the action does not exist for them.
    let refused = invoke(
        &host,
        workspace,
        &caller(&ada),
        request(&room, governance::INVITE, json!({ "role": "viewer" })),
    )
    .await;
    assert_eq!(refused, Err(ActionHostError::UnknownAction));

    // Granted invite, Ada invites up to her own role and no further: the
    // room's own decision, refused with its reason.
    grant(rooms, &room, &owner, &ada, governance::INVITE).await;
    invoke(
        &host,
        workspace,
        &caller(&ada),
        request(&room, governance::INVITE, json!({ "role": "editor" })),
    )
    .await
    .unwrap();
    let above = invoke(
        &host,
        workspace,
        &caller(&ada),
        request(&room, governance::INVITE, json!({ "role": "moderator" })),
    )
    .await;
    assert!(
        matches!(&above, Err(ActionHostError::Refused { reason }) if reason.contains("Moderator")),
        "{above:?}"
    );

    // Cancelling takes it off the pending list.
    let mut cancel = Map::new();
    cancel.insert("invite".into(), invite_id.clone().into());
    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, CANCEL_INVITATION, Value::Object(cancel)),
    )
    .await
    .unwrap();
    assert!(
        !rooms
            .pending(&room, &owner)
            .await
            .unwrap()
            .iter()
            .any(|pending| hex::encode(pending.invite_id) == invite_id)
    );

    // A resource naming another room is not this room's action.
    let elsewhere = invoke(
        &host,
        workspace,
        &caller(&owner),
        request(
            "another-room",
            governance::INVITE,
            json!({ "role": "viewer" }),
        ),
    )
    .await;
    assert_eq!(elsewhere, Err(ActionHostError::UnknownAction));
    relay.shutdown().await;
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn roles_follow_the_room_and_destructive_governance_waits_for_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let bo = admit(rooms, &room, &owner, 2).await;
    let person = |cert: &DeviceCert| hex::encode(cert.user_public_key);

    // The owner makes Bo a Moderator through the action.
    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(
            &room,
            governance::SET_ROLE,
            json!({ "person": person(&bo), "role": "moderator" }),
        ),
    )
    .await
    .unwrap();
    let members = rooms.summary(&room).await.unwrap().members;
    assert!(
        members
            .iter()
            .any(|member| member.user_public_key == bo.user_public_key
                && member.role == MemberRole::Moderator),
        "{members:?}"
    );

    // Bo restricts Ada, an Editor, and lifts it again.
    let restricted = invoke(
        &host,
        workspace,
        &caller(&bo),
        request(
            &room,
            governance::RESTRICT,
            json!({ "person": person(&ada), "minutes": 5 }),
        ),
    )
    .await
    .unwrap();
    assert!(restricted["generation"].as_u64().is_some(), "{restricted}");
    invoke(
        &host,
        workspace,
        &caller(&bo),
        request(
            &room,
            governance::UNRESTRICT,
            json!({ "person": person(&ada) }),
        ),
    )
    .await
    .unwrap();

    // Banning would remove Ada: it waits for Bo's own confirmation, which
    // a caller's word is not, and nothing changes.
    let mut ban = request(&room, governance::BAN, json!({ "person": person(&ada) }));
    ban.confirmation = Some(Confirmation::Confirmed);
    assert_eq!(
        invoke(&host, workspace, &caller(&bo), ban).await,
        Err(ActionHostError::ConfirmationRequired)
    );
    let remove = request(
        &room,
        governance::REMOVE_MEMBER,
        json!({ "person": person(&ada) }),
    );
    assert_eq!(
        invoke(&host, workspace, &caller(&bo), remove).await,
        Err(ActionHostError::ConfirmationRequired)
    );
    let end = request(
        &room,
        governance::CALL,
        json!({ "operation": "end", "call": hex::encode([9; 32]), "epoch": 1 }),
    );
    assert_eq!(
        invoke(&host, workspace, &caller(&owner), end).await,
        Err(ActionHostError::ConfirmationRequired)
    );
    let members = rooms.summary(&room).await.unwrap().members;
    assert!(
        members
            .iter()
            .any(|member| member.user_public_key == ada.user_public_key && !member.removed),
        "{members:?}"
    );

    // An Editor is not told a ban exists: unknown and forbidden answer alike.
    let ban = request(&room, governance::BAN, json!({ "person": person(&bo) }));
    assert_eq!(
        invoke(&host, workspace, &caller(&ada), ban).await,
        Err(ActionHostError::UnknownAction)
    );

    // A Moderator hands out no roles: set-role is not theirs to see.
    let promote = request(
        &room,
        governance::SET_ROLE,
        json!({ "person": person(&ada), "role": "viewer" }),
    );
    assert_eq!(
        invoke(&host, workspace, &caller(&bo), promote).await,
        Err(ActionHostError::UnknownAction)
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn a_host_without_apps_answers_only_its_rooms_governance() {
    let plane = GovernedPlane {
        governance: HostRoomActions::new(),
        apps: None,
    };
    let someone = Caller::authenticated([1; 32], [2; 32]);
    let governance = RemoteActionMessage::Invoke(request("room", governance::INVITE, json!({})));
    let mut app = request("room", "archive", json!({}));
    app.target.provider = "app".into();
    assert!(plane.answers(&someone, &governance));
    assert!(!plane.answers(&someone, &RemoteActionMessage::Invoke(app)));
    // Nothing is attached: the room's actions are unknown, never run.
    let Some(answer) = plane.serve(7, &someone, governance) else {
        panic!("an invocation is answered");
    };
    drop(plane);
    let RemoteActionMessage::Receipt(receipt) = settled(answer).await else {
        panic!("a receipt");
    };
    assert_eq!(
        receipt.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::UnknownAction
        }
    );
}

#[tokio::test]
#[cfg_attr(
    feature = "agents",
    expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after the assertions"
    )
)]
async fn a_member_who_may_install_bots_asks_the_rooms_host_for_one_and_takes_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = view_read_relay(dir.path(), &key, &owner).await;
    let rooms = relay.rooms().unwrap();
    let (room, workspace, host) = governed(rooms).await;
    let ada = admit(rooms, &room, &owner, 1).await;
    let standup = json!({ "entry": "so.tilo.bots.standup", "version": "1.2.0" });

    // An Editor by role alone is neither offered a bot nor runs the request.
    assert!(
        !offered(&host, workspace, &caller(&ada))
            .await
            .iter()
            .any(|verb| verb == super::REQUEST_BOT)
    );
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &caller(&ada),
            request(&room, super::REQUEST_BOT, standup.clone()),
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));
    assert!(rooms.bot_requests(&room).await.unwrap().is_empty());

    // Let install bots, Ada asks the room's host for one: every member reads
    // the request in the room's governance, and the host follows it.
    grant(rooms, &room, &owner, &ada, governance::INSTALL_BOT).await;
    assert!(
        offered(&host, workspace, &caller(&ada))
            .await
            .iter()
            .any(|verb| verb == super::REQUEST_BOT)
    );
    invoke(
        &host,
        workspace,
        &caller(&ada),
        request(&room, super::REQUEST_BOT, standup),
    )
    .await
    .unwrap();
    let requests = rooms.bot_requests(&room).await.unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].request.entry, "so.tilo.bots.standup");
    assert_eq!(requests[0].request.version, "1.2.0");
    assert_eq!(requests[0].requested_by, hex::encode(ada.user_public_key));

    // Taking it back needs uninstalling bots, which the owner has.
    let withdraw = json!({ "entry": "so.tilo.bots.standup" });
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &caller(&ada),
            request(&room, super::WITHDRAW_BOT_REQUEST, withdraw.clone()),
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));
    invoke(
        &host,
        workspace,
        &caller(&owner),
        request(&room, super::WITHDRAW_BOT_REQUEST, withdraw),
    )
    .await
    .unwrap();
    assert!(rooms.bot_requests(&room).await.unwrap().is_empty());

    // A request names a directory entry and a version, nothing else.
    assert!(matches!(
        invoke(
            &host,
            workspace,
            &caller(&owner),
            request(
                &room,
                super::REQUEST_BOT,
                json!({ "entry": "../standup", "version": "1.2.0" })
            ),
        )
        .await,
        Err(ActionHostError::InvalidRequest { .. })
    ));
    relay.shutdown().await;
}

/// A member gives the host only a secret a connector is known to take, and
/// a request's inputs are text settings by name.
#[test]
fn connector_secrets_are_the_known_ones_and_request_inputs_are_text() {
    let args = |value: Value| {
        super::Args::of(&InvokeArgument::Json(value.as_object().unwrap().clone())).unwrap()
    };
    for wrong in [
        json!({ "app": "so.tilo.mail", "purpose": "bot-token", "secret": "x" }),
        json!({ "app": "so.tilo.discord", "purpose": "password", "secret": "x" }),
        json!({ "app": "so.tilo.discord", "purpose": "bot-token", "secret": "  " }),
    ] {
        assert!(
            super::connector_secret(&args(wrong.clone())).is_err(),
            "{wrong}"
        );
    }
    let inputs = args(json!({ "inputs": { "guild": "123", "server": "Garden club" } }))
        .text_map("inputs")
        .unwrap();
    assert_eq!(inputs.get("guild").map(String::as_str), Some("123"));
    assert!(
        args(json!({ "inputs": { "guild": 123 } }))
            .text_map("inputs")
            .is_err()
    );
    assert!(args(json!({})).text_map("inputs").unwrap().is_empty());
}
