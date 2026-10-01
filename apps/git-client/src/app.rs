//! Root widget for the git client.
//!
//! Owns the in-memory [`RepoModel`], the currently-shown diff, the active
//! screen and all transient selection / scroll state. Durable preferences
//! live in the CRDT via [`Settings`]. The widget draws everything itself
//! into the Vello scene — a top app bar with a screen switcher, then the
//! active screen (changes / history / panels). Events are routed by
//! hit-testing the regions each screen emits.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{ButtonFace, FlapState, SplitLayout, TextInput, TextInputKind, Widget};

use crate::changes::{ChangesColumn, ChangesHit};
use crate::diffview::{self, DiffView, HunkInteraction};
use crate::draw;
use crate::graph::{self, GraphLayout};
use crate::history::{self, HistoryView};
use crate::model::{ChangeArea, DiffKey, DiffMap, FileChange, RepoModel};
use crate::panels::{BranchAction, PanelHit, PanelsView, RemoteAction};
use crate::settings::{DiffLayout, Settings};

/// Which embedded text field currently holds keyboard focus. Text editing is
/// delegated to the toolkit's [`TextInput`] (cursor, selection, word-nav,
/// clipboard); this tracks which one keys route to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    /// The commit-message summary field.
    Commit,
    /// The new-branch name field on the Branches screen.
    Branch,
    /// The repository-path field on the empty "open a repository" screen.
    Open,
}

/// Host event resources retained while one event is routed through embedded
/// text inputs and the app's own geometry handlers.
struct InputEventCtx<'a> {
    event: &'a WindowEvent,
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: &'a mut AutoCommit,
    path: &'a ObjId,
    taffy: &'a TaffyTree<()>,
}

/// Which top-level screen is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Changes,
    History,
    Panels,
}

/// A destructive discard action armed for confirmation, scoped to the file
/// currently shown in the diff pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiscardTarget {
    /// Revert the whole file's worktree changes (or delete an untracked file).
    File,
    /// Revert a single hunk of the file's worktree changes.
    Hunk(usize),
}

impl Screen {
    /// Whether this screen has a primary left list column that adapts into an
    /// overlay flap on a narrow window. The Changes (file list) and History
    /// (commit list) screens are master-detail; Branches & Remotes is a
    /// full-width dashboard with no collapsible list.
    fn has_sidebar(self) -> bool {
        matches!(self, Screen::Changes | Screen::History)
    }
}

const TABS: [(Screen, &str); 3] = [
    (Screen::Changes, "Changes"),
    (Screen::History, "History"),
    (Screen::Panels, "Branches & Remotes"),
];

const APPBAR_H: f64 = 56.0;
/// Second app-bar row that hosts the screen tabs when the title, tab group and
/// settings cluster cannot share the 56px row (narrow windows). Nothing is
/// hidden or overwritten: the title keeps `TITLE_MIN_W`, the settings stay on
/// the first row and the tabs move underneath.
const APPBAR_TAB_ROW_H: f64 = 44.0;
/// Horizontal app-bar gutter: the title's left inset and the cluster's right
/// inset are the same value so the bar reads as one composed row.
const APPBAR_GUTTER: f64 = 20.0;
/// Width the overlay-mode hamburger occupies before the title starts.
const HAMBURGER_SLOT: f64 = 44.0;
/// Narrowest title block the one-row app bar may leave before it wraps.
const TITLE_MIN_W: f64 = 96.0;
/// Height shared by every app-bar control (tabs, segmented control, syntax
/// toggle, hamburger) so they sit on one centre line.
const APPBAR_CONTROL_H: f64 = 32.0;
/// Gap between the tab group and the settings cluster, and between the two
/// settings controls.
const APPBAR_CLUSTER_GAP: f64 = 12.0;
/// Fractional app-bar text sizes. Whole-number sizes (15 px Bold title, 14 px
/// branch pill, 13 px tabs, 12 px settings, 13/12 px diff header) stay inline
/// at their call sites — the F1 spacing ratchet counts named whole-number
/// `f32` consts and these files carry none to spare.
const SUBTITLE_FONT: f32 = 11.5;
const DIFF_DISCARD_FONT: f32 = 11.5;

/// App-bar band and control heights measured from the renderer the bar paints
/// with: the 1× constants until the text scale outgrows them, then the
/// measured lines plus their padding, so the title, subtitle, pill, tabs and
/// settings grow together instead of the subtitle overprinting the title.
/// `Default` is the bar's 1× rhythm: 9 px above the title, 3 px to the
/// subtitle (9 + 18.75 + 3 + 14.4 + 9 ≤ 56), 7 px around a tab label inside a
/// 32 px control and 6 px around a control inside its row.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BarMetrics {
    /// Height of the title row (title + subtitle stack).
    row_h: f64,
    /// Height of the wrapped tab row.
    tab_row_h: f64,
    /// Height of every bar control (tabs, segmented switch, toggle, hamburger).
    control_h: f64,
    /// Measured title and subtitle line heights.
    title_h: f64,
    subtitle_h: f64,
    /// Vertical rhythm: title block padding, title→subtitle gap, tab label
    /// padding inside a control, control padding inside a row.
    title_pad: f64,
    title_gap: f64,
    control_pad: f64,
    row_pad: f64,
}

impl Default for BarMetrics {
    fn default() -> Self {
        Self {
            row_h: APPBAR_H,
            tab_row_h: APPBAR_TAB_ROW_H,
            control_h: APPBAR_CONTROL_H,
            title_h: 18.75,
            subtitle_h: 14.375,
            title_pad: 9.0,
            title_gap: 3.0,
            control_pad: 7.0,
            row_pad: 6.0,
        }
    }
}

/// What the diff-pane header shows about the selected file, copied out of the
/// diff so the header can paint while `self` is mutated.
struct DiffHeaderInfo {
    old_path: Option<String>,
    path: String,
    additions: u32,
    deletions: u32,
}

/// Measured widths of the app-bar tabs: each tab's cell, and the whole run
/// including the 6px gaps between cells.
struct TabWidths {
    cells: Vec<f64>,
    total: f64,
}

/// Measured widths of the two app-bar settings controls: the Unified/Split
/// segmented control (both halves as wide as the wider label) and the
/// Syntax toggle, each with symmetric label padding.
#[derive(Clone, Copy, Debug)]
struct SettingsWidths {
    segmented: f64,
    syntax: f64,
}

impl SettingsWidths {
    fn measure(tr: &mut TextRenderer) -> Self {
        let label_w = |tr: &mut TextRenderer, label: &str| {
            f64::from(tr.measure_weighted(label, 12.0, None, TextWeight::Medium).0)
        };
        let half = label_w(tr, "Unified").max(label_w(tr, "Split")) + 24.0;
        Self {
            segmented: half * 2.0 + 6.0,
            syntax: label_w(tr, "Syntax") + 24.0,
        }
    }

    /// Width of the whole cluster, the gap between the two controls included.
    fn total(self) -> f64 {
        self.segmented + APPBAR_CLUSTER_GAP + self.syntax
    }
}

impl BarMetrics {
    fn measure(tr: &mut TextRenderer) -> Self {
        let one = Self::default();
        let title_h = f64::from(tr.measure_weighted("Ag", 15.0, None, TextWeight::Bold).1);
        let subtitle_h = f64::from(tr.measure("Ag", SUBTITLE_FONT, None).1);
        let tab_h = f64::from(tr.measure_weighted("Ag", 13.0, None, TextWeight::Medium).1);
        let control_h = (tab_h + 2.0 * one.control_pad).max(APPBAR_CONTROL_H);
        let stack = one.title_pad + title_h + one.title_gap + subtitle_h + one.title_pad;
        let row_h = stack.max(control_h + 2.0 * one.row_pad).max(APPBAR_H);
        let tab_row_h = (control_h + 2.0 * one.row_pad).max(APPBAR_TAB_ROW_H);
        Self {
            row_h,
            tab_row_h,
            control_h,
            title_h,
            subtitle_h,
            ..one
        }
    }
}
const CHANGES_COL_W: f64 = 320.0;

/// Below this allocated width (logical px) the primary list column stops
/// fitting comfortably beside a readable detail pane, so it collapses into an
/// overlay flap. Chosen so the 320px list + a usable diff/detail pane keep
/// breathing room above the breakpoint and the app bar's tab + settings
/// clusters still fit.
const NARROW_BREAKPOINT: f64 = 760.0;

/// Width of the overlay flap when fully revealed on a narrow window. Matches
/// the docked changes column so the list looks identical in both layouts.
const FLAP_W: f64 = CHANGES_COL_W;

/// One owned repository operation. Completion stays available until its action
/// invocation consumes it, even when a normal UI sync receives it first.
struct RemoteJob {
    receiver: Receiver<Result<(), String>>,
    result: Option<Result<(), String>>,
    invocation: Option<tilo_actions::InvocationId>,
    progress_sent: bool,
    /// Palette label while the transfer is in flight (live hosted extra).
    label: String,
}

pub struct GitClientApp {
    model: RepoModel,
    diffs: DiffMap,
    graph_layout: GraphLayout,

    /// Framework-owned action dispatch (the only action-dispatch state the app
    /// holds). Every durable view-preference toggle dispatches its
    /// [`GitClientAction`](crate::actions::GitClientAction) through this: the
    /// host brackets the undo boundary, encodes and applies through the executor
    /// (so it replays byte-identically on peers), and remembers the last action
    /// for the affordance == `ActionSpec` covenant test.
    actions: crate::resource::ActionSink,

    /// No repository is bound and none was reachable at launch — the app shows
    /// the honest "open a repository" empty state rather than any placeholder
    /// data. Cleared once a repo is opened (via the path field or at startup).
    awaiting_repo: bool,
    /// Which embedded text field holds focus, if any.
    focus: Option<Field>,
    /// Commit-message summary field (toolkit `TextInput`, local mode).
    commit_input: TextInput,
    /// New-branch name field (toolkit `TextInput`, local mode).
    branch_input: TextInput,
    /// Repository-path field on the empty-state screen (toolkit `TextInput`).
    open_input: TextInput,
    /// Hit rects for the empty-state open field + button.
    open_input_rect: Rect,
    open_button_rect: Rect,

    /// Path the repository was opened from, when backed by a real on-disk
    /// repo. `None` for the bundled sample (the `--shots` path), in which
    /// case actions fall back to the in-memory model mutation so the
    /// screenshot still demonstrates the flow. When `Some`, every action
    /// drives `git2` against the real repo and re-reads the model.
    repo_path: Option<std::path::PathBuf>,
    /// Last action error, surfaced in the UI (e.g. "no identity set").
    last_error: Option<String>,
    remote_job: Option<RemoteJob>,

    settings: Settings,
    /// Fingerprint of the last shared-repo projection written to the document.
    shared_repo_fingerprint: Option<String>,
    /// Set once after the first sync seeds the settings into the doc.
    settings_seeded: bool,
    /// An initial preference was forced at construction (the `GIT_CLIENT_SPLIT`
    /// screenshot path), so the first sync keeps it rather than adopting the
    /// document's persisted value. Interactive toggles never set this — they go
    /// through the action executor, which writes the document directly.
    settings_forced: bool,
    /// Cached hit rects for the app-bar settings controls, set during draw.
    settings_seg: Rect,
    settings_syn: Rect,
    /// Last painted subtitle, shared with the native live text node.
    status_rect: Rect,
    status_text: String,

    screen: Screen,
    rect: Rect,

    // --- Changes screen state ---
    selected_change: Option<(ChangeArea, usize)>,
    changes_scroll: f64,
    diff_scroll: f64,
    hover_hunk: Option<usize>,
    /// A discard action armed for confirmation on the selected file: the next
    /// click on the same (now red) affordance performs it; any other
    /// interaction disarms. Guards the destructive worktree revert behind a
    /// deliberate second click.
    armed_discard: Option<DiscardTarget>,

    // --- History screen state ---
    selected_commit: usize,
    history_scroll: f64,
    detail_scroll: f64,

    // --- Panels screen state ---
    hover_action: Option<RemoteAction>,
    /// Hit rects for the Panels "new branch" input + create button.
    new_branch_input: Rect,
    new_branch_button: Rect,

    // --- Responsive sidebar (the primary list column) ---
    /// Eased open/closed state of the primary list column, shared with every
    /// other adaptive app via the toolkit kernel. Open when docked (wide),
    /// closed overlay flap when narrow, summoned by the hamburger.
    flap: FlapState,
    /// Wall-clock of the last flap animation tick, so the eased slide is
    /// frame-rate independent.
    last_flap_tick: Instant,
    /// Hit rect for the app-bar hamburger toggle (overlay mode only).
    hamburger_rect: Rect,

    last_mouse: Point,

    // Click regions emitted by the last draw, used for hit-testing the
    // next click. Boxed closures keep geometry out of the event handler.
    change_hits: Vec<Box<dyn Fn(Point) -> Option<ChangesHit>>>,
    change_semantic_hits: Vec<(Rect, ChangesHit)>,
    diff_hunk_buttons: Vec<diffview::HunkButton>,
    /// Row metrics the diff painted last, so wheel scrolling clamps to the
    /// same pitch the rows use at the current text scale.
    diff_metrics: diffview::Metrics,
    /// Measured diff-pane header height from the last draw.
    diff_header_h: f64,
    /// Hit rect for the diff-header "Discard file" button (worktree side only).
    discard_file_rect: Rect,
    history_rows: Vec<Rect>,
    /// Row metrics the commit list painted last (see `diff_metrics`).
    history_metrics: history::Metrics,
    panel_hits: Vec<Box<dyn Fn(Point) -> Option<PanelHit>>>,
    panel_semantic_hits: Vec<(Rect, PanelHit)>,
    tab_rects: Vec<(Screen, Rect)>,
    /// Measured app-bar height from the last draw: one row, or two when the
    /// tabs wrap under the title on a narrow window. Body, scroll and hit
    /// computations read this rather than the one-row constant.
    appbar_h: f64,
    /// Title block (name, branch pill, subtitle) from the last draw, so tests
    /// can prove it never runs under the tab group or the settings cluster.
    title_rect: Rect,
}

impl Default for GitClientApp {
    /// Equivalent to [`GitClientApp::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl GitClientApp {
    /// Build the app, loading the repository discovered from the current
    /// directory (or `GIT_CLIENT_REPO`). Opens the empty repository picker
    /// when no repository is reachable.
    ///
    /// Setting `GIT_CLIENT_SAMPLE=1` forces the bundled fixture regardless
    /// of any reachable repository — the headless screenshot path uses
    /// this so the capture is a fully populated, deterministic state
    /// (rename + delete + new file across all three areas, a multi-lane
    /// commit graph) rather than whatever happens to sit around the CWD.
    pub fn new() -> Self {
        let force_sample =
            std::env::var("GIT_CLIENT_SAMPLE").is_ok_and(|v| v != "0" && !v.is_empty());
        if force_sample {
            let (model, diffs) = crate::sample::model();
            return Self::from_model(model, diffs);
        }
        if let Some((path, model, diffs)) = Self::load_repo() {
            let mut app = Self::from_model(model, diffs);
            app.repo_path = Some(path);
            app
        } else {
            // No reachable repository — show the honest "open a repository"
            // empty state rather than any placeholder data. The path field
            // is pre-filled with the current directory as a convenience.
            let mut app = Self::from_model(RepoModel::default(), DiffMap::new());
            app.awaiting_repo = true;
            if let Ok(cwd) = std::env::current_dir() {
                app.open_input.set_text(&cwd.to_string_lossy());
            }
            app
        }
    }

    /// Open a real repository discovered from `path` and build the app
    /// bound to it, so every action drives `git2` and re-reads. Returns
    /// `None` when no repository is reachable. Used by the live app's
    /// `GIT_CLIENT_REPO` path and by integration tests that need to drive
    /// the production actions against a temp repo.
    pub fn open_at(path: &std::path::Path) -> Option<Self> {
        let anchor = crate::repo::workdir(path).unwrap_or_else(|| path.to_path_buf());
        let (model, diffs) = crate::repo::load(path)?;
        let mut app = Self::from_model(model, diffs);
        app.repo_path = Some(anchor);
        Some(app)
    }

    /// Build the honest empty "open a repository" state with no repo bound —
    /// the same state the live app shows when no repository is reachable.
    /// Exposed so integration tests can drive the open flow.
    #[doc(hidden)]
    pub fn open_empty() -> Self {
        let mut app = Self::from_model(RepoModel::default(), DiffMap::new());
        app.awaiting_repo = true;
        app
    }

    /// `--shots --scenario`: open a screen directly so each one can be
    /// reviewed from a single offscreen frame.
    pub fn shot_screen(&mut self, screen: Screen) {
        self.switch_screen(screen);
    }

    /// `--shots --scenario split`: the side-by-side diff layout, pinned so
    /// the document sync does not fold it back to the stored preference.
    pub fn shot_split_diff(&mut self) {
        self.settings.diff_layout = DiffLayout::SideBySide;
        self.settings_forced = true;
    }

    /// `--shots --scenario commit`: a typed commit summary so the commit box
    /// and its primary action render in their enabled state.
    pub fn shot_commit_message(&mut self, message: &str) {
        self.commit_input.set_text(message);
    }

    /// `--shots --scenario discard`: the two-click discard confirmation
    /// armed on the selected file, so the danger state is capturable.
    pub fn shot_arm_discard(&mut self) {
        self.armed_discard = Some(DiscardTarget::File);
    }

    /// Construct from an explicit model — used by the sample path and tests.
    pub fn from_model(mut model: RepoModel, diffs: DiffMap) -> Self {
        let graph_layout = graph::layout(&mut model.commits);
        // Pre-select the first changed file so the diff pane is populated.
        let selected_change = first_change(&model);
        // Allow the headless capture to open a specific screen so each
        // screen can be reviewed; defaults to Changes for the live app.
        let screen = match std::env::var("GIT_CLIENT_SCREEN").as_deref() {
            Ok("history") => Screen::History,
            Ok("panels") => Screen::Panels,
            _ => Screen::Changes,
        };
        let mut settings = Settings::default();
        let split_forced = std::env::var("GIT_CLIENT_SPLIT").is_ok();
        if split_forced {
            settings.diff_layout = DiffLayout::SideBySide;
        }
        // The primary list column defaults open when docked. The headless
        // narrow capture sets `GIT_CLIENT_FLAP_OPEN` so the single offscreen
        // frame shows the overlay flap fully revealed over its scrim rather
        // than the closed default a fresh narrow window opens to.
        let mut flap = FlapState::open_when_docked();
        if std::env::var("GIT_CLIENT_FLAP_OPEN").is_ok_and(|v| v != "0" && !v.is_empty()) {
            flap.snap(true);
        }
        let commit_input = TextInput::local()
            .with_id("git.commit-message")
            .with_accessible_name("Commit message")
            .with_placeholder("Summary of changes…")
            .with_font_size(13.0);
        let branch_input = TextInput::local()
            .with_id("git.branch-name")
            .with_accessible_name("New branch name")
            .with_kind(TextInputKind::Name)
            .with_placeholder("new branch name…")
            .with_font_size(12.5);
        let open_input = TextInput::local()
            .with_id("git.repository-path")
            .with_accessible_name("Repository path")
            .with_kind(TextInputKind::Path)
            .with_placeholder("/path/to/repository")
            .with_font_size(14.0)
            .with_mono();
        Self {
            actions: crate::resource::ActionSink::default(),
            flap,
            last_flap_tick: ui::time::now(),
            hamburger_rect: Rect::ZERO,
            model,
            diffs,
            graph_layout,
            awaiting_repo: false,
            focus: None,
            commit_input,
            branch_input,
            open_input,
            open_input_rect: Rect::ZERO,
            open_button_rect: Rect::ZERO,
            repo_path: None,
            last_error: None,
            remote_job: None,
            settings,
            shared_repo_fingerprint: None,
            settings_seeded: false,
            settings_forced: split_forced,
            settings_seg: Rect::ZERO,
            settings_syn: Rect::ZERO,
            status_rect: Rect::ZERO,
            status_text: String::new(),
            screen,
            rect: Rect::ZERO,
            selected_change,
            changes_scroll: 0.0,
            diff_scroll: 0.0,
            hover_hunk: None,
            armed_discard: None,
            selected_commit: 0,
            history_scroll: 0.0,
            detail_scroll: 0.0,
            hover_action: None,
            new_branch_input: Rect::ZERO,
            new_branch_button: Rect::ZERO,
            last_mouse: Point::ZERO,
            change_hits: Vec::new(),
            change_semantic_hits: Vec::new(),
            diff_hunk_buttons: Vec::new(),
            diff_metrics: diffview::Metrics::default(),
            diff_header_h: 36.0,
            discard_file_rect: Rect::ZERO,
            history_rows: Vec::new(),
            history_metrics: history::Metrics::default(),
            panel_hits: Vec::new(),
            panel_semantic_hits: Vec::new(),
            tab_rects: Vec::new(),
            appbar_h: APPBAR_H,
            title_rect: Rect::ZERO,
        }
    }

    // --- Test-facing drivers --------------------------------------------
    // These expose the *production* action methods so integration tests can
    // drive the exact code path the UI uses, then assert on the real repo.
    // They are thin pass-throughs — no test-only logic lives behind them.

    /// Drive the production "stage/unstage this file" action by path.
    #[doc(hidden)]
    pub fn test_toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        self.toggle_stage(area, idx);
    }
    /// Drive the production "stage all" action.
    #[doc(hidden)]
    pub fn test_stage_all(&mut self) {
        self.stage_all();
    }
    /// Drive the production "unstage all" action.
    #[doc(hidden)]
    pub fn test_unstage_all(&mut self) {
        self.unstage_all();
    }
    /// Set the commit message and drive the production commit action.
    #[doc(hidden)]
    pub fn test_commit(&mut self, message: &str) {
        self.commit_input.set_text(message);
        self.do_commit();
    }
    /// Drive the production per-hunk stage/unstage action for the selected file.
    #[doc(hidden)]
    pub fn test_stage_hunk(&mut self, hunk: usize) {
        self.stage_hunk(hunk);
    }
    /// Drive the production branch action (checkout / delete / stash-apply).
    #[doc(hidden)]
    pub fn test_branch_action(&mut self, action: BranchAction) {
        self.apply_branch_action(action);
    }
    /// Drive the production "create branch" action from a name.
    #[doc(hidden)]
    pub fn test_create_branch(&mut self, name: &str) {
        self.branch_input.set_text(name);
        self.create_new_branch();
    }
    /// Drive the production "discard file" action by area + index.
    #[doc(hidden)]
    pub fn test_discard_file(&mut self, area: ChangeArea, idx: usize) {
        self.discard_file(area, idx);
    }
    /// Drive the production "discard hunk" action for the selected file.
    #[doc(hidden)]
    pub fn test_discard_hunk(&mut self, hunk: usize) {
        self.discard_hunk(hunk);
    }
    /// Whether the app is showing the empty "open a repository" state.
    #[doc(hidden)]
    pub fn test_awaiting_repo(&self) -> bool {
        self.awaiting_repo
    }
    /// Drive the empty-state "open this path" action.
    #[doc(hidden)]
    pub fn test_open_path(&mut self, path: &str) {
        self.open_input.set_text(path);
        self.open_repo_from_input();
    }
    /// Select a file in a given area + index (mirrors a list click).
    #[doc(hidden)]
    pub fn test_select(&mut self, area: ChangeArea, idx: usize) {
        self.selected_change = Some((area, idx));
    }
    /// The current `head` shorthand from the live model.
    #[doc(hidden)]
    pub fn test_head(&self) -> &str {
        &self.model.head
    }
    /// Counts of (staged, unstaged, untracked) entries in the live model.
    #[doc(hidden)]
    pub fn test_change_counts(&self) -> (usize, usize, usize) {
        (
            self.model.staged.len(),
            self.model.unstaged.len(),
            self.model.untracked.len(),
        )
    }
    /// The live model's commit summaries, newest first.
    #[doc(hidden)]
    pub fn test_commit_summaries(&self) -> Vec<String> {
        self.model
            .commits
            .iter()
            .map(|c| c.summary.clone())
            .collect()
    }
    /// The live model's local + remote branch names.
    #[doc(hidden)]
    pub fn test_branch_names(&self) -> Vec<String> {
        self.model.branches.iter().map(|b| b.name.clone()).collect()
    }
    /// The last action error message, if any.
    #[doc(hidden)]
    pub fn test_last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn load_repo() -> Option<(std::path::PathBuf, RepoModel, DiffMap)> {
        let path = std::env::var("GIT_CLIENT_REPO").map_or_else(
            |_| std::env::current_dir().unwrap_or_else(|_| ".".into()),
            std::path::PathBuf::from,
        );
        // Anchor actions on the working directory so later opens are stable
        // even if the CWD changes; fall back to the given path when bare.
        let anchor = crate::repo::workdir(&path).unwrap_or_else(|| path.clone());
        let (model, diffs) = crate::repo::load(&path)?;
        Some((anchor, model, diffs))
    }

    /// Re-read the live repository into the model after a mutation, so the
    /// UI reflects the *actual* on-disk state (proving the op took effect).
    /// A no-op for the sample-backed app. Preserves the selected screen and
    /// clamps stale selections.
    fn reload(&mut self) {
        let Some(path) = self.repo_path.clone() else {
            return;
        };
        if let Some((model, diffs)) = crate::repo::load(&path) {
            self.model = model;
            self.diffs = diffs;
            self.graph_layout = graph::layout(&mut self.model.commits);
            self.model.graph_width = self.graph_layout.lanes;
            self.selected_change = first_change(&self.model);
            if self.selected_commit >= self.model.commits.len() {
                self.selected_commit = 0;
            }
            self.diff_scroll = 0.0;
        }
    }

    /// Run an action and record any error for the status line. On success
    /// the model is reloaded from disk so the render reflects reality.
    fn run_action(&mut self, result: anyhow::Result<()>) {
        match result {
            Ok(()) => {
                self.last_error = None;
                self.reload();
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
            }
        }
    }

    /// The repo path for actions; `None` means sample-backed (no real repo).
    fn repo(&self) -> Option<std::path::PathBuf> {
        self.repo_path.clone()
    }

    fn repository_busy(&mut self) -> bool {
        if self.remote_job.is_some() {
            self.last_error = Some(ui::i18n!("git-remote-busy"));
            return true;
        }
        false
    }

    fn start_remote(
        &mut self,
        action: RemoteAction,
        remote: Option<String>,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> Result<(), String> {
        if self.remote_job.is_some() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        let path = self
            .repo()
            .ok_or_else(|| "remote operation requires a repository".to_string())?;
        let label = match action {
            RemoteAction::Fetch => "Fetching…",
            RemoteAction::Pull => "Pulling…",
            RemoteAction::Push => "Pushing…",
        }
        .to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("git-remote".into())
            .spawn(move || {
                let result = match action {
                    RemoteAction::Fetch => crate::repo::fetch(&path, remote.as_deref()).map(|_| ()),
                    RemoteAction::Pull => crate::repo::pull(&path, remote.as_deref()),
                    RemoteAction::Push => crate::repo::push(&path, remote.as_deref()),
                }
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        self.last_error = None;
        self.remote_job = Some(RemoteJob {
            receiver,
            result: None,
            invocation,
            progress_sent: false,
            label,
        });
        Ok(())
    }

    fn drain_remote(&mut self) {
        let Some(job) = self.remote_job.as_mut() else {
            return;
        };
        if job.result.is_some() {
            return;
        }
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(ui::i18n!("git-remote-worker-stopped")),
        };
        self.last_error = result.as_ref().err().cloned();
        job.result = Some(result);
        if job.invocation.is_none() {
            self.remote_job = None;
        }
        // A failed transfer can still have fetched objects/refs. Refresh from
        // disk on either outcome; never infer rollback from an error.
        self.reload();
    }

    // --- Responsive layout -------------------------------------------

    /// Resolve the sidebar layout from the app's allocated width. The active
    /// screen also gates this: only master-detail screens have a collapsible
    /// list, so screens without one always report `Docked` (their content is
    /// already full-width and no flap/scrim/hamburger appears).
    fn layout_mode(&self) -> SplitLayout {
        if !self.screen.has_sidebar() {
            return SplitLayout::Docked;
        }
        SplitLayout::from_width(self.rect.width(), NARROW_BREAKPOINT)
    }

    /// Whether the primary list column is currently a revealed/open overlay.
    fn flap_open(&self) -> bool {
        self.flap.is_open(self.layout_mode())
    }

    /// Advance the flap reveal toward its target using real elapsed time.
    /// Called once per drawn frame so the slide + scrim fade are smooth and
    /// frame-rate independent.
    fn tick_flap(&mut self) {
        let now = ui::time::now();
        let dt = (now - self.last_flap_tick).as_secs_f64();
        self.last_flap_tick = now;
        self.flap.tick(self.layout_mode(), dt.min(0.1));
    }

    /// The diff-map key for the currently selected change, if any. Resolves
    /// the staged vs. worktree side from the selected area so a partially
    /// staged file shows the correct diff on each side.
    fn current_diff_key(&self) -> Option<DiffKey> {
        let (area, idx) = self.selected_change?;
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        Some(DiffKey::for_area(area, list.get(idx)?.path.clone()))
    }

    /// Resolve the [`FileDiff`] for the currently selected change, if any.
    fn current_diff(&self) -> Option<&crate::model::FileDiff> {
        self.diffs.get(&self.current_diff_key()?)
    }

    fn selected_is_staged(&self) -> bool {
        matches!(self.selected_change, Some((ChangeArea::Staged, _)))
    }

    // --- Mutations on the model (mirror a real git op) -----------------

    /// Stage or unstage a single file. Against a real repo this calls
    /// `git2` (`add` / `reset HEAD -- path`) and re-reads the model so the
    /// list reflects the actual index. For the sample it falls back to the
    /// in-memory move so the offscreen capture still demonstrates the flow.
    fn toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        if self.repository_busy() {
            return;
        }
        if let Some(path) = self.repo() {
            let list = match area {
                ChangeArea::Staged => &self.model.staged,
                ChangeArea::Unstaged => &self.model.unstaged,
                ChangeArea::Untracked => &self.model.untracked,
            };
            let Some(file) = list.get(idx).map(|f| f.path.clone()) else {
                return;
            };
            let res = match area {
                ChangeArea::Staged => crate::repo::unstage_file(&path, &file),
                ChangeArea::Unstaged | ChangeArea::Untracked => {
                    crate::repo::stage_file(&path, &file)
                }
            };
            self.run_action(res);
            return;
        }
        self.toggle_stage_in_memory(area, idx);
    }

    /// In-memory move used only by the sample-backed app (no repo on disk).
    fn toggle_stage_in_memory(&mut self, area: ChangeArea, idx: usize) {
        match area {
            ChangeArea::Staged => {
                if idx < self.model.staged.len() {
                    let mut f = self.model.staged.remove(idx);
                    f.area = ChangeArea::Unstaged;
                    self.model.unstaged.push(f);
                }
            }
            ChangeArea::Unstaged => {
                if idx < self.model.unstaged.len() {
                    let mut f = self.model.unstaged.remove(idx);
                    f.area = ChangeArea::Staged;
                    self.model.staged.push(f);
                }
            }
            ChangeArea::Untracked => {
                if idx < self.model.untracked.len() {
                    let mut f = self.model.untracked.remove(idx);
                    f.area = ChangeArea::Staged;
                    f.kind = crate::model::ChangeKind::Added;
                    self.model.staged.push(f);
                }
            }
        }
        self.selected_change = first_change(&self.model);
    }

    fn stage_all(&mut self) {
        if self.repository_busy() {
            return;
        }
        if let Some(path) = self.repo() {
            let res = crate::repo::stage_all(&path);
            self.run_action(res);
            return;
        }
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.unstaged.drain(..) {
            f.area = ChangeArea::Staged;
            moved.push(f);
        }
        for mut f in self.model.untracked.drain(..) {
            f.area = ChangeArea::Staged;
            f.kind = crate::model::ChangeKind::Added;
            moved.push(f);
        }
        self.model.staged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    fn unstage_all(&mut self) {
        if self.repository_busy() {
            return;
        }
        if let Some(path) = self.repo() {
            let res = crate::repo::unstage_all(&path);
            self.run_action(res);
            return;
        }
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.staged.drain(..) {
            f.area = ChangeArea::Unstaged;
            moved.push(f);
        }
        self.model.unstaged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    fn do_commit(&mut self) {
        if self.repository_busy() {
            return;
        }
        if self.model.staged.is_empty() || self.commit_input.value().trim().is_empty() {
            return;
        }
        if let Some(path) = self.repo() {
            let msg = self.commit_input.value().to_string();
            let res = crate::repo::commit(&path, &msg).map(|_| ());
            let ok = res.is_ok();
            self.run_action(res);
            if ok {
                self.commit_input.set_text("");
                self.set_focus(None);
                // Land on the History screen so the new commit is visible.
                self.switch_screen(Screen::History);
                self.selected_commit = 0;
                self.history_scroll = 0.0;
            }
            return;
        }
        self.do_commit_in_memory();
    }

    /// Sample-only commit: synthesise a tip commit so the offscreen capture
    /// shows the action's effect without a repository.
    fn do_commit_in_memory(&mut self) {
        let staged = std::mem::take(&mut self.model.staged);
        let parent = self.model.commits.first().map(|c| c.id.clone());
        let id = format!("{:07x}", self.model.commits.len() * 0x1f7 + 0xa10);
        let message = self.commit_input.value().to_string();
        let summary = message.lines().next().unwrap_or("").to_string();
        let body = message
            .split_once('\n')
            .map(|x| x.1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let new = crate::model::Commit {
            id,
            summary,
            body,
            author_name: self.model.author_name.clone(),
            author_email: self.model.author_email.clone(),
            when: "just now".into(),
            time: chrono::Utc::now().timestamp(),
            parents: parent.into_iter().collect(),
            refs: vec![
                crate::model::RefLabel {
                    name: "HEAD".into(),
                    kind: crate::model::RefKind::Head,
                },
                crate::model::RefLabel {
                    name: self.model.head.clone(),
                    kind: crate::model::RefKind::LocalBranch,
                },
            ],
            lane: 0,
            files: staged,
        };
        if let Some(prev) = self.model.commits.first_mut() {
            prev.refs
                .retain(|r| r.kind != crate::model::RefKind::Head && r.name != self.model.head);
        }
        self.model.commits.insert(0, new);
        self.graph_layout = graph::layout(&mut self.model.commits);
        self.model.graph_width = self.graph_layout.lanes;
        self.commit_input.set_text("");
        self.set_focus(None);
        self.selected_change = first_change(&self.model);
    }

    /// Discard a file's worktree changes (revert unstaged edits, or delete an
    /// untracked file). Two-click armed: the first click on a discard
    /// affordance arms the row; only a second confirming click reaches here.
    fn discard_file(&mut self, area: ChangeArea, idx: usize) {
        if self.repository_busy() {
            return;
        }
        self.armed_discard = None;
        let Some(path) = self.repo() else { return };
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        let Some(file) = list.get(idx).map(|f| f.path.clone()) else {
            return;
        };
        let res = crate::repo::discard_file(&path, &file);
        self.run_action(res);
    }

    /// Discard a single hunk of the selected (unstaged) file's worktree change.
    fn discard_hunk(&mut self, hunk: usize) {
        if self.repository_busy() {
            return;
        }
        let Some((area, _)) = self.selected_change else {
            return;
        };
        // Only worktree (unstaged/untracked) hunks are discardable — a staged
        // file's diff is the index side, which "discard" doesn't act on.
        if area == ChangeArea::Staged {
            return;
        }
        if let (Some(path), Some(file)) = (self.repo(), self.current_path()) {
            let res = crate::repo::discard_hunk(&path, &file, hunk);
            self.run_action(res);
        }
    }

    /// Open the repository at the empty-state path field, binding it if valid.
    fn open_repo_from_input(&mut self) {
        if self.repository_busy() {
            return;
        }
        let raw = self.open_input.value().trim().to_string();
        if raw.is_empty() {
            return;
        }
        let path = std::path::PathBuf::from(&raw);
        match crate::repo::load(&path) {
            Some((model, diffs)) => {
                let anchor = crate::repo::workdir(&path).unwrap_or(path);
                self.model = model;
                self.diffs = diffs;
                self.graph_layout = graph::layout(&mut self.model.commits);
                self.model.graph_width = self.graph_layout.lanes;
                self.repo_path = Some(anchor);
                self.selected_change = first_change(&self.model);
                self.awaiting_repo = false;
                self.last_error = None;
                self.set_focus(None);
                self.screen = Screen::Changes;
                self.flap.reconcile(self.layout_mode());
            }
            None => {
                self.last_error = Some(format!("No git repository at {raw}"));
            }
        }
    }

    /// Give keyboard focus to one embedded text field (or none), keeping the
    /// toolkit inputs' focus flags mutually exclusive.
    fn set_focus(&mut self, field: Option<Field>) {
        self.focus = field;
        self.commit_input.set_focus(field == Some(Field::Commit));
        self.branch_input.set_focus(field == Some(Field::Branch));
        self.open_input.set_focus(field == Some(Field::Open));
    }

    // --- Durable view-preference actions -------------------------------
    // The diff-view preferences are the app's only shared-document state.
    // Each toggle updates the local snapshot for an immediate redraw AND
    // dispatches its `GitClientAction` through the framework `ActionHost`,
    // which brackets the undo boundary and applies it through the executor
    // (so it records one Ctrl+Z step and replays byte-identically on peers).
    // The next `sync` re-reads the same value straight back out of the doc.
    // These are the exact actions the Ctrl+K resource tree exports — one
    // source of truth (see `crate::actions`).

    /// Flip the diff-pane layout (unified ⇄ side-by-side). Routed to by the
    /// app-bar segmented control and the `d` shortcut.
    fn toggle_diff_layout(&mut self, doc: &mut AutoCommit) {
        self.settings.toggle_layout();
        self.diff_scroll = 0.0;
        let action = crate::actions::set_diff_layout_action(self.settings.diff_layout);
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
        );
    }

    /// Flip diff syntax highlighting. Routed to by the app-bar `</>` pill.
    fn toggle_syntax_highlight(&mut self, doc: &mut AutoCommit) {
        self.settings.syntax_highlight = !self.settings.syntax_highlight;
        let action = crate::actions::GitClientAction::SetSyntaxHighlight {
            on: self.settings.syntax_highlight,
        };
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
        );
    }

    /// Borrow the current external repository projection for the bounded
    /// resource catalogue. Presentation-only state is intentionally excluded.
    pub(crate) fn resource_model(&self) -> Option<&RepoModel> {
        (!self.awaiting_repo).then_some(&self.model)
    }

    /// The last preference [`EncodedAction`](tilo_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec`
    /// covenant test.
    #[doc(hidden)]
    pub fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Show the diff preferences the framework resource already holds. The
    /// live on-disk repository model is not replaced by the shared projection.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::doc::GitClientState) {
        if self.settings_forced {
            return;
        }
        self.settings = state.settings;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::GitClient>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Label for an in-flight fetch/pull/push, when one is running.
    pub(crate) fn remote_job_label(&self) -> Option<String> {
        self.remote_job.as_ref().map(|job| job.label.clone())
    }

    /// Write the shared repository projection when branches/commits/remotes move.
    fn publish_shared_repo_if_needed(&mut self, doc: &mut AutoCommit) {
        if self.awaiting_repo {
            return;
        }
        let fingerprint = crate::doc::shared_fingerprint(&self.model);
        if self.shared_repo_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        let shared = crate::doc::SharedRepo::from_model(&self.model);
        let json = serde_json::to_string(&shared).unwrap_or_else(|_| "{}".into());
        let action = crate::actions::GitClientAction::WriteSharedRepo { json };
        self.actions.dispatch_with_context(
            doc,
            crate::actions::GIT_CLIENT_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
        );
        self.shared_repo_fingerprint = Some(fingerprint);
    }

    fn preference_actions(&self) -> [crate::actions::GitClientAction; 4] {
        [
            crate::actions::set_diff_layout_action(self.settings.diff_layout),
            crate::actions::GitClientAction::SetShowWhitespace {
                on: self.settings.show_whitespace,
            },
            crate::actions::GitClientAction::SetSyntaxHighlight {
                on: self.settings.syntax_highlight,
            },
            crate::actions::GitClientAction::SetWrapLines {
                on: self.settings.wrap_lines,
            },
        ]
    }

    fn publish_settings(&mut self, doc: &mut AutoCommit) {
        for action in self.preference_actions() {
            self.actions.dispatch_with_context(
                doc,
                crate::actions::GIT_CLIENT_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.git-client", "Git", &action),
            );
        }
    }

    // --- Drawing -------------------------------------------------------

    /// The app bar's raised surface and its bottom edge.
    fn paint_appbar_surface(ctx: &mut PaintCtx<'_>, bar: Rect) {
        let raised = ctx.theme.tokens.colors.surfaces.raised;
        ctx.fill_rect(bar, raised);
        draw::hline(ctx, bar.y1, bar.x0, bar.x1, draw::edge(raised));
    }

    fn draw_appbar(&mut self, ctx: &mut PaintCtx<'_>) {
        let overlay = self.layout_mode() == SplitLayout::Overlay;
        // Measure every cluster before placing anything: the title is clamped
        // against measured neighbours, never against a guessed width.
        let tabs = Self::measure_appbar_tabs(ctx.text, self.rect.width() < 420.0);
        let settings = SettingsWidths::measure(ctx.text);
        let cluster_w = settings.total();
        let left_pad = if overlay { HAMBURGER_SLOT } else { 0.0 };
        let title_x = self.rect.x0 + APPBAR_GUTTER + left_pad;
        // Tab group background carries a 6px inset on either side.
        let tab_group_w = tabs.total + 12.0;
        let one_row_right = self.rect.x1 - APPBAR_GUTTER;
        let cluster_x = one_row_right - cluster_w;
        let one_row_tabs_left = cluster_x - APPBAR_CLUSTER_GAP - tab_group_w;
        // Wrap the tabs under the title when a readable title cannot sit
        // beside the tab group and the settings on one row.
        let wrap = one_row_tabs_left - 8.0 - title_x < TITLE_MIN_W;
        let m = BarMetrics::measure(ctx.text);
        // On the narrowest windows even the title and measured settings cannot
        // share a row. Keep upstream's separate settings row at that limit.
        let separate_settings_row = cluster_x - APPBAR_CLUSTER_GAP - title_x < 40.0;
        self.appbar_h = if separate_settings_row {
            m.row_h * 2.0 + m.tab_row_h
        } else if wrap {
            m.row_h + m.tab_row_h
        } else {
            m.row_h
        };
        let bar = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.appbar_h,
        );
        Self::paint_appbar_surface(ctx, bar);
        let row1 = Rect::new(bar.x0, bar.y0, bar.x1, bar.y0 + m.row_h);
        // The span of `row` from `x0` to `x1`.
        let span = |row: Rect, x0: f64, x1: f64| Rect::new(x0, row.y0, x1, row.y1);

        // Hamburger toggle: shown only when the primary list column has
        // collapsed into an overlay flap (narrow window, master-detail
        // screen). It summons/dismisses the flap. The title block shifts
        // right to make room. Outside overlay mode there's no flap to toggle.
        if overlay {
            self.draw_hamburger(ctx, row1, m.control_h);
        } else {
            self.hamburger_rect = Rect::ZERO;
        }

        self.tab_rects.clear();
        if wrap {
            // Row 1: title + settings cluster. Row 2: the screen tabs, left
            // aligned on the same gutter as the title.
            let title_limit = if separate_settings_row {
                one_row_right
            } else {
                cluster_x - APPBAR_CLUSTER_GAP
            };
            self.draw_appbar_title(ctx, span(row1, title_x, title_limit), &m);
            let settings_row = if separate_settings_row {
                Rect::new(bar.x0, row1.y1 + m.tab_row_h, bar.x1, bar.y1)
            } else {
                row1
            };
            self.draw_settings_cluster(
                ctx,
                span(settings_row, cluster_x, settings_row.x1),
                settings,
                m.control_h,
            );
            let row2 = Rect::new(bar.x0, row1.y1, bar.x1, row1.y1 + m.tab_row_h);
            let tx_start = bar.x0 + APPBAR_GUTTER + 6.0;
            let wrapped = Self::fit_wrapped_appbar_tabs(&tabs.cells, bar.x1, tx_start, ctx.theme);
            self.draw_appbar_tab_group(ctx, span(row2, tx_start, row2.x1), &wrapped, m.control_h);
        } else {
            let tx_start = one_row_tabs_left + 6.0;
            let title_limit = one_row_tabs_left - 8.0;
            self.draw_appbar_title(ctx, span(row1, title_x, title_limit), &m);
            self.draw_appbar_tab_group(ctx, span(row1, tx_start, row1.x1), &tabs, m.control_h);
            self.draw_settings_cluster(ctx, span(row1, cluster_x, row1.x1), settings, m.control_h);
        }
    }

    /// Each tab's pixel width and their combined width (including the 6px
    /// gaps between them) — needed both to clamp the title and to lay out
    /// the tab group's pill background.
    fn measure_appbar_tabs(tr: &mut TextRenderer, compact: bool) -> TabWidths {
        let widths: Vec<f64> = TABS
            .iter()
            .map(|(_, label)| {
                f64::from(tr.measure_weighted(label, 13.0, None, TextWeight::Medium).0)
                    + if compact { 16.0 } else { 28.0 }
            })
            .collect();
        let total = widths.iter().sum::<f64>() + (TABS.len() as f64 - 1.0) * 6.0;
        TabWidths {
            cells: widths,
            total,
        }
    }

    /// Fit the wrapped (second-row) tab group between `tx_start` and the
    /// bar's right gutter at the theme's minimum pointer width.
    fn fit_wrapped_appbar_tabs(
        widths: &[f64],
        bar_x1: f64,
        tx_start: f64,
        theme: &Theme,
    ) -> TabWidths {
        let available = (bar_x1 - APPBAR_GUTTER - 6.0 - tx_start).max(0.0);
        Self::fit_appbar_tabs(
            widths,
            available,
            f64::from(theme.tokens.metrics.min_pointer_target),
        )
    }

    /// Compress only the padding/slack of a wrapped tab row. Every semantic
    /// target keeps the theme's minimum pointer width; labels are elided at
    /// paint time while accessibility and keyboard routing retain full names.
    fn fit_appbar_tabs(widths: &[f64], available: f64, minimum: f64) -> TabWidths {
        let gaps = TABS.len().saturating_sub(1) as f64 * 6.0;
        let natural_cells = widths.iter().sum::<f64>();
        let allowed_cells = (available - gaps).max(minimum * widths.len() as f64);
        if natural_cells <= allowed_cells {
            return TabWidths {
                cells: widths.to_vec(),
                total: natural_cells + gaps,
            };
        }
        let slack = widths
            .iter()
            .map(|width| (width - minimum).max(0.0))
            .sum::<f64>();
        let keep = (slack - (natural_cells - allowed_cells)).max(0.0);
        let factor = if slack > 0.0 { keep / slack } else { 0.0 };
        let fitted: Vec<f64> = widths
            .iter()
            .map(|width| minimum + (width - minimum).max(0.0) * factor)
            .collect();
        let total = fitted.iter().sum::<f64>() + gaps;
        TabWidths {
            cells: fitted,
            total,
        }
    }

    /// Draw the repo name, branch pill and pending-changes/error subtitle into
    /// `slot` (the title's span of its bar row), clamped so a long name
    /// ellipsises before the tab group.
    fn draw_appbar_title(&mut self, ctx: &mut PaintCtx<'_>, slot: Rect, m: &BarMetrics) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let title_x = slot.x0;
        let right_limit = slot.x1;
        let title_max = (right_limit - title_x).max(40.0) as f32;
        // Elide rather than wrap: a wrapped repository name or subtitle used
        // to spill out of the bar on a narrow window.
        let name =
            ctx.text
                .elide_to_width_weighted(&self.model.name, 15.0, title_max, TextWeight::Bold);
        let (nw, _) = ctx
            .text
            .measure_weighted(&name, 15.0, None, TextWeight::Bold);
        let nh = m.title_h;
        // The title + subtitle stack centres in the row: at 1× that is the
        // historical `y0 + 9`, at 2× the row has grown to hold both lines.
        let stack_h = nh + m.title_gap + m.subtitle_h;
        let title_y = slot.y0 + (slot.height() - stack_h) / 2.0;
        ctx.text(
            &name,
            &TextStyle::new(15.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(title_x, title_y),
            None,
        );
        let mut right = title_x + f64::from(nw);
        let pill_x = right + 8.0;
        // Paint the branch pill only when its *measured* width fits left of
        // the next control (the pill measures `head` at 14px Medium with 6px
        // padding either side); centre it on the title's measured line.
        let pill_h = draw::pill_height(ctx.text, 14.0);
        let pill_w = draw::pill_width(ctx.text, &self.model.head, 14.0);
        if pill_x + pill_w <= right_limit {
            draw::pill(
                ctx,
                Point::new(pill_x, title_y + (nh - pill_h) / 2.0),
                &self.model.head,
                &draw::PillStyle {
                    size: 14.0,
                    fg: c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
                    bg: theme.accent_primary,
                    edge: draw::PillEdge::Bare,
                },
            );
            right = pill_x + pill_w;
        }
        // change count subtitle — or the last action error when present.
        let (sub, sub_color) = if self.remote_job.is_some() {
            (ui::i18n!("git-remote-running"), c.text_dim)
        } else {
            match &self.last_error {
                Some(err) => (err.clone(), c.semantic.danger),
                None => (
                    format!(
                        "{} pending change{}",
                        self.model.change_count(),
                        if self.model.change_count() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ),
                    c.text_dim,
                ),
            }
        };
        self.status_text.clone_from(&sub);
        let sub = ctx.text.elide_to_width(&sub, SUBTITLE_FONT, title_max);
        let (sw, _) = ctx.text.measure(&sub, SUBTITLE_FONT, None);
        let sub_y = title_y + nh + m.title_gap;
        self.status_rect = Rect::new(
            title_x,
            sub_y,
            title_x + f64::from(title_max),
            sub_y + m.subtitle_h,
        );
        ctx.text(
            &sub,
            &TextStyle::new(SUBTITLE_FONT),
            sub_color,
            Point::new(title_x, sub_y),
            None,
        );
        self.title_rect = Rect::new(
            title_x,
            slot.y0,
            right.max(title_x + f64::from(sw)),
            slot.y1,
        );
    }

    /// Draw the tab-group pill background and each tab of `tabs`, starting at
    /// `row.x0` and centred in `row` at `tab_h` tall, recording each hit rect
    /// in `self.tab_rects`.
    fn draw_appbar_tab_group(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        tabs: &TabWidths,
        tab_h: f64,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let tx_start = row.x0;
        let tab_y = row.y0 + (row.height() - tab_h) / 2.0;
        let group_bg = Rect::new(
            tx_start - 6.0,
            tab_y,
            tx_start + tabs.total + 6.0,
            tab_y + tab_h,
        );
        // The tab group is a base-toned track on the raised bar; its derived
        // edge is what separates the two surfaces on the dark preset.
        draw::card(ctx, group_bg, 9.0, c.surfaces.base);
        let label_style = TextStyle::new(13.0).with_weight(TextWeight::Medium);
        let mut tx = tx_start;
        for (i, (screen, label)) in TABS.iter().enumerate() {
            let Some(&w) = tabs.cells.get(i) else {
                continue;
            };
            let trect = Rect::new(tx, tab_y, tx + w, tab_y + tab_h);
            let active = *screen == self.screen;
            if active {
                ctx.fill_round(trect, 7.0, theme.accent_primary);
            }
            let fg = if active {
                c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
            } else {
                c.text_dim
            };
            let shown = ctx.text.elide_to_width_weighted(
                label,
                13.0,
                (w - 12.0).max(0.0) as f32,
                TextWeight::Medium,
            );
            let lw = f64::from(ctx.text.measure_style(&shown, None, &label_style).0);
            let label_y =
                draw::centered_top(ctx.text, &shown, 13.0, TextWeight::Medium, tab_y, tab_h);
            ctx.text(
                &shown,
                &label_style,
                fg,
                Point::new(tx + (w - lw) / 2.0, label_y),
                None,
            );
            self.tab_rects.push((*screen, trect));
            tx += w + 6.0;
        }
    }

    /// Draw the diff-layout segmented control and the syntax toggle from
    /// `row.x0`, each `control_h` tall and centred in `row`, and record their
    /// hit rects.
    fn draw_settings_cluster(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        row: Rect,
        widths: SettingsWidths,
        control_h: f64,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let x = row.x0;
        let y = row.y0 + (row.height() - control_h) / 2.0;
        let label_style = TextStyle::new(12.0).with_weight(TextWeight::Medium);
        // Diff-layout segmented control.
        let seg = Rect::new(x, y, x + widths.segmented, y + control_h);
        draw::card(ctx, seg, 9.0, c.surfaces.base);
        let half = widths.segmented / 2.0;
        let unified_active = self.settings.diff_layout == DiffLayout::Unified;
        let active_rect = if unified_active {
            Rect::new(seg.x0 + 3.0, seg.y0 + 3.0, seg.x0 + half, seg.y1 - 3.0)
        } else {
            Rect::new(seg.x0 + half, seg.y0 + 3.0, seg.x1 - 3.0, seg.y1 - 3.0)
        };
        ctx.fill_round(active_rect, 7.0, theme.accent_primary);
        for (i, label) in ["Unified", "Split"].iter().enumerate() {
            let active = (i == 0) == unified_active;
            let fg = if active {
                c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
            } else {
                c.text_dim
            };
            let cx = seg.x0 + half * i as f64;
            let lw = f64::from(ctx.text.measure_style(label, None, &label_style).0);
            let label_y =
                draw::centered_top(ctx.text, label, 12.0, TextWeight::Medium, y, control_h);
            ctx.text(
                label,
                &label_style,
                fg,
                Point::new(cx + (half - lw) / 2.0, label_y),
                None,
            );
        }
        // Note: the segmented control's click rect is stored as the whole
        // seg; the click handler flips the layout.
        self.settings_seg = seg;

        // Syntax-highlight toggle. Keep the visible label explicit: this is a
        // preference, not a repository status, and "Syn" is ambiguous when a
        // control is viewed without its tooltip or action palette.
        let hx = seg.x1 + APPBAR_CLUSTER_GAP;
        let hl = Rect::new(hx, y, hx + widths.syntax, y + control_h);
        let on = self.settings.syntax_highlight;
        if on {
            ctx.fill_round(hl, 9.0, theme.accent_primary);
        } else {
            draw::card(ctx, hl, 9.0, c.surfaces.base);
        }
        let hlfg = if on {
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
        } else {
            c.text_dim
        };
        let label = "Syntax";
        let lw = f64::from(ctx.text.measure_style(label, None, &label_style).0);
        let label_y = draw::centered_top(ctx.text, label, 12.0, TextWeight::Medium, y, control_h);
        ctx.text(
            label,
            &label_style,
            hlfg,
            Point::new(hl.x0 + (hl.width() - lw) / 2.0, label_y),
            None,
        );
        self.settings_syn = hl;
    }

    /// Draw the hamburger toggle at the left of the app bar and record its hit
    /// rect. Highlighted while the flap is open so it reads as an active
    /// toggle. Three stacked bars — the universal "open the menu" affordance.
    fn draw_hamburger(&mut self, ctx: &mut PaintCtx<'_>, bar: Rect, size: f64) {
        let bx = bar.x0 + 12.0;
        let by = bar.y0 + (bar.height() - size) / 2.0;
        let btn = Rect::new(bx, by, bx + size, by + size);
        self.hamburger_rect = btn;
        self.paint_hamburger(ctx, btn);
    }

    /// Paint the hamburger glyph into `btn` (its placed rect) — also used to
    /// repaint it above an open flap so the toggle stays reachable.
    fn paint_hamburger(&self, ctx: &mut PaintCtx<'_>, btn: Rect) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let size = btn.height();
        let open = self.flap_open();
        if open {
            ctx.fill_round(btn, 8.0, theme.accent_primary);
        } else {
            draw::card(ctx, btn, 8.0, c.surfaces.base);
        }
        let fg = if open {
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
        } else {
            c.text_dim
        };
        // Three 2px bars at a 6px pitch, centred as a block in the button.
        let inset = 8.0;
        let x0 = btn.x0 + inset;
        let x1 = btn.x1 - inset;
        let bar_h = 2.0;
        let pitch = 6.0;
        let block_h = pitch * 2.0 + bar_h;
        let block_top = btn.y0 + (size - block_h) / 2.0;
        for i in 0..3 {
            let ly = block_top + f64::from(i) * pitch;
            let line = Rect::new(x0, ly, x1, ly + bar_h);
            ctx.fill_round(line, 1.0, fg);
        }
    }

    /// Resolve a master-detail screen's geometry responsively from the flap.
    ///
    /// Returns `(list_rect, detail_rect, list_visible)`. Wide (docked): the
    /// list column sits beside the detail, which is inset to make room; the
    /// reveal eases the column's width so a toggle is smooth. Narrow
    /// (overlay): the detail spans the full body and the list column slides
    /// in over it from the left edge (the caller paints the scrim between the
    /// two via [`Self::draw_overlay_scrim`]). The list is hidden entirely
    /// while collapsed so it doesn't paint behind a zero-width column.
    fn master_detail_rects(&self, body: Rect, list_w: f64) -> (Rect, Rect, bool) {
        match self.layout_mode() {
            SplitLayout::Docked => {
                let inset = list_w * self.flap.reveal();
                let col = Rect::new(body.x0, body.y0, body.x0 + inset, body.y1);
                let detail = Rect::new(col.x1, body.y0, body.x1, body.y1);
                (col, detail, inset > 0.5)
            }
            SplitLayout::Overlay => {
                let slide = self.flap.slide();
                let x0 = body.x0 - list_w * (1.0 - slide);
                let col = Rect::new(x0, body.y0, x0 + list_w, body.y1);
                (col, body, self.flap.reveal() > 0.001)
            }
        }
    }

    /// Paint the dimming scrim over `body` in overlay mode, sized to the flap
    /// reveal. A no-op when docked or fully closed. Drawn after the detail and
    /// before the list column so the column floats above the dimmed content.
    fn draw_overlay_scrim(&self, ctx: &mut PaintCtx<'_>, body: Rect) {
        if self.layout_mode() != SplitLayout::Overlay {
            return;
        }
        let reveal = self.flap.reveal();
        if reveal <= 0.001 {
            return;
        }
        let scrim = ctx
            .theme
            .tokens
            .colors
            .surfaces
            .scrim
            .with_alpha_factor((reveal * 0.86) as f32);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            scrim,
            None,
            &Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, body.y1),
        );
    }

    fn draw_changes(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx, body: Rect) {
        let (col, pane, list_visible) = self.master_detail_rects(body, CHANGES_COL_W);
        // Detail (diff) first — it sits behind the overlay flap.
        self.draw_diff_pane(ctx, pane);
        // Scrim between detail and list (overlay mode only).
        self.draw_overlay_scrim(ctx, body);
        if list_visible {
            let msg = self.commit_input.value().to_string();
            let out = ChangesColumn::draw(
                ctx,
                col,
                &self.model,
                self.selected_change,
                self.changes_scroll,
                &msg,
            );
            self.change_hits = out.hits;
            self.change_semantic_hits = out.semantic_hits;
            // Position and draw the commit-message TextInput in the reserved
            // rect — the toolkit input owns its frame, text, caret and
            // selection so editing is a first-class ui component.
            self.commit_input.apply_layout_external(out.commit_input);
            self.commit_input
                .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
        } else {
            self.change_hits.clear();
            self.change_semantic_hits.clear();
            self.commit_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// Paint the diff detail pane (header + diff body) into `pane`.
    fn draw_diff_pane(&mut self, ctx: &mut PaintCtx<'_>, pane: Rect) {
        // The header is 36 px at 1× and grows to hold its 13 px title line
        // plus padding at larger text scales; the body and scroll clamp read
        // the same measured height.
        let (_, title_h) = ctx
            .text
            .measure_weighted("Ag", 13.0, None, TextWeight::Medium);
        self.diff_header_h = (f64::from(title_h) + 2.0 * 9.0).max(36.0);
        let header = Rect::new(pane.x0, pane.y0, pane.x1, pane.y0 + self.diff_header_h);
        let c = &ctx.theme.tokens.colors;
        ctx.fill_rect(header, c.surfaces.raised);
        draw::hline(
            ctx,
            header.y1,
            header.x0,
            header.x1,
            draw::edge(c.surfaces.raised),
        );

        let key = self.current_diff_key();
        // Pull the header facts out as owned values so the `self.diffs` borrow
        // ends before we mutate `self` (discard rect) or re-borrow for the body.
        let info = key
            .as_ref()
            .and_then(|k| self.diffs.get(k))
            .map(|d| DiffHeaderInfo {
                old_path: d.old_path.clone(),
                path: d.path.clone(),
                additions: d.additions(),
                deletions: d.deletions(),
            });
        self.discard_file_rect = Rect::ZERO;
        let staged = self.selected_is_staged();

        if let Some(info) = info {
            self.draw_diff_header_file(ctx, header, info, staged);
        } else {
            let hint = "Select a file to view its diff";
            let hy = draw::centered_top(
                ctx.text,
                hint,
                13.0,
                TextWeight::Regular,
                header.y0,
                header.height(),
            );
            ctx.text(
                hint,
                &TextStyle::new(13.0),
                c.text_dim,
                Point::new(header.x0 + 16.0, hy),
                None,
            );
        }

        let viewport = Rect::new(pane.x0, header.y1, pane.x1, pane.y1);
        self.draw_diff_body(ctx, viewport, key.as_ref(), staged);
    }

    /// Draw the diff-pane header for the selected file: title, +/− summary,
    /// and (on the worktree side of a real repo) the two-click "Discard
    /// file" affordance.
    fn draw_diff_header_file(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        header: Rect,
        info: DiffHeaderInfo,
        staged: bool,
    ) {
        let c = &ctx.theme.tokens.colors;
        let title = match info.old_path {
            Some(old) => format!("{old}  →  {}", info.path),
            None => info.path,
        };
        let summary = format!("+{}  −{}", info.additions, info.deletions);
        let aw = f64::from(draw::mono_width(ctx.text, &summary, 12.0));
        let sy = draw::mono_centered_top(ctx.text, &summary, 12.0, header.y0, header.height());
        draw::mono(
            ctx,
            &summary,
            12.0,
            c.text_dim,
            Point::new(header.x1 - aw - 16.0, sy),
        );

        // Discard-file affordance on the worktree (unstaged / untracked)
        // side of a real repo. Two-click armed: shows "Discard file?" when
        // armed, filled danger.
        let mut title_max = (header.width() - aw - 16.0 - 16.0 - 12.0).max(40.0) as f32;
        let can_discard = self.repo_path.is_some() && !staged && self.selected_change.is_some();
        if can_discard {
            title_max = self.draw_discard_file_button(ctx, header, aw);
        }
        // Elide rather than wrap: a second title line has no room in the
        // header band and used to paint over the diff's first row.
        let title = ctx
            .text
            .elide_to_width_weighted(&title, 13.0, title_max, TextWeight::Medium);
        let ty = draw::centered_top(
            ctx.text,
            &title,
            13.0,
            TextWeight::Medium,
            header.y0,
            header.height(),
        );
        ctx.text(
            &title,
            &TextStyle::new(13.0).with_weight(TextWeight::Medium),
            c.text_primary,
            Point::new(header.x0 + 16.0, ty),
            None,
        );
    }

    /// Draw the "Discard file" / (armed) "Discard file?" button and return
    /// the title's clamped max width now that the button occupies space.
    fn draw_discard_file_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        header: Rect,
        summary_w: f64,
    ) -> f32 {
        let armed = self.armed_discard == Some(DiscardTarget::File);
        let label = if armed {
            "Discard file?"
        } else {
            "Discard file"
        };
        let lw = f64::from(
            ctx.text
                .measure_weighted(label, DIFF_DISCARD_FONT, None, TextWeight::Medium)
                .0,
        );
        let bw = lw + 20.0;
        let bx = header.x1 - summary_w - 16.0 - 12.0 - bw;
        let brect = Rect::new(bx, header.y0 + 6.0, bx + bw, header.y1 - 6.0);
        // Destructive outline at rest, filled danger once armed — the shared
        // face keeps the danger ink legible on every ground it sits on.
        let face = ButtonFace::new(brect, label)
            .radius(6.0)
            .text_size(DIFF_DISCARD_FONT);
        let face = if armed {
            face.danger()
        } else {
            face.danger_outline()
        };
        draw::button(ctx, &face);
        self.discard_file_rect = brect;
        (bx - header.x0 - 24.0).max(40.0) as f32
    }

    /// Draw the diff body into `viewport` (the pane below its header): the
    /// hunk view for the selected file, or an empty pane when nothing is
    /// selected.
    fn draw_diff_body(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        viewport: Rect,
        key: Option<&DiffKey>,
        staged: bool,
    ) {
        let armed_discard = match self.armed_discard {
            Some(DiscardTarget::Hunk(h)) => Some(h),
            _ => None,
        };
        if let Some(diff) = key.and_then(|k| self.diffs.get(k)) {
            let interaction = HunkInteraction {
                hovered: self.hover_hunk,
                staged_side: staged,
                armed_discard,
            };
            let out = DiffView::draw_with_metrics(
                ctx,
                viewport,
                diff,
                self.diff_scroll,
                &self.settings,
                interaction,
            );
            self.diff_hunk_buttons = out.buttons;
            self.diff_metrics = out.metrics;
        } else {
            ctx.fill_rect(viewport, ctx.theme.tokens.colors.surfaces.base);
            self.diff_hunk_buttons.clear();
        }
    }

    fn current_path(&self) -> Option<String> {
        let (area, idx) = self.selected_change?;
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        list.get(idx).map(|f| f.path.clone())
    }

    fn draw_history(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) {
        match self.layout_mode() {
            SplitLayout::Docked => {
                // Wide: the commit list keeps its generous ~56% split, eased
                // in/out by the flap reveal so a toggle slides smoothly.
                let full_split = (body.width() * 0.56).max(420.0).min(body.width() - 320.0);
                let split = body.x0 + full_split * self.flap.reveal();
                let detail = Rect::new(split, body.y0, body.x1, body.y1);
                if let Some(commit) = self.model.commits.get(self.selected_commit) {
                    HistoryView::draw_detail(ctx, detail, commit, self.detail_scroll);
                }
                if self.flap.reveal() > 0.001 {
                    let list = Rect::new(body.x0, body.y0, split, body.y1);
                    let out = HistoryView::draw_list(
                        ctx,
                        list,
                        &self.model,
                        &self.graph_layout,
                        self.selected_commit,
                        self.history_scroll,
                    );
                    self.history_rows = out.rows;
                    self.history_metrics = out.metrics;
                } else {
                    self.history_rows.clear();
                }
            }
            SplitLayout::Overlay => {
                // Narrow: detail spans full width; the commit list overlays it
                // as a fixed-width flap behind a scrim.
                let detail = body;
                if let Some(commit) = self.model.commits.get(self.selected_commit) {
                    HistoryView::draw_detail(ctx, detail, commit, self.detail_scroll);
                }
                self.draw_overlay_scrim(ctx, body);
                if self.flap.reveal() > 0.001 {
                    let slide = self.flap.slide();
                    let x0 = body.x0 - FLAP_W * (1.0 - slide);
                    let list = Rect::new(x0, body.y0, x0 + FLAP_W, body.y1);
                    let out = HistoryView::draw_list(
                        ctx,
                        list,
                        &self.model,
                        &self.graph_layout,
                        self.selected_commit,
                        self.history_scroll,
                    );
                    self.history_rows = out.rows;
                    self.history_metrics = out.metrics;
                } else {
                    self.history_rows.clear();
                }
            }
        }
    }

    fn draw_panels(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx, body: Rect) {
        let panels = PanelsView::draw(ctx, body, &self.model, self.hover_action);
        self.panel_hits = panels.hits;
        self.panel_semantic_hits = panels.semantic_hits;
        // New-branch creator: a compact labelled input + button drawn in the
        // toolbar's centre band, between the branch status (left) and the
        // fetch/pull/push cluster (right). Only meaningful for a real repo.
        if self.repo_path.is_some() {
            self.draw_new_branch(ctx, presence, body);
        } else {
            self.new_branch_input = Rect::ZERO;
            self.new_branch_button = Rect::ZERO;
            self.branch_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// Draw the new-branch input + "Create" button in the panels toolbar and
    /// record their hit rects. The toolbar is the top 56px band of `body`.
    fn draw_new_branch(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx, body: Rect) {
        let tb = Rect::new(body.x0, body.y0, body.x1, body.y0 + 56.0);
        // Reserve room for the 3 action buttons (84*3 + gaps + margins ≈ 300).
        let actions_w = 300.0;
        let btn_w = 70.0;
        let gap = 8.0;
        let h = 28.0;
        let y = tb.y0 + (tb.height() - h) / 2.0;
        let right = tb.x1 - actions_w;
        let bx = right - btn_w;
        let input = Rect::new(
            (right - btn_w - gap - 180.0).max(tb.x0 + 180.0),
            y,
            bx - gap,
            y + h,
        );
        if input.width() < 60.0 {
            // Too tight to render meaningfully — skip.
            self.new_branch_input = Rect::ZERO;
            self.new_branch_button = Rect::ZERO;
            self.branch_input.apply_layout_external(Rect::ZERO);
            return;
        }
        // Input field — the toolkit TextInput draws its own frame + caret.
        self.branch_input.apply_layout_external(input);
        self.branch_input
            .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
        self.new_branch_input = input;
        // Create button.
        let brect = Rect::new(bx, y, bx + btn_w, y + h);
        let enabled = !self.branch_input.value().trim().is_empty();
        draw::button(
            ctx,
            &ButtonFace::new(brect, "Branch")
                .primary()
                .radius(6.0)
                .text_size(12.0)
                .disabled(!enabled),
        );
        self.new_branch_button = brect;
    }

    fn create_new_branch(&mut self) {
        if self.repository_busy() {
            return;
        }
        let name = self.branch_input.value().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(path) = self.repo() {
            let res = crate::repo::create_branch(&path, &name, None, true);
            let ok = res.is_ok();
            self.run_action(res);
            if ok {
                self.branch_input.set_text("");
                self.set_focus(None);
            }
        }
    }

    /// Draw the empty "open a repository" state: a centered card with a path
    /// field (toolkit `TextInput`) and an Open button. This is the honest
    /// default when no repository is reachable — no placeholder repo data.
    fn draw_open(&mut self, ctx: &mut PaintCtx<'_>, presence: &PresenceCtx) {
        ctx.fill_rect(self.rect, ctx.theme.tokens.colors.surfaces.base);

        let cw = 520.0_f64.min(self.rect.width() - 48.0);
        let cx = self.rect.x0 + (self.rect.width() - cw) / 2.0;
        let cy = self.rect.y0 + (self.rect.height() * 0.30).max(80.0);

        let y = Self::draw_open_intro(ctx, Point::new(cx, cy), cw);
        self.draw_open_path_field(ctx, presence, Point::new(cx, y), cw);
    }

    /// Draw the branch-glyph mark, title and subtitle of the `width`-wide
    /// column whose top-left is `origin`, above the path field. Returns the y
    /// position the path field should start at.
    fn draw_open_intro(ctx: &mut PaintCtx<'_>, origin: Point, width: f64) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let (cx, cw, cy) = (origin.x, width, origin.y);
        // Branch glyph mark.
        let mark = Rect::new(cx + cw / 2.0 - 22.0, cy, cx + cw / 2.0 + 22.0, cy + 44.0);
        ctx.fill_round(mark, 12.0, c.accent[2]);
        let glyph_style = TextStyle::new(24.0).with_weight(TextWeight::Bold);
        let gw = f64::from(ctx.text.measure_style("⑂", None, &glyph_style).0);
        ctx.text(
            "⑂",
            &glyph_style,
            theme.accent_primary,
            Point::new(mark.x0 + (mark.width() - gw) / 2.0, mark.y0 + 8.0),
            None,
        );

        let mut y = cy + 64.0;
        let title = "No repository open";
        let title_style = TextStyle::new(20.0).with_weight(TextWeight::Bold);
        let tw = f64::from(ctx.text.measure_style(title, None, &title_style).0);
        ctx.text(
            title,
            &title_style,
            c.text_primary,
            Point::new(cx + (cw - tw) / 2.0, y),
            None,
        );
        y += 30.0;
        let sub = "Open a git repository to view its changes, history and branches.";
        let sw = f64::from(ctx.text.measure(sub, 13.0, None).0);
        ctx.text(
            sub,
            &TextStyle::new(13.0),
            c.text_dim,
            Point::new(cx + (cw - sw).max(0.0) / 2.0, y),
            Some(cw as f32),
        );
        y + 40.0
    }

    /// Draw the repository path field, Open button, and any open error of the
    /// `width`-wide column from `origin`, below the intro block drawn by
    /// [`Self::draw_open_intro`].
    fn draw_open_path_field(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        origin: Point,
        width: f64,
    ) {
        let c = &ctx.theme.tokens.colors;
        let (cx, cw, mut y) = (origin.x, width, origin.y);
        // Path field + Open button.
        let btn_w = 92.0;
        let gap = 10.0;
        let field_h = 40.0;
        let field = Rect::new(cx, y, cx + cw - btn_w - gap, y + field_h);
        self.open_input.apply_layout_external(field);
        self.open_input
            .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, presence);
        self.open_input_rect = field;
        let brect = Rect::new(field.x1 + gap, y, cx + cw, y + field_h);
        let enabled = !self.open_input.value().trim().is_empty();
        draw::button(
            ctx,
            &ButtonFace::new(brect, "Open")
                .primary()
                .radius(8.0)
                .text_size(14.0)
                .weight(TextWeight::Semibold)
                .disabled(!enabled),
        );
        self.open_button_rect = brect;

        // Any open error (e.g. "No git repository at …").
        if let Some(err) = &self.last_error {
            y += field_h + 16.0;
            let ew = f64::from(ctx.text.measure(err, 12.5, None).0);
            ctx.text(
                err,
                &TextStyle::new(12.5),
                c.semantic.danger,
                Point::new(cx + (cw - ew).max(0.0) / 2.0, y),
                Some(cw as f32),
            );
        }
    }

    // --- Event helpers -------------------------------------------------

    /// Switch the active screen and reconcile the flap to the new screen's
    /// layout mode. Switching to a full-width screen (Branches & Remotes) or
    /// between master-detail screens may change whether a flap exists, so the
    /// reconcile re-applies the right default for the destination.
    fn switch_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.set_focus(None);
        self.armed_discard = None;
        self.flap.reconcile(self.layout_mode());
    }

    fn handle_click(&mut self, pos: Point, doc: &mut AutoCommit) {
        // Hamburger toggle (overlay mode only) summons / dismisses the flap.
        if self.hamburger_rect.width() > 0.0 && self.hamburger_rect.contains(pos) {
            self.flap.toggle(self.layout_mode());
            return;
        }
        // While the overlay flap is open, a click on the scrim (the dimmed
        // detail to the right of the list column) dismisses it — and is
        // consumed so it doesn't also hit the content behind.
        if self.layout_mode() == SplitLayout::Overlay && self.flap_open() {
            let col_w = match self.screen {
                Screen::History => FLAP_W,
                _ => CHANGES_COL_W,
            };
            if pos.y < self.rect.y0 + self.appbar_h {
                return;
            }
            if pos.x > self.rect.x0 + col_w {
                self.flap.close();
                return;
            }
            match self.screen {
                Screen::Changes => self.handle_changes_click(pos),
                Screen::History => self.handle_history_click(pos),
                Screen::Panels => {}
            }
            return;
        }
        // Tabs.
        for (screen, rect) in &self.tab_rects {
            if rect.contains(pos) {
                self.switch_screen(*screen);
                return;
            }
        }
        // Settings segmented control + syntax toggle — durable preferences,
        // dispatched as actions so they undo and replicate.
        if self.settings_seg.contains(pos) {
            self.toggle_diff_layout(doc);
            return;
        }
        if self.settings_syn.contains(pos) {
            self.toggle_syntax_highlight(doc);
            return;
        }

        match self.screen {
            Screen::Changes => self.handle_changes_click(pos),
            Screen::History => self.handle_history_click(pos),
            Screen::Panels => self.handle_panels_click(pos),
        }
    }

    fn handle_changes_click(&mut self, pos: Point) {
        // Discard-file button (diff header) — armed two-click confirm.
        if self.discard_file_rect.width() > 0.0 && self.discard_file_rect.contains(pos) {
            if self.armed_discard == Some(DiscardTarget::File) {
                if let Some((area, idx)) = self.selected_change {
                    self.discard_file(area, idx);
                }
            } else {
                self.armed_discard = Some(DiscardTarget::File);
            }
            return;
        }
        // Hunk action buttons take priority (they overlap the diff body).
        if let Some((hunk, action)) = diffview::hunk_action_at(&self.diff_hunk_buttons, pos) {
            match action {
                diffview::HunkAction::Toggle => {
                    self.armed_discard = None;
                    self.stage_hunk(hunk);
                }
                diffview::HunkAction::Discard => {
                    if self.armed_discard == Some(DiscardTarget::Hunk(hunk)) {
                        self.discard_hunk(hunk);
                    } else {
                        self.armed_discard = Some(DiscardTarget::Hunk(hunk));
                    }
                }
            }
            return;
        }
        // Resolve against the changes column regions.
        let mut hit: Option<ChangesHit> = None;
        for f in &self.change_hits {
            if let Some(h) = f(pos) {
                hit = Some(h);
                break;
            }
        }
        // Any other interaction disarms a pending discard.
        self.armed_discard = None;
        let Some(hit) = hit else {
            return;
        };
        match hit {
            ChangesHit::SelectFile(area, idx) => {
                self.selected_change = Some((area, idx));
                self.diff_scroll = 0.0;
            }
            ChangesHit::ToggleStage(area, idx) => self.toggle_stage(area, idx),
            ChangesHit::StageAll(_) => self.stage_all(),
            ChangesHit::UnstageAll(_) => self.unstage_all(),
            ChangesHit::Commit => self.do_commit(),
            ChangesHit::FocusMessage => self.set_focus(Some(Field::Commit)),
        }
    }

    /// Stage (or unstage) a single hunk of the selected file. Against a real
    /// repo this applies exactly that hunk to the index via `git2` (the
    /// equivalent of `git add -p` / `git restore --staged -p`) and re-reads.
    /// The `hunk` argument is the hunk's index within the file's diff, which
    /// the diff view assigns its button to.
    ///
    /// The currently shown diff direction depends on which area is selected:
    /// a staged file shows the HEAD→index diff (so a hunk action *unstages*),
    /// an unstaged/untracked file shows the index→workdir diff (so a hunk
    /// action *stages*). That matches `apply_hunk`'s ordering exactly.
    fn stage_hunk(&mut self, hunk: usize) {
        if self.repository_busy() {
            return;
        }
        let Some((area, _)) = self.selected_change else {
            return;
        };
        if let (Some(path), Some(file)) = (self.repo(), self.current_path()) {
            let unstage = area == ChangeArea::Staged;
            // Map the displayed line's hunk to the Nth diff hunk for the file.
            let local = Self::local_hunk_index(&file, hunk);
            let res = crate::repo::apply_hunk(&path, &file, local, unstage);
            self.run_action(res);
            return;
        }
        // Sample fallback: move the whole file to prove the hit-test.
        if let Some((area, idx)) = self.selected_change {
            self.toggle_stage_in_memory(area, idx);
        }
    }

    /// The 0-based hunk position (within the file's diff, excluding meta
    /// "hunk 0 = file header" noise) for the diff line's `hunk` field. The
    /// parsed `DiffLine.hunk` is already a 0-based index into the file's
    /// `DiffHunk` list, so it maps directly; this helper exists so the
    /// mapping is named and testable if the parser's convention shifts.
    fn local_hunk_index(file: &str, hunk: usize) -> usize {
        // `hunk` indexes `FileDiff.hunks`; libgit2's hunk callback walks the
        // same hunks in the same order, so they're 1:1.
        let _ = file;
        hunk
    }

    fn handle_history_click(&mut self, pos: Point) {
        for (i, rect) in self.history_rows.iter().enumerate() {
            if rect.contains(pos) {
                self.selected_commit = i;
                self.detail_scroll = 0.0;
                return;
            }
        }
    }

    fn handle_panels_click(&mut self, pos: Point) {
        // Create button (the input's own focus is handled before this router).
        if self.new_branch_button.width() > 0.0 && self.new_branch_button.contains(pos) {
            self.create_new_branch();
            return;
        }
        let mut hit: Option<PanelHit> = None;
        for f in &self.panel_hits {
            if let Some(action) = f(pos) {
                hit = Some(action);
                break;
            }
        }
        match hit {
            Some(PanelHit::Remote(a)) => self.apply_remote_action(a),
            Some(PanelHit::Branch(a)) => self.apply_branch_action(a),
            None => {}
        }
    }

    /// Apply a remote action (fetch / pull / push) against the live repo via
    /// `git2`, then re-read the model so the ahead/behind counters reflect
    /// the real tracking state. For the sample, reconcile the counters so the
    /// offscreen capture shows the effect.
    fn apply_remote_action(&mut self, action: RemoteAction) {
        if self.repo().is_some() {
            if let Err(error) = self.start_remote(action, None, None) {
                self.last_error = Some(error);
            }
            return;
        }
        if let Some(b) = self.model.branches.iter_mut().find(|b| b.is_head) {
            match action {
                RemoteAction::Fetch => {}
                RemoteAction::Pull => b.behind = 0,
                RemoteAction::Push => b.ahead = 0,
            }
        }
    }

    /// Apply a branch action (checkout / create / delete) or a stash apply
    /// against the live repo, then re-read. Sample-backed apps ignore these
    /// (the panels' branch buttons only appear for real repos).
    fn apply_branch_action(&mut self, action: BranchAction) {
        if self.repository_busy() {
            return;
        }
        let Some(path) = self.repo() else { return };
        let res = match action {
            BranchAction::Checkout(name) => crate::repo::checkout_branch(&path, &name),
            BranchAction::Delete(name) => crate::repo::delete_branch(&path, &name),
            BranchAction::StashApply(i) => crate::repo::stash_apply(&path, i),
        };
        self.run_action(res);
    }

    fn scroll_active(&mut self, dy: f64, pos: Point) {
        // When the list is an open overlay flap it captures the wheel over its
        // own column; the detail behind it scrolls only when the flap is shut.
        let overlay_open = self.layout_mode() == SplitLayout::Overlay && self.flap_open();
        match self.screen {
            Screen::Changes => {
                // The list column occupies the left `CHANGES_COL_W` strip
                // whether docked or an open overlay; scroll it there.
                let over_list = (overlay_open || self.layout_mode() == SplitLayout::Docked)
                    && pos.x < self.rect.x0 + CHANGES_COL_W
                    && self.flap_open();
                if over_list {
                    self.changes_scroll = (self.changes_scroll - dy).max(0.0);
                } else {
                    // Clamp at the pitch and header height the last draw
                    // painted, so 2× rows scroll as far as they extend.
                    let metrics = self.diff_metrics;
                    let max = self
                        .current_diff()
                        .map_or(0.0, |d| DiffView::content_height_with_metrics(d, &metrics))
                        - (self.rect.height() - self.appbar_h - self.diff_header_h);
                    self.diff_scroll = (self.diff_scroll - dy).clamp(0.0, max.max(0.0));
                }
            }
            Screen::History => {
                let list_w = if overlay_open {
                    FLAP_W
                } else {
                    (self.rect.width() * 0.56).max(420.0)
                };
                let over_list = pos.x < self.rect.x0 + list_w && self.flap_open();
                if over_list {
                    let max = HistoryView::content_height(&self.model, &self.history_metrics)
                        - (self.rect.height() - self.appbar_h);
                    self.history_scroll = (self.history_scroll - dy).clamp(0.0, max.max(0.0));
                } else {
                    self.detail_scroll = (self.detail_scroll - dy).max(0.0);
                }
            }
            Screen::Panels => {}
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this is the single derived semantic inventory for the app's three mutually exclusive screens"
    )]
    fn accessibility_targets(&self) -> Vec<(String, Rect, ui::a11y::AccessNode)> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let button = |target: &str, rect: Rect, label: String| {
            (
                target.to_string(),
                rect,
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(rect),
            )
        };
        if self.awaiting_repo {
            let mut targets = Vec::new();
            if self.open_input_rect.area() > 0.0 {
                targets.push((
                    "git.open-path".into(),
                    self.open_input_rect,
                    self.open_input
                        .accessibility()
                        .unwrap_or_else(|| {
                            AccessNode::new(Role::TextInput).with_name("Repository path")
                        })
                        .with_bounds(self.open_input_rect),
                ));
            }
            if self.open_button_rect.area() > 0.0 {
                targets.push(button(
                    "git.open-repository",
                    self.open_button_rect,
                    "Open repository".into(),
                ));
            }
            return targets;
        }

        let mut targets = Vec::new();
        if self.hamburger_rect.area() > 0.0 {
            targets.push(button(
                "git.navigation-toggle",
                self.hamburger_rect,
                if self.flap_open() {
                    "Hide repository navigation"
                } else {
                    "Show repository navigation"
                }
                .into(),
            ));
        }
        // An open compact flap is a modal navigation surface: the visual
        // scrim, pointer routing, keyboard routing and semantic tree must all
        // agree that obscured detail/app-bar controls are temporarily inert.
        let overlay_modal = self.layout_mode() == SplitLayout::Overlay && self.flap_open();
        if !overlay_modal && self.status_rect.area() > 0.0 {
            targets.push((
                "git.status".into(),
                self.status_rect,
                AccessNode::new(Role::StaticText)
                    .with_name(&self.status_text)
                    .with_value(&self.status_text)
                    .with_live(ui::a11y::Politeness::Polite)
                    .with_bounds(self.status_rect),
            ));
        }
        if !overlay_modal {
            targets.extend(self.tab_rects.iter().map(|(screen, rect)| {
                let label = TABS
                    .iter()
                    .find(|(candidate, _)| candidate == screen)
                    .map_or("Repository view", |(_, label)| *label);
                (
                    match screen {
                        Screen::Changes => "git.screen.changes",
                        Screen::History => "git.screen.history",
                        Screen::Panels => "git.screen.branches",
                    }
                    .to_string(),
                    *rect,
                    AccessNode::new(Role::Tab)
                        .with_name(label)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(*screen == self.screen),
                        )
                        .with_bounds(*rect),
                )
            }));
        }
        if !overlay_modal && self.settings_seg.area() > 0.0 {
            targets.push(button(
                "git.diff-layout",
                self.settings_seg,
                "Diff layout".into(),
            ));
        }
        if !overlay_modal && self.settings_syn.area() > 0.0 {
            targets.push(button(
                "git.syntax-highlight",
                self.settings_syn,
                "Syntax highlighting".into(),
            ));
        }
        match self.screen {
            Screen::Changes if self.commit_input.get_rect().area() > 0.0 => {
                for (rect, hit) in &self.change_semantic_hits {
                    if matches!(hit, ChangesHit::FocusMessage) {
                        continue;
                    }
                    let (target, name, role, selected) = match hit {
                        ChangesHit::SelectFile(area, row) => {
                            let list = match area {
                                ChangeArea::Staged => &self.model.staged,
                                ChangeArea::Unstaged => &self.model.unstaged,
                                ChangeArea::Untracked => &self.model.untracked,
                            };
                            let Some(change) = list.get(*row) else {
                                continue;
                            };
                            (
                                format!("git.change.open:{area:?}:{}", change.path),
                                format!("Open {} diff, {}", change.path, change.kind.badge()),
                                Role::ListItem,
                                self.selected_change == Some((*area, *row)),
                            )
                        }
                        ChangesHit::ToggleStage(area, row) => {
                            let verb = if *area == ChangeArea::Staged {
                                "Unstage"
                            } else {
                                "Stage"
                            };
                            let list = match area {
                                ChangeArea::Staged => &self.model.staged,
                                ChangeArea::Unstaged => &self.model.unstaged,
                                ChangeArea::Untracked => &self.model.untracked,
                            };
                            let Some(change) = list.get(*row) else {
                                continue;
                            };
                            (
                                format!("git.change.{verb}:{area:?}:{}", change.path),
                                format!("{verb} {}", change.path),
                                Role::Button,
                                false,
                            )
                        }
                        ChangesHit::StageAll(area) => (
                            format!("git.changes.stage-all:{area:?}"),
                            "Stage all changes".into(),
                            Role::Button,
                            false,
                        ),
                        ChangesHit::UnstageAll(area) => (
                            format!("git.changes.unstage-all:{area:?}"),
                            "Unstage all changes".into(),
                            Role::Button,
                            false,
                        ),
                        ChangesHit::Commit => (
                            "git.changes.commit".into(),
                            "Commit staged changes".into(),
                            Role::Button,
                            false,
                        ),
                        ChangesHit::FocusMessage => continue,
                    };
                    targets.push((
                        target,
                        *rect,
                        AccessNode::new(role)
                            .with_name(name)
                            .with_state(AccessState::new().focusable(true).selected(selected))
                            .with_bounds(*rect),
                    ));
                }
                targets.push((
                    "git.commit-message".into(),
                    self.commit_input.get_rect(),
                    self.commit_input
                        .accessibility()
                        .unwrap_or_else(|| {
                            AccessNode::new(Role::TextInput).with_name("Commit message")
                        })
                        .with_bounds(self.commit_input.get_rect()),
                ));
                if !overlay_modal {
                    for hunk in &self.diff_hunk_buttons {
                        let label = match hunk.action {
                            diffview::HunkAction::Toggle if self.selected_is_staged() => {
                                "Unstage hunk"
                            }
                            diffview::HunkAction::Toggle => "Stage hunk",
                            diffview::HunkAction::Discard => "Discard hunk",
                        };
                        targets.push(button(
                            &format!("git.hunk:{}:{:?}", hunk.hunk, hunk.action),
                            hunk.rect,
                            label.into(),
                        ));
                    }
                }
            }
            Screen::History => {
                for (index, rect) in self.history_rows.iter().enumerate() {
                    if let Some(commit) = self.model.commits.get(index) {
                        targets.push((
                            format!("git.commit:{}", commit.id),
                            *rect,
                            AccessNode::new(Role::ListItem)
                                .with_name(format!(
                                    "{} by {}, {}",
                                    commit.summary, commit.author_name, commit.when
                                ))
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .selected(index == self.selected_commit),
                                )
                                .with_bounds(*rect),
                        ));
                    }
                }
            }
            Screen::Panels => {
                for (rect, hit) in &self.panel_semantic_hits {
                    let (target, label) = match hit {
                        PanelHit::Remote(RemoteAction::Fetch) => {
                            ("git.remote.fetch".into(), "Fetch from remote".into())
                        }
                        PanelHit::Remote(RemoteAction::Pull) => {
                            ("git.remote.pull".into(), "Pull from remote".into())
                        }
                        PanelHit::Remote(RemoteAction::Push) => {
                            ("git.remote.push".into(), "Push to remote".into())
                        }
                        PanelHit::Branch(BranchAction::Checkout(name)) => (
                            format!("git.branch.checkout:{name}"),
                            format!("Check out {name}"),
                        ),
                        PanelHit::Branch(BranchAction::Delete(name)) => (
                            format!("git.branch.delete:{name}"),
                            format!("Delete {name}"),
                        ),
                        PanelHit::Branch(BranchAction::StashApply(index)) => (
                            format!("git.stash.apply:{index}"),
                            format!("Apply stash {index}"),
                        ),
                    };
                    targets.push(button(&target, *rect, label));
                }
                if self.new_branch_input.area() > 0.0 {
                    targets.push((
                        "git.new-branch-name".into(),
                        self.new_branch_input,
                        self.branch_input
                            .accessibility()
                            .unwrap_or_else(|| {
                                AccessNode::new(Role::TextInput).with_name("New branch name")
                            })
                            .with_bounds(self.new_branch_input),
                    ));
                }
                if self.new_branch_button.area() > 0.0 {
                    targets.push(button(
                        "git.create-branch",
                        self.new_branch_button,
                        "Create branch".into(),
                    ));
                }
            }
            Screen::Changes => {}
        }
        if !overlay_modal && self.discard_file_rect.area() > 0.0 {
            targets.push(button(
                "git.discard-file",
                self.discard_file_rect,
                "Discard file changes".into(),
            ));
        }
        targets
    }
}

/// Whether `p` falls inside a non-empty `rect` (a zero-size rect never hits).
fn rect_hit(rect: Rect, p: Point) -> bool {
    rect.width() > 0.0 && rect.contains(p)
}

fn first_change(model: &RepoModel) -> Option<(ChangeArea, usize)> {
    if !model.staged.is_empty() {
        Some((ChangeArea::Staged, 0))
    } else if !model.unstaged.is_empty() {
        Some((ChangeArea::Unstaged, 0))
    } else if !model.untracked.is_empty() {
        Some((ChangeArea::Untracked, 0))
    } else {
        None
    }
}

impl Widget for GitClientApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::GIT_CLIENT_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        self.drain_remote();
        if self.settings_seeded {
            // Adopt document changes: remote collaborators, and this app's own
            // preference actions, which the framework `ActionHost` applies
            // straight to the document through the executor.
            self.settings = Settings::read(state);
        } else {
            // First sync: adopt any persisted preferences, then seed the doc so
            // it is the source of truth and collaborators see the same
            // preferences. An initial override forced at construction (the
            // GIT_CLIENT_SPLIT screenshot path) wins over the persisted value.
            if !self.settings_forced {
                // Read without creating the map. The preference actions below
                // are what write it, hosted or standalone.
                self.settings = Settings::read_only(state);
            }
            self.publish_settings(state);
            self.settings_seeded = true;
        }
        self.publish_shared_repo_if_needed(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The app draws itself; a single full-size leaf node carries it.
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see the
        // `TaffyTreeExt` doc comment in `ui::widget::mod`), and
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so there is nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Real fallback rather than a panic if `node` somehow doesn't
        // belong to this tree, matching every `ui` widget's `apply_layout`.
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // The layout mode is now known from the allocated width — reconcile
        // the flap so a window resize across the breakpoint adopts the new
        // mode's default (wide ⇒ docked-open, narrow ⇒ overlay-closed) unless
        // the user expressed an explicit preference this session.
        self.flap.reconcile(self.layout_mode());
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);

        // Honest empty state: no repository bound. Show the open-a-repository
        // screen instead of the workspace chrome.
        if self.awaiting_repo {
            self.draw_open(&mut ctx, presence);
            return;
        }

        // Advance the flap reveal toward its target so the slide + scrim fade
        // are smooth. The live loop keeps redrawing while
        // `needs_animation_frame` is true.
        self.flap.reconcile(self.layout_mode());
        self.tick_flap();

        ctx.fill_rect(self.rect, theme.tokens.colors.surfaces.base);

        self.draw_appbar(&mut ctx);

        let body = Rect::new(
            self.rect.x0,
            self.rect.y0 + self.appbar_h,
            self.rect.x1,
            self.rect.y1,
        );
        match self.screen {
            Screen::Changes => self.draw_changes(&mut ctx, presence, body),
            Screen::History => self.draw_history(&mut ctx, body),
            Screen::Panels => self.draw_panels(&mut ctx, presence, body),
        }
        if self.layout_mode() == SplitLayout::Overlay && self.flap_open() {
            // Repaint the toggle above the open flap at the rect the app bar
            // placed it — the same measured row, not a fixed 1× bar height.
            let btn = self.hamburger_rect;
            self.paint_hamburger(&mut ctx, btn);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                // Hover state for hunk buttons + remote actions.
                self.hover_hunk =
                    diffview::hunk_button_at(&self.diff_hunk_buttons, self.last_mouse);
                self.hover_action = None;
                for f in &self.panel_hits {
                    if let Some(PanelHit::Remote(a)) = f(self.last_mouse) {
                        self.hover_action = Some(a);
                        break;
                    }
                }
                // Keep an in-flight text-selection drag updating.
                self.forward_to_inputs(&mut InputEventCtx {
                    event,
                    scale_factor: ctx.scale_factor,
                    modifiers: *ctx.modifiers,
                    state: ctx.state,
                    path: ctx.path,
                    taffy: ctx.taffy,
                });
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_press(&mut InputEventCtx {
                    event,
                    scale_factor: ctx.scale_factor,
                    modifiers: *ctx.modifiers,
                    state: ctx.state,
                    path: ctx.path,
                    taffy: ctx.taffy,
                });
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                self.forward_to_inputs(&mut InputEventCtx {
                    event,
                    scale_factor: ctx.scale_factor,
                    modifiers: *ctx.modifiers,
                    state: ctx.state,
                    path: ctx.path,
                    taffy: ctx.taffy,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 36.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                self.scroll_active(dy, self.last_mouse);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { .. } => {
                self.handle_key(
                    event,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.taffy,
                    ctx.handled,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "GitClientApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let children = self
            .accessibility_targets()
            .into_iter()
            .enumerate()
            .map(|(index, (target, _, node))| {
                // The ordinal is only the synthetic-tree geometry key. The
                // action target is semantic and stable across responsive
                // layouts, so assistive technology never activates a
                // different control after a row appears or disappears.
                node.with_key(index as u64).with_action_target(target)
            })
            .collect();
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("Git Client")
                .with_description(if self.remote_job.is_some() {
                    ui::i18n!("git-remote-running")
                } else if self.awaiting_repo {
                    "Open a repository".to_string()
                } else {
                    "Repository changes, history, branches and remotes".to_string()
                })
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        self.accessibility_targets()
            .get(*index)
            .map(|(_, rect, _)| *rect)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, path) = ui::doc::document(state, path);

        let field = match target {
            "git.open-path" => Some(Field::Open),
            "git.commit-message" => Some(Field::Commit),
            "git.new-branch-name" => Some(Field::Branch),
            _ => None,
        };
        if let Some(field) = field {
            if matches!(action, Action::Focus | Action::Activate) {
                self.set_focus(Some(field));
                return true;
            }
            let input = match field {
                Field::Open => &mut self.open_input,
                Field::Commit => &mut self.commit_input,
                Field::Branch => &mut self.branch_input,
            };
            return input.perform_action(action, state, path);
        }
        if !matches!(action, Action::Activate | Action::ToggleCheck) {
            return false;
        }
        if target == "git.open-repository" && self.awaiting_repo {
            self.open_repo_from_input();
            return true;
        }
        let Some(rect) = self
            .accessibility_targets()
            .into_iter()
            .find_map(|(candidate, rect, _)| (candidate == target).then_some(rect))
        else {
            return false;
        };
        self.handle_click(rect.center(), state);
        true
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.remote_job.as_ref()?.invocation != Some(invocation) {
            return None;
        }
        // libgit2 operations are currently non-cancellable. In particular,
        // never report a cancelled push when the server may have accepted it.
        self.drain_remote();
        let job = self.remote_job.as_mut()?;
        if let Some(result) = job.result.take() {
            self.remote_job = None;
            return Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "git.remote_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
        if !job.progress_sent {
            job.progress_sent = true;
            return Some(tilo_actions::InvocationUpdate::Progress {
                invocation,
                sequence: 1,
                phase: "git.remote".into(),
                completed: None,
                total: None,
                message: Some(ui::i18n!("git-remote-running")),
            });
        }
        None
    }

    fn needs_animation_frame(&self) -> bool {
        // Redraw while the flap animates, and while any embedded text field is
        // focused so its caret keeps blinking.
        self.flap.is_animating(self.layout_mode())
            || self
                .remote_job
                .as_ref()
                .is_some_and(|job| job.result.is_none())
            || self.commit_input.needs_animation_frame()
            || self.branch_input.needs_animation_frame()
            || self.open_input.needs_animation_frame()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

impl ui::resources::PublishesResources for GitClientApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the repository door — keep only live working-tree status and in-flight
    /// remote jobs.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        // The framework root already owns the repository door.
        if self.is_hosted() && id == "git_client.repo" {
            return None;
        }
        if let Some(found) = self.publish_resource(doc, id) {
            return Some(found);
        }
        if self.is_hosted() {
            return crate::resource::live_extras(self)
                .into_iter()
                .find(|resource| resource.id.as_str() == id);
        }
        None
    }
}

impl ui::undo::CommittedActions for GitClientApp {
    /// Hand the host the document heads captured before an on-screen preference
    /// control dispatched, so it records exactly one undo boundary bracketing
    /// it — the same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl GitClientApp {
    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use crate::actions::GitClientOperation;
        if type_id != crate::actions::GIT_CLIENT_OPERATION_TYPE {
            return Ok(None);
        }
        let operation: crate::actions::GitClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let remote = match (kind, operation) {
            (tilo_actions::OperationKind::Effect, GitClientOperation::Fetch { remote }) => {
                Some((RemoteAction::Fetch, remote))
            }
            (tilo_actions::OperationKind::Effect, GitClientOperation::Pull { remote }) => {
                Some((RemoteAction::Pull, remote))
            }
            (tilo_actions::OperationKind::Effect, GitClientOperation::Push { remote }) => {
                Some((RemoteAction::Push, remote))
            }
            _ => None,
        };
        if let Some((action, remote)) = remote {
            self.start_remote(action, Some(remote), Some(invocation))?;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        self.answer_resource_operation(kind, type_id, payload, doc, path)
            .map(|handled| {
                handled.then(|| {
                    ui::widget::ResourceOperationStart::Completed(
                        tilo_actions::ActionOutput::default(),
                    )
                })
            })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive typed protocol match keeps publication-to-handler coverage auditable"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != crate::actions::GIT_CLIENT_OPERATION_TYPE {
            return Ok(false);
        }
        if kind == tilo_actions::OperationKind::Effect && self.repository_busy() {
            return Err(ui::i18n!("git-remote-busy"));
        }
        let operation: crate::actions::GitClientOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match (kind, operation) {
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::ShowChanges,
            ) => {
                self.switch_screen(Screen::Changes);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::ShowHistory,
            ) => {
                self.switch_screen(Screen::History);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::ShowBranches,
            ) => {
                self.switch_screen(Screen::Panels);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::OpenChange { area, path },
            ) => {
                let area = match area.as_str() {
                    "staged" => ChangeArea::Staged,
                    "unstaged" => ChangeArea::Unstaged,
                    "untracked" => ChangeArea::Untracked,
                    _ => return Err("unknown change area".into()),
                };
                let list = match area {
                    ChangeArea::Staged => &self.model.staged,
                    ChangeArea::Unstaged => &self.model.unstaged,
                    ChangeArea::Untracked => &self.model.untracked,
                };
                let index = list
                    .iter()
                    .position(|change| change.path == path)
                    .ok_or_else(|| "changed file no longer exists".to_string())?;
                self.switch_screen(Screen::Changes);
                self.selected_change = Some((area, index));
                self.diff_scroll = 0.0;
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::GitClientOperation::OpenCommit { id },
            ) => {
                let index = self
                    .model
                    .commits
                    .iter()
                    .position(|commit| commit.id == id)
                    .ok_or_else(|| "commit is outside the current history window".to_string())?;
                self.switch_screen(Screen::History);
                self.selected_commit = index;
                self.detail_scroll = 0.0;
            }
            (tilo_actions::OperationKind::Effect, crate::actions::GitClientOperation::StageAll) => {
                self.stage_all();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::UnstageAll,
            ) => {
                self.unstage_all();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::StageFile { path },
            ) => {
                let found = self
                    .model
                    .unstaged
                    .iter()
                    .position(|change| change.path == path)
                    .map(|index| (ChangeArea::Unstaged, index))
                    .or_else(|| {
                        self.model
                            .untracked
                            .iter()
                            .position(|change| change.path == path)
                            .map(|index| (ChangeArea::Untracked, index))
                    })
                    .ok_or_else(|| "file is no longer unstaged".to_string())?;
                self.toggle_stage(found.0, found.1);
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::UnstageFile { path },
            ) => {
                let index = self
                    .model
                    .staged
                    .iter()
                    .position(|change| change.path == path)
                    .ok_or_else(|| "file is no longer staged".to_string())?;
                self.toggle_stage(ChangeArea::Staged, index);
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::DiscardFile { path },
            ) => {
                let found = self
                    .model
                    .unstaged
                    .iter()
                    .position(|change| change.path == path)
                    .map(|index| (ChangeArea::Unstaged, index))
                    .or_else(|| {
                        self.model
                            .untracked
                            .iter()
                            .position(|change| change.path == path)
                            .map(|index| (ChangeArea::Untracked, index))
                    })
                    .ok_or_else(|| "file has no worktree changes".to_string())?;
                if self.repo_path.is_none() {
                    return Err("discard is unavailable in the demonstration repository".into());
                }
                self.discard_file(found.0, found.1);
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::Commit { message },
            ) => {
                if self.model.staged.is_empty() {
                    return Err("there are no staged changes to commit".into());
                }
                if message.trim().is_empty() {
                    return Err("commit message cannot be empty".into());
                }
                self.commit_input.set_text(&message);
                self.do_commit();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::CreateBranch { name },
            ) => {
                if name.trim().is_empty() {
                    return Err("branch name cannot be empty".into());
                }
                if self.repo_path.is_none() {
                    return Err("branch creation requires a real repository".into());
                }
                self.branch_input.set_text(&name);
                self.create_new_branch();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::CheckoutBranch { name },
            ) => self.apply_branch_action(BranchAction::Checkout(name)),
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::DeleteBranch { name },
            ) => self.apply_branch_action(BranchAction::Delete(name)),
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::ApplyStash { index },
            ) => self.apply_branch_action(BranchAction::StashApply(index)),
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::SaveStash { message },
            ) => {
                let path = self
                    .repo()
                    .ok_or_else(|| "stashing requires a real repository".to_string())?;
                crate::repo::stash_save(&path, &message).map_err(|error| error.to_string())?;
                self.last_error = None;
                self.reload();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::GitClientOperation::Fetch { .. }
                | crate::actions::GitClientOperation::Pull { .. }
                | crate::actions::GitClientOperation::Push { .. },
            ) => return Err("remote operations require lifecycle-aware action dispatch".into()),
            _ => return Err("operation kind does not match Git operation".into()),
        }
        if kind == tilo_actions::OperationKind::Effect
            && let Some(error) = &self.last_error
        {
            return Err(error.clone());
        }
        Ok(true)
    }
}

impl GitClientApp {
    /// Forward one event to a single embedded [`TextInput`], returning whether
    /// it consumed the event.
    fn fwd(
        input: &mut TextInput,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        let mut ft: Option<NodeId> = None;
        input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: NodeId::new(0),
                taffy,
            },
        );
        handled
    }

    /// Forward a pointer event to every text field (so an in-flight drag-select
    /// keeps updating regardless of which field started it).
    fn forward_to_inputs(&mut self, ctx: &mut InputEventCtx<'_>) {
        Self::fwd(
            &mut self.commit_input,
            ctx.event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut *ctx.state,
            ctx.path,
            ctx.taffy,
        );
        Self::fwd(
            &mut self.branch_input,
            ctx.event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut *ctx.state,
            ctx.path,
            ctx.taffy,
        );
        Self::fwd(
            &mut self.open_input,
            ctx.event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut *ctx.state,
            ctx.path,
            ctx.taffy,
        );
    }

    /// Route a left-press: text fields first (focus + caret placement), then
    /// the geometry click router. Blurs any focused field on a click outside.
    fn handle_press(&mut self, ctx: &mut InputEventCtx<'_>) {
        let pos = self.last_mouse;
        // Empty "open a repository" state routes entirely to its own controls.
        if self.awaiting_repo {
            if self.open_button_rect.width() > 0.0 && self.open_button_rect.contains(pos) {
                self.open_repo_from_input();
                return;
            }
            if self.open_input_rect.width() > 0.0 && self.open_input_rect.contains(pos) {
                self.set_focus(Some(Field::Open));
                Self::fwd(
                    &mut self.open_input,
                    ctx.event,
                    ctx.scale_factor,
                    ctx.modifiers,
                    &mut *ctx.state,
                    ctx.path,
                    ctx.taffy,
                );
                return;
            }
            self.set_focus(None);
            return;
        }
        // A press on a visible text field focuses it and places the caret.
        if self.screen == Screen::Changes && rect_hit(self.commit_input.get_rect(), pos) {
            self.set_focus(Some(Field::Commit));
            Self::fwd(
                &mut self.commit_input,
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                &mut *ctx.state,
                ctx.path,
                ctx.taffy,
            );
            return;
        }
        if self.screen == Screen::Panels && rect_hit(self.new_branch_input, pos) {
            self.set_focus(Some(Field::Branch));
            Self::fwd(
                &mut self.branch_input,
                ctx.event,
                ctx.scale_factor,
                ctx.modifiers,
                &mut *ctx.state,
                ctx.path,
                ctx.taffy,
            );
            return;
        }
        // Anywhere else: blur the fields and run the normal click router.
        self.set_focus(None);
        self.handle_click(pos, ctx.state);
    }

    /// Submit the focused field's action (Enter).
    fn submit_focused(&mut self, field: Field) {
        match field {
            Field::Commit => self.do_commit(),
            Field::Branch => self.create_new_branch(),
            Field::Open => self.open_repo_from_input(),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event contract and modal/global/list keyboard layers are kept together"
    )]
    fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
        handled: &mut bool,
    ) {
        let WindowEvent::KeyboardInput { event: ev, .. } = event else {
            return;
        };
        if ev.state != ElementState::Pressed {
            return;
        }

        // A focused text field takes the keys, except Enter (submit) and Escape
        // (blur) which the app intercepts. Text editing — cursor, selection,
        // word-nav, clipboard — is all handled by the toolkit TextInput.
        if let Some(field) = self.focus {
            match &ev.logical_key {
                Key::Named(NamedKey::Enter) => {
                    self.submit_focused(field);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.set_focus(None);
                    *handled = true;
                }
                _ => {
                    let input: &mut TextInput = match field {
                        Field::Commit => &mut self.commit_input,
                        Field::Branch => &mut self.branch_input,
                        Field::Open => &mut self.open_input,
                    };
                    if Self::fwd(input, event, 1.0, modifiers, state, path, taffy) {
                        *handled = true;
                    }
                }
            }
            return;
        }

        // The empty-state screen has no other keys.
        if self.awaiting_repo {
            return;
        }

        // Esc dismisses an open overlay flap (Adwaita flap behaviour).
        if let Key::Named(NamedKey::Escape) = &ev.logical_key
            && self.layout_mode() == SplitLayout::Overlay
            && self.flap_open()
        {
            self.flap.close();
            *handled = true;
            return;
        }

        if self.layout_mode() == SplitLayout::Overlay && self.flap_open() {
            match (&ev.logical_key, self.screen) {
                (Key::Named(NamedKey::ArrowDown), Screen::History) => {
                    if self.selected_commit + 1 < self.model.commits.len() {
                        self.selected_commit += 1;
                        self.detail_scroll = 0.0;
                    }
                }
                (Key::Named(NamedKey::ArrowUp), Screen::History) => {
                    self.selected_commit = self.selected_commit.saturating_sub(1);
                    self.detail_scroll = 0.0;
                }
                (Key::Named(NamedKey::ArrowDown), Screen::Changes) => {
                    self.select_next_change(1);
                }
                (Key::Named(NamedKey::ArrowUp), Screen::Changes) => {
                    self.select_next_change(-1);
                }
                _ => {}
            }
            *handled = true;
            return;
        }

        // Global shortcuts: 1/2/3 switch screens.
        if let Key::Character(s) = &ev.logical_key {
            match s.as_str() {
                "1" => {
                    self.switch_screen(Screen::Changes);
                    *handled = true;
                }
                "2" => {
                    self.switch_screen(Screen::History);
                    *handled = true;
                }
                "3" => {
                    self.switch_screen(Screen::Panels);
                    *handled = true;
                }
                "d" => {
                    self.toggle_diff_layout(state);
                    *handled = true;
                }
                _ => {}
            }
        }

        // Arrow navigation in the active list.
        match (&ev.logical_key, self.screen) {
            (Key::Named(NamedKey::ArrowDown), Screen::History) => {
                if self.selected_commit + 1 < self.model.commits.len() {
                    self.selected_commit += 1;
                    self.detail_scroll = 0.0;
                }
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowUp), Screen::History) => {
                self.selected_commit = self.selected_commit.saturating_sub(1);
                self.detail_scroll = 0.0;
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowDown), Screen::Changes) => {
                self.select_next_change(1);
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowUp), Screen::Changes) => {
                self.select_next_change(-1);
                *handled = true;
            }
            _ => {}
        }
    }

    fn select_next_change(&mut self, dir: i32) {
        let all = self.model.all_changes().len();
        if all == 0 {
            return;
        }
        // Flatten current selection to a global index.
        let cur = match self.selected_change {
            Some((ChangeArea::Staged, i)) => i,
            Some((ChangeArea::Unstaged, i)) => self.model.staged.len() + i,
            Some((ChangeArea::Untracked, i)) => {
                self.model.staged.len() + self.model.unstaged.len() + i
            }
            None => 0,
        } as i32;
        let next = (cur + dir).clamp(0, all as i32 - 1) as usize;
        // Map back to (area, idx).
        let s = self.model.staged.len();
        let u = self.model.unstaged.len();
        self.selected_change = Some(if next < s {
            (ChangeArea::Staged, next)
        } else if next < s + u {
            (ChangeArea::Unstaged, next - s)
        } else {
            (ChangeArea::Untracked, next - s - u)
        });
        self.diff_scroll = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    /// Build an app over the bundled sample, laid out at `w` x 800 with the
    /// flap settled to its mode default so geometry is deterministic.
    fn app_at(w: f64, screen: Screen) -> GitClientApp {
        let (model, diffs) = crate::sample::model();
        let mut app = GitClientApp::from_model(model, diffs);
        app.screen = screen;
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
        app.flap.reconcile(app.layout_mode());
        // Settle the reveal to the resolved default for this width.
        app.flap.snap(app.flap_open());
        app
    }

    /// The first sync publishes preferences through the action sink.
    #[test]
    fn a_fresh_client_publishes_preferences_through_the_sink() {
        let mut app = GitClientApp::new();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        let recorded = app
            .actions
            .last_dispatched()
            .expect("the first sync submits a preference action");
        assert_eq!(
            recorded.type_id.as_str(),
            crate::actions::GIT_CLIENT_ACTION_TYPE
        );
        assert_eq!(
            crate::settings::Settings::read_only(&state.doc).diff_layout,
            app.settings.diff_layout
        );
    }

    /// Adaptive screen mode: wide Changes docks, narrow master-detail overlays closed,
    /// the breakpoint boundary is inclusive, Panels never collapses, and crossing the
    /// breakpoint restores the mode default. Each phase is a former test with its own fresh fixture.
    #[test]
    fn screen_mode_follows_the_breakpoint_and_preset() {
        wide_changes_screen_is_docked();
        narrow_master_detail_screen_is_overlay_closed();
        breakpoint_boundary_is_inclusive_docked();
        panels_screen_never_collapses();
        crossing_breakpoint_restores_mode_default();
    }

    fn wide_changes_screen_is_docked() {
        let app = app_at(1280.0, Screen::Changes);
        assert_eq!(app.layout_mode(), SplitLayout::Docked);
        assert!(app.flap_open(), "docked list column is open by default");
    }

    fn narrow_master_detail_screen_is_overlay_closed() {
        let app = app_at(680.0, Screen::Changes);
        assert_eq!(app.layout_mode(), SplitLayout::Overlay);
        assert!(!app.flap_open(), "narrow overlay flap is closed by default");
    }

    fn breakpoint_boundary_is_inclusive_docked() {
        assert_eq!(
            app_at(NARROW_BREAKPOINT, Screen::Changes).layout_mode(),
            SplitLayout::Docked,
        );
        assert_eq!(
            app_at(NARROW_BREAKPOINT - 1.0, Screen::Changes).layout_mode(),
            SplitLayout::Overlay,
        );
    }

    fn panels_screen_never_collapses() {
        // The full-width dashboard has no list column, so it stays docked even
        // on a narrow window — no flap, scrim or hamburger.
        let app = app_at(400.0, Screen::Panels);
        assert_eq!(app.layout_mode(), SplitLayout::Docked);
    }

    /// The overlay flap: the hamburger toggles it, a scrim click dismisses it, a click
    /// inside keeps it open, and while open it hides and blocks background controls.
    /// Each phase is a former test with its own fresh fixture.
    #[test]
    fn overlay_flap_toggles_dismisses_and_blocks_the_background() {
        hamburger_click_toggles_overlay_flap();
        scrim_click_dismisses_open_overlay_flap();
        click_inside_open_flap_does_not_dismiss_it();
        open_overlay_hides_and_blocks_background_controls();
    }

    fn hamburger_click_toggles_overlay_flap() {
        let mut app = app_at(680.0, Screen::Changes);
        assert!(!app.flap_open());
        // Simulate the hamburger being drawn this frame, then click it.
        app.hamburger_rect = Rect::new(12.0, 12.0, 44.0, 44.0);
        app.handle_click(Point::new(28.0, 28.0), &mut AutoCommit::new());
        assert!(app.flap_open(), "hamburger summons the overlay flap");
        app.handle_click(Point::new(28.0, 28.0), &mut AutoCommit::new());
        assert!(!app.flap_open(), "hamburger dismisses it again");
    }

    fn scrim_click_dismisses_open_overlay_flap() {
        let mut app = app_at(680.0, Screen::Changes);
        app.flap.snap(true); // open overlay flap
        assert!(app.flap_open());
        // Click far to the right of the list column (over the dimmed detail).
        app.handle_click(Point::new(600.0, 400.0), &mut AutoCommit::new());
        assert!(!app.flap_open(), "clicking the scrim closes the flap");
    }

    fn click_inside_open_flap_does_not_dismiss_it() {
        let mut app = app_at(680.0, Screen::Changes);
        app.flap.snap(true);
        // A click within the list column must not be treated as a scrim tap.
        app.handle_click(Point::new(40.0, 400.0), &mut AutoCommit::new());
        assert!(
            app.flap_open(),
            "clicks on the list column keep the flap open"
        );
    }

    fn open_overlay_hides_and_blocks_background_controls() {
        let mut app = app_at(680.0, Screen::Changes);
        app.flap.snap(true);
        app.hamburger_rect = Rect::new(12.0, 12.0, 44.0, 44.0);
        app.tab_rects = vec![(Screen::History, Rect::new(100.0, 12.0, 180.0, 44.0))];
        app.settings_seg = Rect::new(450.0, 12.0, 560.0, 44.0);
        let semantic = app.accessibility_targets();
        assert!(
            semantic
                .iter()
                .any(|(target, _, _)| target == "git.navigation-toggle")
        );
        assert!(
            semantic
                .iter()
                .all(|(target, _, _)| target != "git.screen.history" && target != "git.diff-layout"),
            "obscured app-bar controls must not remain in the screen-reader tree"
        );

        app.handle_click(Point::new(140.0, 28.0), &mut AutoCommit::new());
        assert_eq!(app.screen, Screen::Changes, "covered tab click is consumed");
        assert!(
            app.flap_open(),
            "covered app-bar click does not dismiss the drawer"
        );
    }

    fn crossing_breakpoint_restores_mode_default() {
        // Open the flap while docked-wide, then narrow past the breakpoint:
        // the stale preference clears and the overlay default (closed) wins.
        let mut app = app_at(1280.0, Screen::Changes);
        app.flap.close(); // user collapses the docked column
        assert!(!app.flap_open());
        app.rect = Rect::new(0.0, 0.0, 680.0, 800.0);
        app.flap.reconcile(app.layout_mode());
        assert!(!app.flap_open(), "overlay default is closed");
        // Widen again → back to docked-open default.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.flap.reconcile(app.layout_mode());
        assert!(app.flap_open(), "docked default restores the open column");
    }

    /// Semantic parity: the app-bar diff-layout toggle equals the palette action,
    /// semantic open-repository and text edits reach their inputs, and synthetic
    /// accessibility controls sit at their drawn rects. Each phase is a former test with its own fresh fixture.
    #[test]
    fn affordances_and_semantic_controls_reach_the_production_actions() {
        appbar_diff_layout_toggle_matches_the_palette_action();
        semantic_open_repository_binds_the_entered_repository();
        semantic_text_edits_reach_embedded_repository_commit_and_branch_inputs();
        synthetic_accessibility_controls_are_addressable_at_their_drawn_rects();
    }

    /// The affordance == `ActionSpec` covenant on the *real* production path:
    /// the app-bar diff-layout control mutates the shared document, records one
    /// undo boundary, and dispatches the byte-identical `EncodedAction` the
    /// Ctrl+K palette would — one source of truth, not two code paths.
    fn appbar_diff_layout_toggle_matches_the_palette_action() {
        let (model, diffs) = crate::sample::model();
        let mut app = GitClientApp::from_model(model, diffs);
        let mut doc = AutoCommit::new();
        // First sync seeds the default (unified) preferences into the document.
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(Settings::read_only(&doc).diff_layout, DiffLayout::Unified);

        // The action Ctrl+K would dispatch right now: flip unified → split.
        let resources = ui::resources::resources(&app, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "diff-layout")
            .unwrap();
        let palette_action = match &spec.input {
            tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => {
                panic!("diff-layout is an immediate action")
            }
        };

        // Drive the real app-bar segmented control.
        app.toggle_diff_layout(&mut doc);

        // It mutated the shared document...
        assert_eq!(
            Settings::read_only(&doc).diff_layout,
            DiffLayout::SideBySide,
            "the control switched the persisted layout"
        );
        // ...recorded exactly one undo boundary bracketing the toggle...
        assert!(
            ui::undo::take_committed_action(&mut app).is_some(),
            "one undo boundary recorded for the control's action"
        );
        // ...and dispatched the byte-identical action Ctrl+K would.
        let dispatched = app
            .last_dispatched()
            .expect("the control dispatched an action");
        assert_eq!(dispatched.type_id, crate::actions::GIT_CLIENT_ACTION_TYPE);
        assert_eq!(
            dispatched, &palette_action,
            "the app-bar control and the Ctrl+K entry build one and the same action"
        );
    }

    /// App-bar geometry: controls stay visible and separate at narrow widths, inside
    /// the window without overlap, on one centre line per row, and a wrapped bar
    /// moves the body down. Each phase is a former test with its own fresh fixture.
    #[test]
    fn appbar_controls_lay_out_inside_the_window_without_overlap() {
        appbar_controls_stay_visible_and_separate_at_narrow_widths();
        appbar_controls_stay_inside_the_window_and_never_overlap();
        appbar_controls_share_one_centre_line_per_row();
        wrapped_appbar_moves_the_body_down_with_it();
    }

    fn appbar_controls_stay_visible_and_separate_at_narrow_widths() {
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        for width in [320.0, 460.0, 680.0, 1280.0] {
            for screen in [Screen::Changes, Screen::History, Screen::Panels] {
                let mut app = app_at(width, screen);
                let mut scene = Scene::new();
                app.draw_appbar(&mut PaintCtx::new(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut text,
                    &theme,
                ));
                for (_, rect) in &app.tab_rects {
                    assert!(
                        rect.width() + 0.5 >= f64::from(theme.tokens.metrics.min_pointer_target),
                        "tab target shrank below the theme minimum at {width}px: {rect:?}"
                    );
                }
                let mut controls: Vec<Rect> = app.tab_rects.iter().map(|(_, rect)| *rect).collect();
                controls.extend([app.settings_seg, app.settings_syn]);
                if app.hamburger_rect.area() > 0.0 {
                    controls.push(app.hamburger_rect);
                }
                for (index, rect) in controls.iter().enumerate() {
                    assert!(
                        rect.x0 >= 0.0
                            && rect.x1 <= width
                            && rect.y0 >= 0.0
                            && rect.y1 <= app.appbar_h,
                        "control {index} is outside the {width}px window: {rect:?}"
                    );
                    for other in controls.iter().skip(index + 1) {
                        assert!(
                            !(rect.x0 < other.x1
                                && rect.x1 > other.x0
                                && rect.y0 < other.y1
                                && rect.y1 > other.y0),
                            "controls overlap at {width}px: {rect:?} and {other:?}"
                        );
                    }
                }
            }
        }
    }

    fn semantic_open_repository_binds_the_entered_repository() {
        let directory = tempfile::tempdir().unwrap();
        let _repository = git2::Repository::init(directory.path()).unwrap();
        let mut app = GitClientApp::open_empty();
        app.open_input.set_text(directory.path().to_str().unwrap());
        app.open_button_rect = Rect::new(20.0, 20.0, 180.0, 60.0);
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.open-repository",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(!app.test_awaiting_repo());
        assert_eq!(app.test_last_error(), None);
    }

    fn semantic_text_edits_reach_embedded_repository_commit_and_branch_inputs() {
        let mut app = GitClientApp::open_empty();
        let mut doc = automerge::AutoCommit::new();
        for (target, text) in [
            ("git.open-path", r"C:\Work\Grün 🌍"),
            ("git.commit-message", "Keep Unicode: Grün 🌍"),
            ("git.new-branch-name", "feature/grün"),
        ] {
            assert!(Widget::perform_semantic_action(
                &mut app,
                target,
                &ui::a11y::Action::SetValue(text.into()),
                &mut doc,
                &automerge::ROOT,
            ));
        }
        assert_eq!(app.open_input.value(), r"C:\Work\Grün 🌍");
        assert_eq!(app.commit_input.value(), "Keep Unicode: Grün 🌍");
        assert_eq!(app.branch_input.value(), "feature/grün");
        assert!(app.test_awaiting_repo(), "editing a path must not open it");
    }

    fn synthetic_accessibility_controls_are_addressable_at_their_drawn_rects() {
        let mut app = app_at(1_280.0, Screen::Changes);
        app.tab_rects = vec![
            (Screen::Changes, Rect::new(20.0, 8.0, 120.0, 40.0)),
            (Screen::History, Rect::new(124.0, 8.0, 224.0, 40.0)),
        ];
        app.settings_seg = Rect::new(1_000.0, 8.0, 1_080.0, 40.0);
        app.settings_syn = Rect::new(1_084.0, 8.0, 1_160.0, 40.0);
        app.commit_input
            .apply_layout_external(Rect::new(360.0, 700.0, 700.0, 736.0));

        let node = Widget::accessibility(&app).expect("git client is semantic");
        assert!(!node.children.is_empty());
        let targets: std::collections::HashSet<_> = node
            .children
            .iter()
            .filter_map(|child| child.action_target.as_deref())
            .collect();
        assert_eq!(targets.len(), node.children.len());
        assert!(targets.contains("git.screen.changes"));
        assert!(targets.contains("git.screen.history"));
        assert!(targets.contains("git.diff-layout"));
        assert!(targets.contains("git.syntax-highlight"));
        assert!(targets.contains("git.commit-message"));
        for (index, child) in node.children.iter().enumerate() {
            assert_eq!(
                Widget::synthetic_accessibility_rect(&app, &[index]),
                child.bounds.map(ui::a11y::AccessBounds::to_rect),
                "semantic control {index} must activate inside the visible control"
            );
        }

        let mut doc = automerge::AutoCommit::new();
        let before = app.settings.diff_layout;
        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.diff-layout",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_ne!(app.settings.diff_layout, before);
        assert!(app.last_dispatched().is_some());

        assert!(Widget::perform_semantic_action(
            &mut app,
            "git.commit-message",
            &ui::a11y::Action::Focus,
            &mut doc,
            &automerge::ROOT,
        ));
        assert_eq!(app.focus, Some(Field::Commit));
    }

    /// Paint one frame with a real text renderer so every measured app-bar
    /// rect (`tab_rects`, settings cluster, hamburger, title block) is the
    /// production geometry, not a hand-poked stand-in.
    fn draw_frame(app: &mut GitClientApp) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(
            app,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
    }

    fn intersects(a: Rect, b: Rect) -> bool {
        a.area() > 0.0 && b.area() > 0.0 && a.intersect(b).area() > 0.0
    }

    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    fn appbar_controls_stay_inside_the_window_and_never_overlap() {
        for width in [390.0, 480.0, 680.0, 800.0, 1280.0] {
            let mut app = app_at(width, Screen::Changes);
            draw_frame(&mut app);

            let mut controls: Vec<(&str, Rect)> = Vec::new();
            for (screen, rect) in &app.tab_rects {
                controls.push((
                    match screen {
                        Screen::Changes => "tab.changes",
                        Screen::History => "tab.history",
                        Screen::Panels => "tab.panels",
                    },
                    *rect,
                ));
            }
            controls.push(("settings.layout", app.settings_seg));
            controls.push(("settings.syntax", app.settings_syn));
            if app.hamburger_rect.area() > 0.0 {
                controls.push(("hamburger", app.hamburger_rect));
            }
            controls.push(("title", app.title_rect));
            assert_eq!(
                app.tab_rects.len(),
                TABS.len(),
                "{width}: every screen tab is placed"
            );

            let bar = Rect::new(0.0, 0.0, width, app.appbar_h);
            for (name, rect) in &controls {
                assert!(
                    rect.area() > 0.0,
                    "{width}: {name} must be painted with a real rect"
                );
                assert!(
                    rect.x0 >= bar.x0 - 0.01 && rect.x1 <= bar.x1 + 0.01,
                    "{width}: {name} {rect:?} leaves the window horizontally"
                );
                assert!(
                    rect.y0 >= bar.y0 - 0.01 && rect.y1 <= bar.y1 + 0.01,
                    "{width}: {name} {rect:?} leaves the app bar {bar:?}"
                );
            }
            for (i, (a_name, a)) in controls.iter().enumerate() {
                for (b_name, b) in controls.iter().skip(i + 1) {
                    assert!(
                        !intersects(*a, *b),
                        "{width}: {a_name} {a:?} overlaps {b_name} {b:?}"
                    );
                }
            }
            // The title keeps a readable minimum whichever row layout applies.
            assert!(
                app.title_rect.width() >= 40.0,
                "{width}: title block {:?} collapsed",
                app.title_rect
            );
            // A wide window keeps the one-row bar; the wrapped bar is exactly
            // one extra tab row, so the body never loses more than that.
            if width >= 1280.0 {
                assert_eq!(app.appbar_h, APPBAR_H, "{width}: wide bar stays one row");
            } else {
                assert!(
                    app.appbar_h == APPBAR_H || app.appbar_h == APPBAR_H + APPBAR_TAB_ROW_H,
                    "{width}: unexpected app-bar height {}",
                    app.appbar_h
                );
            }
        }
    }

    fn appbar_controls_share_one_centre_line_per_row() {
        let mut app = app_at(1280.0, Screen::Changes);
        draw_frame(&mut app);
        let row_centre = APPBAR_H / 2.0;
        for (_, rect) in &app.tab_rects {
            assert!(
                (rect.center().y - row_centre).abs() < 0.01,
                "tab {rect:?} off-centre"
            );
            assert!((rect.height() - APPBAR_CONTROL_H).abs() < 0.01);
        }
        for rect in [app.settings_seg, app.settings_syn] {
            assert!(
                (rect.center().y - row_centre).abs() < 0.01,
                "{rect:?} off-centre"
            );
            assert!((rect.height() - APPBAR_CONTROL_H).abs() < 0.01);
        }
        // Equal gutters: the title starts as far from the left edge as the
        // settings cluster ends from the right edge.
        assert!((app.title_rect.x0 - APPBAR_GUTTER).abs() < 0.01);
        assert!((1280.0 - app.settings_syn.x1 - APPBAR_GUTTER).abs() < 0.01);
    }

    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    fn wrapped_appbar_moves_the_body_down_with_it() {
        let mut app = app_at(480.0, Screen::Changes);
        draw_frame(&mut app);
        assert_eq!(
            app.appbar_h,
            APPBAR_H + APPBAR_TAB_ROW_H,
            "480px cannot hold title, tabs and settings on one row"
        );
        // Tabs sit on the second row, left aligned on the title gutter.
        let first_tab = app.tab_rects[0].1;
        assert!(
            first_tab.y0 >= APPBAR_H - 0.01,
            "tabs wrap under the title: {first_tab:?}"
        );
        assert!((first_tab.x0 - (APPBAR_GUTTER + 6.0)).abs() < 0.01);
        // Nothing from the body (diff header row) is painted under the bar:
        // the hunk buttons the diff emits all start below the measured bar.
        for button in &app.diff_hunk_buttons {
            assert!(
                button.rect.y0 >= app.appbar_h,
                "hunk button {:?} paints under the app bar",
                button.rect
            );
        }
    }
}
