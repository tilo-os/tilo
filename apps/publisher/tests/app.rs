use std::time::Duration;

use publisher::{App, KeyEffect, Publisher};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};
use ui::tilo_actions::{
    Availability, ConfirmationPolicy, DoorError, PermissionRequirement, RiskLevel,
};

/// A home of its own, with no key the developer's environment names.
#[expect(clippy::expect_used, reason = "a test without its home cannot run")]
fn home() -> tilo_test_env::TestHome {
    let mut home = tilo_test_env::TestHome::new().expect("test home");
    home.env().remove(registry_client::key::KEY_ENV);
    home
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

    let written = home.config().join("tilo/publisher-key.json");
    let key = registry_client::key::load(&written).expect("the key file tilo-app reads");
    let public = registry_client::key::public_hex(&key);
    app.harness()
        .expect(&SemanticExpectation::named("public-key").name_contains(&public))
        .expect("the public key is the file's");
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
        registry_client::key::load(&written)
            .map(|again| again.to_bytes())
            .ok(),
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
    app.harness()
        .expect(
            &SemanticExpectation::named("notice").name_contains("name a domain or a forge account"),
        )
        .expect("an application with nothing to claim says so");

    app.invoke(Publisher::SET_DOMAIN.with("example.com".into()))
        .expect("domain");
    assert!(
        app.effect(&KeyEffect::ApplyForNamespaces).is_err(),
        "an unreachable registry fails the application"
    );
    app.harness()
        .wait_until(Duration::from_secs(10), |driver| {
            driver.a11y_nodes().iter().any(|(_, node)| {
                node.label()
                    .is_some_and(|label| label.contains("127.0.0.1:9"))
            })
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
    assert_eq!(app.state().domain, "example.com");
    assert!(
        !home.config().join("tilo/publisher-key.json").exists(),
        "a capture writes no key"
    );
}
