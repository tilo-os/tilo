#![allow(
    clippy::exit,
    reason = "a capture scenario reports a window with two bars as its exit status"
)]

//! A window of its own, outside a Tilo session: one bar across the top, as
//! the native runner draws it on a desktop that leaves the frame to the app
//! (`xdg-decoration`: client-side). The bar is the app's own header row —
//! its location, its actions, its search — and the window's minimize,
//! maximize and close stand in their pill at its trailing end. The app's
//! frame starts under that one bar, never under a second title row.
//! Run:  `cargo run -p ui-examples --example ui-shots -- window_of_its_own [OUT_DIR]`

use std::path::{Path, PathBuf};

use tilo_ui::TextRole;
use tilo_ui::headless::FrameHarness;
use tilo_ui::icons::Icon;
use tilo_ui::state::State;
use tilo_ui::theme::Theme;
use tilo_ui::widget::tabs::TOOLBAR_ROW_HEIGHT;
use tilo_ui::widget::{
    Button, Column, ContentKind, Element, NavItem, NavList, NavSection, NavShell, Text, Widget,
};

fn icon_button(id: &str, icon: Icon, name: &str) -> Element {
    Box::new(Button::icon_only(id, icon, name))
}

/// The Tasks app's shape: its lists beside the inbox.
fn tasks() -> NavShell {
    let sidebar = NavList::new(vec![NavSection::untitled(
        "lists",
        vec![
            NavItem::new("inbox", "Inbox")
                .icon(Icon::Inbox)
                .live_count(4),
            NavItem::new("upcoming", "Upcoming").icon(Icon::Calendar),
            NavItem::new("anytime", "Anytime").icon(Icon::Layers),
            NavItem::new("logbook", "Logbook").icon(Icon::CircleCheck),
        ],
    )])
    .selected("inbox");
    let tasks = Column::new()
        .push(Box::new(
            Text::new("Design review").with_role(TextRole::Heading),
        ))
        .push(Box::new(Text::new("Thursday, 14:00 · with Ada and Sam")))
        .push(Box::new(Text::new("Renew the certificate")))
        .push(Box::new(Text::new("Reply to the printer quote")))
        .push(Box::new(Text::new("Book the offsite room")));
    NavShell::new()
        .content_kind(ContentKind::Reading)
        .sidebar(Box::new(sidebar))
        .content(Box::new(tasks))
        .sidebar_header("Tasks", Vec::new())
        .content_header(
            "Inbox",
            vec![
                icon_button("new-task", Icon::Plus, "New task"),
                icon_button("filter", Icon::List, "Filter"),
            ],
        )
        .content_count(4)
}

/// One window of its own at `size`: the app's frame under its one bar, the
/// window's controls in the bar's pill.
fn shoot(theme: &Theme, size: (u32, u32), out: &Path) {
    let mut harness = FrameHarness::for_root(State::new(), tasks(), size, theme.clone());
    harness.host_window_slots();
    harness.host_window_controls();
    for _ in 0..2 {
        harness.root_mut::<NavShell>().snap();
        if let Err(error) = harness.relayout_at(size) {
            eprintln!("window_of_its_own: layout failed: {error}");
            std::process::exit(1);
        }
    }
    // One bar: the app's frame starts under the row, not under a title row
    // and then the row.
    let frame = Widget::get_rect(harness.root::<NavShell>());
    if (frame.y0 - TOOLBAR_ROW_HEIGHT).abs() > 0.5 {
        eprintln!(
            "window_of_its_own: the app starts at {} under a bar {TOOLBAR_ROW_HEIGHT} tall: a second bar stands over it",
            frame.y0
        );
        std::process::exit(1);
    }
    let snapshot = harness.inspection();
    let Some(row) = snapshot
        .root
        .children
        .iter()
        .find(|node| node.path == tilo_ui::headless::WINDOW_SLOTS)
    else {
        eprintln!("window_of_its_own: the window has no bar");
        std::process::exit(1);
    };
    let bar = row.rect[3] - row.rect[1];
    if (bar - TOOLBAR_ROW_HEIGHT).abs() > 0.5 {
        eprintln!("window_of_its_own: the bar is {bar} tall, not one row");
        std::process::exit(1);
    }
    println!("one bar: {:?} over the app at y={}", row.rect, frame.y0);
    harness.render_png(out);
    println!("wrote {}", out.display());
}

pub fn run(args: &[String]) {
    let dir = args
        .first()
        .cloned()
        .unwrap_or_else(|| "target/look/window-of-its-own".to_string());
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("window_of_its_own: cannot create {dir}: {error}");
        std::process::exit(1);
    }
    let dir = PathBuf::from(dir);
    for size in [(1280, 800), (390, 844)] {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let out = dir.join(format!("inbox-{}-{name}.png", size.0));
            shoot(&theme, size, &out);
        }
    }
}
