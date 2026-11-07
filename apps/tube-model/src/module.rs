//! Tube's records' export: what the document host calls to open, read and change the library and
//! the watch history.
//!
//! `docs/architecture/apps.md` §1 makes an app's model and verbs a WebAssembly module with no
//! imports beyond the host's. This is Tube's entry point in it: the host finds each export by the
//! record's kind ([`tilo_model_guest::abi::export`]) and it answers every operation the host asks
//! for, through [`tilo_model_guest::serve`].

use crate::{Library, WatchHistory};

tilo_model_guest::serve_record!(Library, record_tube_library);
tilo_model_guest::serve_record!(WatchHistory, record_tube_watch_history);

#[cfg(test)]
mod tests {
    use tilo_model_guest::abi::export;
    use tilo_resource::Resource;

    #[test]
    fn the_exports_are_the_ones_the_host_looks_for() {
        assert_eq!(export(Library::KIND), "record_tube_library");
        assert_eq!(export(WatchHistory::KIND), "record_tube_watch_history");
    }
}
