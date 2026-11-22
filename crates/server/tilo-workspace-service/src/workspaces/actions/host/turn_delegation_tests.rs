//! A member delegates their own work to a server: their own device admits that
//! server's work as theirs under the delegation it signed, and takes it back by
//! signing it again.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and the server a test binds is kept until it ends"
)]

use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit_device, governed};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::test_host;
use crate::workspaces::tests::governor;
use collab_types::work_delegation::WorkDelegation;

const AGENT: &str = "scout";

/// The member's delegation of undoable work in `app` to `server`, signed by their
/// own device `key`.
fn delegating(
    key: &SigningKey,
    person: &DeviceCert,
    server: [u8; 32],
    workspace: &str,
    app: &str,
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
        Vec::new(),
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
