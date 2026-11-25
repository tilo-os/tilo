//! YouTube's connector, as its own library in the runtime bundle
//! (`libtilo_youtube_engine.so`), opened by the process the system runs an
//! app's YouTube connector in.
//!
//! An app's types of the `youtube` kinds are kept in step with YouTube by its
//! connector, the one place the service is asked: it holds the network and the
//! client, which the process that runs the app's instances has neither of. The
//! system starts the connector as the app's own launch with no window, and that
//! process opens this library the first time it runs as YouTube's connector.
//! The process every app shares maps none of it.
//!
//! Outside the thin build this crate is an empty library.

// "YouTube" is the service's own name, as its product writes it, not code.
#![allow(
    clippy::doc_markdown,
    reason = "YouTube is a name, not code: it reads as YouTube in prose, not as `YouTube`"
)]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
mod connector {
    use std::sync::Arc;

    use tilo_engine_loader::ConnectorApi;

    /// The name of the origin this connector serves, as the launcher names it
    /// (`TILO_TILOLANG_CONNECTOR`).
    pub const ORIGIN: &str = "youtube";

    /// Run YouTube's connector on the socket the launcher handed this process:
    /// every instance that attaches has its questions answered by the live
    /// service, until the session closes the socket.
    fn run() -> Result<(), String> {
        let service: Arc<dyn tilolang_youtube::Service> = Arc::new(tilolang_youtube::Live::new());
        tilolang_youtube::connector::run(&service)
    }

    /// The one connector this library answers with.
    static API: ConnectorApi = ConnectorApi {
        origin: ORIGIN,
        run,
    };

    /// The entry the connector's process opens this library for.
    #[unsafe(no_mangle)]
    pub fn tilo_connector_entry() -> &'static ConnectorApi {
        &API
    }

    /// Proves this connector loads and runs against the runtime it is built
    /// with: the origin it serves is the one a launch names, and the client it
    /// asks needs no socket to be built.
    pub fn self_check() -> Result<&'static str, String> {
        if ORIGIN != tilolang_youtube::connector::ORIGIN {
            return Err(format!(
                "this library serves `{ORIGIN}`, not the connector's own origin"
            ));
        }
        Ok("serves the YouTube origin")
    }
}

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_youtube_engine_check() -> bool {
    match connector::self_check() {
        Ok(report) => {
            println!("tilo-youtube-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-youtube-engine: {error}");
            false
        }
    }
}
