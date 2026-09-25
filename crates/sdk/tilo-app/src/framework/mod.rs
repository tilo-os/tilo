//! The app framework: a resource, its actions, and views bound to it.
//!
//! An app author writes three things:
//!
//! 1. **a resource type** — a `#[derive(Model)]` record (or any type
//!    implementing [`Resource`](tilo_resource::Resource) by hand, such as a
//!    timeline or a rich-text document);
//! 2. **its actions** — `#[action]` methods in a `#[resource]` impl: typed,
//!    replayable, and exposed to the palette, agents and scripts;
//! 3. **views** — a function from a [`ViewCx`] to widgets, where every value
//!    shown is read from the resource and every edit is an action.
//!
//! [`run`] hosts that on the toolkit's component runtime, so without writing
//! any of it the app gets persistence and sync of its record, collaboration
//! and presence, versioned history (a History mode that renders past versions
//! with the app's own view), per-author undo, permissions enforced on every
//! action (controls of forbidden actions disable themselves), the action
//! palette and agent/script door, and search indexing.
//!
//! ```ignore
//! #[derive(Model, Default)]
//! pub struct Note { pub body: String }
//!
//! #[resource(kind = "example.note", label = "Note")]
//! impl Note {
//!     /// Edit the note.
//!     #[action]
//!     pub fn edit(&mut self, text: String) { self.body = text; }
//! }
//!
//! struct NoteApp;
//! impl ResourceApp for NoteApp {
//!     type Resource = Note;
//!     const ID: &'static str = "example.note";
//!     const TITLE: &'static str = "Note";
//!     fn view(cx: &mut ViewCx<Note>) -> Element {
//!         Box::new(Column::new().push(Box::new(
//!             cx.input(|note| note.body.clone(), Note::EDIT).multiline(true),
//!         )))
//!     }
//! }
//!
//! fn main() { tilo_app::framework::run::<NoteApp>() }
//! ```
//!
//! # Extension contracts
//!
//! Custom parts do not leave the framework. Each contract asks only for what
//! the framework cannot know:
//!
//! * **A custom resource type** implements
//!   [`Resource`](tilo_resource::Resource): `read` its state from a document
//!   (the same call renders history), name its replayable `Action` type, and
//!   optionally declare named actions. Its own merge semantics live in its
//!   `Action::apply`.
//! * **A custom widget or renderer** implements the toolkit's `Widget` for
//!   pixels and input, plus [`ResourceSurface`] to take part: `present` the
//!   resource's state, submit edits through its [`Dispatcher`], and report
//!   where its user is with [`ResourceSelection`]. Built from a `ViewCx`, the
//!   same surface renders past versions read-only in History mode.
//! * **Local view state** is a [`LocalState`](crate::LocalState): never
//!   persisted, never shared, never undone.

mod app;
mod controls;
mod dispatch;
mod each;
mod live;
mod preview;
mod root;
mod surface;
mod switch;
pub mod testing;
mod view;

pub use app::{ResourceApp, definition, launch_spec, run};
pub use controls::{ActionControl, Actions};
pub use dispatch::Dispatcher;
pub use each::Each;
pub use live::Live;
pub use surface::{ResourceSelection, ResourceSurface, SurfaceCx};
pub use view::ViewCx;
