//! What a view and a custom surface both reach: the record's dispatcher
//! and live state, the run it is part of, its storage and effects.
//! [`ViewCx`](super::ViewCx) and [`SurfaceCx`](super::SurfaceCx) dereference
//! to it, so each reads the same accessors.

use std::rc::{Rc, Weak};

use tilo_resource::Resource;
use ui_core::LocalState;
use ui_core::record::RecordSaveStatus;

use super::dispatch::Dispatcher;
use super::effect::{EffectAnswer, EffectCx, Effects, Projection};
use super::live::Live;
use super::scenario::{Run, RunMode};
use super::settings::{Setting, ViewStorage};

/// The record, run and storage a view or surface works in. See the module
/// docs.
pub struct RecordEnv<R: Resource> {
    pub(crate) live: Live<R>,
    pub(crate) alive: Weak<()>,
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) saved: LocalState<RecordSaveStatus>,
    pub(crate) run: Run,
    pub(crate) storage: Rc<ViewStorage>,
    pub(crate) effects: Effects<R>,
}

impl<R: Resource> Clone for RecordEnv<R> {
    fn clone(&self) -> Self {
        Self {
            live: self.live.clone(),
            alive: self.alive.clone(),
            dispatch: self.dispatch.clone(),
            saved: self.saved.clone(),
            run: self.run.clone(),
            storage: Rc::clone(&self.storage),
            effects: self.effects.clone(),
        }
    }
}

impl<R: Resource> RecordEnv<R> {
    /// The same environment for a scope that lives as long as `alive`.
    pub(crate) fn living(&self, alive: &Rc<()>) -> Self {
        Self {
            alive: Rc::downgrade(alive),
            ..self.clone()
        }
    }

    /// Open, in this window and in place of the record it shows, what a
    /// launch of the app naming `arguments` opens: the record
    /// [`ResourceApp::launch_record`](super::ResourceApp::launch_record)
    /// names, with its launch actions — the document a person chose with
    /// the app's own Open. The window's view is rebuilt for that record on
    /// the next frame. Refused in a past version and a capture.
    ///
    /// The outcome answers once the host opened it, or why it could not.
    ///
    /// # Errors
    /// Returns that the window cannot open another record.
    pub fn open_here(
        &self,
        arguments: &[String],
    ) -> Result<ui_core::application::ReplacementOutcome, String> {
        self.run.open_here(arguments)
    }

    /// The names of the participants, for showing the ids actions record
    /// ([`People::name`](super::People::name)).
    pub fn people(&self) -> super::People {
        self.run.people.clone()
    }

    /// Where edits go. Custom widgets keep a clone.
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.dispatch.clone()
    }

    /// The dispatcher of the resource nested in this one at `id`, whose
    /// edits are this resource's action `wrap(edit)`: a rich-text body, a
    /// draft, a SQL buffer. See [`Dispatcher::child`].
    pub fn child<C: Resource>(
        &self,
        id: &str,
        wrap: impl Fn(C::Action) -> R::Action + 'static,
    ) -> Dispatcher<C> {
        self.dispatch.child(id, wrap)
    }

    /// Who this view is for: the participant reading and editing through
    /// it, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). Compare the
    /// authors the resource records with it ("your message", "you reacted")
    /// instead of keeping the reader in the resource state, which is the
    /// same for everyone. `None` in a view of a past version.
    pub fn me(&self) -> Option<&str> {
        self.dispatch.me()
    }

    /// Whether this view shows a past version in History mode, drawn from a
    /// frozen document. It must start no work there: a surface that fetches,
    /// watches or spawns a worker is built without it and presents what the
    /// document holds.
    pub fn is_past(&self) -> bool {
        self.dispatch.is_past()
    }

    /// Whether this participant may only view, when the view is built.
    /// Bound controls already follow permission changes on their own; to show
    /// or disable something else while the participant may only view, bind to
    /// [`ViewCx::read_only_state`](super::ViewCx::read_only_state), which follows role changes while the app
    /// runs.
    pub fn read_only(&self) -> bool {
        self.dispatch.read_only()
    }

    /// Whether the record's latest edits are durable, for a status line.
    /// Saving is automatic; there is nothing to save by hand.
    pub fn save_status(&self) -> LocalState<RecordSaveStatus> {
        self.saved.clone()
    }

    /// What the view is built for: a window someone uses, a past version in
    /// History, or a screenshot. Start work that reaches outside the process
    /// (an engine, a socket, a probe) only when it [is live](RunMode::is_live).
    pub fn mode(&self) -> RunMode {
        self.run.mode
    }

    /// The capture's stand-in for a live source of type `V`
    /// ([`Fixture::provide`](super::Fixture::provide)); `None` outside a
    /// capture, or when the scenario provides none.
    pub fn fixture<V: 'static>(&self) -> Option<Rc<V>> {
        self.run.fixture::<V>()
    }

    /// The content store the host lent the app: opaque blobs by hash that
    /// outlive it. A capture's holds the blobs its scenario put there.
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        self.run.content()
    }

    /// What the launch that opened this window named — the file a person
    /// opened the app with, its command line — for a view that shows what
    /// the launch's actions ([`ResourceApp::launch`](super::ResourceApp::launch))
    /// made: the note a launch imported, opened. Empty for a plain start and
    /// for a past version.
    pub fn launch(&self) -> super::launch::LaunchInput<'_> {
        self.run.launch()
    }

    /// What the window's launch named (paths, URLs), for a view that acts
    /// on them beyond the actions the launch took: the file a save writes
    /// back to. Empty for a plain start, a past version and a capture.
    pub fn launch_arguments(&self) -> &[String] {
        &self.run.launched.arguments
    }

    /// The value of the app's own option `name` (`--purpose`) the window's
    /// launch named; see [`ResourceApp::OPTIONS`](super::ResourceApp::OPTIONS).
    /// `None` for a past version.
    pub fn launch_option(&self, name: &str) -> Option<&str> {
        super::launch::option(&self.run.launched.options, name)
    }

    /// Whether the window's launch named the app's switch `name`; see
    /// [`ResourceApp::FLAGS`](super::ResourceApp::FLAGS).
    pub fn launch_flag(&self, name: &str) -> bool {
        super::launch::flag(&self.run.launched.options, name)
    }

    /// The service of type `T` the host that mounts the app in its own
    /// process lent it ([`HostContext::lend`](ui_core::application::HostContext::lend)),
    /// taken once: the shell's connection to the session's process
    /// supervisor for a terminal it hosts. `None` in the app's own process,
    /// and for a past version or a capture, which start nothing.
    pub fn take_lent<S: 'static>(&self) -> Option<S> {
        if !self.run.mode.is_live() {
            return None;
        }
        self.run.launched.lent.take::<S>()
    }

    /// Whether the window is private ([`ResourceApp::PRIVATE_WINDOWS`](super::ResourceApp::PRIVATE_WINDOWS)):
    /// its record, settings and files are thrown away with the process. A
    /// view keeps what it would otherwise remember on the device (visits,
    /// a session to restore) to itself.
    pub fn is_private(&self) -> bool {
        self.run.private
    }

    /// The app's typed settings of type `S`, kept current while this scope
    /// lives. `S` declares whether they follow the person or stay on the
    /// device ([`Scope`](ui_core::storage::Scope)).
    pub fn settings<S>(&self) -> Setting<S>
    where
        S: ui_core::storage::SettingsSchema + Clone + PartialEq + Send + Sync,
    {
        self.storage.follow(self.alive.clone())
    }

    /// The device this window runs on: stable across launches, distinct on
    /// each of the person's devices. `None` in a view of a past version. An
    /// action that records where something happened (a speed test run on
    /// this device) takes it as an argument, so every replica applies the
    /// same value.
    pub fn device(&self) -> Option<&str> {
        self.storage.device()
    }

    /// The person's own record of `P` ([`PersonRecord`](super::PersonRecord)):
    /// theirs alone, kept in their own room and synced to their other
    /// devices, never to a collaborator, whatever room this record is in.
    /// `None` where the host keeps no records of the person's (a view of a
    /// past version) or the record cannot be opened.
    pub fn person<P: Resource>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.person::<P>()
    }

    /// The person's record `P` that one app keeps for all of theirs — their
    /// accounts, which Settings keeps: the same record on every device, held
    /// here too and converging with the keeper's over the person's own room.
    /// Changed only by its owner and editors
    /// ([`SharedRecord`](tilo_resource::SharedRecord),
    /// [`PersonRecord::editable`](super::PersonRecord::editable)); for its
    /// owner it is [`Self::person`].
    pub fn shared_person<P: tilo_resource::SharedRecord>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.shared_person::<P>()
    }

    /// The app's files, secrets and settings documents, as its host gave
    /// them. A secret of one document item is
    /// `cx.storage().secrets().item(purpose, id)`.
    pub fn storage(&self) -> &ui_core::storage::AppStorage {
        self.storage.storage()
    }

    /// Carry out the entries of the [`Outbox`](tilo_resource::Outbox) at
    /// `outbox` that this device owns, and record how each settled.
    ///
    /// Every replica holds the entries; `owns` says which are this device's
    /// to carry out (the device whose disk a file operation names, the one
    /// holding the mail account). For each such pending entry the framework
    /// runs `run` in the live app — never in History or a capture — at once
    /// or as a job through its [`EffectCx`], and records the answer with
    /// the outbox's settle edit: content from outside, nobody's undo step.
    /// The answer settles the entry: [`Effected::settled`](super::Effected::settled)
    /// with how it went, [`Effected::value`](super::Effected::value) with
    /// the result `O`, or [`Effected::done`](super::Effected::done) for
    /// `O`'s default. An `Err` leaves the entry pending, to run again after
    /// the next change or when the returned [`Projection`] is told the
    /// outside changed (the server came back); a failure for good is a
    /// [`Settled::Failed`](tilo_resource::Settled::Failed).
    pub fn outbox<E, O>(
        &self,
        outbox: tilo_resource::Path<R, tilo_resource::Outbox<E, O>>,
        owns: impl Fn(&R::State, &E) -> bool + 'static,
        run: impl FnMut(&tilo_resource::Id, E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        R: Resource<State = R> + tilo_resource::Model + 'static,
        E: tilo_resource::Model + Clone + 'static,
        O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
    {
        self.effects
            .outbox(self.alive.clone(), outbox, owns, run, false)
    }

    /// [`Self::outbox`], where an entry is the outside half of the action
    /// that queued it: one an admitted invocation on this device queues runs
    /// at once under that invocation — `cx.invocation()` names it, and its
    /// custody authorises what the handler does for it (a native host that
    /// acts only on behalf of an invocation) — and the invocation completes
    /// only once its entries settled, failing with a failed one. Entries
    /// queued elsewhere, replayed or still pending after a restart run as
    /// [`Self::outbox`] runs them, with no invocation.
    pub fn outbox_awaited<E, O>(
        &self,
        outbox: tilo_resource::Path<R, tilo_resource::Outbox<E, O>>,
        owns: impl Fn(&R::State, &E) -> bool + 'static,
        run: impl FnMut(&tilo_resource::Id, E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        R: Resource<State = R> + tilo_resource::Model + 'static,
        E: tilo_resource::Model + Clone + 'static,
        O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
    {
        self.effects
            .outbox(self.alive.clone(), outbox, owns, run, true)
    }
}
