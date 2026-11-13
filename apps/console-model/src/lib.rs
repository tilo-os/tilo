//! Console's record: which agent runs its Agents place shows, the verb that clears that choice,
//! and the effects the app answers.
//!
//! The app draws this record and changes it by its verb: what one person's window submits and
//! what another device applies are the same action on either side.
//!
//! The runs themselves are the workspaces' history, which the app reads through the session; this
//! record holds no part of them. Everything here is plain data — no toolkit types, no IO.

mod effect;
mod record;

pub use effect::ConsoleEffect;
pub use record::Console;
