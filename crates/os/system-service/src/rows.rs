//! What this computer's readings offer a person's search: each item of a list a reading keeps,
//! by its own title and how choosing it opens it.

use tilo_actions::ReadingRow;
use tilo_journal::JOURNAL;
use tilo_system_resources::containers::CONTAINERS;
use tilo_system_resources::disks::DISKS;
use tilo_system_resources::printers::PRINTERS;
use tilo_system_resources::processes::PROCESSES;
use tilo_system_resources::scanners::SCANNERS;
use tilo_system_resources::windows_apps::{WINDOWS_APPS, WindowsApps};

use crate::Readings;
use crate::i18n::name;

/// The apps that show a reading's items, opened when the reading offers no action on one.
const LOGS_APP: &str = "so.tilo.logs";
const CONTAINERS_APP: &str = "so.tilo.containers";
const DISKS_APP: &str = "so.tilo.disks";
const MONITOR_APP: &str = "so.tilo.monitor";
const PRINT_APP: &str = "so.tilo.print-centre";
/// The Windows Apps window, which shows a program of this computer at its page.
const WINDOWS_APP: &str = "so.tilo.windows";

/// The longest a record's message reads as a row's title.
const TITLE: usize = 96;
/// The longest of a container's log text one row keeps to be found by.
const LOG_TEXT: usize = 4 * 1024;

/// One item as a row: its stable id in the reading, its title, and how it opens.
fn row(id: impl Into<String>, title: impl Into<String>) -> ReadingRow {
    ReadingRow::new(id, title)
}

/// `text` as a row reads it: whitespace collapsed, cut to [`TITLE`].
fn cut(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > TITLE {
        let mut short: String = collapsed.chars().take(TITLE - 1).collect();
        short.push('…');
        short
    } else {
        collapsed
    }
}

/// Every item of every reading that is a row of its own.
#[must_use]
pub fn rows(readings: &Readings) -> Vec<(String, ReadingRow)> {
    let mut rows = Vec::new();
    rows.extend(journal_rows(readings));
    rows.extend(container_rows(readings));
    rows.extend(disk_rows(readings));
    rows.extend(process_rows(readings));
    rows.extend(print_rows(readings));
    rows
}

/// This computer's Windows programs, as its compatibility layer lists them: each a row the
/// Windows Apps window shows, so choosing one opens that window at the program's page.
#[must_use]
pub fn windows_rows(programs: &WindowsApps) -> Vec<(String, ReadingRow)> {
    let group = name("so.tilo.system.windows-apps", "Windows apps");
    programs
        .apps
        .iter()
        .map(|program| {
            let found = row(program.id.clone(), program.name.clone())
                .subtitled(program.publisher.clone())
                .grouped(group.clone())
                .icon("app-window")
                .field(&program.runner)
                .fact(program.runner.clone())
                .shown_by(WINDOWS_APP);
            (WINDOWS_APPS.to_owned(), found)
        })
        .collect()
}

fn journal_rows(readings: &Readings) -> Vec<(String, ReadingRow)> {
    let group = name("so.tilo.system.journal", "System journal");
    readings
        .journal
        .entries
        .iter()
        .map(|entry| {
            let title = cut(&entry.message);
            let mut found = row(entry.realtime_us.to_string(), title)
                .subtitled(entry.unit.clone())
                .grouped(group.clone())
                .icon("scroll-text")
                .fact(entry.time.clone())
                .field(&entry.identifier)
                .field(&entry.message)
                .shown_by(LOGS_APP);
            if !entry.hostname.is_empty() {
                found = found.field(&entry.hostname);
            }
            (JOURNAL.to_owned(), found)
        })
        .collect()
}

fn container_rows(readings: &Readings) -> Vec<(String, ReadingRow)> {
    let containers = name("so.tilo.system.containers", "Containers");
    let mut rows = Vec::new();
    for container in &readings.containers.containers {
        let title = if container.name.is_empty() {
            container.short_id().to_owned()
        } else {
            container.name.clone()
        };
        let mut found = row(container.id.clone(), title)
            .subtitled(container.status_detail.clone())
            .grouped(containers.clone())
            .icon("box")
            .field(&container.image)
            .field(&container.command)
            .shown_by(CONTAINERS_APP);
        for label in &container.labels {
            found = found.field(format!("{} {}", label.name, label.value));
        }
        // The log text a container kept is found by its words, as the app's log pane reads it.
        let logs = container.logs.join("\n");
        if !logs.trim().is_empty() {
            let logs: String = logs.chars().take(LOG_TEXT).collect();
            found = found.field(logs);
        }
        rows.push((CONTAINERS.to_owned(), found));
    }
    for image in &readings.containers.images {
        let found = row(image.id.clone(), image.reference())
            .grouped(name("so.tilo.system.containers.image", "Images"))
            .icon("layers")
            .field(&image.id)
            .shown_by(CONTAINERS_APP);
        rows.push((CONTAINERS.to_owned(), found));
    }
    for volume in &readings.containers.volumes {
        let found = row(volume.name.clone(), volume.name.clone())
            .subtitled(volume.driver.clone())
            .grouped(name("so.tilo.system.containers.volume", "Volumes"))
            .icon("database")
            .fact(volume.mountpoint.clone())
            .shown_by(CONTAINERS_APP);
        rows.push((CONTAINERS.to_owned(), found));
    }
    for distrobox in &readings.containers.distroboxes {
        let found = row(distrobox.name.clone(), distrobox.name.clone())
            .subtitled(distrobox.status_detail.clone())
            .grouped(name("so.tilo.system.containers.distrobox", "Distroboxes"))
            .icon("box-select")
            .field(&distrobox.image)
            .shown_by(CONTAINERS_APP);
        rows.push((CONTAINERS.to_owned(), found));
    }
    rows
}

fn disk_rows(readings: &Readings) -> Vec<(String, ReadingRow)> {
    let drives = name("so.tilo.system.disks", "Disks");
    let partitions = name("so.tilo.system.disks.partition", "Partitions");
    let mut rows = Vec::new();
    for drive in &readings.disks.drives {
        let title = drive.model.clone().unwrap_or_else(|| drive.name.clone());
        let mut found = row(drive.name.clone(), title)
            .grouped(drives.clone())
            .icon("hard-drive")
            .field(&drive.name)
            .fact(drive.kind.clone());
        if let Some(serial) = &drive.serial {
            found = found.subtitled(serial.clone()).field(serial);
        }
        if let Some(mountpoint) = &drive.mountpoint {
            found = found.fact(mountpoint.clone());
        }
        let opens = if drive.partitions.is_empty() {
            found.opened_by("mount", [("device", drive.name.clone())])
        } else {
            found.shown_by(DISKS_APP)
        };
        rows.push((DISKS.to_owned(), opens));
        for part in &drive.partitions {
            let title = part.label.clone().unwrap_or_else(|| part.name.clone());
            let mut found = row(part.name.clone(), title)
                .grouped(partitions.clone())
                .icon("database")
                .field(&part.name)
                .fact(drive.name.clone());
            if let Some(fstype) = &part.fstype {
                found = found.subtitled(fstype.clone()).field(fstype);
            }
            if let Some(mountpoint) = &part.mountpoint {
                found = found.fact(mountpoint.clone());
            }
            rows.push((
                DISKS.to_owned(),
                found.opened_by("mount", [("device", part.name.clone())]),
            ));
        }
    }
    rows
}

fn process_rows(readings: &Readings) -> Vec<(String, ReadingRow)> {
    let group = name("so.tilo.system.processes", "Processes");
    readings
        .processes
        .processes
        .iter()
        .map(|process| {
            let found = row(
                format!("{}:{}", process.pid, process.started_at),
                process.name.clone(),
            )
            .subtitled(process.user.clone())
            .grouped(group.clone())
            .icon("activity")
            .field(&process.name)
            .fact(format!("pid {}", process.pid))
            .shown_by(MONITOR_APP);
            (PROCESSES.to_owned(), found)
        })
        .collect()
}

fn print_rows(readings: &Readings) -> Vec<(String, ReadingRow)> {
    let printers = name("so.tilo.system.printers", "Printers");
    let jobs = name("so.tilo.system.printers.job", "Print jobs");
    let scanners = name("so.tilo.system.scanners", "Scanners");
    let mut rows = Vec::new();
    for queue in &readings.printers.queues {
        let mut found = row(queue.name.clone(), queue.name.clone())
            .grouped(printers.clone())
            .icon("printer")
            .shown_by(PRINT_APP);
        if let Some(reason) = &queue.reason {
            found = found.subtitled(reason.clone()).field(reason);
        }
        for size in &queue.media {
            found = found.field(size);
        }
        rows.push((PRINTERS.to_owned(), found));
    }
    for job in &readings.printers.jobs {
        let title = if job.title.trim().is_empty() {
            format!("Job {}", job.id)
        } else {
            job.title.clone()
        };
        let found = row(job.id.clone(), title)
            .subtitled(job.destination.clone())
            .grouped(jobs.clone())
            .icon("file")
            .field(&job.owner)
            .shown_by(PRINT_APP);
        rows.push((PRINTERS.to_owned(), found));
    }
    for scanner in &readings.scanners.found {
        let found = row(scanner.device.clone(), scanner.description.clone())
            .grouped(scanners.clone())
            .icon("scan-line")
            .field(&scanner.device)
            .shown_by(PRINT_APP);
        rows.push((SCANNERS.to_owned(), found));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_journal::JournalEntry;
    use tilo_system_resources::containers::{
        Container, ContainerState, ContainerStats, Containers,
    };
    use tilo_system_resources::disks::Partition;
    use tilo_system_resources::printers::{JobState, PrintJob, PrinterQueue, QueueState};
    use tilo_system_resources::processes::Process;
    use tilo_system_resources::scanners::ScannerShown;

    fn readings() -> Readings {
        let mut readings = Readings::default();
        readings.journal.entries = vec![JournalEntry {
            realtime_us: 42,
            unit: "sshd.service".into(),
            identifier: "sshd".into(),
            priority: 6,
            message: "Accepted publickey for ada".into(),
            pid: Some(900),
            hostname: "workstation".into(),
            boot_id: "boot".into(),
            transport: "journal".into(),
            extra: Vec::new(),
            time: "10:00".into(),
            timestamp: "2026-11-24 10:00".into(),
        }];
        readings.containers = Containers {
            containers: vec![Container {
                id: "c1".into(),
                name: "web".into(),
                image: "nginx:1".into(),
                state: ContainerState::Running,
                status_detail: "Up 2 hours".into(),
                command: String::new(),
                created: String::new(),
                ports: Vec::new(),
                mounts: Vec::new(),
                env: Vec::new(),
                labels: Vec::new(),
                stats: ContainerStats::default(),
                logs: vec!["serving on port 80".into()],
                logs_error: None,
                logs_fetched_unix_ms: None,
            }],
            ..Containers::default()
        };
        readings.disks.drives = vec![tilo_system_resources::disks::Drive {
            name: "sda".into(),
            model: Some("Work SSD".into()),
            partitions: vec![Partition {
                name: "sda2".into(),
                label: Some("Data".into()),
                fstype: Some("ext4".into()),
                part_type_name: None,
                size_bytes: 1,
                used_bytes: None,
                mountpoint: None,
                uuid: None,
                encryption: None,
            }],
            ..tilo_system_resources::disks::Drive::default()
        }];
        readings.processes.processes = vec![Process {
            pid: 4242,
            started_at: 7,
            name: "firefox".into(),
            user: "ada".into(),
            endable: true,
            ..Process::default()
        }];
        readings.printers.queues = vec![PrinterQueue {
            name: "Office".into(),
            state: QueueState::Ready,
            default: true,
            accepting: true,
            reason: None,
            reason_code: None,
            supplies: Vec::new(),
            reachable: true,
            media: vec!["A4".into()],
            default_media: None,
        }];
        readings.printers.jobs = vec![PrintJob {
            id: "7".into(),
            title: "Quarterly report".into(),
            owner: "ada".into(),
            size_bytes: 1,
            submitted: "10:00".into(),
            destination: "Office".into(),
            state: JobState::Waiting,
            progress: None,
        }];
        readings.scanners.found = vec![ScannerShown {
            device: "epson:1".into(),
            description: "Epson Perfection".into(),
        }];
        readings
    }

    /// Every reading's items are rows of their own, each opened the way its reading offers: the
    /// partition mounts, and the rest open the app that shows them.
    #[test]
    fn every_reading_lists_its_items_with_how_choosing_one_opens_it() {
        let rows = rows(&readings());
        let find = |title: &str| {
            rows.iter()
                .find(|(_, row)| row.title == title)
                .unwrap_or_else(|| panic!("`{title}` is a row: {rows:#?}"))
        };

        let (resource, record) = find("Accepted publickey for ada");
        assert_eq!(resource, JOURNAL);
        assert_eq!(record.app.as_deref(), Some(LOGS_APP));

        let (resource, container) = find("web");
        assert_eq!(resource, CONTAINERS);
        assert_eq!(container.app.as_deref(), Some(CONTAINERS_APP));
        assert!(
            container
                .fields
                .iter()
                .any(|field| field.contains("port 80")),
            "a container's log text is found"
        );

        let (resource, partition) = find("Data");
        assert_eq!(resource, DISKS);
        assert_eq!(partition.action.as_deref(), Some("mount"));
        assert_eq!(partition.arguments["device"], "sda2");

        let (resource, process) = find("firefox");
        assert_eq!(resource, PROCESSES);
        assert_eq!(process.app.as_deref(), Some(MONITOR_APP));

        assert_eq!(find("Office").0, PRINTERS);
        assert_eq!(find("Quarterly report").0, PRINTERS);
        assert_eq!(find("Quarterly report").1.app.as_deref(), Some(PRINT_APP));
        assert_eq!(find("Epson Perfection").1.app.as_deref(), Some(PRINT_APP));
    }
}
