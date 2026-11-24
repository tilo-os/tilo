app so.tilo.disks "Disks" (
    icon: .hard_drive,
    category: .system,
    tagline: "This computer's drives, their partitions and their health",
    keywords: ["disks", "drives", "partitions", "format", "mount", "smart", "laufwerke", "festplatten"],
    splash_budget: 250,
)

use so.tilo.device as device

/// The names a person gives their drives, by serial number: the workspace's, so a name follows
/// the drive to every device it is plugged into.
type Disks {
    names: Map<Text>
}

root disks: Disks

default view window for Disks

/// What a person calls a drive: the name they gave it, else its model, else its name in `/dev`.
fn drive_name(d: Disks, drive: device.Drive) -> Text {
    if let serial = drive.serial {
        if let named = d.names.get(serial) {
            named
        } else if let model = drive.model { model } else { drive.name }
    } else if let model = drive.model { model } else { drive.name }
}

/// Bytes in the words a person reads.
fn size_words(bytes: Int) -> Text {
    if bytes < 1000 {
        std.say("{bytes} B")
    } else if bytes < 1000000 {
        std.say("{bytes / 1000} kB")
    } else if bytes < 1000000000 {
        std.say("{bytes / 1000000} MB")
    } else if bytes < 1000000000000 {
        std.say("{bytes / 1000000000} GB")
    } else { std.say("{bytes / 1000000000000} TB") }
}

fn rate_words(bytes: Float) -> Text {
    if bytes < 1000000.0 {
        std.say("{(bytes / 1000.0).to_int()} kB/s")
    } else if bytes < 1000000000.0 {
        std.say("{(bytes / 1000000.0).to_int()} MB/s")
    } else {
        std.say("{(bytes / 1000000000.0).to_int()} GB/s")
    }
}

/// The size a drive allocates to its partitions.
fn allocated(drive: device.Drive, at: Int) -> Int {
    if at >= drive.partitions.len() {
        0
    } else {
        (drive.partitions.at(at)?.size_bytes ?? 0) + allocated(drive, at + 1)
    }
}

fn health_words(health: device.DiskHealth) -> Text {
    if health.failing {
        std.say("Predicted to fail")
    } else if health.supported { std.say("Healthy") } else { std.say("Its health is not reported") }
}

/// A drive's name in the words of the person reading it, by its serial.
action name_drive(d: Disks, serial: Text, name: Text) {
    if name.trim() != "" { d.names.put(serial, name.trim()) }
}

/// The filesystems a person may write from here.
fn filesystems() -> List<Text> {
    ["ext4", "xfs", "btrfs", "exfat", "vfat"]
}

/// The window: the drives down one side, and the chosen one beside them.
view window(d: Disks) {
    state chosen: Text = ""
    state tab: Text = "overview"
    state formatting: Text = ""
    state filesystem: Text = "ext4"
    let disks = device.disks
    let drive = disks.drives.find(item -> item.name == chosen)
    ui.NavShell(content_kind: .page, title: "Disks", sidebar_title: "Drives") {
        sidebar: {
            stack {
                if let error = disks.read_error {
                    ui.StatusLine(error, status: .error)
                }
                if disks.drives.is_empty() {
                    ui.EmptyState("No drives", icon: .hard_drive, inline: true)
                }
                stack(gap: .none) {
                    for each in disks.drives {
                        drive_row(d, each, chosen)
                    }
                }
            }
        }
        actions: {
            ui.Button("Read the drives again", icon: .rotate_cw, icon_only: true) {
                on press: device.refresh(device.disks)
            }
        }
        if let drive = drive {
            drive_detail(d, disks, drive, tab, formatting)
        } else {
            ui.EmptyState(
                "Choose a drive",
                message: "Pick a drive on the left to see its map, its health and its partitions.",
                icon: .hard_drive,
            )
        }
    }
    format_dialog(formatting, filesystem)
}

/// One drive in the list: what it is called, how big it is, and whether it needs attention.
view drive_row(d: Disks, drive: device.Drive, chosen: State<Text>) {
    if let health = drive.health {
        if health.failing {
            ui.ListRow(
                drive_name(d, drive),
                secondary: "{size_words(drive.size_bytes)} - {drive.name}",
                value: health_words(health),
                meta_tone: .danger,
                icon: .hard_drive,
                selected: chosen == drive.name,
            ) {
                on activate: set(chosen, drive.name)
            }
        } else if health.supported {
            ui.ListRow(
                drive_name(d, drive),
                secondary: "{size_words(drive.size_bytes)} - {drive.name}",
                value: health_words(health),
                meta_tone: .success,
                icon: .hard_drive,
                selected: chosen == drive.name,
            ) {
                on activate: set(chosen, drive.name)
            }
        } else {
            drive_row_of(d, drive, chosen, health_words(health))
        }
    } else {
        drive_row_of(d, drive, chosen, "")
    }
}

/// A drive's row with the reading it shows first and no tone: its health says nothing to tone.
view drive_row_of(d: Disks, drive: device.Drive, chosen: State<Text>, reading: Text) {
    ui.ListRow(
        drive_name(d, drive),
        secondary: "{size_words(drive.size_bytes)} - {drive.name}",
        value: reading,
        meta_tone: .neutral,
        icon: .hard_drive,
        selected: chosen == drive.name,
    ) {
        on activate: set(chosen, drive.name)
    }
}

/// The chosen drive: its map and facts, its health, or its partitions.
view drive_detail(
    d: Disks,
    disks: device.Disks,
    drive: device.Drive,
    tab: State<Text>,
    formatting: State<Text>,
) {
    ui.ObjectHeader(
        drive_name(d, drive),
        caption: "{drive.name}, {size_words(drive.size_bytes)}",
        icon: .hard_drive,
    )
    ui.ViewTabs(selected: tab_index(tab), label: "Drive") {
        ui.Segment("Overview")
        ui.Segment("Health")
        ui.Segment("Partitions")
        on change(index): set(tab, tab_key(index))
    }
    if tab == "health" {
        health_pane(drive)
    } else if tab == "partitions" {
        partitions_pane(d, drive, formatting)
    } else {
        overview_pane(d, disks, drive, formatting)
    }
}

fn tab_index(tab: Text) -> Int {
    if tab == "health" { 1 } else if tab == "partitions" { 2 } else { 0 }
}

fn tab_key(index: Int) -> Text {
    if index == 1 { "health" } else if index == 2 { "partitions" } else { "overview" }
}

/// The drive as one bar: its partitions by size, then the space no partition uses.
view overview_pane(d: Disks, disks: device.Disks, drive: device.Drive, formatting: State<Text>) {
    let free = drive.size_bytes - allocated(drive, 0)
    ui.Section("How it is divided") {
        if drive.size_bytes > 0 {
            row(gap: .none) {
                for partition in drive.partitions {
                    shape(
                        partition.size_bytes * 1000 / drive.size_bytes,
                        partition.mountpoint != .none,
                    )
                }
                if free > 0 {
                    shape(free * 1000 / drive.size_bytes, false)
                }
            }
        }
        ui.PropertyList {
            for partition in drive.partitions {
                ui.Property("{partition.name} {label_of(partition)}", partition_words(partition))
            }
            ui.Property("Unallocated", size_words(free))
        }
    }
    if let benchmark = disks.benchmark {
        if benchmark.device == drive.name {
            ui.Banner(
                "The drive read {rate_words(benchmark.read_per_sec)}",
                message: "{benchmark.reads_per_sec.to_int()} random reads a second",
                kind: .info,
                icon: .gauge,
            )
        }
    }
    row(gap: .tight, wrap: true) {
        // Where the drive is open: a filesystem written on the whole drive is the drive's own
        // `mountpoint`, and one on a partition is that partition's, which is the one to close.
        let open_part = drive.partitions.find(part -> part.mountpoint != .none)
        if let part = open_part {
            if let mounted = part.mountpoint {
                ui.Button("Close", icon: .log_out) {
                    on press: device.unmount(device.disks, device: part.name)
                }
                ui.Text("Open at {mounted}", role: .caption, secondary: true)
            }
        } else if let mounted = drive.mountpoint {
            ui.Button("Close", icon: .log_out) {
                on press: device.unmount(device.disks, device: drive.name)
            }
            ui.Text("Open at {mounted}", role: .caption, secondary: true)
        } else if not drive.partitions.is_empty() {
            ui.Button("Open the first partition", icon: .folder_open, role: .primary) {
                on press: mount_first(drive)
            }
        } else {
            ui.Button("Open", icon: .folder_open, role: .primary) {
                on press: device.mount(device.disks, device: drive.name)
            }
        }
        ui.Button("Measure", icon: .gauge) {
            on press: device.benchmark(device.disks, device: drive.name)
        }
        if drive.removable {
            ui.Button("Eject", icon: .log_out, role: .destructive) {
                on press: device.eject(device.disks, device: drive.name)
            }
        }
        ui.Button("Erase…", icon: .trash, role: .destructive) {
            on press: set(formatting, drive.name)
        }
    }
}

/// Mounts the drive's first partition, which is what a drive without a filesystem of its own
/// holds.
action mount_first(drive: device.Drive) {
    if let first = drive.partitions.first() {
        device.mount(device.disks, device: first.name)
    }
}

/// A piece of the drive's bar: its share of a thousand, filled while it is open.
view shape(share: Int, mounted: Bool) {
    let text = if mounted { "open" } else { "" }
    ui.Drawing(
        [
            ui.Shape.rect(
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                fill: .some(if mounted { .accent } else { .track }),
            ),
        ],
        meaning: .decoration,
        label: text,
        width: share.to_float(),
        height: 12.0,
        fill,
    )
}

/// The drive's own account of its health.
view health_pane(drive: device.Drive) {
    if let health = drive.health {
        ui.Section("Health") {
            ui.PropertyList {
                ui.Property("Verdict", health_words(health))
                if not health.warnings.is_empty() {
                    ui.Property("Warnings", health.warnings.join(", "))
                }
                if let temperature = health.temperature_c {
                    ui.Property("Temperature", "{temperature} °C")
                }
                if let hours = health.power_on_hours {
                    ui.Property("Powered on", "{hours} hours")
                }
                if let cycles = health.power_cycles {
                    ui.Property("Power cycles", "{cycles}")
                }
                if let used = health.percent_used {
                    ui.Property("Life used", "{used}%")
                }
                if let spare = health.available_spare {
                    ui.Property("Spare capacity", "{spare}%")
                }
                if let written = health.bytes_written {
                    ui.Property("Written", size_words(written))
                }
                if let read = health.bytes_read {
                    ui.Property("Read", size_words(read))
                }
                if let errors = health.media_errors {
                    ui.Property("Media errors", "{errors}")
                }
                if let bad = health.bad_sectors {
                    ui.Property("Bad sectors", "{bad}")
                }
                if let self_test = health.self_test {
                    ui.Property("Last self-test", self_test)
                }
            }
        }
    } else {
        ui.EmptyState(
            "Its health is not reported",
            message: "This drive reports no health of its own.",
            icon: .activity,
        )
    }
}

/// Every partition with what it holds and what can be done to it.
view partitions_pane(d: Disks, drive: device.Drive, formatting: State<Text>) {
    if drive.partitions.is_empty() {
        ui.EmptyState(
            "No partitions",
            message: "This drive holds no partition table; open it as one filesystem or erase it.",
            icon: .hard_drive,
        )
    }
    stack(gap: .none) {
        for partition in drive.partitions {
            ui.ListRow(
                partition.name,
                secondary: partition_words(partition),
                value: if partition.mountpoint != .none {
                    std.say("Open")
                } else { std.say("Closed") },
                icon: if partition.mountpoint != .none { .folder_open } else { .hard_drive },
            ) {
                trailing: {
                    row(gap: .tight) {
                        if let mount = partition.mountpoint {
                            ui.Text(mount, role: .caption, secondary: true)
                            ui.Button("Close", size: .small) {
                                on press: device.unmount(device.disks, device: partition.name)
                            }
                        } else {
                            ui.Button("Open", size: .small, role: .primary) {
                                on press: device.mount(device.disks, device: partition.name)
                            }
                        }
                        ui.Button(
                            "Erase…",
                            icon: .trash,
                            icon_only: true,
                            role: .destructive,
                            size: .small,
                        ) {
                            on press: set(formatting, partition.name)
                        }
                    }
                }
            }
            if let used = partition.used_bytes {
                ui.ProgressBar(
                    value: used.to_float() / partition.size_bytes.to_float(),
                    label: "{size_words(used)} of {size_words(partition.size_bytes)}",
                    warning: 0.85,
                    danger: 0.95,
                )
            }
        }
    }
}

fn label_of(partition: device.Partition) -> Text {
    if let label = partition.label { label } else { "" }
}

fn partition_words(partition: device.Partition) -> Text {
    let kind = if let fstype = partition.fstype { fstype } else { std.say("no filesystem") }
    let label = if let label = partition.label { label } else { "" }
    let type_name = if let type_name = partition.part_type_name { type_name } else { "" }
    if label != "" {
        "{size_words(partition.size_bytes)}, {kind}, {label}"
    } else if type_name != "" {
        "{size_words(partition.size_bytes)}, {kind}, {type_name}"
    } else {
        "{size_words(partition.size_bytes)}, {kind}"
    }
}

/// Erases the device the confirmation named, and writes the chosen filesystem.
action format_device(device_name: Text, filesystem: Text) {
    device.format(device.disks, device: device_name, filesystem: filesystem)
}

/// The confirmation an erase asks for: what it erases, and the filesystem to write on it.
view format_dialog(device_name: State<Text>, filesystem: State<Text>) {
    if device_name != "" {
        ui.ModalDialog(
            title: "Erase {device_name}?",
            description: .some("Everything on it is lost, and cannot be brought back."),
            open: true,
            primary: "Erase",
            secondary: "Cancel",
            destructive: true,
        ) {
            body: {
                ui.Text("The drive is erased and a new filesystem written on it.")
                row(gap: .tight, wrap: true) {
                    for choice in filesystems() {
                        ui.Chip(choice, selected: filesystem == choice) {
                            on press: set(filesystem, choice)
                        }
                    }
                }
            }
            on act_primary: format_device(device_name, filesystem).then(set(device_name, ""))
            on act_secondary: set(device_name, "")
            on dismiss: set(device_name, "")
        }
    }
}
