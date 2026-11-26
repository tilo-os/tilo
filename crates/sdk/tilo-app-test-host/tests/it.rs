#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "attached_view.rs"]
pub mod attached_view;
#[path = "chain_list.rs"]
pub mod chain_list;
#[path = "child_resources.rs"]
pub mod child_resources;
#[path = "confirmation.rs"]
pub mod confirmation;
#[path = "direct_resources.rs"]
pub mod direct_resources;
#[path = "effects.rs"]
pub mod effects;
#[path = "framework.rs"]
pub mod framework;
#[path = "gestures.rs"]
pub mod gestures;
#[path = "history_actions.rs"]
pub mod history_actions;
#[path = "identity.rs"]
pub mod identity;
#[path = "item_effects.rs"]
pub mod item_effects;
#[path = "kinds.rs"]
pub mod kinds;
#[path = "launch.rs"]
pub mod launch;
#[path = "live_permissions.rs"]
pub mod live_permissions;
#[path = "live_rows.rs"]
pub mod live_rows;
#[path = "model_contract.rs"]
pub mod model_contract;
#[path = "other_resources.rs"]
pub mod other_resources;
#[path = "outbox.rs"]
pub mod outbox;
#[path = "person_resources.rs"]
pub mod person_resources;
#[path = "row_effects.rs"]
pub mod row_effects;
#[path = "settings.rs"]
pub mod settings;
#[path = "share_links.rs"]
pub mod share_links;
#[path = "support/checklist.rs"]
pub mod support_checklist;
// A collaboration host that keeps each channel's record as the device's store does, for the
// tests that drive the toolkit's collaboration over real sockets.
#[cfg(unix)]
#[path = "coedit_keystone.rs"]
pub mod coedit_keystone;
#[cfg(unix)]
#[path = "large_document_transfer.rs"]
pub mod large_document_transfer;
#[cfg(unix)]
#[path = "real_socket_n_peer.rs"]
pub mod real_socket_n_peer;
#[cfg(unix)]
#[path = "support/collab.rs"]
pub mod support_collab;
#[path = "surface_semantics.rs"]
pub mod surface_semantics;
#[path = "unreadable.rs"]
pub mod unreadable;
#[path = "workspace_arrival.rs"]
pub mod workspace_arrival;
#[path = "workspace_resources.rs"]
pub mod workspace_resources;
