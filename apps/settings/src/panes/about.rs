//! About: the facts of this device.
//!
//! Reads real system facts where they're cheap: `/etc/os-release`
//! (`PRETTY_NAME`), `/proc/sys/kernel/osrelease` (kernel),
//! `/proc/uptime` (uptime), the hostname, the processor (`/proc/cpuinfo`,
//! or the system-on-chip's own name in `/sys/devices/soc0` where the CPU
//! reports no model, as on Arm) and the memory. A fact that cannot be read
//! has no row.
//!
//! Below the facts, the problems this computer recorded: the desktop
//! stopping or stopping responding, a part of it restarting, the computer
//! itself stopping, a fault the system's core went on from. The host keeps the records in the person's private crash
//! directory and answers them through its `tilo.crash-records` actions, the
//! door an agent or a script uses too; About lists what each says happened
//! and reads one record's text only when its page opens. Nothing leaves the
//! computer.
//!
//! Who the person is lives in Users, which reads the identities from the
//! one place that keeps them; About shows no second copy that could
//! disagree with it. Backing up the identity is offered there too; its
//! action stays [`AboutEffect::BackUpIdentity`].

use std::cell::{OnceCell, RefCell};
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_actions::remote::crash_records::{
    self as records, CrashKind, CrashRecord, CrashRecordText, StopReason,
};
use ui::tilo_resource;
use ui::widget::{Element, Widget};

use crate::area::AreaContext;
use crate::categories::Category;
use crate::effects::SettingsEffect;
use crate::i18n::t;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    SettingsSection, block, block_section, hero, note, row, rows, section, thing_row, value_row,
};

crate::effects::effect_family!(AboutEffect => answer);

/// What About offers to do with this device's identity.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum AboutEffect {
    /// Open the identity backup, to carry the person's identity to a new
    /// device.
    #[effect(job, label = "Back up identity")]
    BackUpIdentity,
    /// Open a problem's page: what happened, when, and the record's text.
    #[effect(view, label = "Problem details")]
    OpenProblem {
        #[param("The record's id, as the host's crash-records list answers it")]
        record: String,
    },
}

/// Answer `effect` by opening the tool or the page it names.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: AboutEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    match effect {
        AboutEffect::BackUpIdentity => {
            if !app.reaches_devices() {
                return Err("only a window someone uses opens the identity backup".into());
            }
            Ok(app.job(
                cx,
                move || os_actions::open_tool(portable_identity_backup_intent(), None),
                |_: &SettingsApp, opened: Result<(), String>, _| opened.map(|()| Effected::done()),
            ))
        }
        AboutEffect::OpenProblem { record } => {
            let area = area(app).ok_or_else(|| t("settings-about-problem-missing"))?;
            area.model().borrow_mut().open_problem(&record)?;
            Ok(Effected::done())
        }
    }
}

/// A fact's row, when the fact could be read.
fn fact_row(label: &str, value: &str) -> Option<provider_settings_ui::widgets::Item> {
    (value != "unknown").then(|| value_row(label, value))
}

pub const PORTABLE_BACKUP_BUTTON: &str = "about.identity-backup.button";

pub(crate) fn portable_identity_backup_intent() -> os_actions::ToolIntent {
    os_actions::ToolIntent::IdentityBackup
}

/// Snapshot of the data the About pane reads at construction time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutInfo {
    pub pretty_name: String,
    pub kernel: String,
    pub uptime: String,
    pub hostname: String,
    pub arch: String,
    pub cpu: String,
    pub memory: String,
}

impl AboutInfo {
    pub fn from_system() -> Self {
        Self::from_paths(
            Path::new("/etc/os-release"),
            Path::new("/proc/sys/kernel/osrelease"),
            Path::new("/proc/uptime"),
            Path::new("/proc/sys/kernel/hostname"),
            &[Path::new("/proc/cpuinfo"), Path::new("/sys/devices/soc0")],
            Path::new("/proc/meminfo"),
        )
    }

    pub fn from_paths(
        os_release: &Path,
        kernel_path: &Path,
        uptime_path: &Path,
        hostname_path: &Path,
        [cpuinfo_path, soc_dir]: &[&Path; 2],
        meminfo_path: &Path,
    ) -> Self {
        let pretty_name = fs::read_to_string(os_release)
            .ok()
            .and_then(|s| parse_os_release_pretty_name(&s))
            .unwrap_or_else(|| "unknown".to_string());
        let kernel = fs::read_to_string(kernel_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let uptime = fs::read_to_string(uptime_path)
            .ok()
            .and_then(|s| parse_uptime_seconds(&s))
            .map_or_else(|| "unknown".to_string(), format_uptime);
        let hostname = fs::read_to_string(hostname_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "this-device".to_string());
        let cpu = fs::read_to_string(cpuinfo_path)
            .ok()
            .and_then(|s| parse_cpu_model(&s))
            .or_else(|| soc_name(soc_dir))
            .unwrap_or_else(|| "unknown".to_string());
        let memory = fs::read_to_string(meminfo_path)
            .ok()
            .and_then(|s| parse_mem_total_kb(&s))
            .map_or_else(|| "unknown".to_string(), format_memory);
        // Compile-time target arch equals the running machine's arch — an
        // honest fact that needs no syscall.
        let arch = std::env::consts::ARCH.to_string();
        Self {
            pretty_name,
            kernel,
            uptime,
            hostname,
            arch,
            cpu,
            memory,
        }
    }
}

fn fact_sections(info: &AboutInfo) -> Vec<SettingsSection> {
    // The device's name in the accent, the OS beneath: said once.
    let mut hero_column = ui::widget::Column::new()
        .with_no_padding()
        .push(hero(ui::icons::Icon::Monitor, &info.hostname));
    if info.pretty_name != "unknown" {
        hero_column = hero_column.push(Box::new(
            ui::widget::Text::new(&info.pretty_name)
                .with_role(ui::tokens::typography::TextRole::Body),
        ));
    }
    let mut sections = vec![block_section(Box::new(hero_column))];

    let system: Vec<_> = [
        fact_row(&t("settings-about-kernel"), &info.kernel),
        fact_row(&t("settings-about-architecture"), &info.arch),
        fact_row(&t("settings-about-uptime"), &info.uptime),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !system.is_empty() {
        sections.push(section(&t("settings-about-system-card"), rows(system)));
    }

    let hardware: Vec<_> = [
        fact_row(&t("settings-about-processor"), &info.cpu),
        fact_row(&t("settings-about-memory"), &info.memory),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !hardware.is_empty() {
        sections.push(section(&t("settings-about-hardware-card"), rows(hardware)));
    }
    sections
}

crate::area::pane_area! {
    AboutArea;
    fn poll(&self) {
        AboutArea::poll(self);
    }
    fn busy(&self) -> bool {
        AboutArea::busy(self)
    }
}

/// The window's [`AboutArea`].
pub fn area(app: &SettingsApp) -> Option<&AboutArea> {
    app.areas().find()
}

/// A call to the host's records, answering on its state.
type RecordsCall = ui::InvocationCall;

/// What lists the problems this computer recorded and reads one. Each call
/// answers on the state it returns; nothing here waits.
pub trait ProblemRecords {
    fn list(&self) -> RecordsCall;
    fn read(&self, id: &str) -> RecordsCall;
}

/// The host's records, read through the action door as the person using
/// Settings, as an agent reads them.
struct RemoteRecords {
    remote: ui::RemoteActions,
}

impl ProblemRecords for RemoteRecords {
    fn list(&self) -> RecordsCall {
        use ui::tilo_actions::remote::InvokeArgument;
        self.remote
            .invoke(records::target(records::LIST), InvokeArgument::None, None)
    }

    fn read(&self, id: &str) -> RecordsCall {
        use ui::tilo_actions::remote::InvokeArgument;
        let mut arguments = serde_json::Map::new();
        arguments.insert("id".into(), id.into());
        self.remote.invoke(
            records::target(records::READ),
            InvokeArgument::Json(arguments),
            None,
        )
    }
}

/// What the about area keeps for the window: the model, made when the pane
/// or an effect first needs it.
pub struct AboutArea {
    /// Whether it asks the host for the problems this device recorded.
    devices: bool,
    /// The pages the panes drilled into: a problem's page among them.
    pages: crate::pages::PanePages,
    /// The window's client for the action door, shared with the other
    /// areas that act through the host.
    remote: ui::RemoteActions,
    model: OnceCell<Rc<RefCell<AboutModel>>>,
    /// Moves whenever what the page shows of the model does.
    shown: LocalState<u64>,
}

impl AboutArea {
    pub fn new(cx: &AreaContext, remote: ui::RemoteActions) -> Self {
        Self {
            devices: cx.devices,
            pages: cx.pages.clone(),
            remote,
            model: OnceCell::new(),
            shown: LocalState::new(0),
        }
    }

    /// The area over `model` in place of the one it would make: a test's.
    #[cfg(test)]
    fn over(cx: &AreaContext, model: AboutModel) -> Self {
        let area = Self::new(cx, ui::RemoteActions::current());
        let _ = area.model.set(Rc::new(RefCell::new(model)));
        area
    }

    /// The model: this device's facts, and the problems it recorded when
    /// the window reaches the device.
    pub fn model(&self) -> &Rc<RefCell<AboutModel>> {
        self.model.get_or_init(|| {
            let source = self.devices.then(|| {
                Rc::new(RemoteRecords {
                    remote: self.remote.clone(),
                }) as Rc<dyn ProblemRecords>
            });
            Rc::new(RefCell::new(AboutModel::new(
                self.pages.clone(),
                AboutInfo::from_system(),
                source,
            )))
        })
    }

    /// Take in what the host answered, if the model exists, and move what
    /// the page is keyed by when what it shows changed.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get()
            && let Ok(mut model) = model.try_borrow_mut()
            && model.poll()
        {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    /// Whether the host still owes an answer the page waits for.
    fn busy(&self) -> bool {
        self.model
            .get()
            .and_then(|model| model.try_borrow().ok())
            .is_some_and(|model| model.busy())
    }
}

/// The problems as far as they have been read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Problems {
    Reading,
    Read(Vec<CrashRecord>),
    Unreadable,
}

/// The record a problem's page shows, as far as it has been read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Reading,
    Read(CrashRecordText),
    Unreadable,
}

/// The problem whose page is open.
struct Opened {
    id: String,
    call: Option<RecordsCall>,
    shown: Shown,
}

/// The most problems the page lists, newest first; an agent lists them all.
const LISTED: usize = 50;

/// The key of the page of the record `id`.
fn page_key(id: &str) -> String {
    format!("problem:{id}")
}

pub struct AboutModel {
    /// The pages the panes drilled into: a problem's page among them.
    pages: crate::pages::PanePages,
    info: AboutInfo,
    source: Option<Rc<dyn ProblemRecords>>,
    problems: Problems,
    listing: Option<RecordsCall>,
    opened: Option<Opened>,
    pages_seen: u64,
    changed: bool,
}

impl AboutModel {
    /// The model over `info`, asking `source` for the problems; without one
    /// (a screenshot, a test of the facts) there are none.
    pub fn new(
        pages: crate::pages::PanePages,
        info: AboutInfo,
        source: Option<Rc<dyn ProblemRecords>>,
    ) -> Self {
        let listing = source.as_ref().map(|source| source.list());
        let problems = if listing.is_some() {
            Problems::Reading
        } else {
            Problems::Read(Vec::new())
        };
        let pages_seen = pages.revision();
        Self {
            pages,
            info,
            source,
            problems,
            listing,
            opened: None,
            pages_seen,
            changed: true,
        }
    }

    /// The problems listed, once read.
    fn records(&self) -> &[CrashRecord] {
        match &self.problems {
            Problems::Read(records) => records,
            Problems::Reading | Problems::Unreadable => &[],
        }
    }

    /// Open the page of the problem `id`.
    ///
    /// # Errors
    ///
    /// No problem listed is called `id`.
    pub fn open_problem(&mut self, id: &str) -> Result<(), String> {
        let record = self
            .records()
            .iter()
            .find(|record| record.id == id)
            .ok_or_else(|| t("settings-about-problem-missing"))?;
        let title = kind_label(record.kind);
        self.pages.open(Category::About, page_key(id), title);
        self.follow_page();
        Ok(())
    }

    /// Read the record of the problem whose page is open, and forget the
    /// one whose page closed.
    fn follow_page(&mut self) {
        let wanted = self
            .pages
            .top(Category::About)
            .and_then(|page| page.key.strip_prefix("problem:").map(str::to_owned));
        if wanted.as_deref() == self.opened.as_ref().map(|opened| opened.id.as_str()) {
            return;
        }
        self.changed = true;
        self.opened = wanted.map(|id| Opened {
            call: self.source.as_ref().map(|source| source.read(&id)),
            shown: Shown::Reading,
            id,
        });
    }

    /// Take in the host's answers and follow the page shown.
    fn take_answers(&mut self) {
        if let Some(answer) = self.listing.as_ref().and_then(crate::device_fleet::settled) {
            self.listing = None;
            self.problems = answer
                .ok()
                .and_then(|output| serde_json::from_value(output.value?).ok())
                .map_or(Problems::Unreadable, Problems::Read);
            self.changed = true;
        }
        let revision = self.pages.revision();
        if revision != self.pages_seen {
            self.pages_seen = revision;
            self.follow_page();
        }
        if let Some(opened) = self.opened.as_mut()
            && let Some(answer) = opened.call.as_ref().and_then(crate::device_fleet::settled)
        {
            opened.call = None;
            opened.shown = answer
                .ok()
                .and_then(|output| serde_json::from_value(output.value?).ok())
                .map_or(Shown::Unreadable, Shown::Read);
            self.changed = true;
        }
    }

    fn problems_section(&self, cx: &ViewCx<Settings>) -> SettingsSection {
        let items = match &self.problems {
            Problems::Reading => vec![note(&t("settings-about-problems-reading"))],
            Problems::Unreadable => vec![note(&t("settings-about-problems-unreadable"))],
            Problems::Read(records) if records.is_empty() => {
                vec![note(&t("settings-about-problems-none"))]
            }
            Problems::Read(records) => {
                let mut items: Vec<_> = records
                    .iter()
                    .take(LISTED)
                    .map(|record| problem_row(cx, record))
                    .collect();
                items.push(note(&t("settings-about-problems-private")));
                items
            }
        };
        section(&t("settings-about-problems-card"), rows(items))
    }

    /// The page of the problem `opened`: what happened, then the record.
    fn problem_page(&self, opened: &Opened) -> Box<dyn Widget> {
        let listed = self.records().iter().find(|record| record.id == opened.id);
        let record = match &opened.shown {
            Shown::Read(text) => Some(&text.record),
            Shown::Reading | Shown::Unreadable => listed,
        };
        let mut sections = Vec::new();
        if let Some(record) = record {
            sections.push(section(
                &t("settings-about-problem-card"),
                rows(facts(record)),
            ));
        }
        let details = match &opened.shown {
            Shown::Reading => vec![note(&t("settings-about-problem-reading"))],
            Shown::Unreadable => vec![note(&t("settings-about-problem-missing"))],
            Shown::Read(text) => {
                let mut items = vec![block(Box::new(
                    ui::widget::Text::new(&text.text)
                        .with_role(ui::tokens::typography::TextRole::Code),
                ))];
                if text.truncated {
                    items.push(note(&t("settings-about-problem-truncated")));
                }
                items
            }
        };
        sections.push(section(&t("settings-about-problem-details"), rows(details)));
        provider_settings_ui::widgets::page(sections)
    }
}

/// A problem's row, opening its page.
fn problem_row(cx: &ViewCx<Settings>, record: &CrashRecord) -> provider_settings_ui::widgets::Item {
    let line = thing_row(
        &format!("settings.about.problem.{}", record.id),
        &kind_label(record.kind),
        &[when(record)],
        None,
    );
    row(Box::new(cx.actions().bind_effect_invocation(
        line,
        &SettingsEffect::from(AboutEffect::OpenProblem {
            record: record.id.clone(),
        }),
    )))
}

/// When `record` happened, as the person's formats write it.
fn when(record: &CrashRecord) -> String {
    ui::format::timestamp(&ui::format::UnixSecs(record.at))
}

/// What happened, in the person's words.
fn kind_label(kind: CrashKind) -> String {
    t(match kind {
        CrashKind::Froze => "settings-about-problem-froze",
        CrashKind::Crashed => "settings-about-problem-crashed",
        CrashKind::PartRestarted => "settings-about-problem-part-restarted",
        CrashKind::ComputerStopped => "settings-about-problem-computer-stopped",
        CrashKind::SystemFault => "settings-about-problem-system-fault",
    })
}

fn reason_label(reason: StopReason) -> String {
    t(match reason {
        StopReason::Panic => "settings-about-problem-reason-panic",
        StopReason::Oops => "settings-about-problem-reason-oops",
        StopReason::Emergency => "settings-about-problem-reason-emergency",
    })
}

/// The facts of `record`, each it knows on a row of its own.
fn facts(record: &CrashRecord) -> Vec<provider_settings_ui::widgets::Item> {
    let mut facts = vec![
        value_row(&t("settings-about-problem-what"), &kind_label(record.kind)),
        value_row(&t("settings-about-problem-when"), &when(record)),
    ];
    if let Some(reason) = record.reason {
        facts.push(value_row(
            &t("settings-about-problem-reason"),
            &reason_label(reason),
        ));
    }
    if let Some(version) = &record.version {
        facts.push(value_row(&t("settings-about-problem-version"), version));
    }
    if let Some(build) = &record.build {
        facts.push(value_row(&t("settings-about-problem-build"), build));
    }
    facts
}

impl AboutModel {
    /// The page as the model shows it now: the facts and the problems, or
    /// the page of the problem opened.
    fn page(&self, cx: &ViewCx<Settings>) -> Element {
        if let Some(opened) = &self.opened {
            return self.problem_page(opened);
        }
        let mut sections = fact_sections(&self.info);
        sections.push(self.problems_section(cx));
        provider_settings_ui::widgets::page(sections)
    }

    /// Take in the host's answers and follow the page shown; whether what
    /// the page shows changed.
    fn poll(&mut self) -> bool {
        self.take_answers();
        std::mem::take(&mut self.changed)
    }

    /// Whether the host still owes an answer.
    fn busy(&self) -> bool {
        self.listing.is_some()
            || self
                .opened
                .as_ref()
                .is_some_and(|opened| opened.call.is_some())
    }
}

/// The About page as view code over what the area reads: built again
/// whenever what it shows changes ([`AboutArea::poll`]).
pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let model = Rc::clone(held.model());
    model.borrow_mut().poll();
    Some(cx.switch(&held.shown, move |cx, _| model.borrow().page(cx)))
}

/// The system-on-chip's own name, where the kernel publishes it
/// (`/sys/devices/soc0`): its family and part, `Snapdragon X1E80100`. A
/// family given only as a JEDEC vendor code names no processor.
pub(crate) fn soc_name(soc_dir: &Path) -> Option<String> {
    let read = |name: &str| {
        fs::read_to_string(soc_dir.join(name))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let family = read("family").filter(|family| !family.starts_with("jep106:"))?;
    Some(match read("machine") {
        Some(machine) => format!("{family} {machine}"),
        None => family,
    })
}

pub(crate) fn parse_os_release_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
            let unquoted = rest.trim_matches('"').trim_matches('\'').to_string();
            if !unquoted.is_empty() {
                return Some(unquoted);
            }
        }
    }
    None
}

/// Pull the CPU model from `/proc/cpuinfo` — the first `model name` line.
pub(crate) fn parse_cpu_model(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some((key, val)) = line.split_once(':')
            && key.trim() == "model name"
        {
            let v = val.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Pull total memory (in kB) from `/proc/meminfo`'s `MemTotal` line.
pub(crate) fn parse_mem_total_kb(contents: &str) -> Option<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok());
        }
    }
    None
}

/// Format a memory size reported in KiB (as `/proc/meminfo` does) with
/// binary units, `15.6 GiB`.
pub(crate) fn format_memory(kb: u64) -> String {
    ui::format::memory(kb.saturating_mul(1024))
}

pub(crate) fn parse_uptime_seconds(contents: &str) -> Option<f64> {
    contents
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
}

pub(crate) fn format_uptime(seconds: f64) -> String {
    let dur = Duration::from_secs_f64(seconds.max(0.0));
    let total = dur.as_secs();
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The /etc/os-release, /proc/uptime, /proc/cpuinfo and /proc/meminfo
    /// parsers and their display formats.
    #[test]
    fn system_facts_parse_and_format() {
        for (input, want) in [
            (
                "NAME=Fedora\nPRETTY_NAME=\"Fedora Linux 42\"\nID=fedora\n",
                Some("Fedora Linux 42"),
            ),
            ("PRETTY_NAME=Fedora42\n", Some("Fedora42")),
            ("NAME=Foo\n", None),
        ] {
            assert_eq!(
                parse_os_release_pretty_name(input).as_deref(),
                want,
                "{input:?}"
            );
        }

        // Only the first field is the uptime.
        assert_eq!(parse_uptime_seconds("12345.67 6789.01\n"), Some(12345.67));
        for (seconds, want) in [(93_780.0, "1d 2h 3m"), (7_380.0, "2h 3m"), (420.0, "7m")] {
            assert_eq!(format_uptime(seconds), want, "{seconds}s");
        }

        // The first model name wins.
        let s = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7 7840U\nprocessor\t: 1\nmodel name\t: AMD Ryzen 7 7840U\n";
        assert_eq!(parse_cpu_model(s).as_deref(), Some("AMD Ryzen 7 7840U"));
        assert!(parse_cpu_model("no model here\n").is_none());

        // 16 GiB in kB.
        assert_eq!(
            parse_mem_total_kb("MemTotal:       16330124 kB\n"),
            Some(16_330_124)
        );
        assert!(parse_mem_total_kb("SwapTotal: 0 kB\n").is_none());
        assert_eq!(format_memory(16_330_124), "15.6 GiB");
        assert_eq!(format_memory(512 * 1024), "512 MiB");
    }

    /// Missing system files fall back to honest placeholders; the backup
    /// action is a shell-owned typed intent.
    #[test]
    fn about_info_falls_back_when_nothing_can_be_read() {
        let missing = Path::new("/nonexistent/tilo/settings/test/path");
        let info = AboutInfo::from_paths(
            missing,
            missing,
            missing,
            missing,
            &[missing, missing],
            missing,
        );
        assert_eq!(info.pretty_name, "unknown");
        assert_eq!(info.kernel, "unknown");
        assert_eq!(info.uptime, "unknown");
        assert_eq!(info.hostname, "this-device");
        assert_eq!(info.cpu, "unknown");
        assert_eq!(info.memory, "unknown");
        // Architecture is a compile-time fact, always present.
        assert!(!info.arch.is_empty());

        let intent = portable_identity_backup_intent();
        assert_eq!(intent, os_actions::ToolIntent::IdentityBackup);
        assert_eq!(intent.package_id(), None);
    }

    #[test]
    fn about_names_the_real_processor_or_no_processor_and_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        let cpuinfo = dir.path().join("cpuinfo");
        // An Arm CPU reports no model name.
        std::fs::write(
            &cpuinfo,
            "processor\t: 0\nCPU implementer\t: 0x51\nCPU part\t: 0x001\n",
        )
        .unwrap();
        let soc = dir.path().join("soc0");
        std::fs::create_dir(&soc).unwrap();
        std::fs::write(soc.join("family"), "Snapdragon\n").unwrap();
        std::fs::write(soc.join("machine"), "X1E80100\n").unwrap();
        let missing = Path::new("/nonexistent/tilo/settings/test/path");
        let info = AboutInfo::from_paths(
            missing,
            missing,
            missing,
            missing,
            &[&cpuinfo, &soc],
            missing,
        );
        assert_eq!(info.cpu, "Snapdragon X1E80100", "the chip's own name");
        let text = page_shown(info);
        assert!(
            text.iter().any(|line| line == "Snapdragon X1E80100"),
            "{text:#?}"
        );

        std::fs::write(soc.join("family"), "jep106:0070\n").unwrap();
        let info = AboutInfo::from_paths(
            missing,
            missing,
            missing,
            missing,
            &[&cpuinfo, &soc],
            missing,
        );
        let text = page_shown(info);
        for gone in [
            t("settings-about-processor"),
            "unknown".to_owned(),
            t("settings-about-memory"),
        ] {
            assert!(
                !text.contains(&gone),
                "a fact that cannot be read has no row: {gone} in {text:#?}"
            );
        }
        // Who the person is is Users' to say; About keeps no second copy.
        for gone in ["Person", "Not connected", "Workspace identity", "Identity"] {
            assert!(!text.iter().any(|line| line == gone), "{gone}: {text:#?}");
        }
    }

    /// The host's records, as fixtures: a desktop that stopped responding
    /// and the computer stopping, as the shell's `tilo.crash-records`
    /// answers them.
    struct FixtureRecords {
        reads: std::cell::RefCell<Vec<String>>,
    }

    const FREEZE: &str = "2026-10-24-225201-412-freeze.log";
    const STOPPED: &str = "2026-10-25-081400-3f9a0c1d2e4b5a69-stopped.log";

    fn fixture_records() -> Vec<CrashRecord> {
        vec![
            CrashRecord {
                id: STOPPED.into(),
                at: 1_761_376_440,
                kind: CrashKind::ComputerStopped,
                reason: Some(StopReason::Panic),
                version: None,
                build: None,
            },
            CrashRecord {
                id: FREEZE.into(),
                at: 1_761_342_721,
                kind: CrashKind::Froze,
                reason: None,
                version: Some("0.1.0".into()),
                build: Some("9f2c41d07be3a65e".into()),
            },
        ]
    }

    fn answered(value: serde_json::Value) -> RecordsCall {
        ui::Arriving::ready(ui::tilo_actions::InvocationUpdate::Succeeded {
            invocation: ui::tilo_actions::InvocationId::new(),
            output: ui::tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        })
    }

    impl ProblemRecords for FixtureRecords {
        fn list(&self) -> RecordsCall {
            answered(serde_json::to_value(fixture_records()).unwrap())
        }

        fn read(&self, id: &str) -> RecordsCall {
            self.reads.borrow_mut().push(id.to_owned());
            let record = fixture_records()
                .into_iter()
                .find(|record| record.id == id)
                .unwrap();
            let text = if record.kind == CrashKind::ComputerStopped {
                "tilo computer stopped\nreason:    panic\n\n<0>[ 1843.212002] Kernel panic - not syncing: Oops: Fatal exception\n"
            } else {
                "tilo desktop freeze\n\nthread 412 (shell)\n   0: std::sys::sync::mutex::futex::Mutex::lock_contended\n"
            };
            answered(
                serde_json::to_value(CrashRecordText {
                    record,
                    text: text.into(),
                    truncated: false,
                })
                .unwrap(),
            )
        }
    }

    fn info() -> AboutInfo {
        let missing = Path::new("/nonexistent/tilo/settings/test/path");
        AboutInfo::from_paths(
            missing,
            missing,
            missing,
            missing,
            &[missing, missing],
            missing,
        )
    }

    /// A Settings window on About, whose area shows the model `model`
    /// makes in the window's context.
    fn window_on(
        model: impl FnOnce(crate::pages::PanePages) -> AboutModel + 'static,
    ) -> ui::prelude::TestApp<crate::resource::SettingsShell> {
        crate::area::replace_in_next_window(move |context| {
            AboutArea::over(context, model(context.pages.clone()))
        });
        let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, Category::About);
        app
    }

    /// The window's About model, changed by `change`; the page shows the
    /// change once the window settles.
    fn change_model<T: 'static>(
        app: &mut ui::prelude::TestApp<crate::resource::SettingsShell>,
        change: impl FnOnce(&mut AboutModel) -> T + 'static,
    ) -> T {
        let changed = app
            .with_widget::<crate::window::Inspected, _>(move |settings| {
                change(&mut area(settings).unwrap().model().borrow_mut())
            })
            .expect("the Settings window is mounted");
        app.settle();
        changed
    }

    /// What About's page announces for a device with the facts `info`.
    fn page_shown(info: AboutInfo) -> Vec<String> {
        let mut app = window_on(move |cx| AboutModel::new(cx, info, None));
        provider_settings_ui::widgets::announced_text(crate::effects::page_of(
            app.harness().root_widget(),
        ))
    }

    /// What the window announces.
    fn shown(app: &mut ui::prelude::TestApp<crate::resource::SettingsShell>) -> Vec<String> {
        provider_settings_ui::widgets::announced_text(app.harness().root_widget())
    }

    /// About lists a freeze and the computer stopping as the host answers
    /// them, in the person's words and without their stacks; opening one
    /// reads its record, and only then.
    #[test]
    fn about_lists_the_problems_and_opens_one_to_its_record() {
        let source = Rc::new(FixtureRecords {
            reads: std::cell::RefCell::default(),
        });
        let records = Rc::clone(&source) as Rc<dyn ProblemRecords>;
        let mut app = window_on(move |cx| {
            let model = AboutModel::new(cx, info(), Some(records));
            assert_eq!(model.problems, Problems::Reading, "read once asked");
            model
        });

        let listed = shown(&mut app);
        for shown in [
            t("settings-about-problems-card"),
            t("settings-about-problem-froze"),
            t("settings-about-problem-computer-stopped"),
            ui::format::timestamp(&ui::format::UnixSecs(1_761_342_721)),
            ui::format::timestamp(&ui::format::UnixSecs(1_761_376_440)),
            t("settings-about-problems-private"),
        ] {
            assert!(
                listed.iter().any(|line| line.contains(&shown)),
                "{shown} in {listed:#?}"
            );
        }
        assert!(
            !listed
                .iter()
                .any(|line| line.contains("Kernel panic") || line.contains("lock_contended")),
            "no record is read to list it: {listed:#?}"
        );
        assert!(source.reads.borrow().is_empty());

        assert!(change_model(&mut app, |model| model
            .open_problem("2026-10-20-000000-1.log")
            .is_err()));
        let title = change_model(&mut app, |model| {
            model.open_problem(STOPPED).unwrap();
            model.pages.top(Category::About).map(|page| page.title)
        });
        assert_eq!(title, Some(t("settings-about-problem-computer-stopped")));
        app.settle();
        let page = shown(&mut app);
        for shown in [
            t("settings-about-problem-card"),
            t("settings-about-problem-reason-panic"),
            "Kernel panic - not syncing".to_owned(),
        ] {
            assert!(
                page.iter().any(|line| line.contains(&shown)),
                "{shown} in {page:#?}"
            );
        }
        assert_eq!(*source.reads.borrow(), [STOPPED.to_owned()]);

        change_model(&mut app, |model| model.pages.back());
        app.settle();
        let back = shown(&mut app);
        assert!(
            back.contains(&t("settings-about-problem-froze")),
            "{back:#?}"
        );
    }

    /// Without the host's records (a screenshot) About says none were
    /// recorded rather than inventing any.
    #[test]
    fn about_without_records_says_none_were_recorded() {
        let mut app = window_on(|cx| AboutModel::new(cx, info(), None));
        let text = shown(&mut app);
        assert!(
            text.contains(&t("settings-about-problems-none")),
            "{text:#?}"
        );
    }
}
