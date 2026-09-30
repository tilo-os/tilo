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
use std::time::{Duration, Instant, SystemTime};

use automerge::{AutoCommit, ObjId};
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
use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::WorkspaceTree;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

fn consume_record_requests(
    chrome: &Chrome,
    index: &std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>>,
) {
    #[allow(deprecated, reason = "drain the stable active-room compatibility API")]
    let legacy = chrome.borrow_mut().take_record_requests();
    if !legacy.is_empty() {
        let room = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .map(|workspace| workspace.room)
        };
        if let Some(room) = room {
            let mut index = index.borrow_mut();
            for (id, kind) in legacy {
                if let Some(record) = ui::record::RecordId::parse(&id) {
                    index.reference(room, &record, &ui::record::RecordKind::new(kind));
                }
            }
        }
    }
    let requested = chrome.borrow_mut().take_tracked_record_requests();
    if requested.is_empty() {
        return;
    }
    let mut index = index.borrow_mut();
    let mut completed = Vec::new();
    for request in requested {
        #[expect(
            clippy::suspicious_operation_groupings,
            reason = "a tracked request names its workspace by `workspace_id`, not `id`"
        )]
        let authorized = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| {
                    workspace.id == request.workspace_id && workspace.room == request.room
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
}

impl FramePaint {
    /// Wrap a scene from a producer without retained-damage authority.
    pub fn full(scene: Scene) -> Self {
        Self {
            scene,
            revision: 0,
            previous_revision: None,
            damage_regions: None,
            hdr_layers: Vec::new(),
        }
    }

    /// The same frame, with `layers` composited where HDR is shown.
    #[must_use]
    pub fn with_hdr_layers(mut self, layers: Vec<crate::shell::hdr_surface::HdrLayer>) -> Self {
        self.hdr_layers = layers;
        self
    }

    /// External scene mutation invalidates every retained-damage proof.
    pub fn force_full(&mut self) {
        // Revision zero is deliberately never produced by `paint_with_damage`. It makes this
        // frame an unversioned boundary, so a later retained frame cannot reuse pixels from
        // commands appended outside the captured sections.
        self.revision = 0;
        self.damage_regions = None;
        self.previous_revision = None;
    }
}

#[derive(Clone)]
struct ProducedSections {
    backdrop: Scene,
    topbar: Scene,
    workspace: Scene,
    topbar_bounds: Option<Rect>,
    quick_settings: Scene,
    above_quick_settings: Scene,
    quick_settings_bounds: Option<Rect>,
    global_filter: bool,
    revision: u64,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DesktopColorFilter {
    #[default]
    None,
    Grayscale,
    Inverted,
    GrayscaleInverted,
}

use settings_store::magnifier::{MagnifierState, MagnifierView};

#[derive(Clone, Copy, Debug, PartialEq)]
struct VisualAccessibility {
    color_filter: DesktopColorFilter,
    magnifier: MagnifierState,
    cursor_crosshair: bool,
}

impl Default for VisualAccessibility {
    fn default() -> Self {
        Self {
            color_filter: DesktopColorFilter::None,
            magnifier: MagnifierState {
                zoom: settings_store::magnifier::OFF,
                view: MagnifierView::Lens,
            },
            cursor_crosshair: false,
        }
    }
}

impl VisualAccessibility {
    fn load() -> Self {
        let section = settings_store::load_section("accessibility");
        Self {
            color_filter: match section
                .get("color_filter")
                .and_then(serde_json::Value::as_str)
            {
                Some("grayscale") => DesktopColorFilter::Grayscale,
                Some("inverted") => DesktopColorFilter::Inverted,
                Some("grayscale_inverted") => DesktopColorFilter::GrayscaleInverted,
                _ => DesktopColorFilter::None,
            },
            magnifier: MagnifierState::from_section(&section),
            cursor_crosshair: section
                .get("cursor_crosshair")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }
    }
}

use crate::shell::output_scene::{self, OutputGeometry};

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
    taffy: TaffyTree<()>,
    /// Widget-pointer map captured during `build_layout`, kept alive so the
    /// measure closure in `compute_layout_with_measure` can re-borrow each frame.
    node_map: HashMap<NodeId, *mut dyn Widget>,
    /// Inner node — the host's own root child, target of `apply_layout` so
    /// the host's rect tree roots at the viewport origin.
    host_node: Option<NodeId>,
    /// Outer node — a viewport-sized wrapper giving `compute_layout` an
    /// explicit root size (Taffy needs a definite extent; the host itself is
    /// `percent(1.0)`).
    root_node: Option<NodeId>,
    text_renderer: TextRenderer,
    theme: Theme,
    watch_configured_theme: bool,
    theme_store_stamp: Option<SystemTime>,
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
    /// The session is locked: a capture sees nothing of the desktop at all.
    session_locked: bool,
    /// XDG and input-method transient surfaces.  These are intentionally
    /// outside the workspace tree: they follow parent geometry and paint
    /// above the app that owns them without becoming a window/tab.
    transient_overlays: Vec<(u64, ForeignSurfaceWidget)>,
    /// Pixel-exact final app frames shown only while a newly authorised
    /// secondary surface is preparing its first buffer.
    floating_transfer_proxies: Vec<FloatingTransferProxy>,
    osk_anchor_x: Option<f64>,
    osk_active: bool,
    /// A layout rebuild invalidates every Taffy node id held by the native
    /// input loop. Re-seat its external focus cursor before the next event;
    /// otherwise the first-login wizard starts unfocused and a step change
    /// can leave keyboard input pointing at a widget that no longer exists.
    focus_tree_changed: bool,
    /// Stable position used to preserve focus across incidental rebuilds,
    /// matching `ui::App`; an explicit focus-reset ignores it and starts the
    /// replacement wizard step at its first enabled control.
    native_focus_ordinal: Option<usize>,
    /// Latest produced sections, not an acknowledgement. A skipped presenter therefore creates a
    /// revision mismatch at the bridge and safely selects its dense path.
    last_produced_sections: Option<ProducedSections>,
    next_scene_revision: u64,
    /// HDR layers of the latest [`Self::paint`], until taken.
    painted_hdr_layers: Vec<crate::shell::hdr_surface::HdrLayer>,
    /// What the pointer shows right now, so captures draw the same image.
    pointer_image: crate::compositor::cursor::PointerImage,
    /// The session network, once peer sync is up, so a control invoke can
    /// address an enrolled device.
    enrolled_actions: Option<std::sync::Arc<crate::shell::network::ShellNetwork>>,
}

#[derive(Clone, Copy, Debug, Default)]
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
    theme_store_stamp: Option<std::time::SystemTime>,
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
    /// `TILO_SHOTS_THEME=light|high-contrast|dark` and
    /// `TILO_SHOTS_TEXT_SCALE=<factor>`. Unset keeps the dark default — lets
    /// a theme-driven chrome change be proven in high contrast and at large
    /// text, not just its comfortable default density.
    fn shots_theme_override() -> Option<Theme> {
        let requested_theme = std::env::var("TILO_SHOTS_THEME").ok();
        let requested_scale = std::env::var("TILO_SHOTS_TEXT_SCALE")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite());
        if requested_theme.is_none() && requested_scale.is_none() {
            return None;
        }
        let theme = match requested_theme
            .as_deref()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("light") => Theme::light(),
            Some("high-contrast" | "high_contrast" | "hc") => Theme::high_contrast(),
            _ => Theme::dark(),
        };
        Some(if let Some(scale) = requested_scale {
            theme.with_text_scale(scale)
        } else {
            theme
        })
    }

    /// Build a frame builder over a *shared* [`WorkspaceTreeHandle`](crate::shell::state::WorkspaceTreeHandle), with
    /// the peer-sync palette rows on or off.
    ///
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
            theme_store_stamp: settings_store::settings_path()
                .and_then(|path| std::fs::metadata(path).ok()?.modified().ok()),
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
            host_node: None,
            root_node: None,
            text_renderer: presentation.text_renderer,
            last_room_epoch: 0,
            theme: presentation.theme,
            watch_configured_theme: false,
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
            session_locked: false,
            transient_overlays: Vec::new(),
            floating_transfer_proxies: Vec::new(),
            osk_anchor_x: None,
            osk_active: false,
            focus_tree_changed: true,
            native_focus_ordinal: None,
            last_produced_sections: None,
            next_scene_revision: 1,
            painted_hdr_layers: Vec::new(),
            pointer_image: crate::compositor::cursor::PointerImage::default(),
            enrolled_actions: None,
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
        if *focus == target {
            return;
        }
        if let Some(old) = *focus
            && let Some(&widget) = self.node_map.get(&old)
        {
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

    fn widget_name(&self, id: NodeId) -> Option<&'static str> {
        self.node_map.get(&id).map(|&widget| {
            // SAFETY: this map belongs to the current, live layout tree.
            unsafe { (*widget).name() }
        })
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

    fn initial_focus_target(&self, order: &[NodeId]) -> Option<NodeId> {
        // A focused Terminal must receive typing, including Space and Enter.
        // Chrome controls come first in DFS and used to steal initial focus;
        // Activate then opened workspace settings instead of sending CR.
        if self.active_tile_is_terminal() {
            // TileHost children are `dyn AppWidget`; chrome focus order
            // compares `dyn Widget` fat pointers and used to miss the
            // Terminal even though it is in the live node map.
            if let Some((&id, _)) = self.node_map.iter().find(|(_, mapped)| {
                let widget = unsafe { &***mapped };
                widget.name() == "Terminal" && widget.is_focusable() && widget.is_enabled()
            }) {
                return Some(id);
            }
            if let Some(terminal) = order
                .iter()
                .copied()
                .find(|id| self.widget_name(*id) == Some("Terminal"))
            {
                return Some(terminal);
            }
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
        let target = if self.active_tile_is_terminal() {
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
        let reset = self.host.take_focus_reset();
        if reset {
            self.relayout();
        }
        if reset {
            self.focus_first(focus);
        } else if self.focus_tree_changed {
            self.restore_focus_after_tree_change(focus);
        } else if focus.is_none() {
            self.focus_first(focus);
        }
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
        let layout_result = self.taffy.compute_layout_with_measure(
            outer,
            available_space,
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        );
        if let Err(err) = layout_result {
            // Same "should be structurally impossible" situation as
            // `rebuild_layout_tree`'s node-creation call: log and skip this
            // frame's apply_layout rather than aborting the session.
            tracing::error!(
                %err,
                "frame-builder: compute_layout failed; skipping apply_layout this frame"
            );
            return;
        }
        self.host.apply_layout(&self.taffy, child, Point::ORIGIN);
        self.layout_osk_overlay();
    }

    fn layout_osk_overlay(&mut self) {
        if self.osk_overlay.is_none() {
            return;
        }
        let outputs = self.effective_outputs();
        let anchor = self.osk_anchor_x.unwrap_or(0.0);
        let output = outputs
            .iter()
            .find(|o| anchor >= o.x && anchor < o.x + o.width)
            .or_else(|| outputs.first());
        let Some(output) = output else { return };

        let margin = 20.0_f64.min(output.width / 20.0);
        let width = (output.width - margin * 2.0).clamp(1.0, 960.0);
        let height = (output.height * 0.40)
            .clamp(220.0, 380.0)
            .min((output.height - margin * 2.0).max(1.0));
        let x0 = output.x + (output.width - width) / 2.0;
        let y0 = (output.height - height - margin).max(0.0);
        if let Some(keyboard) = self.osk_overlay.as_mut() {
            keyboard.place_floating(Rect::new(x0, y0, x0 + width, y0 + height));
        }
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
        self.host.sync(doc, root);
        let sync_done = std::time::Instant::now();
        let changed = self.host.update(doc, root);
        let update_done = std::time::Instant::now();
        let total = update_done.duration_since(started);
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
        }
        changed
    }

    /// Turn "a peer's changes landed in the tree" into "the widget tree is
    /// rebuilt". `resync_strips_from_tree` splices groups/tabs, gated on the
    /// chrome revision; peer-sync writes the tree from the network thread,
    /// which can't touch the chrome, so this bumps the revision whenever the
    /// room epoch (or a pending peer-window decision) has moved. Cheap on a
    /// quiet frame — one `u64` read under the tree's read lock.
    fn pump_room_contents(&mut self) -> bool {
        let epoch = {
            let chrome = self.chrome.borrow();
            let tree = chrome.tree();
            tree.desktop.room_epoch()
        };
        // Peer windows are examined on rounds after an arrival too, not just
        // the arrival itself: `peer_windows::examine` starts at most one
        // launch per app per pass, so several windows at once need more than
        // one look.
        let owed = {
            let chrome = self.chrome.borrow();
            let tree = chrome.tree();
            crate::shell::peer_windows::any_pending(&tree)
                || crate::shell::record_hydration::any_waiting()
        };
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
    /// Cheap on a quiet frame: `RoomRecords::replicate_with` compares a
    /// generation counter before reading any document, and the chrome only
    /// bumps when something actually arrived.
    fn replicate_room_records(&self) {
        let index = self.host.room_records();

        // Anything an exported action asked to bring here, first — so a
        // reference made this frame publishes in the same pass.
        consume_record_requests(&self.chrome, &index);

        let arrived = {
            let chrome = self.chrome.borrow();
            let mut tree = chrome.tree_mut();
            let mut index = index.borrow_mut();
            index.replicate_with(&mut tree)
        };
        if arrived {
            self.chrome.borrow_mut().bump();
        }
    }

    /// Render the current frame into a fresh Vello scene, in scene pixels:
    /// every display draws its part of the desktop at its own scale. Caller
    /// hands it to a renderer (offscreen/live/compositor backend).
    pub fn paint(&mut self) -> Scene {
        let recording = self
            .hdr_layers_composited()
            .then(crate::shell::hdr_surface::HdrLayerRecording::begin);
        let logical = self.paint_logical();
        let layers = recording.map(crate::shell::hdr_surface::HdrLayerRecording::finish);
        self.painted_hdr_layers = self.hdr_layers_to_scene(layers.unwrap_or_default());
        self.to_scene(logical)
    }

    /// The HDR layers the latest [`Self::paint`] drew, in scene pixels.
    pub fn take_hdr_layers(&mut self) -> Vec<crate::shell::hdr_surface::HdrLayer> {
        std::mem::take(&mut self.painted_hdr_layers)
    }

    /// Whether HDR surfaces may be composited outside the scene. A
    /// whole-desktop colour filter or magnifier transforms everything vello
    /// draws, so while one is on HDR surfaces show their SDR rendition with
    /// it.
    fn hdr_layers_composited(&self) -> bool {
        !(self.watch_configured_theme
            && (self.night_light
                || !matches!(
                    self.visual_accessibility.color_filter,
                    DesktopColorFilter::None
                )
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

    /// [`Self::paint`] before the displays magnify it: the desktop in
    /// logical coordinates, the space the widget tree is laid out in.
    fn paint_logical(&mut self) -> Scene {
        self.refresh_configured_theme();
        self.refresh_magnifier();
        let mut scene = Scene::new();
        self.host.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
        );
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
        scene
    }

    /// Produce the same ordered shell composition as [`paint`](Self::paint), together with
    /// retained-damage metadata for the DRM bridge. The dense [`paint`](Self::paint) path stays
    /// independent for capture and pixel-oracle use.
    #[allow(
        clippy::too_many_lines,
        reason = "Retained-damage metadata must be derived alongside the exact scene composition; splitting this transaction risks draw-order or damage-region divergence."
    )]
    pub fn paint_with_damage(&mut self) -> FramePaint {
        self.refresh_configured_theme();
        self.refresh_magnifier();
        let recording = self
            .hdr_layers_composited()
            .then(crate::shell::hdr_surface::HdrLayerRecording::begin);
        let mut sections = self.host.draw_scene_sections(
            Affine::IDENTITY,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
        );
        for proxy in &self.floating_transfer_proxies {
            proxy.draw(&mut sections.above_quick_settings);
        }
        for (_, surface) in &mut self.floating_overlays {
            if surface.is_mapped() {
                surface.draw(
                    &mut sections.above_quick_settings,
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
                &mut sections.above_quick_settings,
                Affine::IDENTITY,
                &mut self.text_renderer,
                &self.theme,
                &self.presence,
            );
        }
        for (_, surface) in &mut self.transient_overlays {
            if surface.is_mapped() {
                surface.draw(
                    &mut sections.above_quick_settings,
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
                &mut sections.above_quick_settings,
                Affine::IDENTITY,
                &mut self.text_renderer,
                &self.theme,
                &self.presence,
            );
        }

        let hdr_layers = self.hdr_layers_to_scene(
            recording
                .map(crate::shell::hdr_surface::HdrLayerRecording::finish)
                .unwrap_or_default(),
        );
        let revision = self.next_scene_revision;
        self.next_scene_revision = self.next_scene_revision.saturating_add(1);
        let previous_revision = self
            .last_produced_sections
            .as_ref()
            .map(|last| last.revision);
        let global_filter = self.watch_configured_theme
            && (self.night_light
                || !matches!(
                    self.visual_accessibility.color_filter,
                    DesktopColorFilter::None
                )
                || self.visual_accessibility.magnifier.is_on()
                || self.visual_accessibility.cursor_crosshair);
        let damage_regions = (!global_filter)
            .then(|| self.damage_from_sections(&sections, global_filter))
            .flatten();

        let mut scene = Scene::new();
        scene.append(&sections.backdrop, None);
        scene.append(&sections.workspace, None);
        // OS chrome owns this layer. Its normal controls stay inside the
        // reserved top-bar band, while anchored surfaces such as the center
        // widget editor must remain above app tabs and client content.
        scene.append(&sections.topbar, None);
        scene.append(&sections.quick_settings, None);
        scene.append(&sections.above_quick_settings, None);
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
        let scene = self.to_scene(scene);

        self.last_produced_sections = Some(ProducedSections {
            backdrop: sections.backdrop,
            topbar: sections.topbar,
            workspace: sections.workspace,
            topbar_bounds: sections.topbar_bounds,
            quick_settings: sections.quick_settings,
            above_quick_settings: sections.above_quick_settings,
            quick_settings_bounds: sections.quick_settings_bounds,
            global_filter,
            revision,
        });
        FramePaint {
            scene,
            revision,
            previous_revision,
            damage_regions,
            hdr_layers,
        }
    }

    fn damage_from_sections(
        &self,
        current: &crate::shell::overlay_host::ShellSceneSections,
        current_global_filter: bool,
    ) -> Option<Vec<FineDamageRect>> {
        let previous = self.last_produced_sections.as_ref()?;
        if previous.global_filter || current_global_filter {
            return None;
        }
        if !previous.backdrop.encoding_eq(&current.backdrop)
            || !previous.workspace.encoding_eq(&current.workspace)
            || !previous
                .above_quick_settings
                .encoding_eq(&current.above_quick_settings)
        {
            return None;
        }
        let topbar_changed = !previous.topbar.encoding_eq(&current.topbar);
        let quick_settings_changed = !previous.quick_settings.encoding_eq(&current.quick_settings);
        if !topbar_changed && !quick_settings_changed {
            return None;
        }
        let mut regions = Vec::with_capacity(2);
        if topbar_changed {
            let old = self.topbar_damage_rect(&previous.topbar, previous.topbar_bounds)?;
            let new = self.topbar_damage_rect(&current.topbar, current.topbar_bounds)?;
            regions.extend([old, new]);
        }
        if quick_settings_changed {
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
        (!regions.is_empty()).then_some(regions)
    }

    fn topbar_damage_rect(&self, section: &Scene, bounds: Option<Rect>) -> Option<FineDamageRect> {
        // An empty topbar is valid only for a non-production/test root; because a changed empty
        // section has no pixels to repair, it must not be the sole sparse authority.
        if section.encoding().is_empty() {
            return None;
        }
        self.native_damage_rect(bounds)
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
        // Vello evaluates clip layers in 16px tiles. Even outside a layer's
        // exact shape, compositing that tile can round an existing channel by
        // one unit. Retain complete clip tiles so a changed shadow cannot
        // leave those neighbouring pixels from the previous frame behind.
        // Matches TILE_WIDTH/TILE_HEIGHT in vello_encoding/src/config.rs.
        const CLIP_TILE_EDGE: f64 = 16.0;
        // Sections are drawn in logical coordinates; the damage is in the
        // scene pixels each display drew them into.
        let rect = self.layout_rect_to_scene(rect?)?;
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
            DesktopColorFilter::Grayscale | DesktopColorFilter::GrayscaleInverted
        ) {
            // W3C `color` blend with an achromatic source preserves the
            // backdrop luminosity while replacing hue/saturation: true
            // grayscale, not a translucent grey veil.
            overlay(Mix::Color, Color::WHITE);
        }
        if matches!(
            self.visual_accessibility.color_filter,
            DesktopColorFilter::Inverted | DesktopColorFilter::GrayscaleInverted
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
        let automatic_theme = settings_store::str_at(&settings, "appearance", "theme")
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
        self.narrator
            .set_enabled(crate::shell::shell_narrator::screen_reader_setting());
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
        if !self.watch_configured_theme
            || std::env::var_os("TILO_SHOTS_THEME").is_some()
            || std::env::var_os("TILO_SHOTS_TEXT_SCALE").is_some()
        {
            return;
        }
        let appearance_generation = self.chrome.borrow().appearance_generation;
        let appearance_changed = appearance_generation != self.appearance_generation;
        if !appearance_changed && self.last_theme_store_check.elapsed() < Duration::from_millis(750)
        {
            return;
        }
        self.last_theme_store_check = Instant::now();
        let stamp = settings_store::settings_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
        let phase_changed = self
            .day_phase
            .is_some_and(|(schedule, night)| schedule.is_night(ui::theme::local_moment()) != night);
        if stamp == self.theme_store_stamp && !phase_changed && !appearance_changed {
            return;
        }
        self.theme_store_stamp = stamp;
        self.appearance_generation = appearance_generation;
        self.theme = Theme::configured();
        crate::shell::i18n::apply_configured_locale();
        self.refresh_display_comfort();
        self.rebuild_layout_tree();
        self.relayout();
    }

    /// Enable the session-wide configured theme and live settings watcher.
    /// Offscreen builders stay deterministic unless the shots override asks
    /// for a specific palette.
    pub fn enable_configured_theme(&mut self) {
        self.watch_configured_theme = true;
        self.theme = Theme::configured();
        crate::shell::i18n::apply_configured_locale();
        self.appearance_generation = self.chrome.borrow().appearance_generation;
        self.refresh_display_comfort();
        self.theme_store_stamp = settings_store::settings_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
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
        let Some(relay) = self.peer_presence.as_ref() else {
            return false;
        };
        let tiles = self.host.tab_tiles();
        let moved = relay.pump(&mut self.presence, &tiles);
        let roster_changed = relay.refresh_roster(&self.presence);
        moved || roster_changed
    }

    /// Read-only view of the presence the next paint will draw.
    pub fn presence(&self) -> &PresenceCtx {
        &self.presence
    }

    /// Put an authenticated remote-presence payload through the same fold
    /// that a received peer packet uses. Deterministic product proofs need
    /// this seam so they can render intent without standing up a network
    /// relay; it is unavailable in production-only builds.
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
        let demand = self.animation_demand();
        let mut owners: Vec<String> = crate::shell::animation_diagnostics::host_labels(demand.host)
            .filter(|label| *label != "content")
            .map(str::to_string)
            .collect();
        if demand.host & (1 << 10) != 0 {
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
        owners
    }

    pub(crate) fn animation_demand(&self) -> AnimationDemand {
        AnimationDemand {
            host: self.host.animation_demand_bits(Instant::now()),
            floating: self
                .floating_overlays
                .iter()
                .any(|(_, surface)| surface.needs_animation_frame()),
            transient: self
                .transient_overlays
                .iter()
                .any(|(_, surface)| surface.needs_animation_frame()),
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
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A round soon while a peer's window waits to be opened here: its room's
    /// update may land after this round's decision, with nothing left to wake
    /// an idle session for it (a window waiting on its record, too).
    fn peer_windows_poll(&self) -> Option<Instant> {
        let chrome = self.chrome.borrow();
        let tree = chrome.tree();
        crate::shell::peer_windows::any_pending(&tree)
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

    pub fn set_capture_recording_transition_progress(&mut self, permille: u16) {
        self.host
            .set_capture_recording_transition_progress(permille);
    }

    pub fn set_capture_pip_transition_progress(&mut self, permille: u16) {
        self.host.set_capture_pip_transition_progress(permille);
    }

    pub fn set_capture_recording_paused(&mut self, paused: bool) {
        self.host.set_capture_recording_paused(paused);
    }

    pub fn set_capture_recording_state_transition_progress(&mut self, permille: u16) {
        self.host
            .set_capture_recording_state_transition_progress(permille);
    }

    pub fn set_capture_recording_receipt_transition_progress(&mut self, permille: u16) {
        self.host
            .set_capture_recording_receipt_transition_progress(permille);
    }

    pub fn set_capture_recording_finished(&mut self) {
        self.host.set_capture_recording_finished();
    }

    pub fn set_capture_screenshot_finished(
        &mut self,
        copied: bool,
        width: u32,
        height: u32,
        preview: ui::widget::CapturePreview,
    ) {
        self.host
            .set_capture_screenshot_finished(copied, width, height, preview);
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

    /// Paint live selected apps in virtual-desktop coordinates without the
    /// physical output clip. This is the authoritative source for a full-app
    /// or whole-strip capture, including apps not presently on the monitor.
    pub fn paint_capture_targets(
        &mut self,
        target_ids: &[String],
        extent: ui::widget::CaptureExtent,
        complete_semantic_content: bool,
    ) -> (Scene, Vec<Rect>) {
        let mut scene = Scene::new();
        if self.session_locked {
            return (scene, Vec::new());
        }
        // This paint exists only to be captured, so protected surfaces draw
        // their placeholder wherever they are, on screen or not.
        let _capture = crate::shell::capture_protection::CapturePaint::begin();
        let rects = self.host.draw_capture_targets(
            &ui::widget::LiveCapture {
                target_ids,
                extent,
                complete_semantic_content,
            },
            &mut ui::paint::PaintCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut self.text_renderer,
                &self.theme,
            ),
            &self.presence,
        );
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
        let window_radius = f64::from(self.theme.tokens.radii.xl);
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
    fn enable_test_onboarding(&mut self, view: crate::onboarding_view::OnboardingView) {
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

    /// Install/remove the trusted keyboard mirror and anchor it to the output
    /// containing the currently focused field.
    pub fn set_osk_surface(
        &mut self,
        surface_id: Option<u64>,
        focused_surface: Option<u64>,
        active: bool,
    ) {
        let anchor = focused_surface
            .and_then(|id| self.host.foreign_surface_rect(id))
            .map(|rect| rect.center().x);
        self.osk_anchor_x = anchor;
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
    }

    /// Reconcile authenticated app-owned toplevels with the compositor's
    /// presentation pass. Widgets are retained by surface id so a geometry
    /// update never resets buffer sequence, mapping state, or opening chrome.
    pub fn set_floating_surfaces(&mut self, surfaces: &[(u64, String, Rect)]) {
        self.floating_overlays
            .retain(|(surface_id, _)| surfaces.iter().any(|(id, _, _)| id == surface_id));
        for (surface_id, app_id, rect) in surfaces {
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
                (surface.is_mapped() && rect.contains(point)).then_some((*surface_id, rect))
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
        state: &mut AutoCommit,
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
                (*widget).perform_action_at(action, &key_path, state, root_obj)
            } else {
                (*widget).perform_action(action, state, root_obj)
            }
        };
        if handled {
            if matches!(action, Action::Focus) {
                self.set_native_focus(focus, Some(taffy_id));
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
        state: &mut AutoCommit,
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
        ) || !self.active_tile_is_terminal()
        {
            return false;
        }
        // Super+Shift+Enter is DetachTab (W08). Super+Space is search.
        // Only unmodified Space/Enter belong on the live Terminal first.
        let mods = modifiers.state();
        if mods.super_key() || mods.shift_key() {
            return false;
        }
        let Some((&id, &widget)) = self
            .node_map
            .iter()
            .find(|(_, mapped)| unsafe { (***mapped).name() } == "Terminal")
        else {
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
                    state,
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
        state: &mut AutoCommit,
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
                    state,
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
            let activated =
                unsafe { (*widget).perform_action(&ui::a11y::Action::Activate, state, root_obj) };
            if activated {
                handled = true;
            }
        }
        // Match `ui::App`: unmodified Tab advances the global DFS focus order
        // only when the focused widget did not claim it. A terminal and a
        // multiline editor legitimately consume Tab as input; intercepting it
        // before dispatch made completion/indentation impossible. Ctrl+Tab and
        // other modified chords remain shell shortcuts.
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
        for (which, rect) in host.peer_sync_prompt_rects_for_tests() {
            put(format!("peer_sync_{which}"), rect);
        }
        // The collaborator dropdown's own controls, named so a scenario
        // clicks the button a person clicks rather than invoking the action
        // behind it — a click proves the control is wired, an invocation
        // only proves the action.
        let (collab_invite, collab_join) = host.collaborators_action_rects_for_tests();
        put("collab_invite".into(), collab_invite);
        put("collab_join".into(), collab_join);
        put(
            "collab_room_activity".into(),
            host.collaborators_activity_rect_for_tests(),
        );
        for (name, rect) in host.quick_settings_semantic_rects() {
            put(name, rect);
        }
        for (name, rect) in host.power_menu_semantic_rects() {
            put(name.into(), rect);
        }
    }
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
    out.insert(path.clone(), w.get_rect());
    if my_index == 0 {
        let bare = if parent_path.is_empty() {
            name
        } else {
            format!("{parent_path}/{name}")
        };
        out.entry(bare).or_insert_with(|| w.get_rect());
    }
    if let Some(id) = w.test_id() {
        out.insert(id.to_string(), w.get_rect());
    }
    // Affordances the widget paints itself. Skipping zero-area entries
    // is what makes a name mean "on screen now".
    for (name, rect) in w.test_rects() {
        if rect.area() > 0.0 {
            out.insert(format!("{path}/{name}"), rect);
            out.entry(name).or_insert(rect);
        }
    }
    let mut child_counts: HashMap<String, usize> = HashMap::new();
    for child in w.children() {
        walk_widget(child, &path, &mut child_counts, out);
    }
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
    use crate::shell::state::FreshId as _;
    use automerge::ReadDoc;
    use automerge::transaction::Transactable;
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{KeyLocation, ModifiersState, PhysicalKey};

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn workspace_switches_keep_record_rooms_chevrons_and_passive_activity() {
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
                    .room;
                let destination_room = workspaces
                    .iter()
                    .find(|workspace| workspace.id == destination_id)
                    .expect("destination workspace")
                    .room;
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
                &mut document,
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
                &mut document,
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
                source.owner = None;
                source.capabilities.clear();
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

        /// The top bar's workspace chevrons follow the *live* active workspace:
        /// the bar refreshes `workspace_index` in `Widget::update` when the
        /// chrome revision bumps. Drives the per-frame path (`pump_apps` → paint)
        /// and reads chevron state the way a scenario would.
        fn topbar_workspace_chevrons_track_the_live_active_workspace() {
            let mut fb = FrameBuilder::new((1600, 900));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mut frame = |fb: &mut FrameBuilder| {
                fb.pump_apps(&mut doc, &root);
                fb.relayout();
                let _ = fb.paint();
                fb.widget_registry()
            };

            let reg = frame(&mut fb);
            assert!(
                reg.contains_key("topbar_workspace_up"),
                "the up-chevron click target is always published"
            );
            assert!(
                !reg.contains_key("topbar_workspace_up_enabled"),
                "booted on the first workspace: nothing above it, up must read disabled"
            );
            let booted_with_more_below = fb.chrome().tree().desktop.workspaces.len() > 1;
            assert_eq!(
                reg.contains_key("topbar_workspace_down_enabled"),
                booted_with_more_below,
                "down reads enabled exactly when a workspace follows the active one"
            );

            // A new workspace is appended last and made active, so the one
            // we booted on is now above us: up must go live, down must not.
            fb.with_chrome(|st| {
                st.add_workspace("second");
            });
            let reg = frame(&mut fb);
            assert!(
                reg.contains_key("topbar_workspace_up_enabled"),
                "after switching to a later workspace the up-chevron must read enabled"
            );
            assert!(
                !reg.contains_key("topbar_workspace_down_enabled"),
                "on the last workspace the down-chevron must read disabled"
            );

            // One step back up: still below the boot workspace, so both
            // chevrons are live — the bar must track each switch, not just
            // the first one after it was built.
            fb.with_chrome(|st| {
                assert!(st.switch_workspace(-1));
            });
            let reg = frame(&mut fb);
            let idx = |fb: &FrameBuilder| {
                let tree = fb.chrome().tree().clone();
                tree.desktop
                    .workspaces
                    .iter()
                    .position(|w| w.id == tree.desktop.active)
                    .expect("active workspace is in the tree")
            };
            assert!(
                idx(&fb) > 0,
                "one step up from the last workspace cannot be the first"
            );
            assert!(
                reg.contains_key("topbar_workspace_down_enabled"),
                "with the new workspace below, the down-chevron must read enabled"
            );

            // All the way back to the top: up goes dark, down stays live.
            fb.with_chrome(|st| while st.switch_workspace(-1) {});
            let reg = frame(&mut fb);
            assert_eq!(idx(&fb), 0);
            assert!(
                !reg.contains_key("topbar_workspace_up_enabled"),
                "back on the first workspace the up-chevron must read disabled again"
            );
            assert!(
                reg.contains_key("topbar_workspace_down_enabled"),
                "with a workspace below, the down-chevron must read enabled"
            );
        }

        record_bring_keeps_its_authorized_room_across_workspace_switch();
        startup_keeps_unread_activity_passive();
        topbar_workspace_chevrons_track_the_live_active_workspace();
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
            use collab_types::caps::{CapOverride, Scope, Subject, Verb};

            // A room owned by somebody else, with something in it.
            let owner = "quinten".to_string();
            let mut host = WorkspaceTree::default();
            let host_ws = host.append_empty_workspace("Shared");
            host.adopt_ownerless_workspaces(&owner);
            let room = host
                .room_of(host_ws)
                .expect("the host's workspace has a room");
            host.grant_in_workspace(
                host_ws,
                &owner,
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath("/shared".to_string()),
                    Verb::Read,
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
                tree.set_room(active, room)
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

            fb.set_osk_surface(Some(SURFACE), None, false);
            assert!(fb.osk_surface_hit((800.0, 700.0)).is_none());

            fb.set_osk_surface(Some(SURFACE), None, true);
            let (id, rect) = fb
                .osk_surface_hit((800.0, 700.0))
                .expect("active keyboard is hit-testable");
            assert_eq!(id, SURFACE);
            assert_eq!(rect.width(), 960.0);
            assert_eq!(rect.y1, 880.0);

            fb.set_osk_surface(Some(SURFACE), None, false);
            assert!(fb.osk_surface_hit((800.0, 700.0)).is_none());
            crate::shell::foreign::store().forget(SURFACE);
        }

        fn hdr_surfaces_are_recorded_as_frame_layers() {
            const SURFACE: u64 = 9_900_702;
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
            fb.set_osk_surface(Some(SURFACE), None, true);

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

            crate::shell::foreign::store().publish_rgba(SURFACE, &[40; 16], 2, 2);
            assert!(
                fb.paint_with_damage().hdr_layers.is_empty(),
                "an SDR frame has none"
            );
            crate::shell::foreign::store().forget(SURFACE);
        }

        native_damage_covers_clip_tiles_and_clamps_the_viewport();
        transfer_proxy_covers_both_ownership_edges_until_destination_pixels_exist();
        night_light_is_neutral_at_daylight_and_warmer_at_lower_kelvin();
        osk_overlay_is_bottom_anchored_and_active_only();
        hdr_surfaces_are_recorded_as_frame_layers();
    }

    fn key_press(key: Key) -> WindowEvent {
        // SAFETY: winit keeps platform-private key metadata inaccessible to
        // constructors. The production path under test reads only the public
        // fields initialized immediately below.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = key;
        event.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
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
        fb.dispatch_event(&press, &chord, &mut doc, &root, &mut focus);

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

        fb.dispatch_event(&press, &chord, &mut doc, &root, &mut focus);
        assert_eq!(
            fb.chrome.borrow_mut().take_input_source_request(),
            Some(0),
            "the sources wrap around"
        );
    }

    #[test]
    fn super_s_moves_the_wheel_disc_to_the_pointer() {
        let mut fb = FrameBuilder::new((1600, 900));
        let _ = fb.paint_settled();
        let before = *fb
            .widget_registry()
            .get("wheel_disc")
            .expect("the docked wheel is addressable");
        let at = Point::new(1180.0, 450.0);
        fb.host_mut().note_pointer(at);
        let chord = Modifiers::from(ModifiersState::SUPER);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut focus = None;
        assert!(
            fb.dispatch_event(
                &key_press(Key::Character("s".into())),
                &chord,
                &mut doc,
                &root,
                &mut focus
            ),
            "Super+S is handled by the shell"
        );
        let summoned = fb
            .chrome()
            .wheel_summon()
            .expect("Super+S writes chrome.wheel_summon");
        assert!(
            (summoned.x - at.x).abs() < 1.0 && (summoned.y - at.y).abs() < 1.0,
            "summon follows the pointer, got {summoned:?}"
        );
        let _ = fb.paint_settled();
        let after = *fb
            .widget_registry()
            .get("wheel_disc")
            .expect("the summoned wheel stays addressable");
        assert!(
            (after.x1 - at.x).abs() < 2.0,
            "the rim's right edge sits on the pointer so a shot at that point sees the wheel; \
             got x1={} for pointer x={}",
            after.x1,
            at.x
        );
        assert!(
            (after.x0 - before.x0).abs() > 50.0,
            "the disc actually moved: docked {before:?} summoned {after:?}"
        );
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

    fn onboarding_frame() -> (FrameBuilder, tempfile::TempDir) {
        let identity_dir = tempfile::tempdir().expect("temporary identity directory");
        let mut fb = FrameBuilder::new((1280, 800));
        let identity_path = identity_dir.path().join("primary.json");
        fb.enable_test_onboarding(crate::onboarding_view::OnboardingView::with_minter(
            Box::new(move || {
                Ok(crate::onboarding::MintedIdentity {
                    fingerprint: "ab".repeat(32),
                    path: identity_path.clone(),
                })
            }),
        ));
        fb.relayout();
        (fb, identity_dir)
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
            assert_eq!(focused_name(&fb, focus).as_deref(), Some("Use existing"));
        }

        fn native_oobe_exports_named_roles_state_and_focus_to_accesskit() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            fb.reconcile_native_focus(&mut focus);

            let update = fb.build_a11y_tree_update(focus, 1.0);
            let (existing_id, existing) = exported_node_named(&update, "Use existing")
                .expect("existing-identity action reaches the external tree");
            let (_, create) = exported_node_named(&update, "Create identity")
                .expect("create-identity action reaches the external tree");
            // Identity setup has distinct backup and final hand-off states, so
            // its semantic progress surface contains four stages.
            let (_, progress) = exported_node_named(&update, "Identity setup, step 1 of 4: Choose")
                .expect("the OOBE progress state reaches the external tree");

            assert_eq!(existing.role(), ui::accesskit::Role::Button);
            assert_eq!(create.role(), ui::accesskit::Role::Button);
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
            let (existing_id, _) = exported_node_named(&intro, "Use existing")
                .expect("the external tree exposes the Connect-existing action");
            assert!(fb.dispatch_a11y_action(
                existing_id,
                &ui::a11y::Action::Activate,
                &mut doc,
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
                &mut doc,
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
                &mut doc,
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
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            fb.reconcile_native_focus(&mut focus);
            let intro = fb.build_a11y_tree_update(focus, 1.0);

            doc.put(&automerge::ROOT, crate::onboarding_view::KEY_BEGIN, true)
                .expect("begin identity setup");
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);
            let protect = fb.build_a11y_tree_update(focus, 1.0);

            assert!(
                exported_node_named(&protect, "Use existing").is_none(),
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
                &mut doc,
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
                &mut doc,
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(focused_name(&fb, focus).as_deref(), Some("Create identity"));
            assert!(fb.dispatch_event(
                &key_press(Key::Named(NamedKey::Enter)),
                &Modifiers::default(),
                &mut doc,
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(
                doc.get(&automerge::ROOT, crate::onboarding_view::KEY_BEGIN)
                    .expect("read begin action")
                    .and_then(|(value, _)| value.as_bool()),
                Some(true),
                "activation must write the begin action into the document"
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
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            for key in [
                crate::onboarding_view::KEY_BEGIN,
                crate::onboarding_view::KEY_UNDERSTOOD,
            ] {
                doc.put(&automerge::ROOT, key, true).expect("advance OOBE");
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
            doc.put(&automerge::ROOT, crate::onboarding_view::KEY_CREATE, true)
                .expect("create identity");
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
            assert!(
                names
                    .iter()
                    .any(|name| name.contains("without an encrypted backup")),
                "skip acknowledgement must be keyboard reachable: {names:?}"
            );

            let acknowledgement = fb
                .focusable_order()
                .into_iter()
                .find(|id| {
                    focused_name(&fb, Some(*id))
                        .is_some_and(|name| name.contains("without an encrypted backup"))
                })
                .expect("acknowledgement focus target");
            fb.set_native_focus(&mut focus, Some(acknowledgement));
            let enter = key_press(Key::Named(NamedKey::Enter));
            assert!(fb.dispatch_event(
                &enter,
                &Modifiers::default(),
                &mut doc,
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
                &mut doc,
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
                &mut doc,
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(focused_name(&fb, focus).as_deref(), Some("Create identity"));

            let shift = Modifiers::from(ModifiersState::SHIFT);
            assert!(fb.dispatch_event(&tab, &shift, &mut doc, &automerge::ROOT, &mut focus,));
            assert_eq!(focused_name(&fb, focus).as_deref(), Some("Use existing"));
        }

        fn native_oobe_focus_order_excludes_disabled_controls() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut doc = AutoCommit::new();
            doc.put(&automerge::ROOT, crate::onboarding_view::KEY_BEGIN, true)
                .expect("begin identity setup");
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
                (unsafe { (**widget).name() } == "Terminal").then_some(*id)
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
                &mut doc,
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
        fb.relayout();
        let mut focus = None;
        fb.reconcile_native_focus(&mut focus);
        assert_eq!(
            fb.widget_name(focus.expect("a terminal tile must take initial focus")),
            Some("Terminal"),
            "chrome must not keep initial focus on a workspace pill"
        );
        let probe = fb.node_map.values().find_map(|widget| {
            // SAFETY: node_map pointers are owned by the live widget tree.
            let widget = unsafe { &mut **widget };
            (widget.name() == "Terminal")
                .then(|| widget.as_any_mut())
                .flatten()
                .and_then(|any| any.downcast_mut::<terminal::TerminalWidget>())
                .map(|term| term.attach_input_probe())
        });
        let Some(probe) = probe else {
            panic!("production chrome must mount a hosted Terminal");
        };
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
                    &mut doc,
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
            "this case is the VM path: chrome, not the Terminal, holds focus"
        );
        let probe = fb.node_map.values().find_map(|widget| {
            let widget = unsafe { &mut **widget };
            (widget.name() == "Terminal")
                .then(|| widget.as_any_mut())
                .flatten()
                .and_then(|any| any.downcast_mut::<terminal::TerminalWidget>())
                .map(|term| term.attach_input_probe())
        });
        let Some(probe) = probe else {
            panic!("production chrome must mount a hosted Terminal");
        };
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
                    &mut doc,
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
            (widget.name() == "Terminal")
                .then(|| widget.as_any_mut())
                .flatten()
                .and_then(|any| any.downcast_mut::<terminal::TerminalWidget>())
                .map(|term| term.attach_input_probe())
        });
        let Some(probe) = probe else {
            panic!("production chrome must mount a hosted Terminal");
        };
        let groups_before = {
            let chrome = fb.chrome();
            chrome
                .tree()
                .desktop
                .active_workspace()
                .map(|ws| {
                    ws.strips
                        .iter()
                        .map(|s| s.window_groups.len())
                        .sum::<usize>()
                })
                .unwrap_or(0)
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
                .map(|g| g.tabs.len())
                .unwrap_or(0)
        };
        let mut doc = AutoCommit::new();
        let chord = Modifiers::from(ModifiersState::SUPER | ModifiersState::SHIFT);
        assert!(
            fb.dispatch_event(
                &key_press(Key::Named(NamedKey::Enter)),
                &chord,
                &mut doc,
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
            chrome
                .tree()
                .desktop
                .active_workspace()
                .map(|ws| {
                    ws.strips
                        .iter()
                        .map(|s| s.window_groups.len())
                        .sum::<usize>()
                })
                .unwrap_or(0)
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
                .map(|g| g.tabs.len())
                .unwrap_or(0)
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
                &mut doc,
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
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            fb.reconcile_native_focus(&mut focus);
            assert_eq!(focused_name(&fb, focus).as_deref(), Some("Use existing"));

            doc.put(&automerge::ROOT, crate::onboarding_view::KEY_BEGIN, true)
                .expect("begin identity setup");
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);

            assert!(
                focused_name(&fb, focus).is_some_and(|name| name.contains("I understand")),
                "focus must move into the replacement OOBE step"
            );
        }

        fn native_oobe_step_transition_keeps_the_new_page_at_its_top() {
            let (mut fb, _identity_dir) = onboarding_frame();
            let mut focus = None;
            let mut doc = AutoCommit::new();
            fb.reconcile_native_focus(&mut focus);

            doc.put(&automerge::ROOT, crate::onboarding_view::KEY_CONNECT, true)
                .expect("open identity restore");
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
                &mut doc,
                &automerge::ROOT,
                &mut focus,
            ));
            assert_eq!(focused_name(&fb, focus).as_deref(), Some("Create identity"));

            fb.rebuild_layout_tree();
            fb.relayout();
            fb.reconcile_native_focus(&mut focus);
            assert_eq!(
                focused_name(&fb, focus).as_deref(),
                Some("Create identity"),
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
            let room = tree.desktop.workspaces[0].room;
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

        /// Super+F on a Tilo window covers the wheel. Hosted apps are xdg
        /// clients; chrome fullscreen still wins so W07 sees
        /// `fullscreen_group`. Maximize stays beside the wheel.
        fn super_f_on_a_tilo_group_covers_the_wheel() {
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
                "Super+F must cover the wheel; row width {} vs output {}",
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

        restored_native_tab_gets_a_configure_rect_before_its_first_buffer();
        set_fullscreen_promotes_group_and_restores_on_exit();
        set_fullscreen_with_unknown_group_does_not_blank_shell();
        super_f_on_a_tilo_group_covers_the_wheel();
    }

    #[test]
    fn overview_and_workspace_switch_request_a_run_of_frames() {
        fn overview_toggle_requests_frames_before_draw() {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            fb.with_chrome(|st| {
                st.overlays.overview = true;
                st.bump();
            });
            // Latch on the host before update, as Super+Tab does.
            fb.host_mut().latch_overview_transition();
            assert!(
                fb.animation_owners()
                    .iter()
                    .any(|owner| owner == "root-transition"),
                "overview must request frames on the chord, not only after draw; owners {:?}",
                fb.animation_owners()
            );
            let mut doc = AutoCommit::new();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            assert!(
                fb.animation_owners()
                    .iter()
                    .any(|owner| owner == "root-transition"),
                "overview must request frames on the update that sees the toggle, not only after draw; owners {:?}",
                fb.animation_owners()
            );
        }

        fn workspace_switch_requests_frames_before_draw() {
            let mut fb = FrameBuilder::new((1600, 900));
            let _ = fb.paint_settled();
            let mut switched = false;
            fb.with_chrome(|st| {
                switched = st.switch_workspace(1);
            });
            assert!(switched, "the demo tree has a second workspace");
            // Latch on the host before update, as Super+PageDown does.
            fb.host_mut().start_workspace_switch_frames();
            assert!(
                fb.animation_owners()
                    .iter()
                    .any(|owner| owner == "workspace-switch"),
                "a workspace switch must request frames on the chord, not only after draw; owners {:?}",
                fb.animation_owners()
            );
            let mut doc = AutoCommit::new();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            assert!(
                fb.animation_owners()
                    .iter()
                    .any(|owner| owner == "workspace-switch"),
                "a workspace switch must keep requesting a run of frames; owners {:?}",
                fb.animation_owners()
            );
        }

        overview_toggle_requests_frames_before_draw();
        workspace_switch_requests_frames_before_draw();
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
                    .map(|workspace| chrome.local_user_for_room(&workspace.room))
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
                &mut document,
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
            .unwrap();
        };

        invoke("appearance.theme.light");
        let _ = frame.paint();
        let light = panel_ground(&frame.theme);
        let light_bg = frame.theme.bg_app;
        let light_y = relative_luminance(light);

        invoke("appearance.theme.dark");
        let _ = frame.paint();
        let dark = panel_ground(&frame.theme);
        let dark_bg = frame.theme.bg_app;
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
            values.insert("theme".into(), serde_json::Value::String(theme.into()));
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
        let light_bg = frame.theme.bg_app;
        let light_y = relative_luminance(light);

        write("dark");
        frame.with_chrome(ChromeState::note_appearance_change);
        let _ = frame.paint();
        let dark = panel_ground(&frame.theme);
        let dark_bg = frame.theme.bg_app;
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
}

#[cfg(test)]
#[path = "frame_retained_tests.rs"]
mod retained_tests;
