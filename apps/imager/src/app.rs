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
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::customize::CustomizePanel;
use crate::doc::ImagerDoc;
use crate::model::{
    Action, InstallProfile, JobPhase, Progress, QosRelease, Source, SourceKind, Step, TargetDrive,
    TargetKind, fmt_eta, fmt_rate, fmt_size,
};
use crate::write::{WriteRequest, spawn_job};

// ---- layout constants (logical px) -------------------------------------
const OUTER: f64 = 28.0;
// The compositor draws the window chrome + app name (OS owns chrome), so the
// app itself opens straight into the step indicator — no in-app title bar.
const CRUMB_TOP: f64 = 26.0;
const CRUMB_H: f64 = 34.0;
const FOOTER_H: f64 = 78.0;
const CONTENT_MAX_W: f64 = 900.0;
/// Height of the Source step's "no qOS releases" card. Shared by the draw and
/// layout passes so the two stay in step.
const RELEASE_ZERO_H: f64 = 84.0;

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
    back: Option<Rect>,
    primary: Option<Rect>,
    hits: Vec<(Rect, Hit)>,
    content: Rect,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (internal-disk visibility, the system-disk destructive-write ack, confirm-field focus, and seed/repaint bookkeeping), so grouping them would need a real design pass rather than a mechanical lint fix"
)]
pub struct ImagerApp {
    // ---- CRDT-backed (cached each `sync`) ----
    source: Option<Source>,
    recent: Vec<Source>,
    targets: Vec<TargetDrive>,
    /// qOS releases offered on the Source step. Like the drive list this is
    /// read from the document and never invented: an offered release is a
    /// write source for a job that erases the target, so one that does not
    /// exist on this machine must not be clickable.
    releases: Vec<QosRelease>,
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
            releases: Vec::new(),
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
            actions: ui::ActionHost::new(),
        }
    }

    fn target(&self) -> Option<&TargetDrive> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    /// The releases the document actually knows about — empty on the live
    /// path, seeded content under `--demo` / `--shots`.
    fn releases(&self) -> &[QosRelease] {
        &self.releases
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
        s.index() < self.step.index() && (s != Step::Customize || self.customizable())
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
        self.source.as_ref().is_some_and(|s| s.installable)
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
        Some(
            self.source
                .as_ref()
                .map_or_else(|| "qOS image".into(), |s| s.name.clone()),
        )
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
        !self.confirm.is_empty() && (self.confirm == want || self.confirm == format!("/dev/{want}"))
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
        tr.draw_text_weighted(
            scene,
            s,
            size,
            color,
            Affine::translate((x, top)),
            max_w,
            weight,
        );
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
        Self::txt(
            scene,
            tr,
            s,
            size,
            color,
            right - f64::from(w),
            top,
            None,
            weight,
        );
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
        Self::txt(
            scene,
            tr,
            s,
            size,
            color,
            cx - f64::from(w) / 2.0,
            top,
            None,
            weight,
        );
    }

    fn fill_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color) {
        scene.fill(
            Fill::NonZero,
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(
            &Stroke::new(w),
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
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

    /// A small check-mark glyph inside a filled disc of radius `rad` at `c`.
    fn check_disc(scene: &mut Scene, t: Affine, center: Point, rad: f64, disc: Color, tick: Color) {
        scene.fill(Fill::NonZero, t, disc, None, &Circle::new(center, rad));
        let mut p = BezPath::new();
        p.move_to((center.x - rad * 0.42, center.y + rad * 0.02));
        p.line_to((center.x - rad * 0.10, center.y + rad * 0.34));
        p.line_to((center.x + rad * 0.46, center.y - rad * 0.34));
        scene.stroke(&Stroke::new(rad * 0.24), t, tick, None, &p);
    }

    fn section_label(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        s: &str,
        x: f64,
        y: f64,
    ) {
        Self::txt(
            scene,
            tr,
            s,
            11.0,
            theme.tokens.colors.text_dim,
            x,
            y,
            None,
            TextWeight::Semibold,
        );
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

        // ---- Step-indicator pills ----
        let steps = Step::all();
        let crumb_w = 132.0f64;
        let gap = 14.0f64;
        let total = crumb_w * steps.len() as f64 + gap * (steps.len() as f64 - 1.0);
        let mut cx = v.x0 + (v.width() - total) / 2.0;
        let cy = v.y0 + CRUMB_TOP;
        let mut crumbs = Vec::new();
        for s in steps {
            let r = Rect::new(cx, cy, cx + crumb_w, cy + CRUMB_H);
            crumbs.push((r, s));
            if self.crumb_reachable(s) {
                hits.push((r, Hit::StepJump(s)));
            }
            cx += crumb_w + gap;
        }

        // ---- Per-step content hit-targets ----
        match self.step {
            Step::Source => self.layout_source(content, &mut hits),
            Step::Drive => self.layout_drive(content, &mut hits),
            Step::Confirm => Self::layout_confirm(content, &mut hits),
            // Customize's fields/toggles are owned by `CustomizePanel`, which
            // hit-tests its own rects in `handle_event`; Write (in progress)
            // and Done have no interactive content beyond the footer. No app
            // hits for any of the three.
            Step::Customize | Step::Write | Step::Done => {}
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

        Layout {
            crumbs,
            back,
            primary,
            hits,
            content,
        }
    }

    /// The primary action button rect, right-aligned in the footer — or `None`
    /// on the Write station (a live-progress screen with no primary).
    fn primary_button_rect(&self, content: Rect, by: f64) -> Option<Rect> {
        if self.step == Step::Write {
            return None;
        }
        let w = match self.step {
            Step::Confirm => 250.0,
            Step::Done => 190.0,
            _ => 170.0,
        };
        Some(Rect::new(content.x1 - w, by, content.x1, by + 44.0))
    }

    fn layout_source(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        let x0 = content.x0;
        let x1 = content.x1;
        let mut y = content.y0 + 44.0;
        // Browse drop-zone.
        let browse = Rect::new(x0, y, x1, y + 96.0);
        hits.push((browse, Hit::PickFile));
        y = browse.y1 + (if self.source.is_some() { 16.0 } else { 28.0 });
        if self.source.is_some() {
            y += 92.0 + 24.0; // selected card
        }
        // qOS releases list. With none known, the zero-state card takes the
        // space and there is nothing here to click.
        y += 22.0; // section label
        if self.releases().is_empty() {
            y += RELEASE_ZERO_H + 10.0;
        }
        for (i, _) in self.releases().iter().enumerate() {
            let r = Rect::new(x0, y, x1, y + 54.0);
            hits.push((r, Hit::Release(i)));
            y += 54.0 + 10.0;
        }
        // Recent list.
        if !self.recent.is_empty() {
            y += 12.0 + 20.0; // gap + section label, as drawn
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
        // Rescan button top-right of the header row.
        let rescan = Rect::new(x1 - 104.0, content.y0, x1, content.y0 + 30.0);
        hits.push((rescan, Hit::Rescan));
        let mut y = content.y0 + 52.0;
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

    fn layout_confirm(content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        // The typed device-name confirmation field sits under the review card.
        let field = Rect::new(
            content.x0,
            content.y1 - 96.0,
            content.x1,
            content.y1 - 96.0 + 46.0,
        );
        hits.push((field, Hit::ConfirmField));
    }
}

// =========================================================================
// Header + breadcrumb
// =========================================================================
impl ImagerApp {
    fn draw_header(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
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
            self.txt_right(
                scene,
                tr,
                &self.status,
                11.5,
                col,
                v.x1 - OUTER,
                v.y0 + 8.0,
                TextWeight::Medium,
            );
        }

        // ---- Step indicator ----
        for (i, (r, s)) in lay.crumbs.iter().enumerate() {
            let done = self.step_completed(*s);
            let current = *s == self.step;
            let hov = r.contains(self.last_mouse) && self.crumb_reachable(*s);
            let bg = if current {
                with_alpha(c.accent[5], 40)
            } else if hov {
                c.neutral[2]
            } else {
                Color::TRANSPARENT
            };
            if bg != Color::TRANSPARENT {
                Self::fill_rr(scene, t, *r, 10.0, bg);
            }
            if current {
                Self::stroke_rr(scene, t, *r, 10.0, c.accent[5], 1.4);
            }
            // Number disc / check.
            let disc_c = Point::new(r.x0 + 22.0, f64::midpoint(r.y0, r.y1));
            if done {
                Self::check_disc(scene, t, disc_c, 11.0, c.semantic.success, Color::WHITE);
            } else {
                let ring = if current { c.accent[5] } else { c.neutral[4] };
                scene.fill(Fill::NonZero, t, ring, None, &Circle::new(disc_c, 11.0));
                self.txt_center(
                    scene,
                    tr,
                    &format!("{}", s.index() + 1),
                    12.0,
                    Color::WHITE,
                    disc_c.x,
                    disc_c.y - 8.0,
                    TextWeight::Bold,
                );
            }
            let label_c = if current || done {
                c.text_primary
            } else {
                c.text_dim
            };
            Self::txt(
                scene,
                tr,
                s.title(),
                13.5,
                label_c,
                r.x0 + 40.0,
                f64::midpoint(r.y0, r.y1) - 9.0,
                None,
                if current {
                    TextWeight::Semibold
                } else {
                    TextWeight::Medium
                },
            );
            // Connector to the next pill.
            if let Some(next) = lay.crumbs.get(i + 1) {
                let nx0 = next.0.x0;
                let midy = f64::midpoint(r.y0, r.y1);
                let cc = if s.index() < self.step.index() {
                    c.semantic.success
                } else {
                    c.neutral[3]
                };
                scene.stroke(
                    &Stroke::new(2.0),
                    t,
                    cc,
                    None,
                    &Line::new(Point::new(r.x1 + 4.0, midy), Point::new(nx0 - 4.0, midy)),
                );
            }
        }
    }

    fn draw_footer(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let v = self.rect;
        let fy = v.y1 - FOOTER_H;
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new(Point::new(v.x0, fy), Point::new(v.x1, fy)),
        );

        // Back.
        if let Some(r) = lay.back {
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(
                scene,
                t,
                r,
                10.0,
                if hov { c.neutral[3] } else { c.neutral[2] },
            );
            Self::stroke_rr(scene, t, r, 10.0, c.border_standard, 1.0);
            self.txt_center(
                scene,
                tr,
                "‹  Back",
                14.0,
                c.text_primary,
                f64::midpoint(r.x0, r.x1),
                r.y0 + 12.0,
                TextWeight::Medium,
            );
        }

        // Primary.
        if let Some(r) = lay.primary {
            self.draw_primary(scene, tr, theme, t, r);
        }

        // A hint between them (center) — e.g. what's still needed.
        if let Some(hint) = self.footer_hint() {
            self.txt_center(
                scene,
                tr,
                &hint,
                12.0,
                c.text_dim,
                f64::midpoint(v.x0, v.x1),
                fy + FOOTER_H / 2.0 - 8.0,
                TextWeight::Regular,
            );
        }
    }

    fn footer_hint(&self) -> Option<String> {
        match self.step {
            Step::Source if self.source.is_none() => Some("Choose an image to continue".into()),
            Step::Drive if self.selected.is_none() => Some("Select a drive to continue".into()),
            Step::Confirm if !self.confirm_matches() => self
                .target()
                .map(|d| format!("Type “{}” to unlock", d.name)),
            Step::Write => Some("Keep the drive connected until this finishes…".into()),
            _ => None,
        }
    }

    fn draw_primary(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        r: Rect,
    ) {
        let c = &theme.tokens.colors;
        let hov = r.contains(self.last_mouse);
        let (label, enabled, destructive) = match self.step {
            Step::Source => (
                "Continue  ›".to_string(),
                self.can_advance(Step::Source),
                false,
            ),
            Step::Drive => (
                "Continue  ›".to_string(),
                self.can_advance(Step::Drive),
                false,
            ),
            Step::Customize => ("Continue  ›".to_string(), true, false),
            Step::Confirm => {
                let dev = self
                    .target()
                    .map(super::model::TargetDrive::dev_path)
                    .unwrap_or_default();
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
            if hov {
                c.semantic.danger
            } else {
                with_alpha(c.semantic.danger, 220)
            }
        } else if hov {
            c.accent[6]
        } else {
            c.accent[5]
        };
        Self::fill_rr(scene, t, r, 11.0, bg);
        let fg = if enabled { Color::WHITE } else { c.neutral[6] };
        self.txt_center(
            scene,
            tr,
            &label,
            14.0,
            fg,
            f64::midpoint(r.x0, r.x1),
            r.y0 + 13.0,
            TextWeight::Bold,
        );
    }
}

// =========================================================================
// Step bodies
// =========================================================================
impl ImagerApp {
    fn draw_step_title(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        title: &str,
        sub: &str,
    ) {
        let c = &theme.tokens.colors;
        Self::txt(
            scene,
            tr,
            title,
            19.0,
            c.text_primary,
            content.x0,
            content.y0,
            None,
            TextWeight::Bold,
        );
        Self::txt(
            scene,
            tr,
            sub,
            13.0,
            c.text_dim,
            content.x0,
            content.y0 + 26.0,
            Some(content.width() as f32),
            TextWeight::Regular,
        );
    }

    fn draw_source_step(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let content = lay.content;
        let sub = if self.releases().is_empty() {
            "The image to write. Pick a local .iso or .img file."
        } else {
            "The image to write. Pick a local .iso or .img file, or a qOS release below."
        };
        self.draw_step_title(scene, tr, theme, content, "Choose the source image", sub);
        let browse = self.draw_source_dropzone(scene, tr, theme, t, content);
        let y = self.draw_source_selected_card(scene, tr, theme, t, content, browse);
        let y = self.draw_source_releases(scene, tr, theme, t, content, y);
        self.draw_source_recent(scene, tr, theme, t, content, y);
    }

    /// Draws the "Browse for an image…" drop-zone and returns its rect (the
    /// selected-source card below it is anchored off `browse.y1`).
    fn draw_source_dropzone(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let y = content.y0 + 44.0;
        let browse = Rect::new(content.x0, y, content.x1, y + 96.0);
        let hov = browse.contains(self.last_mouse);
        Self::fill_rr(
            scene,
            t,
            browse,
            14.0,
            if hov {
                c.neutral[2]
            } else {
                with_alpha(c.accent[5], 16)
            },
        );
        Self::dashed_rr(
            scene,
            t,
            browse,
            14.0,
            if hov {
                c.accent[6]
            } else {
                with_alpha(c.accent[5], 160)
            },
            1.6,
        );
        // Download/arrow glyph.
        let gx = browse.x0 + 46.0;
        let gy = f64::midpoint(browse.y0, browse.y1);
        let mut arr = BezPath::new();
        arr.move_to((gx, gy - 14.0));
        arr.line_to((gx, gy + 8.0));
        scene.stroke(&Stroke::new(2.4), t, c.accent[6], None, &arr);
        let mut head = BezPath::new();
        head.move_to((gx - 7.0, gy + 1.0));
        head.line_to((gx, gy + 9.0));
        head.line_to((gx + 7.0, gy + 1.0));
        scene.stroke(&Stroke::new(2.4), t, c.accent[6], None, &head);
        scene.stroke(
            &Stroke::new(2.4),
            t,
            c.accent[6],
            None,
            &Line::new(
                Point::new(gx - 12.0, gy + 15.0),
                Point::new(gx + 12.0, gy + 15.0),
            ),
        );
        Self::txt(
            scene,
            tr,
            "Browse for an image…",
            16.0,
            c.text_primary,
            browse.x0 + 84.0,
            browse.y0 + 30.0,
            None,
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            "Opens the system file picker · .iso, .img, .raw",
            12.5,
            c.text_dim,
            browse.x0 + 84.0,
            browse.y0 + 54.0,
            None,
            TextWeight::Regular,
        );
        browse
    }

    /// Draws the selected-source summary card below the drop-zone (or draws
    /// nothing, if no source is selected yet), returning the y for the next
    /// section.
    fn draw_source_selected_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        browse: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let y = browse.y1 + 16.0;
        let Some(src) = &self.source else {
            return browse.y1 + 28.0;
        };
        let card = Rect::new(content.x0, y, content.x1, y + 92.0);
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(
            scene,
            t,
            card,
            14.0,
            with_alpha(c.semantic.success, 150),
            1.4,
        );
        Self::check_disc(
            scene,
            t,
            Point::new(card.x0 + 28.0, f64::midpoint(card.y0, card.y1)),
            13.0,
            c.semantic.success,
            Color::WHITE,
        );
        let badge = Rect::new(
            card.x0 + 52.0,
            card.y0 + 18.0,
            card.x0 + 52.0 + 46.0,
            card.y0 + 18.0 + 22.0,
        );
        Self::fill_rr(scene, t, badge, 6.0, source_color(theme, src.kind));
        self.txt_center(
            scene,
            tr,
            src.kind.tag(),
            12.0,
            Color::WHITE,
            f64::midpoint(badge.x0, badge.x1),
            badge.y0 + 4.0,
            TextWeight::Bold,
        );
        Self::txt(
            scene,
            tr,
            &src.name,
            15.5,
            c.text_primary,
            card.x0 + 108.0,
            card.y0 + 18.0,
            Some((card.width() - 130.0) as f32),
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            &src.summary(),
            12.0,
            c.text_dim,
            card.x0 + 108.0,
            card.y0 + 42.0,
            Some((card.width() - 130.0) as f32),
            TextWeight::Regular,
        );
        if src.installable {
            Self::txt(
                scene,
                tr,
                "● Can be pre-configured",
                11.5,
                c.semantic.success,
                card.x0 + 108.0,
                card.y0 + 62.0,
                None,
                TextWeight::Medium,
            );
        } else {
            Self::txt(
                scene,
                tr,
                "Written as-is",
                11.5,
                c.text_dim,
                card.x0 + 108.0,
                card.y0 + 62.0,
                None,
                TextWeight::Medium,
            );
        }
        card.y1 + 24.0
    }

    /// Draws the "qOS release" heading, its zero-state, and the list of known
    /// releases, returning the y for the next section.
    fn draw_source_releases(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        // qOS releases. With none to choose between, the heading names the
        // section rather than offering a choice that is not there.
        let heading = if self.releases().is_empty() {
            "qOS RELEASES"
        } else {
            "OR CHOOSE A qOS RELEASE"
        };
        self.section_label(scene, tr, theme, heading, content.x0, y);
        y += 22.0;
        if self.releases().is_empty() {
            // Nothing is known to be on this machine, so nothing is offered.
            // The honest zero-state says so rather than leaving a bare heading.
            let card = Rect::new(content.x0, y, content.x1, y + RELEASE_ZERO_H);
            Self::fill_rr(scene, t, card, 12.0, c.surfaces.raised);
            Self::dashed_rr(scene, t, card, 12.0, c.border_standard, 1.2);
            let cx = f64::midpoint(card.x0, card.x1);
            self.txt_center(
                scene,
                tr,
                "No qOS releases found",
                14.0,
                c.text_primary,
                cx,
                card.y0 + 24.0,
                TextWeight::Semibold,
            );
            self.txt_center(
                scene,
                tr,
                "Releases stored on this machine appear here.",
                12.0,
                c.text_dim,
                cx,
                card.y0 + 47.0,
                TextWeight::Regular,
            );
            y = card.y1 + 10.0;
        }
        for rel in self.releases() {
            let r = Rect::new(content.x0, y, content.x1, y + 54.0);
            let selected = self.source.as_ref().is_some_and(|s| {
                s.kind == SourceKind::QosImage && s.name == format!("{} {}", rel.name, rel.version)
            });
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(
                scene,
                t,
                r,
                11.0,
                if selected {
                    with_alpha(c.accent[5], 34)
                } else if hov {
                    c.neutral[2]
                } else {
                    c.surfaces.raised
                },
            );
            Self::stroke_rr(
                scene,
                t,
                r,
                11.0,
                if selected {
                    c.accent[5]
                } else {
                    c.border_standard
                },
                if selected { 1.4 } else { 1.0 },
            );
            let badge = Rect::new(
                r.x0 + 14.0,
                r.y0 + 15.0,
                r.x0 + 14.0 + 42.0,
                r.y0 + 15.0 + 22.0,
            );
            Self::fill_rr(scene, t, badge, 6.0, c.accent[5]);
            self.txt_center(
                scene,
                tr,
                "qOS",
                11.0,
                Color::WHITE,
                f64::midpoint(badge.x0, badge.x1),
                badge.y0 + 5.0,
                TextWeight::Bold,
            );
            Self::txt(
                scene,
                tr,
                &format!("qOS {}", rel.version),
                14.5,
                c.text_primary,
                r.x0 + 68.0,
                r.y0 + 9.0,
                None,
                TextWeight::Semibold,
            );
            let where_ = if rel.local_path.is_empty() {
                "download"
            } else {
                "on disk"
            };
            Self::txt(
                scene,
                tr,
                &format!("{}  ·  {}  ·  {}", rel.channel, fmt_size(rel.size), where_),
                11.5,
                c.text_dim,
                r.x0 + 68.0,
                r.y0 + 30.0,
                None,
                TextWeight::Regular,
            );
            self.txt_right(
                scene,
                tr,
                "installable",
                11.0,
                c.semantic.success,
                r.x1 - 16.0,
                r.y0 + 20.0,
                TextWeight::Medium,
            );
            y += 54.0 + 10.0;
        }
        y
    }

    /// Draws the "Recent" section listing up to the three most recent sources.
    fn draw_source_recent(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        mut y: f64,
    ) {
        let c = &theme.tokens.colors;
        if self.recent.is_empty() {
            return;
        }
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
            Self::txt(
                scene,
                tr,
                &src.name,
                13.0,
                c.text_primary,
                r.x0 + 22.0,
                r.y0 + 6.0,
                Some((r.width() - 120.0) as f32),
                TextWeight::Medium,
            );
            Self::txt(
                scene,
                tr,
                &fmt_size(src.size),
                11.0,
                c.text_dim,
                r.x0 + 22.0,
                r.y0 + 24.0,
                None,
                TextWeight::Regular,
            );
            self.txt_right(
                scene,
                tr,
                src.kind.tag(),
                10.5,
                c.neutral[6],
                r.x1 - 12.0,
                r.y0 + 15.0,
                TextWeight::Semibold,
            );
            y += 44.0 + 8.0;
        }
    }

    fn draw_drive_step(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        self.draw_step_title(
            scene,
            tr,
            theme,
            content,
            "Select the target drive",
            "The drive to write to — everything on it will be erased.",
        );
        // Detected count (to the left of the Rescan button).
        let detected = if self.show_internal {
            format!("{} drive(s)", self.targets.len())
        } else {
            format!("{} removable", self.removable_count())
        };
        self.txt_right(
            scene,
            tr,
            &detected,
            11.5,
            c.text_dim,
            content.x1 - 116.0,
            content.y0 + 8.0,
            TextWeight::Medium,
        );
        // Rescan button (top-right).
        let rescan = Rect::new(
            content.x1 - 104.0,
            content.y0,
            content.x1,
            content.y0 + 30.0,
        );
        let hov = rescan.contains(self.last_mouse);
        Self::fill_rr(
            scene,
            t,
            rescan,
            9.0,
            if hov { c.neutral[3] } else { c.neutral[2] },
        );
        Self::stroke_rr(scene, t, rescan, 9.0, c.border_standard, 1.0);
        self.txt_center(
            scene,
            tr,
            "⟳ Rescan",
            12.5,
            c.text_primary,
            f64::midpoint(rescan.x0, rescan.x1),
            rescan.y0 + 7.0,
            TextWeight::Medium,
        );

        let vis = self.visible_targets();
        let mut y = content.y0 + 52.0;
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
        let tr_rect = Rect::new(
            content.x0,
            content.y1 - 44.0,
            content.x0 + 340.0,
            content.y1 - 44.0 + 34.0,
        );
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
            let ack = Rect::new(
                tr_rect.x1 + 12.0,
                tr_rect.y0,
                (tr_rect.x1 + 12.0 + 470.0).min(content.x1),
                tr_rect.y1,
            );
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
        Self::fill_rr(
            scene,
            t,
            row,
            10.0,
            if hov { c.neutral[3] } else { c.neutral[2] },
        );
        Self::stroke_rr(
            scene,
            t,
            row,
            10.0,
            if checked {
                with_alpha(accent, 170)
            } else {
                c.border_standard
            },
            1.0,
        );
        let box_r = Rect::new(
            row.x0 + 12.0,
            row.y0 + 9.0,
            row.x0 + 12.0 + 16.0,
            row.y0 + 9.0 + 16.0,
        );
        Self::fill_rr(
            scene,
            t,
            box_r,
            4.0,
            if checked { accent } else { c.neutral[4] },
        );
        if checked {
            Self::check_disc(
                scene,
                t,
                Point::new(
                    f64::midpoint(box_r.x0, box_r.x1),
                    f64::midpoint(box_r.y0, box_r.y1),
                ),
                8.0,
                accent,
                Color::WHITE,
            );
        }
        Self::txt(
            scene,
            tr,
            label,
            12.5,
            c.text_primary,
            box_r.x1 + 10.0,
            row.y0 + 9.0,
            Some((row.width() - 40.0) as f32),
            TextWeight::Medium,
        );
    }

    fn draw_drive_empty(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let card = Rect::new(
            content.x0,
            content.y0 + 60.0,
            content.x1,
            content.y0 + 60.0 + 240.0,
        );
        Self::fill_rr(scene, t, card, 16.0, c.surfaces.raised);
        Self::dashed_rr(scene, t, card, 16.0, c.border_standard, 1.4);
        let cx = f64::midpoint(card.x0, card.x1);
        // A simple USB-stick glyph.
        let gy = card.y0 + 66.0;
        let body = Rect::new(cx - 16.0, gy, cx + 16.0, gy + 54.0);
        Self::fill_rr(scene, t, body, 6.0, c.neutral[4]);
        let cap = Rect::new(cx - 10.0, gy - 16.0, cx + 10.0, gy);
        Self::fill_rr(scene, t, cap, 3.0, c.neutral[5]);
        self.txt_center(
            scene,
            tr,
            "Insert a drive",
            17.0,
            c.text_primary,
            cx,
            card.y0 + 140.0,
            TextWeight::Semibold,
        );
        self.txt_center(
            scene,
            tr,
            "Plug in a USB stick or SD card, then press Rescan.",
            13.0,
            c.text_dim,
            cx,
            card.y0 + 168.0,
            TextWeight::Regular,
        );
        let hint = if self.show_internal {
            "No drives detected."
        } else {
            "No removable drives detected — enable “Show internal disks” to target a fixed disk."
        };
        self.txt_center(
            scene,
            tr,
            hint,
            12.0,
            c.neutral[6],
            cx,
            card.y0 + 196.0,
            TextWeight::Regular,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_drive_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        r: Rect,
        idx: usize,
        drive: &TargetDrive,
    ) {
        let c = &theme.tokens.colors;
        let selected = self.selected == Some(idx);
        let hover = self.hover_target == Some(idx) && !selected;
        let danger = !drive.removable;
        let bg = if selected {
            with_alpha(c.accent[5], 30)
        } else if hover {
            c.neutral[2]
        } else {
            c.surfaces.raised
        };
        Self::fill_rr(scene, t, r, 14.0, bg);
        let border = if selected {
            c.accent[5]
        } else if danger {
            with_alpha(c.semantic.danger, 150)
        } else {
            c.border_standard
        };
        Self::stroke_rr(scene, t, r, 14.0, border, if selected { 2.0 } else { 1.0 });

        // Selection check on the left.
        if selected {
            Self::check_disc(
                scene,
                t,
                Point::new(r.x0 + 28.0, f64::midpoint(r.y0, r.y1)),
                13.0,
                c.accent[5],
                Color::WHITE,
            );
        }
        let left = if selected { r.x0 + 54.0 } else { r.x0 + 18.0 };

        // Kind badge.
        let badge = Rect::new(left, r.y0 + 18.0, left + 54.0, r.y0 + 18.0 + 24.0);
        Self::fill_rr(scene, t, badge, 6.0, kind_color(theme, drive.kind));
        self.txt_center(
            scene,
            tr,
            drive.kind.tag(),
            12.0,
            Color::WHITE,
            f64::midpoint(badge.x0, badge.x1),
            badge.y0 + 5.0,
            TextWeight::Bold,
        );

        self.draw_drive_card_details(scene, tr, theme, t, r, drive, left);
    }

    /// Draws the removable/fixed/system chip, the model + capacity line, and
    /// the destructive-erase (or locked) note for a drive card.
    #[allow(
        clippy::too_many_arguments,
        reason = "shares the (scene, tr, theme, t) render context plus per-card geometry with every draw_* helper in this file; bundling that context is a wider render-context refactor out of scope here"
    )]
    fn draw_drive_card_details(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        r: Rect,
        drive: &TargetDrive,
        left: f64,
    ) {
        let c = &theme.tokens.colors;
        // Removable / fixed / system chip on the right.
        let (chip_label, chip_c) = if drive.is_system {
            ("SYSTEM DISK", c.semantic.danger)
        } else if drive.removable {
            ("REMOVABLE", c.semantic.success)
        } else {
            ("INTERNAL DISK", c.semantic.warning)
        };
        let (cw, _) = tr.measure_weighted(chip_label, 10.5, None, TextWeight::Bold);
        let chip = Rect::new(
            r.x1 - 16.0 - f64::from(cw) - 18.0,
            r.y0 + 18.0,
            r.x1 - 16.0,
            r.y0 + 18.0 + 24.0,
        );
        Self::fill_rr(scene, t, chip, 6.0, with_alpha(chip_c, 40));
        self.txt_center(
            scene,
            tr,
            chip_label,
            10.5,
            chip_c,
            f64::midpoint(chip.x0, chip.x1),
            chip.y0 + 6.0,
            TextWeight::Bold,
        );

        // Model + capacity.
        Self::txt(
            scene,
            tr,
            &drive.model,
            16.0,
            c.text_primary,
            left,
            r.y0 + 50.0,
            Some((r.width() - (left - r.x0) - 30.0) as f32),
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            &format!("{}  ·  {}", drive.dev_path(), fmt_size(drive.size)),
            12.5,
            c.text_dim,
            left,
            r.y0 + 72.0,
            None,
            TextWeight::Regular,
        );

        // Destructive line — or, for a not-yet-acknowledged system disk, a
        // clear "locked" note pointing at the acknowledgement gate.
        let (line, col) = if drive.is_system && !self.system_ack {
            (
                "🔒 Locked — this is your system disk. Acknowledge below to select it.".to_string(),
                c.text_dim,
            )
        } else {
            (
                format!("⚠ Erases {}", drive.contents_summary()),
                c.semantic.danger,
            )
        };
        Self::txt(
            scene,
            tr,
            &line,
            11.5,
            col,
            left,
            r.y1 - 24.0,
            Some((r.width() - (left - r.x0) - 30.0) as f32),
            TextWeight::Medium,
        );
    }

    fn draw_customize_step(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        if !self.customizable() {
            self.draw_step_title(scene, tr, theme, content, "Customize (optional)", "");
            let card = Rect::new(
                content.x0,
                content.y0 + 60.0,
                content.x1,
                content.y0 + 60.0 + 150.0,
            );
            Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
            self.txt_center(
                scene,
                tr,
                "This image is written unchanged",
                16.0,
                c.text_primary,
                f64::midpoint(card.x0, card.x1),
                card.y0 + 46.0,
                TextWeight::Semibold,
            );
            self.txt_center(
                scene,
                tr,
                "Pre-configuration (hostname, user, region) is available for qOS",
                13.0,
                c.text_dim,
                f64::midpoint(card.x0, card.x1),
                card.y0 + 76.0,
                TextWeight::Regular,
            );
            self.txt_center(
                scene,
                tr,
                "images. Press Continue to write it as-is.",
                13.0,
                c.text_dim,
                f64::midpoint(card.x0, card.x1),
                card.y0 + 96.0,
                TextWeight::Regular,
            );
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
        let removable = self.target().is_some_and(|d| d.removable);
        self.customize.draw(
            scene,
            t,
            tr,
            theme,
            presence,
            content,
            &self.profile,
            removable,
        );
    }

    fn draw_confirm_step(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let content = lay.content;
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
        let card = Rect::new(
            content.x0,
            content.y0 + 46.0,
            content.x1,
            content.y0 + 46.0 + 150.0,
        );
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
        let row =
            |scene: &mut Scene, tr: &mut TextRenderer, y: f64, k: &str, v: String, ok: bool| {
                Self::txt(
                    scene,
                    tr,
                    k,
                    12.0,
                    c.text_dim,
                    card.x0 + 20.0,
                    y,
                    None,
                    TextWeight::Semibold,
                );
                let col = if ok {
                    c.text_primary
                } else {
                    c.semantic.danger
                };
                Self::txt(
                    scene,
                    tr,
                    &v,
                    14.0,
                    col,
                    card.x0 + 96.0,
                    y - 1.0,
                    Some((card.width() - 116.0) as f32),
                    TextWeight::Semibold,
                );
            };
        let src_v = self.source.as_ref().map_or_else(
            || "—".into(),
            |s| format!("{}   ({})", s.name, fmt_size(s.size)),
        );
        row(
            scene,
            tr,
            card.y0 + 24.0,
            "Image",
            src_v,
            self.source.is_some(),
        );
        let tgt_v = self.target().map_or_else(
            || "—".into(),
            |d| format!("{} · {} · {}", d.model, d.dev_path(), fmt_size(d.size)),
        );
        row(
            scene,
            tr,
            card.y0 + 58.0,
            "Drive",
            tgt_v,
            self.target().is_some(),
        );
        // Fit line + mode.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let fit = if tgt.fits(src.size) {
                (
                    format!(
                        "fits — {} free afterwards",
                        fmt_size(tgt.size.saturating_sub(src.size))
                    ),
                    c.semantic.success,
                )
            } else {
                ("image is larger than the drive".into(), c.semantic.danger)
            };
            Self::txt(
                scene,
                tr,
                "Capacity",
                12.0,
                c.text_dim,
                card.x0 + 20.0,
                card.y0 + 92.0,
                None,
                TextWeight::Semibold,
            );
            Self::txt(
                scene,
                tr,
                &fit.0,
                13.0,
                fit.1,
                card.x0 + 96.0,
                card.y0 + 91.0,
                None,
                TextWeight::Medium,
            );
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
        Self::txt(
            scene,
            tr,
            "Action",
            12.0,
            c.text_dim,
            card.x0 + 20.0,
            card.y0 + 122.0,
            None,
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            &act,
            13.0,
            c.text_primary,
            card.x0 + 96.0,
            card.y0 + 121.0,
            Some((card.width() - 116.0) as f32),
            TextWeight::Medium,
        );

        // Danger banner — louder for a fixed/internal (install) target.
        let internal = self.target().is_some_and(|d| !d.removable);
        let system = self.target().is_some_and(|d| d.is_system);
        let banner = Rect::new(
            content.x0,
            card.y1 + 16.0,
            content.x1,
            card.y1 + 16.0 + 44.0,
        );
        Self::fill_rr(
            scene,
            t,
            banner,
            11.0,
            with_alpha(c.semantic.danger, if internal { 40 } else { 28 }),
        );
        Self::stroke_rr(
            scene,
            t,
            banner,
            11.0,
            with_alpha(c.semantic.danger, if internal { 170 } else { 130 }),
            if internal { 1.4 } else { 1.0 },
        );
        let warn = if system {
            "⚠  This is your system disk. Everything on it — the running OS included — is erased. This cannot be undone."
        } else if internal {
            "⚠  This erases the entire internal disk. This cannot be undone."
        } else {
            "⚠  This permanently erases the drive. This cannot be undone."
        };
        Self::txt(
            scene,
            tr,
            warn,
            13.0,
            c.semantic.danger,
            banner.x0 + 16.0,
            banner.y0 + 14.0,
            Some((banner.width() - 32.0) as f32),
            TextWeight::Semibold,
        );

        // Typed confirmation field.
        let field = Rect::new(
            content.x0,
            content.y1 - 96.0,
            content.x1,
            content.y1 - 96.0 + 46.0,
        );
        let focused = self.confirm_focused;
        let matches = self.confirm_matches();
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        Self::txt(
            scene,
            tr,
            "CONFIRM THE TARGET DEVICE",
            11.0,
            c.text_dim,
            field.x0 + 2.0,
            field.y0 - 17.0,
            None,
            TextWeight::Semibold,
        );
        Self::fill_rr(scene, t, field, 11.0, c.neutral[2]);
        Self::stroke_rr(
            scene,
            t,
            field,
            11.0,
            if matches {
                c.semantic.success
            } else if focused {
                c.border_focus
            } else {
                with_alpha(c.semantic.danger, 160)
            },
            if focused || matches { 1.6 } else { 1.2 },
        );
        let (shown, col) = if self.confirm.is_empty() {
            (format!("type “{want}” here"), c.neutral[6])
        } else {
            (
                self.confirm.clone(),
                if matches {
                    c.semantic.success
                } else {
                    c.text_primary
                },
            )
        };
        Self::txt(
            scene,
            tr,
            &shown,
            14.0,
            col,
            field.x0 + 14.0,
            field.y0 + 13.0,
            Some((field.width() - 28.0) as f32),
            TextWeight::Medium,
        );
        if matches {
            self.txt_right(
                scene,
                tr,
                "✓ armed",
                12.5,
                c.semantic.success,
                field.x1 - 14.0,
                field.y0 + 14.0,
                TextWeight::Bold,
            );
        }
        // Gate note.
        let note = if crate::write::allow_write() {
            "Live write enabled (QOS_IMAGER_ALLOW_WRITE=1)."
        } else {
            "Safe mode — no bytes are written unless QOS_IMAGER_ALLOW_WRITE=1."
        };
        Self::txt(
            scene,
            tr,
            note,
            11.0,
            c.neutral[6],
            field.x0 + 2.0,
            field.y1 + 8.0,
            None,
            TextWeight::Regular,
        );
    }

    fn draw_write_progress(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let verb_ing = match self.action {
            Action::Install => "Installing",
            Action::Flash => "Writing",
        };
        let title = if writing {
            format!("{verb_ing}…")
        } else {
            "Verifying…".to_string()
        };
        self.draw_step_title(scene, tr, theme, content, &title, "");

        // Big overall ring.
        let cxp = f64::midpoint(content.x0, content.x1);
        let cyp = content.y0 + 150.0;
        let radius = 76.0;
        let overall = p.overall();
        scene.stroke(
            &Stroke::new(12.0),
            t,
            c.neutral[3],
            None,
            &Circle::new(Point::new(cxp, cyp), radius),
        );
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
            let ring_c = if writing {
                c.semantic.info
            } else {
                c.accent[6]
            };
            scene.stroke(&Stroke::new(12.0), t, ring_c, None, &arc);
        }
        self.txt_center(
            scene,
            tr,
            &format!("{:.0}%", overall * 100.0),
            30.0,
            c.text_primary,
            cxp,
            cyp - 22.0,
            TextWeight::Bold,
        );
        let caption = if writing {
            verb_ing.to_ascii_lowercase()
        } else {
            "verifying".to_string()
        };
        self.txt_center(
            scene,
            tr,
            &caption,
            12.0,
            c.text_dim,
            cxp,
            cyp + 14.0,
            TextWeight::Medium,
        );

        // Rate + ETA line.
        let mut meta = String::new();
        if p.rate_bps > 0 {
            meta.push_str(&fmt_rate(p.rate_bps));
        }
        if let Some(src) = &self.source
            && let Some(eta) = p.eta_secs(src.size)
        {
            if !meta.is_empty() {
                meta.push_str("   ·   ");
            }
            meta.push_str(&format!("~{} left", fmt_eta(eta)));
        }
        if !meta.is_empty() {
            self.txt_center(
                scene,
                tr,
                &meta,
                13.0,
                c.text_dim,
                cxp,
                cyp + radius + 20.0,
                TextWeight::Medium,
            );
        }

        // Two phase bars.
        let bx0 = content.x0 + 40.0;
        let bx1 = content.x1 - 40.0;
        let mut yy = cyp + radius + 56.0;
        for (label, frac, active, fill) in [
            ("Write", p.write_frac, writing, c.semantic.info),
            ("Verify", p.verify_frac, !writing, c.semantic.success),
        ] {
            Self::txt(
                scene,
                tr,
                label,
                12.0,
                if active { c.text_primary } else { c.text_dim },
                bx0,
                yy,
                None,
                TextWeight::Semibold,
            );
            self.txt_right(
                scene,
                tr,
                &format!("{:.0}%", frac * 100.0),
                12.0,
                c.text_dim,
                bx1,
                yy,
                TextWeight::Regular,
            );
            let bar = Rect::new(bx0, yy + 20.0, bx1, yy + 20.0 + 10.0);
            Self::bar(scene, t, bar, frac, c.neutral[3], fill);
            yy += 48.0;
        }
        // Image → drive line.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            self.txt_center(
                scene,
                tr,
                &format!("{}  →  {}", src.name, tgt.dev_path()),
                12.0,
                c.neutral[6],
                cxp,
                yy + 4.0,
                TextWeight::Regular,
            );
        }
    }

    fn draw_write_done(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        ok: bool,
    ) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        let cxp = f64::midpoint(content.x0, content.x1);
        let cyp = content.y0 + 120.0;
        let install = self.action == Action::Install;
        if ok {
            Self::check_disc(
                scene,
                t,
                Point::new(cxp, cyp),
                46.0,
                c.semantic.success,
                Color::WHITE,
            );
            let (headline, sub): (&str, &str) = if install {
                (
                    "qOS is installed",
                    "The disk was written and verified — remove the media and reboot.",
                )
            } else {
                (
                    "Drive is ready",
                    "The image was written and verified — you can safely remove the drive.",
                )
            };
            self.txt_center(
                scene,
                tr,
                headline,
                26.0,
                c.text_primary,
                cxp,
                cyp + 66.0,
                TextWeight::Bold,
            );
            self.txt_center(
                scene,
                tr,
                sub,
                14.0,
                c.text_dim,
                cxp,
                cyp + 100.0,
                TextWeight::Regular,
            );
        } else {
            // Danger disc with an "!".
            scene.fill(
                Fill::NonZero,
                t,
                c.semantic.danger,
                None,
                &Circle::new(Point::new(cxp, cyp), 46.0),
            );
            self.txt_center(
                scene,
                tr,
                "!",
                44.0,
                Color::WHITE,
                cxp,
                cyp - 34.0,
                TextWeight::Bold,
            );
            let headline = if install {
                "Install failed"
            } else {
                "Write failed"
            };
            self.txt_center(
                scene,
                tr,
                headline,
                26.0,
                c.text_primary,
                cxp,
                cyp + 66.0,
                TextWeight::Bold,
            );
            self.txt_center(
                scene,
                tr,
                "The drive was not changed. See the message below.",
                14.0,
                c.text_dim,
                cxp,
                cyp + 100.0,
                TextWeight::Regular,
            );
        }
        // Summary card.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let card = Rect::new(
                content.x0 + 80.0,
                cyp + 140.0,
                content.x1 - 80.0,
                cyp + 140.0 + 84.0,
            );
            Self::fill_rr(scene, t, card, 12.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, card, 12.0, c.border_standard, 1.0);
            Self::txt(
                scene,
                tr,
                "Image",
                12.0,
                c.text_dim,
                card.x0 + 18.0,
                card.y0 + 18.0,
                None,
                TextWeight::Semibold,
            );
            Self::txt(
                scene,
                tr,
                &src.name,
                13.5,
                c.text_primary,
                card.x0 + 90.0,
                card.y0 + 17.0,
                Some((card.width() - 110.0) as f32),
                TextWeight::Medium,
            );
            Self::txt(
                scene,
                tr,
                "Drive",
                12.0,
                c.text_dim,
                card.x0 + 18.0,
                card.y0 + 48.0,
                None,
                TextWeight::Semibold,
            );
            Self::txt(
                scene,
                tr,
                &format!("{} · {}", tgt.model, tgt.dev_path()),
                13.5,
                c.text_primary,
                card.x0 + 90.0,
                card.y0 + 47.0,
                Some((card.width() - 110.0) as f32),
                TextWeight::Medium,
            );
            if ok {
                self.txt_right(
                    scene,
                    tr,
                    "✓ verified",
                    12.5,
                    c.semantic.success,
                    card.x1 - 18.0,
                    card.y0 + 18.0,
                    TextWeight::Bold,
                );
            }
        }
    }
}

// =========================================================================
// Interaction / state mutation
// =========================================================================
impl ImagerApp {
    fn select_source(&mut self, state: &mut AutoCommit, src: Source) {
        if let Some(prev) = &self.source
            && !prev.path.is_empty()
            && !self.recent.iter().any(|s| s.path == prev.path)
        {
            self.recent.insert(0, prev.clone());
            self.recent.truncate(6);
            ImagerDoc::set_recent(state, &self.recent);
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
            PortalOutcome::Failed(reason) => {
                self.set_status(state, &format!("File picker unavailable: {reason}"));
            }
        }
        self.dirty = true;
    }

    fn select_target(&mut self, state: &mut AutoCommit, idx: usize) {
        let Some(drive) = self.targets.get(idx) else {
            return;
        };
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
        let req = WriteRequest {
            source: src,
            target: tgt.clone(),
            action: self.action,
            profile: self.profile.clone(),
        };
        let (start, rx) = spawn_job(req, &self.confirm);
        match start {
            crate::write::JobStart::Started => {
                self.job_rx = rx;
                self.progress = Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.0,
                    verify_frac: 0.0,
                    rate_bps: 0,
                };
                ImagerDoc::set_progress(state, &self.progress);
                let mode = if crate::write::allow_write() {
                    "writing"
                } else {
                    "simulated write"
                };
                self.set_status(
                    state,
                    &format!(
                        "{} → {} ({mode})",
                        self.source
                            .as_ref()
                            .map(|s| s.name.clone())
                            .unwrap_or_default(),
                        tgt.dev_path()
                    ),
                );
                // The write has begun: move onto the live-progress station.
                self.goto(state, Step::Write);
            }
            crate::write::JobStart::ConfirmationMismatch => {
                self.set_status(state, "Confirmation does not match — write refused.");
            }
            crate::write::JobStart::TooSmall => {
                self.set_status(state, "Image is larger than the drive — write refused.");
            }
            crate::write::JobStart::Incomplete => {
                self.set_status(state, "Select a source and drive before writing.");
            }
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
                let name = self
                    .source
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
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
        self.export_text = Some(
            self.profile
                .to_headless_config(self.source.as_ref(), self.target()),
        );
        self.dirty = true;
    }

    /// Edit the Confirm step's typed device-name field. This is a *transient*
    /// arming gesture for the (external) write — presentation, not a document
    /// action — so it writes only the `confirm` scalar. (The profile fields, by
    /// contrast, are real actions handled by [`CustomizePanel`].)
    fn type_into_confirm(&mut self, state: &mut AutoCommit, ch: Option<char>, backspace: bool) {
        if backspace {
            self.confirm.pop();
        } else if let Some(c) = ch
            && !c.is_control()
        {
            self.confirm.push(c);
        }
        let conf = self.confirm.clone();
        ImagerDoc::set_confirm(state, &conf);
        self.dirty = true;
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
                self.selected =
                    remembered.and_then(|name| self.targets.iter().position(|d| d.name == name));
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
                if let Some(src) = self
                    .releases
                    .get(i)
                    .map(super::model::QosRelease::to_source)
                {
                    self.select_source(state, src);
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
                    if self.target().is_some_and(|d| d.is_system) {
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
        self.releases = ImagerDoc::releases(state);
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
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
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
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);
        let lay = self.compute_layout();
        let tr = text_renderer;

        self.draw_header(scene, tr, theme, transform, &lay);
        match self.step {
            Step::Source => self.draw_source_step(scene, tr, theme, transform, &lay),
            Step::Drive => self.draw_drive_step(scene, tr, theme, transform, &lay),
            Step::Customize => {
                self.draw_customize_step(scene, tr, theme, transform, &lay, presence);
            }
            Step::Confirm => self.draw_confirm_step(scene, tr, theme, transform, &lay),
            Step::Write => self.draw_write_progress(scene, tr, theme, transform, &lay),
            Step::Done => self.draw_write_done(
                scene,
                tr,
                theme,
                transform,
                &lay,
                self.progress.phase == JobPhase::Success,
            ),
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
            let removable = self.target().is_some_and(|d| d.removable);
            let profile = self.profile.clone();
            let resp = self.customize.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                &mut self.actions,
                &profile,
                removable,
                lay.content,
                node_id,
                taffy,
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
                // Repaint on move so buttons/cards light up under the pointer.
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                if self.export_text.is_some() {
                    self.export_text = None;
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                let hit = lay
                    .hits
                    .iter()
                    .find(|(r, _)| r.contains(p))
                    .map(|(_, h)| h.clone());
                if let Some(hit) = hit {
                    self.dispatch_hit(state, hit);
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // A click on empty space drops the confirm field's focus.
                self.confirm_focused = false;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.export_text.is_some() {
                    if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                        self.export_text = None;
                        *handled = true;
                        self.dirty = true;
                    }
                    return;
                }
                // The Confirm step's typed device-name field (a transient arming
                // gesture, not a profile edit).
                if self.confirm_focused && self.step == Step::Confirm {
                    match logical_key {
                        Key::Named(NamedKey::Backspace) => {
                            self.type_into_confirm(state, None, true);
                            *handled = true;
                        }
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
                        Key::Named(NamedKey::Space) => {
                            self.type_into_confirm(state, Some(' '), false);
                            *handled = true;
                        }
                        Key::Character(s) => {
                            if let Some(ch) = s.chars().next() {
                                self.type_into_confirm(state, Some(ch), false);
                                *handled = true;
                            }
                        }
                        _ => {}
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
                    Key::Character(s)
                        if matches!(s.as_str(), "r" | "R") && self.step == Step::Drive =>
                    {
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

    fn name(&self) -> &'static str {
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
    fn draw_export(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        text: &str,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let w = (self.rect.width() * 0.62).min(640.0);
        let h = (self.rect.height() * 0.8).min(620.0);
        let cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
        Self::txt(
            scene,
            tr,
            "Unattended install config",
            18.0,
            c.text_primary,
            card.x0 + 20.0,
            card.y0 + 16.0,
            None,
            TextWeight::Bold,
        );
        Self::txt(
            scene,
            tr,
            "qos-install.conf — the same options this wizard sets; secrets are referenced, not embedded.",
            11.5,
            c.text_dim,
            card.x0 + 20.0,
            card.y0 + 42.0,
            Some((card.width() - 40.0) as f32),
            TextWeight::Regular,
        );
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
            Self::txt(
                scene,
                tr,
                line,
                12.0,
                col,
                card.x0 + 20.0,
                y,
                Some((card.width() - 40.0) as f32),
                TextWeight::Regular,
            );
            y += 17.0;
        }
        self.txt_center(
            scene,
            tr,
            "click anywhere or press Esc to close",
            11.0,
            c.neutral[6],
            cx,
            card.y1 - 26.0,
            TextWeight::Regular,
        );
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
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
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
        app.handle_event(
            ev,
            1.0,
            &m,
            doc,
            &automerge::ROOT,
            &mut h,
            &mut ft,
            node,
            &taffy,
        );
    }

    /// Click at a point: move the pointer there (so hover state settles), then
    /// press — exactly the real winit sequence the compositor delivers.
    fn click_at(app: &mut ImagerApp, doc: &mut AutoCommit, p: Point) {
        feed(app, doc, &cursor_ev(p.x, p.y));
        feed(app, doc, &click_ev());
    }

    fn center(r: Rect) -> Point {
        Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
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

    /// A release row is a *write source for erasing a drive*, so the live app
    /// must offer none of its own invention: the list comes from the document,
    /// and a live (unseeded) document has nothing in it. Sample releases are
    /// opt-in via `--demo` / `--shots` like every other sample surface here.
    #[test]
    fn live_app_offers_no_qos_release_to_write() {
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.releases().is_empty(), "no fabricated qOS releases");

        // ...and nothing on the Source step is clickable that would select one.
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        let offered = app
            .compute_layout()
            .hits
            .iter()
            .any(|(_, h)| matches!(h, Hit::Release(_)));
        assert!(
            !offered,
            "no release row may be hit-testable on the live path"
        );
        assert!(
            app.source.is_none(),
            "and nothing is pre-selected as the source"
        );
    }

    /// The gate is a gate, not a deletion: the demo document still offers them.
    #[test]
    fn demo_document_offers_its_seeded_releases() {
        let (mut app, _doc) = seeded_app();
        assert_eq!(app.releases().len(), 2);
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        let offered = app
            .compute_layout()
            .hits
            .iter()
            .filter(|(_, h)| matches!(h, Hit::Release(_)))
            .count();
        assert_eq!(offered, 2, "each seeded release has a row to click");
    }

    #[test]
    fn layout_has_six_step_stations() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let lay = app.compute_layout();
        assert_eq!(lay.crumbs.len(), 6);
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
        tx.send(Progress {
            phase: JobPhase::Success,
            write_frac: 1.0,
            verify_frac: 1.0,
            rate_bps: 0,
        })
        .unwrap();
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
        let iso = app
            .recent
            .iter()
            .find(|s| s.kind == SourceKind::IsoFile)
            .cloned()
            .unwrap();
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
        app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(
            f.path().to_path_buf(),
        )));
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
        assert_eq!(
            app.selected, None,
            "the system disk is not selectable by accident"
        );
        assert!(
            ImagerDoc::status(&doc)
                .to_lowercase()
                .contains("system disk")
        );

        // The explicit acknowledgement unlocks it.
        app.dispatch_hit(&mut doc, Hit::ToggleSystemAck);
        assert!(app.system_ack);
        app.dispatch_hit(&mut doc, Hit::Target(1));
        assert_eq!(
            app.selected,
            Some(1),
            "acknowledged → the system disk can be selected"
        );

        // Withdrawing the acknowledgement drops the selection again.
        app.dispatch_hit(&mut doc, Hit::ToggleSystemAck);
        assert!(!app.system_ack);
        assert_eq!(
            app.selected, None,
            "withdrawing consent deselects the system disk"
        );
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
        assert_eq!(
            app.step,
            Step::Customize,
            "the panel only handles events on this step"
        );
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

        assert_eq!(
            ImagerDoc::profile(&doc).user.password,
            "hunt2",
            "typed value persisted to the doc"
        );
        assert_eq!(
            app.customize.field_value(CzField::Password),
            "hunt2",
            "the input holds the real secret"
        );

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
        assert_eq!(
            ImagerDoc::profile(&doc).hostname,
            "lab01",
            "persisted to the doc"
        );
        assert_eq!(
            app.customize.field_value(CzField::Hostname),
            "lab01",
            "the input holds it"
        );
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
        app.progress = Progress {
            phase: JobPhase::Success,
            write_frac: 1.0,
            verify_frac: 1.0,
            rate_bps: 0,
        };
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
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "encrypt")
            .unwrap();
        let palette_action = match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("encrypt is an immediate action"),
        };

        // Click the real on-screen toggle: move the pointer onto it, then press.
        let rect = CustomizePanel::encrypt_rect(app.content_rect());
        let (cx, cy) = (
            f64::midpoint(rect.x0, rect.x1),
            f64::midpoint(rect.y0, rect.y1),
        );
        feed(&mut app, &mut doc, &cursor_ev(cx, cy));
        feed(&mut app, &mut doc, &click_ev());

        // It applied to the document...
        assert!(
            ImagerDoc::profile(&doc).encrypt,
            "the toggle enabled encryption in the doc"
        );
        // ...recorded exactly one undo boundary...
        assert!(
            app.take_committed_action().is_some(),
            "one undo boundary bracketed the toggle"
        );
        // ...and dispatched the byte-identical action Ctrl+K would.
        let dispatched = app
            .last_dispatched()
            .expect("the toggle dispatched an action");
        assert_eq!(dispatched.type_id, crate::actions::IMAGER_ACTION_TYPE);
        assert_eq!(
            dispatched, &palette_action,
            "the toggle and Ctrl+K are one source of truth"
        );
    }
}
