//! Root widget for the Image Writer (imager) app.
//!
//! `ImagerApp` is a single self-drawing "canvas" widget that renders a linear
//! **wizard** — Source → Drive → Customize → Write — with a persistent step
//! breadcrumb and a Back / primary-action footer, modelled on media writers
//! like Fedora Media Writer and balenaEtcher.
//!
//! * **Source** — pick a local `.iso` / `.img` through the host XDG portal
//!   (never a bespoke picker, per HIG §5), or a known qOS release.
//! * **Drive** — choose a target from the *real* removable block devices
//!   (`lsblk`); internal disks are hidden behind an explicit opt-in.
//! * **Customize** (optional) — pre-configure a qOS image (hostname, first
//!   user, region) so the written media boots ready; plain ISOs are written
//!   as-is and this step is a no-op.
//! * **Write** — a typed device-name confirmation gates a real, byte-for-byte
//!   streaming copy with live progress, then a read-back **verify** pass, then
//!   a done / failure screen.
//!
//! Every write is doubly gated: the typed confirmation here, plus the
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
use ui::portal::{FileDialog, Filter, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::doc::ImagerDoc;
use crate::model::{
    fmt_eta, fmt_rate, fmt_size, Action, InstallMode, InstallProfile, JobPhase, Progress, Source,
    SourceKind, Step, TargetDrive, TargetKind,
};
use crate::write::{spawn_job, WriteRequest};

// ---- layout constants (logical px) -------------------------------------
const OUTER: f64 = 28.0;
const HEADER_H: f64 = 30.0;
const CRUMB_TOP: f64 = 78.0;
const CRUMB_H: f64 = 34.0;
const FOOTER_H: f64 = 78.0;
const CONTENT_MAX_W: f64 = 900.0;

/// Which text field, if any, is currently capturing keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Hostname,
    Username,
    FullName,
    Password,
    Timezone,
    Keyboard,
    Confirm,
}

/// A clickable hit-target for one frame. Pure function of the viewport +
/// state, so `draw` and `handle_event` agree without ordering hazards.
#[derive(Clone)]
enum Hit {
    PickFile,
    Release(usize),
    Recent(usize),
    Target(usize),
    ToggleInternal,
    Rescan,
    Field(Field),
    ToggleAutologin,
    ToggleEncrypt,
    TogglePersist,
    ExportConfig,
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

/// The exact rects of the Customize step, computed once and shared by
/// `layout_customize` (hit-testing) and `draw_customize_step` (painting) so the
/// two can never drift out of alignment.
struct CustomizeGeom {
    identity_label_y: f64,
    hostname: Rect,
    full_name: Rect,
    username: Rect,
    password: Rect,
    timezone: Rect,
    region_label_y: f64,
    keyboard: Rect,
    autologin: Rect,
    encrypt: Rect,
    persist: Rect,
    export: Rect,
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
    export_text: Option<String>,
    focus: Option<Field>,
    // ---- render caches ----
    seeded: bool,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    hover_target: Option<usize>,
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
            export_text: None,
            focus: None,
            seeded: false,
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            hover_target: None,
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

    /// Can the flow move forward from `from`? (Also gates breadcrumb jumps.)
    fn can_advance(&self, from: Step) -> bool {
        match from {
            Step::Source => self.source.is_some(),
            Step::Drive => self.selected.is_some(),
            Step::Customize => true,
            Step::Write => false,
        }
    }

    /// Whether a completed/available breadcrumb step can be jumped to.
    fn crumb_reachable(&self, s: Step) -> bool {
        match s {
            Step::Source => true,
            Step::Drive => self.source.is_some(),
            Step::Customize => self.selected.is_some() && self.customizable(),
            Step::Write => self.selected.is_some(),
        }
    }

    /// Only qOS images can be pre-configured; a plain ISO is written as-is.
    fn customizable(&self) -> bool {
        self.source.as_ref().map(|s| s.installable).unwrap_or(false)
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

    /// A small check-mark glyph inside a filled disc of radius `rad` at `c`.
    fn check_disc(scene: &mut Scene, t: Affine, center: Point, rad: f64, disc: Color, tick: Color) {
        scene.fill(Fill::NonZero, t, disc, None, &Circle::new(center, rad));
        let mut p = BezPath::new();
        p.move_to((center.x - rad * 0.42, center.y + rad * 0.02));
        p.line_to((center.x - rad * 0.10, center.y + rad * 0.34));
        p.line_to((center.x + rad * 0.46, center.y - rad * 0.34));
        scene.stroke(&Stroke::new(rad * 0.24), t, tick, None, &p);
    }

    /// A pill toggle switch with a trailing label. Returns nothing; the caller
    /// registers the hit-rect.
    fn draw_toggle(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, r: Rect, label: &str, sub: &str, on: bool) {
        let c = &theme.tokens.colors;
        let hov = r.contains(self.last_mouse);
        Self::fill_rr(scene, t, r, 12.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, r, 12.0, if on { with_alpha(c.semantic.success, 150) } else { c.border_standard }, 1.0);
        // Switch.
        let sw = Rect::new(r.x0 + 14.0, r.y0 + (r.height() - 22.0) / 2.0, r.x0 + 14.0 + 40.0, r.y0 + (r.height() - 22.0) / 2.0 + 22.0);
        Self::fill_rr(scene, t, sw, 11.0, if on { c.semantic.success } else { c.neutral[4] });
        let knob_x = if on { sw.x1 - 20.0 } else { sw.x0 + 2.0 };
        let knob = Rect::new(knob_x, sw.y0 + 2.0, knob_x + 18.0, sw.y1 - 2.0);
        Self::fill_rr(scene, t, knob, 9.0, Color::WHITE);
        let tx = sw.x1 + 12.0;
        self.txt(scene, tr, label, 13.5, c.text_primary, tx, r.y0 + 12.0, Some((r.x1 - tx - 12.0) as f32), TextWeight::Semibold);
        if !sub.is_empty() {
            self.txt(scene, tr, sub, 11.5, c.text_dim, tx, r.y0 + 30.0, Some((r.x1 - tx - 12.0) as f32), TextWeight::Regular);
        }
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
        let top = CRUMB_TOP + CRUMB_H + 22.0;
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

        // ---- Breadcrumb pills ----
        let steps = Step::all();
        let crumb_w = 150.0f64;
        let gap = 20.0f64;
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
            Step::Customize => self.layout_customize(content, &mut hits),
            Step::Write => self.layout_write(content, &mut hits),
        }

        // ---- Footer (Back + primary) ----
        let fy = v.y1 - FOOTER_H;
        let by = fy + (FOOTER_H - 44.0) / 2.0;
        let (mut back, mut primary) = (None, None);
        let show_back = matches!(self.step, Step::Drive | Step::Customize)
            || (self.step == Step::Write && self.progress.phase == JobPhase::Idle);
        if show_back {
            let r = Rect::new(content.x0, by, content.x0 + 120.0, by + 44.0);
            hits.push((r, Hit::Back));
            back = Some(r);
        }
        if let Some(p) = self.primary_button_rect(content, by) {
            let hit = match self.step {
                Step::Write if self.progress.phase == JobPhase::Idle => Hit::StartWrite,
                Step::Write if matches!(self.progress.phase, JobPhase::Success | JobPhase::Failed) => {
                    Hit::WriteAnother
                }
                Step::Write => Hit::Next, // active: no-op placeholder, not registered below
                _ => Hit::Next,
            };
            if !(self.step == Step::Write && self.progress.phase.is_active()) {
                hits.push((p, hit));
                primary = Some(p);
            }
        }

        Layout { crumbs, back, primary, hits, content }
    }

    /// The primary action button rect, right-aligned in the footer — or `None`
    /// when the current step has no primary (mid-write).
    fn primary_button_rect(&self, content: Rect, by: f64) -> Option<Rect> {
        if self.step == Step::Write && self.progress.phase.is_active() {
            return None;
        }
        let w = match (self.step, self.progress.phase) {
            (Step::Write, JobPhase::Idle) => 240.0,
            (Step::Write, _) => 190.0,
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
    }

    /// Shared geometry for the Customize step. Uses a deliberate vertical
    /// rhythm so section labels, field labels and inputs never overlap.
    fn customize_geom(&self, content: Rect) -> CustomizeGeom {
        let x0 = content.x0;
        let x1 = content.x1;
        let fw = (content.width() - 16.0) / 2.0;
        let fh = 38.0;
        let row_pitch = fh + 40.0; // input + label + gap
        // Identity block: label, then hostname, then two field rows.
        let identity_label_y = content.y0 + 52.0;
        let host_y = content.y0 + 92.0;
        let hostname = Rect::new(x0, host_y, x1, host_y + fh);
        let r2 = host_y + row_pitch;
        let full_name = Rect::new(x0, r2, x0 + fw, r2 + fh);
        let username = Rect::new(x0 + fw + 16.0, r2, x1, r2 + fh);
        let r3 = r2 + row_pitch;
        let password = Rect::new(x0, r3, x0 + fw, r3 + fh);
        let timezone = Rect::new(x0 + fw + 16.0, r3, x1, r3 + fh);
        // Region & options block (same label→field rhythm as Identity above).
        let region_label_y = r3 + fh + 26.0;
        let kb_y = region_label_y + 40.0;
        let keyboard = Rect::new(x0, kb_y, x0 + fw, kb_y + fh);
        let tog_y = kb_y + fh + 22.0;
        let th = 46.0;
        let autologin = Rect::new(x0, tog_y, x0 + fw, tog_y + th);
        let encrypt = Rect::new(x0 + fw + 16.0, tog_y, x1, tog_y + th);
        let p_y = tog_y + th + 12.0;
        let persist = Rect::new(x0, p_y, x0 + fw, p_y + th);
        let export = Rect::new(x1 - 250.0, p_y + 8.0, x1, p_y + 8.0 + 32.0);
        CustomizeGeom {
            identity_label_y,
            hostname,
            full_name,
            username,
            password,
            timezone,
            region_label_y,
            keyboard,
            autologin,
            encrypt,
            persist,
            export,
        }
    }

    fn layout_customize(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        if !self.customizable() {
            return; // informational only
        }
        let g = self.customize_geom(content);
        hits.push((g.hostname, Hit::Field(Field::Hostname)));
        hits.push((g.full_name, Hit::Field(Field::FullName)));
        hits.push((g.username, Hit::Field(Field::Username)));
        hits.push((g.password, Hit::Field(Field::Password)));
        hits.push((g.timezone, Hit::Field(Field::Timezone)));
        hits.push((g.keyboard, Hit::Field(Field::Keyboard)));
        hits.push((g.autologin, Hit::ToggleAutologin));
        hits.push((g.encrypt, Hit::ToggleEncrypt));
        if self.target().map(|d| d.removable).unwrap_or(false) {
            hits.push((g.persist, Hit::TogglePersist));
        }
        hits.push((g.export, Hit::ExportConfig));
    }

    fn layout_write(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        if self.progress.phase == JobPhase::Idle {
            // The typed-confirmation field sits under the review card.
            let field = Rect::new(content.x0, content.y1 - 96.0, content.x1, content.y1 - 96.0 + 46.0);
            hits.push((field, Hit::Field(Field::Confirm)));
        }
    }
}

// =========================================================================
// Header + breadcrumb
// =========================================================================
impl ImagerApp {
    fn draw_header(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let v = self.rect;
        // Title + subtitle.
        self.txt(scene, tr, "Image Writer", 24.0, c.text_primary, v.x0 + OUTER, v.y0 + 18.0, None, TextWeight::Bold);
        self.txt(
            scene,
            tr,
            "Write a disk image to a USB drive",
            13.0,
            c.text_dim,
            v.x0 + OUTER + 190.0,
            v.y0 + 26.0,
            None,
            TextWeight::Regular,
        );
        // A subtle status chip on the right of the header.
        if !self.status.is_empty() {
            let col = match self.progress.phase {
                JobPhase::Failed => c.semantic.danger,
                JobPhase::Success => c.semantic.success,
                _ => c.text_dim,
            };
            self.txt_right(scene, tr, &self.status, 12.0, col, v.x1 - OUTER, v.y0 + 24.0, TextWeight::Medium);
        }
        let _ = HEADER_H;

        // ---- Breadcrumb ----
        for (i, (r, s)) in lay.crumbs.iter().enumerate() {
            let done = s.index() < self.step.index() && self.crumb_reachable(*s);
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
            let disc_c = Point::new(r.x0 + 22.0, (r.y0 + r.y1) / 2.0);
            if done {
                Self::check_disc(scene, t, disc_c, 11.0, c.semantic.success, Color::WHITE);
            } else {
                let ring = if current { c.accent[5] } else { c.neutral[4] };
                scene.fill(Fill::NonZero, t, ring, None, &Circle::new(disc_c, 11.0));
                self.txt_center(scene, tr, &format!("{}", s.index() + 1), 12.0, Color::WHITE, disc_c.x, disc_c.y - 8.0, TextWeight::Bold);
            }
            let label_c = if current { c.text_primary } else if done { c.text_primary } else { c.text_dim };
            self.txt(scene, tr, s.title(), 13.5, label_c, r.x0 + 40.0, (r.y0 + r.y1) / 2.0 - 9.0, None, if current { TextWeight::Semibold } else { TextWeight::Medium });
            // Connector to the next pill.
            if i + 1 < lay.crumbs.len() {
                let nx0 = lay.crumbs[i + 1].0.x0;
                let midy = (r.y0 + r.y1) / 2.0;
                let cc = if s.index() < self.step.index() { c.semantic.success } else { c.neutral[3] };
                scene.stroke(&Stroke::new(2.0), t, cc, None, &Line::new(Point::new(r.x1 + 4.0, midy), Point::new(nx0 - 4.0, midy)));
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
            Step::Write if self.progress.phase == JobPhase::Idle && !self.confirm_matches() => {
                self.target().map(|d| format!("Type “{}” to unlock the write", d.name))
            }
            Step::Write if self.progress.phase.is_active() => Some("Do not remove the drive…".into()),
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
            Step::Write => match self.progress.phase {
                JobPhase::Idle => {
                    let dev = self.target().map(|d| d.dev_path()).unwrap_or_default();
                    (format!("Erase & write  {dev}"), self.confirm_matches() && self.image_fits(), true)
                }
                JobPhase::Success => ("Write another".to_string(), true, false),
                JobPhase::Failed => ("Try again".to_string(), true, false),
                _ => ("Writing…".to_string(), false, false),
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
        self.draw_step_title(scene, tr, theme, content, "Choose an image", "Pick a local .iso or .img file, or a qOS release below.");
        let mut y = content.y0 + 44.0;

        // Browse drop-zone.
        let browse = Rect::new(content.x0, y, content.x1, y + 96.0);
        let hov = browse.contains(self.last_mouse);
        Self::fill_rr(scene, t, browse, 14.0, if hov { c.neutral[2] } else { with_alpha(c.accent[5], 16) });
        Self::dashed_rr(scene, t, browse, 14.0, if hov { c.accent[6] } else { with_alpha(c.accent[5], 160) }, 1.6);
        // Download/arrow glyph.
        let gx = browse.x0 + 46.0;
        let gy = (browse.y0 + browse.y1) / 2.0;
        let mut arr = BezPath::new();
        arr.move_to((gx, gy - 14.0));
        arr.line_to((gx, gy + 8.0));
        scene.stroke(&Stroke::new(2.4), t, c.accent[6], None, &arr);
        let mut head = BezPath::new();
        head.move_to((gx - 7.0, gy + 1.0));
        head.line_to((gx, gy + 9.0));
        head.line_to((gx + 7.0, gy + 1.0));
        scene.stroke(&Stroke::new(2.4), t, c.accent[6], None, &head);
        scene.stroke(&Stroke::new(2.4), t, c.accent[6], None, &Line::new(Point::new(gx - 12.0, gy + 15.0), Point::new(gx + 12.0, gy + 15.0)));
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
            self.txt(scene, tr, &src.summary(), 12.0, c.text_dim, card.x0 + 108.0, card.y0 + 42.0, Some((card.width() - 130.0) as f32), TextWeight::Regular);
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
        self.draw_step_title(scene, tr, theme, content, "Select a drive", "Everything on the chosen drive will be erased.");
        // Detected count (to the left of the Rescan button).
        let detected = if self.show_internal {
            format!("{} drive(s)", self.targets.len())
        } else {
            format!("{} removable", self.removable_count())
        };
        self.txt_right(scene, tr, &detected, 11.5, c.text_dim, content.x1 - 116.0, content.y0 + 8.0, TextWeight::Medium);
        // Rescan button (top-right).
        let rescan = Rect::new(content.x1 - 104.0, content.y0, content.x1, content.y0 + 30.0);
        let hov = rescan.contains(self.last_mouse);
        Self::fill_rr(scene, t, rescan, 9.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, rescan, 9.0, c.border_standard, 1.0);
        self.txt_center(scene, tr, "⟳ Rescan", 12.5, c.text_primary, (rescan.x0 + rescan.x1) / 2.0, rescan.y0 + 7.0, TextWeight::Medium);

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
        let tr_rect = Rect::new(content.x0, content.y1 - 44.0, content.x0 + 340.0, content.y1 - 44.0 + 34.0);
        let hov = tr_rect.contains(self.last_mouse);
        Self::fill_rr(scene, t, tr_rect, 10.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, tr_rect, 10.0, if self.show_internal { with_alpha(c.semantic.danger, 150) } else { c.border_standard }, 1.0);
        let box_r = Rect::new(tr_rect.x0 + 12.0, tr_rect.y0 + 9.0, tr_rect.x0 + 12.0 + 16.0, tr_rect.y0 + 9.0 + 16.0);
        Self::fill_rr(scene, t, box_r, 4.0, if self.show_internal { c.semantic.danger } else { c.neutral[4] });
        if self.show_internal {
            Self::check_disc(scene, t, Point::new((box_r.x0 + box_r.x1) / 2.0, (box_r.y0 + box_r.y1) / 2.0), 8.0, c.semantic.danger, Color::WHITE);
        }
        self.txt(scene, tr, "Show internal disks (dangerous)", 12.5, c.text_primary, box_r.x1 + 10.0, tr_rect.y0 + 9.0, None, TextWeight::Medium);
    }

    fn draw_drive_empty(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, content: Rect) {
        let c = &theme.tokens.colors;
        let card = Rect::new(content.x0, content.y0 + 60.0, content.x1, content.y0 + 60.0 + 240.0);
        Self::fill_rr(scene, t, card, 16.0, c.surfaces.raised);
        Self::dashed_rr(scene, t, card, 16.0, c.border_standard, 1.4);
        let cx = (card.x0 + card.x1) / 2.0;
        // A simple USB-stick glyph.
        let gy = card.y0 + 66.0;
        let body = Rect::new(cx - 16.0, gy, cx + 16.0, gy + 54.0);
        Self::fill_rr(scene, t, body, 6.0, c.neutral[4]);
        let cap = Rect::new(cx - 10.0, gy - 16.0, cx + 10.0, gy);
        Self::fill_rr(scene, t, cap, 3.0, c.neutral[5]);
        self.txt_center(scene, tr, "Insert a USB drive", 17.0, c.text_primary, cx, card.y0 + 140.0, TextWeight::Semibold);
        self.txt_center(scene, tr, "Plug in a USB stick or SD card, then press Rescan.", 13.0, c.text_dim, cx, card.y0 + 168.0, TextWeight::Regular);
        self.txt_center(scene, tr, "No removable drives detected.", 12.0, c.neutral[6], cx, card.y0 + 196.0, TextWeight::Regular);
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

        // Selection check on the left.
        if selected {
            Self::check_disc(scene, t, Point::new(r.x0 + 28.0, (r.y0 + r.y1) / 2.0), 13.0, c.accent[5], Color::WHITE);
        }
        let left = if selected { r.x0 + 54.0 } else { r.x0 + 18.0 };

        // Kind badge.
        let badge = Rect::new(left, r.y0 + 18.0, left + 54.0, r.y0 + 18.0 + 24.0);
        Self::fill_rr(scene, t, badge, 6.0, kind_color(theme, drive.kind));
        self.txt_center(scene, tr, drive.kind.tag(), 12.0, Color::WHITE, (badge.x0 + badge.x1) / 2.0, badge.y0 + 5.0, TextWeight::Bold);

        // Removable / fixed chip on the right.
        let (chip_label, chip_c) = if drive.removable { ("REMOVABLE", c.semantic.success) } else { ("INTERNAL DISK", c.semantic.danger) };
        let (cw, _) = tr.measure_weighted(chip_label, 10.5, None, TextWeight::Bold);
        let chip = Rect::new(r.x1 - 16.0 - cw as f64 - 18.0, r.y0 + 18.0, r.x1 - 16.0, r.y0 + 18.0 + 24.0);
        Self::fill_rr(scene, t, chip, 6.0, with_alpha(chip_c, 40));
        self.txt_center(scene, tr, chip_label, 10.5, chip_c, (chip.x0 + chip.x1) / 2.0, chip.y0 + 6.0, TextWeight::Bold);

        // Model + capacity.
        self.txt(scene, tr, &drive.model, 16.0, c.text_primary, left, r.y0 + 50.0, Some((r.width() - (left - r.x0) - 30.0) as f32), TextWeight::Semibold);
        self.txt(scene, tr, &format!("{}  ·  {}", drive.dev_path(), fmt_size(drive.size)), 12.5, c.text_dim, left, r.y0 + 72.0, None, TextWeight::Regular);

        // Destructive line.
        self.txt(scene, tr, &format!("⚠ Erases {}", drive.contents_summary()), 11.5, c.semantic.danger, left, r.y1 - 24.0, Some((r.width() - (left - r.x0) - 30.0) as f32), TextWeight::Medium);
    }

    fn draw_customize_step(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
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
        self.draw_step_title(scene, tr, theme, content, "Customize (optional)", "Pre-configure the written media so it boots ready to use.");

        let g = self.customize_geom(content);
        self.section_label(scene, tr, theme, "IDENTITY", content.x0, g.identity_label_y);
        self.draw_field(scene, tr, theme, t, g.hostname, Field::Hostname, "Hostname", &self.profile.hostname, false);
        self.draw_field(scene, tr, theme, t, g.full_name, Field::FullName, "Full name", &self.profile.user.full_name, false);
        self.draw_field(scene, tr, theme, t, g.username, Field::Username, "Username", &self.profile.user.username, false);
        self.draw_field(scene, tr, theme, t, g.password, Field::Password, "Password", &self.profile.user.password, true);
        self.draw_field(scene, tr, theme, t, g.timezone, Field::Timezone, "Time zone", &self.profile.timezone, false);

        self.section_label(scene, tr, theme, "REGION & OPTIONS", content.x0, g.region_label_y);
        self.draw_field(scene, tr, theme, t, g.keyboard, Field::Keyboard, "Keyboard", &self.profile.keyboard, false);
        self.draw_toggle(scene, tr, theme, t, g.autologin, "Auto-login", "Sign in without a password prompt", self.profile.user.autologin);
        self.draw_toggle(scene, tr, theme, t, g.encrypt, "Encrypt storage", "Protect user data with LUKS", self.profile.encrypt);
        if self.target().map(|d| d.removable).unwrap_or(false) {
            let persist = self.profile.mode == InstallMode::Persistent;
            self.draw_toggle(scene, tr, theme, t, g.persist, "Persistent storage", "Keep changes across reboots on the stick", persist);
        }
        // Export config (secondary).
        let hov = g.export.contains(self.last_mouse);
        Self::fill_rr(scene, t, g.export, 9.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, g.export, 9.0, c.accent[6], 1.0);
        self.txt_center(scene, tr, "⤓ Export unattended config", 12.0, c.accent[7], (g.export.x0 + g.export.x1) / 2.0, g.export.y0 + 8.0, TextWeight::Semibold);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_field(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, r: Rect, f: Field, label: &str, value: &str, secret: bool) {
        let c = &theme.tokens.colors;
        self.txt(scene, tr, label, 11.0, c.text_dim, r.x0 + 2.0, r.y0 - 17.0, None, TextWeight::Semibold);
        let focused = self.focus == Some(f);
        Self::fill_rr(scene, t, r, 9.0, c.neutral[2]);
        Self::stroke_rr(scene, t, r, 9.0, if focused { c.border_focus } else { c.border_standard }, if focused { 1.6 } else { 1.0 });
        let shown = if secret {
            "•".repeat(value.chars().count().min(16))
        } else if value.is_empty() {
            String::new()
        } else {
            value.to_string()
        };
        self.txt(scene, tr, &shown, 13.5, c.text_primary, r.x0 + 12.0, r.y0 + 10.0, Some((r.width() - 24.0) as f32), TextWeight::Regular);
        if focused {
            let (tw, _) = tr.measure_weighted(&shown, 13.5, None, TextWeight::Regular);
            let cx = r.x0 + 12.0 + tw as f64 + 2.0;
            scene.stroke(&Stroke::new(1.4), t, c.accent[6], None, &Line::new(Point::new(cx, r.y0 + 8.0), Point::new(cx, r.y1 - 8.0)));
        }
    }

    fn draw_write_step(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        match self.progress.phase {
            JobPhase::Idle => self.draw_write_review(scene, tr, theme, t, lay),
            JobPhase::Writing | JobPhase::Verifying => self.draw_write_progress(scene, tr, theme, t, lay),
            JobPhase::Success => self.draw_write_done(scene, tr, theme, t, lay, true),
            JobPhase::Failed => self.draw_write_done(scene, tr, theme, t, lay, false),
        }
    }

    fn draw_write_review(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        self.draw_step_title(scene, tr, theme, content, "Review & write", "Confirm the target, then type its name to unlock the write.");

        // Review card: Image → Drive.
        let card = Rect::new(content.x0, content.y0 + 46.0, content.x1, content.y0 + 46.0 + 150.0);
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
        let row = |this: &Self, scene: &mut Scene, tr: &mut TextRenderer, y: f64, k: &str, v: String, ok: bool| {
            this.txt(scene, tr, k, 12.0, c.text_dim, card.x0 + 20.0, y, None, TextWeight::Semibold);
            let col = if ok { c.text_primary } else { c.semantic.danger };
            this.txt(scene, tr, &v, 14.0, col, card.x0 + 96.0, y - 1.0, Some((card.width() - 116.0) as f32), TextWeight::Semibold);
        };
        let src_v = self.source.as_ref().map(|s| format!("{}   ({})", s.name, fmt_size(s.size))).unwrap_or_else(|| "—".into());
        row(self, scene, tr, card.y0 + 24.0, "Image", src_v, self.source.is_some());
        let tgt_v = self.target().map(|d| format!("{} · {} · {}", d.model, d.dev_path(), fmt_size(d.size))).unwrap_or_else(|| "—".into());
        row(self, scene, tr, card.y0 + 58.0, "Drive", tgt_v, self.target().is_some());
        // Fit line + mode.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let fit = if tgt.fits(src.size) {
                (format!("fits — {} free after write", fmt_size(tgt.size.saturating_sub(src.size))), c.semantic.success)
            } else {
                ("image is larger than the drive".into(), c.semantic.danger)
            };
            self.txt(scene, tr, "Capacity", 12.0, c.text_dim, card.x0 + 20.0, card.y0 + 92.0, None, TextWeight::Semibold);
            self.txt(scene, tr, &fit.0, 13.0, fit.1, card.x0 + 96.0, card.y0 + 91.0, None, TextWeight::Medium);
        }
        // Action summary.
        let act = if self.customizable() && self.action == Action::Install {
            format!("Write qOS + pre-config ({}, user {})", self.profile.hostname, self.profile.user.username)
        } else {
            "Raw image write, then read-back verify".to_string()
        };
        self.txt(scene, tr, "Action", 12.0, c.text_dim, card.x0 + 20.0, card.y0 + 122.0, None, TextWeight::Semibold);
        self.txt(scene, tr, &act, 13.0, c.text_primary, card.x0 + 96.0, card.y0 + 121.0, Some((card.width() - 116.0) as f32), TextWeight::Medium);

        // Danger banner.
        let banner = Rect::new(content.x0, card.y1 + 16.0, content.x1, card.y1 + 16.0 + 44.0);
        Self::fill_rr(scene, t, banner, 11.0, with_alpha(c.semantic.danger, 28));
        Self::stroke_rr(scene, t, banner, 11.0, with_alpha(c.semantic.danger, 130), 1.0);
        self.txt(scene, tr, "⚠  This permanently erases the drive. This cannot be undone.", 13.0, c.semantic.danger, banner.x0 + 16.0, banner.y0 + 14.0, None, TextWeight::Semibold);

        // Typed confirmation field.
        let field = Rect::new(content.x0, content.y1 - 96.0, content.x1, content.y1 - 96.0 + 46.0);
        let focused = self.focus == Some(Field::Confirm);
        let matches = self.confirm_matches();
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        self.txt(scene, tr, "CONFIRM THE TARGET DEVICE", 11.0, c.text_dim, field.x0 + 2.0, field.y0 - 17.0, None, TextWeight::Semibold);
        Self::fill_rr(scene, t, field, 11.0, c.neutral[2]);
        Self::stroke_rr(scene, t, field, 11.0, if matches { c.semantic.success } else if focused { c.border_focus } else { with_alpha(c.semantic.danger, 160) }, if focused || matches { 1.6 } else { 1.2 });
        let (shown, col) = if self.confirm.is_empty() {
            (format!("type “{want}” here"), c.neutral[6])
        } else {
            (self.confirm.clone(), if matches { c.semantic.success } else { c.text_primary })
        };
        self.txt(scene, tr, &shown, 14.0, col, field.x0 + 14.0, field.y0 + 13.0, Some((field.width() - 28.0) as f32), TextWeight::Medium);
        if matches {
            self.txt_right(scene, tr, "✓ armed", 12.5, c.semantic.success, field.x1 - 14.0, field.y0 + 14.0, TextWeight::Bold);
        }
        // Gate note.
        let note = if crate::write::allow_write() {
            "Live write enabled (QOS_IMAGER_ALLOW_WRITE=1)."
        } else {
            "Safe mode — no bytes are written unless QOS_IMAGER_ALLOW_WRITE=1."
        };
        self.txt(scene, tr, note, 11.0, c.neutral[6], field.x0 + 2.0, field.y1 + 8.0, None, TextWeight::Regular);
    }

    fn draw_write_progress(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let title = if writing { "Writing…" } else { "Verifying…" };
        self.draw_step_title(scene, tr, theme, content, title, "");

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
        self.txt_center(scene, tr, if writing { "writing" } else { "verifying" }, 12.0, c.text_dim, cxp, cyp + 14.0, TextWeight::Medium);

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
        if ok {
            Self::check_disc(scene, t, Point::new(cxp, cyp), 46.0, c.semantic.success, Color::WHITE);
            self.txt_center(scene, tr, "Done", 26.0, c.text_primary, cxp, cyp + 66.0, TextWeight::Bold);
            self.txt_center(scene, tr, "The image was written and verified.", 14.0, c.text_dim, cxp, cyp + 100.0, TextWeight::Regular);
        } else {
            // Danger disc with an "!".
            scene.fill(Fill::NonZero, t, c.semantic.danger, None, &Circle::new(Point::new(cxp, cyp), 46.0));
            self.txt_center(scene, tr, "!", 44.0, Color::WHITE, cxp, cyp - 34.0, TextWeight::Bold);
            self.txt_center(scene, tr, "Write failed", 26.0, c.text_primary, cxp, cyp + 66.0, TextWeight::Bold);
            self.txt_center(scene, tr, "The drive was not written. See the message below.", 14.0, c.text_dim, cxp, cyp + 100.0, TextWeight::Regular);
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
            FileDialog::open("Choose a disk image")
                .accept_label("Choose")
                .filter(Filter::new("Disk images").glob("*.iso").glob("*.img").glob("*.raw").mimetype("application/x-iso9660-image"))
                .thread_name("imager-portal")
                .spawn(),
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
            PortalOutcome::OpenedMany(paths) => {
                if let Some(src) = paths.first().and_then(|p| crate::probe::inspect_source(p)) {
                    self.select_source(state, src);
                } else {
                    self.set_status(state, "That file is not a readable disk image.");
                }
            }
            PortalOutcome::Cancelled => self.set_status(state, "File selection cancelled."),
            PortalOutcome::Failed(reason) => self.set_status(state, &format!("File picker unavailable: {reason}")),
        }
        self.dirty = true;
    }

    fn select_target(&mut self, state: &mut AutoCommit, idx: usize) {
        if idx < self.targets.len() {
            self.selected = Some(idx);
            ImagerDoc::set_selected(state, Some(idx));
            self.confirm.clear();
            ImagerDoc::set_confirm(state, "");
            self.dirty = true;
        }
    }

    fn set_status(&mut self, state: &mut AutoCommit, s: &str) {
        self.status = s.to_string();
        ImagerDoc::set_status(state, s);
    }

    fn goto(&mut self, state: &mut AutoCommit, s: Step) {
        self.step = s;
        ImagerDoc::set_step(state, s);
        self.focus = None;
        self.dirty = true;
    }

    fn advance(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Source if self.can_advance(Step::Source) => self.goto(state, Step::Drive),
            Step::Drive if self.can_advance(Step::Drive) => {
                // A qOS image goes through Customize; a plain ISO skips it.
                if self.customizable() {
                    self.action = Action::Install;
                    ImagerDoc::set_action(state, Action::Install);
                    self.goto(state, Step::Customize);
                } else {
                    self.goto(state, Step::Write);
                }
            }
            Step::Customize => self.goto(state, Step::Write),
            _ => {}
        }
    }

    fn back(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Write if self.customizable() => self.goto(state, Step::Customize),
            Step::Write => self.goto(state, Step::Drive),
            Step::Customize => self.goto(state, Step::Drive),
            Step::Drive => self.goto(state, Step::Source),
            Step::Source => {}
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
                let msg = if p.phase == JobPhase::Success {
                    format!("Done — {} written and verified", self.source.as_ref().map(|s| s.name.clone()).unwrap_or_default())
                } else {
                    "Write failed.".to_string()
                };
                self.set_status(state, &msg);
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

    fn type_into_field(&mut self, state: &mut AutoCommit, f: Field, ch: Option<char>, backspace: bool) {
        let target: &mut String = match f {
            Field::Hostname => &mut self.profile.hostname,
            Field::FullName => &mut self.profile.user.full_name,
            Field::Username => &mut self.profile.user.username,
            Field::Password => &mut self.profile.user.password,
            Field::Timezone => &mut self.profile.timezone,
            Field::Keyboard => &mut self.profile.keyboard,
            Field::Confirm => &mut self.confirm,
        };
        if backspace {
            target.pop();
        } else if let Some(c) = ch {
            if !c.is_control() {
                target.push(c);
            }
        }
        if f == Field::Confirm {
            let conf = self.confirm.clone();
            ImagerDoc::set_confirm(state, &conf);
        } else {
            ImagerDoc::set_profile(state, &self.profile);
        }
        self.dirty = true;
    }

    fn do_rescan(&mut self, state: &mut AutoCommit) {
        if std::env::var("IMAGER_LIVE").is_ok() {
            if let Some(t) = crate::probe::probe() {
                self.targets = t.clone();
                ImagerDoc::write_targets(state, &t);
            }
            self.set_status(state, &format!("Rescanned — {} drive(s)", self.targets.len()));
        } else {
            self.set_status(state, "Rescanned drives");
        }
        self.selected = self.selected.filter(|i| *i < self.targets.len());
        ImagerDoc::set_selected(state, self.selected);
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
                self.dirty = true;
            }
            Hit::Rescan => self.do_rescan(state),
            Hit::Field(f) => {
                self.focus = Some(f);
                self.dirty = true;
            }
            Hit::ToggleAutologin => {
                self.profile.user.autologin = !self.profile.user.autologin;
                ImagerDoc::set_profile(state, &self.profile);
                self.dirty = true;
            }
            Hit::ToggleEncrypt => {
                self.profile.encrypt = !self.profile.encrypt;
                ImagerDoc::set_profile(state, &self.profile);
                self.dirty = true;
            }
            Hit::TogglePersist => {
                self.profile.mode = if self.profile.mode == InstallMode::Persistent { InstallMode::Live } else { InstallMode::Persistent };
                ImagerDoc::set_profile(state, &self.profile);
                self.dirty = true;
            }
            Hit::ExportConfig => self.export_config(state),
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
                let targets = if std::env::var("IMAGER_LIVE").is_ok() {
                    crate::probe::probe().unwrap_or_else(crate::model::sample_targets)
                } else {
                    crate::model::sample_targets()
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
        self.selected = ImagerDoc::selected(state).filter(|i| *i < self.targets.len());
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

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);
        let lay = self.compute_layout();
        let tr = text_renderer;

        self.draw_header(scene, tr, theme, transform, &lay);
        match self.step {
            Step::Source => self.draw_source_step(scene, tr, theme, transform, &lay),
            Step::Drive => self.draw_drive_step(scene, tr, theme, transform, &lay),
            Step::Customize => self.draw_customize_step(scene, tr, theme, transform, &lay),
            Step::Write => self.draw_write_step(scene, tr, theme, transform, &lay),
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
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton};
        use winit::keyboard::{Key, NamedKey};

        let lay = self.compute_layout();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                // Which drive card (if any) is under the pointer, resolved from
                // this frame's hit-rects. Computed into a local first so the
                // `visible_targets()` borrow ends before we write the field.
                let hover_now = lay.hits.iter().find_map(|(r, h)| match h {
                    Hit::Target(i) if r.contains(p) => Some(*i),
                    _ => None,
                });
                self.hover_target = hover_now;
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
                if let Some((_, hit)) = lay.hits.iter().find(|(r, _)| r.contains(p)) {
                    self.dispatch_hit(state, hit.clone());
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                self.focus = None;
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
                // Field typing.
                if let Some(f) = self.focus {
                    match logical_key {
                        Key::Named(NamedKey::Backspace) => {
                            self.type_into_field(state, f, None, true);
                            *handled = true;
                        }
                        Key::Named(NamedKey::Enter) => {
                            if f == Field::Confirm && self.step == Step::Write {
                                self.start_write(state);
                            }
                            self.focus = None;
                            *handled = true;
                            self.dirty = true;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.focus = None;
                            *handled = true;
                            self.dirty = true;
                        }
                        Key::Named(NamedKey::Space) => {
                            self.type_into_field(state, f, Some(' '), false);
                            *handled = true;
                        }
                        Key::Character(s) => {
                            if let Some(ch) = s.chars().next() {
                                self.type_into_field(state, f, Some(ch), false);
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
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    #[test]
    fn sync_seeds_sample_state_at_source_step() {
        let (app, _doc) = seeded_app();
        assert_eq!(app.targets.len(), 2);
        assert!(app.source.is_some());
        assert_eq!(app.step, Step::Source);
        assert_eq!(app.recent.len(), 3);
    }

    #[test]
    fn layout_has_four_breadcrumb_steps() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let lay = app.compute_layout();
        assert_eq!(lay.crumbs.len(), 4);
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
        assert_eq!(app.step, Step::Write);
    }

    #[test]
    fn plain_iso_skips_customize() {
        let (mut app, mut doc) = seeded_app();
        let iso = app.recent.iter().find(|s| s.kind == SourceKind::IsoFile).cloned().unwrap();
        app.select_source(&mut doc, iso);
        app.advance(&mut doc); // → Drive
        app.select_target(&mut doc, 0);
        app.advance(&mut doc); // installable == false → straight to Write
        assert_eq!(app.step, Step::Write);
        assert!(!app.customizable());
    }

    #[test]
    fn write_refused_without_matching_confirmation() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.goto(&mut doc, Step::Write);
        app.confirm = "wrong".into();
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
        assert!(app.job_rx.is_none());
        assert!(ImagerDoc::status(&doc).to_lowercase().contains("confirm"));
    }

    #[test]
    fn write_starts_with_matching_confirmation_but_stays_gated() {
        // With the gate off, a matching confirmation starts a *simulated* job:
        // the phase advances to Writing and a channel is installed, but no
        // device is ever opened (proven in write.rs).
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.goto(&mut doc, Step::Write);
        app.confirm = "sdb".into();
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Writing);
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

    #[test]
    fn typing_into_hostname_persists_to_profile() {
        let (mut app, mut doc) = seeded_app();
        app.focus = Some(Field::Hostname);
        for _ in 0..app.profile.hostname.len() {
            app.type_into_field(&mut doc, Field::Hostname, None, true);
        }
        for ch in "lab01".chars() {
            app.type_into_field(&mut doc, Field::Hostname, Some(ch), false);
        }
        assert_eq!(app.profile.hostname, "lab01");
        assert_eq!(ImagerDoc::profile(&doc).hostname, "lab01");
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
}
