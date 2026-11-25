//! A workspace VM's daemon serves a socket of its own for the VM's tenant: the tenant's account
//! alone is admitted, as the person's unnamed agent, and every other account is refused by name,
//! the Browser's included though it may enter the directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::{Tenant, serve_tenant_for_test};
use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;
use std::io::{BufRead as _, BufReader};
use std::sync::Arc;
use tilo_actions::remote::{
    ActionProvider as _, Caller, CatalogueQuery, LineClient, device_agent_id,
};

/// The tenant a test serves, in a directory of its own under `root`.
fn tenant(root: &std::path::Path, uid: u32) -> Tenant {
    Tenant {
        uid,
        sockets: root.join(format!("tenant-{uid}")),
    }
}

const REFUSED_TEST: &str =
    "daemon::tenant_tests::a_connection_from_another_account_is_refused_by_name";

/// A connection from an account that is not the tenant's is refused by name, naming both the
/// account that came and the one the socket serves: the directory's group is only the first
/// gate, and the kernel's word about the peer is the one that decides.
#[test]
fn a_connection_from_another_account_is_refused_by_name() {
    let Some(root) = isolated_root(REFUSED_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    // The test's own account is the one that connects, and the tenant is any other.
    let own = crate::seats::own_account();
    let served = own.checked_add(1).expect("no account above this one");
    let tenant = tenant(&root, served);
    std::fs::create_dir_all(&tenant.sockets).unwrap();
    let socket = serve_tenant_for_test(&daemon, &tenant).unwrap();
    let stream = tilo_socket::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).unwrap();
    assert!(
        answer.contains(&format!(
            "uid {own} is not the account it serves (uid {served})"
        )),
        "the refusal names the account that came and the one served: {answer}"
    );
    assert!(
        answer.contains("this socket is the workspace's tenant's alone"),
        "{answer}"
    );
}

const AGENT_TEST: &str =
    "daemon::tenant_tests::the_tenants_connection_is_the_persons_unnamed_agent";

/// A connection from the tenant's account is admitted as the person's unnamed agent: an agent of
/// theirs which acts for them, never the person, so an action the person attends asks them.
#[test]
fn the_tenants_connection_is_the_persons_unnamed_agent() {
    use tilo_action_policy::{Admission, Answer, Invoked, admit};

    let Some(root) = isolated_root(AGENT_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let probe = Arc::new(tilo_actions::remote::TestProvider::new());
    daemon.provide("so.tilo.probe", Arc::clone(&probe) as _);
    let tenant = tenant(&root, crate::seats::own_account());
    std::fs::create_dir_all(&tenant.sockets).unwrap();
    let socket = serve_tenant_for_test(&daemon, &tenant).unwrap();
    let client = LineClient::of_stream(tilo_socket::connect(&socket).unwrap()).unwrap();
    // Whom a client names does not count: the daemon decides by the account connecting.
    let _ = client
        .query(&Caller::local_system(), CatalogueQuery::default())
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the tenant's connection reaches the daemon");
    let caller = probe
        .callers()
        .into_iter()
        .next()
        .expect("the daemon asked the tenant's connection as somebody");
    let person = daemon.actor().to_owned();
    let person_key: [u8; 32] = hex::decode(&person).unwrap().try_into().unwrap();
    assert!(
        caller.acting().is_agent(),
        "the tenant is an agent: {caller:?}"
    );
    assert_eq!(
        caller.acting().acted_for_person(),
        Some(person.as_str()),
        "it acts for the person whose workspace the VM serves: {caller:?}"
    );
    assert_eq!(
        caller.acting().agent_id(),
        Some(device_agent_id(&person_key, super::UNNAMED_AGENT).as_str()),
        "the unnamed agent is the one the device gives it: {caller:?}"
    );
    // An action the person attends asks them: it is never admitted as the person's own act.
    assert_eq!(
        admit(
            Invoked {
                answer: Answer::Allow,
                attended: true,
                outside: false,
                resource: None,
            },
            caller.acting().asker(true),
        ),
        Ok(Admission::AsksFirst),
        "{caller:?}"
    );
}

/// Leave `root`'s profile the device a host placed for `workspace`'s VM records, certified by a
/// host its person granted placing them: the certificate a workspace VM's daemon holds, minted
/// here through the test trust root.
fn confined(workspace: tilo_desktop_model::WorkspaceId) {
    tilo_identity::sources::test_trust::install();
    let keys = tilo_api::calls::device_keys();
    let record = crate::device::services::device_root_record().unwrap();
    let person = tilo_identity::store::load_from_path(record).unwrap();
    let held = tilo_device_identity::persist::read_device_cert(&keys.device_cert).unwrap();
    let cert = crate::device::confined::placed_cert(
        &person,
        held.device_public_key,
        tilo_workspace_service::environment::vm_placed_records(workspace),
        "The VM",
    );
    tilo_device_identity::persist::write_device_cert(&keys.device_cert, &cert).unwrap();
    tilo_device_identity::persist::write_device_cert(
        &keys.device_cert_for_user(&cert.user_public_key),
        &cert,
    )
    .unwrap();
}

const CONFINED_TEST: &str =
    "daemon::tenant_tests::a_confined_daemons_tenant_socket_serves_its_account_alone";

/// A daemon placed for records of one workspace serves its tenant's socket as that account
/// alone: every other account is refused by name, and the tenant is never served the person's
/// connection, which a placed device has none of.
///
/// It admits nothing itself: the tenant's calls, a ticket for a new computer among them, go to
/// its workspace's host, which this fixture's login named none of, so each is refused here and
/// reaches no provider of this device. What the host decides of them (a placed caller, never the
/// person; no ticket; no record its certificate does not name) is asserted on a real host
/// (`device::confined::tests`).
#[test]
fn a_confined_daemons_tenant_socket_serves_its_account_alone() {
    let Some(root) = isolated_root(CONFINED_TEST) else {
        return;
    };
    confined(tilo_desktop_model::WorkspaceId::new());
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let probe = Arc::new(tilo_actions::remote::TestProvider::new());
    daemon.provide("so.tilo.probe", Arc::clone(&probe) as _);
    // No connection of a confined device's is the person's, its own account's included: the
    // workspace it names is its host's, which it does not hold.
    assert!(
        daemon.connection_tree_in(None).is_err(),
        "a confined device has no connection that is the person"
    );

    // Browser's account, or any other, is refused by name, in a directory of its own.
    let own = crate::seats::own_account();
    let served = own.checked_add(1).expect("no account above this one");
    let elsewhere = tenant(&root, served);
    std::fs::create_dir_all(&elsewhere.sockets).unwrap();
    let refused = serve_tenant_for_test(&daemon, &elsewhere).unwrap();
    let stream = tilo_socket::connect(&refused).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).unwrap();
    assert!(
        answer.contains(&format!(
            "uid {own} is not the account it serves (uid {served})"
        )),
        "{answer}"
    );

    // The tenant's own account is answered by the confined device's own tree, which holds no
    // workspace to admit a call in: the refusal reaches no provider, and is never the person's
    // call, which would have run.
    let tenant = tenant(&root, own);
    std::fs::create_dir_all(&tenant.sockets).unwrap();
    let socket = serve_tenant_for_test(&daemon, &tenant).unwrap();
    let client = LineClient::of_stream(tilo_socket::connect(&socket).unwrap()).unwrap();
    let answered = client
        .query(&Caller::local_system(), CatalogueQuery::default())
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the confined daemon answers the tenant");
    let refused = answered.expect_err("the tenant reaches no workspace here");
    let issued = client
        .invoke(
            &Caller::local_system(),
            tilo_actions::remote::InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::device_tickets::PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_actions::remote::device_tickets::RESOURCE,
                        tilo_actions::remote::device_tickets::ISSUE,
                    ),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::new()),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the confined daemon answers the tenant");
    assert!(
        issued.is_err(),
        "a placed device's tenant is issued no ticket: {issued:?}"
    );
    assert!(
        probe.callers().is_empty(),
        "no call was made as the person: {refused:?}"
    );
}
