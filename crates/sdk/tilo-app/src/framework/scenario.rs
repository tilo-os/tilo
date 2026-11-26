//! What the app is built for — a window someone uses, a past version, a
//! screenshot — and what a screenshot shows that the resource does not hold.
//!
//! A view is built in one [`RunMode`]. Only a [`Live`](RunMode::Live) view
//! starts work that reaches outside the process: a mail engine, a socket, a
//! journal reader, a hardware probe. History rebuilds the same view for every
//! past version it previews ([`Past`](RunMode::Past)), and a screenshot
//! ([`Capture`](RunMode::Capture)) must come out the same on every machine,
//! so neither may.
//!
//! A capture's scene is prepared before its view is built, by
//! [`ResourceApp::fixture`](super::ResourceApp::fixture): typed values that
//! stand in for live sources (a scan result, a fake backend, a fetched
//! page), a scratch directory, blobs in the content store, versions of the
//! resource written by other people at other times, and members shown
//! where they are in it. Views read the values with
//! [`ResourceEnv::fixture`](super::ResourceEnv::fixture), surfaces with
//! [`ResourceEnv::fixture`](super::ResourceEnv::fixture).

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_resource::{Doc, Invoke, Resource};
use ui_core::history_mode::HistoryDefinition;

use super::surface::ResourceSelection;

/// What a view is built for. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// A window someone uses: real sources, and the work that keeps them
    /// current.
    Live,
    /// A past version, previewed read-only in History mode. The view is
    /// built again for every version shown; it starts nothing.
    Past,
    /// A screenshot (`--shots`, `--shots-dir`, `--responsive-matrix`): the
    /// scenario's fixtures instead of devices, networks and clocks, and
    /// nothing that reaches outside the process.
    Capture,
}

impl RunMode {
    /// Whether the view may start work that reaches outside the process.
    pub fn is_live(self) -> bool {
        self == Self::Live
    }
}

/// The values a capture's views and surfaces read in place of live sources.
#[derive(Default)]
pub(crate) struct Fixtures {
    values: HashMap<TypeId, Rc<dyn Any>>,
    members: Vec<Shown>,
    content: Option<tilo_content::ContentStore>,
    /// Kept for as long as anything built from the capture is.
    _directory: Option<tempfile::TempDir>,
}

/// The mode a root was built in, and what its views read in it.
#[derive(Clone)]
pub(crate) struct Run {
    pub(crate) mode: RunMode,
    pub(crate) fixtures: Rc<Fixtures>,
    /// What the launch that opened the window named.
    pub(crate) launched: Rc<Launched>,
    /// A private window: a profile thrown away with the process.
    pub(crate) private: bool,
    /// A view another app's window embeds, not a window of its own.
    pub(crate) embedded: bool,
    /// The folder History lists, for an app that declares
    /// [`ResourceApp::FOLDER_HISTORY`](super::ResourceApp::FOLDER_HISTORY): filled with the
    /// workspace's tree once the root's storage is open, and observed by the root under
    /// the app's History scope.
    pub(crate) folder_history: Option<(
        ui_core::history_mode::folder::FolderHistorySource,
        &'static HistoryDefinition,
    )>,
    /// The names of the members, as the host's roster has them.
    pub(crate) people: super::People,
    /// Opens another record in this window, in a window someone uses.
    pub(crate) opener: Option<super::launch::OpenHere>,
    /// A capture a headless drive invokes actions on
    /// ([`Capture::driven`](ui_core::application::Capture::driven)): the
    /// effects it is asked for run, though it starts no work on its own.
    pub(crate) driven: bool,
    /// What the workspace holds, when the record the window opened was not
    /// on this device: the view shows once it arrived.
    pub(crate) workspace:
        Option<tilo_ui_base::arriving::Arriving<(), tilo_ui_base::workspace_sync::WorkspaceUnreachable>>,
}

/// The arguments, the app's own options, the file and the playback handoff
/// a launch handed the
/// app.
#[derive(Default)]
pub(crate) struct Launched {
    /// The record selected by the host for this launch.
    pub(crate) record: Option<ui_core::resource::DocumentId>,
    pub(crate) arguments: Vec<String>,
    pub(crate) options: Vec<ui_core::application::LaunchOption>,
    pub(crate) file: Option<ui_core::application::OpenFile>,
    pub(crate) handoff: Option<ui_core::application::PlaybackHandoff>,
    /// The services the host that mounts the app in-process lent it.
    pub(crate) lent: Lent,
    /// The item the launch asked to show ([`LaunchTarget::Record`]'s
    /// `reveal`), until a view that opens items has shown it.
    ///
    /// [`LaunchTarget::Record`]: ui_core::application::LaunchTarget::Record
    pub(crate) reveal: std::cell::RefCell<Option<String>>,
    /// Where the window says which file or folder it shows.
    pub(crate) shows: Option<ui_core::application_view::WindowShows>,
}

/// What an in-process host lent the app
/// ([`ProcessContext::lend`](ui_core::application::ProcessContext::lend)), each
/// taken once by the view or surface that uses it.
#[derive(Default)]
pub(crate) struct Lent(std::cell::RefCell<Vec<Box<dyn Any>>>);

impl Lent {
    pub(crate) fn new(services: Vec<Box<dyn Any>>) -> Self {
        Self(std::cell::RefCell::new(services))
    }

    /// The lent service of type `T`, once.
    pub(crate) fn take<T: 'static>(&self) -> Option<T> {
        let mut services = self.0.borrow_mut();
        let index = services.iter().position(|service| service.is::<T>())?;
        services
            .remove(index)
            .downcast::<T>()
            .ok()
            .map(|service| *service)
    }
}

impl Run {
    pub(crate) fn live(content: Option<tilo_content::ContentStore>) -> Self {
        Self {
            mode: RunMode::Live,
            fixtures: Rc::new(Fixtures {
                content,
                ..Fixtures::default()
            }),
            launched: Rc::default(),
            private: false,
            embedded: false,
            folder_history: None,
            people: super::People::new(None),
            opener: None,
            driven: false,
            workspace: None,
        }
    }

    /// This run in a private window, when `private`.
    pub(crate) fn in_private(mut self, private: bool) -> Self {
        self.private = private;
        self
    }

    /// This run as a view another app's window embeds, when `embedded`.
    pub(crate) fn in_embedded(mut self, embedded: bool) -> Self {
        self.embedded = embedded;
        self
    }

    /// This run, listing the changes of the folder `folder_history` reads.
    pub(crate) fn showing_folder_history(
        mut self,
        folder_history: Option<(
            ui_core::history_mode::folder::FolderHistorySource,
            &'static HistoryDefinition,
        )>,
    ) -> Self {
        self.folder_history = folder_history;
        self
    }

    pub(crate) fn past() -> Self {
        Self {
            mode: RunMode::Past,
            fixtures: Rc::default(),
            launched: Rc::default(),
            private: false,
            embedded: false,
            folder_history: None,
            people: super::People::new(None),
            opener: None,
            driven: false,
            workspace: None,
        }
    }

    /// The same run, a capture a headless drive invokes actions on when
    /// `driven`.
    pub(crate) fn driven_by_drive(mut self, driven: bool) -> Self {
        self.driven = driven;
        self
    }

    /// The same run, its view waiting for what the workspace holds
    /// (`workspace`) when its record was not on this device.
    pub(crate) fn awaiting_workspace(
        mut self,
        workspace: Option<
            tilo_ui_base::arriving::Arriving<(), tilo_ui_base::workspace_sync::WorkspaceUnreachable>,
        >,
    ) -> Self {
        self.workspace = workspace;
        self
    }

    /// The same run, able to open another record in its window.
    pub(crate) fn opening_with(mut self, opener: Option<super::launch::OpenHere>) -> Self {
        self.opener = opener;
        self
    }

    /// Open what a launch naming `arguments` opens in this window.
    pub(crate) fn open_here(
        &self,
        arguments: &[String],
    ) -> Result<ui_core::application::ReplacementOutcome, String> {
        Ok(self
            .opener
            .as_ref()
            .ok_or_else(|| "this window shows the record it was opened on".to_owned())?
            .open(arguments))
    }

    pub(crate) fn for_member(mut self, me: Option<&tilo_resource::MemberId>) -> Self {
        self.people = super::People::new(me);
        self
    }

    /// The same run, opened by a launch that named `launched`.
    pub(crate) fn launched(mut self, launched: Launched) -> Self {
        self.launched = Rc::new(launched);
        self
    }

    pub(crate) fn launch(&self) -> super::launch::LaunchInput<'_> {
        super::launch::LaunchInput::new(&self.launched.arguments, self.launched.file.as_ref())
            .with_content(self.fixtures.content.as_ref())
            .with_handoff(self.launched.handoff.as_ref())
            .with_options(&self.launched.options)
    }

    pub(crate) fn fixture<T: 'static>(&self) -> Option<Rc<T>> {
        let value = self.fixtures.values.get(&TypeId::of::<T>())?;
        Rc::clone(value).downcast::<T>().ok()
    }
    pub(crate) fn content(&self) -> Option<tilo_content::ContentStore> {
        self.fixtures.content.clone()
    }

    /// The members a capture shows, where they are in the resource.
    pub(crate) fn members(&self) -> &[Shown] {
        &self.fixtures.members
    }
}

/// Someone a capture shows in the resource.
#[derive(Clone, Debug)]
pub(crate) enum Shown {
    /// A member by name, where their window says they are.
    Member(String, ResourceSelection),
    /// An agent whose own view of this window stands where its action left it.
    Agent(super::Landing),
}

/// What other providers' actions answer in a capture
/// ([`Fixture::answer`]), by the action.
#[derive(Clone, Default)]
pub(crate) struct Answers(HashMap<String, serde_json::Value>);

impl Answers {
    fn key(target: &tilo_actions::remote::RemoteActionTarget) -> String {
        format!(
            "{}/{}/{}",
            target.provider, target.address.resource, target.address.action
        )
    }

    /// What the action at `target` answers in this capture.
    pub(crate) fn get(
        &self,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Option<&serde_json::Value> {
        self.0.get(&Self::key(target))
    }
}

/// A version of the resource another person wrote before the capture.
struct Version<R: Resource> {
    author: tilo_resource::MemberId,
    at_ms: i64,
    invoke: Invoke<R>,
}

/// The scene of one screenshot scenario that its actions cannot stage:
/// handed to [`ResourceApp::fixture`](super::ResourceApp::fixture) before
/// the view is built.
pub struct Fixture<R: Resource> {
    scenario: Option<String>,
    /// The member the capture's window reads as
    /// ([`ResourceEnv::me`](super::ResourceEnv::me)), for a scene with
    /// something of their own in it.
    person: tilo_resource::MemberId,
    values: HashMap<TypeId, Rc<dyn Any>>,
    /// What the record starts with, written before the versions.
    start: Option<Start>,
    history: Vec<Version<R>>,
    members: Vec<Shown>,
    content: Option<tilo_content::ContentStore>,
    directory: Option<tempfile::TempDir>,
}

/// Writes a capture's starting state into the record.
type Start = Box<dyn Fn(&mut Doc) -> Result<(), String>>;

impl<R: Resource> Fixture<R> {
    pub(crate) fn new(
        scenario: Option<&str>,
        person: &tilo_resource::MemberId,
        content: Option<tilo_content::ContentStore>,
    ) -> Self {
        Self {
            scenario: scenario.map(str::to_owned),
            person: person.clone(),
            values: HashMap::new(),
            start: None,
            history: Vec::new(),
            members: Vec::new(),
            content,
            directory: None,
        }
    }

    /// The scenario being captured; `None` for a plain `--shots`.
    pub fn scenario(&self) -> Option<&str> {
        self.scenario.as_deref()
    }

    /// Who the capture's window is for: the member the view reads as
    /// its own ([`ResourceEnv::me`](super::ResourceEnv::me)). A scene that holds
    /// something of the person's own — a seat at the table, an issue
    /// assigned to them — stages it for them.
    pub fn person(&self) -> &tilo_resource::MemberId {
        &self.person
    }

    /// Stand `value` in for a live source: the views and surfaces of this
    /// capture read it with `cx.fixture::<T>()`. One value per type; a
    /// second replaces the first.
    pub fn provide<T: 'static>(&mut self, value: T) {
        self.values.insert(TypeId::of::<T>(), Rc::new(value));
    }

    /// Stand `value` in for what another provider's action at `target`
    /// answers: a view's [`ResourceEnv::query`](super::ResourceEnv::query) of
    /// it reads `value`, as the action's result, in this capture.
    pub fn answer(
        &mut self,
        target: &tilo_actions::remote::RemoteActionTarget,
        value: &impl serde::Serialize,
    ) {
        let mut answers = self
            .values
            .remove(&TypeId::of::<Answers>())
            .and_then(|answers| answers.downcast::<Answers>().ok())
            .map(|answers| (*answers).clone())
            .unwrap_or_default();
        answers.0.insert(
            Answers::key(target),
            serde_json::to_value(value).unwrap_or_default(),
        );
        self.provide(answers);
    }

    /// A private scratch directory for files the scene shows (a folder to
    /// browse, a repository to diff), removed after the capture.
    pub fn directory(&mut self) -> std::io::Result<&std::path::Path> {
        if self.directory.is_none() {
            self.directory = Some(
                tempfile::Builder::new()
                    .prefix("tilo-scenario-")
                    .tempdir()?,
            );
        }
        match &self.directory {
            Some(directory) => Ok(directory.path()),
            None => Err(std::io::Error::other("no scenario directory")),
        }
    }

    /// The content store the capture's views read from
    /// ([`ResourceEnv::content`](super::ResourceEnv::content)), for the blobs a scene
    /// shows: photos, attachments, thumbnails. A private one when the host
    /// lent none.
    pub fn content(&mut self) -> &tilo_content::ContentStore {
        self.content
            .get_or_insert_with(tilo_content::ContentStore::memory)
    }

    /// What the record holds when the capture starts: `state`, written into
    /// it directly as the record's first content, before the versions. For
    /// a scene whose starting content no single action makes as it stands
    /// (items under fixed ids the scenario names, written by several
    /// people) in fields the record keeps to its own actions
    /// (`#[model(readonly)]`), which refuse every generic edit, a
    /// scenario's or a version's alike. It is nobody's change: committed as
    /// the record's genesis and checked by no API, so only ever a
    /// capture's.
    pub fn start(&mut self, state: R::State)
    where
        R::State: tilo_resource::Fields,
    {
        self.start = Some(Box::new(move |doc| {
            let root = ui_core::automerge::ROOT;
            let before = <R::State as tilo_resource::Fields>::read_fields(doc, &root)
                .map_err(|error| format!("read the record: {error}"))?;
            tilo_resource::Fields::write_changed_fields(
                &state,
                &before,
                doc,
                &root,
                &tilo_resource::Delta::default(),
            )
            .map_err(|error| format!("write the record's start: {error}"))?;
            doc.commit_with(
                ui_core::automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
            );
            Ok(())
        }));
    }

    /// A version of the resource `author` wrote at `at_ms` (milliseconds
    /// since the Unix epoch), before the capture: it arrives as their
    /// change does over sync, attributed to them in history and on no one's
    /// undo stack here. Versions are written in the order given, before the
    /// scenario's own actions.
    pub fn version(&mut self, author: tilo_resource::MemberId, at_ms: i64, invoke: Invoke<R>) {
        self.history.push(Version {
            author,
            at_ms,
            invoke,
        });
    }

    /// A member named `name`, shown where they are in the resource
    /// (a selected item, a caret) the way their presence arrives in a
    /// shared window.
    pub fn member(&mut self, name: &str, at: ResourceSelection) {
        self.members.push(Shown::Member(name.to_owned(), at));
    }

    /// The agent `member` (shown as `name`) whose own view of the window stands
    /// at `place`, where its action left it.
    pub fn agent(
        &mut self,
        member: &tilo_resource::MemberId,
        name: &str,
        place: tilo_resource::Place,
    ) {
        let id = String::from_utf8_lossy(&member.actor_bytes()).into_owned();
        let presence = match tilo_actions::PresenceId::new(id) {
            Ok(presence) => presence,
            Err(error) => {
                tracing::warn!(%error, "a capture's agent names no presence; it is not shown");
                return;
            }
        };
        self.members.push(Shown::Agent(super::Landing {
            presence,
            member: Some(member.clone()),
            name: Some(name.to_owned()),
            place,
        }));
    }

    /// Write the record's start and the other people's versions into `doc`.
    pub(crate) fn write_history(
        &self,
        doc: &mut Doc,
        registry: &tilo_actions::ActionRegistry,
    ) -> Result<(), String> {
        if let Some(start) = &self.start {
            start(doc)?;
        }
        for version in &self.history {
            write_version::<R>(doc, registry, version)?;
        }
        Ok(())
    }

    pub(crate) fn has_history(&self) -> bool {
        self.start.is_some() || !self.history.is_empty()
    }

    /// What the capture's views read.
    pub(crate) fn into_run(self) -> Run {
        capture_run(Fixtures {
            values: self.values,
            members: self.members,
            content: self.content,
            _directory: self.directory,
        })
    }

    /// The content store the capture lends, once the fixture is prepared.
    pub(crate) fn lent_content(&self) -> Option<&tilo_content::ContentStore> {
        self.content.as_ref()
    }
}

/// The run of a capture whose views read `fixtures`: made here once rather
/// than in every app's [`Fixture::into_run`].
#[inline(never)]
fn capture_run(fixtures: Fixtures) -> Run {
    Run {
        mode: RunMode::Capture,
        fixtures: Rc::new(fixtures),
        launched: Rc::default(),
        private: false,
        embedded: false,
        folder_history: None,
        people: super::People::new(None),
        opener: None,
        driven: false,
        workspace: None,
    }
}

/// Apply one version as its author's change at its time: the envelope a
/// member's device would have sent, replayed as sync replays it.
fn write_version<R: Resource>(
    doc: &mut Doc,
    registry: &tilo_actions::ActionRegistry,
    version: &Version<R>,
) -> Result<(), String> {
    let action = version.invoke.build(version.at_ms)?;
    let encoded = tilo_resource::encode::<R>(&action).map_err(|error| error.to_string())?;
    replay_version(
        doc,
        registry,
        (&action, encoded),
        tilo_actions::ActionKey::new("", R::KIND, version.invoke.id),
        R::LABEL,
        (&version.author, version.at_ms),
    )
}

/// [`write_version`] once its action is built and encoded: `action`, the
/// action `key` names on a resource labelled `label`, written by `author` at
/// `at_ms`. Not generic, so it is compiled once rather than for every
/// resource.
#[inline(never)]
fn replay_version(
    doc: &mut Doc,
    registry: &tilo_actions::ActionRegistry,
    (action, encoded): (&dyn tilo_actions::Action, tilo_actions::EncodedAction),
    key: tilo_actions::ActionKey,
    label: &str,
    (author, at_ms): (&tilo_resource::MemberId, i64),
) -> Result<(), String> {
    let kind = key.kind.clone();
    let mut context = tilo_actions::ActionContext::for_resource(&kind, &kind, label)
        .with_change(action.change_kind())
        .with_host_actor(&tilo_actions::Acting::member(author.clone()), None);
    if let Some(described) = action
        .describe(doc)
        .or_else(|| tilo_actions::Described::of_published(&encoded, key))
    {
        context = context.with_described(described);
    }
    let envelope = tilo_actions::ActionEnvelope {
        id: tilo_actions::ActionId::new(),
        action: encoded,
        base_heads: doc.get_heads().iter().map(|head| head.0).collect(),
        timestamp: at_ms.div_euclid(1000),
        locus: action.locus(),
        actor: Some(author.actor_bytes()),
        context: Some(context),
    };
    tilo_actions::replay(doc, &envelope, registry).map_err(|error| error.to_string())
}
