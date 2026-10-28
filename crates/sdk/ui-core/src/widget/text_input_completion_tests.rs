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
    assert_eq!(input.value(), "hi @Ada Lovelace");
    assert!(
        !input.is_completion_open(),
        "the accepted mention is not a token"
    );
    type_text(&mut input, " and more");
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
