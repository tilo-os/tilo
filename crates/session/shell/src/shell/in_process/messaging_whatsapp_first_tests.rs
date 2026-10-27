//! Default app effects must obtain the original protected decision before SDK assembly.
use super::*;
use crate::shell::in_process::messaging::{first_account, whatsapp_first};
use tilo_messaging_protocol::supervisor::whatsapp::WhatsappLinking;
#[derive(Default)]
struct RefusingSource {
    activations: AtomicUsize,
    states: Mutex<Vec<AccountState>>,
}
impl whatsapp_first::Source for RefusingSource {
    fn activate(
        &self,
        account: &crate::shell::in_process::messaging::whatsapp::NativeWhatsappAccount,
        _: &std::path::Path,
        _: String,
        _: &crate::shell::in_process::messaging::network::Policy,
    ) -> Result<Arc<WhatsappLinking>, SendError> {
        self.states
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .push(account.state());
        self.activations.fetch_add(1, Ordering::SeqCst);
        Err(SendError::Unavailable)
    }
}
#[test]
fn default_whatsapp_preflight_denial_never_starts_sdk_or_publishes_account() {
    protected_preflight(false);
}
#[test]
fn default_whatsapp_approved_preflight_invokes_factory_and_fails_closed() {
    protected_preflight(true);
}
fn protected_preflight(approve: bool) {
    let mut rig = Rig::new();
    let _paths = tilo_paths::scope_profile(rig.root.path().join("wa-owner"));
    let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
    let lifecycle = LocalSessionAuthority::new();
    lifecycle.lifecycle().publish_native_state(false, false);
    let socket = rig.root.path().join("wa-first.sock");
    let _source = source(&rig, &lifecycle, &socket);
    let mut apps = crate::shell::in_process::InProcessApps::native_for_profile(
        &rig.root.path().join("wa-apps"),
        &rig.root.path().join("wa-apps").join("person"),
        &rig.owner.principal,
        socket,
    )
    .unwrap();
    rig.owner.device = apps.device_id.clone();
    rig.owner.service = Service::Whatsapp;
    let vault = accounts::CredentialOwner::deferred(
        &rig.owner.principal,
        &apps.device_id,
        SecretStore::open,
    )
    .unwrap();
    apps.install_credential_owner(vault.clone()).unwrap();
    rig.broker = vault.broker().unwrap();
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    let fixture = Arc::new(RefusingSource::default());
    apps.first_accounts = Some(first_account::fixture_whatsapp(
        rig.owner.principal.clone(),
        Arc::new(|| {}),
        fixture.clone(),
    ));
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    let original_document = mounted.harness.dump_json();
    effect(
        &mut mounted.harness,
        &MessagesEffect::OpenAccountSetup,
        false,
    )
    .unwrap();
    let begin = MessagesEffect::BeginFirstEnrollment {
        service: "whatsapp".into(),
    };
    assert!(effect(&mut mounted.harness, &begin, false).is_err());
    let review = effect_with_pump(&mut mounted.harness, &begin, true, || {
        apps.pump_messaging_records();
    })
    .unwrap()
    .value
    .unwrap();
    assert_eq!(review["state"], "review");
    rig.owner.account = review["account"].as_str().unwrap().into();
    assert_eq!(fixture.activations.load(Ordering::SeqCst), 0);
    assert!(apps.messaging_services.is_empty());
    assert!(vault.grants().unwrap().is_empty());
    let request = MessagesEffect::ReviewFirstEnrollment {
        service: "whatsapp".into(),
        account: rig.owner.account.clone(),
        display_name: "Fixture WhatsApp".into(),
    };
    let proposed = effect_with_pump(&mut mounted.harness, &request, true, || {
        apps.pump_messaging_records();
    })
    .unwrap()
    .value
    .unwrap();
    let proposal = proposed["proposal"].as_str().unwrap().to_owned();
    rig.request = vault
        .proposals()
        .unwrap()
        .into_iter()
        .find(|p| p.id == proposal)
        .unwrap()
        .request;
    for _ in 0..3 {
        apps.pump_messaging_records();
    }
    assert_eq!(fixture.activations.load(Ordering::SeqCst), 0);
    assert!(apps.messaging_services.is_empty());
    decide(&mut apps, &rig, &vault, proposal, approve);
    let client = apps.first_accounts.as_ref().unwrap().client();
    let account = ServiceAccount::new("whatsapp", &rig.owner.account);
    observe(
        &mut apps,
        &mut mounted,
        &client,
        &account,
        &fixture,
        &original_document,
        approve,
    );
    assert!(
        rig.root.path().is_dir(),
        "the observer must retain its live original profile"
    );
    drop(rig);
}
fn observe(
    apps: &mut crate::shell::in_process::InProcessApps,
    mounted: &mut MountedMessages,
    client: &tilo_messaging::EnrollmentClient,
    account: &ServiceAccount,
    fixture: &RefusingSource,
    original_document: &str,
    approve: bool,
) {
    assert!(
        mounted
            .harness
            .wait_until(tilo_test_env::hang_bound(), |_| {
                apps.pump_messaging_records();
                client.phase(account) == tilo_messaging::EnrollmentPhase::Interrupted
            })
    );
    assert_eq!(
        fixture.activations.load(Ordering::SeqCst),
        usize::from(approve)
    );
    assert!(client.linked(account).is_none());
    assert!(apps.default_native_accounts("whatsapp").unwrap().is_empty());
    assert_eq!(
        mounted.harness.dump_json(),
        original_document,
        "protected enrollment must preserve the complete original shared document"
    );
    assert_eq!(
        *fixture.states.lock().unwrap(),
        if approve {
            vec![AccountState::Unavailable]
        } else {
            Vec::new()
        }
    );
}

fn decide(
    apps: &mut crate::shell::in_process::InProcessApps,
    rig: &Rig,
    vault: &accounts::CredentialOwner,
    proposal: String,
    approve: bool,
) {
    if approve {
        let identity = apps.native_messaging_identity(rig.owner.clone()).unwrap();
        let credential = apps
            .native_messaging_credential(&identity, rig.request.clone())
            .unwrap();
        drop(identity);
        approve_fixture_profile_key(apps, &credential, proposal);
    } else {
        vault
            .decide_proposal(&proposal, accounts::AccessDecision::Deny)
            .unwrap();
    }
}
