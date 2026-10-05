use std::time::Duration;

use publisher::{App, KeyEffect, Publisher};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};
use ui::tilo_actions::{
    Availability, ConfirmationPolicy, DoorError, PermissionRequirement, RiskLevel,
};

/// A machine of its own: a home with no key the developer's environment
/// names, and secrets kept in this test's memory.
struct Machine {
    _home: tilo_test_env::TestHome,
    _host: ui::host::HostScope,
    secrets: ui::secrets::SecretStore,
}

impl Machine {
    /// The publisher key in the machine's secret store, as `tilo-app` reads it.
    #[expect(clippy::expect_used, reason = "the in-memory store always answers")]
    fn key(&self) -> Option<registry_client::key::SigningKey> {
        registry_client::key::stored(&self.secrets).expect("the secret store answers")
    }
}

#[expect(clippy::expect_used, reason = "a test without its home cannot run")]
fn home() -> Machine {
    let mut home = tilo_test_env::TestHome::new().expect("test home");
    home.env().remove(registry_client::key::KEY_ENV);
    let services = ui::host::HostServices::null();
    let secrets = services.secrets.store(registry_client::key::SECRETS_APP);
    Machine {
        _home: home,
        _host: ui::host::enter(&services),
        secrets,
    }
}

#[test]
fn the_registry_must_be_https_or_this_machine() {
    let mut profile = Replica::<Publisher>::new();
    assert!(
        profile
            .apply(Publisher::SET_REGISTRY.with("http://apps.example.com".into()))
            .is_err()
    );
    profile
        .apply(Publisher::SET_REGISTRY.with("http://127.0.0.1:8080".into()))
        .expect("a local registry");
    assert_eq!(
        profile.state().expect("profile").registry,
        "http://127.0.0.1:8080"
    );
}

#[test]
fn creating_a_key_needs_confirmation_and_keeps_it_out_of_the_resource() {
    let home = home();
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .expect(&SemanticExpectation::named("create-key"))
        .expect("an empty machine offers to create a key");
    assert!(
        app.effect(&KeyEffect::CreateKey).is_err(),
        "creating a key is confirmed first"
    );
    app.effect_confirmed(&KeyEffect::CreateKey)
        .expect("create the key");
    app.harness()
        .wait_for_named("public-key", Duration::from_secs(2))
        .expect("the new key's public half is shown");

    let key = home.key().expect("the key tilo-app reads");
    let public = registry_client::key::public_hex(&key);
    app.harness()
        .expect(&SemanticExpectation::named("public-key").name_contains(&public))
        .expect("the public key is the stored key's");
    app.harness()
        .expect(
            &SemanticExpectation::named("proof-line")
                .name_contains(format!("tilo-publisher={public}")),
        )
        .expect("the proof line names the key");

    let secret = hex::encode(key.to_bytes());
    let state = format!("{:?}", app.state());
    assert!(
        !state.contains(&secret),
        "the secret never enters the resource"
    );

    let again = app.effect_confirmed(&KeyEffect::CreateKey);
    assert!(
        matches!(&again, Err(DoorError::Unavailable { code, .. }) if code == "key-exists"),
        "an existing key is never replaced: {again:?}"
    );
    assert_eq!(
        home.key().map(|again| again.to_bytes()),
        Some(key.to_bytes())
    );
}

#[test]
fn the_key_operations_are_published_on_the_profile_as_the_key_stands() {
    let _home = home();
    let mut app = TestApp::<App>::new().expect("the app opens");
    let resources = app.harness().resources();
    let profile = resources
        .iter()
        .find(|resource| resource.kind == "so.tilo.publisher.profile")
        .expect("the profile is published");
    assert_eq!(
        resources
            .iter()
            .filter(|resource| resource
                .actions
                .iter()
                .any(|action| action.id == "create-key"))
            .count(),
        1,
        "the key operations are published once, on the profile"
    );
    let action = |id: &str| {
        profile
            .actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("{id} is published on the profile"))
    };
    for id in ["create-key", "copy-secret"] {
        let contract = &action(id).contract;
        assert_eq!(contract.permission, PermissionRequirement::Admin, "{id}");
        assert_eq!(contract.risk, RiskLevel::Privileged, "{id}");
        assert_eq!(contract.confirmation, ConfirmationPolicy::Confirm, "{id}");
    }
    for id in ["copy-proof", "apply-for-namespaces", "check-status"] {
        assert!(
            matches!(
                &action(id).contract.availability,
                Availability::Unavailable { code, .. } if code == "no-key"
            ),
            "{id} waits for a key"
        );
    }
    let refused = app.effect(&KeyEffect::CopyProof);
    assert!(
        matches!(&refused, Err(DoorError::Unavailable { code, .. }) if code == "no-key"),
        "{refused:?}"
    );

    app.effect_confirmed(&KeyEffect::CreateKey)
        .expect("create the key");
    let resources = app.harness().resources();
    let available = |id: &str| {
        resources
            .iter()
            .flat_map(|resource| &resource.actions)
            .find(|action| action.id == id)
            .is_some_and(|action| action.contract.availability == Availability::Available)
    };
    assert!(available("copy-proof") && available("apply-for-namespaces"));
    assert!(!available("create-key"), "one key per machine");
}

#[test]
fn applying_reports_what_the_registry_answered() {
    let _home = home();
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.effect_confirmed(&KeyEffect::CreateKey)
        .expect("create the key");
    app.invoke(Publisher::SET_REGISTRY.with("http://127.0.0.1:9".into()))
        .expect("an unreachable registry");
    let nothing = app.effect(&KeyEffect::ApplyForNamespaces);
    assert!(
        matches!(&nothing, Err(DoorError::Failed(message)) if message.contains("name a domain or a forge account")),
        "the job answers its caller: {nothing:?}"
    );
    fn notice_detail(node: &ui::headless::InspectionNode) -> Option<&str> {
        if node.test_id.as_deref() == Some("notice") {
            return node
                .semantics
                .as_ref()
                .and_then(|node| node.description.as_deref());
        }
        node.children.iter().find_map(notice_detail)
    }
    let inspection = app.harness().inspection();
    let detail =
        notice_detail(&inspection.root).expect("the actual status banner publishes its detail");
    assert!(
        detail.contains("name a domain or a forge account"),
        "the application explains why it failed: {detail}"
    );

    app.invoke(Publisher::SET_DOMAIN.with("example.com".into()))
        .expect("domain");
    assert!(
        app.effect(&KeyEffect::ApplyForNamespaces).is_err(),
        "an unreachable registry fails the application"
    );
    app.harness()
        .wait_until(Duration::from_secs(10), |driver| {
            notice_detail(&driver.inspection().root)
                .is_some_and(|detail| detail.contains("127.0.0.1:9"))
        })
        .then_some(())
        .expect("an unreachable registry is named in the notice");
}

#[test]
fn the_claims_are_durable() {
    let _home = home();
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Publisher::SET_DOMAIN.with("example.com".into()))
        .expect("domain");
    app.invoke(Publisher::SET_FORGE_ACCOUNT.with("codeberg:ada".into()))
        .expect("forge");
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().domain, "example.com");
    assert_eq!(app.state().forge_account, "codeberg:ada");
}

#[test]
fn a_capture_shows_a_key_without_reading_the_machine_s() {
    let home = home();
    let mut app = TestApp::<App>::capture(Some("key")).expect("the capture opens");
    app.harness()
        .expect(&SemanticExpectation::named("proof-line").name_contains("tilo-publisher="))
        .expect("the scenario's key is shown");
    assert_eq!(app.state().domain, "lovelace.dev");
    assert!(home.key().is_none(), "a capture writes no key");
}
