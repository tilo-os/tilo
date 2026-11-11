//! The record's export: what the document host calls to open, read and change which runs Console
//! shows.
//!
//! `docs/architecture/apps.md` §1 makes an app's model and verbs a WebAssembly module with no
//! imports beyond the host's. This is Console's entry point in it: the host finds it by the
//! record's kind ([`tilo_model_guest::abi::export`]) and it answers every operation the host asks
//! for, through [`tilo_model_guest::serve`].

use crate::Console;

tilo_model_guest::serve_record!(Console, record_console_view);

#[cfg(test)]
mod tests {
    use tilo_model_guest::abi::export;
    use tilo_resource::Resource;

    #[test]
    fn the_export_is_the_one_the_host_looks_for() {
        assert_eq!(export(crate::Console::KIND), "record_console_view");
    }
}
