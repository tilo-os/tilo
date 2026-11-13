use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

static FETCHES: AtomicUsize = AtomicUsize::new(0);

/// The engine's code is fetched as the palette opens, not on the first query that needs it.
///
/// Where the engine is not linked in — the browser desktop fetches it as a module of its own —
/// a search that fetches it on its query has nothing to show until the module lands, and says
/// it is still searching for that whole time: on a cold page that is the tens of seconds
/// between typing and the first row. The palette opening is the moment the person asked for
/// search, and the time before their first key is theirs to spend fetching it.
///
/// Its own process: the engine's once-only state is global, so the fetch count here is the
/// process's.
#[test]
fn opening_the_palette_fetches_the_search_engine() {
    ui::search::defer_engine(|| {
        FETCHES.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(FETCHES.load(Ordering::SeqCst), 0, "nothing asked yet");
    assert!(
        ui::search::engine_ready(),
        "no fetch was asked for, so nothing waits on the engine"
    );

    let mut search = DesktopSearch::new();
    search.open(super::tests::data());
    assert_eq!(
        FETCHES.load(Ordering::SeqCst),
        1,
        "opening the palette fetches the engine, before a key is typed"
    );
    assert!(
        !ui::search::engine_ready(),
        "the palette opening waits on the engine's code, so the first key is answered with it"
    );

    // A second open does not fetch again: the loader runs once until the engine is installed.
    search.open(super::tests::data());
    assert_eq!(
        FETCHES.load(Ordering::SeqCst),
        1,
        "the engine is asked for once"
    );
}
