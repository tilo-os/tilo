//! The pages of Print Centre, composed from the toolkit's semantic widgets.
//!
//! Every page is a column inside one vertical `Scrollable` that spans the
//! window, so the scroll bar sits on the window edge; the column is bounded to
//! the readable measure and centred. A page's title, the way back and the
//! page's actions are the window bar's ([`super::window_bar`]): no page draws
//! them. Nothing here picks a colour, size, radius
//! or padding: text names its role, buttons their role, and the toolkit
//! spaces what it composes.

use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::tokens::{Spacing, SpacingStep, TextRole};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Actions, Badge, Banner, BarGauge, Button, ButtonRole, Card, CardGrid, CardWidth, ChipStyle,
    Column, Container, ControlSize, Divider, Dropdown, Field, Form, FormSection, GaugeThresholds,
    Image, ImageCropEditor, ListRow, NumberInput, ProgressBar, Property, PropertyList, Row,
    Scrollable, Section, SectionHeader, Spacer, Text, TextInput, Toggle, ToggleGroup, Tone,
};

use super::{
    Door, El, PrintChoice, PrintPreview, ScanState, Snapshot, UiCommand, display_name,
    failed_areas, friendly_queue_state, friendly_reason, job_title, supply_level,
};
use crate::locales::CATALOG;
use crate::resource::{
    OCR_LANGUAGES, PrintCentreEffect, PrintDefaults, SCAN_COLORS, SCAN_RESOLUTIONS, SCAN_SOURCES,
};
use ui::tilo_resource::Atom;

fn text(s: impl AsRef<str>, role: TextRole) -> El {
    Box::new(Text::new(s.as_ref()).with_role(role))
}

fn copy(s: impl AsRef<str>) -> El {
    text(s, TextRole::Body)
}

fn muted(s: impl AsRef<str>) -> El {
    Box::new(Text::new(s.as_ref()).with_role(TextRole::Caption).dimmed())
}

/// A button that sends `command` to the page controller.
fn command_button(id: &str, label: impl AsRef<str>, tx: &Door, command: UiCommand) -> Button {
    let tx = tx.clone();
    Button::new(id, label.as_ref())
        .with_enabled(tx.0.is_some())
        .control_size(ControlSize::Large)
        .on_effect(move || {
            let _ = tx.send(command.clone());
        })
}

fn primary(button: Button) -> Button {
    button.role(ButtonRole::Primary)
}

fn destructive(button: Button) -> Button {
    button.role(ButtonRole::Destructive)
}

/// Trailing action group: the last button leads, the others come before it.
fn actions(mut buttons: Vec<Button>) -> Option<Actions> {
    let last = buttons.pop()?;
    let mut group = Actions::new(last);
    for button in buttons {
        group = group.secondary(button);
    }
    Some(group)
}

/// Buttons side by side, wrapping on a narrow window.
fn button_row(buttons: Vec<Button>) -> El {
    let mut row = Row::new();
    for button in buttons {
        row = row.push(Box::new(button));
    }
    Box::new(row)
}

/// A group of items on the ground, spaced by the toolkit.
fn section(items: Vec<El>) -> El {
    Box::new(Section::new(items))
}

fn titled_section(title: impl AsRef<str>, items: Vec<El>) -> El {
    Box::new(Section::titled(title.as_ref(), items))
}

/// Space that, with the column's own gap on each side, separates two groups of
/// a page by the group rhythm.
fn group_break() -> El {
    Box::new(Spacer::token(SpacingStep::S0))
}

/// The groups of a page, one group rhythm apart.
fn groups(items: Vec<El>) -> Column {
    stack_of(column(), items)
}

/// `items` appended to `body`, a group rhythm between neighbours.
fn stack_of(mut body: Column, items: Vec<El>) -> Column {
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            body = body.push(group_break());
        }
        body = body.push(item);
    }
    body
}

/// A page column: the toolkit's gutter and gap, as tall as its content.
fn column() -> Column {
    Column::new().with_intrinsic_height()
}

/// Content with no gutter of its own, for a card body or a group.
fn bare_column() -> Column {
    Column::new().with_no_padding().with_intrinsic_height()
}

/// Content bounded to the readable measure of every page, so it starts at the
/// same edge on each one.
fn centred(content: El) -> El {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_centered_max_width(1120.0)
            .with_child(content),
    )
}

/// A scrolling page: the scroll view spans the window, the column inside it
/// holds the readable measure.
fn page(body: Column) -> El {
    Box::new(Scrollable::new_vertical().with_child(centred(Box::new(body))))
}

/// A page whose trailing actions stay below the scrolling content.
fn page_with_footer(body: Column, footer: Option<Actions>) -> El {
    let mut root = Column::new()
        .with_no_padding()
        .with_fill_remaining_height()
        .push(Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(centred(Box::new(body))),
        ));
    if let Some(footer) = footer {
        // The scroll view keeps a lane for its bar that the footer does not
        // have. An empty trailing item leaves one row gap, the same width,
        // so the footer and the content share a right edge.
        let aligned = Row::new()
            .no_wrap()
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(centred(Box::new(column().push(Box::new(footer))))),
            ))
            .push(Box::new(Spacer::token(SpacingStep::S0)));
        root = root
            .push(Box::new(Divider::horizontal()))
            .push(Box::new(aligned));
    }
    Box::new(root)
}

/// The notice's subject and detail for the areas that could not be read.
fn unavailable_copy(areas: &[&str]) -> (&'static str, &'static str) {
    match areas {
        ["printers"] => (
            "print-centre-unavailable-printers",
            "print-centre-unavailable-printers-detail",
        ),
        ["documents"] => (
            "print-centre-unavailable-documents",
            "print-centre-unavailable-documents-detail",
        ),
        ["scanners"] => (
            "print-centre-unavailable-scanners",
            "print-centre-unavailable-scanners-detail",
        ),
        _ => (
            "print-centre-unavailable-some",
            "print-centre-unavailable-some-detail",
        ),
    }
}

/// What the person sees when a part of the system could not be read.
pub(super) fn unavailable_notice(areas: &[&str], tx: &Door) -> Option<El> {
    if areas.is_empty() {
        return None;
    }
    let (subject, detail) = unavailable_copy(areas);
    // Trying again is what resolves it, so the banner holds that action.
    Some(Box::new(
        Banner::error(CATALOG.t(subject))
            .with_message(CATALOG.t(detail))
            .with_id("print-centre.service")
            .with_action(
                command_button(
                    "print-centre.service.retry",
                    CATALOG.t("print-centre-try-again"),
                    tx,
                    UiCommand::Refresh,
                )
                .with_icon(Icon::Refresh),
            ),
    ))
}

pub(super) fn loading_view() -> El {
    page(column().push(section(vec![
            copy(CATALOG.t("print-centre-loading-detail")),
            Box::new(
                ProgressBar::indeterminate()
                    .with_label(crate::locales::CATALOG.t("print-centre-loading")),
            ),
        ])))
}

/// A supply gauge stays neutral while there is plenty, then turns warning
/// and danger as the supply runs out, so the band comes from how much is
/// left rather than how full the bar is.
fn supply_gauge(level: f32) -> BarGauge {
    let gauge = BarGauge::new(level);
    if level <= 0.10 {
        gauge.with_thresholds(GaugeThresholds {
            warning: 0.0,
            danger: 0.0,
        })
    } else if level <= 0.25 {
        gauge.with_thresholds(GaugeThresholds {
            warning: 0.0,
            danger: 2.0,
        })
    } else {
        gauge.neutral()
    }
}

/// The measure of a supply: what it is called and how much is left, over a
/// track that shows the whole scale.
fn supply_meter(name: impl Into<String>, level: f32) -> El {
    let percent = (level.clamp(0.0, 1.0) * 100.0).round();
    Box::new(supply_gauge(level).with_label(name, format!("{percent}%")))
}

/// How far a document has printed, with the page it is on.
fn job_progress(job: &os_actions::printing::PrintJob) -> El {
    let total = job.impressions_total.unwrap_or(1).max(1);
    let done = job.impressions_completed.unwrap_or(0).min(total);
    let percent = job.progress_percent().unwrap_or(0);
    let label = if matches!(job.state, Some(os_actions::printing::JobState::Processing)) {
        CATALOG.t_args(
            "print-centre-page-progress",
            &[
                (
                    "page",
                    ArgValue::Text(done.saturating_add(1).min(total).to_string()),
                ),
                ("total", ArgValue::Text(total.to_string())),
            ],
        )
    } else {
        CATALOG.t_args(
            "print-centre-pages-progress",
            &[
                ("done", ArgValue::Text(done.to_string())),
                ("total", ArgValue::Text(total.to_string())),
            ],
        )
    };
    // Progress is neutral until the printer stops the document.
    let gauge = BarGauge::new(f32::from(percent) / 100.0).with_label(label, format!("{percent}%"));
    Box::new(
        if matches!(job.state, Some(os_actions::printing::JobState::Stopped)) {
            gauge.with_thresholds(GaugeThresholds {
                warning: 0.0,
                danger: 2.0,
            })
        } else {
            gauge.neutral()
        },
    )
}

fn live_job_card(job: &os_actions::printing::PrintJob, tx: &Door) -> El {
    let status = match job.state {
        Some(os_actions::printing::JobState::Held) => CATALOG.t("print-centre-job-waiting"),
        Some(os_actions::printing::JobState::Stopped) => CATALOG.t("print-centre-job-attention"),
        _ => CATALOG.t("print-centre-job-printing"),
    };
    let mut summary = bare_column()
        .push(copy(status))
        .push(muted(CATALOG.t_args(
            "print-centre-sent-to",
            &[("printer", ArgValue::Text(display_name(&job.destination)))],
        )))
        .push(job_progress(job));
    if let Some(reason) = job.state_reasons.first() {
        summary = summary.push(copy(friendly_reason(reason)));
    }
    let card = Card::new(Box::new(summary)).with_title(job_title(job));
    let card = match actions(vec![
        destructive(command_button(
            "print-centre.job.cancel",
            CATALOG.t("print-centre-cancel"),
            tx,
            UiCommand::RequestCancel(job.id.clone()),
        )),
        command_button(
            "print-centre.queue",
            CATALOG.t("print-centre-open-queue"),
            tx,
            UiCommand::Queue,
        ),
    ]) {
        Some(footer) => card.with_footer(footer),
        None => card,
    };
    Box::new(card)
}

/// The printer's state as a status pill: the only colour on the card.
fn status_badge(queue: &os_actions::printing::PrinterQueue) -> El {
    use os_actions::printing::QueueState;
    let tone = match queue.state {
        QueueState::Ready if queue.accepting_jobs => Tone::Success,
        QueueState::Printing => Tone::Info,
        QueueState::Offline => Tone::Danger,
        QueueState::Ready | QueueState::Paused => Tone::Warning,
    };
    Box::new(Badge::text(friendly_queue_state(queue)).style(ChipStyle::Tinted(tone)))
}

/// A printer on the dashboard: the whole card opens its page.
fn printer_card(queue: &os_actions::printing::PrinterQueue, tx: &Door) -> El {
    let supplies = if let Some((level, _)) = supply_level(queue) {
        supply_meter(CATALOG.t("print-centre-lowest-supply"), level)
    } else {
        muted(CATALOG.t("print-centre-supplies-unavailable"))
    };
    let detail = if queue.default {
        CATALOG.t("print-centre-preferred")
    } else {
        CATALOG.t("print-centre-available")
    };
    let card = Card::new(Box::new(
        bare_column()
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_align_items(taffy::AlignItems::Center)
                    .push(status_badge(queue))
                    .push(muted(detail)),
            ))
            .push(supplies),
    ))
    .with_title(display_name(&queue.name));
    let id = format!("print-centre.device.open.{}", queue.name);
    match tx.1.as_ref().and_then(|actions| {
        actions.effect(&crate::resource::PrintCentreEffect::OpenPrinter {
            printer: queue.name.clone(),
        })
    }) {
        Some(binding) => Box::new(card.on_action(id, binding)),
        None => Box::new(card),
    }
}

fn tool_row(
    id: &str,
    icon: Icon,
    title: impl Into<String>,
    detail: impl Into<String>,
    tx: &Door,
    command: UiCommand,
) -> El {
    let tx = tx.clone();
    Box::new(
        ListRow::new(id, title)
            .icon(icon)
            .secondary(detail)
            .disclosure()
            .on_view(move || {
                let _ = tx.send(command.clone());
            }),
    )
}

fn is_active(job: &os_actions::printing::PrintJob) -> bool {
    matches!(
        job.state,
        Some(
            os_actions::printing::JobState::Pending
                | os_actions::printing::JobState::Held
                | os_actions::printing::JobState::Processing
                | os_actions::printing::JobState::Stopped
        )
    )
}

pub(super) fn live_view(snapshot: &Snapshot, tx: &Door) -> El {
    let queues = snapshot.queues.as_deref().unwrap_or(&[]);
    let jobs = snapshot.jobs.as_deref().unwrap_or(&[]);
    let scanners = snapshot.scanners.as_deref().unwrap_or(&[]);
    let active: Vec<_> = jobs.iter().filter(|job| is_active(job)).collect();

    let mut items: Vec<El> = Vec::new();
    if let Some(issue) = unavailable_notice(&failed_areas(snapshot, true, true, true), tx) {
        items.push(issue);
    }
    if let Some(job) = active.first() {
        items.push(live_job_card(job, tx));
    }
    if snapshot.queues.is_ok() {
        items.push(if queues.is_empty() {
            section(vec![
                copy(CATALOG.t("print-centre-no-printers")),
                Box::new(
                    primary(command_button(
                        "print-centre.discover.empty",
                        CATALOG.t("print-centre-find-printer"),
                        tx,
                        UiCommand::Discover,
                    ))
                    .with_icon(Icon::Search),
                ),
            ])
        } else {
            Box::new(
                bare_column()
                    .with_rhythm(ui::tokens::Rhythm::Section)
                    .push(Box::new(Section::new(Vec::new()).with_header(
                        SectionHeader::heading(CATALOG.t("print-centre-section-printers")),
                    )))
                    .push(Box::new(
                        CardGrid::new(queues.iter().map(|queue| printer_card(queue, tx)).collect())
                            .with_card_width(CardWidth::Wide),
                    )),
            )
        });
    }

    items.push(live_tools(scanners, jobs, snapshot.jobs.is_ok(), tx));
    page(groups(items))
}

fn live_tools(
    scanners: &[tilo_capture::scanner::Scanner],
    jobs: &[os_actions::printing::PrintJob],
    jobs_available: bool,
    tx: &Door,
) -> El {
    let mut tools: Vec<El> = Vec::new();
    if let Some(scanner) = scanners.first() {
        tools.push(tool_row(
            "print-centre.scan.open",
            Icon::Camera,
            CATALOG.t("print-centre-scan-document"),
            CATALOG.t_args(
                "print-centre-scanner-ready",
                &[("scanner", ArgValue::Text(scanner.description.clone()))],
            ),
            tx,
            UiCommand::ScanSetup,
        ));
    }
    if jobs_available {
        let history = if jobs
            .iter()
            .any(|job| matches!(job.state, Some(os_actions::printing::JobState::Completed)))
        {
            CATALOG.t("print-centre-history-review")
        } else {
            CATALOG.t("print-centre-history-empty")
        };
        tools.push(tool_row(
            "print-centre.history.open",
            Icon::History,
            CATALOG.t("print-centre-print-history"),
            history,
            tx,
            UiCommand::History,
        ));
    }
    tools.push(tool_row(
        "print-centre.diagnostics.open",
        Icon::Info,
        CATALOG.t("print-centre-device-help"),
        CATALOG.t("print-centre-device-help-detail"),
        tx,
        UiCommand::Diagnostics,
    ));
    Box::new(Section::titled(
        CATALOG.t("print-centre-section-tools"),
        vec![Box::new(tools.into_iter().fold(
            bare_column().with_rhythm(ui::tokens::Rhythm::Section),
            Column::push,
        ))],
    ))
}

fn history_status(job: &os_actions::printing::PrintJob) -> String {
    match job.state {
        Some(os_actions::printing::JobState::Completed) => CATALOG.t("print-centre-status-printed"),
        Some(os_actions::printing::JobState::Canceled) => CATALOG.t("print-centre-status-canceled"),
        _ => CATALOG.t("print-centre-status-not-printed"),
    }
}

/// A finished document: what became of it and where, with the way to print
/// it again always in view.
fn history_card(job: &os_actions::printing::PrintJob, tx: &Door) -> El {
    let again = command_button(
        &format!("print-centre.history.repeat.{}", job.id),
        CATALOG.t("print-centre-print-again"),
        tx,
        UiCommand::RetryJob(job.id.clone()),
    )
    .with_icon(Icon::Refresh);
    let body = bare_column()
        .push(copy(CATALOG.t_args(
            "print-centre-job-on",
            &[
                ("status", ArgValue::Text(history_status(job))),
                ("printer", ArgValue::Text(display_name(&job.destination))),
            ],
        )))
        .push(muted(&job.submitted));
    let card = Card::new(Box::new(body)).with_title(job_title(job));
    Box::new(match actions(vec![again]) {
        Some(footer) => card.with_footer(footer),
        None => card,
    })
}

pub(super) fn history_view(snapshot: &Snapshot, tx: &Door) -> El {
    let jobs = snapshot.jobs.as_deref().unwrap_or(&[]);
    let terminal: Vec<_> = jobs
        .iter()
        .filter(|job| {
            matches!(
                job.state,
                Some(
                    os_actions::printing::JobState::Completed
                        | os_actions::printing::JobState::Canceled
                        | os_actions::printing::JobState::Aborted
                )
            )
        })
        .collect();
    let mut items: Vec<El> = Vec::new();
    if let Some(issue) = unavailable_notice(&failed_areas(snapshot, false, true, false), tx) {
        items.push(issue);
    }
    // With the jobs unread, "No completed documents yet" would claim what
    // nobody knows: the notice above says they couldn't be checked.
    if terminal.is_empty() && snapshot.jobs.is_ok() {
        items.push(copy(CATALOG.t("print-centre-nothing-completed")));
    } else {
        items.push(titled_section(
            CATALOG.t("print-centre-documents"),
            vec![Box::new(
                CardGrid::new(
                    terminal
                        .into_iter()
                        .take(50)
                        .map(|job| history_card(job, tx))
                        .collect(),
                )
                .with_card_width(CardWidth::Wide),
            )],
        ));
    }
    page(groups(items))
}

fn device_summary(supplies_known: bool, queue: &os_actions::printing::PrinterQueue) -> String {
    CATALOG.t(if supplies_known {
        "print-centre-device-answered"
    } else if queue.telemetry.available {
        "print-centre-device-no-supplies"
    } else {
        "print-centre-device-unreachable"
    })
}

pub(super) fn diagnostics_view(snapshot: &Snapshot, tx: &Door) -> El {
    let queues = snapshot.queues.as_deref().unwrap_or(&[]);
    let scanners = snapshot.scanners.as_deref().unwrap_or(&[]);
    let responding = queues
        .iter()
        .filter(|queue| queue.telemetry.available)
        .count();
    let supply_reports = queues
        .iter()
        .filter(|queue| {
            queue
                .telemetry
                .supplies
                .as_ref()
                .is_some_and(|items| !items.is_empty())
        })
        .count();
    let check = Section::new(vec![Box::new(PropertyList::new(vec![
        Property::new(
            CATALOG.t("print-centre-printers-responding"),
            CATALOG.t_args(
                "print-centre-printers-responding-count",
                &[
                    ("responding", ArgValue::Text(responding.to_string())),
                    ("total", ArgValue::Text(queues.len().to_string())),
                ],
            ),
        ),
        Property::new(
            CATALOG.t("print-centre-reporting-levels"),
            CATALOG.t_args(
                "print-centre-reporting-levels-count",
                &[("count", ArgValue::Text(supply_reports.to_string()))],
            ),
        ),
        Property::new(
            CATALOG.t("print-centre-scanners"),
            scanners.len().to_string(),
        ),
    ]))])
    .with_header(SectionHeader::heading(
        CATALOG.t("print-centre-section-connection-check"),
    ));
    let mut items: Vec<El> = Vec::new();
    if let Some(issue) = unavailable_notice(&failed_areas(snapshot, true, false, true), tx) {
        items.push(issue);
    }
    items.push(Box::new(check));
    for queue in queues {
        items.push(printer_check(queue, tx));
    }
    items.push(scanner_check(scanners.is_empty(), tx));
    page(groups(items))
}

/// What one printer answered to the status check, in plain words.
fn printer_check(queue: &os_actions::printing::PrinterQueue, tx: &Door) -> El {
    let supplies_known = queue.telemetry.available
        && queue
            .telemetry
            .supplies
            .as_ref()
            .is_some_and(|items| !items.is_empty());
    let mut items: Vec<El> = vec![
        copy(friendly_queue_state(queue)),
        muted(device_summary(supplies_known, queue)),
    ];
    for reason in queue.telemetry.state_reasons.iter().take(3) {
        items.push(copy(friendly_reason(reason)));
    }
    if let Some(supplies) = &queue.telemetry.supplies {
        for supply in supplies.iter().take(6) {
            if let Some(percent) = supply.level_percent {
                items.push(supply_meter(&supply.name, f32::from(percent) / 100.0));
            }
        }
    }
    items.push(Box::new(command_button(
        "print-centre.diagnostics.device",
        CATALOG.t("print-centre-open-controls"),
        tx,
        UiCommand::Device(queue.name.clone()),
    )));
    titled_section(display_name(&queue.name), items)
}

fn scanner_check(none_found: bool, tx: &Door) -> El {
    let items: Vec<El> = if none_found {
        vec![copy(CATALOG.t("print-centre-no-scanner"))]
    } else {
        vec![
            copy(CATALOG.t("print-centre-scanner-ready")),
            Box::new(command_button(
                "print-centre.diagnostics.scan",
                CATALOG.t("print-centre-start-scanning"),
                tx,
                UiCommand::ScanSetup,
            )),
        ]
    };
    titled_section(CATALOG.t("print-centre-scanner"), items)
}

fn queue_state(job: &os_actions::printing::PrintJob) -> String {
    match job.state {
        Some(os_actions::printing::JobState::Completed) => CATALOG.t("print-centre-status-printed"),
        Some(os_actions::printing::JobState::Canceled) => CATALOG.t("print-centre-status-canceled"),
        Some(os_actions::printing::JobState::Aborted) => CATALOG.t("print-centre-status-failed"),
        Some(os_actions::printing::JobState::Held) => CATALOG.t("print-centre-status-waiting"),
        Some(os_actions::printing::JobState::Stopped) => CATALOG.t("print-centre-job-attention"),
        _ => CATALOG.t("print-centre-status-in-progress"),
    }
}

fn queue_summary(job: &os_actions::printing::PrintJob) -> Column {
    let summary = bare_column()
        .push(copy(queue_state(job)))
        .push(muted(CATALOG.t_args(
            "print-centre-sent-to",
            &[("printer", ArgValue::Text(display_name(&job.destination)))],
        )));
    if is_active(job) {
        summary.push(job_progress(job))
    } else {
        summary
    }
}

fn queue_card(job: &os_actions::printing::PrintJob, pending_cancel: Option<&str>, tx: &Door) -> El {
    let mut buttons: Vec<Button> = Vec::new();
    if matches!(
        job.state,
        Some(
            os_actions::printing::JobState::Stopped
                | os_actions::printing::JobState::Canceled
                | os_actions::printing::JobState::Aborted
        )
    ) {
        buttons.push(command_button(
            "print-centre.job.retry",
            CATALOG.t("print-centre-try-again"),
            tx,
            UiCommand::RetryJob(job.id.clone()),
        ));
    }
    if !matches!(
        job.state,
        Some(os_actions::printing::JobState::Completed | os_actions::printing::JobState::Canceled)
    ) {
        let confirming = pending_cancel == Some(job.id.as_str());
        buttons.push(destructive(command_button(
            "print-centre.job.cancel",
            if confirming {
                CATALOG.t("print-centre-confirm-cancel")
            } else {
                CATALOG.t("print-centre-cancel")
            },
            tx,
            if confirming {
                UiCommand::ConfirmCancel(job.id.clone())
            } else {
                UiCommand::RequestCancel(job.id.clone())
            },
        )));
    }
    let card = Card::new(Box::new(queue_summary(job))).with_title(job_title(job));
    Box::new(match actions(buttons) {
        Some(footer) => card.with_footer(footer),
        None => card,
    })
}

pub(super) fn queue_view(snapshot: &Snapshot, tx: &Door, pending_cancel: Option<&str>) -> El {
    let jobs = snapshot.jobs.as_deref().unwrap_or(&[]);
    let mut items: Vec<El> = Vec::new();
    if let Some(issue) = unavailable_notice(&failed_areas(snapshot, false, true, false), tx) {
        items.push(issue);
    }
    if pending_cancel.is_some() {
        items.push(Box::new(
            Banner::warning("Cancel this document?")
                .with_message(CATALOG.t("print-centre-cancel-pending-message")),
        ));
    }
    if jobs.is_empty() && snapshot.jobs.is_ok() {
        items.push(copy(CATALOG.t("print-centre-nothing-waiting")));
    } else {
        items.push(titled_section(
            CATALOG.t("print-centre-documents"),
            vec![Box::new(
                CardGrid::new(
                    jobs.iter()
                        .take(24)
                        .map(|job| queue_card(job, pending_cancel, tx))
                        .collect(),
                )
                .with_card_width(CardWidth::Wide),
            )],
        ));
    }
    page(groups(items))
}

pub(super) fn device_view(queue: &os_actions::printing::PrinterQueue, tx: &Door) -> El {
    let enabled =
        queue.accepting_jobs && !matches!(queue.state, os_actions::printing::QueueState::Paused);
    let mut buttons = vec![command_button(
        "print-centre.device.enabled",
        if enabled {
            CATALOG.t("print-centre-pause-printer")
        } else {
            CATALOG.t("print-centre-resume-printer")
        },
        tx,
        UiCommand::SetEnabled(queue.name.clone(), !enabled),
    )];
    if !queue.default {
        buttons.push(command_button(
            "print-centre.device.default",
            CATALOG.t("print-centre-make-preferred"),
            tx,
            UiCommand::SetDefault(queue.name.clone()),
        ));
    }
    buttons.push(command_button(
        "print-centre.device.queue",
        CATALOG.t("print-centre-view-queue"),
        tx,
        UiCommand::Queue,
    ));
    buttons.push(command_button(
        "print-centre.device.diagnostics",
        CATALOG.t("print-centre-check-printer"),
        tx,
        UiCommand::Diagnostics,
    ));
    let supplies: Vec<El> = match &queue.telemetry.supplies {
        Some(items) if !items.is_empty() => items
            .iter()
            .map(|supply| {
                let percent = supply.level_percent.unwrap_or(0);
                supply_meter(&supply.name, f32::from(percent) / 100.0)
            })
            .collect(),
        _ => vec![copy(CATALOG.t("print-centre-no-supply-report"))],
    };
    let status = Property::new(CATALOG.t("print-centre-state"), friendly_queue_state(queue));
    let availability = Property::new(
        CATALOG.t("print-centre-availability"),
        if queue.default {
            CATALOG.t("print-centre-preferred")
        } else {
            CATALOG.t("print-centre-available")
        },
    );
    page(groups(vec![
        titled_section(
            CATALOG.t("print-centre-details"),
            vec![Box::new(PropertyList::new(vec![status, availability]))],
        ),
        titled_section(CATALOG.t("print-centre-ink-toner"), supplies),
        titled_section(
            CATALOG.t("print-centre-printer-controls"),
            vec![button_row(buttons)],
        ),
    ]))
}

pub(super) fn discovery_view(
    result: Option<&Result<Vec<os_actions::printing::DiscoveredPrinter>, String>>,
    tx: &Door,
) -> El {
    let body: El = match result {
        None => section(vec![
            copy(CATALOG.t("print-centre-searching")),
            Box::new(
                ProgressBar::indeterminate()
                    .with_label(crate::locales::CATALOG.t("print-centre-finding-printers")),
            ),
        ]),
        Some(Ok(printers)) if printers.is_empty() => section(vec![
            copy(CATALOG.t("print-centre-no-printers-found")),
            Box::new(command_button(
                "print-centre.discover.retry",
                CATALOG.t("print-centre-search-again"),
                tx,
                UiCommand::Discover,
            )),
        ]),
        Some(Ok(printers)) => {
            let cards: Vec<El> = printers
                .iter()
                .map(|printer| {
                    let add_id = format!("print-centre.discover.add.{}", printer.name);
                    Box::new(
                        Card::new(muted(
                            printer
                                .location
                                .as_deref()
                                .unwrap_or(&CATALOG.t("print-centre-nearby")),
                        ))
                        .with_title_action(
                            &printer.name,
                            Box::new(
                                command_button(
                                    &add_id,
                                    CATALOG.t("print-centre-add"),
                                    tx,
                                    UiCommand::AddPrinter(printer.clone()),
                                )
                                .with_icon(Icon::Plus),
                            ),
                        ),
                    ) as El
                })
                .collect();
            titled_section(
                CATALOG.t("print-centre-printers-network"),
                vec![Box::new(
                    CardGrid::new(cards).with_card_width(CardWidth::Wide),
                )],
            )
        }
        Some(Err(_)) => section(vec![
            copy(CATALOG.t("print-centre-search-failed")),
            Box::new(command_button(
                "print-centre.discover.retry",
                CATALOG.t("print-centre-try-again"),
                tx,
                UiCommand::Discover,
            )),
        ]),
    };
    page(groups(vec![body]))
}

fn printer_picker(snapshot: &Snapshot, selected: Option<&str>, tx: &Door) -> Dropdown {
    let options = snapshot
        .queues
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|queue| {
            let label = format!(
                "{} ({})",
                display_name(&queue.name),
                friendly_queue_state(queue)
            );
            (queue.name.clone(), label)
        })
        .collect::<Vec<_>>();
    let mut picker = Dropdown::new(Vec::new(), "")
        .with_id("print-centre.print.printer")
        .with_placeholder(&crate::locales::CATALOG.t("print-centre-choose-printer"));
    picker.set_value_options(options);
    if let Some(selected) = selected {
        picker.set_selected(selected);
    }
    match tx.1.as_ref() {
        Some(actions) => actions.bind_effect(picker, PrintCentreEffect::CHOOSE_PRINTER),
        None => picker,
    }
}

/// What the print setup form is filled from: the range being typed, the
/// options the chosen printer reports, the values chosen so far, and the page
/// of the document under review.
pub(super) struct PrintForm<'a> {
    pub(super) copies: &'a ui::LocalState<f64>,
    pub(super) two_sided: &'a ui::LocalState<bool>,
    pub(super) range: &'a ui::LocalState<String>,
    pub(super) options: &'a os_actions::printing::PrinterOptions,
    pub(super) choice: &'a PrintChoice,
    /// How many pages the document has: the range field is drawn only when
    /// there is more than one to choose from.
    pub(super) pages: usize,
}

/// The document being printed: its preview, which the person can check before
/// anything is sent, page by page for a document of more than one. A wrong
/// page or a wrong paper size is caught here rather than on paper.
fn document_card(filename: &str, preview: &PrintPreview, tx: &Door) -> El {
    // How many pages the document has decides whether there is a pager at
    // all: one page has nothing to turn.
    let (page, pages) = match preview {
        PrintPreview::Ready { page, pages, .. } => (*page, *pages),
        _ => (0, 0),
    };
    let preview_widget: El = match preview {
        PrintPreview::Ready {
            rgba,
            width,
            height,
            ..
        } => Box::new(
            Container::new()
                .with_height(300.0)
                .with_no_padding()
                .with_child(Box::new(
                    Image::from_rgba(rgba.clone(), *width, *height)
                        .filling()
                        .with_alt(format!("Preview of {filename}")),
                )),
        ),
        PrintPreview::Loading => Box::new(
            Container::new()
                .with_height(138.0)
                .with_child(Box::new(ProgressBar::indeterminate().with_label(
                    crate::locales::CATALOG.t("print-centre-preparing-preview"),
                ))),
        ),
        PrintPreview::Unavailable => Box::new(
            bare_column()
                .push(copy(CATALOG.t("print-centre-preview-unavailable")))
                .push(muted(CATALOG.t("print-centre-original-unchanged"))),
        ),
    };
    let body = if pages > 1 {
        let shown = page.min(pages.saturating_sub(1));
        // The count keeps its own line above the pager: a card this narrow
        // would otherwise break "Page 3 of 40" into three.
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(preview_widget)
                .push(Box::new(
                    Text::new(&CATALOG.t_args(
                        "print-centre-page-of",
                        &[
                            ("page", ArgValue::Text(shown.saturating_add(1).to_string())),
                            ("pages", ArgValue::Text(pages.to_string())),
                        ],
                    ))
                    .with_role(TextRole::Label),
                ))
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_align_items(taffy::AlignItems::Center)
                        .with_gap(Spacing::BASE.s2)
                        .push(Box::new(command_button(
                            "print-centre.print.previous-page",
                            CATALOG.t("print-centre-previous"),
                            tx,
                            UiCommand::PreviousPrintPage,
                        )))
                        .push(Box::new(command_button(
                            "print-centre.print.next-page",
                            CATALOG.t("print-centre-next"),
                            tx,
                            UiCommand::NextPrintPage,
                        ))),
                )),
        ) as El
    } else {
        preview_widget
    };
    Box::new(Card::new(body).with_title(filename))
}

fn print_settings(picker: Dropdown, form: &PrintForm<'_>, tx: &Door) -> Form {
    let mut options = FormSection::new("Options").grouped().field(
        Field::new(
            "Copies",
            NumberInput::from_state(form.copies)
                .with_range(1.0, 99.0)
                .with_step(1.0)
                .with_id("print-centre.print.copies")
                .compact(),
        )
        .with_trailing_control(),
    );
    // A document with one page has no pages to choose, so it is not offered a
    // field that would change nothing. The count comes with the preview, so
    // the form is told it.
    if form.pages > 1 {
        options = options.field(
            Field::new(
                "Pages",
                TextInput::from_state(form.range)
                    .with_id("print-centre.print.pages")
                    .with_placeholder(crate::locales::CATALOG.t("print-centre-all-pages"))
                    .with_accessible_name(crate::locales::CATALOG.t("print-centre-a11y-pages")),
            )
            .with_help("Print some of the pages, as in 3-5 or 1, 4, 9. Leave it empty for all."),
        );
    }
    // The driver's own list decides which of these fields is drawn: a paper
    // size, a colour mode, a two-sided mode or an orientation it never
    // reported is a field it would silently ignore.
    for (option, key, choices, chosen) in [
        (
            "Paper",
            "page-size",
            &form.options.page_sizes,
            form.choice.page_size.as_deref(),
        ),
        (
            "Colour",
            "colour",
            &form.options.colour_modes,
            form.choice.colour.as_deref(),
        ),
        (
            "Two sides",
            "two-sided",
            &form.options.duplex_modes,
            form.choice.duplex.as_deref(),
        ),
        (
            "Orientation",
            "orientation",
            &form.options.orientations,
            form.choice.orientation.as_deref(),
        ),
    ] {
        if choices.len() < 2 {
            continue;
        }
        options = options.field(Field::new(
            option,
            driver_choice(key, choices, chosen, tx).with_accessible_name(option.to_owned()),
        ));
    }
    // A printer that reports no duplex mode keeps the one setting every CUPS
    // printer reads, so a person can still ask for both sides.
    if form.options.duplex_modes.len() < 2 {
        let mut both_sides = Toggle::from_state(form.two_sided);
        if let Some(binding) =
            tx.1.as_ref()
                .and_then(|actions| actions.field_binding(PrintDefaults::paths().two_sided))
        {
            both_sides = both_sides.on_action(binding);
        }
        options =
            options.field(Field::new("Print on both sides", both_sides).with_trailing_control());
    }
    Form::new()
        .section(
            FormSection::new("Printer").field(
                Field::new("Send to", picker).with_help("Choose where to send this document."),
            ),
        )
        .section(options)
}

pub(super) fn print_setup_view(
    snapshot: &Snapshot,
    path: &std::path::Path,
    preview: &PrintPreview,
    selected: Option<&str>,
    copies: &ui::LocalState<f64>,
    two_sided: &ui::LocalState<bool>,
    form: &PrintForm<'_>,
    tx: &Door,
) -> El {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Selected document");
    let form = PrintForm {
        copies,
        two_sided,
        range: form.range,
        options: form.options,
        choice: form.choice,
        pages: form.pages,
    };
    let settings = print_settings(printer_picker(snapshot, selected, tx), &form, tx);
    let layout = Row::new()
        .align_start()
        .push(Box::new(
            Container::new()
                .with_width(260.0)
                .with_no_padding()
                .with_child(document_card(filename, preview, tx)),
        ))
        .push(Box::new(
            Container::new()
                .with_min_width(320.0)
                .with_flex_grow()
                .with_no_padding()
                .with_child(Box::new(settings)),
        ));

    let mut items: Vec<El> = Vec::new();
    if let Some(issue) = unavailable_notice(&failed_areas(snapshot, true, false, false), tx) {
        items.push(issue);
    }
    items.push(Box::new(layout));
    let body = groups(items);

    let mut buttons = vec![command_button(
        "print-centre.print.cancel",
        CATALOG.t("print-centre-cancel"),
        tx,
        UiCommand::CancelPrintSetup,
    )];
    if selected.is_some() {
        buttons.push(primary(command_button(
            "print-centre.print.submit",
            CATALOG.t("print-centre-print"),
            tx,
            UiCommand::PrintDocument,
        )));
    }
    page_with_footer(body, actions(buttons))
}

/// One of the driver's own options, as the choices it reported: the values
/// it listed, with the one the person chose standing on the selection lift.
fn driver_choice(
    key: &str,
    choices: &[os_actions::printing::OptionChoice],
    chosen: Option<&str>,
    tx: &Door,
) -> ToggleGroup {
    let default = os_actions::printing::PrinterOptions::default_key(choices).unwrap_or_default();
    let selected = choices
        .iter()
        .position(|choice| choice.key == chosen.unwrap_or(default))
        .unwrap_or(0);
    let labels: Vec<&str> = choices.iter().map(|choice| choice.label.as_str()).collect();
    let values: Vec<String> = choices.iter().map(|choice| choice.key.clone()).collect();
    let send = tx.clone();
    let key = key.to_owned();
    let id = format!("print-centre.print.{key}");
    ToggleGroup::new(segments(&labels), selected)
        .with_id(&id)
        .on_view(&format!("{id}.select"), move |index| {
            let Some(value) = values.get(index) else {
                return;
            };
            let _ = send.send(UiCommand::SetPrintOption {
                option: key.clone(),
                value: value.clone(),
            });
        })
}

/// A choice with its name above it.
fn setting(label: impl AsRef<str>, control: ToggleGroup) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(Spacing::BASE.s1)
            .push(text(label, TextRole::Label))
            .push(Box::new(control)),
    )
}

fn segments(labels: &[&str]) -> Vec<Segment> {
    labels.iter().map(|label| Segment::new(*label)).collect()
}

/// What the scan looked like to the text reader, and what can be done to the
/// page.
fn page_details(
    page_data: &super::ScannedPage,
    selected_page: usize,
    page_count: usize,
    tx: &Door,
) -> Column {
    let orientation = if page_data.height >= page_data.width {
        CATALOG.t("print-centre-portrait")
    } else {
        CATALOG.t("print-centre-landscape")
    };
    let mut details = bare_column()
        .push(text(
            CATALOG.t_args(
                "print-centre-page-of-document",
                &[
                    (
                        "page",
                        ArgValue::Text(selected_page.saturating_add(1).to_string()),
                    ),
                    ("pages", ArgValue::Text(page_count.to_string())),
                ],
            ),
            TextRole::Heading,
        ))
        .push(Box::new(PropertyList::new(vec![
            Property::new(CATALOG.t("print-centre-orientation"), orientation),
            Property::new(
                CATALOG.t("print-centre-text-language"),
                &page_data.ocr_language,
            ),
        ])))
        .push(muted(&page_data.ocr_note));
    if let Some(recognized) = &page_data.recognized_text {
        let snippet: String = recognized.chars().take(320).collect();
        if !snippet.trim().is_empty() {
            details = details.push(titled_section(
                CATALOG.t("print-centre-recognized-text"),
                vec![copy(&snippet)],
            ));
        }
    }
    let mut tools = vec![
        command_button(
            "print-centre.scan.rotate",
            CATALOG.t("print-centre-rotate-right"),
            tx,
            UiCommand::RotateScanPage,
        )
        .with_icon(Icon::RotateCw),
        command_button(
            "print-centre.scan.apply-crop",
            CATALOG.t("print-centre-apply-crop"),
            tx,
            UiCommand::ApplyScanCrop,
        ),
    ];
    let mut tool_items: Vec<El> = Vec::new();
    if page_data.ocr_in_progress {
        tool_items.push(muted(CATALOG.t("print-centre-updating-text")));
    } else if page_data.ocr_needs_refresh {
        tools.push(command_button(
            "print-centre.scan.recognize",
            CATALOG.t("print-centre-update-text"),
            tx,
            UiCommand::RecognizeCurrentPage,
        ));
    }
    if page_data
        .recognized_text
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
    {
        tools.push(command_button(
            "print-centre.scan.copy-text",
            CATALOG.t("print-centre-copy-text"),
            tx,
            UiCommand::CopyRecognizedText,
        ));
    }
    tool_items.insert(0, button_row(tools));
    details.push(titled_section(
        CATALOG.t("print-centre-page-tools"),
        tool_items,
    ))
}

/// Moving between, reordering and removing the pages of the document.
fn page_navigation(selected_page: usize, page_count: usize, tx: &Door) -> Vec<Button> {
    let has_previous = selected_page > 0;
    let has_next = selected_page.saturating_add(1) < page_count;
    let mut buttons: Vec<Button> = Vec::new();
    if has_previous {
        buttons.push(command_button(
            "print-centre.scan.previous",
            CATALOG.t("print-centre-previous-page"),
            tx,
            UiCommand::PreviousScanPage,
        ));
    }
    if has_next {
        buttons.push(command_button(
            "print-centre.scan.next",
            CATALOG.t("print-centre-next-page"),
            tx,
            UiCommand::NextScanPage,
        ));
    }
    if has_previous {
        buttons.push(command_button(
            "print-centre.scan.move-earlier",
            CATALOG.t("print-centre-move-earlier"),
            tx,
            UiCommand::MoveScanPageEarlier,
        ));
    }
    if has_next {
        buttons.push(command_button(
            "print-centre.scan.move-later",
            CATALOG.t("print-centre-move-later"),
            tx,
            UiCommand::MoveScanPageLater,
        ));
    }
    buttons.push(destructive(command_button(
        "print-centre.scan.delete-page",
        CATALOG.t("print-centre-remove-page"),
        tx,
        UiCommand::DeleteScanPage,
    )));
    buttons
}

fn scan_review_view(page_data: &super::ScannedPage, scan: &ScanState, tx: &Door) -> El {
    let selected_page = scan.selected;
    let page_count = scan.pages.len();
    let editor = ImageCropEditor::new(
        page_data.rgba.clone(),
        page_data.width,
        page_data.height,
        scan.crop.clone(),
    );
    let details = page_details(page_data, selected_page, page_count, tx);
    // The editor shows the whole page inside a box of its own proportions, so
    // the page starts on the column's edge instead of floating in the middle.
    let longest = f64::from(page_data.width.max(page_data.height).max(1));
    let fit = 360.0 / longest;
    let side = |pixels: u32| ui::num::f32_of_f64((f64::from(pixels) * fit).round());
    let (editor_width, editor_height) = (side(page_data.width), side(page_data.height));
    let review = Row::new()
        .align_start()
        .push(Box::new(
            Container::new()
                .with_min_width(editor_width + 24.0)
                .with_max_width(editor_width + 24.0)
                .with_no_padding()
                .with_child(titled_section(
                    CATALOG.t("print-centre-crop"),
                    vec![Box::new(
                        Container::new()
                            .with_size(editor_width, editor_height)
                            .with_no_padding()
                            .with_child(Box::new(editor)),
                    )],
                )),
        ))
        .push(Box::new(
            Container::new()
                .with_min_width(280.0)
                .with_flex_grow()
                .with_no_padding()
                .with_child(Box::new(details)),
        ));

    let mut items: Vec<El> = vec![Box::new(review)];
    items.push(titled_section(
        CATALOG.t("print-centre-pages-in-document"),
        vec![
            muted(CATALOG.t_args(
                "print-centre-pages-in-document-count",
                &[("count", ArgValue::Text(page_count.to_string()))],
            )),
            button_row(page_navigation(selected_page, page_count, tx)),
        ],
    ));
    let body = groups(items);
    let footer = actions(vec![
        command_button(
            "print-centre.scan.new-document",
            CATALOG.t("print-centre-new-document"),
            tx,
            UiCommand::NewScannedDocument,
        ),
        command_button(
            "print-centre.scan.save-page",
            CATALOG.t("print-centre-export-page"),
            tx,
            UiCommand::SaveScannedPage,
        ),
        command_button(
            "print-centre.scan.again",
            CATALOG.t("print-centre-scan-another"),
            tx,
            UiCommand::ScanAnother,
        ),
        primary(command_button(
            "print-centre.scan.save-document",
            CATALOG.t("print-centre-save-document"),
            tx,
            UiCommand::SaveScannedDocument,
        )),
    ]);
    page_with_footer(body, footer)
}

/// The scanners the person can choose from.
fn scanner_choices(snapshot: &Snapshot, selected: Option<&str>, tx: &Door) -> Vec<El> {
    let mut devices: Vec<El> = Vec::new();
    if let Ok(scanners) = &snapshot.scanners {
        for scanner in scanners {
            let device = scanner.device.clone();
            let tx = tx.clone();
            devices.push(Box::new(
                ListRow::new(
                    format!("print-centre.scan.device.{}", scanner.device),
                    &scanner.description,
                )
                .icon(Icon::Camera)
                .selected(selected == Some(scanner.device.as_str()))
                .on_view(move || {
                    let _ = tx.send(UiCommand::SelectScanner(device.clone()));
                }),
            ));
        }
        if scanners.is_empty() {
            devices.push(copy(CATALOG.t("print-centre-no-scanner-available")));
        }
    } else if let Some(issue) = unavailable_notice(&["scanners"], tx) {
        devices.push(issue);
    }
    devices
}

/// A scan setting's segments, bound to the default it sets: the choice is
/// the defaults' field edit, as the palette and an agent make it.
fn scan_choice<V: 'static>(
    (id, labels): (&str, &[&str]),
    state: &ui::LocalState<usize>,
    tx: &Door,
    path: ui::tilo_resource::Path<PrintDefaults, V>,
    edit: impl Fn(
        &ui::tilo_resource::Path<PrintDefaults, V>,
        usize,
    ) -> Option<ui::tilo_resource::Edit<PrintDefaults>>
    + Send
    + Sync
    + 'static,
) -> ToggleGroup {
    let group = ToggleGroup::new(segments(labels), state.get()).with_id(id);
    match tx.1.as_ref().and_then(|actions| {
        actions.edit_binding_with(path, move |path, index: usize| {
            edit(path, index).ok_or_else(|| "not one of the choices".to_owned())
        })
    }) {
        Some(binding) => group.on_action(binding),
        None => group,
    }
}

fn scan_settings(scan: &ScanState, tx: &Door) -> Vec<El> {
    let paths = PrintDefaults::paths();
    let atom = |values: &'static [&'static str]| {
        move |path: &ui::tilo_resource::Path<PrintDefaults, Atom>, index: usize| {
            values
                .get(index)
                .map(|value| path.set(Atom((*value).to_owned())))
        }
    };
    vec![
        setting(
            CATALOG.t("print-centre-quality"),
            scan_choice(
                (
                    "print-centre.scan.resolution",
                    &["150 dpi", "300 dpi", "600 dpi"],
                ),
                &scan.resolution,
                tx,
                paths.scan_resolution,
                |path, index| SCAN_RESOLUTIONS.get(index).map(|dpi| path.set(*dpi)),
            ),
        ),
        setting(
            CATALOG.t("print-centre-appearance"),
            scan_choice(
                ("print-centre.scan.color", &["Color", "Grayscale", "Text"]),
                &scan.color,
                tx,
                paths.scan_color,
                atom(&SCAN_COLORS),
            ),
        ),
        setting(
            CATALOG.t("print-centre-paper-source"),
            scan_choice(
                (
                    "print-centre.scan.source",
                    &["Flatbed", "Feeder", "Two-sided"],
                ),
                &scan.source,
                tx,
                paths.scan_source,
                atom(&SCAN_SOURCES),
            ),
        ),
        setting(
            CATALOG.t("print-centre-text-language"),
            scan_choice(
                (
                    "print-centre.scan.language",
                    &["English", "German", "French", "Spanish"],
                ),
                &scan.language,
                tx,
                paths.ocr_language,
                |path, index| {
                    OCR_LANGUAGES
                        .get(index)
                        .map(|(value, _)| path.set(Atom((*value).to_owned())))
                },
            ),
        ),
    ]
}

pub(super) fn scan_setup_view(
    snapshot: &Snapshot,
    selected: Option<&str>,
    scan: &ScanState,
    tx: &Door,
) -> El {
    if let Some(page_data) = scan.result.as_ref() {
        return scan_review_view(page_data, scan, tx);
    }
    let page_count = scan.pages.len();
    let mut items: Vec<El> = Vec::new();
    if page_count > 0 {
        items.push(muted(CATALOG.t_args(
            "print-centre-pages-captured",
            &[("count", ArgValue::Text(page_count.to_string()))],
        )));
    }
    items.push(titled_section(
        CATALOG.t("print-centre-scanner"),
        scanner_choices(snapshot, selected, tx),
    ));
    items.push(titled_section(
        CATALOG.t("print-centre-page-settings"),
        scan_settings(scan, tx),
    ));
    let body = groups(items);

    let mut buttons = vec![
        command_button(
            "print-centre.scan.cancel",
            CATALOG.t("print-centre-cancel"),
            tx,
            UiCommand::Dashboard,
        ),
        command_button(
            "print-centre.scan.new-document",
            CATALOG.t("print-centre-new-document"),
            tx,
            UiCommand::NewScannedDocument,
        )
        .with_enabled(!scan.in_progress && tx.0.is_some()),
    ];
    if scan.in_progress {
        buttons.push(
            command_button(
                "print-centre.scan.busy",
                CATALOG.t("print-centre-scanning-page"),
                tx,
                UiCommand::Dashboard,
            )
            .with_enabled(false),
        );
    } else if selected.is_some() {
        buttons.push(primary(command_button(
            "print-centre.scan.start",
            CATALOG.t("print-centre-scan-page"),
            tx,
            UiCommand::ScanPage,
        )));
    }
    page_with_footer(body, actions(buttons))
}
