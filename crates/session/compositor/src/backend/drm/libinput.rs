//! libinput devices: configured as they arrive, their events shaped by the
//! person's settings before the compositor routes them.
//!
//! Each device gets the plan [`input_config::plan_for`] decides when it is
//! added and again whenever the configuration changes. Key edges pass the
//! accessibility filters ([`AccessKeys`]); tablet tools are mapped by their
//! tablet's own settings ([`super::tablet`]).
//!
//! Tablet pads go to the client of the focused surface when it takes them
//! ([`State::pad_input`]). A pad button the person mapped to a shortcut
//! presses that shortcut wherever the focus is, as GNOME does, and the
//! client never sees it; a ring or strip no client takes scrolls.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use smithay::backend::input::{InputEvent as DeviceEvent, TabletToolEvent, TabletToolType};
use smithay::backend::libinput::LibinputInputBackend;
use smithay::input::keyboard::xkb;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::input::{
    AccelProfile, ClickMethod as LibinputClickMethod, Device, DeviceCapability,
    ScrollMethod as LibinputScrollMethod, SendEventsMode,
};
use smithay::utils::{Logical, Size};
use smithay::wayland::tablet_manager::{
    TabletDescriptor, TabletPadDescriptor, TabletPadGroupDescriptor,
};

use super::access_keys::AccessKeys;
use super::input_config::{
    self, ClickMethod, ConfigurableDevice, DeviceCaps, DevicePlan, InputConfig, ScrollMethod,
    SendEvents, device_key,
};
use super::tablet::{
    ButtonOutcome, EraserRoute, KeyChord, TabletConfig, apply_pressure_curve, chord_keycodes,
    eraser_route, map_tablet_position, pen_target, shape_normalised, stylus_button,
};
use crate::LoopData;
use crate::input::{InputEvent, PadAction, TabletEvent, TabletPadEvent, TabletPhase};
use crate::state::State;

/// An output as a tablet maps onto it: connector, position, logical size.
type OutputRect = (String, (i32, i32), (u32, u32));

/// Resolution used to normalise libinput's absolute tablet position.
const NORMALISE: i32 = 1 << 16;

/// The seat's input devices and what shapes their events.
pub(super) struct Input {
    config: InputConfig,
    devices: Vec<Device>,
    access: AccessKeys,
    /// The mouse-keys glide timer, while one is armed.
    glide: Option<RegistrationToken>,
    /// The keymap shortcut chords are typed on.
    keymap: Option<xkb::Keymap>,
    /// Pad rings and strips turned into scroll steps.
    dials: super::input_source::DialSteps,
    pads: Vec<Pad>,
}

/// A tablet pad as clients are told about it, and the mode its groups are
/// in.
struct Pad {
    device: Device,
    event: TabletPadEvent,
    modes: Vec<u32>,
}

impl Input {
    pub(super) fn new(config: InputConfig) -> Self {
        let mut access = AccessKeys::default();
        let _released = access.configure(config.bounce_keys, config.mouse_keys);
        Self {
            config,
            devices: Vec::new(),
            access,
            glide: None,
            keymap: None,
            dials: super::input_source::DialSteps::default(),
            pads: Vec::new(),
        }
    }

    /// Take a new configuration and apply it to every attached device.
    pub(super) fn set_config(&mut self, config: InputConfig, state: &mut State) {
        for event in self.access.configure(config.bounce_keys, config.mouse_keys) {
            state.handle_input(event);
        }
        let (delay, rate) = config.key_repeat_params();
        if let Some(keyboard) = state.seat.get_keyboard() {
            keyboard.change_repeat_info(
                i32::try_from(rate).unwrap_or(i32::MAX),
                i32::try_from(delay.as_millis()).unwrap_or(i32::MAX),
            );
        }
        self.config = config;
        input_config::reconfigure_all(&self.config, &mut self.devices);
    }
}

/// How far one ring or strip step scrolls, in logical pixels (a wheel
/// notch).
const DIAL_SCROLL: f64 = 15.0;

/// Route one tablet pad event. `mode` is the mode group of the control
/// and its mode, as libinput reports it with every event.
pub(super) fn dispatch_pad(
    input: &Rc<RefCell<Input>>,
    device: &Device,
    action: PadAction,
    mode: (u32, u32),
    state: &mut State,
) {
    let (switched, event, chord, steps) = {
        let mut this = input.borrow_mut();
        let this = &mut *this;
        let Some(pad) = this.pads.iter_mut().find(|pad| &pad.device == device) else {
            return;
        };
        let (group, mode) = mode;
        let switched = match usize::try_from(group)
            .ok()
            .and_then(|index| pad.modes.get_mut(index))
        {
            Some(current) if *current != mode => {
                *current = mode;
                Some(PadAction::Mode { group, mode })
            }
            _ => None,
        }
        .map(|switch| pad.event(switch));
        let event = pad.event(action);
        let chord = match action {
            PadAction::Button { button, pressed } => this.pad_chord(device, button).map(|chord| {
                if pressed {
                    this.chord(&chord)
                } else {
                    Vec::new()
                }
            }),
            _ => None,
        };
        // Followed even while a client takes the control, so a scroll
        // starts afresh where it is.
        let steps = this.dials.steps(&action);
        let scroll = f64::from(steps) * DIAL_SCROLL * this.config.scroll_speed;
        (switched, event, chord, (steps != 0).then_some(scroll))
    };
    if let Some(switched) = switched {
        state.pad_input(&switched);
    }
    if let Some(chord) = chord {
        // The press typed the shortcut; its release goes nowhere.
        for key in chord {
            state.handle_input(key);
        }
        return;
    }
    if state.pad_input(&event) {
        return;
    }
    if let Some(vertical) = steps {
        state.handle_input(InputEvent::PointerAxis {
            horizontal: 0.0,
            vertical,
        });
    }
}

impl Pad {
    fn event(&self, action: PadAction) -> TabletPadEvent {
        TabletPadEvent {
            action,
            ..self.event.clone()
        }
    }
}

/// A pad's buttons, rings and strips in libinput's mode groups, and the
/// mode each group is in.
fn pad_layout(device: &Device) -> (TabletPadDescriptor, Vec<u32>) {
    let count = |n: i32| u32::try_from(n).unwrap_or(0);
    let buttons = count(device.tablet_pad_number_of_buttons());
    let rings = count(device.tablet_pad_number_of_rings());
    let strips = count(device.tablet_pad_number_of_strips());
    let mut modes = Vec::new();
    let groups = (0..count(device.tablet_pad_number_of_mode_groups()))
        .filter_map(|index| device.tablet_pad_mode_group(index))
        .map(|group| {
            modes.push(group.mode());
            TabletPadGroupDescriptor {
                buttons: (0..buttons).filter(|&b| group.has_button(b)).collect(),
                rings: (0..rings).filter(|&r| group.has_ring(r)).collect(),
                strips: (0..strips).filter(|&s| group.has_strip(s)).collect(),
                modes: group.number_of_modes(),
            }
        })
        .collect();
    let identity = TabletDescriptor::from(device);
    let pad = TabletPadDescriptor {
        name: identity.name,
        syspath: identity.syspath,
        buttons,
        groups,
    };
    (pad, modes)
}

impl Input {
    /// A new pad: clients are told about it, attached to the pen tablet of
    /// its device group (the pad itself when that tablet is not known).
    fn add_pad(&mut self, device: &Device) -> Vec<InputEvent> {
        let (pad, modes) = pad_layout(device);
        let group = device.device_group();
        let tablet = self
            .devices
            .iter()
            .find(|known| {
                known.has_capability(DeviceCapability::TabletTool) && known.device_group() == group
            })
            .map_or_else(|| TabletDescriptor::from(device), TabletDescriptor::from);
        let pad = Pad {
            device: device.clone(),
            event: TabletPadEvent {
                pad,
                tablet,
                action: PadAction::Added,
            },
            modes,
        };
        let mut events = vec![InputEvent::TabletPad(pad.event.clone())];
        events.extend(
            (0_u32..)
                .zip(&pad.modes)
                .filter(|(_, mode)| **mode != 0)
                .map(|(group, &mode)| {
                    InputEvent::TabletPad(pad.event(PadAction::Mode { group, mode }))
                }),
        );
        self.pads.push(pad);
        events
    }

    /// A pad unplugged: clients are told.
    fn remove_pad(&mut self, device: &Device) -> Vec<InputEvent> {
        let Some(index) = self.pads.iter().position(|pad| &pad.device == device) else {
            return Vec::new();
        };
        let pad = self.pads.swap_remove(index);
        vec![InputEvent::TabletPad(pad.event(PadAction::Removed))]
    }

    /// The chord a pad button is mapped to in its tablet's settings.
    fn pad_chord(&self, device: &Device, button: u32) -> Option<KeyChord> {
        let key = device_key(device.id_vendor(), device.id_product(), device.name());
        // A pad is its own libinput device; its tablet's settings are found
        // by the pad's name, else by the tablet's model.
        let model = format!("{:04x}:{:04x}:", device.id_vendor(), device.id_product());
        self.config
            .tablets
            .get(&key)
            .or_else(|| {
                self.config
                    .tablets
                    .iter()
                    .find(|(known, _)| known.starts_with(&model))
                    .map(|(_, config)| config)
            })
            .and_then(|config| super::tablet::pad_button(config, button))
            .cloned()
    }
}

/// Route one libinput event.
pub(super) fn dispatch(
    input: &Rc<RefCell<Input>>,
    handle: &LoopHandle<'static, LoopData>,
    event: DeviceEvent<LibinputInputBackend>,
    state: &mut State,
    extent: Size<i32, Logical>,
) {
    let events = {
        let mut this = input.borrow_mut();
        match event {
            DeviceEvent::DeviceAdded { mut device } => {
                input_config::configure(&this.config, &mut device);
                let pad = if device.has_capability(DeviceCapability::TabletPad) {
                    this.add_pad(&device)
                } else {
                    Vec::new()
                };
                this.devices.push(device);
                pad
            }
            DeviceEvent::DeviceRemoved { device } => {
                this.devices.retain(|known| known != &device);
                this.remove_pad(&device)
            }
            DeviceEvent::TabletToolAxis { event } => {
                this.tablet(&event, TabletPhase::Motion, state)
            }
            DeviceEvent::TabletToolProximity { event } => {
                use smithay::backend::input::{ProximityState, TabletToolProximityEvent};
                let phase = match TabletToolProximityEvent::state(&event) {
                    ProximityState::In => TabletPhase::ProximityIn,
                    ProximityState::Out => TabletPhase::ProximityOut,
                };
                this.tablet(&event, phase, state)
            }
            DeviceEvent::TabletToolTip { event } => {
                use smithay::backend::input::{TabletToolTipEvent, TabletToolTipState};
                let phase = match TabletToolTipEvent::tip_state(&event) {
                    TabletToolTipState::Down => TabletPhase::TipDown,
                    TabletToolTipState::Up => TabletPhase::TipUp,
                };
                this.tablet(&event, phase, state)
            }
            DeviceEvent::TabletToolButton { event } => {
                use smithay::backend::input::{ButtonState, TabletToolButtonEvent};
                let phase = TabletPhase::Button {
                    button: TabletToolButtonEvent::button(&event),
                    pressed: TabletToolButtonEvent::button_state(&event) == ButtonState::Pressed,
                };
                this.tablet(&event, phase, state)
            }
            DeviceEvent::TouchDown { event } => {
                let (x, y) = touch_position(&event, state);
                vec![InputEvent::TouchDown {
                    slot: touch_slot(&event),
                    x,
                    y,
                }]
            }
            DeviceEvent::TouchMotion { event } => {
                let (x, y) = touch_position(&event, state);
                vec![InputEvent::TouchMotion {
                    slot: touch_slot(&event),
                    x,
                    y,
                }]
            }
            other => InputEvent::from_device(other, extent)
                .map(|event| match event {
                    // libinput has no scroll speed; the compositor scales.
                    InputEvent::PointerAxis {
                        horizontal,
                        vertical,
                    } => vec![InputEvent::PointerAxis {
                        horizontal: horizontal * this.config.scroll_speed,
                        vertical: vertical * this.config.scroll_speed,
                    }],
                    event => this.access.filter(event, Instant::now()),
                })
                .unwrap_or_default(),
        }
    };
    for event in events {
        state.handle_input(event);
    }
    arm_glide(input, handle);
}

/// A touch's place in the logical layout: on the touchscreen's own output
/// (see [`super::touch_map`]).
fn touch_position<E>(event: &E, state: &State) -> (f64, f64)
where
    E: smithay::backend::input::AbsolutePositionEvent<LibinputInputBackend>,
{
    // Normalised to the panel, at a resolution far finer than any output.
    const SCALE: i32 = 1 << 16;
    use super::touch_map::{TouchOutput, calibrate_touch_position, touch_output_for_name};
    let outputs: Vec<(
        String,
        smithay::utils::Rectangle<i32, Logical>,
        smithay::utils::Transform,
    )> = state
        .space
        .outputs()
        .filter_map(|output| {
            let geometry = state.space.output_geometry(output)?;
            Some((output.name(), geometry, output.current_transform()))
        })
        .collect();
    let device = event.device();
    let named = device.output_name();
    let Some(output) = touch_output_for_name(
        outputs
            .iter()
            .map(|(name, geometry, transform)| TouchOutput {
                connector_name: name.rsplit('/').next().unwrap_or(name),
                key: name,
                position: (geometry.loc.x, geometry.loc.y),
                size: (geometry.size.w, geometry.size.h),
                transform: *transform,
            }),
        named,
    ) else {
        return (0.0, 0.0);
    };
    calibrate_touch_position(
        output,
        event.x_transformed(SCALE) / f64::from(SCALE),
        event.y_transformed(SCALE) / f64::from(SCALE),
    )
}

/// A touch's slot number.
fn touch_slot<E>(event: &E) -> u32
where
    E: smithay::backend::input::TouchEvent<LibinputInputBackend>,
{
    crate::input::touch_slot(event.slot())
}

/// Keep the mouse-keys glide ticking while a direction key is held.
fn arm_glide(input: &Rc<RefCell<Input>>, handle: &LoopHandle<'static, LoopData>) {
    let mut this = input.borrow_mut();
    if this.glide.is_some() {
        return;
    }
    let Some(deadline) = this.access.next_deadline() else {
        return;
    };
    let ticking = Rc::clone(input);
    let token = handle.insert_source(Timer::from_deadline(deadline), move |now, (), data| {
        let mut this = ticking.borrow_mut();
        if let Some(event) = this.access.tick(now) {
            data.state.handle_input(event);
        }
        if let Some(next) = this.access.next_deadline() {
            TimeoutAction::ToInstant(next)
        } else {
            this.glide = None;
            TimeoutAction::Drop
        }
    });
    match token {
        Ok(token) => this.glide = Some(token),
        Err(error) => tracing::warn!(?error, "mouse keys: glide timer not armed"),
    }
}

impl Input {
    /// A tablet tool event shaped by its tablet's settings, plus the keys
    /// of a shortcut a remapped button or eraser presses.
    fn tablet<E: TabletToolEvent<LibinputInputBackend>>(
        &mut self,
        event: &E,
        phase: TabletPhase,
        state: &State,
    ) -> Vec<InputEvent> {
        let device = event.device();
        let key = device_key(device.id_vendor(), device.id_product(), device.name());
        let config = self.config.tablets.get(&key).cloned().unwrap_or_default();
        let outputs: Vec<OutputRect> = state
            .space
            .outputs()
            .filter_map(|output| {
                let geometry = state.space.output_geometry(output)?;
                let size = (
                    u32::try_from(geometry.size.w).ok()?,
                    u32::try_from(geometry.size.h).ok()?,
                );
                Some((output.name(), (geometry.loc.x, geometry.loc.y), size))
            })
            .collect();
        let Some(target) = pen_target(
            outputs
                .iter()
                .map(|(name, position, size)| (name.as_str(), name.as_str(), *position, *size)),
            config.output.as_deref(),
        ) else {
            return Vec::new();
        };
        let normalised = shape_normalised(
            (
                event.x_transformed(NORMALISE) / f64::from(NORMALISE),
                event.y_transformed(NORMALISE) / f64::from(NORMALISE),
            ),
            device.size(),
            target,
            config.keep_aspect,
            config.left_handed,
        );
        let (x, y) = map_tablet_position(normalised, target);
        let mut shaped = TabletEvent::from_tool(event, phase);
        shaped.x = x;
        shaped.y = y;
        shaped.pressure = apply_pressure_curve(config.pressure_curve, event.pressure());
        self.remap(shaped, &config)
    }

    /// Apply the tablet's button and eraser mapping.
    fn remap(&mut self, mut event: TabletEvent, config: &TabletConfig) -> Vec<InputEvent> {
        let mut chord = None;
        if event.tool.tool_type == TabletToolType::Eraser {
            match eraser_route(config) {
                EraserRoute::Native => {}
                EraserRoute::Pointer(button) => {
                    event.as_pointer = true;
                    event.tip_button = Some(button);
                }
                EraserRoute::Shortcut(shortcut) => {
                    event.as_pointer = true;
                    event.tip_button = None;
                    if event.phase == TabletPhase::TipDown {
                        chord = Some(shortcut);
                    }
                }
            }
        }
        if let TabletPhase::Button { button, pressed } = event.phase {
            match stylus_button(config, button) {
                ButtonOutcome::Button(code) => {
                    event.phase = TabletPhase::Button {
                        button: code,
                        pressed,
                    };
                }
                ButtonOutcome::Shortcut(shortcut) => {
                    // The chord runs on the press; the release is the
                    // button's own and goes nowhere.
                    event.phase = TabletPhase::Motion;
                    if pressed {
                        chord = Some(shortcut);
                    }
                }
                ButtonOutcome::Ignore => event.phase = TabletPhase::Motion,
            }
        }
        let mut events = vec![InputEvent::Tablet(event)];
        if let Some(chord) = chord {
            events.extend(self.chord(&chord));
        }
        events
    }

    /// The key events that type `chord`: modifiers down, the key down and
    /// up, modifiers up in reverse.
    fn chord(&mut self, chord: &KeyChord) -> Vec<InputEvent> {
        if self.keymap.is_none() {
            let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
            self.keymap = xkb::Keymap::new_from_names(
                &context,
                "",
                "",
                "",
                "",
                None,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            );
        }
        let keymap = self.keymap.as_ref();
        let Some(codes) = chord_keycodes(chord, |ch| {
            keymap.and_then(|keymap| char_keycode(keymap, ch))
        }) else {
            tracing::debug!(?chord, "tablet: shortcut has no key on this layout");
            return Vec::new();
        };
        let down = codes.iter().map(|&keycode| InputEvent::Key {
            keycode,
            pressed: true,
        });
        let up = codes.iter().rev().map(|&keycode| InputEvent::Key {
            keycode,
            pressed: false,
        });
        down.chain(up).collect()
    }
}

/// The evdev keycode of the key typing `ch` on its first level.
fn char_keycode(keymap: &xkb::Keymap, ch: char) -> Option<u32> {
    let (min, max) = (keymap.min_keycode().raw(), keymap.max_keycode().raw());
    (min..=max).find_map(|raw| {
        let syms = keymap.key_get_syms_by_level(xkb::Keycode::new(raw), 0, 0);
        syms.iter()
            .any(|sym| sym.key_char() == Some(ch))
            .then(|| raw.saturating_sub(8))
    })
}

impl TabletEvent {
    /// A libinput tool event's identity and axes, its position still raw.
    fn from_tool<E: TabletToolEvent<LibinputInputBackend>>(event: &E, phase: TabletPhase) -> Self {
        let device = event.device();
        Self {
            tablet: smithay::wayland::tablet_manager::TabletDescriptor {
                name: device.name().to_string(),
                usb_id: smithay::backend::input::Device::usb_id(&device),
                syspath: smithay::backend::input::Device::syspath(&device),
            },
            tool: event.tool(),
            phase,
            x: 0.0,
            y: 0.0,
            pressure: event.pressure(),
            distance: event.distance(),
            tilt: event.tilt(),
            rotation: event.rotation(),
            slider: event.slider_position(),
            wheel: (event.wheel_delta(), event.wheel_delta_discrete()),
            as_pointer: false,
            tip_button: Some(crate::input::BTN_LEFT),
        }
    }
}

/// libinput half of [`ConfigurableDevice`]. Every failure is ignored:
/// libinput errors on an unsupported setting, and [`input_config::plan_for`]'s
/// capability gates already avoid asking; anything left is a driver quirk
/// that must not take the session down.
impl ConfigurableDevice for Device {
    fn caps(&self) -> DeviceCaps {
        device_caps(self)
    }

    fn apply(&mut self, plan: &DevicePlan) {
        if let Some(on) = plan.tap {
            let _ = self.config_tap_set_enabled(on);
        }
        if let Some(on) = plan.tap_drag {
            let _ = self.config_tap_set_drag_enabled(on);
        }
        if let Some(on) = plan.tap_drag_lock {
            let _ = self.config_tap_set_drag_lock_enabled(on);
        }
        if let Some(on) = plan.natural_scroll {
            let _ = self.config_scroll_set_natural_scroll_enabled(on);
        }
        if let Some(on) = plan.disable_while_typing {
            let _ = self.config_dwt_set_enabled(on);
        }
        if let Some(on) = plan.middle_emulation {
            let _ = self.config_middle_emulation_set_enabled(on);
        }
        if let Some(speed) = plan.accel_speed() {
            let _ = self.config_accel_set_speed(speed);
        }
        if let Some(on) = plan.left_handed {
            let _ = self.config_left_handed_set(on);
        }
        if let Some(mode) = plan.send_events {
            let _ = self.config_send_events_set_mode(match mode {
                SendEvents::Enabled => SendEventsMode::ENABLED,
                SendEvents::Disabled => SendEventsMode::DISABLED,
                SendEvents::DisabledOnExternalMouse => SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE,
            });
        }
        if let Some(flat) = plan.flat_acceleration {
            let _ = self.config_accel_set_profile(if flat {
                AccelProfile::Flat
            } else {
                AccelProfile::Adaptive
            });
        }
        if let Some(method) = plan.click_method {
            let _ = self.config_click_set_method(match method {
                ClickMethod::ButtonAreas => LibinputClickMethod::ButtonAreas,
                ClickMethod::Clickfinger => LibinputClickMethod::Clickfinger,
            });
        }
        if let Some(method) = plan.scroll_method {
            let _ = self.config_scroll_set_method(match method {
                ScrollMethod::TwoFinger => LibinputScrollMethod::TwoFinger,
                ScrollMethod::Edge => LibinputScrollMethod::Edge,
            });
        }
    }
}

/// Ask one libinput device what it can be configured to do.
fn device_caps(device: &Device) -> DeviceCaps {
    let click_methods = device.config_click_methods();
    let scroll_methods = device.config_scroll_methods();
    DeviceCaps {
        supports_tap: device.config_tap_finger_count() > 0,
        supports_natural_scroll: device.config_scroll_has_natural_scroll(),
        supports_accel: device.config_accel_is_available(),
        supports_dwt: device.config_dwt_is_available(),
        supports_middle_emulation: device.config_middle_emulation_is_available(),
        is_pointer: device.has_capability(DeviceCapability::Pointer),
        supports_left_handed: device.config_left_handed_is_available(),
        supports_flat_acceleration: device.config_accel_profiles().contains(&AccelProfile::Flat),
        supports_click_areas: click_methods.contains(&LibinputClickMethod::ButtonAreas),
        supports_clickfinger: click_methods.contains(&LibinputClickMethod::Clickfinger),
        default_click_method: match device.config_click_default_method() {
            Some(LibinputClickMethod::ButtonAreas) => Some(ClickMethod::ButtonAreas),
            Some(LibinputClickMethod::Clickfinger) => Some(ClickMethod::Clickfinger),
            _ => None,
        },
        supports_two_finger_scroll: scroll_methods.contains(&LibinputScrollMethod::TwoFinger),
        supports_edge_scroll: scroll_methods.contains(&LibinputScrollMethod::Edge),
        supports_disable_on_external_mouse: device
            .config_send_events_modes()
            .contains(SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE),
        // Key 30 is A: a device with letters, not a power button.
        is_keyboard: device.has_capability(DeviceCapability::Keyboard)
            && device.keyboard_has_key(30).unwrap_or(false),
        is_touch: device.has_capability(DeviceCapability::Touch),
        is_tablet_tool: device.has_capability(DeviceCapability::TabletTool),
        is_tablet_pad: device.has_capability(DeviceCapability::TabletPad),
    }
}
