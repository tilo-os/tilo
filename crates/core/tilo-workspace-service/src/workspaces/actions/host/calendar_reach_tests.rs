//! Where this server connects for a person's calendars: the public internet,
//! and its own network only where its operator lets it call the addresses
//! people name. The address is judged where the connection is made, by the
//! one judge of addresses ([`tilo_net::address::public_addresses`]): when a
//! person adds an account themselves, and every time a kept account is read
//! again. So a calendar server named inside this server's network is
//! connected to by nobody, and neither is one that was added while the
//! operator allowed it and is read after the operator shut that again.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the server handle a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::{Ipv4Addr, TcpListener};

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionProvider as _, InvokeArgument, InvokeRequest, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionAddress, InvocationId};

use super::super::tests::{admit, caller, governed, settled};
use crate::workspaces::person_settings::calendars::{self as held, fake_caldav};
use crate::workspaces::tests::{governor, test_host};

/// The password of the made-up account.
const PASSWORD: &str = "a made-up password";

/// A request of one of the calendars' actions, with `argument` when it has
/// one.
fn asking(action: &str, argument: Value) -> InvokeRequest {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: person_accounts::PROVIDER.to_owned(),
            address: ActionAddress::new(held::RESOURCE, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// How many connections were made to `listener`, which waits for none.
fn connections(listener: &TcpListener) -> usize {
    std::iter::from_fn(|| listener.accept().ok()).count()
}

/// A person adds a calendar account whose server is named `localhost`, where
/// a listener of this machine counts who connects. With the operator's
/// setting shut the add is refused and the listener counts nobody; once the
/// operator lets the server call its own network the same add connects; and
/// shut again, nobody is connected to: the setting is read at each
/// connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_calendar_server_whose_name_answers_inside_is_connected_to_by_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let caller = caller(&device);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port here");
    listener
        .set_nonblocking(true)
        .expect("a listener that waits for nobody");
    let port = listener.local_addr().expect("its address").port();
    let adding = || {
        asking(
            held::ADD,
            json!({
                "serverUrl": format!("http://localhost:{port}/dav/"),
                "username": "ada",
                "password": PASSWORD,
            }),
        )
    };

    let shut = format!("{:?}", settled(page.invoke(&caller, adding())).await);
    assert_eq!(connections(&listener), 0, "nothing is connected to");
    assert!(shut.starts_with("Err("), "the add is refused");

    workspaces.call_private_model_addresses(true);
    let opened = format!("{:?}", settled(page.invoke(&caller, adding())).await);
    assert!(
        connections(&listener) >= 1,
        "the operator's own network is connected to"
    );
    assert!(
        opened.starts_with("Err("),
        "a listener that answers nothing is no calendar server"
    );

    workspaces.call_private_model_addresses(false);
    let shut_again = format!("{:?}", settled(page.invoke(&caller, adding())).await);
    assert_eq!(
        connections(&listener),
        0,
        "the setting is read at each connection: nothing is connected to"
    );
    assert!(shut_again.starts_with("Err("), "the add is refused");
}

/// A calendar account was added on this machine while the operator let the
/// server call its own network, and its server was read. Once the operator
/// shuts that, a read of the kept account asks its server nothing: the
/// address is judged again at the connection, not once at the add.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kept_calendar_is_read_from_nobody_once_the_operator_shuts_its_own_network() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 2).await;
    let page = actions.device_page(workspace_id, &device);
    let caller = caller(&device);
    let calendars = fake_caldav::Server::start("ada", PASSWORD);
    calendars.add_calendar("work", "Work");

    workspaces.call_private_model_addresses(true);
    let adding = asking(
        held::ADD,
        json!({
            "serverUrl": calendars.url(),
            "username": "ada",
            "password": PASSWORD,
            "name": "Mine",
        }),
    );
    let added = format!("{:?}", settled(page.invoke(&caller, adding)).await);
    assert!(
        added.contains("Completed"),
        "her own server takes her sign-in while the operator lets this server call it"
    );
    assert!(
        !calendars.requests().is_empty(),
        "her server was asked while that was allowed"
    );
    // Whether the server hands out what it was asked once or every time, the
    // count before and the count after are read the same way.
    let before = calendars.requests().len();

    workspaces.call_private_model_addresses(false);
    let read = asking(held::SYNC, json!({}));
    let shut = format!("{:?}", settled(page.invoke(&caller, read)).await);
    assert_eq!(
        calendars.requests().len(),
        before,
        "her server is asked nothing"
    );
    assert!(shut.starts_with("Err("), "the read is refused");
}
