//! `collab-broker` — the production fan-out server for
//! `QUICK_COLLAB_SOCK`.
//!
//! Apps built on `ui::App` connect to the socket named by
//! `QUICK_COLLAB_SOCK` and speak the Plane-A protocol
//! (`ui::collaboration::{CollabRequest, CollabEvent}`). Until now only
//! the test relay bound that socket; this binary serves it for real, so
//! two app instances on one machine collaborate:
//!
//! ```text
//! collab-broker /tmp/notes.sock &
//! QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=alice notes &
//! QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=bob   notes
//! ```
//!
//! All relay logic lives in `ui::collaboration::broker` so it can be
//! unit-tested from the `ui` crate's own test suite.

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: collab-broker <socket-path>");
            std::process::exit(2);
        }
    };

    eprintln!("collab-broker: listening on {path}");
    if let Err(e) = ui::collaboration::broker::serve_path(&path) {
        eprintln!("collab-broker: failed to serve {path}: {e}");
        std::process::exit(1);
    }
}
