//! The interaction layer of a conversation as the UI host runs it, over the `interaction`
//! package: a message's own context menu with the actions it did not place added, and its
//! swipe, each run in the window without asking the app what to show. What a hover repaints is
//! counted on the toolbar itself (`ui::widget::HoverToolbar`'s tests).
//!
//! With `TILO_INTERACTION_SHOTS=<dir>` the test `shots` writes each state at desktop and phone
//! sizes, light and dark.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;
use ui::headless::InspectionNode;

const DESKTOP: (u32, u32) = (1280, 800);
const PHONE: (u32, u32) = (420, 860);

fn staged(viewport: (u32, u32)) -> TestApp<TlApp> {
    support::installed("interaction");
    let mut app = TestApp::<TlApp>::capture_at(Some("a conversation"), viewport)
        .expect("the scenario stages");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    for _ in 0..8 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

fn walk<'a>(node: &'a InspectionNode, out: &mut Vec<&'a InspectionNode>) {
    out.push(node);
    for child in &node.children {
        walk(child, out);
    }
}

/// The rects of the widgets named `widget`, in tree order.
fn rects_of(app: &mut TestApp<TlApp>, widget: &str) -> Vec<[f64; 4]> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .filter(|node| node.widget == widget)
        .map(|node| node.rect)
        .collect()
}

/// The names of the open menu's items, in order.
fn menu_items(app: &mut TestApp<TlApp>) -> Vec<String> {
    fn items(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        if node.role == ui::a11y::Role::MenuItem
            && let Some(name) = &node.name
        {
            out.push(name.clone());
        }
        for child in &node.children {
            items(child, out);
        }
    }
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let mut out = Vec::new();
    for node in all.into_iter().filter(|node| node.widget == "ContextMenu") {
        if let Some(semantics) = &node.semantics
            && semantics.state.expanded == Some(true)
        {
            items(semantics, &mut out);
        }
    }
    out
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
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
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
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.iter()
        .filter_map(|node| node.semantics.as_ref().and_then(|a| a.name.clone()))
        .collect()
}

/// Whether some text in the window reads `text`.
fn shows(app: &mut TestApp<TlApp>, text: &str) -> bool {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.iter().any(|node| {
        node.semantics
            .as_ref()
            .is_some_and(|access| access.name.as_deref() == Some(text))
    })
}

fn center(rect: [f64; 4]) -> (f64, f64) {
    (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0)
}

/// The last message's row.
fn last_message(app: &mut TestApp<TlApp>) -> [f64; 4] {
    if let Some(rect) = rects_of(app, "SwipeRow").last() {
        return *rect;
    }
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let names: Vec<String> = all.iter().map(|node| node.widget.clone()).collect();
    let said: Vec<String> = all
        .iter()
        .filter_map(|node| node.semantics.as_ref().and_then(|a| a.name.clone()))
        .collect();
    panic!("each message swipes; the window holds {names:?}, saying {said:?}");
}

fn right_click(app: &mut TestApp<TlApp>, (x, y): (f64, f64)) {
    let harness = app.harness();
    harness.send(&ui::synthetic_input::cursor_moved(x, y));
    harness.send(&ui::synthetic_input::mouse_input(
        ui::winit::event::ElementState::Pressed,
        ui::winit::event::MouseButton::Right,
    ));
    harness.send(&ui::synthetic_input::mouse_input(
        ui::winit::event::ElementState::Released,
        ui::winit::event::MouseButton::Right,
    ));
    harness.relayout();
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
fn composer(app: &mut TestApp<TlApp>) -> (f64, f64) {
    let inputs = rects_of(app, "RichInput");
    center(*inputs.last().expect("the transcript has its composer"))
}

/// The room's people, as its roster names them to every window in it.
fn in_the_room(app: &mut TestApp<TlApp>) {
    let mut presence = ui::presence::PresenceCtx::default();
    for (id, name) in [("u_mia", "Mia"), ("u_leo", "Leo")] {
        presence
            .names
            .insert(ui::permissions::UserId(id.to_owned()), name.to_owned());
    }
    app.harness().set_presence_context(presence);
    app.harness().relayout();
}

/// The composer's send button: the button on the composer's line.
fn send_button(app: &mut TestApp<TlApp>) -> (f64, f64) {
    let (_, y) = composer(app);
    let buttons = rects_of(app, "Button");
    let line = buttons
        .iter()
        .find(|rect| (center(**rect).1 - y).abs() < rect[3])
        .copied()
        .expect("the composer has its send button");
    center(line)
}

/// The open completion drawer's rows, as assistive technology reads them.
fn drawer_rows(app: &mut TestApp<TlApp>) -> Vec<String> {
    fn find(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        if node.role == ui::a11y::Role::ListBox {
            for row in &node.children {
                if let Some(name) = &row.name {
                    out.push(name.clone());
                }
            }
            return;
        }
        for child in &node.children {
            find(child, out);
        }
    }
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let mut out = Vec::new();
    for node in all.into_iter().filter(|node| node.widget == "RichInput") {
        if let Some(access) = &node.semantics {
            find(access, &mut out);
        }
    }
    out
}

/// Whether `access` or anything under it says `text` in its name or value.
fn says(access: &ui::a11y::AccessNode, text: &str) -> bool {
    access
        .name
        .as_deref()
        .is_some_and(|name| name.contains(text))
        || access
            .value
            .as_deref()
            .is_some_and(|value| value.contains(text))
        || access.children.iter().any(|child| says(child, text))
}

/// Everything `access` and what is under it say, in order.
fn texts(access: &ui::a11y::AccessNode, out: &mut String) {
    for part in [&access.name, &access.value].into_iter().flatten() {
        out.push_str(part);
        out.push(' ');
    }
    for child in &access.children {
        texts(child, out);
    }
}

/// Whether some node in the window says `text` in its name or value.
fn reads(app: &mut TestApp<TlApp>, text: &str) -> bool {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.iter().any(|node| {
        node.semantics
            .as_ref()
            .is_some_and(|access| says(access, text))
    })
}

/// What the composer holds.
fn draft(app: &mut TestApp<TlApp>) -> String {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.iter()
        .rev()
        .find(|node| node.widget == "RichInput")
        .and_then(|node| node.semantics.as_ref())
        .and_then(|access| access.value.clone())
        .unwrap_or_default()
}

/// The room's membership as its policy records it: the person at the window owns it, Mia
/// edits, Leo views, and the agent `planner` was admitted. Zoe is in the room's roster, but no
/// member.
fn members_of_the_room(app: &mut TestApp<TlApp>) {
    use ui::permissions::policy::{self, Role, Subject, roles::AgentPreset};
    let me = app.permissions().standing().person.clone();
    let room = app.permissions().standing().room().to_owned();
    let mut workspace = policy::RoomPolicy::new(&room, &me);
    workspace.members.insert("u_mia".to_owned(), Role::Editor);
    workspace.members.insert("u_leo".to_owned(), Role::Viewer);
    workspace.grants.extend(AgentPreset::Contributor.grants(
        &room,
        "planner",
        &Subject::person(&me),
    ));
    let standing = policy::Standing::person(workspace, me);
    app.set_permissions(ui::permissions::PermissionContext::from_standing(
        &room, standing,
    ));
    let mut presence = ui::presence::PresenceCtx::default();
    // Mia is here from her laptop, which the room's host paired with her person; Leo is named
    // as himself.
    for (id, name) in [
        ("mia-laptop", "Mia"),
        ("u_leo", "Leo"),
        ("u_zoe", "Zoe"),
        ("agent:planner", "Planner"),
    ] {
        presence
            .names
            .insert(ui::permissions::UserId(id.to_owned()), name.to_owned());
    }
    presence.set_participant_kind(
        ui::permissions::UserId("agent:planner".to_owned()),
        ui::presence::ParticipantKind::Agent,
    );
    presence.set_persons([(
        ui::permissions::UserId("mia-laptop".to_owned()),
        ui::permissions::UserId("u_mia".to_owned()),
    )]);
    app.harness().set_presence_context(presence);
    settle(app);
}

#[test]
fn the_composers_drawer_offers_the_members_of_the_conversation() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    members_of_the_room(&mut app);
    let (x, y) = composer(&mut app);
    app.harness().click_at(x, y);
    app.harness().type_text("@");
    app.harness().relayout();
    let people = drawer_rows(&mut app);
    // Who spoke last in the conversation first (its scenario's people), then its members.
    let members = people.iter().position(|row| row == "Members").map_or_else(
        || panic!("the members have their header: {people:?}"),
        |at| &people[at + 1..],
    );
    assert!(
        people
            .first()
            .is_some_and(|row| row == "In this conversation"),
        "{people:?}"
    );
    for member in ["Mia", "Leo"] {
        assert!(
            members.iter().any(|row| row.contains(member)),
            "{member} is a member and is offered: {people:?}"
        );
    }
    assert!(
        !people.iter().any(|row| row.contains("Zoe")),
        "who is in the roster but no member is not offered: {people:?}"
    );
    // The drawer shows its first rows; typing finds the rest.
    app.harness().type_text("pla");
    app.harness().relayout();
    let found = drawer_rows(&mut app);
    assert!(
        found.len() == 1 && found[0].contains("Planner"),
        "the admitted agent is a member too: {found:?}"
    );
    app.harness().key("Enter");
    settle(&mut app);
    // An agent's mention reads as one: `@Planner · Agent`.
    let draft = draft(&mut app);
    assert!(draft.starts_with("@Planner"), "{draft:?}");
}

#[test]
fn the_composers_drawer_offers_the_rooms_people_and_writes_a_mention_that_is_sent() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    in_the_room(&mut app);
    let (x, y) = composer(&mut app);
    app.harness().click_at(x, y);
    app.harness().type_text("thanks @");
    app.harness().relayout();
    let people = drawer_rows(&mut app);
    assert!(
        people.iter().any(|row| row == "Mia") && people.iter().any(|row| row == "Leo"),
        "the trigger opens the room's people, with nothing declared by the view: {people:?}"
    );
    app.harness().type_text("mi");
    // No settling: the drawer narrows in the window.
    app.harness().relayout();
    assert_eq!(drawer_rows(&mut app), ["Mia"]);
    app.harness().key("Enter");
    settle(&mut app);
    assert_eq!(draft(&mut app), "thanks @Mia ");
    assert!(
        drawer_rows(&mut app).is_empty(),
        "the pick closed the drawer"
    );
    app.harness().type_text("for the build");
    settle(&mut app);
    let (bx, by) = send_button(&mut app);
    app.harness().click_at(bx, by);
    settle(&mut app);
    assert!(
        reads(&mut app, "for the build"),
        "the message was sent: {:?}",
        all_names(&mut app)
    );
    let sent = {
        let snapshot = app.harness().inspection();
        let mut all = Vec::new();
        walk(&snapshot.root, &mut all);
        all.iter()
            .rev()
            .find(|node| node.widget == "BlockEditor")
            .and_then(|node| node.semantics.clone())
            .expect("the sent message shows its rich text")
    };
    let mut said = String::new();
    texts(&sent, &mut said);
    assert!(
        said.contains("Mia") && said.contains("for the build"),
        "the sent message holds its mention, read as the person: {said:?}"
    );
    assert_eq!(draft(&mut app), "", "and the draft cleared");
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
            in_the_room(&mut app);
            let (cx, cy) = composer(&mut app);
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
            let (bx, by) = send_button(&mut app);
            app.harness().click_at(bx, by);
            settle(&mut app);
            app.harness().render_png(&name("sent-reference"));
        }
    }
}
