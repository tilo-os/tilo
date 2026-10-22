//! The window's content, built from the toolkit's widgets.
//!
//! The drive list is a [`NavList`] in the shell's sidebar; the detail is one
//! scrolling page of sections, property lists, stat tiles and tables. Nothing
//! here paints, sizes or colours anything: every control says what it is and
//! the theme decides how it looks. Interaction leaves as an [`Intent`] the
//! app applies to the window and the machine.

use std::cell::Cell;
use std::rc::Rc;

use ui::format::size;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, Chip, ChipStyle, ContextMenuArea, Element, EmptyState, ListRow, MenuEntry, NavItem,
    NavList, NavSection, Property, PropertyList, Row, RowStatus, Scrollable, Section,
    SegmentedStyle, StatGroup, StatTile, Text, ToggleGroup, Tone,
};
use ui::window_toolbar::{LocationSegment, ToolbarAction, ToolbarLocation, WindowToolbar};

use crate::app::{ActionKind, Tab};
use crate::map::{MapEntry, PartitionMap};
use crate::model::{
    AttrSeverity, Drive, DriveKind, Partition, SmartAttr, SmartInfo, SmartStatus, fmt_hours,
};
use crate::offer::{self, Offer};

/// What a control in the window asks for. The app applies each one to the
/// window and the machine, the same way for a click, a key or an
/// accessibility action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    SelectDrive(usize),
    /// A partition of the selected drive, or `None` for the whole disk.
    SelectPartition(Option<usize>),
    SetTab(Tab),
    /// An operation on the current selection (the window bar, the header).
    Run(ActionKind),
    /// An operation on partition `partition` of the selected drive (`None`:
    /// the whole drive), from that row's context menu.
    RunOn {
        action: ActionKind,
        partition: Option<usize>,
    },
    Rescan,
}

/// The queue controls write to and the app drains.
pub(crate) type Intents = ui::Intents<Intent>;

/// Everything the detail page shows, borrowed from the app.
pub(crate) struct DetailModel<'a> {
    pub drive: &'a Drive,
    pub tab: Tab,
    pub partition: Option<usize>,
    /// The drive's name over the page, when the header names a partition
    /// of it and no window bar shows the path.
    pub drive_line: Option<String>,
}

/// The bar action that rescans the machine.
pub(crate) const RESCAN: &str = "rescan";
/// What the location's drive level commits: back to the whole drive.
pub(crate) const WHOLE_DRIVE: &str = "drive";

/// Where the window is: the drive, and the partition of it when one is
/// chosen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Location {
    pub drive: String,
    pub partition: Option<String>,
}

impl Location {
    /// What the header and the bar name at rest: the deepest level.
    pub(crate) fn title(&self) -> &str {
        self.partition.as_deref().unwrap_or(&self.drive)
    }
}

/// The icon an operation shows, by its icon-set name.
fn action_icon(action: ActionKind) -> Icon {
    match action {
        ActionKind::Mount => Icon::Link,
        ActionKind::Unmount => Icon::Unlink,
        ActionKind::Format => Icon::Eraser,
        ActionKind::Benchmark => Icon::Gauge,
        ActionKind::Eject => Icon::Power,
    }
}

/// What an operation's control says it does: its outcome, or why it cannot.
fn action_name(action: ActionKind, offer: Offer) -> String {
    match offer {
        Offer::Unavailable(reason) => format!("{}: unavailable. {reason}", action.title()),
        Offer::Available | Offer::Hidden => action.describe().to_owned(),
    }
}

/// The selection's operations as the content header's buttons, for a
/// window no desktop bar hosts (Rescan stays in the drive list's header).
pub(crate) fn header_actions(offers: &[(ActionKind, Offer)], intents: &Intents) -> Vec<Element> {
    offers
        .iter()
        .map(|&(action, offer)| -> Element {
            let intents = intents.clone();
            Box::new(
                Button::icon_only(
                    &format!("disks-action-{}", action.key()),
                    action_icon(action),
                    action.label(),
                )
                .with_enabled(offer.is_available())
                .tooltip(action_name(action, offer))
                .on_effect(move || intents.push(Intent::Run(action))),
            )
        })
        .collect()
}

/// The window bar's slots: the sidebar toggle, where the window is (the
/// drive, and the partition when one is chosen), and the selection's
/// operations followed by Rescan.
pub(crate) fn window_toolbar(
    selection: Option<(Location, Vec<(ActionKind, Offer)>)>,
    sidebar_open: bool,
) -> WindowToolbar {
    let (location, offers) = selection.unwrap_or_default();
    let mut actions: Vec<ToolbarAction> = offers
        .into_iter()
        .map(|(action, offer)| ToolbarAction {
            id: action.key().to_owned(),
            icon: action_icon(action).name().to_owned(),
            name: action_name(action, offer),
            enabled: offer.is_available(),
            ..ToolbarAction::default()
        })
        .collect();
    actions.push(ToolbarAction {
        id: RESCAN.to_owned(),
        icon: Icon::Refresh.name().to_owned(),
        name: "Rescan drives".to_owned(),
        enabled: true,
        ..ToolbarAction::default()
    });
    WindowToolbar {
        sidebar: Some(sidebar_open),
        location: (!location.drive.is_empty()).then(|| ToolbarLocation {
            label: location.title().to_owned(),
            // A partition is a level under its drive: the path goes back up.
            segments: match &location.partition {
                Some(partition) => vec![
                    LocationSegment {
                        label: location.drive.clone(),
                        value: WHOLE_DRIVE.to_owned(),
                    },
                    LocationSegment {
                        label: partition.clone(),
                        value: "partition".to_owned(),
                    },
                ],
                None => Vec::new(),
            },
            ..ToolbarLocation::default()
        }),
        actions,
        ..WindowToolbar::default()
    }
}

/// A context menu of what `part` of `drive` offers (`partition`: its
/// index, `None` for the whole drive): each operation it shows, and under an
/// unavailable one, why.
fn operations_menu(
    drive: &Drive,
    partition: Option<usize>,
    intents: &Intents,
) -> impl Fn(Option<ui::vello::kurbo::Point>) -> Vec<MenuEntry> + 'static {
    let part = partition.and_then(|i| drive.partitions.get(i));
    let offers = offer::shown(drive, part);
    let intents = intents.clone();
    move |_| {
        let mut entries = Vec::new();
        for &(action, offer) in &offers {
            let intents = intents.clone();
            let run = move || intents.push(Intent::RunOn { action, partition });
            let entry = if action.destructive() {
                MenuEntry::destructive(action.label(), run)
            } else {
                MenuEntry::new(action.label(), run)
            };
            entries.push(entry.with_icon(action_icon(action)).with_enabled(offer.is_available()));
            if let Offer::Unavailable(reason) = offer {
                entries.push(MenuEntry::caption(reason));
            }
        }
        entries
    }
}

/// The glyph of a drive: a stick for USB, the drive for the rest.
pub(crate) fn drive_icon(drive: &Drive) -> Icon {
    match drive.kind {
        DriveKind::Usb => Icon::Usb,
        _ if drive.removable => Icon::SdCard,
        DriveKind::Zram | DriveKind::Virtual => Icon::Layers,
        _ => Icon::HardDrive,
    }
}

/// The drive list: one destination per drive under the name it shows, its
/// size beside it and a dot when its health needs the person.
pub(crate) fn drive_list(
    drives: &[(&Drive, String)],
    selected: usize,
    intents: &Intents,
) -> NavList {
    let items = drives
        .iter()
        .enumerate()
        .map(|(index, (drive, label))| {
            let item = NavItem::new(drive_id(index), label.clone())
                .icon(drive_icon(drive))
                .meta(size(drive.size))
                .without_actions();
            match drive.smart_status() {
                SmartStatus::Warning => item.status(RowStatus::Warning),
                SmartStatus::Failing => item.status(RowStatus::Error),
                SmartStatus::Passed | SmartStatus::Unknown => item,
            }
        })
        .collect();
    let intents = intents.clone();
    NavList::new(vec![NavSection::untitled("drives", items)])
        .selected(&drive_id(selected))
        .with_accessible_name("Drives")
        .on_select(move |id| {
            if let Some(index) = id.strip_prefix("drive:").and_then(|n| n.parse().ok()) {
                intents.push(Intent::SelectDrive(index));
            }
        })
}

fn drive_id(index: usize) -> String {
    format!("drive:{index}")
}

/// The sidebar header's rescan control.
pub(crate) fn rescan_button(intents: &Intents) -> Button {
    let intents = intents.clone();
    Button::icon_only("disks-rescan", Icon::Refresh, "Rescan drives")
        .on_effect(move || intents.push(Intent::Rescan))
}

/// Nothing to show: the system reported no block devices.
/// No drives to show: none attached, or (with `unreadable`, the reason) the
/// machine's storage could not be read.
pub(crate) fn empty_state(unreadable: Option<&str>, intents: &Intents) -> Element {
    let state = match unreadable {
        Some(reason) => EmptyState::new("Storage can't be read")
            .with_icon(Icon::HardDrive)
            .with_message(reason),
        None => EmptyState::new("No drives found")
            .with_icon(Icon::HardDrive)
            .with_message("Attach a drive, then rescan."),
    };
    Box::new(
        state
            .with_action("Rescan")
            .on_effect(intents.sender(Intent::Rescan)),
    )
}

/// The detail of the selected drive: its state at a glance and one tab of
/// detail. Its operations are in the window bar (or the content header) and
/// each row's context menu.
pub(crate) fn detail(
    model: &DetailModel<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
) -> Element {
    let drive = model.drive;
    // The drive's state, its actions and the tabbed detail are groups of
    // the page, a group apart; a tab sits a sibling step over its content.
    let tab = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(tabs(model.tab, intents)))
        .push(match model.tab {
            Tab::Overview => overview(model, intents),
            Tab::Smart => health(drive),
            Tab::Partitions => partitions(model, intents),
        });
    let mut page = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group);
    if let Some(line) = &model.drive_line {
        page = page.push(Box::new(
            Text::new(line).with_role(TextRole::Caption).dimmed(),
        ));
    }
    let page = page
        .push(Box::new(badges(drive)))
        .push(Box::new(tab));
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_persisted_offset(Rc::clone(offset))
            .with_child(Box::new(page)),
    )
}

fn status_tone(status: SmartStatus) -> Tone {
    match status {
        SmartStatus::Passed => Tone::Success,
        SmartStatus::Warning => Tone::Warning,
        SmartStatus::Failing => Tone::Danger,
        SmartStatus::Unknown => Tone::Neutral,
    }
}

/// The drive's state at a glance: its health when it reports any, what kind
/// of device it is, and whether it can be pulled out.
fn badges(drive: &Drive) -> Row {
    let status = drive.smart_status();
    let mut row = Row::new();
    // A device that reports nothing has no health to show; the Health tab
    // says why.
    if status != SmartStatus::Unknown {
        row = row.push(Box::new(
            Chip::new(status.label()).style(ChipStyle::Tinted(status_tone(status))),
        ));
    }
    row = row.push(Box::new(Chip::new(drive.kind.tag())));
    if drive.removable {
        row = row.push(Box::new(Chip::new("Removable")));
    }
    row
}

fn tabs(selected: Tab, intents: &Intents) -> ToggleGroup {
    let intents = intents.clone();
    let all = Tab::all();
    let index = all.iter().position(|tab| *tab == selected).unwrap_or(0);
    ToggleGroup::new(
        all.iter().map(|tab| Segment::new(tab.title())).collect(),
        index,
    )
    .with_style(SegmentedStyle::Tabs)
    .on_view("disks-tabs", move |i| {
        if let Some(tab) = Tab::all().get(i) {
            intents.push(Intent::SetTab(*tab));
        }
    })
}

// ---- Overview ---------------------------------------------------------

fn overview(model: &DetailModel<'_>, intents: &Intents) -> Element {
    let drive = model.drive;
    let mut entries: Vec<MapEntry> = drive
        .partitions
        .iter()
        .enumerate()
        .map(|(index, part)| MapEntry {
            partition: Some(index),
            device: part.name.clone(),
            label: format!("{} {}", part.display_label(), size(part.size)),
            size: part.size,
            used: part.used_frac(),
        })
        .collect();
    if drive.free_tail() > 0 {
        entries.push(MapEntry {
            partition: None,
            device: String::new(),
            label: format!("Unallocated {}", size(drive.free_tail())),
            size: drive.free_tail(),
            used: None,
        });
    }
    let pick = intents.clone();
    let map = PartitionMap::new(entries, model.partition, move |part| {
        pick.push(Intent::SelectPartition(part));
    });

    let mut page = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Group);
    page = page.push(Box::new(Section::titled(
        "Partition map",
        vec![Box::new(map)],
    )));
    page = page.push(Box::new(filesystems(drive, model.partition, intents)));
    page = page.push(Box::new(Section::titled(
        "Details",
        vec![Box::new(PropertyList::new(drive_facts(drive)))],
    )));
    Box::new(page)
}

fn filesystems(drive: &Drive, selected: Option<usize>, intents: &Intents) -> Section {
    let mut items: Vec<Element> = drive
        .partitions
        .iter()
        .enumerate()
        .filter(|(_, part)| part.mountpoint.is_some())
        .map(|(index, part)| -> Element {
            Box::new(ContextMenuArea::new(
                Box::new(filesystem(index, part, selected == Some(index), intents)),
                operations_menu(drive, Some(index), intents),
            ))
        })
        .collect();
    if items.is_empty() {
        items.push(Box::new(
            Text::new("No filesystems are mounted.")
                .with_role(TextRole::Body)
                .dimmed(),
        ));
    }
    Section::titled("Filesystems", items)
}

/// Share of a filesystem in use above which it needs attention.
const NEARLY_FULL: f64 = 0.9;
/// Share above which there is hardly any room left.
const FULL: f64 = 0.97;

/// A mounted filesystem: what it is, where it is, and how much room is left.
fn filesystem(index: usize, part: &Partition, selected: bool, intents: &Intents) -> ListRow {
    let pick = intents.clone();
    let secondary = match (part.mount_display(), &part.fstype) {
        (Some(mount), _) if mount == "Swap" => "Swap space".to_owned(),
        (Some(mount), Some(fstype)) if !fstype.is_empty() => format!("{fstype} at {mount}"),
        (Some(mount), _) => format!("At {mount}"),
        (None, _) => String::new(),
    };
    let mut row = ListRow::new(format!("disks-filesystem-{index}"), part.display_label())
        .icon(Icon::Layers)
        .secondary(secondary)
        .selected(selected)
        .on_view(move || pick.push(Intent::SelectPartition(Some(index))));
    if let Some(used) = part.used {
        row = row.meta(format!("{} free", size(part.size.saturating_sub(used))));
    }
    match part.used_frac() {
        Some(used) if used >= FULL => row.error(),
        Some(used) if used >= NEARLY_FULL => row.warning(),
        _ => row,
    }
}

fn interface_name(drive: &Drive) -> String {
    match drive.tran.as_deref() {
        Some("nvme") => "NVMe".into(),
        Some("sata") => "SATA".into(),
        Some("usb") => "USB".into(),
        Some(other) => other.to_owned(),
        None => "Not reported".into(),
    }
}

fn drive_facts(drive: &Drive) -> Vec<Property> {
    let mut facts = vec![
        Property::new("Device", format!("/dev/{}", drive.name)).with_kind(TextKind::Identifier),
        Property::new("Type", drive.kind.long()),
        Property::new("Capacity", size(drive.size)).with_kind(TextKind::Numeric),
        Property::new("Interface", interface_name(drive)),
    ];
    if let Some(serial) = &drive.serial {
        facts.push(Property::new("Serial number", serial.clone()).with_kind(TextKind::Identifier));
    }
    if let Some(use_) = drive.system_use() {
        facts.push(Property::new("In use", use_));
    }
    facts
}

// ---- Health -----------------------------------------------------------

fn health(drive: &Drive) -> Element {
    let status = drive.smart_status();
    let generic = match status {
        SmartStatus::Passed => "All monitored attributes are within normal limits.",
        SmartStatus::Warning => "Some attributes need attention. Back up important data.",
        SmartStatus::Failing => {
            "The drive reports imminent failure. Replace it as soon as possible."
        }
        SmartStatus::Unknown => "This device does not report health data.",
    };
    // An unknown verdict is best explained by the probe's own reason
    // (privileges, a USB bridge, a virtual device).
    let message = match (&drive.smart, status) {
        (
            Some(SmartInfo {
                detail: Some(detail),
                ..
            }),
            SmartStatus::Unknown,
        ) => detail.as_str(),
        _ => generic,
    };

    let mut page = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Group);
    page = page.push(Box::new(Text::new(message).with_role(TextRole::Body)));

    if let Some(smart) = &drive.smart {
        let tiles = smart_tiles(smart);
        if !tiles.is_empty() {
            page = page.push(Box::new(StatGroup::new(tiles)));
        }
        if !smart.attrs.is_empty() {
            page = page.push(Box::new(Section::titled(
                "Attributes",
                vec![Box::new(attribute_table(&smart.attrs))],
            )));
        }
    }
    Box::new(page)
}

/// A tile that carries a status colour only when its value means something.
fn tile(tile: StatTile, tone: Tone) -> Element {
    match tone {
        Tone::Neutral => Box::new(tile),
        tone => Box::new(tile.tone(tone)),
    }
}

fn smart_tiles(smart: &SmartInfo) -> Vec<Element> {
    let mut tiles: Vec<Element> = Vec::new();
    if let Some(temp) = smart.temp_c {
        let tone = if temp >= 60 {
            Tone::Danger
        } else if temp >= 48 {
            Tone::Warning
        } else {
            Tone::Neutral
        };
        tiles.push(tile(
            StatTile::new("Temperature", temp.to_string()).unit("°C"),
            tone,
        ));
    }
    if let Some(hours) = smart.power_on_hours {
        tiles.push(Box::new(StatTile::new("Power-on time", fmt_hours(hours))));
    }
    if let Some(cycles) = smart.power_cycles {
        tiles.push(Box::new(StatTile::new("Power cycles", cycles.to_string())));
    }
    if let Some(sectors) = smart.reallocated {
        let tone = if sectors > 0 {
            Tone::Warning
        } else {
            Tone::Neutral
        };
        tiles.push(tile(
            StatTile::new("Reallocated sectors", sectors.to_string()),
            tone,
        ));
    }
    if let Some(used) = smart.pct_used {
        tiles.push(Box::new(StatTile::new("Wear", used.to_string()).unit("%")));
    }
    if let Some(written) = smart.written_gb {
        tiles.push(Box::new(StatTile::new(
            "Host writes",
            size((written.max(0) as u64).saturating_mul(1_000_000_000)),
        )));
    }
    tiles
}

fn attribute_table(attrs: &[SmartAttr]) -> DataTable<SmartAttr> {
    // Only ATA numbers its attributes; an NVMe drive's log has no ids.
    let mut columns = Vec::new();
    if attrs.iter().any(|attr| attr.id.is_some()) {
        columns.push(
            ColumnDef::new("id", "ID", ColumnWidth::Auto)
                .with_kind(TextKind::Numeric)
                .with_text(|attr: &SmartAttr| {
                    attr.id.map(|id| id.to_string()).unwrap_or_default()
                }),
        );
    }
    columns.extend([
        ColumnDef::new("name", "Attribute", ColumnWidth::Flex(1.0))
            .with_text(|attr: &SmartAttr| attr.name.clone()),
        ColumnDef::new("value", "Value", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|attr: &SmartAttr| attr.value.clone()),
        ColumnDef::new("status", "Status", ColumnWidth::Auto)
            .with_text(|attr: &SmartAttr| severity_label(attr.severity).to_owned()),
    ]);
    let mut table = DataTable::new(columns)
    .with_id("disks-attributes")
    .with_accessible_name("SMART attributes");
    table.set_rows(attrs.to_vec());
    table
}

fn severity_label(severity: AttrSeverity) -> &'static str {
    match severity {
        AttrSeverity::Ok => "OK",
        AttrSeverity::Warn => "Warning",
        AttrSeverity::Crit => "Critical",
    }
}

// ---- Partitions -------------------------------------------------------

/// What a partition's row says under its name: its device when the name is
/// not the device's, what it holds when that is not the name either, its
/// filesystem, where that is mounted, and what of the running system it
/// holds (why it is not formatted or unmounted here).
fn partition_summary(part: &Partition, title: &str, serves: Option<&str>) -> String {
    let mut summary = partition_facts(part, title);
    if let Some(serves) = serves {
        let mut chars = serves.chars();
        if let Some(first) = chars.next() {
            summary.push_str(", ");
            summary.extend(first.to_lowercase());
            summary.push_str(chars.as_str());
        }
    }
    summary
}

fn partition_facts(part: &Partition, title: &str) -> String {
    let mut facts: Vec<String> = Vec::new();
    if title != part.name {
        facts.push(part.name.clone());
    }
    facts.extend(part.fstype.clone().filter(|fs| !fs.is_empty()));
    if let Some(encryption) = &part.encryption {
        let locked = encryption.unlocked_as.is_none();
        facts.push(match (facts.is_empty(), locked) {
            (true, true) => format!("Encrypted with {}, locked", encryption.scheme),
            (false, true) => format!("encrypted with {}, locked", encryption.scheme),
            (true, false) => format!("Encrypted with {}", encryption.scheme),
            (false, false) => format!("encrypted with {}", encryption.scheme),
        });
        if locked {
            return facts.join(", ");
        }
    }
    let sentence_start = facts.is_empty();
    facts.push(match (part.mount_display(), sentence_start) {
        (Some(mount), true) if mount == "Swap" => "In use".to_owned(),
        (Some(mount), false) if mount == "Swap" => "in use".to_owned(),
        (Some(mount), true) => format!("Mounted at {mount}"),
        (Some(mount), false) => format!("mounted at {mount}"),
        (None, true) => "Not mounted".to_owned(),
        (None, false) => "not mounted".to_owned(),
    });
    facts.join(", ")
}

fn partitions(model: &DetailModel<'_>, intents: &Intents) -> Element {
    let drive = model.drive;
    if drive.partitions.is_empty() {
        return Box::new(Section::new(vec![Box::new(
            Text::new("This drive has no partitions.")
                .with_role(TextRole::Body)
                .dimmed(),
        )]));
    }
    let rows: Vec<Element> = drive
        .partitions
        .iter()
        .enumerate()
        .map(|(index, part)| -> Element {
            let title = part.display_label();
            let summary = partition_summary(part, &title, drive.partition_use(part));
            let pick = intents.clone();
            let row = ListRow::new(format!("disks-partition-{index}"), title)
                .icon(Icon::Layers)
                .secondary(summary)
                .meta(size(part.size))
                .selected(model.partition == Some(index))
                .on_view(move || pick.push(Intent::SelectPartition(Some(index))));
            Box::new(ContextMenuArea::new(
                Box::new(row),
                operations_menu(drive, Some(index), intents),
            ))
        })
        .collect();
    Box::new(Section::new(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window bar shows the system drive's Format as unavailable and
    /// says why; a fixed internal drive has no Eject in it at all.
    #[test]
    fn the_window_bar_says_why_the_system_drive_is_not_formatted() {
        let drive = crate::probe::captured::yoga().unwrap().drives.remove(0);
        let location = Location {
            drive: drive.model.clone(),
            partition: None,
        };
        let bar = window_toolbar(Some((location, offer::shown(&drive, None))), true);
        let format = bar
            .actions
            .iter()
            .find(|action| action.id == ActionKind::Format.key())
            .expect("Format is shown");
        assert!(!format.enabled);
        assert!(format.name.contains(crate::model::HOLDS_SYSTEM), "{}", format.name);
        assert!(bar.actions.iter().all(|action| action.id != ActionKind::Eject.key()));
    }

    /// A chosen partition is a level under its drive in the bar's location.
    #[test]
    fn a_partition_is_a_level_under_its_drive() {
        let location = Location {
            drive: "SAMSUNG".into(),
            partition: Some("home".into()),
        };
        let bar = window_toolbar(Some((location, Vec::new())), true);
        let location = bar.location.expect("a location");
        assert_eq!(location.label, "home");
        let levels: Vec<_> = location.segments.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(levels, ["SAMSUNG", "home"]);
        assert_eq!(location.segments[0].value, WHOLE_DRIVE);
    }
}
