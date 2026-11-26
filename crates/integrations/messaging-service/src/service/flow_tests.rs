//! The one enrollment flow, driven through a provider that records what it
//! was asked: one enrollment per service, link tokens, SIM choices and their
//! cancellation, the order each service's steps take, each account's key
//! kept in and read back from the person's workspace vault, and retirement
//! when the person's session ends.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering as Atomic};
use std::sync::{Arc, Mutex};

use tilo_messaging::{EnrollmentPhase, Pairing, SendError, ServiceAccount};
use tilo_messaging_protocol::supervisor::AccountSupervisor;

use super::{Flow, Order, Provider};
use crate::link::{Handler, Responder};
use crate::service::Service;
use crate::service::guard::{AccountGuard, Running};
use crate::service::identity::{Credential, Identity, Ownership};
use crate::session;

/// The person's workspace vault, as the compositor keeps it for these flows:
/// each entry's reference and the key it keeps.
#[derive(Default)]
struct Vault {
    keys: BTreeMap<String, String>,
    /// The person may not use Passwords: nothing is kept or handed out.
    refuses: bool,
}

/// The compositor's side, as far as these flows ask it.
struct Compositor(Arc<Mutex<Vault>>);
impl Handler<session::Call, session::Reply, session::Event> for Compositor {
    fn call(
        &self,
        call: session::Call,
        reply: Responder<session::Call, session::Reply, session::Event>,
    ) {
        let mut vault = self.0.lock().unwrap();
        reply.send(match call {
            session::Call::Person => session::Reply::Person {
                accounts: tilo_resource::Model::to_json(&accounts::person::AccountList::default()),
                directory: tilo_resource::Model::to_json(
                    &tilo_contact_directory::ContactDirectory::default(),
                ),
            },
            session::Call::KeepKey { .. } | session::Call::Key { .. } if vault.refuses => {
                session::Reply::Refused
            }
            session::Call::KeepKey { key, .. } => {
                let item = format!("vault.item:{:032x}", vault.keys.len());
                vault.keys.insert(item.clone(), key.to_string());
                session::Reply::Kept { item }
            }
            session::Call::Key { item } => match vault.keys.get(&item) {
                Some(key) => session::Reply::Key {
                    key: zeroize::Zeroizing::new(key.clone()),
                },
                None => session::Reply::Refused,
            },
            _ => session::Reply::Done,
        });
    }
    fn event(&self, _: session::Event) {}
}

struct Rig {
    runtime: tokio::runtime::Runtime,
    service: Arc<Service>,
    vault: Arc<Mutex<Vault>>,
    _compositor: session::SessionLink,
    _profile: tempfile::TempDir,
}

fn rig() -> Rig {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&tilo_test_env::node_key());
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
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let actor = hex::encode(certificate.user_public_key);
    let ownership = Ownership::with_host(
        tilo_identity::identity_store::DeviceIdentity::kept(certificate, device),
        &actor,
        "device",
    )
    .unwrap();
    ownership.session(true, 1);
    let vault = Arc::<Mutex<Vault>>::default();
    let (service_end, compositor_end) = std::os::unix::net::UnixStream::pair().unwrap();
    let compositor = session::SessionLink::start(
        "compositor",
        compositor_end.try_clone().unwrap(),
        compositor_end,
        Arc::new(Compositor(vault.clone())),
    )
    .unwrap();
    let handler = super::super::SessionHandler::new();
    let link = session::SessionLink::start(
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
            Arc::new(ownership),
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
        vault,
        _compositor: compositor,
        _profile: profile,
    }
}

struct Started {
    guard: Arc<AccountGuard>,
    supervisor: Arc<AccountSupervisor>,
}
impl Running for Started {
    fn guard(&self) -> &Arc<AccountGuard> {
        &self.guard
    }
    fn supervisor(&self) -> &Arc<AccountSupervisor> {
        &self.supervisor
    }
}

/// A provider of any order that links with the token "scanned", offers two
/// SIMs and starts an idle account, recording the vault entry and key each
/// account started under.
struct Fake<const ORDER: u8> {
    begun: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
    alive: Arc<Mutex<bool>>,
    keys: Arc<Mutex<Vec<(String, String)>>>,
}

impl<const ORDER: u8> Fake<ORDER> {
    fn start(
        &self,
        service: &Arc<Service>,
        identity: &Identity,
        credential: Credential,
    ) -> Result<(Arc<Started>, String), SendError> {
        self.keys.lock().unwrap().push((
            credential.item.clone(),
            hex::encode(credential.profile_key().expose_secret()),
        ));
        let scope = format!("signal:{}", identity.owner.account);
        let account = service.account(identity, credential, &scope, |guard, supervisor| {
            Started { guard, supervisor }
        })?;
        Ok((account, scope))
    }
}

const LINK_THEN_START: u8 = 0;
const CHOOSE_THEN_START: u8 = 2;

struct Linking(Arc<AtomicUsize>, Arc<Mutex<bool>>);

impl<const ORDER: u8> Provider for Fake<ORDER> {
    type Held = Linking;
    type Account = Started;
    const SERVICE: tilo_messaging_protocol::Service = tilo_messaging_protocol::Service::Signal;
    const ORIGIN: &'static str = "https://signal.org";
    const ORDER: Order = match ORDER {
        LINK_THEN_START => Order::LinkThenStart,
        CHOOSE_THEN_START => Order::ChooseThenStart,
        _ => Order::StartThenLink,
    };

    fn key_title(name: &str) -> String {
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
        service: &Arc<Service>,
        identity: &Identity,
        credential: Credential,
        _: &str,
        held: Option<Linking>,
    ) -> Result<(Arc<Started>, String, Option<Linking>), SendError> {
        let (account, scope) = self.start(service, identity, credential)?;
        Ok((account, scope, held))
    }

    fn resume(
        &self,
        service: &Arc<Service>,
        identity: &Identity,
        credential: Credential,
        _: String,
        _: &accounts::Account,
    ) -> Result<Arc<Started>, SendError> {
        Ok(self.start(service, identity, credential)?.0)
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
    keys: Arc<Mutex<Vec<(String, String)>>>,
}

fn drive<const ORDER: u8>(rig: &Rig) -> Driven<ORDER> {
    let begun = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let alive = Arc::new(Mutex::new(true));
    let keys = Arc::<Mutex<Vec<(String, String)>>>::default();
    Driven {
        flow: Flow::new(
            Fake::<ORDER> {
                begun: begun.clone(),
                cancelled: cancelled.clone(),
                alive: alive.clone(),
                keys: keys.clone(),
            },
            Arc::downgrade(&rig.service),
        ),
        begun,
        cancelled,
        alive,
        keys,
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
    let Driven { flow, begun, .. } = drive::<LINK_THEN_START>(&rig);
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

tilo_test_env::scenario! {
    fn an_enrollment_is_reviewed_chosen_and_retired_by_its_flow;

    phase reviewing_needs_a_valid_name_and_a_vault_that_keeps_the_key;
    {
        let rig = rig();
        let Driven {
            flow, cancelled, keys, ..
        } = drive::<LINK_THEN_START>(&rig);
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
        // The person may not use Passwords here: no key is kept, so the
        // account never starts and the enrollment ends.
        rig.vault.lock().unwrap().refuses = true;
        assert_eq!(
            rig.runtime
                .block_on(flow.clone().review(pairing.account.clone(), "Phone".into()))
                .err(),
            Some(SendError::Revoked)
        );
        assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Interrupted);
        assert_eq!(cancelled.load(Atomic::SeqCst), 1, "the link was stopped");
        assert!(keys.lock().unwrap().is_empty(), "nothing started");
        assert!(!rig.service.runs(&pairing.account));
    }

    phase a_reviewed_account_starts_under_the_key_its_vault_entry_keeps;
    {
        let rig = rig();
        let Driven { flow, keys, .. } = drive::<LINK_THEN_START>(&rig);
        let pairing = rig.runtime.block_on(flow.clone().begin()).unwrap();
        rig.runtime
            .block_on(
                flow.clone()
                    .finish(pairing.account.clone(), "scanned".into()),
            )
            .unwrap();
        rig.runtime
            .block_on(flow.clone().review(pairing.account.clone(), "Phone".into()))
            .unwrap();
        let kept: Vec<(String, String)> = rig.vault.lock().unwrap().keys.clone().into_iter().collect();
        assert_eq!(kept.len(), 1, "one entry for the one account");
        assert_eq!(*keys.lock().unwrap(), kept, "started under the key the vault keeps");
        assert_eq!(kept[0].1.len(), 64, "a 256-bit key");
        assert!(rig.service.runs(&pairing.account));
        assert_eq!(phase(&flow, &pairing.account), EnrollmentPhase::Linking);
        assert_eq!(
            rig.runtime
                .block_on(flow.review(pairing.account, "Phone".into()))
                .err(),
            Some(SendError::Revoked),
            "a started account is not reviewed again"
        );
    }

    phase a_published_account_starts_again_under_the_key_its_entry_keeps;
    {
        let rig = rig();
        let Driven { flow, keys, .. } = drive::<LINK_THEN_START>(&rig);
        let kept = "ab".repeat(32);
        rig.vault
            .lock()
            .unwrap()
            .keys
            .insert("vault.item:kept".into(), kept.clone());
        let published = |id: &str, item: &str| {
            let mut row = accounts::Account::new(id, "signal");
            row.config = serde_json::json!({
                "messaging_scope": format!("signal:{id}"),
                "linking_generation": 1,
            });
            accounts::email::set_password_item(&mut row.config, Some(item));
            row
        };
        let kept_id = uuid::Uuid::new_v4().to_string();
        let gone_id = uuid::Uuid::new_v4().to_string();
        let mut store = accounts::person::AccountList::default();
        store.keep(&published(&kept_id, "vault.item:kept")).unwrap();
        store.keep(&published(&gone_id, "vault.item:gone")).unwrap();
        rig.runtime.block_on(flow.restore(store));
        assert_eq!(
            *keys.lock().unwrap(),
            vec![("vault.item:kept".to_owned(), kept)],
            "only the account whose entry the vault keeps starts, under that key"
        );
        assert!(rig.service.runs(&ServiceAccount::new("signal", &kept_id)));
        assert!(!rig.service.runs(&ServiceAccount::new("signal", &gone_id)));
    }

    phase a_sim_is_chosen_from_this_scan_only_and_closing_the_chooser_answers_nothing;
    {
        let rig = rig();
        let Driven { flow, .. } = drive::<CHOOSE_THEN_START>(&rig);
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

    phase cancelling_or_ending_the_session_or_breaking_the_link_retires_the_enrollment;
    {
        let rig = rig();
        let Driven {
            flow,
            cancelled,
            alive,
            ..
        } = drive::<LINK_THEN_START>(&rig);
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
        rig.service.ownership.session(true, 2);
        flow.check();
        assert_eq!(
            phase(&flow, &pairing.account),
            EnrollmentPhase::Interrupted,
            "an enrollment admitted under an ended session does not continue"
        );
        rig.service.ownership.session(false, 2);
        assert_eq!(
            rig.runtime.block_on(flow.begin()).err(),
            Some(SendError::Revoked),
            "nothing is admitted while the session is not usable"
        );
    }
}

#[test]
fn a_service_that_links_first_offers_only_its_own_first_step() {
    let rig = rig();
    let Driven { flow: link, .. } = drive::<LINK_THEN_START>(&rig);
    assert_eq!(link.prepare().err(), Some(SendError::Unavailable));
    assert_eq!(
        rig.runtime.block_on(link.choices()).err(),
        Some(SendError::Unavailable)
    );
}
