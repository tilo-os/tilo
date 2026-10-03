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
//! * **Local view state** is a [`LocalState`](crate::LocalState): never
//!   persisted, never shared, never undone.
//! * **Edits bound to where the user is** — the selection, the caret, the
//!   playhead, the folder shown — are typed builders of the resource's
//!   action ([`ActionDecl`](tilo_resource::ActionDecl)), published on the
//!   resource beside its declarations:
//!   [`ResourceSurface::bound_declarations`] from a surface's view state,
//!   [`ViewCx::bind_declarations`] from a view's local state, and
//!   `Resource::bound_declarations` from the resource state itself.
//! * **Live external rows** — processes, devices, containers, journal
//!   entries, fetched catalogue rows, running jobs — are [`LiveRow`]s from
//!   [`ResourceSurface::live_rows`], merged by id into the resource's
//!   published children. A row carries only view and effect operations.
//!   Neither rows nor children need to be published whole: a bounded page is
//!   published and the rest resolve by id (`ResourceSurface::live_row`,
//!   `Resource::child`).
//! * **One published tree** — the palette, agents and scripts see the
//!   resource's publication and nothing beside it that changes the document:
//!   a document action a content widget publishes through its own
//!   `PublishesResources` is dropped (a debug build stops at it).
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
mod rows;
mod stage;
mod surface;
mod switch;
// The test hosts discover native presentation, which the browser has not got.
#[cfg(not(target_arch = "wasm32"))]
pub mod testing;
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
pub use rows::LiveRow;
pub use stage::stage;
pub use surface::{ResourceSelection, ResourceSurface, SurfaceCx};
pub use view::ViewCx;
