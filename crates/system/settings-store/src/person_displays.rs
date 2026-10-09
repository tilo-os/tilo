//! One person's displays across all of their devices, in one shared layout.
//!
//! Every device publishes the outputs it drives through the personal device
//! fabric ([`DeviceOutput`]). The person's [`Arrangement`] is one roaming
//! setting (`tilo.display.arrangement`): for each display, whether it takes
//! part in the shared layout, its order, and its position in one coordinate
//! space. Each display's entry carries its own revision, and devices merge
//! the arrangement entry by entry ([`Arrangement::merge`]), so edits made
//! at the same time to different displays on different devices all survive.
//!
//! Each device stays authoritative for its own hardware: modes, scale,
//! rotation and how its own outputs sit next to each other come from its
//! local display configuration. So [`Layout::resolve`] places a device's
//! outputs as one rigid group, anchored by the first of them in arrangement
//! order; the arrangement decides where the groups sit relative to each
//! other. Arranging a device's screens among themselves happens on that
//! device; arranging devices happens anywhere.
//!
//! The shell turns this layout into pointer and keyboard flow
//! ([`Layout::step_pointer`]); Settings draws and edits it. Both resolve the
//! same inputs with the same function, so what the canvas shows is where the
//! pointer goes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Settings section holding the arrangement leaf.
pub const ARRANGEMENT_SECTION: &str = "display";
/// Leaf name of the arrangement within [`ARRANGEMENT_SECTION`].
pub const ARRANGEMENT_LEAF: &str = "arrangement";
/// The roaming key, as the continuity registry names it.
pub const ARRANGEMENT_KEY: &str = "tilo.display.arrangement";
/// Bound on displays in one person's arrangement.
pub const MAX_ARRANGED_DISPLAYS: usize = 128;
/// Bound on outputs one device may publish.
pub const MAX_DEVICE_OUTPUTS: usize = 16;
/// Bound on identifier and name lengths.
pub const MAX_NAME_BYTES: usize = 128;
/// Coordinates stay well inside `i32` so every sum is safe.
pub const MAX_COORDINATE: i32 = 1_000_000;

/// One display's place in the person's shared layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrangedDisplay {
    /// Fabric device id of the device that drives the display.
    pub device: String,
    /// The device's stable output id ([`DeviceOutput::id`]).
    pub output: String,
    /// Whether the display takes part in the shared layout: the pointer
    /// can reach it from the person's other devices.
    pub enabled: bool,
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

impl ArrangedDisplay {
    /// The same entry ignoring its revision.
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arrangement {
    pub displays: Vec<ArrangedDisplay>,
}

impl Arrangement {
    /// Decode a stored arrangement, or `None` when it is malformed or out of
    /// bounds. Duplicate entries keep the highest revision.
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

    /// The arrangement in a settings document; empty when absent or invalid.
    pub fn from_document(document: &Map<String, Value>) -> Self {
        document
            .get(ARRANGEMENT_SECTION)
            .and_then(|section| section.get(ARRANGEMENT_LEAF))
            .and_then(Self::from_value)
            .unwrap_or_default()
    }

    /// The arrangement in the person's settings.
    pub fn load() -> Self {
        Self::from_document(&crate::load())
    }

    /// Store this as the person's arrangement; the device fabric carries it
    /// to their other devices.
    ///
    /// # Errors
    ///
    /// Returns an error when the settings document cannot be written.
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

    /// This arrangement with `changes` applied. Only entries that really
    /// change are written, each at a revision above every one here, so a
    /// merge with edits made elsewhere to other displays keeps both.
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

/// One output a device reports through the fabric.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceOutput {
    /// Stable monitor id: the EDID make, model and serial, with the
    /// connector only to tell identical monitors apart.
    pub id: String,
    /// What a person calls it, e.g. `Built-in display` or `Dell U2720Q`.
    pub name: String,
    pub builtin: bool,
    /// Logical rectangle in its device's own desktop, where that device's
    /// pointer moves. Zero-sized while the output is off.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// The mode it runs at, in pixels and millihertz.
    pub mode_width: u32,
    pub mode_height: u32,
    pub refresh_mhz: u32,
    /// Scale times 1000.
    pub scale_milli: u16,
    pub connected: bool,
    /// The device shows its desktop on it.
    pub enabled: bool,
}

impl DeviceOutput {
    /// Whether the output shows part of its device's desktop right now.
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

/// One of the person's devices and the outputs it reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonDevice {
    pub device: String,
    pub label: String,
    /// The device this view was produced on.
    pub local: bool,
    pub online: bool,
    pub outputs: Vec<DeviceOutput>,
}

/// An axis-aligned rectangle in logical pixels.
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

    /// Clamp a point inside, keeping it a hair short of the far edges.
    fn clamp(self, (x, y): (f64, f64)) -> (f64, f64) {
        let max_x = f64::from(self.right()) - EDGE_INSET;
        let max_y = f64::from(self.bottom()) - EDGE_INSET;
        (
            x.clamp(f64::from(self.x), max_x.max(f64::from(self.x))),
            y.clamp(f64::from(self.y), max_y.max(f64::from(self.y))),
        )
    }
}

/// How far inside a far edge a clamped pointer rests: less than a pixel, so
/// it still reads as that edge's last pixel.
const EDGE_INSET: f64 = 1.0 / 64.0;

/// One display placed in the shared layout.
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
    /// On the device this layout was resolved on.
    pub local: bool,
    pub online: bool,
    /// Takes part in the shared layout.
    pub enabled: bool,
    /// Position in display order, from zero.
    pub order: usize,
    /// Where it sits in its own device's desktop.
    pub local_rect: Rect,
    /// Where it sits in the shared layout.
    pub rect: Rect,
}

impl ResolvedDisplay {
    /// A shared-layout point as its device's desktop coordinates.
    pub fn to_device(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            x - f64::from(self.rect.x) + f64::from(self.local_rect.x),
            y - f64::from(self.rect.y) + f64::from(self.local_rect.y),
        )
    }

    /// One of its device's desktop coordinates as a shared-layout point.
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

/// Where the pointer is after one motion through the shared layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PointerStep {
    /// The new shared-layout position.
    pub shared: (f64, f64),
    /// The display it is on, as an index into [`Layout::displays`].
    pub display: usize,
    /// The same position in that display's device's desktop.
    pub device_point: (f64, f64),
}

/// Where each device's desktop sits in the shared layout: the offset from
/// its own coordinates. Devices with a placed arranged output are anchored
/// by the first of them in display order; the rest line up to the right,
/// in the order given.
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

/// A resolved display with the order key it sorts by.
type Keyed = (Option<i64>, ResolvedDisplay);

/// The person's displays resolved into one layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    /// Every active display, in display order.
    pub displays: Vec<ResolvedDisplay>,
    /// Connected displays their device has switched off: listed so they
    /// can be shown and kept in or out of the layout, but not reachable.
    /// Their rectangles are empty.
    pub inactive: Vec<ResolvedDisplay>,
}

impl Layout {
    /// Resolve the observed device desktop origins until an explicit shared
    /// arrangement has placed that device. Shell and Settings use this bridge
    /// so unarranged outputs retain the same checked coordinates in both.
    pub fn resolve_at_device_origins(
        devices: &[PersonDevice],
        arrangement: &Arrangement,
        origins: &BTreeMap<String, (i32, i32)>,
    ) -> Self {
        let mut observed = arrangement.clone();
        for device in devices {
            if device.outputs.iter().any(|output| {
                observed
                    .entry(&device.device, &output.id)
                    .is_some_and(|entry| entry.placed)
            }) {
                continue;
            }
            let Some(&(x, y)) = origins.get(&device.device) else {
                continue;
            };
            let Some(output) = device.outputs.iter().find(|output| output.active()) else {
                continue;
            };
            let mut anchor = observed
                .entry(&device.device, &output.id)
                .cloned()
                .unwrap_or_else(|| ArrangedDisplay {
                    device: device.device.clone(),
                    output: output.id.clone(),
                    enabled: true,
                    order: i64::try_from(observed.displays.len()).unwrap_or(0),
                    placed: false,
                    x: 0,
                    y: 0,
                    rev: 0,
                });
            anchor.placed = true;
            anchor.x = x.saturating_add(output.x);
            anchor.y = y.saturating_add(output.y);
            observed
                .displays
                .retain(|entry| !(entry.device == anchor.device && entry.output == anchor.output));
            observed.displays.push(anchor);
        }
        Self::resolve(devices, &observed)
    }
    /// Place every active output of `devices` per `arrangement`.
    ///
    /// A device's outputs keep their own desktop's relative layout. The
    /// first of them in display order with a position anchors the group; a
    /// device with none is put to the right of everything placed so far,
    /// devices in id order, so every device resolves the same layout from
    /// the same inputs. Displays missing from the arrangement are enabled
    /// and ordered after the arranged ones.
    pub fn resolve(devices: &[PersonDevice], arrangement: &Arrangement) -> Self {
        let mut sorted: Vec<&PersonDevice> = devices.iter().collect();
        sorted.sort_by(|a, b| a.device.cmp(&b.device));
        let offsets = device_offsets(&sorted, arrangement);

        let mut displays: Vec<Keyed> = Vec::new();
        let mut inactive: Vec<Keyed> = Vec::new();
        for device in &sorted {
            let (dx, dy) = offsets
                .get(device.device.as_str())
                .copied()
                .unwrap_or_default();
            let mut outputs: Vec<&DeviceOutput> = device
                .outputs
                .iter()
                .filter(|output| output.connected)
                .collect();
            outputs.sort_by_key(|output| (!output.active(), output.x, output.y));
            for output in outputs {
                let entry = arrangement.entry(&device.device, &output.id);
                let active = output.active();
                let resolved = ResolvedDisplay {
                    device: device.device.clone(),
                    output: output.id.clone(),
                    name: output.name.clone(),
                    device_label: device.label.clone(),
                    builtin: output.builtin,
                    local: device.local,
                    online: device.online,
                    enabled: entry.is_none_or(|entry| entry.enabled),
                    order: 0,
                    local_rect: if active {
                        output.rect()
                    } else {
                        Rect::default()
                    },
                    rect: if active {
                        output.rect().translate(dx, dy)
                    } else {
                        Rect::default()
                    },
                };
                let keyed = (entry.map(|entry| entry.order), resolved);
                if active {
                    displays.push(keyed);
                } else {
                    inactive.push(keyed);
                }
            }
        }
        // Arranged displays in their order, then the rest in device order.
        let by_order = |(order, _): &Keyed| (order.is_none(), order.unwrap_or_default());
        displays.sort_by_key(by_order);
        inactive.sort_by_key(by_order);
        let count = displays.len();
        let number = |offset: usize, list: Vec<Keyed>| -> Vec<ResolvedDisplay> {
            list.into_iter()
                .enumerate()
                .map(|(order, (_, mut display))| {
                    display.order = order.saturating_add(offset);
                    display
                })
                .collect()
        };
        Self {
            displays: number(0, displays),
            inactive: number(count, inactive),
        }
    }

    /// Every display listed, active ones first, each in display order.
    pub fn listed(&self) -> impl Iterator<Item = &ResolvedDisplay> {
        self.displays.iter().chain(&self.inactive)
    }

    pub fn find(&self, device: &str, output: &str) -> Option<&ResolvedDisplay> {
        self.displays
            .iter()
            .find(|display| display.is(device, output))
    }

    /// The bounding box of `device`'s displays in the shared layout.
    pub fn device_bounds(&self, device: &str) -> Option<Rect> {
        self.displays
            .iter()
            .filter(|display| display.device == device)
            .map(|display| display.rect)
            .reduce(Rect::union)
    }

    /// The display of `device` under one of its desktop coordinates, as an
    /// index into [`Self::displays`].
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
        // With nothing reachable there the pointer stops at this display's
        // edge and keeps the motion along it, as at the edge of one screen.
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

    /// The entry that pins `display` where this layout shows it, keeping
    /// the order key `previous` gives it, else `order`.
    fn pin(display: &ResolvedDisplay, previous: &Arrangement, order: i64) -> ArrangedDisplay {
        let entry = previous.entry(&display.device, &display.output);
        ArrangedDisplay {
            device: display.device.clone(),
            output: display.output.clone(),
            enabled: display.enabled,
            order: entry.map_or(order, |entry| entry.order),
            placed: true,
            x: display.rect.x,
            y: display.rect.y,
            rev: 0,
        }
    }

    /// Entries for every shown display `previous` does not arrange yet, in
    /// display order after the arranged ones, so an edit elsewhere does not
    /// move them.
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

    /// `device`'s displays moved together by `(dx, dy)`.
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

    /// One display taking part in the layout or not. A switched-off
    /// display keeps the choice for when it is switched on again.
    #[must_use]
    pub fn set_enabled(
        &self,
        previous: &Arrangement,
        device: &str,
        output: &str,
        enabled: bool,
    ) -> Arrangement {
        // Pinning what is not arranged yet keeps every device where it is
        // shown: an arranged display moves the line-up of the others.
        let previous = &previous.with_changes(self.pin_missing(previous));
        let order = previous.max_order().saturating_add(ORDER_GAP);
        let entry = if let Some(display) = self.find(device, output) {
            ArrangedDisplay {
                enabled,
                ..Self::pin(display, previous, order)
            }
        } else if let Some(existing) = previous.entry(device, output) {
            ArrangedDisplay {
                enabled,
                ..existing.clone()
            }
        } else {
            ArrangedDisplay {
                device: device.into(),
                output: output.into(),
                enabled,
                order,
                placed: false,
                x: 0,
                y: 0,
                rev: 0,
            }
        };
        previous.with_changes([entry])
    }

    /// One display moved to `index` in display order. Only its own order
    /// key changes, unless the keys around it have no room left.
    #[must_use]
    pub fn move_to_order(
        &self,
        previous: &Arrangement,
        device: &str,
        output: &str,
        index: usize,
    ) -> Arrangement {
        let pinned = previous.with_changes(self.pin_missing(previous));
        let Some(moving) = self.find(device, output) else {
            return pinned;
        };
        let others: Vec<(&ResolvedDisplay, i64)> = self
            .displays
            .iter()
            .filter(|display| !display.is(device, output))
            .filter_map(|display| {
                pinned
                    .entry(&display.device, &display.output)
                    .map(|entry| (display, entry.order))
            })
            .collect();
        let index = index.min(others.len());
        let below = index
            .checked_sub(1)
            .and_then(|at| others.get(at))
            .map(|(_, order)| *order);
        let above = others.get(index).map(|(_, order)| *order);
        let key = match (below, above) {
            (None, None) => Some(0),
            (Some(low), None) => Some(low.saturating_add(ORDER_GAP)),
            (None, Some(high)) => Some(high.saturating_sub(ORDER_GAP)),
            (Some(low), Some(high)) => (high.saturating_sub(low) >= 2)
                .then(|| low.saturating_add(high.saturating_sub(low) / 2)),
        };
        if let Some(order) = key {
            return pinned.with_changes([ArrangedDisplay {
                order,
                ..Self::pin(moving, &pinned, 0)
            }]);
        }
        // No room between the neighbours: space every key out again.
        let mut sequence: Vec<&ResolvedDisplay> =
            others.iter().map(|(display, _)| *display).collect();
        sequence.insert(index, moving);
        let respaced = sequence
            .into_iter()
            .zip(1_i64..)
            .map(|(display, position)| ArrangedDisplay {
                order: position.saturating_mul(ORDER_GAP),
                ..Self::pin(display, &pinned, 0)
            })
            .collect::<Vec<_>>();
        pinned.with_changes(respaced)
    }

    /// One of `device`'s displays recorded at `(x, y)` and taking the lead
    /// of its group, so the group resolves around it. Used when a device
    /// rearranges its own screens: the moved screen stays where it was
    /// dropped and the rest follow the device's layout.
    #[must_use]
    pub fn anchor_display(
        &self,
        previous: &Arrangement,
        device: &str,
        output: &str,
        x: i32,
        y: i32,
    ) -> Arrangement {
        let pinned = previous.with_changes(self.pin_missing(previous));
        let Some(display) = self.find(device, output) else {
            return pinned;
        };
        let mut changes = Vec::new();
        let mut entry = Self::pin(display, &pinned, 0);
        entry.x = x;
        entry.y = y;
        // Take the lead by swapping order keys with the current one.
        let lead = self
            .displays
            .iter()
            .filter(|other| other.device == device && !other.is(device, output))
            .filter_map(|other| {
                pinned
                    .entry(&other.device, &other.output)
                    .filter(|other| other.placed && other.order < entry.order)
            })
            .min_by_key(|other| other.order);
        if let Some(lead) = lead {
            changes.push(ArrangedDisplay {
                order: entry.order,
                ..lead.clone()
            });
            entry.order = lead.order;
        }
        changes.push(entry);
        pinned.with_changes(changes)
    }
}

/// Current version of [`PersonDisplays`].
pub const SCHEMA_VERSION: u16 = 1;

/// The shell's read-only view of the person's devices and their displays,
/// published for Settings. The shell owns the authenticated fabric; Settings
/// reads this and edits only the arrangement.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonDisplays {
    pub schema_version: u16,
    pub publisher_pid: u32,
    pub publisher_start_ticks: u64,
    pub local_device: String,
    pub devices: Vec<PersonDevice>,
}

impl PersonDisplays {
    /// The live publisher's view, or `None` when no live shell published
    /// one (no fabric identity, or the shell is gone).
    pub fn load() -> Option<Self> {
        let path = path()?;
        Self::load_at(&path)
    }

    pub fn load_at(path: &Path) -> Option<Self> {
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

    /// # Errors
    ///
    /// Returns an error when the runtime directory is unavailable or the
    /// file cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is unavailable"))?;
        self.save_at(&path)
    }

    /// # Errors
    ///
    /// Returns an error when the file cannot be written.
    pub fn save_at(&self, path: &Path) -> std::io::Result<()> {
        let mut body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        body.push(b'\n');
        tilo_files::native::atomic::write_creating_parent(path, &body)
    }

    /// The person's layout as resolved from this view and `arrangement`.
    pub fn layout(&self, arrangement: &Arrangement) -> Layout {
        Layout::resolve(&self.devices, arrangement)
    }
}

/// Where the shell publishes [`PersonDisplays`]: beside the display
/// hardware snapshot in the private runtime directory.
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

    fn arranged(
        device: &str,
        output: &str,
        enabled: bool,
        order: i64,
        x: i32,
        y: i32,
    ) -> ArrangedDisplay {
        ArrangedDisplay {
            device: device.into(),
            output: output.into(),
            enabled,
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
        assert!(layout.displays.iter().all(|d| d.enabled));
        assert_eq!(
            layout.displays.iter().map(|d| d.order).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn a_device_keeps_its_own_screen_layout_around_its_anchor() {
        let arrangement = Arrangement {
            displays: vec![
                arranged("pi", "pi-hdmi", true, 1, -1920, 100),
                arranged("laptop", "hdmi", false, 2, 5000, 5000),
            ],
        };
        let layout = Layout::resolve(&laptop_and_pi(), &arrangement);
        // Arranged order first, then the rest.
        let names: Vec<_> = layout.displays.iter().map(|d| d.output.as_str()).collect();
        assert_eq!(names, ["pi-hdmi", "hdmi", "panel"]);
        // The laptop's HDMI anchors its group; the panel keeps sitting left
        // of it, whatever position was stored for it.
        let hdmi = layout.find("laptop", "hdmi").unwrap();
        let panel = layout.find("laptop", "panel").unwrap();
        assert_eq!((hdmi.rect.x, hdmi.rect.y), (5000, 5000));
        assert_eq!((panel.rect.x, panel.rect.y), (5000 - 1440, 5000));
        assert!(!hdmi.enabled);
        assert!(panel.enabled);
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
        // Rightward motion past the laptop's right edge lands on the Pi,
        // carrying the part of the motion beyond the edge.
        let from = (3359.5, 500.25);
        let step = layout
            .step_pointer(hdmi, from, (4.0, 1.0), |_| true)
            .unwrap();
        assert_eq!(step.display, pi);
        assert_eq!(step.shared, (3363.5, 501.25));
        assert_eq!(step.device_point, (3.5, 501.25));
        // And back.
        let back = layout
            .step_pointer(pi, step.shared, (-8.0, 0.0), |_| true)
            .unwrap();
        assert_eq!(back.display, hdmi);
        assert_eq!(back.device_point, (3355.5, 501.25));
        // An unreachable neighbour is a wall the pointer slides along.
        let blocked = layout
            .step_pointer(hdmi, from, (4.0, 3.0), |d| d.device != "pi")
            .unwrap();
        assert_eq!(blocked.display, hdmi);
        assert_eq!(blocked.shared.1, 503.25, "motion along the edge is kept");
        assert!(
            blocked.shared.0 < 3360.0 && blocked.shared.0 > 3359.9,
            "rests at the edge"
        );
        // Below the shorter panel there is nothing: it stays on the panel.
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
            displays: vec![arranged("tv", "tv-panel", true, 0, -4000, 0)],
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

        // A toggle while some displays are not arranged yet keeps every
        // device where it is shown.
        let first = layout.set_enabled(&previous, "pi", "pi-hdmi", false);
        assert_eq!(
            Layout::resolve(&devices, &first)
                .displays
                .iter()
                .map(|d| d.rect)
                .collect::<Vec<_>>(),
            layout.displays.iter().map(|d| d.rect).collect::<Vec<_>>()
        );

        let disabled = again.set_enabled(&moved, "pi", "pi-hdmi", false);
        assert!(!disabled.entry("pi", "pi-hdmi").unwrap().enabled);
        let changed: Vec<_> = disabled
            .displays
            .iter()
            .filter(|entry| moved.entry(&entry.device, &entry.output) != Some(*entry))
            .map(|entry| entry.output.as_str())
            .collect();
        assert_eq!(changed, ["pi-hdmi"], "a toggle writes only its own entry");

        let reordered = again.move_to_order(&moved, "pi", "pi-hdmi", 0);
        let resolved = Layout::resolve(&devices, &reordered);
        assert_eq!(resolved.displays[0].output, "pi-hdmi");
        assert_eq!(resolved.find("pi", "pi-hdmi").unwrap().rect.x, -1920);
        let changed: Vec<_> = reordered
            .displays
            .iter()
            .filter(|entry| moved.entry(&entry.device, &entry.output) != Some(*entry))
            .map(|entry| entry.output.as_str())
            .collect();
        assert_eq!(changed, ["pi-hdmi"], "a reorder writes only its own entry");
    }

    #[test]
    fn concurrent_edits_to_different_displays_both_survive_the_merge() {
        let devices = laptop_and_pi();
        let base = Layout::resolve(&devices, &Arrangement::default());
        let shared = base.move_device(&Arrangement::default(), "pi", 0, 0);
        let layout = Layout::resolve(&devices, &shared);
        // On the laptop, the Pi is taken out of the layout; at the same
        // time, on the Pi, the laptop's HDMI moves to the front.
        let on_laptop = layout.set_enabled(&shared, "pi", "pi-hdmi", false);
        let on_pi = layout.move_to_order(&shared, "laptop", "hdmi", 0);
        let merged = Arrangement::merge([("laptop", on_laptop.clone()), ("pi", on_pi.clone())]);
        let resolved = Layout::resolve(&devices, &merged);
        assert!(!resolved.find("pi", "pi-hdmi").unwrap().enabled);
        assert_eq!(resolved.displays[0].output, "hdmi");
        // Merging is order independent and idempotent.
        assert_eq!(
            merged,
            Arrangement::merge([("pi", on_pi), ("laptop", on_laptop)])
        );
        assert_eq!(merged, Arrangement::merge([("x", merged.clone())]));
        // The same display edited on both: the higher revision wins, and a
        // tie goes to the same author everywhere.
        let a = shared.with_changes([arranged("pi", "pi-hdmi", true, 0, 1, 1)]);
        let b = shared.with_changes([arranged("pi", "pi-hdmi", true, 0, 2, 2)]);
        let winner = Arrangement::merge([("aa", a.clone()), ("bb", b.clone())]);
        assert_eq!(winner, Arrangement::merge([("bb", b), ("aa", a)]));
        assert_eq!(winner.entry("pi", "pi-hdmi").unwrap().x, 2);
    }

    #[test]
    fn switched_off_displays_are_listed_and_keep_their_choice() {
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
        let off = &layout.inactive[0];
        assert_eq!((off.output.as_str(), off.order), ("pi-dp", 3));
        let arrangement = layout.set_enabled(&Arrangement::default(), "pi", "pi-dp", false);
        let entry = arrangement.entry("pi", "pi-dp").unwrap();
        assert!(!entry.enabled && !entry.placed);
        // Switched on, it is out of the layout and does not move its device.
        devices[1].outputs[1] = output("pi-dp", 1920, 0, 1920, 1080);
        let on = Layout::resolve(&devices, &arrangement);
        assert!(!on.find("pi", "pi-dp").unwrap().enabled);
        assert_eq!(on.find("pi", "pi-hdmi").unwrap().rect.x, 3360);
    }

    #[test]
    fn a_rearranged_screen_anchors_its_device_where_it_was_dropped() {
        let devices = laptop_and_pi();
        let layout = Layout::resolve(&devices, &Arrangement::default());
        // The laptop rearranges: HDMI now sits left of the panel locally.
        let mut rearranged = devices;
        rearranged[0].outputs[1].x = -1920;
        let arrangement =
            layout.anchor_display(&Arrangement::default(), "laptop", "hdmi", -1920, 0);
        let after = Layout::resolve(&rearranged, &arrangement);
        assert_eq!(after.find("laptop", "hdmi").unwrap().rect.x, -1920);
        assert_eq!(after.find("laptop", "panel").unwrap().rect.x, 0);
        assert_eq!(after.find("pi", "pi-hdmi").unwrap().rect.x, 3360);
    }

    #[test]
    fn stored_arrangements_are_bounded_and_deduplicated() {
        let entry = serde_json::json!({"device": "a", "output": "b", "enabled": true,
            "order": 0, "placed": true, "x": 1, "y": 2, "rev": 1});
        let value = serde_json::json!({"displays": [entry, entry]});
        assert_eq!(Arrangement::from_value(&value).unwrap().displays.len(), 1);
        let far = serde_json::json!({"displays": [{"device": "a", "output": "b", "enabled": true,
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
            "display".into(),
            serde_json::json!({"arrangement": value, "scale": 1.0}),
        );
        assert_eq!(Arrangement::from_document(&document).displays.len(), 1);
    }
}
