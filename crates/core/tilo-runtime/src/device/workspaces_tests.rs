//! A logged-in computer's own workspace service holds the workspace its login entered as a member
//! copy synced with the server that hosts it: the session dials from the device's endpoint and
//! reads with its key service, and the copy opens what the server sealed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the server and the device stay up until it ends"
)]

use std::time::Duration;

use automerge::ReadDoc as _;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, DESKTOP_KIND};
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DeviceWrites as _, DocumentRead};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::Workspaces;

use super::ServiceWorkspaces;
use crate::device::first_workspace::FirstWorkspace;
use std::sync::Arc;

/// A device `root` certified, and its key.
fn enrol(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let cert = tilo_peer_state::fixture::labelled(root, &key, label, capabilities);
    (key, cert)
}

/// The layout's front as `workspaces` reads it now, when it reads.
fn front(workspaces: &Workspaces, workspace: &str) -> Option<String> {
    let read = ServiceDocuments::of(workspaces.clone())
        .read(workspace, DESKTOP_APP, DESKTOP_DOCUMENT, None)
        .ok()?;
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    let layout = automerge::AutoCommit::load(&bytes).ok()?;
    match layout.get(automerge::ROOT, "front").ok()? {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Found the layout on `workspaces` with `want` as its front, as the device whose service this
/// is writes it: on the founding every copy of the record derives, naming this device's person.
async fn found_front(workspaces: &Workspaces, workspace: &str, want: &str) {
    let person = tilo_actions::Acting::person(tilo_actions::PersonId::from_root(
        workspaces.device_cert().user_public_key,
    ));
    let mut document = tilo_desktop_model::record::founded(workspace).expect("the layout founds");
    automerge::transaction::Transactable::put(&mut document, automerge::ROOT, "front", want)
        .unwrap();
    document.commit_with(
        automerge::transaction::CommitOptions::default().with_message(
            tilo_actions::ActionContext::whole_edit_by(&person, &document),
        ),
    );
    ServiceWrites::of(workspaces.clone())
        .write(
            workspace,
            &document.save(),
            tilo_peer_state::workspace_documents::DocumentWriting {
                app: DESKTOP_APP,
                kind: Some(DESKTOP_KIND),
                record: DESKTOP_DOCUMENT,
            },
        )
        .await
        .unwrap_or_else(|refusal| panic!("the server keeps the layout: {refusal:?}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_devices_service_holds_its_workspace_and_opens_what_the_server_sealed() {
    Box::pin(holds_its_workspace_and_lists_what_the_server_keeps(
        DeviceCapabilities::member(),
    ))
    .await;
}

/// A server on an operating system Tilo does not control, as one in a container or on a rented
/// machine is, is certified foreign: it never administers a workspace, so the person's device that hands it their
/// own workspace lists what the server keeps for them, and the login reads it there all the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_foreign_server_handed_the_persons_workspace_has_its_workspaces_listed_by_their_device() {
    Box::pin(holds_its_workspace_and_lists_what_the_server_keeps(
        DeviceCapabilities {
            foreign: true,
            ..DeviceCapabilities::member()
        },
    ))
    .await;
}

#[expect(
    clippy::too_many_lines,
    reason = "one login, the workspace it holds and the records that open here, read top to bottom"
)]
async fn holds_its_workspace_and_lists_what_the_server_keeps(
    server_capabilities: DeviceCapabilities,
) {
    let foreign = server_capabilities.foreign;
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", server_capabilities);
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = crate::device::keep::server::KeptServer::start(
        test_server::on_loopback_relay(tilo_server_protocol::config::ServerConfig::loopback()),
        state.path().join("server"),
        crate::device::keep::server::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let workspace = hosted.create("Studio").await.unwrap().workspace;
    hosted
        .enter(&workspace, (&laptop_cert, &laptop_key))
        .await
        .expect("a device of the person enters the workspace");
    found_front(&hosted, &workspace, "Main").await;

    // The computer's login, as `tilo login` left it, naming the server's address.
    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.clone(),
        workspace_name: Some("Studio".into()),
        agent: "Claude Code".into(),
        cert: laptop_cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    };
    let own = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the login enters its workspace from the device's endpoint",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(own.lent())),
    )
    .await
    .expect("the login enters");

    // The device's one copy of the workspace is its service's: the session keeps none.
    let id = tilo_server_protocol::WorkspaceId::parse(&workspace).unwrap();
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;
    assert!(
        own.copies().is_member(&workspace).await.unwrap(),
        "the workspace the login entered is a member copy here"
    );
    let copy = own
        .peer(id)
        .expect("the service holds the workspace it entered");
    assert!(
        copy.is_member(&laptop_cert.user_public_key),
        "the copy names the person a member"
    );
    assert!(own.holds(id), "held as a member copy");
    assert!(
        own.workspaces()
            .iter()
            .any(|held| held.id().to_string() == workspace),
        "the workspaces this device holds name it"
    );
    tilo_test_env::within("the member copy opens the server's sealed layout", async {
        loop {
            let (copies, named) = (own.copies().clone(), workspace.clone());
            let read = tokio::task::spawn_blocking(move || front(&copies, &named))
                .await
                .unwrap();
            if read.as_deref() == Some("Main") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    // As the login does, the device founds the workspace it keeps its own lists in and hands
    // it to the server, with the change naming the server among its holders.
    let person = tilo_test_env::within(
        "the device founds its own workspace",
        tilo_workspace_service::workspaces::person_workspace::found(own.copies()),
    )
    .await
    .expect("the person's own workspace");
    tilo_test_env::within(
        "the server holds the person's own workspace",
        own.copies().attach_server(
            &person,
            server_cert.device_public_key,
            server.node_addr(),
            tilo_peer_state::CopyRole::KeyHolder,
        ),
    )
    .await
    .expect("the server takes the person's own workspace");

    // With no login to name it, the copy names the person's server: the host that keeps the
    // workspace's keys and is not this device. The account asked there answers.
    let hosts = own.copies().hosts(&workspace).await.unwrap();
    let found = crate::device::home::server_among(&hosts, &laptop_cert.device_public_key)
        .expect("the copy names the server");
    assert_eq!(found.endpoint_addr.id, server.node_addr().id);
    assert_eq!(
        found.device_certificate.device_public_key, server_cert.device_public_key,
        "the server signs as the device its endpoint is keyed by"
    );
    let account = crate::device::account::Account::at(&own, &found.endpoint_addr, &workspace);
    let listed = tilo_test_env::within(
        "the server answers the account its copy names",
        account.listed(
            &laptop_cert.user_public_key,
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            None,
        ),
    )
    .await
    .expect("the person's workspace list");
    assert!(
        listed
            .workspaces
            .iter()
            .any(|listed| listed.workspace == workspace),
        "the list the server answers names the workspace it keeps: {listed:?}"
    );
    assert!(
        listed
            .workspaces
            .iter()
            .all(|listed| listed.workspace != person),
        "the person's list of workspaces never names their own workspace: {listed:?}"
    );
    // Their own workspace is no row in that list: the login finds the server keeping it by
    // entering the workspace and reading its own record, which names the device that keeps it.
    let own_hosts = own.copies().hosts(&person).await.unwrap();
    assert!(
        own_hosts
            .iter()
            .any(|hint| hint.host.device_public_key == server_cert.device_public_key),
        "the own workspace's record names the server keeping it: {own_hosts:?}"
    );
    if foreign {
        assert!(
            !hosted
                .list_for(&laptop_cert.user_public_key, &workspace, "Studio")
                .await,
            "a foreign server writes nothing of its person's list on its own key"
        );
    }
    // The device lists only what the server signed an address for: an entry another device's
    // address names is no entry of the server's.
    let elsewhere = own
        .copies()
        .kept_at(&person)
        .await
        .expect("the own workspace is kept");
    let forged = tilo_server_protocol::workspaces::KeptWorkspace {
        workspace: workspace.clone(),
        name: "Studio".into(),
        host_hint: hex::encode(postcard::to_allocvec(&elsewhere.hint.unwrap()).unwrap()),
    };
    assert!(
        !own.copies()
            .list_kept_by(
                &laptop_cert.user_public_key,
                server_cert.device_public_key,
                &[forged]
            )
            .await,
        "an address for another workspace lists nothing"
    );

    // A workspace the person makes after the hand-over is founded on this device, placed at
    // their server and listed there: their other device reads it in the list the server answers
    // and enters it at the server, with this device away from the call.
    let labs = tilo_test_env::within(
        "the device founds a workspace and places it at the server",
        own.copies().found("Labs", None),
    )
    .await
    .expect("the device founds the workspace");
    assert!(
        hosted.summary(&labs.workspace).await.is_ok(),
        "the server keeps the workspace its person's device founded"
    );
    tilo_test_env::within("the server's list names the new workspace", async {
        loop {
            let listed = account
                .listed(
                    &laptop_cert.user_public_key,
                    &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
                    Some(&labs.workspace),
                )
                .await;
            if listed.is_ok_and(|listed| {
                listed
                    .workspaces
                    .iter()
                    .any(|listed| listed.workspace == labs.workspace && listed.host.is_some())
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let (phone_key, phone_cert) = enrol(&root, "phone", DeviceCapabilities::member());
    assert!(
        hosted
            .enter(&labs.workspace, (&phone_cert, &phone_key))
            .await
            .is_ok(),
        "the person's other device enters the workspace at their server"
    );

    entered.close();
    server.shutdown().await;
}

/// A workspace this computer joins after its login reaches it over its host's link like the one it
/// logged into: the join is the host's own entry, and its answer names where the host holds the
/// workspace, so this device's service takes it in as a member copy and the records sealed there
/// open here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_joined_after_the_login_reaches_this_devices_copy() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = crate::device::keep::server::KeptServer::start(
        test_server::on_loopback_relay(tilo_server_protocol::config::ServerConfig::loopback()),
        state.path().join("server"),
        crate::device::keep::server::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let studio = hosted.create("Studio").await.unwrap().workspace;
    hosted
        .enter(&studio, (&laptop_cert, &laptop_key))
        .await
        .expect("a device of the person enters the workspace it logs into");
    found_front(&hosted, &studio, "Main").await;
    // A second workspace of the host's, which this device is a member of and which the login did
    // not name: the join is how it reaches the device.
    let labs = hosted.create("Labs").await.unwrap().workspace;
    hosted
        .enter(&labs, (&laptop_cert, &laptop_key))
        .await
        .expect("the same device is a member of the second workspace");
    found_front(&hosted, &labs, "Bench").await;

    // The computer's login, as `tilo login` left it, naming the server's address.
    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: studio.clone(),
        workspace_name: Some("Studio".into()),
        agent: "Claude Code".into(),
        cert: laptop_cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    };
    let own = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the login enters its workspace from the device's endpoint",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(own.lent())),
    )
    .await
    .expect("the login enters");
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;
    assert!(
        !own.copies().is_member(&labs).await.unwrap(),
        "a workspace the login did not enter is not held before the join"
    );

    // The join goes over the device's link: the host's answer says where it holds the workspace,
    // and its records reach the copy the device's service keeps by it.
    tilo_test_env::within(
        "the host enters the device into the workspace it joins",
        entered.join(&labs),
    )
    .await
    .expect("the host enters a device that is a member of its workspace");
    tilo_test_env::within(
        "the device's service keeps the joined workspace",
        own.hold(&entered),
    )
    .await;
    assert!(
        own.copies().is_member(&labs).await.unwrap(),
        "the joined workspace is a member copy here"
    );
    tilo_test_env::within("the joined workspace's records arrive here", async {
        loop {
            let (copies, named) = (own.copies().clone(), labs.clone());
            let read = tokio::task::spawn_blocking(move || front(&copies, &named))
                .await
                .unwrap();
            if read.as_deref() == Some("Bench") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    entered.close();
    server.shutdown().await;
}

/// A provider registered on a device answers what another device of the workspace asks over
/// `tilo-actions/1` only as this device's own copy of the workspace says: the asking device is
/// refused while its role does not let it run the action, and once the person's server writes it
/// a role that does, the change reaches the copy and the same call is answered. Nobody signs in on
/// this device for any of it: its key and certificate are all it opens the workspace with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one grant, from the server to this device's copy to a provided call, read top to bottom"
)]
async fn a_grant_written_on_the_server_reaches_this_devices_copy_and_admits_a_provided_call() {
    use tilo_actions::remote::{
        ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    use tilo_server_protocol::workspaces::Role;

    const PROVIDER: &str = "so.tilo.devices";
    // The person the workspace fixtures sign pre-admissions as, so a guest is let in as the
    // server's owner lets people in.
    let root = SigningKey::from_bytes(&[1; 32]);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = crate::device::keep::server::KeptServer::start(
        test_server::on_loopback_relay(tilo_server_protocol::config::ServerConfig::loopback()),
        state.path().join("server"),
        crate::device::keep::server::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let workspace = hosted.create("Home").await.unwrap().workspace;
    hosted
        .enter(&workspace, (&laptop_cert, &laptop_key))
        .await
        .expect("a device of the person enters the workspace");
    // Another person's device, in the workspace as a viewer.
    let (guest_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 2);
    let (guest_key, guest_cert) = enrol(&guest_root, "Bo's laptop", DeviceCapabilities::member());
    tilo_workspace_service::workspaces::fixtures::entry_join_as(
        &hosted,
        &server_cert,
        &workspace,
        (&guest_key, &guest_cert),
        81,
        Role::Viewer,
    )
    .await;

    let login = tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.clone(),
        workspace_name: Some("Home".into()),
        agent: "tilo".into(),
        cert: laptop_cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    };
    let own = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&login).await.unwrap(),
    )
    .await
    .expect("the laptop's own service opens");
    let entered = tilo_test_env::within(
        "the device enters its workspace",
        tilo_api::daemon::enter::connect(&laptop_key, &login, Some(own.lent())),
    )
    .await
    .expect("the device enters");
    tilo_test_env::within(
        "the device's service keeps the workspace",
        own.hold(&entered),
    )
    .await;

    // An action an editor runs and a viewer does not: an undoable change to the device's record.
    let provided = Arc::new(declaring(
        PROVIDER,
        "rename",
        tilo_actions::ActionKind::Undoable,
    ));
    own.copies().provide(PROVIDER, Arc::clone(&provided) as _);
    let router = iroh::protocol::Router::builder(own.endpoint())
        .accept(
            tilo_peer_state::actions::ACTIONS_ALPN,
            own.copies().action_service(),
        )
        .spawn();

    let guest_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&guest_key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let asking = tilo_peer_state::actions::ActionService::new(
        guest_key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    let laptop = Arc::new(asking.remote(&guest_endpoint, &own.endpoint().addr(), &workspace));
    let ask = || {
        let laptop = Arc::clone(&laptop);
        async move {
            let request = InvokeRequest {
                asks: false,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new("devices", "rename"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            tilo_test_env::within(
                "the laptop answers",
                laptop.invoke_until_settled(request, Duration::from_secs(20)),
            )
            .await
            .expect("the laptop answered")
        }
    };

    let refused = tilo_test_env::within("the guest is in the laptop's copy", async {
        loop {
            let answer = ask().await;
            match &answer {
                Err(ActionCallError::Refused { reason }) if reason.contains("do not let") => {
                    return answer;
                }
                // The guest's join has not reached the laptop's copy yet.
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await;
    assert!(refused.is_err(), "a viewer may not rename the device");
    assert!(
        provided.asked().is_empty(),
        "a refused call never reaches the provider"
    );

    hosted
        .set_member_role(
            &workspace,
            &server_cert,
            tilo_server_protocol::workspaces::SetMemberRoleRequest {
                request_id: [82; 32],
                user_public_key: guest_root.verifying_key().to_bytes(),
                role: Role::Editor,
            },
        )
        .await
        .expect("the server writes the guest a new role");
    tilo_test_env::within("the new role reaches the laptop's copy", async {
        loop {
            if ask().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let caller = provided.callers().pop().expect("the provider was asked");
    assert_eq!(
        caller.device_public_key(),
        Some(&guest_cert.device_public_key),
        "the provider sees the device that asked"
    );

    guest_endpoint.close().await;
    let _ = router.shutdown().await;
    entered.close();
    server.shutdown().await;
}

/// A desktop that kept its own key groups, sealed under its master key in the platform secret
/// store, hands them to this device's service once: the service seals them again under its own
/// vault, lists the workspace once it holds it, and opens it with the keys the desktop's group
/// had, founding no group beside it. Handed again, nothing it keeps is replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_desktops_key_groups_are_taken_in_and_open_its_workspace() {
    use tilo_session_client::workspace_keys::{MlsVault, vault};
    use tilo_workspace_keys::KeyStorage as _;

    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let (own, directory, key, cert, _) = ServiceWorkspaces::test_of_person().await;

    // The desktop's layout, as its session kept it under its state directory.
    let workspace = tilo_server_protocol::WorkspaceId::new();
    let state = directory.path().join("desktop-state");
    let root = state.join("tilo/collaboration/mls");
    let kept = std::sync::Arc::new(tilo_workspace_keys::KeyDir::new(root.join("groups")));
    let desktop_vault = MlsVault::open(kept.clone(), &vault::master_key_locator(&root)).unwrap();
    let signer = tilo_identity::identity_store::DeviceIdentity::kept(cert, key).signer;
    let desktop = key_group::GroupPeer::create(
        Some(kept.peers()),
        workspace,
        Box::new(signer),
        &desktop_vault.group_state_key(),
    )
    .await
    .unwrap();
    let desktops_keys = desktop
        .content_keys()
        .expect("the desktop's group has keys");

    let carried = vault::carried(&state)
        .unwrap()
        .expect("the desktop kept groups");
    let handed = serde_json::to_value(&carried).unwrap();
    let (imported, _) = own
        .import(serde_json::from_value(handed.clone()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        imported["groups"],
        serde_json::json!([workspace.to_string()]),
        "{imported}"
    );

    own.found(workspace, "Studio").await.unwrap();
    assert!(
        own.workspaces()
            .iter()
            .any(|listed| listed.id() == workspace && listed.name == "Studio"),
        "the service lists the workspace"
    );
    tilo_test_env::within("the service opens the desktop's group", async {
        loop {
            let access = own.keys().get().map(|keys| keys.access(workspace).0);
            if let Some(tilo_workspace_keys::KeyAccess::Ready(keys)) = access {
                assert!(
                    keys.same_write_key(&desktops_keys),
                    "the workspace opens with the desktop's group's keys, not a new group's"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    let (again, _) = own
        .import(serde_json::from_value(handed).unwrap())
        .await
        .unwrap();
    assert_eq!(again["groups"], serde_json::json!([]), "{again}");
}

/// A computer with no login founds no workspace for its person before they arrive. A desktop
/// whose own workspace service founded its person's workspace, kept nowhere else, hands it over
/// once: this device's service keeps it from then on in the desktop service's place, lists it and
/// reads what the desktop wrote in it, and it is the person's workspace here, so none is founded
/// beside it, then or at a later start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_the_desktop_founded_moves_here_and_is_the_persons_own() {
    use tilo_session_client::workspace_keys::vault;
    use tilo_workspace_service::workspaces::moving_out;

    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let (own, directory, key, cert, _) = ServiceWorkspaces::test_of_person().await;
    let home = FirstWorkspace::open(Arc::clone(&own), None);
    assert_eq!(
        home.get(),
        None,
        "nothing is founded before the person arrives"
    );
    let state = directory.path().join("desktop-state");
    let person = desktop_founds_its_persons_workspace(&state, &cert, key.clone()).await;

    let service_root = state.join("tilo/workspace-service");
    let moving = moving_out(&service_root, &cert, &key)
        .unwrap()
        .expect("the desktop's service kept workspaces");
    assert_eq!(
        moving
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>(),
        [person]
    );
    let groups = vault::carried(&state)
        .unwrap()
        .expect("the desktop kept groups");
    let handed = serde_json::json!({
        "sealed_with": groups.sealed_with,
        "groups": groups.groups,
        "service": moving.to_hex().unwrap(),
    });
    let (imported, moved) = own
        .import(serde_json::from_value(handed.clone()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        imported["moved"],
        serde_json::json!([person.to_string()]),
        "{imported}"
    );
    home.moved_in(&moved);
    assert_eq!(
        home.here().await.unwrap(),
        person.to_string(),
        "the desktop's workspace is the person's here"
    );

    let listed: Vec<_> = own
        .workspaces()
        .iter()
        .map(tilo_desktop_model::Workspace::id)
        .collect();
    assert_eq!(listed, [person], "the person keeps one workspace");
    tilo_test_env::within("this device reads what the desktop wrote", async {
        while front(own.copies(), &person.to_string()).as_deref() != Some("desk") {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    // This device's service keeps it in the desktop's service's place.
    let keeping: Vec<_> = own
        .copies()
        .hosts(&person.to_string())
        .await
        .unwrap()
        .iter()
        .map(|hint| hint.host.device_public_key)
        .collect();
    assert_eq!(keeping, [own.copies().device_cert().device_public_key]);

    let (again, _) = own
        .import(serde_json::from_value(handed).unwrap())
        .await
        .unwrap();
    assert_eq!(again["moved"], serde_json::json!([]), "{again}");
    let started_again = FirstWorkspace::open(own, None);
    assert_eq!(started_again.get(), Some(person.to_string().as_str()));
}

/// The computer that made the person's root founds their workspace once, at the id their key
/// names, and holds it at every later start: one workspace, however often it is asked to found.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_computer_that_made_the_root_founds_the_persons_workspace_once() {
    let (own, _directory, _, cert, _) = ServiceWorkspaces::test_of_person().await;
    let person = cert.user_public_key;
    let home = FirstWorkspace::open(Arc::clone(&own), None);
    assert!(
        own.workspaces().is_empty(),
        "nothing is founded at the start"
    );
    let founded = home.found().await.unwrap();
    assert_eq!(
        founded,
        tilo_server_protocol::servers::person_workspace_id(&person),
        "it is founded at the id the person's key names"
    );
    assert_eq!(home.found().await.unwrap(), founded, "it is founded once");
    assert!(
        home.keeps_account(),
        "the computer keeps its person's account"
    );
    let listed: Vec<_> = own
        .workspaces()
        .iter()
        .map(|held| (held.id().to_string(), held.name.clone()))
        .collect();
    assert_eq!(listed, [(founded.clone(), "Home".to_owned())]);
    let started_again = FirstWorkspace::open(own, None);
    assert_eq!(started_again.get(), Some(founded.as_str()));
}

/// A computer that holds the person's root but not their workspace, and holds another workspace
/// of its own, has none of the person's: every ask of theirs that needs it is refused in words
/// that say how to add the computer, and nothing founds theirs beside the one their other devices
/// hold. The workspace it does hold stays as it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_holding_the_root_alone_has_no_workspace_of_the_persons() {
    let (own, _directory, _, cert, _) = ServiceWorkspaces::test_of_person().await;
    let person = cert.user_public_key;
    let elsewhere = tilo_desktop_model::WorkspaceId::new();
    own.found(elsewhere, "Plans").await.unwrap();

    let home = FirstWorkspace::open(Arc::clone(&own), None);
    assert_eq!(home.get(), None, "the other workspace is not the person's");
    assert!(!home.keeps_account(), "it keeps no account of theirs");
    let refused = home
        .here()
        .await
        .expect_err("the person has no workspace here");
    assert!(refused.contains("tilo device add"), "{refused}");
    let theirs = tilo_server_protocol::servers::person_workspace_id(&person);
    let settling = Arc::clone(&home);
    let named = theirs.clone();
    let refused = tokio::task::spawn_blocking(move || settling.settle_if_theirs(&named))
        .await
        .unwrap()
        .expect_err("a call in the person's workspace finds none");
    assert!(refused.contains("tilo device add"), "{refused}");
    let settling = Arc::clone(&home);
    tokio::task::spawn_blocking(move || settling.settle_if_theirs(&elsewhere.to_string()))
        .await
        .unwrap()
        .expect("a call in another workspace is left as it is");
    let held: Vec<_> = own
        .workspaces()
        .iter()
        .map(|listed| listed.id().to_string())
        .collect();
    assert_eq!(
        held,
        [elsewhere.to_string()],
        "nothing was founded beside it"
    );
    let started_again = FirstWorkspace::open(own, None);
    assert_eq!(started_again.get(), None);
}

/// The standing of the workspace a fresh person's computer founds encodes as the JSON the
/// daemon publishes to their shell: its people and the devices each holds travel with string
/// keys, and it reads back as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_founded_workspaces_standing_crosses_as_json() {
    use tilo_peer_state::workspace_view::WorkspaceReading;
    use tilo_workspace_service::workspaces::standing;

    let (own, _directory, _, cert, _) = ServiceWorkspaces::test_of_person().await;
    let person = cert.user_public_key;
    let home = FirstWorkspace::open(Arc::clone(&own), None);
    let founded = home.found().await.unwrap();

    let held = own.held();
    let (workspace, _, peer) = held
        .iter()
        .find(|(workspace, _, _)| workspace.to_string() == founded)
        .expect("the workspace this computer founded is held");
    let signer = own.signer();
    let reader = standing::Reader {
        person,
        device: cert.device_public_key,
        signer: signer.as_ref(),
    };
    let reading = standing::reading(*workspace, peer, &reader, None, Vec::new());
    assert_eq!(reading.id, founded);
    // It keeps the workspace it founded, and so waits for no host to take it in: its shell
    // launches the workspace's apps at once.
    assert!(
        reading
            .hosting
            .as_ref()
            .is_some_and(|hosting| hosting.admitted),
        "the founding device is admitted to its own workspace: {:?}",
        reading.hosting
    );
    // It is the holders record naming it that lets it in, not the copy: another device of the
    // same person reading these same bytes, named nowhere in it, waits for its host.
    let unnamed = SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
    let copy = standing::reading(
        *workspace,
        peer,
        &standing::Reader {
            person,
            device: unnamed,
            signer: signer.as_ref(),
        },
        None,
        Vec::new(),
    );
    assert!(
        copy.hosting
            .as_ref()
            .is_some_and(|hosting| !hosting.admitted),
        "a device with a copy that no holders record names is not admitted: {:?}",
        copy.hosting
    );
    assert!(
        reading
            .members
            .devices
            .values()
            .any(|devices| devices.contains(&cert.device_public_key)),
        "this device is listed among its person's: {:?}",
        reading.members.devices
    );

    let json = serde_json::to_vec(&reading).expect("the standing encodes as JSON");
    let read: WorkspaceReading = serde_json::from_slice(&json).expect("and reads back");
    assert_eq!(
        read.members, reading.members,
        "its members cross as they are"
    );
    assert_eq!(read.people, reading.people, "its people cross as they are");
    assert_eq!(
        read.device_labels, reading.device_labels,
        "its labels cross"
    );
}

/// A desktop of the device `cert` certifies, keeping its state under `state`, whose own
/// workspace service founded its person's workspace, kept nowhere else, and wrote the layout's
/// front in it before it stopped: the workspace.
async fn desktop_founds_its_persons_workspace(
    state: &std::path::Path,
    cert: &DeviceCert,
    key: SigningKey,
) -> tilo_server_protocol::WorkspaceId {
    use tilo_session_client::workspace_keys::{MlsVault, vault};
    use tilo_workspace_service::own_device;

    let root = state.join("tilo/collaboration/mls");
    let kept = std::sync::Arc::new(tilo_workspace_keys::KeyDir::new(root.join("groups")));
    let desktop_vault = MlsVault::open(kept, &vault::master_key_locator(&root)).unwrap();
    let identity = tilo_identity::identity_store::DeviceIdentity::kept(cert.clone(), key.clone());
    let desktop_keys = own_device::KeyService::open(
        "desktop-groups",
        &root.join("groups"),
        desktop_vault.group_state_key(),
        std::sync::Arc::clone(&identity.signer),
        std::sync::Arc::new(tilo_workspace_keys::NoSealChecks),
    )
    .unwrap();
    let service_root = state.join("tilo/workspace-service");
    let desktop = own_device::open(
        service_root,
        own_device::Device {
            cert,
            signer: &identity.signer,
            itself: None,
        },
        super::test_endpoint(&SigningKey::from_bytes(&[71; 32])).await,
        desktop_keys.groups(),
        |config| tilo_workspace_service::service::ServiceConfig {
            reaching: tilo_workspace_service::Reaching::OnItsOwn,
            founding_registries: tilo_workspace_service::service::RegistriesFile::default(),
            ..config
        },
    )
    .await
    .unwrap();
    let person = tilo_server_protocol::WorkspaceId::parse(
        &tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key),
    )
    .unwrap();
    let name = tilo_workspace_service::workspaces::person_workspace::NAME;
    desktop
        .hold_own_workspace(&person.to_string(), name, cert)
        .await
        .unwrap();
    desktop
        .workspaces()
        .enter(&person.to_string(), (cert, &key))
        .await
        .unwrap();
    found_front(desktop.workspaces(), &person.to_string(), "desk").await;
    tilo_test_env::within("the desktop reads what it wrote", async {
        while front(desktop.workspaces(), &person.to_string()).as_deref() != Some("desk") {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    desktop.shutdown().await;
    drop(desktop);
    drop(desktop_keys);
    person
}

/// A provider of `provider` that declares the one action it answers, `devices#action`, of
/// `kind`, as every provider declares what it answers: what it declares the action's kind as is
/// what the workspace's grants are read for, and an action declared nowhere is refused.
fn declaring(
    provider: &'static str,
    action: &'static str,
    kind: tilo_actions::ActionKind,
) -> tilo_actions::remote::TestProvider {
    use tilo_actions::remote::{CataloguePage, CatalogueRow};
    tilo_actions::remote::TestProvider::new().with_catalogue(move |_, query| {
        let row = CatalogueRow::for_action(
            provider,
            provider,
            tilo_actions::ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new("devices", action),
                resource_kind: "device".into(),
                resource_label: "Devices".into(),
                action_label: action.into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract {
                    kind,
                    ..tilo_actions::ActionContract::default()
                },
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            },
        );
        Ok(CataloguePage::from_rows("1".into(), vec![row], &query))
    })
}

/// The daemon keeping the workspaces answers an action through the device's dispatch: the
/// same `Answers` its endpoint serves `tilo-actions/1` with, never a dispatch of the server's
/// own.
#[tokio::test]
async fn a_server_answers_an_action_through_its_devices_dispatch() {
    use std::sync::Arc;
    use tilo_actions::remote::{
        ActionProvider as _, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    };

    const PROVIDER: &str = "so.tilo.devices";
    let root = SigningKey::from_bytes(&[1; 32]);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = crate::device::keep::server::KeptServer::start(
        test_server::on_loopback_relay(tilo_server_protocol::config::ServerConfig::loopback()),
        state.path().join("server"),
        crate::device::keep::server::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server has workspaces")
        .clone();
    let workspace = hosted.create("Home").await.unwrap().workspace;

    let provided = Arc::new(declaring(
        PROVIDER,
        "restart",
        tilo_actions::ActionKind::Outside,
    ));
    hosted.provide(PROVIDER, Arc::clone(&provided) as _);
    let caller = tilo_workspace_service::service::local_caller(&server_cert)
        .in_workspace(workspace.as_str());
    let receipt = hosted
        .device_actions()
        .invoke(
            &caller,
            InvokeRequest {
                asks: false,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new("devices", "restart"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        )
        .await
        .expect("the device's dispatch answers")
        .expect("the provider takes the action");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{:?}",
        receipt.status
    );
    let asked = provided.callers().pop().expect("the provider was asked");
    assert_eq!(
        asked.device_public_key(),
        Some(&server_cert.device_public_key),
        "the provider sees the server's device ask"
    );
    server.shutdown().await;
}

/// A kept workspace's entry is settled once, as it is made: the values the
/// deployment names are the founder's first change, through the same entry
/// change an owner makes later. A start that finds the workspace held settles
/// nothing, so the owner's later change is what every start reads.
#[tokio::test]
async fn a_kept_workspaces_entry_is_settled_once_and_the_owners_change_stays() {
    use tilo_action_policy::{EntryMail, EntryPolicy, ListingPolicy, Role};

    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let (own, _directory, _key, cert, _) = ServiceWorkspaces::test_of_person().await;
    let workspace = tilo_server_protocol::WorkspaceId::new();
    let deployment = EntryPolicy {
        listing: Some(ListingPolicy {
            description: "Coffee and tea".into(),
            default_role: Role::Editor,
        }),
        invitation_only: true,
        wait_list_max: 4,
        contact: Some("hello@lounge.example".into()),
        mail: Some(EntryMail {
            from: "invitations@lounge.example".into(),
            templates: None,
        }),
    };
    assert!(
        own.found_kept(workspace, "Lounge", Some(&deployment))
            .await
            .unwrap(),
        "this start founds the workspace"
    );
    let workspaces = own.service().workspaces();
    let entry = workspaces
        .entry_policy(&workspace.to_string())
        .await
        .expect("the kept workspace is founded here");
    assert_eq!(
        entry, deployment,
        "the entry is the one the deployment names"
    );

    // The owner changes the wait list later, through the same entry change.
    let changed = EntryPolicy {
        wait_list_max: 7,
        contact: Some("owner@lounge.example".into()),
        ..deployment.clone()
    };
    workspaces
        .found_entry(&workspace.to_string(), &cert, changed.clone())
        .await
        .expect("the owner changes the entry");

    // A later start finds the workspace held and settles nothing over it.
    assert!(
        !own.found_kept(workspace, "Lounge", Some(&deployment))
            .await
            .unwrap(),
        "the workspace is held, not founded again"
    );
    let entry = workspaces
        .entry_policy(&workspace.to_string())
        .await
        .unwrap();
    assert_eq!(entry, changed, "the owner's change survives the restart");
}
