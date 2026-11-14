//! One test binary. `tests/host.rs` and `tests/own_records.rs` each linked the whole host as a
//! binary of its own; they are modules of this one. A test still runs in a process of its own
//! under nextest, so what one file's tests hold in a static is never another's.

mod host;
mod own_records;
