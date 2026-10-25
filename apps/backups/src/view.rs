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

use chrono::{Datelike, NaiveDate, NaiveDateTime};
use ui::LocalState;
use ui::fact::{Measure, Measured};
use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::TextRole;
use ui::widget::{
    Banner, Button, ChipStyle, Column, Container, ControlSize, Dropdown, Element, EmptyState,
    Field, FormSection, ListRow, ModalDialog, NavItem, NavList, NavSection, NumberInput,
    ProgressBar, ProgressRing, ProgressRingSize, Property, PropertyList, Row, Scrollable, Section,
    SectionHeader, Spacer, StatGroup, StatTile, Text, Tone, TreeNode, TreeRowDecoration,
    TreeRowTone, TreeView, Widget,
};

use crate::i18n;
use crate::model::PolicyField;
use crate::model::{
    Catalog, DestKind, Destination, Filter, Health, Retention, Schedule, SnapFile, SnapStatus,
    SnapType, Snapshot, Source, SourceKind,
};

/// A place in the sidebar, and the page the content shows for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    All,
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
pub type Intents = ui::Intents<Intent>;

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

/// A stored size in the platform's unit for stored data (SI).
pub fn size(bytes: u64) -> String {
    ui::format::size(bytes)
}

/// `12:05` or `12:05 PM`.
fn clock(at: NaiveDateTime) -> String {
    ui::clock::time_of_day(&at)
}

/// "Today", "Yesterday", or the weekday and date.
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    let days = (day - today).num_days();
    match ui::format::near_day(days) {
        Some(near) if days <= 0 => near,
        _ => ui::format::day_heading(day, day.year() != today.year()),
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
pub(crate) fn schedule_label(schedule: Schedule) -> String {
    match schedule {
        Schedule::OnDemand => "When you back up".into(),
        Schedule::Hourly { minute: 0 } => "Every hour, on the hour".into(),
        Schedule::Hourly { minute } => format!("Every hour at {minute:02} past"),
        Schedule::Daily { hour, minute } => {
            format!("Every day at {}", ui::clock::hour_minute(hour, minute))
        }
        Schedule::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "Every {} at {}",
            ui::format::weekday_name(weekday),
            ui::clock::hour_minute(hour, minute)
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

/// What an unlabelled snapshot is, on a page that already names its source:
/// taken by hand, or the retention class that keeps it.
fn kind_title(snapshot: &Snapshot) -> &'static str {
    match (snapshot.kind, snapshot.retention) {
        (SnapType::Manual, _) => "Manual snapshot",
        (SnapType::Auto, Retention::Hourly) => "Hourly snapshot",
        (SnapType::Auto, Retention::Daily) => "Daily snapshot",
        (SnapType::Auto, Retention::Weekly) => "Weekly snapshot",
        (SnapType::Auto, Retention::Monthly) => "Monthly snapshot",
        (SnapType::Auto, Retention::Keep) => "Kept snapshot",
        (SnapType::Unknown, _) | (_, Retention::Unknown) => "Snapshot",
    }
}

/// The dot a place shows when it needs the person; a healthy one shows none.
fn health_status(health: Health) -> Option<ui::widget::RowStatus<'static>> {
    match health {
        Health::Ok | Health::Unknown => None,
        Health::Warning => Some(ui::widget::RowStatus::Warning),
    }
}

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
                .secondary(schedule_label(source.schedule))
                .without_actions();
            let item = match health_status(source.health) {
                Some(status) => item.status(status),
                None => item,
            };
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
            let item = NavItem::new(Page::Destination(dest.id.clone()).nav_id(), &dest.name)
                .icon(destination_icon(dest.kind))
                .meta(size(dest.used_bytes))
                .without_actions();
            // How full it is, where the provider says how big it is.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a share in 0..=1 drawn as a bar"
            )]
            let item = match dest.used_fraction() {
                Some(fraction) => item.level(fraction as f32),
                None => item,
            };
            match health_status(dest.health) {
                Some(status) => item.status(status),
                None => item,
            }
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
    .with_id("backups-places")
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
            Dropdown::from_state(
                vec!["Newest first".to_owned(), "Oldest first".to_owned()],
                &LocalState::new(
                    match current {
                        Order::NewestFirst => "Newest first",
                        Order::OldestFirst => "Oldest first",
                    }
                    .to_owned(),
                ),
            )
            .with_id("backups-order")
            .with_accessible_name("Order of snapshots")
            .control_size(ControlSize::Small)
            .on_change(move |choice| {
                order.push(Intent::SetOrder(if choice == "Oldest first" {
                    Order::OldestFirst
                } else {
                    Order::NewestFirst
                }));
            }),
        ))
}

pub fn content(
    model: &Model<'_>,
    intents: &Intents,
    offset: &Rc<Cell<(f32, f32)>>,
    retention: &[LocalState<f64>; 4],
) -> Element {
    // The banner, the job, the tiles, the details and the timeline are
    // separate groups of the page, a group step apart.
    let mut page = Column::new()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Group);
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
                .push(destination_summary(dest, model.catalog, model.now))
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
    let at = ui::time::wall();
    let tile = |label: &str, fact: Measured, unit: Option<String>, tone: Option<Tone>| {
        let mut tile = StatTile::new(label, fact.value_or_dash().to_owned());
        if let Some(unit) = unit {
            tile = tile.unit(unit);
        }
        if let Some(tone) = tone {
            tile = tile.tone(tone);
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(tile))
                .push(Box::new(
                    Text::new(&fact.caption(at))
                        .with_role(TextRole::Caption)
                        .dimmed(),
                )),
        ) as Element
    };
    let protected_fact = Measured::measured(
        Measure::new("Protected size").unit(unit.clone()),
        value,
        "repository manifest",
        at,
        None,
    );
    let mut tiles: Vec<Element> = vec![
        tile("Protected", protected_fact, Some(unit), None),
        tile(
            "Snapshots",
            Measured::measured(
                Measure::new("Snapshots"),
                i18n::group_digits(count as u64),
                "repository manifest",
                at,
                None,
            ),
            None,
            None,
        ),
    ];
    tiles.push(match catalog.last_backup(filter) {
        Some(last) => {
            let (value, unit) = ago(last.taken, now);
            let tone = (now - last.taken > chrono::Duration::days(2)).then_some(Tone::Warning);
            tile(
                "Last backup",
                Measured::measured(
                    Measure::new("Time since last backup").unit(unit.clone()),
                    value,
                    "repository manifest",
                    at,
                    None,
                ),
                Some(unit),
                tone,
            )
        }
        None => tile(
            "Last backup",
            Measured::unknown(
                Measure::new("Time since last backup"),
                "no completed snapshot",
            ),
            None,
            Some(Tone::Warning),
        ),
    });
    tiles.push(match catalog.next_run(filter, now) {
        Some(next) => {
            let (value, unit) = ahead(next, now);
            tile(
                "Next run",
                Measured::measured(
                    Measure::new("Time to next run").unit(unit.clone()),
                    value,
                    "schedule",
                    at,
                    None,
                ),
                Some(unit),
                None,
            )
        }
        None => tile(
            "Next run",
            Measured::unknown(
                Measure::new("Time to next run"),
                "no scheduler runs on this device; back up when you choose",
            ),
            None,
            None,
        ),
    });
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

/// A destination's capacity, and what it holds. The ring shows only where
/// the destination reports a capacity; nothing is estimated.
fn destination_summary(dest: &Destination, catalog: &Catalog, now: NaiveDateTime) -> Element {
    let mut facts = vec![
        Property::new("Kind", dest.kind.label()),
        Property::new("Used", size(dest.used_bytes)).with_kind(TextKind::Numeric),
    ];
    if let Some(total) = dest.total_bytes {
        facts.push(
            Property::new("Free", size(total.saturating_sub(dest.used_bytes)))
                .with_kind(TextKind::Numeric),
        );
        facts.push(Property::new("Capacity", size(total)).with_kind(TextKind::Numeric));
    }
    facts.push(Property::new(
        "Encryption",
        if dest.encrypted {
            "Encrypted"
        } else {
            "Not encrypted"
        },
    ));
    if let Some(last) = catalog.last_backup(Filter::Destination(&dest.id)) {
        facts.push(Property::new("Last backup", when(last.taken, now)));
    }
    match dest.health {
        Health::Warning => facts.push(Property::new("Status", "Last run failed")),
        Health::Unknown => facts.push(Property::new("Status", "Not checked yet")),
        Health::Ok => {}
    }
    let facts = PropertyList::new(facts);
    let Some(frac) = dest.used_fraction() else {
        return Box::new(facts);
    };
    let mut ring = ProgressRing::new(frac)
        .value(format!("{:.0}%", frac * 100.0))
        .caption("used")
        .size(ProgressRingSize::Large);
    if frac > 0.85 {
        ring = ring.tone(Tone::Warning);
    }
    Box::new(
        Row::new()
            .no_wrap()
            .align_start()
            .push(Box::new(ring))
            .push(Box::new(facts)),
    )
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
        (None, false) => kind_title(snapshot).into(),
    };
    let dest = catalog
        .destination(&snapshot.dest_id)
        .map_or("its destination", |d| d.name.as_str());
    let secondary = match snapshot.status {
        SnapStatus::Complete => changed(snapshot),
        SnapStatus::Verifying => format!("Verifying on {dest}"),
        SnapStatus::Failed => match &snapshot.failure {
            Some(reason) => format!("Couldn’t copy to {dest}: {reason}"),
            None => format!("Couldn’t copy to {dest} (no reason was recorded)"),
        },
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
    let mut tags: Vec<(&str, ChipStyle)> = Vec::new();
    if snapshot.kind == SnapType::Manual {
        tags.push(("Manual", ChipStyle::Neutral));
    }
    if snapshot.kind == SnapType::Unknown {
        tags.push(("Type not recorded", ChipStyle::Neutral));
    }
    if snapshot.retention == Retention::Keep {
        tags.push(("Kept", ChipStyle::Neutral));
    }
    if !tags.is_empty() {
        row = row.tags(&tags);
    }
    match snapshot.status {
        SnapStatus::Complete => row,
        SnapStatus::Verifying => row.active(),
        SnapStatus::Failed => row.error(),
    }
}

/// How many snapshots each class keeps.
fn retention_page(states: &[LocalState<f64>; 4]) -> Container {
    let mut keep = FormSection::new("Snapshots to keep").grouped();
    for (field, state) in PolicyField::ALL.iter().zip(states) {
        keep = keep.field(Field::new(
            format!("{} snapshots to keep", field.label()),
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
    let form = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .push(Box::new(keep))
        .push(Box::new(total));
    // A form is bounded to the readable measure, on the page's leading
    // edge, so each count sits near its label.
    Container::new()
        .with_no_padding()
        .readable()
        .with_child(Box::new(form))
}

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
    if let Some(reason) = &snapshot.failure {
        facts.push(Property::new("Failure", reason.clone()));
    }
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
        Button::new("backups-restore", "Restore…")
            .with_enabled(usable)
            .on_view(move || intents.push(Intent::OpenRestore))
    };
    let verify = {
        let intents = intents.clone();
        Button::new("backups-verify", "Verify files")
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
        .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
        .open(true)
        .primary_action("Choose folder…", move || {
            choose.push(Intent::ChooseRestoreFolder);
        })
        .secondary_action("Cancel", move || cancel.push(Intent::CloseRestore))
        .on_dismiss(move || dismiss.push(Intent::CloseRestore))
}

/// The snapshot's file index as a tree, every folder open, sizes trailing.
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
    let mut branches = Vec::new();
    open_branches(&root, &mut branches);
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
    tree.restore_expanded(branches);
    Some(Box::new(tree))
}

/// The id of every folder under and including `node`.
fn open_branches(node: &TreeNode<Option<u64>>, out: &mut Vec<u64>) {
    if node.is_branch() {
        out.push(node.id);
        for child in &node.children {
            open_branches(child, out);
        }
    }
}

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
            .with_message("Choose a folder to keep a copy of every file in it, checked so you know it is intact.")
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
            "Backups are paused until it is back or you choose another location.",
        )
        .with_details(*error)
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

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.forward();
        Widget::update(&mut self.inner, state, path)
    }
}
