//! Importing from another browser, as its own library in the runtime bundle
//! (`libtilo_browser_import_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
#[cfg(feature = "thin")]
extern crate tilo_accounts_engine;

#[cfg(feature = "thin")]
pub use browser_continuity;
#[cfg(feature = "thin")]
pub use tilo_browser_sessions;

/// Run the engine's own code: write a Chromium history database with one web
/// page and one local file, and read it back through the capture, which must
/// keep the page and skip the file.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use browser_continuity::sources::{GrantedProfile, parse_chromium_history};

    let profile = tempfile::tempdir().map_err(|error| format!("no scratch profile: {error}"))?;
    {
        let database = rusqlite::Connection::open(profile.path().join("History"))
            .map_err(|error| format!("SQLite could not create a database: {error}"))?;
        database
            .execute_batch(
                "CREATE TABLE urls (url TEXT, title TEXT, last_visit_time INTEGER, visit_count INTEGER);
                 INSERT INTO urls VALUES ('https://example.org/', 'Example', 13350000000000000, 3);
                 INSERT INTO urls VALUES ('file:///etc/passwd', 'Local', 13350000000000001, 1);",
            )
            .map_err(|error| format!("SQLite could not write the history: {error}"))?;
    }
    let directory = std::fs::File::open(profile.path())
        .map_err(|error| format!("could not open the scratch profile: {error}"))?;
    let granted = GrantedProfile::from_directory(directory)
        .map_err(|error| format!("the scratch profile is not grantable: {error}"))?;
    let batch = parse_chromium_history(&granted)
        .map_err(|error| format!("the capture could not read the history: {error}"))?;
    match batch.history.as_slice() {
        [page] if page.url == "https://example.org/" && page.visit_count == 3 => Ok(format!(
            "read 1 page of a Chromium history through SQLite {}",
            rusqlite::version()
        )),
        other => Err(format!(
            "the capture read {} pages, not the one web page",
            other.len()
        )),
    }
}
