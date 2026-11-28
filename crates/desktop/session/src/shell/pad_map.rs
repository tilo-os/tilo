//! A drawing tablet's pad map, as the shell that draws it reads it: while the person holds the
//! pad's map button (the one its settings pick) or one of its mode buttons, a card says what each
//! button, ring and strip does now, arranged around a drawing of the pad where its model's
//! arrangement is known, else as a list. Like the hardware keys' readout it takes no input and no
//! focus.
//!
//! What a control does is, first, what never reaches an app: the map button itself and a button
//! mapped to a desktop shortcut. Then the label the focused tablet-aware app gave it for its
//! current mode (`zwp_tablet_pad_v2.set_feedback`), and then Tilo's own mapping: a mode button
//! steps its group's mode, a ring or strip scrolls, zooms, sizes the brush or steps the volume or
//! brightness in the mode its group is in. A control the focused app takes without naming is the
//! app's to decide.
//!
//! The compositor says which pad is held and what its app said ([`tilo_compositor::PadMap`]); the
//! session reads the pad's settings and the model's arrangement and publishes the words as
//! `device.pad_map`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Map, Value};
use settings_store::hardware::{PadDial, PadDialAction, TabletConfig};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_compositor::{TabletPadControl as Control, TabletPadGroupDescriptor};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

use crate::i18n::{t, t_args};

pub(crate) use tilo_device_surfaces::pad_map::{PadMap as Reading, PadMapControl};

/// The resource the pad map is published as.
pub(crate) const RESOURCE: &str = "session:pad-map";

/// Where libwacom keeps its tablet database: the system copy, then local overrides, which win.
const LIBWACOM_DIRS: [&str; 2] = ["/usr/share/libwacom", "/etc/libwacom"];

/// The pad map as a view of another language reads it (`device.pad_map`).
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Pad map")]
pub(crate) struct PadMapRoot;

/// An edge of a pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];
}

/// The controls along each edge of a pad, top to bottom or left to right, as libwacom places them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Layout {
    left: Vec<Control>,
    right: Vec<Control>,
    top: Vec<Control>,
    bottom: Vec<Control>,
}

impl Layout {
    fn side(&self, side: Side) -> &[Control] {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
            Side::Top => &self.top,
            Side::Bottom => &self.bottom,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut Vec<Control> {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
            Side::Top => &mut self.top,
            Side::Bottom => &mut self.bottom,
        }
    }

    fn side_of(&self, control: Control) -> Option<Side> {
        Side::ALL
            .into_iter()
            .find(|side| self.side(*side).contains(&control))
    }
}

/// What one control does right now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Does {
    /// The focused tablet-aware app's own word for it.
    App(String),
    /// A desktop shortcut, by its name.
    Shortcut(String),
    /// What a ring or strip does in its group's mode.
    Dial(PadDialAction),
    /// A mode button: its group is in `mode` (from zero) of `modes`.
    ModeSwitch {
        mode: u32,
        modes: u32,
    },
    /// The button that shows this map.
    ShowsMap,
    /// The focused app takes it, and has not said what it does.
    AppDecides,
    Nothing,
}

impl Does {
    fn words(&self) -> String {
        match self {
            Self::App(label) | Self::Shortcut(label) => label.clone(),
            Self::Dial(action) => t(match action {
                PadDialAction::Nothing => "pad-map-nothing",
                PadDialAction::Scroll => "pad-map-dial-scroll",
                PadDialAction::Zoom => "pad-map-dial-zoom",
                PadDialAction::BrushSize => "pad-map-dial-brush-size",
                PadDialAction::Volume => "pad-map-dial-volume",
                PadDialAction::Brightness => "pad-map-dial-brightness",
            }),
            Self::ModeSwitch { mode, modes } => t_args(
                "pad-map-mode-switch",
                &[
                    ("mode", i64::from(*mode).saturating_add(1).into()),
                    ("modes", i64::from(*modes).into()),
                ],
            ),
            Self::ShowsMap => t("pad-map-shows-map"),
            Self::AppDecides => t("pad-map-app-decides"),
            Self::Nothing => t("pad-map-nothing"),
        }
    }

    /// Whether the control does something worth reading first.
    const fn active(&self) -> bool {
        !matches!(
            self,
            Self::Nothing | Self::AppDecides | Self::Dial(PadDialAction::Nothing)
        )
    }
}

/// What the control is called: "Button 1", "Ring 1", "Strip 1".
fn control_name(control: Control) -> String {
    let (key, index) = match control {
        Control::Button(index) => ("pad-map-button", index),
        Control::Ring(index) => ("pad-map-ring", index),
        Control::Strip(index) => ("pad-map-strip", index),
    };
    t_args(
        key,
        &[("number", i64::from(index).saturating_add(1).into())],
    )
}

/// What the control's keycap reads: a button's number, a dial's name.
fn control_cap(control: Control) -> String {
    match control {
        Control::Button(index) => index.saturating_add(1).to_string(),
        Control::Ring(_) | Control::Strip(_) => control_name(control),
    }
}

/// What each control of the held pad does, buttons first, then rings, then strips. `config` is
/// the pad's tablet's settings, when it has any.
fn controls(map: &tilo_compositor::PadMap, config: Option<&TabletConfig>) -> Vec<(Control, Does)> {
    let pad = &map.pad;
    let label = |control| {
        map.labels
            .iter()
            .find(|(labelled, _)| *labelled == control)
            .map(|(_, label)| label.clone())
    };
    let group_of = |control: Control| {
        pad.groups.iter().position(|group| match control {
            Control::Button(button) => group.buttons.contains(&button),
            Control::Ring(ring) => group.rings.contains(&ring),
            Control::Strip(strip) => group.strips.contains(&strip),
        })
    };
    let mode_of = |control| {
        group_of(control)
            .and_then(|group| map.modes.get(group))
            .copied()
            .unwrap_or(0)
    };
    // libinput's mode buttons are in no group's buttons; one steps every group's mode.
    let modes = pad
        .groups
        .iter()
        .map(|group| group.modes)
        .max()
        .unwrap_or(1)
        .max(1);
    let mut entries = Vec::new();
    for button in 0..pad.buttons {
        let control = Control::Button(button);
        // A mapped shortcut and the map button never reach the app, so they come first; then
        // the app's word; then what the button does on its own.
        let does = if config.and_then(|config| config.pad_map_button) == Some(button) {
            Does::ShowsMap
        } else if let Some(shortcut) = config.and_then(|config| config.pad_buttons.get(&button)) {
            Does::Shortcut(tilo_sheets::word(&shortcut.label_key()))
        } else if let Some(label) = label(control) {
            Does::App(label)
        } else if !pad.groups.is_empty() && group_of(control).is_none() {
            Does::ModeSwitch {
                mode: map.modes.first().copied().unwrap_or(0),
                modes,
            }
        } else if map.app_focused {
            Does::AppDecides
        } else {
            Does::Nothing
        };
        entries.push((control, does));
    }
    let mut dials = |pick: fn(&TabletPadGroupDescriptor) -> &[u32],
                     make: fn(u32) -> Control,
                     dial: fn(u32) -> PadDial| {
        let mut all: Vec<u32> = pad
            .groups
            .iter()
            .flat_map(|group| pick(group).iter().copied())
            .collect();
        all.sort_unstable();
        all.dedup();
        for index in all {
            let control = make(index);
            let does = if let Some(label) = label(control) {
                Does::App(label)
            } else if map.app_focused {
                Does::AppDecides
            } else {
                Does::Dial(config.map_or_else(PadDialAction::default, |config| {
                    config.dial_action(dial(index), mode_of(control))
                }))
            };
            entries.push((control, does));
        }
    };
    dials(|group| &group.rings, Control::Ring, PadDial::Ring);
    dials(|group| &group.strips, Control::Strip, PadDial::Strip);
    entries
}

/// The settings of the tablet the held pad belongs to, from `tablets` (by device key): the pad's
/// own, else its model's, as the compositor finds them for its buttons.
fn tablet_config<'a>(
    map: &tilo_compositor::PadMap,
    tablets: &'a BTreeMap<String, TabletConfig>,
) -> Option<&'a TabletConfig> {
    let (vendor, product) = map.tablet.usb_id?;
    let key = settings_store::hardware::input_device_key(vendor, product, &map.pad.name);
    let model = format!("{vendor:04x}:{product:04x}:");
    tablets.get(&key).or_else(|| {
        tablets
            .iter()
            .find(|(known, _)| known.starts_with(&model))
            .map(|(_, config)| config)
    })
}

/// The map `map` stands as, with its tablet's settings `config` and its model's arrangement
/// `layout`.
pub(crate) fn reading_of(
    map: &tilo_compositor::PadMap,
    config: Option<&TabletConfig>,
    layout: Option<&Layout>,
) -> Reading {
    let entries = controls(map, config);
    let spoken = entries
        .iter()
        .map(|(control, does)| {
            t_args(
                "pad-map-entry",
                &[
                    ("control", control_name(*control).into()),
                    ("action", does.words().into()),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let shown = |control: Control, does: &Does| PadMapControl {
        name: control_name(control),
        cap: control_cap(control),
        does: does.words(),
        active: does.active(),
    };
    let along = |side: Side| -> Vec<PadMapControl> {
        let Some(layout) = layout else {
            return Vec::new();
        };
        layout
            .side(side)
            .iter()
            .filter_map(|control| {
                entries
                    .iter()
                    .find(|(listed, _)| listed == control)
                    .map(|(control, does)| shown(*control, does))
            })
            .collect()
    };
    Reading {
        visible: true,
        name: map.pad.name.clone(),
        hint: t("pad-map-hint"),
        drawn: layout.is_some(),
        controls: entries
            .iter()
            .map(|(control, does)| shown(*control, does))
            .collect(),
        top: along(Side::Top),
        left: along(Side::Left),
        right: along(Side::Right),
        bottom: along(Side::Bottom),
        spoken,
    }
}

/// The layout of the pad with USB ids `vendor:product` and the given control counts, from the
/// first matching `.tablet` file in `dirs` (later directories override earlier ones). `None` when
/// no file describes the model or its placement misses a button libinput reports.
fn find_layout(
    dirs: &[PathBuf],
    (vendor, product): (u32, u32),
    buttons: u32,
    rings: u32,
    strips: u32,
) -> Option<Layout> {
    dirs.iter().rev().find_map(|dir| {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "tablet"))
            .collect();
        files.sort();
        files.iter().find_map(|file| {
            let text = read_small(file)?;
            if !matches_device(&text, vendor, product) {
                return None;
            }
            parse_layout(&text, buttons, rings, strips)
        })
    })
}

/// A `.tablet` file's text; they are a few kilobytes, so anything much larger is not one.
fn read_small(path: &Path) -> Option<String> {
    const LIMIT: u64 = 64 * 1024;
    if std::fs::metadata(path).ok()?.len() > LIMIT {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The `key=value` entries of `section`.
fn entries<'a>(text: &'a str, section: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
    let mut current = "";
    text.lines().filter_map(move |line| {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            return None;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
            return None;
        }
        if current != section {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        Some((key.trim(), value.trim()))
    })
}

fn value<'a>(text: &'a str, section: &'a str, key: &str) -> Option<&'a str> {
    entries(text, section).find_map(|(k, v)| (k == key).then_some(v))
}

/// Whether the file's `DeviceMatch` names a USB or Bluetooth device with these ids.
fn matches_device(text: &str, vendor: u32, product: u32) -> bool {
    let Some(matches) = value(text, "Device", "DeviceMatch") else {
        return false;
    };
    matches.split(';').any(|entry| {
        let mut parts = entry.split('|');
        let bus = parts.next().unwrap_or_default();
        let id = |part: Option<&str>| part.and_then(|p| u32::from_str_radix(p, 16).ok());
        matches!(bus, "usb" | "bluetooth")
            && id(parts.next()) == Some(vendor)
            && id(parts.next()) == Some(product)
    })
}

/// A libwacom button letter; `A` is libinput's button 0.
fn button_of(letter: &str) -> Option<u32> {
    let mut chars = letter.trim().chars();
    let first = chars.next()?;
    if chars.next().is_some() || !first.is_ascii_uppercase() {
        return None;
    }
    u32::from(first).checked_sub(u32::from('A'))
}

/// The layout a `.tablet` file describes for a pad with these counts.
fn parse_layout(text: &str, buttons: u32, rings: u32, strips: u32) -> Option<Layout> {
    let mut layout = Layout::default();
    for (side, key) in [
        (Side::Left, "Left"),
        (Side::Right, "Right"),
        (Side::Top, "Top"),
        (Side::Bottom, "Bottom"),
    ] {
        if let Some(list) = value(text, "Buttons", key) {
            layout.side_mut(side).extend(
                list.split(';')
                    .filter_map(button_of)
                    .filter(|button| *button < buttons)
                    .map(Control::Button),
            );
        }
    }
    let placed = |layout: &Layout, button| layout.side_of(Control::Button(button)).is_some();
    if buttons == 0 || !(0..buttons).all(|button| placed(&layout, button)) {
        return None;
    }
    // A ring sits in the middle of the edge its mode button is on, with the mode button at its
    // centre; a strip heads its edge.
    for ring in 0..rings {
        let key = if ring == 0 { "Ring" } else { "Ring2" };
        let mode_button = value(text, "Buttons", key).and_then(button_of);
        place_dial(&mut layout, Control::Ring(ring), mode_button, ring, true);
    }
    for strip in 0..strips {
        let key = if strip == 0 {
            "Touchstrip"
        } else {
            "Touchstrip2"
        };
        let mode_button = value(text, "Buttons", key).and_then(button_of);
        place_dial(
            &mut layout,
            Control::Strip(strip),
            mode_button,
            strip,
            false,
        );
    }
    Some(layout)
}

fn place_dial(
    layout: &mut Layout,
    dial: Control,
    mode_button: Option<u32>,
    index: u32,
    centred: bool,
) {
    let fallback = if index.is_multiple_of(2) {
        Side::Left
    } else {
        Side::Right
    };
    let mode_button = mode_button.map(Control::Button);
    let side = mode_button
        .and_then(|button| layout.side_of(button))
        .unwrap_or(fallback);
    let controls = layout.side_mut(side);
    if let Some(button) = mode_button {
        controls.retain(|control| *control != button);
    }
    let at = if centred { controls.len() / 2 } else { 0 };
    let mut group = vec![dial];
    group.extend(mode_button);
    controls.splice(at..at, group);
}

/// A pad model as its arrangement is looked up: its USB ids and how many buttons, rings and
/// strips libinput reports.
type Model = ((u32, u32), u32, u32, u32);

/// The pad map's state between passes: the tablets' settings, the arrangement of each model
/// looked up once, and the map as it stands.
#[derive(Debug, Default)]
pub(crate) struct Held {
    tablets: BTreeMap<String, TabletConfig>,
    /// Each pad model's arrangement, by its USB ids and control counts, looked up once.
    layouts: BTreeMap<Model, Option<Layout>>,
    /// Where the arrangements are read: libwacom's database, unless said.
    dirs: Option<Vec<PathBuf>>,
    reading: Reading,
}

/// The pad map, shared between the pass that follows the compositor and the service that
/// publishes it.
pub(crate) type Shared = Rc<RefCell<Held>>;

impl Held {
    pub(crate) fn new(tablets: BTreeMap<String, TabletConfig>) -> Self {
        Self {
            tablets,
            ..Self::default()
        }
    }

    /// The tablets' settings as the person changed them.
    pub(crate) fn set_tablets(&mut self, tablets: BTreeMap<String, TabletConfig>) {
        self.tablets = tablets;
    }

    /// Read the arrangements of pad models from `dirs` rather than libwacom's database.
    #[cfg(test)]
    pub(crate) fn read_layouts_from(&mut self, dirs: Vec<PathBuf>) {
        self.dirs = Some(dirs);
        self.layouts.clear();
    }

    /// The map as the compositor's held pad `map` stands now, or nothing held.
    pub(crate) fn follow(&mut self, map: Option<&tilo_compositor::PadMap>) {
        let Some(map) = map else {
            self.reading = Reading::default();
            return;
        };
        let layout = map.tablet.usb_id.and_then(|ids| {
            let counts = |pick: fn(&TabletPadGroupDescriptor) -> &[u32]| {
                let mut all: Vec<u32> = map
                    .pad
                    .groups
                    .iter()
                    .flat_map(|group| pick(group).iter().copied())
                    .collect();
                all.sort_unstable();
                all.dedup();
                u32::try_from(all.len()).unwrap_or(u32::MAX)
            };
            let model = (
                ids,
                map.pad.buttons,
                counts(|group| &group.rings),
                counts(|group| &group.strips),
            );
            let dirs = self
                .dirs
                .clone()
                .unwrap_or_else(|| LIBWACOM_DIRS.iter().map(PathBuf::from).collect());
            self.layouts
                .entry(model)
                .or_insert_with(|| find_layout(&dirs, ids, model.1, model.2, model.3))
                .clone()
        });
        self.reading = reading_of(map, tablet_config(map, &self.tablets), layout.as_ref());
    }
}

/// The pad map as the session publishes it.
pub(crate) struct Served {
    held: Shared,
    published: Option<Reading>,
}

impl Served {
    pub(crate) fn new(held: Shared) -> Self {
        Self {
            held,
            published: None,
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<NoEffect>(
            tilo_resource::LiveRow::typed::<PadMapRoot>(RESOURCE, t("session-pad-map"))
                .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.published.clone().unwrap_or_default()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(ActionAddress::new(resource, action)))
    }

    fn poll(&mut self) -> bool {
        let now = self.held.borrow().reading.clone();
        if self.published.as_ref() == Some(&now) {
            return false;
        }
        self.published = Some(now);
        true
    }
}

impl crate::Session {
    /// Follow the pad whose map the person holds up, every pass.
    pub(crate) fn follow_pad_map(&self) {
        let map = self.compositor.pad_map();
        self.shared.pad_map.borrow_mut().follow(map.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_compositor::{TabletDescriptor, TabletPadDescriptor};

    const INTUOS_PRO: &str = "\
[Device]
Name=Wacom Intuos Pro M
DeviceMatch=usb|056a|0357;bluetooth|056a|0360;
[Features]
NumRings=1
[Buttons]
Left=A;B;C;D;E;F;G;H;I
Ring=I
RingNumModes=4
";

    /// A pad of four buttons: 0 to 2 in its one group with a ring and a strip, button 3 its mode
    /// button.
    fn held(
        app_focused: bool,
        labels: Vec<(Control, String)>,
        modes: Vec<u32>,
    ) -> tilo_compositor::PadMap {
        tilo_compositor::PadMap {
            pad: TabletPadDescriptor {
                name: "Test tablet Pad".into(),
                syspath: None,
                buttons: 4,
                groups: vec![TabletPadGroupDescriptor {
                    buttons: vec![0, 1, 2],
                    rings: vec![0],
                    strips: vec![0],
                    modes: 2,
                }],
            },
            tablet: TabletDescriptor {
                name: "Test tablet".into(),
                usb_id: Some((0x056a, 0x0357)),
                syspath: None,
            },
            modes,
            labels,
            app_focused,
        }
    }

    fn does(reading: &Reading, name: &str) -> String {
        reading
            .controls
            .iter()
            .find(|control| control.name == name)
            .map_or_else(
                || panic!("no {name} in {reading:?}"),
                |control| control.does.clone(),
            )
    }

    #[test]
    fn the_map_shows_the_focused_apps_labels_where_nothing_outranks_them() {
        let mut config = TabletConfig::default();
        config
            .pad_buttons
            .insert(2, settings_store::hardware::ShortcutAction::Search);
        let map = held(
            true,
            vec![
                (Control::Button(0), "Undo".into()),
                (Control::Button(2), "Never shown".into()),
                (Control::Ring(0), "Brush size".into()),
            ],
            vec![0],
        );
        let reading = reading_of(&map, Some(&config), None);
        assert!(reading.visible);
        assert_eq!(reading.name, "Test tablet Pad");
        assert_eq!(does(&reading, "Button 1"), "Undo");
        assert_eq!(does(&reading, "Ring 1"), "Brush size");
        assert_eq!(
            does(&reading, "Button 2"),
            "Up to the app",
            "a control the app takes and did not name is its to decide"
        );
        assert_eq!(
            does(&reading, "Button 3"),
            "Search everywhere",
            "a mapped shortcut never reaches the app, whatever it says"
        );
        assert_eq!(does(&reading, "Strip 1"), "Up to the app");
        assert!(
            reading.spoken.contains("Button 1: Undo"),
            "{}",
            reading.spoken
        );
        assert!(!reading.drawn, "no arrangement was given: listed");
        assert!(
            [&reading.top, &reading.left, &reading.right, &reading.bottom]
                .iter()
                .all(|edge| edge.is_empty())
        );
    }

    #[test]
    fn the_map_falls_back_to_tilos_mapping() {
        let mut config = TabletConfig {
            pad_map_button: Some(1),
            ..TabletConfig::default()
        };
        config.pad_buttons.insert(
            0,
            settings_store::hardware::ShortcutAction::WorkspaceOverview,
        );
        config.set_dial_action(PadDial::Ring(0), 0, PadDialAction::Zoom);
        config.set_dial_action(PadDial::Ring(0), 1, PadDialAction::Volume);

        let reading = reading_of(&held(false, Vec::new(), vec![0]), Some(&config), None);
        assert_eq!(does(&reading, "Button 2"), "Shows this map");
        assert_eq!(
            does(&reading, "Button 3"),
            "Does nothing",
            "an app without tablet support gets nothing from an unmapped button"
        );
        assert_eq!(does(&reading, "Button 4"), "Next mode (now 1 of 2)");
        assert_eq!(does(&reading, "Ring 1"), "Zoom");
        assert_eq!(does(&reading, "Strip 1"), "Scroll");
        let active: Vec<&str> = reading
            .controls
            .iter()
            .filter(|control| control.active)
            .map(|control| control.name.as_str())
            .collect();
        assert_eq!(
            active,
            ["Button 1", "Button 2", "Button 4", "Ring 1", "Strip 1"],
            "what does nothing is not read first"
        );
        assert!(
            reading.spoken.contains("Ring 1: Zoom"),
            "{}",
            reading.spoken
        );

        // The ring's action follows its group's mode.
        let reading = reading_of(&held(false, Vec::new(), vec![1]), Some(&config), None);
        assert_eq!(does(&reading, "Ring 1"), "Volume");
        assert_eq!(does(&reading, "Button 4"), "Next mode (now 2 of 2)");

        // A pad with no settings scrolls with its dials and does nothing with its buttons.
        let reading = reading_of(&held(false, Vec::new(), vec![0]), None, None);
        assert_eq!(does(&reading, "Button 1"), "Does nothing");
        assert_eq!(does(&reading, "Ring 1"), "Scroll");
    }

    #[test]
    fn a_tablets_settings_are_found_by_its_pad_else_by_its_model() {
        let map = held(false, Vec::new(), vec![0]);
        let config = TabletConfig {
            pad_map_button: Some(3),
            ..TabletConfig::default()
        };
        let by_model: BTreeMap<String, TabletConfig> =
            [("056a:0357:Test tablet Pen".to_owned(), config.clone())].into();
        assert_eq!(tablet_config(&map, &by_model), Some(&config));
        let other: BTreeMap<String, TabletConfig> =
            [("056a:0001:Another".to_owned(), config)].into();
        assert_eq!(tablet_config(&map, &other), None);
    }

    #[test]
    fn a_ring_sits_mid_edge_around_its_mode_button() {
        use Control::{Button, Ring};
        assert!(matches_device(INTUOS_PRO, 0x056a, 0x0357));
        assert!(matches_device(INTUOS_PRO, 0x056a, 0x0360));
        assert!(!matches_device(INTUOS_PRO, 0x056a, 0x0358));
        let layout = parse_layout(INTUOS_PRO, 9, 1, 0).expect("every button is placed");
        assert_eq!(
            layout.left,
            [
                Button(0),
                Button(1),
                Button(2),
                Button(3),
                Ring(0),
                Button(8),
                Button(4),
                Button(5),
                Button(6),
                Button(7),
            ]
        );
        assert!(layout.right.is_empty() && layout.top.is_empty());
        assert_eq!(
            parse_layout(INTUOS_PRO, 10, 1, 0),
            None,
            "a button no edge holds"
        );
        assert_eq!(
            parse_layout("[Device]\nName=Pad\n", 4, 0, 0),
            None,
            "no [Buttons] section, no layout"
        );
        let strips = parse_layout("[Buttons]\nLeft=A;B\nRight=C;D\n", 4, 0, 2).expect("placed");
        assert_eq!(
            strips.left.first(),
            Some(&Control::Strip(0)),
            "a strip heads its edge"
        );
        assert_eq!(strips.right.first(), Some(&Control::Strip(1)));
    }

    #[test]
    fn a_known_model_is_drawn_with_each_control_at_its_edge() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pad.tablet"),
            "[Device]\nDeviceMatch=usb|056a|0357;\n[Buttons]\nLeft=A;B\nRight=C;D\nRing=D\n",
        )
        .expect("write");
        std::fs::write(dir.path().join("readme.txt"), INTUOS_PRO).expect("write");
        let mut held_map = Held::new(BTreeMap::new());
        held_map.read_layouts_from(vec![dir.path().to_path_buf()]);
        let map = held(false, Vec::new(), vec![0]);
        held_map.follow(Some(&map));
        let reading = held_map.reading.clone();
        assert!(reading.drawn, "the model's arrangement is known");
        let at = |edge: &[PadMapControl]| -> Vec<String> {
            edge.iter().map(|control| control.name.clone()).collect()
        };
        assert_eq!(at(&reading.left), ["Strip 1", "Button 1", "Button 2"]);
        assert_eq!(at(&reading.right), ["Ring 1", "Button 4", "Button 3"]);
        assert!(reading.top.is_empty() && reading.bottom.is_empty());
        assert_eq!(reading.controls.len(), 6, "the list holds every control");

        let mut unknown = held(false, Vec::new(), vec![0]);
        unknown.tablet.usb_id = Some((0x056a, 0x0001));
        held_map.follow(Some(&unknown));
        assert!(!held_map.reading.drawn, "an unknown model is listed");
        held_map.follow(None);
        assert_eq!(
            held_map.reading,
            Reading::default(),
            "nothing held, nothing shown"
        );
    }
}
