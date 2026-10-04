use std::time::Duration;

use publisher::{App, Publisher};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};
use ui::tilo_actions::{ActionAddress, DoorError};

const KEY: &str = "so.tilo.publisher.key:root";

fn invoke(app: &mut TestApp<App>, action: &str, confirm: bool) -> Result<(), DoorError> {
    app.harness()
        .invoke_action(
            &ActionAddress::new(KEY, action),
            &ui::serde_json::Map::new(),
            confirm,
            None,
        )
        .map(|_| ())
}

/// Whether the app now says `fragment` in a notice: its headline or its detail.
fn says(app: &mut TestApp<App>, fragment: &str, within: Duration) -> bool {
    app.harness().wait_until(within, |driver| {
        driver.a11y_nodes().iter().any(|(_, node)| {
            [node.label(), node.description()]
                .into_iter()
                .flatten()
                .any(|text| text.contains(fragment))
        })
    })
}

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
        invoke(&mut app, "create-key", false).is_err(),
        "creating a key is confirmed first"
    );
    invoke(&mut app, "create-key", true).expect("create the key");
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

    invoke(&mut app, "create-key", true).expect("the job starts");
    assert!(
        says(&mut app, "exists", Duration::from_secs(2)),
        "an existing key is never replaced"
    );
    assert_eq!(
        registry_client::key::load(&written)
            .map(|again| again.to_bytes())
            .ok(),
        Some(key.to_bytes())
    );
}

#[test]
fn applying_reports_what_the_registry_answered() {
    let _home = home();
    let mut app = TestApp::<App>::new().expect("the app opens");
    invoke(&mut app, "create-key", true).expect("create the key");
    app.invoke(Publisher::SET_REGISTRY.with("http://127.0.0.1:9".into()))
        .expect("an unreachable registry");
    assert!(
        invoke(&mut app, "apply", false).is_ok(),
        "the application starts as a job"
    );
    assert!(
        says(
            &mut app,
            "name a domain or a forge account",
            Duration::from_secs(5)
        ),
        "an application with nothing to claim says so"
    );

    app.invoke(Publisher::SET_DOMAIN.with("example.com".into()))
        .expect("domain");
    invoke(&mut app, "apply", false).expect("apply again");
    assert!(
        says(&mut app, "127.0.0.1:9", Duration::from_secs(10)),
        "an unreachable registry is named in the notice"
    );
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
