//! Game-controller navigation for the local desktop.
//!
//! A controller in the room is one more way to ask for things the desktop
//! already does. [`ControllerNavMapper`] turns the typed
//! [`ForwardedGamepadEvent`] stream into [`NavIntent`]s — pure, clocked by
//! the caller, so dead zones, auto-repeat and the Guide chord are testable
//! without hardware. [`ControllerNavRuntime`] owns the evdev sources and
//! feeds them through one mapper per controller.
//!
//! A controller leased to another device or app (see
//! [`crate::shell::physical_gamepad`]) is closed here for the lease's
//! lifetime: one press never acts in two places.
//!
//! Bindings, by printed legend (so `A` confirms on every family, including
//! Nintendo where it sits east):
//!
//! | Input                     | Intent                              |
//! |---------------------------|-------------------------------------|
//! | Left stick / D-pad        | move focus (auto-repeats)           |
//! | A / B                     | activate / back                     |
//! | X / Y                     | actions for the focused app / search|
//! | LB / RB                   | previous / next tab                 |
//! | LT / RT                   | previous / next workspace           |
//! | Start / Select            | controller guide / overview         |
//! | Guide + left/right        | previous / next window              |
//! | Guide + up/down           | previous / next strip               |
//! | Guide + LB / RB           | move the window left / right        |
//! | Guide alone               | quick settings                      |

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use os_actions::{
    ForwardedGamepadEvent, GamepadAxis, GamepadButton, LinuxPhysicalGamepadCatalog,
    LinuxPhysicalGamepadSource, PhysicalGamepadHotplug, PhysicalGamepadId,
};
use ui::ControllerFamily;

/// Stick deflection (of `i16::MAX`) that starts a move.
pub const STICK_PRESS: i16 = 18_000;
/// Deflection a held stick direction must fall under to end. The gap to
/// [`STICK_PRESS`] keeps a stick resting near the threshold from chattering.
pub const STICK_RELEASE: i16 = 11_000;
/// Trigger travel that counts as a press, and the level it must fall under
/// before the next press registers.
const TRIGGER_PRESS: i16 = 16_000;
const TRIGGER_RELEASE: i16 = 8_000;
/// A held direction waits this long before repeating…
pub const REPEAT_DELAY: Duration = Duration::from_millis(380);
/// …then repeats at this interval, shortening by [`REPEAT_ACCELERATION`]
/// per step down to [`REPEAT_FASTEST`], so long lists stay reachable.
pub const REPEAT_FIRST: Duration = Duration::from_millis(160);
pub const REPEAT_FASTEST: Duration = Duration::from_millis(45);
const REPEAT_ACCELERATION: f64 = 0.82;
/// Raw events read per controller per pass; the rest wait for the next one.
const READ_BUDGET: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDirection {
    Up,
    Down,
    Left,
    Right,
}

/// What a controller press means to the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavIntent {
    /// Move focus within the focused app or open shell surface.
    Move(NavDirection),
    Activate,
    Back,
    /// The focused app's typed actions.
    Actions,
    Search,
    Help,
    Overview,
    QuickSettings,
    PreviousTab,
    NextTab,
    PreviousWorkspace,
    NextWorkspace,
    FocusPreviousWindow,
    FocusNextWindow,
    PreviousStrip,
    NextStrip,
    MoveGroupLeft,
    MoveGroupRight,
}

impl NavIntent {}

/// The four face buttons by the role their printed legend carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceRole {
    /// `A` (`×` on `PlayStation`).
    Confirm,
    /// `B` (`○`).
    Cancel,
    /// `X` (`□`).
    Third,
    /// `Y` (`△`).
    Fourth,
}

/// Linux reports face buttons by position. Nintendo prints `A` east and `B`
/// south — the mirror of Xbox — so the roles follow the legend, matching
/// the glyphs [`ui::input::ControllerButton::label`] draws.
fn face_role(family: ControllerFamily, button: GamepadButton) -> Option<FaceRole> {
    use GamepadButton::{East, North, South, West};
    Some(match family {
        ControllerFamily::Nintendo | ControllerFamily::JoyConPair => match button {
            East => FaceRole::Confirm,
            South => FaceRole::Cancel,
            North => FaceRole::Third,
            West => FaceRole::Fourth,
            _ => return None,
        },
        // A single right Joy-Con held sideways rotates its legends.
        ControllerFamily::JoyConRight => match button {
            South => FaceRole::Confirm,
            West => FaceRole::Cancel,
            East => FaceRole::Third,
            North => FaceRole::Fourth,
            _ => return None,
        },
        ControllerFamily::Generic
        | ControllerFamily::Xbox
        | ControllerFamily::PlayStation
        | ControllerFamily::JoyConLeft => match button {
            South => FaceRole::Confirm,
            East => FaceRole::Cancel,
            West => FaceRole::Third,
            North => FaceRole::Fourth,
            _ => return None,
        },
    })
}

/// The face button that carries `role` on `family`, for drawing its glyph.
pub fn face_button(family: ControllerFamily, role: FaceRole) -> ui::input::ControllerButton {
    use ui::input::ControllerButton;
    [
        (GamepadButton::South, ControllerButton::South),
        (GamepadButton::East, ControllerButton::East),
        (GamepadButton::West, ControllerButton::West),
        (GamepadButton::North, ControllerButton::North),
    ]
    .into_iter()
    .find(|(button, _)| face_role(family, *button) == Some(role))
    .map_or(ControllerButton::South, |(_, glyph)| glyph)
}

#[derive(Clone, Copy, Debug)]
struct HeldDirection {
    direction: NavDirection,
    /// Whether Guide was held when the direction began; a repeat keeps the
    /// meaning it started with.
    chrome: bool,
    next: Instant,
    interval: Duration,
}

/// Pure controller-to-intent translation for one controller.
#[derive(Clone, Debug)]
pub struct ControllerNavMapper {
    family: ControllerFamily,
    passthrough: bool,
    stick: (i16, i16),
    hat: (i8, i8),
    held: Option<HeldDirection>,
    guide_down: bool,
    guide_chorded: bool,
    triggers: [bool; 2],
}

impl ControllerNavMapper {
    pub const fn new(family: ControllerFamily) -> Self {
        Self {
            family,
            passthrough: false,
            stick: (0, 0),
            hat: (0, 0),
            held: None,
            guide_down: false,
            guide_chorded: false,
            triggers: [false; 2],
        }
    }

    pub const fn family(&self) -> ControllerFamily {
        self.family
    }

    /// While a fullscreen window (typically a game) has focus, the
    /// controller is its input: only Guide chords reach the desktop.
    pub fn set_passthrough(&mut self, passthrough: bool) {
        if self.passthrough != passthrough && passthrough {
            self.held = None;
        }
        self.passthrough = passthrough;
    }

    /// Translate one event observed at `now`.
    pub fn feed(&mut self, event: ForwardedGamepadEvent, now: Instant) -> Vec<NavIntent> {
        let mut out = Vec::new();
        match event {
            ForwardedGamepadEvent::Axis { axis, value } => match axis {
                GamepadAxis::LeftX => {
                    self.stick.0 = value;
                    self.update_direction(now, &mut out);
                }
                GamepadAxis::LeftY => {
                    self.stick.1 = value;
                    self.update_direction(now, &mut out);
                }
                GamepadAxis::LeftTrigger => {
                    self.trigger(0, value, NavIntent::PreviousWorkspace, &mut out);
                }
                GamepadAxis::RightTrigger => {
                    self.trigger(1, value, NavIntent::NextWorkspace, &mut out);
                }
                GamepadAxis::RightX | GamepadAxis::RightY => {}
            },
            ForwardedGamepadEvent::Hat { x, y } => {
                self.hat = (x.signum(), y.signum());
                self.update_direction(now, &mut out);
            }
            ForwardedGamepadEvent::Button { button, pressed } => {
                self.button(button, pressed, &mut out);
            }
        }
        out
    }

    /// Emit the auto-repeats due by `now`.
    pub fn tick(&mut self, now: Instant) -> Vec<NavIntent> {
        let mut out = Vec::new();
        while let Some(held) = self.held.as_mut()
            && held.next <= now
        {
            let intent = direction_intent(held.direction, held.chrome);
            let interval = held.interval;
            held.next = held.next.checked_add(interval).unwrap_or(now);
            held.interval = interval.mul_f64(REPEAT_ACCELERATION).max(REPEAT_FASTEST);
            // A stalled caller must not burst a backlog of moves at once.
            if held.next < now {
                held.next = now.checked_add(held.interval).unwrap_or(now);
            }
            let chorded = held.chrome;
            self.emit(intent, chorded, &mut out);
        }
        out
    }

    /// When [`Self::tick`] next has work.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.held.map(|held| held.next)
    }

    /// Forget all held state, as when the controller disconnects or is
    /// leased away mid-press.
    pub fn reset(&mut self) {
        *self = Self {
            passthrough: self.passthrough,
            ..Self::new(self.family)
        };
    }

    fn emit(&mut self, intent: NavIntent, chorded: bool, out: &mut Vec<NavIntent>) {
        if chorded {
            self.guide_chorded = true;
        }
        if self.passthrough && !chorded {
            return;
        }
        out.push(intent);
    }

    fn button(&mut self, button: GamepadButton, pressed: bool, out: &mut Vec<NavIntent>) {
        if button == GamepadButton::Guide {
            if pressed {
                self.guide_down = true;
                self.guide_chorded = false;
            } else if self.guide_down {
                self.guide_down = false;
                if !self.guide_chorded {
                    // Guide is a chord key first; a bare tap is its own
                    // action, and belongs to the desktop even in a game.
                    out.push(NavIntent::QuickSettings);
                }
            }
            return;
        }
        if !pressed {
            return;
        }
        let chorded = self.guide_down;
        let intent = match button {
            GamepadButton::LeftBumper if chorded => NavIntent::MoveGroupLeft,
            GamepadButton::RightBumper if chorded => NavIntent::MoveGroupRight,
            GamepadButton::LeftBumper => NavIntent::PreviousTab,
            GamepadButton::RightBumper => NavIntent::NextTab,
            GamepadButton::Start => NavIntent::Help,
            GamepadButton::Select => NavIntent::Overview,
            GamepadButton::LeftStick | GamepadButton::RightStick | GamepadButton::Guide => {
                return;
            }
            GamepadButton::South
            | GamepadButton::East
            | GamepadButton::West
            | GamepadButton::North => match face_role(self.family, button) {
                Some(FaceRole::Confirm) => NavIntent::Activate,
                Some(FaceRole::Cancel) => NavIntent::Back,
                Some(FaceRole::Third) => NavIntent::Actions,
                Some(FaceRole::Fourth) => NavIntent::Search,
                None => return,
            },
        };
        self.emit(intent, chorded, out);
    }

    fn trigger(&mut self, index: usize, value: i16, intent: NavIntent, out: &mut Vec<NavIntent>) {
        let Some(down) = self.triggers.get_mut(index) else {
            return;
        };
        if !*down && value >= TRIGGER_PRESS {
            *down = true;
            let chorded = self.guide_down;
            self.emit(intent, chorded, out);
        } else if *down && value < TRIGGER_RELEASE {
            *down = false;
        }
    }

    fn update_direction(&mut self, now: Instant, out: &mut Vec<NavIntent>) {
        let previous = self.held.map(|held| held.direction);
        let desired =
            hat_direction(self.hat, previous).or_else(|| stick_direction(self.stick, previous));
        if desired == previous {
            return;
        }
        let Some(direction) = desired else {
            self.held = None;
            return;
        };
        let chrome = self.guide_down;
        self.held = Some(HeldDirection {
            direction,
            chrome,
            next: now.checked_add(REPEAT_DELAY).unwrap_or(now),
            interval: REPEAT_FIRST,
        });
        self.emit(direction_intent(direction, chrome), chrome, out);
        if self.passthrough && !chrome {
            self.held = None;
        }
    }
}

const fn direction_intent(direction: NavDirection, chrome: bool) -> NavIntent {
    match (direction, chrome) {
        (_, false) => NavIntent::Move(direction),
        (NavDirection::Left, true) => NavIntent::FocusPreviousWindow,
        (NavDirection::Right, true) => NavIntent::FocusNextWindow,
        (NavDirection::Up, true) => NavIntent::PreviousStrip,
        (NavDirection::Down, true) => NavIntent::NextStrip,
    }
}

/// The D-pad direction, keeping the held one through a diagonal roll so
/// sliding a thumb across the pad does not re-fire.
fn hat_direction(hat: (i8, i8), previous: Option<NavDirection>) -> Option<NavDirection> {
    let horizontal = match hat.0 {
        -1 => Some(NavDirection::Left),
        1 => Some(NavDirection::Right),
        _ => None,
    };
    let vertical = match hat.1 {
        -1 => Some(NavDirection::Up),
        1 => Some(NavDirection::Down),
        _ => None,
    };
    match (horizontal, vertical) {
        (Some(_), Some(v)) if previous == Some(v) => Some(v),
        (Some(h), _) => Some(h),
        (None, v) => v,
    }
}

/// The stick direction along its dominant axis, with hysteresis: a held
/// direction lasts until it falls under [`STICK_RELEASE`], a new one needs
/// [`STICK_PRESS`].
fn stick_direction(stick: (i16, i16), previous: Option<NavDirection>) -> Option<NavDirection> {
    let (x, y) = (i32::from(stick.0), i32::from(stick.1));
    let horizontal = x.unsigned_abs() >= y.unsigned_abs();
    let (magnitude, direction) = if horizontal {
        (
            x.unsigned_abs(),
            if x < 0 {
                NavDirection::Left
            } else {
                NavDirection::Right
            },
        )
    } else {
        (
            y.unsigned_abs(),
            // evdev's Y grows downward.
            if y < 0 {
                NavDirection::Up
            } else {
                NavDirection::Down
            },
        )
    };
    let threshold = if previous == Some(direction) {
        STICK_RELEASE
    } else {
        STICK_PRESS
    };
    (magnitude >= u32::from(threshold.unsigned_abs())).then_some(direction)
}

/// Legend family for a discovered controller.
pub fn controller_family(vendor_id: Option<u16>, name: &str) -> ControllerFamily {
    ControllerFamily::from_hid(vendor_id, name)
}

/// A descriptor the event loop should (stop) polling so controller input
/// wakes the compositor instead of waiting for unrelated work.
#[derive(Debug)]
pub enum ControllerWake {
    Watch { key: u64, fd: OwnedFd },
    Unwatch { key: u64 },
}

struct NavDevice {
    key: u64,
    source: LinuxPhysicalGamepadSource,
    mapper: ControllerNavMapper,
}

struct KnownController {
    family: ControllerFamily,
}

/// Owns the local controllers that drive the desktop.
pub struct ControllerNavRuntime {
    catalog: LinuxPhysicalGamepadCatalog,
    hotplug: Option<PhysicalGamepadHotplug>,
    known: BTreeMap<PhysicalGamepadId, KnownController>,
    devices: BTreeMap<PhysicalGamepadId, NavDevice>,
    /// Controllers that could not be opened; retried on the next hotplug.
    refused: BTreeSet<PhysicalGamepadId>,
    rescan: bool,
    backlog: bool,
    next_key: u64,
    wakes: Vec<ControllerWake>,
}

impl Default for ControllerNavRuntime {
    fn default() -> Self {
        Self::new(LinuxPhysicalGamepadCatalog::default())
    }
}

impl ControllerNavRuntime {
    pub fn new(catalog: LinuxPhysicalGamepadCatalog) -> Self {
        let mut wakes = Vec::new();
        let hotplug = match catalog.hotplug_watch() {
            Ok(hotplug) => {
                match hotplug.wake_fd() {
                    Ok(fd) => wakes.push(ControllerWake::Watch { key: 0, fd }),
                    Err(error) => {
                        tracing::warn!(%error, "controller hotplug cannot wake the compositor");
                    }
                }
                Some(hotplug)
            }
            Err(error) => {
                tracing::info!(%error, "controller hotplug is not observable here");
                None
            }
        };
        Self {
            catalog,
            hotplug,
            known: BTreeMap::new(),
            devices: BTreeMap::new(),
            refused: BTreeSet::new(),
            rescan: true,
            backlog: false,
            next_key: 1,
            wakes,
        }
    }

    /// Descriptor registrations queued since the last call.
    pub fn take_wakes(&mut self) -> Vec<ControllerWake> {
        std::mem::take(&mut self.wakes)
    }

    /// When [`Self::poll`] next has work without new input.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        if self.backlog {
            return Some(now);
        }
        self.devices
            .values()
            .filter_map(|device| device.mapper.next_deadline())
            .min()
    }

    /// Read every local controller and return the intents it produced with
    /// the legend family of the controller that produced each.
    /// `forwarded` names controllers leased away; `passthrough` is true
    /// while a fullscreen window owns controller input.
    pub fn poll(
        &mut self,
        now: Instant,
        forwarded: &[PhysicalGamepadId],
        passthrough: bool,
    ) -> Vec<(NavIntent, ControllerFamily)> {
        if self
            .hotplug
            .as_mut()
            .is_some_and(PhysicalGamepadHotplug::take_changes)
        {
            self.rescan = true;
            self.refused.clear();
        }
        if std::mem::take(&mut self.rescan) {
            self.refresh_catalog();
        }
        self.reconcile(forwarded);
        self.backlog = false;
        let mut out = Vec::new();
        let mut lost = Vec::new();
        for (id, device) in &mut self.devices {
            device.mapper.set_passthrough(passthrough);
            let family = device.mapper.family();
            match device.source.drain(READ_BUDGET) {
                Ok(drained) => {
                    self.backlog |= drained.saturated;
                    for event in drained.events {
                        out.extend(
                            device
                                .mapper
                                .feed(event, now)
                                .into_iter()
                                .map(|intent| (intent, family)),
                        );
                    }
                }
                Err(error) => {
                    tracing::info!(%error, "controller left desktop navigation");
                    lost.push(id.clone());
                    continue;
                }
            }
            out.extend(
                device
                    .mapper
                    .tick(now)
                    .into_iter()
                    .map(|intent| (intent, family)),
            );
        }
        if !lost.is_empty() {
            for id in lost {
                self.close(&id);
            }
            self.rescan = true;
        }
        out
    }

    fn refresh_catalog(&mut self) {
        match self.catalog.discover() {
            Ok(found) => {
                self.known = found
                    .into_iter()
                    .map(|info| {
                        let family = controller_family(info.vendor_id, &info.name);
                        (info.id, KnownController { family })
                    })
                    .collect();
            }
            Err(error) => tracing::warn!(%error, "could not enumerate game controllers"),
        }
    }

    fn reconcile(&mut self, forwarded: &[PhysicalGamepadId]) {
        let stale: Vec<PhysicalGamepadId> = self
            .devices
            .keys()
            .filter(|id| !self.known.contains_key(*id) || forwarded.contains(id))
            .cloned()
            .collect();
        for id in stale {
            self.close(&id);
        }
        let wanted: Vec<(PhysicalGamepadId, ControllerFamily)> = self
            .known
            .iter()
            .filter(|(id, _)| {
                !self.devices.contains_key(*id)
                    && !forwarded.contains(id)
                    && !self.refused.contains(*id)
            })
            .map(|(id, known)| (id.clone(), known.family))
            .collect();
        for (id, family) in wanted {
            self.open(id, family);
        }
    }

    fn open(&mut self, id: PhysicalGamepadId, family: ControllerFamily) {
        let mut source = match self.catalog.open(&id) {
            Ok(source) => source,
            Err(error) => {
                tracing::info!(%error, "controller is not available for desktop navigation");
                self.refused.insert(id);
                return;
            }
        };
        // Presses from before this controller joined (or before its lease
        // ended) must not replay as desktop actions.
        if source.drain(READ_BUDGET.saturating_mul(4)).is_err() {
            self.refused.insert(id);
            return;
        }
        let key = self.next_key;
        self.next_key = self.next_key.saturating_add(1);
        match source.wake_fd() {
            Ok(fd) => self.wakes.push(ControllerWake::Watch { key, fd }),
            Err(error) => tracing::warn!(%error, "controller cannot wake the compositor"),
        }
        self.devices.insert(
            id,
            NavDevice {
                key,
                source,
                mapper: ControllerNavMapper::new(family),
            },
        );
    }

    fn close(&mut self, id: &PhysicalGamepadId) {
        if let Some(device) = self.devices.remove(id) {
            self.wakes.push(ControllerWake::Unwatch { key: device.key });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start.checked_add(Duration::from_millis(ms)).unwrap()
    }

    fn press(button: GamepadButton) -> ForwardedGamepadEvent {
        ForwardedGamepadEvent::Button {
            button,
            pressed: true,
        }
    }

    fn release(button: GamepadButton) -> ForwardedGamepadEvent {
        ForwardedGamepadEvent::Button {
            button,
            pressed: false,
        }
    }

    fn axis(axis: GamepadAxis, value: i16) -> ForwardedGamepadEvent {
        ForwardedGamepadEvent::Axis { axis, value }
    }

    #[test]
    fn stick_inside_the_dead_zone_does_nothing() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        assert!(mapper.feed(axis(GamepadAxis::LeftX, 9_000), t).is_empty());
        assert!(mapper.feed(axis(GamepadAxis::LeftY, -17_000), t).is_empty());
        assert!(mapper.tick(at(t, 2_000)).is_empty());
        assert_eq!(mapper.next_deadline(), None);
    }

    #[test]
    fn stick_moves_along_its_dominant_axis_with_hysteresis() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        assert_eq!(
            mapper.feed(axis(GamepadAxis::LeftX, 25_000), t),
            vec![NavIntent::Move(NavDirection::Right)]
        );
        // Easing back toward the threshold keeps the same held direction
        // instead of re-firing.
        assert!(mapper.feed(axis(GamepadAxis::LeftX, 14_000), t).is_empty());
        assert!(mapper.feed(axis(GamepadAxis::LeftX, 20_000), t).is_empty());
        // Under the release level it ends; a new deflection fires again.
        assert!(mapper.feed(axis(GamepadAxis::LeftX, 5_000), t).is_empty());
        assert_eq!(mapper.next_deadline(), None);
        assert_eq!(
            mapper.feed(axis(GamepadAxis::LeftY, -30_000), t),
            vec![NavIntent::Move(NavDirection::Up)]
        );
    }

    #[test]
    fn a_held_direction_repeats_and_accelerates() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        assert_eq!(
            mapper.feed(ForwardedGamepadEvent::Hat { x: 0, y: 1 }, t),
            vec![NavIntent::Move(NavDirection::Down)]
        );
        assert!(
            mapper.tick(at(t, 379)).is_empty(),
            "no repeat inside the delay"
        );
        let mut fired = Vec::new();
        let mut now = t;
        while fired.len() < 8 {
            now = at(now, 1);
            if !mapper.tick(now).is_empty() {
                fired.push(now);
            }
        }
        assert_eq!(fired[0], at(t, 380));
        let gaps: Vec<Duration> = fired
            .windows(2)
            .map(|w| w[1].duration_since(w[0]))
            .collect();
        assert_eq!(gaps[0], REPEAT_FIRST);
        assert!(
            gaps.windows(2).all(|w| w[1] <= w[0]),
            "repeat never slows down: {gaps:?}"
        );
        assert!(gaps.last().unwrap() < &REPEAT_FIRST);
        assert!(gaps.iter().all(|gap| *gap >= REPEAT_FASTEST));
        // Releasing stops the repeat.
        assert!(
            mapper
                .feed(ForwardedGamepadEvent::Hat { x: 0, y: 0 }, now)
                .is_empty()
        );
        assert!(mapper.tick(at(t, 10_000)).is_empty());
    }

    #[test]
    fn a_stalled_caller_gets_one_repeat_not_a_burst() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        mapper.feed(ForwardedGamepadEvent::Hat { x: 1, y: 0 }, t);
        assert_eq!(mapper.tick(at(t, 5_000)).len(), 1);
    }

    #[test]
    fn guide_turns_directions_and_bumpers_into_chrome_moves() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        assert!(mapper.feed(press(GamepadButton::Guide), t).is_empty());
        assert_eq!(
            mapper.feed(ForwardedGamepadEvent::Hat { x: 1, y: 0 }, t),
            vec![NavIntent::FocusNextWindow]
        );
        mapper.feed(ForwardedGamepadEvent::Hat { x: 0, y: 0 }, t);
        assert_eq!(
            mapper.feed(axis(GamepadAxis::LeftY, 30_000), t),
            vec![NavIntent::NextStrip]
        );
        mapper.feed(axis(GamepadAxis::LeftY, 0), t);
        assert_eq!(
            mapper.feed(press(GamepadButton::LeftBumper), t),
            vec![NavIntent::MoveGroupLeft]
        );
        // A chord is not a tap: releasing Guide does not open anything.
        assert!(mapper.feed(release(GamepadButton::Guide), t).is_empty());
        assert_eq!(
            mapper.feed(press(GamepadButton::LeftBumper), t),
            vec![NavIntent::PreviousTab]
        );
        assert_eq!(
            mapper.feed(ForwardedGamepadEvent::Hat { x: -1, y: 0 }, t),
            vec![NavIntent::Move(NavDirection::Left)]
        );
    }

    #[test]
    fn a_bare_guide_tap_opens_quick_settings() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::PlayStation);
        assert!(mapper.feed(press(GamepadButton::Guide), t).is_empty());
        assert_eq!(
            mapper.feed(release(GamepadButton::Guide), t),
            vec![NavIntent::QuickSettings]
        );
    }

    #[test]
    fn a_repeat_keeps_the_meaning_it_started_with() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        mapper.feed(press(GamepadButton::Guide), t);
        mapper.feed(ForwardedGamepadEvent::Hat { x: 1, y: 0 }, t);
        mapper.feed(release(GamepadButton::Guide), t);
        assert_eq!(mapper.tick(at(t, 400)), vec![NavIntent::FocusNextWindow]);
    }

    #[test]
    fn face_buttons_follow_the_printed_legend() {
        let t = Instant::now();
        let roles = |family| {
            let mut mapper = ControllerNavMapper::new(family);
            [
                GamepadButton::South,
                GamepadButton::East,
                GamepadButton::West,
                GamepadButton::North,
            ]
            .map(|button| mapper.feed(press(button), t))
        };
        let xbox = roles(ControllerFamily::Xbox);
        assert_eq!(
            xbox,
            [
                vec![NavIntent::Activate],
                vec![NavIntent::Back],
                vec![NavIntent::Actions],
                vec![NavIntent::Search],
            ]
        );
        assert_eq!(roles(ControllerFamily::PlayStation), xbox);
        assert_eq!(roles(ControllerFamily::Generic), xbox);
        // Nintendo prints A east and B south: the same legends, mirrored.
        assert_eq!(
            roles(ControllerFamily::Nintendo),
            [
                vec![NavIntent::Back],
                vec![NavIntent::Activate],
                vec![NavIntent::Search],
                vec![NavIntent::Actions],
            ]
        );
        assert_eq!(
            roles(ControllerFamily::JoyConPair),
            roles(ControllerFamily::Nintendo)
        );
    }

    #[test]
    fn roles_agree_with_the_glyphs_the_guide_draws() {
        use ui::input::ControllerButton;
        for family in [
            ControllerFamily::Xbox,
            ControllerFamily::Nintendo,
            ControllerFamily::JoyConPair,
            ControllerFamily::JoyConRight,
            ControllerFamily::Generic,
        ] {
            for (evdev, glyph) in [
                (GamepadButton::South, ControllerButton::South),
                (GamepadButton::East, ControllerButton::East),
                (GamepadButton::West, ControllerButton::West),
                (GamepadButton::North, ControllerButton::North),
            ] {
                let expected = match glyph.label(family) {
                    "A" => FaceRole::Confirm,
                    "B" => FaceRole::Cancel,
                    "X" => FaceRole::Third,
                    "Y" => FaceRole::Fourth,
                    other => panic!("{family:?} {glyph:?} has legend {other}"),
                };
                assert_eq!(face_role(family, evdev), Some(expected), "{family:?}");
            }
        }
    }

    #[test]
    fn the_guide_draws_the_button_that_carries_each_role() {
        use ui::input::ControllerButton;
        assert_eq!(
            face_button(ControllerFamily::Xbox, FaceRole::Confirm),
            ControllerButton::South
        );
        assert_eq!(
            face_button(ControllerFamily::Nintendo, FaceRole::Confirm),
            ControllerButton::East
        );
        assert_eq!(
            face_button(ControllerFamily::Nintendo, FaceRole::Third),
            ControllerButton::North
        );
        assert_eq!(
            face_button(ControllerFamily::PlayStation, FaceRole::Cancel),
            ControllerButton::East
        );
    }

    #[test]
    fn triggers_switch_workspaces_once_per_pull() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        assert_eq!(
            mapper.feed(axis(GamepadAxis::RightTrigger, 30_000), t),
            vec![NavIntent::NextWorkspace]
        );
        assert!(
            mapper
                .feed(axis(GamepadAxis::RightTrigger, 32_000), t)
                .is_empty()
        );
        assert!(
            mapper
                .feed(axis(GamepadAxis::RightTrigger, 12_000), t)
                .is_empty()
        );
        assert!(
            mapper
                .feed(axis(GamepadAxis::RightTrigger, 20_000), t)
                .is_empty(),
            "still inside the release band"
        );
        mapper.feed(axis(GamepadAxis::RightTrigger, 0), t);
        assert_eq!(
            mapper.feed(axis(GamepadAxis::LeftTrigger, 20_000), t),
            vec![NavIntent::PreviousWorkspace]
        );
    }

    #[test]
    fn start_and_select_open_the_guide_and_overview() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Nintendo);
        assert_eq!(
            mapper.feed(press(GamepadButton::Start), t),
            vec![NavIntent::Help]
        );
        assert_eq!(
            mapper.feed(press(GamepadButton::Select), t),
            vec![NavIntent::Overview]
        );
        assert!(mapper.feed(release(GamepadButton::Start), t).is_empty());
    }

    #[test]
    fn passthrough_leaves_everything_but_guide_chords_to_the_game() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        mapper.set_passthrough(true);
        assert!(mapper.feed(press(GamepadButton::South), t).is_empty());
        assert!(
            mapper
                .feed(ForwardedGamepadEvent::Hat { x: 1, y: 0 }, t)
                .is_empty()
        );
        assert!(mapper.tick(at(t, 1_000)).is_empty(), "no repeat either");
        mapper.feed(ForwardedGamepadEvent::Hat { x: 0, y: 0 }, t);
        mapper.feed(press(GamepadButton::Guide), t);
        assert_eq!(
            mapper.feed(press(GamepadButton::Start), t),
            vec![NavIntent::Help]
        );
        assert_eq!(
            mapper.feed(ForwardedGamepadEvent::Hat { x: 1, y: 0 }, t),
            vec![NavIntent::FocusNextWindow]
        );
        mapper.feed(ForwardedGamepadEvent::Hat { x: 0, y: 0 }, t);
        assert!(mapper.feed(release(GamepadButton::Guide), t).is_empty());
        mapper.feed(press(GamepadButton::Guide), t);
        assert_eq!(
            mapper.feed(release(GamepadButton::Guide), t),
            vec![NavIntent::QuickSettings]
        );
    }

    #[test]
    fn a_d_pad_roll_through_a_diagonal_keeps_the_held_direction() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        mapper.feed(ForwardedGamepadEvent::Hat { x: 0, y: -1 }, t);
        assert!(
            mapper
                .feed(ForwardedGamepadEvent::Hat { x: 1, y: -1 }, t)
                .is_empty()
        );
        assert_eq!(
            mapper.feed(ForwardedGamepadEvent::Hat { x: 1, y: 0 }, t),
            vec![NavIntent::Move(NavDirection::Right)]
        );
    }

    #[test]
    fn the_d_pad_wins_over_a_resting_stick() {
        let t = Instant::now();
        let mut mapper = ControllerNavMapper::new(ControllerFamily::Xbox);
        mapper.feed(axis(GamepadAxis::LeftX, 30_000), t);
        assert_eq!(
            mapper.feed(ForwardedGamepadEvent::Hat { x: 0, y: 1 }, t),
            vec![NavIntent::Move(NavDirection::Down)]
        );
    }

    #[test]
    fn legend_families_come_from_vendor_then_name() {
        assert_eq!(
            controller_family(Some(0x045e), "Controller"),
            ControllerFamily::Xbox
        );
        assert_eq!(
            controller_family(Some(0x054c), "Wireless Controller"),
            ControllerFamily::PlayStation
        );
        assert_eq!(
            controller_family(Some(0x057e), "Nintendo Switch Pro Controller"),
            ControllerFamily::Nintendo
        );
        assert_eq!(
            controller_family(None, "Nintendo Switch Combined Joy-Cons"),
            ControllerFamily::JoyConPair
        );
    }
}
