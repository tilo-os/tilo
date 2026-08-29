//! Printers pane — configured printers plus their live CUPS job queue.
//!
//! Printers are read from CUPS via `lpstat -p -d` ([`read_cups_printers`]);
//! the job queue is read from `lpstat -o` ([`parse_lpstat_jobs`]); and each
//! job's **Cancel** button really runs `cancel <job-id>` through
//! [`JobCanceller`], then re-reads the queue. When `lpstat` is absent (a
//! minimal container, a non-qOS host without CUPS, or `--shots` on CI) the
//! pane shows an **honest empty state** — it never fabricates printers, so a
//! machine with no CUPS reads as "no printers" rather than inventing a list.
//!
//! Adding and per-printer administration need privileged CUPS operations and
//! a dialog flow that lives outside this app, so those affordances are not
//! offered here rather than shipping buttons that can't act.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Column, Row, Spacer, Text, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::widgets::{El, caption, card, rows};

/// A configured printer and its readiness.
#[derive(Clone, Debug, PartialEq)]
pub struct Printer {
    pub name: String,
    pub location: String,
    pub state: PrinterState,
    pub default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrinterState {
    Ready,
    Printing,
    Paused,
    Offline,
}

impl PrinterState {
    fn label(self) -> &'static str {
        match self {
            PrinterState::Ready => "Ready",
            PrinterState::Printing => "Printing",
            PrinterState::Paused => "Paused",
            PrinterState::Offline => "Offline",
        }
    }

    fn color(self) -> vello::peniko::Color {
        let t = ui::Theme::dark();
        match self {
            PrinterState::Ready | PrinterState::Printing => t.status_success,
            PrinterState::Paused => t.text_dim,
            PrinterState::Offline => t.text_dim,
        }
    }
}

/// A live job in a printer's CUPS queue, as reported by `lpstat -o`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintJob {
    /// The CUPS job id, e.g. `Office_LaserJet-42` — the handle `cancel`
    /// takes.
    pub id: String,
    pub owner: String,
    /// Size in bytes as reported by CUPS.
    pub size: u64,
    /// Submission time text, verbatim from `lpstat`.
    pub when: String,
}

impl PrintJob {
    /// Human-readable size, e.g. `1.0 MB`.
    fn size_label(&self) -> String {
        let b = self.size as f64;
        if b >= 1_048_576.0 {
            format!("{:.1} MB", b / 1_048_576.0)
        } else if b >= 1024.0 {
            format!("{:.0} KB", b / 1024.0)
        } else {
            format!("{} B", self.size)
        }
    }
}

/// Side-effect interface for cancelling a CUPS job. Production runs
/// `cancel <id>`; tests inject a recorder.
pub trait JobCanceller: Send + Sync {
    fn cancel(&self, job_id: &str) -> Result<(), String>;
}

/// Production impl: `cancel <job-id>`.
pub struct RealCanceller;

impl JobCanceller for RealCanceller {
    fn cancel(&self, job_id: &str) -> Result<(), String> {
        let out = Command::new("cancel")
            .arg(job_id)
            .output()
            .map_err(|e| format!("cancel not available: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

impl Printer {
    #[cfg(test)]
    fn sample(name: &str, location: &str, state: PrinterState, default: bool) -> Self {
        Self {
            name: name.to_string(),
            location: location.to_string(),
            state,
            default,
        }
    }
}

/// The real printer list from CUPS, or an empty list when CUPS is absent.
/// We deliberately do **not** substitute a sample here: a machine with no
/// CUPS shows an honest empty state rather than fabricated printers.
fn printers_for_display() -> Vec<Printer> {
    read_cups_printers().unwrap_or_default()
}

/// A realistic printer list used only by tests / deterministic `--shots`,
/// never as a production fallback.
#[cfg(test)]
fn sample_printers() -> Vec<Printer> {
    vec![
        Printer::sample(
            "Office LaserJet",
            "Floor 2 · Copy room",
            PrinterState::Ready,
            true,
        ),
        Printer::sample("Front Desk Inkjet", "Reception", PrinterState::Ready, false),
        Printer::sample("Label Printer", "Shipping", PrinterState::Offline, false),
    ]
}

/// Read configured printers from CUPS via `lpstat -p -d`.
fn read_cups_printers() -> Option<Vec<Printer>> {
    if !have("lpstat") {
        return None;
    }
    let out = Command::new("lpstat").args(["-p", "-d"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_lpstat(&String::from_utf8_lossy(&out.stdout)))
}

/// Read the live job queue from `lpstat -o`. Returns an empty vec when
/// `lpstat` is missing or the queue is empty.
fn read_cups_jobs() -> Vec<PrintJob> {
    if !have("lpstat") {
        return Vec::new();
    }
    let Ok(out) = Command::new("lpstat").arg("-o").output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_lpstat_jobs(&String::from_utf8_lossy(&out.stdout))
}

fn have(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Parse `lpstat -p -d` output into [`Printer`]s.
fn parse_lpstat(stdout: &str) -> Vec<Printer> {
    let default = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("system default destination:"))
        .map(|s| s.trim().to_string());

    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("printer ") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once(" is ") else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let lower = tail.to_ascii_lowercase();
        let state = if lower.contains("printing") {
            PrinterState::Printing
        } else if lower.contains("disabled") || lower.contains("stopped") {
            PrinterState::Paused
        } else if lower.contains("idle") {
            PrinterState::Ready
        } else {
            PrinterState::Offline
        };
        out.push(Printer {
            name: name.to_string(),
            location: "CUPS queue".to_string(),
            state,
            default: default.as_deref() == Some(name),
        });
    }
    out
}

/// Parse `lpstat -o` output into [`PrintJob`]s. Each line is
/// `<job-id> <owner> <size> <submission time…>`, e.g.
/// `Office_LaserJet-42  ada  1048576  Mon 04 Jul 2026 10:15:00 AM`.
pub fn parse_lpstat_jobs(stdout: &str) -> Vec<PrintJob> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(id), Some(owner), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // The id must look like `<dest>-<number>`; skip header/other noise.
        if !id
            .rsplit_once('-')
            .is_some_and(|(_, n)| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
        {
            continue;
        }
        let size = size.parse::<u64>().unwrap_or(0);
        let when = parts.collect::<Vec<_>>().join(" ");
        out.push(PrintJob {
            id: id.to_string(),
            owner: owner.to_string(),
            size,
            when,
        });
    }
    out
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Printers pane widget — holds the printer list, live job queue, and the
/// canceller so per-job Cancel really runs.
pub struct PrintersPane {
    printers: Vec<Printer>,
    jobs: Vec<PrintJob>,
    canceller: Arc<dyn JobCanceller>,
    jobs_lister: Arc<dyn Fn() -> Vec<PrintJob> + Send + Sync>,
    dispatched_cancels: Vec<String>,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for PrintersPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PrintersPane {
    pub fn new() -> Self {
        let jobs_lister: Arc<dyn Fn() -> Vec<PrintJob> + Send + Sync> = Arc::new(read_cups_jobs);
        let jobs = jobs_lister();
        Self {
            printers: printers_for_display(),
            jobs,
            canceller: Arc::new(RealCanceller),
            jobs_lister,
            dispatched_cancels: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test / `--shots` constructor: pin the printers, the initial queue, and
    /// the canceller. The queue lister returns the pinned jobs so a Cancel
    /// re-read is deterministic.
    pub fn with_data(
        printers: Vec<Printer>,
        jobs: Vec<PrintJob>,
        canceller: Arc<dyn JobCanceller>,
    ) -> Self {
        let snapshot = jobs.clone();
        let jobs_lister: Arc<dyn Fn() -> Vec<PrintJob> + Send + Sync> =
            Arc::new(move || snapshot.clone());
        Self {
            printers,
            jobs,
            canceller,
            jobs_lister,
            dispatched_cancels: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    pub fn jobs(&self) -> &[PrintJob] {
        &self.jobs
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let printer_card = if self.printers.is_empty() {
            // Honest empty state — no CUPS printers configured (or no CUPS at
            // all). We never invent a list here.
            card(
                "Printers",
                rows(vec![caption(
                    "No printers configured. Printers set up through CUPS will \
                     appear here.",
                )]),
            )
        } else {
            let mut printer_rows: Vec<El> = Vec::new();
            for p in &self.printers {
                printer_rows.push(printer_row(p));
            }
            card("Printers", rows(printer_rows))
        };

        let job_card = if self.jobs.is_empty() {
            card("Print queue", rows(vec![caption("No active jobs.")]))
        } else {
            let mut job_rows: Vec<El> = Vec::new();
            for j in &self.jobs {
                job_rows.push(job_row(j));
            }
            card("Print queue", rows(job_rows))
        };

        crate::widgets::cards(vec![printer_card, job_card])
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(PrintersPane::new())
}

fn printer_row(p: &Printer) -> El {
    let name = if p.default {
        format!("{}  (default)", p.name)
    } else {
        p.name.clone()
    };
    // Name + status chip inline on the top line; location beneath. A single
    // leading Text keeps the status on the same line as the name.
    let top = Row::new()
        .push(Box::new(Text::new(&name).with_size(13.0)))
        .push(Box::new(Spacer::new()))
        .push(crate::widgets::status_pill(
            p.state.label(),
            p.state.color(),
        ));
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(top))
            .push(Box::new(
                Text::new(&p.location)
                    .with_size(11.0)
                    .with_color(ui::Theme::dark().text_dim),
            )),
    )
}

/// A job row shows the job id, owner + size, and a real Cancel button.
fn job_row(j: &PrintJob) -> El {
    let cancel_id = format!("settings.printers.cancel.{}", slug(&j.id));
    let cancel_key = cancel_id.clone();

    // Job id + Cancel inline on the top line; owner · size beneath.
    let top = Row::new()
        .push(Box::new(Text::new(&j.id).with_size(13.0)))
        .push(Box::new(Spacer::new()))
        .push(Box::new(Button::new(
            &cancel_id,
            "Cancel",
            Some(&cancel_key),
        )));
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(top))
            .push(Box::new(
                Text::new(&format!("{} · {}", j.owner, j.size_label()))
                    .with_size(11.0)
                    .with_color(ui::Theme::dark().text_dim),
            )),
    )
}

impl Widget for PrintersPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Per-job Cancel: when a job's cancel key is set, run `cancel <id>`
        // once, clear the key, and re-read the queue so the cancelled job
        // drops out. Dedup per id so a single click doesn't re-fire.
        let jobs: Vec<(String, String)> = self
            .jobs
            .iter()
            .map(|j| {
                (
                    j.id.clone(),
                    format!("settings.printers.cancel.{}", slug(&j.id)),
                )
            })
            .collect();
        let mut cancelled_any = false;
        for (id, key) in jobs {
            let pressed = matches!(
                state.get(root, key.as_str()),
                Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
            );
            if !pressed || self.dispatched_cancels.contains(&id) {
                continue;
            }
            self.dispatched_cancels.push(id.clone());
            let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
            if let Err(e) = self.canceller.cancel(&id) {
                warn!(target: "qos::settings", op = "cancel_job", job = %id, error = %e, "print job cancel failed");
            }
            cancelled_any = true;
        }
        if cancelled_any {
            self.jobs = (self.jobs_lister)()
                .into_iter()
                .filter(|j| !self.dispatched_cancels.contains(&j.id))
                .collect();
            self.structure_dirty = true;
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "PrintersPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingCanceller {
        calls: Mutex<Vec<String>>,
    }
    impl JobCanceller for RecordingCanceller {
        fn cancel(&self, job_id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(job_id.to_string());
            Ok(())
        }
    }

    fn sample_jobs() -> Vec<PrintJob> {
        vec![
            PrintJob {
                id: "Office_LaserJet-42".into(),
                owner: "ada".into(),
                size: 1_048_576,
                when: "Mon".into(),
            },
            PrintJob {
                id: "Front_Desk-7".into(),
                owner: "ada".into(),
                size: 2048,
                when: "Mon".into(),
            },
        ]
    }

    #[test]
    fn parse_lpstat_extracts_names_states_and_default() {
        let stdout = "\
printer Office_LaserJet is printing job 42.  enabled since Mon\n\
printer Front_Desk is idle.  enabled since Mon\n\
printer Old_Dot_Matrix is disabled since Tue.\n\
system default destination: Front_Desk\n\
";
        let printers = parse_lpstat(stdout);
        assert_eq!(printers.len(), 3);
        assert_eq!(printers[0].state, PrinterState::Printing);
        assert!(printers[1].default);
        assert_eq!(printers[2].state, PrinterState::Paused);
    }

    #[test]
    fn parse_lpstat_jobs_extracts_id_owner_size() {
        let stdout = "\
Office_LaserJet-42        ada          1048576   Mon 04 Jul 2026 10:15:00 AM\n\
Front_Desk-7             charles      2048      Mon 04 Jul 2026 10:16:00 AM\n\
this is not a job line\n\
";
        let jobs = parse_lpstat_jobs(stdout);
        assert_eq!(jobs.len(), 2, "the non-job line is skipped: {jobs:?}");
        assert_eq!(jobs[0].id, "Office_LaserJet-42");
        assert_eq!(jobs[0].owner, "ada");
        assert_eq!(jobs[0].size, 1_048_576);
        assert_eq!(jobs[0].size_label(), "1.0 MB");
        assert_eq!(jobs[1].id, "Front_Desk-7");
    }

    #[test]
    fn empty_queue_yields_no_jobs() {
        assert!(parse_lpstat_jobs("").is_empty());
        assert!(parse_lpstat_jobs("\n  \n").is_empty());
    }

    /// Clicking a job's Cancel runs `cancel <id>` exactly once and drops the
    /// job from the live queue.
    #[test]
    fn cancel_button_cancels_job_and_refreshes_queue() {
        let canceller = Arc::new(RecordingCanceller::default());
        let mut pane = PrintersPane::with_data(sample_printers(), sample_jobs(), canceller.clone());
        assert_eq!(pane.jobs().len(), 2);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Press Cancel on the first job.
        let key = format!("settings.printers.cancel.{}", slug("Office_LaserJet-42"));
        let _ = doc.put(&root, key.as_str(), automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root); // must not re-cancel
        assert_eq!(
            canceller.calls.lock().unwrap().as_slice(),
            ["Office_LaserJet-42"]
        );
        assert!(
            pane.take_structure_dirty(),
            "cancel refreshes the queue view"
        );
        assert!(
            !pane.jobs().iter().any(|j| j.id == "Office_LaserJet-42"),
            "the cancelled job left the queue: {:?}",
            pane.jobs()
        );
    }

    #[test]
    fn builds_layout_with_and_without_jobs() {
        let canceller = Arc::new(RecordingCanceller::default());
        for jobs in [sample_jobs(), vec![]] {
            let mut pane = PrintersPane::with_data(sample_printers(), jobs, canceller.clone());
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        }
    }

    /// The production display list is the real CUPS list (or empty) — it must
    /// never fall back to the illustrative sample. On a box without CUPS this
    /// is an empty vec; on a box with CUPS it is the real printers. Either way
    /// it can never contain the sample's fabricated "Office `LaserJet`".
    #[test]
    fn production_list_never_fabricates_the_sample() {
        let real = printers_for_display();
        assert!(
            !real.iter().any(|p| p.name == "Office LaserJet"),
            "the sample must not leak into the production list: {real:?}"
        );
    }

    /// With no CUPS printers the pane renders an honest empty state and builds
    /// cleanly rather than inventing rows.
    #[test]
    fn empty_printer_list_builds_honest_state() {
        let canceller = Arc::new(RecordingCanceller::default());
        let mut pane = PrintersPane::with_data(vec![], vec![], canceller);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        assert!(pane.jobs().is_empty());
    }
}
