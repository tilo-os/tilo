//! Where a person's devices are reached while their Tilo is closed: a
//! device hands over its own address and nobody else's; its person reads
//! back which devices are reached and never an address or a key; nobody
//! but the person does either; an address reaches one person; and it goes
//! when its person removes it, when its device's key is revoked and when
//! the push service says it is gone. Every token and key here is made up.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the server a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::person_telling;
use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, ReceiptStatus};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
    sign_revocation_entry,
};

use super::super::tests::{caller, settled};
use super::telling_tests::{ASSISTANT, Setup, agent_of, nobody};
use crate::workspaces::push_addresses::{
    self, AddressList, LIST, Listed, MAX_ADDRESSES, PushEnvironment, PushKind, REGISTER, UNREGISTER,
};
use crate::workspaces::tests::guest_device;

/// The app the made-up tokens were given to.
const APP: &str = "so.tilo.chat";

/// A token as a push service gives one. Made up.
fn token(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

/// A device's key and secret, of the sizes real ones have. Made up.
fn key(n: u8) -> Value {
    let byte = format!("{n:02x}");
    json!({"p256dh": format!("04{}", byte.repeat(64)), "auth": byte.repeat(16)})
}

/// What a device hands over, with the made-up token and key `n`.
fn registration(n: u8) -> Value {
    json!({"kind": "apns", "token": token(n), "environment": "production", "topic": APP, "key": key(n)})
}

/// A device of the person whose root is `root`, as that root certifies
/// one.
fn device_of(root: &SigningKey, label: &str) -> DeviceCert {
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// `root` revokes the key `device`.
fn revocation(root: &SigningKey, device: [u8; 32]) -> RevocationEntry {
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, root).unwrap();
    entry
}

/// What `caller` is answered about the addresses, over the page `device`
/// holds.
async fn ask(
    setup: &Setup,
    device: &DeviceCert,
    caller: &Caller,
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let page = setup.page(setup.shared, device);
    let mut request = super::accounts_tests::asking(action, argument);
    request.target = person_telling::target(push_addresses::RESOURCE, action);
    let receipt = settled(page.invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("the server answers at once, not {other:?}"),
    }
}

/// What the person is answered on `device`, a device of theirs.
async fn person(setup: &Setup, device: &DeviceCert, action: &str, argument: Value) -> Value {
    ask(setup, device, &caller(device), action, argument)
        .await
        .unwrap()
}

/// Which devices are reached, as their person reads it on `device`.
async fn listed(setup: &Setup, device: &DeviceCert) -> Vec<Listed> {
    let answer = person(setup, device, LIST, json!({})).await;
    serde_json::from_value::<AddressList>(answer)
        .unwrap()
        .addresses
}

/// The tokens the server keeps for the person `root`, the newest first.
fn tokens(setup: &Setup, root: &[u8; 32]) -> Vec<String> {
    setup
        .workspaces
        .push_addresses(root)
        .into_iter()
        .map(|address| address.token)
        .collect()
}

fn named(device: &DeviceCert) -> String {
    hex::encode(device.device_public_key)
}

#[tokio::test]
async fn a_device_hands_over_its_own_address_and_its_person_reads_back_no_address() {
    let setup = Setup::new().await;
    let root = SigningKey::from_bytes(&rand::random());
    let (phone, laptop) = (device_of(&root, "phone"), device_of(&root, "laptop"));
    let ada = phone.user_public_key;

    // The phone hands over where it is reached. What it is answered names
    // the device the transport certified, and holds no address and no key.
    let answer = person(&setup, &phone, REGISTER, registration(1)).await;
    let said = answer.to_string();
    assert!(!said.contains(&token(1)), "{said}");
    assert!(!said.contains(&"01".repeat(16)), "{said}");
    let kept: AddressList = serde_json::from_value(answer).unwrap();
    assert_eq!(kept.addresses.len(), 1);
    let own = kept.addresses.first().unwrap();
    assert_eq!(own.device, named(&phone));
    assert!(own.this_device);
    assert_eq!(own.kind, PushKind::Apns);
    assert_eq!(
        (own.environment, own.topic.as_str()),
        (Some(PushEnvironment::Production), APP)
    );
    // What sends a push reads the whole of it.
    assert_eq!(tokens(&setup, &ada), [token(1)]);

    // Handed over again by the same device, an address takes the place of
    // the one before.
    person(&setup, &phone, REGISTER, registration(2)).await;
    assert_eq!(tokens(&setup, &ada), [token(2)]);

    // Her laptop is another device of hers. Each keeps its own, the newest
    // first, and each reads which one is its own.
    person(&setup, &laptop, REGISTER, registration(3)).await;
    let read: Vec<(String, bool)> = listed(&setup, &phone)
        .await
        .into_iter()
        .map(|listed| (listed.device, listed.this_device))
        .collect();
    assert_eq!(read, [(named(&laptop), false), (named(&phone), true)]);

    // A request names no device to hand over for: an address is the asking
    // device's, as the transport certified it.
    let mut for_another = registration(4);
    for_another
        .as_object_mut()
        .unwrap()
        .insert("device".into(), json!(named(&laptop)));
    let refused = ask(&setup, &phone, &caller(&phone), REGISTER, for_another)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ActionCallError::InvalidRequest { .. }),
        "{refused:?}"
    );

    // What is no token, no app's name or no key is refused, and so is an
    // environment the push service does not have. Nothing of it is kept.
    let with = |token: &str, topic: &str, key: Value, environment: &str| json!({"kind": "apns", "token": token, "environment": environment, "topic": topic, "key": key});
    let short_key = json!({"p256dh": "04", "auth": "05"});
    for bad in [
        with("../../3/device/x", APP, key(5), "production"),
        with(&token(5), "so.tilo/../x", key(5), "production"),
        with(&token(5), APP, short_key, "production"),
    ] {
        let refused = ask(&setup, &phone, &caller(&phone), REGISTER, bad)
            .await
            .unwrap_err();
        assert!(
            matches!(refused, ActionCallError::Refused { .. }),
            "{refused:?}"
        );
    }
    let refused = ask(
        &setup,
        &phone,
        &caller(&phone),
        REGISTER,
        with(&token(5), APP, key(5), "staging"),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(refused, ActionCallError::InvalidRequest { .. }),
        "{refused:?}"
    );
    assert_eq!(tokens(&setup, &ada), [token(3), token(2)]);

    // She takes her phone's address back on the phone; the laptop's stays.
    person(&setup, &phone, UNREGISTER, json!({})).await;
    assert_eq!(tokens(&setup, &ada), [token(3)]);
    // And from the phone she takes back the laptop's, naming it as the
    // list does.
    person(
        &setup,
        &phone,
        UNREGISTER,
        json!({"device": named(&laptop)}),
    )
    .await;
    assert!(listed(&setup, &phone).await.is_empty());
    assert!(tokens(&setup, &ada).is_empty());
}

#[tokio::test]
async fn where_a_persons_devices_are_reached_is_theirs_alone() {
    let setup = Setup::new().await;
    let ada = &setup.ada;
    person(&setup, ada, REGISTER, registration(1)).await;

    // Her own agent, over her own device, hands over nothing, reads
    // nothing and removes nothing. Nor does an agent that says it acts for
    // her over a device that is not hers, an agent no device stands
    // behind, an app, or a bot installed for her.
    let (_, bo) = guest_device();
    let strangers = [
        ("her agent", (ada, agent_of(ada))),
        (
            "an agent over a device that is not its person's",
            (
                &bo,
                Caller::delegated_agent(ASSISTANT, ada.user_public_key, bo.device_public_key),
            ),
        ),
        (
            "an agent no device stands behind",
            (ada, Caller::local_agent(ASSISTANT, "ada")),
        ),
        ("an app", (ada, Caller::local_app("so.tilo.chat"))),
        (
            "a bot installed for her",
            (
                ada,
                Caller::bot("a-bot", ada.user_public_key, ada.device_public_key),
            ),
        ),
    ];
    for (what, (page, stranger)) in &strangers {
        for (action, argument) in [
            (REGISTER, registration(9)),
            (LIST, json!({})),
            (UNREGISTER, json!({})),
        ] {
            nobody(what, ask(&setup, page, stranger, action, argument).await);
        }
    }
    assert_eq!(tokens(&setup, &ada.user_public_key), [token(1)]);

    // Another person hands over their own and reads their own. Nothing of
    // hers is theirs to read or to remove, also when they name her device.
    person(&setup, &bo, REGISTER, registration(2)).await;
    let theirs = listed(&setup, &bo).await;
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs.first().unwrap().device, named(&bo));
    person(&setup, &bo, UNREGISTER, json!({"device": named(ada)})).await;
    assert_eq!(tokens(&setup, &ada.user_public_key), [token(1)]);
    assert_eq!(tokens(&setup, &bo.user_public_key), [token(2)]);

    // An address reaches one person. An app whose person signed out and
    // another signed in hands the same token over for the other: from then
    // on it is theirs, and nothing of the first person's is sent to it.
    person(&setup, &bo, REGISTER, registration(1)).await;
    assert!(tokens(&setup, &ada.user_public_key).is_empty());
    assert_eq!(tokens(&setup, &bo.user_public_key), [token(1)]);

    // An action the addresses do not have is nobody's either.
    nobody(
        "an action that is not there",
        ask(&setup, ada, &caller(ada), "tell", json!({"words": "x"})).await,
    );
}

#[tokio::test]
async fn an_address_goes_with_its_devices_key_and_when_the_push_service_says_so() {
    let setup = Setup::new().await;
    // The server of these tests is certified for the person whose root
    // this is, so it takes what that root revokes.
    let root = SigningKey::from_bytes(&[1; 32]);
    let (phone, laptop) = (device_of(&root, "phone"), device_of(&root, "laptop"));
    let ada = phone.user_public_key;
    person(&setup, &phone, REGISTER, registration(1)).await;
    person(&setup, &laptop, REGISTER, registration(2)).await;

    // Another root has nothing to say about her phone.
    let other = SigningKey::from_bytes(&rand::random());
    let _ = setup
        .workspaces
        .take_revocations(vec![revocation(&other, phone.device_public_key)])
        .await;
    assert_eq!(tokens(&setup, &ada), [token(2), token(1)]);

    // Her own root revokes the phone's key: where the phone was reached is
    // forgotten with it. What each workspace carries of a revocation is
    // its own test's.
    let _ = setup
        .workspaces
        .take_revocations(vec![revocation(&root, phone.device_public_key)])
        .await;
    assert_eq!(tokens(&setup, &ada), [token(2)]);

    // The push service says the laptop's address is gone. It is forgotten,
    // unless the laptop handed it over after the moment the service names.
    let handed_over = setup.workspaces.push_addresses(&ada).first().unwrap().at_ms;
    setup
        .workspaces
        .forget_push_address(&ada, &token(2), Some(handed_over.saturating_sub(1)));
    assert_eq!(tokens(&setup, &ada), [token(2)]);
    setup
        .workspaces
        .forget_push_address(&ada, &token(2), Some(handed_over));
    assert!(tokens(&setup, &ada).is_empty());
}

#[tokio::test]
async fn a_few_addresses_are_kept_and_what_a_push_showed_is_taken_back_once() {
    let setup = Setup::new().await;
    let root = SigningKey::from_bytes(&rand::random());
    let ada = root.verifying_key().to_bytes();

    // One more device than a person keeps addresses for: the newest are
    // kept and the oldest is let go.
    let devices: Vec<DeviceCert> = (0..=MAX_ADDRESSES)
        .map(|_| device_of(&root, "device"))
        .collect();
    for (n, device) in (1_u8..).zip(&devices) {
        person(&setup, device, REGISTER, registration(n)).await;
    }
    let kept = tokens(&setup, &ada);
    assert_eq!(kept.len(), MAX_ADDRESSES);
    assert_eq!(kept.first(), Some(&token(9)));
    assert!(!kept.contains(&token(1)));

    // A push showed one thing on two devices and another on one of them.
    let workspaces = &setup.workspaces;
    workspaces.push_shown(&ada, &[token(8), token(9)], "thing-1");
    workspaces.push_shown(&ada, &[token(9)], "thing-2");
    // An app that starts again hands the same address over again, and what
    // is remembered of its device stays.
    person(&setup, devices.last().unwrap(), REGISTER, registration(9)).await;

    // The first is taken back from both devices, once.
    let ids = ["thing-1".to_owned(), "thing-0".to_owned()];
    let mut taken: Vec<(String, Vec<String>)> = workspaces
        .push_taken_back(&ada, &ids, PushKind::Apns)
        .into_iter()
        .map(|(address, ids)| (address.token, ids))
        .collect();
    taken.sort();
    assert_eq!(
        taken,
        [
            (token(8), vec!["thing-1".to_owned()]),
            (token(9), vec!["thing-1".to_owned()])
        ]
    );
    assert!(
        workspaces
            .push_taken_back(&ada, &ids, PushKind::Apns)
            .is_empty()
    );
    // The second is still remembered, on the one device that showed it.
    let taken = workspaces.push_taken_back(&ada, &["thing-2".to_owned()], PushKind::Apns);
    assert_eq!(taken.len(), 1);
    assert_eq!(taken.first().unwrap().0.token, token(9));
}

/// A device hands over the web push endpoint it subscribed with, in place of Apple's token: the
/// same record and the same action keep it, its person reads back the kind and never the
/// endpoint, and what reaches it is the endpoint. One that is no `https` address, or that names
/// another device, is refused, and nothing of it is kept.
#[tokio::test]
async fn a_web_push_address_is_an_https_endpoint_of_the_asking_device() {
    let setup = Setup::new().await;
    let root = SigningKey::from_bytes(&rand::random());
    let ada = root.verifying_key().to_bytes();
    let phone = device_of(&root, "Pixel");
    let endpoint = "https://push.example/endpoint/abc";
    let web = |endpoint: &str| json!({"kind": "web-push", "endpoint": endpoint, "key": key(1)});

    let answer = person(&setup, &phone, REGISTER, web(endpoint)).await;
    let said = answer.to_string();
    assert!(
        !said.contains(endpoint),
        "the person reads no address: {said}"
    );
    let listed = listed(&setup, &phone).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].kind, listed[0].device.as_str()),
        (PushKind::WebPush, named(&phone).as_str())
    );
    assert!(listed[0].this_device);
    // What sends a push reads the endpoint, and a push's showing is remembered by it.
    let kept = setup.workspaces.push_addresses(&ada);
    assert_eq!(kept[0].kind, PushKind::WebPush);
    assert_eq!(kept[0].reached_as(), endpoint);
    assert!(kept[0].token.is_empty());
    setup
        .workspaces
        .push_shown(&ada, &[endpoint.to_owned()], "thing-1");
    let taken = setup
        .workspaces
        .push_taken_back(&ada, &["thing-1".to_owned()], PushKind::WebPush);
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].0.reached_as(), endpoint);

    // One that is no `https` address names no push service; one naming another device names
    // what an address never names.
    let kept_before = setup.workspaces.push_addresses(&ada).len();
    for refused in [
        web("http://push.example/endpoint/abc"),
        web("push.example/endpoint/abc"),
        web(""),
        json!({"kind": "web-push", "endpoint": endpoint, "key": key(1),
               "device": named(&phone)}),
        json!({"kind": "web-push", "endpoint": endpoint}),
    ] {
        let answer = ask(&setup, &phone, &caller(&phone), REGISTER, refused).await;
        assert!(answer.is_err(), "an address not one was kept: {answer:?}");
    }
    assert_eq!(setup.workspaces.push_addresses(&ada).len(), kept_before);
    // A device's own endpoint is forgotten with its key, and when the push service says so.
    setup.workspaces.forget_push_address(&ada, endpoint, None);
    assert!(setup.workspaces.push_addresses(&ada).is_empty());
}
