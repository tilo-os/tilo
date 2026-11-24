//! The bounded `Command` helpers every per-domain module uses, kept in `tilo-bounded-command` so
//! this crate's domains and the print engine share one vocabulary.

pub(crate) use bounded_command::command::{run, run_capture, which};
