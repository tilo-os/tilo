use super::*;
use crate::shell::in_process::messaging::{first_account, working_profile::provision};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

struct FixtureSource {
    streams: Mutex<Option<(UnixStream, UnixStream)>>,
    linking: Mutex<Vec<std::thread::JoinHandle<()>>>,
    activations: AtomicUsize,
    history_root: Mutex<Option<PathBuf>>,
}
impl first_account::Source for FixtureSource {
    fn provision(
        &self,
        identity: NativeMessagingIdentity,
        _: &Path,
        policy: crate::shell::in_process::messaging::network::Policy,
    ) -> Result<provision::Provisioning, SendError> {
        identity.check()?;
        let _review =
            crate::shell::in_process::messaging::network::request(identity.clone(), &policy)?;
        let (stream, peer) = UnixStream::pair().unwrap();
        self.linking.lock().unwrap().push(link_daemon(peer));
        let (send, stopped) = mpsc::sync_channel(1);
        send.send(Ok(
            provision::UnpublishedSignalProfile::writer_free_fixture(identity),
        ))
        .map_err(|_| SendError::Unavailable)?;
        Ok(provision::Provisioning { stream, stopped })
    }
    fn activate(
        &self,
        account: &Arc<NativeSignalAccount>,
        wake: crate::session_runtime::Wake,
        storage: NativeSignalStorage,
        _: crate::shell::in_process::messaging::network::Policy,
    ) -> Result<NativeSignalActivation, SendError> {
        self.activations.fetch_add(1, Ordering::SeqCst);
        *self.history_root.lock().unwrap() = Some(storage.history.clone());
        let (sender, receiver) = self.streams.lock().unwrap().take().unwrap();
        account.activate_native_on_worker(
            wake,
            NativeSignalHandles {
                private_root: storage.history,
                signal_account: storage.signal_account,
                sender,
                receiver,
            },
        )
    }
}

#[test]
fn empty_native_messages_first_account_protected_review_receive_reply_and_revoke() {
    let mut rig = Rig::new();
    let _paths = tilo_paths::scope_profile(rig.root.path().join("first-owner"));
    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("first-account.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = crate::shell::in_process::InProcessApps::native_for_profile(
        &rig.root.path().join("first-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let vault = accounts::CredentialOwner::deferred(
        &rig.owner.principal,
        &apps.device_id,
        SecretStore::open,
    )
    .unwrap();
    apps.install_credential_owner(vault.clone()).unwrap();
    rig.broker = vault.broker().unwrap();
    let (sender, send_peer) = UnixStream::pair().unwrap();
    let (receiver, receive_peer) = UnixStream::pair().unwrap();
    let source = Arc::new(FixtureSource {
        streams: Mutex::new(Some((sender, receiver))),
        linking: Mutex::default(),
        activations: AtomicUsize::new(0),
        history_root: Mutex::default(),
    });
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    apps.bind_messaging_network_prompts(crate::portal::PromptQueueHandle::new(
        crate::portal::PromptResolver::AutoOutcome(crate::portal::PromptOutcome::AllowOnce),
    ));
    apps.first_accounts = Some(first_account::fixture(
        rig.owner.principal.clone(),
        Arc::new(|| {}),
        source.clone(),
    ));
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    let (account, proposal) = link_and_review(&mut apps, &mut mounted, &source, &vault, &mut rig);
    let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
    let credential = apps
        .native_messaging_credential(&identity, rig.request.clone())
        .unwrap();
    drop(identity);
    approve_fixture_profile_key(&mut apps, &credential, proposal);
    drop(credential);
    seed_person(&mut apps, &directory());
    let writes = Arc::new(AtomicUsize::new(0));
    let daemon = sender_daemon(send_peer, writes.clone());
    let (arrive, inbox) = receiver_daemon(receive_peer);
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |_| {
                apps.pump_messaging_records();
                mounted.client.account_state(&account) == AccountState::Ready
            }),
        "the initially empty app loan must become usable after original protected approval"
    );
    assert_eq!(source.activations.load(Ordering::SeqCst), 1);
    let reply = receive_and_reply(&mut apps, &mut mounted, &account, &arrive);
    let history_root = source.history_root.lock().unwrap().clone().unwrap();
    let paths = [
        history_root.join("history/history.enc"),
        history_root.join("journal/vault.enc"),
    ];
    let ciphertexts = paths.each_ref().map(|path| std::fs::read(path).unwrap());
    revoke_native_permission(&rig, &mut apps, &mut mounted, &reply, &writes);
    assert!(
        effect(
            &mut mounted.harness,
            &MessagesEffect::ResetFirstEnrollment,
            false
        )
        .is_err(),
        "a published account cannot be reset as an unpublished linking attempt"
    );
    assert_eq!(
        paths.each_ref().map(|path| std::fs::read(path).unwrap()),
        ciphertexts,
        "original protected revocation must leave private history and send journal unchanged"
    );
    drop(rig);
    drop(mounted);
    drop(apps);
    daemon.join().unwrap();
    inbox.join().unwrap();
    for thread in source.linking.lock().unwrap().drain(..) {
        thread.join().unwrap();
    }
}

fn link_and_review(
    apps: &mut crate::shell::in_process::InProcessApps,
    mounted: &mut MountedMessages,
    source: &FixtureSource,
    vault: &accounts::CredentialOwner,
    rig: &mut Rig,
) -> (ServiceAccount, String) {
    assert!(apps.messaging_services.is_empty());
    assert!(vault.grants().unwrap().is_empty());
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenAccountSetup,
        false,
    )
    .unwrap();
    let begin = MessagesEffect::BeginFirstEnrollment {
        service: "signal".into(),
    };
    assert!(effect(&mut mounted.harness, &begin, false).is_err());
    let pairing = effect_with_pump(&mut mounted.harness, &begin, true, || {
        apps.pump_messaging_records();
    })
    .unwrap()
    .value
    .unwrap();

    let id = pairing["account"].as_str().unwrap().to_owned();
    rig.owner.account = id.clone();
    let account = ServiceAccount::new("signal", &id);
    assert!(
        !mounted
            .harness
            .dump_json()
            .contains("synthetic-private-link-key")
    );
    let finish = MessagesEffect::CompleteFirstEnrollment {
        service: "signal".into(),
        account: id.clone(),
        attempt: pairing["attempt"].as_str().unwrap().into(),
    };
    effect_with_pump(&mut mounted.harness, &finish, true, || {
        apps.pump_messaging_records();
    })
    .unwrap();
    assert!(apps.messaging_services.is_empty());
    assert_eq!(source.activations.load(Ordering::SeqCst), 0);
    let review = MessagesEffect::ReviewFirstEnrollment {
        service: "signal".into(),
        account: id,
        display_name: "Fixture Signal".into(),
    };
    let proposal = effect_with_pump(&mut mounted.harness, &review, true, || {
        apps.pump_messaging_records();
    })
    .unwrap()
    .value
    .unwrap()["proposal"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(vault.grants().unwrap().is_empty());
    assert_eq!(source.activations.load(Ordering::SeqCst), 0);
    let pending = vault.proposals().unwrap();
    let item = pending
        .iter()
        .find(|pending| pending.id == proposal)
        .unwrap()
        .request
        .item_id
        .clone();
    rig.request = AccessRequest {
        app_id: "so.tilo.messages".into(),
        item_id: item,
        purpose: "Private Signal messaging".into(),
        origin: Some("https://signal.org".into()),
    };
    (account, proposal)
}

fn receive_and_reply(
    apps: &mut crate::shell::in_process::InProcessApps,
    mounted: &mut MountedMessages,
    account: &ServiceAccount,
    arrive: &mpsc::Sender<()>,
) -> MessagesEffect {
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenPerson {
            person: PERSON.into(),
        },
        false,
    )
    .unwrap();
    let selected = route();
    let choice = serde_json::json!({"service":"signal","account":account.account,"recipient":selected.recipient,"association":selected.association.as_str(),"scope":selected.scope,"revision":selected.revision}).to_string();
    effect(
        &mut mounted.harness,
        &MessagesEffect::SelectRoute {
            choice: choice.clone(),
        },
        false,
    )
    .unwrap();
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
            })
    );
    let reply = MessagesEffect::Send {
        choice,
        operation: "native-reply-operation".into(),
        text: "native private reply".into(),
    };
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
    reply
}

#[test]
fn interrupted_first_account_reset_retires_old_review_before_fresh_pairing() {
    let mut rig = Rig::new();
    let _paths = xdg_paths::scope_profile(rig.root.path().join("first-owner"));
    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("first-account.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = crate::shell::in_process::InProcessApps::native_for_profile(
        &rig.root.path().join("first-apps"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    let vault = accounts::CredentialOwner::deferred(
        &rig.owner.principal,
        &apps.device_id,
        SecretStore::open,
    )
    .unwrap();
    apps.install_credential_owner(vault.clone()).unwrap();
    rig.broker = vault.broker().unwrap();
    let (sender, _send_peer) = UnixStream::pair().unwrap();
    let (receiver, _receive_peer) = UnixStream::pair().unwrap();
    let source = Arc::new(FixtureSource {
        streams: Mutex::new(Some((sender, receiver))),
        linking: Mutex::default(),
        activations: AtomicUsize::new(0),
        history_root: Mutex::default(),
    });
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    apps.bind_messaging_network_prompts(crate::portal::PromptQueueHandle::new(
        crate::portal::PromptResolver::AutoOutcome(crate::portal::PromptOutcome::AllowOnce),
    ));
    apps.first_accounts = Some(first_account::fixture(
        rig.owner.principal.clone(),
        Arc::new(|| {}),
        source.clone(),
    ));
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    let (old, proposal) = link_and_review(&mut apps, &mut mounted, &source, &vault, &mut rig);
    assert!(
        effect(
            &mut mounted.harness,
            &MessagesEffect::ResetFirstEnrollment,
            false
        )
        .is_err(),
        "pending protected review cannot be silently reset"
    );
    vault
        .decide_proposal(&proposal, accounts::AccessDecision::Deny)
        .unwrap();
    let fresh = reset_and_begin(&mut apps, &mut mounted, &old);
    assert_ne!(fresh["account"].as_str().unwrap(), old.account);
    assert!(vault.grants().unwrap().is_empty());
    assert!(apps.messaging_services.is_empty());
    assert_eq!(source.activations.load(Ordering::SeqCst), 0);
    let finish = MessagesEffect::CompleteFirstEnrollment {
        service: "signal".into(),
        account: fresh["account"].as_str().unwrap().into(),
        attempt: fresh["attempt"].as_str().unwrap().into(),
    };
    effect_with_pump(&mut mounted.harness, &finish, true, || {
        apps.pump_messaging_records();
    })
    .unwrap();
    assert!(
        effect(
            &mut mounted.harness,
            &MessagesEffect::ResetFirstEnrollment,
            false
        )
        .is_err(),
        "a fresh reviewed account is not an interrupted attempt"
    );
    assert!(
        !mounted
            .harness
            .dump_json()
            .contains("synthetic-private-link-key")
    );
    effect(&mut mounted.harness, &MessagesEffect::CancelLink, false).unwrap();
    apps.pump_messaging_records();
    assert!(vault.grants().unwrap().is_empty());
    assert_eq!(source.activations.load(Ordering::SeqCst), 0);
    drop(rig);
    drop(mounted);
    drop(apps);
    for thread in source.linking.lock().unwrap().drain(..) {
        thread.join().unwrap();
    }
}

fn reset_and_begin(
    apps: &mut crate::shell::in_process::InProcessApps,
    mounted: &mut MountedMessages,
    old: &ServiceAccount,
) -> serde_json::Value {
    assert!(
        mounted
            .harness
            .wait_until(ui::time::Duration::from_secs(5), |_| {
                apps.pump_messaging_records();
                apps.first_accounts.as_ref().unwrap().client().phase(old)
                    == tilo_messaging::EnrollmentPhase::Interrupted
            })
    );
    effect(
        &mut mounted.harness,
        &MessagesEffect::ResetFirstEnrollment,
        false,
    )
    .unwrap();
    // Do not pump the host between reset and the explicit Begin. The host must
    // retire the cancelled old slot before handling this already-queued request.
    let begin = MessagesEffect::BeginFirstEnrollment {
        service: "signal".into(),
    };
    assert!(
        effect(&mut mounted.harness, &begin, false).is_err(),
        "reset does not bypass confirmation for new provider activity"
    );
    effect_with_pump(&mut mounted.harness, &begin, true, || {
        apps.pump_messaging_records();
    })
    .unwrap()
    .value
    .unwrap()
}
