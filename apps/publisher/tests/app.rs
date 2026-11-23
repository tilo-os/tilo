use publisher::{App, KeyEffect, Publisher};
use ui::headless::SemanticExpectation;
use ui::prelude::{Invoke, Replica, TestApp};
use ui::tilo_resource::Path;

/// Replace the text of one field of the profile, as its input does.
#[expect(clippy::expect_used, reason = "a path of the profile always encodes")]
fn edit(field: &Path<Publisher, String>, text: &str) -> Invoke<Publisher> {
    Invoke::edit(&field.replace(text)).expect("an edit")
}
use ui::tilo_actions::{ActionKind, ApiError, Availability, ConfirmationPolicy, RiskLevel};

/// A machine of its own: a home with no key the developer's environment names, and secrets
/// kept in this test's memory.
struct Machine {
    _home: tilo_test_env::TestHome,
    _host: ui::process::ProcessScope,
    secrets: ui::secrets::SecretStore,
}

impl Machine {
    /// The publisher key in the machine's secret store, as `tilolang` reads it.
    #[expect(clippy::expect_used, reason = "the in-memory store always answers")]
    fn key(&self) -> Option<app_registry::publisher::key::SigningKey> {
        app_registry::publisher::key::stored(&self.secrets).expect("the secret store answers")
    }
}

#[expect(clippy::expect_used, reason = "a test without its home cannot run")]
fn home() -> Machine {
    let mut home = tilo_test_env::TestHome::new().expect("test home");
    home.env().remove(app_registry::publisher::key::KEY_ENV);
    let services = ui::process::ProcessServices::null();
    let secrets = services
        .secrets
        .store(app_registry::publisher::key::SECRETS_APP);
    Machine {
        _home: home,
        _host: ui::process::enter(&services),
        secrets,
    }
}

#[test]
fn the_registry_must_be_https_or_this_machine() {
    let mut profile = Replica::<Publisher>::new();
    for address in [
        "https://apps.example.com",
        "http://127.0.0.1:8080",
        "http://127.1.2.3:8080",
        "http://localhost:8080",
        "http://localhost.:8080",
        "http://tilo.localhost:8080",
        "http://[::1]:8080",
    ] {
        assert!(app_registry::publisher::RegistryClient::validate_url(address).is_ok());
        profile
            .apply(edit(&Publisher::paths().registry, address))
            .expect("HTTPS or an actual loopback host");
        assert_eq!(profile.state().expect("profile").registry, address);
    }
    let saved = profile.state().expect("profile").registry;
    for address in [
        "http://apps.example.com",
        "http://localhost.example.com",
        "http://127.0.0.1.example.com",
        "http://0.0.0.0/",
        "http://[::]/",
        "http://[::ffff:127.0.0.1]/",
        "http://localhost@apps.example.com",
        "https://placeholder:placeholder@apps.example.com",
        "https://placeholder@apps.example.com",
        "https://:placeholder@apps.example.com",
        "http://placeholder:placeholder@[::1]:8080",
        "https://",
        "not a URL",
        "file:///tmp/registry",
    ] {
        assert!(app_registry::publisher::RegistryClient::validate_url(address).is_err());
        assert!(
            profile
                .apply(edit(&Publisher::paths().registry, address))
                .is_err(),
            "reject an invalid address at the action API: {address}"
        );
        assert_eq!(
            profile.state().expect("profile").registry,
            saved,
            "rejected edits preserve the valid registry"
        );
    }
}

#[test]
fn publisher_model_registry_url_agrees_with_client_check_and_loads_stored_credentials() {
    let table = [
        "https://apps.example.com",
        "https://eu1.apps.example.com:8443/base",
        "http://localhost:8080",
        "http://localhost.:8080",
        "http://tilo.localhost:8080",
        "http://a.b.localhost.:8080",
        "http://127.0.0.1:8080",
        "http://127.1.2.3:8080",
        "http://[::1]:8080",
        "http://apps.example.com",
        "http://localhost.example.com",
        "http://127.0.0.1.example.com",
        "http://0.0.0.0/",
        "http://[::]/",
        "http://[::ffff:127.0.0.1]/",
        "http://localhost@apps.example.com",
        "https://placeholder:placeholder@apps.example.com",
        "https://placeholder@apps.example.com",
        "https://:placeholder@apps.example.com",
        "http://placeholder:placeholder@127.0.0.1:8080",
        "http://placeholder:placeholder@[::1]:8080",
        "file:///tmp/registry",
        "ws://localhost:8080",
        "https://",
        "not a URL",
    ];
    for address in table {
        assert_eq!(
            publisher_model::registry_url(address).is_ok(),
            app_registry::publisher::RegistryClient::validate_url(address).is_ok(),
            "model and client disagreed on {address}"
        );
    }

    // A stored profile carrying credentials still loads, and the client refuses to use it.
    let stored_url = "https://placeholder:placeholder@apps.example.com".to_owned();
    let mut stored = Replica::<Publisher>::new();
    ui::tilo_resource::write_key(
        stored.doc_mut(),
        &ui::tilo_resource::__private::ROOT,
        "registry",
        &stored_url,
    )
    .expect("write stored profile");
    let saved = stored.save();
    let loaded = Replica::<Publisher>::load(&saved).expect("stored profile loads");
    let registry = loaded.state().expect("stored state").registry;
    assert_eq!(registry, stored_url);
    assert!(app_registry::publisher::RegistryClient::new(&registry).is_err());
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
        .wait_for_named("public-key", tilo_test_env::hang_bound())
        .expect("the new key's public half is shown");

    let key = home.key().expect("the key tilolang reads");
    let public = app_registry::publisher::key::public_hex(&key);
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
        matches!(&again, Err(ApiError::Unavailable { code, .. }) if code == "key-exists"),
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
        .find(|resource| resource.kind == "publisher.profile")
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
        assert_eq!(contract.kind, ActionKind::Outside, "{id}");
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
        matches!(&refused, Err(ApiError::Unavailable { code, .. }) if code == "no-key"),
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

/// Copying the secret key waits for the clipboard off the UI thread, so it answers later:
/// through the api it is asked first, as before, then runs as the job it declares.
#[test]
fn copying_the_secret_key_is_a_job_that_ends_as_the_clipboard_says() {
    let _home = home();
    let (host, recorder) = ui::process::ProcessServices::recording();
    let _host = ui::process::enter(&host);
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.effect_confirmed(&KeyEffect::CreateKey)
        .expect("create the key");
    assert!(
        app.effect(&KeyEffect::CopySecret).is_err(),
        "copying the secret key asks first"
    );
    assert!(recorder.secret_copies().is_empty());
    app.effect_confirmed(&KeyEffect::CopySecret)
        .expect("the key is on the clipboard");
    assert_eq!(
        recorder.secret_copies().len(),
        1,
        "copied once, as a secret the clipboard does not keep"
    );
}

#[test]
fn applying_reports_what_the_registry_answered() {
    fn notice_detail(node: &ui::headless::InspectionNode) -> Option<&str> {
        if node.test_id.as_deref() == Some("notice") {
            return node
                .semantics
                .as_ref()
                .and_then(|node| node.description.as_deref());
        }
        node.children.iter().find_map(notice_detail)
    }

    let _home = home();
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.effect_confirmed(&KeyEffect::CreateKey)
        .expect("create the key");
    app.invoke(edit(&Publisher::paths().registry, "http://127.0.0.1:9"))
        .expect("an unreachable registry");
    let nothing = app.effect(&KeyEffect::ApplyForNamespaces);
    assert!(
        matches!(&nothing, Err(ApiError::Failed(message)) if message.contains("name a domain or a forge account")),
        "the job answers its caller: {nothing:?}"
    );
    let inspection = app.harness().inspection();
    let detail =
        notice_detail(&inspection.root).expect("the actual status banner publishes its detail");
    assert!(
        detail.contains("name a domain or a forge account"),
        "the application explains why it failed: {detail}"
    );

    app.invoke(edit(&Publisher::paths().domain, "example.com"))
        .expect("domain");
    assert!(
        app.effect(&KeyEffect::ApplyForNamespaces).is_err(),
        "an unreachable registry fails the application"
    );
    app.harness()
        .wait_until(tilo_test_env::hang_bound(), |driver| {
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
    app.invoke(edit(&Publisher::paths().domain, "example.com"))
        .expect("domain");
    app.invoke(edit(&Publisher::paths().forge_account, "codeberg:ada"))
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

#[test]
fn narrow_key_actions_reserve_their_wrapped_rows_before_namespaces() {
    let mut app =
        TestApp::<App>::capture_at(Some("key"), (390, 844)).expect("the narrow capture opens");
    let targets = app.harness().targets();
    let heading = targets
        .get("namespaces-heading")
        .expect("namespace heading bounds");
    for id in ["copy-proof", "copy-secret", "check-status"] {
        let button = targets
            .get(id)
            .unwrap_or_else(|| panic!("{id} is laid out"));
        assert!(
            button.x0 >= 0.0 && button.x1 <= 390.0,
            "{id} fits: {button:?}"
        );
        assert!(
            button.y1 <= heading.y0,
            "{id} precedes namespace heading: {button:?}, {heading:?}"
        );
    }
    app.harness()
        .expect(&SemanticExpectation::named("proof-line").name_contains("tilo-publisher="))
        .expect("scrolling retains the exact published proof value");
}

#[test]
fn history_names_the_field_that_changed() {
    let _home = home();
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(edit(&Publisher::paths().domain, "example.com"))
        .expect("domain");
    app.invoke(edit(&Publisher::paths().contact, "ada@example.com"))
        .expect("contact");
    assert!(
        app.invoke(edit(&Publisher::paths().registry, "not a URL"))
            .is_err(),
        "the invariant refuses an address the client would not use"
    );
    let catalogue = tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .history()
        .into_iter()
        .filter(|entry| entry.what.is_some())
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        ["Changed the domain to claim", "Changed the contact address"]
    );
}

/// German is one of the languages the app ships, and it reads in German: the copy comes
/// from `locales/de-DE.ftl`, never from the English source beside it, so a device set to
/// German reads a German window.
#[test]
fn a_german_window_reads_the_catalogue() {
    let catalogue = &publisher::locales::CATALOG;
    assert!(
        catalogue.languages().contains(&"de-DE"),
        "the app ships German among {:?}",
        catalogue.languages()
    );
    catalogue.set_locale("de-DE");
    assert_eq!(catalogue.t("publisher-create-key"), "Schlüssel anlegen");
    catalogue.set_locale("en-US");
    assert_eq!(catalogue.t("publisher-create-key"), "Create key");
}
