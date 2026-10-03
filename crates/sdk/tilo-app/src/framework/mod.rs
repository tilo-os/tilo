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
//! with the app's own view and restores any of them), per-author undo,
//! permissions enforced on every action (controls of forbidden actions disable
//! themselves, and a role change takes effect while the app runs), the action
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
//! * **Extending an existing widget** — adding one hook to a toolkit widget,
//!   a view or a surface — implements
//!   [`Decorator`](crate::widget::Decorator) instead of `Widget`: name the
//!   wrapped widget and override only the hooks that change; every other
//!   `Widget` method, including the ones containers must forward, reaches
//!   the wrapped widget by default.
//! * **A resource nested in another** — a note's rich-text body, a draft's
//!   message, a saved query's SQL — is a child resource: a resource type of
//!   its own stored at a place in its parent's document, whose edits the
//!   parent carries in one of its own actions (`EditBody { page, edit }`).
//!   The parent publishes it from `children` with
//!   [`publish_child`](tilo_resource::publish_child); a view or surface
//!   edits it through [`ViewCx::child`] / [`SurfaceCx::child`], a
//!   [`Dispatcher`] of the child type that wraps each edit into the
//!   parent's action. The child is addressed, permissioned, undone (by its
//!   own coalescing) and named in history like a root, and applies as the
//!   parent. Rich text is `tilo_rich_text::RichTextField` over such a
//!   dispatcher; plain text is [`PlainText`](tilo_resource::PlainText),
//!   whose [`TextControl`] binds a `TextInput` or `CodeEditor` so every
//!   keystroke is a splice action.
//! * **Local view state** is a [`LocalState`](crate::LocalState): never
//!   persisted, never shared, never undone.
//! * **Screenshot scenarios** (`--shots --scenario NAME`) stage the resource
//!   with actions ([`ResourceApp::scenario`]) and view state where it lives:
//!   [`ViewCx::on_stage`] for a view's local state, `ResourceSurface::stage`
//!   for a custom surface's. [`stage`] runs a scenario on any harness, so a
//!   test composes the exact frame `--shots` writes.

mod app;
mod controls;
// In-place reload swaps native code into a running process; a browser build
// has neither the thin shared library nor the host that loads it.
#[cfg(not(target_arch = "wasm32"))]
pub mod dev_reload;
mod dispatch;
mod each;
pub mod history;
mod live;
mod preview;
mod root;
mod stage;
mod surface;
mod switch;
// The test hosts discover native presentation, which the browser has not got.
#[cfg(not(target_arch = "wasm32"))]
pub mod testing;
mod text;
mod view;

#[cfg(not(target_arch = "wasm32"))]
pub use app::definition_launch_spec;
pub use app::launch_spec;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use app::run;
pub use app::{ResourceApp, definition};
pub use controls::{ActionControl, Actions};
pub use dispatch::{DispatchGroup, Dispatcher};
pub use each::Each;
pub use live::Live;
pub use preview::PastVersion;
pub use stage::stage;
pub use surface::{ResourceSelection, ResourceSurface, SurfaceCx};
pub use text::{EditsText, TextControl};
pub use view::ViewCx;
