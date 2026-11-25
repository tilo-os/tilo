//! A window with menu buttons, as the page shows it, and what opens each of its menus.
//!
//! `page-menus OUT` writes `light.html` and `dark.html` (the closed window in each theme) and
//! `menus.json` (for each menu, the button's name, what its rows are and the patches the page
//! applies when it opens) into `OUT`; with `--native` it also writes each menu as the app paints
//! it, open in light, dark and high contrast, as `OUT/native/{theme}-{menu}.png`. `tools/web/page-menus.mjs` opens each menu in Chromium, set in the faces
//! the page serves, and fails on any label cut short: the page lays a menu's rows out itself,
//! and only a browser measures them in the face it draws.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "explicit example executable: a malformed invocation or an unwritable directory ends it at once"
)]
use std::path::PathBuf;

use tilo_ui::app::driver::Driver;
use tilo_ui::composition::Composed;
use tilo_ui::composition::page::{Page, document};
use tilo_ui::icons::Icon;
use tilo_ui::widget::{Button, Column, IconWidget, MenuItem, Row, Text};

/// The menus, each on its own button: rows with glyphs and without, a long label, a separator, a
/// destructive row that asks first, one with a shortcut, toggles and a nested menu.
fn menus() -> Vec<(&'static str, Vec<MenuItem>)> {
    vec![
        (
            "Note",
            vec![
                MenuItem::icon_action(Icon::Copy, "Copy link", || {}),
                MenuItem::icon_action(Icon::Pin, "Pin to the top", || {}),
                MenuItem::icon_action(Icon::Folder, "Move to a folder…", || {}),
            ],
        ),
        (
            "Share",
            vec![
                MenuItem::icon_action(
                    Icon::Copy,
                    "Copy a link anyone in the workspace can open",
                    || {},
                ),
                MenuItem::icon_action(Icon::History, "Show in History", || {})
                    .with_shortcut("Ctrl+H"),
                MenuItem::separator(),
                MenuItem::destructive_action("Delete this note", || {})
                    .with_icon(Icon::Trash2)
                    .asking_first(),
            ],
        ),
        (
            "View",
            vec![
                MenuItem::selection("Largest first").with_checked(true),
                MenuItem::selection("Newest first").with_checked(false),
                MenuItem::submenu(
                    "Group by",
                    vec![MenuItem::selection("Folder"), MenuItem::selection("Person")],
                ),
            ],
        ),
    ]
}

fn named<'a>(node: &'a Composed, name: &str) -> Option<&'a Composed> {
    let here = node
        .semantics
        .as_ref()
        .is_some_and(|access| access.name.as_deref() == Some(name));
    if here {
        return Some(node);
    }
    node.children.iter().find_map(|child| named(child, name))
}

fn window() -> Column {
    let mut buttons = Row::new().with_gap(8.0);
    for (name, items) in menus() {
        buttons = buttons.push(Box::new(Button::new(name, name).with_menu(items)));
    }
    let mut tree = Column::new()
        .push(Box::new(Text::new("Sunday lunch")))
        .push(Box::new(buttons));
    for (icon, words) in [
        (Icon::Folder, "Groceries: leeks, butter, a good loaf"),
        (Icon::Pin, "The boiler man comes at ten"),
        (Icon::Copy, "Ask Ana for the tart recipe"),
    ] {
        tree = tree.push(Box::new(
            Row::new()
                .with_gap(8.0)
                .push(Box::new(IconWidget::new(icon)))
                .push(Box::new(Text::new(words))),
        ));
    }
    tree
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: page-menus OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let mut opened = Vec::new();
    let mut text = tilo_ui::text_render::TextRenderer::new();
    let label_style = tilo_ui::Theme::light().text(tilo_ui::TextRole::Body);
    let mut body = None;
    for (name, items) in menus() {
        // A window of its own for each menu, so each opens from the page as first shown.
        let mut driver = Driver::new(tilo_ui::State::new(), Box::new(window()), (720, 480));
        driver.relayout();
        let closed = driver.composition();
        let mut page = Page::new();
        let shown = page.render(&closed);
        let [x, y, width, height] = named(&closed.root, name).expect("each menu's button").rect;
        driver.click_at(x + width / 2.0, y + height / 2.0);
        driver.settle();
        let open = driver.composition();
        assert!(!open.layers.is_empty(), "the {name} menu opens");
        // What each row is, in order, for the check to find drawn: a rule, a toggle's state, a
        // nested menu's chevron, a danger, and how wide the app sets its label.
        let rows: Vec<_> = items
            .iter()
            .map(|item| {
                let width = item
                    .shown_label()
                    .map(|label| text.measure_style(&label, None, &label_style).0);
                serde_json::json!({
                    "separator": item.is_separator(),
                    "checked": item.checked(),
                    "submenu": item.submenu_items().is_some(),
                    "danger": item.is_destructive(),
                    "width": width,
                })
            })
            .collect();
        // The panel as the app lays it out, for the page's to match.
        let [_, _, panel_width, panel_height] = open.layers.first().expect("the menu's layer").rect;
        opened.push(serde_json::json!({
            "menu": name,
            "rows": rows,
            "panel": [panel_width, panel_height],
            "patches": page.patch(&open),
        }));
        body.get_or_insert(shown);
    }
    let body = body.expect("a menu");
    for (scheme, theme) in [
        ("light", tilo_ui::Theme::light()),
        ("dark", tilo_ui::Theme::dark()),
    ] {
        std::fs::write(
            out.join(format!("{scheme}.html")),
            document("Menus", &theme, &body, ""),
        )
        .unwrap();
    }
    std::fs::write(
        out.join("menus.json"),
        serde_json::to_string(&opened).unwrap(),
    )
    .unwrap();
    if std::env::args().nth(2).as_deref() == Some("--native") {
        for (scheme, theme) in [
            ("light", tilo_ui::Theme::light()),
            ("dark", tilo_ui::Theme::dark()),
            ("contrast", tilo_ui::Theme::high_contrast()),
        ] {
            for (name, _) in menus() {
                let mut driver = Driver::new(tilo_ui::State::new(), Box::new(window()), (720, 480));
                driver.set_theme(theme.clone());
                driver.relayout();
                let closed = driver.composition();
                let [x, y, width, height] =
                    named(&closed.root, name).expect("each menu's button").rect;
                driver.click_at(x + width / 2.0, y + height / 2.0);
                // The menu as it rests, every motion it arrived by at its end.
                driver.settle_for_capture(std::time::Duration::from_secs(2));
                driver.render_png(&out.join("native").join(format!("{scheme}-{name}.png")));
            }
        }
    }
}
