//! The person's messaging accounts as their own process (`tilo-messaging`).
//!
//! Signal, `WhatsApp` and SMS accounts are enrolled and run here, not in the
//! compositor and not in the Messages app: the service holds the person's
//! device identity and each running account's key, starts the Signal provider
//! in its sandbox and keeps every account's encrypted history. Messages
//! reaches it over the `messaging` session socket, connected and named by the
//! launcher like every app→service connection ([`wire`], client in
//! [`tilo_messaging::service`]).
//!
//! The compositor starts the service with the session and answers what only
//! it holds over the service's standard input and output ([`session`]). The
//! service stops when that session closes.

pub use tilo_messaging::service::{MESSAGES_APP, SERVICE, SERVICE_FD_ENV, client, link, wire};

pub mod session;

#[cfg(feature = "service")]
pub mod service;
