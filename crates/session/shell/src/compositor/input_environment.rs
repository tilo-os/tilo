//! The session's input devices and the kind of input used last.
//!
//! Shortcut help and on-screen keyboard policy must describe the hardware in
//! front of the person, not a guess. Backends report device arrival and
//! removal with the capabilities the device really has, plus the class of
//! each physical event; this tracker folds both into the
//! [`ui::InteractionEnvironment`] the shell chrome reads. Nothing is assumed
//! available until a device says so.

use std::collections::BTreeMap;

use ui::{
    ControllerFamily, InputAvailability, InputDevice, InputModality, InteractionEnvironment,
    PointerPrecision,
};

/// Devices currently attached, keyed by the backend's device identity, and
/// the modality of the most recent physical event.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InputEnvironment {
    devices: BTreeMap<String, InputAvailability>,
    last_used: Option<InputModality>,
    /// Legend family of the controller used most recently. Controllers are
    /// read outside libinput and announce no device, so using one is what
    /// makes it available.
    controller: Option<ControllerFamily>,
    typed_since_touch: bool,
    tablet_mode: bool,
}

impl InputEnvironment {
    /// Record a device with the capabilities it reported. Returns whether
    /// the combined availability changed.
    pub fn device_added(&mut self, id: impl Into<String>, capabilities: InputAvailability) -> bool {
        let before = self.available();
        self.devices.insert(id.into(), capabilities);
        before != self.available()
    }

    /// Forget a device. Returns whether the combined availability changed.
    pub fn device_removed(&mut self, id: &str) -> bool {
        let before = self.available();
        self.devices.remove(id);
        before != self.available()
    }

    /// Record the class of one physical input event. Using a device proves
    /// it exists even when no backend announced it (a headless session's
    /// injected input). Returns whether anything changed.
    pub fn note_used(&mut self, modality: InputModality) -> bool {
        let changed = self.last_used != Some(modality);
        self.last_used = Some(modality);
        match modality {
            InputModality::Keyboard => self.typed_since_touch = true,
            InputModality::Touch => self.typed_since_touch = false,
            InputModality::Pointer | InputModality::Controller => {}
        }
        changed
    }

    /// Record a controller press. Returns whether anything changed.
    pub fn note_controller(&mut self, family: ControllerFamily) -> bool {
        let changed = self.controller != Some(family);
        self.controller = Some(family);
        self.note_used(InputModality::Controller) || changed
    }

    /// Whether a physical key was pressed after the most recent touch (or
    /// before any touch). Pointer use in between does not reset it.
    pub fn typed_since_touch(&self) -> bool {
        self.typed_since_touch
    }

    /// Record a convertible's tablet-mode switch. Returns whether it changed.
    pub fn set_tablet_mode(&mut self, on: bool) -> bool {
        let changed = self.tablet_mode != on;
        self.tablet_mode = on;
        changed
    }

    /// Whether a convertible reports that its keyboard is folded away.
    pub fn tablet_mode(&self) -> bool {
        self.tablet_mode
    }

    /// The modality of the most recent physical input, if any arrived yet.
    pub fn last_used(&self) -> Option<InputModality> {
        self.last_used
    }

    /// Union of every attached device's capabilities and every modality in
    /// use.
    pub fn available(&self) -> InputAvailability {
        let attached = self
            .devices
            .values()
            .fold(InputAvailability::NONE, |all, device| all.union(*device));
        let attached = if self.controller.is_some() {
            attached.with(InputDevice::Controller)
        } else {
            attached
        };
        match self.last_used {
            Some(InputModality::Keyboard) => attached.with(InputDevice::Keyboard),
            Some(InputModality::Pointer) => attached.with(InputDevice::Pointer),
            Some(InputModality::Touch) => attached.with(InputDevice::Touch),
            Some(InputModality::Controller) => attached.with(InputDevice::Controller),
            None => attached,
        }
    }

    /// The environment the shell chrome should adapt to. Before any input
    /// arrives, a touchscreen without a keyboard is treated as touch in use,
    /// so a tablet shows gesture help from the first frame.
    pub fn environment(&self) -> InteractionEnvironment {
        let available = self.available();
        let modality = self.last_used.unwrap_or_else(|| {
            if available.contains(InputDevice::Touch) && !available.contains(InputDevice::Keyboard)
            {
                InputModality::Touch
            } else {
                InputModality::Pointer
            }
        });
        InteractionEnvironment {
            modality,
            pointer_precision: if modality == InputModality::Touch {
                PointerPrecision::Coarse
            } else {
                PointerPrecision::Fine
            },
            available,
            controller: self.controller,
            ..InteractionEnvironment::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(device: InputDevice) -> InputAvailability {
        InputAvailability::NONE.with(device)
    }

    #[test]
    fn nothing_is_available_until_a_device_reports_it() {
        let env = InputEnvironment::default().environment();
        assert_eq!(env.available, InputAvailability::NONE);
        assert_eq!(env.preferred_hint_modality(), InputModality::Pointer);
    }

    #[test]
    fn a_touch_only_device_leads_with_gestures_before_any_input() {
        let mut input = InputEnvironment::default();
        assert!(input.device_added("panel", only(InputDevice::Touch)));
        let env = input.environment();
        assert_eq!(env.modality, InputModality::Touch);
        assert_eq!(env.pointer_precision, PointerPrecision::Coarse);
        assert_eq!(env.preferred_hint_modality(), InputModality::Touch);
    }

    #[test]
    fn a_laptop_with_a_touchscreen_follows_what_was_used_last() {
        let mut input = InputEnvironment::default();
        input.device_added("keyboard", only(InputDevice::Keyboard));
        input.device_added("touchpad", only(InputDevice::Pointer));
        input.device_added("panel", only(InputDevice::Touch));
        assert_eq!(
            input.environment().preferred_hint_modality(),
            InputModality::Keyboard
        );
        assert!(input.note_used(InputModality::Touch));
        assert_eq!(
            input.environment().preferred_hint_modality(),
            InputModality::Touch
        );
        assert!(!input.note_used(InputModality::Touch));
        assert!(input.note_used(InputModality::Keyboard));
        assert_eq!(
            input.environment().preferred_hint_modality(),
            InputModality::Keyboard
        );
    }

    #[test]
    fn unplugging_the_only_keyboard_removes_it_from_availability() {
        let mut input = InputEnvironment::default();
        input.device_added("keyboard", only(InputDevice::Keyboard));
        input.device_added("panel", only(InputDevice::Touch));
        assert!(input.device_removed("keyboard"));
        assert!(!input.available().contains(InputDevice::Keyboard));
        assert_eq!(
            input.environment().preferred_hint_modality(),
            InputModality::Touch
        );
        assert!(!input.device_removed("keyboard"));
    }

    #[test]
    fn a_controller_leads_until_another_input_is_used_and_keeps_its_legends() {
        let mut input = InputEnvironment::default();
        input.device_added("keyboard", only(InputDevice::Keyboard));
        assert!(input.note_controller(ControllerFamily::PlayStation));
        assert!(!input.note_controller(ControllerFamily::PlayStation));
        let env = input.environment();
        assert_eq!(env.preferred_hint_modality(), InputModality::Controller);
        assert_eq!(env.controller, Some(ControllerFamily::PlayStation));
        input.note_used(InputModality::Keyboard);
        let env = input.environment();
        assert_eq!(env.preferred_hint_modality(), InputModality::Keyboard);
        assert!(env.available.contains(InputDevice::Controller));
        assert_eq!(env.controller, Some(ControllerFamily::PlayStation));
    }

    #[test]
    fn typing_is_remembered_across_pointer_use_until_the_next_touch() {
        let mut input = InputEnvironment::default();
        assert!(!input.typed_since_touch());
        input.note_used(InputModality::Keyboard);
        input.note_used(InputModality::Pointer);
        assert!(input.typed_since_touch());
        input.note_used(InputModality::Touch);
        assert!(!input.typed_since_touch());
        assert!(input.set_tablet_mode(true));
        assert!(!input.set_tablet_mode(true));
        assert!(input.tablet_mode());
    }

    #[test]
    fn a_second_device_of_the_same_kind_changes_nothing_visible() {
        let mut input = InputEnvironment::default();
        assert!(input.device_added("mouse", only(InputDevice::Pointer)));
        assert!(!input.device_added("touchpad", only(InputDevice::Pointer)));
        assert!(!input.device_removed("mouse"));
        assert!(input.available().contains(InputDevice::Pointer));
    }
}
