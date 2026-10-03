//! Root of the Backups app.
//!
//! [`BackupsApp`] is a [`NavShell`] window: the places (every backup, each
//! source, each destination, the retention policy) are the sidebar, the
//! selected place is the content, and the selected snapshot is the inspector
//! (see [`crate::view`]). This module owns what the shell cannot: the
//! catalog, the local repository and its background jobs, the restore sheet,
//! the transient result toast and the health bubble.
//!
//! State split:
//!   * the catalog (snapshot listing), navigation, selection, the restore
//!     sheet and device/backend observations are local to this widget;
//!   * the durable retention policy lives in the Automerge document (see
//!     [`BackupsDoc`]) and is changed only through a typed action.

use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};

use automerge::{AutoCommit, ObjId};
use chrono::NaiveDateTime;
use ui::LocalState;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{AppModel, Column, ModalDialog, ModelPath, NavShell, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::actions::{self, BACKUPS_ACTION_TYPE};
use crate::doc::{BackupsDoc, PolicyField};
use crate::i18n;
use crate::model::{Catalog, Filter, RetentionPolicy};
use crate::view::{self, Intent, Intents, JobKind, Order, Page, Setup};

/// How long a result toast stays before it hides itself.
const TOAST_TTL: Duration = Duration::from_millis(3_500);

#[derive(Clone, Copy)]
enum PickPurpose {
    Source,
    Destination,
    Restore,
}

enum JobEvent {
    Progress(crate::engine::Progress),
    Created(crate::engine::BackupManifest),
    Verified(crate::engine::Verification),
    Restored(crate::engine::Verification),
    Failed(String),
}

struct Toast {
    text: String,
    kind: ToastKind,
    shown: Instant,
}

/// Which screen the window shows instead of the catalog, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SetupKey {
    Start,
    ChooseDestination,
    Running(String),
    Unavailable(String),
}

/// What the shell currently shows, so it is rebuilt only when it changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ViewKey {
    catalog: u64,
    setup: Option<SetupKey>,
    page: Page,
    selected: Option<String>,
    order: Order,
    job: Option<JobKind>,
    can_act: bool,
    failure: Option<(String, String)>,
}

pub struct BackupsApp {
    shell: NavShell,
    intents: Intents,
    catalog: Catalog,
    /// Bumped whenever `catalog` is replaced.
    catalog_rev: u64,
    page: Page,
    selected: Option<String>,
    policy: RetentionPolicy,
    /// One per [`PolicyField::ALL`] entry: what the retention steppers show.
    retention: [LocalState<f64>; 4],
    order: Order,
    restore: Option<ModalDialog<Column>>,
    toast: Option<Toast>,
    /// The last operation that failed and needs the person: what, and what
    /// to do about it. Shown as a banner above the content until dismissed.
    failure: Option<(String, String)>,
    /// The health of every backup, surfaced when it changes.
    status_bubble: ui::StatusBubble,
    /// The pointer, once it has moved over the window.
    cursor: Option<Point>,
    /// The content's scroll offset, kept across rebuilds.
    scroll: Rc<Cell<(f32, f32)>>,
    built: Option<ViewKey>,
    dirty: bool,
    /// A capture staged the sample catalog: controls show as they would on a
    /// configured device.
    demo: bool,
    /// A capture asked for the sidebar open as a drawer: done once the shell
    /// has laid out narrow.
    open_drawer: bool,
    /// Framework-owned action dispatch: every retention-policy edit goes through
    /// here so it brackets one undo boundary, encodes under [`BACKUPS_ACTION_TYPE`],
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: crate::resource::ActionSink,
    /// Retained complete recovery-point index. The visible resource snapshot
    /// stays bounded, while Ctrl+K searches every already-loaded version.
    search_index: crate::search_provider::BackupsSearchIndex,
    source_path: Option<PathBuf>,
    destination_path: Option<PathBuf>,
    /// A persisted local configuration can outlive an unplugged drive or a
    /// damaged repository. Keep that distinct from first-run so actions never
    /// imply the repository is ready when it was not successfully loaded.
    repository_error: Option<String>,
    picker: Option<(PickPurpose, ui::portal::PortalChannel)>,
    job: Option<Receiver<JobEvent>>,
    /// The running job: what it does, and its progress line.
    job_status: Option<view::Job>,
}

impl Default for BackupsApp {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> NaiveDateTime {
    ui::clock::local_now().naive_local()
}

impl BackupsApp {
    pub(crate) fn resource_catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub(crate) fn is_configured(&self) -> bool {
        self.source_path.is_some()
            && self.destination_path.is_some()
            && self.repository_error.is_none()
    }

    pub(crate) fn unavailable_reason(&self) -> Option<&str> {
        self.repository_error.as_deref()
    }

    /// The live app: starts on an empty catalog (real first-run state) and
    /// shows setup until the person configures a source + destination.
    pub fn new() -> Self {
        let mut app = Self::with_catalog(Catalog::empty());
        if let Some((source, destination)) = Self::read_config() {
            app.source_path = Some(source.clone());
            app.destination_path = Some(destination.clone());
            // `list_snapshots` correctly treats a brand-new repository as an
            // empty one. A *saved* destination is different: never create a
            // missing mount path on startup, or an unplugged drive could look
            // like a fresh local repository and receive a backup in the wrong
            // place later.
            if destination.is_dir() {
                match crate::engine::list_snapshots(&destination) {
                    Ok(manifests) => {
                        app.set_catalog(Catalog::from_local(&source, &destination, &manifests));
                    }
                    Err(error) => {
                        app.repository_error =
                            Some(format!("Couldn’t read the saved backup location: {error}."));
                    }
                }
            } else {
                app.repository_error = Some(
                    "The saved backup location is not mounted or is no longer a folder.".into(),
                );
            }
        }
        app
    }

    fn with_catalog(catalog: Catalog) -> Self {
        let search_index = crate::search_provider::BackupsSearchIndex::new(&catalog);
        let policy = RetentionPolicy::default();
        Self {
            shell: NavShell::new()
                .content_header("", Vec::new())
                .inspector_header("Snapshot", Vec::new())
                .with_id("backups-shell"),
            intents: Intents::default(),
            catalog,
            catalog_rev: 0,
            page: Page::All,
            selected: None,
            policy,
            retention: [
                LocalState::new(f64::from(policy.hourly)),
                LocalState::new(f64::from(policy.daily)),
                LocalState::new(f64::from(policy.weekly)),
                LocalState::new(f64::from(policy.monthly)),
            ],
            order: Order::NewestFirst,
            restore: None,
            toast: None,
            failure: None,
            status_bubble: ui::StatusBubble::new(),
            cursor: None,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            built: None,
            dirty: true,
            demo: false,
            open_drawer: false,
            actions: crate::resource::ActionSink::default(),
            search_index,
            source_path: None,
            destination_path: None,
            repository_error: None,
            picker: None,
            job: None,
            job_status: None,
        }
    }

    fn set_catalog(&mut self, catalog: Catalog) {
        self.catalog = catalog;
        self.catalog_rev += 1;
        self.search_index.sync(&self.catalog);
        self.dirty = true;
    }

    /// Stage a capture: the sample catalog laid out from the (pinned) clock,
    /// then the scenario's view state.
    pub fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("setup") {
            return;
        }
        if scenario == Some("unavailable") {
            self.source_path = Some(PathBuf::from("/home/ana/Documents"));
            self.destination_path = Some(PathBuf::from("/media/ana/Seagate"));
            self.repository_error =
                Some("The backup drive “Seagate 2 TB” is not connected.".into());
            self.dirty = true;
            return;
        }
        self.demo = true;
        self.set_catalog(Catalog::sample(now()));
        match scenario {
            Some("progress") => {
                self.job_status = Some(view::Job::new(
                    JobKind::Backup,
                    i18n::counted("backups-progress-files", 642, &view::size(3_800_000_000)),
                ));
            }
            Some("failure") => {
                self.page = Page::Source("workspace".into());
                self.failure = Some((
                    "Offsite copy failed".into(),
                    "Backblaze B2 didn’t respond. Check the connection, then back up again.".into(),
                ));
            }
            Some("restore") => {
                self.selected = Some("m-0-0824".into());
                self.open_restore();
            }
            Some("status") => {
                let now = ui::time::now();
                self.status_bubble.show(&self.health_line(), now);
                self.status_bubble.set_hover(true, now);
            }
            Some("sources") => self.page = Page::Source("home".into()),
            Some("destinations") => self.page = Page::Destination("pool".into()),
            Some("drive") => self.page = Page::Destination("usb".into()),
            Some("offsite") => self.page = Page::Destination("b2".into()),
            Some("schedules") => self.page = Page::Retention,
            Some("drawer") => self.open_drawer = true,
            _ => {}
        }
        self.dirty = true;
    }

    /// Adjust one retention field by `delta`, clamped to `[0, 999]`, through the
    /// action host: the same [`BackupsAction`](crate::actions::BackupsAction)
    /// the Ctrl+K increment/decrement spec exports.
    #[cfg(test)]
    pub(crate) fn bump_retention(
        &mut self,
        doc: &mut AutoCommit,
        root: &ObjId,
        field: PolicyField,
        delta: i64,
    ) {
        let cur = BackupsDoc::retention(doc, root, field);
        self.dispatch_policy(doc, &actions::bump_retention_action(field, cur, delta));
    }

    fn dispatch_policy(&mut self, doc: &mut AutoCommit, action: &crate::actions::BackupsAction) {
        self.actions.dispatch_with_context(
            doc,
            BACKUPS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.backups", "Backups", action),
        );
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// test.
    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Show the retention policy the framework resource already holds.
    /// An empty/default projection must not wipe unpublished local policy.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::BackupsState) {
        let untouched = state.policy == RetentionPolicy::default();
        let local_unpublished = self.policy != RetentionPolicy::default();
        if untouched && local_unpublished {
            return;
        }
        self.set_policy(state.policy);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    #[cfg(test)]
    pub(crate) fn ui_policy(&self) -> &RetentionPolicy {
        &self.policy
    }

    #[cfg(test)]
    pub(crate) fn ui_policy_mut(&mut self) -> &mut RetentionPolicy {
        &mut self.policy
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Backups>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Take the stored policy, and show it in the steppers.
    fn set_policy(&mut self, policy: RetentionPolicy) {
        self.policy = policy;
        for (state, field) in self.retention.iter().zip(PolicyField::ALL) {
            let stored = f64::from(policy_value(&policy, field));
            if state.with(|shown| (*shown - stored).abs() > f64::EPSILON) {
                state.set(stored);
            }
        }
    }

    /// Steppers the person moved dispatch their class's absolute count.
    fn commit_retention(&mut self, doc: &mut AutoCommit, root: &ObjId) {
        for (i, field) in PolicyField::ALL.iter().enumerate() {
            let Some(state) = self.retention.get(i) else {
                continue;
            };
            let shown = state.with(|v| v.round().clamp(0.0, 999.0)) as u32;
            if shown != policy_value(&self.policy, *field) {
                self.dispatch_policy(doc, &actions::set_retention_action(*field, shown));
                self.policy = BackupsDoc::policy(doc, root);
                self.dirty = true;
            }
        }
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            kind: ToastKind::Success,
            shown: ui::time::now(),
        });
        self.dirty = true;
    }

    /// An operation failed: say what and why where the person works, until
    /// they dismiss it or the next run starts.
    fn fail(&mut self, title: &str, message: impl Into<String>) {
        self.failure = Some((title.to_owned(), message.into()));
        self.dirty = true;
    }

    fn config_path() -> Option<PathBuf> {
        Some(tilo_paths::xdg::state_home()?.join("tilo/backups/config.json"))
    }

    fn read_config() -> Option<(PathBuf, PathBuf)> {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(Self::config_path()?).ok()?).ok()?;
        Some((
            PathBuf::from(value.get("source")?.as_str()?),
            PathBuf::from(value.get("destination")?.as_str()?),
        ))
    }

    fn save_config(&self) {
        let (Some(source), Some(destination), Some(path)) = (
            &self.source_path,
            &self.destination_path,
            Self::config_path(),
        ) else {
            return;
        };
        let value = serde_json::json!({ "source": source, "destination": destination });
        if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
            let _ = tilo_files::native::atomic::write_creating_parent(&path, &bytes);
        }
    }

    fn pick_folder(&mut self, purpose: PickPurpose) {
        let (title, accept) = match purpose {
            PickPurpose::Source => ("Choose a folder to back up", "Back up this folder"),
            PickPurpose::Destination => ("Choose where to keep backups", "Use this location"),
            PickPurpose::Restore => ("Restore into an empty folder", "Restore here"),
        };
        self.picker = Some((
            purpose,
            ui::portal::PickRequest::directory(title, accept)
                .thread_name("backups-folder-picker")
                .send(),
        ));
    }

    /// Refuse an operation on an unreadable repository, saying why.
    fn repository_ready(&mut self) -> bool {
        if let Some(error) = self.repository_error.clone() {
            self.fail(
                "Backup location unavailable",
                format!("{error} Choose another location to continue."),
            );
            return false;
        }
        true
    }

    fn start_job(
        &mut self,
        kind: JobKind,
        status: &str,
        name: &str,
        work: impl FnOnce(std::sync::mpsc::Sender<JobEvent>) + Send + 'static,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.job = Some(rx);
        self.job_status = Some(view::Job::new(kind, status));
        self.failure = None;
        self.dirty = true;
        if let Err(error) = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || work(tx))
        {
            self.job = None;
            self.job_status = None;
            self.fail("Couldn’t start", error.to_string());
        }
    }

    fn start_backup(&mut self) {
        if !self.repository_ready() {
            return;
        }
        let (Some(source), Some(destination)) =
            (self.source_path.clone(), self.destination_path.clone())
        else {
            return;
        };
        self.start_job(
            JobKind::Backup,
            "Preparing files…",
            "backups-create",
            move |tx| {
                let progress_tx = tx.clone();
                let event =
                    match crate::engine::create_snapshot(&source, &destination, |progress| {
                        let _ = progress_tx.send(JobEvent::Progress(progress.clone()));
                    }) {
                        Ok(manifest) => JobEvent::Created(manifest),
                        Err(error) => JobEvent::Failed(error.to_string()),
                    };
                let _ = tx.send(event);
            },
        );
    }

    fn start_verify(&mut self) {
        if !self.repository_ready() {
            return;
        }
        let (Some(destination), Some(id)) = (self.destination_path.clone(), self.selected.clone())
        else {
            return;
        };
        self.start_job(
            JobKind::Verify,
            "Reading every file…",
            "backups-verify",
            move |tx| {
                let event = match crate::engine::verify_snapshot(&destination, &id) {
                    Ok(result) => JobEvent::Verified(result),
                    Err(error) => JobEvent::Failed(error.to_string()),
                };
                let _ = tx.send(event);
            },
        );
    }

    fn start_restore(&mut self, restore_to: PathBuf) {
        if !self.repository_ready() {
            return;
        }
        let (Some(destination), Some(id)) = (self.destination_path.clone(), self.selected.clone())
        else {
            return;
        };
        self.start_job(
            JobKind::Restore,
            "Restoring and verifying files…",
            "backups-restore",
            move |tx| {
                let progress_tx = tx.clone();
                let event = match crate::engine::restore_snapshot(
                    &destination,
                    &id,
                    &restore_to,
                    false,
                    |progress| {
                        let _ = progress_tx.send(JobEvent::Progress(progress.clone()));
                    },
                ) {
                    Ok(result) => JobEvent::Restored(result),
                    Err(error) => JobEvent::Failed(error.to_string()),
                };
                let _ = tx.send(event);
            },
        );
    }

    fn drain_effects(&mut self) {
        self.drain_picker();
        self.drain_job();
    }

    fn drain_picker(&mut self) {
        let portal_result =
            self.picker
                .as_ref()
                .and_then(|(purpose, receiver)| match receiver.try_recv() {
                    Ok(outcome) => Some((*purpose, outcome)),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some((
                        *purpose,
                        ui::portal::PortalOutcome::Failed(
                            "The folder chooser closed unexpectedly.".into(),
                        ),
                    )),
                });
        let Some((purpose, outcome)) = portal_result else {
            return;
        };
        self.picker = None;
        self.dirty = true;
        match outcome {
            ui::portal::PortalOutcome::Opened(path) => match purpose {
                PickPurpose::Source => {
                    self.source_path = Some(path);
                    self.pick_folder(PickPurpose::Destination);
                }
                PickPurpose::Destination => {
                    if let Some(source) = &self.source_path {
                        match crate::engine::validate_paths(source, &path) {
                            Ok(_) => {
                                self.destination_path = Some(path);
                                self.repository_error = None;
                                self.save_config();
                                self.start_backup();
                            }
                            Err(error) => self.fail("Choose another location", error.to_string()),
                        }
                    }
                }
                PickPurpose::Restore => {
                    // The sheet promises a destination chooser, not a
                    // background restore. Dismiss it only after the person
                    // explicitly chose a folder; `start_restore` still calls
                    // the engine with `overwrite = false`.
                    self.restore = None;
                    self.start_restore(path);
                }
            },
            ui::portal::PortalOutcome::Cancelled => {}
            ui::portal::PortalOutcome::Failed(error) => {
                self.fail("Couldn’t open the folder chooser", error);
            }
        }
    }

    fn drain_job(&mut self) {
        loop {
            let event = self
                .job
                .as_ref()
                .and_then(|receiver| match receiver.try_recv() {
                    Ok(event) => Some(event),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(JobEvent::Failed(
                        "The backup worker stopped unexpectedly.".into(),
                    )),
                });
            let Some(event) = event else {
                break;
            };
            if let JobEvent::Progress(progress) = event {
                if let Some(job) = &self.job_status {
                    *job.line.borrow_mut() = i18n::counted(
                        "backups-progress-files",
                        progress.files,
                        &view::size(progress.bytes),
                    );
                }
                continue;
            }
            self.job = None;
            self.job_status = None;
            self.dirty = true;
            match event {
                JobEvent::Created(manifest) => {
                    self.reload_repository();
                    self.toast(i18n::counted(
                        "backups-backed-up-files",
                        manifest.files,
                        &view::size(manifest.bytes),
                    ));
                }
                JobEvent::Verified(result) => self.toast(i18n::counted(
                    "backups-verified-files",
                    result.files,
                    &view::size(result.bytes),
                )),
                JobEvent::Restored(result) => self.toast(i18n::counted(
                    "backups-restored-files",
                    result.files,
                    &view::size(result.bytes),
                )),
                JobEvent::Failed(error) => self.fail("Backup failed", error),
                JobEvent::Progress(_) => {}
            }
            break;
        }
    }

    fn reload_repository(&mut self) {
        let (Some(source), Some(destination)) =
            (self.source_path.clone(), self.destination_path.clone())
        else {
            return;
        };
        match crate::engine::list_snapshots(&destination) {
            Ok(manifests) => {
                self.set_catalog(Catalog::from_local(&source, &destination, &manifests));
            }
            Err(error) => {
                self.repository_error = Some(format!(
                    "The backup finished, but its history couldn’t be reopened: {error}."
                ));
            }
        }
    }

    // ---- the restore sheet ------------------------------------------------

    fn open_restore(&mut self) {
        let Some(snapshot) = self
            .selected
            .as_deref()
            .and_then(|id| self.catalog.snapshot(id))
        else {
            return;
        };
        self.restore = Some(view::restore_dialog(
            snapshot,
            &self.catalog,
            now(),
            &self.intents,
        ));
        self.dirty = true;
    }

    // ---- intents ------------------------------------------------------------

    fn drain_intents(&mut self, doc: &mut AutoCommit, root: &ObjId) {
        for intent in self.intents.take() {
            self.apply(intent);
        }
        self.commit_retention(doc, root);
    }

    fn apply(&mut self, intent: Intent) {
        self.dirty = true;
        match intent {
            Intent::Navigate(page) => {
                if page != self.page {
                    self.page = page;
                    self.selected = None;
                    self.scroll.set((0.0, 0.0));
                }
            }
            Intent::SelectSnapshot(id) => {
                self.selected = Some(id);
                if !self.shell.is_inspector_open() {
                    self.shell.toggle_inspector();
                }
            }
            Intent::BackUpNow => self.start_backup(),
            Intent::Verify => self.start_verify(),
            Intent::OpenRestore => self.open_restore(),
            Intent::ChooseRestoreFolder => self.pick_folder(PickPurpose::Restore),
            Intent::CloseRestore => self.restore = None,
            Intent::Setup => {
                if self.source_path.is_some() && self.repository_error.is_none() {
                    self.pick_folder(PickPurpose::Destination);
                } else {
                    self.repository_error = None;
                    self.source_path = None;
                    self.destination_path = None;
                    self.pick_folder(PickPurpose::Source);
                }
            }
            Intent::DismissFailure => self.failure = None,
            Intent::SetOrder(order) => self.order = order,
        }
    }

    /// The newest snapshot of the page, unless one is chosen there already.
    fn keep_selection(&mut self) {
        if !self.page.lists_snapshots() {
            return;
        }
        let filter = self.page.filter();
        let still_listed = self
            .selected
            .as_deref()
            .and_then(|id| self.catalog.snapshot(id))
            .is_some_and(|snapshot| filter.admits(snapshot));
        if !still_listed {
            self.selected = self
                .catalog
                .timeline(filter)
                .first()
                .map(|snapshot| snapshot.id.clone());
        }
    }

    // ---- the shell's content ----------------------------------------------

    fn setup_key(&self) -> Option<SetupKey> {
        if let Some(error) = &self.repository_error {
            return Some(SetupKey::Unavailable(error.clone()));
        }
        if !self.catalog.sources.is_empty() || !self.catalog.destinations.is_empty() {
            return None;
        }
        if let Some(job) = &self.job_status {
            return Some(SetupKey::Running(job.line.borrow().clone()));
        }
        Some(if self.source_path.is_some() {
            SetupKey::ChooseDestination
        } else {
            SetupKey::Start
        })
    }

    fn can_act(&self) -> bool {
        self.demo || self.is_configured()
    }

    fn view_key(&self) -> ViewKey {
        ViewKey {
            catalog: self.catalog_rev,
            setup: self.setup_key(),
            page: self.page.clone(),
            selected: self.selected.clone(),
            order: self.order,
            job: self.job_status.as_ref().map(|job| job.kind),
            can_act: self.can_act(),
            failure: self.failure.clone(),
        }
    }

    fn set_content_header(&mut self, title: String, actions: Vec<ui::widget::Element>) {
        let shell = std::mem::take(&mut self.shell);
        self.shell = shell.content_header(title, actions);
    }

    /// Rebuild the columns when what they show changed.
    fn refresh_view(&mut self) {
        self.keep_selection();
        let key = self.view_key();
        if self.built.as_ref() == Some(&key) {
            return;
        }
        let sidebar_stale = self.built.as_ref().is_none_or(|built| {
            built.catalog != key.catalog || built.page != key.page || built.setup != key.setup
        });
        self.built = Some(key.clone());
        self.dirty = true;
        if let Some(setup) = &key.setup {
            let state = match setup {
                SetupKey::Start => Setup::Start,
                SetupKey::ChooseDestination => Setup::ChooseDestination,
                SetupKey::Running(line) => Setup::Running(line),
                SetupKey::Unavailable(error) => Setup::Unavailable(error),
            };
            self.shell.set_sidebar(None);
            self.shell.set_inspector(None);
            self.set_content_header(String::new(), Vec::new());
            self.shell.set_content(view::setup(&state, &self.intents));
            return;
        }
        if sidebar_stale {
            self.shell.set_sidebar(Some(Box::new(view::sidebar(
                &self.catalog,
                &self.page,
                &self.intents,
            ))));
        }
        let actions = view::header_actions(key.job, key.can_act, &self.intents);
        self.set_content_header(view::page_title(&self.catalog, &self.page), actions);
        let now = now();
        let model = view::Model {
            catalog: &self.catalog,
            page: &self.page,
            selected: self.selected.as_deref(),
            now,
            order: self.order,
            job: self.job_status.as_ref(),
            failure: self
                .failure
                .as_ref()
                .map(|(title, message)| (title.as_str(), message.as_str())),
        };
        let content = view::content(&model, &self.intents, &self.scroll, &self.retention);
        self.shell.set_content(content);
        let details = self
            .selected
            .as_deref()
            .filter(|_| self.page.lists_snapshots())
            .and_then(|id| self.catalog.snapshot(id))
            .map(|snapshot| {
                view::snapshot_details(
                    snapshot,
                    &self.catalog,
                    now,
                    key.can_act && key.job.is_none(),
                    &self.intents,
                )
            });
        self.shell.set_inspector(details);
    }

    /// "All backups healthy", or how many need a look.
    fn health_line(&self) -> String {
        let failed = self.catalog.failed(Filter::All) as u64;
        if failed == 0 {
            i18n::t("backups-health-ok")
        } else {
            i18n::counted("backups-health-attention", failed, "")
        }
    }

    // ---- overlays -----------------------------------------------------------

    fn paint_toast(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        let Some(toast) = &self.toast else {
            return;
        };
        let age = toast.shown.elapsed();
        if age >= TOAST_TTL {
            self.toast = None;
            return;
        }
        paint_transient(
            scene,
            transform,
            text,
            theme,
            window,
            ToastAnchor::BottomCenter,
            &ToastCard::new(&toast.text, toast.kind),
        );
    }

    fn paint_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        if self.setup_key().is_some() {
            return;
        }
        let now = ui::time::now();
        let line = self.health_line();
        self.status_bubble.show(&line, now);
        // Resting the pointer on the bubble's corner holds it open.
        if let Some(cursor) = self.cursor {
            self.status_bubble
                .pointer_moved(window, cursor, text, theme, now);
        }
        self.status_bubble
            .paint(scene, transform, text, theme, window, now);
    }

    // ---- resource operations ----------------------------------------------

    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        if !matches!(
            kind,
            tilo_actions::OperationKind::Effect | tilo_actions::OperationKind::View
        ) {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        if self.job.is_some() {
            return Err("A backup operation is already running; wait for it to finish.".into());
        }
        let has_snapshot = |id: &str| self.catalog.snapshots.iter().any(|s| s.id == id);
        let has_source = |id: &str| self.catalog.sources.iter().any(|s| s.id == id);
        self.dirty = true;
        match operation {
            crate::actions::BackupOperation::Setup => self.pick_folder(PickPurpose::Source),
            crate::actions::BackupOperation::BackupNow => {
                if self.source_path.is_none() || self.destination_path.is_none() {
                    return Err("Backups are not configured on this device.".into());
                }
                self.start_backup();
            }
            crate::actions::BackupOperation::Verify { snapshot_id } => {
                if self.destination_path.is_none() || !has_snapshot(&snapshot_id) {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.selected = Some(snapshot_id);
                self.start_verify();
            }
            crate::actions::BackupOperation::Restore { snapshot_id } => {
                if self.destination_path.is_none() || !has_snapshot(&snapshot_id) {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.selected = Some(snapshot_id);
                self.pick_folder(PickPurpose::Restore);
            }
            crate::actions::BackupOperation::OpenSource { source_id } => {
                if !has_source(&source_id) {
                    return Err("That source is unavailable on this device.".into());
                }
                self.page = Page::Source(source_id);
                self.selected = None;
            }
            crate::actions::BackupOperation::OpenDestination { destination_id } => {
                if !self
                    .catalog
                    .destinations
                    .iter()
                    .any(|destination| destination.id == destination_id)
                {
                    return Err("That destination is unavailable on this device.".into());
                }
                self.page = Page::Destination(destination_id);
                self.selected = None;
            }
            crate::actions::BackupOperation::OpenSnapshot { snapshot_id } => {
                if !has_snapshot(&snapshot_id) {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.page = Page::All;
                self.selected = Some(snapshot_id);
            }
            crate::actions::BackupOperation::OpenSchedule { source_id } => {
                if !has_source(&source_id) {
                    return Err("That schedule's source is unavailable on this device.".into());
                }
                self.page = Page::Retention;
            }
        }
        Ok(true)
    }

    fn reconcile(&mut self, doc: &AutoCommit, root: &ObjId) {
        self.drain_effects();
        let stored = BackupsDoc::policy(doc, root);
        if stored != self.policy {
            self.set_policy(stored);
            self.dirty = true;
        }
    }
}

fn policy_value(policy: &RetentionPolicy, field: PolicyField) -> u32 {
    match field {
        PolicyField::Hourly => policy.hourly,
        PolicyField::Daily => policy.daily,
        PolicyField::Weekly => policy.weekly,
        PolicyField::Monthly => policy.monthly,
    }
}

impl ui::widget::Decorator for BackupsApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
            return;
        }
        self.shell.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
            return;
        }
        if request.wants::<dyn ui::undo::CommittedActions>() {
            request.provide::<dyn ui::undo::CommittedActions>(self);
            return;
        }
        self.shell.provide_mut(request);
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::BACKUPS_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(&mut self.shell, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, root) = ui::doc::document(state, path);
            self.reconcile(doc, root);
            self.drain_intents(doc, root);
        }
        self.refresh_view();
        if self.open_drawer && self.shell.geometry().sidebar == ui::widget::SidebarPlacement::Drawer
        {
            self.open_drawer = false;
            self.shell.open_sidebar();
            self.shell.snap();
            self.dirty = true;
        }
        self.shell.sync(state, path);
        if let Some(dialog) = &mut self.restore {
            dialog.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        {
            let (doc, root) = ui::doc::document(state, path);
            self.drain_intents(doc, root);
        }
        let shell = self.shell.update(state, path);
        let dialog = self
            .restore
            .as_mut()
            .is_some_and(|dialog| dialog.update(state, path));
        shell || dialog || self.dirty
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.shell.build_layout(taffy, theme, node_map);
        // The host reaches the undo bracket through the node map.
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if let Some(dialog) = &mut self.restore {
            let (doc, root) = ui::doc::document(ctx.state, ctx.path);
            dialog.handle_detached(event, ctx.scale_factor, *ctx.modifiers, doc, root);
            *ctx.handled = true;
        } else {
            self.shell.handle_event(event, ctx);
        }
        let (doc, root) = ui::doc::document(ctx.state, ctx.path);
        self.drain_intents(doc, root);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self.shell.children();
        if let Some(dialog) = &self.restore {
            children.push(dialog);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.shell.for_each_child(visitor);
        if let Some(dialog) = &self.restore {
            visitor(dialog);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self.shell.children_mut();
        if let Some(dialog) = &mut self.restore {
            children.push(dialog);
        }
        children
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text, theme, presence);
        let window = presence
            .viewport
            .get()
            .unwrap_or_else(|| self.shell.get_rect());
        self.paint_status(scene, transform, text, theme, window);
        self.paint_toast(scene, transform, text, theme, window);
        if let Some(dialog) = &mut self.restore {
            dialog.draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
            || self.picker.is_some()
            || self.job.is_some()
            || self.status_bubble.needs_frame(ui::time::now())
            || self.shell.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.toast.is_some() || self.restore.is_some() || self.shell.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shell.mark_drawn();
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.restore.as_ref().and_then(Widget::modal_capture)
    }

    fn traps_focus(&self) -> bool {
        self.restore.is_some()
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn name(&self) -> &'static str {
        "BackupsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("backups_app")
    }
}

impl ui::search::capability::SearchSource for BackupsApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}

impl ui::resources::PublishesResources for BackupsApp {
    /// Standalone keeps the full hand-built door. When hosted, policy
    /// Document ops ride [`tilo_resource::publish`]; this returns only live
    /// catalog extras (never a second `backups.policy` / workspace root).
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == crate::actions::BACKUPS_RESOURCE_ID {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::undo::CommittedActions for BackupsApp {
    /// Hand the host the heads captured before an on-screen control dispatched
    /// its action, so it records exactly one undo boundary bracketing it — the
    /// same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SnapStatus;
    use automerge::ROOT;

    fn synced(app: &mut BackupsApp, doc: &mut AutoCommit) {
        Widget::sync(app, doc, &ROOT);
    }

    fn staged(scenario: Option<&str>) -> (BackupsApp, AutoCommit) {
        let mut app = BackupsApp::with_catalog(Catalog::empty());
        let mut doc = AutoCommit::new();
        app.stage_scenario(scenario);
        synced(&mut app, &mut doc);
        (app, doc)
    }

    /// The live app fabricates nothing: no catalog, no sidebar, only setup.
    #[test]
    fn a_fresh_device_shows_setup_and_no_places() {
        let (app, _) = staged(Some("setup"));
        assert!(app.catalog.snapshots.is_empty());
        assert_eq!(app.setup_key(), Some(SetupKey::Start));
        assert!(!app.shell.has_sidebar());
    }

    /// A saved repository that can't be read is not first run: it says so,
    /// keeps its controls inert, and refuses effects until reselected.
    #[test]
    fn an_unreadable_repository_blocks_effects_until_reselected() {
        let (mut app, _) = staged(Some("unavailable"));
        assert!(matches!(app.setup_key(), Some(SetupKey::Unavailable(_))));
        assert!(!app.is_configured());
        app.start_backup();
        assert!(
            app.job.is_none(),
            "no worker starts on an unreadable repository"
        );
        assert!(app.failure.is_some(), "the refusal says why");
    }

    /// Choosing a place in the sidebar changes the page and keeps a snapshot
    /// of that page selected; the selection follows the page's filter.
    #[test]
    fn places_navigate_and_selection_follows_the_page() {
        let (mut app, mut doc) = staged(None);
        assert!(app.shell.has_sidebar());
        assert_eq!(app.page, Page::All);
        assert_eq!(app.selected.as_deref(), Some("w-0-1000"), "newest first");

        app.intents
            .push(Intent::Navigate(Page::Source("etc".into())));
        synced(&mut app, &mut doc);
        assert_eq!(app.page, Page::Source("etc".into()));
        let selected = app.selected.clone().expect("a snapshot of the source");
        assert_eq!(app.catalog.snapshot(&selected).unwrap().source_id, "etc");

        app.intents
            .push(Intent::Navigate(Page::Destination("b2".into())));
        synced(&mut app, &mut doc);
        let selected = app.selected.clone().expect("a snapshot on b2");
        assert_eq!(app.catalog.snapshot(&selected).unwrap().dest_id, "b2");

        app.intents.push(Intent::Navigate(Page::Retention));
        synced(&mut app, &mut doc);
        assert_eq!(app.page, Page::Retention);
    }

    /// Navigation, selection and the restore sheet are device-local: none of
    /// them writes the collaborative document.
    #[test]
    fn view_state_never_writes_the_document() {
        let (mut app, mut doc) = staged(None);
        doc.commit();
        let before = doc.get_heads();
        for intent in [
            Intent::Navigate(Page::Source("home".into())),
            Intent::SelectSnapshot("m-0-0824".into()),
            Intent::OpenRestore,
            Intent::CloseRestore,
        ] {
            app.intents.push(intent);
            synced(&mut app, &mut doc);
        }
        doc.commit();
        assert_eq!(doc.get_heads(), before);
    }

    /// The restore sheet is a modal: open, it is the top layer's only
    /// control surface and traps focus; Cancel closes it.
    #[test]
    fn the_restore_sheet_is_modal_and_cancellable() {
        let (mut app, mut doc) = staged(Some("restore"));
        assert!(app.restore.is_some(), "the scenario opens the sheet");
        assert!(Widget::traps_focus(&app));
        assert!(
            Widget::children(&app)
                .iter()
                .any(|child| child.test_id() == Some("backups-restore")),
            "the sheet is reachable for focus and accessibility"
        );
        app.intents.push(Intent::CloseRestore);
        synced(&mut app, &mut doc);
        assert!(app.restore.is_none());
        assert!(!Widget::traps_focus(&app));
    }

    /// A stepper the person moves dispatches the class's absolute count, the
    /// same encoding the palette's increment exports.
    #[test]
    fn a_retention_stepper_dispatches_the_palette_action() {
        let (mut app, mut doc) = staged(Some("schedules"));
        let daily = BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily);
        let expected = actions::encode(&actions::bump_retention_action(
            PolicyField::Daily,
            daily,
            1,
        ));
        app.retention[1].set(f64::from(daily + 1));
        Widget::update(&mut app, &mut doc, &ROOT);
        let dispatched = app.last_dispatched().expect("the stepper dispatched");
        assert_eq!(dispatched.payload, expected.payload);
        assert_eq!(
            BackupsDoc::retention(&doc, &ROOT, PolicyField::Daily),
            daily + 1
        );
        // A policy changed elsewhere (undo, a peer) reaches the stepper.
        BackupsDoc::set_retention(&mut doc, &ROOT, PolicyField::Daily, 3);
        synced(&mut app, &mut doc);
        assert!(app.retention[1].with(|v| (*v - 3.0).abs() < f64::EPSILON));
    }

    /// A failure is a banner the person can dismiss; it never hides the
    /// timeline it concerns.
    #[test]
    fn a_failure_banner_stays_until_dismissed() {
        let (mut app, mut doc) = staged(Some("failure"));
        assert!(app.failure.is_some());
        assert!(app.catalog.timeline(app.page.filter()).len() > 1);
        app.intents.push(Intent::DismissFailure);
        synced(&mut app, &mut doc);
        assert!(app.failure.is_none());
    }

    /// The health the bubble reports counts failed snapshots in words.
    #[test]
    fn the_health_line_counts_what_needs_attention() {
        let (mut app, _) = staged(None);
        assert!(app.health_line().contains("needs attention"));
        for snapshot in &mut app.catalog.snapshots {
            snapshot.status = SnapStatus::Complete;
        }
        assert_eq!(app.health_line(), "All backups healthy");
    }
}
