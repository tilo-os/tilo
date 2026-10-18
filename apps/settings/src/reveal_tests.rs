//! A search hit or a link that names a setting opens its page scrolled to
//! the setting, with the keyboard on its control: typing "disable touchpad
//! while typing" in the desktop palette lands on that switch, not on the
//! top of a long page.

use crate::{SettingsShell, categories::Category, settings_app::SettingsApp};
use ui::framework::testing::TestApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget;

const VIEWPORT: (u32, u32) = (1200, 640);

fn focused(widget: &dyn Widget) -> bool {
    widget.is_focused() || widget.children().into_iter().any(focused)
}

/// The rect of the first control of `setting`, and whether the keyboard is
/// on it.
fn control(root: &dyn Widget, setting: &settings_store::index::Setting) -> Option<(Rect, bool)> {
    if root.test_id().is_some_and(|id| setting.is_control(id)) {
        return Some((root.get_rect(), focused(root)));
    }
    root.children()
        .into_iter()
        .find_map(|child| control(child, setting))
}

fn settle(app: &mut TestApp<SettingsShell>) {
    for _ in 0..20 {
        app.settle();
        let _ = app.harness().poll_frame();
    }
}

#[test]
fn a_setting_opens_scrolled_to_its_control_with_the_keyboard_on_it() {
    ui::i18n::I18nManager::init("en-US");
    for id in [
        "power.critical",
        "pointing.middle_click",
        "appearance.hide_top_bar",
        "notifications.quiet_hours",
    ] {
        let setting = settings_store::index::setting(id).expect("indexed");
        let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
        app.harness().resize(VIEWPORT).unwrap();
        let _ = app.harness().poll_frame();
        app.call(
            "open",
            serde_json::json!({ "anchor": format!("{}{id}", crate::effects::SETTING_ANCHOR) }),
        )
        .expect("the setting opens");
        settle(&mut app);
        assert_eq!(
            app.with_widget::<SettingsApp, _>(|settings| settings.selected()),
            Category::from_slug(setting.page),
            "{id} opens its page"
        );
        assert_eq!(
            app.with_widget::<SettingsApp, _>(|settings| settings.pending_reveal()),
            Some(None),
            "{id} was shown"
        );
        let (rect, focused) =
            control(app.harness().root_widget(), setting).expect("the page shows the control");
        assert!(focused, "{id}: the keyboard is on its control");
        assert!(
            rect.y0 >= 0.0 && rect.y1 <= f64::from(VIEWPORT.1),
            "{id}: its control is on screen at {rect:?}"
        );
    }
}

/// A reveal takes the keyboard from the retained filter and can cause a
/// follow-up layout. Resizing must retain that target, but moving to the filter must
/// stop retaining it: a reveal is not a permanent focus request.
#[test]
fn a_revealed_control_keeps_focus_across_layouts_until_the_keyboard_moves() {
    ui::i18n::I18nManager::init("en-US");
    let setting = settings_store::index::setting("appearance.hide_top_bar").expect("indexed");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    app.harness().resize(VIEWPORT).unwrap();
    app.harness().click_named("settings.search").unwrap();
    assert!(
        filter_focused(app.harness().root_widget()),
        "the filter starts with the keyboard"
    );
    app.call(
        "open",
        serde_json::json!({ "anchor": format!("{}{}", crate::effects::SETTING_ANCHOR, setting.id) }),
    )
    .expect("the setting opens");
    settle(&mut app);
    assert!(
        !filter_focused(app.harness().root_widget()),
        "revealing transfers the keyboard away from the filter"
    );
    for viewport in [(1200, 700), VIEWPORT] {
        app.harness().resize(viewport).unwrap();
        app.harness().recompose().expect("a full pane layout");
        settle(&mut app);
        let (rect, focused) = control(app.harness().root_widget(), setting).expect("the control");
        assert!(
            focused,
            "the revealed control keeps the keyboard after layout"
        );
        assert!(
            rect.y0 >= 0.0 && rect.y1 <= f64::from(viewport.1),
            "the control remains on screen at {rect:?}"
        );
    }
    app.harness().click_named("settings.search").unwrap();
    settle(&mut app);
    assert!(
        !control(app.harness().root_widget(), setting).unwrap().1,
        "the keyboard leaves the revealed control"
    );
    app.harness().resize((1200, 700)).unwrap();
    app.harness().recompose().expect("a full pane layout");
    settle(&mut app);
    assert!(
        !control(app.harness().root_widget(), setting).unwrap().1,
        "layout must not return the keyboard to a previous reveal"
    );
    fn filter_focused(widget: &dyn Widget) -> bool {
        (widget.test_id() == Some("settings.search") && focused(widget))
            || widget.children().into_iter().any(filter_focused)
    }
    assert!(
        filter_focused(app.harness().root_widget()),
        "the filter keeps the keyboard"
    );
}

/// A link naming a setting opens Settings scrolled to it, as the palette's
/// hit does.
#[test]
fn a_setting_link_shows_the_setting() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::launched(&["settings://power#power.critical"])
        .expect("the actual framework launch");
    settle(&mut app);
    let window = app.harness().root_widget().get_rect();
    let setting = settings_store::index::setting("power.critical").expect("indexed");
    assert_eq!(
        app.with_widget::<SettingsApp, _>(|settings| settings.selected()),
        Some(Category::Power)
    );
    let (rect, focused) =
        control(app.harness().root_widget(), setting).expect("the page shows the control");
    assert!(
        rect.y0 >= window.y0 && rect.y1 <= window.y1,
        "its control is on screen at {rect:?} in {window:?}"
    );
    // A fresh window gives the keyboard to its first control when it is
    // shown; the open window above keeps it on the setting.
    let _ = focused;
}

/// A reopened window shows the page it showed, unless a link names another.
#[test]
fn a_reopened_window_shows_its_page() {
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    app.call(
        "open",
        serde_json::json!({ "anchor": crate::effects::pane_row_id(Category::Keyboard) }),
    )
    .expect("the page opens");
    settle(&mut app);
    let mut reopened = app.reopen().expect("the same window, reopened");
    settle(&mut reopened);
    assert_eq!(
        reopened.with_widget::<SettingsApp, _>(|settings| settings.selected()),
        Some(Category::Keyboard)
    );
}
