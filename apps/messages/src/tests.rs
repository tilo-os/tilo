#![allow(
    clippy::expect_used,
    reason = "headless test fixtures must construct successfully before assertions"
)]

use crate::{MessagesEffect, MessagesShell};
use tilo_contact_directory::ContactDirectory;
use ui::framework::testing::TestApp;
use ui::tilo_resource::Id;

const PERSON: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn live_starts_empty_and_transport_action_is_explicitly_unavailable() {
    let mut app =
        TestApp::<MessagesShell>::for_person("messages-fixture-owner").expect("headless app");
    let directory = app
        .shared_person::<ContactDirectory>()
        .expect("shared directory");
    assert!(directory.get().people.is_empty());
    let send = MessagesEffect::Send {
        choice: "unconfigured".into(),
        operation: "fixture-operation".into(),
        text: "fixture text".into(),
    };
    let heads = app.read_document(|doc| doc.clone().get_heads());
    assert!(app.effect(&send).is_err());
    assert_eq!(
        app.read_document(|doc| doc.clone().get_heads()),
        heads,
        "a refused send writes nothing"
    );
    assert!(directory.get().people.is_empty());
}

#[test]
fn unversioned_recipient_confirmation_cannot_create_a_transport_association() {
    let mut app =
        TestApp::<MessagesShell>::for_person("messages-fixture-owner").expect("headless app");
    let directory = app
        .shared_person::<ContactDirectory>()
        .expect("shared directory");
    directory
        .invoke(&ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(PERSON), "Alex".into())))
        .expect("fixture contact");
    directory
        .invoke(&ContactDirectory::PROJECT_CONTACT.with((
            Id::from_raw("00000000-0000-4000-8000-000000000002"),
            "Alex".into(),
        )))
        .expect("same name contact");
    app.settle();
    app.effect(&MessagesEffect::OpenPerson {
        person: PERSON.into(),
    })
    .expect("open exact person");
    assert!(
        app.effect(&MessagesEffect::ConfirmRecipient {
            person: PERSON.into(),
            service: "signal".into(),
            recipient: "+12025550123".into(),
        })
        .is_err()
    );
    assert!(
        directory
            .get()
            .people
            .get(&Id::from_raw(PERSON))
            .expect("contact")
            .endpoints
            .is_empty()
    );
    assert!(
        app.effect(&MessagesEffect::ConfirmRecipient {
            person: "00000000-0000-4000-8000-000000000002".into(),
            service: "signal".into(),
            recipient: "+12025550123".into()
        })
        .is_err()
    );
}

#[test]
fn account_or_recipient_cannot_be_substituted_into_a_dropdown_selection() {
    let mut app =
        TestApp::<MessagesShell>::for_person("messages-fixture-owner").expect("headless app");
    let directory = app
        .shared_person::<ContactDirectory>()
        .expect("shared directory");
    directory
        .invoke(&ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(PERSON), "Alex".into())))
        .expect("fixture contact");
    directory
        .invoke(&ContactDirectory::CONFIRM_ENDPOINT.with((
            Id::from_raw(PERSON),
            Id::from_raw("signal"),
            "signal".into(),
            "+12025550123".into(),
        )))
        .expect("fixture endpoint");
    let accounts = app
        .shared_person::<tilo_messaging::PersonAccounts>()
        .expect("account record");
    assert!(
        !accounts.editable(),
        "Messages has no account-editor authority"
    );
    app.settle();
    app.effect(&MessagesEffect::OpenPerson {
        person: PERSON.into(),
    })
    .expect("open person");
    let valid = serde_json::json!({"service":"signal", "account":"", "recipient":"", "association":"", "scope":"", "revision":""})
        .to_string();
    app.effect(&MessagesEffect::SelectRoute { choice: valid })
        .expect("select explicit unavailable service");
    let forged = serde_json::json!({"service":"signal", "account":"different-account", "recipient":"+12025550123"}).to_string();
    assert!(
        app.effect(&MessagesEffect::SelectRoute { choice: forged })
            .is_err()
    );
    let forged = serde_json::json!({"service":"signal", "account":"fixture-signal", "recipient":"+12025550199"}).to_string();
    assert!(
        app.effect(&MessagesEffect::SelectRoute { choice: forged })
            .is_err()
    );
}

#[test]
fn narrow_and_wide_surfaces_publish_accessible_routes_and_shared_navigation() {
    for (width, theme) in [
        (420, ui::theme::Theme::dark()),
        (1180, ui::theme::Theme::dark()),
        (420, ui::theme::Theme::light()),
        (1180, ui::theme::Theme::high_contrast()),
    ] {
        let mut app =
            TestApp::<MessagesShell>::for_person("messages-fixture-owner").expect("headless app");
        app.shared_person::<ContactDirectory>()
            .expect("shared directory")
            .invoke(&ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(PERSON), "Alex".into())))
            .expect("fixture contact");
        app.settle();
        app.effect(&MessagesEffect::OpenPerson {
            person: PERSON.into(),
        })
        .expect("open exact person");
        app.harness().set_theme(theme);
        app.harness()
            .resize((width, 760))
            .expect("resize production surface");
        let nodes = app.harness().a11y_nodes();
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Message using"))
        );
        // NavShell owns Back only in the narrow push layout; a wide window
        // keeps the overview visible and needs no application-drawn Back.
        assert_eq!(
            nodes.iter().any(|(_, node)| node.label() == Some("Back")),
            width == 420,
        );
        let labels = if width == 420 {
            vec!["Message using", "Back"]
        } else {
            vec!["Message using"]
        };
        for label in labels {
            let node = nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .expect("visible navigation control");
            let bounds = node.1.bounds().expect("control geometry");
            assert!(
                bounds.x0 >= 0.0 && bounds.x1 <= f64::from(width),
                "{label} must be visible immediately after restoring selection: {bounds:?}"
            );
        }
        assert!(app.effect(&MessagesEffect::Back).is_ok());
    }
}

#[test]
fn scoped_live_selection_rejects_substituted_sender_scope_recipient_and_revision() {
    let mut app = live_messages_with_sender();
    let valid = serde_json::json!({"service":"signal", "account":"fixture-signal", "recipient":"+12025550123", "association":"signal", "scope":"fixture-signal-scope", "revision":"fixture-signal-revision"});
    app.effect(&MessagesEffect::SelectRoute {
        choice: valid.to_string(),
    })
    .expect("exact versioned choice");
    app.effect(&MessagesEffect::SelectRoute {
        choice: serde_json::to_string_pretty(&valid).expect("formatted invocation"),
    })
    .expect("identity comparison ignores JSON whitespace");
    app.settle();
    let nodes = app.harness().a11y_nodes();
    let route_control = nodes
        .iter()
        .find(|(_, node)| {
            node.label()
                .is_some_and(|label| label.starts_with("Message using"))
        })
        .expect("shared header route control");
    let displayed = route_control
        .1
        .label()
        .expect("selected route is named for menu accessibility");
    assert!(displayed.contains("fixture-signal"));
    assert!(displayed.contains("+12025550123"));
    for (field, forged) in [
        ("account", "fixture-whatsapp"),
        ("scope", "other-tenant"),
        ("recipient", "+12025550199"),
        ("revision", "changed-key"),
        ("association", "other-person-link"),
    ] {
        let mut altered = valid.clone();
        altered[field] = forged.into();
        assert!(
            app.effect(&MessagesEffect::SelectRoute {
                choice: altered.to_string()
            })
            .is_err(),
            "substituted {field}"
        );
    }
    let effect = MessagesEffect::ConfirmAccount {
        person: PERSON.into(),
        choice: valid.to_string(),
    };
    assert!(matches!(
        app.effect(&effect),
        Err(ui::tilo_actions::DoorError::ConfirmationRequired)
    ));
    app.effect_confirmed(&effect)
        .expect("reviewed account forwards the exact shared action");
}

#[test]
fn public_route_action_refuses_revoked_identity_until_a_new_association_is_reviewed() {
    let mut app = live_messages_with_sender();
    let old = serde_json::json!({"service":"signal", "account":"fixture-signal", "recipient":"+12025550123", "association":"signal", "scope":"fixture-signal-scope", "revision":"fixture-signal-revision"});
    app.effect(&MessagesEffect::SelectRoute {
        choice: old.to_string(),
    })
    .expect("original explicit route");
    let directory = app
        .shared_person::<ContactDirectory>()
        .expect("shared directory");
    directory
        .invoke(
            &ContactDirectory::FORGET_ENDPOINT.with((Id::from_raw(PERSON), Id::from_raw("signal"))),
        )
        .expect("reviewed revocation fixture");
    app.settle();
    assert!(
        app.effect(&MessagesEffect::SelectRoute {
            choice: old.to_string()
        })
        .is_err()
    );
    assert!(
        app.effect_confirmed(&MessagesEffect::ConfirmAccount {
            person: PERSON.into(),
            choice: old.to_string()
        })
        .is_err(),
        "revoked association id cannot revive"
    );
    let mut revised = old.clone();
    revised["association"] = "signal-new-association".into();
    revised["revision"] = "signal-new-identity".into();
    assert!(
        app.effect(&MessagesEffect::SelectRoute {
            choice: revised.to_string()
        })
        .is_err(),
        "new identity cannot be silently selected"
    );
    let review = MessagesEffect::ConfirmAccount {
        person: PERSON.into(),
        choice: revised.to_string(),
    };
    assert!(matches!(
        app.effect(&review),
        Err(ui::tilo_actions::DoorError::ConfirmationRequired)
    ));
    app.effect_confirmed(&review)
        .expect("review exact new association and revision");
    app.effect(&MessagesEffect::SelectRoute {
        choice: revised.to_string(),
    })
    .expect("new exact route");
    assert!(
        app.effect(&MessagesEffect::SelectRoute {
            choice: old.to_string()
        })
        .is_err()
    );
    assert_eq!(
        directory.get().resolve_account(
            "signal",
            "fixture-signal-scope",
            "+12025550123",
            "signal-new-identity"
        ),
        Ok(Id::from_raw(PERSON))
    );
    assert!(
        directory
            .get()
            .resolve_account(
                "signal",
                "fixture-signal-scope",
                "+12025550123",
                "fixture-signal-revision"
            )
            .is_err()
    );
}

struct AccountOwnerFixture;
impl ui::framework::ResourceApp for AccountOwnerFixture {
    type Resource = tilo_messaging::PersonAccounts;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Metadata owner fixture";
    fn view(cx: &mut ui::framework::ViewCx<Self::Resource>) -> ui::widget::Element {
        let _ = cx.shared_person::<tilo_messaging::PersonAccounts>();
        Box::new(ui::widget::Column::new())
    }
}

fn live_messages_with_sender() -> TestApp<MessagesShell> {
    live_messages_with_history(None)
}

pub(crate) fn live_messages_with_history(
    history: Option<tilo_messaging::HistoryClient>,
) -> TestApp<MessagesShell> {
    live_messages_with_service(history)
}

pub(crate) fn live_messages_with_service<T: std::any::Any>(
    history: Option<T>,
) -> TestApp<MessagesShell> {
    live_messages_with_service_and_scope(history, "fixture-signal-scope")
}

pub(crate) fn live_messages_with_service_and_scope<T: std::any::Any>(
    history: Option<T>,
    scope: &str,
) -> TestApp<MessagesShell> {
    let person = "11".repeat(32);
    let mut settings = TestApp::<AccountOwnerFixture>::for_person(&person).expect("metadata owner");
    let account_record = settings
        .shared_person::<tilo_messaging::PersonAccounts>()
        .expect("owner account record");
    assert!(account_record.editable());
    account_record
        .invoke(&tilo_messaging::PersonAccounts::SAVE_ACCOUNT.with((
            "fixture-signal".into(),
            "signal".into(),
            "Personal".into(),
            serde_json::json!({"messaging_scope":scope}),
            true,
            0,
        )))
        .expect("configured metadata fixture");
    let mut app = if let Some(history) = history {
        TestApp::<MessagesShell>::for_person_with_lent(&person, history)
    } else {
        TestApp::<MessagesShell>::for_person(&person)
    }
    .expect("live Messages host");
    let accounts = app
        .shared_person::<tilo_messaging::PersonAccounts>()
        .expect("read-only accounts");
    assert_eq!(
        accounts.record().route(),
        account_record.record().route(),
        "same owner's shared record route"
    );
    accounts
        .record()
        .merge_peer_document(
            account_record
                .record()
                .document()
                .expect("owner record document"),
        )
        .expect("existing shared-record sync path");
    drop(account_record);
    drop(settings);
    assert!(!accounts.editable());
    assert!(
        accounts
            .get()
            .accounts
            .get(&Id::from_raw("fixture-signal"))
            .is_some(),
        "shared metadata was persisted by its actual owner"
    );
    let directory = app
        .shared_person::<ContactDirectory>()
        .expect("shared directory");
    directory
        .invoke(&ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(PERSON), "Alex".into())))
        .expect("person");
    directory
        .invoke(&ContactDirectory::CONFIRM_ACCOUNT.with((
            Id::from_raw(PERSON),
            Id::from_raw("signal"),
            "signal".into(),
            "fixture-signal-scope".into(),
            "+12025550123".into(),
            "fixture-signal-revision".into(),
        )))
        .expect("reviewed fixture identity");
    app.settle();
    app.effect(&MessagesEffect::OpenPerson {
        person: PERSON.into(),
    })
    .expect("open exact live person");
    app
}
