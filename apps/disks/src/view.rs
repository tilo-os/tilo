//! The window's content, built from the toolkit's widgets.
//!
//! The drive list is a [`NavList`] in the shell's sidebar; the detail is one
//! scrolling page of sections, property lists, stat tiles and tables. Nothing
//! here paints, sizes or colours anything: every control says what it is and
//! the theme decides how it looks. Interaction leaves as an [`Intent`] the
//! app applies to the window and the machine.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::format::bytes_si;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Button, ButtonRole, Chip, ChipStyle, Element, EmptyState, ListRow, NavItem, NavList,
    NavSection, Property, PropertyList, Row, RowStatus, Scrollable, Section, SegmentedStyle,
    StatGroup, StatTile, Text, ToggleGroup, Tone,
};

use crate::app::{ActionKind, Tab};
use crate::map::{MapEntry, PartitionMap};
use crate::model::{
    AttrSeverity, Drive, DriveKind, Partition, SmartAttr, SmartInfo, SmartStatus, fmt_hours,
};

/// What a control in the window asks for. The app applies each one to the
/// window and the machine, the same way for a click, a key or an
/// accessibility action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    SelectDrive(usize),
    /// A partition of the selected drive, or `None` for the whole disk.
    SelectPartition(Option<usize>),
    SetTab(Tab),
    Run(ActionKind),
    Rescan,
}

/// The queue controls write to and the app drains.
#[derive(Clone, Default)]
pub(crate) struct Intents(Rc<RefCell<Vec<Intent>>>);

impl Intents {
    pub(crate) fn push(&self, intent: Intent) {
        self.0.borrow_mut().push(intent);
    }

    pub(crate) fn take(&self) -> Vec<Intent> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// Everything the detail page shows, borrowed from the app.
pub(crate) struct DetailModel<'a> {
    pub drive: &'a Drive,
    pub tab: Tab,
    pub partition: Option<usize>,
    /// Which of [`ActionKind::bar`] apply to the current selection.
    pub enabled: [bool; 5],
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
                .meta(bytes_si(drive.size))
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
pub(crate) fn empty_state(intents: &Intents) -> Element {
    let intents = intents.clone();
    Box::new(
        EmptyState::new("No drives found")
            .with_icon(Icon::HardDrive)
            .with_message("Attach a drive, then rescan.")
            .with_action("Rescan")
            .on_effect(move || intents.push(Intent::Rescan)),
    )
}

/// The detail of the selected drive: its state at a glance, the actions that
/// apply, and one tab of detail.
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
    let page = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group)
        .push(Box::new(badges(drive)))
        .push(Box::new(actions(model, intents)))
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

/// What the action buttons act on, in words.
fn target_line(drive: &Drive, partition: Option<usize>) -> String {
    match partition.and_then(|i| drive.partitions.get(i)) {
        Some(part) => format!("Applies to {} ({})", part.display_label(), part.name),
        None => format!("Applies to the whole disk ({})", drive.name),
    }
}

fn actions(model: &DetailModel<'_>, intents: &Intents) -> Section {
    let mut row = Row::new();
    for (kind, enabled) in ActionKind::bar().into_iter().zip(model.enabled) {
        let intents = intents.clone();
        let button = Button::new(&format!("disks-action-{}", kind.key()), kind.label())
            .role(if kind.destructive() {
                ButtonRole::Destructive
            } else {
                ButtonRole::Secondary
            })
            .with_enabled(enabled)
            .tooltip(kind.describe());
        // Each acts on the machine; Format asks for the device's name first.
        let button = button.on_effect(move || intents.push(Intent::Run(kind)));
        row = row.push(Box::new(button));
    }
    Section::new(vec![
        Box::new(
            Text::new(&target_line(model.drive, model.partition))
                .with_role(TextRole::Caption)
                .dimmed(),
        ),
        Box::new(row),
    ])
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
            label: format!("{} {}", part.display_label(), bytes_si(part.size)),
            size: part.size,
            used: part.used_frac(),
        })
        .collect();
    if drive.free_tail() > 0 {
        entries.push(MapEntry {
            partition: None,
            device: String::new(),
            label: format!("Unallocated {}", bytes_si(drive.free_tail())),
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
            Box::new(filesystem(index, part, selected == Some(index), intents))
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
        row = row.meta(format!("{} free", bytes_si(part.size.saturating_sub(used))));
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
        Property::new("Capacity", bytes_si(drive.size)).with_kind(TextKind::Numeric),
        Property::new("Interface", interface_name(drive)),
    ];
    if let Some(serial) = &drive.serial {
        facts.push(Property::new("Serial number", serial.clone()).with_kind(TextKind::Identifier));
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
            bytes_si((written.max(0) as u64).saturating_mul(1_000_000_000)),
        )));
    }
    tiles
}

fn attribute_table(attrs: &[SmartAttr]) -> DataTable<SmartAttr> {
    let mut table = DataTable::new(vec![
        ColumnDef::new("id", "ID", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|attr: &SmartAttr| attr.id.to_string()),
        ColumnDef::new("name", "Attribute", ColumnWidth::Flex(1.0))
            .with_text(|attr: &SmartAttr| attr.name.clone()),
        ColumnDef::new("value", "Value", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|attr: &SmartAttr| attr.value.clone()),
        ColumnDef::new("status", "Status", ColumnWidth::Auto)
            .with_text(|attr: &SmartAttr| severity_label(attr.severity).to_owned()),
    ])
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

/// What a partition's row says under its name: its device when the name is a
/// label, its filesystem, and where that is mounted.
fn partition_summary(part: &Partition, labelled: bool) -> String {
    let mut facts: Vec<String> = Vec::new();
    if labelled {
        facts.push(part.name.clone());
    }
    facts.extend(part.fstype.clone().filter(|fs| !fs.is_empty()));
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
            let label = part.label.clone().filter(|label| !label.is_empty());
            let summary = partition_summary(part, label.is_some());
            let pick = intents.clone();
            Box::new(
                ListRow::new(
                    format!("disks-partition-{index}"),
                    label.unwrap_or_else(|| part.name.clone()),
                )
                .icon(Icon::Layers)
                .secondary(summary)
                .meta(bytes_si(part.size))
                .selected(model.partition == Some(index))
                .on_view(move || pick.push(Intent::SelectPartition(Some(index)))),
            )
        })
        .collect();
    Box::new(Section::new(rows))
}
