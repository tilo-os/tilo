//! The Board package as the UI host runs it with cards on it: every control of the window,
//! each card's included, is bound to an action the window publishes, so the window's actions
//! reach agents and `tilo-control actions`; and a card's context menu offers its actions.

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

/// The board with its release scenario staged: three columns with cards in each.
fn staged() -> TestApp<TlApp> {
    support::installed("board");
    let mut app = TestApp::<TlApp>::capture_at(Some("a release board"), DESKTOP)
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

/// Every resource `resources` publishes, with the ones under them.
fn flattened(resources: &[ui::tilo_actions::ResourceSpec]) -> Vec<&ui::tilo_actions::ResourceSpec> {
    resources
        .iter()
        .flat_map(|resource| std::iter::once(resource).chain(flattened(&resource.children)))
        .collect()
}

/// The id the package's ids lock gives `name`.
fn id_of(name: &str) -> String {
    include_str!("../../tl/board/app.ids")
        .lines()
        .find_map(|line| {
            let (key, id) = line.split_once(" = ")?;
            (key.trim() == name).then(|| id.trim().to_owned())
        })
        .unwrap_or_else(|| panic!("the ids lock names `{name}`"))
}

/// The controls bound to an action the window does not publish: the live host refuses to
/// publish any action while one is.
fn stale_bindings(app: &mut TestApp<TlApp>) -> Vec<String> {
    app.harness()
        .native_app_audit()
        .issues
        .into_iter()
        .filter(|issue| issue.code == "stale-action-binding")
        .map(|issue| format!("{}: {}", issue.control, issue.message))
        .collect()
}

/// Where the card titled `title` is drawn, `[x, y, width, height]`.
fn card_rect(app: &mut TestApp<TlApp>, title: &str) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.iter()
        .find(|node| {
            node.widget == "Card"
                && node
                    .semantics
                    .as_ref()
                    .is_some_and(|access| access.name.as_deref() == Some(title))
        })
        .map_or_else(|| panic!("no card `{title}`"), |node| node.rect)
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

fn right_click(app: &mut TestApp<TlApp>, rect: [f64; 4]) {
    let (x, y) = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
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
fn a_board_holding_cards_publishes_its_actions_and_each_card_its_own_control() {
    let mut app = staged();
    assert_eq!(
        stale_bindings(&mut app),
        Vec::<String>::new(),
        "every control on a board with cards is bound to a published action"
    );
    let resources = app.harness().resources();
    let all = flattened(&resources);
    let actions: Vec<&str> = all
        .iter()
        .flat_map(|resource| resource.actions.iter().map(|action| action.id.as_str()))
        .collect();
    for action in ["new_card", "move_to", "delete_card"] {
        assert!(
            actions.contains(&id_of(action).as_str()),
            "the board publishes `{action}`: {actions:?}"
        );
    }
    // A card opens its page when pressed: that press is a control of the window's own, which
    // an agent reaches as a person does.
    assert!(
        all.iter().any(|resource| resource.kind == "tilo.control"
            && resource.label == "Write the release notes"),
        "the card's press is published: {:?}",
        all.iter().map(|r| (&r.kind, &r.label)).collect::<Vec<_>>()
    );
}

#[test]
fn a_cards_menu_offers_its_moves_and_actions_and_keeps_every_binding_published() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged();
    let card = card_rect(&mut app, "Write the release notes");
    right_click(&mut app, card);
    let items = menu_items(&mut app);
    for wanted in ["Move to Doing", "Move to Done", "Delete"] {
        assert!(
            items.iter().any(|item| item == wanted),
            "the card's menu offers {wanted}: {items:?}"
        );
    }
    assert!(
        !items.iter().any(|item| item == "Move to To do"),
        "a card is not offered the column it is in: {items:?}"
    );
    assert_eq!(
        stale_bindings(&mut app),
        Vec::<String>::new(),
        "an open menu's rows are published too"
    );
    let rows: Vec<(String, String)> = flattened(&app.harness().resources())
        .into_iter()
        .filter(|resource| resource.id.as_str().starts_with("tilo.control:menu."))
        .map(|resource| (resource.id.as_str().to_owned(), resource.label.clone()))
        .collect();
    assert!(
        rows.iter().any(|(_, label)| label == "Delete"),
        "an agent reaches the menu's rows: {rows:?}"
    );
    // An agent choosing a row is the person's choice: the card moves.
    let (done, _) = rows
        .iter()
        .find(|(_, label)| label == "Move to Done")
        .expect("Move to Done is published");
    app.harness()
        .invoke_published(done, "invoke", &serde_json::Map::new())
        .expect("the row is reachable");
    settle(&mut app);
    let card = card_rect(&mut app, "Write the release notes");
    right_click(&mut app, card);
    let items = menu_items(&mut app);
    assert!(
        items.iter().any(|item| item == "Move to To do")
            && !items.iter().any(|item| item == "Move to Done"),
        "the card is in Done now: {items:?}"
    );
}
