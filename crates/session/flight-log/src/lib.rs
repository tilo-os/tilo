//! A Tilo session's flight records: the recorder that keeps them while the
//! compositor runs ([`recorder`], the `tilo-flight-recorder` program) and the
//! report that summarises them afterwards ([`report`], `tilo-session-report`).

pub mod recorder;
pub mod report;
pub mod value;
