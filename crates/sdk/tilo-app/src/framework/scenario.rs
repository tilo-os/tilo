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
//! resource written by other people at other times, and collaborators shown
//! where they are in it. Views read the values with
//! [`RecordEnv::fixture`](super::RecordEnv::fixture), surfaces with
//! [`RecordEnv::fixture`](super::RecordEnv::fixture).

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_resource::{Doc, Invoke, Resource};
use ui_core::history_mode::HistoryDefinition;
use ui_core::history_mode::checked_files::FileHistorySource;

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
    collaborators: Vec<(String, ResourceSelection)>,
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
    /// The file whose history History shows beside the resource's, for an
    /// app that declares [`ResourceApp::FILE_HISTORY`](super::ResourceApp::FILE_HISTORY),
    /// and the History scope the root witnesses it under.
    pub(crate) file_history: Option<(FileHistorySource, &'static HistoryDefinition)>,
    /// The names of the participants, as the host's roster has them.
    pub(crate) people: super::People,
    /// Opens another record in this window, in a window someone uses.
    pub(crate) opener: Option<super::launch::OpenHere>,
    /// A capture a headless drive invokes actions on
    /// ([`Capture::driven`](ui_core::application::Capture::driven)): the
    /// effects it is asked for run, though it starts no work on its own.
    pub(crate) driven: bool,
}

/// The arguments, the app's own options, the file and the playback handoff
/// a launch handed the
/// app.
#[derive(Default)]
pub(crate) struct Launched {
    /// The record selected by the host for this launch.
    pub(crate) record: Option<ui_core::record::RecordId>,
    pub(crate) arguments: Vec<String>,
    pub(crate) options: Vec<ui_core::application::LaunchOption>,
    pub(crate) file: Option<ui_core::application::OpenFile>,
    pub(crate) handoff: Option<ui_core::application::PlaybackHandoff>,
    /// The services the host that mounts the app in-process lent it.
    pub(crate) lent: Lent,
}

/// What an in-process host lent the app
/// ([`HostContext::lend`](ui_core::application::HostContext::lend)), each
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
            file_history: None,
            people: super::People::new(None),
            opener: None,
            driven: false,
        }
    }

    /// This run in a private window, when `private`.
    pub(crate) fn in_private(mut self, private: bool) -> Self {
        self.private = private;
        self
    }

    /// This run, showing the history of the file `file_history` names.
    pub(crate) fn showing_file_history(
        mut self,
        file_history: Option<(FileHistorySource, &'static HistoryDefinition)>,
    ) -> Self {
        self.file_history = file_history;
        self
    }

    pub(crate) fn past() -> Self {
        Self {
            mode: RunMode::Past,
            fixtures: Rc::default(),
            launched: Rc::default(),
            private: false,
            file_history: None,
            people: super::People::new(None),
            opener: None,
            driven: false,
        }
    }

    /// The same run, a capture a headless drive invokes actions on when
    /// `driven`.
    pub(crate) fn driven_by_drive(mut self, driven: bool) -> Self {
        self.driven = driven;
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

    pub(crate) fn for_participant(mut self, me: Option<&str>) -> Self {
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

    /// The collaborators a capture shows, where they are in the resource.
    pub(crate) fn collaborators(&self) -> &[(String, ResourceSelection)] {
        &self.fixtures.collaborators
    }
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
    author: String,
    at_ms: i64,
    invoke: Invoke<R>,
}

/// The scene of one screenshot scenario that its actions cannot stage:
/// handed to [`ResourceApp::fixture`](super::ResourceApp::fixture) before
/// the view is built.
pub struct Fixture<R: Resource> {
    scenario: Option<String>,
    values: HashMap<TypeId, Rc<dyn Any>>,
    /// What the record starts with, written before the versions.
    start: Option<Start>,
    history: Vec<Version<R>>,
    collaborators: Vec<(String, ResourceSelection)>,
    content: Option<tilo_content::ContentStore>,
    directory: Option<tempfile::TempDir>,
}

/// Writes a capture's starting state into the record.
type Start = Box<dyn Fn(&mut Doc) -> Result<(), String>>;

impl<R: Resource> Fixture<R> {
    pub(crate) fn new(scenario: Option<&str>, content: Option<tilo_content::ContentStore>) -> Self {
        Self {
            scenario: scenario.map(str::to_owned),
            values: HashMap::new(),
            start: None,
            history: Vec::new(),
            collaborators: Vec::new(),
            content,
            directory: None,
        }
    }

    /// The scenario being captured; `None` for a plain `--shots`.
    pub fn scenario(&self) -> Option<&str> {
        self.scenario.as_deref()
    }

    /// Stand `value` in for a live source: the views and surfaces of this
    /// capture read it with `cx.fixture::<T>()`. One value per type; a
    /// second replaces the first.
    pub fn provide<T: 'static>(&mut self, value: T) {
        self.values.insert(TypeId::of::<T>(), Rc::new(value));
    }

    /// Stand `value` in for what another provider's action at `target`
    /// answers: a view's [`RecordEnv::query`](super::RecordEnv::query) of
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
    /// ([`RecordEnv::content`](super::RecordEnv::content)), for the blobs a scene
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
    /// the record's genesis and checked by no door, so only ever a
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
    pub fn version(&mut self, author: &str, at_ms: i64, invoke: Invoke<R>) {
        self.history.push(Version {
            author: author.to_owned(),
            at_ms,
            invoke,
        });
    }

    /// A collaborator named `name`, shown where they are in the resource
    /// (a selected item, a caret) the way their presence arrives in a
    /// shared window.
    pub fn collaborator(&mut self, name: &str, at: ResourceSelection) {
        self.collaborators.push((name.to_owned(), at));
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
        Run {
            mode: RunMode::Capture,
            fixtures: Rc::new(Fixtures {
                values: self.values,
                collaborators: self.collaborators,
                content: self.content,
                _directory: self.directory,
            }),
            launched: Rc::default(),
            private: false,
            file_history: None,
            people: super::People::new(None),
            opener: None,
            driven: false,
        }
    }

    /// The content store the capture lends, once the fixture is prepared.
    pub(crate) fn lent_content(&self) -> Option<&tilo_content::ContentStore> {
        self.content.as_ref()
    }
}

/// Apply one version as its author's change at its time: the envelope a
/// collaborator's device would have sent, replayed as sync replays it.
fn write_version<R: Resource>(
    doc: &mut Doc,
    registry: &tilo_actions::ActionRegistry,
    version: &Version<R>,
) -> Result<(), String> {
    use tilo_actions::Action as _;
    let action = version.invoke.build(version.at_ms)?;
    let encoded = tilo_resource::encode::<R>(&action).map_err(|error| error.to_string())?;
    let mut context = tilo_actions::ActionContext::for_resource(R::KIND, R::KIND, R::LABEL)
        .with_change(action.change_kind())
        .with_host_invoker(version.author.clone(), None);
    if let Some(described) = action.describe(doc).or_else(|| {
        tilo_actions::Described::of_published(
            &encoded,
            tilo_actions::ActionKey::new("", R::KIND, version.invoke.id),
        )
    }) {
        context = context.with_described(described);
    }
    let envelope = tilo_actions::ActionEnvelope {
        id: tilo_actions::ActionId::new(),
        action: encoded,
        base_heads: doc.get_heads().iter().map(|head| head.0).collect(),
        timestamp: version.at_ms.div_euclid(1000),
        locus: action.locus(),
        actor: Some(version.author.as_bytes().to_vec()),
        context: Some(context),
    };
    tilo_actions::replay(doc, &envelope, registry).map_err(|error| error.to_string())
}
