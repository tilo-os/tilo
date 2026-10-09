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
//! # Test the public workflow
//!
//! [`testing::TestApp`] mounts the real app on an isolated profile. Start with
//! [`actions`](testing::HeadlessApp::actions) to discover the participant's
//! complete action contracts, then [`call_on`](testing::HeadlessApp::call_on)
//! using the discovered address and JSON arguments. Test a refusal before
//! [`call_confirmed_on`](testing::HeadlessApp::call_confirmed_on) for a destructive
//! operation, and verify that a live role change still refuses a saved address.
//! The runnable `resource-journey` example exercises that path alongside a bound
//! input control, authored task identity, undo and reopen without a display.
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
//! * **Handing a result to another app** — a picture to place, a file to
//!   attach — is an effect declared `content`: it answers
//!   [`Effected::content`] with the hash of bytes in the device's content
//!   store, its declaration's result is a `tilo.content` resource, and an
//!   operation that takes content (`ActionSpec::prompted_content`) receives
//!   it as its input. The caller chains the two by those types and never
//!   reads the producing app's document.
//! * **Local view state** is a [`LocalState`](crate::LocalState): never
//!   persisted, never shared, never undone.
//!   A surface whose window should reopen as it was left (the open note,
//!   the caret) also provides
//!   [`ApplicationViewState`](ui_core::application_view::ApplicationViewState):
//!   the root saves and restores it with the window.
//! * **Launching on something** — a path or URL on the command line, a file
//!   a host hands over — asks the app which record that is
//!   ([`ResourceApp::launch_record`]: a file's own record, one derived from
//!   it, a minted one) and which of the resource's actions the launch takes
//!   there ([`ResourceApp::launch`], through the same door as every
//!   control), or which of its effects, for a launch that shows something
//!   (a link to one item) rather than editing
//!   ([`Invoke::effect`](tilo_resource::Invoke::effect)); the view reads the
//!   same launch ([`ViewCx::launch`]) to show what those actions made.
//!   [`ResourceApp::OPENS`] names the media types it is handed and
//!   [`ResourceApp::ARGUMENTS`] how many arguments it takes. A handed file's
//!   bytes are kept in the launch's content store ([`LaunchInput::content`]),
//!   which the actions name by hash, so the record never carries the file.
//!   The app's own Open is the same launch in the window it runs in
//!   ([`ViewCx::open_here`], [`SurfaceCx::opener`]): the window shows the
//!   record that launch names in place of its own, so a document has one
//!   record however it was opened.
//!   An app that offers private windows ([`ResourceApp::PRIVATE_WINDOWS`],
//!   `--private`) runs one on a profile thrown away with the process,
//!   attached to no runtime, and its view knows ([`ViewCx::is_private`]).
//!   A command the binary runs instead of a window (`--emit-photos DIR`) is a
//!   [`Command`] in [`ResourceApp::commands`]; a helper the binary also is
//!   (a privileged writer started with its own command line) is a
//!   [`Helper`] in [`ResourceApp::helpers`], asked before anything of the
//!   app starts; and the app's messages are
//!   its [`ResourceApp::catalog`], loaded in every host that mounts it. A
//!   launch that continues playback another device or app handed over says
//!   where it stood ([`LaunchInput::handoff`]): the media is what the launch
//!   names, the position and play state are the view's to restore.
//!   The app's own options — how the window runs rather than what it opens,
//!   such as the layout an on-screen keyboard starts in — are
//!   [`ResourceApp::OPTIONS`] and [`ResourceApp::FLAGS`], read with
//!   [`ViewCx::launch_option`] / [`SurfaceCx::launch_option`].
//! * **A system surface** — the session's on-screen keyboard — declares its
//!   window's role in [`ResourceApp::BEHAVIOUR`]
//!   ([`WindowRole::InputSurface`](ui_core::application::WindowRole)): the
//!   window announces the app's id to the compositor and no headless drive
//!   operates it. The connections it serves the session through (an input
//!   method, a virtual keyboard) are the surface's own, opened in a live run
//!   ([`SurfaceCx::mode`]) and woken through [`SurfaceCx::waker`].
//! * **Mounted in another process** — the shell hosting a terminal tab in
//!   its own — the app is the same component, on a record the host opens
//!   for it, publishing into the host's palette and agent door. What only
//!   the host can provide (the session's process supervisor) it lends
//!   ([`HostContext::lend`](ui_core::application::HostContext::lend)); the
//!   view takes it with [`ViewCx::take_lent`] and otherwise uses its own.
//! * **Who is editing** is [`ViewCx::me`] / [`SurfaceCx::me`] for the reader
//!   of a view, and [`ActionCx::author`](tilo_resource::ActionCx::author)
//!   inside an action: the same id, pinned with the action so every replica
//!   reads it. The resource state stays the same for everyone; a view
//!   compares the authors it records with `me`, and an action signs what it
//!   writes with its author rather than carrying one in its payload. What a
//!   view shows for an id is the name the host's roster has for it
//!   ([`ViewCx::people`], [`SurfaceCx::people`]): the document keeps ids,
//!   so a name that changes or arrives late reads right everywhere.
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
//! * **The person's own data** — what they watched, the words they taught
//!   the keyboard, where they stopped reading: data that grows, has history
//!   and is undone, but belongs to the person rather than the room — is a
//!   resource kept in the person's own room ([`ViewCx::person`],
//!   [`SurfaceCx::person`]). The [`PersonRecord`] is one record per kind on
//!   all of the person's devices, edited with the resource's actions,
//!   attributed, in its history and undone per step, and synced to the
//!   person's devices only — never to a collaborator, whatever room the app
//!   is open in. A preference is still a setting.
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
//!   and answering its caller. Importing and exporting is such a job: the
//!   host's file chooser opens through [`EffectCx::choose_file`] and
//!   [`EffectCx::save_file`], the same on every host, a browser page
//!   included.
//!   An effect whose harm depends on its arguments (formatting the system
//!   disk) classifies each invocation (`#[effect(risk = "…")]`), and asks
//!   for confirmation only when it is destructive. A surface withholds one
//!   of the resource's own actions while its live state forbids it
//!   ([`ResourceSurface::availability`]). Effects never run in History mode. A variant
//!   that only changes what its caller sees (open, reveal, show) is
//!   `#[effect(view)]`, published as a View operation and answered by the
//!   same handler.
//! * **Operations on one item** — open *this* snippet, reveal *this* tag,
//!   connect *this* connection, run *this* saved query — are effects bound
//!   to the item: the resource's `children` carry
//!   `.with_action(MyEffect::Open { id }.bound())`, a live row
//!   [`LiveRow::effect`]. The palette, an agent or a script invokes it on
//!   the item through the same door as every other action, and the handler
//!   of `MyEffect` receives that value. An effect only ever offered this way
//!   is `#[effect(row)]`, so the resource does not also offer it with an id
//!   to type. No view or surface answers a raw `Widget::start_operation`
//!   for it.
//! * **Jobs nobody invoked** — a route fetched when the destination
//!   changes, a feed refreshed when it opens — start through the view's or
//!   surface's [`Jobs`] ([`ViewCx::jobs`], [`SurfaceCx::jobs`]), under a key
//!   the latest request takes over, and finish like an effect's.
//! * **Projections** — an outside consequence an action recorded in the
//!   document (an outbox, a filesystem operation, a device's configuration)
//!   — are drained with [`ViewCx::drain`]: the framework runs the drain in
//!   the live app after every change, on every participant's device, and
//!   the drain acknowledges each entry with an action.
//! * **Files of the mounted filesystem** an app shows (a file manager's
//!   selected file) have a history of their own, kept by the filesystem's
//!   projection rather than the app's document. The app declares it
//!   ([`ResourceApp::FILE_HISTORY`]) and its surface names the file
//!   ([`SurfaceCx::file_history`]); History offers it beside the resource's.
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
//!   published children. A row carries only view and effect operations
//!   (an effect bound to the row with `LiveRow::effect`) and may say how
//!   its entity stands right now (`LiveRow::presentation`). An edit of the
//!   resource bound to a row (rename *this* file) is one of
//!   [`ResourceSurface::row_declarations`], published on the row.
//!   Neither rows nor children need to be published whole: a bounded page is
//!   published and the rest resolve by id (`ResourceSurface::live_row`,
//!   `Resource::child`).
//! * **One search source** — what the app's search finds is the resource's
//!   entries ([`Resource::search`](tilo_resource::Resource::search)) and,
//!   for what a surface shows that the resource does not hold (a page's
//!   text, a book's outline, processes), the surface's own
//!   ([`ResourceSurface::search_entries`]), indexed together. An entry says
//!   how its hit reads — its result group, subtitle, icon and the action it
//!   invokes — so the app needs no search provider beside it.
//! * **One published tree** — the palette, agents and scripts see the
//!   resource's publication and nothing beside it that changes the document:
//!   a document action a content widget publishes through its own
//!   `PublishesResources` is dropped (a debug build stops at it).
//! * **A participant without a window** — an agent, a service, a script —
//!   holds a replica of a record and edits it through the same door as a
//!   person: a [`Participant`] of the record's kind (each app's kind is
//!   [`resource_kind`]), checked against its permissions, attributed to it
//!   and recorded in history with the task it belongs to.
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
//!   state, `ResourceSurface::stage` for a custom surface's. What a person
//!   does in the scene (typing, a drag held over its target) is
//!   [`ResourceApp::scenario_input`], driven through the window's own event
//!   path. The capture then waits until no surface reports unfinished visual work (an async
//!   page render, a decode). [`stage`] runs a scenario on any harness, and
//!   [`TestApp::capture`](testing::HeadlessApp::capture) composes the exact
//!   frame `--shots` writes in a test.

mod app;
mod attached;
mod controls;
// In-place reload swaps native code into a running process; a browser build
// has neither the thin shared library nor the host that loads it.
mod delivery;
#[cfg(not(target_arch = "wasm32"))]
pub mod dev_reload;
mod dispatch;
mod each;
mod effect;
mod form;
pub mod history;
mod kinds;
mod launch;
mod live;
mod participant;
mod people;
mod person;
mod preview;
#[cfg(feature = "test-support")]
pub mod profiling;
mod reactive;
mod record;
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

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use app::definition_launch_spec;
pub use app::launch_spec;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use app::run;
pub use app::{FileHistory, ResourceApp, SURFACE_FLAG, SeedCx, definition, run_helper};
pub use attached::AttachedView;
pub use controls::{ActionControl, Actions};
pub use delivery::HostDelivery;
pub use dispatch::{DispatchGroup, Dispatcher, Landed, Refused, Submit};
pub use each::Each;
pub use effect::{
    ChosenFile, DocumentVersion, EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected,
    JobProgress, Jobs, Progress, Projection,
};
pub use form::{Form, FormSubmit};
pub use kinds::{hosted_kinds, resource_kind, seed_record, seed_record_in_room};
pub use launch::{Command, Helper, LaunchInput, OpenHere};
pub use live::Live;
pub use participant::Participant;
pub use people::People;
pub use person::PersonRecord;
pub use preview::PastVersion;
pub use record::{OtherRecord, Records};
pub use rows::LiveRow;
pub use scenario::{Fixture, RunMode};
pub use settings::Setting;
pub use stage::{stage, time_frames};
pub use surface::{ResourceSelection, ResourceSurface, SurfaceCx};
pub use text::{EditsText, TextControl};
pub use tilo_resource::{Effect, NoEffect};
pub use view::{Bindable, EffectsHandle, ViewCx};

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod headless_results_tests;

/// Record content and canonical hashing. Use the store borrowed through
/// `SurfaceCx::content` or `LaunchInput::content` so the host binds reads and
/// writes to the resource's content lane.
pub mod content {
    pub use tilo_content::{ContentStore, hash_bytes, valid_hash};
}
