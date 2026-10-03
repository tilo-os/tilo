//! The window's content, built from the toolkit's widgets.
//!
//! The drive list is a [`NavList`] in the shell's sidebar; the detail is one
//! scrolling page of sections, property lists, stat tiles and tables. Nothing
//! here paints, sizes or colours anything: every control says what it is and
//! the theme decides how it looks. Interaction leaves as an [`Intent`] the
//! app applies to the shared document.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::format::bytes_si;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::{SpacingStep, TextRole};
use ui::widget::Widget;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Badge, Button, ButtonRole, ChipStyle, Element, EmptyState, NavItem, NavList, NavSection,
    ProgressBar, Property, PropertyList, Row, Scrollable, Section, SegmentedStyle, Spacer,
    StatGroup, StatTile, Text, ToggleGroup, Tone,
};

use crate::app::{ActionKind, Tab};
use crate::map::{MapEntry, PartitionMap};
use crate::model::{
    AttrSeverity, Drive, Partition, SmartAttr, SmartInfo, SmartStatus, Topology, fmt_hours,
};

/// What a control in the window asks for. The app applies each one to the
/// shared document and the machine, the same way for a click, a key or an
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

/// The drive list: one destination per drive.
pub(crate) fn drive_list(topology: &Topology, selected: usize, intents: &Intents) -> NavList {
    let items = topology
        .drives
        .iter()
        .enumerate()
        .map(|(index, drive)| {
            NavItem::new(drive_id(index), drive.model.clone())
                .icon(Icon::HardDrive)
                .without_actions()
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
        .on_click(move || intents.push(Intent::Rescan))
}

/// Nothing to show: the system reported no block devices.
pub(crate) fn empty_state(intents: &Intents) -> Element {
    Box::new(RescanOnAction::new(
        EmptyState::new("No drives found")
            .with_icon(Icon::HardDrive)
            .with_message("Attach a drive, then rescan.")
            .with_action("Rescan"),
        intents.clone(),
    ))
}

/// An empty state whose one action rescans the drives.
pub(crate) struct RescanOnAction {
    state: EmptyState,
    intents: Intents,
}

impl RescanOnAction {
    fn new(state: EmptyState, intents: Intents) -> Self {
        Self { state, intents }
    }

    fn forward(&mut self) {
        if self.state.take_action() {
            self.intents.push(Intent::Rescan);
        }
    }
}

impl ui::widget::Decorator for RescanOnAction {
    type Inner = EmptyState;

    fn inner(&self) -> &EmptyState {
        &self.state
    }

    fn inner_mut(&mut self) -> &mut EmptyState {
        &mut self.state
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        Widget::handle_event(&mut self.state, event, ctx);
        self.forward();
    }

    fn update(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.forward();
        Widget::update(&mut self.state, state, path)
    }
}

/// The detail of the selected drive: its state at a glance, the actions that
/// apply, and one tab of detail.
pub(crate) fn detail(
    model: &DetailModel<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
) -> Element {
    let drive = model.drive;
    let mut page = ui::widget::Column::new().with_intrinsic_height();
    page = page.push(Box::new(badges(drive)));
    page = page.push(group_gap());
    page = page.push(Box::new(actions(model, intents)));
    page = page.push(group_gap());
    page = page.push(Box::new(tabs(model.tab, intents)));
    page = page.push(match model.tab {
        Tab::Overview => overview(model, intents),
        Tab::Smart => health(drive),
        Tab::Partitions => partitions(model, intents),
    });
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_persisted_offset(Rc::clone(offset))
            .with_child(Box::new(page)),
    )
}

/// Two sibling gaps either side of nothing: the space between page groups.
fn group_gap() -> Element {
    Box::new(Spacer::token(SpacingStep::S0))
}

fn status_tone(status: SmartStatus) -> Tone {
    match status {
        SmartStatus::Passed => Tone::Success,
        SmartStatus::Warning => Tone::Warning,
        SmartStatus::Failing => Tone::Danger,
        SmartStatus::Unknown => Tone::Neutral,
    }
}

fn badges(drive: &Drive) -> Row {
    let status = drive.smart_status();
    let mut row = Row::new().push(Box::new(
        Badge::text(status.label()).style(ChipStyle::Tinted(status_tone(status))),
    ));
    row = row.push(Box::new(
        Badge::text(drive.kind.tag()).style(ChipStyle::Neutral),
    ));
    if drive.removable {
        row = row.push(Box::new(Badge::text("Removable").style(ChipStyle::Neutral)));
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
        let button = Button::new(&format!("disks-action-{}", kind.key()), kind.label(), None)
            .role(if kind.destructive() {
                ButtonRole::Destructive
            } else {
                ButtonRole::Secondary
            })
            .with_enabled(enabled)
            .tooltip(kind.describe())
            .on_click(move || intents.push(Intent::Run(kind)));
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
    .with_id("disks-tabs")
    .on_change(move |i| {
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
        })
        .collect();
    if drive.free_tail() > 0 {
        entries.push(MapEntry {
            partition: None,
            device: String::new(),
            label: format!("Unallocated {}", bytes_si(drive.free_tail())),
            size: drive.free_tail(),
        });
    }
    let pick = intents.clone();
    let map = PartitionMap::new(entries, model.partition, move |part| {
        pick.push(Intent::SelectPartition(part));
    });

    let mut page = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_no_padding();
    page = page.push(Box::new(Section::titled(
        "Partition map",
        vec![Box::new(map)],
    )));
    page = page.push(group_gap());
    page = page.push(Box::new(filesystems(drive, model.partition)));
    page = page.push(group_gap());
    page = page.push(Box::new(Section::titled(
        "Details",
        vec![Box::new(PropertyList::new(drive_facts(drive)))],
    )));
    Box::new(page)
}

fn filesystems(drive: &Drive, selected: Option<usize>) -> Section {
    let mounted: Vec<(usize, &Partition)> = drive
        .partitions
        .iter()
        .enumerate()
        .filter(|(_, part)| part.mountpoint.is_some())
        .collect();
    let mut items: Vec<Element> = Vec::new();
    if mounted.is_empty() {
        items.push(Box::new(
            Text::new("No filesystems are mounted.")
                .with_role(TextRole::Body)
                .dimmed(),
        ));
    }
    for (n, (index, part)) in mounted.into_iter().enumerate() {
        if n > 0 {
            items.push(group_gap());
        }
        items.push(Box::new(filesystem(part, selected == Some(index))));
    }
    Section::titled("Filesystems", items)
}

fn filesystem(part: &Partition, selected: bool) -> ui::widget::Column {
    let title = Text::new(&part.display_label()).with_role(TextRole::Label);
    let title = if selected { title.strong() } else { title };
    let mount = part.mount_display().unwrap_or_default();
    let used = part.used_frac().unwrap_or(0.0);
    let usage = match part.used {
        Some(used) => format!(
            "{} used of {}, {} free",
            bytes_si(used),
            bytes_si(part.size),
            bytes_si(part.size.saturating_sub(used)),
        ),
        None => bytes_si(part.size),
    };
    let fs = part.fstype.clone().unwrap_or_default();
    ui::widget::Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(title))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    Text::new(&mount)
                        .with_role(TextRole::Caption)
                        .with_kind(TextKind::Identifier)
                        .dimmed(),
                )),
        ))
        .push(Box::new(ProgressBar::new(used as f32)))
        .push(Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(
                    Text::new(&usage)
                        .with_role(TextRole::Caption)
                        .numeric()
                        .dimmed(),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    Text::new(&fs).with_role(TextRole::Caption).dimmed(),
                )),
        ))
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
        .with_no_padding();
    page = page.push(Box::new(Text::new(message).with_role(TextRole::Body)));

    if let Some(smart) = &drive.smart {
        let tiles = smart_tiles(smart);
        if !tiles.is_empty() {
            page = page.push(group_gap());
            page = page.push(Box::new(StatGroup::new(tiles)));
        }
        if !smart.attrs.is_empty() {
            page = page.push(group_gap());
            page = page.push(Box::new(Section::titled(
                "Attributes",
                vec![Box::new(attribute_table(&smart.attrs))],
            )));
        }
    }
    Box::new(page)
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
        tiles.push(Box::new(
            StatTile::new("Temperature", temp.to_string())
                .unit("°C")
                .tone(tone),
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
        tiles.push(Box::new(
            StatTile::new("Reallocated sectors", sectors.to_string()).tone(tone),
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

/// One row of the partitions table.
#[derive(Clone)]
struct PartitionRow {
    device: String,
    label: Option<String>,
    filesystem: Option<String>,
    size: String,
    mount: Option<String>,
    used: Option<String>,
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
    let rows: Vec<PartitionRow> = drive
        .partitions
        .iter()
        .map(|part| PartitionRow {
            device: part.name.clone(),
            label: part.label.clone().filter(|label| !label.is_empty()),
            filesystem: part.fstype.clone(),
            size: bytes_si(part.size),
            mount: part.mount_display(),
            used: part.used_frac().map(|f| format!("{:.0}%", f * 100.0)),
        })
        .collect();
    let mut selection = ui::selection::SelectionModel::new();
    if let Some(index) = model.partition {
        selection.select_only(index);
    }
    let pick = intents.clone();
    let mut table = DataTable::new(vec![
        ColumnDef::new("device", "Device", ColumnWidth::Flex(1.0))
            .with_kind(TextKind::Identifier)
            .with_text(|row: &PartitionRow| row.device.clone()),
        ColumnDef::new("label", "Label", ColumnWidth::Flex(1.0))
            .with_optional_text(|row: &PartitionRow| row.label.clone(), "No label"),
        ColumnDef::new("filesystem", "Filesystem", ColumnWidth::Auto)
            .with_optional_text(|row: &PartitionRow| row.filesystem.clone(), "None"),
        ColumnDef::new("size", "Size", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|row: &PartitionRow| row.size.clone()),
        ColumnDef::new("mount", "Mount point", ColumnWidth::Flex(1.5))
            .with_kind(TextKind::Identifier)
            .with_optional_text(|row: &PartitionRow| row.mount.clone(), "Not mounted"),
        ColumnDef::new("used", "Used", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_optional_text(|row: &PartitionRow| row.used.clone(), "—"),
    ])
    .with_id("disks-partitions")
    .with_accessible_name("Partitions")
    .selectable(true)
    .with_selection(selection)
    .on_row_click(move |index, _, _| pick.push(Intent::SelectPartition(Some(index))));
    table.set_rows(rows);
    Box::new(Section::new(vec![Box::new(table)]))
}
