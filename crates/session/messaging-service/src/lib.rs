//! The person's messaging accounts as their own process (`tilo-messaging`).
//!
//! Signal, `WhatsApp`, SMS and Tilo accounts are enrolled and run here, not in
//! the compositor and not in the Messages app: the service holds the
//! person's device custody, the vault keys and grants Messages was given,
//! starts the Signal provider in its sandbox and keeps every account's
//! encrypted history. Messages reaches it over the `messaging` session
//! socket, connected and named by the launcher like every app→service
//! connection ([`wire`], client in [`tilo_messaging::service`]); Passwords
//! reaches it there too, to answer the permission requests the service
//! makes.
//!
//! The compositor starts the service with the session and answers what only
//! it holds over the service's standard input and output ([`door`]). The
//! service stops when that door closes.

pub use tilo_messaging::service::{
    MESSAGES_APP, PASSWORDS_APP, SERVICE, SERVICE_FD_ENV, client, link, wire,
};

pub mod door;

#[cfg(feature = "service")]
pub mod service;
