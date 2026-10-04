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
//!   * the durable retention policy lives in the resource document and changes
//!     only through a typed action; device-local locations are typed settings;
//!   * filesystem work is a typed effect running through framework jobs.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Effected, Setting, SurfaceCx,
};

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

use crate::actions::{self, BackupsEffect, Blocked};
use crate::doc::PolicyField;
use crate::i18n;
use crate::model::{Catalog, Filter, RetentionPolicy};
use crate::resource::{BackupLocation, Backups, DemoRepository};
use crate::view::{self, Intent, Intents, JobKind, Order, Page, Setup};

/// How long a result toast stays before it hides itself.
const TOAST_TTL: Duration = Duration::from_millis(3_500);

/// A folder chooser's answer, from the worker that waited for it.
type Picked = ui::portal::PortalOutcome;

/// Wait for the user to choose a folder. Runs on a job's worker thread.
fn pick_folder(title: &str, accept: &str) -> Picked {
    ui::portal::PickRequest::directory(title, accept)
        .thread_name("backups-folder-picker")
        .send()
        .recv()
        .unwrap_or_else(|_| {
            ui::portal::PortalOutcome::Failed("The folder chooser closed unexpectedly".into())
        })
}

/// What the surface's effects are doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Work {
    Idle,
    /// A folder chooser is open.
    Choosing,
    /// A backup, verification or restore is running.
    Running,
}

/// What a running job last reported, written by its worker and shown by the
/// progress banner.
type JobProgress = Arc<Mutex<Option<String>>>;

fn report(progress: &JobProgress, status: String) {
    if let Ok(mut shown) = progress.lock() {
        *shown = Some(status);
    }
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
    restore_changed: bool,
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
    /// All document edits and device effects use the framework dispatcher.
    dispatcher: Option<Dispatcher<Backups>>,
    location: Option<Setting<BackupLocation>>,
    loaded: BackupLocation,
    /// Retained complete recovery-point index. The visible resource snapshot
    /// stays bounded, while Ctrl+K searches every already-loaded version.
    search_index: crate::search_provider::BackupsSearchIndex,
    source_path: Option<PathBuf>,
    destination_path: Option<PathBuf>,
    /// A persisted local configuration can outlive an unplugged drive or a
    /// damaged repository. Keep that distinct from first-run so actions never
    /// imply the repository is ready when it was not successfully loaded.
    repository_error: Option<String>,
    work: Work,
    job_progress: JobProgress,
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

    /// A live surface starts empty; attach loads only this device's setting.
    pub fn new() -> Self {
        Self::with_catalog(Catalog::empty())
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
            restore_changed: false,
            toast: None,
            failure: None,
            status_bubble: ui::StatusBubble::new(),
            cursor: None,
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            built: None,
            dirty: true,
            demo: false,
            open_drawer: false,
            dispatcher: None,
            location: None,
            loaded: BackupLocation::default(),
            search_index,
            source_path: None,
            destination_path: None,
            repository_error: None,
            work: Work::Idle,
            job_progress: JobProgress::default(),
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
        if matches!(scenario, Some("setup" | "empty")) {
            return;
        }
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
            Some("drawer" | "rail") => self.open_drawer = true,
            _ => {}
        }
        self.dirty = true;
    }

    /// Take part in the resource: edits and effects go through the
    /// dispatcher, and the repository shown is this device's (a live
    /// window) or the scenario's (a capture).
    pub(crate) fn attach_framework(&mut self, cx: &SurfaceCx<Backups>) {
        self.dispatcher = Some(cx.dispatcher());
        if let Some(demo) = cx.fixture::<DemoRepository>() {
            self.set_catalog(demo.catalog.clone());
            self.demo = !demo.catalog.sources.is_empty();
            self.source_path.clone_from(&demo.location.source);
            self.destination_path.clone_from(&demo.location.destination);
            self.repository_error.clone_from(&demo.error);
        } else if cx.mode().is_live() {
            let location = cx.settings::<BackupLocation>();
            self.load_location(location.get());
            self.location = Some(location);
        }
    }

    /// Show the retention policy the document holds.
    pub(crate) fn present_policy(&mut self, policy: RetentionPolicy) {
        self.set_policy(policy);
    }

    /// Load the repository at `location`. A saved destination that is gone
    /// (an unplugged drive) is reported, never created: otherwise it could
    /// look like a fresh repository and receive a backup in the wrong place.
    fn load_location(&mut self, location: BackupLocation) {
        self.loaded = location.clone();
        self.source_path = location.source;
        self.destination_path = location.destination;
        self.repository_error = None;
        self.catalog = Catalog::empty();
        if let (Some(source), Some(destination)) = (&self.source_path, &self.destination_path) {
            if destination.is_dir() {
                match crate::engine::list_snapshots(destination) {
                    Ok(manifests) => {
                        self.catalog = Catalog::from_local(source, destination, &manifests);
                    }
                    Err(error) => {
                        self.repository_error =
                            Some(format!("Couldn’t read the saved backup location: {error}"));
                    }
                }
            } else {
                self.repository_error = Some(
                    "Couldn’t read the saved backup location: it is not mounted or is no longer a folder."
                        .into(),
                );
            }
        }
        self.catalog_rev += 1;
        self.search_index.sync(&self.catalog);
        self.dirty = true;
    }

    /// Reload the repository after a job changed it.
    fn reload_repository(&mut self) -> Result<(), String> {
        let (Some(source), Some(destination)) = (&self.source_path, &self.destination_path) else {
            return Ok(());
        };
        let manifests = crate::engine::list_snapshots(destination).map_err(|error| {
            let error = format!("Its repository could not be reloaded: {error}");
            self.repository_error = Some(error.clone());
            error
        })?;
        self.catalog = Catalog::from_local(source, destination, &manifests);
        self.catalog_rev += 1;
        self.search_index.sync(&self.catalog);
        self.dirty = true;
        Ok(())
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
    fn commit_retention(&mut self) {
        if self.dispatcher.as_ref().is_some_and(Dispatcher::read_only) {
            self.set_policy(self.policy);
            return;
        }
        for (state, field) in self.retention.iter().zip(PolicyField::ALL) {
            let shown = state.with(|value| value.round().clamp(0.0, 999.0)) as u32;
            if shown != policy_value(&self.policy, field) {
                if let Some(dispatcher) = &self.dispatcher {
                    dispatcher.apply(&actions::set_retention_action(field, shown));
                }
            }
        }
    }

    fn invoke(&self, effect: &BackupsEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    fn toast_now(&mut self, message: impl Into<String>) {
        self.toast(message);
    }

    fn toast_error(&mut self, message: impl Into<String>) {
        self.fail("Backup operation failed", message);
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

    /// Why the repository effects cannot run here now, if they cannot.
    pub(crate) fn blocked(&self) -> Option<Blocked> {
        if self.work != Work::Idle {
            return Some(Blocked {
                code: "busy",
                reason: "A backup operation is already running; wait for it to finish.".into(),
            });
        }
        if let Some(error) = &self.repository_error {
            return Some(Blocked {
                code: "not_configured",
                reason: format!("{error} Choose another backup location to continue."),
            });
        }
        if !self.is_configured() {
            return Some(Blocked {
                code: "not_configured",
                reason: "Choose a source and destination before running a backup".into(),
            });
        }
        None
    }

    /// The effects that cannot run on this device right now.
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if let Some(blocked) = self.blocked() {
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::BACKUP_NOW,
                blocked.code,
                blocked.reason.clone(),
            ));
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::VERIFY,
                blocked.code,
                blocked.reason.clone(),
            ));
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::RESTORE_SNAPSHOT,
                blocked.code,
                blocked.reason,
            ));
        }
        if self.work != Work::Idle {
            statuses.push(EffectStatus::unavailable(
                BackupsEffect::SETUP,
                "busy",
                "A backup operation is already running; wait for it to finish.",
            ));
        }
        statuses
    }

    fn has_snapshot(&self, id: &str) -> bool {
        self.catalog
            .snapshots
            .iter()
            .any(|snapshot| snapshot.id == id)
    }

    /// Answer one effect, from a button, the palette, an agent or a script.
    pub(crate) fn answer_effect(
        &mut self,
        effect: BackupsEffect,
        cx: &mut EffectCx<Backups, Self>,
    ) -> EffectAnswer {
        self.dirty = true;
        match effect {
            BackupsEffect::Setup => {
                if self.work != Work::Idle {
                    return Err(
                        "A backup operation is already running; wait for it to finish.".into(),
                    );
                }
                // A saved location that can no longer be read keeps its
                // source: only a new destination is chosen.
                if self.source_path.is_some() && !self.is_configured() {
                    Ok(self.choose_destination(cx))
                } else {
                    Ok(self.choose_source(cx))
                }
            }
            BackupsEffect::BackupNow => {
                if let Some(blocked) = self.blocked() {
                    self.toast_error(blocked.reason.clone());
                    return Err(blocked.reason);
                }
                Ok(self.start_backup(cx))
            }
            BackupsEffect::Verify { snapshot_id } => {
                self.repository_ready(&snapshot_id)?;
                self.selected = Some(snapshot_id.clone());
                Ok(self.start_verify(snapshot_id, cx))
            }
            BackupsEffect::RestoreSnapshot { snapshot_id } => {
                self.repository_ready(&snapshot_id)?;
                self.selected = Some(snapshot_id.clone());
                Ok(self.choose_restore_folder(snapshot_id, cx))
            }
            BackupsEffect::ShowSource { source_id } => {
                if !self
                    .catalog
                    .sources
                    .iter()
                    .any(|source| source.id == source_id)
                {
                    return Err("That source is unavailable on this device.".into());
                }
                self.page = Page::Source(source_id);
                self.selected = None;
                Ok(Effected::done())
            }
            BackupsEffect::ShowDestination { destination_id } => {
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
                Ok(Effected::done())
            }
            BackupsEffect::ShowSnapshot { snapshot_id } => {
                if !self.has_snapshot(&snapshot_id) {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.selected = Some(snapshot_id);
                self.page = Page::All;
                if !self.shell.is_inspector_open() {
                    self.shell.toggle_inspector();
                }
                Ok(Effected::done())
            }
            BackupsEffect::ShowSchedule { source_id } => {
                if !self
                    .catalog
                    .sources
                    .iter()
                    .any(|source| source.id == source_id)
                {
                    return Err("That schedule's source is unavailable on this device.".into());
                }
                self.page = Page::Retention;
                Ok(Effected::done())
            }
        }
    }

    /// Whether a verification or restore of `snapshot_id` may start now.
    fn repository_ready(&mut self, snapshot_id: &str) -> Result<(), String> {
        if let Some(blocked) = self.blocked() {
            self.toast_error(blocked.reason.clone());
            return Err(blocked.reason);
        }
        if !self.has_snapshot(snapshot_id) {
            return Err("That snapshot is unavailable in this local repository.".into());
        }
        Ok(())
    }

    fn choose_source(&mut self, cx: &mut EffectCx<Backups, Self>) -> Effected {
        self.work = Work::Choosing;
        cx.thread(
            |_| pick_folder("Choose a folder to protect", "Protect folder"),
            |app: &mut Self, picked, cx| {
                app.work = Work::Idle;
                match picked {
                    Picked::Opened(path) => {
                        app.source_path = Some(path);
                        Ok(app.choose_destination(cx))
                    }
                    Picked::Cancelled => Err("No folder was chosen; nothing was set up.".into()),
                    Picked::Failed(error) => {
                        app.toast_error(error.clone());
                        Err(error)
                    }
                }
            },
        )
    }

    fn choose_destination(&mut self, cx: &mut EffectCx<Backups, Self>) -> Effected {
        self.work = Work::Choosing;
        cx.thread(
            |_| pick_folder("Choose a backup location", "Use this location"),
            |app: &mut Self, picked, cx| {
                app.work = Work::Idle;
                let destination = match picked {
                    Picked::Opened(path) => path,
                    Picked::Cancelled => {
                        return Err("No location was chosen; nothing was set up.".into());
                    }
                    Picked::Failed(error) => {
                        app.toast_error(error.clone());
                        return Err(error);
                    }
                };
                let Some(source) = app.source_path.clone() else {
                    return Err("Choose a folder to protect first.".into());
                };
                if let Err(error) = crate::engine::validate_paths(&source, &destination) {
                    app.toast_error(error.to_string());
                    return Err(error.to_string());
                }
                let location = BackupLocation {
                    source: Some(source),
                    destination: Some(destination),
                };
                if let Some(setting) = &app.location
                    && let Err(error) = setting.set(location.clone())
                {
                    let error = format!("Couldn’t save the backup location: {error}");
                    app.toast_error(error.clone());
                    return Err(error);
                }
                app.load_location(location);
                Ok(app.start_backup(cx))
            },
        )
    }

    fn start_job(&mut self, kind: JobKind, status: &str) -> JobProgress {
        self.work = Work::Running;
        self.job_progress = JobProgress::new(Mutex::new(Some(status.to_owned())));
        self.job_status = Some(view::Job::new(kind, status));
        self.failure = None;
        self.dirty = true;
        Arc::clone(&self.job_progress)
    }

    fn finish_job(&mut self) {
        self.work = Work::Idle;
        self.job_status = None;
        self.dirty = true;
    }

    fn start_backup(&mut self, cx: &mut EffectCx<Backups, Self>) -> Effected {
        let (Some(source), Some(destination)) =
            (self.source_path.clone(), self.destination_path.clone())
        else {
            return Effected::done();
        };
        let policy = cx.live().get().policy;
        let shown = self.start_job(JobKind::Backup, "Preparing files…");
        cx.thread(
            move |progress| {
                let created = crate::engine::create_snapshot(&source, &destination, |step| {
                    report(
                        &shown,
                        i18n::counted("backups-progress-files", step.files, &view::size(step.bytes)),
                    );
                    progress.report("copying", Some(step.files), None);
                })
                .map_err(|error| error.to_string())?;
                progress.report("pruning", None, None);
                let pruned = crate::engine::prune_snapshots(&destination, &policy)
                    .map_err(|error| error.to_string());
                Ok::<_, String>((created, pruned))
            },
            |app: &mut Self, result, _| {
                app.finish_job();
                let (manifest, pruned) = result.inspect_err(|error| {
                    app.toast_error(error.clone());
                })?;
                if let Err(error) = app.reload_repository() {
                    app.toast_error(
                        "Backup finished, but its history could not be reopened. Choose the location again before another operation.",
                    );
                    return Err(error);
                }
                match &pruned {
                    Ok(_) => app.toast_now(format!(
                        "Backup complete: {} files, {}",
                        manifest.files,
                        ui::format::bytes(manifest.bytes)
                    )),
                    Err(error) => app.toast_error(format!(
                        "Backup complete, but older snapshots could not be removed: {error}"
                    )),
                }
                Ok(Effected::value(serde_json::json!({
                    "snapshot": manifest.id,
                    "files": manifest.files,
                    "bytes": manifest.bytes,
                    "removed": pruned.unwrap_or_default(),
                })))
            },
        )
    }

    fn start_verify(&mut self, id: String, cx: &mut EffectCx<Backups, Self>) -> Effected {
        let Some(destination) = self.destination_path.clone() else {
            return Effected::done();
        };
        self.start_job(JobKind::Verify, "Checking every file…");
        cx.thread(
            move |_| crate::engine::verify_snapshot(&destination, &id),
            |app: &mut Self, result, _| {
                app.finish_job();
                let result = result.map_err(|error| {
                    app.toast_error(error.to_string());
                    error.to_string()
                })?;
                app.toast_now(format!(
                    "Verified {} files, {}",
                    result.files,
                    ui::format::bytes(result.bytes)
                ));
                Ok(Effected::value(serde_json::json!({
                    "files": result.files,
                    "bytes": result.bytes,
                })))
            },
        )
    }

    fn choose_restore_folder(&mut self, id: String, cx: &mut EffectCx<Backups, Self>) -> Effected {
        self.work = Work::Choosing;
        cx.thread(
            |_| pick_folder("Restore into an empty folder", "Restore here"),
            |app: &mut Self, picked, cx| {
                app.work = Work::Idle;
                match picked {
                    // The sheet promises a destination chooser, not a
                    // background restore: it closes only once a folder was
                    // chosen, and the engine never overwrites one.
                    Picked::Opened(path) => {
                        app.restore = None;
                        app.restore_changed = true;
                        Ok(app.start_restore(id, path, cx))
                    }
                    Picked::Cancelled => Err("No folder was chosen; nothing was restored.".into()),
                    Picked::Failed(error) => {
                        app.toast_error(error.clone());
                        Err(error)
                    }
                }
            },
        )
    }

    fn start_restore(
        &mut self,
        id: String,
        restore_to: PathBuf,
        cx: &mut EffectCx<Backups, Self>,
    ) -> Effected {
        let Some(destination) = self.destination_path.clone() else {
            return Effected::done();
        };
        let shown = self.start_job(JobKind::Restore, "Restoring and checking files…");
        cx.thread(
            move |progress| {
                crate::engine::restore_snapshot(&destination, &id, &restore_to, false, |step| {
                    report(
                        &shown,
                        i18n::counted(
                            "backups-progress-files",
                            step.files,
                            &view::size(step.bytes),
                        ),
                    );
                    progress.report("restoring", Some(step.files), None);
                })
            },
            |app: &mut Self, result, _| {
                app.finish_job();
                let result = result.map_err(|error| {
                    app.toast_error(error.to_string());
                    error.to_string()
                })?;
                app.toast_now(format!(
                    "Restored {} files, {}",
                    result.files,
                    ui::format::bytes(result.bytes)
                ));
                Ok(Effected::value(serde_json::json!({
                    "files": result.files,
                    "bytes": result.bytes,
                })))
            },
        )
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
        self.restore_changed = true;
        self.restore = Some(view::restore_dialog(
            snapshot,
            &self.catalog,
            now(),
            &self.intents,
        ));
        self.dirty = true;
    }

    // ---- intents ------------------------------------------------------------

    fn drain_intents(&mut self) {
        for intent in self.intents.take() {
            self.apply(intent);
        }
        self.commit_retention();
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
            Intent::BackUpNow => self.invoke(&BackupsEffect::BackupNow),
            Intent::Verify => {
                if let Some(snapshot_id) = self.selected.clone() {
                    self.invoke(&BackupsEffect::Verify { snapshot_id });
                }
            }
            Intent::OpenRestore => self.open_restore(),
            Intent::ChooseRestoreFolder => {
                if let Some(snapshot_id) = self.selected.clone() {
                    self.invoke(&BackupsEffect::RestoreSnapshot { snapshot_id });
                }
            }
            Intent::CloseRestore => {
                self.restore = None;
                self.restore_changed = true;
            }
            Intent::Setup => self.invoke(&BackupsEffect::Setup),
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
        (self.demo || self.is_configured()) && self.work == Work::Idle
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

    /// Settings and job progress are local observations; neither writes the document.
    fn reconcile(&mut self) {
        if let Some(location) = self.location.as_ref().map(Setting::get)
            && location != self.loaded
            && self.work != Work::Running
        {
            self.load_location(location);
        }
        if self.work == Work::Running
            && let Ok(shown) = self.job_progress.lock()
            && let (Some(shown), Some(job)) = (shown.as_ref(), &self.job_status)
        {
            *job.line.borrow_mut() = shown.clone();
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
        if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
            return;
        }
        self.shell.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
            return;
        }
        self.shell.provide_mut(request);
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.reconcile();
        self.drain_intents();
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
        self.drain_intents();
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
        let shell = self.shell.build_layout(taffy, theme, node_map);
        let mut children = vec![shell];
        if let Some(dialog) = &mut self.restore {
            let node = dialog.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(node).cloned().unwrap_or_default();
            style.position = ui::taffy::Position::Absolute;
            let _ = taffy.set_style(node, style);
            children.push(node);
        }
        let node = taffy
            .new_with_children(
                ui::taffy::Style {
                    display: ui::taffy::Display::Flex,
                    size: ui::taffy::Size {
                        width: ui::taffy::Dimension::percent(1.0),
                        height: ui::taffy::Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &children,
            )
            .expect("shell and modal are valid layout children");
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    // NavShell owns its own manual policy at the child node. Applying it to
    // this overlay parent would interpret the modal as a shell column.
    fn manual_layout_policy(&self) -> Option<ui::widget::ManualPolicy> {
        None
    }
    fn layout_info(&self) -> Option<ui::widget::LayoutInfo> {
        None
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: Point,
    ) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let origin = Point::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        );
        let children = taffy.children(node).unwrap_or_default();
        if let Some(&shell) = children.first() {
            self.shell.apply_layout(taffy, shell, origin);
        }
        if let (Some(dialog), Some(&node)) = (&mut self.restore, children.get(1)) {
            dialog.apply_layout(taffy, node, origin);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        if let Some(dialog) = &mut self.restore {
            if let Some(&node) = ctx.taffy.children(ctx.node_id).unwrap_or_default().get(1) {
                dialog.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            *ctx.handled = true;
        } else {
            if let Some(&node) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() {
                self.shell
                    .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            // Preserve the four page shortcuts when no retained control owns the key.
            if !*ctx.handled
                && let WindowEvent::KeyboardInput { event, .. } = event
                && event.state == winit::event::ElementState::Pressed
                && let winit::keyboard::Key::Character(key) = &event.logical_key
            {
                let page = match key.as_str() {
                    "1" => Some(Page::All),
                    "2" => self
                        .catalog
                        .sources
                        .first()
                        .map(|s| Page::Source(s.id.clone())),
                    "3" => self
                        .catalog
                        .destinations
                        .first()
                        .map(|d| Page::Destination(d.id.clone())),
                    "4" => Some(Page::Retention),
                    _ => None,
                };
                if let Some(page) = page {
                    self.apply(Intent::Navigate(page));
                    *ctx.handled = true;
                }
            }
        }
        self.drain_intents();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Window)
                .with_name("Backups")
                .with_bounds(self.shell.get_rect()),
        )
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.shell];
        if let Some(dialog) = &self.restore {
            children.push(dialog);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.shell);
        if let Some(dialog) = &self.restore {
            visitor(dialog);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut dyn Widget> = vec![&mut self.shell];
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

    fn take_structure_dirty(&mut self) -> bool {
        let changed = std::mem::take(&mut self.restore_changed);
        let shell = self.shell.take_structure_dirty();
        let dialog = self
            .restore
            .as_mut()
            .is_some_and(Widget::take_structure_dirty);
        changed || shell || dialog
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
            || self.work != Work::Idle
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SnapStatus;
    use automerge::{AutoCommit, ROOT};

    fn synced(app: &mut BackupsApp, doc: &mut AutoCommit) {
        Widget::sync(app, doc, &ROOT);
    }

    fn staged(scenario: Option<&str>) -> (BackupsApp, AutoCommit) {
        let mut app = BackupsApp::with_catalog(Catalog::empty());
        let mut doc = AutoCommit::new();
        if !matches!(scenario, Some("setup" | "empty" | "unavailable")) {
            app.set_catalog(Catalog::sample(now()));
            app.demo = true;
        }
        if scenario == Some("unavailable") {
            app.source_path = Some(PathBuf::from("/documents"));
            app.destination_path = Some(PathBuf::from("/backup-drive"));
            app.repository_error = Some("The backup drive is unavailable.".into());
        }
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
        let blocked = app
            .blocked()
            .expect("unreadable repositories block effects");
        assert_eq!(blocked.code, "not_configured");
        assert!(blocked.reason.contains("Choose another backup location"));
        assert_eq!(
            app.work,
            Work::Idle,
            "no worker starts on an unreadable repository"
        );
        assert_eq!(app.effect_statuses().len(), 3);
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

/// Main's former hit-map regression cases, driven through the replacement
/// retained shell, rows, fields and modal. Geometry comes from the actual
/// laid-out widgets and semantic nodes, never a second layout implementation.
#[cfg(test)]
mod retained_tests {
    use super::*;
    use automerge::ReadDoc;
    use ui::a11y::Action;
    use ui::headless::Framed;

    type Frame = Framed<BackupsApp>;

    fn frame(scenario: Option<&str>, width: u32, height: u32, scale: f32) -> Frame {
        let mut app = BackupsApp::with_catalog(Catalog::sample(now()));
        app.demo = true;
        app.stage_scenario(scenario);
        Framed::new(
            ui::State::new(),
            app,
            (width, height),
            Theme::dark().with_text_scale(scale),
        )
    }

    fn settle(f: &mut Frame) {
        Framed::harness_mut(f).relayout();
        f.shell.snap();
        Framed::harness_mut(f).relayout();
    }

    fn labelled(f: &Frame, name: &str) -> Rect {
        let bounds = Framed::harness(f)
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(name))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("{name} is present in the real accessibility tree"));
        Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
    }

    fn click(f: &mut Frame, name: &str) {
        let at = labelled(f, name).center();
        Framed::harness_mut(f).click_at(at.x, at.y);
        settle(f);
    }

    fn navigation_row(f: &Frame, name: &str) -> (ui::accesskit::NodeId, Rect) {
        let (id, node) = Framed::harness(f)
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.role() == ui::accesskit::Role::TreeItem && node.label() == Some(name)
            })
            .unwrap_or_else(|| panic!("navigation row {name} is accessible"));
        let bounds = node.bounds().expect("navigation row has painted bounds");
        (id, Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1))
    }

    fn inside(rect: Rect, width: f64, height: f64) {
        assert!(rect.width() > 0.0 && rect.height() > 0.0, "{rect:?}");
        assert!(rect.x0 >= 0.0 && rect.x1 <= width, "{rect:?} fits {width}");
        assert!(
            rect.y0 >= 0.0 && rect.y1 <= height,
            "{rect:?} fits {height}"
        );
    }

    fn rows(f: &Frame) -> Vec<(String, Rect)> {
        let mut rows: Vec<_> = Framed::harness(f)
            .targets()
            .into_iter()
            .filter(|(id, rect)| {
                id.starts_with("snapshot-") && rect.width() > 0.0 && rect.height() > 0.0
            })
            .collect();
        rows.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
        rows
    }

    #[test]
    fn catalog_and_repository_state_decide_the_offered_controls() {
        let live = BackupsApp::new();
        assert!(live.catalog.sources.is_empty());
        assert!(live.catalog.destinations.is_empty());
        assert!(live.catalog.snapshots.is_empty());
        let mut first = Framed::new(ui::State::new(), live, (1000, 700), Theme::dark());
        inside(labelled(&first, "Choose folder…"), 1000.0, 700.0);
        assert!(!first.shell.has_sidebar());
        let sample = frame(None, 1280, 800, 1.0);
        assert!(!sample.catalog.sources.is_empty());
        assert!(!sample.catalog.destinations.is_empty());
        assert!(
            sample
                .catalog
                .snapshots
                .iter()
                .all(|snapshot| !snapshot.files.is_empty())
        );

        first.source_path = Some(PathBuf::from("/documents"));
        first.destination_path = Some(PathBuf::from("/backup-drive"));
        first.repository_error = Some("The drive is unavailable".into());
        settle(&mut first);
        assert!(!first.is_configured());
        let blocked = first.blocked().expect("repository is blocked");
        assert_eq!(blocked.code, "not_configured");
        assert_eq!(first.effect_statuses().len(), 3);
        inside(labelled(&first, "Choose another location…"), 1000.0, 700.0);
        let resources: Vec<_> = actions::live_rows(&sample.catalog, Some(&blocked))
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect();
        for id in ["verify", "restore"] {
            let offered = resources[2].children[0]
                .actions
                .iter()
                .find(|a| a.id == id)
                .unwrap();
            assert!(matches!(&offered.contract.availability,
                tilo_actions::Availability::Unavailable { reason, .. } if reason == &blocked.reason));
        }

        let mut empty = frame(None, 1280, 800, 1.0);
        let mut catalog = empty.catalog.clone();
        catalog.snapshots.clear();
        empty.set_catalog(catalog);
        settle(&mut empty);
        assert!(rows(&empty).is_empty());
        inside(labelled(&empty, "Back up now"), 1280.0, 800.0);
        let tree = Framed::harness(&empty).inspection_json().unwrap();
        assert!(!tree.contains("backups-verify"));
        assert!(!tree.contains("backups-restore"));
        assert!(tree.contains("No snapshots yet"));
    }

    fn wheel(f: &mut Frame, at: Point, dy: f64) {
        Framed::harness_mut(f).send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(at.x, at.y),
        });
        Framed::harness_mut(f).scroll(0.0, dy);
        settle(f);
    }

    #[test]
    fn rail_paints_and_scrolls_only_when_needed() {
        let narrow = frame(None, 390, 844, 1.0);
        assert_eq!(
            narrow.shell.geometry().sidebar,
            ui::widget::SidebarPlacement::Drawer
        );
        assert!(!narrow.shell.is_sidebar_open());
        assert!(
            !Framed::harness(&narrow)
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.role() == ui::accesskit::Role::TreeItem),
            "the closed offscreen drawer exposes no painted or actionable rows"
        );
        let mut short = frame(None, 1280, 300, 1.0);
        assert_eq!(
            short.shell.geometry().sidebar,
            ui::widget::SidebarPlacement::Docked
        );
        let header = short.shell.geometry().toggle;
        let all = navigation_row(&short, "All backups").1;
        let at = short.shell.geometry().sidebar_column.center();
        wheel(&mut short, at, -2.0);
        assert_eq!(
            short.shell.geometry().toggle,
            header,
            "the header never scrolls"
        );
        let after = Framed::harness(&short)
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.role() == ui::accesskit::Role::TreeItem && node.label() == Some("All backups")
            })
            .and_then(|(_, node)| node.bounds());
        assert!(
            after.is_none_or(|rect| rect.y0 < all.y0),
            "the short sidebar scrolls its rows"
        );
        wheel(&mut short, at, 100.0);
        assert_eq!(navigation_row(&short, "All backups").1, all);
        let at = short.shell.geometry().content_column.center();
        wheel(&mut short, at, -2.0);
        assert_eq!(
            navigation_row(&short, "All backups").1,
            all,
            "timeline scroll leaves navigation alone"
        );
        let mut tall = frame(None, 1280, 1000, 1.0);
        let all = navigation_row(&tall, "All backups").1;
        let at = tall.shell.geometry().sidebar_column.center();
        wheel(&mut tall, at, -2.0);
        assert_eq!(
            navigation_row(&tall, "All backups").1,
            all,
            "a fitting sidebar does not scroll"
        );
    }

    #[test]
    fn phone_chrome_keeps_tabs_clear_and_fully_named() {
        // The four cramped tabs became a drawer of fully named places.
        for width in [340, 390] {
            for scale in [1.0, 2.0] {
                let mut f = frame(None, width, 844, scale);
                let primary = labelled(&f, "Back up now");
                let toggle = labelled(&f, "Show sidebar");
                inside(primary, f64::from(width), 844.0);
                inside(toggle, f64::from(width), 844.0);
                assert!(primary.intersect(toggle).is_zero_area());
                click(&mut f, "Show sidebar");
                assert!(f.shell.is_sidebar_open());
                for name in ["All backups", "Home", "Internal pool", "Retention"] {
                    let item = navigation_row(&f, name).1;
                    inside(item, f64::from(width), 844.0);
                }
            }
        }
    }

    #[test]
    fn view_navigation_is_device_local_through_pointer_keyboard_and_accessibility() {
        let mut f = frame(None, 1280, 800, 1.0);
        let before = Framed::harness(&f).state().doc.clone().get_heads();
        let destination = navigation_row(&f, "Internal pool").1.center();
        Framed::harness_mut(&mut f).click_at(destination.x, destination.y);
        settle(&mut f);
        assert_eq!(f.page, Page::Destination("pool".into()));
        let retention = navigation_row(&f, "Retention").0;
        Framed::harness_mut(&mut f)
            .perform_a11y_action(retention, &Action::Activate)
            .unwrap();
        settle(&mut f);
        assert_eq!(f.page, Page::Retention);
        Framed::harness_mut(&mut f).key("1");
        settle(&mut f);
        assert_eq!(f.page, Page::All);
        click(&mut f, "Restore…");
        assert!(f.restore.is_some());
        assert_eq!(Framed::harness(&f).state().doc.clone().get_heads(), before);
        Framed::harness_mut(&mut f).key("Escape");
        settle(&mut f);
        assert!(f.restore.is_none());
        f.source_path = Some(PathBuf::from("/protected"));
        f.destination_path = Some(PathBuf::from("/backups"));
        Framed::harness_mut(&mut f).key("4");
        settle(&mut f);
        assert_eq!(f.page, Page::Retention);
        assert_eq!(Framed::harness(&f).state().doc.clone().get_heads(), before);
    }

    #[test]
    fn retention_steppers_stay_reachable_and_stack_at_large_text() {
        for (width, scale) in [(1280, 1.0), (390, 1.0), (880, 2.0), (390, 2.0)] {
            let f = frame(Some("schedules"), width, 1000, scale);
            let mut controls = Vec::new();
            for field in PolicyField::ALL {
                // Field gives the control its visible label. Inspect the
                // spinbutton itself, not the separate static label node.
                let bounds = Framed::harness(&f)
                    .a11y_nodes()
                    .into_iter()
                    .find(|(_, node)| {
                        node.role() == ui::accesskit::Role::SpinButton
                            && node.label() == Some(field.label())
                    })
                    .and_then(|(_, node)| node.bounds())
                    .expect("the retention spinbutton has accessible bounds");
                let rect = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1);
                inside(rect, f64::from(width), 1000.0);
                assert!(
                    rect.width() >= 2.0 * rect.height(),
                    "steppers have distinct square targets"
                );
                if scale == 2.0 {
                    assert!(rect.height() >= 44.0);
                }
                controls.push(rect);
            }
            for pair in controls.windows(2) {
                assert!(pair[0].y1 <= pair[1].y0, "retention fields never overlap");
            }
        }
    }

    #[test]
    fn recovery_surfaces_reflow_with_text_scale_without_overlap_or_elision() {
        let normal = frame(None, 1280, 800, 1.0);
        let large = frame(None, 1280, 800, 2.0);
        let one = rows(&normal);
        let two = rows(&large);
        assert!(one.len() > 3);
        assert!(two.len() > 3);
        assert!(two[0].1.height() > one[0].1.height());
        for pair in two.windows(2) {
            assert!(pair[0].1.y1 <= pair[1].1.y0);
        }
        let mut phone = frame(None, 390, 844, 2.0);
        let points = rows(&phone);
        assert!(
            points.len() >= 3,
            "every recovery point remains a retained target"
        );
        assert!(points.iter().all(|(_, rect)| rect.height() >= 44.0));
        for pair in points.windows(2) {
            assert!(pair[0].1.y1 <= pair[1].1.y0);
        }
        // Scroll the real content until this recovery point can be touched,
        // then select it at its painted rectangle (the stats precede it).
        let id = &points[1].0;
        let content = phone.shell.geometry().content_column;
        for _ in 0..60 {
            let row = Framed::harness(&phone).targets()[id];
            if row.y0 >= content.y0 + phone.shell.geometry().header_height && row.y1 <= content.y1 {
                break;
            }
            let dy = if row.y0 < content.y0 + phone.shell.geometry().header_height {
                1.0
            } else {
                -1.0
            };
            wheel(&mut phone, content.center(), dy);
        }
        let row = Framed::harness(&phone).targets()[id];
        inside(row, 390.0, 844.0);
        Framed::harness_mut(&mut phone).click_at(row.center().x, row.center().y);
        settle(&mut phone);
        assert_eq!(
            phone.selected.as_deref(),
            points[1].0.strip_prefix("snapshot-")
        );
        assert!(phone.shell.is_inspector_open());
        let verify = labelled(&phone, "Verify files");
        let restore = labelled(&phone, "Restore…");
        assert!(verify.intersect(restore).is_zero_area());

        let mut unavailable = BackupsApp::new();
        let error = "Couldn’t read the saved backup location: the drive is unavailable.";
        unavailable.source_path = Some(PathBuf::from("/documents"));
        unavailable.repository_error = Some(error.into());
        let recovery = Framed::new(
            ui::State::new(),
            unavailable,
            (520, 820),
            Theme::dark().with_text_scale(2.0),
        );
        let tree = Framed::harness(&recovery).inspection_json().unwrap();
        assert!(
            tree.contains(error),
            "full failure reason reaches accessibility"
        );
        assert!(
            tree.contains("Backups are paused until it is back or you choose another location.")
        );
        inside(
            labelled(&recovery, "Choose another location…"),
            520.0,
            820.0,
        );

        let mut narrow = frame(None, 390, 844, 2.0);
        for source in &mut narrow.catalog.sources {
            source.path =
                "/an/intentionally/very/long/device/local/path/that/cannot/fit/a/phone/header"
                    .into();
        }
        narrow.open_restore();
        settle(&mut narrow);
        let dialog = narrow.restore.as_ref().unwrap();
        let tree = ui::headless::FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(view::restore_dialog(
                narrow
                    .catalog
                    .snapshot(narrow.selected.as_deref().unwrap())
                    .unwrap(),
                &narrow.catalog,
                now(),
                &Intents::default(),
            )),
            (390, 844),
            Theme::dark().with_text_scale(2.0),
        )
        .inspection_json()
        .unwrap();
        assert!(!tree.contains("/an/intentionally"));
        inside(dialog.primary_rect(), 390.0, 844.0);
        inside(dialog.secondary_rect(), 390.0, 844.0);
    }

    #[test]
    fn hosted_restore_modal_maps_assistive_actions_and_traps_keyboard_focus() {
        use ui::framework::testing::TestApp;
        let mut app = TestApp::<crate::resource::BackupsShell>::capture(Some("restore")).unwrap();
        for _ in 0..4 {
            app.harness().key("Tab");
            let focused = app
                .harness()
                .focused_access_node()
                .expect("the restore contents own keyboard focus");
            let bounds = focused
                .bounds
                .expect("the focused contents have geometry")
                .to_rect();
            let card = app
                .harness()
                .with_widget_mut::<BackupsApp, _>(|surface| {
                    surface.restore.as_ref().unwrap().dialog_rect()
                })
                .unwrap();
            assert!(
                card.contains(bounds.center()),
                "Tab cannot reach the background: {focused:?}"
            );
        }
        let choose = app
            .harness()
            .a11y_node_named("Choose folder…")
            .expect("restore primary action");
        app.harness()
            .perform_a11y_action(choose, &Action::Activate)
            .unwrap();
        app.settle();
        app.harness()
            .with_widget_mut::<BackupsApp, _>(|surface| {
                assert_eq!(
                    surface.work,
                    Work::Idle,
                    "an unconfigured capture cannot start a restore"
                );
                assert!(surface.restore.is_some());
            })
            .unwrap();
        let cancel = app
            .harness()
            .a11y_node_named("Cancel")
            .expect("restore cancel action");
        app.harness()
            .perform_a11y_action(cancel, &Action::Activate)
            .unwrap();
        app.settle();
        app.harness()
            .with_widget_mut::<BackupsApp, _>(|surface| assert!(surface.restore.is_none()))
            .unwrap();
        // Reopen through the real toolbar, then exercise keyboard dismissal.
        let open = app
            .harness()
            .a11y_node_named("Restore…")
            .expect("restore toolbar action");
        app.harness()
            .perform_a11y_action(open, &Action::Activate)
            .unwrap();
        app.settle();
        app.harness().key("Escape");
        app.settle();
        app.harness()
            .with_widget_mut::<BackupsApp, _>(|surface| assert!(surface.restore.is_none()))
            .unwrap();
    }

    #[test]
    fn restore_sheet_offers_only_truthful_modal_storage_controls() {
        let mut f = frame(None, 1280, 800, 1.0);
        inside(labelled(&f, "Back up now"), 1280.0, 800.0);
        inside(labelled(&f, "Verify files"), 1280.0, 800.0);
        let behind = labelled(&f, "Home").center();
        click(&mut f, "Restore…");
        let before = f.page.clone();
        let dialog = f.restore.as_ref().unwrap();
        let primary = dialog.primary_rect();
        let cancel = dialog.secondary_rect();
        assert!((primary.y0 - cancel.y0).abs() < f64::EPSILON);
        assert!((primary.y1 - cancel.y1).abs() < f64::EPSILON);
        assert!(cancel.x1 <= primary.x0);
        assert!(primary.height() >= 32.0);
        inside(primary, 1280.0, 800.0);
        inside(cancel, 1280.0, 800.0);
        assert!(Widget::traps_focus(&*f));
        let names: Vec<_> = Framed::harness(&f)
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, n)| n.label().map(str::to_owned))
            .collect();
        assert!(names.iter().any(|n| n == "Choose folder…"));
        assert!(names.iter().any(|n| n == "Cancel"));
        assert!(!names.iter().any(|n| n.contains("Choose files")));
        assert!(
            !names.iter().any(|n| n == "Home"),
            "modal hides background controls from assistive technology"
        );
        Framed::harness_mut(&mut f).click_at(behind.x, behind.y);
        settle(&mut f);
        assert_eq!(f.page, before, "the scrim swallows background navigation");
        assert!(
            f.restore.is_some(),
            "a scrim press never dismisses recovery"
        );
        click(&mut f, "Cancel");
        assert!(f.restore.is_none());
        let live: Vec<_> = actions::live_rows(&f.catalog, None)
            .into_iter()
            .map(ui::framework::LiveRow::into_resource)
            .collect();
        assert_eq!(
            live.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            [
                "backups.source-set",
                "backups.destination-set",
                "backups.snapshot-set"
            ]
        );
        let snapshot = &live[2].children[0];
        assert_eq!(
            snapshot
                .actions
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["open", "verify", "restore"]
        );
        assert!(
            snapshot
                .children
                .iter()
                .any(|child| child.kind == "backups.restore-target")
        );
    }

    #[test]
    fn a_fresh_backup_record_stays_empty_until_a_retention_action() {
        let mut app = BackupsApp::new();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(
            state
                .doc
                .get(automerge::ROOT, "ret_hourly")
                .unwrap()
                .is_none()
        );
    }
}
