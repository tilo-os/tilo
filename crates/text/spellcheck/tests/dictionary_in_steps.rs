//! A program without threads (a browser page) builds the English dictionary
//! in steps in its idle time: no check waits for it, and the fields check
//! again once it is built.
#![cfg(not(feature = "lazy-dictionaries"))]

use spellcheck::{
    build_dictionaries_in_steps, dictionaries_generation, prepare_dictionaries, spans_for_language,
    suggestions_for_language,
};

#[test]
fn the_english_dictionary_builds_in_idle_steps_and_no_check_waits_for_it() {
    build_dictionaries_in_steps();
    assert!(
        spans_for_language("en-US", "teh cat").is_empty(),
        "a check before the dictionary is built flags nothing rather than building it"
    );
    assert!(suggestions_for_language("en-US", "teh", 3).is_empty());
    let generation = dictionaries_generation();

    // Each idle slice allows a couple of asks of the budget; the build takes
    // many slices.
    let mut slices = 0;
    loop {
        slices += 1;
        let mut asks = 0;
        let done = prepare_dictionaries(&mut || {
            asks += 1;
            asks < 2
        })
        .expect("the dictionaries' code is linked in");
        if done {
            break;
        }
        assert_eq!(
            dictionaries_generation(),
            generation,
            "nothing is announced half built"
        );
        assert!(slices < 10_000, "the build ends");
    }
    assert!(slices > 10, "the build was sliced: {slices} slices");
    assert_eq!(
        dictionaries_generation(),
        generation + 1,
        "fields check again once it is built"
    );
    assert_eq!(spans_for_language("en-US", "teh cat"), vec![(0, 3)]);
    assert!(
        suggestions_for_language("en-US", "teh", 3)
            .iter()
            .any(|word| word == "the")
    );
    assert_eq!(
        prepare_dictionaries(&mut || false),
        Some(true),
        "built once"
    );
}
