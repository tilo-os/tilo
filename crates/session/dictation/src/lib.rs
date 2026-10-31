//! Dictation: a local speech recognizer over the microphone, allowed by the
//! person once per dictation, whose final text is committed into the field
//! they were typing into.
//!
//! What lives here:
//!
//! - [`session`] — one dictation from the key that asks for it to the text it
//!   commits: what the person allowed, the private draft, the bounds, the kept
//!   transcript. Nothing records before [`DictationLink::allow`], and that
//!   names the dictation it grants, so neither a remote action nor the
//!   recognizer being ready is ever consent.
//! - [`runtime`] — the recognizer worker: a speech worker process over private
//!   pipes, its audio, and the vocabulary it recognizes with.
//! - [`capture`] — the microphone, opened only after consent.
//! - [`target`] — the field side, as the compositor offers it: which field the
//!   person types into, and committing text into it once.
//! - [`service`] — the `dictation:session` resource and its actions, published
//!   like every other session service, and the session's handle on it.
//!
//! A draft never leaves this crate except as the final text inserted into the
//! field, and never enters a reading, a log or a workspace.

pub mod capture;
pub mod effects;
pub mod runtime;
pub mod service;
pub mod session;
pub mod status;
pub mod stream;
pub mod target;

pub use effects::DictationAction;
pub use stream::PhononTranscript;
pub use service::{
    DICTATION, DICTATION_KIND, DictationLink, DictationService, Request, dictation_resource,
};
pub use session::{Dictation, MAX_COMMIT_BYTES, MAX_DRAFT_BYTES, SESSION_WINDOW};
pub use status::{DictationError, DictationStatus, RecoveryReason, RuntimeError, SessionId};
pub use target::{Field, FieldRefusal, Insertion, Target};
