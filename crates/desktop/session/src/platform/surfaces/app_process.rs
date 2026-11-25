//! A toolkit app shown on a layer of the compositor, in the session's process.
//!
//! The top bar, the launcher and the panels are ordinary toolkit apps. An
//! [`AppProcess`] shows one of them on a rectangle of the desktop: it drives the
//! app as any surface does (input in, frames out), has each frame drawn where it
//! changed the way the app's own frames are, and hands the compositor the
//! buffer as a [`SurfaceLayer`]. The compositor knows nothing of the app;
//! the app knows nothing of the compositor.

use std::cell::Cell;

use tilo_session_core::surface::{
    BackdropImage, BackdropRequest, CursorIcon, Scroll, ShellSurface, SurfaceDrag, SurfaceIme,
    SurfaceKey, SurfaceLayer, SurfaceTextInput,
};

use ui::App;
use ui::app::{AppSurface, SurfaceDamage};
use ui::device_local_api::SharedApi;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{
    DeviceId, ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, RawKeyEvent,
    TouchPhase, WindowEvent,
};
use winit::keyboard::{ModifiersState, PhysicalKey, SmolStr};
use winit::platform::scancode::PhysicalKeyExtScancode as _;

use super::frames::Frames;

// The API a hosted surface publishes through: every host's, so a surface app is reached the
// same on each.
use tilo_session_core::surfaces::{Published, through_api, through_api_as};

/// The format files are dragged in.
const URI_LIST: &str = "text/uri-list";

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

use super::Placement;

/// The surface the app is drawn on, as the toolkit asks it.
struct Surface {
    size: PhysicalSize<u32>,
    scale: f64,
    redraw: Cell<bool>,
    /// The pointer shape the app asked for last.
    cursor: Cell<CursorIcon>,
}

impl AppSurface for Surface {
    fn scale_factor(&self) -> f64 {
        self.scale
    }

    fn inner_size(&self) -> PhysicalSize<u32> {
        self.size
    }

    fn request_redraw(&self) {
        self.redraw.set(true);
    }

    fn set_cursor(&self, cursor: winit::window::CursorIcon) {
        self.cursor.set(cursor);
    }
}

/// The files a dropped `text/uri-list` names: its `file:` URIs as paths,
/// comments and other schemes skipped.
fn dropped_paths(data: &[u8]) -> Vec<std::path::PathBuf> {
    String::from_utf8_lossy(data)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| url::Url::parse(line).ok()?.to_file_path().ok())
        .collect()
}

/// One toolkit app shown on a rectangle of the desktop.
pub struct AppProcess {
    app: App,
    placement: Placement,
    surface: Surface,
    frames: Frames,
    scene: vello::Scene,
    damage: SurfaceDamage,
    /// The app asked for another frame when it drew the last.
    wants: bool,
    /// The modifiers the app last heard of.
    modifiers: ModifiersState,
    /// The app's actions for this device: what it publishes, and what the
    /// session's catalogue asked of it, run on the next frame.
    api: SharedApi,
    /// The regions the app drew translucent material in last frame, in
    /// its physical pixels: bounds, blur deviation, texel size.
    backdrops: Vec<(vello::kurbo::Rect, f64, (u32, u32))>,
    /// The blurred backdrops the compositor made, by region.
    provided: Vec<Option<vello::peniko::ImageData>>,
    /// The next frame is rasterized whole (its backdrops changed).
    full: bool,
    /// Where what the app publishes stands: moves with every frame it
    /// draws and every action it runs.
    revision: u64,
    /// What an assistive technology knows the app's window by.
    name: Option<String>,
}

impl AppProcess {
    /// Show `app` at `placement`. Nothing is drawn before the first
    /// [`ShellSurface::prepare_frame`].
    pub fn new(app: App, placement: Placement) -> Self {
        // Drags the app starts come to the compositor through this host.
        tilo_ui_base::dnd::host_drags_in_process();
        let (width, height) = placement.physical_size();
        Self {
            app,
            placement,
            surface: Surface {
                size: PhysicalSize::new(width, height),
                scale: placement.scale,
                redraw: Cell::new(true),
                cursor: Cell::new(CursorIcon::Default),
            },
            frames: Frames::new((width, height)),
            scene: vello::Scene::new(),
            damage: SurfaceDamage::default(),
            wants: true,
            modifiers: ModifiersState::empty(),
            api: SharedApi::default(),
            backdrops: Vec::new(),
            provided: Vec::new(),
            full: false,
            revision: 0,
            name: None,
        }
    }

    /// Name the app's window for assistive technology: what a screen reader
    /// says the person reached.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Run what was asked through the API now, then publish what the app
    /// offers: the session serves an invocation without waiting for a
    /// frame. Whether anything ran.
    fn serve_api(&mut self) -> bool {
        let app = &mut self.app;
        let ran = self
            .api
            .serve_with(|actor, address, arguments| match actor {
                Some(actor) => app.invoke_local_resource_action_as(actor, address, arguments),
                None => app.invoke_local_resource_action(address, arguments),
            });
        if ran {
            self.wants = true;
        }
        self.api
            .publish_resources(Some(self.app.collect_resources()));
        ran
    }

    /// The app, to hand it events of its own (external events, settings).
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// Repaint this surface when the person's theme changes, even while its app is idle.
    pub fn set_theme(&mut self, theme: tilo_ui_base::Theme) {
        self.app.set_theme(theme);
        self.wants = true;
        self.full = true;
    }

    /// Where the app is shown.
    pub const fn placement(&self) -> Placement {
        self.placement
    }

    /// Show the app on another rectangle or at another scale.
    pub fn set_placement(&mut self, placement: Placement) {
        if placement == self.placement {
            return;
        }
        let size = placement.physical_size();
        if size != self.placement.physical_size()
            || placement.scale.to_bits() != self.placement.scale.to_bits()
        {
            self.surface.size = PhysicalSize::new(size.0, size.1);
            self.surface.scale = placement.scale;
            self.frames.resize(size);
            self.app.surface_input(
                &self.surface,
                WindowEvent::Resized(self.surface.size),
                false,
            );
            self.surface.redraw.set(true);
            // What the app draws now is not a change of what it drew before.
            self.full = true;
        }
        self.placement = placement;
    }

    /// A logical point of the desktop in the app's physical pixels.
    fn local(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.placement.origin;
        (
            (x - f64::from(ox)) * self.placement.scale,
            (y - f64::from(oy)) * self.placement.scale,
        )
    }

    fn input(&mut self, event: WindowEvent) {
        self.app.surface_input(&self.surface, event, false);
        self.wants = true;
    }

    /// Perform `action` on the app's node `target`. A node the app does not
    /// handle itself is pressed where it is drawn, as the app's own window
    /// does for its assistive technology.
    fn act(&mut self, target: ui::accesskit::NodeId, action: &tilo_ui_base::a11y::Action) {
        if let Some((x, y)) = self.app.dispatch_a11y_action(target, action) {
            self.input(tilo_ui_base::synthetic_input::cursor_moved(x, y));
            self.input(tilo_ui_base::synthetic_input::mouse_press());
            self.input(tilo_ui_base::synthetic_input::mouse_release());
        }
        self.wants = true;
    }
}

impl Published for AppProcess {
    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        self.app.collect_resources()
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, ui::tilo_actions::ApiError>
    {
        // Through the app's own API, as every caller on this device.
        let api = self.api.clone();
        self.revision = self.revision.wrapping_add(1);
        through_api(&api, address, arguments, || {
            self.serve_api();
        })
    }

    /// The same as [`Self::invoke`], as `actor` (an agent or a script): the app's own API admits
    /// the call for whoever asked, so an effect that acts on this device's person alone is
    /// refused to anyone else.
    fn invoke_as(
        &mut self,
        actor: &ui::tilo_actions::Actor,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, ui::tilo_actions::ApiError>
    {
        let api = self.api.clone();
        self.revision = self.revision.wrapping_add(1);
        through_api_as(&api, Some(actor), address, arguments, || {
            self.serve_api();
        })
    }

    fn revision(&self) -> Option<u64> {
        Some(self.revision)
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        Some(self.app.build_a11y_tree_update(self.placement.scale))
    }

    fn a11y_name(&self) -> Option<String> {
        self.name.clone()
    }

    fn a11y_action(
        &mut self,
        target: ui::accesskit::NodeId,
        action: &tilo_ui_base::a11y::Action,
    ) -> bool {
        self.act(target, action);
        true
    }

    fn wants_keyboard(&self) -> bool {
        ShellSurface::wants_keyboard(self)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.app.refresh_semantic_tree();
        self.app
            .semantic_tree()
            .iter()
            .filter_map(|node| node.name.clone())
            .collect()
    }

    /// The controls the app's window shows by name and by what it does, each
    /// where it stands on the desktop: what its accessibility tree exports,
    /// read with the same rule the app's own window hands the desktop
    /// ([`ui::App::window_controls`]), moved from the window's content to
    /// where the window is placed.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        let (ox, oy) = self.placement.origin;
        let mut shown: std::collections::BTreeMap<String, ui::window_targets::WindowControl> =
            std::collections::BTreeMap::new();
        for control in self.app.window_controls() {
            // Two controls of one name: the first in reading order is the one
            // the name means, as the window itself finds it.
            shown.entry(control.name.clone()).or_insert_with(|| {
                ui::window_targets::WindowControl {
                    rect: (
                        f64::from(ox) + control.rect.0,
                        f64::from(oy) + control.rect.1,
                        control.rect.2,
                        control.rect.3,
                    ),
                    ..control
                }
            });
        }
        shown.into_values().collect()
    }

    /// Press the control `name` the app's window shows: the activation an
    /// assistive technology performs on it.
    fn press(&mut self, name: &str) -> Result<(), String> {
        self.press_action(name, ui::window_targets::TargetAction::Activate)
    }

    /// Act on the control that does `identity`, through the same path a screen
    /// reader's activation takes in the app's own window: the control is found
    /// by what it does, never by what the person's language calls it.
    fn press_identity(
        &mut self,
        identity: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        // The same route [`Self::press_action`] takes: the app finds the control by what it does
        // and acts on it, as assistive technology does.
        if self.app.press_window_control(identity, action) {
            self.wants = true;
            return Ok(());
        }
        Err(format!("no control does {identity} answers {action:?}"))
    }

    /// Act on the control `name` the app's window shows, through the same
    /// path a screen reader's activation takes in the app's own window.
    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        let verb = match action {
            ui::window_targets::TargetAction::Activate => tilo_ui_base::a11y::Action::Activate,
            ui::window_targets::TargetAction::ShowContextMenu => {
                tilo_ui_base::a11y::Action::ShowContextMenu
            }
        };
        let node = self
            .app
            .build_a11y_tree_update(self.placement.scale)
            .nodes
            .into_iter()
            .find(|(_, described)| {
                !described.is_hidden()
                    && described.label().map(str::trim) == Some(name)
                    && described.supports_action(action.accesskit())
            })
            .map(|(node, _)| node)
            .ok_or_else(|| format!("no control named {name} answers {action:?}"))?;
        self.act(node, &verb);
        Ok(())
    }
}

impl ShellSurface for AppProcess {
    fn layers(&self) -> Vec<SurfaceLayer> {
        let size = (
            i32::try_from(self.placement.size.0).unwrap_or(i32::MAX),
            i32::try_from(self.placement.size.1).unwrap_or(i32::MAX),
        );
        vec![SurfaceLayer {
            location: self.placement.origin,
            size,
            frame: self.frames.frame(),
            pose: tilo_session_core::surface::LayerPose::SETTLED,
        }]
    }

    fn withholds_captures(&self) -> bool {
        self.app.withholds_captures()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (lx, ly) = self.local(x, y);
        if lx < 0.0 || ly < 0.0 {
            return false;
        }
        // Where the app drew nothing, the pointer reaches what lies beneath.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative point inside a surface on a screen"
        )]
        let (px, py) = (lx as u32, ly as u32);
        self.frames.covers(px, py)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        let (lx, ly) = self.local(x, y);
        self.input(WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(lx, ly),
        });
    }

    fn pointer_leave(&mut self) {
        self.input(WindowEvent::CursorLeft {
            device_id: DeviceId::dummy(),
        });
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input(mouse_input(button, pressed));
    }

    fn pointer_modifiers(&mut self, held: tilo_session_core::surface::KeyModifiers) {
        let modifiers = held.state();
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            self.input(WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
        }
    }

    fn pointer_axis(&mut self, scroll: Scroll) {
        self.input(mouse_wheel(scroll, self.placement.scale));
    }

    fn key(&mut self, key: &SurfaceKey) {
        let modifiers = key.modifiers.state();
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            self.input(WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
        }
        self.input(key_event(key));
    }

    /// The field the app keeps the focus in, if it takes typing: an input
    /// method composes into it as into a client's field, its candidate window
    /// under the caret.
    fn text_input(&self) -> Option<SurfaceTextInput> {
        let field = self.app.ime_window_state()?;
        let (hint, purpose) = field.content.wayland();
        let (ox, oy) = self.placement.origin;
        let scale = self.placement.scale;
        let edge = |v: u32, at: i32| f64::from(at) + f64::from(v) / scale;
        Some(SurfaceTextInput {
            hint,
            purpose,
            x: edge(field.x, ox),
            y: edge(field.y, oy),
            width: f64::from(field.width) / scale,
            height: f64::from(field.height) / scale,
        })
    }

    fn ime(&mut self, event: SurfaceIme) {
        self.input(WindowEvent::Ime(ime_event(event)));
    }

    fn cursor(&self) -> CursorIcon {
        self.surface.cursor.get()
    }

    /// The toolkit takes files dropped on it (as a window of the desktop
    /// it runs in would), so a drag that offers files is taken.
    fn drop_offered(&mut self, mimes: &[String], x: f64, y: f64) -> Option<String> {
        if !mimes.iter().any(|mime| mime == URI_LIST) {
            return None;
        }
        // Drop zones learn where the drag is from the pointer.
        self.pointer_motion(x, y);
        Some(URI_LIST.to_owned())
    }

    fn drop_left(&mut self) {
        self.input(WindowEvent::HoveredFileCancelled);
        self.pointer_leave();
    }

    fn drop_data(&mut self, mime: &str, data: Vec<u8>) {
        if mime != URI_LIST {
            return;
        }
        for path in dropped_paths(&data) {
            self.input(WindowEvent::HoveredFile(path.clone()));
            self.input(WindowEvent::DroppedFile(path));
        }
    }

    fn backdrops(&self) -> Vec<BackdropRequest> {
        let (ox, oy) = self.placement.origin;
        let scale = self.placement.scale;
        self.backdrops
            .iter()
            .map(|(bounds, blur_px, texels)| BackdropRequest {
                rect: (
                    f64::from(ox) + bounds.x0 / scale,
                    f64::from(oy) + bounds.y0 / scale,
                    bounds.width() / scale,
                    bounds.height() / scale,
                ),
                texels: *texels,
                blur: blur_px * f64::from(texels.0) / bounds.width().max(1.0),
            })
            .collect()
    }

    fn set_backdrops(&mut self, images: Vec<Option<BackdropImage>>) {
        self.provided.resize(images.len(), None);
        self.provided.truncate(images.len());
        for (slot, image) in images.into_iter().enumerate() {
            if let (Some(image), Some(provided)) = (image, self.provided.get_mut(slot)) {
                *provided = Some(ui::raster::rgba8_premultiplied(
                    image.rgba,
                    image.texels.0,
                    image.texels.1,
                ));
            }
        }
        self.wants = true;
        self.full = true;
    }

    fn take_drag(&mut self) -> Option<SurfaceDrag> {
        tilo_ui_base::dnd::take_hosted_drag().map(|offer| SurfaceDrag { offer, icon: None })
    }

    /// The pointer may have moved anywhere while it carried the drag; the
    /// compositor routes it anew, so the app hears it left.
    fn drag_ended(&mut self, _dropped: bool) {
        self.pointer_leave();
    }

    /// A wake the app took has not drawn yet, or it says a frame is due now
    /// while something moves; else the earliest deadline its content sets.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        if self.wants || self.app.wake_pending() {
            return Some(std::time::Instant::now());
        }
        self.app.surface_next_frame(ui::time::now())
    }

    fn prepare_frame(&mut self) -> bool {
        if self.api.has_queued() {
            self.serve_api();
        }
        let now = ui::time::now();
        let due = self.app.surface_next_frame(now).is_some_and(|at| at <= now);
        if !(self.wants || self.surface.redraw.replace(false) || self.app.wake_pending() || due) {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        self.app.surface_accessibility();
        self.scene.reset();
        // Translucent material records what lies below it; the compositor
        // blurs that and hands it back (`set_backdrops`).
        // The images are this app's: every host on the thread provides its
        // own before it draws.
        tilo_ui_base::part_paint::backdrop::provide_backdrops(
            self.provided
                .iter()
                .enumerate()
                .filter_map(|(slot, image)| Some((slot, image.clone()?)))
                .collect(),
        );
        let recording = tilo_ui_base::part_paint::backdrop::BackdropRecording::begin();
        tilo_ui_base::part_paint::backdrop::layer_scene(&self.scene, 0);
        self.wants =
            self.app
                .surface_frame_with_damage(&self.surface, &mut self.scene, &mut self.damage);
        self.backdrops = recording
            .finish()
            .iter()
            .map(|surface| (surface.bounds, surface.blur_px, surface.texels()))
            .collect();
        let changed = self.damage.take();
        let changed = if std::mem::take(&mut self.full) {
            None
        } else {
            changed
        };
        self.frames.present(&self.scene, changed.as_deref());
        self.api
            .publish_resources(Some(self.app.collect_resources()));
        true
    }
}

/// A scroll as winit reports it at `scale`: a wheel's notches are lines, as winit reports a
/// Wayland client's `axis_value120`, fingers' pixels are physical, and their lifting ends the
/// scroll, which a scroller glides on from. The axis counts down as positive and the toolkit
/// moves content against winit's delta, so the signs turn.
pub(crate) fn mouse_wheel(scroll: Scroll, scale: f64) -> WindowEvent {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "winit's lines are f32; a scroll's notches are few"
    )]
    let (delta, phase) = match scroll {
        Scroll::Notches(x, y) => (
            MouseScrollDelta::LineDelta(-x as f32, -y as f32),
            TouchPhase::Moved,
        ),
        Scroll::Pixels(x, y) => (
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(-x * scale, -y * scale)),
            TouchPhase::Moved,
        ),
        Scroll::Lifted => (
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.0)),
            TouchPhase::Ended,
        ),
    };
    WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta,
        phase,
    }
}

/// The toolkit's event for a pointer button (evdev code) changing state.
pub(crate) fn mouse_input(button: u32, pressed: bool) -> WindowEvent {
    let button = match button {
        BTN_LEFT => MouseButton::Left,
        BTN_RIGHT => MouseButton::Right,
        BTN_MIDDLE => MouseButton::Middle,
        BTN_SIDE => MouseButton::Back,
        BTN_EXTRA => MouseButton::Forward,
        other => MouseButton::Other(u16::try_from(other).unwrap_or(u16::MAX)),
    };
    let state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button,
    }
}

/// The toolkit's key event for a key the compositor's keymap resolved.
pub(crate) fn key_event(key: &SurfaceKey) -> WindowEvent {
    let text = key
        .pressed
        .then_some(key.text.as_deref())
        .flatten()
        .map(SmolStr::new);
    let under_chord = key.modifiers.ctrl || key.modifiers.alt || key.modifiers.logo;
    let (logical_key, location) = tilo_ui_base::keys::chord(
        key.keysym,
        key.text.as_deref(),
        key.latin_keysym,
        under_chord,
    );
    let (key_without_modifiers, _) = tilo_ui_base::keys::from_keysym(key.unmodified_keysym, None);
    let state = if key.pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    let event = KeyEvent::synthetic(
        RawKeyEvent {
            physical_key: PhysicalKey::from_scancode(key.keycode),
            state,
        },
        logical_key,
        text.clone(),
        location,
        key.repeat,
        text,
        key_without_modifiers,
    );
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

/// The toolkit's IME event for what the input method serving a hosted field
/// sent.
pub(crate) fn ime_event(event: SurfaceIme) -> Ime {
    match event {
        SurfaceIme::Commit(text) => Ime::Commit(text),
        SurfaceIme::Preedit(text, cursor) => Ime::Preedit(text, cursor),
        SurfaceIme::Enabled => Ime::Enabled,
        SurfaceIme::Disabled => Ime::Disabled,
    }
}

#[cfg(test)]
mod tests {
    use super::dropped_paths;

    #[test]
    fn a_uri_list_drops_its_files_decoded_and_skips_the_rest() {
        let list = b"# from a file manager\r\nfile:///home/ada/My%20Notes.txt\r\nhttps://tilo.so/\r\n\r\nfile://localhost/tmp/a\n";
        assert_eq!(
            dropped_paths(list),
            vec![
                std::path::PathBuf::from("/home/ada/My Notes.txt"),
                std::path::PathBuf::from("/tmp/a"),
            ]
        );
    }
}
