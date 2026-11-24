//! The Tilo web entry's runtime in the browser.
//!
//! The entry pages (`web/index.html`, `web/app/index.html`, `web/sign-in.html`)
//! sign a person in, admit this browser to a workspace as one of its devices
//! and pair it, then hand the browser to the page the server renders of the
//! workspace. This crate is what they call for that: the certified peer and
//! its session with the workspace's host (`peer`), the person's root
//! identity (`root_identity`), the link grammar (`link`), the way in
//! (`way_in`) and the workspaces this browser entered (`entered`), and the
//! store the peer keeps its certificates and outbox in (`peer_store`).
//!
//! Everything is browser-only; native workspace builds compile an empty
//! library.
// The crate's tests are host tests, so its test target builds empty for wasm32.
#![cfg(not(all(target_arch = "wasm32", test)))]
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        clippy::future_not_send,
        reason = "browser futures hold JavaScript handles and run on the page's single thread"
    )
)]

// The workspaces this browser entered, by the rules every device keeps them by.
#[cfg(target_arch = "wasm32")]
mod entered;
#[cfg(target_arch = "wasm32")]
mod invited_server;
#[cfg(target_arch = "wasm32")]
mod link;
// Which peer speaks for the page's person: its tests run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
mod page_person;
#[cfg(target_arch = "wasm32")]
mod peer;
// The page's peer network: the session's sync over this page's endpoint.
#[cfg(target_arch = "wasm32")]
mod peer_sync;
// What the page says of itself, as its live layer hands it back: its tests
// run on the host too.
#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(
        dead_code,
        reason = "only the page's board reads what it says of itself"
    )
)]
mod presence_value;
// A pairing link drawn as the code a phone reads.
#[cfg(target_arch = "wasm32")]
mod qr_code;
// The page's user root: the envelope it is kept in, and what it signs.
#[cfg(target_arch = "wasm32")]
mod root_identity;
// The sign-in route's answer to a service (`web/sign-in.mjs`).
#[cfg(target_arch = "wasm32")]
mod sign_in_page;
// A store's files on a page, committed later and in order.
#[cfg(any(target_arch = "wasm32", test))]
mod page_files;
// What the page keeps in its profile's store, by name.
#[cfg(any(target_arch = "wasm32", test))]
mod peer_files;
// The page's profile stores, over IndexedDB through a worker.
#[cfg(target_arch = "wasm32")]
mod peer_store;
// The accounts a browser's profiles keep, read for the entry page.
#[cfg(target_arch = "wasm32")]
mod browser_accounts;
#[cfg(target_arch = "wasm32")]
mod way_in;
#[cfg(target_arch = "wasm32")]
mod workspaces;
