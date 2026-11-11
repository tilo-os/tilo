//! Logging an agent's computer in with the code a person's tab copied
//! ("Set up with your agent"): the code is a pairing link to that tab, the
//! computer makes a device key of its own and asks the workspace's host to
//! have the person's root certify it (`login::sign_in`), the tab approves
//! the one request naming its invite, and the computer enters the workspace
//! as that person (`login::enter`), its session the agent's run. The root
//! never leaves the tab.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one login, in order"
)]

#[path = "support/participant.rs"]
mod participant;
mod support;

tilo_automerge_isolate::test_worker!();

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_relay_client::login::PairingLink;
use tilo_relay_client::login::{Door, sign_in};
use tilo_relay_protocol::web::API_PREFIX;
use tilo_session_host::device::{Device, run_as_agent};
use tilo_session_host::invite::guest_certificate;
use tilo_session_host::{CertifyError, answer_pairing, pending_pairings};
use tilo_user_identity::{CertificationPolicy, DeviceCapabilities, UserIdentity};

use support::{door, enrol, enter as enter_as, start_workspace_host, wait_for};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_logs_in_with_the_tabs_code_and_shows_in_the_workspace_as_the_person() {
    // Alice's host keeps the Lounge; its door lets people in.
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let lounge = host
        .relay
        .workspaces()
        .unwrap()
        .create("Lounge")
        .await
        .unwrap();
    // Listed, so its door lets people in, as the site's entry names it.
    host.relay
        .workspaces()
        .unwrap()
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_relay_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let https = host.https();
    let approver = tilo_relay_client::MemberApprover::new(host.iroh());

    // Bob's tab holds his root and came in through the door.
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let tab_key = new_key();
    let tab_cert = guest_certificate(
        &bob,
        tab_key.verifying_key().to_bytes(),
        "Firefox on Linux",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let tab = enter_as(
        &host,
        door(&host, &alice, &host_device, &lounge.workspace, user).await,
        &tab_cert,
        &tab_key,
        &host_device.cert,
        lounge.workspace_id,
    )
    .await;

    // The tab opened an invite and copied its code, as identity-actions.mjs
    // `connectAgent` writes it.
    let invite = [0x7a_u8; 16];
    let code = format!(
        "https://tilo.sh/#/@{}/w/{}?pair={}{}",
        host.web.addr,
        lounge.workspace,
        hex::encode(user),
        hex::encode(invite),
    );
    let link = PairingLink::parse(&code).unwrap();
    assert_eq!(link.user_public_key, user);
    assert_eq!(link.invite, Some(invite));
    assert_eq!(link.workspace_id(), lounge.workspace);

    // The agent's computer logs in with it.
    let cli_key = new_key();
    let asking = tokio::spawn({
        let https = https.clone();
        let link = link.clone();
        let key = cli_key.clone();
        async move {
            sign_in(
                &*https,
                &link,
                "Claude Code",
                "Claude Code on build-box",
                &key,
                |_| {},
            )
            .await
        }
    });

    // The tab approves the request that names its invite, and only that one.
    let waiting = wait_for("the agent's request to wait for Bob's root", || async {
        pending_pairings(&approver, &bob)
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.request.invite == invite)
    })
    .await;
    assert_eq!(
        waiting.request.device_public_key,
        cli_key.verifying_key().to_bytes()
    );
    assert_eq!(waiting.request.device_label, "Claude Code on build-box");
    answer_pairing(&approver, &bob, waiting.request_digest, true)
        .await
        .unwrap();
    let signed = tilo_test_env::within("the login", asking)
        .await
        .unwrap()
        .unwrap();
    assert!(
        pending_pairings(&approver, &bob).await.unwrap().is_empty(),
        "an answered code no longer waits: it worked once"
    );

    // The computer is a member device of Bob's for thirty days, no more,
    // and pinned Alice's host.
    assert_eq!(signed.cert.user_public_key, user);
    assert_eq!(
        signed.cert.device_public_key,
        cli_key.verifying_key().to_bytes()
    );
    assert_eq!(signed.cert.capabilities, DeviceCapabilities::member());
    assert!(!signed.cert.capabilities.can_admit_devices);
    assert_eq!(
        signed
            .cert
            .expires_at_ns
            .unwrap()
            .checked_sub(signed.cert.issued_at_ns),
        Some(CertificationPolicy::browser().lifetime_ns)
    );
    assert_eq!(signed.host_cert, host_device.cert);

    // It enters the Lounge as Bob, through the door as the page would, its
    // session the agent's run.
    let door = Door {
        path: format!("{API_PREFIX}/directory/{}/", lounge.workspace),
    };
    let agent = Device::enter(Some(&door), &signed, Arc::new(cli_key.clone()))
        .await
        .unwrap();
    assert_eq!(agent.workspace_id(), lounge.workspace_id);
    assert_eq!(
        run_as_agent(agent.session(), "Claude Code").await,
        Ok(()),
        "its session is already the agent's run, and says so again"
    );
    assert!(
        run_as_agent(agent.session(), "Codex").await.is_err(),
        "a session handed to one agent is never another's"
    );

    // A code naming a root this host serves nobody under logs nothing in:
    // the host turns the request away, and Bob's tab never sees it.
    let stranger = new_key();
    let mut other = link.clone();
    other.user_public_key = stranger.verifying_key().to_bytes();
    let stray = sign_in(
        &*https,
        &other,
        "Codex",
        "Codex on laptop",
        &new_key(),
        |_| {},
    )
    .await;
    assert!(
        matches!(
            stray,
            Err(CertifyError::Transport(_) | CertifyError::Denied(_))
        ),
        "{:?}",
        stray.as_ref().map(|signed| &signed.label)
    );
    assert!(pending_pairings(&approver, &bob).await.unwrap().is_empty());

    agent.close();
    tab.close();
    drop(host.web);
    host.relay.shutdown().await;
}
