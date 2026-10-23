//! A setting found by Settings' search is its row: toggling it there
//! applies it, the same control as in its pane.

use settings_store::index;
use ui::framework::testing::TestApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget;

use crate::categories::Category;
use crate::prefs::AppearancePrefs;
use crate::{SettingsShell, settings_app::SettingsApp};

const VIEWPORT: (u32, u32) = (1472, 920);

/// The rect of the first control of `setting` at or right of `left`, and
/// whether it is checked where it is a switch.
fn control(root: &dyn Widget, setting: &index::Setting, left: f64) -> Option<(Rect, Option<bool>)> {
    let rect = root.get_rect();
    if root.test_id().is_some_and(|id| setting.is_control(id))
        && rect.x0 >= left
        && rect.area() > 0.0
    {
        return Some((rect, switched(root)));
    }
    root.children()
        .into_iter()
        .find_map(|child| control(child, setting, left))
}

/// Whether the switch in `widget` is on, as it tells assistive technology;
/// `None` where it holds no switch.
fn switched(widget: &dyn Widget) -> Option<bool> {
    match widget.accessibility() {
        Some(node) if node.role == ui::a11y::Role::Switch => node.state.checked,
        _ => widget.children().into_iter().find_map(switched),
    }
}

fn settle(app: &mut TestApp<SettingsShell>) {
    for _ in 0..20 {
        app.settle();
        let _ = app.harness().poll_frame();
    }
}

/// Settings, wide, with `query` typed into the window bar's search.
fn searching(query: &str) -> TestApp<SettingsShell> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    app.harness().resize(VIEWPORT).unwrap();
    settle(&mut app);
    app.surface::<SettingsApp, _>(|settings| settings.narrow_sidebar(query))
        .expect("the window is Settings");
    settle(&mut app);
    app
}

/// Where the content starts, right of the sidebar.
fn content_left() -> f64 {
    crate::sidebar::width_for_text_scale(1.0) - 1.0
}

fn found(app: &mut TestApp<SettingsShell>) -> Vec<&'static str> {
    app.with_widget::<SettingsApp, _>(|settings| settings.found_settings())
        .unwrap_or_default()
}

#[test]
fn every_setting_row_is_an_indexed_setting_drawn_by_its_control() {
    let mut ids = std::collections::HashSet::new();
    for entry in &crate::area::registry().settings {
        assert!(ids.insert(entry.id), "{} has two rows", entry.id);
        let setting = index::setting(entry.id)
            .unwrap_or_else(|| panic!("{} is not in the settings index", entry.id));
        let mut app = searching(&crate::i18n::t(&setting.name_key()));
        assert!(
            found(&mut app).contains(&entry.id),
            "{}: its name finds it, got {:?}",
            entry.id,
            found(&mut app)
        );
        let left = content_left();
        assert!(
            control(app.harness().root_widget(), setting, left).is_some(),
            "{}: its row is shown with its control",
            entry.id
        );
    }
}

#[test]
fn toggling_a_setting_found_by_search_applies_it_as_its_pane_does() {
    let setting = index::setting("appearance.high_contrast").expect("indexed");
    let mut app = searching("high contrast");
    assert_eq!(
        found(&mut app).first(),
        Some(&"appearance.high_contrast"),
        "the setting named is found first"
    );
    let left = content_left();
    let before = app.settings::<AppearancePrefs>().get().high_contrast;
    let (rect, checked) =
        control(app.harness().root_widget(), setting, left).expect("the switch is found");
    assert_eq!(
        checked,
        Some(before),
        "the switch shows the setting as it is"
    );

    app.harness().click_at(rect.center().x, rect.center().y);
    settle(&mut app);
    assert_eq!(
        app.settings::<AppearancePrefs>().get().high_contrast,
        !before,
        "flipping the switch where it was found applies the setting"
    );
    let (_, checked) = control(app.harness().root_widget(), setting, left).expect("still found");
    assert_eq!(checked, Some(!before), "the switch shows what it changed");

    // The same control in its pane shows the change, and changes it back.
    app.call(
        "open",
        serde_json::json!({ "anchor": format!("{}{}", crate::effects::SETTING_ANCHOR, setting.id) }),
    )
    .expect("the setting opens in its pane");
    settle(&mut app);
    assert_eq!(
        app.with_widget::<SettingsApp, _>(|settings| settings.selected()),
        Some(Category::Accessibility)
    );
    assert!(
        found(&mut app).is_empty(),
        "the pane is shown, not what was found"
    );
    let (rect, checked) =
        control(app.harness().root_widget(), setting, left).expect("the pane's switch");
    assert_eq!(
        checked,
        Some(!before),
        "the pane shows the change made in search"
    );
    app.harness().click_at(rect.center().x, rect.center().y);
    settle(&mut app);
    assert_eq!(
        app.settings::<AppearancePrefs>().get().high_contrast,
        before
    );
}

/// The palette flips a yes-or-no setting through the section field the
/// index names for it ([`index::Switch`]): the field its row in Settings
/// shows and changes.
#[test]
fn every_indexed_switch_names_the_field_its_row_shows() {
    for setting in index::SETTINGS {
        let Some(switch) = setting.switch else {
            continue;
        };
        assert!(
            super::entry(setting.id).is_some(),
            "{}: a switch the palette offers has its row in Settings",
            setting.id
        );
        let mut app = searching(&crate::i18n::t(&setting.name_key()));
        let left = content_left();
        let checked = |app: &mut TestApp<SettingsShell>| {
            control(app.harness().root_widget(), setting, left)
                .and_then(|(_, checked)| checked)
                .unwrap_or_else(|| panic!("{}: its switch is found", setting.id))
        };
        let before = checked(&mut app);
        let path: Vec<serde_json::Value> = switch
            .path()
            .map(|field| serde_json::json!({ "field": field }))
            .collect();
        // As the palette flips it, with the person's confirmation where the
        // change makes the device less private.
        app.call_confirmed_on(
            &format!("settings:{}", switch.section),
            "field-edit",
            serde_json::json!({ "edit": { "path": path, "op": "set", "value": !before } }),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", setting.id));
        settle(&mut app);
        assert_eq!(
            checked(&mut app),
            !before,
            "{}: the row shows {}.{} as the palette changes it",
            setting.id,
            switch.section,
            switch.field
        );
    }
}
