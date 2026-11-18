//! The `diffmerge.session` resource: the two texts compared or the three texts
//! merged, which side each change takes, and the merged result, as the app that
//! draws it and an agent that works it both read it.
//!
//! The verbs recompute the merge from the resource's own inputs, so every peer
//! given the same change composes the same result. Views and file reading stay
//! in the app; the file a comparison was opened from is an effect.

// Line alignment is index arithmetic on line numbers the texts themselves bound;
// a comparison of a handful of lines cannot overflow an address. Panic-producing
// lints stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "line alignment is bounded index arithmetic on the texts it is given"
)]

pub mod actions;
pub mod doc;
pub mod model;
mod resource;

pub use actions::DiffmergeEffect;
pub use doc::{Comparison, HunkChoice, MergeInputs, Session};
pub use resource::session;
