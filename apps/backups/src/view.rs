//! The window's content, built from the toolkit's widgets.
//!
//! The sidebar is a [`NavList`] of places (every backup, each source, each
//! destination, the retention policy); the content is one scrolling page of
//! summary tiles and the snapshot timeline; the inspector shows the selected
//! snapshot. Nothing here paints, sizes or colours anything: every widget
//! says what it is and the theme decides how it looks. Interaction leaves as
//! an [`Intent`] the app applies.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use ui::LocalState;
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Banner, Button, ButtonRole, Column, ControlSize, Element, EmptyState, Field, FormSection,
    ListRow, ModalDialog, NavItem, NavList, NavSection, NumberInput, ProgressBar, ProgressRing,
    ProgressRingSize, Property, PropertyList, Row, Scrollable, Section, SectionHeader, Spacer,
    StatGroup, StatTile, Text, ToggleGroup, Tone, TreeNode, TreeRowDecoration, TreeRowTone,
    TreeView, Widget,
};

use crate::doc::PolicyField;
use crate::i18n;
use crate::model::{
    Catalog, DestKind, Destination, Filter, Health, Schedule, SnapFile, SnapStatus, Snapshot,
    Source, SourceKind,
};

/// A place in the sidebar, and the page the content shows for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// Every snapshot of every source.
    All,
    /// One source's snapshots and schedule.
    Source(String),
    /// One destination's capacity and the snapshots stored on it.
    Destination(String),
    /// The retention policy.
    Retention,
}

impl Page {
    pub fn nav_id(&self) -> String {
        match self {
            Page::All => "all".into(),
            Page::Source(id) => format!("source:{id}"),
            Page::Destination(id) => format!("destination:{id}"),
            Page::Retention => "retention".into(),
        }
    }

    pub fn from_nav_id(id: &str) -> Option<Page> {
        match id {
            "all" => Some(Page::All),
            "retention" => Some(Page::Retention),
            _ => {
                if let Some(source) = id.strip_prefix("source:") {
                    Some(Page::Source(source.into()))
                } else {
                    id.strip_prefix("destination:")
                        .map(|dest| Page::Destination(dest.into()))
                }
            }
        }
    }

    /// The snapshots the page lists.
    pub fn filter(&self) -> Filter<'_> {
        match self {
            Page::All | Page::Retention => Filter::All,
            Page::Source(id) => Filter::Source(id),
            Page::Destination(id) => Filter::Destination(id),
        }
    }

    /// Whether the page lists snapshots.
    pub fn lists_snapshots(&self) -> bool {
        !matches!(self, Page::Retention)
    }
}

/// What a control in the window asks for. The app applies each one the same
/// way for a click, a key or an accessibility action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    Navigate(Page),
    SelectSnapshot(String),
    BackUpNow,
    Verify,
    OpenRestore,
    ChooseRestoreFolder,
    CloseRestore,
    Setup,
    DismissFailure,
    SetOrder(Order),
}

/// The queue controls write to and the app drains.
#[derive(Clone, Default)]
pub struct Intents(Rc<RefCell<Vec<Intent>>>);

impl Intents {
    pub fn push(&self, intent: Intent) {
        self.0.borrow_mut().push(intent);
    }

    pub fn take(&self) -> Vec<Intent> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// The order the timeline lists snapshots in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    NewestFirst,
    OldestFirst,
}

/// What a running job is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Backup,
    Verify,
    Restore,
}

impl JobKind {
    fn title(self) -> &'static str {
        match self {
            JobKind::Backup => "Backing up",
            JobKind::Verify => "Verifying",
            JobKind::Restore => "Restoring",
        }
    }
}

/// A running backup, verification or restore, and its progress line.
#[derive(Clone)]
pub struct Job {
    pub kind: JobKind,
    /// Rewritten as the worker reports progress; read by the progress row
    /// each frame, so progress never rebuilds the page.
    pub line: Rc<RefCell<String>>,
}

impl Job {
    pub fn new(kind: JobKind, line: impl Into<String>) -> Self {
        Self {
            kind,
            line: Rc::new(RefCell::new(line.into())),
        }
    }
}

/// Everything a page shows, borrowed from the app.
pub struct Model<'a> {
    pub catalog: &'a Catalog,
    pub page: &'a Page,
    pub selected: Option<&'a str>,
    pub now: NaiveDateTime,
    pub order: Order,
    pub job: Option<&'a Job>,
    /// The last operation failed: what, and what to do.
    pub failure: Option<(&'a str, &'a str)>,
}

// ---- formatting ---------------------------------------------------------

/// A stored size in the platform's unit for stored data (SI).
pub fn size(bytes: u64) -> String {
    ui::format::bytes_si(bytes)
}

/// `12:05`.
fn clock(at: NaiveDateTime) -> String {
    format!("{:02}:{:02}", at.hour(), at.minute())
}

/// "Today", "Yesterday", or the weekday and date.
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    match (today - day).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => format!(
            "{} {} {}",
            ui::format::weekday_name(day.weekday().num_days_from_sunday() as usize),
            day.day(),
            ui::format::month_name(day.month0() as usize)
        ),
    }
}

/// "Today at 10:00", "Yesterday at 18:00", "Sunday 28 June at 04:00".
pub fn when(at: NaiveDateTime, now: NaiveDateTime) -> String {
    format!("{} at {}", day_label(at.date(), now.date()), clock(at))
}

/// A summary tile's value and unit for an instant in the past.
fn ago(at: NaiveDateTime, now: NaiveDateTime) -> (String, String) {
    let minutes = (now - at).num_minutes().max(0);
    let (count, key) = if minutes < 60 {
        (minutes, "backups-unit-minutes-ago")
    } else if minutes < 24 * 60 {
        (minutes / 60, "backups-unit-hours-ago")
    } else {
        (minutes / (24 * 60), "backups-unit-days-ago")
    };
    let count = count.unsigned_abs();
    (count.to_string(), i18n::counted(key, count, ""))
}

/// A summary tile's value and unit for an instant to come.
fn ahead(at: NaiveDateTime, now: NaiveDateTime) -> (String, String) {
    let minutes = (at - now).num_minutes().max(0);
    if minutes < 60 {
        let count = minutes.unsigned_abs();
        return (
            count.to_string(),
            i18n::counted("backups-unit-minutes", count, ""),
        );
    }
    if at.date() == now.date() {
        return (clock(at), String::new());
    }
    let day = day_label(at.date(), now.date());
    (clock(at), day.to_lowercase())
}

/// `"105.3 GB"` → `("105.3", "GB")`.
fn split_unit(text: &str) -> (String, String) {
    text.split_once(' ').map_or_else(
        || (text.to_owned(), String::new()),
        |(value, unit)| (value.to_owned(), unit.to_owned()),
    )
}

fn source_icon(kind: SourceKind) -> Icon {
    match kind {
        SourceKind::Home => Icon::Home,
        SourceKind::System => Icon::Settings,
        SourceKind::Workspace => Icon::Users,
        SourceKind::Custom => Icon::Folder,
    }
}

fn destination_icon(kind: DestKind) -> Icon {
    match kind {
        DestKind::LocalFolder => Icon::Folder,
        DestKind::LocalBtrfs | DestKind::External => Icon::HardDrive,
        DestKind::RemoteRestic => Icon::Globe,
    }
}

/// "Every day at 18:00".
fn schedule_label(schedule: Schedule) -> String {
    match schedule {
        Schedule::OnDemand => "When you back up".into(),
        Schedule::Hourly { minute: 0 } => "Every hour, on the hour".into(),
        Schedule::Hourly { minute } => format!("Every hour at {minute:02} past"),
        Schedule::Daily { hour, minute } => format!("Every day at {hour:02}:{minute:02}"),
        Schedule::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "Every {} at {hour:02}:{minute:02}",
            ui::format::weekday_name(weekday.num_days_from_sunday() as usize)
        ),
    }
}

/// "240.0 MB changed in 312 files".
fn changed(snapshot: &Snapshot) -> String {
    i18n::counted(
        "backups-changed-files",
        u64::from(snapshot.files_changed),
        &size(snapshot.delta_bytes),
    )
}

/// "240.0 MB in 312 files", beside a "Changed" label.
fn changed_size(snapshot: &Snapshot) -> String {
    i18n::counted(
        "backups-size-in-files",
        u64::from(snapshot.files_changed),
        &size(snapshot.delta_bytes),
    )
}

fn snapshot_title(snapshot: &Snapshot, catalog: &Catalog) -> String {
    if let Some(label) = &snapshot.label {
        return label.clone();
    }
    catalog
        .source(&snapshot.source_id)
        .map_or_else(|| "Snapshot".into(), |source| source.name.clone())
}

// ---- sidebar ------------------------------------------------------------

/// The places: every backup, each source, each destination, the policy.
pub fn sidebar(catalog: &Catalog, page: &Page, intents: &Intents) -> NavList {
    let all = NavItem::new(Page::All.nav_id(), "All backups")
        .icon(Icon::History)
        .count(catalog.total_snapshots() as u32)
        .without_actions();
    let sources = catalog
        .sources
        .iter()
        .map(|source| {
            let item = NavItem::new(Page::Source(source.id.clone()).nav_id(), &source.name)
                .icon(source_icon(source.kind))
                .without_actions();
            // Failed runs wait for the person; a plain count does not.
            let failed = catalog.failed(Filter::Source(&source.id));
            if failed > 0 {
                item.live_count(failed as u32)
            } else {
                item.count(catalog.count_for(&source.id) as u32)
            }
        })
        .collect();
    let destinations = catalog
        .destinations
        .iter()
        .map(|dest| {
            NavItem::new(Page::Destination(dest.id.clone()).nav_id(), &dest.name)
                .icon(destination_icon(dest.kind))
                .without_actions()
        })
        .collect();
    let policy = NavItem::new(Page::Retention.nav_id(), "Retention")
        .icon(Icon::Calendar)
        .without_actions();
    let intents = intents.clone();
    NavList::new(vec![
        NavSection::untitled("overview", vec![all]),
        NavSection::new("sources", "Sources", sources),
        NavSection::new("destinations", "Destinations", destinations),
        NavSection::untitled("policy", vec![policy]),
    ])
    .selected(&page.nav_id())
    .with_accessible_name("Backups")
    .on_select(move |id| {
        if let Some(page) = Page::from_nav_id(&id) {
            intents.push(Intent::Navigate(page));
        }
    })
}

/// The content column's title.
pub fn page_title(catalog: &Catalog, page: &Page) -> String {
    match page {
        Page::All => "All backups".into(),
        Page::Source(id) => catalog
            .source(id)
            .map_or_else(|| "Source".into(), |s| s.name.clone()),
        Page::Destination(id) => catalog
            .destination(id)
            .map_or_else(|| "Destination".into(), |d| d.name.clone()),
        Page::Retention => "Retention".into(),
    }
}

/// The content header's controls: the one main action of the window, and
/// History.
pub fn header_actions(job: Option<JobKind>, can_back_up: bool, intents: &Intents) -> Vec<Element> {
    let back_up = intents.clone();
    vec![
        Box::new(
            Button::new(
                "backups-back-up-now",
                if job == Some(JobKind::Backup) {
                    "Backing up…"
                } else {
                    "Back up now"
                },
                None,
            )
            .role(ButtonRole::Primary)
            .control_size(ControlSize::Small)
            .with_enabled(can_back_up && job.is_none())
            .on_effect(move || back_up.push(Intent::BackUpNow)),
        ),
        Box::new(ui::widget::history_button()),
    ]
}

/// The timeline's heading, with its order beside it.
fn timeline_heading(current: Order, intents: &Intents) -> Row {
    let order = intents.clone();
    Row::new()
        .no_wrap()
        .push(Box::new(
            Text::new("Snapshots")
                .with_role(TextRole::Heading)
                .with_heading_level(2),
        ))
        .push(Box::new(Spacer::new()))
        .push(Box::new(
            ToggleGroup::new(
                vec![
                    Segment::new("Newest first").short("Newest"),
                    Segment::new("Oldest first").short("Oldest"),
                ],
                usize::from(current == Order::OldestFirst),
            )
            .control_size(ControlSize::Small)
            .on_view("backups-order", move |index| {
                order.push(Intent::SetOrder(if index == 0 {
                    Order::NewestFirst
                } else {
                    Order::OldestFirst
                }));
            }),
        ))
}

// ---- pages ----------------------------------------------------------------

/// The content for `model.page`.
pub fn content(
    model: &Model<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
    retention: &[LocalState<f64>; 4],
) -> Element {
    let mut page = Column::new().with_intrinsic_height();
    if let Some((title, message)) = model.failure {
        page = page.push(failure_banner(title, message, intents));
    }
    if let Some(job) = model.job {
        page = page.push(Box::new(progress(job)));
    }
    page = match model.page {
        Page::All => page
            .push(Box::new(summary(model.catalog, Filter::All, model.now)))
            .push(snapshot_list(model, intents)),
        Page::Source(id) => match model.catalog.source(id) {
            Some(source) => page
                .push(Box::new(summary(
                    model.catalog,
                    Filter::Source(id),
                    model.now,
                )))
                .push(Box::new(source_details(source)))
                .push(snapshot_list(model, intents)),
            None => page.push(Box::new(EmptyState::new("This source is no longer set up"))),
        },
        Page::Destination(id) => match model.catalog.destination(id) {
            Some(dest) => page
                .push(Box::new(destination_summary(
                    dest,
                    model.catalog,
                    model.now,
                )))
                .push(snapshot_list(model, intents)),
            None => page.push(Box::new(EmptyState::new(
                "This destination is no longer set up",
            ))),
        },
        Page::Retention => page.push(Box::new(retention_page(retention))),
    };
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_persisted_offset(Rc::clone(offset))
            .with_child(Box::new(page)),
    )
}

/// An error the person has to act on, above the content it concerns.
fn failure_banner(title: &str, message: &str, intents: &Intents) -> Element {
    Box::new(OnSignal::new(
        Banner::error(title).with_message(message).dismissible(),
        Banner::take_dismissed,
        intents.clone(),
        Intent::DismissFailure,
    ))
}

/// A running backup, verification or restore. Traversal totals are unknown
/// until the tree has been walked, so the bar is indeterminate: it shows
/// work without inventing a percentage.
fn progress(job: &Job) -> Section {
    let line = Rc::clone(&job.line);
    Section::new(vec![
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(
                    Text::new(job.kind.title())
                        .with_role(TextRole::Label)
                        .strong(),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    Text::dynamic(move || line.borrow().clone())
                        .with_role(TextRole::Caption)
                        .numeric()
                        .dimmed(),
                )),
        ),
        Box::new(ProgressBar::indeterminate()),
    ])
}

/// Protected size, snapshot count, last backup and next run, all derived
/// from the catalog and the schedules.
fn summary(catalog: &Catalog, filter: Filter<'_>, now: NaiveDateTime) -> StatGroup {
    let protected = match filter {
        Filter::Source(id) => catalog.protected_bytes(id),
        Filter::All | Filter::Destination(_) => catalog.total_protected(),
    };
    let (value, unit) = split_unit(&size(protected));
    let count = catalog.timeline(filter).len();
    let mut tiles: Vec<Element> = vec![
        Box::new(StatTile::new("Protected", value).unit(unit)),
        Box::new(StatTile::new("Snapshots", i18n::group_digits(count as u64))),
    ];
    tiles.push(Box::new(match catalog.last_backup(filter) {
        Some(last) => {
            let (value, unit) = ago(last.taken, now);
            let tile = StatTile::new("Last backup", value).unit(unit);
            // Two days without a good backup is the tile's news.
            if now - last.taken > chrono::Duration::days(2) {
                tile.tone(Tone::Warning)
            } else {
                tile
            }
        }
        None => StatTile::new("Last backup", "None yet").tone(Tone::Warning),
    }));
    tiles.push(Box::new(match catalog.next_run(filter, now) {
        Some(next) => {
            let (value, unit) = ahead(next, now);
            StatTile::new("Next run", value).unit(unit)
        }
        None => StatTile::new("Next run", "On demand"),
    }));
    StatGroup::new(tiles)
}

/// Where a source lives and when it runs.
fn source_details(source: &Source) -> PropertyList {
    let mut facts = vec![
        Property::new("Folder", source.path.clone()).with_kind(TextKind::Identifier),
        Property::new("Kind", source.kind.label()),
        Property::new("Schedule", schedule_label(source.schedule)),
    ];
    if !source.enabled {
        facts.push(Property::new("Status", "Paused"));
    }
    PropertyList::new(facts)
}

/// A destination's capacity, and what it holds.
fn destination_summary(dest: &Destination, catalog: &Catalog, now: NaiveDateTime) -> Row {
    let frac = dest.used_fraction();
    let mut ring = ProgressRing::new(frac)
        .value(format!("{:.0}%", frac * 100.0))
        .caption("used")
        .size(ProgressRingSize::Large);
    if frac > 0.85 {
        ring = ring.tone(Tone::Warning);
    }
    let mut facts = vec![
        Property::new("Kind", dest.kind.label()),
        Property::new("Used", size(dest.used_bytes)).with_kind(TextKind::Numeric),
        Property::new(
            "Free",
            size(dest.total_bytes.saturating_sub(dest.used_bytes)),
        )
        .with_kind(TextKind::Numeric),
        Property::new("Capacity", size(dest.total_bytes)).with_kind(TextKind::Numeric),
        Property::new(
            "Encryption",
            if dest.encrypted {
                "Encrypted"
            } else {
                "Not encrypted"
            },
        ),
    ];
    if let Some(last) = catalog.last_backup(Filter::Destination(&dest.id)) {
        facts.push(Property::new("Last backup", when(last.taken, now)));
    }
    if dest.health != Health::Ok {
        facts.push(Property::new("Status", "Last run failed"));
    }
    Row::new()
        .no_wrap()
        .align_start()
        .push(Box::new(ring))
        .push(Box::new(PropertyList::new(facts)))
}

/// The snapshots of the page, grouped by day.
fn snapshot_list(model: &Model<'_>, intents: &Intents) -> Element {
    let mut snaps = model.catalog.timeline(model.page.filter());
    if snaps.is_empty() {
        return Box::new(
            EmptyState::new("No snapshots yet")
                .with_icon(Icon::History)
                .with_message("Back up now to create the first one."),
        );
    }
    if model.order == Order::OldestFirst {
        snaps.reverse();
    }
    let show_source = !matches!(model.page, Page::Source(_));
    let today = model.now.date();
    let mut days: Vec<Element> = vec![Box::new(timeline_heading(model.order, intents))];
    let mut day_rows: Vec<Element> = Vec::new();
    let mut current: Option<NaiveDate> = None;
    let flush = |day: NaiveDate, rows: &mut Vec<Element>, days: &mut Vec<Element>| {
        let count = rows.len();
        days.push(Box::new(Section::new(std::mem::take(rows)).with_header(
            SectionHeader::group(day_label(day, today)).count(count),
        )));
    };
    for snapshot in snaps {
        let day = snapshot.taken.date();
        if let Some(previous) = current
            && previous != day
        {
            flush(previous, &mut day_rows, &mut days);
        }
        current = Some(day);
        day_rows.push(Box::new(snapshot_row(
            snapshot,
            model,
            show_source,
            intents,
        )));
    }
    if let Some(day) = current {
        flush(day, &mut day_rows, &mut days);
    }
    Box::new(Section::new(days))
}

/// One snapshot: what it captured, when, and whether it needs a look.
fn snapshot_row(
    snapshot: &Snapshot,
    model: &Model<'_>,
    show_source: bool,
    intents: &Intents,
) -> ListRow {
    let catalog = model.catalog;
    let source = catalog.source(&snapshot.source_id);
    let title = match (&snapshot.label, show_source) {
        (Some(label), _) => label.clone(),
        (None, true) => snapshot_title(snapshot, catalog),
        (None, false) => format!("{} snapshot", snapshot.kind.label()),
    };
    let dest = catalog
        .destination(&snapshot.dest_id)
        .map_or("its destination", |d| d.name.as_str());
    let secondary = match snapshot.status {
        SnapStatus::Complete => changed(snapshot),
        SnapStatus::Verifying => format!("Verifying on {dest}"),
        SnapStatus::Failed => format!("Couldn’t copy to {dest}"),
    };
    let id = snapshot.id.clone();
    let intents = intents.clone();
    let mut row = ListRow::new(format!("snapshot-{}", snapshot.id), title)
        .secondary(secondary)
        .meta(clock(snapshot.taken))
        .selected(model.selected == Some(snapshot.id.as_str()))
        .on_activate(move || intents.push(Intent::SelectSnapshot(id.clone())));
    if let Some(source) = source {
        row = row.icon(source_icon(source.kind));
    }
    match snapshot.status {
        SnapStatus::Complete => row,
        SnapStatus::Verifying => row.active(),
        SnapStatus::Failed => row.error(),
    }
}

/// How many snapshots each class keeps.
fn retention_page(states: &[LocalState<f64>; 4]) -> Column {
    let mut keep = FormSection::new("Snapshots to keep").grouped();
    for (field, state) in PolicyField::ALL.iter().zip(states) {
        keep = keep.field(Field::new(
            field.label(),
            NumberInput::from_state(state)
                .with_range(0.0, 999.0)
                .with_id(&format!("backups-keep-{}", field.key()))
                .with_accessible_name(format!("{} snapshots to keep", field.label())),
        ));
    }
    let counts = states.clone();
    let total = Text::dynamic(move || {
        let total: f64 = counts.iter().map(|state| state.with(|v| *v)).sum();
        let total = total.max(0.0).round() as u64;
        i18n::counted("backups-retention-total", total, "")
    })
    .with_role(TextRole::Body)
    .dimmed();
    Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(keep))
        .push(Box::new(total))
}

// ---- the selected snapshot ----------------------------------------------

/// The inspector: everything known about the selected snapshot, and what
/// can be done with it.
pub fn snapshot_details(
    snapshot: &Snapshot,
    catalog: &Catalog,
    now: NaiveDateTime,
    can_act: bool,
    intents: &Intents,
) -> Element {
    let source = catalog.source(&snapshot.source_id);
    let dest = catalog.destination(&snapshot.dest_id);
    let mut facts = vec![
        Property::new("Taken", when(snapshot.taken, now)),
        Property::new("Status", snapshot.status.label()),
        Property::new("Type", snapshot.kind.label()),
        Property::new("Retention", snapshot.retention.label()),
        Property::new("Size", size(snapshot.size_bytes)).with_kind(TextKind::Numeric),
        Property::new("Changed", changed_size(snapshot)).with_kind(TextKind::Numeric),
    ];
    if let Some(source) = source {
        facts.push(Property::new("Source", source.name.clone()));
        facts.push(Property::new("Folder", source.path.clone()).with_kind(TextKind::Identifier));
    }
    if let Some(dest) = dest {
        facts.push(Property::new("Stored on", dest.name.clone()));
        facts.push(Property::new(
            "Encryption",
            if dest.encrypted {
                "Encrypted"
            } else {
                "Not encrypted"
            },
        ));
    }
    let usable = can_act && snapshot.status != SnapStatus::Failed;
    let restore = {
        let intents = intents.clone();
        Button::new("backups-restore", "Restore…", None)
            .with_enabled(usable)
            .on_view(move || intents.push(Intent::OpenRestore))
    };
    let verify = {
        let intents = intents.clone();
        Button::new("backups-verify", "Verify files", None)
            .with_enabled(usable)
            .tooltip("Read every file and compare its checksum")
            .on_effect(move || intents.push(Intent::Verify))
    };
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .push(Box::new(
                        Text::new(&snapshot_title(snapshot, catalog)).with_role(TextRole::Heading),
                    ))
                    .push(Box::new(PropertyList::new(facts)))
                    .push(Box::new(
                        Row::new().push(Box::new(restore)).push(Box::new(verify)),
                    )),
            )),
    )
}

// ---- restore --------------------------------------------------------------

/// The restore sheet's body: what recovery does, the snapshot it restores,
/// and a read-only preview of its contents. Recovery is the whole snapshot,
/// so the preview selects nothing.
pub fn restore_dialog(
    snapshot: &Snapshot,
    catalog: &Catalog,
    now: NaiveDateTime,
    intents: &Intents,
) -> ModalDialog<Column> {
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(
            Text::new(
                "The whole snapshot is restored into a new or empty folder you choose, and \
                 every file is checked. Nothing that already exists is replaced.",
            )
            .with_role(TextRole::Body),
        ));
    let mut facts = vec![
        Property::new("Taken", when(snapshot.taken, now)),
        Property::new("Size", size(snapshot.size_bytes)).with_kind(TextKind::Numeric),
    ];
    if let Some(dest) = catalog.destination(&snapshot.dest_id) {
        facts.push(Property::new("Stored on", dest.name.clone()));
    }
    body = body.push(Box::new(PropertyList::new(facts)));
    body = body.push(match file_tree(&snapshot.files) {
        Some(tree) => {
            Box::new(Section::new(vec![tree]).with_header(SectionHeader::group("Contents")))
                as Element
        }
        None => Box::new(
            Text::new("The file list for this snapshot isn’t available yet.")
                .with_role(TextRole::Body)
                .dimmed(),
        ),
    });
    let title = match &snapshot.label {
        Some(label) => format!("Restore “{label}”"),
        None => format!(
            "Restore {} from {}",
            snapshot_title(snapshot, catalog),
            when(snapshot.taken, now).to_lowercase()
        ),
    };
    let choose = intents.clone();
    let cancel = intents.clone();
    let dismiss = intents.clone();
    ModalDialog::new(title, body)
        .with_test_id("backups-restore")
        .with_max_width(560.0)
        .open(true)
        .primary_action("Choose folder…", move || {
            choose.push(Intent::ChooseRestoreFolder);
        })
        .secondary_action("Cancel", move || cancel.push(Intent::CloseRestore))
        .on_dismiss(move || dismiss.push(Intent::CloseRestore))
}

/// The snapshot's file index as a tree, its top level open, sizes trailing.
fn file_tree(files: &[SnapFile]) -> Option<Element> {
    // Build the forest from the depth-ordered index.
    let mut stack: Vec<TreeNode<Option<u64>>> = Vec::new();
    let mut roots: Vec<TreeNode<Option<u64>>> = Vec::new();
    let mut depths: Vec<usize> = Vec::new();
    let close = |stack: &mut Vec<TreeNode<Option<u64>>>,
                 depths: &mut Vec<usize>,
                 roots: &mut Vec<TreeNode<Option<u64>>>| {
        if let Some(node) = stack.pop() {
            depths.pop();
            match stack.last_mut() {
                Some(parent) => parent.children.push(node),
                None => roots.push(node),
            }
        }
    };
    for (index, file) in files.iter().enumerate() {
        while depths.last().is_some_and(|depth| *depth >= file.depth) {
            close(&mut stack, &mut depths, &mut roots);
        }
        let id = index as u64 + 1;
        let node = if file.is_dir {
            TreeNode::branch(id, file.name.clone(), file.size, Vec::new())
        } else {
            TreeNode::leaf(id, file.name.clone(), file.size)
        };
        stack.push(node);
        depths.push(file.depth);
    }
    while !stack.is_empty() {
        close(&mut stack, &mut depths, &mut roots);
    }
    let root = match roots.len() {
        0 => return None,
        1 => roots.pop()?,
        _ => TreeNode::branch(0, "Snapshot", None, roots),
    };
    let root_id = root.id;
    let mut tree = TreeView::new(root)
        .with_id("backups-restore-contents")
        .with_row_decoration(|model: &TreeNode<Option<u64>>, row, theme| {
            let node = model.find(row.id);
            let mut face = TreeRowDecoration::new().icon(
                if node.is_some_and(TreeNode::is_branch) {
                    Icon::Folder
                } else {
                    Icon::File
                },
                theme.tokens.colors.text_dim,
            );
            if let Some(bytes) = node.and_then(|node| node.data) {
                face = face.trailing(size(bytes));
            }
            face.tone(TreeRowTone::Primary)
        });
    tree.restore_expanded([root_id]);
    Some(Box::new(tree))
}

// ---- first run and recovery -----------------------------------------------

/// Where setup stands on a device with no usable repository.
pub enum Setup<'a> {
    /// Nothing chosen yet.
    Start,
    /// A source is chosen; the destination is next.
    ChooseDestination,
    /// The first backup is running.
    Running(&'a str),
    /// The saved location could not be read.
    Unavailable(&'a str),
}

/// The one screen of a device that has no backups to show.
pub fn setup(state: &Setup<'_>, intents: &Intents) -> Element {
    let state = match state {
        Setup::Start => EmptyState::new("Protect your important files")
            .with_icon(Icon::Archive)
            .with_message("Choose a folder, and every file in it is backed up with a checksum.")
            .with_action("Choose folder…"),
        Setup::ChooseDestination => EmptyState::new("Choose where to keep backups")
            .with_icon(Icon::HardDrive)
            .with_message("Pick a drive or a folder outside the one you are backing up.")
            .with_action("Choose location…"),
        Setup::Running(status) => EmptyState::new("Creating your first backup")
            .with_icon(Icon::Archive)
            .with_message(*status),
        Setup::Unavailable(error) => EmptyState::error(
            "Backup location unavailable",
            format!("{error} Backups are paused until it is back or you choose another location."),
        )
        .with_icon(Icon::HardDrive)
        .with_action("Choose another location…"),
    };
    Box::new(OnSignal::new(
        state,
        EmptyState::take_action,
        intents.clone(),
        Intent::Setup,
    ))
}

// ---- signal forwarding ------------------------------------------------------

/// A widget whose one signal (a dismissed banner, an empty state's action)
/// leaves as an intent.
pub struct OnSignal<W> {
    inner: W,
    take: fn(&mut W) -> bool,
    intents: Intents,
    intent: Intent,
}

impl<W> OnSignal<W> {
    fn new(inner: W, take: fn(&mut W) -> bool, intents: Intents, intent: Intent) -> Self {
        Self {
            inner,
            take,
            intents,
            intent,
        }
    }

    fn forward(&mut self) {
        if (self.take)(&mut self.inner) {
            self.intents.push(self.intent.clone());
        }
    }
}

impl<W: Widget + 'static> ui::widget::Decorator for OnSignal<W> {
    type Inner = W;

    fn inner(&self) -> &W {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        Widget::handle_event(&mut self.inner, event, ctx);
        self.forward();
    }

    fn update(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.forward();
        Widget::update(&mut self.inner, state, path)
    }
}
