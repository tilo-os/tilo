//! # Tilo action calls
//!
//! Answering action calls against a provider. [`tilo_actions`] keeps what a call is: the
//! messages a peer sends, the provider trait that answers them, and who is calling. This crate
//! keeps how a host answers: [`serve`] maps one request onto a provider, [`settle_with`] holds an
//! answer until the invocation it follows moves, and [`InvocationLog`] runs each invocation once
//! however often its caller asks. A read of a record is answered here too: [`read_resources`]
//! reads and follows what an ask names, and [`answer_read`] answers a posted read over the API.
//! Only the hosts and processes that answer calls depend on it.

mod log;
mod reads;
mod serve;
mod wait;

pub use log::{Admission, DEFAULT_LOG_CAPACITY, InvocationLog, Reservation};
pub use reads::{
    answer_read, answered, copies_readable, read_answered, read_resources, readings_from,
    readings_of,
};
pub use serve::serve;
pub use wait::settle_with;

#[cfg(test)]
mod serve_tests;
