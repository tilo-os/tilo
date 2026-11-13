//! One person's displays across all of their devices, in one shared layout.
//!
//! Every device publishes the outputs it drives as [`DeviceOutput`], and the person's
//! [`Arrangement`] is one roaming setting: for each display, its order and its position in one
//! coordinate space. The pointer crosses only where two displays meet, so a display placed apart
//! from the others is out of reach. Each display's entry carries its
//! own revision, so edits made at the same time to different displays on different devices all
//! survive.
//!
//! Each device stays authoritative for its own hardware: modes, scale, rotation and how its own
//! outputs sit next to each other come from its local display configuration. [`Layout::resolve`]
//! places a device's outputs as one rigid group, and the arrangement decides where the groups sit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const ARRANGEMENT_SECTION: &str = crate::hardware::DISPLAY_PERSON_SECTION;
pub const ARRANGEMENT_LEAF: &str = "arrangement";
pub const ARRANGEMENT_KEY: &str = "tilo.display_person.arrangement";
pub const MAX_ARRANGED_DISPLAYS: usize = 128;
pub const MAX_DEVICE_OUTPUTS: usize = 16;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_COORDINATE: i32 = 1_000_000;

/// One display's place in the person's shared layout. Unknown fields are ignored: entries stored
/// while a display could be left out of the layout carry an `enabled` flag, which placement
/// replaced.
#[cfg_attr(feature = "model", derive(tilo_resource::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct ArrangedDisplay {
    /// Fabric device id of the device that drives the display.
    pub device: String,
    /// The device's stable output id ([`DeviceOutput::id`]).
    pub output: String,
    /// Sort key of the display order; lower comes first.
    pub order: i64,
    /// Whether `x`/`y` were ever set. A display first arranged while off
    /// has no position until it is placed.
    pub placed: bool,
    /// Top-left in the shared coordinate space, logical pixels.
    pub x: i32,
    pub y: i32,
    /// Revision of this entry. An edit sets it above every revision in the
    /// arrangement it edited; merging keeps the highest per display.
    pub rev: u64,
}

crate::read_through_json_value!(ArrangedDisplay);

impl ArrangedDisplay {
    fn same_as(&self, other: &Self) -> bool {
        Self {
            rev: 0,
            ..self.clone()
        } == Self {
            rev: 0,
            ..other.clone()
        }
    }
}

/// Spacing of fresh order keys, leaving room to insert between them.
const ORDER_GAP: i64 = 1024;

/// The person's display arrangement.
#[cfg_attr(feature = "model", derive(tilo_resource::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct Arrangement {
    pub displays: Vec<ArrangedDisplay>,
}

crate::read_through_json_value!(Arrangement);

#[cfg(feature = "model")]
tilo_resource::json_model!(Arrangement);

impl Arrangement {
    pub fn from_value(value: &Value) -> Option<Self> {
        let decoded: Self = serde_json::from_value(value.clone()).ok()?;
        if decoded.displays.len() > MAX_ARRANGED_DISPLAYS {
            return None;
        }
        if decoded.displays.iter().any(|entry| {
            !valid_name(&entry.device)
                || !valid_name(&entry.output)
                || entry.x.unsigned_abs() > MAX_COORDINATE.unsigned_abs()
                || entry.y.unsigned_abs() > MAX_COORDINATE.unsigned_abs()
                || entry.order.unsigned_abs() > MAX_ORDER.unsigned_abs()
        }) {
            return None;
        }
        Some(Self::merge([("", decoded)]))
    }

    /// Merge the arrangements several devices hold, entry by entry: for
    /// each display the entry with the highest revision wins, ties going to
    /// the author that sorts last, so every device picks the same winner.
    pub fn merge<'a>(arrangements: impl IntoIterator<Item = (&'a str, Self)>) -> Self {
        let mut winners: BTreeMap<(String, String), (u64, &'a str, ArrangedDisplay)> =
            BTreeMap::new();
        for (author, arrangement) in arrangements {
            for entry in arrangement.displays {
                let key = (entry.device.clone(), entry.output.clone());
                let newer = winners
                    .get(&key)
                    .is_none_or(|(rev, by, _)| (entry.rev, author) > (*rev, *by));
                if newer {
                    winners.insert(key, (entry.rev, author, entry));
                }
            }
        }
        let mut displays: Vec<ArrangedDisplay> =
            winners.into_values().map(|(_, _, entry)| entry).collect();
        displays
            .sort_by(|a, b| (a.order, &a.device, &a.output).cmp(&(b.order, &b.device, &b.output)));
        displays.truncate(MAX_ARRANGED_DISPLAYS);
        Self { displays }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_document(document: &Map<String, Value>) -> Self {
        document
            .get(ARRANGEMENT_SECTION)
            .and_then(|section| section.get(ARRANGEMENT_LEAF))
            .and_then(Self::from_value)
            .unwrap_or_default()
    }

    pub fn load() -> Self {
        Self::from_document(&crate::load())
    }

    pub fn save(&self) -> std::io::Result<()> {
        let mut values = Map::new();
        values.insert(ARRANGEMENT_LEAF.into(), self.to_value());
        crate::update_section(ARRANGEMENT_SECTION, values)
    }

    pub fn entry(&self, device: &str, output: &str) -> Option<&ArrangedDisplay> {
        self.displays
            .iter()
            .find(|entry| entry.device == device && entry.output == output)
    }

    fn max_rev(&self) -> u64 {
        self.displays
            .iter()
            .map(|entry| entry.rev)
            .max()
            .unwrap_or(0)
    }

    fn max_order(&self) -> i64 {
        self.displays
            .iter()
            .map(|entry| entry.order)
            .max()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn with_changes(&self, changes: impl IntoIterator<Item = ArrangedDisplay>) -> Self {
        let rev = self.max_rev().saturating_add(1);
        let mut next = self.clone();
        for mut change in changes {
            let full = next.displays.len() >= MAX_ARRANGED_DISPLAYS;
            match next
                .displays
                .iter_mut()
                .find(|entry| entry.device == change.device && entry.output == change.output)
            {
                Some(entry) if entry.same_as(&change) => {}
                Some(entry) => {
                    change.rev = rev;
                    *entry = change;
                }
                None if !full => {
                    change.rev = rev;
                    next.displays.push(change);
                }
                None => {}
            }
        }
        next.displays
            .sort_by(|a, b| (a.order, &a.device, &a.output).cmp(&(b.order, &b.device, &b.output)));
        next
    }
}

/// Order keys stay well inside `i64`.
const MAX_ORDER: i64 = 1 << 52;

fn valid_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME_BYTES && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceOutput {
    /// Stable monitor id: the EDID make, model and serial, with the
    /// connector only to tell identical monitors apart.
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub mode_width: u32,
    pub mode_height: u32,
    pub refresh_mhz: u32,
    pub scale_milli: u16,
    pub connected: bool,
    pub enabled: bool,
}

impl DeviceOutput {
    pub fn active(&self) -> bool {
        self.connected && self.enabled && self.width > 0 && self.height > 0
    }

    /// Check bounds a peer's report must satisfy.
    pub fn valid(&self) -> bool {
        valid_name(&self.id)
            && valid_name(&self.name)
            && self.x.unsigned_abs() <= MAX_COORDINATE.unsigned_abs()
            && self.y.unsigned_abs() <= MAX_COORDINATE.unsigned_abs()
            && i32::try_from(self.width).is_ok_and(|width| width <= MAX_COORDINATE)
            && i32::try_from(self.height).is_ok_and(|height| height <= MAX_COORDINATE)
            && (!self.enabled || (500..=4000).contains(&self.scale_milli))
    }

    pub fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonDevice {
    pub device: String,
    pub label: String,
    pub local: bool,
    pub online: bool,
    pub outputs: Vec<DeviceOutput>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn right(self) -> i32 {
        self.x.saturating_add_unsigned(self.width)
    }

    pub fn bottom(self) -> i32 {
        self.y.saturating_add_unsigned(self.height)
    }

    pub fn contains(self, (x, y): (f64, f64)) -> bool {
        x >= f64::from(self.x)
            && x < f64::from(self.right())
            && y >= f64::from(self.y)
            && y < f64::from(self.bottom())
    }

    #[must_use]
    pub fn translate(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
            ..self
        }
    }

    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self {
            x,
            y,
            width: u32::try_from(right.saturating_sub(x)).unwrap_or(0),
            height: u32::try_from(bottom.saturating_sub(y)).unwrap_or(0),
        }
    }

    fn clamp(self, (x, y): (f64, f64)) -> (f64, f64) {
        let max_x = f64::from(self.right()) - EDGE_INSET;
        let max_y = f64::from(self.bottom()) - EDGE_INSET;
        (
            x.clamp(f64::from(self.x), max_x.max(f64::from(self.x))),
            y.clamp(f64::from(self.y), max_y.max(f64::from(self.y))),
        )
    }
}

const EDGE_INSET: f64 = 1.0 / 64.0;

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one display, each read on its own"
)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDisplay {
    pub device: String,
    pub output: String,
    pub name: String,
    pub device_label: String,
    pub builtin: bool,
    pub local: bool,
    pub online: bool,
    pub order: usize,
    pub local_rect: Rect,
    pub rect: Rect,
}

impl ResolvedDisplay {
    pub fn to_device(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            x - f64::from(self.rect.x) + f64::from(self.local_rect.x),
            y - f64::from(self.rect.y) + f64::from(self.local_rect.y),
        )
    }

    pub fn to_shared(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            x - f64::from(self.local_rect.x) + f64::from(self.rect.x),
            y - f64::from(self.local_rect.y) + f64::from(self.rect.y),
        )
    }

    pub fn is(&self, device: &str, output: &str) -> bool {
        self.device == device && self.output == output
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointerStep {
    pub shared: (f64, f64),
    pub display: usize,
    pub device_point: (f64, f64),
}

fn device_offsets<'a>(
    sorted: &[&'a PersonDevice],
    arrangement: &Arrangement,
) -> BTreeMap<&'a str, (i32, i32)> {
    let mut offsets: BTreeMap<&str, (i32, i32)> = BTreeMap::new();
    let mut placed_bounds: Option<Rect> = None;
    let mut unplaced: Vec<(&PersonDevice, Rect)> = Vec::new();
    for device in sorted {
        let active: Vec<&DeviceOutput> = device
            .outputs
            .iter()
            .filter(|output| output.active())
            .collect();
        let Some(bounds) = active
            .iter()
            .map(|output| output.rect())
            .reduce(Rect::union)
        else {
            continue;
        };
        let anchor = active
            .iter()
            .filter_map(|output| {
                arrangement
                    .entry(&device.device, &output.id)
                    .filter(|entry| entry.placed)
                    .map(|entry| (entry, *output))
            })
            .min_by_key(|(entry, _)| (entry.order, &entry.output));
        if let Some((entry, output)) = anchor {
            let offset = (
                entry.x.saturating_sub(output.x),
                entry.y.saturating_sub(output.y),
            );
            offsets.insert(device.device.as_str(), offset);
            let shared = bounds.translate(offset.0, offset.1);
            placed_bounds = Some(placed_bounds.map_or(shared, |all| all.union(shared)));
        } else {
            unplaced.push((device, bounds));
        }
    }
    for (device, bounds) in unplaced {
        let offset = placed_bounds.map_or_else(
            || (bounds.x.saturating_neg(), bounds.y.saturating_neg()),
            |all| {
                (
                    all.right().saturating_sub(bounds.x),
                    all.y.saturating_sub(bounds.y),
                )
            },
        );
        offsets.insert(device.device.as_str(), offset);
        let shared = bounds.translate(offset.0, offset.1);
        placed_bounds = Some(placed_bounds.map_or(shared, |all| all.union(shared)));
    }
    offsets
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub displays: Vec<ResolvedDisplay>,
}

impl Layout {
    /// Lay out the person's screens that show something: a device placed in the arrangement
    /// stands where its anchor screen was put, and the devices not placed yet line up to the
    /// right of the others, by id, so every device and Settings lay them out alike.
    pub fn resolve(devices: &[PersonDevice], arrangement: &Arrangement) -> Self {
        let mut sorted: Vec<&PersonDevice> = devices.iter().collect();
        sorted.sort_by(|a, b| a.device.cmp(&b.device));
        let offsets = device_offsets(&sorted, arrangement);

        let mut displays: Vec<(Option<i64>, ResolvedDisplay)> = Vec::new();
        for device in &sorted {
            let (dx, dy) = offsets
                .get(device.device.as_str())
                .copied()
                .unwrap_or_default();
            let mut outputs: Vec<&DeviceOutput> = device
                .outputs
                .iter()
                .filter(|output| output.active())
                .collect();
            outputs.sort_by_key(|output| (output.x, output.y));
            for output in outputs {
                let entry = arrangement.entry(&device.device, &output.id);
                displays.push((
                    entry.map(|entry| entry.order),
                    ResolvedDisplay {
                        device: device.device.clone(),
                        output: output.id.clone(),
                        name: output.name.clone(),
                        device_label: device.label.clone(),
                        builtin: output.builtin,
                        local: device.local,
                        online: device.online,
                        order: 0,
                        local_rect: output.rect(),
                        rect: output.rect().translate(dx, dy),
                    },
                ));
            }
        }
        displays.sort_by_key(|(order, _)| (order.is_none(), order.unwrap_or_default()));
        Self {
            displays: displays
                .into_iter()
                .enumerate()
                .map(|(order, (_, mut display))| {
                    display.order = order;
                    display
                })
                .collect(),
        }
    }

    pub fn find(&self, device: &str, output: &str) -> Option<&ResolvedDisplay> {
        self.displays
            .iter()
            .find(|display| display.is(device, output))
    }

    pub fn device_bounds(&self, device: &str) -> Option<Rect> {
        self.displays
            .iter()
            .filter(|display| display.device == device)
            .map(|display| display.rect)
            .reduce(Rect::union)
    }

    pub fn locate(&self, device: &str, point: (f64, f64)) -> Option<usize> {
        self.displays
            .iter()
            .position(|display| display.device == device && display.local_rect.contains(point))
    }

    /// Move the pointer at `from` (a shared-layout point on display
    /// `current`) by `delta`. It lands on whichever display `reachable`
    /// admits under the new point; with none there it slides along the
    /// current display's edge, as it does at the edge of a single screen.
    /// Sub-pixel position is kept, so motion speed carries across edges.
    pub fn step_pointer(
        &self,
        current: usize,
        from: (f64, f64),
        delta: (f64, f64),
        reachable: impl Fn(&ResolvedDisplay) -> bool,
    ) -> Option<PointerStep> {
        let here = self.displays.get(current)?;
        let proposed = (from.0 + delta.0, from.1 + delta.1);
        let target = std::iter::once((current, here))
            .chain(self.displays.iter().enumerate())
            .find(|&(index, display)| {
                display.rect.contains(proposed) && (index == current || reachable(display))
            });
        let (index, display, shared) = target.map_or_else(
            || (current, here, here.rect.clamp(proposed)),
            |(index, display)| (index, display, proposed),
        );
        Some(PointerStep {
            shared,
            display: index,
            device_point: display.to_device(shared),
        })
    }

    fn pin(display: &ResolvedDisplay, previous: &Arrangement, order: i64) -> ArrangedDisplay {
        let entry = previous.entry(&display.device, &display.output);
        ArrangedDisplay {
            device: display.device.clone(),
            output: display.output.clone(),
            order: entry.map_or(order, |entry| entry.order),
            placed: true,
            x: display.rect.x,
            y: display.rect.y,
            rev: 0,
        }
    }

    fn pin_missing(&self, previous: &Arrangement) -> Vec<ArrangedDisplay> {
        let mut next_order = previous.max_order();
        self.displays
            .iter()
            .filter(|display| previous.entry(&display.device, &display.output).is_none())
            .map(|display| {
                next_order = next_order.saturating_add(ORDER_GAP);
                Self::pin(display, previous, next_order)
            })
            .collect()
    }

    #[must_use]
    pub fn move_device(
        &self,
        previous: &Arrangement,
        device: &str,
        dx: i32,
        dy: i32,
    ) -> Arrangement {
        let pinned = previous.with_changes(self.pin_missing(previous));
        let moved = self
            .displays
            .iter()
            .filter(|display| display.device == device)
            .map(|display| {
                let mut entry = Self::pin(display, &pinned, 0);
                entry.x = display.rect.x.saturating_add(dx);
                entry.y = display.rect.y.saturating_add(dy);
                entry
            })
            .collect::<Vec<_>>();
        pinned.with_changes(moved)
    }
}

pub const SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonDisplays {
    pub schema_version: u16,
    pub publisher_pid: u32,
    pub publisher_start_ticks: u64,
    pub local_device: String,
    pub devices: Vec<PersonDevice>,
}

static PUBLISHED: crate::published::Published<PersonDisplays> = crate::published::Published::new();

impl PersonDisplays {
    pub fn load() -> Option<Self> {
        let path = path()?;
        Self::load_at(&path)
    }

    pub fn published() -> Option<std::sync::Arc<Self>> {
        PUBLISHED.get(&path()?)
    }

    pub fn published_generation() -> u64 {
        PUBLISHED.generation()
    }

    pub fn load_at(path: &Path) -> Option<Self> {
        crate::published::note_file_read();
        let bytes = tilo_files::native::bounded::read(path, 2_097_152).ok()??;
        let view: Self = serde_json::from_slice(&bytes).ok()?;
        (view.schema_version == SCHEMA_VERSION
            && crate::hardware::publisher_live_in(
                path.parent()?,
                view.publisher_pid,
                view.publisher_start_ticks,
            ))
        .then_some(view)
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is unavailable"))?;
        if PUBLISHED.get(&path).as_deref() != Some(self) {
            PUBLISHED.set(&path, self.clone());
        }
        self.save_at(&path)
    }

    pub fn save_at(&self, path: &Path) -> std::io::Result<()> {
        // Equality includes the publisher identity and every output fact.
        // A regular observation is reusable only under its current live lease.
        if tilo_files::native::is_regular_file(path).unwrap_or(false)
            && Self::load_at(path).as_ref() == Some(self)
        {
            return Ok(());
        }
        let mut body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        body.push(b'\n');
        tilo_files::native::atomic::write_creating_parent(path, &body)
    }

    pub fn layout(&self, arrangement: &Arrangement) -> Layout {
        Layout::resolve(&self.devices, arrangement)
    }
}

pub fn path() -> Option<PathBuf> {
    crate::hardware::display_snapshot_path().and_then(|snapshot| {
        snapshot
            .parent()
            .map(|dir| dir.join("person-displays.json"))
    })
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the expected points are exact binary fractions"
)]
mod tests {
    use super::*;

    fn output(id: &str, x: i32, y: i32, width: u32, height: u32) -> DeviceOutput {
        DeviceOutput {
            id: id.into(),
            name: id.into(),
            builtin: false,
            x,
            y,
            width,
            height,
            mode_width: width,
            mode_height: height,
            refresh_mhz: 60_000,
            scale_milli: 1000,
            connected: true,
            enabled: true,
        }
    }

    fn arranged(device: &str, output: &str, order: i64, x: i32, y: i32) -> ArrangedDisplay {
        ArrangedDisplay {
            device: device.into(),
            output: output.into(),
            order,
            placed: true,
            x,
            y,
            rev: 1,
        }
    }

    fn device(id: &str, local: bool, outputs: Vec<DeviceOutput>) -> PersonDevice {
        PersonDevice {
            device: id.into(),
            label: id.to_uppercase(),
            local,
            online: true,
            outputs,
        }
    }

    fn laptop_and_pi() -> Vec<PersonDevice> {
        vec![
            device(
                "laptop",
                true,
                vec![
                    output("panel", 0, 0, 1440, 900),
                    output("hdmi", 1440, 0, 1920, 1080),
                ],
            ),
            device("pi", false, vec![output("pi-hdmi", 0, 0, 1920, 1080)]),
        ]
    }

    #[test]
    fn unarranged_devices_line_up_left_to_right_by_id() {
        let layout = Layout::resolve(&laptop_and_pi(), &Arrangement::default());
        let names: Vec<_> = layout.displays.iter().map(|d| d.output.as_str()).collect();
        assert_eq!(names, ["panel", "hdmi", "pi-hdmi"]);
        assert_eq!(layout.find("laptop", "hdmi").unwrap().rect.x, 1440);
        let pi = layout.find("pi", "pi-hdmi").unwrap();
        assert_eq!((pi.rect.x, pi.rect.y), (3360, 0));
        assert_eq!(
            layout.displays.iter().map(|d| d.order).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn a_device_keeps_its_own_screen_layout_around_its_anchor() {
        let arrangement = Arrangement {
            displays: vec![
                arranged("pi", "pi-hdmi", 1, -1920, 100),
                arranged("laptop", "hdmi", 2, 5000, 5000),
            ],
        };
        let layout = Layout::resolve(&laptop_and_pi(), &arrangement);
        let names: Vec<_> = layout.displays.iter().map(|d| d.output.as_str()).collect();
        assert_eq!(names, ["pi-hdmi", "hdmi", "panel"]);
        let hdmi = layout.find("laptop", "hdmi").unwrap();
        let panel = layout.find("laptop", "panel").unwrap();
        assert_eq!((hdmi.rect.x, hdmi.rect.y), (5000, 5000));
        assert_eq!((panel.rect.x, panel.rect.y), (5000 - 1440, 5000));
    }

    #[test]
    fn the_pointer_crosses_at_the_mapped_position_and_slides_along_blocked_edges() {
        let layout = Layout::resolve(&laptop_and_pi(), &Arrangement::default());
        let hdmi = layout
            .displays
            .iter()
            .position(|d| d.output == "hdmi")
            .unwrap();
        let pi = layout
            .displays
            .iter()
            .position(|d| d.output == "pi-hdmi")
            .unwrap();
        let from = (3359.5, 500.25);
        let step = layout
            .step_pointer(hdmi, from, (4.0, 1.0), |_| true)
            .unwrap();
        assert_eq!(step.display, pi);
        assert_eq!(step.shared, (3363.5, 501.25));
        assert_eq!(step.device_point, (3.5, 501.25));
        let back = layout
            .step_pointer(pi, step.shared, (-8.0, 0.0), |_| true)
            .unwrap();
        assert_eq!(back.display, hdmi);
        assert_eq!(back.device_point, (3355.5, 501.25));
        let blocked = layout
            .step_pointer(hdmi, from, (4.0, 3.0), |d| d.device != "pi")
            .unwrap();
        assert_eq!(blocked.display, hdmi);
        assert_eq!(blocked.shared.1, 503.25, "motion along the edge is kept");
        assert!(
            blocked.shared.0 < 3360.0 && blocked.shared.0 > 3359.9,
            "rests at the edge"
        );
        let panel = layout
            .displays
            .iter()
            .position(|d| d.output == "panel")
            .unwrap();
        let down = layout
            .step_pointer(panel, (100.0, 899.0), (0.0, 5.0), |_| true)
            .unwrap();
        assert_eq!(down.display, panel);
        assert!(down.shared.1 < 900.0);
    }

    #[test]
    fn edits_touch_only_their_displays_and_keep_offline_places() {
        let devices = laptop_and_pi();
        let previous = Arrangement {
            displays: vec![arranged("tv", "tv-panel", 0, -4000, 0)],
        };
        let layout = Layout::resolve(&devices, &previous);
        let moved = layout.move_device(&previous, "pi", -3360 - 1920, 0);
        let again = Layout::resolve(&devices, &moved);
        let pi = again.find("pi", "pi-hdmi").unwrap();
        assert_eq!(pi.rect.x, -1920);
        assert_eq!(again.find("laptop", "panel").unwrap().rect.x, 0);
        assert_eq!(
            moved.entry("tv", "tv-panel"),
            previous.entry("tv", "tv-panel")
        );
    }

    #[test]
    fn concurrent_moves_of_different_devices_both_survive_the_merge() {
        let devices = vec![
            device("laptop", true, vec![output("panel", 0, 0, 1440, 900)]),
            device("pi", false, vec![output("pi-hdmi", 0, 0, 1920, 1080)]),
            device("tv", false, vec![output("tv-panel", 0, 0, 1920, 1080)]),
        ];
        let base = Layout::resolve(&devices, &Arrangement::default());
        let shared = base.move_device(&Arrangement::default(), "pi", 0, 0);
        let layout = Layout::resolve(&devices, &shared);
        let on_laptop = layout.move_device(&shared, "pi", 0, 900);
        let on_pi = layout.move_device(&shared, "tv", -1920 - 3360, 0);
        let merged = Arrangement::merge([("laptop", on_laptop.clone()), ("pi", on_pi.clone())]);
        let resolved = Layout::resolve(&devices, &merged);
        assert_eq!(resolved.find("pi", "pi-hdmi").unwrap().rect.y, 900);
        assert_eq!(resolved.find("tv", "tv-panel").unwrap().rect.x, -1920);
        assert_eq!(
            merged,
            Arrangement::merge([("pi", on_pi), ("laptop", on_laptop)])
        );
        assert_eq!(merged, Arrangement::merge([("x", merged.clone())]));
        let a = shared.with_changes([arranged("pi", "pi-hdmi", 0, 1, 1)]);
        let b = shared.with_changes([arranged("pi", "pi-hdmi", 0, 2, 2)]);
        let winner = Arrangement::merge([("aa", a.clone()), ("bb", b.clone())]);
        assert_eq!(winner, Arrangement::merge([("bb", b), ("aa", a)]));
        assert_eq!(winner.entry("pi", "pi-hdmi").unwrap().x, 2);
    }

    #[test]
    fn switched_off_displays_are_out_of_the_layout() {
        let mut devices = laptop_and_pi();
        devices[1].outputs.push(DeviceOutput {
            enabled: false,
            width: 0,
            height: 0,
            ..output("pi-dp", 0, 0, 0, 0)
        });
        let layout = Layout::resolve(&devices, &Arrangement::default());
        assert!(
            layout.find("pi", "pi-dp").is_none(),
            "an off display is not reachable"
        );
        assert_eq!(layout.displays.len(), 3);
    }

    #[test]
    fn stored_arrangements_are_bounded_and_deduplicated() {
        let entry = serde_json::json!({"device": "a", "output": "b", "enabled": true,
            "order": 0, "placed": true, "x": 1, "y": 2, "rev": 1});
        let value = serde_json::json!({"displays": [entry, entry]});
        assert_eq!(
            Arrangement::from_value(&value).unwrap().displays.len(),
            1,
            "an entry stored with the flag that once left a display out still reads"
        );
        let far = serde_json::json!({"displays": [{"device": "a", "output": "b",
            "order": 0, "placed": true, "x": 2_000_000, "y": 0, "rev": 1}]});
        assert!(Arrangement::from_value(&far).is_none());
        for (field, value) in [
            ("x", serde_json::json!(i32::MIN)),
            ("y", serde_json::json!(i32::MIN)),
            ("order", serde_json::json!(i64::MIN)),
        ] {
            let mut invalid = far.clone();
            invalid["displays"][0][field] = value;
            assert!(Arrangement::from_value(&invalid).is_none());
        }
        let mut malformed_output = output("bad", i32::MIN, 0, 100, 100);
        assert!(!malformed_output.valid());
        malformed_output.x = 0;
        malformed_output.y = i32::MIN;
        assert!(!malformed_output.valid());
        let unknown = serde_json::json!({"displays": [], "extra": 1});
        assert!(Arrangement::from_value(&unknown).is_none());
        let mut document = Map::new();
        document.insert(
            ARRANGEMENT_SECTION.into(),
            serde_json::json!({"arrangement": value}),
        );
        assert_eq!(Arrangement::from_document(&document).displays.len(), 1);
    }
}

#[cfg(all(test, any(unix, windows)))]
mod publication_regressions {
    use super::*;

    #[test]
    fn unchanged_live_person_observations_keep_the_published_file() {
        let dir = crate::test_dir();
        let path = dir.path().join("person-displays.json");
        let (publisher_pid, publisher_start_ticks) =
            crate::hardware::publisher_identity_in(dir.path()).unwrap();
        let mut view = PersonDisplays {
            schema_version: SCHEMA_VERSION,
            publisher_pid,
            publisher_start_ticks,
            local_device: "own-device".into(),
            devices: Vec::new(),
        };
        view.save_at(&path).unwrap();
        let retained = std::fs::File::open(&path).unwrap();
        let original = tilo_files::native::atomic::FileIdentity::of(&retained).unwrap();
        view.save_at(&path).unwrap();
        let current = std::fs::File::open(&path).unwrap();
        assert_eq!(
            original,
            tilo_files::native::atomic::FileIdentity::of(&current).unwrap(),
            "an unchanged live observation must not perform durable replacement"
        );
        assert_eq!(PersonDisplays::load_at(&path), Some(view.clone()));
        view.local_device = "changed-device".into();
        view.save_at(&path).unwrap();
        assert_eq!(PersonDisplays::load_at(&path), Some(view.clone()));
        std::fs::write(&path, b"invalid observation").unwrap();
        view.save_at(&path).unwrap();
        assert_eq!(PersonDisplays::load_at(&path), Some(view.clone()));
        std::fs::remove_file(&path).unwrap();
        view.save_at(&path).unwrap();
        assert_eq!(PersonDisplays::load_at(&path), Some(view.clone()));
        let mut stale = view.clone();
        stale.publisher_start_ticks = stale.publisher_start_ticks.saturating_add(1);
        stale.save_at(&path).unwrap();
        assert_eq!(
            PersonDisplays::load_at(&path),
            None,
            "a previous publisher identity never becomes live by saving"
        );
        view.save_at(&path).unwrap();
        assert_eq!(PersonDisplays::load_at(&path), Some(view));
    }
    #[cfg(unix)]
    #[test]
    fn matching_symlink_observations_are_replaced_under_the_owned_path() {
        let dir = crate::test_dir();
        let path = dir.path().join("person-displays.json");
        let alias = dir.path().join("alias.json");
        let (publisher_pid, publisher_start_ticks) =
            crate::hardware::publisher_identity_in(dir.path()).unwrap();
        let view = PersonDisplays {
            schema_version: SCHEMA_VERSION,
            publisher_pid,
            publisher_start_ticks,
            local_device: "own-device".into(),
            devices: Vec::new(),
        };
        view.save_at(&alias).unwrap();
        let original = std::fs::read(&alias).unwrap();
        std::os::unix::fs::symlink(&alias, &path).unwrap();
        view.save_at(&path).unwrap();
        assert!(
            std::fs::symlink_metadata(&path).unwrap().is_file(),
            "matching linked observations must still use atomic directory-entry replacement"
        );
        assert_eq!(std::fs::read(&alias).unwrap(), original);
        assert_eq!(PersonDisplays::load_at(&path), Some(view));
    }
}
