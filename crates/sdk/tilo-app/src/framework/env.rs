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
    pub(crate) saves: super::saves::Saves,
    pub(crate) run: Run,
    pub(crate) storage: Rc<ViewStorage>,
    pub(crate) effects: Effects<R>,
}

/// One of the documents the app's windows started in the workspace
/// ([`RecordEnv::window_documents`]).
pub struct WindowDocument<R: Resource> {
    /// The record it is.
    pub record: ui_core::record::RecordId,
    /// What it holds, as last kept.
    pub state: Rc<R::State>,
    /// When it last changed, in milliseconds since the epoch; `0` when no
    /// change of it says.
    pub changed_ms: u64,
}

impl<R: Resource> WindowDocument<R> {
    /// The launch argument naming it, for [`RecordEnv::open_here`].
    pub fn uri(&self) -> String {
        self.record.uri()
    }
}

impl<R: Resource> Clone for WindowDocument<R> {
    fn clone(&self) -> Self {
        Self {
            record: self.record.clone(),
            state: Rc::clone(&self.state),
            changed_ms: self.changed_ms,
        }
    }
}

impl<R: Resource> Clone for RecordEnv<R> {
    fn clone(&self) -> Self {
        let Shared {
            alive,
            saved,
            saves,
            run,
            storage,
        } = Shared::of(
            &self.alive,
            &self.saved,
            (&self.saves, &self.run, &self.storage),
        );
        Self {
            live: self.live.clone(),
            alive,
            dispatch: self.dispatch.clone(),
            saved,
            saves,
            run,
            storage,
            effects: self.effects.clone(),
        }
    }
}

/// What a [`RecordEnv`] holds that does not name its resource type, cloned
/// here once rather than in every app's clone of it.
struct Shared {
    alive: Weak<()>,
    saved: LocalState<RecordSaveStatus>,
    saves: super::saves::Saves,
    run: Run,
    storage: Rc<ViewStorage>,
}

impl Shared {
    #[inline(never)]
    fn of(
        alive: &Weak<()>,
        status: &LocalState<RecordSaveStatus>,
        (saves, run, storage): (&super::saves::Saves, &Run, &Rc<ViewStorage>),
    ) -> Self {
        Self {
            alive: alive.clone(),
            saved: status.clone(),
            saves: saves.clone(),
            run: run.clone(),
            storage: Rc::clone(storage),
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

    /// The documents this app's windows started in this workspace, each as
    /// last kept, the most recently changed first, without the one this
    /// window shows: what an app whose windows each show a document of their
    /// own ([`ResourceApp::WINDOW_RESOURCE`](super::ResourceApp::WINDOW_RESOURCE))
    /// lists, so a document made in a window that was closed is reached
    /// again. Open one here with [`Self::open_here`] naming
    /// [`WindowDocument::uri`]. Reads every one of them, so ask when the
    /// list is shown, not on every frame. Empty in a past version and where
    /// the window keeps no records in a workspace.
    pub fn window_documents(&self) -> Vec<WindowDocument<R>> {
        if self.is_past() {
            return Vec::new();
        }
        let Some(store) = self.storage.room_store() else {
            return Vec::new();
        };
        let kind = ui_core::record::RecordKind::new(R::KIND);
        let shown = self.run.launched.record.as_ref();
        let mut documents: Vec<WindowDocument<R>> = store
            .window_records(&kind)
            .into_iter()
            .filter(|(record, _)| Some(record) != shown)
            .filter_map(|(record, mut doc)| {
                let changed_s = doc
                    .get_heads()
                    .iter()
                    .filter_map(|head| doc.get_change_by_hash(head).map(|change| change.timestamp()))
                    .max()
                    .unwrap_or(0);
                let state = tilo_resource::Reader::<R>::read(&doc)
                    .map_err(|error| {
                        tracing::warn!(record = %record, %error, "window document unreadable; left out");
                    })
                    .ok()?
                    .state()
                    .clone();
                Some(WindowDocument {
                    record,
                    state,
                    changed_ms: u64::try_from(changed_s).unwrap_or(0).saturating_mul(1000),
                })
            })
            .collect();
        documents.sort_by_key(|document| std::cmp::Reverse(document.changed_ms));
        documents
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

    /// The chain of `P` the app keeps beside its record
    /// ([`Chain`](super::Chain)): its parts, read as they change, and its
    /// dispatcher. `None` in a view of a past version, or where `P`'s base
    /// is not this record's resource.
    pub fn chain<P: super::Chained<Base = R>>(&self) -> Option<super::Chain<P>> {
        let live = self.live.clone();
        let state = self.storage.chain::<P>(move || live.get())?;
        let applying = Rc::downgrade(&state);
        let me = self.dispatch.me()?.to_owned();
        let dispatch = self.dispatch.for_room_record::<P>(
            state.id(),
            self.dispatch.is_past(),
            move |action| {
                let state = applying.upgrade().ok_or_else(|| {
                    tilo_actions::ActionError::Refused("the chain is closed".into())
                })?;
                state
                    .apply_as(action, &tilo_actions::ActionInvoker::actor(&me))
                    .map(drop)
            },
        );
        Some(super::Chain::new(state, dispatch))
    }

    /// The children of `C` this record lists ([`Children`](super::Children)):
    /// each a record of its own, read as they change, with the one
    /// lifecycle every container has. `None` in a view of a past version,
    /// or where `C`'s parent is not this record's resource.
    pub fn children<C: super::ChildRecord<Parent = R>>(&self) -> Option<super::Children<C>> {
        let live = self.live.clone();
        let state = self.storage.children::<C>(move || live.get())?;
        let me = self.dispatch.me()?.to_owned();
        Some(super::Children::new(state, me).dispatching(&self.dispatch))
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

    /// Whether the record's latest edits are durable, for automation and
    /// for a view that presents the save status itself
    /// ([`Saves::shown_by_view`](super::Saves::shown_by_view)). Saving is
    /// automatic and the window already says what there is to say (only a
    /// save that stalls or fails): never show this as "Saving…" or "Saved".
    pub fn save_status(&self) -> LocalState<RecordSaveStatus> {
        self.saved.clone()
    }

    /// The window's report of saves the app makes outside its record (a
    /// file it writes), presented with the record's own on the window's
    /// save status. See [`Saves`](super::Saves).
    pub fn saves(&self) -> super::Saves {
        self.saves.clone()
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

    /// Read the result of the view action at `target` with `argument`,
    /// through the session's action door as the person using the app, and
    /// keep it current ([`Query`](super::Query)): a live action's answer
    /// follows its provider's changes, one that answers once is read once.
    /// It is followed while this scope lives.
    pub fn query<T>(
        &self,
        target: tilo_actions::remote::RemoteActionTarget,
        argument: tilo_actions::remote::InvokeArgument,
    ) -> super::Query<T>
    where
        T: serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        if !self.run.mode.is_live() {
            // A past version or a capture reaches no provider: a capture
            // reads what its scenario answers for the action.
            let answers = self.run.fixture::<super::scenario::Answers>();
            return super::Query::fixed(
                answers.as_deref().and_then(|answers| answers.get(&target)),
            );
        }
        super::Query::start(self.dispatch.opener(), &self.alive, target, argument)
    }

    /// Invoke the action at `target` of another provider with `argument`,
    /// through the session's action door as the person using the app: a
    /// write a [`Self::query`] then reads back. Why it did not go ahead is
    /// told as any refused call of the window is. A past version or a
    /// capture invokes nothing.
    pub fn invoke(
        &self,
        target: tilo_actions::remote::RemoteActionTarget,
        argument: tilo_actions::remote::InvokeArgument,
    ) {
        if self.run.mode.is_live() {
            self.dispatch.opener().invoke(target, argument);
        }
    }

    /// The person's own record of `P` ([`PersonRecord`](super::PersonRecord)):
    /// theirs alone, kept in their own room and synced to their other
    /// devices, never to a collaborator, whatever room this record is in.
    /// `None` where the host keeps no records of the person's (a view of a
    /// past version) or the record cannot be opened.
    pub fn person<P: Resource>(&self) -> Option<super::PersonRecord<P>> {
        self.storage.person::<P>()
    }

    /// The record `P` that `owner` keeps in the window's room and every app
    /// of the room opens (`collab_types::WORKSPACE_RECORDS`): the
    /// workspace's emoji, which are Chat's. Changes to it are decided by the
    /// room's grants on `owner`'s resource. `None` where the host keeps no
    /// such records (a view of a past version) or it cannot be opened.
    pub fn workspace<P: Resource>(&self, owner: &'static str) -> Option<super::WorkspaceRecord<P>> {
        self.storage.workspace::<P>(owner)
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
