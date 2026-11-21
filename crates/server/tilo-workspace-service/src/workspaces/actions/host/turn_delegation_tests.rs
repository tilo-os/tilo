//! A member delegates their own work to this server: a turn the server carries
//! out for their agent acts on the server's own device — the device their work
//! is admitted on then — and nothing else is one it acts on. The delegation is
//! their own device's, so their device takes it back by signing it again.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and the server a test binds is kept until it ends"
)]

use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
use tilo_actions::remote::ActionCallError::UnknownAction;
use tilo_actions::remote::Caller;
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit_device, caller, governed};
use serde_json::json;

use super::turn_use_tests::{a_mail_login, add_mail, asked, proving};
use crate::workspaces::fixtures::test_host_taking_part as test_host;
use crate::workspaces::hosted_use::{HostedTurn, UseAnswer};
use crate::workspaces::tests::governor;
use crate::workspaces::{WorkspaceId, Workspaces};
use collab_types::work_delegation::WorkDelegation;
use tilo_actions::remote::ActionCallError::Refused;

const AGENT: &str = "scout";

/// The caller a turn's tools act as when the server carries it out for `person`'s
/// agent `agent`: the caller names `device`, this server's own, where the person
/// delegated the work to it.
fn on_server(person: &DeviceCert, device: &DeviceCert, workspace: &str) -> Caller {
    Caller::authenticated(person.user_public_key, device.device_public_key)
        .in_workspace(workspace)
        .delegated_to(AGENT)
        .named("Scout")
        .in_agent_session("agent-session:test")
        .on_turn(
            tilo_action_policy::turn::Turn::new("conversation-1", "message-1"),
            Vec::new(),
        )
}

/// The turn of `AGENT`, its person, and the workspace it works in.
fn turn<'a>(workspace_id: WorkspaceId, caller: &'a Caller, keeper: &'a str) -> HostedTurn<'a> {
    HostedTurn {
        workspace: workspace_id,
        caller,
        keeper,
        bound: None,
    }
}

/// The member's delegation of undoable work in `app` to `server`, letting it use
/// the vault items `secrets`, signed by their own device `key`.
fn delegating(
    key: &SigningKey,
    person: &DeviceCert,
    server: [u8; 32],
    workspace: &str,
    app: &str,
    secrets: Vec<String>,
) -> WorkDelegation {
    let member = Grantee::Member(tilo_action_policy::MemberId::agent(AGENT));
    let grant = Grant::new(
        "delegated-work",
        member,
        TreePath::workspace(workspace).app(app),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person(hex::encode(person.user_public_key)),
    );
    let now = tilo_clock::unix_now_ns();
    WorkDelegation::sign(
        key,
        person.user_public_key,
        server,
        workspace,
        Some(AGENT.to_owned()),
        vec![grant],
        secrets,
        (now.saturating_sub(1), now.saturating_add(600_000_000_000)),
    )
    .unwrap()
}

/// Write `delegation` into `workspace` as `key`, the member's device that signed
/// it, through the workspace service's own entry for it.
async fn author(
    workspaces: &Workspaces,
    workspace: &str,
    key: &SigningKey,
    delegation: &WorkDelegation,
) {
    workspaces
        .author_work_delegation(workspace, delegation, key)
        .await
        .unwrap();
}

/// A turn for a member acts on this server's own device only while the member's
/// delegation of that work to it is live: without one, and once they revoke it,
/// it is refused. Their own device is one their work is admitted on all the same.
#[tokio::test]
async fn a_turn_acts_on_the_server_only_under_the_members_live_delegation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, _service) = governed(workspaces).await;
    let (ada_key, ada) = admit_device(workspaces, &workspace, &owner, 1).await;
    let keeper = hex::encode(ada.user_public_key);
    let hers = on_server(&ada, &owner, &workspace);
    let theirs = on_server(&ada, &ada, &workspace);
    let by_server = turn(workspace_id, &hers, &keeper);
    let by_her_device = turn(workspace_id, &theirs, &keeper);

    // Nothing delegated yet: the server's own device is no device her work is
    // admitted on, and her own device is.
    assert_eq!(
        workspaces.keepers_turn(&by_server).await,
        Err(UnknownAction)
    );
    assert!(workspaces.keepers_turn(&by_her_device).await.is_ok());
    assert_eq!(
        workspaces
            .work_device(
                workspace_id,
                ada.user_public_key,
                &tilo_action_policy::MemberId::agent(AGENT)
            )
            .await,
        Some(ada.device_public_key),
        "a turn acts on her own device where she delegated nothing"
    );

    // Her device delegates the work to the server as her agent, and from then on
    // the server's device is the one her work is admitted on.
    let delegation = delegating(
        &ada_key,
        &ada,
        owner.device_public_key,
        &workspace,
        "so.tilo.mail",
        Vec::new(),
    );
    author(workspaces, &workspace, &ada_key, &delegation).await;
    assert!(
        workspaces.keepers_turn(&by_server).await.is_ok(),
        "the server takes her work on its own device under her delegation"
    );
    assert!(workspaces.keepers_turn(&by_her_device).await.is_ok());
    assert_eq!(
        workspaces
            .work_device(
                workspace_id,
                ada.user_public_key,
                &tilo_action_policy::MemberId::agent(AGENT)
            )
            .await,
        Some(owner.device_public_key),
        "a delegated turn acts on the server"
    );

    // A turn of another agent of hers is not covered: the delegation names the
    // actor the server acts as, and nobody else.
    let another = Caller::authenticated(ada.user_public_key, owner.device_public_key)
        .in_workspace(&workspace)
        .delegated_to("someone-else")
        .in_agent_session("agent-session:other")
        .on_turn(
            tilo_action_policy::turn::Turn::new("conversation-1", "message-1"),
            Vec::new(),
        );
    let theirs_agent = turn(workspace_id, &another, &keeper);
    assert_eq!(
        workspaces.keepers_turn(&theirs_agent).await,
        Err(UnknownAction)
    );

    // Her device revokes it, and the server takes nothing of hers from then on.
    let revoked = delegation
        .revoked(&ada_key, tilo_clock::unix_now_ns())
        .unwrap();
    author(workspaces, &workspace, &ada_key, &revoked).await;
    assert_eq!(
        workspaces.keepers_turn(&by_server).await,
        Err(UnknownAction),
        "a revocation stops the server's turn at once"
    );
}

/// Another person's server, named among the workspace's key-holding holders. The
/// member delegated nothing to it.
async fn named_server(
    workspaces: &Workspaces,
    workspace: &str,
    governor_key: &SigningKey,
) -> DeviceCert {
    let root = SigningKey::from_bytes(&[92; 32]);
    let key = SigningKey::from_bytes(&[91; 32]);
    let mut server = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Household server".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut server, &root).unwrap();
    let hint = tilo_peer_state::owner::testing::governor::holding_hint(
        &server,
        &key,
        tilo_server_protocol::WorkspaceId::parse(workspace).unwrap(),
        iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&server.device_public_key).unwrap(),
        ),
    );
    let now = tilo_clock::wall();
    let state = workspaces.workspace(workspace).await.unwrap();
    let mut governed = state.lock().await;
    let mut candidate = governed.peer().clone();
    candidate
        .governance_mut()
        .author_keeping_device(&hint, governor_key, now)
        .unwrap();
    let collaboration = governed.collaboration().clone();
    governed
        .commit(candidate, governor_key, now, |_, _| Ok(collaboration))
        .unwrap();
    server
}

/// The member's own device holds the workspace from what the server hands it: the
/// server's work is admitted there as theirs, under the delegation their device
/// signed — and only within its paths. Another server's work, which they
/// delegated nothing to, is refused, as it is once they revoke the delegation.
#[tokio::test]
async fn the_members_device_admits_the_servers_work_under_their_delegation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _workspace_id, _service) = governed(workspaces).await;
    let (ada_key, ada) = admit_device(workspaces, &workspace, &owner, 1).await;
    let other = named_server(workspaces, &workspace, &key).await;

    // Her device joins the workspace from the server and takes its governance in,
    // as a joining device does.
    let held = workspaces.state(&workspace).await.unwrap();
    let mut hers = tilo_peer_state::StatePeer::new(
        &workspace,
        owner.clone(),
        held.certificates(),
        tilo_user_identity::RevocationList::new(),
        tilo_clock::wall(),
    )
    .unwrap();
    hers.select_exact_admissions().unwrap();
    let take = |hers: &mut tilo_peer_state::StatePeer, held: &tilo_peer_state::StatePeer| {
        hers.receive_from(
            tilo_peer_state::TransportOrigin::RetainedServerDelta(owner.device_public_key),
            held.workspace_message(),
            tilo_clock::wall(),
        )
        .unwrap();
    };
    take(&mut hers, &held);
    let now = tilo_clock::wall();

    // The other server takes nothing of hers: she delegated it no work.
    assert!(
        hers.permissions(other.device_public_key, now).is_err(),
        "no delegation names that server"
    );

    // Her own device delegates her mail's work to it, as her agent, and her device
    // admits the server's work as hers from then on — only where its paths reach.
    let delegation = delegating(
        &ada_key,
        &ada,
        other.device_public_key,
        &workspace,
        "so.tilo.mail",
        Vec::new(),
    );
    author(workspaces, &workspace, &ada_key, &delegation).await;
    take(&mut hers, &workspaces.state(&workspace).await.unwrap());
    let now = tilo_clock::wall();
    assert_eq!(
        hers.workspace().work_delegations(),
        vec![delegation.clone()],
        "her device took the delegation in"
    );
    assert_eq!(
        hers.governance().holder_role(&other.device_public_key),
        Some(tilo_peer_state::CopyRole::KeyHolder),
        "the delegation names a key holder of the workspace"
    );
    let access = hers
        .permissions(other.device_public_key, now)
        .expect("the delegation admits the server's work as hers");
    assert_eq!(
        access.acting.by.member_id(),
        Some(&tilo_action_policy::MemberId::agent(AGENT))
    );
    assert!(
        hers.permits_app_action(
            other.device_public_key,
            "so.tilo.mail",
            "message",
            "send",
            now
        )
        .unwrap(),
        "the delegation's path lets the server send her mail"
    );
    assert!(
        !hers
            .permits_app_action(
                other.device_public_key,
                "so.tilo.chat",
                "message",
                "send",
                now
            )
            .unwrap(),
        "work outside the delegation's paths is not hers to give"
    );

    // Her device revokes it, and takes nothing of the server's from then on.
    let revoked = delegation
        .revoked(&ada_key, tilo_clock::unix_now_ns())
        .unwrap();
    author(workspaces, &workspace, &ada_key, &revoked).await;
    take(&mut hers, &workspaces.state(&workspace).await.unwrap());
    let now = tilo_clock::wall();
    assert!(
        hers.permissions(other.device_public_key, now).is_err(),
        "the revocation stops the server's work at once"
    );
}

/// A server uses a member's vault item only where their delegation names it:
/// an item it does not name is refused before the vault is opened and stays on
/// their own devices, a named one is asked about and used, and their
/// revocation stops both at once again.
#[tokio::test]
async fn a_server_uses_only_the_vault_items_the_members_delegation_names() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let (ada_key, ada) = admit_device(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let login = a_mail_login(&service, workspace_id, (&ada, &as_ada)).await;
    let item = login
        .as_str()
        .unwrap()
        .trim_start_matches("vault.item:")
        .to_owned();
    let proved = proving(workspaces);
    let keeper = hex::encode(ada.user_public_key);
    let on_its_device = on_server(&ada, &owner, &workspace);
    let by_server = turn(workspace_id, &on_its_device, &keeper);
    let with = |address: &str| add_mail(address, "imap.example.test", &login);

    // Her delegation names the work and no item of hers: the server is refused
    // before the vault is opened, and nobody is asked about it.
    let unnamed = delegating(
        &ada_key,
        &ada,
        owner.device_public_key,
        &workspace,
        "so.tilo.mail",
        Vec::new(),
    );
    author(workspaces, &workspace, &ada_key, &unnamed).await;
    let adding = with("ada@example.test");
    let refused = workspaces
        .use_for_turn(&by_server, &adding, json!({}))
        .await;
    assert!(
        matches!(&refused, Err(Refused { reason }) if reason.contains("stays on your own devices")),
        "{refused:?}"
    );
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody is asked"
    );

    // A device of her own is not the server's: everything of hers is hers to use
    // there, and the same use asks her there.
    let on_hers = on_server(&ada, &ada, &workspace);
    let by_her_device = turn(workspace_id, &on_hers, &keeper);
    let later = with("later@example.test");
    let hers = asked(
        workspaces
            .use_for_turn(&by_her_device, &later, json!({}))
            .await,
    );
    assert_eq!(hers.person, keeper);

    // Named, the server's turn is asked about it, and what she allows is made.
    let named = delegating(
        &ada_key,
        &ada,
        owner.device_public_key,
        &workspace,
        "so.tilo.mail",
        vec![item],
    );
    author(workspaces, &workspace, &ada_key, &named).await;
    let open = asked(
        workspaces
            .use_for_turn(&by_server, &adding, json!({}))
            .await,
    );
    assert_eq!(open.agent, AGENT);
    assert_eq!(open.person, keeper);
    let account = workspaces
        .answer_use(&by_server, &open.ask, &open.detail, UseAnswer::Always)
        .await
        .expect("allowed, it is made")
        .expect("it answers the account");
    assert_eq!(account["address"], "ada@example.test");
    assert_eq!(proved.lock().unwrap().len(), 1, "it signed in once");

    // Her revocation stops it at once: the server is no device her work is on.
    assert!(
        workspaces
            .revoke_work_delegation(
                &workspace,
                &owner.device_public_key,
                &ada.user_public_key,
                &ada_key,
            )
            .await
            .unwrap()
    );
    let other = with("other@example.test");
    assert_eq!(
        workspaces.use_for_turn(&by_server, &other, json!({})).await,
        Err(UnknownAction)
    );
    server.shutdown().await;
}
