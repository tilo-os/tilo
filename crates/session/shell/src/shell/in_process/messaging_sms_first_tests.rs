//! The real default loan, original Passwords decision and private native D-Bus.
use super::*;
use crate::shell::in_process::messaging::sms_first;
use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
const MODEM: &str = "/org/freedesktop/ModemManager1/Modem/7";
const SIM: &str = "/org/freedesktop/ModemManager1/SIM/7";
const INCOMING: &str = "/org/freedesktop/ModemManager1/SMS/9";
const OUTGOING: &str = "/org/freedesktop/ModemManager1/SMS/10";
const NUMBER: &str = "+12025550123";
#[derive(Default)]
struct Evidence {
    creates: usize,
    sends: usize,
    text: String,
    number: String,
    class: i32,
    data: Vec<u8>,
    timestamp: String,
}
struct Modem(&'static str);
#[zbus::interface(name = "org.freedesktop.ModemManager1.Modem")]
impl Modem {
    #[zbus(property)]
    fn sim(&self) -> OwnedObjectPath {
        self.0.try_into().unwrap()
    }
}
struct Sim {
    iccid: &'static str,
    operator: &'static str,
}
#[zbus::interface(name = "org.freedesktop.ModemManager1.Sim")]
impl Sim {
    #[zbus(property)]
    fn sim_identifier(&self) -> String {
        self.iccid.into()
    }
    #[zbus(property)]
    fn operator_name(&self) -> String {
        self.operator.into()
    }
}
struct Messaging(Arc<Mutex<Evidence>>);
#[zbus::interface(name = "org.freedesktop.ModemManager1.Modem.Messaging")]
impl Messaging {
    fn create(&self, mut properties: HashMap<String, OwnedValue>) -> OwnedObjectPath {
        assert_eq!(properties.len(), 3);
        assert!(bool::try_from(properties.remove("delivery-report-request").unwrap()).unwrap());
        let mut state = self.0.lock().unwrap();
        state.creates = state.creates.checked_add(1).unwrap();
        state.number = String::try_from(properties.remove("number").unwrap()).unwrap();
        state.text = String::try_from(properties.remove("text").unwrap()).unwrap();
        drop(state);
        OUTGOING.try_into().unwrap()
    }
    #[zbus(property)]
    fn messages(&self) -> Vec<OwnedObjectPath> {
        let mut paths = vec![INCOMING.try_into().unwrap()];
        if self.0.lock().unwrap().creates > 0 {
            paths.push(OUTGOING.try_into().unwrap());
        }
        paths
    }
}
struct Sms {
    state: Arc<Mutex<Evidence>>,
    incoming: bool,
}
#[zbus::interface(name = "org.freedesktop.ModemManager1.Sms")]
impl Sms {
    fn send(&self) {
        assert!(!self.incoming);
        let mut state = self.state.lock().unwrap();
        state.sends = state.sends.checked_add(1).unwrap();
    }
    #[zbus(property)]
    fn number(&self) -> String {
        if self.incoming {
            NUMBER.into()
        } else {
            self.state.lock().unwrap().number.clone()
        }
    }
    #[zbus(property)]
    fn text(&self) -> String {
        if self.incoming {
            "private SIM arrival".into()
        } else {
            self.state.lock().unwrap().text.clone()
        }
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        if self.incoming { 3 } else { 0 }
    }
    #[zbus(property)]
    fn pdu_type(&self) -> u32 {
        if self.incoming { 1 } else { 2 }
    }
    #[zbus(property)]
    fn class(&self) -> i32 {
        self.state.lock().unwrap().class
    }
    #[zbus(property)]
    fn data(&self) -> Vec<u8> {
        self.state.lock().unwrap().data.clone()
    }
    #[zbus(property)]
    fn timestamp(&self) -> String {
        self.state.lock().unwrap().timestamp.clone()
    }
    #[zbus(property)]
    fn delivery_report_request(&self) -> bool {
        !self.incoming
    }
}
struct FixtureBus {
    child: std::process::Child,
    address: String,
    _root: tempfile::TempDir,
    _server: zbus::Connection,
    state: Arc<Mutex<Evidence>>,
}
impl FixtureBus {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                root.path().join("bus").display()
            ))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty());
        let address = address.trim().to_owned();
        let state = Arc::new(Mutex::new(Evidence {
            class: -1,
            timestamp: "2026-10-07T00:00:00+00:00".into(),
            ..Evidence::default()
        }));
        let server = crate::session_runtime::runtime().unwrap().block_on(async {
            zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .name("org.freedesktop.ModemManager1")
                .unwrap()
                .serve_at("/org/freedesktop/ModemManager1", zbus::fdo::ObjectManager)
                .unwrap()
                .serve_at(MODEM, Modem(SIM))
                .unwrap()
                .serve_at(MODEM, Messaging(state.clone()))
                .unwrap()
                .serve_at(
                    SIM,
                    Sim {
                        iccid: "8901000000000000001",
                        operator: "Synthetic SIM",
                    },
                )
                .unwrap()
                .serve_at(
                    INCOMING,
                    Sms {
                        state: state.clone(),
                        incoming: true,
                    },
                )
                .unwrap()
                .serve_at(
                    OUTGOING,
                    Sms {
                        state: state.clone(),
                        incoming: false,
                    },
                )
                .unwrap()
                .build()
                .await
                .unwrap()
        });
        Self {
            child,
            address,
            _root: root,
            _server: server,
            state,
        }
    }
}
impl Drop for FixtureBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct Source {
    address: String,
    receipt: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
    held: std::sync::atomic::AtomicBool,
}
impl sms_first::Source for Source {
    fn activate(
        &self,
        account: Arc<crate::shell::in_process::messaging::sms::Account>,
        selected: sms_first::Selected,
        root: std::path::PathBuf,
        scope: String,
    ) -> Result<(), SendError> {
        sms_first::activate(&account, selected, root, scope)?;
        if let Some(receipt) = &self.receipt {
            self.held.store(true, Ordering::Release);
            receipt
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| SendError::Unavailable)?;
        }
        Ok(())
    }
    fn connection(&self) -> Result<zbus::Connection, SendError> {
        crate::session_runtime::runtime().unwrap().block_on(async {
            zbus::connection::Builder::address(self.address.as_str())
                .unwrap()
                .build()
                .await
                .map_err(|_| SendError::Unavailable)
        })
    }
    fn choices(
        &self,
        identity: NativeMessagingIdentity,
    ) -> Result<Vec<sms_first::Selected>, SendError> {
        identity.check()?;
        crate::session_runtime::runtime().unwrap().block_on(async {
            let connection = zbus::connection::Builder::address(self.address.as_str())
                .unwrap()
                .build()
                .await
                .map_err(|_| SendError::Unavailable)?;
            let choices = sms_first::fixture_choices(&connection).await?;
            identity.check()?;
            Ok(choices)
        })
    }
}
#[test]
fn empty_native_messages_sim_choice_protected_review_receive_reply_and_revoke() {
    let bus = FixtureBus::new();
    let mut rig = Rig::new();
    let _paths = tilo_paths::scope_profile(rig.root.path().join("sms-owner"));
    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("sms-first.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = crate::shell::in_process::InProcessApps::native_for_profile(
        &rig.root.path().join("sms-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    rig.owner.service = Service::Sms;
    let vault = accounts::CredentialOwner::deferred(
        &rig.owner.principal,
        &apps.device_id,
        SecretStore::open,
    )
    .unwrap();
    apps.install_credential_owner(vault.clone()).unwrap();
    rig.broker = vault.broker().unwrap();
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    let (release, receipt) = std::sync::mpsc::channel();
    let sms_source = Arc::new(Source {
        address: bus.address.clone(),
        receipt: Some(Mutex::new(receipt)),
        held: std::sync::atomic::AtomicBool::new(false),
    });
    apps.first_accounts = Some(
        crate::shell::in_process::messaging::first_account::fixture_sms(
            rig.owner.principal.clone(),
            Arc::new(|| {}),
            sms_source.clone(),
        ),
    );
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    let account = enroll(&mut apps, &mut mounted, &mut rig, &vault, &bus.state);
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |_| {
                apps.pump_messaging_records();
                sms_source.held.load(Ordering::Acquire)
            })
    );
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenPerson {
            person: PERSON.into(),
        },
        false,
    )
    .unwrap();
    assert!(
        sms_source.held.load(Ordering::Acquire),
        "the native completion receipt is deliberately still held while opening the conversation"
    );
    release.send(()).unwrap();
    let (choice, reply) = receive_reply(&mut apps, &mut mounted, &account, &bus.state);
    let private = apps.native_messaging_root.as_ref().unwrap().join(
        blake3::hash(&serde_json::to_vec(&rig.owner).unwrap())
            .to_hex()
            .as_str(),
    );
    assert!(
        !std::fs::read(private.join("sim.enc"))
            .unwrap()
            .windows(19)
            .any(|bytes| bytes == b"8901000000000000001")
    );
    let Running { apps, mounted } = restart(
        Running { apps, mounted },
        &rig,
        &vault,
        &bus,
        &Proof {
            account: &account,
            choice: &choice,
            private: &private,
        },
    );
    revoke(&mut Running { apps, mounted }, &rig, &vault, &bus, &reply);
    drop(rig);
}

fn revoke(
    running: &mut Running,
    rig: &Rig,
    vault: &accounts::CredentialOwner,
    bus: &FixtureBus,
    reply: &MessagesEffect,
) {
    let private = running.apps.native_messaging_root.as_ref().unwrap().join(
        blake3::hash(&serde_json::to_vec(&rig.owner).unwrap())
            .to_hex()
            .as_str(),
    );
    let paths = [
        private.join("history/history.enc"),
        private.join("journal/vault.enc"),
    ];
    let before = paths.each_ref().map(|p| std::fs::read(p).unwrap());
    let grant = vault
        .grants()
        .unwrap()
        .into_iter()
        .find(|g| g.item_id == rig.request.item_id)
        .unwrap();
    vault.revoke_grant(&grant).unwrap();
    let account = ServiceAccount::new("sms", &rig.owner.account);
    assert_eq!(
        running.mounted.client.account_state(&account),
        AccountState::Locked
    );
    assert!(
        effect_with_pump(&mut running.mounted.harness, reply, true, || running
            .apps
            .pump_messaging_records())
        .is_err()
    );
    assert_eq!(paths.each_ref().map(|p| std::fs::read(p).unwrap()), before);
    assert_eq!(bus.state.lock().unwrap().creates, 1);
    assert_eq!(bus.state.lock().unwrap().sends, 1);
}

fn enroll(
    apps: &mut crate::shell::in_process::InProcessApps,
    mounted: &mut MountedMessages,
    rig: &mut Rig,
    vault: &accounts::CredentialOwner,
    evidence: &Mutex<Evidence>,
) -> ServiceAccount {
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenAccountSetup,
        false,
    )
    .unwrap();
    assert!(effect(&mut mounted.harness, &MessagesEffect::ListSmsSims, false).is_err());
    let choices = effect_with_pump(
        &mut mounted.harness,
        &MessagesEffect::ListSmsSims,
        true,
        || apps.pump_messaging_records(),
    )
    .unwrap()
    .value
    .unwrap();
    assert_eq!(choices["choices"].as_array().unwrap().len(), 1);
    assert_eq!(evidence.lock().unwrap().creates, 0);
    assert_eq!(evidence.lock().unwrap().sends, 0);
    let selected = effect_with_pump(
        &mut mounted.harness,
        &MessagesEffect::SelectSmsSim {
            selection: choices["choices"][0]["selection"].as_str().unwrap().into(),
        },
        true,
        || apps.pump_messaging_records(),
    )
    .unwrap()
    .value
    .unwrap();
    rig.owner.account = selected["account"].as_str().unwrap().into();
    let account = ServiceAccount::new("sms", &rig.owner.account);
    assert!(apps.messaging_services.is_empty());
    assert!(vault.grants().unwrap().is_empty());
    let scope = format!("sms:{}", rig.owner.account);
    let mut contacts = tilo_contact_directory::ContactDirectory::default();
    contacts
        .project_contact(
            tilo_resource::Id::from_raw(PERSON),
            "Synthetic contact".into(),
        )
        .unwrap();
    contacts
        .confirm_account(
            tilo_resource::Id::from_raw(PERSON),
            tilo_resource::Id::from_raw("sms-proof"),
            "sms".into(),
            scope,
            NUMBER.into(),
            "sim-reviewed-recipient".into(),
        )
        .unwrap();
    seed_person(apps, &contacts);
    let proposal = effect_with_pump(
        &mut mounted.harness,
        &MessagesEffect::ReviewFirstEnrollment {
            service: "sms".into(),
            account: rig.owner.account.clone(),
            display_name: "Fixture SIM".into(),
        },
        true,
        || apps.pump_messaging_records(),
    )
    .unwrap()
    .value
    .unwrap()["proposal"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(apps.messaging_services.is_empty());
    assert!(vault.grants().unwrap().is_empty());
    let request = vault
        .proposals()
        .unwrap()
        .into_iter()
        .find(|p| p.id == proposal)
        .unwrap()
        .request;
    rig.request = request;
    let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
    let credential = apps
        .native_messaging_credential(&identity, rig.request.clone())
        .unwrap();
    drop(identity);
    approve_fixture_profile_key(apps, &credential, proposal);
    drop(credential);
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |_| {
                apps.pump_messaging_records();
                mounted.client.account_state(&account) == AccountState::Ready
            })
    );
    account
}
fn receive_reply(
    apps: &mut crate::shell::in_process::InProcessApps,
    mounted: &mut MountedMessages,
    account: &ServiceAccount,
    evidence: &Mutex<Evidence>,
) -> (String, MessagesEffect) {
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenPerson {
            person: PERSON.into(),
        },
        false,
    )
    .unwrap();
    let routes = effect(
        &mut mounted.harness,
        &MessagesEffect::ListRoutes {
            person: PERSON.into(),
        },
        false,
    )
    .unwrap()
    .value
    .unwrap();
    let choice = serde_json::to_string(
        routes
            .as_array()
            .unwrap()
            .iter()
            .find(|route| route["service"] == "sms" && route["account"] == account.account)
            .expect("the chosen SIM must have its own explicit route"),
    )
    .unwrap();
    effect(
        &mut mounted.harness,
        &MessagesEffect::SelectRoute {
            choice: choice.clone(),
        },
        false,
    )
    .unwrap();
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |h| {
                apps.pump_messaging_records();
                h.a11y_nodes()
                    .iter()
                    .any(|(_, n)| n.label() == Some("private SIM arrival"))
            })
    );
    let reply = MessagesEffect::Send {
        choice: choice.clone(),
        operation: "sms-first-reply".into(),
        text: "private SIM reply".into(),
    };
    effect_with_pump(&mut mounted.harness, &reply, true, || {
        apps.pump_messaging_records();
    })
    .unwrap();
    assert_eq!(evidence.lock().unwrap().creates, 1);
    assert_eq!(evidence.lock().unwrap().sends, 1);
    assert!(
        mounted
            .harness
            .a11y_nodes()
            .iter()
            .any(|(_, n)| n.label() == Some("private SIM reply"))
    );
    assert!(!mounted.harness.dump_json().contains("private SIM"));
    assert!(!mounted.harness.dump_json().contains("8901000000000000001"));
    (choice, reply)
}
struct Running {
    apps: crate::shell::in_process::InProcessApps,
    mounted: MountedMessages,
}
struct Proof<'a> {
    account: &'a ServiceAccount,
    choice: &'a str,
    private: &'a std::path::Path,
}
fn restart(
    running: Running,
    rig: &Rig,
    vault: &accounts::CredentialOwner,
    bus: &FixtureBus,
    proof: &Proof<'_>,
) -> Running {
    let Running { apps, mounted } = running;
    let Proof {
        account,
        choice,
        private,
    } = *proof;
    let sim_ciphertext = std::fs::read(private.join("sim.enc")).unwrap();
    let grants = vault.grants().unwrap();
    drop(mounted);
    drop(apps);
    let mut apps = crate::shell::in_process::InProcessApps::native_for_profile(
        &rig.root.path().join("sms-apps"),
        &rig.owner.principal,
        rig.root.path().join("sms-first.sock"),
    )
    .unwrap();
    assert_eq!(apps.device_id, rig.owner.device);
    apps.install_credential_owner(vault.clone()).unwrap();
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    apps.first_accounts = Some(
        crate::shell::in_process::messaging::first_account::fixture_sms(
            rig.owner.principal.clone(),
            Arc::new(|| {}),
            Arc::new(Source {
                address: bus.address.clone(),
                receipt: None,
                held: std::sync::atomic::AtomicBool::new(false),
            }),
        ),
    );
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |_| {
                apps.pump_messaging_records();
                mounted.client.account_state(account) == AccountState::Ready
            }),
        "default startup must resume the selected encrypted SIM using the existing grant"
    );
    assert_eq!(
        vault
            .grants()
            .unwrap()
            .iter()
            .map(accounts::CredentialGrant::id)
            .collect::<Vec<_>>(),
        grants
            .iter()
            .map(accounts::CredentialGrant::id)
            .collect::<Vec<_>>()
    );
    assert!(vault.proposals().unwrap().is_empty());
    assert_eq!(
        std::fs::read(private.join("sim.enc")).unwrap(),
        sim_ciphertext
    );
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenPerson {
            person: PERSON.into(),
        },
        false,
    )
    .unwrap();
    effect(
        &mut mounted.harness,
        &MessagesEffect::SelectRoute {
            choice: choice.to_owned(),
        },
        false,
    )
    .unwrap();
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |h| {
                apps.pump_messaging_records();
                h.a11y_nodes()
                    .iter()
                    .any(|(_, n)| n.label() == Some("private SIM reply"))
            }),
        "the same account history must return after restart"
    );
    assert_eq!(bus.state.lock().unwrap().creates, 1);
    assert_eq!(bus.state.lock().unwrap().sends, 1);
    Running { apps, mounted }
}
