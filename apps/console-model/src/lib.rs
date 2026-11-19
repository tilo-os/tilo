//! Console's record: which agent runs its Agents place shows, the verb that clears that
//! choice, and the effects the app answers.

mod console;
mod effect;
mod rows;

pub use console::Console;
pub use effect::ConsoleEffect;
pub use rows::{Agent, Session};
