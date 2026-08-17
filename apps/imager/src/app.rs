//! Root widget for the Disk Imager (imager) app.
//!
//! `ImagerApp` is a single self-drawing "canvas" widget that renders a guided
//! **wizard** with a six-station step indicator —
//! Source → Drive → Customize → Confirm → Write → Done — and a Back /
//! primary-action footer, covering both writing an image to a stick and
//! installing qOS onto a disk.
//!
//! * **Source** — pick a local `.iso` / `.img` through the host XDG portal
//!   (never a bespoke picker, per HIG §5), or a known qOS release.
//! * **Drive** — choose the target from the *real* block devices (`lsblk`);
//!   internal disks are hidden behind an explicit opt-in, and the **system
//!   disk** cannot be selected without a separate acknowledgement.
//! * **Customize** (qOS only) — the shared guided-customization step, delegated
//!   in full to [`CustomizePanel`](crate::customize); a plain ISO skips it.
//! * **Confirm** — a read-only review of every choice, an erase warning that
//!   escalates for an internal / system disk, and the typed device-name gate.
//! * **Write** — a real, byte-for-byte streaming copy with live progress and a
//!   read-back **verify** pass (captioned "Write" or "Install").
//! * **Done** — the verified / failed outcome with a fresh-start affordance.
//!
//! Every write is doubly gated: the typed confirmation at Confirm, plus the
//! `QOS_IMAGER_ALLOW_WRITE=1` allow-gate in [`crate::write`]. With the gate off
//! (tests, `--shots`, normal dev runs) the exact same progress UI is driven by
//! a simulated worker that touches no device.
//!
//! All persistent state (selected source/target, the live profile, the typed
//! confirmation, the current step, job progress) lives in the Automerge
//! document so it is undoable and replicates to collaborators. The transient
//! job/portal channels are the only non-CRDT state.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::customize::CustomizePanel;
use crate::doc::ImagerDoc;
use crate::model::{
    fmt_eta, fmt_rate, fmt_size, Action, InstallProfile, JobPhase, Progress, Source, SourceKind,
    Step, TargetDrive, TargetKind,
};
use crate::write::{spawn_job, WriteRequest};

// ---- layout constants (logical px) -------------------------------------
const OUTER: f64 = 28.0;
// The compositor draws the window chrome + app name (OS owns chrome), so the
// app itself opens straight into the step indicator — no in-app title bar.
const CRUMB_TOP: f64 = 24.0;
// The step-indicator band. Tall enough to host either full labelled pills, an
// icon rail with a caption line, or a compact segmented bar — whichever the
// window width admits (see [`ImagerApp::layout_stepbar`]).
const CRUMB_H: f64 = 46.0;
const FOOTER_H: f64 = 78.0;
const CONTENT_MAX_W: f64 = 900.0;

/// How the six-station step indicator is drawn for the current window width.
/// The indicator is fully responsive: it picks the richest tier that fits the
/// available width so it can never overflow or clip a station.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepBarMode {
    /// Full labelled pills — a disc + step icon + text label per station, with
    /// connectors between. Used when the whole labelled bar fits.
    Full,
    /// An icon rail — one disc (step icon inside) per station with connectors,
    /// and the current station named in a caption below. Used at medium widths.
    Rail,
    /// A compact segmented progress bar — six segments under a
    /// "Step N of 6 · Title" line. Always fits, for the narrowest windows.
    Compact,
}

/// The lucide glyph that stands for each wizard station — used in the step
/// indicator discs and (in the compact tiers) as the station's only mark.
fn step_icon(s: Step) -> Icon {
    match s {
        Step::Source => Icon::Disc3,
        Step::Drive => Icon::HardDrive,
        Step::Customize => Icon::SlidersHorizontal,
        Step::Confirm => Icon::Shield,
        Step::Write => Icon::Download,
        Step::Done => Icon::Check,
    }
}

/// A clickable hit-target for one frame. Pure function of the viewport +
/// state, so `draw` and `handle_event` agree without ordering hazards. The
/// Customize step's own affordances live inside [`CustomizePanel`], not here.
#[derive(Clone)]
enum Hit {
    PickFile,
    Release(usize),
    Recent(usize),
    Target(usize),
    ToggleInternal,
    ToggleSystemAck,
    Rescan,
    ConfirmField,
    StepJump(Step),
    Back,
    Next,
    StartWrite,
    WriteAnother,
}

struct Layout {
    crumbs: Vec<(Rect, Step)>,
    crumb_mode: StepBarMode,
    back: Option<Rect>,
    primary: Option<Rect>,
    hits: Vec<(Rect, Hit)>,
    content: Rect,
}

/// The Confirm step's rects for one frame — pure function of the content rect,
/// shared by drawing, hit-testing and event routing so they can never drift.
struct ConfirmGeom {
    card: Rect,
    banner: Rect,
    field: Rect,
}

pub struct ImagerApp {
    // ---- CRDT-backed (cached each `sync`) ----
    source: Option<Source>,
    recent: Vec<Source>,
    targets: Vec<TargetDrive>,
    selected: Option<usize>,
    action: Action,
    profile: InstallProfile,
    progress: Progress,
    confirm: String,
    status: String,
    step: Step,
    // ---- transient job / portal plumbing (not in the doc) ----
    job_rx: Option<std::sync::mpsc::Receiver<Progress>>,
    portal_rx: Option<PortalChannel>,
    show_internal: bool,
    /// The user has explicitly acknowledged that selecting the **system** disk
    /// (the one the running OS boots from) will destroy it. A system disk can
    /// never be selected without this — the guard against picking it by
    /// accident. Reset whenever the drive inventory changes.
    system_ack: bool,
    export_text: Option<String>,
    /// Whether the Confirm step's typed device-name field is capturing keys.
    confirm_focused: bool,
    // ---- render caches ----
    seeded: bool,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    hover_target: Option<usize>,
    /// The shared guided-customization panel — the one component the write and
    /// install flows both use to collect the install profile. It owns its own
    /// fields (real [`ui::widget::TextInput`]s, the password masked + a11y
    /// protected) and dispatches each edit through [`Self::actions`]. See
    /// [`crate::customize`].
    customize: CustomizePanel,
    /// The Confirm step's typed device-name gate — a real shared
    /// [`ui::widget::TextInput`] (caret, selection, clipboard and a11y for free)
    /// rather than a hand-rolled `String`. It is the editing surface only; the
    /// authoritative armed value stays [`Self::confirm`] (mirrored to the doc),
    /// so the write-gating logic and its tests read one field.
    confirm_input: TextInput,
    /// Framework-owned action dispatch (the only action-dispatch state the app
    /// holds). Every guided-customization profile edit dispatches its
    /// [`ImagerAction`](crate::actions::ImagerAction) through this host, which
    /// brackets the undo boundary and applies it through the executor so it
    /// records one Ctrl+Z step and replays byte-identically on peers.
    actions: ui::ActionHost,
}

impl Default for ImagerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ImagerApp {
    pub fn new() -> Self {
        Self {
            source: None,
            recent: Vec::new(),
            targets: Vec::new(),
            selected: None,
            action: Action::Flash,
            profile: InstallProfile::default(),
            progress: Progress::default(),
            confirm: String::new(),
            status: String::new(),
            step: Step::Source,
            job_rx: None,
            portal_rx: None,
            show_internal: false,
            system_ack: false,
            export_text: None,
            confirm_focused: false,
            seeded: false,
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            hover_target: None,
            customize: CustomizePanel::new(),
            confirm_input: {
                let mut t = TextInput::local();
                t.set_plain_chrome(true); // the app draws the field box itself
                t
            },
            actions: ui::ActionHost::new(),
        }
    }

    fn target(&self) -> Option<&TargetDrive> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    fn releases(&self) -> Vec<crate::model::QosRelease> {
        crate::model::sample_releases()
    }

    /// The drives shown on the Drive step: always the removable ones, plus
    /// internal/fixed disks only when the user explicitly opts in.
    fn visible_targets(&self) -> Vec<(usize, &TargetDrive)> {
        self.targets
            .iter()
            .enumerate()
            .filter(|(_, d)| self.show_internal || d.removable)
            .collect()
    }

    fn removable_count(&self) -> usize {
        self.targets.iter().filter(|d| d.removable).count()
    }

    /// Whether the currently-visible drive list contains the system disk — the
    /// only case where the extra "I understand" acknowledgement is offered.
    fn has_visible_system_disk(&self) -> bool {
        self.visible_targets().iter().any(|(_, d)| d.is_system)
    }

    /// Can the flow move forward from `from`? (Also gates the step-indicator.)
    fn can_advance(&self, from: Step) -> bool {
        match from {
            Step::Source => self.source.is_some(),
            Step::Drive => self.selected.is_some(),
            Step::Customize => true,
            // Confirm advances into the (destructive) write only once armed.
            Step::Confirm => self.confirm_matches() && self.image_fits(),
            // Write is in-progress and Done is terminal — neither advances.
            Step::Write | Step::Done => false,
        }
    }

    /// Whether `s` is a completed station (drawn with a green check). A step is
    /// completed once the flow has moved past it — except a Customize station
    /// that was *skipped* (a plain ISO carries no profile), which reads as
    /// not-engaged rather than done.
    fn step_completed(&self, s: Step) -> bool {
        s.index() < self.step.index() && !(s == Step::Customize && !self.customizable())
    }

    /// Whether a step can be jumped to by clicking its indicator station. The
    /// execution stations (Write, Done) are never jump targets.
    fn crumb_reachable(&self, s: Step) -> bool {
        match s {
            Step::Source => true,
            Step::Drive => self.source.is_some(),
            Step::Customize => self.selected.is_some() && self.customizable(),
            Step::Confirm => self.selected.is_some(),
            Step::Write | Step::Done => false,
        }
    }

    /// Only qOS images can be pre-configured; a plain ISO is written as-is.
    fn customizable(&self) -> bool {
        self.source.as_ref().map(|s| s.installable).unwrap_or(false)
    }

    /// Writing to a **removable** drive is a plain image write ([`Action::Flash`],
    /// verb "Write"); a customizable qOS image aimed at a **fixed/internal** disk
    /// is a full install ([`Action::Install`], verb "Install"). This single
    /// helper is the source of truth for the write-vs-install naming everywhere.
    fn intended_action(&self) -> Action {
        match (self.customizable(), self.target()) {
            (true, Some(t)) if !t.removable => Action::Install,
            _ => Action::Flash,
        }
    }

    /// The label for the customization resource Ctrl+K publishes, or `None` when
    /// the current source has no profile to configure (a plain ISO, or nothing
    /// selected) so the palette shows nothing to act on. Used by the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    pub(crate) fn customization_source_label(&self) -> Option<String> {
        if !self.customizable() {
            return None;
        }
        Some(self.source.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| "qOS image".into()))
    }

    /// The last profile [`EncodedAction`](qos_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec` covenant
    /// test.
    #[doc(hidden)]
    pub fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    fn confirm_matches(&self) -> bool {
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        !self.confirm.is_empty()
            && (self.confirm == want || self.confirm == format!("/dev/{want}"))
    }

    fn image_fits(&self) -> bool {
        match (&self.source, self.target()) {
            (Some(s), Some(t)) => t.fits(s.size),
            _ => false,
        }
    }
}

/// Whether this process should enumerate real block devices. The live binary
/// sets `IMAGER_LIVE=1`; `--shots`, `--demo` and tests leave it unset so the
/// document's seeded/posed state is authoritative and no `lsblk` runs.
fn live_probe_enabled() -> bool {
    std::env::var("IMAGER_LIVE").is_ok()
}

// ---- theme-derived colour helpers --------------------------------------
fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

fn source_color(theme: &Theme, kind: SourceKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        SourceKind::QosImage => c.accent[5],
        SourceKind::IsoFile => c.semantic.info,
    }
}

fn kind_color(theme: &Theme, kind: TargetKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        TargetKind::Usb => c.semantic.success,
        TargetKind::Nvme => c.accent[5],
        TargetKind::Ssd => c.semantic.info,
        TargetKind::Hdd => c.neutral[5],
        TargetKind::SdCard => c.accent[7],
        TargetKind::Other => c.neutral[4],
    }
}

// =========================================================================
// Drawing primitives
// =========================================================================
impl ImagerApp {
    #[allow(clippy::too_many_arguments)]
    fn txt(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        x: f64,
        top: f64,
        max_w: Option<f32>,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(scene, s, size, color, Affine::translate((x, top)), max_w, weight);
    }

    #[allow(clippy::too_many_arguments)]
    fn txt_right(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        right: f64,
        top: f64,
        weight: TextWeight,
    ) {
        let (w, _) = tr.measure_weighted(s, size, None, weight);
        self.txt(scene, tr, s, size, color, right - w as f64, top, None, weight);
    }

    #[allow(clippy::too_many_arguments)]
    fn txt_center(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        cx: f64,
        top: f64,
        weight: TextWeight,
    ) {
        let (w, _) = tr.measure_weighted(s, size, None, weight);
        self.txt(scene, tr, s, size, color, cx - w as f64 / 2.0, top, None, weight);
    }

    fn fill_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color) {
        scene.fill(Fill::NonZero, t, c, None, &RoundedRect::from_rect(r, radius));
    }
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(&Stroke::new(w), t, c, None, &RoundedRect::from_rect(r, radius));
    }
    fn dashed_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        let stroke = Stroke::new(w).with_dashes(0.0, [7.0, 6.0]);
        scene.stroke(&stroke, t, c, None, &RoundedRect::from_rect(r, radius));
    }
    fn bar(scene: &mut Scene, t: Affine, r: Rect, frac: f64, track: Color, fill: Color) {
        let radius = (r.height() / 2.0).min(6.0);
        Self::fill_rr(scene, t, r, radius, track);
        let frac = frac.clamp(0.0, 1.0);
        if frac > 0.0 {
            let w = (r.width() * frac).max(radius * 2.0).min(r.width());
            let fr = Rect::new(r.x0, r.y0, r.x0 + w, r.y1);
            Self::fill_rr(scene, t, fr, radius, fill);
        }
    }

    /// A triangle-alert mark (warning triangle + exclamation) filling `r`, in
    /// `color`. The icon set has no triangle-alert glyph, so this is the one
    /// bespoke semantic mark — it is not an interactive control.
    fn warn_mark(scene: &mut Scene, t: Affine, r: Rect, color: Color) {
        let cx = (r.x0 + r.x1) / 2.0;
        let top = r.y0 + r.height() * 0.06;
        let bot = r.y1 - r.height() * 0.08;
        let mut tri = BezPath::new();
        tri.move_to((cx, top));
        tri.line_to((r.x1, bot));
        tri.line_to((r.x0, bot));
        tri.close_path();
        let stroke = Stroke::new(1.8).with_caps(vello::kurbo::Cap::Round).with_join(vello::kurbo::Join::Round);
        scene.stroke(&stroke, t, color, None, &tri);
        // Exclamation: a short bar over a dot.
        let bar_top = r.y0 + r.height() * 0.34;
        let bar_bot = r.y0 + r.height() * 0.62;
        scene.stroke(&Stroke::new(1.8).with_caps(vello::kurbo::Cap::Round), t, color, None, &Line::new(Point::new(cx, bar_top), Point::new(cx, bar_bot)));
        scene.fill(Fill::NonZero, t, color, None, &Circle::new(Point::new(cx, r.y0 + r.height() * 0.76), 1.1));
    }

    /// A small check-mark glyph inside a filled disc of radius `rad` at `c`.
    fn check_disc(scene: &mut Scene, t: Affine, center: Point, rad: f64, disc: Color, tick: Color) {
        scene.fill(Fill::NonZero, t, disc, None, &Circle::new(center, rad));
        let mut p = BezPath::new();
        p.move_to((center.x - rad * 0.42, center.y + rad * 0.02));
        p.line_to((center.x - rad * 0.10, center.y + rad * 0.34));
        p.line_to((center.x + rad * 0.46, center.y - rad * 0.34));
        scene.stroke(&Stroke::new(rad * 0.24), t, tick, None, &p);
    }

    fn section_label(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, s: &str, x: f64, y: f64) {
        self.txt(scene, tr, s, 11.0, theme.tokens.colors.text_dim, x, y, None, TextWeight::Semibold);
    }
}

// =========================================================================
// Layout
// =========================================================================
impl ImagerApp {
    fn content_rect(&self) -> Rect {
        let v = self.rect;
        let top = CRUMB_TOP + CRUMB_H + 30.0;
        let bottom = v.y1 - FOOTER_H - 10.0;
        let avail_w = v.width() - OUTER * 2.0;
        let w = avail_w.min(CONTENT_MAX_W);
        let x0 = v.x0 + (v.width() - w) / 2.0;
        Rect::new(x0, v.y0 + top, x0 + w, v.y0 + bottom)
    }

    fn compute_layout(&self) -> Layout {
        let v = self.rect;
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let content = self.content_rect();

        // ---- Responsive step indicator ----
        let (crumbs, crumb_mode) = self.layout_stepbar(&mut hits);

        // ---- Per-step content hit-targets ----
        match self.step {
            Step::Source => self.layout_source(content, &mut hits),
            Step::Drive => self.layout_drive(content, &mut hits),
            // The Customize step's fields/toggles are owned by `CustomizePanel`,
            // which hit-tests its own rects in `handle_event` — no app hits here.
            Step::Customize => {}
            Step::Confirm => self.layout_confirm(content, &mut hits),
            // Write (in progress) and Done have no interactive content beyond
            // the footer.
            Step::Write | Step::Done => {}
        }

        // ---- Footer (Back + primary) ----
        let fy = v.y1 - FOOTER_H;
        let by = fy + (FOOTER_H - 44.0) / 2.0;
        let (mut back, mut primary) = (None, None);
        let show_back = matches!(self.step, Step::Drive | Step::Customize | Step::Confirm);
        if show_back {
            let r = Rect::new(content.x0, by, content.x0 + 120.0, by + 44.0);
            hits.push((r, Hit::Back));
            back = Some(r);
        }
        // The Write station is a live progress screen with no primary action.
        if let Some(p) = self.primary_button_rect(content, by) {
            let hit = match self.step {
                Step::Confirm => Hit::StartWrite,
                Step::Done => Hit::WriteAnother,
                _ => Hit::Next,
            };
            hits.push((p, hit));
            primary = Some(p);
        }

        Layout { crumbs, crumb_mode, back, primary, hits, content }
    }

    /// Lay out the six-station step indicator so it **fits any window width**.
    /// Returns the per-station rects (always six, one per [`Step`], for hit
    /// testing and drawing) and the tier chosen for the current width. Reachable
    /// stations get a [`Hit::StepJump`] on their rect. This is a pure function
    /// of the viewport + state (no text measurement), so draw and hit-test
    /// always agree and the tier is deterministic in tests.
    fn layout_stepbar(&self, hits: &mut Vec<(Rect, Hit)>) -> (Vec<(Rect, Step)>, StepBarMode) {
        let v = self.rect;
        let steps = Step::all();
        let n = steps.len() as f64;
        let cy = v.y0 + CRUMB_TOP;

        // The bar lives in the same centred column as the content, capped so it
        // never stretches edge-to-edge on a very wide window.
        let bar_w = (v.width() - OUTER * 2.0).max(0.0).min(CONTENT_MAX_W);
        let bx0 = v.x0 + (v.width() - bar_w) / 2.0;

        // Pick the richest tier that fits `bar_w`.
        let full_gap = 10.0;
        let full_min_pill = 116.0; // disc + step icon + longest label + padding
        let full_total = full_min_pill * n + full_gap * (n - 1.0);
        let rail_min = 34.0 * n + 96.0; // discs + connectors + the current caption
        let mode = if bar_w >= full_total {
            StepBarMode::Full
        } else if bar_w >= rail_min {
            StepBarMode::Rail
        } else {
            StepBarMode::Compact
        };

        let mut crumbs = Vec::with_capacity(steps.len());
        match mode {
            StepBarMode::Full => {
                let pill_w = ((bar_w - full_gap * (n - 1.0)) / n).min(150.0);
                let total = pill_w * n + full_gap * (n - 1.0);
                let mut cx = bx0 + (bar_w - total) / 2.0;
                for s in steps {
                    // Pill height 34, vertically centred in the band.
                    let r = Rect::new(cx, cy + 6.0, cx + pill_w, cy + 40.0);
                    crumbs.push((r, s));
                    cx += pill_w + full_gap;
                }
            }
            StepBarMode::Rail => {
                // Discs evenly spaced centre-to-centre; the hit rect is the disc
                // box. The current station is named in a caption below (drawn in
                // `draw_stepbar`), so no per-disc labels crowd the row.
                let d = 26.0;
                let step = (bar_w - d) / (n - 1.0);
                for (i, s) in steps.into_iter().enumerate() {
                    let cxi = bx0 + d / 2.0 + step * i as f64;
                    let cyd = cy + 13.0;
                    let r = Rect::new(cxi - d / 2.0, cyd - d / 2.0, cxi + d / 2.0, cyd + d / 2.0);
                    crumbs.push((r, s));
                }
            }
            StepBarMode::Compact => {
                // Six segments filling the bar, under a "Step N of 6 · Title"
                // line drawn in `draw_stepbar`.
                let seg_gap = 5.0;
                let seg_w = (bar_w - seg_gap * (n - 1.0)) / n;
                let seg_top = cy + CRUMB_H - 12.0;
                let mut cx = bx0;
                for s in steps {
                    let r = Rect::new(cx, seg_top, cx + seg_w, seg_top + 7.0);
                    crumbs.push((r, s));
                    cx += seg_w + seg_gap;
                }
            }
        }

        for (r, s) in &crumbs {
            if self.crumb_reachable(*s) {
                hits.push((*r, Hit::StepJump(*s)));
            }
        }
        (crumbs, mode)
    }

    /// The confirm-step geometry (review card, danger banner, typed-name field),
    /// as a pure function of the content rect so draw + hit-test + event-routing
    /// never drift. The field is pinned directly under the banner rather than to
    /// the window bottom, so the review reads as one top-aligned column.
    fn confirm_geom(content: Rect) -> ConfirmGeom {
        let card = Rect::new(content.x0, content.y0 + 46.0, content.x1, content.y0 + 46.0 + 150.0);
        let banner = Rect::new(content.x0, card.y1 + 16.0, content.x1, card.y1 + 16.0 + 46.0);
        let field = Rect::new(content.x0, banner.y1 + 34.0, content.x1, banner.y1 + 34.0 + 48.0);
        ConfirmGeom { card, banner, field }
    }

    /// The Y at which a step's body starts, below its title + subtitle. On a
    /// narrow window the gap widens so a wrapped two-line subtitle never
    /// collides with the first card. Pure function of the content rect, so draw
    /// and hit-test always agree.
    fn step_body_top(content: Rect) -> f64 {
        content.y0 + if content.width() < 470.0 { 62.0 } else { 44.0 }
    }

    /// The Y of the Drive step's controls row (detected count + Rescan). The
    /// device cards start [`Self::drive_cards_top`] below it. One source of
    /// truth for draw + hit-test.
    fn drive_controls_y(content: Rect) -> f64 {
        Self::step_body_top(content) + 4.0
    }
    fn drive_cards_top(content: Rect) -> f64 {
        Self::drive_controls_y(content) + 44.0
    }
    /// The Rescan button rect, right-aligned in the Drive controls row.
    fn rescan_rect(content: Rect) -> Rect {
        let y = Self::drive_controls_y(content);
        Rect::new(content.x1 - 104.0, y, content.x1, y + 30.0)
    }

    /// The primary action button rect, right-aligned in the footer — or `None`
    /// on the Write station (a live-progress screen with no primary).
    fn primary_button_rect(&self, content: Rect, by: f64) -> Option<Rect> {
        if self.step == Step::Write {
            return None;
        }
        let desired: f64 = match self.step {
            Step::Confirm => 250.0,
            Step::Done => 190.0,
            _ => 170.0,
        };
        // Shrink so the primary never overruns the Back button on a narrow
        // footer (the label is elided to fit in `draw_primary`).
        let show_back = matches!(self.step, Step::Drive | Step::Customize | Step::Confirm);
        let reserved = if show_back { 120.0 + 20.0 } else { 0.0 };
        let w = desired.min((content.width() - reserved).max(140.0));
        Some(Rect::new(content.x1 - w, by, content.x1, by + 44.0))
    }

    fn layout_source(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        let x0 = content.x0;
        let x1 = content.x1;
        let mut y = Self::step_body_top(content);
        // Browse drop-zone.
        let browse = Rect::new(x0, y, x1, y + 96.0);
        hits.push((browse, Hit::PickFile));
        y = browse.y1 + (if self.source.is_some() { 16.0 } else { 28.0 });
        if self.source.is_some() {
            y += 92.0 + 24.0; // selected card
        }
        // qOS releases list.
        y += 22.0; // section label
        for (i, _) in self.releases().iter().enumerate() {
            let r = Rect::new(x0, y, x1, y + 54.0);
            hits.push((r, Hit::Release(i)));
            y += 54.0 + 10.0;
        }
        // Recent list.
        if !self.recent.is_empty() {
            y += 20.0;
            for (i, _) in self.recent.iter().enumerate().take(3) {
                let r = Rect::new(x0, y, x1, y + 44.0);
                if y + 44.0 > content.y1 {
                    break;
                }
                hits.push((r, Hit::Recent(i)));
                y += 44.0 + 8.0;
            }
        }
    }

    fn layout_drive(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        let x0 = content.x0;
        let x1 = content.x1;
        // Rescan button in its own controls row (below the title + subtitle), so
        // it never collides with the title on a narrow window.
        hits.push((Self::rescan_rect(content), Hit::Rescan));
        let mut y = Self::drive_cards_top(content);
        let vis = self.visible_targets();
        if vis.is_empty() {
            // Empty state occupies the body; the only action is the internal toggle below.
        } else {
            for (idx, _) in &vis {
                let r = Rect::new(x0, y, x1, y + 112.0);
                hits.push((r, Hit::Target(*idx)));
                y += 112.0 + 12.0;
                if y + 112.0 > content.y1 - 46.0 {
                    break;
                }
            }
        }
        // "Show internal disks" toggle pinned near the bottom of the content.
        let tr = Rect::new(x0, content.y1 - 44.0, x0 + 320.0, content.y1 - 44.0 + 34.0);
        hits.push((tr, Hit::ToggleInternal));
        // The system-disk acknowledgement sits beside it, only once a system
        // disk is actually visible.
        if self.has_visible_system_disk() {
            let ack = Rect::new(tr.x1 + 12.0, tr.y0, (tr.x1 + 12.0 + 470.0).min(x1), tr.y1);
            hits.push((ack, Hit::ToggleSystemAck));
        }
    }

    fn layout_confirm(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        // The typed device-name confirmation field sits just under the danger
        // banner (shared geometry with draw + event routing).
        hits.push((Self::confirm_geom(content).field, Hit::ConfirmField));
    }
}

// =========================================================================
// Header + breadcrumb
// =========================================================================
impl ImagerApp {
    fn draw_header(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let v = self.rect;
        // No in-app app-name header: the compositor owns the window chrome and
        // shows the app's name. The app opens straight into the step indicator,
        // with a slim status line tucked into the top-right margin.
        if !self.status.is_empty() {
            let col = match self.progress.phase {
                JobPhase::Failed => c.semantic.danger,
                JobPhase::Success => c.semantic.success,
                _ => c.text_dim,
            };
            self.txt_right(scene, tr, &self.status, 11.5, col, v.x1 - OUTER, v.y0 + 8.0, TextWeight::Medium);
        }

        // ---- Responsive step indicator ----
        self.draw_stepbar(scene, tr, theme, t, lay);
    }

    /// One step-indicator disc: a green check for a completed station, an
    /// accent-filled disc with a white step glyph for the current one, and a
    /// muted outlined disc with a dim glyph for a future one.
    fn draw_step_disc(&self, scene: &mut Scene, theme: &Theme, t: Affine, center: Point, rad: f64, s: Step, done: bool, current: bool) {
        let c = &theme.tokens.colors;
        if done {
            Self::check_disc(scene, t, center, rad, c.semantic.success, Color::WHITE);
            return;
        }
        let (fill, glyph) = if current {
            (c.accent[5], Color::WHITE)
        } else {
            (c.neutral[3], c.text_dim)
        };
        scene.fill(Fill::NonZero, t, fill, None, &Circle::new(center, rad));
        if current {
            scene.stroke(&Stroke::new(1.4), t, with_alpha(c.accent[5], 90), None, &Circle::new(center, rad + 2.6));
        }
        let g = rad * 1.15;
        let ir = Rect::new(center.x - g, center.y - g, center.x + g, center.y + g);
        step_icon(s).draw_with_stroke(scene, t, ir, glyph, 1.6);
    }

    /// Draw the step indicator in whichever responsive tier `compute_layout`
    /// chose — full labelled pills, an icon rail, or a compact segmented bar.
    fn draw_stepbar(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        match lay.crumb_mode {
            StepBarMode::Full => {
                for (i, (r, s)) in lay.crumbs.iter().enumerate() {
                    let done = self.step_completed(*s);
                    let current = *s == self.step;
                    let hov = r.contains(self.last_mouse) && self.crumb_reachable(*s);
                    if current {
                        Self::fill_rr(scene, t, *r, 11.0, with_alpha(c.accent[5], 38));
                        Self::stroke_rr(scene, t, *r, 11.0, c.accent[5], 1.4);
                    } else if hov {
                        Self::fill_rr(scene, t, *r, 11.0, c.neutral[2]);
                    }
                    let midy = (r.y0 + r.y1) / 2.0;
                    self.draw_step_disc(scene, theme, t, Point::new(r.x0 + 21.0, midy), 11.0, *s, done, current);
                    let label_c = if current || done { c.text_primary } else { c.text_dim };
                    self.txt(scene, tr, s.title(), 13.0, label_c, r.x0 + 38.0, midy - 9.0, Some((r.width() - 44.0) as f32), if current { TextWeight::Semibold } else { TextWeight::Medium });
                    if i + 1 < lay.crumbs.len() {
                        let nx0 = lay.crumbs[i + 1].0.x0;
                        let cc = if s.index() < self.step.index() { c.semantic.success } else { c.neutral[3] };
                        scene.stroke(&Stroke::new(2.0), t, cc, None, &Line::new(Point::new(r.x1 + 4.0, midy), Point::new(nx0 - 4.0, midy)));
                    }
                }
            }
            StepBarMode::Rail => {
                // Connectors first, so the discs sit on top of them.
                for i in 0..lay.crumbs.len().saturating_sub(1) {
                    let (r, s) = lay.crumbs[i];
                    let (nr, _) = lay.crumbs[i + 1];
                    let midy = (r.y0 + r.y1) / 2.0;
                    let cc = if s.index() < self.step.index() { c.semantic.success } else { c.neutral[3] };
                    scene.stroke(&Stroke::new(2.0), t, cc, None, &Line::new(Point::new(r.x1 + 3.0, midy), Point::new(nr.x0 - 3.0, midy)));
                }
                for (r, s) in &lay.crumbs {
                    let done = self.step_completed(*s);
                    let current = *s == self.step;
                    let center = Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
                    self.draw_step_disc(scene, theme, t, center, 11.0, *s, done, current);
                }
                // Caption naming the current station.
                if let Some((r, _)) = lay.crumbs.first() {
                    let bar_x0 = r.x0;
                    let bar_x1 = lay.crumbs.last().map(|(r, _)| r.x1).unwrap_or(bar_x0);
                    let cx = (bar_x0 + bar_x1) / 2.0;
                    let cap = format!("Step {} of {} · {}", self.step.index() + 1, lay.crumbs.len(), self.step.title());
                    self.txt_center(scene, tr, &cap, 12.5, c.text_primary, cx, r.y1 + 6.0, TextWeight::Semibold);
                }
            }
            StepBarMode::Compact => {
                if let (Some((first, _)), Some((last, _))) = (lay.crumbs.first(), lay.crumbs.last()) {
                    let bx0 = first.x0;
                    let bx1 = last.x1;
                    // Label line above the segments.
                    let ly = first.y0 - 22.0;
                    self.txt(scene, tr, &format!("Step {} of {}", self.step.index() + 1, lay.crumbs.len()), 12.0, c.text_dim, bx0, ly, None, TextWeight::Semibold);
                    self.txt_right(scene, tr, self.step.title(), 13.5, c.text_primary, bx1, ly - 1.0, TextWeight::Bold);
                }
                for (r, s) in &lay.crumbs {
                    let done = self.step_completed(*s);
                    let current = *s == self.step;
                    let col = if done {
                        c.semantic.success
                    } else if current {
                        c.accent[5]
                    } else {
                        c.neutral[3]
                    };
                    Self::fill_rr(scene, t, *r, r.height() / 2.0, col);
                }
            }
        }
    }

    fn draw_footer(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let v = self.rect;
        let fy = v.y1 - FOOTER_H;
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new(Point::new(v.x0, fy), Point::new(v.x1, fy)));

        // Back.
        if let Some(r) = lay.back {
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(scene, t, r, 10.0, if hov { c.neutral[3] } else { c.neutral[2] });
            Self::stroke_rr(scene, t, r, 10.0, c.border_standard, 1.0);
            self.txt_center(scene, tr, "‹  Back", 14.0, c.text_primary, (r.x0 + r.x1) / 2.0, r.y0 + 12.0, TextWeight::Medium);
        }

        // Primary.
        if let Some(r) = lay.primary {
            self.draw_primary(scene, tr, theme, t, r);
        }

        // A hint between them (center) — e.g. what's still needed.
        if let Some(hint) = self.footer_hint() {
            self.txt_center(scene, tr, &hint, 12.0, c.text_dim, (v.x0 + v.x1) / 2.0, fy + FOOTER_H / 2.0 - 8.0, TextWeight::Regular);
        }
    }

    fn footer_hint(&self) -> Option<String> {
        match self.step {
            Step::Source if self.source.is_none() => Some("Choose an image to continue".into()),
            Step::Drive if self.selected.is_none() => Some("Select a drive to continue".into()),
            Step::Confirm if !self.confirm_matches() => {
                self.target().map(|d| format!("Type “{}” to unlock", d.name))
            }
            Step::Write => Some("Keep the drive connected until this finishes…".into()),
            _ => None,
        }
    }

    fn draw_primary(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, r: Rect) {
        let c = &theme.tokens.colors;
        let hov = r.contains(self.last_mouse);
        let (label, enabled, destructive) = match self.step {
            Step::Source => ("Continue  ›".to_string(), self.can_advance(Step::Source), false),
            Step::Drive => ("Continue  ›".to_string(), self.can_advance(Step::Drive), false),
            Step::Customize => ("Continue  ›".to_string(), true, false),
            Step::Confirm => {
                let dev = self.target().map(|d| d.dev_path()).unwrap_or_default();
                // "Erase & write /dev/sdb" for a stick; "Erase & install
                // /dev/nvme0n1" for a disk install — the verb reads plainly.
                (
                    format!("Erase & {}  {dev}", self.action.verb().to_ascii_lowercase()),
                    self.can_advance(Step::Confirm),
                    true,
                )
            }
            // The Write station has no primary (returned None above); Done
            // offers a fresh start.
            Step::Write => ("Writing…".to_string(), false, false),
            Step::Done => match self.progress.phase {
                JobPhase::Failed => ("Try again".to_string(), true, false),
                _ => ("Start over".to_string(), true, false),
            },
        };
        let bg = if !enabled {
            c.neutral[3]
        } else if destructive {
            if hov { c.semantic.danger } else { with_alpha(c.semantic.danger, 220) }
        } else if hov {
            c.accent[6]
        } else {
            c.accent[5]
        };
        Self::fill_rr(scene, t, r, 11.0, bg);
        let fg = if enabled { Color::WHITE } else { c.neutral[6] };
        let label = tr.elide_to_width_weighted(&label, 14.0, (r.width() - 24.0) as f32, TextWeight::Bold);
        self.txt_center(scene, tr, &label, 14.0, fg, (r.x0 + r.x1) / 2.0, r.y0 + 13.0, TextWeight::Bold);
    }
}

// =========================================================================
// Step bodies
// =========================================================================
impl ImagerApp {
    fn draw_step_title(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, content: Rect, title: &str, sub: &str) {
        let c = &theme.tokens.colors;
        self.txt(scene, tr, title, 19.0, c.text_primary, content.x0, content.y0, None, TextWeight::Bold);
        self.txt(scene, tr, sub, 13.0, c.text_dim, content.x0, content.y0 + 26.0, Some(content.width() as f32), TextWeight::Regular);
    }

    fn draw_source_step(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        self.draw_step_title(scene, tr, theme, content, "Choose the source image", "Pick a local .iso, .img or .raw file — or a qOS release below.");
        let mut y = Self::step_body_top(content);

        // Browse drop-zone.
        let browse = Rect::new(content.x0, y, content.x1, y + 96.0);
        let hov = browse.contains(self.last_mouse);
        Self::fill_rr(scene, t, browse, 14.0, if hov { c.neutral[2] } else { with_alpha(c.accent[5], 16) });
        Self::dashed_rr(scene, t, browse, 14.0, if hov { c.accent[6] } else { with_alpha(c.accent[5], 160) }, 1.6);
        // A folder-open glyph in an accent tile leads the drop-zone.
        let gy = (browse.y0 + browse.y1) / 2.0;
        let tile = Rect::new(browse.x0 + 24.0, gy - 22.0, browse.x0 + 24.0 + 44.0, gy + 22.0);
        Self::fill_rr(scene, t, tile, 11.0, with_alpha(c.accent[5], 34));
        let ic = Rect::new(tile.x0 + 11.0, tile.y0 + 11.0, tile.x1 - 11.0, tile.y1 - 11.0);
        Icon::FolderOpen.draw_with_stroke(scene, t, ic, c.accent[6], 1.8);
        self.txt(scene, tr, "Browse for an image…", 16.0, c.text_primary, browse.x0 + 84.0, browse.y0 + 30.0, None, TextWeight::Semibold);
        self.txt(scene, tr, "Opens the system file picker · .iso, .img, .raw", 12.5, c.text_dim, browse.x0 + 84.0, browse.y0 + 54.0, None, TextWeight::Regular);
        y = browse.y1 + 16.0;

        // Selected-source card.
        if let Some(src) = &self.source {
            let card = Rect::new(content.x0, y, content.x1, y + 92.0);
            Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, card, 14.0, with_alpha(c.semantic.success, 150), 1.4);
            Self::check_disc(scene, t, Point::new(card.x0 + 28.0, (card.y0 + card.y1) / 2.0), 13.0, c.semantic.success, Color::WHITE);
            let badge = Rect::new(card.x0 + 52.0, card.y0 + 18.0, card.x0 + 52.0 + 46.0, card.y0 + 18.0 + 22.0);
            Self::fill_rr(scene, t, badge, 6.0, source_color(theme, src.kind));
            self.txt_center(scene, tr, src.kind.tag(), 12.0, Color::WHITE, (badge.x0 + badge.x1) / 2.0, badge.y0 + 4.0, TextWeight::Bold);
            self.txt(scene, tr, &src.name, 15.5, c.text_primary, card.x0 + 108.0, card.y0 + 18.0, Some((card.width() - 130.0) as f32), TextWeight::Semibold);
            // Label + size only — no leaked filesystem path in chrome (HIG §4);
            // the file name is already the card title above.
            self.txt(scene, tr, &format!("{}  ·  {}", src.label, fmt_size(src.size)), 12.0, c.text_dim, card.x0 + 108.0, card.y0 + 42.0, Some((card.width() - 130.0) as f32), TextWeight::Regular);
            if src.installable {
                self.txt(scene, tr, "● Can be pre-configured", 11.5, c.semantic.success, card.x0 + 108.0, card.y0 + 62.0, None, TextWeight::Medium);
            } else {
                self.txt(scene, tr, "Written as-is", 11.5, c.text_dim, card.x0 + 108.0, card.y0 + 62.0, None, TextWeight::Medium);
            }
            y = card.y1 + 24.0;
        } else {
            y = browse.y1 + 28.0;
        }

        // qOS releases.
        self.section_label(scene, tr, theme, "OR CHOOSE A qOS RELEASE", content.x0, y);
        y += 22.0;
        for rel in self.releases().iter() {
            let r = Rect::new(content.x0, y, content.x1, y + 54.0);
            let selected = self.source.as_ref().map(|s| s.kind == SourceKind::QosImage && s.name == format!("{} {}", rel.name, rel.version)).unwrap_or(false);
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(scene, t, r, 11.0, if selected { with_alpha(c.accent[5], 34) } else if hov { c.neutral[2] } else { c.surfaces.raised });
            Self::stroke_rr(scene, t, r, 11.0, if selected { c.accent[5] } else { c.border_standard }, if selected { 1.4 } else { 1.0 });
            let badge = Rect::new(r.x0 + 14.0, r.y0 + 15.0, r.x0 + 14.0 + 42.0, r.y0 + 15.0 + 22.0);
            Self::fill_rr(scene, t, badge, 6.0, c.accent[5]);
            self.txt_center(scene, tr, "qOS", 11.0, Color::WHITE, (badge.x0 + badge.x1) / 2.0, badge.y0 + 5.0, TextWeight::Bold);
            self.txt(scene, tr, &format!("qOS {}", rel.version), 14.5, c.text_primary, r.x0 + 68.0, r.y0 + 9.0, None, TextWeight::Semibold);
            let where_ = if rel.local_path.is_empty() { "download" } else { "on disk" };
            self.txt(scene, tr, &format!("{}  ·  {}  ·  {}", rel.channel, fmt_size(rel.size), where_), 11.5, c.text_dim, r.x0 + 68.0, r.y0 + 30.0, None, TextWeight::Regular);
            self.txt_right(scene, tr, "installable", 11.0, c.semantic.success, r.x1 - 16.0, r.y0 + 20.0, TextWeight::Medium);
            y += 54.0 + 10.0;
        }

        // Recent.
        if !self.recent.is_empty() {
            y += 12.0;
            self.section_label(scene, tr, theme, "RECENT", content.x0, y);
            y += 20.0;
            for src in self.recent.iter().take(3) {
                if y + 44.0 > content.y1 {
                    break;
                }
                let r = Rect::new(content.x0, y, content.x1, y + 44.0);
                let hov = r.contains(self.last_mouse);
                if hov {
                    Self::fill_rr(scene, t, r, 9.0, c.neutral[2]);
                }
                let sw = Rect::new(r.x0 + 6.0, r.y0 + 10.0, r.x0 + 11.0, r.y1 - 10.0);
                Self::fill_rr(scene, t, sw, 2.0, source_color(theme, src.kind));
                self.txt(scene, tr, &src.name, 13.0, c.text_primary, r.x0 + 22.0, r.y0 + 6.0, Some((r.width() - 120.0) as f32), TextWeight::Medium);
                self.txt(scene, tr, &fmt_size(src.size), 11.0, c.text_dim, r.x0 + 22.0, r.y0 + 24.0, None, TextWeight::Regular);
                self.txt_right(scene, tr, src.kind.tag(), 10.5, c.neutral[6], r.x1 - 12.0, r.y0 + 15.0, TextWeight::Semibold);
                y += 44.0 + 8.0;
            }
        }
    }

    fn draw_drive_step(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        self.draw_step_title(scene, tr, theme, content, "Select the target drive", "Everything on the chosen drive is erased.");
        // Controls row (below the title): detected count on the left, Rescan on
        // the right — its own row so it never collides with the title.
        let rescan = Self::rescan_rect(content);
        let detected = if self.show_internal {
            format!("{} drive(s) detected", self.targets.len())
        } else {
            format!("{} removable drive(s)", self.removable_count())
        };
        self.txt(scene, tr, &detected, 12.0, c.text_dim, content.x0, rescan.y0 + 8.0, Some((rescan.x0 - content.x0 - 12.0) as f32), TextWeight::Medium);
        let hov = rescan.contains(self.last_mouse);
        Self::fill_rr(scene, t, rescan, 9.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, rescan, 9.0, c.border_standard, 1.0);
        let rir = Rect::new(rescan.x0 + 16.0, rescan.y0 + 8.0, rescan.x0 + 30.0, rescan.y0 + 22.0);
        Icon::RotateCw.draw_with_stroke(scene, t, rir, c.text_primary, 1.6);
        self.txt(scene, tr, "Rescan", 12.5, c.text_primary, rescan.x0 + 36.0, rescan.y0 + 7.0, None, TextWeight::Medium);

        let vis = self.visible_targets();
        let mut y = Self::drive_cards_top(content);
        if vis.is_empty() {
            self.draw_drive_empty(scene, tr, theme, t, content);
        } else {
            for (idx, drive) in &vis {
                if y + 112.0 > content.y1 - 46.0 {
                    break;
                }
                let r = Rect::new(content.x0, y, content.x1, y + 112.0);
                self.draw_drive_card(scene, tr, theme, t, r, *idx, drive);
                y += 112.0 + 12.0;
            }
        }

        // Show-internal toggle.
        let tr_rect = Rect::new(content.x0, content.y1 - 44.0, content.x0 + 340.0, content.y1 - 44.0 + 34.0);
        self.draw_check_row(
            scene,
            tr,
            theme,
            t,
            tr_rect,
            self.show_internal,
            c.semantic.danger,
            "Show internal disks",
        );
        // System-disk acknowledgement, shown only when the system disk is
        // actually in the list. It is the explicit gate that lets the system
        // disk be selected at all (see `select_target`).
        if self.has_visible_system_disk() {
            let ack = Rect::new(tr_rect.x1 + 12.0, tr_rect.y0, (tr_rect.x1 + 12.0 + 470.0).min(content.x1), tr_rect.y1);
            self.draw_check_row(
                scene,
                tr,
                theme,
                t,
                ack,
                self.system_ack,
                c.semantic.danger,
                "I understand — erasing my system disk destroys the running OS",
            );
        }
    }

    /// A labelled checkbox row (box + tick + caption) used by the Drive step's
    /// reveal + acknowledgement gates. `accent` tints the checked box.
    #[allow(clippy::too_many_arguments)]
    fn draw_check_row(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        row: Rect,
        checked: bool,
        accent: Color,
        label: &str,
    ) {
        let c = &theme.tokens.colors;
        let hov = row.contains(self.last_mouse);
        Self::fill_rr(scene, t, row, 10.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, row, 10.0, if checked { with_alpha(accent, 170) } else { c.border_standard }, 1.0);
        let box_r = Rect::new(row.x0 + 12.0, row.y0 + 9.0, row.x0 + 12.0 + 16.0, row.y0 + 9.0 + 16.0);
        Self::fill_rr(scene, t, box_r, 4.0, if checked { accent } else { c.neutral[4] });
        if checked {
            Self::check_disc(scene, t, Point::new((box_r.x0 + box_r.x1) / 2.0, (box_r.y0 + box_r.y1) / 2.0), 8.0, accent, Color::WHITE);
        }
        self.txt(scene, tr, label, 12.5, c.text_primary, box_r.x1 + 10.0, row.y0 + 9.0, Some((row.width() - 40.0) as f32), TextWeight::Medium);
    }

    fn draw_drive_empty(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, content: Rect) {
        let c = &theme.tokens.colors;
        let top = Self::drive_cards_top(content) + 6.0;
        let card = Rect::new(content.x0, top, content.x1, top + 240.0);
        Self::fill_rr(scene, t, card, 16.0, c.surfaces.raised);
        Self::dashed_rr(scene, t, card, 16.0, c.border_standard, 1.4);
        let cx = (card.x0 + card.x1) / 2.0;
        // A drive glyph in a soft disc.
        let gcy = card.y0 + 74.0;
        scene.fill(Fill::NonZero, t, c.neutral[2], None, &Circle::new(Point::new(cx, gcy), 34.0));
        let gr = Rect::new(cx - 18.0, gcy - 18.0, cx + 18.0, gcy + 18.0);
        Icon::HardDrive.draw_with_stroke(scene, t, gr, c.text_dim, 1.8);
        self.txt_center(scene, tr, "Insert a drive", 17.0, c.text_primary, cx, card.y0 + 140.0, TextWeight::Semibold);
        self.txt_center(scene, tr, "Plug in a USB stick or SD card, then press Rescan.", 13.0, c.text_dim, cx, card.y0 + 168.0, TextWeight::Regular);
        let hint = if self.show_internal {
            "No drives detected."
        } else {
            "No removable drives detected — enable “Show internal disks” to target a fixed disk."
        };
        self.txt_center(scene, tr, hint, 12.0, c.neutral[6], cx, card.y0 + 196.0, TextWeight::Regular);
    }

    fn draw_drive_card(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, r: Rect, idx: usize, drive: &TargetDrive) {
        let c = &theme.tokens.colors;
        let selected = self.selected == Some(idx);
        let hover = self.hover_target == Some(idx) && !selected;
        let danger = !drive.removable;
        let bg = if selected { with_alpha(c.accent[5], 30) } else if hover { c.neutral[2] } else { c.surfaces.raised };
        Self::fill_rr(scene, t, r, 14.0, bg);
        let border = if selected {
            c.accent[5]
        } else if danger {
            with_alpha(c.semantic.danger, 150)
        } else {
            c.border_standard
        };
        Self::stroke_rr(scene, t, r, 14.0, border, if selected { 2.0 } else { 1.0 });

        // Leading device-type tile, tinted by kind, with a drive glyph.
        let cyc = (r.y0 + r.y1) / 2.0;
        let kc = kind_color(theme, drive.kind);
        let tile = Rect::new(r.x0 + 18.0, cyc - 26.0, r.x0 + 70.0, cyc + 26.0);
        Self::fill_rr(scene, t, tile, 12.0, with_alpha(kc, 40));
        let gi = Rect::new(tile.x0 + 13.0, tile.y0 + 13.0, tile.x1 - 13.0, tile.y1 - 13.0);
        Icon::HardDrive.draw_with_stroke(scene, t, gi, kc, 1.9);
        if selected {
            Self::check_disc(scene, t, Point::new(tile.x1 - 1.0, tile.y0 + 1.0), 9.0, c.accent[5], Color::WHITE);
        }
        let left = tile.x1 + 16.0;

        // Removable / fixed / system chip on the right.
        let (chip_label, chip_c) = if drive.is_system {
            ("SYSTEM DISK", c.semantic.danger)
        } else if drive.removable {
            ("REMOVABLE", c.semantic.success)
        } else {
            ("INTERNAL DISK", c.semantic.warning)
        };
        let (cw, _) = tr.measure_weighted(chip_label, 10.5, None, TextWeight::Bold);
        let chip = Rect::new(r.x1 - 16.0 - cw as f64 - 18.0, r.y0 + 18.0, r.x1 - 16.0, r.y0 + 18.0 + 24.0);
        Self::fill_rr(scene, t, chip, 6.0, with_alpha(chip_c, 40));
        self.txt_center(scene, tr, chip_label, 10.5, chip_c, (chip.x0 + chip.x1) / 2.0, chip.y0 + 6.0, TextWeight::Bold);

        // Model (single-line elided so it never wraps into the meta) + a meta
        // line: kind · device · capacity.
        let model_max = (chip.x0 - left - 12.0).max(40.0) as f32;
        let model = tr.elide_to_width_weighted(&drive.model, 16.0, model_max, TextWeight::Semibold);
        self.txt(scene, tr, &model, 16.0, c.text_primary, left, r.y0 + 24.0, None, TextWeight::Semibold);
        let meta = format!("{}  ·  {}  ·  {}", drive.kind.tag(), drive.dev_path(), fmt_size(drive.size));
        let meta = tr.elide_to_width_weighted(&meta, 12.5, (r.x1 - 16.0 - left) as f32, TextWeight::Regular);
        self.txt(scene, tr, &meta, 12.5, c.text_dim, left, r.y0 + 48.0, None, TextWeight::Regular);

        // Destructive line — or, for a not-yet-acknowledged system disk, a
        // clear "locked" note pointing at the acknowledgement gate.
        let icr = Rect::new(left, r.y1 - 28.0, left + 15.0, r.y1 - 13.0);
        let (line, col) = if drive.is_system && !self.system_ack {
            Icon::Shield.draw_with_stroke(scene, t, icr, c.text_dim, 1.6);
            ("Locked — this is your system disk. Acknowledge below to select it.".to_string(), c.text_dim)
        } else {
            Self::warn_mark(scene, t, Rect::new(left, r.y1 - 28.0, left + 16.0, r.y1 - 12.0), c.semantic.danger);
            (format!("Erases {}", drive.contents_summary()), c.semantic.danger)
        };
        let line = tr.elide_to_width_weighted(&line, 11.5, (r.x1 - 12.0 - (left + 22.0)) as f32, TextWeight::Medium);
        self.txt(scene, tr, &line, 11.5, col, left + 22.0, r.y1 - 26.0, None, TextWeight::Medium);
    }

    fn draw_customize_step(&mut self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        if !self.customizable() {
            self.draw_step_title(scene, tr, theme, content, "Customize (optional)", "");
            let card = Rect::new(content.x0, content.y0 + 60.0, content.x1, content.y0 + 60.0 + 150.0);
            Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
            self.txt_center(scene, tr, "This image is written unchanged", 16.0, c.text_primary, (card.x0 + card.x1) / 2.0, card.y0 + 46.0, TextWeight::Semibold);
            self.txt_center(scene, tr, "Pre-configuration (hostname, user, region) is available for qOS", 13.0, c.text_dim, (card.x0 + card.x1) / 2.0, card.y0 + 76.0, TextWeight::Regular);
            self.txt_center(scene, tr, "images. Press Continue to write it as-is.", 13.0, c.text_dim, (card.x0 + card.x1) / 2.0, card.y0 + 96.0, TextWeight::Regular);
            return;
        }
        let installing = self.action == Action::Install;
        let sub = if installing {
            "Set up the machine qOS will install — you can change any of this later."
        } else {
            "Pre-configure the written media so it boots ready to use."
        };
        self.draw_step_title(scene, tr, theme, content, "Customize (optional)", sub);

        // The whole form is the shared guided-customization panel.
        let removable = self.target().map(|d| d.removable).unwrap_or(false);
        self.customize.draw(scene, t, tr, theme, presence, content, &self.profile, removable);
    }

    fn draw_confirm_step(&mut self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        let g = Self::confirm_geom(content);
        let (title, sub) = match self.action {
            Action::Install => (
                "Confirm & install",
                "Review the install, then type the drive's name to unlock it.",
            ),
            Action::Flash => (
                "Confirm & write",
                "Review the write, then type the drive's name to unlock it.",
            ),
        };
        self.draw_step_title(scene, tr, theme, content, title, sub);

        // Review card: Image → Drive.
        let card = g.card;
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
        let row = |this: &Self, scene: &mut Scene, tr: &mut TextRenderer, y: f64, k: &str, v: String, ok: bool| {
            this.txt(scene, tr, k, 12.0, c.text_dim, card.x0 + 20.0, y, None, TextWeight::Semibold);
            let col = if ok { c.text_primary } else { c.semantic.danger };
            this.txt(scene, tr, &v, 14.0, col, card.x0 + 100.0, y - 1.0, Some((card.width() - 120.0) as f32), TextWeight::Semibold);
        };
        let src_v = self.source.as_ref().map(|s| format!("{}   ({})", s.name, fmt_size(s.size))).unwrap_or_else(|| "—".into());
        row(self, scene, tr, card.y0 + 24.0, "Image", src_v, self.source.is_some());
        let tgt_v = self.target().map(|d| format!("{} · {} · {}", d.model, d.dev_path(), fmt_size(d.size))).unwrap_or_else(|| "—".into());
        row(self, scene, tr, card.y0 + 58.0, "Drive", tgt_v, self.target().is_some());
        // Fit line + mode.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let fit = if tgt.fits(src.size) {
                (format!("fits — {} free afterwards", fmt_size(tgt.size.saturating_sub(src.size))), c.semantic.success)
            } else {
                ("image is larger than the drive".into(), c.semantic.danger)
            };
            self.txt(scene, tr, "Capacity", 12.0, c.text_dim, card.x0 + 20.0, card.y0 + 92.0, None, TextWeight::Semibold);
            self.txt(scene, tr, &fit.0, 13.0, fit.1, card.x0 + 100.0, card.y0 + 91.0, None, TextWeight::Medium);
        }
        // Action summary.
        let act = match self.action {
            Action::Install => format!(
                "Install qOS with your settings (host {}, user {})",
                self.profile.hostname, self.profile.user.username
            ),
            Action::Flash if self.customizable() => format!(
                "Write qOS, pre-configured (host {}, user {})",
                self.profile.hostname, self.profile.user.username
            ),
            Action::Flash => "Write the image, then read-back verify".to_string(),
        };
        self.txt(scene, tr, "Action", 12.0, c.text_dim, card.x0 + 20.0, card.y0 + 122.0, None, TextWeight::Semibold);
        self.txt(scene, tr, &act, 13.0, c.text_primary, card.x0 + 100.0, card.y0 + 121.0, Some((card.width() - 120.0) as f32), TextWeight::Medium);

        // Danger banner — louder for a fixed/internal (install) target.
        let internal = self.target().map(|d| !d.removable).unwrap_or(false);
        let system = self.target().map(|d| d.is_system).unwrap_or(false);
        let banner = g.banner;
        Self::fill_rr(scene, t, banner, 11.0, with_alpha(c.semantic.danger, if internal { 40 } else { 28 }));
        Self::stroke_rr(scene, t, banner, 11.0, with_alpha(c.semantic.danger, if internal { 170 } else { 130 }), if internal { 1.4 } else { 1.0 });
        let wr = Rect::new(banner.x0 + 16.0, banner.y0 + (banner.height() - 20.0) / 2.0, banner.x0 + 36.0, banner.y0 + (banner.height() - 20.0) / 2.0 + 20.0);
        Self::warn_mark(scene, t, wr, c.semantic.danger);
        let warn = if system {
            "This is your system disk. Everything on it — the running OS included — is erased. This cannot be undone."
        } else if internal {
            "This erases the entire internal disk. This cannot be undone."
        } else {
            "This permanently erases the drive. This cannot be undone."
        };
        self.txt(scene, tr, warn, 13.0, c.semantic.danger, banner.x0 + 48.0, banner.y0 + (banner.height() - 16.0) / 2.0, Some((banner.width() - 64.0) as f32), TextWeight::Semibold);

        // Typed confirmation field — a real shared TextInput (caret, selection,
        // clipboard and a11y for free), the box chrome drawn here around it.
        let field = g.field;
        let focused = self.confirm_focused;
        let matches = self.confirm_matches();
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        self.txt(scene, tr, "CONFIRM THE TARGET DEVICE", 11.0, c.text_dim, field.x0 + 2.0, field.y0 - 18.0, None, TextWeight::Semibold);
        Self::fill_rr(scene, t, field, 11.0, c.neutral[2]);
        Self::stroke_rr(scene, t, field, 11.0, if matches { c.semantic.success } else if focused { c.border_focus } else { with_alpha(c.semantic.danger, 160) }, if focused || matches { 1.6 } else { 1.2 });
        let armed_w = if matches { 84.0 } else { 0.0 };
        let inner = Rect::new(field.x0 + 14.0, field.y0, field.x1 - 16.0 - armed_w, field.y1);
        self.confirm_input.set_placeholder(&format!("type “{want}” to confirm"));
        self.confirm_input.set_focus(focused);
        self.confirm_input.apply_layout_external(inner);
        Widget::draw(&mut self.confirm_input, scene, t, tr, theme, presence);
        if matches {
            let cy = field.y0 + (field.height() - 16.0) / 2.0;
            let ic = Rect::new(field.x1 - 78.0, cy, field.x1 - 62.0, cy + 16.0);
            Icon::Check.draw_with_stroke(scene, t, ic, c.semantic.success, 2.0);
            self.txt(scene, tr, "armed", 12.5, c.semantic.success, field.x1 - 58.0, field.y0 + (field.height() - 15.0) / 2.0, None, TextWeight::Bold);
        }
        // Gate note.
        let note = if crate::write::allow_write() {
            "Live write enabled — the write is real and irreversible."
        } else {
            "Safe mode — no bytes are written until live-write support is enabled."
        };
        self.txt(scene, tr, note, 11.0, c.neutral[6], field.x0 + 2.0, field.y1 + 8.0, None, TextWeight::Regular);
    }

    fn draw_write_progress(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let verb_ing = match self.action {
            Action::Install => "Installing",
            Action::Flash => "Writing",
        };
        let title = if writing { format!("{verb_ing}…") } else { "Verifying…".to_string() };
        self.draw_step_title(scene, tr, theme, content, &title, "");

        // Big overall ring.
        let cxp = (content.x0 + content.x1) / 2.0;
        let cyp = content.y0 + 150.0;
        let radius = 76.0;
        let overall = p.overall();
        scene.stroke(&Stroke::new(12.0), t, c.neutral[3], None, &Circle::new(Point::new(cxp, cyp), radius));
        if overall > 0.0 {
            let mut arc = BezPath::new();
            let start = -std::f64::consts::FRAC_PI_2;
            let sweep = overall * std::f64::consts::TAU;
            let steps = (sweep.abs() / 0.08).ceil().max(1.0) as usize;
            arc.move_to((cxp + radius * start.cos(), cyp + radius * start.sin()));
            for i in 1..=steps {
                let a = start + sweep * (i as f64 / steps as f64);
                arc.line_to((cxp + radius * a.cos(), cyp + radius * a.sin()));
            }
            let ring_c = if writing { c.semantic.info } else { c.accent[6] };
            scene.stroke(&Stroke::new(12.0), t, ring_c, None, &arc);
        }
        self.txt_center(scene, tr, &format!("{:.0}%", overall * 100.0), 30.0, c.text_primary, cxp, cyp - 22.0, TextWeight::Bold);
        let caption = if writing { verb_ing.to_ascii_lowercase() } else { "verifying".to_string() };
        self.txt_center(scene, tr, &caption, 12.0, c.text_dim, cxp, cyp + 14.0, TextWeight::Medium);

        // Rate + ETA line.
        let mut meta = String::new();
        if p.rate_bps > 0 {
            meta.push_str(&fmt_rate(p.rate_bps));
        }
        if let Some(src) = &self.source {
            if let Some(eta) = p.eta_secs(src.size) {
                if !meta.is_empty() {
                    meta.push_str("   ·   ");
                }
                meta.push_str(&format!("~{} left", fmt_eta(eta)));
            }
        }
        if !meta.is_empty() {
            self.txt_center(scene, tr, &meta, 13.0, c.text_dim, cxp, cyp + radius + 20.0, TextWeight::Medium);
        }

        // Two phase bars.
        let bx0 = content.x0 + 40.0;
        let bx1 = content.x1 - 40.0;
        let mut yy = cyp + radius + 56.0;
        for (label, frac, active, fill) in [
            ("Write", p.write_frac, writing, c.semantic.info),
            ("Verify", p.verify_frac, !writing, c.semantic.success),
        ] {
            self.txt(scene, tr, label, 12.0, if active { c.text_primary } else { c.text_dim }, bx0, yy, None, TextWeight::Semibold);
            self.txt_right(scene, tr, &format!("{:.0}%", frac * 100.0), 12.0, c.text_dim, bx1, yy, TextWeight::Regular);
            let bar = Rect::new(bx0, yy + 20.0, bx1, yy + 20.0 + 10.0);
            Self::bar(scene, t, bar, frac, c.neutral[3], fill);
            yy += 48.0;
        }
        // Image → drive line.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            self.txt_center(scene, tr, &format!("{}  →  {}", src.name, tgt.dev_path()), 12.0, c.neutral[6], cxp, yy + 4.0, TextWeight::Regular);
        }
    }

    fn draw_write_done(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout, ok: bool) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        let cxp = (content.x0 + content.x1) / 2.0;
        let cyp = content.y0 + 120.0;
        let install = self.action == Action::Install;
        if ok {
            Self::check_disc(scene, t, Point::new(cxp, cyp), 46.0, c.semantic.success, Color::WHITE);
            let (headline, sub): (&str, &str) = if install {
                ("qOS is installed", "The disk was written and verified — remove the media and reboot.")
            } else {
                ("Drive is ready", "The image was written and verified — you can safely remove the drive.")
            };
            self.txt_center(scene, tr, headline, 26.0, c.text_primary, cxp, cyp + 66.0, TextWeight::Bold);
            self.txt_center(scene, tr, sub, 14.0, c.text_dim, cxp, cyp + 100.0, TextWeight::Regular);
        } else {
            // Danger disc with a warning mark.
            scene.fill(Fill::NonZero, t, c.semantic.danger, None, &Circle::new(Point::new(cxp, cyp), 46.0));
            Self::warn_mark(scene, t, Rect::new(cxp - 24.0, cyp - 22.0, cxp + 24.0, cyp + 26.0), Color::WHITE);
            let headline = if install { "Install failed" } else { "Write failed" };
            self.txt_center(scene, tr, headline, 26.0, c.text_primary, cxp, cyp + 66.0, TextWeight::Bold);
            self.txt_center(scene, tr, "The drive was not changed. See the message below.", 14.0, c.text_dim, cxp, cyp + 100.0, TextWeight::Regular);
        }
        // Summary card.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let card = Rect::new(content.x0 + 80.0, cyp + 140.0, content.x1 - 80.0, cyp + 140.0 + 84.0);
            Self::fill_rr(scene, t, card, 12.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, card, 12.0, c.border_standard, 1.0);
            self.txt(scene, tr, "Image", 12.0, c.text_dim, card.x0 + 18.0, card.y0 + 18.0, None, TextWeight::Semibold);
            self.txt(scene, tr, &src.name, 13.5, c.text_primary, card.x0 + 90.0, card.y0 + 17.0, Some((card.width() - 110.0) as f32), TextWeight::Medium);
            self.txt(scene, tr, "Drive", 12.0, c.text_dim, card.x0 + 18.0, card.y0 + 48.0, None, TextWeight::Semibold);
            self.txt(scene, tr, &format!("{} · {}", tgt.model, tgt.dev_path()), 13.5, c.text_primary, card.x0 + 90.0, card.y0 + 47.0, Some((card.width() - 110.0) as f32), TextWeight::Medium);
            if ok {
                self.txt_right(scene, tr, "✓ verified", 12.5, c.semantic.success, card.x1 - 18.0, card.y0 + 18.0, TextWeight::Bold);
            }
        }
    }
}

// =========================================================================
// Interaction / state mutation
// =========================================================================
impl ImagerApp {
    fn select_source(&mut self, state: &mut AutoCommit, src: Source) {
        if let Some(prev) = &self.source {
            if !prev.path.is_empty() && !self.recent.iter().any(|s| s.path == prev.path) {
                self.recent.insert(0, prev.clone());
                self.recent.truncate(6);
                ImagerDoc::set_recent(state, &self.recent);
            }
        }
        // A non-installable source cannot carry a pre-config profile.
        if !src.installable {
            self.action = Action::Flash;
            ImagerDoc::set_action(state, Action::Flash);
        }
        self.source = Some(src.clone());
        ImagerDoc::set_source(state, Some(&src));
        self.set_status(state, &format!("Selected {}", src.name));
        self.dirty = true;
    }

    fn open_file_picker(&mut self, state: &mut AutoCommit) {
        self.portal_rx = Some(
            PickRequest::open("Choose a disk image", "Choose")
                .filter(
                    FileFilter::new("Disk images")
                        .glob("*.iso")
                        .glob("*.img")
                        .glob("*.raw")
                        .mimetype("application/x-iso9660-image"),
                )
                .thread_name("imager-portal")
                .send(),
        );
        self.set_status(state, "Opening the file picker…");
        self.dirty = true;
    }

    /// Drain a pending portal result, if any. Called each `sync`.
    fn drain_portal(&mut self, state: &mut AutoCommit) {
        let Some(rx) = &self.portal_rx else { return };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                return;
            }
        };
        self.portal_rx = None;
        match outcome {
            PortalOutcome::Opened(path) => match crate::probe::inspect_source(&path) {
                Some(src) => self.select_source(state, src),
                None => self.set_status(state, "That file is not a readable disk image."),
            },
            PortalOutcome::Cancelled => self.set_status(state, "File selection cancelled."),
            PortalOutcome::Failed(reason) => self.set_status(state, &format!("File picker unavailable: {reason}")),
        }
        self.dirty = true;
    }

    fn select_target(&mut self, state: &mut AutoCommit, idx: usize) {
        let Some(drive) = self.targets.get(idx) else { return };
        // Hard guard: the system disk (the one the running OS boots from) can
        // never be selected by a stray click. It only becomes selectable after
        // the user ticks the explicit acknowledgement below the list.
        if drive.is_system && !self.system_ack {
            self.set_status(
                state,
                "That is your system disk — tick “I understand” below to select it.",
            );
            self.dirty = true;
            return;
        }
        self.selected = Some(idx);
        ImagerDoc::set_selected(state, Some(idx));
        self.confirm.clear();
        ImagerDoc::set_confirm(state, "");
        self.dirty = true;
    }

    fn set_status(&mut self, state: &mut AutoCommit, s: &str) {
        self.status = s.to_string();
        ImagerDoc::set_status(state, s);
    }

    fn goto(&mut self, state: &mut AutoCommit, s: Step) {
        self.step = s;
        ImagerDoc::set_step(state, s);
        self.confirm_focused = false;
        self.customize.clear_focus();
        self.dirty = true;
    }

    fn advance(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Source if self.can_advance(Step::Source) => self.goto(state, Step::Drive),
            Step::Drive if self.can_advance(Step::Drive) => {
                // Record whether this is a write or an install (drives the
                // naming), then route: a qOS image goes through Customize; a
                // plain ISO skips straight to the Confirm review.
                let act = self.intended_action();
                self.action = act;
                ImagerDoc::set_action(state, act);
                if self.customizable() {
                    self.goto(state, Step::Customize);
                } else {
                    self.goto(state, Step::Confirm);
                }
            }
            Step::Customize => self.goto(state, Step::Confirm),
            // Confirm advances into the write only when the typed-name gate is
            // armed; that same gesture kicks off the (external) job.
            Step::Confirm if self.can_advance(Step::Confirm) => self.start_write(state),
            _ => {}
        }
    }

    fn back(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Confirm if self.customizable() => self.goto(state, Step::Customize),
            Step::Confirm => self.goto(state, Step::Drive),
            Step::Customize => self.goto(state, Step::Drive),
            Step::Drive => self.goto(state, Step::Source),
            // No going back from the source, an in-flight write, or the outcome.
            Step::Source | Step::Write | Step::Done => {}
        }
    }

    fn start_write(&mut self, state: &mut AutoCommit) {
        let (Some(src), Some(tgt)) = (self.source.clone(), self.target().cloned()) else {
            self.set_status(state, "Select a source and drive first.");
            self.dirty = true;
            return;
        };
        if !self.confirm_matches() {
            self.set_status(state, "Type the exact device name to confirm.");
            self.dirty = true;
            return;
        }
        let req = WriteRequest { source: src, target: tgt.clone(), action: self.action, profile: self.profile.clone() };
        let (start, rx) = spawn_job(req, &self.confirm);
        match start {
            crate::write::JobStart::Started => {
                self.job_rx = rx;
                self.progress = Progress { phase: JobPhase::Writing, write_frac: 0.0, verify_frac: 0.0, rate_bps: 0 };
                ImagerDoc::set_progress(state, &self.progress);
                let mode = if crate::write::allow_write() { "writing" } else { "simulated write" };
                self.set_status(state, &format!("{} → {} ({mode})", self.source.as_ref().map(|s| s.name.clone()).unwrap_or_default(), tgt.dev_path()));
                // The write has begun: move onto the live-progress station.
                self.goto(state, Step::Write);
            }
            crate::write::JobStart::ConfirmationMismatch => self.set_status(state, "Confirmation does not match — write refused."),
            crate::write::JobStart::TooSmall => self.set_status(state, "Image is larger than the drive — write refused."),
            crate::write::JobStart::Incomplete => self.set_status(state, "Select a source and drive before writing."),
        }
        self.dirty = true;
    }

    /// Drain live job progress, folding it into the doc. Called each `sync`.
    fn drain_job(&mut self, state: &mut AutoCommit) {
        let Some(rx) = &self.job_rx else { return };
        let mut latest = None;
        loop {
            match rx.try_recv() {
                Ok(p) => latest = Some(p),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // The worker finished; if we never saw a terminal phase,
                    // treat a non-success as a failure.
                    if self.progress.phase.is_active() {
                        self.progress.phase = JobPhase::Failed;
                        ImagerDoc::set_progress(state, &self.progress);
                        self.set_status(state, "Write ended unexpectedly.");
                        self.goto(state, Step::Done);
                    }
                    self.job_rx = None;
                    self.dirty = true;
                    return;
                }
            }
        }
        if let Some(p) = latest {
            self.progress = p;
            ImagerDoc::set_progress(state, &p);
            self.dirty = true;
            if matches!(p.phase, JobPhase::Success | JobPhase::Failed) {
                self.job_rx = None;
                let name = self.source.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                let msg = if p.phase == JobPhase::Success {
                    format!("Done — {name} {} and verified", self.action.past())
                } else {
                    format!("{} failed.", self.action.title())
                };
                self.set_status(state, &msg);
                // Terminal phase → show the outcome station.
                self.goto(state, Step::Done);
            }
        }
    }

    fn write_another(&mut self, state: &mut AutoCommit) {
        self.progress = Progress::default();
        ImagerDoc::set_progress(state, &self.progress);
        self.confirm.clear();
        ImagerDoc::set_confirm(state, "");
        self.goto(state, Step::Source);
        self.set_status(state, "");
    }

    fn export_config(&mut self, _state: &mut AutoCommit) {
        self.export_text = Some(self.profile.to_headless_config(self.source.as_ref(), self.target()));
        self.dirty = true;
    }

    /// The inner text rect of the Confirm step's typed-name field for the
    /// current viewport — shared by draw and event routing so the caret and the
    /// text land in the same place.
    fn confirm_input_inner(&self) -> Rect {
        let field = Self::confirm_geom(self.content_rect()).field;
        let armed_w = if self.confirm_matches() { 84.0 } else { 0.0 };
        Rect::new(field.x0 + 14.0, field.y0, field.x1 - 16.0 - armed_w, field.y1)
    }

    fn do_rescan(&mut self, state: &mut AutoCommit) {
        if live_probe_enabled() {
            // Re-read the live block-device inventory. `probe()` returns `None`
            // only when `lsblk` is unavailable — a transient failure — so in
            // that case we keep the last-known list rather than blanking it.
            // A real (possibly empty) result is authoritative and replaces it,
            // so unplugging the selected drive genuinely drops it from the list.
            let remembered = self.target().map(|d| d.name.clone());
            if let Some(t) = crate::probe::probe() {
                self.targets = t.clone();
                ImagerDoc::write_targets(state, &t);
                // Keep the same physical drive selected across a rescan by name,
                // not by index (indices shift as drives come and go).
                self.selected = remembered.and_then(|name| {
                    self.targets.iter().position(|d| d.name == name)
                });
            }
            let n = self.targets.len();
            let removable = self.removable_count();
            self.set_status(
                state,
                &format!("Rescanned — {removable} removable, {n} total"),
            );
        } else {
            self.set_status(state, "Rescanned drives");
        }
        self.selected = self.selected.filter(|i| *i < self.targets.len());
        ImagerDoc::set_selected(state, self.selected);
        // A vanished/changed inventory invalidates any system-disk acknowledgement.
        self.system_ack = false;
        self.dirty = true;
    }

    fn dispatch_hit(&mut self, state: &mut AutoCommit, hit: Hit) {
        match hit {
            Hit::PickFile => self.open_file_picker(state),
            Hit::Release(i) => {
                if let Some(rel) = self.releases().get(i) {
                    self.select_source(state, rel.to_source());
                }
            }
            Hit::Recent(i) => {
                if let Some(src) = self.recent.get(i).cloned() {
                    self.select_source(state, src);
                }
            }
            Hit::Target(i) => self.select_target(state, i),
            Hit::ToggleInternal => {
                self.show_internal = !self.show_internal;
                ImagerDoc::set_show_internal(state, self.show_internal);
                // Hiding internal disks again also withdraws the system-disk
                // acknowledgement, so re-revealing them re-arms the guard.
                if !self.show_internal {
                    self.system_ack = false;
                }
                self.dirty = true;
            }
            Hit::ToggleSystemAck => {
                self.system_ack = !self.system_ack;
                if !self.system_ack {
                    // Withdrawing consent deselects the system disk if it was chosen.
                    if self.target().map(|d| d.is_system).unwrap_or(false) {
                        self.selected = None;
                        ImagerDoc::set_selected(state, None);
                    }
                }
                self.dirty = true;
            }
            Hit::Rescan => self.do_rescan(state),
            Hit::ConfirmField => {
                self.confirm_focused = true;
                self.customize.clear_focus();
                self.confirm_input.set_focus(true);
                self.confirm_input.apply_layout_external(self.confirm_input_inner());
                self.dirty = true;
            }
            Hit::StepJump(s) => {
                if self.crumb_reachable(s) {
                    self.goto(state, s);
                }
            }
            Hit::Back => self.back(state),
            Hit::Next => self.advance(state),
            Hit::StartWrite => self.start_write(state),
            Hit::WriteAnother => self.write_another(state),
        }
    }
}

// =========================================================================
// Widget impl
// =========================================================================
impl Widget for ImagerApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if ImagerDoc::is_empty(state) {
                // Start genuinely blank. In the live app a real `lsblk` probe
                // fills the drive list; if it finds nothing (or is unavailable)
                // the list stays empty and the Drive step shows its honest
                // "insert a drive" state — the imager never invents drives.
                // Sample content is opt-in (`--demo` / `--shots`), which seeds
                // the document *before* the first sync so this branch is skipped.
                let targets = if live_probe_enabled() {
                    crate::probe::probe().unwrap_or_default()
                } else {
                    Vec::new()
                };
                ImagerDoc::seed(state, &targets);
            }
            self.seeded = true;
        }

        // Fold in any async work before reading the doc back.
        self.drain_portal(state);
        self.drain_job(state);

        self.targets = ImagerDoc::targets(state);
        self.source = ImagerDoc::source(state);
        self.recent = ImagerDoc::recent(state);
        self.profile = ImagerDoc::profile(state);
        self.progress = ImagerDoc::progress(state);
        self.action = ImagerDoc::action(state);
        self.confirm = ImagerDoc::confirm(state);
        self.status = ImagerDoc::status(state);
        self.step = ImagerDoc::step(state);
        self.show_internal = ImagerDoc::show_internal(state);
        self.selected = ImagerDoc::selected(state).filter(|i| *i < self.targets.len());

        // Refresh the shared customization panel's fields from the profile
        // (each unfocused field mirrors the stored value; a focused one stays
        // the source of truth).
        self.customize.sync(&self.profile);
        // Mirror the armed device-name into the confirm field's TextInput while
        // it is not being edited; a focused input stays the source of truth and
        // writes back through the event path.
        let confirm_editing = self.confirm_focused && self.step == Step::Confirm;
        self.confirm_input.set_focus(confirm_editing);
        if !confirm_editing {
            self.confirm_input.set_text(&self.confirm);
        }
        self.dirty = true;
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size { width: percent(1.0_f32), height: percent(1.0_f32) },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);
        let lay = self.compute_layout();
        let tr = text_renderer;

        self.draw_header(scene, tr, theme, transform, &lay);
        match self.step {
            Step::Source => self.draw_source_step(scene, tr, theme, transform, &lay),
            Step::Drive => self.draw_drive_step(scene, tr, theme, transform, &lay),
            Step::Customize => self.draw_customize_step(scene, tr, theme, transform, &lay, presence),
            Step::Confirm => self.draw_confirm_step(scene, tr, theme, transform, &lay, presence),
            Step::Write => self.draw_write_progress(scene, tr, theme, transform, &lay),
            Step::Done => self.draw_write_done(scene, tr, theme, transform, &lay, self.progress.phase == JobPhase::Success),
        }
        self.draw_footer(scene, tr, theme, transform, &lay);

        if let Some(text) = self.export_text.clone() {
            self.draw_export(scene, tr, theme, transform, &text);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton};
        use winit::keyboard::{Key, NamedKey};

        let lay = self.compute_layout();

        // The Customize step is delegated in full to the shared customization
        // panel, which owns its fields, toggles and export affordance. Only
        // events it does not consume (footer clicks, global keys) fall through
        // to the wizard chrome below.
        if self.step == Step::Customize && self.customizable() && self.export_text.is_none() {
            let removable = self.target().map(|d| d.removable).unwrap_or(false);
            let profile = self.profile.clone();
            let resp = self.customize.handle_event(
                event, scale_factor, modifiers, state, path, &mut self.actions, &profile, removable,
                lay.content, node_id, taffy,
            );
            if resp.export_requested {
                self.export_config(state);
            }
            if resp.handled {
                *handled = true;
                self.dirty = true;
                return;
            }
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                // Which drive card (if any) is under the pointer, resolved from
                // this frame's hit-rects.
                let hover_now = lay.hits.iter().find_map(|(r, h)| match h {
                    Hit::Target(i) if r.contains(p) => Some(*i),
                    _ => None,
                });
                self.hover_target = hover_now;
                // Forward pointer moves to the focused confirm field so a drag
                // extends its selection like any other text input.
                if self.confirm_focused && self.step == Step::Confirm {
                    self.confirm_input.apply_layout_external(self.confirm_input_inner());
                    let (mut h, mut ft) = (false, None);
                    self.confirm_input.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut ft, node_id, taffy);
                }
                // Repaint on move so buttons/cards light up under the pointer.
                self.dirty = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let p = self.last_mouse;
                if self.export_text.is_some() {
                    self.export_text = None;
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                let hit = lay.hits.iter().find(|(r, _)| r.contains(p)).map(|(_, h)| h.clone());
                if let Some(hit) = hit {
                    let is_confirm_field = matches!(hit, Hit::ConfirmField);
                    self.dispatch_hit(state, hit);
                    // Forward the press to the now-focused confirm input so the
                    // caret lands where the user clicked (and a drag can begin).
                    if is_confirm_field {
                        self.confirm_input.apply_layout_external(self.confirm_input_inner());
                        let (mut h, mut ft) = (false, None);
                        self.confirm_input.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut ft, node_id, taffy);
                    }
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // A click on empty space drops the confirm field's focus.
                self.confirm_focused = false;
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => {
                if self.export_text.is_some() {
                    if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                        self.export_text = None;
                        *handled = true;
                        self.dirty = true;
                    }
                    return;
                }
                // The Confirm step's typed device-name gate — routed into the
                // shared TextInput (a transient arming gesture, not a profile
                // edit). Enter arms & starts the write; Escape blurs.
                if self.confirm_focused && self.step == Step::Confirm {
                    match logical_key {
                        Key::Named(NamedKey::Enter) => {
                            if self.can_advance(Step::Confirm) {
                                self.start_write(state);
                            }
                            self.confirm_focused = false;
                            *handled = true;
                            self.dirty = true;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.confirm_focused = false;
                            *handled = true;
                            self.dirty = true;
                        }
                        _ => {
                            // Feed the keystroke to the real input, then mirror
                            // its value into the armed-name gate + the document.
                            let inner = self.confirm_input_inner();
                            self.confirm_input.set_focus(true);
                            self.confirm_input.apply_layout_external(inner);
                            let (mut h, mut ft) = (false, None);
                            self.confirm_input.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut ft, node_id, taffy);
                            let val = self.confirm_input.value().to_string();
                            self.confirm = val.clone();
                            ImagerDoc::set_confirm(state, &val);
                            *handled = true;
                            self.dirty = true;
                        }
                    }
                    return;
                }
                // Global shortcuts.
                match logical_key {
                    Key::Named(NamedKey::Enter) => {
                        self.advance(state);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.back(state);
                        *handled = true;
                    }
                    Key::Character(s) if matches!(s.as_str(), "r" | "R") && self.step == Step::Drive => {
                        self.do_rescan(state);
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// The resource tree the shell's Ctrl+K palette walks — delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`]. Empty unless an installable qOS image is selected.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// The framework host drains the undo boundary an on-screen profile control
    /// dispatched, so it records exactly one boundary bracketing it — the same
    /// path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn name(&self) -> &str {
        "ImagerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Keep the event loop ticking while a job or file picker is in flight, so
    /// the progress bars animate and the portal result is drained promptly.
    fn needs_animation_frame(&self) -> bool {
        self.job_rx.is_some() || self.portal_rx.is_some()
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
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

// ---- overlay: exported config ------------------------------------------
impl ImagerApp {
    fn draw_export(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, text: &str) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let w = (self.rect.width() * 0.62).min(640.0);
        let h = (self.rect.height() * 0.8).min(620.0);
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
        self.txt(scene, tr, "Unattended install config", 18.0, c.text_primary, card.x0 + 20.0, card.y0 + 16.0, None, TextWeight::Bold);
        self.txt(scene, tr, "qos-install.conf — the same options this wizard sets; secrets are referenced, not embedded.", 11.5, c.text_dim, card.x0 + 20.0, card.y0 + 42.0, Some((card.width() - 40.0) as f32), TextWeight::Regular);
        let mut y = card.y0 + 70.0;
        for line in text.lines() {
            if y > card.y1 - 56.0 {
                break;
            }
            let col = if line.starts_with('#') {
                c.neutral[6]
            } else if line.starts_with('[') {
                c.accent[6]
            } else {
                c.text_primary
            };
            self.txt(scene, tr, line, 12.0, col, card.x0 + 20.0, y, Some((card.width() - 40.0) as f32), TextWeight::Regular);
            y += 17.0;
        }
        self.txt_center(scene, tr, "click anywhere or press Esc to close", 11.0, c.neutral[6], cx, card.y1 - 26.0, TextWeight::Regular);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_app() -> (ImagerApp, AutoCommit) {
        // Tests drive the wizard against the demo document (sample drives + a
        // qOS image + recent history) — the same content `--shots`/`--demo` use.
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    // ---- synthetic winit events for driving the real event path -----------

    fn char_ev(s: &str) -> WindowEvent {
        use winit::event::KeyEvent;
        use winit::keyboard::{Key, SmolStr};
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(s));
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(s));
        WindowEvent::KeyboardInput { device_id: unsafe { std::mem::zeroed() }, event: ev, is_synthetic: false }
    }

    fn cursor_ev(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn click_ev() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        }
    }

    /// Feed one event through the real `handle_event` path with throwaway
    /// framework out-params.
    fn feed(app: &mut ImagerApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        let m = winit::event::Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let (mut h, mut ft) = (false, None);
        app.handle_event(ev, 1.0, &m, doc, &automerge::ROOT, &mut h, &mut ft, node, &taffy);
    }

    /// Click at a point: move the pointer there (so hover state settles), then
    /// press — exactly the real winit sequence the compositor delivers.
    fn click_at(app: &mut ImagerApp, doc: &mut AutoCommit, p: Point) {
        feed(app, doc, &cursor_ev(p.x, p.y));
        feed(app, doc, &click_ev());
    }

    fn center(r: Rect) -> Point {
        Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0)
    }

    /// The rect of the first hit matching `pred`, from this frame's layout.
    fn hit_rect(app: &ImagerApp, pred: impl Fn(&Hit) -> bool) -> Rect {
        app.compute_layout()
            .hits
            .into_iter()
            .find(|(_, h)| pred(h))
            .unwrap_or_else(|| panic!("expected a matching hit target this frame"))
            .0
    }

    #[test]
    fn sync_reads_demo_state_at_source_step() {
        let (app, _doc) = seeded_app();
        assert_eq!(app.targets.len(), 2);
        assert!(app.source.is_some());
        assert_eq!(app.step, Step::Source);
        assert_eq!(app.recent.len(), 3);
    }

    #[test]
    fn live_app_starts_blank_with_no_fabricated_content() {
        // The live path (no demo seed) starts genuinely empty: no drives are
        // invented and no image is pre-selected.
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.targets.is_empty(), "no fabricated drives");
        assert!(app.source.is_none(), "no fabricated image");
        assert!(app.recent.is_empty(), "no fabricated recent history");
        assert_eq!(app.step, Step::Source);
    }

    #[test]
    fn layout_has_six_step_stations() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let lay = app.compute_layout();
        assert_eq!(lay.crumbs.len(), 6);
    }

    /// The step indicator must **fit the window at every width** — the fix for
    /// the overflow bug where the "Source" pill was clipped off the left edge at
    /// ~620px. At no width may any station rect fall outside the viewport, and
    /// there are always six stations. Asserted on the production
    /// `compute_layout` path.
    #[test]
    fn step_bar_fits_at_every_width_and_keeps_six_stations() {
        let (mut app, _doc) = seeded_app();
        for w in [300.0, 320.0, 360.0, 420.0, 500.0, 560.0, 700.0, 820.0, 1024.0, 1180.0, 1600.0] {
            app.rect = Rect::new(0.0, 0.0, w, 820.0);
            let lay = app.compute_layout();
            assert_eq!(lay.crumbs.len(), 6, "six stations at width {w}");
            for (r, s) in &lay.crumbs {
                assert!(r.x0 >= app.rect.x0 - 0.5, "station {s:?} clipped past the left edge at width {w}: x0={}", r.x0);
                assert!(r.x1 <= app.rect.x1 + 0.5, "station {s:?} clipped past the right edge at width {w}: x1={}", r.x1);
                assert!(r.width() > 0.0 && r.height() > 0.0, "station {s:?} has zero area at width {w}");
            }
        }
    }

    /// The indicator degrades through its three responsive tiers as the window
    /// narrows: full labelled pills → icon rail → compact segmented bar.
    #[test]
    fn step_bar_degrades_through_tiers_by_width() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        assert_eq!(app.compute_layout().crumb_mode, StepBarMode::Full, "wide window → labelled pills");
        app.rect = Rect::new(0.0, 0.0, 700.0, 820.0);
        assert_eq!(app.compute_layout().crumb_mode, StepBarMode::Rail, "medium window → icon rail");
        app.rect = Rect::new(0.0, 0.0, 340.0, 820.0);
        assert_eq!(app.compute_layout().crumb_mode, StepBarMode::Compact, "narrow window → segmented bar");
    }

    /// The Confirm gate is a real shared [`TextInput`] (not a hand-rolled
    /// `String`): typing through the actual click + key event path routes into
    /// the input, mirrors to the armed value + document, and arms the write.
    #[test]
    fn confirm_gate_is_a_shared_textinput_that_gates_the_write() {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        app.select_target(&mut doc, 0); // sdb
        app.goto(&mut doc, Step::Confirm);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.confirm_matches(), "not armed before typing");

        // Click the field to focus it, then type the device name.
        let field = hit_rect(&app, |h| matches!(h, Hit::ConfirmField));
        click_at(&mut app, &mut doc, center(field));
        assert!(app.confirm_focused);
        for s in ["s", "d", "b"] {
            feed(&mut app, &mut doc, &char_ev(s));
        }

        // The shared input holds the value, and it mirrored to the gate + doc.
        assert_eq!(app.confirm_input.value(), "sdb", "the shared TextInput holds the typed value");
        assert_eq!(app.confirm, "sdb", "mirrored to the armed-name gate");
        assert_eq!(ImagerDoc::confirm(&doc), "sdb", "persisted to the document");
        assert!(app.confirm_matches(), "typed device name arms the write");
    }

    #[test]
    fn advancing_requires_selections() {
        let (mut app, mut doc) = seeded_app();
        // Source is seeded → can advance to Drive.
        app.advance(&mut doc);
        assert_eq!(app.step, Step::Drive);
        // No target selected yet → stays on Drive.
        app.advance(&mut doc);
        assert_eq!(app.step, Step::Drive);
        app.select_target(&mut doc, 0);
        app.advance(&mut doc);
        // Seeded source is an installable qOS image → goes through Customize.
        assert_eq!(app.step, Step::Customize);
        app.advance(&mut doc);
        assert_eq!(app.step, Step::Confirm);
    }

    /// The primary interaction, proven end to end by clicking the real
    /// on-screen targets: walk Source → Drive → Customize → Confirm → Write,
    /// then let the (simulated) job finish and land on Done. Every transition
    /// goes through the actual hit-testing + dispatch path, not a shortcut.
    #[test]
    fn clicking_through_the_wizard_reaches_write_then_done() {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        assert_eq!(app.step, Step::Source);

        // Source → Drive: click Continue.
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Drive);

        // Pick the USB stick by clicking its card.
        let usb = hit_rect(&app, |h| matches!(h, Hit::Target(0)));
        click_at(&mut app, &mut doc, center(usb));
        assert_eq!(app.selected, Some(0));

        // Drive → Customize (the seeded qOS image is customizable).
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Customize);

        // Customize → Confirm.
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Confirm);

        // Arm the write: click the typed-name field, then type the device name.
        let field = hit_rect(&app, |h| matches!(h, Hit::ConfirmField));
        click_at(&mut app, &mut doc, center(field));
        assert!(app.confirm_focused);
        for s in ["s", "d", "b"] {
            feed(&mut app, &mut doc, &char_ev(s));
        }
        assert!(app.confirm_matches(), "typed name armed the write");

        // Confirm → Write: click the (now-enabled) primary button.
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Write);
        assert_eq!(app.progress.phase, JobPhase::Writing);
        assert!(app.job_rx.is_some());

        // The job reports success → the flow lands on the Done station. (Drive
        // the drain with a terminal progress so the test is deterministic and
        // never waits on the worker thread.)
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Progress { phase: JobPhase::Success, write_frac: 1.0, verify_frac: 1.0, rate_bps: 0 }).unwrap();
        app.job_rx = Some(rx);
        app.drain_job(&mut doc);
        // Keep the sender alive past the drain: dropping it first would look like
        // the worker died and be reported as a failure rather than success.
        drop(tx);
        assert_eq!(app.step, Step::Done);
        assert_eq!(app.progress.phase, JobPhase::Success);
    }

    #[test]
    fn plain_iso_skips_customize() {
        let (mut app, mut doc) = seeded_app();
        let iso = app.recent.iter().find(|s| s.kind == SourceKind::IsoFile).cloned().unwrap();
        app.select_source(&mut doc, iso);
        app.advance(&mut doc); // → Drive
        app.select_target(&mut doc, 0);
        app.advance(&mut doc); // installable == false → straight to Confirm
        assert_eq!(app.step, Step::Confirm);
        assert!(!app.customizable());
    }

    #[test]
    fn write_refused_without_matching_confirmation() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.goto(&mut doc, Step::Confirm);
        app.confirm = "wrong".into();
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
        assert!(app.job_rx.is_none());
        assert_eq!(app.step, Step::Confirm, "stays on the Confirm station");
        assert!(ImagerDoc::status(&doc).to_lowercase().contains("confirm"));
    }

    #[test]
    fn write_starts_with_matching_confirmation_and_advances_to_write_station() {
        // A matching typed confirmation starts a job: the phase advances to
        // Writing, the flow moves onto the Write station, and a progress channel
        // is installed. Whether the backend is simulated or real is decided by
        // the allow-gate in write.rs (tested there); either way these UI
        // invariants hold and never depend on the process environment.
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.goto(&mut doc, Step::Confirm);
        app.confirm = "sdb".into();
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Writing);
        assert_eq!(app.step, Step::Write, "the write station is now showing");
        assert!(app.job_rx.is_some());
        assert!(app.needs_animation_frame());
    }

    #[test]
    fn portal_open_selects_inspected_source() {
        let (mut app, mut doc) = seeded_app();
        // Simulate the portal returning a real qOS-looking image.
        let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
        std::fs::write(f.path(), b"qos payload for the imager test").unwrap();
        app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(f.path().to_path_buf())));
        app.drain_portal(&mut doc);
        // A `.iso` file → a valid source is selected.
        assert!(app.source.is_some());
        assert_eq!(app.source.as_ref().unwrap().kind, SourceKind::IsoFile);
    }

    #[test]
    fn drive_step_hides_internal_disks_until_opted_in() {
        let (mut app, _doc) = seeded_app();
        // Sample has one USB (removable) + one NVMe (fixed).
        assert_eq!(app.visible_targets().len(), 1);
        assert_eq!(app.removable_count(), 1);
        app.show_internal = true;
        assert_eq!(app.visible_targets().len(), 2);
    }

    /// The system disk can never be selected by a stray click; it takes an
    /// explicit acknowledgement, and withdrawing that consent deselects it.
    #[test]
    fn system_disk_needs_acknowledgement_before_selection() {
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        let mut targets = crate::model::sample_targets();
        targets[1].is_system = true; // the internal NVMe is the system disk
        ImagerDoc::seed(&mut doc, &targets);
        ImagerDoc::set_source(&mut doc, Some(&crate::model::sample_source()));
        app.sync(&mut doc, &automerge::ROOT);
        app.show_internal = true; // reveal internal disks
        assert!(app.has_visible_system_disk());

        // A stray click on the system disk is refused with a clear reason.
        app.dispatch_hit(&mut doc, Hit::Target(1));
        assert_eq!(app.selected, None, "the system disk is not selectable by accident");
        assert!(ImagerDoc::status(&doc).to_lowercase().contains("system disk"));

        // The explicit acknowledgement unlocks it.
        app.dispatch_hit(&mut doc, Hit::ToggleSystemAck);
        assert!(app.system_ack);
        app.dispatch_hit(&mut doc, Hit::Target(1));
        assert_eq!(app.selected, Some(1), "acknowledged → the system disk can be selected");

        // Withdrawing the acknowledgement drops the selection again.
        app.dispatch_hit(&mut doc, Hit::ToggleSystemAck);
        assert!(!app.system_ack);
        assert_eq!(app.selected, None, "withdrawing consent deselects the system disk");
    }

    /// Set the Customize step up with one field cleared and focused, ready for
    /// the shared panel to receive keystrokes through the real event path.
    fn customize_with_field_focused(field: crate::customize::CzField) -> (ImagerApp, AutoCommit) {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        // Clear the target field so the typed assertion is exact.
        let mut p = ImagerDoc::profile(&doc);
        match field {
            crate::customize::CzField::Password => p.user.password.clear(),
            crate::customize::CzField::Hostname => p.hostname.clear(),
            _ => {}
        }
        ImagerDoc::set_profile(&mut doc, &p);
        ImagerDoc::set_step(&mut doc, Step::Customize);
        app.sync(&mut doc, &automerge::ROOT); // reads the profile + step back
        assert_eq!(app.step, Step::Customize, "the panel only handles events on this step");
        assert!(app.customizable(), "the seeded qOS image is customizable");
        app.customize.focus_for_test(field);
        (app, doc)
    }

    /// The password field is a shared secret `TextInput` inside the customization
    /// panel: typing through the real keyboard/event path persists to the doc,
    /// the input holds the true value, and its a11y node stays protected with no
    /// value (the plaintext never leaks).
    #[test]
    fn typing_into_password_uses_secret_textinput_and_never_leaks_to_a11y() {
        use crate::customize::CzField;
        let (mut app, mut doc) = customize_with_field_focused(CzField::Password);
        for s in ["h", "u", "n", "t", "2"] {
            feed(&mut app, &mut doc, &char_ev(s));
        }

        assert_eq!(ImagerDoc::profile(&doc).user.password, "hunt2", "typed value persisted to the doc");
        assert_eq!(app.customize.field_value(CzField::Password), "hunt2", "the input holds the real secret");

        let node = Widget::accessibility(app.customize.password_input()).expect("a11y node");
        assert_eq!(node.value, None, "the secret must not reach the a11y tree");
        assert!(node.state.protected, "password input is marked protected");
    }

    #[test]
    fn typing_into_hostname_persists_to_profile_through_the_panel() {
        use crate::customize::CzField;
        let (mut app, mut doc) = customize_with_field_focused(CzField::Hostname);
        for s in ["l", "a", "b", "0", "1"] {
            feed(&mut app, &mut doc, &char_ev(s));
        }
        assert_eq!(ImagerDoc::profile(&doc).hostname, "lab01", "persisted to the doc");
        assert_eq!(app.customize.field_value(CzField::Hostname), "lab01", "the input holds it");
    }

    #[test]
    fn export_produces_headless_config() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 1);
        app.export_config(&mut doc);
        let text = app.export_text.clone().unwrap();
        assert!(text.contains("[install]"));
        assert!(text.contains("$QOS_USER_PASSWORD"));
    }

    #[test]
    fn write_another_resets_to_source() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0);
        app.progress = Progress { phase: JobPhase::Success, write_frac: 1.0, verify_frac: 1.0, rate_bps: 0 };
        app.write_another(&mut doc);
        assert_eq!(app.step, Step::Source);
        assert_eq!(app.progress.phase, JobPhase::Idle);
    }

    /// The affordance == `ActionSpec` covenant on the *real* production path: a
    /// click on the on-screen Customize encryption toggle (routed through the
    /// shared panel's real event handling) applies to the document, records
    /// exactly one undo boundary, and dispatches the byte-identical
    /// `EncodedAction` the Ctrl+K palette would — one source of truth.
    #[test]
    fn encrypt_toggle_dispatches_the_palette_action_and_records_one_undo_boundary() {
        use qos_actions::ActionInput;
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        app.step = Step::Customize;
        // The seeded qOS source is customizable; encryption defaults off.
        assert!(app.customizable());
        assert!(!ImagerDoc::profile(&doc).encrypt);

        // The action Ctrl+K would dispatch right now: enable encryption.
        let label = app.customization_source_label().unwrap();
        let resources = crate::actions::provide_resources(&label, &ImagerDoc::profile(&doc));
        let spec = resources[0].actions.iter().find(|a| a.id == "encrypt").unwrap();
        let palette_action = match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("encrypt is an immediate action"),
        };

        // Click the real on-screen toggle: move the pointer onto it, then press.
        let rect = CustomizePanel::encrypt_rect(app.content_rect());
        let (cx, cy) = ((rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0);
        feed(&mut app, &mut doc, &cursor_ev(cx, cy));
        feed(&mut app, &mut doc, &click_ev());

        // It applied to the document...
        assert!(ImagerDoc::profile(&doc).encrypt, "the toggle enabled encryption in the doc");
        // ...recorded exactly one undo boundary...
        assert!(
            app.take_committed_action().is_some(),
            "one undo boundary bracketed the toggle"
        );
        // ...and dispatched the byte-identical action Ctrl+K would.
        let dispatched = app.last_dispatched().expect("the toggle dispatched an action");
        assert_eq!(dispatched.type_id, crate::actions::IMAGER_ACTION_TYPE);
        assert_eq!(
            dispatched, &palette_action,
            "the toggle and Ctrl+K are one source of truth"
        );
    }
}
