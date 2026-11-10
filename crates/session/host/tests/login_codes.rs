//! A host's login codes and the person's stand-in, through the one client of them
//! (`tilo_relay_client::login_code`) that both the tab (through the runtime) and `tilo login` ask
//! with: the tab makes a code and follows it, the computer resolves it, reports its steps and
//! claims a sign-in code, and the root keeps and retires its stand-in.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the test holds its host for the length of the test, and shuts it down at the end"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; the test is one login, in order"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_relay_client::login_code;
use tilo_relay_protocol::login_code::{Report, Step};
use tilo_session_host::invite::guest_certificate;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{door, enrol, enter as enter_as, start_workspace_host};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tab_and_a_computer_go_through_a_code_with_the_one_client() {
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
    let https = host.https();

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

    // Setting up: the code names the tab's link, the computer resolves it once and reports.
    let link = format!(
        "https://tilo.sh/#/@{}/w/{}?pair={}{}",
        host.web.addr,
        lounge.workspace,
        hex::encode(user),
        hex::encode([0x7a_u8; 16]),
    );
    let made = login_code::create(&*https, Some(link.clone()))
        .await
        .unwrap();
    let waiting = login_code::follow(&*https, &made.follow).await.unwrap();
    assert!(!waiting.resolved);
    let resolved = login_code::resolve(&*https, &made.code).await.unwrap();
    assert_eq!(resolved.link.as_deref(), Some(link.as_str()));
    login_code::report(
        &*https,
        &Report {
            report: resolved.report.clone(),
            step: Step::LoggedIn,
            agent: Some("Claude Code".into()),
            detail: None,
        },
    )
    .await
    .unwrap();
    let polled = login_code::follow(&*https, &made.follow).await.unwrap();
    assert!(polled.resolved);
    assert_eq!(polled.agent.as_deref(), Some("Claude Code"));
    assert!(polled.steps.contains(&Step::LoggedIn), "{polled:?}");
    // A code works once: resolving it again finds nothing, said as its kind.
    let spent = login_code::resolve(&*https, &made.code).await.unwrap_err();
    assert_eq!(spent.code(), "unknown", "{spent:?}");

    // A sign-in needs the person's stand-in here, which the root keeps: the host makes a key and
    // the root certifies it.
    let key = login_code::keep_stand_in(&*https, &bob).await.unwrap();
    assert_eq!(key.len(), 64);
    assert_eq!(
        login_code::keep_stand_in(&*https, &bob).await.unwrap(),
        key,
        "asked again, the host names the same key"
    );

    // Signing in: a device of Bob's claims the code the fresh tab made.
    let asking = login_code::create(&*https, None).await.unwrap();
    let resolved = login_code::resolve(&*https, &asking.code).await.unwrap();
    assert!(resolved.link.is_none());
    let laptop = new_key();
    let laptop_cert = guest_certificate(
        &bob,
        laptop.verifying_key().to_bytes(),
        "Claude Code on build-box",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let postcard = hex::encode(postcard::to_allocvec(&laptop_cert).unwrap());
    login_code::claim(
        &*https,
        &resolved.report,
        postcard.clone(),
        &laptop,
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let claimed = login_code::follow(&*https, &asking.follow)
        .await
        .unwrap()
        .claimed
        .expect("the tab learns whom it was claimed for");
    assert_eq!(claimed.user, hex::encode(user));
    assert_eq!(claimed.by, "Claude Code on build-box");

    // A claim signed by another key than the certificate's is refused.
    let again = login_code::create(&*https, None).await.unwrap();
    let resolved = login_code::resolve(&*https, &again.code).await.unwrap();
    let forged = login_code::claim(
        &*https,
        &resolved.report,
        postcard,
        &new_key(),
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap_err();
    assert_ne!(forged.code(), "unreachable", "{forged:?}");

    // Kept another way now: the host forgets the stand-in.
    login_code::retire_stand_in(&*https, &bob).await.unwrap();

    tab.close();
    drop(host.web);
    host.relay.shutdown().await;
}
