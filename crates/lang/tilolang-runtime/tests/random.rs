//! Draws: `random.int`, `random.float`, `random.choice`, `random.shuffle` and `random.id` are
//! made where the action runs, and the numbers they drew are kept in its journal, so a
//! journal, so a run resumed or replayed reads the numbers it drew the first time.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::run::{JournalEntry, RunOutcome};
    use tilolang_runtime::{
        AmStore, InvokeOutcome, Resolution, ResourceRef, Runtime, SimpleAuthority, Store, Value,
    };

    const DICE: &str = r#"app so.tilo.dice "Dice"

type Roll {
    value: Int
    mark: Text
}

action roll(r: Roll) uses random {
    r.value = random.int(1, 6)
    r.mark = random.id()
}

action pick() -> Option<Int> uses random {
    return random.choice([3, 5, 7])
}

action none_pick() -> Option<Int> uses random {
    return random.choice([1, 2].filter(x -> x > 9))
}

action mixed() -> List<Int> uses random {
    return random.shuffle([1, 2, 3, 4, 5])
}

action half() -> Float uses random {
    return random.float()
}
"#;

    const BOARD: &str = r#"app so.tilo.board "Board"

type Board {
    marks: Int merge counter
    drawn: Int
}

action mark(b: Board, n: Int) {
    b.marks += 1
    b.drawn = n
}
"#;

    /// The runner's action draws before it calls another package's action, which asks: what it
    /// drew has to survive the run waiting for a person.
    const RUNNER: &str = r#"app so.tilo.runner "Runner"

use so.tilo.board as board

action start(b: board.Board) -> Int uses random {
    let first = random.int(1, 1000)
    board.mark(b, n: first)
    return first
}
"#;

    fn runtime() -> (Runtime, AmStore) {
        let store = AmStore::new();
        let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
        (rt, store)
    }

    /// A draw of `0..2^32` is one eight-digit part of an id.
    const ID_BITS: i64 = 1 << 32;

    #[test]
    fn an_actions_draws_are_kept_beside_its_change() {
        let (rt, _store) = runtime();
        let app = App::compile(DICE, &[]);
        app.load(&rt);
        let roll = rt.create("Roll").unwrap();
        let out = rt
            .invoke_action(
                &me(),
                (app.action("roll").as_str(), vec![Value::resource(&roll)]),
            )
            .unwrap();
        assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");

        let Value::Int(value) = rt.read(&roll, "value").unwrap() else {
            panic!("a roll's value is a number");
        };
        assert!((1..=6).contains(&value), "a die shows 1 to 6: {value}");

        let Value::Text(mark) = rt.read(&roll, "mark").unwrap() else {
            panic!("an id is text");
        };
        assert_eq!(mark.len(), 32, "an id is 128 bits: {mark}");
        assert!(
            mark.chars().all(|c| c.is_ascii_hexdigit()),
            "an id is hex: {mark}"
        );

        // The change says what it drew: the die, then the four parts of the id.
        let history = rt.history(&roll.doc).unwrap();
        let effects = &history.last().expect("the change it made").effects;
        let Some(&JournalEntry::Random {
            upto: 6,
            value: drew,
        }) = effects.first()
        else {
            panic!("the change's first entry is the draw it made: {effects:?}");
        };
        assert_eq!(
            drew + 1,
            value,
            "the value the action wrote is the draw it made, counted from the lowest"
        );
        let parts: Vec<&JournalEntry> = effects.iter().skip(1).collect();
        assert_eq!(parts.len(), 4, "an id is four draws: {effects:?}");
        for (part, digits) in parts.iter().zip(mark.as_bytes().chunks(8)) {
            let JournalEntry::Random { upto, value } = part else {
                panic!("an id's parts are draws: {part:?}");
            };
            assert_eq!(*upto, ID_BITS);
            assert_eq!(
                format!("{value:08x}"),
                String::from_utf8_lossy(digits),
                "the id it wrote is the numbers it drew"
            );
        }
    }

    #[test]
    fn a_run_resumed_reads_the_number_it_drew() {
        let (rt, mut store) = runtime();
        let board_app = App::compile(BOARD, &[]);
        let runner = App::compile(RUNNER, &[board_app.meta().clone()]);
        board_app.load(&rt);
        runner.load(&rt);
        let board = rt.create("Board").unwrap();
        let out = rt
            .invoke_action(
                &me(),
                (
                    runner.action("start").as_str(),
                    vec![Value::resource(&board)],
                ),
            )
            .unwrap();
        let InvokeOutcome::Suggested(first) = out else {
            panic!("the other package's action must ask: {out:?}");
        };

        // The number the run drew is in its journal before it goes on, and the action it asked
        // for has not run yet.
        let journal = store
            .load_journal(&first.run)
            .unwrap()
            .expect("a suspended run's journal");
        // The entry is the number drawn below the range's width; `random.int(1, 1000)` adds the
        // lowest to it.
        let drawn = match journal.entries.first() {
            Some(&JournalEntry::Random { upto: 1000, value }) => value + 1,
            other => panic!("the run's first entry is the draw it made: {other:?}"),
        };
        assert_eq!(marks(&rt, &board), 0);

        // Resumed, it reads the number it drew the first time, not a new one.
        let out = rt.resolve(&first.run, &me(), Resolution::Approve).unwrap();
        assert_eq!(
            out,
            RunOutcome::Done(Value::Int(drawn)),
            "a resumed run reads the draw it made"
        );
        assert_eq!(marks(&rt, &board), 1, "the step it approved ran once");
        let Value::Int(passed) = rt.read(&board, "drawn").unwrap() else {
            panic!("the number the action was passed is a number");
        };
        assert_eq!(
            passed, drawn,
            "the action it approved was passed the number the run drew"
        );
    }

    #[test]
    fn a_choice_makes_one_of_the_list_or_none_of_nothing() {
        let (rt, _store) = runtime();
        let app = App::compile(DICE, &[]);
        app.load(&rt);
        let InvokeOutcome::Done(Value::Opt(picked)) = rt
            .invoke_action(&me(), (app.action("pick").as_str(), Vec::new()))
            .unwrap()
        else {
            panic!("a choice of three numbers");
        };
        let Some(Value::Int(picked)) = picked.as_deref() else {
            panic!("a choice of [3, 5, 7] picked {picked:?}");
        };
        assert!(
            [3, 5, 7].contains(picked),
            "a choice is of the list: {picked}"
        );

        // Drawing from nothing draws nothing, so it reads `.none`.
        let InvokeOutcome::Done(Value::Opt(none)) = rt
            .invoke_action(&me(), (app.action("none_pick").as_str(), Vec::new()))
            .unwrap()
        else {
            panic!("a choice of nothing");
        };
        assert!(none.is_none(), "a choice of no number is `.none`");
    }

    #[test]
    fn a_shuffle_keeps_the_items_it_was_given() {
        let (rt, _store) = runtime();
        let app = App::compile(DICE, &[]);
        app.load(&rt);
        let InvokeOutcome::Done(Value::List(items)) = rt
            .invoke_action(&me(), (app.action("mixed").as_str(), Vec::new()))
            .unwrap()
        else {
            panic!("a shuffle of five numbers");
        };
        let mut numbers: Vec<i64> = items
            .iter()
            .map(|v| match v {
                Value::Int(n) => *n,
                other => panic!("a shuffled number is a number: {other:?}"),
            })
            .collect();
        numbers.sort_unstable();
        assert_eq!(numbers, [1, 2, 3, 4, 5], "a shuffle loses and adds nothing");
    }

    #[test]
    fn a_float_is_between_nothing_and_one() {
        let (rt, _store) = runtime();
        let app = App::compile(DICE, &[]);
        app.load(&rt);
        let InvokeOutcome::Done(Value::Float(n)) = rt
            .invoke_action(&me(), (app.action("half").as_str(), Vec::new()))
            .unwrap()
        else {
            panic!("a float");
        };
        assert!((0.0..1.0).contains(&n), "a float draw is [0, 1): {n}");
    }

    fn marks(rt: &Runtime, board: &ResourceRef) -> i64 {
        match rt.read(board, "marks").unwrap() {
            Value::Int(n) => n,
            other => panic!("marks is {other:?}"),
        }
    }
}
