//! Drawing tablets: one card per tablet, holding its pen's mapping, feel
//! and buttons and the pad buttons, rings and strips of the same device.

use settings_store::hardware::{
    EraserAction, InputDeviceKind, InputDeviceSnapshot, PadDial, PadDialAction, StylusButtonAction,
    TabletConfig,
};
use ui::framework::{Actions, ViewCx};
use ui::i18n::ArgValue;
use ui::tilo_resource;
use ui::widget::{Dropdown, Slider, Spacer, Toggle};

use super::{
    Item, id_dropdown, localized_choices, rows, section, setting_row, setting_row_described,
};
use crate::effects::effect;
use crate::i18n::{t, t_args};
use crate::prefs::InputDevicePrefs;
use crate::prefs::input_device_prefs as verbs;
use crate::resource::Settings;

use crate::prefs::tablet_choices::{
    MAX_PAD_BUTTONS, MAX_PAD_DIALS, MAX_PAD_MODES, PAD_BUTTON_NONE, PAD_BUTTON_SHOW_MAP,
    WHOLE_DESKTOP, dial_id, eraser_id, pad_button_id, shortcut_id, stylus_button_id,
};

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
    pub(super) pen: Option<&'a InputDeviceSnapshot>,
    pub(super) pad: Option<&'a InputDeviceSnapshot>,
}

impl TabletDevice<'_> {
    /// The tablet's name, without the kernel's "Pen"/"Pad" suffix.
    pub(super) fn title(&self) -> String {
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
#[cfg(test)]
pub(super) fn parse_dial_action_id(id: &str) -> Option<(&'static str, u32, u32)> {
    let (dial, rest) = if let Some(rest) = id.strip_prefix("ring-") {
        ("ring", rest)
    } else {
        ("strip", id.strip_prefix("strip-")?)
    };
    let (index, mode) = rest.split_once("-mode-")?;
    Some((dial, index.parse().ok()?, mode.parse().ok()?))
}

/// One card per attached drawing tablet, holding everything that device
/// has: where the pen maps, how it feels, its pen buttons, and the pad
/// buttons, rings and strips on the same tablet. Settings are stored per
/// device identity, so each tablet keeps its own.
pub(super) fn tablet_cards(
    cx: &ViewCx<InputDevicePrefs>,
    actions: &Actions<Settings>,
    tablets: &[InputDeviceSnapshot],
    pads: &[InputDeviceSnapshot],
    configs: &std::collections::BTreeMap<String, TabletConfig>,
    outputs: &[(String, String)],
) -> Vec<provider_settings_ui::widgets::SettingsSection> {
    let config =
        |snapshot: &InputDeviceSnapshot| configs.get(&snapshot.key).cloned().unwrap_or_default();
    tablet_devices(tablets, pads)
        .into_iter()
        .map(|tablet| {
            let mut items = Vec::new();
            if let Some(pen) = tablet.pen {
                items.extend(pen_rows(cx, actions, pen, &config(pen), outputs));
            }
            if let Some(pad) = tablet.pad {
                items.extend(pad_rows(cx, pad, &config(pad)));
            }
            if items.is_empty() {
                items.push(provider_settings_ui::widgets::note(&t(
                    "input-pad-no-buttons",
                )));
            }
            section(&tablet.title(), rows(items))
        })
        .collect()
}

/// A dropdown over stable ids for one of a tablet's or pad's choices, its
/// id the device's resource and the choice: `settings.tablet:<key>.eraser`.
fn device_dropdown(
    resource: &str,
    action: &str,
    choices: Vec<(String, String)>,
    selected: &str,
    accessible_name: &str,
) -> Dropdown {
    id_dropdown(
        choices,
        selected,
        &format!("{resource}.{action}"),
        accessible_name,
    )
}

/// A tablet pen's rows: where it maps, how it feels, its buttons.
#[allow(
    clippy::too_many_lines,
    reason = "one declarative row list mirroring the visible pen settings"
)]
fn pen_rows(
    cx: &ViewCx<InputDevicePrefs>,
    actions: &Actions<Settings>,
    device: &InputDeviceSnapshot,
    config: &TabletConfig,
    outputs: &[(String, String)],
) -> Vec<Item> {
    let resource = tablet_resource_id(&device.key);
    let verbs = cx.actions();
    let mapped = config
        .output
        .clone()
        .filter(|output| outputs.iter().any(|(id, _)| id == output))
        .unwrap_or_else(|| WHOLE_DESKTOP.to_string());
    let key = device.key.clone();
    let button = |which: &'static str, selected: StylusButtonAction, label: &str| {
        let key = key.clone();
        verbs.bind(
            device_dropdown(
                &resource,
                &format!("{which}-button"),
                stylus_button_choices(),
                &stylus_button_id(selected),
                label,
            ),
            InputDevicePrefs::SET_TABLET_BUTTON
                .map(move |choice: String| (key.clone(), which.to_owned(), choice)),
        )
    };
    let toggle = |action: &str, on: bool| {
        Toggle::from_state(&ui::LocalState::new(on)).with_id(&format!("{resource}.{action}"))
    };
    vec![
        setting_row(
            &t("input-tablet-map-to"),
            Box::new({
                let key = key.clone();
                actions.bind_effect(
                    device_dropdown(
                        &resource,
                        "map-to",
                        outputs.to_vec(),
                        &mapped,
                        &t("input-tablet-map-to"),
                    ),
                    effect(super::InputEffect::SET_TABLET_OUTPUT)
                        .map(move |output: String| (key.clone(), output)),
                )
            }),
        ),
        setting_row_described(
            &t("input-tablet-keep-aspect"),
            &t("input-tablet-keep-aspect-description"),
            Box::new({
                let key = key.clone();
                verbs.bind(
                    toggle("keep-aspect", config.keep_aspect),
                    InputDevicePrefs::SET_TABLET_KEEP_ASPECT.map(move |on: bool| (key.clone(), on)),
                )
            }),
        ),
        setting_row_described(
            &t("input-tablet-left-handed"),
            &t("input-tablet-left-handed-description"),
            Box::new({
                let key = key.clone();
                verbs.bind(
                    toggle("left-handed", config.left_handed),
                    InputDevicePrefs::SET_TABLET_LEFT_HANDED.map(move |on: bool| (key.clone(), on)),
                )
            }),
        ),
        setting_row_described(
            &t("input-tablet-tip-feel"),
            &t("input-tablet-tip-feel-description"),
            provider_settings_ui::widgets::trailing_slider(Box::new({
                let key = key.clone();
                verbs.bind(
                    Slider::from_state(&ui::LocalState::new(
                        settings_store::hardware::feel_for_pressure_curve(config.pressure_curve),
                    ))
                    .with_id(&format!("{resource}.tip-feel")),
                    InputDevicePrefs::SET_TABLET_TIP_FEEL.map(move |feel: f64| (key.clone(), feel)),
                )
            })),
        ),
        setting_row(
            &t("input-tablet-lower-button"),
            Box::new(button(
                "lower",
                config.lower_button,
                &t("input-tablet-lower-button"),
            )),
        ),
        setting_row(
            &t("input-tablet-upper-button"),
            Box::new(button(
                "upper",
                config.upper_button,
                &t("input-tablet-upper-button"),
            )),
        ),
        setting_row_described(
            &t("input-tablet-third-button"),
            &t("input-tablet-third-button-description"),
            Box::new(button(
                "third",
                config.third_button,
                &t("input-tablet-third-button"),
            )),
        ),
        setting_row_described(
            &t("input-tablet-eraser"),
            &t("input-tablet-eraser-description"),
            Box::new({
                let key = key.clone();
                verbs.bind(
                    device_dropdown(
                        &resource,
                        "eraser",
                        eraser_choices(),
                        &eraser_id(config.eraser),
                        &t("input-tablet-eraser"),
                    ),
                    InputDevicePrefs::SET_TABLET_ERASER
                        .map(move |choice: String| (key.clone(), choice)),
                )
            }),
        ),
    ]
}

/// A tablet pad's rows: a shortcut for each of its buttons, then what
/// each ring and strip does in each mode. Tablet-aware apps get the
/// unmapped buttons, rings and strips themselves.
fn pad_rows(
    cx: &ViewCx<InputDevicePrefs>,
    device: &InputDeviceSnapshot,
    config: &TabletConfig,
) -> Vec<Item> {
    let resource = pad_resource_id(&device.key);
    let verbs = cx.actions();
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
            let key = device.key.clone();
            setting_row(
                &label,
                Box::new(
                    verbs.bind(
                        device_dropdown(
                            &resource,
                            &format!("button-{button}"),
                            pad_button_choices(),
                            &pad_button_id(config, button),
                            &label,
                        ),
                        InputDevicePrefs::SET_PAD_BUTTON
                            .map(move |choice: String| (key.clone(), button, choice)),
                    ),
                ),
            )
        })
        .collect();
    for (dial, modes) in pad_dials(device) {
        for mode in 0..modes {
            let label = dial_label(dial, mode, modes);
            let (kind, index) = match dial {
                PadDial::Ring(index) => ("ring", index),
                PadDial::Strip(index) => ("strip", index),
            };
            let key = device.key.clone();
            items.push(setting_row(
                &label,
                Box::new(verbs.bind(
                    device_dropdown(
                        &resource,
                        &dial_action_id(dial, mode),
                        dial_choices(),
                        dial_id(config.dial_action(dial, mode)),
                        &label,
                    ),
                    InputDevicePrefs::SET_PAD_DIAL.map(move |choice: String| {
                        (key.clone(), kind.to_owned(), index, mode, choice)
                    }),
                )),
            ));
        }
    }
    if !pad_dials(device).is_empty() {
        items.push(provider_settings_ui::widgets::note(&t(
            "input-pad-dials-caption",
        )));
    }
    items
}

/// What a tablet's live row offers, bound to the tablet `device`: its
/// aspect, hand, feel, pen buttons and eraser.
pub(super) fn pen_operations(
    device: &str,
) -> Vec<Option<tilo_resource::ActionDecl<InputDevicePrefs>>> {
    let key = device.to_owned();
    vec![
        InputDevicePrefs::SET_TABLET_KEEP_ASPECT
            .bind(verbs::SetTabletKeepAspect::paths().device, key.clone())
            .declaration(
                "set-tablet-keep-aspect",
                "Keep the tablet's aspect ratio, or stretch it",
            ),
        InputDevicePrefs::SET_TABLET_LEFT_HANDED
            .bind(verbs::SetTabletLeftHanded::paths().device, key.clone())
            .declaration(
                "set-tablet-left-handed",
                "Turn the tablet around for the left hand, or back",
            ),
        InputDevicePrefs::SET_TABLET_TIP_FEEL
            .bind(verbs::SetTabletTipFeel::paths().device, key.clone())
            .declaration("set-tablet-tip-feel", "Set how firmly the pen must press"),
        InputDevicePrefs::SET_TABLET_BUTTON
            .bind(verbs::SetTabletButton::paths().device, key.clone())
            .declaration(
                "set-tablet-button",
                "Set what one of the pen's buttons does",
            ),
        InputDevicePrefs::SET_TABLET_ERASER
            .bind(verbs::SetTabletEraser::paths().device, key)
            .declaration("set-tablet-eraser", "Set what the pen's eraser end does"),
    ]
}

/// What a pad's live row offers, bound to the pad `device`: its buttons,
/// rings and strips.
pub(super) fn pad_operations(
    device: &str,
) -> Vec<Option<tilo_resource::ActionDecl<InputDevicePrefs>>> {
    vec![
        InputDevicePrefs::SET_PAD_BUTTON
            .bind(verbs::SetPadButton::paths().device, device.to_owned())
            .declaration("set-pad-button", "Set what a pad button does"),
        InputDevicePrefs::SET_PAD_DIAL
            .bind(verbs::SetPadDial::paths().device, device.to_owned())
            .declaration("set-pad-dial", "Set what a ring or strip does in one mode"),
    ]
}
