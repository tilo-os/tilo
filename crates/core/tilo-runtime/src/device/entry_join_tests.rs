//! A person's computer joins a public workspace another person's host keeps through its open
//! entry: the computer's own service asks the entry, signs its own admission with the device's
//! key and takes the workspace in, at the role the entry gives. The host signs nothing for it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard; the host and the device stay up until it ends"
)]

use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_server_protocol::directory::Listing;
use tilo_server_protocol::web::{PageHost, PageHostAddress};
use tilo_server_protocol::workspaces::Role;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use super::ServiceWorkspaces;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one join, from a refused entry to an open one and back again, read top to bottom"
)]
async fn a_computer_joins_a_public_workspace_through_its_open_entry() {
    // Bob's host keeps the Lounge; Ada has never been in it.
    let (bob, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let (host_key, host_cert) = enrol(&bob, "lounge-host", DeviceCapabilities::member());
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (laptop_key, laptop_cert) = enrol(&ada, "Ada's laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = crate::device::keep::server::KeptServer::start(
        test_server::on_loopback_relay(tilo_server_protocol::config::ServerConfig::loopback(
            state.path().join("host"),
        )),
        crate::device::keep::server::ServerIdentity {
            user_public_key: bob.verifying_key().to_bytes(),
            device_signing_key: host_key,
            device_cert: host_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the host has workspaces")
        .clone();
    let lounge = hosted.create("Lounge").await.unwrap().workspace;
    let id = tilo_server_protocol::WorkspaceId::parse(&lounge).expect("the workspace's id");
    // The host as the workspace's directory publishes it: who it is and where it is dialled.
    let host = PageHost {
        server_cert: host_cert.clone(),
        address: Some(PageHostAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    };
    let reached = tilo_server_client::ServerAddress::at(server.node_addr());
    let own = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop"),
        &laptop_key,
        &laptop_cert,
        &reached,
    )
    .await
    .expect("the laptop's own service opens");
    let copies = own.copies();
    // The join the person's session asks of the daemon, with the host its directory published.
    let asked = serde_json::Map::from_iter([
        (
            "workspace".to_owned(),
            serde_json::Value::from(lounge.clone()),
        ),
        ("host".to_owned(), serde_json::to_value(&host).unwrap()),
    ]);
    let joining = || async {
        tilo_test_env::within(
            "the entry answers",
            crate::device::shell::listed(&own, tilo_peer_state::workspace_view::JOIN, &asked),
        )
        .await
        .map(|joined| {
            let joined = joined
                .as_str()
                .expect("the join answers the workspace's id");
            tilo_server_protocol::WorkspaceId::parse(joined).expect("a workspace's id")
        })
    };

    // An entry the workspace has not opened refuses, in its own words, and leaves nothing here.
    let refused = joining()
        .await
        .expect_err("a workspace that is not public admits nobody");
    assert!(
        refused.contains("not public"),
        "the refusal says why: {refused}"
    );
    assert!(
        !copies.is_member(&lounge).await.unwrap(),
        "a refused join holds nothing"
    );
    assert!(
        !hosted
            .membership(&lounge)
            .await
            .unwrap()
            .1
            .contains_key(&hex::encode(ada.verifying_key().to_bytes())),
        "the host admitted nobody for the refused join"
    );

    // Its owner opens the entry to anyone, as viewers.
    hosted
        .publish(
            &lounge,
            &host_cert,
            Listing {
                description: "Anyone may come in".into(),
                default_role: Role::Viewer,
            },
        )
        .await
        .expect("the owner publishes the workspace");

    // The computer joins: the admission is its own device's, and the workspace is kept here.
    let joined = joining().await.expect("the open entry admits the computer");
    assert_eq!(joined, id, "the join answers the workspace it entered");
    assert!(
        copies.is_member(&lounge).await.unwrap(),
        "the joined workspace is kept here"
    );
    let marks = hosted.membership(&lounge).await.unwrap().1;
    let ada_mark = marks
        .get(&hex::encode(ada.verifying_key().to_bytes()))
        .expect("the workspace lists Ada");
    assert_eq!(
        ada_mark.role, "viewer",
        "the entry's published role is the one she has"
    );
    assert!(
        hosted
            .summary(&lounge)
            .await
            .unwrap()
            .devices
            .values()
            .flatten()
            .any(|device| *device == laptop_cert.device_public_key),
        "the laptop is the device the workspace admitted"
    );

    // The workspace opens here: what the copy holds reads as the workspace's own name.
    tilo_test_env::within("the joined workspace opens here", async {
        loop {
            if own
                .workspaces()
                .iter()
                .any(|kept| kept.id() == id && kept.name == "Lounge")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;

    // A second join is the first one: the same workspace, and no second admission.
    let again = joining().await.expect("joining again answers the same");
    assert_eq!(again, id);
    let log = hosted.membership_log(&lounge).await.unwrap();
    let joined_again = joining().await.expect("and again");
    assert_eq!(joined_again, id);
    assert_eq!(
        hosted.membership_log(&lounge).await.unwrap(),
        log,
        "joining again writes nothing more into the workspace's membership"
    );

    server.shutdown().await;
}
