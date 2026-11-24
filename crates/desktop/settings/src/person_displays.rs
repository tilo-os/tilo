//! One person's displays across all of their devices, in one shared layout.
//!
//! Every device publishes the outputs it drives as [`DeviceOutput`], and the person's placement
//! is one roaming setting: for each display, its order and its position in one coordinate space.
//! The pointer crosses only where two displays meet, so a display placed apart from the others
//! is out of reach. The placement is a keyed collection of [`PlacedDisplay`]s, one entry per
//! device and output, so edits made at the same time to different displays on different devices
//! all survive.
//!
//! Each device stays authoritative for its own hardware: modes, scale, rotation and how its own
//! outputs sit next to each other come from its local display configuration. [`Layout::resolve`]
//! places a device's outputs as one rigid group, and the placement decides where the groups sit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PLACEMENTS_SECTION: &str = crate::hardware::DISPLAY_PERSON_SECTION;
pub const PLACEMENTS_LEAF: &str = "placement";
pub const MAX_PLACED_DISPLAYS: usize = 128;
pub const MAX_DEVICE_OUTPUTS: usize = 16;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_COORDINATE: i32 = 1_000_000;

/// One display's place in the person's shared layout. Entries are keyed by the device and the
/// output they drive ([`Identified`](tilo_resource::Identified)), so a device that places its own
/// displays writes only their entries.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct PlacedDisplay {
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
}

crate::read_through_json_value!(PlacedDisplay);

#[cfg(feature = "model")]
impl tilo_resource::Identified for PlacedDisplay {
    fn id(&self) -> tilo_resource::Id {
        tilo_resource::Id::from_raw(format!("{}/{}", self.device, self.output))
    }
}

impl PlacedDisplay {
    /// Whether the entry is within the bounds an action accepts and a peer's report must keep.
    #[must_use]
    pub fn valid(&self) -> bool {
        valid_name(&self.device)
            && valid_name(&self.output)
            && self.x.unsigned_abs() <= MAX_COORDINATE.unsigned_abs()
            && self.y.unsigned_abs() <= MAX_COORDINATE.unsigned_abs()
            && self.order.unsigned_abs() <= MAX_ORDER.unsigned_abs()
    }
}

/// Spacing of fresh order keys, leaving room to insert between them.
pub const ORDER_GAP: i64 = 1024;

/// The placement as the settings document keeps it: one entry per display, by
/// `<device>/<output>`, so an edit to one display writes only its entry and a concurrent edit to
/// another display on another device stays.
#[cfg(feature = "model")]
pub mod places {
    use std::collections::BTreeMap;

    use serde::Deserialize as _;
    use tilo_resource::{Identified as _, Keyed};

    use super::{MAX_PLACED_DISPLAYS, PlacedDisplay};

    /// Whether the placement holds no display.
    #[must_use]
    pub fn is_empty(placement: &Keyed<PlacedDisplay>) -> bool {
        placement.is_empty()
    }

    pub fn serialize<S: serde::Serializer>(
        placement: &Keyed<PlacedDisplay>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let entries: BTreeMap<String, &PlacedDisplay> = placement
            .iter()
            .map(|entry| (entry.id().as_str().to_owned(), entry))
            .collect();
        serde::Serialize::serialize(&entries, serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Keyed<PlacedDisplay>, D::Error> {
        let entries = BTreeMap::<String, PlacedDisplay>::deserialize(deserializer)?;
        Ok(entries
            .into_values()
            .filter(PlacedDisplay::valid)
            .take(MAX_PLACED_DISPLAYS)
            .collect())
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

/// The entry that places `device`'s `output`, if the placement holds one.
fn placed<'a>(
    placement: &'a [PlacedDisplay],
    device: &str,
    output: &str,
) -> Option<&'a PlacedDisplay> {
    placement
        .iter()
        .find(|entry| entry.device == device && entry.output == output)
}

fn device_offsets<'a>(
    sorted: &[&'a PersonDevice],
    placement: &[PlacedDisplay],
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
                placed(placement, &device.device, &output.id)
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
    /// Lay out the person's screens that show something: a device placed in the placements
    /// stands where its anchor screen was put, and the devices not placed yet line up to the
    /// right of the others, by id, so every device and Settings lay them out alike.
    pub fn resolve(devices: &[PersonDevice], placement: &[PlacedDisplay]) -> Self {
        let mut sorted: Vec<&PersonDevice> = devices.iter().collect();
        sorted.sort_by(|a, b| a.device.cmp(&b.device));
        let offsets = device_offsets(&sorted, placement);

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
                let entry = placed(placement, &device.device, &output.id);
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
        let bytes = tilo_files::native::bounded::read(path, 2_097_152).ok()?;
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

    pub fn layout(&self, placement: &[PlacedDisplay]) -> Layout {
        Layout::resolve(&self.devices, placement)
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

    fn arranged(device: &str, output: &str, order: i64, x: i32, y: i32) -> PlacedDisplay {
        PlacedDisplay {
            device: device.into(),
            output: output.into(),
            order,
            placed: true,
            x,
            y,
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

    tilo_test_env::scenario! {
        fn the_shared_layout_places_displays_and_carries_the_pointer;

        phase unarranged_devices_line_up_left_to_right_by_id;
        {
            let layout = Layout::resolve(&laptop_and_pi(), &[]);
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

        phase a_device_keeps_its_own_screen_layout_around_its_anchor;
        {
            let placement = [
                arranged("pi", "pi-hdmi", 1, -1920, 100),
                arranged("laptop", "hdmi", 2, 5000, 5000),
            ];
            let layout = Layout::resolve(&laptop_and_pi(), &placement);
            let names: Vec<_> = layout.displays.iter().map(|d| d.output.as_str()).collect();
            assert_eq!(names, ["pi-hdmi", "hdmi", "panel"]);
            let hdmi = layout.find("laptop", "hdmi").unwrap();
            let panel = layout.find("laptop", "panel").unwrap();
            assert_eq!((hdmi.rect.x, hdmi.rect.y), (5000, 5000));
            assert_eq!((panel.rect.x, panel.rect.y), (5000 - 1440, 5000));
        }

        phase the_pointer_crosses_at_the_mapped_position_and_slides_along_blocked_edges;
        {
            let layout = Layout::resolve(&laptop_and_pi(), &[]);
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

        phase switched_off_displays_are_out_of_the_layout;
        {
            let mut devices = laptop_and_pi();
            devices[1].outputs.push(DeviceOutput {
                enabled: false,
                width: 0,
                height: 0,
                ..output("pi-dp", 0, 0, 0, 0)
            });
            let layout = Layout::resolve(&devices, &[]);
            assert!(
                layout.find("pi", "pi-dp").is_none(),
                "an off display is not reachable"
            );
            assert_eq!(layout.displays.len(), 3);
        }
    }

    tilo_test_env::scenario! {
        fn placed_entries_stay_bounded;

        phase an_entry_outside_the_bounds_is_not_valid;
        {
            assert!(arranged("a", "b", 0, 0, 0).valid());
            let mut far = arranged("a", "b", 0, MAX_COORDINATE + 1, 0);
            assert!(!far.valid());
            far.x = 0;
            far.y = i32::MIN;
            assert!(!far.valid());
            far.y = 0;
            far.order = i64::MIN;
            assert!(!far.valid());
            assert!(!arranged("", "b", 0, 0, 0).valid());
            assert!(!arranged("a", "b\n", 0, 0, 0).valid());
        }

        phase an_output_outside_the_bounds_is_not_valid;
        {
            let mut malformed_output = output("bad", i32::MIN, 0, 100, 100);
            assert!(!malformed_output.valid());
            malformed_output.x = 0;
            malformed_output.y = i32::MIN;
            assert!(!malformed_output.valid());
        }
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
