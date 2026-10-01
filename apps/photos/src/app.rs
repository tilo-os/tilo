//! The root widget: a toolbar, the image canvas, and a status line.
//!
//! # Composition
//!
//! Every control here is a shared toolkit widget. The buttons are
//! [`ui::widget::Button`], the canvas is [`ui::widget::ImageViewer`], and the
//! nothing-open state is [`ui::widget::EmptyState`].
//!
//! There is no title bar. The compositor owns window chrome; an app that drew
//! its own name at the top would be drawing it twice.
//!
//! # Where the pixels live
//!
//! The document holds a portable content reference and an [`Edits`] value.
//! This struct holds the decoded original and the rendered result, both
//! outside the CRDT, and re-derives the rendering whenever that reference or
//! the edits change. That check is what keeps a rotate from re-decoding the
//! file, and what keeps the decoded buffer from being rebuilt on every frame.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use ui::time::Duration;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use tilo_content::ContentStore;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::ImageData;

use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::animation::{Animated, Curve, Transition};
use ui::chrome::fill_scrim;
use ui::icons::{Icon, draw_icon};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::Spacing;
use ui::widget::{
    Button, ButtonFace, ButtonStyle, DateIndexIntent, DateIndexMarker, DateIndexMarkerKind,
    DateIndexedScrollbar, EmptyState, EmptyStateContent, ImageViewer, ModalAction, ModalFrame,
    ModalHit, ModalLayout, ModalSpec, ProgressBar, ScrollMetrics, SearchField, SegmentedFace,
    TextInput, TextInputKind, ToggleGroup, Widget,
};
use winit::keyboard::{Key, NamedKey};

use crate::doc::PhotosDoc;
use crate::model::{Edits, LibraryPhoto, OpenImage, Rect01};
use crate::pixels::{self, Rgba};
use crate::strings::tr;
use crate::timeline_projection::{TimelineGeometry, TimelinePhoto, TimelineProjection};

/// Toolbar height, and the status bar's. Both are chrome around the canvas,
/// which takes everything left over.
const BAR_H: f32 = 48.0;
const PAD: f32 = Spacing::BASE.s3;

/// A `Spacing::BASE` step as the `f64` this painter's geometry uses.
#[allow(
    clippy::cast_lossless,
    reason = "const context: `f64::from` is not const; f32 -> f64 is exact"
)]
const fn step(px: f32) -> f64 {
    px as f64
}

/// The photo-details sheet on one rhythm (HIG §7b): `s4` sheet padding,
/// title ↔ subtitle `s2` (related rows), subtitle ↔ first field `s6` (the
/// next group), label ↔ its input `s1`, field ↔ field `s2`, body ↔ footer
/// `s2`. The line boxes are measured, so a wrapped 200% subtitle pushes the
/// fields down instead of under "Folder", and the field list scrolls beneath
/// the pinned footer when the viewport caps the sheet.
const SHEET_INSET: f64 = step(Spacing::BASE.s4);
const SHEET_TOP_PAD: f64 = step(Spacing::BASE.s4);
const SHEET_TITLE_GAP: f64 = step(Spacing::BASE.s2);
const SHEET_SUBTITLE_GAP: f64 = step(Spacing::BASE.s6);
const SHEET_LABEL_GAP: f64 = step(Spacing::BASE.s1);
const SHEET_INPUT_H: f64 = 38.0;
const SHEET_FIELD_GAP: f64 = step(Spacing::BASE.s2);
const SHEET_BODY_FOOTER_GAP: f64 = step(Spacing::BASE.s2);
/// Library shelf rhythm: chips beside each other are siblings (`s2` on the
/// dense shelf), the album chip row keeps `s3` above the filter chips, the
/// filter row keeps `s2` above the grid, and the phone's album actions keep
/// `s2` below the filters and above the grid.
const LIB_CHIP_GAP: f64 = step(Spacing::BASE.s2);
const LIB_ROW_GAP: f64 = step(Spacing::BASE.s3);
const LIB_ROW_GAP_TIGHT: f64 = step(Spacing::BASE.s2);

/// The rest face of a neutral control that sits on `ground`: one control
/// hairline (`HAIRLINE_ALPHA`) composited over that ground, so the face is a
/// measured lift off whatever it is painted on — on a `raised` card footer or
/// sheet, where `Colors::lift(1.0)` would resolve to the ground itself and
/// the face would vanish into it. Its edge then derives from this face via
/// [`surface::card`].
const SHEET_BUTTON_H: f64 = 44.0;
const RETAINED_CONTROL_OPERATION_TYPE: &str = "tilo.ui.control.invoke";

/// Button height. Deliberately shorter than the bar so the controls sit in
/// the strip rather than filling it edge to edge.
const BTN_H: f32 = 34.0;

/// The shared `Button` face paints its label at this renderer-relative size
/// (`Button::measure`), and the renderer applies the accessibility text scale
/// plus the default `TextStyle` line height. The toolbar capsules and the
/// status band keep their authored 1× heights until that measured line no
/// longer fits, then grow just enough to contain it — so 200% labels stop
/// colliding with the capsule edge while 1× density is untouched.
/// (Alignment review 2026-09-16; kept local to this file because the toolbar
/// is sized in `build_layout`, which sees the theme but no renderer.)
const BUTTON_LABEL_SIZE: f32 = 14.0;
const STATUS_TEXT_SIZE: f32 = 13.0;
const BTN_PAD_Y: f32 = 8.0;
const STATUS_PAD_Y: f32 = 4.0;

/// Line height the renderer will give `size` at `text_scale` — the default
/// `TextStyle` leading on the scaled size, not a local copy of that factor.
fn line_h(size: f32, text_scale: f32) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a line height in logical px fits f32 exactly as the renderer reports it"
    )]
    let line = ui::text_metrics::line_height_px(size, text_scale) as f32;
    line
}

/// Toolbar capsule height at `text_scale`: the authored [`BTN_H`] at 1×.
fn btn_h(text_scale: f32) -> f32 {
    BTN_H.max(line_h(BUTTON_LABEL_SIZE, text_scale) + 2.0 * BTN_PAD_Y)
}

/// Status band height at `text_scale`: the authored `BAR_H * 0.6` at 1×, and
/// tall enough to contain the filename line at any scale (plan task 19).
fn status_h(text_scale: f32) -> f32 {
    (BAR_H * 0.6).max(line_h(STATUS_TEXT_SIZE, text_scale) + 2.0 * STATUS_PAD_Y)
}

/// A library card's action row: Details on the left, remove and favourite on
/// the right, all inset [`CARD_ACTION_EDGE_INSET`] from the card's edges.
///
/// This is the *one* description of that row. `draw_library` paints these
/// rects, `handle_event` hit-tests them, `accessibility_tree` exports them and
/// `library_card_action_rects` hands them to tests — four call sites that used
/// to carry their own copies of the arithmetic (and the right-hand cluster ran
/// flush to `card.x1`, 5 px from the edge against Details' 12).
#[derive(Clone, Copy, Debug, PartialEq)]
struct CardActions {
    details: Rect,
    remove: Rect,
    favorite: Rect,
}

const CARD_ACTION_EDGE_INSET: f64 = 8.0;
const CARD_ACTION_ROW_H: f64 = 44.0;
const CARD_ACTION_CONTROL_W: f64 = 44.0;

fn card_action_controls(card: Rect) -> CardActions {
    let action_y = card.y1 - CARD_ACTION_ROW_H;
    let favorite = Rect::new(
        card.x1 - CARD_ACTION_EDGE_INSET - CARD_ACTION_CONTROL_W,
        action_y,
        card.x1 - CARD_ACTION_EDGE_INSET,
        card.y1,
    );
    let remove = Rect::new(
        favorite.x0 - CARD_ACTION_CONTROL_W,
        action_y,
        favorite.x0,
        card.y1,
    );
    let details = Rect::new(
        card.x0 + CARD_ACTION_EDGE_INSET,
        action_y,
        (card.x0 + 108.0).min(remove.x0),
        card.y1,
    );
    CardActions {
        details,
        remove,
        favorite,
    }
}

/// Per-character label width used to size a button in `build_layout`, which
/// gets a [`Theme`] but no [`TextRenderer`] to shape text with.
///
/// An **upper bound**, not an average. The shared [`ui::widget::Toolbar`]
/// estimates 7.0 for the same job, which is too tight for this one: at 14 px
/// Medium, "Open" shapes to 36.5 px — 9.13 a character — so a 7.0 budget left
/// the label 8.5 px wider than the button drawn around it, and `Button::paint`
/// centres the cluster, so it spilled out of both ends onto its neighbours.
/// `toolbar_labels_fit_inside_the_buttons_they_are_sized_for` in
/// `tests/end_to_end.rs` shapes the real glyphs and holds this bound.
pub const APPROX_CHAR_W: f32 = 10.0;

/// Glyph box, and the gap to a label beside it. `Button::paint` derives the
/// icon from the button height as `(h - 14).clamp(12, 22)`, which at
/// [`BTN_H`] is exactly this.
const ICON_W: f32 = BTN_H - 14.0;
const ICON_GAP: f32 = 6.0;
/// Padding either side of an icon-and-label cluster.
const LABEL_PAD_X: f32 = 12.0;
/// Gap between adjacent buttons.
const BTN_GAP: f32 = 6.0;

/// Coarse, bundled coastlines for the private/offline Places overview. These
/// are intentionally only continental silhouettes: they add orientation but
/// cannot be mistaken for current navigation data or a precision basemap.
const OFFLINE_WORLD_LANDMASSES: &[&[(f64, f64)]] = &[
    &[
        (72.0, -168.0),
        (70.0, -140.0),
        (60.0, -125.0),
        (50.0, -124.0),
        (32.0, -117.0),
        (18.0, -104.0),
        (8.0, -83.0),
        (20.0, -74.0),
        (45.0, -61.0),
        (58.0, -72.0),
        (72.0, -95.0),
        (72.0, -168.0),
    ],
    &[
        (13.0, -81.0),
        (6.0, -77.0),
        (-8.0, -78.0),
        (-25.0, -70.0),
        (-55.0, -68.0),
        (-52.0, -42.0),
        (-20.0, -40.0),
        (2.0, -50.0),
        (13.0, -61.0),
        (13.0, -81.0),
    ],
    &[
        (36.0, -10.0),
        (51.0, -5.0),
        (61.0, 15.0),
        (70.0, 40.0),
        (58.0, 65.0),
        (72.0, 110.0),
        (62.0, 165.0),
        (45.0, 145.0),
        (20.0, 121.0),
        (7.0, 106.0),
        (22.0, 78.0),
        (10.0, 43.0),
        (31.0, 31.0),
        (36.0, -10.0),
    ],
    &[
        (35.0, -17.0),
        (31.0, 31.0),
        (12.0, 44.0),
        (-12.0, 43.0),
        (-35.0, 20.0),
        (-30.0, 10.0),
        (-5.0, 8.0),
        (15.0, -17.0),
        (35.0, -17.0),
    ],
    &[
        (-10.0, 113.0),
        (-18.0, 146.0),
        (-39.0, 147.0),
        (-44.0, 115.0),
        (-25.0, 112.0),
        (-10.0, 113.0),
    ],
    &[
        (-63.0, -180.0),
        (-70.0, -120.0),
        (-72.0, -60.0),
        (-68.0, 0.0),
        (-72.0, 60.0),
        (-69.0, 120.0),
        (-63.0, 180.0),
        (-63.0, -180.0),
    ],
];
/// Horizontal room a group separator occupies, rule included.
const SEP_W: f32 = 17.0;

/// Breathing room between the photograph and the chrome above and below it.
/// Without it a contain-fit image whose limiting axis is the height butts
/// straight against the toolbar rule and looks wedged under it.
const CANVAS_INSET: f32 = 10.0;

/// Library previews are recognition aids, never an alternate full-resolution
/// image cache. At most four in flight on the shared preview pipeline's single
/// worker keeps a hostile library from occupying a core or retaining unbounded
/// decoded pixels.
const THUMBNAIL_SIZE: thumbnail::ThumbSize = thumbnail::ThumbSize::new(320, 180);
const THUMBNAIL_QUEUE_LIMIT: usize = 4;
const DIRECTORY_IMPORT_PER_FRAME: usize = 2;
/// A receipt must not retain an unbounded directory's diagnostics. The total
/// count remains visible; these are the concrete entries a person can inspect.
const DIRECTORY_FAILURE_DETAIL_LIMIT: usize = 20;
const DIRECTORY_FAILURES_PER_PAGE: usize = 4;
const ANALYSIS_QUEUE_LIMIT: usize = 2;
/// One bounded materialized library page. This is intentionally independent
/// of the number of cards that happen to fit in a particular window.
const LIBRARY_PAGE_SIZE: usize = 24;
/// People, places, and albums are projected one bounded page at a time. The
/// backing vectors retain the full collection so “Browse all” is real rather
/// than a prettier name for truncation.
const COLLECTION_PAGE_SIZE: usize = 4;

/// A four-column shelf is only useful while each photograph remains a
/// recognisable, comfortably targetable card. At tablet widths a two-column
/// shelf works well; below this a single card preserves independent Details,
/// favourite, and removal targets at the 44px touch minimum.
const LIBRARY_TWO_COLUMN_MAX_W: f64 = 720.0;

const LIBRARY_ONE_COLUMN_MAX_W: f64 = 460.0;
/// The full masthead needs room for destinations, density and selection.
/// Tablets retain photo density but use the compact masthead below this width.
const LIBRARY_DESKTOP_MASTHEAD_MIN_W: f64 = 960.0;
/// Presentation-only density for the chronological library.  It is deliberately
/// pure: a pinch and the visible +/- buttons must land on the same geometry.
/// The lower bound protects the 44 px card actions; the upper bound keeps a
/// photograph recognisable rather than turning the timeline into a file list.
pub fn timeline_column_count(width: f64, density: u8, large_text: bool) -> usize {
    if large_text && width < 620.0 {
        return 1;
    }
    let base = if width < LIBRARY_ONE_COLUMN_MAX_W {
        1
    } else if width < 720.0 {
        2
    } else if width < 1_080.0 {
        3
    } else {
        4
    };
    let density = usize::from(density.min(4));
    let maximum = if large_text || width < LIBRARY_TWO_COLUMN_MAX_W {
        // A phone and compact tablet need real card footers, not a density
        // mode that turns the timeline into horizontally clipped glyphs.
        // Pinch/zoom still adjusts the spaciousness of the same one/two
        // columns through row geometry and preserves the scroll anchor.
        2
    } else if width >= 1_440.0 {
        6
    } else {
        5
    };
    (base + density).min(maximum).max(1)
}

/// Bounded virtual window for a row of a long chronological collection.
/// Keeping this independent of painting prevents a large library from being
/// accidentally decoded or exposed to accessibility as one giant surface.
pub fn timeline_virtual_range(
    total: usize,
    first_row: usize,
    rows: usize,
    columns: usize,
) -> std::ops::Range<usize> {
    let columns = columns.max(1);
    let start = first_row.saturating_mul(columns).min(total);
    let end = start
        .saturating_add(rows.saturating_add(2).saturating_mul(columns))
        .min(total);
    start..end
}

/// Measured rows for the compact library masthead.  Compact and 200% text
/// share this source of truth so the count, selection affordance, density,
/// collection rail, and first date never occupy the same vertical space.
///
/// Text is painted on a baseline, so the row bounds deliberately include a
/// little extra leading rather than treating the baseline as the row's top.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CompactHeaderGeometry {
    title_y: f64,
    count_y: f64,
    controls: Rect,
    albums_y: f64,
    album_actions: Rect,
    timeline_y: f64,
}

fn compact_library_header_geometry(
    bounds: Rect,
    large_text: bool,
    has_albums: bool,
) -> CompactHeaderGeometry {
    let top = bounds.y0 + f64::from(PAD);
    if large_text {
        let controls_y = top + 68.0;
        let albums_y = controls_y + 56.0;
        let actions_y = albums_y + if has_albums { 54.0 } else { 0.0 } + 10.0;
        let album_actions = Rect::new(
            bounds.x0 + f64::from(PAD),
            actions_y,
            bounds.x1 - f64::from(PAD),
            actions_y + 44.0,
        );
        CompactHeaderGeometry {
            title_y: top,
            // Keep this deliberately short at 200%; a wrapped count makes
            // the next explicit controls row impossible to reason about.
            count_y: top + 34.0,
            controls: Rect::new(
                bounds.x0 + f64::from(PAD),
                controls_y,
                bounds.x0 + f64::from(PAD) + 142.0,
                controls_y + 44.0,
            ),
            albums_y,
            album_actions,
            timeline_y: album_actions.y1 + 10.0,
        }
    } else {
        // A compact library still needs one quiet line of context beneath its
        // title.  Without it, the icon-only density controls read as an
        // unrelated toolbar and a small library has no indication that its
        // chronology is complete.  Give the count its own baseline rather
        // than squeezing it into the title/control row.
        let count_y = top + 26.0;
        let albums_y = top + 50.0;
        CompactHeaderGeometry {
            title_y: top,
            count_y,
            controls: Rect::new(
                bounds.x1 - 118.0,
                bounds.y0 + 8.0,
                bounds.x1 - f64::from(PAD),
                bounds.y0 + 44.0,
            ),
            albums_y,
            album_actions: Rect::ZERO,
            timeline_y: albums_y + if has_albums { 38.0 } else { 0.0 } + 8.0,
        }
    }
}

struct ThumbnailResult {
    generation: u64,
    id: String,
    preview: Option<Rgba>,
}

struct AnalysisResult {
    invocation: tilo_actions::InvocationId,
    photo_id: String,
    content: Option<String>,
    result: AnalysisOutcome,
}

/// Per-device checkpoint for the sensitive People scan.  It contains only a
/// cursor and status, never face geometry, embeddings, crops, model output,
/// source paths, or a person name.  Keeping it outside Automerge makes resume
/// local to the owner who consented.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct PeopleScanProgress {
    next_offset: usize,
    completed_batches: u64,
    last_library_size: usize,
    status: String,
    last_error: Option<String>,
}

/// The exact local batch admitted to the one analysis worker.  This is kept
/// in memory rather than in the CRDT (or the checkpoint) because it contains
/// no biometric result, and because a cursor is only durable *after* this
/// batch succeeds.
#[derive(Clone, Debug)]
struct PendingPeopleScan {
    start: usize,
    scanned: usize,
    total: usize,
}

impl PeopleScanProgress {
    /// Plan a bounded contiguous slice without moving the durable cursor.
    /// The caller commits `advance_after_success` only after the face worker
    /// has returned and its opaque local clusters were accepted.
    fn batch_range(&self, available: usize, limit: usize) -> Option<(usize, usize)> {
        (available > 0).then(|| {
            let start = self.next_offset % available;
            // Do not wrap a final partial batch: every locally available
            // photograph is covered once before the next pass begins.
            (start, limit.min(available.saturating_sub(start)))
        })
    }

    fn advance_after_success(&mut self, start: usize, scanned: usize, total: usize) {
        if total == 0 {
            return;
        }
        self.next_offset = start.saturating_add(scanned) % total;
        self.last_library_size = total;
        self.completed_batches = self.completed_batches.saturating_add(1);
        if self.next_offset == 0 {
            "complete"
        } else {
            "paused"
        }
        .clone_into(&mut self.status);
        self.last_error = None;
    }

    fn record_failure(&mut self, error: String) {
        // In particular, do not alter next_offset here: retry must send the
        // same slice, not skip photographs whose private analysis failed.
        "error".clone_into(&mut self.status);
        self.last_error = Some(error);
    }
    fn load() -> Self {
        Self::file()
            .and_then(|file| file.read())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let (Some(file), Ok(bytes)) = (Self::file(), serde_json::to_vec(self)) {
            let _ = file.write(&bytes);
        }
    }

    fn file() -> Option<crate::files::StateFile> {
        crate::files::StateFile::in_state("photos/people-scan.json")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageAnalysisComponent {
    Ocr,
    Vision,
}

#[expect(
    clippy::large_enum_variant,
    reason = "outcomes travel through a small bounded analysis channel and are consumed on the next frame; boxing would add an allocation per job for no meaningful saving"
)]
enum AnalysisOutcome {
    Image {
        component: ImageAnalysisComponent,
        result: Result<crate::model::ImageAnalysis, String>,
    },
    People(Result<crate::people::VerifiedFaceClusterBatch, String>),
}

struct AnalysisJob {
    invocation: tilo_actions::InvocationId,
    photo_id: String,
    content: Option<String>,
    image: OpenImage,
    kind: AnalysisJobKind,
}

/// All pixel intelligence shares exactly one bounded worker. Adding a new
/// analysis kind must not create another request thread or bypass the queue.
enum AnalysisJobKind {
    Ocr {
        language: String,
    },
    Vision {
        capability: tilo_vision::LocalVisionCapability,
    },
    People {
        capability: tilo_vision::LocalFaceCapability,
        images: Vec<(String, OpenImage)>,
    },
}

fn analysis_failure(
    invocation: tilo_actions::InvocationId,
    message: String,
) -> tilo_actions::InvocationUpdate {
    tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: "photos.analysis_failed".into(),
            message,
            details: None,
            retryable: true,
        },
    }
}

fn mark_analysis_shared(analysis: &mut crate::model::ImageAnalysis) {
    "shared".clone_into(&mut analysis.provenance.scope);
    if let Some(provenance) = &mut analysis.ocr_provenance {
        "shared".clone_into(&mut provenance.scope);
    }
    if let Some(provenance) = &mut analysis.vision_provenance {
        "shared".clone_into(&mut provenance.scope);
    }
}

fn analysis_worker(
    rx: &Receiver<AnalysisJob>,
    tx: &SyncSender<AnalysisResult>,
    content: &ContentStore,
) {
    while let Ok(job) = rx.recv() {
        if tx
            .send(AnalysisResult {
                invocation: job.invocation,
                photo_id: job.photo_id,
                content: job.content,
                result: match job.kind {
                    AnalysisJobKind::Ocr { language } => AnalysisOutcome::Image {
                        component: ImageAnalysisComponent::Ocr,
                        result: crate::analysis::recognize_text(&job.image, &language, content),
                    },
                    AnalysisJobKind::Vision { capability } => AnalysisOutcome::Image {
                        component: ImageAnalysisComponent::Vision,
                        result: crate::analysis::analyze_image(&job.image, &capability, content),
                    },
                    AnalysisJobKind::People { capability, images } => AnalysisOutcome::People(
                        crate::analysis::detect_people(images, &capability, content),
                    ),
                },
            })
            .is_err()
        {
            return;
        }
    }
}

/// Wake the idle host whenever a preview settles, success or failure, so a
/// finished thumbnail paints without waiting for user input.
fn with_thumbnail_wake(service: thumbnail::ThumbnailService) -> thumbnail::ThumbnailService {
    let wake = ui::wake::WakeHandle::new();
    service.set_wake(move || wake.wake());
    service
}

/// The shared-pipeline request for one library photograph. A content
/// reference is the preview's content address and persists in the preview
/// cache; a legacy path-only photograph has no stable content identity, so its
/// preview is generated for this session only.
fn thumbnail_request(image: &OpenImage, content: &ContentStore) -> thumbnail::ThumbRequest {
    let persistent = image
        .content
        .as_deref()
        .and_then(|address| thumbnail::ThumbKey::of_address(address, THUMBNAIL_SIZE));
    let key = persistent.unwrap_or_else(|| {
        thumbnail::ThumbKey::of_bytes(decode_key(image).as_bytes(), THUMBNAIL_SIZE)
    });
    let (image, content) = (image.clone(), content.clone());
    let request = thumbnail::ThumbRequest::load(key, thumbnail::FileCategory::Image, move || {
        crate::doc::bytes_of(&image, &content)
    });
    if persistent.is_some() {
        request
    } else {
        request.transient()
    }
}

/// One toolbar affordance. Buttons post their intent here rather than
/// mutating the document from inside a closure, because the document is not
/// reachable from a `FnMut()` — it arrives in `handle_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Open,
    Library,
    RotateLeft,
    RotateRight,
    CropToView,
    Reset,
    Copy,
    Share,
    Save,
    More,
}

/// Order is stable for both the compact popup's keyboard traversal and its
/// semantic menu. The commands themselves remain the normal toolbar actions.
const COMPACT_MORE_ENTRIES: [Hit; 5] = [
    Hit::CropToView,
    Hit::Reset,
    Hit::Copy,
    Hit::Share,
    Hit::Save,
];

/// Controls inside the in-grid album editor. They are intentionally a small
/// local presentation state machine; every durable outcome is still one
/// `PhotosAction::SaveAlbum` action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlbumEditorHit {
    New,
    Edit,
    PreviousPage,
    NextPage,
    Save,
    Cancel,
    Delete,
    ConfirmDelete,
}

/// Controls in the photo-details sheet.  The fields themselves are shared
/// `TextInput`s; these are only the surrounding sheet actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataEditorHit {
    Save,
    Cancel,
    Organize,
    TimeAndPlace,
    ClearFolder,
    ClearPeople,
    ClearTags,
    ClearCapturedAt,
    ClearLocation,
    /// Set the draft location to the centre of the offline reference map.
    /// Pointer activation uses the exact point under the pointer instead.
    PlaceOnMap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum MetadataEditorSection {
    #[default]
    Organize,
    TimeAndPlace,
}

/// The local selection shelf. Selection is intentionally not a CRDT value:
/// collaborators should see the durable favourite/album result, never a
/// private cursor-like set that changes while somebody is deciding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionHit {
    Start,
    SelectAll,
    Clear,
    Favorite,
    EditDetails,
    ChooseAlbum,
    CancelDestinationChooser,
    CloseDestinationChooser,
    ApplyDestination,
    PreviousDestinationPage,
    NextDestinationPage,
    AddToAlbum,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimelineHit {
    ZoomOut,
    ZoomIn,
}

/// The library's three primary browse destinations. This is presentation-only
/// and deliberately separated from durable albums/metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryDestination {
    Timeline,
    Albums,
    People,
    Places,
    Analysis,
}

#[derive(Clone)]
struct PlaceItem {
    photo: LibraryPhoto,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PlacesHit {
    Timeline,
    Albums,
    People,
    Places,
    Analysis,
    ToggleList,
    ZoomIn,
    ZoomOut,
    /// A canonical place key, never a representative photo id.  A marker is
    /// a collection affordance and must not silently choose one photo from it.
    Place(String),
    /// A photo row inside an already-open place collection.
    Photo(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollectionPageHit {
    AlbumsPrevious,
    AlbumsNext,
    PeoplePrevious,
    PeopleNext,
    PlacesPrevious,
    PlacesNext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryStatusHit {
    Stop,
    Details,
    Dismiss,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PeopleSelection {
    Approved(String),
    Suggestion(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PeopleIntent {
    Back,
    ToggleRecognition,
    Scan,
    CancelScan,
    OpenApproved(String),
    OpenSuggestion(String),
    BrowseApproved,
    SaveName,
    BeginMerge,
    MergeApprovedInto(String),
    ApproveSuggestion,
    RejectSuggestion,
    HideSuggestion,
    RestoreSuggestion,
    MergeSuggestionWith(String),
    ToggleHidden,
    Page(bool),
    DeleteLocalData,
    ConfirmDeleteLocalData,
}

struct PeopleControl {
    intent: PeopleIntent,
    label: String,
    bounds: Rect,
    button: Button,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryAnalysisHit {
    Start,
    Pause,
    Resume,
    RetryFailures,
}

/// One chooser page always fits: six chips in a desktop row or a 3×2 compact
/// grid. The complete album collection remains available through paging.
const SELECTION_ALBUMS_PER_PAGE: usize = 6;

/// A compact materialised card.  The full library remains canonical in the
/// document; this carries just enough metadata to make the visible shelf
/// scannable without another document walk while painting.
#[derive(Clone)]
struct LibraryItem {
    id: String,
    thumbnail_key: String,
    name: String,
    favorite: bool,
    captured_at: String,
}

/// Presentation-only geometry for a stable justified row.
#[derive(Clone, Copy, Debug)]
struct TimelineItemLayout {
    width: f64,
    height: f64,
}

#[derive(Clone)]
struct MetadataFilterCard {
    folder: Option<String>,
    person: Option<String>,
    tag: Option<String>,
    /// A favourite view is a first-class, shared collection but selecting it
    /// remains local presentation state. Keep it in the same card model as
    /// the metadata filters so pointer, keyboard, and assistive technology
    /// all address the exact visible chip.
    favorites: bool,
    bounds: Rect,
}

/// How a toolbar item presents itself.
///
/// An [`Face::IconOnly`] button is not a mute one: the label rides along as
/// the tooltip, which [`ui::widget::Button`] also adopts as the accessible
/// name, so the glyph stays reachable by pointer-dwell and by screen reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// Glyph and words together.
    Labelled,
    /// A deliberately wide labelled overflow trigger used only for the
    /// large-text compact toolbar.  It keeps the word “More” visible instead
    /// of asking a pointer user to infer a horizontal-ellipsis glyph.
    LargeOverflow,
    /// Glyph alone.
    IconOnly,
}

/// What the canvas can honestly render right now. A shared document can name
/// an image before this device has received its content, so "no pixels yet"
/// is distinct from both an empty library and corrupt local bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CanvasState {
    #[default]
    Empty,
    Loading,
    Error,
    Ready,
}

/// A slot in the toolbar strip, in painting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// Index into [`PhotosApp::buttons`].
    Item(usize),
    /// A hairline dividing two groups.
    Separator,
    /// Greedy gap: soaks the leftover width so what follows sits hard right.
    Spacer,
}

/// The toolbar, in order: what opens a file, what adjusts it, what gets the
/// result out.
///
/// The four adjusters are glyph-only at every width. They are the operations
/// whose icons are unambiguous, and spelling all seven out is what pushed the
/// strip to a fixed 912 px — wider than a half-tiled window, which silently
/// clipped Save off the right-hand edge.
///
/// Crop and Save reuse the closest shared semantic glyphs (`Scissors` and
/// `Download`). Rotation uses distinct clockwise/counter-clockwise glyphs so
/// icon-only compact controls remain distinguishable without a tooltip.
const TOOLBAR: &[(Hit, &str, Icon, Face)] = &[
    (Hit::Open, "photos-open", Icon::Folder, Face::Labelled),
    (Hit::Library, "photos-library", Icon::Image, Face::Labelled),
    (
        Hit::RotateLeft,
        "photos-rotate-left",
        Icon::RotateCcw,
        Face::IconOnly,
    ),
    (
        Hit::RotateRight,
        "photos-rotate-right",
        Icon::RotateCw,
        Face::IconOnly,
    ),
    (
        Hit::CropToView,
        "photos-crop",
        Icon::Scissors,
        Face::IconOnly,
    ),
    (Hit::Reset, "photos-reset", Icon::Refresh, Face::IconOnly),
    (Hit::Copy, "photos-copy", Icon::Copy, Face::Labelled),
    (Hit::Share, "photos-share", Icon::Send, Face::Labelled),
    (Hit::Save, "photos-save", Icon::Download, Face::Labelled),
    (
        Hit::More,
        "photos-more",
        Icon::MoreHorizontal,
        Face::Labelled,
    ),
];

/// The strip's shape: Open, a rule, the adjusters, then everything that gets
/// the photograph out, pushed to the trailing edge.
const SLOTS: &[Slot] = &[
    Slot::Item(0),
    Slot::Item(1),
    Slot::Separator,
    Slot::Item(2),
    Slot::Item(3),
    Slot::Item(4),
    Slot::Item(5),
    Slot::Spacer,
    Slot::Item(6),
    Slot::Item(7),
    Slot::Item(8),
];

/// Phone chrome keeps the two navigation verbs and the two fast rotations
/// visible; less-frequent destructive/export operations live in an explicit
/// labeled More menu rather than becoming an indistinguishable icon run.
const COMPACT_SLOTS: &[Slot] = &[
    Slot::Item(0),
    Slot::Item(1),
    // At phone width the labelled Library and More controls already create
    // the useful groups; dropping the extra rule buys a real 24 px safety
    // margin, so neither edge control is clipped at 390 px.
    Slot::Item(2),
    Slot::Item(3),
    Slot::Spacer,
    Slot::Item(9),
];

/// Width a button needs to show `face` with `label`.
/// Width of one toolbar item whose capsule is `btn_h` tall ([`btn_h`] at the
/// current text scale; [`BTN_H`] at 1×). An icon-only item is square.
fn item_width(face: Face, label: &str, btn_h: f32) -> f32 {
    match face {
        Face::IconOnly => btn_h,
        Face::Labelled => {
            let text = label.chars().count() as f32 * APPROX_CHAR_W;
            (LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + text).max(btn_h)
        }
        // The normal estimate is calibrated for 100% text.  At the supported
        // 200% screenshot scale the overflow label needs a real word-sized
        // slot, while every other command is represented by an accessible
        // icon in the compact menu.
        Face::LargeOverflow => {
            let text = label.chars().count() as f32 * APPROX_CHAR_W * 2.0;
            (LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + text).max(btn_h)
        }
    }
}

/// Width the whole strip needs, `faces` giving each item's presentation.
///
/// Exposed so a test can assert the strip fits a window rather than
/// discovering it does not by finding Save missing from a screenshot.
pub fn toolbar_width(faces: &[Face]) -> f32 {
    toolbar_width_at(faces, BTN_H)
}

/// [`toolbar_width`] for capsules `btn_h` tall — the height `refit_toolbar`
/// builds the buttons with, so the fit decision uses the same widths the strip
/// will actually lay out at the theme's text scale.
fn toolbar_width_at(faces: &[Face], btn_h: f32) -> f32 {
    let mut w = PAD * 2.0;
    for (i, slot) in SLOTS.iter().enumerate() {
        if i > 0 {
            w += BTN_GAP;
        }
        w += match *slot {
            // Unreachable: `faces` and `TOOLBAR` are always built the same
            // length, one entry per `SLOTS::Item` index.
            Slot::Item(n) => match (faces.get(n), TOOLBAR.get(n)) {
                (Some(&face), Some(item)) => item_width(face, &tr(item.1), btn_h),
                _ => 0.0,
            },
            Slot::Separator => SEP_W,
            Slot::Spacer => 0.0,
        };
    }
    w
}

/// Each item's presentation at a given strip width, for capsules `btn_h` tall.
///
/// The labelled items give up their words rather than the strip giving up an
/// item: a control that has shrunk to its glyph is still reachable, one that
/// has fallen off the edge is not.
fn faces_for_width(width: f32, btn_h: f32) -> Vec<Face> {
    let roomy: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
    // Width 0 is the first frame, before a layout has happened. Assume room;
    // the next frame corrects it with a real measurement.
    if width <= 0.0 || toolbar_width_at(&roomy, btn_h) <= width {
        return roomy;
    }
    if width < 600.0 {
        return TOOLBAR
            .iter()
            .enumerate()
            .map(|(index, _)| {
                // Open keeps its tooltip and accessible name when compact;
                // spending that width on the explicit More label is the
                // clearer trade on a 390 px phone.
                if index == 9 || (width >= 380.0 && index == 1) {
                    Face::Labelled
                } else {
                    Face::IconOnly
                }
            })
            .collect();
    }
    TOOLBAR.iter().map(|_| Face::IconOnly).collect()
}

/// Authored content observed by local transient work, excluding operational metadata.
#[derive(Clone, PartialEq)]
struct LiveContent {
    image: Option<OpenImage>,
    edits: Edits,
    library: crate::model::PhotoLibrary,
}
impl LiveContent {
    fn read(doc: &AutoCommit) -> Self {
        Self {
            image: PhotosDoc::image(doc),
            edits: PhotosDoc::edits(doc),
            library: PhotosDoc::library(doc),
        }
    }
}

/// The rendered image, and the inputs it was rendered from.
///
/// Keeping the inputs beside the output is what lets `sync` decide in a couple
/// of comparisons whether anything needs redoing.
struct Rendered {
    identity: String,
    edits: Edits,
    /// The edited pixels, kept so Copy and Save export exactly what is shown
    /// rather than re-running the pipeline and hoping it agrees.
    rgba: Rgba,
}

/// The compact, explicitly shared context shown beside an open photograph.
///
/// This is a projection of the library record, not a second metadata store:
/// it gives the viewer useful orientation without turning a filename or a
/// device-local EXIF read into collaboration state.  The persistent editor
/// remains the library's canonical metadata action surface.
#[derive(Clone, Debug)]
struct ViewerDetails {
    name: String,
    dimensions: String,
    edited: bool,
    favorite: bool,
    description: Option<String>,
    captured_at: Option<String>,
    place: Option<String>,
    people: Vec<String>,
    tags: Vec<String>,
    folder: Option<String>,
}

fn viewer_copy(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let clipped: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "library mode and the three independent transient album-editor flags have distinct event and reconciliation lifecycles"
)]
pub struct PhotosApp {
    /// Canonical dispatcher shared by toolbar edits and exported document actions.
    actions: crate::resource::ActionSink,
    /// Where imported originals are kept and photographs are read from. The
    /// document names content by hash; this handle resolves it.
    content: ContentStore,
    /// Test-only evidence that each visible batch route creates the exact
    /// envelope the corresponding published action descriptor creates.
    #[cfg(test)]
    last_library_action: Option<tilo_actions::EncodedAction>,
    /// Decoded original, before any edit. `None` until a file opens.
    original: Option<(String, Rgba)>,
    original_identity: Option<String>,
    observed_content: Option<LiveContent>,
    import_content: Option<LiveContent>,
    picker_stale: bool,
    rendered: Option<Rendered>,
    viewer: Option<ImageViewer>,
    /// Bounded presentation projection for the open-view information card.
    /// It is refreshed from the document and never written back from paint.
    viewer_details: Option<ViewerDetails>,
    buttons: Vec<(Hit, Button)>,
    pending: Rc<Cell<Option<Hit>>>,
    /// Where the last export went, or why it failed. Shown in the status line
    /// so an action is never silent.
    notice: Option<String>,
    /// The status line, recomputed in `sync` — `draw` has no document.
    status: String,
    rect: Rect,
    toolbar_rect: Rect,
    canvas_rect: Rect,
    status_rect: Rect,
    /// Where each group rule landed, filled in by `apply_layout` so `draw`
    /// paints them without re-deriving the strip's geometry.
    separator_rects: Vec<Rect>,
    /// Presentation of each toolbar item at the width last laid out.
    faces: Vec<Face>,
    /// Kept local: text preference changes presentation, never document
    /// contents or another collaborator's toolbar.
    large_text_toolbar: bool,
    /// Capsule height the toolbar was last built for ([`btn_h`] at the
    /// theme's text scale). A change rebuilds the strip like a face change.
    toolbar_btn_h: f32,
    compact_more_open: bool,
    compact_more_focus: usize,
    compact_more_hits: Vec<(Hit, Rect)>,
    /// Portal channel while a file chooser is open.
    picker: Option<ui::portal::PortalChannel>,
    /// The chooser that delivers chosen files' contents, where files have no
    /// path (a browser page).
    contents_picker: Option<ui::portal::ContentsChannel>,
    directory_picker: Option<ui::portal::PortalChannel>,
    directory_picker_retain_exif_location: bool,
    directory_import: Option<DirectoryImport>,
    /// Last completed local job remains visible until the next one begins.
    /// Paths stay device-local and are reduced to file names in presentation.
    directory_import_receipt: Option<DirectoryImportReceipt>,
    directory_import_details_open: bool,
    directory_import_details_page: usize,
    directory_import_details_layout: Option<ModalLayout>,
    directory_import_pending: Rc<Cell<Option<DirectoryStatusHit>>>,
    directory_import_buttons: Vec<(DirectoryStatusHit, Button)>,
    directory_import_progress: ProgressBar,
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    pending_resource_open: Option<(tilo_actions::InvocationId, String)>,
    analysis_tx: Option<SyncSender<AnalysisJob>>,
    analysis_rx: Receiver<AnalysisResult>,
    active_analysis: HashSet<tilo_actions::InvocationId>,
    cancelled_analysis: HashSet<tilo_actions::InvocationId>,
    active_people_scans: HashSet<tilo_actions::InvocationId>,
    pending_people_scans: HashMap<tilo_actions::InvocationId, PendingPeopleScan>,
    library_analysis: crate::library_analysis::LibraryAnalysisState,
    library_analysis_capability: Option<tilo_vision::LocalVisionCapability>,
    library_analysis_pending: Rc<Cell<Option<LibraryAnalysisHit>>>,
    library_analysis_buttons: Vec<(LibraryAnalysisHit, Button)>,
    library_analysis_progress: ProgressBar,
    library_analysis_failure_hits: Vec<(String, Rect)>,
    /// (shared analyzed, locally available unprocessed, awaiting local bytes)
    /// over the complete library, never the current visual filter.
    library_analysis_coverage: (usize, usize, usize),
    /// Private People-review state. It is intentionally neither a document
    /// field nor a resource: unconfirmed face proposals belong only on this
    /// device until an explicit review commits a compact label membership.
    people_controller: crate::people::PeopleController,
    /// Next library offset for the explicitly started People scan.  Keeping a
    /// cursor makes a large local library progress in 12-photo work units
    /// without ever submitting hundreds of biometric previews at once.
    people_scan_progress: PeopleScanProgress,
    /// A distinct People collection surface. Its private suggestion state is
    /// visually and semantically separated from shared approved labels.
    people_mode: bool,
    people_selection: Option<PeopleSelection>,
    /// The person review sheet enters with the shared motion primitive.  The
    /// renderer snaps it open when reduced motion is requested.
    people_detail_transition: Transition,
    people_merge_mode: bool,
    people_show_hidden: bool,
    people_review_page: usize,
    people_approved: Vec<crate::people::SharedPerson>,
    people_delete_confirm: bool,
    people_name_input: TextInput,
    people_name_bounds: Rect,
    people_controls: Vec<PeopleControl>,
    people_control_signature: String,
    people_pending: Rc<RefCell<Option<PeopleIntent>>>,
    canvas_state: CanvasState,
    search_provider: crate::search::PhotosSearchProvider,
    /// Heads that produced the cached immutable/Arc-backed search catalogue.
    /// An unchanged `sync` must not rebuild a full-library index.
    search_heads: Vec<automerge::ChangeHash>,
    /// A rejected index generation never erases the last-good provider. This
    /// bounded diagnostic is visible and exported until a later refresh
    /// succeeds.
    search_refresh_error: Option<String>,
    #[cfg(test)]
    force_search_refresh_error: bool,
    /// The library is deliberately a bounded presentation slice.  The CRDT
    /// can hold considerably more entries than a window can paint or an
    /// assistive technology should receive in one update.
    library_items: Vec<LibraryItem>,
    library_total: usize,
    /// A local query over the current library projection.  The intelligence
    /// owner can extend this predicate with OCR and descriptions without
    /// changing the shared `SearchField` interaction contract.
    library_search: SearchField,
    library_query: String,
    /// Incremental results from the retained full-text catalogue. A broad
    /// query advances by one bounded candidate page per sync; paint and
    /// filtering never scan or normalize the whole corpus.
    library_query_indexed: String,
    library_query_generation: u64,
    library_query_cursor: Option<ui::search::SearchCursor>,
    library_query_matches: HashSet<String>,
    library_query_complete: bool,
    library_destination: LibraryDestination,
    places_list_mode: bool,
    places_viewport: crate::places::MapViewport,
    /// The exact canonical place collection currently opened from a resource,
    /// map marker, keyboard, or accessibility action. Presentation-only.
    places_open_key: Option<String>,
    places_items: Vec<PlaceItem>,
    places_page: usize,
    places_hits: Vec<(PlacesHit, Rect)>,
    collection_page_hits: Vec<(CollectionPageHit, Rect, bool)>,
    collection_page_buttons: Vec<(CollectionPageHit, Button)>,
    collection_page_pending: Rc<Cell<Option<CollectionPageHit>>>,
    places_map_bounds: Rect,
    places_drag_origin: Option<Point>,
    /// A short local response to a map pan or zoom. It deliberately animates
    /// only presentation geometry—camera position itself stays immediate so
    /// keyboard, pointer, agents, and screen readers all observe the same
    /// completed operation at once.
    places_camera_feedback: Option<Animated<f64>>,
    places_motion_reduced: bool,
    /// Count for the always-addressable Favorites virtual collection. This is
    /// a scalar projection of the shared library, so paint never walks the
    /// document and the chip stays bounded even for a very large collection.
    library_favorite_count: usize,
    library_offset: usize,
    library_albums: Vec<(String, String, usize)>,
    /// At most four stable member ids per album for the bounded cover mosaic.
    library_album_covers: HashMap<String, Vec<String>>,
    albums_page: usize,
    active_album: Option<String>,
    /// Metadata browse filters originate from canonical folder/person
    /// resources. They are local presentation state, just like an album view;
    /// changing a filter must never rewrite another collaborator's metadata.
    active_folder: Option<String>,
    active_person: Option<String>,
    /// Tag filtering is local navigation derived from a durable tag resource.
    /// It never mutates a collaborator's metadata just by opening a view.
    active_tag: Option<String>,
    /// AI-derived object/pet collections use the same local-only navigation
    /// rule as tags. Their labels are present only after explicit sharing.
    active_object: Option<String>,
    active_pet: Option<String>,
    /// The stable Favorites resource is a view over shared favourite markers,
    /// never a private second list. Opening it is local; toggling a star is a
    /// typed shared document action.
    active_favorites: bool,
    /// Bounded labels used for discoverable visual filtering. Search and the
    /// resource tree still resolve every stable id outside this shelf.
    library_folders: Vec<(String, usize)>,
    library_people: Vec<(String, usize)>,
    people_page: usize,
    /// One bounded representative image per approved person, selected from
    /// the shared metadata projection and rendered from the existing cache.
    library_person_covers: HashMap<String, String>,
    library_tags: Vec<(String, usize)>,
    metadata_filter_cards: Vec<MetadataFilterCard>,
    /// Visible paging affordances for a one-card enlarged-text shelf. These
    /// are local navigation only; their stable semantic targets let keyboard
    /// and assistive-technology users reach the same bounded slice.
    library_page_hits: Vec<(bool, bool, Rect)>,
    timeline_density_hits: Vec<(TimelineHit, Rect)>,
    timeline_density_buttons: Vec<(TimelineHit, Button)>,
    timeline_density_pending: Rc<Cell<Option<TimelineHit>>>,
    /// Retained shared controls over the bespoke timeline surface.  The
    /// date/photo canvas owns its rhythm; header actions still use toolkit
    /// button states, focus treatment, and accessible names.
    library_select_button: Button,
    library_album_buttons: Vec<(AlbumEditorHit, Button)>,
    /// Number of visual density steps, from spacious (0) to compact (4).
    /// This remains local UI state: collaborators should never change a
    /// person's preferred thumbnail size.
    timeline_density: u8,
    /// Exact variable-height chronology coordinate.
    timeline_scroll_y: f64,
    timeline_projection: TimelineProjection,
    timeline_projection_heads: Vec<automerge::ChangeHash>,
    timeline_projection_filter: String,
    timeline_viewport_height: f64,
    /// Shared chronological navigation face.  It owns no library data: the
    /// current row offset/density remain private presentation state, while
    /// date boundaries are a bounded projection of canonical photo metadata.
    timeline_index: DateIndexedScrollbar,
    timeline_index_markers: Vec<DateIndexMarker>,
    timeline_index_metrics: ScrollMetrics,
    library_details_cards: Vec<(String, Rect)>,
    album_cards: Vec<(Option<String>, Rect)>,
    album_edit_cards: Vec<(String, Rect)>,
    album_editor: bool,
    album_editor_id: Option<String>,
    album_editor_creating: bool,
    album_name_input: TextInput,
    album_editor_members: HashSet<String>,
    album_delete_confirm: bool,
    /// Tab order inside the transient sheet: name, cancel, delete (when
    /// editing), save. The `TextInput` remains the actual focused widget.
    album_editor_focus: usize,
    album_editor_rect: Rect,
    album_editor_hits: Vec<(AlbumEditorHit, Rect)>,
    /// Shared, retained controls for moving through the local library shelf
    /// while selecting album membership. They never write the document.
    album_page_buttons: Vec<(AlbumEditorHit, Button)>,
    album_page_pending: Rc<Cell<Option<AlbumEditorHit>>>,
    metadata_editor: bool,
    metadata_editor_ids: Vec<String>,
    metadata_folder_input: TextInput,
    metadata_people_input: TextInput,
    metadata_tags_input: TextInput,
    metadata_captured_at_input: TextInput,
    metadata_place_input: TextInput,
    metadata_latitude_input: TextInput,
    metadata_longitude_input: TextInput,
    metadata_location_map: crate::places::MapViewport,
    metadata_location_map_bounds: Rect,
    /// A draft-only provenance choice. It becomes durable only on Save, and
    /// lets collaborators distinguish deliberate map placement from typing.
    metadata_location_from_map: bool,
    /// Whether each field started with multiple values in a multi-photo edit.
    /// An untouched empty mixed field means Keep, never Clear.
    metadata_mixed: [bool; 5],
    /// A visible Clear command resolves the otherwise ambiguous empty mixed
    /// field without magic sentinel text.
    metadata_force_clear: [bool; 5],
    metadata_editor_section: MetadataEditorSection,
    /// Tab order within the active section plus Cancel and Save.
    metadata_editor_focus: usize,
    metadata_editor_hits: Vec<(MetadataEditorHit, Rect)>,
    /// Scroll offset of the sheet's field list. The sheet is capped at the
    /// viewport and its footer is pinned, so when the fields need more room
    /// than that leaves (200% text on an 800×900 window) they scroll under
    /// the footer instead of the footer painting over the last field.
    metadata_editor_scroll: f64,
    /// The field list's visible region from the last paint — where wheel
    /// events scroll and outside which presses are not forwarded to a
    /// scrolled-away input.
    metadata_editor_body: Rect,
    /// Set when keyboard focus moves; the next paint scrolls that field into
    /// view.
    metadata_editor_reveal_focus: bool,
    /// The metadata sheet is laid out over the media canvas, but its command
    /// controls are still real toolkit buttons. Keeping them retained gives
    /// hover/press/focus treatment, native action publication and keyboard
    /// activation to the same controls a pointer user sees.
    metadata_editor_buttons: Vec<(MetadataEditorHit, Button)>,
    metadata_editor_pending: Rc<Cell<Option<MetadataEditorHit>>>,
    /// At most two viewportfuls of decoded thumbnails.  Originals are never
    /// scanned at draw time, and a large library cannot turn a frame into a
    /// disk-I/O loop.
    library_thumbnails: HashMap<String, ImageData>,
    thumbnail_resource_keys: HashMap<String, String>,
    /// The visible entries whose bytes were unavailable or invalid in the
    /// current document generation. Keeping this negative cache avoids
    /// decoding the same corrupt peer payload every frame; it is cleared when
    /// shared metadata changes so a fresh content reference gets a retry.
    thumbnail_unavailable: HashSet<String>,
    /// A bad peer-provided image must not be decoded once per frame. Content
    /// hashes are immutable, and a user reopening a changed legacy path makes
    /// a new attempt through `open_path`, so retaining this failure is safe.
    original_decode_failed: Option<String>,
    /// The shared preview pipeline; its one worker serializes disk and
    /// content-store access. Results are stamped with a visible-library
    /// generation before they may enter the UI cache, so an old job can never
    /// paint a card that now names another photograph.
    thumbnail_service: thumbnail::ThumbnailService,
    /// The pipeline key of every queued preview, by thumbnail id.
    thumbnail_keys: HashMap<String, thumbnail::ThumbKey>,
    /// `None` is a deliberately terminal generation. Counter exhaustion is
    /// practically unreachable, but reusing zero could admit an ancient
    /// queued result, so the cache fails closed instead.
    thumbnail_generation: Option<u64>,
    thumbnail_visible_ids: Vec<String>,
    thumbnail_queued: HashSet<String>,
    /// A destructive library removal takes two deliberate activations. The
    /// stable photo id, not a card index, keeps confirmation correct if a
    /// collaborator changes the grid between clicks.
    library_remove_confirm: Option<String>,
    /// Local selection is an affordance for applying one canonical batch
    /// action. It is bounded to cards the current view has materialized.
    selection_mode: bool,
    selected_photo_ids: HashSet<String>,
    selection_anchor: Option<String>,
    /// The destination is an explicit local choice, never inferred from the
    /// album currently being browsed (which would commonly be a no-op).
    selection_destination_album: Option<String>,
    selection_album_chooser: bool,
    /// The chooser pages a bounded materialised slice rather than silently
    /// clipping albums beyond the available shelf height.
    selection_album_page: usize,
    /// Projection refreshed from the shared library; it lets the visible
    /// batch control say Favorite or Unfavorite truthfully without storing
    /// selection state in the document.
    selection_all_favorite: bool,
    selection_hits: Vec<(SelectionHit, Rect)>,
    selection_destination_hits: Vec<(String, Rect)>,
    /// Stable-id hit targets for the currently materialised grid cards.
    library_cards: Vec<(String, Rect, bool)>,
    pointer: Point,
    library_mode: bool,
}

struct DirectoryImport {
    receiver: Receiver<DirectoryImportEvent>,
    cancel: Arc<AtomicBool>,
    invocation: Option<tilo_actions::InvocationId>,
    mode: DirectoryImportMode,
    discovered: usize,
    imported: usize,
    refreshed: usize,
    unchanged: usize,
    skipped: usize,
    sequence: u64,
    pending: VecDeque<crate::import::IndexedPhoto>,
    failures: Vec<crate::import::ImportFailure>,
    worker_done: bool,
    cancelled: bool,
    limit_reached: bool,
    produced_resources: Vec<String>,
    /// Keeps deterministic offscreen progress fixtures alive. Real jobs own
    /// their sender on the worker and always leave this empty.
    _shot_sender: Option<std::sync::mpsc::Sender<DirectoryImportEvent>>,
}

enum DirectoryImportEvent {
    Progress(crate::import::ImportProgress),
    Finished(Result<crate::import::ImportReport, String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryImportMode {
    Import,
    Reindex,
}

#[derive(Clone, Debug)]
struct DirectoryImportReceipt {
    mode: DirectoryImportMode,
    discovered: usize,
    imported: usize,
    refreshed: usize,
    unchanged: usize,
    failures: Vec<crate::import::ImportFailure>,
    failure_count: usize,
    cancelled: bool,
    limit_reached: bool,
}

impl Default for PhotosApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotosApp {
    fn discover_library_analysis_capability() -> Result<tilo_vision::LocalVisionCapability, String>
    {
        tilo_vision::discover_local_capability(&agent_provider_config::ProviderConfig::load())
            .ok_or_else(|| {
                "No verified local image-analysis model is ready. Install and start one in Settings."
                    .to_owned()
            })
    }

    fn begin_library_analysis(
        &mut self,
        invocation: tilo_actions::InvocationId,
        doc: &AutoCommit,
        retry_failures: bool,
    ) -> Result<(), String> {
        if self.library_analysis.phase == crate::library_analysis::LibraryAnalysisPhase::Running {
            return Err("Photo analysis is already running on this device".to_owned());
        }
        let capability = Self::discover_library_analysis_capability()?;
        if retry_failures {
            if !self.library_analysis.begin_retry(
                invocation,
                capability.model_label.clone(),
                capability.revision.clone(),
            ) {
                return Err("There are no failed photographs to retry".to_owned());
            }
        } else {
            let library = PhotosDoc::library(doc);
            let mut unavailable = 0;
            let targets = library
                .photos
                .into_iter()
                .filter(|photo| {
                    !photo.analysis.as_ref().is_some_and(|analysis| {
                        analysis.has_vision_component(
                            photo.image.content.as_deref(),
                            &capability.revision,
                        )
                    })
                })
                .filter_map(|photo| {
                    if photo.image.content.is_some()
                        && crate::doc::bytes_of(&photo.image, &self.content).is_some()
                    {
                        Some(photo.id)
                    } else {
                        unavailable += 1;
                        None
                    }
                })
                .collect::<Vec<_>>();
            self.library_analysis.begin(
                invocation,
                targets,
                unavailable,
                capability.model_label.clone(),
                capability.revision.clone(),
            );
        }
        self.library_analysis_capability = Some(capability);
        self.active_analysis.insert(invocation);
        self.schedule_next_library_analysis(doc)?;
        Ok(())
    }

    fn resume_library_analysis(
        &mut self,
        invocation: tilo_actions::InvocationId,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        let capability = Self::discover_library_analysis_capability()?;
        if self.library_analysis.model_revision.as_deref() != Some(capability.revision.as_str()) {
            return Err(
                "The local model changed while analysis was paused. Start a new scan so provenance stays consistent."
                    .to_owned(),
            );
        }
        if !self.library_analysis.resume(invocation) {
            return Err("Photo analysis is not paused".to_owned());
        }
        self.library_analysis_capability = Some(capability);
        self.active_analysis.insert(invocation);
        self.schedule_next_library_analysis(doc)?;
        Ok(())
    }

    fn pause_library_analysis(&mut self) -> bool {
        let invocation = self.library_analysis.invocation;
        let paused = self.library_analysis.pause();
        if paused {
            if let Some(invocation) = invocation {
                self.active_analysis.remove(&invocation);
            }
            self.notice = Some(
                "Photo analysis will pause after the photograph already being processed".to_owned(),
            );
        }
        paused
    }

    fn activate_library_analysis(&mut self, hit: LibraryAnalysisHit, doc: &AutoCommit) -> bool {
        let operation = match hit {
            LibraryAnalysisHit::Start => crate::actions::PhotosOperation::AnalyzeLibrary,
            LibraryAnalysisHit::Pause => crate::actions::PhotosOperation::PauseLibraryAnalysis,
            LibraryAnalysisHit::Resume => crate::actions::PhotosOperation::ResumeLibraryAnalysis,
            LibraryAnalysisHit::RetryFailures => {
                crate::actions::PhotosOperation::RetryLibraryAnalysisFailures
            }
        };
        let Ok(encoded) = tilo_actions::EncodedAction::encode_operation(
            crate::actions::PHOTOS_OPERATION_TYPE,
            &operation,
        ) else {
            self.notice = Some("Photo analysis action could not be prepared".to_owned());
            return false;
        };
        let _ = &encoded;
        #[cfg(test)]
        {
            self.last_library_action = Some(encoded);
        }
        let invocation = tilo_actions::InvocationId::new();
        let result = match hit {
            LibraryAnalysisHit::Start => self.begin_library_analysis(invocation, doc, false),
            LibraryAnalysisHit::Pause => self
                .pause_library_analysis()
                .then_some(())
                .ok_or_else(|| "Photo analysis is not running".to_owned()),
            LibraryAnalysisHit::Resume => self.resume_library_analysis(invocation, doc),
            LibraryAnalysisHit::RetryFailures => self.begin_library_analysis(invocation, doc, true),
        };
        if let Err(error) = result {
            self.notice = Some(error);
            return false;
        }
        true
    }

    fn schedule_next_library_analysis(&mut self, doc: &AutoCommit) -> Result<(), String> {
        loop {
            let Some(photo_id) = self
                .library_analysis
                .take_next(|id| PhotosDoc::photo(doc, id).map(|photo| photo.image.name))
            else {
                if self.library_analysis.finish_if_empty()
                    && let Some(invocation) = self.library_analysis.invocation
                {
                    self.active_analysis.remove(&invocation);
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::json!({
                                "analyzed": self.library_analysis.succeeded,
                                "failed": self.library_analysis.failed,
                                "unavailable": self.library_analysis.unavailable,
                            })),
                            resources: vec!["photos.library-analysis".to_owned()],
                        },
                    });
                    self.notice = Some(format!(
                        "Photo analysis finished: {} analyzed, {} failed",
                        self.library_analysis.succeeded, self.library_analysis.failed
                    ));
                }
                return Ok(());
            };
            let Some(photo) = PhotosDoc::photo(doc, &photo_id) else {
                let _ = self.library_analysis.record_failure(
                    crate::library_analysis::LibraryAnalysisFailure {
                        photo_id: photo_id.clone(),
                        name: photo_id,
                        message: "The photograph was removed before analysis started".to_owned(),
                    },
                );
                continue;
            };
            let capability = self.library_analysis_capability.clone().ok_or_else(|| {
                "The verified local image-analysis model is no longer available".to_owned()
            })?;
            let invocation = self
                .library_analysis
                .invocation
                .ok_or_else(|| "Photo analysis lost its invocation identity".to_owned())?;
            let sender = self
                .analysis_tx
                .as_ref()
                .ok_or_else(|| "local image analysis is unavailable on this device".to_owned())?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id,
                    content: photo.image.content.clone(),
                    image: photo.image,
                    kind: AnalysisJobKind::Vision { capability },
                })
                .map_err(|_| "local image analysis worker is busy; retry shortly".to_owned())?;
            return Ok(());
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each analysis outcome variant is reconciled against the current document and library-analysis progress in one exhaustive match"
    )]
    fn drain_library_analysis_result(&mut self, completed: AnalysisResult, doc: &mut AutoCommit) {
        let photo_name = self
            .library_analysis
            .current_name
            .clone()
            .unwrap_or_else(|| completed.photo_id.clone());
        let stale = PhotosDoc::photo(doc, &completed.photo_id)
            .as_ref()
            .is_none_or(|photo| photo.image.content != completed.content);
        let item_result = match completed.result {
            AnalysisOutcome::Image {
                component: ImageAnalysisComponent::Vision,
                result: Ok(mut analysis),
            } if !stale => {
                mark_analysis_shared(&mut analysis);
                let action = crate::actions::PhotosAction::MergeImageAnalysis {
                    photo_id: completed.photo_id.clone(),
                    analysis,
                };
                self.actions
                    .try_dispatch_with_context(
                        doc,
                        crate::actions::PHOTOS_ACTION_TYPE,
                        &action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.photos",
                            "Photos",
                            &action,
                        ),
                    )
                    .map_err(|error| error.to_string())
            }
            AnalysisOutcome::Image {
                component: ImageAnalysisComponent::Ocr,
                ..
            } => {
                Err("The image-analysis worker returned an OCR result for a vision scan".to_owned())
            }
            AnalysisOutcome::Image { result: Ok(_), .. } => {
                Err("The photograph changed before local analysis finished".to_owned())
            }
            AnalysisOutcome::Image {
                result: Err(error), ..
            } => Err(error),
            AnalysisOutcome::People(_) => {
                Err("The image-analysis worker returned the wrong result kind".to_owned())
            }
        };
        match item_result {
            Ok(()) => {
                let _ = self.library_analysis.record_success(&completed.photo_id);
            }
            Err(message) => {
                let _ = self.library_analysis.record_failure(
                    crate::library_analysis::LibraryAnalysisFailure {
                        photo_id: completed.photo_id,
                        name: photo_name,
                        message,
                    },
                );
            }
        }
        if let Some(invocation) = self.library_analysis.invocation {
            if self.library_analysis.phase
                == crate::library_analysis::LibraryAnalysisPhase::Complete
            {
                self.active_analysis.remove(&invocation);
                self.resource_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "analyzed": self.library_analysis.succeeded,
                            "failed": self.library_analysis.failed,
                            "unavailable": self.library_analysis.unavailable,
                        })),
                        resources: vec!["photos.library-analysis".to_owned()],
                    },
                });
                self.notice = Some(format!(
                    "Photo analysis finished: {} analyzed, {} failed",
                    self.library_analysis.succeeded, self.library_analysis.failed
                ));
            } else {
                self.resource_result = Some(tilo_actions::InvocationUpdate::Progress {
                    invocation,
                    sequence: u64::try_from(self.library_analysis.completed).unwrap_or(u64::MAX),
                    phase: if self.library_analysis.phase
                        == crate::library_analysis::LibraryAnalysisPhase::Paused
                    {
                        "paused".to_owned()
                    } else {
                        "analyzing".to_owned()
                    },
                    completed: Some(
                        u64::try_from(self.library_analysis.completed).unwrap_or(u64::MAX),
                    ),
                    total: Some(u64::try_from(self.library_analysis.total).unwrap_or(u64::MAX)),
                    message: Some(format!(
                        "{} analyzed, {} failed",
                        self.library_analysis.succeeded, self.library_analysis.failed
                    )),
                });
            }
        }
        if self.library_analysis.phase == crate::library_analysis::LibraryAnalysisPhase::Running
            && let Err(error) = self.schedule_next_library_analysis(doc)
        {
            self.library_analysis.pause();
            self.notice = Some(format!("Photo analysis paused: {error}"));
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one drain loop applies every completed analysis job kind with the same stale/cancelled checks"
    )]
    fn drain_analysis_results(&mut self, doc: &mut AutoCommit) {
        while let Ok(completed) = self.analysis_rx.try_recv() {
            if self.library_analysis.invocation == Some(completed.invocation) {
                self.drain_library_analysis_result(completed, doc);
                continue;
            }
            self.active_analysis.remove(&completed.invocation);
            self.active_people_scans.remove(&completed.invocation);
            let pending_people_scan = self.pending_people_scans.remove(&completed.invocation);
            if self.cancelled_analysis.remove(&completed.invocation) {
                continue;
            }
            let stale = matches!(&completed.result, AnalysisOutcome::Image { .. })
                && PhotosDoc::photo(doc, &completed.photo_id)
                    .as_ref()
                    .is_none_or(|photo| photo.image.content != completed.content);
            let update = if stale {
                tilo_actions::InvocationUpdate::Failed {
                    invocation: completed.invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.analysis_stale".into(),
                        message:
                            "This photograph changed before local analysis finished. Retry it."
                                .into(),
                        details: None,
                        retryable: true,
                    },
                }
            } else {
                match completed.result {
                    AnalysisOutcome::Image {
                        component: _,
                        result: Ok(mut analysis),
                    } => {
                        mark_analysis_shared(&mut analysis);
                        let action = crate::actions::PhotosAction::MergeImageAnalysis {
                            photo_id: completed.photo_id,
                            analysis,
                        };
                        match self.actions.try_dispatch_with_context(
                            doc,
                            crate::actions::PHOTOS_ACTION_TYPE,
                            &action,
                            tilo_actions::ActionContext::for_action(
                                "so.tilo.photos",
                                "Photos",
                                &action,
                            ),
                        ) {
                            Ok(()) => {
                                self.notice = Some(
                                    "Local image analysis was shared with this photo library"
                                        .to_owned(),
                                );
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation: completed.invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                            Err(error) => analysis_failure(completed.invocation, error.to_string()),
                        }
                    }
                    AnalysisOutcome::Image {
                        result: Err(error), ..
                    } => analysis_failure(completed.invocation, error),
                    AnalysisOutcome::People(Err(error)) => {
                        self.people_scan_progress.record_failure(error.clone());
                        self.people_scan_progress.save();
                        analysis_failure(completed.invocation, error)
                    }
                    AnalysisOutcome::People(Ok(batch)) => {
                        let added = self.people_controller.ingest_verified_face_clusters(batch);
                        if let Some(pending) = pending_people_scan {
                            self.people_scan_progress.advance_after_success(
                                pending.start,
                                pending.scanned,
                                pending.total,
                            );
                            self.people_scan_progress.save();
                            self.notice = Some(if added == 0 {
                                "No new local face suggestions were found in this batch".to_owned()
                            } else {
                                format!(
                                    "{added} local People suggestion{} ready to review",
                                    if added == 1 { " is" } else { "s are" }
                                )
                            });
                            if self.people_scan_progress.next_offset != 0 {
                                if let Err(error) =
                                    self.schedule_people_scan_batch(completed.invocation, doc)
                                {
                                    self.people_scan_progress.record_failure(error.clone());
                                    self.people_scan_progress.save();
                                    analysis_failure(completed.invocation, error)
                                } else {
                                    tilo_actions::InvocationUpdate::Progress {
                                        invocation: completed.invocation,
                                        sequence: self.people_scan_progress.completed_batches,
                                        phase: "scanning".to_owned(),
                                        completed: Some(
                                            u64::try_from(self.people_scan_progress.next_offset)
                                                .unwrap_or(u64::MAX),
                                        ),
                                        total: Some(
                                            u64::try_from(
                                                self.people_scan_progress.last_library_size,
                                            )
                                            .unwrap_or(u64::MAX),
                                        ),
                                        message: Some("Scanning the next local batch".to_owned()),
                                    }
                                }
                            } else {
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation: completed.invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                        } else {
                            self.people_scan_progress.record_failure(
                                "People scan result lost its local checkpoint; retry this batch"
                                    .to_owned(),
                            );
                            self.people_scan_progress.save();
                            analysis_failure(
                                completed.invocation,
                                "People scan result lost its local checkpoint; retry this batch"
                                    .to_owned(),
                            )
                        }
                    }
                }
            };
            self.resource_result = Some(update);
        }
    }

    fn toolbar_slots(&self) -> &'static [Slot] {
        if self.large_text_toolbar || (self.rect.width() > 0.0 && self.rect.width() < 600.0) {
            COMPACT_SLOTS
        } else {
            SLOTS
        }
    }

    /// An app whose content store lives in memory with it: tests and offscreen
    /// shots. The runtime uses [`Self::new_persistent`].
    pub fn new() -> Self {
        Self::with_content(ContentStore::memory())
    }

    /// An app keeping imported originals in, and reading photographs from,
    /// `content`. The handle is shared with the app's background workers.
    #[allow(
        clippy::too_many_lines,
        reason = "construction lists every app-owned state field explicitly so local, shared, and transient state remain auditable"
    )]
    pub fn with_content(content: ContentStore) -> Self {
        let (analysis_job_tx, analysis_job_rx) =
            std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        let (analysis_result_tx, analysis_rx) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        let analysis_tx = ui::rt::spawn_thread("photos-local-analysis", {
            let content = content.clone();
            move || analysis_worker(&analysis_job_rx, &analysis_result_tx, &content)
        })
        .ok()
        .map(|_| analysis_job_tx);
        // In memory until `new_persistent` swaps in the per-user preview cache:
        // tests and offscreen shots must not write the user's cache.
        let thumbnail_service = with_thumbnail_wake(thumbnail::ThumbnailService::ephemeral(1));
        let pending: Rc<Cell<Option<Hit>>> = Rc::new(Cell::new(None));
        let album_page_pending: Rc<Cell<Option<AlbumEditorHit>>> = Rc::new(Cell::new(None));
        let metadata_editor_pending: Rc<Cell<Option<MetadataEditorHit>>> = Rc::new(Cell::new(None));
        let timeline_density_pending: Rc<Cell<Option<TimelineHit>>> = Rc::new(Cell::new(None));
        let collection_page_pending: Rc<Cell<Option<CollectionPageHit>>> = Rc::new(Cell::new(None));
        let people_pending = Rc::new(RefCell::new(None));
        let library_analysis_pending: Rc<Cell<Option<LibraryAnalysisHit>>> =
            Rc::new(Cell::new(None));
        let directory_import_pending: Rc<Cell<Option<DirectoryStatusHit>>> =
            Rc::new(Cell::new(None));
        let faces: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
        let mut buttons = Vec::new();
        for ((hit, key, icon, _), face) in TOOLBAR.iter().zip(faces.iter()) {
            // 1× capsules; the first `build_layout` refits them to the theme.
            buttons.push((*hit, build_button(*hit, key, *icon, *face, BTN_H, &pending)));
        }
        let initial_analysis_capability = Self::discover_library_analysis_capability().ok();
        Self {
            actions: crate::resource::ActionSink::default(),
            content,
            #[cfg(test)]
            last_library_action: None,
            original: None,
            original_identity: None,
            observed_content: None,
            import_content: None,
            picker_stale: false,
            rendered: None,
            viewer: None,
            viewer_details: None,
            buttons,
            pending,
            notice: None,
            status: String::new(),
            rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            separator_rects: Vec::new(),
            faces,
            large_text_toolbar: false,
            toolbar_btn_h: BTN_H,
            compact_more_open: false,
            compact_more_focus: 0,
            compact_more_hits: Vec::new(),
            picker: None,
            contents_picker: None,
            directory_picker: None,
            directory_picker_retain_exif_location: true,
            directory_import: None,
            directory_import_receipt: None,
            directory_import_details_open: false,
            directory_import_details_page: 0,
            directory_import_details_layout: None,
            directory_import_buttons: build_directory_import_buttons(&directory_import_pending),
            directory_import_pending,
            directory_import_progress: ProgressBar::indeterminate()
                .with_label("Indexing photo folder"),
            resource_invocation: None,
            resource_result: None,
            pending_resource_open: None,
            analysis_tx,
            analysis_rx,
            active_analysis: HashSet::new(),
            cancelled_analysis: HashSet::new(),
            active_people_scans: HashSet::new(),
            pending_people_scans: HashMap::new(),
            library_analysis: crate::library_analysis::LibraryAnalysisState::default(),
            library_analysis_capability: initial_analysis_capability,
            library_analysis_buttons: build_library_analysis_buttons(&library_analysis_pending),
            library_analysis_progress: ProgressBar::new(0.0).with_label("Photo analysis"),
            library_analysis_pending,
            library_analysis_failure_hits: Vec::new(),
            library_analysis_coverage: (0, 0, 0),
            people_controller: crate::people::PeopleController::new(),
            people_scan_progress: PeopleScanProgress::load(),
            people_mode: false,
            people_selection: None,
            people_detail_transition: Transition::closed(),
            people_merge_mode: false,
            people_show_hidden: false,
            people_review_page: 0,
            people_approved: Vec::new(),
            people_delete_confirm: false,
            people_name_input: TextInput::local()
                .with_id("photos-people-name")
                .with_kind(TextInputKind::Name)
                .with_enter_submits(true)
                .with_placeholder("Name this person"),
            people_name_bounds: Rect::ZERO,
            people_controls: Vec::new(),
            people_control_signature: String::new(),
            people_pending,
            canvas_state: CanvasState::Empty,
            search_provider: crate::search::PhotosSearchProvider::empty(),
            search_heads: Vec::new(),
            search_refresh_error: None,
            #[cfg(test)]
            force_search_refresh_error: false,
            library_items: Vec::new(),
            library_total: 0,
            library_search: SearchField::new()
                .with_id("photos-library-search")
                .with_placeholder("Search photos, people, places, text"),
            library_query: String::new(),
            library_query_indexed: String::new(),
            library_query_generation: 0,
            library_query_cursor: None,
            library_query_matches: HashSet::new(),
            library_query_complete: true,
            library_destination: LibraryDestination::Timeline,
            places_list_mode: false,
            places_viewport: crate::places::MapViewport::default(),
            places_open_key: None,
            places_items: Vec::new(),
            places_page: 0,
            places_hits: Vec::new(),
            collection_page_hits: Vec::new(),
            collection_page_buttons: build_collection_page_buttons(&collection_page_pending),
            collection_page_pending,
            places_map_bounds: Rect::ZERO,
            places_drag_origin: None,
            places_camera_feedback: None,
            places_motion_reduced: false,
            library_favorite_count: 0,
            library_offset: 0,
            library_albums: Vec::new(),
            library_album_covers: HashMap::new(),
            albums_page: 0,
            active_album: None,
            active_folder: None,
            active_person: None,
            active_tag: None,
            active_object: None,
            active_pet: None,
            active_favorites: false,
            library_folders: Vec::new(),
            library_people: Vec::new(),
            people_page: 0,
            library_person_covers: HashMap::new(),
            library_tags: Vec::new(),
            metadata_filter_cards: Vec::new(),
            library_page_hits: Vec::new(),
            timeline_density_hits: Vec::new(),
            timeline_density_buttons: build_timeline_density_buttons(&timeline_density_pending),
            timeline_density_pending,
            library_select_button: build_library_header_button("photos-library-select", "Select"),
            library_album_buttons: build_library_album_buttons(),
            timeline_density: 1,
            timeline_scroll_y: 0.0,
            timeline_projection: TimelineProjection::default(),
            timeline_projection_heads: Vec::new(),
            timeline_projection_filter: String::new(),
            timeline_viewport_height: 0.0,
            timeline_index: DateIndexedScrollbar::new("photos:timeline-index"),
            timeline_index_markers: Vec::new(),
            timeline_index_metrics: ScrollMetrics::new(0.0, 0.0, 0.0),
            library_details_cards: Vec::new(),
            album_cards: Vec::new(),
            album_edit_cards: Vec::new(),
            album_editor: false,
            album_editor_id: None,
            album_editor_creating: false,
            album_name_input: TextInput::local().with_id("photos-album-name"),
            album_editor_members: HashSet::new(),
            album_delete_confirm: false,
            album_editor_focus: 0,
            album_editor_rect: Rect::ZERO,
            album_editor_hits: Vec::new(),
            album_page_buttons: build_album_page_buttons(&album_page_pending),
            album_page_pending,
            metadata_editor: false,
            metadata_editor_ids: Vec::new(),
            metadata_folder_input: TextInput::local()
                .with_id("photos-metadata-folder")
                .with_placeholder("Mixed — leave unchanged"),
            metadata_people_input: TextInput::local()
                .with_id("photos-metadata-people")
                .with_placeholder("Mixed — leave unchanged"),
            metadata_tags_input: TextInput::local()
                .with_id("photos-metadata-tags")
                .with_placeholder("Mixed — leave unchanged"),
            metadata_captured_at_input: TextInput::local()
                .with_id("photos-metadata-captured-at")
                .with_placeholder("Mixed — leave unchanged"),
            metadata_place_input: TextInput::local()
                .with_id("photos-metadata-place")
                .with_placeholder("Optional place name"),
            metadata_latitude_input: TextInput::local()
                .with_id("photos-metadata-latitude")
                .with_placeholder("Latitude"),
            metadata_longitude_input: TextInput::local()
                .with_id("photos-metadata-longitude")
                .with_placeholder("Longitude"),
            metadata_location_map: crate::places::MapViewport::default(),
            metadata_location_map_bounds: Rect::ZERO,
            metadata_location_from_map: false,
            metadata_mixed: [false; 5],
            metadata_force_clear: [false; 5],
            metadata_editor_section: MetadataEditorSection::Organize,
            metadata_editor_focus: 0,
            metadata_editor_hits: Vec::new(),
            metadata_editor_scroll: 0.0,
            metadata_editor_body: Rect::ZERO,
            metadata_editor_reveal_focus: false,
            metadata_editor_buttons: build_metadata_editor_buttons(&metadata_editor_pending),
            metadata_editor_pending,
            library_thumbnails: HashMap::new(),
            thumbnail_resource_keys: HashMap::new(),
            thumbnail_unavailable: HashSet::new(),
            original_decode_failed: None,
            thumbnail_service,
            thumbnail_keys: HashMap::new(),
            thumbnail_generation: Some(0),
            thumbnail_visible_ids: Vec::new(),
            thumbnail_queued: HashSet::new(),
            library_remove_confirm: None,
            selection_mode: false,
            selected_photo_ids: HashSet::new(),
            selection_anchor: None,
            selection_destination_album: None,
            selection_album_chooser: false,
            selection_album_page: 0,
            selection_all_favorite: false,
            selection_hits: Vec::new(),
            selection_destination_hits: Vec::new(),
            library_cards: Vec::new(),
            pointer: Point::ZERO,
            library_mode: false,
        }
    }

    /// Rebuild the strip for `width` if the room available has changed which
    /// items can afford their labels.
    ///
    /// Rebuilding rather than relabelling in place because a `Button` owns its
    /// label for its lifetime. The cost lands only on a resize that crosses
    /// the threshold, and it carries the enabled state across so a rebuild
    /// never re-enables a control the document says is unavailable.
    fn refit_toolbar(&mut self, width: f32, btn_h: f32) {
        let want = if self.large_text_toolbar {
            TOOLBAR
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    if index == 9 {
                        Face::LargeOverflow
                    } else {
                        Face::IconOnly
                    }
                })
                .collect()
        } else {
            faces_for_width(width, btn_h)
        };
        // Both heights come from the same pure `btn_h()` on the theme's scale,
        // so exact comparison is the honest one.
        #[allow(
            clippy::float_cmp,
            reason = "outputs of one pure function on one input"
        )]
        if want == self.faces && btn_h == self.toolbar_btn_h {
            return;
        }
        let enabled: Vec<bool> = self.buttons.iter().map(|(_, b)| b.is_enabled()).collect();
        self.buttons.clear();
        for (((hit, key, icon, _), face), was_enabled) in
            TOOLBAR.iter().zip(want.iter()).zip(enabled)
        {
            let mut button = build_button(*hit, key, *icon, *face, btn_h, &self.pending);
            button.set_enabled(was_enabled);
            self.buttons.push((*hit, button));
        }
        self.faces = want;
        self.toolbar_btn_h = btn_h;
    }

    /// Import `path` through the canonical durable action. The decoded source
    /// path remains only in this process; the action writes portable metadata
    /// and the staged content hash into the shared document.
    pub fn open_path(&mut self, doc: &mut AutoCommit, path: &str) {
        self.open_bytes(doc, path, bounded_file_bytes(path).as_deref());
    }

    /// Import a photograph chosen by its contents, where it has no path: its
    /// name labels it and its bytes live in the content store.
    pub fn open_contents(&mut self, doc: &mut AutoCommit, name: &str, bytes: &[u8]) {
        let bytes = (bytes.len() <= pixels::MAX_ENCODED_BYTES).then_some(bytes);
        self.open_bytes(doc, name, bytes);
    }

    fn open_bytes(&mut self, doc: &mut AutoCommit, path: &str, bytes: Option<&[u8]>) {
        match bytes.and_then(pixels::decode) {
            Some(img) => {
                // Put the bytes in the content store before naming them, so
                // the hash this document is about to carry is one this machine
                // can already answer for — and one the room can be handed.
                // Otherwise the document would name a picture only the path
                // could find, and the path is true on one machine.
                let content = match self.content.put(bytes.unwrap_or_default()) {
                    Ok(content) => content,
                    Err(error) => {
                        tracing::warn!(%error, "photos: could not store an opened original");
                        self.notice = Some(tr("photos-open-failed"));
                        return;
                    }
                };
                let open = OpenImage::from_path(path, img.w, img.h).with_content(content);
                self.original = Some((path.to_string(), img));
                self.original_identity = Some(decode_key(&open.clone().portable()));
                self.rendered = None;
                self.viewer = None;
                self.original_decode_failed = None;
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::ImportPhoto {
                        photo: LibraryPhoto::from_image(open),
                    },
                );
                self.notice = None;
            }
            None => {
                self.notice = Some(tr("photos-open-failed"));
            }
        }
    }

    /// The path of the photograph this instance is showing, if any.
    ///
    /// Read off the decoded original rather than the document, because the two
    /// answer different questions and only this one is true of the *pixels on
    /// screen*: `refresh` reloads it from the document and clears it when the
    /// file will not decode, so a path here means there is something drawn to
    /// point at. A document naming a file this machine cannot read would
    /// otherwise publish a rect around an empty canvas.
    pub fn open_image_path(&self) -> Option<&str> {
        self.original.as_ref().map(|(p, _)| p.as_str())
    }

    /// The pixels currently shown, if any. This is what Copy and Save export.
    pub fn edited_pixels(&self) -> Option<&Rgba> {
        self.rendered.as_ref().map(|r| &r.rgba)
    }

    /// The rect the canvas was laid out into. The photograph is drawn inside
    /// this and nowhere else.
    pub fn canvas_rect(&self) -> Rect {
        self.canvas_rect
    }

    /// The toolbar's rect.
    pub fn toolbar_rect(&self) -> Rect {
        self.toolbar_rect
    }

    /// The status line's rect.
    pub fn status_rect(&self) -> Rect {
        self.status_rect
    }

    /// Every toolbar control as `(accessible name, rect)`, in strip order.
    ///
    /// Exposed so a test can assert that each control is actually within the
    /// window. `canvas_rect` staying inside the frame says nothing about the
    /// strip above it, which is how a fixed-width toolbar came to push Save
    /// off the right-hand edge of any window narrower than 912 px while every
    /// geometry assertion still passed.
    pub fn toolbar_item_rects(&self) -> Vec<(String, Rect)> {
        // Unreachable: `self.buttons` is always built one entry per `TOOLBAR`
        // item, so every index here has a matching `TOOLBAR` entry.
        self.buttons
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                self.toolbar_slots()
                    .iter()
                    .any(|slot| matches!(slot, Slot::Item(item) if *item == *index))
            })
            .filter_map(|(i, (_, b))| TOOLBAR.get(i).map(|item| (tr(item.1), b.get_rect())))
            .collect()
    }

    /// Measured library-card geometry for screenshot and interaction
    /// regressions. Every returned tuple is `(card, details, remove,
    /// favorite)`; the three controls are their actual 44px hit rectangles,
    /// not the smaller icon artwork inset inside them.
    pub fn library_card_action_rects(&self) -> Vec<(Rect, Rect, Rect, Rect)> {
        self.library_cards
            .iter()
            .filter(|(id, _, _)| {
                self.library_details_cards
                    .iter()
                    .any(|(candidate, _)| candidate == id)
            })
            .map(|(_, card, _)| {
                let actions = card_action_controls(*card);
                (*card, actions.details, actions.remove, actions.favorite)
            })
            .collect()
    }

    /// The status line as it currently reads.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Whether a photograph is loaded and rendered.
    pub fn has_image(&self) -> bool {
        self.viewer.is_some()
    }

    /// Device-local People controller for a platform-specific face pipeline
    /// or the shared People management surface. It intentionally exposes no
    /// document reference: local clusters and their evidence never sync.
    pub fn people_controller(&self) -> &crate::people::PeopleController {
        &self.people_controller
    }

    /// Runtime constructor: photographs in the app's `content` store and the
    /// device-local People review queue. Tests use [`Self::new`] so they never
    /// read or mutate the signed-in user's private recognition state.
    #[must_use]
    pub fn new_persistent(content: ContentStore) -> Self {
        let mut app = Self::with_content(content);
        app.people_controller = crate::people::PeopleController::persistent();
        if let Ok(service) = thumbnail::ThumbnailService::open_default(1) {
            app.thumbnail_service = with_thumbnail_wake(service);
        }
        app
    }

    /// Mutable local People controller. Callers use this for explicit opt-in,
    /// review, hiding, splitting, and deletion of private clusters.
    pub fn people_controller_mut(&mut self) -> &mut crate::people::PeopleController {
        &mut self.people_controller
    }

    /// Commit a reviewed cluster's compact, user-approved membership through
    /// the normal typed action dispatcher. The local cluster stays local; the
    /// action contains only the selected display name and stable photo ids.
    pub fn approve_people_cluster(&mut self, doc: &mut AutoCommit, cluster_id: &str) -> bool {
        let Some(action) = self.people_controller.approve_membership(cluster_id) else {
            self.notice = Some(
                "Name and confirm this local People suggestion before sharing its labels"
                    .to_owned(),
            );
            return false;
        };
        self.commit_library_action(doc, &action);
        self.notice =
            Some("Approved person labels were added to the shared photo library".to_owned());
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "rebuilds every retained People control from one cluster snapshot so labels and routes cannot drift"
    )]
    fn refresh_people_controls(&mut self) {
        let clusters: Vec<_> = self
            .people_controller
            .all_clusters()
            .map(|cluster| {
                (
                    cluster.id.clone(),
                    cluster.display_name.clone(),
                    cluster.photos.len(),
                    cluster.hidden,
                    cluster.rejected,
                )
            })
            .collect();
        let signature = format!(
            "{:?}|{}|{}|{}|{}|{:?}|{:?}",
            self.people_selection,
            self.people_merge_mode,
            self.people_show_hidden,
            self.people_review_page,
            self.people_controller.privacy().recognition_enabled,
            self.people_approved
                .iter()
                .map(|person| (&person.label, person.count()))
                .collect::<Vec<_>>(),
            clusters
        );
        if signature == self.people_control_signature {
            return;
        }
        self.people_control_signature = signature;
        self.people_controls.clear();
        let pending = Rc::clone(&self.people_pending);
        let recognition_enabled = self.people_controller.privacy().recognition_enabled;
        let mut push = |intent: PeopleIntent, label: String, primary: bool, danger: bool| {
            let visible_label = match &intent {
                PeopleIntent::OpenApproved(_) => "View photos",
                PeopleIntent::OpenSuggestion(_) => "Review",
                _ => label.as_str(),
            };
            let button = Button::new(
                &format!("photos-people-control-{}", self.people_controls.len()),
                visible_label,
                None,
            )
            .with_accessible_name(&label);
            let mut button = if matches!(
                &intent,
                PeopleIntent::Back
                    | PeopleIntent::OpenApproved(_)
                    | PeopleIntent::OpenSuggestion(_)
                    | PeopleIntent::BrowseApproved
                    | PeopleIntent::BeginMerge
                    | PeopleIntent::ToggleHidden
                    | PeopleIntent::Page(_)
            ) {
                let queued = intent.clone();
                let pending = Rc::clone(&pending);
                button.on_view(move || *pending.borrow_mut() = Some(queued.clone()))
            } else {
                let queued = intent.clone();
                let pending = Rc::clone(&pending);
                button.on_effect(move || *pending.borrow_mut() = Some(queued.clone()))
            }
            .fill_width(44.0);
            if primary || matches!(&intent, PeopleIntent::OpenSuggestion(_)) {
                button = button.primary();
            } else if danger {
                button = button.danger();
            } else if matches!(
                &intent,
                PeopleIntent::Back | PeopleIntent::ToggleHidden | PeopleIntent::DeleteLocalData
            ) {
                button = button.ghost();
            }
            if matches!(intent, PeopleIntent::Scan) && !recognition_enabled {
                button = button.with_enabled(false);
            }
            self.people_controls.push(PeopleControl {
                intent,
                label,
                bounds: Rect::ZERO,
                button,
            });
        };

        push(
            PeopleIntent::Back,
            if self.people_selection.is_some() {
                "Back to People"
            } else {
                "Back to Library"
            }
            .to_owned(),
            false,
            false,
        );
        if self.people_selection.is_none() {
            let recognition_on = self.people_controller.privacy().recognition_enabled;
            push(
                PeopleIntent::ToggleRecognition,
                if recognition_on {
                    "Turn recognition off"
                } else {
                    "Turn recognition on"
                }
                .to_owned(),
                false,
                false,
            );
            if self.active_people_scans.is_empty() {
                push(
                    PeopleIntent::Scan,
                    "Scan local library".to_owned(),
                    true,
                    false,
                );
            } else {
                push(
                    PeopleIntent::CancelScan,
                    "Pause scanning".to_owned(),
                    false,
                    false,
                );
            }
            let page_size = if self.canvas_rect.width() < 760.0 {
                2
            } else {
                8
            };
            let start = self.people_review_page.saturating_mul(page_size);
            for person in self.people_approved.iter().skip(start).take(page_size) {
                push(
                    PeopleIntent::OpenApproved(person.label.clone()),
                    format!(
                        "{}  ·  {} photo{}",
                        person.label,
                        person.count(),
                        if person.count() == 1 { "" } else { "s" }
                    ),
                    false,
                    false,
                );
            }
            let visible_clusters: Vec<_> = self
                .people_controller
                .all_clusters()
                .filter(|cluster| self.people_show_hidden || (!cluster.hidden && !cluster.rejected))
                .skip(start)
                .take(page_size)
                .map(|cluster| {
                    (
                        cluster.id.clone(),
                        cluster.display_name.clone(),
                        cluster.photos.len(),
                        cluster.hidden || cluster.rejected,
                    )
                })
                .collect();
            for (id, name, count, hidden) in visible_clusters {
                let state = if hidden { "hidden" } else { "review" };
                push(
                    PeopleIntent::OpenSuggestion(id),
                    format!(
                        "{}  ·  {count} photo{}  ·  {state}",
                        name.unwrap_or_else(|| "Unnamed suggestion".to_owned()),
                        if count == 1 { "" } else { "s" }
                    ),
                    false,
                    false,
                );
            }
            push(
                PeopleIntent::ToggleHidden,
                if self.people_show_hidden {
                    "Hide dismissed suggestions"
                } else {
                    "Review hidden suggestions"
                }
                .to_owned(),
                false,
                false,
            );
            if self.people_review_page > 0 {
                push(
                    PeopleIntent::Page(false),
                    "Previous".to_owned(),
                    false,
                    false,
                );
            }
            let total = self.people_approved.len().max(
                self.people_controller
                    .all_clusters()
                    .filter(|cluster| {
                        self.people_show_hidden || (!cluster.hidden && !cluster.rejected)
                    })
                    .count(),
            );
            if start.saturating_add(page_size) < total {
                push(PeopleIntent::Page(true), "Next".to_owned(), false, false);
            }
            push(
                if self.people_delete_confirm {
                    PeopleIntent::ConfirmDeleteLocalData
                } else {
                    PeopleIntent::DeleteLocalData
                },
                if self.people_delete_confirm {
                    "Confirm deleting local face data"
                } else {
                    "Delete local face data"
                }
                .to_owned(),
                false,
                self.people_delete_confirm,
            );
        } else {
            match self.people_selection.clone() {
                Some(PeopleSelection::Approved(name)) => {
                    if self.people_merge_mode {
                        for person in &self.people_approved {
                            if person.label != name {
                                push(
                                    PeopleIntent::MergeApprovedInto(person.label.clone()),
                                    format!("Merge into {}", person.label),
                                    false,
                                    false,
                                );
                            }
                        }
                    } else {
                        push(
                            PeopleIntent::BrowseApproved,
                            "Browse photographs".to_owned(),
                            true,
                            false,
                        );
                        push(PeopleIntent::SaveName, "Save name".to_owned(), false, false);
                        push(
                            PeopleIntent::BeginMerge,
                            "Merge person…".to_owned(),
                            false,
                            false,
                        );
                    }
                }
                Some(PeopleSelection::Suggestion(id)) => {
                    let hidden = self
                        .people_controller
                        .cluster(&id)
                        .is_some_and(|cluster| cluster.hidden || cluster.rejected);
                    if self.people_merge_mode {
                        let candidates: Vec<_> = self
                            .people_controller
                            .all_clusters()
                            .filter(|cluster| cluster.id != id && !cluster.rejected)
                            .map(|cluster| {
                                (
                                    cluster.id.clone(),
                                    cluster
                                        .display_name
                                        .clone()
                                        .unwrap_or_else(|| "Unnamed suggestion".to_owned()),
                                )
                            })
                            .collect();
                        for (candidate, name) in candidates {
                            push(
                                PeopleIntent::MergeSuggestionWith(candidate),
                                format!("Combine with {name}"),
                                false,
                                false,
                            );
                        }
                    } else if hidden {
                        push(
                            PeopleIntent::RestoreSuggestion,
                            "Return to review".to_owned(),
                            true,
                            false,
                        );
                    } else {
                        push(
                            PeopleIntent::ApproveSuggestion,
                            "Approve and add to People".to_owned(),
                            true,
                            false,
                        );
                        push(
                            PeopleIntent::HideSuggestion,
                            "Decide later".to_owned(),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::RejectSuggestion,
                            "Dismiss suggestion".to_owned(),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::BeginMerge,
                            "Combine…".to_owned(),
                            false,
                            false,
                        );
                    }
                }
                None => {}
            }
        }
    }

    fn select_people_item(&mut self, selection: PeopleSelection) {
        let initial = match &selection {
            PeopleSelection::Approved(name) => name.clone(),
            PeopleSelection::Suggestion(id) => self
                .people_controller
                .cluster(id)
                .and_then(|cluster| cluster.display_name.clone())
                .unwrap_or_default(),
        };
        self.people_selection = Some(selection);
        self.people_detail_transition
            .set_open(true, Duration::from_millis(190), Curve::EaseOut);
        self.people_merge_mode = false;
        self.people_delete_confirm = false;
        self.people_name_input.set_text(&initial);
        self.people_name_input.set_focus(false);
        self.people_control_signature.clear();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match routes every People intent to its local or shared effect"
    )]
    fn handle_people_intent(&mut self, intent: PeopleIntent, doc: &mut AutoCommit) {
        match intent {
            PeopleIntent::Back => {
                if self.people_selection.is_some() {
                    self.people_selection = None;
                    self.people_merge_mode = false;
                    self.people_detail_transition.set_open(
                        false,
                        Duration::from_millis(160),
                        Curve::EaseOut,
                    );
                } else {
                    self.library_destination = LibraryDestination::Timeline;
                    self.people_mode = false;
                }
            }
            PeopleIntent::ToggleRecognition => {
                let enabled = !self.people_controller.privacy().recognition_enabled;
                self.people_controller.set_recognition_enabled(enabled);
                if enabled {
                    self.people_controller
                        .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                }
                self.notice = Some(self.people_controller.capability().summary().to_owned());
            }
            PeopleIntent::Scan => {
                let invocation = tilo_actions::InvocationId::new();
                let payload =
                    serde_json::to_vec(&crate::actions::PhotosOperation::AnalyzePeopleLibrary)
                        .unwrap_or_default();
                if let Err(error) = self.admit_resource_operation_with_document(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::PHOTOS_OPERATION_TYPE,
                    &payload,
                    doc,
                    &automerge::ROOT,
                ) {
                    self.notice = Some(error);
                } else {
                    self.notice = Some("Scanning the next local batch…".to_owned());
                }
            }
            PeopleIntent::CancelScan => {
                for invocation in self.active_people_scans.drain() {
                    self.active_analysis.remove(&invocation);
                    self.cancelled_analysis.insert(invocation);
                    self.pending_people_scans.remove(&invocation);
                }
                "cancelled".clone_into(&mut self.people_scan_progress.status);
                self.people_scan_progress.save();
                self.notice = Some("People scan paused".to_owned());
            }
            PeopleIntent::OpenApproved(name) => {
                self.select_people_item(PeopleSelection::Approved(name));
            }
            PeopleIntent::OpenSuggestion(id) => {
                self.select_people_item(PeopleSelection::Suggestion(id));
            }
            PeopleIntent::BrowseApproved => {
                if let Some(PeopleSelection::Approved(name)) = &self.people_selection {
                    self.active_person = Some(name.clone());
                    self.library_destination = LibraryDestination::Timeline;
                    self.people_mode = false;
                    self.timeline_scroll_y = 0.0;
                }
            }
            PeopleIntent::SaveName => {
                if let Some(PeopleSelection::Approved(from)) = self.people_selection.clone() {
                    let to = self.people_name_input.value().trim().to_owned();
                    if to.is_empty() {
                        self.notice = Some("Enter a name first".to_owned());
                    } else {
                        self.commit_library_action(
                            doc,
                            &crate::actions::PhotosAction::RenamePerson {
                                from,
                                to: to.clone(),
                            },
                        );
                        self.people_selection = Some(PeopleSelection::Approved(to));
                        self.notice = Some("Person renamed in the shared library".to_owned());
                    }
                }
            }
            PeopleIntent::BeginMerge => self.people_merge_mode = true,
            PeopleIntent::MergeApprovedInto(to) => {
                if let Some(PeopleSelection::Approved(from)) = self.people_selection.clone() {
                    self.commit_library_action(
                        doc,
                        &crate::actions::PhotosAction::RenamePerson { from, to },
                    );
                    self.people_selection = None;
                    self.people_merge_mode = false;
                    self.notice = Some("People and their photographs were merged".to_owned());
                }
            }
            PeopleIntent::ApproveSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let name = self.people_name_input.value().trim().to_owned();
                    if matches!(
                        self.people_controller.confirm_suggestion(&id, &name),
                        crate::people::PeopleChange::Unchanged
                    ) || !self.approve_people_cluster(doc, &id)
                    {
                        self.notice = Some("Enter a short name before approving".to_owned());
                    } else {
                        let _ = self.people_controller.set_hidden(&id, true);
                        self.people_selection = None;
                    }
                }
            }
            PeopleIntent::RejectSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.reject_suggestion(&id);
                    self.people_selection = None;
                    self.notice = Some("Suggestion dismissed on this device".to_owned());
                }
            }
            PeopleIntent::HideSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.set_hidden(&id, true);
                    self.people_selection = None;
                    self.notice = Some("Suggestion hidden; you can restore it later".to_owned());
                }
            }
            PeopleIntent::RestoreSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.restore_suggestion(&id);
                    self.people_selection = None;
                    self.notice = Some("Suggestion returned to review".to_owned());
                }
            }
            PeopleIntent::MergeSuggestionWith(source) => {
                if let Some(PeopleSelection::Suggestion(destination)) =
                    self.people_selection.clone()
                {
                    let _ = self.people_controller.merge_clusters(&destination, &source);
                    self.people_merge_mode = false;
                    self.notice = Some("Local suggestions combined for review".to_owned());
                }
            }
            PeopleIntent::ToggleHidden => self.people_show_hidden = !self.people_show_hidden,
            PeopleIntent::Page(forward) => {
                self.people_review_page = if forward {
                    self.people_review_page.saturating_add(1)
                } else {
                    self.people_review_page.saturating_sub(1)
                };
            }
            PeopleIntent::DeleteLocalData => {
                self.people_delete_confirm = true;
                self.notice = Some(
                    "Activate again to delete local face suggestions and model identity".to_owned(),
                );
            }
            PeopleIntent::ConfirmDeleteLocalData => {
                self.people_controller.delete_all_local_biometric_data();
                self.people_delete_confirm = false;
                self.people_selection = None;
                self.notice =
                    Some("Local face data deleted; approved shared names were kept".to_owned());
            }
        }
        self.people_control_signature.clear();
    }

    /// Reveal the bounded library grid.  Kept public for the deterministic
    /// offscreen gallery; ordinary interaction reaches it through the same
    /// toolbar button and semantic action as a user.
    pub fn show_library(&mut self) {
        self.library_mode = true;
        self.people_mode = false;
        self.library_destination = LibraryDestination::Timeline;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.timeline_scroll_y = 0.0;
        self.library_remove_confirm = None;
        self.finish_selection();
    }

    /// Resolve a canonical timeline resource without treating a visual card as
    /// the source of truth. The shared capture value is the only input: a
    /// removed or changed boundary refuses instead of jumping to a nearby,
    /// misleading date.
    fn view_timeline(
        &mut self,
        doc: &AutoCommit,
        anchor: Option<&crate::actions::TimelineAnchor>,
    ) -> Result<(), String> {
        self.show_library();
        let Some(anchor) = anchor else {
            return Ok(());
        };
        let mut photos: Vec<_> = PhotosDoc::library(doc)
            .photos
            .into_iter()
            .map(|photo| {
                let displayed_day = photo
                    .captured_at
                    .as_deref()
                    .map_or_else(|| "Undated".to_owned(), capture_day);
                let canonical_day = photo
                    .captured_at
                    .as_deref()
                    .and_then(crate::actions::timeline_day_from_capture);
                (displayed_day, canonical_day, photo.id)
            })
            .collect();
        photos.sort_by(|(left_day, _, left_id), (right_day, _, right_id)| {
            let left_undated = left_day == "Undated";
            let right_undated = right_day == "Undated";
            left_undated
                .cmp(&right_undated)
                .then_with(|| right_day.cmp(left_day))
                .then_with(|| left_id.cmp(right_id))
        });
        let target = photos.iter().find(|(_, canonical_day, _)| match anchor {
            crate::actions::TimelineAnchor::Month { month } => canonical_day
                .as_deref()
                .is_some_and(|day| day.starts_with(month)),
            crate::actions::TimelineAnchor::Day { day: wanted } => {
                canonical_day.as_deref() == Some(wanted)
            }
        });
        let Some((target_day, _, _)) = target else {
            return Err("that timeline boundary is no longer in the shared library".to_owned());
        };
        if let Some(offset) = self
            .timeline_projection
            .day_offset(target_day, self.timeline_viewport_height)
        {
            self.timeline_scroll_y = offset;
            return Ok(());
        }
        // A semantic action can arrive before the first painted frame. Build
        // the same lightweight metadata projection now so the jump is exact
        // rather than falling back to a guessed fixed-height row.
        let library = PhotosDoc::library(doc);
        self.timeline_projection
            .replace(library.photos.iter().map(|photo| {
                TimelinePhoto {
                    id: photo.id.clone(),
                    name: photo.image.name.clone(),
                    favorite: photo.favorite,
                    day: photo
                        .captured_at
                        .as_deref()
                        .map_or_else(|| "Undated".to_owned(), capture_day),
                    captured_at: photo.captured_at.clone().unwrap_or_default(),
                    aspect_ratio: if photo.image.width > 0 && photo.image.height > 0 {
                        f64::from(photo.image.width) / f64::from(photo.image.height)
                    } else {
                        1.5
                    },
                }
            }));
        let width = (self.canvas_rect.width() - f64::from(PAD * 2.0) - 18.0).max(44.0);
        self.timeline_projection.set_geometry(TimelineGeometry {
            width,
            target_height: 168.0,
            gap: 16.0,
            compact: self.canvas_rect.width() < LIBRARY_DESKTOP_MASTHEAD_MIN_W,
        });
        self.timeline_scroll_y = self
            .timeline_projection
            .day_offset(target_day, self.timeline_viewport_height)
            .unwrap_or(0.0);
        Ok(())
    }

    /// Reveal the People collection without enabling recognition or changing
    /// any document metadata. This is used by the resource action and visual
    /// acceptance scenarios.
    pub fn show_people(&mut self) {
        self.show_library();
        self.people_mode = true;
        self.library_destination = LibraryDestination::People;
    }

    /// Reveal albums as a first-class collection. Opening the collection is
    /// presentation-only; opening a specific album still resolves its stable
    /// resource id and then enters the chronological photo view.
    pub fn show_albums(&mut self) {
        self.show_library();
        self.people_mode = false;
        self.library_destination = LibraryDestination::Albums;
    }

    /// Pose the album overview for deterministic offscreen visual coverage.
    pub fn show_albums_for_shot(&mut self) {
        self.show_albums();
    }

    /// Pose one real local suggestion for the deterministic visual gallery.
    /// The named cluster must already exist; this never invents recognition
    /// output or runs on a live launch.
    pub fn show_people_suggestion_for_shot(&mut self, cluster_id: &str) {
        self.show_people();
        if self.people_controller.cluster(cluster_id).is_some() {
            self.select_people_item(PeopleSelection::Suggestion(cluster_id.to_owned()));
            // Acceptance shots exercise the settled visual state. Live entry
            // still starts the shared transition in `select_people_item`.
            self.people_detail_transition = Transition::open();
        }
    }

    /// Reveal the whole-library local-analysis dashboard for deterministic
    /// visual acceptance. No scan is started and no result is fabricated.
    pub fn show_library_analysis(&mut self) {
        self.show_library();
        self.library_destination = LibraryDestination::Analysis;
        self.library_analysis_capability = Self::discover_library_analysis_capability().ok();
    }

    fn begin_selection(&mut self) {
        self.selection_mode = true;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_page = 0;
        self.selection_all_favorite = false;
        self.notice = Some("Select photographs, then apply a shared library action".to_owned());
    }

    fn finish_selection(&mut self) {
        self.selection_mode = false;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_page = 0;
        self.selection_all_favorite = false;
    }

    /// Toggle one materialised card, or the inclusive visible range from the
    /// previous anchor. A range is a view operation, so it is intentionally
    /// bounded to the cards the person can currently inspect.
    fn toggle_library_selection(&mut self, photo_id: &str, extend: bool) {
        if !self.selection_mode {
            return;
        }
        let visible: Vec<_> = self.library_cards.iter().map(|(id, _, _)| id).collect();
        if extend
            && let Some(anchor) = &self.selection_anchor
            && let (Some(start), Some(end)) = (
                visible.iter().position(|id| **id == *anchor),
                visible.iter().position(|id| **id == photo_id),
            )
        {
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            if let Some(range) = visible.get(start..=end) {
                for id in range {
                    self.selected_photo_ids.insert((**id).clone());
                }
            }
            return;
        }
        if !self.selected_photo_ids.insert(photo_id.to_owned()) {
            self.selected_photo_ids.remove(photo_id);
        }
        self.selection_anchor = Some(photo_id.to_owned());
    }

    fn select_all_visible(&mut self) {
        self.selected_photo_ids
            .extend(self.library_cards.iter().map(|(id, _, _)| id.clone()));
    }

    fn set_selected_favorite(&mut self, doc: &mut AutoCommit, favorite: bool) {
        let mut ids: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            self.notice = Some("Select at least one photograph first".to_owned());
            return;
        }
        self.commit_library_action(
            doc,
            &crate::actions::PhotosAction::SetFavorites {
                photo_ids: ids,
                favorite,
            },
        );
        self.notice = Some(if favorite {
            "Selected photographs added to Favorites".to_owned()
        } else {
            "Selected photographs removed from Favorites".to_owned()
        });
        self.finish_selection();
    }

    fn add_selected_to_destination_album(&mut self, doc: &mut AutoCommit) {
        let Some(album_id) = self.selection_destination_album.clone() else {
            self.notice = Some("Choose a destination album first".to_owned());
            return;
        };
        let library = PhotosDoc::library(doc);
        let Some(album) = library.album(&album_id) else {
            self.notice = Some("That album is no longer available".to_owned());
            return;
        };
        let mut photo_ids = album.photo_ids.clone();
        let mut selected: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        selected.sort_unstable();
        for id in selected {
            if !photo_ids.contains(&id) {
                photo_ids.push(id);
            }
        }
        if self.selected_photo_ids.is_empty() {
            self.notice = Some("Select at least one photograph first".to_owned());
            return;
        }
        self.commit_library_action(
            doc,
            &crate::actions::PhotosAction::SetAlbumMembership {
                album_id,
                photo_ids,
            },
        );
        self.notice = Some("Selected photographs added to the album".to_owned());
        self.finish_selection();
    }

    fn handle_selection_hit(&mut self, hit: SelectionHit, doc: &mut AutoCommit) {
        match hit {
            SelectionHit::Start => self.begin_selection(),
            SelectionHit::SelectAll => self.select_all_visible(),
            SelectionHit::Clear => {
                self.selected_photo_ids.clear();
                self.selection_anchor = None;
            }
            SelectionHit::Favorite => {
                let library = PhotosDoc::library(doc);
                let all_favorite = !self.selected_photo_ids.is_empty()
                    && self
                        .selected_photo_ids
                        .iter()
                        .all(|id| library.photo(id).is_some_and(|photo| photo.favorite));
                self.set_selected_favorite(doc, !all_favorite);
            }
            SelectionHit::EditDetails => self.begin_selected_metadata_editor(doc),
            SelectionHit::ChooseAlbum => {
                if self.selected_photo_ids.is_empty() || self.library_albums.is_empty() {
                    self.notice = Some("Select photographs and create an album first".to_owned());
                } else {
                    self.selection_album_chooser = true;
                    self.selection_album_page = 0;
                }
            }
            SelectionHit::CancelDestinationChooser | SelectionHit::CloseDestinationChooser => {
                self.selection_album_chooser = false;
            }
            SelectionHit::ApplyDestination => {
                if self.selection_destination_album.is_some() {
                    self.selection_album_chooser = false;
                } else {
                    self.notice = Some("Choose a destination album first".to_owned());
                }
            }
            SelectionHit::PreviousDestinationPage => {
                self.selection_album_page = self.selection_album_page.saturating_sub(1);
            }
            SelectionHit::NextDestinationPage => {
                let page_count = self
                    .library_albums
                    .len()
                    .div_ceil(SELECTION_ALBUMS_PER_PAGE);
                self.selection_album_page =
                    (self.selection_album_page + 1).min(page_count.saturating_sub(1));
            }
            SelectionHit::AddToAlbum => self.add_selected_to_destination_album(doc),
            SelectionHit::Done => self.finish_selection(),
        }
    }

    /// Pose the ordinary library editor for deterministic offscreen coverage.
    /// The production entry point is the visible “Edit album” control; shot
    /// rendering has no window event stream to click it through.
    pub fn show_album_editor_for_shot(&mut self, doc: &AutoCommit) {
        self.show_library();
        self.active_album = PhotosDoc::library(doc)
            .albums
            .first()
            .map(|album| album.id.clone());
        self.begin_edit_album(doc);
    }

    /// Pose the shared metadata editor for visual regression coverage.
    pub fn show_metadata_editor_for_shot(&mut self, doc: &AutoCommit) {
        self.show_library();
        let library = PhotosDoc::library(doc);
        if let Some(id) = library
            .photos
            .iter()
            .find(|photo| !photo.metadata_summary().is_empty())
            .or_else(|| library.photos.first())
            .map(|photo| photo.id.clone())
        {
            self.begin_metadata_editor(doc, &id);
        }
    }

    /// Pose a mixed-value multi-photo edit on its location section. This is
    /// the state most likely to regress silently because blank means “keep”
    /// until the user explicitly clears or replaces the dimension.
    pub fn show_batch_metadata_editor_for_shot(&mut self, doc: &AutoCommit) {
        self.show_selection_for_shot(doc);
        self.begin_selected_metadata_editor(doc);
        self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
    }

    /// Pose the offline Places destination for deterministic visual coverage.
    pub fn show_places_for_shot(&mut self) {
        self.show_library();
        self.library_destination = LibraryDestination::Places;
        self.places_open_key = None;
        self.places_viewport = crate::places::MapViewport {
            latitude: 18.0,
            longitude: 0.0,
            zoom: 1.5,
        };
    }

    /// Apply a local map-camera change and give the marker layer a restrained
    /// settle cue. The viewport is presentation state, so the animation never
    /// delays a semantic action or writes shared metadata.
    fn set_places_viewport(&mut self, viewport: crate::places::MapViewport) {
        let viewport = viewport.clamped();
        if self.places_viewport == viewport {
            return;
        }
        self.places_viewport = viewport;
        if !self.places_motion_reduced {
            self.places_camera_feedback = Some(Animated::new(
                1.0,
                0.0,
                Duration::from_millis(180),
                Curve::EaseOut,
            ));
        }
    }

    fn zoom_places_by(&mut self, delta: f64) {
        self.set_places_viewport(crate::places::MapViewport {
            zoom: self.places_viewport.zoom + delta,
            ..self.places_viewport
        });
    }

    fn places_camera_feedback(&mut self) -> f64 {
        let Some(feedback) = self.places_camera_feedback else {
            return 0.0;
        };
        let value = feedback.current(ui::time::now());
        if feedback.done(ui::time::now()) {
            self.places_camera_feedback = None;
            0.0
        } else {
            value
        }
    }

    /// Open the exact durable collection behind a Places resource or marker.
    /// This reads only shared metadata, so it remains usable offline and
    /// deliberately fails if a collaborator removed the addressed place.
    fn open_place_collection(&mut self, doc: &AutoCommit, place_key: &str) -> Result<(), String> {
        let photos: Vec<_> = PhotosDoc::library(doc)
            .photos
            .into_iter()
            .filter(|photo| {
                photo.location.as_ref().is_some_and(|location| {
                    location.is_valid() && location.place_key() == place_key
                })
            })
            .collect();
        let Some(location) = photos.first().and_then(|photo| photo.location.as_ref()) else {
            return Err("that place is no longer in the shared library".to_owned());
        };
        self.show_library();
        self.library_destination = LibraryDestination::Places;
        self.places_open_key = Some(place_key.to_owned());
        self.places_list_mode = true;
        self.places_page = 0;
        self.set_places_viewport(crate::places::MapViewport {
            latitude: location.latitude,
            longitude: location.longitude,
            zoom: 12.0,
        });
        self.notice = Some(format!(
            "Showing {} shared photograph{} at this place offline",
            photos.len(),
            if photos.len() == 1 { "" } else { "s" }
        ));
        Ok(())
    }

    /// Pose a running folder import for hermetic offscreen visual coverage.
    /// The held sender prevents the ordinary sync loop from treating this
    /// deterministic fixture as a crashed worker.
    pub fn show_directory_import_progress_for_shot(&mut self) {
        self.show_library();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.directory_import_receipt = None;
        self.directory_import_details_open = false;
        self.directory_import = Some(DirectoryImport {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            invocation: None,
            mode: DirectoryImportMode::Import,
            discovered: 184,
            imported: 112,
            refreshed: 0,
            unchanged: 7,
            skipped: 2,
            sequence: 184,
            pending: VecDeque::new(),
            failures: vec![
                crate::import::ImportFailure {
                    source: "damaged-scan.jpg".into(),
                    message: "This image cannot be decoded safely".into(),
                    retryable: true,
                },
                crate::import::ImportFailure {
                    source: "archive-link".into(),
                    message: "Symbolic links are not followed during photo import".into(),
                    retryable: false,
                },
            ],
            worker_done: false,
            cancelled: false,
            limit_reached: false,
            produced_resources: Vec::new(),
            _shot_sender: Some(sender),
        });
    }

    /// Pose a completed partial import with a deliberately bounded issue
    /// receipt. It exists only for offscreen regression images.
    pub fn show_directory_import_errors_for_shot(&mut self) {
        self.show_library();
        self.directory_import = None;
        let failures = (1..=9)
            .map(|number| crate::import::ImportFailure {
                source: format!("photo-{number:02}.jpg").into(),
                message: if number % 3 == 0 {
                    "This image cannot be decoded safely".into()
                } else {
                    "Could not read this image".into()
                },
                retryable: number % 3 != 0,
            })
            .collect();
        self.directory_import_receipt = Some(DirectoryImportReceipt {
            mode: DirectoryImportMode::Import,
            discovered: 268,
            imported: 241,
            refreshed: 0,
            unchanged: 0,
            failures,
            failure_count: 27,
            cancelled: false,
            limit_reached: false,
        });
        self.directory_import_details_open = true;
        self.directory_import_details_page = 0;
    }

    /// Pose the ordinary selection shelf for deterministic visual coverage.
    /// Two real photographs are selected so the capture exercises the active
    /// controls and selected-card treatment, not merely their disabled state.
    pub fn show_selection_for_shot(&mut self, doc: &AutoCommit) {
        self.show_library();
        self.begin_selection();
        for photo in PhotosDoc::library(doc).photos.into_iter().take(2) {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id);
        }
    }

    /// Pose the first page of a deliberately long destination list. The
    /// selection is local; this exists only to make pagination visible in the
    /// deterministic accessibility and responsive screenshots.
    pub fn show_selection_destination_chooser_for_shot(&mut self, doc: &AutoCommit) {
        if let Some(photo) = PhotosDoc::library(doc).photos.first() {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id.clone());
        }
        self.selection_album_chooser = true;
        self.selection_album_page = 0;
    }

    /// Zoom the canvas, as the scroll wheel would. Lets a caller frame a crop
    /// without simulating a chain of scroll events by hand.
    ///
    /// `ImageViewer` exposes no direct zoom setter to a widget that only
    /// depends on it (only a `#[cfg(test)]` one, private to that crate), so
    /// this drives the same `MouseWheel` path a real scroll takes: park the
    /// cursor at the viewer's own centre — where the widget's screen-to-image
    /// mapping is invariant under a zoom change, so pan stays exactly where it
    /// was — then send one wheel notch sized to land on `zoom` exactly.
    pub fn set_zoom(&mut self, zoom: f32) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        let rect = Widget::get_rect(v);
        if rect.width() <= 0.0 || rect.height() <= 0.0 || v.zoom() <= 0.0 {
            return;
        }
        let center =
            winit::dpi::PhysicalPosition::new((rect.x0 + rect.x1) * 0.5, (rect.y0 + rect.y1) * 0.5);
        // Solves `v.zoom() * ZOOM_STEP.powf(scroll_y) == zoom` for the notch
        // the widget's own wheel handler would need to see. `1.1` mirrors the
        // private `ZOOM_STEP` constant in `ui::widget::image_viewer` — there is
        // no public way to ask the widget for it.
        let scroll_y = (zoom / v.zoom()).ln() / 1.1_f32.ln();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        // A fresh leaf on a fresh tree cannot fail; bail out rather than
        // panic if it somehow does.
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return;
        };
        // SAFETY: `DeviceId` has no public constructor; a zeroed value is
        // winit's own "no device" sentinel, and every field below is
        // overwritten or unused by `ImageViewer::handle_event`. Same idiom
        // `ui::widget::image_viewer`'s own tests use.
        let device_id = unsafe { std::mem::zeroed() };
        v.handle_event(
            &winit::event::WindowEvent::CursorMoved {
                device_id,
                position: center,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        v.handle_event(
            &winit::event::WindowEvent::MouseWheel {
                device_id,
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, scroll_y),
                phase: winit::event::TouchPhase::Moved,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// The visible region of the canvas in normalized image coordinates, or
    /// the whole image when the viewer has not been laid out yet.
    ///
    /// `ImageViewer` exposes no direct query for this either, so it is derived
    /// in `visible_image_rect` from the same "contain fit, then zoom and
    /// pan" transform `ImageViewer::draw` applies — using only the widget's
    /// already-public `zoom()` / `pan()` / rect, and this app's own knowledge
    /// of the original image's pixel size.
    pub fn visible_rect(&self) -> Rect01 {
        match (self.viewer.as_ref(), self.original.as_ref()) {
            (Some(v), Some((_, original))) => {
                visible_image_rect(v, f64::from(original.w), f64::from(original.h))
                    .unwrap_or(Rect01::FULL)
            }
            _ => Rect01::FULL,
        }
    }

    /// Copy what is shown to the system clipboard as raw RGBA.
    ///
    /// Raw rather than PNG because that is what the clipboard carries between
    /// sandboxes here — [`ui::clipboard_copy_image`] hands the buffer to the
    /// compositor's selection, and the receiving app gets pixels without
    /// either side agreeing on a container format.
    pub fn copy_to_clipboard(&mut self) -> bool {
        let Some(img) = self.edited_pixels() else {
            return false;
        };
        let ok = ui::clipboard_copy_image(img.w as usize, img.h as usize, &img.bytes);
        self.notice = Some(if ok {
            tr("photos-copied")
        } else {
            tr("photos-copy-failed")
        });
        ok
    }

    /// Write what is shown to `path` as a PNG.
    pub fn save_to(&mut self, path: &str) -> bool {
        let Some(bytes) = self.edited_pixels().and_then(pixels::encode_png) else {
            self.notice = Some(tr("photos-save-failed"));
            return false;
        };
        let saved = tilo_files::file_at(std::path::Path::new(path))
            .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes));
        if saved.is_ok() {
            self.notice = Some(format!("{} {path}", tr("photos-saved")));
            true
        } else {
            self.notice = Some(tr("photos-save-failed"));
            false
        }
    }

    /// Default save destination: the source file with `-edited` before the
    /// extension, so Save never silently overwrites the original.
    pub fn suggested_save_path(open: &OpenImage) -> String {
        let p = std::path::Path::new(&open.path);
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent = p
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        parent
            .join(format!("{stem}-edited.png"))
            .to_string_lossy()
            .into_owned()
    }

    /// Apply `next` to the document as an undoable edit.
    fn commit_edits(&mut self, doc: &mut AutoCommit, next: Edits) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::PHOTOS_ACTION_TYPE,
            &crate::actions::PhotosAction::set(&next),
            tilo_actions::ActionContext::for_action(
                "so.tilo.photos",
                "Photos",
                &crate::actions::PhotosAction::set(&next),
            ),
        );
        self.notice = None;
    }

    /// Show the library, open image, and edits the framework resource already holds.
    /// An empty projection does not wipe a library that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::PhotosState) {
        if state.open.is_none() && state.library.photos.is_empty() {
            return;
        }
        self.observed_content = Some(LiveContent {
            image: state.open.clone(),
            edits: state.edits.clone(),
            library: state.library.clone(),
        });
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Photos>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Every card interaction routes through the same typed, replayable
    /// action registry as resources and search results.  The canvas never
    /// writes document fields directly.
    fn commit_library_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::actions::PhotosAction,
    ) {
        #[cfg(test)]
        {
            self.last_library_action =
                tilo_actions::EncodedAction::encode(crate::actions::PHOTOS_ACTION_TYPE, action)
                    .ok();
        }
        self.actions.dispatch_with_context(
            doc,
            crate::actions::PHOTOS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.photos", "Photos", &action),
        );
    }

    fn begin_new_album(&mut self, _doc: &AutoCommit) {
        // A local `untitled-1` counter collides when two offline replicas
        // create their first album. Mint the durable identity independently
        // of its editable name so concurrent creations remain separate.
        let id = format!("photos.album:{}", tilo_actions::InvocationId::new().0);
        self.album_editor = true;
        self.album_editor_creating = true;
        self.album_editor_id = Some(id);
        self.album_name_input.set_text("Untitled album");
        self.album_name_input.set_focus(true);
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some("Name the album, choose photographs, then save".to_owned());
    }

    /// Open the compact, shared-widget metadata sheet for an explicit card.
    /// Nothing is inferred from pixels: people are only labels the user types.
    fn begin_metadata_editor(&mut self, doc: &AutoCommit, photo_id: &str) {
        self.begin_metadata_editor_for_ids(doc, vec![photo_id.to_owned()]);
    }

    fn begin_selected_metadata_editor(&mut self, doc: &AutoCommit) {
        let mut ids: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            self.notice = Some("Select at least one photograph first".to_owned());
            return;
        }
        self.begin_metadata_editor_for_ids(doc, ids);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "seeds every metadata field, mixed-value flag and draft input from one library snapshot"
    )]
    fn begin_metadata_editor_for_ids(&mut self, doc: &AutoCommit, photo_ids: Vec<String>) {
        let library = PhotosDoc::library(doc);
        if photo_ids.is_empty()
            || photo_ids.len() > crate::actions::MAX_BATCH_PHOTOS
            || !photo_ids.iter().all(|id| library.photo(id).is_some())
        {
            self.notice = Some("A selected photograph is no longer in the library".to_owned());
            return;
        }
        let photos: Vec<_> = photo_ids
            .iter()
            .filter_map(|id| library.photo(id))
            .collect();
        let common = |values: Vec<String>| {
            values
                .first()
                .filter(|first| values.iter().all(|value| value == *first))
                .cloned()
        };
        let folders: Vec<_> = photos
            .iter()
            .map(|photo| photo.folder.clone().unwrap_or_default())
            .collect();
        let people: Vec<_> = photos.iter().map(|photo| photo.people.join(", ")).collect();
        let tags: Vec<_> = photos.iter().map(|photo| photo.tags.join(", ")).collect();
        let captured: Vec<_> = photos
            .iter()
            .map(|photo| photo.captured_at.clone().unwrap_or_default())
            .collect();
        let locations: Vec<_> = photos.iter().map(|photo| photo.location.clone()).collect();
        let common_folder = common(folders);
        let common_people = common(people);
        let common_tags = common(tags);
        let common_captured = common(captured);
        let common_location = locations
            .first()
            .filter(|first| locations.iter().all(|value| value == *first))
            .cloned();

        self.metadata_editor = true;
        self.metadata_editor_ids = photo_ids;
        self.metadata_mixed = [
            common_folder.is_none(),
            common_people.is_none(),
            common_tags.is_none(),
            common_captured.is_none(),
            common_location.is_none(),
        ];
        self.metadata_force_clear = [false; 5];
        self.metadata_folder_input
            .set_text(common_folder.as_deref().unwrap_or_default());
        self.metadata_people_input
            .set_text(common_people.as_deref().unwrap_or_default());
        self.metadata_tags_input
            .set_text(common_tags.as_deref().unwrap_or_default());
        self.metadata_captured_at_input
            .set_text(common_captured.as_deref().unwrap_or_default());
        if let Some(Some(location)) = common_location {
            self.metadata_place_input
                .set_text(location.label.as_deref().unwrap_or_default());
            self.metadata_latitude_input
                .set_text(&format!("{:.6}", location.latitude));
            self.metadata_longitude_input
                .set_text(&format!("{:.6}", location.longitude));
            self.metadata_location_map.latitude = location.latitude;
            self.metadata_location_map.longitude = location.longitude;
            self.metadata_location_map.zoom = 4.0;
            self.metadata_location_from_map = location.provenance.source == "map";
        } else {
            self.metadata_place_input.set_text("");
            self.metadata_latitude_input.set_text("");
            self.metadata_longitude_input.set_text("");
            self.metadata_location_map = crate::places::MapViewport::default();
            self.metadata_location_from_map = false;
        }
        for (input, mixed, ordinary) in [
            (
                &mut self.metadata_folder_input,
                self.metadata_mixed[0],
                "Folder",
            ),
            (
                &mut self.metadata_people_input,
                self.metadata_mixed[1],
                "Add people (+); = replaces, - removes",
            ),
            (
                &mut self.metadata_tags_input,
                self.metadata_mixed[2],
                "Add tags (+); = replaces, - removes",
            ),
            (
                &mut self.metadata_captured_at_input,
                self.metadata_mixed[3],
                "Date and time",
            ),
        ] {
            input.set_placeholder(if mixed {
                "Mixed — leave unchanged"
            } else {
                ordinary
            });
        }
        let location_hint = if self.metadata_mixed[4] {
            "Mixed — leave unchanged"
        } else {
            "Latitude"
        };
        self.metadata_latitude_input.set_placeholder(location_hint);
        self.metadata_longitude_input
            .set_placeholder(if self.metadata_mixed[4] {
                "Mixed — leave unchanged"
            } else {
                "Longitude"
            });
        self.metadata_folder_input.set_focus(true);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_place_input.set_focus(false);
        self.metadata_latitude_input.set_focus(false);
        self.metadata_longitude_input.set_focus(false);
        self.metadata_editor_section = MetadataEditorSection::Organize;
        self.metadata_editor_focus = 0;
        self.metadata_editor_scroll = 0.0;
        self.metadata_editor_reveal_focus = true;
        self.album_editor = false;
        // The sheet owns the full privacy explanation. Keeping a duplicate
        // in the persistent status band made it wrap below a phone viewport.
        self.notice = None;
    }

    fn cancel_metadata_editor(&mut self) {
        self.metadata_editor = false;
        self.metadata_editor_ids.clear();
        self.metadata_folder_input.set_focus(false);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_place_input.set_focus(false);
        self.metadata_latitude_input.set_focus(false);
        self.metadata_longitude_input.set_focus(false);
        self.metadata_editor_focus = 0;
        self.notice = Some("Photo details discarded".to_owned());
    }

    #[expect(
        clippy::too_many_lines,
        reason = "builds the single atomic batch metadata action from all sheet fields in one place"
    )]
    fn save_metadata_editor(&mut self, doc: &mut AutoCommit) {
        if self.metadata_editor_ids.is_empty() {
            return;
        }
        let optional = |input: &TextInput| {
            let value = input.value().trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        let text_patch = |input: &TextInput, mixed: bool, clear: bool| {
            if clear {
                crate::actions::MetadataPatch::Clear
            } else if mixed && input.value().trim().is_empty() {
                crate::actions::MetadataPatch::Keep
            } else {
                optional(input).map_or(
                    crate::actions::MetadataPatch::Clear,
                    crate::actions::MetadataPatch::Set,
                )
            }
        };
        let labels_patch = |input: &TextInput, mixed: bool, clear: bool| {
            if clear {
                crate::actions::MetadataLabelsPatch::Clear
            } else {
                let raw = input.value().trim();
                let (mode, raw) = match raw.strip_prefix('=') {
                    Some(value) => ("set", value),
                    None => match raw.strip_prefix('+') {
                        Some(value) => ("add", value),
                        None => match raw.strip_prefix('-') {
                            Some(value) => ("remove", value),
                            None if self.metadata_editor_ids.len() > 1 && !raw.is_empty() => {
                                ("add", raw)
                            }
                            None => ("set", raw),
                        },
                    },
                };
                let values = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if mixed && values.is_empty() {
                    crate::actions::MetadataLabelsPatch::Keep
                } else {
                    match mode {
                        "add" => crate::actions::MetadataLabelsPatch::Add(values),
                        "remove" => crate::actions::MetadataLabelsPatch::Remove(values),
                        _ if values.is_empty() => crate::actions::MetadataLabelsPatch::Clear,
                        _ => crate::actions::MetadataLabelsPatch::Set(values),
                    }
                }
            }
        };
        let latitude = self.metadata_latitude_input.value().trim();
        let longitude = self.metadata_longitude_input.value().trim();
        let location = if self.metadata_force_clear[4] {
            crate::actions::MetadataPatch::Clear
        } else if self.metadata_mixed[4] && latitude.is_empty() && longitude.is_empty() {
            crate::actions::MetadataPatch::Keep
        } else if latitude.is_empty() && longitude.is_empty() {
            crate::actions::MetadataPatch::Clear
        } else {
            let (Ok(latitude), Ok(longitude)) = (latitude.parse::<f64>(), longitude.parse::<f64>())
            else {
                self.notice =
                    Some("Location needs numeric latitude and longitude together".to_owned());
                return;
            };
            crate::actions::MetadataPatch::Set(crate::model::PhotoLocation {
                latitude,
                longitude,
                label: optional(&self.metadata_place_input),
                provenance: crate::model::LocationProvenance {
                    source: if self.metadata_location_from_map {
                        "map".to_owned()
                    } else {
                        "user".to_owned()
                    },
                    scope: "shared".to_owned(),
                },
            })
        };
        self.commit_library_action(
            doc,
            &crate::actions::PhotosAction::SetMetadataBatch {
                photo_ids: self.metadata_editor_ids.clone(),
                folder: text_patch(
                    &self.metadata_folder_input,
                    self.metadata_mixed[0],
                    self.metadata_force_clear[0],
                ),
                people: labels_patch(
                    &self.metadata_people_input,
                    self.metadata_mixed[1],
                    self.metadata_force_clear[1],
                ),
                tags: labels_patch(
                    &self.metadata_tags_input,
                    self.metadata_mixed[2],
                    self.metadata_force_clear[2],
                ),
                captured_at: text_patch(
                    &self.metadata_captured_at_input,
                    self.metadata_mixed[3],
                    self.metadata_force_clear[3],
                ),
                location,
            },
        );
        let count = self.metadata_editor_ids.len();
        self.metadata_editor = false;
        self.metadata_editor_ids.clear();
        self.metadata_editor_focus = 0;
        self.notice = Some(if count == 1 {
            "Shared photo details saved".to_owned()
        } else {
            format!("Shared details saved for {count} photographs")
        });
        if count > 1 {
            self.finish_selection();
        }
    }

    fn set_metadata_editor_focus(&mut self, focus: usize) {
        let max = if self.metadata_editor_section == MetadataEditorSection::Organize {
            4
        } else {
            5
        };
        self.metadata_editor_focus = focus.min(max);
        self.metadata_editor_reveal_focus = true;
        let organize = self.metadata_editor_section == MetadataEditorSection::Organize;
        self.metadata_folder_input.set_focus(organize && focus == 0);
        self.metadata_people_input.set_focus(organize && focus == 1);
        self.metadata_tags_input.set_focus(organize && focus == 2);
        self.metadata_captured_at_input
            .set_focus(!organize && focus == 0);
        self.metadata_place_input.set_focus(!organize && focus == 1);
        self.metadata_latitude_input
            .set_focus(!organize && focus == 2);
        self.metadata_longitude_input
            .set_focus(!organize && focus == 3);
    }

    fn set_metadata_editor_section(&mut self, section: MetadataEditorSection) {
        self.metadata_editor_section = section;
        self.metadata_editor_scroll = 0.0;
        self.set_metadata_editor_focus(0);
    }

    fn clear_metadata_dimension(&mut self, dimension: usize) {
        let Some(force_clear) = self.metadata_force_clear.get_mut(dimension) else {
            return;
        };
        *force_clear = !*force_clear;
        if !*force_clear {
            return;
        }
        match dimension {
            0 => self.metadata_folder_input.set_text(""),
            1 => self.metadata_people_input.set_text(""),
            2 => self.metadata_tags_input.set_text(""),
            3 => self.metadata_captured_at_input.set_text(""),
            4 => {
                self.metadata_place_input.set_text("");
                self.metadata_latitude_input.set_text("");
                self.metadata_longitude_input.set_text("");
                self.metadata_location_from_map = false;
            }
            _ => {}
        }
    }

    /// Write an explicit offline-map point into the draft fields. No document
    /// update occurs here: batch edits retain their mixed values until the
    /// user deliberately saves this one shared provenance-bearing patch.
    fn set_metadata_location_from_map_point(&mut self, point: Point) -> bool {
        let bounds = self.metadata_location_map_bounds;
        if !bounds.contains(point) {
            return false;
        }
        let Some((latitude, longitude)) = self.metadata_location_map.unproject(
            point.x - bounds.x0,
            point.y - bounds.y0,
            bounds.width(),
            bounds.height(),
        ) else {
            return false;
        };
        self.metadata_latitude_input
            .set_text(&format!("{latitude:.6}"));
        self.metadata_longitude_input
            .set_text(&format!("{longitude:.6}"));
        // Re-centre the private draft map so its persistent crosshair is an
        // honest confirmation of the point just chosen.
        self.metadata_location_map.latitude = latitude;
        self.metadata_location_map.longitude = longitude;
        self.metadata_mixed[4] = false;
        self.metadata_force_clear[4] = false;
        self.metadata_location_from_map = true;
        self.notice =
            Some("Offline map point selected; it will be shared only after Save".to_owned());
        true
    }

    fn activate_metadata_editor_hit(&mut self, hit: MetadataEditorHit, doc: &mut AutoCommit) {
        match hit {
            MetadataEditorHit::Save => self.save_metadata_editor(doc),
            MetadataEditorHit::Cancel => self.cancel_metadata_editor(),
            MetadataEditorHit::Organize => {
                self.set_metadata_editor_section(MetadataEditorSection::Organize);
            }
            MetadataEditorHit::TimeAndPlace => {
                self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
            }
            MetadataEditorHit::ClearFolder => self.clear_metadata_dimension(0),
            MetadataEditorHit::ClearPeople => self.clear_metadata_dimension(1),
            MetadataEditorHit::ClearTags => self.clear_metadata_dimension(2),
            MetadataEditorHit::ClearCapturedAt => self.clear_metadata_dimension(3),
            MetadataEditorHit::ClearLocation => self.clear_metadata_dimension(4),
            MetadataEditorHit::PlaceOnMap => {
                let bounds = self.metadata_location_map_bounds;
                let _ = self.set_metadata_location_from_map_point(bounds.center());
            }
        }
    }

    fn begin_edit_album(&mut self, doc: &AutoCommit) {
        let Some(id) = self.active_album.clone() else {
            self.notice = Some("Choose an album to edit".to_owned());
            return;
        };
        let Some(album) = PhotosDoc::library(doc).album(&id).cloned() else {
            self.notice = Some("That album is no longer available".to_owned());
            self.active_album = None;
            return;
        };
        self.album_editor = true;
        self.album_editor_creating = false;
        self.album_editor_id = Some(album.id);
        self.album_name_input.set_text(&album.name);
        self.album_name_input.set_focus(true);
        self.album_editor_members = album.photo_ids.into_iter().collect();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
    }

    fn cancel_album_editor(&mut self) {
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some("Album changes discarded".to_owned());
    }

    fn save_album_editor(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.album_editor_id.clone() else {
            return;
        };
        let name = self.album_name_input.value().trim().to_owned();
        let library = PhotosDoc::library(doc);
        let stale = if self.album_editor_creating {
            library.album(&id).is_some()
        } else {
            library.album(&id).is_none()
        };
        if stale
            || name.is_empty()
            || !self
                .album_editor_members
                .iter()
                .all(|photo| library.photo(photo).is_some())
        {
            self.notice = Some(if stale {
                "Album changed elsewhere; review and save again".to_owned()
            } else {
                "An album needs a name and photographs still in this library".to_owned()
            });
            return;
        }
        let action = crate::actions::PhotosAction::SaveAlbum {
            id: id.clone(),
            name,
            // Preserve the library's stable browse order. HashSet iteration
            // would make two otherwise identical UI edits serialize to
            // arbitrary membership order and create needless collaboration
            // conflicts.
            photo_ids: library
                .photos
                .iter()
                .filter(|photo| self.album_editor_members.contains(&photo.id))
                .map(|photo| photo.id.clone())
                .collect(),
            create: self.album_editor_creating,
        };
        self.commit_library_action(doc, &action);
        self.active_album = Some(id);
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some("Album saved for everyone in this workspace".to_owned());
    }

    fn delete_active_album(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self
            .album_editor_id
            .clone()
            .or_else(|| self.active_album.clone())
        else {
            return;
        };
        self.commit_library_action(doc, &crate::actions::PhotosAction::RemoveAlbum { id });
        self.active_album = None;
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some("Album deleted; photographs remain in the library".to_owned());
    }

    /// Move through the bounded library shelf without changing any shared
    /// state.  Album editing keeps its unsaved membership locally, so paging
    /// must stay available while the sheet is open: otherwise a phone-sized
    /// sheet can leave most of a large library impossible to include.
    fn library_page_step(&self) -> usize {
        if self.large_text_toolbar && self.canvas_rect.width() < 620.0 {
            if self.album_editor { 2 } else { 1 }
        } else {
            LIBRARY_PAGE_SIZE
        }
    }

    fn page_library(&mut self, forward: bool) {
        let step = self.library_page_step();
        if forward {
            self.library_offset =
                (self.library_offset + step).min(self.library_total.saturating_sub(1));
        } else {
            self.library_offset = self.library_offset.saturating_sub(step);
        }
    }

    fn can_page_library(&self, forward: bool) -> bool {
        let step = self.library_page_step();
        if forward {
            self.library_offset.saturating_add(step) < self.library_total
        } else {
            self.library_offset > 0
        }
    }

    fn page_collection(&mut self, hit: CollectionPageHit) -> bool {
        let place_total = self
            .places_items
            .iter()
            .filter(|item| {
                self.places_open_key.as_ref().is_none_or(|key| {
                    item.photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == *key)
                })
            })
            .count();
        let page_size = if matches!(
            hit,
            CollectionPageHit::AlbumsPrevious | CollectionPageHit::AlbumsNext
        ) && self.library_destination == LibraryDestination::Albums
            && self.canvas_rect.width() >= 700.0
        {
            8
        } else {
            COLLECTION_PAGE_SIZE
        };
        let (page, total, forward) = match hit {
            CollectionPageHit::AlbumsPrevious => {
                (&mut self.albums_page, self.library_albums.len(), false)
            }
            CollectionPageHit::AlbumsNext => {
                (&mut self.albums_page, self.library_albums.len(), true)
            }
            CollectionPageHit::PeoplePrevious => {
                (&mut self.people_page, self.library_people.len(), false)
            }
            CollectionPageHit::PeopleNext => {
                (&mut self.people_page, self.library_people.len(), true)
            }
            CollectionPageHit::PlacesPrevious => (&mut self.places_page, place_total, false),
            CollectionPageHit::PlacesNext => (&mut self.places_page, place_total, true),
        };
        let pages = total.max(1).div_ceil(page_size);
        let next = if forward {
            page.saturating_add(1).min(pages.saturating_sub(1))
        } else {
            page.saturating_sub(1)
        };
        if next == *page {
            return false;
        }
        *page = next;
        true
    }

    fn library_page_label(&self) -> String {
        if self.library_total == 0 {
            "Showing 0 of 0".to_owned()
        } else {
            let first = self.library_offset + 1;
            let last = (self.library_offset + self.library_page_step()).min(self.library_total);
            format!("Showing {first}–{last} of {}", self.library_total)
        }
    }

    /// Change the local timeline density through any input source.  One step
    /// is shared by the +/- affordances, Ctrl-wheel and pinch, which makes
    /// their resulting layout testable for parity.
    fn adjust_timeline_density(&mut self, increase: bool) -> bool {
        let before = self.timeline_density;
        if increase {
            self.timeline_density = self.timeline_density.saturating_add(1).min(4);
        } else {
            self.timeline_density = self.timeline_density.saturating_sub(1);
        }
        if before == self.timeline_density {
            return false;
        }
        true
    }

    fn scroll_timeline_rows(&mut self, delta: f64) {
        // Keyboard rows remain a useful semantic step, but the canonical
        // offset is pixels so portrait and justified rows scroll smoothly.
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + delta * 168.0,
            self.timeline_viewport_height,
        );
    }

    fn scroll_timeline_pixels(&mut self, delta: f64) {
        if !delta.is_finite() {
            return;
        }
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + delta,
            self.timeline_viewport_height,
        );
    }

    /// Apply every chronological-index input through the same local scroll
    /// and density helpers used by the ordinary timeline.  The index never
    /// owns a second offset, which is what keeps a pinch anchor and a dragged
    /// date thumb in agreement.
    fn apply_timeline_index_intent(&mut self, intent: &DateIndexIntent) -> bool {
        match *intent {
            DateIndexIntent::ScrollTo { position } => {
                self.timeline_scroll_y =
                    DateIndexedScrollbar::offset_for(self.timeline_index_metrics, position);
                true
            }
            DateIndexIntent::AdjustDensity { delta } => match delta.cmp(&0) {
                std::cmp::Ordering::Greater => self.adjust_timeline_density(true),
                std::cmp::Ordering::Less => self.adjust_timeline_density(false),
                std::cmp::Ordering::Equal => false,
            },
            // A preview is presentation only. The widget has retained it for
            // paint and AT's current value; it must not make collaboration
            // history noisy by changing a collection just because it was
            // hovered.
            DateIndexIntent::PreviewChanged(_) => true,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "embedded TextInput uses the Widget event contract; keyboard and pointer handling stay beside the album-sheet controls"
    )]
    fn handle_album_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.album_editor {
            return false;
        }
        for (_, button) in &mut self.album_page_buttons {
            let mut sink = None;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if let Some(hit) = self.album_page_pending.take() {
            match hit {
                AlbumEditorHit::PreviousPage => self.page_library(false),
                AlbumEditorHit::NextPage => self.page_library(true),
                _ => {}
            }
            *ctx.handled = true;
            return true;
        }
        let mut sink = None;
        self.album_name_input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                focus_target: &mut sink,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::PageDown) => {
                    self.page_library(true);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::PageUp) => {
                    self.page_library(false);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Tab) => {
                    let steps = if self.album_editor_creating { 3 } else { 4 };
                    self.album_editor_focus = if ctx.modifiers.state().shift_key() {
                        (self.album_editor_focus + steps - 1) % steps
                    } else {
                        (self.album_editor_focus + 1) % steps
                    };
                    self.album_name_input
                        .set_focus(self.album_editor_focus == 0);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_album_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key() =>
                {
                    self.save_album_editor(ctx.state);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    match (self.album_editor_creating, self.album_editor_focus) {
                        (_, 1) => self.cancel_album_editor(),
                        (false, 2) if self.album_delete_confirm => {
                            self.delete_active_album(ctx.state);
                        }
                        (false, 2) => self.album_delete_confirm = true,
                        (true, 2) | (false, 3) => self.save_album_editor(ctx.state),
                        _ => {}
                    }
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
            if self.metadata_editor_section == MetadataEditorSection::TimeAndPlace
                && matches!(self.metadata_editor_focus, 2 | 3)
            {
                // Coordinates subsequently authored through the shared text
                // input are no longer accurately described as a map pin.
                self.metadata_location_from_map = false;
            }
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && self.metadata_editor_section == MetadataEditorSection::TimeAndPlace
            && self.set_metadata_location_from_map_point(self.pointer)
        {
            *ctx.handled = true;
            return true;
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            if let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
            {
                match hit {
                    AlbumEditorHit::Save => self.save_album_editor(ctx.state),
                    AlbumEditorHit::Cancel => self.cancel_album_editor(),
                    AlbumEditorHit::Delete => {
                        self.album_delete_confirm = true;
                        self.notice = Some("Press Delete album? again to keep photographs and delete only the album".to_owned());
                    }
                    AlbumEditorHit::ConfirmDelete => self.delete_active_album(ctx.state),
                    _ => {}
                }
                *ctx.handled = true;
                return true;
            }
            if let Some((id, _, _)) = self
                .library_cards
                .iter()
                .find(|(_, rect, _)| rect.contains(self.pointer))
                .cloned()
            {
                if !self.album_editor_members.insert(id.clone()) {
                    self.album_editor_members.remove(&id);
                }
                self.album_delete_confirm = false;
                *ctx.handled = true;
                return true;
            }
        }
        true
    }

    fn handle_collection_page_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.library_mode || self.collection_page_hits.is_empty() {
            return false;
        }
        for (hit, button) in &mut self.collection_page_buttons {
            if self
                .collection_page_hits
                .iter()
                .any(|(visible, _, _)| visible == hit)
            {
                let mut sink = None;
                button.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        focus_target: &mut sink,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            }
        }
        if let Some(hit) = self.collection_page_pending.take() {
            self.page_collection(hit);
            *ctx.handled = true;
            return true;
        }
        false
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keyboard, pointer and embedded TextInput routing for the metadata sheet stay together under the Widget event contract"
    )]
    fn handle_metadata_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.metadata_editor {
            return false;
        }
        // The sheet floats over a custom media surface, so dispatch its
        // retained controls explicitly. They still use the normal widget
        // event contract and publish the same action surface as every other
        // button in the app.
        for (hit, button) in &mut self.metadata_editor_buttons {
            let body_control = matches!(
                hit,
                MetadataEditorHit::ClearFolder
                    | MetadataEditorHit::ClearPeople
                    | MetadataEditorHit::ClearTags
                    | MetadataEditorHit::ClearCapturedAt
                    | MetadataEditorHit::ClearLocation
            );
            if body_control
                && matches!(event, winit::event::WindowEvent::MouseInput { .. })
                && !self.metadata_editor_body.contains(self.pointer)
            {
                continue;
            }
            let mut sink = None;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit, ctx.state);
            *ctx.handled = true;
            return true;
        }
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            let focus_count = if self.metadata_editor_section == MetadataEditorSection::Organize {
                5
            } else {
                6
            };
            match &key.logical_key {
                Key::Named(NamedKey::Tab) => {
                    self.set_metadata_editor_focus(if ctx.modifiers.state().shift_key() {
                        (self.metadata_editor_focus + focus_count - 1) % focus_count
                    } else {
                        (self.metadata_editor_focus + 1) % focus_count
                    });
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key() =>
                {
                    self.save_metadata_editor(ctx.state);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == focus_count - 2 => {
                    self.cancel_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == focus_count - 1 => {
                    self.save_metadata_editor(ctx.state);
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
        }
        // A wheel over the sheet scrolls its field list (clamped at paint,
        // where the content height is known).
        if let winit::event::WindowEvent::MouseWheel { delta, .. } = event
            && self.metadata_editor_body.contains(self.pointer)
        {
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 40.0,
                // Physical pixels, like every pointer position this app
                // converts; without the division a 2× display scrolls twice
                // the trackpad distance.
                winit::event::MouseScrollDelta::PixelDelta(position) => {
                    position.y / ctx.scale_factor
                }
            };
            self.metadata_editor_scroll = (self.metadata_editor_scroll - dy).max(0.0);
            *ctx.handled = true;
            return true;
        }
        // A press outside the field list's visible region belongs to the
        // pinned footer (or the scrim), never to an input scrolled under it.
        let press_outside_body = matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            }
        ) && !self.metadata_editor_body.contains(self.pointer);
        for input in [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
            &mut self.metadata_place_input,
            &mut self.metadata_latitude_input,
            &mut self.metadata_longitude_input,
        ] {
            if press_outside_body {
                break;
            }
            let mut sink = None;
            input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && let Some((hit, _)) = self
            .metadata_editor_hits
            .iter()
            .find(|(_, rect)| rect.contains(self.pointer))
            .copied()
        {
            self.activate_metadata_editor_hit(hit, ctx.state);
            *ctx.handled = true;
        }
        true
    }

    /// Perform a device-local operation and surface failures to canonical
    /// operation dispatch. A local notice alone is not an invocation result.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match over device-local Photos operations keeps their effects auditable"
    )]
    fn perform_effect(
        &mut self,
        operation: crate::actions::PhotosOperation,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        match operation {
            // Resource/search routing has already brought this Photos
            // instance forward. Viewing does not alter document or OS state.
            crate::actions::PhotosOperation::Open { path: None } => {
                self.import_content = Some(LiveContent::read(doc));
                self.picker_stale = false;
                let filter = ui::portal::FileFilter::new(&tr("photos-filter-images"))
                    .mimetype("image/png")
                    .mimetype("image/jpeg")
                    .mimetype("image/webp");
                if crate::files::native_paths() {
                    self.picker = Some(
                        ui::portal::PickRequest::open(&tr("photos-open-title"), &tr("photos-open"))
                            .thread_name("photos-portal-open")
                            .filter(filter)
                            .all_files()
                            .send(),
                    );
                } else {
                    self.contents_picker = Some(
                        ui::portal::PickRequest::open_multiple(
                            &tr("photos-open-title"),
                            &tr("photos-open"),
                        )
                        .filter(filter)
                        .send_contents(),
                    );
                }
            }
            crate::actions::PhotosOperation::ImportDirectory {
                path: None,
                retain_exif_location,
            } => {
                self.directory_picker_retain_exif_location = retain_exif_location;
                self.directory_picker = Some(
                    ui::portal::PickRequest::directory("Import photo folder", "Import folder")
                        .thread_name("photos-portal-directory-import")
                        .send(),
                );
            }
            crate::actions::PhotosOperation::ImportDirectory {
                path: Some(path),
                retain_exif_location,
            } => {
                self.begin_directory_import(
                    std::path::PathBuf::from(path),
                    None,
                    DirectoryImportMode::Import,
                    retain_exif_location,
                );
            }
            crate::actions::PhotosOperation::ImportCameraMedia { media_id, name } => {
                return Err(format!(
                    "Camera import requires document dispatch: {media_id} ({name})"
                ));
            }
            crate::actions::PhotosOperation::ReindexDirectory {
                path,
                retain_exif_location,
            } => {
                self.begin_directory_import(
                    std::path::PathBuf::from(path),
                    None,
                    DirectoryImportMode::Reindex,
                    retain_exif_location,
                );
            }
            crate::actions::PhotosOperation::AnalyzeText { .. }
            | crate::actions::PhotosOperation::AnalyzeImage { .. }
            | crate::actions::PhotosOperation::AnalyzePeopleLibrary
            | crate::actions::PhotosOperation::AnalyzeLibrary
            | crate::actions::PhotosOperation::PauseLibraryAnalysis
            | crate::actions::PhotosOperation::ResumeLibraryAnalysis
            | crate::actions::PhotosOperation::RetryLibraryAnalysisFailures => {
                return Err("local image analysis requires document dispatch".to_owned());
            }
            crate::actions::PhotosOperation::Copy => {
                self.copy_to_clipboard();
            }
            crate::actions::PhotosOperation::ShareReference { resource_id } => {
                let exists = PhotosDoc::image(doc)
                    .is_some_and(|image| crate::actions::resource_id(&image) == resource_id)
                    || PhotosDoc::photo(doc, &resource_id).is_some();
                if exists {
                    // This is deliberately a local clipboard handoff, not an
                    // invented upload or recipient-delivery success. The
                    // address identifies either the open editor image or an
                    // unselected, addressable library card.
                    ui::clipboard_copy(&format!("tilo://resource/{resource_id}"));
                    self.notice = Some(tr("photos-share-copied"));
                } else {
                    let error = tr("photos-share-failed");
                    self.notice = Some(error.clone());
                    return Err(error);
                }
            }
            crate::actions::PhotosOperation::Save { path } => {
                if let Some(open) = PhotosDoc::image(doc) {
                    self.save_to(path.as_deref().unwrap_or(&Self::suggested_save_path(&open)));
                }
            }
            crate::actions::PhotosOperation::ExportEditedContent { .. } => {
                return Err("edited content export requires lifecycle dispatch".into());
            }
            crate::actions::PhotosOperation::View => {
                self.library_mode = false;
                self.album_editor = false;
                self.metadata_editor = false;
            }
            crate::actions::PhotosOperation::ViewLibrary => self.show_library(),
            crate::actions::PhotosOperation::ViewAlbums => self.show_albums(),
            crate::actions::PhotosOperation::ViewTimeline { anchor } => {
                self.view_timeline(doc, anchor.as_ref())?;
            }
            crate::actions::PhotosOperation::ViewPeople => {
                self.show_library();
                self.people_mode = true;
                self.library_destination = LibraryDestination::People;
                self.notice = Some(
                    "People is local-first: install a signed face model, then review every suggestion before sharing labels".to_owned(),
                );
            }
            crate::actions::PhotosOperation::ViewLibraryAnalysis => {
                self.show_library_analysis();
                self.notice = Some(
                    "Local analysis makes descriptions, objects, pets, and text searchable"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Settings › Models to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::EnablePeopleRecognition => {
                self.people_controller.set_recognition_enabled(true);
                self.people_controller
                    .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                self.notice = Some(self.people_controller.capability().summary().to_owned());
            }
            crate::actions::PhotosOperation::ViewFavorites => self.view_favorites(doc)?,
            crate::actions::PhotosOperation::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.places_open_key = None;
                self.notice = Some("Showing offline Places".to_owned());
            }
            crate::actions::PhotosOperation::ViewPlace { place_key } => {
                self.open_place_collection(doc, &place_key)?;
            }
            crate::actions::PhotosOperation::ViewAlbum { .. }
            | crate::actions::PhotosOperation::ViewFolder { .. }
            | crate::actions::PhotosOperation::ViewPerson { .. }
            | crate::actions::PhotosOperation::ViewTag { .. }
            | crate::actions::PhotosOperation::ViewObject { .. }
            | crate::actions::PhotosOperation::ViewPet { .. }
            | crate::actions::PhotosOperation::Open { path: Some(_) } => {}
        }
        Ok(())
    }

    /// Navigate to an extant stable album id without producing a CRDT action.
    /// This is deliberately separate from `perform_effect`: callers need a
    /// useful stale-address error instead of silently falling back to all
    /// photographs if a collaborator removed the album after search indexed
    /// it.
    fn view_album(&mut self, doc: &AutoCommit, album_id: &str) -> Result<(), String> {
        if PhotosDoc::album(doc, album_id).is_none() {
            return Err("that album is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = Some(album_id.to_owned());
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing selected album".to_owned());
        Ok(())
    }

    /// Enter a metadata-derived library view after verifying the label still
    /// exists. An old global-search result therefore fails honestly when a
    /// collaborator removes the final matching photograph.
    fn view_metadata_filter(
        &mut self,
        doc: &AutoCommit,
        folder: Option<String>,
        person: Option<String>,
        tag: Option<String>,
    ) -> Result<(), String> {
        let library = PhotosDoc::library(doc);
        let exists = match (&folder, &person, &tag) {
            (Some(folder), None, None) => library
                .photos
                .iter()
                .any(|photo| photo.folder.as_deref() == Some(folder)),
            (None, Some(person), None) => library
                .photos
                .iter()
                .any(|photo| photo.people.iter().any(|value| value == person)),
            (None, None, Some(tag)) => library
                .photos
                .iter()
                .any(|photo| photo.tags.iter().any(|value| value == tag)),
            _ => false,
        };
        if !exists {
            return Err("that metadata view is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = None;
        self.active_folder = folder;
        self.active_person = person;
        self.active_tag = tag;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing shared photo metadata view".to_owned());
        Ok(())
    }

    /// Open one exact intelligence-derived collection. These labels are
    /// still local navigation; only the accepted analysis record is shared.
    fn view_analysis_filter(
        &mut self,
        doc: &AutoCommit,
        label: String,
        pet: bool,
    ) -> Result<(), String> {
        let exists = PhotosDoc::library(doc).photos.iter().any(|photo| {
            photo.analysis.as_ref().is_some_and(|analysis| {
                if pet {
                    analysis.pets.contains(&label)
                } else {
                    analysis.objects.contains(&label)
                }
            })
        });
        if !exists {
            return Err("that analysis collection is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = (!pet).then_some(label.clone());
        self.active_pet = pet.then_some(label);
        self.active_favorites = false;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.notice = Some("Showing explicitly shared image analysis".to_owned());
        Ok(())
    }

    /// Navigate to the stable favorites resource after verifying that it is
    /// still non-empty. A stale global-search result must fail rather than
    /// silently changing the user's view to an unrelated all-photos shelf.
    fn view_favorites(&mut self, doc: &AutoCommit) -> Result<(), String> {
        if !PhotosDoc::library(doc)
            .photos
            .iter()
            .any(|photo| photo.favorite)
        {
            return Err("there are no favorite photographs in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = true;
        self.library_offset = 0;
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing favorite photographs".to_owned());
        Ok(())
    }

    /// Project the open photograph's portable library record into the small
    /// viewer card. Content identity wins; old path-only records retain their
    /// legacy match so opening one still has useful context.
    fn viewer_details_for(doc: &AutoCommit, open: &OpenImage, edits: Edits) -> ViewerDetails {
        let photo = PhotosDoc::library(doc).photos.into_iter().find(|photo| {
            match (&open.content, &photo.image.content) {
                (Some(open_content), Some(photo_content)) => open_content == photo_content,
                (None, None) => open.path == photo.image.path,
                _ => false,
            }
        });
        let (width, height) = open.edited_size(&edits);
        ViewerDetails {
            name: open.name.clone(),
            dimensions: format!("{width} × {height}"),
            edited: !edits.is_identity(),
            favorite: photo.as_ref().is_some_and(|photo| photo.favorite),
            description: photo
                .as_ref()
                .and_then(|photo| photo.analysis.as_ref())
                .map(|analysis| analysis.description.trim())
                .filter(|description| !description.is_empty())
                .map(str::to_owned),
            captured_at: photo.as_ref().and_then(|photo| photo.captured_at.clone()),
            place: photo.as_ref().and_then(|photo| {
                photo
                    .location
                    .as_ref()
                    .filter(|location| location.is_valid())
                    .map(crate::model::PhotoLocation::display_name)
            }),
            people: photo
                .as_ref()
                .map_or_else(Vec::new, |photo| photo.people.clone()),
            tags: photo
                .as_ref()
                .map_or_else(Vec::new, |photo| photo.tags.clone()),
            folder: photo.and_then(|photo| photo.folder),
        }
    }

    /// Re-derive the rendering if the path or the edits have moved.
    fn refresh(&mut self, doc: &AutoCommit) {
        let Some(open) = PhotosDoc::image(doc) else {
            self.rendered = None;
            self.viewer = None;
            self.viewer_details = None;
            self.original = None;
            self.canvas_state = if self.notice.is_some() {
                CanvasState::Error
            } else {
                CanvasState::Empty
            };
            return;
        };
        let edits = PhotosDoc::edits(doc);
        self.viewer_details = Some(Self::viewer_details_for(doc, &open, edits));

        // Load the original if this is a picture we have not decoded yet. A
        // peer may have opened it, so this is not only the local Open path.
        let identity = decode_key(&open);
        if self.original_identity.as_ref() != Some(&identity) || self.original.is_none() {
            let key = decode_key(&open);
            if self.original_decode_failed.as_deref() == Some(&key) {
                self.original = None;
                self.rendered = None;
                self.viewer = None;
                self.canvas_state = CanvasState::Error;
                return;
            }
            let bytes = crate::doc::bytes_of(&open, &self.content);
            if let Some(img) = bytes.as_deref().and_then(pixels::decode) {
                self.original = Some((open.path, img));
                self.original_identity = Some(identity.clone());
                self.original_decode_failed = None;
            } else {
                self.original = None;
                self.rendered = None;
                self.viewer = None;
                if bytes.is_some() {
                    self.original_decode_failed = Some(key);
                    self.canvas_state = CanvasState::Error;
                } else {
                    self.canvas_state = CanvasState::Loading;
                }
                return;
            }
        }
        let Some((_, original)) = self.original.as_ref() else {
            return;
        };

        let unchanged = self
            .rendered
            .as_ref()
            .is_some_and(|r| r.identity == identity && r.edits == edits);
        if unchanged {
            return;
        }

        let rgba = pixels::render(original, edits.crop, edits.rotate_quarters);
        let image = to_peniko(&rgba);
        // `ImageViewer` has no in-place image swap yet, so an edit rebuilds
        // the widget — which also resets zoom/pan to fit-to-viewport. For
        // "crop to view" that is the right outcome anyway: the new picture is
        // exactly what was framed, so showing it at zoom 1 is showing what was
        // asked for.
        self.viewer = Some(ImageViewer::new(image));
        self.canvas_state = CanvasState::Ready;
        self.rendered = Some(Rendered {
            identity,
            edits,
            rgba,
        });
    }

    /// Drain a click that a button posted, and act on it.
    fn drain_pending(&mut self, doc: &mut AutoCommit) {
        let Some(hit) = self.pending.take() else {
            return;
        };
        let edits = PhotosDoc::edits(doc);
        match hit {
            Hit::Open => {
                let _ =
                    self.perform_effect(crate::actions::PhotosOperation::Open { path: None }, doc);
            }
            Hit::Library => {
                let _ = self.perform_effect(crate::actions::PhotosOperation::ViewLibrary, doc);
            }
            Hit::RotateLeft => self.commit_edits(doc, edits.rotated_left()),
            Hit::RotateRight => self.commit_edits(doc, edits.rotated_right()),
            Hit::CropToView => {
                let view = self.visible_rect();
                // Cropping to the whole image is a no-op; say so rather than
                // writing an edit that changes nothing.
                if view.is_full() {
                    self.notice = Some(tr("photos-crop-noop"));
                } else {
                    self.commit_edits(doc, edits.cropped_from_view(view));
                }
            }
            Hit::Reset => self.commit_edits(doc, Edits::default()),
            Hit::Copy => {
                let _ = self.perform_effect(crate::actions::PhotosOperation::Copy, doc);
            }
            Hit::Share => {
                if let Some(image) = PhotosDoc::image(doc) {
                    let _ = self.perform_effect(
                        crate::actions::PhotosOperation::ShareReference {
                            resource_id: crate::actions::resource_id(&image),
                        },
                        doc,
                    );
                } else {
                    self.notice = Some(tr("photos-share-failed"));
                }
            }
            Hit::Save => {
                let _ =
                    self.perform_effect(crate::actions::PhotosOperation::Save { path: None }, doc);
            }
            Hit::More => {
                self.compact_more_open = !self.compact_more_open;
                self.compact_more_focus = 0;
            }
        }
    }

    /// Synthetic semantics are necessary because Photos composes its toolbar
    /// directly into the root layout instead of exposing it through
    /// [`Widget::children`]. Keep the target based on the stable toolbar key,
    /// not its visual index: the narrow layout changes labels, but an assistive
    /// technology holding "Rotate right" must keep reaching that action.
    fn toolbar_semantics(&self) -> AccessNode {
        let visible: std::collections::HashSet<_> = self
            .toolbar_slots()
            .iter()
            .filter_map(|slot| match slot {
                Slot::Item(index) => Some(*index),
                _ => None,
            })
            .collect();
        let mut children: Vec<_> = self
            .buttons
            .iter()
            .zip(TOOLBAR)
            .enumerate()
            .filter(|(index, _)| visible.contains(index))
            .map(|(_, ((_, button), (_, key, _, _)))| {
                AccessNode::new(Role::Button)
                    .with_name(tr(key))
                    .with_state(AccessState {
                        focusable: button.is_focusable(),
                        focused: button.is_focused(),
                        disabled: !button.is_enabled(),
                        ..Default::default()
                    })
                    .with_bounds(button.get_rect())
                    .with_action_target(format!("photos:toolbar:{key}"))
            })
            .collect();
        if self.compact_more_open && self.toolbar_slots() == COMPACT_SLOTS {
            let menu_children = [
                ("photos-crop", "photos-crop"),
                ("photos-reset", "photos-reset"),
                ("photos-copy", "photos-copy"),
                ("photos-share", "photos-share"),
                ("photos-save", "photos-save"),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (target_key, label_key))| {
                let menu_hit = COMPACT_MORE_ENTRIES.get(index).copied();
                let disabled = TOOLBAR
                    .iter()
                    .position(|(_, key, _, _)| *key == target_key)
                    .and_then(|index| self.buttons.get(index))
                    .is_none_or(|(_, button)| !button.is_enabled());
                AccessNode::new(Role::MenuItem)
                    .with_name(tr(label_key))
                    .with_state(AccessState {
                        focusable: !disabled,
                        focused: index == self.compact_more_focus,
                        disabled,
                        ..Default::default()
                    })
                    .with_bounds(
                        self.compact_more_hits
                            .iter()
                            .find(|(hit, _)| Some(*hit) == menu_hit)
                            .map_or(Rect::ZERO, |(_, bounds)| *bounds),
                    )
                    .with_action_target(format!("photos:toolbar:{target_key}"))
            })
            .collect();
            children.push(
                AccessNode::new(Role::Menu)
                    .with_name(tr("photos-more"))
                    .with_children(menu_children),
            );
        }
        AccessNode::new(Role::Toolbar)
            .with_name(tr("photos-tools"))
            .with_bounds(self.toolbar_rect)
            .with_hashed_key("photos:toolbar")
            .with_children(children)
    }

    /// The canvas has no intrinsic source name — it is raw pixel data — so the
    /// app supplies the file name and current edit state that a sighted person
    /// gets from the status bar. The status remains a polite live region for
    /// copy/save/open failures without stealing keyboard focus.
    #[allow(
        clippy::too_many_lines,
        reason = "the synthetic canvas accessibility tree keeps each visual hit target beside its stable semantic route"
    )]
    fn accessibility_tree(&self) -> AccessNode {
        let image_label = || {
            self.original.as_ref().map_or_else(
                || tr("photos-image"),
                |(path, _)| path.rsplit('/').next().unwrap_or(path).to_owned(),
            )
        };
        let (image_name, mut image_description) = match self.rendered.as_ref() {
            Some(rendered) if rendered.edits.is_identity() => {
                (image_label(), tr("photos-image-help"))
            }
            Some(_) => (
                format!("{} — {}", image_label(), tr("photos-edited")),
                tr("photos-image-help"),
            ),
            None => self.empty_state_text(),
        };
        if let Some(details) = self.viewer_details.as_ref() {
            let mut shared = Vec::new();
            if let Some(description) = details.description.as_deref() {
                shared.push(format!("{}: {description}", tr("photos-description")));
            }
            if let Some(captured_at) = details.captured_at.as_deref() {
                shared.push(format!("{}: {captured_at}", tr("photos-captured")));
            }
            if let Some(place) = details.place.as_deref() {
                shared.push(format!("{}: {place}", tr("photos-place")));
            }
            if !details.people.is_empty() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-people"),
                    details.people.join(", ")
                ));
            }
            if !details.tags.is_empty() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-tags"),
                    details.tags.join(", ")
                ));
            }
            if let Some(folder) = details.folder.as_deref() {
                shared.push(format!("{}: {folder}", tr("photos-folder")));
            }
            if !shared.is_empty() {
                image_description.push_str(". ");
                image_description.push_str(&tr("photos-shared-details"));
                image_description.push_str(": ");
                image_description.push_str(&shared.join(". "));
            }
        }
        let canvas = if self.library_mode {
            let mut cards = Vec::new();
            cards.extend(self.places_hits.iter().filter_map(|(hit, bounds)| {
                let (name, target) = match hit {
                    PlacesHit::Timeline => ("Timeline", "photos:library:timeline"),
                    PlacesHit::Albums => ("Albums", "photos:library:albums"),
                    PlacesHit::People => ("People", "photos:library:people"),
                    PlacesHit::Places => ("Places", "photos:library:places"),
                    PlacesHit::Analysis => ("Photo analysis", "photos:library:analysis"),
                    PlacesHit::ToggleList
                    | PlacesHit::ZoomIn
                    | PlacesHit::ZoomOut
                    | PlacesHit::Place(_)
                    | PlacesHit::Photo(_) => return None,
                };
                Some(
                    AccessNode::new(Role::RadioButton)
                        .with_name(name)
                        .with_bounds(*bounds)
                        .with_state(AccessState {
                            focusable: true,
                            checked: Some(match hit {
                                PlacesHit::Timeline => {
                                    self.library_destination == LibraryDestination::Timeline
                                }
                                PlacesHit::Albums => {
                                    self.library_destination == LibraryDestination::Albums
                                }
                                PlacesHit::People => {
                                    self.library_destination == LibraryDestination::People
                                }
                                PlacesHit::Places => {
                                    self.library_destination == LibraryDestination::Places
                                }
                                PlacesHit::Analysis => {
                                    self.library_destination == LibraryDestination::Analysis
                                }
                                _ => false,
                            }),
                            ..Default::default()
                        })
                        .with_action_target(target),
                )
            }));
            if self.library_destination == LibraryDestination::Analysis {
                cards.extend(
                    self.library_analysis_buttons
                        .iter()
                        .filter_map(|(hit, button)| {
                            let bounds = button.get_rect();
                            if bounds == Rect::ZERO {
                                return None;
                            }
                            let (name, target) = match hit {
                                LibraryAnalysisHit::Start => {
                                    ("Analyze unprocessed photos", "photos:analysis:start")
                                }
                                LibraryAnalysisHit::Pause => {
                                    ("Pause after this photo", "photos:analysis:pause")
                                }
                                LibraryAnalysisHit::Resume => {
                                    ("Resume photo analysis", "photos:analysis:resume")
                                }
                                LibraryAnalysisHit::RetryFailures => {
                                    ("Retry failed photos", "photos:analysis:retry")
                                }
                            };
                            Some(
                                AccessNode::new(Role::Button)
                                    .with_name(name)
                                    .with_bounds(bounds)
                                    .with_state(AccessState {
                                        focusable: button.is_focusable(),
                                        focused: button.is_focused(),
                                        disabled: !button.is_enabled(),
                                        ..Default::default()
                                    })
                                    .with_action_target(target),
                            )
                        }),
                );
                cards.extend(
                    self.library_analysis_failure_hits
                        .iter()
                        .map(|(id, bounds)| {
                            let failure = self
                                .library_analysis
                                .failures()
                                .find(|failure| failure.photo_id == *id);
                            AccessNode::new(Role::Button)
                                .with_name(failure.map_or_else(
                                    || "Open failed photograph".to_owned(),
                                    |failure| format!("Open {}", failure.name),
                                ))
                                .with_description(
                                    failure.map_or("Local analysis failed", |failure| {
                                        failure.message.as_str()
                                    }),
                                )
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(format!("photos:analysis:failure:{id}"))
                                .with_hashed_key(format!("photos:analysis-failure:{id}"))
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name("Photo analysis")
                    .with_description("Local descriptions, text, objects, and pets. Pixels stay on this device; approved compact results become searchable in the shared library.")
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:library-analysis")
                    .with_live(Politeness::Polite)
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::Albums {
                cards.extend(self.album_cards.iter().filter_map(|(id, bounds)| {
                    let id = id.as_ref()?;
                    let (_, name, count) = self
                        .library_albums
                        .iter()
                        .find(|(album_id, _, _)| album_id == id)?;
                    Some(
                        AccessNode::new(Role::ListItem)
                            .with_name(format!(
                                "{name}, {count} photo{}",
                                if *count == 1 { "" } else { "s" }
                            ))
                            .with_description("Open this shared album")
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_hashed_key(format!("photos:album-card:{id}"))
                            .with_action_target(format!("photos:library-album:{id}")),
                    )
                }));
                cards.extend(self.album_edit_cards.iter().map(|(id, bounds)| {
                    AccessNode::new(Role::Button)
                        .with_name("Edit album")
                        .with_bounds(*bounds)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                        .with_hashed_key(format!("photos:album-edit:{id}"))
                        .with_action_target(format!("photos:album-edit:{id}"))
                }));
                AccessNode::new(Role::List)
                    .with_name("Albums")
                    .with_description("Shared photo collections")
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:albums")
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::People || self.people_mode {
                if self.people_selection.is_some() && !self.people_merge_mode {
                    cards.push(
                        AccessNode::new(Role::TextInput)
                            .with_name("Person name")
                            .with_value(self.people_name_input.value())
                            .with_bounds(self.people_name_input.get_rect())
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.people_name_input.is_focused(),
                                ..Default::default()
                            })
                            .with_hashed_key("photos:people:name")
                            .with_action_target("photos:people:name"),
                    );
                }
                cards.extend(
                    self.people_controls
                        .iter()
                        .enumerate()
                        .map(|(index, control)| {
                            AccessNode::new(Role::Button)
                                .with_name(control.label.clone())
                                .with_bounds(control.button.get_rect())
                                .with_state(AccessState {
                                    focusable: control.button.is_focusable(),
                                    focused: control.button.is_focused(),
                                    disabled: !control.button.is_enabled(),
                                    ..Default::default()
                                })
                                .with_action_target(format!("photos:people:control:{index}"))
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name("People")
                    .with_description("Approved shared names and private suggestions awaiting review on this device")
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:people")
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::Places {
                cards.extend(self.places_hits.iter().filter_map(|(hit, bounds)| {
                    let (name, target) = match hit {
                        PlacesHit::Timeline
                        | PlacesHit::Albums
                        | PlacesHit::People
                        | PlacesHit::Places
                        | PlacesHit::Analysis => return None,
                        PlacesHit::ToggleList => (
                            if self.places_list_mode {
                                "Show map"
                            } else {
                                "Show list"
                            },
                            "photos:places:toggle-list".to_owned(),
                        ),
                        PlacesHit::ZoomIn => ("Zoom in", "photos:places:zoom-in".to_owned()),
                        PlacesHit::ZoomOut => ("Zoom out", "photos:places:zoom-out".to_owned()),
                        PlacesHit::Place(key) => {
                            ("Open place collection", format!("photos:places:open:{key}"))
                        }
                        PlacesHit::Photo(id) => {
                            ("Open photograph", format!("photos:places:photo:{id}"))
                        }
                    };
                    Some(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    )
                }));
                // These semantic controls intentionally share the same pure
                // camera operation as keyboard arrows and pointer drag. They
                // make panning available to screen readers and agents without
                // introducing coordinate-only automation.
                for (name, target) in [
                    ("Pan map left", "photos:places:pan-left"),
                    ("Pan map right", "photos:places:pan-right"),
                    ("Pan map up", "photos:places:pan-up"),
                    ("Pan map down", "photos:places:pan-down"),
                ] {
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_description(
                                "Move the offline map without changing shared location data",
                            )
                            .with_bounds(self.places_map_bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
                cards.extend(
                    self.collection_page_hits
                        .iter()
                        .map(|(hit, bounds, enabled)| {
                            collection_page_access_node(*hit, *bounds, *enabled)
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name("Places")
                    .with_description("Offline map of explicitly shared photo locations. Arrow keys pan; plus and minus zoom; L changes to the list.")
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:places")
                    .with_children(cards)
            } else {
                cards.extend(
                    self.library_cards
                        .iter()
                        .filter_map(|(id, bounds, favorite)| {
                            let item = self.library_items.iter().find(|item| item.id == *id)?;
                            let name = &item.name;
                            let open_target = format!("photos:library-photo:{id}");
                            let mut children = vec![if self.selection_mode {
                                AccessNode::new(Role::ToggleButton)
                                .with_name(name.clone())
                                .with_description(
                                    "Selected photographs receive the chosen shared batch action",
                                )
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    checked: Some(self.selected_photo_ids.contains(id)),
                                    ..Default::default()
                                })
                                .with_action_target(open_target)
                            } else if self.album_editor {
                                AccessNode::new(Role::ToggleButton)
                                    .with_name(name.clone())
                                    .with_description("Included in this album")
                                    .with_bounds(*bounds)
                                    .with_state(AccessState {
                                        focusable: true,
                                        checked: Some(self.album_editor_members.contains(id)),
                                        ..Default::default()
                                    })
                                    .with_action_target(open_target)
                            } else {
                                AccessNode::new(Role::Button)
                                    .with_name(name.clone())
                                    .with_description(tr("photos-open"))
                                    .with_bounds(*bounds)
                                    .with_state(AccessState {
                                        focusable: true,
                                        ..Default::default()
                                    })
                                    .with_action_target(open_target)
                            }];
                            if !self.album_editor && !self.selection_mode {
                                let actions = card_action_controls(*bounds);
                                let favorite_target = format!("photos:library-favorite:{id}");
                                children.push(
                                    AccessNode::new(Role::ToggleButton)
                                        .with_name("Favorite")
                                        .with_bounds(actions.favorite)
                                        .with_state(AccessState {
                                            focusable: true,
                                            checked: Some(*favorite),
                                            ..Default::default()
                                        })
                                        .with_action_target(favorite_target),
                                );
                                let confirmed = self.library_remove_confirm.as_deref() == Some(id);
                                children.push(
                                    AccessNode::new(Role::Button)
                                        .with_name(if confirmed {
                                            format!("Confirm removing {name} from library")
                                        } else {
                                            format!("Remove {name} from library")
                                        })
                                        .with_description(
                                            "Removes this photograph from every album",
                                        )
                                        .with_bounds(actions.remove)
                                        .with_state(AccessState {
                                            focusable: true,
                                            ..Default::default()
                                        })
                                        .with_action_target(format!("photos:library-remove:{id}")),
                                );
                                children.push(
                                    AccessNode::new(Role::Button)
                                        .with_name(format!("Edit details for {name}"))
                                        .with_description(
                                            "Edit shared folder, people, tags, and capture date",
                                        )
                                        .with_bounds(actions.details)
                                        .with_state(AccessState {
                                            focusable: true,
                                            ..Default::default()
                                        })
                                        .with_action_target(format!("photos:library-details:{id}")),
                                );
                            }
                            Some(
                                AccessNode::new(Role::ListItem)
                                    .with_name(name.clone())
                                    .with_bounds(*bounds)
                                    .with_hashed_key(format!("photos:library-item:{id}"))
                                    .with_children(children),
                            )
                        }),
                );
                cards.extend(self.album_cards.iter().map(|(id, bounds)| {
                    let label = id
                        .as_ref()
                        .and_then(|id| {
                            self.library_albums
                                .iter()
                                .find(|(album_id, _, _)| album_id == id)
                        })
                        .map_or_else(
                            || "All photos".to_owned(),
                            |(_, name, count)| format!("{name} ({count})"),
                        );
                    let target = format!("photos:library-album:{}", id.as_deref().unwrap_or("all"));
                    AccessNode::new(Role::ToggleButton)
                        .with_name(label)
                        .with_bounds(*bounds)
                        .with_state(AccessState {
                            focusable: true,
                            checked: Some(id.as_ref() == self.active_album.as_ref()),
                            ..Default::default()
                        })
                        .with_action_target(target)
                }));
                cards.extend(self.metadata_filter_cards.iter().map(|card| {
                    let label = if card.favorites {
                        "Favorites".to_owned()
                    } else {
                        match (&card.folder, &card.person, &card.tag) {
                            (Some(folder), None, None) => format!("Folder: {folder}"),
                            (None, Some(person), None) => format!("People: {person}"),
                            (None, None, Some(tag)) => format!("Tag: {tag}"),
                            _ => "All metadata".to_owned(),
                        }
                    };
                    AccessNode::new(Role::ToggleButton)
                        .with_name(label)
                        .with_bounds(card.bounds)
                        .with_state(AccessState {
                            focusable: true,
                            checked: Some(
                                card.folder.as_ref() == self.active_folder.as_ref()
                                    && card.person.as_ref() == self.active_person.as_ref()
                                    && card.tag.as_ref() == self.active_tag.as_ref()
                                    && card.favorites == self.active_favorites
                                    && self.active_album.is_none(),
                            ),
                            ..Default::default()
                        })
                        .with_action_target(if card.favorites {
                            "photos:library-favorites".to_owned()
                        } else {
                            format!(
                                "photos:library-metadata:{}",
                                metadata_filter_id(
                                    card.folder.as_deref(),
                                    card.person.as_deref(),
                                    card.tag.as_deref()
                                )
                            )
                        })
                }));
                cards.extend(
                    self.collection_page_hits
                        .iter()
                        .map(|(hit, bounds, enabled)| {
                            collection_page_access_node(*hit, *bounds, *enabled)
                        }),
                );
                for (hit, bounds) in &self.timeline_density_hits {
                    let (name, target) = match hit {
                        TimelineHit::ZoomOut => {
                            ("Show fewer photographs", "photos:timeline:density-out")
                        }
                        TimelineHit::ZoomIn => {
                            ("Show more photographs", "photos:timeline:density-in")
                        }
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_description(
                                "Changes how many photographs appear in each timeline row",
                            )
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
                for (forward, enabled, bounds) in &self.library_page_hits {
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(if *forward {
                                "Show next photograph"
                            } else {
                                "Show previous photograph"
                            })
                            .with_description(self.library_page_label())
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: *enabled,
                                disabled: !*enabled,
                                ..Default::default()
                            })
                            .with_action_target(if *forward {
                                "photos:library-page:next"
                            } else {
                                "photos:library-page:previous"
                            }),
                    );
                }
                if self.selection_album_chooser {
                    // A modal needs one focus scope. Leaving the dimmed grid,
                    // filters, or card operations in the AT tree would let a
                    // screen-reader user activate controls a pointer user cannot.
                    cards.clear();
                }
                for (hit, bounds) in &self.selection_hits {
                    let (name, target, disabled) = match hit {
                        SelectionHit::Start => {
                            ("Select photographs", "photos:selection:start", false)
                        }
                        SelectionHit::SelectAll => (
                            "Select all visible photographs",
                            "photos:selection:all",
                            false,
                        ),
                        SelectionHit::Clear => (
                            "Clear photograph selection",
                            "photos:selection:clear",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::Favorite => (
                            if self.selection_all_favorite {
                                "Remove selected photographs from Favorites"
                            } else {
                                "Add selected photographs to Favorites"
                            },
                            "photos:selection:favorite",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::EditDetails => (
                            "Edit details for selected photographs",
                            "photos:selection:edit-details",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::ChooseAlbum => (
                            "Choose destination album",
                            "photos:selection:choose-album",
                            self.selected_photo_ids.is_empty() || self.library_albums.is_empty(),
                        ),
                        SelectionHit::CancelDestinationChooser => (
                            "Cancel destination choice",
                            "photos:selection:destination-cancel",
                            false,
                        ),
                        SelectionHit::CloseDestinationChooser => (
                            "Close destination chooser",
                            "photos:selection:destination-close",
                            false,
                        ),
                        SelectionHit::ApplyDestination => (
                            "Use selected destination album",
                            "photos:selection:destination-apply",
                            self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::PreviousDestinationPage => (
                            "Show earlier destination albums",
                            "photos:selection:destination-previous",
                            self.selection_album_page == 0,
                        ),
                        SelectionHit::NextDestinationPage => (
                            "Show more destination albums",
                            "photos:selection:destination-next",
                            (self.selection_album_page + 1) * SELECTION_ALBUMS_PER_PAGE
                                >= self.library_albums.len(),
                        ),
                        SelectionHit::AddToAlbum => (
                            "Add selected photographs to selected album",
                            "photos:selection:add-to-album",
                            self.selected_photo_ids.is_empty()
                                || self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::Done => (
                            "Finish selecting photographs",
                            "photos:selection:done",
                            false,
                        ),
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: !disabled,
                                disabled,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
                for (album_id, bounds) in &self.selection_destination_hits {
                    let name = self
                        .library_albums
                        .iter()
                        .find(|(id, _, _)| id == album_id)
                        .map_or_else(
                            || "Choose destination album".to_owned(),
                            |(_, name, _)| format!("Use {name} as destination album"),
                        );
                    cards.push(
                        AccessNode::new(Role::ToggleButton)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                checked: Some(
                                    self.selection_destination_album.as_deref()
                                        == Some(album_id.as_str()),
                                ),
                                ..Default::default()
                            })
                            .with_action_target(format!("photos:selection:destination:{album_id}")),
                    );
                }
                if self.metadata_editor {
                    // The inspector owns the interaction scope. Keeping the
                    // dimmed grid's cards in the a11y tree would let a screen
                    // reader activate controls that pointer and keyboard users
                    // correctly cannot reach while editing details.
                    cards.clear();
                    let fields: &[(&str, &str, &TextInput)] = match self.metadata_editor_section {
                        MetadataEditorSection::Organize => &[
                            ("Folder", "folder", &self.metadata_folder_input),
                            ("People", "people", &self.metadata_people_input),
                            ("Tags", "tags", &self.metadata_tags_input),
                        ],
                        MetadataEditorSection::TimeAndPlace => &[
                            ("Captured", "captured", &self.metadata_captured_at_input),
                            ("Place name", "place", &self.metadata_place_input),
                            ("Latitude", "latitude", &self.metadata_latitude_input),
                            ("Longitude", "longitude", &self.metadata_longitude_input),
                        ],
                    };
                    for (name, target, input) in fields {
                        cards.push(
                            AccessNode::new(Role::TextInput)
                                .with_name(*name)
                                .with_value(input.value())
                                .with_bounds(input.get_rect())
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: input.is_focused(),
                                    ..Default::default()
                                })
                                .with_hashed_key(format!("photos:metadata:{name}"))
                                .with_action_target(format!("photos:metadata:field:{target}")),
                        );
                    }
                    for (hit, bounds) in &self.metadata_editor_hits {
                        let (name, target) = match hit {
                            MetadataEditorHit::Save => {
                                ("Save photo details", "photos:metadata:save")
                            }
                            MetadataEditorHit::Cancel => {
                                ("Cancel photo details", "photos:metadata:cancel")
                            }
                            MetadataEditorHit::Organize => {
                                ("Organize fields", "photos:metadata:organize")
                            }
                            MetadataEditorHit::TimeAndPlace => {
                                ("Time and place fields", "photos:metadata:time-and-place")
                            }
                            MetadataEditorHit::ClearFolder => {
                                ("Clear folder", "photos:metadata:clear-folder")
                            }
                            MetadataEditorHit::ClearPeople => {
                                ("Clear people", "photos:metadata:clear-people")
                            }
                            MetadataEditorHit::ClearTags => {
                                ("Clear tags", "photos:metadata:clear-tags")
                            }
                            MetadataEditorHit::ClearCapturedAt => {
                                ("Clear capture time", "photos:metadata:clear-captured-at")
                            }
                            MetadataEditorHit::ClearLocation => {
                                ("Clear location", "photos:metadata:clear-location")
                            }
                            MetadataEditorHit::PlaceOnMap => (
                                "Set location from offline map centre",
                                "photos:metadata:place-on-map",
                            ),
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(name)
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(target),
                        );
                    }
                    for (hit, button) in &self.metadata_editor_buttons {
                        let bounds = button.get_rect();
                        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                            continue;
                        }
                        let (name, target) = match hit {
                            MetadataEditorHit::Save => {
                                ("Save photo details", "photos:metadata:save")
                            }
                            MetadataEditorHit::Cancel => {
                                ("Cancel photo details", "photos:metadata:cancel")
                            }
                            MetadataEditorHit::Organize => {
                                ("Organize fields", "photos:metadata:organize")
                            }
                            MetadataEditorHit::TimeAndPlace => {
                                ("Time and place fields", "photos:metadata:time-and-place")
                            }
                            MetadataEditorHit::ClearFolder => {
                                ("Clear folder", "photos:metadata:clear-folder")
                            }
                            MetadataEditorHit::ClearPeople => {
                                ("Clear people", "photos:metadata:clear-people")
                            }
                            MetadataEditorHit::ClearTags => {
                                ("Clear tags", "photos:metadata:clear-tags")
                            }
                            MetadataEditorHit::ClearCapturedAt => {
                                ("Clear capture time", "photos:metadata:clear-captured-at")
                            }
                            MetadataEditorHit::ClearLocation => {
                                ("Clear location", "photos:metadata:clear-location")
                            }
                            MetadataEditorHit::PlaceOnMap => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(name)
                                .with_bounds(bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(target),
                        );
                    }
                } else if self.album_editor {
                    cards.push(
                        AccessNode::new(Role::TextInput)
                            .with_name("Album name")
                            .with_value(self.album_name_input.value())
                            .with_bounds(self.album_name_input.get_rect())
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.album_name_input.is_focused(),
                                ..Default::default()
                            })
                            .with_hashed_key("photos:album-editor:name"),
                    );
                    for (hit, button) in &self.album_page_buttons {
                        let (name, target) = match hit {
                            AlbumEditorHit::PreviousPage => (
                                "Show previous photographs",
                                "photos:album-editor:previous-page",
                            ),
                            AlbumEditorHit::NextPage => {
                                ("Show next photographs", "photos:album-editor:next-page")
                            }
                            _ => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(name)
                                .with_description(self.library_page_label())
                                .with_bounds(button.get_rect())
                                .with_state(AccessState {
                                    focusable: button.is_enabled(),
                                    focused: button.is_focused(),
                                    disabled: !button.is_enabled(),
                                    ..Default::default()
                                })
                                .with_action_target(target),
                        );
                    }
                    for (hit, bounds) in &self.album_editor_hits {
                        let (name, target) = match hit {
                            AlbumEditorHit::Save => ("Save album", "photos:album-editor:save"),
                            AlbumEditorHit::Cancel => {
                                ("Cancel album editing", "photos:album-editor:cancel")
                            }
                            AlbumEditorHit::Delete => {
                                ("Delete album", "photos:album-editor:delete")
                            }
                            AlbumEditorHit::ConfirmDelete => (
                                "Confirm deleting album",
                                "photos:album-editor:confirm-delete",
                            ),
                            AlbumEditorHit::New
                            | AlbumEditorHit::Edit
                            | AlbumEditorHit::PreviousPage
                            | AlbumEditorHit::NextPage => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(name)
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(target),
                        );
                    }
                } else {
                    for (hit, bounds) in &self.album_editor_hits {
                        let (name, target) = match hit {
                            AlbumEditorHit::New => ("New album", "photos:album-editor:new"),
                            AlbumEditorHit::Edit => {
                                ("Edit selected album", "photos:album-editor:edit")
                            }
                            _ => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(name)
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(target),
                        );
                    }
                }
                if self.selection_album_chooser {
                    AccessNode::new(Role::Dialog)
                        .with_name("Choose destination album")
                        .with_description("Choose one album, then use it as the destination")
                        .with_bounds(self.canvas_rect)
                        .with_state(AccessState::new().modal(true))
                        .with_hashed_key("photos:destination-album-dialog")
                        .with_children(cards)
                } else {
                    if !self.album_editor && !self.metadata_editor {
                        cards.push(self.timeline_index.accessibility(
                            self.timeline_index_metrics,
                            &self.timeline_index_markers,
                        ));
                    }
                    AccessNode::new(Role::List)
                        .with_name(tr("photos-library"))
                        .with_description(format!("{} photographs", self.library_total))
                        .with_bounds(self.canvas_rect)
                        .with_hashed_key("photos:library-grid")
                        .with_children(cards)
                }
            }
        } else {
            AccessNode::new(Role::Image)
                .with_name(image_name)
                .with_description(image_description)
                .with_bounds(self.canvas_rect)
                .with_hashed_key("photos:canvas")
        };
        let mut children = vec![self.toolbar_semantics(), canvas];
        if !self.status.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.status.clone())
                    .with_live(Politeness::Polite)
                    .with_bounds(self.status_rect)
                    .with_hashed_key("photos:status"),
            );
        }
        if let Some(job) = self.directory_import.as_ref() {
            let mut status_children = vec![
                AccessNode::new(Role::StaticText)
                    .with_name(if job.mode == DirectoryImportMode::Reindex {
                        "Reindexing photo folder"
                    } else {
                        "Importing photo folder"
                    })
                    .with_description(format!(
                        "{} found, {} imported, {} refreshed, {} issues",
                        job.discovered,
                        job.imported,
                        job.refreshed,
                        job.failures.len()
                    ))
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:directory-import-progress"),
                AccessNode::new(Role::Button)
                    .with_name("Stop")
                    .with_action_target("photos:directory-import:stop")
                    .with_hashed_key("photos:directory-import-stop"),
            ];
            if !job.failures.is_empty() {
                status_children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Details")
                        .with_description(format!("Inspect {} import issues", job.failures.len()))
                        .with_action_target("photos:directory-import:details")
                        .with_hashed_key("photos:directory-import-details"),
                );
            }
            children.push(
                AccessNode::new(Role::Group)
                    .with_name("Photo folder progress")
                    .with_state(AccessState {
                        busy: true,
                        ..Default::default()
                    })
                    .with_children(status_children)
                    .with_hashed_key("photos:directory-import-status"),
            );
        } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
            let mut status_children = vec![
                AccessNode::new(Role::StaticText)
                    .with_name(if receipt.mode == DirectoryImportMode::Reindex {
                        "Photo folder reindex result"
                    } else {
                        "Photo folder import result"
                    })
                    .with_description(format!(
                        "{} imported, {} refreshed, {} unchanged, {} issues",
                        receipt.imported,
                        receipt.refreshed,
                        receipt.unchanged,
                        receipt.failure_count,
                    ))
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:directory-import-result"),
            ];
            if receipt.failure_count > 0 {
                status_children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Details")
                        .with_description(format!(
                            "Inspect {} retained issue details",
                            receipt.failures.len()
                        ))
                        .with_action_target("photos:directory-import:details")
                        .with_hashed_key("photos:directory-import-details"),
                );
            }
            status_children.push(
                AccessNode::new(Role::Button)
                    .with_name("Dismiss")
                    .with_action_target("photos:directory-import:dismiss")
                    .with_hashed_key("photos:directory-import-dismiss"),
            );
            children.push(
                AccessNode::new(Role::Group)
                    .with_name("Photo folder result")
                    .with_children(status_children)
                    .with_hashed_key("photos:directory-import-status"),
            );
        }
        if self.directory_import_details_open {
            let (failures, total) = if let Some(job) = self.directory_import.as_ref() {
                (&job.failures[..], job.failures.len())
            } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
                (&receipt.failures[..], receipt.failure_count)
            } else {
                (&[][..], 0)
            };
            let retained = failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT);
            let pages = retained.div_ceil(DIRECTORY_FAILURES_PER_PAGE).max(1);
            let page = self.directory_import_details_page.min(pages - 1);
            let mut detail_children = failures
                .iter()
                .take(retained)
                .skip(page * DIRECTORY_FAILURES_PER_PAGE)
                .take(DIRECTORY_FAILURES_PER_PAGE)
                .enumerate()
                .map(|(index, failure)| {
                    let name = failure
                        .source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Unavailable item");
                    AccessNode::new(Role::ListItem)
                        .with_name(name)
                        .with_description(format!(
                            "{}. {}",
                            failure.message,
                            if failure.retryable {
                                "This item can be retried"
                            } else {
                                "This item needs a change before retrying"
                            }
                        ))
                        .with_hashed_key(format!("photos:directory-failure:{page}:{index}"))
                })
                .collect::<Vec<_>>();
            detail_children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(format!(
                        "Showing {} retained details out of {total} issues, page {} of {pages}",
                        retained,
                        page + 1
                    ))
                    .with_hashed_key("photos:directory-failure-summary"),
            );
            for (key, label, disabled) in [
                ("details-previous", "Previous", page == 0),
                ("details-next", "Next", page + 1 >= pages),
                ("details-close", "Close", false),
            ] {
                detail_children.push(
                    AccessNode::new(Role::Button)
                        .with_name(label)
                        .with_state(AccessState {
                            disabled,
                            focusable: !disabled,
                            ..Default::default()
                        })
                        .with_action_target(format!("photos:directory-import:{key}"))
                        .with_hashed_key(format!("photos:directory-import:{key}")),
                );
            }
            children.push(
                AccessNode::new(Role::Dialog)
                    .with_name("Folder import issues")
                    .with_description(format!(
                        "{retained} issue details retained; {} additional issues",
                        total.saturating_sub(retained)
                    ))
                    .with_state(AccessState {
                        modal: true,
                        ..Default::default()
                    })
                    .with_children(detail_children)
                    .with_hashed_key("photos:directory-import-dialog"),
            );
        }
        AccessNode::new(Role::Group)
            .with_name(tr("photos-app"))
            .with_bounds(self.rect)
            .with_semantic_key("photos")
            .with_children(children)
    }

    /// Dispatch a toolbar action requested by assistive technology through the
    /// identical `drain_pending` path used by pointer and keyboard activation.
    fn perform_toolbar_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        doc: &mut AutoCommit,
    ) -> bool {
        let Some(key) = target.strip_prefix("photos:toolbar:") else {
            return false;
        };
        let Some((index, (hit, _, _, _))) = TOOLBAR
            .iter()
            .enumerate()
            .find(|(_, (_, toolbar_key, _, _))| *toolbar_key == key)
        else {
            return false;
        };
        match action {
            AccessAction::Focus => {
                for (_, button) in &mut self.buttons {
                    button.set_focus(false);
                }
                if let Some((_, button)) = self.buttons.get_mut(index) {
                    button.set_focus(true);
                }
                true
            }
            AccessAction::Activate
                if self.buttons.get(index).is_some_and(|(_, b)| b.is_enabled()) =>
            {
                self.pending.set(Some(*hit));
                self.drain_pending(doc);
                true
            }
            _ => false,
        }
    }

    fn perform_directory_semantic_action(&mut self, target: &str, action: &AccessAction) -> bool {
        let Some(command) = target.strip_prefix("photos:directory-import:") else {
            return false;
        };
        if matches!(action, AccessAction::Focus) {
            let status_hit = match command {
                "stop" => Some(DirectoryStatusHit::Stop),
                "details" => Some(DirectoryStatusHit::Details),
                "dismiss" => Some(DirectoryStatusHit::Dismiss),
                _ => None,
            };
            if let Some(hit) = status_hit {
                let available = match hit {
                    DirectoryStatusHit::Stop => self.directory_import.is_some(),
                    DirectoryStatusHit::Details => {
                        self.directory_import
                            .as_ref()
                            .is_some_and(|job| !job.failures.is_empty())
                            || self
                                .directory_import_receipt
                                .as_ref()
                                .is_some_and(|receipt| receipt.failure_count > 0)
                    }
                    DirectoryStatusHit::Dismiss => {
                        self.directory_import.is_none() && self.directory_import_receipt.is_some()
                    }
                };
                if available {
                    for (candidate, button) in &mut self.directory_import_buttons {
                        button.set_focus(*candidate == hit);
                    }
                }
                return available;
            }
            if !self.directory_import_details_open {
                return false;
            }
            let retained = self.directory_import.as_ref().map_or_else(
                || {
                    self.directory_import_receipt
                        .as_ref()
                        .map_or(0, |receipt| receipt.failures.len())
                },
                |job| job.failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT),
            );
            let pages = retained.div_ceil(DIRECTORY_FAILURES_PER_PAGE).max(1);
            return match command {
                "details-previous" => self.directory_import_details_page > 0,
                "details-next" => self.directory_import_details_page + 1 < pages,
                "details-close" => true,
                _ => false,
            };
        }
        if !matches!(action, AccessAction::Activate) {
            return false;
        }
        match command {
            "stop" => self.activate_directory_status(DirectoryStatusHit::Stop),
            "details" => self.activate_directory_status(DirectoryStatusHit::Details),
            "dismiss" => self.activate_directory_status(DirectoryStatusHit::Dismiss),
            "details-previous" => self.page_directory_details(false),
            "details-next" => self.page_directory_details(true),
            "details-close" => self.close_directory_details(),
            _ => false,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the library semantics mirror every visible card, editor, and metadata filter target"
    )]
    fn perform_library_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        doc: &mut AutoCommit,
    ) -> bool {
        if let Some(destination) = target.strip_prefix("photos:library:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                match destination {
                    "timeline" => {
                        self.people_mode = false;
                        self.library_destination = LibraryDestination::Timeline;
                    }
                    "albums" => self.show_albums(),
                    "people" => self.show_people(),
                    "places" => {
                        self.people_mode = false;
                        self.library_destination = LibraryDestination::Places;
                    }
                    "analysis" => self.show_library_analysis(),
                    _ => return false,
                }
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:places:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            if let Some(photo_id) = command.strip_prefix("photo:") {
                if PhotosDoc::photo(doc, photo_id).is_none() {
                    return false;
                }
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::SelectPhoto {
                        photo_id: photo_id.to_owned(),
                    },
                );
                self.library_mode = false;
                return true;
            }
            match command {
                "toggle-list" => self.places_list_mode = !self.places_list_mode,
                "zoom-in" => self.zoom_places_by(1.0),
                "zoom-out" => self.zoom_places_by(-1.0),
                "pan-left" => self.set_places_viewport(self.places_viewport.panned(
                    44.0,
                    0.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-right" => self.set_places_viewport(self.places_viewport.panned(
                    -44.0,
                    0.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-up" => self.set_places_viewport(self.places_viewport.panned(
                    0.0,
                    44.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-down" => self.set_places_viewport(self.places_viewport.panned(
                    0.0,
                    -44.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                _ => {
                    let Some(key) = command.strip_prefix("open:") else {
                        return false;
                    };
                    return self.open_place_collection(doc, key).is_ok();
                }
            }
            return true;
        }
        if let Some(album_id) = target.strip_prefix("photos:album-edit:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                if PhotosDoc::album(doc, album_id).is_none() {
                    return false;
                }
                self.active_album = Some(album_id.to_owned());
                self.begin_edit_album(doc);
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:collection-page:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let hit = match command {
                "albums-previous" => CollectionPageHit::AlbumsPrevious,
                "albums-next" => CollectionPageHit::AlbumsNext,
                "people-previous" => CollectionPageHit::PeoplePrevious,
                "people-next" => CollectionPageHit::PeopleNext,
                "places-previous" => CollectionPageHit::PlacesPrevious,
                "places-next" => CollectionPageHit::PlacesNext,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) {
                self.page_collection(hit);
            }
            return true;
        }
        if target == "photos:people:name" {
            return match action {
                AccessAction::Focus => {
                    self.people_name_input.set_focus(true);
                    true
                }
                AccessAction::SetValue(value) => {
                    self.people_name_input.set_text(value);
                    true
                }
                _ => false,
            };
        }
        if let Some(index) = target
            .strip_prefix("photos:people:control:")
            .and_then(|value| value.parse::<usize>().ok())
        {
            let Some(control) = self.people_controls.get(index) else {
                return false;
            };
            return match action {
                AccessAction::Focus => {
                    for item in &mut self.people_controls {
                        item.button.set_focus(false);
                    }
                    if let Some(control) = self.people_controls.get_mut(index) {
                        control.button.set_focus(true);
                    }
                    true
                }
                AccessAction::Activate if control.button.is_enabled() => {
                    let intent = control.intent.clone();
                    self.handle_people_intent(intent, doc);
                    true
                }
                _ => false,
            };
        }
        if target.starts_with("photos:timeline-index:") {
            if matches!(action, AccessAction::Focus) {
                self.timeline_index.set_focus(true);
                return true;
            }
            if let Some(intent) = self.timeline_index.perform_access_action(
                target,
                action,
                self.timeline_index_metrics,
                &self.timeline_index_markers,
            ) {
                return self.apply_timeline_index_intent(&intent);
            }
            return false;
        }
        if let Some(direction) = target.strip_prefix("photos:timeline:density-") {
            return match (direction, action) {
                ("in", AccessAction::Activate) => self.adjust_timeline_density(true),
                ("out", AccessAction::Activate) => self.adjust_timeline_density(false),
                ("in" | "out", AccessAction::Focus) => true,
                _ => false,
            };
        }
        if let Some(command) = target.strip_prefix("photos:analysis:") {
            if let Some(photo_id) = command.strip_prefix("failure:") {
                if matches!(action, AccessAction::Focus) {
                    return self
                        .library_analysis
                        .failures()
                        .any(|failure| failure.photo_id == photo_id);
                }
                if !matches!(action, AccessAction::Activate)
                    || !self
                        .library_analysis
                        .failures()
                        .any(|failure| failure.photo_id == photo_id)
                {
                    return false;
                }
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::SelectPhoto {
                        photo_id: photo_id.to_owned(),
                    },
                );
                self.library_mode = false;
                return true;
            }
            let hit = match command {
                "start" => LibraryAnalysisHit::Start,
                "pause" => LibraryAnalysisHit::Pause,
                "resume" => LibraryAnalysisHit::Resume,
                "retry" => LibraryAnalysisHit::RetryFailures,
                _ => return false,
            };
            if matches!(action, AccessAction::Focus) {
                for (candidate, button) in &mut self.library_analysis_buttons {
                    button.set_focus(*candidate == hit);
                }
                return true;
            }
            return matches!(action, AccessAction::Activate)
                && self.activate_library_analysis(hit, doc);
        }
        if let Some(command) = target.strip_prefix("photos:selection:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                if let Some(album_id) = command.strip_prefix("destination:") {
                    if PhotosDoc::album(doc, album_id).is_none() {
                        self.notice =
                            Some("That destination album is no longer available".to_owned());
                        return false;
                    }
                    self.selection_destination_album = Some(album_id.to_owned());
                    self.notice =
                        Some("Destination album selected; choose Use album to continue".to_owned());
                    return true;
                }
                let hit = match command {
                    "start" => SelectionHit::Start,
                    "all" => SelectionHit::SelectAll,
                    "clear" => SelectionHit::Clear,
                    "favorite" => SelectionHit::Favorite,
                    "edit-details" => SelectionHit::EditDetails,
                    "choose-album" => SelectionHit::ChooseAlbum,
                    "destination-cancel" => SelectionHit::CancelDestinationChooser,
                    "destination-close" => SelectionHit::CloseDestinationChooser,
                    "destination-apply" => SelectionHit::ApplyDestination,
                    "destination-previous" => SelectionHit::PreviousDestinationPage,
                    "destination-next" => SelectionHit::NextDestinationPage,
                    "add-to-album" => SelectionHit::AddToAlbum,
                    "done" => SelectionHit::Done,
                    _ => return false,
                };
                self.handle_selection_hit(hit, doc);
            }
            return true;
        }
        if let Some(direction) = target.strip_prefix("photos:library-page:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let forward = match direction {
                "next" => true,
                "previous" => false,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) && self.can_page_library(forward) {
                self.page_library(forward);
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:album-editor:") {
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "new" => self.begin_new_album(doc),
                "edit" => self.begin_edit_album(doc),
                "previous-page" if self.can_page_library(false) => self.page_library(false),
                "next-page" if self.can_page_library(true) => self.page_library(true),
                "save" => self.save_album_editor(doc),
                "cancel" => self.cancel_album_editor(),
                "delete" => self.album_delete_confirm = true,
                "confirm-delete" => self.delete_active_album(doc),
                _ => return false,
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:metadata:") {
            if let Some(field) = command.strip_prefix("field:") {
                // Organize and Time & Place fields share the same focus slots.
                let focus = match field {
                    "folder" | "captured" => 0,
                    "people" | "place" => 1,
                    "tags" | "latitude" => 2,
                    "longitude" => 3,
                    _ => return false,
                };
                return match action {
                    AccessAction::Focus => {
                        self.set_metadata_editor_focus(focus);
                        true
                    }
                    AccessAction::SetValue(value) => {
                        match field {
                            "folder" => self.metadata_folder_input.set_text(value),
                            "people" => self.metadata_people_input.set_text(value),
                            "tags" => self.metadata_tags_input.set_text(value),
                            "captured" => self.metadata_captured_at_input.set_text(value),
                            "place" => self.metadata_place_input.set_text(value),
                            "latitude" => {
                                self.metadata_latitude_input.set_text(value);
                                self.metadata_location_from_map = false;
                            }
                            "longitude" => {
                                self.metadata_longitude_input.set_text(value);
                                self.metadata_location_from_map = false;
                            }
                            _ => return false,
                        }
                        true
                    }
                    _ => false,
                };
            }
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "save" => self.save_metadata_editor(doc),
                "cancel" => self.cancel_metadata_editor(),
                "organize" => self.set_metadata_editor_section(MetadataEditorSection::Organize),
                "time-and-place" => {
                    self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
                }
                "clear-folder" => self.clear_metadata_dimension(0),
                "clear-people" => self.clear_metadata_dimension(1),
                "clear-tags" => self.clear_metadata_dimension(2),
                "clear-captured-at" => self.clear_metadata_dimension(3),
                "clear-location" => self.clear_metadata_dimension(4),
                "place-on-map" => {
                    let bounds = self.metadata_location_map_bounds;
                    let _ = self.set_metadata_location_from_map_point(bounds.center());
                }
                _ => return false,
            }
            return true;
        }
        if let Some(album_id) = target.strip_prefix("photos:library-album:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            self.active_album = (album_id != "all").then(|| album_id.to_owned());
            self.library_destination = LibraryDestination::Timeline;
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.active_favorites = false;
            self.library_offset = 0;
            self.timeline_scroll_y = 0.0;
            return true;
        }
        if let Some(id) = target.strip_prefix("photos:library-details:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                self.begin_metadata_editor(doc, id);
            }
            return true;
        }
        if target == "photos:library-favorites" {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                let _ = self.view_favorites(doc);
            }
            return true;
        }
        if let Some(filter_id) = target.strip_prefix("photos:library-metadata:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            let Some(card) = self.metadata_filter_cards.iter().find(|card| {
                metadata_filter_id(
                    card.folder.as_deref(),
                    card.person.as_deref(),
                    card.tag.as_deref(),
                ) == filter_id
            }) else {
                return false;
            };
            self.active_album = None;
            self.active_folder.clone_from(&card.folder);
            self.active_person.clone_from(&card.person);
            self.active_tag.clone_from(&card.tag);
            self.active_favorites = card.favorites;
            self.library_offset = 0;
            return true;
        }
        let (photo_id, set_favorite, remove) =
            if let Some(id) = target.strip_prefix("photos:library-photo:") {
                (id, false, false)
            } else if let Some(id) = target.strip_prefix("photos:library-favorite:") {
                (id, true, false)
            } else if let Some(id) = target.strip_prefix("photos:library-remove:") {
                (id, false, true)
            } else {
                return false;
            };
        if self.selection_album_chooser {
            return false;
        }
        if !matches!(action, AccessAction::Activate) {
            if matches!(action, AccessAction::Focus) && self.selection_mode {
                self.selection_anchor = Some(photo_id.to_owned());
            }
            return matches!(action, AccessAction::Focus);
        }
        let Some(item) = self.library_items.iter().find(|item| item.id == photo_id) else {
            return false;
        };
        let favorite = item.favorite;
        if self.selection_mode {
            self.toggle_library_selection(photo_id, false);
        } else if remove {
            if self.library_remove_confirm.as_deref() == Some(photo_id) {
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::RemovePhoto {
                        photo_id: photo_id.to_owned(),
                    },
                );
                self.library_remove_confirm = None;
                self.notice = Some("Photograph removed from the shared library".to_owned());
            } else {
                self.library_remove_confirm = Some(photo_id.to_owned());
                self.notice = Some(
                    "Activate Remove again to confirm; it will also leave every album".to_owned(),
                );
            }
        } else if set_favorite {
            self.commit_library_action(
                doc,
                &crate::actions::PhotosAction::SetFavorite {
                    photo_id: photo_id.to_owned(),
                    favorite: !favorite,
                },
            );
        } else if self.album_editor {
            if !self.album_editor_members.insert(photo_id.to_owned()) {
                self.album_editor_members.remove(photo_id);
            }
        } else {
            self.commit_library_action(
                doc,
                &crate::actions::PhotosAction::SelectPhoto {
                    photo_id: photo_id.to_owned(),
                },
            );
            self.library_mode = false;
        }
        true
    }

    fn begin_directory_import(
        &mut self,
        path: std::path::PathBuf,
        invocation: Option<tilo_actions::InvocationId>,
        mode: DirectoryImportMode,
        retain_exif_location: bool,
    ) {
        if self.directory_import.is_some() {
            self.notice = Some("A photo folder job is already running".into());
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let content = self.content.clone();
        let spawn = ui::rt::spawn_thread("photos-directory-import", move || {
            let result = crate::import::index_directory(
                &path,
                &content,
                crate::import::ImportOptions {
                    retain_exif_location,
                    ..crate::import::ImportOptions::default()
                },
                &worker_cancel,
                |progress| {
                    let _ = sender.send(DirectoryImportEvent::Progress(progress));
                },
            );
            let _ = sender.send(DirectoryImportEvent::Finished(result));
        });
        if spawn.is_err() {
            self.notice = Some("Photos could not start the folder indexing job".into());
            if let Some(invocation) = invocation {
                self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.directory_worker_unavailable".into(),
                        message: "Photos could not start the folder indexing job".into(),
                        details: None,
                        retryable: true,
                    },
                });
            }
            return;
        }
        self.directory_import_receipt = None;
        self.directory_import_details_open = false;
        self.directory_import_details_page = 0;
        self.directory_import = Some(DirectoryImport {
            receiver,
            cancel,
            invocation,
            mode,
            discovered: 0,
            imported: 0,
            refreshed: 0,
            unchanged: 0,
            skipped: 0,
            sequence: 0,
            pending: VecDeque::new(),
            failures: Vec::new(),
            worker_done: false,
            cancelled: false,
            limit_reached: false,
            produced_resources: Vec::new(),
            _shot_sender: None,
        });
        self.notice = Some(match mode {
            DirectoryImportMode::Import => "Preparing folder import…".into(),
            DirectoryImportMode::Reindex => "Preparing folder reindex…".into(),
        });
    }

    fn stop_directory_import(&mut self) -> bool {
        let Some(job) = self.directory_import.as_mut() else {
            return false;
        };
        job.cancel.store(true, Ordering::Release);
        job.cancelled = true;
        job.pending.clear();
        if let Some(invocation) = job.invocation.take() {
            self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(
                    "Folder indexing was stopped; completed photographs were kept".into(),
                ),
            });
        }
        self.resource_invocation = None;
        self.notice = Some("Stopping folder indexing… completed photographs are kept".into());
        true
    }

    fn activate_directory_status(&mut self, hit: DirectoryStatusHit) -> bool {
        match hit {
            DirectoryStatusHit::Stop => self.stop_directory_import(),
            DirectoryStatusHit::Details => {
                let has_failures = self
                    .directory_import
                    .as_ref()
                    .is_some_and(|job| !job.failures.is_empty())
                    || self
                        .directory_import_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.failure_count > 0);
                if has_failures {
                    self.directory_import_details_open = true;
                    self.directory_import_details_page = 0;
                }
                has_failures
            }
            DirectoryStatusHit::Dismiss => {
                if self.directory_import.is_some() || self.directory_import_receipt.is_none() {
                    return false;
                }
                self.directory_import_receipt = None;
                self.directory_import_details_open = false;
                self.directory_import_details_layout = None;
                true
            }
        }
    }

    fn page_directory_details(&mut self, forward: bool) -> bool {
        if !self.directory_import_details_open {
            return false;
        }
        let retained = self.directory_import.as_ref().map_or_else(
            || {
                self.directory_import_receipt
                    .as_ref()
                    .map_or(0, |receipt| receipt.failures.len())
            },
            |job| job.failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT),
        );
        let pages = retained.div_ceil(DIRECTORY_FAILURES_PER_PAGE).max(1);
        let next = if forward {
            (self.directory_import_details_page + 1).min(pages - 1)
        } else {
            self.directory_import_details_page.saturating_sub(1)
        };
        if next == self.directory_import_details_page {
            return false;
        }
        self.directory_import_details_page = next;
        true
    }

    fn close_directory_details(&mut self) -> bool {
        if !self.directory_import_details_open {
            return false;
        }
        self.directory_import_details_open = false;
        self.directory_import_details_layout = None;
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "applies every directory import event kind and the final summary from one worker drain"
    )]
    fn drain_directory_import(&mut self, doc: &mut AutoCommit) {
        if self.directory_import.is_none() {
            return;
        }
        loop {
            let event = self
                .directory_import
                .as_ref()
                .map(|job| job.receiver.try_recv());
            match event {
                None => return,
                Some(Ok(DirectoryImportEvent::Progress(progress))) => {
                    let Some(job) = self.directory_import.as_mut() else {
                        return;
                    };
                    job.discovered = progress.discovered;
                    job.skipped = progress.skipped;
                    job.sequence = job.sequence.saturating_add(1);
                    if let Some(invocation) = job.invocation {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Progress {
                            invocation,
                            sequence: job.sequence,
                            phase: "indexing".into(),
                            completed: Some(progress.indexed as u64),
                            total: None,
                            message: Some(format!(
                                "{} photographs discovered; {} could not be indexed",
                                progress.discovered, progress.skipped
                            )),
                        });
                    }
                }
                Some(Ok(DirectoryImportEvent::Finished(Ok(report)))) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        job.discovered = report.discovered;
                        job.skipped = report.failures.len();
                        let cancellation_was_requested = job.cancelled;
                        job.cancelled |= report.cancelled;
                        job.limit_reached = report.limit_reached;
                        job.failures = report.failures;
                        if !cancellation_was_requested {
                            job.pending = report.photos.into();
                        }
                        job.worker_done = true;
                    }
                    break;
                }
                Some(Ok(DirectoryImportEvent::Finished(Err(message)))) => {
                    let Some(mut job) = self.directory_import.take() else {
                        return;
                    };
                    self.notice = Some(message.clone());
                    if let Some(invocation) = job.invocation.take() {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_index_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                    return;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => break,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    let Some(mut job) = self.directory_import.take() else {
                        return;
                    };
                    let message = "The photo folder worker stopped before returning a result";
                    self.notice = Some(message.into());
                    if let Some(invocation) = job.invocation.take() {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_worker_stopped".into(),
                                message: message.into(),
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                    return;
                }
            }
        }

        for _ in 0..DIRECTORY_IMPORT_PER_FRAME {
            let indexed = self
                .directory_import
                .as_mut()
                .and_then(|job| job.pending.pop_front());
            let Some(indexed) = indexed else { break };
            let existing = PhotosDoc::photo(doc, &indexed.photo.id);
            let mode = self
                .directory_import
                .as_ref()
                .map_or(DirectoryImportMode::Import, |job| job.mode);
            let (action, changed) = match (mode, existing.as_ref()) {
                (DirectoryImportMode::Reindex, Some(current)) => {
                    let capture_changes =
                        current.captured_at.is_none() && indexed.photo.captured_at.is_some();
                    let location_changes = current.location.as_ref().is_none_or(|location| {
                        location.provenance.source == "exif"
                            && current.location != indexed.photo.location
                    });
                    (
                        crate::actions::PhotosAction::RefreshImportedMetadata {
                            photo_id: indexed.photo.id.clone(),
                            captured_at: indexed.photo.captured_at.clone(),
                            location: indexed.photo.location.clone(),
                        },
                        capture_changes || location_changes,
                    )
                }
                (_, Some(_)) => (
                    crate::actions::PhotosAction::ImportPhoto {
                        photo: indexed.photo.clone(),
                    },
                    false,
                ),
                (_, None) => (
                    crate::actions::PhotosAction::ImportPhoto {
                        photo: indexed.photo.clone(),
                    },
                    true,
                ),
            };
            if !changed {
                if let Some(job) = self.directory_import.as_mut() {
                    job.unchanged = job.unchanged.saturating_add(1);
                }
                continue;
            }
            match self.actions.try_dispatch_with_context(
                doc,
                crate::actions::PHOTOS_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.photos", "Photos", &action),
            ) {
                Ok(()) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        if existing.is_some() {
                            job.refreshed = job.refreshed.saturating_add(1);
                        } else {
                            job.imported = job.imported.saturating_add(1);
                        }
                        job.produced_resources.push(indexed.photo.id);
                    }
                }
                Err(error) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        job.skipped = job.skipped.saturating_add(1);
                        job.failures.push(crate::import::ImportFailure {
                            source: indexed.source,
                            message: error.to_string(),
                            retryable: false,
                        });
                    }
                }
            }
        }

        if let Some(mut job) = self
            .directory_import
            .take_if(|job| job.worker_done && job.pending.is_empty())
        {
            let noun = if job.mode == DirectoryImportMode::Import {
                "import"
            } else {
                "reindex"
            };
            let message = format!(
                "Folder {noun} finished: {} imported · {} refreshed · {} unchanged · {} issue{}",
                job.imported,
                job.refreshed,
                job.unchanged,
                job.failures.len(),
                if job.failures.len() == 1 { "" } else { "s" }
            );
            self.notice = Some(message);
            let failure_count = job.failures.len();
            self.directory_import_receipt = Some(DirectoryImportReceipt {
                mode: job.mode,
                discovered: job.discovered,
                imported: job.imported,
                refreshed: job.refreshed,
                unchanged: job.unchanged,
                failures: job
                    .failures
                    .iter()
                    .take(DIRECTORY_FAILURE_DETAIL_LIMIT)
                    .cloned()
                    .collect(),
                failure_count,
                cancelled: job.cancelled,
                limit_reached: job.limit_reached,
            });
            if let Some(invocation) = job.invocation.take() {
                let failures: Vec<_> = job
                    .failures
                    .iter()
                    .take(50)
                    .map(|failure| serde_json::json!({
                        "name": failure.source.file_name().and_then(|name| name.to_str()).unwrap_or("Unavailable item"),
                        "message": failure.message,
                        "retryable": failure.retryable,
                    }))
                    .collect();
                self.resource_result = Some(if job.cancelled {
                    tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(
                            "Folder indexing was cancelled; completed photographs were kept".into(),
                        ),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::json!({
                                "operation": noun,
                                "discovered": job.discovered,
                                "imported": job.imported,
                                "refreshed": job.refreshed,
                                "unchanged": job.unchanged,
                                "failed": job.failures.len(),
                                "limit_reached": job.limit_reached,
                                "failures": failures,
                            })),
                            resources: job.produced_resources,
                        },
                    }
                });
            }
            self.resource_invocation = None;
        } else if let Some(job) = self.directory_import.as_ref() {
            self.notice = Some(format!(
                "{} folder: {} found · {} imported · {} refreshed · {} issue{}",
                if job.mode == DirectoryImportMode::Import {
                    "Importing"
                } else {
                    "Reindexing"
                },
                job.discovered,
                job.imported,
                job.refreshed,
                job.failures.len(),
                if job.failures.len() == 1 { "" } else { "s" }
            ));
        }
    }

    /// Collect a chosen file from the portal, if one has arrived.
    #[expect(
        clippy::too_many_lines,
        reason = "drains every portal picker channel with its outcome handling side by side"
    )]
    fn drain_picker(&mut self, doc: &mut AutoCommit) {
        self.drain_contents_picker(doc);
        if let Some(rx) = self.directory_picker.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::PortalOutcome::Opened(path)) => {
                    self.directory_picker = None;
                    let invocation = self.resource_invocation.take();
                    self.begin_directory_import(
                        path,
                        invocation,
                        DirectoryImportMode::Import,
                        self.directory_picker_retain_exif_location,
                    );
                }
                Ok(ui::portal::PortalOutcome::Cancelled) => {
                    self.directory_picker = None;
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some("folder selection was cancelled".into()),
                        });
                    }
                }
                Ok(ui::portal::PortalOutcome::Failed(message)) => {
                    self.directory_picker = None;
                    self.notice = Some(format!("Folder selection failed: {message}"));
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_portal_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.directory_picker = None;
                }
            }
        }
        let Some(rx) = self.picker.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.picker = None;
                self.import_content = None;
                if self.picker_stale {
                    // The chooser already ran. Settle its real selection outcome
                    // without importing into content changed since dispatch.
                    let message = format!(
                        "Selected {}; import was invalidated by changed document content",
                        path.display()
                    );
                    self.notice = Some(message.clone());
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.stale_selection".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        });
                    }
                    return;
                }
                let path = path.to_string_lossy().into_owned();
                self.open_path(doc, &path);
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(if self.notice.is_none() {
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    } else {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.open_failed".into(),
                                message: tr("photos-open-failed"),
                                details: None,
                                retryable: false,
                            },
                        }
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Ok(ui::portal::PortalOutcome::Cancelled) => {
                self.picker = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("photo selection was cancelled".into()),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                self.notice = Some(format!("{}: {message}", tr("photos-open-failed")));
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "photos.portal_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.picker = None;
                self.notice = Some("The system file picker stopped unexpectedly".into());
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "photos.portal_disconnected".into(),
                            message: "the system file picker stopped unexpectedly".into(),
                            details: None,
                            retryable: true,
                        },
                    });
                }
            }
        }
    }

    /// Which toolbar buttons are usable right now. Everything but Open needs
    /// an image; Reset additionally needs something to undo.
    fn enabled(hit: Hit, has_image: bool, edited: bool) -> bool {
        match hit {
            Hit::Open | Hit::Library => true,
            Hit::Reset => has_image && edited,
            _ => has_image,
        }
    }

    /// The status line: the file, its edited size, and any notice.
    fn status_text(&self, doc: &AutoCommit) -> String {
        if self.library_mode {
            let mut s = match self.library_destination {
                LibraryDestination::Timeline => format!("{} photographs", self.library_total),
                LibraryDestination::Albums => {
                    format!("Albums  ·  {} collections", self.library_albums.len())
                }
                LibraryDestination::People => {
                    format!("People  ·  {} photographs", self.library_total)
                }
                LibraryDestination::Places => {
                    format!("Places  ·  {} photographs", self.library_total)
                }
                LibraryDestination::Analysis => {
                    let (searchable, ready, awaiting) = self.library_analysis_coverage;
                    format!(
                        "Photo analysis  ·  {searchable} searchable  ·  {ready} ready  ·  {awaiting} awaiting local bytes"
                    )
                }
            };
            for detail in self.notice.iter().chain(self.search_refresh_error.iter()) {
                s.push_str("  ·  ");
                s.push_str(detail);
            }
            return s;
        }
        let Some(open) = PhotosDoc::image(doc) else {
            return self
                .notice
                .iter()
                .chain(self.search_refresh_error.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("  ·  ");
        };
        let edits = PhotosDoc::edits(doc);
        let (w, h) = open.edited_size(&edits);
        let mut s = format!("{}  ·  {w} × {h}", open.name);
        if !edits.is_identity() {
            s.push_str("  ·  ");
            s.push_str(&tr("photos-edited"));
        }
        if let Some(n) = &self.notice {
            s.push_str("  ·  ");
            s.push_str(n);
        }
        if let Some(error) = &self.search_refresh_error {
            s.push_str("  ·  ");
            s.push_str(error);
        }
        s
    }

    /// Paint a quiet information card over the viewer rather than giving the
    /// photograph a competing permanent sidebar.  The card uses only the
    /// portable, explicitly shared library record, so a collaborator sees
    /// the same description, people, and place without this surface ever
    /// reading device-local EXIF or paths.
    #[expect(
        clippy::too_many_lines,
        reason = "the viewer details card keeps its responsive geometry and painted rows together"
    )]
    fn draw_viewer_details(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use vello::peniko::Fill;

        let Some(details) = self.viewer_details.as_ref() else {
            return;
        };
        // On short windows, preserving a usable, contain-fit photograph is
        // more valuable than placing a card over most of it. The full shared
        // detail is still present in the image accessibility description.
        if self.canvas_rect.width() < 340.0 || self.canvas_rect.height() < 300.0 {
            return;
        }
        let compact = self.canvas_rect.width() < 720.0 || theme.text_scale > 1.25;
        let horizontal_inset = if compact { 14.0 } else { 20.0 };
        let card_width = if compact {
            self.canvas_rect.width() - horizontal_inset * 2.0
        } else {
            self.canvas_rect.width().min(460.0)
        };
        let has_caption = details.description.is_some();
        let card_height = if compact {
            if has_caption { 138.0 } else { 112.0 }
        } else if has_caption {
            148.0
        } else {
            120.0
        };
        let card = Rect::new(
            self.canvas_rect.x0 + horizontal_inset,
            self.canvas_rect.y1 - card_height - horizontal_inset,
            self.canvas_rect.x0 + horizontal_inset + card_width,
            self.canvas_rect.y1 - horizontal_inset,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.window_card.with_alpha(0.94),
            None,
            &RoundedRect::from_rect(card, theme.radius_standard),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(card, theme.radius_standard),
        );

        let icon = Rect::new(
            card.x0 + 14.0,
            card.y0 + 14.0,
            card.x0 + 34.0,
            card.y0 + 34.0,
        );
        draw_icon(
            scene,
            transform,
            icon,
            Icon::Image,
            theme.accent_primary,
            1.4,
        );
        let title_right = if details.favorite {
            card.x1 - 38.0
        } else {
            card.x1 - 14.0
        };
        text_renderer.draw_text_weighted(
            scene,
            &viewer_copy(&details.name, if compact { 34 } else { 52 }),
            16.0,
            theme.text_primary,
            transform * Affine::translate((card.x0 + 42.0, card.y0 + 13.0)),
            Some((title_right - card.x0 - 42.0) as f32),
            TextWeight::Semibold,
        );
        if details.favorite {
            draw_icon(
                scene,
                transform,
                Rect::new(
                    card.x1 - 30.0,
                    card.y0 + 14.0,
                    card.x1 - 14.0,
                    card.y0 + 30.0,
                ),
                Icon::Star,
                theme.accent_primary,
                1.6,
            );
        }
        let state = if details.edited {
            format!("{} · {}", details.dimensions, tr("photos-edited"))
        } else {
            format!("{} · {}", details.dimensions, tr("photos-shared-details"))
        };
        text_renderer.draw_text_weighted(
            scene,
            &state,
            12.0,
            theme.text_dim,
            transform * Affine::translate((card.x0 + 42.0, card.y0 + 37.0)),
            Some((card.x1 - card.x0 - 56.0) as f32),
            TextWeight::Regular,
        );

        let mut y = card.y0 + 62.0;
        if let Some(description) = details.description.as_deref() {
            text_renderer.draw_text_weighted(
                scene,
                &viewer_copy(description, if compact { 76 } else { 108 }),
                13.0,
                theme.text_primary,
                transform * Affine::translate((card.x0 + 14.0, y)),
                Some((card.width() - 28.0) as f32),
                TextWeight::Regular,
            );
            y += 24.0;
        }
        let mut context = Vec::new();
        if let Some(captured_at) = details.captured_at.as_deref() {
            context.push(format!("{}: {captured_at}", tr("photos-captured")));
        }
        if let Some(place) = details.place.as_deref() {
            context.push(format!("{}: {place}", tr("photos-place")));
        }
        if context.is_empty() {
            context.push(tr("photos-no-shared-context"));
        }
        text_renderer.draw_text_weighted(
            scene,
            &viewer_copy(&context.join(" · "), if compact { 56 } else { 76 }),
            12.0,
            theme.text_dim,
            transform * Affine::translate((card.x0 + 14.0, y)),
            Some((card.width() - 28.0) as f32),
            TextWeight::Regular,
        );
        y += 21.0;
        let mut labels = Vec::new();
        if !details.people.is_empty() {
            labels.push(format!(
                "{}: {}",
                tr("photos-people"),
                details.people.join(", ")
            ));
        }
        if !details.tags.is_empty() {
            labels.push(
                details
                    .tags
                    .iter()
                    .map(|tag| format!("#{tag}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        if let Some(folder) = details.folder.as_deref() {
            labels.push(format!("{}: {folder}", tr("photos-folder")));
        }
        if !labels.is_empty() && y < card.y1 - 10.0 {
            text_renderer.draw_text_weighted(
                scene,
                &viewer_copy(&labels.join(" · "), if compact { 56 } else { 78 }),
                12.0,
                theme.text_dim,
                transform * Affine::translate((card.x0 + 14.0, y)),
                Some((card.width() - 28.0) as f32),
                TextWeight::Regular,
            );
        }
    }

    fn refresh_search_provider(
        &self,
        doc: &AutoCommit,
    ) -> Result<(), ui::search::SearchCatalogError> {
        #[cfg(test)]
        if self.force_search_refresh_error {
            return Err(ui::search::SearchCatalogError::NonCanonicalResult);
        }
        self.search_provider.sync(doc)
    }

    fn advance_library_query(&mut self) {
        let query = self.library_query.trim().to_owned();
        let generation = self.search_provider.generation();
        if query.is_empty() {
            self.library_query_indexed.clear();
            self.library_query_generation = generation;
            self.library_query_cursor = None;
            self.library_query_matches.clear();
            self.library_query_complete = true;
            return;
        }
        if self.library_query_indexed != query || self.library_query_generation != generation {
            self.library_query_indexed.clone_from(&query);
            self.library_query_generation = generation;
            self.library_query_cursor = None;
            self.library_query_matches.clear();
            self.library_query_complete = false;
        }
        if self.library_query_complete {
            return;
        }
        let (ids, next, _inspected) = self
            .search_provider
            .photo_matches_page(&query, self.library_query_cursor.as_ref());
        self.library_query_matches.extend(ids);
        self.library_query_cursor = next;
        self.library_query_complete = self.library_query_cursor.is_none();
    }

    fn empty_state_text(&self) -> (String, String) {
        match self.canvas_state {
            CanvasState::Loading => (tr("photos-loading-title"), tr("photos-loading-body")),
            CanvasState::Error => (tr("photos-error-title"), tr("photos-error-body")),
            CanvasState::Empty | CanvasState::Ready => {
                (tr("photos-empty-title"), tr("photos-empty-body"))
            }
        }
    }

    /// The shared empty-state composition is ideal at normal reading sizes,
    /// but its single centred title/body runs cannot reflow inside a 390 px
    /// viewport once text reaches 200%. Keep that exceptional presentation
    /// local while retaining the same copy, icon, colors and semantics.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        title: &str,
        body: &str,
    ) {
        if theme.text_scale <= 1.25 || self.canvas_rect.width() >= 620.0 {
            EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                self.canvas_rect,
                &EmptyStateContent {
                    icon: Some(Icon::Image),
                    title,
                    message: Some(body),
                    action_label: None,
                },
                Point::ZERO,
            );
            return;
        }

        let bounds = self.canvas_rect;
        let content_w = (bounds.width() - 48.0).max(120.0);
        let title_size = 16.0;
        let body_size = 11.0;
        let title_lines = wrap_words(title, 19);
        let body_lines = wrap_words(body, 28);
        let scale = f64::from(theme.text_scale);
        let title_step = 21.0 * scale;
        let body_step = 15.0 * scale;
        let icon_r = 29.0;
        let content_h = icon_r * 2.0
            + 26.0
            + title_step * title_lines.len() as f64
            + 12.0
            + body_step * body_lines.len() as f64;
        let top = bounds.y0 + ((bounds.height() - content_h) * 0.42).max(24.0);
        let icon_center = Point::new(bounds.center().x, top + icon_r);
        let icon_plate = Circle::new(icon_center, icon_r);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &icon_plate,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &icon_plate,
        );
        draw_icon(
            scene,
            transform,
            Rect::from_center_size(icon_center, (24.0, 24.0)),
            Icon::Image,
            theme.tokens.colors.accent[6],
            1.5,
        );

        let mut y = top + icon_r * 2.0 + 26.0;
        for line in title_lines {
            draw_centered_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                &line,
                &TextStyle::new(title_size).with_weight(TextWeight::Medium),
                theme.text_primary,
                Point::new(bounds.center().x, y),
                content_w,
            );
            y += title_step;
        }
        y += 12.0;
        for line in body_lines {
            draw_centered_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                &line,
                &TextStyle::new(body_size),
                theme.text_dim,
                Point::new(bounds.center().x, y),
                content_w,
            );
            y += body_step;
        }
    }

    /// Lay out the toolbar strip: one taffy node per [`SLOTS`] entry (a
    /// button, a separator rule, or the greedy spacer), wrapped in the bar
    /// row itself. Split out of `build_layout` to keep that dispatch under
    /// the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "these taffy nodes are freshly built with fixed, valid styles"
    )]
    fn build_toolbar_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut bar_children = Vec::new();
        for slot in self.toolbar_slots() {
            let node = match *slot {
                Slot::Item(n) => match self.buttons.get_mut(n) {
                    Some((_, button)) => button.build_layout(taffy, theme, node_map),
                    // Unreachable: SLOTS only ever indexes within self.buttons.
                    None => taffy.new_leaf(Style::default()).unwrap(),
                },
                Slot::Separator => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::length(SEP_W),
                            height: Dimension::length(self.toolbar_btn_h * 0.6),
                        },
                        flex_shrink: 0.0,
                        ..Default::default()
                    })
                    .unwrap(),
                // Soaks the residual width so the export group sits hard
                // right instead of leaving a third of the strip bare.
                Slot::Spacer => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::length(0.0),
                            height: Dimension::length(0.0),
                        },
                        flex_grow: 1.0,
                        flex_shrink: 1.0,
                        ..Default::default()
                    })
                    .unwrap(),
            };
            bar_children.push(node);
        }
        taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        // `bar_h` at the scale the capsules were built for:
                        // the authored BAR_H at 1×.
                        height: Dimension::length(self.toolbar_btn_h + (BAR_H - BTN_H)),
                    },
                    flex_shrink: 0.0,
                    align_items: Some(AlignItems::Center),
                    gap: Size {
                        width: LengthPercentage::length(BTN_GAP),
                        height: LengthPercentage::length(0.0),
                    },
                    padding: taffy::Rect {
                        left: LengthPercentage::length(PAD),
                        right: LengthPercentage::length(PAD),
                        top: LengthPercentage::length(0.0),
                        bottom: LengthPercentage::length(0.0),
                    },
                    ..Default::default()
                },
                &bar_children,
            )
            .unwrap()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "People control geometry for every responsive band is computed in one place so hit targets stay consistent"
    )]
    fn layout_people_controls(&mut self) {
        let bounds = self.canvas_rect;
        let pad = if bounds.width() < 520.0 { 16.0 } else { 28.0 };
        let content = Rect::new(
            bounds.x0 + pad,
            bounds.y0 + 18.0,
            bounds.x1 - pad,
            bounds.y1 - 18.0,
        );
        let wide = content.width() >= 760.0;
        let overview = self.people_selection.is_none();
        // Keep the navigation and recognition card in separate vertical
        // bands.  The former implementation put the card *over* the
        // segmented navigation, leaving a very confusing invisible route
        // control in the People overview.
        let recognition_top = content.y0 + 118.0;
        let recognition_bottom = recognition_top + if wide { 86.0 } else { 122.0 };
        let body_y = if overview {
            recognition_bottom + 30.0
        } else {
            content.y0 + 124.0
        };
        let mut action_row = 0usize;
        let approved_count = self
            .people_controls
            .iter()
            .filter(|control| matches!(control.intent, PeopleIntent::OpenApproved(_)))
            .count();
        let people_lane_w = if overview && wide {
            (content.width() * 0.67).max(420.0)
        } else {
            content.width()
        };
        let columns = if overview && wide {
            if people_lane_w >= 760.0 { 3 } else { 2 }
        } else if wide {
            3
        } else {
            1
        };
        let gap = 12.0;
        let card_width = if wide {
            (people_lane_w - gap * (columns - 1) as f64) / columns as f64
        } else {
            content.width()
        };
        self.people_name_bounds = Rect::ZERO;
        if self.people_selection.is_some() && !self.people_merge_mode {
            let form_x = if wide {
                content.x0 + content.width() * 0.52
            } else {
                content.x0
            };
            self.people_name_bounds = Rect::new(
                form_x,
                body_y + if wide { 116.0 } else { 338.0 },
                content.x1,
                body_y + if wide { 160.0 } else { 382.0 },
            );
        }
        let mut approved_index = 0usize;
        let mut suggestion_index = 0usize;
        for control in &mut self.people_controls {
            control.bounds = match &control.intent {
                PeopleIntent::Back => Rect::new(
                    content.x1 - 152.0_f64.min(content.width()),
                    content.y0,
                    content.x1,
                    content.y0 + 44.0,
                ),
                PeopleIntent::ToggleRecognition if !wide => Rect::new(
                    content.x0 + 12.0,
                    recognition_top + 64.0,
                    content.x0 + (content.width() - 8.0) * 0.5,
                    recognition_top + 108.0,
                ),
                PeopleIntent::ToggleRecognition => Rect::new(
                    content.x1 - 356.0,
                    recognition_top + 21.0,
                    content.x1 - 182.0,
                    recognition_top + 65.0,
                ),
                PeopleIntent::Scan | PeopleIntent::CancelScan if !wide => Rect::new(
                    content.x0 + (content.width() + 8.0) * 0.5,
                    recognition_top + 64.0,
                    content.x1 - 12.0,
                    recognition_top + 108.0,
                ),
                PeopleIntent::Scan | PeopleIntent::CancelScan => Rect::new(
                    content.x1 - 170.0,
                    recognition_top + 21.0,
                    content.x1,
                    recognition_top + 65.0,
                ),
                PeopleIntent::OpenApproved(_) => {
                    let index = approved_index;
                    approved_index += 1;
                    if wide {
                        let row = index / columns;
                        let column = index % columns;
                        let x = content.x0 + column as f64 * (card_width + gap);
                        let y = body_y + 34.0 + row as f64 * 236.0;
                        Rect::new(x + 12.0, y + 176.0, x + card_width - 12.0, y + 220.0)
                    } else {
                        let y = body_y + 34.0 + index as f64 * 92.0;
                        Rect::new(content.x1 - 122.0, y + 20.0, content.x1 - 12.0, y + 64.0)
                    }
                }
                PeopleIntent::OpenSuggestion(_) => {
                    let index = suggestion_index;
                    suggestion_index += 1;
                    if overview && wide {
                        let lane_x = content.x0 + people_lane_w + 20.0;
                        let y = body_y + 34.0 + index as f64 * 164.0;
                        Rect::new(lane_x + 108.0, y + 92.0, content.x1 - 12.0, y + 136.0)
                    } else {
                        let approved_rows = approved_count.div_ceil(columns);
                        let section_y = if wide {
                            body_y + 42.0 + approved_rows as f64 * 236.0
                        } else {
                            body_y + 42.0 + approved_count as f64 * 92.0
                        };
                        if wide {
                            let row = index / columns;
                            let column = index % columns;
                            let x = content.x0 + column as f64 * (card_width + gap);
                            let y = section_y + 36.0 + row as f64 * 236.0;
                            Rect::new(x + 12.0, y + 176.0, x + card_width - 12.0, y + 220.0)
                        } else {
                            let y = section_y + 36.0 + index as f64 * 92.0;
                            Rect::new(content.x1 - 122.0, y + 20.0, content.x1 - 12.0, y + 64.0)
                        }
                    }
                }
                PeopleIntent::BrowseApproved
                | PeopleIntent::SaveName
                | PeopleIntent::BeginMerge
                | PeopleIntent::ApproveSuggestion
                | PeopleIntent::RejectSuggestion
                | PeopleIntent::HideSuggestion
                | PeopleIntent::RestoreSuggestion => {
                    let row = action_row;
                    action_row += 1;
                    let x = if wide {
                        content.x0 + content.width() * 0.52
                    } else {
                        content.x0
                    };
                    Rect::new(
                        x,
                        body_y + if wide { 176.0 } else { 392.0 } + row as f64 * 52.0,
                        content.x1,
                        body_y + if wide { 220.0 } else { 436.0 } + row as f64 * 52.0,
                    )
                }
                PeopleIntent::MergeApprovedInto(_) | PeopleIntent::MergeSuggestionWith(_) => {
                    let row = action_row;
                    action_row += 1;
                    Rect::new(
                        content.x0 + if wide { content.width() * 0.52 } else { 0.0 },
                        body_y + if wide { 176.0 } else { 392.0 } + row as f64 * 52.0,
                        content.x1,
                        body_y + if wide { 220.0 } else { 436.0 } + row as f64 * 52.0,
                    )
                }
                PeopleIntent::ToggleHidden if !wide => Rect::new(
                    content.x0,
                    content.y1 - 96.0,
                    content.x0 + (content.width() - 8.0) * 0.5,
                    content.y1 - 52.0,
                ),
                PeopleIntent::DeleteLocalData | PeopleIntent::ConfirmDeleteLocalData if !wide => {
                    Rect::new(
                        content.x0 + (content.width() + 8.0) * 0.5,
                        content.y1 - 96.0,
                        content.x1,
                        content.y1 - 52.0,
                    )
                }
                PeopleIntent::Page(forward) if !wide => {
                    let half = (content.width() - 8.0) * 0.5;
                    let x = if *forward {
                        content.x0 + half + 8.0
                    } else {
                        content.x0
                    };
                    Rect::new(x, content.y1 - 44.0, x + half, content.y1)
                }
                PeopleIntent::ToggleHidden => Rect::new(
                    content.x0,
                    content.y1 - 44.0,
                    content.x0 + 210.0,
                    content.y1,
                ),
                PeopleIntent::Page(forward) => {
                    let half = 96.0;
                    let x = if *forward {
                        content.x0 + 316.0
                    } else {
                        content.x0 + 216.0
                    };
                    Rect::new(x, content.y1 - 96.0, x + half, content.y1 - 52.0)
                }
                PeopleIntent::DeleteLocalData | PeopleIntent::ConfirmDeleteLocalData => Rect::new(
                    content.x1 - 210.0,
                    content.y1 - 44.0,
                    content.x1,
                    content.y1,
                ),
            };
        }
    }

    /// Lay out the canvas: the `ImageViewer` if a photograph is open, else a
    /// placeholder leaf sized to soak the remaining height. `flex_basis: 0`
    /// with `min_size: 0` is what stops a large image's intrinsic size from
    /// pushing the layout wider than the window — the failure mode where
    /// content is silently clipped because the container grew past its
    /// parent instead of the content being scaled to fit. Split out of
    /// `build_layout` to keep that dispatch under the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "these taffy nodes are freshly built with fixed, valid styles"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "the canvas layout dispatch keeps each surface's retained widget nodes together"
    )]
    fn build_canvas_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let people_surface = self.library_mode
            && (self.library_destination == LibraryDestination::People || self.people_mode);
        let canvas = if people_surface {
            self.layout_people_controls();
            let mut children = Vec::new();
            let show_name = self.people_selection.is_some() && !self.people_merge_mode;
            if show_name {
                let field = self.people_name_input.build_layout(taffy, theme, node_map);
                let rect = self.people_name_bounds;
                let _ = taffy.set_style(
                    field,
                    Style {
                        position: Position::Absolute,
                        inset: taffy::Rect {
                            left: LengthPercentageAuto::length(
                                (rect.x0 - self.canvas_rect.x0) as f32,
                            ),
                            top: LengthPercentageAuto::length(
                                (rect.y0 - self.canvas_rect.y0) as f32,
                            ),
                            right: LengthPercentageAuto::auto(),
                            bottom: LengthPercentageAuto::auto(),
                        },
                        size: Size {
                            width: Dimension::length(rect.width() as f32),
                            height: Dimension::length(rect.height() as f32),
                        },
                        ..Default::default()
                    },
                );
                children.push(field);
            }
            for control in &mut self.people_controls {
                let node = control.button.build_layout(taffy, theme, node_map);
                let rect = control.bounds;
                let _ = taffy.set_style(
                    node,
                    Style {
                        position: Position::Absolute,
                        inset: taffy::Rect {
                            left: LengthPercentageAuto::length(
                                (rect.x0 - self.canvas_rect.x0) as f32,
                            ),
                            top: LengthPercentageAuto::length(
                                (rect.y0 - self.canvas_rect.y0) as f32,
                            ),
                            right: LengthPercentageAuto::auto(),
                            bottom: LengthPercentageAuto::auto(),
                        },
                        size: Size {
                            width: Dimension::length(rect.width() as f32),
                            height: Dimension::length(rect.height() as f32),
                        },
                        ..Default::default()
                    },
                );
                children.push(node);
            }
            taffy
                .new_with_children(
                    Style {
                        position: Position::Relative,
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::percent(1.0),
                        },
                        ..Default::default()
                    },
                    &children,
                )
                .unwrap()
        } else {
            match self.viewer.as_mut() {
                Some(v) => v.build_layout(taffy, theme, node_map),
                None => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::auto(),
                        },
                        flex_grow: 1.0,
                        ..Default::default()
                    })
                    .unwrap(),
            }
        };
        taffy
            .set_style(
                canvas,
                Style {
                    size: Size {
                        width: Dimension::auto(),
                        height: Dimension::auto(),
                    },
                    min_size: Size {
                        width: Dimension::length(0.0),
                        height: Dimension::length(0.0),
                    },
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    flex_basis: Dimension::length(0.0),
                    margin: taffy::Rect {
                        left: LengthPercentageAuto::length(0.0),
                        right: LengthPercentageAuto::length(0.0),
                        top: LengthPercentageAuto::length(CANVAS_INSET),
                        bottom: LengthPercentageAuto::length(CANVAS_INSET),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        canvas
    }

    /// Lay out the status band: zero height with nothing to say (reserving
    /// the band anyway would leave a rule and a strip of empty chrome under
    /// an empty state, which reads as a control that failed to draw), else a
    /// fixed band. Split out of `build_layout` to keep that dispatch under
    /// the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "this taffy node is freshly built with a fixed, valid style"
    )]
    fn build_status_node(&self, taffy: &mut TaffyTree<()>, text_scale: f32) -> NodeId {
        taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(if self.status.is_empty() {
                        0.0
                    } else {
                        status_h(text_scale)
                    }),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap()
    }

    #[allow(clippy::unwrap_used, reason = "fresh layout node with fixed size")]
    fn build_library_search_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.library_search
            .set_placeholder(if theme.text_scale > 1.25 {
                "Search photos"
            } else {
                "Search photos, people, places, text"
            });
        let field = self.library_search.build_layout(taffy, theme, node_map);
        taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(if self.library_mode { 46.0 } else { 0.0 }),
                    },
                    padding: taffy::Rect {
                        left: LengthPercentage::length(PAD),
                        right: LengthPercentage::length(PAD),
                        top: LengthPercentage::length(if self.library_mode { 4.0 } else { 0.0 }),
                        bottom: LengthPercentage::length(if self.library_mode { 4.0 } else { 0.0 }),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &[field],
            )
            .unwrap()
    }

    fn draw_collection_pager(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        bounds: Rect,
        page: usize,
        total: usize,
        previous: CollectionPageHit,
        next: CollectionPageHit,
    ) {
        let theme = ctx.theme;
        self.collection_page_hits.clear();
        let page_size = if matches!(previous, CollectionPageHit::AlbumsPrevious)
            && self.library_destination == LibraryDestination::Albums
            && self.canvas_rect.width() >= 700.0
        {
            8
        } else {
            COLLECTION_PAGE_SIZE
        };
        let pages = total.max(1).div_ceil(page_size);
        if pages <= 1 {
            return;
        }
        let previous_enabled = page > 0;
        let next_enabled = page + 1 < pages;
        let button_w = 88.0;
        let label_w = 104.0;
        let previous_rect = Rect::new(bounds.x0, bounds.y0, bounds.x0 + button_w, bounds.y1);
        let next_rect = Rect::new(bounds.x1 - button_w, bounds.y0, bounds.x1, bounds.y1);
        let presence = PresenceCtx::empty();
        for (hit, rect, enabled) in [
            (previous, previous_rect, previous_enabled),
            (next, next_rect, next_enabled),
        ] {
            if let Some((_, button)) = self
                .collection_page_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(ctx.scene, ctx.transform, ctx.text, theme, &presence);
            }
            self.collection_page_hits.push((hit, rect, enabled));
        }
        ctx.text.draw_text_weighted(
            ctx.scene,
            &format!("Page {} of {pages}", page + 1),
            12.0,
            theme.text_dim,
            ctx.transform
                * Affine::translate((bounds.center().x - label_w * 0.5, bounds.y0 + 12.0)),
            Some(label_w as f32),
            TextWeight::Regular,
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the import status banner keeps its progress, actions and exact hit geometry together"
    )]
    fn draw_directory_import_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use vello::peniko::Fill;
        let (title, summary, issue_count, active) =
            if let Some(job) = self.directory_import.as_ref() {
                (
                    if job.mode == DirectoryImportMode::Import {
                        "Importing photo folder"
                    } else {
                        "Reindexing photo folder"
                    }
                    .to_owned(),
                    format!(
                        "{} found · {} imported · {} refreshed · {} issue{}",
                        job.discovered,
                        job.imported,
                        job.refreshed,
                        job.failures.len(),
                        if job.failures.len() == 1 { "" } else { "s" }
                    ),
                    job.failures.len(),
                    true,
                )
            } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
                let noun = if receipt.mode == DirectoryImportMode::Reindex {
                    "Reindex complete"
                } else {
                    "Import complete"
                };
                (
                    noun.to_owned(),
                    format!(
                        "{} found · {} imported · {} refreshed · {} unchanged · {} issue{}{}{}",
                        receipt.discovered,
                        receipt.imported,
                        receipt.refreshed,
                        receipt.unchanged,
                        receipt.failure_count,
                        if receipt.failure_count == 1 { "" } else { "s" },
                        if receipt.cancelled {
                            " · cancelled"
                        } else {
                            ""
                        },
                        if receipt.limit_reached {
                            " · folder limit reached"
                        } else {
                            ""
                        },
                    ),
                    receipt.failure_count,
                    false,
                )
            } else {
                self.directory_import_details_layout = None;
                return;
            };
        let compact = self.canvas_rect.width() < 620.0;
        let height = if compact { 142.0 } else { 98.0 };
        let bounds = Rect::new(
            self.canvas_rect.x0 + 16.0,
            (self.canvas_rect.y1 - height - 12.0).max(self.canvas_rect.y0 + 8.0),
            self.canvas_rect.x1 - 16.0,
            self.canvas_rect.y1 - 12.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(bounds, 10.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(bounds, 10.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            &title,
            14.0,
            theme.text_primary,
            transform * Affine::translate((bounds.x0 + 16.0, bounds.y0 + 15.0)),
            Some((bounds.width() - if compact { 32.0 } else { 260.0 }) as f32),
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            &summary,
            12.0,
            theme.text_dim,
            transform * Affine::translate((bounds.x0 + 16.0, bounds.y0 + 41.0)),
            Some((bounds.width() - if compact { 32.0 } else { 260.0 }) as f32),
            TextWeight::Regular,
        );

        if active {
            let progress_right = if compact {
                bounds.x1 - 16.0
            } else {
                bounds.x1 - 244.0
            };
            self.directory_import_progress
                .apply_layout_external(Rect::new(
                    bounds.x0 + 16.0,
                    bounds.y0 + 68.0,
                    progress_right,
                    bounds.y0 + 76.0,
                ));
            self.directory_import_progress.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &PresenceCtx::empty(),
            );
        }

        let visible = |hit| match hit {
            DirectoryStatusHit::Stop => active,
            DirectoryStatusHit::Details => issue_count > 0,
            DirectoryStatusHit::Dismiss => !active,
        };
        let mut shown = self
            .directory_import_buttons
            .iter()
            .filter(|(hit, _)| visible(*hit))
            .count();
        let button_y = if compact {
            bounds.y1 - 50.0
        } else {
            bounds.y0 + 29.0
        };
        let mut button_x = if compact {
            bounds.x0 + 16.0
        } else {
            bounds.x1 - 16.0 - shown as f64 * 104.0 - shown.saturating_sub(1) as f64 * 8.0
        };
        for (hit, button) in &mut self.directory_import_buttons {
            if visible(*hit) {
                let rect = Rect::new(button_x, button_y, button_x + 104.0, button_y + 40.0);
                button.set_enabled(true);
                button.apply_layout_external(rect);
                button.draw(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &PresenceCtx::empty(),
                );
                button_x += 112.0;
                shown = shown.saturating_sub(1);
            } else {
                button.set_enabled(false);
                button.apply_layout_external(Rect::ZERO);
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the import-issues modal keeps layout, paging, paint and hit geometry together so they cannot drift"
    )]
    fn draw_directory_import_details(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.directory_import_details_open {
            self.directory_import_details_layout = None;
            return;
        }
        let (failures, failure_count) = if let Some(job) = self.directory_import.as_ref() {
            (&job.failures[..], job.failures.len())
        } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
            (&receipt.failures[..], receipt.failure_count)
        } else {
            self.directory_import_details_open = false;
            self.directory_import_details_layout = None;
            return;
        };
        if failures.is_empty() {
            self.directory_import_details_open = false;
            self.directory_import_details_layout = None;
            return;
        }
        let retained = failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT);
        let pages = retained.div_ceil(DIRECTORY_FAILURES_PER_PAGE).max(1);
        self.directory_import_details_page = self
            .directory_import_details_page
            .min(pages.saturating_sub(1));
        let page = self.directory_import_details_page;
        let actions = [
            ModalAction::secondary("Previous").enabled(page > 0),
            ModalAction::secondary("Next").enabled(page + 1 < pages),
            ModalAction::primary("Close"),
        ];
        let base_spec = ModalSpec::new("Folder import issues")
            .actions(&actions)
            .body_height(282.0)
            .max_width(620.0)
            .dismiss_on_scrim(true);
        let layout = ModalFrame::layout(self.rect, &base_spec, text_renderer, theme);
        let hovered = |index: usize| {
            layout
                .actions
                .get(index)
                .is_some_and(|rect| rect.contains(self.pointer))
        };
        let actions = [
            ModalAction::secondary("Previous")
                .enabled(page > 0)
                .hovered(hovered(0)),
            ModalAction::secondary("Next")
                .enabled(page + 1 < pages)
                .hovered(hovered(1)),
            ModalAction::primary("Close").hovered(hovered(2)),
        ];
        let spec = ModalSpec::new("Folder import issues")
            .actions(&actions)
            .body_height(282.0)
            .max_width(620.0)
            .dismiss_on_scrim(true)
            .close_hovered(layout.close.is_some_and(|rect| rect.contains(self.pointer)));
        ModalFrame::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
        let (ink, dim) = ModalFrame::ink(theme);
        let not_retained = failure_count.saturating_sub(retained);
        let overview = if not_retained == 0 {
            format!(
                "{failure_count} issue{} · page {} of {pages}",
                if failure_count == 1 { "" } else { "s" },
                page + 1
            )
        } else {
            format!(
                "{failure_count} issues · {retained} details retained · {not_retained} additional"
            )
        };
        text_renderer.draw_text_weighted(
            scene,
            &overview,
            12.0,
            dim,
            transform * Affine::translate((layout.body.x0, layout.body.y0 + 2.0)),
            Some(layout.body.width() as f32),
            TextWeight::Regular,
        );
        let list = Rect::new(
            layout.body.x0,
            layout.body.y0 + 26.0,
            layout.body.x1,
            layout.body.y1,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &list,
        );
        for (row, failure) in failures
            .iter()
            .take(retained)
            .skip(page * DIRECTORY_FAILURES_PER_PAGE)
            .take(DIRECTORY_FAILURES_PER_PAGE)
            .enumerate()
        {
            let y = list.y0 + row as f64 * 58.0;
            let name = failure
                .source
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Unavailable item");
            text_renderer.draw_text_weighted(
                scene,
                name,
                13.0,
                ink,
                transform * Affine::translate((list.x0, y + 5.0)),
                Some((list.width() - 118.0) as f32),
                TextWeight::Semibold,
            );
            text_renderer.draw_text_weighted(
                scene,
                if failure.retryable {
                    "Can retry"
                } else {
                    "Needs a change"
                },
                11.0,
                if failure.retryable {
                    theme.accent_primary
                } else {
                    dim
                },
                transform * Affine::translate((list.x1 - 104.0, y + 6.0)),
                Some(104.0),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                &failure.message,
                11.0,
                dim,
                transform * Affine::translate((list.x0, y + 28.0)),
                Some(list.width() as f32),
                TextWeight::Regular,
            );
            if row + 1 < DIRECTORY_FAILURES_PER_PAGE {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(list.x0, y + 54.0, list.x1, y + 55.0),
                );
            }
        }
        scene.pop_layer();
        ModalFrame::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        self.directory_import_details_layout = Some(layout);
    }

    /// Paint the shared library destination control over a canvas-owned
    /// header. `ToggleGroup::paint` keeps the interaction language identical
    /// to retained segmented controls while this app preserves its bounded
    /// custom timeline/map hit testing.
    fn draw_library_navigation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
    ) {
        let labels = ["Timeline", "Albums", "People", "Places", "Analyze"];
        let intents = [
            PlacesHit::Timeline,
            PlacesHit::Albums,
            PlacesHit::People,
            PlacesHit::Places,
            PlacesHit::Analysis,
        ];
        let selected = match self.library_destination {
            LibraryDestination::Timeline => 0,
            LibraryDestination::Albums => 1,
            LibraryDestination::People => 2,
            LibraryDestination::Places => 3,
            LibraryDestination::Analysis => 4,
        };
        let segment_width = bounds.width() / labels.len() as f64;
        let segments: Vec<_> = (0..labels.len())
            .map(|index| {
                Rect::new(
                    bounds.x0 + segment_width * index as f64,
                    bounds.y0,
                    if index + 1 == labels.len() {
                        bounds.x1
                    } else {
                        bounds.x0 + segment_width * (index + 1) as f64
                    },
                    bounds.y1,
                )
            })
            .collect();
        ToggleGroup::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &SegmentedFace::new(&segments, &labels, selected)
                .pill()
                .radius(8.0)
                .text_size(12.0),
        );
        self.places_hits.extend(intents.into_iter().zip(segments));
    }

    /// First-class album overview. Album cards are a bounded projection over
    /// canonical album resources; their covers reuse the asynchronous
    /// thumbnail cache and never decode originals during paint.
    #[expect(
        clippy::too_many_lines,
        reason = "the album overview keeps responsive grid geometry, bounded cover paint and hit targets together"
    )]
    fn draw_albums(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        self.places_hits.clear();
        self.album_cards.clear();
        self.album_edit_cards.clear();
        self.album_editor_hits.clear();
        self.collection_page_hits.clear();

        let bounds = self.canvas_rect;
        let compact = bounds.width() < 700.0 || theme.text_scale > 1.25;
        let inset = if compact { 16.0 } else { 28.0 };
        let content = Rect::new(
            bounds.x0 + inset,
            bounds.y0 + 16.0,
            bounds.x1 - inset,
            bounds.y1 - 16.0,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Albums",
            if compact { 23.0 } else { 28.0 },
            theme.text_primary,
            transform * Affine::translate((content.x0, content.y0)),
            Some(180.0),
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!(
                "{} album{} · shared and available on every device",
                self.library_albums.len(),
                if self.library_albums.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            13.0,
            theme.text_dim,
            transform * Affine::translate((content.x0, content.y0 + 35.0)),
            Some((content.width() - 220.0).max(120.0) as f32),
            TextWeight::Regular,
        );

        let nav_y = if compact {
            content.y0 + 64.0
        } else {
            content.y0
        };
        let nav_w = if compact {
            content.width()
        } else {
            content.width().min(450.0)
        };
        let nav_x = if compact {
            content.x0
        } else {
            content.x1 - nav_w
        };
        self.draw_library_navigation(
            scene,
            transform,
            text_renderer,
            theme,
            Rect::new(nav_x, nav_y, nav_x + nav_w, nav_y + 40.0),
        );

        let actions_y = nav_y + if compact { 54.0 } else { 58.0 };
        for (hit, rect, enabled) in [
            (
                AlbumEditorHit::New,
                Rect::new(content.x0, actions_y, content.x0 + 118.0, actions_y + 44.0),
                true,
            ),
            (
                AlbumEditorHit::Edit,
                Rect::new(
                    content.x0 + 126.0,
                    actions_y,
                    content.x0 + 252.0,
                    actions_y + 44.0,
                ),
                self.active_album.is_some(),
            ),
        ] {
            if hit == AlbumEditorHit::Edit && !enabled {
                continue;
            }
            if let Some((_, button)) = self
                .library_album_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, theme, presence);
            }
            if enabled {
                self.album_editor_hits.push((hit, rect));
            }
        }

        let grid_y = actions_y + 62.0;
        if self.library_albums.is_empty() {
            let empty = Rect::new(content.x0, grid_y, content.x1, content.y1);
            draw_icon(
                scene,
                transform,
                Rect::new(
                    empty.center().x - 28.0,
                    empty.y0 + 44.0,
                    empty.center().x + 28.0,
                    empty.y0 + 100.0,
                ),
                Icon::Image,
                theme.text_dim,
                1.5,
            );
            text_renderer.draw_text_weighted(
                scene,
                "Bring related moments together",
                20.0,
                theme.text_primary,
                transform * Affine::translate((empty.x0, empty.y0 + 118.0)),
                Some(empty.width() as f32),
                TextWeight::Semibold,
            );
            text_renderer.draw_text_weighted(
                scene,
                "Create an album, then select photographs to add them. Albums preserve shared membership and history.",
                14.0,
                theme.text_dim,
                transform * Affine::translate((empty.x0, empty.y0 + 151.0)),
                Some(empty.width() as f32),
                TextWeight::Regular,
            );
            self.draw_album_editor(scene, transform, text_renderer, theme, presence);
            return;
        }

        let columns = if content.width() >= 1080.0 {
            4
        } else if content.width() >= 720.0 {
            3
        } else if content.width() >= 430.0 {
            2
        } else {
            1
        };
        let gap = if compact { 12.0 } else { 18.0 };
        let card_w = (content.width() - gap * (columns - 1) as f64) / columns as f64;
        let card_h = if columns == 1 { 188.0 } else { 220.0 };
        let album_page_size = if compact { COLLECTION_PAGE_SIZE } else { 8 };
        let start = self.albums_page * album_page_size;
        for (index, (id, name, count)) in self
            .library_albums
            .iter()
            .skip(start)
            .take(album_page_size)
            .enumerate()
        {
            let row = index / columns;
            let column = index % columns;
            let card = Rect::new(
                content.x0 + column as f64 * (card_w + gap),
                grid_y + row as f64 * (card_h + gap),
                content.x0 + column as f64 * (card_w + gap) + card_w,
                grid_y + row as f64 * (card_h + gap) + card_h,
            );
            if card.y0 >= content.y1 {
                break;
            }
            scene.fill(
                Fill::NonZero,
                transform,
                if card.contains(self.pointer) {
                    theme.bg_hover
                } else {
                    theme.tokens.colors.surfaces.window_card
                },
                None,
                &RoundedRect::from_rect(card, 12.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &RoundedRect::from_rect(card, 12.0),
            );
            let preview = Rect::new(card.x0 + 8.0, card.y0 + 8.0, card.x1 - 8.0, card.y1 - 60.0);
            let covers = self
                .library_album_covers
                .get(id)
                .cloned()
                .unwrap_or_default();
            if covers.is_empty() {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(preview, 8.0),
                );
                draw_icon(
                    scene,
                    transform,
                    preview.inflate(-preview.width() * 0.35, -preview.height() * 0.28),
                    Icon::Image,
                    theme.text_dim,
                    1.5,
                );
            } else {
                let mosaic_columns = if covers.len() == 1 { 1 } else { 2 };
                let mosaic_rows = covers.len().div_ceil(mosaic_columns);
                // A slim gutter makes an album cover read as a collection of
                // moments rather than one accidentally sliced photograph.
                // Keep the raised preview underneath so the gutter inherits
                // the system surface rather than a hard-coded colour.
                let mosaic_gap = 3.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(preview, 8.0),
                );
                for (cover_index, cover_id) in covers.iter().enumerate() {
                    let col = cover_index % mosaic_columns;
                    let row = cover_index / mosaic_columns;
                    let cell = Rect::new(
                        preview.x0 + preview.width() * col as f64 / mosaic_columns as f64,
                        preview.y0 + preview.height() * row as f64 / mosaic_rows as f64,
                        preview.x0 + preview.width() * (col + 1) as f64 / mosaic_columns as f64,
                        preview.y0 + preview.height() * (row + 1) as f64 / mosaic_rows as f64,
                    );
                    let tile = Rect::new(
                        cell.x0 + if col == 0 { 0.0 } else { mosaic_gap * 0.5 },
                        cell.y0 + if row == 0 { 0.0 } else { mosaic_gap * 0.5 },
                        cell.x1
                            - if col + 1 == mosaic_columns {
                                0.0
                            } else {
                                mosaic_gap * 0.5
                            },
                        cell.y1
                            - if row + 1 == mosaic_rows {
                                0.0
                            } else {
                                mosaic_gap * 0.5
                            },
                    );
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        theme.tokens.colors.neutral[2],
                        None,
                        &tile,
                    );
                    if let Some(image) = self.library_thumbnails.get(cover_id) {
                        let scale = (tile.width() / f64::from(image.width))
                            .max(tile.height() / f64::from(image.height));
                        let drawn_w = f64::from(image.width) * scale;
                        let drawn_h = f64::from(image.height) * scale;
                        scene.push_layer(
                            Fill::NonZero,
                            vello::peniko::BlendMode::default(),
                            1.0,
                            transform,
                            &tile,
                        );
                        scene.draw_image(
                            image,
                            transform
                                * Affine::translate((
                                    tile.center().x - drawn_w * 0.5,
                                    tile.center().y - drawn_h * 0.5,
                                ))
                                * Affine::scale(scale),
                        );
                        scene.pop_layer();
                    } else {
                        let icon_size = tile.width().min(tile.height()).min(36.0);
                        draw_icon(
                            scene,
                            transform,
                            Rect::new(
                                tile.center().x - icon_size * 0.5,
                                tile.center().y - icon_size * 0.5,
                                tile.center().x + icon_size * 0.5,
                                tile.center().y + icon_size * 0.5,
                            ),
                            Icon::Image,
                            theme.text_dim,
                            1.25,
                        );
                    }
                }
            }
            text_renderer.draw_text_weighted(
                scene,
                name,
                15.0,
                theme.text_primary,
                transform * Affine::translate((card.x0 + 14.0, card.y1 - 45.0)),
                Some((card.width() - 78.0) as f32),
                TextWeight::Semibold,
            );
            text_renderer.draw_text_weighted(
                scene,
                &format!("{count} photo{}", if *count == 1 { "" } else { "s" }),
                12.0,
                theme.text_dim,
                transform * Affine::translate((card.x0 + 14.0, card.y1 - 23.0)),
                Some((card.width() - 78.0) as f32),
                TextWeight::Regular,
            );
            let edit = Rect::new(
                card.x1 - 54.0,
                card.y1 - 50.0,
                card.x1 - 10.0,
                card.y1 - 6.0,
            );
            // Keep the secondary card operation on the shared button face:
            // hover and focus feedback must agree with People, Places, and
            // the metadata inspector even though card activation itself is
            // handled by this bounded gallery surface.
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(edit, "")
                    .style(ButtonStyle::Ghost)
                    .icon(Icon::Pencil)
                    .radius(10.0)
                    .hovered(edit.contains(self.pointer))
                    .accessible_name("Edit album"),
            );
            self.album_edit_cards.push((id.clone(), edit));
            self.album_cards.push((Some(id.clone()), card));
        }
        self.draw_collection_pager(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            Rect::new(content.x1 - 292.0, actions_y, content.x1, actions_y + 44.0),
            self.albums_page,
            self.library_albums.len(),
            CollectionPageHit::AlbumsPrevious,
            CollectionPageHit::AlbumsNext,
        );
        self.draw_album_editor(scene, transform, text_renderer, theme, presence);
    }

    /// Paint a small, cropped cover mosaic without decoding or retaining any
    /// image beyond the bounded thumbnail cache.  A group of photographs is
    /// much more legible than a generic avatar, particularly when a person
    /// has not been named yet.
    fn draw_people_mosaic(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        photo_ids: &[String],
    ) {
        use vello::peniko::Fill;

        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.neutral[2],
            None,
            &RoundedRect::from_rect(rect, 10.0),
        );
        if photo_ids.is_empty() {
            draw_icon(
                scene,
                transform,
                rect.inflate(-rect.width() * 0.35, -rect.height() * 0.35),
                Icon::Users,
                theme.text_dim,
                1.5,
            );
            return;
        }
        let count = photo_ids.len().min(4);
        let columns = if count == 1 { 1 } else { 2 };
        let rows = count.div_ceil(columns);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(rect, 10.0),
        );
        for (index, photo_id) in photo_ids.iter().take(count).enumerate() {
            let column = index % columns;
            let row = index / columns;
            let tile = Rect::new(
                rect.x0 + rect.width() * column as f64 / columns as f64,
                rect.y0 + rect.height() * row as f64 / rows as f64,
                rect.x0 + rect.width() * (column + 1) as f64 / columns as f64,
                rect.y0 + rect.height() * (row + 1) as f64 / rows as f64,
            );
            if let Some(image) = self.library_thumbnails.get(photo_id) {
                let scale = (tile.width() / f64::from(image.width))
                    .max(tile.height() / f64::from(image.height));
                let drawn_w = f64::from(image.width) * scale;
                let drawn_h = f64::from(image.height) * scale;
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &tile,
                );
                scene.draw_image(
                    image,
                    transform
                        * Affine::translate((
                            tile.center().x - drawn_w * 0.5,
                            tile.center().y - drawn_h * 0.5,
                        ))
                        * Affine::scale(scale),
                );
                scene.pop_layer();
            } else {
                draw_icon(
                    scene,
                    transform,
                    tile.inflate(-tile.width() * 0.36, -tile.height() * 0.30),
                    Icon::Image,
                    theme.text_dim,
                    1.25,
                );
            }
        }
        scene.pop_layer();
    }

    fn draw_people_card(
        &self,
        ctx: &mut PaintCtx<'_>,
        card: Rect,
        title: &str,
        subtitle: &str,
        photo_ids: &[String],
        private: bool,
        compact: bool,
    ) {
        use vello::peniko::Fill;
        let theme = ctx.theme;

        let hovered = card.contains(self.pointer);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            if hovered {
                theme.bg_hover
            } else {
                theme.tokens.colors.surfaces.window_card
            },
            None,
            &RoundedRect::from_rect(card, 12.0),
        );
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(card, 12.0),
        );
        let (mosaic, copy_x, title_y, subtitle_y, copy_w) = if compact {
            (
                Rect::new(card.x0 + 8.0, card.y0 + 8.0, card.x0 + 94.0, card.y1 - 8.0),
                card.x0 + 108.0,
                card.y0 + 17.0,
                card.y0 + 42.0,
                (card.x1 - card.x0 - 130.0).max(72.0),
            )
        } else {
            (
                Rect::new(
                    card.x0 + 10.0,
                    card.y0 + 10.0,
                    card.x1 - 10.0,
                    card.y0 + 118.0,
                ),
                card.x0 + 14.0,
                card.y0 + 132.0,
                card.y0 + 156.0,
                card.width() - 28.0,
            )
        };
        self.draw_people_mosaic(ctx.scene, ctx.transform, theme, mosaic, photo_ids);
        ctx.text.draw_text_weighted(
            ctx.scene,
            title,
            if compact { 15.0 } else { 16.0 },
            theme.text_primary,
            ctx.transform * Affine::translate((copy_x, title_y)),
            Some(copy_w as f32),
            TextWeight::Semibold,
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            subtitle,
            12.0,
            theme.text_dim,
            ctx.transform * Affine::translate((copy_x, subtitle_y)),
            Some(copy_w as f32),
            TextWeight::Regular,
        );
        if private {
            let private_x = if compact { copy_x } else { card.x0 + 14.0 };
            let private_y = if compact {
                card.y0 + 64.0
            } else {
                card.y0 + 180.0
            };
            draw_icon(
                ctx.scene,
                ctx.transform,
                Rect::new(private_x, private_y, private_x + 14.0, private_y + 14.0),
                Icon::Shield,
                theme.text_dim,
                1.2,
            );
            ctx.text.draw_text_weighted(
                ctx.scene,
                "Private on this device",
                11.0,
                theme.text_dim,
                ctx.transform * Affine::translate((private_x + 19.0, private_y - 1.0)),
                Some((card.x1 - private_x - 28.0) as f32),
                TextWeight::Medium,
            );
        }
    }

    /// A deliberately calm People home: approved shared identities are a
    /// collection, while local face work is a private review queue. It never
    /// paints a made-up suggestion when no signed face model is installed.
    #[expect(
        clippy::too_many_lines,
        reason = "the People home keeps overview, review queue and detail geometry together with their hit targets"
    )]
    fn draw_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use vello::peniko::Fill;

        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        let bounds = self.canvas_rect;
        let inset = if bounds.width() < 520.0 { 16.0 } else { 28.0 };
        let content = Rect::new(
            bounds.x0 + inset,
            bounds.y0 + 18.0,
            bounds.x1 - inset,
            bounds.y1 - 18.0,
        );
        let wide = content.width() >= 760.0;
        let nav = Rect::new(
            content.x0,
            content.y0 + 62.0,
            content.x1.min(content.x0 + 520.0),
            content.y0 + 102.0,
        );
        let recognition_top = content.y0 + 118.0;
        let recognition = Rect::new(
            content.x0,
            recognition_top,
            content.x1,
            recognition_top + if wide { 86.0 } else { 122.0 },
        );

        text_renderer.draw_text_weighted(
            scene,
            "People",
            if wide { 30.0 } else { 26.0 },
            theme.text_primary,
            transform * Affine::translate((content.x0, content.y0)),
            Some((content.width() - 164.0).max(160.0) as f32),
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Your people, with private suggestions kept on this device until you choose to share a name.",
            13.0,
            theme.text_dim,
            transform * Affine::translate((content.x0, content.y0 + 37.0)),
            Some((content.width() - 164.0).max(180.0) as f32),
            TextWeight::Regular,
        );
        self.draw_library_navigation(scene, transform, text_renderer, theme, nav);

        if let Some(selection) = self.people_selection.clone() {
            let body_y = content.y0 + 124.0;
            let (title, subtitle, photo_ids, private, hidden) = match &selection {
                PeopleSelection::Approved(name) => {
                    let ids = self
                        .people_approved
                        .iter()
                        .find(|person| &person.label == name)
                        .map_or_else(Vec::new, |person| person.photo_ids.clone());
                    (
                        name.clone(),
                        format!(
                            "{} shared photograph{}",
                            ids.len(),
                            if ids.len() == 1 { "" } else { "s" }
                        ),
                        ids,
                        false,
                        false,
                    )
                }
                PeopleSelection::Suggestion(id) => self.people_controller.cluster(id).map_or_else(
                    || {
                        (
                            "Suggestion unavailable".to_owned(),
                            "This local suggestion is no longer available.".to_owned(),
                            Vec::new(),
                            true,
                            true,
                        )
                    },
                    |cluster| {
                        (
                            cluster
                                .display_name
                                .clone()
                                .unwrap_or_else(|| "Who is this?".to_owned()),
                            format!(
                                "{} · {} photograph{}",
                                cluster.origin.label(),
                                cluster.photos.len(),
                                if cluster.photos.len() == 1 { "" } else { "s" }
                            ),
                            cluster.visible_photo_ids(),
                            true,
                            cluster.hidden || cluster.rejected,
                        )
                    },
                ),
            };
            let panel = Rect::new(content.x0, body_y, content.x1, content.y1);
            let reveal = if theme.reduce_motion {
                1.0_f32
            } else {
                self.people_detail_transition.value(ui::time::now())
            };
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                reveal,
                transform,
                &RoundedRect::from_rect(panel, 14.0),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.window_card,
                None,
                &RoundedRect::from_rect(panel, 14.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &RoundedRect::from_rect(panel, 14.0),
            );
            let (mosaic, copy_x) = if wide {
                (
                    Rect::new(
                        panel.x0 + 18.0,
                        panel.y0 + 18.0,
                        panel.x0 + panel.width() * 0.47,
                        panel.y0 + 254.0,
                    ),
                    panel.x0 + panel.width() * 0.52,
                )
            } else {
                (
                    Rect::new(
                        panel.x0 + 14.0,
                        panel.y0 + 14.0,
                        panel.x1 - 14.0,
                        panel.y0 + 180.0,
                    ),
                    panel.x0 + 16.0,
                )
            };
            self.draw_people_mosaic(scene, transform, theme, mosaic, &photo_ids);
            let copy_y = if wide {
                panel.y0 + 24.0
            } else {
                panel.y0 + 202.0
            };
            if private {
                let badge = Rect::new(copy_x, copy_y, copy_x + 22.0, copy_y + 22.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(badge, 6.0),
                );
                draw_icon(
                    scene,
                    transform,
                    badge.inflate(-5.0, -5.0),
                    Icon::Shield,
                    theme.text_dim,
                    1.2,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    "PRIVATE SUGGESTION",
                    11.0,
                    theme.text_dim,
                    transform * Affine::translate((copy_x + 30.0, copy_y + 3.0)),
                    Some((panel.x1 - copy_x - 44.0) as f32),
                    TextWeight::Semibold,
                );
            }
            let title_y = copy_y + if private { 40.0 } else { 0.0 };
            text_renderer.draw_text_weighted(
                scene,
                if self.people_merge_mode {
                    "Combine people"
                } else {
                    &title
                },
                24.0,
                theme.text_primary,
                transform * Affine::translate((copy_x, title_y)),
                Some((panel.x1 - copy_x - 20.0) as f32),
                TextWeight::Semibold,
            );
            text_renderer.draw_text_weighted(
                scene,
                if self.people_merge_mode {
                    "Choose another person or suggestion to combine with this one. Nothing is shared until you approve a named suggestion."
                } else if hidden {
                    "This suggestion is hidden on this device. You can return it to the review queue."
                } else {
                    &subtitle
                },
                13.0,
                theme.text_dim,
                transform * Affine::translate((copy_x, title_y + 36.0)),
                Some((panel.x1 - copy_x - 20.0) as f32),
                TextWeight::Regular,
            );
            if !self.people_merge_mode && !hidden {
                text_renderer.draw_text_weighted(
                    scene,
                    if private {
                        "Name this person"
                    } else {
                        "Display name"
                    },
                    12.0,
                    theme.text_dim,
                    transform * Affine::translate((copy_x, title_y + 82.0)),
                    Some((panel.x1 - copy_x - 20.0) as f32),
                    TextWeight::Medium,
                );
                if wide {
                    text_renderer.draw_text_weighted(
                        scene,
                        if private {
                            "Approving adds only this name and these photo memberships to the shared library."
                        } else {
                            "This name and its photo memberships are shared with your collaborators."
                        },
                        12.0,
                        theme.text_dim,
                        transform * Affine::translate((copy_x, panel.y1 - 42.0)),
                        Some((panel.x1 - copy_x - 20.0) as f32),
                        TextWeight::Regular,
                    );
                }
            }
            scene.pop_layer();
            return;
        }

        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.window_card,
            None,
            &RoundedRect::from_rect(recognition, 12.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(recognition, 12.0),
        );
        let shield = Rect::new(
            recognition.x0 + 16.0,
            recognition.y0 + 18.0,
            recognition.x0 + 52.0,
            recognition.y0 + 54.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(shield, 10.0),
        );
        draw_icon(
            scene,
            transform,
            shield.inflate(-9.0, -9.0),
            Icon::Shield,
            theme.text_primary,
            1.4,
        );
        let recognition_copy_x = recognition.x0 + 64.0;
        text_renderer.draw_text_weighted(
            scene,
            "On-device recognition",
            15.0,
            theme.text_primary,
            transform * Affine::translate((recognition_copy_x, recognition.y0 + 16.0)),
            Some(if wide {
                300.0
            } else {
                (recognition.width() - 80.0) as f32
            }),
            TextWeight::Semibold,
        );
        let scan_state = match self.people_scan_progress.status.as_str() {
            "running" => "Scanning locally. Suggestions will appear here for review.",
            "error" => self
                .people_scan_progress
                .last_error
                .as_deref()
                .unwrap_or("The last local scan could not finish."),
            "cancelled" => "Scanning is paused. Start again to continue from the next local batch.",
            _ => self.people_controller.capability().summary(),
        };
        text_renderer.draw_text_weighted(
            scene,
            scan_state,
            12.0,
            theme.text_dim,
            transform * Affine::translate((recognition_copy_x, recognition.y0 + 42.0)),
            Some(if wide {
                (recognition.width() - 450.0).max(220.0) as f32
            } else {
                (recognition.width() - 80.0) as f32
            }),
            TextWeight::Regular,
        );
        let body_y = recognition.y1 + 30.0;
        let people_lane_w = if wide {
            (content.width() * 0.67).max(420.0)
        } else {
            content.width()
        };
        let review_x = if wide {
            content.x0 + people_lane_w + 20.0
        } else {
            content.x0
        };
        let review_w = if wide {
            content.x1 - review_x
        } else {
            content.width()
        };
        text_renderer.draw_text_weighted(
            scene,
            "People you’ve named",
            18.0,
            theme.text_primary,
            transform * Affine::translate((content.x0, body_y)),
            Some(people_lane_w as f32),
            TextWeight::Semibold,
        );
        if wide {
            text_renderer.draw_text_weighted(
                scene,
                "Review queue",
                18.0,
                theme.text_primary,
                transform * Affine::translate((review_x, body_y)),
                Some(review_w as f32),
                TextWeight::Semibold,
            );
        }
        let approved_controls: Vec<_> = self
            .people_controls
            .iter()
            .filter(|control| matches!(control.intent, PeopleIntent::OpenApproved(_)))
            .collect();
        if approved_controls.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                "People you approve will be shared here as searchable collections.",
                13.0,
                theme.text_dim,
                transform * Affine::translate((content.x0, body_y + 34.0)),
                Some(people_lane_w as f32),
                TextWeight::Regular,
            );
        }
        for control in approved_controls {
            let PeopleIntent::OpenApproved(name) = &control.intent else {
                continue;
            };
            let person = self
                .people_approved
                .iter()
                .find(|person| &person.label == name);
            let count = person.map_or(0, crate::people::SharedPerson::count);
            let photo_ids = person.map_or_else(Vec::new, |person| person.photo_ids.clone());
            let card = if wide {
                Rect::new(
                    control.bounds.x0 - 12.0,
                    control.bounds.y0 - 176.0,
                    control.bounds.x1 + 12.0,
                    control.bounds.y1 + 8.0,
                )
            } else {
                Rect::new(
                    content.x0,
                    control.bounds.y0 - 20.0,
                    content.x1,
                    control.bounds.y1 + 20.0,
                )
            };
            self.draw_people_card(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                card,
                name,
                &format!(
                    "{count} photograph{} · shared",
                    if count == 1 { "" } else { "s" }
                ),
                &photo_ids,
                false,
                !wide,
            );
        }
        if !wide {
            let approved_count = self
                .people_controls
                .iter()
                .filter(|control| matches!(control.intent, PeopleIntent::OpenApproved(_)))
                .count();
            text_renderer.draw_text_weighted(
                scene,
                "Suggestions to review",
                18.0,
                theme.text_primary,
                transform
                    * Affine::translate((content.x0, body_y + 42.0 + approved_count as f64 * 92.0)),
                Some(content.width() as f32),
                TextWeight::Semibold,
            );
        }
        let suggestion_controls: Vec<_> = self
            .people_controls
            .iter()
            .filter(|control| matches!(control.intent, PeopleIntent::OpenSuggestion(_)))
            .collect();
        if suggestion_controls.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                match self.people_controller.capability() {
                    crate::people::FaceCapability::DisabledByOwner => "Turn on recognition when you want private suggestions from this device.",
                    crate::people::FaceCapability::NoVerifiedFaceModel => "Recognition is on, but this device does not have a verified local face model yet.",
                    crate::people::FaceCapability::Ready { .. } => "No suggestions are waiting for review.",
                },
                13.0,
                theme.text_dim,
                transform * Affine::translate((review_x, body_y + 34.0)),
                Some(review_w as f32),
                TextWeight::Regular,
            );
        }
        for control in suggestion_controls {
            let PeopleIntent::OpenSuggestion(id) = &control.intent else {
                continue;
            };
            let Some(cluster) = self.people_controller.cluster(id) else {
                continue;
            };
            let card = if wide {
                Rect::new(
                    review_x,
                    control.bounds.y0 - 92.0,
                    content.x1,
                    control.bounds.y1 + 16.0,
                )
            } else {
                Rect::new(
                    content.x0,
                    control.bounds.y0 - 20.0,
                    content.x1,
                    control.bounds.y1 + 20.0,
                )
            };
            self.draw_people_card(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                card,
                cluster
                    .display_name
                    .as_deref()
                    .unwrap_or("Suggested person"),
                &format!(
                    "{} photograph{} · {}",
                    cluster.photos.len(),
                    if cluster.photos.len() == 1 { "" } else { "s" },
                    cluster.origin.label()
                ),
                &cluster.visible_photo_ids(),
                true,
                true,
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the library-analysis surface keeps progress, controls, failure paging and hit geometry together"
    )]
    fn draw_library_analysis(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        self.places_hits.clear();
        self.library_analysis_failure_hits.clear();

        let compact = self.canvas_rect.width() < 700.0 || theme.text_scale > 1.25;
        let inset = if compact { 16.0 } else { 30.0 };
        let content = Rect::new(
            self.canvas_rect.x0 + inset,
            self.canvas_rect.y0 + 16.0,
            self.canvas_rect.x1 - inset,
            self.canvas_rect.y1 - 16.0,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Photo analysis",
            if compact { 23.0 } else { 28.0 },
            theme.text_primary,
            transform * Affine::translate((content.x0, content.y0)),
            Some(content.width() as f32),
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Search inside your photographs without sending pixels away.",
            14.0,
            theme.text_dim,
            transform * Affine::translate((content.x0, content.y0 + 36.0)),
            Some(content.width() as f32),
            TextWeight::Regular,
        );
        self.draw_library_navigation(
            scene,
            transform,
            text_renderer,
            theme,
            Rect::new(
                content.x0,
                content.y0 + 58.0,
                if compact {
                    content.x1
                } else {
                    (content.x0 + 450.0).min(content.x1)
                },
                content.y0 + 98.0,
            ),
        );

        let card_y = content.y0 + 116.0;
        let card_h = if compact { 174.0 } else { 142.0 };
        let status_card = RoundedRect::from_rect(
            Rect::new(content.x0, card_y, content.x1, card_y + card_h),
            12.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.window_card,
            None,
            &status_card,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &status_card,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.accent[6],
            None,
            &RoundedRect::from_rect(
                Rect::new(
                    content.x0,
                    card_y + 10.0,
                    content.x0 + 3.0,
                    card_y + card_h - 10.0,
                ),
                2.0,
            ),
        );

        let (model_title, model_detail) = self.library_analysis_capability.as_ref().map_or_else(
            || {
                (
                    "Local model unavailable".to_owned(),
                    "Install and start a verified local vision model in Settings.".to_owned(),
                )
            },
            |capability| {
                let revision = capability.revision.chars().take(10).collect::<String>();
                (
                    capability.model_label.clone(),
                    format!("Verified local model · revision {revision}"),
                )
            },
        );
        text_renderer.draw_text_weighted(
            scene,
            &model_title,
            17.0,
            theme.text_primary,
            transform * Affine::translate((content.x0 + 18.0, card_y + 16.0)),
            Some(if compact {
                (content.width() - 36.0) as f32
            } else {
                (content.width() * 0.42) as f32
            }),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            &model_detail,
            12.0,
            theme.text_dim,
            transform * Affine::translate((content.x0 + 18.0, card_y + 42.0)),
            Some(if compact {
                (content.width() - 36.0) as f32
            } else {
                (content.width() * 0.42) as f32
            }),
            TextWeight::Regular,
        );

        let phase = self.library_analysis.phase;
        let phase_label = match phase {
            crate::library_analysis::LibraryAnalysisPhase::Idle => "Ready",
            crate::library_analysis::LibraryAnalysisPhase::Running => "Analyzing locally",
            crate::library_analysis::LibraryAnalysisPhase::Paused => "Paused",
            crate::library_analysis::LibraryAnalysisPhase::Complete => "Finished",
        };
        let (analyzed, available, awaiting) = self.library_analysis_coverage;
        let summary = if phase == crate::library_analysis::LibraryAnalysisPhase::Idle {
            format!(
                "{analyzed} searchable · {available} ready to analyze · {awaiting} awaiting local bytes"
            )
        } else {
            format!(
                "{} of {} finished · {} succeeded · {} failed",
                self.library_analysis.completed,
                self.library_analysis.total,
                self.library_analysis.succeeded,
                self.library_analysis.failed
            )
        };
        text_renderer.draw_text_weighted(
            scene,
            phase_label,
            13.0,
            theme.text_primary,
            transform * Affine::translate((content.x0 + 18.0, card_y + 72.0)),
            Some(180.0),
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            &summary,
            12.0,
            theme.text_dim,
            transform * Affine::translate((content.x0 + 18.0, card_y + 94.0)),
            Some((content.width() - 36.0) as f32),
            TextWeight::Regular,
        );
        if !compact {
            for (index, (value, label)) in [
                (analyzed, "Searchable"),
                (available, "Ready"),
                (awaiting, "Waiting"),
            ]
            .into_iter()
            .enumerate()
            {
                let x = content.x0 + content.width() * (0.49 + index as f64 * 0.17);
                text_renderer.draw_text_weighted(
                    scene,
                    &value.to_string(),
                    24.0,
                    theme.text_primary,
                    transform * Affine::translate((x, card_y + 34.0)),
                    Some((content.width() * 0.13) as f32),
                    TextWeight::Semibold,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    11.0,
                    theme.text_dim,
                    transform * Affine::translate((x, card_y + 68.0)),
                    Some((content.width() * 0.13) as f32),
                    TextWeight::Medium,
                );
            }
        }
        self.library_analysis_progress
            .set_progress(self.library_analysis.progress() as f32);
        let progress_y = card_y + if compact { 124.0 } else { 116.0 };
        self.library_analysis_progress
            .apply_layout_external(Rect::new(
                content.x0 + 18.0,
                progress_y,
                content.x1 - 18.0,
                progress_y + 8.0,
            ));
        self.library_analysis_progress
            .draw(scene, transform, text_renderer, theme, presence);
        if let Some(name) = self.library_analysis.current_name.as_deref() {
            text_renderer.draw_text_weighted(
                scene,
                &format!("Now: {name}"),
                11.0,
                theme.text_dim,
                transform * Affine::translate((content.x0 + 18.0, progress_y + 17.0)),
                Some((content.width() - 36.0) as f32),
                TextWeight::Regular,
            );
        }

        let controls_y = card_y + card_h + 14.0;
        let visible_hit = match phase {
            crate::library_analysis::LibraryAnalysisPhase::Running => LibraryAnalysisHit::Pause,
            crate::library_analysis::LibraryAnalysisPhase::Paused => LibraryAnalysisHit::Resume,
            _ => LibraryAnalysisHit::Start,
        };
        let primary_w = if compact { content.width() } else { 250.0 };
        for (hit, button) in &mut self.library_analysis_buttons {
            let visible = *hit == visible_hit
                || (*hit == LibraryAnalysisHit::RetryFailures
                    && self.library_analysis.has_failures()
                    && phase != crate::library_analysis::LibraryAnalysisPhase::Running);
            button.set_enabled(match *hit {
                LibraryAnalysisHit::Start => {
                    self.library_analysis_capability.is_some() && available > 0
                }
                LibraryAnalysisHit::Pause => {
                    phase == crate::library_analysis::LibraryAnalysisPhase::Running
                }
                LibraryAnalysisHit::Resume => {
                    phase == crate::library_analysis::LibraryAnalysisPhase::Paused
                }
                LibraryAnalysisHit::RetryFailures => self.library_analysis.has_failures(),
            });
            let x = if *hit == LibraryAnalysisHit::RetryFailures {
                content.x0 + primary_w + 10.0
            } else {
                content.x0
            };
            let width = if compact { content.width() } else { 250.0 };
            let y = if compact && *hit == LibraryAnalysisHit::RetryFailures {
                controls_y + 54.0
            } else {
                controls_y
            };
            if visible {
                button.apply_layout_external(Rect::new(x, y, x + width, y + 44.0));
                button.draw(scene, transform, text_renderer, theme, presence);
            } else {
                button.apply_layout_external(Rect::ZERO);
            }
        }

        let info_y = controls_y
            + if compact && self.library_analysis.has_failures() {
                116.0
            } else {
                66.0
            };
        text_renderer.draw_text_weighted(
            scene,
            "What becomes searchable",
            17.0,
            theme.text_primary,
            transform * Affine::translate((content.x0, info_y)),
            Some(content.width() as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Descriptions, visible text, objects, and pets. Original pixels stay on this device. The compact result and its model provenance are shared with this photo library so collaborators and system search see the same truth.",
            13.0,
            theme.text_dim,
            transform * Affine::translate((content.x0, info_y + 28.0)),
            Some(content.width() as f32),
            TextWeight::Regular,
        );
        let capability_cards_y = info_y + if compact { 92.0 } else { 66.0 };
        let capability_columns = if compact { 2 } else { 4 };
        let capability_gap = 10.0;
        let capability_w = (content.width() - capability_gap * (capability_columns - 1) as f64)
            / capability_columns as f64;
        for (index, (title, detail, icon)) in [
            ("Visible text", "Full-text OCR", Icon::Search),
            ("Scenes", "Descriptions + objects", Icon::Sparkles),
            ("Pets", "Separate pet facets", Icon::Star),
            ("People", "Private review in People", Icon::Users),
        ]
        .into_iter()
        .enumerate()
        {
            let column = index % capability_columns;
            let row = index / capability_columns;
            let x = content.x0 + column as f64 * (capability_w + capability_gap);
            let y = capability_cards_y + row as f64 * 70.0;
            let card = RoundedRect::from_rect(Rect::new(x, y, x + capability_w, y + 60.0), 9.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.window_card,
                None,
                &card,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &card,
            );
            draw_icon(
                scene,
                transform,
                Rect::new(x + 14.0, y + 20.0, x + 32.0, y + 38.0),
                icon,
                theme.tokens.colors.accent[6],
                1.5,
            );
            text_renderer.draw_text_weighted(
                scene,
                title,
                13.0,
                theme.text_primary,
                transform * Affine::translate((x + 48.0, y + 10.0)),
                Some((capability_w - 60.0) as f32),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                detail,
                11.0,
                theme.text_dim,
                transform * Affine::translate((x + 48.0, y + 33.0)),
                Some((capability_w - 60.0) as f32),
                TextWeight::Regular,
            );
        }

        let capability_rows = 4usize.div_ceil(capability_columns);
        let failure_y = capability_cards_y + capability_rows as f64 * 70.0 + 12.0;
        if self.library_analysis.has_failures() {
            text_renderer.draw_text_weighted(
                scene,
                "Needs attention",
                17.0,
                theme.text_primary,
                transform * Affine::translate((content.x0, failure_y)),
                Some(content.width() as f32),
                TextWeight::Medium,
            );
            for (index, failure) in self.library_analysis.failures().rev().take(4).enumerate() {
                let y = failure_y + 30.0 + index as f64 * 46.0;
                let row = Rect::new(content.x0, y, content.x1, y + 40.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &row,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.semantic.danger,
                    None,
                    &Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &failure.name,
                    13.0,
                    theme.text_primary,
                    transform * Affine::translate((row.x0 + 12.0, row.y0 + 6.0)),
                    Some((row.width() * 0.38) as f32),
                    TextWeight::Medium,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &failure.message,
                    12.0,
                    theme.text_dim,
                    transform * Affine::translate((row.x0 + row.width() * 0.42, row.y0 + 7.0)),
                    Some((row.width() * 0.55) as f32),
                    TextWeight::Regular,
                );
                self.library_analysis_failure_hits
                    .push((failure.photo_id.clone(), row));
            }
        }
    }

    /// Render the visible part of the library; the remainder stays reachable
    /// through cursor-paged search and canonical resources.
    #[expect(
        clippy::too_many_lines,
        reason = "the library paint keeps masthead, grid/timeline geometry and bounded materialisation together"
    )]
    fn draw_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        let chip_radius = f64::from(theme.tokens.radii.sm);
        self.library_cards.clear();
        self.album_cards.clear();
        self.metadata_filter_cards.clear();
        self.library_page_hits.clear();
        self.timeline_density_hits.clear();
        self.library_details_cards.clear();
        self.places_hits.clear();
        self.collection_page_hits.clear();
        if self.library_items.is_empty() {
            let (title, body) = if !self.library_query.trim().is_empty()
                && !self.library_query_complete
            {
                (
                    "Searching photographs…".to_owned(),
                    "Matching names, people, places, recognized text, and image descriptions locally."
                        .to_owned(),
                )
            } else if self.library_query.trim().is_empty() {
                (
                    tr("photos-library-empty-title"),
                    tr("photos-library-empty-body"),
                )
            } else {
                (
                    "No matching photographs".to_owned(),
                    "Try a different name, person, place, tag, or text phrase.".to_owned(),
                )
            };
            self.draw_empty_state(scene, transform, text_renderer, theme, &title, &body);
            // Empty collections are useful before the first import too; keep
            // the same New album affordance available rather than stranding
            // the user behind an empty-state dead end.
            self.draw_album_editor(scene, transform, text_renderer, theme, presence);
            return;
        }
        let mut bounds = self.canvas_rect;
        // On a desktop the details sheet is a persistent side panel, not a
        // transient popover. Give it an actual layout lane so filters, cards,
        // and destructive controls never continue underneath an opaque sheet
        // and emerge as clipped half-targets at its edge.
        if self.metadata_editor && theme.text_scale <= 1.25 && bounds.width() >= 720.0 {
            bounds.x1 = (bounds.x1 - 560.0).max(bounds.x0);
        }
        // Enlarged text needs its own density rule. Five desktop cards leave
        // only a few words of metadata per card at 200%, which makes labels
        // overlap their independent destructive controls.
        let large_text = theme.text_scale > 1.25;
        let columns = timeline_column_count(bounds.width(), self.timeline_density, large_text);
        let gap = if columns <= 2 { 8.0 } else { 16.0 };
        let card_w =
            ((bounds.width() - f64::from(PAD * 2.0) - gap * columns.saturating_sub(1) as f64)
                / columns as f64)
                .max(1.0);
        let desktop_library = !large_text && bounds.width() >= LIBRARY_DESKTOP_MASTHEAD_MIN_W;
        // Compact library navigation is photo-first. Collections remain
        // canonical/searchable, while their dense chip rail moves out of the
        // small viewport instead of pushing the first date below the fold.
        let album_h = if desktop_library {
            // Album creation/editing shares this secondary collection rail;
            // reserving it even for an empty library keeps those controls out
            // of the primary destination masthead and avoids first-use layout
            // shifts when the first album appears.
            52.0
        } else {
            0.0
        };
        let compact_header = compact_library_header_geometry(bounds, large_text, album_h > 0.0);
        let masthead = Rect::new(
            bounds.x0 + f64::from(PAD),
            bounds.y0 + 8.0,
            bounds.x1 - f64::from(PAD),
            bounds.y0 + 64.0,
        );
        // The library header is a quiet reading order, not a boxed dashboard.
        // Photographs start immediately below it and remain the visual anchor.
        let heading_y = if desktop_library {
            masthead.y0 + 15.0
        } else {
            compact_header.title_y
        };
        text_renderer.draw_text_weighted(
            scene,
            &tr("photos-library"),
            if desktop_library { 24.0 } else { 18.0 },
            theme.text_primary,
            transform
                * Affine::translate((
                    if desktop_library {
                        masthead.x0 + 16.0
                    } else {
                        bounds.x0 + f64::from(PAD)
                    },
                    heading_y,
                )),
            Some((bounds.width() - f64::from(PAD * 2.0)) as f32),
            TextWeight::Medium,
        );
        // First-class destinations use the same stable local navigation
        // target rather than turning People/Places into a search suggestion.
        if desktop_library {
            self.draw_library_navigation(
                scene,
                transform,
                text_renderer,
                theme,
                Rect::new(
                    masthead.x0 + 224.0,
                    masthead.y0 + 12.0,
                    masthead.x1 - 224.0,
                    masthead.y0 + 48.0,
                ),
            );
        }
        let count = if !self.library_query.trim().is_empty() && !self.library_query_complete {
            format!("{} matches so far · searching locally", self.library_total)
        } else if desktop_library {
            format!("{} photographs · shared library", self.library_total)
        } else {
            format!("{} photos", self.library_total)
        };
        let count_width = if desktop_library { 260.0 } else { 180.0 };
        text_renderer.draw_text_weighted(
            scene,
            &count,
            13.0,
            theme.text_dim,
            transform
                * Affine::translate((
                    if desktop_library {
                        masthead.x0 + 16.0
                    } else {
                        bounds.x0 + f64::from(PAD)
                    },
                    if desktop_library {
                        heading_y + 32.0
                    } else {
                        compact_header.count_y
                    },
                )),
            Some(count_width as f32),
            TextWeight::Regular,
        );
        // Density is a first-class browsing affordance, not a hidden gesture.
        // The two small controls are intentionally next to the count so they
        // remain discoverable on a touch display and at 200% text.
        let density_y = if desktop_library {
            masthead.y0 + 16.0
        } else {
            compact_header.controls.y0
        };
        let density_x = if desktop_library {
            bounds.x1 - f64::from(PAD) - 96.0
        } else if large_text {
            compact_header.controls.x1 + 8.0
        } else {
            // Keep the two density controls immediately before Select on a
            // compact masthead. They previously occupied the exact same rect
            // and painted a second control underneath it.
            compact_header.controls.x0 - 104.0
        };
        for (hit, rect, enabled) in [
            (
                TimelineHit::ZoomOut,
                Rect::new(density_x, density_y, density_x + 44.0, density_y + 44.0),
                self.timeline_density > 0,
            ),
            (
                TimelineHit::ZoomIn,
                Rect::new(
                    density_x + 52.0,
                    density_y,
                    density_x + 96.0,
                    density_y + 44.0,
                ),
                self.timeline_density < 4,
            ),
        ] {
            if let Some((_, button)) = self
                .timeline_density_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, theme, presence);
            }
            if enabled {
                self.timeline_density_hits.push((hit, rect));
            }
        }
        if desktop_library && !self.library_albums.is_empty() {
            let mut x = bounds.x0 + f64::from(PAD) + if desktop_library { 60.0 } else { 0.0 };
            let y = if desktop_library {
                masthead.y1 + 13.0
            } else {
                compact_header.albums_y
            };
            if desktop_library {
                text_renderer.draw_text_weighted(
                    scene,
                    "ALBUMS",
                    10.0,
                    theme.text_dim,
                    transform * Affine::translate((bounds.x0 + f64::from(PAD), y + 7.0)),
                    Some(54.0),
                    TextWeight::Medium,
                );
            }
            let mut albums = vec![(
                None,
                if large_text {
                    "All".to_owned()
                } else {
                    format!("All photos ({})", self.library_total)
                },
            )];
            albums.extend(
                self.library_albums
                    .iter()
                    .skip(self.albums_page * COLLECTION_PAGE_SIZE)
                    .take(COLLECTION_PAGE_SIZE)
                    .map(|(id, name, count)| {
                        (
                            Some(id.clone()),
                            if large_text {
                                compact_label(name, 12)
                            } else {
                                format!("{name} ({count})")
                            },
                        )
                    }),
            );
            let album_pages = self
                .library_albums
                .len()
                .max(1)
                .div_ceil(COLLECTION_PAGE_SIZE);
            let album_chip_limit =
                bounds.x1 - f64::from(PAD) - 232.0 - if album_pages > 1 { 308.0 } else { 0.0 };
            for (album_id, label) in albums {
                let width = if large_text {
                    (label.chars().count() as f64 * 13.0 + 22.0).clamp(64.0, 190.0)
                } else {
                    (label.chars().count() as f64 * 7.2 + 18.0).min(150.0)
                };
                if x + width > album_chip_limit {
                    break;
                }
                let chip = Rect::new(x, y, x + width, y + if large_text { 44.0 } else { 26.0 });
                let selected = album_id == self.active_album;
                // A quiet chip is a small card: raised fill plus derived edge;
                // the album in force carries the accent (HIG §9.4/§9.5 "Chip
                // quiet: selected accent, no edge") with ink chosen against it.
                let ink = if selected {
                    let fill = theme.tokens.colors.accent[6];
                    surface::pill(scene, transform, chip, fill, None, Some(chip_radius));
                    theme.on_surface(fill).ink
                } else {
                    surface::card(
                        scene,
                        transform,
                        chip,
                        chip_radius,
                        theme.tokens.colors.surfaces.raised,
                    );
                    theme.text_primary
                };
                text_renderer.draw_text_weighted(
                    scene,
                    &label,
                    12.0,
                    ink,
                    transform * Affine::translate((chip.x0 + 8.0, chip.y0 + 6.0)),
                    Some((width - 16.0) as f32),
                    TextWeight::Regular,
                );
                self.album_cards.push((album_id, chip));
                x += width + LIB_CHIP_GAP;
            }
            self.draw_collection_pager(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                Rect::new(
                    bounds.x1 - f64::from(PAD) - 292.0,
                    y - 7.0,
                    bounds.x1 - f64::from(PAD),
                    y + 33.0,
                ),
                self.albums_page,
                self.library_albums.len(),
                CollectionPageHit::AlbumsPrevious,
                CollectionPageHit::AlbumsNext,
            );
        }
        // Metadata filters are deliberately in the shelf rather than hidden
        // behind global search: folders and people are an everyday browse
        // route.  The six-per-kind cap keeps this compact; every other label
        // remains available through the canonical resource/search surface.
        let header_h = if desktop_library {
            56.0
        } else {
            (compact_header.timeline_y - heading_y - album_h).max(0.0)
        };
        let filter_y = heading_y + header_h + album_h;
        let mut filter_x = bounds.x0 + f64::from(PAD) + if desktop_library { 60.0 } else { 0.0 };
        // Paging remains for the album editor's bounded membership draft;
        // the normal timeline itself scrolls continuously by chronological row.
        let desktop_pager =
            self.album_editor && desktop_library && self.library_total > columns.saturating_mul(2);
        let filter_x1 = bounds.x1 - f64::from(PAD) - if desktop_pager { 192.0 } else { 0.0 };
        let compact_filters = columns <= 2;
        let mut filters = vec![(
            None,
            None,
            None,
            false,
            if compact_filters {
                "All".to_owned()
            } else {
                "All metadata".to_owned()
            },
        )];
        if large_text {
            // Search and collections are the compact navigation model at
            // 200%; do not add a third row of metadata chips.
        } else if compact_filters {
            // A phone has one shelf row. Show one folder *and* one explicit
            // people label rather than filling it with folders and hiding the
            // privacy-controlled people route.
            if let Some((folder, count)) = self.library_folders.first() {
                filters.push((
                    Some(folder.clone()),
                    None,
                    None,
                    false,
                    format!("{} ({count})", compact_label(folder, 8)),
                ));
            }
            if let Some((person, count)) = self.library_people.first() {
                filters.push((
                    None,
                    Some(person.clone()),
                    None,
                    false,
                    format!("{} ({count})", compact_label(person, 8)),
                ));
            }
        }
        if self.library_favorite_count > 0 {
            filters.insert(
                1,
                (
                    None,
                    None,
                    None,
                    true,
                    format!("Favorites ({})", self.library_favorite_count),
                ),
            );
        }
        for (folder, person, tag, favorites, label) in filters {
            // On desktop, collections live in the album rail and the date
            // rhythm does the browsing work.  This avoids a second dashboard
            // row of metadata chips above every photograph.
            if desktop_library || large_text || compact_filters {
                break;
            }
            let width = if large_text {
                (label.chars().count() as f64 * 13.0 + 22.0).clamp(64.0, 200.0)
            } else {
                (label.chars().count() as f64 * 6.6 + 18.0).clamp(
                    if compact_filters { 54.0 } else { 92.0 },
                    if compact_filters { 110.0 } else { 196.0 },
                )
            };
            if filter_x + width > filter_x1 {
                break;
            }
            let chip_h = if large_text { 44.0 } else { 28.0 };
            let chip = Rect::new(filter_x, filter_y, filter_x + width, filter_y + chip_h);
            let selected = folder.as_ref() == self.active_folder.as_ref()
                && person.as_ref() == self.active_person.as_ref()
                && tag.as_ref() == self.active_tag.as_ref()
                && self.active_favorites == favorites
                && self.active_album.is_none();
            if selected {
                // Accent means live (HIG §9.4): the filter in force carries
                // the accent fill and needs no edge against it.
                surface::pill(
                    scene,
                    transform,
                    chip,
                    theme.tokens.colors.accent[6],
                    None,
                    Some(chip_radius),
                );
            } else {
                surface::card(
                    scene,
                    transform,
                    chip,
                    chip_radius,
                    theme.tokens.colors.surfaces.raised,
                );
            }
            text_renderer.draw_text_weighted(
                scene,
                &label,
                12.0,
                theme.text_primary,
                transform
                    * Affine::translate((
                        chip.x0 + 8.0,
                        chip.y0 + if large_text { 10.0 } else { 7.0 },
                    )),
                Some((width - 16.0) as f32),
                TextWeight::Medium,
            );
            self.metadata_filter_cards.push(MetadataFilterCard {
                folder,
                person,
                tag,
                favorites,
                bounds: chip,
            });
            filter_x += width + 8.0;
        }
        if desktop_pager {
            let previous = Rect::new(
                filter_x1 + 8.0,
                filter_y - 8.0,
                filter_x1 + 92.0,
                filter_y + 36.0,
            );
            let next = Rect::new(
                previous.x1 + 8.0,
                filter_y - 8.0,
                previous.x1 + 92.0,
                filter_y + 36.0,
            );
            for (forward, rect, label) in [(false, previous, "Previous"), (true, next, "Next")] {
                let enabled = self.can_page_library(forward);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if enabled {
                        theme.tokens.colors.surfaces.raised
                    } else {
                        theme.tokens.colors.neutral[2]
                    },
                    None,
                    &rect,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.border_standard,
                    None,
                    &rect,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    11.0,
                    if enabled {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    },
                    transform * Affine::translate((rect.x0 + 9.0, rect.y0 + 13.0)),
                    Some((rect.width() - 18.0) as f32),
                    TextWeight::Medium,
                );
                self.library_page_hits.push((forward, enabled, rect));
            }
        }
        // Compact album actions now occupy the header's title lane (and the
        // enlarged-text variant has its own measured row). Do not preserve
        // the old beneath-the-chips reservation: it was an invisible 54px
        // gutter that made a one-day timeline look prematurely finished.
        let compact_album_actions_h = 0.0;
        let filter_h = if desktop_library || large_text {
            0.0
        } else {
            34.0
        };
        let pager_h = if large_text
            && bounds.width() < 620.0
            && self.album_editor
            && self.library_total > 1
        {
            52.0
        } else {
            0.0
        };
        let pager_y = heading_y + header_h + album_h + filter_h + compact_album_actions_h;
        if pager_h > 0.0 {
            let previous = Rect::new(
                bounds.x0 + f64::from(PAD),
                pager_y + 4.0,
                bounds.x0 + f64::from(PAD) + 116.0,
                pager_y + 46.0,
            );
            let next = Rect::new(
                bounds.x1 - f64::from(PAD) - 88.0,
                pager_y + 4.0,
                bounds.x1 - f64::from(PAD),
                pager_y + 46.0,
            );
            for (forward, rect, label) in [(false, previous, "Previous"), (true, next, "Next")] {
                let enabled = self.can_page_library(forward);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if enabled {
                        theme.tokens.colors.surfaces.raised
                    } else {
                        theme.tokens.colors.neutral[2]
                    },
                    None,
                    &rect,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(rect.x0, rect.y1 - 1.0, rect.x1, rect.y1),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    12.0,
                    if enabled {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    },
                    transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 10.0)),
                    Some((rect.width() - 20.0) as f32),
                    TextWeight::Medium,
                );
                self.library_page_hits.push((forward, enabled, rect));
            }
            text_renderer.draw_text_weighted(
                scene,
                &format!("{} / {}", self.library_offset + 1, self.library_total),
                12.0,
                theme.text_dim,
                transform * Affine::translate((previous.x1 + 10.0, pager_y + 14.0)),
                Some((next.x0 - previous.x1 - 20.0).max(1.0) as f32),
                TextWeight::Medium,
            );
        }
        // Selection owns an explicit action shelf above the cards. On a
        // narrow or enlarged-text window it becomes two comfortable rows
        // instead of compressing five actions into an unreadable strip.
        let selection_h = if self.selection_mode {
            // Selection is a compact contextual task bar, not a second
            // dashboard. Narrow and 200% layouts keep the count, one safe
            // contextual action, and Done in the same 44px-target row.
            52.0
        } else {
            0.0
        };
        let selection_y = pager_y + pager_h;
        let selection_band =
            Rect::new(bounds.x0, selection_y, bounds.x1, selection_y + selection_h);
        self.draw_selection_controls(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            bounds,
            masthead,
            selection_band,
            desktop_library,
            compact_header.controls,
            presence,
        );
        let top = selection_y + selection_h;
        // The worker preserves source aspect ratio. A 3:2 shelf avoids the
        // oversized letterbox trays the old 16:10 geometry produced.
        let default_preview_h = (card_w * 0.667).clamp(
            76.0,
            if large_text && self.metadata_editor {
                // The sheet owns the right side; retain one complete, bounded
                // background card without making its action row fall below
                // the viewport.
                300.0
            } else if large_text {
                // At 200% a wide two-column card would otherwise derive a
                // ~470 px preview from its width and leave no complete row
                // below the enlarged browse controls. Keep a generous,
                // aspect-preserving tray while ensuring the actions remain
                // reachable in an ordinary 900 px desktop window.
                if bounds.width() < 620.0 { 220.0 } else { 320.0 }
            } else if columns == 1 {
                320.0
            } else if desktop_library {
                // Two complete desktop rows are more useful than one large
                // row above a blank canvas. The card remains comfortably
                // wide at this three-column breakpoint.
                168.0
            } else {
                178.0
            },
        );
        // The footer has two text lines and an honest 44px action row. Keep
        // it roomy instead of letting desktop density turn controls into
        // loose glyphs or make metadata collide with a filename.
        // Cards are photographs in every responsive mode.  A compact screen
        // opens a card directly or enters selection; it never trades half a
        // tile for a filename and three permanent action buttons.
        let viewport_height = (bounds.y1 - top - f64::from(PAD)).max(0.0);
        self.timeline_viewport_height = viewport_height;
        let timeline_left = bounds.x0 + f64::from(PAD);
        let timeline_width = bounds.x1 - f64::from(PAD) - 18.0 - timeline_left;
        // Header controls need their compact arrangement well before the
        // photographs need to collapse to a single column. Keeping those
        // breakpoints independent gives tablets and narrow desktop strips a
        // useful, expressive chronology instead of two enormous image rows.
        let single_column_timeline = timeline_width < 520.0;
        // At 200% text, retain two genuinely readable photographs rather
        // than either stretching one tile across the whole desktop or
        // squeezing a justified row into three narrow targets.
        let timeline_layout_width = if large_text && !single_column_timeline {
            timeline_width.min(1_216.0)
        } else {
            timeline_width
        };
        let timeline_target_height = if large_text && !single_column_timeline {
            410.0
        } else {
            default_preview_h
        };
        let anchor = self.timeline_projection.anchor_at(self.timeline_scroll_y);
        let geometry_changed = self.timeline_projection.set_geometry(TimelineGeometry {
            width: timeline_layout_width.max(44.0),
            target_height: timeline_target_height,
            gap,
            compact: single_column_timeline,
        });
        if geometry_changed
            && let Some(anchor) = anchor.as_ref()
            && let Some(restored) = self
                .timeline_projection
                .restore_anchor(anchor, viewport_height)
        {
            self.timeline_scroll_y = restored;
        }
        self.timeline_scroll_y = self
            .timeline_projection
            .clamp_scroll(self.timeline_scroll_y, viewport_height);
        self.timeline_index_metrics = ScrollMetrics::new(
            self.timeline_projection.total_height(),
            viewport_height,
            self.timeline_scroll_y,
        );
        self.timeline_index_markers = self
            .timeline_projection
            .date_markers(viewport_height)
            .into_iter()
            .map(|(day, position, major)| {
                DateIndexMarker::new(
                    day.clone(),
                    timeline_date_label(&day),
                    position,
                    if major {
                        DateIndexMarkerKind::Major
                    } else {
                        DateIndexMarkerKind::Minor
                    },
                )
            })
            .collect();
        self.timeline_index.set_rect(Rect::new(
            bounds.x1 - f64::from(PAD) - ui::widget::date_indexed_scrollbar::DEFAULT_TRACK_WIDTH,
            top + 4.0,
            bounds.x1 - f64::from(PAD),
            bounds.y1 - f64::from(PAD),
        ));
        let visible_days = self
            .timeline_projection
            .visible_days(self.timeline_scroll_y, viewport_height);
        // Visible geometry can move beyond the queued thumbnail window before
        // the next sync. Resolve identities from the full authored snapshot.
        let photo_images: HashMap<_, _> = self
            .observed_content
            .iter()
            .flat_map(|content| &content.library.photos)
            .map(|photo| (photo.id.as_str(), &photo.image))
            .collect();
        let visible_items: Vec<_> = if large_text && self.metadata_editor {
            Vec::new()
        } else {
            self.timeline_projection
                .visible_photos(self.timeline_scroll_y, viewport_height)
                .into_iter()
                .filter_map(|entry| {
                    let image = photo_images.get(entry.photo.id.as_str())?;
                    Some((
                        LibraryItem {
                            id: entry.photo.id.clone(),
                            thumbnail_key: thumbnail_key(image),
                            name: entry.photo.name.clone(),
                            favorite: entry.photo.favorite,
                            captured_at: entry.photo.day.clone(),
                        },
                        TimelineItemLayout {
                            width: entry.width,
                            height: entry.height,
                        },
                        timeline_left + entry.x,
                        top + entry.y,
                    ))
                })
                .collect()
        };
        // Clip the moving content to the measured canvas region so a fast
        // scroll can never paint over the search/header or status bar.
        let timeline_clip = Rect::new(bounds.x0, top, bounds.x1, bounds.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &timeline_clip,
        );
        for day in visible_days {
            let heading_y = top + day.y;
            let day_rule = Rect::new(
                timeline_left,
                heading_y + 24.0,
                bounds.x1 - f64::from(PAD) - 18.0,
                heading_y + 25.0,
            );
            text_renderer.draw_text_weighted(
                scene,
                &timeline_date_label(&day.key),
                if large_text { 15.0 } else { 13.0 },
                theme.text_primary,
                transform * Affine::translate((day_rule.x0, heading_y + 4.0)),
                Some(day_rule.width() as f32),
                TextWeight::Medium,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(day_rule.x0 + 148.0, day_rule.y0, day_rule.x1, day_rule.y1),
            );
        }
        for (item, item_layout, projected_x, projected_y) in &visible_items {
            let id = &item.id;
            let name = &item.name;
            let favorite = item.favorite;
            // The desktop timeline reserves no artificial 3:2 tray.  Its
            // width stays predictable for scrolling while every item's
            // height is derived from document-owned intrinsic dimensions.
            // This is stable before pixels hydrate, so decoded images never
            // reflow the chronology under a user's pointer.
            let card_w = item_layout.width;
            let item_preview_h = item_layout.height;
            let item_card_h = item_preview_h;
            // A 200% or phone canvas may have room for the beginning of only
            // one tall portrait. Paint that first card (the clip protects the
            // masthead) so a chronological view never degenerates into an
            // empty canvas plus a misleading continuation card.
            let x = *projected_x;
            let y = *projected_y;
            let card = Rect::new(x, y, x + card_w, y + item_card_h);
            let preview = Rect::new(x, y, x + card_w, y + item_preview_h);
            let card_fully_visible = card.y0 >= top && card.y1 <= bounds.y1;
            scene.fill(
                Fill::NonZero,
                transform,
                if card.contains(self.pointer) {
                    theme.tokens.colors.neutral[3]
                } else {
                    theme.tokens.colors.surfaces.window_card
                },
                None,
                &card,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.neutral[2],
                None,
                &preview,
            );
            if let Some(image) = self.library_thumbnails.get(&item.thumbnail_key) {
                let scale = (preview.width() / f64::from(image.width))
                    .min(preview.height() / f64::from(image.height));
                let drawn_w = f64::from(image.width) * scale;
                let drawn_h = f64::from(image.height) * scale;
                let image_transform = transform
                    * Affine::translate((
                        preview.x0 + (preview.width() - drawn_w) * 0.5,
                        preview.y0 + (preview.height() - drawn_h) * 0.5,
                    ))
                    * Affine::scale(scale);
                scene.draw_image(image, image_transform);
            } else {
                // Hydration is real asynchronous work. A reserved-aspect
                // skeleton tells the difference between "arriving" and an
                // unavailable source without fabricating a thumbnail or
                // covering the gallery in a repeated generic image glyph.
                let unavailable = self.thumbnail_unavailable.contains(&item.thumbnail_key);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if unavailable {
                        theme.tokens.colors.neutral[3]
                    } else {
                        theme.tokens.colors.neutral[2]
                    },
                    None,
                    &preview,
                );
                let line_w = (preview.width() * 0.34).clamp(36.0, 112.0);
                let line = Rect::new(
                    preview.center().x - line_w * 0.5,
                    preview.center().y - 2.0,
                    preview.center().x + line_w * 0.5,
                    preview.center().y + 2.0,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if unavailable {
                        theme.text_dim
                    } else {
                        theme.border_standard
                    },
                    None,
                    &line,
                );
                if unavailable {
                    let icon_rect = Rect::from_center_size(
                        Point::new(preview.center().x, preview.center().y - 22.0),
                        (18.0, 18.0),
                    );
                    draw_icon(
                        scene,
                        transform,
                        icon_rect,
                        Icon::Image,
                        theme.text_dim,
                        1.5,
                    );
                }
            }
            let actions = card_action_controls(card);
            let action_y = actions.details.y0;
            let control_w = CARD_ACTION_CONTROL_W;
            let details = actions.details;
            let remove = actions.remove;
            let star = actions.favorite;
            // Both text rows are explicitly constrained to this card. The
            // action row owns the bottom 44px, so no title can collide with a
            // neighboring column or a control.
            let display_name = compact_label(
                name,
                ((card_w - 18.0) / if large_text { 16.0 } else { 8.0 })
                    .floor()
                    .max(8.0) as usize,
            );
            if card_fully_visible && desktop_library && !self.album_editor && !self.selection_mode {
                if card.contains(self.pointer) {
                    // Desktop controls are deliberately transient: the
                    // photograph remains the dominant surface until someone
                    // intends to act on it.
                    if desktop_library {
                        let overlay = Rect::new(card.x0, action_y, card.x1, card.y1);
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            theme.tokens.colors.neutral[0].with_alpha(0.78),
                            None,
                            &overlay,
                        );
                        text_renderer.draw_text_weighted(
                            scene,
                            &display_name,
                            12.0,
                            theme.text_primary,
                            transform * Affine::translate((card.x0 + 10.0, action_y + 16.0)),
                            Some((card.width() - control_w * 3.0 - 18.0).max(1.0) as f32),
                            TextWeight::Medium,
                        );
                    }
                    for (target, active) in [
                        (details, details.contains(self.pointer)),
                        (remove, remove.contains(self.pointer)),
                        (star, star.contains(self.pointer)),
                    ] {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            if active {
                                theme.bg_hover
                            } else {
                                theme.tokens.colors.neutral[3]
                            },
                            None,
                            &Rect::new(
                                target.x0 + 4.0,
                                target.y0 + 4.0,
                                target.x1 - 4.0,
                                target.y1 - 4.0,
                            ),
                        );
                    }
                    draw_icon(
                        scene,
                        transform,
                        Rect::new(
                            details.center().x - 8.0,
                            details.y0 + 16.0,
                            details.center().x + 8.0,
                            details.y0 + 32.0,
                        ),
                        Icon::Pencil,
                        theme.tokens.colors.accent[6],
                        1.5,
                    );
                    let confirmed = self.library_remove_confirm.as_deref() == Some(id);
                    draw_icon(
                        scene,
                        transform,
                        Rect::from_center_size(remove.center(), (16.0, 16.0)),
                        if confirmed { Icon::Check } else { Icon::Trash2 },
                        if confirmed {
                            theme.tokens.colors.semantic.danger
                        } else {
                            theme.text_dim
                        },
                        1.5,
                    );
                    draw_icon(
                        scene,
                        transform,
                        Rect::from_center_size(star.center(), (17.0, 17.0)),
                        Icon::Star,
                        if favorite {
                            theme.tokens.colors.accent[6]
                        } else {
                            theme.text_dim
                        },
                        1.5,
                    );
                }
                self.library_details_cards.push((id.clone(), details));
            } else if card_fully_visible
                && !self.album_editor
                && !self.selection_mode
                && card.width() >= 148.0
            {
                // Touch and enlarged-text layouts cannot depend on hover.
                // Keep the image dominant while providing an honest 44px
                // action shelf on tiles wide enough to contain all controls.
                let overlay = Rect::new(card.x0, action_y, card.x1, card.y1);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.neutral[0].with_alpha(0.82),
                    None,
                    &overlay,
                );
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(details.center(), (16.0, 16.0)),
                    Icon::Pencil,
                    theme.tokens.colors.accent[6],
                    1.5,
                );
                let confirmed = self.library_remove_confirm.as_deref() == Some(id);
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(remove.center(), (16.0, 16.0)),
                    if confirmed { Icon::Check } else { Icon::Trash2 },
                    if confirmed {
                        theme.tokens.colors.semantic.danger
                    } else {
                        theme.text_dim
                    },
                    1.5,
                );
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(star.center(), (17.0, 17.0)),
                    Icon::Star,
                    if favorite {
                        theme.tokens.colors.accent[6]
                    } else {
                        theme.text_dim
                    },
                    1.5,
                );
                self.library_details_cards.push((id.clone(), details));
            }
            if self.album_editor && self.album_editor_members.contains(id) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Rect::new(card.x0, card.y0, card.x1, card.y0 + 3.0),
                );
            }
            if self.selection_mode && self.selected_photo_ids.contains(id) {
                scene.stroke(
                    &Stroke::new(3.0),
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &card,
                );
                let badge = Point::new(card.x1 - 24.0, card.y0 + 24.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Circle::new(badge, 15.0),
                );
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(badge, (18.0, 18.0)),
                    Icon::Check,
                    theme.tokens.colors.surfaces.base,
                    2.0,
                );
            }
            if card_fully_visible {
                self.library_cards.push((id.clone(), card, favorite));
            }
        }
        scene.pop_layer();
        if !self.album_editor && !self.metadata_editor && !self.selection_album_chooser {
            self.timeline_index.paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.timeline_index_metrics,
                &self.timeline_index_markers,
            );
        }
        self.draw_album_editor(scene, transform, text_renderer, theme, presence);
        self.draw_metadata_editor(scene, transform, text_renderer, theme, presence);
        self.draw_selection_destination_sheet(scene, transform, text_renderer, theme, bounds);
    }

    /// Render the location library as an offline-first map or an equivalent
    /// list. The map is a reference-grid substrate, not a disguised online
    /// tile service: opening Places never sends coordinates off-device.
    #[expect(
        clippy::too_many_lines,
        reason = "the Places map and its equivalent list share one geometry pass so hit targets and paint agree"
    )]
    fn draw_places(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;
        self.places_hits.clear();
        self.collection_page_hits.clear();
        let bounds = Rect::new(
            self.canvas_rect.x0 + 10.0,
            self.canvas_rect.y0 + 10.0,
            self.canvas_rect.x1 - 10.0,
            self.canvas_rect.y1 - 10.0,
        );
        let compact = bounds.width() < 760.0 || theme.text_scale > 1.25;
        // Build the location projection once per frame. The heading, marker
        // layer, and desktop rail all describe the same canonical collections
        // without cloning or regrouping library photographs during paint.
        let place_clusters =
            crate::places::clusters(self.places_items.iter().map(|item| &item.photo));
        let heading_h = if compact { 86.0 } else { 68.0 };
        let heading = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y0 + heading_h);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &heading,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &heading,
        );
        let open_place = self.places_open_key.as_ref().and_then(|key| {
            self.places_items.iter().find_map(|item| {
                item.photo.location.as_ref().and_then(|location| {
                    (location.place_key() == *key).then(|| location.display_name())
                })
            })
        });
        let visible_place_count = self
            .places_items
            .iter()
            .filter(|item| {
                self.places_open_key.as_ref().is_none_or(|key| {
                    item.photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == *key)
                })
            })
            .count();
        text_renderer.draw_text_weighted(
            scene,
            open_place.as_deref().unwrap_or("Places"),
            if compact { 23.0 } else { 25.0 },
            theme.text_primary,
            transform * Affine::translate((heading.x0 + 16.0, heading.y0 + 14.0)),
            Some(if compact { 210.0 } else { 290.0 }),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!(
                "{} photographs · {} named place{}{}",
                visible_place_count,
                place_clusters.len(),
                if place_clusters.len() == 1 { "" } else { "s" },
                if open_place.is_some() {
                    " at this place"
                } else {
                    ""
                }
            ),
            12.0,
            theme.text_dim,
            transform * Affine::translate((heading.x0 + 16.0, heading.y0 + 42.0)),
            Some(if compact {
                (heading.width() - 32.0).max(80.0) as f32
            } else {
                (heading.width() - 510.0).max(80.0) as f32
            }),
            TextWeight::Regular,
        );
        if compact {
            self.draw_library_navigation(
                scene,
                transform,
                text_renderer,
                theme,
                Rect::new(
                    heading.x0 + 12.0,
                    heading.y0 + 56.0,
                    heading.x1 - 12.0,
                    heading.y1 - 10.0,
                ),
            );
        } else {
            self.draw_library_navigation(
                scene,
                transform,
                text_renderer,
                theme,
                Rect::new(
                    (heading.x1 - 450.0).max(heading.x0 + 360.0),
                    heading.y0 + 14.0,
                    heading.x1 - 10.0,
                    heading.y0 + 54.0,
                ),
            );
        }
        if self.places_items.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme, "No places yet", "Import photos with GPS metadata or add an explicit shared location in photo details. Photos does not guess or upload locations.");
            return;
        }
        let content = Rect::new(bounds.x0, heading.y1 + 12.0, bounds.x1, bounds.y1);
        let show_overview = !self.places_list_mode && self.places_open_key.is_none();
        let rail_w = if show_overview && content.width() >= 960.0 {
            300.0
        } else {
            0.0
        };
        let map = Rect::new(content.x0, content.y0, content.x1 - rail_w, content.y1);
        self.places_map_bounds = map;
        let rail =
            (rail_w > 0.0).then(|| Rect::new(map.x1 + 12.0, content.y0, content.x1, content.y1));
        let toggle = Rect::new(map.x1 - 132.0, map.y0 + 12.0, map.x1 - 12.0, map.y0 + 50.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &toggle,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &toggle,
        );
        text_renderer.draw_text_weighted(
            scene,
            if self.places_list_mode {
                "Map"
            } else {
                "Photo list"
            },
            12.0,
            theme.text_primary,
            transform * Affine::translate((toggle.x0 + 14.0, toggle.y0 + 11.0)),
            Some((toggle.width() - 28.0) as f32),
            TextWeight::Medium,
        );
        self.places_hits.push((PlacesHit::ToggleList, toggle));
        if self.places_list_mode {
            self.draw_places_list(scene, transform, text_renderer, theme, map);
            return;
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.neutral[1],
            None,
            &map,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &map,
        );
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &map,
        );
        // A bundled, deliberately low-detail land silhouette gives the
        // offline map genuine geographic orientation without contacting a
        // tile provider. Roads and live place data belong to Maps; Photos
        // needs a calm spatial index that remains useful on an airplane.
        for landmass in OFFLINE_WORLD_LANDMASSES {
            let mut coast = BezPath::new();
            let mut visible_points = 0_usize;
            for &(latitude, longitude) in *landmass {
                let location = crate::model::PhotoLocation {
                    latitude,
                    longitude,
                    label: None,
                    provenance: crate::model::LocationProvenance::default(),
                };
                let Some((x, y)) =
                    self.places_viewport
                        .project(&location, map.width(), map.height())
                else {
                    continue;
                };
                let point = (map.x0 + x, map.y0 + y);
                if visible_points == 0 {
                    coast.move_to(point);
                } else {
                    coast.line_to(point);
                }
                visible_points += 1;
            }
            if visible_points >= 3 {
                coast.close_path();
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.neutral[3],
                    None,
                    &coast,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.neutral[5],
                    None,
                    &coast,
                );
            }
        }
        // Calm offline geographic grid: it retains spatial orientation while
        // clearly avoiding a claim that live roads/tiles were downloaded.
        for step in 1..6 {
            let fx = map.x0 + map.width() * f64::from(step) / 6.0;
            let fy = map.y0 + map.height() * f64::from(step) / 6.0;
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(fx, map.y0, fx + 1.0, map.y1),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(map.x0, fy, map.x1, fy + 1.0),
            );
        }
        let map_summary = Rect::new(map.x0 + 14.0, map.y0 + 14.0, map.x0 + 226.0, map.y0 + 84.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised.with_alpha(0.94),
            None,
            &RoundedRect::from_rect(map_summary, 12.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(map_summary, 12.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            "Your places",
            13.0,
            theme.text_primary,
            transform * Affine::translate((map_summary.x0 + 12.0, map_summary.y0 + 12.0)),
            Some(150.0),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Drag to explore · scroll to zoom",
            11.0,
            theme.text_dim,
            transform * Affine::translate((map_summary.x0 + 12.0, map_summary.y0 + 34.0)),
            Some((map_summary.width() - 24.0) as f32),
            TextWeight::Regular,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Offline and private",
            11.0,
            theme.tokens.colors.accent[8],
            transform * Affine::translate((map_summary.x0 + 12.0, map_summary.y0 + 51.0)),
            Some((map_summary.width() - 24.0) as f32),
            TextWeight::Medium,
        );
        let zoom_out = Rect::new(map.x0 + 14.0, map.y1 - 98.0, map.x0 + 54.0, map.y1 - 58.0);
        let zoom_in = Rect::new(map.x0 + 14.0, map.y1 - 52.0, map.x0 + 54.0, map.y1 - 12.0);
        for (hit, rect, label) in [
            (PlacesHit::ZoomOut, zoom_out, "−"),
            (PlacesHit::ZoomIn, zoom_in, "+"),
        ] {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised.with_alpha(0.94),
                None,
                &rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                20.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 14.0, rect.y0 + 6.0)),
                Some(16.0),
                TextWeight::Medium,
            );
            self.places_hits.push((hit, rect));
        }
        let camera_feedback = self.places_camera_feedback();
        for cluster in &place_clusters {
            let location = crate::model::PhotoLocation {
                latitude: cluster.latitude,
                longitude: cluster.longitude,
                label: Some(cluster.label.clone()),
                provenance: crate::model::LocationProvenance::default(),
            };
            let Some((px, py)) = self
                .places_viewport
                .project(&location, map.width(), map.height())
            else {
                continue;
            };
            let point = Point::new(map.x0 + px, map.y0 + py);
            if !map.contains(point) {
                continue;
            }
            let radius = (16.0 + (cluster.count() as f64).sqrt() * 2.5).min(25.0)
                * (1.0 + camera_feedback * 0.10);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.accent[6],
                None,
                &Circle::new(point, radius),
            );
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &Circle::new(point, radius),
            );
            if let Some(photo_id) = cluster.photo_ids.first()
                && let Some(image) = self.library_thumbnails.get(
                    self.thumbnail_resource_keys
                        .get(photo_id)
                        .unwrap_or(photo_id),
                )
            {
                let side = (radius * 1.25).max(18.0);
                let scale = (side / f64::from(image.width)).min(side / f64::from(image.height));
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &Circle::new(point, radius - 3.0),
                );
                let image_transform = transform
                    * Affine::translate((
                        point.x - f64::from(image.width) * scale * 0.5,
                        point.y - f64::from(image.height) * scale * 0.5,
                    ))
                    * Affine::scale(scale);
                scene.draw_image(image, image_transform);
                scene.pop_layer();
            }
            let count_badge = Rect::from_center_size(
                Point::new(point.x + radius - 3.0, point.y - radius + 3.0),
                (18.0, 18.0),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.accent[8],
                None,
                &RoundedRect::from_rect(count_badge, 9.0),
            );
            text_renderer.draw_text_weighted(
                scene,
                &cluster.count().to_string(),
                10.0,
                theme.tokens.colors.surfaces.base,
                transform * Affine::translate((count_badge.x0 + 6.0, count_badge.y0 + 4.0)),
                Some(10.0),
                TextWeight::Medium,
            );
            let hit = Rect::new(
                point.x - radius,
                point.y - radius,
                point.x + radius,
                point.y + radius,
            );
            self.places_hits
                .push((PlacesHit::Place(cluster.key.clone()), hit));
            if map.width() >= 520.0 {
                let label_width =
                    (cluster.label.chars().count() as f64 * 6.5 + 24.0).clamp(72.0, 180.0);
                let label_x =
                    (point.x - label_width * 0.5).clamp(map.x0 + 6.0, map.x1 - label_width - 6.0);
                let label_y = (point.y + radius + 8.0).min(map.y1 - 38.0);
                let label = Rect::new(label_x, label_y, label_x + label_width, label_y + 30.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(label, 10.0),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &cluster.label,
                    10.0,
                    theme.text_primary,
                    transform * Affine::translate((label.x0 + 10.0, label.y0 + 8.0)),
                    Some((label.width() - 20.0) as f32),
                    TextWeight::Medium,
                );
            }
        }
        scene.pop_layer();
        // The mode switch belongs to map chrome, so paint it after the clipped
        // cartography instead of letting the map substrate cover it.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &toggle,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &toggle,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Photo list",
            12.0,
            theme.text_primary,
            transform * Affine::translate((toggle.x0 + 14.0, toggle.y0 + 11.0)),
            Some((toggle.width() - 28.0) as f32),
            TextWeight::Medium,
        );
        if let Some(rail) = rail {
            self.draw_places_overview_rail(
                scene,
                transform,
                text_renderer,
                theme,
                rail,
                &place_clusters,
            );
        }
    }

    /// A compact, keyboard-addressable summary of the canonical collections
    /// shown on the map.  It keeps the map spatial while making the next
    /// useful decision obvious on a desktop: choose the city, not a random
    /// representative photograph.
    fn draw_places_overview_rail(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        clusters: &[crate::places::PlaceCluster],
    ) {
        use vello::peniko::Fill;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(rail, 14.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(rail, 14.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            "Places",
            18.0,
            theme.text_primary,
            transform * Affine::translate((rail.x0 + 16.0, rail.y0 + 18.0)),
            Some((rail.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!("{} collections in this library", clusters.len()),
            12.0,
            theme.text_dim,
            transform * Affine::translate((rail.x0 + 16.0, rail.y0 + 44.0)),
            Some((rail.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        let row_h = 82.0;
        let mut y = rail.y0 + 76.0;
        for cluster in clusters.iter().take(5) {
            if y + row_h > rail.y1 - 12.0 {
                break;
            }
            let row = Rect::new(rail.x0 + 10.0, y, rail.x1 - 10.0, y + row_h - 6.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.neutral[2],
                None,
                &RoundedRect::from_rect(row, 10.0),
            );
            let marker = Point::new(row.x0 + 26.0, row.y0 + row.height() * 0.5);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.accent[6],
                None,
                &Circle::new(marker, 14.0),
            );
            text_renderer.draw_text_weighted(
                scene,
                &cluster.label,
                14.0,
                theme.text_primary,
                transform * Affine::translate((row.x0 + 52.0, row.y0 + 13.0)),
                Some((row.width() - 68.0) as f32),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                &format!(
                    "{} photo{} · open collection",
                    cluster.count(),
                    if cluster.count() == 1 { "" } else { "s" }
                ),
                11.0,
                theme.text_dim,
                transform * Affine::translate((row.x0 + 52.0, row.y0 + 37.0)),
                Some((row.width() - 68.0) as f32),
                TextWeight::Regular,
            );
            self.places_hits
                .push((PlacesHit::Place(cluster.key.clone()), row));
            y += row_h;
        }
        text_renderer.draw_text_weighted(
            scene,
            "Locations stay on your device until you choose to share them.",
            11.0,
            theme.text_dim,
            transform * Affine::translate((rail.x0 + 16.0, rail.y1 - 30.0)),
            Some((rail.width() - 32.0) as f32),
            TextWeight::Regular,
        );
    }

    fn draw_places_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        map: Rect,
    ) {
        use vello::peniko::Fill;
        let items: Vec<PlaceItem> = self
            .places_items
            .iter()
            .filter(|item| {
                self.places_open_key.as_ref().is_none_or(|key| {
                    item.photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == *key)
                })
            })
            .cloned()
            .collect();
        self.draw_collection_pager(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            Rect::new(map.x0 + 12.0, map.y0 + 8.0, map.x1 - 124.0, map.y0 + 48.0),
            self.places_page,
            items.len(),
            CollectionPageHit::PlacesPrevious,
            CollectionPageHit::PlacesNext,
        );
        let mut y = map.y0 + 64.0;
        for item in items
            .iter()
            .skip(self.places_page * COLLECTION_PAGE_SIZE)
            .take(COLLECTION_PAGE_SIZE)
        {
            if y + 58.0 > map.y1 {
                break;
            }
            let row = Rect::new(map.x0 + 12.0, y, map.x1 - 12.0, y + 50.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &row,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &row,
            );
            text_renderer.draw_text_weighted(
                scene,
                &item.photo.image.name,
                14.0,
                theme.text_primary,
                transform * Affine::translate((row.x0 + 12.0, row.y0 + 8.0)),
                Some((row.width() - 220.0) as f32),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                &format!(
                    "{} · {}",
                    item.photo.location.as_ref().map_or_else(
                        || "Shared location".to_owned(),
                        crate::model::PhotoLocation::display_name
                    ),
                    item.photo.captured_at.as_deref().unwrap_or("Undated")
                ),
                11.0,
                theme.text_dim,
                transform * Affine::translate((row.x0 + 12.0, row.y0 + 29.0)),
                Some((row.width() - 220.0) as f32),
                TextWeight::Regular,
            );
            self.places_hits
                .push((PlacesHit::Photo(item.photo.id.clone()), row));
            y += 58.0;
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the responsive selection task bar keeps its painted controls and exact pointer hit geometry in one audited component"
    )]
    fn draw_selection_controls(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        bounds: Rect,
        masthead: Rect,
        selection_band: Rect,
        desktop_library: bool,
        compact_selection_rect: Rect,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;
        let theme = ctx.theme;
        let (selection_y, selection_h) = (selection_band.y0, selection_band.height());

        self.selection_hits.clear();
        self.selection_destination_hits.clear();
        let transform = ctx.transform;
        let draw_button = |scene: &mut Scene,
                           text_renderer: &mut TextRenderer,
                           hit: SelectionHit,
                           rect: Rect,
                           label: &str,
                           enabled: bool,
                           hits: &mut Vec<(SelectionHit, Rect)>| {
            scene.fill(
                Fill::NonZero,
                transform,
                if enabled {
                    theme.tokens.colors.surfaces.raised
                } else {
                    theme.tokens.colors.neutral[2]
                },
                None,
                &rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                if enabled {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 13.0)),
                Some((rect.width() - 20.0) as f32),
                TextWeight::Medium,
            );
            if enabled {
                hits.push((hit, rect));
            }
        };

        if !self.selection_mode {
            // The normal desktop masthead has intentional empty room between
            // its destination tabs and the right-edge density controls. On
            // compact layouts this short, named control follows the heading
            // rather than replacing a destructive card affordance.
            let rect = if desktop_library {
                Rect::new(
                    masthead.x1 - 216.0,
                    masthead.y0 + 16.0,
                    masthead.x1 - 104.0,
                    masthead.y1 - 16.0,
                )
            } else {
                compact_selection_rect
            };
            self.library_select_button.apply_layout_external(rect);
            self.library_select_button
                .draw(ctx.scene, ctx.transform, ctx.text, theme, presence);
            self.selection_hits.push((SelectionHit::Start, rect));
            return;
        }

        let selected = self.selected_photo_ids.len();
        let destination_label = self
            .selection_destination_album
            .as_deref()
            .and_then(|id| {
                self.library_albums
                    .iter()
                    .find(|(album_id, _, _)| album_id == id)
                    .map(|(_, name, _)| name)
            })
            .map_or_else(
                || "Choose album".to_owned(),
                |name| format!("Add to {}", compact_label(name, 12)),
            );
        let wide = desktop_library && selection_h <= 52.0;
        let x = bounds.x0 + f64::from(PAD);
        let max_x = bounds.x1 - f64::from(PAD);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &Rect::new(x, selection_y, max_x, selection_y + selection_h - 4.0),
        );
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            theme.border_standard,
            None,
            &Rect::new(x, selection_y, max_x, selection_y + selection_h - 4.0),
        );
        let label = format!("{selected} selected");
        if wide {
            ctx.text.draw_text_weighted(
                ctx.scene,
                &label,
                13.0,
                theme.text_primary,
                ctx.transform * Affine::translate((x + 12.0, selection_y + 15.0)),
                Some(112.0),
                TextWeight::Medium,
            );
            let mut button_x = x + 122.0;
            for (hit, label, width, enabled) in [
                (
                    SelectionHit::SelectAll,
                    "Select all",
                    102.0,
                    !self.library_cards.is_empty(),
                ),
                (SelectionHit::Clear, "Clear", 76.0, selected > 0),
                (
                    SelectionHit::Favorite,
                    if self.selection_all_favorite {
                        "Unfavorite"
                    } else {
                        "Favorite"
                    },
                    104.0,
                    selected > 0,
                ),
                (
                    SelectionHit::EditDetails,
                    "Edit details",
                    112.0,
                    selected > 0,
                ),
                (
                    if self.selection_destination_album.is_some() {
                        SelectionHit::AddToAlbum
                    } else {
                        SelectionHit::ChooseAlbum
                    },
                    destination_label.as_str(),
                    128.0,
                    selected > 0 && !self.library_albums.is_empty(),
                ),
                (SelectionHit::Done, "Done", 78.0, true),
            ] {
                let rect = Rect::new(
                    button_x,
                    selection_y + 4.0,
                    button_x + width,
                    selection_y + 44.0,
                );
                draw_button(
                    ctx.scene,
                    ctx.text,
                    hit,
                    rect,
                    label,
                    enabled,
                    &mut self.selection_hits,
                );
                button_x += width + 6.0;
            }
        } else {
            ctx.text.draw_text_weighted(
                ctx.scene,
                &label,
                12.0,
                theme.text_primary,
                ctx.transform * Affine::translate((x + 10.0, selection_y + 15.0)),
                Some(92.0),
                TextWeight::Medium,
            );
            let row_y = selection_y + 4.0;
            let middle_x = x + 98.0;
            let middle_w = ((max_x - 84.0 - middle_x - 6.0) * 0.5).max(72.0);
            for (hit, label, rect, enabled) in [
                (
                    SelectionHit::Done,
                    "Done",
                    Rect::new(max_x - 78.0, row_y, max_x, row_y + 40.0),
                    true,
                ),
                (
                    SelectionHit::EditDetails,
                    "Details",
                    Rect::new(middle_x, row_y, middle_x + middle_w, row_y + 40.0),
                    selected > 0,
                ),
                (
                    if self.selection_destination_album.is_some() {
                        SelectionHit::AddToAlbum
                    } else {
                        SelectionHit::ChooseAlbum
                    },
                    destination_label.as_str(),
                    Rect::new(middle_x + middle_w + 6.0, row_y, max_x - 84.0, row_y + 40.0),
                    selected > 0 && !self.library_albums.is_empty(),
                ),
            ] {
                draw_button(
                    ctx.scene,
                    ctx.text,
                    hit,
                    rect,
                    label,
                    enabled,
                    &mut self.selection_hits,
                );
            }
        }
    }

    /// The album picker is a real modal surface, not a third row packed into
    /// the selection bar. That keeps 200% text, paging, and the card grid
    /// from competing for the same few pixels. The underlying selection stays
    /// intact but is deliberately inert until this sheet closes.
    #[allow(
        clippy::too_many_lines,
        reason = "the modal keeps responsive layout, drawing, paging, and exact hit geometry together so they cannot drift"
    )]
    fn draw_selection_destination_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
    ) {
        self.selection_destination_hits.clear();
        if !self.library_mode || !self.selection_album_chooser {
            return;
        }
        // The sheet owns the interaction tree while it is visible. The
        // selection bar remains visible behind the scrim for context, but it
        // cannot receive a second, conflicting command.
        self.selection_hits.clear();
        let page_count = self
            .library_albums
            .len()
            .div_ceil(SELECTION_ALBUMS_PER_PAGE);
        self.selection_album_page = self.selection_album_page.min(page_count.saturating_sub(1));
        let start = self.selection_album_page * SELECTION_ALBUMS_PER_PAGE;
        let end = (start + SELECTION_ALBUMS_PER_PAGE).min(self.library_albums.len());
        let previous_enabled = self.selection_album_page > 0;
        let next_enabled = end < self.library_albums.len();
        let apply_enabled = self.selection_destination_album.is_some();

        // This used to be a hand-painted panel with four local button
        // imitations.  ModalFrame gives the chooser the same elevation,
        // close target, focus treatment, and platform action order as the
        // People and metadata surfaces while retaining its canonical album
        // resource ids and existing accessibility tree.
        let base_actions = [
            ModalAction::secondary("Previous").enabled(previous_enabled),
            ModalAction::secondary("Next").enabled(next_enabled),
            ModalAction::secondary("Cancel"),
            ModalAction::primary("Use album").enabled(apply_enabled),
        ];
        let base_spec = ModalSpec::new("Choose destination album")
            .actions(&base_actions)
            .body_height(310.0)
            .max_width(620.0)
            .dismiss_on_scrim(false);
        let layout = ModalFrame::layout(bounds, &base_spec, text_renderer, theme);
        let hovered = |index: usize| {
            layout
                .actions
                .get(index)
                .is_some_and(|rect| rect.contains(self.pointer))
        };
        let actions = [
            ModalAction::secondary("Previous")
                .enabled(previous_enabled)
                .hovered(hovered(0)),
            ModalAction::secondary("Next")
                .enabled(next_enabled)
                .hovered(hovered(1)),
            ModalAction::secondary("Cancel").hovered(hovered(2)),
            ModalAction::primary("Use album")
                .enabled(apply_enabled)
                .hovered(hovered(3)),
        ];
        let spec = ModalSpec::new("Choose destination album")
            .actions(&actions)
            .body_height(310.0)
            .max_width(620.0)
            .dismiss_on_scrim(false)
            .close_hovered(layout.close.is_some_and(|rect| rect.contains(self.pointer)));
        ModalFrame::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);

        let (ink, dim) = ModalFrame::ink(theme);
        // Primary-button rows use the theme's contrast-checked accent ink;
        // album choices stay readable in every system theme without baking
        // a foreground colour into this app-owned presentation.
        let selected_ink = theme.tokens.colors.on_accent(4.5);
        let summary = format!(
            "Albums {}–{} of {} · choose one destination",
            start + 1,
            end,
            self.library_albums.len()
        );
        text_renderer.draw_text_weighted(
            scene,
            &summary,
            12.0,
            dim,
            transform * Affine::translate((layout.body.x0, layout.body.y0 + 2.0)),
            Some(layout.body.width() as f32),
            TextWeight::Regular,
        );
        let list_top = layout.body.y0 + 30.0;
        let list_bottom = layout.body.y1 - 2.0;
        let visible_count = end.saturating_sub(start).max(1);
        let row_h = ((list_bottom - list_top) / visible_count as f64).clamp(54.0, 68.0);
        let visible_albums = self.library_albums.get(start..end).unwrap_or_default();
        for (index, (id, name, count)) in visible_albums.iter().enumerate() {
            let y = list_top + index as f64 * row_h;
            let row = Rect::new(layout.body.x0, y, layout.body.x1, y + row_h - 6.0);
            let chosen = self.selection_destination_album.as_deref() == Some(id);
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(row, "")
                    .style(if chosen {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Neutral
                    })
                    .radius(10.0)
                    .hovered(row.contains(self.pointer))
                    .accessible_name(&format!("Use {name} as destination album")),
            );
            text_renderer.draw_text_weighted(
                scene,
                name,
                13.0,
                if chosen { selected_ink } else { ink },
                transform * Affine::translate((row.x0 + 14.0, row.y0 + 12.0)),
                Some((row.width() - 128.0) as f32),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                &format!("{count} photo{}", if *count == 1 { "" } else { "s" }),
                11.0,
                if chosen {
                    selected_ink.with_alpha(0.78)
                } else {
                    dim
                },
                transform * Affine::translate((row.x1 - 100.0, row.y0 + 14.0)),
                Some(86.0),
                TextWeight::Regular,
            );
            self.selection_destination_hits.push((id.clone(), row));
        }
        ModalFrame::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        // One layout rect per declared action, in declaration order.
        let &[previous_rect, next_rect, cancel_rect, apply_rect] = layout.actions.as_slice() else {
            return;
        };
        if previous_enabled {
            self.selection_hits
                .push((SelectionHit::PreviousDestinationPage, previous_rect));
        }
        if next_enabled {
            self.selection_hits
                .push((SelectionHit::NextDestinationPage, next_rect));
        }
        self.selection_hits
            .push((SelectionHit::CancelDestinationChooser, cancel_rect));
        if let Some(close) = layout.close {
            self.selection_hits
                .push((SelectionHit::CloseDestinationChooser, close));
        }
        if apply_enabled {
            self.selection_hits
                .push((SelectionHit::ApplyDestination, apply_rect));
        }
    }

    /// Paint the album editor directly over the library, keeping the selected
    /// photographs visible behind it. On narrow screens it becomes a full
    /// width sheet, avoiding a tiny side panel with clipped controls.
    #[allow(
        clippy::too_many_lines,
        reason = "the compact/wide album-sheet geometry and its shared-widget paint path must remain auditable together"
    )]
    fn draw_album_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.library_mode || self.metadata_editor {
            return;
        }
        // The Albums overview owns its visible New/Edit controls. Keep those
        // retained hit targets intact unless an editor sheet is actually
        // replacing them.
        if !self.album_editor && self.library_destination == LibraryDestination::Albums {
            return;
        }
        self.album_editor_hits.clear();
        // On phone and 200% the library opens with chronology above the fold.
        // Album editing remains available through its canonical resource
        // action; it does not occupy a permanent New/Edit row in front of
        // every first photo.
        if !self.album_editor
            && (theme.text_scale > 1.25
                || self.canvas_rect.width() < LIBRARY_DESKTOP_MASTHEAD_MIN_W)
        {
            return;
        }
        if self.album_editor && theme.text_scale > 1.25 {
            self.draw_large_text_album_editor(scene, transform, text_renderer, theme, presence);
            return;
        }
        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 360.0 };
        // A phone must keep actual photographs reachable while editing; a
        // compact bottom sheet leaves the upper grid visible instead of
        // turning “choose photographs” into an instruction with no surface.
        let panel_h = if !self.album_editor && narrow {
            // Album management is an ordinary library route, not a tiny
            // desktop-only affordance. Keep its buttons at the same 44px
            // target as the card actions below it.
            44.0
        } else if narrow {
            // The editor has two paging controls and a footer. Their 44px
            // targets need a real gap rather than being squeezed into the
            // former 34px desktop rhythm.
            280.0
        } else {
            332.0
        };
        let album_chips_h = if self.library_albums.is_empty() {
            0.0
        } else {
            26.0 + LIB_ROW_GAP
        };
        let control_radius = f64::from(theme.tokens.radii.md);
        let panel_radius = f64::from(theme.tokens.radii.lg);
        let panel_y = if !self.album_editor && narrow {
            // Kept in lock-step with `draw_library`'s album and metadata
            // filter rows: the buttons start where the filter row (chip +
            // `s2`) ends, so the shelf keeps one rhythm.
            bounds.y0 + f64::from(PAD) + 31.0 + album_chips_h + 28.0 + LIB_ROW_GAP_TIGHT
        } else if narrow {
            bounds.y1 - panel_h - 8.0
        } else {
            bounds.y0 + 10.0
        };
        let desktop_header_actions = !self.album_editor
            && !narrow
            && theme.text_scale <= 1.25
            && bounds.width() >= LIBRARY_DESKTOP_MASTHEAD_MIN_W;
        let panel = if desktop_header_actions {
            // Album management belongs to the album rail, not between the
            // Library title and its first-class destination tabs. When the
            // rail is paged, leave its shared pager the rightmost lane.
            let album_pages = self
                .library_albums
                .len()
                .max(1)
                .div_ceil(COLLECTION_PAGE_SIZE);
            let pager_reserve = if album_pages > 1 { 300.0 } else { 0.0 };
            let right = bounds.x1 - f64::from(PAD) - pager_reserve;
            Rect::new(right - 226.0, bounds.y0 + 72.0, right, bounds.y0 + 116.0)
        } else {
            Rect::new(
                if narrow {
                    bounds.x0 + 8.0
                } else {
                    bounds.x1 - panel_w - 12.0
                },
                panel_y,
                if narrow {
                    bounds.x1 - 8.0
                } else {
                    bounds.x1 - 12.0
                },
                panel_y + panel_h,
            )
        };
        self.album_editor_rect = panel;
        if !self.album_editor {
            let new = Rect::new(panel.x0, panel.y0, panel.x0 + 110.0, panel.y0 + 44.0);
            let edit = Rect::new(new.x1 + 8.0, panel.y0, new.x1 + 116.0, panel.y0 + 44.0);
            for (hit, rect, enabled) in [
                (AlbumEditorHit::New, new, true),
                (AlbumEditorHit::Edit, edit, self.active_album.is_some()),
            ] {
                if let Some((_, button)) = self
                    .library_album_buttons
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == hit)
                {
                    button.set_enabled(enabled);
                    button.apply_layout_external(rect);
                    button.draw(scene, transform, text_renderer, theme, presence);
                }
                if enabled {
                    self.album_editor_hits.push((hit, rect));
                }
            }
            return;
        }
        ui::surface::shadow(
            scene,
            transform,
            panel,
            panel_radius,
            &theme.tokens.elevation.e3,
        );
        // The sheet is a card over the grid: raised fill, derived edge.
        surface::card(
            scene,
            transform,
            panel,
            panel_radius,
            theme.tokens.colors.surfaces.raised,
        );
        text_renderer.draw_text_weighted(
            scene,
            if self.album_editor_creating {
                "New album"
            } else {
                "Edit album"
            },
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 16.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Album name",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 48.0)),
            Some(180.0),
            TextWeight::Regular,
        );
        let name_rect = Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 66.0,
            panel.x1 - 16.0,
            panel.y0 + 106.0,
        );
        self.album_name_input.apply_layout_external(name_rect);
        self.album_name_input
            .draw(scene, transform, text_renderer, theme, presence);
        text_renderer.draw_text_weighted(
            scene,
            "Choose photographs in the grid",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 122.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        text_renderer.draw_text_weighted(
            scene,
            &self.library_page_label(),
            14.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 146.0)),
            Some((panel.width() - 150.0) as f32),
            TextWeight::Medium,
        );
        // Right-aligned to the sheet's 16 px inset — the Next button's edge on
        // the row below — from its measured width, instead of starting at a
        // fixed offset and ending wherever the digits stop.
        let selected = format!("{} selected", self.album_editor_members.len());
        let (selected_w, _) =
            text_renderer.measure_weighted(&selected, 13.0, Some(100.0), TextWeight::Regular);
        text_renderer.draw_text_weighted(
            scene,
            &selected,
            13.0,
            theme.text_dim,
            transform
                * Affine::translate((
                    panel.x1 - 16.0 - f64::from(selected_w.min(100.0)),
                    panel.y0 + 147.0,
                )),
            Some(100.0),
            TextWeight::Regular,
        );
        let page_y = panel.y0 + 166.0;
        let previous = Rect::new(panel.x0 + 16.0, page_y, panel.x0 + 108.0, page_y + 44.0);
        let next = Rect::new(panel.x1 - 108.0, page_y, panel.x1 - 16.0, page_y + 44.0);
        for (hit, rect) in [
            (AlbumEditorHit::PreviousPage, previous),
            (AlbumEditorHit::NextPage, next),
        ] {
            let enabled = match hit {
                AlbumEditorHit::PreviousPage => self.can_page_library(false),
                AlbumEditorHit::NextPage => self.can_page_library(true),
                _ => false,
            };
            if let Some((_, button)) = self
                .album_page_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, theme, presence);
            }
        }
        let y = panel.y1 - 60.0;
        let cancel = Rect::new(panel.x0 + 16.0, y, panel.x0 + 88.0, y + 44.0);
        let save = Rect::new(panel.x1 - 92.0, y, panel.x1 - 16.0, y + 44.0);
        let delete = Rect::new(save.x0 - 88.0, y, save.x0 - 8.0, y + 44.0);
        for (hit, rect, label, style, focused) in [
            (
                AlbumEditorHit::Cancel,
                cancel,
                "Cancel",
                ButtonStyle::Neutral,
                self.album_editor_focus == 1,
            ),
            (
                AlbumEditorHit::Save,
                save,
                "Save",
                ButtonStyle::Primary,
                self.album_editor_focus == if self.album_editor_creating { 2 } else { 3 },
            ),
        ] {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(rect, label)
                    .style(style)
                    .radius(control_radius)
                    .hovered(rect.contains(self.pointer))
                    .focused(focused),
            );
            self.album_editor_hits.push((hit, rect));
        }
        if !self.album_editor_creating {
            let (hit, label) = if self.album_delete_confirm {
                (AlbumEditorHit::ConfirmDelete, "Delete album?")
            } else {
                (AlbumEditorHit::Delete, "Delete")
            };
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(delete, label)
                    .style(ButtonStyle::Danger)
                    .radius(control_radius)
                    .hovered(delete.contains(self.pointer))
                    .focused(self.album_editor_focus == 2),
            );
            self.album_editor_hits.push((hit, delete));
        }
    }

    /// A large-text album sheet uses a short vertical rhythm instead of trying
    /// to scale the desktop side panel into a phone viewport.  Its action
    /// targets are still the same local intents and shared `TextInput` as the
    /// regular sheet; only this device's presentation changes.
    #[allow(
        clippy::too_many_lines,
        reason = "large-text album geometry deliberately keeps every field and action in one auditable sheet"
    )]
    fn draw_large_text_album_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 460.0 };
        let panel_h = (bounds.height() - 16.0).min(620.0);
        let panel = Rect::new(
            if narrow {
                bounds.x0 + 8.0
            } else {
                bounds.x1 - panel_w - 12.0
            },
            if narrow {
                bounds.y1 - panel_h - 8.0
            } else {
                bounds.y0 + 12.0
            },
            if narrow {
                bounds.x1 - 8.0
            } else {
                bounds.x1 - 12.0
            },
            if narrow {
                bounds.y1 - 8.0
            } else {
                bounds.y0 + 12.0 + panel_h
            },
        );
        self.album_editor_rect = panel;
        let panel_radius = f64::from(theme.tokens.radii.lg);
        ui::surface::shadow(
            scene,
            transform,
            panel,
            panel_radius,
            &theme.tokens.elevation.e3,
        );
        surface::card(
            scene,
            transform,
            panel,
            f64::from(theme.tokens.radii.lg),
            theme.tokens.colors.surfaces.raised,
        );
        text_renderer.draw_text_weighted(
            scene,
            if self.album_editor_creating {
                "New album"
            } else {
                "Edit album"
            },
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 16.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Album name",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 66.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        self.album_name_input.apply_layout_external(Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 94.0,
            panel.x1 - 16.0,
            panel.y0 + 170.0,
        ));
        self.album_name_input
            .draw(scene, transform, text_renderer, theme, presence);
        text_renderer.draw_text_weighted(
            scene,
            "Choose photos",
            11.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 190.0)),
            Some((panel.width() - 180.0) as f32),
            TextWeight::Regular,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!("{} selected", self.album_editor_members.len()),
            12.0,
            theme.text_primary,
            transform * Affine::translate((panel.x1 - 146.0, panel.y0 + 190.0)),
            Some(130.0),
            TextWeight::Medium,
        );
        let page_y = panel.y0 + 222.0;
        let previous = Rect::new(panel.x0 + 16.0, page_y, panel.x0 + 132.0, page_y + 48.0);
        let next = Rect::new(panel.x0 + 140.0, page_y, panel.x0 + 256.0, page_y + 48.0);
        for (hit, rect, label) in [
            (AlbumEditorHit::PreviousPage, previous, "Previous"),
            (AlbumEditorHit::NextPage, next, "Next"),
        ] {
            let enabled = match hit {
                AlbumEditorHit::PreviousPage => self.can_page_library(false),
                AlbumEditorHit::NextPage => self.can_page_library(true),
                _ => false,
            };
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(rect, label)
                    .style(ButtonStyle::Neutral)
                    .radius(10.0)
                    .hovered(enabled && rect.contains(self.pointer))
                    .disabled(!enabled),
            );
            if enabled {
                self.album_editor_hits.push((hit, rect));
            }
        }
        self.library_cards.clear();
        let row_top = panel.y0 + 286.0;
        for (index, item) in self.library_items.iter().take(2).enumerate() {
            let row_y = row_top + index as f64 * 84.0;
            let row = Rect::new(panel.x0 + 16.0, row_y, panel.x1 - 16.0, row_y + 74.0);
            let selected = self.album_editor_members.contains(&item.id);
            scene.fill(
                Fill::NonZero,
                transform,
                if selected {
                    theme.tokens.colors.neutral[3]
                } else {
                    theme.tokens.colors.surfaces.window_card
                },
                None,
                &row,
            );
            scene.stroke(
                &Stroke::new(if selected { 2.0 } else { 1.0 }),
                transform,
                if selected {
                    theme.tokens.colors.accent[6]
                } else {
                    theme.border_standard
                },
                None,
                &row,
            );
            let toggle = Rect::new(row.x0 + 10.0, row.y0 + 18.0, row.x0 + 48.0, row.y0 + 56.0);
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.border_standard,
                None,
                &toggle,
            );
            if selected {
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(toggle.center(), (20.0, 20.0)),
                    Icon::Check,
                    theme.tokens.colors.accent[6],
                    1.8,
                );
            }
            text_renderer.draw_text_weighted(
                scene,
                &item.name,
                11.0,
                theme.text_primary,
                transform * Affine::translate((row.x0 + 60.0, row.y0 + 12.0)),
                Some((row.width() - 72.0) as f32),
                TextWeight::Medium,
            );
            self.library_cards
                .push((item.id.clone(), row, item.favorite));
        }
        let action_h = 60.0;
        let action_y = panel.y1 - action_h - 16.0;
        let cancel = Rect::new(
            panel.x0 + 16.0,
            action_y,
            panel.x0 + 166.0,
            action_y + action_h,
        );
        let save = Rect::new(
            panel.x1 - 166.0,
            action_y,
            panel.x1 - 16.0,
            action_y + action_h,
        );
        let delete = Rect::new(
            panel.x0 + 16.0,
            action_y - action_h - 10.0,
            panel.x1 - 16.0,
            action_y - 10.0,
        );
        for (hit, rect, label, style, focused) in [
            (
                AlbumEditorHit::Cancel,
                cancel,
                "Cancel",
                ButtonStyle::Neutral,
                self.album_editor_focus == 1,
            ),
            (
                AlbumEditorHit::Save,
                save,
                "Save",
                ButtonStyle::Primary,
                self.album_editor_focus == if self.album_editor_creating { 2 } else { 3 },
            ),
        ] {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(rect, label)
                    .style(style)
                    .radius(12.0)
                    .hovered(rect.contains(self.pointer))
                    .focused(focused),
            );
            self.album_editor_hits.push((hit, rect));
        }
        if !self.album_editor_creating {
            let hit = if self.album_delete_confirm {
                AlbumEditorHit::ConfirmDelete
            } else {
                AlbumEditorHit::Delete
            };
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(
                    delete,
                    if self.album_delete_confirm {
                        "Delete album?"
                    } else {
                        "Delete album"
                    },
                )
                .style(ButtonStyle::Danger)
                .radius(12.0)
                .hovered(delete.contains(self.pointer))
                .focused(self.album_editor_focus == 2),
            );
            self.album_editor_hits.push((hit, delete));
        }
    }

    /// A focused details sheet built from shared `TextInputs`. It intentionally
    /// asks for labels rather than offering facial analysis: people metadata is
    /// explicit, editable, and replicated only after Save.
    #[allow(
        clippy::too_many_lines,
        reason = "the metadata sheet paints four shared fields and its actions in one auditable geometry block"
    )]
    fn draw_metadata_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        if !self.metadata_editor {
            self.metadata_editor_hits.clear();
            return;
        }
        for input in [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
            &mut self.metadata_place_input,
            &mut self.metadata_latitude_input,
            &mut self.metadata_longitude_input,
        ] {
            input.apply_layout_external(Rect::ZERO);
        }
        for (_, button) in &mut self.metadata_editor_buttons {
            button.apply_layout_external(Rect::ZERO);
        }
        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 720.0;
        let panel_w = if narrow {
            (bounds.width() - 16.0).max(0.0)
        } else {
            536.0_f64.min(bounds.width() - 28.0)
        };
        // On a narrow strip this is a true detail sheet, not an awkwardly
        // translucent sidebar over an interactive photo grid. The shared
        // scrim keeps the context visible while presenting one focus scope.
        if narrow {
            fill_scrim(scene, transform, bounds, 0.42, theme);
        }
        let panel_bottom = (bounds.y0 + 700.0).min(bounds.y1 - 8.0);
        let panel = Rect::new(
            if narrow {
                bounds.x0 + 8.0
            } else {
                bounds.x1 - panel_w - 14.0
            },
            bounds.y0 + 8.0,
            bounds.x1 - if narrow { 8.0 } else { 14.0 },
            panel_bottom,
        );
        surface::card(
            scene,
            transform,
            panel,
            f64::from(theme.tokens.radii.lg),
            theme.tokens.colors.surfaces.raised,
        );

        let count = self.metadata_editor_ids.len();
        let title = if count == 1 {
            "Photo details".to_owned()
        } else {
            format!("Edit {count} photographs")
        };
        let subtitle = if count == 1 {
            "Organize this photograph or correct when and where it was taken."
        } else {
            "Only fields you change are applied. Mixed values stay untouched."
        };
        let scale = f64::from(theme.text_scale);
        let inset = SHEET_INSET;
        let inner_width = (panel.width() - 2.0 * inset).max(0.0) as f32;
        let title_h = f64::from(
            text_renderer
                .measure_weighted(&title, 20.0, Some(inner_width), TextWeight::Medium)
                .1,
        );
        let subtitle_h = f64::from(
            text_renderer
                .measure_weighted(subtitle, 11.0, Some(inner_width), TextWeight::Regular)
                .1,
        );
        let title_y = panel.y0 + SHEET_TOP_PAD;
        let subtitle_y = title_y + title_h + SHEET_TITLE_GAP;
        text_renderer.draw_text_weighted(
            scene,
            &title,
            20.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + inset, title_y)),
            Some(inner_width),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            subtitle,
            11.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + inset, subtitle_y)),
            Some(inner_width),
            TextWeight::Regular,
        );

        let tab_y = subtitle_y + subtitle_h + SHEET_SUBTITLE_GAP;
        let tab_h = 40.0 * scale;
        let tab_gap = 8.0;
        let tab_w = (panel.width() - 40.0 - tab_gap) * 0.5;
        let mut hits = Vec::new();
        for (section, hit, _label, x) in [
            (
                MetadataEditorSection::Organize,
                MetadataEditorHit::Organize,
                "Organize",
                panel.x0 + 20.0,
            ),
            (
                MetadataEditorSection::TimeAndPlace,
                MetadataEditorHit::TimeAndPlace,
                "Time & place",
                panel.x0 + 20.0 + tab_w + tab_gap,
            ),
        ] {
            let rect = Rect::new(x, tab_y, x + tab_w, tab_y + tab_h);
            if let Some((_, button)) = self
                .metadata_editor_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_style(if self.metadata_editor_section == section {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Neutral
                });
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, theme, presence);
            }
        }

        let fields_top = tab_y + tab_h + SHEET_FIELD_GAP * scale;
        let footer_y = panel.y1 - SHEET_BUTTON_H * scale - SHEET_INSET;
        let body = Rect::new(
            panel.x0,
            fields_top,
            panel.x1,
            (footer_y - SHEET_BODY_FOOTER_GAP).max(fields_top),
        );
        self.metadata_editor_body = body;
        let label_h = f64::from(
            text_renderer
                .measure_weighted(
                    "Captured · Mixed — unchanged",
                    11.0,
                    Some((inner_width - 100.0 * theme.text_scale).max(1.0)),
                    TextWeight::Medium,
                )
                .1,
        )
        .max(36.0 * scale);
        let label_gap = SHEET_LABEL_GAP * scale;
        let input_h = SHEET_INPUT_H * scale;
        let field_gap = SHEET_FIELD_GAP * scale;
        let row_h = label_h + label_gap + input_h + field_gap;
        let map_h = 154.0 * scale;
        let time_and_place = self.metadata_editor_section == MetadataEditorSection::TimeAndPlace;
        let content_h = 3.0 * row_h
            + if time_and_place {
                map_h + field_gap
            } else {
                0.0
            };
        if std::mem::take(&mut self.metadata_editor_reveal_focus) {
            let focused_row_top = match (time_and_place, self.metadata_editor_focus) {
                (false, focus @ 0..=2) | (true, focus @ 0..=1) => Some(focus as f64 * row_h),
                (true, 2 | 3) => Some(2.0 * row_h + map_h + field_gap),
                _ => None,
            };
            if let Some(top) = focused_row_top {
                let bottom = top + row_h - field_gap;
                if top < self.metadata_editor_scroll {
                    self.metadata_editor_scroll = top;
                } else if bottom > self.metadata_editor_scroll + body.height() {
                    self.metadata_editor_scroll = bottom - body.height();
                }
            }
        }
        self.metadata_editor_scroll = self
            .metadata_editor_scroll
            .clamp(0.0, (content_h - body.height()).max(0.0));
        let fields_y = fields_top - self.metadata_editor_scroll;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        let label_and_input =
            |scene: &mut Scene,
             text_renderer: &mut TextRenderer,
             input: &mut TextInput,
             label: &str,
             mixed: bool,
             clear_hit: MetadataEditorHit,
             force_clear: bool,
             rect: Rect,
             buttons: &mut Vec<(MetadataEditorHit, Button)>| {
                let visible_label = if mixed {
                    format!("{label} · Mixed — unchanged")
                } else {
                    label.to_owned()
                };
                text_renderer.draw_text_weighted(
                    scene,
                    &visible_label,
                    11.0,
                    theme.text_dim,
                    transform * Affine::translate((rect.x0, rect.y0)),
                    Some((rect.width() - 100.0 * scale).max(1.0) as f32),
                    TextWeight::Medium,
                );
                let clear = Rect::new(
                    rect.x1 - 92.0 * scale,
                    rect.y0,
                    rect.x1,
                    rect.y0 + 36.0 * scale,
                );
                if let Some((_, button)) = buttons
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == clear_hit)
                {
                    button.set_style(if force_clear {
                        ButtonStyle::Danger
                    } else {
                        ButtonStyle::Ghost
                    });
                    button.apply_layout_external(clear);
                    button.draw(scene, transform, text_renderer, theme, presence);
                }
                input.apply_layout_external(Rect::new(
                    rect.x0,
                    rect.y0 + label_h + label_gap,
                    rect.x1,
                    rect.y1,
                ));
                input.draw(scene, transform, text_renderer, theme, presence);
            };

        match self.metadata_editor_section {
            MetadataEditorSection::Organize => {
                for (index, (field, (mixed, force_clear))) in [
                    (
                        "Folder",
                        &mut self.metadata_folder_input,
                        MetadataEditorHit::ClearFolder,
                    ),
                    (
                        "People",
                        &mut self.metadata_people_input,
                        MetadataEditorHit::ClearPeople,
                    ),
                    (
                        "Tags",
                        &mut self.metadata_tags_input,
                        MetadataEditorHit::ClearTags,
                    ),
                ]
                .into_iter()
                .zip(
                    self.metadata_mixed
                        .into_iter()
                        .zip(self.metadata_force_clear),
                )
                .enumerate()
                {
                    let (label, input, hit) = field;
                    let y = fields_y + index as f64 * row_h;
                    label_and_input(
                        scene,
                        text_renderer,
                        input,
                        label,
                        mixed,
                        hit,
                        force_clear,
                        Rect::new(panel.x0 + 20.0, y, panel.x1 - 20.0, y + row_h - field_gap),
                        &mut self.metadata_editor_buttons,
                    );
                }
            }
            MetadataEditorSection::TimeAndPlace => {
                // Location deserves a real pick surface, not the old 58px
                // strip that made every coordinate edit feel accidental.
                // Keep enough fixed room for the date, place and coordinate
                // fields first; the map absorbs the responsive remainder.
                label_and_input(
                    scene,
                    text_renderer,
                    &mut self.metadata_captured_at_input,
                    "Captured",
                    self.metadata_mixed[3],
                    MetadataEditorHit::ClearCapturedAt,
                    self.metadata_force_clear[3],
                    Rect::new(
                        panel.x0 + 20.0,
                        fields_y,
                        panel.x1 - 20.0,
                        fields_y + row_h - field_gap,
                    ),
                    &mut self.metadata_editor_buttons,
                );
                label_and_input(
                    scene,
                    text_renderer,
                    &mut self.metadata_place_input,
                    "Place name",
                    self.metadata_mixed[4],
                    MetadataEditorHit::ClearLocation,
                    self.metadata_force_clear[4],
                    Rect::new(
                        panel.x0 + 20.0,
                        fields_y + row_h,
                        panel.x1 - 20.0,
                        fields_y + row_h * 2.0 - field_gap,
                    ),
                    &mut self.metadata_editor_buttons,
                );
                let map = Rect::new(
                    panel.x0 + 20.0,
                    fields_y + row_h * 2.0,
                    panel.x1 - 20.0,
                    fields_y + row_h * 2.0 + map_h,
                );
                self.metadata_location_map_bounds = map;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.neutral[2],
                    None,
                    &map,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.border_standard,
                    None,
                    &map,
                );
                for step in 1..5 {
                    let x = map.x0 + map.width() * f64::from(step) / 5.0;
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        theme.border_standard,
                        None,
                        &Rect::new(x, map.y0, x + 1.0, map.y1),
                    );
                }
                for step in 1..3 {
                    let y = map.y0 + map.height() * f64::from(step) / 3.0;
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        theme.border_standard,
                        None,
                        &Rect::new(map.x0, y, map.x1, y + 1.0),
                    );
                }
                let center = map.center();
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Circle::new(center, 7.0),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    "Click to pin a location · saved only when you save changes",
                    10.0,
                    theme.text_primary,
                    transform * Affine::translate((map.x0 + 12.0, map.y0 + 10.0)),
                    Some((map.width() - 20.0) as f32),
                    TextWeight::Medium,
                );
                hits.push((MetadataEditorHit::PlaceOnMap, map.intersect(body)));
                let y = map.y1 + field_gap;
                let gap = 10.0;
                let field_w = (panel.width() - 40.0 - gap) * 0.5;
                for (label, input, x) in [
                    (
                        "Latitude",
                        &mut self.metadata_latitude_input,
                        panel.x0 + 20.0,
                    ),
                    (
                        "Longitude",
                        &mut self.metadata_longitude_input,
                        panel.x0 + 20.0 + field_w + gap,
                    ),
                ] {
                    let visible_label = if self.metadata_mixed[4] {
                        format!("{label} · Mixed")
                    } else {
                        label.to_owned()
                    };
                    text_renderer.draw_text_weighted(
                        scene,
                        &visible_label,
                        11.0,
                        theme.text_dim,
                        transform * Affine::translate((x, y)),
                        Some(field_w as f32),
                        TextWeight::Medium,
                    );
                    input.apply_layout_external(Rect::new(
                        x,
                        y + label_h + label_gap,
                        x + field_w,
                        y + row_h - field_gap,
                    ));
                    input.draw(scene, transform, text_renderer, theme, presence);
                }
            }
        }

        scene.pop_layer();
        scene.fill(
            Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &Rect::new(
                panel.x0 + 20.0,
                footer_y - 12.0,
                panel.x1 - 20.0,
                footer_y - 11.0,
            ),
        );
        let cancel = Rect::new(panel.x0 + 20.0, footer_y, panel.x0 + 142.0, panel.y1 - 16.0);
        let save = Rect::new(panel.x1 - 156.0, footer_y, panel.x1 - 20.0, panel.y1 - 16.0);
        let footer_focus = if self.metadata_editor_section == MetadataEditorSection::Organize {
            (3, 4)
        } else {
            (4, 5)
        };
        for (hit, rect, focused, style) in [
            (
                MetadataEditorHit::Cancel,
                cancel,
                self.metadata_editor_focus == footer_focus.0,
                ButtonStyle::Neutral,
            ),
            (
                MetadataEditorHit::Save,
                save,
                self.metadata_editor_focus == footer_focus.1,
                ButtonStyle::Primary,
            ),
        ] {
            if let Some((_, button)) = self
                .metadata_editor_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_style(style);
                button.set_focus(focused);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, theme, presence);
            }
        }
        self.metadata_editor_hits = hits;
    }
}

fn wrap_words(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let separator = usize::from(!line.is_empty());
        if !line.is_empty()
            && line
                .chars()
                .count()
                .saturating_add(separator)
                .saturating_add(word.chars().count())
                > max_chars
        {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn metadata_filter_id(folder: Option<&str>, person: Option<&str>, tag: Option<&str>) -> String {
    if let Some(folder) = folder {
        crate::actions::folder_resource_id(folder)
    } else if let Some(person) = person {
        crate::actions::person_resource_id(person)
    } else if let Some(tag) = tag {
        crate::actions::tag_resource_id(tag)
    } else {
        "all".to_owned()
    }
}

fn draw_centered_text(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    color: vello::peniko::Color,
    top_center: Point,
    max_width: f64,
) {
    let (measured, _) = ctx.text.measure_style(text, None, style);
    let width = f64::from(measured).min(max_width);
    let origin = Point::new(top_center.x - width * 0.5, top_center.y);
    ctx.text(text, style, color, origin, Some(max_width as f32));
}

/// The region of a `w`×`h` image `viewer` currently frames, in normalized
/// `0..1` coordinates — the inverse of the transform `ImageViewer::draw`
/// applies (contain-fit, then the widget's own zoom and pan). `None` when the
/// viewer or the image has a zero dimension.
fn visible_image_rect(viewer: &ImageViewer, w: f64, h: f64) -> Option<Rect01> {
    let rect = Widget::get_rect(viewer);
    let (rw, rh) = (rect.width(), rect.height());
    if rw <= 0.0 || rh <= 0.0 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let base = (rw / w).min(rh / h);
    let total_scale = base * f64::from(viewer.zoom());
    if total_scale <= 0.0 {
        return None;
    }
    let pan = viewer.pan();
    let (cx, cy) = ((rect.x0 + rect.x1) * 0.5, (rect.y0 + rect.y1) * 0.5);
    let (dw, dh) = (w * total_scale, h * total_scale);
    let dx = cx - dw * 0.5 + pan.x * total_scale;
    let dy = cy - dh * 0.5 + pan.y * total_scale;

    let x0 = ((rect.x0 - dx) / total_scale / w).clamp(0.0, 1.0);
    let x1 = ((rect.x1 - dx) / total_scale / w).clamp(0.0, 1.0);
    let y0 = ((rect.y0 - dy) / total_scale / h).clamp(0.0, 1.0);
    let y1 = ((rect.y1 - dy) / total_scale / h).clamp(0.0, 1.0);
    Some(Rect01 {
        x: x0,
        y: y0,
        w: (x1 - x0).max(0.0),
        h: (y1 - y0).max(0.0),
    })
}

/// Keep compact card chrome single-line.  Character iteration, rather than a
/// byte slice, preserves Unicode names in both painting and accessibility.
fn compact_label(label: &str, max_chars: usize) -> String {
    let mut chars = label.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Build one toolbar button in the presentation `face` asks for.
///
/// A glyph-only button keeps its words as the tooltip, which
/// [`ui::widget::Button`] also adopts as its accessible name — so dropping the
/// label costs the pointer user a hover and the screen-reader user nothing.
fn build_button(
    hit: Hit,
    key: &str,
    icon: Icon,
    face: Face,
    btn_h: f32,
    pending: &Rc<Cell<Option<Hit>>>,
) -> Button {
    let label = tr(key);
    let slot = pending.clone();
    let shown = match face {
        Face::Labelled => label.as_str(),
        Face::IconOnly | Face::LargeOverflow => "",
    };
    // “Library” has a measured 104 px compact face (including icon and
    // padding); the conservative generic estimate is 120 px. Keeping the
    // compact value prevents the first phone button from being pushed a few
    // pixels beyond the left edge while retaining room for every glyph.
    let width = if key == "photos-library" && matches!(face, Face::Labelled) {
        104.0
    } else {
        item_width(face, &label, btn_h)
    };
    let button = Button::new(key, shown, None);
    // The large-text overflow face paints a deliberately smaller “More”
    // caption below. An icon-only Button centres its glyph in the entire
    // word-sized slot, so retaining the ellipsis here would paint it through
    // that caption instead of beside it.
    let button = if matches!(face, Face::LargeOverflow) {
        button
    } else {
        button.with_icon(icon)
    };
    button
        .with_size(width, btn_h)
        .tooltip(label.clone())
        .with_accessible_name(&label)
        .on_effect(move || slot.set(Some(hit)))
}

/// A local navigation button in the album sheet. Membership paging is device
/// presentation state, so its callback only posts local intent; it never
/// mutates the document from inside the widget.
fn build_album_page_button(
    hit: AlbumEditorHit,
    id: &str,
    label: &str,
    pending: &Rc<Cell<Option<AlbumEditorHit>>>,
) -> Button {
    let slot = pending.clone();
    Button::new(id, label, None)
        .with_size(92.0, 44.0)
        .tooltip(label)
        .with_accessible_name(label)
        .on_effect(move || slot.set(Some(hit)))
}

fn build_album_page_buttons(
    pending: &Rc<Cell<Option<AlbumEditorHit>>>,
) -> Vec<(AlbumEditorHit, Button)> {
    [
        (
            AlbumEditorHit::PreviousPage,
            "photos-album-previous-page",
            "Previous",
        ),
        (AlbumEditorHit::NextPage, "photos-album-next-page", "Next"),
    ]
    .into_iter()
    .map(|(hit, id, label)| (hit, build_album_page_button(hit, id, label, pending)))
    .collect()
}

/// Native controls layered over the custom metadata inspector. The inspector
/// owns responsive geometry; `Button` owns interaction states, focus, action
/// publication, tooltips, and the accessible name. This keeps the sheet from
/// becoming a painted collection of fake controls merely because it is docked
/// over the photo canvas.
fn build_metadata_editor_buttons(
    pending: &Rc<Cell<Option<MetadataEditorHit>>>,
) -> Vec<(MetadataEditorHit, Button)> {
    [
        (MetadataEditorHit::Organize, "organize", "Organize"),
        (
            MetadataEditorHit::TimeAndPlace,
            "time-place",
            "Time & place",
        ),
        (
            MetadataEditorHit::ClearFolder,
            "clear-folder",
            "Clear folder",
        ),
        (
            MetadataEditorHit::ClearPeople,
            "clear-people",
            "Clear people",
        ),
        (MetadataEditorHit::ClearTags, "clear-tags", "Clear tags"),
        (
            MetadataEditorHit::ClearCapturedAt,
            "clear-captured-at",
            "Clear capture time",
        ),
        (
            MetadataEditorHit::ClearLocation,
            "clear-location",
            "Clear location",
        ),
        (
            MetadataEditorHit::PlaceOnMap,
            "place-on-map",
            "Set location from offline map",
        ),
        (MetadataEditorHit::Cancel, "cancel", "Cancel"),
        (MetadataEditorHit::Save, "save", "Save changes"),
    ]
    .into_iter()
    .map(|(hit, id, label)| {
        let slot = Rc::clone(pending);
        (
            hit,
            Button::new(&format!("photos-metadata-{id}"), label, None)
                .tooltip(label)
                .with_accessible_name(label)
                .on_effect(move || slot.set(Some(hit))),
        )
    })
    .collect()
}

fn build_library_header_button(id: &str, label: &str) -> Button {
    Button::new(id, label, None)
        .with_size(142.0, 44.0)
        .tooltip(label)
        .with_accessible_name(label)
}

fn build_library_album_buttons() -> Vec<(AlbumEditorHit, Button)> {
    [
        (AlbumEditorHit::New, "photos-library-new-album", "New album"),
        (
            AlbumEditorHit::Edit,
            "photos-library-edit-album",
            "Edit album",
        ),
    ]
    .into_iter()
    .map(|(hit, id, label)| (hit, build_library_header_button(id, label)))
    .collect()
}

fn build_collection_page_buttons(
    pending: &Rc<Cell<Option<CollectionPageHit>>>,
) -> Vec<(CollectionPageHit, Button)> {
    [
        (
            CollectionPageHit::AlbumsPrevious,
            "photos-albums-previous",
            "Previous",
        ),
        (CollectionPageHit::AlbumsNext, "photos-albums-next", "Next"),
        (
            CollectionPageHit::PeoplePrevious,
            "photos-people-previous",
            "Previous",
        ),
        (CollectionPageHit::PeopleNext, "photos-people-next", "Next"),
        (
            CollectionPageHit::PlacesPrevious,
            "photos-places-previous",
            "Previous",
        ),
        (CollectionPageHit::PlacesNext, "photos-places-next", "Next"),
    ]
    .into_iter()
    .map(|(hit, id, label)| {
        let slot = pending.clone();
        (
            hit,
            Button::new(id, label, None)
                .with_size(88.0, 40.0)
                .tooltip(label)
                .with_accessible_name(label)
                .on_effect(move || slot.set(Some(hit))),
        )
    })
    .collect()
}

fn build_directory_import_buttons(
    pending: &Rc<Cell<Option<DirectoryStatusHit>>>,
) -> Vec<(DirectoryStatusHit, Button)> {
    [
        (
            DirectoryStatusHit::Stop,
            "photos-directory-stop",
            "Stop",
            Icon::Square,
        ),
        (
            DirectoryStatusHit::Details,
            "photos-directory-details",
            "Details",
            Icon::List,
        ),
        (
            DirectoryStatusHit::Dismiss,
            "photos-directory-dismiss",
            "Dismiss",
            Icon::X,
        ),
    ]
    .into_iter()
    .map(|(hit, id, label, icon)| {
        let button = Button::new(id, label, None)
            .with_icon(icon)
            .with_size(104.0, 40.0)
            .tooltip(label)
            .with_accessible_name(label);
        let button = if hit == DirectoryStatusHit::Details {
            let slot = pending.clone();
            button.on_view(move || slot.set(Some(hit)))
        } else {
            let slot = pending.clone();
            button.on_effect(move || slot.set(Some(hit)))
        };
        (hit, button)
    })
    .collect()
}

/// Shared retained controls placed over the custom timeline geometry.  The
/// timeline owns placement, while Button owns its visual states, names and
/// minimum target treatment.
fn build_timeline_density_buttons(
    pending: &Rc<Cell<Option<TimelineHit>>>,
) -> Vec<(TimelineHit, Button)> {
    [
        (
            TimelineHit::ZoomOut,
            "photos-timeline-density-out",
            "Show fewer photographs",
            Icon::Minimize2,
        ),
        (
            TimelineHit::ZoomIn,
            "photos-timeline-density-in",
            "Show more photographs",
            Icon::Plus,
        ),
    ]
    .into_iter()
    .map(|(hit, id, name, icon)| {
        let slot = pending.clone();
        (
            hit,
            Button::new(id, "", None)
                .with_icon(icon)
                .with_size(44.0, 44.0)
                .tooltip(name)
                .with_accessible_name(name)
                .on_effect(move || slot.set(Some(hit))),
        )
    })
    .collect()
}

fn build_library_analysis_buttons(
    pending: &Rc<Cell<Option<LibraryAnalysisHit>>>,
) -> Vec<(LibraryAnalysisHit, Button)> {
    [
        (
            LibraryAnalysisHit::Start,
            "photos-analysis-start",
            "Analyze unprocessed photos",
            Icon::Sparkles,
        ),
        (
            LibraryAnalysisHit::Pause,
            "photos-analysis-pause",
            "Pause after this photo",
            Icon::Pause,
        ),
        (
            LibraryAnalysisHit::Resume,
            "photos-analysis-resume",
            "Resume analysis",
            Icon::Play,
        ),
        (
            LibraryAnalysisHit::RetryFailures,
            "photos-analysis-retry",
            "Retry failed photos",
            Icon::Refresh,
        ),
    ]
    .into_iter()
    .map(|(hit, id, label, icon)| {
        let slot = pending.clone();
        (
            hit,
            Button::new(id, label, None)
                .with_icon(icon)
                .with_size(220.0, 44.0)
                .tooltip(label)
                .with_accessible_name(label)
                .on_effect(move || slot.set(Some(hit))),
        )
    })
    .collect()
}

/// Wrap an RGBA buffer as the image type the scene draws.
fn to_peniko(img: &Rgba) -> ImageData {
    pixels::to_peniko(img)
}

/// A local decode-cache key. The content hash, when present, is immutable;
/// include the legacy path too so older documents remain distinguishable.
fn decode_key(open: &OpenImage) -> String {
    format!(
        "{}\0{}",
        open.path,
        open.content.as_deref().unwrap_or_default()
    )
}

fn thumbnail_key(open: &OpenImage) -> String {
    format!("{}:320x180", decode_key(open))
}

/// Turn the portable capture value into a calm timeline label without parsing
/// it into the viewer's timezone. A collaborator who only supplied a date
/// sees exactly that date; an ISO timestamp naturally groups with its day.
/// The stored value remains untouched for ordering, search, and resources.
fn timeline_date_label(captured_at: &str) -> String {
    let day = capture_day(captured_at);
    let Some((year, rest)) = day.split_once('-') else {
        return day;
    };
    let Some((month, day_of_month)) = rest.split_once('-') else {
        return day;
    };
    let month = match month {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return day,
    };
    if day_of_month.len() == 2 && day_of_month.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{month} {day_of_month}, {year}")
    } else {
        day
    }
}

fn collection_page_access_node(hit: CollectionPageHit, bounds: Rect, enabled: bool) -> AccessNode {
    let (name, target) = match hit {
        CollectionPageHit::AlbumsPrevious => ("Previous albums", "albums-previous"),
        CollectionPageHit::AlbumsNext => ("More albums", "albums-next"),
        CollectionPageHit::PeoplePrevious => ("Previous people", "people-previous"),
        CollectionPageHit::PeopleNext => ("More people", "people-next"),
        CollectionPageHit::PlacesPrevious => ("Previous places", "places-previous"),
        CollectionPageHit::PlacesNext => ("More places", "places-next"),
    };
    AccessNode::new(Role::Button)
        .with_name(name)
        .with_description("Browse the complete collection one bounded page at a time")
        .with_bounds(bounds)
        .with_state(AccessState {
            focusable: enabled,
            disabled: !enabled,
            ..Default::default()
        })
        .with_action_target(format!("photos:collection-page:{target}"))
}

/// Preserve a capture day as the timeline's chronological identity. Display
/// formatting must never become the sort/group key: doing so would collapse
/// every "Jun" from different years into one ambiguous bucket.
fn capture_day(captured_at: &str) -> String {
    captured_at
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("Undated")
        .to_owned()
}

/// Read at most the encoded-image budget plus one sentinel byte. A hostile
/// file can otherwise exhaust memory before the decoder gets a chance to
/// enforce its decoded-pixel limit.
impl PhotosApp {
    /// Import the photographs a contents chooser delivered.
    fn drain_contents_picker(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.contents_picker.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::ContentsOutcome::Failed("the file chooser stopped responding".into())
            }
        };
        self.contents_picker = None;
        let stale = std::mem::take(&mut self.picker_stale);
        self.import_content = None;
        let invocation = self.resource_invocation.take();
        let result = match outcome {
            ui::portal::ContentsOutcome::Picked(files) if stale => {
                let message = format!(
                    "Selected {} photos; import was invalidated by changed document content",
                    files.len()
                );
                self.notice = Some(message.clone());
                Err(("photos.stale_selection", message))
            }
            ui::portal::ContentsOutcome::Picked(files) => {
                for file in files {
                    self.open_contents(doc, &file.name, &file.bytes);
                }
                if self.notice.is_none() {
                    Ok(())
                } else {
                    Err(("photos.open_failed", tr("photos-open-failed")))
                }
            }
            ui::portal::ContentsOutcome::Cancelled => {
                if let Some(invocation) = invocation {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("photo selection was cancelled".into()),
                    });
                }
                return;
            }
            ui::portal::ContentsOutcome::Failed(message) => {
                self.notice = Some(format!("{}: {message}", tr("photos-open-failed")));
                Err(("photos.portal_failed", message))
            }
        };
        if let Some(invocation) = invocation {
            self.resource_result = Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err((code, message)) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: code.into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
    }
}

fn bounded_file_bytes(path: &str) -> Option<Vec<u8>> {
    crate::files::read_native(std::path::Path::new(path), pixels::MAX_ENCODED_BYTES as u64)
}

impl PhotosApp {
    fn reconcile_content(&mut self, doc: &AutoCommit) {
        let current = LiveContent::read(doc);
        if self
            .import_content
            .as_ref()
            .is_some_and(|old| old != &current)
        {
            self.picker_stale = true;
            if let Some((invocation, _)) = self.pending_resource_open.take() {
                self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("Photo import invalidated by changed document content".into()),
                });
                self.import_content = None;
            }
        }
        if let Some(old) = &self.observed_content
            && old != &current
        {
            self.pending.take();
            self.album_page_pending.take();
            self.compact_more_open = false;
            self.library_cards.clear();
            self.library_details_cards.clear();
            let metadata_changed = self
                .metadata_editor_ids
                .iter()
                .any(|id| old.library.photo(id) != current.library.photo(id));
            let album_changed = self
                .album_editor_id
                .as_ref()
                .is_some_and(|id| old.library.album(id) != current.library.album(id))
                || self
                    .album_editor_members
                    .iter()
                    .any(|id| current.library.photo(id).is_none());
            if metadata_changed {
                self.cancel_metadata_editor();
                self.notice = Some("Photo changed; unsaved details discarded".into());
            }
            if album_changed {
                self.cancel_album_editor();
                self.notice =
                    Some("Album or membership changed; unsaved album changes discarded".into());
            }
            if self
                .active_album
                .as_ref()
                .is_some_and(|id| current.library.album(id).is_none())
            {
                self.active_album = None;
            }
        }
        self.observed_content = Some(current);
        // Recreating the viewer on changed immutable identity/edits also drops
        // an active drag before the next input can apply its stale viewport.
        self.refresh(doc);
    }

    /// Take completed thumbnail work without blocking the UI thread.
    fn drain_thumbnail_results(&mut self) {
        let Some(generation) = self.thumbnail_generation else {
            return;
        };
        let settled: Vec<(String, Option<thumbnail::ThumbState>)> = self
            .thumbnail_keys
            .iter()
            .filter_map(|(id, key)| match self.thumbnail_service.peek(key) {
                Some(state) if state.is_loading() => None,
                state => Some((id.clone(), state)),
            })
            .collect();
        for (id, state) in settled {
            self.thumbnail_keys.remove(&id);
            match state {
                // Dropped from the shared queue: forget it here too, so the
                // next schedule asks again.
                None => {
                    self.thumbnail_queued.remove(&id);
                }
                Some(state) => self.accept_thumbnail_result(ThumbnailResult {
                    generation,
                    id,
                    preview: state.ready().and_then(|preview| {
                        pixels::Rgba::new(preview.width, preview.height, preview.rgba.clone())
                    }),
                }),
            }
        }
    }

    /// Accept a worker result only when it still belongs to the materialised
    /// library slice. This is the generation fence that makes scrolling,
    /// filtering and collaboration safe while a decode is in flight.
    fn accept_thumbnail_result(&mut self, result: ThumbnailResult) {
        if Some(result.generation) != self.thumbnail_generation
            || !self.thumbnail_visible_ids.contains(&result.id)
        {
            return;
        }
        self.thumbnail_queued.remove(&result.id);
        if let Some(preview) = result.preview {
            self.library_thumbnails
                .insert(result.id, to_peniko(&preview));
        } else {
            self.thumbnail_unavailable.insert(result.id);
        }
    }

    /// Every toolkit button retained by Photos outside `Widget::children`.
    ///
    /// Photos lays these controls over several custom-painted surfaces, so
    /// the ordinary recursive widget plumbing cannot discover them. Keep the
    /// inventory in one place so resource publication, policy refresh,
    /// invocation draining, and operation dispatch cannot drift apart.
    fn retained_buttons(&self) -> Vec<&Button> {
        let mut buttons = Vec::new();
        buttons.extend(self.buttons.iter().map(|(_, button)| button));
        buttons.extend(
            self.directory_import_buttons
                .iter()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.library_analysis_buttons
                .iter()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.collection_page_buttons
                .iter()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.timeline_density_buttons
                .iter()
                .map(|(_, button)| button),
        );
        buttons.push(&self.library_select_button);
        buttons.extend(self.library_album_buttons.iter().map(|(_, button)| button));
        buttons.extend(self.album_page_buttons.iter().map(|(_, button)| button));
        buttons.extend(
            self.metadata_editor_buttons
                .iter()
                .map(|(_, button)| button),
        );
        buttons.extend(self.people_controls.iter().map(|control| &control.button));
        buttons
    }

    fn retained_buttons_mut(&mut self) -> Vec<&mut Button> {
        let mut buttons = Vec::new();
        buttons.extend(self.buttons.iter_mut().map(|(_, button)| button));
        buttons.extend(
            self.directory_import_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.library_analysis_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.collection_page_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.timeline_density_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.push(&mut self.library_select_button);
        buttons.extend(
            self.library_album_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(self.album_page_buttons.iter_mut().map(|(_, button)| button));
        buttons.extend(
            self.metadata_editor_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.people_controls
                .iter_mut()
                .map(|control| &mut control.button),
        );
        buttons
    }

    /// Apply callbacks after the host has admitted and dispatched an emitted
    /// control action. Pointer handling only queues the action; the callback
    /// runs after that event has returned, so `sync` is the common completion
    /// boundary for pointer, keyboard, accessibility, and agent activation.
    fn drain_action_control_pending(&mut self, doc: &mut AutoCommit) {
        self.drain_pending(doc);
        if let Some(hit) = self.directory_import_pending.take() {
            self.activate_directory_status(hit);
        }
        if let Some(hit) = self.library_analysis_pending.take() {
            self.activate_library_analysis(hit, doc);
        }
        if let Some(hit) = self.collection_page_pending.take() {
            self.page_collection(hit);
        }
        if let Some(hit) = self.timeline_density_pending.take() {
            self.adjust_timeline_density(matches!(hit, TimelineHit::ZoomIn));
        }
        if let Some(hit) = self.album_page_pending.take() {
            match hit {
                AlbumEditorHit::PreviousPage => self.page_library(false),
                AlbumEditorHit::NextPage => self.page_library(true),
                _ => {}
            }
        }
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit, doc);
        }
        let people = self.people_pending.borrow_mut().take();
        if let Some(intent) = people {
            self.handle_people_intent(intent, doc);
        }
    }

    /// Fill the small bounded worker queue each synchronization. This keeps
    /// decoding off the UI thread while avoiding a 24-frame wait for a single
    /// visible shelf to become useful after a scroll or first library open.
    fn schedule_library_thumbnail(&mut self, sources: Vec<LibraryPhoto>, document_changed: bool) {
        self.thumbnail_resource_keys = sources
            .iter()
            .map(|photo| (photo.id.clone(), thumbnail_key(&photo.image)))
            .collect();
        let ids: Vec<_> = sources
            .iter()
            .map(|photo| thumbnail_key(&photo.image))
            .collect();
        if document_changed || ids != self.thumbnail_visible_ids {
            let Some(next_generation) = self
                .thumbnail_generation
                .and_then(|generation| generation.checked_add(1))
            else {
                // Never reuse a generation token. Any result still in the
                // worker's queue is now untrusted, and no new work is sent.
                self.withdraw_thumbnail_requests(&[]);
                self.thumbnail_generation = None;
                self.thumbnail_visible_ids = ids;
                self.thumbnail_queued.clear();
                self.library_thumbnails.clear();
                self.thumbnail_unavailable.clear();
                return;
            };
            self.withdraw_thumbnail_requests(&ids);
            self.thumbnail_generation = Some(next_generation);
            self.thumbnail_visible_ids = ids;
            self.thumbnail_queued.clear();
            if document_changed {
                self.library_thumbnails.clear();
                // Bytes behind an unchanged address may have arrived, and a
                // legacy path may name new pixels: ask the pipeline afresh.
                for photo in &sources {
                    self.thumbnail_service
                        .forget(&thumbnail_request(&photo.image, &self.content).key());
                }
            }
            self.library_thumbnails
                .retain(|id, _| self.thumbnail_visible_ids.contains(id));
            self.thumbnail_unavailable
                .retain(|id| self.thumbnail_visible_ids.contains(id));
        }
        if self.thumbnail_generation.is_none() {
            return;
        }
        for photo in sources {
            // The shared queue is bounded, but its worker can take a job
            // between two requests. Without this independent accounting fence
            // a fast worker lets one sync enqueue an arbitrarily large visible
            // slice while `thumbnail_queued` continues to grow. Keep the whole
            // in-flight generation strictly bounded.
            if self.thumbnail_queued.len() >= THUMBNAIL_QUEUE_LIMIT {
                break;
            }
            if self
                .library_thumbnails
                .contains_key(&thumbnail_key(&photo.image))
                || self
                    .thumbnail_unavailable
                    .contains(&thumbnail_key(&photo.image))
                || self.thumbnail_queued.contains(&thumbnail_key(&photo.image))
            {
                continue;
            }
            let id = thumbnail_key(&photo.image);
            let request = thumbnail_request(&photo.image, &self.content);
            self.thumbnail_keys.insert(id.clone(), request.key());
            self.thumbnail_service.request(request);
            self.thumbnail_queued.insert(id);
        }
    }

    /// Withdraw queued previews that are no longer on screen. Work already
    /// running settles into the shared cache; its result is simply not taken.
    fn withdraw_thumbnail_requests(&mut self, still_visible: &[String]) {
        let service = &self.thumbnail_service;
        self.thumbnail_keys.retain(|id, key| {
            let keep = still_visible.contains(id);
            if !keep {
                service.cancel(key);
            }
            keep
        });
    }
}

impl Widget for PhotosApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.places_motion_reduced = reduced;
        if reduced {
            // Reduced motion keeps the camera operation immediate and every
            // marker fully present; only the decorative settle cue disappears.
            self.places_camera_feedback = None;
        }
    }

    fn needs_animation_frame(&self) -> bool {
        !self.places_motion_reduced
            && (self
                .places_camera_feedback
                .is_some_and(|feedback| !feedback.done(ui::time::now()))
                || self.people_detail_transition.is_animating(ui::time::now()))
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            crate::actions::PHOTOS_OPERATION_TYPE,
            RETAINED_CONTROL_OPERATION_TYPE,
        ]
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
        if let Some(started) = self.admit_resource_operation_with_document(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        if request.type_id == RETAINED_CONTROL_OPERATION_TYPE {
            for button in self.retained_buttons_mut() {
                if let Some(started) = button.start_operation(op, doc, path)? {
                    return Ok(Some(started));
                }
            }
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested && self.library_analysis.is_running_invocation(invocation) {
            self.pause_library_analysis();
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(
                    "photo analysis paused; the current local photograph may finish safely".into(),
                ),
            });
        }
        if cancellation_requested && self.active_analysis.remove(&invocation) {
            self.cancelled_analysis.insert(invocation);
            if self.active_people_scans.remove(&invocation) {
                self.pending_people_scans.remove(&invocation);
                "cancelled".clone_into(&mut self.people_scan_progress.status);
                self.people_scan_progress.save();
            }
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("local image analysis was cancelled".into()),
            });
        }
        if cancellation_requested
            && self
                .pending_resource_open
                .as_ref()
                .is_some_and(|(pending, _)| *pending == invocation)
        {
            self.pending_resource_open = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("photo opening was cancelled".into()),
            });
        }
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            // Dropping the receiver closes this app's portal request channel;
            // a late worker response can no longer open a file after the
            // caller has cancelled the action.
            self.picker = None;
            self.directory_picker = None;
            if let Some(import) = self.directory_import.as_mut() {
                import.cancel.store(true, Ordering::Relaxed);
                import.invocation = None;
                import.cancelled = true;
                import.pending.clear();
            }
            self.resource_invocation = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(
                    "folder indexing was cancelled; completed photographs are kept".into(),
                ),
            });
        }
        if self
            .resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "synchronization derives the bounded library, filters, search, and thumbnail work from one document snapshot"
    )]
    fn sync(&mut self, doc: &mut AppModel, path: &ModelPath) {
        let (doc, path) = ui::doc::document(doc, path);
        self.reconcile_content(doc);
        self.drain_action_control_pending(doc);
        self.drain_analysis_results(doc);
        if let Some((invocation, path)) = self.pending_resource_open.take() {
            self.import_content = None;
            self.open_path(doc, &path);
            self.resource_result = Some(if self.notice.is_none() {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.open_failed".into(),
                        message: tr("photos-open-failed"),
                        details: None,
                        retryable: false,
                    },
                }
            });
        }
        self.drain_picker(doc);
        self.drain_directory_import(doc);
        self.refresh(doc);
        self.library_search.sync(doc, path);
        if let Some(query) = self.library_search.take_dirty() {
            self.library_query = query;
            self.timeline_scroll_y = 0.0;
        }

        // An address can arrive before its bytes. Do not offer Copy, Save or
        // a blind crop against an empty canvas; the durable resource remains
        // available to a headless collaborator that deliberately chooses it.
        let has_image = self.has_image();
        let edited = !PhotosDoc::edits(doc).is_identity();
        for (hit, button) in &mut self.buttons {
            button.set_enabled(Self::enabled(*hit, has_image, edited));
        }
        let heads = doc.get_heads();
        let document_changed = heads != self.search_heads;
        if document_changed {
            self.search_refresh_error = match self.refresh_search_provider(doc) {
                Ok(()) => None,
                Err(error) => Some(format!("Photo search kept its previous index: {error}")),
            };
            self.thumbnail_unavailable.clear();
            self.search_heads.clone_from(&heads);
        }
        self.advance_library_query();
        self.status = self.status_text(doc);
        let library = PhotosDoc::library(doc);
        self.library_analysis_coverage = library.photos.iter().fold(
            (0usize, 0usize, 0usize),
            |(analyzed, available, awaiting), photo| {
                if photo.analysis.as_ref().is_some_and(|analysis| {
                    analysis
                        .vision_provenance_for_content(photo.image.content.as_deref())
                        .is_some()
                }) {
                    (analyzed + 1, available, awaiting)
                } else if crate::doc::bytes_of(&photo.image, &self.content).is_some() {
                    (analyzed, available + 1, awaiting)
                } else {
                    (analyzed, available, awaiting + 1)
                }
            },
        );
        self.selection_all_favorite = !self.selected_photo_ids.is_empty()
            && self
                .selected_photo_ids
                .iter()
                .all(|id| library.photo(id).is_some_and(|photo| photo.favorite));
        if self
            .selection_destination_album
            .as_ref()
            .is_some_and(|id| library.album(id).is_none())
        {
            self.selection_destination_album = None;
            self.selection_album_chooser = false;
            self.notice = Some("The selected destination album was removed elsewhere".to_owned());
        }
        let membership = self.active_album.as_ref().and_then(|id| {
            library
                .albums
                .iter()
                .find(|album| &album.id == id)
                .map(|album| &album.photo_ids)
        });
        let query_active = !self.library_query.trim().is_empty();
        let matches_filter = |photo: &LibraryPhoto| {
            let query_matches = !query_active || self.library_query_matches.contains(&photo.id);
            membership.is_none_or(|ids| ids.contains(&photo.id))
                && query_matches
                && (!self.active_favorites || photo.favorite)
                && self
                    .active_folder
                    .as_ref()
                    .is_none_or(|folder| photo.folder.as_ref() == Some(folder))
                && self
                    .active_person
                    .as_ref()
                    .is_none_or(|person| photo.people.contains(person))
                && self
                    .active_tag
                    .as_ref()
                    .is_none_or(|tag| photo.tags.contains(tag))
                && self.active_object.as_ref().is_none_or(|object| {
                    photo
                        .analysis
                        .as_ref()
                        .is_some_and(|analysis| analysis.objects.contains(object))
                })
                && self.active_pet.as_ref().is_none_or(|pet| {
                    photo
                        .analysis
                        .as_ref()
                        .is_some_and(|analysis| analysis.pets.contains(pet))
                })
        };
        self.library_total = library
            .photos
            .iter()
            .filter(|photo| matches_filter(photo))
            .count();
        self.library_favorite_count = library.photos.iter().filter(|photo| photo.favorite).count();
        self.library_offset = self
            .library_offset
            .min(self.library_total.saturating_sub(1));
        let mut timeline_items: Vec<_> = library
            .photos
            .iter()
            .filter(|photo| matches_filter(photo))
            .map(|photo| LibraryItem {
                id: photo.id.clone(),
                thumbnail_key: thumbnail_key(&photo.image),
                name: photo.image.name.clone(),
                favorite: photo.favorite,
                captured_at: photo
                    .captured_at
                    .as_deref()
                    .map_or_else(|| "Undated".to_owned(), capture_day),
            })
            .collect();
        // A timeline reads newest-first.  The id tie-breaker makes peers with
        // the same capture-day stable without making the date a mutable sort
        // key in the shared document.
        timeline_items.sort_by(|left, right| {
            let left_undated = left.captured_at == "Undated";
            let right_undated = right.captured_at == "Undated";
            left_undated
                .cmp(&right_undated)
                .then_with(|| right.captured_at.cmp(&left.captured_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        let projection_filter = format!(
            "{}\u{1f}{}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{}",
            self.library_query_indexed,
            self.library_query_matches.len(),
            self.active_album,
            self.active_folder,
            self.active_person,
            self.active_tag,
            self.active_object,
            self.active_pet,
            self.active_favorites
        );
        if self.timeline_projection_heads != heads
            || self.timeline_projection_filter != projection_filter
        {
            self.timeline_projection.reconcile(
                library
                    .photos
                    .iter()
                    .filter(|photo| matches_filter(photo))
                    .map(|photo| TimelinePhoto {
                        id: photo.id.clone(),
                        name: photo.image.name.clone(),
                        favorite: photo.favorite,
                        day: photo
                            .captured_at
                            .as_deref()
                            .map_or_else(|| "Undated".to_owned(), capture_day),
                        captured_at: photo.captured_at.clone().unwrap_or_default(),
                        aspect_ratio: if photo.image.width > 0 && photo.image.height > 0 {
                            f64::from(photo.image.width) / f64::from(photo.image.height)
                        } else {
                            1.5
                        },
                    }),
            );
            self.timeline_projection_heads = heads;
            self.timeline_projection_filter = projection_filter;
            self.library_total = self.timeline_projection.len();
        }
        // Markers are rebuilt from exact pixel day offsets during draw, once
        // the real viewport and responsive geometry are known.
        self.timeline_index_markers.clear();
        let visible_ids: HashSet<_> = self
            .timeline_projection
            .visible_photos(self.timeline_scroll_y, self.timeline_viewport_height)
            .into_iter()
            .map(|entry| entry.photo.id.clone())
            .collect();
        self.library_items = if visible_ids.is_empty() {
            timeline_items.into_iter().take(120).collect()
        } else {
            timeline_items
                .into_iter()
                .filter(|item| visible_ids.contains(&item.id))
                .take(120)
                .collect()
        };
        self.places_items = library
            .photos
            .iter()
            .filter(|photo| {
                photo
                    .location
                    .as_ref()
                    .is_some_and(crate::model::PhotoLocation::is_valid)
            })
            .cloned()
            .map(|photo| PlaceItem { photo })
            .collect();
        self.library_albums = library
            .albums
            .iter()
            .map(|album| (album.id.clone(), album.name.clone(), album.photo_ids.len()))
            .collect();
        self.library_album_covers = library
            .albums
            .iter()
            .map(|album| {
                let covers = album
                    .photo_ids
                    .iter()
                    .filter(|id| library.photo(id).is_some())
                    .take(4)
                    .cloned()
                    .collect();
                (album.id.clone(), covers)
            })
            .collect();
        let mut folders = std::collections::BTreeMap::<String, usize>::new();
        let mut people = std::collections::BTreeMap::<String, usize>::new();
        let mut tags = std::collections::BTreeMap::<String, usize>::new();
        for photo in &library.photos {
            if let Some(folder) = &photo.folder {
                *folders.entry(folder.clone()).or_default() += 1;
            }
            for person in &photo.people {
                *people.entry(person.clone()).or_default() += 1;
            }
            for tag in &photo.tags {
                *tags.entry(tag.clone()).or_default() += 1;
            }
        }
        self.people_approved = crate::people::approved_people(&library);
        self.library_folders = folders.into_iter().take(6).collect();
        self.library_people = people.into_iter().collect();
        let album_page_size = if self.library_destination == LibraryDestination::Albums
            && self.canvas_rect.width() >= 700.0
        {
            8
        } else {
            COLLECTION_PAGE_SIZE
        };
        self.albums_page = self
            .albums_page
            .min(self.library_albums.len().saturating_sub(1) / album_page_size);
        self.people_page = self
            .people_page
            .min(self.library_people.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.places_page = self
            .places_page
            .min(self.places_items.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.library_person_covers.clear();
        for (person, _) in self
            .library_people
            .iter()
            .skip(self.people_page * COLLECTION_PAGE_SIZE)
            .take(COLLECTION_PAGE_SIZE)
        {
            if let Some(photo) = library
                .photos
                .iter()
                .find(|photo| photo.people.iter().any(|label| label == person))
            {
                self.library_person_covers
                    .insert(person.clone(), photo.id.clone());
            }
        }
        // The dedicated People management surface may show approved people
        // outside the current browse-shelf page, so add their bounded covers
        // without changing collection-page navigation.
        for person in &self.people_approved {
            if self.library_person_covers.contains_key(&person.label) {
                continue;
            }
            if let Some(photo) = library
                .photos
                .iter()
                .find(|photo| photo.people.iter().any(|label| label == &person.label))
            {
                self.library_person_covers
                    .insert(person.label.clone(), photo.id.clone());
            }
        }
        self.library_tags = tags.into_iter().take(6).collect();
        // Request exactly the sorted, virtualised timeline slice, not the
        // document's original insertion order.  Otherwise a visible newest
        // photo can remain a placeholder while an off-screen old one decodes.
        let mut thumbnail_sources: Vec<_> = self
            .library_items
            .iter()
            .take(24)
            .filter_map(|item| library.photo(&item.id).cloned())
            .collect();
        if self.library_destination == LibraryDestination::Places {
            thumbnail_sources = self
                .places_items
                .iter()
                .take(24)
                .map(|item| item.photo.clone())
                .collect();
        } else if self.library_destination == LibraryDestination::Albums {
            thumbnail_sources = self
                .library_albums
                .iter()
                .skip(self.albums_page * album_page_size)
                .take(album_page_size)
                .flat_map(|(id, _, _)| self.library_album_covers.get(id).into_iter().flatten())
                .filter_map(|photo_id| library.photo(photo_id).cloned())
                .take(24)
                .collect();
        } else {
            for cover in self.library_person_covers.values() {
                if thumbnail_sources.iter().all(|photo| &photo.id != cover)
                    && let Some(photo) = library.photo(cover).cloned()
                {
                    thumbnail_sources.push(photo);
                }
            }
            thumbnail_sources.truncate(24);
        }
        if self.library_destination == LibraryDestination::People {
            for cluster in self.people_controller.all_clusters() {
                for photo_id in cluster.visible_photo_ids().into_iter().take(4) {
                    if thumbnail_sources.iter().all(|photo| photo.id != photo_id)
                        && let Some(photo) = library.photo(&photo_id).cloned()
                    {
                        thumbnail_sources.push(photo);
                    }
                }
            }
            thumbnail_sources.truncate(40);
        }
        self.drain_thumbnail_results();
        self.schedule_library_thumbnail(thumbnail_sources, document_changed);
        self.refresh_people_controls();
    }

    #[allow(
        clippy::unwrap_used,
        reason = "root's taffy node always builds from freshly-built children"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Refit against the width the last layout produced. The first frame
        // has no rect yet and lays out roomy; the frame after corrects it.
        // Large text uses the same labelled More overflow model as a phone:
        // a small, visible “More” doorway keeps the hidden commands
        // discoverable, while the four immediate navigation/edit controls
        // retain their accessible icon names without colliding.
        self.large_text_toolbar = theme.text_scale > 1.25;
        self.refit_toolbar(self.rect.width() as f32, btn_h(theme.text_scale));

        let bar = self.build_toolbar_node(taffy, theme, node_map);
        let search = self.build_library_search_node(taffy, theme, node_map);
        let canvas = self.build_canvas_node(taffy, theme, node_map);
        let status = self.build_status_node(taffy, theme.text_scale);

        let root = taffy
            .new_with_children(
                Style {
                    flex_direction: FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[bar, search, canvas, status],
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
        let here = Point::new(x, y);

        let children = taffy.children(node).unwrap();
        let Some(&bar) = children.first() else { return };
        let bl = taffy.layout(bar).unwrap();
        self.toolbar_rect = Rect::new(
            x + f64::from(bl.location.x),
            y + f64::from(bl.location.y),
            x + f64::from(bl.location.x) + f64::from(bl.size.width),
            y + f64::from(bl.location.y) + f64::from(bl.size.height),
        );
        let bar_origin = Point::new(x + f64::from(bl.location.x), y + f64::from(bl.location.y));
        let bar_children = taffy.children(bar).unwrap();
        self.separator_rects.clear();
        for (slot, &bn) in self.toolbar_slots().iter().zip(bar_children.iter()) {
            match *slot {
                Slot::Item(n) => {
                    if let Some((_, button)) = self.buttons.get_mut(n) {
                        button.apply_layout(taffy, bn, bar_origin);
                    }
                }
                Slot::Separator => {
                    let sl = taffy.layout(bn).unwrap();
                    let sx =
                        bar_origin.x + f64::from(sl.location.x) + f64::from(sl.size.width) / 2.0;
                    let sy = bar_origin.y + f64::from(sl.location.y);
                    self.separator_rects.push(Rect::new(
                        sx,
                        sy,
                        sx + 1.0,
                        sy + f64::from(sl.size.height),
                    ));
                }
                Slot::Spacer => {}
            }
        }

        if let Some(&search) = children.get(1) {
            let sl = taffy.layout(search).unwrap();
            let search_origin =
                Point::new(x + f64::from(sl.location.x), y + f64::from(sl.location.y));
            if let Some(&field) = taffy.children(search).unwrap_or_default().first() {
                self.library_search
                    .apply_layout(taffy, field, search_origin);
            }
        }
        if let Some(&canvas) = children.get(2) {
            let cl = taffy.layout(canvas).unwrap();
            let canvas_origin =
                Point::new(x + f64::from(cl.location.x), y + f64::from(cl.location.y));
            self.canvas_rect = Rect::new(
                canvas_origin.x,
                canvas_origin.y,
                canvas_origin.x + f64::from(cl.size.width),
                canvas_origin.y + f64::from(cl.size.height),
            );
            let people_surface = self.library_mode
                && (self.library_destination == LibraryDestination::People || self.people_mode);
            if people_surface {
                let canvas_children = taffy.children(canvas).unwrap_or_default();
                let show_name = self.people_selection.is_some() && !self.people_merge_mode;
                let mut index = 0usize;
                if show_name {
                    if let Some(&field) = canvas_children.get(index) {
                        self.people_name_input
                            .apply_layout(taffy, field, canvas_origin);
                    }
                    index += 1;
                }
                for control in &mut self.people_controls {
                    if let Some(&button) = canvas_children.get(index) {
                        control.button.apply_layout(taffy, button, canvas_origin);
                    }
                    index += 1;
                }
            } else if let Some(v) = self.viewer.as_mut() {
                v.apply_layout(taffy, canvas, here);
            }
        }
        if let Some(&status) = children.get(3) {
            let sl = taffy.layout(status).unwrap();
            self.status_rect = Rect::new(
                x + f64::from(sl.location.x),
                y + f64::from(sl.location.y),
                x + f64::from(sl.location.x) + f64::from(sl.size.width),
                y + f64::from(sl.location.y) + f64::from(sl.size.height),
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the top-level paint entry point keeps surface precedence and overlay order explicit"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        // App background. Every colour on this path comes from the theme.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        // The toolbar sits on the raised surface so it reads as chrome
        // against the canvas below it.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.toolbar_rect,
        );
        // …and is separated from the canvas by a real rule, not only by that
        // fill. Under the high-contrast palette `surfaces.raised` and
        // `surfaces.base` are both pure black by design — that theme
        // deliberately carries structure in borders rather than in fills — so
        // a toolbar defined only by its plate has no edge at all there. The
        // same rule above the status line, for the same reason.
        // The status line's rule is drawn only when the line has something in
        // it; an empty band would otherwise be fenced off as if it did.
        // Bar-role hairline under the toolbar plate (HIG §9.5); the status
        // band's rule sits on the base canvas, so it derives from that.
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::chrome_hairline(theme),
            None,
            &Rect::new(
                self.rect.x0,
                self.toolbar_rect.y1 - 1.0,
                self.rect.x1,
                self.toolbar_rect.y1,
            ),
        );
        if !self.status.is_empty() {
            let edge = self.status_rect.y0;
            scene.fill(
                Fill::NonZero,
                transform,
                surface::edge_on(theme.tokens.colors.surfaces.base),
                None,
                &Rect::new(self.rect.x0, edge - 1.0, self.rect.x1, edge),
            );
        }

        // Group rules: what opens a file, what adjusts it, what exports it —
        // derived against the raised plate they sit on.
        let group_rule = surface::edge_on(theme.tokens.colors.surfaces.raised);
        for sep in &self.separator_rects {
            scene.fill(Fill::NonZero, transform, group_rule, None, sep);
        }

        for slot in self.toolbar_slots() {
            let Slot::Item(index) = *slot else { continue };
            if let Some((_, button)) = self.buttons.get_mut(index) {
                button.draw(scene, transform, text_renderer, theme, presence);
            }
        }
        // Button chrome has a fixed 34px height.  Paint the one high-text
        // overflow word ourselves at a smaller, centred size so it remains
        // visibly labelled without letting a 200%-scaled button caption
        // bleed through the toolbar rule.
        if self.large_text_toolbar
            && let Some((_, more)) = self.buttons.get(9)
        {
            let rect = more.get_rect();
            text_renderer.draw_text_weighted(
                scene,
                "More",
                10.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 34.0, rect.y0 + 8.0)),
                Some((rect.width() - 40.0) as f32),
                TextWeight::Medium,
            );
        }
        if self.library_mode {
            self.library_search
                .draw(scene, transform, text_renderer, theme, presence);
            if self.library_destination == LibraryDestination::Albums {
                self.draw_albums(scene, transform, text_renderer, theme, presence);
            } else if self.library_destination == LibraryDestination::People || self.people_mode {
                self.draw_people(scene, transform, text_renderer, theme);
                if self.people_selection.is_some() && !self.people_merge_mode {
                    self.people_name_input
                        .draw(scene, transform, text_renderer, theme, presence);
                }
                for control in &mut self.people_controls {
                    control
                        .button
                        .draw(scene, transform, text_renderer, theme, presence);
                }
            } else if self.library_destination == LibraryDestination::Places {
                self.draw_places(scene, transform, text_renderer, theme, presence);
            } else if self.library_destination == LibraryDestination::Analysis {
                self.draw_library_analysis(scene, transform, text_renderer, theme, presence);
            } else {
                self.draw_library(scene, transform, text_renderer, theme, presence);
            }
        } else if let Some(v) = self.viewer.as_mut() {
            v.draw(scene, transform, text_renderer, theme, presence);
            self.draw_viewer_details(scene, transform, text_renderer, theme);
        } else {
            let (title, body) = self.empty_state_text();
            self.draw_empty_state(scene, transform, text_renderer, theme, &title, &body);
        }
        // This is deliberately after the canvas/library so the compact menu
        // is a real popup rather than a panel hidden behind the grid.
        self.draw_compact_more(scene, transform, text_renderer, theme);

        self.draw_directory_import_status(scene, transform, text_renderer, theme);

        if !self.status.is_empty() {
            let inner = self.status_rect;
            // Centre the line on its measured height inside a band sized by
            // `status_h` — at 2× the filename used to run off the bottom of a
            // fixed band anchored 4 px from its top (plan task 19).
            let budget = (inner.width() - 2.0 * f64::from(PAD)) as f32;
            let (_, line_h) = text_renderer.measure_weighted(
                &self.status,
                STATUS_TEXT_SIZE,
                Some(budget),
                TextWeight::Regular,
            );
            let y = inner.y0 + (inner.height() - f64::from(line_h)).max(0.0) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                &self.status,
                STATUS_TEXT_SIZE,
                theme.text_primary,
                transform * Affine::translate((inner.x0 + f64::from(PAD), y)),
                Some(budget),
                TextWeight::Regular,
            );
        }
        self.draw_directory_import_details(scene, transform, text_renderer, theme);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event entry point preserves explicit precedence between album chips, grid cards, toolbar, and viewer"
    )]
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.reconcile_content(ctx.state);
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            let next = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            if self.library_mode
                && self.library_destination == LibraryDestination::Places
                && let Some(previous) = self.places_drag_origin.replace(next)
            {
                self.set_places_viewport(self.places_viewport.panned(
                    next.x - previous.x,
                    next.y - previous.y,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                ));
            }
            self.pointer = next;
        }
        if self.directory_import_details_open {
            if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == winit::event::ElementState::Pressed
            {
                match &key.logical_key {
                    Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                        self.close_directory_details();
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.page_directory_details(false);
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        self.page_directory_details(true);
                    }
                    _ => return,
                }
                *ctx.handled = true;
                return;
            }
            if matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            ) {
                if let Some(layout) = self.directory_import_details_layout.clone() {
                    match layout.hit(self.pointer) {
                        Some(ModalHit::Close | ModalHit::Scrim | ModalHit::Action(2)) => {
                            self.close_directory_details();
                        }
                        Some(ModalHit::Action(0)) => {
                            self.page_directory_details(false);
                        }
                        Some(ModalHit::Action(1)) => {
                            self.page_directory_details(true);
                        }
                        Some(ModalHit::Card | ModalHit::Action(_)) | None => {}
                    }
                }
                *ctx.handled = true;
                return;
            }
            // A modal swallows pointer/keyboard input that it does not use;
            // cursor motion still reaches it so hover feedback remains live.
            if !matches!(event, winit::event::WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
                return;
            }
        }
        for (_, button) in &mut self.directory_import_buttons {
            let mut sink = None;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if self.directory_import_pending.get().is_none()
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .directory_import_buttons
                .iter()
                .find(|(_, button)| button.is_enabled() && button.get_rect().contains(self.pointer))
        {
            self.directory_import_pending.set(Some(*hit));
        }
        if let Some(hit) = self.directory_import_pending.take() {
            if self.activate_directory_status(hit) {
                *ctx.handled = true;
            }
            return;
        }
        if *ctx.handled {
            return;
        }
        let people_surface = self.library_mode
            && (self.library_destination == LibraryDestination::People || self.people_mode);
        if people_surface {
            if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == winit::event::ElementState::Pressed
                && matches!(key.logical_key, Key::Named(NamedKey::Escape))
            {
                self.handle_people_intent(PeopleIntent::Back, ctx.state);
                *ctx.handled = true;
                return;
            }
            let root_children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
            if let Some(&canvas) = root_children.get(2) {
                let canvas_children = ctx.taffy.children(canvas).unwrap_or_default();
                let show_name = self.people_selection.is_some() && !self.people_merge_mode;
                let mut index = 0usize;
                if show_name {
                    if let Some(&field) = canvas_children.get(index) {
                        self.people_name_input
                            .handle_event(event, &mut ctx.reborrow(ctx.path, field));
                    }
                    index += 1;
                    if self.people_name_input.take_submit() {
                        let intent = match self.people_selection {
                            Some(PeopleSelection::Approved(_)) => PeopleIntent::SaveName,
                            Some(PeopleSelection::Suggestion(_)) => PeopleIntent::ApproveSuggestion,
                            None => PeopleIntent::Back,
                        };
                        self.handle_people_intent(intent, ctx.state);
                        *ctx.handled = true;
                        return;
                    }
                }
                for control in &mut self.people_controls {
                    if let Some(&button) = canvas_children.get(index) {
                        control
                            .button
                            .handle_event(event, &mut ctx.reborrow(ctx.path, button));
                    }
                    index += 1;
                }
            }
            let pending = self.people_pending.borrow_mut().take();
            if let Some(intent) = pending {
                self.handle_people_intent(intent, ctx.state);
                *ctx.handled = true;
                return;
            }
            if *ctx.handled {
                return;
            }
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && self.places_map_bounds.contains(self.pointer)
            && !self
                .places_hits
                .iter()
                .any(|(_, rect)| rect.contains(self.pointer))
        {
            self.places_drag_origin = Some(self.pointer);
            *ctx.handled = true;
            return;
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            self.places_drag_origin = None;
        }
        if self.library_mode && self.library_destination == LibraryDestination::Analysis {
            for (_, button) in &mut self.library_analysis_buttons {
                let mut sink = None;
                button.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        focus_target: &mut sink,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
                if sink.is_some() {
                    *ctx.focus_target = sink;
                }
            }
            if let Some(hit) = self.library_analysis_pending.take() {
                self.activate_library_analysis(hit, ctx.state);
                *ctx.handled = true;
                return;
            }
            if matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            ) && let Some((photo_id, _)) = self
                .library_analysis_failure_hits
                .iter()
                .find(|(_, bounds)| bounds.contains(self.pointer))
                .cloned()
            {
                self.commit_library_action(
                    ctx.state,
                    &crate::actions::PhotosAction::SelectPhoto { photo_id },
                );
                self.library_mode = false;
                *ctx.handled = true;
                return;
            }
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                winit::event::MouseScrollDelta::PixelDelta(point) => point.y / 80.0,
            };
            self.zoom_places_by(amount * 0.25);
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && let Some(hit) = self.timeline_density_pending.take()
        {
            self.adjust_timeline_density(matches!(hit, TimelineHit::ZoomIn));
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .places_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            match hit {
                PlacesHit::Timeline => {
                    self.people_mode = false;
                    self.library_destination = LibraryDestination::Timeline;
                }
                PlacesHit::Albums => self.show_albums(),
                PlacesHit::People => {
                    self.people_mode = true;
                    self.library_destination = LibraryDestination::People;
                }
                PlacesHit::Places => {
                    self.people_mode = false;
                    self.library_destination = LibraryDestination::Places;
                }
                PlacesHit::Analysis => {
                    self.people_mode = false;
                    self.show_library_analysis();
                }
                PlacesHit::ToggleList => self.places_list_mode = !self.places_list_mode,
                PlacesHit::ZoomIn => self.zoom_places_by(1.0),
                PlacesHit::ZoomOut => self.zoom_places_by(-1.0),
                PlacesHit::Place(key) => {
                    let _ = self.open_place_collection(ctx.state, &key);
                }
                PlacesHit::Photo(id) => {
                    self.commit_library_action(
                        ctx.state,
                        &crate::actions::PhotosAction::SelectPhoto { photo_id: id },
                    );
                    self.library_mode = false;
                }
            }
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::ArrowLeft) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        44.0,
                        0.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        -44.0,
                        0.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        0.0,
                        44.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        0.0,
                        -44.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Character(key) if key == "+" || key == "=" => self.zoom_places_by(1.0),
                Key::Character(key) if key == "-" => self.zoom_places_by(-1.0),
                Key::Character(key) if key.eq_ignore_ascii_case("l") => {
                    self.places_list_mode = !self.places_list_mode;
                }
                _ => return,
            }
            *ctx.handled = true;
            return;
        }

        // The phone overflow is a real keyboard menu, not a pointer-only
        // drawing. Its entries are normal canonical toolbar actions, so this
        // just supplies focus traversal and activation for the compact locus.
        if self.compact_more_open
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.compact_more_open = false;
                }
                Key::Named(NamedKey::ArrowDown) => self.step_compact_more_focus(true),
                Key::Named(NamedKey::ArrowUp) => self.step_compact_more_focus(false),
                Key::Named(NamedKey::Enter) => {
                    if let Some(hit) = self.focused_compact_more_hit()
                        && self.is_toolbar_hit_enabled(hit)
                    {
                        self.pending.set(Some(hit));
                        self.compact_more_open = false;
                        self.drain_pending(ctx.state);
                    }
                }
                Key::Character(text) if text == " " => {
                    if let Some(hit) = self.focused_compact_more_hit()
                        && self.is_toolbar_hit_enabled(hit)
                    {
                        self.pending.set(Some(hit));
                        self.compact_more_open = false;
                        self.drain_pending(ctx.state);
                    }
                }
                _ => return,
            }
            *ctx.handled = true;
            return;
        }

        // The labeled compact overflow is painted above both the viewer and
        // the library.  Give it first refusal of clicks so an action in the
        // popup cannot also select a photograph behind it.
        if self.compact_more_open
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .compact_more_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            self.pending.set(Some(hit));
            self.compact_more_open = false;
            self.drain_pending(ctx.state);
            *ctx.handled = true;
            return;
        }

        // The library search is a real shared SearchField, placed directly
        // beneath the toolbar so OCR/descriptions and people/object facets
        // have one obvious entry point instead of another filter-chip row.
        if self.library_mode
            && let Some(&search_wrap) = ctx.taffy.children(ctx.node_id).unwrap_or_default().get(1)
            && let Some(&search) = ctx.taffy.children(search_wrap).unwrap_or_default().first()
        {
            self.library_search
                .handle_event(event, &mut ctx.reborrow(ctx.path, search));
            if *ctx.handled {
                return;
            }
        }

        // Give the shared chronological index first refusal only while its
        // narrow track/preview lane owns the pointer (or it holds a drag / AT
        // focus). Ordinary scrolling anywhere else remains the timeline's
        // smooth wheel path below.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
        {
            let index_owns_event = self.timeline_index.contains_interaction_point(self.pointer)
                || self.timeline_index.is_dragging()
                || self.timeline_index.is_focused();
            if index_owns_event
                && matches!(
                    event,
                    winit::event::WindowEvent::CursorMoved { .. }
                        | winit::event::WindowEvent::MouseInput { .. }
                        | winit::event::WindowEvent::MouseWheel { .. }
                        | winit::event::WindowEvent::KeyboardInput { .. }
                )
            {
                let was_dragging = self.timeline_index.is_dragging();
                if let Some(intent) = self.timeline_index.handle_event(
                    event,
                    ctx.scale_factor,
                    self.timeline_index_metrics,
                    &self.timeline_index_markers,
                ) {
                    let _ = self.apply_timeline_index_intent(&intent);
                    *ctx.handled = true;
                    return;
                }
                if was_dragging || self.timeline_index.is_dragging() {
                    *ctx.handled = true;
                    return;
                }
            }
        }

        if self.library_mode
            && self.album_editor
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let direction = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => y.signum(),
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y.signum() as f32,
            };
            if direction < 0.0 {
                self.page_library(true);
            } else if direction > 0.0 {
                self.page_library(false);
            }
            *ctx.handled = true;
            return;
        }

        // Ctrl + wheel is the cross-platform density gesture.  Keep an
        // ordinary wheel for chronological browsing; otherwise a trackpad
        // scroll would unexpectedly rearrange the entire library.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
            && ctx.modifiers.state().control_key()
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            if amount != 0.0 {
                // Natural scroll semantics: up means see more at once.
                self.adjust_timeline_density(amount < 0.0);
                *ctx.handled = true;
                return;
            }
        }

        // Tilo's vendored Wayland winit maps zwp_pointer_gestures_v1 to this
        // same event; macOS/iOS use it natively.  Ctrl-wheel above remains the
        // fallback on compositors that do not advertise pinch gestures.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::PinchGesture { delta, .. } = event
            && delta.is_finite()
            && *delta != 0.0
        {
            self.adjust_timeline_density(*delta > 0.0);
            *ctx.handled = true;
            return;
        }

        if self.library_mode && self.handle_metadata_editor_event(event, ctx) {
            return;
        }

        if self.library_mode && self.handle_album_editor_event(event, ctx) {
            return;
        }

        if self.handle_collection_page_event(event, ctx) {
            return;
        }

        if self.library_mode
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value.eq_ignore_ascii_case("a"))
                && self.selection_mode
            {
                self.select_all_visible();
                *ctx.handled = true;
                return;
            }
            if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value == " ")
            {
                if self.selection_mode {
                    self.finish_selection();
                } else {
                    self.begin_selection();
                }
                *ctx.handled = true;
                return;
            }
            if self.selection_mode && matches!(&key.logical_key, Key::Named(NamedKey::Escape)) {
                self.finish_selection();
                *ctx.handled = true;
                return;
            }
            let forward = match &key.logical_key {
                Key::Named(NamedKey::PageDown | NamedKey::ArrowRight) => Some(true),
                Key::Named(NamedKey::PageUp | NamedKey::ArrowLeft) => Some(false),
                _ => None,
            };
            if let Some(forward) = forward {
                if self.album_editor {
                    self.page_library(forward);
                } else {
                    self.scroll_timeline_rows(if forward { 3.0 } else { -3.0 });
                }
                *ctx.handled = true;
                return;
            }
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .timeline_density_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            self.adjust_timeline_density(matches!(hit, TimelineHit::ZoomIn));
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .album_edit_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = Some(album_id);
            self.begin_edit_album(ctx.state);
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((forward, enabled, _)) = self
                .library_page_hits
                .iter()
                .find(|(_, _, rect)| rect.contains(self.pointer))
                .copied()
            && enabled
        {
            self.page_library(forward);
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && self.selection_album_chooser
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .selection_destination_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.selection_destination_album = Some(album_id);
            self.notice =
                Some("Destination album selected; choose Use album to continue".to_owned());
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .selection_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            self.handle_selection_hit(hit, ctx.state);
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && self.selection_album_chooser
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
        {
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            match hit {
                AlbumEditorHit::New => self.begin_new_album(ctx.state),
                AlbumEditorHit::Edit => self.begin_edit_album(ctx.state),
                _ => {}
            }
            *ctx.handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .album_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = album_id;
            self.library_destination = LibraryDestination::Timeline;
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.active_favorites = false;
            self.library_offset = 0;
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some(card) = self
                .metadata_filter_cards
                .iter()
                .find(|card| card.bounds.contains(self.pointer))
                .cloned()
        {
            if card.favorites {
                let _ = self.view_favorites(ctx.state);
            } else {
                self.active_album = None;
                self.active_folder = card.folder;
                self.active_person = card.person;
                self.active_tag = card.tag;
                self.active_favorites = false;
                self.library_offset = 0;
                self.timeline_scroll_y = 0.0;
                self.notice = None;
            }
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((id, _)) = self
                .library_details_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.begin_metadata_editor(ctx.state, &id);
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let pixels = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 48.0,
                winit::event::MouseScrollDelta::PixelDelta(position) => -position.y,
            };
            self.scroll_timeline_pixels(pixels);
            *ctx.handled = true;
            return;
        }

        // A library card is a direct manipulation surface, but it is still
        // only a front end for a canonical action. The trailing star and
        // remove affordance have their own hit areas, so destructive and
        // metadata changes do not unexpectedly open the editor.
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((photo_id, card, favorite)) = self
                .library_cards
                .iter()
                .find(|(_, card, _)| card.contains(self.pointer))
                .cloned()
        {
            if self.selection_mode {
                self.toggle_library_selection(&photo_id, ctx.modifiers.state().shift_key());
            } else if !self.album_editor
                && !self.large_text_toolbar
                && self.canvas_rect.width() >= 720.0
                && self.pointer.y >= card.y1 - 44.0
                && self.pointer.x >= card.x1 - 88.0
            {
                let actions = card_action_controls(card);
                if actions.favorite.contains(self.pointer) {
                    self.library_remove_confirm = None;
                    self.commit_library_action(
                        ctx.state,
                        &crate::actions::PhotosAction::SetFavorite {
                            photo_id,
                            favorite: !favorite,
                        },
                    );
                } else if self.library_remove_confirm.as_deref() == Some(&photo_id) {
                    self.commit_library_action(
                        ctx.state,
                        &crate::actions::PhotosAction::RemovePhoto { photo_id },
                    );
                    self.library_remove_confirm = None;
                    self.notice = Some("Photograph removed from the shared library".to_owned());
                } else {
                    self.library_remove_confirm = Some(photo_id);
                    self.notice = Some(
                        "Click × again to remove it from the library and every album".to_owned(),
                    );
                }
            } else {
                self.library_remove_confirm = None;
                self.commit_library_action(
                    ctx.state,
                    &crate::actions::PhotosAction::SelectPhoto { photo_id },
                );
                self.library_mode = false;
            }
            *ctx.handled = true;
            return;
        }
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if let Some(&bar) = children.first() {
            let bar_children = ctx.taffy.children(bar).unwrap_or_default();
            for (slot, &bn) in self.toolbar_slots().iter().zip(bar_children.iter()) {
                let Slot::Item(n) = *slot else { continue };
                let Some((_, button)) = self.buttons.get_mut(n) else {
                    continue;
                };
                button.handle_event(event, &mut ctx.reborrow(ctx.path, bn));
            }
        }
        if let (Some(v), Some(&canvas)) = (self.viewer.as_mut(), children.get(2)) {
            v.handle_event(event, &mut ctx.reborrow(ctx.path, canvas));
        }
        self.drain_pending(ctx.state);
    }

    fn name(&self) -> &'static str {
        "PhotosApp"
    }

    /// Hand the concrete app back out of a `Box<dyn Widget>`.
    ///
    /// A compositor hosting this in a tab holds it as a trait object, so
    /// "open this file in the photo viewer that is already on screen" has no
    /// route in without it. Opening still goes through
    /// [`PhotosApp::open_path`] — this only recovers the type, it does not add
    /// a second way to change what is shown.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.accessibility_tree())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        self.reconcile_content(state);
        self.perform_toolbar_semantic_action(target, action, state)
            || self.perform_directory_semantic_action(target, action)
            || self.perform_library_semantic_action(target, action, state)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0, button] => self
                .buttons
                .get(*button)
                .map(|(_, button)| button.get_rect()),
            [1] => Some(self.canvas_rect),
            [2] if !self.status.is_empty() => Some(self.status_rect),
            _ => None,
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::search::capability::SearchSource for PhotosApp {
    /// Keep the palette projection derived from the same fresh document
    /// resource tree as agents and assistive technology. The provider's
    /// catalog gives Ctrl+K Unicode-safe, cursor-paged bounded queries.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for PhotosApp {
    /// The open photograph and the edits it supports, published to whatever is
    /// hosting this widget.
    ///
    /// The same tree [`crate::actions::PhotosResources`] serves to a packaged
    /// instance over the socket, so an app compiled into the compositor and one
    /// running as its own process offer the identical surface at the identical
    /// addresses. Without this the tree existed but only a caller already
    /// inside this process could reach it.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        // The framework root publishes the library, including the open
        // photograph as a child. Live button and search-status extras stay
        // on the widget.
        let mut resources = if self.actions.is_hosted() {
            Vec::new()
        } else {
            crate::actions::PhotosResources.publish_resources(doc)
        };
        resources.extend(
            self.retained_buttons()
                .into_iter()
                .flat_map(|button| button.resources(doc)),
        );
        if let Some(error) = &self.search_refresh_error {
            resources.push(crate::actions::search_status_resource(error));
        }
        resources
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if id == "photos.search-status" {
            return self
                .search_refresh_error
                .as_deref()
                .map(crate::actions::search_status_resource);
        }
        if self.actions.is_hosted() && id == "photos.library" {
            return None;
        }
        self.search_provider
            .resource(id)
            .or_else(|| {
                self.retained_buttons()
                    .into_iter()
                    .flat_map(|button| button.resources(doc))
                    .find(|resource| resource.id.as_str() == id)
            })
            .or_else(|| crate::actions::PhotosResources.publish_resource(doc, id))
    }
}

impl ui::widget::controls::ActionControls for PhotosApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.retained_buttons_mut()
            .into_iter()
            .find_map(|button| ui::widget::controls::take_control_invocation(button))
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        // Photos projects its custom media surfaces and retained buttons into
        // one complete semantic tree above. Inventory that visible tree once:
        // appending every retained widget here also advertises controls that
        // are currently hidden (other routes, closed sheets, disabled jobs),
        // producing orphan automation bindings with no matching UI target.
        // The buttons' canonical resources remain published by `resources`
        // and are dispatched through the same retained widgets.
        ui::widget::semantic_action_bindings(self.accessibility())
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        for button in self.retained_buttons_mut() {
            button.refresh_control_action_availability(allowed);
        }
    }
}

impl ui::history_mode::capability::DocumentHistory for PhotosApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for PhotosApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl PhotosApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
            crate::actions::PhotosOperation::View => {
                self.library_mode = false;
                self.album_editor = false;
                self.metadata_editor = false;
            }
            crate::actions::PhotosOperation::ViewLibrary => self.show_library(),
            crate::actions::PhotosOperation::ViewAlbums => self.show_albums(),
            crate::actions::PhotosOperation::ViewTimeline { anchor } => {
                self.view_timeline(doc, anchor.as_ref())?;
            }
            crate::actions::PhotosOperation::ViewPeople => {
                self.show_library();
                self.people_mode = true;
                self.library_destination = LibraryDestination::People;
                self.notice = Some(
                    "People recognition is local and opt-in; install a verified face model to begin"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::ViewLibraryAnalysis => {
                self.show_library_analysis();
                self.notice = Some(
                    "Local analysis makes descriptions, objects, pets, and text searchable"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Settings › Models to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::ViewFavorites => self.view_favorites(doc)?,
            crate::actions::PhotosOperation::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.places_open_key = None;
                self.notice = Some("Showing offline Places".to_owned());
            }
            crate::actions::PhotosOperation::ViewPlace { place_key } => {
                self.open_place_collection(doc, &place_key)?;
            }
            crate::actions::PhotosOperation::ViewAlbum { album_id } => {
                self.view_album(doc, &album_id)?;
            }
            crate::actions::PhotosOperation::ViewFolder { folder } => {
                self.view_metadata_filter(doc, Some(folder), None, None)?;
            }
            crate::actions::PhotosOperation::ViewPerson { person } => {
                self.view_metadata_filter(doc, None, Some(person), None)?;
            }
            crate::actions::PhotosOperation::ViewTag { tag } => {
                self.view_metadata_filter(doc, None, None, Some(tag))?;
            }
            crate::actions::PhotosOperation::ViewObject { object } => {
                self.view_analysis_filter(doc, object, false)?;
            }
            crate::actions::PhotosOperation::ViewPet { pet } => {
                self.view_analysis_filter(doc, pet, true)?;
            }
            crate::actions::PhotosOperation::Open { path: Some(_) } => {
                return Err("direct open requires lifecycle dispatch".into());
            }
            crate::actions::PhotosOperation::Save { path } => {
                let open =
                    PhotosDoc::image(doc).ok_or_else(|| "no photograph is open".to_string())?;
                let destination = path.unwrap_or_else(|| Self::suggested_save_path(&open));
                if !self.save_to(&destination) {
                    return Err(tr("photos-save-failed"));
                }
            }
            other => self.perform_effect(other, doc)?,
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        if let crate::actions::PhotosOperation::ExportEditedContent {
            resource_id,
            source_content,
            edits,
        } = &operation
        {
            let output = crate::actions::export_edited_content(
                resource_id,
                source_content,
                *edits,
                doc,
                &self.content,
            )?;
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(output)));
        }
        if let crate::actions::PhotosOperation::Open { path: Some(file) } = operation {
            self.import_content = Some(LiveContent::read(doc));
            self.pending_resource_open = Some((invocation, file));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if self.picker.is_some()
            || self.directory_picker.is_some()
            || self.directory_import.is_some()
        {
            self.resource_invocation = Some(invocation);
            if let Some(import) = self.directory_import.as_mut() {
                import.invocation = Some(invocation);
            }
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one dispatcher routes every retained-control and Photos operation type with shared validation"
    )]
    fn admit_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if type_id == RETAINED_CONTROL_OPERATION_TYPE {
            let op = ui::widget::ResourceOperation::local(invocation, kind, type_id, payload);
            for button in self.retained_buttons_mut() {
                if let Some(started) = button.start_operation(&op, doc, path)? {
                    return Ok(Some(started));
                }
            }
        }
        if kind == tilo_actions::OperationKind::Document {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        match &operation {
            crate::actions::PhotosOperation::AnalyzeLibrary => {
                self.begin_library_analysis(invocation, doc, false)?;
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            crate::actions::PhotosOperation::ResumeLibraryAnalysis => {
                self.resume_library_analysis(invocation, doc)?;
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            crate::actions::PhotosOperation::RetryLibraryAnalysisFailures => {
                self.begin_library_analysis(invocation, doc, true)?;
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            crate::actions::PhotosOperation::PauseLibraryAnalysis => {
                if !self.pause_library_analysis() {
                    return Err("Photo analysis is not running".to_owned());
                }
                return Ok(Some(ui::widget::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "completed": self.library_analysis.completed,
                            "remaining": self.library_analysis.remaining(),
                        })),
                        resources: vec!["photos.library-analysis".to_owned()],
                    },
                )));
            }
            _ => {}
        }
        if let crate::actions::PhotosOperation::AnalyzeText {
            resource_id,
            language,
        } = operation
        {
            let photo = PhotosDoc::photo(doc, &resource_id)
                .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
            if photo.image.content.is_none() {
                return Err("This legacy photograph needs a verified content identity before text recognition can be shared".to_owned());
            }
            let sender = self
                .analysis_tx
                .as_ref()
                .ok_or_else(|| "local text recognition is unavailable on this device".to_owned())?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id: resource_id,
                    content: photo.image.content.clone(),
                    image: photo.image,
                    kind: AnalysisJobKind::Ocr { language },
                })
                .map_err(|_| {
                    "text recognition is busy; wait for the current photos to finish".to_owned()
                })?;
            self.active_analysis.insert(invocation);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if let crate::actions::PhotosOperation::AnalyzeImage { resource_id } = operation {
            let photo = PhotosDoc::photo(doc, &resource_id)
                .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
            if photo.image.content.is_none() {
                return Err("This legacy photograph needs a verified content identity before image analysis can be shared".to_owned());
            }
            let capability = tilo_vision::discover_local_capability(
                &agent_provider_config::ProviderConfig::load(),
            )
            .ok_or_else(|| {
                "No verified local image-analysis model is ready. Install and start one in Settings."
                    .to_owned()
            })?;
            let sender = self
                .analysis_tx
                .as_ref()
                .ok_or_else(|| "local image analysis is unavailable on this device".to_owned())?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id: resource_id,
                    content: photo.image.content.clone(),
                    image: photo.image,
                    kind: AnalysisJobKind::Vision { capability },
                })
                .map_err(|_| {
                    "local image analysis is busy; wait for the current photos to finish".to_owned()
                })?;
            self.active_analysis.insert(invocation);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if matches!(
            operation,
            crate::actions::PhotosOperation::AnalyzePeopleLibrary
        ) {
            self.schedule_people_scan_batch(invocation, doc)?;
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }

        let crate::actions::PhotosOperation::ImportCameraMedia { media_id, name } = operation
        else {
            return self.admit_resource_operation(invocation, kind, type_id, payload, doc, path);
        };

        // This is deliberately a compound admission boundary: verify the
        // portable Camera identity and bytes before committing the one
        // idempotent Photos library/selection update.  A missing local blob
        // remains a refusal instead of becoming a path-only phantom entry.
        let photo = crate::camera_import_photo(&media_id, &name, &self.content)?;
        let id = photo.id.clone();
        let action = crate::actions::PhotosAction::ImportPhoto { photo };
        self.actions
            .try_dispatch_with_context(
                doc,
                crate::actions::PHOTOS_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.photos", "Photos", &action),
            )
            .map_err(|error| error.to_string())?;
        self.notice = Some("Imported Camera photo".to_owned());
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput {
                value: Some(serde_json::Value::String(id.clone())),
                resources: vec![id],
            },
        )))
    }
}

impl PhotosApp {
    /// Admit one fresh contiguous People slice plus a bounded, device-local
    /// anchor set from earlier local suggestions. Anchors use ordinary local
    /// pixels only to let the signed runtime bridge disjoint batches; they do
    /// not count toward cursor progress and never leave this device.
    fn schedule_people_scan_batch(
        &mut self,
        invocation: tilo_actions::InvocationId,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        if !self.people_controller.privacy().recognition_enabled {
            return Err("People recognition is off on this device. Enable it in People before scanning local photographs.".to_owned());
        }
        let config = agent_provider_config::ProviderConfig::load();
        self.people_controller.discover_face_capability(&config);
        let capability = tilo_vision::discover_local_face_capability(&config).ok_or_else(|| {
            "No verified local face model is ready. Install and start one in Settings.".to_owned()
        })?;
        let locally_available: Vec<_> = PhotosDoc::library(doc)
            .photos
            .into_iter()
            .filter(|photo| crate::doc::bytes_of(&photo.image, &self.content).is_some())
            .map(|photo| (photo.id, photo.image))
            .collect();
        let anchor_ids = self.people_controller.reconciliation_anchor_photo_ids(2);
        let anchor_count = anchor_ids
            .iter()
            .filter(|id| {
                locally_available
                    .iter()
                    .any(|(available, _)| available == *id)
            })
            .count()
            .min(tilo_vision::MAX_FACE_BATCH.saturating_sub(1));
        let (start, scan_len) = self
            .people_scan_progress
            .batch_range(
                locally_available.len(),
                tilo_vision::MAX_FACE_BATCH.saturating_sub(anchor_count),
            )
            .ok_or_else(|| {
                "No locally available photographs are ready for People recognition.".to_owned()
            })?;
        let mut photos: Vec<_> = locally_available
            .iter()
            .skip(start)
            .take(scan_len)
            .cloned()
            .collect();
        for id in anchor_ids {
            if photos.iter().any(|(photo_id, _)| photo_id == &id) {
                continue;
            }
            if let Some(anchor) = locally_available
                .iter()
                .find(|(photo_id, _)| photo_id == &id)
            {
                photos.push(anchor.clone());
            }
        }
        let Some((_, first)) = photos.first() else {
            return Err(
                "No locally available photographs are ready for People recognition.".to_owned(),
            );
        };
        let sender = self
            .analysis_tx
            .as_ref()
            .ok_or_else(|| "local People recognition is unavailable on this device".to_owned())?;
        sender
            .try_send(AnalysisJob {
                invocation,
                photo_id: "photos.people".to_owned(),
                content: None,
                image: first.clone(),
                kind: AnalysisJobKind::People {
                    capability,
                    images: photos,
                },
            })
            .map_err(|_| {
                "local image analysis is busy; wait for the current photos to finish".to_owned()
            })?;
        "running".clone_into(&mut self.people_scan_progress.status);
        self.people_scan_progress.last_error = None;
        self.people_scan_progress.last_library_size = locally_available.len();
        self.people_scan_progress.save();
        self.active_analysis.insert(invocation);
        self.active_people_scans.insert(invocation);
        self.pending_people_scans.insert(
            invocation,
            PendingPeopleScan {
                start,
                scanned: scan_len,
                total: locally_available.len(),
            },
        );
        Ok(())
    }

    fn is_toolbar_hit_enabled(&self, hit: Hit) -> bool {
        self.buttons
            .iter()
            .find(|(candidate, _)| *candidate == hit)
            .is_some_and(|(_, button)| button.is_enabled())
    }

    fn focused_compact_more_hit(&self) -> Option<Hit> {
        COMPACT_MORE_ENTRIES.get(self.compact_more_focus).copied()
    }

    fn step_compact_more_focus(&mut self, forward: bool) {
        for _ in 0..COMPACT_MORE_ENTRIES.len() {
            self.compact_more_focus = if forward {
                (self.compact_more_focus + 1) % COMPACT_MORE_ENTRIES.len()
            } else {
                (self.compact_more_focus + COMPACT_MORE_ENTRIES.len() - 1)
                    % COMPACT_MORE_ENTRIES.len()
            };
            if self
                .focused_compact_more_hit()
                .is_some_and(|hit| self.is_toolbar_hit_enabled(hit))
            {
                break;
            }
        }
    }

    /// Draw the phone-only action menu over the current surface. Its entries
    /// reuse the canonical toolbar actions, so hiding their buttons never
    /// creates a second edit/export implementation.
    fn draw_compact_more(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.compact_more_hits.clear();
        if !self.compact_more_open || self.toolbar_slots() != COMPACT_SLOTS {
            return;
        }
        let entries = [
            (Hit::CropToView, tr("photos-crop")),
            (Hit::Reset, tr("photos-reset")),
            (Hit::Copy, tr("photos-copy")),
            (Hit::Share, tr("photos-share")),
            (Hit::Save, tr("photos-save")),
        ];
        let width = 178.0;
        let row_h = 44.0;
        let panel = Rect::new(
            self.toolbar_rect.x1 - width - f64::from(PAD),
            self.toolbar_rect.y1 + 4.0,
            self.toolbar_rect.x1 - f64::from(PAD),
            self.toolbar_rect.y1 + 4.0 + row_h * entries.len() as f64,
        );
        // A floating menu: Panel role ground + border (HIG §9.5), rows
        // separated by inset derived rules rather than full-bleed ones.
        let menu_radius = f64::from(theme.tokens.radii.md);
        let ground = ui::chrome::panel_ground(theme);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            ground,
            None,
            &vello::kurbo::RoundedRect::from_rect(panel, menu_radius),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::panel_border(theme),
            None,
            &surface::edge_outline(panel, menu_radius),
        );
        for (index, (hit, label)) in entries.into_iter().enumerate() {
            let row = Rect::new(
                panel.x0,
                panel.y0 + index as f64 * row_h,
                panel.x1,
                panel.y0 + (index + 1) as f64 * row_h,
            );
            let enabled = self
                .buttons
                .iter()
                .find(|(candidate, _)| *candidate == hit)
                .is_some_and(|(_, button)| button.is_enabled());
            if index == self.compact_more_focus && enabled {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.bg_hover,
                    None,
                    &row,
                );
            }
            text_renderer.draw_text_weighted(
                scene,
                &label,
                13.0,
                if enabled {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                transform * Affine::translate((row.x0 + 12.0, row.y0 + 13.0)),
                Some((row.width() - 24.0) as f32),
                TextWeight::Medium,
            );
            if index > 0 {
                surface::separator(
                    scene,
                    transform,
                    row.x0,
                    row.x1,
                    row.y0 + 0.5,
                    step(Spacing::BASE.s3),
                    ground,
                );
            }
            if enabled {
                self.compact_more_hits.push((hit, row));
            }
        }
    }
}

/// Lets the compositor host Photos in a tab directly, the same way it hosts
/// the terminal, the file explorer and the word processor, rather than
/// standing a placeholder card where the photograph should be.
impl ui::widget::AppWidget for PhotosApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's tab vocabulary has no picture variant. Photos edits a
        // file and writes the result back, which is what `Editor` names; the
        // glyph below is what actually tells the two apart in a tab pill.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        // The open file's own name, so two photographs open at once are two
        // distinguishable tabs rather than two tabs called "Photos". Nothing
        // open falls back to the app's name, which is the only true thing to
        // say about an empty window.
        self.open_image_path()
            .and_then(|p| p.rsplit('/').next())
            .filter(|n| !n.is_empty())
            .unwrap_or("Photos")
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font image glyph, matching the convention every other hosted
        // app's tab pill uses.
        ui::widget::IconDescriptor::Glyph("\u{F02E9}")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}

#[cfg(test)]
#[path = "app/tests.rs"]
mod tests;

#[cfg(test)]
mod history_reconcile_tests {
    use super::*;
    use automerge::ROOT;
    use tilo_automerge::ApplyIncoming;
    fn restore(doc: &mut AutoCommit, selected: &[automerge::ChangeHash]) {
        let heads = doc.get_heads();
        let plan = ui::history_mode::restore::prepare_content_restore(
            doc,
            &crate::framework_history::SCHEMA,
            &heads,
            selected,
        )
        .unwrap();
        ui::history_mode::restore::apply_prepared_content_restore(doc, &plan).unwrap();
    }
    fn setup() -> (PhotosApp, AutoCommit, String) {
        let mut doc = AutoCommit::new();
        let image = OpenImage::from_path("historical-missing.png", 4, 3);
        PhotosDoc::set_image(&mut doc, Some(&image));
        let id = PhotosDoc::library(&doc).photos[0].id.clone();
        let mut app = PhotosApp::new();
        app.sync(&mut doc, &ROOT);
        (app, doc, id)
    }
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn history_accepts_label_and_album_formats_and_rejects_malformed_conflicts() {
        eprintln!(
            "phase history_accepts_membership_map_labels_and_rejects_all_malformed_conflicts"
        );
        {
            use automerge::{ObjType, ReadDoc, transaction::Transactable};
            let (_, mut doc, id) = setup();
            let mut library = PhotosDoc::library(&doc);
            library.photos[0].people = vec!["Alice".into()];
            library.photos[0].tags = vec!["Portrait".into()];
            PhotosDoc::set_library(&mut doc, &library);
            crate::framework_history::validate(&doc).unwrap();
            let root = doc.get(ROOT, "photos_library").unwrap().unwrap().1;
            for key in ["photo_people", "photo_tags"] {
                let column = doc.get(&root, key).unwrap().unwrap().1;
                let row = doc.get(&column, id.as_str()).unwrap().unwrap().1;
                doc.put(&row, "Removed", false).unwrap();
                crate::framework_history::validate(&doc).unwrap();
                let mut array = doc.clone();
                array
                    .put(&column, id.as_str(), "[\"Array label\"]")
                    .unwrap();
                assert!(crate::framework_history::validate(&array).is_err());

                // Two replicas can create different membership maps for one photo.
                let mut left = doc.fork();
                let mut right = doc.fork();
                let left_row = left.put_object(&column, id.as_str(), ObjType::Map).unwrap();
                let right_row = right
                    .put_object(&column, id.as_str(), ObjType::Map)
                    .unwrap();
                left.put(&left_row, "Valid", true).unwrap();
                right.put(&right_row, "Invalid", "not a boolean").unwrap();
                left.merge_incoming(&mut right).unwrap();
                assert_eq!(left.get_all(&column, id.as_str()).unwrap().len(), 2);
                assert!(crate::framework_history::validate(&left).is_err());

                // A valid winning/losing membership does not excuse an invalid sibling value.
                let mut left = doc.fork();
                let mut right = doc.fork();
                left.put(&row, "Conflict", true).unwrap();
                right.put(&row, "Conflict", "not a boolean").unwrap();
                left.merge_incoming(&mut right).unwrap();
                assert_eq!(left.get_all(&row, "Conflict").unwrap().len(), 2);
                assert!(crate::framework_history::validate(&left).is_err());

                for invalid_label in [String::new(), "x".repeat(65_537)] {
                    let mut invalid = doc.clone();
                    invalid.put(&row, invalid_label.as_str(), true).unwrap();
                    assert!(crate::framework_history::validate(&invalid).is_err());
                }
            }
        }

        eprintln!("phase history_accepts_album_membership_formats_and_rejects_malformed_conflicts");
        {
            use automerge::{ObjType, ReadDoc, transaction::Transactable};
            let (_, mut doc, ids) = album_membership_fixture();
            seed_membership_album(&mut doc, &ids);
            // The production writer emits the current absolute JSON membership.
            crate::framework_history::validate(&doc).unwrap();
            let root = doc.get(ROOT, "photos_library").unwrap().unwrap().1;
            let albums = doc.get(&root, "albums").unwrap().unwrap().1;
            let row = doc
                .get(&albums, "photos.album:membership")
                .unwrap()
                .unwrap()
                .1;
            let mut list = doc.clone();
            let members = list.put_object(&row, "photo_ids", ObjType::List).unwrap();
            list.insert(&members, 0, ids[0].as_str()).unwrap();
            assert!(crate::framework_history::validate(&list).is_err());

            // Concurrent replacements retain both values; a malformed one may
            // not hide behind a valid sibling.
            for invalid_json in [false, true] {
                let mut left = doc.fork();
                let mut right = doc.fork();
                left.set_actor(automerge::ActorId::from([2; 16].as_slice()));
                right.set_actor(automerge::ActorId::from([3; 16].as_slice()));
                left.put(&row, "photo_ids", if invalid_json { "[7]" } else { "[]" })
                    .unwrap();
                right
                    .put(
                        &row,
                        "photo_ids",
                        serde_json::to_string(&[ids[0].as_str()]).unwrap(),
                    )
                    .unwrap();
                left.merge_incoming(&mut right).unwrap();
                assert_eq!(left.get_all(&row, "photo_ids").unwrap().len(), 2);
                assert_eq!(
                    crate::framework_history::validate(&left).is_err(),
                    invalid_json
                );
            }

            for invalid_members in [vec!["x".repeat(65_537)], vec![ids[0].clone(); 10_001]] {
                let mut invalid = doc.clone();
                invalid
                    .put(
                        &row,
                        "photo_ids",
                        serde_json::to_string(&invalid_members).unwrap(),
                    )
                    .unwrap();
                assert!(crate::framework_history::validate(&invalid).is_err());
            }
        }
    }

    #[test]
    fn thumbnails_follow_photo_identity_and_visibility() {
        eprintln!(
            "phase newly_visible_timeline_photo_uses_immutable_thumbnail_outside_queued_window"
        );
        {
            let mut doc = AutoCommit::new();
            let mut library = crate::model::PhotoLibrary::default();
            for index in 0..80 {
                let mut photo = crate::model::LibraryPhoto::from_image(
                    OpenImage::from_path(&format!("missing-timeline-{index}.png"), 30, 20)
                        .with_content(format!("{index:064x}")),
                );
                photo.captured_at =
                    Some(format!("2026-{:02}-{:02}", 1 + index / 28, 1 + index % 28));
                library.photos.push(photo);
            }
            let target = library.photos[2].clone();
            PhotosDoc::set_library(&mut doc, &library);
            let mut app = PhotosApp::new();
            app.show_library();
            app.canvas_rect = Rect::new(0.0, 0.0, 1_440.0, 900.0);
            app.sync(&mut doc, &ROOT);
            assert!(!app.thumbnail_resource_keys.contains_key(&target.id));
            app.view_timeline(
                &doc,
                Some(&crate::actions::TimelineAnchor::Day {
                    day: target.captured_at.clone().unwrap(),
                }),
            )
            .unwrap();
            let mut text = TextRenderer::new();
            let mut missing = Scene::new();
            app.draw_library(
                &mut missing,
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert!(app.library_cards.iter().any(|(id, _, _)| id == &target.id));
            assert!(!app.thumbnail_resource_keys.contains_key(&target.id));
            app.library_thumbnails.insert(
                thumbnail_key(&target.image),
                to_peniko(&crate::shots_pattern(30, 20)),
            );
            let mut hydrated = Scene::new();
            app.draw_library(
                &mut hydrated,
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert!(app.library_cards.iter().any(|(id, _, _)| id == &target.id));
            assert_eq!(
                hydrated.encoding().resources.patches.len(),
                missing.encoding().resources.patches.len() + 1,
                "the production painter records the cached immutable image even before it enters the queued thumbnail window"
            );
        }

        eprintln!("phase changed_photo_identity_invalidates_cached_thumbnail_and_late_old_result");
        {
            let (mut app, mut doc, id) = setup();
            let old = app.thumbnail_generation.unwrap();
            app.library_thumbnails
                .insert(id.clone(), to_peniko(&crate::shots_pattern(4, 3)));
            let mut lib = PhotosDoc::library(&doc);
            lib.photos[0].image.content = Some("a".repeat(64));
            PhotosDoc::set_library(&mut doc, &lib);
            app.sync(&mut doc, &ROOT);
            assert!(!app.library_thumbnails.contains_key(&id));
            app.accept_thumbnail_result(ThumbnailResult {
                generation: old,
                id: id.clone(),
                preview: Some(crate::shots_pattern(4, 3)),
            });
            assert!(!app.library_thumbnails.contains_key(&id));
        }
    }

    #[test]
    fn changed_targets_invalidate_metadata_and_album_drafts() {
        eprintln!(
            "phase changed_target_invalidates_metadata_draft_before_input_but_unrelated_change_preserves_it"
        );
        {
            let (mut app, mut doc, id) = setup();
            let mut lib = PhotosDoc::library(&doc);
            lib.photos[0].tags = vec!["restored".into()];
            PhotosDoc::set_library(&mut doc, &lib);
            let selected = doc.get_heads();
            lib.photos[0].tags = vec!["live".into()];
            PhotosDoc::set_library(&mut doc, &lib);
            app.sync(&mut doc, &ROOT);
            app.begin_metadata_editor(&doc, &id);
            app.metadata_tags_input.set_text("unsaved draft");
            PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
            app.sync(&mut doc, &ROOT);
            assert!(app.metadata_editor);
            assert_eq!(app.metadata_tags_input.value(), "unsaved draft");
            restore(&mut doc, &selected);
            app.perform_semantic_action("nonexistent", &AccessAction::Activate, &mut doc, &ROOT);
            assert!(
                !app.metadata_editor,
                "stale draft must detach before any new input"
            );
            app.save_metadata_editor(&mut doc);
            assert_eq!(PhotosDoc::library(&doc).photos[0].tags, vec!["restored"]);
            app.begin_metadata_editor(&doc, &id);
            app.metadata_tags_input.set_text("next edit");
            app.save_metadata_editor(&mut doc);
            assert_eq!(PhotosDoc::library(&doc).photos[0].tags, vec!["next edit"]);
        }

        eprintln!("phase changed_second_batch_photo_invalidates_the_whole_metadata_draft");
        {
            let (mut app, mut doc, first) = setup();
            let second = crate::model::LibraryPhoto::from_image(OpenImage::from_path(
                "second-missing.png",
                4,
                3,
            ));
            let second_id = second.id.clone();
            let mut library = PhotosDoc::library(&doc);
            library.photos.push(second);
            PhotosDoc::set_library(&mut doc, &library);
            app.sync(&mut doc, &ROOT);
            app.begin_metadata_editor_for_ids(&doc, vec![first, second_id.clone()]);
            app.metadata_tags_input.set_text("unsaved batch draft");
            library.photos[1].tags = vec!["remote edit".into()];
            PhotosDoc::set_library(&mut doc, &library);
            app.perform_semantic_action("nonexistent", &AccessAction::Activate, &mut doc, &ROOT);
            assert!(
                !app.metadata_editor,
                "a changed non-first batch target invalidates the draft before input"
            );
            app.save_metadata_editor(&mut doc);
            assert_eq!(
                PhotosDoc::library(&doc).photo(&second_id).unwrap().tags,
                vec!["remote edit"]
            );
        }

        eprintln!("phase changed_album_or_deleted_member_detaches_draft_and_cannot_resurrect_it");
        {
            let (mut app, mut doc, id) = setup();
            let selected = doc.get_heads();
            let mut lib = PhotosDoc::library(&doc);
            lib.albums.push(crate::model::Album {
                id: "photos.album:membership".into(),
                name: "Original".into(),
                photo_ids: vec![id],
            });
            PhotosDoc::set_library(&mut doc, &lib);
            app.sync(&mut doc, &ROOT);
            app.active_album = Some("photos.album:membership".into());
            app.begin_edit_album(&doc);
            app.album_name_input.set_text("stale draft");
            restore(&mut doc, &selected);
            app.sync(&mut doc, &ROOT);
            assert!(!app.album_editor);
            app.save_album_editor(&mut doc);
            assert!(PhotosDoc::library(&doc).albums.is_empty());
            app.begin_new_album(&doc);
            app.album_name_input.set_text("Next album");
            app.save_album_editor(&mut doc);
            assert_eq!(PhotosDoc::library(&doc).albums[0].name, "Next album");
        }
    }
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn late_opens_imports_and_restores_settle_against_current_content() {
        eprintln!("phase pending_resource_open_cannot_overwrite_changed_content_and_settles_once");
        {
            let (mut app, mut doc, _) = setup();
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("import.png");
            std::fs::write(
                &path,
                pixels::encode_png(&crate::shots_pattern(4, 3)).unwrap(),
            )
            .unwrap();
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::PhotosOperation::Open {
                path: Some(path.to_string_lossy().into()),
            })
            .unwrap();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::PHOTOS_OPERATION_TYPE,
                    &payload,
                ),
                &mut doc,
                &ROOT,
            )
            .unwrap();
            PhotosDoc::set_image(&mut doc, None);
            let heads = doc.get_heads();
            app.sync(&mut doc, &ROOT);
            assert!(PhotosDoc::image(&doc).is_none());
            assert_eq!(doc.get_heads(), heads);
            assert!(
                matches!(app.poll_resource_operation(invocation,false,&doc,&ROOT),Some(tilo_actions::InvocationUpdate::Cancelled{invocation:id,..}) if id==invocation)
            );
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &ROOT)
                    .is_none()
            );
        }

        eprintln!(
            "phase already_dispatched_picker_reports_actual_outcome_without_importing_stale_selection"
        );
        {
            for outcome in [0, 1, 2] {
                let (mut app, mut doc, _) = setup();
                let invocation = tilo_actions::InvocationId::new();
                let (tx, rx) = std::sync::mpsc::channel();
                app.picker = Some(rx);
                app.resource_invocation = Some(invocation);
                app.import_content = Some(LiveContent::read(&doc));
                PhotosDoc::set_image(&mut doc, None);
                app.sync(&mut doc, &ROOT);
                tx.send(match outcome {
                    0 => ui::portal::PortalOutcome::Opened(std::path::PathBuf::from("chosen.png")),
                    1 => ui::portal::PortalOutcome::Cancelled,
                    _ => ui::portal::PortalOutcome::Failed("actual picker failure".into()),
                })
                .unwrap();
                let heads = doc.get_heads();
                app.sync(&mut doc, &ROOT);
                assert_eq!(doc.get_heads(), heads);
                assert!(PhotosDoc::image(&doc).is_none());
                match app
                    .poll_resource_operation(invocation, false, &doc, &ROOT)
                    .unwrap()
                {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation: id,
                        error,
                    } => {
                        assert_eq!(id, invocation);
                        assert_eq!(
                            error.code,
                            if outcome == 0 {
                                "photos.stale_selection"
                            } else {
                                "photos.portal_failed"
                            }
                        );
                        if outcome == 0 {
                            assert!(error.message.contains("chosen.png"));
                        } else {
                            assert_eq!(error.message, "actual picker failure");
                        }
                    }
                    tilo_actions::InvocationUpdate::Cancelled { invocation: id, .. } => {
                        assert_eq!(id, invocation);
                        assert_eq!(outcome, 1);
                    }
                    other => panic!("unexpected settlement {other:?}"),
                }
                assert!(
                    app.poll_resource_operation(invocation, false, &doc, &ROOT)
                        .is_none()
                );
            }
        }

        eprintln!("phase failed_import_reports_failure_even_when_previous_photo_is_still_decoded");
        {
            let mut doc = AutoCommit::new();
            let store = ContentStore::memory();
            let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
            let open =
                OpenImage::from_path("existing.png", 6, 4).with_content(store.put(&bytes).unwrap());
            PhotosDoc::set_image(&mut doc, Some(&open));
            let mut app = PhotosApp::with_content(store);
            app.sync(&mut doc, &ROOT);
            assert!(app.open_image_path().is_some());
            let invocation = tilo_actions::InvocationId::new();
            let tmp = tempfile::tempdir().unwrap();
            let payload = serde_json::to_vec(&crate::actions::PhotosOperation::Open {
                path: Some(tmp.path().join("absent.png").to_string_lossy().into()),
            })
            .unwrap();
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::PHOTOS_OPERATION_TYPE,
                    &payload,
                ),
                &mut doc,
                &ROOT,
            )
            .unwrap();
            app.sync(&mut doc, &ROOT);
            assert!(
                matches!(app.poll_resource_operation(invocation,false,&doc,&ROOT),Some(tilo_actions::InvocationUpdate::Failed{invocation:id,..}) if id==invocation)
            );
            assert_eq!(PhotosDoc::image(&doc), Some(open));
        }

        eprintln!("phase content_restore_drops_active_viewer_drag_before_next_pointer_move");
        {
            let mut doc = AutoCommit::new();
            let store = ContentStore::memory();
            let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
            let open =
                OpenImage::from_path("same.png", 6, 4).with_content(store.put(&bytes).unwrap());
            PhotosDoc::set_image(&mut doc, Some(&open));
            PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
            let selected = doc.get_heads();
            PhotosDoc::set_edits(&mut doc, &Edits::default());
            let mut app = PhotosApp::with_content(store);
            app.sync(&mut doc, &ROOT);
            let mut taffy = TaffyTree::new();
            let mut nodes = HashMap::new();
            let theme = Theme::dark();
            let node = app.build_layout(&mut taffy, &theme, &mut nodes);
            taffy
                .set_style(
                    node,
                    Style {
                        size: Size {
                            width: Dimension::length(800.),
                            height: Dimension::length(600.),
                        },
                        ..Default::default()
                    },
                )
                .unwrap();
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: AvailableSpace::Definite(800.),
                        height: AvailableSpace::Definite(600.),
                    },
                )
                .unwrap();
            app.apply_layout(&taffy, node, Point::ZERO);
            let center = app.viewer.as_ref().unwrap().get_rect().center();
            let device_id = unsafe { std::mem::zeroed() };
            let mut handled = false;
            let mut focus = None;
            for event in [
                winit::event::WindowEvent::CursorMoved {
                    device_id,
                    position: winit::dpi::PhysicalPosition::new(center.x, center.y),
                },
                winit::event::WindowEvent::MouseInput {
                    device_id,
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                },
            ] {
                app.viewer.as_mut().unwrap().handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.,
                        modifiers: &winit::event::Modifiers::default(),
                        state: &mut doc,
                        path: &ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
            }
            restore(&mut doc, &selected);
            app.handle_event(
                &winit::event::WindowEvent::CursorMoved {
                    device_id,
                    position: winit::dpi::PhysicalPosition::new(center.x + 40., center.y + 20.),
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut doc,
                    path: &ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert_eq!(app.viewer.as_ref().unwrap().pan(), vello::kurbo::Vec2::ZERO);
            assert!((app.viewer.as_ref().unwrap().zoom() - 1.).abs() < f32::EPSILON);
            let next = PhotosDoc::edits(&doc).rotated_right();
            app.commit_edits(&mut doc, next);
            app.sync(&mut doc, &ROOT);
            assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 2);
        }
    }

    fn album_membership_fixture() -> (PhotosApp, AutoCommit, [String; 4]) {
        let mut doc = AutoCommit::new();
        // Browse order deliberately differs from lexical identity order.
        let photos: Vec<_> = ["c", "a", "d", "b"]
            .into_iter()
            .map(|label| {
                LibraryPhoto::from_image(
                    OpenImage::from_path(&format!("{label}.png"), 4, 3)
                        .with_content(label.repeat(64)),
                )
            })
            .collect();
        let ids = std::array::from_fn(|i| photos[i].id.clone());
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos,
                albums: Vec::new(),
            },
        );
        let mut app = PhotosApp::new();
        app.library_mode = true;
        app.sync(&mut doc, &ROOT);
        (app, doc, ids)
    }

    fn activate_album_control(app: &mut PhotosApp, doc: &mut AutoCommit, target: &str) {
        assert!(app.perform_semantic_action(target, &AccessAction::Activate, doc, &ROOT));
        app.sync(doc, &ROOT);
    }

    fn seed_membership_album(doc: &mut AutoCommit, ids: &[String; 4]) {
        let mut library = PhotosDoc::library(doc);
        library.albums.push(crate::model::Album {
            id: "photos.album:membership".into(),
            name: "Original".into(),
            photo_ids: vec![ids[0].clone(), ids[3].clone()],
        });
        PhotosDoc::set_library(doc, &library);
    }

    #[test]
    fn album_membership_edits_save_selected_ids_in_browse_order() {
        eprintln!("phase album_membership_create_saves_selected_ids_in_browse_order");
        {
            let (mut app, mut doc, ids) = album_membership_fixture();
            activate_album_control(&mut app, &mut doc, "photos:album-editor:new");
            app.album_name_input.set_text("Selected photographs");
            // Select in reverse browse order; toggle a third photograph off again.
            for i in [3, 1, 0, 1] {
                activate_album_control(
                    &mut app,
                    &mut doc,
                    &format!("photos:library-photo:{}", ids[i]),
                );
            }
            activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
            let library = PhotosDoc::library(&doc);
            assert_eq!(library.albums.len(), 1);
            assert_eq!(library.albums[0].name, "Selected photographs");
            assert_eq!(
                library.albums[0].photo_ids,
                vec![ids[0].clone(), ids[3].clone()]
            );
            assert!(!app.album_editor);
        }

        eprintln!("phase album_membership_rename_preserves_selected_ids_and_order");
        {
            let (mut app, mut doc, ids) = album_membership_fixture();
            seed_membership_album(&mut doc, &ids);
            app.sync(&mut doc, &ROOT);
            activate_album_control(
                &mut app,
                &mut doc,
                "photos:library-album:photos.album:membership",
            );
            activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
            app.album_name_input.set_text("Renamed album");
            activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
            let library = PhotosDoc::library(&doc);
            assert_eq!(library.albums.len(), 1);
            let album = library.album("photos.album:membership").unwrap();
            assert_eq!(album.name, "Renamed album");
            assert_eq!(album.photo_ids, vec![ids[0].clone(), ids[3].clone()]);
            assert!(!app.album_editor);
        }

        eprintln!("phase album_membership_after_restore_supports_normal_edit_and_browse_order");
        {
            let (mut app, mut doc, ids) = album_membership_fixture();
            seed_membership_album(&mut doc, &ids);
            let selected = doc.get_heads();
            let mut library = PhotosDoc::library(&doc);
            library.albums[0].name = "Later album".into();
            library.albums[0].photo_ids = vec![ids[1].clone(), ids[2].clone()];
            PhotosDoc::set_library(&mut doc, &library);
            app.sync(&mut doc, &ROOT);
            activate_album_control(
                &mut app,
                &mut doc,
                "photos:library-album:photos.album:membership",
            );
            activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
            app.album_name_input.set_text("Unsaved later draft");
            restore(&mut doc, &selected);
            app.sync(&mut doc, &ROOT);
            assert!(!app.album_editor, "restore must detach the obsolete draft");
            let restored = PhotosDoc::library(&doc);
            let album = restored.album("photos.album:membership").unwrap();
            assert_eq!(album.name, "Original");
            assert_eq!(album.photo_ids, vec![ids[0].clone(), ids[3].clone()]);

            activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
            app.album_name_input.set_text("Edited after restore");
            activate_album_control(&mut app, &mut doc, "photos:library-album:all");
            for i in [3, 2] {
                activate_album_control(
                    &mut app,
                    &mut doc,
                    &format!("photos:library-photo:{}", ids[i]),
                );
            }
            activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
            let library = PhotosDoc::library(&doc);
            let album = library.album("photos.album:membership").unwrap();
            assert_eq!(album.name, "Edited after restore");
            assert_eq!(album.photo_ids, vec![ids[0].clone(), ids[2].clone()]);
            assert_eq!(library.photos.len(), 4);
            assert!(!app.album_editor);
        }
    }
}
