//! A relayout (a resize, a theme change, a parent's reflow) keeps a pane's
//! controls, and with them the hover, press and focus they hold; only a
//! change in what the page shows, or in the language, rebuilds them.

use crate::SettingsShell;
use crate::effects::{NavigationEffect, SettingsEffect};
use crate::prefs::AppearancePrefs;
use ui::framework::testing::TestApp;
use ui::i18n::{I18nManager, PseudoLocale};
use ui::vello::kurbo::Rect;
use ui::widget::Widget;

fn opened(pane: &str) -> TestApp<SettingsShell> {
    I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    app.harness().resize((1200, 900)).unwrap();
    app.effect(&SettingsEffect::from(NavigationEffect::OpenPane {
        pane: pane.into(),
    }))
    .expect("the pane opens");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<SettingsShell>) {
    for _ in 0..10 {
        app.settle();
        let _ = app.harness().poll_frame();
    }
}

/// Which widget the control `id` is, and where.
fn control(app: &mut TestApp<SettingsShell>, id: &str) -> (*const (), Rect) {
    let widget = ui::widget::find_test_id(app.harness().root_widget(), id)
        .unwrap_or_else(|| panic!("the page shows {id}"));
    (
        std::ptr::from_ref::<dyn Widget>(widget).cast(),
        widget.get_rect(),
    )
}

fn relayouts(app: &mut TestApp<SettingsShell>) {
    for (viewport, theme) in [
        ((900, 800), ui::Theme::light()),
        ((1400, 900), ui::Theme::high_contrast()),
        ((1200, 900), ui::Theme::dark()),
    ] {
        app.harness().relayout_themed(viewport, theme).unwrap();
        let _ = app.harness().poll_frame();
    }
}

#[test]
fn relayouts_keep_the_controls_of_panes_that_follow_their_fields() {
    for (pane, id) in [
        ("appearance", "appearance.ui_scale"),
        ("accessibility", "a11y.sticky_keys"),
        ("default-apps", "open-workspace-app-defaults"),
    ] {
        let mut app = opened(pane);
        let (before, _) = control(&mut app, id);
        relayouts(&mut app);
        assert_eq!(control(&mut app, id).0, before, "{pane} kept {id}");
    }
}

/// A press that a relayout interrupts still lands: the toggle that took the
/// press is the one the release reaches.
#[test]
fn a_press_survives_a_relayout() {
    let mut app = opened("accessibility");
    let id = "a11y.high_contrast";
    let centre = control(&mut app, id).1.center();
    app.harness()
        .send(&ui::synthetic_input::cursor_moved(centre.x, centre.y));
    app.harness().send(&ui::synthetic_input::mouse_press());
    relayouts(&mut app);
    let centre = control(&mut app, id).1.center();
    app.harness()
        .send(&ui::synthetic_input::cursor_moved(centre.x, centre.y));
    app.harness().send(&ui::synthetic_input::mouse_release());
    settle(&mut app);
    assert!(app.settings::<AppearancePrefs>().get().high_contrast);
}

/// A value changed elsewhere moves the control it has; what the page shows
/// otherwise (the chosen theme's tile) and the language rebuild it.
#[test]
fn values_move_controls_and_shape_or_language_rebuild_them() {
    let mut app = opened("appearance");
    let (before, _) = control(&mut app, "appearance.ui_scale");
    app.settings::<AppearancePrefs>()
        .update(|prefs| prefs.reduce_motion = true)
        .unwrap();
    settle(&mut app);
    assert_eq!(control(&mut app, "appearance.ui_scale").0, before);

    app.settings::<AppearancePrefs>()
        .update(|prefs| prefs.choose_theme("Dark graphite"))
        .unwrap();
    settle(&mut app);
    let (rebuilt, _) = control(&mut app, "appearance.ui_scale");
    assert_ne!(rebuilt, before, "a new theme rebuilds the page");

    let english = app.harness().inspection_json().unwrap();
    I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
    settle(&mut app);
    let relabelled = app.harness().inspection_json().unwrap();
    let rebuilt_again = control(&mut app, "appearance.ui_scale").0;
    I18nManager::set_pseudo_locale(PseudoLocale::Off);
    assert_ne!(rebuilt_again, rebuilt, "the language rebuilds the page");
    assert_ne!(english, relabelled);
}
