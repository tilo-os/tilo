//! A computer that does not keep its person's root sets up its boot key for disk unlock through one
//! of theirs that does: the root device asks the person which computer and which key, their yes
//! certifies the boot key and the computer's disk unlocks with the half it published, their no
//! leaves it locked and says so, and with no root device reachable the computer is told so before
//! anything waits.
//!
//! The two devices are the person's laptop, which keeps their root in this account's identity
//! store, and their desk, on its own endpoint, which keeps none. Both run the daemon's own
//! dispatch of the person's device actions over the person's workspace, and the desk sets up its
//! unlock through the daemon's `setUpUnlock`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::asked::{Asked, AskedKeeping, AskedRecords as _, Offer, Resolve};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_device_identity::login::{
    NO_ROOT_DEVICE, PROVIDER, RELEASE_HALF, RESOURCE, UNLOCK_DECLINED,
};
use tilo_invocations::Invocations;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_remote_actions::asked::AskedStore;
use tilo_unlock_protocol::published::UnlockFabricState;
use tilo_unlock_protocol::{EnrolRequest, EnrolResponse, HALF_LEN, Release, SlotId};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert, verify_device_cert};
use tilo_workspace_service::workspaces::person_workspace::{self, devices};

use super::asked::DeviceAsked;
use super::boot_unlock::{BootUnlock, UNLOCK_FACT};
use super::ticket::{OnThisDevice, Tickets};
use super::ticket_tests::{endpoint, no_root, root_in, serve};

/// How long a step the devices take between them is given.
const WITHIN: Duration = Duration::from_secs(30);

/// One device's dispatch of the person's device actions, keeping what waits in their workspace.
fn dispatch(
    copies: &tilo_workspace_service::workspaces::Workspaces,
    key: [u8; 32],
) -> (Arc<AskedStore>, Arc<Invocations>, Arc<DeviceAsked>) {
    let keeping: Arc<dyn AskedKeeping> = copies.asked_keeping();
    let store = AskedStore::opened(None, Some(keeping), Vec::new()).unwrap();
    store.asks_on(&key);
    let invocations: Arc<Invocations> = Arc::default();
    let asked = DeviceAsked::new(
        Arc::clone(&store),
        Arc::clone(&invocations),
        Box::new(|_| Err("nothing waits here to be sent again".to_owned())),
    );
    (store, invocations, asked)
}

/// A boot of the desk's: its boot key, the request `tilo-auth sibling enrol` hands the daemon
/// for it, and the half it keeps.
struct Boot {
    key: SigningKey,
    half: [u8; HALF_LEN],
    args: Value,
}

fn a_boot(keyslot: u16) -> Boot {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let mut half = [0; HALF_LEN];
    getrandom::fill(&mut half).unwrap();
    let slot = SlotId {
        volume_uuid: [0x5a; 16],
        keyslot,
    };
    let request = EnrolRequest::new(&key, slot, None, None).unwrap();
    let args = json!({
        "request": hex::encode(postcard::to_allocvec(&request).unwrap()),
        "half": hex::encode(half),
    });
    Boot { key, half, args }
}

/// How the desk's set-up `setting_up` ends once `answering`, the person's answer on the laptop,
/// was given: a set-up that ends before they answered fails the test with how it ended.
async fn answered(
    setting_up: impl std::future::Future<Output = Result<Value, super::key_use::Refusal>>,
    answering: impl std::future::Future<Output = ()>,
) -> Result<Value, super::key_use::Refusal> {
    tokio::pin!(setting_up);
    tokio::select! {
        biased;
        ended = &mut setting_up => {
            panic!("the desk's set-up ended before the person answered: {ended:?}")
        }
        () = answering => {}
    }
    setting_up.await
}

/// What the person is asked on the laptop, once the desk's call reached it there.
async fn asked_on_the_laptop(laptop: &DeviceAsked, person: &[u8; 32]) -> Asked {
    let started = Instant::now();
    loop {
        if let Some(asked) = laptop.store().asked_of(person).into_iter().next() {
            return asked;
        }
        assert!(
            started.elapsed() < WITHIN,
            "the person is never asked on the laptop"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The desk's disk-unlock enrolments, as the person's devices record carries them.
async fn published(
    copies: &tilo_workspace_service::workspaces::Workspaces,
    person: &[u8; 32],
    desk: [u8; 32],
) -> UnlockFabricState {
    devices::of(copies, person)
        .await
        .devices
        .get(&tilo_resource::Id::from_raw(hex::encode(desk)))
        .and_then(|device| device.facts.get(UNLOCK_FACT))
        .and_then(|value| hex::decode(value).ok())
        .and_then(|bytes| postcard::from_bytes(&bytes).ok())
        .unwrap_or_default()
}

/// The person's `releaseHalf` on the laptop for a boot of the desk's enrolled as `enrolment`:
/// the half, sealed to the boot's ephemeral key and opened with it as early boot opens it.
async fn released(
    laptop: &Tickets,
    person: &Caller,
    enrolment: &tilo_unlock_protocol::published::UnlockEnrolment,
) -> [u8; HALF_LEN] {
    let ephemeral = tilo_seal::SecretKey::generate().unwrap();
    let digest = [0x3c; 32];
    let args = json!({
        "cert": hex::encode(postcard::to_allocvec(&enrolment.cert).unwrap()),
        "sealed_half": hex::encode(postcard::to_allocvec(&enrolment.sealed_half).unwrap()),
        "request_digest": hex::encode(digest),
        "ephemeral": hex::encode(ephemeral.public_key().unwrap().to_bytes()),
    });
    let Value::Object(argument) = args else {
        panic!("a JSON object literal is an object")
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, RELEASE_HALF),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = laptop
        .invoke(&request, person)
        .await
        .expect("the release is answered")
        .expect("the laptop releases the desk's half");
    let output = match receipt.status {
        tilo_actions::remote::ReceiptStatus::Completed { output } => output.value,
        other => panic!("the release did not end at once: {other:?}"),
    };
    let release: Release =
        postcard::from_bytes(&hex::decode(output.unwrap()["release"].as_str().unwrap()).unwrap())
            .unwrap();
    *tilo_unlock_protocol::open_release(&ephemeral, &digest, &release)
        .expect("the boot opens the release")
        .expose()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one computer's enrolment through another, its yes, its no and nobody to ask, read \
              top to bottom"
)]
async fn a_computer_without_the_root_sets_up_its_disk_unlock_through_one_that_keeps_it() {
    // The person is the one this account founded here: their root is kept for this account
    // alone, on the laptop.
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let account = rustix::process::geteuid().as_raw();
    let (laptop_service, _state, laptop_key, laptop_cert, _paths) =
        super::workspaces::ServiceWorkspaces::test_of_account_person(account).await;
    let account_dir = super::services::account_identity_dir(account).unwrap();
    let person = laptop_cert.user_public_key;
    let copies = laptop_service.copies();
    person_workspace::found(copies)
        .await
        .expect("the person's own workspace is founded here");
    laptop_service
        .found(
            super::first_workspace::own_workspace(&person),
            super::first_workspace::FOUNDED_NAME,
        )
        .await
        .expect("the person's workspace is founded");
    let root = root_in(account_dir.clone())().unwrap().unwrap();
    devices::mark_root_holder(copies, &root, &laptop_cert)
        .await
        .expect("the root marks the laptop that keeps it");

    // The desk: a computer of the person's, certified by their root, on its own endpoint.
    let desk_key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let mut desk_cert = DeviceCert {
        user_public_key: person,
        device_public_key: desk_key.verifying_key().to_bytes(),
        device_label: "The desk".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(60 * 1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut desk_cert, &root).unwrap();
    devices::keep(copies, &desk_cert)
        .await
        .expect("the desk is in the person's devices");
    // The desk enters the person's own workspace, as each of their devices does.
    copies
        .enter(
            &tilo_server_protocol::servers::person_workspace_id(&person),
            (&desk_cert, &desk_key),
        )
        .await
        .expect("the desk enters the person's workspace");
    let desk = desk_cert.device_public_key;

    // The laptop's daemon: the person's device actions under their root, answered to their other
    // devices on its endpoint, and the calls that wait for that root taken up there.
    let laptop = Tickets::new(
        (laptop_key.clone(), laptop_cert.clone()),
        laptop_service.endpoint(),
        None,
        root_in(account_dir),
        None,
        Some(Arc::clone(&laptop_service)),
        tilo_device_identity::login::Place::at(
            home.root().join("laptop"),
            tilo_device_identity::IdentityPaths::under(home.root().join("laptop-keys")),
        ),
        copies.action_service(),
    );
    copies.provide(PROVIDER, Arc::new(OnThisDevice(laptop.clone())));
    let (laptop_asks, _, laptop_asked) = dispatch(copies, laptop_cert.device_public_key);
    laptop_asked.serve_at_root(laptop.clone());

    // The desk's daemon, which keeps no root of the person's.
    let desk_endpoint = endpoint(&desk_key).await;
    let desk_tickets = Tickets::new(
        (desk_key.clone(), desk_cert.clone()),
        desk_endpoint.clone(),
        None,
        no_root(),
        // The person's own workspace, which the desk reaches their other devices in.
        Some(super::first_workspace::FirstWorkspace::founded(
            Arc::clone(&laptop_service),
            super::first_workspace::own_workspace(&person),
        )),
        Some(Arc::clone(&laptop_service)),
        tilo_device_identity::login::Place::at(
            home.root().join("desk"),
            tilo_device_identity::IdentityPaths::under(home.root().join("desk-keys")),
        ),
        ActionService::new(desk_key.clone(), Arc::new(NoActions)),
    );
    desk_tickets.reach_at(
        laptop_cert.device_public_key,
        laptop_service.endpoint().addr(),
    );
    let (_, desk_invocations, desk_asked) = dispatch(copies, desk);
    for asked in [&laptop_asked, &desk_asked] {
        let follows = Arc::downgrade(asked);
        copies.follow_asked_at_root(move |asks| {
            if let Some(asked) = follows.upgrade() {
                asked.kept_changed(asks);
            }
        });
    }
    let desk_unlock = BootUnlock {
        device: desk_cert.clone(),
        tickets: Arc::new(desk_tickets),
        invocations: desk_invocations,
        asked: desk_asked,
        workspaces: Arc::clone(&laptop_service),
    };
    let router = serve(&laptop_service, copies.action_service());
    // The person's devices name the laptop as one that keeps their root, and the desk reaches it.
    let laptop_key = laptop_cert.device_public_key;
    let keeping_root = devices::of(copies, &person).await.root_holders(&person);
    assert!(
        keeping_root.contains(&laptop_key),
        "the laptop is named as keeping the root: {keeping_root:?}"
    );
    desk_unlock
        .tickets
        .reach(laptop_key)
        .await
        .expect("the desk reaches the laptop");

    // The desk's identity store keeps no root of the person's; the laptop's does.
    let laptop_store = home.config().to_owned();
    let desk_store = home.root().join("desk-config");
    std::fs::create_dir_all(&desk_store).unwrap();
    let person_on_laptop = Caller::authenticated(person, laptop_cert.device_public_key);

    // The desk sets up a boot. The laptop asks the person, naming the desk and the boot key, and
    // their yes there certifies it.
    home.env().set("XDG_CONFIG_HOME", &desk_store);
    let boot = a_boot(1);
    let enrolled = answered(desk_unlock.enrol(&boot.args), async {
        let asked = asked_on_the_laptop(&laptop_asked, &person).await;
        assert_eq!(
            asked.facts.words.replace(['\u{2068}', '\u{2069}'], ""),
            "Set up unlock for The desk?"
        );
        let fingerprint = hex::encode(boot.key.verifying_key().to_bytes());
        let shown = asked
            .facts
            .facts
            .iter()
            .map(|fact| (fact.label.as_str(), fact.value.as_str()))
            .collect::<Vec<_>>();
        assert!(
            shown.contains(&("Computer", "The desk")),
            "the ask names the computer: {shown:?}"
        );
        assert!(
            shown.iter().any(|(label, value)| *label == "Boot key"
                && value.replace(' ', "")
                    == fingerprint
                        .get(..16)
                        .expect("a boot key is 64 hex digits, so its first 16 are there")),
            "the ask names the boot key: {shown:?}"
        );
        home.env().set("XDG_CONFIG_HOME", &laptop_store);
        laptop_asks
            .resolve(&asked.record, &person_on_laptop, &Resolve::default())
            .await
            .expect("the answer is decided at once")
            .expect("the person's yes is taken on the laptop");
    })
    .await;
    let enrolled = enrolled.expect("the desk's boot key is certified through the laptop");
    let response: EnrolResponse =
        postcard::from_bytes(&hex::decode(enrolled["enrolled"].as_str().unwrap()).unwrap())
            .unwrap();
    verify_device_cert(&response.cert).expect("the person's root certified the boot key");
    assert_eq!(response.cert.user_public_key, person);
    assert_eq!(
        response.cert.device_public_key,
        boot.key.verifying_key().to_bytes()
    );
    assert_eq!(response.cert.capabilities, DeviceCapabilities::nothing());

    // The desk published its enrolment, and the laptop releases its half to the desk's boot: the
    // disk unlocks with the half the desk kept, which never left it in the clear.
    let state = published(copies, &person, desk).await;
    let enrolment = state
        .enrolments
        .iter()
        .find(|enrolment| enrolment.cert == response.cert)
        .expect("the desk published its enrolment");
    assert_eq!(enrolment.kept_by, desk);
    assert_eq!(
        released(&laptop, &person_on_laptop, enrolment).await,
        boot.half,
        "the desk's boot unlocks with the half it kept"
    );

    // Another boot of the desk's: the person says no on the laptop. The desk is told so, and
    // nothing is certified or published for it.
    home.env().set("XDG_CONFIG_HOME", &desk_store);
    let refused = a_boot(2);
    let declined = answered(desk_unlock.enrol(&refused.args), async {
        let asked = asked_on_the_laptop(&laptop_asked, &person).await;
        laptop_asks
            .resolve(
                &asked.record,
                &person_on_laptop,
                &Resolve {
                    resolution: Offer::Decline,
                    ..Resolve::default()
                },
            )
            .await
            .expect("the answer is decided at once")
            .expect("the person's no is taken on the laptop");
    })
    .await;
    let declined = declined.expect_err("a declined enrolment certifies nothing");
    assert_eq!(declined.code, UNLOCK_DECLINED);
    assert_eq!(
        declined.message,
        "You said no on your other device, so this computer won’t unlock its disk with your \
         devices. It still opens with its passphrase or recovery key."
    );
    let state = published(copies, &person, desk).await;
    assert_eq!(
        state.enrolments.len(),
        1,
        "the declined boot is not published: {:?}",
        state.enrolments
    );
    assert!(state.enrolments.iter().all(
        |enrolment| enrolment.cert.device_public_key != refused.key.verifying_key().to_bytes()
    ),);
    // The person answered: the laptop asks them nothing more.
    let started = Instant::now();
    loop {
        let waiting = laptop_asked.store().asked_of(&person);
        if waiting.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < WITHIN,
            "the answered ask still waits on the laptop: {:?}",
            waiting
                .iter()
                .map(|asked| (&asked.name, &asked.facts.words))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The laptop goes away: with no device of theirs that keeps the root to ask, the desk is told
    // so at once, and nothing waits for an answer nobody can give.
    let _ = router.shutdown().await;
    laptop_service.endpoint().close().await;
    let unreachable = a_boot(3);
    let nobody = desk_unlock
        .enrol(&unreachable.args)
        .await
        .expect_err("nobody can be asked");
    assert_eq!(nobody.code, NO_ROOT_DEVICE);
    assert_eq!(
        nobody.message,
        "None of your devices that keep your identity can be reached right now. Turn one on, \
         connect it to the internet and sign in, then try again."
    );
    assert!(
        laptop_asked.store().asked_of(&person).is_empty(),
        "nothing waits for the person"
    );
}
