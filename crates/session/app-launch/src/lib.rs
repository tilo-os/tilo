//! Starting an application from the desktop.
//!
//! A desktop surface asks for an app by name — a launch string — and this
//! crate turns that name into a process: what to run, with which arguments,
//! in which environment, and in whose room. A launch is one of
//!
//! - a Tilo package id (`so.tilo.notes`), which starts through
//!   `tilo-launcher` in the package's sandbox ([`plan`]);
//! - `host:<desktop-id>`, one of the machine's own applications, which runs
//!   from its `.desktop` entry the way the host runs it ([`plan`]);
//! - `window:<n>`, a window already arriving: nothing to start;
//! - `record:<app>:<record>` and `content:<app>:<hash>`, the app opening
//!   one of its records or content another app handed over;
//! - a document or a link, which goes to the app that claims its type or
//!   scheme ([`routing`]).
//!
//! Everything the session knows that the plan needs — the room and what was
//! granted in it ([`Room`]), the displays and the person's session variables
//! ([`env`]) — is passed in as data, so a plan is assertable without a
//! process appearing. [`spawner`] is the part that does start something, and
//! reports how it ended so a crash can be told from a normal exit.

pub mod env;
pub mod launch;
pub mod plan;
pub mod room;
pub mod routing;
pub mod spawner;

pub use env::SessionEnvironment;
pub use launch::{Launch, Open};
pub use plan::{Kind, PlanContext, PlanError, Planned};
pub use room::Room;
pub use spawner::{End, Event, Spawner, Started};
