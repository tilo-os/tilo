//! The plan an "on another computer" preparation makes: its choices made or
//! left open, the values the review shows, and the computers already planned.

use super::*;
use crate::hosting::testing::TestHost;
use install_profile_model::plan::Token;

fn token() -> Token {
    Token {
        nonce: "00112233445566778899aabbccddeeff".to_owned(),
        expires_at_ns: 1_700_000_000_000_000_000,
    }
}

fn id() -> String {
    "a".repeat(32)
}

fn server() -> machine::Choices {
    machine::Choices {
        os: Some(machine::Os::TiloServer),
        ..machine::Choices::default()
    }
}

#[test]
fn the_other_computer_chooses_its_os_and_defaults_to_tilo_os() {
    let mut journey = Journey::new();
    journey.destination = Some(Destination::Another);
    assert_eq!(journey.plan.os(), machine::Os::TiloOs);
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Target);

    journey.page = Page::Destination;
    journey.choose("server", true).unwrap();
    assert_eq!(journey.plan.os, Some(machine::Os::TiloServer));
}

#[test]
fn the_server_is_a_destination_and_an_os_plan_never_asks_for_workspaces() {
    let mut journey = Journey::new();
    journey.destination = Some(Destination::Another);
    journey.choose("os", true).unwrap();
    assert!(journey.plan.open().is_empty(), "{:?}", journey.plan.open());
    // Hosting is a server's own choice; an OS plan answers it by installing.
    let plan = journey
        .plan
        .install("spare-laptop")
        .plan(&id(), token())
        .unwrap();
    assert!(
        !plan.open_choices().contains(&"hosting"),
        "{:?}",
        plan.open_choices()
    );
    assert_eq!(plan.destination, "tilo-os");
}

#[test]
fn a_plan_makes_the_choices_set_and_leaves_the_rest_open() {
    let choices = machine::Choices {
        os: Some(machine::Os::TiloOs),
        disk: None,
        updates: None,
        name_open: true,
        ..machine::Choices::default()
    };
    let plan = choices
        .install("spare-laptop")
        .plan(&id(), token())
        .unwrap();
    assert_eq!(plan.destination, "tilo-os");
    assert_eq!(plan.disk.kind, "");
    assert_eq!(plan.updates, "");
    assert_eq!(plan.unlock, "tpm");
    assert_eq!(
        plan.open_choices(),
        vec!["name", "disk", "updates"],
        "the choices the window left open are the document's"
    );
    assert!(!plan.complete());
}

#[test]
fn a_server_answers_workspaces_and_no_purpose_question_is_asked() {
    let plan = server().install("cupboard").plan(&id(), token()).unwrap();
    assert_eq!(plan.destination, "tilo-server");
    assert!(plan.hosting.is_empty());
    let held = machine::Choices {
        hosting: Some(machine::Hosting::Open),
        ..server()
    };
    let plan = held.install("cupboard").plan(&id(), token()).unwrap();
    assert!(plan.open_choices().contains(&"hosting"));
}

#[test]
fn the_delivery_choice_is_the_persons_and_does_not_continue() {
    let mut journey = Journey::new();
    journey.destination = Some(Destination::Another);
    journey.plan.os = Some(machine::Os::TiloServer);
    journey.page = Page::Method;
    journey.choose("netboot", true).unwrap();
    assert_eq!(journey.plan.delivery, machine::Delivery::Netboot);
    assert_eq!(journey.page, Page::Method, "a choice here does not advance");
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Machine);
    journey.page = Page::Machine;
    journey.choose("network-open", true).unwrap();
    assert!(journey.plan.network.is_none());
    journey.next().unwrap();
    assert_eq!(journey.page, Page::Review);
}

#[test]
fn the_plan_screens_paint_at_desktop_and_narrow_widths() {
    for (scenario, ids) in [
        ("installer-plan-os", &["installer.plan.os"][..]),
        ("installer-disk", &["installer.plan.disk"][..]),
        ("installer-method", &["installer.plan.delivery"][..]),
        (
            "installer-machine",
            &[
                "installer.plan.network",
                "installer.plan.unlock",
                "installer.plan.updates",
                "installer.plan.hosting",
                "installer.device-name",
            ][..],
        ),
        (
            "installer-plan-review",
            &[
                "installer.review.plan-disk",
                "installer.review.plan-delivery",
            ][..],
        ),
        ("installer-plan-ready", &[][..]),
        ("installer-plan-waiting", &[][..]),
    ] {
        for (viewport, tag) in [((1180, 2200), "desktop"), ((420, 2600), "narrow")] {
            let mut app = TestHost::capture_at(Some(scenario), viewport);
            app.harness()
                .set_theme(Theme::dark().with_reduce_motion(true));
            harness::paint_app(&mut app, scenario, ids, viewport, tag);
            app.harness().close_components();
        }
    }
}

tilo_test_env::scenario! {
    fn the_review_names_what_the_plan_decides_and_what_the_computer_answers;

    phase the_plan_row_shows_the_value_it_takes;
    {
        let mut app =
            TestHost::capture_at(Some("installer-plan-review"), (1180, 820));
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let plan = &preview.journey.plan;
            assert_eq!(plan.os, Some(machine::Os::TiloServer));
            assert_eq!(plan.disk, None);
            assert_eq!(plan.hosting, Some(machine::Hosting::HoldsNone));
            assert_eq!(plan.open(), vec!["name", "disk"]);
            for id in [
                "installer.review.plan-os",
                "installer.review.plan-disk",
                "installer.review.plan-delivery",
            ] {
                assert!(
                    harness::control(preview, id).is_some(),
                    "the review shows {id}"
                );
            }
            assert!(
                harness::control(preview, "installer.review.plan-hosting").is_some(),
                "a server's review shows what it holds"
            );
            let name = harness::control(preview, "installer.review.plan-name")
                .expect("the review shows the computer's name row")
                .accessibility()
                .expect("the name row is readable");
            let spoken = format!(
                "{}, {}",
                t("installer-preview-device-name-label"),
                t("installer-preview-name-open-title")
            );
            assert_eq!(
                name.name.as_deref(),
                Some(spoken.as_str()),
                "the name row says what it is and what it takes: {name:?}"
            );
            let model = harness::control(preview, "installer.review.target")
                .expect("the review shows the model row")
                .accessibility()
                .expect("the model row is readable");
            let spoken = format!(
                "{}, {}",
                t("installer-preview-target-model-label"),
                t("installer-preview-target-review-nameless")
            );
            assert_eq!(
                model.name.as_deref(),
                Some(spoken.as_str()),
                "the model row says what it is and that its hardware is checked when it installs: {model:?}"
            );
        })
        .unwrap();
    }

    phase a_made_plan_names_the_code_and_how_it_travels;
    {
        let mut app =
            TestHost::capture_at(Some("installer-plan-ready"), (1180, 820));
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let created = preview.machine.created.as_ref().unwrap();
            assert_eq!(created.plan.plan, "0123456789abcdef0123456789abcdef");
            assert!(created.token_file.display().to_string().contains("install-plans"));
            assert!(created.commands.medium.contains("tilo-server-installer"));
            for id in ["installer.plan.token", "installer.plan.command"] {
                assert!(
                    harness::control(preview, id).is_some(),
                    "the ready page shows {id}"
                );
            }
        })
        .unwrap();
    }

    phase waiting_computers_show_what_they_still_ask;
    {
        let mut app =
            TestHost::capture_at(Some("installer-plan-waiting"), (1180, 820));
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.machine.waiting.len(), 2);
            assert!(
                harness::control(preview, "installer.waiting.0.disk-answer").is_some(),
                "a waiting computer's disk choice is answered from its reported drives"
            );
            assert!(
                harness::control(preview, "installer.waiting.1.network").is_some(),
                "a question the plan left open is shown"
            );
        })
        .unwrap();
    }
}
