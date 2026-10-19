//! With the engine fetched on first use (`lazy-engine`): a small index
//! answers without it and never asks for it; a large one asks, finds
//! nothing until it is installed, and then answers with every record.
//! Its own process, so the engine's statics start empty.
#![cfg(feature = "lazy-engine")]

use std::sync::atomic::{AtomicUsize, Ordering};

use tilo_search::{
    Document, FieldWeight, Page, SearchIndex, engine_generation, engine_ready, install_engine,
    tantivy_engine,
};

static FETCHES: AtomicUsize = AtomicUsize::new(0);

fn fetch() {
    FETCHES.fetch_add(1, Ordering::SeqCst);
}

fn rows(count: u64) -> impl Iterator<Item = Document> {
    (0..count).map(|key| Document::new(key).field(format!("row {key}"), FieldWeight::TITLE))
}

#[test]
fn only_a_large_index_waits_for_the_engine() {
    tilo_search::defer_engine(fetch);

    let mut small = SearchIndex::default();
    small.replace_all(rows(10));
    small.commit().unwrap();
    assert!(!small.is_dirty());
    assert_eq!(small.search("row 7", Page::first(5)).hits[0].key, 7);
    assert_eq!(
        FETCHES.load(Ordering::SeqCst),
        0,
        "a small index fetches nothing"
    );
    assert!(engine_ready());

    let mut large = SearchIndex::default();
    large.replace_all(rows(2000));
    large.commit().unwrap();
    assert_eq!(
        FETCHES.load(Ordering::SeqCst),
        1,
        "a large index fetches the engine"
    );
    assert!(!engine_ready());
    assert!(large.search("row 1999", Page::first(5)).hits.is_empty());
    assert_eq!(FETCHES.load(Ordering::SeqCst), 1, "one fetch at a time");

    let generation = engine_generation();
    install_engine(tantivy_engine);
    assert!(engine_ready());
    assert_eq!(engine_generation(), generation + 1);
    assert_eq!(large.search("row 1999", Page::first(5)).hits[0].key, 1999);
    assert_eq!(small.search("row 3", Page::first(5)).hits[0].key, 3);
}
