//! A document that does not read as a music library: the library the window
//! holds stays searchable and playable, and only a read that succeeds
//! replaces it.

use super::tests::app_with_doc;
use crate::seed;
use automerge::transaction::Transactable as _;

#[test]
fn a_library_that_does_not_read_is_not_an_empty_one() {
    let (mut app, mut doc) = app_with_doc();
    let tracks = app.lib.tracks.len();
    let playlists = app.lib.playlists().count();
    assert!(
        tracks > 0 && playlists > 0,
        "the sample library is populated"
    );

    // A document a newer or broken writer left behind: the tracks' field is
    // no longer a map of records.
    doc.put(automerge::ROOT, "tracks", "not a map of tracks")
        .expect("the field is written");
    app.reload_library(&doc);

    assert!(app.library_read.failed(), "the library does not read");
    assert_eq!(
        app.lib.tracks.len(),
        tracks,
        "the library that last read whole stays searchable and playable"
    );
    assert_eq!(app.stored_library.tracks.len(), tracks);
    assert_eq!(
        app.lib.playlists().count(),
        playlists,
        "its playlists stay, not an empty list"
    );

    // An empty library that this app's version reads is a library with
    // nothing in it, not a failure: the window shows it empty.
    let empty = seed::empty_document();
    app.reload_library(&empty);
    assert!(!app.library_read.failed(), "an empty library is valid");
    assert!(app.lib.tracks.is_empty());
}
