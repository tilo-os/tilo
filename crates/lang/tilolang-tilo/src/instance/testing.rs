//! Shared agreement test table and driver for [`Platform`](super::Platform) implementers.
//!
//! Every production host (`OnSession`, `PagePlatform`, `Packages`, `HostPlatform`) drives its
//! real `Instance` through the exact same probe package, pinned UTC instant and expected-text
//! table here, proving they agree on shown dates, interpolated sentences, 12- and 24-hour clocks,
//! Sunday- and Saturday-start weeks, and German typed dates in an English-only app.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test-support harness; panics report assertion failures"
)]

use std::rc::Rc;

use tilo_formats::{DateStyle, FirstDayOfWeek, RegionFormats, TimeStyle};
use tilo_render_protocol::patch::{self, Level, Node, ToUi, Transport as _, local};

use crate::host::RootView;
use crate::instance::{Config, Instance, Mode, Platform};
use crate::resources::ResourceStore;
use crate::runtime::ids::MemberId;
use crate::runtime::{Actor, Value};

/// App id of the shared probe package.
pub const PROBE_APP: &str = "so.tilo.lang.probe";

/// Minimal tilolang app exercising every `Platform` date, time, clock, week-start and typed-date
/// behavior through rendered views and unseen change marks.
pub const PROBE_SOURCE: &str = r#"app so.tilo.lang.probe "Probe" (icon: .hash, category: .utility)

/// One entry exercising shown dates, sentence interpolation, clocks, week starts and typed dates.
type Entry {
    key: Text merge register
    day: Date = Date("2026-10-29")
    at: Time = Time("2026-06-30T14:05:00Z")
    parsed: Date = Date("1970-01-01")
    parsed_at: Time = Time("1970-01-01T00:00:00Z")
}

root Entry

default view probe_view for Entry

/// Change the entry's day so an unseen change mark shows it as a person reads it.
action set_day(e: Entry, day: Date) {
    e.day = day
}

/// Change the entry's time so an unseen change mark shows it as a person reads it.
action set_time(e: Entry, at: Time) {
    e.at = at
}

/// Parse a date typed in words in the person's language (falling back to English).
action read_day(e: Entry, words: Text) uses time {
    e.parsed = Date.parse(words, near: Time("2026-10-29T12:00:00Z")) ?? Date("1970-01-01")
}

/// Parse a moment typed in words (e.g. `tomorrow 9am`, matching mail's snooze/send_later fallback).
action read_time(e: Entry, words: Text) uses time {
    e.parsed_at = Time.parse(words, near: Time("2026-10-29T07:00:00Z")) ?? Time("2026-10-29T07:00:00Z") + 18h
}

view probe_view(e: Entry) {
    std.page(e) {
        section("Dates") {
            ui.Text("sentence-day:{e.day}")
            ui.Text("sentence-time:{e.at}")
            ui.Text("clock:{e.at.clock()}")
            ui.Text("week:{e.day.start_of_week().iso()}")
            ui.Text("parsed:{e.parsed.iso()}")
            ui.Text("parsed-time:{e.parsed_at.iso()}")
            ui.ChangeMark(e)
        }
    }
}
"#;

/// Action ids resolved from [`compile_probe`].
pub struct ProbeActions {
    pub set_day: String,
    pub set_time: String,
    pub read_day: String,
    pub read_time: String,
}

/// Compile [`PROBE_SOURCE`] as an English-only package (`locales = [("en-US", ...)]`), so every
/// host treats it identically when a German person opens it: its window speaks English while
/// dates typed in words still parse in the person's language.
#[must_use]
pub fn compile_probe() -> (tilolang::Compiled, ProbeActions) {
    let mut compiled = tilolang::compile_package(PROBE_SOURCE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the probe package compiles: {diagnostics:?}"));
    tilolang::meta::append_locales(
        &mut compiled.wasm,
        &[("en-US".to_owned(), "d-probe = Probe\n".to_owned())],
    );
    let action_id = |name: &str| {
        compiled
            .meta
            .actions
            .iter()
            .find(|action| action.name == name)
            .unwrap_or_else(|| panic!("missing probe action {name}"))
            .id
            .clone()
    };
    let actions = ProbeActions {
        set_day: action_id("set_day"),
        set_time: action_id("set_time"),
        read_day: action_id("read_day"),
        read_time: action_id("read_time"),
    };
    (compiled, actions)
}

/// Pinned wall-clock instant for the agreement table: Tuesday, 2026-06-30 10:42:00 UTC.
#[must_use]
pub fn pinned_wall() -> tilo_clock::time::SystemTime {
    let ts = jiff::civil::date(2026, 6, 30)
        .at(10, 42, 0, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .unwrap_or_else(|error| panic!("valid pinned date: {error}"))
        .timestamp();
    let secs = u64::try_from(ts.as_second()).unwrap_or_else(|error| panic!("after epoch: {error}"));
    tilo_clock::time::UNIX_EPOCH
        .checked_add(tilo_clock::time::Duration::from_secs(secs))
        .unwrap_or_else(|| panic!("pinned wall time overflows SystemTime"))
}

/// UTC zone for deterministic per-thread pinning: ensures `2026-10-29T12:00:00Z` is unambiguously
/// Thursday in local time rather than already Friday at UTC+12 or later.
#[must_use]
pub fn utc_zone() -> tilo_clock::local::Zone {
    tilo_clock::local::Zone::of(jiff::tz::TimeZone::UTC)
}

/// One row of the shared date agreement table (evaluated at `2026-06-30T10:42:00Z` UTC).
pub struct DateCase {
    pub label: &'static str,
    pub iso: &'static str,
    pub expected_shown: &'static str,
    pub expected_sentence: &'static str,
}

/// Shared date cases: today, another day of this year, a future year and a past year.
pub const DATE_CASES: &[DateCase] = &[
    DateCase {
        label: "today",
        iso: "2026-06-30",
        expected_shown: "Today",
        expected_sentence: "Jun 30",
    },
    DateCase {
        label: "another day of this year",
        iso: "2026-10-29",
        expected_shown: "Oct 29",
        expected_sentence: "Oct 29",
    },
    DateCase {
        label: "a day in a future year",
        iso: "2027-06-29",
        expected_shown: "Jun 29, 2027",
        expected_sentence: "Jun 29, 2027",
    },
    DateCase {
        label: "a day in a past year",
        iso: "2025-06-29",
        expected_shown: "Overdue since Jun 29, 2025",
        expected_sentence: "Jun 29, 2025",
    },
];

/// One row of the shared time/clock agreement table (evaluated at `2026-06-30T10:42:00Z` UTC).
pub struct TimeCase {
    pub label: &'static str,
    pub style: TimeStyle,
    pub iso: &'static str,
    pub expected_shown: &'static str,
    pub expected_sentence: &'static str,
    pub expected_clock: &'static str,
}

/// Shared time and clock cases: 24-hour and 12-hour styles for `2026-06-30T14:05:00Z`.
pub const TIME_CASES: &[TimeCase] = &[
    TimeCase {
        label: "24-hour clock",
        style: TimeStyle::H24,
        iso: "2026-06-30T14:05:00Z",
        expected_shown: "14:05",
        expected_sentence: "Jun 30, 14:05",
        expected_clock: "14:05",
    },
    TimeCase {
        label: "12-hour clock",
        style: TimeStyle::H12,
        iso: "2026-06-30T14:05:00Z",
        expected_shown: "2:05 PM",
        expected_sentence: "Jun 30, 2:05 PM",
        expected_clock: "2:05 PM",
    },
];

/// One row of the shared week-start agreement table for Thursday `2026-10-29`.
pub struct WeekCase {
    pub label: &'static str,
    pub first_day: FirstDayOfWeek,
    pub expected_offset: i64,
    pub expected_start_iso: &'static str,
}

/// Shared week-start cases: Monday, Sunday and Saturday for Thursday `2026-10-29`.
pub const WEEK_CASES: &[WeekCase] = &[
    WeekCase {
        label: "Monday-start week",
        first_day: FirstDayOfWeek::Monday,
        expected_offset: 0,
        expected_start_iso: "2026-10-26",
    },
    WeekCase {
        label: "Sunday-start week",
        first_day: FirstDayOfWeek::Sunday,
        expected_offset: 6,
        expected_start_iso: "2026-10-25",
    },
    WeekCase {
        label: "Saturday-start week",
        first_day: FirstDayOfWeek::Saturday,
        expected_offset: 5,
        expected_start_iso: "2026-10-24",
    },
];

/// Apply every queued root open/patch batch from `end` onto `root`.
pub fn take_root(end: &mut local::End, root: &mut Option<Node>) {
    while let Some(batch) = end
        .recv()
        .unwrap_or_else(|error| panic!("local transport recv: {error}"))
    {
        for message in patch::decode_batch::<ToUi>(&batch)
            .unwrap_or_else(|error| panic!("valid patch batch: {error}"))
        {
            match message {
                ToUi::Open {
                    level: Level::Root,
                    tree,
                    ..
                } => *root = Some(tree),
                ToUi::Patch {
                    level: Level::Root,
                    patches,
                } => {
                    let tree = root.as_mut().unwrap_or_else(|| panic!("patch before open"));
                    for change in patches {
                        change
                            .apply(tree)
                            .unwrap_or_else(|error| panic!("patch applies: {error}"));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Flatten every prop value in `node` and its descendants for assertions.
#[must_use]
pub fn tree_text(node: &Node) -> String {
    fn walk(node: &Node, out: &mut Vec<String>) {
        for value in node.props.values() {
            out.push(format!("{value:?}"));
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out.join(" ")
}

/// Extract the `said` text of the first `ui.ChangeMark` inside `node`.
#[must_use]
pub fn change_mark_said(node: &Node) -> Option<String> {
    if node.kind == "ui.ChangeMark"
        && let Some(patch::Value::Text(said)) = node.props.get("said")
    {
        return Some(said.clone());
    }
    for child in &node.children {
        if let Some(said) = change_mark_said(child) {
            return Some(said);
        }
    }
    None
}

/// What each host's test supplies so [`assert_platform_agrees`] can drive it through the one
/// shared agreement table.
pub trait AgreementHost {
    /// Human-readable host name for assertion messages (`OnSession`, `PagePlatform`, …).
    fn name(&self) -> &'static str;

    /// Update the person's region formats (per-thread or host-local, never process-wide).
    fn set_formats(&mut self, formats: RegionFormats);

    /// Update the person's preferred language (`"en-US"`, `"de-DE"`).
    fn set_person_language(&mut self, locale: &str);

    /// Invoke `action_id` on the probe root `Entry` with extra `args` (after the target ref).
    fn invoke(&mut self, actor: &Actor, action_id: &str, args: Vec<Value>);

    /// Sync/settle the host and return the latest rendered root tree.
    fn sync_and_read(&mut self) -> Node;

    /// Direct `Platform::week_start` (or `None` if inspected purely through rendered views).
    fn week_start(&self) -> Option<i64> {
        None
    }
}

/// Reusable [`AgreementHost`] backed by an in-process [`Instance`] over a local patch channel
/// (used by `OnSession`, `PagePlatform` and `Packages`).
pub struct LocalAgreementHost<F, L> {
    name: &'static str,
    platform: Rc<dyn Platform>,
    instance: Instance,
    window_end: local::End,
    root: Option<Node>,
    set_formats: F,
    set_person_language: L,
}

impl<F, L> LocalAgreementHost<F, L>
where
    F: FnMut(RegionFormats),
    L: FnMut(&str),
{
    /// Open `PROBE_APP` on `platform` and sync its initial root.
    pub fn open(
        name: &'static str,
        platform: Rc<dyn Platform>,
        set_formats: F,
        set_person_language: L,
    ) -> Self {
        let (app_end, mut window_end) = local::pair();
        let mut instance = Instance::new(
            Config {
                app: PROBE_APP.to_owned(),
                me: "ada".into(),
                mode: Mode::Live,
                device: None,
                store: ResourceStore::in_memory(),
                root: RootView::default(),
                sink: Some(Box::new(crate::runtime::MemorySink::new())),
                link_home: None,
            },
            Rc::clone(&platform),
            Rc::new(crate::names::Installed),
            Box::new(app_end),
        );
        instance.sync();
        let host = instance
            .host()
            .unwrap_or_else(|| panic!("{name}: instance host"));
        let _ = host
            .root_of(PROBE_APP)
            .unwrap_or_else(|error| panic!("{name}: probe root: {error}"));
        instance.update();
        let mut root = None;
        take_root(&mut window_end, &mut root);
        Self {
            name,
            platform,
            instance,
            window_end,
            root,
            set_formats,
            set_person_language,
        }
    }
}

impl<F, L> AgreementHost for LocalAgreementHost<F, L>
where
    F: FnMut(RegionFormats),
    L: FnMut(&str),
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn set_formats(&mut self, formats: RegionFormats) {
        (self.set_formats)(formats);
        self.instance.person_changed();
    }

    fn set_person_language(&mut self, locale: &str) {
        (self.set_person_language)(locale);
        self.instance.person_changed();
    }

    fn invoke(&mut self, actor: &Actor, action_id: &str, mut args: Vec<Value>) {
        let host = self
            .instance
            .host()
            .unwrap_or_else(|| panic!("{}: instance host", self.name));
        let entry = host
            .root_of(PROBE_APP)
            .unwrap_or_else(|error| panic!("{}: probe root: {error}", self.name));
        args.insert(0, Value::resource(&entry));
        host.runtime()
            .invoke_action(actor, (action_id, args))
            .unwrap_or_else(|error| panic!("{}: invoke {action_id}: {error}", self.name));
    }

    fn sync_and_read(&mut self) -> Node {
        self.instance.sync();
        self.instance.update();
        take_root(&mut self.window_end, &mut self.root);
        self.root
            .clone()
            .unwrap_or_else(|| panic!("{}: root tree", self.name))
    }

    fn week_start(&self) -> Option<i64> {
        Some(self.platform.week_start())
    }
}

/// Restores the thread-local clock zone installed for [`assert_platform_agrees`], even on panic.
struct ZoneGuard;

impl Drop for ZoneGuard {
    fn drop(&mut self) {
        tilo_clock::local::install(None);
    }
}

/// Drive `host` through the full shared agreement table under the pinned UTC wall clock
/// (`2026-06-30T10:42:00Z`) and pinned UTC zone.
pub fn assert_platform_agrees(host: &mut impl AgreementHost, actions: &ProbeActions) {
    let ada = Actor::Member(MemberId::person("ada"));
    let grace = Actor::Member(MemberId::person("grace"));

    tilo_clock::testing::with_wall(pinned_wall(), || {
        tilo_clock::local::install(Some(utc_zone()));
        let _zone_guard = ZoneGuard;

        check_dates(host, actions, &grace);
        check_times(host, actions, &ada, &grace);
        check_weeks(host, actions, &grace);
        check_german_typed_dates(host, actions, &ada, &grace);
    });
}

fn check_dates(host: &mut impl AgreementHost, actions: &ProbeActions, grace: &Actor) {
    let name = host.name();
    host.set_person_language("en-US");
    host.set_formats(RegionFormats {
        date: DateStyle::Language,
        time: TimeStyle::H24,
        number: None,
        first_day: FirstDayOfWeek::Monday,
    });
    for case in DATE_CASES {
        host.invoke(grace, &actions.set_day, vec![Value::text(case.iso)]);
        let root = host.sync_and_read();
        let text = tree_text(&root);
        let said = change_mark_said(&root).unwrap_or_default();
        assert_eq!(
            said,
            format!("moved it to {}", case.expected_shown),
            "{name} ({}) shown mismatch",
            case.label
        );
        let expected_sentence = format!("sentence-day:{}", case.expected_sentence);
        assert!(
            text.contains(&expected_sentence),
            "{name} ({}) sentence mismatch: expected `{expected_sentence}` in `{text}`",
            case.label
        );
    }
}

fn check_times(host: &mut impl AgreementHost, actions: &ProbeActions, ada: &Actor, grace: &Actor) {
    let name = host.name();
    for case in TIME_CASES {
        host.set_formats(RegionFormats {
            date: DateStyle::Language,
            time: case.style,
            number: None,
            first_day: FirstDayOfWeek::Monday,
        });
        // Change `at` to a different instant first so Grace's subsequent write to `case.iso`
        // always records a fresh unseen version whose change mark formats `case.iso`.
        host.invoke(
            ada,
            &actions.set_time,
            vec![Value::text("2026-06-30T08:00:00Z")],
        );
        host.invoke(grace, &actions.set_time, vec![Value::text(case.iso)]);
        let root = host.sync_and_read();
        let text = tree_text(&root);
        let said = change_mark_said(&root).unwrap_or_default();
        assert_eq!(
            said,
            format!("set the at time to {}", case.expected_shown),
            "{name} ({}) shown time mismatch",
            case.label
        );
        let expected_sentence = format!("sentence-time:{}", case.expected_sentence);
        assert!(
            text.contains(&expected_sentence),
            "{name} ({}) sentence time mismatch: expected `{expected_sentence}` in `{text}`",
            case.label
        );
        let expected_clock = format!("clock:{}", case.expected_clock);
        assert!(
            text.contains(&expected_clock),
            "{name} ({}) clock mismatch: expected `{expected_clock}` in `{text}`",
            case.label
        );
    }
}

fn check_weeks(host: &mut impl AgreementHost, actions: &ProbeActions, grace: &Actor) {
    let name = host.name();
    host.invoke(grace, &actions.set_day, vec![Value::text("2026-10-29")]);
    for case in WEEK_CASES {
        host.set_formats(RegionFormats {
            date: DateStyle::Language,
            time: TimeStyle::H24,
            number: None,
            first_day: case.first_day,
        });
        if let Some(offset) = host.week_start() {
            assert_eq!(
                offset, case.expected_offset,
                "{name} ({}) week_start offset",
                case.label
            );
        }
        let root = host.sync_and_read();
        let text = tree_text(&root);
        let expected_week = format!("week:{}", case.expected_start_iso);
        assert!(
            text.contains(&expected_week),
            "{name} ({}) start_of_week mismatch: expected `{expected_week}` in `{text}`",
            case.label
        );
    }
}

fn check_german_typed_dates(
    host: &mut impl AgreementHost,
    actions: &ProbeActions,
    ada: &Actor,
    grace: &Actor,
) {
    let name = host.name();
    // German person in an English-only app: the window still speaks English ("Today"), while
    // typed dates read German first ("Freitag" -> Fri 2026-10-30 near Thu 2026-10-29T12:00:00Z
    // UTC) and fall back to English ("friday" -> 2026-10-30, "tomorrow 9am" near 07:00 UTC ->
    // 2026-10-30T09:00:00Z, not +18h), with German winning when both languages read the text.
    // Ada invokes `read_day`/`read_time` before Grace's `set_day` so Grace's write remains the
    // unseen change shown by `ui.ChangeMark(e)`.
    host.set_person_language("de-DE");
    host.invoke(ada, &actions.read_day, vec![Value::text("Freitag")]);
    host.invoke(ada, &actions.read_time, vec![Value::text("tomorrow 9am")]);
    host.invoke(grace, &actions.set_day, vec![Value::text("2026-06-30")]);
    let root = host.sync_and_read();
    let text = tree_text(&root);
    let said = change_mark_said(&root).unwrap_or_default();
    assert_eq!(
        said, "moved it to Today",
        "{name} English-only app stays in English for a German person"
    );
    assert!(
        text.contains("parsed:2026-10-30"),
        "{name} German 'Freitag' near Thu 2026-10-29T12:00:00Z parses as 2026-10-30: `{text}`"
    );
    assert!(
        text.contains("parsed-time:2026-10-30T09:00:00Z"),
        "{name} 'tomorrow 9am' under German parses as tomorrow 09:00 UTC (not +18h): `{text}`"
    );

    // Clear `parsed` first with an unrecognized phrase so the subsequent English `"friday"`
    // assertion proves fresh parsing rather than carrying over `"Freitag"`.
    host.invoke(ada, &actions.read_day, vec![Value::text("sometime")]);
    let text = tree_text(&host.sync_and_read());
    assert!(
        text.contains("parsed:1970-01-01"),
        "{name} unrecognized phrase falls back to sentinel: `{text}`"
    );

    host.invoke(ada, &actions.read_day, vec![Value::text("friday")]);
    let text = tree_text(&host.sync_and_read());
    assert!(
        text.contains("parsed:2026-10-30"),
        "{name} English 'friday' under German falls back to English and parses as 2026-10-30: `{text}`"
    );

    host.invoke(ada, &actions.read_day, vec![Value::text("Montag friday")]);
    let text = tree_text(&host.sync_and_read());
    assert!(
        text.contains("parsed:2026-11-02"),
        "{name} German wins over English where both languages read the text: `{text}`"
    );
}
