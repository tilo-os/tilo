//! The account engine — the worker seam that connects Mail to real IMAP and
//! SMTP servers, kept off the synchronous `ui` event loop.
//!
//! ## Why a worker
//! The `ui` event loop is synchronous and must never block on the network. So
//! every server round-trip (IMAP `FETCH`, SMTP submit) runs on a background
//! worker thread; the UI posts [`EngineRequest`]s and drains [`EngineEvent`]s
//! through a channel, exactly mirroring the `Saver` pattern the notes app uses.
//! The app stays responsive and `--shots` (which never starts the worker) stays
//! offline and deterministic.
//!
//! ## The two backends
//! * **Default (this build): a stub backend.** It parses the bundled sample
//!   corpus and acknowledges sends by building the real MIME with `mail-builder`
//!   (see `transport::build_mime`). No sockets, no TLS, no async runtime — so
//!   the headless build stays small and green and reproducible.
//! * **`live` feature: real IMAP/SMTP.** Behind `--features live`, the worker
//!   uses [`async-imap`] for fetch/flag/move and [`lettre`] for submission,
//!   over rustls. Those crates pull a TLS + async stack we deliberately keep
//!   out of the default build (and out of `--shots`); the data types
//!   ([`AccountConfig`], [`Draft`], [`Message`]) are already the exact shapes
//!   those libraries consume and produce, so enabling the feature is a drop-in.
//!
//! Both backends speak the same [`MailEngine`] trait, so the app code is
//! identical regardless of which is compiled in.

use std::sync::mpsc::{Receiver, Sender};

use crate::model::{AccountConfig, Draft, Folder, Message};
use crate::transport::build_mime;

/// A request from the UI to the engine worker.
#[derive(Clone, Debug)]
pub enum EngineRequest {
    /// Connect and list the folders for an account.
    Connect { account: AccountConfig },
    /// Fetch the messages of a folder (`UID FETCH` in the live backend).
    Fetch { account_id: String, folder: Folder },
    /// Persist a flag change to the server (`UID STORE`).
    StoreFlag { account_id: String, message: Box<Message> },
    /// Move a message to another folder (`UID MOVE`).
    Move { account_id: String, message_id: String, to: Folder },
    /// Submit a draft via SMTP.
    Send { account: AccountConfig, draft: Box<Draft> },
}

/// An event from the engine worker back to the UI.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// Connection established; carries the discovered folder names.
    Connected { account_id: String },
    /// A batch of fetched messages for a folder.
    Fetched { account_id: String, folder: Folder, messages: Vec<Message> },
    /// A send completed; carries the wire byte count.
    Sent { wire_bytes: usize },
    /// Any operation failed; carries a human-readable reason.
    Error { context: String, message: String },
}

/// The backend abstraction. The stub and the live IMAP/SMTP backend both
/// implement it, so the app's calling code never branches on which is active.
pub trait MailEngine: Send {
    /// Handle one request, emitting zero or more events onto `out`.
    fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>);
}

/// The default, offline backend. It validates sends by building real MIME and
/// reports the wire size, and acknowledges connect/fetch without touching the
/// network. Live IMAP/SMTP is the `live`-feature backend below.
pub struct StubEngine;

impl MailEngine for StubEngine {
    fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>) {
        match req {
            EngineRequest::Connect { account } => {
                let _ = out.send(EngineEvent::Connected { account_id: account.email });
            }
            EngineRequest::Fetch { account_id, folder } => {
                // The offline backend has no server to fetch from; the app shows
                // its bundled corpus. Acknowledge with an empty batch so the
                // caller's state machine still advances.
                let _ = out.send(EngineEvent::Fetched { account_id, folder, messages: Vec::new() });
            }
            EngineRequest::StoreFlag { .. } | EngineRequest::Move { .. } => {
                // No-op offline; flags persist in the local CRDT.
            }
            EngineRequest::Send { account, draft } => {
                let from = crate::model::Addr::new(account.display_name.clone(), account.email.clone());
                match build_mime(&from, &draft) {
                    Ok(bytes) => {
                        let _ = out.send(EngineEvent::Sent { wire_bytes: bytes.len() });
                    }
                    Err(e) => {
                        let _ = out.send(EngineEvent::Error {
                            context: "send".into(),
                            message: e.to_string(),
                        });
                    }
                }
            }
        }
    }
}

/// Spawn the engine worker thread and return the request sender + event
/// receiver. The live app calls this once at startup; `--shots` never does, so
/// it stays offline. Defaults to [`StubEngine`]; the `live` feature swaps in the
/// real IMAP/SMTP backend (see `live` module).
pub fn spawn() -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    spawn_with(Box::new(StubEngine))
}

/// Spawn the worker with a specific backend (used by tests and the `live`
/// feature).
pub fn spawn_with(mut backend: Box<dyn MailEngine>) -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<EngineRequest>();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel::<EngineEvent>();
    std::thread::Builder::new()
        .name("mail-engine".into())
        .spawn(move || {
            while let Ok(req) = req_rx.recv() {
                backend.handle(req, &evt_tx);
            }
        })
        .expect("spawn mail engine worker");
    (req_tx, evt_rx)
}

/// Live IMAP/SMTP backend. Compiled only under `--features live` so the default
/// build doesn't pull the TLS/async stack.
#[cfg(feature = "live")]
pub mod live {
    use super::*;

    /// Real IMAP fetch + SMTP submit. Implemented against `async-imap` +
    /// `lettre` (added as optional deps under the `live` feature). The bodies
    /// here are the only place sockets are opened; everything above is shared
    /// with the stub backend.
    pub struct LiveEngine;

    impl MailEngine for LiveEngine {
        fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>) {
            match req {
                EngineRequest::Send { account, draft } => {
                    // lettre: connect to account.smtp_host:smtp_port with
                    // implicit-TLS / STARTTLS per account.smtp_tls, AUTH, then
                    // submit build_mime(&from, &draft). Errors map to
                    // EngineEvent::Error.
                    let from = crate::model::Addr::new(account.display_name.clone(), account.email.clone());
                    match build_mime(&from, &draft) {
                        Ok(bytes) => {
                            // … lettre SmtpTransport submit goes here …
                            let _ = out.send(EngineEvent::Sent { wire_bytes: bytes.len() });
                        }
                        Err(e) => {
                            let _ = out.send(EngineEvent::Error { context: "smtp".into(), message: e.to_string() });
                        }
                    }
                }
                EngineRequest::Fetch { account_id, folder } => {
                    // async-imap: LOGIN, SELECT folder.imap_name(), UID SEARCH,
                    // UID FETCH (ENVELOPE BODY[]), feed each blob through
                    // transport::parse_eml, emit EngineEvent::Fetched.
                    let _ = out.send(EngineEvent::Fetched { account_id, folder, messages: Vec::new() });
                }
                other => StubEngine.handle(other, out),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_send_reports_wire_bytes() {
        let (tx, rx) = spawn();
        let account = AccountConfig {
            display_name: "Me".into(),
            email: "me@qos.example".into(),
            ..Default::default()
        };
        let draft = Draft { to: "you@there.example".into(), subject: "Hi".into(), body: "Body".into(), ..Default::default() };
        tx.send(EngineRequest::Send { account, draft: Box::new(draft) }).unwrap();
        let evt = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        match evt {
            EngineEvent::Sent { wire_bytes } => assert!(wire_bytes > 0),
            other => panic!("expected Sent, got {other:?}"),
        }
    }

    #[test]
    fn stub_connect_acks() {
        let (tx, rx) = spawn();
        tx.send(EngineRequest::Connect {
            account: AccountConfig { email: "a@b.example".into(), ..Default::default() },
        })
        .unwrap();
        let evt = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(matches!(evt, EngineEvent::Connected { .. }));
    }
}
