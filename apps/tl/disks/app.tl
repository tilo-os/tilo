app so.tilo.disks "Disks" (
    icon: .hard_drive,
    category: .system,
    tagline: "Partitions, formats and mounts drives",
    keywords: ["disks", "drives", "partitions", "format", "mount", "smart", "laufwerke", "festplatten", "disk", "drive", "partition", "usb", "storage", "ssd", "hard drive", "disk utility", "gnome disks", "disk management", "partition manager", "kde partition manager", "speicher"],
    splash_budget: 250,
)

use so.tilo.device as device
use so.tilo.session as session

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

/// What the drive plugs in by, or that it reports no transport.
fn interface_words(drive: device.Drive) -> Text {
    if let transport = drive.transport {
        if transport == "nvme" {
            "NVMe"
        } else if transport == "sata" {
            "SATA"
        } else if transport == "usb" { "USB" } else { transport }
    } else { std.say("Not reported") }
}

/// What kind of drive it is, as it is written on its box.
fn kind_words(drive: device.Drive) -> Text {
    if drive.kind != "disk" or drive.name.starts_with("zram") {
        std.say("Virtual block device")
    } else if let transport = drive.transport {
        if transport == "nvme" {
            std.say("NVMe solid state drive")
        } else if transport == "usb" {
            std.say("Removable USB drive")
        } else if drive.rotational {
            std.say("Hard disk drive")
        } else { std.say("Solid state drive") }
    } else if drive.rotational { std.say("Hard disk drive") } else { std.say("Block drive") }
}

/// Where a partition is mounted, as a person reads it: the kernel's `[SWAP]` marker is "Swap".
fn mounted_words(part: device.Partition) -> Text {
    let mount = part.mountpoint ?? ""
    if mount == "[swap]" {
        std.say("Swap")
    } else if mount == "" { std.say("Not mounted") } else { mount }
}

/// What a partition is for, when it serves something a person knows by name.
fn purpose_words(part: device.Partition) -> Text {
    let mount = part.mountpoint ?? ""
    let fstype = part.fstype ?? ""
    if fstype == "swap" or fstype == "linux-swap" or mount == "[swap]" {
        std.say("Swap space")
    } else if mount == "/" {
        std.say("Running system")
    } else if mount == "/home" {
        std.say("Home folders")
    } else if mount == "/boot" or mount.starts_with("/boot/") {
        std.say("Starts this device")
    } else { "" }
}

/// Whether erasing or closing this partition would take the running system with it.
fn serves_system(part: device.Partition) -> Bool {
    let mount = part.mountpoint ?? ""
    mount == "/" or mount == "/boot" or mount.starts_with("/boot/")
}

/// Why erasing `target` (the whole drive when empty) would destroy the running system; empty
/// when it would not. Closing what holds the system is refused for the same reason.
fn system_use(drive: device.Drive, target: Text) -> Text {
    if target == "" {
        if drive.partitions.any(part -> serves_system(part) or (part.mountpoint ?? "") == "/home") {
            std.say("Holds the running system")
        } else { "" }
    } else if let part = drive.partitions.find(item -> item.name == target) {
        if (part.mountpoint ?? "") == "/home" {
            std.say("Holds the home folders")
        } else if serves_system(part) { std.say("Holds the running system") } else { "" }
    } else { "" }
}

/// Why erasing `target` (the whole drive when empty) is refused; empty when it is not. What
/// holds the running system stays, and so does a partition's own encryption.
fn erase_reason(drive: device.Drive, target: Text) -> Text {
    let refused = system_use(drive, target)
    if refused != "" {
        refused
    } else if target == "" {
        ""
    } else if let part = drive.partitions.find(item -> item.name == target) {
        if part.encryption != .none {
            std.say("Formatting would remove its encryption")
        } else { "" }
    } else { "" }
}

/// What a person calls a partition: its label, else its type, else its filesystem, else its
/// name in `/dev`.
fn part_name(part: device.Partition) -> Text {
    let label = part.label ?? ""
    let kind = part.part_type_name ?? ""
    let fstype = part.fstype ?? ""
    if label != "" {
        label
    } else if kind != "" { kind } else if fstype != "" { fstype } else { part.name }
}

/// What a mounted partition holds, as its row reads it.
fn filesystem_line(part: device.Partition) -> Text {
    let mount = part.mountpoint ?? ""
    let fstype = part.fstype ?? ""
    if mount == "[swap]" {
        std.say("Swap space")
    } else if fstype != "" { std.say("{fstype} at {mount}") } else { std.say("Mounted at {mount}") }
}

/// How much of a partition is filled, 0 where the drive does not say.
fn filled(part: device.Partition) -> Float {
    if let used = part.used_bytes {
        if part.size_bytes > 0 {
            used.to_float() / part.size_bytes.to_float()
        } else { 0.0 }
    } else { 0.0 }
}

/// What is left on a partition, where the drive says.
fn room_left(part: device.Partition) -> Text {
    if let used = part.used_bytes {
        "{size_words(part.size_bytes - used)} free"
    } else { "" }
}

/// The chosen partition, when the person chose one rather than the whole drive.
fn chosen_partition(drive: device.Drive, chosen: Text) -> Option<device.Partition> {
    drive.partitions.find(part -> part.name == chosen)
}

/// What an erase erases, in the words its confirmation asks with.
fn format_loses(drive: device.Drive, target: Text) -> Text {
    if target == "" {
        let count = drive.partitions.len()
        if count == 0 {
            std.say("Everything on it is erased for good.")
        } else if count == 1 {
            std.say("Its partition and every file on it are erased for good.")
        } else {
            std.say("Its {count} partitions and every file on them are erased for good.")
        }
    } else if let part = drive.partitions.find(item -> item.name == target) {
        let used = part.used_bytes ?? 0
        let label = part.label ?? ""
        if used > 0 and label != "" {
            std.say("The {size_words(used)} of files on “{label}” are erased for good.")
        } else if used > 0 {
            std.say("The {size_words(used)} of files on it are erased for good.")
        } else if label != "" {
            std.say("Every file on “{label}” is erased for good.")
        } else {
            std.say("Every file on it is erased for good.")
        }
    } else {
        std.say("Everything on it is erased for good.")
    }
}

/// A drive's name in the words of the person reading it, by its serial.
action name_drive(d: Disks, serial: Text, name: Text) (
    icon: .pencil,
    label: "Name drive",
    did: "Named a drive",
) {
    require name.trim() == name or name.trim() == "" "A drive's name is not blank and not padded"
    let named = name.trim()
    if named == "" { d.names.remove(serial) } else { d.names.put(serial, named) }
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
    state chosen_part: Text = ""
    let disks = device.disks
    let drive = disks.drives.find(item -> item.name == chosen)
    ui.NavShell(content_kind: .page, title: "Disks", sidebar_title: "Drives") {
        sidebar: {
            stack {
                if let error = disks.read_error {
                    ui.StatusLine(error, status: .error)
                } else if not disks.serving {
                    ui.Text("Loading", role: .caption, secondary: true)
                }
                if disks.drives.is_empty() and disks.serving {
                    ui.EmptyState(
                        "No drives on this device",
                        message: "Attach a drive, then rescan.",
                        icon: .hard_drive,
                        inline: true,
                    )
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
            drive_detail(d, disks, drive, tab, formatting, chosen_part)
        } else {
            ui.EmptyState(
                "Choose a drive",
                message: "Pick a drive on the left to see its map, its health and its partitions.",
                icon: .hard_drive,
            )
        }
    }
    format_dialog(d, formatting, filesystem)
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
    chosen_part: State<Text>,
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
        overview_pane(d, disks, drive, formatting, chosen_part)
    }
}

fn tab_index(tab: Text) -> Int {
    if tab == "health" { 1 } else if tab == "partitions" { 2 } else { 0 }
}

fn tab_key(index: Int) -> Text {
    if index == 1 { "health" } else if index == 2 { "partitions" } else { "overview" }
}

/// The drive as one bar: its partitions by size, then the space no partition uses.
view overview_pane(
    d: Disks,
    disks: device.Disks,
    drive: device.Drive,
    formatting: State<Text>,
    chosen_part: State<Text>,
) {
    let free = drive.size_bytes - allocated(drive, 0)
    ui.Section("How it is divided") {
        if drive.size_bytes > 0 {
            row(gap: .none) {
                for partition in drive.partitions {
                    partition_block(
                        partition,
                        partition.size_bytes * 1000 / drive.size_bytes,
                        chosen_part,
                    )
                }
                if free > 0 {
                    free_block(free * 1000 / drive.size_bytes)
                }
            }
            // The legend names each piece of the bar; choosing one shows what it is, and the
            // whole drive is one choice of its own, so it can be erased as one.
            row(gap: .tight, wrap: true) {
                ui.Chip("The whole drive", selected: chosen_part == "") {
                    on press: set(chosen_part, "")
                }
                for partition in drive.partitions {
                    ui.Chip(
                        "{partition.name} · {size_words(partition.size_bytes)}",
                        selected: chosen_part == partition.name,
                    ) {
                        on press: set(chosen_part, partition.name)
                    }
                }
                ui.Chip("Unallocated · {size_words(free)}")
            }
            if let part = drive.partitions.find(item -> item.name == chosen_part) {
                ui.Text(partition_words(part), role: .caption, secondary: true)
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
    mounted_filesystems(drive, chosen_part)
    row(gap: .tight, wrap: true) {
        // Where the drive is open: a filesystem written on the whole drive is the drive's own
        // `mountpoint`, and one on a partition is that partition's, which is the one to close.
        let open_part = drive.partitions.find(part -> part.mountpoint != .none)
        if let part = open_part {
            let refused = system_use(drive, part.name)
            ui.Button("Close", icon: .log_out, enabled: refused == "", description: refused) {
                on press: device.unmount(device.disks, device: part.name)
            }
            if let mounted = part.mountpoint {
                ui.Text("Open at {mounted}", role: .caption, secondary: true)
                ways_on(d, mounted)
            }
        } else if let mounted = drive.mountpoint {
            ui.Button("Close", icon: .log_out) {
                on press: device.unmount(device.disks, device: drive.name)
            }
            ui.Text("Open at {mounted}", role: .caption, secondary: true)
            ways_on(d, mounted)
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
        let refused = erase_reason(drive, chosen_part)
        ui.Button(
            "Erase…",
            icon: .trash,
            role: .destructive,
            enabled: refused == "",
            description: refused,
        ) {
            on press: set(formatting, if chosen_part == "" { drive.name } else { chosen_part })
        }
    }
    if let part = chosen_partition(drive, chosen_part) {
        partition_details(part)
    } else {
        drive_details(drive)
    }
}

/// What the drive is: the facts a person checks before using it.
view drive_details(drive: device.Drive) {
    ui.Section("Details") {
        ui.PropertyList {
            ui.Property("Device", "/dev/{drive.name}", copyable: true)
            ui.Property("Type", kind_words(drive))
            ui.Property("Capacity", size_words(drive.size_bytes))
            ui.Property("Interface", interface_words(drive))
            if let serial = drive.serial {
                ui.Property("Serial number", serial, copyable: true)
            }
            if let fstype = drive.fstype {
                ui.Property("Filesystem", fstype)
            }
            if let mounted = drive.mountpoint {
                ui.Property("Mounted at", mounted)
            }
        }
    }
}

/// What one partition is: what it holds, where it is mounted, and what it is for.
view partition_details(part: device.Partition) {
    ui.Section("Details") {
        ui.PropertyList {
            ui.Property("Device", "/dev/{part.name}", copyable: true)
            ui.Property("Size", size_words(part.size_bytes))
            if let kind = part.part_type_name {
                ui.Property("Type", kind)
            }
            if let fstype = part.fstype {
                ui.Property("Filesystem", fstype)
            }
            if let encryption = part.encryption {
                if let unlocked = encryption.unlocked_as {
                    ui.Property("Encryption", "{encryption.scheme}, unlocked")
                } else {
                    ui.Property("Encryption", "{encryption.scheme}, locked")
                }
            }
            ui.Property("Mounted at", mounted_words(part))
            if let used = part.used_bytes {
                ui.Property("Used", size_words(used))
                ui.Property("Free", size_words(part.size_bytes - used))
            }
            if let purpose = purpose(part) {
                ui.Property("Used for", purpose)
            }
        }
    }
}

/// The filesystems mounted from the drive, each with what it holds and what is left, and the
/// sentence a drive with none reads.
view mounted_filesystems(drive: device.Drive, chosen_part: State<Text>) {
    let mounted = drive.partitions.filter(part -> part.mountpoint != .none)
    ui.Section("Filesystems") {
        if mounted.is_empty() {
            ui.Text("No filesystems are mounted.", role: .body, secondary: true)
        }
        for part in mounted {
            ui.ListRow(
                part_name(part),
                secondary: filesystem_line(part),
                meta: room_left(part),
                meta_tone: if filled(part) >= 0.9 {
                    if filled(part) >= 0.97 { .danger } else { .warning }
                } else { .neutral },
                icon: .layers,
                selected: chosen_part == part.name,
            ) {
                on activate: set(chosen_part, part.name)
            }
        }
    }
}

/// What a partition is for, where it serves something a person knows by name.
fn purpose(part: device.Partition) -> Option<Text> {
    let said = purpose_words(part)
    if said == "" { .none } else { .some(said) }
}

/// Mounts the drive's first partition, which is what a drive without a filesystem of its own
/// holds.
action mount_first(drive: device.Drive) {
    if let first = drive.partitions.first() {
        device.mount(device.disks, device: first.name)
    }
}

/// One partition's piece of the drive's bar. It is a control: pressing it shows what the
/// partition is, and assistive technology reads the partition's name and facts.
view partition_block(partition: device.Partition, share: Int, chosen_part: State<Text>) {
    let open = partition.mountpoint != .none
    ui.Drawing(
        [
            ui.Shape.rect(
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                fill: .some(if open { .accent } else { .track }),
            ),
        ],
        meaning: .picture,
        label: "{partition.name}: {partition_words(partition)}",
        width: share.to_float(),
        height: 12.0,
        fill,
    ) {
        on press: set(chosen_part, partition.name)
    }
}

/// The share of the bar no partition uses.
view free_block(share: Int) {
    ui.Drawing(
        [ui.Shape.rect(x: 0.0, y: 0.0, w: 1.0, h: 1.0, fill: .some(.track))],
        meaning: .decoration,
        label: "",
        width: share.to_float(),
        height: 12.0,
        fill,
    )
}

/// The two ways on into a mounted folder: Files opens it, Disk Usage measures it.
view ways_on(d: Disks, path: Text) {
    ui.Button("Open in Files", icon: .folder_open) {
        on press: open_in_files(d, path)
    }
    ui.Button("Disk Usage", icon: .gauge) {
        on press: open_usage(d)
    }
}

/// Opens a mounted folder in Files.
action open_in_files(d: Disks, path: Text) uses files (icon: .folder_open, label: "Open in Files") {
    File.open(path)
}

/// Opens Disk Usage, where a folder's use is measured.
action open_usage(d: Disks) (icon: .gauge, label: "Open Disk Usage") {
    session.open_app(app: "so.tilo.disk-usage")
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
            let locked = locked_partition(partition)
            ui.ListRow(
                partition.name,
                secondary: partition_words(partition),
                value: if locked {
                    std.say("Locked")
                } else if partition.mountpoint != .none {
                    std.say("Open")
                } else { std.say("Closed") },
                icon: if locked {
                    .lock
                } else if partition.mountpoint != .none { .folder_open } else { .hard_drive },
            ) {
                trailing: {
                    row(gap: .tight, wrap: true) {
                        let refused = system_use(drive, partition.name)
                        let cant_erase = erase_reason(drive, partition.name)
                        if let mount = partition.mountpoint {
                            ui.Text(mount, role: .caption, secondary: true)
                            ui.Button(
                                "Close",
                                size: .small,
                                enabled: refused == "",
                                description: refused,
                            ) {
                                on press: device.unmount(device.disks, device: partition.name)
                            }
                            ways_on(d, mount)
                        } else if locked {
                            if let scheme = encryption_words(partition) {
                                ui.Badge(scheme, tone: .warning)
                            }
                            ui.Text("Locked; unlock it first", role: .caption, secondary: true)
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
                            enabled: cant_erase == "",
                            description: cant_erase,
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
    let kind = if locked_partition(partition) {
        std.say("nothing inside")
    } else if let fstype = partition.fstype { fstype } else { std.say("no filesystem") }
    let label = if let label = partition.label { label } else { "" }
    let type_name = if let type_name = partition.part_type_name { type_name } else { "" }
    let scheme = if let encryption = encryption_words(partition) {
        std.say(", {encryption}")
    } else { "" }
    let held = if label != "" {
        "{kind}, {label}"
    } else if type_name != "" { "{kind}, {type_name}" } else { kind }
    "{size_words(partition.size_bytes)}, {held}{scheme}"
}

/// How a partition is encrypted: its scheme, and whether it is open.
fn encryption_words(partition: device.Partition) -> Option<Text> {
    if let encryption = partition.encryption {
        if encryption.unlocked_as == .none {
            .some(std.say("{encryption.scheme}, locked"))
        } else { .some(encryption.scheme) }
    } else { .none }
}

/// Whether the partition is encrypted and not unlocked: its filesystem is inside the mapping,
/// which this device has not opened.
fn locked_partition(partition: device.Partition) -> Bool {
    if let encryption = partition.encryption { encryption.unlocked_as == .none } else { false }
}

/// Erases the device the confirmation named, and writes the chosen filesystem.
action format_device(device_name: Text, filesystem: Text) {
    device.format(device.disks, device: device_name, filesystem: filesystem)
}

/// The confirmation an erase asks for: what it erases, and the filesystem to write on it.
/// The confirmation an erase asks for: what it erases, and the filesystem to write on it.
view format_dialog(d: Disks, target: State<Text>, filesystem: State<Text>) {
    if target != "" {
        let drive = device.disks.drives.find(
            dr -> dr.name == target or dr.partitions.any(p -> p.name == target),
        )
        if let drive = drive {
            let whole = target == drive.name
            ui.ModalDialog(
                title: "Erase {erase_name(d, drive, target)}?",
                description: .some(format_loses(drive, if whole { "" } else { target })),
                open: true,
                primary: "Erase",
                secondary: "Cancel",
                destructive: true,
            ) {
                body: {
                    ui.Text("It is erased and a new {filesystem} filesystem is written on it.")
                    row(gap: .tight, wrap: true) {
                        for choice in filesystems() {
                            ui.Chip(choice, selected: filesystem == choice) {
                                on press: set(filesystem, choice)
                            }
                        }
                    }
                }
                on act_primary: format_device(target, filesystem).then(set(target, ""))
                on act_secondary: set(target, "")
                on dismiss: set(target, "")
            }
        }
    }
}

/// What a person calls the thing an erase erases: the drive by its name, a partition by its own.
fn erase_name(d: Disks, drive: device.Drive, target: Text) -> Text {
    if target == drive.name {
        drive_name(d, drive)
    } else if let part = drive.partitions.find(item -> item.name == target) {
        part_name(part)
    } else { target }
}
