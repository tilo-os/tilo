//! Root widget for the Backups app.
//!
//! The whole UI is custom-drawn into the Vello scene (a dense dashboard:
//! source rail, snapshot timeline, storage gauges, retention editor, restore
//! sheet) with our own layout + hit-testing — the kind of chart/canvas
//! surface the toolkit expects apps to build on top of `Widget`. Reusable
//! ui widgets don't fit a timeline this dense, so we lay it out by hand.
//!
//! State split:
//!   * the *catalog* (snapshot listing), navigation, selections, modal state,
//!     focus, and device/backend observations are local to this widget;
//!   * durable retention policy lives in the Automerge doc (see
//!     [`BackupsDoc`]) and is changed only through a typed action.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::chrome::{HAIRLINE_ALPHA, PANEL_BORDER_ALPHA, chrome_hairline, panel_border, panel_ground};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::Spacing;
use ui::tokens::colors::Colors;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonFace, ModalAction, ModalFrame, ModalSpec, SegmentedFace, SidebarDrawer, Toggle,
    ToggleFace, ToggleGroup, Widget,
};
use ui::{card_column_width, card_columns};

use crate::actions::{self, BACKUPS_ACTION_TYPE};
use crate::doc::{BackupsDoc, PolicyField};
use crate::model::{
    Catalog, DestKind, Destination, Health, Retention, RetentionPolicy, SnapFile, SnapStatus,
    SnapType, Snapshot, Source, SourceKind, View,
};
use crate::paint;

// Fixed bands are the 1× *floors* of renderer-measured values: the top bar and
// its tabs (`chrome_metrics`), the timeline's day header / snapshot row /
// expanded detail (`draw_timeline_list`). At 1× the measured value equals the
// constant; the band only grows once the accessibility text scale makes its
// labels taller than the band was drawn for.
const BAR_H: f64 = 52.0;
const RAIL_W: f64 = 256.0;

// ---- spacing rhythm (HIG §7b) ------------------------------------------
//
// One rhythm for the dashboard, read from the unscaled token scale so every
// gap here is a named step: `S1` between a row and the detail attached to
// it, `S2` between related rows (rail rows, snapshot rows in a day), `S3`
// between sibling cards and inside a card's separator inset, `S4` for card
// padding and the gap between sections, `S6` before the next group.
const S1: f64 = Spacing::BASE.s1 as f64;
const S2: f64 = Spacing::BASE.s2 as f64;
const S3: f64 = Spacing::BASE.s3 as f64;
const S4: f64 = Spacing::BASE.s4 as f64;
const S6: f64 = Spacing::BASE.s6 as f64;
/// Window gutter and card padding.
const PAD: f64 = S4;
/// A view tab's pill sits `S1` inside its hit cell, so neighbouring pills
/// keep an `S2` gap while the cells stay contiguous for hit-testing.
const TAB_PILL_INSET: f64 = S1;
const TAB_W: f64 = 132.0;
/// Smallest usable view tab; the strip shrinks to the band it is given down
/// to this target width instead of overrunning the primary action.
const MIN_TAB_W: f64 = 44.0;
const TAB_H: f64 = 34.0;
const ROW_H: f64 = 46.0;
const DETAIL_H: f64 = 70.0;
const DAY_H: f64 = 30.0;
/// Space permanently reserved for the transient status pill. The bubble can
/// surface after a job finishes or while the user reaches the lower corner,
/// so content must never be placed underneath it.
const STATUS_SAFE_BOTTOM: f64 = 56.0;
/// Below this window width the Timeline source rail overlays (a summoned
/// drawer) instead of docking beside the snapshot list. Both breakpoints are
/// scaled by the text scale: an 800px window at 2× carries what a 400px
/// window carries at 1×, so it takes the compact chrome.
const RAIL_BREAKPOINT: f64 = 820.0;
/// Below this width the top-bar chrome compacts (icon-only primary action,
/// tighter tabs) and multi-column tile/card grids drop toward a single column.
const NARROW_BREAKPOINT: f64 = 720.0;
/// The consequence of an unavailable repository must remain visible at every
/// text scale: it is the reason backup actions are deliberately disabled.
const RECOVERY_SAFETY_COPY: &str = "Backups are paused until this location is reopened.";

/// A closed overlay is positioned mostly outside the content rect. Require a
/// non-zero eased reveal before painting it; otherwise its off-screen tail can
/// show as a mysterious sliver at the opposite edge of a phone window.
fn should_paint_rail(drawer: &SidebarDrawer, rail: Rect) -> bool {
    rail.width() > 4.0 && drawer.reveal() > 0.001
}

/// The face a selected row takes on `ground`: `(fill, edge)`. `selection_tier`
/// lifts `surfaces.base → raised`, which is invisible on a raised card on the
/// light preset and a weak step on the dark one, so the fill is the derived
/// edge colour of the ground used as a wash — `edge_on` climbs its alpha
/// ladder until the step registers on that ground — and the ring is the
/// panel-strength derived edge: a step, never an accent wash (HIG §9.3).
///
/// Open — needs coordinator: `ui::chrome::selection_tier_on(theme, state,
/// ground)` once merged (the same helper lives in firewall, backups and
/// remote).
fn selected_on(ground: Color) -> (Color, Color) {
    (
        ui::surface::edge_on(ground),
        ui::surface::edge_on_with(ground, PANEL_BORDER_ALPHA),
    )
}

/// Greedily wraps recovery copy using the same renderer that paints it.
///
/// Recovery guidance is not decorative: eliding it can hide either the
/// repository failure or the fact that backup operations are paused. Keep
/// every word, including at 200% text, and let the notice grow vertically.
fn wrap_recovery_copy(text: &str, tr: &mut TextRenderer, size: f32, max_width: f64) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();

    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if f64::from(tr.measure(&candidate, size, None).0) > max_width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Click/hover targets discovered during `draw` and consumed by
/// `handle_event`. Cleared and rebuilt every frame.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Tab(View),
    Source(Option<String>),
    Snap(String),
    Dest(String),
    Policy(PolicyField, i64),
    RestoreOpen,
    RestoreConfirm,
    RestoreCancel,
    Setup,
    BackupNow,
    Verify,
    /// The Timeline source-rail collapse / summon toggle.
    ToggleRail,
    /// The scrim behind an open overlay rail (tap to dismiss).
    CloseRail,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShotScenario {
    Restore,
}

/// Local view state. It is intentionally never read from or written to the
/// collaborative document: another device's active modal or local repository
/// selection must not navigate this device or trigger a restore prompt.
struct UiCache {
    view: View,
    sel_source: Option<String>,
    sel_snapshot: Option<String>,
    sel_dest: Option<String>,
    restore_open: bool,
    policy: RetentionPolicy,
}

impl Default for UiCache {
    fn default() -> Self {
        Self {
            view: View::Timeline,
            sel_source: None,
            sel_snapshot: None,
            sel_dest: Some("pool".into()),
            restore_open: false,
            policy: RetentionPolicy::default(),
        }
    }
}

pub struct BackupsApp {
    catalog: Catalog,
    ui: UiCache,
    rect: Rect,
    node: Option<NodeId>,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    last_mouse: Point,
    timeline_scroll: f64,
    timeline_view: Rect,
    timeline_content_h: f64,
    /// Scroll offset of the source rail's rows (the header band stays put)
    /// and how far they can scroll, as the last frame measured them.
    rail_scroll: f64,
    rail_scroll_max: f64,
    /// Top-bar band and tab height as the last frame measured them
    /// (`chrome_metrics`); `BAR_H` / `TAB_H` floors.
    bar_h: f64,
    tab_h: f64,
    policy_focus: usize,
    toast: Option<(String, ToastKind, Instant)>,
    seeded: bool,
    /// The Timeline source rail as the shared adaptive drawer: docked beside the
    /// snapshot list on wide windows, a summoned overlay drawer when narrow.
    drawer: SidebarDrawer,
    /// `--shots` hook: force the overlay rail open so its narrow state can be
    /// captured headlessly (`TILO_BACKUPS_RAIL_OPEN=1`).
    force_rail_open: bool,
    /// Framework-owned action dispatch: every retention-policy edit goes through
    /// here so it brackets one undo boundary, encodes under [`BACKUPS_ACTION_TYPE`],
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: crate::resource::ActionSink,
    /// Transient floating status bubble: the health + snapshot tally surfaced as
    /// a bottom-right pill instead of a permanent bottom strip, so the dashboard
    /// spans to the window bottom. Poked when the tally/health changes and shown
    /// while the pointer hovers the bottom-right corner, then fades.
    status_bubble: ui::StatusBubble,
    /// The label last shown in the bubble, so a poke fires only on a real change
    /// (`draw` runs every frame; poking unconditionally would pin it open).
    status_shown: String,
    /// `--shots` / `--scenario status` forces the bubble fully visible so a still
    /// frame captures the floating pill.
    shots_status: bool,
    /// One-shot visual-regression state. Never persisted or sent to the
    /// backup worker; it only lets CI exercise recovery/progress framing.
    shot_scenario: Option<ShotScenario>,
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
    job_status: Option<String>,
}

impl Default for BackupsApp {
    fn default() -> Self {
        Self::new()
    }
}

/// One recent-activity feed row: the snapshot and the "when" and "what"
/// lines it is listed with.
struct ActivityEntry<'a> {
    event: &'a Snapshot,
    when: String,
    action: String,
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
    /// shows onboarding until the user configures a source + destination.
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
                        app.catalog = Catalog::from_local(&source, &destination, &manifests);
                        app.sync_search_index();
                    }
                    Err(error) => {
                        app.repository_error =
                            Some(format!("Couldn’t read the saved backup location: {error}"));
                    }
                }
            } else {
                app.repository_error = Some(
                    "Couldn’t read the saved backup location: it is not mounted or is no longer a folder."
                        .into(),
                );
            }
        }
        app
    }

    /// The demo app: pre-seeded with the sample catalog. Used only by the
    /// `--shots` renderer so screenshots exercise a populated dashboard —
    /// never the live launch.
    pub fn new_demo() -> Self {
        Self::with_catalog(Catalog::sample())
    }

    fn with_catalog(catalog: Catalog) -> Self {
        let search_index = crate::search_provider::BackupsSearchIndex::new(&catalog);
        Self {
            catalog,
            ui: UiCache::default(),
            rect: Rect::ZERO,
            node: None,
            hits: Vec::new(),
            hover: None,
            last_mouse: Point::ZERO,
            timeline_scroll: 0.0,
            timeline_view: Rect::ZERO,
            timeline_content_h: 0.0,
            rail_scroll: 0.0,
            rail_scroll_max: 0.0,
            bar_h: BAR_H,
            tab_h: TAB_H,
            policy_focus: 0,
            toast: None,
            seeded: false,
            drawer: SidebarDrawer::new()
                .left()
                .docked_width(RAIL_W)
                .overlay_width(RAIL_W.max(288.0))
                .breakpoint(RAIL_BREAKPOINT),
            force_rail_open: std::env::var("TILO_BACKUPS_RAIL_OPEN").as_deref() == Ok("1"),
            actions: crate::resource::ActionSink::default(),
            status_bubble: ui::StatusBubble::new(),
            status_shown: String::new(),
            shots_status: false,
            shot_scenario: None,
            search_index,
            source_path: None,
            destination_path: None,
            repository_error: None,
            picker: None,
            job: None,
            job_status: None,
        }
    }

    fn sync_search_index(&self) {
        self.search_index.sync(&self.catalog);
    }

    /// Adjust one retention field by `delta`, clamped to `[0, 999]`, through the
    /// action host. This is the single production funnel both the on-screen +/-
    /// buttons and the keyboard arrow / `+` / `-` handlers call, and it dispatches
    /// the byte-identical [`BackupsAction`](crate::actions::BackupsAction) the
    /// Ctrl+K increment/decrement spec exports.
    pub(crate) fn bump_retention(
        &mut self,
        doc: &mut AutoCommit,
        root: &ObjId,
        field: PolicyField,
        delta: i64,
    ) {
        let cur = BackupsDoc::retention(doc, root, field);
        let action = actions::bump_retention_action(field, cur, delta);
        self.actions.dispatch_with_context(
            doc,
            BACKUPS_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.backups", "Backups", &action),
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
        let local_unpublished = self.ui.policy != RetentionPolicy::default();
        if untouched && local_unpublished {
            return;
        }
        self.ui.policy = state.policy;
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
        &self.ui.policy
    }

    #[cfg(test)]
    pub(crate) fn ui_policy_mut(&mut self) -> &mut RetentionPolicy {
        &mut self.ui.policy
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Backups>,
    ) {
        self.actions.attach(dispatcher);
    }

    fn load_cache(&mut self, doc: &AutoCommit, root: &ObjId) {
        self.ui.policy = BackupsDoc::policy(doc, root);
    }

    fn toast_now(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ToastKind::Success, ui::time::now()));
    }

    /// Errors must read as errors — a green success toast after a rejected
    /// restore is actively misleading and can make recovery unsafe.
    fn toast_error(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ToastKind::Error, ui::time::now()));
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
            PickPurpose::Source => ("Choose a folder to protect", "Protect folder"),
            PickPurpose::Destination => ("Choose a backup location", "Use this location"),
            PickPurpose::Restore => ("Restore into an empty folder", "Restore here"),
        };
        self.picker = Some((
            purpose,
            ui::portal::PickRequest::directory(title, accept)
                .thread_name("backups-folder-picker")
                .send(),
        ));
    }

    fn start_backup(&mut self) {
        if let Some(error) = self.repository_error.clone() {
            self.toast_error(format!(
                "{error} Choose another backup location to continue."
            ));
            return;
        }
        let (Some(source), Some(destination)) =
            (self.source_path.clone(), self.destination_path.clone())
        else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.job = Some(rx);
        self.job_status = Some("Preparing files…".into());
        if let Err(error) = std::thread::Builder::new()
            .name("backups-create".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                match crate::engine::create_snapshot(&source, &destination, |progress| {
                    let _ = progress_tx.send(JobEvent::Progress(progress.clone()));
                }) {
                    Ok(manifest) => {
                        let _ = tx.send(JobEvent::Created(manifest));
                    }
                    Err(error) => {
                        let _ = tx.send(JobEvent::Failed(error.to_string()));
                    }
                }
            })
        {
            self.job = None;
            self.job_status = None;
            self.toast_error(format!("Couldn't start the backup worker: {error}"));
        }
    }

    fn start_verify(&mut self) {
        if let Some(error) = self.repository_error.clone() {
            self.toast_error(format!(
                "{error} Choose another backup location to continue."
            ));
            return;
        }
        let (Some(destination), Some(id)) =
            (self.destination_path.clone(), self.ui.sel_snapshot.clone())
        else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.job = Some(rx);
        self.job_status = Some("Checking every file…".into());
        if let Err(error) = std::thread::Builder::new()
            .name("backups-verify".into())
            .spawn(
                move || match crate::engine::verify_snapshot(&destination, &id) {
                    Ok(result) => {
                        let _ = tx.send(JobEvent::Verified(result));
                    }
                    Err(error) => {
                        let _ = tx.send(JobEvent::Failed(error.to_string()));
                    }
                },
            )
        {
            self.job = None;
            self.job_status = None;
            self.toast_error(format!("Couldn't start the verification worker: {error}"));
        }
    }

    fn start_restore(&mut self, restore_to: PathBuf) {
        if let Some(error) = self.repository_error.clone() {
            self.toast_error(format!(
                "{error} Choose another backup location to continue."
            ));
            return;
        }
        let (Some(destination), Some(id)) =
            (self.destination_path.clone(), self.ui.sel_snapshot.clone())
        else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.job = Some(rx);
        self.job_status = Some("Restoring and checking files…".into());
        if let Err(error) = std::thread::Builder::new()
            .name("backups-restore".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                match crate::engine::restore_snapshot(
                    &destination,
                    &id,
                    &restore_to,
                    false,
                    |progress| {
                        let _ = progress_tx.send(JobEvent::Progress(progress.clone()));
                    },
                ) {
                    Ok(result) => {
                        let _ = tx.send(JobEvent::Restored(result));
                    }
                    Err(error) => {
                        let _ = tx.send(JobEvent::Failed(error.to_string()));
                    }
                }
            })
        {
            self.job = None;
            self.job_status = None;
            self.toast_error(format!("Couldn't start the recovery worker: {error}"));
        }
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
                            "The folder chooser closed unexpectedly".into(),
                        ),
                    )),
                });
        if let Some((purpose, outcome)) = portal_result {
            self.picker = None;
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
                                Err(error) => self.toast_error(error.to_string()),
                            }
                        }
                    }
                    PickPurpose::Restore => {
                        // The sheet promises a destination chooser, not a
                        // background restore. Dismiss it only after the user
                        // explicitly chose a folder; `start_restore` still
                        // calls the engine with `overwrite = false`.
                        self.ui.restore_open = false;
                        self.start_restore(path);
                    }
                },
                ui::portal::PortalOutcome::Cancelled => {}
                ui::portal::PortalOutcome::Failed(error) => self.toast_error(error),
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
                        "The backup worker stopped unexpectedly".into(),
                    )),
                });
            let Some(event) = event else {
                break;
            };
            match event {
                JobEvent::Progress(progress) => {
                    self.job_status = Some(format!(
                        "{} files · {}",
                        progress.files,
                        ui::format::bytes(progress.bytes)
                    ));
                }
                JobEvent::Created(manifest) => {
                    self.job = None;
                    self.job_status = None;
                    if let (Some(source), Some(destination)) =
                        (&self.source_path, &self.destination_path)
                    {
                        match crate::engine::list_snapshots(destination) {
                            Ok(manifests) => {
                                self.catalog = Catalog::from_local(source, destination, &manifests);
                                self.sync_search_index();
                            }
                            Err(error) => {
                                self.repository_error = Some(format!(
                                    "Backup finished, but its repository could not be reloaded: {error}"
                                ));
                                self.toast_error(
                                    "Backup finished, but its history could not be reopened. Choose the location again before another operation.",
                                );
                                break;
                            }
                        }
                    }
                    self.toast_now(format!(
                        "Backup complete · {} files · {}",
                        manifest.files,
                        ui::format::bytes(manifest.bytes)
                    ));
                    break;
                }
                JobEvent::Verified(result) => {
                    self.job = None;
                    self.job_status = None;
                    self.toast_now(format!(
                        "Verified {} files · {}",
                        result.files,
                        ui::format::bytes(result.bytes)
                    ));
                    break;
                }
                JobEvent::Restored(result) => {
                    self.job = None;
                    self.job_status = None;
                    self.toast_now(format!(
                        "Restored {} files · {}",
                        result.files,
                        ui::format::bytes(result.bytes)
                    ));
                    break;
                }
                JobEvent::Failed(error) => {
                    self.job = None;
                    self.job_status = None;
                    self.toast_error(error);
                    break;
                }
            }
        }
    }

    // ---- semantic colour mapping (all sourced from the Theme) -----------

    fn retention_color(theme: &Theme, r: Retention) -> Color {
        let c = &theme.tokens.colors;
        match r {
            Retention::Hourly => c.semantic.info,
            Retention::Daily => theme.accent_primary,
            Retention::Weekly => c.semantic.success,
            Retention::Monthly => c.semantic.warning,
            Retention::Keep => c.accent[8],
        }
    }

    fn health_color(theme: &Theme, h: Health) -> Color {
        let c = &theme.tokens.colors;
        match h {
            Health::Ok => c.semantic.success,
            Health::Warning => c.semantic.warning,
            Health::Error => c.semantic.danger,
        }
    }

    fn status_color(theme: &Theme, s: SnapStatus) -> Color {
        let c = &theme.tokens.colors;
        match s {
            SnapStatus::Complete => c.semantic.success,
            SnapStatus::Verifying => c.semantic.info,
            SnapStatus::Failed => c.semantic.danger,
        }
    }

    // ---- top-level draw -------------------------------------------------

    /// Fresh install: nothing configured yet, show onboarding not a dashboard.
    fn is_empty(&self) -> bool {
        self.catalog.sources.is_empty() && self.catalog.destinations.is_empty()
    }

    /// Text scale the renderer is painting at; every breakpoint and fixed
    /// band is scaled by it.
    fn text_scale(tr: &TextRenderer) -> f64 {
        f64::from(tr.text_scale())
    }

    /// Measure the top-bar chrome: the tab height carries its 13.5px label
    /// plus the pill inset, the bar carries the tabs. Exactly `TAB_H` /
    /// `BAR_H` at 1×.
    fn chrome_metrics(&mut self, tr: &mut TextRenderer) {
        let (_, label_h) = tr.measure("Hg", 13.5, None);
        self.tab_h = TAB_H.max(f64::from(label_h) + 12.0);
        self.bar_h = BAR_H.max(self.tab_h + 18.0);
    }

    fn render(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.hits.clear();
        let c = &theme.tokens.colors;

        // Window background.
        paint::fill_rect(scene, t, self.rect, c.surfaces.base);

        // Bands and breakpoints follow the text scale before anything is
        // placed off them.
        self.chrome_metrics(tr);
        self.drawer
            .set_breakpoint(RAIL_BREAKPOINT * Self::text_scale(tr));

        // Reserve the transient status bubble's footprint so it never
        // covers a dashboard control when a job finishes.
        // The dashboard starts a section step (`S4`) under the bar, or under
        // the progress banner when one is shown (`S2` under the bar, 34px).
        let progress_h = if self.job_status.is_some() {
            42.0 * Self::text_scale(tr)
        } else {
            0.0
        };
        let content = Rect::new(
            self.rect.x0 + PAD,
            self.rect.y0 + self.bar_h + S4 + progress_h,
            self.rect.x1 - PAD,
            self.rect.y1 - S3 - STATUS_SAFE_BOTTOM,
        );

        // Advance + resolve the Timeline source rail. Ticking eases the reveal
        // off a real wall-clock delta; resolving re-decides docked-vs-overlay
        // from the width so the top bar's hamburger reflects the live state.
        self.drawer.tick(false);
        let _ = self.drawer.resolve(content);
        // `--shots`: force the overlay drawer open *after* the first resolve
        // (which settles the eased reveal to the width default), then re-resolve.
        if self.force_rail_open && self.drawer.is_overlay() && !self.drawer.is_open() {
            self.drawer.open();
            self.drawer.snap(true);
            let _ = self.drawer.resolve(content);
        }

        self.draw_top_bar(scene, t, tr, theme);
        self.draw_job_progress(scene, t, tr, theme);

        if self.is_empty() {
            // Fresh install: nothing to back up yet. Show onboarding instead of
            // an empty dashboard so the app reads as ready, not broken.
            self.draw_empty_onboarding(scene, t, tr, theme, content);
        } else {
            match self.ui.view {
                View::Timeline => self.draw_timeline_view(scene, t, tr, theme, content),
                View::Sources => self.draw_sources_view(scene, t, tr, theme, content),
                View::Destinations => self.draw_destinations_view(scene, t, tr, theme, content),
                View::Schedules => self.draw_schedules_view(scene, t, tr, theme, content),
            }
        }

        // The Timeline source rail draws last so an overlay drawer floats over
        // the snapshot list (with a dismiss scrim); docked, it simply sits beside.
        if !self.is_empty() && self.ui.view == View::Timeline {
            let side = self.drawer.sidebar_rect();
            let scrim = self.drawer.scrim_rect();
            if scrim.width() > 0.5 {
                self.drawer.draw_scrim(scene, t);
                self.hits.push((scrim, Hit::CloseRail));
            }
            if should_paint_rail(&self.drawer, side) {
                self.draw_rail(scene, t, tr, theme, side);
            }
        }

        // The phone-width primary action is a compact plus glyph. Keep its
        // full purpose discoverable for pointer users as well as in the
        // semantic tree below.
        self.draw_compact_backup_tooltip(scene, t, tr, theme);
        self.draw_compact_tab_tooltip(scene, t, tr, theme);

        // The transient status pill floats over the bottom-right, beneath the
        // restore modal + toast so those still overlay it.
        self.draw_status_bubble(scene, t, tr, theme);

        // The confirmation sheet is deliberately above the dashboard and its
        // controls. It is the last non-transient surface before notifications.
        // While it is open only its own controls exist for hit-testing and
        // accessibility: nothing behind the scrim is exported, so a reader
        // never lands on a row the scrim would swallow (`hit_at` agrees).
        if self.ui.restore_open {
            self.hits.clear();
            self.draw_restore_modal(scene, t, tr, theme);
        }

        self.draw_toast(scene, t, tr, theme);
    }

    fn draw_job_progress(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(status) = self.job_status.as_deref() else {
            return;
        };
        let c = &theme.tokens.colors;
        let scale = Self::text_scale(tr);
        let rect = Rect::new(
            self.rect.x0 + PAD,
            self.rect.y0 + self.bar_h + S2,
            self.rect.x1 - PAD,
            self.rect.y0 + self.bar_h + 42.0 * scale,
        );
        paint::card(scene, t, rect, 8.0, c.surfaces.raised);
        paint::text(
            scene,
            tr,
            t,
            "Backup in progress",
            12.0,
            c.text_primary,
            rect.x0 + 12.0 * scale,
            rect.y0 + 10.0 * scale,
        );
        paint::text(
            scene,
            tr,
            t,
            status,
            12.0,
            c.text_dim,
            rect.x0 + 132.0 * scale,
            rect.y0 + 10.0 * scale,
        );
        let track = Rect::new(rect.x0 + S3, rect.y1 - 7.0, rect.x1 - S3, rect.y1 - 4.0);
        paint::fill_round(scene, t, track, 2.0, c.surfaces.base);
        // Indeterminate progress is honest: traversal totals are not known before
        // walking the tree, so this communicates work without inventing a percent.
        paint::fill_round(
            scene,
            t,
            Rect::new(
                track.x0,
                track.y0,
                track.x0 + track.width() * 0.38,
                track.y1,
            ),
            2.0,
            theme.accent_primary,
        );
    }

    fn draw_top_bar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.bar_h,
        );
        // Bar role: the raised tier with one derived hairline under it.
        paint::fill_rect(scene, t, bar, c.surfaces.raised);
        paint::hline(
            scene,
            t,
            bar.x0,
            bar.x1,
            bar.y1 - 0.5,
            chrome_hairline(theme),
            1.0,
        );

        let cy = bar.y0 + self.bar_h / 2.0;
        let narrow = self.rect.width() < NARROW_BREAKPOINT * Self::text_scale(tr);

        let brand_right = self.draw_top_bar_left_cluster(scene, tr, theme, t, bar, cy, narrow);
        // The primary action compacts to an icon below the narrow breakpoint
        // (as the breakpoint contract promises); with the full label the tab
        // strip's minimum width overran the band and the last tab painted
        // underneath the button at phone widths. The wide face is sized from
        // its label so it never clips at a larger text scale.
        let action_w = if self.is_empty() {
            0.0
        } else if narrow {
            self.tab_h
        } else {
            let (bw, _) = tr.measure("Backing up…", 14.0, None);
            114.0_f64.max(f64::from(bw) + 28.0)
        };
        let action_gap = if self.is_empty() { 0.0 } else { 8.0 };
        self.draw_top_bar_tabs(
            scene,
            tr,
            theme,
            t,
            cy,
            brand_right,
            bar.x1 - PAD - action_w - action_gap,
        );
        if !self.is_empty() {
            let rect = Rect::new(bar.x1 - PAD - action_w, cy - 17.0, bar.x1 - PAD, cy + 17.0);
            let busy = self.job.is_some() || self.job_status.is_some();
            let label = if busy { "Backing up…" } else { "Back up now" };
            let face = if narrow {
                ButtonFace::new(rect, "")
                    .icon(Icon::Archive)
                    .accessible_name(label)
            } else {
                ButtonFace::new(rect, label)
            };
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &face
                    .primary()
                    .radius(9.0)
                    .disabled(busy)
                    .hovered(self.hover == Some(Hit::BackupNow)),
            );
            if !busy {
                self.hits.push((rect, Hit::BackupNow));
            }
        }
    }

    /// The top bar's left cluster: the Timeline-rail hamburger (when
    /// relevant) and the "N sources · M destinations" tally label. Returns
    /// the x just past the label, so the tab strip can centre in the
    /// remaining gap. Split out of [`Self::draw_top_bar`] to keep it under
    /// the line budget.
    fn draw_top_bar_left_cluster(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        bar: Rect,
        cy: f64,
        narrow: bool,
    ) -> f64 {
        let c = &theme.tokens.colors;
        // A hamburger leads it whenever the Timeline rail isn't fully
        // docked-open (overlay, or a collapsed dock), so the source rail is
        // always reachable.
        let mut lx = bar.x0 + PAD;
        let show_hamburger = self.ui.view == View::Timeline
            && !self.is_empty()
            && !(self.drawer.is_docked() && self.drawer.is_open());
        if show_hamburger {
            let hb = Rect::new(lx, cy - 17.0, lx + 34.0, cy + 17.0);
            let hov = hb.contains(self.last_mouse);
            self.drawer.draw_toggle(scene, t, theme, hb, hov);
            self.hits.push((hb, Hit::ToggleRail));
            lx = hb.x1 + 12.0;
        }
        // Left label: the contextual source/destination tally. The compositor
        // owns window chrome, so the app draws no wordmark or logo here (per the
        // HIG) — this reads as the dashboard's scope, not a brand. Dropped at
        // narrow so the tabs + primary action fit.
        if !narrow {
            let sub = format!(
                "{} sources · {} destinations",
                self.catalog.sources.len(),
                self.catalog.destinations.len()
            );
            paint::text(scene, tr, t, &sub, 13.0, c.text_dim, lx, cy - 7.0);
            let (tw, _) = tr.measure(&sub, 13.0, None);
            lx += f64::from(tw);
        }
        lx
    }

    /// The view segmented-tab control, centred between the left cluster and
    /// the backup button, with per-tab width clamped so all tabs always fit.
    /// Split out of [`Self::draw_top_bar`] to keep it under the line budget.
    fn draw_top_bar_tabs(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        cy: f64,
        brand_right: f64,
        btn_x0: f64,
    ) {
        let c = &theme.tokens.colors;
        // Segmented tab control, centred in the gap between the brand and the
        // button, with the per-tab width clamped so four tabs always fit.
        let band_x0 = brand_right + 14.0;
        let band_x1 = btn_x0 - 14.0;
        // Retention stays reachable after configuring a live repository.
        let views = &View::ALL;
        let n = views.len() as f64;
        // The band decides the tab width: never wider than `TAB_W`, never
        // below a usable target, and never a fixed floor that could overrun
        // the band into the primary action.
        let tab_w = ((band_x1 - band_x0) / n).clamp(MIN_TAB_W, TAB_W);
        let seg_total = tab_w * n;
        let seg_x0 = (band_x0 + ((band_x1 - band_x0) - seg_total) / 2.0).max(band_x0);
        let seg = Rect::new(
            seg_x0,
            cy - self.tab_h / 2.0,
            seg_x0 + seg_total,
            cy + self.tab_h / 2.0,
        );
        // The track is a well in the raised bar (window tier + derived edge)
        // so the shared pills lift off it on both palettes.
        paint::card(scene, t, seg, 9.0, c.surfaces.base);
        // The active tab floats as an inset pill inside the raised track; the
        // whole strip is one shared segmented control. Abbreviate the labels
        // once the tabs get too tight so "Destinations"/"Schedules" don't
        // collide with their neighbours — the abbreviation is content the app
        // owns; the pill fill + selection palette come from the shared control.
        let narrow_tabs = tab_w < 96.0;
        let fs = if narrow_tabs { 12.0_f32 } else { 13.5 };
        // A label that still does not fit its tab (340px windows, or a
        // larger text scale) elides against the pill's inner width rather
        // than clipping mid-glyph against its neighbour.
        let label_max_w = (tab_w - 8.0 - 8.0).max(8.0) as f32;
        let mut pill_rects: Vec<Rect> = Vec::with_capacity(views.len());
        let mut labels: Vec<String> = Vec::with_capacity(views.len());
        for (i, v) in views.iter().enumerate() {
            let tab = Rect::new(
                seg.x0 + i as f64 * tab_w,
                seg.y0,
                seg.x0 + (i as f64 + 1.0) * tab_w,
                seg.y1,
            );
            pill_rects.push(tab.inset(-TAB_PILL_INSET));
            let label = if narrow_tabs {
                match v {
                    View::Timeline => "Timeline",
                    View::Sources => "Sources",
                    View::Destinations => "Dests",
                    View::Schedules => "Policy",
                }
            } else {
                v.title()
            };
            labels.push(tr.elide_to_width(label, fs, label_max_w));
            self.hits.push((tab, Hit::Tab(*v)));
        }
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected = views.iter().position(|v| self.ui.view == *v).unwrap_or(0);
        let hovered = views.iter().position(|v| self.hover == Some(Hit::Tab(*v)));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&pill_rects, &labels, selected)
                .pill()
                .hovered(hovered)
                .radius(7.0)
                .text_size(fs),
        );
    }

    /// Health signal (kept as text so the pill needs no bespoke tint) followed
    /// by the snapshot tally and protected size.
    fn status_label(&self) -> String {
        let failed = self
            .catalog
            .snapshots
            .iter()
            .filter(|s| s.status == SnapStatus::Failed)
            .count();
        let health = if failed > 0 {
            format!("{failed} backup needs attention")
        } else {
            "All backups healthy".to_string()
        };
        format!(
            "{health}  ·  {} snapshots · {} protected",
            self.catalog.total_snapshots(),
            ui::format::bytes(self.catalog.total_protected()),
        )
    }

    /// Surfaces on a status change (a completed job shifts the tally/health) or
    /// while the pointer hovers the bottom-right corner, then fades.
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let label = self.status_label();
        // Poke only on a genuine change — draw runs every frame, so an
        // unconditional poke would pin the bubble open forever.
        if label != self.status_shown {
            self.status_bubble.poke(now);
            self.status_shown.clone_from(&label);
        }
        let text_size = 12.0f32;
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 14.0;
        let rect = Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        // Reveal zone: the bottom-right corner band (plus the pill), so nudging
        // the pointer there re-surfaces the status on demand.
        let zone = Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
        let over = self.shots_status || zone.contains(self.last_mouse);
        self.status_bubble.set_hover(over, now);
        let alpha = self.status_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            t,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, t, tr, theme, rect, &label, text_size);
        scene.pop_layer();
    }

    /// `--shots`: force the status bubble fully visible so a single headless
    /// frame captures the floating pill.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(ui::time::now());
    }

    /// Stage deterministic non-destructive states for screenshot coverage.
    pub fn stage_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "progress" => self.job_status = Some("642 files · 3.8 GB verified so far".into()),
            "failure" => self.toast_error(
                "Backup paused: the destination is unavailable. Reconnect it, then retry.",
            ),
            "restore" => self.shot_scenario = Some(ShotScenario::Restore),
            // The other dashboard views, so every primary screen is capturable.
            "sources" => self.ui.view = View::Sources,
            "destinations" => self.ui.view = View::Destinations,
            "schedules" => self.ui.view = View::Schedules,
            "unavailable" => {
                self.catalog = Catalog::empty();
                self.sync_search_index();
                self.source_path = Some(PathBuf::from("/home/you/Documents"));
                self.destination_path = Some(PathBuf::from("/media/backup-drive"));
                self.repository_error = Some(
                    "Couldn’t read the saved backup location: the drive is unavailable.".into(),
                );
            }
            _ => {}
        }
    }

    // ---- EMPTY onboarding -----------------------------------------------

    /// Renders the persisted recovery state for an unreadable saved
    /// repository. Returns `true` when the unavailable state owned the empty
    /// surface, so first-run onboarding is never rendered underneath it.
    fn draw_unavailable_repository_notice(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) -> bool {
        let c = &theme.tokens.colors;
        let Some(error) = self.repository_error.as_deref() else {
            return false;
        };
        // A saved configuration is not equivalent to a usable repository.
        // Make the distinction persistent and actionable rather than falling
        // through to first-run copy or a transient error toast.
        let scale = f64::from(theme.text_scale);
        let card_w = content.width().min(620.0 * scale);
        let inner_w = (card_w - 48.0 * scale).max(0.0);
        let detail_lines = wrap_recovery_copy(error, tr, 12.5, inner_w);
        let safety_lines = wrap_recovery_copy(RECOVERY_SAFETY_COPY, tr, 11.5, inner_w);
        let detail_y = 54.0 * scale;
        let detail_line_h = 17.0 * scale;
        let safety_y = detail_y + detail_lines.len() as f64 * detail_line_h + 7.0 * scale;
        let safety_line_h = 16.0 * scale;
        let button_y = safety_y + safety_lines.len() as f64 * safety_line_h + 18.0 * scale;
        // Keep the recovery target separate from all of the complete
        // explanatory copy instead of compressing it into one line.
        let card_h = button_y + 60.0 * scale;
        let card = Rect::new(
            content.center().x - card_w / 2.0,
            content.center().y - card_h / 2.0,
            content.center().x + card_w / 2.0,
            content.center().y + card_h / 2.0,
        );
        paint::card(scene, t, card, 16.0, c.surfaces.raised);
        paint::stroke_round(
            scene,
            t,
            card,
            16.0,
            c.semantic.danger.with_alpha_factor(0.8),
            1.0,
        );
        let title = if scale >= 1.5 {
            "Location unavailable"
        } else {
            "Backup location needs attention"
        };
        paint::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            title,
            17.0,
            c.semantic.danger,
            Point::new(card.center().x, card.y0 + 22.0 * scale),
        );
        for (index, line) in detail_lines.iter().enumerate() {
            paint::text_center(
                &mut PaintCtx::new(scene, t, tr, theme),
                line,
                12.5,
                c.text_dim,
                Point::new(
                    card.center().x,
                    card.y0 + detail_y + index as f64 * detail_line_h,
                ),
            );
        }
        for (index, line) in safety_lines.iter().enumerate() {
            paint::text_center(
                &mut PaintCtx::new(scene, t, tr, theme),
                line,
                11.5,
                c.text_dim,
                Point::new(
                    card.center().x,
                    card.y0 + safety_y + index as f64 * safety_line_h,
                ),
            );
        }
        let button_w = (252.0 * scale).min(card.width() - 40.0 * scale);
        let button = Rect::new(
            card.center().x - button_w / 2.0,
            card.y0 + button_y,
            card.center().x + button_w / 2.0,
            card.y0 + button_y + 42.0 * scale,
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(button, "Choose another location")
                .primary()
                .radius(9.0)
                .hovered(self.hover == Some(Hit::Setup)),
        );
        self.hits.push((button, Hit::Setup));
        true
    }

    /// First-run state: no sources or destinations configured. Renders a
    /// centered prompt so the fresh app reads as ready rather than as a broken
    /// blank dashboard.
    fn draw_empty_onboarding(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        if self.draw_unavailable_repository_notice(scene, t, tr, theme, content) {
            return;
        }
        let c = &theme.tokens.colors;
        let cy = content.y0 + content.height() * 0.36;
        let title = if self.job.is_some() {
            "Creating a verified backup"
        } else if self.source_path.is_some() {
            "Now choose where to keep it"
        } else {
            "Protect your important files"
        };
        paint::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            title,
            19.0,
            c.text_primary,
            Point::new(content.center().x, cy),
        );
        let detail = self.job_status.as_deref().unwrap_or_else(|| {
            if self.source_path.is_some() {
                "Pick another drive or a folder outside the source."
            } else {
                "Choose a folder. Backups will be checksummed and restorable."
            }
        });
        paint::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            detail,
            13.0,
            c.text_dim,
            Point::new(content.center().x, cy + 30.0),
        );
        if self.job.is_none() && self.picker.is_none() {
            let label = if self.source_path.is_some() {
                "Choose backup location"
            } else {
                "Choose folder to protect"
            };
            let button = Rect::new(
                content.center().x - 110.0,
                cy + 70.0,
                content.center().x + 110.0,
                cy + 112.0,
            );
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(button, label)
                    .primary()
                    .radius(9.0)
                    .hovered(self.hover == Some(Hit::Setup)),
            );
            self.hits.push((button, Hit::Setup));
        }
    }

    // ---- TIMELINE view --------------------------------------------------

    fn draw_timeline_view(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        // Narrow windows need recovery cards; a wide timeline still has room
        // for the measured columns, whose rows grow with the renderer's type.
        if theme.text_scale >= 1.5 && content.width() < NARROW_BREAKPOINT {
            self.draw_large_type_timeline(scene, t, tr, theme, content);
            return;
        }
        // Main column: the drawer's resolved content rect (inset by the docked
        // rail, or full width in overlay). A small gap keeps it off a docked rail.
        let docked_gap = if self.drawer.is_docked() { 18.0 } else { 0.0 };
        let drawer_rect = self.drawer.content_rect();
        let main = Rect::new(
            drawer_rect.x0 + docked_gap,
            content.y0,
            content.x1,
            content.y1,
        );

        // Summary stat tiles — reflow from four abreast down to a 2×2 grid so
        // they never crush at narrow widths; the minimum tile width and the
        // tile band follow the text scale (label line + value line, 82px at 1×).
        let cols = card_columns(main.width(), 156.0 * Self::text_scale(tr), 12.0).clamp(1, 4);
        let rows = 4usize.div_ceil(cols);
        let (_, label_h) = tr.measure("Hg", 11.5, None);
        let (_, value_h) = tr.measure_weighted("Hg", 22.0, None, TextWeight::Bold);
        // `PAD` above the label, `S2` to the value, `PAD` below it.
        let tile_h = 82.0_f64.max(PAD + f64::from(label_h) + S2 + f64::from(value_h) + PAD);
        let tile_block_h = rows as f64 * tile_h + (rows as f64 - 1.0) * S3;
        let tiles = Rect::new(main.x0, main.y0, main.x1, main.y0 + tile_block_h);
        self.draw_summary_tiles(scene, t, tr, theme, tiles, cols);

        // Timeline header: the next group, `S6` under the tile block.
        let c = &theme.tokens.colors;
        let hy = tiles.y1 + S6;
        let scope = match &self.ui.sel_source {
            Some(id) => self
                .catalog
                .source(id)
                .map(|s| s.path.clone())
                .unwrap_or_default(),
            None => "All sources".to_string(),
        };
        paint::text_bold(
            scene,
            tr,
            t,
            "Snapshot timeline",
            15.0,
            c.text_primary,
            main.x0,
            hy,
        );
        // Measure the title at the weight it is painted with; a regular-weight
        // measurement under-reads the bold run and the scope crowded it. The
        // scope and sort note then share the title's measured centre line.
        let (title_width, title_height) =
            tr.measure_weighted("Snapshot timeline", 15.0, None, TextWeight::Bold);
        let title_height = f64::from(title_height);
        paint::text_vc(
            &mut PaintCtx::new(scene, t, tr, theme),
            &scope,
            12.5,
            theme.accent_primary,
            Rect::new(
                main.x0 + f64::from(title_width) + 14.0,
                hy,
                main.x1,
                hy + title_height,
            ),
        );
        let (sort_w, sort_h) = tr.measure("newest first", 11.5, None);
        paint::text(
            scene,
            tr,
            t,
            "newest first",
            11.5,
            c.text_dim,
            main.x1 - f64::from(sort_w),
            hy + (title_height - f64::from(sort_h)) / 2.0,
        );

        // The list starts `S2` under the title's measured line.
        let list = Rect::new(main.x0, hy + title_height + S2, main.x1, main.y1);
        self.draw_timeline_list(scene, t, tr, theme, list);
    }

    /// A dense, multi-column timeline cannot remain legible at 200% text on a
    /// narrow window. Reflow it into a scrollable, single-column recovery
    /// list: every version remains an independently selectable target for
    /// pointer, touch, keyboard, and assistive-technology users.
    fn draw_large_type_timeline(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        paint::text_bold(
            scene,
            tr,
            t,
            "Recovery points",
            17.0,
            c.text_primary,
            content.x0,
            content.y0,
        );
        paint::text(
            scene,
            tr,
            t,
            &self.large_type_health_label(),
            12.0,
            c.text_dim,
            content.x0,
            content.y0 + 30.0,
        );
        self.draw_large_type_snapshot_actions(scene, t, tr, theme, content);
        let list = Rect::new(content.x0, content.y0 + 182.0, content.x1, content.y1);
        self.draw_large_type_snapshot_list(scene, t, tr, theme, list);
    }

    fn large_type_health_label(&self) -> String {
        let failed = self
            .catalog
            .snapshots
            .iter()
            .filter(|snapshot| snapshot.status == SnapStatus::Failed)
            .count();
        if failed == 0 {
            "All backups healthy".into()
        } else {
            format!("{failed} backup needs attention")
        }
    }

    fn draw_large_type_snapshot_actions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        if self.ui.sel_snapshot.is_none() {
            paint::text(
                scene,
                tr,
                t,
                "Select a recovery point to verify or restore.",
                12.0,
                c.text_dim,
                content.x0,
                content.y0 + 96.0,
            );
            return;
        }
        paint::text_bold(
            scene,
            tr,
            t,
            "Selected recovery point",
            12.0,
            c.text_primary,
            content.x0,
            content.y0 + 64.0,
        );
        let verify = Rect::new(
            content.x0,
            content.y0 + 116.0,
            content.x0 + (content.width() - 10.0) / 2.0,
            content.y0 + 168.0,
        );
        let restore = Rect::new(verify.x1 + 10.0, verify.y0, content.x1, verify.y1);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(verify, "Verify")
                .radius(8.0)
                .hovered(self.hover == Some(Hit::Verify)),
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(restore, "Restore…")
                .primary()
                .radius(8.0)
                .hovered(self.hover == Some(Hit::RestoreOpen)),
        );
        self.hits.push((verify, Hit::Verify));
        self.hits.push((restore, Hit::RestoreOpen));
    }

    fn draw_large_type_snapshot_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
    ) {
        const CARD_H: f64 = 82.0;
        const GAP: f64 = 8.0;
        let c = &theme.tokens.colors;
        let snaps: Vec<Snapshot> = self
            .catalog
            .timeline(self.ui.sel_source.as_deref())
            .into_iter()
            .cloned()
            .collect();
        if snaps.is_empty() {
            self.timeline_content_h = 0.0;
            self.timeline_scroll = 0.0;
            Self::draw_timeline_empty_state(scene, t, tr, theme, list);
            return;
        }
        self.timeline_view = list;
        self.timeline_content_h = snaps.len() as f64 * (CARD_H + GAP) - GAP;
        let max_scroll = (self.timeline_content_h - list.height()).max(0.0);
        self.timeline_scroll = self.timeline_scroll.clamp(0.0, max_scroll);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &list);
        let mut y = list.y0 - self.timeline_scroll;
        for snapshot in &snaps {
            let card = Rect::new(list.x0, y, list.x1, y + CARD_H);
            if card.y1 > list.y0 && card.y0 < list.y1 {
                self.draw_large_type_snapshot_row(scene, t, tr, theme, card, snapshot);
                self.hits.push((card, Hit::Snap(snapshot.id.clone())));
            }
            y += CARD_H + GAP;
        }
        scene.pop_layer();
        if max_scroll > 0.0 {
            // At high text scale the list's continuation is part of the
            // workflow, not a hidden implementation detail. Keep a thicker,
            // persistent thumb visible rather than relying on a hover-only
            // affordance or clipped final card to imply scrollability.
            let track_h = list.height();
            let thumb_h = (track_h * (track_h / self.timeline_content_h)).max(36.0);
            let thumb_y = list.y0 + (track_h - thumb_h) * (self.timeline_scroll / max_scroll);
            let thumb = Rect::new(list.x1 - 7.0, thumb_y, list.x1 - 2.0, thumb_y + thumb_h);
            paint::fill_round(scene, t, thumb, 2.5, c.text_dim.with_alpha_factor(0.72));
        }
    }

    fn draw_large_type_snapshot_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        snapshot: &Snapshot,
    ) {
        let c = &theme.tokens.colors;
        let selected = self.ui.sel_snapshot.as_deref() == Some(snapshot.id.as_str());
        let background = if selected {
            theme.accent_primary.with_alpha_factor(0.16)
        } else if self.hover == Some(Hit::Snap(snapshot.id.clone())) {
            c.lift(0.35)
        } else {
            c.surfaces.raised
        };
        paint::card(scene, t, card, 12.0, background);
        paint::stroke_round(
            scene,
            t,
            card,
            12.0,
            if selected {
                theme.accent_primary.with_alpha_factor(0.7)
            } else {
                ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26)
            },
            1.0,
        );
        paint::dot(
            scene,
            t,
            Point::new(card.x0 + 16.0, card.y0 + 28.0),
            4.0,
            Self::status_color(theme, snapshot.status),
        );
        paint::text_bold(
            scene,
            tr,
            t,
            &format!("{} · {}", snapshot.time, snapshot.kind.label()),
            14.0,
            c.text_primary,
            card.x0 + 30.0,
            card.y0 + 16.0,
        );
        paint::text(
            scene,
            tr,
            t,
            &format!(
                "{} · {} files · {}",
                ui::format::bytes(snapshot.size_bytes),
                snapshot.files_changed,
                snapshot.retention.label()
            ),
            11.5,
            c.text_dim,
            card.x0 + 16.0,
            card.y0 + 50.0,
        );
    }

    fn draw_rail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
    ) {
        let c = &theme.tokens.colors;
        let ground = self.rail_ground(theme);
        // Clip every rail pixel so a sliding overlay reveal never spills rows
        // over the snapshot list beside it.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rail);
        // A raised card when docked, a Panel-role surface when the drawer
        // floats over content in overlay mode; either way the edge derives
        // from the fill it sits on.
        if self.drawer.is_overlay() {
            paint::fill_round(scene, t, rail, 14.0, ground);
            paint::stroke_round(scene, t, rail.inset(-0.5), 13.5, panel_border(theme), 1.0);
        } else {
            paint::card(scene, t, rail, 14.0, ground);
        }
        let rows_top = self.draw_rail_header(scene, t, tr, theme, rail);

        // The rows scroll under the header band when the catalog is taller
        // than the rail; their clip and their hit rects stop at the band.
        let rows_clip = Rect::new(rail.x0, rows_top, rail.x1, rail.y1);
        self.rail_scroll = self.rail_scroll.clamp(0.0, self.rail_scroll_max);
        let first_row_hit = self.hits.len();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rows_clip);
        let mut y = rows_top - self.rail_scroll;

        // "All sources" pseudo-row + each source.
        let all_sel = self.ui.sel_source.is_none();
        self.draw_source_row(
            &mut PaintCtx::new(scene, t, tr, theme),
            rail,
            &mut y,
            all_sel,
        );
        for i in 0..self.catalog.sources.len() {
            let Some(src) = self.catalog.sources.get(i) else {
                continue;
            };
            let sel = self.ui.sel_source.as_deref() == Some(src.id.as_str());
            let src = src.clone();
            self.draw_source_row_real(
                &mut PaintCtx::new(scene, t, tr, theme),
                rail,
                &mut y,
                &src,
                sel,
            );
        }

        // Next group: a derived separator inset `S3` (HIG §9.5) against the
        // rail's own ground, `S4` above and below, then the caption and its
        // rows `S2` apart.
        y += S4 - S2;
        paint::separator(scene, t, rail.x0, rail.x1, y, S3, ground);
        y += S4;
        let (_, cap_h) = tr.measure("DESTINATIONS", 10.5, None);
        paint::text(
            scene,
            tr,
            t,
            "DESTINATIONS",
            10.5,
            c.text_dim,
            rail.x0 + PAD,
            y,
        );
        y += f64::from(cap_h) + S2;
        for i in 0..self.catalog.destinations.len() {
            let Some(d) = self.catalog.destinations.get(i).cloned() else {
                continue;
            };
            self.draw_dest_compact(scene, t, tr, theme, rail, &mut y, &d);
        }
        scene.pop_layer();
        self.finish_rail_rows(scene, t, theme, rows_clip, y, first_row_hit);
        scene.pop_layer();
    }

    /// What the rail's rows sit on: the Panel ground while the drawer floats
    /// over content, the raised card when docked. Every rail separator, hover
    /// and selected face derives against this, never against `raised` blindly.
    fn rail_ground(&self, theme: &Theme) -> Color {
        if self.drawer.is_overlay() {
            panel_ground(theme)
        } else {
            theme.tokens.colors.surfaces.raised
        }
    }

    /// The rail's header band — the caption and, while the rail is
    /// docked-open, the collapse toggle on one centre line (the top-bar
    /// hamburger owns the overlay case, so the 30px toggle band is only
    /// reserved when the toggle is drawn). Returns where the rows start: a
    /// token step under the band. Split out of [`Self::draw_rail`].
    fn draw_rail_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let show_toggle = self.drawer.is_docked() && self.drawer.is_open();
        let toggle_h: f64 = 30.0;
        let head_top = rail.y0 + PAD;
        let (_, cap_h) = tr.measure("SOURCES", 10.5, None);
        let cap_h = f64::from(cap_h);
        let head_h = if show_toggle {
            toggle_h.max(cap_h)
        } else {
            cap_h
        };
        let head_cy = head_top + head_h / 2.0;
        paint::text(
            scene,
            tr,
            t,
            "SOURCES",
            10.5,
            c.text_dim,
            rail.x0 + PAD,
            head_cy - cap_h / 2.0,
        );
        if show_toggle {
            let cb = Rect::new(
                rail.x1 - PAD - toggle_h,
                head_cy - toggle_h / 2.0,
                rail.x1 - PAD,
                head_cy + toggle_h / 2.0,
            );
            let hov = cb.contains(self.last_mouse);
            self.drawer.draw_toggle(scene, t, theme, cb, hov);
            self.hits.push((cb, Hit::ToggleRail));
        }
        head_top + head_h + S2
    }

    /// After the rail's rows are painted: their extent (the trailing `S2`
    /// swapped for the card padding) against the space under the header
    /// band decides the scroll range; a row that slid under the band is no
    /// longer a target, so the hits pushed since `first_row_hit` are clipped
    /// to `rows_clip`; and a scrollable rail shows a thumb. Split out of
    /// [`Self::draw_rail`].
    fn finish_rail_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        rows_clip: Rect,
        y_end: f64,
        first_row_hit: usize,
    ) {
        let content_h = y_end + self.rail_scroll - rows_clip.y0 - S2 + PAD;
        self.rail_scroll_max = (content_h - rows_clip.height()).max(0.0);
        let rows = self.hits.split_off(first_row_hit);
        self.hits.extend(rows.into_iter().filter_map(|(r, hit)| {
            let r = r.intersect(rows_clip);
            (r.width() > 0.0 && r.height() > 0.0).then_some((r, hit))
        }));
        if self.rail_scroll_max <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let track_h = rows_clip.height();
        let thumb_h = (track_h * (track_h / content_h)).max(28.0);
        let thumb_y =
            rows_clip.y0 + (track_h - thumb_h) * (self.rail_scroll / self.rail_scroll_max);
        let thumb = Rect::new(
            rows_clip.x1 - S1 - 3.0,
            thumb_y,
            rows_clip.x1 - S1,
            thumb_y + thumb_h,
        );
        paint::fill_round(scene, t, thumb, 1.5, c.text_dim.with_alpha_factor(0.45));
    }

    /// The ring a selected card takes: the stronger, panel-strength derived
    /// edge over the card's rest edge — selection as a step (HIG §9.3), never
    /// an accent wash.
    fn selected_ring(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, radius: f64) {
        let c = &theme.tokens.colors;
        paint::stroke_round(
            scene,
            t,
            rect.inset(-0.5),
            radius - 0.5,
            ui::surface::edge_on_with(c.surfaces.raised, ui::chrome::PANEL_BORDER_ALPHA),
            1.0,
        );
    }

    /// Shared row highlight (selected / hovered) used by the rail rows.
    fn row_bg(
        scene: &mut Scene,
        t: Affine,
        row: Rect,
        selected: bool,
        hovered: bool,
        ground: Color,
    ) {
        // Selection is a step, not an accent wash (HIG §9.3). These rows sit
        // on the rail (`ground`: the raised card, or the Panel ground in
        // overlay mode), where `selection_tier`'s lift toward the raised tier
        // would vanish, so both states are measured against the ground
        // itself: the selected row takes the derived wash with the
        // panel-strength ring (`selected_on`), hover half that wash.
        if selected {
            let (wash, ring) = selected_on(ground);
            paint::fill_round(scene, t, row, 9.0, wash);
            paint::stroke_round(scene, t, row.inset(-0.5), 8.5, ring, 1.0);
        } else if hovered {
            paint::fill_round(
                scene,
                t,
                row,
                9.0,
                Colors::hairline_on(ground, HAIRLINE_ALPHA / 2),
            );
        }
    }

    /// The "All sources" aggregate row.
    fn draw_source_row(&mut self, ctx: &mut PaintCtx<'_>, rail: Rect, y: &mut f64, sel: bool) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let h = 38.0;
        let row = Rect::new(rail.x0 + 8.0, *y, rail.x1 - 8.0, *y + h);
        Self::row_bg(
            ctx.scene,
            ctx.transform,
            row,
            sel,
            self.hover == Some(Hit::Source(None)),
            self.rail_ground(theme),
        );
        let cy = row.y0 + h / 2.0;
        paint::text_bold(
            ctx.scene,
            ctx.text,
            ctx.transform,
            "All sources",
            13.5,
            c.text_primary,
            row.x0 + 14.0,
            cy - 7.0,
        );
        let count: usize = self.catalog.total_snapshots();
        paint::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &format!("{count}"),
            12.0,
            c.text_dim,
            row.x1 - 14.0,
            cy - 7.0,
        );
        self.hits.push((row, Hit::Source(None)));
        *y += h + S2;
    }

    fn draw_source_row_real(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rail: Rect,
        y: &mut f64,
        src: &Source,
        sel: bool,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let h = 50.0;
        let row = Rect::new(rail.x0 + 8.0, *y, rail.x1 - 8.0, *y + h);
        Self::row_bg(
            ctx.scene,
            ctx.transform,
            row,
            sel,
            self.hover == Some(Hit::Source(Some(src.id.clone()))),
            self.rail_ground(theme),
        );
        let ix = row.x0 + 22.0;
        let cy = row.y0 + h / 2.0;
        Self::draw_source_icon(
            ctx.scene,
            ctx.transform,
            src.kind,
            Point::new(ix, cy),
            10.0,
            theme.accent_primary,
        );
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &src.path,
            13.5,
            c.text_primary,
            ix + 18.0,
            row.y0 + 9.0,
        );
        let count = self.catalog.count_for(&src.id);
        let sub = format!("{} · {} snapshots", src.cadence.label(), count);
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &sub,
            11.0,
            c.text_dim,
            ix + 18.0,
            row.y0 + 28.0,
        );
        // Right column: protected size + health dot.
        paint::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &ui::format::bytes(self.catalog.protected_bytes(&src.id)),
            11.5,
            c.text_dim,
            row.x1 - 14.0,
            row.y0 + 9.0,
        );
        paint::dot(
            ctx.scene,
            ctx.transform,
            Point::new(row.x1 - 18.0, row.y0 + 33.0),
            3.5,
            Self::health_color(theme, src.health),
        );
        self.hits.push((row, Hit::Source(Some(src.id.clone()))));
        *y += h + S2;
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "t/y/h/c/d are the standard graphics convention: transform, position, row height, colour tokens, and this row's Destination"
    )]
    fn draw_dest_compact(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        y: &mut f64,
        d: &Destination,
    ) {
        let c = &theme.tokens.colors;
        // The row is as tall as its three measured lines need — name, bar,
        // usage — each a token step apart, so the usage caption never runs
        // into the next row's name (the fixed 48px row clipped it by 1-3px).
        let (_, name_h) = tr.measure("Hg", 12.5, None);
        let (_, usage_h) = tr.measure("Hg", 10.5, None);
        let bar_h = 6.0;
        let name_top = *y + S1 + 2.0;
        let bar_top = name_top + f64::from(name_h) + S1;
        let usage_top = bar_top + bar_h + S1;
        let h = usage_top + f64::from(usage_h) + S1 + 2.0 - *y;
        let row = Rect::new(rail.x0 + S2, *y, rail.x1 - S2, *y + h);
        if self.hover == Some(Hit::Dest(d.id.clone())) {
            // Hover is half the selected wash, measured against the rail.
            let ground = self.rail_ground(theme);
            paint::fill_round(
                scene,
                t,
                row,
                9.0,
                Colors::hairline_on(ground, HAIRLINE_ALPHA / 2),
            );
        }
        let ix = row.x0 + 18.0;
        Self::draw_dest_icon(
            scene,
            t,
            d.kind,
            Point::new(ix, name_top + f64::from(name_h) / 2.0),
            8.0,
            Self::health_color(theme, d.health),
        );
        paint::text(
            scene,
            tr,
            t,
            &d.name,
            12.5,
            c.text_primary,
            ix + S4,
            name_top,
        );
        let frac = d.used_bytes as f64 / d.total_bytes as f64;
        let bar = Rect::new(ix, bar_top, row.x1 - S3, bar_top + bar_h);
        let barcol = if frac > 0.85 {
            c.semantic.warning
        } else {
            theme.accent_primary
        };
        paint::bar(scene, t, bar, frac, barcol, c.surfaces.base);
        let usage = format!(
            "{} / {}",
            ui::format::bytes(d.used_bytes),
            ui::format::bytes(d.total_bytes)
        );
        paint::text(scene, tr, t, &usage, 10.5, c.text_dim, ix, usage_top);
        self.hits.push((row, Hit::Dest(d.id.clone())));
        *y += h + S2;
    }

    fn draw_summary_tiles(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        region: Rect,
        cols: usize,
    ) {
        let c = &theme.tokens.colors;
        let gap = S3;
        let cols = cols.max(1);
        let tile_w = card_column_width(region.width(), cols, gap);
        let rows = 4usize.div_ceil(cols);
        let tile_h = ((region.height() - gap * (rows as f64 - 1.0)) / rows as f64).max(64.0);
        let next_run = self
            .ui
            .sel_source
            .as_ref()
            .and_then(|id| self.catalog.source(id))
            .map_or_else(|| "In 38 min".to_string(), |s| s.next_run.clone());
        let failed = self
            .catalog
            .snapshots
            .iter()
            .filter(|s| s.status == SnapStatus::Failed)
            .count();
        let last = if failed == 0 {
            "OK · 2h ago".to_string()
        } else {
            "1 failed".to_string()
        };
        let tiles: [(&str, String, Color); 4] = [
            (
                "Protected",
                ui::format::bytes(self.catalog.total_protected()),
                theme.accent_primary,
            ),
            (
                "Snapshots",
                format!("{}", self.catalog.total_snapshots()),
                c.semantic.info,
            ),
            ("Next run", next_run, c.semantic.success),
            (
                "Last backup",
                last,
                if failed == 0 {
                    c.semantic.success
                } else {
                    c.semantic.warning
                },
            ),
        ];
        for (i, (label, value, accent)) in tiles.iter().enumerate() {
            let col = i % cols;
            let tile_row = i / cols;
            let x0 = region.x0 + col as f64 * (tile_w + gap);
            let y0 = region.y0 + tile_row as f64 * (tile_h + gap);
            let tile = Rect::new(x0, y0, x0 + tile_w, y0 + tile_h);
            paint::card(scene, t, tile, 12.0, c.surfaces.raised);
            // accent ticker on the left edge (live state colour)
            let tick = Rect::new(tile.x0, tile.y0 + S3, tile.x0 + 3.0, tile.y1 - S3);
            paint::fill_round(scene, t, tick, 1.5, *accent);
            paint::text(
                scene,
                tr,
                t,
                label,
                11.5,
                c.text_dim,
                tile.x0 + PAD,
                tile.y0 + PAD,
            );
            // The value follows the label's measured line by `S2` and elides
            // against the tile instead of running out of it.
            let vsize = if value.len() > 11 { 17.0 } else { 22.0 };
            let (_, label_h) = tr.measure("Hg", 11.5, None);
            let shown = tr.elide_to_width_weighted(
                value,
                vsize,
                (tile.width() - 2.0 * PAD).max(0.0) as f32,
                TextWeight::Bold,
            );
            paint::text_bold(
                scene,
                tr,
                t,
                &shown,
                vsize,
                c.text_primary,
                tile.x0 + PAD,
                tile.y0 + PAD + f64::from(label_h) + S2,
            );
        }
    }

    fn draw_timeline_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
    ) {
        let c = &theme.tokens.colors;
        self.timeline_view = list;
        // Group snapshots by day. Clone to owned so we don't hold an
        // immutable borrow of `self.catalog` across the `&mut self` row draws.
        let snaps: Vec<Snapshot> = self
            .catalog
            .timeline(self.ui.sel_source.as_deref())
            .into_iter()
            .cloned()
            .collect();
        let show_source = self.ui.sel_source.is_none();

        // Row bands follow the measured type (`ROW_H` / `DAY_H` / `DETAIL_H`
        // are their 1× floors): a snapshot row carries its size + meta stack,
        // a day header its bold label, the detail its two metadata lines.
        let line = |tr: &mut TextRenderer, size: f32| f64::from(tr.measure("Hg", size, None).1);
        let (h12, h11_5, h13, h10_5, h12b) = (
            line(tr, 12.0),
            line(tr, 11.5),
            line(tr, 13.0),
            line(tr, 10.5),
            f64::from(tr.measure_weighted("Hg", 12.0, None, TextWeight::Bold).1),
        );
        let row_h = ROW_H.max(h13 + h10_5 - 1.0 + 16.0);
        let day_h = DAY_H.max(h12b + 15.0);
        let detail_h = DETAIL_H.max(12.0 + h12 + 6.0 + h11_5 + 12.0);

        // First measure total content height (so we can clamp scroll).
        let mut content_h = 0.0;
        let mut last_day: Option<u32> = None;
        for s in &snaps {
            if last_day != Some(s.day) {
                content_h += day_h;
                last_day = Some(s.day);
            }
            content_h += row_h + S2;
            if self.ui.sel_snapshot.as_deref() == Some(s.id.as_str()) {
                content_h += detail_h + S1;
            }
        }
        self.timeline_content_h = content_h;
        let max_scroll = (content_h - list.height()).max(0.0);
        self.timeline_scroll = self.timeline_scroll.clamp(0.0, max_scroll);

        // Clip to the list region and draw with the scroll offset.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &list);
        let mut y = list.y0 - self.timeline_scroll;
        last_day = None;
        for s in &snaps {
            if last_day != Some(s.day) {
                // Day header: the label is centred in its band and the rule
                // runs along the label's own centre line.
                if y + day_h > list.y0 && y < list.y1 {
                    let label_top = y + (day_h - h12b) / 2.0;
                    paint::text_bold(
                        scene,
                        tr,
                        t,
                        &Catalog::day_label(s.day),
                        12.0,
                        c.text_dim,
                        list.x0 + 2.0,
                        label_top,
                    );
                    let (label_w, _) = tr.measure_weighted(
                        &Catalog::day_label(s.day),
                        12.0,
                        None,
                        TextWeight::Bold,
                    );
                    // The rule runs from `S3` after the label to the list
                    // edge, derived against the window tier it lies on.
                    paint::separator(
                        scene,
                        t,
                        list.x0 + 2.0 + f64::from(label_w) + S3,
                        list.x1,
                        y + day_h / 2.0,
                        0.0,
                        c.surfaces.base,
                    );
                }
                y += day_h;
                last_day = Some(s.day);
            }
            let selected = self.ui.sel_snapshot.as_deref() == Some(s.id.as_str());
            let row = Rect::new(list.x0, y, list.x1, y + row_h);
            let visible = row.y1 > list.y0 && row.y0 < list.y1;
            if visible {
                let ctx = &mut PaintCtx::new(scene, t, tr, theme);
                self.draw_snapshot_row(ctx, row, s, selected, show_source);
                self.hits.push((row, Hit::Snap(s.id.clone())));
            }
            y += row_h + S2;
            if selected {
                // The detail is attached to its row: `S1` under it (before,
                // it started 2px under the row and read as touching), and
                // the next row follows the detail by the ordinary `S2`.
                let det_top = y - S2 + S1;
                let det = Rect::new(list.x0, det_top, list.x1, det_top + detail_h);
                if det.y1 > list.y0 && det.y0 < list.y1 {
                    self.draw_snapshot_detail(scene, t, tr, theme, det, s);
                }
                y = det.y1 + S2;
            }
        }
        scene.pop_layer();

        // Scrollbar track indicator.
        if max_scroll > 0.0 {
            let track_h = list.height();
            let thumb_h = (track_h * (track_h / content_h)).max(28.0);
            let thumb_y = list.y0 + (track_h - thumb_h) * (self.timeline_scroll / max_scroll);
            let thumb = Rect::new(list.x1 - 4.0, thumb_y, list.x1 - 1.0, thumb_y + thumb_h);
            paint::fill_round(scene, t, thumb, 2.0, c.text_dim.with_alpha_factor(0.45));
        }
    }

    /// A configured repository may legitimately have no completed snapshots
    /// yet (for example immediately after migration or pruning).  That is a
    /// ready-to-use state, not an empty broken list.
    fn draw_timeline_empty_state(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
    ) {
        let c = &theme.tokens.colors;
        let title_y = list.y0 + list.height().min(220.0) * 0.38;
        paint::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            "No snapshots yet",
            16.0,
            c.text_primary,
            Point::new(list.center().x, title_y),
        );
        paint::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            "Run a backup to create the first verified recovery point.",
            12.5,
            c.text_dim,
            Point::new(list.center().x, title_y + 26.0),
        );
    }

    fn draw_snapshot_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        s: &Snapshot,
        selected: bool,
        show_source: bool,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let hovered = self.hover == Some(Hit::Snap(s.id.clone()));
        // Every row is an edged card on the window tier; hover and selection
        // are steps of the same lift (HIG §9.3), never an accent wash: a
        // half lift at rest, the raised tier on hover, and the raised tier
        // with the stronger panel-strength edge when selected.
        if selected {
            paint::card(ctx.scene, ctx.transform, row, 10.0, c.surfaces.raised);
            Self::selected_ring(ctx.scene, ctx.transform, theme, row, 10.0);
        } else if hovered {
            paint::card(ctx.scene, ctx.transform, row, 10.0, c.surfaces.raised);
        } else {
            paint::card(ctx.scene, ctx.transform, row, 10.0, c.lift(0.5));
        }
        let cy = f64::midpoint(row.y0, row.y1);
        // Status dot.
        paint::dot(
            ctx.scene,
            ctx.transform,
            Point::new(row.x0 + 16.0, cy),
            4.0,
            Self::status_color(theme, s.status),
        );
        // Time (bold), centred on the row from its measured bold line.
        let (time_w, time_h) = ctx
            .text
            .measure_weighted(&s.time, 14.0, None, TextWeight::Bold);
        paint::text_bold(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &s.time,
            14.0,
            c.text_primary,
            row.x0 + 30.0,
            cy - f64::from(time_h) / 2.0,
        );
        // Type + retention pills follow the measured time label (the 92px
        // column at 1×; wider once the time is). The `Auto` pill takes a
        // dim-ink wash like the `Manual` pill takes an accent one, so it reads
        // as a pill on the light theme too (a surface lift was invisible there).
        let mut px = (row.x0 + 92.0).max(row.x0 + 30.0 + f64::from(time_w) + 12.0);
        let (tfg, tbg) = match s.kind {
            SnapType::Manual => (
                theme.accent_primary,
                theme.accent_primary.with_alpha_factor(0.18),
            ),
            SnapType::Auto => (c.text_dim, c.text_dim.with_alpha_factor(0.18)),
        };
        px += paint::pill(ctx, Point::new(px, cy), s.kind.label(), 11.0, tfg, tbg) + 6.0;
        let rc = Self::retention_color(theme, s.retention);
        px += paint::tag_pill(ctx, Point::new(px, cy), s.retention.label(), 11.0, rc) + 8.0;
        // Right side: total over "+delta · files", the two-line stack centred
        // on the row (its line boxes overlap by 1px, as at 1×) instead of the
        // meta line crowding the row's bottom edge. Measured first so the
        // source path can stop where the stack begins.
        let total_x = row.x1 - 16.0;
        let total = ui::format::bytes(s.size_bytes);
        let meta = format!(
            "+{} · {} files",
            ui::format::bytes(s.delta_bytes),
            s.files_changed
        );
        let (size_w, size_h) = ctx.text.measure(&total, 13.0, None);
        let (meta_w, meta_h) = ctx.text.measure(&meta, 10.5, None);
        let stack_x0 = total_x - f64::from(size_w.max(meta_w));
        // Source path (only in "all" mode), elided against the right stack.
        if show_source && let Some(src) = self.catalog.source(&s.source_id) {
            let path_max_w = (stack_x0 - 12.0 - px).max(24.0) as f32;
            let shown = ctx.text.elide_to_width(&src.path, 12.0, path_max_w);
            paint::text_vc(ctx, &shown, 12.0, c.text_dim, Rect { x0: px, ..row });
        }
        let stack_h = f64::from(size_h) + f64::from(meta_h) - 1.0;
        let size_top = cy - stack_h / 2.0;
        paint::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &total,
            13.0,
            c.text_primary,
            total_x,
            size_top,
        );
        paint::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &meta,
            10.5,
            c.text_dim,
            total_x,
            size_top + f64::from(size_h) - 1.0,
        );
    }

    fn draw_snapshot_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        detail_rect: Rect,
        s: &Snapshot,
    ) {
        let c = &theme.tokens.colors;
        // The detail reads as the quieter companion of its selected row: the
        // same half lift the resting rows take, with its own derived edge.
        paint::card(scene, t, detail_rect, 10.0, c.lift(0.5));
        let dest = self.catalog.destination(&s.dest_id);
        let dest_name = dest.map(|found| found.name.clone()).unwrap_or_default();
        let dest_kind = dest.map_or("", |found| found.kind.label());
        // Buttons are sized from their labels (the shared face paints at the
        // 14px base size) so `Restore…` never clips at a larger text scale.
        let (restore_w, _) = tr.measure("Restore…", 14.0, None);
        let (verify_w, _) = tr.measure("Verify", 14.0, None);
        let restore_w = 88.0_f64.max(f64::from(restore_w) + 28.0);
        let verify_w = 84.0_f64.max(f64::from(verify_w) + 28.0);
        let restore = Rect::new(
            detail_rect.x1 - 8.0 - restore_w,
            detail_rect.y0 + 25.0,
            detail_rect.x1 - 8.0,
            detail_rect.y1 - 8.0,
        );
        let verify = Rect::new(
            restore.x0 - 8.0 - verify_w,
            detail_rect.y0 + 25.0,
            restore.x0 - 8.0,
            detail_rect.y1 - 8.0,
        );
        // The metadata lines stop where the action buttons begin; at phone
        // widths the second line otherwise ran underneath Verify. The second
        // line follows the first's measured height.
        let text_x = detail_rect.x0 + 16.0;
        let text_max_w = ((verify.x0 - 12.0) - text_x).max(40.0) as f32;
        let stored = tr.elide_to_width(
            &format!("Stored on {dest_name}  ·  {dest_kind}"),
            12.0,
            text_max_w,
        );
        let (_, stored_h) = tr.measure("Hg", 12.0, None);
        paint::text(
            scene,
            tr,
            t,
            &stored,
            12.0,
            c.text_dim,
            text_x,
            detail_rect.y0 + 12.0,
        );
        let enc = if dest.is_some_and(|found| found.encrypted) {
            "encrypted"
        } else {
            "plaintext"
        };
        let meta = tr.elide_to_width(
            &format!(
                "{} files · {} changed · {}",
                s.files_changed,
                ui::format::bytes(s.delta_bytes),
                enc
            ),
            11.5,
            text_max_w,
        );
        paint::text(
            scene,
            tr,
            t,
            &meta,
            11.5,
            c.text_dim,
            text_x,
            detail_rect.y0 + 12.0 + f64::from(stored_h) + 6.0,
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(verify, "Verify")
                .radius(7.0)
                .hovered(self.hover == Some(Hit::Verify)),
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(restore, "Restore…")
                .primary()
                .radius(7.0)
                .hovered(self.hover == Some(Hit::RestoreOpen)),
        );
        self.hits.push((verify, Hit::Verify));
        self.hits.push((restore, Hit::RestoreOpen));
    }

    // ---- SOURCES view ---------------------------------------------------

    fn draw_sources_view(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        Self::draw_view_header(
            scene,
            t,
            tr,
            theme,
            content,
            "Backup sources",
            "Folders captured on a schedule into your destinations.",
        );

        // At narrow widths the three inline stat columns collapse to one line
        // beneath the path, so nothing is crushed against the toggle.
        let narrow = content.width() < 620.0;
        let mut y = Self::view_body_top(tr, content);
        let card_h = if narrow { 104.0 } else { 96.0 };
        for i in 0..self.catalog.sources.len() {
            let Some(src) = self.catalog.sources.get(i).cloned() else {
                continue;
            };
            self.draw_source_card(
                &mut PaintCtx::new(scene, t, tr, theme),
                Rect::new(content.x0, y, content.x1, y + card_h),
                narrow,
                &src,
            );
            y += card_h + S3;
        }

        // Recent activity feed — the next group — fills the remaining space
        // `S6` under the last card.
        let region = Rect::new(content.x0, y - S3 + S6, content.x1, content.y1);
        if region.height() > 80.0 {
            self.draw_recent_activity(scene, t, tr, theme, region);
        }
    }

    /// Every view's title + subtitle group: the subtitle follows the title's
    /// measured line by `S1` (a title and the line that explains it), and
    /// the body starts `S4` under it ([`Self::view_body_top`]) — one header
    /// rhythm for Sources, Destinations and Schedules.
    fn draw_view_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        title: &str,
        subtitle: &str,
    ) {
        let c = &theme.tokens.colors;
        paint::text_bold(
            scene,
            tr,
            t,
            title,
            16.0,
            c.text_primary,
            content.x0,
            content.y0,
        );
        let (_, title_h) = tr.measure_weighted(title, 16.0, None, TextWeight::Bold);
        paint::text(
            scene,
            tr,
            t,
            subtitle,
            12.5,
            c.text_dim,
            content.x0,
            content.y0 + f64::from(title_h) + S1,
        );
    }

    /// Where a view's body starts under [`Self::draw_view_header`].
    fn view_body_top(tr: &mut TextRenderer, content: Rect) -> f64 {
        let (_, title_h) = tr.measure_weighted("Hg", 16.0, None, TextWeight::Bold);
        let (_, sub_h) = tr.measure("Hg", 12.5, None);
        content.y0 + f64::from(title_h) + S1 + f64::from(sub_h) + S4
    }

    /// One source card: icon + path + kind, the cadence/snapshots/protected
    /// stats, health + next-run, and the enable toggle. Split out of
    /// [`Self::draw_sources_view`] to keep it under the line budget.
    fn draw_source_card(&mut self, ctx: &mut PaintCtx<'_>, card: Rect, narrow: bool, src: &Source) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let sel = self.ui.sel_source.as_deref() == Some(src.id.as_str());
        paint::card(ctx.scene, ctx.transform, card, 14.0, c.surfaces.raised);
        if sel {
            Self::selected_ring(ctx.scene, ctx.transform, theme, card, 14.0);
        }
        let ix = card.x0 + 34.0;
        Self::draw_source_icon(
            ctx.scene,
            ctx.transform,
            src.kind,
            Point::new(ix, card.y0 + 30.0),
            13.0,
            theme.accent_primary,
        );
        paint::text_bold(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &src.path,
            17.0,
            c.text_primary,
            ix + 28.0,
            card.y0 + 16.0,
        );
        let kind = match src.kind {
            SourceKind::Home => "Home directory",
            SourceKind::System => "System configuration",
            SourceKind::Workspace => "Collaborative workspace",
            SourceKind::Custom => "Custom path",
        };
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            kind,
            12.0,
            c.text_dim,
            ix + 28.0,
            card.y0 + 40.0,
        );

        if narrow {
            Self::draw_source_card_stats_narrow(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                &self.catalog,
                card,
                ix,
                src,
            );
        } else {
            Self::draw_source_card_stats_wide(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                &self.catalog,
                card,
                src,
            );
        }

        self.draw_source_card_footer(ctx.scene, ctx.transform, ctx.text, theme, card, src);
    }

    /// The health dot, next-run label, and enable toggle at the bottom of a
    /// source card. Split out of [`Self::draw_source_card`] to keep it under
    /// the line budget.
    fn draw_source_card_footer(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        src: &Source,
    ) {
        let c = &theme.tokens.colors;
        paint::dot(
            scene,
            t,
            Point::new(card.x0 + 16.0, card.y1 - 18.0),
            3.5,
            Self::health_color(theme, src.health),
        );
        paint::text(
            scene,
            tr,
            t,
            &format!("Next run {}", src.next_run),
            11.5,
            c.text_dim,
            card.x0 + 28.0,
            card.y1 - 24.0,
        );

        // Enable switch (shared Toggle look).
        let tw = 52.0;
        let tog = Rect::new(
            card.x1 - 22.0 - tw,
            card.y1 - 30.0,
            card.x1 - 22.0,
            card.y1 - 30.0 + 22.0,
        );
        let on = src.enabled;
        Toggle::paint(scene, t, theme, &ToggleFace::new(tog, on));
        paint::text_right(
            scene,
            tr,
            t,
            if on { "Enabled" } else { "Paused" },
            10.5,
            c.text_dim,
            tog.x0 - 8.0,
            tog.center().y - 5.0,
        );

        self.hits.push((card, Hit::Source(Some(src.id.clone()))));
    }

    /// The single compact stats line shown for a source card at narrow
    /// widths. Split out of [`Self::draw_source_card`] to keep it under the
    /// line budget.
    fn draw_source_card_stats_narrow(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        catalog: &Catalog,
        card: Rect,
        ix: f64,
        src: &Source,
    ) {
        let c = &theme.tokens.colors;
        let stats = format!(
            "{} · {} snapshots · {}",
            src.cadence.label(),
            catalog.count_for(&src.id),
            ui::format::bytes(catalog.protected_bytes(&src.id)),
        );
        paint::text(
            scene,
            tr,
            t,
            &stats,
            12.0,
            c.text_dim,
            ix + 28.0,
            card.y0 + 60.0,
        );
    }

    /// The three cadence/snapshots/protected stat columns shown for a source
    /// card at wide widths. Split out of [`Self::draw_source_card`] to keep
    /// it under the line budget.
    fn draw_source_card_stats_wide(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        catalog: &Catalog,
        card: Rect,
        src: &Source,
    ) {
        let c = &theme.tokens.colors;
        let stat = |sc: &mut Scene,
                    tr: &mut TextRenderer,
                    x: f64,
                    label: &str,
                    value: &str,
                    col: Color| {
            paint::text(sc, tr, t, label, 10.5, c.text_dim, x, card.y0 + 16.0);
            paint::text_bold(sc, tr, t, value, 15.0, col, x, card.y0 + 32.0);
        };
        let col0 = card.x0 + card.width() * 0.46;
        let col1 = card.x0 + card.width() * 0.62;
        let col2 = card.x0 + card.width() * 0.78;
        stat(
            scene,
            tr,
            col0,
            "CADENCE",
            src.cadence.label(),
            c.text_primary,
        );
        stat(
            scene,
            tr,
            col1,
            "SNAPSHOTS",
            &format!("{}", catalog.count_for(&src.id)),
            c.semantic.info,
        );
        stat(
            scene,
            tr,
            col2,
            "PROTECTED",
            &ui::format::bytes(catalog.protected_bytes(&src.id)),
            theme.accent_primary,
        );
    }

    fn draw_recent_activity(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        region: Rect,
    ) {
        let c = &theme.tokens.colors;
        paint::card(scene, t, region, 14.0, c.surfaces.raised);
        paint::text_bold(
            scene,
            tr,
            t,
            "Recent activity",
            13.5,
            c.text_primary,
            region.x0 + PAD,
            region.y0 + PAD,
        );
        paint::text_right(
            scene,
            tr,
            t,
            "last 7 events",
            11.0,
            c.text_dim,
            region.x1 - PAD,
            region.y0 + PAD + 2.0,
        );

        // Below this the fixed activity columns won't fit; the row folds to two
        // lines (when + size on top, the action beneath).
        let narrow = region.width() < 660.0;
        let events: Vec<Snapshot> = self
            .catalog
            .timeline(None)
            .into_iter()
            .take(7)
            .cloned()
            .collect();
        let row_h = if narrow { 46.0 } else { 38.0 };
        // Rows start `S2` under the card title's measured line.
        let (_, title_h) = tr.measure_weighted("Hg", 13.5, None, TextWeight::Bold);
        let y0 = region.y0 + PAD + f64::from(title_h) + S2;
        for (i, event) in events.iter().enumerate() {
            let ry = y0 + i as f64 * row_h;
            if ry + row_h > region.y1 - 8.0 {
                break;
            }
            let path = self
                .catalog
                .source(&event.source_id)
                .map(|found| found.path.clone())
                .unwrap_or_default();
            let entry = ActivityEntry {
                event,
                when: format!("{} {}", Catalog::day_label(event.day), event.time),
                action: format!("{} snapshot of {}", event.kind.label(), path),
            };
            let row = Rect::new(region.x0, ry, region.x1, ry + row_h);
            let ctx = &mut PaintCtx::new(scene, t, tr, theme);
            if narrow {
                Self::draw_recent_activity_row_narrow(ctx, row, &entry);
            } else {
                self.draw_recent_activity_row_wide(ctx, row, &entry);
            }
            if i + 1 < events.len() && ry + row_h * 2.0 <= region.y1 - 8.0 {
                paint::separator(
                    scene,
                    t,
                    region.x0,
                    region.x1,
                    ry + row_h,
                    S3,
                    c.surfaces.raised,
                );
            }
        }
    }

    /// One narrow-layout recent-activity row: status dot, when + size on top,
    /// the action beneath. Split out of [`Self::draw_recent_activity`] to
    /// keep it under the line budget.
    fn draw_recent_activity_row_narrow(
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        entry: &ActivityEntry<'_>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        paint::dot(
            ctx.scene,
            ctx.transform,
            Point::new(row.x0 + 22.0, row.y0 + 12.0),
            4.0,
            Self::status_color(theme, entry.event.status),
        );
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &entry.when,
            12.5,
            c.text_primary,
            row.x0 + 36.0,
            row.y0 + 4.0,
        );
        paint::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &ui::format::bytes(entry.event.delta_bytes),
            12.0,
            c.text_dim,
            row.x1 - 18.0,
            row.y0 + 4.0,
        );
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &entry.action,
            11.5,
            c.text_dim,
            row.x0 + 36.0,
            row.y0 + 24.0,
        );
    }

    /// One wide-layout recent-activity row: status dot, when, action, dest,
    /// status label, size. Split out of [`Self::draw_recent_activity`] to
    /// keep it under the line budget.
    fn draw_recent_activity_row_wide(
        &self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        entry: &ActivityEntry<'_>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let cy = row.center().y;
        paint::dot(
            ctx.scene,
            ctx.transform,
            Point::new(row.x0 + 22.0, cy),
            4.0,
            Self::status_color(theme, entry.event.status),
        );
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &entry.when,
            12.5,
            c.text_primary,
            row.x0 + 36.0,
            cy - 7.0,
        );
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &entry.action,
            12.0,
            c.text_dim,
            row.x0 + 168.0,
            cy - 7.0,
        );
        let dest = self
            .catalog
            .destination(&entry.event.dest_id)
            .map(|found| found.name.clone())
            .unwrap_or_default();
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &format!("→ {dest}"),
            11.5,
            Self::dest_color(theme, &entry.event.dest_id),
            row.x0 + 440.0,
            cy - 6.0,
        );
        let status = match entry.event.status {
            SnapStatus::Complete => "completed",
            SnapStatus::Verifying => "verifying",
            SnapStatus::Failed => "failed",
        };
        paint::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            status,
            11.5,
            Self::status_color(theme, entry.event.status),
            row.x1 - 110.0,
            cy - 6.0,
        );
        paint::text_right(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &ui::format::bytes(entry.event.delta_bytes),
            12.0,
            c.text_dim,
            row.x1 - 18.0,
            cy - 7.0,
        );
    }

    // ---- DESTINATIONS view ---------------------------------------------

    fn draw_destinations_view(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        Self::draw_view_header(
            scene,
            t,
            tr,
            theme,
            content,
            "Destinations",
            "Where snapshots are stored. Capacity is shown per repository.",
        );

        // Reflow the repository cards into an aligned grid — tall gauge cards
        // when several fit abreast, compact horizontal cards in a single column
        // when the window is too narrow for the ring layout. Sibling cards
        // sit `S3` apart, like the summary tiles.
        let n = self.catalog.destinations.len();
        let gap = S3;
        let cols = card_columns(content.width(), 240.0, gap).clamp(1, n.max(1));
        let cw = card_column_width(content.width(), cols, gap);
        let top = Self::view_body_top(tr, content);
        let compact = cols == 1;
        let ch = if compact { 116.0 } else { 250.0 };
        let row_gap = S3;
        let mut rows_used = 1usize;
        for i in 0..n {
            let Some(d) = self.catalog.destinations.get(i).cloned() else {
                continue;
            };
            let col = i % cols;
            let row = i / cols;
            rows_used = row + 1;
            let x0 = content.x0 + col as f64 * (cw + gap);
            let y0 = top + row as f64 * (ch + row_gap);
            let card = Rect::new(x0, y0, x0 + cw, y0 + ch);
            let sel = self.ui.sel_dest.as_deref() == Some(d.id.as_str());
            if compact {
                Self::draw_dest_card_compact(scene, t, tr, theme, card, &d, sel);
            } else {
                Self::draw_dest_card_tall(scene, t, tr, theme, card, &d, sel);
            }
            self.hits.push((card, Hit::Dest(d.id.clone())));
        }

        // Aggregate bar across the bottom.
        let used: u64 = self.catalog.destinations.iter().map(|d| d.used_bytes).sum();
        let total: u64 = self
            .catalog
            .destinations
            .iter()
            .map(|d| d.total_bytes)
            .sum();
        // The aggregate is the next group: `S6` under the last card row.
        let ay = top + rows_used as f64 * (ch + row_gap) - row_gap + S6;
        paint::text_bold(
            scene,
            tr,
            t,
            "Total capacity",
            13.5,
            c.text_primary,
            content.x0,
            ay,
        );
        let bar = Rect::new(content.x0, ay + 24.0, content.x1, ay + 38.0);
        paint::bar(
            scene,
            t,
            bar,
            used as f64 / total as f64,
            theme.accent_primary,
            c.lift(1.0),
        );
        paint::text(
            scene,
            tr,
            t,
            &format!(
                "{} used of {} across {} destinations",
                ui::format::bytes(used),
                ui::format::bytes(total),
                self.catalog.destinations.len()
            ),
            11.5,
            c.text_dim,
            content.x0,
            ay + 44.0,
        );

        // Distribution: where the snapshots actually live.
        self.draw_dest_distribution(scene, t, tr, theme, content, ay + 76.0);
    }

    fn dest_color(theme: &Theme, id: &str) -> Color {
        let c = &theme.tokens.colors;
        match id {
            "pool" => theme.accent_primary,
            "usb" => c.semantic.info,
            "b2" => c.semantic.success,
            _ => c.text_dim,
        }
    }

    /// The full repository card: a big capacity ring plus used/free figures.
    fn draw_dest_card_tall(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
        sel: bool,
    ) {
        let c = &theme.tokens.colors;
        paint::card(scene, t, card, 16.0, c.surfaces.raised);
        if sel {
            Self::selected_ring(scene, t, theme, card, 16.0);
        }

        Self::draw_dest_card_tall_header(scene, t, tr, theme, card, d);
        Self::draw_dest_card_tall_ring(scene, t, tr, theme, card, d);
        Self::draw_dest_card_tall_figures(scene, t, tr, theme, card, d);
        Self::draw_dest_card_tall_footer(scene, t, tr, theme, card, d);
    }

    /// The icon + name + kind label at the top of a tall destination card.
    /// Split out of [`Self::draw_dest_card_tall`] to keep it under the line
    /// budget.
    fn draw_dest_card_tall_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
    ) {
        let c = &theme.tokens.colors;
        Self::draw_dest_icon(
            scene,
            t,
            d.kind,
            Point::new(card.x0 + 24.0, card.y0 + 26.0),
            9.0,
            Self::health_color(theme, d.health),
        );
        paint::text_bold(
            scene,
            tr,
            t,
            &d.name,
            16.0,
            c.text_primary,
            card.x0 + 40.0,
            card.y0 + 18.0,
        );
        paint::text(
            scene,
            tr,
            t,
            d.kind.label(),
            11.5,
            c.text_dim,
            card.x0 + 24.0,
            card.y0 + 44.0,
        );
    }

    /// The capacity ring in the middle of a tall destination card. Split out
    /// of [`Self::draw_dest_card_tall`] to keep it under the line budget.
    fn draw_dest_card_tall_ring(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
    ) {
        let c = &theme.tokens.colors;
        let frac = d.used_bytes as f64 / d.total_bytes as f64;
        let center = Point::new(card.center().x, card.y0 + 120.0);
        let ringcol = if frac > 0.85 {
            c.semantic.warning
        } else {
            theme.accent_primary
        };
        paint::ring(scene, t, center, 44.0, 11.0, frac, ringcol, c.surfaces.base);
        paint::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            &format!("{:.0}%", frac * 100.0),
            20.0,
            c.text_primary,
            Point::new(center.x, center.y - 16.0),
        );
        paint::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            "used",
            10.5,
            c.text_dim,
            Point::new(center.x, center.y + 8.0),
        );
    }

    /// The used/free byte figures beneath the ring on a tall destination
    /// card. Split out of [`Self::draw_dest_card_tall`] to keep it under the
    /// line budget.
    fn draw_dest_card_tall_figures(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
    ) {
        let c = &theme.tokens.colors;
        let fy = card.y0 + 184.0;
        paint::text(scene, tr, t, "Used", 11.0, c.text_dim, card.x0 + 24.0, fy);
        paint::text_right(
            scene,
            tr,
            t,
            &ui::format::bytes(d.used_bytes),
            12.5,
            c.text_primary,
            card.x1 - 24.0,
            fy,
        );
        paint::text(
            scene,
            tr,
            t,
            "Free",
            11.0,
            c.text_dim,
            card.x0 + 24.0,
            fy + 18.0,
        );
        paint::text_right(
            scene,
            tr,
            t,
            &ui::format::bytes(d.total_bytes - d.used_bytes),
            12.5,
            c.semantic.success,
            card.x1 - 24.0,
            fy + 18.0,
        );
    }

    /// The footer chips (encryption + last sync) at the bottom of a tall
    /// destination card. Split out of [`Self::draw_dest_card_tall`] to keep
    /// it under the line budget.
    fn draw_dest_card_tall_footer(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
    ) {
        let c = &theme.tokens.colors;
        let mut chx = card.x0 + 24.0;
        if d.encrypted {
            chx += paint::tag_pill(
                &mut PaintCtx::new(scene, t, tr, theme),
                Point::new(chx, card.y1 - 20.0),
                "encrypted",
                10.0,
                c.semantic.success,
            ) + 6.0;
        }
        paint::tag_pill(
            &mut PaintCtx::new(scene, t, tr, theme),
            Point::new(chx, card.y1 - 20.0),
            &format!("sync {}", d.last_sync),
            10.0,
            c.semantic.info,
        );
    }

    /// The compact repository card for a single-column (narrow) layout: name +
    /// percent on top, a capacity bar and figures beneath — no crushed ring.
    fn draw_dest_card_compact(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
        sel: bool,
    ) {
        let c = &theme.tokens.colors;
        paint::card(scene, t, card, 14.0, c.surfaces.raised);
        if sel {
            Self::selected_ring(scene, t, theme, card, 14.0);
        }
        Self::draw_dest_card_compact_header(scene, t, tr, theme, card, d);
        Self::draw_dest_card_compact_body(scene, t, tr, theme, card, d);
    }

    /// The icon + name + kind label + capacity percent at the top of a
    /// compact destination card. Split out of
    /// [`Self::draw_dest_card_compact`] to keep it under the line budget.
    fn draw_dest_card_compact_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
    ) {
        let c = &theme.tokens.colors;
        Self::draw_dest_icon(
            scene,
            t,
            d.kind,
            Point::new(card.x0 + 22.0, card.y0 + 24.0),
            9.0,
            Self::health_color(theme, d.health),
        );
        paint::text_bold(
            scene,
            tr,
            t,
            &d.name,
            15.0,
            c.text_primary,
            card.x0 + 40.0,
            card.y0 + 14.0,
        );
        paint::text(
            scene,
            tr,
            t,
            d.kind.label(),
            11.0,
            c.text_dim,
            card.x0 + 40.0,
            card.y0 + 34.0,
        );

        let frac = d.used_bytes as f64 / d.total_bytes as f64;
        let pctcol = if frac > 0.85 {
            c.semantic.warning
        } else {
            theme.accent_primary
        };
        paint::text_right(
            scene,
            tr,
            t,
            &format!("{:.0}%", frac * 100.0),
            20.0,
            pctcol,
            card.x1 - 20.0,
            card.y0 + 18.0,
        );
    }

    /// The capacity bar, used/free figures, and trailing chips on a compact
    /// destination card. Split out of [`Self::draw_dest_card_compact`] to
    /// keep it under the line budget.
    fn draw_dest_card_compact_body(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        d: &Destination,
    ) {
        let c = &theme.tokens.colors;
        let frac = d.used_bytes as f64 / d.total_bytes as f64;
        let bar = Rect::new(
            card.x0 + 22.0,
            card.y0 + 60.0,
            card.x1 - 22.0,
            card.y0 + 70.0,
        );
        let barcol = if frac > 0.85 {
            c.semantic.warning
        } else {
            theme.accent_primary
        };
        paint::bar(scene, t, bar, frac, barcol, c.surfaces.base);
        paint::text(
            scene,
            tr,
            t,
            &format!(
                "{} of {}",
                ui::format::bytes(d.used_bytes),
                ui::format::bytes(d.total_bytes)
            ),
            11.5,
            c.text_dim,
            card.x0 + 22.0,
            card.y0 + 80.0,
        );
        let mut chx = card.x1 - 22.0;
        // Trailing chips, laid right-to-left.
        let sync = format!("sync {}", d.last_sync);
        let (sw, _) = tr.measure(&sync, 10.0, None);
        chx -= f64::from(sw) + 20.0;
        paint::tag_pill(
            &mut PaintCtx::new(scene, t, tr, theme),
            Point::new(chx, card.y0 + 86.0),
            &sync,
            10.0,
            c.semantic.info,
        );
        if d.encrypted {
            let (ew, _) = tr.measure("encrypted", 10.0, None);
            chx -= f64::from(ew) + 26.0;
            paint::tag_pill(
                &mut PaintCtx::new(scene, t, tr, theme),
                Point::new(chx, card.y0 + 86.0),
                "encrypted",
                10.0,
                c.semantic.success,
            );
        }
    }

    fn draw_dest_distribution(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        y0: f64,
    ) {
        let c = &theme.tokens.colors;
        paint::text_bold(
            scene,
            tr,
            t,
            "Snapshots stored per destination",
            13.5,
            c.text_primary,
            content.x0,
            y0,
        );
        let counts: Vec<(String, String, usize)> = self
            .catalog
            .destinations
            .iter()
            .map(|dest| {
                let count = self
                    .catalog
                    .snapshots
                    .iter()
                    .filter(|snap| snap.dest_id == dest.id)
                    .count();
                (dest.id.clone(), dest.name.clone(), count)
            })
            .collect();
        let total: usize = counts
            .iter()
            .map(|(_, _, count)| *count)
            .sum::<usize>()
            .max(1);

        // Stacked proportional bar.
        let bar = Rect::new(content.x0, y0 + 22.0, content.x1, y0 + 40.0);
        paint::fill_round(scene, t, bar, 9.0, c.lift(1.0));
        let mut bar_x = bar.x0;
        for (id, _, count) in &counts {
            let bar_w = bar.width() * (*count as f64 / total as f64);
            if bar_w > 1.0 {
                let seg = Rect::new(bar_x, bar.y0, (bar_x + bar_w).min(bar.x1), bar.y1);
                paint::fill_round(scene, t, seg, 9.0, Self::dest_color(theme, id));
            }
            bar_x += bar_w;
        }

        // Legend — wraps onto further rows so it never runs off the right edge.
        let mut lx = content.x0;
        let mut ly = y0 + 56.0;
        for (id, name, count) in &counts {
            let label = format!("{name}  ·  {count} snapshots");
            let (w, _) = tr.measure(&label, 12.0, None);
            let item_w = 16.0 + f64::from(w) + 28.0;
            if lx + item_w > content.x1 && lx > content.x0 + 0.5 {
                lx = content.x0;
                ly += 24.0;
            }
            paint::dot(
                scene,
                t,
                Point::new(lx + 5.0, ly + 6.0),
                5.0,
                Self::dest_color(theme, id),
            );
            paint::text(scene, tr, t, &label, 12.0, c.text_dim, lx + 16.0, ly);
            lx += item_w + 20.0;
        }
    }

    // ---- SCHEDULES view -------------------------------------------------

    fn draw_schedules_view(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        // Below this the editor + summary stack vertically instead of a 60/40 split.
        let scale = Self::text_scale(tr);
        let stacked = Self::schedules_stack_for(content, scale);
        Self::draw_schedules_header(scene, t, tr, theme, content, stacked);

        let top = Self::view_body_top(tr, content);
        let panel = Self::schedules_panel_rect(content, top, stacked, scale);
        let c = &theme.tokens.colors;
        paint::card(scene, t, panel, 16.0, c.surfaces.raised);

        let p = self.ui.policy;
        self.draw_schedules_editor_rows(scene, t, tr, theme, panel, p);

        // Summary of effective retention — beside the editor when wide, stacked
        // beneath it when narrow.
        let side = Self::schedules_side_rect(content, top, panel, stacked);
        if side.y1 <= content.y1 {
            Self::draw_schedules_summary(scene, t, tr, theme, side, p);
        }

        // Retention ruler — one lane per class, dots = snapshots kept. The
        // next group: `S6` below whichever of {editor, stacked summary} is
        // lowest.
        let ruler_top = if stacked { side.y1 } else { panel.y1 } + S6;
        let ruler = Rect::new(content.x0, ruler_top, content.x1, ruler_top + 200.0);
        if ruler.y1 < content.y1 {
            Self::draw_retention_ruler(scene, t, tr, theme, ruler, p);
        }
    }

    fn schedules_stack_for(content: Rect, text_scale: f64) -> bool {
        content.width() < 660.0 * text_scale
    }

    /// The "Retention policy" title + subtitle. Split out of
    /// [`Self::draw_schedules_view`] to keep it under the line budget.
    fn draw_schedules_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        stacked: bool,
    ) {
        let desc = if stacked {
            "Snapshots kept per class before pruning."
        } else {
            "How many snapshots to keep in each class before pruning. Applies to every source."
        };
        Self::draw_view_header(scene, t, tr, theme, content, "Retention policy", desc);
    }

    /// The editor panel's rect: full width when stacked, 60% when
    /// side-by-side with the summary. Split out of
    /// [`Self::draw_schedules_view`] to keep it under the line budget.
    fn schedules_panel_rect(content: Rect, top: f64, stacked: bool, scale: f64) -> Rect {
        let panel_w = if stacked {
            content.width()
        } else {
            content.width() * 0.6
        };
        // Four 56px rows `S2` apart inside `PAD` of padding.
        Rect::new(
            content.x0,
            top,
            content.x0 + panel_w,
            top + PAD
                + 4.0 * (56.0 + (scale - 1.0) * 44.0)
                + 3.0 * (S2 + (scale - 1.0) * 8.0)
                + PAD,
        )
    }

    /// The four per-class retention rows inside the editor panel. Split out
    /// of [`Self::draw_schedules_view`] to keep it under the line budget.
    fn draw_schedules_editor_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        p: RetentionPolicy,
    ) {
        let vals = [p.hourly, p.daily, p.weekly, p.monthly];
        for (i, field) in PolicyField::ALL.iter().enumerate() {
            // `vals` and `PolicyField::ALL` both have exactly 4 entries.
            let value = vals.get(i).copied().unwrap_or(0);
            self.draw_schedules_editor_row(
                &mut PaintCtx::new(scene, t, tr, theme),
                panel,
                i,
                *field,
                value,
            );
        }
    }

    /// One retention-class row: focus highlight, dot + label + unit, and the
    /// -/value/+ stepper. Split out of [`Self::draw_schedules_editor_rows`]
    /// to keep it under the line budget.
    fn draw_schedules_editor_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        panel: Rect,
        i: usize,
        field: PolicyField,
        value: u32,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let scale = Self::text_scale(ctx.text);
        // Rows are `S2` apart inside the panel's `PAD`; keyboard focus is the
        // focus ring token, like every other focused control.
        let row_h = 56.0 + (scale - 1.0) * 44.0;
        let row_step = row_h + S2 + (scale - 1.0) * 8.0;
        let ry = panel.y0 + PAD + i as f64 * row_step;
        let row = Rect::new(panel.x0 + S3, ry, panel.x1 - S3, ry + row_h);
        let focused = self.policy_focus == i;
        if focused {
            paint::fill_round(ctx.scene, ctx.transform, row, 10.0, c.lift(0.5));
            paint::stroke_round(ctx.scene, ctx.transform, row, 10.0, c.border_focus, 2.0);
        }
        let rc = Self::retention_color(
            theme,
            match field {
                PolicyField::Hourly => Retention::Hourly,
                PolicyField::Daily => Retention::Daily,
                PolicyField::Weekly => Retention::Weekly,
                PolicyField::Monthly => Retention::Monthly,
            },
        );
        paint::dot(
            ctx.scene,
            ctx.transform,
            Point::new(row.x0 + 18.0 * scale, row.center().y),
            5.0,
            rc,
        );
        paint::text_bold(
            ctx.scene,
            ctx.text,
            ctx.transform,
            field.label(),
            14.5,
            c.text_primary,
            row.x0 + 34.0 * scale,
            row.y0 + 12.0 * scale,
        );
        paint::text(
            ctx.scene,
            ctx.text,
            ctx.transform,
            &format!("keep over the {}", field.unit()),
            11.0,
            c.text_dim,
            row.x0 + 34.0 * scale,
            row.y0 + 32.0 * scale,
        );

        // Stepper: [-]  value  [+]
        let cy = row.center().y;
        let plus = Rect::new(
            row.x1 - 44.0 * scale,
            cy - 18.0 * scale,
            row.x1 - 8.0 * scale,
            cy + 18.0 * scale,
        );
        let valx0 = plus.x0 - 64.0 * scale;
        let minus = Rect::new(
            valx0 - 44.0 * scale,
            cy - 18.0 * scale,
            valx0 - 8.0 * scale,
            cy + 18.0 * scale,
        );
        self.draw_stepper(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            minus,
            "–",
            Hit::Policy(field, -1),
        );
        self.draw_stepper(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            plus,
            "+",
            Hit::Policy(field, 1),
        );
        paint::text_center(
            ctx,
            &format!("{value}"),
            20.0,
            c.text_primary,
            Point::new(
                f64::midpoint(valx0 - 8.0 * scale, plus.x0),
                cy - 12.0 * scale,
            ),
        );
    }

    /// The side summary panel's rect: beside the editor when wide, stacked
    /// beneath it when narrow. Split out of [`Self::draw_schedules_view`] to
    /// keep it under the line budget.
    fn schedules_side_rect(content: Rect, top: f64, panel: Rect, stacked: bool) -> Rect {
        // The editor and its summary are sections `S4` apart, whichever way
        // they are arranged.
        if stacked {
            Rect::new(content.x0, panel.y1 + S4, content.x1, panel.y1 + S4 + 210.0)
        } else {
            Rect::new(panel.x1 + S4, top, content.x1, panel.y1)
        }
    }

    /// The "Effective window" summary card. Split out of
    /// [`Self::draw_schedules_view`] to keep it under the line budget.
    fn draw_schedules_summary(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        p: RetentionPolicy,
    ) {
        let c = &theme.tokens.colors;
        paint::card(scene, t, side, 16.0, c.surfaces.raised);
        paint::text_bold(
            scene,
            tr,
            t,
            "Effective window",
            13.5,
            c.text_primary,
            side.x0 + 16.0,
            side.y0 + 14.0,
        );
        let total = p.hourly + p.daily + p.weekly + p.monthly;
        let lines = [
            format!("{} hourly  →  last {} hours", p.hourly, p.hourly),
            format!("{} daily   →  last {} days", p.daily, p.daily),
            format!("{} weekly  →  last {} weeks", p.weekly, p.weekly),
            format!("{} monthly →  last {} months", p.monthly, p.monthly),
        ];
        for (i, line) in lines.iter().enumerate() {
            paint::text(
                scene,
                tr,
                t,
                line,
                12.0,
                c.text_dim,
                side.x0 + 16.0,
                side.y0 + 44.0 + i as f64 * 24.0,
            );
        }
        paint::separator(
            scene,
            t,
            side.x0,
            side.x1,
            side.y0 + 150.0,
            S3,
            c.surfaces.raised,
        );
        paint::text(
            scene,
            tr,
            t,
            "Snapshots retained",
            11.5,
            c.text_dim,
            side.x0 + 16.0,
            side.y0 + 162.0,
        );
        paint::text_bold(
            scene,
            tr,
            t,
            &format!("up to {total}"),
            20.0,
            theme.accent_primary,
            side.x0 + 16.0,
            side.y0 + 180.0,
        );
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "t/c/p/n/r/k are the standard graphics convention: transform, colour tokens, the RetentionPolicy, dot count, dot radius, and the dot loop index"
    )]
    fn draw_retention_ruler(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        region: Rect,
        p: RetentionPolicy,
    ) {
        let c = &theme.tokens.colors;
        paint::card(scene, t, region, 16.0, c.surfaces.raised);
        paint::text_bold(
            scene,
            tr,
            t,
            "Retention at a glance",
            13.5,
            c.text_primary,
            region.x0 + 18.0,
            region.y0 + 14.0,
        );
        paint::text(
            scene,
            tr,
            t,
            "Each dot is a snapshot the policy keeps; older ones are pruned to make room.",
            11.5,
            c.text_dim,
            region.x0 + 18.0,
            region.y0 + 34.0,
        );

        let lanes: [(Retention, &str, u32); 4] = [
            (Retention::Hourly, "Hourly", p.hourly),
            (Retention::Daily, "Daily", p.daily),
            (Retention::Weekly, "Weekly", p.weekly),
            (Retention::Monthly, "Monthly", p.monthly),
        ];
        let lane_h = 32.0;
        let top = region.y0 + 60.0;
        let dots_x0 = region.x0 + 110.0;
        let dots_x1 = region.x1 - 90.0;
        for (i, (ret, label, count)) in lanes.iter().enumerate() {
            let ly = top + i as f64 * lane_h;
            let cy = ly + lane_h / 2.0;
            let col = Self::retention_color(theme, *ret);
            paint::text(
                scene,
                tr,
                t,
                label,
                12.5,
                c.text_primary,
                region.x0 + 18.0,
                cy - 7.0,
            );
            // Lay out `count` dots across the lane, capped so they always fit.
            let n = (*count).max(1);
            let span = dots_x1 - dots_x0;
            let step = (span / f64::from(n)).min(20.0);
            let r = (step * 0.32).clamp(2.5, 5.0);
            for k in 0..n {
                let dx = dots_x0 + r + f64::from(k) * step;
                if dx > dots_x1 {
                    break;
                }
                paint::dot(
                    scene,
                    t,
                    Point::new(dx, cy),
                    r,
                    col.with_alpha_factor(if k == 0 { 1.0 } else { 0.55 }),
                );
            }
            paint::text_right(
                scene,
                tr,
                t,
                &format!("keep {count}"),
                12.0,
                col,
                region.x1 - 18.0,
                cy - 7.0,
            );
        }
    }

    // see `draw_source_row` above.
    fn draw_stepper(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        b: Rect,
        glyph: &str,
        hit: Hit,
    ) {
        // The shared button face: rest edge, hover and ink contrast come with
        // it instead of a hand-painted ramp fill.
        let hovered = self.hover.as_ref() == Some(&hit);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(b, glyph)
                .radius(8.0)
                .text_size(18.0)
                .hovered(hovered),
        );
        self.hits.push((b, hit));
    }

    // ---- restore modal --------------------------------------------------

    fn draw_restore_modal(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let snap = self
            .ui
            .sel_snapshot
            .as_ref()
            .and_then(|id| self.catalog.snapshot(id))
            .cloned();
        let Some(snap) = snap else { return };
        let src = self.catalog.source(&snap.source_id);

        // The shared modal chrome (scrim, card, title row, action row) frames
        // the sheet; the app paints only its body. The width the sheet used
        // to size itself (760, or the window less its margin) still decides
        // whether the body stacks.
        let narrow_modal = 760.0_f64.min(self.rect.width() - 40.0) < 620.0;
        let title = if narrow_modal {
            // A long local path or localized date must not squeeze a recovery
            // warning off a phone-sized sheet: the short heading here, the
            // bounded provenance and the contract as body lines.
            "Restore snapshot".to_string()
        } else {
            format!(
                "Restore  ·  {}  @  {} {}",
                src.map_or("Unknown source", |source| source.path.as_str()),
                Catalog::day_label(snap.day),
                snap.time
            )
        };
        let intro: Vec<String> = if narrow_modal {
            vec![
                Self::narrow_restore_metadata(src, &snap),
                "Complete snapshot · choose a new or empty folder".to_string(),
            ]
        } else {
            vec![
                "Recover the complete snapshot into a new or empty folder. Existing files are never replaced."
                    .to_string(),
            ]
        };
        let actions = [
            ModalAction::secondary("Cancel").hovered(self.hover == Some(Hit::RestoreCancel)),
            ModalAction::primary("Choose empty folder…")
                .hovered(self.hover == Some(Hit::RestoreConfirm)),
        ];
        // Cancel (and Escape) is the visible way out. The frame's own close
        // control would be a second cancel the sheet's two-control contract
        // does not want, and a stray backdrop click must not dismiss a
        // recovery sheet — the scrim still swallows it (`hit_at`).
        let spec = ModalSpec::new(&title)
            .actions(&actions)
            .body_height(if narrow_modal { 500.0 } else { 350.0 })
            .max_width(760.0)
            .closable(false)
            .dismiss_on_scrim(false);
        let layout = ModalFrame::layout(self.rect, &spec, tr, theme);
        ModalFrame::paint_backdrop(scene, t, tr, theme, &layout, &spec);

        let (contents, safety) =
            Self::draw_restore_intro(scene, t, tr, theme, layout.body, narrow_modal, &intro);
        Self::draw_restore_contents(scene, t, tr, theme, contents, &snap);
        Self::draw_restore_safety(scene, t, tr, theme, safety, &snap);
        ModalFrame::paint_actions(scene, t, tr, theme, &layout, &spec);

        // The hit rects are the frame's own action rects, in authored order,
        // so a press lands exactly where the face was painted.
        if let [cancel, confirm] = layout.actions.as_slice() {
            self.hits.push((*confirm, Hit::RestoreConfirm));
            self.hits.push((*cancel, Hit::RestoreCancel));
        }
    }

    /// The body's intro lines (the recovery contract, or the provenance +
    /// contract when stacked) under the frame's title row, then the split of
    /// the remaining body into the contents preview and the safety summary:
    /// side by side when wide, stacked when the sheet is too narrow for two
    /// readable columns.
    fn draw_restore_intro(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        narrow_modal: bool,
        intro: &[String],
    ) -> (Rect, Rect) {
        let (_, dim) = ModalFrame::ink(theme);
        let size = if narrow_modal { 11.5 } else { 12.0 };
        let (_, line_h) = tr.measure("Hg", size, None);
        let line_h = f64::from(line_h);
        let mut y = body.y0;
        for line in intro {
            let shown = tr.elide_to_width(line, size, body.width() as f32);
            paint::text(scene, tr, t, &shown, size, dim, body.x0, y);
            y += line_h + 3.0;
        }
        let body_y = y + 11.0;
        let body_bottom = body.y1;
        if narrow_modal {
            // Give the safety summary enough height for its four factual rows
            // and the estimate line, so nothing overlaps.
            let mid = body_y + (body_bottom - body_y) * 0.46;
            (
                Rect::new(body.x0, body_y, body.x1, mid - 8.0),
                Rect::new(body.x0, mid + 10.0, body.x1, body_bottom),
            )
        } else {
            let split = body.x0 + body.width() * 0.58;
            (
                Rect::new(body.x0, body_y, split - 12.0, body_bottom),
                Rect::new(split + 12.0, body_y, body.x1, body_bottom),
            )
        }
    }

    /// Phone-width provenance deliberately uses a source kind, never an
    /// unbounded local path. The painter still elides it against the actual
    /// viewport to accommodate localized day/time labels.
    fn narrow_restore_metadata(src: Option<&Source>, snap: &Snapshot) -> String {
        let source = src.map_or("Unknown source", |source| source.kind.label());
        format!(
            "From {source} · {} {}",
            Catalog::day_label(snap.day),
            snap.time
        )
    }

    /// A non-interactive contents preview. The restore engine currently
    /// restores whole snapshots, so these rows intentionally are not selectable
    /// and never imply a per-file restore capability.
    fn draw_restore_contents(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tree: Rect,
        snap: &Snapshot,
    ) {
        let c = &theme.tokens.colors;
        // A well inside the sheet: the window tier with its derived edge.
        paint::card(scene, t, tree, 12.0, c.surfaces.base);
        paint::text(
            scene,
            tr,
            t,
            "Snapshot contents",
            11.0,
            c.text_dim,
            tree.x0 + 14.0,
            tree.y0 + 10.0,
        );

        // Rows carry their 12.5px name (30px at 1×) and start under the
        // caption's measured line.
        let files = &snap.files;
        let (_, name_h) = tr.measure("Hg", 12.5, None);
        let (_, caption_h) = tr.measure("Hg", 11.0, None);
        let row_h = 30.0_f64.max(f64::from(name_h) + 12.0);
        let ty0 = tree.y0 + 10.0 + f64::from(caption_h) + 6.25;
        // No indexer yet on the live app: a real snapshot without an indexed
        // listing shows an honest empty state rather than fabricated files.
        if files.is_empty() {
            paint::text_vc(
                &mut PaintCtx::new(scene, t, tr, theme),
                "File index not available for this snapshot yet.",
                12.5,
                c.text_dim,
                Rect::new(tree.x0 + 14.0, ty0, tree.x1, ty0 + row_h),
            );
        }
        for (i, f) in files.iter().enumerate() {
            let ry = ty0 + i as f64 * row_h;
            // Don't spill rows past the (possibly shortened) tree card.
            if ry + row_h > tree.y1 - 4.0 {
                break;
            }
            Self::draw_restore_content_row(scene, t, tr, theme, tree, ry, row_h, f);
        }
    }

    /// One informational row in the snapshot preview. It deliberately has no
    /// hover, selection, hit target, or checkbox because recovery is all-or-
    /// nothing at this engine boundary.
    fn draw_restore_content_row(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tree: Rect,
        ry: f64,
        row_h: f64,
        f: &SnapFile,
    ) {
        let c = &theme.tokens.colors;
        let row = Rect::new(tree.x0 + 6.0, ry, tree.x1 - 6.0, ry + row_h - 2.0);
        let ix = row.x0 + 10.0 + f.depth as f64 * 18.0;
        let icol = if f.is_dir {
            theme.accent_primary
        } else {
            c.text_dim
        };
        if f.is_dir {
            // folder glyph
            let fr = Rect::new(ix, row.center().y - 5.0, ix + 13.0, row.center().y + 5.0);
            paint::fill_round(scene, t, fr, 2.0, icol.with_alpha_factor(0.8));
        } else {
            let fr = Rect::new(
                ix + 1.0,
                row.center().y - 6.0,
                ix + 11.0,
                row.center().y + 6.0,
            );
            paint::stroke_round(scene, t, fr, 2.0, icol, 1.3);
        }
        paint::text_vc(
            &mut PaintCtx::new(scene, t, tr, theme),
            &f.name,
            12.5,
            c.text_primary,
            Rect::new(ix + 20.0, row.y0, row.x1, row.y0 + row_h - 2.0),
        );
        if !f.size.is_empty() {
            paint::text_right(
                scene,
                tr,
                t,
                &f.size,
                11.0,
                c.text_dim,
                row.x1 - 10.0,
                row.center().y - 6.0,
            );
        }
    }

    /// The factual recovery contract and size summary. This replaces decorative
    /// checkboxes: every statement is enforced by `engine::restore_snapshot`.
    fn draw_restore_safety(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        opt: Rect,
        snap: &Snapshot,
    ) {
        let c = &theme.tokens.colors;
        paint::text_bold(
            scene,
            tr,
            t,
            "Safe recovery",
            13.5,
            c.text_primary,
            opt.x0,
            opt.y0 + 8.0,
        );
        let facts = [
            "Restores the complete snapshot",
            "Choose a new or empty destination folder",
            "Never replaces existing files",
            "Verifies every restored file",
        ];
        // Facts start under the heading's measured line and step by their own
        // (28px at 1×); the bullet rides each fact's centre.
        let (_, head_h) = tr.measure_weighted("Hg", 13.5, None, TextWeight::Bold);
        let (_, fact_h) = tr.measure("Hg", 12.5, None);
        let facts_top = opt.y0 + 8.0 + f64::from(head_h) + 13.0;
        let pitch = 28.0_f64.max(f64::from(fact_h) + 12.0);
        for (i, fact) in facts.iter().enumerate() {
            let oy = facts_top + i as f64 * pitch;
            paint::dot(
                scene,
                t,
                Point::new(opt.x0 + 5.0, oy + f64::from(fact_h) / 2.0),
                2.5,
                theme.accent_primary,
            );
            let shown = tr.elide_to_width(fact, 12.5, (opt.width() - 16.0).max(0.0) as f32);
            paint::text(
                scene,
                tr,
                t,
                &shown,
                12.5,
                c.text_primary,
                opt.x0 + 16.0,
                oy,
            );
        }
        // Size summary, under a rule derived against the sheet.
        paint::separator(
            scene,
            t,
            opt.x0,
            opt.x1,
            opt.y1 - 40.0,
            0.0,
            c.surfaces.raised,
        );
        paint::text(
            scene,
            tr,
            t,
            "Estimated restore",
            11.0,
            c.text_dim,
            opt.x0,
            opt.y1 - 30.0,
        );
        paint::text_right(
            scene,
            tr,
            t,
            &ui::format::bytes(snap.size_bytes),
            14.0,
            c.text_primary,
            opt.x1,
            opt.y1 - 32.0,
        );
    }

    fn draw_toast(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some((msg, kind, when)) = self.toast.as_ref() else {
            return;
        };
        let age = when.elapsed().as_secs_f64();
        if age > 3.5 {
            self.toast = None;
            return;
        }
        let alpha = if age > 2.5 {
            (1.0 - (age - 2.5)) as f32
        } else {
            1.0
        };
        paint_transient(
            scene,
            t,
            tr,
            theme,
            self.rect,
            ToastAnchor::BottomCenter,
            &ToastCard::new(msg, *kind).alpha(alpha),
        );
    }

    // ---- little vector icons -------------------------------------------

    #[allow(
        clippy::many_single_char_names,
        reason = "t/x/y/r/k/a are the standard graphics convention: transform, position, radius, loop index, angle"
    )]
    fn draw_source_icon(
        scene: &mut Scene,
        t: Affine,
        kind: SourceKind,
        center: Point,
        r: f64,
        color: Color,
    ) {
        let (x, y) = (center.x, center.y);
        match kind {
            SourceKind::Home => {
                paint::poly(
                    scene,
                    t,
                    &[(x - r, y + r * 0.2), (x, y - r), (x + r, y + r * 0.2)],
                    color,
                    2.0,
                    false,
                );
                paint::poly(
                    scene,
                    t,
                    &[
                        (x - r * 0.7, y),
                        (x - r * 0.7, y + r),
                        (x + r * 0.7, y + r),
                        (x + r * 0.7, y),
                    ],
                    color,
                    2.0,
                    false,
                );
            }
            SourceKind::System => {
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    t,
                    color,
                    None,
                    &vello::kurbo::Circle::new(center, r * 0.55),
                );
                for k in 0..8 {
                    let a = f64::from(k) * std::f64::consts::TAU / 8.0;
                    let (dx, dy) = (a.cos(), a.sin());
                    paint::poly(
                        scene,
                        t,
                        &[
                            (x + dx * r * 0.6, y + dy * r * 0.6),
                            (x + dx * r, y + dy * r),
                        ],
                        color,
                        2.0,
                        false,
                    );
                }
            }
            SourceKind::Workspace => {
                paint::stroke_round(
                    scene,
                    t,
                    Rect::new(x - r, y - r * 0.7, x + r, y + r * 0.7),
                    2.0,
                    color,
                    2.0,
                );
                paint::poly(
                    scene,
                    t,
                    &[(x - r * 0.5, y + r), (x + r * 0.5, y + r)],
                    color,
                    2.0,
                    false,
                );
            }
            SourceKind::Custom => {
                paint::stroke_round(
                    scene,
                    t,
                    Rect::new(x - r, y - r, x + r, y + r),
                    3.0,
                    color,
                    2.0,
                );
            }
        }
    }

    fn draw_dest_icon(
        scene: &mut Scene,
        t: Affine,
        kind: DestKind,
        center: Point,
        r: f64,
        color: Color,
    ) {
        let (x, y) = (center.x, center.y);
        match kind {
            DestKind::LocalBtrfs => {
                for k in 0..3 {
                    let yy = y - r + f64::from(k) * r;
                    paint::stroke_round(
                        scene,
                        t,
                        Rect::new(x - r, yy, x + r, yy + r * 0.7),
                        1.5,
                        color,
                        1.6,
                    );
                }
            }
            DestKind::LocalFolder => {
                paint::stroke_round(
                    scene,
                    t,
                    Rect::new(x - r, y - r * 0.7, x + r, y + r * 0.7),
                    2.0,
                    color,
                    1.8,
                );
            }
            DestKind::External => {
                paint::stroke_round(
                    scene,
                    t,
                    Rect::new(x - r, y - r * 0.6, x + r, y + r * 0.6),
                    2.0,
                    color,
                    1.8,
                );
                paint::dot(scene, t, Point::new(x + r * 0.5, y), 1.6, color);
            }
            DestKind::RemoteRestic => {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.8),
                    t,
                    color,
                    None,
                    &vello::kurbo::Circle::new(center, r),
                );
                paint::poly(scene, t, &[(x - r, y), (x + r, y)], color, 1.6, false);
                paint::poly(scene, t, &[(x, y - r), (x, y + r)], color, 1.6, false);
            }
        }
    }

    // ---- event handling -------------------------------------------------

    fn hit_at(&self, p: Point) -> Option<Hit> {
        // Last pushed wins (topmost), so iterate in reverse.
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(_, h)| h.clone());
        // A modal is modal: while the restore sheet is open only its own
        // controls are reachable, so a press on the scrim can neither select
        // a snapshot behind it nor switch views.
        if self.ui.restore_open && !matches!(hit, Some(Hit::RestoreConfirm | Hit::RestoreCancel)) {
            return None;
        }
        hit
    }

    fn dispatch(&mut self, hit: Hit, doc: &mut AutoCommit, root: &ObjId) {
        match hit {
            Hit::Tab(v) => self.ui.view = v,
            Hit::Source(opt) => {
                self.ui.sel_source = opt;
                self.ui.sel_snapshot = None;
                self.ui.view = View::Timeline;
                self.timeline_scroll = 0.0;
                // Picking a source from an overlay rail dismisses the drawer.
                if self.drawer.is_overlay() {
                    self.drawer.close();
                }
            }
            Hit::Snap(id) => {
                let toggle = self.ui.sel_snapshot.as_deref() == Some(id.as_str());
                self.ui.sel_snapshot = if toggle { None } else { Some(id) };
            }
            Hit::Dest(id) => {
                self.ui.sel_dest = Some(id);
                self.ui.view = View::Destinations;
                if self.drawer.is_overlay() {
                    self.drawer.close();
                }
            }
            Hit::Policy(field, delta) => {
                self.bump_retention(doc, root, field, delta);
                self.policy_focus = PolicyField::ALL
                    .iter()
                    .position(|f| *f == field)
                    .unwrap_or(0);
            }
            Hit::Setup => {
                if self.source_path.is_some() {
                    self.pick_folder(PickPurpose::Destination);
                } else {
                    self.pick_folder(PickPurpose::Source);
                }
            }
            Hit::BackupNow => self.start_backup(),
            Hit::Verify => self.start_verify(),
            Hit::RestoreOpen => self.ui.restore_open = true,
            Hit::RestoreConfirm => self.pick_folder(PickPurpose::Restore),
            Hit::RestoreCancel => self.ui.restore_open = false,
            // The Timeline source-rail collapse / summon toggle.
            Hit::ToggleRail => self.drawer.toggle(),
            // A tap on the scrim behind an open overlay rail dismisses it.
            Hit::CloseRail => self.drawer.close(),
        }
        self.load_cache(doc, root);
    }

    fn cycle_view(&mut self, dir: i32, doc: &AutoCommit, root: &ObjId) {
        let views = &View::ALL;
        let idx = views.iter().position(|v| *v == self.ui.view).unwrap_or(0) as i32;
        let n = views.len() as i32;
        let next = ((idx + dir) % n + n) % n;
        // `next` is always in `0..n` by construction of the double-modulo above.
        if let Some(&view) = views.get(next as usize) {
            self.ui.view = view;
        }
        self.load_cache(doc, root);
    }

    fn move_snapshot_selection(&mut self, dir: i32, doc: &AutoCommit, root: &ObjId) {
        let snaps = self.catalog.timeline(self.ui.sel_source.as_deref());
        if snaps.is_empty() {
            return;
        }
        let cur = self
            .ui
            .sel_snapshot
            .as_deref()
            .and_then(|id| snaps.iter().position(|s| s.id == id));
        let next = match cur {
            Some(i) => ((i as i32 + dir).rem_euclid(snaps.len() as i32)) as usize,
            None => {
                if dir >= 0 {
                    0
                } else {
                    snaps.len() - 1
                }
            }
        };
        let Some(snap) = snaps.get(next) else {
            return;
        };
        let id = snap.id.clone();
        self.ui.sel_snapshot = Some(id);
        self.load_cache(doc, root);
    }
}

impl BackupsApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
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
        match operation {
            crate::actions::BackupOperation::Setup => self.pick_folder(PickPurpose::Source),
            crate::actions::BackupOperation::BackupNow => {
                if self.source_path.is_none() || self.destination_path.is_none() {
                    return Err("Backups are not configured on this device.".into());
                }
                self.start_backup();
            }
            crate::actions::BackupOperation::Verify { snapshot_id } => {
                if self.destination_path.is_none()
                    || !self.catalog.snapshots.iter().any(|s| s.id == snapshot_id)
                {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.ui.sel_snapshot = Some(snapshot_id);
                self.start_verify();
            }
            crate::actions::BackupOperation::Restore { snapshot_id } => {
                if self.destination_path.is_none()
                    || !self.catalog.snapshots.iter().any(|s| s.id == snapshot_id)
                {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.ui.sel_snapshot = Some(snapshot_id);
                self.pick_folder(PickPurpose::Restore);
            }
            crate::actions::BackupOperation::OpenSource { source_id } => {
                if !self
                    .catalog
                    .sources
                    .iter()
                    .any(|source| source.id == source_id)
                {
                    return Err("That source is unavailable on this device.".into());
                }
                self.ui.sel_source = Some(source_id);
                self.ui.sel_snapshot = None;
                self.ui.view = View::Sources;
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
                self.ui.sel_dest = Some(destination_id);
                self.ui.view = View::Destinations;
            }
            crate::actions::BackupOperation::OpenSnapshot { snapshot_id } => {
                if !self
                    .catalog
                    .snapshots
                    .iter()
                    .any(|snapshot| snapshot.id == snapshot_id)
                {
                    return Err("That snapshot is unavailable in this local repository.".into());
                }
                self.ui.sel_snapshot = Some(snapshot_id);
                self.ui.view = View::Timeline;
            }
            crate::actions::BackupOperation::OpenSchedule { source_id } => {
                if !self
                    .catalog
                    .sources
                    .iter()
                    .any(|source| source.id == source_id)
                {
                    return Err("That schedule's source is unavailable on this device.".into());
                }
                self.ui.sel_source = Some(source_id);
                self.ui.view = View::Schedules;
            }
        }
        Ok(true)
    }
}

impl Widget for BackupsApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::BACKUPS_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.drain_effects();
        if !self.seeded {
            // Readers fall back to the default policy. The record stays empty
            // until a retention action writes a field.
            self.seeded = true;
        }
        self.load_cache(state, path);
        // Auto-select the newest snapshot locally so the detail panel + actions
        // are visible. This is a convenience selection, never a replicated
        // document mutation.
        if self.ui.sel_snapshot.is_none() {
            let newest = self
                .catalog
                .timeline(self.ui.sel_source.as_deref())
                .first()
                .map(|s| s.id.clone());
            if let Some(id) = newest {
                self.ui.sel_snapshot = Some(id);
            }
        }
        // Dev/test hook (off by default): force a starting view / open the
        // restore sheet so each screen can be captured headlessly via
        // `BACKUPS_VIEW=destinations … --shots`.
        if let Ok(v) = std::env::var("BACKUPS_VIEW") {
            match v.as_str() {
                "sources" => {
                    self.ui.view = View::Sources;
                }
                "destinations" => {
                    self.ui.view = View::Destinations;
                }
                "schedules" => {
                    self.ui.view = View::Schedules;
                }
                _ => {}
            }
        }
        if self.shot_scenario == Some(ShotScenario::Restore) {
            self.ui.restore_open = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only fails on tree-internal bookkeeping errors, never on
        // caller input; there is none to report here.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `node` was created by `build_layout` in this same tree, so a lookup
        // miss would mean the frame cycle called these out of order.
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.render(scene, transform, text_renderer, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // The root fills the window — always paint, never cull.
        self.render(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let new_hover = self.hit_at(self.last_mouse);
                if new_hover != self.hover {
                    self.hover = new_hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hit) = self.hit_at(self.last_mouse) {
                    self.dispatch(hit, ctx.state, ctx.path);
                    *ctx.handled = true;
                } else if self.ui.restore_open {
                    // The sheet's scrim swallows the press; nothing behind a
                    // modal may activate.
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.ui.view == View::Timeline => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                // The source rail takes the wheel first: in overlay mode it
                // floats over the list, and its rows scroll only once the
                // catalog is taller than the rail (`rail_scroll_max`).
                let rail = self.drawer.sidebar_rect();
                if self.rail_scroll_max > 0.0
                    && should_paint_rail(&self.drawer, rail)
                    && rail.contains(self.last_mouse)
                {
                    self.rail_scroll = (self.rail_scroll + dy).clamp(0.0, self.rail_scroll_max);
                    *ctx.handled = true;
                } else if self.timeline_view.contains(self.last_mouse) {
                    let max = (self.timeline_content_h - self.timeline_view.height()).max(0.0);
                    self.timeline_scroll = (self.timeline_scroll + dy).clamp(0.0, max);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                self.handle_key(&key_event.logical_key, ctx.state, ctx.path, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "BackupsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("backups_app")
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one match keeps every painted hit target paired with its semantic name"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        let controls = self
            .hits
            .iter()
            .map(|(rect, hit)| {
                let (name, role, state) = match hit {
                    Hit::Tab(view) => (
                        format!("{} view", view.title()),
                        Role::RadioButton,
                        AccessState::new()
                            .focusable(true)
                            .checked(self.ui.view == *view),
                    ),
                    Hit::Source(Some(id)) => (
                        format!("Select source {id}"),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::Source(None) => (
                        "All sources".into(),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::Snap(id) => (
                        format!("Open snapshot {id}"),
                        Role::ListItem,
                        AccessState::new()
                            .focusable(true)
                            .selected(self.ui.sel_snapshot.as_deref() == Some(id)),
                    ),
                    Hit::Dest(id) => (
                        format!("Select destination {id}"),
                        Role::ListItem,
                        AccessState::new()
                            .focusable(true)
                            .selected(self.ui.sel_dest.as_deref() == Some(id)),
                    ),
                    Hit::Policy(field, delta) => (
                        format!(
                            "{} retention {}",
                            field.label(),
                            if *delta > 0 { "increase" } else { "decrease" }
                        ),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::ToggleRail => (
                        "Show backup sources".into(),
                        Role::ToggleButton,
                        AccessState::new()
                            .focusable(true)
                            .checked(self.drawer.is_open()),
                    ),
                    Hit::CloseRail => (
                        "Close backup sources".into(),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::Setup => (
                        if let Some(error) = self.repository_error.as_deref() {
                            format!(
                                "Choose another backup location. {error} {RECOVERY_SAFETY_COPY}"
                            )
                        } else {
                            "Set up backups".into()
                        },
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::BackupNow => (
                        "Back up now".into(),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::Verify => (
                        "Verify latest backup".into(),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::RestoreOpen => (
                        "Recover complete snapshot".into(),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::RestoreConfirm => (
                        "Choose an empty folder for recovery".into(),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                    Hit::RestoreCancel => (
                        "Cancel recovery".into(),
                        Role::Button,
                        AccessState::new().focusable(true),
                    ),
                };
                // A tab's accessible bounds are its painted pill, not the
                // contiguous hit cell, so neighbouring tabs report the gap
                // that is on screen.
                let bounds = if matches!(hit, Hit::Tab(_)) {
                    rect.inset(-TAB_PILL_INSET)
                } else {
                    *rect
                };
                AccessNode::new(role)
                    .with_name(name)
                    .with_state(state)
                    .with_bounds(bounds)
                    .with_action_target(format!("backups.hit:{hit:?}"))
            })
            .collect();
        Some(
            AccessNode::new(Role::Window)
                .with_name("Backups")
                .with_bounds(self.rect)
                .with_children(controls),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        if !matches!(action, A11yAction::Activate | A11yAction::ToggleCheck) {
            return false;
        }
        let hit = self
            .hits
            .iter()
            .find(|(_, hit)| format!("backups.hit:{hit:?}") == target)
            .map(|(_, hit)| hit.clone());
        if let Some(hit) = hit {
            self.dispatch(hit, state, path);
            true
        } else {
            false
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
            || self.picker.is_some()
            || self.job.is_some()
            || self.drawer.is_animating()
            || self.status_bubble.needs_frame(ui::time::now())
    }
}

impl ui::search::capability::SearchSource for BackupsApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_index.provider())]
    }
}

impl ui::resources::PublishesResources for BackupsApp {
    /// Standalone / `--shots` keeps the full hand-built door. When hosted,
    /// policy Document ops ride [`tilo_resource::publish`]; this returns only
    /// live catalog extras (never a second `backups.policy` / workspace root).
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
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records exactly one undo boundary bracketing it — the
    /// same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl BackupsApp {
    fn handle_key(&mut self, key: &Key, doc: &mut AutoCommit, root: &ObjId, handled: &mut bool) {
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.ui.restore_open {
                    self.ui.restore_open = false;
                } else {
                    self.ui.sel_snapshot = None;
                }
                self.load_cache(doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::Tab) => {
                self.cycle_view(1, doc, root);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.ui.view == View::Schedules {
                    self.policy_focus =
                        (self.policy_focus + PolicyField::ALL.len() - 1) % PolicyField::ALL.len();
                } else {
                    self.move_snapshot_selection(-1, doc, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if self.ui.view == View::Schedules {
                    self.policy_focus = (self.policy_focus + 1) % PolicyField::ALL.len();
                } else {
                    self.move_snapshot_selection(1, doc, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if self.ui.view == View::Schedules {
                    // `policy_focus` is always kept `% PolicyField::ALL.len()`.
                    if let Some(&f) = PolicyField::ALL.get(self.policy_focus) {
                        self.bump_retention(doc, root, f, -1);
                        self.load_cache(doc, root);
                    }
                } else {
                    self.cycle_view(-1, doc, root);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                if self.ui.view == View::Schedules {
                    // `policy_focus` is always kept `% PolicyField::ALL.len()`.
                    if let Some(&f) = PolicyField::ALL.get(self.policy_focus) {
                        self.bump_retention(doc, root, f, 1);
                        self.load_cache(doc, root);
                    }
                } else {
                    self.cycle_view(1, doc, root);
                }
                *handled = true;
            }
            Key::Character(s) => self.handle_character_key(s, doc, root, handled),
            _ => {}
        }
    }

    /// The character-key half of [`Self::handle_key`], split out to keep
    /// that dispatcher under the line limit. Same arms, same order.
    fn handle_character_key(
        &mut self,
        s: &str,
        doc: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        match s {
            "1" => {
                self.ui.view = View::Timeline;
                self.load_cache(doc, root);
                *handled = true;
            }
            "2" => {
                self.ui.view = View::Sources;
                self.load_cache(doc, root);
                *handled = true;
            }
            "3" => {
                self.ui.view = View::Destinations;
                self.load_cache(doc, root);
                *handled = true;
            }
            "4" => {
                self.ui.view = View::Schedules;
                self.load_cache(doc, root);
                *handled = true;
            }
            "+" | "=" if self.ui.view == View::Schedules => {
                // `policy_focus` is always kept `% PolicyField::ALL.len()`.
                if let Some(&f) = PolicyField::ALL.get(self.policy_focus) {
                    self.bump_retention(doc, root, f, 1);
                    self.load_cache(doc, root);
                }
                *handled = true;
            }
            "-" | "_" if self.ui.view == View::Schedules => {
                // `policy_focus` is always kept `% PolicyField::ALL.len()`.
                if let Some(&f) = PolicyField::ALL.get(self.policy_focus) {
                    self.bump_retention(doc, root, f, -1);
                    self.load_cache(doc, root);
                }
                *handled = true;
            }
            _ => {}
        }
    }
}

impl BackupsApp {
    fn draw_compact_backup_tooltip(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.rect.width() >= NARROW_BREAKPOINT || self.hover != Some(Hit::BackupNow) {
            return;
        }
        let Some((anchor, _)) = self
            .hits
            .iter()
            .rev()
            .find(|(_, hit)| matches!(hit, Hit::BackupNow))
        else {
            return;
        };
        ui::widget::tooltip::draw_hover_tooltip(
            scene,
            t,
            tr,
            theme,
            *anchor,
            "Back up now",
            self.rect,
        );
    }
    fn draw_compact_tab_tooltip(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if theme.text_scale < 1.5 {
            return;
        }
        let Some((anchor, Hit::Tab(view))) = self
            .hits
            .iter()
            .rev()
            .find(|(_, hit)| matches!(hit, Hit::Tab(_)))
            .filter(|(_, hit)| Some(hit) == self.hover.as_ref())
        else {
            return;
        };
        ui::widget::tooltip::draw_hover_tooltip(
            scene,
            t,
            tr,
            theme,
            *anchor,
            view.title(),
            self.rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live app fabricates nothing, the demo seeds an indexed sample catalog,
    /// and repository state (unreadable, first run, configured but empty) decides
    /// which effects and controls are offered; each case builds its own app.
    #[test]
    fn catalog_and_repository_state_decide_the_offered_controls() {
        live_constructor_starts_with_an_empty_catalog();
        demo_constructor_seeds_the_sample_catalog();
        the_sample_catalog_indexes_every_snapshot();
        unreadable_saved_repository_blocks_effects_until_reselected();
        first_run_has_a_reachable_setup_action();
        configured_empty_repository_has_no_phantom_snapshot_controls();
    }

    /// Case of `catalog_and_repository_state_decide_the_offered_controls`.
    fn live_constructor_starts_with_an_empty_catalog() {
        // The live app must not fabricate sources / snapshots / destinations.
        let app = BackupsApp::with_catalog(Catalog::empty());
        assert!(app.catalog.sources.is_empty(), "no fabricated sources");
        assert!(app.catalog.snapshots.is_empty(), "no fabricated snapshots");
        assert!(
            app.catalog.destinations.is_empty(),
            "no fabricated destinations"
        );
    }

    /// Case of `catalog_and_repository_state_decide_the_offered_controls`.
    fn unreadable_saved_repository_blocks_effects_until_reselected() {
        let mut app = BackupsApp::with_catalog(Catalog::empty());
        app.source_path = Some(PathBuf::from("/documents"));
        app.destination_path = Some(PathBuf::from("/backup-drive"));
        app.repository_error = Some("Couldn’t read the saved backup location: unavailable".into());

        assert!(
            !app.is_configured(),
            "paths alone must not advertise a usable local repository"
        );
        app.start_backup();
        assert!(
            app.job.is_none(),
            "no worker starts against an unknown repository"
        );
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|(_, kind, _)| *kind == ToastKind::Error),
            "the blocked operation is reported as an error, not success"
        );
    }

    /// Case of `catalog_and_repository_state_decide_the_offered_controls`.
    fn demo_constructor_seeds_the_sample_catalog() {
        // The --shots/demo builder keeps the populated sample dashboard.
        let app = BackupsApp::new_demo();
        assert!(!app.catalog.sources.is_empty(), "demo has sources");
        assert!(!app.catalog.snapshots.is_empty(), "demo has snapshots");
        assert!(
            !app.catalog.destinations.is_empty(),
            "demo has destinations"
        );
    }

    /// The source/destination rail is not painted while closed off-screen, scrolls
    /// under a fixed header when short, and does not scroll when tall; each case
    /// builds its own frame.
    #[test]
    fn rail_paints_and_scrolls_only_when_needed() {
        closed_overlay_rail_is_not_painted_from_its_offscreen_tail();
        short_rail_scrolls_its_rows_under_a_fixed_header();
        tall_rail_does_not_scroll();
    }

    /// Case of `rail_paints_and_scrolls_only_when_needed`.
    fn closed_overlay_rail_is_not_painted_from_its_offscreen_tail() {
        let mut drawer = SidebarDrawer::new()
            .left()
            .docked_width(RAIL_W)
            .overlay_width(RAIL_W.max(288.0))
            .breakpoint(RAIL_BREAKPOINT);
        let outer = Rect::new(20.0, 66.0, 370.0, 832.0);
        drawer.resolve(outer);
        let rail = drawer.sidebar_rect();
        assert!(drawer.reveal().abs() < f64::EPSILON);
        assert!(rail.x1 <= outer.x0);
        assert!(!should_paint_rail(&drawer, rail));
    }

    /// A short window's rail cannot show the whole catalog: its rows scroll
    /// under the fixed header band by the wheel, the band's toggle stays put,
    /// and a row that slid under the band is no longer a hit target.
    /// Case of `rail_paints_and_scrolls_only_when_needed`.
    fn short_rail_scrolls_its_rows_under_a_fixed_header() {
        let mut f = Frame::demo_sized(1280.0, 480.0);
        assert!(f.app.drawer.is_docked());
        assert!(
            f.app.rail_scroll_max > 0.0,
            "the demo catalog overflows a 480px window's rail"
        );
        let toggle = f.rect_of(|h| matches!(h, Hit::ToggleRail));
        let all_before = f.rect_of(|h| matches!(h, Hit::Source(None)));
        assert!(
            all_before.y0 >= toggle.y1,
            "rows start under the header band"
        );
        let rows_top = all_before.y0;

        let rail = f.app.drawer.sidebar_rect();
        f.wheel(rail.center(), 2.0);
        assert!(
            f.app.rail_scroll > 0.0,
            "the wheel over the rail scrolls its rows"
        );
        assert_eq!(
            f.rect_of(|h| matches!(h, Hit::ToggleRail)),
            toggle,
            "the header band does not move"
        );
        for (r, h) in &f.app.hits {
            if matches!(h, Hit::Source(_) | Hit::Dest(_)) {
                assert!(
                    r.y0 >= rows_top - 1e-6,
                    "{h:?} is clipped to the rows' band"
                );
                assert!(r.height() > 0.0 && r.width() > 0.0);
            }
        }
        if f.has_hit(|h| matches!(h, Hit::Source(None))) {
            assert!(f.rect_of(|h| matches!(h, Hit::Source(None))).y1 < all_before.y1);
        }

        // The wheel over the snapshot list scrolls the timeline, not the rail.
        let list_centre = f.app.timeline_view.center();
        let rail_scroll = f.app.rail_scroll;
        f.wheel(list_centre, 1.0);
        assert!((f.app.rail_scroll - rail_scroll).abs() < f64::EPSILON);
        // And back to the top.
        f.wheel(rail.center(), -20.0);
        assert!(f.app.rail_scroll.abs() < f64::EPSILON);
    }

    /// At 1280×800 the whole catalog fits the rail: nothing scrolls.
    /// Case of `rail_paints_and_scrolls_only_when_needed`.
    fn tall_rail_does_not_scroll() {
        let f = Frame::demo();
        assert!(f.app.rail_scroll_max.abs() < f64::EPSILON);
        assert!(f.app.rail_scroll.abs() < f64::EPSILON);
    }

    /// A headless harness that drives the **production** widget path: it draws a
    /// real frame (populating the hit map exactly as the live app does), then
    /// synthesises the pointer events a real click produces and feeds them
    /// through `handle_event`. Assertions read the widget's local navigation
    /// state while durable policy assertions continue to use `BackupsDoc`.
    struct Frame {
        app: BackupsApp,
        state: ui::State,
        taffy: TaffyTree<()>,
        node: NodeId,
        theme: Theme,
        tr: TextRenderer,
    }

    impl Frame {
        fn demo() -> Self {
            Self::demo_sized(1280.0, 800.0)
        }

        fn demo_sized(width: f64, height: f64) -> Self {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut app = BackupsApp::new_demo();
            app.rect = Rect::new(0.0, 0.0, width, height);
            let mut f = Self {
                app,
                state: ui::State::new(),
                taffy,
                node,
                theme: Theme::dark(),
                tr: TextRenderer::new(),
            };
            f.draw();
            f
        }

        /// Re-run the live frame cycle: sync from the CRDT, then draw (which
        /// repopulates `hits` from the current state).
        fn draw(&mut self) {
            self.tr.set_text_scale(self.theme.text_scale);
            self.app.sync(&mut self.state.doc, &self.state.root);
            let mut scene = Scene::new();
            self.app
                .render(&mut scene, Affine::IDENTITY, &mut self.tr, &self.theme);
        }

        /// The rect of the top-most drawn hit matching `pred` (matching the
        /// production `hit_at`, which scans back-to-front).
        fn rect_of(&self, pred: impl Fn(&Hit) -> bool) -> Rect {
            self.app
                .hits
                .iter()
                .rev()
                .find(|(_, h)| pred(h))
                .map(|(r, _)| *r)
                .expect("expected hit is present in the drawn frame")
        }

        fn has_hit(&self, pred: impl Fn(&Hit) -> bool) -> bool {
            self.app.hits.iter().any(|(_, h)| pred(h))
        }

        /// Move the pointer to `p` and press the left button through the real
        /// event path, then redraw.
        fn click(&mut self, p: Point) {
            use winit::dpi::PhysicalPosition;
            use winit::event::{DeviceId, Modifiers};
            let m = Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: PhysicalPosition::new(p.x, p.y),
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &m,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: self.node,
                    taffy: &self.taffy,
                },
            );
            self.app.handle_event(
                &WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &m,
                    state: &mut self.state.doc,
                    path: &self.state.root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: self.node,
                    taffy: &self.taffy,
                },
            );
            self.draw();
        }

        fn click_hit(&mut self, pred: impl Fn(&Hit) -> bool) {
            let c = self.rect_of(pred).center();
            self.click(c);
        }

        /// Move the pointer to `p` and turn the wheel `notches` lines
        /// (positive scrolls the content up) through the real event path,
        /// then redraw.
        fn wheel(&mut self, p: Point, notches: f32) {
            use winit::dpi::PhysicalPosition;
            use winit::event::{DeviceId, Modifiers, TouchPhase};
            let m = Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            for ev in [
                WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: PhysicalPosition::new(p.x, p.y),
                },
                WindowEvent::MouseWheel {
                    device_id: DeviceId::dummy(),
                    delta: MouseScrollDelta::LineDelta(0.0, -notches),
                    phase: TouchPhase::Moved,
                },
            ] {
                self.app.handle_event(
                    &ev,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &m,
                        state: &mut self.state.doc,
                        path: &self.state.root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: self.node,
                        taffy: &self.taffy,
                    },
                );
            }
            self.draw();
        }

        fn key(&mut self, key: &Key) {
            let mut handled = false;
            self.app
                .handle_key(key, &mut self.state.doc, &self.state.root, &mut handled);
            assert!(handled, "the restore keyboard action was handled");
            self.draw();
        }

        fn view(&self) -> View {
            self.app.ui.view
        }
    }

    /// At phone width (and 200% text) the view tabs stay clear of the backup
    /// action and keep their full semantic names; each case builds its own frame.
    #[test]
    fn phone_chrome_keeps_tabs_clear_and_fully_named() {
        phone_width_keeps_every_tab_clear_of_the_primary_action();
        phone_tabs_do_not_overlap_the_compact_backup_action();
        phone_200_percent_icon_tabs_keep_full_semantic_names();
        compact_phone_chrome_keeps_full_semantic_names();
    }

    /// At phone width the four view tabs and the primary action share a 390px
    /// bar. Every tab must stay a distinct, reachable target that ends before
    /// the action begins; with the full-width label the Schedules tab used to
    /// be painted underneath "Back up now".
    /// Case of `phone_chrome_keeps_tabs_clear_and_fully_named`.
    fn phone_width_keeps_every_tab_clear_of_the_primary_action() {
        for width in [390.0, 340.0] {
            let f = Frame::demo_sized(width, 844.0);
            let action = f.rect_of(|h| matches!(h, Hit::BackupNow));
            let tabs: Vec<Rect> = View::ALL
                .iter()
                .map(|v| f.rect_of(|h| matches!(h, Hit::Tab(t) if t == v)))
                .collect();
            assert_eq!(tabs.len(), 4);
            for (i, tab) in tabs.iter().enumerate() {
                assert!(
                    tab.x1 <= action.x0,
                    "tab {i} ({tab:?}) must end before the action ({action:?}) at {width}"
                );
                assert!(
                    tab.width() >= MIN_TAB_W,
                    "tab {i} keeps a usable target width at {width}"
                );
                for other in &tabs[i + 1..] {
                    assert!(
                        tab.intersect(*other).is_zero_area(),
                        "tabs are pairwise disjoint"
                    );
                }
            }
            assert!(
                action.x1 <= f.app.rect.x1 - PAD + 0.5,
                "action stays inside the bar gutter at {width}"
            );
        }
    }

    /// Pointer, keyboard and accessibility activation all switch views through the
    /// production dispatcher, and navigation and modal state never touch the
    /// collaborative document; each case builds its own frame.
    #[test]
    fn view_navigation_is_device_local_through_pointer_keyboard_and_accessibility() {
        clicking_a_view_tab_switches_the_dashboard_view();
        configured_repositories_keep_the_retention_view_reachable();
        accessibility_tab_activation_reuses_the_hit_dispatcher();
        navigation_and_modal_state_do_not_mutate_the_collaborative_document();
    }

    /// Case of `view_navigation_is_device_local_through_pointer_keyboard_and_accessibility`.
    fn clicking_a_view_tab_switches_the_dashboard_view() {
        let mut f = Frame::demo();
        assert_eq!(f.view(), View::Timeline, "demo opens on the timeline");
        // Click the real "Destinations" tab pill.
        f.click_hit(|h| matches!(h, Hit::Tab(View::Destinations)));
        assert_eq!(
            f.view(),
            View::Destinations,
            "clicking the tab changed the local view through the production path"
        );
        // And a second tab confirms it is not a one-shot.
        f.click_hit(|h| matches!(h, Hit::Tab(View::Schedules)));
        assert_eq!(f.view(), View::Schedules);
    }

    /// Case of `view_navigation_is_device_local_through_pointer_keyboard_and_accessibility`.
    fn configured_repositories_keep_the_retention_view_reachable() {
        let mut f = Frame::demo();
        // A real repository sets this after reading its local configuration.
        f.app.source_path = Some(PathBuf::from("/protected"));
        f.app.destination_path = Some(PathBuf::from("/backups"));
        f.draw();

        assert!(f.has_hit(|hit| matches!(hit, Hit::Tab(View::Schedules))));
        f.key(&Key::Character("4".into()));
        assert_eq!(f.view(), View::Schedules);
    }

    /// Retention steppers stay reachable and dispatch the durable action on wide
    /// and phone layouts, and stack before colliding at 200% text; each case
    /// builds its own frame.
    #[test]
    fn retention_steppers_stay_reachable_and_stack_at_large_text() {
        retention_steppers_stay_reachable_and_dispatch_on_wide_and_phone_layouts();
        large_text_retention_editor_stacks_before_controls_collide();
    }

    /// Case of `retention_steppers_stay_reachable_and_stack_at_large_text`.
    fn retention_steppers_stay_reachable_and_dispatch_on_wide_and_phone_layouts() {
        let mut f = Frame::demo();
        f.click_hit(|hit| matches!(hit, Hit::Tab(View::Schedules)));

        for width in [1280.0, 390.0] {
            f.app.rect = Rect::new(0.0, 0.0, width, 844.0);
            f.draw();
            for field in PolicyField::ALL {
                let minus = f.rect_of(
                    |hit| matches!(hit, Hit::Policy(candidate, -1) if *candidate == field),
                );
                let plus = f
                    .rect_of(|hit| matches!(hit, Hit::Policy(candidate, 1) if *candidate == field));
                assert!(minus.x1 <= plus.x0, "{field:?} stepper buttons overlap");
                assert!(
                    (minus.y0 - plus.y0).abs() < f64::EPSILON,
                    "{field:?} controls leave their row"
                );
                assert!(
                    (minus.y1 - plus.y1).abs() < f64::EPSILON,
                    "{field:?} controls differ in height"
                );
                assert!(minus.x0 >= 0.0 && plus.x1 <= width);
            }
        }

        let before = BackupsDoc::retention(&f.state.doc, &f.state.root, PolicyField::Daily);
        f.click_hit(|hit| matches!(hit, Hit::Policy(PolicyField::Daily, 1)));
        assert_eq!(
            BackupsDoc::retention(&f.state.doc, &f.state.root, PolicyField::Daily),
            before + 1,
            "the visible phone control uses the durable action path"
        );
    }

    /// Case of `retention_steppers_stay_reachable_and_stack_at_large_text`.
    fn large_text_retention_editor_stacks_before_controls_collide() {
        let mut f = Frame::demo();
        f.app.rect = Rect::new(0.0, 0.0, 880.0, 800.0);
        f.theme = Theme::dark().with_text_scale(2.0);
        f.click_hit(|hit| matches!(hit, Hit::Tab(View::Schedules)));

        let content = Rect::new(20.0, 118.0, 860.0, 732.0);
        assert!(
            BackupsApp::schedules_stack_for(content, 2.0),
            "200% type uses the single-column retention editor"
        );
        for field in PolicyField::ALL {
            let minus =
                f.rect_of(|hit| matches!(hit, Hit::Policy(candidate, -1) if *candidate == field));
            let plus =
                f.rect_of(|hit| matches!(hit, Hit::Policy(candidate, 1) if *candidate == field));
            assert!(minus.x1 <= plus.x0, "{field:?} controls overlap at 200%");
            assert!(minus.width() >= 44.0 && plus.width() >= 44.0);
        }
    }

    /// Case of `phone_chrome_keeps_tabs_clear_and_fully_named`.
    fn phone_tabs_do_not_overlap_the_compact_backup_action() {
        let mut f = Frame::demo();
        f.app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        f.draw();

        let backup = f.rect_of(|hit| matches!(hit, Hit::BackupNow));
        for (tab, _) in f
            .app
            .hits
            .iter()
            .filter(|(_, hit)| matches!(hit, Hit::Tab(_)))
        {
            assert!(
                tab.x1 <= backup.x0 || tab.x0 >= backup.x1,
                "tab {tab:?} overlaps the compact backup action {backup:?}"
            );
        }
    }

    /// Recovery points, timeline rows, unavailable-repository copy and narrow
    /// restore metadata reflow with the text scale without overlap, elision or
    /// leaking local paths; each case builds its own frame.
    #[test]
    fn recovery_surfaces_reflow_with_text_scale_without_overlap_or_elision() {
        phone_200_percent_text_reflows_every_recovery_point();
        timeline_rows_follow_the_text_scale_without_overlapping();
        unavailable_recovery_copy_reflows_without_elision_at_phone_large_text();
        narrow_restore_metadata_never_exposes_or_depends_on_a_long_local_path();
    }

    /// Case of `recovery_surfaces_reflow_with_text_scale_without_overlap_or_elision`.
    fn phone_200_percent_text_reflows_every_recovery_point() {
        let mut f = Frame::demo();
        f.app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        f.theme = Theme::dark().with_text_scale(2.0);
        f.draw();

        assert!(f.has_hit(|hit| matches!(hit, Hit::Verify)));
        assert!(f.has_hit(|hit| matches!(hit, Hit::RestoreOpen)));
        let snapshot_cards: Vec<(Rect, String)> = f
            .app
            .hits
            .iter()
            .filter_map(|(rect, hit)| match hit {
                Hit::Snap(id) => Some((*rect, id.clone())),
                _ => None,
            })
            .collect();
        assert!(
            snapshot_cards.len() >= 3,
            "large text keeps multiple recovery points pointer-selectable"
        );
        assert!(snapshot_cards.iter().all(|(card, _)| card.height() >= 44.0));
        assert!(
            snapshot_cards
                .windows(2)
                .all(|cards| cards[0].0.y1 <= cards[1].0.y0)
        );
        let verify = f.rect_of(|hit| matches!(hit, Hit::Verify));
        let restore = f.rect_of(|hit| matches!(hit, Hit::RestoreOpen));
        assert!(
            verify.x1 <= restore.x0 || restore.x1 <= verify.x0 || verify.y1 <= restore.y0,
            "large-type controls never overlap"
        );
        assert!(
            restore.y1 <= snapshot_cards[0].0.y0,
            "recovery list clears the large-text action row"
        );
        f.click(snapshot_cards[1].0.center());
        assert_eq!(
            f.app.ui.sel_snapshot.as_deref(),
            Some(snapshot_cards[1].1.as_str()),
            "the touched recovery card becomes the exact selected version"
        );
        f.key(&Key::Named(NamedKey::ArrowDown));
        assert_eq!(
            f.app.ui.sel_snapshot.as_deref(),
            Some(snapshot_cards[2].1.as_str()),
            "keyboard traversal advances to the next visible recovery point"
        );

        let tree = f
            .app
            .accessibility()
            .expect("large-text accessibility tree");
        let selectable = tree
            .children
            .iter()
            .filter(|node| node.role == Role::ListItem)
            .count();
        assert!(
            selectable >= 2,
            "multiple recovery points reach assistive tech"
        );
    }

    /// Case of `phone_chrome_keeps_tabs_clear_and_fully_named`.
    fn phone_200_percent_icon_tabs_keep_full_semantic_names() {
        let mut f = Frame::demo();
        f.app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        f.theme = Theme::dark().with_text_scale(2.0);
        f.draw();

        let tree = f
            .app
            .accessibility()
            .expect("large-text accessibility tree");
        let names: Vec<&str> = tree
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        for name in [
            "Timeline view",
            "Sources view",
            "Destinations view",
            "Schedules view",
        ] {
            assert!(
                names.contains(&name),
                "icon tab retains its full name: {names:?}"
            );
        }
    }

    /// Case of `phone_chrome_keeps_tabs_clear_and_fully_named`.
    fn compact_phone_chrome_keeps_full_semantic_names() {
        let mut f = Frame::demo();
        f.app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        f.draw();

        let tree = f.app.accessibility().expect("phone accessibility tree");
        let names: Vec<&str> = tree
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        assert!(names.contains(&"Back up now"));
        assert!(names.contains(&"Destinations view"));
        assert!(names.contains(&"Schedules view"));
    }

    /// Case of `catalog_and_repository_state_decide_the_offered_controls`.
    fn configured_empty_repository_has_no_phantom_snapshot_controls() {
        let mut f = Frame::demo();
        f.app.catalog.snapshots.clear();
        f.draw();

        assert!(f.app.timeline_content_h.abs() < f64::EPSILON);
        assert!(!f.has_hit(|hit| matches!(hit, Hit::Snap(_) | Hit::Verify | Hit::RestoreOpen)));
        assert!(f.has_hit(|hit| matches!(hit, Hit::BackupNow)));
    }

    /// Case of `view_navigation_is_device_local_through_pointer_keyboard_and_accessibility`.
    fn navigation_and_modal_state_do_not_mutate_the_collaborative_document() {
        let mut f = Frame::demo();
        f.state.doc.commit();
        let before = f.state.doc.get_heads();

        f.click_hit(|h| matches!(h, Hit::Tab(View::Destinations)));
        assert_eq!(f.view(), View::Destinations);
        assert_eq!(f.state.doc.get_heads(), before, "view is device-local");

        // Timeline exposes the restore sheet; opening it must be equally local.
        f.click_hit(|h| matches!(h, Hit::Tab(View::Timeline)));
        f.click_hit(|h| matches!(h, Hit::RestoreOpen));
        assert!(f.app.ui.restore_open);
        assert_eq!(f.state.doc.get_heads(), before, "modal is device-local");
    }

    /// Case of `view_navigation_is_device_local_through_pointer_keyboard_and_accessibility`.
    fn accessibility_tab_activation_reuses_the_hit_dispatcher() {
        let mut f = Frame::demo();
        let tree = f.app.accessibility().expect("backups exports a window");
        assert_eq!(tree.role, Role::Window);
        assert!(f.app.perform_semantic_action(
            "backups.hit:Tab(Destinations)",
            &A11yAction::Activate,
            &mut f.state.doc,
            &f.state.root,
        ));
        assert_eq!(f.view(), View::Destinations);
    }

    /// Snapshot rows, day headers and the expanded detail keep their 1× bands
    /// exactly and grow with the text scale, and the hit rect of every row is
    /// the rect it was painted at: consecutive rows never overlap.
    /// Case of `recovery_surfaces_reflow_with_text_scale_without_overlap_or_elision`.
    fn timeline_rows_follow_the_text_scale_without_overlapping() {
        let rows_of = |f: &Frame| -> Vec<Rect> {
            let mut rows: Vec<Rect> = f
                .app
                .hits
                .iter()
                .filter_map(|(r, h)| matches!(h, Hit::Snap(_)).then_some(*r))
                .collect();
            rows.sort_by(|a, b| a.y0.total_cmp(&b.y0));
            rows
        };
        let mut f = Frame::demo_sized(1280.0, 800.0);
        let one = rows_of(&f);
        assert!(one.len() > 3, "the demo timeline paints several rows");
        assert!(one.iter().all(|r| (r.height() - ROW_H).abs() < 1e-6));
        assert!((f.app.bar_h - BAR_H).abs() < 1e-6 && (f.app.tab_h - TAB_H).abs() < 1e-6);

        f.theme = Theme::dark().with_text_scale(2.0);
        f.draw();
        let two = rows_of(&f);
        let (_, h13) = f.tr.measure("Hg", 13.0, None);
        let (_, h10_5) = f.tr.measure("Hg", 10.5, None);
        let want = ROW_H.max(f64::from(h13) + f64::from(h10_5) - 1.0 + 16.0);
        assert!(want > ROW_H, "2× type needs more than the 1× row");
        assert!(two.iter().all(|r| (r.height() - want).abs() < 1e-6));
        for pair in two.windows(2) {
            assert!(pair[1].y0 >= pair[0].y1, "rows never overlap at 2×");
        }
        assert!(f.app.bar_h > BAR_H && f.app.tab_h > TAB_H);
        // The first row starts under the (taller) top bar and content chrome.
        assert!(two[0].y0 >= f.app.bar_h + 14.0);
    }

    /// Real storage effects are reachable from the timeline, and the restore
    /// sheet uses the shared modal frame, swallows the scrim and exposes only its
    /// truthful controls; each case builds its own frame.
    #[test]
    fn restore_sheet_offers_only_truthful_modal_storage_controls() {
        real_storage_effects_are_reachable_from_the_timeline();
        restore_sheet_uses_the_shared_modal_frame_and_swallows_the_scrim();
    }

    /// The restore sheet is framed by the shared modal chrome: its two
    /// controls sit on one action line inside the window, and while it is
    /// open nothing behind the scrim can activate.
    /// Case of `restore_sheet_offers_only_truthful_modal_storage_controls`.
    fn restore_sheet_uses_the_shared_modal_frame_and_swallows_the_scrim() {
        let mut f = Frame::demo();
        // Where the Sources tab is painted; the sheet does not move it.
        let tab = f.rect_of(|hit| matches!(hit, Hit::Tab(View::Sources)));
        f.click_hit(|hit| matches!(hit, Hit::RestoreOpen));
        assert!(f.app.ui.restore_open);
        let confirm = f.rect_of(|hit| matches!(hit, Hit::RestoreConfirm));
        let cancel = f.rect_of(|hit| matches!(hit, Hit::RestoreCancel));
        assert!((confirm.y0 - cancel.y0).abs() < 1e-6 && (confirm.y1 - cancel.y1).abs() < 1e-6);
        assert!(cancel.x1 <= confirm.x0, "Cancel leads the primary action");
        assert!(f.app.rect.contains(confirm.center()) && f.app.rect.contains(cancel.center()));
        assert!(
            confirm.height() >= 32.0,
            "actions are real buttons, not badges"
        );
        // While the sheet is open nothing behind the scrim is exported: not
        // for hit-testing and not to a screen reader.
        assert!(
            !f.has_hit(|hit| matches!(hit, Hit::Tab(_) | Hit::Snap(_) | Hit::Source(_))),
            "controls behind the scrim are not exported while the sheet is open"
        );

        // A press on a view tab behind the scrim is swallowed.
        let before = f.view();
        f.click(tab.center());
        assert_eq!(
            f.view(),
            before,
            "the scrim swallows presses behind the sheet"
        );
        assert!(
            f.app.ui.restore_open,
            "a scrim press does not dismiss a recovery sheet"
        );

        // Cancel is still the visible way out.
        f.click(cancel.center());
        assert!(!f.app.ui.restore_open);
    }

    /// Case of `restore_sheet_offers_only_truthful_modal_storage_controls`.
    fn real_storage_effects_are_reachable_from_the_timeline() {
        let mut f = Frame::demo();
        assert!(f.has_hit(|hit| matches!(hit, Hit::BackupNow)));
        assert!(f.has_hit(|hit| matches!(hit, Hit::Verify)));
        assert!(f.has_hit(|hit| matches!(hit, Hit::RestoreOpen)));
        f.click_hit(|hit| matches!(hit, Hit::RestoreOpen));
        assert!(f.has_hit(|hit| matches!(hit, Hit::RestoreConfirm)));
        assert!(f.has_hit(|hit| matches!(hit, Hit::RestoreCancel)));
        assert_eq!(
            f.app
                .hits
                .iter()
                .filter(|(_, hit)| matches!(hit, Hit::RestoreConfirm | Hit::RestoreCancel))
                .count(),
            2,
            "the sheet has only its truthful choose-folder and cancel controls"
        );
        let a11y = f.app.accessibility().expect("restore accessibility tree");
        let names: Vec<&str> = a11y
            .children
            .iter()
            .filter_map(|node| node.name.as_deref())
            .collect();
        assert!(names.contains(&"Choose an empty folder for recovery"));
        assert!(names.contains(&"Cancel recovery"));
        assert!(
            !names.iter().any(|name| name.contains("Choose files")),
            "screen readers receive the same whole-snapshot contract as sighted users"
        );
        let resources = ui::resources::resources(&f.app, &f.state.doc);
        assert_eq!(resources.len(), 2, "policy and catalog are driveable");
        assert!(
            resources
                .iter()
                .any(|resource| resource.kind == "backups.policy")
        );
        assert!(
            resources
                .iter()
                .any(|resource| resource.kind == "backups.workspace")
        );
        assert!(
            tilo_actions::flatten(&resources)
                .iter()
                .any(|entry| entry.action.kind == tilo_actions::OperationKind::Effect)
        );
    }

    /// Case of `catalog_and_repository_state_decide_the_offered_controls`.
    fn first_run_has_a_reachable_setup_action() {
        let mut app = BackupsApp::with_catalog(Catalog::empty());
        app.rect = Rect::new(0.0, 0.0, 1000.0, 700.0);
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let mut scene = Scene::new();
        app.render(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
        );
        assert!(app.hits.iter().any(|(_, hit)| matches!(hit, Hit::Setup)));
    }

    /// Case of `recovery_surfaces_reflow_with_text_scale_without_overlap_or_elision`.
    fn unavailable_recovery_copy_reflows_without_elision_at_phone_large_text() {
        const ERROR: &str = "Couldn’t read the saved backup location: the drive is unavailable.";
        let mut f = Frame::demo();
        f.app.catalog = Catalog::empty();
        f.app.source_path = Some(PathBuf::from("/documents"));
        f.app.destination_path = Some(PathBuf::from("/backup-drive"));
        f.app.repository_error = Some(ERROR.into());
        f.app.rect = Rect::new(0.0, 0.0, 520.0, 820.0);
        f.theme = Theme::dark().with_text_scale(2.0);
        f.tr.set_text_scale(2.0);
        f.draw();

        let max_copy_width = 480.0 - 48.0 * 2.0;
        for (copy, size) in [(ERROR, 12.5), (RECOVERY_SAFETY_COPY, 11.5)] {
            let lines = wrap_recovery_copy(copy, &mut f.tr, size, max_copy_width);
            assert_eq!(
                lines.join(" "),
                copy,
                "every recovery word remains available at 200% text"
            );
            assert!(
                lines.iter().all(|line| !line.contains('…')
                    && f64::from(f.tr.measure(line, size, None).0) <= max_copy_width),
                "each recovery line fits without clipping or ellipsis: {lines:?}"
            );
        }

        let setup = f.rect_of(|hit| matches!(hit, Hit::Setup));
        assert!(setup.height() >= 44.0 && setup.x0 >= 0.0 && setup.x1 <= 520.0);
        assert!(setup.y0 >= 0.0 && setup.y1 <= 820.0);
        for (other, hit) in &f.app.hits {
            if !matches!(hit, Hit::Setup) {
                assert!(
                    setup.x1 <= other.x0
                        || other.x1 <= setup.x0
                        || setup.y1 <= other.y0
                        || other.y1 <= setup.y0,
                    "the recovery target must remain disjoint from {hit:?}"
                );
            }
        }

        let recovery_name = f
            .app
            .accessibility()
            .expect("recovery accessibility tree")
            .children
            .into_iter()
            .find(|node| node.action_target.as_deref() == Some("backups.hit:Setup"))
            .and_then(|node| node.name)
            .expect("recovery action has an accessible name");
        assert!(recovery_name.contains(ERROR));
        assert!(recovery_name.contains(RECOVERY_SAFETY_COPY));
        assert!(!recovery_name.contains('…'));
    }

    /// Case of `catalog_and_repository_state_decide_the_offered_controls`.
    fn the_sample_catalog_indexes_every_snapshot() {
        // Demo/shots data is populated; the live catalog stays empty.
        let demo = Catalog::sample();
        assert!(
            demo.snapshots.iter().all(|s| !s.files.is_empty()),
            "every sample snapshot carries an indexed listing"
        );
        assert!(
            Catalog::empty().snapshots.is_empty(),
            "the live catalog has no snapshots to fabricate files for"
        );
    }

    /// Case of `recovery_surfaces_reflow_with_text_scale_without_overlap_or_elision`.
    fn narrow_restore_metadata_never_exposes_or_depends_on_a_long_local_path() {
        let mut catalog = Catalog::sample();
        let source = catalog.sources.first_mut().expect("sample source");
        source.path =
            "/an/intentionally/very/long/device/local/path/that/cannot/fit/a/phone/header".into();
        let snapshot = catalog.snapshots.first().expect("sample snapshot");
        let source = catalog.source(&snapshot.source_id);
        let metadata = BackupsApp::narrow_restore_metadata(source, snapshot);

        assert!(metadata.starts_with("From "));
        assert!(!metadata.contains("/an/intentionally"));
        let mut renderer = TextRenderer::new();
        let fitted = renderer.elide_to_width(&metadata, 11.5, 342.0);
        assert!(renderer.measure(&fitted, 11.5, None).0 <= 342.0);
    }

    #[test]
    fn a_fresh_backup_record_stays_empty_until_a_retention_action() {
        use automerge::ReadDoc;
        use ui::widget::Widget;
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
