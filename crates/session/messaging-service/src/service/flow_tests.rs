//! The one enrollment flow, driven through a provider that records what it
//! was asked: one enrollment per service, link tokens, SIM choices and their
//! cancellation, the order each service's steps take, and retirement when
//! the person's session ends.

use std::sync::atomic::{AtomicUsize, Ordering as Atomic};
use std::sync::{Arc, Mutex};

use tilo_messaging::{EnrollmentPhase, Pairing, SendError, ServiceAccount};
use tilo_messaging_protocol::supervisor::AccountSupervisor;

use super::{Flow, Order, Provider};
use crate::door;
use crate::link::{Handler, Responder};
use crate::service::Service;
use crate::service::gate::{Gate, Running};
use crate::service::identity::{Credential, Custody, Identity};

/// The compositor's side, as far as these flows ask it.
struct Compositor;
impl Handler<door::Call, door::Reply, door::Event> for Compositor {
    fn call(&self, call: door::Call, reply: Responder<door::Call, door::Reply, door::Event>) {
        reply.send(match call {
            door::Call::Person => door::Reply::Person {
                accounts: accounts::AccountStore::default(),
                directory: tilo_resource::Model::to_json(
                    &tilo_contact_directory::ContactDirectory::default(),
                ),
            },
            _ => door::Reply::Done,
        });
    }
    fn event(&self, _: door::Event) {}
}

struct Rig {
    runtime: tokio::runtime::Runtime,
    service: Arc<Service>,
    _compositor: door::DoorLink,
    _vault: tempfile::TempDir,
    _profile: tempfile::TempDir,
}

fn rig() -> Rig {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Synthetic messaging device".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let principal = hex::encode(certificate.user_public_key);
    let custody = Custody::with_host(
        tilo_identity_host::custody::HostIdentity {
            device_cert: certificate,
            device_signing_key: device,
            workspace_device_certs: std::collections::HashMap::new(),
            workspace_revocations: std::collections::HashMap::new(),
            identity_paths: None,
        },
        &principal,
        "device",
    )
    .unwrap();
    custody.session(true, 1);
    let vault_dir = tempfile::tempdir().unwrap();
    let store = vault_dir.path().to_owned();
    let vault = accounts::CredentialOwner::deferred(&principal, "device", move || {
        accounts::SecretStore::open_in(&store)
    })
    .unwrap();
    let (service_end, compositor_end) = std::os::unix::net::UnixStream::pair().unwrap();
    let compositor = door::DoorLink::start(
        "compositor",
        compositor_end.try_clone().unwrap(),
        compositor_end,
        Arc::new(Compositor),
    )
    .unwrap();
    let handler = super::super::DoorHandler::new();
    let link = door::DoorLink::start(
        "service",
        service_end.try_clone().unwrap(),
        service_end,
        handler.clone(),
    )
    .unwrap();
    let profile = tempfile::tempdir().unwrap();
    let service = {
        let _entered = runtime.enter();
        Service::assemble(
            Arc::new(custody),
            vault,
            profile.path(),
            link,
            runtime.handle().clone(),
        )
        .unwrap()
    };
    handler.bind(&service);
    Rig {
        runtime,
        service,
        _compositor: compositor,
        _vault: vault_dir,
        _profile: profile,
    }
}

struct Started {
    gate: Arc<Gate>,
    supervisor: Arc<AccountSupervisor>,
}
impl Running for Started {
    fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }
    fn supervisor(&self) -> &Arc<AccountSupervisor> {
        &self.supervisor
    }
}

/// A provider of any order that links with the token "scanned" and offers
/// two SIMs. It never starts an account: the vault in these tests has no
/// system custody, so no grant is ever made.
struct Fake<const ORDER: u8> {
    begun: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
    alive: Arc<Mutex<bool>>,
}

const LINK_THEN_GRANT: u8 = 0;
const CHOOSE_THEN_GRANT: u8 = 2;
const GRANT: u8 = 3;

struct Linking(Arc<AtomicUsize>, Arc<Mutex<bool>>);

impl<const ORDER: u8> Provider for Fake<ORDER> {
    type Held = Linking;
    type Account = Started;
    const SERVICE: tilo_messaging_protocol::Service = tilo_messaging_protocol::Service::Signal;
    const ORIGIN: &'static str = "https://signal.org";
    const ORDER: Order = match ORDER {
        LINK_THEN_GRANT => Order::LinkThenGrant,
        CHOOSE_THEN_GRANT => Order::ChooseThenGrant,
        GRANT => Order::Grant,
        _ => Order::GrantThenLink,
    };

    fn purpose(name: &str) -> String {
        format!("fake {name}")
    }

    fn begin(
        &self,
        _: &Arc<Service>,
        identity: &Identity,
        held: &mut Option<Linking>,
        _: Option<&Arc<Started>>,
    ) -> Result<Pairing, SendError> {
        self.begun.fetch_add(1, Atomic::SeqCst);
        *held = Some(Linking(self.cancelled.clone(), self.alive.clone()));
        Ok(Pairing::new(
            ServiceAccount::new("signal", &identity.owner.account),
            "scanned".into(),
            identity.owner.linking_generation,
            zeroize::Zeroizing::new(b"qr".to_vec()),
        ))
    }

    fn finish(
        &self,
        held: &mut Option<Linking>,
        attempt: String,
    ) -> Result<Option<String>, SendError> {
        held.as_ref().ok_or(SendError::Revoked)?;
        (attempt == "scanned")
            .then(|| Some("+15550100".into()))
            .ok_or(SendError::Revoked)
    }

    fn choices(&self, _: &Arc<Service>, _: &Identity) -> Result<Vec<(String, Linking)>, SendError> {
        Ok(["Carrier · …1234", "…5678"]
            .into_iter()
            .map(|label| {
                (
                    label.into(),
                    Linking(self.cancelled.clone(), self.alive.clone()),
                )
            })
            .collect())
    }

    fn approved(
        &self,
        _: &Arc<Service>,
        _: &Identity,
        _: Credential,
        _: &str,
        _: Option<Linking>,
    ) -> Result<(Arc<Started>, String, Option<Linking>), SendError> {
        Err(SendError::Unavailable)
    }

    fn resume(
        &self,
        _: &Arc<Service>,
        _: &Identity,
        _: Credential,
        _: String,
        _: &accounts::Account,
    ) -> Result<Arc<Started>, SendError> {
        Err(SendError::Unavailable)
    }

    fn alive(held: &Linking) -> bool {
        *held.1.lock().unwrap()
    }

    fn cancel(held: &Linking) {
        held.0.fetch_add(1, Atomic::SeqCst);
    }
}

/// A flow over [`Fake`] and what the fake counts.
struct Driven<const ORDER: u8> {
    flow: Arc<Flow<Fake<ORDER>>>,
    begun: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
    alive: Arc<Mutex<bool>>,
}

fn drive<const ORDER: u8>(rig: &Rig) -> Driven<ORDER> {
    let begun = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let alive = Arc::new(Mutex::new(true));
    Driven {
        flow: Flow::new(
            Fake::<ORDER> {
                begun: begun.clone(),
                cancelled: cancelled.clone(),
                alive: alive.clone(),
            },
            Arc::downgrade(&rig.service),
        ),
        begun,
        cancelled,
        alive,
    }
}

fn phase<P: Provider>(flow: &Flow<P>, account: &ServiceAccount) -> EnrollmentPhase {
    flow.views()
        .into_iter()
        .find(|view| &view.account == account)
        .map_or(EnrollmentPhase::Empty, |view| view.phase)
}

#[test]
fn a_link_is_finished_only_with_its_own_token_and_then_reviewed_once() {
    let rig = rig();
    let Driven { flow, begun, .. } = drive::<LINK_THEN_GRANT>(&rig);
    let pairing = rig.runtime.block_on(flow.clone().begin()).unwrap();
    assert_eq!(begun.load(Atomic::SeqCst), 1);
    assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Linking);
    assert_eq!(
        rig.runtime.block_on(flow.clone().begin()).err(),
        Some(SendError::AlreadyReserved),
        "one enrollment per service at a time"
    );
    assert_eq!(
        rig.runtime
            .block_on(
                flow.clone()
                    .finish(pairing.account.clone(), "forged".into())
            )
            .err(),
        Some(SendError::Revoked)
    );
    let review = rig
        .runtime
        .block_on(
            flow.clone()
                .finish(pairing.account.clone(), "scanned".into()),
        )
        .unwrap();
    assert_eq!(review.number.as_deref(), Some("+15550100"));
    assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Review);
    assert_eq!(
        rig.runtime
            .block_on(flow.finish(pairing.account, "scanned".into()))
            .err(),
        Some(SendError::Revoked),
        "a verified link is not finished twice"
    );
}

#[test]
fn reviewing_needs_a_valid_name_and_a_request_the_vault_cannot_make_ends_the_enrollment() {
    let rig = rig();
    let Driven {
        flow, cancelled, ..
    } = drive::<LINK_THEN_GRANT>(&rig);
    let pairing = rig.runtime.block_on(flow.clone().begin()).unwrap();
    assert_eq!(
        rig.runtime
            .block_on(flow.clone().review(pairing.account.clone(), "Phone".into()))
            .err(),
        Some(SendError::Revoked),
        "a Signal account is reviewed only after its link is verified"
    );
    assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Linking);
    rig.runtime
        .block_on(
            flow.clone()
                .finish(pairing.account.clone(), "scanned".into()),
        )
        .unwrap();
    for name in ["", "  ", "line\nbreak"] {
        assert_eq!(
            rig.runtime
                .block_on(flow.clone().review(pairing.account.clone(), name.into()))
                .err(),
            Some(SendError::InvalidRequest),
            "{name:?}"
        );
    }
    // This vault keeps its key in a file, never system custody: no key is
    // prepared, so no request is made and the enrollment ends.
    assert_eq!(
        rig.runtime
            .block_on(flow.clone().review(pairing.account.clone(), "Phone".into()))
            .err(),
        Some(SendError::Revoked)
    );
    assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Interrupted);
    assert_eq!(cancelled.load(Atomic::SeqCst), 1, "the link was stopped");
}

#[test]
fn a_sim_is_chosen_from_this_scan_only_and_closing_the_chooser_answers_nothing() {
    let rig = rig();
    let Driven { flow, .. } = drive::<CHOOSE_THEN_GRANT>(&rig);
    let choices = rig.runtime.block_on(flow.clone().choices()).unwrap();
    assert_eq!(choices.len(), 2);
    assert_ne!(choices[0].selection, choices[1].selection);
    assert_eq!(
        flow.select("not-offered").err(),
        Some(SendError::InvalidRequest)
    );
    let choices = rig.runtime.block_on(flow.clone().choices()).unwrap();
    flow.cancel_choices();
    assert_eq!(
        flow.select(&choices[0].selection).err(),
        Some(SendError::Revoked),
        "a closed chooser selects nothing"
    );
    let choices = rig.runtime.block_on(flow.clone().choices()).unwrap();
    let review = flow.select(&choices[1].selection).unwrap();
    assert_eq!(review.number.as_deref(), Some(choices[1].label.as_str()));
    assert_eq!(review.generation, 1);
    assert_eq!(review.scope, format!("signal:{}", review.account.account));
    assert_eq!(phase(&flow, &review.account), EnrollmentPhase::Review);
    assert_eq!(
        rig.runtime.block_on(flow.choices()).err(),
        Some(SendError::AlreadyReserved)
    );
}

#[test]
fn each_order_offers_only_its_own_first_step() {
    let rig = rig();
    let Driven { flow: link, .. } = drive::<LINK_THEN_GRANT>(&rig);
    assert_eq!(link.prepare().err(), Some(SendError::Unavailable));
    assert_eq!(
        rig.runtime.block_on(link.choices()).err(),
        Some(SendError::Unavailable)
    );
    let Driven {
        flow: grant, begun, ..
    } = drive::<GRANT>(&rig);
    let review = grant.prepare().unwrap();
    assert_eq!(phase(&grant, &review.account), EnrollmentPhase::Review);
    assert_eq!(
        rig.runtime.block_on(grant.begin()).err(),
        Some(SendError::Unavailable)
    );
    assert_eq!(begun.load(Atomic::SeqCst), 0);
}

#[test]
fn cancelling_or_ending_the_session_or_breaking_the_link_retires_the_enrollment() {
    let rig = rig();
    let Driven {
        flow,
        cancelled,
        alive,
        ..
    } = drive::<LINK_THEN_GRANT>(&rig);
    let pairing = rig.runtime.block_on(flow.clone().begin()).unwrap();
    flow.cancel(&pairing.account);
    assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Interrupted);
    assert_eq!(cancelled.load(Atomic::SeqCst), 1);
    let pairing = rig.runtime.block_on(flow.clone().begin()).unwrap();
    *alive.lock().unwrap() = false;
    flow.check();
    assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Interrupted);
    *alive.lock().unwrap() = true;
    let pairing = rig.runtime.block_on(flow.clone().begin()).unwrap();
    rig.service.custody.session(true, 2);
    flow.check();
    assert_eq!(
        phase(&flow, &pairing.account),
        EnrollmentPhase::Interrupted,
        "an enrollment admitted under an ended session does not continue"
    );
    rig.service.custody.session(false, 2);
    assert_eq!(
        rig.runtime.block_on(flow.begin()).err(),
        Some(SendError::Revoked),
        "nothing is admitted while the session is not usable"
    );
}
