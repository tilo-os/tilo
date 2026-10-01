//! Root widget for the Disk Imager (imager) app.
//!
//! `ImagerApp` is a single self-drawing "canvas" widget that renders a guided
//! **wizard** with a six-station step indicator —
//! Source → Drive → Customize → Confirm → Write → Done — and a Back /
//! primary-action footer for creating a verified USB/SD installation medium.
//!
//! * **Source** — pick a local `.iso` / `.img` through the host XDG portal
//!   (never a bespoke picker, per HIG §5), or a known Tilo release.
//! * **Drive** — choose external media from the *real* block devices (`lsblk`);
//!   internal and system disks are never eligible. Installation onto those is
//!   handled after boot by the live Tilo installer.
//! * **Customize** (Tilo only) — the shared guided-customization step, delegated
//!   in full to [`CustomizePanel`](crate::customize); a plain ISO skips it.
//! * **Confirm** — a read-only review of every choice, an erase warning, and
//!   the typed device-name gate.
//! * **Write** — a real, byte-for-byte streaming copy with live progress and a
//!   read-back **verify** pass.
//! * **Done** — the verified / failed outcome with a fresh-start affordance.
//!
//! Every write is doubly gated: the typed confirmation at Confirm, then native
//! host authentication for a short-lived privileged writer. The wizard itself
//! stays unprivileged; tests and shots cannot start the helper.
//!
//! Reviewed sources and non-secret install preferences are collaborative
//! Automerge resources. Drive selection, typed confirmation, progress and
//! worker channels are local capabilities: another peer can inspect a plan,
//! but never select/arm/write this machine's device.

use std::collections::HashMap;
use std::fmt::Write as _;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::WindowEvent;

use ui::paint::{PaintCtx, with_alpha};
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_metrics::{centered_text_top, line_height_px, renderer_type_size};
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::Metrics;
use ui::tokens::colors::{NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST};
use ui::widget::{Badge, Button, ButtonFace, TextInput, Widget};

use crate::customize::{CustomizePanel, CzField};
use crate::doc::ImagerDoc;
use crate::export_dialog::ExportDialog;
use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceKind, Step, TargetDrive, TargetKind,
    TiloRelease, fmt_eta, fmt_rate, fmt_size,
};
use crate::write::{JobControl, WriteRequest, spawn_job};
use crate::write_setup::WriteSetupPanel;

// ---- layout constants (logical px) -------------------------------------
// The spacing rhythm (HIG §7b) is the crate's: `crate::rhythm` names every
// step once, and this immediate-mode layout — a pure function of the
// viewport that receives no `Theme` — imports the ones it uses.
use crate::rhythm::{GROUP, HERO, LABEL, REGION, RELATED, SECTION, SIBLING};

/// Page gutter; below [`COMPACT_GUTTER_BELOW`] the compact gutter applies.
const OUTER: f64 = GROUP;
const OUTER_COMPACT: f64 = SECTION;
const COMPACT_GUTTER_BELOW: f64 = 600.0;
// The compositor draws the window chrome + app name (OS owns chrome), so the
// app itself opens straight into the step indicator — no in-app title bar.
const CRUMB_TOP: f64 = GROUP;
/// A step station is a pointer target.
const CRUMB_H: f64 = Metrics::BASE.min_pointer_target as f64;
/// Wide step stations: the labelled width, and the gap carrying the connector.
const CRUMB_W: f64 = 132.0;
const CRUMB_GAP: f64 = SECTION;
/// Footer actions and the confirmation field are panel rows.
const CONTROL_H: f64 = Metrics::BASE.panel_row_height as f64;
/// Small secondary buttons (Rescan, Preview): the minimum pointer target.
const SMALL_CONTROL_H: f64 = Metrics::BASE.min_pointer_target as f64;
const FOOTER_H: f64 = CONTROL_H + 2.0 * SECTION;
const CONTENT_MAX_W: f64 = 900.0;
/// Hand-drawn glyphs: the drop-zone arrow's box, the check discs, the hero.
const ICON: f64 = Metrics::BASE.icon_lg as f64;
const CHECK_R: f64 = ICON / 2.0;
const HERO_DISC_R: f64 = 2.0 * ICON;
/// Progress ring: radius and stroke.
const RING_R: f64 = 76.0;
const RING_W: f64 = 12.0;
const RING_OUTER: f64 = RING_R + RING_W / 2.0;
/// Phase bar thickness.
const BAR_H: f64 = RELATED;
/// The zero-state USB-stick glyph: cap and body.
const GLYPH_CAP_H: f64 = SECTION;
const GLYPH_BODY_H: f64 = 54.0;
const GLYPH_H: f64 = GLYPH_CAP_H + GLYPH_BODY_H;
/// Gap between a step title and its description line (title + explanation).
const STEP_SUB_GAP: f64 = RELATED;
/// The gap between a step's heading group and its first content block is the
/// theme's `spacing.s3`, cached from `build_layout` into `heading_gap` because
/// the renderer hook that measures the heading receives no `Theme`.
fn default_heading_gap() -> f64 {
    f64::from(ui::tokens::spacing::Spacing::default().s3)
}
/// Unmeasured floor for the heading group (a wrapped description needs the
/// taller compact value); the measured group is never shorter.
const STEP_HEADER_FLOOR: f64 = 44.0;
const STEP_HEADER_FLOOR_COMPACT: f64 = 64.0;

/// Copy of the Drive step's zero state, shared by the measurer and painter.
const EMPTY_TITLE: &str = "Insert a drive";
const EMPTY_LINE_1: &str = "Plug in a USB stick or SD card, then press Rescan.";
const EMPTY_LINE_2: &str = "Internal disks are available after booting this installation medium.";
const RESCAN_LABEL: &str = "⟳ Rescan";
const PLAIN_TITLE: &str = "This image is written unchanged";
const PLAIN_BODY: &str = "Pre-configuration (hostname, user, region) is available for Tilo images. Press Continue to write it as-is.";

/// The text measurements the layout model needs, so the unmeasured 1×
/// defaults and the renderer-measured bands share one set of formulas: a
/// band is always "its line boxes plus rhythm steps", never a hand-tuned
/// pixel count.
trait Measure {
    /// Line-box height of `size`-pt text.
    fn line(&mut self, size: f32) -> f64;
    /// Advance width of `s`.
    fn width(&mut self, s: &str, size: f32, weight: TextWeight) -> f64;
    /// Height of `s` wrapped to `max_w`.
    fn para(&mut self, s: &str, size: f32, weight: TextWeight, max_w: f64) -> f64;
}

impl Measure for TextRenderer {
    fn line(&mut self, size: f32) -> f64 {
        f64::from(self.measure("Hg", size, None).1)
    }
    fn width(&mut self, s: &str, size: f32, weight: TextWeight) -> f64 {
        f64::from(self.measure_weighted(s, size, None, weight).0)
    }
    fn para(&mut self, s: &str, size: f32, weight: TextWeight, max_w: f64) -> f64 {
        f64::from(
            self.measure_weighted(s, size, Some(max_w.max(1.0) as f32), weight)
                .1,
        )
    }
}

/// The layout before the renderer has measured anything: the same formulas
/// over a type-scale estimate, so a probe frame never places a control on a
/// zero-height band.
struct Estimate;

impl Measure for Estimate {
    /// The toolkit's line-height formula at 1× — what `TextRenderer::measure`
    /// reports for one line — so the unmeasured and measured bands agree.
    fn line(&mut self, size: f32) -> f64 {
        line_height_px(size, 1.0)
    }
    fn width(&mut self, s: &str, size: f32, _weight: TextWeight) -> f64 {
        s.chars().count() as f64 * f64::from(size) * 0.55
    }
    fn para(&mut self, s: &str, size: f32, weight: TextWeight, max_w: f64) -> f64 {
        let lines = (self.width(s, size, weight) / max_w.max(1.0))
            .ceil()
            .max(1.0);
        lines * self.line(size)
    }
}

/// Text-derived geometry of the Source step's blocks: the drop zone, the
/// selected-source card, the release rows, the recent rows and the section
/// captions. Measured from the renderer in `adjust_layout_with_measure` and
/// read by both the hit layout and the painters, so a row is clickable
/// exactly where it is drawn at every text scale.
#[derive(Clone, Debug, PartialEq)]
struct SourceMetrics {
    /// Text scale the bands were measured at; the compact breakpoint scales
    /// by it.
    text_scale: f64,
    /// Browse drop zone: band, and the tops of its title / hint lines
    /// (the stack is centred in the band).
    dropzone_h: f64,
    dz_title_top: f64,
    dz_sub_top: f64,
    /// Selected-source card, wide and compact: badge band, card band and the
    /// tops of the summary and tag lines. The compact card stacks the name
    /// beside the kind badge, so its lines start under the badge band.
    card_badge_h: f64,
    card_h: f64,
    card_summary_top: f64,
    card_tag_top: f64,
    card_h_compact: f64,
    card_summary_top_compact: f64,
    card_tag_top_compact: f64,
    /// Release row: band and the top of its channel/size line.
    release_row_h: f64,
    release_detail_top: f64,
    /// Recent row: band and the top of its size line.
    recent_row_h: f64,
    recent_size_top: f64,
    /// A section caption's band: its line box plus the gap to its list.
    section_h: f64,
    /// The "no releases" zero-state card.
    zero_h: f64,
}

impl Default for SourceMetrics {
    fn default() -> Self {
        Self::from_lines(1.0, &mut Estimate)
    }
}

impl SourceMetrics {
    fn from_lines(text_scale: f64, m: &mut dyn Measure) -> Self {
        let (h16, h15_5, h14_5, h14) = (m.line(16.0), m.line(15.5), m.line(14.5), m.line(14.0));
        let (h13, h12_5, h12, h11_5, h11) = (
            m.line(13.0),
            m.line(12.5),
            m.line(12.0),
            m.line(11.5),
            m.line(11.0),
        );
        // Drop zone: the title/hint stack sits `GROUP` inside the band.
        let dz_stack = h16 + RELATED + h12_5;
        // Cards and rows: `SECTION` / `RELATED` padding, `LABEL` between the
        // lines of one block.
        let card_badge_h = h12 + RELATED;
        let card_summary_top = SECTION + h15_5 + LABEL;
        let card_tag_top = card_summary_top + h12 + LABEL;
        let card_summary_top_compact = SECTION + h15_5.max(card_badge_h) + RELATED;
        let card_tag_top_compact = card_summary_top_compact + h12 + LABEL;
        let release_detail_top = RELATED + h14_5 + LABEL;
        let recent_size_top = RELATED + h13 + LABEL;
        Self {
            text_scale,
            dropzone_h: dz_stack + 2.0 * GROUP,
            dz_title_top: GROUP,
            dz_sub_top: GROUP + h16 + RELATED,
            card_badge_h,
            card_h: card_tag_top + h11_5 + SECTION,
            card_summary_top,
            card_tag_top,
            card_h_compact: card_tag_top_compact + h11_5 + SECTION,
            card_summary_top_compact,
            card_tag_top_compact,
            release_row_h: release_detail_top + h11_5 + RELATED,
            release_detail_top,
            recent_row_h: recent_size_top + h11 + RELATED,
            recent_size_top,
            section_h: h11 + RELATED,
            zero_h: h14 + RELATED + h12 + 2.0 * GROUP,
        }
    }

    fn card_h(&self, compact: bool) -> f64 {
        if compact {
            self.card_h_compact
        } else {
            self.card_h
        }
    }
}

/// Text-derived geometry of the Drive step: the Rescan button, the drive
/// card's stacked lines and the zero-state card.
#[derive(Clone, Debug, PartialEq)]
struct DriveMetrics {
    /// Rescan: its label plus a `SECTION` inset each side.
    rescan_w: f64,
    /// Kind badge / removable chip band.
    badge_h: f64,
    /// Line tops inside a card: model, device · size, the erase note.
    model_top: f64,
    dev_top: f64,
    erase_top: f64,
    card_h: f64,
    /// Zero-state card: glyph, title and both lines, and the band.
    empty_glyph_top: f64,
    empty_title_top: f64,
    empty_line_top: f64,
    empty_line2_top: f64,
    empty_h: f64,
}

impl Default for DriveMetrics {
    fn default() -> Self {
        Self::from_lines(&mut Estimate, CONTENT_MAX_W)
    }
}

impl DriveMetrics {
    fn from_lines(m: &mut dyn Measure, content_w: f64) -> Self {
        let (h17, h16, h12_5, h12, h11_5) = (
            m.line(17.0),
            m.line(16.0),
            m.line(12.5),
            m.line(12.0),
            m.line(11.5),
        );
        // The badge band, shared by the kind badge and the removable chip.
        let badge_h = h12 + RELATED;
        let model_top = SECTION + badge_h + RELATED;
        let dev_top = model_top + h16 + LABEL;
        let erase_top = dev_top + h12_5 + LABEL;
        let para_w = (content_w - 2.0 * GROUP).max(1.0);
        let empty_glyph_top = REGION;
        let empty_title_top = empty_glyph_top + GLYPH_H + GROUP;
        let empty_line_top = empty_title_top + h17 + RELATED;
        let empty_line2_top =
            empty_line_top + m.para(EMPTY_LINE_1, 13.0, TextWeight::Regular, para_w) + RELATED;
        Self {
            rescan_w: m.width(RESCAN_LABEL, 12.5, TextWeight::Medium) + 2.0 * SECTION,
            badge_h,
            model_top,
            dev_top,
            erase_top,
            card_h: erase_top + h11_5 + SECTION,
            empty_glyph_top,
            empty_title_top,
            empty_line_top,
            empty_line2_top,
            empty_h: empty_line2_top
                + m.para(EMPTY_LINE_2, 12.0, TextWeight::Regular, para_w)
                + REGION,
        }
    }
}

/// One key/value row of the Confirm review card.
struct ConfirmRow {
    key: &'static str,
    value: String,
    size: f32,
    weight: TextWeight,
    tone: RowTone,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowTone {
    Primary,
    Success,
    Danger,
}

/// Text-derived geometry of the Confirm step: the review card's rows, the
/// danger banner and the caption / note around the confirmation field.
#[derive(Clone, Debug, PartialEq, Default)]
struct ConfirmMetrics {
    /// Value column offset from the card's left edge: the widest key plus a
    /// `GROUP` gap.
    value_x: f64,
    /// Row tops (offsets from the card top) and the card band.
    row_top: Vec<f64>,
    card_h: f64,
    banner_h: f64,
    label_h: f64,
    note_h: f64,
}

/// Text-derived geometry of the Write step below its title.
#[derive(Clone, Debug, PartialEq)]
struct WriteMetrics {
    /// The ring's percentage readout.
    pct_h: f64,
    /// The rate / ETA line.
    meta_h: f64,
    /// The 12 pt lines: the ring caption, each phase label, the foot line.
    small_h: f64,
}

impl Default for WriteMetrics {
    fn default() -> Self {
        Self::from_lines(&mut Estimate)
    }
}

impl WriteMetrics {
    fn from_lines(m: &mut dyn Measure) -> Self {
        Self {
            pct_h: m.line(30.0),
            meta_h: m.line(13.0),
            small_h: m.line(12.0),
        }
    }

    /// Ring centre offset from the content top for a heading band `head`.
    fn ring_cy(&self, head: f64) -> f64 {
        head + REGION + RING_OUTER
    }

    /// Top of the rate / ETA line.
    fn meta_top(&self, head: f64) -> f64 {
        self.ring_cy(head) + RING_OUTER + SIBLING
    }

    /// Top of the first phase label.
    fn bars_top(&self, head: f64) -> f64 {
        self.meta_top(head) + self.meta_h + GROUP
    }

    /// Pitch of one labelled phase bar.
    fn bar_pitch(&self) -> f64 {
        self.small_h + LABEL + BAR_H + SIBLING
    }

    /// Top of the "image → drive" caption after both bars.
    fn foot_top(&self, head: f64) -> f64 {
        self.bars_top(head) + 2.0 * self.bar_pitch() - SIBLING + GROUP
    }

    fn extent(&self, head: f64) -> f64 {
        self.foot_top(head) + self.small_h + SECTION
    }
}

/// Text-derived geometry of the Done step: hero, copy and the summary card.
#[derive(Clone, Debug, PartialEq, Default)]
struct DoneMetrics {
    headline_h: f64,
    /// Height of the sub-copy block (one line, or the wrapped failure detail).
    sub_h: f64,
    /// Value column offset from the card's left edge.
    value_x: f64,
    row_h: f64,
    /// Card top offset from the content top, and its band.
    card_top: f64,
    card_h: f64,
}

impl DoneMetrics {
    fn disc_cy(content_y0: f64) -> f64 {
        content_y0 + HERO + HERO_DISC_R
    }

    fn headline_top(content_y0: f64) -> f64 {
        Self::disc_cy(content_y0) + HERO_DISC_R + GROUP
    }

    fn sub_top(&self, content_y0: f64) -> f64 {
        Self::headline_top(content_y0) + self.headline_h + RELATED
    }

    fn extent(&self) -> f64 {
        self.card_top + self.card_h + SECTION
    }
}

/// Every text-derived band of the wizard, measured together: one comparison
/// decides whether the layout re-solves and one assignment stores the
/// result, so a band added later cannot be left out of either.
#[derive(Clone, Debug, PartialEq, Default)]
struct Bands {
    source: SourceMetrics,
    drive: DriveMetrics,
    confirm: ConfirmMetrics,
    write: WriteMetrics,
    done: DoneMetrics,
    /// The "written unchanged" card of a plain-ISO Customize step.
    plain_card_h: f64,
}

/// A clickable hit-target for one frame. Pure function of the viewport +
/// state, so `draw` and `handle_event` agree without ordering hazards. The
/// Customize step's own affordances live inside [`CustomizePanel`], not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    PickFile,
    Release(usize),
    CancelDownload,
    Recent(usize),
    Target(usize),
    Rescan,
    ConfirmField,
    StepJump(Step),
    Back,
    Next,
    StartWrite,
    CancelWrite,
    WriteAnother,
}

struct Layout {
    crumbs: Vec<(Rect, Step)>,
    /// At compact widths the six-step rail shows numbered/checkmarked discs
    /// without labels. The page heading still names the current station, and
    /// this keeps every station visible instead of clipping the first/last
    /// controls outside the window.
    compact_crumbs: bool,
    back: Option<Rect>,
    primary: Option<Rect>,
    hits: Vec<(Rect, Hit)>,
    viewport_content: Rect,
    content: Rect,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (internal-disk visibility, the system-disk destructive-write ack, and seed/repaint bookkeeping), so grouping them would need a real design pass rather than a mechanical lint fix"
)]
pub struct ImagerApp {
    // ---- collaborative records (cached each `sync`) ----
    source: Option<Source>,
    recent: Vec<Source>,
    profile: InstallProfile,
    /// Host-session Wi-Fi passphrase. Never enters the collaborative document.
    wifi_psk: String,
    // ---- local machine/window state ----
    targets: Vec<TargetDrive>,
    /// Tilo releases offered on the Source step are a bounded local catalogue;
    /// they are never collaborative state. An offered release is a
    /// write source for a job that erases the target, so one that does not
    /// exist on this machine must not be clickable.
    releases: Vec<TiloRelease>,
    selected: Option<usize>,
    action: Action,
    progress: Progress,
    /// Peer-local destructive-write arming field. This must never replicate:
    /// a collaborator may review the plan, but cannot type consent for us.
    confirm_input: TextInput,
    status: String,
    step: Step,
    // ---- transient job / portal plumbing (not in the doc) ----
    job_rx: Option<std::sync::mpsc::Receiver<Progress>>,
    job_control: Option<JobControl>,
    portal_rx: Option<PortalChannel>,
    release_rx: Option<std::sync::mpsc::Receiver<crate::release::DownloadEvent>>,
    release_control: Option<crate::release::DownloadControl>,
    release_download_index: Option<usize>,
    release_download_progress: Option<(u64, u64)>,
    release_download_stage: Option<&'static str>,
    export_dialog: ExportDialog,
    /// Stages the read-only export modal after the demo document has synced.
    /// Used only by the deterministic `export` screenshot scenario.
    export_after_sync: bool,
    /// Makes the destructive action look armed in an offscreen documentation
    /// frame without enabling (or bypassing) the real write backend.
    shot_confirmation_ready: bool,
    /// False until the first live sync has reconciled any persisted active
    /// phase with this process's necessarily empty worker channels.
    startup_write_state_reconciled: bool,
    // ---- render caches ----
    seeded: bool,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    /// Measured height of the current step's heading group (title + wrapped
    /// description + the gap before content), refreshed from the renderer in
    /// `adjust_layout_with_measure`. `None` until the first measurement or on
    /// steps that place their own header; see [`Self::step_header_h`].
    step_heading_h: Option<f64>,
    /// `theme.tokens.spacing.s3`, refreshed in `build_layout` for the
    /// renderer hook (which receives no `Theme`).
    heading_gap: f64,
    /// Every step's text-derived bands, refreshed with the heading; see
    /// [`Bands`].
    bands: Bands,
    /// Measured line box of a step title (the accessible heading's bounds).
    title_h: f64,
    hover_target: Option<usize>,
    /// Vertical offset for the step body. The step rail and footer stay put.
    scroll_y: f64,
    /// Keyboard focus for canvas controls. Profile fields retain their focus
    /// in `CustomizePanel`, using the same Tab sequence.
    keyboard_hit: Option<Hit>,
    /// The shared guided-customization panel — the one component the write and
    /// install flows both use to collect the install profile. It owns its own
    /// fields (real [`ui::widget::TextInput`]s, the password masked + a11y
    /// protected) and dispatches each edit through [`Self::actions`]. See
    /// [`crate::customize`].
    customize: CustomizePanel,
    /// Wi-Fi and identity applied onto the written root by this machine.
    write_setup: WriteSetupPanel,
    /// Framework-owned action dispatch (the only action-dispatch state the app
    /// holds). Every guided-customization profile edit dispatches its
    /// [`ImagerAction`](crate::actions::ImagerAction) through this host, which
    /// brackets the undo boundary and applies it through the executor so it
    /// records one Ctrl+Z step and replays byte-identically on peers.
    actions: crate::resource::ActionSink,
}

impl Default for ImagerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ImagerApp {
    pub fn new() -> Self {
        let mut app = Self {
            source: None,
            recent: Vec::new(),
            targets: Vec::new(),
            releases: Vec::new(),
            selected: None,
            action: Action::Flash,
            profile: InstallProfile::default(),
            wifi_psk: String::new(),
            progress: Progress::default(),
            confirm_input: TextInput::local()
                .with_id("imager_device_confirmation")
                .with_font_size(14.0)
                .with_plain_chrome()
                .with_enter_submits(true),
            status: String::new(),
            step: Step::Source,
            job_rx: None,
            job_control: None,
            portal_rx: None,
            release_rx: None,
            release_control: None,
            release_download_index: None,
            release_download_progress: None,
            release_download_stage: None,
            export_dialog: ExportDialog::new(),
            export_after_sync: false,
            shot_confirmation_ready: false,
            startup_write_state_reconciled: false,
            seeded: false,
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            step_heading_h: None,
            heading_gap: default_heading_gap(),
            bands: Bands::default(),
            title_h: Estimate.line(19.0),
            hover_target: None,
            scroll_y: 0.0,
            keyboard_hit: None,
            customize: CustomizePanel::new(),
            write_setup: WriteSetupPanel::new(),
            actions: crate::resource::ActionSink::default(),
        };
        app.estimate_bands();
        app
    }

    /// Every band at its unmeasured 1× estimate, so a layout taken before
    /// the renderer's first measurement still places every block — the
    /// state-dependent Confirm rows and Done copy, the plain-ISO card — on a
    /// real band rather than at zero height.
    fn estimate_bands(&mut self) {
        self.bands = self.measure_bands(&mut Estimate, CONTENT_MAX_W, 1.0);
    }

    /// Every band at content width `content_w`, from `m` at `text_scale`:
    /// the same formulas over the renderer (`adjust_layout_with_measure`)
    /// and over the 1× estimate (`estimate_bands`).
    fn measure_bands(&self, m: &mut dyn Measure, content_w: f64, text_scale: f64) -> Bands {
        let plain_card_h = SECTION
            + m.line(16.0)
            + RELATED
            + m.para(
                PLAIN_BODY,
                13.0,
                TextWeight::Regular,
                (content_w - 2.0 * SECTION).max(1.0),
            )
            + SECTION;
        Bands {
            source: SourceMetrics::from_lines(text_scale, m),
            drive: DriveMetrics::from_lines(m, content_w),
            confirm: self.measure_confirm(m, content_w),
            write: WriteMetrics::from_lines(m),
            done: self.measure_done(m, content_w, crate::compact_content(content_w, text_scale)),
            plain_card_h,
        }
    }

    pub(crate) fn target(&self) -> Option<&TargetDrive> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    /// The releases the document actually knows about — empty on the live
    /// path, seeded content under `--demo` / `--shots`.
    fn releases(&self) -> &[TiloRelease] {
        &self.releases
    }

    /// Only external installation media is shown. Internal installation and
    /// dual boot belong to the installer on the booted Tilo medium.
    fn visible_targets(&self) -> Vec<(usize, &TargetDrive)> {
        self.targets
            .iter()
            .enumerate()
            .filter(|(_, drive)| drive.is_media_target())
            .collect()
    }

    fn removable_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|drive| drive.is_media_target())
            .count()
    }

    /// Can the flow move forward from `from`? (Also gates the step-indicator.)
    fn can_advance(&self, from: Step) -> bool {
        self.can_advance_with_writer(from, crate::write::write_available())
    }

    /// Pure half of the advance gate, split so the live-helper capability is
    /// covered without mutating process-wide environment variables in UI
    /// tests that run concurrently.
    fn can_advance_with_writer(&self, from: Step, writer_available: bool) -> bool {
        match from {
            Step::Source => self.source.is_some(),
            Step::Drive => self.selected.is_some(),
            Step::Customize => true,
            // Confirm advances into the (destructive) write only once armed.
            Step::Confirm => {
                self.startup_write_state_reconciled
                    && self.confirm_matches()
                    && self.image_fits()
                    && writer_available
            }
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

    /// Only a Tilo image authenticated by the bundled release catalog can be
    /// pre-configured; every arbitrary local image is written as-is.
    fn customizable(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| self.source_is_official(source))
    }

    /// Confirm can collect Wi-Fi and enrollment for any selected image. Official
    /// installable media still uses Customize for the secret-free embedded
    /// profile; on-write handoff is staged separately and applied after the
    /// verified write onto the installed root.
    fn offers_write_setup(&self) -> bool {
        self.source.is_some()
    }

    fn source_is_official(&self, source: &Source) -> bool {
        source.kind == SourceKind::TiloImage
            && source.installable
            && source.expected_sha256.as_deref().is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            && self.releases().iter().any(|release| {
                release.image_sha256 == source.expected_sha256.as_deref().unwrap_or_default()
                    && release.size == source.size
                    && format!("{} {}", release.name, release.version) == source.name
            })
    }

    /// Read-only projections used to build the searchable resource hierarchy.
    /// The target list and job state describe this machine, so publishing them
    /// does not turn them into shared authority.
    pub(crate) fn resource_source(&self) -> Option<&Source> {
        self.source.as_ref()
    }
    pub(crate) fn resource_targets(&self) -> &[TargetDrive] {
        &self.targets
    }
    pub(crate) fn resource_progress(&self) -> &Progress {
        &self.progress
    }

    /// The last profile [`EncodedAction`](tilo_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec` covenant
    /// test.
    #[doc(hidden)]
    pub fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Show the selected image and install profile the framework resource holds.
    /// An empty projection does not wipe a selection that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::ImagerState) {
        if state.source.is_none() && self.source.is_some() {
            return;
        }
        if state.source.is_some() {
            self.source = state.source.clone();
        }
        let profile_changed = state.profile.hostname != self.profile.hostname
            || state.profile.keyboard != self.profile.keyboard
            || state.profile.timezone != self.profile.timezone
            || state.profile.locale != self.profile.locale
            || state.profile.user.username != self.profile.user.username
            || state.profile.user.full_name != self.profile.user.full_name
            || state.profile.encrypt != self.profile.encrypt;
        if profile_changed {
            self.profile = state.profile.clone();
            self.customize.sync(&self.profile);
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Imager>,
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

    /// Whether startup has finished reconciling an in-flight write helper.
    pub(crate) fn startup_write_reconciled(&self) -> bool {
        self.startup_write_state_reconciled
    }

    /// Whether a write/verify worker channel is open on this surface.
    pub(crate) fn job_active(&self) -> bool {
        self.job_rx.is_some()
    }

    /// Stage the peer-local confirmation for a deterministic offscreen frame.
    #[doc(hidden)]
    pub fn seed_confirmation_for_shot(&mut self, value: &str) {
        self.confirm_input.set_text(value);
        self.shot_confirmation_ready = true;
    }

    /// Headless-shot only: expose the long, searchable time-zone chooser so
    /// its overlay placement is part of visual review.
    pub fn shot_open_customize_timezone(&mut self) {
        self.customize.open_timezone_for_shot();
    }

    /// Prime local-only state for deterministic offscreen review. None of this
    /// device/job presentation data is written into the collaborative doc.
    pub fn seed_local_shot_state(&mut self, scenario: &str) {
        self.targets = if scenario == "drive-empty" || scenario == "source-empty" {
            Vec::new()
        } else {
            crate::model::sample_targets()
        };
        self.releases = crate::model::sample_releases();
        match scenario {
            "drive" => {
                self.selected = Some(0);
                self.step = Step::Drive;
                self.status = "1 removable, 2 total".into();
            }
            "customize" | "customize-timezone-open" | "export" => {
                self.selected = Some(0);
                self.step = Step::Customize;
                self.status = "Pre-configuring Tilo 1.0".into();
            }
            "confirm" => {
                self.selected = Some(0);
                self.step = Step::Confirm;
                self.status = "Ready to write Tilo 1.0 → /dev/sdb".into();
            }
            "progress" => {
                self.selected = Some(0);
                self.step = Step::Write;
                self.progress = Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.62,
                    verify_frac: 0.0,
                    rate_bps: 176_000_000,
                    error: None,
                };
                self.status = "Writing Tilo 1.0 → /dev/sdb".into();
            }
            "done" => {
                self.selected = Some(0);
                self.step = Step::Done;
                self.progress = Progress {
                    phase: JobPhase::Success,
                    write_frac: 1.0,
                    verify_frac: 1.0,
                    rate_bps: 0,
                    error: None,
                };
                self.status = "Done — Tilo 1.0 written and verified".into();
            }
            "done-failed" => {
                self.selected = Some(0);
                self.step = Step::Done;
                self.progress = Progress {
                    phase: JobPhase::Failed,
                    write_frac: 0.36,
                    verify_frac: 0.0,
                    rate_bps: 0,
                    error: Some("Write failed — device disappeared mid-write".into()),
                };
                self.status = "Write failed — device disappeared mid-write".into();
            }
            "drive-empty" => {
                self.step = Step::Drive;
                self.status = "No removable drives detected".into();
            }
            "drive-internal" => {
                // The Drive step at rest: nothing selected yet, so the card
                // shows its rest edge rather than the selection ring.
                self.step = Step::Drive;
                self.status = "1 removable, 2 total".into();
            }
            _ => {}
        }
        // The unmeasured bands still describe this posed state (the review
        // rows, the failure copy) until the renderer measures them.
        self.estimate_bands();
    }

    /// Local demo inventory/catalogue used by `--demo`; the collaborative
    /// document still carries only the reviewed source/profile records.
    pub fn seed_local_demo_state(&mut self) {
        self.seed_local_shot_state("source");
    }

    fn confirm_matches(&self) -> bool {
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        let confirm = self.confirm_input.value();
        !confirm.is_empty() && (confirm == want || confirm == format!("/dev/{want}"))
    }

    fn image_fits(&self) -> bool {
        match (&self.source, self.target()) {
            (Some(s), Some(t)) => t.fits(s.size),
            _ => false,
        }
    }
}

// =========================================================================
// Accessibility semantics for the immediate-mode canvas
// =========================================================================
impl ImagerApp {
    fn accessible_step_copy(&self) -> (&'static str, String) {
        match self.step {
            Step::Source => (
                "Choose the source image",
                if self.releases().is_empty() {
                    "Pick a local ISO or IMG file to write.".into()
                } else {
                    "Pick a local ISO or IMG file, or choose a Tilo release.".into()
                },
            ),
            Step::Drive => (
                "Select the target drive",
                "Only external installation media is available here. The selected drive will be erased."
                    .into(),
            ),
            Step::Customize => (
                "Prepare your setup",
                if self.customizable() {
                    "These non-secret choices travel with the installer and shorten setup on the device."
                        .into()
                } else {
                    "This image will be written unchanged.".into()
                },
            ),
            Step::Confirm => (
                if self.action == Action::Install {
                    "Confirm and install"
                } else {
                    "Confirm and write"
                },
                "Review the destructive operation, then type the drive name to unlock it."
                    .into(),
            ),
            Step::Write => (
                if self.progress.phase == JobPhase::Verifying {
                    "Verifying the drive"
                } else {
                    "Writing the drive"
                },
                "Keep the drive connected until writing and verification finish.".into(),
            ),
            Step::Done => {
                let success = self.progress.phase == JobPhase::Success;
                (
                    if success {
                        "Drive is ready"
                    } else {
                        "Write failed"
                    },
                    if success {
                        "The image was written and verified. You can safely remove the drive."
                            .into()
                    } else {
                        "The drive may be incomplete. Retry before using it.".into()
                    },
                )
            }
        }
    }

    fn hit_target(hit: Hit) -> String {
        match hit {
            Hit::PickFile => "imager:hit:pick-file".into(),
            Hit::Release(index) => format!("imager:hit:release:{index}"),
            Hit::CancelDownload => "imager:hit:cancel-download".into(),
            Hit::Recent(index) => format!("imager:hit:recent:{index}"),
            Hit::Target(index) => format!("imager:hit:target:{index}"),
            Hit::Rescan => "imager:hit:rescan".into(),
            Hit::ConfirmField => "imager:hit:confirm-field".into(),
            Hit::StepJump(step) => format!("imager:hit:step:{}", step.key()),
            Hit::Back => "imager:hit:back".into(),
            Hit::Next => "imager:hit:next".into(),
            Hit::StartWrite => "imager:hit:start-write".into(),
            Hit::CancelWrite => "imager:hit:cancel-write".into(),
            Hit::WriteAnother => "imager:hit:write-another".into(),
        }
    }

    fn hit_from_target(target: &str) -> Option<Hit> {
        let suffix = target.strip_prefix("imager:hit:")?;
        match suffix {
            "pick-file" => Some(Hit::PickFile),
            "rescan" => Some(Hit::Rescan),
            "confirm-field" => Some(Hit::ConfirmField),
            "back" => Some(Hit::Back),
            "next" => Some(Hit::Next),
            "start-write" => Some(Hit::StartWrite),
            "cancel-write" => Some(Hit::CancelWrite),
            "cancel-download" => Some(Hit::CancelDownload),
            "write-another" => Some(Hit::WriteAnother),
            _ => {
                let (kind, value) = suffix.split_once(':')?;
                match kind {
                    "step" => match value {
                        "source" => Some(Hit::StepJump(Step::Source)),
                        "drive" => Some(Hit::StepJump(Step::Drive)),
                        "customize" => Some(Hit::StepJump(Step::Customize)),
                        "confirm" => Some(Hit::StepJump(Step::Confirm)),
                        "write" => Some(Hit::StepJump(Step::Write)),
                        "done" => Some(Hit::StepJump(Step::Done)),
                        _ => None,
                    },
                    _ => None,
                }
            }
        }
    }

    fn a11y_hit_enabled(&self, hit: Hit) -> bool {
        match hit {
            Hit::StepJump(step) => self.crumb_reachable(step),
            Hit::Next => match self.step {
                Step::Source | Step::Drive | Step::Customize => self.can_advance(self.step),
                Step::Confirm | Step::Write | Step::Done => false,
            },
            Hit::StartWrite => self.can_advance(Step::Confirm),
            Hit::CancelWrite => self
                .job_control
                .as_ref()
                .is_some_and(|control| !control.is_cancelled()),
            Hit::CancelDownload => self
                .release_control
                .as_ref()
                .is_some_and(|control| !control.is_cancelled()),
            Hit::Release(index) => self.release_rx.is_none() && self.releases.get(index).is_some(),
            Hit::Recent(index) => self.recent.get(index).is_some(),
            Hit::Target(index) => self
                .targets
                .get(index)
                .is_some_and(TargetDrive::is_media_target),
            Hit::PickFile | Hit::Rescan | Hit::ConfirmField | Hit::Back | Hit::WriteAnother => true,
        }
    }

    fn focus_accessible_profile_slot(&mut self, slot: usize) {
        let lay = self.compute_layout();
        self.keyboard_hit = None;
        self.confirm_input.set_focus(false);
        self.customize.focus_keyboard_slot(slot);
        if let Some(rect) = self.customize.keyboard_slot_rect(lay.content, slot) {
            self.ensure_visible(rect, lay.viewport_content);
        }
        self.dirty = true;
    }

    fn a11y_control(
        &self,
        role: ui::a11y::Role,
        name: impl Into<String>,
        rect: Rect,
        hit: Hit,
        disabled: bool,
    ) -> ui::a11y::AccessNode {
        ui::a11y::AccessNode::new(role)
            .with_name(name)
            .with_state(ui::a11y::AccessState {
                focused: self.keyboard_hit == Some(hit)
                    || (hit == Hit::ConfirmField && self.confirm_input.is_focused()),
                focusable: !disabled,
                disabled,
                ..Default::default()
            })
            .with_bounds(rect)
            .with_action_target(Self::hit_target(hit))
    }

    /// A semantic group whose bounds are the union of its children's, so
    /// assistive technology (and the geometry lint) can locate the group and
    /// walk into it instead of meeting a bound-less container.
    fn a11y_group(name: &str, children: Vec<ui::a11y::AccessNode>) -> ui::a11y::AccessNode {
        let bounds = children
            .iter()
            .filter_map(|child| child.bounds.map(ui::a11y::AccessBounds::to_rect))
            .reduce(|a, b| a.union(b));
        let node = ui::a11y::AccessNode::new(ui::a11y::Role::Group)
            .with_name(name)
            .with_children(children);
        match bounds {
            Some(rect) => node.with_bounds(rect),
            None => node,
        }
    }

    fn a11y_step_navigation(&self, lay: &Layout) -> ui::a11y::AccessNode {
        let children = lay
            .crumbs
            .iter()
            .map(|(rect, step)| {
                let reachable = self.crumb_reachable(*step);
                self.a11y_control(
                    ui::a11y::Role::Button,
                    format!("Step {} of 6: {}", step.index() + 1, step.title()),
                    *rect,
                    Hit::StepJump(*step),
                    !reachable,
                )
                .with_state(ui::a11y::AccessState {
                    focused: self.keyboard_hit == Some(Hit::StepJump(*step)),
                    focusable: reachable,
                    selected: *step == self.step,
                    disabled: !reachable,
                    ..Default::default()
                })
                .with_position(step.index() + 1, Step::all().len())
            })
            .collect();
        Self::a11y_group("Setup steps", children)
    }

    fn a11y_source_controls(&self, lay: &Layout) -> Vec<ui::a11y::AccessNode> {
        lay.hits
            .iter()
            .filter_map(|(rect, hit)| match *hit {
                Hit::PickFile => Some(self.a11y_control(
                    ui::a11y::Role::Button,
                    "Browse for an image",
                    *rect,
                    *hit,
                    false,
                )),
                Hit::Release(index) => self.releases.get(index).map(|release| {
                    self.a11y_control(
                        ui::a11y::Role::Button,
                        format!("Download {}", release.display_name()),
                        *rect,
                        *hit,
                        false,
                    )
                    .with_description(format!(
                        "{} · {} image",
                        release.target_label(),
                        fmt_size(release.size)
                    ))
                    .with_action_target(format!("imager:release:{}", release.selection_id()))
                }),
                Hit::CancelDownload => Some(
                    self.a11y_control(
                        ui::a11y::Role::Button,
                        "Cancel Tilo installer download",
                        *rect,
                        *hit,
                        self.release_control
                            .as_ref()
                            .is_none_or(crate::release::DownloadControl::is_cancelled),
                    )
                    .with_description("Keeps the verified partial download for a safe resume"),
                ),
                Hit::Recent(index) => self.recent.get(index).map(|source| {
                    self.a11y_control(
                        ui::a11y::Role::Button,
                        format!("Use recent image {}", source.name),
                        *rect,
                        *hit,
                        false,
                    )
                    .with_action_target(format!("imager:recent:{}", source.path))
                }),
                _ => None,
            })
            .collect()
    }

    fn a11y_drive_controls(&self, lay: &Layout) -> Vec<ui::a11y::AccessNode> {
        lay.hits
            .iter()
            .filter_map(|(rect, hit)| match *hit {
                Hit::Rescan => Some(self.a11y_control(
                    ui::a11y::Role::Button,
                    "Rescan drives",
                    *rect,
                    *hit,
                    false,
                )),
                Hit::Target(index) => self.targets.get(index).map(|drive| {
                    self.a11y_control(
                        ui::a11y::Role::RadioButton,
                        format!(
                            "{} on {}, {}",
                            drive.model,
                            drive.dev_path(),
                            fmt_size(drive.size)
                        ),
                        *rect,
                        *hit,
                        false,
                    )
                    .with_description(format!(
                        "External drive. Erases {}",
                        drive.contents_summary()
                    ))
                    .with_action_target(format!("imager:drive:{}", drive.name))
                    .with_state(ui::a11y::AccessState {
                        focused: self.keyboard_hit == Some(*hit),
                        focusable: true,
                        checked: Some(self.selected == Some(index)),
                        selected: self.selected == Some(index),
                        ..Default::default()
                    })
                }),
                _ => None,
            })
            .collect()
    }

    fn a11y_footer_controls(&self, lay: &Layout) -> Vec<ui::a11y::AccessNode> {
        let mut controls = Vec::new();
        if let Some(rect) = lay.back {
            controls.push(self.a11y_control(
                ui::a11y::Role::Button,
                "Back",
                rect,
                Hit::Back,
                false,
            ));
        }
        if let Some(rect) = lay.primary {
            let (hit, name, disabled) = match self.step {
                Step::Source => (
                    Hit::Next,
                    "Continue".into(),
                    !self.can_advance(Step::Source),
                ),
                Step::Drive => (Hit::Next, "Continue".into(), !self.can_advance(Step::Drive)),
                Step::Customize => (Hit::Next, "Continue".into(), false),
                Step::Confirm => (
                    Hit::StartWrite,
                    self.target().map_or_else(
                        || "Erase and write selected drive".into(),
                        |drive| format!("Erase and write {}", drive.dev_path()),
                    ),
                    !self.can_advance(Step::Confirm),
                ),
                Step::Done => (
                    Hit::WriteAnother,
                    if self.progress.phase == JobPhase::Failed {
                        "Review and retry".into()
                    } else {
                        "Start over".into()
                    },
                    false,
                ),
                Step::Write => (
                    Hit::CancelWrite,
                    "Cancel image write".into(),
                    self.job_control
                        .as_ref()
                        .is_none_or(JobControl::is_cancelled),
                ),
            };
            controls.push(self.a11y_control(ui::a11y::Role::Button, name, rect, hit, disabled));
        }
        controls
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the semantic tree mirrors the six mutually exclusive immediate-mode wizard screens in one auditable order"
    )]
    fn accessibility_tree(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        let lay = self.compute_layout();
        let (title, description) = self.accessible_step_copy();
        let status_lower = self.status.to_ascii_lowercase();
        let status_is_error = self.progress.phase == JobPhase::Failed
            || [
                "failed",
                "unavailable",
                "invalid",
                "refused",
                "must ",
                "larger",
            ]
            .iter()
            .any(|word| status_lower.contains(word));
        // The spoken description always has a band: the painted heading's
        // wrapped description where the step shows one, otherwise one line of
        // description type — never a zero-height rect (Write, a plain-ISO
        // Customize and Done paint no description but still describe the step).
        let description_line = line_height_px(13.0, self.bands.source.text_scale as f32);
        let mut children = vec![
            self.a11y_step_navigation(&lay)
                .with_hashed_key("step-navigation"),
            AccessNode::new(Role::Heading { level: 1 })
                .with_name(title)
                .with_hashed_key("step-heading")
                .with_bounds(Rect::new(
                    lay.content.x0,
                    lay.content.y0,
                    lay.content.x1,
                    lay.content.y0 + self.title_h,
                )),
            AccessNode::new(Role::Paragraph)
                .with_value(description)
                .with_hashed_key("step-description")
                .with_bounds(Rect::new(
                    lay.content.x0,
                    lay.content.y0 + self.title_h + STEP_SUB_GAP,
                    lay.content.x1,
                    lay.content.y0
                        + (self.step_header_h(lay.content) - self.heading_gap)
                            .max(self.title_h + STEP_SUB_GAP + description_line),
                )),
        ];
        if !self.status.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Status")
                    .with_value(self.status.clone())
                    .with_hashed_key("status")
                    .with_live(if status_is_error {
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    }),
            );
        }

        match self.step {
            Step::Source => {
                if let Some(source) = &self.source {
                    children.push(
                        AccessNode::new(Role::StaticText)
                            .with_name("Selected image")
                            .with_value(format!(
                                "{}, {}, {}",
                                source.name,
                                fmt_size(source.size),
                                if self.source_is_official(source) {
                                    "Official Tilo release — catalog verified"
                                } else {
                                    "Local image — unverified, written as-is"
                                }
                            ))
                            .with_hashed_key("selected-image"),
                    );
                }
                children.push(
                    Self::a11y_group("Image sources", self.a11y_source_controls(&lay))
                        .with_hashed_key("source-controls"),
                );
            }
            Step::Drive => children.push(
                Self::a11y_group(
                    "External destination drives",
                    self.a11y_drive_controls(&lay),
                )
                .with_hashed_key("drive-controls"),
            ),
            Step::Customize if self.customizable() => children.push(
                Self::a11y_group("Setup profile", self.customize.accessibility(lay.content))
                    .with_hashed_key("profile-controls"),
            ),
            Step::Customize => children.push(
                AccessNode::new(Role::StaticText)
                    .with_value("Pre-configuration is only available for Tilo images.")
                    .with_hashed_key("profile-unavailable"),
            ),
            Step::Confirm => {
                if let (Some(source), Some(target)) = (&self.source, self.target()) {
                    children.push(
                        AccessNode::new(Role::Group)
                            .with_name("Write review")
                            .with_hashed_key("write-review")
                            .with_children(vec![
                                AccessNode::new(Role::StaticText)
                                    .with_name("Image")
                                    .with_value(format!("{}, {}", source.name, fmt_size(source.size))),
                                AccessNode::new(Role::StaticText)
                                    .with_name("Drive")
                                    .with_value(format!("{} on {}, {}", target.model, target.dev_path(), fmt_size(target.size))),
                                AccessNode::new(Role::StaticText)
                                    .with_name("Warning")
                                    .with_value("This permanently erases the selected drive and cannot be undone."),
                            ]),
                    );
                }
                if self.offers_write_setup() {
                    children.push(
                        Self::a11y_group(
                            "Setup on write",
                            self.write_setup.accessibility(
                                lay.content,
                                self.write_setup_top(lay.content),
                                self.profile.enroll_local_identity,
                            ),
                        )
                        .with_hashed_key("write-setup"),
                    );
                }
                if let Some((rect, _)) = lay.hits.iter().find(|(_, hit)| *hit == Hit::ConfirmField)
                {
                    let expected = self
                        .target()
                        .map_or("the drive name", |drive| drive.name.as_str());
                    children.push(
                        AccessNode::new(Role::TextInput)
                            .with_name("Confirm the target device")
                            .with_description(format!(
                                "Type {expected} to unlock the destructive write"
                            ))
                            .with_value(self.confirm_input.value())
                            .with_state(AccessState {
                                focused: self.confirm_input.is_focused(),
                                focusable: true,
                                required: true,
                                invalid: !self.confirm_input.value().is_empty()
                                    && !self.confirm_matches(),
                                ..Default::default()
                            })
                            .with_bounds(*rect)
                            .with_action_target(Self::hit_target(Hit::ConfirmField)),
                    );
                }
            }
            Step::Write => children.push(
                AccessNode::new(Role::Group)
                    .with_name("Write progress")
                    .with_hashed_key("write-progress")
                    .with_children(vec![
                        AccessNode::new(Role::ProgressBar)
                            .with_name(if self.progress.phase == JobPhase::Verifying {
                                "Verifying image"
                            } else {
                                "Writing image"
                            })
                            .with_value(format!("{:.0} percent", self.progress.overall() * 100.0))
                            .with_state(AccessState::new().busy(true))
                            .with_hashed_key("overall")
                            .with_live(Politeness::Polite),
                        AccessNode::new(Role::ProgressBar)
                            .with_name("Write phase")
                            .with_value(format!("{:.0} percent", self.progress.write_frac * 100.0))
                            .with_hashed_key("write"),
                        AccessNode::new(Role::ProgressBar)
                            .with_name("Verify phase")
                            .with_value(format!("{:.0} percent", self.progress.verify_frac * 100.0))
                            .with_hashed_key("verify"),
                    ]),
            ),
            Step::Done => children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(if self.progress.phase == JobPhase::Success {
                        "Write completed and verified"
                    } else {
                        "Write failure details"
                    })
                    .with_value(
                        self.progress
                            .error
                            .clone()
                            .unwrap_or_else(|| self.status.clone()),
                    )
                    .with_hashed_key("write-result")
                    .with_live(if self.progress.phase == JobPhase::Success {
                        Politeness::Polite
                    } else {
                        Politeness::Assertive
                    }),
            ),
        }
        let footer = self.a11y_footer_controls(&lay);
        if !footer.is_empty() {
            children
                .push(Self::a11y_group("Wizard actions", footer).with_hashed_key("wizard-actions"));
        }
        AccessNode::new(Role::Window)
            .with_name("Start Media Creator")
            .with_children(children)
    }
}

/// Whether this process should enumerate real block devices. The live binary
/// sets `IMAGER_LIVE=1`; `--shots`, `--demo` and tests leave it unset so the
/// document's seeded/posed state is authoritative and no `lsblk` runs.
fn live_probe_enabled() -> bool {
    std::env::var("IMAGER_LIVE").is_ok()
}

// ---- theme-derived colour helpers --------------------------------------

fn source_color(theme: &Theme, kind: SourceKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        SourceKind::TiloImage => c.accent[5],
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

/// A run of wizard text: the string, the style it is shaped with and its ink.
pub(crate) struct TextRun<'a> {
    text: &'a str,
    style: TextStyle,
    color: Color,
}

impl<'a> TextRun<'a> {
    pub(crate) fn new(text: &'a str, style: TextStyle, color: Color) -> Self {
        Self { text, style, color }
    }

    /// The run with its line box's top-left at `at`, wrapped at `max_w`.
    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        at: Point,
        max_w: Option<f32>,
    ) {
        tr.draw_style(
            scene,
            self.text,
            self.color,
            Affine::translate(at.to_vec2()),
            max_w,
            &self.style,
        );
    }

    fn width(&self, tr: &mut TextRenderer) -> f64 {
        f64::from(tr.measure_style(self.text, None, &self.style).0)
    }

    fn draw_right(&self, scene: &mut Scene, tr: &mut TextRenderer, right: f64, top: f64) {
        let w = self.width(tr);
        self.draw(scene, tr, Point::new(right - w, top), None);
    }

    fn draw_center(&self, scene: &mut Scene, tr: &mut TextRenderer, cx: f64, top: f64) {
        let w = self.width(tr);
        self.draw(scene, tr, Point::new(cx - w / 2.0, top), None);
    }

    fn line_top(&self, tr: &mut TextRenderer, band_y0: f64, band_h: f64) -> f64 {
        ImagerApp::line_top(
            tr,
            self.text,
            self.style.size,
            self.style.weight,
            band_y0,
            band_h,
        )
    }

    /// Centred on `cx`, its line box centred in the band
    /// `band_y0 ..= band_y0 + band_h`.
    fn draw_center_v(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        cx: f64,
        band_y0: f64,
        band_h: f64,
    ) {
        let top = self.line_top(tr, band_y0, band_h);
        self.draw_center(scene, tr, cx, top);
    }

    /// Right-aligned at `right`, its line box centred in the band
    /// `band_y0 ..= band_y0 + band_h`.
    fn draw_right_v(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        right: f64,
        band_y0: f64,
        band_h: f64,
    ) {
        let top = self.line_top(tr, band_y0, band_h);
        self.draw_right(scene, tr, right, top);
    }

    /// A paragraph centred on `cx` while it fits `max_w`, wrapped as a
    /// left-aligned block of that width once it does not.
    fn draw_para_centered(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        cx: f64,
        top: f64,
        max_w: f64,
    ) {
        if self.width(tr) <= max_w {
            self.draw_center(scene, tr, cx, top);
        } else {
            self.draw(
                scene,
                tr,
                Point::new(cx - max_w / 2.0, top),
                Some(max_w as f32),
            );
        }
    }
}

// =========================================================================
// Drawing primitives
// =========================================================================
impl ImagerApp {
    /// Wrap a terminal failure into a small, measured block. Error strings can
    /// contain long device paths or platform diagnostics, so character-count
    /// truncation is not enough: every returned line is measured in the same
    /// font used to paint it and is guaranteed to fit `max_width`.
    fn failure_detail_lines(
        tr: &mut TextRenderer,
        detail: &str,
        max_width: f32,
        max_lines: usize,
    ) -> Vec<String> {
        const SIZE: f32 = 13.5;
        const WEIGHT: TextWeight = TextWeight::Regular;
        let mut remaining = detail.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut lines = Vec::new();
        while !remaining.is_empty() && lines.len() < max_lines {
            if tr.measure_weighted(&remaining, SIZE, None, WEIGHT).0 <= max_width {
                lines.push(remaining);
                break;
            }

            let last_line = lines.len() + 1 == max_lines;
            let ellipsis_width = if last_line {
                tr.measure_weighted("…", SIZE, None, WEIGHT).0
            } else {
                0.0
            };
            let allowed = (max_width - ellipsis_width).max(1.0);
            let mut fitting_end = 0;
            let mut whitespace_end = None;
            for (index, character) in remaining.char_indices() {
                let end = index + character.len_utf8();
                let Some(candidate) = remaining.get(..end) else {
                    break;
                };
                if tr.measure_weighted(candidate, SIZE, None, WEIGHT).0 > allowed {
                    break;
                }
                fitting_end = end;
                if character.is_whitespace() {
                    whitespace_end = Some(index);
                }
            }
            if fitting_end == 0 {
                // Even one glyph does not fit; returning no line is safer than
                // painting outside the card at extreme accessibility scales.
                break;
            }
            let split_at = whitespace_end
                .filter(|index| *index > 0)
                .unwrap_or(fitting_end);
            let Some(prefix) = remaining.get(..split_at) else {
                break;
            };
            let mut line = prefix.trim_end().to_string();
            if last_line {
                line.push('…');
                lines.push(line);
                break;
            }
            lines.push(line);
            let Some(rest) = remaining.get(split_at..) else {
                break;
            };
            remaining = rest.trim_start().to_string();
        }
        lines
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
    /// A crisp stroke on the outermost pixel row of `r` (HIG §9.5).
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(
            &Stroke::new(w),
            t,
            c,
            None,
            &RoundedRect::from_rect(r.inset(-w / 2.0), (radius - w / 2.0).max(0.0)),
        );
    }
    fn dashed_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        let stroke = Stroke::new(w).with_dashes(0.0, [7.0, 6.0]);
        scene.stroke(
            &stroke,
            t,
            c,
            None,
            &RoundedRect::from_rect(r.inset(-w / 2.0), (radius - w / 2.0).max(0.0)),
        );
    }
    /// A zero-state slot: the card fill under a dashed derived edge.
    fn dashed_card(scene: &mut Scene, t: Affine, r: Rect, radius: f64, fill: Color, w: f64) {
        Self::fill_rr(scene, t, r, radius, fill);
        Self::dashed_rr(scene, t, r, radius, surface::edge_on(fill), w);
    }
    /// The face of a hoverable list row or card (a release row, a drive
    /// card): raised at rest, the theme's hover tint under the pointer; its
    /// edge is `edge_on(face)` through `ui::surface::card`. Buttons are not
    /// painted here — they are `Button::paint` faces.
    fn row_face(theme: &Theme, hovered: bool) -> Color {
        if hovered {
            theme.bg_hover
        } else {
            theme.tokens.colors.surfaces.raised
        }
    }
    /// Ink for a label on a filled badge or button, resolved against the
    /// fill it sits on rather than assumed white (text: 4.5:1).
    fn ink_on(theme: &Theme, fill: Color) -> Color {
        theme
            .tokens
            .colors
            .ink_on(fill, Color::WHITE, ON_FILL_MIN_CONTRAST)
    }
    /// Ink for a glyph on a filled disc (a check mark, the failure "!"):
    /// the non-text bar (3:1), as the toolkit's Toggle knob and Slider thumb.
    fn glyph_ink(theme: &Theme, fill: Color) -> Color {
        theme
            .tokens
            .colors
            .ink_on(fill, Color::WHITE, NON_TEXT_MIN_CONTRAST)
    }
    fn bar(scene: &mut Scene, t: Affine, r: Rect, frac: f64, track: Color, fill: Color) {
        let radius = r.height() / 2.0;
        surface::card(scene, t, r, radius, track);
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

    /// Top of the line box of `s` centred in the band `band_y0 ..= band_y0 +
    /// band_h` (`ui::text_metrics::centered_text_top`) — text sits on its
    /// measured line inside its container, never on a hand-tuned offset.
    fn line_top(
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        weight: TextWeight,
        band_y0: f64,
        band_h: f64,
    ) -> f64 {
        let (_, h) = tr.measure_weighted(s, size, None, weight);
        centered_text_top(band_y0, band_h, f64::from(h))
    }

    /// A kind badge (`Tilo`, `ISO`, `USB`): the toolkit [`Badge`], as wide as
    /// its label needs, filling the band `at.y ..= at.y + band_h` from `at.x`.
    /// Returns the badge's rect so the text column can follow it.
    fn kind_badge(
        ctx: &mut PaintCtx<'_>,
        at: Point,
        band_h: f64,
        label: &str,
        fill: Color,
    ) -> Rect {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (x, top) = (at.x, at.y);
        let size = renderer_type_size(tr, theme.tokens.typography.scale.xs);
        let (w, _) = tr.measure(label, size, None);
        let badge = Rect::new(x, top, x + f64::from(w) + 2.0 * RELATED, top + band_h);
        Badge::paint_at(scene, t, badge, label, fill, tr, theme);
        badge
    }

    fn section_label(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        s: &str,
        x: f64,
        y: f64,
    ) {
        TextRun::new(
            s,
            TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            theme.tokens.colors.text_dim,
        )
        .draw(scene, tr, Point::new(x, y), None);
    }
}

// =========================================================================
// Layout
// =========================================================================
impl ImagerApp {
    /// Whether `content` takes the steps' compact form; see
    /// [`crate::compact_content`].
    fn compact_content(&self, content: Rect) -> bool {
        crate::compact_content(content.width(), self.bands.source.text_scale)
    }

    /// Title and description painted at the top of the Source step; one
    /// source for the painter and for the heading measurement.
    fn source_heading(&self) -> (&'static str, Option<&'static str>) {
        (
            "Choose the source image",
            Some(if self.releases().is_empty() {
                "The image to write. Pick a local .iso or .img file."
            } else {
                "The image to write. Pick a local .iso or .img file, or a Tilo release below."
            }),
        )
    }

    /// Title and description painted at the top of the Drive step.
    fn drive_heading() -> (&'static str, Option<&'static str>) {
        (
            "Select the target drive",
            Some("The drive to write to — everything on it will be erased."),
        )
    }

    /// Title and description painted at the top of the Customize step; a
    /// plain ISO has no description (its card says it is written unchanged).
    fn customize_heading(&self) -> (&'static str, Option<&'static str>) {
        let description = self.customizable().then(|| {
            if self.action == Action::Install {
                "Set up the machine Tilo will install — you can change any of this later."
            } else {
                "These choices travel with the installer, so setup on the device is shorter."
            }
        });
        ("Prepare your setup", description)
    }

    /// Title and description painted at the top of the Confirm step.
    fn confirm_heading(&self) -> (&'static str, Option<&'static str>) {
        match self.action {
            Action::Install => (
                "Confirm & install",
                Some("Review the install, then type the drive's name to unlock it."),
            ),
            Action::Flash => (
                "Confirm & write",
                Some("Review the write, then type the drive's name to unlock it."),
            ),
        }
    }

    /// Title painted at the top of the Write step (no description).
    fn write_heading(&self) -> (&'static str, Option<&'static str>) {
        let title = match (self.progress.phase == JobPhase::Writing, self.action) {
            (true, Action::Install) => "Installing…",
            (true, Action::Flash) => "Writing…",
            (false, _) => "Verifying…",
        };
        (title, None)
    }

    /// The heading group whose measured height positions the step's first
    /// content block: the title and its optional description. Done is a
    /// centred hero and has no heading.
    fn step_heading(&self) -> Option<(&'static str, Option<&'static str>)> {
        match self.step {
            Step::Source => Some(self.source_heading()),
            Step::Drive => Some(Self::drive_heading()),
            Step::Customize => Some(self.customize_heading()),
            Step::Confirm => Some(self.confirm_heading()),
            Step::Write => Some(self.write_heading()),
            Step::Done => None,
        }
    }

    /// The unmeasured 1× floor for the heading group at this content width.
    fn step_header_floor(&self, content: Rect) -> f64 {
        if self.compact_content(content) {
            STEP_HEADER_FLOOR_COMPACT
        } else {
            STEP_HEADER_FLOOR
        }
    }

    /// Distance from the content top to the first content block: the
    /// measured heading group when the renderer has supplied one, never less
    /// than the 1× floor the layout tests pin.
    fn step_header_h(&self, content: Rect) -> f64 {
        let floor = self.step_header_floor(content);
        self.step_heading_h.map_or(floor, |h| h.max(floor))
    }

    /// The page gutter for a window `width` logical pixels wide.
    fn gutter_for(width: f64) -> f64 {
        if width < COMPACT_GUTTER_BELOW {
            OUTER_COMPACT
        } else {
            OUTER
        }
    }

    /// The content column width for a window `width` logical pixels wide
    /// (mirrors [`Self::content_viewport_rect`]).
    fn content_width_for(width: f64) -> f64 {
        (width - Self::gutter_for(width) * 2.0)
            .min(CONTENT_MAX_W)
            .max(0.0)
    }

    /// Measure the current heading group at the width the content will get
    /// for a window `width` logical pixels wide. Returns the title line box
    /// and the whole group (title, gap, wrapped description, content gap).
    fn measure_step_heading(&self, tr: &mut TextRenderer, width: f64) -> Option<(f64, f64)> {
        let (_, sub) = self.step_heading()?;
        let content_w = Self::content_width_for(width);
        let title_h = tr.line(19.0);
        let group = match sub {
            Some(sub) => {
                title_h
                    + STEP_SUB_GAP
                    + tr.para(sub, 13.0, TextWeight::Regular, content_w)
                    + self.heading_gap
            }
            None => title_h + self.heading_gap,
        };
        Some((title_h, group))
    }

    fn content_viewport_rect(&self) -> Rect {
        let v = self.rect;
        let top = CRUMB_TOP + CRUMB_H + REGION;
        let bottom = v.y1 - FOOTER_H - SIBLING;
        let w = Self::content_width_for(v.width());
        let x0 = v.x0 + (v.width() - w) / 2.0;
        Rect::new(x0, v.y0 + top, x0 + w, v.y0 + bottom)
    }

    /// The step body's height at `viewport`'s width: every block measured,
    /// plus the bottom margin, never shorter than the viewport.
    fn content_extent(&self, viewport: Rect) -> f64 {
        let requested = match self.step {
            Step::Source => self.source_extent(viewport),
            Step::Drive => self.drive_extent(viewport),
            Step::Customize => self.customize_extent(viewport),
            Step::Confirm => self.confirm_extent(viewport),
            Step::Write => self.bands.write.extent(self.step_header_h(viewport)),
            Step::Done => self.bands.done.extent(),
        };
        viewport.height().max(requested)
    }

    fn max_scroll(&self) -> f64 {
        let viewport = self.content_viewport_rect();
        (self.content_extent(viewport) - viewport.height()).max(0.0)
    }

    fn content_rect(&self) -> Rect {
        let viewport = self.content_viewport_rect();
        let extent = self.content_extent(viewport);
        Rect::new(
            viewport.x0,
            viewport.y0 - self.scroll_y,
            viewport.x1,
            viewport.y0 - self.scroll_y + extent,
        )
    }

    fn compute_layout(&self) -> Layout {
        let v = self.rect;
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let content = self.content_rect();

        // ---- Step-indicator pills ----
        let steps = Step::all();
        let full_total = CRUMB_W * steps.len() as f64 + CRUMB_GAP * (steps.len() as f64 - 1.0);
        let compact_crumbs = v.width() - OUTER * 2.0 < full_total;
        let rail_margin = if compact_crumbs { OUTER_COMPACT } else { OUTER };
        let rail_w = (v.width() - rail_margin * 2.0).max(0.0);
        let (crumb_w, gap, total) = if compact_crumbs {
            // Six controls remain inside the content margins down to the
            // supported 320 px compact viewport. Prefer useful hit targets to
            // labels which cannot fit; the screen heading supplies the name.
            let gap = if rail_w >= 280.0 { RELATED } else { LABEL };
            let gaps = gap * (steps.len() as f64 - 1.0);
            let crumb_w = ((rail_w - gaps) / steps.len() as f64).max(24.0);
            (crumb_w, gap, crumb_w * steps.len() as f64 + gaps)
        } else {
            (CRUMB_W, CRUMB_GAP, full_total)
        };
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
            Step::Source => {
                self.layout_source(content, &mut hits);
            }
            Step::Drive => self.layout_drive(content, &mut hits),
            Step::Confirm => self.layout_confirm(content, &mut hits),
            // Customize's fields/toggles are owned by `CustomizePanel`, which
            // hit-tests its own rects in `handle_event`; Write (in progress)
            // and Done have no interactive content beyond the footer. No app
            // hits for any of the three.
            Step::Customize | Step::Write | Step::Done => {}
        }

        // ---- Footer (Back + primary) ----
        let fy = v.y1 - FOOTER_H;
        let by = fy + SECTION;
        let mut back = None;
        let show_back = matches!(self.step, Step::Drive | Step::Customize | Step::Confirm);
        if show_back {
            let width = if self.compact_content(content) {
                (content.width() - SIBLING) / 2.0
            } else {
                120.0
            };
            let r = Rect::new(content.x0, by, content.x0 + width, by + CONTROL_H);
            hits.push((r, Hit::Back));
            back = Some(r);
        }
        let p = self.primary_button_rect(content, by);
        let hit = match self.step {
            Step::Confirm => Hit::StartWrite,
            Step::Write => Hit::CancelWrite,
            Step::Done => Hit::WriteAnother,
            _ => Hit::Next,
        };
        hits.push((p, hit));
        let primary = Some(p);

        Layout {
            crumbs,
            compact_crumbs,
            back,
            primary,
            hits,
            viewport_content: self.content_viewport_rect(),
            content,
        }
    }

    /// The primary action button rect, right-aligned in the footer.
    fn primary_button_rect(&self, content: Rect, by: f64) -> Rect {
        let has_back = matches!(self.step, Step::Drive | Step::Customize | Step::Confirm);
        let w = if has_back && self.compact_content(content) {
            (content.width() - SIBLING) / 2.0
        } else {
            match self.step {
                Step::Confirm => 250.0,
                Step::Done => 190.0,
                _ => 170.0,
            }
        };
        Rect::new(content.x1 - w, by, content.x1, by + CONTROL_H)
    }

    /// Hit targets of the Source step, block by block in the measured bands
    /// the painters fill ([`SourceMetrics`]). Every block starts at the
    /// previous block's bottom plus its rhythm step: `SIBLING` between rows
    /// and between the drop zone and its result card, `GROUP` before a new
    /// section, whose caption band (`section_h`) then leads its list.
    /// Returns the y just under the last block.
    fn layout_source(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) -> f64 {
        let x0 = content.x0;
        let x1 = content.x1;
        let compact = self.compact_content(content);
        let m = &self.bands.source;
        let mut y = content.y0 + self.step_header_h(content);
        // Browse drop-zone.
        let browse = Rect::new(x0, y, x1, y + m.dropzone_h);
        hits.push((browse, Hit::PickFile));
        y = browse.y1;
        if self.source.is_some() {
            y += SIBLING + m.card_h(compact); // selected card
        }
        // Tilo releases: caption, then the zero-state card or the rows.
        y += GROUP + m.section_h;
        if self.releases().is_empty() {
            y += m.zero_h;
        }
        for (i, _) in self.releases().iter().enumerate() {
            if i > 0 {
                y += SIBLING;
            }
            let r = Rect::new(x0, y, x1, y + m.release_row_h);
            if self.release_download_index == Some(i) {
                hits.push((r, Hit::CancelDownload));
            } else if self.release_rx.is_none() {
                hits.push((r, Hit::Release(i)));
            }
            y = r.y1;
        }
        // Recent list.
        if !self.recent.is_empty() {
            y += GROUP + m.section_h;
            for (i, _) in self.recent.iter().enumerate().take(3) {
                if i > 0 {
                    y += SIBLING;
                }
                let r = Rect::new(x0, y, x1, y + m.recent_row_h);
                if r.y1 > content.y1 {
                    break;
                }
                hits.push((r, Hit::Recent(i)));
                y = r.y1;
            }
        }
        y
    }

    /// Height the Source step's blocks need at `viewport`'s width, measured
    /// with the same arithmetic that places the hits, plus the bottom margin.
    fn source_extent(&self, viewport: Rect) -> f64 {
        let unbounded = Rect::new(viewport.x0, 0.0, viewport.x1, f64::INFINITY);
        self.layout_source(unbounded, &mut Vec::new()) + SECTION
    }

    /// The Rescan button: top-right on the title line when the content is
    /// wide, on its own line under the heading when compact.
    fn drive_rescan_rect(&self, content: Rect) -> Rect {
        let y = if self.compact_content(content) {
            content.y0 + self.step_header_h(content)
        } else {
            content.y0
        };
        Rect::new(
            content.x1 - self.bands.drive.rescan_w,
            y,
            content.x1,
            y + SMALL_CONTROL_H,
        )
    }

    /// Where the first drive card (or the zero-state card) starts.
    fn drive_cards_top(&self, content: Rect) -> f64 {
        let head = content.y0 + self.step_header_h(content);
        if self.compact_content(content) {
            head + SMALL_CONTROL_H + SIBLING
        } else {
            head
        }
    }

    fn layout_drive(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        hits.push((self.drive_rescan_rect(content), Hit::Rescan));
        let card_h = self.bands.drive.card_h;
        let mut y = self.drive_cards_top(content);
        for (i, (idx, _)) in self.visible_targets().iter().enumerate() {
            if i > 0 {
                y += SIBLING;
            }
            let r = Rect::new(content.x0, y, content.x1, y + card_h);
            if r.y1 > content.y1 {
                break;
            }
            hits.push((r, Hit::Target(*idx)));
            y = r.y1;
        }
    }

    fn drive_extent(&self, viewport: Rect) -> f64 {
        let m = &self.bands.drive;
        let top = self.drive_cards_top(viewport) - viewport.y0;
        let n = self.visible_targets().len() as f64;
        let body = if n == 0.0 {
            m.empty_h
        } else {
            n * m.card_h + (n - 1.0) * SIBLING
        };
        top + body + SECTION
    }

    fn customize_extent(&self, viewport: Rect) -> f64 {
        let head = self.step_header_h(viewport);
        if self.customizable() {
            self.customize.extent(viewport)
        } else {
            head + self.bands.plain_card_h + SECTION
        }
    }

    fn layout_confirm(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        hits.push((self.confirm_field_rect(content), Hit::ConfirmField));
    }

    /// The review card's band on the Confirm step.
    fn confirm_card_rect(&self, content: Rect) -> Rect {
        let top = content.y0 + self.step_header_h(content);
        Rect::new(content.x0, top, content.x1, top + self.bands.confirm.card_h)
    }

    /// The danger banner follows the card as its sibling.
    fn confirm_banner_rect(&self, content: Rect) -> Rect {
        let top = self.confirm_card_rect(content).y1 + SIBLING;
        Rect::new(
            content.x0,
            top,
            content.x1,
            top + self.bands.confirm.banner_h,
        )
    }

    /// Origin of the optional on-write setup form (Wi-Fi / enrollment).
    fn write_setup_top(&self, content: Rect) -> f64 {
        self.confirm_banner_rect(content).y1 + GROUP
    }

    /// The typed device-name confirmation field. It follows the danger banner
    /// (and optional write-setup form) in reading order — review, warning,
    /// optional network/identity, type the name, then the footer's primary
    /// action — a `GROUP` below the previous block, under its caption line.
    /// One rect for the hit layout and the painter.
    fn confirm_field_rect(&self, content: Rect) -> Rect {
        let m = &self.bands.confirm;
        let after = if self.offers_write_setup() {
            self.write_setup_top(content)
                + self
                    .write_setup
                    .extent(content, self.write_setup_top(content))
        } else {
            self.confirm_banner_rect(content).y1 + GROUP
        };
        let top = after + m.label_h + LABEL;
        Rect::new(content.x0, top, content.x1, top + CONTROL_H)
    }

    fn confirm_extent(&self, viewport: Rect) -> f64 {
        let field = self.confirm_field_rect(viewport);
        field.y1 + RELATED + self.bands.confirm.note_h + SECTION - viewport.y0
    }

    /// The rows of the Confirm review card, in reading order. Capacity is
    /// only a row once both ends of the write are known.
    fn confirm_rows(&self) -> Vec<ConfirmRow> {
        let src_v = self.source.as_ref().map_or_else(
            || "—".into(),
            |s| format!("{}   ({})", s.name, fmt_size(s.size)),
        );
        let tgt_v = self.target().map_or_else(
            || "—".into(),
            |d| format!("{} · {} · {}", d.model, d.dev_path(), fmt_size(d.size)),
        );
        let mut rows = vec![
            ConfirmRow {
                key: "Image",
                value: src_v,
                size: 14.0,
                weight: TextWeight::Semibold,
                tone: if self.source.is_some() {
                    RowTone::Primary
                } else {
                    RowTone::Danger
                },
            },
            ConfirmRow {
                key: "Drive",
                value: tgt_v,
                size: 14.0,
                weight: TextWeight::Semibold,
                tone: if self.target().is_some() {
                    RowTone::Primary
                } else {
                    RowTone::Danger
                },
            },
        ];
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let (value, tone) = if tgt.fits(src.size) {
                (
                    format!(
                        "fits — {} free afterwards",
                        fmt_size(tgt.size.saturating_sub(src.size))
                    ),
                    RowTone::Success,
                )
            } else {
                ("image is larger than the drive".into(), RowTone::Danger)
            };
            rows.push(ConfirmRow {
                key: "Capacity",
                value,
                size: 13.0,
                weight: TextWeight::Medium,
                tone,
            });
        }
        let act = match self.action {
            Action::Install => format!(
                "Install Tilo with your settings (host {}, user {})",
                self.profile.hostname, self.profile.user.username
            ),
            Action::Flash => {
                let mut parts = if self.customizable() {
                    vec![format!(
                        "Write Tilo, pre-configured (host {}, user {})",
                        self.profile.hostname, self.profile.user.username
                    )]
                } else {
                    vec!["Write the image, then read-back verify".to_string()]
                };
                if !self.profile.wifi_ssid.trim().is_empty() {
                    parts.push(format!("Wi-Fi {}", self.profile.wifi_ssid.trim()));
                }
                if self.profile.enroll_local_identity {
                    let label = self.profile.device_label.trim();
                    if label.is_empty() {
                        parts.push("enroll identity".into());
                    } else {
                        parts.push(format!("enroll as {label}"));
                    }
                }
                parts.join(" · ")
            }
        };
        rows.push(ConfirmRow {
            key: "Action",
            value: act,
            size: 13.0,
            weight: TextWeight::Medium,
            tone: RowTone::Primary,
        });
        rows
    }

    /// The erase warning under the review card — louder for a fixed/internal
    /// (install) target.
    fn confirm_warning(&self) -> &'static str {
        let internal = self.target().is_some_and(|d| !d.removable);
        let system = self.target().is_some_and(|d| d.is_system);
        if system {
            "⚠  This is your system disk. Everything on it — the running OS included — is erased. This cannot be undone."
        } else if internal {
            "⚠  This erases the entire internal disk. This cannot be undone."
        } else {
            "⚠  This permanently erases the drive. This cannot be undone."
        }
    }

    /// The note under the confirmation field. Shot mode poses the field as
    /// ready for documentation, but never enables the backend.
    fn confirm_note(&self) -> &'static str {
        if crate::write::write_available() || self.shot_confirmation_ready {
            "Ready to write. Check the selected device before continuing."
        } else {
            "Writing is disabled in this preview."
        }
    }

    /// Measure the Confirm step's bands for a content column `content_w` wide.
    fn measure_confirm(&self, m: &mut dyn Measure, content_w: f64) -> ConfirmMetrics {
        let rows = self.confirm_rows();
        let key_w = rows
            .iter()
            .map(|row| m.width(row.key, 12.0, TextWeight::Semibold))
            .fold(0.0, f64::max);
        let value_x = SECTION + key_w + GROUP;
        let value_w = (content_w - value_x - SECTION).max(1.0);
        let key_h = m.line(12.0);
        let mut row_top = Vec::with_capacity(rows.len());
        let mut y = SECTION;
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                y += SIBLING;
            }
            row_top.push(y);
            y += key_h.max(m.para(&row.value, row.size, row.weight, value_w));
        }
        ConfirmMetrics {
            value_x,
            row_top,
            card_h: y + SECTION,
            banner_h: m.para(
                self.confirm_warning(),
                13.0,
                TextWeight::Semibold,
                (content_w - 2.0 * SECTION).max(1.0),
            ) + 2.0 * SIBLING,
            label_h: m.line(11.0),
            note_h: m.line(11.0),
        }
    }

    /// Measure the Done step's bands for a content column `content_w` wide,
    /// in its wide or `compact` form.
    fn measure_done(&self, m: &mut dyn Measure, content_w: f64, compact: bool) -> DoneMetrics {
        let ok = self.progress.phase == JobPhase::Success;
        let headline_h = m.line(26.0);
        // The success copy wraps to the content width (`txt_para_centered`);
        // the failure detail is bounded to two measured lines
        // (`failure_detail_lines`), so the card sits below that band whether
        // one or both are used and a longer cause never pushes it around.
        let sub_h = if ok {
            let (_, sub) = Self::done_ok_copy(self.action == Action::Install, compact);
            m.para(sub, 14.0, TextWeight::Regular, content_w)
        } else {
            2.0 * m.line(13.5)
        };
        let value_x = SECTION
            + ["Image", "Drive"]
                .iter()
                .map(|key| m.width(key, 12.0, TextWeight::Semibold))
                .fold(0.0, f64::max)
            + GROUP;
        let row_h = m.line(13.5).max(m.line(12.0));
        let headline_top = DoneMetrics::headline_top(0.0);
        let card_top = headline_top + headline_h + RELATED + sub_h + GROUP;
        DoneMetrics {
            headline_h,
            sub_h,
            value_x,
            row_h,
            card_top,
            card_h: SECTION + row_h + SIBLING + row_h + SECTION,
        }
    }
}

// =========================================================================
// Header + breadcrumb
// =========================================================================
impl ImagerApp {
    fn draw_scrollbar(&self, scene: &mut Scene, theme: &Theme, t: Affine, lay: &Layout) {
        let max = self.max_scroll();
        if max <= 0.0 {
            return;
        }
        let viewport = lay.viewport_content;
        let track = Rect::new(
            viewport.x1 + RELATED,
            viewport.y0,
            viewport.x1 + RELATED + LABEL,
            viewport.y1,
        );
        let thumb_h = (viewport.height() * viewport.height() / lay.content.height()).max(32.0);
        let travel = (track.height() - thumb_h).max(0.0);
        let y = track.y0 + travel * (self.scroll_y / max);
        Self::fill_rr(
            scene,
            t,
            Rect::new(track.x0, y, track.x1, y + thumb_h),
            LABEL / 2.0,
            theme.tokens.colors.text_tertiary,
        );
    }

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
            TextRun::new(
                &self.status,
                TextStyle::new(11.5).with_weight(TextWeight::Medium),
                col,
            )
            .draw_right(scene, tr, v.x1 - OUTER, v.y0 + RELATED);
        }

        // ---- Step indicator ----
        let radius = theme.radius_standard;
        for (i, (r, s)) in lay.crumbs.iter().enumerate() {
            let done = self.step_completed(*s);
            let current = *s == self.step;
            let hov = r.contains(self.last_mouse) && self.crumb_reachable(*s);
            let bg = if current {
                with_alpha(c.accent[5], 40)
            } else if hov {
                theme.bg_hover
            } else {
                Color::TRANSPARENT
            };
            if bg != Color::TRANSPARENT {
                Self::fill_rr(scene, t, *r, radius, bg);
            }
            if current {
                Self::stroke_rr(scene, t, *r, radius, c.accent[5], 1.4);
            }
            // Number disc / check, centred on the station's centre line.
            // Compact layouts center the disc in its station and omit the
            // label, keeping all six stations on-screen.
            let disc_r = CRUMB_H / 2.0 - LABEL;
            let disc_c = Point::new(
                if lay.compact_crumbs {
                    f64::midpoint(r.x0, r.x1)
                } else {
                    r.x0 + SIBLING + disc_r
                },
                f64::midpoint(r.y0, r.y1),
            );
            if done {
                Self::check_disc(
                    scene,
                    t,
                    disc_c,
                    disc_r,
                    c.semantic.success,
                    Self::glyph_ink(theme, c.semantic.success),
                );
            } else {
                let ring = if current { c.accent[5] } else { c.neutral[4] };
                scene.fill(Fill::NonZero, t, ring, None, &Circle::new(disc_c, disc_r));
                TextRun::new(
                    &format!("{}", s.index() + 1),
                    TextStyle::new(12.0).with_weight(TextWeight::Bold),
                    Self::ink_on(theme, ring),
                )
                .draw_center_v(
                    scene,
                    tr,
                    disc_c.x,
                    disc_c.y - disc_r,
                    2.0 * disc_r,
                );
            }
            if !lay.compact_crumbs {
                let label_c = if current || done {
                    c.text_primary
                } else {
                    c.text_dim
                };
                let weight = if current {
                    TextWeight::Semibold
                } else {
                    TextWeight::Medium
                };
                let label_top =
                    Self::line_top(tr, s.title(), 13.5, weight, disc_c.y - disc_r, 2.0 * disc_r);
                TextRun::new(s.title(), TextStyle::new(13.5).with_weight(weight), label_c).draw(
                    scene,
                    tr,
                    Point::new(disc_c.x + disc_r + RELATED, label_top),
                    None,
                );
            }
            // Connector to the next pill, inset a step from both stations.
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
                    &Line::new(
                        Point::new(r.x1 + LABEL, midy),
                        Point::new(nx0 - LABEL, midy),
                    ),
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
        // The footer rule sits on `surfaces.base`, derived against it.
        scene.fill(
            Fill::NonZero,
            t,
            surface::edge_on(c.surfaces.base),
            None,
            &Rect::new(v.x0, fy - 0.5, v.x1, fy + 0.5),
        );

        // Back: the toolkit's neutral Button face — `Button::paint` shares
        // `face_colors` with the retained widget, so it matches every other
        // button. The keyboard focus ring is painted once for every hit by
        // `draw`, not doubled here.
        if let Some(r) = lay.back {
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(r, "‹  Back")
                    .radius(theme.radius_standard)
                    .hovered(r.contains(self.last_mouse)),
            );
        }

        // Primary.
        if let Some(r) = lay.primary {
            self.draw_primary(scene, tr, theme, t, r);
        }

        // A hint between them (center) — e.g. what's still needed.
        if !self.compact_content(lay.content)
            && let Some(hint) = self.footer_hint()
        {
            TextRun::new(&hint, TextStyle::new(12.0), c.text_dim).draw_center_v(
                scene,
                tr,
                f64::midpoint(v.x0, v.x1),
                fy,
                FOOTER_H,
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
                let label = if r.width() < 220.0 {
                    format!("Erase & {}", self.action.verb().to_ascii_lowercase())
                } else {
                    format!("Erase & {}  {dev}", self.action.verb().to_ascii_lowercase())
                };
                (
                    label,
                    self.can_advance(Step::Confirm) || self.shot_confirmation_ready,
                    true,
                )
            }
            Step::Write => (
                if self
                    .job_control
                    .as_ref()
                    .is_some_and(JobControl::is_cancelled)
                {
                    "Stopping…".to_string()
                } else {
                    "Cancel write".to_string()
                },
                self.job_control
                    .as_ref()
                    .is_some_and(|control| !control.is_cancelled()),
                false,
            ),
            Step::Done => match self.progress.phase {
                JobPhase::Failed => ("Review & retry".to_string(), true, false),
                _ => ("Start over".to_string(), true, false),
            },
        };
        // The toolkit Button face (`Button::paint` shares `face_colors` with
        // the retained widget): a disabled action is one opaque neutral face
        // with quieter ink, never a dimmed accent that still reads as
        // actionable; the enabled primary / danger fills carry ink resolved
        // against them, and hover shifts the fill without touching the ink.
        let face = ButtonFace::new(r, &label)
            .radius(theme.radius_standard)
            .weight(TextWeight::Bold)
            .hovered(hov)
            .disabled(!enabled);
        let face = if destructive {
            face.danger()
        } else {
            face.primary()
        };
        Button::paint(scene, t, tr, theme, &face);
    }
}

// =========================================================================
// Step bodies
// =========================================================================
impl ImagerApp {
    fn draw_step_title(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        title: &str,
        sub: Option<&str>,
    ) {
        let c = &theme.tokens.colors;
        let (_, title_h) = tr.measure_weighted(title, 19.0, None, TextWeight::Bold);
        TextRun::new(
            title,
            TextStyle::new(19.0).with_weight(TextWeight::Bold),
            c.text_primary,
        )
        .draw(scene, tr, Point::new(content.x0, content.y0), None);
        if let Some(sub) = sub {
            TextRun::new(sub, TextStyle::new(13.0), c.text_dim).draw(
                scene,
                tr,
                Point::new(content.x0, content.y0 + f64::from(title_h) + STEP_SUB_GAP),
                Some(content.width() as f32),
            );
        }
    }

    // ---- Source -----------------------------------------------------------

    fn draw_source_step(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let content = lay.content;
        let (title, sub) = self.source_heading();
        Self::draw_step_title(scene, tr, theme, content, title, sub);
        let browse = self.draw_source_dropzone(scene, tr, theme, t, content);
        let mut bottom = browse.y1;
        if self.source.is_some() {
            bottom = self.draw_source_selected_card(scene, tr, theme, t, content, bottom + SIBLING);
        }
        let bottom = self.draw_source_releases(scene, tr, theme, t, content, bottom + GROUP);
        self.draw_source_recent(scene, tr, theme, t, content, bottom);
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
        let compact = self.compact_content(content);
        let m = &self.bands.source;
        let y = content.y0 + self.step_header_h(content);
        let browse = Rect::new(content.x0, y, content.x1, y + m.dropzone_h);
        let hov = browse.contains(self.last_mouse);
        let radius = theme.radius_standard;
        Self::fill_rr(
            scene,
            t,
            browse,
            radius,
            if hov {
                theme.bg_hover
            } else {
                with_alpha(c.accent[5], 16)
            },
        );
        Self::dashed_rr(
            scene,
            t,
            browse,
            radius,
            if hov {
                c.accent[6]
            } else {
                with_alpha(c.accent[5], 160)
            },
            1.6,
        );
        // Download/arrow glyph in an `ICON` box, inset a gutter from the edge
        // and centred on the band.
        let inset = if compact { SECTION } else { GROUP };
        let gx = browse.x0 + inset + ICON / 2.0;
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
                Point::new(gx - ICON / 2.0, gy + 15.0),
                Point::new(gx + ICON / 2.0, gy + 15.0),
            ),
        );
        // Title + hint stacked and centred in the band (see `SourceMetrics`),
        // a `SECTION` after the icon.
        let text_x = gx + ICON / 2.0 + SECTION;
        let text_w = (browse.x1 - text_x - SECTION).max(0.0) as f32;
        TextRun::new(
            "Browse for an image…",
            TextStyle::new(16.0).with_weight(TextWeight::Semibold),
            c.text_primary,
        )
        .draw(
            scene,
            tr,
            Point::new(text_x, browse.y0 + m.dz_title_top),
            Some(text_w),
        );
        TextRun::new(
            if compact {
                "Choose an .iso, .img, or .raw file"
            } else {
                "Opens the system file picker · .iso, .img, .raw"
            },
            TextStyle::new(12.5),
            c.text_dim,
        )
        .draw(
            scene,
            tr,
            Point::new(text_x, browse.y0 + m.dz_sub_top),
            Some(text_w),
        );
        browse
    }

    /// Draws the selected-source summary card starting at `top`, returning
    /// its bottom edge.
    #[allow(
        clippy::too_many_lines,
        reason = "the compact and regular source-card variants intentionally share one renderer so their visual state cannot drift"
    )]
    fn draw_source_selected_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        top: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let Some(src) = &self.source else {
            return top;
        };
        let compact = self.compact_content(content);
        let m = &self.bands.source;
        let card = Rect::new(content.x0, top, content.x1, top + m.card_h(compact));
        Self::fill_rr(scene, t, card, theme.radius_standard, c.surfaces.raised);
        Self::stroke_rr(
            scene,
            t,
            card,
            theme.radius_standard,
            with_alpha(c.semantic.success, 150),
            1.4,
        );
        // The check disc sits a `SECTION` in; the badge band starts at the
        // top padding and is as wide as its label needs. The compact check
        // rides the badge line's centre, the wide one the card's.
        let badge_top = card.y0 + SECTION;
        let badge_h = m.card_badge_h;
        let disc_x = card.x0 + if compact { RELATED } else { SECTION } + CHECK_R;
        Self::check_disc(
            scene,
            t,
            Point::new(
                disc_x,
                if compact {
                    badge_top + badge_h / 2.0
                } else {
                    f64::midpoint(card.y0, card.y1)
                },
            ),
            CHECK_R,
            c.semantic.success,
            Self::glyph_ink(theme, c.semantic.success),
        );
        let badge_x = disc_x + CHECK_R + if compact { RELATED } else { SIBLING };
        let badge = Self::kind_badge(
            &mut PaintCtx::new(scene, t, tr, theme),
            Point::new(badge_x, badge_top),
            badge_h,
            src.kind.tag(),
            source_color(theme, src.kind),
        );
        let title_x = badge.x1 + SIBLING;
        let detail_x = if compact { badge_x } else { title_x };
        let (summary_top, tag_top) = if compact {
            (m.card_summary_top_compact, m.card_tag_top_compact)
        } else {
            (m.card_summary_top, m.card_tag_top)
        };
        let title_max_w = (card.x1 - title_x - SECTION).max(0.0) as f32;
        let name = tr.elide_to_width_weighted(&src.name, 15.5, title_max_w, TextWeight::Semibold);
        // The compact name shares the badge's centre line; the wide one sits
        // on the card's top padding.
        let title_top = if compact {
            Self::line_top(
                tr,
                &name,
                15.5,
                TextWeight::Semibold,
                badge.y0,
                badge.height(),
            )
        } else {
            card.y0 + SECTION
        };
        TextRun::new(
            &name,
            TextStyle::new(15.5).with_weight(TextWeight::Semibold),
            c.text_primary,
        )
        .draw(scene, tr, Point::new(title_x, title_top), Some(title_max_w));
        // One line: a long image path elides against the card instead of
        // wrapping into the tag line beneath it.
        let summary = if compact {
            format!("{}  ·  {}", src.label, fmt_size(src.size))
        } else {
            src.summary()
        };
        let detail_max_w = (card.x1 - detail_x - SECTION).max(0.0) as f32;
        let summary = tr.elide_to_width(&summary, 12.0, detail_max_w);
        TextRun::new(&summary, TextStyle::new(12.0), c.text_dim).draw(
            scene,
            tr,
            Point::new(detail_x, card.y0 + summary_top),
            Some(detail_max_w),
        );
        let (tag, tag_c) = if self.source_is_official(src) {
            (
                "● Official Tilo release · catalog verified",
                c.semantic.success,
            )
        } else {
            ("Local image · unverified · written as-is", c.text_dim)
        };
        TextRun::new(
            tag,
            TextStyle::new(11.5).with_weight(TextWeight::Medium),
            tag_c,
        )
        .draw(
            scene,
            tr,
            Point::new(detail_x, card.y0 + tag_top),
            Some(detail_max_w),
        );
        card.y1
    }

    /// Draws the "Tilo release" caption, its zero-state or the list of known
    /// releases from `top`, returning the bottom of the block.
    fn draw_source_releases(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        top: f64,
    ) -> f64 {
        // Tilo releases. With none to choose between, the heading names the
        // section rather than offering a choice that is not there.
        let heading = if self.releases().is_empty() {
            "Tilo RELEASES"
        } else {
            "OR CHOOSE A Tilo RELEASE"
        };
        Self::section_label(scene, tr, theme, heading, content.x0, top);
        let mut y = top + self.bands.source.section_h;
        if self.releases().is_empty() {
            // Nothing is known to be on this machine, so nothing is offered.
            // The honest zero-state says so rather than leaving a bare heading.
            y = self.draw_no_releases_card(scene, tr, theme, t, content, y);
        }
        for (i, rel) in self.releases().iter().enumerate() {
            if i > 0 {
                y += SIBLING;
            }
            let row = Rect::new(
                content.x0,
                y,
                content.x1,
                y + self.bands.source.release_row_h,
            );
            y = self.draw_release_row(&mut PaintCtx::new(scene, t, tr, theme), row, i, rel);
        }
        y
    }

    /// The empty-state card shown in place of the release list when nothing
    /// is known to be on this machine. Returns its bottom edge.
    fn draw_no_releases_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let card = Rect::new(content.x0, y, content.x1, y + self.bands.source.zero_h);
        Self::dashed_card(
            scene,
            t,
            card,
            theme.radius_standard,
            c.surfaces.raised,
            1.2,
        );
        let cx = f64::midpoint(card.x0, card.x1);
        // Title + hint stack centred in the card's measured band.
        let title_h = tr.line(14.0);
        let sub_h = tr.line(12.0);
        let stack_h = title_h + RELATED + sub_h;
        let title_top = card.y0 + (card.height() - stack_h) / 2.0;
        TextRun::new(
            "No Tilo releases found",
            TextStyle::new(14.0).with_weight(TextWeight::Semibold),
            c.text_primary,
        )
        .draw_center(scene, tr, cx, title_top);
        TextRun::new(
            "Releases stored on this machine appear here.",
            TextStyle::new(12.0),
            c.text_dim,
        )
        .draw_center(scene, tr, cx, title_top + title_h + RELATED);
        card.y1
    }

    /// One release row: selection badge, name, channel/size/location, and the
    /// "installable" tag. Returns its bottom edge.
    fn draw_release_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        r: Rect,
        index: usize,
        rel: &TiloRelease,
    ) -> f64 {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let c = &theme.tokens.colors;
        let m = &self.bands.source;
        let selected = self
            .source
            .as_ref()
            .is_some_and(|source| rel.matches_source(source));
        let hov = r.contains(self.last_mouse);
        let radius = theme.radius_standard;
        if selected {
            Self::fill_rr(scene, t, r, radius, with_alpha(c.accent[5], 34));
            Self::stroke_rr(scene, t, r, radius, c.accent[5], 1.4);
        } else {
            surface::card(scene, t, r, radius, Self::row_face(theme, hov));
        }
        // The kind badge on its band, centred in the row and as wide as its
        // label needs; the text column starts a `SIBLING` after it.
        let badge = Self::kind_badge(
            &mut PaintCtx::new(scene, t, tr, theme),
            Point::new(r.x0 + SECTION, r.y0 + (r.height() - m.card_badge_h) / 2.0),
            m.card_badge_h,
            "Tilo",
            c.accent[5],
        );
        let text_x = badge.x1 + SIBLING;
        let title = format!("Tilo {}", rel.version);
        let (title_w, title_h) = tr.measure_weighted(&title, 14.5, None, TextWeight::Semibold);
        let (inst_w, _) = tr.measure_weighted("installable", 11.0, None, TextWeight::Medium);
        let title_max_w = (r.x1 - SECTION - f64::from(inst_w) - SIBLING - text_x)
            .max(f64::from(title_w).min(72.0)) as f32;
        TextRun::new(
            &rel.display_name(),
            TextStyle::new(14.5).with_weight(TextWeight::Semibold),
            c.text_primary,
        )
        .draw(
            scene,
            tr,
            Point::new(text_x, r.y0 + RELATED),
            Some(title_max_w),
        );
        let downloading = self.release_download_index == Some(index);
        let where_ = if downloading {
            self.release_download_stage.unwrap_or("preparing")
        } else if rel.local_path.is_empty() {
            "download"
        } else {
            "on disk"
        };
        TextRun::new(
            &format!(
                "{}  ·  {}  ·  {}  ·  {}",
                rel.target_label(),
                rel.channel,
                fmt_size(rel.size),
                where_
            ),
            TextStyle::new(11.5),
            c.text_dim,
        )
        .draw(
            scene,
            tr,
            Point::new(text_x, r.y0 + m.release_detail_top),
            Some((r.x1 - SECTION - text_x).max(0.0) as f32),
        );
        // The tag shares the title's centre line.
        TextRun::new(
            if downloading {
                "Cancel"
            } else {
                "official · verified"
            },
            TextStyle::new(11.0).with_weight(TextWeight::Medium),
            if downloading {
                c.semantic.danger
            } else {
                c.semantic.success
            },
        )
        .draw_right_v(scene, tr, r.x1 - 16.0, r.y0 + RELATED, f64::from(title_h));
        if downloading {
            let fraction = self
                .release_download_progress
                .and_then(|(done, total)| (total > 0).then_some(done as f64 / total as f64))
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let track = Rect::new(r.x0 + 68.0, r.y1 - 5.0, r.x1 - 16.0, r.y1 - 2.0);
            Self::fill_rr(scene, t, track, 1.5, c.border_standard);
            if fraction > 0.0 {
                Self::fill_rr(
                    scene,
                    t,
                    Rect::new(
                        track.x0,
                        track.y0,
                        track.x0 + track.width() * fraction,
                        track.y1,
                    ),
                    1.5,
                    c.accent[5],
                );
            }
        }
        r.y0 + 54.0 + 10.0
    }

    /// Draws the "Recent" section (up to the three most recent sources) a
    /// `GROUP` below `bottom`.
    fn draw_source_recent(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        bottom: f64,
    ) {
        let c = &theme.tokens.colors;
        if self.recent.is_empty() {
            return;
        }
        let m = &self.bands.source;
        let top = bottom + GROUP;
        Self::section_label(scene, tr, theme, "RECENT", content.x0, top);
        let mut y = top + m.section_h;
        for (i, src) in self.recent.iter().take(3).enumerate() {
            if i > 0 {
                y += SIBLING;
            }
            let r = Rect::new(content.x0, y, content.x1, y + m.recent_row_h);
            if r.y1 > content.y1 {
                break;
            }
            let hov = r.contains(self.last_mouse);
            if hov {
                Self::fill_rr(scene, t, r, theme.radius_standard, theme.bg_hover);
            }
            // Kind swatch: a `LABEL`-wide bar inset a `RELATED` from the row.
            let sw = Rect::new(
                r.x0 + RELATED,
                r.y0 + RELATED,
                r.x0 + RELATED + LABEL,
                r.y1 - RELATED,
            );
            Self::fill_rr(scene, t, sw, LABEL / 2.0, source_color(theme, src.kind));
            let text_x = sw.x1 + SIBLING;
            let (tag_w, _) = tr.measure_weighted(src.kind.tag(), 10.5, None, TextWeight::Semibold);
            TextRun::new(
                &src.name,
                TextStyle::new(13.0).with_weight(TextWeight::Medium),
                c.text_primary,
            )
            .draw(
                scene,
                tr,
                Point::new(text_x, r.y0 + RELATED),
                Some((r.x1 - SECTION - f64::from(tag_w) - SIBLING - text_x).max(0.0) as f32),
            );
            TextRun::new(&fmt_size(src.size), TextStyle::new(11.0), c.text_dim).draw(
                scene,
                tr,
                Point::new(text_x, r.y0 + m.recent_size_top),
                None,
            );
            // The kind tag is centred on the row.
            TextRun::new(
                src.kind.tag(),
                TextStyle::new(10.5).with_weight(TextWeight::Semibold),
                c.text_tertiary,
            )
            .draw_right_v(scene, tr, r.x1 - SECTION, r.y0, r.height());
            y = r.y1;
        }
    }

    // ---- Drive ------------------------------------------------------------

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
        let (title, sub) = Self::drive_heading();
        Self::draw_step_title(scene, tr, theme, content, title, sub);
        let compact = self.compact_content(content);
        // Rescan button, with the detected count on its centre line: to its
        // left when wide, at the content's left edge when compact.
        let rescan = self.drive_rescan_rect(content);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(rescan, RESCAN_LABEL)
                .radius(theme.radius_standard)
                .text_size(12.5)
                .hovered(rescan.contains(self.last_mouse)),
        );
        let detected = format!("{} removable", self.removable_count());
        if compact {
            let count_top = Self::line_top(
                tr,
                &detected,
                11.5,
                TextWeight::Medium,
                rescan.y0,
                rescan.height(),
            );
            TextRun::new(
                &detected,
                TextStyle::new(11.5).with_weight(TextWeight::Medium),
                c.text_dim,
            )
            .draw(scene, tr, Point::new(content.x0, count_top), None);
        } else {
            TextRun::new(
                &detected,
                TextStyle::new(11.5).with_weight(TextWeight::Medium),
                c.text_dim,
            )
            .draw_right_v(scene, tr, rescan.x0 - SIBLING, rescan.y0, rescan.height());
        }

        let vis = self.visible_targets();
        if vis.is_empty() {
            self.draw_drive_empty(scene, tr, theme, t, content);
            return;
        }
        let card_h = self.bands.drive.card_h;
        let mut y = self.drive_cards_top(content);
        for (i, (idx, drive)) in vis.iter().enumerate() {
            if i > 0 {
                y += SIBLING;
            }
            let r = Rect::new(content.x0, y, content.x1, y + card_h);
            if r.y1 > content.y1 {
                break;
            }
            self.draw_drive_card(scene, tr, theme, t, r, *idx, drive);
            y = r.y1;
        }
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
        let m = &self.bands.drive;
        let top = self.drive_cards_top(content);
        let card = Rect::new(content.x0, top, content.x1, top + m.empty_h);
        Self::dashed_card(
            scene,
            t,
            card,
            theme.radius_standard,
            c.surfaces.raised,
            1.4,
        );
        let cx = f64::midpoint(card.x0, card.x1);
        // A simple USB-stick glyph in the card's own ink tiers.
        let gy = card.y0 + m.empty_glyph_top;
        let cap = Rect::new(cx - 10.0, gy, cx + 10.0, gy + GLYPH_CAP_H);
        Self::fill_rr(scene, t, cap, theme.tokens.radii.sm.into(), c.text_dim);
        let body = Rect::new(cx - 16.0, cap.y1, cx + 16.0, cap.y1 + GLYPH_BODY_H);
        Self::fill_rr(
            scene,
            t,
            body,
            theme.tokens.radii.sm.into(),
            c.text_tertiary,
        );
        TextRun::new(
            EMPTY_TITLE,
            TextStyle::new(17.0).with_weight(TextWeight::Semibold),
            c.text_primary,
        )
        .draw_center(scene, tr, cx, card.y0 + m.empty_title_top);
        let para_w = (card.width() - 2.0 * GROUP).max(0.0);
        TextRun::new(EMPTY_LINE_1, TextStyle::new(13.0), c.text_dim).draw_para_centered(
            scene,
            tr,
            cx,
            card.y0 + m.empty_line_top,
            para_w,
        );
        TextRun::new(EMPTY_LINE_2, TextStyle::new(12.0), c.text_tertiary).draw_para_centered(
            scene,
            tr,
            cx,
            card.y0 + m.empty_line2_top,
            para_w,
        );
    }

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
        let m = &self.bands.drive;
        let selected = self.selected == Some(idx);
        let hover = self.hover_target == Some(idx) && !selected;
        let danger = !drive.removable;
        let radius = theme.radius_standard;
        if selected {
            Self::fill_rr(scene, t, r, radius, with_alpha(c.accent[5], 30));
            Self::stroke_rr(scene, t, r, radius, c.accent[5], 2.0);
        } else {
            surface::card(scene, t, r, radius, Self::row_face(theme, hover));
            if danger {
                Self::stroke_rr(scene, t, r, radius, with_alpha(c.semantic.danger, 150), 1.0);
            }
        }

        // Selection check on the left, on the card's centre line.
        if selected {
            Self::check_disc(
                scene,
                t,
                Point::new(r.x0 + SECTION + CHECK_R, f64::midpoint(r.y0, r.y1)),
                CHECK_R,
                c.accent[5],
                Self::glyph_ink(theme, c.accent[5]),
            );
        }
        let left = if selected {
            r.x0 + SECTION + 2.0 * CHECK_R + SIBLING
        } else {
            r.x0 + SECTION
        };

        // Kind badge: as wide as its label needs, on the badge band.
        Self::kind_badge(
            &mut PaintCtx::new(scene, t, tr, theme),
            Point::new(left, r.y0 + SECTION),
            m.badge_h,
            drive.kind.tag(),
            kind_color(theme, drive.kind),
        );

        self.draw_drive_card_details(scene, tr, theme, t, r, drive, left);
    }

    /// Draws the removable/fixed/system chip, the model + capacity line, and
    /// the destructive-erase note for a drive card, each on its measured line.
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
        let m = &self.bands.drive;
        // Removable / fixed / system chip on the right, on the badge band.
        let (chip_label, chip_c) = if drive.is_system {
            ("SYSTEM DISK", c.semantic.danger)
        } else if drive.removable {
            ("REMOVABLE", c.semantic.success)
        } else {
            ("INTERNAL DISK", c.semantic.warning)
        };
        let (cw, _) = tr.measure_weighted(chip_label, 10.5, None, TextWeight::Bold);
        let chip = Rect::new(
            r.x1 - SECTION - f64::from(cw) - 2.0 * RELATED,
            r.y0 + SECTION,
            r.x1 - SECTION,
            r.y0 + SECTION + m.badge_h,
        );
        Self::fill_rr(
            scene,
            t,
            chip,
            theme.tokens.radii.sm.into(),
            with_alpha(chip_c, 40),
        );
        TextRun::new(
            chip_label,
            TextStyle::new(10.5).with_weight(TextWeight::Bold),
            chip_c,
        )
        .draw_center_v(
            scene,
            tr,
            f64::midpoint(chip.x0, chip.x1),
            chip.y0,
            chip.height(),
        );

        // Model + capacity, then the erase note: three single lines a `LABEL`
        // apart on the bands `DriveMetrics` measured, each elided to the
        // column — an unbounded `lsblk` model must never wrap into the line
        // below it.
        let max_w = (r.x1 - SECTION - left).max(0.0) as f32;
        let model = tr.elide_to_width_weighted(&drive.model, 16.0, max_w, TextWeight::Semibold);
        TextRun::new(
            &model,
            TextStyle::new(16.0).with_weight(TextWeight::Semibold),
            c.text_primary,
        )
        .draw(scene, tr, Point::new(left, r.y0 + m.model_top), None);
        let device = tr.elide_to_width_weighted(
            &format!("{}  ·  {}", drive.dev_path(), fmt_size(drive.size)),
            12.5,
            max_w,
            TextWeight::Regular,
        );
        TextRun::new(&device, TextStyle::new(12.5), c.text_dim).draw(
            scene,
            tr,
            Point::new(left, r.y0 + m.dev_top),
            None,
        );
        let erase = tr.elide_to_width_weighted(
            &format!("⚠ Erases {}", drive.contents_summary()),
            11.5,
            max_w,
            TextWeight::Medium,
        );
        TextRun::new(
            &erase,
            TextStyle::new(11.5).with_weight(TextWeight::Medium),
            c.semantic.danger,
        )
        .draw(scene, tr, Point::new(left, r.y0 + m.erase_top), None);
    }

    // ---- Customize --------------------------------------------------------

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
        let (title, sub) = self.customize_heading();
        Self::draw_step_title(scene, tr, theme, content, title, sub);
        if !self.customizable() {
            let top = content.y0 + self.step_header_h(content);
            let card = Rect::new(content.x0, top, content.x1, top + self.bands.plain_card_h);
            surface::card(scene, t, card, theme.radius_standard, c.surfaces.raised);
            let cx = f64::midpoint(card.x0, card.x1);
            let title_h = tr.line(16.0);
            TextRun::new(
                PLAIN_TITLE,
                TextStyle::new(16.0).with_weight(TextWeight::Semibold),
                c.text_primary,
            )
            .draw_center(scene, tr, cx, card.y0 + SECTION);
            TextRun::new(PLAIN_BODY, TextStyle::new(13.0), c.text_dim).draw_para_centered(
                scene,
                tr,
                cx,
                card.y0 + SECTION + title_h + RELATED,
                (card.width() - 2.0 * SECTION).max(0.0),
            );
            return;
        }

        // The whole form is the shared guided-customization panel.
        self.customize
            .draw(&mut PaintCtx::new(scene, t, tr, theme), presence, content);
    }

    // ---- Confirm ----------------------------------------------------------

    fn draw_confirm_step(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        presence: &PresenceCtx,
    ) {
        let content = lay.content;
        let (title, sub) = self.confirm_heading();
        Self::draw_step_title(scene, tr, theme, content, title, sub);
        self.draw_confirm_review_card(scene, tr, theme, t, content);
        self.draw_confirm_danger_banner(scene, tr, theme, t, content);
        if self.offers_write_setup() {
            let top = self.write_setup_top(content);
            self.write_setup.draw(
                &mut PaintCtx::new(scene, t, tr, theme),
                presence,
                content,
                top,
                &self.profile,
            );
        }
        self.draw_confirm_field(scene, tr, theme, t, content);
    }

    /// The review card: key/value rows on the measured bands of
    /// [`ConfirmMetrics`], key and value sharing one centre line per row.
    fn draw_confirm_review_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let m = &self.bands.confirm;
        let card = self.confirm_card_rect(content);
        surface::card(scene, t, card, theme.radius_standard, c.surfaces.raised);
        let value_x = card.x0 + m.value_x;
        let value_w = (card.x1 - SECTION - value_x).max(0.0) as f32;
        for (row, top) in self.confirm_rows().iter().zip(m.row_top.iter()) {
            let y = card.y0 + top;
            let (_, kh) = tr.measure_weighted(row.key, 12.0, None, TextWeight::Semibold);
            let (_, vh) = tr.measure_weighted(&row.value, row.size, Some(value_w), row.weight);
            let key_top = y + (f64::from(vh) - f64::from(kh)).max(0.0) / 2.0;
            let value_top = y + (f64::from(kh) - f64::from(vh)).max(0.0) / 2.0;
            TextRun::new(
                row.key,
                TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                c.text_dim,
            )
            .draw(scene, tr, Point::new(card.x0 + SECTION, key_top), None);
            let col = match row.tone {
                RowTone::Primary => c.text_primary,
                RowTone::Success => c.semantic.success,
                RowTone::Danger => c.semantic.danger,
            };
            TextRun::new(
                &row.value,
                TextStyle::new(row.size).with_weight(row.weight),
                col,
            )
            .draw(scene, tr, Point::new(value_x, value_top), Some(value_w));
        }
    }

    /// Danger banner below the review card — louder for a fixed/internal
    /// (install) target.
    fn draw_confirm_danger_banner(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let internal = self.target().is_some_and(|d| !d.removable);
        let banner = self.confirm_banner_rect(content);
        let radius = theme.radius_standard;
        Self::fill_rr(
            scene,
            t,
            banner,
            radius,
            with_alpha(c.semantic.danger, if internal { 40 } else { 28 }),
        );
        Self::stroke_rr(
            scene,
            t,
            banner,
            radius,
            with_alpha(c.semantic.danger, if internal { 170 } else { 130 }),
            if internal { 1.4 } else { 1.0 },
        );
        TextRun::new(
            self.confirm_warning(),
            TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            c.semantic.danger,
        )
        .draw(
            scene,
            tr,
            Point::new(banner.x0 + SECTION, banner.y0 + SIBLING),
            Some((banner.width() - 2.0 * SECTION).max(0.0) as f32),
        );
    }

    /// The typed confirmation field under its caption, and the write-gate
    /// note below it.
    fn draw_confirm_field(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let m = &self.bands.confirm;
        let field = self.confirm_field_rect(content);
        let focused = self.confirm_input.is_focused();
        let matches = self.confirm_matches();
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        TextRun::new(
            "CONFIRM THE TARGET DEVICE",
            TextStyle::new(11.0).with_weight(TextWeight::Semibold),
            c.text_dim,
        )
        .draw(
            scene,
            tr,
            Point::new(field.x0, field.y0 - LABEL - m.label_h),
            None,
        );
        // The field is the toolkit input's face — raised at rest, the active
        // tint while focused — under the gate's own semantic edge: danger
        // until the name matches, focus while typing, success once armed.
        let radius = theme.radius_standard;
        Self::fill_rr(
            scene,
            t,
            field,
            radius,
            if focused {
                theme.bg_active
            } else {
                c.surfaces.raised
            },
        );
        Self::stroke_rr(
            scene,
            t,
            field,
            radius,
            if matches {
                c.semantic.success
            } else if focused {
                c.border_focus
            } else {
                with_alpha(c.semantic.danger, 160)
            },
            if focused || matches { 2.0 } else { 1.0 },
        );
        self.confirm_input
            .set_placeholder(&format!("type “{want}” here"));
        let (armed_w, _) = tr.measure_weighted("✓ armed", 12.5, None, TextWeight::Bold);
        let text_right = if matches {
            field.x1 - SECTION - f64::from(armed_w) - SIBLING
        } else {
            field.x1 - SIBLING
        };
        self.confirm_input.apply_layout_external(Rect::new(
            field.x0 + SIBLING,
            field.y0 + LABEL,
            text_right,
            field.y1 - LABEL,
        ));
        self.confirm_input
            .draw(scene, t, tr, theme, &PresenceCtx::empty());
        if matches {
            TextRun::new(
                "✓ armed",
                TextStyle::new(12.5).with_weight(TextWeight::Bold),
                c.semantic.success,
            )
            .draw_right_v(scene, tr, field.x1 - SECTION, field.y0, field.height());
        }
        TextRun::new(self.confirm_note(), TextStyle::new(11.0), c.text_tertiary).draw(
            scene,
            tr,
            Point::new(field.x0, field.y1 + RELATED),
            None,
        );
    }

    // ---- Write ------------------------------------------------------------

    fn draw_write_progress(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let content = lay.content;
        let (title, sub) = self.write_heading();
        Self::draw_step_title(scene, tr, theme, content, title, sub);
        let head = self.step_header_h(content);
        self.draw_write_ring(scene, tr, theme, t, content, head);
        self.draw_write_rate_eta(scene, tr, theme, content, head);
        self.draw_write_phase_bars(scene, tr, theme, t, content, head);
        self.draw_write_source_target_line(scene, tr, theme, content, head);
    }

    /// The big overall-progress ring with its percentage and phase caption
    /// stacked and centred inside it.
    fn draw_write_ring(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        head: f64,
    ) {
        let c = &theme.tokens.colors;
        let m = &self.bands.write;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let cxp = f64::midpoint(content.x0, content.x1);
        let cyp = content.y0 + m.ring_cy(head);
        let overall = p.overall();
        scene.stroke(
            &Stroke::new(RING_W),
            t,
            c.neutral[3],
            None,
            &Circle::new(Point::new(cxp, cyp), RING_R),
        );
        if overall > 0.0 {
            let mut arc = BezPath::new();
            let start = -std::f64::consts::FRAC_PI_2;
            let sweep = overall * std::f64::consts::TAU;
            let steps = (sweep.abs() / 0.08).ceil().max(1.0) as usize;
            arc.move_to((cxp + RING_R * start.cos(), cyp + RING_R * start.sin()));
            for i in 1..=steps {
                let a = start + sweep * (i as f64 / steps as f64);
                arc.line_to((cxp + RING_R * a.cos(), cyp + RING_R * a.sin()));
            }
            let ring_c = if writing {
                c.semantic.info
            } else {
                c.accent[6]
            };
            scene.stroke(&Stroke::new(RING_W), t, ring_c, None, &arc);
        }
        let stack_h = m.pct_h + LABEL + m.small_h;
        let pct_top = cyp - stack_h / 2.0;
        TextRun::new(
            &format!("{:.0}%", overall * 100.0),
            TextStyle::new(30.0).with_weight(TextWeight::Bold),
            c.text_primary,
        )
        .draw_center(scene, tr, cxp, pct_top);
        let caption = match (writing, self.action) {
            (true, Action::Install) => "installing",
            (true, Action::Flash) => "writing",
            (false, _) => "verifying",
        };
        TextRun::new(
            caption,
            TextStyle::new(12.0).with_weight(TextWeight::Medium),
            c.text_dim,
        )
        .draw_center(scene, tr, cxp, pct_top + m.pct_h + LABEL);
    }

    /// The rate + ETA line below the ring, when either is known.
    fn draw_write_rate_eta(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        head: f64,
    ) {
        let c = &theme.tokens.colors;
        let p = &self.progress;
        let cxp = f64::midpoint(content.x0, content.x1);
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
            let _ = write!(meta, "~{} left", fmt_eta(eta));
        }
        if !meta.is_empty() {
            TextRun::new(
                &meta,
                TextStyle::new(13.0).with_weight(TextWeight::Medium),
                c.text_dim,
            )
            .draw_center(scene, tr, cxp, content.y0 + self.bands.write.meta_top(head));
        }
    }

    /// The Write / Verify phase bars: a label line, its bar a `LABEL` below,
    /// the next pair a `SIBLING` after.
    fn draw_write_phase_bars(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        head: f64,
    ) {
        let c = &theme.tokens.colors;
        let m = &self.bands.write;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let bx0 = content.x0 + REGION;
        let bx1 = content.x1 - REGION;
        let mut yy = content.y0 + m.bars_top(head);
        for (label, frac, active, fill) in [
            ("Write", p.write_frac, writing, c.semantic.info),
            ("Verify", p.verify_frac, !writing, c.semantic.success),
        ] {
            TextRun::new(
                label,
                TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                if active { c.text_primary } else { c.text_dim },
            )
            .draw(scene, tr, Point::new(bx0, yy), None);
            TextRun::new(
                &format!("{:.0}%", frac * 100.0),
                TextStyle::new(12.0),
                c.text_dim,
            )
            .draw_right(scene, tr, bx1, yy);
            let bar_top = yy + m.small_h + LABEL;
            let bar = Rect::new(bx0, bar_top, bx1, bar_top + BAR_H);
            Self::bar(scene, t, bar, frac, c.neutral[3], fill);
            yy += m.bar_pitch();
        }
    }

    /// The "image → drive" caption below the phase bars.
    fn draw_write_source_target_line(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        head: f64,
    ) {
        let c = &theme.tokens.colors;
        let cxp = f64::midpoint(content.x0, content.x1);
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            TextRun::new(
                &format!("{}  →  {}", src.name, tgt.dev_path()),
                TextStyle::new(12.0),
                c.text_tertiary,
            )
            .draw_center(scene, tr, cxp, content.y0 + self.bands.write.foot_top(head));
        }
    }

    // ---- Done -------------------------------------------------------------

    fn draw_write_done(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        ok: bool,
    ) {
        let content = lay.content;
        let install = self.action == Action::Install;
        if ok {
            self.draw_write_done_ok_hero(scene, tr, theme, t, content, install);
        } else {
            let headline = if install {
                "Install failed"
            } else {
                "Write failed"
            };
            let reason = self
                .progress
                .error
                .as_deref()
                .unwrap_or("The drive may be incomplete. Review the plan before trying again.");
            self.draw_write_done_failed_hero(scene, tr, theme, t, content, (headline, reason));
        }
        self.draw_write_done_summary(scene, tr, theme, t, content, ok);
    }

    /// The success hero's headline and sub-copy, shared by the painter and
    /// the band measurement (`measure_done`).
    fn done_ok_copy(install: bool, compact: bool) -> (&'static str, &'static str) {
        match (install, compact) {
            (true, false) => (
                "Tilo is installed",
                "The disk was written and verified — remove the media and reboot.",
            ),
            (true, true) => (
                "Tilo is installed",
                "Written and verified. Remove the media and reboot.",
            ),
            (false, false) => (
                "Drive is ready",
                "The image was written and verified — you can safely remove the drive.",
            ),
            (false, true) => (
                "Drive is ready",
                "Written and verified. You can safely remove the drive.",
            ),
        }
    }

    /// The success hero: check disc + headline + sub for the completed step.
    fn draw_write_done_ok_hero(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        install: bool,
    ) {
        let c = &theme.tokens.colors;
        let m = &self.bands.done;
        let cxp = f64::midpoint(content.x0, content.x1);
        let compact = self.compact_content(content);
        Self::check_disc(
            scene,
            t,
            Point::new(cxp, DoneMetrics::disc_cy(content.y0)),
            HERO_DISC_R,
            c.semantic.success,
            Self::glyph_ink(theme, c.semantic.success),
        );
        let (headline, sub) = Self::done_ok_copy(install, compact);
        TextRun::new(
            headline,
            TextStyle::new(26.0).with_weight(TextWeight::Bold),
            c.text_primary,
        )
        .draw_center(scene, tr, cxp, DoneMetrics::headline_top(content.y0));
        TextRun::new(sub, TextStyle::new(14.0), c.text_dim).draw_para_centered(
            scene,
            tr,
            cxp,
            m.sub_top(content.y0),
            content.width(),
        );
    }

    /// The failure hero: danger disc with an "!", headline + measured detail.
    fn draw_write_done_failed_hero(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        copy: (&str, &str),
    ) {
        let c = &theme.tokens.colors;
        let m = &self.bands.done;
        let cxp = f64::midpoint(content.x0, content.x1);
        let cyp = DoneMetrics::disc_cy(content.y0);
        let compact = self.compact_content(content);
        scene.fill(
            Fill::NonZero,
            t,
            c.semantic.danger,
            None,
            &Circle::new(Point::new(cxp, cyp), HERO_DISC_R),
        );
        TextRun::new(
            "!",
            TextStyle::new(44.0).with_weight(TextWeight::Bold),
            Self::glyph_ink(theme, c.semantic.danger),
        )
        .draw_center_v(scene, tr, cxp, cyp - HERO_DISC_R, 2.0 * HERO_DISC_R);
        TextRun::new(
            copy.0,
            TextStyle::new(26.0).with_weight(TextWeight::Bold),
            c.text_primary,
        )
        .draw_center(scene, tr, cxp, DoneMetrics::headline_top(content.y0));
        let max_width =
            (content.width() - if compact { SECTION } else { 2.0 * HERO }).max(40.0) as f32;
        let line_h = tr.line(13.5);
        let sub_top = m.sub_top(content.y0);
        for (index, line) in Self::failure_detail_lines(tr, copy.1, max_width, 2)
            .iter()
            .enumerate()
        {
            TextRun::new(line, TextStyle::new(13.5), c.text_dim).draw_center(
                scene,
                tr,
                cxp,
                sub_top + index as f64 * line_h,
            );
        }
    }

    /// The Image/Drive summary card below the hero, when both are known.
    fn draw_write_done_summary(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        ok: bool,
    ) {
        let c = &theme.tokens.colors;
        let m = &self.bands.done;
        let (Some(src), Some(tgt)) = (&self.source, self.target()) else {
            return;
        };
        let compact = self.compact_content(content);
        let inset = if compact { 0.0 } else { HERO };
        let top = content.y0 + m.card_top;
        let card = Rect::new(content.x0 + inset, top, content.x1 - inset, top + m.card_h);
        surface::card(scene, t, card, theme.radius_standard, c.surfaces.raised);
        let value_x = card.x0 + m.value_x;
        let (verified_w, _) = tr.measure_weighted("✓ verified", 12.5, None, TextWeight::Bold);
        let reserve = if ok {
            f64::from(verified_w) + SIBLING
        } else {
            0.0
        };
        for (i, (key, value, reserve)) in [
            ("Image", src.name.clone(), reserve),
            ("Drive", format!("{} · {}", tgt.model, tgt.dev_path()), 0.0),
        ]
        .into_iter()
        .enumerate()
        {
            let row_top = card.y0 + SECTION + i as f64 * (m.row_h + SIBLING);
            let key_top = Self::line_top(tr, key, 12.0, TextWeight::Semibold, row_top, m.row_h);
            TextRun::new(
                key,
                TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                c.text_dim,
            )
            .draw(scene, tr, Point::new(card.x0 + SECTION, key_top), None);
            let value_top = Self::line_top(tr, &value, 13.5, TextWeight::Medium, row_top, m.row_h);
            TextRun::new(
                &value,
                TextStyle::new(13.5).with_weight(TextWeight::Medium),
                c.text_primary,
            )
            .draw(
                scene,
                tr,
                Point::new(value_x, value_top),
                Some((card.x1 - SECTION - reserve - value_x).max(0.0) as f32),
            );
            if ok && i == 0 {
                TextRun::new(
                    "✓ verified",
                    TextStyle::new(12.5).with_weight(TextWeight::Bold),
                    c.semantic.success,
                )
                .draw_right_v(scene, tr, card.x1 - SECTION, row_top, m.row_h);
            }
        }
    }
}

// =========================================================================
// Interaction / state mutation
// =========================================================================
impl ImagerApp {
    fn select_source(&mut self, state: &mut AutoCommit, src: &Source) {
        let previous = self
            .source
            .as_ref()
            .filter(|previous| !previous.path.is_empty())
            .cloned()
            .map(Box::new);
        // A source outside the authenticated catalog cannot carry a profile.
        if !self.source_is_official(src) {
            self.action = Action::Flash;
        }
        let action = crate::actions::ImagerAction::SelectSource {
            source: Box::new(src.clone()),
            previous,
            recent_rank: ImagerDoc::next_recent_rank(state),
        };
        self.actions.dispatch_with_context(
            state,
            crate::actions::IMAGER_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.imager", "Imager", &action),
        );
        self.source = Some(src.clone());
        self.set_status(
            state,
            &format!(
                "Selected {} — {}",
                src.name,
                if self.source_is_official(src) {
                    "official release verified"
                } else {
                    "local image, written as-is"
                }
            ),
        );
        self.dirty = true;
    }

    fn open_file_picker(&mut self, state: &AutoCommit) {
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
                Some(src) => self.select_source(state, &src),
                None => self.set_status(state, "That file is not a readable disk image."),
            },
            PortalOutcome::Cancelled => self.set_status(state, "File selection cancelled."),
            PortalOutcome::Failed(reason) => {
                self.set_status(state, &format!("File picker unavailable: {reason}"));
            }
        }
        self.dirty = true;
    }

    fn download_release(&mut self, state: &AutoCommit, index: usize, release: TiloRelease) {
        if self.release_rx.is_some() {
            self.set_status(state, "A Tilo installer download is already in progress.");
            return;
        }
        self.set_status(
            state,
            &format!("Downloading and verifying {}…", release.display_name()),
        );
        let job = crate::release::spawn_download(release);
        self.release_rx = Some(job.events);
        self.release_control = Some(job.control);
        self.release_download_index = Some(index);
        self.release_download_progress = None;
        self.release_download_stage = Some("preparing");
        self.dirty = true;
    }

    fn drain_release(&mut self, state: &mut AutoCommit) {
        let Some(rx) = &self.release_rx else { return };
        let mut events = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(event) => events.push(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if !events
                        .iter()
                        .any(|event| matches!(event, crate::release::DownloadEvent::Finished(_)))
                    {
                        events.push(crate::release::DownloadEvent::Finished(Err(
                            "The Tilo installer download stopped unexpectedly.".into(),
                        )));
                    }
                    break;
                }
            }
        }
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                crate::release::DownloadEvent::Downloading { received, total } => {
                    self.release_download_progress = Some((received, total));
                    self.release_download_stage = Some("downloading");
                    let percent = received.saturating_mul(100).checked_div(total).unwrap_or(0);
                    self.set_status(state, &format!("Downloading Tilo installer… {percent}%"));
                }
                crate::release::DownloadEvent::Verifying => {
                    self.release_download_progress = None;
                    self.release_download_stage = Some("verifying");
                    self.set_status(state, "Cryptographically verifying the Tilo installer…");
                }
                crate::release::DownloadEvent::Expanding { written, total } => {
                    self.release_download_progress = Some((written, total));
                    self.release_download_stage = Some("preparing image");
                    self.set_status(state, "Preparing the verified Tilo installer image…");
                }
                crate::release::DownloadEvent::Finished(result) => {
                    self.release_rx = None;
                    self.release_control = None;
                    self.release_download_index = None;
                    self.release_download_progress = None;
                    self.release_download_stage = None;
                    match result {
                        Ok(source) => {
                            if let Some(release) = self
                                .releases
                                .iter_mut()
                                .find(|release| release.matches_source(&source))
                            {
                                release.local_path.clone_from(&source.path);
                            }
                            self.select_source(state, &source);
                            self.set_status(
                                state,
                                "Tilo installer downloaded and cryptographically verified.",
                            );
                        }
                        Err(error) => self.set_status(state, &error),
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn select_target(&mut self, state: &AutoCommit, idx: usize) {
        let Some(drive) = self.targets.get(idx) else {
            return;
        };
        // Hard guard repeated below the visible-list filter: stale persisted or
        // collaborative state cannot turn the media creator into an internal-
        // disk raw writer.
        if !drive.is_media_target() {
            self.set_status(
                state,
                "Internal and system disks can only be installed from the booted Tilo medium.",
            );
            self.dirty = true;
            return;
        }
        self.selected = Some(idx);
        self.confirm_input.set_text("");
        self.dirty = true;
    }

    fn set_status(&mut self, _state: &AutoCommit, s: &str) {
        self.status = s.to_string();
    }

    fn goto(&mut self, _state: &mut AutoCommit, s: Step) {
        self.step = s;
        self.scroll_y = 0.0;
        self.keyboard_hit = None;
        self.confirm_input.set_focus(false);
        self.customize.clear_focus();
        self.write_setup.clear_focus();
        self.dirty = true;
    }

    fn advance(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Source if self.can_advance(Step::Source) => self.goto(state, Step::Drive),
            Step::Drive if self.can_advance(Step::Drive) => {
                self.action = Action::Flash;
                if self.customizable() {
                    self.goto(state, Step::Customize);
                } else {
                    self.goto(state, Step::Confirm);
                }
            }
            Step::Customize => match self.profile.validate_media_profile() {
                Ok(()) => self.goto(state, Step::Confirm),
                Err(message) => self.set_status(state, &message),
            },
            // Confirm advances into the write only when the typed-name gate is
            // armed; that same gesture kicks off the (external) job.
            Step::Confirm if self.can_advance(Step::Confirm) => self.start_write(state),
            _ => {}
        }
    }

    fn back(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Confirm if self.customizable() => self.goto(state, Step::Customize),
            Step::Confirm | Step::Customize => self.goto(state, Step::Drive),
            Step::Drive => self.goto(state, Step::Source),
            // No going back from the source, an in-flight write, or the outcome.
            Step::Source | Step::Write | Step::Done => {}
        }
    }

    fn start_write(&mut self, state: &mut AutoCommit) {
        let operation = crate::actions::ImagerOperation::Write {
            device: self.target().map(TargetDrive::dev_path).unwrap_or_default(),
            confirmation: self.confirm_input.value().to_string(),
        };
        if let Err(reason) = self.perform_operation(&operation, state) {
            self.set_status(state, &reason);
            self.dirty = true;
        }
    }

    /// One semantic implementation shared by the direct Confirm control and
    /// the resource-operation route.
    #[allow(
        clippy::too_many_lines,
        reason = "write, cancellation, status, and export share one operation-dispatch boundary"
    )]
    fn perform_operation(
        &mut self,
        operation: &crate::actions::ImagerOperation,
        state: &mut AutoCommit,
    ) -> Result<(), String> {
        if matches!(operation, crate::actions::ImagerOperation::Write { .. })
            && !self.startup_write_state_reconciled
        {
            return Err("Restoring the previous image-write state; wait for drive review before starting another write".into());
        }
        if matches!(operation, crate::actions::ImagerOperation::Cancel) {
            let Some(control) = self.job_control.clone() else {
                return Err("No image write is currently running".into());
            };
            control.cancel();
            self.set_status(
                state,
                "Stopping safely… Keep the drive connected until cancellation finishes.",
            );
            self.dirty = true;
            return Ok(());
        }
        let crate::actions::ImagerOperation::Write {
            device,
            confirmation,
        } = operation
        else {
            return match operation {
                crate::actions::ImagerOperation::Verify => Err(
                    "Read-back verification runs automatically after every successful write".into(),
                ),
                crate::actions::ImagerOperation::Cancel => {
                    Err("No image write is currently running".into())
                }
                crate::actions::ImagerOperation::Status => Ok(()),
                crate::actions::ImagerOperation::ExportConfig => {
                    if self.source.is_none() || self.target().is_none() {
                        Err("Select an image source and target drive first".into())
                    } else {
                        self.export_config(state);
                        Ok(())
                    }
                }
                crate::actions::ImagerOperation::SetWifiPsk { value } => {
                    self.wifi_psk.clone_from(value);
                    Ok(())
                }
                crate::actions::ImagerOperation::Write { .. } => {
                    Err("Invalid image-write operation dispatch".into())
                }
            };
        };
        let (Some(mut src), Some(tgt)) = (self.source.clone(), self.target().cloned()) else {
            return Err("Select a source and drive first.".into());
        };
        // Treat the collaborative source record as a claim, never as trust.
        // Only an exact bundled-catalog match may transport a setup profile.
        src.installable = self.source_is_official(&src);
        if !src.installable {
            src.kind = SourceKind::IsoFile;
            src.expected_sha256 = None;
        }
        if src.installable {
            self.profile.validate_media_profile()?;
        }
        if *device != tgt.dev_path() {
            return Err("The selected target changed; review the write plan again.".into());
        }
        // Stage Wi-Fi / enrollment for any image the operator configured on
        // Confirm. Official media still embeds only the secret-free profile
        // block; the passphrase and new device key stay in the staged tree
        // and are copied onto the installed root after verification.
        let handoff_stage =
            if !self.profile.wifi_ssid.trim().is_empty() || self.profile.enroll_local_identity {
                let local = crate::handoff::local_identity_from_env()?;
                crate::handoff::stage_setup_handoff(&self.profile, &self.wifi_psk, &local)?
                    .map(|stage| stage.root)
            } else {
                None
            };
        let req = WriteRequest {
            source: src,
            target: tgt.clone(),
            action: self.action,
            profile: self.profile.clone(),
            handoff_stage: handoff_stage.clone(),
        };
        let (start, rx, control) = spawn_job(req, confirmation);
        let refuse = |message: String| -> Result<(), String> {
            if let Some(stage) = handoff_stage.as_ref() {
                let _ = std::fs::remove_dir_all(stage);
            }
            Err(message)
        };
        match start {
            crate::write::JobStart::Started => {
                self.job_rx = rx;
                self.job_control = control;
                self.progress = Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.0,
                    verify_frac: 0.0,
                    rate_bps: 0,
                    error: None,
                };
                self.set_status(
                    state,
                    &format!(
                        "{} → {} (writing)",
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
                return refuse("Confirmation does not match — write refused.".into());
            }
            crate::write::JobStart::TooSmall => {
                return refuse("Image is larger than the drive — write refused.".into());
            }
            crate::write::JobStart::UnsafeTarget => {
                return refuse(
                    "Internal and system disks can only be installed from the booted Tilo medium."
                        .into(),
                );
            }
            crate::write::JobStart::Incomplete => {
                return refuse("Select a source and drive before writing.".into());
            }
            crate::write::JobStart::InvalidProfile(message) => return refuse(message),
            crate::write::JobStart::WriteDisabled => {
                return refuse(
                    "Raw-device writing is disabled; enable the privileged Imager backend first."
                        .into(),
                );
            }
        }
        self.dirty = true;
        Ok(())
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
                        self.progress.error = Some(
                            "The writer stopped unexpectedly. Reconnect the drive, then review and retry."
                                .into(),
                        );
                        self.set_status(
                            state,
                            "The writer stopped unexpectedly. Reconnect the drive, then review and retry.",
                        );
                        self.goto(state, Step::Done);
                    }
                    self.job_rx = None;
                    self.job_control = None;
                    self.dirty = true;
                    return;
                }
            }
        }
        if let Some(mut p) = latest {
            // Cancellation may race with a terminal helper event already in
            // the pipe. Honour the user's stop request conservatively: never
            // turn that race into a claim that the medium is ready to use.
            if p.phase == JobPhase::Success
                && self
                    .job_control
                    .as_ref()
                    .is_some_and(JobControl::is_cancelled)
            {
                p.phase = JobPhase::Failed;
                p.error = Some(
                    "Image writing was cancelled. The drive may be incomplete; review and retry before using it."
                        .into(),
                );
            }
            let terminal_phase = p.phase;
            let terminal_error = p.error.clone();
            self.progress = p;
            self.dirty = true;
            if matches!(terminal_phase, JobPhase::Success | JobPhase::Failed) {
                self.job_rx = None;
                self.job_control = None;
                let name = self
                    .source
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let msg = if terminal_phase == JobPhase::Success {
                    format!("Done — {name} {} and verified", self.action.past())
                } else {
                    terminal_error.unwrap_or_else(|| {
                        format!(
                            "{} failed. Reconnect the drive, then review and retry.",
                            self.action.title()
                        )
                    })
                };
                self.set_status(state, &msg);
                // Terminal phase → show the outcome station.
                self.goto(state, Step::Done);
            }
        }
    }

    fn write_another(&mut self, state: &mut AutoCommit) {
        if self.progress.phase == JobPhase::Failed {
            // Preserve the reviewed source, target, and customized setup. A
            // retry still requires a fresh peer-local destructive confirmation
            // and passes every target/source validation again in the helper.
            self.progress = Progress::default();
            self.confirm_input.set_text("");
            self.goto(state, Step::Confirm);
            self.set_status(
                state,
                "Review the image and drive, then confirm the target again to retry.",
            );
            return;
        }
        self.progress = Progress::default();
        self.confirm_input.set_text("");
        self.goto(state, Step::Source);
        self.set_status(state, "");
    }

    fn export_config(&mut self, _state: &mut AutoCommit) {
        let text = self
            .profile
            .to_headless_config(self.source.as_ref(), self.target());
        self.export_dialog.show(text);
        self.dirty = true;
    }

    pub(crate) fn seed_export_for_shot(&mut self) {
        self.export_after_sync = true;
    }

    /// Replace the last probe snapshot without carrying destructive consent
    /// across an unplug/replug substitution. Device names such as `disk4`,
    /// `sdb`, and `PhysicalDrive2` are reusable slots, not hardware identity.
    fn apply_drive_inventory(&mut self, targets: Vec<TargetDrive>) {
        let remembered = self.target().cloned();
        self.targets = targets;
        self.selected = remembered.as_ref().and_then(|expected| {
            self.targets
                .iter()
                .position(|actual| Self::same_rescanned_target(expected, actual))
        });
        if self.selected.is_none() {
            self.confirm_input.set_text("");
        }
    }

    fn same_rescanned_target(expected: &TargetDrive, actual: &TargetDrive) -> bool {
        // Slot names, model strings and capacities are not physical identity.
        // Two identical sticks can reuse `sdb`, `disk4`, or `PhysicalDrive2`;
        // without a stable ID, require a fresh choice and destructive
        // confirmation after every rescan.
        let stable_identity_matches = match (&expected.stable_id, &actual.stable_id) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => false,
        };
        stable_identity_matches
            && expected.name == actual.name
            && expected.model == actual.model
            && expected.size == actual.size
            && expected.removable == actual.removable
            && expected.is_system == actual.is_system
    }

    fn do_rescan(&mut self, state: &AutoCommit) -> bool {
        if live_probe_enabled() {
            // Re-read the live block-device inventory. `probe()` returns `None`
            // only when the platform probe is unavailable or malformed, so in
            // that case we keep the last-known list rather than blanking it.
            // A real (possibly empty) result is authoritative and replaces it,
            // so unplugging the selected drive genuinely drops it from the list.
            let Some(t) = crate::probe::probe() else {
                self.set_status(
                    state,
                    "Drive rescan failed. Check the system disk tools and try again.",
                );
                self.dirty = true;
                return false;
            };
            self.apply_drive_inventory(t);
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
        self.dirty = true;
        true
    }

    fn dispatch_hit(&mut self, state: &mut AutoCommit, hit: Hit) {
        match hit {
            Hit::PickFile => self.open_file_picker(state),
            Hit::Release(i) => {
                if let Some(release) = self.releases.get(i).cloned() {
                    if release.url.is_empty() {
                        self.select_source(state, &release.to_source());
                    } else {
                        self.download_release(state, i, release);
                    }
                }
            }
            Hit::CancelDownload => {
                if let Some(control) = self.release_control.clone() {
                    control.cancel();
                    self.set_status(
                        state,
                        "Cancelling safely… The verified partial download will be kept for resume.",
                    );
                    self.dirty = true;
                }
            }
            Hit::Recent(i) => {
                if let Some(src) = self.recent.get(i).cloned() {
                    self.select_source(state, &src);
                }
            }
            Hit::Target(i) => self.select_target(state, i),
            Hit::Rescan => {
                self.do_rescan(state);
            }
            Hit::ConfirmField => {
                self.confirm_input.set_focus(true);
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
            Hit::CancelWrite => {
                let _ = self.perform_operation(&crate::actions::ImagerOperation::Cancel, state);
            }
            Hit::WriteAnother => self.write_another(state),
        }
    }

    /// Jobs are supervised only by this process. A restart cannot make a
    /// remote/persisted progress claim look trustworthy, so every fresh window
    /// begins with an idle local job surface and requires a new review.
    fn reconcile_persisted_write_state(&mut self, _state: &mut AutoCommit) {
        self.startup_write_state_reconciled = true;
    }
}

// =========================================================================
// Widget impl
// =========================================================================
impl Widget for ImagerApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::IMAGER_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        if !self.seeded {
            if ImagerDoc::is_empty(state) {
                // Start genuinely blank. In the live app a real `lsblk` probe
                // fills the drive list; if it finds nothing (or is unavailable)
                // the list stays empty and the Drive step shows its honest
                // "insert a drive" state — the imager never invents drives.
                // Sample content is opt-in (`--demo` / `--shots`), which seeds
                // the document *before* the first sync so this branch is skipped.
                let action = crate::actions::ImagerAction::EnsureRecords;
                self.actions.dispatch_with_context(
                    state,
                    crate::actions::IMAGER_ACTION_TYPE,
                    &action,
                    tilo_actions::ActionContext::for_action("so.tilo.imager", "Imager", &action),
                );
            }
            if live_probe_enabled() {
                self.targets = crate::probe::probe().unwrap_or_default();
            }
            if live_probe_enabled() {
                self.reconcile_persisted_write_state(state);
            } else {
                // Demo/tests/shots intentionally pose worker-less progress
                // frames. They still complete startup so operations cannot be
                // held unavailable forever outside the live binary.
                self.startup_write_state_reconciled = true;
            }
            self.seeded = true;
        }

        if live_probe_enabled() && self.releases.is_empty() {
            match crate::release::bundled_releases() {
                Ok(releases) => self.releases = releases,
                Err(error) => self.status = error,
            }
        }

        // Fold in any async work before reading the doc back.
        self.drain_portal(state);
        self.drain_release(state);
        self.drain_job(state);

        self.source = ImagerDoc::source(state);
        self.recent = ImagerDoc::recent(state);
        self.profile = ImagerDoc::profile(state);
        self.action = Action::Flash;
        self.selected = self.selected.filter(|i| {
            self.targets
                .get(*i)
                .is_some_and(TargetDrive::is_media_target)
        });

        // Refresh the shared customization panel's fields from the profile
        // (each unfocused field mirrors the stored value; a focused one stays
        // the source of truth).
        self.customize.sync(&self.profile);
        self.write_setup.sync(&self.profile, &self.wifi_psk);
        if self.export_after_sync {
            self.export_after_sync = false;
            self.export_config(state);
        }
        self.dirty = true;
    }

    /// Measure the step heading group with the renderer that will paint it,
    /// so the first content block starts below the real (scaled, wrapped)
    /// title and description instead of a fixed 44px band.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        tr: &mut TextRenderer,
    ) -> bool {
        // Prefer the allocation being solved; before the first solve (or on a
        // probe without one) fall back to the last applied rect rather than
        // wrapping the description at zero width.
        let width = taffy
            .layout(node)
            .ok()
            .map(|layout| f64::from(layout.size.width))
            .filter(|w| *w > 0.0)
            .unwrap_or(self.rect.width());
        let content_w = Self::content_width_for(width);
        let measured = self.measure_step_heading(tr, width);
        let heading = measured.map(|(_, group)| group);
        let mut changed = match (self.step_heading_h, heading) {
            (None, None) => false,
            (Some(prev), Some(next)) => (prev - next).abs() > 0.5,
            _ => true,
        };
        self.step_heading_h = heading;
        if let Some((title_h, _)) = measured {
            changed |= (self.title_h - title_h).abs() > 0.5;
            self.title_h = title_h;
        }
        // Every step's bands, from the renderer that will paint them: one
        // comparison, so no band can be left out of the change detection.
        let text_scale = f64::from(tr.text_scale());
        let bands = self.measure_bands(tr, content_w, text_scale);
        changed |= bands != self.bands;
        self.bands = bands;
        // The customization panel hangs off the measured heading and measures
        // its own caption line and Preview button.
        let probe = Rect::new(0.0, 0.0, content_w, 0.0);
        let head = self.step_header_h(probe);
        changed |= self.customize.measure(tr, head);
        changed |= self.write_setup.measure(tr);
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The heading-to-content gap is the theme's `s3`; cache it here for
        // the renderer hook, which measures the heading without a `Theme`.
        self.heading_gap = f64::from(theme.tokens.spacing.s3);
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see
        // `ui::widget::TaffyTreeExt`'s doc comment), and `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so there is
        // nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
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
        // A missing layout entry for our own node would be a taffy bug, not a
        // recoverable app condition — skip this frame's rect update rather
        // than panic; the next frame's layout pass will have one.
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.scroll_y = self.scroll_y.min(self.max_scroll());
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
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &lay.viewport_content,
        );
        match self.step {
            Step::Source => self.draw_source_step(scene, tr, theme, transform, &lay),
            Step::Drive => self.draw_drive_step(scene, tr, theme, transform, &lay),
            Step::Customize => {
                self.draw_customize_step(scene, tr, theme, transform, &lay, presence);
            }
            Step::Confirm => self.draw_confirm_step(scene, tr, theme, transform, &lay, presence),
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
        scene.pop_layer();
        self.draw_scrollbar(scene, theme, transform, &lay);
        self.draw_footer(scene, tr, theme, transform, &lay);
        if let Some(focused) = self.keyboard_hit
            && let Some((rect, _)) = lay.hits.iter().find(|(_, hit)| *hit == focused)
        {
            Self::stroke_rr(
                scene,
                transform,
                rect.inflate(2.0, 2.0),
                theme.radius_standard + 2.0,
                theme.tokens.colors.border_focus,
                2.0,
            );
        }

        if self.export_dialog.is_open() {
            self.export_dialog.layout_in(self.rect, theme, tr);
            self.export_dialog
                .draw_detached(scene, transform, tr, theme, presence);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the canvas wizard routes modal, text-field, profile-panel, pointer, scroll and keyboard capture in one explicitly ordered event transaction"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use winit::event::{ElementState, KeyEvent, MouseButton};

        let lay = self.compute_layout();

        // The detached shared modal owns capture and dismissal before any
        // wizard field or shortcut sees the event.
        if self.export_dialog.handle_detached(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            ctx.state,
            ctx.path,
        ) {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        if self.handle_tab_event(event, *ctx.modifiers, &lay) {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        // Destructive consent is peer-local and goes through the toolkit's
        // real editor before any wizard shortcut sees the event. On-write
        // Wi-Fi / enrollment fields sit above it and take events first.
        if self.step == Step::Confirm && !self.export_dialog.is_open() {
            if self.handle_write_setup_event(event, ctx, &lay) {
                *ctx.handled = true;
                self.dirty = true;
                return;
            }
            let mut input_handled = false;
            let mut focus_target = None;
            self.confirm_input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut input_handled,
                    focus_target: &mut focus_target,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if focus_target.is_some() {
                self.confirm_input.set_focus(true);
                self.write_setup.clear_focus();
            }
            if self.confirm_input.take_dirty().is_some() {
                self.dirty = true;
            }
            if self.confirm_input.take_submit() {
                if self.can_advance(Step::Confirm) {
                    self.start_write(ctx.state);
                }
                self.confirm_input.set_focus(false);
                *ctx.handled = true;
                self.dirty = true;
                return;
            }
            if input_handled {
                *ctx.handled = true;
                self.dirty = true;
                return;
            }
        }

        // The Customize step is delegated in full to the shared customization
        // panel, which owns its fields, toggles and export affordance. Only
        // events it does not consume (footer clicks, global keys) fall through
        // to the wizard chrome below.
        let consumed = self.handle_customize_event(event, ctx, &lay);
        if consumed {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y, ctx.scale_factor, &lay);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(ctx.state, &lay, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. }
                if lay.viewport_content.contains(self.last_mouse) =>
            {
                use winit::event::MouseScrollDelta;
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 52.0,
                    MouseScrollDelta::PixelDelta(point) => point.y / ctx.scale_factor,
                };
                let next = (self.scroll_y - dy).clamp(0.0, self.max_scroll());
                if (next - self.scroll_y).abs() > f64::EPSILON {
                    self.scroll_y = next;
                    self.dirty = true;
                }
                *ctx.handled = true;
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
                self.handle_key_pressed(logical_key, ctx.state, ctx.handled);
            }
            _ => {}
        }
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
        Ok(self
            .answer_resource_operation_v2(
                request.kind,
                request.type_id,
                request.payload,
                doc,
                path,
            )?
            .map(ui::widget::ResourceOperationStart::Completed))
    }

    fn name(&self) -> &'static str {
        "ImagerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.export_dialog
            .accessibility()
            .or_else(|| Some(self.accessibility_tree()))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this is the single assistive-action router for canvas hits, editable profile fields, and peer-local destructive confirmation"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action as AccessAction;
        let (state, _) = ui::doc::document(state, path);

        if self.export_dialog.is_open() {
            return false;
        }
        if let Some(field_key) = target.strip_prefix("imager:write-setup:") {
            if self.step != Step::Confirm || !self.offers_write_setup() {
                return false;
            }
            return match (field_key, action) {
                ("enroll-identity", AccessAction::Activate | AccessAction::Focus) => {
                    let on = !self.profile.enroll_local_identity;
                    let field_action = crate::actions::ImagerAction::SetEnrollLocalIdentity { on };
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.profile = ImagerDoc::profile(state);
                    true
                }
                ("wifi-ssid", AccessAction::SetValue(value)) => {
                    let field_action = crate::actions::ImagerAction::SetWifiSsid {
                        value: value.clone(),
                    };
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.profile = ImagerDoc::profile(state);
                    true
                }
                ("device-label", AccessAction::SetValue(value)) => {
                    let field_action = crate::actions::ImagerAction::SetDeviceLabel {
                        value: value.clone(),
                    };
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.profile = ImagerDoc::profile(state);
                    true
                }
                ("wifi-psk", AccessAction::SetValue(value)) => {
                    self.wifi_psk.clone_from(value);
                    true
                }
                _ => false,
            };
        }
        if let Some(field_key) = target.strip_prefix("imager:profile:") {
            if self.step != Step::Customize || !self.customizable() {
                return false;
            }
            if field_key == "preview" {
                return match action {
                    AccessAction::Focus => {
                        self.focus_accessible_profile_slot(7);
                        true
                    }
                    AccessAction::Activate => {
                        self.export_config(state);
                        true
                    }
                    _ => false,
                };
            }
            let Some(field) = CzField::from_key(field_key) else {
                return false;
            };
            let Some(slot) = [
                CzField::Hostname,
                CzField::FullName,
                CzField::Username,
                CzField::Locale,
                CzField::Timezone,
                CzField::Keyboard,
                CzField::Filesystem,
            ]
            .iter()
            .position(|candidate| *candidate == field) else {
                return false;
            };
            return match action {
                AccessAction::Focus => {
                    self.focus_accessible_profile_slot(slot);
                    true
                }
                AccessAction::SetValue(value) => {
                    let Some(value) = self.customize.canonical_value(field, value) else {
                        self.set_status(state, &format!("Choose a listed {}", field.label()));
                        return true;
                    };
                    let field_action = CustomizePanel::field_action(field, value);
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.focus_accessible_profile_slot(slot);
                    true
                }
                _ => false,
            };
        }

        let identity_hit = target
            .strip_prefix("imager:release:")
            .and_then(|identity| {
                self.releases
                    .iter()
                    .position(|release| release.selection_id() == identity)
                    .map(Hit::Release)
            })
            .or_else(|| {
                target.strip_prefix("imager:recent:").and_then(|path| {
                    self.recent
                        .iter()
                        .position(|source| source.path == path)
                        .map(Hit::Recent)
                })
            })
            .or_else(|| {
                target.strip_prefix("imager:drive:").and_then(|name| {
                    self.targets
                        .iter()
                        .position(|drive| drive.name == name)
                        .map(Hit::Target)
                })
            });
        let Some(hit) = identity_hit.or_else(|| Self::hit_from_target(target)) else {
            return false;
        };
        if !self
            .compute_layout()
            .hits
            .iter()
            .any(|(_, candidate)| *candidate == hit)
        {
            return false;
        }
        if !self.a11y_hit_enabled(hit) {
            return false;
        }
        match action {
            AccessAction::Focus => {
                let lay = self.compute_layout();
                self.customize.clear_focus();
                self.keyboard_hit = Some(hit);
                self.confirm_input.set_focus(hit == Hit::ConfirmField);
                if let Some((rect, _)) = lay.hits.iter().find(|(_, candidate)| *candidate == hit) {
                    self.ensure_visible(*rect, lay.viewport_content);
                }
                self.dirty = true;
                true
            }
            AccessAction::SetValue(value) if hit == Hit::ConfirmField => {
                self.confirm_input.set_text(value);
                self.confirm_input.set_focus(true);
                self.keyboard_hit = Some(hit);
                self.dirty = true;
                true
            }
            AccessAction::Activate => {
                self.dispatch_hit(state, hit);
                self.dirty = true;
                true
            }
            _ => false,
        }
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
        self.job_rx.is_some() || self.portal_rx.is_some() || self.release_rx.is_some()
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

impl ui::resources::PublishesResources for ImagerApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the profile / write-plan door — keep only live extras (none today:
    /// job status is a View on the resource, not a second published tree).
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == "imager.sources" {
            return None;
        }
        fn find(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.publish_resources(doc), id)
    }
}

impl ui::undo::CommittedActions for ImagerApp {
    /// The framework host drains the undo boundary an on-screen profile control
    /// dispatched, so it records exactly one boundary bracketing it — the same
    /// path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ImagerApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        let expected_kind = match operation {
            crate::actions::ImagerOperation::Status
            | crate::actions::ImagerOperation::ExportConfig => tilo_actions::OperationKind::View,
            crate::actions::ImagerOperation::SetWifiPsk { .. } => {
                tilo_actions::OperationKind::Effect
            }
            _ => tilo_actions::OperationKind::Effect,
        };
        if kind != expected_kind {
            return Err("Imager operation kind does not match its contract".into());
        }
        let mut local_doc = doc.clone();
        self.perform_operation(&operation, &mut local_doc)?;
        Ok(true)
    }

    fn answer_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        match crate::actions::decode_operation(type_id, payload)? {
            Some(crate::actions::ImagerOperation::Status) => {
                if kind != tilo_actions::OperationKind::View {
                    return Err("Imager status is a view operation".into());
                }
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "phase": self.progress.phase.key(),
                        "progress": self.progress.overall(),
                        "message": self.status,
                    })),
                    resources: Vec::new(),
                }));
            }
            Some(crate::actions::ImagerOperation::ExportConfig) => {
                if kind != tilo_actions::OperationKind::View {
                    return Err("Imager config export is a view operation".into());
                }
                if self.source.is_none() || self.target().is_none() {
                    return Err("Select an image source and target drive first".into());
                }
                let text = self
                    .profile
                    .to_headless_config(self.source.as_ref(), self.target());
                self.export_dialog.show(text.clone());
                self.dirty = true;
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::Value::String(text)),
                    resources: Vec::new(),
                }));
            }
            _ => {}
        }
        self.answer_resource_operation(kind, type_id, payload, doc, path)
            .map(|handled| handled.then(tilo_actions::ActionOutput::default))
    }
}

impl ImagerApp {
    /// The Customize step's delegated handling. Returns `true` when the panel
    /// consumed the event, so the caller stops before the wizard chrome below
    /// ever sees it.
    fn handle_customize_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        lay: &Layout,
    ) -> bool {
        if !(self.step == Step::Customize && self.customizable() && !self.export_dialog.is_open()) {
            return false;
        }
        let removable = self.target().is_some_and(|d| d.removable);
        let profile = self.profile.clone();
        let resp = self.customize.handle_event(
            event,
            ctx,
            &mut self.actions,
            &profile,
            removable,
            lay.content,
        );
        if resp.export_requested {
            self.export_config(ctx.state);
        }
        resp.handled
    }

    /// Confirm-step Wi-Fi / enrollment form for the selected image.
    fn handle_write_setup_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        lay: &Layout,
    ) -> bool {
        if !self.offers_write_setup() {
            return false;
        }
        let profile = self.profile.clone();
        let top = self.write_setup_top(lay.content);
        let resp = self.write_setup.handle_event(
            event,
            ctx,
            &mut self.actions,
            &profile,
            lay.content,
            top,
        );
        if let Some(psk) = resp.wifi_psk {
            self.wifi_psk = psk;
        }
        if resp.handled {
            self.confirm_input.set_focus(false);
            self.profile = ImagerDoc::profile(ctx.state);
        }
        resp.handled
    }

    /// Track the pointer and which drive card (if any) is under it.
    fn handle_cursor_moved(&mut self, px: f64, py: f64, scale_factor: f64, lay: &Layout) {
        let p = Point::new(px / scale_factor, py / scale_factor);
        self.last_mouse = p;
        let hover_now = lay.hits.iter().find_map(|(r, h)| match h {
            Hit::Target(i) if r.contains(p) => Some(*i),
            _ => None,
        });
        self.hover_target = hover_now;
        // Repaint on move so buttons/cards light up under the pointer.
        self.dirty = true;
    }

    /// A left click in the content area dispatches a hit-tested affordance or,
    /// on empty space, drops the confirm field's focus.
    fn handle_left_click(&mut self, state: &mut AutoCommit, lay: &Layout, handled: &mut bool) {
        self.keyboard_hit = None;
        let p = self.last_mouse;
        let hit = lay
            .hits
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, h)| *h);
        if let Some(hit) = hit {
            self.dispatch_hit(state, hit);
            *handled = true;
            self.dirty = true;
            return;
        }
        self.confirm_input.set_focus(false);
    }

    /// Keyboard handling for the Confirm step's typed device-name field and
    /// global shortcuts. The export dialog captures input before this path.
    fn handle_key_pressed(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        use winit::keyboard::{Key, NamedKey};

        if matches!(logical_key, Key::Named(NamedKey::Enter))
            && let Some(hit) = self.keyboard_hit
        {
            self.dispatch_hit(state, hit);
            *handled = true;
            self.dirty = true;
            return;
        }

        // The Confirm step's typed device-name field (a transient arming
        // gesture, not a profile edit).
        if self.confirm_input.is_focused() && self.step == Step::Confirm {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.confirm_input.set_focus(false);
                *handled = true;
                self.dirty = true;
            }
            return;
        }
        self.handle_global_shortcut_key(logical_key, state, handled);
    }

    fn ensure_visible(&mut self, rect: Rect, viewport: Rect) {
        let padding = 10.0;
        if rect.y1 > viewport.y1 - padding {
            self.scroll_y += rect.y1 - (viewport.y1 - padding);
        } else if rect.y0 < viewport.y0 + padding {
            self.scroll_y -= viewport.y0 + padding - rect.y0;
        }
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    fn handle_tab_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        lay: &Layout,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        self.handle_tab_navigation(modifiers.state().shift_key(), lay);
        true
    }

    fn handle_tab_navigation(&mut self, reverse: bool, lay: &Layout) {
        if self.step == Step::Customize && self.customizable() {
            const CUSTOM_SLOTS: usize = 7;
            let back_slot = CUSTOM_SLOTS;
            let primary_slot = CUSTOM_SLOTS + 1;
            let current = self.customize.keyboard_slot().or(match self.keyboard_hit {
                Some(Hit::Back) => Some(back_slot),
                Some(Hit::Next) => Some(primary_slot),
                _ => None,
            });
            let count = CUSTOM_SLOTS + 2;
            let next = match (current, reverse) {
                (Some(0) | None, true) => count - 1,
                (Some(index), true) => index - 1,
                (Some(index), false) => (index + 1) % count,
                (None, false) => 0,
            };
            self.customize.clear_focus();
            self.keyboard_hit = None;
            if next < CUSTOM_SLOTS {
                self.customize.focus_keyboard_slot(next);
                if let Some(rect) = self.customize.keyboard_slot_rect(lay.content, next) {
                    self.ensure_visible(rect, lay.viewport_content);
                }
            } else {
                self.keyboard_hit = Some(if next == back_slot {
                    Hit::Back
                } else {
                    Hit::Next
                });
            }
            return;
        }

        let focusable = lay.hits.iter().map(|(_, hit)| *hit).collect::<Vec<_>>();
        if focusable.is_empty() {
            return;
        }
        let current = self
            .keyboard_hit
            .and_then(|selected| focusable.iter().position(|hit| *hit == selected));
        let next = match (current, reverse) {
            (Some(0) | None, true) => focusable.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % focusable.len(),
            (None, false) => 0,
        };
        self.customize.clear_focus();
        self.confirm_input.set_focus(false);
        self.keyboard_hit = focusable.get(next).copied();
        if self.keyboard_hit == Some(Hit::ConfirmField) {
            self.confirm_input.set_focus(true);
        }
        if let Some((rect, _)) = lay.hits.get(next) {
            self.ensure_visible(*rect, lay.viewport_content);
        }
    }

    fn handle_global_shortcut_key(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        use winit::keyboard::{Key, NamedKey};

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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_app() -> (ImagerApp, AutoCommit) {
        // Tests drive the wizard against the demo document (sample drives + a
        // Tilo image + recent history) — the same content `--shots`/`--demo` use.
        let mut app = ImagerApp::new();
        app.targets = crate::model::sample_targets();
        app.releases = crate::model::sample_releases();
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    /// Blank start, demo seeding, local state and restart reconciliation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn startup_state_is_blank_local_and_reconciled_before_writes() {
        // Phase `local_wizard_state_never_leaks_through_the_collaborative_document`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = doc.save();
            app.selected = Some(0);
            app.goto(&mut doc, Step::Confirm);
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.5,
                verify_frac: 0.0,
                rate_bps: 1,
                error: None,
            };
            app.set_status(&doc, "local writer status");
            assert_eq!(
                doc.save(),
                before,
                "selection, station, status, and job progress are local"
            );
            let mut peer = AutoCommit::load(&before).unwrap();
            let mut peer_app = ImagerApp::new();
            peer_app.sync(&mut peer, &automerge::ROOT);
            assert_eq!(peer_app.step, Step::Source);
            assert_eq!(peer_app.selected, None);
            assert_eq!(peer_app.progress.phase, JobPhase::Idle);
            assert!(peer_app.status.is_empty());
        }

        // Phase `sync_reads_demo_state_at_source_step`.
        {
            let (app, _doc) = seeded_app();
            assert_eq!(app.targets.len(), 2);
            assert!(app.source.is_some());
            assert_eq!(app.step, Step::Source);
            assert_eq!(app.recent.len(), 3);
        }

        // Phase `live_app_starts_blank_with_no_fabricated_content`.
        {
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

        // Phase `restart_drops_unsupervised_local_write_state_and_requires_new_review`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.step = Step::Write;
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.8,
                verify_frac: 0.0,
                rate_bps: 64_000_000,
                error: None,
            };
            app.status = "Writing locally".into();
            let saved = doc.save();
            let mut restarted = ImagerApp::new();
            let mut restored_doc = AutoCommit::load(&saved).unwrap();
            restarted.sync(&mut restored_doc, &automerge::ROOT);
            assert_eq!(restarted.step, Step::Source);
            assert_eq!(restarted.progress.phase, JobPhase::Idle);
            assert!(restarted.selected.is_none());
            assert!(restarted.status.is_empty());
            assert!(!restarted.can_advance_with_writer(Step::Drive, true));
        }

        // Phase `write_operations_are_blocked_until_startup_state_is_reconciled`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.startup_write_state_reconciled = false;
            app.confirm_input.set_text("sdb");

            assert!(!app.can_advance_with_writer(Step::Confirm, true));
            let error = app
                .perform_operation(
                    &crate::actions::ImagerOperation::Write {
                        device: app.target().unwrap().dev_path(),
                        confirmation: "sdb".into(),
                    },
                    &mut doc,
                )
                .expect_err("unreconciled startup must not expose a write");
            assert!(error.contains("Restoring the previous image-write state"));

            let root = ui::resources::resources(&app, &doc)
                .into_iter()
                .find(|resource| resource.kind == "imager.sources")
                .expect("imager sources resource owns the write Effect");
            let write = root
                .actions
                .iter()
                .find(|action| action.id == "write")
                .expect("write action");
            assert!(!write.contract.availability.is_available());
        }

        // Phase `live_app_offers_no_tilo_release_to_write`.
        // A release row is a *write source for erasing a drive*, so the live app
        // must offer none of its own invention: the list comes from the document,
        // and a live (unseeded) document has nothing in it. Sample releases are
        // opt-in via `--demo` / `--shots` like every other sample surface here.
        {
            let mut app = ImagerApp::new();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.releases().is_empty(), "no fabricated Tilo releases");

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

        // Phase `demo_document_offers_its_seeded_releases`.
        // The gate is a gate, not a deletion: the demo document still offers them.
        {
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

    fn named_key_ev(key: winit::keyboard::NamedKey) -> WindowEvent {
        use winit::event::KeyEvent;
        use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(key);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn wheel_ev(lines: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, lines),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    fn cursor_ev(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    #[cfg(not(feature = "host-media-writer"))]
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut h,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// Click at a point: move the pointer there (so hover state settles), then
    /// press — exactly the real winit sequence the compositor delivers.
    #[cfg(not(feature = "host-media-writer"))]
    fn click_at(app: &mut ImagerApp, doc: &mut AutoCommit, p: Point) {
        feed(app, doc, &cursor_ev(p.x, p.y));
        feed(app, doc, &click_ev());
    }

    #[cfg(not(feature = "host-media-writer"))]
    fn center(r: Rect) -> Point {
        Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
    }

    /// The rect of the first hit matching `pred`, from this frame's layout.
    #[cfg(not(feature = "host-media-writer"))]
    fn hit_rect(app: &ImagerApp, pred: impl Fn(&Hit) -> bool) -> Rect {
        app.compute_layout()
            .hits
            .into_iter()
            .find(|(_, h)| pred(h))
            .unwrap_or_else(|| panic!("expected a matching hit target this frame"))
            .0
    }

    fn accessible_named<'a>(
        node: &'a ui::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a ui::a11y::AccessNode> {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| accessible_named(child, name))
    }

    fn accessible_focus_order(node: &ui::a11y::AccessNode, names: &mut Vec<String>) {
        if node.state.focusable {
            names.push(node.name.clone().unwrap_or_default());
        }
        for child in &node.children {
            accessible_focus_order(child, names);
        }
    }

    /// Accessible semantics, selection labels, live regions and tab order.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wizard_semantics_labels_live_regions_and_keyboard_order() {
        // Phase `main_canvas_semantics_expose_heading_sources_and_keyboard_order`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);

            let tree = app.accessibility_tree();
            assert_eq!(tree.role, ui::a11y::Role::Window);
            assert_eq!(tree.name.as_deref(), Some("Start Media Creator"));
            let heading = accessible_named(&tree, "Choose the source image").expect("step heading");
            assert_eq!(heading.role, ui::a11y::Role::Heading { level: 1 });
            let selected = accessible_named(&tree, "Selected image").expect("selected image");
            assert!(
                selected
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains("Tilo"))
            );

            let mut order = Vec::new();
            accessible_focus_order(&tree, &mut order);
            assert_eq!(
                order.first().map(String::as_str),
                Some("Step 1 of 6: Source")
            );
            let browse = order
                .iter()
                .position(|name| name == "Browse for an image")
                .expect("browse in focus order");
            let continue_button = order
                .iter()
                .position(|name| name == "Continue")
                .expect("continue in focus order");
            assert!(browse < continue_button);
            assert!(
                accessible_named(&tree, "Step 3 of 6: Customize")
                    .expect("future customize step")
                    .state
                    .disabled
            );
        }

        // Phase `hardware_variants_have_distinct_semantic_selection_and_review_labels`.
        {
            use ui::a11y::Action as AccessAction;

            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            let mut generic = crate::model::sample_releases().remove(0);
            generic.architecture = "aarch64".into();
            let mut board = generic.clone();
            board.hardware_profile = "lenovo-83ed".into();
            let target = format!("imager:release:{}", board.selection_id());
            assert_ne!(generic.selection_id(), board.selection_id());
            app.releases = vec![generic, board.clone()];
            let tree = app.accessibility_tree();
            assert!(
                accessible_named(&tree, &format!("Download {}", board.display_name())).is_some()
            );
            assert!(app.perform_semantic_action(
                &target,
                &AccessAction::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert!(board.matches_source(app.source.as_ref().unwrap()));
            assert!(!app.releases[0].matches_source(app.source.as_ref().unwrap()));
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            let tree = app.accessibility_tree();
            assert!(
                accessible_named(&tree, "Image")
                    .unwrap()
                    .value
                    .as_deref()
                    .unwrap()
                    .contains("lenovo-83ed only")
            );
        }

        // Phase `profile_and_confirmation_semantics_are_editable_and_focusable`.
        {
            use ui::a11y::Action as AccessAction;

            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Customize);

            let tree = app.accessibility_tree();
            let mut order = Vec::new();
            accessible_focus_order(&tree, &mut order);
            let profile_order = [
                "Hostname",
                "Full name",
                "Username",
                "Language and locale",
                "Time zone",
                "Keyboard layout",
                "Filesystem",
                "Preview embedded setup",
            ];
            let positions = profile_order.map(|name| {
                order
                    .iter()
                    .position(|candidate| candidate == name)
                    .unwrap_or_else(|| panic!("missing accessible profile control {name}"))
            });
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
            for name in [
                "Language and locale",
                "Time zone",
                "Keyboard layout",
                "Filesystem",
            ] {
                assert_eq!(
                    accessible_named(&tree, name).expect("profile chooser").role,
                    ui::a11y::Role::ComboBox,
                    "{name} is exposed as a chooser, not unrestricted text"
                );
            }

            assert!(app.perform_semantic_action(
                "imager:profile:hostname",
                &AccessAction::SetValue("accessible-host".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(ImagerDoc::profile(&doc).hostname, "accessible-host");

            for (target, chosen, expected) in [
                ("imager:profile:locale", "de_DE.UTF-8", "de_DE.UTF-8"),
                ("imager:profile:timezone", "Europe/Berlin", "Europe/Berlin"),
                ("imager:profile:keyboard", "German (QWERTZ)", "de"),
            ] {
                assert!(app.perform_semantic_action(
                    target,
                    &AccessAction::SetValue(chosen.into()),
                    &mut doc,
                    &automerge::ROOT,
                ));
                let profile = ImagerDoc::profile(&doc);
                let actual = match target {
                    "imager:profile:locale" => profile.locale,
                    "imager:profile:timezone" => profile.timezone,
                    _ => profile.keyboard,
                };
                assert_eq!(actual, expected);
            }
            let timezone_before = ImagerDoc::profile(&doc).timezone;
            assert!(app.perform_semantic_action(
                "imager:profile:timezone",
                &AccessAction::SetValue("Europe/Not_A_Real_Zone".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(ImagerDoc::profile(&doc).timezone, timezone_before);
            assert!(app.status.contains("Choose a listed Time zone"));

            app.rect = Rect::new(0.0, 0.0, 320.0, 480.0);
            app.scroll_y = 0.0;
            assert!(app.perform_semantic_action(
                "imager:profile:keyboard",
                &AccessAction::Focus,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(app.scroll_y > 0.0, "assistive focus scrolls into view");

            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            app.goto(&mut doc, Step::Confirm);
            let target_name = app.target().expect("target").name.clone();
            let confirm = accessible_named(&app.accessibility_tree(), "Confirm the target device")
                .expect("confirmation input")
                .clone();
            assert_eq!(confirm.role, ui::a11y::Role::TextInput);
            assert!(confirm.state.required);
            assert!(app.perform_semantic_action(
                "imager:hit:confirm-field",
                &AccessAction::SetValue(target_name),
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(app.confirm_matches());
            assert!(app.confirm_input.is_focused());
        }

        // Phase `write_progress_and_status_are_live_regions`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            app.step = Step::Write;
            app.status = "Verifying written data".into();
            app.progress = Progress {
                phase: JobPhase::Verifying,
                write_frac: 1.0,
                verify_frac: 0.4,
                rate_bps: 1,
                error: None,
            };

            let tree = app.accessibility_tree();
            let status = accessible_named(&tree, "Status").expect("live status");
            assert_eq!(status.live, Some(ui::a11y::Politeness::Polite));
            let progress = accessible_named(&tree, "Verifying image").expect("live progress");
            assert_eq!(progress.role, ui::a11y::Role::ProgressBar);
            assert!(progress.state.busy);
            assert_eq!(progress.live, Some(ui::a11y::Politeness::Polite));
            assert!(
                progress
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains("percent"))
            );
            assert_eq!(
                accessible_named(&tree, "Write phase")
                    .expect("write phase progress")
                    .value
                    .as_deref(),
                Some("100 percent")
            );
            assert_eq!(
                accessible_named(&tree, "Verify phase")
                    .expect("verify phase progress")
                    .value
                    .as_deref(),
                Some("40 percent")
            );
        }

        // Phase `tab_reaches_every_profile_control_and_both_footer_actions`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 480.0);
            app.goto(&mut doc, Step::Customize);
            let tab = named_key_ev(winit::keyboard::NamedKey::Tab);

            for slot in 0..7 {
                feed(&mut app, &mut doc, &tab);
                assert_eq!(app.customize.keyboard_slot(), Some(slot));
            }
            assert!(app.scroll_y > 0.0, "lower controls are scrolled into view");

            feed(&mut app, &mut doc, &tab);
            assert_eq!(app.keyboard_hit, Some(Hit::Back));
            feed(&mut app, &mut doc, &tab);
            assert_eq!(app.keyboard_hit, Some(Hit::Next));
            feed(
                &mut app,
                &mut doc,
                &named_key_ev(winit::keyboard::NamedKey::Enter),
            );
            assert_eq!(app.step, Step::Confirm, "focused Continue activates");
        }
    }

    /// Step stations and headings at compact and wide layouts.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn step_rail_keeps_six_stations_inside_the_viewport() {
        // Phase `layout_has_six_step_stations`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let lay = app.compute_layout();
            assert_eq!(lay.crumbs.len(), 6);
            assert!(!lay.compact_crumbs);
        }

        // Phase `compact_layout_keeps_every_step_station_inside_the_viewport`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);
            let lay = app.compute_layout();

            assert!(lay.compact_crumbs);
            assert_eq!(lay.crumbs.len(), Step::all().len());
            // A station is a pointer target (HIG §9.6): the token is the floor,
            // not the layout's own constant.
            let target = f64::from(Metrics::BASE.min_pointer_target);
            assert!(lay.crumbs.iter().all(|(rect, _)| {
                rect.x0 >= app.rect.x0
                    && rect.x1 <= app.rect.x1
                    && rect.width() >= 40.0
                    && rect.height() >= target
            }));
            assert_eq!(
                lay.crumbs.first().map(|(_, step)| *step),
                Some(Step::Source)
            );
            assert_eq!(lay.crumbs.last().map(|(_, step)| *step), Some(Step::Done));
        }

        // Phase `compact_step_targets_use_the_same_on_screen_rects_as_the_rail`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            let lay = app.compute_layout();

            for (rect, step) in &lay.crumbs {
                if app.crumb_reachable(*step) {
                    assert!(lay.hits.iter().any(|(hit_rect, hit)| {
                        hit_rect == rect
                            && matches!(hit, Hit::StepJump(hit_step) if hit_step == step)
                    }));
                }
            }
        }

        // Phase `step_content_starts_below_the_measured_heading_group`.
        // The Source drop zone starts below the measured heading group plus the
        // heading-to-content gap, at 1× and at 2×, and the Confirm review card
        // follows the same rule; the 1× floors the compact test pins still hold.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);

            let browse_top = |app: &ImagerApp| {
                let lay = app.compute_layout();
                let browse = lay
                    .hits
                    .iter()
                    .find_map(|(rect, hit)| matches!(hit, Hit::PickFile).then_some(*rect))
                    .unwrap();
                browse.y0 - lay.content.y0
            };
            let unmeasured = browse_top(&app);
            assert!((unmeasured - STEP_HEADER_FLOOR).abs() < 1e-6);

            assert!(app.adjust_layout_with_measure(&mut taffy, node, &mut tr));
            let (title, sub) = app.source_heading();
            let (_, th) = tr.measure_weighted(title, 19.0, None, TextWeight::Bold);
            let (_, sh) = tr.measure(sub.unwrap(), 13.0, Some(CONTENT_MAX_W as f32));
            let needed = f64::from(th) + STEP_SUB_GAP + f64::from(sh) + app.heading_gap;
            let at_1x = browse_top(&app);
            assert!(
                (at_1x - needed).abs() < 1e-6,
                "drop zone starts after the measured heading"
            );
            assert!(
                at_1x > unmeasured,
                "the measured group needs more than the 1x floor"
            );

            tr.set_text_scale(2.0);
            assert!(app.adjust_layout_with_measure(&mut taffy, node, &mut tr));
            let at_2x = browse_top(&app);
            assert!(at_2x > at_1x * 1.5, "doubled text moves the content down");

            app.goto(&mut doc, Step::Confirm);
            tr.set_text_scale(1.0);
            app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
            let (title, sub) = app.confirm_heading();
            let (_, th) = tr.measure_weighted(title, 19.0, None, TextWeight::Bold);
            let (_, sh) = tr.measure(sub.unwrap(), 13.0, Some(CONTENT_MAX_W as f32));
            let needed = f64::from(th) + STEP_SUB_GAP + f64::from(sh) + app.heading_gap;
            let lay = app.compute_layout();
            assert!((app.step_header_h(lay.content) - needed).abs() < 1e-6);
            // The scroll extent grows by the same amount, so the bottom-anchored
            // confirmation field cannot ride up into the review card.
            let extent = app.content_extent(app.content_viewport_rect());
            let confirm_field = lay
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::ConfirmField).then_some(*rect))
                .unwrap();
            let card_bottom = app.confirm_card_rect(lay.content).y1;
            assert!(
                lay.content.y0 + extent >= confirm_field.y1 + RELATED + app.bands.confirm.note_h,
                "the extent covers the field and its note"
            );
            assert!(
                confirm_field.y0 >= card_bottom + SIBLING,
                "field stays below the review card"
            );
        }
    }

    /// Body blocks, banners, footers, scrolling and failure detail layout.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn step_body_and_footer_reflow_without_overlap() {
        // Phase `source_blocks_follow_the_text_scale_without_overlapping`.
        // The Source blocks keep their 1× bands exactly and grow with the type at
        // 2×: every hit rect is the measured band, consecutive rows never
        // overlap, and the scroll extent covers the taller content.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);

            let rects = |app: &ImagerApp| {
                let lay = app.compute_layout();
                let pick = |f: &dyn Fn(&Hit) -> bool| -> Vec<Rect> {
                    let mut v: Vec<Rect> = lay
                        .hits
                        .iter()
                        .filter_map(|(r, h)| f(h).then_some(*r))
                        .collect();
                    v.sort_by(|a, b| a.y0.total_cmp(&b.y0));
                    v
                };
                (
                    pick(&|h| matches!(h, Hit::PickFile))[0],
                    pick(&|h| matches!(h, Hit::Release(_))),
                    pick(&|h| matches!(h, Hit::Recent(_))),
                )
            };

            app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
            // Every band is its measured line boxes plus rhythm steps — a row is
            // two lines a `LABEL` apart under `RELATED` padding — the hit rects
            // are exactly the bands the painters fill, and no two neighbouring
            // blocks come closer than a `LABEL` step at 1×.
            let m = app.bands.source.clone();
            let row = RELATED + tr.line(14.5) + LABEL + tr.line(11.5) + RELATED;
            assert!(
                (m.release_row_h - row).abs() < 1e-6,
                "release row is measured"
            );
            // The renderer's 1× line boxes are the toolkit's `line_height_px`, so
            // every measured band equals its unmeasured estimate: the probe frame
            // and the painted frame place the blocks identically.
            let floors = SourceMetrics::default();
            for (got, want) in [
                (m.dropzone_h, floors.dropzone_h),
                (m.card_h, floors.card_h),
                (m.card_h_compact, floors.card_h_compact),
                (m.release_row_h, floors.release_row_h),
                (m.recent_row_h, floors.recent_row_h),
                (m.section_h, floors.section_h),
                (m.zero_h, floors.zero_h),
            ] {
                assert!(
                    (got - want).abs() < 1e-6,
                    "1× band {got} equals its estimate {want}"
                );
            }
            let (browse, releases, recents) = rects(&app);
            // The 1× bands `after/dark/source.png` shows: the drop zone 91.6 px
            // (a 16 pt and a 12.5 pt line inside `s6` padding), a release row
            // 52.5 px, a recent row 50 px — and every block at least a pointer
            // target tall.
            let target = f64::from(Metrics::BASE.min_pointer_target);
            assert!(
                (browse.height() - 91.625).abs() < 1e-6,
                "{}",
                browse.height()
            );
            assert!(releases.iter().all(|r| (r.height() - 52.5).abs() < 1e-6));
            assert!(recents.iter().all(|r| (r.height() - 50.0).abs() < 1e-6));
            assert!(
                releases
                    .iter()
                    .chain(&recents)
                    .all(|r| r.height() >= target)
            );
            assert!((browse.height() - m.dropzone_h).abs() < 1e-6);
            assert!(
                releases
                    .iter()
                    .all(|r| (r.height() - m.release_row_h).abs() < 1e-6)
            );
            assert!(
                recents
                    .iter()
                    .all(|r| (r.height() - m.recent_row_h).abs() < 1e-6)
            );
            assert!(!releases.is_empty() && !recents.is_empty());
            let mut blocks: Vec<Rect> = std::iter::once(browse)
                .chain(releases.iter().copied())
                .chain(recents.iter().copied())
                .collect();
            blocks.sort_by(|a, b| a.y0.total_cmp(&b.y0));
            for pair in blocks.windows(2) {
                assert!(
                    pair[1].y0 - pair[0].y1 >= LABEL - 1e-6,
                    "Source blocks keep a token gap at 1× ({} → {})",
                    pair[0].y1,
                    pair[1].y0
                );
            }

            tr.set_text_scale(2.0);
            assert!(app.adjust_layout_with_measure(&mut taffy, node, &mut tr));
            let m = app.bands.source.clone();
            assert!(m.dropzone_h > 96.0 && m.release_row_h > 54.0 && m.recent_row_h > 44.0);
            let (browse, releases, recents) = rects(&app);
            assert!((browse.height() - m.dropzone_h).abs() < 1e-6);
            assert!(
                releases
                    .iter()
                    .all(|r| (r.height() - m.release_row_h).abs() < 1e-6)
            );
            assert!(
                recents
                    .iter()
                    .all(|r| (r.height() - m.recent_row_h).abs() < 1e-6)
            );
            let mut all: Vec<Rect> = std::iter::once(browse)
                .chain(releases.iter().copied())
                .chain(recents.iter().copied())
                .collect();
            all.sort_by(|a, b| a.y0.total_cmp(&b.y0));
            for pair in all.windows(2) {
                assert!(
                    pair[1].y0 >= pair[0].y1,
                    "Source blocks never overlap at 2×"
                );
            }
            // The content extent covers the last block plus the bottom margin.
            let viewport = app.content_viewport_rect();
            let last = all.last().unwrap();
            let lay = app.compute_layout();
            assert!(lay.content.y0 + app.content_extent(viewport) >= last.y1 + SECTION - 1e-6);
        }

        // Phase `confirm_field_follows_the_danger_banner`.
        // The Confirm blocks stack on their measured bands at both content
        // widths: the banner is the card's sibling, the field follows the banner
        // a group later under its caption, rows never overlap, and the hit rect
        // is the painted rect — the layout and the painter read one function.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            for width in [1200.0, 320.0] {
                let (mut app, mut doc) = seeded_app();
                app.rect = Rect::new(0.0, 0.0, width, 800.0);
                app.select_target(&doc, 0);
                app.goto(&mut doc, Step::Confirm);
                app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
                let lay = app.compute_layout();
                let field = lay
                    .hits
                    .iter()
                    .find_map(|(rect, hit)| matches!(hit, Hit::ConfirmField).then_some(*rect))
                    .unwrap();
                let m = app.bands.confirm.clone();
                let card = app.confirm_card_rect(lay.content);
                let banner = app.confirm_banner_rect(lay.content);
                assert!((card.y0 - (lay.content.y0 + app.step_header_h(lay.content))).abs() < 1e-6);
                assert!(
                    (banner.y0 - (card.y1 + SIBLING)).abs() < 1e-6,
                    "banner is the card's sibling at width {width}"
                );
                let setup = if app.offers_write_setup() {
                    app.write_setup
                        .extent(lay.content, app.write_setup_top(lay.content))
                } else {
                    0.0
                };
                assert!(
                    (field.y0 - (banner.y1 + GROUP + setup + m.label_h + LABEL)).abs() < 1e-6,
                    "field follows the banner and write-setup form under its caption at width {width}"
                );
                assert!((field.height() - CONTROL_H).abs() < 1e-6);
                assert_eq!(field, app.confirm_field_rect(lay.content));
                // Measured rows: one per review line, each at least a key line
                // and a `SIBLING` apart, the card wrapping the last with padding.
                assert_eq!(app.confirm_rows().len(), m.row_top.len());
                assert_eq!(m.row_top.len(), 4, "image, drive, capacity, action");
                for pair in m.row_top.windows(2) {
                    assert!(pair[1] - pair[0] >= tr.line(12.0) + SIBLING - 1e-6);
                }
                assert!(m.card_h >= m.row_top.last().copied().unwrap_or(0.0) + SECTION);
                // It still fits inside the step's extent (no scroll needed for
                // the field itself on a tall window).
                let extent = app.content_extent(app.content_viewport_rect());
                assert!(field.y1 + RELATED + m.note_h <= lay.content.y0 + extent);
            }
        }

        // Phase `compact_body_controls_reserve_space_for_wrapped_headings`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);

            let source = app.compute_layout();
            let browse = source
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::PickFile).then_some(*rect))
                .unwrap();
            assert!(browse.y0 >= source.content.y0 + 64.0);
            assert!(browse.x0 >= source.content.x0 && browse.x1 <= source.content.x1);

            app.goto(&mut doc, Step::Drive);
            let drive = app.compute_layout();
            let rescan = drive
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::Rescan).then_some(*rect))
                .unwrap();
            let target = drive
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::Target(_)).then_some(*rect))
                .unwrap();
            assert!(rescan.y0 >= drive.content.y0 + 52.0);
            assert!(target.y0 >= rescan.y1 + 12.0);
        }

        // Phase `compact_footer_actions_never_overlap`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);

            for step in [Step::Drive, Step::Customize, Step::Confirm] {
                app.goto(&mut doc, step);
                let lay = app.compute_layout();
                let back = lay.back.unwrap();
                let primary = lay.primary.unwrap();
                assert!(back.x0 >= lay.content.x0);
                assert!(primary.x1 <= lay.content.x1);
                assert!(back.x1 + 12.0 <= primary.x0);
                assert!(back.width() >= 130.0 && primary.width() >= 130.0);
            }
        }

        // Phase `short_viewport_scrolls_body_without_moving_sticky_footer`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 480.0);
            app.goto(&mut doc, Step::Customize);
            let before = app.compute_layout();
            let footer_before = before.primary.unwrap();
            assert!(app.max_scroll() > 100.0);

            feed(&mut app, &mut doc, &cursor_ev(160.0, 220.0));
            feed(&mut app, &mut doc, &wheel_ev(-3.0));
            assert!(app.scroll_y > 0.0);
            let after = app.compute_layout();
            assert!(after.content.y0 < before.content.y0);
            assert_eq!(after.primary, Some(footer_before));
            assert_eq!(after.viewport_content, before.viewport_content);
        }

        // Phase `failure_detail_is_measured_bounded_and_visible_at_compact_width`.
        {
            let mut renderer = TextRenderer::new();
            let detail = "Could not start the authorized writer because the selected removable drive disappeared while the operating system was preparing it. Reconnect it and try again.";
            let max_width = 280.0;
            let lines = ImagerApp::failure_detail_lines(&mut renderer, detail, max_width, 2);

            assert_eq!(
                lines.len(),
                2,
                "compact failure copy uses both visible lines"
            );
            assert!(
                lines.iter().all(|line| {
                    renderer
                        .measure_weighted(line, 13.5, None, TextWeight::Regular)
                        .0
                        <= max_width
                }),
                "no failure-detail line paints beyond its content bounds"
            );
            assert!(
                lines.join(" ").contains("authorized writer"),
                "the actionable cause is visible rather than replaced by generic compact copy"
            );
            assert!(
                lines.last().is_some_and(|line| line.ends_with('…')),
                "bounded overflow is signalled"
            );
        }
    }

    /// Selection, capability, profile validity and confirmation gating.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wizard_advance_and_confirm_require_valid_choices_and_capability() {
        // Phase `advancing_requires_selections`.
        {
            let (mut app, mut doc) = seeded_app();
            // Source is seeded → can advance to Drive.
            app.advance(&mut doc);
            assert_eq!(app.step, Step::Drive);
            // No target selected yet → stays on Drive.
            app.advance(&mut doc);
            assert_eq!(app.step, Step::Drive);
            app.select_target(&doc, 0);
            app.advance(&mut doc);
            // Seeded source is an installable Tilo image → goes through Customize.
            assert_eq!(app.step, Step::Customize);
            app.advance(&mut doc);
            assert_eq!(app.step, Step::Confirm);
        }

        // Phase `privileged_helper_capability_enables_confirm_but_disabled_backend_does_not`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("sdb");

            assert!(crate::write::WriteBackend::PrivilegedHelper.is_available());
            assert!(app.can_advance_with_writer(
                Step::Confirm,
                crate::write::WriteBackend::PrivilegedHelper.is_available()
            ));
            assert!(!app.can_advance_with_writer(
                Step::Confirm,
                crate::write::WriteBackend::Disabled.is_available()
            ));
        }

        // Phase `invalid_media_profile_blocks_confirm_and_direct_write`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.profile.hostname = "bad host".into();
            app.goto(&mut doc, Step::Customize);

            app.advance(&mut doc);
            assert_eq!(app.step, Step::Customize);
            assert!(app.status.contains("Hostname"));

            // A direct operation dispatch must repeat the gate rather than relying
            // only on wizard navigation.
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("sdb");
            app.start_write(&mut doc);
            assert_eq!(app.step, Step::Confirm);
            assert!(app.job_rx.is_none());
            assert!(app.status.contains("Hostname"));
        }

        // Phase `plain_iso_skips_customize`.
        {
            let (mut app, mut doc) = seeded_app();
            let iso = app
                .recent
                .iter()
                .find(|s| s.kind == SourceKind::IsoFile)
                .cloned()
                .unwrap();
            app.select_source(&mut doc, &iso);
            app.advance(&mut doc); // → Drive
            app.select_target(&doc, 0);
            app.advance(&mut doc); // installable == false → straight to Confirm
            assert_eq!(app.step, Step::Confirm);
            assert!(!app.customizable());
        }

        // Phase `local_tilo_claim_cannot_unlock_preconfiguration`.
        {
            let (mut app, mut doc) = seeded_app();
            let official = crate::model::sample_source();
            let mut local = official.clone();
            local.kind = SourceKind::IsoFile;
            local.path = "/tmp/tilo-official-looking.iso".into();
            local.identity = None;
            app.select_source(&mut doc, &local);
            assert!(!app.customizable());
            assert_eq!(app.action, Action::Flash);

            app.source = Some(official);
            assert!(app.customizable());
        }

        // Phase `official_installable_write_stages_handoff_instead_of_refusing`.
        // Catalog-authenticated images used to refuse Wi-Fi / enrollment on
        // Confirm. They must stage the same handoff local images do; tests
        // then stop at the disabled write backend, never at an installer-stick
        // refusal.
        {
            let (mut app, mut doc) = seeded_app();
            assert!(app.customizable(), "seeded source is official");
            assert!(
                app.offers_write_setup(),
                "official Confirm must collect Wi-Fi and enrollment"
            );
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("sdb");
            app.profile.wifi_ssid = "studio".into();
            app.wifi_psk = "correct-horse".into();
            let err = app
                .perform_operation(
                    &crate::actions::ImagerOperation::Write {
                        device: "/dev/sdb".into(),
                        confirmation: "sdb".into(),
                    },
                    &mut doc,
                )
                .expect_err("privileged writer is off in unit tests");
            assert!(
                !err.to_lowercase().contains("installer stick"),
                "official installable write must not refuse handoff: {err}"
            );
            assert!(
                err.to_lowercase().contains("disabled") || err.to_lowercase().contains("backend"),
                "expected write-disabled after staging handoff, got: {err}"
            );
            assert!(app.job_rx.is_none());
            assert_eq!(app.step, Step::Confirm);
        }

        // Phase `write_refused_without_matching_confirmation`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0); // sdb
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("wrong");
            app.start_write(&mut doc);
            assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
            assert!(app.job_rx.is_none());
            assert_eq!(app.step, Step::Confirm, "stays on the Confirm station");
            assert!(app.status.to_lowercase().contains("confirm"));
        }
    }

    /// Release download job presentation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn release_download_is_local_visible_cancellable_and_keeps_the_loop_awake() {
        // Phase `pending_release_download_keeps_the_event_loop_awake`.
        {
            let (mut app, _doc) = seeded_app();
            assert!(!app.needs_animation_frame());

            let (_tx, rx) = std::sync::mpsc::channel();
            app.release_rx = Some(rx);
            assert!(app.needs_animation_frame());

            app.release_rx = None;
            assert!(!app.needs_animation_frame());
        }

        // Phase `active_release_download_replaces_selection_with_an_accessible_cancel_action`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            let (_tx, rx) = std::sync::mpsc::channel();
            app.release_rx = Some(rx);
            app.release_control = Some(crate::release::DownloadControl::default());
            app.release_download_index = Some(0);
            app.release_download_progress = Some((25, 100));
            app.release_download_stage = Some("downloading");

            let layout = app.compute_layout();
            assert!(
                layout
                    .hits
                    .iter()
                    .any(|(_, hit)| *hit == Hit::CancelDownload)
            );
            assert!(
                !layout
                    .hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::Release(_)))
            );
            let tree = app.accessibility_tree();
            assert!(accessible_named(&tree, "Cancel Tilo installer download").is_some());
        }

        // Phase `release_progress_is_local_visible_and_terminal_events_clear_the_job`.
        {
            let (mut app, mut doc) = seeded_app();
            let (tx, rx) = std::sync::mpsc::channel();
            app.release_rx = Some(rx);
            app.release_control = Some(crate::release::DownloadControl::default());
            app.release_download_index = Some(0);
            tx.send(crate::release::DownloadEvent::Downloading {
                received: 25,
                total: 100,
            })
            .unwrap();
            app.drain_release(&mut doc);
            assert_eq!(app.release_download_progress, Some((25, 100)));
            assert!(app.status.contains("25%"));

            tx.send(crate::release::DownloadEvent::Finished(Err(
                "Tilo installer download cancelled.".into(),
            )))
            .unwrap();
            app.drain_release(&mut doc);
            assert!(app.release_rx.is_none());
            assert!(app.release_control.is_none());
            assert!(app.status.contains("cancelled"));
        }
    }

    /// The primary interaction, proven end to end by clicking the real
    /// on-screen targets: walk Source → Drive → Customize → Confirm. Every transition
    /// goes through the actual hit-testing + dispatch path, not a shortcut.
    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn clicking_through_the_wizard_refuses_a_fake_write_when_backend_is_disabled() {
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

        // Drive → Customize (the seeded Tilo image is customizable).
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
        assert!(app.confirm_input.is_focused());
        app.confirm_input.set_text("wrong target");
        app.confirm_input.select_all();
        for s in ["s", "d", "b"] {
            feed(&mut app, &mut doc, &char_ev(s));
        }
        assert!(
            app.confirm_matches(),
            "selection replacement through TextInput armed the write"
        );
        assert!(
            automerge::ReadDoc::get(&doc, &automerge::ROOT, "confirm")
                .unwrap()
                .is_none(),
            "destructive consent is never written to the shared document"
        );

        if crate::write::allow_write() {
            // Never point this UI test at a real block device.
            return;
        }

        // The primary control stays disabled: a matching phrase cannot turn an
        // unavailable backend into a synthetic success.
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Confirm);
        assert_eq!(app.progress.phase, JobPhase::Idle);
        assert!(app.job_rx.is_none());
    }

    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn matching_confirmation_does_not_simulate_a_disabled_write() {
        if crate::write::allow_write() {
            return;
        }
        let (mut app, mut doc) = seeded_app();
        app.select_target(&doc, 0); // sdb
        app.goto(&mut doc, Step::Confirm);
        app.confirm_input.set_text("sdb");
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Idle);
        assert_eq!(app.step, Step::Confirm);
        assert!(app.job_rx.is_none());
        assert!(app.status.contains("disabled"));
    }

    /// Portal sources, external drives, rescans and forbidden disks.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn source_and_target_selection_accept_only_inspected_external_media() {
        // Phase `portal_open_selects_inspected_source`.
        {
            let (mut app, mut doc) = seeded_app();
            // The recording host answers with a real Tilo-looking image.
            let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
            std::fs::write(f.path(), b"tilo payload for the imager test").unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(PortalOutcome::Opened(f.path().to_path_buf()));
            app.open_file_picker(&doc);
            assert_eq!(recorder.pick_requests()[0].title(), "Choose a disk image");
            app.drain_portal(&mut doc);
            // A `.iso` file → a valid source is selected.
            assert!(app.source.is_some());
            assert_eq!(app.source.as_ref().unwrap().kind, SourceKind::IsoFile);
        }

        // Phase `drive_step_only_offers_external_installation_media`.
        {
            let (app, _doc) = seeded_app();
            // Sample has one USB (removable) + one NVMe (fixed).
            assert_eq!(app.visible_targets().len(), 1);
            assert_eq!(app.removable_count(), 1);
        }

        // Phase `authoritative_empty_rescan_clears_stale_target_and_consent`.
        {
            let (mut app, doc) = seeded_app();
            app.select_target(&doc, 0);
            app.confirm_input.set_text("sdb");

            app.apply_drive_inventory(Vec::new());

            assert!(app.targets.is_empty());
            assert_eq!(app.selected, None);
            assert!(app.confirm_input.value().is_empty());
        }

        // Phase `rescan_preserves_only_the_same_physical_target`.
        {
            let (mut app, doc) = seeded_app();
            app.select_target(&doc, 0);
            app.confirm_input.set_text("sdb");
            let selected = app.target().unwrap().clone();
            let mut reordered = crate::model::sample_targets();
            reordered.swap(0, 1);

            app.apply_drive_inventory(reordered);

            assert_eq!(app.selected, Some(1));
            assert_eq!(
                app.target().and_then(|drive| drive.stable_id.as_deref()),
                selected.stable_id.as_deref()
            );
            assert_eq!(app.confirm_input.value(), "sdb");

            let mut replacement = selected;
            replacement.stable_id = Some("different-usb-stick".into());
            app.apply_drive_inventory(vec![replacement]);

            assert_eq!(
                app.selected, None,
                "a reused OS device slot is not the reviewed drive"
            );
            assert!(
                app.confirm_input.value().is_empty(),
                "consent never carries to replacement hardware"
            );

            let mut unidentified_before = crate::model::sample_targets()[0].clone();
            unidentified_before.stable_id = None;
            app.targets = vec![unidentified_before.clone()];
            app.selected = Some(0);
            app.confirm_input.set_text(&unidentified_before.name);
            app.apply_drive_inventory(vec![unidentified_before]);
            assert_eq!(
                app.selected, None,
                "a drive without stable identity must be chosen again after rescan"
            );
            assert!(app.confirm_input.value().is_empty());
        }

        // Phase `internal_and_system_disks_can_never_be_selected`.
        {
            let mut app = ImagerApp::new();
            let mut doc = AutoCommit::new();
            let mut targets = crate::model::sample_targets();
            targets[1].is_system = true; // the internal NVMe is the system disk
            app.targets = targets.clone();
            ImagerDoc::seed(&mut doc, &targets);
            ImagerDoc::set_source(&mut doc, Some(&crate::model::sample_source()));
            app.sync(&mut doc, &automerge::ROOT);

            app.dispatch_hit(&mut doc, Hit::Target(1));
            assert_eq!(app.selected, None, "the system disk is never selectable");
            assert!(app.status.to_lowercase().contains("system disk"));
        }
    }

    /// Set the Customize step up with one field cleared and focused, ready for
    /// the shared panel to receive keystrokes through the real event path.
    fn customize_with_field_focused(field: crate::customize::CzField) -> (ImagerApp, AutoCommit) {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        // Clear the target field so the typed assertion is exact.
        let mut p = ImagerDoc::profile(&doc);
        if field == crate::customize::CzField::Hostname {
            p.hostname.clear();
        }
        ImagerDoc::set_profile(&mut doc, &p);
        app.step = Step::Customize;
        app.sync(&mut doc, &automerge::ROOT); // reads the profile + step back
        assert_eq!(
            app.step,
            Step::Customize,
            "the panel only handles events on this step"
        );
        assert!(app.customizable(), "the seeded Tilo image is customizable");
        app.customize.focus_for_test(field);
        (app, doc)
    }

    /// Hostname typing, keyboard chooser and preview.
    #[test]
    fn profile_panel_edits_persist_through_the_real_event_path() {
        // Phase `typing_into_hostname_persists_to_profile_through_the_panel`.
        {
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

        // Phase `keyboard_chooser_filters_and_commits_through_the_real_event_path`.
        {
            use crate::customize::CzField;
            use winit::keyboard::NamedKey;

            let (mut app, mut doc) = customize_with_field_focused(CzField::Keyboard);
            feed(&mut app, &mut doc, &named_key_ev(NamedKey::Enter));
            for character in ["g", "e", "r", "m", "a", "n"] {
                feed(&mut app, &mut doc, &char_ev(character));
            }
            feed(&mut app, &mut doc, &named_key_ev(NamedKey::Enter));

            assert_eq!(ImagerDoc::profile(&doc).keyboard, "de");
            assert_eq!(
                app.customize.field_value(CzField::Keyboard),
                "German (QWERTZ)"
            );
        }

        // Phase `preview_shows_the_embedded_profile`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 1);
            app.export_config(&mut doc);
            let text = app.export_dialog.text();
            assert!(text.contains("\"schema\": 1"));
            assert!(text.contains("\"hostname\""));
            assert!(!text.contains("password"));
            assert!(app.export_dialog.is_open());
        }
    }

    /// Write another, failure, cancellation and cancel races.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn write_completion_failure_and_cancel_return_to_a_truthful_state() {
        // Phase `write_another_resets_to_source`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.progress = Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
                error: None,
            };
            app.write_another(&mut doc);
            assert_eq!(app.step, Step::Source);
            assert_eq!(app.progress.phase, JobPhase::Idle);
        }

        // Phase `failed_write_keeps_its_cause_and_returns_to_a_fresh_review`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Write);
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.4,
                verify_frac: 0.0,
                rate_bps: 0,
                error: None,
            };
            let (tx, rx) = std::sync::mpsc::channel();
            app.job_rx = Some(rx);
            tx.send(Progress {
                phase: JobPhase::Failed,
                write_frac: 0.4,
                verify_frac: 0.0,
                rate_bps: 0,
                error: Some(
                    "Could not write the drive: the selected drive was disconnected.".into(),
                ),
            })
            .unwrap();

            app.drain_job(&mut doc);
            assert_eq!(app.step, Step::Done);
            assert_eq!(app.progress.phase, JobPhase::Failed);
            assert!(
                app.status.contains("disconnected"),
                "the privileged writer's actionable cause reaches the UI"
            );
            let tree = app.accessibility_tree();
            let result = accessible_named(&tree, "Write failure details")
                .expect("accessible terminal result");
            assert!(
                result
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains("disconnected"))
            );

            app.confirm_input.set_text("sdb");
            app.write_another(&mut doc);
            assert_eq!(app.step, Step::Confirm, "failure returns to review");
            assert!(app.source.is_some(), "the verified source is preserved");
            assert_eq!(app.selected, Some(0), "the selected drive is preserved");
            assert!(
                app.confirm_input.value().is_empty(),
                "retry requires fresh destructive consent"
            );
            assert_eq!(app.progress.phase, JobPhase::Idle);
        }

        // Phase `active_write_cancel_operation_requests_a_safe_stop`.
        {
            let (mut app, mut doc) = seeded_app();
            let control = JobControl::new();
            app.job_control = Some(control.clone());
            app.goto(&mut doc, Step::Write);

            app.perform_operation(&crate::actions::ImagerOperation::Cancel, &mut doc)
                .unwrap();

            assert!(control.is_cancelled());
            assert!(app.status.contains("Keep the drive connected"));
            let footer = app.a11y_footer_controls(&app.compute_layout());
            let cancel = footer
                .iter()
                .find(|node| node.name.as_deref() == Some("Cancel image write"))
                .expect("write screen exposes a cancel button");
            assert!(cancel.state.disabled);
        }

        // Phase `cancel_wins_a_race_with_an_already_queued_success`.
        {
            let (mut app, mut doc) = seeded_app();
            let control = JobControl::new();
            control.cancel();
            app.job_control = Some(control);
            app.progress.phase = JobPhase::Verifying;
            app.goto(&mut doc, Step::Write);
            let (tx, rx) = std::sync::mpsc::channel();
            app.job_rx = Some(rx);
            tx.send(Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
                error: None,
            })
            .unwrap();

            app.drain_job(&mut doc);

            assert_eq!(app.progress.phase, JobPhase::Failed);
            assert!(app.progress.error.as_deref().is_some_and(|message| {
                message.contains("cancelled") && message.contains("incomplete")
            }));
            assert_eq!(app.step, Step::Done);
        }
    }
}
