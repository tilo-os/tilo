#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashSet;

use poker::game::{BUY_IN, Street};
use poker::{App, Table};
use ui::prelude::{Invoke, Replica, TestApp};

/// Everyone at the table, each with their own replica of it: an action is
/// applied by its player's replica and synced to the others, as the room
/// delivers it.
struct Room {
    replicas: Vec<Replica<Table>>,
    names: Vec<&'static str>,
    now: i64,
}

impl Room {
    fn new(names: &[&'static str]) -> Self {
        let mut first = Replica::<Table>::new();
        let replicas = names
            .iter()
            .map(|name| first.fork().as_author(name))
            .collect();
        Self {
            replicas,
            names: names.to_vec(),
            now: 1_000,
        }
    }

    fn at(&self, name: &str) -> usize {
        self.names
            .iter()
            .position(|n| *n == name)
            .expect("a player")
    }

    /// `name` invokes `invoke`; every replica then has it.
    fn act(&mut self, name: &str, invoke: Invoke<Table>) -> Result<(), String> {
        self.now += 1_000;
        let at = self.at(name);
        let replica = &mut self.replicas[at];
        replica.set_now_ms(self.now);
        replica
            .apply(invoke)
            .map_err(|error| format!("{error:?}"))?;
        self.sync();
        Ok(())
    }

    fn sync(&mut self) {
        for i in 1..self.replicas.len() {
            let (head, tail) = self.replicas.split_at_mut(i);
            head[0].sync_with(&mut tail[0]).expect("sync");
        }
        for i in (1..self.replicas.len()).rev() {
            let (head, tail) = self.replicas.split_at_mut(i);
            head[0].sync_with(&mut tail[0]).expect("sync");
        }
    }

    fn table(&self) -> Table {
        let table = self.replicas[0].state().expect("table");
        for replica in &self.replicas[1..] {
            assert_eq!(replica.state().expect("table"), table, "replicas agree");
        }
        table
    }

    fn seat_all(&mut self) {
        for name in self.names.clone() {
            self.act(name, Table::SIT.with(1)).expect("sit");
        }
    }

    /// Whoever is to act checks or calls, until the hand is over.
    fn call_down(&mut self) {
        for _ in 0..64 {
            let table = self.table();
            let Some(actor) = table.hand.actor().map(|p| p.player.clone()) else {
                return;
            };
            let name = self
                .names
                .iter()
                .copied()
                .find(|n| *n == actor)
                .expect("a player");
            self.act(name, Table::CALL.invoke()).expect("call");
        }
        panic!("the hand never ended");
    }

    fn total_chips(&self) -> i64 {
        let table = self.table();
        table.players.keys().map(|id| table.chips(id)).sum::<i64>()
            + if table.hand.street.in_play() {
                table.hand.pot()
            } else {
                0
            }
    }
}

#[test]
fn people_joining_get_different_seats() {
    let mut room = Room::new(&["ada", "grace", "alan"]);
    // All three ask for seat 1 before hearing of each other.
    for at in 0..3 {
        room.replicas[at].set_now_ms(100 + i64::try_from(at).unwrap());
        room.replicas[at].apply(Table::SIT.with(1)).expect("sit");
    }
    room.sync();
    let seating = room.table().seating();
    let seated: Vec<&str> = seating.iter().flatten().map(String::as_str).collect();
    assert_eq!(
        seated,
        vec!["ada", "grace", "alan"],
        "each in a seat of their own"
    );
}

#[test]
fn a_hand_plays_to_the_showdown_and_keeps_every_chip() {
    let mut room = Room::new(&["ada", "grace", "alan"]);
    room.seat_all();
    assert_eq!(room.table().dealer(), Some("ada"));
    assert!(
        room.act("grace", Table::DEAL.invoke()).is_err(),
        "only the dealer deals"
    );
    room.act("ada", Table::DEAL.invoke()).expect("deal");

    let table = room.table();
    assert_eq!(table.hand.street, Street::Preflop);
    let dealt: Vec<u8> = table
        .hand
        .players
        .iter()
        .flat_map(|p| p.cards.clone())
        .chain(table.hand.deck.clone())
        .collect();
    assert_eq!(
        dealt.iter().collect::<HashSet<_>>().len(),
        52,
        "one deck, no card twice"
    );
    assert_eq!(room.total_chips(), 3 * BUY_IN);

    room.call_down();
    let table = room.table();
    assert_eq!(table.hand.street, Street::Done);
    assert_eq!(table.hand.board.len(), 5);
    assert!(!table.hand.awards.is_empty(), "someone won the pot");
    assert!(
        table.hand.players.iter().all(|p| p.shown),
        "a showdown shows every live hand"
    );
    assert_eq!(room.total_chips(), 3 * BUY_IN, "no chip made or lost");
}

#[test]
fn only_the_player_to_act_may_act() {
    let mut room = Room::new(&["ada", "grace", "alan"]);
    room.seat_all();
    room.act("ada", Table::DEAL.invoke()).expect("deal");
    let actor = room
        .table()
        .hand
        .actor()
        .expect("someone acts")
        .player
        .clone();
    let other = room.names.iter().copied().find(|n| *n != actor).unwrap();
    assert!(room.act(other, Table::FOLD.invoke()).is_err());
    assert!(room.act(other, Table::CALL.invoke()).is_err());
}

#[test]
fn folding_to_a_raise_gives_the_raiser_the_pot() {
    let mut room = Room::new(&["ada", "grace"]);
    room.seat_all();
    room.act("ada", Table::DEAL.invoke()).expect("deal");
    let first = room.table().hand.actor().unwrap().player.clone();
    let first = room.names.iter().copied().find(|n| *n == first).unwrap();
    room.act(first, Table::RAISE.with(100.0)).expect("raise");
    let second = room.table().hand.actor().unwrap().player.clone();
    let second = room.names.iter().copied().find(|n| *n == second).unwrap();
    room.act(second, Table::FOLD.invoke()).expect("fold");
    let table = room.table();
    assert_eq!(table.hand.street, Street::Done);
    assert_eq!(table.hand.awards.len(), 1);
    assert_eq!(table.hand.awards[0].player, first);
    // The folder loses only their blind.
    assert!(table.chips(second) < BUY_IN && table.chips(second) >= BUY_IN - 10);
    assert_eq!(table.chips(first) + table.chips(second), 2 * BUY_IN);
}

#[test]
fn all_in_runs_the_board_out() {
    let mut room = Room::new(&["ada", "grace"]);
    room.seat_all();
    room.act("ada", Table::DEAL.invoke()).expect("deal");
    let first = room.table().hand.actor().unwrap().player.clone();
    let first = room.names.iter().copied().find(|n| *n == first).unwrap();
    room.act(first, Table::ALL_IN.invoke()).expect("all in");
    let second = room.table().hand.actor().unwrap().player.clone();
    let second = room.names.iter().copied().find(|n| *n == second).unwrap();
    room.act(second, Table::CALL.invoke()).expect("call");
    let table = room.table();
    assert_eq!(table.hand.street, Street::Done);
    assert_eq!(table.hand.board.len(), 5);
    assert_eq!(table.chips("ada") + table.chips("grace"), 2 * BUY_IN);
}

#[test]
fn leaving_folds_your_hand() {
    let mut room = Room::new(&["ada", "grace", "alan"]);
    room.seat_all();
    room.act("ada", Table::DEAL.invoke()).expect("deal");
    room.act("alan", Table::LEAVE.invoke()).expect("leave");
    let table = room.table();
    assert!(!table.players.contains_key("alan"));
    let alan = table
        .hand
        .players
        .iter()
        .find(|p| p.player == "alan")
        .unwrap();
    assert!(alan.folded);
    assert!(table.hand.street.in_play(), "the others play on");
}

/// Opening the table seats nobody: whoever opens it watches, and takes a
/// seat with the "Take a seat" button, through the published action, once.
#[test]
fn opening_the_table_seats_nobody_until_they_take_a_seat() {
    let app = TestApp::<App>::for_person("ada").expect("the app opens");
    assert!(
        app.state().players.is_empty(),
        "watching is where you start"
    );
    let mut app = app.reopen().expect("the app reopens");
    assert!(app.state().players.is_empty(), "nor does opening it again");

    take_a_seat(&mut app);
    let table = app.state();
    assert_eq!(table.seat_of("ada"), Some(0));
    assert_eq!(table.chips("ada"), BUY_IN);
    assert!(app.invoke(Table::SIT.with(3)).is_err(), "one seat each");
    assert!(
        app.harness().a11y_node_named("Take a seat").is_none(),
        "a seated player is offered no other seat"
    );
}

/// A seat is chosen from the table everyone sees: until the room's table
/// has arrived, the window shows it arriving and offers no seat.
#[test]
fn taking_a_seat_waits_for_the_table_to_arrive() {
    let told = |state| {
        Some(ui::room_sync::RoomSync {
            state,
            workspace: "Lounge".into(),
            writable: None,
        })
    };
    ui::room_sync::set(told(ui::room_sync::RoomSyncState::Connecting));
    let mut app = TestApp::<App>::for_person("ada").expect("the app opens");
    let harness = app.harness();
    assert!(
        harness.a11y_node_named("Loading").is_some(),
        "the table is on its way"
    );
    assert!(
        harness.a11y_node_named("Take a seat").is_none(),
        "no seat is offered at a table that has not arrived"
    );

    ui::room_sync::set(told(ui::room_sync::RoomSyncState::Synced));
    let _ = app.harness().poll_frame();
    assert!(take_a_seat_enabled(&mut app), "the table has arrived");
    take_a_seat(&mut app);
    assert_eq!(app.state().seat_of("ada"), Some(0));
    ui::room_sync::set(None);
}

fn take_a_seat_enabled(app: &mut TestApp<App>) -> bool {
    let harness = app.harness();
    let button = harness
        .a11y_node_named("Take a seat")
        .expect("someone watching is offered a seat");
    let (_, node) = harness
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == button)
        .expect("the button is in the tree");
    !node.is_disabled()
}

fn take_a_seat(app: &mut TestApp<App>) {
    let harness = app.harness();
    let button = harness
        .a11y_node_named("Take a seat")
        .expect("someone watching is offered a seat");
    harness
        .perform_a11y_action(button, &ui::a11y::Action::Activate)
        .expect("the button takes the seat");
    let _ = harness.poll_frame();
}

/// Permissions apply to every action: a viewer cannot take a seat.
#[test]
fn a_viewer_cannot_sit_down() {
    let mut app = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    assert!(app.invoke(Table::SIT.with(1)).is_err());
    assert!(app.state().players.is_empty());
}

/// The app satisfies the Tilo-native contract: every control that can cause
/// an effect is bound to a published action.
#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}

/// The table is the window bar's location; the window draws no title row
/// of its own, and the blinds stand with the pot.
#[test]
fn the_window_bar_names_the_table_and_the_window_draws_no_title() {
    let mut app = TestApp::<App>::for_person("ada").expect("the app opens");
    app.harness().present_in_window_bar();
    let _ = app.harness().poll_frame();
    let bar = ui::window_toolbar::declared().expect("the table declares its slots");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Poker table")
    );
    let harness = app.harness();
    assert!(
        harness.a11y_node_named("Poker table").is_none(),
        "the window draws no title of its own"
    );
    assert!(
        harness.a11y_node_named("Blinds 5 and 10").is_some(),
        "the blinds stand with the pot"
    );
}

/// A table with `ada` (whose window this is) and `grace` seated, a hand
/// dealt, and the turn brought to whoever is not `ada`.
fn waiting_on_grace() -> TestApp<App> {
    let mut app = TestApp::<App>::for_person("ada").expect("the app opens");
    take_a_seat(&mut app);
    let grace = ui::tilo_actions::ActionInvoker::actor("grace");
    app.call_by(
        grace,
        "so.tilo.poker.table",
        "sit",
        ui::serde_json::json!({ "seat": 2 }),
    )
    .expect("grace sits down");
    let dealer = app.state().dealer().map(str::to_owned).expect("a dealer");
    let dealer = ui::tilo_actions::ActionInvoker::actor(&dealer);
    app.call_by(
        dealer,
        "so.tilo.poker.table",
        "deal",
        ui::serde_json::json!({}),
    )
    .expect("the hand is dealt");
    // Whoever the button put first acts first: pass the turn until it is
    // Grace's, so the person watching is the one offered the fold.
    for _ in 0..2 {
        let Some(actor) = app.state().hand.actor().map(|p| p.player.clone()) else {
            break;
        };
        if actor != "ada" {
            break;
        }
        let ada = ui::tilo_actions::ActionInvoker::actor("ada");
        app.call_by(
            ada,
            "so.tilo.poker.table",
            "call",
            ui::serde_json::json!({}),
        )
        .expect("the turn is passed");
    }
    assert_eq!(
        app.state().hand.actor().map(|p| p.player.as_str()),
        Some("grace"),
        "Grace is to act, and the window is Ada's"
    );
    app.settle();
    app
}

/// The button called `name`, and whether it can act.
fn button(app: &mut TestApp<App>, name: &str) -> (ui::accesskit::NodeId, bool) {
    let harness = app.harness();
    let id = harness
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("the window offers {name:?}"));
    let (_, node) = harness
        .a11y_nodes()
        .into_iter()
        .find(|(node_id, _)| *node_id == id)
        .expect("the button is in the tree");
    (id, !node.is_disabled())
}

/// Run `frame` at `offset` into the turn that began at `since_ms`: the wall
/// clock the countdown and the table's rule both read, and the frame clock
/// the host wakes on.
fn into_turn(
    since_ms: i64,
    start: ui::time::Instant,
    offset: ui::time::Duration,
    frame: impl FnOnce(),
) {
    let since = u64::try_from(since_ms).unwrap_or(0);
    let wall = ui::time::UNIX_EPOCH + ui::time::Duration::from_millis(since) + offset;
    ui::time::testing::with_wall(wall, || {
        ui::time::testing::with_monotonic_time(start + offset, frame);
    });
}

/// The fold for a slow player is not refused for thirty seconds behind a
/// button that looks ready: it waits disabled, says how many seconds are
/// left, and opens when the table's rule does.
#[test]
fn folding_for_a_slow_player_counts_down_and_opens() {
    use ui::time::{Duration, Instant};

    let mut app = waiting_on_grace();
    let since = app.state().hand.since_ms;
    let start = Instant::now();
    assert!(
        app.harness()
            .a11y_node_named("Fold for grace in 30s")
            .is_some(),
        "the caption says why the fold is not offered yet"
    );
    let (_, open) = button(&mut app, "Fold for the slow player");
    assert!(!open, "the fold is not open yet");

    // Nineteen seconds of the turn gone: the caption has counted down.
    into_turn(since, start, Duration::from_secs(19), || {
        let _ = app.harness().poll_frame();
    });
    assert!(
        app.harness()
            .a11y_node_named("Fold for grace in 11s")
            .is_some(),
        "the countdown follows the clock"
    );
    let (_, open) = button(&mut app, "Fold for the slow player");
    assert!(!open, "still closed at 11 seconds left");

    // The table's own instant passed: the button acts, and the caption is
    // gone because there is nothing left to explain.
    let mut id = None;
    into_turn(since, start, Duration::from_secs(31), || {
        let _ = app.harness().poll_frame();
        let (button, open) = button(&mut app, "Fold for the slow player");
        assert!(open, "the fold is open once the player took too long");
        id = Some(button);
    });
    assert!(
        app.harness()
            .a11y_node_named("Fold for grace in 0s")
            .is_none(),
        "an open move says nothing about waiting"
    );
    let id = id.expect("the fold is offered");
    into_turn(since, start, Duration::from_secs(31), || {
        app.harness()
            .perform_a11y_action(id, &ui::a11y::Action::Activate)
            .expect("the fold acts");
        let _ = app.harness().poll_frame();
    });
    let folded = app
        .state()
        .hand
        .players
        .iter()
        .find(|p| p.player == "grace")
        .is_some_and(|p| p.folded);
    assert!(folded, "the slow player's hand is folded");
}

/// A hand may be dealt by anyone but the dealer only once the table has
/// waited: the button waits with the count, instead of refusing.
#[test]
fn dealing_is_offered_with_the_graces_count() {
    use ui::time::Instant;

    let start = Instant::now();
    ui::time::testing::with_monotonic_time(start, || {
        let mut app = TestApp::<App>::for_person("ada").expect("the app opens");
        take_a_seat(&mut app);
        let grace = ui::tilo_actions::ActionInvoker::actor("grace");
        app.call_by(
            grace,
            "so.tilo.poker.table",
            "sit",
            ui::serde_json::json!({ "seat": 2 }),
        )
        .expect("grace sits down");
        let dealer = app.state().dealer().map(str::to_owned).expect("a dealer");
        let dealer = ui::tilo_actions::ActionInvoker::actor(&dealer);
        app.call_by(
            dealer,
            "so.tilo.poker.table",
            "deal",
            ui::serde_json::json!({}),
        )
        .expect("the hand is dealt");
        app.settle();
        // The visible player folds, so the hand is over and the table waits
        // for a deal again.
        for _ in 0..8 {
            let Some(actor) = app.state().hand.actor().map(|p| p.player.clone()) else {
                break;
            };
            let who = ui::tilo_actions::ActionInvoker::actor(&actor);
            let verb = if actor == "ada" { "fold" } else { "call" };
            app.call_by(who, "so.tilo.poker.table", verb, ui::serde_json::json!({}))
                .expect("the turn is taken");
            if !app.state().hand.street.in_play() {
                break;
            }
        }
        assert!(!app.state().hand.street.in_play(), "the hand is over");

        // The dealer's own button acts at once; the other player's waits.
        let dealer = app.state().dealer().map(str::to_owned).expect("a dealer");
        if dealer == "ada" {
            let (_, open) = button(&mut app, "Deal next hand");
            assert!(open, "the dealer deals at once");
            return;
        }
        let (_, open) = button(&mut app, "Deal next hand");
        assert!(!open, "the other player waits out the grace");
        assert!(
            app.harness()
                .a11y_node_named("Anyone may deal in 15s")
                .is_some(),
            "the button says how long the wait is"
        );

        let since = app.state().hand.since_ms;
        into_turn(since, start, ui::time::Duration::from_secs(16), || {
            let _ = app.harness().poll_frame();
            let (_, open) = button(&mut app, "Deal next hand");
            assert!(open, "anyone may deal once the table has waited");
        });
    });
}
