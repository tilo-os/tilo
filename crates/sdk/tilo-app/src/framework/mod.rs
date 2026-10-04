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
//! * **Settings** — preferences that follow a person or stay on a device,
//!   never seen by collaborators — are a typed
//!   [`SettingsSchema`](crate::storage::SettingsSchema) whose
//!   [`Scope`](crate::storage::Scope) says how far they travel: `Personal`
//!   roams across the person's own devices, `DeviceLocal` never leaves this
//!   one, `Shared` reads a per-user section every app follows (`locale`,
//!   `text_assistance`). [`ViewCx::settings`] and [`SurfaceCx::settings`]
//!   hand out a [`Setting`] kept current while the view lives; a write from
//!   another window or device reaches it on the next frame. Files and
//!   secrets come from the same context ([`ViewCx::storage`],
//!   [`SurfaceCx::secrets`], [`SurfaceCx::files`]); a secret that belongs to
//!   one document item (a profile's password) is
//!   `secrets().item(purpose, id)`, so the document names the item and the
//!   secret stays on the device.
//! * **Effects** — work with no document delta (reload, back up now,
//!   connect, run a query) — are a typed vocabulary: `#[derive(Effect)]` on
//!   an enum, one variant per effect. A view answers it with
//!   [`ViewCx::effects`], a custom surface with
//!   [`ResourceSurface::effect`] (`impl ResourceSurface<R, MyEffect>`); the
//!   framework publishes it beside the resource's actions, with the
//!   availability and labels the handler reports now ([`EffectStatus`]),
//!   and every caller — a button ([`ViewCx::effect_button`],
//!   [`Dispatcher::effect`]), the palette, an agent, a script — reaches the
//!   handler as a typed value. Long work is a job started through the
//!   [`EffectCx`] ([`thread`](EffectCx::thread), [`spawn`](EffectCx::spawn),
//!   [`reply`](EffectCx::reply)): it reports progress, may be cancelled, and
//!   finishes on the UI thread, changing the document with ordinary actions
//!   and answering its caller. Effects never run in History mode. An effect
//!   that changes only what the window shows (open a page) is declared
//!   `#[effect(view)]`, a view operation any participant may invoke; one
//!   bound to a single thing a live row names (analyze this folder) is
//!   offered on the row as [`Effect::operation`](tilo_resource::Effect::operation).
//! * **Projections** — an outside consequence an action recorded in the
//!   document (an outbox, a filesystem operation, a device's configuration)
//!   — are drained with [`ViewCx::drain`]: the framework runs the drain in
//!   the live app after every change, on every participant's device, and
//!   the drain acknowledges each entry with an action.
//! * **Run mode.** A view is built for a window someone uses, a past version
//!   in History, or a screenshot ([`RunMode`], from [`ViewCx::mode`] and
//!   [`SurfaceCx::mode`]). Only a live view starts work that reaches outside
//!   the process (a mail engine, a socket, a journal reader, a hardware
//!   probe): History rebuilds the view for every version it previews, and a
//!   screenshot must come out the same on every machine.
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
//! * **Screenshot scenarios** (`--shots --scenario NAME`, every one with
//!   `--shots-dir DIR`, each at [`ResourceApp::shot_viewport`]) are prepared
//!   in three parts. What the resource does not hold is the scenario's
//!   [`Fixture`] ([`ResourceApp::fixture`]), prepared before the view is
//!   built: typed stand-ins for live sources that views read with
//!   [`ViewCx::fixture`] and surfaces with [`SurfaceCx::fixture`], scratch
//!   files, content-store blobs, versions other people wrote at other times,
//!   and collaborators shown where they are. The resource is staged with
//!   actions ([`ResourceApp::scenario`]), and view state where it lives,
//!   once the actions are laid out: [`ViewCx::on_stage`] for a view's local
//!   state, `ResourceSurface::stage` for a custom surface's. The capture
//!   then waits until no surface reports unfinished visual work (an async
//!   page render, a decode). [`stage`] runs a scenario on any harness, and
//!   [`TestApp::capture`](testing::HeadlessApp::capture) composes the exact
//!   frame `--shots` writes in a test.

mod app;
mod controls;
// In-place reload swaps native code into a running process; a browser build
// has neither the thin shared library nor the host that loads it.
#[cfg(not(target_arch = "wasm32"))]
pub mod dev_reload;
mod dispatch;
mod each;
mod effect;
pub mod history;
mod launch;
mod live;
mod preview;
mod root;
mod rows;
mod scenario;
mod settings;
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
pub use app::{ResourceApp, SeedCx, definition};
pub use controls::{ActionControl, Actions};
pub use dispatch::{DispatchGroup, Dispatcher, Refused};
pub use each::Each;
pub use effect::{EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, Progress};
pub use launch::{Command, LaunchInput};
pub use live::Live;
pub use preview::PastVersion;
pub use rows::LiveRow;
pub use scenario::{Fixture, RunMode};
pub use settings::Setting;
pub use stage::stage;
pub use surface::{ResourceSelection, ResourceSurface, SurfaceCx};
pub use text::{EditsText, TextControl};
pub use tilo_resource::{Effect, NoEffect};
pub use view::{EffectsHandle, ViewCx};
