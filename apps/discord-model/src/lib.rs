//! The Discord connector's vocabulary, shared by the app (which queues ops
//! and reads events), the outside worker (which performs ops and reports
//! events) and the bridge program's tests. Plain serde types: nothing here
//! talks to Discord or to a room.

pub mod event;
pub mod op;
pub mod perm;
