//! A toolkit widget shown on a rectangle of the desktop: a surface that is
//! a widget tree of its own rather than an app (the desktop's strips and
//! window bars). The toolkit's portable host runs it (input in, frames out)
//! and the frame is drawn as an app's is.

use tilo_session_core::surface::WindowPicture;
use tilo_session_core::surface::{HostIme, HostLayer, HostTextInput, SurfaceHost};

use ui::headless::FrameHarness;
use ui::retained_raster::{Repaint, RetainedFrames, TargetKey};
use vello::kurbo::Affine;
use vello::peniko::Color;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};

use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_session_core::surface::HostKey;
use winit::event::Modifiers;
use winit::keyboard::ModifiersState;

use crate::frames::Frames;
use crate::{Placement, ime_event, key_event, mouse_input};
use tilo_desktop_surface::DesktopSurface;
use ui::widget::Widget;

/// The colour a widget tree draws on: nothing, so the desktop shows through
/// wherever it painted nothing.
const TRANSPARENT: Color = Color::TRANSPARENT;

/// The display's frame grid: when the next frame of what only moves is due,
/// locked to the cadence the displays run at rather than to the loop's turns.
#[derive(Clone, Copy, Debug)]
struct Cadence {
    /// How long the displays hold a frame; nil until one says, when every turn
    /// is a frame, as a tree with no display under it drew before.
    interval: Duration,
    /// When the next frame is due, once one was drawn.
    next: Option<Instant>,
}

impl Cadence {
    /// Every turn is a frame until a display says how long it holds one.
    const fn new() -> Self {
        Self {
            interval: Duration::ZERO,
            next: None,
        }
    }

    /// The displays hold each frame for `interval`.
    fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// Whether a turn at `now` comes before the grid: a frame it drew of what
    /// only moves would show nothing the last one did not.
    fn early(&self, now: Instant) -> bool {
        self.next.is_some_and(|next| now < next)
    }

    /// When a frame of what only moves is next due: the grid while it holds,
    /// and now once it has passed.
    fn due(&self, now: Instant) -> Instant {
        self.next.unwrap_or(now).max(now)
    }

    /// A frame was drawn at `now`: the grid moves on one display frame. A
    /// frame drawn late skips the deadlines it missed instead of queueing
    /// frames to catch up.
    fn drawn(&mut self, now: Instant) {
        let from = match self.next {
            Some(next) if now < next => next,
            _ => now,
        };
        self.next = Some(from.checked_add(self.interval).unwrap_or(from));
    }
}

/// What a widget tree's next frame is for.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Owed {
    /// Nothing moved: the tree draws no frame.
    #[default]
    Nothing,
    /// Input arrived, the placement or the theme changed, or the tree queued
    /// work of its own.
    Frame,
    /// Blurred backdrops arrived: the tree draws its material again, so the
    /// images replace the placeholders its last frame baked in.
    Material,
}

/// One widget tree shown on a rectangle of the desktop.
pub struct WidgetHost {
    host: FrameHarness,
    placement: Placement,
    frames: Frames,
    /// The frame the layer shows, to know what of the next one changed.
    retained: RetainedFrames,
    /// Moves whenever the layer may show pixels of another frame entirely.
    generation: u64,
    scene: vello::Scene,
    below: bool,
    /// Set by the tree while keys should come to it rather than to the
    /// focused window (an open menu or field).
    keyboard: Option<Rc<dyn Fn() -> bool>>,
    /// The modifiers the tree last heard of.
    modifiers: ModifiersState,
    /// The tree covers the outputs' bounding box, following them.
    covers: bool,
    /// Other clients' windows the tree names, drawn by the compositor in
    /// their own pixels: a surface showing other apps' windows (the
    /// workspace overview) never draws their buffers itself.
    pictures: Option<Rc<dyn Fn() -> Vec<WindowPicture>>>,
    /// The regions the tree drew translucent material in last frame, in its
    /// physical pixels: bounds, blur deviation, texel size.
    backdrops: Vec<(vello::kurbo::Rect, f64, (u32, u32))>,
    /// The blurred backdrops the compositor made, by region.
    provided: Vec<Option<ui::peniko::ImageData>>,
    /// What the next frame is for, if the tree owes one.
    owed: Owed,
    /// Moves with every frame the tree drew: what it publishes changes only
    /// with what it shows, so a reader of its resources reads them again
    /// only when this moved.
    revision: u64,
    /// The wells the tree's places report to as it lays them out: shared
    /// with the window policy placing the windows they stand for
    /// ([`crate::WellPolicy`]).
    wells: Option<ui::widget::Wells>,
    /// The previews the tree's views report to as it lays them out: shared
    /// with the closures that name the windows it pictures
    /// ([`SurfaceHost::window_pictures`]).
    previews: Option<ui::widget::Previews>,
    /// What an assistive technology knows the tree's window by.
    name: Option<String>,
    /// Something outside the tree asked for its next pass without holding the
    /// host to ask (a command queued on the desk's handle): the frame that
    /// answers it.
    asked: Rc<std::cell::Cell<bool>>,
    /// The display's grid: what the tree moves through is drawn once per
    /// display frame, so the screens show as many frames as they run at.
    cadence: Cadence,
    /// Fingers are carrying the desk's strips: the surface draws them
    /// following the hand until it lets go.
    hand_on_deck: bool,
    /// What the role that placed the tree closes when the pointer is pressed
    /// elsewhere: the shared handle runs it before the tree is borrowed, so
    /// the role may ask the layer it holds to draw again.
    dismissed: Option<Rc<dyn Fn()>>,
}

impl WidgetHost {
    /// Show `root` at `placement`, above the windows or (`below`) under
    /// them. Nothing is drawn before the first
    /// [`SurfaceHost::prepare_frame`].
    ///
    /// # Panics
    /// Never: the placement's scale is a positive output scale.
    #[must_use]
    pub fn new(root: Box<dyn ui::Widget>, placement: Placement, below: bool) -> Self {
        let mut host = FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            root,
            placement.size,
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId(String::new()),
        );
        host.set_caller_palette_enabled(false);
        // This host presents on its own frame clock, the compositor's, as a
        // browser page does on animation frames: a word typed, a drag or a
        // wheel arrives faster than frames, so the key down, the text and the
        // key up of one keystroke reach the tree's next frame instead of
        // three of their own. Each event still reaches the widgets at once,
        // against the layout the person saw.
        host.set_coalesce_input_frames(true);
        // Each frame is compared with the one the layer shows, so only what
        // changed is drawn again.
        host.track_scene_parts(true);
        let _ = host.set_input_scale(placement.scale);
        let size = placement.physical_size();
        Self {
            host,
            placement,
            frames: Frames::new(size),
            retained: RetainedFrames::default(),
            generation: 0,
            scene: vello::Scene::new(),
            below,
            keyboard: None,
            pictures: None,
            backdrops: Vec::new(),
            provided: Vec::new(),
            modifiers: ModifiersState::empty(),
            covers: false,
            owed: Owed::Frame,
            revision: 0,
            wells: None,
            previews: None,
            name: None,
            asked: Rc::new(std::cell::Cell::new(false)),
            cadence: Cadence::new(),
            hand_on_deck: false,
            dismissed: None,
        }
    }

    /// The flag something outside the tree sets to ask for its next pass: what
    /// wakes the host holding a desk another surface commands, without holding
    /// the host to ask.
    #[must_use]
    pub fn asking(&self) -> Rc<std::cell::Cell<bool>> {
        Rc::clone(&self.asked)
    }

    /// Report the wells of a tree's places ([`ui::widget::WindowWell`]) to
    /// `wells` as it lays them out: the same handle the window policy
    /// placing the windows they stand for reads.
    pub fn install_wells(&mut self, wells: Option<ui::widget::Wells>) {
        self.wells = wells;
    }

    /// Report the previews of a tree's windows ([`ui::widget::WindowPreview`])
    /// to `previews` as it lays them out: the same handle the pictures the
    /// host hands the compositor are read from.
    pub fn install_previews(&mut self, previews: Option<ui::widget::Previews>) {
        self.previews = previews;
    }

    /// Lay the tree out with the wells and the previews in effect, as `work`
    /// runs it: a placement made then reports to them, so a policy reading
    /// them in the same frame sees the windows where the tree just laid them
    /// out. What `work` draws, it draws over this tree's own backdrops.
    fn with_reports<T>(&mut self, work: impl FnOnce(&mut FrameHarness) -> T) -> T {
        let _wells = self.wells.clone().map(ui::widget::window_well::install);
        let _previews = self
            .previews
            .clone()
            .map(ui::widget::window_preview::install);
        // Translucent material records what lies below it; the compositor
        // blurs that and hands it back (`set_backdrops`). The images are
        // this tree's, and the toolkit holds one tree's at a time: every
        // pass that may draw provides them first, a layout asked for
        // between frames as much as a frame. A pass that drew without them
        // drew its material over the stand-in, and kept it: the compositor
        // hands a backdrop over again only when what lies below changes.
        ui::backdrop::provide_backdrops(
            self.provided
                .iter()
                .enumerate()
                .filter_map(|(slot, image)| Some((slot, image.clone()?)))
                .collect(),
        );
        work(&mut self.host)
    }

    /// Cover every output, following them as they come and go: chrome
    /// over the whole desktop.
    #[must_use]
    pub const fn covering_outputs(mut self) -> Self {
        self.covers = true;
        self
    }

    /// Draw the windows `pictures` names over the tree, where the tree's
    /// drawing puts them: what the compositor shows of a window's own
    /// pixels.
    #[must_use]
    pub fn with_window_pictures(mut self, pictures: Rc<dyn Fn() -> Vec<WindowPicture>>) -> Self {
        self.pictures = Some(pictures);
        self
    }

    /// Name the tree's window for assistive technology: what a screen reader
    /// says the person reached.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Take keys from the focused window while `wants` holds: the tree
    /// sets it while a menu or field of its own is open.
    #[must_use]
    pub fn with_keyboard(mut self, wants: Rc<dyn Fn() -> bool>) -> Self {
        self.keyboard = Some(wants);
        self
    }

    /// Close what the role has open when the pointer is pressed elsewhere.
    #[must_use]
    pub fn with_dismissal(mut self, dismissed: Rc<dyn Fn()>) -> Self {
        self.dismissed = Some(dismissed);
        self
    }

    /// The toolkit's host of the widget tree, to drive it directly.
    pub fn host_mut(&mut self) -> &mut FrameHarness {
        &mut self.host
    }

    /// The toolkit's host of the widget tree, to read what it says of itself without
    /// changing it.
    #[must_use]
    pub const fn host(&self) -> &FrameHarness {
        &self.host
    }

    /// Whether the desk's fingers are already carrying its strips: the travel
    /// that opened a gesture then moves the deck it landed on instead of
    /// opening another.
    #[must_use]
    pub const fn wants_hand_on_deck(&self) -> bool {
        self.hand_on_deck
    }

    /// Fingers landed on the deck, `travel` pixels into the gesture.
    pub fn begin_strip_deck(&mut self, travel: f64) {
        self.hand_on_deck = true;
        self.deck_drag(travel, true);
    }

    /// The fingers have carried the deck `travel` pixels since they landed.
    pub fn drag_strip_deck(&mut self, travel: f64) {
        self.deck_drag(travel, false);
    }

    /// The fingers lifted after carrying the deck `travel` pixels.
    pub fn end_strip_deck(&mut self, travel: f64) {
        self.hand_on_deck = false;
        self.deck_drag(travel, false);
    }

    /// Draw the desk's strips following the fingers.
    fn deck_drag(&mut self, travel: f64, began: bool) {
        self.with_surface(|surface| {
            let shown = surface.shown_strip_id();
            let slides = surface.slides_mut();
            if began {
                slides.begin_lift(shown);
            }
            slides.lift_to(travel);
        });
        self.owe();
    }

    /// Three fingers landed on the strip shown, `travel` pixels into the
    /// gesture: the strip follows them from here.
    pub fn begin_strip_carry(&mut self, travel: f64) {
        self.with_surface(|surface| {
            surface.carry_strip(travel, true);
        });
        self.owe();
    }

    /// The fingers have carried the shown strip `travel` pixels since they
    /// landed: it moves with them, one pixel of their travel to one of its
    /// own, against the rubber band past either end.
    pub fn drag_strip_carry(&mut self, travel: f64) {
        self.with_surface(|surface| {
            surface.carry_strip(travel, false);
        });
        self.owe();
    }

    /// The fingers lifted (or the hand was taken back): the strip coasts on
    /// the velocity they let go at and comes to rest, focusing the window it
    /// left in view.
    pub fn end_strip_carry(&mut self, cancelled: bool) {
        self.with_surface(|surface| {
            surface.release_strip(cancelled);
        });
        self.owe();
    }

    /// The desktop surface the tree draws, when it draws one: the strips,
    /// window bars and edges, whatever wraps them (the desk's own widgets
    /// and launchpad) around.
    fn with_surface<R>(&mut self, work: impl FnOnce(&mut DesktopSurface) -> R) -> Option<R> {
        self.with_reports(|host| {
            let mut walk: Vec<&mut dyn Widget> = vec![host.root_widget_mut()];
            while let Some(next) = walk.pop() {
                if let Some(surface) = next
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<DesktopSurface>())
                {
                    return Some(work(surface));
                }
                walk.extend(next.children_mut());
            }
            None
        })
    }

    /// Where what the tree publishes stands: it may have changed only when
    /// this moved since it was last read.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the tree has a frame to draw by what a live host draws for: input, a new
    /// placement or layout ([`Self::relayout`]), queued work (a wake, an invocation), a motion
    /// or a deadline that came due. A widget reporting itself dirty is not a reason on its own:
    /// leaves report so by default (a text field always does), so a host that drew for it would
    /// draw every frame while one is shown. A tree fed only through these (a tilolang view's
    /// patches arrive as a relayout) asks this before [`SurfaceHost::prepare_frame`].
    pub fn wants_frame(&mut self) -> bool {
        if self.owed != Owed::Nothing || self.asked.replace(false) {
            return true;
        }
        self.host.frame_schedule().queued_work
            || self
                .next_deadline()
                .is_some_and(|deadline| deadline <= Instant::now())
    }

    /// Where the tree is shown.
    pub const fn placement(&self) -> Placement {
        self.placement
    }

    /// A frame is owed for what just moved, unless the tree already owes one
    /// of its material: that frame draws what moved as well.
    pub(crate) fn owe(&mut self) {
        if self.owed == Owed::Nothing {
            self.owed = Owed::Frame;
        }
    }

    /// Show the tree on another rectangle or at another scale.
    pub fn set_placement(&mut self, placement: Placement) {
        if placement == self.placement {
            return;
        }
        if placement.size != self.placement.size {
            let _ = self.host.resize(placement.size);
        }
        let _ = self.host.set_input_scale(placement.scale);
        let size = placement.physical_size();
        if size != self.placement.physical_size() {
            self.frames.resize(size);
            // What the tree drew for the old size says nothing of this one.
            self.generation = self.generation.wrapping_add(1);
            self.retained.forget();
        }
        self.placement = placement;
        self.owe();
    }

    /// Draw the tree with `theme`: the person's palette, tokens and fonts,
    /// as the session's presentation settings hand them to every surface.
    pub fn set_theme(&mut self, theme: ui::Theme) {
        self.host.set_theme(theme);
        self.owe();
    }

    /// Lay the tree out and draw it now, whether or not a frame is due:
    /// what a caller reading its layout needs first.
    pub fn relayout(&mut self) {
        self.with_reports(ui::headless::FrameHarness::relayout);
        self.owe();
    }

    /// Run the tree's own pass — what it takes up, and the layout it lays
    /// itself out by — without building or drawing a scene, for a host whose
    /// drawing stands behind another's ([`WidgetHost`]). Whether the tree moved,
    /// so the frame it stands under is drawn again.
    ///
    /// Only what is asked of it brings it a pass: a command queued on its
    /// handle, an invocation or a change it owes. A clock or a motion its views
    /// would draw brings none, as nothing of them shows — an idle host behind
    /// another's drawing asks for no frame at all.
    pub fn advance(&mut self) -> bool {
        let queued = self.host.frame_schedule().queued_work;
        let asked = self.asked.replace(false);
        if !asked && !queued && self.owed == Owed::Nothing {
            return false;
        }
        self.owed = Owed::Nothing;
        self.with_reports(ui::headless::FrameHarness::relayout);
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// A rectangle of the tree's own logical pixels, as the desktop's
    /// logical `(x, y, w, h)`: the tree lays itself out in logical pixels
    /// whatever scale it is drawn at, so only where the surface stands is
    /// added. Dividing by the display's scale as well would halve every
    /// control on a 2 display, and a press at its centre would land past it.
    fn logical(&self, rect: vello::kurbo::Rect) -> (f64, f64, f64, f64) {
        let (ox, oy) = self.placement.origin;
        (
            f64::from(ox) + rect.x0,
            f64::from(oy) + rect.y0,
            rect.width(),
            rect.height(),
        )
    }

    /// A logical point of the desktop in the tree's physical pixels.
    fn local(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.placement.origin;
        (
            (x - f64::from(ox)) * self.placement.scale,
            (y - f64::from(oy)) * self.placement.scale,
        )
    }

    /// Give the keyboard up: no widget of the tree holds it. What a host asks
    /// of a field it drew when the person went elsewhere (a press in another
    /// surface, a window brought forward), so the field stops taking keys.
    pub fn clear_focus(&mut self) {
        self.host.clear_focus();
        self.owe();
    }

    fn input(&mut self, event: &WindowEvent) {
        self.host.send(event);
        self.owe();
    }
}

impl crate::Published for WidgetHost {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        self.host.resources()
    }

    fn revision(&self) -> Option<u64> {
        Some(self.revision)
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut crate::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        let started = self
            .host
            .invoke_published(&address.resource, &address.action, arguments);
        self.owe();
        started
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        Some(self.host.a11y_tree_update_at(self.placement.scale))
    }

    fn a11y_name(&self) -> Option<String> {
        self.name.clone()
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        let acted = self.host.perform_a11y_action(target, action).is_ok();
        self.owe();
        acted
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(self)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.host
            .a11y_nodes()
            .into_iter()
            .flat_map(|(_, node)| {
                // A live region is heard by what it says, which it carries
                // as its value: a confirmation has no name of its own.
                let words = if node.live().is_some() {
                    node.value().map(str::to_owned)
                } else {
                    None
                };
                node.label().map(str::to_owned).into_iter().chain(words)
            })
            .filter(|words| !words.is_empty())
            .collect()
    }

    /// The tree's controls: the widgets addressed by a target (a place's
    /// switch, a tab's pill, an onboarding step) and the elements it names
    /// in its accessibility tree, each where it stands on the desktop.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        let mut shown: std::collections::BTreeMap<String, (f64, f64, f64, f64)> =
            std::collections::BTreeMap::new();
        for (name, rect) in self.host.targets() {
            shown.insert(name, self.logical(rect));
        }
        // The controls of an app the tree shows as its own (setup's steps),
        // by the ids the app names them with: the tree's targets stop at it.
        let mut embedded = Vec::new();
        each_embedded_app(self.host.root_widget_mut(), &mut |app| {
            embedded.extend(app.named_controls());
        });
        for (name, rect) in embedded {
            let logical = self.logical(rect);
            shown.entry(name).or_insert(logical);
        }
        for (_, node) in self.host.a11y_nodes() {
            // A hidden node stands nowhere the person can reach.
            if node.is_hidden() {
                continue;
            }
            let (Some(name), Some(bounds)) = (node.label(), node.bounds()) else {
                continue;
            };
            shown.entry(name.to_owned()).or_insert_with(|| {
                self.logical(vello::kurbo::Rect::new(
                    bounds.x0, bounds.y0, bounds.x1, bounds.y1,
                ))
            });
        }
        shown.into_iter().collect()
    }

    /// The name `controls` showed, pressed: a widget addressed by its own
    /// name or test id, else an element of the accessibility tree — a chip, a
    /// window bar's pill — activated as assistive technology activates it.
    fn press(&mut self, name: &str) -> Result<(), String> {
        if let Err(named) = self.host.perform_named(name, &ui::a11y::Action::Activate) {
            // A control of an app the tree shows as its own, by the id it
            // names it with, else an element of the accessibility tree.
            let mut pressed = false;
            each_embedded_app(self.host.root_widget_mut(), &mut |app| {
                pressed = pressed || app.press_named(name);
            });
            if !pressed {
                let node = self
                    .host
                    .a11y_nodes()
                    .into_iter()
                    .find(|(_, node)| node.label() == Some(name))
                    .map(|(node, _)| node)
                    .ok_or(named)?;
                self.host
                    .perform_a11y_action(node, &ui::a11y::Action::Activate)?;
            }
        }
        self.owe();
        Ok(())
    }
}

impl SurfaceHost for WidgetHost {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            location: self.placement.origin,
            size: (
                i32::try_from(self.placement.size.0).unwrap_or(i32::MAX),
                i32::try_from(self.placement.size.1).unwrap_or(i32::MAX),
            ),
            frame: self.frames.frame(),
        }]
    }

    fn withholds_captures(&self) -> bool {
        self.host.capture_protected()
    }

    fn below_windows(&self) -> bool {
        self.below
    }

    fn window_pictures(&self) -> Vec<WindowPicture> {
        self.pictures
            .as_ref()
            .map_or_else(Vec::new, |pictures| pictures())
    }

    fn backdrops(&self) -> Vec<tilo_session_core::surface::BackdropRequest> {
        let (ox, oy) = self.placement.origin;
        let scale = self.placement.scale;
        self.backdrops
            .iter()
            .map(
                |(bounds, blur_px, texels)| tilo_session_core::surface::BackdropRequest {
                    rect: (
                        f64::from(ox) + bounds.x0 / scale,
                        f64::from(oy) + bounds.y0 / scale,
                        bounds.width() / scale,
                        bounds.height() / scale,
                    ),
                    texels: *texels,
                    blur: blur_px * f64::from(texels.0) / bounds.width().max(1.0),
                },
            )
            .collect()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_session_core::surface::BackdropImage>>) {
        self.provided.resize(images.len(), None);
        self.provided.truncate(images.len());
        for (slot, image) in images.into_iter().enumerate() {
            if let (Some(image), Some(provided)) = (image, self.provided.get_mut(slot)) {
                *provided = Some(ui::raster::rgba8_premultiplied(
                    image.rgba,
                    image.texels.0,
                    image.texels.1,
                ));
                self.owed = Owed::Material;
            }
        }
        self.owe();
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (lx, ly) = self.local(x, y);
        if lx < 0.0 || ly < 0.0 {
            return false;
        }
        if self.below {
            // The desktop under the windows takes the pointer wherever the
            // compositor offers it: there is nothing beneath it.
            let (width, height) = self.frames.size();
            return lx < f64::from(width) && ly < f64::from(height);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative point inside a surface on a screen"
        )]
        let (px, py) = (lx as u32, ly as u32);
        self.frames.alpha(px, py).is_some_and(|alpha| alpha > 0)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        let (lx, ly) = self.local(x, y);
        self.input(&WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(lx, ly),
        });
    }

    fn pointer_leave(&mut self) {
        self.input(&WindowEvent::CursorLeft {
            device_id: DeviceId::dummy(),
        });
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input(&mouse_input(button, pressed));
    }

    fn press_elsewhere_closes(&self) -> Option<Rc<dyn Fn()>> {
        self.dismissed.clone()
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        let scale = self.placement.scale;
        self.input(&WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                -horizontal * scale,
                -vertical * scale,
            )),
            phase: TouchPhase::Moved,
        });
    }

    fn outputs_changed(&mut self, outputs: &[tilo_session_core::surface::HostedOutput]) {
        if !self.covers {
            return;
        }
        let Some((x0, y0, x1, y1)) = outputs
            .iter()
            .map(|output| {
                let (x, y, w, h) = output.rect;
                (x, y, x.saturating_add(w), y.saturating_add(h))
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (
            u32::try_from(x1.saturating_sub(x0)),
            u32::try_from(y1.saturating_sub(y0)),
        ) else {
            return;
        };
        // The scale of the output the surface stands on, so what it paints
        // has the pixels that output shows rather than being stretched over
        // them: a bar on a 2 display is drawn at 2.
        let scale = tilo_session_core::surface::HostedOutput::scale_at(
            outputs,
            (
                f64::from(x0) + f64::from(width) / 2.0,
                f64::from(y0) + f64::from(height) / 2.0,
            ),
        );
        self.set_placement(Placement {
            origin: (x0, y0),
            size: (width.max(1), height.max(1)),
            scale,
        });
    }

    fn wants_keyboard(&self) -> bool {
        // A palette the tree presents is over everything it draws, whatever
        // the tree's own state says: the palette takes the keys a person
        // types, not the window the tree stands under. Without this the
        // session's keys went to the window behind `tilo.host:search`'s
        // palette, and nothing reached its field.
        self.host.palette_shown() || self.keyboard.as_ref().is_some_and(|wants| wants())
    }

    fn key(&mut self, key: &HostKey) {
        let modifiers = key.modifiers.state();
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            self.input(&WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
        }
        self.input(&key_event(key));
    }

    /// The field the toolkit keeps the focus in, if it takes typing: an input
    /// method composes into it as into a client's field, its candidate window
    /// under the caret.
    fn text_input(&self) -> Option<HostTextInput> {
        let field = self.host.ime_window()?;
        let (hint, purpose) = field.content.wayland();
        let (x, y, width, height) = self.logical(vello::kurbo::Rect::new(
            f64::from(field.x),
            f64::from(field.y),
            f64::from(field.x.saturating_add(field.width)),
            f64::from(field.y.saturating_add(field.height)),
        ));
        Some(HostTextInput {
            hint,
            purpose,
            x,
            y,
            width,
            height,
        })
    }

    fn ime(&mut self, event: HostIme) {
        self.input(&WindowEvent::Ime(ime_event(event)));
    }

    /// A frame is due now for a change it owes the compositor; what only
    /// moves is due on the display's grid, one frame per refresh; else the
    /// earliest deadline the tree sets.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        let schedule = self.host.frame_schedule();
        if self.owed != Owed::Nothing {
            return Some(Instant::now());
        }
        if schedule.needs_animation_frame {
            return Some(self.cadence.due(Instant::now()));
        }
        schedule.next_wake_deadline
    }

    fn set_frame_interval(&mut self, interval: Duration) {
        self.cadence.set_interval(interval);
    }

    fn prepare_frame(&mut self) -> bool {
        if self.asked.replace(false) {
            // What asked for this pass is outside the tree and needs the tree
            // to have taken it: a widget that reads something the host holds
            // (the overlays the chrome draws, a model an action wrote) hears of
            // it in its update, which a redraw of what it last showed never
            // runs. The frame below then draws what the pass settled.
            self.with_reports(ui::headless::FrameHarness::relayout);
            self.owe();
        }
        let now = Instant::now();
        // What only moves draws on the display's grid: a turn that comes
        // before the next display frame has nothing new to show, and drawing
        // it would put more frames on the screen than it runs at.
        if self.owed == Owed::Nothing
            && self.cadence.early(now)
            && self.host.frame_schedule().needs_animation_frame
        {
            return false;
        }
        // The tree lays out with the wells in effect, so a placement made
        // now reports to them, and a policy reading them in the same frame
        // sees the windows where the tree just laid them out.
        // What was owed is consumed by this frame whether or not the tree drew anything of
        // its own.
        let owed = std::mem::take(&mut self.owed);
        // A surface draws its material with the image it has when it draws:
        // new backdrops ask the tree for a frame of its own, even when
        // nothing else in it moved.
        let drew = if owed == Owed::Material {
            self.with_reports(ui::headless::FrameHarness::relayout);
            true
        } else {
            self.with_reports(|host| host.poll_frame().unwrap_or(false))
        };
        if !(drew || owed != Owed::Nothing) {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        let scale = self.placement.scale;
        self.scene.reset();
        self.scene
            .append(self.host.scene(), Some(Affine::scale(scale)));
        self.backdrops = self
            .host
            .backdrops()
            .regions
            .iter()
            .map(|region| {
                (
                    region.rect,
                    region.blur_px,
                    ui::backdrop::backdrop_texels(region.rect, region.sampling, region.cell_px),
                )
            })
            .collect();
        let size = self.frames.size();
        let key = TargetKey {
            width: size.0,
            height: size.1,
            to_device: Affine::scale(scale),
            base_color: TRANSPARENT,
            generation: self.generation,
        };
        let parts = self.host.scene_parts();
        let damage = match self.retained.repaint(&self.scene, parts, key) {
            // Nothing of what it shows changed: there is nothing to draw.
            Repaint::Regions(changed) if changed.is_empty() => return false,
            Repaint::Regions(changed) => Some(changed),
            Repaint::Full(_) => None,
        };
        self.retained.presented(&self.scene, parts, key);
        self.frames.present(&self.scene, damage.as_deref());
        self.cadence.drawn(now);
        true
    }
}

/// Hand `each` every app `widget`'s tree shows as its own ([`ui::app::EmbeddedApp`]).
fn each_embedded_app(widget: &mut dyn ui::Widget, each: &mut dyn FnMut(&mut ui::app::EmbeddedApp)) {
    if let Some(app) = widget
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<ui::app::EmbeddedApp>())
    {
        each(app);
        return;
    }
    for child in widget.children_mut() {
        each_embedded_app(child, each);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::arithmetic_side_effects,
        reason = "test fixtures"
    )]

    use super::Cadence;
    use std::time::{Duration, Instant};

    const FRAME: Duration = Duration::from_micros(16_667);

    #[test]
    fn a_tree_with_no_display_under_it_draws_every_turn() {
        let cadence = Cadence::new();
        let now = Instant::now();
        assert!(!cadence.early(now), "nothing paces a tree with no display");
        assert_eq!(cadence.due(now), now, "the first frame is due at once");
    }

    #[test]
    fn one_frame_is_due_per_display_frame() {
        let mut cadence = Cadence::new();
        cadence.set_interval(FRAME);
        let drawn = Instant::now();
        cadence.drawn(drawn);
        assert!(
            cadence.early(drawn + Duration::from_millis(1)),
            "a turn before the grid shows nothing new"
        );
        assert!(
            !cadence.early(drawn + FRAME),
            "the frame is due at the grid"
        );
        assert_eq!(cadence.due(drawn), drawn + FRAME);
    }

    #[test]
    fn a_frame_drawn_late_skips_the_deadlines_it_missed() {
        let mut cadence = Cadence::new();
        cadence.set_interval(FRAME);
        let drawn = Instant::now();
        cadence.drawn(drawn);
        // Three display frames later: the grid moves on from now, it does not
        // queue the two frames that went by.
        let late = drawn + FRAME * 3;
        cadence.drawn(late);
        assert_eq!(cadence.due(late), late + FRAME);
    }

    #[test]
    fn past_the_grid_a_frame_is_due_now() {
        let mut cadence = Cadence::new();
        cadence.set_interval(FRAME);
        let drawn = Instant::now();
        cadence.drawn(drawn);
        let idle = drawn + Duration::from_secs(5);
        assert!(!cadence.early(idle));
        assert_eq!(cadence.due(idle), idle);
    }
}
