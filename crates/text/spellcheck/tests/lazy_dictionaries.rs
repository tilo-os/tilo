//! With the dictionaries fetched on first use (`lazy-dictionaries`): the
//! first check asks for them once and flags nothing; once installed, the
//! same check flags the misspelled word and suggests its correction.
//! Its own process, so the dictionaries' statics start empty.
#![cfg(feature = "lazy-dictionaries")]

use std::sync::atomic::{AtomicUsize, Ordering};

use spellcheck::{
    builtin_dictionaries, defer_dictionaries, dictionaries_generation, install_dictionaries,
    spans_for_language, suggestions_for_language,
};

static FETCHES: AtomicUsize = AtomicUsize::new(0);

fn fetch() {
    FETCHES.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn checks_flag_words_once_the_dictionaries_are_in() {
    defer_dictionaries(fetch);
    let text = "the quikc fox";
    assert!(spans_for_language("en-US", text).is_empty());
    assert!(spans_for_language("en-US", text).is_empty());
    assert_eq!(FETCHES.load(Ordering::SeqCst), 1, "one fetch at a time");

    let generation = dictionaries_generation();
    install_dictionaries(builtin_dictionaries);
    assert_eq!(dictionaries_generation(), generation + 1);
    assert_eq!(spans_for_language("en-US", text), vec![(4, 9)]);
    assert!(
        suggestions_for_language("en-US", "quikc", 5).contains(&"quick".to_owned()),
        "suggestions come from the dictionaries"
    );
}
