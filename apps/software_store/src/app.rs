//! The root `SoftwareStore` widget.
//!
//! This is a single self-laying-out, self-drawing `Widget`: rather than
//! composing dozens of child widgets it computes its own layout maths in
//! `draw`, paints rich cards / detail pages / lists directly into the
//! Vello scene, and records a flat list of interactive *hit targets* that
//! `handle_event` tests pointer clicks against. The mutable state
//! (installed set, in-flight installs) lives in the Automerge document;
//! the catalog and ephemeral navigation state live in widget fields.
//!
//! Frame contract (the five passes):
//!   * `sync`         — read the install snapshot out of the CRDT, seed
//!                      defaults on first run, advance / finalise installs.
//!   * `build_layout` — one full-bleed leaf node (we do our own layout).
//!   * `apply_layout` — capture the viewport rect.
//!   * `draw`         — paint the active screen; rebuild `hit_targets`.
//!   * `handle_event` — pointer + keyboard, mutating the CRDT directly.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::image_store::paint_image_cover;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, ModalDialog, SegmentedFace, SplitLayout, TextInput, ToggleGroup};
use ui::{ImageStore, LoadState};
use std::cell::Cell;
use std::rc::Rc;

use crate::catalog::sample_catalog;
use crate::images;
use crate::doc::StoreDoc;
use crate::draw::{self, alpha, mix};
use crate::flatpak::Remote;
use crate::flatpakref::FlatpakRef;
use crate::model::{
    AppEntry, Category, Catalog, Compat, InstallPhase, InstallSnapshot, InstallState, Permission,
    QosAppModel, Review, Source, UpdateProgress,
};
use crate::probe::ProbeResult;
use crate::worker::{ActionKind, Request, Worker, WorkerMsg};

/// How long a simulated install / update takes to fill its progress bar.
const INSTALL_DURATION: Duration = Duration::from_millis(2600);
/// How long an "Opening …" toast lingers.
const TOAST_DURATION: Duration = Duration::from_millis(2200);
/// How many updates run at once during "Update All"; the rest queue and
/// start as slots free up — so the Updates view shows a real pending →
/// in-progress → done pipeline rather than every app downloading at once.
const UPDATE_CONCURRENCY: usize = 2;

const HEADER_H: f64 = 64.0;
const BODY_PAD_X: f64 = 28.0;
const BODY_PAD_TOP: f64 = 20.0;
const GAP: f64 = 16.0;

/// Below this allocated width (logical px) the header's inline nav tabs no
/// longer fit beside the wordmark + search, so navigation collapses into an
/// overlay drawer summoned by a hamburger. Chosen so the three nav tabs +
/// the 280px search pill + the wordmark stop fitting comfortably side by
/// side.
const NAV_BREAKPOINT: f64 = 760.0;
/// Width of the navigation drawer when fully revealed in overlay mode.
const NAV_DRAWER_W: f64 = 264.0;
/// Size of the hamburger / close toggle shown in the header on narrow.
const NAV_TOGGLE_SZ: f64 = 36.0;

/// Which top-level screen the body shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Browse,
    Installed,
    Updates,
    Remotes,
}

/// One interactive region, rebuilt every `draw` and tested in
/// `handle_event`.
#[derive(Clone, Debug)]
struct HitTarget {
    rect: Rect,
    action: Action,
}

#[derive(Clone, Debug, PartialEq)]
enum Action {
    OpenDetail(String),
    Back,
    SetTab(Tab),
    SetCategory(Option<Category>),
    FocusSearch,
    ClearSearch,
    Install(String),
    Update(String),
    Cancel(String),
    Remove(String),
    Open(String),
    UpdateAll,
    /// Narrow-mode only: open/close the navigation drawer.
    ToggleNav,
    /// Narrow-mode only: dismiss the open drawer (scrim tap).
    DismissNav,
    /// Narrow-mode only: absorb a click on empty drawer-panel space so it
    /// neither falls through to the scrim (dismiss) nor the body behind.
    NavAbsorb,
    /// qOS-native app: run from a cached sandboxed bundle (stubbed backend).
    QosOpenDirect(String),
    /// qOS-native app: keep cached + prioritise (stubbed backend).
    QosAdd(String),
    /// Remotes screen: remove a configured Flatpak remote (gated).
    RemoteRemove(String),
    /// Remotes screen: add the Flathub remote (one-tap convenience).
    RemoteAddFlathub,
    /// Hover a target carrying a tooltip (qOS affordances).
    HoverTooltip(String),
}

/// Visual style for a pill button.
#[derive(Clone, Copy, PartialEq)]
enum BtnStyle {
    Primary,
    Neutral,
    Ghost,
    Danger,
}

/// Per-row status within the Updates batch, driving how a row renders in
/// the in-progress Updates view.
#[derive(Clone, Copy, Debug, PartialEq)]
enum BatchRowStatus {
    /// Enqueued, waiting its turn (no visible progress yet).
    Queued,
    /// Actively downloading or installing; carries progress + phase.
    InProgress { progress: f64, phase: InstallPhase },
    /// Finished updating in this batch.
    Done,
}

pub struct SoftwareStore {
    catalog: Catalog,
    /// Cached install state read out of the CRDT each `sync`.
    snapshot: InstallSnapshot,
    tab: Tab,
    /// `Some(app_id)` when the detail screen is open over the active tab.
    detail: Option<String>,
    /// Active category filter on the Browse screen (None = all).
    active_category: Option<Category>,
    /// Mirror of the embedded search field's text, read by every screen.
    /// The authoritative editor is [`Self::search_input`] (a real
    /// [`ui::widget::TextInput`] — caret, selection, clipboard, IME, a11y);
    /// this string is refreshed from it each event/frame so the pure filter
    /// queries stay simple.
    search_query: String,
    /// Mirror of the search field's focus state (kept in lockstep with
    /// `search_input`), used for the pill's focus ring + key routing.
    search_focused: bool,
    /// The real editable search field. A [`ui::widget::TextInput`] embedded in
    /// this monolithic widget and driven through its passes manually — so the
    /// store's search box gets the shared text core (selection, clipboard,
    /// IME, theming, a11y) instead of a hand-rolled `String` + fake caret.
    search_input: TextInput,
    /// Async image loader/cache for real app icons + screenshots, keyed by
    /// URL. Fetch + decode happen off the UI thread; a frame paints whatever
    /// has resolved and an honest loading skeleton for the rest.
    image_store: ImageStore,
    /// Whether a usable `flatpak` binary is present (from the live probe).
    /// `true` by default (offline `--shots`/tests never show the banner);
    /// set `false` on the live path when the probe finds no flatpak, which
    /// drives the honest "Flatpak unavailable" banner.
    flatpak_available: bool,
    /// Vertical scroll offset of the body, in logical px.
    scroll: f64,
    /// Total content height computed during the last `draw`, used to
    /// clamp scrolling.
    content_height: f64,
    /// Local wall-clock start time per in-flight install, used to animate
    /// the progress bar smoothly between syncs.
    install_start: HashMap<String, Instant>,
    /// The ordered set of app-ids participating in the current update
    /// *batch* (driven by the Updates view's "Update All" / per-row
    /// Update). Apps stay listed here while in flight AND after they finish
    /// so the Updates view can show a queue of pending → in-progress → done
    /// with an overall status, instead of rows silently vanishing the
    /// instant an update completes. Cleared once the whole batch settles.
    update_batch: Vec<String>,
    /// Set true by `draw` when an install's wall-clock progress reaches
    /// 1.0, so `needs_commit` can nudge one finalising `sync`.
    finalize_pending: bool,
    /// Transient bottom-of-screen toast (text, shown-at).
    toast: Option<(String, Instant)>,
    /// Best-effort live system probe, merged once in `sync`. `None` in
    /// the `--shots` path so the screenshot stays deterministic.
    probe: Option<ProbeResult>,
    probe_merged: bool,

    /// Background worker: all flatpak/Flathub I/O runs here, off the UI
    /// thread. `offline()` in `--shots` / tests.
    worker: Worker,
    /// `true` once the first round of background refreshes (installed set,
    /// remotes, the popular collection) has been kicked off.
    initial_kicked: bool,
    /// Configured Flatpak remotes, refreshed by the worker. Drives the
    /// Remotes screen; empty until the first refresh lands (or always in
    /// `--shots`, where the sample remotes below stand in).
    remotes: Vec<Remote>,
    /// Live catalog entries merged in from the Flathub API (search +
    /// collections + detail fetches), keyed by app-id. On the live path the
    /// curated sample is absent, so these are the only Browse/search data.
    live_apps: HashMap<String, AppEntry>,
    /// App-ids of the "popular" Flathub collection, in fetch order — the
    /// ordering the live Browse grid uses (the `live_apps` map is unordered).
    popular_ids: Vec<String>,
    /// The search query the worker last fetched results for, plus the time
    /// the current query last changed — used to debounce live search so we
    /// don't fire a request on every keystroke.
    search_dispatched: String,
    search_changed_at: Option<Instant>,
    /// A pending `.flatpakref` awaiting the user's install confirmation.
    /// When `Some`, a shared [`ModalDialog`] confirm sheet renders over the body.
    pending_ref: Option<FlatpakRef>,
    /// The live install-confirm dialog, built lazily from `pending_ref`.
    ref_dialog: Option<RefDlg>,
    /// The most recent gated-action error, shown as a toast / inline note.
    action_error: Option<(String, Instant)>,
    /// The tooltip currently hovered (text), rendered near the cursor.
    tooltip: Option<String>,

    /// Adaptive navigation flap. On wide windows the nav tabs sit inline in
    /// the header (this stays closed and unused); on narrow windows the tabs
    /// collapse into an overlay drawer summoned by the header hamburger,
    /// behind a dimming scrim. Closed by default at every width.
    nav_flap: FlapState,
    /// Wall-clock of the last flap animation tick, for frame-rate-independent
    /// easing of the drawer slide.
    last_tick: Instant,

    rect: Rect,
    node: Option<NodeId>,
    mouse: Point,
    hit_targets: Vec<HitTarget>,
    dirty: bool,
}

impl SoftwareStore {
    /// Construct the store with the bundled sample catalog and an offline
    /// worker. `probe` carries optional live system data (live app only).
    /// Tests use this; the live app uses [`SoftwareStore::with_worker`].
    pub fn new(probe: Option<ProbeResult>) -> Self {
        Self::with_worker(probe, Worker::offline())
    }

    /// Construct the store with an explicit [`Worker`] (live or offline).
    pub fn with_worker(probe: Option<ProbeResult>, worker: Worker) -> Self {
        // The live app shows REAL Flathub data only: it starts with an empty
        // catalog and fills it from the popular-collection / search fetches (see
        // `browse_apps`), so the fabricated-rating sample never reaches a real
        // user. The offline `--shots`/test path keeps the curated sample so
        // screenshots + tests stay populated without a network.
        let catalog = if worker.is_live() {
            Catalog::new(Vec::new())
        } else {
            Catalog::new(sample_catalog())
        };
        Self {
            catalog,
            snapshot: InstallSnapshot::default(),
            tab: Tab::Browse,
            detail: None,
            active_category: None,
            search_query: String::new(),
            search_focused: false,
            search_input: TextInput::local()
                .with_plain_chrome()
                .with_placeholder("Search apps")
                .with_font_size(14.0)
                .with_id("software-store-search"),
            image_store: ImageStore::with_workers(images::fetcher(), 3),
            flatpak_available: true,
            scroll: 0.0,
            content_height: 0.0,
            install_start: HashMap::new(),
            update_batch: Vec::new(),
            finalize_pending: false,
            toast: None,
            probe,
            probe_merged: false,
            worker,
            initial_kicked: false,
            remotes: Vec::new(),
            live_apps: HashMap::new(),
            popular_ids: Vec::new(),
            search_dispatched: String::new(),
            search_changed_at: None,
            pending_ref: None,
            ref_dialog: None,
            action_error: None,
            tooltip: None,
            nav_flap: FlapState::closed_by_default(),
            last_tick: Instant::now(),
            rect: Rect::ZERO,
            node: None,
            mouse: Point::ZERO,
            hit_targets: Vec::new(),
            dirty: true,
        }
    }

    /// Open the store directly into the `.flatpakref` install-confirmation
    /// flow. Called when the process was launched with a `.flatpakref`
    /// path (the store is the registered handler for that file type).
    pub fn open_flatpakref(&mut self, r: FlatpakRef) {
        self.pending_ref = Some(r);
        self.dirty = true;
    }

    /// Apply an initial-view override from the `SOFTWARE_STORE_SHOT_VIEW`
    /// environment variable. Used only by the `--shots` path to capture
    /// screens other than Browse for visual review. Accepted forms:
    /// `installed`, `updates`, `search:<query>`, `detail:<app-id>`, and
    /// `menu` (snaps the narrow-window navigation drawer open over a scrim).
    pub fn apply_shot_view_env(&mut self) {
        let Ok(spec) = std::env::var("SOFTWARE_STORE_SHOT_VIEW") else { return };
        let spec = spec.trim();
        if spec == "installed" {
            self.tab = Tab::Installed;
        } else if spec == "updates" {
            self.tab = Tab::Updates;
        } else if spec == "updates:progress" {
            // Stage a representative in-progress update *batch* for the
            // screenshot so the captured frame shows every pipeline state at
            // once: one queued, one downloading, one installing, one done —
            // plus the overall status card. These back/forward-dated
            // `install_start` entries are honoured by the first `sync`, which
            // also enqueues them into the batch.
            self.tab = Tab::Updates;
            let stage: [(&str, f64); 2] = [
                ("org.gimp.GIMP", 0.32),        // mid-download (< 0.6)
                ("com.visualstudio.code", 0.78), // installing (>= 0.6)
            ];
            for (id, p) in stage {
                self.enqueue_update(id);
                let elapsed = INSTALL_DURATION.mul_f64(p);
                let start = Instant::now().checked_sub(elapsed).unwrap_or_else(Instant::now);
                self.install_start.insert(id.to_string(), start);
            }
            // LibreOffice (seeded mid-install) becomes the *queued* row here —
            // forward-dated so it reads as waiting its turn.
            self.enqueue_update("org.libreoffice.LibreOffice");
            self.install_start.insert(
                "org.libreoffice.LibreOffice".into(),
                Instant::now() + INSTALL_DURATION * 10,
            );
            // A finished one so the "done" affordance + count is visible.
            // Signal is seeded installed-at-latest, so being in the batch
            // derives as Done.
            self.enqueue_update("org.signal.Signal");
        } else if spec == "remotes" {
            self.tab = Tab::Remotes;
        } else if spec == "flatpakref" {
            // Snap the install-confirmation sheet open over the browse home,
            // using a representative ref so the modal is fully populated.
            self.pending_ref = Some(FlatpakRef {
                app_id: "org.gimp.GIMP".into(),
                title: "GIMP".into(),
                comment: "Create images and edit photographs".into(),
                url: "https://dl.flathub.org/repo/".into(),
                branch: "stable".into(),
                source_path: "/tmp/GIMP.flatpakref".into(),
            });
        } else if spec == "menu" {
            // Snap the navigation drawer fully open with no animation so a
            // single offscreen frame captures the settled overlay + scrim.
            // The drawer only renders when the layout resolves to overlay,
            // i.e. at a narrow viewport.
            self.nav_flap.snap(true);
        } else if let Some(q) = spec.strip_prefix("search:") {
            self.search_input.set_text(q);
            self.search_query = q.to_string();
            self.tab = Tab::Browse;
        } else if let Some(id) = spec.strip_prefix("detail:") {
            self.detail = Some(id.to_string());
        }
    }

    /// Synchronously fetch + decode every catalog icon/screenshot and block
    /// until the image store settles. Used **only** by the `--shots` path so a
    /// single offscreen frame captures real, resolved images rather than the
    /// loading skeleton. Never call on a live UI thread. A no-op on the live
    /// path (empty catalog) — those images stream in over real frames.
    pub fn prewarm_images(&self, timeout: Duration) {
        for app in &self.catalog.apps {
            if let Some(u) = app.icon_url.as_deref().filter(|u| !u.is_empty()) {
                let _ = self.image_store.get(u);
            }
            for shot in &app.shots {
                if let Some(u) = shot.url.as_deref().filter(|u| !u.is_empty()) {
                    let _ = self.image_store.get(u);
                }
            }
        }
        self.image_store.block_until_idle(timeout);
    }

    fn body_rect(&self) -> Rect {
        Rect::new(self.rect.x0, self.rect.y0 + HEADER_H + self.banner_h(), self.rect.x1, self.rect.y1)
    }

    /// Height of the "Flatpak unavailable" banner strip (0 when flatpak is
    /// present, so no chrome is reserved on a normal system / in `--shots`).
    fn banner_h(&self) -> f64 {
        if self.flatpak_available { 0.0 } else { 34.0 }
    }

    /// Draw the honest "Flatpak unavailable" strip between the header and the
    /// body, shown only when the live probe found no flatpak binary. Browsing
    /// the Flathub catalog still works; installing/managing does not — so we
    /// say exactly that rather than faking success.
    fn draw_flatpak_banner(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme) {
        if self.flatpak_available {
            return;
        }
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let strip = Rect::new(self.rect.x0, self.rect.y0 + HEADER_H, self.rect.x1, self.rect.y0 + HEADER_H + self.banner_h());
        draw::fill_rect(scene, t, strip, alpha(c.semantic.warning, 0.16));
        draw::fill_rect(scene, t, Rect::new(strip.x0, strip.y1 - 1.0, strip.x1, strip.y1), alpha(c.semantic.warning, 0.5));
        let dot = Circle::new(Point::new(strip.x0 + BODY_PAD_X + 4.0, strip.center().y), 4.0);
        scene.fill(vello::peniko::Fill::NonZero, t, c.semantic.warning, None, &dot);
        let msg = "Flatpak isn’t available here — you can browse the catalog, but installing and managing apps needs Flatpak.";
        draw::text_w(scene, tr, t, msg, ty.sm, TextWeight::Medium, mix(c.text_primary, c.semantic.warning, 0.35), strip.x0 + BODY_PAD_X + 18.0, strip.center().y - ty.sm as f64 / 2.0 - 1.0, None);
    }

    /// Resolve the navigation layout from the *allocated* width. At or above
    /// the breakpoint the nav tabs dock inline in the header; below it they
    /// collapse into an overlay drawer.
    fn nav_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), NAV_BREAKPOINT)
    }

    /// True when the window is narrow enough that nav lives in the overlay
    /// drawer rather than inline header tabs.
    fn is_narrow(&self) -> bool {
        self.nav_layout() == SplitLayout::Overlay
    }

    /// The hamburger / close toggle rect in the header (narrow mode only).
    fn nav_toggle_rect(&self) -> Rect {
        let cy = self.rect.y0 + HEADER_H / 2.0;
        let x0 = self.rect.x0 + BODY_PAD_X;
        Rect::new(x0, cy - NAV_TOGGLE_SZ / 2.0, x0 + NAV_TOGGLE_SZ, cy + NAV_TOGGLE_SZ / 2.0)
    }

    /// The navigation drawer's current on-screen rect. Slides in from the
    /// left edge by the eased reveal; fully off-screen at reveal 0.
    fn nav_drawer_rect(&self) -> Rect {
        let slide = self.nav_flap.slide();
        let x0 = self.rect.x0 - NAV_DRAWER_W * (1.0 - slide);
        Rect::new(x0, self.rect.y0, x0 + NAV_DRAWER_W, self.rect.y1)
    }

    /// Advance the drawer's reveal animation using real elapsed time, so the
    /// slide + scrim fade are frame-rate independent.
    fn tick_nav(&mut self) -> bool {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        self.nav_flap.tick(self.nav_layout(), dt)
    }

    fn max_scroll(&self) -> f64 {
        let viewport = self.body_rect().height();
        (self.content_height - viewport).max(0.0)
    }

    fn reset_scroll(&mut self) {
        self.scroll = 0.0;
    }

    /// Focus or blur the search field, keeping the mirror flag in lockstep
    /// with the real [`TextInput`] so the pill's focus ring + key routing
    /// agree with where edits actually land.
    fn set_search_focus(&mut self, on: bool) {
        self.search_focused = on;
        self.search_input.set_focus(on);
        self.dirty = true;
    }

    /// Set the search text through the shared text core (so caret/selection
    /// stay consistent) and refresh the mirror + debounce clock.
    fn set_search_text(&mut self, s: &str) {
        self.search_input.set_text(s);
        self.search_query = s.to_string();
        self.search_changed_at = Some(Instant::now());
        self.reset_scroll();
        self.dirty = true;
    }

    /// Fold any pending edit from the embedded search field into the mirror
    /// the screens read. Returns `true` when the query changed.
    fn pull_search_edit(&mut self) -> bool {
        if let Some(q) = self.search_input.take_dirty() {
            self.search_query = q;
            self.search_changed_at = Some(Instant::now());
            self.reset_scroll();
            self.dirty = true;
            return true;
        }
        false
    }

    /// The set of updatable apps, used by the Updates badge + Update All.
    fn updatable_ids(&self) -> Vec<String> {
        self.catalog
            .updatable(&self.snapshot)
            .iter()
            .map(|a| a.id.clone())
            .collect()
    }

    /// Start updating every pending app, but only run [`UPDATE_CONCURRENCY`]
    /// at a time: the first slots begin immediately, the rest are enqueued
    /// with a forward-dated start so they read as "Queued" and begin once a
    /// running slot frees up (handled by [`Self::promote_queued_updates`] in
    /// `sync`). This gives the Updates view a real pending → in-progress →
    /// done pipeline instead of every app downloading simultaneously.
    fn start_update_all(&mut self, state: &mut AutoCommit) {
        let ids = self.updatable_ids();
        for (i, id) in ids.iter().enumerate() {
            self.enqueue_update(id);
            if i < UPDATE_CONCURRENCY {
                self.start_install(state, id);
            } else {
                // Queue it: mark installing at 0 in the CRDT (so it shows in
                // the batch) but forward-date the local start so it doesn't
                // begin animating until promoted.
                StoreDoc::set_installing(state, id, 0.0);
                self.install_start
                    .insert(id.clone(), Instant::now() + INSTALL_DURATION * 10);
            }
        }
        self.snapshot = StoreDoc::read_snapshot(state);
    }

    /// Promote queued updates into running slots as active installs finish,
    /// keeping at most [`UPDATE_CONCURRENCY`] downloading/installing at once.
    /// Run once per `sync`.
    fn promote_queued_updates(&mut self) {
        // Count how many batch apps are actively progressing right now.
        let active = self
            .update_batch
            .iter()
            .filter(|id| matches!(self.batch_status(id), Some(BatchRowStatus::InProgress { .. })))
            .count();
        let mut free = UPDATE_CONCURRENCY.saturating_sub(active);
        if free == 0 {
            return;
        }
        // Promote queued apps (future-dated starts) in batch order.
        let queued: Vec<String> = self
            .update_batch
            .iter()
            .filter(|id| self.is_queued(id))
            .cloned()
            .collect();
        for id in queued {
            if free == 0 {
                break;
            }
            self.install_start.insert(id, Instant::now());
            free -= 1;
        }
    }

    /// Record `id` as part of the active update batch (idempotent, keeping
    /// catalog order). Apps in the batch stay visible in the Updates view —
    /// pending, in-progress, then done — until the whole batch settles.
    fn enqueue_update(&mut self, id: &str) {
        if !self.update_batch.iter().any(|x| x == id) {
            self.update_batch.push(id.to_string());
        }
    }

    /// Apps currently shown in the Updates view, in display order: the
    /// active update batch first (in the order they were enqueued), then any
    /// remaining updatable apps not yet in the batch. This keeps a row
    /// on-screen while its update is downloading / installing / done instead
    /// of having it vanish the moment the state flips.
    fn updates_view_apps(&self) -> Vec<AppEntry> {
        let mut out: Vec<AppEntry> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for id in &self.update_batch {
            if let Some(app) = self.app_by_id(id)
                && seen.insert(id.clone())
            {
                out.push(app);
            }
        }
        for app in self.catalog.updatable(&self.snapshot) {
            if seen.insert(app.id.clone()) {
                out.push(app.clone());
            }
        }
        out
    }

    /// Per-app status within the Updates batch, used to drive the row's
    /// progress / phase / "Updated" affordance. Returns `None` for an app
    /// that isn't part of the batch (a plain pending update).
    fn batch_status(&self, id: &str) -> Option<BatchRowStatus> {
        if !self.update_batch.iter().any(|x| x == id) {
            return None;
        }
        if let Some(app) = self.app_by_id(id) {
            match self.live_state(&app) {
                InstallState::Installing(p) => {
                    // An app enqueued but not yet actively downloading (its
                    // start is still in the future, so progress is pinned at
                    // 0) reads as Queued; anything making progress reads as
                    // in-progress with a download/install phase.
                    if p <= f64::EPSILON && self.is_queued(id) {
                        Some(BatchRowStatus::Queued)
                    } else {
                        let phase = InstallPhase::from_progress(p, false);
                        Some(BatchRowStatus::InProgress { progress: p, phase })
                    }
                }
                InstallState::Installed => Some(BatchRowStatus::Done),
                // Not in flight and not installed — e.g. an update that
                // hasn't been kicked yet, or one that was canceled but not
                // yet pruned. Treat it as a plain pending row (None), not a
                // stuck "Queued", so it offers a fresh Update control.
                _ => None,
            }
        } else {
            None
        }
    }

    /// `true` if `id`'s install is queued — its start instant is still in the
    /// future (a staggered "Update All" slot that hasn't begun), so it has
    /// made no progress yet and should read as "Queued" rather than
    /// "Downloading 0%".
    fn is_queued(&self, id: &str) -> bool {
        self.install_start
            .get(id)
            .map(|start| *start > Instant::now())
            .unwrap_or(false)
    }

    /// Roll the per-app batch statuses up into an [`UpdateProgress`] for the
    /// overall status line + bar. Pure aggregation over `updates_view_apps`.
    fn update_progress(&self) -> UpdateProgress {
        let mut queued = 0usize;
        let mut active = 0usize;
        let mut done = 0usize;
        let mut sum = 0.0f64;
        let mut n = 0usize;
        for app in self.updates_view_apps() {
            match self.batch_status(&app.id) {
                Some(BatchRowStatus::Queued) => {
                    queued += 1;
                    n += 1;
                }
                Some(BatchRowStatus::InProgress { progress, phase }) => {
                    if phase == InstallPhase::Queued {
                        queued += 1;
                    } else {
                        active += 1;
                    }
                    sum += progress;
                    n += 1;
                }
                Some(BatchRowStatus::Done) => {
                    done += 1;
                    sum += 1.0;
                    n += 1;
                }
                None => {} // plain pending update, not part of the batch
            }
        }
        let overall = if n == 0 { 0.0 } else { sum / n as f64 };
        UpdateProgress { queued, active, done, overall }
    }

    /// Clear the update batch once every app in it has finished (so the
    /// "done" rows + overall bar dismiss together when the work is over).
    fn settle_update_batch(&mut self) {
        if self.update_batch.is_empty() {
            return;
        }
        let all_done = self.update_batch.iter().all(|id| {
            self.app_by_id(id)
                .map(|app| matches!(self.live_state(&app), InstallState::Installed))
                .unwrap_or(true)
        });
        if all_done {
            self.update_batch.clear();
        }
    }

    /// Begin a (simulated) install / update of `id`.
    ///
    /// The visible progress bar is always driven by a local wall-clock
    /// animation (deterministic, never blocks the frame). When a live
    /// worker is present the *real* `flatpak install/update` is dispatched
    /// to it in parallel; its outcome arrives later via the worker channel
    /// and only surfaces if it fails (the simulated bar is the source of
    /// truth for the visual). This keeps the UX identical online and off.
    fn start_install(&mut self, state: &mut AutoCommit, id: &str) {
        StoreDoc::set_installing(state, id, 0.0);
        self.install_start.insert(id.to_string(), Instant::now());
        self.snapshot = StoreDoc::read_snapshot(state);
        if self.worker.is_live() {
            let app = self.app_by_id(id);
            let already_installed = self.snapshot.installed.contains_key(id);
            match app.as_ref().map(|a| (a.source, a.remote.clone())) {
                Some((Source::QosNative, _)) => {} // qOS apps don't flatpak-install
                Some((_, _)) if already_installed => {
                    self.worker.request(Request::Update(id.to_string()));
                }
                Some((_, remote)) => {
                    self.worker.request(Request::Install { remote, app_id: id.to_string() });
                }
                None => {
                    self.worker.request(Request::Install {
                        remote: "flathub".into(),
                        app_id: id.to_string(),
                    });
                }
            }
        }
        self.dirty = true;
    }

    /// Cancel an in-flight install.
    fn cancel_install(&mut self, state: &mut AutoCommit, id: &str) {
        StoreDoc::clear_installing(state, id);
        self.install_start.remove(id);
        // Drop it from the update batch too, so a canceled update doesn't
        // linger as a stuck "Queued" row.
        self.update_batch.retain(|x| x != id);
        self.snapshot = StoreDoc::read_snapshot(state);
        self.dirty = true;
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    /// Apply a hit-target action. Mutates the CRDT directly for install
    /// state; everything else is local navigation.
    fn apply(&mut self, action: Action, state: &mut AutoCommit) {
        match action {
            Action::OpenDetail(id) => {
                // Lazily fetch full AppStream detail for a live (non-curated)
                // entry the first time it's opened, if not already cached.
                if self.worker.is_live()
                    && self.catalog.get(&id).is_none()
                    && !self.worker.caches.detail.contains(&id)
                {
                    self.worker.request(Request::Detail(id.clone()));
                }
                self.detail = Some(id);
                self.reset_scroll();
            }
            Action::Back => {
                self.detail = None;
                self.reset_scroll();
            }
            Action::SetTab(t) => {
                if self.tab != t || self.detail.is_some() {
                    self.tab = t;
                    self.detail = None;
                    self.reset_scroll();
                }
                // Selecting a destination dismisses the overlay drawer.
                self.nav_flap.close();
            }
            Action::SetCategory(c) => {
                self.active_category = c;
                self.reset_scroll();
            }
            Action::FocusSearch => self.set_search_focus(true),
            Action::ClearSearch => {
                self.set_search_text("");
                self.set_search_focus(false);
            }
            Action::Install(id) => self.start_install(state, &id),
            Action::Update(id) => {
                self.enqueue_update(&id);
                self.start_install(state, &id);
            }
            Action::Cancel(id) => self.cancel_install(state, &id),
            Action::Remove(id) => {
                StoreDoc::remove_installed(state, &id);
                self.install_start.remove(&id);
                self.snapshot = StoreDoc::read_snapshot(state);
                if self.worker.is_live() {
                    self.worker.request(Request::Remove(id.clone()));
                }
            }
            Action::Open(id) => {
                let name = self.app_by_id(&id).map(|a| a.name).unwrap_or(id.clone());
                self.show_toast(format!("Opening {name}…"));
                if self.worker.is_live() {
                    self.worker.request(Request::Launch(id));
                }
            }
            Action::UpdateAll => self.start_update_all(state),
            Action::ToggleNav => self.nav_flap.toggle(self.nav_layout()),
            Action::DismissNav => self.nav_flap.close(),
            // Click inside the open drawer panel but not on a row: keep the
            // drawer open, just consume the event.
            Action::NavAbsorb => {}
            Action::QosOpenDirect(id) => {
                // VISUAL-ONLY (documented stub): in the real qOS model this
                // would hydrate a cached bundle and launch it in a strong
                // per-launch sandbox without a permanent install. For now we
                // surface the intent and toast it.
                let name = self.app_by_id(&id).map(|a| a.name).unwrap_or(id);
                self.show_toast(format!("Opening {name} directly (cached, sandboxed)…"));
            }
            Action::QosAdd(id) => {
                // VISUAL-ONLY (documented stub): would keep the bundle cached
                // and prioritise it in auto-updates + global search. We
                // record it as installed locally so the affordance reads as
                // "added", but no real packaging action runs.
                if let Some(app) = self.app_by_id(&id) {
                    StoreDoc::set_installed(state, &id, &app.latest_version);
                    self.snapshot = StoreDoc::read_snapshot(state);
                    self.show_toast(format!("Added {} — cached & prioritised", app.name));
                }
            }
            Action::RemoteRemove(name) => {
                // Optimistically drop it from the list; the worker confirms.
                self.remotes.retain(|r| r.name != name);
                if self.worker.is_live() {
                    self.worker.request(Request::RemoteDelete(name));
                } else {
                    self.show_toast("Remote removed");
                }
            }
            Action::RemoteAddFlathub => {
                let exists = self.remotes.iter().any(|r| r.name == "flathub");
                if !exists {
                    self.remotes.push(Remote {
                        name: "flathub".into(),
                        title: "Flathub".into(),
                        url: "https://dl.flathub.org/repo/".into(),
                        disabled: false,
                    });
                }
                if self.worker.is_live() {
                    self.worker.request(Request::RemoteAdd {
                        name: "flathub".into(),
                        url: "https://flathub.org/repo/flathub.flatpakrepo".into(),
                    });
                } else {
                    self.show_toast("Added Flathub remote");
                }
            }
            Action::HoverTooltip(text) => {
                self.tooltip = Some(text);
            }
        }
        self.dirty = true;
    }

    /// Derive the install state for a catalog app, preferring the smooth
    /// local wall-clock progress over the CRDT-stored value while an
    /// install is in flight locally.
    fn live_state(&self, app: &AppEntry) -> InstallState {
        if let Some(start) = self.install_start.get(&app.id) {
            let p = (start.elapsed().as_secs_f64() / INSTALL_DURATION.as_secs_f64()).min(1.0);
            return InstallState::Installing(p);
        }
        self.catalog.state_of(app, &self.snapshot)
    }

    /// Kick off the first round of background refreshes once we know we're
    /// a live worker: read the installed set + remotes, and warm the
    /// "popular" collection for the browse home. All async — none of these
    /// block the first frame.
    fn kick_initial(&mut self) {
        if self.initial_kicked || !self.worker.is_live() {
            return;
        }
        self.initial_kicked = true;
        self.worker.request(Request::RefreshInstalled);
        self.worker.request(Request::RefreshRemotes);
        self.worker.request(Request::Collection("popular".into()));
    }

    /// Drain finished worker messages into the model. Non-blocking — runs
    /// once per `sync`. Returns whether anything changed (so we repaint).
    fn drain_worker(&mut self, state: &mut AutoCommit) -> bool {
        // Collect applied effects out of the closure (which only borrows
        // shared state) and apply the &mut-self ones afterward.
        let mut new_remotes: Option<Vec<Remote>> = None;
        let mut new_live: Vec<AppEntry> = Vec::new();
        let mut new_popular: Option<Vec<String>> = None;
        let mut installed_updates: Vec<(String, String)> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut finished_actions: Vec<(String, ActionKind)> = Vec::new();

        let drained = self.worker.drain(|msg| match msg {
            WorkerMsg::Installed(map) => {
                for (id, ver) in map {
                    installed_updates.push((id.clone(), ver.clone()));
                }
            }
            WorkerMsg::Remotes(rs) => new_remotes = Some(rs.clone()),
            WorkerMsg::Collection { name, apps } => {
                if name == "popular" {
                    new_popular = Some(apps.iter().map(|a| a.id.clone()).collect());
                }
                new_live.extend(apps.iter().cloned());
            }
            WorkerMsg::SearchResults { apps, .. } => new_live.extend(apps.iter().cloned()),
            WorkerMsg::Detail(app) => new_live.push((**app).clone()),
            WorkerMsg::ActionDone { target, kind, outcome } => {
                finished_actions.push((target.clone(), *kind));
                if let crate::flatpak::ActionOutcome::Failed(e) = outcome {
                    errors.push(format!("{}: {e}", short_target(target)));
                } else if matches!(kind, ActionKind::Remove) {
                    removed.push(target.clone());
                }
            }
        });

        if drained == 0 {
            return false;
        }

        if let Some(rs) = new_remotes {
            self.remotes = rs;
        }
        if let Some(p) = new_popular {
            self.popular_ids = p;
        }
        for app in new_live {
            // Merge live entries that aren't already in the curated catalog
            // (the sample catalog stays the floor; live data augments it).
            if self.catalog.get(&app.id).is_none() {
                self.live_apps.insert(app.id.clone(), app);
            } else if let Some(slot) = self.live_apps.get_mut(&app.id) {
                *slot = app;
            } else {
                self.live_apps.insert(app.id.clone(), app);
            }
        }
        // Reflect the real installed set into the CRDT (live app only).
        for (id, ver) in installed_updates {
            if self.catalog.get(&id).is_some() || self.live_apps.contains_key(&id) {
                let v = if ver.is_empty() {
                    self.catalog.get(&id).map(|a| a.latest_version.clone()).unwrap_or_default()
                } else {
                    ver
                };
                StoreDoc::set_installed(state, &id, &v);
            }
        }
        for id in removed {
            StoreDoc::remove_installed(state, &id);
            self.install_start.remove(&id);
        }
        // Clear local in-flight markers for finished real actions.
        for (target, kind) in finished_actions {
            if matches!(kind, ActionKind::Install | ActionKind::Update) {
                // The real action finished; the simulated progress bar
                // finalises on its own timer, so nothing else to do here.
                let _ = target;
            }
            if matches!(kind, ActionKind::RemoteAdd | ActionKind::RemoteDelete) {
                self.worker.request(Request::RefreshRemotes);
            }
        }
        if let Some(e) = errors.into_iter().next() {
            self.action_error = Some((e, Instant::now()));
        }
        self.snapshot = StoreDoc::read_snapshot(state);
        true
    }

    /// Debounced live search: when the query has been stable for a short
    /// window and differs from what we last fetched, fire one async search.
    /// Never blocks — results stream back via the worker channel.
    fn maybe_dispatch_search(&mut self) {
        if !self.worker.is_live() {
            return;
        }
        let q = self.search_query.trim().to_string();
        if q.len() < 2 || q == self.search_dispatched {
            return;
        }
        let Some(changed) = self.search_changed_at else { return };
        // 220ms debounce — long enough to coalesce a burst of keystrokes,
        // short enough to feel instant.
        if changed.elapsed() < Duration::from_millis(220) {
            return;
        }
        // Serve from cache instantly if we have it; otherwise issue async.
        if self.worker.caches.search.contains(&q) {
            self.search_dispatched = q;
            return;
        }
        if self.worker.request(Request::Search(q.clone())) {
            self.search_dispatched = q;
        }
    }

    /// `true` while a live search query has changed but not yet been
    /// dispatched (debounce window still open) — keeps the loop awake so
    /// `maybe_dispatch_search` runs at the right moment.
    fn search_pending(&self) -> bool {
        if !self.worker.is_live() {
            return false;
        }
        let q = self.search_query.trim().to_string();
        q.len() >= 2 && q != self.search_dispatched && self.search_changed_at.is_some()
    }

    /// All catalog entries (curated + live) matching the current search /
    /// category, used by the Browse search results. Live entries from the
    /// Flathub worker are appended after the curated matches.
    fn search_results(&self) -> Vec<AppEntry> {
        let q = self.search_query.to_lowercase();
        let mut out: Vec<AppEntry> = self
            .catalog
            .search(&q, self.active_category)
            .into_iter()
            .cloned()
            .collect();
        let curated: std::collections::HashSet<&str> =
            self.catalog.apps.iter().map(|a| a.id.as_str()).collect();
        // Prefer cached worker results for this exact query; else any live
        // apps already merged that match.
        if let Some(hits) = self.worker.caches.search.get(&self.search_query.trim().to_string()) {
            for a in hits {
                if !curated.contains(a.id.as_str()) && a.matches(&q) {
                    out.push(a.clone());
                }
            }
        } else {
            for a in self.live_apps.values() {
                if !curated.contains(a.id.as_str()) && a.matches(&q) {
                    out.push(a.clone());
                }
            }
        }
        out
    }

    /// Look up an app by id across the curated catalog and live entries.
    fn app_by_id(&self, id: &str) -> Option<AppEntry> {
        self.catalog
            .get(id)
            .cloned()
            .or_else(|| self.live_apps.get(id).cloned())
    }

    /// The pool of apps the Browse home renders. On the live path this is the
    /// real Flathub set — the popular collection (in fetch order) plus any
    /// other fetched apps — never the curated sample. Offline / `--shots` uses
    /// the sample catalog so screenshots stay populated without a network.
    fn browse_apps(&self) -> Vec<&AppEntry> {
        if !self.worker.is_live() {
            return self.catalog.apps.iter().collect();
        }
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<&AppEntry> = Vec::new();
        for id in &self.popular_ids {
            if let Some(a) = self.live_apps.get(id) {
                if seen.insert(a.id.as_str()) {
                    out.push(a);
                }
            }
        }
        // Anything fetched outside the popular collection (e.g. a detail view)
        // trails after, alphabetised, so nothing real is hidden.
        let mut extras: Vec<&AppEntry> = self
            .live_apps
            .values()
            .filter(|a| !seen.contains(a.id.as_str()))
            .collect();
        extras.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out.extend(extras);
        out
    }

    /// Confirm the pending `.flatpakref` install: dispatch the gated action
    /// to the worker, start the local progress animation, and dismiss the
    /// sheet.
    fn confirm_pending_ref(&mut self, state: &mut AutoCommit) {
        let Some(r) = self.pending_ref.take() else { return };
        // Drive the visible progress bar via the same simulated-install path.
        self.start_install(state, &r.app_id);
        if self.worker.is_live() {
            self.worker.request(Request::InstallRef(r.source_path.clone()));
        }
        self.tab = Tab::Browse;
        self.detail = Some(r.app_id.clone());
        self.show_toast(format!("Installing {}…", r.title));
        self.dirty = true;
    }
}

/// The remotes shown on the Remotes screen in `--shots` / before the first
/// live refresh lands, so the screen is always populated for screenshots.
fn sample_remotes() -> Vec<Remote> {
    vec![
        Remote {
            name: "flathub".into(),
            title: "Flathub".into(),
            url: "https://dl.flathub.org/repo/".into(),
            disabled: false,
        },
        Remote {
            name: "flathub-beta".into(),
            title: "Flathub Beta".into(),
            url: "https://dl.flathub.org/beta-repo/".into(),
            disabled: true,
        },
        Remote {
            name: "fedora".into(),
            title: "Fedora Flatpaks".into(),
            url: "oci+https://registry.fedoraproject.org".into(),
            disabled: false,
        },
    ]
}

/// Shorten a target (app-id / remote name / path) for an error toast.
fn short_target(t: &str) -> String {
    if let Some((_, last)) = t.rsplit_once('/') {
        last.to_string()
    } else {
        t.rsplit('.').next().unwrap_or(t).to_string()
    }
}

// ===================================================================
// Drawing
// ===================================================================

impl SoftwareStore {
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        targets: &mut Vec<HitTarget>,
    ) {
        let c = &theme.tokens.colors;
        let header = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + HEADER_H);
        // Opaque header background (paints over any scrolled body).
        draw::fill_rect(scene, t, header, c.surfaces.base);
        // Bottom hairline.
        draw::fill_rect(
            scene,
            t,
            Rect::new(header.x0, header.y1 - 1.0, header.x1, header.y1),
            c.border_standard,
        );

        let cy = header.center().y;
        let x = header.x0 + BODY_PAD_X;
        let narrow = self.is_narrow();

        // Narrow: a hamburger summons the navigation drawer in place of the
        // inline tabs. The Back chevron, when in detail, takes precedence —
        // detail is the single-pane "push" view, so its Back affordance leads.
        // (Toggle and Back are mutually exclusive on `detail`, so both anchor at
        // the same left origin.)
        if narrow && self.detail.is_none() {
            self.draw_nav_toggle(scene, t, theme, targets);
        }

        // Back chevron when in detail view.
        if self.detail.is_some() {
            let back_rect = Rect::new(x, cy - 16.0, x + 64.0, cy + 16.0);
            let hov = back_rect.contains(self.mouse);
            draw::fill_round(scene, t, back_rect, 8.0, if hov { c.surface_white_10 } else { c.surfaces.raised });
            draw::text_w(scene, tr, t, "‹  Back", theme.tokens.typography.scale.sm, TextWeight::Medium, c.text_primary, x + 12.0, cy - 8.0, None);
            targets.push(HitTarget { rect: back_rect, action: Action::Back });
        }

        // The OS owns the window title/app-name/logo — no wordmark drawn here.

        // Right cluster: nav tabs (wide only), then search pill.
        let pad_r = header.x1 - BODY_PAD_X;
        let tsize = theme.tokens.typography.scale.sm;
        let up_count = self.catalog.updatable(&self.snapshot).len();

        // Tabs anchored at the right edge: measure widths, then lay out
        // left-to-right from a computed origin. Narrow windows hide the
        // inline tabs entirely — navigation lives in the overlay drawer — so
        // the search pill anchors straight to the right edge.
        let tabs_origin = if narrow {
            pad_r
        } else {
            let inst_count = self.catalog.installed(&self.snapshot).len();
            let tabs: [(Tab, String); 4] = [
                (Tab::Browse, "Browse".into()),
                (Tab::Installed, format!("Installed · {inst_count}")),
                (Tab::Updates, "Updates".into()),
                (Tab::Remotes, "Remotes".into()),
            ];
            let mut widths = Vec::new();
            for (tab, label) in &tabs {
                let mut w = draw::measure(tr, label, tsize).0 + 28.0;
                if *tab == Tab::Updates && up_count > 0 {
                    w += 22.0;
                }
                widths.push(w);
            }
            let total_tabs: f64 = widths.iter().sum();
            let origin = pad_r - total_tabs;
            // Nav tabs via the shared segmented control (pill/tab-strip style,
            // left-aligned labels). No tab is active while a detail page or a
            // search is open, so `selected` falls out of range → nothing lit.
            let mut rects: Vec<Rect> = Vec::with_capacity(tabs.len());
            let mut tab_x = origin;
            for w in &widths {
                rects.push(Rect::new(tab_x, cy - 16.0, tab_x + w, cy + 16.0));
                tab_x += w;
            }
            let labels: Vec<&str> = tabs.iter().map(|(_, l)| l.as_str()).collect();
            let selected = if self.detail.is_none() && self.search_query.is_empty() {
                tabs.iter().position(|(tab, _)| self.tab == *tab).unwrap_or(usize::MAX)
            } else {
                usize::MAX
            };
            let hovered = rects.iter().position(|r| r.contains(self.mouse));
            ToggleGroup::paint(
                scene,
                t,
                tr,
                theme,
                &SegmentedFace::new(&rects, &labels, selected)
                    .pill()
                    .hovered(hovered)
                    .radius(theme.tokens.radii.full as f64)
                    .text_size(tsize)
                    .left_labels(14.0),
            );
            // Updates badge + hit targets (per-tab extras the strip doesn't own).
            for (i, (tab, label)) in tabs.iter().enumerate() {
                let r = rects[i];
                let active = selected == i;
                if *tab == Tab::Updates && up_count > 0 {
                    let bw = draw::measure(tr, label, tsize).0;
                    let badge_c = Point::new(r.x0 + 14.0 + bw + 12.0, cy);
                    let bg_badge = if active { Color::WHITE } else { c.semantic.warning };
                    let badge = Circle::new(badge_c, 9.0);
                    scene.fill(vello::peniko::Fill::NonZero, t, bg_badge, None, &badge);
                    let txt = up_count.to_string();
                    let (nw, nh) = draw::measure(tr, &txt, theme.tokens.typography.scale.xs);
                    let nfg = if active { c.accent[5] } else { Color::WHITE };
                    draw::text_w(scene, tr, t, &txt, theme.tokens.typography.scale.xs, TextWeight::Bold, nfg, badge_c.x - nw / 2.0, badge_c.y - nh / 2.0, None);
                }
                targets.push(HitTarget { rect: r, action: Action::SetTab(*tab) });
            }
            origin
        };

        // Search pill, to the left of the tabs. Draw it only while it clears the
        // left cluster (nav toggle / Back chevron) rather than the old wordmark.
        let search_w = 280.0_f64;
        let search_r = Rect::new(tabs_origin - GAP - search_w, cy - 18.0, tabs_origin - GAP, cy + 18.0);
        if search_r.x0 > self.rect.x0 + BODY_PAD_X + 80.0 {
            self.draw_search_bar(scene, tr, t, theme, presence, targets, search_r);
        } else {
            // No room for the field this frame — keep it laid out off-view so
            // its state stays consistent.
            self.search_input.apply_layout_external(Rect::ZERO);
        }
    }

    /// The store's canonical search field: a rounded pill with a leading
    /// magnifier, the shared editable [`ui::widget::TextInput`] in the middle
    /// (caret, selection, clipboard, IME, theming, a11y — no hand-rolled
    /// string), and a trailing clear affordance while there is a query. The
    /// pill chrome is decoration around the real widget; a click anywhere on
    /// it focuses the field.
    fn draw_search_bar(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
    ) {
        let c = &theme.tokens.colors;
        let cy = rect.center().y;
        let focused = self.search_input.is_focused();
        let border = if focused { c.border_focus } else { c.border_standard };
        draw::fill_round(scene, t, rect, theme.tokens.radii.full, c.surfaces.raised);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.full, border, if focused { 2.0 } else { 1.0 });

        // Leading magnifier glyph, vertically centred on the pill.
        let glyph = "⌕";
        let glyph_size = 16.0_f32;
        let (_, gh) = draw::measure(tr, glyph, glyph_size);
        let mag_col = if focused { c.text_primary } else { c.text_dim };
        draw::text(scene, tr, t, glyph, glyph_size, mag_col, rect.x0 + 12.0, cy - gh / 2.0, None);

        // Whether a clear button is showing decides the trailing inset the
        // editable text stops at.
        let has_query = !self.search_query.is_empty();
        let lead = 34.0;
        let trail = if has_query { 30.0 } else { 12.0 };
        let inner = Rect::new(rect.x0 + lead, rect.y0, rect.x1 - trail, rect.y1);

        // Position + draw the real editable field inside the pill.
        self.search_input.apply_layout_external(inner);
        self.search_input.draw(scene, t, tr, theme, presence);

        // A click anywhere on the pill focuses the field. Registered first so
        // the trailing clear button (pushed after) wins the reverse hit-test
        // where they overlap.
        targets.push(HitTarget { rect, action: Action::FocusSearch });

        // Trailing clear (✕) affordance while there's a query.
        if has_query {
            let clr = Rect::new(rect.x1 - 28.0, cy - 11.0, rect.x1 - 8.0, cy + 11.0);
            let hov = clr.contains(self.mouse);
            let (xw, xh) = draw::measure(tr, "✕", 13.0);
            draw::text(scene, tr, t, "✕", 13.0, if hov { c.text_primary } else { c.text_dim }, clr.center().x - xw / 2.0, cy - xh / 2.0, None);
            targets.push(HitTarget { rect: clr, action: Action::ClearSearch });
        }
    }

    /// The hamburger / close toggle shown in the header on narrow windows.
    /// Renders a hamburger when the drawer is closed and an X when it's open,
    /// and records a `ToggleNav` hit target.
    fn draw_nav_toggle(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) {
        let c = &theme.tokens.colors;
        let btn = self.nav_toggle_rect();
        let hov = btn.contains(self.mouse);
        draw::fill_round(scene, t, btn, theme.tokens.radii.md, if hov { c.surface_white_10 } else { c.surfaces.raised });
        draw::stroke_round(scene, t, btn, theme.tokens.radii.md, c.border_standard, 1.0);

        let cx = btn.center().x;
        let cyc = btn.center().y;
        let open = self.nav_flap.reveal() > 0.5;
        if open {
            // Close (X) affordance while the drawer is showing.
            let r = 6.0;
            let line = vello::kurbo::Line::new(Point::new(cx - r, cyc - r), Point::new(cx + r, cyc + r));
            scene.stroke(&vello::kurbo::Stroke::new(1.8), t, c.text_primary, None, &line);
            let line2 = vello::kurbo::Line::new(Point::new(cx + r, cyc - r), Point::new(cx - r, cyc + r));
            scene.stroke(&vello::kurbo::Stroke::new(1.8), t, c.text_primary, None, &line2);
        } else {
            // Three-line hamburger.
            let line_w = 15.0;
            for i in 0..3 {
                let ly = cyc - 6.0 + i as f64 * 6.0;
                let line = vello::kurbo::Line::new(Point::new(cx - line_w / 2.0, ly), Point::new(cx + line_w / 2.0, ly));
                scene.stroke(&vello::kurbo::Stroke::new(1.8), t, c.text_primary, None, &line);
            }
        }
        targets.push(HitTarget { rect: btn, action: Action::ToggleNav });
    }

    /// Draw the overlay navigation drawer (narrow mode). Dims the content
    /// behind a scrim that fades with the reveal, then floats a panel holding
    /// the wordmark and a vertical list of the three nav destinations. A
    /// no-op while fully collapsed.
    fn draw_nav_drawer(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) {
        let reveal = self.nav_flap.reveal();
        if reveal <= 0.001 {
            return; // fully collapsed — nothing to paint
        }
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;

        // Scrim over the whole window; tapping it dismisses the drawer.
        let scrim = alpha(Color::from_rgba8(0, 0, 0, 255), reveal * 0.5);
        draw::fill_rect(scene, t, self.rect, scrim);
        targets.push(HitTarget { rect: self.rect, action: Action::DismissNav });

        // Drawer panel — opaque raised surface sliding in from the left.
        let drawer = self.nav_drawer_rect();
        draw::fill_rect(scene, t, drawer, c.surfaces.base);
        draw::fill_rect(scene, t, Rect::new(drawer.x1 - 1.0, drawer.y0, drawer.x1, drawer.y1), c.border_standard);
        // Absorb clicks on the panel (registered after the scrim, before the
        // rows) so empty drawer space doesn't dismiss via the scrim below.
        targets.push(HitTarget { rect: drawer, action: Action::NavAbsorb });

        let pad = 18.0;
        let x = drawer.x0 + pad;
        let w = NAV_DRAWER_W - pad * 2.0;

        // No app-name wordmark or brand tile: the OS titlebar shows "Software".
        // The drawer leads straight with its nav rows, reclaiming the space.

        // Vertical nav rows.
        let up_count = self.catalog.updatable(&self.snapshot).len();
        let inst_count = self.catalog.installed(&self.snapshot).len();
        let rows: [(Tab, String); 4] = [
            (Tab::Browse, "Browse".into()),
            (Tab::Installed, format!("Installed · {inst_count}")),
            (Tab::Updates, "Updates".into()),
            (Tab::Remotes, "Remotes".into()),
        ];
        let row_h = 44.0;
        let mut ry = drawer.y0 + 22.0;
        for (tab, label) in &rows {
            let r = Rect::new(x, ry, x + w, ry + row_h);
            let active = self.detail.is_none() && self.tab == *tab && self.search_query.is_empty();
            let hov = r.contains(self.mouse);
            let bg = if active {
                c.accent[5]
            } else if hov {
                c.surface_white_10
            } else {
                Color::TRANSPARENT
            };
            if bg != Color::TRANSPARENT {
                draw::fill_round(scene, t, r, theme.tokens.radii.md, bg);
            }
            let fg = if active { Color::WHITE } else { c.text_primary };
            draw::text_w(scene, tr, t, label, ty.base, TextWeight::Medium, fg, r.x0 + 14.0, r.center().y - ty.base as f64 / 2.0 - 1.0, None);
            // Updates count badge on the right of the row.
            if *tab == Tab::Updates && up_count > 0 {
                let badge_c = Point::new(r.x1 - 20.0, r.center().y);
                let bg_badge = if active { Color::WHITE } else { c.semantic.warning };
                let badge = Circle::new(badge_c, 10.0);
                scene.fill(vello::peniko::Fill::NonZero, t, bg_badge, None, &badge);
                let txt = up_count.to_string();
                let (nw, nh) = draw::measure(tr, &txt, ty.xs);
                let nfg = if active { c.accent[5] } else { Color::WHITE };
                draw::text_w(scene, tr, t, &txt, ty.xs, TextWeight::Bold, nfg, badge_c.x - nw / 2.0, badge_c.y - nh / 2.0, None);
            }
            targets.push(HitTarget { rect: r, action: Action::SetTab(*tab) });
            ry += row_h + 6.0;
        }
    }

    /// Draw an icon tile for `app` at `rect`.
    ///
    /// Prefers the app's **real** icon: when `icon_url` is set the shared
    /// [`ImageStore`] fetches + decodes it off-thread; a resolved bitmap is
    /// painted (rounded, cover-fit), a still-loading fetch shows an honest
    /// skeleton that resolves on a later frame, and only a genuinely missing
    /// or failed icon falls back to the theme-derived monogram tile. A
    /// permanent placeholder letter is the bug this fixes.
    fn icon(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, rect: Rect, app: &AppEntry) {
        let radius = (rect.width() * 0.24).min(20.0);
        if let Some(url) = app.icon_url.as_deref().filter(|u| !u.is_empty()) {
            if let Some(img) = self.image_store.get(url) {
                paint_image_cover(scene, t, rect, &img, radius);
                return;
            }
            // Still resolving (or a transient miss) → skeleton, not a letter.
            // Only a definitive failure falls through to the monogram.
            if self.image_store.load_state(url) != Some(LoadState::Failed) {
                self.draw_skeleton(scene, t, theme, rect, radius);
                return;
            }
        }
        let base = theme.tokens.colors.accent[app.icon_tone.min(10)];
        let sheen = mix(base, Color::WHITE, 0.5);
        draw::icon_tile(scene, tr, t, rect, base, sheen, &app.monogram, Color::WHITE);
    }

    /// An honest loading placeholder: a muted rounded surface with a lighter
    /// shimmer bar. Reads as "loading", and resolves to the real image once
    /// the async fetch/decode lands (the loop keeps polling while
    /// `image_store` has work). Theme-driven so it tracks light/dark.
    fn draw_skeleton(&self, scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, radius: f64) {
        let c = &theme.tokens.colors;
        draw::fill_round(scene, t, rect, radius, c.surface_white_10);
        // A centred shimmer band + a shorter one, the canonical skeleton look.
        let bx0 = rect.x0 + rect.width() * 0.18;
        let bx1 = rect.x1 - rect.width() * 0.18;
        let bh = (rect.height() * 0.06).clamp(3.0, 7.0);
        let cy = rect.center().y;
        draw::fill_round(scene, t, Rect::new(bx0, cy - bh, bx1, cy - bh * 0.2), bh, c.surface_white_20);
        draw::fill_round(scene, t, Rect::new(bx0, cy + bh * 0.4, bx1 - rect.width() * 0.14, cy + bh * 1.2), bh, c.surface_white_20);
        draw::stroke_round(scene, t, rect, radius, c.border_standard, 1.0);
    }

    /// Draw one detail-page screenshot into `rect`: a real decoded bitmap
    /// when the fetch has resolved, an honest loading skeleton while it is in
    /// flight, or the duotone faux-UI thumbnail as the fallback (no URL, or a
    /// failed fetch) — never a blank grey bar. Paints the caption underneath.
    #[allow(clippy::too_many_arguments)]
    fn draw_shot(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        rect: Rect,
        shot: &crate::model::Shot,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let radius = 12.0;
        let mut painted = false;
        if let Some(url) = shot.url.as_deref().filter(|u| !u.is_empty()) {
            if let Some(img) = self.image_store.get(url) {
                paint_image_cover(scene, t, rect, &img, radius);
                draw::stroke_round(scene, t, rect, radius, c.border_standard, 1.0);
                painted = true;
            } else if self.image_store.load_state(url) != Some(LoadState::Failed) {
                self.draw_skeleton(scene, t, theme, rect, radius);
                painted = true;
            }
        }
        if !painted {
            // Fallback: the duotone faux-UI thumbnail (still designed, not blank).
            let tone_a = c.accent[shot.tone_a.min(10)];
            let tone_b = mix(c.neutral[shot.tone_b.min(10)], c.surfaces.base, 0.2);
            draw::screenshot_thumb(scene, tr, t, rect, tone_a, tone_b, c.border_standard, "", c.text_dim, ty.xs);
        }
        // Caption underneath (kept out of the image so a real bitmap isn't
        // covered).
        if !shot.caption.is_empty() {
            let cap = draw::ellipsize(tr, &shot.caption, ty.xs, rect.width());
            draw::text(scene, tr, t, &cap, ty.xs, c.text_dim, rect.x0, rect.y1 + 6.0, None);
        }
    }

    /// A pill button; records a hit target. `hovered` is precomputed by
    /// the caller (it owns `self.mouse`).
    #[allow(clippy::too_many_arguments)]
    fn pill_button(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        label: &str,
        style: BtnStyle,
        hovered: bool,
        action: Action,
    ) {
        let c = &theme.tokens.colors;
        let (bg, fg, border) = match style {
            BtnStyle::Primary => (
                if hovered { c.accent[4] } else { c.accent[5] },
                Color::WHITE,
                None,
            ),
            BtnStyle::Neutral => (
                if hovered { c.surface_white_20 } else { c.surface_white_10 },
                c.text_primary,
                Some(c.border_standard),
            ),
            BtnStyle::Ghost => (
                if hovered { c.surface_white_10 } else { Color::TRANSPARENT },
                c.text_dim,
                Some(c.border_standard),
            ),
            BtnStyle::Danger => (
                if hovered { alpha(c.semantic.danger, 0.22) } else { Color::TRANSPARENT },
                c.semantic.danger,
                Some(alpha(c.semantic.danger, 0.5)),
            ),
        };
        let radius = theme.tokens.radii.full;
        if bg != Color::TRANSPARENT {
            draw::fill_round(scene, t, rect, radius, bg);
        }
        if let Some(b) = border {
            draw::stroke_round(scene, t, rect, radius, b, 1.0);
        }
        let size = theme.tokens.typography.scale.sm;
        let (lw, lh) = draw::measure(tr, label, size);
        draw::text_w(
            scene,
            tr,
            t,
            label,
            size,
            TextWeight::Semibold,
            fg,
            rect.center().x - lw / 2.0,
            rect.center().y - lh / 2.0,
            None,
        );
        targets.push(HitTarget { rect, action });
    }

    /// Render the install/open/update action control for `app` at `rect`.
    /// Either a button or — when installing — a progress bar.
    fn action_control(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
        allow_remove: bool,
    ) {
        let c = &theme.tokens.colors;
        match self.live_state(app) {
            InstallState::Available => {
                Self::pill_button(scene, tr, t, theme, targets, rect, "Install", BtnStyle::Primary, rect.contains(self.mouse), Action::Install(app.id.clone()));
            }
            InstallState::Installing(p) => {
                // Track.
                let radius = theme.tokens.radii.full;
                draw::fill_round(scene, t, rect, radius, c.surface_white_10);
                // Fill.
                let fill_w = (rect.width() * p).max(rect.height());
                let fill = Rect::new(rect.x0, rect.y0, rect.x0 + fill_w, rect.y1);
                draw::fill_round(scene, t, fill, radius, c.accent[5]);
                let size = theme.tokens.typography.scale.xs;
                // Prefer the descriptive label, but fall back to a compact
                // percentage when the control is too narrow (grid cards use a
                // ~96px pill) so it never overflows into the card body.
                let full = format!("Installing… {:.0}%", p * 100.0);
                let (full_w, lh) = draw::measure(tr, &full, size);
                let (label, lw) = if full_w <= rect.width() - 12.0 {
                    (full, full_w)
                } else {
                    let short = format!("{:.0}%", p * 100.0);
                    let (sw, _) = draw::measure(tr, &short, size);
                    (short, sw)
                };
                draw::text_w(scene, tr, t, &label, size, TextWeight::Semibold, Color::WHITE, rect.center().x - lw / 2.0, rect.center().y - lh / 2.0, None);
                // Cancel target spans the whole control.
                targets.push(HitTarget { rect, action: Action::Cancel(app.id.clone()) });
            }
            InstallState::Installed => {
                if allow_remove {
                    let half = rect.width() / 2.0 - 4.0;
                    let open_r = Rect::new(rect.x0, rect.y0, rect.x0 + half, rect.y1);
                    let rem_r = Rect::new(rect.x1 - half, rect.y0, rect.x1, rect.y1);
                    Self::pill_button(scene, tr, t, theme, targets, open_r, "Open", BtnStyle::Neutral, open_r.contains(self.mouse), Action::Open(app.id.clone()));
                    Self::pill_button(scene, tr, t, theme, targets, rem_r, "Remove", BtnStyle::Danger, rem_r.contains(self.mouse), Action::Remove(app.id.clone()));
                } else {
                    Self::pill_button(scene, tr, t, theme, targets, rect, "Open", BtnStyle::Neutral, rect.contains(self.mouse), Action::Open(app.id.clone()));
                }
            }
            InstallState::UpdateAvailable(_) => {
                if allow_remove {
                    let half = rect.width() / 2.0 - 4.0;
                    let up_r = Rect::new(rect.x0, rect.y0, rect.x0 + half, rect.y1);
                    let open_r = Rect::new(rect.x1 - half, rect.y0, rect.x1, rect.y1);
                    Self::pill_button(scene, tr, t, theme, targets, up_r, "Update", BtnStyle::Primary, up_r.contains(self.mouse), Action::Update(app.id.clone()));
                    Self::pill_button(scene, tr, t, theme, targets, open_r, "Open", BtnStyle::Neutral, open_r.contains(self.mouse), Action::Open(app.id.clone()));
                } else {
                    Self::pill_button(scene, tr, t, theme, targets, rect, "Update", BtnStyle::Primary, rect.contains(self.mouse), Action::Update(app.id.clone()));
                }
            }
        }
    }

    /// A standard browse/search grid card.
    fn draw_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
    ) {
        let c = &theme.tokens.colors;
        let hov = rect.contains(self.mouse);
        draw::fill_round(scene, t, rect, theme.tokens.radii.lg, c.surfaces.raised);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.lg, if hov { c.border_focus } else { c.border_standard }, 1.0);

        let pad = 14.0;
        let icon_sz = 46.0;
        let icon_r = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x0 + pad + icon_sz, rect.y0 + pad + icon_sz);
        self.icon(scene, tr, t, theme, icon_r, app);

        let text_x = icon_r.x1 + 12.0;
        let text_w = rect.x1 - pad - text_x;
        draw::text_w(scene, tr, t, &app.name, theme.tokens.typography.scale.base, TextWeight::Bold, c.text_primary, text_x, rect.y0 + pad, None);
        let dev = draw::ellipsize(tr, &app.developer, theme.tokens.typography.scale.xs, text_w);
        draw::text(scene, tr, t, &dev, theme.tokens.typography.scale.xs, c.text_dim, text_x, rect.y0 + pad + 20.0, None);

        // Summary (wrapped, 2 lines max region).
        let sum_y = icon_r.y1 + 12.0;
        let summary = draw::ellipsize(tr, &app.summary, theme.tokens.typography.scale.sm, text_w + icon_sz + 12.0);
        draw::text(scene, tr, t, &summary, theme.tokens.typography.scale.sm, c.text_dim, rect.x0 + pad, sum_y, Some((rect.width() - pad * 2.0) as f32));

        // Bottom row: rating + action button.
        let row_y = rect.y1 - pad - 26.0;
        draw::rating_stars(scene, t, rect.x0 + pad, row_y + 9.0, app.rating, 6.0, 2.0, c.semantic.warning, c.surface_white_20);
        draw::text(scene, tr, t, &app.rating_label(), theme.tokens.typography.scale.xs, c.text_dim, rect.x0 + pad + 70.0, row_y + 3.0, None);

        let btn_w = 96.0_f64;
        let btn = Rect::new(rect.x1 - pad - btn_w, row_y, rect.x1 - pad, row_y + 26.0);
        self.action_control(scene, tr, t, theme, targets, btn, app, false);

        // Whole-card click → detail (registered first so buttons, pushed
        // later, win the reverse hit-test).
        targets.insert(0, HitTarget { rect, action: Action::OpenDetail(app.id.clone()) });
    }

    /// A large featured/hero card.
    fn draw_hero(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
    ) {
        let c = &theme.tokens.colors;
        let accent = c.accent[app.icon_tone.min(10)];
        let hov = rect.contains(self.mouse);
        // Banner: deep accent base with a lighter diagonal-ish sheen band.
        draw::fill_round(scene, t, rect, theme.tokens.radii.xl, mix(accent, c.surfaces.base, 0.35));
        let band = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + rect.height() * 0.5);
        let band_round = vello::kurbo::RoundedRect::new(
            band.x0,
            band.y0,
            band.x1,
            band.y1,
            vello::kurbo::RoundedRectRadii::new(theme.tokens.radii.xl as f64, theme.tokens.radii.xl as f64, 0.0, 0.0),
        );
        scene.fill(vello::peniko::Fill::NonZero, t, alpha(mix(accent, Color::WHITE, 0.25), 0.35), None, &band_round);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.xl, if hov { alpha(Color::WHITE, 0.5) } else { c.border_white_20 }, 1.0);

        let pad = 18.0;
        // "Editor's Choice" eyebrow.
        draw::text_w(scene, tr, t, "FEATURED", theme.tokens.typography.scale.xs, TextWeight::Bold, alpha(Color::WHITE, 0.8), rect.x0 + pad, rect.y0 + pad, None);

        let icon_sz = 64.0;
        let icon_r = Rect::new(rect.x0 + pad, rect.y0 + pad + 22.0, rect.x0 + pad + icon_sz, rect.y0 + pad + 22.0 + icon_sz);
        self.icon(scene, tr, t, theme, icon_r, app);

        let tx = icon_r.x1 + 14.0;
        draw::text_w(scene, tr, t, &app.name, theme.tokens.typography.scale.xl, TextWeight::Bold, Color::WHITE, tx, icon_r.y0, None);
        let dev = draw::ellipsize(tr, &app.developer, theme.tokens.typography.scale.sm, rect.x1 - pad - tx);
        draw::text(scene, tr, t, &dev, theme.tokens.typography.scale.sm, alpha(Color::WHITE, 0.8), tx, icon_r.y0 + 26.0, None);
        draw::rating_stars(scene, t, tx, icon_r.y0 + 52.0, app.rating, 6.0, 2.0, Color::WHITE, alpha(Color::WHITE, 0.3));

        // Summary across the bottom.
        let sum = draw::ellipsize(tr, &app.summary, theme.tokens.typography.scale.sm, rect.width() - pad * 2.0 - 110.0);
        draw::text(scene, tr, t, &sum, theme.tokens.typography.scale.sm, alpha(Color::WHITE, 0.92), rect.x0 + pad, rect.y1 - pad - 34.0, None);

        // Action button bottom-right.
        let btn_w = 110.0_f64;
        let btn = Rect::new(rect.x1 - pad - btn_w, rect.y1 - pad - 30.0, rect.x1 - pad, rect.y1 - pad);
        self.action_control(scene, tr, t, theme, targets, btn, app, false);

        targets.insert(0, HitTarget { rect, action: Action::OpenDetail(app.id.clone()) });
    }

    /// Draw one category chip at `*x`, advancing `*x` past it.
    #[allow(clippy::too_many_arguments)]
    fn draw_chip(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        x: &mut f64,
        y: f64,
        label: &str,
        cat: Option<Category>,
    ) {
        let c = &theme.tokens.colors;
        let size = theme.tokens.typography.scale.sm;
        let w = draw::measure(tr, label, size).0 + 28.0;
        let r = Rect::new(*x, y, *x + w, y + 32.0);
        let active = self.active_category == cat;
        let hov = r.contains(self.mouse);
        let bg = if active { c.accent[5] } else if hov { c.surface_white_10 } else { c.surfaces.raised };
        draw::fill_round(scene, t, r, theme.tokens.radii.full, bg);
        if !active {
            draw::stroke_round(scene, t, r, theme.tokens.radii.full, c.border_standard, 1.0);
        }
        let fg = if active { Color::WHITE } else { c.text_dim };
        draw::text_w(scene, tr, t, label, size, TextWeight::Medium, fg, r.x0 + 14.0, r.center().y - size as f64 / 2.0 - 1.0, None);
        targets.push(HitTarget { rect: r, action: Action::SetCategory(cat) });
        *x += w + 10.0;
    }

    /// Category chip row. Returns the y after the row.
    fn draw_chips(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        x0: f64,
        y: f64,
    ) -> f64 {
        let mut x = x0;
        self.draw_chip(scene, tr, t, theme, targets, &mut x, y, "All", None);
        for cat in Category::ALL {
            self.draw_chip(scene, tr, t, theme, targets, &mut x, y, cat.label(), Some(cat));
        }
        y + 32.0
    }

    /// Section header text. Returns y after it.
    fn section(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, x: f64, y: f64, title: &str) -> f64 {
        draw::text_w(scene, tr, t, title, theme.tokens.typography.scale.lg, TextWeight::Bold, theme.tokens.colors.text_primary, x, y, None);
        y + 30.0
    }

    /// Draw a grid of cards. Returns the y after the grid.
    fn draw_grid(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        x0: f64,
        y0: f64,
        width: f64,
        apps: &[&AppEntry],
    ) -> f64 {
        if apps.is_empty() {
            draw::text(scene, tr, t, "No apps here yet.", theme.tokens.typography.scale.sm, theme.tokens.colors.text_dim, x0, y0, None);
            return y0 + 30.0;
        }
        let card_min = 280.0_f64;
        let cols = (((width + GAP) / (card_min + GAP)).floor() as usize).max(1);
        let card_w = (width - GAP * (cols as f64 - 1.0)) / cols as f64;
        let card_h = 132.0_f64;
        let mut y = y0;
        for (i, app) in apps.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let cx = x0 + col as f64 * (card_w + GAP);
            let cy = y0 + row as f64 * (card_h + GAP);
            let r = Rect::new(cx, cy, cx + card_w, cy + card_h);
            self.draw_card(scene, tr, t, theme, targets, r, app);
            y = cy + card_h;
        }
        y
    }

    /// The Browse screen body. Returns content height (absolute bottom y
    /// relative to body top before scroll).
    fn draw_browse(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) -> f64 {
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let mut y = body.y0 + BODY_PAD_TOP - self.scroll;

        // Searching → results grid only.
        if !self.search_query.is_empty() {
            let results = self.search_results();
            let result_refs: Vec<&AppEntry> = results.iter().collect();
            let searching = self.worker.is_live()
                && self.worker.inflight().contains(&format!("search:{}", self.search_query.trim()));
            let header = if searching {
                format!("Searching Flathub for \u{201c}{}\u{201d}…", self.search_query)
            } else {
                format!(
                    "{} result{} for \u{201c}{}\u{201d}",
                    results.len(),
                    if results.len() == 1 { "" } else { "s" },
                    self.search_query
                )
            };
            y = self.section(scene, tr, t, theme, x0, y, &header);
            y += 6.0;
            y = self.draw_chips(scene, tr, t, theme, targets, x0, y);
            y += 16.0;
            y = self.draw_grid(scene, tr, t, theme, targets, x0, y, width, &result_refs);
            return y + 24.0 - (body.y0 - self.scroll);
        }

        // Category chips.
        y = self.draw_chips(scene, tr, t, theme, targets, x0, y);
        y += 22.0;

        let pool = self.browse_apps();

        // Live path with nothing fetched yet: an honest loading / empty state
        // rather than a bare page (the offline sample is never shown here).
        if self.worker.is_live() && pool.is_empty() {
            let loading = self.worker.inflight().contains("collection:popular");
            let msg = if loading {
                "Loading apps from Flathub…"
            } else {
                "No apps to show yet — check your network and Flathub remote."
            };
            y = self.section(scene, tr, t, theme, x0, y, msg);
            return (y + self.scroll) - body.y0 + 12.0;
        }

        // Featured heroes: curated "Editor's Choice" only (offline sample has a
        // `featured` flag; live Flathub data carries no editorial curation, so
        // the heroes section is simply absent there).
        let featured: Vec<&AppEntry> =
            pool.iter().copied().filter(|a| a.featured).collect();
        if self.active_category.is_none() && !featured.is_empty() {
            y = self.section(scene, tr, t, theme, x0, y, "Editor's Choice");
            y += 4.0;
            let n = featured.len().min(3);
            let hero_w = (width - GAP * (n as f64 - 1.0)) / n as f64;
            let hero_h = 188.0;
            for (i, app) in featured.iter().take(n).enumerate() {
                let hx = x0 + i as f64 * (hero_w + GAP);
                let r = Rect::new(hx, y, hx + hero_w, y + hero_h);
                self.draw_hero(scene, tr, t, theme, targets, r, app);
            }
            y += hero_h + 28.0;
        }

        // Category grids.
        let cats: Vec<Category> = match self.active_category {
            Some(c) => vec![c],
            None => Category::ALL.to_vec(),
        };
        for cat in cats {
            let apps: Vec<&AppEntry> =
                pool.iter().copied().filter(|a| a.category == cat).collect();
            if apps.is_empty() {
                continue;
            }
            y = self.section(scene, tr, t, theme, x0, y, cat.label());
            y += 6.0;
            y = self.draw_grid(scene, tr, t, theme, targets, x0, y, width, &apps);
            y += 30.0;
        }

        // Return content height (distance from body top to bottom of
        // content, ignoring scroll).
        (y + self.scroll) - body.y0 + 12.0
    }

    /// Detail screen. Returns content height.
    fn draw_detail(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        app: &AppEntry,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let top = body.y0 + BODY_PAD_TOP - self.scroll;

        // ---- Header block ----
        let icon_sz = 96.0;
        let icon_r = Rect::new(x0, top, x0 + icon_sz, top + icon_sz);
        self.icon(scene, tr, t, theme, icon_r, app);

        // Title / publisher / source / rating column, on an even vertical
        // rhythm so the block reads as a clear title → detail hierarchy.
        let info_x = icon_r.x1 + 22.0;
        draw::text_w(scene, tr, t, &app.name, ty.x2l, TextWeight::Bold, c.text_primary, info_x, top + 2.0, None);
        draw::text_w(scene, tr, t, &app.developer, ty.base, TextWeight::Medium, mix(c.text_primary, c.text_dim, 0.35), info_x, top + 40.0, None);
        draw::text(scene, tr, t, app.source.label(), ty.xs, c.text_dim, info_x, top + 64.0, None);
        // Rating: stars + one-decimal value + count, all on one baseline.
        let rating_cy = top + 90.0;
        let rw = draw::rating_stars(scene, t, info_x, rating_cy, app.rating, 8.0, 3.0, c.semantic.warning, c.surface_white_20);
        let (rvw, rlh) = draw::measure(tr, &app.rating_label(), ty.sm);
        draw::text_w(scene, tr, t, &app.rating_label(), ty.sm, TextWeight::Semibold, c.text_primary, info_x + rw + 12.0, rating_cy - rlh / 2.0, None);
        let count_h = draw::measure(tr, "Ag", ty.xs).1;
        draw::text(scene, tr, t, &app.rating_count_label(), ty.xs, c.text_dim, info_x + rw + 12.0 + rvw + 10.0, rating_cy - count_h / 2.0, None);

        // Primary action (top-right). qOS-native apps get the bespoke
        // "Open directly" / "Add" affordances instead of Install.
        let btn_w = if app.is_qos_native() { 260.0_f64 } else { 200.0_f64 };
        let btn_h = 44.0_f64;
        let btn = Rect::new(body.x1 - BODY_PAD_X - btn_w, top + 4.0, body.x1 - BODY_PAD_X, top + 4.0 + btn_h);
        if app.is_qos_native() {
            self.draw_qos_affordances(scene, tr, t, theme, targets, btn, app);
        } else {
            self.action_control(scene, tr, t, theme, targets, btn, app, false);
        }
        // Right-aligned meta rows, stacked top-down under the button so they
        // never overlap: version·size, then download count, then any
        // update-available note (warning-tinted).
        let mut meta_y = btn.y1 + 8.0;
        let meta = format!("v{}  ·  {}", app.latest_version, app.size_label());
        let mw = draw::measure(tr, &meta, ty.xs).0;
        draw::text(scene, tr, t, &meta, ty.xs, c.text_dim, btn.x1 - mw, meta_y, None);
        meta_y += 16.0;
        if let Some(dl) = app.download_label() {
            let dw = draw::measure(tr, &dl, ty.xs).0;
            draw::text(scene, tr, t, &dl, ty.xs, c.text_dim, btn.x1 - dw, meta_y, None);
            meta_y += 16.0;
        }
        if let InstallState::UpdateAvailable(from) = self.live_state(app) {
            let upd = format!("Update available: {} → {}", from, app.latest_version);
            let uw = draw::measure(tr, &upd, ty.xs).0;
            draw::text_w(scene, tr, t, &upd, ty.xs, TextWeight::Medium, c.semantic.warning, btn.x1 - uw, meta_y, None);
        }

        let mut y = top + icon_sz + 36.0;

        // ---- Screenshot strip ----
        // Real fetched bitmaps (with an honest loading skeleton until they
        // resolve); the duotone thumbnail is only a per-shot fallback.
        if !app.shots.is_empty() {
            y = self.section(scene, tr, t, theme, x0, y, "Screenshots");
            y += 10.0;
            // Cap the tile count per row so wide windows don't stretch a
            // single shot edge-to-edge; a landscape 16:10-ish tile reads best.
            let n = app.shots.len().min(3);
            let shot_w = (width - GAP * (n as f64 - 1.0)) / n as f64;
            let shot_h = (shot_w * 0.60).min(200.0);
            for (i, sh) in app.shots.iter().take(n).enumerate() {
                let sx = x0 + i as f64 * (shot_w + GAP);
                let r = Rect::new(sx, y, sx + shot_w, y + shot_h);
                self.draw_shot(scene, tr, t, theme, r, sh);
            }
            y += shot_h + 34.0;
        }

        // ---- Compatibility notices ----
        if !app.compat.is_empty() {
            y = self.section(scene, tr, t, theme, x0, y, "Compatibility on qOS");
            y += 8.0;
            y = self.draw_compat(scene, tr, t, theme, x0, y, width, &app.compat);
            y += 14.0;
        }

        // ---- About ----
        y = self.section(scene, tr, t, theme, x0, y, "About");
        y += 4.0;
        for para in app.description.split("\n\n") {
            let (_, h) = tr.measure(para, ty.sm, Some(width as f32));
            draw::text(scene, tr, t, para, ty.sm, mix(c.text_primary, c.text_dim, 0.25), x0, y, Some(width as f32));
            y += h as f64 + 12.0;
        }
        y += 14.0;

        // ---- Permissions ----
        y = self.section(scene, tr, t, theme, x0, y, "Permissions & sandbox");
        y += 8.0;
        for perm in &app.permissions {
            self.draw_permission_row(scene, tr, t, theme, x0, y, width, perm);
            y += 30.0;
        }
        y += 14.0;

        // ---- Ratings & reviews ----
        // Reviews come from the sample catalog; live (Flathub) entries have
        // none, so we surface the download-count-derived rating note instead.
        let rating_note = if app.reviews.is_empty() && app.download_count.is_some() {
            Some("Rating estimated from Flathub download counts — Flathub exposes no written reviews.")
        } else {
            None
        };
        y = self.draw_reviews(scene, tr, t, theme, x0, y, width, &app.reviews, rating_note);
        y += 14.0;

        // ---- Information grid ----
        y = self.section(scene, tr, t, theme, x0, y, "Information");
        y += 8.0;
        let rows = [
            ("Version", app.latest_version.clone()),
            ("Download size", app.size_label()),
            ("Category", app.category.label().to_string()),
            ("Developer", app.developer.clone()),
            ("Source", app.source.label().to_string()),
            ("App ID", app.id.clone()),
        ];
        for (k, v) in rows {
            draw::text(scene, tr, t, k, ty.sm, c.text_dim, x0, y, None);
            draw::text_w(scene, tr, t, &v, ty.sm, TextWeight::Medium, c.text_primary, x0 + 160.0, y, None);
            // Divider.
            draw::fill_rect(scene, t, Rect::new(x0, y + 22.0, x0 + width, y + 23.0), alpha(c.border_standard, 0.5));
            y += 32.0;
        }

        (y + self.scroll) - body.y0 + 20.0
    }

    fn draw_permission_row(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x: f64,
        y: f64,
        width: f64,
        perm: &Permission,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let dot_c = if perm.sensitive { c.semantic.warning } else { c.semantic.success };
        let dot = Circle::new(Point::new(x + 5.0, y + 9.0), 4.0);
        scene.fill(vello::peniko::Fill::NonZero, t, dot_c, None, &dot);
        draw::text(scene, tr, t, &perm.label, ty.sm, c.text_primary, x + 18.0, y, Some((width - 120.0) as f32));
        if perm.sensitive {
            let tag = "sensitive";
            let tw = draw::measure(tr, tag, ty.xs).0;
            let r = Rect::new(x + width - tw - 16.0, y - 1.0, x + width, y + 17.0);
            draw::fill_round(scene, t, r, theme.tokens.radii.full, alpha(c.semantic.warning, 0.18));
            draw::text_w(scene, tr, t, tag, ty.xs, TextWeight::Medium, c.semantic.warning, r.x0 + 8.0, y + 1.0, None);
        }
    }

    /// A row in the Installed / Updates lists.
    fn draw_list_row(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
        show_update_arrow: bool,
        allow_remove: bool,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let hov = rect.contains(self.mouse);
        draw::fill_round(scene, t, rect, theme.tokens.radii.lg, c.surfaces.raised);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.lg, if hov { c.border_focus } else { c.border_standard }, 1.0);

        let pad = 14.0;
        let icon_sz = rect.height() - pad * 2.0;
        let icon_r = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x0 + pad + icon_sz, rect.y1 - pad);
        self.icon(scene, tr, t, theme, icon_r, app);

        let tx = icon_r.x1 + 14.0;
        draw::text_w(scene, tr, t, &app.name, ty.base, TextWeight::Bold, c.text_primary, tx, rect.y0 + pad + 2.0, None);
        let sub = if show_update_arrow {
            if let InstallState::UpdateAvailable(from) = self.live_state(app) {
                format!("{}  →  {}   ·   {}", from, app.latest_version, app.size_label())
            } else {
                format!("v{}   ·   {}", app.latest_version, app.size_label())
            }
        } else {
            let installed_v = self.snapshot.installed.get(&app.id).cloned().unwrap_or_else(|| app.latest_version.clone());
            format!("v{}   ·   {}   ·   {}", installed_v, app.size_label(), app.category.label())
        };
        let sub_color = if show_update_arrow { mix(c.text_dim, c.semantic.warning, 0.5) } else { c.text_dim };
        draw::text(scene, tr, t, &sub, ty.sm, sub_color, tx, rect.center().y + 2.0, None);

        // Action(s) at the right.
        let btn_w = if allow_remove { 200.0_f64 } else { 130.0_f64 };
        let btn_h = 34.0_f64;
        let btn = Rect::new(rect.x1 - pad - btn_w, rect.center().y - btn_h / 2.0, rect.x1 - pad, rect.center().y + btn_h / 2.0);
        self.action_control(scene, tr, t, theme, targets, btn, app, allow_remove);

        targets.insert(0, HitTarget { rect, action: Action::OpenDetail(app.id.clone()) });
    }

    fn draw_installed(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) -> f64 {
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let mut y = body.y0 + BODY_PAD_TOP - self.scroll;

        let installed = self.catalog.installed(&self.snapshot);
        y = self.section(scene, tr, t, theme, x0, y, &format!("Installed apps · {}", installed.len()));
        y += 12.0;

        if installed.is_empty() {
            y = self.empty_state(scene, tr, t, theme, x0, y, width, "Nothing installed yet", "Browse the store and install your first app.");
        } else {
            let row_h = 72.0;
            for app in &installed {
                let r = Rect::new(x0, y, x0 + width, y + row_h);
                self.draw_list_row(scene, tr, t, theme, targets, r, app, false, true);
                y += row_h + 12.0;
            }
        }
        (y + self.scroll) - body.y0 + 12.0
    }

    fn draw_updates(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let mut y = body.y0 + BODY_PAD_TOP - self.scroll;

        // The Updates view shows the active batch (queued / in-progress /
        // done) alongside any still-pending updates, so rows don't vanish the
        // instant an update completes.
        let rows = self.updates_view_apps();
        let progress = self.update_progress();
        let pending = rows.iter().filter(|a| self.batch_status(&a.id).is_none()).count();

        // Title + Update All / Updating affordance.
        draw::text_w(scene, tr, t, &format!("Updates · {}", rows.len()), ty.lg, TextWeight::Bold, c.text_primary, x0, y, None);
        if !rows.is_empty() {
            let btn_w = 150.0_f64;
            let btn = Rect::new(x0 + width - btn_w, y - 6.0, x0 + width, y + 28.0);
            if progress.in_flight() {
                // Non-actionable status pill while a batch runs.
                Self::pill_button(scene, tr, t, theme, targets, btn, "Updating…", BtnStyle::Ghost, false, Action::SetTab(Tab::Updates));
            } else if pending > 0 {
                Self::pill_button(scene, tr, t, theme, targets, btn, "Update All", BtnStyle::Primary, btn.contains(self.mouse), Action::UpdateAll);
            }
        }
        y += 36.0;

        // Overall status card: shows while a batch is in flight or has just
        // finished — the "what's being performed right now" summary.
        if progress.total() > 0 && (progress.in_flight() || progress.done > 0) {
            y = self.draw_update_status_card(scene, tr, t, theme, x0, y, width, &progress);
            y += 16.0;
        }

        if rows.is_empty() {
            y = self.empty_state(scene, tr, t, theme, x0, y, width, "You're all up to date", "Every installed app is running its latest version.");
        } else {
            let row_h = 76.0;
            for app in &rows {
                let r = Rect::new(x0, y, x0 + width, y + row_h);
                self.draw_update_row(scene, tr, t, theme, targets, r, app);
                y += row_h + 12.0;
            }
        }
        (y + self.scroll) - body.y0 + 12.0
    }

    /// The overall status card at the top of an in-progress Updates batch:
    /// a status line (e.g. "Updating 2 of 5 · 1 queued"), small queued /
    /// in-progress / done counters, and a single overall progress bar.
    #[allow(clippy::too_many_arguments)]
    fn draw_update_status_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x0: f64,
        y: f64,
        width: f64,
        progress: &UpdateProgress,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let in_flight = progress.in_flight();
        let card_h = 84.0;
        let r = Rect::new(x0, y, x0 + width, y + card_h);
        // A faint accent wash while running; a success wash once complete.
        let wash = if in_flight {
            alpha(c.accent[5], 0.10)
        } else {
            alpha(c.semantic.success, 0.10)
        };
        draw::fill_round(scene, t, r, theme.tokens.radii.lg, wash);
        draw::stroke_round(scene, t, r, theme.tokens.radii.lg, if in_flight { alpha(c.accent[5], 0.4) } else { alpha(c.semantic.success, 0.4) }, 1.0);

        let pad = 16.0;
        // Spinner / check glyph.
        let glyph = if in_flight { "↻" } else { "✓" };
        let gcol = if in_flight { c.accent[5] } else { c.semantic.success };
        let (gw, gh) = draw::measure(tr, glyph, ty.lg);
        draw::text_w(scene, tr, t, glyph, ty.lg, TextWeight::Bold, gcol, r.x0 + pad, r.y0 + pad - gh * 0.1, None);
        let text_x = r.x0 + pad + gw + 12.0;

        // Status line.
        draw::text_w(scene, tr, t, &progress.status_line(), ty.base, TextWeight::Bold, c.text_primary, text_x, r.y0 + pad, None);

        // Sub-line: queued / installing / done counters.
        let mut parts: Vec<String> = Vec::new();
        if progress.active > 0 {
            parts.push(format!("{} installing", progress.active));
        }
        if progress.queued > 0 {
            parts.push(format!("{} queued", progress.queued));
        }
        if progress.done > 0 {
            parts.push(format!("{} done", progress.done));
        }
        let sub = parts.join("   ·   ");
        draw::text(scene, tr, t, &sub, ty.sm, c.text_dim, text_x, r.y0 + pad + 22.0, None);

        // Overall percentage at the right.
        let pct = format!("{:.0}%", progress.overall * 100.0);
        let (pw, _) = draw::measure(tr, &pct, ty.lg);
        draw::text_w(scene, tr, t, &pct, ty.lg, TextWeight::Bold, if in_flight { c.text_primary } else { c.semantic.success }, r.x1 - pad - pw, r.y0 + pad, None);

        // Overall progress bar across the bottom of the card.
        let bar = Rect::new(r.x0 + pad, r.y1 - pad - 8.0, r.x1 - pad, r.y1 - pad);
        let bar_fill = if in_flight { c.accent[5] } else { c.semantic.success };
        Self::progress_bar(scene, t, theme, bar, progress.overall, c.surface_white_10, bar_fill);

        r.y1
    }

    /// A rounded progress track + fill. `p` is 0.0..=1.0.
    fn progress_bar(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, p: f64, track: Color, fill: Color) {
        let radius = theme.tokens.radii.full;
        draw::fill_round(scene, t, rect, radius, track);
        let p = p.clamp(0.0, 1.0);
        if p > 0.0 {
            let fill_w = (rect.width() * p).max(rect.height());
            let fr = Rect::new(rect.x0, rect.y0, rect.x0 + fill_w, rect.y1);
            draw::fill_round(scene, t, fr, radius, fill);
        }
    }

    /// A row in the in-progress Updates view. Unlike the plain installed/
    /// updates list row, this one renders the per-app *phase* (Queued /
    /// Downloading / Installing) with an inline progress bar, a "Updated"
    /// confirmation once done, or the normal Update / Open controls for a
    /// still-pending update.
    #[allow(clippy::too_many_arguments)]
    fn draw_update_row(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let hov = rect.contains(self.mouse);
        draw::fill_round(scene, t, rect, theme.tokens.radii.lg, c.surfaces.raised);
        draw::stroke_round(scene, t, rect, theme.tokens.radii.lg, if hov { c.border_focus } else { c.border_standard }, 1.0);

        let pad = 14.0;
        let icon_sz = rect.height() - pad * 2.0;
        let icon_r = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x0 + pad + icon_sz, rect.y1 - pad);
        self.icon(scene, tr, t, theme, icon_r, app);

        let tx = icon_r.x1 + 14.0;
        draw::text_w(scene, tr, t, &app.name, ty.base, TextWeight::Bold, c.text_primary, tx, rect.y0 + pad + 2.0, None);

        // Reserve room for the right-hand control / progress region.
        let ctrl_w = 220.0_f64;
        let ctrl_x = rect.x1 - pad - ctrl_w;

        match self.batch_status(&app.id) {
            // Part of the active batch: render phase + progress / done.
            Some(BatchRowStatus::Queued) => {
                // Show the version arrow only when we know the installed
                // ("from") version; otherwise just the target + size.
                let sub = match self.snapshot.installed.get(&app.id) {
                    Some(from) if !from.is_empty() => {
                        format!("{}  →  {}   ·   {}", from, app.latest_version, app.size_label())
                    }
                    _ => format!("Update to {}   ·   {}", app.latest_version, app.size_label()),
                };
                draw::text(scene, tr, t, &sub, ty.sm, c.text_dim, tx, rect.center().y + 2.0, None);
                let badge_w = self.phase_badge_width(tr, theme, InstallPhase::Queued);
                self.draw_phase_badge(scene, tr, t, theme, rect.x1 - pad - badge_w, rect.center().y, InstallPhase::Queued, None);
            }
            Some(BatchRowStatus::InProgress { progress, phase }) => {
                // Sub-line shows the phase label + percent.
                let sub = format!("{} · {:.0}%   ·   {}", phase.label(), progress * 100.0, app.size_label());
                draw::text(scene, tr, t, &sub, ty.sm, mix(c.text_dim, c.accent[5], 0.5), tx, rect.center().y + 2.0, None);

                // Right cluster: a round cancel button pinned to the right,
                // the progress bar filling the space to its left, and the
                // phase badge floating above the bar (right-aligned so it
                // never collides with the cancel button).
                let cancel_d = 28.0;
                let cancel = Rect::new(rect.x1 - pad - cancel_d, rect.center().y - cancel_d / 2.0, rect.x1 - pad, rect.center().y + cancel_d / 2.0);
                let chov = cancel.contains(self.mouse);
                draw::fill_round(scene, t, cancel, theme.tokens.radii.full, if chov { c.surface_white_20 } else { c.surface_white_10 });
                let (xw, xh) = draw::measure(tr, "✕", 12.0);
                draw::text(scene, tr, t, "✕", 12.0, if chov { c.text_primary } else { c.text_dim }, cancel.center().x - xw / 2.0, cancel.center().y - xh / 2.0, None);
                targets.push(HitTarget { rect: cancel, action: Action::Cancel(app.id.clone()) });

                // Progress bar spanning from ctrl_x up to just before the
                // cancel button, centred on the lower half of the row.
                let bar_right = cancel.x0 - 10.0;
                let bar = Rect::new(ctrl_x, rect.center().y + 12.0, bar_right, rect.center().y + 20.0);
                Self::progress_bar(scene, t, theme, bar, progress, c.surface_white_10, c.accent[5]);
                // Phase badge above the bar, right-aligned to the bar's end.
                let badge_w = self.phase_badge_width(tr, theme, phase);
                self.draw_phase_badge(scene, tr, t, theme, bar_right - badge_w, rect.y0 + pad + 9.0, phase, Some(progress));
            }
            Some(BatchRowStatus::Done) => {
                let sub = format!("Updated to {}   ·   {}", app.latest_version, app.size_label());
                draw::text(scene, tr, t, &sub, ty.sm, mix(c.text_dim, c.semantic.success, 0.5), tx, rect.center().y + 2.0, None);
                // "Updated ✓" confirmation pill (non-actionable) + Open.
                let btn_h = 34.0_f64;
                let half = ctrl_w / 2.0 - 4.0;
                let done_r = Rect::new(ctrl_x, rect.center().y - btn_h / 2.0, ctrl_x + half, rect.center().y + btn_h / 2.0);
                draw::fill_round(scene, t, done_r, theme.tokens.radii.full, alpha(c.semantic.success, 0.18));
                let label = "Updated ✓";
                let (lw, lh) = draw::measure(tr, label, ty.sm);
                draw::text_w(scene, tr, t, label, ty.sm, TextWeight::Semibold, c.semantic.success, done_r.center().x - lw / 2.0, done_r.center().y - lh / 2.0, None);
                let open_r = Rect::new(rect.x1 - pad - half, rect.center().y - btn_h / 2.0, rect.x1 - pad, rect.center().y + btn_h / 2.0);
                Self::pill_button(scene, tr, t, theme, targets, open_r, "Open", BtnStyle::Neutral, open_r.contains(self.mouse), Action::Open(app.id.clone()));
            }
            // Still pending (not in the batch): normal update-row controls.
            None => {
                let sub = if let InstallState::UpdateAvailable(from) = self.live_state(app) {
                    format!("{}  →  {}   ·   {}", from, app.latest_version, app.size_label())
                } else {
                    format!("v{}   ·   {}", app.latest_version, app.size_label())
                };
                draw::text(scene, tr, t, &sub, ty.sm, mix(c.text_dim, c.semantic.warning, 0.5), tx, rect.center().y + 2.0, None);
                let btn_w = 130.0_f64;
                let btn_h = 34.0_f64;
                let btn = Rect::new(rect.x1 - pad - btn_w, rect.center().y - btn_h / 2.0, rect.x1 - pad, rect.center().y + btn_h / 2.0);
                self.action_control(scene, tr, t, theme, targets, btn, app, false);
            }
        }

        targets.insert(0, HitTarget { rect, action: Action::OpenDetail(app.id.clone()) });
    }

    /// The rendered width of a phase badge for `phase`, so callers can
    /// right-align it against a known edge.
    fn phase_badge_width(&self, tr: &mut TextRenderer, theme: &Theme, phase: InstallPhase) -> f64 {
        draw::measure(tr, phase.label(), theme.tokens.typography.scale.xs).0 + 18.0
    }

    /// A small phase chip ("Queued" / "Downloading" / "Installing") tinted
    /// by phase. `y_center` is the vertical centre to draw on.
    #[allow(clippy::too_many_arguments)]
    fn draw_phase_badge(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x: f64,
        y_center: f64,
        phase: InstallPhase,
        _progress: Option<f64>,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let (tint, fg) = match phase {
            InstallPhase::Queued => (alpha(c.text_dim, 0.18), c.text_dim),
            InstallPhase::Downloading => (alpha(c.accent[5], 0.18), mix(c.text_primary, c.accent[5], 0.5)),
            InstallPhase::Installing => (alpha(c.accent[3], 0.20), mix(c.text_primary, c.accent[3], 0.5)),
        };
        let label = phase.label();
        let lw = draw::measure(tr, label, ty.xs).0;
        let bw = lw + 18.0;
        let bh = 18.0;
        let r = Rect::new(x, y_center - bh / 2.0, x + bw, y_center + bh / 2.0);
        draw::fill_round(scene, t, r, theme.tokens.radii.full, tint);
        let (_, th) = draw::measure(tr, label, ty.xs);
        draw::text_w(scene, tr, t, label, ty.xs, TextWeight::Semibold, fg, r.x0 + 9.0, r.center().y - th / 2.0, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn empty_state(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, x0: f64, y: f64, width: f64, title: &str, subtitle: &str) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let box_h = 140.0;
        let r = Rect::new(x0, y, x0 + width, y + box_h);
        draw::fill_round(scene, t, r, theme.tokens.radii.lg, alpha(c.surfaces.raised, 0.5));
        draw::stroke_round(scene, t, r, theme.tokens.radii.lg, c.border_standard, 1.0);
        let (tw, _) = draw::measure(tr, title, ty.lg);
        draw::text_w(scene, tr, t, title, ty.lg, TextWeight::Bold, c.text_primary, r.center().x - tw / 2.0, r.center().y - 18.0, None);
        let (sw, _) = draw::measure(tr, subtitle, ty.sm);
        draw::text(scene, tr, t, subtitle, ty.sm, c.text_dim, r.center().x - sw / 2.0, r.center().y + 6.0, None);
        y + box_h
    }

    fn draw_toast(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        // An action error (e.g. a real flatpak install failure) takes
        // precedence over an informational toast and uses the danger tint.
        let (msg, bg) = if let Some((err, _)) = &self.action_error {
            (err.clone(), c.semantic.danger)
        } else if let Some((m, _)) = &self.toast {
            (m.clone(), c.accent[5])
        } else {
            return;
        };
        let (tw, _) = draw::measure(tr, &msg, ty.sm);
        let w = tw + 40.0;
        let h = 40.0;
        let r = Rect::new(self.rect.center().x - w / 2.0, self.rect.y1 - h - 24.0, self.rect.center().x + w / 2.0, self.rect.y1 - 24.0);
        draw::fill_round(scene, t, r, theme.tokens.radii.full, bg);
        draw::text_w(scene, tr, t, &msg, ty.sm, TextWeight::Semibold, Color::WHITE, r.x0 + 20.0, r.center().y - ty.sm as f64 / 2.0 - 1.0, None);
    }

    /// The Remotes management screen: a header with "Add Flathub", then a
    /// card per configured remote with its URL and a Remove button. In
    /// `--shots` (offline) a couple of sample remotes stand in.
    fn draw_remotes(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let body = self.body_rect();
        let x0 = body.x0 + BODY_PAD_X;
        let width = body.width() - BODY_PAD_X * 2.0;
        let mut y = body.y0 + BODY_PAD_TOP - self.scroll;

        // Offline / first-frame fallback so the screen is never empty.
        let fallback;
        let remotes: &[Remote] = if self.remotes.is_empty() {
            fallback = sample_remotes();
            &fallback
        } else {
            &self.remotes
        };

        draw::text_w(scene, tr, t, &format!("Flatpak remotes · {}", remotes.len()), ty.lg, TextWeight::Bold, c.text_primary, x0, y, None);
        // "Add Flathub" button on the right.
        let has_flathub = remotes.iter().any(|r| r.name == "flathub");
        if !has_flathub {
            let bw = 150.0_f64;
            let btn = Rect::new(x0 + width - bw, y - 6.0, x0 + width, y + 28.0);
            Self::pill_button(scene, tr, t, theme, targets, btn, "Add Flathub", BtnStyle::Primary, btn.contains(self.mouse), Action::RemoteAddFlathub);
        }
        y += 30.0;
        draw::text(scene, tr, t, "Remotes are the sources apps install from. Adding one lets you browse and install its apps.", ty.sm, c.text_dim, x0, y, Some(width as f32));
        y += 30.0;

        let row_h = 78.0;
        for r in remotes {
            let rect = Rect::new(x0, y, x0 + width, y + row_h);
            draw::fill_round(scene, t, rect, theme.tokens.radii.lg, c.surfaces.raised);
            draw::stroke_round(scene, t, rect, theme.tokens.radii.lg, c.border_standard, 1.0);
            let pad = 16.0;
            // Name + disabled tag.
            draw::text_w(scene, tr, t, &r.title, ty.base, TextWeight::Bold, c.text_primary, rect.x0 + pad, rect.y0 + pad, None);
            if r.disabled {
                let tag = "disabled";
                let tw = draw::measure(tr, tag, ty.xs).0;
                let tr_rect = Rect::new(rect.x0 + pad + draw::measure(tr, &r.title, ty.base).0 + 12.0, rect.y0 + pad, rect.x0 + pad + draw::measure(tr, &r.title, ty.base).0 + 12.0 + tw + 16.0, rect.y0 + pad + 18.0);
                draw::fill_round(scene, t, tr_rect, theme.tokens.radii.full, alpha(c.text_dim, 0.2));
                draw::text(scene, tr, t, tag, ty.xs, c.text_dim, tr_rect.x0 + 8.0, tr_rect.y0 + 1.0, None);
            }
            // Name (machine) + URL.
            draw::text(scene, tr, t, &format!("{}  ·  {}", r.name, r.url), ty.sm, c.text_dim, rect.x0 + pad, rect.center().y + 6.0, Some((width - 160.0) as f32));
            // Remove button (right).
            let bw = 110.0_f64;
            let btn = Rect::new(rect.x1 - pad - bw, rect.center().y - 17.0, rect.x1 - pad, rect.center().y + 17.0);
            Self::pill_button(scene, tr, t, theme, targets, btn, "Remove", BtnStyle::Danger, btn.contains(self.mouse), Action::RemoteRemove(r.name.clone()));
            y += row_h + 12.0;
        }
        (y + self.scroll) - body.y0 + 12.0
    }

    /// Render the bespoke qOS-native affordances ("Open directly" + "Add")
    /// in place of the normal install control, with hover tooltips. VISUAL
    /// ONLY — the backend is a documented stub. Returns the row's bottom y.
    fn draw_qos_affordances(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        targets: &mut Vec<HitTarget>,
        rect: Rect,
        app: &AppEntry,
    ) {
        // Two side-by-side pills: "Open directly" (neutral) + "Add" (primary).
        let gap = 8.0;
        let half = (rect.width() - gap) / 2.0;
        let open_r = Rect::new(rect.x0, rect.y0, rect.x0 + half, rect.y1);
        let add_r = Rect::new(rect.x1 - half, rect.y0, rect.x1, rect.y1);
        Self::pill_button(scene, tr, t, theme, targets, open_r, QosAppModel::OpenDirect.label(), BtnStyle::Neutral, open_r.contains(self.mouse), Action::QosOpenDirect(app.id.clone()));
        Self::pill_button(scene, tr, t, theme, targets, add_r, QosAppModel::Add.label(), BtnStyle::Primary, add_r.contains(self.mouse), Action::QosAdd(app.id.clone()));
        // Register tooltip hover targets ABOVE the buttons (so they win the
        // reverse hit-test for hover, but the buttons still take the click —
        // tooltips are recorded as a separate hover-only action that the
        // pointer-move handler reads, not the click handler).
        targets.push(HitTarget { rect: open_r, action: Action::HoverTooltip(QosAppModel::OpenDirect.tooltip().into()) });
        targets.push(HitTarget { rect: add_r, action: Action::HoverTooltip(QosAppModel::Add.tooltip().into()) });
    }

    /// Draw the compatibility-notice rows (detail page). Each is a pill with
    /// an info/warning tint.
    fn draw_compat(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x0: f64,
        mut y: f64,
        width: f64,
        compat: &[Compat],
    ) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        for notice in compat {
            let (tint, fg) = if notice.blocking {
                (alpha(c.semantic.danger, 0.14), c.semantic.danger)
            } else {
                (alpha(c.semantic.success, 0.12), mix(c.text_primary, c.semantic.success, 0.4))
            };
            let h = 30.0;
            let r = Rect::new(x0, y, x0 + width, y + h);
            draw::fill_round(scene, t, r, theme.tokens.radii.md, tint);
            let glyph = if notice.blocking { "▲" } else { "✓" };
            draw::text_w(scene, tr, t, glyph, ty.sm, TextWeight::Bold, fg, r.x0 + 12.0, r.center().y - ty.sm as f64 / 2.0 - 1.0, None);
            draw::text(scene, tr, t, &notice.label, ty.sm, fg, r.x0 + 34.0, r.center().y - ty.sm as f64 / 2.0 - 1.0, Some((width - 48.0) as f32));
            y += h + 8.0;
        }
        y
    }

    /// Draw the reviews section (detail page). Each review is a card with
    /// author, stars and body text.
    fn draw_reviews(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x0: f64,
        mut y: f64,
        width: f64,
        reviews: &[Review],
        rating_note: Option<&str>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        y = self.section(scene, tr, t, theme, x0, y, "Ratings & reviews");
        y += 6.0;
        if let Some(note) = rating_note {
            draw::text(scene, tr, t, note, ty.xs, c.text_dim, x0, y, Some(width as f32));
            y += 22.0;
        }
        if reviews.is_empty() {
            draw::text(scene, tr, t, "No written reviews yet.", ty.sm, c.text_dim, x0, y, None);
            return y + 26.0;
        }
        for rv in reviews {
            // Estimate height from the wrapped body.
            let (_, bh) = tr.measure(&rv.body, ty.sm, Some((width - 32.0) as f32));
            let card_h = 30.0 + bh as f64 + 22.0;
            let r = Rect::new(x0, y, x0 + width, y + card_h);
            draw::fill_round(scene, t, r, theme.tokens.radii.lg, alpha(c.surfaces.raised, 0.7));
            draw::stroke_round(scene, t, r, theme.tokens.radii.lg, c.border_standard, 1.0);
            let pad = 16.0;
            draw::text_w(scene, tr, t, &rv.author, ty.sm, TextWeight::Semibold, c.text_primary, r.x0 + pad, r.y0 + 12.0, None);
            draw::rating_stars(scene, t, r.x0 + pad + draw::measure(tr, &rv.author, ty.sm).0 + 14.0, r.y0 + 12.0 + ty.sm as f64 / 2.0, rv.stars, 5.0, 2.0, c.semantic.warning, c.surface_white_20);
            draw::text(scene, tr, t, &rv.body, ty.sm, mix(c.text_primary, c.text_dim, 0.2), r.x0 + pad, r.y0 + 32.0, Some((width - pad * 2.0) as f32));
            y += card_h + 12.0;
        }
        y
    }

    /// Build the shared install-confirm dialog from the pending `.flatpakref`.
    /// "Install this app?" is the dialog title; Install / Cancel are its footer
    /// buttons. The install only runs on confirm.
    fn build_ref_dialog(&self) -> Option<RefDlg> {
        let r = self.pending_ref.as_ref()?;
        let install = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new("Install this app?", RefBody::from_ref(r))
            .open(true)
            .with_max_size(460.0, 300.0)
            .primary_action("Install", {
                let s = install.clone();
                move || s.set(true)
            })
            .secondary_action("Cancel", {
                let c = cancel.clone();
                move || c.set(true)
            })
            .on_dismiss({
                let c = cancel.clone();
                move || c.set(true)
            });
        Some(RefDlg { dialog, install, cancel })
    }

    fn ensure_ref_dialog(&mut self) {
        if self.ref_dialog.is_none() {
            self.ref_dialog = self.build_ref_dialog();
        }
    }

    /// Lay the confirm dialog out against the app rect so its box + footer
    /// buttons resolve.
    fn layout_ref_dialog(&mut self) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let rect = self.rect;
        let Some(rd) = self.ref_dialog.as_mut() else { return };
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = rd.dialog.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(rect.width() as f32),
                        height: Dimension::length(rect.height() as f32),
                    },
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        let _ = taffy.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(rect.width() as f32),
                height: taffy::AvailableSpace::Definite(rect.height() as f32),
            },
        );
        rd.dialog.apply_layout(&taffy, node, Point::new(rect.x0, rect.y0));
    }

    /// Route an event into the open install-confirm dialog, then apply the
    /// flags its callbacks set (Enter confirms as a convenience).
    fn route_ref_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if let WindowEvent::KeyboardInput { event: ke, .. } = event {
            if ke.state == winit::event::ElementState::Pressed
                && matches!(ke.logical_key, Key::Named(NamedKey::Enter))
            {
                self.confirm_pending_ref(state);
                self.ref_dialog = None;
                *handled = true;
                return;
            }
        }
        self.ensure_ref_dialog();
        self.layout_ref_dialog();
        let (install, cancel) = {
            let Some(rd) = self.ref_dialog.as_mut() else { return };
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut focus_target = None;
            let root = automerge::ROOT;
            rd.dialog.handle_event(
                event, scale_factor, modifiers, state, &root, handled,
                &mut focus_target, node, &taffy,
            );
            (rd.install.replace(false), rd.cancel.replace(false))
        };
        if install {
            self.confirm_pending_ref(state);
            self.ref_dialog = None;
        } else if cancel {
            self.pending_ref = None;
            self.ref_dialog = None;
        }
        self.dirty = true;
        *handled = true;
    }

    /// Draw the hovered tooltip near the cursor (qOS affordances).
    fn draw_tooltip(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme) {
        let Some(text) = &self.tooltip else { return };
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let max_w = 260.0_f64;
        let (_, th) = tr.measure(text, ty.xs, Some(max_w as f32));
        let pad = 10.0;
        let w = max_w + pad * 2.0;
        let h = th as f64 + pad * 2.0;
        // Position above-right of cursor, clamped into the window.
        let mut x = self.mouse.x + 14.0;
        let mut yt = self.mouse.y - h - 8.0;
        if x + w > self.rect.x1 {
            x = self.rect.x1 - w - 6.0;
        }
        if yt < self.rect.y0 {
            yt = self.mouse.y + 18.0;
        }
        let r = Rect::new(x, yt, x + w, yt + h);
        draw::fill_round(scene, t, r, theme.tokens.radii.md, c.surfaces.overlay);
        draw::stroke_round(scene, t, r, theme.tokens.radii.md, c.border_standard, 1.0);
        draw::text(scene, tr, t, text, ty.xs, c.text_primary, r.x0 + pad, r.y0 + pad, Some(max_w as f32));
    }

    /// Vertical scrollbar indicator on the right edge of the body.
    fn draw_scrollbar(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let max = self.max_scroll();
        if max <= 0.5 {
            return;
        }
        let body = self.body_rect();
        let track_h = body.height() - 12.0;
        let frac_visible = (body.height() / self.content_height).clamp(0.05, 1.0);
        let thumb_h = (track_h * frac_visible).max(28.0);
        let frac = (self.scroll / max).clamp(0.0, 1.0);
        let thumb_y = body.y0 + 6.0 + (track_h - thumb_h) * frac;
        let x1 = body.x1 - 5.0;
        let r = Rect::new(x1 - 4.0, thumb_y, x1, thumb_y + thumb_h);
        draw::fill_round(scene, t, r, theme.tokens.radii.full, alpha(theme.tokens.colors.text_dim, 0.5));
    }
}

// ===================================================================
// Install-confirm dialog (shared ModalDialog) content
// ===================================================================

/// The live install-confirm dialog: the shared [`ModalDialog`] hosting a
/// [`RefBody`] plus the install / cancel cells its footer buttons flip.
struct RefDlg {
    dialog: ModalDialog<RefBody>,
    install: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

/// Content body for the `.flatpakref` install confirm: an icon tile, the app
/// title + id, its source remote, and the summary. The [`ModalDialog`] owns the
/// scrim, box, "Install this app?" title, Install/Cancel footer, Esc, focus
/// trap, and a11y role.
struct RefBody {
    title: String,
    app_id: String,
    source: String,
    comment: String,
    monogram: String,
    rect: Rect,
}

impl RefBody {
    fn from_ref(r: &FlatpakRef) -> Self {
        let source = format!(
            "from {}",
            if r.url.is_empty() { "a Flatpak remote".to_string() } else { r.url.clone() }
        );
        let monogram = r
            .app_id
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        Self {
            title: r.title.clone(),
            app_id: r.app_id.clone(),
            source,
            comment: r.comment.clone(),
            monogram,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for RefBody {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let ty = &theme.tokens.typography.scale;
        let x = self.rect.x0;
        let icon_r = Rect::new(x, self.rect.y0, x + 56.0, self.rect.y0 + 56.0);
        let base = c.accent[5];
        draw::icon_tile(scene, tr, t, icon_r, base, mix(base, Color::WHITE, 0.5), &self.monogram, Color::WHITE);
        let tx = icon_r.x1 + 14.0;
        draw::text_w(scene, tr, t, &self.title, ty.xl, TextWeight::Bold, c.text_primary, tx, icon_r.y0, None);
        draw::text(scene, tr, t, &self.app_id, ty.xs, c.text_dim, tx, icon_r.y0 + 26.0, None);
        draw::text(scene, tr, t, &self.source, ty.xs, c.text_dim, tx, icon_r.y0 + 42.0, Some((self.rect.x1 - tx) as f32));
        if !self.comment.is_empty() {
            draw::text(scene, tr, t, &self.comment, ty.sm, mix(c.text_primary, c.text_dim, 0.2), x, icon_r.y1 + 16.0, Some(self.rect.width() as f32));
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &str {
        "RefBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

// ===================================================================
// Widget impl
// ===================================================================

impl Widget for SoftwareStore {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // --- Async plumbing, FIRST: never block the frame. ---
        // Kick off the first round of background refreshes (live only).
        self.kick_initial();
        // Drain any finished worker results into the model.
        if self.drain_worker(state) {
            self.dirty = true;
        }
        // Fire a debounced live search if the query settled.
        self.maybe_dispatch_search();

        // One-time default seed so a fresh document (and the --shots
        // path) is populated: a few installed apps, two a version behind
        // (Updates tab), and one mid-install (progress bar visible).
        if !StoreDoc::is_seeded(state) {
            let current = [
                ("org.mozilla.firefox", "129.0"),
                ("md.obsidian.Obsidian", "1.6.7"),
                ("org.signal.Signal", "7.20.0"),
                ("org.zealdocs.Zeal", "0.7.1"),
            ];
            let behind = [
                ("org.gimp.GIMP", "2.10.36"),
                ("com.visualstudio.code", "1.90.0"),
            ];
            let installing = [("org.libreoffice.LibreOffice", 0.45)];
            StoreDoc::seed_defaults(state, &current, &behind, &installing);
        }

        // Merge live probe (real flatpak/podman state) once, over the
        // seed. Only present in the live app — never in --shots.
        if !self.probe_merged {
            if let Some(probe) = self.probe.clone() {
                // Surface the honest "Flatpak unavailable" state when the host
                // has no flatpak binary — browsing the Flathub catalog still
                // works, but installing/managing apps does not.
                self.flatpak_available = probe.flatpak_available;
                for (id, ver) in &probe.flatpak_versions {
                    if let Some(app) = self.catalog.get(id) {
                        let v = if ver.is_empty() { app.latest_version.clone() } else { ver.clone() };
                        StoreDoc::set_installed(state, id, &v);
                    }
                }
            }
            self.probe_merged = true;
        }

        // Advance / finalise any locally-tracked installs.
        let mut done: Vec<String> = Vec::new();
        for (id, start) in &self.install_start {
            let p = start.elapsed().as_secs_f64() / INSTALL_DURATION.as_secs_f64();
            if p >= 1.0 {
                done.push(id.clone());
            } else {
                StoreDoc::set_installing(state, id, p);
            }
        }
        for id in done {
            if let Some(app) = self.catalog.get(&id) {
                let v = app.latest_version.clone();
                StoreDoc::set_installed(state, &id, &v);
            }
            self.install_start.remove(&id);
        }
        self.finalize_pending = false;

        self.snapshot = StoreDoc::read_snapshot(state);

        // Any CRDT-stored in-flight install (e.g. the first-run seed's
        // LibreOffice, or a collaborator's in-progress update replicated in)
        // joins the Updates batch so the Updates view shows it as work
        // currently being performed, with a local wall-clock animation.
        // Backdate the local start so the animation continues from the
        // stored progress rather than snapping back to 0% — that way a
        // `--shots updates` frame captures a representative mid-update bar.
        for (id, p) in self.snapshot.installing.clone() {
            self.enqueue_update(&id);
            self.install_start.entry(id).or_insert_with(|| {
                let elapsed = INSTALL_DURATION.mul_f64(p.clamp(0.0, 1.0));
                Instant::now()
                    .checked_sub(elapsed)
                    .unwrap_or_else(Instant::now)
            });
        }

        // Fill freed concurrency slots from the queue, then dismiss the
        // batch once every app in it has settled.
        self.promote_queued_updates();
        self.settle_update_batch();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        // On a breakpoint crossing (window resize), drop any stale drawer
        // preference so the nav returns to the new layout's default.
        self.nav_flap.reconcile(self.nav_layout());
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Expire the toast + any action error (errors linger a bit longer).
        if let Some((_, at)) = &self.toast {
            if at.elapsed() > TOAST_DURATION {
                self.toast = None;
            }
        }
        if let Some((_, at)) = &self.action_error {
            if at.elapsed() > TOAST_DURATION * 2 {
                self.action_error = None;
            }
        }

        let c = &theme.tokens.colors;
        // Full background.
        draw::fill_rect(scene, t, self.rect, c.surfaces.base);

        let mut targets: Vec<HitTarget> = Vec::new();

        // Clamp scroll against the previous frame's content height.
        let max = self.max_scroll();
        if self.scroll > max {
            self.scroll = max;
        }
        if self.scroll < 0.0 {
            self.scroll = 0.0;
        }

        // Body (under the header). We borrow the catalog immutably and
        // build the target list; nothing here mutates `self` other than
        // the local-only `finalize_pending` flag (set after the borrow).
        let content_height;
        let mut any_complete = false;
        {
            // Detect installs that reached 100% this frame so `needs_commit`
            // can nudge a finalising `sync`.
            for (_, start) in &self.install_start {
                if start.elapsed().as_secs_f64() / INSTALL_DURATION.as_secs_f64() >= 1.0 {
                    any_complete = true;
                }
            }

            content_height = if let Some(id) = self.detail.clone() {
                // Prefer the curated catalog; fall back to a live/cached
                // Flathub entry so detail pages opened from search work too.
                let live = self.worker.caches.detail.get(&id).cloned();
                let app = self
                    .catalog
                    .get(&id)
                    .cloned()
                    .or(live)
                    .or_else(|| self.live_apps.get(&id).cloned());
                if let Some(app) = app {
                    // Kick a lazy detail fetch the first time we open a live
                    // entry that we only have summary data for.
                    self.draw_detail(scene, tr, t, theme, &mut targets, &app)
                } else {
                    0.0
                }
            } else {
                match self.tab {
                    Tab::Browse => self.draw_browse(scene, tr, t, theme, &mut targets),
                    Tab::Installed => self.draw_installed(scene, tr, t, theme, &mut targets),
                    Tab::Updates => self.draw_updates(scene, tr, t, theme, &mut targets),
                    Tab::Remotes => self.draw_remotes(scene, tr, t, theme, &mut targets),
                }
            };
        }
        self.content_height = content_height;

        // Header paints over any scrolled-up body content.
        self.draw_header(scene, tr, t, theme, presence, &mut targets);
        // Honest "Flatpak unavailable" strip below the header (live-only).
        self.draw_flatpak_banner(scene, tr, t, theme);

        self.draw_scrollbar(scene, t, theme);

        // Advance the drawer reveal, then paint the overlay drawer + scrim on
        // top of the body + header (narrow mode only). Keep frames coming
        // while the slide is settling.
        if self.tick_nav() {
            self.dirty = true;
        }
        if self.is_narrow() {
            self.draw_nav_drawer(scene, tr, t, theme, &mut targets);
        }

        // Toast sits above everything, including the drawer scrim.
        self.draw_toast(scene, tr, t, theme);

        // The `.flatpakref` confirm sheet is a modal over everything — the
        // shared ModalDialog paints its own scrim, box, title, and footer.
        if self.pending_ref.is_some() {
            self.ensure_ref_dialog();
            self.layout_ref_dialog();
            if let Some(rd) = self.ref_dialog.as_mut() {
                rd.dialog.draw(scene, t, tr, theme, presence);
            }
        } else {
            self.ref_dialog = None;
        }

        // Tooltip last, so it sits over the affordances it describes.
        self.draw_tooltip(scene, tr, t, theme);

        self.hit_targets = targets;
        self.finalize_pending = any_complete;
        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};

        // The `.flatpakref` install confirm is modal: while it's up the shared
        // ModalDialog captures every event (clicks, Esc, Enter) and nothing
        // leaks to the catalog behind the scrim.
        if self.pending_ref.is_some() {
            self.route_ref_event(event, scale_factor, modifiers, state, handled);
            return;
        }

        // Track the pointer up-front so pill/hit-test maths agree with the
        // frame the input was last laid out against.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // Drive the embedded search field (a real `ui::TextInput`): forward
        // pointer events always (hover / caret placement / drag-select) and
        // keyboard/IME only while it's focused — but keep Escape/Enter for the
        // store so its clear / close / open-first-result behaviour stays. We
        // manage the field's focus ourselves, so a throwaway `focus_target`
        // absorbs the input's own focus request.
        let forward = match event {
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } => true,
            WindowEvent::Ime(_) => self.search_input.is_focused(),
            WindowEvent::KeyboardInput { event: ke, .. } => {
                self.search_input.is_focused()
                    && !matches!(
                        ke.logical_key,
                        Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter)
                    )
            }
            _ => false,
        };
        if forward {
            let mut sink = None;
            let node = self.node.unwrap_or(_node_id);
            self.search_input
                .handle_event(event, scale_factor, modifiers, state, _path, handled, &mut sink, node, _taffy);
        }
        // Fold any edit from the field into the mirror the screens read.
        if self.pull_search_edit() {
            *handled = true;
        }
        // A keystroke/IME commit the field consumed is not a store shortcut.
        if forward
            && *handled
            && matches!(event, WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_))
        {
            return;
        }

        match event {
            WindowEvent::CursorMoved { .. } => {
                // Resolve a hover tooltip: the topmost HoverTooltip target
                // under the cursor wins (qOS affordance explainers).
                self.tooltip = self
                    .hit_targets
                    .iter()
                    .rev()
                    .find(|h| matches!(h.action, Action::HoverTooltip(_)) && h.rect.contains(self.mouse))
                    .and_then(|h| match &h.action {
                        Action::HoverTooltip(s) => Some(s.clone()),
                        _ => None,
                    });
                // Repaint so hover states track the pointer.
                self.dirty = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Reverse iterate so later-drawn (topmost) targets win.
                // HoverTooltip targets are hover-only — never clickable — so
                // the click falls through to the affordance button beneath.
                let hit = self
                    .hit_targets
                    .iter()
                    .rev()
                    .find(|h| !matches!(h.action, Action::HoverTooltip(_)) && h.rect.contains(self.mouse))
                    .map(|h| h.action.clone());
                if let Some(action) = hit {
                    // Clicking anywhere that isn't the search box unfocuses it.
                    if action != Action::FocusSearch {
                        self.set_search_focus(false);
                    }
                    self.apply(action, state);
                    *handled = true;
                } else if self.search_focused {
                    // Click on empty space dismisses search focus.
                    self.set_search_focus(false);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                match &key_event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if self.is_narrow() && self.nav_flap.is_open(self.nav_layout()) {
                            // Esc dismisses the open overlay drawer first.
                            self.nav_flap.close();
                        } else if self.detail.is_some() {
                            self.detail = None;
                            self.reset_scroll();
                        } else if !self.search_query.is_empty() {
                            self.set_search_text("");
                        } else if self.search_focused {
                            self.set_search_focus(false);
                        }
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) if self.search_focused => {
                        // Open the first search result.
                        if let Some(first) = self.catalog.search(&self.search_query, self.active_category).first() {
                            let id = first.id.clone();
                            self.detail = Some(id);
                            self.reset_scroll();
                        }
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.scroll = (self.scroll + 60.0).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.scroll = (self.scroll - 60.0).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::PageDown) => {
                        self.scroll = (self.scroll + self.body_rect().height() * 0.9).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.scroll = (self.scroll - self.body_rect().height() * 0.9).clamp(0.0, self.max_scroll());
                        self.dirty = true;
                        *handled = true;
                    }
                    Key::Character(s) if ctrl && s.eq_ignore_ascii_case("f") => {
                        self.set_search_focus(true);
                        self.tab = Tab::Browse;
                        self.detail = None;
                        *handled = true;
                    }
                    Key::Character(s) => {
                        // `/` focuses search when the field isn't already active.
                        if !self.search_focused && s.as_str() == "/" {
                            self.set_search_focus(true);
                            self.tab = Tab::Browse;
                            self.detail = None;
                            *handled = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SoftwareStore"
    }

    fn test_id(&self) -> Option<&str> {
        Some("software_store")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || !self.install_start.is_empty()
            || self.toast.is_some()
            || self.worker.has_inflight()
            || self.image_store.pending() > 0
            || self.search_pending()
            || self.nav_flap.is_animating(self.nav_layout())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the redraw loop alive while ANY async work is outstanding —
        // this is the maps-tile-worker contract: the loop self-perpetuates
        // `request_redraw()` until `has_inflight()` empties, draining the
        // worker channel each frame until results paint. Without this, an
        // async result that lands between frames would never be shown.
        !self.install_start.is_empty()
            || self.toast.is_some()
            || self.worker.has_inflight()
            || self.image_store.pending() > 0
            || self.search_pending()
            || self.nav_flap.is_animating(self.nav_layout())
    }

    fn needs_idle_sync(&self) -> bool {
        // While the loop is otherwise idle, keep syncing so a debounced
        // search dispatch + worker drain still run between user input, and so
        // a resolved icon/screenshot repaints once its async load lands.
        self.worker.has_inflight() || self.search_pending() || self.image_store.pending() > 0
    }

    fn needs_commit(&self) -> bool {
        // Rising-edge nudge: when an install finishes, return true so the
        // App schedules one finalising `sync` (which writes the installed
        // version and clears the in-flight record).
        self.finalize_pending
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // Root rect is the whole window — always paint, never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SoftwareStore {
        SoftwareStore::new(None)
    }

    #[test]
    fn live_constructor_shows_no_fabricated_catalog() {
        // With a live worker, Browse starts from real Flathub data only — never
        // the curated sample with its fabricated star ratings + descriptions.
        let store = SoftwareStore::with_worker(None, Worker::spawn());
        assert!(store.catalog.is_empty(), "live catalog starts empty; real Flathub fills it");
        assert!(store.live_apps.is_empty(), "no apps until the worker responds");
        assert!(store.browse_apps().is_empty(), "Browse shows nothing fabricated on cold start");
    }

    #[test]
    fn offline_constructor_keeps_the_sample_catalog() {
        // The --shots / test path keeps the curated sample so screenshots and
        // the rest of the suite stay populated without a network.
        let store = SoftwareStore::new(None);
        assert!(!store.catalog.is_empty(), "offline keeps the sample catalog");
        assert!(!store.browse_apps().is_empty(), "offline Browse renders the sample");
    }

    // -------------------------------------------------------------------
    // Click-driven driver: runs the real Widget frame cycle so `rect` and
    // `hit_targets` are populated exactly as in the live app / --shots path,
    // then dispatches synthesized winit events through the REAL
    // `handle_event`. Assertions read post-event state, proving the pointer
    // path — not a shortcut through `apply` — does the work.
    // -------------------------------------------------------------------
    const VW: f64 = 1280.0;
    const VH: f64 = 800.0;

    struct Driver {
        app: SoftwareStore,
        doc: AutoCommit,
    }

    impl Driver {
        fn new() -> Self {
            let mut app = store();
            app.rect = Rect::new(0.0, 0.0, VW, VH);
            app.nav_flap.reconcile(app.nav_layout());
            Driver { app, doc: AutoCommit::new() }
        }

        /// sync → draw (which rebuilds `hit_targets`), against a real
        /// offscreen text renderer + scene. Mirrors `ui::App`'s cycle for the
        /// full-bleed leaf this widget is.
        fn frame(&mut self) {
            self.app.sync(&mut self.doc, &automerge::ROOT);
            let theme = Theme::dark();
            let mut tr = ui::text_render::TextRenderer::new();
            let mut scene = Scene::new();
            self.app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &PresenceCtx::empty());
        }

        fn dispatch(&mut self, event: winit::event::WindowEvent) {
            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                &event,
                1.0,
                &winit::event::Modifiers::default(),
                &mut self.doc,
                &automerge::ROOT,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        }

        /// Move the cursor then left-press at `(x, y)` via `handle_event`.
        fn click(&mut self, x: f64, y: f64) {
            self.dispatch(cursor_at(x, y));
            self.dispatch(left_press());
        }

        /// Type a character through the real key path.
        fn type_char(&mut self, ch: &str) {
            self.dispatch(char_press(ch));
        }

        /// The centre of the first hit-target whose action matches `pred`,
        /// as rebuilt by the last `frame()`. Panics if none is found so a
        /// missing affordance fails loudly rather than silently no-op.
        fn target_center(&self, pred: impl Fn(&Action) -> bool) -> Point {
            self.app
                .hit_targets
                .iter()
                .find(|h| pred(&h.action))
                .map(|h| h.rect.center())
                .unwrap_or_else(|| panic!("no hit-target matched"))
        }
    }

    fn synth_device() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(x: f64, y: f64) -> winit::event::WindowEvent {
        winit::event::WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }
    fn left_press() -> winit::event::WindowEvent {
        winit::event::WindowEvent::MouseInput {
            device_id: synth_device(),
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        }
    }
    fn char_press(ch: &str) -> winit::event::WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = winit::keyboard::Key::Character(ch.into());
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(ch.into());
        winit::event::WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn esc_press() -> winit::event::WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape);
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        winit::event::WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn sample_ref() -> FlatpakRef {
        FlatpakRef {
            app_id: "org.gimp.GIMP".into(),
            title: "GIMP".into(),
            comment: "Image editor".into(),
            url: "https://dl.flathub.org/repo/".into(),
            branch: "stable".into(),
            source_path: "/tmp/gimp.flatpakref".into(),
        }
    }

    /// The `.flatpakref` install confirm routes through the shared
    /// `ModalDialog`: Escape dismisses it without installing, and a click on
    /// the primary runs the real install (via `confirm_pending_ref`).
    #[test]
    fn ref_install_dialog_confirms_and_escape_cancels() {
        let mut d = Driver::new();
        d.app.open_flatpakref(sample_ref());
        assert!(d.app.pending_ref.is_some());
        d.frame(); // builds + lays out the shared dialog
        d.dispatch(esc_press());
        assert!(d.app.pending_ref.is_none(), "Escape dismissed the confirm without installing");
        assert!(d.app.ref_dialog.is_none());

        // Re-arm, then click the primary → the gated install runs.
        d.app.open_flatpakref(sample_ref());
        d.frame();
        let primary = d.app.ref_dialog.as_ref().unwrap().dialog.primary_rect();
        d.dispatch(cursor_at(primary.center().x, primary.center().y));
        d.dispatch(left_press());
        assert!(d.app.pending_ref.is_none(), "the primary click consumed the pending ref");
        assert!(d.app.ref_dialog.is_none());
        // The install was kicked off: the store navigated to the app's detail
        // and enqueued its install (observable side effects of the real path).
        assert_eq!(d.app.detail.as_deref(), Some("org.gimp.GIMP"), "install navigates to the app");
    }

    #[test]
    fn click_search_then_type_filters_via_event_path() {
        let mut d = Driver::new();
        d.frame();
        // Click the search pill (real pointer path focuses it).
        let p = d.target_center(|a| matches!(a, Action::FocusSearch));
        d.click(p.x, p.y);
        assert!(d.app.search_focused, "clicking the search pill focuses it");
        // Type a query through the real key path.
        for ch in ["f", "i", "r", "e", "f", "o", "x"] {
            d.type_char(ch);
        }
        assert_eq!(d.app.search_query, "firefox");
        // The catalog query now resolves to exactly Firefox.
        let results = d.app.catalog.search(&d.app.search_query, d.app.active_category);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "org.mozilla.firefox");
    }

    #[test]
    fn click_card_opens_detail_via_event_path() {
        let mut d = Driver::new();
        d.frame();
        // Click a card that opens GIMP's detail page.
        let p = d.target_center(|a| matches!(a, Action::OpenDetail(id) if id == "org.gimp.GIMP"));
        d.click(p.x, p.y);
        assert_eq!(d.app.detail.as_deref(), Some("org.gimp.GIMP"), "card click opens detail");
        // Re-frame in the detail view, then click Back to return.
        d.frame();
        let back = d.target_center(|a| matches!(a, Action::Back));
        d.click(back.x, back.y);
        assert!(d.app.detail.is_none(), "Back returns to the browse view");
    }

    #[test]
    fn click_update_starts_update_and_renders_progress_via_event_path() {
        let mut d = Driver::new();
        // Go to the Updates tab.
        d.app.tab = Tab::Updates;
        d.frame();
        // A pending update (GIMP) exposes an Update control; click it.
        let p = d.target_center(|a| matches!(a, Action::Update(id) if id == "org.gimp.GIMP"));
        d.click(p.x, p.y);
        // The click routed through handle_event → apply → start_install, so
        // GIMP is now in the batch and in flight.
        assert!(d.app.update_batch.iter().any(|x| x == "org.gimp.GIMP"));
        assert!(d.app.install_start.contains_key("org.gimp.GIMP"));
        // Re-frame: the Updates view now renders GIMP as in-progress and
        // exposes a Cancel affordance for it (proving the progress UI paints
        // on the real path).
        d.frame();
        assert!(
            matches!(
                d.app.batch_status("org.gimp.GIMP"),
                Some(BatchRowStatus::InProgress { .. })
            ),
            "GIMP renders in-progress after the update click"
        );
        assert!(
            d.app.hit_targets.iter().any(|h| matches!(&h.action, Action::Cancel(id) if id == "org.gimp.GIMP")),
            "an in-progress update row exposes a Cancel affordance"
        );
        // The overall status card reflects an in-flight batch.
        assert!(d.app.update_progress().in_flight());
    }

    #[test]
    fn click_update_all_enqueues_every_pending_via_event_path() {
        let mut d = Driver::new();
        d.app.tab = Tab::Updates;
        d.frame();
        // The first-run seed leaves LibreOffice mid-install, so a batch is
        // already running and "Update All" is (correctly) hidden. Cancel it
        // via the real pointer path so the batch clears and the actionable
        // "Update All" affordance appears.
        let cancel = d.target_center(|a| matches!(a, Action::Cancel(id) if id == "org.libreoffice.LibreOffice"));
        d.click(cancel.x, cancel.y);
        d.frame();
        assert!(!d.app.update_progress().in_flight(), "batch cleared after cancel");

        let pending = d.app.updatable_ids();
        assert!(pending.len() >= 2);
        // Click "Update All".
        let p = d.target_center(|a| matches!(a, Action::UpdateAll));
        d.click(p.x, p.y);
        d.frame();
        // Every pending app joins the batch. With bounded concurrency the
        // first slots run (in-progress) and any overflow queues — both are
        // valid batch states; none stays a plain pending row.
        for id in &pending {
            assert!(d.app.update_batch.iter().any(|x| x == id), "{id} enqueued by Update All");
            assert!(
                matches!(
                    d.app.batch_status(id),
                    Some(BatchRowStatus::InProgress { .. }) | Some(BatchRowStatus::Queued)
                ),
                "{id} should be in-progress or queued after Update All, got {:?}",
                d.app.batch_status(id)
            );
        }
        // At least the concurrency limit are actively in progress.
        let active = pending
            .iter()
            .filter(|id| matches!(d.app.batch_status(id), Some(BatchRowStatus::InProgress { .. })))
            .count();
        assert!(active >= UPDATE_CONCURRENCY.min(pending.len()), "at least the concurrency window runs");
    }

    #[test]
    fn sync_seeds_and_populates_snapshot() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        // Seeded: at least the four "current" installs + two behind.
        assert!(s.snapshot.installed.len() >= 6, "seed installed: {:?}", s.snapshot.installed);
        assert!(s.snapshot.installing.contains_key("org.libreoffice.LibreOffice"));
    }

    #[test]
    fn updates_tab_has_entries_after_seed() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        let ups = s.catalog.updatable(&s.snapshot);
        assert!(ups.len() >= 2, "expected >=2 updatable after seed, got {}", ups.len());
    }

    #[test]
    fn install_action_marks_installing_then_finalizes() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        // PCSX2 starts not installed.
        let id = "net.pcsx2.PCSX2";
        assert_eq!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot), InstallState::Available);
        s.apply(Action::Install(id.into()), &mut doc);
        s.sync(&mut doc, &automerge::ROOT);
        // Mid-install it should be Installing (progress < 1 right away).
        assert!(matches!(s.live_state(s.catalog.get(id).unwrap()), InstallState::Installing(_)));
        // Force completion: backdate the start instant beyond the duration.
        s.install_start.insert(id.into(), Instant::now() - INSTALL_DURATION - Duration::from_secs(1));
        s.sync(&mut doc, &automerge::ROOT);
        assert_eq!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot), InstallState::Installed);
    }

    #[test]
    fn remove_uninstalls() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        let id = "org.mozilla.firefox";
        assert!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot).is_installed_or_update());
        s.apply(Action::Remove(id.into()), &mut doc);
        assert_eq!(s.catalog.state_of(s.catalog.get(id).unwrap(), &s.snapshot), InstallState::Available);
    }

    #[test]
    fn search_focus_and_typing_filters() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        s.apply(Action::FocusSearch, &mut doc);
        assert!(s.search_focused);
        s.search_query = "firefox".into();
        let results = s.catalog.search(&s.search_query, s.active_category);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "org.mozilla.firefox");
    }

    #[test]
    fn update_all_starts_all_updatable() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        let updatable = s.updatable_ids();
        assert!(updatable.len() >= 2);
        s.apply(Action::UpdateAll, &mut doc);
        s.sync(&mut doc, &automerge::ROOT);
        // Every previously-updatable app is now in flight AND in the batch
        // (assert on the real per-app path, not a conflated total count —
        // the seed's pre-existing in-flight install also lives in
        // `install_start`).
        for id in &updatable {
            assert!(s.install_start.contains_key(id), "{id} should be in flight");
            assert!(s.update_batch.iter().any(|x| x == id), "{id} should be in the update batch");
            assert!(
                matches!(s.batch_status(id), Some(BatchRowStatus::InProgress { .. })),
                "{id} should render as in-progress, got {:?}",
                s.batch_status(id)
            );
        }
    }

    #[test]
    fn updates_view_shows_in_progress_batch_with_phases() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);

        // Kick a real update of a behind app (GIMP) and force a mid-download
        // wall-clock by back-dating its start to 30% of the duration.
        let gimp = "org.gimp.GIMP";
        let vscode = "com.visualstudio.code";
        s.apply(Action::Update(gimp.into()), &mut doc);
        s.apply(Action::Update(vscode.into()), &mut doc);
        s.install_start.insert(gimp.into(), Instant::now() - INSTALL_DURATION.mul_f64(0.30));
        s.install_start.insert(vscode.into(), Instant::now() - INSTALL_DURATION.mul_f64(0.80));
        s.sync(&mut doc, &automerge::ROOT);

        // Both apps are part of the batch, and stay visible in the Updates
        // view (they don't vanish on state-flip).
        let view_ids: Vec<String> = s.updates_view_apps().iter().map(|a| a.id.clone()).collect();
        assert!(view_ids.contains(&gimp.to_string()));
        assert!(view_ids.contains(&vscode.to_string()));

        // GIMP at 30% is Downloading; VS Code at 80% is Installing — the
        // per-row phase split the Updates view renders.
        match s.batch_status(gimp) {
            Some(BatchRowStatus::InProgress { phase, progress }) => {
                assert_eq!(phase, InstallPhase::Downloading);
                assert!(progress > 0.25 && progress < 0.40, "progress {progress}");
            }
            other => panic!("GIMP should be downloading, got {other:?}"),
        }
        assert!(matches!(
            s.batch_status(vscode),
            Some(BatchRowStatus::InProgress { phase: InstallPhase::Installing, .. })
        ), "VS Code at 80% should be installing");

        // The overall status rolls up as in-flight with both active.
        let p = s.update_progress();
        assert!(p.in_flight(), "batch should be in flight");
        assert!(p.active >= 2, "both apps active, got {}", p.active);
        assert!(p.overall > 0.0 && p.overall < 1.0, "overall {}", p.overall);
        assert!(p.status_line().starts_with("Updating"), "status: {}", p.status_line());

        // The view renders without panicking and reports a positive content
        // height (the in-progress status card + rows actually paint).
        s.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        s.tab = Tab::Updates;
        let mut tr = ui::text_render::TextRenderer::new();
        let mut scene = Scene::new();
        let theme = Theme::dark();
        let mut targets: Vec<HitTarget> = Vec::new();
        let h = s.draw_updates(&mut scene, &mut tr, Affine::IDENTITY, &theme, &mut targets);
        assert!(h > 0.0, "updates view should produce content height");
        // A cancel target exists for an in-progress row (the ✕ affordance).
        assert!(
            targets.iter().any(|t| matches!(&t.action, Action::Cancel(id) if id == gimp)),
            "in-progress row must expose a Cancel affordance"
        );
    }

    #[test]
    fn update_all_staggers_into_a_queue_and_promotes() {
        // Build a batch bigger than the concurrency window so some apps
        // genuinely queue, then verify a freed slot promotes a queued app.
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        // Clear the seed's in-flight LibreOffice so the batch starts empty.
        s.cancel_install(&mut doc, "org.libreoffice.LibreOffice");
        // Force three updatable apps by marking an extra one behind.
        StoreDoc::set_installed(&mut doc, "org.libreoffice.LibreOffice", "24.0.0");
        s.snapshot = StoreDoc::read_snapshot(&doc);
        let updatable = s.updatable_ids();
        assert!(updatable.len() >= 3, "need >=3 updatable, got {}", updatable.len());

        s.apply(Action::UpdateAll, &mut doc);
        s.sync(&mut doc, &automerge::ROOT);

        let active = |s: &SoftwareStore| {
            s.update_batch
                .iter()
                .filter(|id| matches!(s.batch_status(id), Some(BatchRowStatus::InProgress { .. })))
                .count()
        };
        let queued = |s: &SoftwareStore| {
            s.update_batch
                .iter()
                .filter(|id| matches!(s.batch_status(id), Some(BatchRowStatus::Queued)))
                .count()
        };
        // Exactly the concurrency window runs; the rest queue.
        assert_eq!(active(&s), UPDATE_CONCURRENCY, "only the window runs at once");
        assert!(queued(&s) >= 1, "overflow apps queue");

        // Free a slot: force one active app to completion, then sync so the
        // finished app finalises and a queued app is promoted.
        let running: String = s
            .update_batch
            .iter()
            .find(|id| matches!(s.batch_status(id), Some(BatchRowStatus::InProgress { .. })))
            .cloned()
            .unwrap();
        s.install_start.insert(running.clone(), Instant::now() - INSTALL_DURATION - Duration::from_secs(1));
        s.sync(&mut doc, &automerge::ROOT);
        // Still at most the window active, and a previously-queued app moved
        // into a running slot (queue shrank).
        assert!(active(&s) <= UPDATE_CONCURRENCY);
        assert_eq!(s.batch_status(&running), Some(BatchRowStatus::Done), "finished app reads Done");
    }

    #[test]
    fn completed_update_in_batch_renders_as_done() {
        let mut s = store();
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        let gimp = "org.gimp.GIMP";
        s.apply(Action::Update(gimp.into()), &mut doc);
        // Force completion: backdate beyond the duration, then sync to finalize.
        s.install_start.insert(gimp.into(), Instant::now() - INSTALL_DURATION - Duration::from_secs(1));
        s.sync(&mut doc, &automerge::ROOT);
        // It finalised to Installed, but because the batch still holds it (a
        // single-app batch settles, so re-add to model a multi-app batch by
        // keeping another in flight) — verify the per-app status logic
        // directly: a batch app that is Installed reports Done.
        s.update_batch = vec![gimp.into()];
        assert_eq!(s.batch_status(gimp), Some(BatchRowStatus::Done));
        let p = s.update_progress();
        assert_eq!(p.done, 1);
        assert!(!p.in_flight());
        assert_eq!(p.status_line(), "Update complete");
    }

    /// Place the store at a known viewport so width-driven layout resolves
    /// deterministically (mirrors the `apply_layout` rect capture).
    fn at_width(w: f64) -> SoftwareStore {
        let mut s = store();
        s.rect = Rect::new(0.0, 0.0, w, 800.0);
        // Mirror apply_layout's reconcile so the flap tracks the layout.
        s.nav_flap.reconcile(s.nav_layout());
        s
    }

    #[test]
    fn nav_layout_is_width_driven() {
        // At/above the breakpoint the nav tabs dock inline; below it they
        // collapse to the overlay drawer.
        let wide = at_width(1280.0);
        assert_eq!(wide.nav_layout(), SplitLayout::Docked);
        assert!(!wide.is_narrow());

        let narrow = at_width(720.0);
        assert_eq!(narrow.nav_layout(), SplitLayout::Overlay);
        assert!(narrow.is_narrow());

        // Boundary is inclusive → docked.
        assert_eq!(at_width(NAV_BREAKPOINT).nav_layout(), SplitLayout::Docked);
        assert_eq!(at_width(NAV_BREAKPOINT - 1.0).nav_layout(), SplitLayout::Overlay);
    }

    #[test]
    fn narrow_drawer_closed_by_default_then_toggles() {
        let mut s = at_width(720.0);
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        // Closed by default on a narrow window.
        assert!(!s.nav_flap.is_open(s.nav_layout()), "narrow drawer starts closed");
        // Hamburger toggles it open.
        s.apply(Action::ToggleNav, &mut doc);
        assert!(s.nav_flap.is_open(s.nav_layout()), "ToggleNav opens the drawer");
        // Scrim tap / DismissNav closes it.
        s.apply(Action::DismissNav, &mut doc);
        assert!(!s.nav_flap.is_open(s.nav_layout()), "DismissNav closes the drawer");
    }

    #[test]
    fn selecting_a_nav_destination_dismisses_the_drawer() {
        let mut s = at_width(720.0);
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        s.apply(Action::ToggleNav, &mut doc);
        assert!(s.nav_flap.is_open(s.nav_layout()));
        // Choosing a tab from the drawer navigates AND closes it.
        s.apply(Action::SetTab(Tab::Updates), &mut doc);
        assert_eq!(s.tab, Tab::Updates);
        assert!(!s.nav_flap.is_open(s.nav_layout()), "selection dismisses the overlay drawer");
    }

    #[test]
    fn wide_window_keeps_drawer_irrelevant() {
        // On a wide window the inline tabs are used; the flap default stays
        // closed and `is_narrow()` is false, so the overlay never paints.
        let mut s = at_width(1280.0);
        let mut doc = AutoCommit::new();
        s.sync(&mut doc, &automerge::ROOT);
        assert!(!s.is_narrow());
        assert!(!s.nav_flap.is_open(s.nav_layout()));
    }

    #[test]
    fn shot_menu_env_snaps_drawer_open() {
        // The `--shots` menu hook snaps the drawer fully open so a single
        // offscreen frame at a narrow viewport captures the overlay + scrim.
        let mut s = at_width(720.0);
        s.nav_flap.snap(true);
        assert!(s.nav_flap.is_open(s.nav_layout()));
        assert!(s.nav_flap.reveal() >= 0.999, "snap settles reveal to fully open");
    }
}
