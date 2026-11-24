/// The printers this computer offers, the jobs they carry and the printers it can find, each
/// read from the printers resource and changed by its own actions, so Print Centre and this
/// page change one set of queues.
view printers_pane() {
    let printers = device.printers
    ui.Section("Printers") {
        if not printers.serving {
            ui.Text("This computer has no printer service", role: .caption, secondary: true)
        } else {
            if printers.queues.is_empty() {
                ui.EmptyState("No printer", icon: .printer, inline: true)
            }
            for queue in printers.queues {
                ui.ListRow(
                    queue.name,
                    secondary: queue_words(queue),
                    selected: queue.default,
                    icon: .printer,
                ) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            if not queue.default {
                                ui.Button("Use first", size: .small) {
                                    on press: device.set_default(
                                        device.printers,
                                        printer: queue.name,
                                    )
                                }
                            }
                            ui.Button(
                                if queue.state == .paused { "Resume" } else { "Pause" },
                                size: .small,
                            ) {
                                on press: device.set_paused(
                                    device.printers,
                                    printer: queue.name,
                                    paused: queue.state != .paused,
                                )
                            }
                            ui.Button(
                                "Remove this printer",
                                icon: .trash,
                                icon_only: true,
                                role: .destructive,
                                size: .small,
                            ) {
                                on press: device.remove(device.printers, printer: queue.name)
                            }
                        }
                    }
                }
                if not queue.media.is_empty() {
                    ui.Text("Paper", role: .caption, secondary: true)
                    row(gap: .tight, align: .center, wrap: true) {
                        for size in queue.media.take(8) {
                            ui.Button(
                                size,
                                size: .small,
                                toggled: queue.default_media == .some(size),
                            ) {
                                on press: device.set_media(
                                    device.printers,
                                    printer: queue.name,
                                    media: size,
                                )
                            }
                        }
                    }
                }
                for supply in queue.supplies {
                    if let level = supply.level {
                        ui.ListRow(supply.name, secondary: "{level}% left", icon: .droplet)
                    } else {
                        ui.ListRow(
                            supply.name,
                            secondary: "Its level is not reported",
                            icon: .droplet,
                        )
                    }
                }
            }
            ui.Button("Look for printers", icon: .search) {
                on press: device.find(device.printers)
            }
            for found in printers.nearby {
                ui.ListRow(
                    found.name,
                    secondary: if let where_ = found.location { where_ } else { found.uri },
                    icon: .printer,
                ) {
                    trailing: {
                        ui.Button("Add", size: .small, role: .primary) {
                            on press: device.add(device.printers, uri: found.uri, name: found.name)
                        }
                    }
                }
            }
        }
    }
    ui.Section("Jobs") {
        if printers.jobs.is_empty() {
            ui.Text("Nothing waits to print", role: .caption, secondary: true)
        }
        for job in printers.jobs {
            ui.ListRow(
                if job.title != "" { job.title } else { "Untitled document" },
                secondary: "{job.destination} - {job_words(job)}",
                icon: .file,
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        if job.state == .stopped {
                            ui.Button("Retry", size: .small) {
                                on press: device.retry_job(device.printers, job: job.id)
                            }
                        }
                        ui.Button(
                            "Cancel this job",
                            icon: .x,
                            icon_only: true,
                            role: .destructive,
                            size: .small,
                        ) {
                            on press: device.cancel_job(device.printers, job: job.id)
                        }
                    }
                }
            }
        }
    }
    if let problem = printers.problem {
        ui.StatusLine(problem, status: .error)
    }
    let scanner_service = device.scanners
    ui.Section("Scanners") {
        if let why = unreadable(scanner_service) {
            std.unreadable_row("Scanners could not be read", why)
        } else if not scanner_service.available {
            ui.Text("This computer has no scanner service", role: .caption, secondary: true)
        } else {
            if scanner_service.found.is_empty() {
                ui.EmptyState("No scanner found", icon: .scan_line, inline: true)
            }
            for scanner in scanner_service.found {
                ui.ListRow(scanner.description, secondary: scanner.device, icon: .scan_line)
            }
            ui.Button("Look for scanners", icon: .search) {
                on press: device.find_scanners(device.scanners)
            }
            if let problem = scanner_service.problem {
                ui.StatusLine(problem, status: .error)
            }
        }
    }
}

/// How a queue stands, in a word.
fn queue_words(queue: device.PrinterQueue) -> Text {
    if queue.state == .printing {
        "Printing"
    } else if queue.state == .paused {
        if let why = queue.reason { "Paused: {why}" } else { "Paused" }
    } else if queue.state == .offline {
        "Offline"
    } else if queue.default { "Ready; the one used first" } else { "Ready" }
}

/// How a job stands, in a word.
fn job_words(job: device.PrintJob) -> Text {
    if job.state == .printing {
        "Printing"
    } else if job.state == .held {
        "Held"
    } else if job.state == .stopped {
        "Stopped"
    } else if job.state == .cancelled {
        "Cancelled"
    } else if job.state == .failed {
        "Failed"
    } else if job.state == .printed { "Printed" } else { "Waiting" }
}
