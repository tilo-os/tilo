//! Root widget for the Install Media (imager) app.
//!
//! `ImagerApp` is a single self-drawing "canvas" widget. It paints the
//! whole three-column flow — **Source → Target → Action / Options** —
//! plus the destructive confirm modal (with a typed device-name gate) and
//! the write/verify progress UI, directly into the Vello scene. All mutable
//! state (selected source/target, the live install profile, the typed
//! confirmation, job progress) is written back into the Automerge document
//! so it is undoable and replicates to collaborators.
//!
//! The real write never happens here: triggering a job hands off to
//! [`crate::write::begin_job`], which is gated behind the typed
//! confirmation and a worker seam that touches no device in this build.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::doc::ImagerDoc;
use crate::model::{
    fmt_rate, fmt_size, Action, DiskLayout, InstallMode, InstallProfile, JobPhase, Progress,
    Source, SourceKind, TargetDrive, TargetKind,
};
use crate::write::{begin_job, JobStart, WriteRequest};

// ---- layout constants (logical px) -------------------------------------
const PAD: f64 = 18.0;
const TOPBAR_H: f64 = 60.0;
const COL_GAP: f64 = 16.0;
const SRC_W: f64 = 312.0;
const TGT_W: f64 = 360.0;

/// Which install-option field, if any, is currently capturing keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Hostname,
    Username,
    FullName,
    Password,
    Locale,
    Keyboard,
    Timezone,
    Confirm,
}

/// A clickable hit-target for one frame. Pure function of the viewport +
/// state, so `draw` and `handle_event` agree without ordering hazards.
#[derive(Clone)]
enum Hit {
    PickIso,
    PickQos,
    Recent(usize),
    Release(usize),
    Target(usize),
    Action(Action),
    Mode(InstallMode),
    Layout(DiskLayout),
    ToggleEncrypt,
    ToggleAutologin,
    LoadProfile(usize),
    ExportConfig,
    StartJob,
    Field(Field),
    ModalConfirm,
    ModalCancel,
}

struct Layout {
    // Source column.
    pick_iso: Rect,
    pick_qos: Rect,
    source_card: Rect,
    recent: Vec<Rect>,
    releases: Vec<Rect>,
    // Target column.
    targets: Vec<Rect>,
    // Options column.
    action_tabs: Vec<(Rect, Action)>,
    options: Rect,
    modes: Vec<(Rect, InstallMode)>,
    fields: Vec<(Rect, Field)>,
    layouts: Vec<(Rect, DiskLayout)>,
    encrypt: Rect,
    autologin: Rect,
    profiles: Vec<Rect>,
    export: Rect,
    start: Rect,
    confirm_field: Rect,
    // Modal / result.
    modal: Option<ModalLayout>,
    hits: Vec<(Rect, Hit)>,
}

#[derive(Clone, Copy)]
struct ModalLayout {
    card: Rect,
    cancel: Rect,
    confirm: Rect,
}

pub struct ImagerApp {
    source: Option<Source>,
    recent: Vec<Source>,
    targets: Vec<TargetDrive>,
    selected: Option<usize>,
    action: Action,
    profile: InstallProfile,
    profiles: Vec<(String, InstallProfile)>,
    progress: Progress,
    confirm: String,
    status: String,
    /// Set when the destructive confirm modal is open.
    modal_open: bool,
    /// The exported headless config, shown in an overlay when present.
    export_text: Option<String>,
    focus: Option<Field>,
    seeded: bool,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    hover: Option<usize>,
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
            profiles: Vec::new(),
            progress: Progress::default(),
            confirm: String::new(),
            status: String::new(),
            modal_open: false,
            export_text: None,
            focus: None,
            seeded: false,
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            hover: None,
        }
    }

    fn target(&self) -> Option<&TargetDrive> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    /// Releases offered in the source column (static for the demo).
    fn releases(&self) -> Vec<crate::model::QosRelease> {
        crate::model::sample_releases()
    }

    // ---- layout --------------------------------------------------------
    fn compute_layout(&self) -> Layout {
        let v = self.rect;
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let content_top = TOPBAR_H + PAD;
        let content_bottom = v.y1 - PAD;

        // ---- Source column ----
        let sx0 = v.x0 + PAD;
        let sx1 = sx0 + SRC_W;
        let pick_w = (SRC_W - 10.0) / 2.0;
        let pick_iso = Rect::new(sx0, content_top + 22.0, sx0 + pick_w, content_top + 22.0 + 38.0);
        let pick_qos = Rect::new(sx0 + pick_w + 10.0, content_top + 22.0, sx1, content_top + 22.0 + 38.0);
        hits.push((pick_iso, Hit::PickIso));
        hits.push((pick_qos, Hit::PickQos));

        let source_card = Rect::new(sx0, pick_iso.y1 + 14.0, sx1, pick_iso.y1 + 14.0 + 96.0);

        // qOS releases list.
        let mut releases = Vec::new();
        let mut ry = source_card.y1 + 40.0;
        for (i, _) in self.releases().iter().enumerate() {
            let r = Rect::new(sx0, ry, sx1, ry + 46.0);
            releases.push(r);
            hits.push((r, Hit::Release(i)));
            ry += 46.0 + 8.0;
        }

        // Recent sources list.
        let mut recent = Vec::new();
        let mut yy = ry + 28.0;
        for (i, _) in self.recent.iter().enumerate() {
            if yy + 40.0 > content_bottom {
                break;
            }
            let r = Rect::new(sx0, yy, sx1, yy + 40.0);
            recent.push(r);
            hits.push((r, Hit::Recent(i)));
            yy += 40.0 + 6.0;
        }

        // ---- Target column ----
        let tx0 = sx1 + COL_GAP;
        let tx1 = tx0 + TGT_W;
        let mut targets = Vec::new();
        let mut ty = content_top + 26.0;
        for (i, _) in self.targets.iter().enumerate() {
            let r = Rect::new(tx0, ty, tx1, ty + 104.0);
            targets.push(r);
            hits.push((r, Hit::Target(i)));
            ty += 104.0 + 12.0;
        }

        // ---- Options column ----
        let ox0 = tx1 + COL_GAP;
        let ox1 = v.x1 - PAD;

        // Action tabs (Write ISO / Install qOS).
        let mut action_tabs = Vec::new();
        let tab_w = (ox1 - ox0 - 10.0) / 2.0;
        for (i, a) in [Action::Flash, Action::Install].into_iter().enumerate() {
            let x = ox0 + i as f64 * (tab_w + 10.0);
            let r = Rect::new(x, content_top, x + tab_w, content_top + 38.0);
            action_tabs.push((r, a));
            hits.push((r, Hit::Action(a)));
        }

        let options = Rect::new(ox0, content_top + 38.0 + 14.0, ox1, content_bottom - 64.0);

        // Fields depend on action.
        let mut modes = Vec::new();
        let mut fields = Vec::new();
        let mut layouts = Vec::new();
        let mut encrypt = Rect::ZERO;
        let mut autologin = Rect::ZERO;
        let mut profiles = Vec::new();
        let mut export = Rect::ZERO;

        if self.action == Action::Install {
            let ix = options.x0 + 16.0;
            let iw = options.width() - 32.0;
            let mut y = options.y0 + 38.0;

            // Mode selector (3 pills).
            let mode_w = (iw - 16.0) / 3.0;
            for (i, m) in InstallMode::all().into_iter().enumerate() {
                let x = ix + i as f64 * (mode_w + 8.0);
                let r = Rect::new(x, y, x + mode_w, y + 34.0);
                modes.push((r, m));
                hits.push((r, Hit::Mode(m)));
            }
            y += 34.0 + 22.0;

            // Text fields: hostname, full name, username, password.
            let field_specs = [
                Field::Hostname,
                Field::FullName,
                Field::Username,
                Field::Password,
                Field::Locale,
                Field::Keyboard,
                Field::Timezone,
            ];
            // Two-column grid.
            let fw = (iw - 14.0) / 2.0;
            for (i, f) in field_specs.iter().enumerate() {
                let col = (i % 2) as f64;
                let row = (i / 2) as f64;
                let x = ix + col * (fw + 14.0);
                let fy = y + row * (54.0) + 16.0;
                let r = Rect::new(x, fy, x + fw, fy + 32.0);
                fields.push((r, *f));
                hits.push((r, Hit::Field(*f)));
            }
            y += 4.0 * 54.0 + 24.0;

            // Disk layout pills.
            let lay_w = (iw - 16.0) / 3.0;
            for (i, l) in DiskLayout::all().into_iter().enumerate() {
                let x = ix + i as f64 * (lay_w + 8.0);
                let r = Rect::new(x, y + 16.0, x + lay_w, y + 16.0 + 32.0);
                layouts.push((r, l));
                hits.push((r, Hit::Layout(l)));
            }
            y += 16.0 + 32.0 + 16.0;

            // Encryption + autologin toggles.
            encrypt = Rect::new(ix, y, ix + fw, y + 28.0);
            autologin = Rect::new(ix + fw + 14.0, y, ix + iw, y + 28.0);
            hits.push((encrypt, Hit::ToggleEncrypt));
            hits.push((autologin, Hit::ToggleAutologin));
            y += 28.0 + 18.0;

            // Saved profiles row.
            let n = self.profiles.len().max(1);
            let pw = (iw - (n as f64 - 1.0) * 8.0) / n as f64;
            for (i, _) in self.profiles.iter().enumerate() {
                let x = ix + i as f64 * (pw + 8.0);
                let r = Rect::new(x, y + 16.0, x + pw, y + 16.0 + 30.0);
                profiles.push(r);
                hits.push((r, Hit::LoadProfile(i)));
            }
            y += 16.0 + 30.0 + 14.0;

            // Export headless config button.
            export = Rect::new(ix, y, ix + iw, y + 32.0);
            hits.push((export, Hit::ExportConfig));
        }

        // Confirm field + Start button (bottom of options column).
        let bar_y = content_bottom - 56.0;
        let confirm_field = Rect::new(ox0, bar_y, ox0 + (ox1 - ox0) * 0.52, bar_y + 40.0);
        hits.push((confirm_field, Hit::Field(Field::Confirm)));
        let start = Rect::new(confirm_field.x1 + 12.0, bar_y, ox1, bar_y + 40.0);
        hits.push((start, Hit::StartJob));

        // ---- Confirm modal ----
        let modal = if self.modal_open {
            let w = 520.0;
            let h = 280.0;
            let cx = (v.x0 + v.x1) / 2.0;
            let cy = (v.y0 + v.y1) / 2.0;
            let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
            let by = card.y1 - 18.0 - 40.0;
            let confirm = Rect::new(card.x1 - 18.0 - 160.0, by, card.x1 - 18.0, by + 40.0);
            let cancel = Rect::new(confirm.x0 - 12.0 - 120.0, by, confirm.x0 - 12.0, by + 40.0);
            hits.push((confirm, Hit::ModalConfirm));
            hits.push((cancel, Hit::ModalCancel));
            Some(ModalLayout { card, cancel, confirm })
        } else {
            None
        };

        Layout {
            pick_iso,
            pick_qos,
            source_card,
            recent,
            releases,
            targets,
            action_tabs,
            options,
            modes,
            fields,
            layouts,
            encrypt,
            autologin,
            profiles,
            export,
            start,
            confirm_field,
            modal,
            hits,
        }
    }

    // ---- drawing primitives -------------------------------------------
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

    // ---- top bar -------------------------------------------------------
    fn draw_topbar(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, TOPBAR_H);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new(Point::new(bar.x0, bar.y1), Point::new(bar.x1, bar.y1)),
        );
        self.txt(scene, tr, "Install Media", 22.0, c.text_primary, PAD, 14.0, None, TextWeight::Bold);
        self.txt(
            scene,
            tr,
            "Write ISOs and install qOS to USB or disk",
            13.0,
            c.text_dim,
            PAD + 188.0,
            21.0,
            None,
            TextWeight::Regular,
        );

        // Flow stepper on the right: Source › Target › Options.
        let steps = ["1 Source", "2 Target", "3 Options"];
        let on = [
            self.source.is_some(),
            self.selected.is_some(),
            self.source.is_some() && self.selected.is_some(),
        ];
        let mut x = self.rect.x1 - PAD - 300.0;
        for (i, label) in steps.iter().enumerate() {
            let col = if on[i] { c.accent[5] } else { c.neutral[5] };
            self.txt(scene, tr, label, 12.5, col, x, 22.0, None, if on[i] { TextWeight::Semibold } else { TextWeight::Medium });
            x += 92.0;
            if i < steps.len() - 1 {
                self.txt(scene, tr, "›", 13.0, c.text_dim, x - 14.0, 22.0, None, TextWeight::Regular);
            }
        }
    }

    fn section_label(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, s: &str, x: f64, y: f64) {
        self.txt(scene, tr, s, 11.0, theme.tokens.colors.text_dim, x, y, None, TextWeight::Semibold);
    }

    // ---- source column -------------------------------------------------
    fn draw_source(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let x0 = lay.pick_iso.x0;
        self.section_label(scene, tr, theme, "SOURCE IMAGE", x0, TOPBAR_H + PAD);

        // Two picker buttons.
        for (r, label, kind) in [
            (lay.pick_iso, "Browse .iso…", SourceKind::IsoFile),
            (lay.pick_qos, "Install qOS", SourceKind::QosImage),
        ] {
            let active = self.source.as_ref().map(|s| s.kind) == Some(kind);
            let hov = r.contains(self.last_mouse);
            let bg = if active { c.accent[5] } else if hov { c.neutral[3] } else { c.surfaces.raised };
            Self::fill_rr(scene, t, r, 9.0, bg);
            Self::stroke_rr(scene, t, r, 9.0, if active { c.border_focus } else { c.border_standard }, 1.0);
            let fg = if active { Color::WHITE } else { c.text_primary };
            self.txt_center(scene, tr, label, 13.5, fg, (r.x0 + r.x1) / 2.0, r.y0 + 10.0, TextWeight::Semibold);
        }

        // Selected-source info card.
        let card = lay.source_card;
        Self::fill_rr(scene, t, card, 12.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 12.0, c.border_standard, 1.0);
        if let Some(src) = &self.source {
            let badge = Rect::new(card.x0 + 14.0, card.y0 + 14.0, card.x0 + 14.0 + 44.0, card.y0 + 14.0 + 22.0);
            Self::fill_rr(scene, t, badge, 6.0, source_color(theme, src.kind));
            self.txt_center(scene, tr, src.kind.tag(), 12.0, Color::WHITE, (badge.x0 + badge.x1) / 2.0, badge.y0 + 4.0, TextWeight::Bold);
            self.txt(scene, tr, &src.name, 14.5, c.text_primary, card.x0 + 14.0, card.y0 + 42.0, Some((card.width() - 28.0) as f32), TextWeight::Semibold);
            self.txt(scene, tr, &src.summary(), 11.5, c.text_dim, card.x0 + 14.0, card.y0 + 64.0, Some((card.width() - 28.0) as f32), TextWeight::Regular);
            if src.installable {
                self.txt_right(scene, tr, "● installable", 11.0, c.semantic.success, card.x1 - 14.0, card.y0 + 18.0, TextWeight::Medium);
            }
        } else {
            self.txt(scene, tr, "No source selected", 13.0, c.text_dim, card.x0 + 14.0, card.y0 + 38.0, None, TextWeight::Regular);
            self.txt(scene, tr, "Browse for an .iso or pick a qOS image.", 11.5, c.neutral[6], card.x0 + 14.0, card.y0 + 60.0, Some((card.width() - 28.0) as f32), TextWeight::Regular);
        }

        // qOS releases.
        self.section_label(scene, tr, theme, "qOS RELEASES", x0, card.y1 + 18.0);
        for (i, rel) in self.releases().iter().enumerate() {
            let Some(&r) = lay.releases.get(i) else { continue };
            let hov = r.contains(self.last_mouse);
            let selected = self
                .source
                .as_ref()
                .map(|s| s.kind == SourceKind::QosImage && s.name == format!("{} {}", rel.name, rel.version))
                .unwrap_or(false);
            Self::fill_rr(scene, t, r, 9.0, if selected { c.accent[2] } else if hov { c.neutral[3] } else { c.surfaces.raised });
            Self::stroke_rr(scene, t, r, 9.0, if selected { c.border_focus } else { c.border_standard }, 1.0);
            self.txt(scene, tr, &format!("qOS {}", rel.version), 13.5, c.text_primary, r.x0 + 12.0, r.y0 + 7.0, None, TextWeight::Semibold);
            let sub = if rel.local_path.is_empty() {
                format!("{}  ·  {}  ·  download", rel.channel, fmt_size(rel.size))
            } else {
                format!("{}  ·  {}  ·  local", rel.channel, fmt_size(rel.size))
            };
            self.txt(scene, tr, &sub, 11.0, c.text_dim, r.x0 + 12.0, r.y0 + 26.0, None, TextWeight::Regular);
        }

        // Recent sources.
        if !lay.recent.is_empty() {
            let ry0 = lay.releases.last().map(|r| r.y1).unwrap_or(card.y1) + 18.0;
            self.section_label(scene, tr, theme, "RECENT", x0, ry0);
        }
        for (i, src) in self.recent.iter().enumerate() {
            let Some(&r) = lay.recent.get(i) else { continue };
            let hov = r.contains(self.last_mouse);
            if hov {
                Self::fill_rr(scene, t, r, 8.0, c.neutral[3]);
            }
            let sw = Rect::new(r.x0 + 4.0, r.y0 + 8.0, r.x0 + 10.0, r.y1 - 8.0);
            Self::fill_rr(scene, t, sw, 2.0, source_color(theme, src.kind));
            self.txt(scene, tr, &src.name, 12.5, c.text_primary, r.x0 + 18.0, r.y0 + 4.0, Some((r.width() - 70.0) as f32), TextWeight::Medium);
            self.txt(scene, tr, &fmt_size(src.size), 11.0, c.text_dim, r.x0 + 18.0, r.y0 + 22.0, None, TextWeight::Regular);
            self.txt_right(scene, tr, src.kind.tag(), 10.5, c.neutral[6], r.x1 - 10.0, r.y0 + 13.0, TextWeight::Semibold);
        }
    }

    // ---- target column -------------------------------------------------
    fn draw_target(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let x0 = lay.targets.first().map(|r| r.x0).unwrap_or(self.rect.x0 + PAD + SRC_W + COL_GAP);
        self.section_label(scene, tr, theme, "TARGET DRIVE", x0, TOPBAR_H + PAD);
        self.txt_right(scene, tr, &format!("{} detected", self.targets.len()), 11.0, c.text_dim, x0 + TGT_W, TOPBAR_H + PAD, TextWeight::Regular);

        for (i, drive) in self.targets.iter().enumerate() {
            let Some(&r) = lay.targets.get(i) else { continue };
            let selected = self.selected == Some(i);
            let hover = self.hover == Some(i) && !selected;
            let danger = !drive.removable; // fixed/internal disks are louder
            let bg = if selected { c.accent[2] } else if hover { c.neutral[3] } else { c.surfaces.raised };
            Self::fill_rr(scene, t, r, 12.0, bg);
            let border = if selected {
                c.border_focus
            } else if danger {
                with_alpha(c.semantic.danger, 150)
            } else {
                c.border_standard
            };
            Self::stroke_rr(scene, t, r, 12.0, border, if selected { 1.8 } else { 1.0 });

            // Kind badge.
            let badge = Rect::new(r.x0 + 14.0, r.y0 + 14.0, r.x0 + 14.0 + 52.0, r.y0 + 14.0 + 22.0);
            Self::fill_rr(scene, t, badge, 6.0, kind_color(theme, drive.kind));
            self.txt_center(scene, tr, drive.kind.tag(), 12.0, Color::WHITE, (badge.x0 + badge.x1) / 2.0, badge.y0 + 4.0, TextWeight::Bold);

            // Removable / fixed chip on the right.
            let (chip_label, chip_c) = if drive.removable {
                ("REMOVABLE", c.semantic.success)
            } else {
                ("FIXED DISK", c.semantic.danger)
            };
            let (cw, _) = tr.measure_weighted(chip_label, 10.5, None, TextWeight::Bold);
            let chip = Rect::new(r.x1 - 14.0 - cw as f64 - 18.0, r.y0 + 14.0, r.x1 - 14.0, r.y0 + 14.0 + 22.0);
            Self::fill_rr(scene, t, chip, 6.0, with_alpha(chip_c, 40));
            self.txt_center(scene, tr, chip_label, 10.5, chip_c, (chip.x0 + chip.x1) / 2.0, chip.y0 + 5.0, TextWeight::Bold);

            // Model + capacity.
            self.txt(scene, tr, &drive.model, 15.0, c.text_primary, r.x0 + 14.0, r.y0 + 42.0, Some((r.width() - 28.0) as f32), TextWeight::Semibold);
            let sub = format!("{}  ·  {}", drive.dev_path(), fmt_size(drive.size));
            self.txt(scene, tr, &sub, 12.0, c.text_dim, r.x0 + 14.0, r.y0 + 63.0, None, TextWeight::Regular);

            // Destructive warning line — always present.
            self.txt(scene, tr, &format!("⚠ erases {}", drive.contents_summary()), 11.0, c.semantic.danger, r.x0 + 14.0, r.y1 - 22.0, Some((r.width() - 28.0) as f32), TextWeight::Medium);
        }

        if self.targets.is_empty() {
            self.txt(scene, tr, "No drives detected.", 14.0, c.text_dim, x0, TOPBAR_H + PAD + 30.0, None, TextWeight::Regular);
        }
    }

    // ---- options column ------------------------------------------------
    fn draw_options(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        // Action tabs.
        for (r, a) in &lay.action_tabs {
            let active = *a == self.action;
            let enabled = match a {
                Action::Install => self.source.as_ref().map(|s| s.installable).unwrap_or(false),
                Action::Flash => true,
            };
            let bg = if active { c.accent[5] } else { c.surfaces.raised };
            Self::fill_rr(scene, t, *r, 9.0, bg);
            Self::stroke_rr(scene, t, *r, 9.0, if active { c.border_focus } else { c.border_standard }, 1.0);
            let fg = if active { Color::WHITE } else if enabled { c.text_primary } else { c.neutral[6] };
            self.txt_center(scene, tr, a.title(), 14.0, fg, (r.x0 + r.x1) / 2.0, r.y0 + 10.0, TextWeight::Semibold);
        }

        // Options body.
        let body = lay.options;
        Self::fill_rr(scene, t, body, 12.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, body, 12.0, c.border_standard, 1.0);

        match self.action {
            Action::Flash => self.draw_flash_body(scene, tr, theme, t, body),
            Action::Install => self.draw_install_body(scene, tr, theme, t, lay),
        }

        // Confirm field + Start button.
        self.draw_action_bar(scene, tr, theme, t, lay);
    }

    fn draw_flash_body(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, body: Rect) {
        let c = &theme.tokens.colors;
        let x = body.x0 + 18.0;
        self.txt(scene, tr, "Raw image write", 16.0, c.text_primary, x, body.y0 + 18.0, None, TextWeight::Bold);
        self.txt(
            scene,
            tr,
            "Copies the image byte-for-byte onto the drive (dd-style), then re-reads it and compares a blake3 digest to verify.",
            12.5,
            c.text_dim,
            x,
            body.y0 + 44.0,
            Some((body.width() - 36.0) as f32),
            TextWeight::Regular,
        );

        // What → where summary.
        let mut y = body.y0 + 96.0;
        let row = |this: &Self, scene: &mut Scene, tr: &mut TextRenderer, k: &str, val: String, ok: bool, y: f64| {
            this.txt(scene, tr, k, 12.0, c.text_dim, x, y, None, TextWeight::Medium);
            let col = if ok { c.text_primary } else { c.semantic.danger };
            this.txt(scene, tr, &val, 13.0, col, x + 70.0, y, Some((body.width() - 36.0 - 70.0) as f32), TextWeight::Semibold);
        };
        let src_v = self.source.as_ref().map(|s| format!("{} ({})", s.name, fmt_size(s.size))).unwrap_or_else(|| "— pick a source —".into());
        row(self, scene, tr, "Image", src_v, self.source.is_some(), y);
        y += 30.0;
        let tgt_v = self.target().map(|d| format!("{} · {}", d.dev_path(), fmt_size(d.size))).unwrap_or_else(|| "— pick a target —".into());
        row(self, scene, tr, "Drive", tgt_v, self.target().is_some(), y);
        y += 30.0;
        // Fit check.
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            if tgt.fits(src.size) {
                row(self, scene, tr, "Fits", format!("yes — {} free after write", fmt_size(tgt.size.saturating_sub(src.size))), true, y);
            } else {
                row(self, scene, tr, "Fits", "NO — image larger than drive".into(), false, y);
            }
        }

        // Progress block (always reserve the space so layout is stable).
        self.draw_progress(scene, tr, theme, t, Rect::new(body.x0 + 18.0, body.y1 - 86.0, body.x1 - 18.0, body.y1 - 14.0));
    }

    fn draw_install_body(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        let body = lay.options;
        let x = body.x0 + 16.0;
        self.section_label(scene, tr, theme, "INSTALL MODE", x, body.y0 + 16.0);
        for (r, m) in &lay.modes {
            let active = *m == self.profile.mode;
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(scene, t, *r, 8.0, if active { c.accent[5] } else if hov { c.neutral[3] } else { c.neutral[2] });
            Self::stroke_rr(scene, t, *r, 8.0, if active { c.border_focus } else { c.border_standard }, 1.0);
            let fg = if active { Color::WHITE } else { c.text_primary };
            let short = match m {
                InstallMode::Live => "Live stick",
                InstallMode::Persistent => "Persistent",
                InstallMode::DiskInstall => "To disk",
            };
            self.txt_center(scene, tr, short, 12.5, fg, (r.x0 + r.x1) / 2.0, r.y0 + 9.0, TextWeight::Semibold);
        }

        // Text fields.
        let label_for = |f: Field| match f {
            Field::Hostname => ("Hostname", self.profile.hostname.clone()),
            Field::FullName => ("Full name", self.profile.user.full_name.clone()),
            Field::Username => ("Username", self.profile.user.username.clone()),
            Field::Password => ("Password", "•".repeat(self.profile.user.password.chars().count().min(12))),
            Field::Locale => ("Locale", self.profile.locale.clone()),
            Field::Keyboard => ("Keyboard", self.profile.keyboard.clone()),
            Field::Timezone => ("Timezone", self.profile.timezone.clone()),
            _ => ("", String::new()),
        };
        for (r, f) in &lay.fields {
            let (label, val) = label_for(*f);
            self.txt(scene, tr, label, 10.5, c.text_dim, r.x0, r.y0 - 15.0, None, TextWeight::Semibold);
            let focused = self.focus == Some(*f);
            Self::fill_rr(scene, t, *r, 7.0, c.neutral[2]);
            Self::stroke_rr(scene, t, *r, 7.0, if focused { c.border_focus } else { c.border_standard }, if focused { 1.6 } else { 1.0 });
            let shown = if val.is_empty() { "—".to_string() } else { val };
            self.txt(scene, tr, &shown, 12.5, c.text_primary, r.x0 + 10.0, r.y0 + 7.0, Some((r.width() - 20.0) as f32), TextWeight::Regular);
            if focused {
                let (tw, _) = tr.measure_weighted(&shown, 12.5, None, TextWeight::Regular);
                let cx = r.x0 + 10.0 + tw as f64 + 2.0;
                scene.stroke(&Stroke::new(1.4), t, c.accent[6], None, &Line::new(Point::new(cx, r.y0 + 6.0), Point::new(cx, r.y1 - 6.0)));
            }
        }

        // Disk layout.
        if let Some((first, _)) = lay.layouts.first() {
            self.section_label(scene, tr, theme, "DISK LAYOUT", x, first.y0 - 16.0);
        }
        for (r, l) in &lay.layouts {
            let active = *l == self.profile.layout;
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(scene, t, *r, 8.0, if active { c.accent[5] } else if hov { c.neutral[3] } else { c.neutral[2] });
            Self::stroke_rr(scene, t, *r, 8.0, if active { c.border_focus } else { c.border_standard }, 1.0);
            let fg = if active { Color::WHITE } else { c.text_primary };
            let short = match l {
                DiskLayout::WholeDisk => "Whole disk",
                DiskLayout::SeparateHome => "Sep. /home",
                DiskLayout::BtrfsSubvols => "Btrfs",
            };
            self.txt_center(scene, tr, short, 12.0, fg, (r.x0 + r.x1) / 2.0, r.y0 + 8.0, TextWeight::Medium);
        }

        // Encryption + autologin toggles.
        self.draw_toggle(scene, tr, theme, t, lay.encrypt, "Encrypt disk (LUKS)", self.profile.encrypt);
        self.draw_toggle(scene, tr, theme, t, lay.autologin, "Auto-login user", self.profile.user.autologin);

        // Saved profiles.
        if let Some(first) = lay.profiles.first() {
            self.section_label(scene, tr, theme, "SAVED PROFILES", x, first.y0 - 16.0);
        }
        for (i, r) in lay.profiles.iter().enumerate() {
            let name = self.profiles.get(i).map(|(n, _)| n.as_str()).unwrap_or("");
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(scene, t, *r, 8.0, if hov { c.neutral[3] } else { c.neutral[2] });
            Self::stroke_rr(scene, t, *r, 8.0, c.border_standard, 1.0);
            self.txt_center(scene, tr, name, 12.0, c.text_primary, (r.x0 + r.x1) / 2.0, r.y0 + 8.0, TextWeight::Medium);
        }

        // Export headless config.
        let hov = lay.export.contains(self.last_mouse);
        Self::fill_rr(scene, t, lay.export, 9.0, if hov { c.accent[4] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, lay.export, 9.0, c.accent[6], 1.0);
        self.txt_center(scene, tr, "⤓  Export unattended install config", 12.5, c.accent[7], (lay.export.x0 + lay.export.x1) / 2.0, lay.export.y0 + 8.0, TextWeight::Semibold);
    }

    fn draw_toggle(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, r: Rect, label: &str, on: bool) {
        let c = &theme.tokens.colors;
        let hov = r.contains(self.last_mouse);
        Self::fill_rr(scene, t, r, 8.0, if hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, r, 8.0, c.border_standard, 1.0);
        // Switch knob.
        let sw = Rect::new(r.x0 + 10.0, r.y0 + 6.0, r.x0 + 10.0 + 34.0, r.y1 - 6.0);
        Self::fill_rr(scene, t, sw, 8.0, if on { c.semantic.success } else { c.neutral[4] });
        let knob_x = if on { sw.x1 - 14.0 } else { sw.x0 + 2.0 };
        let knob = Rect::new(knob_x, sw.y0 + 2.0, knob_x + 12.0, sw.y1 - 2.0);
        Self::fill_rr(scene, t, knob, 6.0, Color::WHITE);
        self.txt(scene, tr, label, 12.0, c.text_primary, sw.x1 + 10.0, r.y0 + 7.0, Some((r.width() - 70.0) as f32), TextWeight::Medium);
    }

    fn draw_action_bar(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, lay: &Layout) {
        let c = &theme.tokens.colors;
        // Typed confirmation field — the safety gate before a write.
        let r = lay.confirm_field;
        let focused = self.focus == Some(Field::Confirm);
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        let matches = !self.confirm.is_empty() && (self.confirm == want || self.confirm == format!("/dev/{want}"));
        Self::fill_rr(scene, t, r, 9.0, c.neutral[2]);
        Self::stroke_rr(scene, t, r, 9.0, if matches { c.semantic.success } else if focused { c.border_focus } else { c.semantic.danger }, if focused || matches { 1.6 } else { 1.0 });
        let placeholder = if want.is_empty() {
            "type the target device name to confirm".to_string()
        } else {
            format!("type \"{want}\" to confirm")
        };
        let (shown, col) = if self.confirm.is_empty() {
            (placeholder, c.neutral[6])
        } else {
            (self.confirm.clone(), if matches { c.semantic.success } else { c.text_primary })
        };
        self.txt(scene, tr, &shown, 13.0, col, r.x0 + 12.0, r.y0 + 11.0, Some((r.width() - 24.0) as f32), TextWeight::Medium);

        // Start button — destructive accent, disabled until ready.
        let ready = self.can_start();
        let s = lay.start;
        let hov = s.contains(self.last_mouse);
        let bg = if !ready {
            c.neutral[3]
        } else if hov {
            c.semantic.danger
        } else {
            with_alpha(c.semantic.danger, 210)
        };
        Self::fill_rr(scene, t, s, 9.0, bg);
        let label = match self.action {
            Action::Flash => "Write to drive",
            Action::Install => "Install qOS",
        };
        let fg = if ready { Color::WHITE } else { c.neutral[6] };
        self.txt_center(scene, tr, &format!("⚠ {label}"), 14.0, fg, (s.x0 + s.x1) / 2.0, s.y0 + 11.0, TextWeight::Bold);
    }

    fn draw_progress(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, area: Rect) {
        let c = &theme.tokens.colors;
        let p = &self.progress;
        if p.phase == JobPhase::Idle {
            self.txt(scene, tr, "Idle — configure the write above, then confirm to begin.", 11.5, c.neutral[6], area.x0, area.y0 + 4.0, None, TextWeight::Regular);
            return;
        }
        // Phase label + state colour.
        let (state_c, headline) = match p.phase {
            JobPhase::Writing => (c.semantic.info, format!("Writing…  {:.0}%", p.write_frac * 100.0)),
            JobPhase::Verifying => (c.accent[6], format!("Verifying…  {:.0}%", p.verify_frac * 100.0)),
            JobPhase::Success => (c.semantic.success, "Complete — drive written and verified".into()),
            JobPhase::Failed => (c.semantic.danger, "Failed — see status line".into()),
            JobPhase::Idle => (c.neutral[6], String::new()),
        };
        self.txt(scene, tr, &headline, 13.0, state_c, area.x0, area.y0, None, TextWeight::Semibold);
        if p.rate_bps > 0 && p.phase.is_active() {
            self.txt_right(scene, tr, &fmt_rate(p.rate_bps), 11.5, c.text_dim, area.x1, area.y0, TextWeight::Regular);
        }

        // Write bar.
        let wb = Rect::new(area.x0, area.y0 + 24.0, area.x1, area.y0 + 24.0 + 10.0);
        Self::bar(scene, t, wb, p.write_frac, c.neutral[3], if p.phase == JobPhase::Failed { c.semantic.danger } else { c.semantic.info });
        self.txt(scene, tr, "Write", 10.5, c.text_dim, area.x0, area.y0 + 38.0, None, TextWeight::Regular);

        // Verify bar.
        let vb = Rect::new(area.x0, area.y0 + 54.0, area.x1, area.y0 + 54.0 + 10.0);
        Self::bar(scene, t, vb, p.verify_frac, c.neutral[3], c.semantic.success);
        self.txt(scene, tr, "Verify", 10.5, c.text_dim, area.x0, area.y0 + 68.0, None, TextWeight::Regular);
    }

    // ---- modal ---------------------------------------------------------
    fn draw_modal(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, ml: &ModalLayout) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let card = ml.card;
        Self::fill_rr(scene, t, card, 16.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 16.0, with_alpha(c.semantic.danger, 150), 1.4);

        let dev = self.target().map(|d| d.dev_path()).unwrap_or_else(|| "the drive".into());
        let model = self.target().map(|d| d.model.clone()).unwrap_or_default();
        self.txt(scene, tr, &format!("Erase {dev}?"), 20.0, c.semantic.danger, card.x0 + 22.0, card.y0 + 20.0, None, TextWeight::Bold);
        let body = format!(
            "ALL DATA on {model} ({dev}) will be destroyed and replaced with \"{}\". \
             This cannot be undone.",
            self.source.as_ref().map(|s| s.name.clone()).unwrap_or_default()
        );
        self.txt(scene, tr, &body, 13.0, c.text_dim, card.x0 + 22.0, card.y0 + 52.0, Some((card.width() - 44.0) as f32), TextWeight::Regular);

        // Echo the typed confirmation so the user sees the gate is armed.
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        let matches = self.confirm == want || self.confirm == format!("/dev/{want}");
        self.txt(scene, tr, &format!("Typed confirmation: \"{}\"", self.confirm), 12.5, if matches { c.semantic.success } else { c.semantic.danger }, card.x0 + 22.0, card.y1 - 96.0, None, TextWeight::Medium);
        self.txt(scene, tr, "(In this development build no bytes are written — the write is simulated.)", 11.0, c.neutral[6], card.x0 + 22.0, card.y1 - 76.0, Some((card.width() - 44.0) as f32), TextWeight::Regular);

        // Cancel.
        let cancel_hov = ml.cancel.contains(self.last_mouse);
        Self::fill_rr(scene, t, ml.cancel, 9.0, if cancel_hov { c.neutral[3] } else { c.neutral[2] });
        Self::stroke_rr(scene, t, ml.cancel, 9.0, c.border_standard, 1.0);
        self.txt_center(scene, tr, "Cancel", 14.0, c.text_primary, (ml.cancel.x0 + ml.cancel.x1) / 2.0, ml.cancel.y0 + 11.0, TextWeight::Medium);

        // Confirm — only fully lit when the typed gate matches.
        let confirm_hov = ml.confirm.contains(self.last_mouse);
        let armed = matches;
        let cc = c.semantic.danger;
        Self::fill_rr(scene, t, ml.confirm, 9.0, if !armed { c.neutral[3] } else if confirm_hov { cc } else { with_alpha(cc, 210) });
        let fg = if armed { Color::WHITE } else { c.neutral[6] };
        self.txt_center(scene, tr, "Erase & write", 14.0, fg, (ml.confirm.x0 + ml.confirm.x1) / 2.0, ml.confirm.y0 + 11.0, TextWeight::Bold);
    }

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
        self.txt(scene, tr, "qos-install.conf — same options the interactive flow uses; secrets are referenced, not embedded.", 11.5, c.text_dim, card.x0 + 20.0, card.y0 + 42.0, Some((card.width() - 40.0) as f32), TextWeight::Regular);
        // Monospace-ish dump.
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
        // Dismiss hint.
        self.txt_center(scene, tr, "click anywhere or press Esc to close", 11.0, c.neutral[6], cx, card.y1 - 26.0, TextWeight::Regular);
    }

    // ---- status line ---------------------------------------------------
    fn draw_statusline(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme) {
        if self.status.is_empty() {
            return;
        }
        let c = &theme.tokens.colors;
        let col = if self.progress.phase == JobPhase::Failed {
            c.semantic.danger
        } else if self.progress.phase == JobPhase::Success {
            c.semantic.success
        } else {
            c.semantic.info
        };
        self.txt(scene, tr, &self.status, 12.0, col, PAD, self.rect.y1 - 18.0, None, TextWeight::Medium);
    }

    // ---- state predicates ----------------------------------------------
    fn can_start(&self) -> bool {
        self.source.is_some()
            && self.target().is_some()
            && self.progress.phase == JobPhase::Idle
            && self
                .source
                .as_ref()
                .zip(self.target())
                .map(|(s, t)| t.fits(s.size))
                .unwrap_or(false)
    }

    // ---- interaction ---------------------------------------------------
    fn select_source(&mut self, state: &mut AutoCommit, src: Source) {
        // Push the previous source onto the recent list.
        if let Some(prev) = &self.source {
            if !self.recent.iter().any(|s| s.path == prev.path && !prev.path.is_empty()) && !prev.path.is_empty() {
                self.recent.insert(0, prev.clone());
                self.recent.truncate(6);
                ImagerDoc::set_recent(state, &self.recent);
            }
        }
        // If the source can't be installed, fall back to Flash.
        if !src.installable && self.action == Action::Install {
            self.action = Action::Flash;
            ImagerDoc::set_action(state, Action::Flash);
        }
        self.source = Some(src.clone());
        ImagerDoc::set_source(state, Some(&src));
        self.status = format!("Source: {}", src.name);
        ImagerDoc::set_status(state, &self.status);
        self.dirty = true;
    }

    fn select_target(&mut self, state: &mut AutoCommit, idx: usize) {
        if idx < self.targets.len() {
            self.selected = Some(idx);
            ImagerDoc::set_selected(state, Some(idx));
            // Clear a stale confirmation when the target changes.
            self.confirm.clear();
            ImagerDoc::set_confirm(state, "");
            self.dirty = true;
        }
    }

    fn set_action(&mut self, state: &mut AutoCommit, a: Action) {
        if a == Action::Install && !self.source.as_ref().map(|s| s.installable).unwrap_or(false) {
            self.status = "Pick a qOS image to install (ISO files can only be written).".into();
            ImagerDoc::set_status(state, &self.status);
            self.dirty = true;
            return;
        }
        self.action = a;
        ImagerDoc::set_action(state, a);
        self.dirty = true;
    }

    fn open_confirm(&mut self, state: &mut AutoCommit) {
        if !self.can_start() {
            self.status = "Select a source and a target that fits before writing.".into();
            ImagerDoc::set_status(state, &self.status);
            self.dirty = true;
            return;
        }
        self.modal_open = true;
        self.focus = Some(Field::Confirm);
        self.dirty = true;
    }

    fn confirm_write(&mut self, state: &mut AutoCommit) {
        let (Some(src), Some(tgt)) = (self.source.clone(), self.target().cloned()) else {
            return;
        };
        let req = WriteRequest {
            source: src,
            target: tgt.clone(),
            action: self.action,
            profile: self.profile.clone(),
        };
        match begin_job(&req, &self.confirm) {
            JobStart::Started => {
                // Drive the simulated job: jump straight to a completed
                // verify so the (headless, non-ticking) screenshot shows a
                // real progress UI. A live event loop would advance these
                // over time from the backend's byte offsets.
                self.progress = Progress {
                    phase: JobPhase::Success,
                    write_frac: 1.0,
                    verify_frac: 1.0,
                    rate_bps: 142_000_000,
                };
                ImagerDoc::set_progress(state, &self.progress);
                self.status = format!(
                    "{} {} → {} complete (simulated — no bytes written in this build)",
                    self.action.title(),
                    self.source.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
                    tgt.dev_path()
                );
            }
            JobStart::ConfirmationMismatch => {
                self.status = "Confirmation does not match the target device — write refused.".into();
            }
            JobStart::TooSmall => {
                self.status = "Image is larger than the target drive — write refused.".into();
            }
            JobStart::Incomplete => {
                self.status = "Pick a source and target before writing.".into();
            }
        }
        ImagerDoc::set_status(state, &self.status);
        self.modal_open = false;
        self.dirty = true;
    }

    fn cancel_confirm(&mut self, _state: &mut AutoCommit) {
        self.modal_open = false;
        self.focus = None;
        self.dirty = true;
    }

    fn export_config(&mut self, _state: &mut AutoCommit) {
        let text = self.profile.to_headless_config(self.source.as_ref(), self.target());
        self.export_text = Some(text);
        self.dirty = true;
    }

    fn load_profile(&mut self, state: &mut AutoCommit, idx: usize) {
        if let Some((name, prof)) = self.profiles.get(idx).cloned() {
            self.profile = prof.clone();
            ImagerDoc::set_profile(state, &prof);
            self.status = format!("Loaded profile \"{name}\"");
            ImagerDoc::set_status(state, &self.status);
            self.dirty = true;
        }
    }

    fn toggle_encrypt(&mut self, state: &mut AutoCommit) {
        self.profile.encrypt = !self.profile.encrypt;
        ImagerDoc::set_profile(state, &self.profile);
        self.dirty = true;
    }

    fn toggle_autologin(&mut self, state: &mut AutoCommit) {
        self.profile.user.autologin = !self.profile.user.autologin;
        ImagerDoc::set_profile(state, &self.profile);
        self.dirty = true;
    }

    fn set_mode(&mut self, state: &mut AutoCommit, m: InstallMode) {
        self.profile.mode = m;
        ImagerDoc::set_profile(state, &self.profile);
        self.dirty = true;
    }

    fn set_layout(&mut self, state: &mut AutoCommit, l: DiskLayout) {
        self.profile.layout = l;
        ImagerDoc::set_profile(state, &self.profile);
        self.dirty = true;
    }

    /// Append/backspace a character into the focused text field.
    fn type_into_field(&mut self, state: &mut AutoCommit, f: Field, ch: Option<char>, backspace: bool) {
        let target: &mut String = match f {
            Field::Hostname => &mut self.profile.hostname,
            Field::FullName => &mut self.profile.user.full_name,
            Field::Username => &mut self.profile.user.username,
            Field::Password => &mut self.profile.user.password,
            Field::Locale => &mut self.profile.locale,
            Field::Keyboard => &mut self.profile.keyboard,
            Field::Timezone => &mut self.profile.timezone,
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

    fn do_refresh(&mut self, state: &mut AutoCommit) {
        let targets = if std::env::var("IMAGER_LIVE").is_ok() {
            crate::probe::probe().unwrap_or_else(|| self.targets.clone())
        } else {
            self.status = "Rescanned drives".into();
            ImagerDoc::set_status(state, &self.status);
            self.dirty = true;
            return;
        };
        ImagerDoc::write_targets(state, &targets);
        self.targets = targets;
        self.status = format!("Rescanned — {} drive(s)", self.targets.len());
        ImagerDoc::set_status(state, &self.status);
        self.dirty = true;
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

        self.targets = ImagerDoc::targets(state);
        self.source = ImagerDoc::source(state);
        self.recent = ImagerDoc::recent(state);
        self.profile = ImagerDoc::profile(state);
        self.profiles = ImagerDoc::profiles(state);
        self.progress = ImagerDoc::progress(state);
        self.action = ImagerDoc::action(state);
        self.confirm = ImagerDoc::confirm(state);
        self.status = ImagerDoc::status(state);
        let sel = ImagerDoc::selected(state).filter(|i| *i < self.targets.len());
        self.selected = sel;
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
                    width: percent(1.0),
                    height: percent(1.0),
                },
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
        self.draw_topbar(scene, tr, theme, transform);
        self.draw_source(scene, tr, theme, transform, &lay);
        self.draw_target(scene, tr, theme, transform, &lay);
        self.draw_options(scene, tr, theme, transform, &lay);
        self.draw_statusline(scene, tr, theme);

        if let Some(text) = &self.export_text {
            self.draw_export(scene, tr, theme, transform, text);
        } else if let Some(ml) = lay.modal {
            self.draw_modal(scene, tr, theme, transform, &ml);
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
                let prev = self.hover;
                self.hover = lay.targets.iter().position(|r| r.contains(p));
                if prev != self.hover {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                // Export overlay swallows clicks (click anywhere closes it).
                if self.export_text.is_some() {
                    self.export_text = None;
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // Modal is blocking.
                if let Some(ml) = lay.modal {
                    if ml.confirm.contains(p) {
                        self.confirm_write(state);
                    } else if ml.cancel.contains(p) {
                        self.cancel_confirm(state);
                    }
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // Dispatch by hit-target.
                if let Some((_, hit)) = lay.hits.iter().find(|(r, _)| r.contains(p)) {
                    self.dispatch_hit(state, hit.clone());
                    *handled = true;
                    self.dirty = true;
                    return;
                }
                // Click on empty space clears field focus.
                self.focus = None;
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                // Export overlay: Esc closes.
                if self.export_text.is_some() {
                    if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                        self.export_text = None;
                        *handled = true;
                        self.dirty = true;
                    }
                    return;
                }
                // Modal key handling.
                if self.modal_open {
                    match logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.cancel_confirm(state);
                            *handled = true;
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.confirm_write(state);
                            *handled = true;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.type_into_field(state, Field::Confirm, None, true);
                            *handled = true;
                        }
                        Key::Character(s) => {
                            if let Some(ch) = s.chars().next() {
                                self.type_into_field(state, Field::Confirm, Some(ch), false);
                                *handled = true;
                            }
                        }
                        _ => {}
                    }
                    self.dirty = true;
                    return;
                }
                // Field typing.
                if let Some(f) = self.focus {
                    match logical_key {
                        Key::Named(NamedKey::Backspace) => {
                            self.type_into_field(state, f, None, true);
                            *handled = true;
                        }
                        Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                            self.focus = None;
                            *handled = true;
                            self.dirty = true;
                        }
                        Key::Character(s) => {
                            if let Some(ch) = s.chars().next() {
                                self.type_into_field(state, f, Some(ch), false);
                                *handled = true;
                            }
                        }
                        Key::Named(NamedKey::Space) => {
                            self.type_into_field(state, f, Some(' '), false);
                            *handled = true;
                        }
                        _ => {}
                    }
                    return;
                }
                // Global shortcuts.
                match logical_key {
                    Key::Character(s) => match s.as_str() {
                        "r" | "R" => {
                            self.do_refresh(state);
                            *handled = true;
                        }
                        _ => {}
                    },
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

impl ImagerApp {
    fn dispatch_hit(&mut self, state: &mut AutoCommit, hit: Hit) {
        match hit {
            Hit::PickIso => {
                // A real build opens the file picker; the demo selects a
                // recent ISO if one exists, else reports the picker would open.
                if let Some(iso) = self.recent.iter().find(|s| s.kind == SourceKind::IsoFile).cloned() {
                    self.select_source(state, iso);
                } else {
                    self.status = "File picker would open here (demo).".into();
                    ImagerDoc::set_status(state, &self.status);
                    self.dirty = true;
                }
            }
            Hit::PickQos => {
                let src = crate::model::sample_releases()[0].to_source();
                self.select_source(state, src);
            }
            Hit::Recent(i) => {
                if let Some(src) = self.recent.get(i).cloned() {
                    self.select_source(state, src);
                }
            }
            Hit::Release(i) => {
                if let Some(rel) = crate::model::sample_releases().get(i) {
                    self.select_source(state, rel.to_source());
                }
            }
            Hit::Target(i) => self.select_target(state, i),
            Hit::Action(a) => self.set_action(state, a),
            Hit::Mode(m) => self.set_mode(state, m),
            Hit::Layout(l) => self.set_layout(state, l),
            Hit::ToggleEncrypt => self.toggle_encrypt(state),
            Hit::ToggleAutologin => self.toggle_autologin(state),
            Hit::LoadProfile(i) => self.load_profile(state, i),
            Hit::ExportConfig => self.export_config(state),
            Hit::StartJob => self.open_confirm(state),
            Hit::Field(f) => {
                self.focus = Some(f);
                self.dirty = true;
            }
            Hit::ModalConfirm => self.confirm_write(state),
            Hit::ModalCancel => self.cancel_confirm(state),
        }
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
    fn sync_seeds_sample_state() {
        let (app, _doc) = seeded_app();
        assert_eq!(app.targets.len(), 2);
        assert!(app.source.is_some());
        assert_eq!(app.selected, Some(0));
        assert_eq!(app.action, Action::Flash);
        assert_eq!(app.recent.len(), 3);
    }

    #[test]
    fn layout_has_one_card_per_target_and_two_action_tabs() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let lay = app.compute_layout();
        assert_eq!(lay.targets.len(), 2);
        assert_eq!(lay.action_tabs.len(), 2);
        assert_eq!(lay.releases.len(), 2);
    }

    #[test]
    fn selecting_target_clears_confirmation() {
        let (mut app, mut doc) = seeded_app();
        app.confirm = "sdb".into();
        ImagerDoc::set_confirm(&mut doc, "sdb");
        app.select_target(&mut doc, 1);
        assert_eq!(app.selected, Some(1));
        assert_eq!(ImagerDoc::confirm(&doc), "");
    }

    #[test]
    fn start_gated_until_source_target_and_fit() {
        let (mut app, mut doc) = seeded_app();
        // Source is the qOS image (2.4 GB); USB stick (30 GB) fits.
        app.select_target(&mut doc, 0);
        assert!(app.can_start());
        // Open confirm does NOT write.
        app.open_confirm(&mut doc);
        assert!(app.modal_open);
        assert_eq!(app.progress.phase, JobPhase::Idle);
    }

    #[test]
    fn write_refused_without_matching_confirmation() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.open_confirm(&mut doc);
        app.confirm = "wrong".into();
        app.confirm_write(&mut doc);
        // No success: the gate refused.
        assert_ne!(app.progress.phase, JobPhase::Success);
        assert!(ImagerDoc::status(&doc).to_lowercase().contains("refused"));
    }

    #[test]
    fn write_completes_with_matching_confirmation() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.open_confirm(&mut doc);
        app.confirm = "sdb".into();
        app.confirm_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Success);
        assert!((app.progress.write_frac - 1.0).abs() < 1e-9);
        assert!(!app.modal_open);
    }

    #[test]
    fn install_action_requires_installable_source() {
        let (mut app, mut doc) = seeded_app();
        // Default source is the installable qOS image → Install allowed.
        app.set_action(&mut doc, Action::Install);
        assert_eq!(app.action, Action::Install);
        // Switch to a non-installable ISO from recents.
        let iso = app.recent.iter().find(|s| s.kind == SourceKind::IsoFile).cloned().unwrap();
        app.select_source(&mut doc, iso);
        // Selecting a non-installable source falls back to Flash.
        assert_eq!(app.action, Action::Flash);
        // And re-requesting Install is refused.
        app.set_action(&mut doc, Action::Install);
        assert_eq!(app.action, Action::Flash);
    }

    #[test]
    fn export_produces_headless_config() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 1);
        app.export_config(&mut doc);
        let text = app.export_text.clone().unwrap();
        assert!(text.contains("[install]"));
        assert!(text.contains("[user]"));
        assert!(text.contains("$QOS_USER_PASSWORD"));
    }

    #[test]
    fn typing_into_field_persists_to_profile() {
        let (mut app, mut doc) = seeded_app();
        app.focus = Some(Field::Hostname);
        // Replace hostname char by char.
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
    fn toggle_encrypt_persists() {
        let (mut app, mut doc) = seeded_app();
        let before = app.profile.encrypt;
        app.toggle_encrypt(&mut doc);
        assert_ne!(app.profile.encrypt, before);
        assert_eq!(ImagerDoc::profile(&doc).encrypt, !before);
    }
}
