//! The interaction layer of a conversation as the UI host runs it, over the `interaction`
//! package: a message's own context menu with the actions it did not place added, and its
//! swipe, each run in the window without asking the app what to show. What a hover repaints is
//! counted on the toolbar itself (`ui::widget::HoverToolbar`'s tests).
//!
//! With `TILO_INTERACTION_SHOTS=<dir>` the test `shots` writes each state at desktop and phone
//! sizes, light and dark.

use crate::change_marks::{menu_items, right_click};
use tilo_app_test_host::TestApp;
use tilolang_process::window::app::TlApp;

const DESKTOP: (u32, u32) = (1280, 800);
const PHONE: (u32, u32) = (420, 860);

fn staged(viewport: (u32, u32)) -> TestApp<TlApp> {
    crate::support::installed("interaction");
    let mut app = TestApp::<TlApp>::capture_at(Some("a conversation"), viewport)
        .expect("the scenario stages");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    crate::change_marks::captured(app, 8);
}

/// The rects of the widgets named `widget`, in tree order.
pub(crate) fn rects_of(window: &ui::headless::FrameHarness, widget: &str) -> Vec<[f64; 4]> {
    let snapshot = window.inspection();
    let all = snapshot.root.nodes();
    all.into_iter()
        .filter(|node| node.widget == widget)
        .map(|node| node.rect)
        .collect()
}

/// Where the open menu draws its item `name`.
fn menu_item_bounds(app: &mut TestApp<TlApp>, name: &str) -> Option<ui::kurbo::Rect> {
    fn find(node: &ui::a11y::AccessNode, name: &str) -> Option<ui::kurbo::Rect> {
        if node.role == ui::a11y::Role::MenuItem && node.name.as_deref() == Some(name) {
            return node
                .bounds
                .map(|b| ui::kurbo::Rect::new(b.x0, b.y0, b.x1, b.y1));
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.iter()
        .filter(|node| node.widget == "ContextMenu")
        .find_map(|node| {
            node.semantics
                .as_ref()
                .and_then(|access| find(access, name))
        })
}

/// Every accessible name in the window, for a failure's message.
fn all_names(app: &mut TestApp<TlApp>) -> Vec<String> {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.iter()
        .filter_map(|node| node.semantics.as_ref().and_then(|a| a.name.clone()))
        .collect()
}

/// Whether some text in the window reads `text`.
fn shows(app: &mut TestApp<TlApp>, text: &str) -> bool {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.iter().any(|node| {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(text))
    })
}

pub(crate) fn center(rect: [f64; 4]) -> (f64, f64) {
    (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0)
}

/// The last message's row.
fn last_message(app: &mut TestApp<TlApp>) -> [f64; 4] {
    if let Some(rect) = rects_of(app.harness(), "SwipeRow").last() {
        return *rect;
    }
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    let names: Vec<String> = all.iter().map(|node| node.widget.clone()).collect();
    let said: Vec<String> = all
        .iter()
        .filter_map(|node| node.semantics.as_ref().and_then(|a| a.name.clone()))
        .collect();
    panic!("each message swipes; the window holds {names:?}, saying {said:?}");
}

#[test]
fn a_messages_menu_is_its_views_then_every_action_it_did_not_place() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    let row = last_message(&mut app);
    right_click(&mut app, center(row));
    let items = menu_items(&mut app);
    assert_eq!(
        items.first().map(String::as_str),
        Some("👍"),
        "the emoji bar comes first: {items:?}"
    );
    let pin = items.iter().position(|item| item == "Pin");
    assert!(pin.is_some(), "the view's own Pin: {items:?}");
    assert_eq!(
        items.iter().filter(|item| item.starts_with("Pin")).count(),
        1,
        "Pin is placed by the view, so the system does not add it again: {items:?}"
    );
    for action in ["React", "Mark read", "Retract"] {
        let at = items.iter().position(|item| item == action);
        assert!(
            at.is_some_and(|at| at > pin.unwrap_or(0)),
            "{action} is reachable, after the view's own: {items:?}"
        );
    }
}

#[test]
fn choosing_the_views_own_command_runs_its_action_in_the_app() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    let row = last_message(&mut app);
    right_click(&mut app, center(row));
    let pin = menu_item_bounds(&mut app, "Pin").expect("Pin is offered");
    let (x, y) = (pin.center().x, pin.center().y);
    app.harness().click_at(x, y);
    settle(&mut app);
    let names = all_names(&mut app);
    assert!(
        shows(&mut app, "Pinned"),
        "the menu's Pin pinned the message; the window reads {names:?}"
    );
}

#[test]
fn a_swipe_past_the_threshold_runs_the_action_in_the_app() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(PHONE);
    assert!(!shows(&mut app, "Pinned"));
    let row = last_message(&mut app);
    let (x, y) = center(row);
    let width = row[2];
    app.harness()
        .swipe_at((x + width * 0.3, y), (x - width * 0.4, y));
    settle(&mut app);
    assert!(
        shows(&mut app, "Pinned"),
        "the trailing swipe pinned the message"
    );
}

/// The composer's centre.
pub(crate) fn composer(window: &ui::headless::FrameHarness) -> (f64, f64) {
    let inputs = rects_of(window, "RichInput");
    center(*inputs.last().expect("the transcript has its composer"))
}

/// The composer's send button: the last button on the composer's line, at its end past the field
/// (a composer may stand one before the field too, such as the way to add files).
pub(crate) fn send_button(window: &ui::headless::FrameHarness) -> (f64, f64) {
    let (_, y) = composer(window);
    let buttons = rects_of(window, "Button");
    let line = buttons
        .iter()
        .filter(|rect| (center(**rect).1 - y).abs() < rect[3])
        .max_by(|one, two| center(**one).0.total_cmp(&center(**two).0))
        .copied()
        .expect("the composer has its send button");
    center(line)
}

#[test]
fn shots() {
    let Some(dir) = std::env::var_os("TILO_INTERACTION_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _settled = ui::motion::testing::settled();
    for (size, viewport) in [("desktop", DESKTOP), ("phone", PHONE)] {
        for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
            let mut app = staged(viewport);
            app.harness().set_theme(theme);
            settle(&mut app);
            let name = |state: &str| dir.join(format!("{state}-{size}-{shade}.png"));
            app.harness().render_png(&name("rest"));
            let row = last_message(&mut app);
            let (x, y) = center(row);
            app.harness().send(&ui::synthetic_input::cursor_moved(x, y));
            app.harness().relayout();
            app.harness().render_png(&name("hover"));
            right_click(&mut app, (x, y));
            app.harness().relayout();
            app.harness().render_png(&name("menu"));
            app.harness().send(&ui::synthetic_input::named_key(
                ui::winit::keyboard::NamedKey::Escape,
            ));
            app.harness().relayout();
            app.harness()
                .touch(9, ui::winit::event::TouchPhase::Started, x, y);
            app.harness()
                .touch(9, ui::winit::event::TouchPhase::Moved, x - 12.0, y);
            app.harness()
                .touch(9, ui::winit::event::TouchPhase::Moved, x - row[2] * 0.4, y);
            app.harness().relayout();
            app.harness().render_png(&name("swipe"));
            app.harness().touch(
                9,
                ui::winit::event::TouchPhase::Cancelled,
                x - row[2] * 0.4,
                y,
            );
            settle(&mut app);
            // The composer's drawer: opened by its trigger, narrowed while typing, saying
            // nothing matches, and the picked mention sent.
            let (cx, cy) = composer(app.harness());
            app.harness().click_at(cx, cy);
            app.harness().type_text("thanks @");
            settle(&mut app);
            app.harness().render_png(&name("drawer-empty"));
            app.harness().type_text("l");
            settle(&mut app);
            app.harness().render_png(&name("drawer-typing"));
            app.harness().key("Enter");
            settle(&mut app);
            app.harness().render_png(&name("drawer-picked"));
            app.harness().type_text("and @zz");
            settle(&mut app);
            app.harness().render_png(&name("drawer-no-match"));
            app.harness().key("Escape");
            let (bx, by) = send_button(app.harness());
            app.harness().click_at(bx, by);
            settle(&mut app);
            app.harness().render_png(&name("sent-reference"));
        }
    }
}
