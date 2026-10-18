//! The search layer's engine, as an engine library beside the shared runtime
//! (`libtilo_search_engine.so`).
//!
//! tantivy (with its stemmers' tables, its codecs and its query machinery)
//! indexes and searches a `SearchIndex` too large to answer from its records.
//! Most of what apps search is short lists (a palette's actions, its windows,
//! Settings' pages), which the search layer answers itself; only a large
//! index (a file listing, a mailbox, body text) needs the engine. Inside
//! `libruntime_dylib.so` every app process mapped and relocated it; here the
//! search layer opens it the first time such an index commits or is searched
//! (tilo-engine-loader), and no app names it.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides. Outside
//! the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_search_tantivy;

/// The entry the search layer opens (`tilo_search::ENGINE_ENTRY`): the
/// engine. Its type is checked here against `tilo_search::EngineEntry`.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub fn tilo_search_engine_entry() -> tilo_search::MakeEngine {
    tilo_search_tantivy::tantivy_engine
}

#[cfg(feature = "thin")]
const _: tilo_search::EngineEntry = tilo_search_engine_entry;

/// Run the engine's own code the way an app reaches it: an index with body
/// text (which the search layer always gives the engine) commits, which
/// opens this library by name, and a word of one body finds that record.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with, and that the runtime finds it.
///
/// # Errors
/// Names what the engine indexed or found wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use std::sync::Arc;

    use tilo_search::{BodyPiece, BodySource, Document, FieldWeight, Page, SearchIndex};

    struct Body(&'static str);

    impl BodySource for Body {
        fn segment_count(&self) -> usize {
            1
        }

        fn piece_count(&self, _segment: usize) -> usize {
            1
        }

        fn piece(&self, _segment: usize, piece: usize) -> Option<BodyPiece<'_>> {
            (piece == 0).then_some(BodyPiece {
                text: self.0,
                space_before: false,
            })
        }
    }

    let mut index = SearchIndex::default();
    index.upsert(
        Document::new(1)
            .field("Harbour notes", FieldWeight::TITLE)
            .body(Arc::new(Body("the ferry leaves at noon"))),
    );
    index.upsert(
        Document::new(2)
            .field("Groceries", FieldWeight::TITLE)
            .body(Arc::new(Body("oranges and bread"))),
    );
    index.commit().map_err(|error| error.to_string())?;
    let found: Vec<u64> = index
        .search("ferry", Page::first(10))
        .hits
        .iter()
        .map(|hit| hit.key)
        .collect();
    if found.is_empty() {
        return Err(format!(
            "\"ferry\" found nothing: the search layer did not open {}, or it indexed nothing",
            tilo_search::ENGINE_LIBRARY
        ));
    }
    if found != [1] {
        return Err(format!("\"ferry\" found {found:?}, not [1]"));
    }
    Ok(format!(
        "an index of {} records searched, {} bytes",
        index.len(),
        index.index_bytes()
    ))
}
