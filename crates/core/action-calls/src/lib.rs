//! # Tilo action calls
//!
//! Answering action calls against a provider. [`tilo_actions`] keeps what a call is: the
//! messages a peer sends, the provider trait that answers them, and who is calling. This crate
//! keeps how a host answers: [`serve`] maps one request onto a provider, [`settle_with`] holds an
//! answer until the invocation it follows moves, and [`InvocationLog`] runs each invocation once
//! however often its caller asks. Only the hosts and processes that answer calls depend on it.

mod log;
mod serve;
mod wait;

pub use log::{Admission, DEFAULT_LOG_CAPACITY, InvocationLog, Reservation};
pub use serve::serve;
pub use wait::settle_with;

#[cfg(test)]
mod serve_tests;
