//! The sheets the session shows over the desktop (the power menu, the questions it asks for an app
//! or a service, workspace settings), each an app of [`tilo_sheets`] on a layer of its own covering
//! a display, up while its seat's presentation says so. The seat is how the session and the
//! services behind it talk to the sheet: what it shows in, what the person chose out. Its actions
//! are published through its door while it is up. Which display each sheet covers is placed here
//! too, from the one the person is working on.

pub(crate) mod mount;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_sheets::Seat;
use tilo_surface_host::Published;
use tilo_surface_host::{AppHost, Placement};
use ui::application::{ApplicationDefinition, ComponentRunnerInputs, prepare_private_surface};
use ui::host_operation::ResourceOperationStart;
use ui::vello::kurbo::Rect;

use crate::Session;

/// A display's rectangle, in logical pixels: the display a sheet covers.
pub(crate) type Place = (i32, i32, u32, u32);

/// What a sheet is to the session besides a card that is up or down: a sheet is one of these, never
/// two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Up while its presentation says so, and serving then.
    Presented,
    /// A command surface: it answers its actions and draws while it is down too, so its key works
    /// with nothing of it shown.
    ServingWhileDown,
    /// The sheet asks the person something about what they are typing into and leaves the keyboard
    /// where it was: it takes Enter and Escape, and every other key reaches the field under it.
    AskingAboutTyping,
    /// The lock view: drawn and serving over the session's lock, as the lock screen's own process
    /// would.
    OverLock,
}

/// One sheet's app on its layer.
pub(crate) struct Sheet<S: Seat> {
    host: AppHost,
    seat: S,
    /// The displays as the compositor last said they are.
    outputs: Vec<tilo_compositor::HostedOutput>,
    role: Role,
}

impl<S: Seat> Sheet<S> {
    /// Mount `definition` lent `seat` , as `person` on `device` .
    pub(crate) fn mount(
        definition: &'static ApplicationDefinition,
        seat: S,
        (device, person): (&str, &str),
        presentation: ComponentRunnerInputs,
        scale: f64,
    ) -> Result<Self, ui::launch::LaunchError> {
        // The card's arrival and leaving move on the person's motion setting: the shell is where it
        // is known, before any frame of the sheet reads its theme.
        seat.presentation().set_motion(presentation.theme.motion());
        let lent = seat.clone();
        let component =
            prepare_private_surface(definition, device, person, "desktop", 0, |context| {
                context.lend(lent);
            })?;
        let app = ui::App::from_component(component, presentation)?;
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale,
        };
        Ok(Self {
            host: AppHost::new(app, placement).named(definition.title()),
            seat,
            outputs: Vec::new(),
            role: Role::Presented,
        })
    }

    /// The app keeps running while the sheet is down, and its actions stay offered: what it takes
    /// is the whole point of the key that asks it.
    pub(crate) const fn always_serving(mut self) -> Self {
        self.role = Role::ServingWhileDown;
        self
    }

    /// Ask about what the person is typing into without taking their typing from them: the sheet
    /// answers Enter and Escape, the field keeps every other key. A prompt that asks whether to
    /// listen has to leave the person's typing where it was.
    pub(crate) const fn typing_through(mut self) -> Self {
        self.role = Role::AskingAboutTyping;
        self
    }

    /// The sheet stands over the session's lock: while the session is locked it is the one thing
    /// drawn and the one that takes the keyboard and the pointer. The lock view is.
    pub(crate) const fn standing_over_lock(mut self) -> Self {
        self.role = Role::OverLock;
        self
    }

    pub(crate) fn seat(&self) -> &S {
        &self.seat
    }

    fn open(&self) -> bool {
        self.seat.presentation().is_open()
    }

    fn drawn(&self) -> bool {
        self.seat.presentation().is_drawn()
    }

    /// The sheet covers the display the session placed it on, or the first one while it named none;
    /// a sheet that stands on every display at once covers all of them and is told where each one
    /// is.
    fn follow_place(&mut self) {
        let presentation = self.seat.presentation();
        let every = presentation.covers_every_display();
        let named = presentation.place();
        let Some((x, y, w, h)) = covering(named, &self.outputs, every) else {
            return;
        };
        let scale = self.host.placement().scale;
        self.host.set_placement(Placement {
            origin: (x, y),
            size: (
                u32::try_from(w.max(1)).unwrap_or(1),
                u32::try_from(h.max(1)).unwrap_or(1),
            ),
            scale,
        });
        if every {
            // Its own top-left is the corner of the rectangle all of them make: a card is drawn by
            // the app on each display there.
            let displays = self
                .outputs
                .iter()
                .map(|output| output.rect)
                .filter(|(_, _, w, h)| *w > 0 && *h > 0)
                .map(|(dx, dy, dw, dh)| {
                    let (dx, dy) = (dx.saturating_sub(x), dy.saturating_sub(y));
                    Rect::new(
                        f64::from(dx),
                        f64::from(dy),
                        f64::from(dx.saturating_add(dw)),
                        f64::from(dy.saturating_add(dh)),
                    )
                })
                .collect();
            self.seat.covers(displays);
        }
    }
}

/// The display among `outputs` a sheet covers, its rectangle as the compositor reports it: the one
/// the session `named` while it is still there, else the first in reading order, left to right and
/// top to bottom: a display unplugged from under a sheet takes it to the one the person's eye
/// starts at rather than leaving it on nothing. A sheet that stands on `every` display at once
/// covers the rectangle all of them make.
fn covering(
    named: Option<Place>,
    outputs: &[tilo_compositor::HostedOutput],
    every: bool,
) -> Option<(i32, i32, i32, i32)> {
    let shown = || {
        outputs
            .iter()
            .map(|output| output.rect)
            .filter(|(_, _, w, h)| *w > 0 && *h > 0)
    };
    if every {
        let mut corners =
            shown().map(|(x, y, w, h)| (x, y, x.saturating_add(w), y.saturating_add(h)));
        let (mut left, mut top, mut right, mut bottom) = corners.next()?;
        for (x, y, x1, y1) in corners {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x1);
            bottom = bottom.max(y1);
        }
        return Some((
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        ));
    }
    let named = named.and_then(|(x, y, w, h)| {
        let (w, h) = (i32::try_from(w).ok()?, i32::try_from(h).ok()?);
        Some((x, y, w, h))
    });
    named
        .filter(|place| outputs.iter().any(|output| output.rect == *place))
        .or_else(|| shown().min_by_key(|(x, y, _, _)| (*x, *y)))
}

impl<S: Seat> Published for Sheet<S> {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.host.resources()
    }

    fn revision(&self) -> Option<u64> {
        Published::revision(&self.host)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.host.invoke(address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        // A sheet that is down shows nothing to read, even one whose actions stay offered while it
        // is.
        if !self.drawn() {
            return Vec::new();
        }
        self.host.accessible_labels()
    }

    fn offered(&self) -> bool {
        self.role == Role::ServingWhileDown || self.open()
    }

    fn a11y_tree(&mut self) -> Option<ui::accesskit::TreeUpdate> {
        // A sheet that is down shows nothing to read, even one whose actions stay offered while it
        // is.
        if !self.drawn() {
            return None;
        }
        Published::a11y_tree(&mut self.host)
    }

    fn a11y_name(&self) -> Option<String> {
        Published::a11y_name(&self.host)
    }

    fn a11y_action(&mut self, target: ui::accesskit::NodeId, action: &ui::a11y::Action) -> bool {
        self.drawn() && Published::a11y_action(&mut self.host, target, action)
    }

    /// The controls the sheet's card shows by name, where they stand on the desktop: a sheet that
    /// is down shows nothing to read or to press.
    fn controls(&mut self) -> Vec<(String, (f64, f64, f64, f64))> {
        if !self.drawn() {
            return Vec::new();
        }
        Published::controls(&mut self.host)
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        self.press_action(name, ui::window_targets::TargetAction::Activate)
    }

    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        if !self.drawn() {
            return Err(format!("no control named {name}"));
        }
        Published::press_action(&mut self.host, name, action)
    }

    fn wants_keyboard(&self) -> bool {
        SurfaceHost::wants_keyboard(self)
    }

    /// The lock view is the one sheet that shows while the session is locked, so its actions (and
    /// what assistive technology reads of it) are offered then, as the lock screen's own process
    /// offers none.
    fn serves_while_locked(&self) -> bool {
        self.role == Role::OverLock && self.drawn()
    }
}

impl<S: Seat> SurfaceHost for Sheet<S> {
    /// What the person typed into a secret field of the sheet's card: a capture draws such a sheet
    /// as the placeholder, the display keeps showing it.
    fn withholds_captures(&self) -> bool {
        self.host.withholds_captures()
    }

    fn layers(&self) -> Vec<HostLayer> {
        if !self.drawn() {
            return Vec::new();
        }
        self.host.layers()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        // A sheet is modal: a press beside its card is still the sheet's,
        // which takes it as one outside. One on its way out absorbs presses
        // too: a press made while it leaves is not the window's behind it.
        self.drawn() && {
            let placement = self.host.placement();
            let (ox, oy) = placement.origin;
            let (w, h) = placement.size;
            x >= f64::from(ox)
                && y >= f64::from(oy)
                && x < f64::from(ox) + f64::from(w)
                && y < f64::from(oy) + f64::from(h)
        }
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.host.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.host.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.host.pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.host.pointer_axis(horizontal, vertical);
    }

    fn wants_keyboard(&self) -> bool {
        // A sheet takes the keys while it is drawn, whatever window has the focus behind it: a
        // presented modal claims input by construction. One that asks about typing leaves all but
        // its own two keys to the field under it ([`Self::typing_through`]).
        self.drawn() && self.role != Role::AskingAboutTyping
    }

    /// The lock view shows over the session's lock while it is up.
    fn above_lock(&self) -> bool {
        self.role == Role::OverLock && self.drawn()
    }

    /// What shows the lock is this view while the lock screen's app cannot.
    fn drawn_over_lock(&self) -> bool {
        self.above_lock()
    }

    fn claim_key(&mut self, key: &HostKey, _locked: bool, _inhibited: bool) -> KeyClaim {
        // The card's own two keys: the primary answer on Enter, the way out on Escape. Ordinary
        // typing is the field's.
        let answers = self.role == Role::AskingAboutTyping
            && self.open()
            && matches!(
                key.unmodified_keysym,
                xkeysym::key::Return | xkeysym::key::Escape
            );
        if answers {
            KeyClaim::Take
        } else {
            KeyClaim::Pass
        }
    }

    fn key(&mut self, key: &HostKey) {
        self.host.key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.follow_place();
        // A sheet on its way out draws its card leaving until nothing of it is left; only then does
        // the presentation say so, and the sheet stands drawn taking no input no longer. A sheet
        // that draws its own leave (the power menu) presents no modal and keeps its way out.
        let presentation = self.seat.presentation();
        if presentation.phase() == tilo_sheets::Phase::Leaving
            && !self.host.app_mut().presents_modal()
        {
            presentation.gone();
        }
        // A command surface answers the actions it is offered even with nothing of it drawn.
        (self.drawn() || self.role == Role::ServingWhileDown) && self.host.prepare_frame()
    }

    /// A sheet nobody can see asks for no frames.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.drawn().then(|| self.host.next_deadline()).flatten()
    }

    fn set_frame_interval(&mut self, interval: std::time::Duration) {
        self.host.set_frame_interval(interval);
    }

    fn cursor(&self) -> tilo_compositor::CursorIcon {
        self.host.cursor()
    }

    fn backdrops(&self) -> Vec<tilo_compositor::BackdropRequest> {
        self.host.backdrops()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_compositor::BackdropImage>>) {
        self.host.set_backdrops(images);
    }

    fn outputs_changed(&mut self, outputs: &[tilo_compositor::HostedOutput]) {
        self.outputs = outputs.to_vec();
        self.follow_place();
    }
}

impl Session {
    /// Where every sheet goes: the display the person is working on, so a question appears in front
    /// of what it is about and the power menu where they are looking. `None` while the desk knows
    /// no such display (one display needs no telling apart), which leaves a sheet on the first
    /// display there is.
    fn sheet_place(&self) -> Option<Place> {
        let display = self.shared.handle.focused_display()?;
        let monitor = self
            .shared
            .handle
            .read(|tree| tree.outputs.monitor_of(display).map(str::to_owned))?;
        let (x, y, w, h) = self
            .compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .find(|output| output.monitor == monitor)
            .and_then(|output| output.rect)?;
        Some((
            x,
            y,
            u32::try_from(w.max(1)).unwrap_or(1),
            u32::try_from(h.max(1)).unwrap_or(1),
        ))
    }

    /// Tell every sheet which display it covers: the one the person is working on, whenever the
    /// desk names another.
    pub(crate) fn place_sheets(&self) {
        let place = self.sheet_place();
        for presentation in self.sheet_presentations() {
            presentation.place_at(place);
        }
    }

    /// Every mounted sheet's presentation, in the order it draws.
    fn sheet_presentations(&self) -> impl Iterator<Item = &tilo_sheets::Presentation> {
        let shared = &self.shared;
        [
            shared.power.as_ref().map(tilo_sheets::Seat::presentation),
            shared
                .workspace_settings
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            shared
                .directory
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            shared.invite.as_ref().map(tilo_sheets::Seat::presentation),
            shared.portal.as_ref().map(tilo_sheets::Seat::presentation),
            shared
                .file_picker
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            shared.shortcut_guide.presentation(),
            shared
                .display_confirm
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            self.display_sheet.presentation(),
            self.capture_studio
                .as_ref()
                .map(crate::capture_studio::CaptureStudio::presentation),
            shared.dictation.presentation(),
        ]
        .into_iter()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::{Place, covering};

    const RIGHT: Place = (1000, 0, 1000, 700);
    const LEFT_SHOWN: tilo_compositor::HostedOutput = tilo_compositor::HostedOutput {
        rect: (0, 0, 1000, 700),
        scale: 1.0,
    };
    const RIGHT_SHOWN: tilo_compositor::HostedOutput = tilo_compositor::HostedOutput {
        rect: (1000, 0, 1000, 700),
        scale: 1.0,
    };
    const LEFT: (i32, i32, i32, i32) = (0, 0, 1000, 700);
    const RIGHT_RECT: (i32, i32, i32, i32) = (1000, 0, 1000, 700);

    #[test]
    fn a_sheet_covers_the_display_it_was_given() {
        assert_eq!(
            covering(Some(RIGHT), &[LEFT_SHOWN, RIGHT_SHOWN], false),
            Some(RIGHT_RECT)
        );
    }

    #[test]
    fn a_sheet_covers_the_first_display_when_the_one_it_was_given_is_gone() {
        // The focused display was unplugged, and the compositor lists the
        // one still there second: the sheet goes to the one on the left,
        // where the person's eye starts, not to whichever came first.
        let unplugged: Place = (2000, 0, 1000, 700);
        assert_eq!(
            covering(Some(unplugged), &[RIGHT_SHOWN, LEFT_SHOWN], false),
            Some(LEFT)
        );
    }

    #[test]
    fn a_sheet_covers_the_first_display_in_reading_order_when_none_was_given() {
        assert_eq!(
            covering(None, &[RIGHT_SHOWN, LEFT_SHOWN], false),
            Some(LEFT)
        );
        assert_eq!(covering(None, &[], false), None);
    }

    #[test]
    fn a_sheet_standing_on_every_display_covers_all_of_them() {
        assert_eq!(
            covering(None, &[RIGHT_SHOWN, LEFT_SHOWN], true),
            Some((0, 0, 2000, 700))
        );
        // One display above the other, and one that shows nothing at all.
        let above = tilo_compositor::HostedOutput {
            rect: (0, -800, 1000, 800),
            scale: 2.0,
        };
        let blank = tilo_compositor::HostedOutput {
            rect: (0, 0, 0, 0),
            scale: 1.0,
        };
        assert_eq!(
            covering(None, &[LEFT_SHOWN, above, blank], true),
            Some((0, -800, 1000, 1500))
        );
        assert_eq!(covering(None, &[], true), None);
    }
}
