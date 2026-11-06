//! The record's export: what the document host calls to open, read and change the tracker.
//!
//! `docs/architecture/apps.md` §1 makes an app's model and verbs a WebAssembly module with no
//! imports beyond the host's. This is the tracker's entry point in it: the host finds it by the
//! record's kind ([`tilo_model_abi::export`]) and it answers every operation the host asks for,
//! through [`tilo_model_guest::serve`].

use crate::model::Tracker;

tilo_model_guest::serve_record!(Tracker, record_tasks_tracker);

#[cfg(test)]
mod tests {
    use tilo_model_abi::export;

    #[test]
    fn the_export_is_the_one_the_host_looks_for() {
        assert_eq!(export(crate::model::KIND), "record_tasks_tracker");
    }
}
