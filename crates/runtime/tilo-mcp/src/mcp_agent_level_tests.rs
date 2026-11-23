//! How an app's directory is read: a level as one snapshot, each level once in a call and at
//! a cost that does not grow with what the app holds, and a resource found wherever it falls
//! in a level whose first read stops at its bound.

use super::super::DEFAULT_WAIT_MS;
use super::*;
use tilo_actions::tree::LEVEL_PAGES;

tilo_test_env::scenario! {
    fn a_call_reads_the_directory_once_at_a_cost_that_does_not_grow;

    phase a_level_read_across_a_moving_directory_starts_again_at_the_new_revision;
    // A level is read as one snapshot: when the app's directory moves on
    // between its pages, the read starts again at the new revision and the agent
    // sees the whole level, not an error or a mix of two revisions.
    {
        let steady = Notes::new(true);
        let moving = Notes::new(true);
        *moving.moves.lock().unwrap() = 2;
        let (expected, failed) = run(tool(&agent(&steady), "tilo_ls", json!({"path": "notes"})));
        assert!(!failed, "{expected}");
        let (listed, failed) = run(tool(&agent(&moving), "tilo_ls", json!({"path": "notes"})));
        assert!(!failed, "{listed}");
        assert_eq!(listed, expected);
        assert_eq!(
            *moving.moves.lock().unwrap(),
            0,
            "both moves were read past"
        );
    }

    phase a_directory_that_never_holds_still_is_reported;
    // A directory that never holds still for one read is reported, once the
    // time an action is waited for has passed, and is read again with a pause
    // that grows rather than as fast as it answers.
    {
        let moving = Notes::new(true);
        *moving.moves.lock().unwrap() = u32::MAX;
        let (text, failed, waited) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let started = tokio::time::Instant::now();
                let (text, failed) = tool(&agent(&moving), "tilo_ls", json!({"path": "notes"})).await;
                (text, failed, started.elapsed())
            });
        assert!(failed, "{text}");
        assert!(text.contains("kept changing"), "{text}");
        assert!(
            waited >= std::time::Duration::from_millis(DEFAULT_WAIT_MS),
            "gave up after {waited:?}"
        );
        let reads = u32::MAX.saturating_sub(*moving.moves.lock().unwrap());
        assert!(
            (20..=50).contains(&reads),
            "read {reads} times in {waited:?}"
        );
    }

    phase every_note_a_notebook_lists_is_read_at_its_path_past_a_levels_bound;
    // A directory level is read as far as its bound ([`LEVEL_PAGES`] pages), and a notebook
    // that holds more goes on past it. Every note the notebook's read lists is read at the
    // path the read gave for it, wherever the note falls in the directory: a name the pages
    // read first do not carry is looked for on the pages after them, and only one that no
    // page carries is said not to be there.
    {
        const PAGE: usize = 16;
        let grown = PAGE.saturating_mul(LEVEL_PAGES).saturating_mul(2);
        let host = Notes::new(true).with_notes(grown).paging_levels(PAGE);
        run(async {
            let (notebook, failed) =
                answer(&agent(&host), "tilo_read", json!({"path": "notes/Notebook"})).await;
            assert!(!failed, "{notebook}");
            let listed: Vec<(String, String)> = notebook["children"]
                .as_array()
                .expect("the notebook lists its notes")
                .iter()
                .map(|child| {
                    (
                        child["name"].as_str().unwrap().to_owned(),
                        child["path"].as_str().unwrap().to_owned(),
                    )
                })
                .filter(|(name, _)| name.starts_with("Bench note"))
                .collect();
            assert_eq!(listed.len(), grown, "{notebook}");
            for (name, path) in &listed {
                let (text, failed) = tool(&agent(&host), "tilo_read", json!({"path": path})).await;
                assert!(!failed, "{path}: {text}");
                assert!(text.contains(name.as_str()), "{path} reads {name}: {text}");
            }
            let missing = "notes/Notebook/notes.page:ffffffff-1111-4222-8333-000000000000";
            let (text, _) = tool(&agent(&host), "tilo_read", json!({"path": missing})).await;
            assert!(
                text.contains("no notes.page:ffffffff-1111-4222-8333-000000000000 in notes"),
                "a note no page carries is not there: {text}"
            );
        });

        // The address an action answers a note by leads to it the same way.
        let live = Notes::new(true).live().with_notes(grown).paging_levels(PAGE);
        run(async {
            for index in [0, grown.saturating_sub(1)] {
                let address =
                    format!("notes.notebook/pages/{index:08x}-1111-4222-8333-000000000000");
                let (text, failed) =
                    tool(&agent(&live), "tilo_ls", json!({"path": address})).await;
                assert!(!failed, "{address}: {text}");
                assert!(text.contains("#rename"), "{address} lists its note: {text}");
            }
        });

        // A record's host lists each item's actions in the record's own level, after the
        // item: five rows a note here, so the bound of a level's first read falls between a
        // note and the action that reads it. That note is read by its own action like every
        // other, never as one that has none.
        const ROWS: usize = 5;
        let placed = Notes::new(true)
            .live()
            .placing_actions()
            .with_notes(grown)
            .paging_levels(PAGE);
        // The notebook's own action, then Todo and Groceries, come before the notes it grew by.
        let bound = PAGE.saturating_mul(LEVEL_PAGES);
        let cut = bound.saturating_sub(1).div_euclid(ROWS).saturating_sub(2);
        assert_ne!(
            bound.saturating_sub(1).rem_euclid(ROWS),
            0,
            "the bound falls inside a note"
        );
        run(async {
            for index in [0, cut.saturating_sub(1), cut, cut.saturating_add(1), grown.saturating_sub(1)] {
                let key = format!("notes.notebook/pages/{index:08x}-1111-4222-8333-000000000000");
                let path = format!("notes/Notebook/{}", key.replace('/', "\\/"));
                let (text, failed) = tool(&agent(&placed), "tilo_read", json!({"path": path})).await;
                assert!(!failed, "note {index} at {path}: {text}");
                let read = placed.invoked().pop().expect("the read was invoked");
                assert_eq!(
                    (read.0, read.1.as_str()),
                    (placed.id(&key), "read"),
                    "note {index} is read by its own action"
                );
            }
        });
    }

    phase a_call_asks_for_each_directory_level_and_the_running_apps_once;
    // One call asks the app's directory for each level once and asks which apps
    // run once: reading a note finds the action that reads it among the levels
    // its path was resolved through, and running an action resolves its path in
    // the app it looked up, without asking either again.
    {
        let host = Notes::new(true);
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_read", json!({"path": "notes/Todo"})).await;
            assert!(!failed, "{text}");
            let asked = host.take_asked();
            let mut once = asked.clone();
            once.sort();
            once.dedup();
            assert_eq!(asked.len(), once.len(), "asked twice: {asked:#?}");

            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "x"}}),
            )
            .await;
            assert!(!failed, "{text}");
            let asked = host.take_asked();
            assert_eq!(
                asked.iter().filter(|asked| *asked == "apps").count(),
                1,
                "{asked:#?}"
            );
        });
    }

    phase a_read_of_one_note_costs_the_same_in_a_notebook_of_any_size;
    // Reading one note costs what it costs however many notes the notebook
    // holds: a notebook grown from 2 notes to 32 asks the directory nothing
    // more and is answered no more rows for one note's read.
    {
        let read = json!({"path": "notes/Notebook/Todo"});
        let small = work_of(&Notes::new(true).live(), "tilo_read", &read);
        let large = work_of(&Notes::new(true).live().with_notes(30), "tilo_read", &read);
        eprintln!("one read: {small:?} at 2 notes, {large:?} at 32");
        assert_eq!(small.2, large.2, "the same note reads the same");
        assert_eq!(
            (small.0, small.1),
            (large.0, large.1),
            "queries and rows at 2 notes against 32"
        );
    }

    phase an_action_on_the_notebook_or_a_note_costs_the_same_in_a_notebook_of_any_size;
    // Making a note through the notebook's action, or appending to one note's
    // body, costs the same however many notes the notebook holds: the action is
    // found by its address, not in a page of the notebook's notes.
    {
        for (path, args) in [
            ("notes/Notebook#new-note", json!({"title": "Plan"})),
            (
                "notes/Notebook/Todo/Body#append-paragraph",
                json!({"text": "milk"}),
            ),
        ] {
            let call = json!({"path": path, "args": args});
            let small = work_of(&Notes::new(true).live(), "tilo_do", &call);
            let large = work_of(&Notes::new(true).live().with_notes(30), "tilo_do", &call);
            eprintln!("{path}: {small:?} at 2 notes, {large:?} at 32");
            assert_eq!(small.2, large.2, "{path} answers the same");
            assert_eq!(
                (small.0, small.1),
                (large.0, large.1),
                "{path}: queries and rows at 2 notes against 32"
            );
        }
    }

    phase a_find_costs_what_it_finds_in_a_notebook_of_any_size;
    // Finding a note by its title, or the action that makes one, costs the
    // rows that name it however many notes the notebook holds.
    {
        for query in ["Todo", "new note"] {
            let find = json!({"query": query, "app": "notes"});
            let small = work_of(&Notes::new(true).live(), "tilo_find", &find);
            let large = work_of(&Notes::new(true).live().with_notes(30), "tilo_find", &find);
            eprintln!("find {query}: {small:?} at 2 notes, {large:?} at 32");
            assert_eq!(small.2, large.2, "{query} finds the same");
            assert_eq!(
                (small.0, small.1),
                (large.0, large.1),
                "{query}: queries and rows at 2 notes against 32"
            );
        }
        let (text, _) = run(tool(
            &agent(&Notes::new(true).live().with_notes(30)),
            "tilo_find",
            json!({"query": "Todo", "app": "notes"}),
        ));
        assert!(
            text.lines()
                .any(|line| line == "notes/Notebook/Todo  notes.notebook.pages"),
            "the note is named by the path its listing names it: {text}"
        );
    }

    phase a_find_of_many_notes_asks_each_step_of_their_paths_once;
    // A find that names many notes of one notebook asks the directory for each note's own
    // rows once and for the steps above them once for all: what it asks grows by four
    // listings a note (the note is asked for at the app's top and, in its notebook, by its
    // name and by its key, and its own level is read), whatever actions of the note the
    // search answers beside it. It used to ask for the note in its notebook again for each
    // step of its path, and all of it again for each action.
    {
        let notes = || Notes::new(true).live().with_notes(15);
        let one = work_of(
            &notes(),
            "tilo_find",
            &json!({"query": "Bench note 007", "app": "notes", "limit": 100}),
        );
        let many = work_of(
            &notes(),
            "tilo_find",
            &json!({"query": "Bench note", "app": "notes", "limit": 100}),
        );
        // The lines that name a note itself; an action of a note is written after `#`.
        let named = |text: &str| {
            text.lines()
                .filter(|line| line.contains("Bench note") && !line.contains('#'))
                .count()
        };
        eprintln!(
            "find one note: {} queries, {} rows; find {} notes: {} queries, {} rows",
            one.0,
            one.1,
            named(&many.2),
            many.0,
            many.1
        );
        assert_eq!(named(&one.2), 1, "one note is found: {}", one.2);
        assert_eq!(named(&many.2), 15, "each of the fifteen is named once: {}", many.2);
        assert!(
            many.0 <= one.0.saturating_add(4 * 14),
            "fifteen notes of one notebook cost {} queries where one costs {}: more than four \
             for each further note",
            many.0,
            one.0
        );
    }
}
