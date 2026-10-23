//! Device-neutral action bindings and the legends shown beside controls.
//!
//! Applications bind an action id to semantic keyboard modifiers, controller
//! *positions*, or touch gestures. The host supplies platform and controller
//! conventions when painting, so an app never hard-codes `Ctrl`, Xbox `A`, or
//! a textual gesture description into its UI.

use std::collections::BTreeMap;
use tilo_sort::CompactSort as _;

use serde::{Deserialize, Serialize};

use crate::interaction::{ControllerFamily, InteractionEnvironment, KeyboardFamily};
use crate::platform::{Platform, PlatformConventions};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardModifier {
    /// Application command: Command on macOS, Control elsewhere.
    Command,
    Control,
    Alt,
    Shift,
    /// Desktop/system modifier: Command on Mac keyboards, Windows/Super elsewhere.
    System,
    Fn,
    /// The right Alt key of layouts that type a third symbol with it.
    AltGr,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyLegend {
    Character(String),
    Left,
    Right,
    Up,
    Down,
    Tab,
    Enter,
    Escape,
    Space,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    PrintScreen,
    NumLock,
    Function(u8),
}

impl KeyLegend {
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "left" | "arrowleft" => Self::Left,
            "right" | "arrowright" => Self::Right,
            "up" | "arrowup" => Self::Up,
            "down" | "arrowdown" => Self::Down,
            "tab" => Self::Tab,
            "enter" | "return" => Self::Enter,
            "esc" | "escape" => Self::Escape,
            "space" => Self::Space,
            "backspace" => Self::Backspace,
            "delete" | "del" => Self::Delete,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" | "page_up" => Self::PageUp,
            "pagedown" | "page_down" => Self::PageDown,
            "insert" | "ins" => Self::Insert,
            "printscreen" | "print" => Self::PrintScreen,
            "numlock" => Self::NumLock,
            value => value
                .strip_prefix('f')
                .and_then(|number| number.parse::<u8>().ok())
                .map_or_else(|| Self::Character(name.to_owned()), Self::Function),
        }
    }

    pub fn label(&self, platform: PlatformConventions) -> String {
        let family = if platform.platform() == Platform::MacOs {
            KeyboardFamily::Apple
        } else {
            KeyboardFamily::Pc
        };
        self.label_for(platform, family)
    }

    /// The legend of this key on a keyboard of `family` typing with the
    /// active layout ([`crate::keycaps::active`]).
    pub fn label_for(&self, _platform: PlatformConventions, family: KeyboardFamily) -> String {
        let keycaps = crate::keycaps::active().on_family(family);
        match self {
            Self::Character(value) => keycaps.key(value),
            Self::Function(number) => format!("F{number}"),
            named => keycaps.key(named.name()),
        }
    }

    /// The canonical name of a named key.
    fn name(&self) -> &'static str {
        match self {
            Self::Character(_) | Self::Function(_) => "",
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::Tab => "tab",
            Self::Enter => "enter",
            Self::Escape => "escape",
            Self::Space => "space",
            Self::Backspace => "backspace",
            Self::Delete => "delete",
            Self::Home => "home",
            Self::End => "end",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Insert => "insert",
            Self::PrintScreen => "printscreen",
            Self::NumLock => "numlock",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KeyboardBinding {
    pub modifiers: Vec<KeyboardModifier>,
    pub key: KeyLegend,
}

impl KeyboardBinding {
    pub fn new(modifiers: impl IntoIterator<Item = KeyboardModifier>, key: KeyLegend) -> Self {
        let mut modifiers: Vec<_> = modifiers.into_iter().collect();
        modifiers.compact_sort();
        modifiers.dedup();
        Self { modifiers, key }
    }

    pub fn command(key: impl AsRef<str>) -> Self {
        Self::new(
            [KeyboardModifier::Command],
            KeyLegend::from_name(key.as_ref()),
        )
    }

    pub fn system(key: impl AsRef<str>) -> Self {
        Self::new(
            [KeyboardModifier::System],
            KeyLegend::from_name(key.as_ref()),
        )
    }

    /// One label per physical-looking cap; callers should not join these
    /// into prose because [`widget::InputHint`](crate::widget::InputHint)
    /// paints each separately.
    pub fn keycap_labels(&self, platform: PlatformConventions) -> Vec<String> {
        let family = if platform.platform() == Platform::MacOs {
            KeyboardFamily::Apple
        } else {
            KeyboardFamily::Pc
        };
        self.keycap_labels_for(platform, family)
    }

    pub fn keycap_labels_for(
        &self,
        platform: PlatformConventions,
        family: KeyboardFamily,
    ) -> Vec<String> {
        let mut display_modifiers = self.modifiers.clone();
        display_modifiers.compact_sort_by_key(|modifier| crate::keycaps::display_rank(*modifier));
        display_modifiers
            .into_iter()
            .map(|modifier| modifier_label(modifier, platform, family).to_owned())
            .chain(std::iter::once(self.key.label_for(platform, family)))
            .collect()
    }

    pub fn accessible_label(&self, platform: PlatformConventions) -> String {
        self.keycap_labels(platform).join(" + ")
    }
}

/// What `modifier` is called: the platform's own convention where the host
/// is macOS or Windows, otherwise the legend the active layout prints on it.
fn modifier_label(
    modifier: KeyboardModifier,
    platform: PlatformConventions,
    family: KeyboardFamily,
) -> &'static str {
    let family = if platform.platform() == Platform::MacOs {
        KeyboardFamily::Apple
    } else {
        family
    };
    if (modifier, platform.platform(), family)
        == (
            KeyboardModifier::System,
            Platform::Windows,
            KeyboardFamily::Pc,
        )
    {
        return "⊞";
    }
    crate::keycaps::active()
        .on_family(family)
        .modifier(modifier)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerButton {
    South,
    East,
    West,
    North,
    DpadLeft,
    DpadRight,
    DpadUp,
    DpadDown,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    LeftStick,
    RightStick,
    Start,
    Select,
    Guide,
    Sl,
    Sr,
}

impl ControllerButton {
    pub fn label(self, family: ControllerFamily) -> &'static str {
        use ControllerButton::{East, North, South, West};
        match (self, family) {
            (
                South,
                ControllerFamily::Xbox | ControllerFamily::Generic | ControllerFamily::JoyConRight,
            )
            | (East, ControllerFamily::Nintendo | ControllerFamily::JoyConPair) => "A",
            (East, ControllerFamily::Xbox | ControllerFamily::Generic)
            | (South, ControllerFamily::Nintendo | ControllerFamily::JoyConPair)
            | (West, ControllerFamily::JoyConRight) => "B",
            (West, ControllerFamily::Xbox | ControllerFamily::Generic)
            | (North, ControllerFamily::Nintendo | ControllerFamily::JoyConPair)
            | (East, ControllerFamily::JoyConRight) => "X",
            (
                North,
                ControllerFamily::Xbox | ControllerFamily::Generic | ControllerFamily::JoyConRight,
            )
            | (West, ControllerFamily::Nintendo | ControllerFamily::JoyConPair) => "Y",
            (South, ControllerFamily::PlayStation) => "×",
            (East, ControllerFamily::PlayStation) => "○",
            (West, ControllerFamily::PlayStation) => "□",
            (North, ControllerFamily::PlayStation) => "△",
            // A left Joy-Con is rotated counter-clockwise when used alone
            // with SL/SR along the top edge, so its printed arrows rotate
            // with it. Bindings remain physical positions.
            (South, ControllerFamily::JoyConLeft) => "←",
            (East, ControllerFamily::JoyConLeft) => "↓",
            (West, ControllerFamily::JoyConLeft) => "↑",
            (North, ControllerFamily::JoyConLeft) => "→",
            (Self::DpadLeft, _) => "D-pad ←",
            (Self::DpadRight, _) => "D-pad →",
            (Self::DpadUp, _) => "D-pad ↑",
            (Self::DpadDown, _) => "D-pad ↓",
            (Self::LeftBumper, ControllerFamily::PlayStation) => "L1",
            (Self::RightBumper, ControllerFamily::PlayStation) => "R1",
            (Self::LeftTrigger, ControllerFamily::PlayStation) => "L2",
            (Self::RightTrigger, ControllerFamily::PlayStation) => "R2",
            (Self::LeftBumper, _) => "LB",
            (Self::RightBumper, _) => "RB",
            (Self::LeftTrigger, _) => "LT",
            (Self::RightTrigger, _) => "RT",
            (Self::LeftStick, _) => "L3",
            (Self::RightStick, _) => "R3",
            (Self::Start, ControllerFamily::Nintendo | ControllerFamily::JoyConPair)
            | (Self::Start | Self::Select, ControllerFamily::JoyConRight) => "+",
            (Self::Select, ControllerFamily::Nintendo | ControllerFamily::JoyConPair)
            | (Self::Start | Self::Select, ControllerFamily::JoyConLeft) => "−",
            (Self::Start, _) => "Menu",
            (Self::Select, _) => "View",
            (Self::Guide, ControllerFamily::JoyConLeft) => "Capture",
            (
                Self::Guide,
                ControllerFamily::Nintendo
                | ControllerFamily::JoyConRight
                | ControllerFamily::JoyConPair,
            ) => "Home",
            (Self::Guide, _) => "Guide",
            (Self::Sl, _) => "SL",
            (Self::Sr, _) => "SR",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ControllerBinding {
    pub buttons: Vec<ControllerButton>,
}

impl ControllerBinding {
    pub fn new(buttons: impl IntoIterator<Item = ControllerButton>) -> Self {
        let mut buttons: Vec<_> = buttons.into_iter().collect();
        buttons.compact_sort();
        buttons.dedup();
        Self { buttons }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchGesture {
    Tap,
    DoubleTap,
    LongPress,
    SwipeLeft { fingers: u8 },
    SwipeRight { fingers: u8 },
    SwipeUp { fingers: u8 },
    SwipeDown { fingers: u8 },
    PinchIn,
    PinchOut,
}

impl TouchGesture {
    pub fn compact_label(self) -> String {
        match self {
            Self::Tap => "Tap".into(),
            Self::DoubleTap => "2× tap".into(),
            Self::LongPress => "Hold".into(),
            Self::SwipeLeft { fingers } => format!("{fingers}☝ ←"),
            Self::SwipeRight { fingers } => format!("{fingers}☝ →"),
            Self::SwipeUp { fingers } => format!("{fingers}☝ ↑"),
            Self::SwipeDown { fingers } => format!("{fingers}☝ ↓"),
            Self::PinchIn => "Pinch −".into(),
            Self::PinchOut => "Pinch +".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "device", content = "binding")]
pub enum InputBinding {
    Keyboard(KeyboardBinding),
    Controller(ControllerBinding),
    Touch(TouchGesture),
}

impl InputBinding {
    pub fn available_in(&self, environment: InteractionEnvironment) -> bool {
        use crate::interaction::InputDevice;

        match self {
            Self::Keyboard(_) => environment.available.contains(InputDevice::Keyboard),
            Self::Controller(_) => environment.available.contains(InputDevice::Controller),
            Self::Touch(_) => environment.available.contains(InputDevice::Touch),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionBindings {
    pub label: String,
    pub bindings: Vec<InputBinding>,
}

/// Rebindable, app-owned actions. This intentionally stores stable action ids
/// instead of callbacks so apps can persist the map in their ordinary state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSet {
    actions: BTreeMap<String, ActionBindings>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingConflict {
    pub action_id: String,
}

impl BindingSet {
    pub fn insert_action(
        &mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        bindings: Vec<InputBinding>,
    ) {
        self.actions.insert(
            id.into(),
            ActionBindings {
                label: label.into(),
                bindings,
            },
        );
    }

    pub fn actions(&self) -> &BTreeMap<String, ActionBindings> {
        &self.actions
    }

    /// Replace one binding after checking the whole application for a
    /// collision. Passing `allow_conflict` is an explicit UX choice; silent
    /// shadowing is never the default.
    pub fn rebind(
        &mut self,
        action_id: &str,
        index: usize,
        binding: InputBinding,
        allow_conflict: bool,
    ) -> Result<(), BindingConflict> {
        if !allow_conflict
            && let Some((id, _)) = self.actions.iter().find(|(id, action)| {
                id.as_str() != action_id && action.bindings.iter().any(|item| item == &binding)
            })
        {
            return Err(BindingConflict {
                action_id: id.clone(),
            });
        }
        let Some(action) = self.actions.get_mut(action_id) else {
            return Ok(());
        };
        if let Some(slot) = action.bindings.get_mut(index) {
            *slot = binding;
        } else {
            action.bindings.push(binding);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_legends_follow_the_device_and_rebinding_reports_conflicts() {
        {
            let binding = KeyboardBinding::new(
                [
                    KeyboardModifier::Command,
                    KeyboardModifier::Alt,
                    KeyboardModifier::Shift,
                ],
                KeyLegend::Enter,
            );
            assert_eq!(
                binding.keycap_labels(PlatformConventions::new(Platform::MacOs)),
                ["⌘", "⌥", "⇧", "↩"]
            );
            let external_apple = KeyboardBinding::new(
                [KeyboardModifier::System, KeyboardModifier::Alt],
                KeyLegend::Enter,
            );
            assert_eq!(
                external_apple.keycap_labels_for(
                    PlatformConventions::new(Platform::Linux),
                    KeyboardFamily::Apple,
                ),
                ["⌘", "⌥", "↩"]
            );
        }

        {
            assert_eq!(ControllerButton::South.label(ControllerFamily::Xbox), "A");
            assert_eq!(
                ControllerButton::South.label(ControllerFamily::PlayStation),
                "×"
            );
            assert_eq!(
                ControllerButton::South.label(ControllerFamily::Nintendo),
                "B"
            );
            assert_eq!(
                ControllerButton::South.label(ControllerFamily::JoyConLeft),
                "←"
            );
        }

        {
            let chord = InputBinding::Keyboard(KeyboardBinding::command("k"));
            let mut set = BindingSet::default();
            set.insert_action("search", "Search", vec![chord.clone()]);
            set.insert_action("link", "Insert link", vec![]);
            assert_eq!(
                set.rebind("link", 0, chord, false),
                Err(BindingConflict {
                    action_id: "search".into()
                })
            );
        }
    }
}
