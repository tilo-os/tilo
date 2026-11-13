//! The window's content, built from the toolkit's widgets. The drive list is a [`NavList`]
//! in the shell's sidebar; the detail is one scrolling page of sections, property lists,
//! stat tiles and tables. Nothing here paints, sizes or colours anything: every control
//! says what it is and the theme decides how it looks.

use std::cell::Cell;
use std::rc::Rc;

use ui::format::size;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::{
    Chip, ChipStyle, ContextMenuArea, Element, EmptyState, ListRow, MenuEntry, NavItem, NavList,
    NavSection, Property, PropertyList, Row, RowStatus, Scrollable, Section, SkeletonShape,
    StatGroup, StatTile, Text, Tone,
};
use ui::window_toolbar::{
    LocationSegment, ToolbarAction, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
};

use crate::app::{ActionKind, Tab};
use crate::locales::CATALOG;
use crate::map::{MapEntry, PartitionMap};
use crate::model::{
    AttrSeverity, Drive, DriveKind, Partition, SmartAttr, SmartInfo, SmartStatus, fmt_hours,
};
use crate::offer::{self, Offer};

/// What a control in the window asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    SelectDrive(usize),
    SelectPartition(Option<usize>),
    RunOn {
        action: ActionKind,
        partition: Option<usize>,
    },
    OpenInFiles(Option<usize>),
    DiskUsage(Option<usize>),
    Rescan,
}

pub(crate) type Intents = ui::Intents<Intent>;

pub(crate) struct DetailModel<'a> {
    pub drive: &'a Drive,
    pub tab: Tab,
    pub partition: Option<usize>,
}

pub(crate) const RESCAN: &str = "rescan";
pub(crate) const WHOLE_DRIVE: &str = "drive";

/// Where the window is: the drive, and the partition of it when one is chosen.
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
        Offer::Available | Offer::Hidden if in_row(action) => action.describe().to_owned(),
        Offer::Available | Offer::Hidden => action.label(),
    }
}

/// Ejecting a removable drive is the one operation a glyph says plainly; the others are
/// named in the window bar's menu.
fn in_row(action: ActionKind) -> bool {
    action == ActionKind::Eject
}

/// The window bar's slots: the shell's own (`shell`), where the window is (the drive, and
/// the partition when one is chosen), and the selection's operations followed by Rescan.
pub(crate) fn window_toolbar(
    mut shell: WindowToolbar,
    selection: Option<(Location, Vec<(ActionKind, Offer)>)>,
    tab: Tab,
) -> WindowToolbar {
    let (location, offers) = selection.unwrap_or_default();
    let mut actions: Vec<ToolbarAction> = offers
        .into_iter()
        .map(|(action, offer)| ToolbarAction {
            id: action.key().to_owned(),
            icon: action_icon(action).name().to_owned(),
            name: action_name(action, offer),
            enabled: offer.is_available(),
            menu: !in_row(action),
            ..ToolbarAction::default()
        })
        .collect();
    actions.push(ToolbarAction::new(
        RESCAN,
        Icon::Refresh.name(),
        crate::locales::CATALOG.t("disks-rescan-drives"),
    ));
    ui::widget::NavShell::push_window_actions(&mut shell, actions);
    shell.views = (!location.drive.is_empty()).then(|| ToolbarViews {
        name: crate::locales::CATALOG.t("disks-views"),
        choices: Tab::all()
            .into_iter()
            .map(|tab| ToolbarView {
                id: tab.view_id().to_owned(),
                label: tab.title().to_owned(),
            })
            .collect(),
        chosen: tab.view_id().to_owned(),
    });
    let backs = !shell.leading.is_empty();
    shell.location = (!location.drive.is_empty()).then(|| ToolbarLocation {
        label: location.title().to_owned(),
        segments: match &location.partition {
            Some(partition) if !backs => vec![
                LocationSegment {
                    label: location.drive.clone(),
                    value: WHOLE_DRIVE.to_owned(),
                },
                LocationSegment {
                    label: partition.clone(),
                    value: "partition".to_owned(),
                },
            ],
            _ => Vec::new(),
        },
        ..ToolbarLocation::default()
    });
    shell
}

/// A context menu of what `part` of `drive` offers (`partition`: its index, `None` for the
/// whole drive): each operation it shows, and under an unavailable one, why.
fn operations_menu(
    drive: &Drive,
    partition: Option<usize>,
    intents: &Intents,
) -> impl Fn(Option<ui::vello::kurbo::Point>) -> Vec<MenuEntry> + 'static {
    let part = partition.and_then(|i| drive.partitions.get(i));
    let offers = offer::shown(drive, part);
    let mounted = part.is_some_and(|part| part.mountpoint.is_some());
    let intents = intents.clone();
    move |_| {
        let mut entries = Vec::new();
        if mounted {
            let open = intents.clone();
            entries.push(
                MenuEntry::new(CATALOG.t("disks-open-in-files"), move || {
                    open.push(Intent::OpenInFiles(partition));
                })
                .with_icon(Icon::Folder),
            );
            let usage = intents.clone();
            entries.push(
                MenuEntry::new(CATALOG.t("disks-space-left"), move || {
                    usage.push(Intent::DiskUsage(partition));
                })
                .with_icon(Icon::BarChart),
            );
            entries.push(MenuEntry::separator());
        }
        for &(action, offer) in &offers {
            let intents = intents.clone();
            let run = move || intents.push(Intent::RunOn { action, partition });
            let entry = if action.is_delete() {
                MenuEntry::destructive(action.label(), run)
            } else {
                MenuEntry::new(action.label(), run)
            };
            entries.push(
                entry
                    .with_icon(action_icon(action))
                    .with_enabled(offer.is_available()),
            );
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

/// The drive list: one destination per drive under the name it shows, its size beside it
/// and a dot when its health needs the person.
pub(crate) fn drive_list(
    drives: &[(&Drive, String)],
    selected: usize,
    loading: bool,
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
    let section = NavSection::untitled("drives", items).pending(u8::from(loading));
    NavList::new(vec![section])
        .selected(&drive_id(selected))
        .with_accessible_name(crate::locales::CATALOG.t("disks-a11y-drives"))
        .on_view("disks.drives", move |id| {
            if let Some(index) = id.strip_prefix("drive:").and_then(|n| n.parse().ok()) {
                intents.push(Intent::SelectDrive(index));
            }
        })
}

fn drive_id(index: usize) -> String {
    format!("drive:{index}")
}

/// Nothing to show: the system reported no block devices.
pub(crate) fn empty_state(unreadable: Option<&str>, intents: &Intents) -> Element {
    let state = match unreadable {
        Some(reason) => EmptyState::new("Storage can't be read")
            .with_icon(Icon::HardDrive)
            .with_message(reason),
        None => EmptyState::new("No drives found")
            .with_icon(Icon::HardDrive)
            .with_message(CATALOG.t("disks-empty-drives-hint")),
    };
    Box::new(
        state
            .with_action("Rescan")
            .on_effect(intents.sender(Intent::Rescan)),
    )
}

/// The page while this machine's storage is being read, since `since`: the detail page's
/// own layout with the drive's state and the sections' contents standing in, so nothing
/// moves when the drives arrive.
pub(crate) fn loading(offset: &Rc<Cell<(f32, f32)>>, since: ui::time::Instant) -> Element {
    let local = || ui::arriving::Source::Local;
    let sections = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Group)
        .push(Box::new(Section::titled(
            CATALOG.t("disks-section-partition-map"),
            vec![Box::new(PartitionMap::pending(since))],
        )))
        .push(Box::new(Section::titled(
            CATALOG.t("disks-section-filesystems"),
            vec![ui::widget::ArrivingView::placeholder_since(
                local(),
                since,
                SkeletonShape::Row(
                    ui::widget::RowShape::TITLE
                        .led_by(ui::widget::RowLeadingShape::Mark)
                        .with_secondary(1),
                ),
            )],
        )))
        .push(Box::new(Section::titled(
            CATALOG.t("disks-section-details"),
            vec![ui::widget::ArrivingView::placeholder_since(
                local(),
                since,
                SkeletonShape::Text { lines: 4 },
            )],
        )));
    page(
        Row::new().push(ui::widget::ArrivingView::placeholder_since(
            local(),
            since,
            SkeletonShape::Chip,
        )),
        Box::new(sections),
        offset,
    )
}

/// The detail of the selected drive: its state at a glance and the view the window bar
/// chose.
pub(crate) fn detail(
    model: &DetailModel<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
) -> Element {
    let drive = model.drive;
    let content = match model.tab {
        Tab::Overview => overview(model, intents),
        Tab::Smart => health(drive),
        Tab::Partitions => partitions(model, intents),
    };
    page(badges(drive), content, offset)
}

/// The detail page's layout: the drive's state at a glance, then the view the window bar
/// chose (`content`), a group apart.
fn page(state: Row, content: Element, offset: &Rc<Cell<(f32, f32)>>) -> Element {
    let page = ui::widget::Column::new()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group)
        .push(Box::new(state))
        .push(content);
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

/// The drive's state at a glance: its health when it reports any, what kind of device it
/// is, and whether it can be pulled out.
fn badges(drive: &Drive) -> Row {
    let status = drive.smart_status();
    let mut row = Row::new();
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
        CATALOG.t("disks-section-partition-map"),
        vec![Box::new(map)],
    )));
    page = page.push(Box::new(filesystems(drive, model.partition, intents)));
    if let Some(part) = model.partition.and_then(|i| drive.partitions.get(i)) {
        page = page.push(Box::new(partition_details(drive, part)));
        page = page.push(Box::new(Section::titled(
            CATALOG.t("disks-section-drive"),
            vec![Box::new(PropertyList::new(drive_facts(drive)))],
        )));
    } else {
        page = page.push(Box::new(Section::titled(
            CATALOG.t("disks-section-details"),
            vec![Box::new(PropertyList::new(drive_facts(drive)))],
        )));
    }
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
            Text::new(&crate::locales::CATALOG.t("disks-none-mounted"))
                .with_role(TextRole::Body)
                .dimmed(),
        ));
    }
    if let Some(target) = mounted_target(drive, selected) {
        let mount = drive
            .partitions
            .get(target)
            .and_then(Partition::mount_display)
            .unwrap_or_else(|| "these files".to_owned());
        items.push(Box::new(
            Row::new()
                .push(Box::new(mounted_action(
                    "disks-open-in-files",
                    &CATALOG.t("disks-open-in-files"),
                    Icon::Folder,
                    &mount,
                    Intent::OpenInFiles(Some(target)),
                    intents,
                )))
                .push(Box::new(mounted_action(
                    "disks-space-left",
                    &CATALOG.t("disks-space-left"),
                    Icon::BarChart,
                    &mount,
                    Intent::DiskUsage(Some(target)),
                    intents,
                ))),
        ));
    }
    Section::titled(CATALOG.t("disks-section-filesystems"), items)
}

/// The mounted partition the drive page's two ways on act for: the chosen one when it is
/// mounted, else the one holding the most — what a person means by "the drive" when they
/// have not chosen a partition, rather than whichever partition happens to be first
/// (usually a boot partition).
fn mounted_target(drive: &Drive, selected: Option<usize>) -> Option<usize> {
    if let Some((index, part)) = selected.map(|index| (index, drive.partitions.get(index)))
        && let Some(part) = part
        && part.mountpoint.is_some()
    {
        return Some(index);
    }
    drive
        .partitions
        .iter()
        .enumerate()
        .filter(|(_, part)| part.mountpoint.is_some())
        .max_by_key(|(_, part)| part.used.unwrap_or(0))
        .map(|(index, _)| index)
}

/// One of the two ways on from a mounted filesystem, as a button that says what it does
/// and, in its tooltip, which filesystem it does it for.
fn mounted_action(
    id: &str,
    label: &str,
    icon: Icon,
    mount: &str,
    intent: Intent,
    intents: &Intents,
) -> ui::widget::Button {
    ui::widget::Button::new(id, label)
        .with_icon(icon)
        .tooltip(match intent {
            Intent::DiskUsage(_) => format!("How much space {mount} has left"),
            _ => format!("Open {mount} in Files"),
        })
        .on_effect(intents.sender(intent))
}

const NEARLY_FULL: f64 = 0.9;
const FULL: f64 = 0.97;

fn filesystem(index: usize, part: &Partition, selected: bool, intents: &Intents) -> ListRow {
    let pick = intents.clone();
    let secondary = match (part.mount_display(), &part.fstype) {
        (Some(mount), _) if mount == "Swap" => CATALOG.t("disks-swap"),
        (Some(mount), Some(fstype)) if !fstype.is_empty() => CATALOG.t_args(
            "disks-filesystem-at",
            &[
                ("filesystem", ui::i18n::ArgValue::Text(fstype.clone())),
                ("path", ui::i18n::ArgValue::Text(mount)),
            ],
        ),
        (Some(mount), _) => CATALOG.t_args(
            "disks-mounted-at",
            &[("path", ui::i18n::ArgValue::Text(mount))],
        ),
        (None, _) => String::new(),
    };
    let mut row = ListRow::new(format!("disks-filesystem-{index}"), part.display_label())
        .icon(Icon::Layers)
        .secondary(secondary)
        .selected(selected)
        .on_view(move || pick.push(Intent::SelectPartition(Some(index))));
    if let Some(used) = part.used {
        row = row.meta(CATALOG.t_args(
            "disks-partition-free",
            &[(
                "room",
                ui::i18n::ArgValue::Text(size(part.size.saturating_sub(used))),
            )],
        ));
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
        Property::new(CATALOG.t("disks-device"), format!("/dev/{}", drive.name))
            .with_kind(TextKind::Identifier),
        Property::new(CATALOG.t("disks-type"), drive.kind.long()),
        Property::new(CATALOG.t("disks-capacity"), size(drive.size)).with_kind(TextKind::Numeric),
        Property::new(CATALOG.t("disks-interface"), interface_name(drive)),
    ];
    if let Some(serial) = &drive.serial {
        facts.push(
            Property::new(CATALOG.t("disks-serial"), serial.clone())
                .with_kind(TextKind::Identifier),
        );
    }
    facts
}

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
                CATALOG.t("disks-section-attributes"),
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
    let mut columns = Vec::new();
    if attrs.iter().any(|attr| attr.id.is_some()) {
        columns.push(
            ColumnDef::new("id", "ID", ColumnWidth::Auto)
                .with_kind(TextKind::Numeric)
                .with_text(|attr: &SmartAttr| attr.id.map(|id| id.to_string()).unwrap_or_default()),
        );
    }
    columns.extend([
        ColumnDef::new("name", "Attribute", ColumnWidth::Flex(1.0))
            .with_text(|attr: &SmartAttr| attr.name.clone()),
        ColumnDef::new("value", "Value", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|attr: &SmartAttr| attr.value.clone()),
        ColumnDef::new("status", "Status", ColumnWidth::Auto)
            .with_text(|attr: &SmartAttr| severity_label(attr.severity)),
    ]);
    let mut table = DataTable::new(columns)
        .with_id("disks-attributes")
        .with_accessible_name(crate::locales::CATALOG.t("disks-a11y-smart"));
    table.set_rows(attrs.to_vec());
    table
}

fn severity_label(severity: AttrSeverity) -> String {
    let message = match severity {
        AttrSeverity::Ok => "disks-severity-healthy",
        AttrSeverity::Warn => "disks-severity-warning",
        AttrSeverity::Crit => "disks-severity-critical",
    };
    crate::locales::CATALOG.t(message)
}

/// What a partition is for, in words, when it serves something the person knows by name.
fn purpose(drive: &Drive, part: &Partition) -> Option<String> {
    use crate::model::PartRole;
    let windows = |name: &str| {
        let name = name.to_ascii_lowercase();
        name.contains("microsoft") || name.contains("windows")
    };
    let message = match part.role() {
        PartRole::Root => "disks-purpose-root",
        PartRole::Home => "disks-purpose-home",
        PartRole::Efi | PartRole::Boot if drive.is_system_drive() => "disks-purpose-boot",
        PartRole::Swap => "disks-swap",
        _ if part
            .fstype
            .as_deref()
            .is_some_and(|fs| fs.eq_ignore_ascii_case("ntfs"))
            || part.part_type_name.as_deref().is_some_and(windows) =>
        {
            return Some("Windows".to_owned());
        }
        _ => return None,
    };
    Some(crate::locales::CATALOG.t(message))
}

/// What a partition's row says under its name: what it is for, its state and its room, in
/// words ("Home folders, encrypted, 134 GB free", "Not in use"), joined the way this
/// language writes a list.
fn partition_summary(drive: &Drive, part: &Partition) -> String {
    let cat = &crate::locales::CATALOG;
    let lock = part.encryption.as_ref().map(|encryption| {
        if encryption.unlocked_as.is_none() {
            "disks-partition-encrypted-locked"
        } else {
            "disks-partition-encrypted"
        }
    });
    let purpose = purpose(drive, part)
        .filter(|purpose| !purpose.eq_ignore_ascii_case(part.display_label().trim()));
    let state = match purpose {
        Some(purpose) => purpose,
        None if part.mountpoint.is_some() => cat.t("disks-in-use"),
        None => cat.t("disks-not-in-use"),
    };
    let mut facts = vec![state];
    if let Some(lock) = lock {
        facts.push(cat.t(lock));
    }
    if let Some(used) = part.used {
        facts.push(cat.t_args(
            "disks-partition-free",
            &[(
                "room",
                ui::i18n::ArgValue::Text(size(part.size.saturating_sub(used))),
            )],
        ));
    }
    ui::format::list(&facts)
}

/// The partition's facts: its device, size, type, filesystem, encryption, where it is
/// mounted and its room, and what of the running system it holds.
fn partition_details(drive: &Drive, part: &Partition) -> Section {
    let mut facts = vec![
        Property::new(CATALOG.t("disks-device"), format!("/dev/{}", part.name))
            .with_kind(TextKind::Identifier),
        Property::new(CATALOG.t("disks-size"), size(part.size)).with_kind(TextKind::Numeric),
    ];
    if let Some(kind) = part.part_type_name.clone().filter(|kind| !kind.is_empty()) {
        facts.push(Property::new(CATALOG.t("disks-type"), kind));
    }
    if let Some(fstype) = part.fstype.clone().filter(|fs| !fs.is_empty()) {
        facts.push(Property::new(CATALOG.t("disks-filesystem"), fstype));
    }
    if let Some(encryption) = &part.encryption {
        let message = match &encryption.unlocked_as {
            Some(_) => "disks-encryption-unlocked",
            None => "disks-encryption-locked",
        };
        let state = CATALOG.t_args(
            message,
            &[(
                "scheme",
                ui::i18n::ArgValue::Text(encryption.scheme.clone()),
            )],
        );
        facts.push(Property::new(CATALOG.t("disks-encryption"), state));
    }
    facts.push(Property::new(
        CATALOG.t("disks-mounted-at"),
        part.mount_display()
            .unwrap_or_else(|| CATALOG.t("disks-not-mounted")),
    ));
    if let Some(used) = part.used {
        facts.push(Property::new(CATALOG.t("disks-used"), size(used)).with_kind(TextKind::Numeric));
        facts.push(
            Property::new(
                CATALOG.t("disks-free"),
                size(part.size.saturating_sub(used)),
            )
            .with_kind(TextKind::Numeric),
        );
    }
    if let Some(purpose) = purpose(drive, part) {
        facts.push(Property::new(CATALOG.t("disks-used-for"), purpose));
    }
    Section::titled(
        CATALOG.t("disks-section-details"),
        vec![Box::new(PropertyList::new(facts))],
    )
}

fn partitions(model: &DetailModel<'_>, intents: &Intents) -> Element {
    let drive = model.drive;
    if drive.partitions.is_empty() {
        return Box::new(Section::new(vec![Box::new(
            Text::new(&crate::locales::CATALOG.t("disks-drive-empty"))
                .with_role(TextRole::Body)
                .dimmed(),
        )]));
    }
    let rows: Vec<Element> = drive
        .partitions
        .iter()
        .enumerate()
        .map(|(index, part)| -> Element {
            let pick = intents.clone();
            let row = ListRow::new(format!("disks-partition-{index}"), part.display_label())
                .icon(Icon::Layers)
                .secondary(partition_summary(drive, part))
                .meta(size(part.size))
                .selected(model.partition == Some(index))
                .on_view(move || pick.push(Intent::SelectPartition(Some(index))));
            Box::new(ContextMenuArea::new(
                Box::new(row),
                operations_menu(drive, Some(index), intents),
            ))
        })
        .collect();
    let list = Section::new(rows);
    match model.partition.and_then(|i| drive.partitions.get(i)) {
        Some(part) => Box::new(
            ui::widget::Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .with_rhythm(Rhythm::Group)
                .push(Box::new(list))
                .push(Box::new(partition_details(drive, part))),
        ),
        None => Box::new(list),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window bar shows the system drive's Format as unavailable and says why; a fixed
    /// internal drive has no Eject in it at all.
    #[test]
    fn the_window_bar_says_why_the_system_drive_is_not_formatted() {
        let drive = crate::probe::captured::yoga().unwrap().drives.remove(0);
        let location = Location {
            drive: drive.model.clone(),
            partition: None,
        };
        let bar = window_toolbar(
            WindowToolbar::default(),
            Some((location, offer::shown(&drive, None))),
            Tab::Overview,
        );
        let format = bar
            .actions
            .iter()
            .find(|action| action.id == ActionKind::Format.key())
            .expect("Format is shown");
        assert!(!format.enabled);
        assert!(
            format.menu,
            "formatting is named in the bar's menu, not a glyph"
        );
        assert!(
            format.name.contains(crate::model::HOLDS_SYSTEM),
            "{}",
            format.name
        );
        assert!(
            bar.actions
                .iter()
                .all(|action| action.id != ActionKind::Eject.key())
        );
    }

    #[test]
    fn a_partition_is_a_level_under_its_drive() {
        let location = Location {
            drive: "SAMSUNG".into(),
            partition: Some("home".into()),
        };
        let bar = window_toolbar(
            WindowToolbar::default(),
            Some((location, Vec::new())),
            Tab::Overview,
        );
        let location = bar.location.expect("a location");
        assert_eq!(location.label, "home");
        let levels: Vec<_> = location.segments.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(levels, ["SAMSUNG", "home"]);
        assert_eq!(location.segments[0].value, WHOLE_DRIVE);

        let mut shell = WindowToolbar::default();
        shell
            .leading
            .push(ToolbarAction::new("back", "arrow-left", "Back"));
        let bar = window_toolbar(
            shell,
            Some((
                Location {
                    drive: "SAMSUNG".into(),
                    partition: Some("home".into()),
                },
                Vec::new(),
            )),
            Tab::Overview,
        );
        let location = bar.location.expect("a location");
        assert_eq!(location.label, "home");
        assert!(location.segments.is_empty(), "{:?}", location.segments);
    }

    /// A partition row says what the partition is for and its state, in words; its device,
    /// filesystem and mount point are on its own page.
    #[test]
    fn partition_rows_say_what_each_partition_is_for() {
        let drive = crate::probe::captured::yoga().unwrap().drives.remove(0);
        let summaries: Vec<(String, String)> = drive
            .partitions
            .iter()
            .map(|part| (part.name.clone(), partition_summary(&drive, part)))
            .collect();
        let of = |name: &str| {
            summaries
                .iter()
                .find(|(part, _)| part == name)
                .map(|(_, summary)| summary.as_str())
                .unwrap()
        };
        let free = |name: &str| {
            let part = drive
                .partitions
                .iter()
                .find(|part| part.name == name)
                .unwrap();
            size(part.size - part.used.unwrap())
        };
        assert_eq!(
            of("nvme0n1p4"),
            format!("Home folders, encrypted, {} free", free("nvme0n1p4"))
        );
        assert_eq!(
            of("nvme0n1p7"),
            format!("Running system, encrypted, {} free", free("nvme0n1p7"))
        );
        assert_eq!(of("nvme0n1p5"), "Starts this device");
        assert_eq!(
            of("nvme0n1p3"),
            "Not in use",
            "a purpose the name says gives way to the state"
        );
        assert_eq!(of("nvme0n1p2"), "Windows", "Microsoft reserved");
        for (_, summary) in &summaries {
            assert!(!summary.contains("nvme0n1"), "{summary}");
            assert!(
                !summary.contains("ext4") && !summary.contains('/'),
                "{summary}"
            );
        }
    }
}
