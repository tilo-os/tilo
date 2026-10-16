//! Default native Messages, original protected review and two certified peers.
use super::*;
use crate::network::{Actor, NetCmd, PeerIdentityStore};
use crate::session_runtime::runtime;
use iroh::{Endpoint, address_lookup::memory::MemoryLookup, endpoint::presets};
struct Peer {
    rig: Rig,
    apps: crate::shell::in_process::InProcessApps,
    mounted: MountedMessages,
    vault: accounts::CredentialOwner,
    lookup: MemoryLookup,
    commands: tokio::sync::mpsc::Sender<NetCmd>,
    network: Network,
    _source: Arc<NativePersonalSource>,
    _lifecycle: LocalSessionAuthority,
}
impl Peer {
    fn new(root_seed: u8, device_seed: u8) -> Self {
        let mut rig = Rig::new();
        rig.admission =
            tilo_action_broker::collab_broker_capnp::tests::certified_person_fixture_for_seeds(
                root_seed,
                device_seed,
            );
        rig.owner.principal = rig.admission.context.principal().into();
        let _profile = tilo_paths::scope_profile(rig.root.path().join("native-owner"));
        let lifecycle = LocalSessionAuthority::new();
        lifecycle.lifecycle().publish_native_state(false, false);
        let socket = rig.root.path().join("native-first.sock");
        let source = source(&rig, &lifecycle, &socket);
        let mut apps = crate::shell::in_process::InProcessApps::native_for_profile(
            &rig.root.path().join("native-apps"),
            &rig.owner.principal,
            socket,
        )
        .unwrap();
        rig.owner.device = apps.device_id.clone();
        rig.owner.service = Service::Tilo;
        let profile = rig.root.path().join("native-owner");
        let vault =
            accounts::CredentialOwner::deferred(&rig.owner.principal, &apps.device_id, move || {
                let _profile = tilo_paths::scope_profile(profile.clone());
                SecretStore::open()
            })
            .unwrap();
        apps.install_credential_owner(vault.clone()).unwrap();
        apps.bind_messaging_record_wake(Arc::new(|| {}));
        let lookup = MemoryLookup::new();
        let endpoint = runtime()
            .unwrap()
            .block_on(
                Endpoint::builder(presets::N0)
                    .secret_key(iroh::SecretKey::from_bytes(
                        &rig.admission.host.device_signing_key.to_bytes(),
                    ))
                    .address_lookup(lookup.clone())
                    .bind(),
            )
            .unwrap();
        let (commands, task) = runtime()
            .unwrap()
            .block_on(async { Actor::spawn(endpoint.clone(), lookup.clone()) });
        bind(&mut apps, &commands);
        let mounted = mount_messages(&mut apps, AccountState::Unavailable);
        Self {
            rig,
            apps,
            mounted,
            vault,
            lookup,
            commands,
            network: Network { endpoint, task },
            _source: source,
            _lifecycle: lifecycle,
        }
    }
    fn prepare_review(&mut self) -> String {
        effect(
            &mut self.mounted.harness,
            &MessagesEffect::OpenAccountSetup,
            false,
        )
        .unwrap();
        let begin = MessagesEffect::BeginFirstEnrollment {
            service: "tilo".into(),
        };
        assert!(effect(&mut self.mounted.harness, &begin, false).is_err());
        let before = self.mounted.harness.dump_json();
        let review = effect_with_pump(&mut self.mounted.harness, &begin, true, || {
            self.apps.pump_messaging_records();
        })
        .unwrap()
        .value
        .unwrap();
        self.rig.owner.account = review["account"].as_str().unwrap().into();
        assert!(self.apps.messaging_services.is_empty());
        assert!(self.vault.grants().unwrap().is_empty());
        let proposal = effect_with_pump(
            &mut self.mounted.harness,
            &MessagesEffect::ReviewFirstEnrollment {
                service: "tilo".into(),
                account: self.rig.owner.account.clone(),
                display_name: "Fixture native sender".into(),
            },
            true,
            || self.apps.pump_messaging_records(),
        )
        .unwrap()
        .value
        .unwrap()["proposal"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(self.apps.messaging_services.is_empty());
        assert!(self.vault.grants().unwrap().is_empty());
        self.rig.request = self
            .vault
            .proposals()
            .unwrap()
            .into_iter()
            .find(|p| p.id == proposal)
            .unwrap()
            .request;
        assert_eq!(self.mounted.harness.dump_json(), before);
        proposal
    }
    fn enroll(&mut self) {
        let proposal = self.prepare_review();
        let before = self.mounted.harness.dump_json();
        let identity = self
            .apps
            .native_messaging_identity(self.rig.owner.clone())
            .unwrap();
        let credential = self
            .apps
            .native_messaging_credential(&identity, self.rig.request.clone())
            .unwrap();
        approve_fixture_profile_key(&mut self.apps, &credential, proposal);
        drop(credential);
        drop(identity);
        let account = ServiceAccount::new("tilo", &self.rig.owner.account);
        assert!(
            self.mounted
                .harness
                .wait_until(ui::time::Duration::from_secs(10), |_| {
                    self.apps.pump_messaging_records();
                    self.mounted.client.account_state(&account) == AccountState::Ready
                })
        );
        assert_eq!(self.mounted.harness.dump_json(), before);
    }
    fn contacts(&mut self, other: &Self) {
        let mut directory = tilo_contact_directory::ContactDirectory::default();
        let certificate = &other.rig.admission.host.device_cert;
        let address = format!(
            "{}@{}",
            hex::encode(certificate.user_public_key),
            hex::encode(certificate.device_public_key)
        );
        directory
            .project_contact(tilo_resource::Id::from_raw(PERSON), "Fixture peer".into())
            .unwrap();
        directory
            .confirm_account(
                tilo_resource::Id::from_raw(PERSON),
                tilo_resource::Id::from_raw("native-proof"),
                "tilo".into(),
                format!("tilo:{}", self.rig.owner.account),
                address,
                "native-reviewed-device".into(),
            )
            .unwrap();
        seed_person(&mut self.apps, &directory);
    }
    fn choose(&mut self) -> String {
        effect(
            &mut self.mounted.harness,
            &MessagesEffect::OpenPerson {
                person: PERSON.into(),
            },
            false,
        )
        .unwrap();
        let routes = effect(
            &mut self.mounted.harness,
            &MessagesEffect::ListRoutes {
                person: PERSON.into(),
            },
            false,
        )
        .unwrap()
        .value
        .unwrap();
        let route = routes
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["service"] == "tilo" && r["account"] == self.rig.owner.account)
            .unwrap();
        assert!(
            !route["recipient"].as_str().unwrap().is_empty(),
            "confirmed private recipient must be projected"
        );
        let owned = tilo_contact_directory::routing::Route {
            person: tilo_resource::Id::from_raw(PERSON),
            association: tilo_resource::Id::from_raw(route["association"].as_str().unwrap()),
            sender: tilo_resource::Id::from_raw(&self.rig.owner.account),
            service: "tilo".into(),
            scope: route["scope"].as_str().unwrap().into(),
            recipient: route["recipient"].as_str().unwrap().into(),
            revision: route["revision"].as_str().unwrap().into(),
        };
        assert!(
            self.mounted.client.supports(&owned),
            "original current private host must authorize the confirmed route"
        );
        let choice = serde_json::to_string(route).unwrap();
        effect(
            &mut self.mounted.harness,
            &MessagesEffect::SelectRoute {
                choice: choice.clone(),
            },
            false,
        )
        .unwrap();
        choice
    }
    fn restart(self) -> Self {
        let Peer {
            rig,
            apps,
            mounted,
            vault,
            lookup,
            commands,
            network,
            _source: source_owner,
            _lifecycle: lifecycle_owner,
        } = self;
        let grants = vault
            .grants()
            .unwrap()
            .iter()
            .map(|grant| grant.id().to_owned())
            .collect::<Vec<_>>();
        drop(mounted);
        drop(apps);
        let mut apps = crate::shell::in_process::InProcessApps::native_for_profile(
            &rig.root.path().join("native-apps"),
            &rig.owner.principal,
            rig.root.path().join("native-first.sock"),
        )
        .unwrap();
        assert_eq!(apps.device_id, rig.owner.device);
        apps.install_credential_owner(vault.clone()).unwrap();
        apps.bind_messaging_record_wake(Arc::new(|| {}));
        bind(&mut apps, &commands);
        let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
        let account = ServiceAccount::new("tilo", &rig.owner.account);
        assert!(
            mounted
                .harness
                .wait_until(ui::time::Duration::from_secs(10), |_| {
                    apps.pump_messaging_records();
                    mounted.client.account_state(&account) == AccountState::Ready
                }),
            "default startup resumes the exact original grant and encrypted account"
        );
        assert_eq!(
            vault
                .grants()
                .unwrap()
                .iter()
                .map(|grant| grant.id().to_owned())
                .collect::<Vec<_>>(),
            grants
        );
        assert!(vault.proposals().unwrap().is_empty());
        Peer {
            rig,
            apps,
            mounted,
            vault,
            lookup,
            commands,
            network,
            _source: source_owner,
            _lifecycle: lifecycle_owner,
        }
    }
    fn private_paths(&self) -> [std::path::PathBuf; 2] {
        let root = self.apps.native_messaging_root.as_ref().unwrap().join(
            blake3::hash(&serde_json::to_vec(&self.rig.owner).unwrap())
                .to_hex()
                .as_str(),
        );
        [
            root.join("history/history.enc"),
            root.join("journal/vault.enc"),
        ]
    }
}
struct Network {
    endpoint: Endpoint,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Network {
    fn drop(&mut self) {
        self.task.abort();
        runtime().unwrap().block_on(self.endpoint.close());
    }
}

fn bind(
    apps: &mut crate::shell::in_process::InProcessApps,
    commands: &tokio::sync::mpsc::Sender<NetCmd>,
) {
    let (reply, receiver) = tokio::sync::oneshot::channel();
    commands.try_send(NetCmd::PrivateMessages(reply)).unwrap();
    apps.messaging_transport = Some(receiver);
    apps.native_messaging_peers = Some(PeerIdentityStore::new());
}
fn send(peer: &mut Peer, other: &mut Peer, choice: String, text: &str) -> MessagesEffect {
    let reply = MessagesEffect::Send {
        choice,
        operation: uuid::Uuid::new_v4().to_string(),
        text: text.into(),
    };
    effect_with_pump(&mut peer.mounted.harness, &reply, true, || {
        peer.apps.pump_messaging_records();
        other.apps.pump_messaging_records();
    })
    .unwrap();
    reply
}
fn visible(peer: &mut Peer, other: &mut Peer, text: &str) {
    assert!(
        peer.mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(10), |h| {
                peer.apps.pump_messaging_records();
                other.apps.pump_messaging_records();
                h.a11y_nodes().iter().any(|(_, n)| n.label() == Some(text))
            })
    );
}
#[test]
fn default_native_first_account_protected_two_peer_text_and_revoke() {
    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let mut a = Peer::new(161, 162);
    let mut b = Peer::new(171, 172);
    assert_ne!(a.rig.owner.principal, b.rig.owner.principal);
    assert_ne!(a.network.endpoint.id(), b.network.endpoint.id());
    a.lookup.add_endpoint_info(b.network.endpoint.addr());
    b.lookup.add_endpoint_info(a.network.endpoint.addr());
    a.enroll();
    b.enroll();
    a.contacts(&b);
    b.contacts(&a);
    let choice_a = a.choose();
    let choice_b = b.choose();
    let before_a = a.mounted.harness.dump_json();
    let before_b = b.mounted.harness.dump_json();
    send(&mut a, &mut b, choice_a, "private native arrival");
    visible(&mut b, &mut a, "private native arrival");
    send(&mut b, &mut a, choice_b, "private native reply");
    visible(&mut a, &mut b, "private native reply");
    assert_eq!(a.mounted.harness.dump_json(), before_a);
    assert_eq!(b.mounted.harness.dump_json(), before_b);
    let paths_a = a.private_paths();
    let paths_b = b.private_paths();
    let bytes_a = paths_a.each_ref().map(|p| std::fs::read(p).unwrap());
    let bytes_b = paths_b.each_ref().map(|p| std::fs::read(p).unwrap());
    for bytes in bytes_a.iter().chain(bytes_b.iter()) {
        assert!(
            !bytes
                .windows(b"private native".len())
                .any(|w| w == b"private native")
        );
    }
    a = a.restart();
    assert_eq!(
        paths_a.each_ref().map(|p| std::fs::read(p).unwrap()),
        bytes_a
    );
    let choice_a = a.choose();
    visible(&mut a, &mut b, "private native reply");
    let grant = a
        .vault
        .grants()
        .unwrap()
        .into_iter()
        .find(|g| g.item_id == a.rig.request.item_id)
        .unwrap();
    a.vault.revoke_grant(&grant).unwrap();
    assert_eq!(
        a.mounted
            .client
            .account_state(&ServiceAccount::new("tilo", &a.rig.owner.account)),
        AccountState::Locked
    );
    let refused = MessagesEffect::Send {
        choice: choice_a,
        operation: uuid::Uuid::new_v4().to_string(),
        text: "refused native write".into(),
    };
    assert!(
        effect_with_pump(&mut a.mounted.harness, &refused, true, || {
            a.apps.pump_messaging_records();
            b.apps.pump_messaging_records();
        })
        .is_err()
    );
    assert_eq!(
        paths_a.each_ref().map(|p| std::fs::read(p).unwrap()),
        bytes_a
    );
    assert_eq!(
        paths_b.each_ref().map(|p| std::fs::read(p).unwrap()),
        bytes_b
    );
    drop(a);
    drop(b);
}

/// Linux only: it reads the default native accounts from Messages' Linux
/// account provider.
#[cfg(target_os = "linux")]
#[test]
fn default_native_denied_review_never_publishes_or_writes_private_account() {
    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let mut peer = Peer::new(181, 182);
    let before = peer.mounted.harness.dump_json();
    let proposal = peer.prepare_review();
    let component = peer
        .apps
        .mount(
            passwords::definition(),
            "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc",
            42,
            RecordId::room_default(),
            |_| {},
        )
        .unwrap();
    let mut review = ui::headless::FrameHarness::from_component(
        component,
        (1100, 760),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    review.relayout();
    let deny = passwords::PasswordsEffect::DenyAccess { proposal };
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: 42,
        address: tilo_actions::ActionAddress::new("passwords.vault", deny.id()),
    };
    let (_, result) = review
        .start_bound_job(&address, &deny.arguments(), false)
        .unwrap();
    assert!(result.completed().is_some());
    review.close_components();
    drop(review);
    let account = ServiceAccount::new("tilo", &peer.rig.owner.account);
    assert!(
        peer.mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |_| {
                peer.apps.pump_messaging_records();
                peer.mounted.client.account_state(&account) != AccountState::Ready
                    && peer.vault.proposals().unwrap().is_empty()
            })
    );
    assert!(peer.vault.grants().unwrap().is_empty());
    assert!(peer.apps.messaging_services.is_empty());
    assert!(
        peer.apps
            .default_native_accounts("tilo")
            .unwrap()
            .is_empty()
    );
    assert!(peer.private_paths().iter().all(|path| !path.exists()));
    assert_eq!(peer.mounted.harness.dump_json(), before);
    drop(peer);
}
