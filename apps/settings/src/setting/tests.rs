//! A setting found by Settings' search is its row: toggling it there applies
//! it, the same control as in its pane.

use settings_store::index;
use ui::framework::testing::TestApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget;

use crate::SettingsShell;
use crate::categories::Category;
use crate::prefs::AppearancePrefs;

const VIEWPORT: (u32, u32) = (1472, 920);

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

/// Settings in its window, before anything is searched for.
fn mounted() -> TestApp<SettingsShell> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    app.harness().resize(VIEWPORT).unwrap();
    settle(&mut app);
    app
}

/// Type `query` into the window's search, and let what it finds be drawn.
fn search(app: &mut TestApp<SettingsShell>, query: &str) {
    app.with_widget::<crate::window::Inspected, _>(|settings| settings.narrow_sidebar(query))
        .expect("the window is Settings");
    settle(app);
}

fn searching(query: &str) -> TestApp<SettingsShell> {
    let mut app = mounted();
    search(&mut app, query);
    app
}

fn content_left() -> f64 {
    crate::sidebar::width_for_text_scale(1.0) - 1.0
}

fn found(app: &mut TestApp<SettingsShell>) -> Vec<&'static str> {
    app.with_widget::<crate::window::Inspected, _>(|settings| settings.found_settings())
        .unwrap_or_default()
}

#[test]
fn every_setting_row_is_an_indexed_setting_drawn_by_its_control() {
    // One window, searched for each setting in turn. What a search finds
    // follows from the words typed, what it draws is built anew from what it
    // found, and no row here changes a setting or sends the window input.
    // Each row checks that the window draws this search and nothing an
    // earlier one left.
    let mut app = mounted();
    assert_nothing_found(&mut app, "a window just opened");
    let mut ids = std::collections::HashSet::new();
    for entry in &crate::area::registry().settings {
        assert!(ids.insert(entry.id), "{} has two rows", entry.id);
        let setting = index::setting(entry.id)
            .unwrap_or_else(|| panic!("{} is not in the settings index", entry.id));
        search(&mut app, &crate::i18n::t(&setting.name_key()));
        assert!(
            found(&mut app).contains(&entry.id),
            "{}: its name finds it, got {:?}",
            entry.id,
            found(&mut app)
        );
        assert_shows_what_was_found(&mut app, entry.id);
        match entry.shown {
            super::Shown::Bound(_) => assert!(
                control(app.harness().root_widget(), setting, content_left()).is_some(),
                "{}: its row is shown with its control",
                entry.id
            ),
            super::Shown::Value(_) => assert!(
                widget(
                    app.harness().root_widget(),
                    &format!("settings.found:{}", entry.id)
                )
                .is_some(),
                "{}: it is listed by name",
                entry.id
            ),
            super::Shown::BoundSwitch(..) | super::Shown::ViewSwitch(..) => assert!(
                control(app.harness().root_widget(), setting, content_left())
                    .is_some_and(|(_, checked)| checked.is_some())
                    || widget(
                        app.harness().root_widget(),
                        &format!("settings.found:{}", entry.id)
                    )
                    .is_some(),
                "{}: it is its switch, or listed by name while it cannot be switched",
                entry.id
            ),
        }
    }
    // Nothing typed, or only spaces: nothing is found, and nothing of the
    // last search stays in the window.
    for nothing in ["", "   "] {
        search(&mut app, nothing);
        assert_nothing_found(&mut app, &format!("{nothing:?}"));
    }
}

/// The ids of the parts of the window a search draws: a listed setting
/// (`settings.found:`), its button (`settings.found.show:`) and a found
/// page's button (`settings.found.open:`).
fn drawn_of_search(root: &dyn Widget, out: &mut Vec<String>) {
    if let Some(id) = root.test_id().filter(|id| id.starts_with("settings.found")) {
        out.push(id.to_owned());
    }
    for child in root.children() {
        drawn_of_search(child, out);
    }
}

/// The window draws what this search found and nothing of another: one
/// section for each page the found settings are on, and no listed setting
/// that is not among them.
fn assert_shows_what_was_found(app: &mut TestApp<SettingsShell>, row: &str) {
    let found = found(app);
    let mut pages: Vec<&str> = found
        .iter()
        .filter_map(|id| index::setting(id))
        .map(|setting| setting.page)
        .collect();
    pages.sort_unstable();
    pages.dedup();
    let mut drawn = Vec::new();
    drawn_of_search(app.harness().root_widget(), &mut drawn);
    let mut sections: Vec<&str> = drawn
        .iter()
        .filter_map(|id| id.strip_prefix("settings.found.open:"))
        .collect();
    sections.sort_unstable();
    assert_eq!(
        sections, pages,
        "{row}: the window draws one section for each page of what was found ({found:?})"
    );
    for id in &drawn {
        let listed = id
            .strip_prefix("settings.found:")
            .or_else(|| id.strip_prefix("settings.found.show:"));
        if let Some(listed) = listed {
            assert!(
                found.contains(&listed),
                "{row}: the window lists {listed}, which this search did not find ({found:?})"
            );
        }
    }
}

/// The window shows its page and nothing a search found: no setting is
/// found, and no part of what a search draws (a listed setting, its button,
/// a found page's button) is in the window.
fn assert_nothing_found(app: &mut TestApp<SettingsShell>, after: &str) {
    assert!(
        found(app).is_empty(),
        "{after}: with nothing typed nothing is found, got {:?}",
        found(app)
    );
    let mut drawn = Vec::new();
    drawn_of_search(app.harness().root_widget(), &mut drawn);
    assert!(
        drawn.is_empty(),
        "{after}: with nothing typed the window draws nothing a search found, got {drawn:?}"
    );
}

/// What a search finds, asked as the window asks it and without a window:
/// nothing for nothing typed, a setting by its name, each setting once and
/// only on a page this build has, and the same whatever space surrounds the
/// words.
#[test]
fn a_search_finds_each_setting_once_and_only_on_a_page_this_build_has() {
    ui::i18n::I18nManager::init("en-US");
    let ids = |query: &str| -> Vec<&'static str> {
        super::matching(query)
            .iter()
            .map(|setting| setting.id)
            .collect()
    };
    for nothing in ["", " ", "\t \n"] {
        assert!(
            ids(nothing).is_empty(),
            "{nothing:?} finds nothing, got {:?}",
            ids(nothing)
        );
    }
    for entry in &crate::area::registry().settings {
        let setting = index::setting(entry.id)
            .unwrap_or_else(|| panic!("{} is not in the settings index", entry.id));
        let name = crate::i18n::t(&setting.name_key());
        let hits = super::matching(&name);
        assert!(
            hits.iter().any(|hit| hit.id == entry.id),
            "{}: its name {name:?} finds it, got {:?}",
            entry.id,
            ids(&name)
        );
        let mut once = std::collections::HashSet::new();
        for hit in &hits {
            assert!(once.insert(hit.id), "{name:?} finds {} twice", hit.id);
            assert!(
                Category::from_slug(hit.page).is_some(),
                "{name:?} finds {} on {}, a page this build has not",
                hit.id,
                hit.page
            );
        }
    }
    for words in ["high contrast", "screen reader"] {
        assert!(!ids(words).is_empty(), "{words:?} finds a setting");
        assert_eq!(
            ids(&format!("  {words}\t")),
            ids(words),
            "{words:?}: the space around the words changes nothing"
        );
    }
}

fn widget<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
    if root.test_id() == Some(id) {
        return Some(root);
    }
    root.children()
        .into_iter()
        .find_map(|child| widget(child, id))
}

fn spoken(widget: &dyn Widget) -> Option<String> {
    widget
        .accessibility()
        .and_then(|node| node.value)
        .or_else(|| widget.children().into_iter().find_map(spoken))
}

#[test]
fn a_setting_listed_by_name_says_what_its_pane_shows() {
    let setting = index::setting("power.lock_after").expect("indexed");
    let mut app = searching(&crate::i18n::t(&setting.name_key()));
    let row = format!("settings.found:{}", setting.id);
    let before = widget(app.harness().root_widget(), &row)
        .and_then(spoken)
        .expect("it says what it is set to");

    let minutes = app
        .settings::<crate::prefs::PowerPrefs>()
        .get()
        .lock_minutes;
    let sooner = if minutes == 1 { 2 } else { 1 };
    app.call_on(
        "settings:settings.power",
        "field-edit",
        serde_json::json!({ "edit": { "path": [{ "field": "lock_minutes" }], "op": "set", "value": sooner } }),
    )
    .expect("locking sooner asks nothing");
    settle(&mut app);
    let after = widget(app.harness().root_widget(), &row)
        .and_then(spoken)
        .expect("it says what it is set to");
    assert_ne!(after, before, "the row follows the change");

    app.call(
        "open",
        serde_json::json!({ "anchor": format!("{}{}", crate::effects::SETTING_ANCHOR, setting.id) }),
    )
    .expect("the setting opens in its pane");
    settle(&mut app);
    let shown = widget(app.harness().root_widget(), "power.lock")
        .and_then(spoken)
        .expect("the control says its value");
    assert_eq!(shown, after, "search says what the pane shows");
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

    app.call(
        "open",
        serde_json::json!({ "anchor": format!("{}{}", crate::effects::SETTING_ANCHOR, setting.id) }),
    )
    .expect("the setting opens in its pane");
    settle(&mut app);
    assert_eq!(
        app.with_widget::<crate::window::Inspected, _>(|settings| settings.selected()),
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

#[test]
fn a_device_switch_found_by_search_runs_its_panes_action() {
    use crate::accessibility_settings::AccessibilitySettings;
    let setting = index::setting("a11y.screen_reader").expect("indexed");
    let mut app = searching(&crate::i18n::t(&setting.name_key()));
    let left = content_left();
    let before = app.settings::<AccessibilitySettings>().get().screen_reader;
    let (rect, checked) = control(app.harness().root_widget(), setting, left)
        .expect("with a speech engine, the screen reader is its switch");
    assert_eq!(checked, Some(before), "the switch shows it as it is");

    app.harness().click_at(rect.center().x, rect.center().y);
    settle(&mut app);
    assert_eq!(
        app.settings::<AccessibilitySettings>().get().screen_reader,
        !before,
        "flipping it where it was found turns the screen reader over"
    );
    let (_, checked) = control(app.harness().root_widget(), setting, left).expect("still found");
    assert_eq!(checked, Some(!before), "the switch shows what it changed");
}

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

fn held(widget: &dyn Widget) -> String {
    fn walk(node: &ui::a11y::AccessNode, out: &mut String) {
        use std::fmt::Write as _;
        if let Some(checked) = node.state.checked {
            let _ = write!(out, "checked={checked};");
        }
        if let Some(value) = &node.value {
            let _ = write!(out, "value={value};");
        }
        if node.state.selected {
            let _ = write!(out, "selected={:?};", node.name);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = String::new();
    if let Some(access) = widget.accessibility() {
        walk(&access, &mut out);
    }
    for child in widget.children() {
        out.push_str(&held(child));
    }
    out
}

fn control_widget<'a>(
    root: &'a dyn Widget,
    setting: &index::Setting,
    left: f64,
) -> Option<&'a dyn Widget> {
    let rect = root.get_rect();
    if root.test_id().is_some_and(|id| setting.is_control(id))
        && rect.x0 >= left
        && rect.area() > 0.0
    {
        return Some(root);
    }
    root.children()
        .into_iter()
        .find_map(|child| control_widget(child, setting, left))
}

#[test]
fn every_indexed_choice_names_the_field_its_row_shows() {
    for setting in index::SETTINGS {
        let Some(choice) = setting.choice else {
            continue;
        };
        assert!(
            super::entry(setting.id).is_some(),
            "{}: a choice the palette offers has its row in Settings",
            setting.id
        );
        let mut app = searching(&crate::i18n::t(&setting.name_key()));
        let left = content_left();
        let path: Vec<serde_json::Value> = choice
            .path()
            .map(|field| serde_json::json!({ "field": field }))
            .collect();
        let mut shown = std::collections::HashSet::new();
        for option in choice.options {
            app.call_confirmed_on(
                &format!("settings:{}", choice.section),
                "field-edit",
                serde_json::json!({ "edit": { "path": path, "op": "set", "value": option.json() } }),
            )
            .unwrap_or_else(|error| panic!("{} = {}: {error}", setting.id, option.value));
            settle(&mut app);
            let holds = match super::entry(setting.id).map(|entry| &entry.shown) {
                Some(super::Shown::Value(read)) => app
                    .with_widget::<crate::window::Inspected, _>(|settings| {
                        read(settings, &super::Follows::default())
                    })
                    .flatten()
                    .unwrap_or_else(|| panic!("{}: what it is set to is read", setting.id)),
                _ => control_widget(app.harness().root_widget(), setting, left)
                    .map_or_else(|| panic!("{}: its control is found", setting.id), held),
            };
            assert!(
                shown.insert(holds.clone()),
                "{}: the row shows {} as it shows another value ({holds})",
                setting.id,
                option.value
            );
        }
    }
}
