use super::*;
use crate::shell::collab_broker::NativePersonalSource;
use crate::shell::collab_broker_capnp::LocalSessionAuthority;
use messages::MessagesEffect;
use tilo_resource::{Effect, Fields, Resource, SharedRecord};

#[path = "messaging_record_tests.rs"]
mod current_records;

const ACI: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OWN_ACI: &str = "11111111-1111-4111-8111-111111111111";
const PERSON: &str = "00000000-0000-4000-8000-000000000001";
fn route() -> Route {
    Route {
        person: tilo_resource::Id::from_raw(PERSON),
        association: tilo_resource::Id::from_raw("native-proof"),
        sender: tilo_resource::Id::from_raw("fixture-signal"),
        service: "signal".into(),
        scope: format!("signal:{OWN_ACI}"),
        recipient: ACI.into(),
        revision: format!("signal-identity:05{}", "42".repeat(32)),
    }
}
fn directory() -> tilo_contact_directory::ContactDirectory {
    let route = route();
    let mut directory = tilo_contact_directory::ContactDirectory::default();
    directory
        .project_contact(route.person.clone(), "Synthetic contact".into())
        .unwrap();
    directory
        .confirm_account(
            route.person,
            route.association,
            route.service,
            route.scope,
            route.recipient,
            route.revision,
        )
        .unwrap();
    directory
}
fn accounts() -> accounts::AccountStore {
    let mut accounts = accounts::AccountStore::new();
    accounts.upsert(accounts::Account::new("fixture-signal", "signal"));
    accounts
}
struct Provider(Owner);
impl SignalAuthority for Provider {
    fn check_account_scope(&self, scope: &str) -> Result<(), SendError> {
        if scope == route().scope {
            self.check_account()
        } else {
            Err(SendError::Revoked)
        }
    }
    fn check_account(&self) -> Result<(), SendError> {
        accounts()
            .get(&accounts::AccountId::new(&self.0.account))
            .filter(|account| account.enabled)
            .map(|_| ())
            .ok_or(SendError::Revoked)
    }
    fn authorize(&self, requested: &Route) -> Result<Authorization, SendError> {
        if requested != &route() {
            return Err(SendError::Revoked);
        }
        Ok(Authorization::new(
            directory(),
            accounts(),
            tilo_messaging_protocol::routing::LiveBinding {
                owner: self.0.clone(),
                state: tilo_messaging_protocol::routing::LinkState::Linked,
            },
            route(),
        ))
    }
    fn incoming(&self, aci: &str) -> Result<Authorization, SendError> {
        if aci != ACI {
            return Err(SendError::Revoked);
        }
        self.authorize(&route())
    }
}
fn seed_person<P: Resource<State = P> + Fields + SharedRecord>(
    apps: &mut super::super::super::InProcessApps,
    value: &P,
) {
    let records = ui::record::person::PersonRecords::open(
        &mut apps.owners,
        P::OWNER,
        ui::permissions::UserId(apps.author.0.clone()),
    )
    .unwrap();
    let record = records
        .open_record(ui::record::RecordKind::new(P::KIND), |doc| {
            P::initialize(doc).map_err(|error| error.to_string())
        })
        .unwrap();
    record
        .edit(false, |doc| {
            value
                .write_fields(doc, &automerge::ROOT)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    records.flush().unwrap();
}
fn effect(
    harness: &mut ui::headless::FrameHarness,
    effect: &MessagesEffect,
    confirmed: bool,
) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
    effect_with_pump(harness, effect, confirmed, || {})
}
fn effect_with_pump(
    harness: &mut ui::headless::FrameHarness,
    effect: &MessagesEffect,
    confirmed: bool,
    mut pump: impl FnMut(),
) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: 41,
        address: tilo_actions::ActionAddress::new("tilo.messages", effect.id()),
    };
    let (invocation, started) =
        harness.start_bound_job(&address, &effect.arguments(), confirmed)?;
    if let Some(output) = started.completed() {
        harness.relayout();
        return Ok(output);
    }
    if !harness.wait_until(ui::time::Duration::from_secs(5), |harness| {
        pump();
        harness
            .job_updates(invocation)
            .iter()
            .any(tilo_actions::InvocationUpdate::is_terminal)
    }) {
        return Err(tilo_actions::DoorError::Failed(
            "native effect did not finish through its actual job door".into(),
        ));
    }
    match harness
        .job_updates(invocation)
        .iter()
        .find(|update| update.is_terminal())
    {
        Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => Ok(output.clone()),
        Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
            Err(tilo_actions::DoorError::Failed(error.message.clone()))
        }
        _ => Err(tilo_actions::DoorError::Failed(
            "native operation cancelled".into(),
        )),
    }
}
fn probe(reader: &mut BufReader<UnixStream>, peer: &mut UnixStream) {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["method"], "listAccounts");
    writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":[{"aci":OWN_ACI,"number":"+15555550101"}]})).unwrap();
}
fn source(
    rig: &Rig,
    lifecycle: &LocalSessionAuthority,
    socket: &std::path::Path,
) -> Arc<NativePersonalSource> {
    NativePersonalSource::install_for_test(
        socket.to_path_buf(),
        rig.admission.broker.clone(),
        lifecycle.lifecycle(),
        rig.admission.host.clone(),
    )
    .unwrap()
}

#[test]
fn default_native_broker_source_refuses_inactive_wrong_person_and_socket() {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    let socket = rig.root.path().join("owned-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("apps"),
        &rig.owner.principal,
        socket.clone(),
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    assert!(apps.native_messaging_identity(rig.owner.clone()).is_err());
    let attempted = AtomicUsize::new(0);
    rig.grant(AccessDecision::AllowForSession);
    assert!(
        apps.start_signal_account(rig.startup(), |_| {
            attempted.fetch_add(1, Ordering::SeqCst);
            Err(SendError::Unavailable)
        })
        .is_err(),
        "an externally supplied proof cannot bypass inactive default native setup"
    );
    assert_eq!(attempted.load(Ordering::SeqCst), 0);
    lifecycle.lifecycle().publish_native_state(false, false);
    let mut another_profile = rig.owner.clone();
    another_profile.device = "another-sdk-profile".into();
    assert!(apps.native_messaging_identity(another_profile).is_err());
    assert!(
        NativePersonalSource::install_for_test(
            socket.clone(),
            rig.admission.broker.clone(),
            lifecycle.lifecycle(),
            rig.admission.host.clone()
        )
        .is_none(),
        "an active broker socket never accepts another source"
    );
    let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
    identity.check().unwrap();
    assert!(NativePersonalSource::admit_for_socket(&socket, &"33".repeat(32)).is_err());
    assert!(
        NativePersonalSource::admit_for_socket(
            &rig.root.path().join("other-session.sock"),
            &rig.owner.principal
        )
        .is_err()
    );
    lifecycle.lifecycle().set_locked(true);
    assert_eq!(identity.check(), Err(SendError::Revoked));
    lifecycle.lifecycle().set_locked(false);
    assert_eq!(
        identity.check(),
        Err(SendError::Revoked),
        "unlock never revives an old setup execution"
    );
    apps.native_messaging_identity(rig.owner.clone())
        .unwrap()
        .check()
        .unwrap();
    assert!(rig.admission_live());
    drop(identity);
    drop(rig);
}

#[test]
fn first_account_preparation_uses_original_native_source_without_opening_custody() {
    let rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    let socket = rig.root.path().join("first-account-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("first-account-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let observed = opens.clone();
    let vault = rig.root.path().join("unopened-first-account-vault");
    let custody = vault.clone();
    let credential_owner =
        accounts::CredentialOwner::deferred(&rig.owner.principal, &apps.device_id, move || {
            observed.fetch_add(1, Ordering::SeqCst);
            SecretStore::open_in(&custody)
        })
        .unwrap();
    apps.install_credential_owner(credential_owner).unwrap();
    let (inactive, _) = UnixStream::pair().unwrap();
    assert!(apps.prepare_native_signal_enrollment(inactive).is_err());
    lifecycle.lifecycle().publish_native_state(false, false);
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let enrollment = apps.prepare_native_signal_enrollment(stream).unwrap();
    assert_eq!(enrollment.owner().principal, rig.owner.principal);
    assert_eq!(enrollment.owner().device, apps.device_id);
    assert_eq!(enrollment.owner().service, Service::Signal);
    assert_ne!(enrollment.owner().account, rig.owner.account);
    assert!(uuid::Uuid::parse_str(&enrollment.owner().account).is_ok());
    let (another, _) = UnixStream::pair().unwrap();
    let second = apps.prepare_native_signal_enrollment(another).unwrap();
    assert_ne!(enrollment.owner().account, second.owner().account);
    drop(second);
    assert_eq!(
        enrollment.state(),
        tilo_messaging::AccountState::Unavailable
    );
    assert_eq!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    lifecycle.lifecycle().set_locked(true);
    lifecycle.lifecycle().set_locked(false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert!(matches!(
        runtime.block_on(enrollment.begin()),
        Err(SendError::Revoked)
    ));
    assert_eq!(enrollment.state(), tilo_messaging::AccountState::Locked);
    drop(enrollment);
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert!(!vault.exists());
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn native_credential_owner_refuses_foreign_requests_and_retired_source_before_storage() {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("credential-owner-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let opens = Arc::new(AtomicUsize::new(0));
    let observed = opens.clone();
    let root = rig.root.path().join("unopened-vault");
    let custody = root.clone();
    let owner =
        accounts::CredentialOwner::deferred(&rig.owner.principal, &rig.owner.device, move || {
            observed.fetch_add(1, Ordering::SeqCst);
            SecretStore::open_in(&custody)
        })
        .unwrap();
    let mut other_person = rig.owner.clone();
    other_person.principal = "33".repeat(32);
    assert!(
        apps.install_credential_owner(
            accounts::CredentialOwner::from_broker(
                &other_person.principal,
                &rig.owner.device,
                rig.broker.clone(),
            )
            .unwrap()
        )
        .is_err()
    );
    apps.install_credential_owner(owner.clone()).unwrap();
    assert!(
        apps.install_credential_owner(owner).is_err(),
        "original native owner cannot be replaced"
    );
    let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
    let mut foreign_app = rig.request.clone();
    foreign_app.app_id = "so.tilo.other".into();
    assert!(
        apps.native_messaging_credential(&identity, foreign_app)
            .is_err()
    );
    let mut unscoped_origin = rig.request.clone();
    unscoped_origin.origin = None;
    assert!(
        apps.native_messaging_credential(&identity, unscoped_origin)
            .is_err()
    );
    let mut invalid_generation = rig.owner.clone();
    invalid_generation.linking_generation = 0;
    let invalid = apps.native_messaging_identity(invalid_generation).unwrap();
    assert!(
        apps.native_messaging_credential(&invalid, rig.request.clone())
            .is_err()
    );
    lifecycle.lifecycle().set_locked(true);
    assert!(
        apps.native_messaging_credential(&identity, rig.request.clone())
            .is_err()
    );
    lifecycle.lifecycle().set_locked(false);
    assert!(
        apps.native_messaging_credential(&identity, rig.request.clone())
            .is_err()
    );
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    assert!(
        !root.exists(),
        "a refused native request never opens ambient custody"
    );
    drop(invalid);
    drop(identity);
    drop(apps);
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn native_permission_proposal_reaches_original_protected_passwords_confirmation_without_ready_transport()
 {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("permission-proposal-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("permission-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let vault = accounts::CredentialOwner::from_broker(
        &rig.owner.principal,
        &rig.owner.device,
        rig.broker.clone(),
    )
    .unwrap();
    apps.install_credential_owner(vault.clone()).unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let observed = wakes.clone();
    apps.bind_messaging_record_wake(Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
    let credential = apps
        .native_messaging_credential(&identity, rig.request.clone())
        .unwrap();
    assert!(credential.check().is_err());
    let id = apps
        .propose_native_messaging_permission(&identity, rig.request.clone())
        .unwrap();
    assert!(
        credential.check().is_err(),
        "proposing access creates no permission"
    );
    assert!(wakes.load(Ordering::SeqCst) > 0);
    let component = apps
        .mount(
            passwords::definition(),
            "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc",
            42,
            RecordId::room_default(),
            |_| {},
        )
        .unwrap();
    let mut harness = ui::headless::FrameHarness::from_component(
        component,
        (1100, 760),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    harness.relayout();
    let resources =
        serde_json::to_string(&tilo_actions::door::survey(&harness.resources())).unwrap();
    assert!(resources.contains(&format!("passwords:request:{id}")));
    for private in [
        "synthetic private credential",
        "Synthetic Signal account",
        "https://signal.test",
    ] {
        assert!(!resources.contains(private));
    }
    let approve = passwords::PasswordsEffect::ApproveAccess {
        proposal: id,
        lifetime: "session".into(),
    };
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: 42,
        address: tilo_actions::ActionAddress::new("passwords.vault", approve.id()),
    };
    assert!(
        harness
            .start_bound_job(&address, &approve.arguments(), false)
            .is_err()
    );
    assert!(credential.check().is_err());
    let (_, started) = harness
        .start_bound_job(&address, &approve.arguments(), true)
        .unwrap();
    assert!(started.completed().is_some());
    assert!(credential.check().is_ok());
    assert!(vault.proposals().unwrap().is_empty());
    assert!(
        apps.messaging.is_none(),
        "permission approval does not publish a ready provider"
    );
    assert!(apps.messaging_services.is_empty());
    harness.close_components();
    drop(harness);
    drop(credential);
    drop(identity);
    drop(vault);
    drop(apps);
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn native_permission_proposal_retirement_never_grants_after_lock_unlock() {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("retired-permission-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("retired-permission-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let vault = accounts::CredentialOwner::from_broker(
        &rig.owner.principal,
        &rig.owner.device,
        rig.broker.clone(),
    )
    .unwrap();
    apps.install_credential_owner(vault.clone()).unwrap();
    let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
    let id = apps
        .propose_native_messaging_permission(&identity, rig.request.clone())
        .unwrap();
    lifecycle.lifecycle().set_locked(true);
    lifecycle.lifecycle().set_locked(false);
    assert!(
        vault
            .decide_proposal(&id, AccessDecision::AllowAlways)
            .is_err()
    );
    assert!(vault.grants().unwrap().is_empty());
    assert!(vault.proposals().unwrap().is_empty());
    assert!(
        apps.propose_native_messaging_permission(&identity, rig.request.clone())
            .is_err()
    );
    assert!(apps.messaging.is_none());
    drop(identity);
    drop(vault);
    drop(apps);
    assert!(rig.admission_live());
    drop(rig);
}

fn sender_daemon(
    mut send_peer: UnixStream,
    observed: Arc<AtomicUsize>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(send_peer.try_clone().unwrap());
        probe(&mut reader, &mut send_peer);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "listIdentities" => {
                    assert_eq!(request["params"]["account"], "+15555550101");
                    assert_eq!(request["params"]["number"], ACI);
                    serde_json::json!([{"uuid":ACI,"fingerprint":format!("05{}", " 42".repeat(32)),"trustLevel":"TRUSTED_VERIFIED"}])
                }
                "send" => {
                    assert_eq!(request["params"]["account"], "+15555550101");
                    assert_eq!(request["params"]["recipient"], serde_json::json!([ACI]));
                    assert_eq!(request["params"]["message"], "native private reply");
                    assert_eq!(request["id"], "native-reply-operation");
                    observed.fetch_add(1, Ordering::SeqCst);
                    serde_json::json!({"timestamp":43})
                }
                method => panic!("unexpected native operation: {method}"),
            };
            writeln!(
                send_peer,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result})
            )
            .unwrap();
        }
    })
}
fn subscribe(reader: &mut BufReader<UnixStream>, peer: &mut UnixStream) {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["method"], "subscribeReceive");
    assert_eq!(
        request["params"],
        serde_json::json!({"account":"+15555550101"})
    );
    writeln!(
        peer,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":0})
    )
    .unwrap();
}
fn receiver_daemon(
    mut receive_peer: UnixStream,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (arrive, arrival) = std::sync::mpsc::channel();
    let inbox = std::thread::spawn(move || {
        let mut reader = BufReader::new(receive_peer.try_clone().unwrap());
        probe(&mut reader, &mut receive_peer);
        subscribe(&mut reader, &mut receive_peer);
        arrival
            .recv_timeout(ui::time::Duration::from_secs(5))
            .unwrap();
        writeln!(receive_peer, "{}", serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{"subscription":0,"result":{"account":"+15555550101","envelope":{"sourceUuid":ACI,"dataMessage":{"timestamp":42,"message":"native private arrival"}}}}})).unwrap();
        assert_eq!(
            reader.read(&mut [0]).unwrap(),
            0,
            "only the owned receiver is shut down"
        );
    });
    (arrive, inbox)
}
fn select_native_conversation(mounted: &mut MountedMessages) -> MessagesEffect {
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenPerson {
            person: PERSON.into(),
        },
        false,
    )
    .unwrap();
    let selected = route();
    let choice = serde_json::json!({"service":selected.service,"account":selected.sender.as_str(),"recipient":selected.recipient,"association":selected.association.as_str(),"scope":selected.scope,"revision":selected.revision}).to_string();
    effect(
        &mut mounted.harness,
        &MessagesEffect::SelectRoute {
            choice: choice.clone(),
        },
        false,
    )
    .unwrap();
    MessagesEffect::Send {
        choice,
        operation: "native-reply-operation".into(),
        text: "native private reply".into(),
    }
}

fn native_startup(
    rig: &Rig,
    apps: &mut super::super::super::InProcessApps,
) -> SignalAccountStartup {
    seed_person(apps, &directory());
    let mut metadata = accounts::person::PersonAccounts::default();
    metadata
        .save_account(
            "fixture-signal".into(),
            "signal".into(),
            "Synthetic Signal".into(),
            serde_json::json!({"messaging_scope":route().scope, "linking_generation":1}),
            true,
            0,
        )
        .unwrap();
    seed_person(apps, &metadata);
    rig.grant(AccessDecision::AllowForSession);
    if apps.credential_owner.is_none() {
        apps.install_credential_owner(
            accounts::CredentialOwner::from_broker(
                &rig.owner.principal,
                &rig.owner.device,
                rig.broker.clone(),
            )
            .unwrap(),
        )
        .unwrap();
    }
    let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
    let credential = apps
        .native_messaging_credential(&identity, rig.request.clone())
        .unwrap();
    SignalAccountStartup {
        identity,
        credential,
        authority: Arc::new(Provider(rig.owner.clone())),
    }
}

fn revoke_native_permission(
    rig: &Rig,
    apps: &mut super::super::super::InProcessApps,
    mounted: &mut MountedMessages,
    reply: &MessagesEffect,
    writes: &AtomicUsize,
) {
    let component = apps
        .mount(
            passwords::definition(),
            "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc",
            42,
            RecordId::room_default(),
            |context| {
                let loans = context.take_lent();
                assert_eq!(
                    loans.len(),
                    1,
                    "Passwords receives the original vault owner"
                );
                let owner = loans
                    .into_iter()
                    .next()
                    .unwrap()
                    .downcast::<accounts::CredentialOwner>()
                    .unwrap();
                assert!(Arc::ptr_eq(&owner.broker().unwrap(), &rig.broker));
                context.lend(*owner);
            },
        )
        .unwrap();
    let mut vault = ui::headless::FrameHarness::from_component(
        component,
        (900, 600),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    vault.relayout();
    let permission = rig
        .broker
        .lock()
        .unwrap()
        .current_grants()
        .into_iter()
        .find(|grant| grant.scope.as_ref() == Some(&credential_scope(&rig.owner)))
        .unwrap();
    let revoke = passwords::PasswordsEffect::RevokeAccess {
        grant: permission.id().into(),
    };
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: 42,
        address: tilo_actions::ActionAddress::new("passwords.vault", revoke.id()),
    };
    assert!(
        vault
            .start_bound_job(&address, &revoke.arguments(), false)
            .is_err()
    );
    assert_eq!(
        mounted
            .client
            .account_state(&ServiceAccount::new("signal", &rig.owner.account)),
        AccountState::Ready
    );
    let (_, revoked) = vault
        .start_bound_job(&address, &revoke.arguments(), true)
        .unwrap();
    assert!(
        revoked.completed().is_some(),
        "revocation executes on the original native owner"
    );
    assert_eq!(
        mounted
            .client
            .account_state(&ServiceAccount::new("signal", &rig.owner.account)),
        AccountState::Locked
    );
    let MessagesEffect::Send { choice, text, .. } = reply.clone() else {
        unreachable!()
    };
    assert!(
        effect(
            &mut mounted.harness,
            &MessagesEffect::Send {
                choice,
                text,
                operation: "native-after-passwords-revoke".into(),
            },
            true
        )
        .is_err()
    );
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "native Passwords revocation prevents further protocol writes"
    );
    drop(vault);
}

fn native_current_startup(
    rig: &Rig,
    apps: &mut super::super::super::InProcessApps,
) -> SignalAccountStartup {
    let SignalAccountStartup {
        identity,
        credential,
        ..
    } = native_startup(rig, apps);
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    let authority = apps
        .native_messaging_authority(&identity, &credential, &route().scope)
        .unwrap();
    SignalAccountStartup {
        identity,
        credential,
        authority,
    }
}

#[test]
fn default_native_launch_receives_displays_and_replies_on_exact_signed_account() {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("owned-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let (sender, send_peer) = UnixStream::pair().unwrap();
    let (receiver, receive_peer) = UnixStream::pair().unwrap();
    send_peer
        .set_read_timeout(Some(ui::time::Duration::from_secs(5)))
        .unwrap();
    receive_peer
        .set_read_timeout(Some(ui::time::Duration::from_secs(5)))
        .unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let observed = writes.clone();
    let daemon = sender_daemon(send_peer, observed);
    let (arrive, inbox) = receiver_daemon(receive_peer);
    let account = start_current_fixture(&rig, &mut apps, (sender, receiver));
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    await_current_fixture(&mut apps, &mut mounted);
    let reply = select_native_conversation(&mut mounted);
    arrive.send(()).unwrap();
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |harness| {
                apps.pump_messaging_records();
                harness
                    .a11y_nodes()
                    .iter()
                    .any(|(_, node)| node.label() == Some("native private arrival"))
            }),
        "actual idle receiver must display private text in the launched app"
    );
    assert!(effect(&mut mounted.harness, &reply, false).is_err());
    effect_with_pump(&mut mounted.harness, &reply, true, || {
        apps.pump_messaging_records();
    })
    .unwrap();
    assert!(
        mounted
            .harness
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some("native private reply"))
    );
    assert!(
        !mounted
            .harness
            .dump_json()
            .contains("native private arrival")
    );
    assert!(!mounted.harness.dump_json().contains("native private reply"));
    assert!(effect(&mut mounted.harness, &reply, true).is_err());
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "a repeated operation never writes a second protocol message"
    );
    revoke_native_permission(&rig, &mut apps, &mut mounted, &reply, &writes);
    lifecycle.lifecycle().set_locked(true);
    assert_eq!(
        mounted
            .client
            .account_state(&ServiceAccount::new("signal", &rig.owner.account)),
        AccountState::Locked
    );
    assert!(effect(&mut mounted.harness, &reply, true).is_err());
    drop(mounted);
    drop(account);
    drop(apps);
    daemon.join().unwrap();
    inbox.join().unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(rig.admission_live());
    drop(rig);
}

fn start_current_fixture(
    rig: &Rig,
    apps: &mut super::super::super::InProcessApps,
    streams: (UnixStream, UnixStream),
) -> Arc<NativeSignalAccount> {
    let owner = rig.owner.clone();
    let root = rig.root.path().to_path_buf();
    let ui_thread = std::thread::current().id();
    let startup = native_current_startup(rig, apps);
    apps.start_signal_account_on_worker(startup, move |authority| {
        assert_ne!(std::thread::current().id(), ui_thread);
        assemble_fixture(&owner, &root, streams, authority)
    })
    .unwrap()
}
fn await_current_fixture(
    apps: &mut super::super::super::InProcessApps,
    mounted: &mut MountedMessages,
) {
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |_| {
                apps.pump_messaging_records();
                mounted
                    .client
                    .account_state(&ServiceAccount::new("signal", "fixture-signal"))
                    == AccountState::Ready
            }),
        "the mounted original native loan must activate through worker verification and UI acceptance"
    );
}

#[test]
fn native_private_link_setup_crosses_sensitive_actions_and_never_exports_qr() {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("private-link-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let startup = native_startup(&rig, &mut apps);
    let account = apps
        .start_signal_account(startup, |_| Err(SendError::Unavailable))
        .unwrap();
    let (setup, peer) = UnixStream::pair().unwrap();
    let linker = account.prepare_linking(OWN_ACI.into(), setup).unwrap();
    let daemon = link_daemon(peer);
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenAccountSetup,
        false,
    )
    .unwrap();
    effect(
        &mut mounted.harness,
        &MessagesEffect::SelectSetupAccount {
            service: "signal".into(),
            account: "fixture-signal".into(),
        },
        false,
    )
    .unwrap();
    let begin = MessagesEffect::BeginLink {
        service: "signal".into(),
        account: "fixture-signal".into(),
    };
    assert!(effect(&mut mounted.harness, &begin, false).is_err());
    let result = effect(&mut mounted.harness, &begin, true).unwrap();
    let attempt = result.value.unwrap()["attempt"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(linker.state(), AccountState::Linking);
    assert_private_setup_presentation(&mut mounted.harness, true);
    // Acknowledge the actual mounted writer so its independent 100ms save
    // status timer does not mask the app's private presentation deadline.
    mounted.harness.flush_component_records().unwrap();
    let idle_deadline = mounted.harness.root_widget().next_wake_deadline().unwrap();
    let observed = ui::time::now();
    assert!(idle_deadline > observed);
    assert!(idle_deadline <= observed + ui::time::Duration::from_secs(121));
    assert!(mounted.harness.frame_schedule().next_wake_deadline.unwrap() <= idle_deadline);
    let complete = MessagesEffect::CompleteLink {
        service: "signal".into(),
        account: "fixture-signal".into(),
        attempt,
    };
    assert!(effect(&mut mounted.harness, &complete, false).is_err());
    effect(&mut mounted.harness, &complete, true).unwrap();
    assert_eq!(
        linker.state(),
        AccountState::Offline,
        "link evidence never fabricates a ready inbox"
    );
    assert_private_setup_presentation(&mut mounted.harness, false);
    assert_eq!(linker.client().wake_after(), None);
    mounted.harness.flush_component_records().unwrap();
    assert_eq!(mounted.harness.root_widget().next_wake_deadline(), None);
    assert!(effect(&mut mounted.harness, &begin, true).is_err());
    let (sender, receiver, writes) = activate_linked_conversation(&account, &rig, &mut mounted);
    lifecycle.lifecycle().set_locked(true);
    assert_eq!(
        mounted
            .client
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Locked
    );
    effect(&mut mounted.harness, &MessagesEffect::CancelLink, false).unwrap();
    drop(mounted);
    linker.cancel();
    drop(linker);
    drop(account);
    drop(apps);
    daemon.join().unwrap();
    sender.join().unwrap();
    receiver.join().unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(rig.admission_live());
    drop(rig);
}

fn activate_linked_conversation(
    account: &NativeSignalAccount,
    rig: &Rig,
    mounted: &mut MountedMessages,
) -> (
    std::thread::JoinHandle<()>,
    std::thread::JoinHandle<()>,
    Arc<AtomicUsize>,
) {
    let (sender, send_peer) = UnixStream::pair().unwrap();
    let (receiver, receive_peer) = UnixStream::pair().unwrap();
    send_peer
        .set_read_timeout(Some(ui::time::Duration::from_secs(5)))
        .unwrap();
    receive_peer
        .set_read_timeout(Some(ui::time::Duration::from_secs(5)))
        .unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let daemon = sender_daemon(send_peer, writes.clone());
    let (arrive, inbox) = receiver_daemon(receive_peer);
    account
        .activate(|authority| {
            assemble_fixture(&rig.owner, rig.root.path(), (sender, receiver), authority)
        })
        .unwrap();
    assert_eq!(
        mounted
            .client
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Ready
    );
    let reply = select_native_conversation(mounted);
    arrive.send(()).unwrap();
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |harness| harness
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some("native private arrival")))
    );
    assert!(effect(&mut mounted.harness, &reply, false).is_err());
    effect(&mut mounted.harness, &reply, true).unwrap();
    assert!(
        mounted
            .harness
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some("native private reply"))
    );
    assert!(
        !mounted
            .harness
            .dump_json()
            .contains("native private arrival")
    );
    assert!(!mounted.harness.dump_json().contains("native private reply"));
    assert!(effect(&mut mounted.harness, &reply, true).is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    (daemon, inbox, writes)
}
fn link_daemon(mut peer: UnixStream) -> std::thread::JoinHandle<()> {
    peer.set_read_timeout(Some(ui::time::Duration::from_secs(5)))
        .unwrap();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let start: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(start["method"], "startLink");
        let uri = "sgnl://linkdevice?uuid=22222222-2222-4222-8222-222222222222&pub_key=synthetic-private-link-key";
        writeln!(
            peer,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":start["id"],"result":{"deviceLinkUri":uri}})
        )
        .unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let finish: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(finish["method"], "finishLink");
        assert_eq!(finish["params"]["deviceLinkUri"], uri);
        assert_ne!(start["id"], finish["id"]);
        writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0","id":finish["id"],"result":{"number":"+15555550101"}})).unwrap();
        probe(&mut reader, &mut peer);
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    })
}

fn completed_first_enrollment(
    apps: &super::super::super::InProcessApps,
) -> (
    NativeSignalEnrollment,
    NativeVerifiedSignalEnrollment,
    std::thread::JoinHandle<()>,
) {
    let (stream, peer) = UnixStream::pair().unwrap();
    let enrollment = apps.prepare_native_signal_enrollment(stream).unwrap();
    let provider = link_daemon(peer);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pairing = runtime.block_on(enrollment.begin()).unwrap();
    let verified = runtime
        .block_on(enrollment.finish(pairing.attempt))
        .unwrap();
    (enrollment, verified, provider)
}

#[test]
fn reviewed_first_enrollment_publishes_original_settings_action_without_credential_or_ready_loan() {
    use tilo_actions::Action as _;
    let rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("enrollment-publication.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("publication-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    let settings = ui::record::person::PersonRecords::open(
        &mut apps.owners,
        accounts::person::ACCOUNTS_OWNER,
        apps.author.clone(),
    )
    .unwrap();
    let record = settings
        .open_original_record(
            ui::record::RecordKind::new(accounts::person::PersonAccounts::KIND),
            |doc| {
                accounts::person::PersonAccounts::initialize(doc).map_err(|error| error.to_string())
            },
        )
        .unwrap();
    let existing = accounts::person::PersonAccounts::SAVE_ACCOUNT
        .with((
            "existing-mail".into(),
            "email".into(),
            "Existing mail".into(),
            serde_json::json!({}),
            true,
            0,
        ))
        .build(tilo_resource::origin_now_ms())
        .unwrap();
    record
        .edit(true, |doc| {
            existing.apply(doc).map_err(|error| error.to_string())
        })
        .unwrap();
    let (enrollment, candidate, provider) = completed_first_enrollment(&apps);
    let id = candidate.owner().account.clone();
    let published = apps
        .publish_native_signal_enrollment(candidate, "New Signal".into())
        .unwrap();
    assert_eq!(published.account, ServiceAccount::new("signal", &id));
    assert_eq!(published.scope, format!("signal:{OWN_ACI}"));
    let state = record
        .read(accounts::person::PersonAccounts::read)
        .unwrap()
        .unwrap();
    assert_eq!(state.accounts.len(), 2);
    assert!(
        state
            .accounts
            .get(&tilo_resource::Id::from_raw("existing-mail"))
            .is_some()
    );
    let new = state
        .accounts
        .get(&tilo_resource::Id::from_raw(&id))
        .unwrap();
    assert_eq!(new.display_name, "New Signal");
    assert_eq!(
        new.config.0,
        serde_json::json!({"messaging_scope":published.scope,"linking_generation":1})
    );
    assert_eq!(enrollment.state(), tilo_messaging::AccountState::Offline);
    assert!(apps.messaging.is_none());
    assert!(apps.messaging_services.is_empty());
    assert!(apps.credential_owner.is_none());
    let before = state;
    let (duplicate, candidate, second_provider) = completed_first_enrollment(&apps);
    assert!(
        apps.publish_native_signal_enrollment(candidate, "Duplicate".into())
            .is_err()
    );
    assert_eq!(
        record
            .read(accounts::person::PersonAccounts::read)
            .unwrap()
            .unwrap(),
        before
    );
    enrollment.cancel();
    duplicate.cancel();
    provider.join().unwrap();
    second_provider.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn native_profile_custody_refuses_local_key_before_storage_and_retired_enrollment() {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("profile-custody.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("profile-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let startup = native_startup(&rig, &mut apps);
    let account = apps
        .start_signal_account(startup, |_| Err(SendError::Unavailable))
        .unwrap();
    let profile = rig.root.path().join("never-created-profile");
    assert!(account.create_profile(&profile).is_err());
    assert!(account.open_profile(&profile).is_err());
    assert!(
        !profile.exists(),
        "local key custody must refuse before opening profile storage"
    );
    let (enrollment, candidate, provider) = completed_first_enrollment(&apps);
    lifecycle.lifecycle().set_locked(true);
    assert!(
        apps.prepare_native_signal_profile_key(&candidate, "https://messaging.test")
            .is_err()
    );
    assert!(account.create_profile(&profile).is_err());
    lifecycle.lifecycle().set_locked(false);
    assert!(
        apps.prepare_native_signal_profile_key(&candidate, "https://messaging.test")
            .is_err()
    );
    assert!(account.open_profile(&profile).is_err());
    assert!(!profile.exists());
    drop(account);
    assert!(
        apps.credential_owner
            .as_ref()
            .unwrap()
            .read(SecretStore::list)
            .unwrap()
            .iter()
            .all(|item| !item.fields.contains_key("tilo.messaging.profile.scope"))
    );
    enrollment.cancel();
    provider.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn verified_enrollment_profile_key_review_sealed_create_resume_and_revoke_use_original_owner() {
    exercise_verified_enrollment_profile(false);
}

#[cfg(feature = "signal-profile-native-test")]
#[test]
fn verified_enrollment_ram_profile_native_qualification() {
    if crate::shell::in_process::messaging::working_profile::tests::in_noswap_namespace(concat!(
        module_path!(),
        "::verified_enrollment_ram_profile_native_qualification"
    )) {
        return;
    }
    exercise_verified_enrollment_profile(true);
}

fn exercise_verified_enrollment_profile(include_ram: bool) {
    // Existing host fixture APIs isolate both paths and secret backend. This
    // exercises the production custody contract, never the user's OS keyring.
    let rig = Rig::new();
    let _paths = tilo_paths::scope_profile(rig.root.path().join("isolated-profile-owner"));
    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("sealed-profile-owner.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("sealed-profile-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    let vault = accounts::CredentialOwner::deferred(
        &rig.owner.principal,
        &apps.device_id,
        SecretStore::open,
    )
    .unwrap();
    apps.install_credential_owner(vault.clone()).unwrap();
    let (enrollment, candidate, provider) = completed_first_enrollment(&apps);
    let owner = candidate.owner().clone();
    let id = apps
        .prepare_native_signal_profile_key(&candidate, "https://messaging.test")
        .unwrap();
    assert!(
        vault.grants().unwrap().is_empty(),
        "preparation creates no permission"
    );
    assert!(
        apps.prepare_native_signal_profile_key(&candidate, "https://messaging.test")
            .is_err(),
        "never re-key an occupied scope"
    );
    let identity = apps.native_messaging_identity(owner.clone()).unwrap();
    let request = AccessRequest {
        app_id: "so.tilo.messages".into(),
        item_id: id,
        purpose: "Use fixture Signal encryption profile".into(),
        origin: Some("https://messaging.test".into()),
    };
    let credential = apps
        .native_messaging_credential(&identity, request.clone())
        .unwrap();
    assert!(credential.profile_key().is_err());
    let proposal = apps
        .propose_native_messaging_permission(&identity, request)
        .unwrap();
    approve_fixture_profile_key(&mut apps, &credential, proposal);
    let linked = apps
        .publish_native_signal_enrollment(candidate, "Fixture Signal".into())
        .unwrap();
    assert_eq!(linked.account.account, owner.account);
    seed_person(
        &mut apps,
        &tilo_contact_directory::ContactDirectory::default(),
    );
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    let authority = apps
        .native_messaging_authority(&identity, &credential, &linked.scope)
        .unwrap();
    let account = apps
        .start_signal_account(
            SignalAccountStartup {
                identity: identity.clone(),
                credential,
                authority,
            },
            |_| Err(SendError::Unavailable),
        )
        .unwrap();
    drop(identity);
    let root = rig.root.path().join("sealed-account-profile");
    exercise_native_sealed_profile(&account, &vault, &root, include_ram);
    assert_ne!(
        account.client().account_state(&linked.account),
        AccountState::Ready
    );
    drop(account);
    enrollment.cancel();
    provider.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}

fn exercise_native_sealed_profile(
    account: &NativeSignalAccount,
    vault: &accounts::CredentialOwner,
    root: &std::path::Path,
    include_ram: bool,
) {
    let mut profile = account.create_profile(root).unwrap();
    let mut image = profile.begin_use().unwrap();
    image
        .put(
            "data/account".into(),
            zeroize::Zeroizing::new(b"fixture-profile-secret".to_vec()),
        )
        .unwrap();
    profile.checkpoint(image).unwrap();
    drop(profile);
    let sealed = std::fs::read(root.join("profile.enc")).unwrap();
    assert!(
        !sealed
            .windows(b"fixture-profile-secret".len())
            .any(|part| part == b"fixture-profile-secret")
    );
    if include_ram {
        exercise_native_ram_profile_before_launch(account, root);
    }
    let mut resumed = account.open_profile(root).unwrap();
    let image = resumed.begin_use().unwrap();
    assert_eq!(
        image.file("data/account").unwrap(),
        Some(b"fixture-profile-secret".as_slice())
    );
    vault.revoke_app("so.tilo.messages").unwrap();
    assert!(image.file("data/account").is_err());
    resumed.checkpoint(image).unwrap();
    drop(resumed);
    assert!(account.open_profile(root).is_err());
}

fn exercise_native_ram_profile_before_launch(
    account: &NativeSignalAccount,
    root: &std::path::Path,
) {
    let runtime = tempfile::Builder::new()
        .prefix("tilo-signal-custody-")
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap();
    let link = runtime.path().join("untrusted-runtime");
    std::os::unix::fs::symlink(runtime.path(), &link).unwrap();
    assert!(account.materialize_profile(root, &link).is_err());
    assert!(!root.join("active.enc").exists());
    {
        let mut working = account.materialize_profile(root, runtime.path()).unwrap();
        assert!(root.join("active.enc").exists());
        // An invalid package receives no mount source, so cancellation still has
        // proof that this owner never handed its working image to a provider.
        let mut options = launcher::run::RunOptions::default();
        assert!(working.configure_launch(&mut options).is_err());
        assert!(options.data_home.is_none());
        for mut refused in [
            launcher::run::RunOptions {
                allow_unsigned_runtime: true,
                ..signal_profile_launch_options()
            },
            launcher::run::RunOptions {
                landlock_enforce: false,
                ..signal_profile_launch_options()
            },
            launcher::run::RunOptions {
                capabilities_path: Some(runtime.path().join("extra-grants.json")),
                ..signal_profile_launch_options()
            },
        ] {
            assert!(working.configure_launch(&mut refused).is_err());
            assert!(refused.data_home.is_none());
        }
        working.cancel_before_launch().unwrap();
    }
    assert!(!root.join("active.enc").exists());
    assert_eq!(std::fs::read_dir(runtime.path()).unwrap().count(), 1);
    exercise_native_ram_profile_handoff(account, root);
}

fn exercise_native_ram_profile_handoff(account: &NativeSignalAccount, root: &std::path::Path) {
    let root = root.join("isolated-handoff-profile");
    {
        let mut sealed = account.create_profile(&root).unwrap();
        let mut image = sealed.begin_use().unwrap();
        image
            .put(
                "data/account".into(),
                zeroize::Zeroizing::new(b"fixture-profile-secret".to_vec()),
            )
            .unwrap();
        sealed.checkpoint(image).unwrap();
    }
    let runtime = tempfile::Builder::new()
        .prefix("tilo-signal-handoff-")
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap();
    {
        let mut working = account.materialize_profile(&root, runtime.path()).unwrap();
        let mut options = signal_profile_launch_options();
        working.configure_launch(&mut options).unwrap();
        let profile = launcher::appdir::host_app_dir(
            options.data_home.as_ref().unwrap(),
            launcher::appdir::PERSONAL_ROOM,
            "so.tilo.signal-provider",
        );
        assert_eq!(
            std::fs::read(profile.join("data/account")).unwrap(),
            b"fixture-profile-secret"
        );
        assert_eq!(options.identity, launcher::run::IdentitySource::None);
        assert!(options.no_services);
        assert!(working.configure_launch(&mut options).is_err());
        assert!(working.cancel_before_launch().is_err());
        assert_eq!(
            std::fs::read(profile.join("data/account")).unwrap(),
            b"fixture-profile-secret",
            "refused cancellation must retain the handed-out working tree"
        );
        drop(working);
    }
    assert!(root.join("active.enc").exists());
    assert!(account.open_profile(&root).is_err());
    assert_eq!(std::fs::read_dir(runtime.path()).unwrap().count(), 0);
}

fn signal_profile_launch_options() -> launcher::run::RunOptions {
    launcher::run::RunOptions {
        app_id: Some("so.tilo.signal-provider".into()),
        expected_package_hash: Some("fixture-package-selection".into()),
        mode: launcher::run::LaunchMode::Service {
            passthrough_argv: Vec::new(),
        },
        ..Default::default()
    }
}

fn approve_fixture_profile_key(
    apps: &mut super::super::super::InProcessApps,
    credential: &NativeMessagingCredential,
    proposal: String,
) {
    let component = apps
        .mount(
            passwords::definition(),
            "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc",
            42,
            RecordId::room_default(),
            |_| {},
        )
        .unwrap();
    let mut harness = ui::headless::FrameHarness::from_component(
        component,
        (1100, 760),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    harness.relayout();
    let approve = passwords::PasswordsEffect::ApproveAccess {
        proposal,
        lifetime: "session".into(),
    };
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: 42,
        address: tilo_actions::ActionAddress::new("passwords.vault", approve.id()),
    };
    assert!(
        harness
            .start_bound_job(&address, &approve.arguments(), false)
            .is_err()
    );
    assert!(credential.profile_key().is_err());
    let (_, started) = harness
        .start_bound_job(&address, &approve.arguments(), true)
        .unwrap();
    assert!(started.completed().is_some());
    assert!(credential.profile_key().is_ok());
    harness.close_components();
    drop(harness);
}

#[test]
fn retired_native_enrollment_never_initializes_the_canonical_account_owner() {
    let rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("retired-publication.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("retired-publication-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    let (enrollment, candidate, provider) = completed_first_enrollment(&apps);
    lifecycle.lifecycle().set_locked(true);
    lifecycle.lifecycle().set_locked(false);
    assert!(
        apps.publish_native_signal_enrollment(candidate, "Stale identity".into())
            .is_err()
    );
    let initialized = std::cell::Cell::new(0);
    let settings = ui::record::person::PersonRecords::open(
        &mut apps.owners,
        accounts::person::ACCOUNTS_OWNER,
        apps.author.clone(),
    )
    .unwrap();
    let record = settings
        .open_original_record(
            ui::record::RecordKind::new(accounts::person::PersonAccounts::KIND),
            |doc| {
                initialized.set(initialized.get() + 1);
                accounts::person::PersonAccounts::initialize(doc).map_err(|error| error.to_string())
            },
        )
        .unwrap();
    assert_eq!(
        initialized.get(),
        1,
        "retired evidence never opened an empty canonical owner"
    );
    assert!(
        record
            .read(accounts::person::PersonAccounts::read)
            .unwrap()
            .unwrap()
            .accounts
            .is_empty()
    );
    assert!(apps.messaging.is_none());
    enrollment.cancel();
    provider.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}

fn assert_private_setup_presentation(harness: &mut ui::headless::FrameHarness, shown: bool) {
    assert_eq!(
        harness.a11y_nodes().iter().any(|(_, node)| node.label()
            == Some("Private device linking code for the selected account")),
        shown
    );
    assert!(!harness.dump_json().contains("synthetic-private-link-key"));
    assert!(!harness.a11y_nodes().iter().any(|(_, node)| {
        node.value()
            .is_some_and(|value| value.contains("synthetic-private-link-key"))
    }));
    let private = ui::secret_paint::SecretPaint::begin();
    harness.relayout();
    assert_eq!(
        private.finish(),
        shown,
        "the actual mounted private code owns capture exclusion only while shown"
    );
}

#[test]
fn actual_session_lock_retires_visible_private_link_and_never_revives_after_unlock() {
    let mut rig = Rig::new();
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("revoked-link-broker.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = super::super::super::InProcessApps::native_for_profile(
        &rig.root.path().join("apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let startup = native_startup(&rig, &mut apps);
    let account = apps
        .start_signal_account(startup, |_| Err(SendError::Unavailable))
        .unwrap();
    let (setup, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(ui::time::Duration::from_secs(5)))
        .unwrap();
    let linker = account.prepare_linking(OWN_ACI.into(), setup).unwrap();
    let daemon = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let start: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(start["method"], "startLink");
        writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0","id":start["id"],"result":{"deviceLinkUri":"sgnl://linkdevice?uuid=22222222-2222-4222-8222-222222222222&pub_key=synthetic-private-link-key"}})).unwrap();
        assert_eq!(
            reader.read(&mut [0]).unwrap(),
            0,
            "no finishLink after original session lock"
        );
    });
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenAccountSetup,
        false,
    )
    .unwrap();
    effect(
        &mut mounted.harness,
        &MessagesEffect::SelectSetupAccount {
            service: "signal".into(),
            account: "fixture-signal".into(),
        },
        false,
    )
    .unwrap();
    let result = effect(
        &mut mounted.harness,
        &MessagesEffect::BeginLink {
            service: "signal".into(),
            account: "fixture-signal".into(),
        },
        true,
    )
    .unwrap();
    let complete = MessagesEffect::CompleteLink {
        service: "signal".into(),
        account: "fixture-signal".into(),
        attempt: result.value.unwrap()["attempt"].as_str().unwrap().into(),
    };
    assert_private_setup_presentation(&mut mounted.harness, true);
    lifecycle.lifecycle().set_locked(true);
    assert_eq!(linker.state(), AccountState::Locked);
    mounted.harness.relayout();
    assert_private_setup_presentation(&mut mounted.harness, false);
    assert!(effect(&mut mounted.harness, &complete, true).is_err());
    lifecycle.lifecycle().set_locked(false);
    assert_eq!(linker.state(), AccountState::Locked);
    assert!(effect(&mut mounted.harness, &complete, true).is_err());
    drop(mounted);
    drop(linker);
    drop(account);
    drop(apps);
    daemon.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}

#[path = "messaging_first_account_tests.rs"]
mod first_account_journey;

#[test]
fn messaging_network_consent_requires_protected_confirmed_answer_and_retires_with_owner() {
    use crate::portal::{PromptQueueHandle, PromptResolver};
    use crate::shell::chrome_state::SearchScope;
    use crate::shell::chrome_state::{ChromeState, PendingPrompt};
    use crate::shell::in_process::messaging::network;
    use crate::shell::search::SearchRegistry;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ArgValue, ResourceRef};
    use crate::shell::search::providers::permission_prompts::{
        PROVIDER_ID, PermissionPromptsProvider,
    };
    use crate::shell::state::WorkspaceTree;
    for (decision, allowed) in [
        ("deny", false),
        ("allow-once", true),
        ("allow-session", true),
        ("allow-always", false),
    ] {
        let rig = Rig::new();
        let identity = rig.startup().identity;
        let queue = PromptQueueHandle::new(PromptResolver::Channel);
        let cancellation = launcher::owned_service::ServiceCancellation::default();
        let policy = network::Policy {
            prompts: Some(queue.clone()),
            cancellation: cancellation.clone(),
        };
        let mut worker = Some(std::thread::spawn(move || {
            network::request(identity, &policy)
        }));
        let pending = wait_network_prompt(&queue);
        assert_eq!(pending.caller.app_id, "so.tilo.messages");
        let crate::shell::state::Capability::Network { ref hosts, scope } = pending.capability
        else {
            panic!("network review")
        };
        assert_eq!(scope, crate::shell::state::GRANT_ONCE);
        let hosts = hosts.as_ref().unwrap().clone();
        assert_eq!(hosts.len(), 7);
        assert!(
            hosts
                .iter()
                .all(|host| host.ends_with(".signal.org") && !host.contains('*'))
        );
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        chrome.borrow_mut().test_only_owner_override = Some(true);
        let prompt =
            PendingPrompt::with_reply(pending.caller.app_id, pending.capability, pending.reply);
        let id = format!("{decision}@{}", prompt.id);
        chrome.borrow_mut().pending_grants.push_back(prompt);
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(PermissionPromptsProvider::default()));
        let actions = ActionCatalog::build_exact(
            &registry,
            &ResourceRef::new(PROVIDER_ID, &id),
            SearchScope::All,
            None,
            &chrome.borrow(),
            &automerge::AutoCommit::new(),
            &automerge::ROOT,
            &AllVisible,
        );
        let action = actions.select(&ResourceRef::new(PROVIDER_ID, &id)).unwrap();
        let mut doc = automerge::AutoCommit::new();
        assert!(
            action
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &chrome,
                    []
                )
                .is_err()
        );
        assert!(
            !worker.as_ref().unwrap().is_finished(),
            "unconfirmed app request cannot create network consent"
        );
        action
            .invoke(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                [("confirm".into(), ArgValue::Boolean(true))],
            )
            .unwrap();
        match worker.take().unwrap().join().unwrap() {
            Ok(consent) if allowed => {
                assert_eq!(consent.hosts().unwrap(), hosts);
                cancellation.cancel();
                assert!(consent.hosts().is_err());
                drop(consent);
            }
            Err(SendError::Revoked) if !allowed => {}
            _ => panic!("protected temporary network decision differs"),
        }
        drop(worker);
        assert!(chrome.borrow().pending_grants.is_empty());
        assert!(rig.admission_live());
        drop(rig);
    }
}

fn wait_network_prompt(queue: &crate::portal::PromptQueueHandle) -> crate::portal::PendingGrant {
    let deadline = std::time::Instant::now()
        .checked_add(ui::time::Duration::from_secs(5))
        .unwrap();
    loop {
        if let Some(pending) = queue.drain().pop() {
            return pending;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "original network prompt must be observable"
        );
        std::thread::park_timeout(ui::time::Duration::from_millis(1));
    }
}

#[path = "messaging_sms_first_tests.rs"]
mod sms_first_journey;

#[path = "messaging_native_first_tests.rs"]
mod native_first_journey;
