use super::*;
use std::fmt::Write;
use ui::framework::testing::TestApp;

fn fixture(path: &Path) {
    let mut prefs = String::from(
        "user_pref(\"mail.accountmanager.accounts\", \"account1,account2,account3,account4\");\n",
    );
    for index in 1..=4 {
        let microsoft = index == 3;
        let email = if microsoft {
            "example@outlook.de".into()
        } else {
            format!("person{index}@example.org")
        };
        let username = if index == 2 {
            "mailbox-login".to_owned()
        } else {
            email.clone()
        };
        let values = [
            (
                format!("mail.account.account{index}.server"),
                serde_json::json!(format!("server{index}")),
            ),
            (
                format!("mail.account.account{index}.identities"),
                serde_json::json!(format!("id{index}")),
            ),
            (
                format!("mail.server.server{index}.type"),
                serde_json::json!("imap"),
            ),
            (
                format!("mail.server.server{index}.hostname"),
                serde_json::json!(if microsoft {
                    "outlook.office365.com"
                } else {
                    "imap.example.org"
                }),
            ),
            (
                format!("mail.server.server{index}.port"),
                serde_json::json!(993),
            ),
            (
                format!("mail.server.server{index}.socketType"),
                serde_json::json!(3),
            ),
            (
                format!("mail.server.server{index}.userName"),
                serde_json::json!(username),
            ),
            (
                format!("mail.identity.id{index}.useremail"),
                serde_json::json!(email),
            ),
            (
                format!("mail.identity.id{index}.fullName"),
                serde_json::json!("Example"),
            ),
            (
                format!("mail.identity.id{index}.smtpServer"),
                serde_json::json!(format!("smtp{index}")),
            ),
            (
                format!("mail.smtpserver.smtp{index}.hostname"),
                serde_json::json!(if microsoft {
                    "smtp-mail.outlook.com"
                } else {
                    "smtp.example.org"
                }),
            ),
            (
                format!("mail.smtpserver.smtp{index}.port"),
                serde_json::json!(if microsoft { 587 } else { 465 }),
            ),
            (
                format!("mail.smtpserver.smtp{index}.try_ssl"),
                serde_json::json!(if microsoft { 2 } else { 3 }),
            ),
            (
                format!("mail.smtpserver.smtp{index}.username"),
                serde_json::json!(username),
            ),
        ];
        for (key, value) in values {
            writeln!(
                prefs,
                "user_pref({}, {});",
                serde_json::to_string(&key).unwrap(),
                value
            )
            .unwrap();
        }
        if microsoft {
            writeln!(prefs, "user_pref(\"mail.server.server{index}.authMethod\", 10);\nuser_pref(\"mail.smtpserver.smtp{index}.authMethod\", 10);").unwrap();
        }
    }
    std::fs::write(path, prefs).unwrap();
}

fn inspect(app: &mut TestApp<crate::SettingsShell>) -> serde_json::Value {
    let output = app
        .effect(&SettingsEffect::from(
            AccountsEffect::InspectThunderbirdImport,
        ))
        .unwrap();
    serde_json::from_str(output.value.unwrap().as_str().unwrap()).unwrap()
}

fn discover(app: &mut TestApp<crate::SettingsShell>, prefs: &Path) {
    app.effect(&SettingsEffect::from(
        AccountsEffect::DiscoverThunderbirdAccounts {
            prefs: prefs.to_str().unwrap().into(),
        },
    ))
    .unwrap();
}

fn activate(app: &mut TestApp<crate::SettingsShell>, target: &str) {
    app.harness()
        .perform_named(target, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    assert!(
        app.harness().control_failures().is_empty(),
        "{:?}",
        app.harness().control_failures()
    );
}

fn review_and_cancel(
    app: &mut TestApp<crate::SettingsShell>,
    prefs: &Path,
    root: &crate::accounts::AccountRoot,
) {
    discover(app, prefs);
    let initial = inspect(app);
    assert_eq!(initial["accounts"].as_array().unwrap().len(), 4);
    assert_eq!(initial["credentials_imported"], false);
    assert_eq!(initial["access_granted"], false);
    assert!(initial["accounts"][1]["candidate"]["incoming"]["authentication"].is_null());
    assert_eq!(
        initial["accounts"][1]["candidate"]["incoming"]["username"],
        "mailbox-login"
    );
    activate(app, "settings.accounts.import.account2:id2.password");
    assert_eq!(inspect(app)["accounts"][1]["selected"], true);
    assert!(
        app.effect(&SettingsEffect::from(
            AccountsEffect::CommitThunderbirdImport {
                revision: initial["revision"].as_u64().unwrap()
            }
        ))
        .is_err()
    );
    activate(app, "settings.accounts.import.account2:id2.password");
    assert_eq!(inspect(app)["accounts"][1]["selected"], false);
    crate::effects::show_pane(app, crate::categories::Category::Appearance);
    crate::effects::show_pane(app, crate::categories::Category::ExternalAccounts);
    assert_eq!(inspect(app)["accounts"].as_array().unwrap().len(), 4);
    activate(app, "settings.accounts.import.cancel");
    assert!(inspect(app)["accounts"].as_array().unwrap().is_empty());
    assert_eq!(root.store().len(), 1);
}

#[test]
fn mounted_review_selection_navigation_cancel_and_pending_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let prefs = temp.path().join("prefs.js");
    fixture(&prefs);
    let profile = temp.path().join("profile");
    let root = crate::accounts::AccountRoot::at(&profile);
    let mut original = accounts::Account::new("existing", "caldav");
    original.display_name = "Existing owner label".into();
    original.config = serde_json::json!({"owner_extension": "preserve-me"});
    let mut store = root.store();
    store.upsert(original.clone());
    root.save_store(&store).unwrap();
    let mut app = TestApp::<crate::SettingsShell>::open_data_dir(&profile).unwrap();
    // A private headless host intentionally does not take in the device's
    // file. Seed its actual personal record, then project that canonical state.
    let person = app.person::<accounts::person::PersonAccounts>().unwrap();
    person
        .invoke(&accounts::person::PersonAccounts::saving(&original))
        .unwrap();
    app.flush().unwrap();
    person.get().project_in(&profile).unwrap();
    drop(person);
    crate::effects::show_pane(&mut app, crate::categories::Category::ExternalAccounts);
    review_and_cancel(&mut app, &prefs, &root);
    discover(&mut app, &prefs);
    activate(&mut app, "settings.accounts.import.account3:id3.oauth");
    activate(&mut app, "settings.accounts.import.commit");
    app.flush().unwrap();
    app.settle();
    let stored = root.store();
    assert_eq!(
        app.person::<accounts::person::PersonAccounts>()
            .unwrap()
            .get()
            .store()
            .len(),
        2,
        "personal account record includes the import"
    );
    assert_eq!(stored.len(), 2);
    assert_eq!(stored.get(&original.id), Some(&original));
    assert!(stored.list(&"email".into()).is_empty());
    let pending = stored.get(&"example@outlook.de".into()).unwrap();
    let config = pending
        .config_as::<accounts::email::AccountConfig>()
        .unwrap();
    assert!(config.import_pending);
    assert_eq!(config.auth, accounts::email::AuthMethod::OAuth);
    assert_eq!(config.imap_host, "outlook.office365.com");
    assert_eq!(config.smtp_host, "smtp-mail.outlook.com");
    assert_eq!(config.smtp_port, 587);
    assert!(root.vault().get(&pending.id).is_none());
    let mut reopened = app.reopen().unwrap();
    crate::effects::show_pane(&mut reopened, crate::categories::Category::ExternalAccounts);
    assert_eq!(
        reopened
            .person::<accounts::person::PersonAccounts>()
            .unwrap()
            .get()
            .store()
            .len(),
        2,
        "reopened personal record retains the import"
    );
    assert_eq!(root.store().get(&pending.id), Some(pending));
    assert!(
        inspect(&mut reopened)["accounts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(root.store().get(&original.id), Some(&original));
    discover(&mut reopened, &prefs);
    activate(&mut reopened, "settings.accounts.import.account3:id3.oauth");
    activate(&mut reopened, "settings.accounts.import.commit");
    reopened.flush().unwrap();
    reopened.settle();
    assert_eq!(root.store().len(), stored.len());
    assert_eq!(root.store().get(&pending.id), Some(pending));
    assert_eq!(root.store().get(&original.id), Some(&original));
}
