//! Printers pane — configured printers plus their recent queue / job
//! status.
//!
//! Reads real printers from CUPS via `lpstat -p` when it's available
//! ([`read_cups_printers`]), so on a machine with configured printers the
//! panel reflects them. When `lpstat` is absent or reports no printers (a
//! minimal container, or `--shots` on CI) it falls back to a realistic
//! sample so the surface always reads as designed. The job queue is still a
//! sample today (parsing `lpstat -o` per-printer is a follow-up that slots
//! into [`jobs_for_display`] the same way).

use std::process::Command;

use ui::widget::{Button, Column, ProgressBar, Row, Spacer, Text, Widget};

use crate::widgets::{card, card_with_action, caption, rows, El};

pub const ADD_PRINTER_KEY: &str = "printers.add";

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

/// A job in a printer's queue.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintJob {
    pub document: String,
    pub owner: String,
    pub printer: String,
    /// 0..1 completion; `None` for queued-but-not-started.
    pub progress: Option<f32>,
    pub pages: u32,
}

impl Printer {
    fn sample(name: &str, location: &str, state: PrinterState, default: bool) -> Self {
        Self {
            name: name.to_string(),
            location: location.to_string(),
            state,
            default,
        }
    }
}

fn printers_for_display() -> Vec<Printer> {
    match read_cups_printers() {
        Some(printers) if !printers.is_empty() => printers,
        _ => sample_printers(),
    }
}

/// The fallback sample, shown when no real CUPS printers are configured.
fn sample_printers() -> Vec<Printer> {
    vec![
        Printer::sample(
            "Office LaserJet",
            "Floor 2 · Copy room",
            PrinterState::Printing,
            true,
        ),
        Printer::sample("Front Desk Inkjet", "Reception", PrinterState::Ready, false),
        Printer::sample("Label Printer", "Shipping", PrinterState::Offline, false),
    ]
}

/// Read configured printers from CUPS via `lpstat -p -d`. Returns `None`
/// when `lpstat` isn't installed or the call fails; an empty `Vec` when it
/// runs but reports no printers. Best-effort and unprivileged.
fn read_cups_printers() -> Option<Vec<Printer>> {
    let which = Command::new("which")
        .arg("lpstat")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !which {
        return None;
    }
    let out = Command::new("lpstat").args(["-p", "-d"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Some(parse_lpstat(&stdout))
}

/// Parse `lpstat -p -d` output into [`Printer`]s.
///
/// `lpstat -p` prints one line per printer, e.g.:
/// `printer Office_LaserJet is idle.  enabled since …`
/// `printer Front_Desk is printing job 42.`
/// `lpstat -d` prints `system default destination: Office_LaserJet`.
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
        // `<name> is <state>.` — the name is up to " is ".
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
            // CUPS doesn't carry a human location in `lpstat -p`; show the
            // connection/queue name as a stand-in.
            location: "CUPS queue".to_string(),
            state,
            default: default.as_deref() == Some(name),
        });
    }
    out
}

fn jobs_for_display() -> Vec<PrintJob> {
    vec![
        PrintJob {
            document: "Q2-report.pdf".into(),
            owner: "ada".into(),
            printer: "Office LaserJet".into(),
            progress: Some(0.62),
            pages: 14,
        },
        PrintJob {
            document: "invoice-8842.pdf".into(),
            owner: "ada".into(),
            printer: "Office LaserJet".into(),
            progress: None,
            pages: 2,
        },
    ]
}

pub fn build() -> Box<dyn Widget> {
    build_with(&printers_for_display(), &jobs_for_display())
}

pub fn build_with(printers: &[Printer], jobs: &[PrintJob]) -> Box<dyn Widget> {
    let add_btn: El = Box::new(Button::new(
        "settings.printers.add",
        "\u{f0415}  Add printer",
        Some(ADD_PRINTER_KEY),
    ));

    let mut printer_rows: Vec<El> = Vec::new();
    for p in printers {
        printer_rows.push(printer_row(p));
    }
    let printer_card = card_with_action("Printers", add_btn, rows(printer_rows));

    let job_card = if jobs.is_empty() {
        card("Print queue", rows(vec![caption("No active jobs.")]))
    } else {
        let mut job_rows: Vec<El> = Vec::new();
        for j in jobs {
            job_rows.push(job_row(j));
        }
        card("Print queue", rows(job_rows))
    };

    crate::widgets::cards(vec![printer_card, job_card])
}

fn printer_row(p: &Printer) -> El {
    let name = if p.default {
        format!("{}  (default)", p.name)
    } else {
        p.name.clone()
    };
    let block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(&name).with_size(13.0)))
        .push(Box::new(
            Text::new(&p.location)
                .with_size(11.0)
                .with_color(ui::Theme::dark().text_dim),
        ));

    let manage_id = format!("settings.printers.manage.{}", slug(&p.name));
    let manage_key = format!("settings.printers.manage.{}", slug(&p.name));

    Box::new(
        Row::new()
            .push(Box::new(block))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Text::new(p.state.label())
                    .with_size(11.0)
                    .with_color(p.state.color()),
            ))
            .push(Box::new(Button::new(&manage_id, "Manage", Some(&manage_key)))),
    )
}

/// A job row shows the document, owner, and either a progress bar (for the
/// active job) or a "Queued" tag — the "job preview/status".
fn job_row(j: &PrintJob) -> El {
    let block = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Text::new(&format!("{} · {} pages", j.document, j.pages)).with_size(13.0),
        ))
        .push(Box::new(
            Text::new(&format!("{} → {}", j.owner, j.printer))
                .with_size(11.0)
                .with_color(ui::Theme::dark().text_dim),
        ));

    let status: El = match j.progress {
        Some(p) => Box::new(ProgressBar::new(p)),
        None => Box::new(
            Text::new("Queued")
                .with_size(11.0)
                .with_color(ui::Theme::dark().text_dim),
        ),
    };

    let cancel_id = format!("settings.printers.cancel.{}", slug(&j.document));
    let cancel_key = format!("settings.printers.cancel.{}", slug(&j.document));

    Box::new(
        Row::new()
            .push(Box::new(block))
            .push(Box::new(Spacer::new()))
            .push(status)
            .push(Box::new(Button::new(&cancel_id, "Cancel", Some(&cancel_key)))),
    )
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    #[test]
    fn builds_layout_with_jobs() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn empty_queue_renders_no_jobs_message() {
        let printers = printers_for_display();
        let mut pane = build_with(&printers, &[]);
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn active_job_has_progress_queued_job_does_not() {
        let jobs = jobs_for_display();
        assert!(jobs[0].progress.is_some());
        assert!(jobs[1].progress.is_none());
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
        assert_eq!(printers[0].name, "Office_LaserJet");
        assert_eq!(printers[0].state, PrinterState::Printing);
        assert!(!printers[0].default);
        assert_eq!(printers[1].name, "Front_Desk");
        assert_eq!(printers[1].state, PrinterState::Ready);
        assert!(printers[1].default, "the default destination is flagged");
        assert_eq!(printers[2].state, PrinterState::Paused);
    }

    #[test]
    fn parse_lpstat_empty_yields_no_printers() {
        assert!(parse_lpstat("no printers here\n").is_empty());
    }
}
