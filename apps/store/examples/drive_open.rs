//! Headless driver for the store's action pipeline — exercises the
//! *real* click path (CRDT edge → `StoreApp::sync` → pending action →
//! `actions::execute`) without a compositor.
//!
//! Used by `verify-open-flow.sh` to prove, against a real
//! `qos-install` + a real `.qpkg` served over HTTP, that:
//!   * clicking **Open** on a qOS card fetches the package on demand
//!     and installs (caches) it — no separate "Install" step; and
//!   * clicking **Mark installed** pins it.
//!
//! Env:
//!   `QOS_STORE_ITEM_ID`   reverse-DNS package id (default dev.qos.hello)
//!   `QOS_STORE_ITEM_URL`   http(s) URL of the .qpkg (required for `open`)
//!   `QOS_STORE_ITEM_NAME`  display name (default derived from id)
//! Arg 1: `open` | `mark-installed`
//!
//! Exit code 0 = the driven action's outcome was ok.

use automerge::transaction::Transactable;
use ui::widget::Widget;

use store::StoreApp;
use store::model::{ActionKind, InstalledState, StoreItem, StoreItemVersion, StoreSource};

fn main() {
    let action = std::env::args().nth(1).unwrap_or_else(|| "open".into());
    let id = std::env::var("QOS_STORE_ITEM_ID").unwrap_or_else(|_| "dev.qos.hello".into());
    let name = std::env::var("QOS_STORE_ITEM_NAME").unwrap_or_else(|_| "Hello".into());
    let url = std::env::var("QOS_STORE_ITEM_URL").unwrap_or_default();

    // The item as the store would render it, from a registry entry.
    let installed = if action == "mark-installed" {
        InstalledState::Installed {
            version: "0.1.0".into(),
            available: "0.1.0".into(),
            pinned: false,
        }
    } else {
        InstalledState::Not
    };
    let item = StoreItem {
        id: id.clone(),
        name,
        description: "driven by verify-open-flow.sh".into(),
        categories: vec!["System".into()],
        icon_url: None,
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Qos {
            registry_name: "local test registry".into(),
        },
        versions: vec![StoreItemVersion {
            version: "0.1.0".into(),
            released_at_unix: 0,
            download_url: if url.is_empty() { None } else { Some(url) },
            root_hash_hex: None,
            size_bytes: 0,
        }],
        publisher_pubkey_hex: Some("aa".repeat(32)),
        installed,
    };

    // Build the real widget with this catalog.
    let mut app = StoreApp::new();
    app.set_items(vec![item.clone()]);

    // Simulate the click by writing the one-shot CRDT edge the button
    // widget writes, exactly as a live click would, then run the
    // widget's real `sync` so `drain_click_keys` turns it into a
    // pending action.
    let mut state = ui::State::new();
    let key = match action.as_str() {
        "open" => format!("store.action_click.{id}"),
        "mark-installed" => format!("store.pin_click.{id}"),
        other => {
            eprintln!("unknown action: {other}");
            std::process::exit(2);
        }
    };
    if let Err(e) = state.doc.put(&state.root, key.as_str(), true) {
        eprintln!("failed to set click edge: {e}");
        std::process::exit(4);
    }

    app.sync(&mut state.doc, &state.root);
    let pending = app.take_pending_actions();
    println!("driven click → pending actions: {pending:?}");
    let Some((pid, pact)) = pending.into_iter().next() else {
        eprintln!("no pending action produced — click wiring is broken");
        std::process::exit(3);
    };
    assert_eq!(pid, id, "pending action targets the clicked item");
    if action == "open" {
        assert_eq!(
            pact,
            ActionKind::Open,
            "primary click resolved to Open (not Install)"
        );
    } else {
        assert_eq!(
            pact,
            ActionKind::MarkInstalled,
            "pin click resolved to MarkInstalled"
        );
    }

    // Execute the action for real (download → qos-install install →
    // run, or qos-install pin), exactly as the worker thread would.
    let staging = std::env::temp_dir().join("qos-store-drive-staged");
    let _ = std::fs::create_dir_all(&staging);
    let outcome = store::actions::execute(&item, pact, &staging);
    println!(
        "execute outcome: ok={} status={:?}",
        outcome.ok, outcome.status_line
    );
    if outcome.ok {
        std::process::exit(0);
    } else {
        // A launch failure after a successful fetch/cache is still a
        // useful signal; the script inspects the registry to decide.
        std::process::exit(1);
    }
}
