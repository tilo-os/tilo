//! The pages of Print Centre, composed from the toolkit's semantic widgets.
//!
//! Every page is a column inside one vertical `Scrollable` that spans the
//! window, so the scroll bar sits on the window edge; the column is bounded to
//! the readable measure and centred. A page's title, the way back and the
//! page's actions are the window bar's ([`super::window_bar`]): no page draws
//! them. Nothing here picks a colour, size, radius
//! or padding: text names its role, buttons their role, and the toolkit
//! spaces what it composes.

use ui::icons::Icon;
use ui::tokens::{Spacing, SpacingStep, TextRole};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Actions, Badge, Banner, BarGauge, Button, ButtonRole, Card, CardGrid, CardWidth, ChipStyle,
    Column, Container, ControlSize, Divider, Dropdown, Field, Form, FormSection, GaugeThresholds,
    Image, ImageCropEditor, ListRow, NumberInput, ProgressBar, Property, PropertyList, Row,
    Scrollable, Section, SectionHeader, Spacer, Text, Toggle, ToggleGroup, Tone,
};

use super::{
    Door, El, PrintPreview, ScanState, Snapshot, UiCommand, display_name, failed_areas,
    friendly_queue_state, friendly_reason, job_title, supply_level,
};

fn text(s: &str, role: TextRole) -> El {
    Box::new(Text::new(s).with_role(role))
}

fn copy(s: &str) -> El {
    text(s, TextRole::Body)
}

fn muted(s: &str) -> El {
    Box::new(Text::new(s).with_role(TextRole::Caption).dimmed())
}

/// A button that sends `command` to the page controller.
fn command_button(id: &str, label: &str, tx: &Door, command: UiCommand) -> Button {
    let tx = tx.clone();
    Button::new(id, label)
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

fn titled_section(title: &str, items: Vec<El>) -> El {
    Box::new(Section::titled(title, items))
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

/// "printers, documents or scanners"
fn join_areas(areas: &[&str]) -> String {
    match areas.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        Some((last, _)) => (*last).to_owned(),
        None => String::new(),
    }
}

/// What the person sees when a part of the system could not be read.
pub(super) fn unavailable_notice(areas: &[&str], tx: &Door) -> Option<El> {
    if areas.is_empty() {
        return None;
    }
    let subject = match areas {
        ["printers"] => "Printers couldn't be checked",
        ["documents"] => "Print jobs couldn't be checked",
        ["scanners"] => "Scanners couldn't be checked",
        _ => "Some device information couldn't be checked",
    };
    let detail = format!(
        "Print Centre couldn't get {} right now. Your saved devices and documents have not been removed.",
        join_areas(areas)
    );
    // Trying again is what resolves it, so the banner holds that action.
    Some(Box::new(
        Banner::error(subject)
            .with_message(detail)
            .with_id("print-centre.service")
            .with_action(
                command_button(
                    "print-centre.service.retry",
                    "Try again",
                    tx,
                    UiCommand::Refresh,
                )
                .with_icon(Icon::Refresh),
            ),
    ))
}

pub(super) fn loading_view() -> El {
    page(column().push(section(vec![
        copy("Looking for printers, scanners and unfinished documents."),
        Box::new(ProgressBar::indeterminate().with_label("Loading Print Centre")),
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
fn supply_meter(name: &str, level: f32) -> El {
    let percent = (level.clamp(0.0, 1.0) * 100.0).round();
    Box::new(supply_gauge(level).with_label(name, format!("{percent}%")))
}

/// How far a document has printed, with the page it is on.
fn job_progress(job: &os_actions::printing::PrintJob) -> El {
    let total = job.impressions_total.unwrap_or(1).max(1);
    let done = job.impressions_completed.unwrap_or(0).min(total);
    let percent = job.progress_percent().unwrap_or(0);
    let label = if matches!(job.state, Some(os_actions::printing::JobState::Processing)) {
        format!("Page {} of {total}", done.saturating_add(1).min(total))
    } else {
        let pages = if total == 1 { "page" } else { "pages" };
        format!("{done} of {total} {pages}")
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
        Some(os_actions::printing::JobState::Held) => "Waiting to print",
        Some(os_actions::printing::JobState::Stopped) => "Needs attention",
        _ => "Printing now",
    };
    let mut summary = bare_column()
        .push(copy(status))
        .push(muted(&format!(
            "Sent to {}",
            display_name(&job.destination)
        )))
        .push(job_progress(job));
    if let Some(reason) = job.state_reasons.first() {
        summary = summary.push(copy(friendly_reason(reason)));
    }
    let card = Card::new(Box::new(summary)).with_title(job_title(job));
    let card = match actions(vec![
        destructive(command_button(
            "print-centre.job.cancel",
            "Cancel",
            tx,
            UiCommand::RequestCancel(job.id.clone()),
        )),
        command_button("print-centre.queue", "Open queue", tx, UiCommand::Queue),
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
        supply_meter("Lowest supply", level)
    } else {
        muted("Supply levels unavailable")
    };
    let detail = if queue.default {
        "Preferred printer"
    } else {
        "Available to your apps"
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

fn tool_row(id: &str, icon: Icon, title: &str, detail: &str, tx: &Door, command: UiCommand) -> El {
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
                copy("No printers added yet. Find one on your network to print from any app."),
                Box::new(
                    primary(command_button(
                        "print-centre.discover.empty",
                        "Find a printer",
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
                    .push(Box::new(
                        Section::new(Vec::new())
                            .with_header(SectionHeader::heading("Your printers")),
                    ))
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
            "Scan a document",
            &format!("{} is ready", scanner.description),
            tx,
            UiCommand::ScanSetup,
        ));
    }
    if jobs_available {
        let history = if jobs
            .iter()
            .any(|job| matches!(job.state, Some(os_actions::printing::JobState::Completed)))
        {
            "Review what printed and print it again"
        } else {
            "Completed documents will appear here"
        };
        tools.push(tool_row(
            "print-centre.history.open",
            Icon::History,
            "Print history",
            history,
            tx,
            UiCommand::History,
        ));
    }
    tools.push(tool_row(
        "print-centre.diagnostics.open",
        Icon::Info,
        "Device help",
        "Check connections, paper, supplies and scanners",
        tx,
        UiCommand::Diagnostics,
    ));
    Box::new(Section::titled(
        "Tools",
        vec![Box::new(tools.into_iter().fold(
            bare_column().with_rhythm(ui::tokens::Rhythm::Section),
            Column::push,
        ))],
    ))
}

fn history_status(job: &os_actions::printing::PrintJob) -> &'static str {
    match job.state {
        Some(os_actions::printing::JobState::Completed) => "Printed",
        Some(os_actions::printing::JobState::Canceled) => "Canceled",
        _ => "Did not print",
    }
}

/// A finished document: what became of it and where, with the way to print
/// it again always in view.
fn history_card(job: &os_actions::printing::PrintJob, tx: &Door) -> El {
    let again = command_button(
        &format!("print-centre.history.repeat.{}", job.id),
        "Print again",
        tx,
        UiCommand::RetryJob(job.id.clone()),
    )
    .with_icon(Icon::Refresh);
    let body = bare_column()
        .push(copy(&format!(
            "{} on {}",
            history_status(job),
            display_name(&job.destination)
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
        items.push(copy(
            "No completed documents yet. After a document finishes or is canceled, it will appear here.",
        ));
    } else {
        items.push(titled_section(
            "Documents",
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

fn device_summary(
    supplies_known: bool,
    queue: &os_actions::printing::PrinterQueue,
) -> &'static str {
    if supplies_known {
        "The printer answered the latest status check."
    } else if queue.telemetry.available {
        "This printer does not report supply levels."
    } else {
        "Live details are unavailable. The printer may be asleep or disconnected."
    }
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
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
            "Printers responding",
            format!("{responding} of {}", queues.len()),
        ),
        Property::new(
            "Reporting ink or toner levels",
            format!(
                "{supply_reports} {}",
                plural(supply_reports, "printer", "printers")
            ),
        ),
        Property::new("Scanners available", scanners.len().to_string()),
    ]))])
    .with_header(SectionHeader::heading("Connection check"));
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
        "Open printer controls",
        tx,
        UiCommand::Device(queue.name.clone()),
    )));
    titled_section(&display_name(&queue.name), items)
}

fn scanner_check(none_found: bool, tx: &Door) -> El {
    let items: Vec<El> = if none_found {
        vec![copy(
            "No scanner was found. Check its cable or network connection, then check again.",
        )]
    } else {
        vec![
            copy("Your scanner is available for a new document."),
            Box::new(command_button(
                "print-centre.diagnostics.scan",
                "Start scanning",
                tx,
                UiCommand::ScanSetup,
            )),
        ]
    };
    titled_section("Scanner", items)
}

fn queue_state(job: &os_actions::printing::PrintJob) -> &'static str {
    match job.state {
        Some(os_actions::printing::JobState::Completed) => "Printed",
        Some(os_actions::printing::JobState::Canceled) => "Canceled",
        Some(os_actions::printing::JobState::Aborted) => "Could not print",
        Some(os_actions::printing::JobState::Held) => "Waiting",
        Some(os_actions::printing::JobState::Stopped) => "Needs attention",
        _ => "In progress",
    }
}

fn queue_summary(job: &os_actions::printing::PrintJob) -> Column {
    let summary = bare_column()
        .push(copy(queue_state(job)))
        .push(muted(&format!(
            "Sent to {}",
            display_name(&job.destination)
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
            "Try again",
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
                "Confirm cancel"
            } else {
                "Cancel"
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
                .with_message("It will be removed from the printer. This cannot be undone."),
        ));
    }
    if jobs.is_empty() && snapshot.jobs.is_ok() {
        items.push(copy(
            "Nothing waiting. New documents appear here as soon as an app sends them.",
        ));
    } else {
        items.push(titled_section(
            "Documents",
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
            "Pause printer"
        } else {
            "Resume printer"
        },
        tx,
        UiCommand::SetEnabled(queue.name.clone(), !enabled),
    )];
    if !queue.default {
        buttons.push(command_button(
            "print-centre.device.default",
            "Make preferred",
            tx,
            UiCommand::SetDefault(queue.name.clone()),
        ));
    }
    buttons.push(command_button(
        "print-centre.device.queue",
        "View print queue",
        tx,
        UiCommand::Queue,
    ));
    buttons.push(command_button(
        "print-centre.device.diagnostics",
        "Check printer",
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
        _ => vec![copy("This printer does not report supply levels.")],
    };
    let status = Property::new("State", friendly_queue_state(queue));
    let availability = Property::new(
        "Availability",
        if queue.default {
            "Preferred printer"
        } else {
            "Available to your apps"
        },
    );
    page(groups(vec![
        titled_section(
            "Details",
            vec![Box::new(PropertyList::new(vec![status, availability]))],
        ),
        titled_section("Ink and toner", supplies),
        titled_section("Printer controls", vec![button_row(buttons)]),
    ]))
}

pub(super) fn discovery_view(
    result: Option<&Result<Vec<os_actions::printing::DiscoveredPrinter>, String>>,
    tx: &Door,
) -> El {
    let body: El = match result {
        None => section(vec![
            copy("Looking for printers on your network."),
            Box::new(ProgressBar::indeterminate().with_label("Finding nearby printers")),
        ]),
        Some(Ok(printers)) if printers.is_empty() => section(vec![
            copy(
                "No printers found. Make sure the printer is awake and connected to the same network, then search again.",
            ),
            Box::new(command_button(
                "print-centre.discover.retry",
                "Search again",
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
                        Card::new(muted(printer.location.as_deref().unwrap_or("Nearby")))
                            .with_title_action(
                                &printer.name,
                                Box::new(
                                    command_button(
                                        &add_id,
                                        "Add",
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
                "Printers on your network",
                vec![Box::new(
                    CardGrid::new(cards).with_card_width(CardWidth::Wide),
                )],
            )
        }
        Some(Err(_)) => section(vec![
            copy("Could not search right now. Check your network connection and try again."),
            Box::new(command_button(
                "print-centre.discover.retry",
                "Try again",
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
        .with_placeholder("Choose a printer");
    picker.set_value_options(options);
    if let Some(selected) = selected {
        picker.set_selected(selected);
    }
    let select_tx = tx.clone();
    picker.on_change(move |name| {
        let _ = select_tx.send(UiCommand::SelectPrinter(name.to_owned()));
    })
}

/// The document being printed: its preview, which the person can check before
/// anything is sent. The preview spans the card, so the page sits in its middle.
fn document_card(filename: &str, preview: &PrintPreview) -> El {
    let preview_widget: El = match preview {
        PrintPreview::Ready {
            rgba,
            width,
            height,
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
        PrintPreview::Loading => Box::new(Container::new().with_height(138.0).with_child(
            Box::new(ProgressBar::indeterminate().with_label("Preparing document preview")),
        )),
        PrintPreview::Unavailable => Box::new(
            bare_column()
                .push(copy("Preview unavailable"))
                .push(muted("The original document is unchanged.")),
        ),
    };
    Box::new(Card::new(preview_widget).with_title(filename))
}

fn print_settings(
    picker: Dropdown,
    copies: &ui::LocalState<f64>,
    two_sided: &ui::LocalState<bool>,
    tx: &Door,
) -> Form {
    let defaults = tx.clone();
    Form::new()
        .section(
            FormSection::new("Printer").field(
                Field::new("Send to", picker).with_help("Choose where to send this document."),
            ),
        )
        .section(
            FormSection::new("Options")
                .grouped()
                .field(
                    Field::new(
                        "Copies",
                        NumberInput::from_state(copies)
                            .with_range(1.0, 99.0)
                            .with_step(1.0)
                            .with_id("print-centre.print.copies")
                            .compact(),
                    )
                    .with_trailing_control(),
                )
                .field(
                    Field::new(
                        "Print on both sides",
                        Toggle::from_state(two_sided).on_change(move |value| {
                            let _ = defaults.send(UiCommand::SetTwoSided(value));
                        }),
                    )
                    .with_trailing_control(),
                ),
        )
}

pub(super) fn print_setup_view(
    snapshot: &Snapshot,
    path: &std::path::Path,
    preview: &PrintPreview,
    selected: Option<&str>,
    copies: &ui::LocalState<f64>,
    two_sided: &ui::LocalState<bool>,
    tx: &Door,
) -> El {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Selected document");
    let settings = print_settings(
        printer_picker(snapshot, selected, tx),
        copies,
        two_sided,
        tx,
    );
    let layout = Row::new()
        .align_start()
        .push(Box::new(
            Container::new()
                .with_width(260.0)
                .with_no_padding()
                .with_child(document_card(filename, preview)),
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
        "Cancel",
        tx,
        UiCommand::CancelPrintSetup,
    )];
    if selected.is_some() {
        buttons.push(primary(command_button(
            "print-centre.print.submit",
            "Print",
            tx,
            UiCommand::PrintDocument,
        )));
    }
    page_with_footer(body, actions(buttons))
}

/// A choice with its name above it.
fn setting(label: &str, control: ToggleGroup) -> El {
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

/// A segmented choice that stores its index in `state` and rebuilds nothing.
fn choice(id: &str, labels: &[&str], state: &ui::LocalState<usize>) -> ToggleGroup {
    let state_for_change = state.clone();
    ToggleGroup::new(segments(labels), state.get())
        .with_id(id)
        .on_view(&format!("{id}.select"), move |index| {
            state_for_change.set(index);
        })
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
        "Portrait"
    } else {
        "Landscape"
    };
    let mut details = bare_column()
        .push(text(
            &format!("Page {} of {page_count}", selected_page.saturating_add(1)),
            TextRole::Heading,
        ))
        .push(Box::new(PropertyList::new(vec![
            Property::new("Orientation", orientation),
            Property::new("Text language", &page_data.ocr_language),
        ])))
        .push(muted(&page_data.ocr_note));
    if let Some(recognized) = &page_data.recognized_text {
        let snippet: String = recognized.chars().take(320).collect();
        if !snippet.trim().is_empty() {
            details = details.push(titled_section("Recognized text", vec![copy(&snippet)]));
        }
    }
    let mut tools = vec![
        command_button(
            "print-centre.scan.rotate",
            "Rotate right",
            tx,
            UiCommand::RotateScanPage,
        )
        .with_icon(Icon::RotateCw),
        command_button(
            "print-centre.scan.apply-crop",
            "Apply crop",
            tx,
            UiCommand::ApplyScanCrop,
        ),
    ];
    let mut tool_items: Vec<El> = Vec::new();
    if page_data.ocr_in_progress {
        tool_items.push(muted("Updating recognized text…"));
    } else if page_data.ocr_needs_refresh {
        tools.push(command_button(
            "print-centre.scan.recognize",
            "Update recognized text",
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
            "Copy recognized text",
            tx,
            UiCommand::CopyRecognizedText,
        ));
    }
    tool_items.insert(0, button_row(tools));
    details.push(titled_section("Page tools", tool_items))
}

/// Moving between, reordering and removing the pages of the document.
fn page_navigation(selected_page: usize, page_count: usize, tx: &Door) -> Vec<Button> {
    let has_previous = selected_page > 0;
    let has_next = selected_page.saturating_add(1) < page_count;
    let mut buttons: Vec<Button> = Vec::new();
    if has_previous {
        buttons.push(command_button(
            "print-centre.scan.previous",
            "Previous page",
            tx,
            UiCommand::PreviousScanPage,
        ));
    }
    if has_next {
        buttons.push(command_button(
            "print-centre.scan.next",
            "Next page",
            tx,
            UiCommand::NextScanPage,
        ));
    }
    if has_previous {
        buttons.push(command_button(
            "print-centre.scan.move-earlier",
            "Move before previous page",
            tx,
            UiCommand::MoveScanPageEarlier,
        ));
    }
    if has_next {
        buttons.push(command_button(
            "print-centre.scan.move-later",
            "Move after next page",
            tx,
            UiCommand::MoveScanPageLater,
        ));
    }
    buttons.push(destructive(command_button(
        "print-centre.scan.delete-page",
        "Remove this page",
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
                    "Crop",
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

    let mut items: Vec<El> = Vec::new();
    items.push(Box::new(review));
    items.push(titled_section(
        "Pages in this document",
        vec![
            muted(&format!(
                "{page_count} {}",
                plural(page_count, "page", "pages")
            )),
            button_row(page_navigation(selected_page, page_count, tx)),
        ],
    ));
    let body = groups(items);
    let footer = actions(vec![
        command_button(
            "print-centre.scan.new-document",
            "New scanned document",
            tx,
            UiCommand::NewScannedDocument,
        ),
        command_button(
            "print-centre.scan.save-page",
            "Export this page",
            tx,
            UiCommand::SaveScannedPage,
        ),
        command_button(
            "print-centre.scan.again",
            "Scan another page",
            tx,
            UiCommand::ScanAnother,
        ),
        primary(command_button(
            "print-centre.scan.save-document",
            "Save document",
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
            devices.push(copy(
                "No scanner is available. Check its cable or network connection, then refresh Print Centre.",
            ));
        }
    } else if let Some(issue) = unavailable_notice(&["scanners"], tx) {
        devices.push(issue);
    }
    devices
}

fn scan_settings(scan: &ScanState, tx: &Door) -> Vec<El> {
    vec![
        setting(
            "Quality",
            choice(
                "print-centre.scan.resolution",
                &["150 dpi", "300 dpi", "600 dpi"],
                &scan.resolution,
            )
            .on_change({
                let tx = tx.clone();
                move |index| {
                    let _ = tx.send(UiCommand::SetScanResolution(index));
                }
            }),
        ),
        setting(
            "Appearance",
            choice(
                "print-centre.scan.color",
                &["Color", "Grayscale", "Text"],
                &scan.color,
            )
            .on_change({
                let tx = tx.clone();
                move |index| {
                    let _ = tx.send(UiCommand::SetScanColor(index));
                }
            }),
        ),
        setting(
            "Paper source",
            choice(
                "print-centre.scan.source",
                &["Flatbed", "Feeder", "Two-sided"],
                &scan.source,
            )
            .on_change({
                let tx = tx.clone();
                move |index| {
                    let _ = tx.send(UiCommand::SetScanSource(index));
                }
            }),
        ),
        setting(
            "Text language",
            choice(
                "print-centre.scan.language",
                &["English", "German", "French", "Spanish"],
                &scan.language,
            )
            .on_change({
                let tx = tx.clone();
                move |index| {
                    let _ = tx.send(UiCommand::SetOcrLanguage(index));
                }
            }),
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
        items.push(muted(&format!(
            "{page_count} {} captured. New scans are added to this document.",
            plural(page_count, "page", "pages")
        )));
    }
    items.push(titled_section(
        "Scanner",
        scanner_choices(snapshot, selected, tx),
    ));
    items.push(titled_section("Page settings", scan_settings(scan, tx)));
    let body = groups(items);

    let mut buttons = vec![
        command_button(
            "print-centre.scan.cancel",
            "Cancel",
            tx,
            UiCommand::Dashboard,
        ),
        command_button(
            "print-centre.scan.new-document",
            "New scanned document",
            tx,
            UiCommand::NewScannedDocument,
        )
        .with_enabled(!scan.in_progress && tx.0.is_some()),
    ];
    if scan.in_progress {
        buttons.push(
            command_button(
                "print-centre.scan.busy",
                "Scanning page…",
                tx,
                UiCommand::Dashboard,
            )
            .with_enabled(false),
        );
    } else if selected.is_some() {
        buttons.push(primary(command_button(
            "print-centre.scan.start",
            "Scan page",
            tx,
            UiCommand::ScanPage,
        )));
    }
    page_with_footer(body, actions(buttons))
}
