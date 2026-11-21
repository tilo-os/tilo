//! A host's login codes, through the one client of them
//! (`tilo_server_client::login_code`) that both the tab (through the runtime) and `tilo login` ask
//! with: the tab makes a code and follows it, the computer resolves it, reports its steps and
//! claims a sign-in code as one of the person's devices.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one login, in order; the host lives until it ends"
)]

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_enrollment::certification::{VouchError, vouch_with_code};
use tilo_enrollment::requester::certify;
use tilo_server_client::invite::guest_certificate;
use tilo_server_client::{DeviceSigner, login_code};
use tilo_server_protocol::login_code::{ClaimRefused, Report, Step};
use tilo_user_identity::{CertificationPolicy, DeviceCapabilities, UserIdentity};

use crate::support::{enrol, enter as enter_as, entry_grant, start_workspace_host};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tab_and_a_computer_go_through_a_code_with_the_one_client() {
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let lounge = host
        .server
        .workspaces()
        .unwrap()
        .create("Lounge")
        .await
        .unwrap();
    let https = host.https();

    // Bob's tab holds his root and came in through the entry.
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
        entry_grant(&host, &alice, &host_device, &lounge.workspace, user).await,
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
    // Only offered: the host keeps it once the tab follows it with the offer, as it does when
    // its person copies the prompt.
    let unkept = login_code::resolve(&*https, &made.code).await.unwrap_err();
    assert_eq!(unkept.code(), "unknown", "{unkept:?}");
    let offer = |made: &tilo_server_protocol::login_code::Created, link: Option<String>| {
        tilo_server_protocol::login_code::Offer {
            code: made.code.clone(),
            expires_at_ms: made.expires_at_ms,
            link,
        }
    };
    let waiting = login_code::follow(
        &*https,
        &made.follow,
        Some(offer(&made, Some(link.clone()))),
    )
    .await
    .unwrap();
    assert!(!waiting.resolved && !waiting.expired);
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
    let polled = login_code::follow(&*https, &made.follow, None)
        .await
        .unwrap();
    assert!(polled.resolved);
    assert_eq!(polled.agent.as_deref(), Some("Claude Code"));
    assert!(polled.steps.contains(&Step::LoggedIn), "{polled:?}");
    // A code works once: resolving it again finds nothing, said as its kind.
    let spent = login_code::resolve(&*https, &made.code).await.unwrap_err();
    assert_eq!(spent.code(), "unknown", "{spent:?}");

    // Signing in: a device of Bob's claims the code the fresh tab made; a
    // certificate one of his devices signed is no device itself, and is
    // refused, saying why.
    let asking = login_code::create(&*https, None).await.unwrap();
    login_code::follow(&*https, &asking.follow, Some(offer(&asking, None)))
        .await
        .unwrap();
    let resolved = login_code::resolve(&*https, &asking.code).await.unwrap();
    assert!(resolved.link.is_none());
    let laptop = new_key();
    let member_cert = guest_certificate(
        &bob,
        laptop.verifying_key().to_bytes(),
        "Claude Code on build-box",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let signed_in_before = new_key();
    let mut chained = member_cert.clone();
    chained.device_public_key = signed_in_before.verifying_key().to_bytes();
    chained.issued_at_ns = tilo_clock::unix_now_ns();
    chained.expires_at_ns = Some(
        chained
            .issued_at_ns
            .saturating_add(CertificationPolicy::browser().lifetime_ns),
    );
    chained.issuer = Some(Box::new(member_cert.clone()));
    tilo_user_identity::sign_device_cert(&mut chained, &laptop).unwrap();
    let chained = login_code::claim(
        &*https,
        &resolved.report,
        hex::encode(postcard::to_allocvec(&chained).unwrap()),
        &signed_in_before,
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        chained.claim_refused(),
        Some(ClaimRefused::NotADevice),
        "{chained:?}"
    );
    let forged = vouch_with_code(
        &*https,
        &resolved.report,
        &new_key(),
        &member_cert,
        None,
        || panic!("the server accepted a claim signed by a different device"),
    )
    .await
    .unwrap_err();
    let VouchError::Claim(forged) = forged else {
        panic!("the signed claim should have been refused: {forged:?}")
    };
    assert_eq!(
        forged.claim_refused(),
        Some(ClaimRefused::Signature),
        "{forged:?}"
    );

    for root in [None, Some(&bob as &dyn DeviceSigner)] {
        let asking = login_code::create(&*https, None).await.unwrap();
        login_code::follow(&*https, &asking.follow, Some(offer(&asking, None)))
            .await
            .unwrap();
        let resolved = login_code::resolve(&*https, &asking.code).await.unwrap();
        let (claimed, claiming) = tokio::sync::oneshot::channel();
        let vouch = vouch_with_code(
            &*https,
            &resolved.report,
            &laptop,
            &member_cert,
            root,
            || {
                claimed.send(()).unwrap();
            },
        );
        let fresh_tab = async {
            claiming.await.unwrap();
            let polled = login_code::follow(&*https, &asking.follow, None)
                .await
                .unwrap();
            let claimed = polled
                .claimed
                .expect("the tab learns whom it was claimed for");
            assert_eq!(claimed.user, hex::encode(user));
            assert_eq!(claimed.by, "Claude Code on build-box");
            let invite = hex::decode(claimed.invite).unwrap().try_into().unwrap();
            certify(
                &host.iroh(),
                user,
                "Returning browser",
                invite,
                &new_key(),
                |_| {},
            )
            .await
            .unwrap()
        };
        let (approved, (cert, server_cert, label)) =
            tilo_test_env::within("sign in the returning browser", async {
                tokio::join!(vouch, fresh_tab)
            })
            .await;
        approved.unwrap();
        assert_eq!(cert.user_public_key, user);
        assert_eq!(cert.capabilities, DeviceCapabilities::member());
        assert_eq!(
            cert.expires_at_ns,
            Some(
                cert.issued_at_ns
                    .saturating_add(CertificationPolicy::browser().lifetime_ns)
            ),
        );
        assert_eq!(
            cert.issuer.as_deref(),
            root.is_none().then_some(&member_cert)
        );
        assert_eq!(
            server_cert.device_public_key,
            host_device.cert.device_public_key
        );
        assert!(label.starts_with("Returning browser"));
        let polled = login_code::follow(&*https, &asking.follow, None)
            .await
            .unwrap();
        assert!(polled.steps.contains(&Step::Approved), "{polled:?}");
    }

    tab.close();
    drop(host.web);
    host.server.shutdown().await;
}
