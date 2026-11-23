//! Setup as a person, a screen reader and an agent meet it: every step
//! renders without a screen, what the person does goes to the shell as an
//! action, and secrets stay in the shell's buffers.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use tilo_onboarding::{
    OnboardingApp, OnboardingEffect, OnboardingState, Page, Screen, Secrets, TourPage,
};
use ui::framework::ResourceApp as _;
use ui::framework::testing::HeadlessApp;
use ui::tilo_resource::Effect as _;
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<OnboardingApp> {
    HeadlessApp::<OnboardingApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn mounted(page: Page) -> (HeadlessApp<OnboardingApp>, OnboardingState) {
    let state = OnboardingState::new(Secrets::default(), None);
    state.show(page);
    let mut app =
        HeadlessApp::<OnboardingApp>::for_person_with_lent("person", state.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, state)
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

/// Every control with a name, by that name, where it stands. A list row is
/// named by its title and its detail; it is named here by the line it is
/// titled with.
fn named(app: &mut HeadlessApp<OnboardingApp>) -> Vec<(String, Rect)> {
    let titles: std::collections::HashMap<_, _> =
        app.harness().a11y_row_titles().into_iter().collect();
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(id, node)| {
            let bounds = node.bounds()?;
            let name = titles
                .get(&id)
                .cloned()
                .or_else(|| node.label().map(str::to_owned))?;
            Some((name, Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)))
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
fn paired_device_confirmation_names_the_outcome_in_both_themes() {
    let mut app = staged("ready-joined");
    for theme in [ui::theme::Theme::light(), ui::theme::Theme::dark()] {
        app.harness().set_theme(theme);
        app.harness().resize((800, 600)).unwrap();
        app.settle();

        assert!(says(&mut app, "Kitchen laptop"));
        assert!(says(&mut app, "Ada’s devices"));
        assert!(!says(&mut app, "fingerprint"));
        assert!(!says(&mut app, "backup"));
        assert!(!says(&mut app, "System care"));
        let footer = named(&mut app)
            .into_iter()
            .find(|(name, _)| name == "Continue to desktop")
            .expect("the device can enter its desktop")
            .1;
        assert!(footer.y0 >= 0.0 && footer.y1 <= 600.0);
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
    ] {
        assert!(
            scenarios.iter().any(|name| name.starts_with(step)),
            "no scenario shows {step}"
        );
    }
}

#[test]
fn the_intro_offers_both_ways_and_asks_the_shell_for_the_chosen_one() {
    let (mut app, state) = mounted(page(Screen::Intro));
    assert!(says(&mut app, &tilo_onboarding::copy::intro_title()));
    press(&mut app, &tilo_onboarding::t("onboarding-choice-create"));
    assert_eq!(state.take_asked(), [OnboardingEffect::choose("create")]);
    press(&mut app, &tilo_onboarding::t("onboarding-choice-existing"));
    assert_eq!(state.take_asked(), [OnboardingEffect::choose("existing")]);
}

#[test]
fn ticking_the_acknowledgement_asks_the_shell_to_record_it() {
    let (mut app, state) = mounted(page(Screen::NoRecovery));
    press(&mut app, &tilo_onboarding::copy::acknowledgement());
    assert_eq!(
        state.take_asked(),
        [OnboardingEffect::Acknowledge { checked: true }]
    );
}

#[test]
fn create_waits_for_the_shell_to_allow_it() {
    let (mut app, state) = mounted(page(Screen::NoRecovery));
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
    state.show(Page {
        acknowledged: true,
        can_create: true,
        ..page(Screen::NoRecovery)
    });
    app.settle();
    press(&mut app, &create);
    assert_eq!(state.take_asked(), [OnboardingEffect::Continue]);
}

#[test]
fn escape_goes_back_while_setup_may_and_skips_the_tour() {
    let (mut app, state) = mounted(page(Screen::Nearby));
    app.harness().key("Escape");
    app.settle();
    assert_eq!(state.take_asked(), [OnboardingEffect::Back]);

    state.show(Page {
        identity_ready_page: true,
        tour_page: Some(TourPage::Organise),
        ..page(Screen::Done)
    });
    app.settle();
    app.harness().key("Escape");
    app.settle();
    assert_eq!(state.take_asked(), [OnboardingEffect::Skip]);

    state.show(page(Screen::Intro));
    app.settle();
    app.harness().key("Escape");
    app.settle();
    assert!(
        state.take_asked().is_empty(),
        "the intro cannot be dismissed"
    );
}

#[test]
fn a_typed_passphrase_stays_in_the_shells_buffer() {
    let (mut app, state) = mounted(Page {
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
        *state.secrets().connect_passphrase.lock().unwrap(),
        "correct horse battery"
    );
    let asked = state.take_asked();
    assert!(
        asked
            .iter()
            .all(|effect| !format!("{effect:?}").contains("horse")),
        "the secret is not among the actions: {asked:?}"
    );
}

#[test]
fn the_backup_file_is_entered_as_a_value() {
    let (mut app, state) = mounted(page(Screen::Restore));
    press(&mut app, &tilo_onboarding::t("onboarding-connect-file"));
    app.harness().type_text("/b");
    app.settle();
    assert!(state.take_asked().contains(&OnboardingEffect::Enter {
        field: "backup-file".to_owned(),
        value: "/b".to_owned(),
    }));
}

#[test]
fn a_secret_entered_as_an_action_reaches_only_its_buffer() {
    let (mut app, state) = mounted(page(Screen::Restore));
    let output = app
        .effect(&OnboardingEffect::EnterSecret {
            field: "backup-passphrase".to_owned(),
            value: "s3cret passphrase".to_owned(),
        })
        .unwrap();
    app.settle();
    assert!(!format!("{output:?}").contains("s3cret"), "{output:?}");
    assert_eq!(
        *state.secrets().connect_passphrase.lock().unwrap(),
        "s3cret passphrase"
    );
    assert!(state.take_asked().is_empty());
}

#[test]
fn an_agent_cannot_hand_setup_a_secret_or_choose_where_identity_goes() {
    let (mut app, state) = mounted(page(Screen::Restore));
    let refused = app.effect_by(
        &ui::tilo_resource::MemberId::agent("onboarding-test"),
        &OnboardingEffect::EnterSecret {
            field: "backup-passphrase".to_owned(),
            value: "from an agent".to_owned(),
        },
    );
    app.settle();
    assert!(refused.is_err(), "{refused:?}");
    assert!(
        state
            .secrets()
            .connect_passphrase
            .lock()
            .unwrap()
            .is_empty()
    );
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
    let (mut app, state) = mounted(page(Screen::Existing));
    app.effect(&OnboardingEffect::choose("nearby")).unwrap();
    app.effect(&OnboardingEffect::Back).unwrap();
    app.settle();
    assert_eq!(
        state.take_asked(),
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
    let state = OnboardingState::new(Secrets::default(), None);
    state.secrets().enter("new-passphrase", "hunter2hunter2");
    state.show(Page {
        recovery_key_copy: Some(tilo_onboarding::KeyCopy {
            words: tilo_onboarding::SecretText::new("alpha beta gamma".to_owned().into()),
            checks: [1, 2, 3],
        }),
        ..page(Screen::Done)
    });
    let printed = format!("{state:?} {:?}", state.secrets());
    assert!(
        !printed.contains("hunter2") && !printed.contains("gamma"),
        "{printed}"
    );
}

#[test]
fn the_page_changes_in_place_when_the_shell_moves_on() {
    let (mut app, state) = mounted(page(Screen::Intro));
    state.show(page(Screen::Existing));
    app.settle();
    assert!(says(
        &mut app,
        &tilo_onboarding::t("onboarding-existing-title")
    ));
}

/// Review renders of the code screen as a person meets it, in both themes,
/// written only where `TILO_UI_SCREENSHOT_DIR` names a folder.
#[test]
fn the_code_screen_is_reviewed_in_both_themes() {
    let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for theme in [ui::theme::Theme::light(), ui::theme::Theme::dark()] {
        let name = theme.name.clone();
        for scenario in ["nearby", "nearby-joining", "nearby-failed"] {
            let mut app = staged(scenario);
            app.harness().set_theme(theme.clone());
            app.harness().resize(VIEWPORT).unwrap();
            app.settle();
            app.harness()
                .render_png(&directory.join(format!("{scenario}-{name}.png")));
        }
    }
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
