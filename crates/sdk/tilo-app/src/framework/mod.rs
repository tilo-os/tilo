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
//! * **Launching on something** — a path or URL on the command line, a file
//!   a host hands over — asks the app which record that is
//!   ([`ResourceApp::launch_record`]: a file's own record, one derived from
//!   it, a minted one) and which of the resource's actions the launch takes
//!   there ([`ResourceApp::launch`], through the same door as every
//!   control). [`ResourceApp::OPENS`] names the media types it is handed and
//!   [`ResourceApp::ARGUMENTS`] how many arguments it takes. A command the
//!   binary runs instead of a window (`--emit-photos DIR`) is a
//!   [`Command`] in [`ResourceApp::commands`], and the app's messages are
//!   its [`ResourceApp::catalog`], loaded in every host that mounts it.
//! * **Who is editing** is [`ViewCx::me`] / [`SurfaceCx::me`] for the reader
//!   of a view, and [`ActionCx::author`](tilo_resource::ActionCx::author)
//!   inside an action: the same id, pinned with the action so every replica
//!   reads it. The resource state stays the same for everyone; a view
//!   compares the authors it records with `me`, and an action signs what it
//!   writes with its author rather than carrying one in its payload.
//! * **A brand-new record** starts with the resource's
//!   [`initialize`](tilo_resource::Resource::initialize), its genesis: the
//!   same change on every device, so two people creating the same record
//!   converge on one. What depends on who creates it or where (the creator
//!   as owner, a first-run import of existing settings) is
//!   [`ResourceApp::seed`]: actions applied once after the genesis, by the
//!   creator, that are nobody's undo step.
//! * **Which changes are undo steps** is declared by the action
//!   ([`tilo_actions::Action::change_kind`], `#[action(external)]`): content
//!   from outside — fetched rows, a process's output — syncs to everyone but
//!   no undo takes it back. Values applied immediately that continue one
//!   another ([`Resource::coalesces`](tilo_resource::Resource::coalesces)) are
//!   one step — a word typed, a slider dragged — until the surface
//!   [settles](Dispatcher::settle) the gesture.
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
mod launch;
mod live;
mod preview;
mod root;
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
pub use app::{ResourceApp, SeedCx, definition};
pub use controls::{ActionControl, Actions};
pub use dispatch::{DispatchGroup, Dispatcher};
pub use each::Each;
pub use launch::{Command, LaunchInput};
pub use live::Live;
pub use preview::PastVersion;
pub use stage::stage;
pub use surface::{ResourceSelection, ResourceSurface, SurfaceCx};
pub use view::ViewCx;
