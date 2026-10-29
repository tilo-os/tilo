//! What a field completes as a person types: the emoji a `:name:` names and
//! the person an `@name` names, and the fields that offer neither.

use super::*;

fn focused_with_people(people: Vec<MentionCandidate>) -> TextInput {
    let mut input = TextInput::local().with_mention_candidates(people);
    input.set_focus(true);
    input
}

fn type_text(input: &mut TextInput, text: &str) {
    let mut document = AutoCommit::new();
    for character in text.chars() {
        let event = crate::synthetic_input::char_key(&character.to_string());
        drive(input, &mut document, &event, Modifiers::default());
    }
}

fn press_named(input: &mut TextInput, key: NamedKey) -> bool {
    let mut document = AutoCommit::new();
    drive(
        input,
        &mut document,
        &crate::synthetic_input::named_key(key),
        Modifiers::default(),
    )
}

fn ada() -> Vec<MentionCandidate> {
    vec![MentionCandidate::new("u_ada", "Ada Lovelace", false)]
}

#[test]
fn a_prose_field_offers_emoji_and_mentions_by_default() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(ada());
    type_text(&mut input, "hi @ad");
    assert!(input.is_completion_open(), "a mention offers the roster");
    assert_eq!(input.completion.rows()[0].value, "@Ada Lovelace");
    assert!(press_named(&mut input, NamedKey::Enter));
    assert_eq!(
        input.value(),
        "hi @Ada Lovelace ",
        "the mention, then a space to type on after"
    );
    assert!(
        !input.is_completion_open(),
        "the accepted mention is not a token"
    );
    type_text(&mut input, "and more");
    assert!(
        !input.is_completion_open(),
        "typing after an accepted mention does not offer it again"
    );

    let mut input = focused_with_people(Vec::new());
    type_text(&mut input, "pizza :piz");
    assert!(
        input.is_completion_open(),
        "a shortcode offers the catalogue"
    );
    assert_eq!(input.completion.rows()[0].value, "🍕");
    assert!(press_named(&mut input, NamedKey::Tab));
    assert_eq!(input.value(), "pizza 🍕");
}

#[test]
fn plain_and_either_single_opt_out_suppress_what_they_name() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for (setting, mentions, emoji) in [
        (TextCompletion::PLAIN, false, false),
        (TextCompletion::EMOJI, false, true),
        (TextCompletion::MENTIONS, true, false),
    ] {
        let mut input = focused_with_people(ada());
        input.set_completion(setting);
        type_text(&mut input, "hi @ad");
        assert_eq!(
            input.is_completion_open(),
            mentions,
            "{setting:?} and mentions"
        );

        let mut input = focused_with_people(ada());
        input.set_completion(setting);
        type_text(&mut input, "pizza :piz");
        assert_eq!(input.is_completion_open(), emoji, "{setting:?} and emoji");
    }
}

#[test]
fn a_search_secret_or_numeric_field_offers_nothing() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for mut input in [
        focused_with_people(ada()).with_kind(TextInputKind::Search),
        focused_with_people(ada())
            .with_kind(TextInputKind::Password)
            .with_completion(TextCompletion::ALL),
        focused_with_people(ada())
            .with_kind(TextInputKind::Secret)
            .with_completion(TextCompletion::ALL),
        focused_with_people(ada())
            .with_secret(true)
            .with_completion(TextCompletion::ALL),
        focused_with_people(ada())
            .with_kind(TextInputKind::Number)
            .with_completion(TextCompletion::ALL),
        focused_with_people(ada())
            .with_kind(TextInputKind::Pin)
            .with_completion(TextCompletion::ALL),
    ] {
        let kind = input.kind();
        type_text(&mut input, "hi @ad");
        assert!(!input.is_completion_open(), "{kind:?} offers no mention");
        type_text(&mut input, " :piz");
        assert!(!input.is_completion_open(), "{kind:?} offers no emoji");
    }
}

#[test]
fn a_composing_field_offers_nothing() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(ada());
    type_text(&mut input, "hi @ad");
    assert!(input.is_completion_open());
    let mut document = AutoCommit::new();
    drive(
        &mut input,
        &mut document,
        &WindowEvent::Ime(winit::event::Ime::Preedit("a".to_owned(), None)),
        Modifiers::default(),
    );
    assert!(
        !input.is_completion_open(),
        "the input method owns the keys the list would take"
    );
}

#[test]
fn escape_dismisses_a_mention_and_a_new_token_opens_again() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(ada());
    type_text(&mut input, "hi @ad");
    assert!(press_named(&mut input, NamedKey::Escape));
    assert!(!input.is_completion_open());
    type_text(&mut input, "a");
    assert!(!input.is_completion_open(), "the dismissed @ stays closed");
    type_text(&mut input, " @a");
    assert!(input.is_completion_open(), "a new @ opens it again");
}

use crate::widget::completion::{Candidate, CompletionSource, Entry, SlowTier, Tier};

fn channels() -> CompletionSource {
    CompletionSource::new(
        '#',
        vec![Tier::new(vec![
            Candidate::reference("general", "#general", "channel:general").with_aside("Anything"),
            Candidate::reference("design", "#design", "channel:design"),
            Candidate::reference("dev-ops", "#dev-ops", "channel:devops"),
        ])],
    )
    .with_create("Create channel")
}

fn people_source() -> CompletionSource {
    CompletionSource::new(
        '@',
        vec![
            Tier::new(vec![Candidate::reference("Mia Chen", "@Mia Chen", "u_mia")])
                .with_header("In this conversation"),
            Tier::new(vec![
                Candidate::reference("Milan Novak", "@Milan Novak", "u_milan"),
                Candidate::reference("Leo Park", "@Leo Park", "u_leo"),
            ])
            .with_header("Members"),
        ],
    )
}

fn labels(input: &TextInput) -> Vec<String> {
    input
        .completion
        .rows()
        .iter()
        .map(|row| row.label.clone())
        .collect()
}

#[test]
fn the_views_source_opens_on_the_trigger_and_narrows_per_keystroke_in_the_field() {
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(ada());
    input.set_completion_sources(vec![people_source(), channels()]);
    type_text(&mut input, "@");
    assert!(input.is_completion_open(), "it opens on the trigger itself");
    assert_eq!(
        labels(&input),
        [
            "In this conversation",
            "Mia Chen",
            "Members",
            "Milan Novak",
            "Leo Park"
        ],
        "the view's source replaces the roster for @"
    );
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("u_mia"),
        "the first row starts selected"
    );
    type_text(&mut input, "mi");
    assert_eq!(
        labels(&input),
        ["Mia Chen", "Milan Novak"],
        "one list, no headers"
    );
    assert_eq!(
        input.pending_token(),
        Some(0..3),
        "the query stays in the field as the token"
    );
    type_text(&mut input, "l");
    assert_eq!(labels(&input), ["Milan Novak"]);
}

#[test]
fn arrows_wrap_page_keys_move_and_tab_inserts_the_reference_and_a_space() {
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(Vec::new());
    input.set_completion_sources(vec![channels()]);
    let inserted = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&inserted);
    input.set_on_reference_inserted(move |_, range, reference| {
        seen.borrow_mut().push((range, reference.to_owned()));
    });
    type_text(&mut input, "see #");
    assert!(
        press_named(&mut input, NamedKey::ArrowUp),
        "up from the first wraps"
    );
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("channel:devops")
    );
    assert!(press_named(&mut input, NamedKey::ArrowDown));
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("channel:general")
    );
    assert!(press_named(&mut input, NamedKey::PageDown));
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("channel:devops")
    );
    assert!(press_named(&mut input, NamedKey::PageUp));
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("channel:general")
    );
    assert!(press_named(&mut input, NamedKey::Tab));
    assert_eq!(input.value(), "see #general ");
    assert!(!input.is_completion_open());
    assert_eq!(
        *inserted.borrow(),
        [(4..12, "channel:general".to_owned())],
        "a rich field marks the inserted run as the reference"
    );
}

#[test]
fn escape_closes_and_leaves_the_text_and_leaving_the_token_closes() {
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(Vec::new());
    input.set_completion_sources(vec![channels()]);
    type_text(&mut input, "#des");
    assert!(input.is_completion_open());
    assert!(press_named(&mut input, NamedKey::Escape));
    assert!(!input.is_completion_open());
    assert_eq!(input.value(), "#des", "Escape keeps what was typed");
    type_text(&mut input, "ign");
    assert!(
        !input.is_completion_open(),
        "the dismissed token stays closed"
    );

    let mut input = focused_with_people(Vec::new());
    input.set_completion_sources(vec![channels()]);
    type_text(&mut input, "#de");
    assert!(input.is_completion_open());
    press_named(&mut input, NamedKey::Home);
    assert!(!input.is_completion_open(), "the caret left the token");
}

#[test]
fn no_match_says_so_offers_create_and_a_space_then_closes() {
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(Vec::new());
    input.set_completion_sources(vec![channels()]);
    let created = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&created);
    input.set_on_completion_create(move |trigger, name| {
        seen.borrow_mut().push((trigger, name.to_owned()));
    });
    type_text(&mut input, "#launch");
    let rows = labels(&input);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].contains("launch"),
        "a notice names the query: {rows:?}"
    );
    assert_eq!(rows[1], "Create channel \u{201c}launch\u{201d}");
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("\u{0}create"),
        "the notice is passed over; Create is selected"
    );
    assert!(press_named(&mut input, NamedKey::Enter));
    assert_eq!(*created.borrow(), [('#', "launch".to_owned())]);

    let mut input = focused_with_people(Vec::new());
    input.set_completion_sources(vec![people_source()]);
    type_text(&mut input, "@zz");
    assert!(input.is_completion_open(), "one row says nothing matches");
    type_text(&mut input, " ");
    assert!(
        !input.is_completion_open(),
        "a space with nothing matching was prose all along"
    );
    type_text(&mut input, "and on");
    assert!(!input.is_completion_open());
}

#[test]
fn a_name_with_a_space_keeps_the_drawer_while_it_matches() {
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(Vec::new());
    input.set_completion_sources(vec![people_source()]);
    type_text(&mut input, "@milan n");
    assert_eq!(labels(&input), ["Milan Novak"]);
    assert!(press_named(&mut input, NamedKey::Enter));
    assert_eq!(input.value(), "@Milan Novak ");
}

#[test]
fn a_slow_source_shows_progress_and_its_rows_arrive_below_without_moving_the_selection() {
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(Vec::new());
    let waiting = people_source().with_slow(SlowTier {
        header: "Outside the workspace".into(),
        candidates: None,
    });
    input.set_completion_sources(vec![waiting]);
    type_text(&mut input, "@mi");
    assert_eq!(
        labels(&input),
        ["Mia Chen", "Milan Novak", "Outside the workspace"]
    );
    press_named(&mut input, NamedKey::ArrowDown);
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("u_milan")
    );
    let arrived = people_source().with_slow(SlowTier {
        header: "Outside the workspace".into(),
        candidates: Some(vec![Candidate::reference(
            "Mika Ross",
            "@Mika Ross",
            "u_mika",
        )]),
    });
    input.set_completion_sources(vec![arrived]);
    assert_eq!(
        labels(&input),
        [
            "Mia Chen",
            "Milan Novak",
            "Outside the workspace",
            "Mika Ross"
        ]
    );
    assert_eq!(
        input.completion.selected().and_then(Entry::key),
        Some("u_milan"),
        "what arrives late never moves the selection"
    );
}

#[test]
fn the_drawer_is_a_list_that_names_its_count_and_selected_row() {
    let _settled = crate::motion::testing::settled();
    let mut input = focused_with_people(Vec::new());
    input.set_completion_sources(vec![channels()]);
    type_text(&mut input, "#d");
    draw_with_theme(&mut input, &Theme::light());
    let node = Widget::accessibility(&input).expect("the field is accessible");
    let drawer = node
        .children
        .iter()
        .find(|child| child.role == crate::a11y::Role::ListBox)
        .expect("the open drawer is a list box");
    assert!(
        drawer
            .name
            .as_deref()
            .is_some_and(|name| name.contains('2')),
        "it names how many results: {:?}",
        drawer.name
    );
    assert_eq!(drawer.description.as_deref(), Some("design"));
    let options: Vec<_> = drawer
        .children
        .iter()
        .filter(|row| row.role == crate::a11y::Role::ListBoxOption)
        .collect();
    assert_eq!(options.len(), 2);
    assert!(options[0].state.selected);
}
