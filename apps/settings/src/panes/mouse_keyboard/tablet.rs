//! Drawing tablets: one card per tablet, holding its pen's mapping, feel
//! and buttons and the pad buttons, rings and strips of the same device.

use super::{
    ArgValue, EraserAction, InputDeviceKind, InputDeviceSnapshot, Item, MouseKeyboardPane, PadDial,
    PadDialAction, ShortcutAction, Spacer, StylusButtonAction, TabletConfig, localized_choices,
    rows, section, setting_row, setting_row_described, t, t_args,
};

/// The Map-to choice for spanning every display.
pub const WHOLE_DESKTOP: &str = "desktop";
/// Pad buttons Settings offers a shortcut for; pads have far fewer.
pub const MAX_PAD_BUTTONS: u32 = 32;
/// The Pad-button choice for a button that does nothing.
pub(super) const PAD_BUTTON_NONE: &str = "none";
pub(super) const PAD_BUTTON_SHOW_MAP: &str = "show-map";
/// Rings and strips Settings offers a mapping for, of each kind.
pub const MAX_PAD_DIALS: u32 = 4;
/// Modes per ring or strip Settings offers a mapping for.
pub const MAX_PAD_MODES: u32 = 8;

/// Resource id of a tablet's card and actions.
pub fn tablet_resource_id(device: &str) -> String {
    format!("settings.tablet:{device}")
}

/// Resource id of a tablet pad's card and actions.
pub fn pad_resource_id(device: &str) -> String {
    format!("settings.tablet-pad:{device}")
}

/// Map-to choices: the whole desktop, then each connected display by the
/// name a person recognises.
pub fn tablet_output_choices() -> Vec<(String, String)> {
    let mut choices = vec![(WHOLE_DESKTOP.to_string(), t("input-tablet-whole-desktop"))];
    if let Some(snapshot) = settings_store::hardware::DisplayHardwareSnapshot::load() {
        for placement in &snapshot.layout {
            let label = snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == placement.connector)
                .map_or_else(
                    || placement.connector.clone(),
                    |output| {
                        output
                            .descriptor
                            .monitor
                            .as_ref()
                            .map(settings_store::hardware::MonitorIdentity::display_name)
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| output.descriptor.label().to_string())
                    },
                );
            choices.push((placement.connector.clone(), label));
        }
    }
    choices
}

/// The saved output for a Map-to choice id: `None` spans the desktop; an
/// error names a display that is not connected.
pub fn tablet_output_for_id(id: &str) -> Result<Option<String>, String> {
    if id == WHOLE_DESKTOP {
        return Ok(None);
    }
    tablet_output_choices()
        .into_iter()
        .any(|(choice, _)| choice == id)
        .then(|| Some(id.to_string()))
        .ok_or_else(|| format!("`{id}` is not a connected display"))
}

pub(super) fn shortcut_id(action: ShortcutAction) -> String {
    let name = serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    format!("shortcut:{name}")
}

pub(super) fn shortcut_for_id(id: &str) -> Option<ShortcutAction> {
    let name = id.strip_prefix("shortcut:")?;
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

pub(super) fn shortcut_choices() -> impl Iterator<Item = (String, String)> {
    crate::panes::keyboard_shortcuts::shortcut_actions()
        .into_iter()
        .map(|(action, label)| (shortcut_id(action), label))
}

/// Stylus button choices as `(id, label key, action)`, before the shortcuts.
pub(super) const STYLUS_BUTTON_CHOICES: [(&str, &str, StylusButtonAction); 7] = [
    (
        "default",
        "input-pen-button-default",
        StylusButtonAction::Default,
    ),
    ("left", "input-pen-button-left", StylusButtonAction::Left),
    (
        "middle",
        "input-pen-button-middle",
        StylusButtonAction::Middle,
    ),
    ("right", "input-pen-button-right", StylusButtonAction::Right),
    ("back", "input-pen-button-back", StylusButtonAction::Back),
    (
        "forward",
        "input-pen-button-forward",
        StylusButtonAction::Forward,
    ),
    (
        "disabled",
        "input-pen-button-disabled",
        StylusButtonAction::Disabled,
    ),
];

/// What a pen button can do: a mouse button, nothing, or any shortcut.
pub fn stylus_button_choices() -> Vec<(String, String)> {
    localized_choices(&STYLUS_BUTTON_CHOICES)
        .into_iter()
        .chain(shortcut_choices())
        .collect()
}

pub fn stylus_button_id(action: StylusButtonAction) -> String {
    match action {
        StylusButtonAction::Shortcut(shortcut) => shortcut_id(shortcut),
        other => STYLUS_BUTTON_CHOICES
            .iter()
            .find(|(_, _, value)| *value == other)
            .map_or("default", |(id, _, _)| id)
            .to_string(),
    }
}

pub fn stylus_button_for_id(id: &str) -> Option<StylusButtonAction> {
    STYLUS_BUTTON_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
        .or_else(|| shortcut_for_id(id).map(StylusButtonAction::Shortcut))
}

/// Eraser choices as `(id, label key, action)`, before the shortcuts.
pub(super) const ERASER_CHOICES: [(&str, &str, EraserAction); 6] = [
    ("native", "input-eraser-native", EraserAction::Native),
    ("left", "input-pen-button-left", EraserAction::Left),
    ("middle", "input-pen-button-middle", EraserAction::Middle),
    ("right", "input-pen-button-right", EraserAction::Right),
    ("back", "input-pen-button-back", EraserAction::Back),
    ("forward", "input-pen-button-forward", EraserAction::Forward),
];

/// What the eraser end can do: erase, click a mouse button, or any shortcut.
pub fn eraser_choices() -> Vec<(String, String)> {
    localized_choices(&ERASER_CHOICES)
        .into_iter()
        .chain(shortcut_choices())
        .collect()
}

pub fn eraser_id(action: EraserAction) -> String {
    match action {
        EraserAction::Shortcut(shortcut) => shortcut_id(shortcut),
        other => ERASER_CHOICES
            .iter()
            .find(|(_, _, value)| *value == other)
            .map_or("native", |(id, _, _)| id)
            .to_string(),
    }
}

pub fn eraser_for_id(id: &str) -> Option<EraserAction> {
    ERASER_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
        .or_else(|| shortcut_for_id(id).map(EraserAction::Shortcut))
}

/// Ring and strip choices as `(id, label key, action)`.
pub(super) const DIAL_CHOICES: [(&str, &str, PadDialAction); 6] = [
    ("nothing", "input-pad-dial-nothing", PadDialAction::Nothing),
    ("scroll", "input-pad-dial-scroll", PadDialAction::Scroll),
    ("zoom", "input-pad-dial-zoom", PadDialAction::Zoom),
    (
        "brush-size",
        "input-pad-dial-brush-size",
        PadDialAction::BrushSize,
    ),
    ("volume", "input-pad-dial-volume", PadDialAction::Volume),
    (
        "brightness",
        "input-pad-dial-brightness",
        PadDialAction::Brightness,
    ),
];

/// What a ring or strip does in apps without tablet support.
pub fn dial_choices() -> Vec<(String, String)> {
    localized_choices(&DIAL_CHOICES)
}

pub fn dial_id(action: PadDialAction) -> &'static str {
    DIAL_CHOICES
        .iter()
        .find(|(_, _, value)| *value == action)
        .map_or("scroll", |(id, _, _)| id)
}

pub fn dial_for_id(id: &str) -> Option<PadDialAction> {
    DIAL_CHOICES
        .iter()
        .find(|(choice, _, _)| *choice == id)
        .map(|(_, _, value)| *value)
}

/// The resource action id of a ring or strip in one mode.
pub fn dial_action_id(dial: PadDial, mode: u32) -> String {
    match dial {
        PadDial::Ring(index) => format!("ring-{index}-mode-{mode}"),
        PadDial::Strip(index) => format!("strip-{index}-mode-{mode}"),
    }
}

/// A pad's rings then strips, each with how many modes Settings offers.
pub fn pad_dials(device: &InputDeviceSnapshot) -> Vec<(PadDial, u32)> {
    let offered = |controls: &[u32]| {
        (0..MAX_PAD_DIALS)
            .zip(controls.iter().copied())
            .map(|(index, modes)| (index, modes.clamp(1, MAX_PAD_MODES)))
            .collect::<Vec<_>>()
    };
    offered(&device.pad_rings)
        .into_iter()
        .map(|(index, modes)| (PadDial::Ring(index), modes))
        .chain(
            offered(&device.pad_strips)
                .into_iter()
                .map(|(index, modes)| (PadDial::Strip(index), modes)),
        )
        .collect()
}

/// What a pad button does in an app without tablet support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadButtonChoice {
    Nothing,
    /// Shows what every control does while held.
    ShowMap,
    Shortcut(ShortcutAction),
}

/// What a pad button can do: nothing, show the pad's map, or any shortcut.
pub fn pad_button_choices() -> Vec<(String, String)> {
    [
        (PAD_BUTTON_NONE.to_string(), t("input-pad-button-none")),
        (
            PAD_BUTTON_SHOW_MAP.to_string(),
            t("input-pad-button-show-map"),
        ),
    ]
    .into_iter()
    .chain(shortcut_choices())
    .collect()
}

/// The choice `config` makes for pad button `button`.
pub fn pad_button_id(config: &TabletConfig, button: u32) -> String {
    if config.pad_map_button == Some(button) {
        return PAD_BUTTON_SHOW_MAP.to_string();
    }
    config.pad_buttons.get(&button).map_or_else(
        || PAD_BUTTON_NONE.to_string(),
        |shortcut| shortcut_id(*shortcut),
    )
}

/// The choice an id names; `None` when it is not one.
pub fn pad_button_for_id(id: &str) -> Option<PadButtonChoice> {
    match id {
        PAD_BUTTON_NONE => Some(PadButtonChoice::Nothing),
        PAD_BUTTON_SHOW_MAP => Some(PadButtonChoice::ShowMap),
        _ => shortcut_for_id(id).map(PadButtonChoice::Shortcut),
    }
}

/// Make pad button `button` do `choice`. One button shows the map: picking
/// it for another moves it there.
pub fn set_pad_button(config: &mut TabletConfig, button: u32, choice: PadButtonChoice) {
    if config.pad_map_button == Some(button) {
        config.pad_map_button = None;
    }
    match choice {
        PadButtonChoice::Nothing => {
            config.pad_buttons.remove(&button);
        }
        PadButtonChoice::ShowMap => {
            config.pad_buttons.remove(&button);
            config.pad_map_button = Some(button);
        }
        PadButtonChoice::Shortcut(shortcut) => {
            config.pad_buttons.insert(button, shortcut);
        }
    }
}

/// "Ring 1", or "Ring 1, mode 2" when its group has several modes.
pub(super) fn dial_label(dial: PadDial, mode: u32, modes: u32) -> String {
    let (key, index) = match dial {
        PadDial::Ring(index) => ("input-pad-ring", index),
        PadDial::Strip(index) => ("input-pad-strip", index),
    };
    let number = ArgValue::from(i64::from(index) + 1);
    if modes > 1 {
        t_args(
            &format!("{key}-mode"),
            &[
                ("number", number),
                ("mode", ArgValue::from(i64::from(mode) + 1)),
            ],
        )
    } else {
        t_args(key, &[("number", number)])
    }
}

/// A tablet or pad device's card title: its name without the kernel's
/// ` Pen` / ` Pad` suffix, which the rows already make clear.
pub(super) fn device_title(device: &InputDeviceSnapshot) -> String {
    let name = device.name.trim();
    let base = name
        .strip_suffix(" Pen")
        .or_else(|| name.strip_suffix(" Pad"))
        .unwrap_or(name);
    match device.kind {
        InputDeviceKind::TabletPad => t_args("input-pad-title", &[("name", ArgValue::from(base))]),
        _ => base.to_string(),
    }
}

/// A drawing tablet as the person knows it: its pen and the button pad
/// that is part of the same device. A pad no tablet claims (a remote) is a
/// device of its own.
pub(super) struct TabletDevice<'a> {
    pen: Option<&'a InputDeviceSnapshot>,
    pad: Option<&'a InputDeviceSnapshot>,
}

impl TabletDevice<'_> {
    /// The tablet's name, without the kernel's "Pen"/"Pad" suffix.
    fn title(&self) -> String {
        self.pen.or(self.pad).map_or_else(String::new, |device| {
            let name = device.name.trim();
            name.strip_suffix(" Pen")
                .or_else(|| name.strip_suffix(" Pad"))
                .unwrap_or(name)
                .to_owned()
        })
    }
}

/// The attached tablets, each with its own pad, then any pad that belongs
/// to no attached tablet.
pub(super) fn tablet_devices<'a>(
    tablets: &'a [InputDeviceSnapshot],
    pads: &'a [InputDeviceSnapshot],
) -> Vec<TabletDevice<'a>> {
    // Each pad goes to the first tablet of its device, at most once.
    let mut unclaimed: Vec<&'a InputDeviceSnapshot> = pads.iter().collect();
    let mut devices: Vec<TabletDevice<'a>> = tablets
        .iter()
        .map(|pen| {
            let pad = unclaimed
                .iter()
                .position(|pad| same_physical_device(pen, pad))
                .map(|at| unclaimed.remove(at));
            TabletDevice {
                pen: Some(pen),
                pad,
            }
        })
        .collect();
    devices.extend(unclaimed.into_iter().map(|pad| TabletDevice {
        pen: None,
        pad: Some(pad),
    }));
    devices
}

/// Whether `a` and `b` are parts of one physical device: libinput's group
/// when the compositor reported it, otherwise the same USB vendor and
/// product.
pub(super) fn same_physical_device(a: &InputDeviceSnapshot, b: &InputDeviceSnapshot) -> bool {
    if !a.physical_device.is_empty() && !b.physical_device.is_empty() {
        return a.physical_device == b.physical_device;
    }
    let ids = |device: &InputDeviceSnapshot| {
        let mut parts = device.key.splitn(3, ':');
        (
            parts.next().map(str::to_owned),
            parts.next().map(str::to_owned),
        )
    };
    ids(a) == ids(b)
}

/// The ring or strip, its index and mode a [`dial_action_id`] names.
pub(super) fn parse_dial_action_id(id: &str) -> Option<(&'static str, u32, u32)> {
    let (dial, rest) = if let Some(rest) = id.strip_prefix("ring-") {
        ("ring", rest)
    } else {
        ("strip", id.strip_prefix("strip-")?)
    };
    let (index, mode) = rest.split_once("-mode-")?;
    Some((dial, index.parse().ok()?, mode.parse().ok()?))
}

impl MouseKeyboardPane {
    pub(super) fn tablet_config(
        &self,
        device: &InputDeviceSnapshot,
    ) -> settings_store::hardware::TabletConfig {
        self.last
            .tablets
            .get(&device.key)
            .cloned()
            .unwrap_or_default()
    }

    /// One card per attached drawing tablet, holding everything that
    /// device has: where the pen maps, how it feels, its pen buttons, and
    /// the pad buttons, rings and strips on the same tablet. Settings are
    /// stored per device identity, so each tablet keeps its own.
    pub(super) fn tablet_cards(&self) -> Vec<crate::widgets::SettingsSection> {
        let outputs = tablet_output_choices();
        tablet_devices(&self.tablets, &self.pads)
            .into_iter()
            .map(|device| {
                let mut items = Vec::new();
                if let Some(pen) = device.pen {
                    items.extend(self.pen_rows(pen, &outputs));
                }
                if let Some(pad) = device.pad {
                    items.extend(self.pad_rows(pad));
                }
                if items.is_empty() {
                    items.push(crate::widgets::note(&t("input-pad-no-buttons")));
                }
                section(&device.title(), rows(items))
            })
            .collect()
    }

    /// A tablet pen's rows: where it maps, how it feels, its buttons.
    #[allow(
        clippy::too_many_lines,
        reason = "one declarative row list mirroring the visible pen settings"
    )]
    pub(super) fn pen_rows(
        &self,
        device: &InputDeviceSnapshot,
        outputs: &[(String, String)],
    ) -> Vec<Item> {
        let config = self.tablet_config(device);
        let resource = tablet_resource_id(&device.key);
        let mapped = config
            .output
            .clone()
            .filter(|output| outputs.iter().any(|(id, _)| id == output))
            .unwrap_or_else(|| WHOLE_DESKTOP.to_string());
        vec![
            setting_row(
                &t("input-tablet-map-to"),
                Box::new(self.device_dropdown(
                    &resource,
                    &device.key,
                    "map-to",
                    outputs.to_vec(),
                    &mapped,
                    &t("input-tablet-map-to"),
                )),
            ),
            setting_row_described(
                &t("input-tablet-keep-aspect"),
                &t("input-tablet-keep-aspect-description"),
                Box::new(self.device_toggle(
                    &resource,
                    &device.key,
                    "keep-aspect",
                    config.keep_aspect,
                )),
            ),
            setting_row_described(
                &t("input-tablet-left-handed"),
                &t("input-tablet-left-handed-description"),
                Box::new(self.device_toggle(
                    &resource,
                    &device.key,
                    "left-handed",
                    config.left_handed,
                )),
            ),
            setting_row_described(
                &t("input-tablet-tip-feel"),
                &t("input-tablet-tip-feel-description"),
                crate::widgets::trailing_slider(Box::new(self.device_slider(
                    &resource,
                    &device.key,
                    "tip-feel",
                    settings_store::hardware::feel_for_pressure_curve(config.pressure_curve),
                ))),
            ),
            setting_row(
                &t("input-tablet-lower-button"),
                Box::new(self.device_dropdown(
                    &resource,
                    &device.key,
                    "lower-button",
                    stylus_button_choices(),
                    &stylus_button_id(config.lower_button),
                    &t("input-tablet-lower-button"),
                )),
            ),
            setting_row(
                &t("input-tablet-upper-button"),
                Box::new(self.device_dropdown(
                    &resource,
                    &device.key,
                    "upper-button",
                    stylus_button_choices(),
                    &stylus_button_id(config.upper_button),
                    &t("input-tablet-upper-button"),
                )),
            ),
            setting_row_described(
                &t("input-tablet-third-button"),
                &t("input-tablet-third-button-description"),
                Box::new(self.device_dropdown(
                    &resource,
                    &device.key,
                    "third-button",
                    stylus_button_choices(),
                    &stylus_button_id(config.third_button),
                    &t("input-tablet-third-button"),
                )),
            ),
            setting_row_described(
                &t("input-tablet-eraser"),
                &t("input-tablet-eraser-description"),
                Box::new(self.device_dropdown(
                    &resource,
                    &device.key,
                    "eraser",
                    eraser_choices(),
                    &eraser_id(config.eraser),
                    &t("input-tablet-eraser"),
                )),
            ),
        ]
    }

    /// A tablet pad's rows: a shortcut for each of its buttons, then what
    /// each ring and strip does in each mode. Tablet-aware apps get the
    /// unmapped buttons, rings and strips themselves.
    pub(super) fn pad_rows(&self, device: &InputDeviceSnapshot) -> Vec<Item> {
        let config = self.tablet_config(device);
        let resource = pad_resource_id(&device.key);
        let mut items: Vec<Item> = (0..device.pad_buttons.min(MAX_PAD_BUTTONS))
            .map(|button| {
                let label = t_args(
                    "input-pad-button",
                    &[("number", ArgValue::from(i64::from(button) + 1))],
                );
                if device.pad_mode_buttons.contains(&button) {
                    return setting_row_described(
                        &label,
                        &t("input-pad-mode-button-description"),
                        Box::new(Spacer::new()),
                    );
                }
                setting_row(
                    &label,
                    Box::new(self.device_dropdown(
                        &resource,
                        &device.key,
                        &format!("button-{button}"),
                        pad_button_choices(),
                        &pad_button_id(&config, button),
                        &label,
                    )),
                )
            })
            .collect();
        for (dial, modes) in pad_dials(device) {
            for mode in 0..modes {
                let label = dial_label(dial, mode, modes);
                items.push(setting_row(
                    &label,
                    Box::new(self.device_dropdown(
                        &resource,
                        &device.key,
                        &dial_action_id(dial, mode),
                        dial_choices(),
                        dial_id(config.dial_action(dial, mode)),
                        &label,
                    )),
                ));
            }
        }
        if !pad_dials(device).is_empty() {
            items.push(crate::widgets::note(&t("input-pad-dials-caption")));
        }
        items
    }
}
