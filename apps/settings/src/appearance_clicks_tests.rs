//! Pressing the look controls on the Appearance page, through the pointer a
//! person has, changes the appearance setting they are bound to and shows the
//! choice as chosen.

use crate::{SettingsShell, categories::Category};
use ui::framework::testing::TestApp;

fn settle(app: &mut TestApp<SettingsShell>) {
    for _ in 0..200 {
        app.settle();
        let _ = app.harness().poll_frame();
    }
}

fn node_named(app: &mut TestApp<SettingsShell>, name: &str) -> ui::accesskit::Node {
    let id = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("the {name} control is a target"));
    app.harness()
        .a11y_node(id)
        .unwrap_or_else(|| panic!("the {name} control is a target"))
}

fn click(app: &mut TestApp<SettingsShell>, name: &str) {
    let bounds = node_named(app, name)
        .bounds()
        .unwrap_or_else(|| panic!("the {name} control is placed"));
    app.harness().click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    settle(app);
}

#[test]
fn pressing_a_look_choice_changes_the_appearance_setting_and_shows_chosen() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    app.harness().resize((1216, 720)).unwrap();
    let _ = app.harness().poll_frame();
    app.call(
        "open",
        serde_json::json!({ "anchor": crate::effects::pane_row_id(Category::Appearance) }),
    )
    .expect("the Appearance pane opens");
    settle(&mut app);

    let mode = |app: &mut TestApp<SettingsShell>| {
        app.settings::<crate::prefs::AppearancePrefs>().get().mode
    };
    assert_eq!(mode(&mut app), "dark", "the stock look is dark");
    click(&mut app, "Light");
    assert_eq!(
        mode(&mut app),
        "light",
        "pressing Light makes the look light"
    );
    assert_eq!(
        node_named(&mut app, "Light").toggled(),
        Some(ui::accesskit::Toggled::True),
        "the Light segment shows itself chosen"
    );
    assert_eq!(
        node_named(&mut app, "Dark").toggled(),
        Some(ui::accesskit::Toggled::False),
        "the Dark segment no longer shows itself chosen"
    );

    click(&mut app, "Pink");
    assert_eq!(
        app.settings::<crate::prefs::AppearancePrefs>()
            .get()
            .accent
            .as_deref(),
        Some("pink"),
        "pressing Pink accents the look pink"
    );
    assert_eq!(
        node_named(&mut app, "Pink").is_selected(),
        Some(true),
        "the Pink swatch shows itself chosen"
    );
}
