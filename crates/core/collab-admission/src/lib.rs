//! Deciding the document ops a device receives by the changes they carry. The bytes are
//! untrusted, so every change is read where every untrusted Automerge input is
//! ([`tilo_automerge_isolate`]); the wire types and the checks that read no change bytes live
//! in [`collab_types`], which an app links without the worker.

pub mod claim;
pub mod receipt;
pub mod validator;

pub use claim::{Maker, maker_of, mutation_claims, parse_changes};
pub use receipt::{
    check_opened_mutation, claimed_invokers, decide_op, decide_op_replayed, decide_opened,
};
pub use validator::{validate_admitted_inbound_op, validate_replayed_inbound_op};

// A test that reads a whole document's changes loads it in a worker, which
// this test binary serves.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
