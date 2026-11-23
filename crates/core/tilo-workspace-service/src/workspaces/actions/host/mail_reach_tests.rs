//! Where this server connects for a workspace's mail: the public internet,
//! and its own network only where its operator lets it call the addresses a
//! member names. A mail server whose name answers with an address of this
//! machine or its network is refused in the words of the one judge of
//! addresses ([`tilo_net::address::public_addresses`]) and nothing is
//! connected to, so nobody maps this server's network or sends a password
//! into it by naming a mail server.

use std::net::{Ipv4Addr, TcpListener};

use serde_json::json;
use tilo_actions::remote::ActionProvider as _;
use tilo_actions::remote::person_accounts;

use super::super::tests::{admit, caller, governed, settled};
use super::accounts_tests::asking;
use crate::workspaces::tests::{governor, test_host};

/// What the judge of addresses says of a name it refuses.
const INSIDE: &str = "answers with an address of this machine or its network";

/// How many connections were made to `listener`, which waits for none.
fn connections(listener: &TcpListener) -> usize {
    std::iter::from_fn(|| listener.accept().ok()).count()
}

/// A member adds a mail account whose two servers are named `localhost`,
/// where a listener of this machine counts who connects. With the operator's
/// setting shut the add is refused in the judge's words and the listener
/// counts nobody; once the operator lets the server call its own network the
/// same add connects, and whatever it is refused for then is not the judge's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mail_server_whose_name_answers_inside_is_refused_and_connected_to_by_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let (workspace, workspace_id, actions) = governed(&workspaces).await;
    let device = admit(&workspaces, &workspace, &owner, 1).await;
    let page = actions.in_workspace_as(workspace_id, &device);
    let caller = caller(&device);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port here");
    listener
        .set_nonblocking(true)
        .expect("a listener that waits for nobody");
    let port = listener.local_addr().expect("its address").port();
    let adding = || {
        asking(
            person_accounts::ADD_MAIL,
            json!({
                "address": "ada@example.test",
                "incomingHost": "localhost",
                "incomingPort": port,
                "outgoingHost": "localhost",
                "outgoingPort": port,
                "password": "a made-up password",
            }),
        )
    };

    let shut = format!("{:?}", settled(page.invoke(&caller, adding())).await);
    assert!(shut.contains(INSIDE), "refused by the judge: {shut}");
    assert_eq!(connections(&listener), 0, "nothing is connected to");

    workspaces.call_private_model_addresses(true);
    let opened = format!("{:?}", settled(page.invoke(&caller, adding())).await);
    assert!(
        !opened.contains(INSIDE),
        "not the judge's refusal: {opened}"
    );
    assert!(
        connections(&listener) >= 1,
        "the operator's own network is connected to: {opened}"
    );

    workspaces.call_private_model_addresses(false);
    let shut_again = format!("{:?}", settled(page.invoke(&caller, adding())).await);
    assert!(
        shut_again.contains(INSIDE),
        "the setting is read at each connection: {shut_again}"
    );
    assert_eq!(connections(&listener), 0, "nothing is connected to");
}
