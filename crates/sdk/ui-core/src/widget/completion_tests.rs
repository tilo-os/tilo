#![allow(clippy::indexing_slicing, reason = "test assertions")]

use super::*;

fn person(name: &str, handle: &str) -> Candidate {
    Candidate::reference(name, format!("@{name}"), format!("person:{handle}")).with_handle(handle)
}

fn names(rows: &[Entry]) -> Vec<String> {
    rows.iter()
        .map(|entry| match entry {
            Entry::Header { title, busy } => {
                format!("# {title}{}", if *busy { " …" } else { "" })
            }
            Entry::Item { candidate, .. } => candidate.name.clone(),
            Entry::NoMatch(query) => format!("none: {query}"),
            Entry::Create { label, query } => format!("{label}: {query}"),
        })
        .collect()
}

/// This conversation's speakers, then the channel's members, then the workspace.
fn people() -> PreparedSource {
    PreparedSource::new(CompletionSource::new(
        '@',
        vec![
            Tier::new(vec![person("Mia Chen", "mia"), person("Leo Park", "leo")])
                .with_header("In this conversation")
                .with_limit(5),
            Tier::new(vec![
                person("Leo Park", "leo"),
                person("Emil Brandt", "emil"),
                person("Camille Durand", "cam"),
                person("José Ruiz", "jose"),
            ])
            .with_header("Members"),
            Tier::new(vec![
                person("Milan Novak", "milan"),
                person("Amira Haddad", "amira"),
            ]),
        ],
    ))
}

#[test]
fn with_no_letters_each_tier_shows_under_its_header_and_a_person_once() {
    let rows = rows(&people(), "");
    assert_eq!(
        names(&rows),
        [
            "# In this conversation",
            "Mia Chen",
            "Leo Park",
            "# Members",
            "Emil Brandt",
            "Camille Durand",
            "José Ruiz",
            "Milan Novak",
            "Amira Haddad",
        ],
        "Leo is in this conversation, so the members do not list him again"
    );
}

#[test]
fn a_tiers_limit_holds_only_before_letters() {
    let source = PreparedSource::new(CompletionSource::new(
        '@',
        vec![
            Tier::new(
                (0..8)
                    .map(|i| person(&format!("Recent {i}"), &format!("r{i}")))
                    .collect(),
            )
            .with_header("In this conversation")
            .with_limit(5),
        ],
    ));
    let before = rows(&source, "");
    assert_eq!(before.iter().filter(|e| e.is_pickable()).count(), 5);
    let typing = rows(&source, "rec");
    assert_eq!(typing.iter().filter(|e| e.is_pickable()).count(), 8);
}

#[test]
fn typing_orders_prefix_then_word_then_substring_then_fuzzy_without_headers() {
    let rows = rows(&people(), "mi");
    assert_eq!(
        names(&rows),
        // Prefix: Mia (this conversation) before Milan (workspace). Substring: Camille
        // (members), Amira (workspace). Emil holds "mi" inside a word too.
        [
            "Mia Chen",
            "Milan Novak",
            "Emil Brandt",
            "Camille Durand",
            "Amira Haddad"
        ],
    );
    assert!(
        rows.iter()
            .all(|entry| !matches!(entry, Entry::Header { .. }))
    );
}

#[test]
fn a_word_in_the_name_ranks_after_a_prefix_and_before_a_substring() {
    let source = PreparedSource::new(CompletionSource::new(
        '#',
        vec![Tier::new(vec![
            Candidate::text("design-review", "#design-review"),
            Candidate::text("ui design", "#ui design"),
            Candidate::text("redesign", "#redesign"),
            Candidate::text("dev signals", "#dev signals"),
        ])],
    ));
    assert_eq!(
        names(&rows(&source, "design")),
        ["design-review", "ui design", "redesign", "dev signals"],
        "a prefix, a later word, inside a word, then the letters in order"
    );
    assert_eq!(
        names(&rows(&source, "dsg")),
        ["design-review", "ui design", "dev signals"],
        "the letters in order from a word start are a close match"
    );
}

#[test]
fn accents_and_case_fold_as_system_search_folds_them() {
    let names = names(&rows(&people(), "JOSE"));
    assert_eq!(names, ["José Ruiz"]);
    let names_by_handle = rows(&people(), "cam");
    assert_eq!(
        self::names(&names_by_handle),
        ["Camille Durand"],
        "a handle is matched too"
    );
}

#[test]
fn the_matched_letters_are_the_names_own_bytes() {
    let rows = rows(&people(), "jo");
    let Entry::Item { candidate, matched } = &rows[0] else {
        panic!("José matches");
    };
    assert_eq!(matched.len(), 1);
    assert_eq!(candidate.name.get(matched[0].clone()), Some("Jo"));
    let rows = self::rows(&people(), "rui");
    let Entry::Item { candidate, matched } = &rows[0] else {
        panic!("Ruiz matches");
    };
    assert_eq!(candidate.name.get(matched[0].clone()), Some("Rui"));
}

#[test]
fn a_query_nothing_matches_says_so_and_offers_to_create_where_allowed() {
    assert_eq!(names(&rows(&people(), "zz")), ["none: zz"]);
    let channels = PreparedSource::new(
        CompletionSource::new(
            '#',
            vec![Tier::new(vec![Candidate::text("general", "#general")])],
        )
        .with_create("Create channel"),
    );
    assert_eq!(
        names(&rows(&channels, "launch")),
        ["none: launch", "Create channel: launch"]
    );
    let created = rows(&channels, "launch");
    assert!(!created[0].is_pickable(), "the notice is never selected");
    assert!(created[1].is_pickable());
}

#[test]
fn a_slow_source_answers_under_its_own_header_below_and_never_reorders_the_local_rows() {
    let waiting = PreparedSource::new(people().source.with_slow(SlowTier {
        header: "Outside the workspace".into(),
        candidates: None,
    }));
    let before = rows(&waiting, "mi");
    assert_eq!(
        names(&before),
        [
            "Mia Chen",
            "Milan Novak",
            "Emil Brandt",
            "Camille Durand",
            "Amira Haddad",
            "# Outside the workspace …",
        ]
    );
    let mut arrived = people().source;
    arrived.slow.push(SlowTier {
        header: "Outside the workspace".into(),
        candidates: Some(vec![
            person("Mila Ivers", "mila"),
            person("Ada Lovelace", "ada"),
        ]),
    });
    let after = rows(&PreparedSource::new(arrived), "mi");
    assert_eq!(
        names(&after),
        [
            "Mia Chen",
            "Milan Novak",
            "Emil Brandt",
            "Camille Durand",
            "Amira Haddad",
            "# Outside the workspace",
            "Mila Ivers",
        ],
        "a better match from outside comes below what was shown"
    );
    let still_waiting = rows(&waiting, "zz");
    assert_eq!(
        names(&still_waiting),
        ["# Outside the workspace …"],
        "no notice while a source may still answer"
    );
}

#[test]
fn a_still_matching_selected_row_keeps_its_place() {
    let source = people();
    let shown = rows(&source, "m");
    // The person moved the selection to Milan, the second row.
    let milan = shown
        .iter()
        .position(|e| e.key() == Some("person:milan"))
        .unwrap_or_default();
    assert_eq!(milan, 1);
    let next = rows(&source, "mil");
    let (next, selected) = keep_place(next, Some(("person:milan", milan)));
    assert_eq!(selected, Some(1));
    assert_eq!(next[1].key(), Some("person:milan"), "Milan stays second");
    // A selection that no longer matches goes back to the first row.
    let (_, selected) = keep_place(rows(&source, "jo"), Some(("person:milan", 1)));
    assert_eq!(selected, Some(0));
}

#[test]
fn the_first_pickable_row_starts_selected() {
    let (rows, selected) = keep_place(rows(&people(), ""), None);
    assert_eq!(selected, Some(1), "after the header");
    assert!(rows[1].is_pickable());
}

#[test]
fn filtering_a_large_source_per_keystroke_stays_in_the_ui() {
    // Ten thousand members, folded once when the source arrives; each keystroke only
    // matches against the folded names.
    let members: Vec<Candidate> = (0..10_000)
        .map(|i| person(&format!("Member {i:05}"), &format!("m{i}")))
        .collect();
    let source = PreparedSource::new(CompletionSource::new('@', vec![Tier::new(members)]));
    let started = std::time::Instant::now();
    for query in [
        "m",
        "me",
        "mem",
        "memb",
        "member 0",
        "member 01",
        "member 012",
    ] {
        let shown = rows(&source, query);
        assert!(shown.len() <= MAX_ROWS);
        assert!(!shown.is_empty());
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "seven keystrokes over 10k candidates took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_command_row_shows_its_description_and_parameter_hints() {
    let remind = Candidate::text("remind", "/remind")
        .with_aside("Reminds someone later")
        .with_command(Command {
            name: "remind".into(),
            app: None,
            params: vec![
                Param {
                    name: "person".into(),
                    label: "Who".into(),
                    kind: ParamKind::Person,
                    required: true,
                    default: None,
                },
                Param {
                    name: "duration".into(),
                    label: "When".into(),
                    kind: ParamKind::Duration,
                    required: true,
                    default: None,
                },
            ],
            destructive: false,
        });
    let row = Entry::Item {
        candidate: remind,
        matched: Vec::new(),
    }
    .suggestion();
    assert_eq!(
        row.aside.as_deref(),
        Some("Reminds someone later  person duration")
    );
}
