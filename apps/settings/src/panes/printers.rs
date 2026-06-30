//! Printers pane — configured printers plus their recent queue / job
//! status.
//!
//! A real deployment reads printers and jobs from CUPS (`lpstat`); that
//! shell-out isn't wired through the shared `os-actions` dispatcher yet, so
//! this pane renders a realistic sample of printers and an active queue.
//! The data shape ([`Printer`], [`PrintJob`]) matches what a CUPS reader
//! would produce, so swapping the sample for a live `lpstat` parse is a
//! drop-in: only [`printers_for_display`] changes.

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

    Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(printer_card)
            .push(job_card),
    )
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
}
