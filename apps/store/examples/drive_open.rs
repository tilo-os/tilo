//! Headless driver for the store's action pipeline — exercises the
//! *real* typed lifecycle path (`store.app` action → lifecycle queue →
//! `actions::execute`) without a compositor.
//!
//! Used by `verify-open-flow.sh` to prove, against a real
//! `tilo-install` + a real `.tapp` served over HTTP, that:
//!   * clicking **Open** on a Tilo card fetches the package on demand
//!     and installs (caches) it — no separate "Install" step; and
//!   * clicking **Mark installed** pins it.
//!
//! Env:
//!   `TILO_STORE_ITEM_ID`   reverse-DNS package id (default so.tilo.hello)
//!   `TILO_STORE_ITEM_URL`   http(s) URL of the .tapp (required for `open`)
//!   `TILO_STORE_ITEM_NAME`  display name (default derived from id)
//! Arg 1: `open` | `mark-installed`
//!
//! Exit code 0 = the driven action's outcome was ok.

use automerge::AutoCommit;
use ui::tilo_actions::{InvocationId, OperationKind};
use ui::widget::Widget;

use store::StoreApp;
use store::model::{
    ActionKind, InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust,
};

#[allow(
    clippy::too_many_lines,
    reason = "this intentionally linear headless driver documents and verifies the complete visible action-to-worker contract in one place"
)]
fn main() {
    let action = std::env::args().nth(1).unwrap_or_else(|| "open".into());
    let id = std::env::var("TILO_STORE_ITEM_ID").unwrap_or_else(|_| "so.tilo.hello".into());
    let name = std::env::var("TILO_STORE_ITEM_NAME").unwrap_or_else(|_| "Hello".into());
    let url = std::env::var("TILO_STORE_ITEM_URL").unwrap_or_default();

    // The item as the store would render it, from a registry entry.
    let installed = if action == "mark-installed" {
        InstalledState::Installed {
            version: "0.1.0".into(),
            available: "0.1.0".into(),
            pinned: false,
            // The driver represents an app added by the current user, so it
            // exercises the same pin control the Store exposes for a
            // user-managed cache rather than the system-provided variant.
            user_managed: true,
        }
    } else {
        InstalledState::Not
    };
    let item = StoreItem {
        id: id.clone(),
        name,
        description: "driven by verify-open-flow.sh".into(),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: vec!["System".into()],
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: "local test registry".into(),
        },
        trust: StoreTrust::PublisherSigned,
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

    // Drive the exact resource action the visible button and an agent use.
    // A click never becomes a transient CRDT edge: the action host validates
    // the current item and queues one lifecycle operation for the worker.
    let (pact, action_code) = match action.as_str() {
        "open" => (ActionKind::Open, "open"),
        "mark-installed" => (ActionKind::MarkInstalled, "pin"),
        other => {
            eprintln!("unknown action: {other}");
            std::process::exit(2);
        }
    };
    let mut doc = AutoCommit::new();
    let invocation = InvocationId::new();
    let payload =
        serde_json::to_vec(&vec![id.clone(), action_code.to_string()]).unwrap_or_default();
    if app
        .start_operation(
            &ui::widget::ResourceOperation::local(
                invocation,
                OperationKind::Effect,
                "store.app",
                &payload,
            ),
            &mut doc,
            &automerge::ROOT,
        )
        .is_err()
    {
        eprintln!("typed Store action was refused");
        std::process::exit(3);
    }
    let pending = app.take_lifecycle_actions();
    println!("driven action → lifecycle operations: {pending:?}");
    let Some((pid, queued_action, queued_invocation)) = pending.into_iter().next() else {
        eprintln!("no lifecycle operation produced — action wiring is broken");
        std::process::exit(3);
    };
    assert_eq!(pid, id, "pending action targets the clicked item");
    assert_eq!(
        queued_action, pact,
        "lifecycle action preserves the resource action"
    );
    assert_eq!(
        queued_invocation, invocation,
        "worker completion is tied to the action invocation"
    );
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

    // Execute the action for real (download → tilo-install install →
    // run, or tilo-install pin), exactly as the worker thread would.
    let staging = std::env::temp_dir().join("tilo-store-drive-staged");
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
