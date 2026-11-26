//! Poker (`apps/tl/poker`): the table's rules over actions, the hand played to its showdown,
//! the window's seats and the moves it offers, and the graces that open a move.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value};

    struct Poker {
        rt: Runtime,
        app: App,
        auth: TestAuthority,
        root: ResourceRef,
    }

    impl Poker {
        fn new() -> Self {
            let app = app("poker");
            let auth = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
            app.load(&rt);
            let root = rt.create_root("Table").expect("the table's root");
            Self {
                rt,
                app,
                auth,
                root,
            }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> Result<InvokeOutcome, String> {
            self.call_as(&me(), action, args)
        }

        fn call_as(
            &self,
            who: &tilolang_runtime::Actor,
            action: &str,
            args: Vec<Value>,
        ) -> Result<InvokeOutcome, String> {
            let id = self.app.action(action);
            self.rt
                .invoke_action(who, (id.as_str(), args))
                .map_err(|error| error.to_string())
        }

        fn sit(&self, who: &tilolang_runtime::Actor, seat: i64) -> Result<InvokeOutcome, String> {
            self.call_as(
                who,
                "sit",
                vec![Value::resource(&self.root), Value::Int(seat)],
            )
        }

        /// The window as the person looking at it sees it.
        fn window(&self) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("poker_table"),
                    vec![Value::resource(&self.root)],
                )
                .unwrap()
        }

        fn players(&self) -> Vec<ResourceRef> {
            match self.rt.read(&self.root, "players").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("players reads {other:?}"),
            }
        }

        fn hand(&self) -> ResourceRef {
            match self.rt.read(&self.root, "hand").unwrap() {
                Value::Ref(r) => std::rc::Rc::unwrap_or_clone(r.target),
                other => panic!("hand reads {other:?}"),
            }
        }

        /// The players in the hand being played, in deal order.
        fn in_hand(&self) -> Vec<ResourceRef> {
            match self.rt.read(&self.hand(), "players").unwrap() {
                Value::List(items) => items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::Ref(r) => Some(std::rc::Rc::unwrap_or_clone(r.target)),
                        _ => None,
                    })
                    .collect(),
                other => panic!("players reads {other:?}"),
            }
        }

        fn int(&self, r: &ResourceRef, field: &str) -> i64 {
            match self.rt.read(r, field).unwrap() {
                Value::Int(n) => n,
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn flag(&self, r: &ResourceRef, field: &str) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Bool(b) => b,
                other => panic!("{field} reads {other:?}"),
            }
        }

        /// The player of `players` whose person is `who`.
        fn player_of(&self, who: &tilolang_runtime::Actor) -> Option<ResourceRef> {
            self.players()
                .into_iter()
                .find(|p| self.is_person(p, "person", who))
        }

        fn hand_player_of(&self, who: &tilolang_runtime::Actor) -> Option<ResourceRef> {
            self.in_hand()
                .into_iter()
                .find(|p| self.is_person(p, "player", who))
        }

        fn is_person(&self, r: &ResourceRef, field: &str, who: &tilolang_runtime::Actor) -> bool {
            match self.rt.read(r, field).unwrap() {
                Value::Actor(actor) => actor.id() == who.id(),
                other => panic!("{field} reads {other:?}"),
            }
        }

        fn chips(&self, who: &tilolang_runtime::Actor) -> i64 {
            self.player_of(who).map_or(0, |p| self.int(&p, "chips"))
        }

        fn total_chips(&self) -> i64 {
            self.players()
                .iter()
                .map(|p| self.int(p, "chips"))
                .sum::<i64>()
        }

        /// Whether the hand stands at the street `name`.
        fn street_is(&self, name: &str) -> bool {
            let Value::Enum(case) = self.rt.read(&self.hand(), "street").unwrap() else {
                panic!("street is an enum");
            };
            let wanted = self
                .app
                .compiled
                .ids
                .get(&format!("Street.{name}"))
                .map_or_else(
                    || panic!("the package declares Street.{name}"),
                    str::to_owned,
                );
            case.variant == wanted
        }

        fn board(&self) -> usize {
            match self.rt.read(&self.hand(), "board").unwrap() {
                Value::List(cards) => cards.len(),
                other => panic!("board reads {other:?}"),
            }
        }

        /// The place of the player to act, while one is to act.
        fn to_act(&self) -> Option<usize> {
            match self.rt.read(&self.hand(), "to_act").unwrap() {
                Value::Opt(Some(inner)) => match *inner {
                    Value::Int(at) => Some(usize::try_from(at).unwrap_or(0)),
                    other => panic!("to_act reads {other:?}"),
                },
                Value::Opt(None) => None,
                other => panic!("to_act reads {other:?}"),
            }
        }

        /// The actor to act, from the people the test seated.
        fn actor(&self) -> Option<tilolang_runtime::Actor> {
            let at = self.to_act()?;
            let player = self.in_hand().get(at).cloned()?;
            [me(), person("grace"), person("bo")]
                .into_iter()
                .find(|who| self.is_person(&player, "player", who))
        }

        /// Calls on the player to act until the hand is over, so a hand runs to its showdown.
        fn play_out(&self) {
            for _ in 0..24 {
                if self.street_is("done") {
                    return;
                }
                let Some(who) = self.actor() else {
                    return;
                };
                self.call_as(&who, "call", vec![Value::resource(&self.root)])
                    .expect("the player to act calls");
            }
            panic!("the hand did not end");
        }

        /// Presses the button `label` of the window as the person looking at it.
        fn press(&self, label: &str) -> InvokeOutcome {
            let mut view = self.window();
            let button = of_kind(view.tree(), "ui.Button")
                .into_iter()
                .find(|b| b.props.get("label").and_then(Value::as_text) == Some(label))
                .unwrap_or_else(|| panic!("no `{label}` button:\n{}", dump(view.tree())))
                .key
                .clone();
            let out = view.fire(&button, "press").unwrap();
            view.poll_patches().unwrap();
            out
        }

        fn button_labels(&self) -> Vec<String> {
            let view = self.window();
            of_kind(view.tree(), "ui.Button")
                .into_iter()
                .filter_map(|b| {
                    b.props
                        .get("label")
                        .and_then(Value::as_text)
                        .map(str::to_owned)
                })
                .collect()
        }

        fn has_countdown(&self) -> bool {
            let view = self.window();
            !of_kind(view.tree(), "ui.Countdown").is_empty()
        }

        /// The moment the window's button `label` opens at, when it waits for one: the button
        /// keeps the clock, stands dimmed with the time left on its face, and opens by itself
        /// (ui-core's `opens:`).
        fn button_opens(&self, label: &str) -> Option<String> {
            let view = self.window();
            of_kind(view.tree(), "ui.Button")
                .into_iter()
                .filter(|b| b.props.get("label").and_then(Value::as_text) == Some(label))
                .find_map(|b| match b.props.get("opens") {
                    Some(Value::Text(at)) => Some(at.clone()),
                    _ => None,
                })
        }
    }

    fn at(second: u64) -> tilo_clock::time::SystemTime {
        tilo_clock::time::UNIX_EPOCH
            .checked_add(tilo_clock::time::Duration::from_secs(second))
            .expect("a moment the clock holds")
    }

    /// Seats the person looking and Grace, one minute apart, so the person looking is the
    /// table's dealer for the first hand.
    fn seat_two(p: &Poker) {
        tilo_clock::testing::with_wall(at(1_700_000_000), || {
            p.sit(&me(), 1).expect("the person looking sits");
        });
        tilo_clock::testing::with_wall(at(1_700_000_060), || {
            p.sit(&person("grace"), 3).expect("Grace sits");
        });
    }

    fn other(who: &tilolang_runtime::Actor) -> tilolang_runtime::Actor {
        if who == &me() { person("grace") } else { me() }
    }

    fn seat_by(who: &tilolang_runtime::Actor, p: &Poker) -> i64 {
        p.player_of(who).map_or(-1, |player| p.int(&player, "seat"))
    }

    tilo_test_env::scenario! {
        fn the_table_seats_people_deals_hands_and_settles_them;

        phase two_people_asking_for_one_seat_both_get_a_place;
        {
            let p = Poker::new();
            let grace = person("grace");
            tilo_clock::testing::with_wall(at(1_700_000_000), || {
                p.sit(&me(), 3).expect("the person looking sits");
            });
            tilo_clock::testing::with_wall(at(1_700_000_060), || {
                p.sit(&grace, 3).expect("Grace sits at the same seat");
            });
            let seats: Vec<(String, i64)> = p
                .players()
                .into_iter()
                .map(|player| {
                    let who = match p.rt.read(&player, "person").unwrap() {
                        Value::Actor(actor) => format!("{actor:?}"),
                        other => format!("{other:?}"),
                    };
                    (who, p.int(&player, "seat"))
                })
                .collect();
            assert_eq!(seat_by(&me(), &p), 2, "the first to ask gets the seat: {seats:?}");
            assert_eq!(seat_by(&grace, &p), 3, "the later one takes the next free seat: {seats:?}");
            assert_eq!(p.total_chips(), 2000, "each sits down with a buy-in");
        }

        phase a_hand_plays_to_the_showdown_and_keeps_every_chip;
        {
            let p = Poker::new();
            let grace = person("grace");
            p.sit(&me(), 1).unwrap();
            p.sit(&grace, 3).unwrap();
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            assert!(p.street_is("preflop"));
            assert_eq!(p.board(), 0);
            p.play_out();
            assert!(p.street_is("done"));
            assert_eq!(p.board(), 5, "the board came out");
            assert_eq!(p.total_chips(), 2000, "the same chips, moved");
            let awards = match p.rt.read(&p.hand(), "awards").unwrap() {
                Value::List(items) => items.len(),
                other => panic!("awards reads {other:?}"),
            };
            assert!(awards >= 1, "the hand paid somebody");
        }

        phase only_the_player_to_act_may_act;
        {
            let p = Poker::new();
            seat_two(&p);
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            let acting = p.actor().expect("a player is to act");
            let idle = other(&acting);
            let refused = p
                .call_as(&idle, "fold", vec![Value::resource(&p.root)])
                .expect_err("the player not to act cannot fold");
            assert!(refused.contains("It is not your turn"), "{refused}");
            assert!(!p.flag(&p.hand_player_of(&idle).unwrap(), "folded"));
        }

        phase folding_to_a_raise_gives_the_raiser_the_pot;
        {
            let p = Poker::new();
            seat_two(&p);
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            let acting = p.actor().expect("a player is to act");
            let idle = other(&acting);
            p.call_as(
                &acting,
                "raise",
                vec![Value::resource(&p.root), Value::Float(30.0)],
            )
            .unwrap();
            p.call_as(&idle, "fold", vec![Value::resource(&p.root)])
                .unwrap();
            assert!(p.street_is("done"));
            assert_eq!(p.chips(&acting), 1010, "the raiser takes the blinds and the call");
            assert_eq!(p.chips(&idle), 990);
        }

        phase all_in_runs_the_board_out;
        {
            let p = Poker::new();
            seat_two(&p);
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            let acting = p.actor().expect("a player is to act");
            p.call_as(&acting, "all_in", vec![Value::resource(&p.root)])
                .unwrap();
            p.call_as(&other(&acting), "call", vec![Value::resource(&p.root)])
                .unwrap();
            assert!(p.street_is("done"), "nobody was left to bet");
            assert_eq!(p.board(), 5, "the board ran out");
            assert_eq!(p.total_chips(), 2000);
        }

        phase a_player_with_no_chips_buys_back_in_after_the_hand;
        {
            let p = Poker::new();
            let grace = person("grace");
            seat_two(&p);
            // Play all-in hands until one of the two has nothing left: the winner of a hand
            // takes the other's chips.
            for _ in 0..8 {
                if p.chips(&me()) == 0 || p.chips(&grace) == 0 {
                    break;
                }
                p.call("deal", vec![Value::resource(&p.root)]).unwrap();
                let acting = p.actor().expect("a player is to act");
                p.call_as(&acting, "all_in", vec![Value::resource(&p.root)])
                    .unwrap();
                p.call_as(&other(&acting), "call", vec![Value::resource(&p.root)])
                    .unwrap();
            }
            let broke = if p.chips(&me()) == 0 { me() } else { grace };
            assert_eq!(p.chips(&broke), 0, "a hand took everything");
            let refused = p
                .call_as(&broke, "rebuy", vec![Value::resource(&p.root)])
                .expect("a player with no chips buys back in");
            assert!(matches!(refused, InvokeOutcome::Done(_)));
            assert_eq!(p.chips(&broke), 1000);
        }

        phase leaving_folds_your_hand;
        {
            let p = Poker::new();
            let grace = person("grace");
            seat_two(&p);
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            p.call_as(&grace, "leave", vec![Value::resource(&p.root)])
                .unwrap();
            assert!(p.player_of(&grace).is_none(), "they left the table");
            assert!(
                p.flag(&p.hand_player_of(&grace).unwrap(), "folded"),
                "leaving folded their cards"
            );
        }

        phase opening_the_table_seats_nobody_and_offers_a_seat;
        {
            let p = Poker::new();
            assert!(p.players().is_empty(), "the table starts empty");
            let labels = p.button_labels();
            assert!(labels.iter().any(|l| l == "Take a seat"), "{labels:?}");
            let refused = p
                .call("fold", vec![Value::resource(&p.root)])
                .expect_err("nobody at the table can fold");
            assert!(refused.contains("No hand is being played"), "{refused}");
            assert!(p.players().is_empty(), "nobody sat down");
            let refused = p
                .call("leave", vec![Value::resource(&p.root)])
                .expect_err("nobody at the table can leave");
            assert!(refused.contains("You are not at the table"), "{refused}");
        }

        phase a_person_the_authority_gives_no_say_cannot_sit_down;
        {
            let p = Poker::new();
            let guest = person("guest");
            p.auth.deny_invoker(&guest);
            assert!(
                matches!(p.sit(&guest, 1), Ok(InvokeOutcome::Denied)),
                "a viewer the authority gives no say sits nowhere"
            );
            assert!(p.players().is_empty(), "the table did not take them");
        }

        phase the_window_names_its_cards_and_the_turn_offers_its_moves;
        {
            let p = Poker::new();
            let grace = person("grace");
            let _ = grace;
            seat_two(&p);
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            let acting = p.actor().expect("a player is to act");
            p.call_as(&acting, "call", vec![Value::resource(&p.root)])
                .unwrap();
            let view = p.window();
            let title = of_kind(view.tree(), "ui.NavShell")
                .into_iter()
                .find_map(|n| n.props.get("title").and_then(Value::as_text).map(str::to_owned))
                .unwrap_or_default();
            assert_eq!(title, "Hand 1", "{}", dump(view.tree()));
            let labels = p.button_labels();
            for shown in ["Fold", "Check", "Raise", "All in"] {
                assert!(labels.iter().any(|l| l == shown), "{shown} is missing: {labels:?}");
            }
            assert!(!p.has_countdown(), "the player to act waits on nobody");
            // Every card is named for a screen reader: the faces by rank and suit, and the
            // places the board has not filled yet.
            let named: Vec<String> = of_kind(view.tree(), "ui.Drawing")
                .into_iter()
                .filter_map(|n| n.props.get("label").and_then(Value::as_text).map(str::to_owned))
                .collect();
            assert!(
                named.iter().filter(|l| l.as_str() == "No card yet").count() >= 5,
                "{named:?}"
            );
            assert!(
                named.iter().any(|l| l.contains(" of ")),
                "the player's own cards are named: {named:?}"
            );
            // A press runs the same action the pointer would.
            let out = p.press("Fold");
            assert!(matches!(out, InvokeOutcome::Done(_)));
            assert!(p.street_is("done"), "folding heads-up ends the hand");
        }

        phase folding_for_a_slow_player_counts_down_then_opens;
        {
            let p = Poker::new();
            let grace = person("grace");
            p.sit(&me(), 1).unwrap();
            p.sit(&grace, 3).unwrap();
            // The deal's own moment, so the hand's turn began at a moment the test names.
            p.call(
                "deal",
                vec![
                    Value::resource(&p.root),
                    Value::text("2099-01-01T00:00:00Z"),
                ],
            )
            .unwrap();
            // The window of the player waiting shows the fold for the slow player, dimmed with
            // the time left on its face until their turn runs out.
            let opens = p
                .button_opens("Fold for grace")
                .expect("the slow player's fold waits for the turn to run out");
            assert!(opens.starts_with("2099-"), "it opens after the turn: {opens}");
            let refused = p
                .call_as(
                    &grace,
                    "skip",
                    vec![
                        Value::resource(&p.root),
                        Value::text("2099-01-01T00:00:10Z"),
                    ],
                )
                .expect_err("the turn is not over yet");
            assert!(refused.contains("They still have time"), "{refused}");
            p.call_as(
                &grace,
                "skip",
                vec![
                    Value::resource(&p.root),
                    Value::text("2099-01-01T00:00:40Z"),
                ],
            )
            .expect("the turn ran out");
            assert!(p.street_is("done"), "the folded hand went to the other player");
        }

        phase side_pots_keep_a_short_stack_out_of_what_it_never_matched;
        {
            let p = Poker::new();
            let people = [me(), person("grace"), person("bo")];
            p.sit(&people[0], 1).unwrap();
            p.sit(&people[1], 3).unwrap();
            p.sit(&people[2], 5).unwrap();
            // Hand one leaves three different stacks: one player raises and the others fold.
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            let raiser = p.actor().expect("a player is to act");
            p.call_as(
                &raiser,
                "raise",
                vec![Value::resource(&p.root), Value::Float(100.0)],
            )
            .unwrap();
            for _ in 0..2 {
                let Some(who) = p.actor() else { break };
                if who != raiser {
                    p.call_as(&who, "fold", vec![Value::resource(&p.root)])
                        .unwrap();
                }
            }
            assert!(p.street_is("done"));
            assert_eq!(p.total_chips(), 3000, "the same chips, moved");
            let stacks: Vec<i64> = people.iter().map(|who| p.chips(who)).collect();
            let deepest = *stacks.iter().max().unwrap_or(&0);
            let short = *stacks.iter().min().unwrap_or(&0);
            assert!(deepest > short, "the hand moved chips: {stacks:?}");
            // Hand two: everyone in for everything they have; the short stack's all-in cannot
            // win the chips only the deeper stacks matched.
            p.call("deal", vec![Value::resource(&p.root)]).unwrap();
            for _ in 0..9 {
                if p.street_is("done") {
                    break;
                }
                for who in &people {
                    if p.actor().as_ref() == Some(who) {
                        let _ = p.call_as(who, "all_in", vec![Value::resource(&p.root)]);
                    }
                }
            }
            let stacks_now: Vec<i64> = people.iter().map(|who| p.chips(who)).collect();
            assert!(p.street_is("done"), "the board ran out: stacks={stacks_now:?}");
            assert_eq!(p.board(), 5);
            assert_eq!(p.total_chips(), 3000, "every chip is still at the table");
            let awards = p.rt.read(&p.hand(), "awards").unwrap();
            let Value::List(awards) = awards else {
                panic!("awards is a list");
            };
            let amount_of = |award: &Value| match award {
                Value::Ref(r) => {
                    let award = std::rc::Rc::unwrap_or_clone(r.target.clone());
                    match p.rt.read(&award, "amount").unwrap() {
                        Value::Int(n) => n,
                        _ => 0,
                    }
                }
                _ => 0,
            };
            let paid: i64 = awards.iter().map(amount_of).sum();
            let committed: i64 = p
                .in_hand()
                .iter()
                .map(|hp| p.int(hp, "committed"))
                .sum();
            assert_eq!(paid, committed, "every chip put in is paid out: {paid}");
            let short_who = people[stacks.iter().position(|s| *s == short).unwrap_or(0)].clone();
            let short_award = awards
                .iter()
                .filter(|award| match award {
                    Value::Ref(r) => {
                        let award = std::rc::Rc::unwrap_or_clone(r.target.clone());
                        p.is_person(&award, "player", &short_who)
                    }
                    _ => false,
                })
                .map(amount_of)
                .max()
                .unwrap_or(0);
            assert!(
                short_award <= short * 3,
                "the short stack wins at most the pot it matched: {short_award}"
            );
        }

        phase dealing_again_waits_for_the_dealers_grace;
        {
            let p = Poker::new();
            let grace = person("grace");
            seat_two(&p);
            p.call(
                "deal",
                vec![
                    Value::resource(&p.root),
                    Value::text("1970-01-01T00:00:00Z"),
                ],
            )
            .unwrap();
            p.play_out();
            assert!(p.street_is("done"), "the hand played out");
            // The other player cannot deal straight after the hand; the others can once the
            // table has waited.
            let refused = p
                .call_as(
                    &grace,
                    "deal",
                    vec![
                        Value::resource(&p.root),
                        Value::text("1970-01-01T00:00:10Z"),
                    ],
                )
                .expect_err("the grace has not run out");
            assert!(refused.contains("The dealer deals the next hand"), "{refused}");
            p.call_as(
                &grace,
                "deal",
                vec![
                    Value::resource(&p.root),
                    Value::text("1970-01-01T00:00:20Z"),
                ],
            )
            .expect("anyone deals once the table has waited");
            assert!(p.street_is("preflop"));
            assert_eq!(p.int(&p.hand(), "number"), 2);
        }
    }
}
