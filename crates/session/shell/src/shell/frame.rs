//! Per-frame scene builder shared between `bin/shots` and the native
//! compositor. Both build a `ShellOverlayHost` + `Chrome` from a
//! `WorkspaceTree`, maintain a `TaffyTree` keyed by widget pointers, run
//! layout (`compute_layout_with_measure` → `apply_layout`), and draw
//! into a fresh `vello::Scene`.
//!
//! `bin/shots`'s `ShellDriver` wraps a [`FrameBuilder`] and adds a
//! winit `Modifiers` snapshot, a focus cursor, the cached last-cursor
//! point, and click/drag/key synthesis. The native compositor only needs
//! the layout-and-paint core, so both paths render through the same
//! builder and their pixel output is byte-identical.

use crate::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, transaction::Transactable as _};
use taffy::prelude::*;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{
    BlendMode, Blob, Color, Compose, Extend, Fill, ImageAlphaType, ImageBrush, ImageData,
    ImageFormat, Mix,
};
use vello::{FineDamageRect, Scene};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::foreign::ForeignSurfaceWidget;
use crate::shell::overlay_host::{ShellOverlayHost, overlay_part};
use crate::shell::state::WorkspaceTree;

use ui::backdrop::{BackdropFrame, BackdropRecording, BackdropRegion, RecordedBackdrop};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

fn consume_record_requests(
    chrome: &Chrome,
    index: &std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
) {
    let requested = chrome.borrow_mut().take_tracked_record_requests();
    if requested.is_empty() {
        return;
    }
    let mut index = index.borrow_mut();
    let mut completed = Vec::new();
    for request in requested {
        let authorized = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| {
                    workspace.id == request.workspace_id && workspace.room() == request.room
                })
                .is_some_and(|workspace| {
                    // The queued actor records who made the request, but its
                    // role is only an admission-time snapshot. Rehydrate that
                    // same person against the target workspace so revocation
                    // before the frame is observed.
                    let actor = request.actor.principals.iter().find_map(|principal| {
                        if let collab_types::Principal::Person(user) = principal {
                            Some(workspace.actor_for_person(user))
                        } else {
                            None
                        }
                    });
                    actor.is_some_and(|actor| {
                        crate::shell::search::providers::actions::shell_action_authorized_for(
                            workspace,
                            &actor,
                            "record.bring",
                        )
                    })
                })
        };
        if !authorized {
            completed.push((
                request.request_id,
                Err("record.bring authority was revoked for its target workspace".into()),
            ));
            continue;
        }
        let Some(record) = ui::record::RecordId::parse(&request.record_id) else {
            completed.push((request.request_id, Err("record id became invalid".into())));
            continue;
        };
        index.reference(
            request.room,
            &record,
            &ui::record::RecordKind::new(request.kind),
        );
        completed.push((
            request.request_id,
            index
                .holds(request.room, &record)
                .then_some(())
                .ok_or_else(|| "the room record index refused the reference".into()),
        ));
    }
    drop(index);
    let mut state = chrome.borrow_mut();
    for (request_id, result) in completed {
        state.complete_record_request(request_id, result);
    }
}

struct FloatingTransferProxy {
    key: String,
    rect: Rect,
    image: ImageData,
}

/// A produced shell scene plus a conservative retained-damage relation to its predecessor.
/// `None` regions request a dense render; an empty region list is never emitted as sparse work.
pub struct FramePaint {
    pub scene: Scene,
    pub revision: u64,
    pub previous_revision: Option<u64>,
    pub damage_regions: Option<Vec<FineDamageRect>>,
    /// HDR surfaces drawn in `scene`, in scene pixels. A presenter driving an
    /// HDR output composites them at full precision; any other rendering of
    /// `scene` shows their tone-mapped SDR rendition.
    pub hdr_layers: Vec<crate::shell::hdr_surface::HdrLayer>,
    /// What `scene`'s floating surfaces are drawn over. A renderer that
    /// composites materials blurs these before rendering the scene; any
    /// other shows the surfaces opaque.
    pub backdrop: Option<Arc<BackdropFrame>>,
    /// Optional separable canvases; the screen-space scene remains authoritative.
    pub cache: Option<FrameCache>,
    /// The client image drawn on top of everything on its output, pixel for
    /// pixel, which a display covering exactly that image may scan out
    /// instead of rendering `scene`. Read it through
    /// [`FramePaint::scanout_candidate`]: anything drawn into `scene` after
    /// the frame builder found it voids it.
    pub scanout: Option<crate::shell::scanout_candidate::ScanoutCandidate>,
    /// For a presenter that keeps its target and moves pixels within it
    /// ([`FrameBuilder::set_scroll_copy_presenter`]): what a scroll in an
    /// in-process app moved, to copy instead of repainting where it lands.
    /// `damage_regions` stays every pixel the frame changes, so any other
    /// presenter ignores this.
    pub scroll_copies: Option<ScrollCopies>,
}

/// How a presenter that keeps its target presents a frame in which a scroll
/// moved what stays on screen ([`FramePaint::scroll_copies`]): copy, then
/// repaint the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScrollCopies {
    /// Copies within the target, in scene pixels, made in order before
    /// anything is repainted.
    pub copies: Vec<ui::scroll_copy::DeviceCopy>,
    /// What to repaint after them; [`FramePaint::damage_regions`] is this
    /// and where the copies land.
    pub raster: Vec<FineDamageRect>,
}

/// A strip drawn in canvas coordinates. The presenter shows the workspace
/// layer `scroll_x` pixels to the right of `dest_*`, in scene pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripBlit {
    pub dest_x: u32,
    pub dest_y: u32,
    pub dest_w: u32,
    pub dest_h: u32,
    pub scroll_x: u32,
}

/// A frame as layers a presenter retains between frames.
#[derive(Clone)]
pub struct FrameCache {
    /// Width of the widest strip's canvas in scene pixels: wider than the
    /// screen when a strip's content is wider than its viewport.
    pub width: u32,
    /// Where each strip of [`FrameLayers::strips`] is shown, and how far it
    /// is panned, in the same order.
    pub strips: Vec<StripBlit>,
    pub layers: FrameLayers,
    /// Tells this drawing of the desktop as layers apart from every other
    /// one its producer made, so a presenter knows which drawing each of
    /// its layer textures holds.
    pub generation: u64,
    /// Where [`FrameLayers::chrome`] differs from an earlier drawing's
    /// chrome; `None` when that is not known.
    pub chrome_damage: Option<ChromeDamage>,
}

/// Where the chrome of a drawing of the desktop as layers differs from the
/// chrome of the drawing `since` ([`FrameCache::generation`]), in scene
/// pixels and whole 16-pixel tiles: a presenter whose chrome texture still
/// holds that drawing's chrome brings it up to date by rasterising only
/// these rects. Measured on a pan's frames from the frame before and on the
/// layers drawn while the desktop rests ([`FrameBuilder::layers_at_rest`])
/// from the drawing at rest before, so the strip rail moving during a pan or
/// a clock tick at rest rasterises a few tiles, not the whole layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeDamage {
    pub since: u64,
    pub rects: Vec<FineDamageRect>,
}

/// The desktop's sections as layers, bottom to top, in scene pixels.
/// Drawing them over each other in order (source over) gives the frame's
/// scene ([`crate::shell::render::strip_layers::compose`]), so a presenter
/// can rasterize each into its own retained buffer and composite the
/// buffers: a strip's pan then moves that strip's pixels over a backdrop and
/// chrome that stay put, without drawing any of them again.
#[derive(Clone)]
pub struct FrameLayers {
    /// The wallpaper and anything drawn behind every window.
    pub backdrop: Scene,
    /// What of the workspace no strip carries, in place, below the strips.
    pub workspace: Scene,
    /// Each strip on screen, top to bottom, drawn on its own in canvas
    /// coordinates with the surfaces floating over it: shown inside its
    /// [`FrameCache::strips`] destination, read at its pan. Strips whose
    /// drawings meet (each draws its shadows past its row) each draw over
    /// the other there, as on screen.
    pub strips: Vec<Scene>,
    /// The damage parts each of `strips` marked, in the same order: one per
    /// window group, so a presenter rasterises only where a group's drawing
    /// changed. `None` for a strip drawn without well-nested parts.
    pub strip_parts: Vec<Option<ui::scene_parts::SectionParts>>,
    /// Everything above: notices over the desktop, the top bar, quick
    /// settings and every overlay above them.
    pub chrome: Scene,
}

/// The strips the host drew apart for a frame drawn as layers, in scene
/// pixels ([`FrameBuilder::panned_pieces`]).
struct PannedPieces {
    strips: Vec<Scene>,
    /// Each strip's damage parts, in the same order.
    parts: Vec<Option<ui::scene_parts::SectionParts>>,
}

/// Widest workspace layer, in scene pixels. Past this the strips keep their
/// pan in the scene.
const MAX_CANVAS_WIDTH: u32 = 16384;

/// When a producer whose presenter composites layers draws the frame as
/// layers ([`FramePaint::cache`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerPolicy {
    /// Every frame the layers can express.
    #[default]
    Always,
    /// Only while a strip pans, and `PAN_LINGER_FRAMES` frames after it
    /// stops. A frame at rest keeps its screen-space damage, so typing or a
    /// hover repaints what it changed, not a layer.
    WhilePanning,
}

/// Frames a [`LayerPolicy::WhilePanning`] producer keeps drawing layers
/// after the last strip pan moved, so a gesture that pauses for a frame
/// does not leave the layers and come back.
const PAN_LINGER_FRAMES: u32 = 4;

/// What the presenter asks of strip pans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StripPanPresenter {
    /// It reuses strip pixels, so pans land on whole pixels.
    pixel_aligned: bool,
    /// It composites strip pans from retained layers.
    composites_layers: bool,
}

/// The strips' pans as the latest frame drew them, and how many frames ago
/// one last moved.
#[derive(Debug)]
struct StripMotion {
    offsets: Vec<(u64, f64)>,
    still_frames: u32,
}

impl Default for StripMotion {
    fn default() -> Self {
        Self {
            offsets: Vec::new(),
            still_frames: PAN_LINGER_FRAMES,
        }
    }
}

impl StripMotion {
    /// Note the pans of this frame; whether a strip that was already there
    /// moved in the last [`PAN_LINGER_FRAMES`] frames.
    fn note(&mut self, offsets: Vec<(u64, f64)>) -> bool {
        let moved = offsets.iter().any(|(id, scroll)| {
            self.offsets
                .iter()
                .any(|(seen, before)| seen == id && (scroll - before).abs() > f64::EPSILON)
        });
        self.still_frames = if moved {
            0
        } else {
            self.still_frames.saturating_add(1)
        };
        self.offsets = offsets;
        self.still_frames < PAN_LINGER_FRAMES
    }
}

/// Layer composition is a Linux DRM capability; other presenters retain their dense path.
fn separable_layer(scene: &Scene) -> bool {
    #[cfg(target_os = "linux")]
    {
        !tilo_cpu_raster::reads_backdrop(scene.encoding())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = scene;
        false
    }
}

/// How the strips of one frame are drawn.
struct StripPlan {
    /// The strips are drawn in canvas coordinates.
    unbake: bool,
    width: u32,
    /// Every strip with a row on screen, top to bottom, as the host draws
    /// them apart ([`crate::shell::overlay_host::PannedSections::strips`]).
    strips: Vec<StripBlit>,
    /// The strip each of `strips` shows.
    ids: Vec<crate::shell::state::StripId>,
    /// The logical desktop → scene pixels, on the one display the scene is.
    to_scene: Affine,
}

impl StripPlan {
    /// Strips keep their pans in the scene this frame.
    fn baked(width: u32) -> Self {
        Self {
            unbake: false,
            width,
            strips: Vec::new(),
            ids: Vec::new(),
            to_scene: Affine::IDENTITY,
        }
    }
}

impl FramePaint {
    /// Every client frame any scene of this frame draws, pinned until the
    /// frame was presented (see [`crate::shell::gpu_frames::FramePins`]).
    ///
    /// Destructured without `..`: a scene added to `FramePaint` must be
    /// pinned here too, or a presenter could draw a client buffer after it
    /// went back to its client.
    pub fn client_frame_pins(&self) -> crate::shell::gpu_frames::FramePins {
        let Self {
            scene,
            revision: _,
            previous_revision: _,
            damage_regions: _,
            hdr_layers: _,
            backdrop,
            cache,
            // Its image is drawn by `scene`, pinned above. A presenter that
            // scans it out holds the client frame itself, until the flip
            // that replaces it retires.
            scanout: _,
            // Pixels the presenter already holds.
            scroll_copies: _,
        } = self;
        let mut pins = crate::shell::gpu_frames::FramePins::default();
        pins.add_scene(scene);
        if let Some(backdrop) = backdrop {
            for region in &backdrop.regions {
                pins.add_scene(&region.below);
            }
        }
        if let Some(FrameCache {
            width: _,
            strips: _,
            generation: _,
            chrome_damage: _,
            layers:
                FrameLayers {
                    backdrop,
                    workspace,
                    strips,
                    strip_parts: _,
                    chrome,
                },
        }) = cache
        {
            pins.add_scene(backdrop);
            pins.add_scene(workspace);
            for strip in strips {
                pins.add_scene(strip);
            }
            pins.add_scene(chrome);
        }
        pins
    }

    /// Wrap a scene from a producer without retained-damage authority.
    pub fn full(scene: Scene) -> Self {
        Self {
            scene,
            revision: 0,
            previous_revision: None,
            damage_regions: None,
            hdr_layers: Vec::new(),
            backdrop: None,
            cache: None,
            scanout: None,
            scroll_copies: None,
        }
    }

    /// Layers drawn while the desktop rests
    /// ([`FrameBuilder::layers_at_rest`]), for a presenter to retain
    /// ([`crate::shell::present::PresentPipe::warm`]): nothing to present.
    pub fn layers_at_rest(cache: FrameCache) -> Self {
        Self {
            cache: Some(cache),
            ..Self::full(Scene::new())
        }
    }

    /// [`Self::scanout`], while `scene` is still the scene it was found on
    /// top of.
    pub fn scanout_candidate(&self) -> Option<&crate::shell::scanout_candidate::ScanoutCandidate> {
        self.scanout
            .as_ref()
            .filter(|candidate| candidate.still_on_top_of(&self.scene))
    }

    /// The same frame, with its floating surfaces drawn over `backdrop`.
    #[must_use]
    pub fn with_backdrop(mut self, backdrop: Option<Arc<BackdropFrame>>) -> Self {
        if backdrop.is_some() {
            self.cache = None;
            // What a floating surface blurs moves with no copy of its own.
            self.repaint_copies();
        }
        self.backdrop = backdrop;
        self
    }

    /// The same frame, with `layers` composited where HDR is shown.
    #[must_use]
    pub fn with_hdr_layers(mut self, layers: Vec<crate::shell::hdr_surface::HdrLayer>) -> Self {
        if !layers.is_empty() {
            self.cache = None;
            self.repaint_copies();
        }
        self.hdr_layers = layers;
        self
    }

    /// This frame in place of `older`, a frame handed to the presenter before
    /// it and never rendered ([`crate::shell::present::PresentPipe`]: latest
    /// wins). The presenter then goes straight from the frame it holds, the
    /// one `older` was relative to, to this one: the damage is the union of
    /// both frames' damage, relative to `older`'s predecessor, when this frame
    /// was produced right after `older`, and the whole output otherwise.
    /// Copies are dropped: they move pixels of `older`'s image, which the
    /// presenter never had, and `damage_regions` already repaints where they
    /// land.
    #[must_use]
    pub fn superseding(mut self, older: &Self) -> Self {
        self.repaint_copies();
        let chained = self.revision != 0
            && older.revision != 0
            && self.previous_revision == Some(older.revision);
        self.damage_regions = match (&self.damage_regions, &older.damage_regions) {
            (Some(newer), Some(earlier)) if chained => {
                let mut union = earlier.clone();
                union.extend_from_slice(newer);
                Some(without_contained_regions(&union))
            }
            _ => None,
        };
        self.previous_revision = if chained {
            older.previous_revision
        } else {
            None
        };
        self
    }

    /// Drop the copies: `damage_regions` already repaints where they land.
    fn repaint_copies(&mut self) {
        self.scroll_copies = None;
    }

    /// External scene mutation invalidates every retained-damage proof.
    pub fn force_full(&mut self) {
        // Revision zero is deliberately never produced by `paint_with_damage`. It makes this
        // frame an unversioned boundary, so a later retained frame cannot reuse pixels from
        // commands appended outside the captured sections.
        self.revision = 0;
        self.damage_regions = None;
        self.previous_revision = None;
        self.scroll_copies = None;
        self.cache = None;
        // The appended commands are in the scene only; a presenter drawing
        // the layers would miss them.
        self.cache = None;
    }
}

#[derive(Clone)]
struct ProducedSections {
    foreign_images: Vec<super::foreign_damage::DrawnImage>,
    backdrop: Scene,
    topbar: Scene,
    workspace: Scene,
    workspace_overlays: Scene,
    /// Strips drawn in canvas coordinates, as the scene showed them.
    strips: Option<Vec<StripBlit>>,
    /// The [`FrameCache::generation`] of the frame these sections were drawn
    /// as layers in; `None` for a frame drawn in screen space.
    layers_generation: Option<u64>,
    topbar_bounds: Option<Rect>,
    quick_settings: Scene,
    above_quick_settings: Scene,
    quick_settings_bounds: Option<Rect>,
    /// Parts of the workspace, its overlays and the sections above Quick
    /// Settings, which bound damage inside them.
    parts: OpaqueParts,
    /// A filter following the pointer (the magnifier, the crosshair): every
    /// frame it is on is dense.
    global_filter: bool,
    /// The per-pixel tint over the whole desktop (night light, a colour
    /// filter). Sparse frames are exact under it while it stays the same.
    tint: Tint,
    revision: u64,
}

/// [`scene_parts`](ui::scene_parts) of the sections whose damage is bounded
/// only inside their parts. `None` for a section drawn without a recording.
#[derive(Clone, Default)]
struct OpaqueParts {
    workspace: Option<ui::scene_parts::SectionParts>,
    workspace_overlays: Option<ui::scene_parts::SectionParts>,
    /// The top bar's own pieces (the strip it names): the bar is bounded as
    /// a whole too, so its parts only narrow it.
    topbar: Option<ui::scene_parts::SectionParts>,
    /// The Quick Settings panel, the strip ribbon and the strip rail: that
    /// section is bounded as a whole too, so its parts only narrow it.
    quick_settings: Option<ui::scene_parts::SectionParts>,
    above_quick_settings: Option<ui::scene_parts::SectionParts>,
}

impl OpaqueParts {
    fn of(
        recorded: &ui::scene_parts::Recorded,
        sections: &crate::shell::overlay_host::ShellSceneSections,
    ) -> Self {
        Self {
            workspace: recorded.of(&sections.workspace),
            workspace_overlays: recorded.of(&sections.workspace_overlays),
            topbar: recorded.of(&sections.topbar),
            quick_settings: recorded.of(&sections.quick_settings),
            above_quick_settings: recorded.of(&sections.above_quick_settings),
        }
    }
}

/// The tint drawn over the whole desktop: a pure per-pixel function of what
/// is below it, so it changes nothing about which pixels a frame changes as
/// long as it is itself the same.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Tint {
    night_light: Option<Color>,
    color_filter: ColorFilter,
    viewport: (f64, f64),
}

enum QuickSettingsDamage {
    Absent,
    Bounds(FineDamageRect),
    Unknown,
}

// The four flags are independent lifecycle signals from the handoff protocol;
// wrapping them would only obscure the truth table at this private boundary.
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "suspension, native presentation, pending source clear and mapped pixels are independent handoff signals in this truth table"
)]
fn floating_transfer_proxy_needed(
    suspended: bool,
    native_presented: bool,
    source_clear_pending: bool,
    native_pixels_mapped: bool,
) -> bool {
    if suspended {
        // Reverse handoff: the native surface is already hidden, but its last
        // frame remains until the primary app window confirms replacement.
        source_clear_pending
    } else {
        !native_presented || source_clear_pending || !native_pixels_mapped
    }
}

impl FloatingTransferProxy {
    fn new(key: String, rect: Rect, frame: &crate::shell::floating::FloatingTransferFrame) -> Self {
        Self {
            key,
            rect,
            image: ImageData {
                data: Blob::new(frame.rgba.clone()),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::AlphaPremultiplied,
                width: frame.width,
                height: frame.height,
            },
        }
    }

    fn draw(&self, scene: &mut Scene) {
        if self.rect.area() <= 0.0 || self.image.width == 0 || self.image.height == 0 {
            return;
        }
        let sx = self.rect.width() / f64::from(self.image.width);
        let sy = self.rect.height() / f64::from(self.image.height);
        let brush_transform =
            Affine::translate((self.rect.x0, self.rect.y0)).pre_scale_non_uniform(sx, sy);
        let brush = ImageBrush::new(self.image.clone()).with_extend(Extend::Pad);
        // The frozen primary buffer is opaque outside the app's surface.
        // Mask those corner pixels with the exact shared floating silhouette
        // so the warm-up bridge reads as the same soft object, not a crop box.
        let silhouette = ui::surface::soft_outline(self.rect.inset(2.0));
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &brush,
            Some(brush_transform),
            &silhouette,
        );
    }
}

/// Multipliers are deliberately conservative: the selected Kelvin value is
/// a comfort target, not a claim that an uncalibrated panel now has reference
/// colourimetry. Red stays at unity while green/blue fall as warmth rises.
fn night_light_multiplier(kelvin: u32) -> Color {
    #[allow(
        clippy::cast_precision_loss,
        reason = "kelvin is clamped to 2_000..=6_500 before this subtraction, nowhere near f32's precision limit"
    )]
    let warmth =
        ((6_500_u32.saturating_sub(kelvin.clamp(2_000, 6_500))) as f32 / 4_500.0).clamp(0.0, 1.0);
    // warmth is clamped to 0.0..=1.0 above, so green/blue land in 203..=255 and 123..=255 respectively — always a non-negative u8.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=255.0 immediately above"
    )]
    let (green, blue) = (
        (255.0 - 52.0 * warmth).round().clamp(0.0, 255.0) as u8,
        (255.0 - 132.0 * warmth).round().clamp(0.0, 255.0) as u8,
    );
    Color::from_rgb8(255, green, blue)
}

/// Whether night light shows: on the schedule it follows the day/night phase,
/// otherwise the manual switch.
fn night_light_on(manual: bool, scheduled: bool, night: Option<bool>) -> bool {
    if scheduled {
        night.unwrap_or(false)
    } else {
        manual
    }
}

use settings_store::magnifier::{MagnifierState, MagnifierView};
use tilo_accessibility_settings::{AccessibilitySettings, ColorFilter};

#[derive(Clone, Copy, Debug, PartialEq)]
struct VisualAccessibility {
    color_filter: ColorFilter,
    magnifier: MagnifierState,
    cursor_crosshair: bool,
}

impl Default for VisualAccessibility {
    fn default() -> Self {
        Self::of(AccessibilitySettings::default())
    }
}

impl VisualAccessibility {
    fn load() -> Self {
        Self::of(AccessibilitySettings::load())
    }

    fn of(settings: AccessibilitySettings) -> Self {
        Self {
            color_filter: settings.color_filter,
            magnifier: settings.magnifier(),
            cursor_crosshair: settings.cursor_crosshair,
        }
    }
}

use crate::shell::output_scene::{self, OutputGeometry};

/// Whether `widget` is a terminal tab's root: the terminal's own widget, or
/// the framework root of the terminal mounted as its component, which
/// answers to the terminal's address.
fn is_terminal(widget: &dyn Widget) -> bool {
    widget.name() == "Terminal" || matches!(widget.test_id(), Some("terminal" | "so.tilo.terminal"))
}

/// Layout + paint core shared between `bin/shots`'s `ShellDriver` and
/// the native compositor's per-frame tick.
///
/// Owning this gives the caller the ability to mutate chrome state and
/// re-paint — nothing more. Input dispatch, scenario stepping and scene
/// catalogues stay in `bin/shots/driver.rs`; the compositor gets input
/// through the smithay seat layer and translates it into mutations on
/// the same `Chrome` instead.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent compositor state flags describe unrelated display, input, and accessibility features"
)]
pub struct FrameBuilder {
    // Layout retains raw widget pointers into the host and its inline fields.
    // Keep their addresses stable when the FrameBuilder itself is moved.
    host: Box<ShellOverlayHost>,
    chrome: Chrome,
    /// The [`WorkspaceTree::room_epoch`] this builder has already reacted
    /// to; compared each frame by [`Self::pump_room_contents`].
    last_room_epoch: u64,
    /// The tree revisions at which [`Self::pump_room_contents`] last walked
    /// the tabs and found no window owed a look. While they hold, the walk
    /// is skipped; `None` means a look is owed.
    peer_windows_quiet_at: Option<TreeRevisions>,
    taffy: TaffyTree<()>,
    /// Widget-pointer map captured during `build_layout`, kept alive so the
    /// admitted shared layout solver can re-borrow each frame.
    node_map: HashMap<NodeId, *mut dyn Widget>,
    ime_stamp_key: zeroize::Zeroizing<[u8; 32]>,
    /// Inner node — the host's own root child, target of `apply_layout` so
    /// the host's rect tree roots at the viewport origin.
    host_node: Option<NodeId>,
    /// Outer node — a viewport-sized wrapper giving `compute_layout` an
    /// explicit root size (Taffy needs a definite extent; the host itself is
    /// `percent(1.0)`).
    root_node: Option<NodeId>,
    #[cfg(test)]
    layout_tree_builds: u64,
    #[cfg(test)]
    layout_passes: u64,
    text_renderer: TextRenderer,
    theme: Theme,
    /// The theme the settings document describes, before the active
    /// workspace's temperature and the person's colour dress it into
    /// [`Self::theme`].
    configured_theme: Theme,
    watch_configured_theme: bool,
    /// Offscreen capture asked for a theme or text scale
    /// ([`Self::shots_theme_override`]), so the settings file is not
    /// watched. Read once when watching starts, not on every paint.
    shots_theme_pinned: bool,
    /// The session's settings document, followed off the compositor
    /// thread: paint reads its snapshot, never the file. The compositor
    /// hands over its own ([`Self::follow_settings`]); a builder that watches
    /// the configured theme without one starts its own at its first look.
    live_settings: Option<settings_store::live::LiveSettings>,
    /// Every workspace's temperature and the person's colour: what the
    /// chrome wears for the active workspace, and what each app window is
    /// sent for the workspace it is in.
    windows: super::window_appearance::WindowAppearances,
    /// The generation of the followed settings the theme was last read at.
    theme_store_stamp: Option<u64>,
    last_theme_store_check: Instant,
    appearance_generation: u64,
    night_light: bool,
    night_light_temperature: u32,
    /// The day/night schedule an "Automatic" theme or a scheduled night light
    /// follows, and whether it was night when last applied. Sunrise and sunset
    /// change what is drawn without changing the settings file.
    day_phase: Option<(settings_store::day_schedule::Scheduled, bool)>,
    visual_accessibility: VisualAccessibility,
    /// Where the magnifier looks: the pointer, or the keyboard focus when it moved last.
    magnifier_focus: crate::shell::magnifier::FocusTracker,
    /// The keyboard focus rectangle the compositor last reported, while the magnifier is on.
    keyboard_focus_rect: Option<Rect>,
    /// Speaks the chrome when the screen reader is on; silent otherwise.
    narrator: crate::shell::shell_narrator::ShellNarrator,
    /// The logical desktop the shell lays out in: the bounding box of every
    /// display's logical rectangle.
    viewport: (f64, f64),
    /// The physical-pixel scene the displays present from. Equal to
    /// `viewport` unless a display draws at a scale other than 1.
    scene_size: (f64, f64),
    /// The displays behind `viewport`, when known. The viewport is the
    /// *bounding box* of every output, so on multi-monitor it isn't a
    /// display anyone has — a group sized as a fraction of it would span
    /// both monitors. `None` means "one display, the size of the viewport".
    outputs: Option<Vec<OutputGeometry>>,
    /// Live peer presence for this session. Persistent rather than rebuilt
    /// per paint — a cursor is *somewhere* between the frames that move it.
    presence: PresenceCtx,
    /// Source that fills [`Self::presence`]. `None` in a session with
    /// no peer sync, which paints no peers because there are none.
    peer_presence: Option<crate::shell::peer_presence::PeerPresence>,
    /// Compositor-authorised OSK, deliberately outside the workspace/tile
    /// tree. Its widget only mirrors pixels; protocol trust lives in State.
    osk_overlay: Option<ForeignSurfaceWidget>,
    /// Authenticated app-owned call/navigation toplevels. Like the OSK these
    /// are client buffers outside the workspace tree, but each carries
    /// its own desktop placement and native input target.
    floating_overlays: Vec<(u64, ForeignSurfaceWidget)>,
    /// Per-output fullscreen client painted above the unchanged tiled scene.
    fullscreen_overlay: Option<(u64, ForeignSurfaceWidget)>,
    /// A system-modal prompt is up: the desktop is dimmed beneath it.
    system_modal_scrim: bool,
    /// The session is locked: a capture sees nothing of the desktop at all.
    session_locked: bool,
    /// XDG and input-method transient surfaces.  These are intentionally
    /// outside the workspace tree: they follow parent geometry and paint
    /// above the app that owns them without becoming a window/tab.
    transient_overlays: Vec<(u64, ForeignSurfaceWidget)>,
    /// Pixel-exact final app frames shown only while a newly authorised
    /// secondary surface is preparing its first buffer.
    floating_transfer_proxies: Vec<FloatingTransferProxy>,
    /// The centre of the field the on-screen keyboard types into.
    osk_anchor: Option<vello::kurbo::Point>,
    osk_active: bool,
    /// [`crate::compositor::state::State::osk_commits`], for the state dump.
    osk_commits: u64,
    /// The keyboard serves one of the shell's own fields.
    osk_serves_shell: bool,
    /// The lock screen's keyboard keys by name and where they sit, while it
    /// shows; for the state dump.
    lock_keyboard_keys: Vec<(String, Rect)>,
    /// The lock screen's surfaces as last drawn, one per display; for the
    /// state dump.
    lock_surfaces: Vec<crate::compositor::lock_screen::LockSurface>,
    /// A layout rebuild invalidates every Taffy node id held by the native
    /// input loop. Re-seat its external focus cursor before the next event;
    /// otherwise the first-login wizard starts unfocused and a step change
    /// can leave keyboard input pointing at a widget that no longer exists.
    focus_tree_changed: bool,
    /// Stable position used to preserve focus across incidental rebuilds,
    /// matching `ui::App`; an explicit focus-reset ignores it and starts the
    /// replacement wizard step at its first enabled control.
    native_focus_ordinal: Option<usize>,
    /// Return to the same named control, not a node id from a discarded tree.
    palette_focus_return: Option<(Option<crate::shell::state::TabId>, Vec<String>)>,
    palette_focus_active: bool,
    /// Latest produced sections, not an acknowledgement. A skipped presenter therefore creates a
    /// revision mismatch at the bridge and safely selects its dense path.
    last_produced_sections: Option<ProducedSections>,
    next_scene_revision: u64,
    /// Capacity of this builder's actual presenter; absent for dense captures.
    retained_layer_width: Option<u32>,
    /// Which frames that presenter gets as layers.
    layer_policy: LayerPolicy,
    /// What [`LayerPolicy::WhilePanning`] tells a pan by.
    strip_motion: StripMotion,
    /// The frame being drawn is the desktop as layers whatever the policy
    /// ([`Self::layers_at_rest`]), not a frame.
    drawing_layers_at_rest: bool,
    /// The host saw nothing but motion since the last frame
    /// ([`Self::set_frame_only_moved`]); the next scene build may replay
    /// windows at rest that stay where they were drawn.
    frame_only_moved: bool,
    /// Scene builds so far, numbering each for [`ui::retained_present::PanReplay`].
    scene_builds: u64,
    /// A frame changed the desktop since the presenter's layers were last
    /// drawn ([`Self::layers_stale`]).
    layers_stale: bool,
    /// The last [`FrameCache::generation`] handed out.
    layers_generation: u64,
    /// Why the last frame of a presenter that wants every frame as layers
    /// ([`LayerPolicy::Always`]) came without them, once said.
    layers_off: Option<&'static str>,
    /// The sections of the latest drawing of the desktop as layers made at
    /// rest, and its generation: what the next one's [`ChromeDamage`] is
    /// measured from. `None` once a frame came as layers of its own.
    layers_drawn: Option<(u64, ProducedSections)>,
    /// What the presenter asks of strip pans
    /// ([`Self::set_strip_pan_presenter`]): whole-pixel pans, and layers.
    strip_pan_presenter: StripPanPresenter,
    /// The presenter moves pixels within its target
    /// ([`Self::set_scroll_copy_presenter`]).
    scroll_copy_presenter: bool,
    /// Bounds what the sections draw outside their parts, for scroll
    /// copies; it keeps glyph outlines between frames.
    draw_bounds: std::cell::RefCell<tilo_cpu_raster::DrawBounds>,
    /// HDR layers of the latest [`Self::paint`], until taken.
    painted_hdr_layers: Vec<crate::shell::hdr_surface::HdrLayer>,
    /// The backdrops the latest [`Self::paint`] drew floating surfaces over.
    painted_backdrop: Option<Arc<BackdropFrame>>,
    /// What the pointer shows right now, so captures draw the same image.
    pointer_image: crate::compositor::cursor::PointerImage,
    /// The session network, once peer sync is up, so a control invoke can
    /// address an enrolled device.
    enrolled_actions: Option<std::sync::Arc<crate::shell::network::ShellNetwork>>,
    /// Frames whose layout failed, skipped, and when that was last logged.
    skipped_frames: ui::widget::SkippedFrames,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AnimationDemand {
    pub host: u32,
    pub floating: bool,
    pub transient: bool,
    pub osk: bool,
}

impl AnimationDemand {
    pub(crate) fn any(self) -> bool {
        self.host != 0 || self.floating || self.transient || self.osk
    }
}

struct FramePresentationInputs {
    text_renderer: TextRenderer,
    theme: Theme,
    own_identity: String,
    theme_store_stamp: Option<u64>,
}

/// Every counter that moves when the desktop tree may have changed: a peer's
/// round landing (`room_epoch`, from the network thread), a mutation outside
/// retained chrome (`external_revision`), and the chrome's own revision. The
/// overlay host's strip reconcile relies on the same three to see a new tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TreeRevisions {
    room_epoch: u64,
    external: u64,
    chrome: u64,
}

impl TreeRevisions {
    fn read(chrome: &ChromeState, tree: &WorkspaceTree) -> Self {
        Self {
            room_epoch: tree.desktop.room_epoch(),
            external: tree.desktop.external_revision,
            chrome: chrome.revision,
        }
    }
}

/// The theme `shots --theme NAME` asked for; read before
/// `TILO_SHOTS_THEME`, for callers that cannot set the environment.
static SHOTS_THEME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Render every frame builder this process creates in the theme `name`
/// (`dark`, `light`, `high-contrast`). Offscreen capture only; the first
/// request wins.
pub fn request_shots_theme(name: String) {
    let _ = SHOTS_THEME.set(name);
}

/// Whether offscreen capture asked for a theme or a text scale, which then
/// wins over the settings file.
fn shots_theme_requested() -> bool {
    SHOTS_THEME.get().is_some()
        || std::env::var_os("TILO_SHOTS_THEME").is_some()
        || std::env::var_os("TILO_SHOTS_TEXT_SCALE").is_some()
}

/// Whether `app` may write `workspace`'s room for `person` at `now`: the
/// room lets the person change the app's content and makes the app one of
/// its authors, the decision a launch opens the app writable or read-only
/// by. `None` for a person the room has no role for here (this device's view
/// of the room names someone else), so nothing is decided on it.
fn app_writes(
    workspace: &crate::shell::state::Workspace,
    person: &str,
    app: &str,
    now: u64,
) -> Option<bool> {
    workspace.role_of(person)?;
    let changes = workspace.answer_at(
        person,
        &workspace.node().app(app),
        tilo_action_policy::ActionKind::Undoable,
        now,
    ) == tilo_action_policy::Answer::Allow;
    Some(changes && workspace.app_is_author_at(app, now))
}

impl FrameBuilder {
    /// Build a frame builder around the canonical bootstrap workspace
    /// tree at `viewport` size.
    pub fn new(viewport: (u32, u32)) -> Self {
        Self::with_tree(WorkspaceTree::bootstrap_demo(), viewport)
    }

    /// Build a frame builder around an explicit `WorkspaceTree` — used by
    /// `bin/shots`'s `window` pre-roll path (loads tree state from disk first).
    #[allow(
        clippy::needless_pass_by_value,
        reason = "taking &WorkspaceTree would ripple into bin/shots/driver.rs's ShellDriver::with_tree, out of scope for this change; its only caller already owns a tree it discards right after this call"
    )]
    pub fn with_tree(tree: WorkspaceTree, viewport: (u32, u32)) -> Self {
        Self::from_parts(build_tilo_overlay_host_with_chrome(&tree), viewport)
    }

    /// Optional presentation overrides for offscreen `shots` capture:
    /// `TILO_SHOTS_THEME=<preset>` (any name `Theme::named` finds) and
    /// `TILO_SHOTS_TEXT_SCALE=<factor>`. Unset keeps the dark default — lets
    /// a theme-driven chrome change be proven in high contrast and at large
    /// text, not just its comfortable default density.
    fn shots_theme_override() -> Option<Theme> {
        let requested_theme = SHOTS_THEME
            .get()
            .cloned()
            .or_else(|| std::env::var("TILO_SHOTS_THEME").ok());
        let requested_scale = std::env::var("TILO_SHOTS_TEXT_SCALE")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite());
        if requested_theme.is_none() && requested_scale.is_none() {
            return None;
        }
        let theme = requested_theme
            .as_deref()
            .and_then(|name| Theme::named(name.trim()))
            .unwrap_or_else(Theme::dark);
        Some(if let Some(scale) = requested_scale {
            theme.with_text_scale(scale)
        } else {
            theme
        })
    }

    /// Build a frame builder over a *shared* [`WorkspaceTreeHandle`](crate::shell::state::WorkspaceTreeHandle), with
    /// the peer-sync palette rows on or off.
    ///
    /// As [`Self::with_tree`], with `inputs` as its owned fixture's hosted
    /// inputs: a capture gives them the clock its frames show.
    pub fn with_tree_and_inputs(
        tree: &WorkspaceTree,
        viewport: (u32, u32),
        inputs: crate::shell::hosted_inputs::HostedTileInputs,
    ) -> Self {
        Self::from_parts(
            crate::shell::build_tilo_overlay_host_with_inputs(
                tree.clone().shared(),
                false,
                ui::record::RecordStore::in_memory(),
                inputs,
            ),
            viewport,
        )
    }

    /// The compositor needs this rather than [`Self::with_tree`] because
    /// peer-sync writes into the same tree the chrome reads: a replica
    /// applying a peer's change and the strip that paints it must look at
    /// one `Arc<RwLock<_>>`, not two copies.
    pub fn with_handle(
        tree: crate::shell::state::WorkspaceTreeHandle,
        enable_peer_sync: bool,
        viewport: (u32, u32),
    ) -> Self {
        Self::from_parts(
            Self::with_live_status(crate::shell::build_tilo_overlay_host_with_handle(
                tree,
                enable_peer_sync,
            )),
            viewport,
        )
    }

    /// As [`Self::with_handle`], but keeping each open tab's document in
    /// `records` (the session's own store) rather than the in-memory
    /// default a probe or `--shots` run uses.
    pub fn with_handle_and_records(
        tree: crate::shell::state::WorkspaceTreeHandle,
        enable_peer_sync: bool,
        viewport: (u32, u32),
        records: ui::record::RecordStore,
    ) -> Self {
        Self::from_parts(
            Self::with_live_status(crate::shell::build_tilo_overlay_host_with_records(
                tree,
                enable_peer_sync,
                records,
            )),
            viewport,
        )
    }

    /// Attach a running status poller to a freshly-built host. Only the two
    /// real-session constructors go through here — a `--shots` tray driven
    /// by the rendering machine's own battery/Wi-Fi would make every golden
    /// image depend on which laptop produced it.
    fn with_live_status(parts: (ShellOverlayHost, Chrome)) -> (ShellOverlayHost, Chrome) {
        let (host, chrome) = parts;
        let bridge = crate::shell::status::StatusBridge::new();
        bridge.spawn();
        (host.with_status_bridge(bridge), chrome)
    }

    /// Construct the actual shell widgets with owned files and absent ambient services.
    /// # Errors
    /// Rejects persistent trees, unsupported tiles, or missing fixture services before widgets exist.
    pub fn with_fixture_inputs(
        tree: WorkspaceTree,
        viewport: (u32, u32),
        inputs: super::fixture_inputs::ShellFixtureInputs,
    ) -> Result<Self, super::fixture_inputs::ShellFixtureError> {
        super::fixture_inputs::validate(&tree, &inputs.hosted)?;
        let parts =
            super::build_tilo_overlay_host_with_fixture_inputs(tree.shared(), inputs.hosted)?;
        Ok(Self::from_parts_with_presentation(
            parts,
            viewport,
            FramePresentationInputs {
                text_renderer: inputs.text_renderer,
                theme: inputs.theme,
                own_identity: "shell-fixture".into(),
                theme_store_stamp: None,
            },
        ))
    }

    fn from_parts(parts: (ShellOverlayHost, Chrome), viewport: (u32, u32)) -> Self {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("Frame production presentation");
        let presentation = FramePresentationInputs {
            theme: Self::shots_theme_override().unwrap_or_else(Theme::dark),
            text_renderer: TextRenderer::new(),
            own_identity: crate::shell::state::local_user_id(),
            theme_store_stamp: None,
        };
        Self::from_parts_with_presentation(parts, viewport, presentation)
    }

    fn from_parts_with_presentation(
        (host, chrome): (ShellOverlayHost, Chrome),
        viewport: (u32, u32),
        presentation: FramePresentationInputs,
    ) -> Self {
        let mut builder = Self {
            host: Box::new(host),
            chrome,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            ime_stamp_key: {
                let a = uuid::Uuid::new_v4();
                let b = uuid::Uuid::new_v4();
                let mut key = [0; 32];
                for (slot, value) in key.iter_mut().zip(a.as_bytes().iter().chain(b.as_bytes())) {
                    *slot = *value;
                }
                zeroize::Zeroizing::new(key)
            },
            host_node: None,
            root_node: None,
            #[cfg(test)]
            layout_tree_builds: 0,
            #[cfg(test)]
            layout_passes: 0,
            text_renderer: presentation.text_renderer,
            last_room_epoch: 0,
            peer_windows_quiet_at: None,
            configured_theme: presentation.theme.clone(),
            theme: presentation.theme,
            watch_configured_theme: false,
            shots_theme_pinned: false,
            live_settings: None,
            windows: super::window_appearance::WindowAppearances::new(
                ui::tokens::PresenceHue::Blue,
            ),
            theme_store_stamp: presentation.theme_store_stamp,
            last_theme_store_check: Instant::now(),
            appearance_generation: 0,
            night_light: false,
            night_light_temperature: 3_700,
            day_phase: None,
            visual_accessibility: VisualAccessibility::default(),
            magnifier_focus: crate::shell::magnifier::FocusTracker::default(),
            keyboard_focus_rect: None,
            narrator: crate::shell::shell_narrator::ShellNarrator::default(),
            viewport: (f64::from(viewport.0), f64::from(viewport.1)),
            scene_size: (f64::from(viewport.0), f64::from(viewport.1)),
            outputs: None,
            presence: {
                // The broker fans every local app's frame to the whole room
                // including this subscription, so excluding our own identity avoids
                // the owner seeing a presence pill over their own window.
                let mut ctx = PresenceCtx::default();
                ctx.set_own_identity([presentation.own_identity]);
                ctx
            },
            peer_presence: None,
            osk_overlay: None,
            floating_overlays: Vec::new(),
            fullscreen_overlay: None,
            system_modal_scrim: false,
            session_locked: false,
            transient_overlays: Vec::new(),
            floating_transfer_proxies: Vec::new(),
            osk_anchor: None,
            osk_active: false,
            osk_commits: 0,
            osk_serves_shell: false,
            lock_keyboard_keys: Vec::new(),
            lock_surfaces: Vec::new(),
            focus_tree_changed: true,
            native_focus_ordinal: None,
            palette_focus_return: None,
            palette_focus_active: false,
            last_produced_sections: None,
            next_scene_revision: 1,
            retained_layer_width: None,
            layer_policy: LayerPolicy::default(),
            strip_motion: StripMotion::default(),
            drawing_layers_at_rest: false,
            frame_only_moved: false,
            scene_builds: 0,
            layers_stale: false,
            layers_generation: 0,
            layers_off: None,
            layers_drawn: None,
            strip_pan_presenter: StripPanPresenter::default(),
            scroll_copy_presenter: false,
            draw_bounds: std::cell::RefCell::default(),
            painted_hdr_layers: Vec::new(),
            painted_backdrop: None,
            pointer_image: crate::compositor::cursor::PointerImage::default(),
            enrolled_actions: None,
            skipped_frames: ui::widget::SkippedFrames::default(),
        };
        builder.rebuild_layout_tree();
        builder.relayout();
        builder
    }

    /// The displays to resolve window-group widths against: whatever the
    /// compositor last reported, else a single display the size of the viewport.
    fn effective_outputs(&self) -> Vec<OutputGeometry> {
        self.outputs.clone().unwrap_or_else(|| {
            vec![OutputGeometry::unscaled(
                0.0,
                0.0,
                self.viewport.0,
                self.viewport.1,
            )]
        })
    }

    /// The displays the shell lays out and paints across: whatever the
    /// compositor last reported, else one unscaled display the size of the
    /// viewport.
    pub fn output_geometry(&self) -> Vec<OutputGeometry> {
        self.effective_outputs()
    }

    /// Remember the session network so a later control invoke can address an
    /// enrolled device.
    pub fn set_enrolled_action_network(
        &mut self,
        network: std::sync::Arc<crate::shell::network::ShellNetwork>,
    ) {
        self.enrolled_actions = Some(network);
    }

    /// Hand one native action to an enrolled device on a shared workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when this session has no network, or the network
    /// refuses the device or the address.
    pub fn invoke_enrolled_device(
        &self,
        device: &str,
        provider: &str,
        id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let network = self
            .enrolled_actions
            .as_ref()
            .ok_or_else(|| "this session is not on the network".to_string())?;
        let address = tilo_actions::ActionAddress::parse(id)
            .ok_or_else(|| "action id is not a resource address".to_string())?;
        let argument = if args.is_empty() {
            tilo_actions::remote::InvokeArgument::None
        } else {
            tilo_actions::remote::InvokeArgument::Json(args.clone())
        };
        network.invoke_enrolled_device(
            device,
            tilo_actions::remote::RemoteActionTarget {
                provider: provider.to_string(),
                address,
            },
            argument,
        )
    }

    /// Tell the frame builder which displays it's painting across, so a
    /// group's width is a fraction of its own monitor rather than the
    /// combined desktop, and each display draws the desktop at its own
    /// scale. The logical viewport and the scene size follow the displays.
    /// Called at startup and on hotplug/mode change.
    pub fn set_output_geometry(&mut self, outputs: Vec<OutputGeometry>) {
        if outputs.is_empty() || self.outputs.as_deref() == Some(outputs.as_slice()) {
            return;
        }
        let (lw, lh) = output_scene::logical_extent(&outputs);
        let (sw, sh) = output_scene::scene_extent(&outputs);
        if lw > 0.0 && lh > 0.0 {
            self.viewport = (lw, lh);
        }
        if sw > 0.0 && sh > 0.0 {
            self.scene_size = (sw, sh);
        }
        self.outputs = Some(outputs);
        self.align_strip_pans();
        self.rebuild_layout_tree();
    }

    /// The physical-pixel scene [`Self::paint`] produces: what a renderer
    /// target or a whole-desktop capture is sized to.
    pub fn scene_size(&self) -> (f64, f64) {
        self.scene_size
    }

    /// A logical desktop point in scene pixels, through the display it is on.
    pub fn layout_point_to_scene(&self, p: Point) -> Point {
        match &self.outputs {
            Some(outputs) => output_scene::point_to_scene(outputs, p),
            None => p,
        }
    }

    /// The scene pixels a logical desktop rectangle covers, across every
    /// display it touches.
    pub fn layout_rect_to_scene(&self, rect: Rect) -> Option<Rect> {
        match &self.outputs {
            Some(outputs) => output_scene::rect_to_scene(outputs, rect),
            None => Some(rect),
        }
    }

    /// Append a scene drawn in logical desktop coordinates to a painted
    /// scene, drawn by every display at its own scale — for compositor-owned
    /// layers (drag feedback, audience surfaces) added after [`Self::paint`].
    pub fn compose_layout_scene(&self, logical: &Scene, target: &mut Scene) {
        match &self.outputs {
            Some(outputs) => output_scene::compose(outputs, logical, target),
            None => target.append(logical, None),
        }
    }

    fn rebuild_layout_tree(&mut self) {
        #[cfg(test)]
        {
            self.layout_tree_builds = self
                .layout_tree_builds
                .checked_add(1)
                .expect("test layout-tree build counter exhausted");
        }
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        // Push display geometry before the tree is built: the strips read it
        // to resolve each group's width fraction in this same pass.
        let outputs = self.effective_outputs();
        self.host.set_output_geometry(&outputs);
        let child = self
            .host
            .build_layout(&mut self.taffy, &self.theme, &mut self.node_map);
        let (w, h) = self.viewport;
        // Layout dimensions are bounded logical-pixel viewport sizes; the
        // f64->f32 narrowing for taffy's `Dimension` cannot lose meaningful
        // precision at UI scale.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel viewport size; f32 precision is sufficient"
        )]
        let root = self.taffy.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        );
        match root {
            Ok(root) => {
                self.host_node = Some(child);
                self.root_node = Some(root);
            }
            Err(err) => {
                // `child` was just handed back by `self.host.build_layout`
                // against the `TaffyTree` reset a few lines up, so wrapping
                // it in a fresh root node cannot fail in practice. If
                // taffy's internal bookkeeping ever disagrees, degrade to
                // "no layout this frame" rather than aborting the whole
                // compositor session (this crate builds with `panic = "abort"`).
                tracing::error!(
                    %err,
                    "frame-builder: failed to build root taffy node; skipping this rebuild"
                );
                self.host_node = None;
                self.root_node = None;
            }
        }
        self.focus_tree_changed = true;
    }

    /// Enabled keyboard controls in the same parent-before-children DFS used
    /// by `ui::App`. Disabled controls are deliberately absent so Tab can
    /// never land on an unavailable OOBE action.
    fn focusable_order(&self) -> Vec<NodeId> {
        fn collect(
            widget: &dyn Widget,
            node_map: &HashMap<NodeId, *mut dyn Widget>,
            order: &mut Vec<NodeId>,
        ) {
            let pointer = std::ptr::from_ref(widget);
            if widget.is_focusable()
                && widget.is_enabled()
                && let Some((&node, _)) = node_map.iter().find(|(_, mapped)| {
                    // `TileHost` children are `dyn AppWidget`; comparing the
                    // full `dyn Widget` fat pointer misses the same heap
                    // object because the vtables differ.
                    std::ptr::eq(
                        (**mapped as *const dyn Widget).cast::<()>(),
                        pointer.cast::<()>(),
                    )
                })
            {
                order.push(node);
            }
            for child in widget.children() {
                collect(child, node_map, order);
            }
        }

        fn deepest_trap(
            widget: &dyn Widget,
            node_map: &HashMap<NodeId, *mut dyn Widget>,
        ) -> Option<Vec<NodeId>> {
            if widget.traps_focus() {
                let mut order = Vec::new();
                collect(widget, node_map, &mut order);
                return Some(order);
            }
            for child in widget.children() {
                if let Some(order) = deepest_trap(child, node_map) {
                    return Some(order);
                }
            }
            None
        }

        let mut order = Vec::new();
        if self.host_node.is_some() {
            if let Some(trapped) = deepest_trap(self.host.as_ref(), &self.node_map) {
                return trapped;
            }
            collect(self.host.as_ref(), &self.node_map, &mut order);
        }
        order
    }

    fn set_native_focus_with_reveal(
        &mut self,
        focus: &mut Option<NodeId>,
        target: Option<NodeId>,
        reveal: bool,
    ) {
        if *focus == target
            && self.node_map.iter().all(|(&node, &widget)| {
                // SAFETY: every pointer belongs to the current live tree.
                (unsafe { (*widget).is_focused() }) == (Some(node) == target)
            })
        {
            return;
        }
        // Rebuilt overlays can retain their own focus while the compositor
        // still names a desktop control. Only one live widget may own the
        // keyboard and its accessibility cursor, even when ids are replaced.
        for &widget in self.node_map.values() {
            // SAFETY: every pointer belongs to the current live tree.
            unsafe { (*widget).set_focus(false) };
        }
        if let Some(new) = target
            && let Some(&widget) = self.node_map.get(&new)
        {
            unsafe { (*widget).set_focus(true) };
            if reveal {
                self.host
                    .reveal_descendant(std::ptr::from_ref(unsafe { &*widget }));
            }
        }
        *focus = target;
        self.native_focus_ordinal = target.and_then(|id| {
            self.focusable_order()
                .iter()
                .position(|candidate| *candidate == id)
        });
    }

    fn set_native_focus(&mut self, focus: &mut Option<NodeId>, target: Option<NodeId>) {
        self.set_native_focus_with_reveal(focus, target, true);
    }

    /// Request a real frame from each embedded component's existing wake scope.
    #[cfg(test)]
    pub(crate) fn wake_mounted_components(&mut self) -> usize {
        let mut count = 0_usize;
        for pointer in self.node_map.values_mut() {
            // SAFETY: node_map pointers are owned by the live widget tree and
            // remain valid for the exclusive borrow of this frame.
            let widget = unsafe { &mut **pointer };
            if let Some(app) = widget
                .as_any_mut()
                .and_then(|widget| widget.downcast_mut::<ui::app::EmbeddedApp>())
            {
                let _entry = app.app().enter_wake_scope();
                ui::wake::WakeHandle::new().wake();
                count = count.saturating_add(1);
            }
        }
        count
    }

    /// The first widget in the live layout `find` recognises: an app a tab
    /// mounted, for a test that drives it.
    #[cfg(test)]
    pub(crate) fn mounted_mut<T: 'static>(
        &mut self,
        find: impl Fn(&mut dyn Widget) -> Option<&mut T>,
    ) -> Option<&mut T> {
        self.node_map.values_mut().find_map(|widget| {
            // SAFETY: node_map pointers are owned by the live widget tree and
            // remain valid for the borrow of `self`.
            let widget = unsafe { &mut **widget };
            // An app's own widgets are inside the App its tile shows.
            let embedded = widget
                .as_any()
                .is_some_and(<dyn std::any::Any>::is::<ui::app::EmbeddedApp>);
            if embedded {
                widget
                    .as_any_mut()?
                    .downcast_mut::<ui::app::EmbeddedApp>()?
                    .component_root_mut()
                    .and_then(&find)
            } else {
                find(widget)
            }
        })
    }

    #[cfg(test)]
    fn widget_name(&self, id: NodeId) -> Option<&'static str> {
        self.node_map.get(&id).map(|&widget| {
            // SAFETY: this map belongs to the current, live layout tree.
            unsafe { (*widget).name() }
        })
    }

    /// Match `ui::App` for an app hosted in this process: Ctrl+T, Ctrl+W,
    /// Ctrl+Shift+T, Ctrl+Tab and Ctrl+Shift+Tab that the app left unhandled
    /// are the desktop's tab chords — another instance beside it, close its
    /// tab, reopen the last closed one, the next or previous tab of the
    /// group. Never while chrome (a palette, a menu, a dialog) holds the
    /// keyboard. Stepping through tabs and reopening one need no hosted app:
    /// any tile the shell draws itself leaves those chords to the desktop.
    fn apply_hosted_tab_chord(&mut self, event: &WindowEvent, modifiers: Modifiers) -> bool {
        use crate::shell::state::TileContent;
        use ui::shell_tabs::TabRequest;
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != winit::event::ElementState::Pressed || key.repeat {
            return false;
        }
        let mods = modifiers.state();
        let Some(request) = ui::shell_tabs::chord_request(
            &key.logical_key,
            mods.control_key(),
            mods.shift_key(),
            mods.alt_key() || mods.super_key(),
        ) else {
            return false;
        };
        if self.host.modal_capture().is_some() {
            return false;
        }
        match request {
            TabRequest::CycleTab { forward } => {
                let _ = self.host.cycle_focused_tab(if forward { 1 } else { -1 });
                return true;
            }
            TabRequest::ReopenClosed => {
                self.with_chrome(|st| {
                    st.reopen_closed_tab();
                });
                return true;
            }
            TabRequest::OpenBeside { .. }
            | TabRequest::CloseThis
            | TabRequest::PlaceNextWindowBeside { .. } => {}
        }
        let hosted = {
            let chrome = self.chrome();
            let tile = crate::shell::window_actions::active_tab_id(&chrome).and_then(|tab| {
                let tree = chrome.tree();
                tree.desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find(|candidate| candidate.id == tab)
                    .map(|candidate| matches!(candidate.tile, TileContent::AppPlaceholder { .. }))
            });
            tile.unwrap_or(false)
        };
        if !hosted {
            return false;
        }
        self.with_chrome(|st| match request {
            TabRequest::OpenBeside { .. } => {
                if let Err(error) = crate::shell::window_actions::duplicate_active_tab(st) {
                    tracing::debug!(?error, "no new tab for the hosted app");
                }
            }
            TabRequest::CloseThis => {
                if let Err(error) = crate::shell::window_actions::close_active_tab(st) {
                    tracing::debug!(?error, "hosted tab not closed");
                }
            }
            TabRequest::ReopenClosed
            | TabRequest::CycleTab { .. }
            | TabRequest::PlaceNextWindowBeside { .. } => {}
        });
        true
    }

    fn active_tile_is_terminal(&self) -> bool {
        let chrome = self.chrome();
        let tree = chrome.tree();
        let is_terminal = Self::workspace_active_tile_is_terminal(&tree);
        drop(tree);
        is_terminal
    }

    fn workspace_active_tile_is_terminal(tree: &crate::shell::state::WorkspaceTree) -> bool {
        use crate::shell::state::{AppKind, TileContent};
        let Some(ws) = tree.desktop.active_workspace() else {
            return false;
        };
        let strip = tree
            .desktop
            .active_strip
            .get(&ws.id)
            .and_then(|sid| ws.strips.iter().find(|s| s.id == *sid))
            .or_else(|| ws.strips.first());
        let Some(strip) = strip else {
            return false;
        };
        let group = tree
            .desktop
            .active_group_for(strip.id)
            .and_then(|gid| strip.window_groups.iter().find(|g| g.id == gid))
            .or_else(|| strip.window_groups.first());
        let Some(group) = group else {
            return false;
        };
        let tab = group
            .tabs
            .iter()
            .find(|t| t.id == group.active_tab)
            .or_else(|| group.tabs.first());
        tab.is_some_and(|tab| match &tab.tile {
            TileContent::AppPlaceholder {
                kind: AppKind::Terminal,
                ..
            }
            | TileContent::Stub {
                kind: AppKind::Terminal,
                ..
            } => true,
            TileContent::Foreign { app_id, .. } => app_id == "so.tilo.terminal",
            _ => false,
        })
    }

    /// The native editor actually owned by the active mounted tab.
    fn active_terminal_node(&self) -> Option<NodeId> {
        fn terminal_pointer(widget: &dyn Widget) -> Option<*const ()> {
            if is_terminal(widget) && widget.is_focusable() && widget.is_enabled() {
                return Some(std::ptr::from_ref(widget).cast::<()>());
            }
            widget.children().into_iter().find_map(terminal_pointer)
        }
        let pointer = terminal_pointer(self.host.active_app_host()?)?;
        // AppWidget and Widget can have different vtables for one owned object.
        self.node_map
            .iter()
            .find_map(|(&id, &widget)| (widget.cast::<()>().cast_const() == pointer).then_some(id))
    }

    fn has_focus_trap(&self) -> bool {
        fn contains_trap(widget: &dyn Widget) -> bool {
            widget.traps_focus() || widget.children().into_iter().any(contains_trap)
        }
        contains_trap(self.host.as_ref())
    }

    fn initial_focus_target(&self, order: &[NodeId]) -> Option<NodeId> {
        if let Some(palette) = self.palette_focus_target() {
            return Some(palette);
        }
        // A focused Terminal must receive typing, including Space and Enter.
        // Chrome controls come first in DFS and used to steal initial focus;
        // Activate then opened workspace settings instead of sending CR.
        if self.active_tile_is_terminal() {
            return self.active_terminal_node();
        }
        // Overflowing scroll regions are keyboard targets too, but an
        // unnamed container must not take the initial focus from the setup
        // form's first control. Keep it in the Tab order for keyboard scrolling.
        order
            .iter()
            .copied()
            .find(|id| {
                self.node_map.get(id).is_some_and(|&widget| {
                    // SAFETY: this map belongs to the current, live layout tree.
                    unsafe { (*widget).accessibility() }.is_none_or(|node| {
                        node.role != ui::a11y::Role::Group || node.name.is_some()
                    })
                })
            })
            .or_else(|| order.first().copied())
    }

    fn focus_first(&mut self, focus: &mut Option<NodeId>) {
        // After a rebuild the old id belongs to the discarded Taffy tree and
        // must not be dereferenced against the new pointer map.
        if self.focus_tree_changed {
            *focus = None;
        }
        let first = self.initial_focus_target(&self.focusable_order());
        // A freshly presented page belongs at its top. Programmatic initial
        // focus still paints a visible keyboard target and gives assistive
        // technology a deterministic cursor, but it must not scroll a wizard
        // past its heading before the person has navigated. Tab/Shift+Tab and
        // explicit accessibility focus continue to use set_native_focus and
        // therefore reveal their destination.
        self.set_native_focus_with_reveal(focus, first, false);
        self.focus_tree_changed = false;
    }

    fn restore_focus_after_tree_change(&mut self, focus: &mut Option<NodeId>) {
        *focus = None;
        let order = self.focusable_order();
        // Opening a Terminal rebuilds the tree. Restoring the previous
        // chrome ordinal leaves Space/Enter on the workspace pill.
        let target = if let Some(palette) = self.palette_focus_target() {
            Some(palette)
        } else if self.active_tile_is_terminal() {
            self.initial_focus_target(&order)
        } else {
            self.native_focus_ordinal.map_or_else(
                || self.initial_focus_target(&order),
                |ordinal| {
                    order
                        .get(ordinal)
                        .copied()
                        .or_else(|| order.last().copied())
                },
            )
        };
        self.set_native_focus(focus, target);
        self.focus_tree_changed = false;
    }

    /// Keep the native compositor's external cursor attached to the live
    /// widget tree. Called after layout as well as before input so initial
    /// focus and wizard-step focus are visible without a sacrificial key.
    pub(crate) fn reconcile_native_focus(&mut self, focus: &mut Option<NodeId>) {
        if self.restore_focus_after_palette(focus) {
            return;
        }
        let reset = self.host.take_focus_reset();
        if reset {
            self.relayout();
        }
        if reset {
            self.focus_first(focus);
        } else if let Some(palette) = self.palette_focus_target() {
            self.set_native_focus(focus, Some(palette));
            self.focus_tree_changed = false;
        } else if self.focus_tree_changed {
            self.restore_focus_after_tree_change(focus);
        } else if focus.is_none() {
            self.focus_first(focus);
        }
    }

    fn palette_focus_target(&self) -> Option<NodeId> {
        // The anchored composer is a shared popover, and its editor owns
        // typing instead of the mode picker that precedes it in DFS.
        if self.host.person_quick_composer_open() && !self.host.modal_sheet_owns_keyboard() {
            return self.node_map.iter().find_map(|(&node, &widget)| {
                // SAFETY: this map belongs to the current live tree.
                (unsafe { (*widget).test_id() } == Some("person-quick-editor")).then_some(node)
            });
        }
        if self.host.agent_quick_composer_open() && !self.host.modal_sheet_owns_keyboard() {
            return self.node_map.iter().find_map(|(&node, &widget)| {
                // SAFETY: this map belongs to the current live tree.
                (unsafe { (*widget).test_id() } == Some("agent-quick-editor")).then_some(node)
            });
        }
        if !self.host.palette_visible() || self.has_focus_trap() {
            return None;
        }
        self.node_map.iter().find_map(|(&node, &widget)| {
            // SAFETY: the map belongs to this frame's current live tree.
            (unsafe { (*widget).test_id() } == Some("command_palette_input")).then_some(node)
        })
    }

    fn settle_palette_focus(&mut self, palette: NodeId) {
        if !self.palette_focus_active {
            let previous = self
                .native_focus_ordinal
                .and_then(|ordinal| self.focusable_order().get(ordinal).copied())
                .filter(|&node| {
                    node != palette
                        && self.node_map.get(&node).is_some_and(|&widget| {
                            // SAFETY: the map belongs to the current live tree.
                            unsafe { (*widget).is_focused() }
                        })
                });
            self.palette_focus_return = previous.and_then(|node| {
                self.native_focus_path(node)
                    .map(|path| (self.chrome().active_tab_id(), path))
            });
            self.palette_focus_active = true;
        }
        let needs_focus = self.node_map.iter().any(|(&node, &widget)| {
            // SAFETY: all pointers belong to this exclusively owned live tree.
            (unsafe { (*widget).is_focused() }) != (node == palette)
        });
        if needs_focus {
            let mut focus = None;
            self.set_native_focus(&mut focus, Some(palette));
        }
    }

    fn native_focus_path(&self, node: NodeId) -> Option<Vec<String>> {
        fn walk(
            widget: &dyn Widget,
            target: *const (),
            mut path: Vec<String>,
        ) -> Option<Vec<String>> {
            let key = widget
                .test_id()
                .filter(|id| !id.is_empty())
                .map(|id| format!("named:{id}"))
                .or_else(|| {
                    widget
                        .accessibility()
                        .and_then(|node| node.semantic_key)
                        .map(|key| format!("semantic:{key}"))
                });
            let named = key.is_some();
            if let Some(key) = key {
                path.push(key);
            }
            if std::ptr::eq(std::ptr::from_ref(widget).cast::<()>(), target) {
                return named.then_some(path);
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| walk(child, target, path.clone()))
        }
        let target = self.node_map.get(&node)?.cast::<()>();
        walk(self.host.as_ref(), target, Vec::new())
    }

    fn restore_focus_after_palette(&mut self, focus: &mut Option<NodeId>) -> bool {
        if !self.palette_focus_active
            || self.host.palette_visible()
            || self.host.agent_quick_composer_open()
            || self.host.person_quick_composer_open()
        {
            return false;
        }
        self.palette_focus_active = false;
        let Some((tab, path)) = self.palette_focus_return.take() else {
            return false;
        };
        if self.has_focus_trap()
            || self.host.modal_sheet_owns_keyboard()
            || tab != self.chrome().active_tab_id()
        {
            return false;
        }
        // Closing search may have changed topology. Retire/rebuild through
        // the production path before resolving this identity in the live map.
        self.relayout();
        let mut matches = self
            .focusable_order()
            .into_iter()
            .filter(|&node| self.native_focus_path(node).as_deref() == Some(path.as_slice()));
        let target = matches.next();
        if matches.next().is_some() {
            return false;
        }
        self.set_native_focus(focus, target);
        self.focus_tree_changed = false;
        true
    }

    fn focus_tab(&mut self, focus: &mut Option<NodeId>, backwards: bool) -> bool {
        let order = self.focusable_order();
        if order.is_empty() {
            return false;
        }
        let current = (*focus).and_then(|id| order.iter().position(|candidate| *candidate == id));
        let next = match current {
            Some(0) if backwards => order.len().saturating_sub(1),
            Some(index) if backwards => index.saturating_sub(1),
            Some(index) => {
                let candidate = index.saturating_add(1);
                if candidate < order.len() {
                    candidate
                } else {
                    0
                }
            }
            None => 0,
        };
        self.set_native_focus(focus, order.get(next).copied());
        true
    }

    /// Run `compute_layout` + `apply_layout` against the current tree. Animation
    /// state is preserved since the taffy tree isn't rebuilt — only layout
    /// re-runs, unless `take_structure_dirty()` says otherwise, in which
    /// case the taffy tree rebuilds first.
    pub fn relayout(&mut self) {
        #[cfg(test)]
        {
            self.layout_passes = self
                .layout_passes
                .checked_add(1)
                .expect("test layout pass counter exhausted");
        }
        // Apply the compositor's live fullscreen state to the strip rows
        // first, flagging the owning row dirty so the structure-dirty check
        // below rebuilds and the collapse (or restore) takes this frame.
        self.host.sync_fullscreen_from_chrome();
        if self.host.take_structure_dirty() {
            self.rebuild_layout_tree();
        }
        let (w, h) = self.viewport;
        // `None` only in the narrow window where `rebuild_layout_tree` just
        // failed to build a tree (see its doc) — nothing to lay out until
        // the next successful rebuild.
        let (Some(outer), Some(child)) = (self.root_node, self.host_node) else {
            return;
        };
        // Borrowed rather than cloned — the closure only needs read access
        // and the borrow is disjoint from `self.taffy`/`self.text_renderer`.
        // Cloning the 50-100 entry widget-ptr map every relayout was a
        // visible cost on continuous resize.
        let measure_map: &HashMap<NodeId, *mut dyn Widget> = &self.node_map;
        let tr = &mut self.text_renderer;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel viewport size; f32 precision is sufficient"
        )]
        let available_space = Size {
            width: AvailableSpace::Definite(w as f32),
            height: AvailableSpace::Definite(h as f32),
        };
        let mut admitted = ui::bound_dispatch::BoundNodeMap::new();
        // Empty bound text starts at Display::None. Intrinsic remeasurement
        // alone cannot restore its flow when it gains content; the shared
        // solver settles allocation-dependent styles as well as measurements.
        // SAFETY: this frame exclusively owns the exact stable host, Taffy
        // topology and matching pointers for admission and the whole solve.
        let started = std::time::Instant::now();
        let layout_result = unsafe {
            admitted.rebuild(self.host.as_ref(), &self.taffy, child, measure_map);
            crate::compositor::metrics::profile::record_phase(
                "layout/admit",
                started.elapsed(),
                None,
            );
            ui::widget::compute_widget_layout(
                &mut self.taffy,
                outer,
                available_space,
                measure_map,
                &admitted,
                tr,
            )
        };
        if let Err(err) = layout_result {
            // Skip this frame's apply_layout rather than aborting the
            // session, as every host does with a layout that fails.
            if let Some(line) = self.skipped_frames.skip(&err, ui::time::now()) {
                tracing::error!("frame-builder: {line}");
            }
            return;
        }
        let solved = std::time::Instant::now();
        crate::compositor::metrics::profile::record_phase(
            "layout/solve",
            solved.duration_since(started),
            None,
        );
        self.host.apply_layout(&self.taffy, child, Point::ORIGIN);
        crate::compositor::metrics::profile::record_phase("layout/apply", solved.elapsed(), None);
        // Semantic search can open during the pump, before input dispatch
        // attaches the external cursor. Settle its owner before painting.
        if let Some(palette) = self.palette_focus_target() {
            self.settle_palette_focus(palette);
        }
        self.layout_osk_overlay();
    }

    /// Dock the keyboard at the bottom of the output it serves: the one
    /// holding the field it types into, else the display the person is
    /// working on. The release gate taps its keys from the same placement
    /// (`keyboard_rect` in `tools/atspi_external_client.py`).
    fn layout_osk_overlay(&mut self) {
        if self.osk_overlay.is_none() {
            return;
        }
        let outputs = self.effective_outputs();
        let focused = self.host.overlay_area().center();
        let output = self
            .osk_anchor
            .and_then(|anchor| outputs.iter().find(|o| o.rect().contains(anchor)))
            .or_else(|| outputs.iter().find(|o| o.rect().contains(focused)))
            .or_else(|| outputs.first());
        let Some(output) = output else { return };

        let margin = 20.0_f64.min(output.width / 20.0);
        let width = (output.width - margin * 2.0).clamp(1.0, 960.0);
        let height = (output.height * 0.40)
            .clamp(220.0, 380.0)
            .min((output.height - margin * 2.0).max(1.0));
        let x0 = output.x + (output.width - width) / 2.0;
        // From the output's own top: a display beside a taller one, or
        // below another, does not start at the desktop's top edge.
        let y0 = output.y + (output.height - height - margin).max(0.0);
        if let Some(keyboard) = self.osk_overlay.as_mut() {
            keyboard.place_floating(Rect::new(x0, y0, x0 + width, y0 + height));
        }
    }

    /// Install adversarial app content while retaining the real host, pump,
    /// layout and paint passes for native scheduling regression tests.
    #[cfg(test)]
    pub(crate) fn with_fixture_content(content: Box<dyn Widget>) -> Self {
        let mut frame = Self::new((800, 600));
        frame.node_map.clear();
        *frame.host = ShellOverlayHost::with_fixture_content(content);
        frame.rebuild_layout_tree();
        frame
    }

    /// Run the widget tree's per-frame pump (`sync` then `update`).
    /// Returns `true` when something changed and the frame should repaint.
    /// The compositor's frame loop needs this too, or a hosted app that
    /// depends on it (e.g. the terminal draining its PTY) never advances.
    pub fn pump_apps(&mut self, doc: &mut AutoCommit, root: &ObjId) -> bool {
        let started = std::time::Instant::now();
        self.replicate_room_records();
        let records_done = std::time::Instant::now();
        self.pump_room_contents();
        let rooms_done = std::time::Instant::now();
        let before_settle = (doc.base_heads(), doc.pending_ops());
        self.host.settle(&mut ui::widget::Edits::lend(doc), root);
        let mut settled = before_settle != (doc.base_heads(), doc.pending_ops());
        self.host.sync(doc, root);
        let before_settle = (doc.base_heads(), doc.pending_ops());
        self.host.settle(&mut ui::widget::Edits::lend(doc), root);
        if before_settle != (doc.base_heads(), doc.pending_ops()) {
            settled = true;
            self.host.sync(doc, root);
        }
        // First-login setup gives the session its identity mid-session; that
        // principal is "me" too, or the owner sees presence over their windows.
        {
            let chrome = self.chrome.borrow();
            if !self.presence.is_own_identity(&chrome.local_user_id) {
                self.presence.add_own_identity(&chrome.local_user_id);
            }
        }
        let sync_done = std::time::Instant::now();
        let changed = self.host.update(doc, root);
        let host_update_done = std::time::Instant::now();
        let before_settle = (doc.base_heads(), doc.pending_ops());
        self.host.settle(&mut ui::widget::Edits::lend(doc), root);
        if before_settle != (doc.base_heads(), doc.pending_ops()) {
            settled = true;
            self.host.sync(doc, root);
        }
        // `update` may have dropped widgets the pointer map still names: a
        // workspace's content is rebuilt when its first window opens or its
        // last one closes (the top bar and its toolbar go with it), and
        // groups are spliced out. Rebuild the map now, not at the next
        // paint: a backed-up renderer defers that paint for frames, and the
        // loop reads the focused node through the map on every iteration
        // (`focused_ime_requires_deferred_ack`), which then called through a
        // freed widget and ended the session.
        let settle_done = std::time::Instant::now();
        if self.host.take_structure_dirty() {
            self.rebuild_layout_tree();
            self.relayout();
        }
        let update_done = std::time::Instant::now();
        let total = update_done.duration_since(started);
        for (phase, took) in [
            ("widget-pump/records", records_done.duration_since(started)),
            ("widget-pump/rooms", rooms_done.duration_since(records_done)),
            ("widget-pump/sync", sync_done.duration_since(rooms_done)),
            ("widget-pump/update", update_done.duration_since(sync_done)),
            (
                "widget-pump/update/host",
                host_update_done.duration_since(sync_done),
            ),
            (
                "widget-pump/update/settle",
                settle_done.duration_since(host_update_done),
            ),
            (
                "widget-pump/update/structure",
                update_done.duration_since(settle_done),
            ),
        ] {
            crate::compositor::metrics::profile::record_phase(phase, took, None);
        }
        if total >= std::time::Duration::from_millis(50) {
            tracing::warn!(
                target: "tilo::latency",
                elapsed_ms = total.as_secs_f64() * 1_000.0,
                records_ms = records_done.duration_since(started).as_secs_f64() * 1_000.0,
                rooms_ms = rooms_done.duration_since(records_done).as_secs_f64() * 1_000.0,
                sync_ms = sync_done.duration_since(rooms_done).as_secs_f64() * 1_000.0,
                update_ms = update_done.duration_since(sync_done).as_secs_f64() * 1_000.0,
                "widget app pump stalled compositor input"
            );
            // The session log carries stderr lines; the tracing target above
            // is filtered out of it by default.
            eprintln!(
                "SLOW-APP-PUMP total={total:?} (records={:?} rooms={:?} sync={:?} update={:?})",
                records_done.duration_since(started),
                rooms_done.duration_since(records_done),
                sync_done.duration_since(rooms_done),
                update_done.duration_since(sync_done),
            );
        }
        changed || settled
    }

    /// Turn "a peer's changes landed in the tree" into "the widget tree is
    /// rebuilt". `resync_strips_from_tree` splices groups/tabs, gated on the
    /// chrome revision; peer-sync writes the tree from the network thread,
    /// which can't touch the chrome, so this bumps the revision whenever the
    /// room epoch (or a pending peer-window decision) has moved. Cheap on a
    /// quiet frame — three counters read under one acquisition of the tree's
    /// read lock, plus the queued-card and hydration checks; no tab walk.
    fn pump_room_contents(&mut self) -> bool {
        // Peer windows are examined on rounds after an arrival too, not just
        // the arrival itself: `peer_windows::examine` starts at most one
        // launch per app per pass, so several windows at once need more than
        // one look. The tabs are walked again only once the tree may have
        // changed; while a window is owed a look, every round walks.
        let (revisions, pending) = {
            let chrome = self.chrome.borrow();
            let tree = chrome.tree();
            let revisions = TreeRevisions::read(&chrome, &tree);
            let pending = self.peer_windows_quiet_at != Some(revisions)
                && crate::shell::peer_windows::any_pending(&tree);
            drop(tree);
            (revisions, pending)
        };
        self.peer_windows_quiet_at = (!pending).then_some(revisions);
        let epoch = revisions.room_epoch;
        let owed = pending
            || crate::shell::peer_windows::retry_requested()
            || crate::shell::peer_windows::install_requested()
            || crate::shell::record_hydration::any_waiting();
        let decided = if owed {
            let chrome = self.chrome.borrow();
            let mut tree = chrome.tree_mut();
            crate::shell::peer_windows::examine(&mut tree)
        } else {
            false
        };
        // Windows being carried to another workspace advance each round;
        // [`Self::next_wake_deadline`] brings a round back while one waits
        // on something no input announces.
        if self.chrome.borrow().window_transfers.is_pending() {
            crate::shell::window_transfer::tick(&mut self.chrome.borrow_mut());
        }
        if epoch == self.last_room_epoch && !decided {
            return false;
        }
        self.last_room_epoch = epoch;
        self.chrome.borrow_mut().bump();
        true
    }

    /// Hand the rooms' record indexes to the peer-replication path, and fold
    /// in whatever peers brought into the rooms this session is in.
    ///
    /// Here rather than in the frame-tick closure because this is the only
    /// per-frame place holding **both** sides: the index lives behind an
    /// `Rc<RefCell<_>>`, the tree behind an `Arc<RwLock<_>>`, and
    /// `ui::App::set_frame_tick` requires `Send + Sync`, which an `Rc` can't enter.
    ///
    /// Cheap on a quiet frame: `RoomRecords::replication_due` compares a
    /// generation counter before reading any document, the tree is written
    /// only when it is due, and the chrome only bumps when something
    /// actually arrived.
    fn replicate_room_records(&self) {
        let index = self.host.room_records();

        // Anything an exported action asked to bring here, first — so a
        // reference made this frame publishes in the same pass.
        consume_record_requests(&self.chrome, &index);

        let arrived = {
            let chrome = self.chrome.borrow();
            let mut tree = chrome.tree_mut();
            let mut index = index.borrow_mut();
            // Asked through the guard first: a pump with nothing to exchange
            // lends the tree no mutable access, so it announces no write.
            index.replication_due(&tree) && index.replicate_with(&mut tree)
        };
        if arrived {
            self.chrome.borrow_mut().bump();
        }
    }

    /// Render the current frame into a fresh Vello scene, in scene pixels:
    /// every display draws its part of the desktop at its own scale. Caller
    /// hands it to a renderer (offscreen/live/compositor backend).
    pub fn paint(&mut self) -> Scene {
        // Dense captures must not inherit a surrounding presenter's canvas draw.
        let _dense_scope = ui::retained_present::RetainedScene::begin(false);
        let recording = self
            .hdr_layers_composited()
            .then(crate::shell::hdr_surface::HdrLayerRecording::begin);
        let backdrop = BackdropRecording::begin();
        let (logical, sections) = self.paint_logical();
        let surfaces = backdrop.finish();
        let layers = recording.map(crate::shell::hdr_surface::HdrLayerRecording::finish);
        self.painted_hdr_layers = self.hdr_layers_to_scene(layers.unwrap_or_default());
        // A surface drawn into a section sits over the sections below it;
        // one drawn over them all, into the scene itself (layer 0), over
        // what that scene already holds.
        self.painted_backdrop =
            self.backdrop_to_scene(surfaces, |layer| sections.below_layer(layer));
        self.to_scene(logical)
    }

    /// The HDR layers the latest [`Self::paint`] drew, in scene pixels.
    pub fn take_hdr_layers(&mut self) -> Vec<crate::shell::hdr_surface::HdrLayer> {
        std::mem::take(&mut self.painted_hdr_layers)
    }

    /// The backdrops the latest [`Self::paint`] drew its floating surfaces
    /// over, in scene pixels. Hand them to the renderer of that scene.
    pub fn take_backdrop(&mut self) -> Option<Arc<BackdropFrame>> {
        self.painted_backdrop.take()
    }

    /// Recorded floating surfaces in scene pixels, each with the complete
    /// scene below it: the layers `below_layer` names under its own layer,
    /// then what its layer drew before it. A surface whose layer has no
    /// known place (`None`) keeps its opaque placeholder.
    fn backdrop_to_scene<'a>(
        &self,
        surfaces: Vec<RecordedBackdrop>,
        below_layer: impl Fn(u32) -> Option<Vec<&'a Scene>>,
    ) -> Option<Arc<BackdropFrame>> {
        let regions: Vec<BackdropRegion> = surfaces
            .into_iter()
            .filter_map(|surface| {
                let layers = below_layer(surface.layer)?;
                let (rect, scale) = self.surface_to_scene(surface.bounds)?;
                let below = if layers.is_empty() {
                    surface.below
                } else {
                    let mut below = Scene::new();
                    for layer in layers {
                        below.append(layer, None);
                    }
                    below.append(&surface.below, None);
                    below
                };
                Some(BackdropRegion {
                    image: surface.image,
                    rect,
                    blur_px: surface.blur_px * scale,
                    below: self.below_to_scene(below),
                    ground: surface.ground,
                })
            })
            .collect();
        (!regions.is_empty()).then(|| Arc::new(BackdropFrame { regions }))
    }

    /// A floating surface's logical bounds in scene pixels, through the
    /// display it is on, and that display's scale. Every display draws the
    /// surface, but it sits on one; that is the one its backdrop is blurred
    /// for.
    fn surface_to_scene(&self, bounds: Rect) -> Option<(Rect, f64)> {
        match &self.outputs {
            Some(outputs) if !output_scene::is_identity(outputs) => {
                let output = output_scene::output_at(outputs, bounds.center())?;
                let transform = output.to_scene();
                Some((
                    transform.transform_rect_bbox(bounds),
                    transform.determinant().abs().sqrt(),
                ))
            }
            _ => Some((bounds, 1.0)),
        }
    }

    /// Whether HDR surfaces may be composited outside the scene. A
    /// whole-desktop colour filter or magnifier transforms everything vello
    /// draws, so while one is on HDR surfaces show their SDR rendition with
    /// it.
    fn hdr_layers_composited(&self) -> bool {
        !(self.watch_configured_theme
            && (self.night_light
                || !matches!(self.visual_accessibility.color_filter, ColorFilter::None)
                || self.visual_accessibility.magnifier.is_on()))
    }

    /// Layers recorded in logical coordinates, as [`Self::to_scene`] places
    /// the scene they were drawn in: once per display, magnified and clipped.
    fn hdr_layers_to_scene(
        &self,
        layers: Vec<crate::shell::hdr_surface::HdrLayer>,
    ) -> Vec<crate::shell::hdr_surface::HdrLayer> {
        match &self.outputs {
            Some(outputs) if !output_scene::is_identity(outputs) => outputs
                .iter()
                .flat_map(|output| {
                    let transform = output.to_scene();
                    layers.iter().filter_map(move |layer| {
                        layer
                            .transformed(Affine::IDENTITY, output.rect())
                            .and_then(|clipped| clipped.transformed(transform, output.scene))
                    })
                })
                .collect(),
            _ => layers,
        }
    }

    /// The painted desktop in scene pixels. Passes the logical scene through
    /// untouched when no display magnifies it.
    fn to_scene(&self, logical: Scene) -> Scene {
        match &self.outputs {
            Some(outputs) if !output_scene::is_identity(outputs) => {
                let mut scene = Scene::new();
                output_scene::compose(outputs, &logical, &mut scene);
                scene
            }
            _ => logical,
        }
    }

    /// What a floating surface is drawn over, in scene pixels: the scene
    /// cut where the surface begins, so it may stop inside the layers that
    /// enclose the surface (a fade, a panel's clip). Each display's pass
    /// closes them, as the end of a scene does, without reporting a leak.
    fn below_to_scene(&self, logical: Scene) -> Scene {
        match &self.outputs {
            Some(outputs) if !output_scene::is_identity(outputs) => {
                let mut scene = Scene::new();
                let _enclosing = output_scene::compose_closing_leaks(outputs, &logical, &mut scene);
                scene
            }
            _ => logical,
        }
    }

    /// [`Self::paint`] before the displays magnify it: the desktop in
    /// logical coordinates, the space the widget tree is laid out in.
    fn paint_logical(&mut self) -> (Scene, Box<crate::shell::overlay_host::ShellSceneSections>) {
        let motions_at = ui::motion::frame_now();
        self.refresh_configured_theme();
        self.refresh_workspace_temperature();
        self.refresh_magnifier();
        let mut scene = Scene::new();
        ui::backdrop::layer_scene(&scene, 0);
        let sections = self.host.draw_scene_sections(
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
        );
        self.frame_drawn(motions_at);
        sections.compose_into(&mut scene);
        for proxy in &self.floating_transfer_proxies {
            proxy.draw(&mut scene);
        }
        for (_, surface) in &mut self.floating_overlays {
            if surface.is_mapped() {
                surface.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut self.text_renderer,
                    &self.theme,
                    &self.presence,
                );
            }
        }
        if let Some((_, surface)) = &mut self.fullscreen_overlay
            && surface.is_mapped()
        {
            surface.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut self.text_renderer,
                &self.theme,
                &self.presence,
            );
        }
        self.draw_system_modal_scrim(&mut scene);
        for (_, surface) in &mut self.transient_overlays {
            if surface.is_mapped() {
                surface.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut self.text_renderer,
                    &self.theme,
                    &self.presence,
                );
            }
        }
        if self.osk_active
            && let Some(keyboard) = self.osk_overlay.as_mut()
            && keyboard.is_mapped()
        {
            keyboard.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut self.text_renderer,
                &self.theme,
                &self.presence,
            );
        }
        if self.watch_configured_theme && self.night_light {
            let (w, h) = self.viewport;
            let bounds = Rect::new(0.0, 0.0, w, h);
            // Multiply is a real whole-frame colour transform: unlike an
            // amber SrcOver veil it reduces short wavelengths without
            // washing blacks grey.  The native DRM path can later replace
            // this with a gamma LUT without changing the Settings contract.
            scene.push_layer(
                Fill::NonZero,
                BlendMode::new(Mix::Multiply, Compose::SrcOver),
                1.0,
                Affine::IDENTITY,
                &bounds,
            );
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                night_light_multiplier(self.night_light_temperature),
                None,
                &bounds,
            );
            scene.pop_layer();
        }
        if self.watch_configured_theme {
            self.apply_visual_accessibility(&mut scene);
        }
        (scene, sections)
    }

    /// Tell the next scene build whether nothing but motion (an animation
    /// moving laid-out content) happened since the last one: no input, no
    /// client or app content, no chrome change. Windows at rest then show
    /// the drawings they already have where those still hold
    /// ([`ui::retained_present::PanFrame::Moved`]).
    pub fn set_frame_only_moved(&mut self, only_moved: bool) {
        self.frame_only_moved = only_moved;
    }

    /// Produce the same ordered shell composition as [`paint`](Self::paint), together with
    /// retained-damage metadata for the DRM bridge. Both draw the host's sections
    /// ([`crate::shell::overlay_host::ShellOverlayHost::draw_scene_sections`]).
    #[allow(
        clippy::too_many_lines,
        reason = "Retained-damage metadata must be derived alongside the exact scene composition; splitting this transaction risks draw-order or damage-region divergence."
    )]
    pub fn paint_with_damage(&mut self) -> FramePaint {
        let motions_at = ui::motion::frame_now();
        // This producer owns the policy even when invoked during another draw.
        let _dense_scope = ui::retained_present::RetainedScene::begin(false);
        let mut phases = ScenePhases::start();
        let revision = self.chrome_revision();
        self.refresh_configured_theme();
        self.refresh_workspace_temperature();
        self.refresh_magnifier();
        // Theme, locale and workspace temperature can rebuild the whole layout
        // tree here; named apart so a slow scene says whether it was this.
        phases.mark("scene/refresh");
        let only_moved =
            std::mem::take(&mut self.frame_only_moved) && self.chrome_revision() == revision;
        self.scene_builds = self.scene_builds.wrapping_add(1);
        let _pan_frame = ui::retained_present::PanReplay::begin(
            if only_moved {
                ui::retained_present::PanFrame::Moved
            } else {
                ui::retained_present::PanFrame::Drawn
            },
            self.scene_builds,
        );
        let global_filter = self.global_filter();
        // A presenter that moves pixels within its target gets what a scroll
        // in an app moved, while the scene is one display's view of the
        // desktop and nothing follows the pointer over all of it.
        // Apps plan their copies in the display's pixels, so a step of half
        // a logical pixel at 200 % copies.
        let to_scene = self.lone_output_transform();
        let copying = self.scroll_copy_presenter && !self.pointer_filter() && to_scene.is_some();
        let device_per_unit = to_scene.map_or(1.0, |to_scene| {
            let [a, b, c, d, _, _] = to_scene.as_coeffs();
            if b == 0.0 && c == 0.0 && a.to_bits() == d.to_bits() {
                a
            } else {
                1.0
            }
        });
        let _copies_scope = ui::scroll_copy::HostCopies::begin_scaled(copying, device_per_unit);
        let layered = self.retained_layer_width.is_some();
        // A presenter that composites layers cannot composite HDR layers, and
        // they would be recorded in canvas coordinates.
        let recording = self
            .hdr_layers_composited()
            .then(crate::shell::hdr_surface::HdrLayerRecording::begin);
        // Layers drawn at rest are no frame: they leave the pans' motion as
        // the frames noted it.
        let panning = self.drawing_layers_at_rest
            || self.strip_motion.note(
                self.host
                    .strip_scroll_offsets()
                    .into_iter()
                    .map(|(id, scroll, _)| (id, scroll))
                    .collect(),
            );
        let plan = (layered
            && !global_filter
            && match self.layer_policy {
                LayerPolicy::Always => true,
                LayerPolicy::WhilePanning => panning && self.lone_output(),
            })
        .then(|| self.strip_plan())
        .flatten();
        let unbake = plan.as_ref().is_some_and(|plan| plan.unbake);
        if layered && self.layer_policy == LayerPolicy::Always && !unbake {
            self.note_layers_off(if global_filter {
                "a filter over the whole desktop"
            } else if plan.is_none() {
                "no strip plan (not one unrotated output)"
            } else {
                "a strip's pan is not shown as layers (too wide, or between pixels)"
            });
        }
        let retained_scope = ui::retained_present::RetainedScene::begin(unbake);
        let foreign_recording = super::foreign_damage::Recording::begin();
        let backdrop_recording = BackdropRecording::begin();
        let parts_recording = ui::scene_parts::Recording::begin();
        let mut sections = self.host.draw_scene_sections(
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
        );
        self.frame_drawn(motions_at);
        // Each kind of client surface floating over the desktop is one
        // damage part, there whether or not any is mapped: a popup opening
        // or a floating window repainting repaints its own rect.
        let desktop = Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1);
        let above = &mut sections.above_quick_settings;
        overlay_part(above, "shell.floating-transfer", desktop, |scene| {
            for proxy in &self.floating_transfer_proxies {
                proxy.draw(scene);
            }
            None
        });
        overlay_part(above, "shell.floating-windows", desktop, |scene| {
            self.floating_overlays
                .iter_mut()
                .filter(|(_, surface)| surface.is_mapped())
                .map(|(_, surface)| {
                    surface.draw_bounded(
                        scene,
                        Affine::IDENTITY,
                        &mut self.text_renderer,
                        &self.theme,
                        &self.presence,
                    )
                })
                .collect()
        });
        let mut fullscreen_on_top = None;
        overlay_part(above, "shell.fullscreen", desktop, |scene| {
            let (_, surface) = self.fullscreen_overlay.as_mut()?;
            if !surface.is_mapped() {
                return None;
            }
            let draws_before = scene.encoding().draw_tags.len();
            let bounds = surface.draw_bounded(
                scene,
                Affine::IDENTITY,
                &mut self.text_renderer,
                &self.theme,
                &self.presence,
            );
            fullscreen_on_top =
                crate::shell::scanout_candidate::DrawnOnTop::last_in(scene, draws_before);
            Some(vec![bounds?])
        });
        overlay_part(above, "shell.system-modal-scrim", desktop, |scene| {
            self.draw_system_modal_scrim(scene);
            self.system_modal_scrim.then(|| vec![desktop])
        });
        overlay_part(above, "shell.popups", desktop, |scene| {
            self.transient_overlays
                .iter_mut()
                .filter(|(_, surface)| surface.is_mapped())
                .map(|(_, surface)| {
                    surface.draw_bounded(
                        scene,
                        Affine::IDENTITY,
                        &mut self.text_renderer,
                        &self.theme,
                        &self.presence,
                    )
                })
                .collect()
        });
        overlay_part(above, "shell.osk", desktop, |scene| {
            let keyboard = self.osk_overlay.as_mut().filter(|_| self.osk_active)?;
            if !keyboard.is_mapped() {
                return None;
            }
            Some(vec![keyboard.draw_bounded(
                scene,
                Affine::IDENTITY,
                &mut self.text_renderer,
                &self.theme,
                &self.presence,
            )?])
        });
        drop(retained_scope);
        phases.mark("scene/draw-sections");
        crate::compositor::metrics::profile::record_phase(
            "scene/replayed-windows",
            std::time::Duration::ZERO,
            Some(ui::retained_present::take_replayed()),
        );

        let parts = OpaqueParts::of(&parts_recording.finish(), &sections);
        let backdrop_surfaces = backdrop_recording.finish();
        let foreign_images = foreign_recording.finish();
        let hdr_layers = self.hdr_layers_to_scene(
            recording
                .map(crate::shell::hdr_surface::HdrLayerRecording::finish)
                .unwrap_or_default(),
        );
        let panned = std::mem::take(&mut sections.panned);
        // A strip that reads what is below it (a blend mode) cannot be
        // composited on its own.
        let pieces = plan
            .as_ref()
            .filter(|_| unbake)
            .and_then(|plan| Self::panned_pieces(plan, panned))
            .filter(|pieces| pieces.strips.iter().all(separable_layer));
        if unbake
            && (!backdrop_surfaces.is_empty()
                || !hdr_layers.is_empty()
                || fullscreen_on_top.is_some()
                || pieces.is_none())
        {
            if self.layer_policy == LayerPolicy::Always {
                self.note_layers_off(if !backdrop_surfaces.is_empty() {
                    "a floating surface samples what is behind it"
                } else if !hdr_layers.is_empty() {
                    "an HDR surface"
                } else if fullscreen_on_top.is_some() {
                    "a fullscreen client on top"
                } else {
                    "the strips could not be drawn apart"
                });
            }
            // Material sampling needs the original screen-space sections, a
            // fullscreen client may be scanned out of the screen-space
            // scene, and strips the host could not draw apart as planned
            // cannot be shown panned. Discard this provisional retained draw
            // and produce those sections.
            let capacity = self.retained_layer_width.take();
            let frame = self.paint_with_damage();
            self.retained_layer_width = capacity;
            return frame;
        }
        let revision = self.reserve_scene_revision();
        let previous_revision = self
            .last_produced_sections
            .as_ref()
            .map(|last| last.revision);
        let pointer_filter = self.pointer_filter();
        let tint = self.tint();
        let strips = unbake.then(|| {
            plan.as_ref()
                .map(|plan| plan.strips.clone())
                .unwrap_or_default()
        });
        let strips_kept = self
            .last_produced_sections
            .as_ref()
            .is_some_and(|previous| previous.strips == strips);
        // Section and foreign-image bounds are in canvas coordinates while
        // a strip is unbaked. They cannot authorize sparse screen damage.
        let screen_space_damage = strips.as_ref().is_none_or(Vec::is_empty);
        let mut section_copies = Vec::new();
        let damage_regions =
            (revision != 0 && !pointer_filter && strips_kept && screen_space_damage)
                .then(|| {
                    self.damage_from_sections_with_images(
                        &sections,
                        &parts,
                        (pointer_filter, tint),
                        &foreign_images,
                        copying.then_some(&mut section_copies),
                    )
                })
                .flatten();
        // What a floating surface blurs, an HDR layer and a fullscreen
        // client over the desktop are not moved by a copy.
        let copies_shown =
            backdrop_surfaces.is_empty() && hdr_layers.is_empty() && fullscreen_on_top.is_none();
        let (damage_regions, scroll_copies) = match damage_regions {
            Some(regions) if !section_copies.is_empty() && copies_shown => {
                self.present_scroll_copies(&sections, &parts, &section_copies, regions)
            }
            Some(regions) => (Some(self.repaint_copies(&section_copies, regions)), None),
            None => (None, None),
        };
        let damage_regions = self
            .with_backdrop_damage(damage_regions, &backdrop_surfaces)
            .map(|regions| without_contained_regions(&regions));
        phases.mark("scene/damage");
        // A backdrop-dependent section is never independently composited.
        let separable = backdrop_surfaces.is_empty()
            && hdr_layers.is_empty()
            && [
                &sections.workspace,
                &sections.workspace_overlays,
                &sections.topbar,
                &sections.quick_settings,
                &sections.above_quick_settings,
            ]
            .into_iter()
            .all(separable_layer);
        // Layers say where their chrome changed since the drawing a
        // presenter most likely holds (the previous frame's layers, or the
        // last drawing at rest), so one still holding it rasterises only
        // those tiles of its chrome: a clock tick at rest, the strip rail
        // during a pan.
        let measured_from = if self.drawing_layers_at_rest {
            self.layers_drawn
                .as_ref()
                .map(|(since, previous)| (*since, previous))
        } else {
            self.last_produced_sections
                .as_ref()
                .and_then(|previous| previous.layers_generation.map(|since| (since, previous)))
        };
        let chrome_damage =
            measured_from
                .filter(|_| plan.is_some())
                .and_then(|(since, previous)| {
                    let rects =
                        self.chrome_damage(previous, &sections, &parts, (pointer_filter, tint))?;
                    Some(ChromeDamage { since, rects })
                });
        let generation = self.layers_generation.saturating_add(1);
        // Strips drawn apart are in no section: the frame is its layers.
        let layered = plan.zip(pieces).map(|(plan, pieces)| {
            let mut chrome = Scene::new();
            chrome.append(&sections.workspace_overlays, None);
            chrome.append(&sections.topbar, None);
            chrome.append(&sections.quick_settings, None);
            chrome.append(&sections.above_quick_settings, None);
            FrameCache {
                width: plan.width,
                strips: plan.strips,
                generation,
                chrome_damage,
                layers: FrameLayers {
                    backdrop: self.to_scene(sections.backdrop.clone()),
                    // Empty stays empty: magnified, it would gain the
                    // display's clip and read as drawn, and the presenter
                    // would then move no strip pixel.
                    workspace: if sections.workspace.encoding().is_empty() {
                        Scene::new()
                    } else {
                        self.to_scene(sections.workspace.clone())
                    },
                    strips: pieces.strips,
                    strip_parts: pieces.parts,
                    chrome: self.to_scene(chrome),
                },
            }
        });
        if layered.is_some() {
            self.layers_off = None;
        }
        phases.mark("scene/frame-cache");
        if layered.is_some() {
            self.layers_generation = generation;
            if !self.drawing_layers_at_rest {
                // The presenter's layers now come from this frame.
                self.layers_drawn = None;
            }
        }
        let backdrop =
            self.backdrop_to_scene(backdrop_surfaces, |layer| sections.below_layer(layer));
        phases.mark("scene/backdrop");
        // A layered frame's scene is the one a full render of it draws,
        // composed from the layers exactly as the presenter composites them.
        // No filter is on (filters keep the strips in screen space) and no
        // fullscreen client is up.
        let (scene, scanout, cache) = if let Some(cache) = layered {
            (
                crate::shell::render::strip_layers::compose(&cache),
                None,
                separable.then_some(cache),
            )
        } else {
            let (scene, scanout) = self.screen_space_scene(
                &sections,
                fullscreen_on_top,
                &foreign_images,
                global_filter,
            );
            (scene, scanout, None)
        };
        phases.mark("scene/compose");
        phases.report();

        let after_layers = self
            .last_produced_sections
            .as_ref()
            .is_some_and(|previous| {
                previous
                    .strips
                    .as_ref()
                    .is_some_and(|strips| !strips.is_empty())
            });
        self.last_produced_sections = Some(ProducedSections {
            foreign_images,
            backdrop: sections.backdrop,
            topbar: sections.topbar,
            workspace: sections.workspace,
            workspace_overlays: sections.workspace_overlays,
            strips,
            layers_generation: cache.as_ref().map(|cache| cache.generation),
            topbar_bounds: sections.topbar_bounds,
            quick_settings: sections.quick_settings,
            above_quick_settings: sections.above_quick_settings,
            quick_settings_bounds: sections.quick_settings_bounds,
            parts,
            global_filter: pointer_filter,
            tint,
            revision,
        });
        let mut frame = FramePaint {
            scene,
            revision,
            previous_revision,
            damage_regions,
            hdr_layers,
            backdrop,
            cache,
            scanout,
            scroll_copies,
        };
        self.keep_layers_in_step(&mut frame, after_layers);
        frame
    }

    /// Keep a presenter that composites strip pans from retained layers but
    /// gets frames at rest in screen space ([`LayerPolicy::WhilePanning`])
    /// in step with the frames. The first frame drawn in screen space after
    /// a pan (`after_layers`) also carries the desktop as layers: the
    /// presenter composites it from the layers it holds, which still show
    /// the desktop (nothing changed: nothing is rasterised), instead of
    /// rendering the whole output again. Any other frame that changes the
    /// desktop leaves the layers stale until they are drawn at rest
    /// ([`Self::layers_stale`], [`Self::layers_at_rest`]).
    fn keep_layers_in_step(&mut self, frame: &mut FramePaint, after_layers: bool) {
        if self.drawing_layers_at_rest || self.layer_policy != LayerPolicy::WhilePanning {
            return;
        }
        if frame.cache.is_none() && after_layers {
            frame.cache = self.draw_layers_at_rest(true);
        }
        if frame.cache.is_some() {
            self.layers_stale = false;
        } else if !frame.damage_regions.as_ref().is_some_and(Vec::is_empty) {
            // Only a desktop that can be drawn as layers has any to warm.
            self.layers_stale = self.lone_output();
        }
    }

    /// A frame changed the desktop since the layers a presenter composites
    /// strip pans from were last drawn for it: [`Self::layers_at_rest`]
    /// once the desktop rests brings them up to date.
    pub fn layers_stale(&self) -> bool {
        self.layers_stale
    }

    /// The desktop as layers, drawn now, for a presenter that composites
    /// strip pans from retained layers and gets frames at rest in screen
    /// space ([`LayerPolicy::WhilePanning`]). It warms its layers from them
    /// while the desktop rests ([`crate::shell::present::PresentPipe::warm`]),
    /// so the next pan's first frame only copies and composites. This is no
    /// frame: the damage relation between frames, their revisions and the
    /// pans' motion stay as the frames left them. `None` when the desktop is
    /// not drawn as layers: on several displays, under a filter over the
    /// whole desktop, with a strip panned between pixels or materials up.
    ///
    /// The desktop is drawn as layers twice. The first canvas draw after
    /// frames drawn in screen space fills the widgets' canvas caches, and a
    /// widget replayed from its cache encodes otherwise than one drawn
    /// afresh; the second draw is what the frames of a pan then draw, so
    /// the presenter's layers compare equal to the pan's first frame.
    pub fn layers_at_rest(&mut self) -> Option<FrameCache> {
        self.layers_stale = false;
        self.draw_layers_at_rest(false)?;
        self.draw_layers_at_rest(true)
    }

    /// One draw of [`Self::layers_at_rest`]. A drawing a presenter gets
    /// (`handed_over`) is what the next one's [`ChromeDamage`] is measured
    /// from.
    fn draw_layers_at_rest(&mut self, handed_over: bool) -> Option<FrameCache> {
        if self.drawing_layers_at_rest
            || self.layer_policy != LayerPolicy::WhilePanning
            || self.retained_layer_width.is_none()
            || !self.lone_output()
            || self.global_filter()
        {
            return None;
        }
        let sections = self.last_produced_sections.take();
        let revision = self.next_scene_revision;
        self.drawing_layers_at_rest = true;
        let drawn = self.paint_with_damage();
        self.drawing_layers_at_rest = false;
        self.next_scene_revision = revision;
        let drawn_sections = std::mem::replace(&mut self.last_produced_sections, sections);
        if handed_over
            && let (Some(cache), Some(drawn_sections)) = (drawn.cache.as_ref(), drawn_sections)
        {
            self.layers_drawn = Some((cache.generation, drawn_sections));
        }
        drawn.cache
    }

    /// `regions` with where each of `copies` lands, for a frame that makes
    /// none of them.
    fn repaint_copies(
        &self,
        copies: &[ui::scene_parts::SectionCopy],
        mut regions: Vec<FineDamageRect>,
    ) -> Vec<FineDamageRect> {
        for copy in copies {
            regions.extend(self.native_damage_rect(Some(copy.moved.destination())));
        }
        regions
    }

    /// Where the sections drawn over the workspace paint (its overlays, the
    /// top bar, Quick Settings and everything above them), in layout pixels;
    /// `None` when some of it cannot be bounded, or the backdrop leaves a
    /// layer open that blends what is drawn after it.
    fn drawn_over_workspace(
        &self,
        sections: &crate::shell::overlay_host::ShellSceneSections,
        parts: &OpaqueParts,
    ) -> Option<Vec<Rect>> {
        let backdrop = sections.backdrop.encoding();
        if !tilo_cpu_raster::plain_layers_open_at(backdrop, backdrop.draw_tags.len()) {
            return None;
        }
        let mut bounds = self.draw_bounds.borrow_mut();
        let mut above = Vec::new();
        for (scene, scene_parts) in [
            (
                &sections.workspace_overlays,
                parts.workspace_overlays.as_ref(),
            ),
            (&sections.topbar, None),
            (&sections.quick_settings, parts.quick_settings.as_ref()),
            (
                &sections.above_quick_settings,
                parts.above_quick_settings.as_ref(),
            ),
        ] {
            above.extend(ui::scene_parts::drawn_bounds(
                scene,
                scene_parts,
                &mut bounds,
            )?);
        }
        Some(above)
    }

    /// The frame's damage and copies for a presenter that moves pixels
    /// within its target, from the copies the workspace's in-process apps
    /// made (`copies`) and every other change (`regions`): what is drawn over
    /// a copy is repainted where it is and where the copy took it from, a
    /// copy that cannot be made repaints where it lands
    /// ([`ui::scene_parts::present_copies`]). The damage it returns is every
    /// pixel the frame changes, copies included.
    fn present_scroll_copies(
        &self,
        sections: &crate::shell::overlay_host::ShellSceneSections,
        parts: &OpaqueParts,
        copies: &[ui::scene_parts::SectionCopy],
        regions: Vec<FineDamageRect>,
    ) -> (Option<Vec<FineDamageRect>>, Option<ScrollCopies>) {
        let (Some(to_scene), Some(above)) = (
            self.lone_output_transform(),
            self.drawn_over_workspace(sections, parts),
        ) else {
            return (Some(self.repaint_copies(copies, regions)), None);
        };
        let damage = regions
            .iter()
            .map(|rect| {
                Rect::new(
                    f64::from(rect.x),
                    f64::from(rect.y),
                    f64::from(rect.x) + f64::from(rect.width),
                    f64::from(rect.y) + f64::from(rect.height),
                )
            })
            .collect();
        let size = (scene_px(self.scene_size.0), scene_px(self.scene_size.1));
        let presented = ui::scene_parts::present_copies(copies, damage, &above, to_scene, size);
        let raster: Vec<FineDamageRect> = presented
            .raster
            .iter()
            .filter_map(|rect| self.scene_tile_rect(*rect))
            .collect();
        if presented.copies.is_empty() {
            return (Some(raster), None);
        }
        let mut changed = raster.clone();
        changed.extend(
            presented
                .copies
                .iter()
                .filter_map(|copy| self.scene_tile_rect(copy.destination_rect())),
        );
        (
            Some(changed),
            Some(ScrollCopies {
                copies: presented.copies,
                raster: without_contained_regions(&raster),
            }),
        )
    }

    /// The frame's scene from its sections, in scene pixels, with the
    /// filters over the whole desktop; and the fullscreen client a display
    /// may scan out instead, when it is still on top of all of that.
    fn screen_space_scene(
        &self,
        sections: &crate::shell::overlay_host::ShellSceneSections,
        fullscreen_on_top: Option<crate::shell::scanout_candidate::DrawnOnTop>,
        foreign_images: &[super::foreign_damage::DrawnImage],
        global_filter: bool,
    ) -> (
        Scene,
        Option<crate::shell::scanout_candidate::ScanoutCandidate>,
    ) {
        let mut scene = Scene::new();
        sections.compose_into(&mut scene);
        // A popup, the on-screen keyboard or anything else drawn above the
        // fullscreen client in its section leaves it not on top.
        let fullscreen_on_top = fullscreen_on_top
            .filter(|drawn| drawn.still_last_in(&sections.above_quick_settings))
            .and_then(|drawn| {
                let (surface, _) = self.fullscreen_overlay.as_ref()?;
                Some((*surface, drawn))
            });
        // Otherwise a client frame drawn last of all (a maximized window's,
        // drawn edge to edge) is on top of everything on its display.
        let draws_below_filters = scene.encoding().draw_tags.len();
        let client_on_top = fullscreen_on_top.or_else(|| {
            let drawn = crate::shell::scanout_candidate::DrawnOnTop::last_in(
                &scene,
                draws_below_filters.checked_sub(1)?,
            );
            if drawn.is_none() {
                // Why the display composites rather than scans a client out:
                // what each section drew, backdrop first.
                tracing::trace!(
                    target: "tilo::scanout",
                    draws = ?[
                        &sections.backdrop,
                        &sections.workspace,
                        &sections.workspace_overlays,
                        &sections.topbar,
                        &sections.quick_settings,
                        &sections.above_quick_settings,
                    ]
                    .map(|section| section.encoding().draw_tags.len()),
                    "no client frame is the last thing drawn"
                );
            }
            let drawn = drawn?;
            let surface = super::foreign_damage::surface_of(foreign_images, drawn.image())?;
            Some((surface, drawn))
        });
        if self.watch_configured_theme && self.night_light {
            let (w, h) = self.viewport;
            let bounds = Rect::new(0.0, 0.0, w, h);
            scene.push_layer(
                Fill::NonZero,
                BlendMode::new(Mix::Multiply, Compose::SrcOver),
                1.0,
                Affine::IDENTITY,
                &bounds,
            );
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                night_light_multiplier(self.night_light_temperature),
                None,
                &bounds,
            );
            scene.pop_layer();
        }
        if self.watch_configured_theme {
            self.apply_visual_accessibility(&mut scene);
        }
        // Night light, a colour filter and the magnifier change every pixel
        // of the output; the magnifier does it without drawing anything more.
        // While a screenshot holds the windows still, a client's live buffer
        // is never flipped to the screen in place of its held frame.
        let client_on_top = client_on_top.filter(|_| {
            !global_filter
                && scene.encoding().draw_tags.len() == draws_below_filters
                && !super::foreign::store().frames_held()
        });
        let scene = self.to_scene(scene);
        let scanout = client_on_top
            .and_then(|(surface, drawn)| self.scanout_candidate(surface, drawn, &scene));
        (scene, scanout)
    }

    /// The candidate for direct scanout when `drawn`, the fullscreen client
    /// on top of the logical desktop, keeps one buffer pixel per pixel once
    /// `scene` magnifies the desktop for its displays. Each display draws its
    /// own copy inside a clip of its scene rectangle; those rectangles must
    /// not overlap, or another display's copy could be drawn over it.
    fn scanout_candidate(
        &self,
        surface: super::foreign::SurfaceId,
        drawn: crate::shell::scanout_candidate::DrawnOnTop,
        scene: &Scene,
    ) -> Option<crate::shell::scanout_candidate::ScanoutCandidate> {
        let placed = match &self.outputs {
            Some(outputs) if !output_scene::is_identity(outputs) => {
                let centre = drawn.bounds().center();
                let output = outputs.iter().find(|output| output.contains(centre))?;
                let overlapped = outputs.iter().any(|other| {
                    other.id != output.id && other.scene.intersect(output.scene).area() > 0.0
                });
                if overlapped {
                    return None;
                }
                drawn.placed(output.to_scene())
            }
            _ => drawn,
        };
        let bounds = placed.bounds();
        let candidate =
            crate::shell::scanout_candidate::ScanoutCandidate::new(surface, placed, scene);
        tracing::trace!(
            target: "tilo::scanout",
            surface,
            ?bounds,
            pixel_exact = candidate.is_some(),
            "a client frame is on top of everything drawn"
        );
        candidate
    }

    /// A floating surface shows a blur of what is below it, which can change
    /// without its own section changing (a ticking clock under its edge, a
    /// repaint inside the blur's reach). A blurred surface is therefore
    /// repainted with any damage that falls within its bounds and the reach
    /// of its blur ([`RecordedBackdrop::reach`]): the blur reads nothing
    /// further away, so damage elsewhere cannot change it, and a surface on
    /// no display shows nothing to repair. (Every blurred surface used to join
    /// every damage, so one tooltip's blur made a typed character repaint
    /// the palette, the notifications and Quick Settings with it.) An
    /// unchanged frame (`Some` and empty) stays unchanged: nothing below moved.
    fn with_backdrop_damage(
        &self,
        damage: Option<Vec<FineDamageRect>>,
        surfaces: &[RecordedBackdrop],
    ) -> Option<Vec<FineDamageRect>> {
        let mut regions = damage?;
        if regions.is_empty() || surfaces.is_empty() {
            return Some(regions);
        }
        let damaged = regions.clone();
        for surface in surfaces {
            let Some(bounds) = self.native_damage_rect(Some(surface.bounds)) else {
                // On no display: nothing of it is shown.
                continue;
            };
            let reach = self
                .native_damage_rect(Some(
                    surface.bounds.inflate(surface.reach(), surface.reach()),
                ))
                .unwrap_or(bounds);
            let touched = damaged.iter().any(|rect| {
                rect.x < reach.x.saturating_add(reach.width)
                    && reach.x < rect.x.saturating_add(rect.width)
                    && rect.y < reach.y.saturating_add(reach.height)
                    && reach.y < rect.y.saturating_add(rect.height)
            });
            if touched {
                regions.push(bounds);
            }
        }
        Some(regions)
    }

    /// Damage in the sections that are opaque outside their parts: the
    /// workspace (`with_workspace`), its overlays and the sections above
    /// Quick Settings. Each is unchanged, or changed only by foreign buffer
    /// replacements (their buffers' damage), or changed only inside its parts
    /// (their old and new bounds), or both; anything else is `None`.
    fn opaque_damage_regions(
        &self,
        (previous, current): (
            &ProducedSections,
            &crate::shell::overlay_host::ShellSceneSections,
        ),
        parts: &OpaqueParts,
        images: &[super::foreign_damage::DrawnImage],
        with_workspace: bool,
        mut copies: Option<&mut Vec<ui::scene_parts::SectionCopy>>,
    ) -> Option<Vec<FineDamageRect>> {
        let old_scenes = [
            &previous.backdrop,
            &previous.workspace,
            &previous.workspace_overlays,
            &previous.topbar,
            &previous.quick_settings,
            &previous.above_quick_settings,
        ];
        let new_scenes = [
            &current.backdrop,
            &current.workspace,
            &current.workspace_overlays,
            &current.topbar,
            &current.quick_settings,
            &current.above_quick_settings,
        ];
        // A replacement is repaired only where it was recorded, so it may be
        // normalized away only while its image has no other use (a preview).
        // Without normalization, a replaced image is a changed command like
        // any other: inside a part it damages the part, outside it none.
        let normalizable = super::foreign_damage::unique_uses(old_scenes, &previous.foreign_images)
            && super::foreign_damage::unique_uses(new_scenes, images);
        let mut regions = Vec::new();
        // The workspace comes first.
        for (index, (old, old_parts, new, new_parts)) in [
            (
                &previous.workspace,
                &previous.parts.workspace,
                &current.workspace,
                &parts.workspace,
            ),
            (
                &previous.workspace_overlays,
                &previous.parts.workspace_overlays,
                &current.workspace_overlays,
                &parts.workspace_overlays,
            ),
            (
                &previous.above_quick_settings,
                &previous.parts.above_quick_settings,
                &current.above_quick_settings,
                &parts.above_quick_settings,
            ),
        ]
        .into_iter()
        .enumerate()
        .filter(|(index, _)| with_workspace || *index > 0)
        {
            if old.encoding_eq(new) {
                continue;
            }
            let normalized = (normalizable && !images.is_empty())
                .then(|| {
                    super::foreign_damage::normalize_images(new, &previous.foreign_images, images)
                })
                .flatten();
            let compared = match &normalized {
                Some((scene, image_damage)) => {
                    // A buffer's damage where no display shows it (the part
                    // of a window past the screen's edge) repairs nothing.
                    for &bounds in image_damage {
                        regions.extend(self.native_damage_rect(Some(bounds)));
                    }
                    scene
                }
                None => new,
            };
            if old.encoding_eq(compared) {
                continue;
            }
            // The workspace holds the in-process apps whose scrolls copy.
            let rects = match copies.as_deref_mut().filter(|_| index == 0) {
                Some(copies) => {
                    let damage = ui::scene_parts::damage_with_copies(
                        old,
                        old_parts.as_ref()?,
                        compared,
                        new_parts.as_ref()?,
                        &mut self.draw_bounds.borrow_mut(),
                    )
                    .ok()?;
                    copies.extend(damage.copies);
                    damage.rects
                }
                None => {
                    ui::scene_parts::damage(old, old_parts.as_ref()?, compared, new_parts.as_ref()?)
                        .ok()?
                }
            };
            // `scene_parts::damage` admits only finite, non-negative bounds,
            // which already cover antialiasing; a part no display shows
            // needs no repair.
            for rect in rects {
                regions.extend(self.native_damage_rect(Some(rect)));
            }
        }
        Some(regions)
    }

    /// Update only this producer's presenter capacity. Captures keep the default dense path.
    pub fn set_retained_layer_width(&mut self, width: Option<u32>) {
        let width = width.filter(|width| *width > 0);
        if self.retained_layer_width != width {
            self.retained_layer_width = width;
            self.last_produced_sections = None;
        }
    }

    /// Tell the producer what its presenter asks of strip pans: whole-pixel
    /// pans (`pixel_aligned`, a CPU presenter), and frames as layers while
    /// a strip pans (`composites_layers`, the DRM GPU presenter). Layers are
    /// drawn only where the scene is one display, at any scale; there the
    /// pans land on whole device pixels too, so a pan moves strip pixels
    /// without resampling them.
    pub fn set_strip_pan_presenter(&mut self, pixel_aligned: bool, composites_layers: bool) {
        self.strip_pan_presenter = StripPanPresenter {
            pixel_aligned,
            composites_layers,
        };
        if composites_layers {
            self.set_layer_policy(LayerPolicy::WhilePanning);
            // The presenter keeps layers of the output's size, never a
            // canvas: no strip is too wide for it.
            self.set_retained_layer_width(Some(u32::MAX));
        } else {
            self.set_retained_layer_width(None);
        }
        self.align_strip_pans();
    }

    /// Say once why frames stopped coming as layers for a presenter that
    /// wants every frame as layers: it then draws the whole screen, which on
    /// a CPU renderer costs most of a frame.
    fn note_layers_off(&mut self, reason: &'static str) {
        if self.layers_off != Some(reason) {
            self.layers_off = Some(reason);
            eprintln!("STRIP-LAYERS-OFF reason={reason:?}");
        }
    }

    /// The presenter keeps every layer in a canvas of its own, strips in
    /// canvas coordinates up to `width` scene pixels wide: every frame the
    /// layers can express comes as layers ([`LayerPolicy::Always`]), so the
    /// presenter never needs the screen drawn whole after a pan.
    pub fn keep_strip_canvases(&mut self, width: u32) {
        self.set_layer_policy(LayerPolicy::Always);
        self.set_retained_layer_width(Some(width));
        self.align_strip_pans();
    }

    /// Tell the producer whether its presenter keeps its target between
    /// frames and can move pixels within it before it repaints the damage
    /// ([`FramePaint::scroll_copies`]): a scroll in an in-process app then
    /// copies what stays on screen instead of repainting it.
    pub fn set_scroll_copy_presenter(&mut self, copies: bool) {
        if self.scroll_copy_presenter != copies {
            self.scroll_copy_presenter = copies;
            self.last_produced_sections = None;
        }
    }

    /// Whole-pixel pans where the presenter reuses strip pixels as they are.
    ///
    /// A presenter compositing layers gets pans on whole device pixels: the
    /// drawn pan is the gesture's rounded to one device pixel ([`pan_step`]):
    /// a layout pixel at 100 %, half of one at 200 %, two thirds at 150 %,
    /// four fifths at 125 %. A pan then moves the strip by whole device
    /// pixels, which a copy reproduces exactly, and as finely as the panel
    /// can show. What rides the strip moves with it by exactly the pan, so
    /// the panned strips are what the desktop drawn without layers shows, and nothing twitches when a pan ends. Layout and input see the
    /// same drawn pan, so what is under the pointer is what is drawn there.
    /// A CPU presenter keeps whole layout pixels.
    fn align_strip_pans(&mut self) {
        let StripPanPresenter {
            pixel_aligned,
            composites_layers,
        } = self.strip_pan_presenter;
        let layered_step = self
            .lone_output_transform()
            .filter(|_| composites_layers && self.lone_output())
            .and_then(|to_scene| {
                let [scale_x, ..] = to_scene.as_coeffs();
                pan_step(scale_x)
            });
        let quantum = layered_step.or_else(|| pixel_aligned.then_some(1.0));
        self.host.set_strip_pan_quantum(quantum);
    }

    /// One display: the scene a layer-compositing presenter renders as a
    /// whole. Several displays each render their own view of the scene, so
    /// they keep it.
    fn lone_output(&self) -> bool {
        self.outputs
            .as_ref()
            .is_none_or(|outputs| outputs.len() == 1)
    }

    /// Which frames this producer's presenter gets as layers.
    pub fn set_layer_policy(&mut self, policy: LayerPolicy) {
        if self.layer_policy != policy {
            self.layer_policy = policy;
            self.last_produced_sections = None;
        }
    }

    /// A filter following the pointer (the magnifier, the crosshair) moves
    /// pixels or lines across the whole desktop: no frame under it is sparse.
    fn pointer_filter(&self) -> bool {
        self.watch_configured_theme
            && (self.visual_accessibility.magnifier.is_on()
                || self.visual_accessibility.cursor_crosshair)
    }

    /// The per-pixel tint the scene ends with (night light, a colour filter).
    fn tint(&self) -> Tint {
        if !self.watch_configured_theme {
            return Tint::default();
        }
        Tint {
            night_light: self
                .night_light
                .then(|| night_light_multiplier(self.night_light_temperature)),
            color_filter: self.visual_accessibility.color_filter,
            viewport: self.viewport,
        }
    }

    /// A filter over the whole desktop (night light, a colour filter, the
    /// magnifier, the crosshair) transforms every pixel of the scene.
    fn global_filter(&self) -> bool {
        self.watch_configured_theme
            && (self.night_light
                || !matches!(self.visual_accessibility.color_filter, ColorFilter::None)
                || self.visual_accessibility.magnifier.is_on()
                || self.visual_accessibility.cursor_crosshair)
    }

    /// The logical desktop → scene pixels when the scene is one display's
    /// view of it (or the logical desktop itself, unmagnified); `None` on
    /// several displays, each of which draws the desktop at its own place
    /// and scale.
    fn lone_output_transform(&self) -> Option<Affine> {
        match self.outputs.as_deref() {
            None => Some(Affine::IDENTITY),
            Some(outputs) if output_scene::is_identity(outputs) => Some(Affine::IDENTITY),
            Some([output]) => Some(output.to_scene()),
            Some(_) => None,
        }
    }

    /// How the strips are drawn for a presenter that composites layers, in
    /// scene pixels, or `None` when the desktop cannot be drawn as layers:
    /// on several displays each draws it at its own place and scale.
    ///
    /// Every strip with a row on screen is drawn on its own
    /// ([`crate::shell::overlay_host::PannedSections`]) and shown inside its
    /// pan rect rounded out to whole scene pixels; strips whose rects meet
    /// each draw over the other there. On a scaled display the pans are
    /// whole device pixels ([`Self::align_strip_pans`]), so a pan moves a
    /// strip's pixels by whole pixels.
    fn strip_plan(&self) -> Option<StripPlan> {
        use crate::shell::state::StripId;
        let to_scene = self.lone_output_transform()?;
        let screen_w = scene_px(self.scene_size.0).max(1);
        let screen_h = scene_px(self.scene_size.1).max(1);
        // A presenter that keeps the strips as they are on screen
        // ([`LayerPolicy::WhilePanning`]) keeps no canvas texture, so the
        // canvas may be as wide as the strip.
        let max_width = match self.layer_policy {
            LayerPolicy::Always => self.retained_layer_width?.min(MAX_CANVAS_WIDTH),
            LayerPolicy::WhilePanning => self.retained_layer_width?,
        };
        if screen_w > max_width {
            return None;
        }
        // Between two strips both are cut at the deck's edge and move every
        // frame: they are drawn where they are on screen.
        if self.host.strip_deck_moving() {
            return Some(StripPlan::baked(screen_w));
        }
        // Scene pixels per layout pixel along a pan.
        let [scale_x, ..] = to_scene.as_coeffs();
        let mut rows: Vec<(f64, u64, f64, f64)> = self
            .host
            .strip_scroll_offsets()
            .into_iter()
            .filter_map(|(id, scroll, max_scroll)| {
                let rect = self.host.strip_row_rect(StripId(id))?;
                Some((rect.y0, id, scroll, max_scroll))
            })
            .collect();
        // The order the host draws them apart in.
        rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut plan = StripPlan {
            unbake: true,
            width: screen_w,
            strips: Vec::new(),
            ids: Vec::new(),
            to_scene,
        };
        for (_, id, scroll, max_scroll) in rows {
            let strip = StripId(id);
            let (Some(rect), Some(pans)) = (
                self.host.strip_row_rect(strip),
                self.host.strip_pan_rect(strip),
            ) else {
                return Some(StripPlan::baked(screen_w));
            };
            let pad = self.host.strip_canvas_pad(strip);
            let canvas = Rect::new(rect.x0, rect.y0, rect.x1 + max_scroll + 2.0 * pad, rect.y1);
            let right = scene_px(to_scene.transform_rect_bbox(canvas).x1.ceil());
            if right > max_width {
                // Too wide for the presenter: the pans stay in the scene.
                return Some(StripPlan::baked(screen_w));
            }
            plan.width = plan.width.max(right);
            // A strip outside its canvas (a rubber band past the pad) is
            // drawn where it is on screen, unpanned.
            let shift = if ui::widget::pan_within_canvas(scroll, max_scroll, pad) {
                (scroll + pad) * scale_x
            } else {
                0.0
            };
            // A pan between scene pixels keeps its exact screen-space
            // drawing.
            if !nearly_whole(shift) {
                return Some(StripPlan::baked(screen_w));
            }
            let dest = to_scene.transform_rect_bbox(pans);
            let x0 = scene_px(dest.x0.floor()).min(screen_w);
            let y0 = scene_px(dest.y0.floor()).min(screen_h);
            let x1 = scene_px(dest.x1.ceil()).min(screen_w);
            let y1 = scene_px(dest.y1.ceil()).min(screen_h);
            plan.strips.push(StripBlit {
                dest_x: x0,
                dest_y: y0,
                dest_w: x1.saturating_sub(x0).max(1),
                dest_h: y1.saturating_sub(y0).max(1),
                scroll_x: scene_px(shift.round()),
            });
            plan.ids.push(strip);
        }
        Some(plan)
    }

    /// The strips the host drew apart for `plan`, in scene pixels; `None`
    /// when they do not match it, or a strip's floating surfaces drew (on
    /// screen they are drawn above every strip): the frame is then drawn in
    /// screen space.
    fn panned_pieces(
        plan: &StripPlan,
        panned: crate::shell::overlay_host::PannedSections,
    ) -> Option<PannedPieces> {
        let crate::shell::overlay_host::PannedSections {
            strips,
            strip_parts,
            floating_drawn,
        } = panned;
        if floating_drawn
            || strips.len() != plan.ids.len()
            || strips
                .iter()
                .zip(&plan.ids)
                .any(|((drawn, _), planned)| drawn != planned)
            || strip_parts.len() != strips.len()
        {
            return None;
        }
        let (strips, parts) = strips
            .into_iter()
            .zip(strip_parts)
            .map(|((_, scene), parts)| magnified_with_parts(scene, parts, plan.to_scene))
            .unzip();
        Some(PannedPieces { strips, parts })
    }

    #[cfg(test)]
    fn damage_from_sections(
        &self,
        current: &crate::shell::overlay_host::ShellSceneSections,
        current_global_filter: bool,
    ) -> Option<Vec<FineDamageRect>> {
        self.damage_from_sections_with_images(
            current,
            &OpaqueParts::default(),
            (current_global_filter, self.tint()),
            &[],
            None,
        )
    }

    /// `filters` is the pointer filter and the tint this frame ends with.
    /// With `copies`, the workspace's in-process apps' scroll copies are
    /// collected there instead of damaging where they land (the caller
    /// repaints there if it makes none of them).
    fn damage_from_sections_with_images(
        &self,
        current: &crate::shell::overlay_host::ShellSceneSections,
        parts: &OpaqueParts,
        (pointer_filter, tint): (bool, Tint),
        images: &[super::foreign_damage::DrawnImage],
        copies: Option<&mut Vec<ui::scene_parts::SectionCopy>>,
    ) -> Option<Vec<FineDamageRect>> {
        let previous = self.last_produced_sections.as_ref()?;
        if previous.global_filter || pointer_filter || previous.tint != tint {
            return None;
        }
        if !previous.backdrop.encoding_eq(&current.backdrop) {
            return None;
        }
        let mut regions =
            self.opaque_damage_regions((previous, current), parts, images, true, copies)?;
        let (bars_changed, bars) = self.bar_damage(previous, current, parts)?;
        if !bars_changed {
            // Proven identical to the previous frame. `None` means "no
            // authority, redraw everything"; an empty list means the pixels
            // already on screen are still correct.
            return Some(regions);
        }
        regions.extend(bars);
        (!regions.is_empty()).then_some(regions)
    }

    /// Where the chrome of a drawing of the desktop as layers ([`FrameLayers::chrome`]:
    /// the workspace's overlays, the top bar, Quick Settings and the sections
    /// above it) differs between the sections `previous` and `current`, in
    /// scene pixels; `None` when that cannot be bounded. A foreign buffer
    /// replaced in the chrome counts as a changed command: inside a part it
    /// damages the part, outside one it leaves the chrome unbounded.
    fn chrome_damage(
        &self,
        previous: &ProducedSections,
        current: &crate::shell::overlay_host::ShellSceneSections,
        parts: &OpaqueParts,
        (pointer_filter, tint): (bool, Tint),
    ) -> Option<Vec<FineDamageRect>> {
        if previous.global_filter || pointer_filter || previous.tint != tint {
            return None;
        }
        let mut regions =
            self.opaque_damage_regions((previous, current), parts, &[], false, None)?;
        let (bars_changed, bars) = self.bar_damage(previous, current, parts)?;
        regions.extend(bars);
        (!bars_changed || !regions.is_empty()).then_some(regions)
    }

    /// Damage in the top bar and Quick Settings sections between `previous`
    /// and `current`: whether either changed, and where; `None` when a
    /// change cannot be bounded.
    fn bar_damage(
        &self,
        previous: &ProducedSections,
        current: &crate::shell::overlay_host::ShellSceneSections,
        parts: &OpaqueParts,
    ) -> Option<(bool, Vec<FineDamageRect>)> {
        let topbar_changed = !previous.topbar.encoding_eq(&current.topbar);
        let quick_settings_changed = !previous.quick_settings.encoding_eq(&current.quick_settings);
        let mut regions = Vec::new();
        if topbar_changed {
            if let Some(changed) = self.topbar_part_damage(previous, current, parts) {
                regions.extend(changed);
            } else {
                let old = self.topbar_damage_rect(&previous.topbar, previous.topbar_bounds)?;
                let new = self.topbar_damage_rect(&current.topbar, current.topbar_bounds)?;
                regions.extend([old, new]);
            }
        }
        if quick_settings_changed
            && let Some(changed) = self.quick_settings_part_damage(previous, current, parts)
        {
            regions.extend(changed);
        } else if quick_settings_changed {
            let old = self.damage_rect_for_quick_settings(
                &previous.quick_settings,
                previous.quick_settings_bounds,
            );
            let new = self.damage_rect_for_quick_settings(
                &current.quick_settings,
                current.quick_settings_bounds,
            );
            match (old, new) {
                (QuickSettingsDamage::Unknown, _) | (_, QuickSettingsDamage::Unknown) => {
                    return None;
                }
                (QuickSettingsDamage::Absent, QuickSettingsDamage::Absent) => {}
                (QuickSettingsDamage::Bounds(old), QuickSettingsDamage::Absent) => {
                    regions.push(old);
                }
                (QuickSettingsDamage::Absent, QuickSettingsDamage::Bounds(new)) => {
                    regions.push(new);
                }
                (QuickSettingsDamage::Bounds(old), QuickSettingsDamage::Bounds(new)) => {
                    regions.extend([old, new]);
                }
            }
        }
        Some((topbar_changed || quick_settings_changed, regions))
    }

    fn topbar_damage_rect(&self, section: &Scene, bounds: Option<Rect>) -> Option<FineDamageRect> {
        // An empty topbar is valid only for a non-production/test root; because a changed empty
        // section has no pixels to repair, it must not be the sole sparse authority.
        if section.encoding().is_empty() {
            return None;
        }
        self.native_damage_rect(bounds)
    }

    /// Damage in the top bar proven by its parts: each changed part's old and
    /// new bounds. `None` when the parts cannot prove it (a change outside
    /// every part); the bar's whole extent then bounds it.
    fn topbar_part_damage(
        &self,
        previous: &ProducedSections,
        current: &crate::shell::overlay_host::ShellSceneSections,
        parts: &OpaqueParts,
    ) -> Option<Vec<FineDamageRect>> {
        let rects = ui::scene_parts::damage(
            &previous.topbar,
            previous.parts.topbar.as_ref()?,
            &current.topbar,
            parts.topbar.as_ref()?,
        )
        .ok()?;
        let regions: Vec<FineDamageRect> = rects
            .into_iter()
            .filter_map(|rect| self.native_damage_rect(Some(rect)))
            .collect();
        (!regions.is_empty()).then_some(regions)
    }

    /// Damage in the Quick Settings section proven by its parts (the panel,
    /// the strip ribbon, the strip rail): each changed part's old and new
    /// bounds. `None` when the parts cannot prove it; the section's whole
    /// extent then bounds it ([`Self::damage_rect_for_quick_settings`]).
    fn quick_settings_part_damage(
        &self,
        previous: &ProducedSections,
        current: &crate::shell::overlay_host::ShellSceneSections,
        parts: &OpaqueParts,
    ) -> Option<Vec<FineDamageRect>> {
        let rects = ui::scene_parts::damage(
            &previous.quick_settings,
            previous.parts.quick_settings.as_ref()?,
            &current.quick_settings,
            parts.quick_settings.as_ref()?,
        )
        .ok()?;
        // A part no display shows needs no repair.
        let regions: Vec<FineDamageRect> = rects
            .into_iter()
            .filter_map(|rect| self.native_damage_rect(Some(rect)))
            .collect();
        (!regions.is_empty()).then_some(regions)
    }

    /// An empty section with no reported visual extent is a proven absent QS layer. A nonempty
    /// section without bounds is not safe to retain, even if the caller believes it is a close.
    fn damage_rect_for_quick_settings(
        &self,
        section: &Scene,
        bounds: Option<Rect>,
    ) -> QuickSettingsDamage {
        match bounds {
            Some(bounds) => self
                .native_damage_rect(Some(bounds))
                .map_or(QuickSettingsDamage::Unknown, QuickSettingsDamage::Bounds),
            None if section.encoding().is_empty() => QuickSettingsDamage::Absent,
            None => QuickSettingsDamage::Unknown,
        }
    }

    fn native_damage_rect(&self, rect: Option<Rect>) -> Option<FineDamageRect> {
        // Sections are drawn in logical coordinates; the damage is in the
        // scene pixels each display drew them into.
        self.scene_tile_rect(self.layout_rect_to_scene(rect?)?)
    }

    /// `rect`, in scene pixels, rounded out to whole clip tiles inside the
    /// scene.
    pub(crate) fn scene_tile_rect(&self, rect: Rect) -> Option<FineDamageRect> {
        // Vello evaluates clip layers in 16px tiles. Even outside a layer's
        // exact shape, compositing that tile can round an existing channel by
        // one unit. Retain complete clip tiles so a changed shadow cannot
        // leave those neighbouring pixels from the previous frame behind.
        // Matches TILE_WIDTH/TILE_HEIGHT in vello_encoding/src/config.rs.
        const CLIP_TILE_EDGE: f64 = 16.0;
        let (width, height) = self.scene_size;
        let x0 = ((rect.x0 / CLIP_TILE_EDGE).floor() * CLIP_TILE_EDGE).clamp(0.0, width);
        let y0 = ((rect.y0 / CLIP_TILE_EDGE).floor() * CLIP_TILE_EDGE).clamp(0.0, height);
        let x1 = ((rect.x1 / CLIP_TILE_EDGE).ceil() * CLIP_TILE_EDGE).clamp(0.0, width);
        let y1 = ((rect.y1 / CLIP_TILE_EDGE).ceil() * CLIP_TILE_EDGE).clamp(0.0, height);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "damage bounds are clamped to the positive viewport and rounded outward first"
        )]
        Some(FineDamageRect {
            x: x0 as u32,
            y: y0 as u32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    fn apply_color_filter(&self, scene: &mut Scene, bounds: Rect) {
        let mut overlay = |mix: Mix, color: Color| {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::new(mix, Compose::SrcOver),
                1.0,
                Affine::IDENTITY,
                &bounds,
            );
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &bounds);
            scene.pop_layer();
        };
        if matches!(
            self.visual_accessibility.color_filter,
            ColorFilter::Grayscale | ColorFilter::GrayscaleInverted
        ) {
            // W3C `color` blend with an achromatic source preserves the
            // backdrop luminosity while replacing hue/saturation: true
            // grayscale, not a translucent grey veil.
            overlay(Mix::Color, Color::WHITE);
        }
        if matches!(
            self.visual_accessibility.color_filter,
            ColorFilter::Inverted | ColorFilter::GrayscaleInverted
        ) {
            // Difference against white is the exact per-channel inverse.
            overlay(Mix::Difference, Color::WHITE);
        }
    }

    fn apply_visual_accessibility(&self, scene: &mut Scene) {
        let bounds = Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1);
        self.apply_color_filter(scene, bounds);
        let pointer = self.host.pointer_position();
        if self.visual_accessibility.magnifier.is_on()
            && let Some(centre) = self.magnifier_focus.centre()
        {
            let source = scene.clone();
            let magnify = crate::shell::magnifier::magnify(
                centre,
                f64::from(self.visual_accessibility.magnifier.zoom),
            );
            match self.visual_accessibility.magnifier.view {
                MagnifierView::Lens => {
                    // A lens deliberately leaves the underlying coordinate space
                    // untouched: scaling about the centre keeps the target there
                    // exactly where it really is, so it remains clickable.
                    let lens = crate::shell::magnifier::lens_rect(centre, self.viewport);
                    let clip = RoundedRect::from_rect(lens, 18.0);
                    scene.push_layer(
                        Fill::NonZero,
                        BlendMode::default(),
                        1.0,
                        Affine::IDENTITY,
                        &clip,
                    );
                    scene.append(&source, Some(magnify));
                    scene.pop_layer();
                    scene.stroke(
                        &Stroke::new(3.0),
                        Affine::IDENTITY,
                        self.theme.border_focus,
                        None,
                        &clip,
                    );
                }
                MagnifierView::FullScreen => {
                    // The magnified copy covers the whole screen; scaling about the
                    // centre pans it so the screen's edges are reached as the
                    // centre reaches them.
                    *scene = Scene::new();
                    scene.push_layer(
                        Fill::NonZero,
                        BlendMode::default(),
                        1.0,
                        Affine::IDENTITY,
                        &bounds,
                    );
                    scene.append(&source, Some(magnify));
                    scene.pop_layer();
                }
            }
        }
        if self.visual_accessibility.cursor_crosshair
            && let Some(pointer) = pointer
        {
            let color = self.theme.border_focus;
            scene.stroke(
                &Stroke::new(2.0),
                Affine::IDENTITY,
                color,
                None,
                &vello::kurbo::Line::new((0.0, pointer.y), (self.viewport.0, pointer.y)),
            );
            scene.stroke(
                &Stroke::new(2.0),
                Affine::IDENTITY,
                color,
                None,
                &vello::kurbo::Line::new((pointer.x, 0.0), (pointer.x, self.viewport.1)),
            );
        }
    }

    /// Pick up a shortcut's magnifier change at once, and note where the pointer and the keyboard
    /// focus are for the magnifier to follow.
    fn refresh_magnifier(&mut self) {
        self.refresh_magnifier_at(Instant::now());
    }

    fn refresh_magnifier_at(&mut self, now: Instant) {
        if self.host.take_magnifier_changed() && self.watch_configured_theme {
            self.visual_accessibility = VisualAccessibility::load();
        }
        self.magnifier_focus
            .observe(self.host.pointer_position(), self.keyboard_focus_rect, now);
    }

    /// Where the keyboard focus is on screen, for the magnifier to follow: the focused shell
    /// control, else the focused window. Only looked up while the magnifier is on.
    pub fn note_keyboard_focus(&mut self, focus: Option<NodeId>) {
        self.keyboard_focus_rect = if self.visual_accessibility.magnifier.is_on() {
            focus
                .and_then(|id| self.node_map.get(&id))
                // SAFETY: `node_map` belongs to the current, live layout tree.
                .map(|&widget| unsafe { (*widget).get_rect() })
                .filter(|rect| rect.area() > 0.0)
                .or_else(|| self.host.focused_window_rect())
        } else {
            None
        };
    }

    fn refresh_display_comfort(&mut self) {
        let settings = settings_store::load();
        let scheduled_night_light =
            settings_store::bool_at(&settings, "display", "night_light_schedule").unwrap_or(false);
        let automatic_theme = settings_store::str_at(
            &settings,
            "appearance",
            settings_store::day_schedule::MODE_KEY,
        )
        .is_some_and(settings_store::day_schedule::is_automatic);
        self.day_phase = (scheduled_night_light || automatic_theme).then(|| {
            let schedule = settings_store::day_schedule::Scheduled::from_settings(&settings);
            (schedule, schedule.is_night(ui::theme::local_moment()))
        });
        self.night_light = night_light_on(
            settings_store::bool_at(&settings, "display", "night_light").unwrap_or(false),
            scheduled_night_light,
            self.day_phase.map(|(_, night)| night),
        );
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "f64_in only ever returns a value inside 2_000.0..=6_500.0 (the requested range) or the in-range default"
        )]
        {
            self.night_light_temperature = settings_store::f64_in(
                &settings,
                "display",
                "night_light_temperature",
                2_000.0..=6_500.0,
                3_700.0,
            ) as u32;
        }
        self.visual_accessibility = VisualAccessibility::load();
        self.narrator.set_enabled(
            tilo_accessibility_settings::AccessibilitySettings::load()
                .screen_reader()
                .enabled,
        );
    }

    /// Speak what changed in the chrome since the last frame (focus moves in
    /// Quick Settings and the session menu, notifications, readouts).
    pub fn narrate_chrome(&mut self) {
        if self.narrator.enabled() {
            let snapshot = self.host.chrome_narration_snapshot();
            self.narrator.observe(&snapshot);
        }
    }

    /// Speak what changed in a compositor-owned surface drawn outside the
    /// chrome — the lock screen.
    pub fn narrate(&mut self, snapshot: &ui::a11y::AccessNode) {
        self.narrator.observe(snapshot);
    }

    pub fn narrator_enabled(&self) -> bool {
        self.narrator.enabled()
    }

    fn refresh_configured_theme(&mut self) {
        if !self.watch_configured_theme || self.shots_theme_pinned {
            return;
        }
        let appearance_generation = self.chrome.borrow().appearance_generation;
        let appearance_changed = appearance_generation != self.appearance_generation;
        if !appearance_changed && self.last_theme_store_check.elapsed() < Duration::from_millis(750)
        {
            return;
        }
        self.last_theme_store_check = Instant::now();
        // The followed document's generation, one atomic load: the watcher
        // that parses the file on change publishes it, so paint never asks
        // the filesystem whether the settings moved.
        let stamp = Some(self.followed_settings_generation());
        let phase_changed = self
            .day_phase
            .is_some_and(|(schedule, night)| schedule.is_night(ui::theme::local_moment()) != night);
        if stamp == self.theme_store_stamp && !phase_changed && !appearance_changed {
            return;
        }
        self.theme_store_stamp = stamp;
        self.appearance_generation = appearance_generation;
        self.apply_configured_theme();
        crate::shell::i18n::apply_configured_locale();
        ui::format::region::refresh();
        self.refresh_display_comfort();
        self.rebuild_layout_tree();
        self.relayout();
    }

    /// Read the configured theme and the person's colour (their stored
    /// choice, or the one their identity derives) again, and dress the
    /// chrome in them.
    fn apply_configured_theme(&mut self) {
        let snapshot = self
            .live_settings
            .get_or_insert_with(|| settings_store::live::LiveSettings::start(|| {}))
            .snapshot();
        // The wallpaper is part of the look: a choice in Settings lands with
        // the same settings change that would retheme the desktop.
        let active = {
            let chrome = self.chrome.borrow();
            let tree = chrome.tree();
            tree.desktop
                .active_workspace()
                .map(|workspace| workspace.id.0.to_string())
        };
        super::desktop_backdrop::follow_wallpaper_choice(
            &snapshot.document,
            active.as_deref().filter(|_| self.watch_configured_theme),
        );
        // So do the desk widgets' choices (when each shows, which edge).
        self.host.set_desk_widget_prefs(
            settings_store::desk_widgets::DeskWidgetPrefs::from_document(&snapshot.document),
        );
        // `Theme::configured` leaves text size and interface scale to its
        // host; the chrome is that host, so it applies the person's choice
        // here, on every settings change, as an app window does.
        // Read from the same document `Theme::configured` reads.
        let access = Theme::configured_accessibility();
        self.configured_theme = Theme::configured()
            .with_ui_scale(access.ui_scale)
            .with_text_scale(access.text_scale);
        self.windows
            .set_your_colour(crate::shell::state::read_local_presence_hue());
        self.theme = self
            .configured_theme
            .clone()
            .in_window(&self.windows.active());
        self.text_renderer.set_text_scale(self.theme.text_scale);
        self.text_renderer
            .set_distinct_letters(self.theme.tokens.typography.distinct_letters);
    }

    /// Entering a workspace enters its temperature: the neutral material
    /// takes the hue the person set for a workspace
    /// (`appearance.workspace_temperatures`), or else its wallpaper's
    /// ([`super::desktop_backdrop::native_workspace_hue`]); a grey wallpaper
    /// leaves it neutral. Only the hue moves, so a neutral palette shows
    /// none. Every workspace is looked at when the active one changes and
    /// every few seconds otherwise, so a new wallpaper retints the room it
    /// was set on, in the chrome and in every app window shown there. Live
    /// sessions only: offscreen capture stays on the theme it was asked for.
    fn refresh_workspace_temperature(&mut self) {
        if !self.watch_configured_theme {
            return;
        }
        let chrome = self.chrome.borrow();
        let tree = chrome.tree();
        let workspaces = tree
            .desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.id.0.to_string())
            .collect::<Vec<_>>();
        let active = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.id.0.to_string());
        drop(tree);
        drop(chrome);
        let now = Instant::now();
        if !self.windows.due(active.as_deref(), now) {
            return;
        }
        // The snapshot is the whole document, already parsed: paint does no
        // I/O. Only the section the overrides live in is copied.
        let snapshot = self
            .live_settings
            .get_or_insert_with(|| settings_store::live::LiveSettings::start(|| {}))
            .snapshot();
        let settings = serde_json::json!({
            "appearance": snapshot.document.get("appearance"),
        });
        let mut temperature_of = |id: &str| {
            ui::theme::workspace_temperature_override(&settings, id)
                .or_else(|| super::desktop_backdrop::native_workspace_hue(id))
        };
        if !self
            .windows
            .update(&workspaces, active.as_deref(), now, &mut temperature_of)
        {
            return;
        }
        self.theme = self
            .configured_theme
            .clone()
            .in_window(&self.windows.active());
        self.rebuild_layout_tree();
        self.relayout();
    }

    /// What the app window(s) of `app_id` are shown in: the temperature of
    /// the workspace holding its window (the active one before it has a
    /// tile), the person's colour, and how round its corners are drawn:
    /// the theme's window radius while it is framed, 0 while its tile
    /// draws it edge to edge. The compositor sends it over the app's
    /// shell-ui connection whenever it changes.
    #[must_use]
    pub fn window_appearance_for_app(&self, app_id: &str) -> ui::WindowAppearance {
        let id = self.with_app_workspace(app_id, |workspace| workspace.id.0.to_string());
        // The radius the window's frame is clipped with
        // (`ForeignSurfaceWidget::paint_window`).
        let corner_radius = if crate::shell::foreign::store().app_drawn_full_bleed(app_id) {
            0.0
        } else {
            self.theme.tokens.radii.tile
        };
        ui::WindowAppearance {
            corner_radius: Some(corner_radius),
            ..self.windows.in_workspace(id.as_deref())
        }
    }

    /// Where the room of the workspace holding `app_id`'s window stands
    /// with its host (`crate::shell::room_sync`); `None` for a workspace
    /// that syncs with no host.
    #[must_use]
    pub fn room_sync_for_app(&self, app_id: &str) -> Option<ui::room_sync::RoomSync> {
        let person = self
            .with_app_workspace(app_id, crate::shell::state::Workspace::room)
            .map(|room| self.chrome.borrow().local_user_for_room(&room))?;
        self.with_app_workspace(app_id, |workspace| {
            crate::shell::room_sync::told(
                crate::shell::network::room_topic_id(workspace.room()),
                &workspace.name,
                app_writes(workspace, &person, app_id, tilo_clock::unix_now()),
            )
        })
        .flatten()
    }

    /// `read` of the workspace holding a window of `app_id`, if any.
    fn with_app_workspace<T>(
        &self,
        app_id: &str,
        read: impl FnOnce(&crate::shell::state::Workspace) -> T,
    ) -> Option<T> {
        let chrome = self.chrome.borrow();
        let tree = chrome.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .any(|tab| {
                        matches!(
                            &tab.tile,
                            crate::shell::state::TileContent::Foreign { app_id: candidate, .. }
                                if candidate == app_id
                        )
                    })
            })
            .map(read)
    }

    /// The OS palette before the window's workspace tint is applied.
    #[must_use]
    pub fn configured_app_theme(&self) -> &Theme {
        &self.configured_theme
    }

    /// Read settings in paint from `settings`, the session's one followed
    /// document, rather than from the file.
    pub fn follow_settings(&mut self, settings: settings_store::live::LiveSettings) {
        self.theme_store_stamp = Some(settings.generation());
        self.live_settings = Some(settings);
    }

    /// The generation of the settings paint follows.
    fn followed_settings_generation(&mut self) -> u64 {
        self.live_settings
            .get_or_insert_with(|| settings_store::live::LiveSettings::start(|| {}))
            .generation()
    }

    /// Enable the session-wide configured theme and live settings watcher.
    /// Offscreen builders stay deterministic unless the shots override asks
    /// for a specific palette.
    pub fn enable_configured_theme(&mut self) {
        self.watch_configured_theme = true;
        self.shots_theme_pinned = shots_theme_requested();
        self.apply_configured_theme();
        crate::shell::i18n::apply_configured_locale();
        self.appearance_generation = self.chrome.borrow().appearance_generation;
        self.refresh_display_comfort();
        self.theme_store_stamp = Some(self.followed_settings_generation());
        self.rebuild_layout_tree();
        self.relayout();
    }

    /// Attach the session's peer-presence relay. Called once the
    /// broker and peer sync are both up.
    pub fn set_peer_presence(&mut self, presence: crate::shell::peer_presence::PeerPresence) {
        self.peer_presence = Some(presence);
    }

    /// The session's peer-presence relay, if one is attached.
    pub fn peer_presence(&self) -> Option<&crate::shell::peer_presence::PeerPresence> {
        self.peer_presence.as_ref()
    }

    /// Tell the peer-presence relay where this session's pointer is: scene
    /// point `at`, anchored to the tile under it so collaborators can place
    /// it on their own desks (see [`crate::shell::peer_presence::PointerAnchor`]).
    /// `None` — a pointer that has never moved — stays silent.
    pub fn note_local_pointer(&self, at: Option<(f64, f64)>) {
        let Some(relay) = self.peer_presence.as_ref() else {
            return;
        };
        let tiles = self.host.tab_tiles();
        relay
            .set_pointer(at.and_then(|at| crate::shell::peer_presence::anchor_pointer(&tiles, at)));
    }

    /// Fold everything peers sent since the last frame into the presence the
    /// next paint reads, then restate who is in the room. Returns whether
    /// either changed. The roster refresh runs regardless of whether the
    /// fold found anything — someone who joined but hasn't moved their
    /// pointer publishes nothing, but is in the room all the same.
    pub fn pump_peer_presence(&mut self) -> bool {
        // Arriving where people are pulses their faces in the bar, as a
        // wave does the waver's.
        let arrived = self.host.take_arrival_pulses();
        for person in &arrived {
            self.presence.bounce(
                ui::permissions::UserId(person.clone()),
                self.theme.motion(),
                Instant::now(),
            );
        }
        let Some(relay) = self.peer_presence.as_ref() else {
            return !arrived.is_empty();
        };
        let tiles = self.host.tab_tiles();
        relay.set_shown_strip(self.host.shown_strip());
        let moved = relay.pump(&mut self.presence, &tiles);
        let roster_changed = relay.refresh_roster(&self.presence);
        // Cursor chat: offered over a shared window, and what was typed goes
        // to the window it is over on the room's live plane.
        let room = self
            .chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room().to_string());
        let available = room
            .as_deref()
            .is_some_and(|room| relay.chat_target(room).is_some());
        self.host.set_cursor_chat_available(available);
        let reachable = room
            .as_deref()
            .is_some_and(|room| relay.chat_destination(room).is_some());
        self.host.set_cursor_chat_reachable(reachable);
        // Waves at this person, from whoever waved, on the room's live
        // presence: the bar says who, and their face bounces.
        let waves = relay.take_waves();
        let greeted = self.host.receive_waves(&waves);
        for from in waves {
            self.presence.bounce(
                ui::permissions::UserId(from),
                self.theme.motion(),
                Instant::now(),
            );
        }
        let said = self.host.take_cursor_chat_outbox(Instant::now());
        if let Some(target) = room
            .as_deref()
            .filter(|_| !said.is_empty())
            .and_then(|room| relay.chat_destination(room))
        {
            for out in said {
                match out {
                    crate::shell::overlay_host::ChatOut::Say(said) => {
                        relay.say(
                            &target,
                            said.as_ref().map(|(text, sent)| (text.as_str(), *sent)),
                        );
                    }
                    crate::shell::overlay_host::ChatOut::React(emoji) => {
                        relay.react(&target, &emoji);
                    }
                }
            }
        }
        moved || roster_changed || greeted
    }

    /// Read-only view of the presence the next paint will draw.
    pub fn presence(&self) -> &PresenceCtx {
        &self.presence
    }

    /// Put an authenticated remote-presence payload through the same fold
    /// that a received peer packet uses. Deterministic product proofs need
    /// this seam so they can render intent without standing up a network
    /// relay; it is unavailable in production-only builds.
    /// Write `bytes` to the active Terminal window as its session's output:
    /// for captures of a terminal at work.
    ///
    /// # Errors
    /// No Terminal window is active.
    #[cfg(feature = "hosted-test-control")]
    pub fn feed_active_terminal_for_shots(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.relayout();
        let id = self
            .active_terminal_node()
            .ok_or("no Terminal window is active")?;
        let widget = *self
            .node_map
            .get(&id)
            .ok_or("the Terminal is not laid out")?;
        // SAFETY: the pointer belongs to the current node map.
        let terminal = hosted_terminal(unsafe { &mut *widget })
            .ok_or("the active window is not a hosted Terminal")?;
        terminal.feed(bytes);
        Ok(())
    }

    /// The presence the next paint draws, to seed it for captures.
    #[cfg(feature = "hosted-test-control")]
    pub fn presence_mut_for_shots(&mut self) -> &mut PresenceCtx {
        &mut self.presence
    }

    #[cfg(feature = "hosted-test-control")]
    pub fn apply_remote_presence_for_shots(
        &mut self,
        user: String,
        key: ui::presence::PresenceKey,
        payload: ui::collaboration::PresencePayload,
    ) {
        if let ui::collaboration::PresencePayload::Interaction(update) = payload {
            let _ = self.presence.observe_interaction(*update);
        } else {
            self.presence.apply_remote_presence(user, key, payload);
        }
    }

    #[cfg(feature = "hosted-test-control")]
    /// Draw floating chrome as under the reduced-transparency setting, so a
    /// capture can show the opaque material beside the blurred one.
    pub fn set_reduce_transparency_for_shots(&mut self, on: bool) {
        self.theme = self.theme.clone().with_reduce_transparency(on);
    }

    pub fn set_remote_display_name_for_shots(&mut self, user: &str, name: &str) {
        self.presence
            .set_display_name(ui::permissions::UserId(user.to_string()), name.to_string());
    }

    /// Convenience for the offscreen scene-set path: `relayout` then
    /// `paint`, for when state mutations between scenes can change layout.
    pub fn paint_settled(&mut self) -> Scene {
        self.relayout();
        let scene = self.paint();
        // A draw can discover the layout it was handed isn't the one it
        // needs (the palette's filter band grows a row when kind chips
        // wrap, only paint knows label width). A live session settles this
        // next frame; a single-frame capture has none, so settle here.
        if self.host.structure_dirty() {
            self.relayout();
            return self.paint();
        }
        scene
    }

    /// Mutate chrome state — opens overlays, switches tabs, tweaks
    /// quick-settings, etc. Borrows the chrome internally so callers
    /// don't have to juggle the `RefCell`.
    pub fn with_chrome<F: FnOnce(&mut ChromeState)>(&mut self, f: F) {
        let mut st = self.chrome.borrow_mut();
        f(&mut st);
    }

    /// Read-only handle to the chrome cell.
    pub fn chrome(&self) -> std::cell::Ref<'_, ChromeState> {
        self.chrome.borrow()
    }

    /// Promote a window group to full-output (a surface requesting
    /// `xdg_toplevel.set_fullscreen`), or clear it with `None`. The next
    /// `relayout` collapses the layout to that group (top bar + sidebar +
    /// other strips suppressed); a later `None` restores the exact prior tiling.
    pub fn set_fullscreen(&mut self, group: Option<u64>) {
        self.with_chrome(|st| st.set_fullscreen_group(group));
    }

    /// The group of the client in its own full screen this frame, or
    /// `None`. A client's full screen is the overlay drawn above everything
    /// ([`Self::set_client_fullscreen_surface`]), never a layout collapse,
    /// so another display's desktop stays visible and interactive. Except
    /// while the shell takes the input over it (the search palette, a
    /// sheet): the shell's surfaces would draw under the overlay unseen, so
    /// the group is laid out full screen in the desktop itself, on its own
    /// display, beneath them, and the top bar stays hidden.
    pub fn set_client_fullscreen_group(&mut self, group: Option<u64>) {
        let beneath_shell =
            group.filter(|_| crate::shell::input_policy::chrome_captures_input(self));
        self.set_fullscreen(beneath_shell);
    }

    /// The client in full screen, placed over `rect`: drawn as the overlay
    /// above everything, unless [`Self::set_client_fullscreen_group`] laid
    /// it out beneath the shell this frame. Then nothing is drawn above the
    /// shell's surfaces and no frame of it goes straight to the display.
    pub fn set_client_fullscreen_surface(&mut self, surface: Option<(u64, String, Rect)>) {
        let beneath_shell = self.chrome().client_fullscreen_group().is_some();
        self.set_fullscreen_surface(surface.filter(|_| !beneath_shell));
    }

    /// The group currently promoted to full-output, or `None` in the normal
    /// tiled state. Exposed for the compositor + tests.
    pub fn fullscreen_group(&self) -> Option<u64> {
        self.chrome().fullscreen_group()
    }

    /// Cloneable handle to the shared chrome cell.
    pub fn chrome_handle(&self) -> Chrome {
        self.chrome.clone()
    }

    pub fn host(&self) -> &ShellOverlayHost {
        &self.host
    }

    /// Whether the widget tree has an animation in flight. The native render
    /// loop uses this to decide whether the next frame needs repainting, so
    /// it can idle instead of burning a core on an unchanged scene.
    pub fn needs_animation_frame(&self) -> bool {
        self.animation_demand().any()
    }

    pub(crate) fn content_animating_leaves(&self) -> Vec<String> {
        self.host.content_animating_leaves()
    }

    /// Who holds [`Self::next_wake_deadline`]: chrome sources by name, and
    /// widgets by path. Empty when nothing will wake the loop. Allocates, so
    /// only diagnostics and tests call it.
    pub fn wake_deadline_owners(&self) -> Vec<String> {
        let Some(due) = self.next_wake_deadline() else {
            return Vec::new();
        };
        let limit = due
            .checked_add(crate::shell::animation_diagnostics::WAKE_MATCH_SLACK)
            .unwrap_or(due);
        let holds = |deadline: Option<Instant>| deadline.is_some_and(|at| at <= limit);
        let mut owners = self.host.wake_deadline_owners(due);
        for (_, surface) in &self.floating_overlays {
            if holds(surface.next_wake_deadline()) {
                owners.push(format!("floating/{}", surface.name()));
            }
        }
        for (_, surface) in &self.transient_overlays {
            if holds(surface.next_wake_deadline()) {
                owners.push(format!("transient/{}", surface.name()));
            }
        }
        if holds(
            self.osk_overlay
                .as_ref()
                .and_then(Widget::next_wake_deadline),
        ) {
            owners.push("osk".to_string());
        }
        if holds(self.chrome.borrow().window_transfers.next_poll()) {
            owners.push("window-transfers".to_string());
        }
        owners
    }

    /// Paths to the deepest widgets asking for animation frames, host
    /// sources by label; empty when the loop may idle. For tests and
    /// diagnostics.
    pub fn animation_owners(&self) -> Vec<String> {
        self.animation_owners_at(Instant::now())
    }

    /// [`Self::animation_owners`] with the chrome's animations read at
    /// `now`. An animation a frame started after `now` still counts, so a
    /// test can ask whether a frame began one without racing its length.
    pub fn animation_owners_at(&self, now: Instant) -> Vec<String> {
        let demand = self.animation_demand_at(now);
        let mut owners: Vec<String> = crate::shell::animation_diagnostics::host_labels(demand.host)
            .filter(|label| *label != "content")
            .map(str::to_string)
            .collect();
        if demand.host & crate::shell::animation_diagnostics::CONTENT_BIT != 0 {
            owners.extend(
                self.content_animating_leaves()
                    .into_iter()
                    .map(|leaf| format!("content{leaf}")),
            );
        }
        for (_, surface) in &self.floating_overlays {
            if surface.needs_animation_frame() {
                owners.push(format!("floating/{}", surface.name()));
            }
        }
        for (_, surface) in &self.transient_overlays {
            if surface.needs_animation_frame() {
                owners.push(format!("transient/{}", surface.name()));
            }
        }
        if demand.osk {
            owners.push("osk".to_string());
        }
        // A toolkit menu or popover a shell surface draws in its own pass
        // asks through the motion clock; it is named by where it started.
        owners.extend(
            ui::motion::frames_wanted_by(now)
                .into_iter()
                .map(|caller| format!("motion@{caller}")),
        );
        owners
    }

    pub(crate) fn animation_demand(&self) -> AnimationDemand {
        self.animation_demand_at(Instant::now())
    }

    fn animation_demand_at(&self, now: Instant) -> AnimationDemand {
        AnimationDemand {
            host: self.host.animation_demand_bits(now),
            floating: self
                .floating_overlays
                .iter()
                .any(|(_, surface)| surface.needs_animation_frame()),
            // A toolkit menu or popover a shell surface draws in its own
            // overlay pass asks through the motion clock.
            transient: self
                .transient_overlays
                .iter()
                .any(|(_, surface)| surface.needs_animation_frame())
                || ui::motion::frames_wanted(now),
            osk: self
                .osk_overlay
                .as_ref()
                .is_some_and(Widget::needs_animation_frame),
        }
    }

    /// Earliest one-shot widget deadline. Unlike animation demand this lets
    /// the compositor sleep until the visual state can actually change.
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        [
            self.host.next_wake_deadline(),
            self.floating_overlays
                .iter()
                .filter_map(|(_, surface)| surface.next_wake_deadline())
                .min(),
            self.transient_overlays
                .iter()
                .filter_map(|(_, surface)| surface.next_wake_deadline())
                .min(),
            self.osk_overlay
                .as_ref()
                .and_then(Widget::next_wake_deadline),
            self.chrome.borrow().window_transfers.next_poll(),
            self.peer_windows_poll(),
            self.presence_wake(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// When peers' presence next changes on its own: a still pointer's name
    /// steps aside, a sent cursor-chat message starts to fade or goes.
    fn presence_wake(&self) -> Option<Instant> {
        let now = Instant::now();
        let fade = if self.theme.reduce_motion {
            std::time::Duration::ZERO
        } else {
            ui::presence::POINTER_FADE
        };
        [
            self.presence.next_pointer_name_fade(now, fade),
            self.presence.chats.next_change(now, self.theme.motion()),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A round soon while a peer's window waits to be opened here: its room's
    /// update may land after this round's decision, with nothing left to wake
    /// an idle session for it (a window waiting on its record, too). The tabs
    /// are walked only when the tree may have changed since the last round
    /// found none owed a look.
    fn peer_windows_poll(&self) -> Option<Instant> {
        let owed = {
            let chrome = self.chrome.borrow();
            let tree = chrome.tree();
            self.peer_windows_quiet_at != Some(TreeRevisions::read(&chrome, &tree))
                && crate::shell::peer_windows::any_pending(&tree)
        };
        (owed
            || crate::shell::peer_windows::retry_requested()
            || crate::shell::peer_windows::install_requested())
        .then(|| Instant::now().checked_add(std::time::Duration::from_millis(250)))
        .flatten()
    }

    /// Whether any tab is still holding a place for an app that hasn't drawn
    /// yet. The opening card's spinner keeps [`Self::needs_animation_frame`]
    /// true for the wait's duration, so a caller waiting for quiet has to
    /// distinguish "still starting" from "animation wedged".
    pub fn any_tile_opening(&mut self) -> bool {
        self.host.any_tile_opening()
    }

    /// The chrome's monotonic revision counter. Bumps whenever chrome
    /// state mutates (overlay toggles, notifications, tab switches, …).
    /// The render loop repaints when this changes.
    pub fn chrome_revision(&self) -> u64 {
        self.chrome.borrow().revision
    }

    /// The shell was drawn, its motions read at `motions_at` or later: the
    /// motion clock may stop asking for a frame at rest it has now had, and
    /// the window groups' once-per-drawn-frame motion is told the frame
    /// turned. Every paint says it here, whoever asked for the paint.
    fn frame_drawn(&mut self, motions_at: Instant) {
        ui::motion::frame_drawn(motions_at);
        self.host.frame_presented();
    }

    pub fn host_mut(&mut self) -> &mut ShellOverlayHost {
        &mut self.host
    }

    /// Drain user-confirmed screenshot/recording/PiP work after the selector
    /// has closed. The native compositor owns execution because it alone has
    /// the clean scene and GPU-readable pixels.
    pub fn take_capture_intents(&mut self) -> Vec<ui::widget::CaptureIntent> {
        self.host.take_capture_intents()
    }

    /// Whether the compositor-owned Capture Studio is still awaiting a
    /// selection.  Portal cancellation uses this to resolve its request when
    /// the user dismisses the trusted selector without creating an intent.
    pub fn capture_studio_open(&self) -> bool {
        self.host.capture_studio_open()
    }

    /// See [`ShellOverlayHost::capture_choosing_screenshot`].
    pub fn capture_choosing_screenshot(&self) -> bool {
        self.host.capture_choosing_screenshot()
    }

    /// Drain app-originated call/navigation/media handoffs. The app keeps
    /// producing the source; the compositor chooses the shell presentation.
    pub fn take_floating_surface_requests(
        &mut self,
    ) -> Vec<ui::floating_surface::FloatingSurfaceRequest> {
        self.host.take_floating_surface_requests()
    }

    pub fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        self.host.take_floating_surface_source_updates()
    }

    pub fn active_floating_surface_ids(&self) -> Vec<String> {
        self.host.active_floating_surface_ids()
    }

    pub fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        self.host.on_floating_surface_host_message(message);
    }

    pub fn set_capture_recording_active(&mut self, active: bool) {
        self.host.set_capture_recording_active(active);
    }

    pub fn set_capture_pip_transition_progress(&mut self, permille: u16) {
        self.host.set_capture_pip_transition_progress(permille);
    }

    pub fn set_capture_recording_paused(&mut self, paused: bool) {
        self.host.set_capture_recording_paused(paused);
    }

    pub fn set_capture_recording_finished(&mut self) {
        self.host.set_capture_recording_finished();
    }

    pub fn set_capture_screenshot_finished(
        &mut self,
        kept: ui::widget::ScreenshotKept,
        width: u32,
        height: u32,
        preview: ui::widget::CapturePreview,
    ) {
        self.host
            .set_capture_screenshot_finished(kept, width, height, preview);
    }

    pub fn set_capture_screenshot_transition_progress(&mut self, permille: u16) {
        self.host
            .set_capture_screenshot_transition_progress(permille);
    }

    #[doc(hidden)]
    pub fn set_screenshot_markup_transition_progress(&mut self, permille: u16) {
        self.host
            .set_screenshot_markup_transition_progress(permille);
    }

    pub fn set_capture_screenshot_pin_pending(&mut self, pending: bool) {
        self.host.set_capture_screenshot_pin_pending(pending);
    }

    pub fn set_capture_screenshot_pinned(&mut self) {
        self.host.set_capture_screenshot_pinned();
    }

    pub fn set_capture_replay_active(&mut self, active: bool, audio: ui::widget::AudioCaptureMode) {
        self.host.set_capture_replay_active(active, audio);
    }

    pub fn set_capture_replay_buffered_seconds(&mut self, seconds: u8) {
        self.host.set_capture_replay_buffered_seconds(seconds);
    }

    pub fn set_capture_replay_saved(&mut self) {
        self.host.set_capture_replay_saved();
    }

    pub fn set_capture_audio_available(&mut self, system: bool, microphone: bool) {
        self.host.set_capture_audio_available(system, microphone);
    }

    /// Paint whole live windows in virtual-desktop coordinates without the
    /// physical output clip: the authoritative source for a capture of a
    /// window, of the strip, or of a selection inside a window, including
    /// windows not presently on the monitor. Returns each painted window's id
    /// and the rect it was painted in.
    /// `anywhere` also paints windows of workspaces kept off screen, for a
    /// capture that follows its window wherever the person is.
    pub fn paint_capture_targets(
        &mut self,
        capture: &ui::widget::LiveCapture<'_>,
        anywhere: bool,
    ) -> (Scene, Vec<(String, Rect)>) {
        let mut scene = Scene::new();
        if self.session_locked {
            return (scene, Vec::new());
        }
        // This paint exists only to be captured, so protected surfaces draw
        // their placeholder wherever they are, on screen or not.
        let _capture = crate::shell::capture_protection::CapturePaint::begin();
        let mut ctx = ui::paint::PaintCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
        );
        let rects = if anywhere {
            self.host
                .draw_capture_targets_anywhere(capture, &mut ctx, &self.presence)
        } else {
            self.host
                .draw_capture_targets(capture, &mut ctx, &self.presence)
        };
        (scene, rects)
    }

    /// Every window group a capture can target, focused first — what the
    /// capture studio offers for App and Strip captures.
    pub fn capture_targets(&self) -> Vec<ui::widget::CaptureTarget> {
        self.host.capture_targets()
    }

    /// Tell captures whether the session is locked. While it is, every
    /// capture of the desktop is the placeholder, whatever the lock screen or
    /// a stale desktop scene holds.
    pub fn set_session_locked(&mut self, locked: bool) {
        self.session_locked = locked;
    }

    /// The presented scene as a capture may see it: protected windows and
    /// the shell's secret prompts replaced by the themed placeholder, or the
    /// whole desktop while the session is locked. Every path that copies the
    /// desktop off the local display goes through here; the local display
    /// keeps `presented` untouched. Borrows `presented` when nothing on
    /// screen is protected.
    pub fn capture_scene<'s>(&mut self, presented: &'s Scene) -> std::borrow::Cow<'s, Scene> {
        let mut placeholders = Scene::new();
        if self.session_locked {
            let (width, height) = self.viewport;
            crate::shell::capture_protection::paint_placeholder(
                &mut placeholders,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, width, height),
                0.0,
                &self.theme,
            );
            let mut scene = Scene::new();
            self.compose_layout_scene(&placeholders, &mut scene);
            return std::borrow::Cow::Owned(scene);
        }
        let windows = self.capture_protected_window_rects();
        let prompts = self.host.secret_prompt_rects();
        if windows.is_empty() && prompts.is_empty() {
            return std::borrow::Cow::Borrowed(presented);
        }
        let window_radius = f64::from(self.theme.tokens.radii.tile);
        for rect in windows {
            // One pixel past the tile, so the client's antialiased edge is
            // covered too.
            crate::shell::capture_protection::paint_placeholder(
                &mut placeholders,
                Affine::IDENTITY,
                rect.inflate(1.0, 1.0),
                window_radius,
                &self.theme,
            );
        }
        for rect in prompts {
            crate::shell::capture_protection::paint_placeholder(
                &mut placeholders,
                Affine::IDENTITY,
                rect,
                ui::chrome::ChromeRole::Panel.radius(&self.theme, rect.height()),
                &self.theme,
            );
        }
        let mut scene = presented.clone();
        self.compose_layout_scene(&placeholders, &mut scene);
        std::borrow::Cow::Owned(scene)
    }

    /// Where protected client surfaces are drawn on screen, in logical
    /// desktop coordinates: visible tiles, app-owned floating and fullscreen
    /// surfaces, and the popups and on-screen keyboard the compositor marked
    /// protected.
    fn capture_protected_window_rects(&mut self) -> Vec<Rect> {
        let protected_tabs: Vec<u64> = {
            let chrome = self.chrome.borrow();
            crate::shell::foreign::foreign_tabs(&chrome.tree())
                .into_iter()
                .filter(|tab| {
                    crate::shell::foreign::store().is_capture_protected(tab.surface_id, &tab.app_id)
                })
                .map(|tab| tab.surface_id)
                .collect()
        };
        let mut rects: Vec<Rect> = protected_tabs
            .into_iter()
            .filter_map(|surface_id| self.host.foreign_surface_rect(surface_id))
            .collect();
        rects.extend(
            self.floating_overlays
                .iter()
                .chain(self.fullscreen_overlay.iter())
                .chain(self.transient_overlays.iter())
                .filter_map(|(_, widget)| widget.capture_protected_rect()),
        );
        rects.extend(
            self.osk_overlay
                .as_ref()
                .filter(|keyboard| self.osk_active && keyboard.is_mapped())
                .and_then(ForeignSurfaceWidget::capture_protected_rect),
        );
        rects
    }

    /// Append compositor-owned capture surfaces after the clean desktop
    /// source has been consumed. This keeps live mirrors recursive-safe and
    /// recording status out of the encoded pixels.
    /// Returns whether compositor-owned capture chrome appended encoded scene data.
    /// Hold every window of an app this shell runs still, as last drawn,
    /// until [`Self::release_windows`] (a screenshot's freeze; client
    /// windows are held in the frame store).
    pub fn hold_windows(&mut self) {
        if ui::widget::hold_windows() {
            self.host
                .hold_window_content(&mut self.text_renderer, &self.theme, &self.presence);
        }
    }

    /// Let the windows [`Self::hold_windows`] held draw live again.
    pub fn release_windows(&mut self) {
        ui::widget::release_windows();
    }

    pub fn draw_capture_overlays(&mut self, scene: &mut Scene) -> bool {
        let mut logical = Scene::new();
        self.host.draw_capture_overlays(
            &mut logical,
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
        );
        if logical.encoding().is_empty() {
            return false;
        }
        self.compose_layout_scene(&logical, scene);
        true
    }

    pub fn take_closed_capture_mirrors(&mut self) -> Vec<String> {
        self.host.take_closed_capture_mirrors()
    }

    /// Replace desktop chrome with the trusted first-login identity ceremony.
    pub fn enable_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.host.enable_onboarding(identity_dir);
        self.rebuild_layout_tree();
    }

    #[cfg(test)]
    pub(crate) fn enable_test_onboarding(&mut self, view: crate::onboarding_view::OnboardingView) {
        self.host.enable_test_onboarding(view);
        self.rebuild_layout_tree();
    }

    /// Replace desktop chrome with the trusted identity-recovery ceremony.
    pub fn enable_recovery_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.host.enable_recovery_onboarding(identity_dir);
        self.rebuild_layout_tree();
    }

    /// Replace desktop chrome with the reduced host-install identity OOBE.
    pub fn enable_host_desktop_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.host.enable_host_desktop_onboarding(identity_dir);
        self.rebuild_layout_tree();
    }

    /// Replace desktop chrome with host-install identity recovery.
    pub fn enable_host_desktop_recovery_onboarding(&mut self, identity_dir: std::path::PathBuf) {
        self.host
            .enable_host_desktop_recovery_onboarding(identity_dir);
        self.rebuild_layout_tree();
    }

    /// On-screen rectangle of the tile currently painting the foreign
    /// wayland surface `surface_id`, or `None` if no visible tile hosts it.
    pub fn foreign_surface_rect(&mut self, surface_id: u64) -> Option<vello::kurbo::Rect> {
        self.host.foreign_surface_rect(surface_id)
    }

    /// Resolve the visible foreign surface owned by an authenticated app-id.
    /// The live widget rect is authoritative; persisted tree geometry can be a
    /// frame behind during strip motion. If several instances exist, prefer
    /// the first currently visible active tab rather than an off-screen/stale
    /// sibling.
    pub fn foreign_surface_for_app(&mut self, app_id: &str) -> Option<(u64, vello::kurbo::Rect)> {
        let surface_ids: Vec<u64> = {
            let tree = self.chrome.borrow();
            tree.tree()
                .desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(|tab| match &tab.tile {
                    crate::shell::state::TileContent::Foreign {
                        app_id: candidate,
                        surface_id,
                    } if candidate == app_id => Some(*surface_id),
                    _ => None,
                })
                .collect()
        };
        for surface_id in surface_ids {
            if let Some(rect) = self.host.foreign_surface_rect(surface_id) {
                return Some((surface_id, rect));
            }
        }
        None
    }

    pub(crate) fn foreign_surfaces_for_app_room(
        &mut self,
        app_id: &str,
        room: &str,
    ) -> Vec<(u64, vello::kurbo::Rect)> {
        let candidates = {
            let tree = self.chrome.borrow();
            tree.tree()
                .desktop
                .workspaces
                .iter()
                .filter(|workspace| workspace.room().to_string() == room)
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(|tab| match &tab.tile {
                    crate::shell::state::TileContent::Foreign {
                        app_id: candidate,
                        surface_id,
                    } if candidate == app_id => Some(*surface_id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        candidates
            .into_iter()
            .filter_map(|surface| {
                self.host
                    .foreign_surface_rect(surface)
                    .map(|rect| (surface, rect))
            })
            .collect()
    }

    /// Install/remove the trusted keyboard mirror and anchor it to the output
    /// containing the currently focused field.
    pub fn set_osk_surface(
        &mut self,
        surface_id: Option<u64>,
        focused_surface: Option<u64>,
        shell_field: Option<ui::ime::ImeWindowState>,
        active: bool,
    ) {
        let anchor = focused_surface
            .and_then(|id| self.host.foreign_surface_rect(id))
            .map(|rect| rect.center())
            .or_else(|| {
                shell_field.map(|field| {
                    vello::kurbo::Point::new(
                        f64::from(field.x) + f64::from(field.width) / 2.0,
                        f64::from(field.y) + f64::from(field.height) / 2.0,
                    )
                })
            });
        self.osk_serves_shell = shell_field.is_some();
        self.osk_anchor = anchor;
        self.osk_active = active;
        match surface_id {
            Some(id)
                if self
                    .osk_overlay
                    .as_ref()
                    .map(ForeignSurfaceWidget::surface_id)
                    != Some(id) =>
            {
                self.osk_overlay = Some(ForeignSurfaceWidget::new(
                    id,
                    crate::shell::floating::OSK_APP_ID.to_string(),
                ));
            }
            None => self.osk_overlay = None,
            _ => {}
        }
        self.layout_osk_overlay();
        let cover = self.osk_overlay.as_ref().filter(|_| active);
        self.host.set_keyboard_cover(cover.map(Widget::get_rect));
    }

    /// Reconcile exact authenticated interactive bounds separately from the
    /// transparent native allocation that contains the surface's shadow.
    pub fn set_floating_surfaces_with_input(
        &mut self,
        surfaces: &[(u64, String, Rect, Option<Rect>)],
    ) {
        self.floating_overlays
            .retain(|(surface_id, _)| surfaces.iter().any(|(id, _, _, _)| id == surface_id));
        for (surface_id, app_id, rect, input_rect) in surfaces {
            let index = self
                .floating_overlays
                .iter()
                .position(|(id, _)| id == surface_id)
                .unwrap_or_else(|| {
                    self.floating_overlays.push((
                        *surface_id,
                        ForeignSurfaceWidget::new_floating(*surface_id, app_id.clone()),
                    ));
                    self.floating_overlays.len().saturating_sub(1)
                });
            if let Some((_, surface)) = self.floating_overlays.get_mut(index) {
                surface.set_floating_input_rect(*input_rect);
                surface.place_floating(*rect);
            }
        }
    }

    pub fn set_fullscreen_surface(&mut self, surface: Option<(u64, String, Rect)>) {
        self.fullscreen_overlay = surface.map(|(surface_id, app_id, rect)| {
            let mut widget = self
                .fullscreen_overlay
                .take()
                .filter(|(id, _)| *id == surface_id)
                .map_or_else(
                    || ForeignSurfaceWidget::new_floating(surface_id, app_id),
                    |(_, widget)| widget,
                );
            widget.place_floating(rect);
            (surface_id, widget)
        });
    }

    /// Dim the whole desktop beneath a system-modal prompt.
    fn draw_system_modal_scrim(&self, scene: &mut Scene) {
        if !self.system_modal_scrim {
            return;
        }
        let bounds = Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1);
        ui::overlay::scrim::modal(scene, Affine::IDENTITY, bounds, 1.0, &self.theme);
    }

    /// Show or hide the scrim a system-modal prompt dims the desktop with.
    pub fn set_system_modal_scrim(&mut self, shown: bool) {
        self.system_modal_scrim = shown;
    }

    /// Whether the desktop is dimmed under a system-modal prompt.
    pub fn system_modal_scrim(&self) -> bool {
        self.system_modal_scrim
    }

    pub fn fullscreen_surface_rect(&self) -> Option<(u64, Rect)> {
        self.fullscreen_overlay
            .as_ref()
            .filter(|(_, surface)| surface.is_mapped())
            .map(|(id, surface)| (*id, surface.get_rect()))
    }

    pub fn fullscreen_surface_hit(&self, point: (f64, f64)) -> Option<(u64, Rect)> {
        let (surface_id, surface) = self.fullscreen_overlay.as_ref()?;
        let rect = surface.get_rect();
        (surface.is_mapped() && rect.contains(Point::new(point.0, point.1)))
            .then_some((*surface_id, rect))
    }

    /// Reconcile popup/candidate surfaces in compositor z-order.  Widgets are
    /// retained by id, preserving their mapped frame between reactive
    /// position changes while a parent moves across outputs.
    pub fn set_transient_surfaces(&mut self, surfaces: &[(u64, Rect)]) {
        self.transient_overlays
            .retain(|(surface_id, _)| surfaces.iter().any(|(id, _)| id == surface_id));
        for (surface_id, rect) in surfaces {
            let index = self
                .transient_overlays
                .iter()
                .position(|(id, _)| id == surface_id)
                .unwrap_or_else(|| {
                    self.transient_overlays.push((
                        *surface_id,
                        ForeignSurfaceWidget::new_floating(*surface_id, "so.tilo.transient".into()),
                    ));
                    self.transient_overlays.len().saturating_sub(1)
                });
            if let Some((_, surface)) = self.transient_overlays.get_mut(index) {
                surface.place_floating(*rect);
            }
        }
    }

    /// Reconcile frozen transfer frames from both capability-authorised
    /// (window not created yet) and mapped-but-bufferless native surfaces.
    /// Existing widgets retain their image allocation while only geometry
    /// changes under a live pointer drag.
    pub fn set_floating_transfer_surfaces(
        &mut self,
        pending: &[crate::shell::floating::PendingFloatingToplevel],
        live: &[crate::shell::floating::FloatingToplevel],
    ) {
        let presentation_rect =
            |rect: Rect, source: Option<crate::shell::floating::FloatingTransferSource>| {
                source.map_or(rect, |source| source.rect)
            };
        let mut wanted = Vec::new();
        for surface in pending
            .iter()
            .filter(|surface| !surface.suspended || surface.transfer_source.is_some())
        {
            if surface.transfer_frame.is_some() {
                wanted.push((
                    format!("{}:{}", surface.connection, surface.public_id),
                    presentation_rect(surface.rect, surface.transfer_source),
                    surface.transfer_frame.as_ref(),
                ));
            }
        }
        for surface in live.iter().filter(|surface| {
            let native_pixels_ready = self
                .floating_overlays
                .iter()
                .find(|(surface_id, _)| *surface_id == surface.surface_id)
                .is_some_and(|(_, overlay)| overlay.is_mapped());
            floating_transfer_proxy_needed(
                surface.suspended,
                surface.presented,
                surface.transfer_source.is_some(),
                native_pixels_ready,
            )
        }) {
            if surface.transfer_frame.is_some() {
                wanted.push((
                    format!("{}:{}", surface.connection, surface.public_id),
                    presentation_rect(surface.rect, surface.transfer_source),
                    surface.transfer_frame.as_ref(),
                ));
            }
        }
        self.floating_transfer_proxies
            .retain(|proxy| wanted.iter().any(|(key, _, _)| key == &proxy.key));
        for (key, rect, frame) in wanted {
            if let Some(proxy) = self
                .floating_transfer_proxies
                .iter_mut()
                .find(|proxy| proxy.key == key)
            {
                proxy.rect = rect;
            } else if let Some(frame) = frame {
                self.floating_transfer_proxies
                    .push(FloatingTransferProxy::new(key, rect, frame));
            }
        }
    }

    /// Top-most mapped app-owned activity under `point`.
    pub fn floating_surface_hit(&self, point: (f64, f64)) -> Option<(u64, Rect)> {
        let point = Point::new(point.0, point.1);
        self.floating_overlays
            .iter()
            .rev()
            .find_map(|(surface_id, surface)| {
                let rect = surface.get_rect();
                (surface.is_mapped()
                    && rect.contains(point)
                    && surface.floating_input_contains(point))
                .then_some((*surface_id, rect))
            })
    }

    /// Top-most mapped transient at `point`, above normal floating windows.
    pub fn transient_surface_hit(&self, point: (f64, f64)) -> Option<(u64, Rect)> {
        let point = Point::new(point.0, point.1);
        self.transient_overlays
            .iter()
            .rev()
            .find_map(|(surface_id, surface)| {
                let rect = surface.get_rect();
                (surface.is_mapped() && rect.contains(point)).then_some((*surface_id, rect))
            })
    }

    pub fn transient_surface_rect(&self, surface_id: u64) -> Option<Rect> {
        self.transient_overlays
            .iter()
            .find(|(id, surface)| *id == surface_id && surface.is_mapped())
            .map(|(_, surface)| surface.get_rect())
    }

    pub fn transient_surface_rects(&self) -> Vec<(u64, Rect)> {
        self.transient_overlays
            .iter()
            .filter(|(_, surface)| surface.is_mapped())
            .map(|(id, surface)| (*id, surface.get_rect()))
            .collect()
    }

    pub fn floating_surface_rect(&self, surface_id: u64) -> Option<Rect> {
        self.floating_overlays
            .iter()
            .find(|(id, surface)| *id == surface_id && surface.is_mapped())
            .map(|(_, surface)| surface.get_rect())
    }

    pub fn floating_surface_input_rects(&self) -> Vec<(u64, Rect)> {
        self.floating_overlays
            .iter()
            .filter(|(_, surface)| surface.is_mapped())
            .filter_map(|(id, surface)| surface.floating_input_rect().map(|rect| (*id, rect)))
            .collect()
    }

    pub fn floating_surface_rects(&self) -> Vec<(u64, Rect)> {
        self.floating_overlays
            .iter()
            .filter(|(_, surface)| surface.is_mapped())
            .map(|(id, surface)| (*id, surface.get_rect()))
            .collect()
    }

    /// Hit-test the mapped keyboard. An unmapped/hidden surface never steals
    /// clicks from the application beneath it.
    pub fn osk_surface_hit(&self, point: (f64, f64)) -> Option<(u64, Rect)> {
        let keyboard = self.osk_overlay.as_ref()?;
        let rect = keyboard.get_rect();
        (self.osk_active && keyboard.is_mapped() && rect.contains(Point::new(point.0, point.1)))
            .then_some((keyboard.surface_id(), rect))
    }

    /// Record how many commits the on-screen keyboard has made.
    pub fn set_osk_commits(&mut self, commits: u64) {
        self.osk_commits = commits;
    }

    /// Record where the lock screen's keyboard keys sit (empty: hidden).
    pub fn set_lock_keyboard_keys(&mut self, keys: Vec<(String, Rect)>) {
        self.lock_keyboard_keys = keys;
    }

    /// The lock screen's keyboard keys by name, while it shows.
    pub fn lock_keyboard_keys(&self) -> &[(String, Rect)] {
        &self.lock_keyboard_keys
    }

    /// Record the lock screen's surfaces as drawn (empty: unlocked).
    pub fn set_lock_surfaces(
        &mut self,
        surfaces: Vec<crate::compositor::lock_screen::LockSurface>,
    ) {
        self.lock_surfaces = surfaces;
    }

    /// The lock screen's surfaces, one per display, while it shows.
    pub fn lock_surfaces(&self) -> &[crate::compositor::lock_screen::LockSurface] {
        &self.lock_surfaces
    }

    /// Whether the keyboard serves one of the shell's own fields rather
    /// than an application's.
    pub fn osk_serves_shell(&self) -> bool {
        self.osk_serves_shell
    }

    /// How many commits the on-screen keyboard has made this session.
    pub fn osk_commits(&self) -> u64 {
        self.osk_commits
    }

    /// Whether the trusted on-screen keyboard service has a surface, shown
    /// or not.
    /// Whether field activation and the input modality request the keyboard.
    pub fn osk_requested(&self) -> bool {
        self.osk_active
    }

    /// Whether the keyboard has supplied a buffer, even while hidden.
    pub fn osk_mapped(&self) -> bool {
        self.osk_overlay
            .as_ref()
            .is_some_and(ForeignSurfaceWidget::is_mapped)
    }

    pub fn osk_connected(&self) -> bool {
        self.osk_overlay.is_some()
    }

    /// The compositor-owned keyboard's painted rectangle, if it is currently
    /// visible. Native output membership uses this rather than the focused
    /// app's rectangle: the keyboard is its own Wayland surface and may move
    /// to a different output as focus changes.
    pub fn osk_surface_rect(&self) -> Option<(u64, Rect)> {
        let keyboard = self.osk_overlay.as_ref()?;
        (self.osk_active && keyboard.is_mapped())
            .then_some((keyboard.surface_id(), keyboard.get_rect()))
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn viewport(&self) -> (f64, f64) {
        self.viewport
    }

    pub fn capture_pointer_position(&self) -> Option<vello::kurbo::Point> {
        self.host.pointer_position()
    }

    /// The pointer image the compositor is showing, for captures.
    pub fn pointer_image(&self) -> &crate::compositor::cursor::PointerImage {
        &self.pointer_image
    }

    pub fn set_pointer_image(&mut self, image: crate::compositor::cursor::PointerImage) {
        self.pointer_image = image;
    }

    /// The cursor the chrome under `point` asks for: the smallest widget
    /// containing the point that names one wins, so a nested button beats
    /// the bar around it — the same rule `ui::App` applies to app windows.
    pub fn chrome_cursor_at(
        &self,
        point: vello::kurbo::Point,
    ) -> Option<winit::window::CursorIcon> {
        let mut best: Option<(f64, winit::window::CursorIcon)> = None;
        chrome_cursor_walk(self.host.as_ref(), point, &mut best);
        best.map(|(_, icon)| icon)
    }

    /// What the focused chrome widget asks of an input method — its purpose
    /// and caret — when it takes text; `None` otherwise.
    pub fn focused_ime_field(&self, focus: Option<NodeId>) -> Option<ui::ime::ImeWindowState> {
        if self.host.overlay_owns_ime() {
            return self.host.overlay_ime_field();
        }
        let widget = *self.node_map.get(&focus?)?;
        // SAFETY: `node_map` is rebuilt from widgets owned by `host`; the
        // pointer stays valid until the next structural rebuild, the same
        // invariant every focused-node dispatch here relies on.
        ui::ime::ImeWindowState::from_widget(unsafe { &*widget })
    }

    pub(crate) fn focused_ime_requires_deferred_ack(&self, focus: Option<NodeId>) -> bool {
        if self.host.overlay_owns_ime() {
            return false;
        }
        let Some(widget) = focus.and_then(|focus| self.node_map.get(&focus)) else {
            return false;
        };
        // SAFETY: the same live host/pointer map and read lifetime as focused_ime_field.
        unsafe { &**widget }.ime_requires_deferred_ack()
    }

    /// The focused field takes text only through deferred receipts.
    pub(crate) fn focused_ime_refuses_ordinary_commits(&self, focus: Option<NodeId>) -> bool {
        let widget = focus.and_then(|focus| self.node_map.get(&focus));
        // SAFETY: the same live host/pointer map and read lifetime as focused_ime_field.
        !self.host.overlay_owns_ime()
            && widget.is_some_and(|widget| unsafe { &**widget }.ime_refuses_ordinary_commits())
    }

    pub(crate) fn start_deferred_text(
        &mut self,
        focus: Option<NodeId>,
        document: &mut automerge::AutoCommit,
        root: &automerge::ObjId,
    ) -> Option<ui::webview::text_input::Receipt> {
        if self.host.overlay_owns_ime() {
            return None;
        }
        let focus = focus?;
        let mut admitted = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: exclusive frame access owns the live host and pointer map;
        // the callback only starts a private transaction without structural mutation.
        unsafe {
            admitted.rebuild(
                self.host.as_ref(),
                &self.taffy,
                self.host_node?,
                &self.node_map,
            );
            ui::bound_dispatch::invoke_bound_node(
                &admitted,
                &self.node_map,
                focus,
                document,
                root,
                |widget, _, _| widget.start_deferred_text(),
            )
            .ok()
            .flatten()
        }
    }

    /// Admit the original focused owner through the same bound map as native
    /// dispatch. Only deferred fields consume this path; ordinary fields keep IME.
    pub(crate) fn commit_deferred_text(
        &mut self,
        focus: Option<NodeId>,
        text: &str,
        document: &mut automerge::AutoCommit,
        root: &automerge::ObjId,
    ) -> Option<ui::webview::text_input::Receipt> {
        if self.host.overlay_owns_ime() {
            return None;
        }
        let focus = focus?;
        let mut admitted = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: exclusive frame access owns the live host and pointer map;
        // no structural mutation occurs while this cache-only callback runs.
        unsafe {
            admitted.rebuild(
                self.host.as_ref(),
                &self.taffy,
                self.host_node?,
                &self.node_map,
            );
            ui::bound_dispatch::invoke_bound_node(
                &admitted,
                &self.node_map,
                focus,
                document,
                root,
                |widget, _, _| widget.commit_deferred_text(text),
            )
            .ok()
            .flatten()
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn focused_ime_edit_stamp(
        &self,
        focus: Option<NodeId>,
        document: &automerge::AutoCommit,
    ) -> Option<crate::compositor::shell_text_input::NativeEditStamp> {
        if self.host.overlay_owns_ime() {
            return Some(crate::compositor::shell_text_input::NativeEditStamp {
                document: ui::bound_dispatch::DocumentEditStamp::for_document(document),
                fingerprint: self
                    .host
                    .overlay_ime_edit_fingerprint(&self.ime_stamp_key)?,
            });
        }
        let focus = focus?;
        let widget = *self.node_map.get(&focus)?;
        let mut admitted = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: the host, layout and pointer map are the same exclusively
        // owned live build used by focused_ime_field; nothing mutates here.
        unsafe {
            admitted.rebuild(
                self.host.as_ref(),
                &self.taffy,
                self.host_node?,
                &self.node_map,
            );
        }
        Some(crate::compositor::shell_text_input::NativeEditStamp {
            document: admitted.document_edit_stamp(focus, document).ok()?,
            // SAFETY: the live map's pointer remains owned by the host during
            // this read; no raw field value escapes the keyed fingerprint.
            fingerprint: unsafe { &*widget }.ime_edit_fingerprint(&self.ime_stamp_key)?,
        })
    }

    /// Whether a chrome widget under `point` handles a touch contact that
    /// starts there itself, rather than through the pointer events a touch
    /// is otherwise turned into. Mirrors `ui::App` for application windows.
    pub fn claims_raw_touch_at(&self, point: vello::kurbo::Point) -> bool {
        claims_raw_touch_walk(self.host.as_ref(), point)
    }

    /// Inner node id (host's root child). Required by the input dispatch
    /// path in `bin/shots`'s `ShellDriver`; the compositor doesn't need this.
    /// `None` only in the narrow window where `rebuild_layout_tree` just
    /// failed to build a tree (see its doc).
    pub fn host_node(&self) -> Option<NodeId> {
        self.host_node
    }

    /// Build the semantic tree for compositor-owned chrome.
    ///
    /// Hosted applications get this through [`ui::App`]'s winit adapter. The
    /// native shell has no application window, so its AT-SPI bridge calls this
    /// equivalent projection directly after layout. Keeping the projection in
    /// [`ui::accesskit_bridge`] means first-use surfaces expose the same roles,
    /// names, state and stable node ids as every other Tilo UI.
    pub fn build_a11y_tree_update(
        &self,
        focus: Option<NodeId>,
        scale: f64,
    ) -> ui::accesskit::TreeUpdate {
        // A modal sheet takes every key, so the reader's cursor belongs on
        // the part of it that has the keyboard: the host is the focused
        // widget, and the sheet's focused node its active descendant.
        let focus = if self.host.modal_sheet_owns_keyboard() {
            self.host_node.or(focus)
        } else {
            focus
        };
        ui::accesskit_bridge::build_tree_update(self.host.as_ref(), &self.node_map, focus, scale)
    }

    /// Route an assistive-technology action back through the live compositor
    /// widget tree. Returns `true` only when the request addressed a current
    /// semantic node and was handled (directly or through the standard pointer
    /// fallback used by [`ui::App`]).
    pub fn dispatch_a11y_action(
        &mut self,
        target: ui::accesskit::NodeId,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        root_obj: &ObjId,
        focus: &mut Option<NodeId>,
    ) -> bool {
        use ui::a11y::Action;
        use ui::accesskit_bridge::{A11yTarget, resolve_node_id};

        let Some(resolved) = resolve_node_id(self.host.as_ref(), &self.node_map, target) else {
            return false;
        };
        let (taffy_id, key_path, ordinal_path, semantic_target, synthetic) = match resolved {
            A11yTarget::Root => return false,
            A11yTarget::Widget(id) => (id, Vec::new(), Vec::new(), None, false),
            A11yTarget::Synthetic {
                owner,
                key_path,
                ordinal_path,
                semantic_target,
            } => (owner, key_path, ordinal_path, semantic_target, true),
        };
        let Some(&widget) = self.node_map.get(&taffy_id) else {
            return false;
        };

        let handled = unsafe {
            if let Some(target) = semantic_target.as_deref() {
                (*widget).perform_semantic_action(target, action, state, root_obj)
            } else if synthetic {
                (*widget).perform_action(action, &key_path, state, root_obj)
            } else {
                (*widget).perform_action(action, &[], state, root_obj)
            }
        };
        if handled {
            // Semantic input bypasses handle_event's tab writeback. Accept it
            // before the next model sync can restore the previous selection.
            self.host.flush_active_tab_writeback();
            let activated_tab_bar = matches!(action, Action::Activate)
                && semantic_target
                    .as_deref()
                    .is_some_and(|target| target.starts_with("tab:"))
                && unsafe { (*widget).accessibility() }
                    .is_some_and(|node| node.role == ui::a11y::Role::TabBar);
            if matches!(action, Action::Focus) || activated_tab_bar {
                self.set_native_focus(focus, Some(taffy_id));
            }
            if activated_tab_bar && self.chrome().axis_navigation_enabled() {
                self.chrome.borrow_mut().request_axis_gather();
            }
            if self.host.take_focus_reset() {
                self.relayout();
                self.focus_first(focus);
            }
            return true;
        }

        match action {
            Action::Focus => {
                if synthetic && !ordinal_path.is_empty() {
                    unsafe { (*widget).focus_synthetic_accessibility(&ordinal_path) };
                }
                self.set_native_focus(focus, Some(taffy_id));
                true
            }
            Action::Activate | Action::ToggleCheck => {
                let rect = if synthetic {
                    unsafe { (*widget).synthetic_accessibility_rect(&ordinal_path) }
                        .unwrap_or_else(|| unsafe { (*widget).get_rect() })
                } else {
                    unsafe { (*widget).get_rect() }
                };
                let center = rect.center();
                let events = [
                    WindowEvent::CursorMoved {
                        device_id: winit::event::DeviceId::dummy(),
                        position: winit::dpi::PhysicalPosition::new(center.x, center.y),
                    },
                    WindowEvent::MouseInput {
                        device_id: winit::event::DeviceId::dummy(),
                        state: winit::event::ElementState::Pressed,
                        button: winit::event::MouseButton::Left,
                    },
                    WindowEvent::MouseInput {
                        device_id: winit::event::DeviceId::dummy(),
                        state: winit::event::ElementState::Released,
                        button: winit::event::MouseButton::Left,
                    },
                ];
                let mut handled = false;
                for event in events {
                    handled |=
                        self.dispatch_event(&event, &Modifiers::default(), state, root_obj, focus);
                }
                handled
            }
            _ => false,
        }
    }

    /// Read-only access to the taffy tree — also for input dispatch.
    pub fn taffy(&self) -> &TaffyTree<()> {
        &self.taffy
    }

    /// Mutable access to the text renderer, for the input dispatch /
    /// scenario step paths in `bin/shots`'s `ShellDriver`.
    pub fn text_renderer_mut(&mut self) -> &mut TextRenderer {
        &mut self.text_renderer
    }

    /// Resize the scene the backend presents, triggering a structural
    /// rebuild on the next `relayout` (the wrapper root node carries an
    /// explicit extent). Without display geometry the logical viewport is
    /// this same size; with it, [`Self::set_output_geometry`] owns the
    /// logical viewport. Used by the compositor's
    /// `BackendEvent::OutputResized` handler.
    pub fn set_viewport(&mut self, viewport: (u32, u32)) {
        let size = (f64::from(viewport.0), f64::from(viewport.1));
        self.scene_size = size;
        if self.outputs.is_none() {
            self.viewport = size;
        }
        self.rebuild_layout_tree();
    }

    /// Split the host out from the taffy tree as disjoint borrows —
    /// `ShellOverlayHost::handle_event` needs `&mut self` *plus* `&TaffyTree`,
    /// both inside `FrameBuilder`, which accessor methods alone can't satisfy.
    pub fn host_and_taffy_mut(&mut self) -> (&mut ShellOverlayHost, &TaffyTree<()>) {
        (&mut self.host, &self.taffy)
    }

    /// Send Named Space/Enter to the hosted Terminal before chrome Activate.
    fn offer_terminal_typing(
        &self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        state: &mut ui::widget::Edits<'_>,
        root_obj: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        if !matches!(
            event,
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == winit::event::ElementState::Pressed
                    && matches!(
                        key.logical_key,
                        Key::Named(NamedKey::Enter | NamedKey::Space)
                    )
                    // Local consent owns these keys before the underlying
                    // process can edit or submit its original shell line.
                    && !self.host.dictation_owns_key(&key.logical_key)
        ) || !self.active_tile_is_terminal()
            // A window's search field holding the keyboard takes its Enter.
            || self.host.window_search_has_keyboard()
        {
            return false;
        }
        // Super+Shift+Enter is DetachTab (W08). Super+Space is search.
        // Only unmodified Space/Enter belong on the live Terminal first.
        let mods = modifiers.state();
        if mods.super_key() || mods.shift_key() {
            return false;
        }
        let Some(id) = self.active_terminal_node() else {
            return false;
        };
        let Some(&widget) = self.node_map.get(&id) else {
            return false;
        };
        let mut handled = false;
        let taffy = self.taffy();
        unsafe {
            (*widget).handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers,
                    state: state.reborrow(),
                    path: root_obj,
                    handled: &mut handled,
                    focus_target,
                    node_id: id,
                    taffy,
                },
            );
        }
        handled
    }

    /// Dispatch a single `WindowEvent` through the host's `handle_event`
    /// pipeline. Returns whether it was consumed.
    ///
    /// The compositor's `Modifiers`, `AutoCommit` doc, `ObjId` and focus
    /// cursor live outside the `FrameBuilder` — each consumer owns its own —
    /// passed in rather than stored, so the `FrameBuilder` stays a pure
    /// layout/paint core shared by both. Both `ShellDriver::dispatch` and
    /// the compositor's `pump_test_control_conn` route through this one method.
    pub fn dispatch_event(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
        state: &mut ui::widget::Edits<'_>,
        root_obj: &ObjId,
        focus: &mut Option<NodeId>,
    ) -> bool {
        self.reconcile_native_focus(focus);

        let Some(child) = self.host_node() else {
            // No layout tree yet (see `host_node`'s doc) — nothing to
            // dispatch into.
            return false;
        };
        let mut handled = false;
        // Snapshot the focus going in to detect any new target set by the
        // host's `handle_event` — mirrors `ui::App::window_event`.
        let prior_focus = *focus;
        let overlays_before = (
            self.host.palette_visible(),
            self.host.agent_quick_composer_open(),
            self.host.person_quick_composer_open(),
        );
        let mut focus_target: Option<NodeId> = None;
        if self.offer_terminal_typing(event, modifiers, state, root_obj, &mut focus_target) {
            if let Some(new_id) = focus_target
                && prior_focus != Some(new_id)
            {
                self.set_native_focus(focus, Some(new_id));
            }
            return true;
        }
        {
            let (host, taffy) = self.host_and_taffy_mut();
            host.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers,
                    state: state.reborrow(),
                    path: root_obj,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: child,
                    taffy,
                },
            );
        }
        // Apply the requested focus change: drop old widget's focus, raise
        // new widget's. Without this, clicking a focusable widget would set
        // `focus_target` but the widget itself would never see
        // `set_focus(true)`, leaving rendering in the unfocused state.
        if let Some(new_id) = focus_target
            && prior_focus != Some(new_id)
        {
            self.set_native_focus(focus, Some(new_id));
        }

        // Wizard actions can replace the entire subtree during this event.
        // Rebuild and expose a visible first focus immediately, rather than
        // waiting for a second physical key press to repair focus.
        if self.host.take_focus_reset() {
            self.relayout();
            self.focus_first(focus);
        } else if self.host.palette_visible()
            || self.host.agent_quick_composer_open()
            || self.host.person_quick_composer_open()
            || self.palette_focus_active
        {
            // Visibility alone is not a layout change. Pointer motion and key
            // releases used to rebuild admission and solve the entire desktop
            // while search was open. Still settle structural edits, geometry,
            // opening/closing and native focus synchronously before returning.
            let structure_changed = self.host.take_structure_dirty();
            if structure_changed {
                self.rebuild_layout_tree();
            }
            let overlays_after = (
                self.host.palette_visible(),
                self.host.agent_quick_composer_open(),
                self.host.person_quick_composer_open(),
            );
            let geometry_dirty = self
                .root_node
                .is_none_or(|node| self.taffy.dirty(node).unwrap_or(true));
            if structure_changed || geometry_dirty || overlays_before != overlays_after {
                self.relayout();
            }
            self.reconcile_native_focus(focus);
        }
        // Native/DRM chrome does not run through `ui::App`, whose event loop
        // normally turns Enter/Space on the focused widget into an
        // accessibility Activate action. Space/Enter for a live Terminal
        // were already offered above; activate only when they were unhandled.
        if !handled
            && let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == winit::event::ElementState::Pressed
            && matches!(
                event.logical_key,
                Key::Named(NamedKey::Enter | NamedKey::Space)
            )
            && !modifiers.state().super_key()
            && !modifiers.state().shift_key()
            && let Some(focused) = *focus
            && let Some(widget) = self.node_map.get(&focused).copied()
        {
            // SAFETY: `node_map` is rebuilt from widgets owned by `host`; the
            // pointer remains valid until the next structural rebuild. This is
            // the same focused-node dispatch invariant used by `ui::App`.
            let activated = unsafe {
                (*widget).perform_action(&ui::a11y::Action::Activate, &[], state, root_obj)
            };
            if activated {
                handled = true;
            }
        }
        if !handled && self.apply_hosted_tab_chord(event, *modifiers) {
            return true;
        }
        // Match `ui::App`: unmodified Tab advances the global DFS focus order
        // only when the focused widget did not claim it. A terminal and a
        // multiline editor legitimately consume Tab as input; intercepting it
        // before dispatch made completion/indentation impossible. Ctrl+Tab is
        // the desktop's tab chord above, also only when nothing claimed it.
        if !handled
            && let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == winit::event::ElementState::Pressed
            && matches!(event.logical_key, Key::Named(NamedKey::Tab))
            && !modifiers.state().control_key()
            && !modifiers.state().alt_key()
            && !modifiers.state().super_key()
        {
            return self.focus_tab(focus, modifiers.state().shift_key());
        }
        handled
    }

    /// Snapshot of every addressable widget in the live tree, keyed by
    /// either its opt-in `test_id` or a stable path (`Parent/Child[index]`).
    /// Lifted into `FrameBuilder` so the out-of-process control protocol can
    /// reach the same data without going through the shots binary.
    ///
    /// Walked fresh on every call — no hidden cache. Callers needing the
    /// registry across multiple actions should re-fetch after each frame's
    /// `paint`/`relayout`.
    pub fn widget_registry(&self) -> HashMap<String, vello::kurbo::Rect> {
        let mut out = HashMap::new();
        let mut sibling_counts: HashMap<String, usize> = HashMap::new();
        walk_widget(self.host.as_ref(), "", &mut sibling_counts, &mut out);
        self.register_chrome_rects(&mut out);
        out
    }

    /// Register the chrome affordances painted by the overlay host rather
    /// than a child widget (overview's add-workspace tile, workspace cards,
    /// settings tab pills, Permissions tab's Revoke buttons, connect
    /// prompt's buttons), so they're clickable by name. These are drawn
    /// straight into the scene and hit-tested from cached rects, so a tree
    /// walk can't see them.
    ///
    /// Zero-area rects are skipped, so a name means "on screen now" rather
    /// than clickable at the origin while hidden.
    fn register_chrome_rects(&self, out: &mut HashMap<String, vello::kurbo::Rect>) {
        use crate::shell::chrome_state::WsTab;
        let host = &self.host;
        let mut put = |name: String, rect: vello::kurbo::Rect| {
            if rect.area() > 0.0 {
                out.insert(name, rect);
            }
        };
        put("add_workspace".into(), host.add_workspace_rect_for_tests());
        launchpad_targets(host.as_ref(), &mut put);
        for (key, tab) in [
            ("general", WsTab::General),
            ("members", WsTab::Members),
            ("permissions", WsTab::Permissions),
            ("apps", WsTab::Apps),
        ] {
            put(
                format!("ws_settings_tab[{key}]"),
                host.ws_settings_tab_pill_rect(tab),
            );
        }
        put(
            "ws_settings_name_input".into(),
            host.ws_settings_name_rect_for_tests(),
        );
        for (idx, rect) in host.overview_row_rects_for_tests().into_iter().enumerate() {
            put(format!("overview_card[{idx}]"), rect);
        }
        // Revoke buttons are addressable both by position (stable for "the
        // first one") and by the capability id they revoke (stable across
        // reordering).
        for (index, (cap_id, rect)) in host.permissions_revoke_rects().iter().enumerate() {
            put(format!("permissions_revoke[{index}]"), *rect);
            put(format!("permissions_revoke[{cap_id}]"), *rect);
        }
        if let Some(composer) = host.grant_composer_rects() {
            put("grant_subject".into(), composer.subject);
            put("grant_capability".into(), composer.capability);
            put("grant_submit".into(), composer.submit);
            for (key, rect) in [
                ("always", composer.expiry[0]),
                ("session", composer.expiry[1]),
                ("once", composer.expiry[2]),
            ] {
                put(format!("grant_expiry[{key}]"), rect);
            }
        }
        for (index, (user, role_rect, remove_rect)) in
            host.member_control_rects().iter().enumerate()
        {
            put(format!("member_role[{index}]"), *role_rect);
            put(format!("member_role[{user}]"), *role_rect);
            put(format!("member_remove[{index}]"), *remove_rect);
            put(format!("member_remove[{user}]"), *remove_rect);
        }
        // The invite, join and pairing sheet's controls, by their own names.
        for (name, rect) in host.peer_sync_prompt_rects_for_tests() {
            put(name, rect);
        }
        // The collaborator dropdown's own controls, named so a scenario
        // clicks the button a person clicks rather than invoking the action
        // behind it — a click proves the control is wired, an invocation
        // only proves the action.
        put(
            "collab_invite".into(),
            host.collaborators_invite_rect_for_tests(),
        );
        for (name, rect) in host.quick_settings_semantic_rects() {
            put(name, rect);
        }
        for (name, rect) in host.power_menu_semantic_rects() {
            put(name.into(), rect);
        }
        for (name, rect) in host.trusted_frame_targets(&self.theme) {
            put(name.to_string(), rect);
        }
        #[cfg(any(test, debug_assertions))]
        for (name, rect) in host.permission_prompt_choice_rects_for_tests() {
            put(name.to_string(), rect);
        }
    }
}

/// The first-use launchpad's cards, which it paints and hit-tests itself.
fn launchpad_targets(
    widget: &dyn ui::widget::Widget,
    put: &mut impl FnMut(String, vello::kurbo::Rect),
) {
    if let Some(launchpad) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<tilo_desktop::launchpad::CollaborationLaunchpad>())
    {
        for (name, rect) in launchpad.targets() {
            put(name, rect);
        }
        return;
    }
    for child in widget.children() {
        launchpad_targets(child, put);
    }
}

fn claims_raw_touch_walk(widget: &dyn ui::widget::Widget, point: vello::kurbo::Point) -> bool {
    (widget.get_rect().contains(point) && widget.claims_raw_touch(point))
        || widget
            .children()
            .into_iter()
            .any(|child| claims_raw_touch_walk(child, point))
}

fn chrome_cursor_walk(
    widget: &dyn ui::widget::Widget,
    point: vello::kurbo::Point,
    best: &mut Option<(f64, winit::window::CursorIcon)>,
) {
    let rect = widget.get_rect();
    if rect.contains(point)
        && let Some(icon) = widget.cursor_shape(point)
    {
        let area = rect.area().max(0.0);
        if best.is_none_or(|(smallest, _)| area < smallest) {
            *best = Some((area, icon));
        }
    }
    for child in widget.children() {
        chrome_cursor_walk(child, point, best);
    }
}

/// Depth-first walk of the widget tree, registering each visited node:
/// `test_id() -> rect` (highest priority) if set; the path-based key
/// `Parent/.../Self[sibling_index]` for un-tagged widgets; a bare-name
/// form (`Parent/Self`) for the first sibling of a given name.
///
/// `sibling_counts` is a parent-scoped counter map so children sharing a
/// name get deterministic `[0]`, `[1]`, ... suffixes. The shots
/// `ShellDriver` uses this exact walk too — exposed `pub` so both share
/// one implementation.
pub fn walk_widget<S1: std::hash::BuildHasher, S2: std::hash::BuildHasher>(
    w: &dyn ui::widget::Widget,
    parent_path: &str,
    sibling_counts: &mut HashMap<String, usize, S1>,
    out: &mut HashMap<String, vello::kurbo::Rect, S2>,
) {
    let name = w.name().to_string();
    let sibling_key = format!("{parent_path}::{name}");
    let idx = sibling_counts.entry(sibling_key).or_insert(0);
    let my_index = *idx;
    *idx = idx.saturating_add(1);
    let path = if parent_path.is_empty() {
        format!("{name}[{my_index}]")
    } else {
        format!("{parent_path}/{name}[{my_index}]")
    };
    // Shell chrome owns `tilo.shell.trusted-frame/...`. A widget that
    // copies the name, test id, or painted affordance does not publish it.
    if crate::shell::trusted_ui::widget_may_publish(&path) {
        out.insert(path.clone(), w.get_rect());
    }
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name
        } else {
            format!("{parent_path}/{name}")
        };
        if crate::shell::trusted_ui::widget_may_publish(&bare) {
            out.entry(bare).or_insert_with(|| w.get_rect());
        }
    }
    if let Some(id) = w.test_id()
        && crate::shell::trusted_ui::widget_may_publish(id)
    {
        out.insert(id.to_string(), w.get_rect());
    }
    // Affordances the widget paints itself. Skipping zero-area entries
    // is what makes a name mean "on screen now".
    for (name, rect) in w.test_rects() {
        if rect.area() > 0.0 && crate::shell::trusted_ui::widget_may_publish(&name) {
            let qualified = format!("{path}/{name}");
            if crate::shell::trusted_ui::widget_may_publish(&qualified) {
                out.insert(qualified, rect);
            }
            out.entry(name).or_insert(rect);
        }
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for child in w.children() {
        walk_widget(child, &path, &mut child_counts, out);
    }
}

/// The terminal a hosted Terminal tab shows, inside its app.
#[cfg(any(test, feature = "hosted-test-control"))]
fn hosted_terminal(widget: &mut dyn Widget) -> Option<&mut terminal::TerminalWidget> {
    let embedded = widget
        .as_any_mut()?
        .downcast_mut::<ui::app::EmbeddedApp>()?;
    terminal::mounted_terminal(embedded.component_root_mut()?)
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact frame geometry is the behavior under test"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;

    use super::hosted_terminal;
    /// Attach a real process before rebuilding any pointers into its widget.
    /// A probe of the unopened production tab cannot prove PTY dispatch.
    fn live_hosted_terminal_probe(fb: &mut FrameBuilder) -> std::sync::mpsc::Receiver<Vec<u8>> {
        // Focus-window first changes chrome structure; materialize that tab
        // before looking for its mounted application in the current node map.
        fb.relayout();
        let id = fb.active_terminal_node().expect("active mounted Terminal");
        // SAFETY: this pointer belongs to the current active mounted owner.
        let terminal = hosted_terminal(unsafe { &mut **fb.node_map.get(&id).unwrap() })
            .expect("production chrome must mount a hosted Terminal");
        *terminal = terminal::TerminalWidget::spawn_command(terminal::CommandBuilder::new("cat"))
            .expect("attach a real PTY process to the hosted Terminal");
        assert!(
            !terminal.is_disconnected(),
            "the real PTY must be available"
        );
        let probe = terminal.attach_input_probe();
        // Replacing the widget invalidates descendant pointers; rebuild before
        // measurement or focus reconciliation can dereference the old map.
        fb.rebuild_layout_tree();
        fb.relayout();
        probe
    }

    fn finish_hosted_terminal_probe(fb: &mut FrameBuilder) {
        let id = fb.active_terminal_node().expect("active mounted Terminal");
        // SAFETY: the live map was rebuilt after attaching its real process.
        let terminal = hosted_terminal(unsafe { &mut **fb.node_map.get(&id).unwrap() })
            .expect("the live hosted Terminal remains mounted");
        // The assertion already consumed the tested bytes. EOF is cleanup,
        // delivered through the same real PTY so cat and its reader both exit.
        terminal
            .try_write_input(b"\x04")
            .expect("close the real cat input");
        tilo_test_env::wait_until("the real PTY process and reader finish", || {
            terminal.is_disconnected()
        });
    }
    use crate::shell::state::FreshId as _;
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable;
    use winit::event::ElementState;
    use winit::keyboard::ModifiersState;

    /// A peer's window arriving in a workspace nobody governs yet: owed a
    /// look, but [`crate::shell::peer_windows::examine`] keeps it pending
    /// until the room's authority arrives, so it launches nothing here.
    fn arrive_peer_window(frame: &FrameBuilder) -> u64 {
        use crate::shell::state::{
            Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
            next_id,
        };
        let tab = Tab {
            id: next_id(),
            title: "Word Processor".into(),
            tile: TileContent::PeerWindow {
                app_id: "so.tilo.word-processor".into(),
                label: "Word Processor".into(),
                note: String::new(),
                missing: None,
            },
            app_title: None,
        };
        let tab_id = tab.id;
        let mut strip = Strip::new(StripId::from(next_id()), "main");
        strip.window_groups = vec![WindowGroup {
            id: next_id(),
            tabs: vec![tab],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        let mut workspace = Workspace::new(WorkspaceId::from(next_id()), "shared");
        workspace.strips = vec![strip];
        let chrome = frame.chrome.borrow();
        let mut tree = chrome.tree_mut();
        tree.desktop.workspaces.push(workspace);
        // What peer sync does after a round lands, from the network thread.
        tree.desktop.room_epoch = tree.desktop.room_epoch.wrapping_add(1);
        tab_id
    }

    /// The widget addresses the pointer map holds.
    fn mapped_widgets(frame: &FrameBuilder) -> std::collections::BTreeSet<usize> {
        frame
            .node_map
            .values()
            .map(|widget| widget.cast::<()>().addr())
            .collect()
    }

    /// A pump that rebuilds the workspace's content (here a new workspace;
    /// equally its first window opening or its last closing) drops the old
    /// content's widgets, the top bar among them. The frame loop reads the
    /// focused node through the pointer map before the next paint, which a
    /// backed-up renderer can defer for frames: the map must already name
    /// the new widgets when the pump returns, never the dropped ones.
    #[test]
    fn a_pump_that_rebuilds_the_content_leaves_no_pointer_to_dropped_widgets() {
        let mut frame = FrameBuilder::new((1280, 800));
        let mut doc = AutoCommit::new();
        frame.pump_apps(&mut doc, &automerge::ROOT);
        frame.relayout();
        let before = mapped_widgets(&frame);
        frame.with_chrome(|chrome| {
            chrome.add_workspace("content rebuilt by the pump");
        });
        frame.pump_apps(&mut doc, &automerge::ROOT);
        let after_pump = mapped_widgets(&frame);
        assert_ne!(
            after_pump, before,
            "the pump rebuilt the workspace's content"
        );
        frame.rebuild_layout_tree();
        assert_eq!(
            after_pump,
            mapped_widgets(&frame),
            "after the pump the pointer map names exactly the live widgets a rebuild maps"
        );
    }

    #[test]
    fn pump_room_contents_walks_the_tabs_only_after_the_tree_changes() {
        use crate::shell::peer_windows::work_counts;
        let mut frame = FrameBuilder::new((1280, 800));
        let mut doc = AutoCommit::new();
        assert!(
            !crate::shell::peer_windows::any_pending(&frame.chrome().tree()),
            "the demo desktop owes no window a look"
        );
        // The first rounds find the tree as it is and settle what they built.
        for _ in 0..3 {
            frame.pump_apps(&mut doc, &automerge::ROOT);
        }
        let quiet = work_counts();
        for _ in 0..5 {
            frame.pump_apps(&mut doc, &automerge::ROOT);
            assert_eq!(frame.peer_windows_poll(), None, "nothing to wake for");
        }
        assert_eq!(
            work_counts().0,
            quiet.0,
            "quiet rounds read three counters and never walk the tabs"
        );

        let tab = arrive_peer_window(&frame);
        let arrived = work_counts();
        frame.pump_apps(&mut doc, &automerge::ROOT);
        let after_arrival = work_counts();
        assert!(after_arrival.0 > arrived.0, "a landed round is looked at");
        assert_eq!(
            after_arrival.1,
            arrived.1.saturating_add(1),
            "the arriving window is examined once in its round"
        );
        // Still pending: every round looks again, and the loop keeps waking
        // for it, as before.
        for round in 1..=3 {
            frame.pump_apps(&mut doc, &automerge::ROOT);
            assert_eq!(
                work_counts().1,
                after_arrival.1.saturating_add(round),
                "a window still owed a look is examined every round"
            );
            assert!(
                frame.peer_windows_poll().is_some(),
                "a pending window keeps the 250 ms wake"
            );
        }

        // Answered outside the chrome (as a lifecycle callback would), the
        // next round sees it once and the loop goes quiet again.
        {
            let chrome = frame.chrome.borrow();
            let mut tree = chrome.tree_mut();
            for workspace in &mut tree.desktop.workspaces {
                for strip in &mut workspace.strips {
                    for group in &mut strip.window_groups {
                        for candidate in &mut group.tabs {
                            if candidate.id == tab
                                && let crate::shell::state::TileContent::PeerWindow { note, .. } =
                                    &mut candidate.tile
                            {
                                "Answered".clone_into(note);
                            }
                        }
                    }
                }
            }
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        }
        frame.pump_apps(&mut doc, &automerge::ROOT);
        frame.pump_apps(&mut doc, &automerge::ROOT);
        let answered = work_counts();
        for _ in 0..5 {
            frame.pump_apps(&mut doc, &automerge::ROOT);
            assert_eq!(frame.peer_windows_poll(), None);
        }
        assert_eq!(
            work_counts().0,
            answered.0,
            "once answered, quiet rounds stop walking"
        );
    }

    fn pressed_install_on_a_quiet_desktop(
        activate: impl FnOnce(&mut crate::shell::app_notice_card::AppNoticeCard),
    ) {
        use crate::shell::peer_windows::work_counts;
        use crate::shell::state::TileContent;
        use tilo_desktop_model::MissingApp;

        let mut frame = FrameBuilder::new((1280, 800));
        let mut doc = AutoCommit::new();
        assert!(!crate::shell::peer_windows::any_pending(
            &frame.chrome().tree()
        ));
        let tab = arrive_peer_window(&frame);
        {
            let chrome = frame.chrome.borrow();
            let mut tree = chrome.tree_mut();
            let workspace = tree.desktop.workspaces.last_mut().unwrap();
            // The production install path must refuse before reaching the
            // catalogue: this disposable fixture has no owner, so no one may
            // install an app into it.
            assert!(workspace.owner().is_none());
            let TileContent::PeerWindow { note, missing, .. } =
                &mut workspace.strips[0].window_groups[0].tabs[0].tile
            else {
                panic!("the fixture must contain the actual peer window");
            };
            crate::shell::peer_windows::NOT_INSTALLED.clone_into(note);
            *missing = Some(MissingApp::Installable {
                display_name: "Word Processor".into(),
            });
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        }
        for _ in 0..3 {
            frame.pump_apps(&mut doc, &automerge::ROOT);
        }
        assert!(!crate::shell::peer_windows::any_pending(
            &frame.chrome().tree()
        ));
        assert_eq!(frame.peer_windows_poll(), None);
        let quiet = work_counts();
        let mut card = crate::shell::app_notice_card::AppNoticeCard::install(
            tab,
            "Word Processor",
            "Word Processor",
            "",
        );
        activate(&mut card);
        let wake = frame.peer_windows_poll();
        frame.pump_apps(&mut doc, &automerge::ROOT);
        assert_eq!(
            work_counts().1,
            quiet.1.saturating_add(1),
            "the card press owes exactly one examination in the next pump"
        );
        assert!(wake.is_some(), "an install press must wake an idle desktop");
        {
            let chrome = frame.chrome.borrow();
            let tree = chrome.tree();
            let TileContent::PeerWindow { note, missing, .. } =
                &tree.desktop.workspaces.last().unwrap().strips[0].window_groups[0].tabs[0].tile
            else {
                panic!("a refused install must keep the peer window");
            };
            assert!(note.contains("no owner"), "existing refusal: {note}");
            assert_eq!(*missing, None);
        }
        assert_eq!(frame.peer_windows_poll(), None, "the request was drained");
        // Allow the refusal's chrome revision to settle, then prove both
        // examinations and tab walks stop on subsequent quiet rounds.
        frame.pump_apps(&mut doc, &automerge::ROOT);
        let settled = work_counts();
        assert_eq!(settled.1, quiet.1.saturating_add(1));
        for _ in 0..5 {
            frame.pump_apps(&mut doc, &automerge::ROOT);
            assert_eq!(frame.peer_windows_poll(), None);
        }
        assert_eq!(
            work_counts(),
            settled,
            "quiet rounds do no peer-window work"
        );
    }

    #[test]
    fn pump_room_contents_runs_a_pressed_install_without_other_pending_work() {
        pressed_install_on_a_quiet_desktop(
            crate::shell::app_notice_card::AppNoticeCard::press_for_test,
        );
    }

    #[test]
    fn pump_room_contents_runs_a_semantic_install_without_other_pending_work() {
        pressed_install_on_a_quiet_desktop(|card| {
            let target = card.test_id().unwrap().to_owned();
            let mut state = AutoCommit::new();
            assert!(card.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut state),
                &automerge::ROOT,
            ));
        });
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn workspace_switches_keep_record_rooms_menu_and_passive_activity() {
        fn record_bring_keeps_its_authorized_room_across_workspace_switch() {
            use crate::shell::search::action_api::{ActionArgs, ArgValue};
            use crate::shell::search::providers::ActionsProvider;
            use crate::shell::search::{
                InvocationDisposition, ResourceKind, SearchContext, SearchProvider,
            };
            use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
            use ui::record::{RecordId, RecordKind};

            let source_id = WorkspaceId::new();
            let destination_id = WorkspaceId::new();
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces = vec![
                Workspace::new(source_id, "Source"),
                Workspace::new(destination_id, "Destination"),
            ];
            tree.desktop.active = source_id;
            let chrome = ChromeState::new(tree);
            let (source_room, destination_room) = {
                let state = chrome.borrow();
                let mut tree = state.tree_mut();
                let workspaces = &mut tree.desktop.workspaces;
                let source_room = workspaces
                    .iter()
                    .find(|workspace| workspace.id == source_id)
                    .expect("source workspace")
                    .room();
                let destination_room = workspaces
                    .iter()
                    .find(|workspace| workspace.id == destination_id)
                    .expect("destination workspace")
                    .room();
                let source_user = state.local_user_for_room(&source_room);
                workspaces
                    .iter_mut()
                    .find(|workspace| workspace.id == source_id)
                    .expect("source workspace")
                    .set_owner(source_user);
                workspaces
                    .iter_mut()
                    .find(|workspace| workspace.id == destination_id)
                    .expect("destination workspace")
                    .set_owner("somebody-else");
                drop(tree);
                (source_room, destination_room)
            };
            let mut document = AutoCommit::new();
            let result = {
                let state = chrome.borrow();
                ActionsProvider::new()
                    .query(SearchContext {
                        query_scope: None,
                        include_programmatic: false,
                        needle: "bring record",
                        wanted: &[ResourceKind::Action],
                        chrome: &state,
                        doc: &document,
                        doc_root: &automerge::ROOT,
                    })
                    .into_iter()
                    .find(|result| result.id == "record.bring")
                    .expect("record action is discoverable in the authorized source")
            };
            let record = RecordId::minted();
            let args = ActionArgs::bind(
                &result.params,
                [
                    ("id".into(), ArgValue::Text(record.to_string())),
                    ("kind".into(), ArgValue::Text("field-guide".into())),
                ],
            )
            .expect("typed record arguments");
            let ticket = match result
                .on_begin_invoke
                .as_ref()
                .expect("record action is asynchronous")(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &chrome,
                &args,
            )
            .expect("source admits the record request")
            {
                InvocationDisposition::Pending(ticket) => ticket,
                InvocationDisposition::Accepted(_) => {
                    panic!("record request completed before frame")
                }
            };
            chrome.borrow().tree_mut().desktop.active = destination_id;
            let index = std::rc::Rc::new(std::cell::RefCell::new(
                crate::shell::room_records::RoomRecords::in_memory(),
            ));
            consume_record_requests(&chrome, &index);
            assert!(ticket().expect("ticket poll").is_some());
            assert!(index.borrow().holds(source_room, &record));
            assert!(!index.borrow().holds(destination_room, &record));

            let denied_record = RecordId::minted();
            chrome.borrow().tree_mut().desktop.active = source_id;
            let denied_args = ActionArgs::bind(
                &result.params,
                [
                    ("id".into(), ArgValue::Text(denied_record.to_string())),
                    (
                        "kind".into(),
                        ArgValue::Text(RecordKind::new("note").to_string()),
                    ),
                ],
            )
            .expect("typed denied arguments");
            let denied = match result
                .on_begin_invoke
                .as_ref()
                .expect("record action is asynchronous")(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &chrome,
                &denied_args,
            )
            .expect("request admitted before revocation")
            {
                InvocationDisposition::Pending(ticket) => ticket,
                InvocationDisposition::Accepted(_) => {
                    panic!("record request completed before frame")
                }
            };
            {
                let state = chrome.borrow();
                let mut tree = state.tree_mut();
                let source = tree
                    .desktop
                    .workspaces
                    .iter_mut()
                    .find(|workspace| workspace.id == source_id)
                    .expect("source workspace");
                source.policy.owner = None;
                source.sandbox.clear();
                tree.desktop.active = destination_id;
            }
            consume_record_requests(&chrome, &index);
            assert!(denied().is_err(), "revoked target authority fails closed");
            assert!(!index.borrow().holds(source_room, &denied_record));
            assert!(!index.borrow().holds(destination_room, &denied_record));
        }

        fn startup_keeps_unread_activity_passive() {
            let mut frame =
                crate::shell::test_support::PopulatedShellFixture::new((1600, 900), Theme::dark());
            crate::shell::test_support::collaborator_renames_active_workspace(
                &mut frame.chrome().tree_mut(),
            );
            // A live frame: the tick reads the room, then paint.
            let mut doc = AutoCommit::new();
            frame.pump_apps(&mut doc, &automerge::ROOT);
            let _ = frame.paint_settled();
            assert!(
                frame.chrome().activity_glance.unread > 0,
                "the restored fixture has unseen history to present in passive chrome"
            );
            assert!(!frame.chrome().overlays.room_activity);
            assert!(
                !crate::shell::input_policy::chrome_captures_input(&frame),
                "restoring a workspace must leave the application interactive"
            );
        }

        /// The workspace name opens the workspace menu, which lists the
        /// *live* workspaces, and choosing another goes there. Drives the
        /// per-frame path (`pump_apps` → paint) and presses the pill the way
        /// a person would.
        fn topbar_workspace_menu_tracks_the_live_active_workspace() {
            use winit::event::{ElementState, MouseButton};
            let mut fb = FrameBuilder::new((1600, 900));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            fb.with_chrome(|st| {
                st.add_workspace("second");
            });
            fb.pump_apps(&mut doc, &root);
            fb.relayout();
            let _ = fb.paint();
            let pill = *fb
                .widget_registry()
                .get("topbar_workspace_pill")
                .expect("the workspace pill is published");
            let mut focus = None;
            for event in [
                ui::synthetic_input::cursor_moved(pill.center().x, pill.center().y),
                ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left),
                ui::synthetic_input::mouse_input(ElementState::Released, MouseButton::Left),
            ] {
                fb.dispatch_event(
                    &event,
                    &winit::event::Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc),
                    &root,
                    &mut focus,
                );
            }
            let labels: Vec<String> = fb
                .host()
                .workspace_menu_labels()
                .expect("pressing the workspace name opens its menu")
                .into_iter()
                .flatten()
                .collect();
            let names: Vec<String> = fb
                .chrome()
                .tree()
                .desktop
                .workspaces
                .iter()
                .map(|w| w.name.clone())
                .collect();
            for name in &names {
                assert!(labels.contains(name), "{name} is listed: {labels:?}");
            }
            assert!(labels.iter().any(|label| label == "Overview"));
            let first = names.first().expect("a workspace").clone();
            assert!(fb.host_mut().choose_in_workspace_menu(&first));
            let tree = fb.chrome().tree().clone();
            assert_eq!(
                tree.desktop.workspaces.first().map(|w| w.id),
                Some(tree.desktop.active),
                "choosing the first workspace goes there"
            );
            assert!(
                fb.host().workspace_menu_labels().is_none(),
                "and closes the menu"
            );
        }

        record_bring_keeps_its_authorized_room_across_workspace_switch();
        startup_keeps_unread_activity_passive();
        topbar_workspace_menu_tracks_the_live_active_workspace();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn frame_builder_paints_rebuilds_only_on_change_and_keeps_widget_addresses() {
        fn moving_frame_builder_preserves_live_layout_widget_addresses() {
            // Construction already moves the builder out of its constructor;
            // moving it into an allocation exercises another real ownership move.
            let frame = Box::new(FrameBuilder::new((1280, 800)));
            let host: &dyn Widget = frame.host.as_ref();
            let mapped_host = frame.node_map[&frame.host_node.expect("host layout node")];
            assert!(
                std::ptr::addr_eq(mapped_host, std::ptr::from_ref(host)),
                "layout's host pointer must still refer to the live host after a move"
            );
            // Palette and Capture Studio are inline host fields, not independent
            // heap allocations. Their layout bindings must survive the move too.
            for child in host.children() {
                assert!(
                    frame
                        .node_map
                        .values()
                        .any(|&mapped| { std::ptr::addr_eq(mapped, std::ptr::from_ref(child)) }),
                    "{} lost its live layout binding when the frame moved",
                    child.name()
                );
            }
        }

        /// Smoke: a fresh builder produces a non-empty Vello scene at 1600x900.
        /// The exact byte-count would change with any rendering tweak, so we
        /// only assert the scene encoded *something*.
        fn frame_builder_paints_non_empty_scene() {
            let mut fb = FrameBuilder::new((1600, 900));
            let scene = fb.paint();
            assert!(
                !scene.encoding().is_empty(),
                "fresh FrameBuilder should produce a non-empty Vello scene"
            );
        }

        /// `with_chrome` mutations are visible through `chrome()` — the
        /// shared cell is the same one the host borrows during draw.
        fn frame_builder_chrome_round_trips() {
            let mut fb = FrameBuilder::new((1600, 900));
            assert!(!fb.chrome().overlays.overview);
            fb.with_chrome(|st| st.overlays.overview = true);
            assert!(fb.chrome().overlays.overview);
        }

        /// `set_viewport` rebuilds the layout tree without panicking and
        /// keeps `paint` working.
        fn frame_builder_resize_paints_again() {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint();
            fb.set_viewport((1280, 720));
            fb.relayout();
            let scene = fb.paint();
            assert!(!scene.encoding().is_empty());
        }

        /// …and a frame where nothing arrived must not bump, or the shell
        /// rebuilds its widget tree on every idle pass.
        fn a_quiet_frame_does_not_rebuild() {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            fb.pump_apps(&mut doc, &root);
            let settled = fb.chrome_revision();
            fb.pump_apps(&mut doc, &root);
            assert_eq!(
                fb.chrome_revision(),
                settled,
                "nothing arrived; the chrome revision must stand still"
            );
        }

        /// A window from a peer must reach the widget tree, not just the
        /// workspace tree: peer-sync writes the tree from the network thread,
        /// which can't bump the chrome revision `resync_strips_from_tree` gates
        /// its rebuild on. Drives the ingest and per-frame pump.
        fn a_peers_changes_landing_make_the_frame_rebuild() {
            use crate::shell::state::PeerOrigin;
            use collab_types::caps::{Access, CapOverride, Scope, Subject};

            // A room owned by somebody else, with something in it.
            let owner = "ada".to_string();
            let mut host = WorkspaceTree::default();
            let host_ws = host.append_empty_workspace("Shared");
            host.adopt_ownerless_workspaces(&owner);
            let room = host
                .room_of(host_ws)
                .expect("the host's workspace has a room");
            host.grant_sandbox_in_workspace(
                host_ws,
                &owner,
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath("/shared".to_string()),
                    Access::Read,
                ),
            )
            .expect("the owner may grant in their own room");
            host.sync_room_doc(room);
            let changes = host
                .export_room_changes_since(room, &[])
                .expect("the host is in the room");
            assert!(!changes.is_empty(), "the host must have something to send");

            // This session joins it.
            let mut fb = FrameBuilder::new((1600, 900));
            let joined = {
                let chrome = fb.chrome();
                let mut tree = chrome.tree_mut();
                let active = tree.desktop.active;
                // Joined through the peer's invite, which pins it (hub 1)
                // as the room's governor.
                tree.stand_in_room(active, room)
                    && tree.pin_room_authority(room, PeerOrigin::hub(1))
            };
            assert!(joined, "this session joins the peer's room");

            let mut doc = AutoCommit::new();
            let doc_root = automerge::ROOT;
            // Settle: pump once so the builder is past its first frame and
            // any revision movement below is attributable to the arrival.
            fb.pump_apps(&mut doc, &doc_root);
            let before = fb.chrome_revision();

            let landed = {
                let chrome = fb.chrome();
                let mut tree = chrome.tree_mut();
                tree.apply_room_changes(room, &changes, &PeerOrigin::hub(1))
                    .expect("well-formed changes parse")
            };
            assert!(
                landed,
                "the peer's changes must merge into this session's tree"
            );

            fb.pump_apps(&mut doc, &doc_root);
            assert!(
                fb.chrome_revision() > before,
                "a peer's change landed in the tree and nothing told the shell \
                 to rebuild, so it would never be drawn"
            );
        }

        /// `widget_registry` is idempotent — two consecutive calls with no
        /// layout/chrome mutation between return the same keys mapped to the
        /// same rects; the walker may not mutate anything.
        fn widget_registry_is_idempotent_between_calls() {
            let mut fb = FrameBuilder::new((1600, 900));
            // Settle the layout once so rects are populated.
            let _ = fb.paint();
            let snap_a = fb.widget_registry();
            let snap_b = fb.widget_registry();
            assert_eq!(
                snap_a.len(),
                snap_b.len(),
                "widget_registry should return the same number of entries \
                 across repeated calls"
            );
            for (key, rect) in &snap_a {
                assert_eq!(
                    snap_b.get(key),
                    Some(rect),
                    "widget_registry key `{key}` mismatched between calls: \
                     {:?} vs {:?}",
                    rect,
                    snap_b.get(key)
                );
            }
        }

        fn external_scene_append_is_an_unversioned_retained_boundary() {
            let mut frame = FramePaint::full(Scene::new());
            frame.revision = 42;
            frame.previous_revision = Some(41);
            frame.damage_regions = Some(vec![FineDamageRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }]);
            frame.force_full();
            assert_eq!(frame.revision, 0);
            assert_eq!(frame.previous_revision, None);
            assert_eq!(frame.damage_regions, None);
        }

        moving_frame_builder_preserves_live_layout_widget_addresses();
        frame_builder_paints_non_empty_scene();
        frame_builder_chrome_round_trips();
        frame_builder_resize_paints_again();
        a_quiet_frame_does_not_rebuild();
        a_peers_changes_landing_make_the_frame_rebuild();
        widget_registry_is_idempotent_between_calls();
        external_scene_append_is_an_unversioned_retained_boundary();
    }

    fn native_click_named_control(
        frame: &mut FrameBuilder,
        doc: &mut AutoCommit,
        focus: &mut Option<NodeId>,
        name: &str,
    ) {
        let center = frame
            .widget_registry()
            .get(name)
            .expect("the real control is addressable")
            .center();
        for event in [
            WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(center.x, center.y),
            },
            WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
            },
        ] {
            frame.dispatch_event(
                &event,
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(doc),
                &automerge::ROOT,
                focus,
            );
        }
    }

    fn native_type_literal(
        frame: &mut FrameBuilder,
        doc: &mut AutoCommit,
        focus: &mut Option<NodeId>,
        text: &str,
    ) {
        let mut event = key_press(Key::Character(text.into()));
        if let WindowEvent::KeyboardInput { event, .. } = &mut event {
            event.text = Some(text.into());
        }
        assert!(frame.dispatch_event(
            &event,
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(doc),
            &automerge::ROOT,
            focus
        ));
    }

    #[test]
    fn native_overlay_palette_returns_to_the_named_editor_after_escape() {
        for viewport in [(1600, 900), (800, 600)] {
            let first = ui::LocalState::new(String::new());
            let second = ui::LocalState::new(String::new());
            let content = ui::widget::Column::new()
                .with_id("focus-return-form")
                .push(Box::new(
                    ui::widget::TextInput::from_state(&first)
                        .with_id("return-editor")
                        .with_accessible_name("Return editor"),
                ))
                .push(Box::new(
                    ui::widget::TextInput::from_state(&second)
                        .with_id("other-editor")
                        .with_accessible_name("Other editor"),
                ));
            let mut frame = FrameBuilder::new(viewport);
            frame.node_map.clear();
            *frame.host = ShellOverlayHost::with_fixture_content(Box::new(content));
            frame.rebuild_layout_tree();
            let mut doc = AutoCommit::new();
            frame.host.sync(&doc, &automerge::ROOT);
            let _ = frame.paint_settled();
            let mut focus = None;
            native_click_named_control(&mut frame, &mut doc, &mut focus, "return-editor");
            assert!(frame.dispatch_event(
                &key_press(Key::Named(NamedKey::Space)),
                &Modifiers::from(ModifiersState::SUPER),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus
            ));
            native_type_literal(&mut frame, &mut doc, &mut focus, "h");
            assert_eq!(frame.host.palette_query(), "h");
            // Rebuild through the actual viewport path while search owns focus.
            frame.set_viewport(viewport);
            frame.relayout();
            // The first Escape clears what was typed; the second closes.
            for _ in 0..2 {
                assert!(frame.dispatch_event(
                    &key_press(Key::Named(NamedKey::Escape)),
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &mut focus
                ));
            }
            assert!(!frame.host.palette_visible());
            let _ = frame.paint_settled();
            frame.reconcile_native_focus(&mut focus);
            let update = frame.build_a11y_tree_update(focus, 1.0);
            let focused = update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .expect("the returned editor is published to accessibility");
            assert_eq!(focused.1.role(), ui::accesskit::Role::TextInput);
            assert_eq!(focused.1.label(), Some("Return editor"));
            native_type_literal(&mut frame, &mut doc, &mut focus, "z");
            assert_eq!(
                first.with(Clone::clone),
                "z",
                "typing resumes in the original named editor"
            );
            assert_eq!(
                second.with(Clone::clone),
                "",
                "the other editor must receive no draft or focus transfer"
            );
            let editor = focus
                .and_then(|node| frame.node_map.get(&node))
                .expect("native focus remains on an editor");
            assert_eq!(unsafe { (**editor).test_id() }, Some("return-editor"));
        }
    }

    #[test]
    fn native_agent_composer_owns_typing_and_returns_to_the_original_editor() {
        let draft = ui::LocalState::new(String::new());
        let content = ui::widget::TextInput::from_state(&draft)
            .with_id("composer-return-editor")
            .with_accessible_name("Original editor");
        let mut frame = FrameBuilder::new((800, 600));
        frame.node_map.clear();
        *frame.host = ShellOverlayHost::with_fixture_content(Box::new(content));
        frame.rebuild_layout_tree();
        let mut doc = AutoCommit::new();
        frame.host.sync(&doc, &automerge::ROOT);
        let _ = frame.paint_settled();
        let mut focus = None;
        native_click_named_control(&mut frame, &mut doc, &mut focus, "composer-return-editor");
        assert!(
            frame.node_map.values().all(|&widget| {
                // SAFETY: these pointers belong to the current live layout tree.
                unsafe { (*widget).test_id() != Some("agent-quick-editor") }
            }),
            "a closed composer has no editor in the measured/control tree"
        );
        frame.host.agent_quick_composer_mut().open(
            crate::shell::agent_quick_composer::ComposerOwner {
                room: "fixture-room".into(),
                agent: "fixture-agent".into(),
                person: "fixture-person".into(),
                identity_revision: 1,
            },
            "Tide",
            crate::shell::agent_quick_composer::AgentQuickComposerOrigin {
                marker: Rect::new(100.0, 100.0, 132.0, 132.0),
                protected: Rect::new(90.0, 90.0, 210.0, 150.0),
                context: "Reviewing the document".into(),
            },
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        frame.host.sync(&doc, &automerge::ROOT);
        frame.relayout();
        frame.reconcile_native_focus(&mut focus);
        native_type_literal(&mut frame, &mut doc, &mut focus, "hello");
        assert_eq!(draft.get(), "", "the background editor receives no typing");
        assert_eq!(
            frame
                .node_map
                .get(&focus.expect("composer editor owns focus"))
                .map(|widget| unsafe { (**widget).test_id() }),
            Some(Some("agent-quick-editor")),
        );
        assert!(frame.focused_ime_field(focus).is_some());
        assert!(frame.dispatch_event(
            &key_press(Key::Named(NamedKey::Escape)),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        frame.relayout();
        frame.reconcile_native_focus(&mut focus);
        assert!(!frame.host.agent_quick_composer_open());
        assert!(
            frame.node_map.values().all(|&widget| {
                // SAFETY: dismissal rebuilt the map against the live tree.
                unsafe { (*widget).test_id() != Some("agent-quick-editor") }
            }),
            "dismissal retires the composer's layout and metadata nodes"
        );
        native_type_literal(&mut frame, &mut doc, &mut focus, "returned");
        assert_eq!(draft.get(), "returned");
    }

    #[test]
    fn native_overlay_palette_keeps_focus_across_result_rebuilds() {
        for viewport in [(1600, 900), (800, 600)] {
            let mut fb = FrameBuilder::new(viewport);
            let mut doc = AutoCommit::new();
            let mut focus = None;
            fb.relayout();
            assert!(fb.dispatch_event(
                &key_press(Key::Named(NamedKey::Space)),
                &Modifiers::from(ModifiersState::SUPER),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert!(fb.host().palette_visible());
            assert_eq!(
                focus,
                fb.palette_focus_target(),
                "the opening key hands off focus"
            );
            for (character, expected) in [("h", "h"), ("i", "hi")] {
                // Both output changes and refreshed results replace node ids.
                // The next character must still reach the visible query.
                fb.set_viewport(viewport);
                fb.pump_apps(&mut doc, &automerge::ROOT);
                fb.relayout();
                assert!(fb.focus_tree_changed, "search results rebuild their layout");
                let mut typed = key_press(Key::Character(character.into()));
                if let WindowEvent::KeyboardInput { event, .. } = &mut typed {
                    event.text = Some(character.into());
                }
                assert!(fb.dispatch_event(
                    &typed,
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &mut focus,
                ));
                assert_eq!(fb.host().palette_query(), expected);
                assert_eq!(focus, fb.palette_focus_target());
                assert_eq!(
                    fb.node_map
                        .values()
                        .filter(|widget| unsafe { (***widget).is_focused() })
                        .count(),
                    1,
                    "only the current query owns native keyboard focus"
                );
                let _ = fb.paint_settled();
                fb.reconcile_native_focus(&mut focus);
                let field = fb.focused_ime_field(focus).expect("search owns the IME");
                assert!(field.width > 0 && field.height > 0);
                let update = fb.build_a11y_tree_update(focus, 1.0);
                let focused = update
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == update.focus)
                    .expect("the focused input is published");
                assert_eq!(focused.1.role(), ui::accesskit::Role::TextInput);
                assert_eq!(
                    update
                        .nodes
                        .iter()
                        .filter(|(_, node)| node.role() == ui::accesskit::Role::TextInput)
                        .count(),
                    1,
                    "assistive technology sees the same single query input"
                );
            }
        }
    }

    fn assert_retained_bound_text(frame: &FrameBuilder, expected: &str, body_width: f32) {
        let label = frame
            .node_map
            .values()
            .find_map(|&widget| {
                // SAFETY: this production frame still owns the admitted live tree.
                let widget = unsafe { &*widget };
                (widget.test_id() == Some("retained-overlay-text")).then_some(widget)
            })
            .expect("the retained bound Text stays mounted");
        let rect = label.get_rect();
        if expected.is_empty() {
            assert_eq!(rect.height(), 0.0, "empty bound text leaves the flow");
            return;
        }
        let style = frame
            .theme
            .text(ui::tokens::TextRole::Body)
            .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph);
        // The allocation is independent of the label's rectangle, so zero or
        // one-glyph wrapping cannot validate its own broken measurement.
        let (width, height) =
            TextRenderer::new().measure_style(expected, Some(body_width + 1.0), &style);
        assert!(
            rect.width() + 0.5 >= f64::from(body_width),
            "{rect:?} must fill its body allocation"
        );
        // Text::draw shapes with a one-logical-pixel wrap guard. The ink
        // bound must obey that same guard, not a smaller rounding margin.
        assert!(
            rect.width() + 1.0 >= f64::from(width),
            "{rect:?} must fit the complete text width {width}"
        );
        assert!(
            (rect.height() - f64::from(height)).abs() < 1.0,
            "{rect:?} must fit the complete wrapped height {height}"
        );
        assert!(rect.x0 >= 0.0);
        assert!(rect.x1 <= frame.viewport.0);
        assert!(rect.y0 >= 0.0);
        assert!(rect.y1 <= frame.viewport.1);
        assert_eq!(
            label.accessibility().and_then(|node| node.name),
            Some(expected.into())
        );
    }

    #[test]
    fn native_overlay_retained_bound_text_reenters_flow_without_rebuilding() {
        for theme in [Theme::dark(), Theme::high_contrast().with_text_scale(2.0)] {
            for viewport in [(1600, 900), (800, 600)] {
                let text = ui::LocalState::new(String::new());
                let body_width = if viewport.0 == 800 { 240.0 } else { 400.0 };
                let content = ui::widget::Container::new()
                    .with_no_padding()
                    .with_width(body_width)
                    .with_child(Box::new(
                        ui::widget::Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .push(Box::new(
                                ui::widget::Text::from_state(&text, Clone::clone)
                                    .with_wrap_mode(ui::text_render::TextWrapMode::WordOrGlyph)
                                    .with_id("retained-overlay-text"),
                            )),
                    ));
                let mut frame = FrameBuilder::new(viewport);
                // Replacing fixture content retires the old map before any
                // measurement, focus or draw can dereference its pointers.
                frame.node_map.clear();
                *frame.host = ShellOverlayHost::with_fixture_content(Box::new(content));
                frame.theme = theme.clone();
                frame.rebuild_layout_tree();
                let doc = AutoCommit::new();
                // Materialize the shell's first synchronized presentation
                // while the bound text is still empty, before retaining it.
                frame.host.sync(&doc, &automerge::ROOT);
                frame.relayout();
                let _ = frame.paint_settled();
                let builds = frame.layout_tree_builds;
                assert_retained_bound_text(&frame, "", body_width);
                for expected in [
                    "The collaborator is reviewing the current document and checking every change against the requested task before proceeding.",
                    "A longer observation replaces the first one without replacing this tree. It must wrap cleanly within the existing panel, preserving the complete accessible text and usable allocation.",
                    "",
                    "The retained label returns after being empty, without stale Display::None or one-character wrapping.",
                ] {
                    text.set(expected.into());
                    frame.host.sync(&doc, &automerge::ROOT);
                    frame.relayout();
                    let _ = frame.paint();
                    assert_eq!(
                        frame.layout_tree_builds, builds,
                        "sync, relayout and paint retain the original tree"
                    );
                    assert_retained_bound_text(&frame, expected, body_width);
                }
            }
        }
    }

    #[test]
    fn frame_paints_damage_proxies_night_light_and_osk_within_bounds() {
        fn native_damage_covers_clip_tiles_and_clamps_the_viewport() {
            let frame = FrameBuilder::new((3839, 2159));
            assert_eq!(
                frame.native_damage_rect(Some(Rect::new(3203.5, 49.25, 3824.5, 300.2))),
                Some(FineDamageRect {
                    x: 3200,
                    y: 48,
                    width: 639,
                    height: 256,
                }),
                "include the clip tile's neighbouring pixels, including x=3200"
            );
            assert_eq!(
                frame.native_damage_rect(Some(Rect::new(-8.0, 2148.0, 17.0, 2180.0))),
                Some(FineDamageRect {
                    x: 0,
                    y: 2144,
                    width: 32,
                    height: 15,
                }),
                "edge tiles must stop at a viewport that is not tile aligned"
            );
        }

        fn transfer_proxy_covers_both_ownership_edges_until_destination_pixels_exist() {
            assert!(floating_transfer_proxy_needed(false, false, true, false));
            assert!(floating_transfer_proxy_needed(false, true, true, true));
            assert!(floating_transfer_proxy_needed(false, true, false, false));
            assert!(!floating_transfer_proxy_needed(false, true, false, true));
            assert!(floating_transfer_proxy_needed(true, false, true, false));
            assert!(!floating_transfer_proxy_needed(true, false, false, false));
        }

        fn night_light_is_neutral_at_daylight_and_warmer_at_lower_kelvin() {
            assert_eq!(
                night_light_multiplier(6_500),
                Color::from_rgb8(255, 255, 255)
            );
            assert_ne!(night_light_multiplier(2_800), night_light_multiplier(4_500));
        }

        fn osk_overlay_is_bottom_anchored_and_active_only() {
            const SURFACE: u64 = 9_900_701;
            crate::shell::foreign::store().publish_rgba(SURFACE, &[0, 0, 0, 255], 1, 1);
            let mut fb = FrameBuilder::new((1600, 900));

            fb.set_osk_surface(Some(SURFACE), None, None, false);
            assert!(fb.osk_surface_hit((800.0, 700.0)).is_none());

            fb.set_osk_surface(Some(SURFACE), None, None, true);
            let (id, rect) = fb
                .osk_surface_hit((800.0, 700.0))
                .expect("active keyboard is hit-testable");
            assert_eq!(id, SURFACE);
            assert_eq!(rect.width(), 960.0);
            assert_eq!(rect.y1, 880.0);

            fb.set_osk_surface(Some(SURFACE), None, None, false);
            assert!(fb.osk_surface_hit((800.0, 700.0)).is_none());
            crate::shell::foreign::store().forget(SURFACE);
        }

        let _env_lock = tilo_test_env::lock();
        native_damage_covers_clip_tiles_and_clamps_the_viewport();
        transfer_proxy_covers_both_ownership_edges_until_destination_pixels_exist();
        night_light_is_neutral_at_daylight_and_warmer_at_lower_kelvin();
        osk_overlay_is_bottom_anchored_and_active_only();
    }

    #[test]
    fn hdr_surfaces_are_recorded_as_frame_layers() {
        const SURFACE: u64 = 9_900_702;
        let _env_lock = tilo_test_env::lock();
        let signal = crate::shell::hdr_surface::SurfaceSignal::for_colorimetry(
            crate::collab::colorimetry::Colorimetry::hdr10(),
            crate::shell::hdr_tonemap::HdrTonemapConfig::default(),
        )
        .expect("PQ");
        let plane =
            crate::shell::hdr_surface::HdrPlane::from_signal_rgb(2, 2, [[0.6; 3]; 4], signal)
                .expect("plane");
        let plane_id = plane.id();
        crate::shell::foreign::store().publish_hdr_frame(SURFACE, &[40; 16], plane);
        let mut fb = FrameBuilder::new((1600, 900));
        fb.set_osk_surface(Some(SURFACE), None, None, true);

        let frame = fb.paint_with_damage();
        let [layer] = frame.hdr_layers.as_slice() else {
            panic!("one HDR layer, got {}", frame.hdr_layers.len());
        };
        assert_eq!(layer.plane.id(), plane_id);
        assert_eq!(layer.channel, 0);
        assert!(
            layer.bounds.width() > 2.0 && layer.bounds.y1 <= 900.0,
            "the keyboard's surface is scaled into its bottom panel: {:?}",
            layer.bounds
        );
        let _ = fb.paint();
        assert_eq!(fb.take_hdr_layers().len(), 1, "the dense paint records too");
        assert!(fb.take_hdr_layers().is_empty(), "taken once");

        fb.set_retained_layer_width(Some(8192));
        let retained = fb.paint_with_damage();
        assert!(
            retained.cache.is_none(),
            "HDR keeps the dense compositor path"
        );
        assert_eq!(retained.hdr_layers.len(), 1);
        assert_eq!(retained.hdr_layers[0].plane.id(), plane_id);

        crate::shell::foreign::store().publish_rgba(SURFACE, &[40; 16], 2, 2);
        assert!(
            fb.paint_with_damage().hdr_layers.is_empty(),
            "an SDR frame has none"
        );
        crate::shell::foreign::store().forget(SURFACE);
    }

    #[test]
    fn a_fullscreen_client_alone_on_top_is_a_scanout_candidate_until_anything_covers_it() {
        const FULLSCREEN: u64 = 9_900_705;
        const POPUP: u64 = 9_900_706;
        const SIZE: (u32, u32) = (1600, 900);
        let _env_lock = tilo_test_env::lock();
        let store = crate::shell::foreign::store();
        let pixels = [10, 20, 30, 255].repeat(SIZE.0.saturating_mul(SIZE.1) as usize);
        store.publish_rgba(FULLSCREEN, &pixels, SIZE.0, SIZE.1);
        store.publish_rgba(POPUP, &[0, 0, 0, 255], 1, 1);
        let output = Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1));
        let mut fb = FrameBuilder::new(SIZE);
        assert!(
            fb.paint_with_damage().scanout.is_none(),
            "a desktop without a fullscreen client has none"
        );
        fb.set_fullscreen_surface(Some((FULLSCREEN, "Fullscreen".to_owned(), output)));

        // Phase `alone_on_top`.
        let mut frame = fb.paint_with_damage();
        let candidate = frame
            .scanout_candidate()
            .expect("the fullscreen client alone on top is a candidate");
        assert_eq!(candidate.surface, FULLSCREEN);
        assert_eq!(candidate.origin, (0, 0));
        assert_eq!(candidate.size, SIZE);
        assert!(candidate.covers((0, 0), SIZE));

        // Phase `drawn_over_after_the_frame_builder`: a drag or a presentation
        // surface appended to the finished scene voids it on that frame.
        frame.scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(255, 0, 0),
            None,
            &Rect::new(0.0, 0.0, 8.0, 8.0),
        );
        assert!(frame.scanout_candidate().is_none());

        // Phase `a_popup_over_it`.
        fb.set_transient_surfaces(&[(POPUP, Rect::new(10.0, 10.0, 40.0, 40.0))]);
        assert!(fb.paint_with_damage().scanout.is_none(), "a popup above");
        fb.set_transient_surfaces(&[]);
        assert!(fb.paint_with_damage().scanout.is_some(), "the popup closed");

        // Phase `the_keyboard_over_it`.
        fb.set_osk_surface(Some(POPUP), None, None, true);
        assert!(fb.paint_with_damage().scanout.is_none(), "the keyboard up");
        fb.set_osk_surface(Some(POPUP), None, None, false);
        assert!(
            fb.paint_with_damage().scanout.is_some(),
            "the keyboard down"
        );

        // Phase `night_light`: a tint over every pixel of the output.
        fb.shots_theme_pinned = true;
        fb.watch_configured_theme = true;
        fb.night_light = true;
        fb.night_light_temperature = 3_000;
        assert!(fb.paint_with_damage().scanout.is_none(), "night light on");
        fb.night_light = false;
        fb.watch_configured_theme = false;
        assert!(fb.paint_with_damage().scanout.is_some(), "night light off");

        // Phase `a_buffer_that_does_not_fill_the_output`: letterboxed, it
        // is pixel exact but covers only part of the display.
        store.publish_rgba(FULLSCREEN, &pixels[..pixels.len() / 2], SIZE.0, SIZE.1 / 2);
        let frame = fb.paint_with_damage();
        let candidate = frame.scanout_candidate().expect("still pixel exact");
        assert!(!candidate.covers((0, 0), SIZE));

        fb.set_fullscreen_surface(None);
        assert!(fb.paint_with_damage().scanout.is_none(), "fullscreen left");
        store.forget(FULLSCREEN);
        store.forget(POPUP);
    }

    /// Not only the fullscreen overlay: any client frame drawn last of all,
    /// unclipped, is on top of everything on its display (a maximized
    /// window drawn edge to edge is one; a surface floating over the whole
    /// desktop is the simplest). The frame names that client's surface, and
    /// the proof goes as soon as anything is drawn above it.
    #[test]
    fn a_client_frame_drawn_last_of_all_is_a_scanout_candidate() {
        const SURFACE: u64 = 9_900_708;
        const POPUP: u64 = 9_900_709;
        const SIZE: (u32, u32) = (1600, 900);
        let _env_lock = tilo_test_env::lock();
        let store = crate::shell::foreign::store();
        let pixels = [10, 20, 30, 255].repeat(SIZE.0.saturating_mul(SIZE.1) as usize);
        store.publish_rgba(SURFACE, &pixels, SIZE.0, SIZE.1);
        store.publish_rgba(POPUP, &[0, 0, 0, 255], 1, 1);
        let output = Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1));
        let mut fb = FrameBuilder::new(SIZE);
        fb.set_floating_surfaces_with_input(&[(SURFACE, "org.alacritty".to_owned(), output, None)]);
        let frame = fb.paint_with_damage();
        let candidate = frame
            .scanout_candidate()
            .expect("the client frame drawn last of all is a candidate");
        assert_eq!(candidate.surface, SURFACE);
        assert!(candidate.covers((0, 0), SIZE));

        fb.set_transient_surfaces(&[(POPUP, Rect::new(10.0, 10.0, 40.0, 40.0))]);
        assert!(fb.paint_with_damage().scanout.is_none(), "a popup above it");
        fb.set_transient_surfaces(&[]);
        fb.set_floating_surfaces_with_input(&[]);
        store.forget(SURFACE);
        store.forget(POPUP);
    }

    /// A maximized window hides the top bar on the display it fills, as
    /// the person's "hide the top bar" setting does everywhere, so the
    /// window is all app until the bar is reached for; restored, the bar
    /// docks again.
    #[test]
    fn a_maximized_window_hides_the_top_bar_on_its_display() {
        use crate::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
            WorkspaceId, WorkspaceTree,
        };
        use tilo_desktop::chrome::TopBarPresence;
        let tree = |maximized: bool| {
            let ws_id = WorkspaceId(81);
            let strip = StripId(82);
            let mut ws = Workspace::new(ws_id, "Typing");
            ws.strips = vec![Strip {
                id: strip,
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: 83,
                    tabs: vec![Tab {
                        id: 84,
                        title: "Notes".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Other("Notes".into()),
                            label: "Notes".into(),
                        },
                        app_title: None,
                    }],
                    active_tab: 84,
                    width: WindowWidth::Fraction(667),
                    maximized,
                    focus_history: Vec::new(),
                }],
            }];
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces = vec![ws];
            tree.desktop.active = ws_id;
            tree.desktop.active_strip.insert(ws_id, strip);
            tree
        };
        let presence = |maximized: bool| {
            let mut fb = FrameBuilder::with_tree(tree(maximized), (1600, 900));
            fb.relayout();
            let _ = fb.paint();
            fb.relayout();
            let _ = fb.paint();
            fb.chrome().top_bar_presence()
        };
        assert_eq!(presence(false), TopBarPresence::Docked);
        assert_eq!(
            presence(true),
            TopBarPresence::Hidden,
            "the maximized window fills the display the bar stands on"
        );
    }

    /// A window maximized by key is drawn whole in the key's own frame: the
    /// bar steps aside and the window takes the display in that frame, so
    /// the next one repaints nothing. Restored by key, the same.
    #[test]
    fn a_maximize_by_key_is_drawn_whole_in_its_own_frame() {
        use crate::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
            WorkspaceId, WorkspaceTree,
        };
        use tilo_desktop::chrome::TopBarPresence;
        use winit::keyboard::{Key, ModifiersState, NamedKey};
        fn frame(fb: &mut FrameBuilder, doc: &mut AutoCommit) -> FramePaint {
            fb.pump_apps(doc, &automerge::ROOT);
            fb.relayout();
            fb.paint_with_damage()
        }
        let ws_id = WorkspaceId(91);
        let strip = StripId(92);
        let mut ws = Workspace::new(ws_id, "Typing");
        ws.strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: 93,
                tabs: vec![Tab {
                    id: 94,
                    title: "Notes".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Notes".into()),
                        label: "Notes".into(),
                    },
                    app_title: None,
                }],
                active_tab: 94,
                width: WindowWidth::Fraction(667),
                maximized: false,
                focus_history: Vec::new(),
            }],
        }];
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces = vec![ws];
        tree.desktop.active = ws_id;
        tree.desktop.active_strip.insert(ws_id, strip);
        tree.desktop.active_group.insert(strip, 93);
        let mut fb = FrameBuilder::with_tree(tree, (1600, 900));
        let mut doc = AutoCommit::new();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let _ = frame(&mut fb, &mut doc);
            if !fb.needs_animation_frame() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the desktop settles: {:?}",
                fb.animation_owners()
            );
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        let _ = frame(&mut fb, &mut doc);
        let super_held = winit::event::Modifiers::from(ModifiersState::SUPER);
        for (key, presence) in [
            (NamedKey::ArrowUp, TopBarPresence::Hidden),
            (NamedKey::ArrowDown, TopBarPresence::Docked),
        ] {
            let press = ui::synthetic_input::key_press(Key::Named(key), None);
            let mut focus = None;
            assert!(
                fb.dispatch_event(
                    &press,
                    &super_held,
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &mut focus,
                ),
                "Super+{key:?} is the desktop's"
            );
            let _ = frame(&mut fb, &mut doc);
            assert_eq!(
                fb.chrome().top_bar_presence(),
                presence,
                "after Super+{key:?}; maximized {:?}",
                fb.chrome().maximized_group()
            );
            let next = frame(&mut fb, &mut doc);
            assert!(
                matches!(next.damage_regions.as_deref(), Some([])),
                "Super+{key:?} was drawn whole in its own frame; the next repaints {:?}",
                next.damage_regions
            );
        }
    }

    /// A window that was a tile before it went fullscreen keeps drawing over
    /// the whole output: the tile's last configure must not leave it a
    /// centred, tile-sized box floating above the desktop.
    #[test]
    fn a_fullscreen_client_that_was_a_tile_fills_the_output_not_its_old_box() {
        const FULLSCREEN: u64 = 9_900_707;
        const SIZE: (u32, u32) = (1600, 900);
        let _env_lock = tilo_test_env::lock();
        let store = crate::shell::foreign::store();
        let pixels = [10, 20, 30, 255].repeat(SIZE.0.saturating_mul(SIZE.1) as usize);
        store.publish_rgba(FULLSCREEN, &pixels, SIZE.0, SIZE.1);
        // Its tile: the configure it last received, and so its extent.
        store.note_placement(FULLSCREEN, Rect::new(780.0, 70.0, 1656.0, 620.0));
        assert!(store.take_pending_configure(FULLSCREEN).is_some());
        let output = Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1));
        let mut fb = FrameBuilder::new(SIZE);

        // The overlay's configure, as `reconcile_fullscreen_overlay` sends it.
        store.note_fullscreen_extent(FULLSCREEN, (1600, 900));
        fb.set_fullscreen_surface(Some((FULLSCREEN, "Fullscreen".to_owned(), output)));
        let frame = fb.paint_with_damage();
        let candidate = frame
            .scanout_candidate()
            .expect("alone on top: no bar or strip chrome over it");
        assert_eq!(candidate.origin, (0, 0));
        assert!(candidate.covers((0, 0), SIZE), "it fills the output");
        assert_eq!(
            store.surface_origin(FULLSCREEN, output, (0.0, 0.0)),
            (0.0, 0.0),
            "input addresses the output's corner"
        );
        fb.set_fullscreen_surface(None);
        store.forget(FULLSCREEN);
    }

    fn key_press(key: Key) -> WindowEvent {
        let event = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn the_input_source_chord_switches_remembers_and_shows_the_layout() {
        let config = tempfile::tempdir().expect("isolated settings directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        let mut values = serde_json::Map::new();
        values.insert("keyboard_layout".into(), "us".into());
        values.insert("keyboard_layouts".into(), serde_json::json!(["us", "de"]));
        settings_store::update_section("input", values).expect("seed two sources");

        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        let chord = Modifiers::from(ModifiersState::SUPER | ModifiersState::ALT);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut focus = None;
        let press = key_press(Key::Named(NamedKey::Space));
        fb.dispatch_event(
            &press,
            &chord,
            &mut ui::widget::Edits::lend(&mut doc),
            &root,
            &mut focus,
        );

        assert_eq!(
            fb.chrome.borrow_mut().take_input_source_request(),
            Some(1),
            "the seat and the physical keyboard switch at once"
        );
        assert_eq!(
            settings_store::load_section("input")
                .get("keyboard_layout")
                .and_then(serde_json::Value::as_str),
            Some("de"),
            "the choice is persisted, so a settings reload keeps it"
        );
        let caption = fb.chrome().osd.map(|osd| osd.caption());
        assert!(
            matches!(caption.as_deref(), Some("German" | "de")),
            "the readout names the layout: {caption:?}"
        );
        assert!(
            !fb.host().palette_visible(),
            "Super+Alt+Space is not the Super+Space search chord"
        );

        fb.dispatch_event(
            &press,
            &chord,
            &mut ui::widget::Edits::lend(&mut doc),
            &root,
            &mut focus,
        );
        assert_eq!(
            fb.chrome.borrow_mut().take_input_source_request(),
            Some(0),
            "the sources wrap around"
        );
    }

    #[test]
    fn palette_text_input_is_exposed_without_a_native_focus_node() {
        let mut fb = FrameBuilder::new((1600, 900));
        let mut doc = AutoCommit::new();
        let mut focus = None;
        assert!(fb.focused_ime_field(focus).is_none());
        assert!(fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Space)),
            &Modifiers::from(ModifiersState::SUPER),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        let _ = fb.paint_settled();
        assert!(fb.host().palette_visible());
        let field = fb
            .focused_ime_field(None)
            .expect("the open palette owns the input method even without a native focus node");
        assert_eq!(field.content, ui::ime::InputContent::TEXT);
        assert!(field.width > 0 && field.height > 0);
        fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Escape)),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        );
        assert!(!fb.host().palette_visible());
        assert!(fb.focused_ime_field(None).is_none());
    }

    fn query_bounds(node: &ui::a11y::AccessNode) -> Option<ui::a11y::AccessBounds> {
        if node.action_target.as_deref() == Some("command-palette:query") {
            return node.bounds;
        }
        node.children.iter().find_map(query_bounds)
    }

    #[test]
    fn search_query_target_is_the_editable_row_and_disappears_on_close() {
        let mut fb = FrameBuilder::new((1920, 1080));
        let mut doc = AutoCommit::new();
        let mut focus = None;
        let _ = fb.paint_settled();
        assert!(!fb.widget_registry().contains_key("command-palette:query"));
        assert!(fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Space)),
            &Modifiers::from(ModifiersState::SUPER),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        let _ = fb.paint_settled();
        let targets = fb.widget_registry();
        let mut names: Vec<_> = targets.keys().collect();
        names.sort();
        println!("open Search targets ({}): {names:?}", targets.len());
        let query = *targets
            .get("command-palette:query")
            .expect("open Search must publish its editable query row, not just its card");
        let tree = fb
            .host()
            .accessibility()
            .expect("Search accessibility tree");
        assert_eq!(query_bounds(&tree), Some(query.into()));
        assert!(query.area() > 0.0, "the target is a visible editable row");
        let card = fb.host().palette_rect_for_tests();
        assert!(card.contains(query.center()));
        assert!(query.height() < card.height());
        assert!(fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Escape)),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        let _ = fb.paint_settled();
        assert!(!fb.widget_registry().contains_key("command-palette:query"));
    }

    fn focused_name(fb: &FrameBuilder, focus: Option<NodeId>) -> Option<String> {
        let ptr = fb.node_map.get(&focus?)?;
        unsafe { (**ptr).accessibility()?.name }
    }

    fn exported_node_named(
        update: &ui::accesskit::TreeUpdate,
        name: &str,
    ) -> Option<(ui::accesskit::NodeId, ui::accesskit::Node)> {
        update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(name))
            .cloned()
    }

    fn axis_a11y_tree() -> (
        crate::shell::state::WorkspaceTree,
        crate::shell::state::StripId,
        Vec<crate::shell::state::GroupId>,
        Vec<[u64; 2]>,
    ) {
        use crate::shell::state::{
            AppKind, FreshId as _, GroupId, Strip, StripId, Tab, TileContent, WindowGroup,
            WindowWidth, Workspace, WorkspaceId, WorkspaceTree,
        };
        const GROUPS: usize = 5;
        const GROUP_WIDTH_PERMILLE: u32 = 515;
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let groups: Vec<GroupId> = (0..GROUPS)
            .map(|i| 1001u64.saturating_add(i as u64))
            .collect();
        let tabs: Vec<[u64; 2]> = (0..GROUPS)
            .map(|i| {
                let base = (i as u64).saturating_add(1).saturating_mul(10);
                [base.saturating_add(1), base.saturating_add(2)]
            })
            .collect();

        let mk_tab = |id: u64, label: &str| Tab {
            id,
            title: label.into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Test".into()),
                label: label.into(),
            },
            app_title: None,
        };

        let window_groups = groups
            .iter()
            .zip(tabs.iter())
            .enumerate()
            .map(|(i, (&gid, ids))| WindowGroup {
                id: gid,
                tabs: vec![
                    mk_tab(ids[0], &format!("G{i}-A")),
                    mk_tab(ids[1], &format!("G{i}-B")),
                ],
                active_tab: ids[0],
                width: WindowWidth::Fraction(GROUP_WIDTH_PERMILLE),
                maximized: false,
                focus_history: Vec::new(),
            })
            .collect();

        let ws = {
            let mut workspace = Workspace::new(ws_id, "Test");
            workspace.strips = vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups,
            }];
            workspace
        };

        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = HashMap::new();
        active_group.insert(strip_id, groups[0]);

        let tree = WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group,
                collaborators: vec![],
                ..Default::default()
            },
            ..Default::default()
        };
        (tree, strip_id, groups, tabs)
    }

    #[test]
    fn axis_a11y_tab_activation_commits_selection_and_focus_before_resync() {
        for enabled in [false, true] {
            let (tree, strip, groups, tabs) = axis_a11y_tree();
            let mut fb = FrameBuilder::with_tree(tree, (1280, 800));
            fb.with_chrome(|chrome| chrome.set_axis_navigation_enabled(enabled));
            fb.host_mut().run_requested_shortcut_actions();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut focus = None;
            fb.paint_settled();
            for (label, group, tab) in [
                ("G4-A", groups[4], tabs[4][0]),
                ("G0-B", groups[0], tabs[0][1]),
            ] {
                let update = fb.build_a11y_tree_update(focus, 1.0);
                let (target, node) = exported_node_named(&update, label).expect("tab exported");
                assert_eq!(node.role(), ui::accesskit::Role::Tab);
                assert!(node.supports_action(ui::accesskit::Action::Click));
                assert!(fb.dispatch_a11y_action(
                    target,
                    &ui::a11y::Action::Activate,
                    &mut ui::widget::Edits::lend(&mut doc),
                    &root,
                    &mut focus
                ));
                assert_eq!(
                    fb.chrome().tree().desktop.active_group.get(&strip),
                    Some(&group),
                    "activation stamps spatial focus before another event; mode={enabled}"
                );
                fb.pump_apps(&mut doc, &root);
                fb.paint_settled();
                fb.paint_settled();
                let chrome = fb.chrome();
                let tree = chrome.tree();
                let selected = tree.desktop.active_workspace().unwrap().strips[0]
                    .window_groups
                    .iter()
                    .find(|candidate| candidate.id == group)
                    .unwrap()
                    .active_tab;
                assert_eq!(
                    selected, tab,
                    "model resync must retain assistive selection; mode={enabled}"
                );
                drop(tree);
                drop(chrome);
                let update = fb.build_a11y_tree_update(focus, 1.0);
                let (_, node) = exported_node_named(&update, label).expect("same tab exported");
                assert_eq!(node.is_selected(), Some(true));
                let focused = update
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == update.focus)
                    .expect("exported focus");
                assert_eq!(focused.1.role(), ui::accesskit::Role::TabList);
                assert_eq!(
                    focused.1.value(),
                    Some(label),
                    "assistive focus follows the activated tab bar"
                );
                if enabled {
                    let row = fb
                        .host_mut()
                        .group_row_mut_for_tests(strip)
                        .expect("live row");
                    let rect = row
                        .groups
                        .iter()
                        .find(|candidate| candidate.id == group)
                        .unwrap()
                        .get_rect();
                    let (x0, x1) = row.visible_canvas_span();
                    let scroll = row.scroll_x();
                    assert!(
                        rect.x0 >= x0 - scroll - 0.5 && rect.x1 <= x1 - scroll + 0.5,
                        "assistive activation gathers axis focus"
                    );
                }
            }
        }
    }

    /// The desktop's wallpaper is fitted to each display the compositor
    /// reports, the laptop panel and the monitor beside it, and follows a
    /// display that goes away.
    #[test]
    fn the_wallpaper_is_fitted_to_each_display() {
        use crate::shell::overlay_host::OutputGeometry;
        let (tree, _, _, _) = axis_a11y_tree();
        let mut fb = FrameBuilder::with_tree(tree, (3392, 1080));
        let mut panel = OutputGeometry::unscaled(0.0, 0.0, 1472.0, 920.0).with_id(1);
        panel.scale = 2.0;
        panel.scene = Rect::new(0.0, 0.0, 2944.0, 1840.0);
        let mut monitor = OutputGeometry::unscaled(1472.0, 0.0, 1920.0, 1080.0).with_id(2);
        monitor.scene = Rect::new(2944.0, 0.0, 4864.0, 1080.0);
        fb.set_output_geometry(vec![panel.clone(), monitor.clone()]);
        let mut doc = AutoCommit::new();
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.paint_settled();
        assert_eq!(
            fb.host().wallpaper_displays(),
            vec![panel.rect(), monitor.rect()]
        );
        fb.set_output_geometry(vec![panel.clone()]);
        fb.paint_settled();
        assert_eq!(fb.host().wallpaper_displays(), vec![panel.rect()]);
    }

    #[test]
    fn axis_a11y_tab_activation_follows_its_display_and_gathers_only_that_lane() {
        use crate::shell::overlay_host::OutputGeometry;
        let (tree, strip, groups, _) = axis_a11y_tree();
        let mut fb = FrameBuilder::with_tree(tree, (1280, 800));
        let first = OutputGeometry::unscaled(0.0, 0.0, 640.0, 800.0).with_id(10);
        let mut second = OutputGeometry::unscaled(640.0, 0.0, 640.0, 800.0).with_id(20);
        second.scale = 1.5;
        second.scene = Rect::new(640.0, 0.0, 1600.0, 1200.0);
        fb.set_output_geometry(vec![first, second]);
        fb.with_chrome(|chrome| {
            chrome.set_axis_navigation_enabled(true);
            let mut tree = chrome.tree_mut();
            let workspaces = tree.desktop.workspaces.clone();
            for (index, group) in groups.iter().enumerate() {
                tree.desktop.outputs.move_group(
                    &workspaces,
                    *group,
                    if index == 1 || index == 3 { 20 } else { 10 },
                );
            }
            tree.desktop.outputs.set_focused(10);
        });
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut focus = None;
        fb.pump_apps(&mut doc, &root);
        fb.paint_settled();
        fb.paint_settled();
        let other_pan = fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .unwrap()
            .output_pan(10)
            .unwrap()
            .scroll_x();
        let update = fb.build_a11y_tree_update(focus, 1.0);
        let (target, _) = exported_node_named(&update, "G3-B").expect("other display tab exported");
        assert!(fb.dispatch_a11y_action(
            target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &root,
            &mut focus
        ));
        assert_eq!(
            fb.chrome().tree().desktop.active_group.get(&strip),
            Some(&groups[3])
        );
        assert_eq!(
            fb.chrome().tree().desktop.outputs.focused_display(),
            Some(20)
        );
        fb.pump_apps(&mut doc, &root);
        fb.paint_settled();
        fb.paint_settled();
        let row = fb.host_mut().group_row_mut_for_tests(strip).unwrap();
        assert_eq!(
            row.output_pan(10).unwrap().scroll_x(),
            other_pan,
            "assistive gather leaves the other display's pan alone"
        );
        let view = row.output_pan(20).unwrap().viewport();
        let rect = row
            .groups
            .iter()
            .find(|candidate| candidate.id == groups[3])
            .unwrap()
            .get_rect();
        assert!(rect.x0 >= view.x0 - 0.5 && rect.x1 <= view.x1 + 0.5);
        let update = fb.build_a11y_tree_update(focus, 1.0);
        let (_, selected) = exported_node_named(&update, "G3-B").unwrap();
        assert_eq!(selected.is_selected(), Some(true));
        let (_, focused) = update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .unwrap();
        assert_eq!(focused.value(), Some("G3-B"));
    }

    /// Two displays side by side, the primary first, and two strips: the
    /// axis tree's, its groups alternating between the displays, and one
    /// below with group 2001 on the primary and 2002 on the second. Returns
    /// the builder, both strips and the first strip's groups.
    fn two_display_two_strip_desktop() -> (
        FrameBuilder,
        crate::shell::state::StripId,
        crate::shell::state::StripId,
        Vec<crate::shell::state::GroupId>,
    ) {
        use crate::shell::overlay_host::OutputGeometry;
        use crate::shell::state::{FreshId as _, Strip, StripId};
        let (mut tree, strip, groups, _) = axis_a11y_tree();
        // A second strip below, with a window on each display.
        let below = StripId::new();
        let mut below_groups = tree.desktop.workspaces[0].strips[0].window_groups[..2].to_vec();
        for (index, group) in below_groups.iter_mut().enumerate() {
            let offset = u64::try_from(index).unwrap_or(0);
            group.id = 2001_u64.saturating_add(offset);
            for tab in &mut group.tabs {
                tab.id = tab.id.saturating_add(1000);
            }
            group.active_tab = group.tabs[0].id;
        }
        let below_ids: Vec<u64> = below_groups.iter().map(|group| group.id).collect();
        tree.desktop.workspaces[0].strips.push(Strip {
            id: below,
            name: "Below".into(),
            window_groups: below_groups,
        });
        let mut fb = FrameBuilder::with_tree(tree, (2944, 1080));
        fb.set_output_geometry(vec![
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0).with_id(0),
            OutputGeometry::unscaled(1920.0, 0.0, 1024.0, 640.0).with_id(1),
        ]);
        fb.with_chrome(|chrome| {
            let mut tree = chrome.tree_mut();
            let workspaces = tree.desktop.workspaces.clone();
            for (index, group) in groups.iter().chain(&below_ids).enumerate() {
                let display = if *group >= 2001 {
                    u32::from(*group == 2002)
                } else {
                    u32::from(index % 2 == 1)
                };
                tree.desktop
                    .outputs
                    .move_group(&workspaces, *group, display);
            }
            tree.desktop.outputs.set_focused(0);
        });
        (fb, strip, below, groups)
    }

    /// Paint 120 frames on a pinned clock a frame interval apart, so every
    /// rect read afterwards is a settled layout, not a frame of motion.
    fn settle_on_pinned_clock(fb: &mut FrameBuilder, at: &mut ui::time::Instant) {
        for _ in 0..120 {
            *at = at.checked_add(ui::time::Duration::from_millis(16)).unwrap();
            ui::time::testing::with_monotonic_time(*at, || {
                let _ = fb.paint_settled();
            });
        }
    }

    /// Where `group` of the strip `row` is laid out.
    fn row_group_rect(
        fb: &mut FrameBuilder,
        row: crate::shell::state::StripId,
        group: u64,
    ) -> Rect {
        fb.host_mut()
            .group_row_mut_for_tests(row)
            .expect("live row")
            .groups
            .iter()
            .find(|candidate| candidate.id == group)
            .expect("group in the row")
            .get_rect()
    }

    /// Whether the second display's compact top bar is shown.
    fn compact_bar_shown(fb: &FrameBuilder) -> bool {
        fb.widget_registry()
            .keys()
            .any(|key| key.ends_with("topbar_compact[1]"))
    }

    /// On a desktop spread across two displays a fullscreen covers its own
    /// display only. The other display keeps its windows where they were
    /// and its own chrome: the full top bar on the primary,
    /// the compact bar on the second.
    #[test]
    fn a_fullscreen_covers_its_own_display_and_leaves_the_other_alone() {
        let (mut fb, strip, below, groups) = two_display_two_strip_desktop();
        let mut at = ui::time::now();
        let mut settle = |fb: &mut FrameBuilder| settle_on_pinned_clock(fb, &mut at);
        let rect_of = |fb: &mut FrameBuilder, group: u64| {
            row_group_rect(fb, if group >= 2001 { below } else { strip }, group)
        };
        settle(&mut fb);
        assert!(compact_bar_shown(&fb), "the second display has its bar");
        // groups[0] and 2001 live on the primary display, groups[1] and
        // 2002 on the second.
        for (covered, kept, kept_below, hidden_below, display) in [
            (
                groups[1],
                groups[0],
                2001,
                2002,
                Rect::new(1920.0, 0.0, 2944.0, 640.0),
            ),
            (
                groups[0],
                groups[1],
                2002,
                2001,
                Rect::new(0.0, 0.0, 1920.0, 1080.0),
            ),
        ] {
            let on_primary = covered == groups[0];
            let label = if on_primary { "primary" } else { "second" };
            let before = rect_of(&mut fb, kept);
            // Each strip is a full-height screen, one shown at a time: the
            // strip below keeps its window laid out on the other display,
            // where the strip deck has it off screen.
            assert!(before.width() > 1.0, "{label}: the kept window is shown");
            assert!(
                rect_of(&mut fb, kept_below).width() > 1.0,
                "{label}: the other strip's window is laid out"
            );
            assert!(
                fb.host().strip_row_rect(below).is_none(),
                "{label}: the strip deck has the other strip off screen"
            );
            fb.with_chrome(|chrome| chrome.set_user_fullscreen_group(Some(covered)));
            fb.set_fullscreen(None);
            settle(&mut fb);
            let fullscreen = rect_of(&mut fb, covered);
            assert!(
                (fullscreen.x0 - display.x0).abs() < 0.5
                    && (fullscreen.y0 - display.y0).abs() < 0.5
                    && (fullscreen.width() - display.width()).abs() < 0.5
                    && (fullscreen.height() - display.height()).abs() < 0.5,
                "{label}: the fullscreen window covers its display {display:?}, not {fullscreen:?}"
            );
            assert_eq!(
                rect_of(&mut fb, kept),
                before,
                "{label}: the other display keeps its window where it was"
            );
            assert!(
                rect_of(&mut fb, kept_below).width() > 1.0
                    && fb.host().strip_row_rect(below).is_none(),
                "{label}: the other strip keeps its window on the other display, off screen"
            );
            assert!(
                rect_of(&mut fb, hidden_below).width() <= 0.5,
                "{label}: the other strip steps aside on the fullscreen's display"
            );
            let registry = fb.widget_registry();
            assert_eq!(
                rect_named(&registry, "TopBarChrome").height() > 10.0,
                !on_primary,
                "{label}: the full top bar steps aside only for a fullscreen on the primary"
            );
            assert_eq!(
                compact_bar_shown(&fb),
                on_primary,
                "{label}: the compact bar steps aside only for a fullscreen on its display"
            );
            fb.with_chrome(|chrome| chrome.set_user_fullscreen_group(None));
            settle(&mut fb);
            assert_eq!(
                rect_of(&mut fb, kept),
                before,
                "{label}: leaving fullscreen keeps the other display as it was"
            );
            assert!(compact_bar_shown(&fb), "{label}: the compact bar is back");
            assert!(
                rect_of(&mut fb, hidden_below).width() > 1.0,
                "{label}: the other strip's window is back"
            );
        }
    }

    #[test]
    fn axis_a11y_media_control_preserves_spatial_and_native_focus() {
        let (tree, strip, groups, tabs) = axis_a11y_tree();
        let mut fb = FrameBuilder::with_tree(tree, (1280, 800));
        fb.with_chrome(|chrome| chrome.set_axis_navigation_enabled(true));
        fb.host_mut().run_requested_shortcut_actions();
        fb.paint_settled();
        let mut focus = None;
        fb.focus_first(&mut focus);
        let before = focus;
        fb.host_mut()
            .group_row_mut_for_tests(strip)
            .unwrap()
            .groups
            .iter_mut()
            .find(|group| group.id == groups[4])
            .unwrap()
            .bar_mut_for_tests()
            .set_mark(tabs[4][0], Some(ui::widget::TabMark::Playing));
        let update = fb.build_a11y_tree_update(focus, 1.0);
        let (target, node) =
            exported_node_named(&update, "Mute G4-A").expect("new media control exported");
        assert_eq!(node.role(), ui::accesskit::Role::Button);
        let mut doc = AutoCommit::new();
        assert!(fb.dispatch_a11y_action(
            target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus
        ));
        assert_eq!(
            fb.chrome().tree().desktop.active_group.get(&strip),
            Some(&groups[0])
        );
        assert_eq!(focus, before, "mute is a media action, not tab selection");
        assert!(!fb.chrome.borrow_mut().take_axis_gather_request());
        let intent = fb
            .host_mut()
            .group_row_mut_for_tests(strip)
            .unwrap()
            .groups
            .iter_mut()
            .find(|group| group.id == groups[4])
            .unwrap()
            .bar_mut_for_tests()
            .take_mark_click();
        assert_eq!(intent, Some((tabs[4][0], ui::widget::TabMark::Playing)));
    }

    #[test]
    fn an_open_palette_reaches_assistive_technology_once() {
        // The host publishes the palette (with its filter band) itself; the
        // palette widget must not be exported a second time, or a screen
        // reader sees two palettes and may type into the hidden one.
        let mut fb = FrameBuilder::new((1280, 800));
        fb.host_mut().set_palette_visible(true);
        fb.host_mut().set_palette_focus(true);
        fb.host_mut().force_palette_settled();
        fb.relayout();
        let update = fb.build_a11y_tree_update(None, 1.0);
        let named = |name: &str| {
            update
                .nodes
                .iter()
                .filter(|(_, node)| node.label() == Some(name))
                .count()
        };
        assert_eq!(named("Command palette"), 1, "one palette");
        assert_eq!(named("Search filters"), 1, "its filter band, once");
        let queries = update
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == ui::accesskit::Role::TextInput
                    && node.supports_action(ui::accesskit::Action::SetValue)
            })
            .count();
        assert_eq!(queries, 1, "one editable query");
    }

    #[test]
    fn assistive_technology_sets_the_open_palettes_query() {
        // The palette is published inside the host's tree, so setting its
        // query from outside reaches the host first; it must still be typed.
        let mut fb = FrameBuilder::new((1280, 800));
        fb.host_mut().set_palette_visible(true);
        fb.host_mut().set_palette_focus(true);
        fb.host_mut().force_palette_settled();
        fb.relayout();
        let update = fb.build_a11y_tree_update(None, 1.0);
        let (query_id, _) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == ui::accesskit::Role::TextInput
                    && node.supports_action(ui::accesskit::Action::SetValue)
            })
            .cloned()
            .expect("the open palette publishes its query");
        let mut doc = AutoCommit::new();
        let mut focus = None;
        assert!(fb.dispatch_a11y_action(
            query_id,
            &ui::a11y::Action::SetValue("photos".into()),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(fb.host().palette_query(), "photos");
    }

    fn onboarding_frame() -> (FrameBuilder, tempfile::TempDir) {
        let (fb, identity_dir, _press) = onboarding_frame_with_presser();
        (fb, identity_dir)
    }

    /// An onboarding frame plus a way to take a setup action as its button
    /// would, for tests that walk past a step rather than exercise it.
    fn onboarding_frame_with_presser()
    -> (FrameBuilder, tempfile::TempDir, Box<dyn Fn(&'static str)>) {
        let identity_dir = tempfile::tempdir().expect("temporary identity directory");
        let mut fb = FrameBuilder::new((1280, 800));
        let identity_path = identity_dir.path().join("primary.json");
        let view = crate::onboarding_view::OnboardingView::with_minter(Box::new(move || {
            Ok(crate::onboarding::MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: identity_path.clone(),
            })
        }));
        let press = view.presser();
        fb.enable_test_onboarding(view);
        fb.relayout();
        (fb, identity_dir, press)
    }

    #[test]
    fn administrator_prompt_raised_during_onboarding_is_drawn_above_it_and_takes_the_keyboard() {
        use crate::shell::portal_dialog::{
            AuthenticationDecision, AuthenticationRequest, PortalDialogQueue,
        };

        let (mut fb, _identity_dir) = onboarding_frame();
        let mut focus = None;
        let mut doc = AutoCommit::new();
        fb.reconcile_native_focus(&mut focus);
        let onboarding_focus = focused_name(&fb, focus);
        assert!(onboarding_focus.is_some(), "onboarding holds focus");

        // The polkit agent enqueues on the session queue while first-use
        // setup is still on screen.
        let queue = PortalDialogQueue::default();
        let mut receiver = queue
            .enqueue_authentication(AuthenticationRequest {
                handle: "polkit:onboarding".into(),
                message: "Administrator approval is required".into(),
                action_id: "org.tilo.example".into(),
                identities: vec!["quinn".into()],
                identity: 0,
                prompt: "Password:".into(),
                echo: false,
                notice: None,
            })
            .expect("queued");
        fb.host_mut().set_portal_dialog_queue(queue);
        assert!(fb.host().onboarding_active());

        // The ordinary onboarding tick must bring the sheet up.
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        let state = fb.host().portal_dialog_state_json();
        assert_eq!(state["kind"], "authentication", "sheet is up: {state}");
        assert!(
            fb.widget_registry()
                .contains_key(crate::shell::trusted_ui::PORTAL_DIALOG_TARGET),
            "the sheet is on screen above onboarding"
        );
        let _ = fb.paint();

        // Typed keys reach the sheet's secret field, not the focused setup
        // control underneath.
        for key in ["h", "u", "n", "t"] {
            let mut typed = key_press(Key::Character(key.into()));
            if let WindowEvent::KeyboardInput { event, .. } = &mut typed {
                event.text = Some(key.into());
            }
            assert!(fb.dispatch_event(
                &typed,
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
        }
        assert_eq!(fb.host().portal_dialog_state_json()["secret_len"], 4);
        assert!(fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Enter)),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert_eq!(
            receiver.try_recv(),
            Ok(AuthenticationDecision::Answered {
                identity: 0,
                secret: "hunt".into(),
            })
        );

        // Answered: the sheet is gone and onboarding keeps its focus.
        fb.pump_apps(&mut doc, &automerge::ROOT);
        fb.relayout();
        assert!(!fb.host().portal_dialog_open());
        assert!(fb.host().onboarding_active());
        fb.reconcile_native_focus(&mut focus);
        assert_eq!(focused_name(&fb, focus), onboarding_focus);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_oobe_accessibility_tree_owns_focus_roles_and_editable_text() {
        fn native_oobe_initializes_focus_without_test_control() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            fb.reconcile_native_focus(&mut focus);
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Use my existing Tilo identity")
            );
        }

        fn native_oobe_exports_named_roles_state_and_focus_to_accesskit() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            fb.reconcile_native_focus(&mut focus);

            let update = fb.build_a11y_tree_update(focus, 1.0);
            let (existing_id, existing) =
                exported_node_named(&update, "Use my existing Tilo identity")
                    .expect("existing-identity action reaches the external tree");
            let (_, create) = exported_node_named(&update, "Create a new identity")
                .expect("create-identity action reaches the external tree");
            // Identity setup has distinct backup and final hand-off states, so
            // its semantic progress surface contains four stages.
            let (_, progress) = exported_node_named(&update, "Identity setup, step 1 of 4: Choose")
                .expect("the OOBE progress state reaches the external tree");

            // The ways forward are one list of rows that each open a flow.
            assert_eq!(existing.role(), ui::accesskit::Role::ListItem);
            assert_eq!(create.role(), ui::accesskit::Role::ListItem);
            assert_eq!(progress.role(), ui::accesskit::Role::ProgressIndicator);
            assert!(existing.supports_action(ui::accesskit::Action::Focus));
            assert!(existing.supports_action(ui::accesskit::Action::Click));
            assert_eq!(update.focus, existing_id);
            assert_eq!(
                update.tree.as_ref().map(|tree| tree.root),
                Some(ui::accesskit_bridge::ROOT_ID)
            );
        }

        fn native_oobe_routes_external_focus_and_editable_text_to_the_live_widget() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            fb.reconcile_native_focus(&mut focus);

            let intro = fb.build_a11y_tree_update(focus, 1.0);
            let (existing_id, _) = exported_node_named(&intro, "Use my existing Tilo identity")
                .expect("the external tree exposes the Connect-existing action");
            assert!(fb.dispatch_a11y_action(
                existing_id,
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();

            let choice = fb.build_a11y_tree_update(focus, 1.0);
            let (restore_id, _) = exported_node_named(&choice, "Restore from a backup")
                .expect("the use-existing choice offers its restore flow");
            assert!(fb.dispatch_a11y_action(
                restore_id,
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();

            let connect = fb.build_a11y_tree_update(focus, 1.0);
            let (field_id, field) = exported_node_named(&connect, "Identity backup file")
                .expect("the non-secret identity-path field reaches the external tree");
            assert_eq!(field.role(), ui::accesskit::Role::TextInput);
            assert!(field.supports_action(ui::accesskit::Action::Focus));
            assert!(field.supports_action(ui::accesskit::Action::SetValue));
            assert_eq!(field.value(), Some(""));

            assert!(fb.dispatch_a11y_action(
                field_id,
                &ui::a11y::Action::Focus,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Identity backup file"),
                "an external focus request must update the compositor's real focus cursor"
            );

            let probe = "tilo-atspi-edit-probe";
            assert!(fb.dispatch_a11y_action(
                field_id,
                &ui::a11y::Action::SetValue(probe.into()),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();

            let updated = fb.build_a11y_tree_update(focus, 1.0);
            let (_, field) = exported_node_named(&updated, "Identity backup file")
                .expect("the editable field remains externally addressable after its update");
            assert_eq!(field.value(), Some(probe));
        }

        fn native_oobe_external_tree_replaces_step_and_updates_control_state() {
            let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            fb.reconcile_native_focus(&mut focus);
            let intro = fb.build_a11y_tree_update(focus, 1.0);

            press(crate::onboarding_view::KEY_BEGIN);
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);
            let protect = fb.build_a11y_tree_update(focus, 1.0);

            assert!(
                exported_node_named(&protect, "Use my existing Tilo identity").is_none(),
                "nodes from the replaced step must leave the exported tree"
            );
            assert!(
                exported_node_named(&intro, &crate::onboarding::acknowledgement()).is_none(),
                "the next step must not leak into the initial tree"
            );
            let (ack_id, acknowledgement) =
                exported_node_named(&protect, &crate::onboarding::acknowledgement())
                    .expect("the recovery acknowledgement reaches the external tree");
            let (_, create) = exported_node_named(&protect, "Create local identity")
                .expect("the gated create action reaches the external tree");
            assert_eq!(acknowledgement.role(), ui::accesskit::Role::CheckBox);
            assert_eq!(
                acknowledgement.toggled(),
                Some(ui::accesskit::Toggled::False)
            );
            assert!(create.is_disabled());
            assert_eq!(protect.focus, ack_id);

            assert!(fb.dispatch_a11y_action(
                ack_id,
                &ui::a11y::Action::ToggleCheck,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            // The first pump observes the parent flow change; layout rebuilds the
            // composition just as the production frame loop does before its next
            // refresh. The second pump then syncs the newly-created checkbox from
            // the CRDT. Omitting the intervening layout would test two reducer
            // passes against the old tree, which cannot occur in the live loop.
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);
            let acknowledged = fb.build_a11y_tree_update(focus, 1.0);
            let (_, acknowledgement) =
                exported_node_named(&acknowledged, &crate::onboarding::acknowledgement()).unwrap();
            let (_, create) = exported_node_named(&acknowledged, "Create local identity").unwrap();
            assert_eq!(
                acknowledgement.toggled(),
                Some(ui::accesskit::Toggled::True),
                "the rebuilt checkbox must preserve its visible and spoken state"
            );
            assert!(
                !create.is_disabled(),
                "the externally-operated acknowledgement must enable the gated action"
            );
        }

        native_oobe_initializes_focus_without_test_control();
        native_oobe_exports_named_roles_state_and_focus_to_accesskit();
        native_oobe_routes_external_focus_and_editable_text_to_the_live_widget();
        native_oobe_external_tree_replaces_step_and_updates_control_state();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_oobe_keyboard_order_activates_and_traverses_enabled_controls() {
        fn native_oobe_enter_activates_the_focused_button() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            let tab = key_press(Key::Named(NamedKey::Tab));

            assert!(fb.dispatch_event(
                &tab,
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Create a new identity")
            );
            assert!(fb.dispatch_event(
                &key_press(Key::Named(NamedKey::Enter)),
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                doc.get(&automerge::ROOT, crate::onboarding_view::KEY_BEGIN)
                    .expect("read begin action"),
                None,
                "activation is a view action, never a document write"
            );

            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);
            assert!(
                focused_name(&fb, focus).is_some_and(|name| name.contains("I understand")),
                "Enter must advance into the identity-protection step"
            );
        }

        fn native_oobe_backup_tab_order_reaches_fields_acknowledgement_and_continue() {
            let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            for key in [
                crate::onboarding_view::KEY_BEGIN,
                crate::onboarding_view::KEY_UNDERSTOOD,
            ] {
                if key == crate::onboarding_view::KEY_UNDERSTOOD {
                    doc.put(&automerge::ROOT, key, true).expect("advance OOBE");
                } else {
                    press(key);
                }
                fb.pump_apps(&mut doc, &automerge::ROOT);
                fb.relayout();
                // The replacement subtree must sync once before a gated control
                // can be activated, matching the production frame loop.
                fb.pump_apps(&mut doc, &automerge::ROOT);
                fb.relayout();
            }
            // A structural rebuild exposes the acknowledgement control before
            // its document value has been synchronized into the new view.
            doc.put(
                &automerge::ROOT,
                crate::onboarding_view::KEY_UNDERSTOOD,
                true,
            )
            .expect("acknowledge identity protection");
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            press(crate::onboarding_view::KEY_CREATE);
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);

            let names: Vec<_> = fb
                .focusable_order()
                .into_iter()
                .filter_map(|id| focused_name(&fb, Some(id)))
                .collect();
            assert!(
                names.iter().any(|name| name == "New backup passphrase"),
                "backup passphrase must be keyboard reachable: {names:?}"
            );
            assert!(
                names
                    .iter()
                    .any(|name| name == "Confirm new backup passphrase"),
                "backup confirmation must be keyboard reachable: {names:?}"
            );

            // A screen reader reaches the passphrase as password text it can
            // focus and type into: AccessKit's AT-SPI adapter offers Text and
            // EditableText only on a text input with a text run, so the field
            // carries one that never holds what was typed.
            // Its visible label shares the name, so pick the field by role.
            let passphrase_run = |update: &ui::accesskit::TreeUpdate| {
                let (id, field) = update
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.label() == Some("New backup passphrase")
                            && node.role() == ui::accesskit::Role::PasswordInput
                    })
                    .expect("the passphrase reaches AT-SPI as password text");
                assert!(field.supports_action(ui::accesskit::Action::Focus));
                assert!(field.supports_action(ui::accesskit::Action::SetValue));
                assert_eq!(field.value(), None);
                let runs: Vec<_> = update
                    .nodes
                    .iter()
                    .filter(|(node_id, node)| {
                        field.children().contains(node_id)
                            && node.role() == ui::accesskit::Role::TextRun
                    })
                    .collect();
                let [(_, run)] = runs.as_slice() else {
                    panic!("one text run among {:?}", field.children());
                };
                (*id, run.value().map(str::to_owned))
            };
            let (field, typed) = passphrase_run(&fb.build_a11y_tree_update(focus, 1.0));
            assert_eq!(typed.as_deref(), Some(""));
            assert!(fb.dispatch_a11y_action(
                field,
                &ui::a11y::Action::Focus,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert!(fb.dispatch_a11y_action(
                field,
                &ui::a11y::Action::SetValue("tilosecretprobe42".into()),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            fb.relayout();
            let update = fb.build_a11y_tree_update(focus, 1.0);
            assert_eq!(update.focus, field, "AT-SPI focus lands on the field");
            assert_eq!(passphrase_run(&update).1.as_deref(), Some(""));
            assert!(
                update.nodes.iter().all(|(_, node)| {
                    node.value()
                        .is_none_or(|value| !value.contains("tilosecretprobe42"))
                        && node
                            .label()
                            .is_none_or(|label| !label.contains("tilosecretprobe42"))
                }),
                "a typed passphrase never reaches assistive technology"
            );
            assert!(
                names
                    .iter()
                    .any(|name| name.contains("without a recovery key or backup")),
                "skip acknowledgement must be keyboard reachable: {names:?}"
            );

            let acknowledgement = fb
                .focusable_order()
                .into_iter()
                .find(|id| {
                    focused_name(&fb, Some(*id))
                        .is_some_and(|name| name.contains("without a recovery key or backup"))
                })
                .expect("acknowledgement focus target");
            fb.set_native_focus(&mut focus, Some(acknowledgement));
            let enter = key_press(Key::Named(NamedKey::Enter));
            assert!(fb.dispatch_event(
                &enter,
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);
            let tab = key_press(Key::Named(NamedKey::Tab));
            assert!(fb.dispatch_event(
                &tab,
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Continue without backup"),
                "acknowledging the risk must make the adjacent continuation action the next keyboard stop"
            );
        }

        fn native_oobe_tab_and_shift_tab_follow_dfs_order() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            let tab = key_press(Key::Named(NamedKey::Tab));

            assert!(fb.dispatch_event(
                &tab,
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Create a new identity")
            );

            let shift = Modifiers::from(ModifiersState::SHIFT);
            assert!(fb.dispatch_event(
                &tab,
                &shift,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Use my existing Tilo identity")
            );
        }

        fn native_oobe_focus_order_excludes_disabled_controls() {
            let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
            let mut doc = AutoCommit::new();
            press(crate::onboarding_view::KEY_BEGIN);
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();

            let names: Vec<_> = fb
                .focusable_order()
                .into_iter()
                .filter_map(|id| focused_name(&fb, Some(id)))
                .collect();
            assert!(names.iter().any(|name| name.contains("I understand")));
            assert!(names.iter().any(|name| name == "Back"));
            assert!(
                !names.iter().any(|name| name == "Create"),
                "the gated Create action must not become a Tab stop: {names:?}"
            );
        }

        fn focused_terminal_gets_plain_tab_before_focus_traversal() {
            let mut fb = FrameBuilder::new((1280, 800));
            fb.relayout();
            let terminal = fb.node_map.iter().find_map(|(id, widget)| {
                // SAFETY: node_map pointers are owned by the live widget tree and
                // remain valid until the next structural rebuild.
                is_terminal(unsafe { &**widget }).then_some(*id)
            });
            let Some(terminal) = terminal else {
                // Environments without a PTY deliberately mount the terminal
                // fallback instead; the routing rule remains covered by OOBE's
                // unclaimed-Tab traversal test above.
                return;
            };
            let mut focus = None;
            fb.set_native_focus(&mut focus, Some(terminal));
            let mut doc = AutoCommit::new();

            assert!(fb.dispatch_event(
                &key_press(Key::Named(NamedKey::Tab)),
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(focus, Some(terminal), "terminal Tab must not move focus");
        }

        native_oobe_enter_activates_the_focused_button();
        native_oobe_backup_tab_order_reaches_fields_acknowledgement_and_continue();
        native_oobe_tab_and_shift_tab_follow_dfs_order();
        native_oobe_focus_order_excludes_disabled_controls();
        focused_terminal_gets_plain_tab_before_focus_traversal();
    }

    #[test]
    fn native_terminal_focus_follows_the_active_owner_with_two_windows() {
        fn contains_pointer(widget: &dyn Widget, pointer: *const ()) -> bool {
            std::ptr::from_ref(widget).cast::<()>() == pointer
                || widget
                    .children()
                    .into_iter()
                    .any(|child| contains_pointer(child, pointer))
        }
        let (mut tree, strip, groups, tabs) = axis_a11y_tree();
        let workspace = tree.desktop.active_workspace_mut().expect("test workspace");
        workspace.strips[0].window_groups.truncate(2);
        for group in &mut workspace.strips[0].window_groups {
            group.tabs.truncate(1);
            group.tabs[0].tile = crate::shell::state::TileContent::AppPlaceholder {
                kind: crate::shell::state::AppKind::Terminal,
                label: "Terminal".into(),
            };
        }
        let mut frame = FrameBuilder::with_tree(tree, (3200, 900));
        frame.relayout();
        // Both real mounted owners remain in the live widget map. Visit both
        // through the production window focus door: choosing an arbitrary
        // HashMap Terminal cannot satisfy both owner assertions.
        for index in 0..2 {
            frame
                .host_mut()
                .focus_window_on_strip(strip, groups[index], tabs[index][0]);
            assert_eq!(frame.chrome().active_tab_id(), Some(tabs[index][0]));
            let owner = frame
                .host()
                .active_app_host()
                .expect("active mounted app owner");
            let target = frame
                .initial_focus_target(&frame.focusable_order())
                .expect("the active Terminal must have a native keyboard target");
            let pointer = *frame.node_map.get(&target).expect("live focus node");
            // SAFETY: no rebuild or mutation occurs while the live owner and
            // its node-map pointer are inspected.
            assert!(is_terminal(unsafe { &*pointer }));
            assert!(
                contains_pointer(owner, pointer.cast::<()>()),
                "native focus must belong to the active tab, never another Terminal"
            );
        }
    }

    #[test]
    fn native_terminal_typing_and_consent_follow_two_actual_mounted_owners() {
        let (mut tree, strip, groups, tabs) = axis_a11y_tree();
        let workspace = tree.desktop.active_workspace_mut().unwrap();
        workspace.strips[0].window_groups.truncate(2);
        for group in &mut workspace.strips[0].window_groups {
            group.tabs.truncate(1);
            group.tabs[0].tile = crate::shell::state::TileContent::AppPlaceholder {
                kind: crate::shell::state::AppKind::Terminal,
                label: "Terminal".into(),
            };
        }
        let mut frame = FrameBuilder::with_tree(tree, (3200, 900));
        frame.relayout();
        let probes = (0..2)
            .map(|index| {
                frame
                    .host_mut()
                    .focus_window_on_strip(strip, groups[index], tabs[index][0]);
                assert_eq!(frame.chrome().active_tab_id(), Some(tabs[index][0]));
                live_hosted_terminal_probe(&mut frame)
            })
            .collect::<Vec<_>>();
        let mut document = AutoCommit::new();
        let mut focus = None;
        for index in 0..2 {
            assert!(
                frame
                    .host_mut()
                    .focus_window_on_strip(strip, groups[index], tabs[index][0])
            );
            frame.relayout();
            for (key, expected) in [(NamedKey::Space, b' '), (NamedKey::Enter, b'\r')] {
                assert!(frame.dispatch_event(
                    &key_press(Key::Named(key)),
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut document),
                    &automerge::ROOT,
                    &mut focus
                ));
                assert_eq!(
                    tilo_test_env::recv("the probe's keys", &probes[index]),
                    vec![expected]
                );
                assert!(
                    matches!(
                        probes[1 - index].try_recv(),
                        Err(std::sync::mpsc::TryRecvError::Empty)
                    ),
                    "inactive Terminal must receive no bytes"
                );
            }
            frame.host_mut().dictation_surface_mut().consent();
            assert!(frame.dispatch_event(
                &key_press(Key::Named(NamedKey::Enter)),
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &mut focus
            ));
            assert!(matches!(
                frame.host_mut().dictation_surface_mut().take_request(),
                Some(crate::shell::overlay_host::dictation_layer::Request::Cancel(None))
            ));
            assert!(
                probes.iter().all(|probe| matches!(
                    probe.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                )),
                "consent must reach neither PTY"
            );
            // Acknowledge the observed cancellation as the frontend does.
            frame
                .host_mut()
                .dictation_surface_mut()
                .status(crate::compositor::dictation_status::DictationStatus::Idle);
        }
        for index in 0..2 {
            assert!(
                frame
                    .host_mut()
                    .focus_window_on_strip(strip, groups[index], tabs[index][0])
            );
            frame.relayout();
            finish_hosted_terminal_probe(&mut frame);
        }
    }

    #[test]
    fn native_terminal_missing_active_owner_refuses_the_typing_fast_path() {
        let (mut tree, strip, groups, tabs) = axis_a11y_tree();
        let workspace = tree.desktop.active_workspace_mut().unwrap();
        workspace.strips[0].window_groups.truncate(1);
        workspace.strips[0].window_groups[0].tabs.truncate(1);
        workspace.strips[0].window_groups[0].tabs[0].tile =
            crate::shell::state::TileContent::AppPlaceholder {
                kind: crate::shell::state::AppKind::Terminal,
                label: "Terminal".into(),
            };
        let mut frame = FrameBuilder::with_tree(tree, (1280, 900));
        frame.relayout();
        frame
            .host_mut()
            .focus_window_on_strip(strip, groups[0], tabs[0][0]);
        assert_eq!(frame.chrome().active_tab_id(), Some(tabs[0][0]));
        let probe = live_hosted_terminal_probe(&mut frame);
        let active = frame
            .chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_group
            .remove(&strip)
            .unwrap();
        assert!(
            frame.host.active_app_host().is_none(),
            "a live row cannot stand in for a missing active owner"
        );
        assert!(frame.active_terminal_node().is_none());
        let mut document = AutoCommit::new();
        let mut focus = None;
        for key in [NamedKey::Space, NamedKey::Enter] {
            assert!(!frame.offer_terminal_typing(
                &key_press(Key::Named(key)),
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &mut focus
            ));
            assert!(
                matches!(probe.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
                "the observable live PTY must receive no bytes"
            );
        }
        frame
            .chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_group
            .insert(strip, active);
        finish_hosted_terminal_probe(&mut frame);
    }

    #[test]
    fn dictation_consent_keys_do_not_reach_the_hosted_terminal_pty() {
        use crate::shell::state::{AppKind, TileContent};
        // Production chrome path: `FrameBuilder::dispatch_event`, the same
        // route tilo-control type/key and the DRM keyboard take once the
        // compositor has mapped the press.
        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        // W17 / window.new.terminal leaves the Terminal tile active. The
        // demo tree otherwise keeps Notes first, so chrome would still
        // steal initial focus. Switch through the live GroupRow so harvest
        // cannot write the Notes tab back over the tree.
        let target = {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .find(|tab| {
                                matches!(
                                    &tab.tile,
                                    TileContent::AppPlaceholder {
                                        kind: AppKind::Terminal,
                                        ..
                                    } | TileContent::Stub {
                                        kind: AppKind::Terminal,
                                        ..
                                    }
                                )
                            })
                            .map(|tab| (strip.id, group.id, tab.id))
                    })
                })
            })
        };
        let (strip, group, tab) = target.expect("production chrome must include a Terminal tab");
        assert!(
            fb.host_mut().focus_window_on_strip(strip, group, tab),
            "the compositor must be able to focus the hosted Terminal tab"
        );
        let probe = live_hosted_terminal_probe(&mut fb);
        let mut focus = None;
        fb.reconcile_native_focus(&mut focus);
        let focused = focus.expect("a terminal tile must take initial focus");
        assert!(
            // SAFETY: node_map pointers are owned by the live widget tree.
            fb.node_map
                .get(&focused)
                .is_some_and(|&widget| is_terminal(unsafe { &*widget })),
            "chrome must not keep initial focus on a workspace pill: {:?}",
            fb.widget_name(focused)
        );
        let mut doc = AutoCommit::new();
        // Use the real local consent widget and production event dispatch.
        // No recognizer or microphone exists in this keyboard-routing test.
        fb.host_mut().dictation_surface_mut().consent();
        assert!(fb.dispatch_event(
            &key_press(Key::Named(NamedKey::Enter)),
            &Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        ));
        assert!(
            matches!(
                fb.host_mut().dictation_surface_mut().take_request(),
                Some(crate::shell::overlay_host::dictation_layer::Request::Cancel(None))
            ),
            "Enter must select the prompt's conservative cancellation default"
        );
        assert!(
            probe.try_recv().is_err(),
            "consent Enter must never submit the shell line"
        );
        fb.host_mut().dictation_surface_mut().consent();
        for key in [NamedKey::ArrowLeft, NamedKey::Space] {
            assert!(fb.dispatch_event(
                &key_press(Key::Named(key)),
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
        }
        assert!(
            probe.try_recv().is_err(),
            "consent Space must never edit the shell line"
        );
        finish_hosted_terminal_probe(&mut fb);
    }

    #[test]
    fn control_socket_typing_reaches_the_hosted_terminal_pty() {
        use crate::shell::state::{AppKind, TileContent};
        use winit::keyboard::SmolStr;
        // Production chrome path: `FrameBuilder::dispatch_event`, the same
        // route tilo-control type/key and the DRM keyboard take once the
        // compositor has mapped the press.
        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        // W17 / window.new.terminal leaves the Terminal tile active. The
        // demo tree otherwise keeps Notes first, so chrome would still
        // steal initial focus. Switch through the live GroupRow so harvest
        // cannot write the Notes tab back over the tree.
        let target = {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .find(|tab| {
                                matches!(
                                    &tab.tile,
                                    TileContent::AppPlaceholder {
                                        kind: AppKind::Terminal,
                                        ..
                                    } | TileContent::Stub {
                                        kind: AppKind::Terminal,
                                        ..
                                    }
                                )
                            })
                            .map(|tab| (strip.id, group.id, tab.id))
                    })
                })
            })
        };
        let (strip, group, tab) = target.expect("production chrome must include a Terminal tab");
        assert!(
            fb.host_mut().focus_window_on_strip(strip, group, tab),
            "the compositor must be able to focus the hosted Terminal tab"
        );
        let probe = live_hosted_terminal_probe(&mut fb);
        let mut focus = None;
        fb.reconcile_native_focus(&mut focus);
        let focused = focus.expect("a terminal tile must take initial focus");
        assert!(
            // SAFETY: node_map pointers are owned by the live widget tree.
            fb.node_map
                .get(&focused)
                .is_some_and(|&widget| is_terminal(unsafe { &*widget })),
            "chrome must not keep initial focus on a workspace pill: {:?}",
            fb.widget_name(focused)
        );
        let mut doc = AutoCommit::new();
        for key in [
            Key::Character(SmolStr::new_static("e")),
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::Enter),
        ] {
            assert!(
                fb.dispatch_event(
                    &key_press(key),
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &mut focus,
                ),
                "typed keys must be consumed on the production chrome path"
            );
        }
        let mut got = Vec::new();
        while let Ok(chunk) = probe.try_recv() {
            got.extend(chunk);
        }
        assert_eq!(
            got.as_slice(),
            b"e \r",
            "letters, Space and Enter must reach the PTY, not chrome Activate",
        );
        finish_hosted_terminal_probe(&mut fb);
    }

    #[test]
    fn chrome_focus_does_not_steal_terminal_space_and_enter() {
        use crate::shell::state::{AppKind, TileContent};
        use winit::keyboard::SmolStr;
        // W17 / A01 on the live compositor: the workspace pill keeps
        // keyboard focus after the Terminal tile opens. Letters still
        // reach the PTY through tree dispatch; Named Space/Enter used
        // to Activate chrome (Homeoffice workspace settings).
        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        let target = {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .find(|tab| {
                                matches!(
                                    &tab.tile,
                                    TileContent::AppPlaceholder {
                                        kind: AppKind::Terminal,
                                        ..
                                    } | TileContent::Stub {
                                        kind: AppKind::Terminal,
                                        ..
                                    }
                                )
                            })
                            .map(|tab| (strip.id, group.id, tab.id))
                    })
                })
            })
        };
        let (strip, group, tab) = target.expect("production chrome must include a Terminal tab");
        assert!(fb.host_mut().focus_window_on_strip(strip, group, tab));
        let probe = live_hosted_terminal_probe(&mut fb);
        let mut focus = None;
        fb.reconcile_native_focus(&mut focus);
        let chrome_id = fb.node_map.iter().find_map(|(id, widget)| {
            let name = unsafe { (**widget).name() };
            (name == "TopBarChrome").then_some(*id)
        });
        let chrome_id = chrome_id.expect("production chrome must mount the top bar");
        fb.set_native_focus(&mut focus, Some(chrome_id));
        assert_eq!(
            fb.widget_name(focus.expect("top bar must take keyboard focus")),
            Some("TopBarChrome"),
            "this case is the VM path: chrome, not the Terminal, holds focus"
        );
        let mut doc = AutoCommit::new();
        for key in [
            Key::Character(SmolStr::new_static("e")),
            Key::Named(NamedKey::Space),
            Key::Named(NamedKey::Enter),
        ] {
            assert!(
                fb.dispatch_event(
                    &key_press(key),
                    &Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &mut focus,
                ),
                "typed keys must be consumed on the production chrome path"
            );
        }
        let mut got = Vec::new();
        while let Ok(chunk) = probe.try_recv() {
            got.extend(chunk);
        }
        assert_eq!(
            got.as_slice(),
            b"e \r",
            "Space and Enter must reach the PTY while chrome still has focus",
        );
        finish_hosted_terminal_probe(&mut fb);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one production-path case keeps chrome focus, PTY probe, and group split visible"
    )]
    fn super_shift_enter_splits_the_tab_instead_of_reaching_the_pty() {
        use crate::shell::state::{AppKind, TileContent};
        // W08: Super+Shift+Enter is DetachTab. Offering unmodified Space/Enter
        // to the Terminal before chrome must not also swallow this chord.
        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        let target = {
            let chrome = fb.chrome();
            let tree = chrome.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .find(|tab| {
                                matches!(
                                    &tab.tile,
                                    TileContent::AppPlaceholder {
                                        kind: AppKind::Terminal,
                                        ..
                                    } | TileContent::Stub {
                                        kind: AppKind::Terminal,
                                        ..
                                    }
                                )
                            })
                            .map(|tab| (strip.id, group.id, tab.id))
                    })
                })
            })
        };
        let (strip, group, tab) = target.expect("production chrome must include a Terminal tab");
        assert!(fb.host_mut().focus_window_on_strip(strip, group, tab));
        fb.relayout();
        let mut focus = None;
        fb.reconcile_native_focus(&mut focus);
        let chrome_id = fb.node_map.iter().find_map(|(id, widget)| {
            let name = unsafe { (**widget).name() };
            (name == "TopBarChrome").then_some(*id)
        });
        let chrome_id = chrome_id.expect("production chrome must mount the top bar");
        fb.set_native_focus(&mut focus, Some(chrome_id));
        assert_eq!(
            fb.widget_name(focus.expect("top bar must take keyboard focus")),
            Some("TopBarChrome"),
            "chrome holds focus, as on the VM path"
        );
        let probe = fb.node_map.values().find_map(|widget| {
            let widget = unsafe { &mut **widget };
            is_terminal(widget)
                .then(|| hosted_terminal(widget))
                .flatten()
                .map(|term| term.attach_input_probe())
        });
        let Some(probe) = probe else {
            panic!("production chrome must mount a hosted Terminal");
        };
        let groups_before = {
            let chrome = fb.chrome();
            chrome.tree().desktop.active_workspace().map_or(0, |ws| {
                ws.strips
                    .iter()
                    .map(|s| s.window_groups.len())
                    .sum::<usize>()
            })
        };
        let tabs_before = {
            let chrome = fb.chrome();
            chrome
                .tree()
                .desktop
                .active_workspace()
                .and_then(|ws| {
                    ws.strips
                        .iter()
                        .flat_map(|s| s.window_groups.iter())
                        .find(|candidate| candidate.id == group)
                })
                .map_or(0, |g| g.tabs.len())
        };
        let mut doc = AutoCommit::new();
        let chord = Modifiers::from(ModifiersState::SUPER | ModifiersState::SHIFT);
        assert!(
            fb.dispatch_event(
                &key_press(Key::Named(NamedKey::Enter)),
                &chord,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ),
            "Super+Shift+Enter must be consumed as the split-tab chord"
        );
        fb.relayout();
        let mut got = Vec::new();
        while let Ok(chunk) = probe.try_recv() {
            got.extend(chunk);
        }
        assert_eq!(
            got.as_slice(),
            b"",
            "Super+Shift+Enter must not be delivered to the PTY"
        );
        let groups_after = {
            let chrome = fb.chrome();
            chrome.tree().desktop.active_workspace().map_or(0, |ws| {
                ws.strips
                    .iter()
                    .map(|s| s.window_groups.len())
                    .sum::<usize>()
            })
        };
        let tabs_after = {
            let chrome = fb.chrome();
            chrome
                .tree()
                .desktop
                .active_workspace()
                .and_then(|ws| {
                    ws.strips
                        .iter()
                        .flat_map(|s| s.window_groups.iter())
                        .find(|candidate| candidate.id == group)
                })
                .map_or(0, |g| g.tabs.len())
        };
        assert_eq!(
            groups_after,
            groups_before + 1,
            "Super+Shift+Enter must split the tab into a new group"
        );
        assert_eq!(
            tabs_after,
            tabs_before.saturating_sub(1),
            "the source group must keep the remaining tabs"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_oobe_transitions_and_rebuilds_reseat_focus_and_keep_pointer_state() {
        fn native_oobe_pointer_motion_is_retained_before_modal_dispatch() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            let position = Point::new(417.0, 263.0);

            assert!(fb.dispatch_event(
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(position.x, position.y),
                },
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                fb.capture_pointer_position(),
                Some(position),
                "onboarding must retain the motion used to hit-test the following coordinate-free MouseInput"
            );
        }

        fn native_oobe_step_transition_reseats_focus_on_new_first_control() {
            let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            fb.reconcile_native_focus(&mut focus);
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Use my existing Tilo identity")
            );

            press(crate::onboarding_view::KEY_BEGIN);
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);

            assert!(
                focused_name(&fb, focus).is_some_and(|name| name.contains("I understand")),
                "focus must move into the replacement OOBE step"
            );
        }

        fn native_oobe_step_transition_keeps_the_new_page_at_its_top() {
            let (mut fb, _identity_dir, press) = onboarding_frame_with_presser();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            fb.reconcile_native_focus(&mut focus);

            press(crate::onboarding_view::KEY_EXISTING);
            press(crate::onboarding_view::KEY_CHOOSE_RESTORE);
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);

            let scroll_offset = fb
                .node_map
                .values()
                .copied()
                .find_map(|pointer| unsafe {
                    ((*pointer).name() == "Scrollable")
                        .then(|| (*pointer).as_any_mut())
                        .flatten()
                        .and_then(|widget| widget.downcast_mut::<ui::widget::Scrollable>())
                        .map(|scroll| scroll.offset_y())
                })
                .expect("restore page scroll viewport");
            assert_eq!(
                scroll_offset, 0.0,
                "programmatic first focus must not hide a newly presented page's heading"
            );
        }

        fn native_oobe_incidental_rebuild_preserves_focus_ordinal() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            let tab = key_press(Key::Named(NamedKey::Tab));
            assert!(fb.dispatch_event(
                &tab,
                &Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Create a new identity")
            );

            fb.rebuild_layout_tree();
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Create a new identity"),
                "an unrelated structure refresh must preserve the focused ordinal"
            );
        }

        native_oobe_pointer_motion_is_retained_before_modal_dispatch();
        native_oobe_step_transition_reseats_focus_on_new_first_control();
        native_oobe_step_transition_keeps_the_new_page_at_its_top();
        native_oobe_incidental_rebuild_preserves_focus_ordinal();
    }

    #[test]
    fn scheduled_night_light_follows_the_phase_not_the_switch() {
        assert!(night_light_on(false, true, Some(true)));
        assert!(!night_light_on(true, true, Some(false)));
        assert!(night_light_on(true, false, Some(false)));
        assert!(!night_light_on(false, false, Some(true)));
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_tabs_configure_before_first_buffer_and_fullscreen_restores() {
        fn restored_native_tab_gets_a_configure_rect_before_its_first_buffer() {
            use crate::shell::state::{Tab, TileContent};
            use crate::shell::tab_binding::{self, Placement};

            const SURFACE: u64 = 0xfedc_0011;
            const APP: &str = "dev.tilo.calculator";
            crate::shell::launcher::clear_pending_placements();
            let store = crate::shell::foreign::store();
            store.forget(SURFACE);
            let mut tree = WorkspaceTree::bootstrap_default();
            let room = tree.desktop.workspaces[0].room();
            let tab = tab_binding::place_tab(
                &mut tree,
                Tab {
                    id: crate::shell::state::next_id(),
                    title: "Calculator".into(),
                    tile: TileContent::LocalApp {
                        app_id: APP.into(),
                        label: "Calculator".into(),
                        // Restore has already dispatched the app; do not start
                        // a real process from this frame-builder regression.
                        note: crate::shell::peer_windows::OPENING.into(),
                    },
                    app_title: None,
                },
                Placement::NewGroup,
            );
            let mut fb = FrameBuilder::with_tree(tree, (1280, 800));
            let mut doc = AutoCommit::new();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            assert!(fb.foreign_surface_rect(SURFACE).is_none());
            // A real restored LocalApp notice has already appeared on screen.
            // Its host visibility latch must not hide the new app's readiness.
            let _ = fb.paint();
            crate::shell::launcher::set_pending_placement(APP, Placement::IntoTab(tab));
            {
                let chrome = fb.chrome();
                let mut tree = chrome.tree_mut();
                assert_eq!(
                    tab_binding::adopt_toplevel_in_room(&mut tree, SURFACE, Some(APP.into()), room),
                    Some(tab),
                );
            }
            store.note_adopted(SURFACE);
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            assert_eq!(store.frame_seq(SURFACE), 0);
            let rect = fb
                .foreign_surface_rect(SURFACE)
                .expect("restored tab must publish its configure rect before the client can draw");
            assert!(rect.width() > 0.0 && rect.height() > 0.0);
            let (width, height) = store
                .take_pending_configure(SURFACE)
                .expect("layout must make the initial client configure available without input");
            assert!(
                fb.needs_animation_frame(),
                "the opening window must keep progressing"
            );
            assert!(!store.is_presented(SURFACE));
            assert!(
                fb.any_tile_opening(),
                "the visible notice must become an opening card, not an unresponsive-app error"
            );
            let _ = fb.paint();

            let width = u32::try_from(width).expect("positive configured width");
            let height = u32::try_from(height).expect("positive configured height");
            let bytes = width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(4))
                .and_then(|bytes| usize::try_from(bytes).ok())
                .expect("bounded test viewport");
            store.publish_rgba(SURFACE, &vec![255; bytes], width, height);
            let revision = fb.chrome_revision();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            let _ = fb.paint();
            assert!(
                store.is_presented(SURFACE),
                "the first sized buffer must replace the opening presentation"
            );
            assert_eq!(
                fb.chrome_revision(),
                revision,
                "presentation must not need an unrelated tree mutation"
            );
            store.forget(SURFACE);
            crate::shell::launcher::clear_pending_placements();
        }

        /// A group promoted to fullscreen absorbs the whole output (top bar
        /// collapses, the strip's rect rises and spans the width), and clearing
        /// it restores the exact prior layout — the compositor-scene half of the
        /// fullscreen contract, driven through the
        /// `FrameBuilder::set_fullscreen` → `ChromeState` → overlay-host path.
        fn set_fullscreen_promotes_group_and_restores_on_exit() {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();

            // A group that actually exists in the rendered tree.
            let gid = {
                let chrome = fb.chrome();
                let tree = chrome.tree();
                tree.desktop
                    .active_workspace()
                    .expect("bootstrap tree has an active workspace")
                    .strips[0]
                    .window_groups[0]
                    .id
            };

            let before = fb.widget_registry();
            let topbar_before = rect_named(&before, "TopBarChrome");
            let row_before = rect_named(&before, "GroupRow");
            assert!(
                topbar_before.height() > 10.0,
                "top bar visible before fullscreen"
            );

            // Enter fullscreen on that group.
            fb.set_fullscreen(Some(gid));
            assert_eq!(fb.fullscreen_group(), Some(gid));
            let _ = fb.paint_settled();
            assert!(fb.chrome().is_fullscreen());

            let during = fb.widget_registry();
            let topbar_fs = rect_named(&during, "TopBarChrome");
            let row_fs = rect_named(&during, "GroupRow");
            assert!(
                topbar_fs.height() <= 0.5,
                "top bar collapses under fullscreen; got height {}",
                topbar_fs.height(),
            );
            assert!(
                row_fs.y0 < row_before.y0 - 20.0,
                "the strip rises to the top once the bar is gone; before y0 {} during {}",
                row_before.y0,
                row_fs.y0,
            );
            assert!(
                row_fs.width() >= fb.viewport().0 - 1.0,
                "fullscreen strip spans the output width; got {} vs {}",
                row_fs.width(),
                fb.viewport().0,
            );

            // Exit — the layout restores exactly.
            fb.set_fullscreen(None);
            assert_eq!(fb.fullscreen_group(), None);
            let _ = fb.paint_settled();
            assert!(!fb.chrome().is_fullscreen());

            let after = fb.widget_registry();
            let topbar_after = rect_named(&after, "TopBarChrome");
            let row_after = rect_named(&after, "GroupRow");
            assert!(
                (topbar_after.height() - topbar_before.height()).abs() < 0.5,
                "top bar height restores: before {} after {}",
                topbar_before.height(),
                topbar_after.height(),
            );
            assert!(
                (row_after.y0 - row_before.y0).abs() < 0.5
                    && (row_after.x0 - row_before.x0).abs() < 0.5
                    && (row_after.width() - row_before.width()).abs() < 0.5,
                "strip row rect restores exactly: before {row_before:?} after {row_after:?}",
            );
        }

        /// A fullscreen id the rendered tree does NOT host must be a safe
        /// no-op — must not blank the shell by hiding every strip (the live
        /// client tree and rendered tree aren't yet unified).
        fn set_fullscreen_with_unknown_group_does_not_blank_shell() {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            let before = fb.widget_registry();
            let row_before = rect_named(&before, "GroupRow");

            // An id that cannot exist in the tree.
            fb.set_fullscreen(Some(u64::MAX));
            let _ = fb.paint_settled();

            let during = fb.widget_registry();
            let row_during = rect_named(&during, "GroupRow");
            let topbar_during = rect_named(&during, "TopBarChrome");
            assert!(
                row_during.width() > 0.0 && row_during.height() > 0.0,
                "an unknown fullscreen id must not collapse the shell; got {row_during:?}",
            );
            // Geometry matches the normal (non-fullscreen) layout — no promotion,
            // no chrome suppression.
            assert!((row_during.width() - row_before.width()).abs() < 0.5);
            assert!(
                topbar_during.height() > 10.0,
                "top bar stays visible for an unknown fullscreen id",
            );
        }

        /// Expand keeps the top bar.
        fn an_expanded_window_keeps_the_top_bar() {
            let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
            let first = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
            first.maximized = true;
            let group = first.id;
            let mut fb = FrameBuilder::with_tree(tree, (1600, 900));
            let _ = fb.paint_settled();
            fb.with_chrome(|chrome| chrome.set_maximized_group(Some(group)));
            let _ = fb.paint_settled();
            assert!(
                rect_named(&fb.widget_registry(), "TopBarChrome").height() > 10.0,
                "Expand keeps the top bar"
            );
            fb.with_chrome(|chrome| chrome.set_maximized_group(None));
            let _ = fb.paint_settled();
            assert!(
                rect_named(&fb.widget_registry(), "TopBarChrome").height() > 10.0,
                "restoring keeps the top bar"
            );
        }

        /// Super+F on a Tilo window covers the whole output. Hosted apps
        /// are xdg clients; chrome fullscreen still wins so W07 sees
        /// `fullscreen_group`.
        fn super_f_on_a_tilo_group_covers_the_output() {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            assert!(fb.host_mut().toggle_focused_group_fullscreen());
            // The native loop writes `set_fullscreen(None)` every tick so
            // client xdg stays an overlay. Super+F must survive that wipe.
            fb.set_fullscreen(None);
            assert!(
                fb.fullscreen_group().is_some(),
                "compositor set_fullscreen(None) must not clear Super+F"
            );
            let _ = fb.paint_settled();
            assert!(fb.chrome().is_fullscreen());
            let during = fb.widget_registry();
            let row = rect_named(&during, "GroupRow");
            assert!(
                row.width() >= fb.viewport().0 - 1.0,
                "Super+F must cover the output; row width {} vs output {}",
                row.width(),
                fb.viewport().0,
            );
            assert!(
                rect_named(&during, "TopBarChrome").height() <= 0.5,
                "Super+F hides the top bar"
            );
            assert!(fb.host_mut().toggle_focused_group_fullscreen());
            let _ = fb.paint_settled();
            assert!(!fb.chrome().is_fullscreen());
        }

        /// Super+F on a foreign window of a desktop partitioned across two
        /// outputs gives it that whole output, not its strip band under the
        /// hidden top bar, and leaving fullscreen restores its tile.
        fn super_f_foreign_window_covers_its_output_in_a_partitioned_desktop() {
            use crate::shell::output_scene::OutputGeometry;
            use crate::shell::tab_binding::{self, Placement};

            const SURFACE: u64 = 0xfedc_0012;
            const APP: &str = "tilo.test.fullscreen-partition";
            let store = crate::shell::foreign::store();
            store.forget(SURFACE);
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_default();
            let room = tree.desktop.workspaces[0].room();
            crate::shell::launcher::set_pending_placement(APP, Placement::NewGroup);
            assert!(
                tab_binding::adopt_toplevel_in_room(&mut tree, SURFACE, Some(APP.into()), room)
                    .is_some()
            );
            store.note_adopted(SURFACE);
            // Frames on a pinned clock that advances a frame interval each,
            // so the strip's springs run to rest in simulated time: every
            // rect compared below is a settled layout, not a frame of motion.
            let mut at = ui::time::now();
            let settle = |fb: &mut FrameBuilder, at: &mut ui::time::Instant| {
                for _ in 0..120 {
                    *at += ui::time::Duration::from_millis(16);
                    ui::time::testing::with_monotonic_time(*at, || {
                        let _ = fb.paint_settled();
                    });
                }
            };
            let mut fb = FrameBuilder::with_tree(tree, (2944, 1080));
            fb.set_output_geometry(vec![
                OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0).with_id(0),
                OutputGeometry::unscaled(1920.0, 0.0, 1024.0, 640.0).with_id(1),
            ]);
            settle(&mut fb, &mut at);
            let before = fb
                .foreign_surface_rect(SURFACE)
                .expect("tiled foreign window");
            assert!(before.y0 > 0.0);
            assert!(fb.host_mut().toggle_focused_group_fullscreen());
            fb.set_fullscreen(None);
            settle(&mut fb, &mut at);
            let fullscreen = fb
                .foreign_surface_rect(SURFACE)
                .expect("fullscreen foreign window");
            assert!(
                fullscreen.y0.abs() < 0.5 && (fullscreen.height() - 1080.0).abs() < 0.5,
                "Super+F must give the client the whole output height: {fullscreen:?}"
            );
            assert!(
                fullscreen.x0.abs() < 0.5 && (fullscreen.width() - 1920.0).abs() < 0.5,
                "Super+F must give the client the whole output width: {fullscreen:?}"
            );
            assert!(fb.host_mut().toggle_focused_group_fullscreen());
            settle(&mut fb, &mut at);
            let restored = fb
                .foreign_surface_rect(SURFACE)
                .expect("restored foreign window");
            assert_eq!(restored, before, "leaving fullscreen restores the tile");
            store.forget(SURFACE);
            crate::shell::launcher::clear_pending_placements();
        }

        restored_native_tab_gets_a_configure_rect_before_its_first_buffer();
        set_fullscreen_promotes_group_and_restores_on_exit();
        set_fullscreen_with_unknown_group_does_not_blank_shell();
        super_f_on_a_tilo_group_covers_the_output();
        an_expanded_window_keeps_the_top_bar();
        super_f_foreign_window_covers_its_output_in_a_partitioned_desktop();
    }

    #[test]
    fn overview_publishes_new_workspace_before_the_fade_paints() {
        use crate::shell::output_scene::OutputGeometry;
        use crate::shell::state::WorkspaceTree;

        let mut fb = FrameBuilder::with_tree(WorkspaceTree::bootstrap_default(), (3200, 1080));
        fb.set_output_geometry(vec![
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0).with_id(0),
            OutputGeometry::unscaled(1920.0, 0.0, 1280.0, 800.0).with_id(1),
        ]);
        let _ = fb.paint_settled();
        let before = fb.chrome().tree().desktop.workspaces.len();
        fb.with_chrome(|st| {
            st.overlays.overview = true;
            st.bump();
        });
        fb.host_mut().latch_overview_transition();
        // No settled paint: the acceptance check lists targets as soon as the
        // overlay flag is up, which is this moment. The open's first sample
        // is still alpha 0.
        let rect = fb
            .widget_registry()
            .get("add_workspace")
            .copied()
            .unwrap_or(Rect::ZERO);
        assert!(
            rect.area() > 0.0,
            "overview must publish add_workspace as soon as it is open"
        );
        assert!(
            rect.x1 <= 1920.0,
            "the new-workspace control stays on the first display (right edge {})",
            rect.x1
        );
        let _ = fb.paint_with_damage();
        assert!(
            fb.widget_registry()
                .get("add_workspace")
                .is_some_and(|hit| hit.area() > 0.0),
            "the alpha-zero open sample must keep add_workspace addressable"
        );
        let center = rect.center();
        let mut doc = AutoCommit::new();
        let mut focus = None;
        let modifiers = Modifiers::default();
        let _ = fb.dispatch_event(
            &WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(center.x, center.y),
            },
            &modifiers,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        );
        let clicked = fb.dispatch_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
            &modifiers,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &mut focus,
        );
        assert!(
            clicked,
            "the new-workspace click is handled by the overview"
        );
        let after = fb.chrome().tree().desktop.workspaces.len();
        let open = fb.chrome().overlays.overview;
        assert_eq!(
            after,
            before + 1,
            "clicking add_workspace creates a workspace"
        );
        assert!(
            !open,
            "creating a workspace closes the overview so navigation reaches it"
        );
    }

    #[test]
    fn an_unchanged_frame_reports_empty_damage() {
        // Hosted apps can finish a background save between paints (Notes'
        // "Saving…" chip becomes "Saved"). This assertion needs an unchanged
        // scene, so use the production empty workspace with owned services and
        // controlled fonts instead of starting asynchronous demo applications.
        let mut frame = crate::shell::test_support::PopulatedShellFixture::with_tree(
            (800, 600),
            Theme::dark(),
            WorkspaceTree::bootstrap_default(),
        );
        frame.relayout();
        let first = frame.paint_with_damage();
        assert!(
            !first.scene.encoding().is_empty(),
            "real shell chrome is painted"
        );
        assert!(
            first.damage_regions.is_none(),
            "the first frame has no predecessor to retain"
        );
        let second = frame.paint_with_damage();
        assert_eq!(
            second.damage_regions.as_deref(),
            Some(&[][..]),
            "an identical frame must not request another full upload"
        );
    }

    /// Look up a widget's rect by its *last* path segment
    /// (`.../TopBarChrome[0]` → `TopBarChrome`) — registry keys are full
    /// paths, so an exact-key lookup on a nested widget would miss. The
    /// widgets asserted on here are unique in the rendered tree, so the
    /// first match is deterministic.
    #[cfg(test)]
    fn rect_named(reg: &HashMap<String, vello::kurbo::Rect>, name: &str) -> vello::kurbo::Rect {
        reg.iter()
            .find(|(k, _)| {
                let seg = k.rsplit('/').next().unwrap_or(k);
                let leaf = match seg.find('[') {
                    Some(i) => &seg[..i],
                    None => seg,
                };
                leaf == name
            })
            .map_or_else(
                || {
                    panic!(
                        "widget `{name}` not found in registry; keys: {:?}",
                        reg.keys().collect::<Vec<_>>()
                    )
                },
                |(_, r)| *r,
            )
    }

    #[test]
    fn the_magnifier_centres_on_the_keyboard_focus_once_it_moves_past_a_resting_pointer() {
        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        fb.visual_accessibility.magnifier.zoom = 2;
        let t0 = Instant::now();
        let pointer = Point::new(20.0, 700.0);
        fb.host.note_pointer(pointer);
        fb.note_keyboard_focus(None);
        fb.refresh_magnifier_at(t0);
        let focused = fb.focusable_order().into_iter().find_map(|id| {
            let &widget = fb.node_map.get(&id)?;
            // SAFETY: node_map pointers are owned by the live widget tree.
            let rect = unsafe { (*widget).get_rect() };
            (rect.area() > 0.0 && rect.center() != pointer).then_some((id, rect))
        });
        let Some((node, rect)) = focused else {
            panic!("the desktop offers a focusable control");
        };
        assert_eq!(
            fb.magnifier_focus.centre(),
            Some(pointer),
            "before the focus moves, the pointer leads"
        );

        fb.note_keyboard_focus(Some(node));
        fb.refresh_magnifier_at(t0 + Duration::from_millis(50));
        assert_eq!(fb.magnifier_focus.centre(), Some(rect.center()));

        let moved = Point::new(40.0, 690.0);
        fb.host.note_pointer(moved);
        fb.refresh_magnifier_at(t0 + Duration::from_millis(100));
        assert_eq!(fb.magnifier_focus.centre(), Some(moved));
    }

    #[test]
    fn keyboard_focus_is_not_tracked_while_the_magnifier_is_off() {
        let mut fb = FrameBuilder::new((1280, 800));
        fb.relayout();
        let node = fb.focusable_order().into_iter().next();
        fb.note_keyboard_focus(node);
        assert_eq!(fb.keyboard_focus_rect, None);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn theme_actions_repaint_quick_settings_chrome() {
        use crate::shell::search::action_api::ActionArgs;
        use crate::shell::search::providers::ActionsProvider;
        use crate::shell::search::{ResourceKind, SearchContext, SearchProvider};
        use ui::a11y::relative_luminance;
        use ui::chrome::shell::panel_ground;

        let config = tempfile::tempdir().expect("isolated appearance store");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        env.remove("TILO_SHOTS_THEME");
        env.remove("TILO_SHOTS_TEXT_SCALE");

        let mut frame = FrameBuilder::new((1280, 800));
        frame.enable_configured_theme();
        frame.with_chrome(|chrome| {
            let user = {
                let tree = chrome.tree();
                tree.desktop
                    .active_workspace()
                    .map(|workspace| chrome.local_user_for_room(&workspace.room()))
            };
            if let Some(user) = user {
                let mut tree = chrome.tree_mut();
                if let Some(workspace) = tree.desktop.active_workspace_mut() {
                    workspace.set_owner(user);
                }
            }
            chrome.overlays.quick_settings = true;
        });
        frame.relayout();

        let chrome = frame.chrome_handle();
        let provider = ActionsProvider::new();
        let invoke = |id: &str| {
            let row = {
                let document = AutoCommit::new();
                let state = chrome.borrow();
                provider
                    .query(SearchContext {
                        query_scope: None,
                        include_programmatic: false,
                        needle: "",
                        wanted: &[ResourceKind::Action],
                        chrome: &state,
                        doc: &document,
                        doc_root: &automerge::ROOT,
                    })
                    .into_iter()
                    .find(|row| row.id == id)
                    .unwrap_or_else(|| panic!("{id} must be exported"))
            };
            let mut document = AutoCommit::new();
            (row.on_open)(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
            .unwrap();
        };

        invoke("appearance.theme.light");
        let _ = frame.paint();
        let light = panel_ground(&frame.theme);
        let light_bg = frame.theme.tokens.colors.surfaces.base;
        let light_y = relative_luminance(light);

        invoke("appearance.theme.dark");
        let _ = frame.paint();
        let dark = panel_ground(&frame.theme);
        let dark_bg = frame.theme.tokens.colors.surfaces.base;
        let dark_y = relative_luminance(dark);

        assert_ne!(
            (light, light_bg),
            (dark, dark_bg),
            "invoking appearance.theme.light then dark must change the Quick Settings chrome the next paint uses, not only the settings file"
        );
        assert!(
            (light_y - dark_y).abs() > 0.08,
            "light and dark Quick Settings chrome must not share luminance (light {light_y:.3}, dark {dark_y:.3})"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn theme_file_write_repaint_quick_settings_chrome() {
        use ui::a11y::relative_luminance;
        use ui::chrome::shell::panel_ground;

        let config = tempfile::tempdir().expect("isolated appearance store");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        env.remove("TILO_SHOTS_THEME");
        env.remove("TILO_SHOTS_TEXT_SCALE");

        let write = |theme: &str| {
            let mut values = serde_json::Map::new();
            values.insert("mode".into(), serde_json::Value::String(theme.into()));
            values.insert("high_contrast".into(), serde_json::Value::Bool(false));
            settings_store::update_section("appearance", values).expect("write appearance");
        };

        let mut frame = FrameBuilder::new((1280, 800));
        frame.enable_configured_theme();
        frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
        frame.relayout();

        write("light");
        frame.with_chrome(ChromeState::note_appearance_change);
        let _ = frame.paint();
        let light = panel_ground(&frame.theme);
        let light_bg = frame.theme.tokens.colors.surfaces.base;
        let light_y = relative_luminance(light);

        write("dark");
        frame.with_chrome(ChromeState::note_appearance_change);
        let _ = frame.paint();
        let dark = panel_ground(&frame.theme);
        let dark_bg = frame.theme.tokens.colors.surfaces.base;
        let dark_y = relative_luminance(dark);

        assert_ne!(
            (light, light_bg),
            (dark, dark_bg),
            "writing appearance.theme light then dark must change the Quick Settings chrome the next paint uses"
        );
        assert!(
            (light_y - dark_y).abs() > 0.08,
            "light and dark Quick Settings chrome must not share luminance (light {light_y:.3}, dark {dark_y:.3})"
        );
    }

    /// The desktop rasterised on the CPU at `size`, as the next frame shows it.
    fn rendered_frame(frame: &mut FrameBuilder, (width, height): (u32, u32)) -> Vec<u8> {
        let scene = frame.paint();
        let stride = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .expect("a row's bytes fit");
        let len = usize::try_from(height)
            .ok()
            .and_then(|height| height.checked_mul(stride))
            .expect("a frame's bytes fit");
        let mut pixels = vec![0u8; len];
        let mut target = tilo_cpu_raster::Target::new(&mut pixels, width, height, stride)
            .expect("a raster target");
        tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
            scene.encoding(),
            &mut target,
            None,
            frame.theme.tokens.colors.surfaces.base,
        );
        pixels
    }

    /// The share of pixels that differ between two frames.
    fn changed_share(before: &[u8], after: &[u8]) -> f64 {
        let changed = before
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        let pixels = before.chunks_exact(4).len();
        let count = |n: usize| f64::from(u32::try_from(n).expect("a test frame's pixels fit u32"));
        count(changed) / count(pixels)
    }

    /// A desktop following an isolated settings file, and a way to change
    /// its `appearance` section as Settings does: the write, then the
    /// compositor's notice of it.
    fn following_appearance() -> (tempfile::TempDir, tilo_test_env::ScopedEnv, FrameBuilder) {
        let config = tempfile::tempdir().expect("isolated settings store");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        env.remove("TILO_SESSION_SETTINGS");
        env.remove("TILO_SHOTS_THEME");
        env.remove("TILO_SHOTS_TEXT_SCALE");
        for name in [
            "TILO_A11Y_TEXT_SCALE",
            "TILO_TEXT_SCALE",
            "TILO_UI_SCALE",
            "TILO_A11Y_UI_SCALE",
        ] {
            env.remove(name);
        }
        let mut frame = FrameBuilder::new((1280, 800));
        frame.enable_configured_theme();
        (config, env, frame)
    }

    fn set_appearance(frame: &mut FrameBuilder, values: serde_json::Value) {
        let serde_json::Value::Object(values) = values else {
            unreachable!("an appearance section is an object")
        };
        settings_store::update_section("appearance", values).expect("write appearance");
        frame.with_chrome(ChromeState::note_appearance_change);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn toggling_high_contrast_recolours_the_desktop_on_the_next_frame() {
        let (_config, _env, mut frame) = following_appearance();
        set_appearance(
            &mut frame,
            serde_json::json!({"mode": "dark", "high_contrast": false}),
        );
        let before = rendered_frame(&mut frame, (1280, 800));
        set_appearance(&mut frame, serde_json::json!({"high_contrast": true}));
        let contrast = rendered_frame(&mut frame, (1280, 800));
        assert!(
            frame.theme.document.palette.high_contrast,
            "the chrome's theme is the high-contrast one"
        );
        let share = changed_share(&before, &contrast);
        assert!(
            share > 0.01,
            "high contrast changes what the desktop shows on the next frame ({:.2}% of pixels)",
            share * 100.0
        );
        set_appearance(&mut frame, serde_json::json!({"high_contrast": false}));
        let after = rendered_frame(&mut frame, (1280, 800));
        assert!(!frame.theme.document.palette.high_contrast);
        assert!(
            changed_share(&contrast, &after) > 0.01,
            "turning it off changes the next frame back"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_text_size_change_relays_the_desktop_out_on_the_next_frame() {
        let (_config, _env, mut frame) = following_appearance();
        set_appearance(
            &mut frame,
            serde_json::json!({"mode": "dark", "text_scale": 1.0}),
        );
        let before_pixels = rendered_frame(&mut frame, (1280, 800));
        let before = frame.widget_registry();
        assert!((frame.theme.text_scale - 1.0).abs() < f32::EPSILON);

        set_appearance(&mut frame, serde_json::json!({"text_scale": 1.3}));
        let after_pixels = rendered_frame(&mut frame, (1280, 800));
        assert!(
            (frame.theme.text_scale - 1.3).abs() < 1e-3,
            "the chrome's theme takes the person's text size, not the launcher's"
        );
        assert!(
            (frame.text_renderer.text_scale() - 1.3).abs() < 1e-3,
            "the chrome's text is measured and drawn at that size"
        );
        assert_ne!(
            before,
            frame.widget_registry(),
            "the chrome is laid out again"
        );
        assert!(changed_share(&before_pixels, &after_pixels) > 0.0);

        set_appearance(&mut frame, serde_json::json!({"ui_scale": 1.25}));
        let _ = frame.paint();
        assert!(
            (frame.theme.ui_scale - 1.25).abs() < 1e-3,
            "the interface scale reaches the chrome too"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_theme_preset_change_recolours_the_desktop_on_the_next_frame() {
        let (_config, _env, mut frame) = following_appearance();
        set_appearance(
            &mut frame,
            serde_json::json!({"mode": "dark", "dark_theme": "dark"}),
        );
        let before = rendered_frame(&mut frame, (1280, 800));
        set_appearance(&mut frame, serde_json::json!({"mode": "light"}));
        let light = rendered_frame(&mut frame, (1280, 800));
        assert_eq!(frame.theme.name, Theme::light().name);
        assert!(
            changed_share(&before, &light) > 0.01,
            "light shows on the next frame"
        );
        set_appearance(
            &mut frame,
            serde_json::json!({"mode": "dark", "dark_theme": "dark-cool"}),
        );
        let cool = rendered_frame(&mut frame, (1280, 800));
        assert_eq!(
            frame.theme.name,
            Theme::named("dark-cool").expect("a preset").name
        );
        assert!(
            changed_share(&light, &cool) > 0.01,
            "the preset shows on the next frame"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn workspace_temperature_is_read_from_the_followed_settings_not_the_file() {
        let config = tempfile::tempdir().expect("isolated settings store");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        env.remove("TILO_SESSION_SETTINGS");
        env.remove("TILO_SHOTS_THEME");
        env.remove("TILO_SHOTS_TEXT_SCALE");

        let mut frame = FrameBuilder::new((1280, 800));
        let active = frame
            .chrome()
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.id.0.to_string())
            .expect("an active workspace");
        let temperatures = |hue: f64| {
            let mut temperatures = serde_json::Map::new();
            temperatures.insert(active.clone(), serde_json::json!(hue));
            let mut appearance = serde_json::Map::new();
            appearance.insert(
                "workspace_temperatures".into(),
                serde_json::Value::Object(temperatures),
            );
            appearance
        };
        // The file on disk says one hue; the session's followed document
        // says another. Paint must take the followed one: reading the file
        // in paint is what this replaced.
        settings_store::update_section("appearance", temperatures(200.0))
            .expect("write the settings file");
        let mut followed = serde_json::Map::new();
        followed.insert(
            "appearance".into(),
            serde_json::Value::Object(temperatures(40.0)),
        );
        let source = settings_store::HostDocument::new(followed, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "read-only fixture settings",
            ))
        });
        frame.follow_settings(settings_store::live::LiveSettings::start_from(
            std::sync::Arc::new(source),
            || {},
        ));
        frame.watch_configured_theme = true;
        frame.refresh_workspace_temperature();
        assert_eq!(
            frame.windows.active().workspace_hue,
            Some(40.0),
            "the hue comes from the followed settings snapshot"
        );
    }
}

#[path = "frame_scene_revision.rs"]
mod scene_revision;

#[cfg(test)]
#[path = "frame_overlay_latency_tests.rs"]
mod overlay_latency_tests;

#[cfg(test)]
#[path = "frame_retained_tests.rs"]
mod retained_tests;

#[cfg(test)]
#[path = "frame_window_bar_keyboard_tests.rs"]
mod window_bar_keyboard_tests;

#[cfg(test)]
#[path = "frame_quiet_pump_tests.rs"]
mod quiet_pump_tests;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
#[path = "frame_transition_tests.rs"]
mod transition_tests;

/// `regions` without any region another one contains (the first of two
/// equal ones stays): the same pixels, rasterised once. A changed card's
/// old and new bounds and the blurred surface on it often nest.
fn without_contained_regions(regions: &[FineDamageRect]) -> Vec<FineDamageRect> {
    let rect = |region: &FineDamageRect| {
        Rect::new(
            f64::from(region.x),
            f64::from(region.y),
            f64::from(region.x) + f64::from(region.width),
            f64::from(region.y) + f64::from(region.height),
        )
    };
    let contains = |outer: &FineDamageRect, inner: &FineDamageRect| {
        rect(outer).union(rect(inner)) == rect(outer)
    };
    regions
        .iter()
        .enumerate()
        .filter(|&(index, region)| {
            !regions.iter().enumerate().any(|(other, outer)| {
                other != index && contains(outer, region) && (outer != region || other < index)
            })
        })
        .map(|(_, region)| *region)
        .collect()
}

/// `scene`, drawn in logical coordinates, magnified by `to_scene` into
/// scene pixels without clipping it to the display: a strip's canvas runs
/// past the display's right edge, and the pan brings that part in.
/// [`magnified`] for a scene drawn with damage parts, which keep marking
/// what they marked, their bounds magnified with it.
fn magnified_with_parts(
    scene: Scene,
    parts: Option<ui::scene_parts::SectionParts>,
    to_scene: Affine,
) -> (Scene, Option<ui::scene_parts::SectionParts>) {
    if to_scene == Affine::IDENTITY {
        return (scene, parts);
    }
    let Some(parts) = parts else {
        return (magnified(scene, to_scene), None);
    };
    let mut magnified = Scene::new();
    let ((), parts) = ui::scene_parts::record(&mut magnified, |magnified| {
        ui::scene_parts::append(magnified, &scene, Some(&parts), Some(to_scene));
    });
    (magnified, parts)
}

fn magnified(scene: Scene, to_scene: Affine) -> Scene {
    if to_scene == Affine::IDENTITY {
        return scene;
    }
    let mut magnified = Scene::new();
    magnified.append(&scene, Some(to_scene));
    magnified
}

/// One device pixel in layout pixels on a display at `scale` device pixels
/// per layout pixel: two thirds at 150 %, four fifths at 125 %, a half at
/// 200 %. `None` for a scale that is not finite and positive.
fn pan_step(scale: f64) -> Option<f64> {
    (scale.is_finite() && scale > 0.0).then_some(1.0 / scale)
}

/// `value` is within a thousandth of a whole number.
fn nearly_whole(value: f64) -> bool {
    value.is_finite() && (value - value.round()).abs() < 1e-3
}

/// A non-negative scene coordinate as whole pixels, rounded down.
fn scene_px(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let value = value.min(f64::from(u32::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scene pixel edges are finite, non-negative, and clamped to u32"
    )]
    {
        value as u32
    }
}

/// Phase timings of one retained scene build, reported only when the build
/// is slow. A typical build is under a millisecond; the main-thread freezes
/// seen when a window opens or closes (`SLOW-ITER post_pump_paint` of
/// 240-490 ms) were not attributable to a phase, and this names it in the
/// session log (`SLOW-SCENE`).
struct ScenePhases {
    started: std::time::Instant,
    last: std::time::Instant,
    spans: Vec<(&'static str, std::time::Duration)>,
}

impl ScenePhases {
    /// A scene build slower than this is reported.
    const REPORT_AFTER: std::time::Duration = std::time::Duration::from_millis(50);

    fn start() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            last: now,
            spans: Vec::with_capacity(8),
        }
    }

    fn mark(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        let took = now.duration_since(self.last);
        crate::compositor::metrics::profile::record_phase(phase, took, None);
        self.spans.push((phase, took));
        self.last = now;
    }

    fn report(&self) {
        let total = self.started.elapsed();
        if total < Self::REPORT_AFTER {
            return;
        }
        let split = self
            .spans
            .iter()
            .map(|(phase, took)| format!("{phase}={took:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("SLOW-SCENE total={total:?} {split}");
    }
}
