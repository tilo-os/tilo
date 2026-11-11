//! Console's record: which agent runs its Agents place shows, the verb that clears that choice,
//! and the effects the app answers.
//!
//! The app draws this record, and the session's document host runs its verb in the module built
//! from this crate (`docs/architecture/apps.md` §1): what one person's window submits and what a
//! host commits are the same action on either side.
//!
//! The runs themselves are the workspaces' history, which the app reads through the session; this
//! record holds no part of them. Everything here is plain data — no toolkit types, no IO.

mod effect;
#[cfg(feature = "module")]
mod module;
mod record;

pub use effect::ConsoleEffect;
pub use record::Console;
