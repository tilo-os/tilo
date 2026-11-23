//! The Board package as the UI host runs it with cards on it: every control of the window,
//! each card's included, is bound to an action the window publishes, so the window's actions
//! reach agents and `tilo-control actions`; and a card's context menu offers its actions.

use crate::change_marks::{flattened, menu_items, right_click};
use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;

const DESKTOP: (u32, u32) = (1280, 800);

/// The board with its release scenario staged: three columns with cards in each.
fn staged() -> TestApp<TlApp> {
    crate::support::installed("board");
    let mut app = TestApp::<TlApp>::capture_at(Some("a release board"), DESKTOP)
        .expect("the scenario stages");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    crate::change_marks::captured(app, 8);
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

/// Where the card titled `title` is drawn, `[x, y, width, height]`: a card is its row, which
/// says what the card stands for.
fn card_rect(app: &mut TestApp<TlApp>, title: &str) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let all = snapshot.root.nodes();
    all.iter()
        .find(|node| {
            node.widget == "ListRow"
                && node
                    .semantics
                    .as_ref()
                    .is_some_and(|access| access.answers_to(title))
        })
        .map_or_else(|| panic!("no card `{title}`"), |node| node.rect)
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
            actions.contains(&action),
            "the board publishes `{action}`: {actions:?}"
        );
    }
}

#[test]
fn a_cards_menu_offers_its_moves_and_actions_and_keeps_every_binding_published() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged();
    let card = card_rect(&mut app, "Write the release notes");
    right_click(&mut app, (card[0] + card[2] / 2.0, card[1] + card[3] / 2.0));
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
    // An agent choosing a row presses it as a person's assistive tool does: the card moves.
    app.harness()
        .perform_named("Move to Done", &ui::a11y::Action::Activate)
        .expect("the row is reachable");
    settle(&mut app);
    let card = card_rect(&mut app, "Write the release notes");
    right_click(&mut app, (card[0] + card[2] / 2.0, card[1] + card[3] / 2.0));
    let items = menu_items(&mut app);
    assert!(
        items.iter().any(|item| item == "Move to To do")
            && !items.iter().any(|item| item == "Move to Done"),
        "the card is in Done now: {items:?}"
    );
}
