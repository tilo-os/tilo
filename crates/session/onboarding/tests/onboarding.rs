//! Setup as a person, a screen reader and an agent meet it: every step
//! renders without a screen, what the person does goes to the shell as an
//! action, and secrets stay in the shell's buffers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_onboarding::{
    OnboardingApp, OnboardingEffect, OnboardingSeat, Page, Screen, Secrets, TourPage,
};
use ui::framework::ResourceApp as _;
use ui::framework::testing::HeadlessApp;
use ui::tilo_resource::Effect as _;
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<OnboardingApp> {
    HeadlessApp::<OnboardingApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn seated(page: Page) -> (HeadlessApp<OnboardingApp>, OnboardingSeat) {
    let seat = OnboardingSeat::new(Secrets::default(), None);
    seat.show(page);
    let mut app =
        HeadlessApp::<OnboardingApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn page(screen: Screen) -> Page {
    Page {
        screen,
        total_steps: 4,
        recovery_words_total: 24,
        backup_export_available: true,
        ..Page::default()
    }
}

/// Every control with a name, by that name, where it stands.
fn named(app: &mut HeadlessApp<OnboardingApp>) -> Vec<(String, Rect)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            Some((
                node.label()?.to_owned(),
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            ))
        })
        .collect()
}

fn press(app: &mut HeadlessApp<OnboardingApp>, label: &str) {
    let at = named(app)
        .into_iter()
        .find(|(name, _)| name == label)
        .unwrap_or_else(|| panic!("no control named {label}"))
        .1;
    app.harness().click_at(at.center().x, at.center().y);
    app.settle();
}

fn says(app: &mut HeadlessApp<OnboardingApp>, text: &str) -> bool {
    app.harness().a11y_nodes().into_iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
    })
}

#[test]
fn every_step_renders_without_a_screen() {
    for scenario in OnboardingApp::scenarios() {
        let mut app = staged(scenario);
        let pixels = app.harness().render_rgba_transparent();
        assert!(
            pixels.chunks(4).any(|pixel| pixel[3] > 0),
            "{scenario} paints something"
        );
        assert!(
            !named(&mut app).is_empty(),
            "{scenario} publishes what it shows"
        );
    }
}

#[test]
fn there_is_a_scenario_for_every_step() {
    let scenarios = OnboardingApp::scenarios();
    for step in [
        "intro",
        "existing",
        "recovery",
        "nearby",
        "restore",
        "recovery-key",
        "protect",
        "backup",
        "backup-recovery-key",
        "portable-backup",
        "ready",
        "codecs",
        "tour-navigate",
        "tour-organise",
        "tour-collaborate",
        "tour-start",
        "prepared",
        "migration-portable-home",
        "migration-applications",
        "migration-browser",
    ] {
        assert!(
            scenarios.iter().any(|name| name.starts_with(step)),
            "no scenario shows {step}"
        );
    }
}

#[test]
fn the_intro_offers_both_ways_and_asks_the_shell_for_the_chosen_one() {
    let (mut app, seat) = seated(page(Screen::Intro));
    assert!(says(&mut app, &tilo_onboarding::copy::intro_title()));
    press(&mut app, &tilo_onboarding::t("onboarding-choice-create"));
    assert_eq!(seat.take_asked(), [OnboardingEffect::choose("create")]);
    press(&mut app, &tilo_onboarding::t("onboarding-choice-existing"));
    assert_eq!(seat.take_asked(), [OnboardingEffect::choose("existing")]);
}

#[test]
fn ticking_the_acknowledgement_asks_the_shell_to_record_it() {
    let (mut app, seat) = seated(page(Screen::NoRecovery));
    press(&mut app, &tilo_onboarding::copy::acknowledgement());
    assert_eq!(
        seat.take_asked(),
        [OnboardingEffect::Acknowledge { checked: true }]
    );
}

#[test]
fn create_waits_for_the_shell_to_allow_it() {
    let (mut app, seat) = seated(page(Screen::NoRecovery));
    let create = tilo_onboarding::t("onboarding-create-local-identity");
    let disabled = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(create.as_str()))
        .is_some_and(|(_, node)| node.is_disabled());
    assert!(
        disabled,
        "nothing is created before the fact is acknowledged"
    );
    seat.show(Page {
        acknowledged: true,
        can_create: true,
        ..page(Screen::NoRecovery)
    });
    app.settle();
    press(&mut app, &create);
    assert_eq!(seat.take_asked(), [OnboardingEffect::Continue]);
}

#[test]
fn escape_goes_back_while_setup_may_and_skips_the_tour() {
    let (mut app, seat) = seated(page(Screen::Nearby));
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [OnboardingEffect::Back]);

    seat.show(Page {
        identity_ready_page: true,
        tour_page: Some(TourPage::Organise),
        ..page(Screen::Done)
    });
    app.settle();
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [OnboardingEffect::Skip]);

    seat.show(page(Screen::Intro));
    app.settle();
    app.harness().key("Escape");
    app.settle();
    assert!(
        seat.take_asked().is_empty(),
        "the intro cannot be dismissed"
    );
}

#[test]
fn a_typed_passphrase_stays_in_the_shells_buffer() {
    let (mut app, seat) = seated(Page {
        connect_path: "/home/person/backup.tilo-identity".to_owned(),
        ..page(Screen::Restore)
    });
    // The field, not its label above it: both carry the name.
    let field = tilo_onboarding::t("onboarding-connect-passphrase");
    let at = named(&mut app)
        .into_iter()
        .rfind(|(name, _)| *name == field)
        .expect("the passphrase field")
        .1;
    app.harness().click_at(at.center().x, at.center().y);
    app.settle();
    app.harness().type_text("correct horse battery");
    app.settle();
    assert_eq!(
        *seat.secrets().connect_passphrase.lock().unwrap(),
        "correct horse battery"
    );
    let asked = seat.take_asked();
    assert!(
        asked
            .iter()
            .all(|effect| !format!("{effect:?}").contains("horse")),
        "the secret is not among the actions: {asked:?}"
    );
}

#[test]
fn the_backup_file_is_entered_as_a_value() {
    let (mut app, seat) = seated(page(Screen::Restore));
    press(&mut app, &tilo_onboarding::t("onboarding-connect-file"));
    app.harness().type_text("/b");
    app.settle();
    assert!(seat.take_asked().contains(&OnboardingEffect::Enter {
        field: "backup-file".to_owned(),
        value: "/b".to_owned(),
    }));
}

#[test]
fn a_secret_entered_as_an_action_reaches_only_its_buffer() {
    let (mut app, seat) = seated(page(Screen::Restore));
    let output = app
        .effect(&OnboardingEffect::EnterSecret {
            field: "backup-passphrase".to_owned(),
            value: "s3cret passphrase".to_owned(),
        })
        .unwrap();
    app.settle();
    assert!(!format!("{output:?}").contains("s3cret"), "{output:?}");
    assert_eq!(
        *seat.secrets().connect_passphrase.lock().unwrap(),
        "s3cret passphrase"
    );
    assert!(seat.take_asked().is_empty());
}

#[test]
fn an_agent_cannot_hand_setup_a_secret_or_choose_where_identity_goes() {
    let (mut app, seat) = seated(page(Screen::Restore));
    let refused = app.effect_by(
        "agent",
        &OnboardingEffect::EnterSecret {
            field: "backup-passphrase".to_owned(),
            value: "from an agent".to_owned(),
        },
    );
    app.settle();
    assert!(refused.is_err(), "{refused:?}");
    assert!(seat.secrets().connect_passphrase.lock().unwrap().is_empty());
    for declaration in OnboardingEffect::declarations() {
        assert_eq!(
            declaration.attended,
            ["enter-secret", "pick"].contains(&declaration.id),
            "{}",
            declaration.id
        );
    }
}

#[test]
fn the_ordinary_steps_are_driven_without_a_screen() {
    let (mut app, seat) = seated(page(Screen::Existing));
    app.effect(&OnboardingEffect::choose("nearby")).unwrap();
    app.effect(&OnboardingEffect::Back).unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [OnboardingEffect::choose("nearby"), OnboardingEffect::Back]
    );
}

#[test]
fn secrets_are_never_printed() {
    let effect = OnboardingEffect::EnterSecret {
        field: "new-passphrase".to_owned(),
        value: "hunter2hunter2".to_owned(),
    };
    assert!(!format!("{effect:?}").contains("hunter2"));
    let seat = OnboardingSeat::new(Secrets::default(), None);
    seat.secrets().enter("new-passphrase", "hunter2hunter2");
    seat.show(Page {
        recovery_key_copy: Some(tilo_onboarding::KeyCopy {
            words: tilo_onboarding::SecretText::new("alpha beta gamma".to_owned().into()),
            checks: [1, 2, 3],
        }),
        ..page(Screen::Done)
    });
    let printed = format!("{seat:?} {:?}", seat.secrets());
    assert!(
        !printed.contains("hunter2") && !printed.contains("gamma"),
        "{printed}"
    );
}

#[test]
fn the_page_changes_in_place_when_the_shell_moves_on() {
    let (mut app, seat) = seated(page(Screen::Intro));
    seat.show(page(Screen::Existing));
    app.settle();
    assert!(says(
        &mut app,
        &tilo_onboarding::t("onboarding-existing-title")
    ));
}

#[test]
fn the_nearby_code_is_read_in_two_groups() {
    let mut app = staged("nearby-code");
    assert!(says(&mut app, "482 913"));
}

#[test]
fn the_tour_paints_its_card_over_a_dimmed_desktop() {
    let mut app = staged("tour-navigate");
    let pixels = app.harness().render_rgba_transparent();
    let alpha = |x: u32, y: u32| pixels[usize::try_from((y * VIEWPORT.0 + x) * 4 + 3).unwrap()];
    assert!(
        alpha(4, 4) < 255,
        "the desktop shows through around the card"
    );
    let mut app = staged("intro");
    let pixels = app.harness().render_rgba_transparent();
    let alpha = |x: u32, y: u32| pixels[usize::try_from((y * VIEWPORT.0 + x) * 4 + 3).unwrap()];
    assert_eq!(
        alpha(4, 4),
        255,
        "the identity steps paint their own ground"
    );
}
