//! A new device asks to be the person through a host it shares with their
//! computer: the computer made the pairing link, sees the request naming
//! its invite with the code the device shows, and approves or declines it.
//! An approved device connects as the person; a link admits one device, for
//! ten minutes, and a request no link of this computer's named is not shown.

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tilo_browser_peer::Host;
use tilo_relay_client::Envelope;
use tilo_user_identity::{
    CertificationPolicy, CertificationRequest, DeviceCert, RevocationList, issue_device_cert,
};

use super::device_approvals::RootApproval;
use super::site_join_tests::{Site, computer, held, key, until};
use crate::shell::state::RoomId;

/// The person's root, standing in for the identity store.
struct TestRoot(SigningKey);

impl RootApproval for TestRoot {
    fn user_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn approver(&self) -> Result<Arc<dyn tilo_relay_client::DeviceSigner>, String> {
        Ok(Arc::new(self.0.clone()))
    }

    fn certify(&self, request: &CertificationRequest) -> Result<DeviceCert, String> {
        issue_device_cert(
            request,
            &self.0,
            &CertificationPolicy::browser(),
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| error.to_string())
    }
}

/// The person and invite a pairing link's `pair` secret names.
fn pair_secret(link: &str) -> ([u8; 32], [u8; 16]) {
    let secret = link
        .split_once("pair=")
        .map(|(_, rest)| rest.split('&').next().unwrap_or_default())
        .unwrap();
    let bytes = hex::decode(secret).unwrap();
    (
        bytes[..32].try_into().unwrap(),
        bytes[32..].try_into().unwrap(),
    )
}

/// A new device asking to be `user` under `invite` at `site`'s host, as a
/// page that opened the link does; its code lands in `code`.
fn new_device(
    site: &Site,
    user: [u8; 32],
    invite: [u8; 16],
    device: SigningKey,
    label: &'static str,
    code: Arc<Mutex<Option<String>>>,
) -> tokio::task::JoinHandle<Result<(DeviceCert, DeviceCert), tilo_browser_peer::CertifyError>> {
    let host = Host::Https(site.http());
    site.runtime.spawn(async move {
        tilo_browser_peer::certify(&host, user, label, invite, &device, |shown| {
            *code.lock().unwrap() = Some(shown.to_owned());
        })
        .await
    })
}

#[test]
#[serial_test::serial]
fn the_computer_approves_the_device_its_link_invited_and_it_connects_as_the_person() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    let ada = key(1);
    let (net, tree) = computer(&dir.path().join("computer"), ada.clone(), "Ada's MacBook");
    net.set_pairing_root(Arc::new(TestRoot(ada.clone())));
    let room = RoomId::parse(&site.room).unwrap();
    net.join_site(&site.entry).unwrap();
    held(&net, &tree, room);

    let invite = net.invite_device().unwrap();
    assert!(net.inviting_devices());
    let (user, id) = pair_secret(&invite.link);
    assert_eq!(user, ada.verifying_key().to_bytes());
    assert_eq!(id, invite.id);
    assert!(
        invite.link.contains(&site.room),
        "the link names the workspace both devices reach"
    );

    // A device nobody's link invited asks too: it is never shown.
    let stray_code = Arc::new(Mutex::new(None));
    let stray = new_device(&site, user, [0; 16], key(9), "Unknown device", stray_code);

    let code = Arc::new(Mutex::new(None));
    let phone = key(5);
    let asking = new_device(
        &site,
        user,
        id,
        phone.clone(),
        "Safari on iPhone",
        code.clone(),
    );
    let shown = until("the computer to see the invited device", || {
        let asking = net.devices_asking().unwrap();
        (!asking.is_empty()).then_some(asking)
    });
    assert_eq!(shown.len(), 1, "only the invited device is shown");
    let device = &shown[0];
    assert_eq!(device.label, "Safari on iPhone");
    let displayed = until("the device to show its code", || {
        code.lock().unwrap().clone()
    });
    assert_eq!(device.code, displayed, "both screens show the same code");
    assert!(device.requested_at_ms > 0);
    assert_eq!(device.country, None, "this host knows no country");

    net.answer_device(device.digest, true).unwrap();
    let (cert, host_cert) = site.runtime.block_on(asking).unwrap().unwrap();
    assert_eq!(cert.user_public_key, ada.verifying_key().to_bytes());
    assert_eq!(cert.device_public_key, phone.verifying_key().to_bytes());
    assert_eq!(host_cert, site.host_cert);
    assert!(!net.inviting_devices(), "the link admitted its one device");

    // The phone, now Ada's, comes into the Lounge through its door as a
    // page does, and the host subscribes it as her.
    site.admit_device(&cert, &phone, "Safari on iPhone");
    let client = site
        .runtime
        .block_on(tilo_relay_session::RelayClient::connect_https(
            site.http(),
            site.host_cert.device_public_key,
            cert,
            &phone,
            RevocationList::new(),
        ))
        .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: site.workspace_id,
        })
        .unwrap();
    site.runtime.block_on(tilo_test_env::within(
        "the phone is admitted as Ada",
        async {
            loop {
                match client.recv().await {
                    Some(Envelope::Ready { .. }) => break,
                    Some(Envelope::NotAuthorised { .. }) | None => panic!("the phone was refused"),
                    Some(_) => {}
                }
            }
        },
    ));

    // The link admits no second device.
    let again = new_device(&site, user, id, key(6), "Firefox on Linux", Arc::default());
    let refused = site.runtime.block_on(again).unwrap();
    assert!(
        matches!(&refused, Err(tilo_browser_peer::CertifyError::Denied(reason)) if reason.contains("used already")),
        "{refused:?}"
    );
    stray.abort();
    drop(net);
    site.shutdown();
}

#[test]
#[serial_test::serial]
fn a_declined_device_is_admitted_nowhere_and_an_expired_link_shows_nothing() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    let ada = key(1);
    let (net, tree) = computer(&dir.path().join("computer"), ada.clone(), "Ada's MacBook");
    net.set_pairing_root(Arc::new(TestRoot(ada.clone())));
    let room = RoomId::parse(&site.room).unwrap();
    net.join_site(&site.entry).unwrap();
    held(&net, &tree, room);

    let invite = net.invite_device().unwrap();
    let (user, id) = pair_secret(&invite.link);
    let asking = new_device(&site, user, id, key(5), "Chrome on Android", Arc::default());
    let shown = until("the computer to see the device", || {
        let asking = net.devices_asking().unwrap();
        (!asking.is_empty()).then_some(asking)
    });
    net.answer_device(shown[0].digest, false).unwrap();
    let declined = site.runtime.block_on(asking).unwrap();
    assert!(
        matches!(declined, Err(tilo_browser_peer::CertifyError::Denied(_))),
        "{declined:?}"
    );
    assert!(!net.inviting_devices());

    // A link past its ten minutes shows its device no more.
    let invite = net.invite_device().unwrap();
    let (user, id) = pair_secret(&invite.link);
    let late = new_device(&site, user, id, key(7), "Safari on iPad", Arc::default());
    net.expire_device_invites();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(net.devices_asking().unwrap().is_empty());
    late.abort();
    drop(net);
    site.shutdown();
}
