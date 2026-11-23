//! What a view and a custom surface both reach: the record's dispatcher
//! and live state, the run it is part of, its storage and effects.
//! [`ViewCx`](super::ViewCx) and [`SurfaceCx`](super::SurfaceCx) dereference
//! to it, so each reads the same accessors.

use std::rc::{Rc, Weak};

use tilo_resource::Resource;
use ui_core::LocalState;
use ui_core::resource::ResourceSaveStatus;

use super::dispatch::Dispatcher;
use super::effect::{EffectAnswer, EffectCx, Effects, Projection};
use super::live::Live;
use super::scenario::{Run, RunMode};
use super::settings::{Setting, ViewStorage};

/// The record, run and storage a view or surface works in. See the module
/// docs.
pub struct ResourceEnv<R: Resource> {
    pub(crate) live: Live<R>,
    pub(crate) alive: Weak<()>,
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) saved: LocalState<ResourceSaveStatus>,
    pub(crate) saves: super::saves::Saves,
    pub(crate) run: Run,
    pub(crate) storage: Rc<ViewStorage>,
    pub(crate) effects: Effects<R>,
}

/// One of the documents the app's workspace holds, as the app's windows show
/// them ([`ResourceEnv::window_documents`]).
pub struct WindowDocument<R: Resource> {
    /// The record it is.
    pub record: ui_core::resource::DocumentId,
    /// What it holds, as last kept.
    pub state: Rc<R::State>,
    /// What the workspace's index calls it, when it names it: a document an
    /// app window on another device or an agent in a hosted workspace made
    /// is one no window here named.
    pub name: Option<String>,
    /// When it last changed, in milliseconds since the epoch; `0` when no
    /// change of it says.
    pub changed_ms: u64,
}

impl<R: Resource> WindowDocument<R> {
    /// The launch argument naming it, for [`ResourceEnv::open_here`].
    pub fn uri(&self) -> String {
        self.record.uri()
    }
}

impl<R: Resource> Clone for WindowDocument<R> {
    fn clone(&self) -> Self {
        Self {
            record: self.record.clone(),
            state: Rc::clone(&self.state),
            name: self.name.clone(),
            changed_ms: self.changed_ms,
        }
    }
}

impl<R: Resource> Clone for ResourceEnv<R> {
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

/// What a [`ResourceEnv`] holds that does not name its resource type, cloned
/// here once rather than in every app's clone of it.
struct Shared {
    alive: Weak<()>,
    saved: LocalState<ResourceSaveStatus>,
    saves: super::saves::Saves,
    run: Run,
    storage: Rc<ViewStorage>,
}

impl Shared {
    #[inline(never)]
    fn of(
        alive: &Weak<()>,
        status: &LocalState<ResourceSaveStatus>,
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

impl<R: Resource> ResourceEnv<R> {
    /// Wakes the app from another thread — a worker's result, a socket's
    /// message — so the window syncs and draws it without waiting for
    /// input.
    pub fn waker(&self) -> ui_core::WakeHandle {
        self.effects
            .env()
            .map_or_else(ui_core::WakeHandle::new, |env| env.wake.clone())
    }

    /// The same environment for a scope that lives as long as `alive`.
    pub(crate) fn living(&self, alive: &Rc<()>) -> Self {
        Self {
            alive: Rc::downgrade(alive),
            ..self.clone()
        }
    }

    /// Open, in this window and in place of the record it shows, what a
    /// launch of the app naming `arguments` opens: the record
    /// [`ResourceApp::launch_resource`](super::ResourceApp::launch_resource)
    /// names, with its launch actions — the document a person chose with
    /// the app's own Open. The window's view is rebuilt for that record on
    /// the next frame. Refused in a past version and a capture.
    ///
    /// The outcome answers once the host opened it, or why it could not.
    pub fn open_here(
        &self,
        arguments: &[String],
    ) -> Result<ui_core::application::ReplacementOutcome, String> {
        self.run.open_here(arguments)
    }

    /// The host-owned identity of the record this window shows. A durable
    /// activation link can name it without relying on the last-open window.
    /// Absent in a historical view that cannot launch live work.
    pub fn document_id(&self) -> Option<&ui_core::resource::DocumentId> {
        self.run.launched.record.as_ref()
    }

    /// What [`Self::open_here`] does, kept for later: the input that asks
    /// for another record arrives after the view is built (a file chosen in
    /// a picker, a clicked notification). `None` in a past version and a
    /// capture.
    pub fn opener(&self) -> Option<super::OpenHere> {
        self.run.opener.clone()
    }

    /// The documents this app's workspace holds, each as last kept, the most
    /// recently changed first, without the one this window shows: what an app
    /// whose windows each show a document of their own
    /// ([`ResourceApp::WINDOW_RESOURCE`](super::ResourceApp::WINDOW_RESOURCE))
    /// lists, so a document is reached again wherever it was made — in a
    /// window here that was closed, a window on another device, or by an
    /// agent in a hosted workspace (one the workspace names in its index,
    /// through [`WindowDocument::name`]). Open one here with
    /// [`Self::open_here`] naming [`WindowDocument::uri`]. Reads every one of
    /// them, so ask when the list is shown, not on every frame. Empty in a
    /// past version and where the window keeps no records in a workspace.
    pub fn window_documents(&self) -> Vec<WindowDocument<R>> {
        if self.is_past() {
            return Vec::new();
        }
        let Some(store) = self.storage.workspace_store() else {
            return Vec::new();
        };
        let kind = ui_core::resource::ResourceType::new(R::KIND);
        let shown = self.run.launched.record.as_ref();
        let mut documents: Vec<WindowDocument<R>> = store
            .workspace_resources(&kind)
            .into_iter()
            .filter(|(record, _, _)| Some(record) != shown)
            .filter_map(|(record, mut doc, name)| {
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
                    name,
                    changed_ms: u64::try_from(changed_s).unwrap_or(0).saturating_mul(1000),
                })
            })
            .collect();
        documents.sort_by_key(|document| std::cmp::Reverse(document.changed_ms));
        documents
    }

    /// The names of the members, for showing the ids actions record
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
    /// draft, a SQL buffer. It is published with this resource, at `id`, so
    /// an agent or a script addresses it too. See [`Dispatcher::child`].
    pub fn child<C: Resource>(
        &self,
        id: &str,
        wrap: impl Fn(C::Action) -> R::Action + Send + Sync + 'static,
    ) -> Dispatcher<C> {
        self.dispatch.child(id, wrap)
    }

    /// The chain of `P` the app keeps beside its record
    /// ([`Chain`](super::Chain)): its parts, read as they change, and its
    /// dispatcher. `None` in a view of a past version, or where `P`'s base
    /// is not this record's resource.
    pub fn chain<P: super::Chained<Base = R>>(&self) -> Option<super::Chain<P>> {
        let live = self.live.clone();
        let record = self.document_id().cloned();
        let state = self
            .storage
            .chain::<P>(record.as_ref(), move || live.get())?;
        Some(self.chain_of(state))
    }

    /// The chain of `P` kept beside the record `beside` rather than beside
    /// this window's own ([`Chained::chain_beside`](super::Chained::chain_beside)):
    /// the messages of one direct conversation, kept in the record the two
    /// people share. `P`'s base is the type of `beside`'s record.
    pub fn chain_beside<P: super::Chained<Base = R>>(
        &self,
        beside: &super::WorkspaceResource<R>,
    ) -> Option<super::Chain<P>> {
        let live = beside.live();
        let state = self
            .storage
            .chain_beside::<P>(beside.id(), move || live.get())?;
        Some(self.chain_of(state))
    }

    /// The chain held by its state, with the dispatcher that applies its
    /// actions as the window's person.
    fn chain_of<P: super::Chained>(&self, state: Rc<super::chain::ChainState>) -> super::Chain<P> {
        // A past version is read as the person whose store holds its parts.
        let me = self.dispatch.me().unwrap_or_else(|| state.reader()).clone();
        // A role that may only view the workspace views the parts too; a
        // member's own record (Mail's) is theirs to write, so its window's
        // dispatcher is not read-only.
        let read_only = self.dispatch.read_only() || self.dispatch.is_past();
        let dispatch = super::chain::dispatching(&state, &me, |apply, group| {
            self.dispatch
                .for_workspace_resource::<P>(state.id(), read_only, apply, group)
        });
        super::Chain::new(state, dispatch)
    }

    /// The children of `C` this record lists ([`Children`](super::Children)):
    /// each a record of its own, read as they change, with the one
    /// lifecycle every container has. `None` in a view of a past version,
    /// or where `C`'s parent is not this record's resource.
    pub fn children<C: super::ChildResource<Parent = R>>(&self) -> Option<super::Children<C>> {
        let live = self.live.clone();
        let state = self.storage.children::<C>(move || live.get())?;
        let me = self.dispatch.me()?.clone();
        Some(super::Children::new(state, me).dispatching(&self.dispatch))
    }

    /// Who this view is for: the member reading and editing through
    /// it, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). Compare the
    /// authors the resource records with it ("your message", "you reacted")
    /// instead of keeping the reader in the resource state, which is the
    /// same for everyone. `None` in a view of a past version.
    pub fn me(&self) -> Option<&tilo_resource::MemberId> {
        self.dispatch.me()
    }

    /// Whether this view shows a past version in History mode, drawn from a
    /// frozen document. It must start no work there: a surface that fetches,
    /// watches or spawns a worker is built without it and presents what the
    /// document holds.
    pub fn is_past(&self) -> bool {
        self.dispatch.is_past()
    }

    /// Whether this member may only view, when the view is built.
    /// Bound controls already follow permission changes on their own; to show
    /// or disable something else while the member may only view, bind to
    /// [`ViewCx::read_only_state`](super::ViewCx::read_only_state), which follows role changes while the app
    /// runs.
    pub fn read_only(&self) -> bool {
        self.dispatch.read_only()
    }

    /// Whether the record's latest edits are durable, for automation and
    /// an accessibility status. Saving is automatic and the window already
    /// says what there is to say (a save that stalls or fails, and the
    /// document's unsaved mark then): never show this as "Saving…" or
    /// "Saved", nor mark the document edited from it.
    pub fn save_status(&self) -> LocalState<ResourceSaveStatus> {
        self.saved.clone()
    }

    /// The window's report of saves the app makes outside its record (a
    /// file it writes), presented with the record's own on the window's
    /// save status. See [`Saves`](super::Saves).
    pub fn saves(&self) -> super::Saves {
        self.saves.clone()
    }

    /// The workspace the window keeps its records in: what an app keys what it keeps on
    /// this device for the workspace by (a cache, an index). `None` in a past version and where the
    /// window keeps no records in a workspace.
    pub fn workspace(&self) -> Option<String> {
        if self.is_past() {
            return None;
        }
        self.storage
            .workspace_store()
            .map(ui_core::resource::ResourceStore::home)
    }

    /// Whether this window is on the current person's private desktop.
    pub fn on_personal_desktop(&self) -> bool {
        let Some(cluster) = self
            .me()
            .and_then(tilo_resource::MemberId::person_id)
            .and_then(ui_core::resource::person_workspace_of)
        else {
            return false;
        };
        self.workspace()
            .is_some_and(|workspace| workspace == cluster.to_string())
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

    /// Where the window says which file or folder it shows, so it opens on that again after a
    /// restart: the file a launch opened it on, the folder it went to. `None` for a past version.
    pub fn window_shows(&self) -> Option<ui_core::application_view::WindowShows> {
        self.run.launched.shows.clone()
    }

    /// The window shows the file at `path` from now on: what it opens on again after a restart.
    /// An app says this when its document was saved somewhere new (Save As), so a document saved
    /// elsewhere is not reopened where it used to be.
    pub fn show_file(&self, path: &std::path::Path) {
        if let Some(shows) = self.window_shows() {
            shows.show(path);
        }
    }

    /// The folder History lists, for an app that declares
    /// [`ResourceApp::FOLDER_HISTORY`](super::ResourceApp::FOLDER_HISTORY): narrow it to the
    /// file the window shows (the selected one) and open it from the window's own History
    /// command. `None` for any other app, and in a past version.
    pub fn folder_history(&self) -> Option<ui_core::history_mode::folder::FolderHistorySource> {
        self.run
            .folder_history
            .as_ref()
            .map(|(source, _)| source.clone())
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
    /// process lent it ([`ProcessContext::lend`](ui_core::application::ProcessContext::lend)),
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

    /// Whether this is a view another app's window embeds
    /// ([`ui_core::window_share`]) and not a window of its own: it stands
    /// inside that window, which has the bar, so it shows the one thing it
    /// was opened on and nothing to move elsewhere from (no list of the
    /// record's other items, no title of its own).
    pub fn is_embedded(&self) -> bool {
        self.run.embedded
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
    /// through the session's action API as the person using the app, and
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

    /// Keep a record another app keeps in the window's workspace current:
    /// `read` as the host answers it now and again after every write to it,
    /// through the session's record API as the person using the app, who
    /// must be allowed to read it. The answer is the readings every reader
    /// gets (`{"revision", "records": [{"record", "value" | "absent" |
    /// "arriving"}]}`), and fails with the host's reason while the record
    /// cannot be read, or once this person may no longer read it; build what
    /// shows with [`ViewCx::arriving`](super::ViewCx::arriving). Nothing
    /// runs between writes: the host holds the next read until the record
    /// differs. It is followed while this scope lives. A past version or a
    /// capture reads nothing.
    pub fn follow_resource(
        &self,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> ui_core::ResourceReadCall {
        if !self.run.mode.is_live() {
            return ui_core::arriving::Arriving::pending(ui_core::arriving::Source::Local);
        }
        let opener = self.dispatch.opener();
        let followed = opener.follow_resource(read);
        let readings = followed.call().clone();
        opener.keep(self.alive.clone(), Rc::new(followed));
        readings
    }

    /// Invoke the action at `target` of another provider with `argument`,
    /// through the session's action API as the person using the app: a
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

    /// The record `P` the person keeps for themselves, in their own scope
    /// rather than in the window's workspace: presentation data only they use
    /// on their own devices, such as the signature they sign documents with or
    /// the words they taught the on-screen keyboard. No other member reads or
    /// changes it, and it reaches the person's other devices with their
    /// personal store. A record the workspace keeps is
    /// [`Self::shared_workspace`]'s.
    pub fn profile_resource<P: Resource>(&self) -> Option<super::PersonResource<P>> {
        self.storage.profile_resource::<P>()
    }

    /// The record `P` that `owner` keeps in the window's workspace and every app
    /// of the workspace opens (`collab_types::WORKSPACE_DOCUMENTS`): the
    /// workspace's emoji, which are Chat's. Changes to it are decided by the
    /// workspace's grants on `owner`'s resource. `None` where the host keeps no
    /// such records (a view of a past version) or it cannot be opened.
    ///
    /// `None` too while the workspace has not delivered it to this device. A part of the
    /// view ([`ViewCx::switch`](super::ViewCx::switch)) that asks for it then is built
    /// again, it alone, once it is here: a view reads it where it shows it, and need not
    /// ask twice. A view's root is not built again, so what outlives one answer (an effect,
    /// a widget of the app's own) asks through [`Self::resource_opener`], which answers
    /// with the document once it is here.
    pub fn shared_workspace<P: Resource>(
        &self,
        owner: &'static str,
    ) -> Option<super::WorkspaceResource<P>> {
        self.storage.shared_workspace::<P>(owner)
    }

    /// The record `id` of `P` that `owner` keeps in the window's workspace,
    /// named otherwise than [`Self::shared_workspace`]'s: a conversation's,
    /// which the two people it is between name. Held and decided like any
    /// record of the owner's: the workspace's grants on `owner` decide every
    /// change. `None` where the host keeps no records in a workspace (a view
    /// of a past version), for an id that names no record, or where it
    /// cannot be opened.
    pub fn shared_resource<P: Resource>(
        &self,
        owner: &'static str,
        id: &str,
    ) -> Option<super::WorkspaceResource<P>> {
        self.storage.shared_resource::<P>(owner, id)
    }

    /// Opens the record `id` of `P` that `owner` keeps, later, from a widget that outlives this
    /// view: a page's Files window opens each chunk of the workspace's tree as the tree lists it
    /// ([`ResourceOpener::open`](super::ResourceOpener::open)).
    pub fn resource_opener<P: Resource>(&self, owner: &'static str) -> super::ResourceOpener<P> {
        super::ResourceOpener::new(Rc::clone(&self.storage), owner)
    }

    /// The window's own route to the session's services, where it runs in one: what an app
    /// asks the session for what is the session's to decide (Mail an account's password,
    /// Settings keeping one, Browser a website's session). `None` outside a session, in a past
    /// version and in a capture.
    pub fn session(&self) -> Option<ui_core::collaboration::browser_service_sessions::Client> {
        if self.is_past() {
            return None;
        }
        self.storage.session()
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

    /// Carry out the outside work the parts of `chain` owe, on this device
    /// when it owns it: what a mail server or a calendar has not been told
    /// of the items a chain keeps, derived from each part as it is.
    ///
    /// `owed` reads one part's work, each job under the item it names; it is
    /// read again only for a part that changed, so a change costs what its
    /// part holds, never the whole chain. `owns` says which jobs are this
    /// device's (it holds the account), and `work` carries one out as
    /// [`Self::outbox`] carries out an entry. Nothing is written for a job:
    /// the app confirms what the outside now holds with an action on the
    /// part holding the item, after which the part owes it no more. A job is
    /// carried out once while it is owed; one that failed for now (`Err`) is
    /// tried again once the chain moved on or the returned [`Projection`] is
    /// told the outside changed.
    pub fn owed<P, J>(
        &self,
        chain: &super::Chain<P>,
        owed: impl Fn(&P::State) -> Vec<(tilo_resource::Id, J)> + 'static,
        owns: impl Fn(&J) -> bool + 'static,
        work: impl FnMut(&tilo_resource::Id, J, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection
    where
        R: 'static,
        P: super::Chained,
        J: serde::Serialize + Clone + 'static,
    {
        self.effects
            .owed(self.alive.clone(), chain.clone(), owed, owns, work)
    }

    /// [`Self::outbox`], where an entry is the outside half of the action
    /// that queued it: one an admitted invocation on this device queues runs
    /// at once under that invocation — `cx.invocation()` names it, and its
    /// ownership authorises what the handler does for it (a native host that
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
