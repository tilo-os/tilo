app so.tilo.monitor "System Monitor" (
    icon: .activity,
    category: .system,
    tagline: "This computer's processes, and what it uses",
    keywords: ["monitor", "system monitor", "activity", "processes", "cpu", "memory", "task manager", "ressourcen", "prozesse"],
    splash_budget: 250,
)

use so.tilo.device as device

/// One row of the process list: the process, how deep it stands under its parent, and whether it
/// has children.
type Row {
    process: device.Process
    depth: Int = 0
    has_children: Bool = false
}

/// The app's root. The table's shape and where the person stands are the window's own, so
/// nothing is kept here.
type Monitor {
}

root monitor: Monitor

default view window for Monitor

/// How a process stands.
fn state_words(state: device.ProcessState) -> Text {
    if state == .running {
        std.say("Running")
    } else if state == .sleeping {
        std.say("Sleeping")
    } else if state == .waiting {
        std.say("Waiting on the disk")
    } else if state == .stopped {
        std.say("Stopped")
    } else if state == .zombie {
        std.say("Zombie")
    } else if state == .idle { std.say("Idle") } else { std.say("Dead") }
}

fn tab_index(tab: Text) -> Int {
    if tab == "resources" { 1 } else { 0 }
}

fn tab_key(index: Int) -> Text {
    if index == 1 { "resources" } else { "processes" }
}

/// Bytes in the words a person reads: `18.2 MiB`, `1.5 GiB`.
fn bytes_words(bytes: Int) -> Text {
    if bytes < 1024 {
        std.say("{bytes} B")
    } else if bytes < 1048576 {
        std.say("{bytes / 1024} KiB")
    } else if bytes < 1073741824 {
        std.say("{bytes / 1048576} MiB")
    } else { std.say("{bytes / 1073741824} GiB") }
}

/// Bytes a second in the words a person reads.
fn rate_words(bytes: Float) -> Text {
    if bytes < 1024.0 {
        std.say("{bytes.to_int()} B/s")
    } else if bytes < 1048576.0 {
        std.say("{(bytes / 1024.0).to_int()} KiB/s")
    } else if bytes < 1073741824.0 {
        std.say("{(bytes / 1048576.0).to_int()} MiB/s")
    } else {
        std.say("{(bytes / 1073741824.0).to_int()} GiB/s")
    }
}

/// A percentage, whole.
fn percent_words(percent_value: Float) -> Text {
    std.say("{percent_value.to_int()}%")
}

/// A time span in the words a person reads: `3d 4h 12m`.
fn uptime_words(seconds: Int) -> Text {
    let days = seconds / 86400
    let hours = (seconds - days * 86400) / 3600
    let minutes = (seconds - days * 86400 - hours * 3600) / 60
    if days > 0 {
        std.say("{days}d {hours}h {minutes}m")
    } else if hours > 0 { std.say("{hours}h {minutes}m") } else { std.say("{minutes}m") }
}

/// The indentation of a row at `depth`, two spaces a step.
fn indent(depth: Int) -> Text {
    if depth <= 0 { "" } else if depth == 1 { "  " } else { "  " + indent(depth - 1) }
}

/// `items` in reverse, one at a time.
fn reversed(items: List<device.Process>, at: Int) -> List<device.Process> {
    if at < 0 {
        []
    } else if let item = items.at(at) {
        [[item], reversed(items, at - 1)].flat_map(one -> one)
    } else { [] }
}

/// `items` in the order the chosen column asks, rising or falling.
fn sorted(items: List<device.Process>, key: Text, descending: Bool) -> List<device.Process> {
    let rising = if key == "name" {
        items.sort_by(process -> process.name.lower())
    } else if key == "user" {
        items.sort_by(process -> process.user.lower())
    } else if key == "pid" {
        items.sort_by(process -> process.pid)
    } else if key == "memory" {
        items.sort_by(process -> process.memory_bytes)
    } else if key == "threads" {
        items.sort_by(process -> process.threads)
    } else { items.sort_by(process -> process.cpu) }
    if descending { reversed(rising, rising.len() - 1) } else { rising }
}

/// Whether a process matches the words typed in the filter.
fn matches(process: device.Process, query: Text) -> Bool {
    let wanted = query.trim().lower()
    wanted == ""
        or process.name.lower().contains(wanted)
        or process.user.lower().contains(wanted)
        or "{process.pid}".contains(wanted)
}

/// The processes under `parent`, flattened depth first, each with how deep it stands.
fn tree_rows(
    all: List<device.Process>,
    parent: Int,
    depth: Int,
    key: Text,
    descending: Bool,
    kernel: Bool,
    query: Text,
) -> List<Row> {
    if depth > 32 {
        []
    } else {
        let shown = all.filter(
            process -> (process.ppid ?? 0) == parent
                and (kernel or not process.kernel_thread)
                and matches(process, query),
        )
        let children = sorted(shown, key, descending)
        children.flat_map(
            process -> [
                [
                    Row(
                        process: process,
                        depth: depth,
                        has_children: all.any(
                            child -> (child.ppid ?? 0) == process.pid
                                and (kernel or not child.kernel_thread),
                        ),
                    ),
                ],
                tree_rows(all, process.pid, depth + 1, key, descending, kernel, query),
            ]
                .flat_map(one -> one),
        )
    }
}

/// Every process as one flat list, filtered and sorted.
fn flat_rows(
    all: List<device.Process>,
    key: Text,
    descending: Bool,
    kernel: Bool,
    query: Text,
) -> List<Row> {
    sorted(
            all.filter(
                process -> (kernel or not process.kernel_thread) and matches(process, query),
            ),
            key,
            descending,
        )
        .map(process -> Row(process: process, depth: 0, has_children: false))
}

/// Asks every process of an app to close.
action end_app(application: Text) {
    device.end_app(device.processes, application: application)
}

/// The threads every process of `items` runs, counted one at a time.
fn thread_total(items: List<device.Process>, at: Int) -> Int {
    if at >= items.len() {
        0
    } else {
        (items.at(at)?.threads ?? 0) + thread_total(items, at + 1)
    }
}

/// The window: the process list with its inspector, or the machine's readings.
view window(m: Monitor) {
    state tab: Text = "processes"
    state chosen: Int = 0
    state sort: Text = "cpu"
    state descending: Bool = true
    state tree: Bool = true
    state kernel: Bool = false
    state filter: Text = ""
    let now = device.processes
    let machine = now.machine
    ui.NavShell(content_kind: .page, title: "System Monitor") {
        actions: {
            ui.MenuButton("Sort by", icon: .arrow_up_down) {
                ui.MenuItem("Processor", checked: sort == "cpu") {
                    on choose: set(sort, "cpu")
                }
                ui.MenuItem("Memory", checked: sort == "memory") {
                    on choose: set(sort, "memory")
                }
                ui.MenuItem("Name", checked: sort == "name") {
                    on choose: set(sort, "name")
                }
                ui.MenuItem("PID", checked: sort == "pid") {
                    on choose: set(sort, "pid")
                }
                ui.MenuItem("User", checked: sort == "user") {
                    on choose: set(sort, "user")
                }
                ui.MenuItem("Threads", checked: sort == "threads") {
                    on choose: set(sort, "threads")
                }
                ui.MenuSeparator
                ui.MenuItem(
                    if descending { "Falling" } else { "Rising" },
                    icon: if descending { .arrow_down } else { .arrow_up },
                ) {
                    on choose: set(descending, not descending)
                }
            }
            ui.MenuButton("Update frequency", icon: .clock) {
                ui.MenuItem("Every second", checked: now.interval_ms == 1000) {
                    on choose: device.set_interval(device.processes, ms: 1000)
                }
                ui.MenuItem("Every 1.5 seconds", checked: now.interval_ms == 1500) {
                    on choose: device.set_interval(device.processes, ms: 1500)
                }
                ui.MenuItem("Every 2 seconds", checked: now.interval_ms == 2000) {
                    on choose: device.set_interval(device.processes, ms: 2000)
                }
                ui.MenuItem("Every 3 seconds", checked: now.interval_ms == 3000) {
                    on choose: device.set_interval(device.processes, ms: 3000)
                }
                ui.MenuItem("Every 5 seconds", checked: now.interval_ms == 5000) {
                    on choose: device.set_interval(device.processes, ms: 5000)
                }
                ui.MenuSeparator
                ui.MenuItem("Paused", checked: now.paused) {
                    on choose: device.set_interval(device.processes, ms: 0)
                }
            }
            ui.Button("Flat list", icon: .list, toggled: not tree) {
                on press: set(tree, not tree)
            }
            ui.Button("Kernel threads", icon: .cpu, toggled: kernel) {
                on press: set(kernel, not kernel)
            }
        }
        stack {
            ui.ViewTabs(selected: tab_index(tab), label: "Monitor") {
                ui.Segment("Processes")
                ui.Segment("Resources")
                on change(index): set(tab, tab_key(index))
            }
            if not now.serving {
                engine_missing(now)
            } else if tab == "processes" {
                processes_pane(now, chosen, sort, descending, tree, kernel, filter)
            } else {
                resources_pane(now)
            }
        }
    }
}

/// What a computer whose system information service does not answer says.
view engine_missing(now: device.Processes) {
    ui.EmptyState(
        "Processes unavailable",
        message: if let error = now.read_error {
            error
        } else {
            "This computer's system information service is not answering."
        },
        icon: .activity,
        action: "Try again",
    ) {
        on act: device.refresh(device.processes)
    }
}

/// The process list: the four summary tiles, the filter, and the rows beside the inspector.
view processes_pane(
    now: device.Processes,
    chosen: State<Int>,
    sort: State<Text>,
    descending: State<Bool>,
    tree: State<Bool>,
    kernel: State<Bool>,
    filter: State<Text>,
) {
    let machine = now.machine
    ui.StatGroup {
        ui.StatTile(
            "CPU",
            "{machine.cpu.total.to_int()}",
            unit: "%",
            samples: machine.cpu.history,
        ) {
            on activate: set(chosen, 0)
        }
        ui.StatTile(
            "Memory in use",
            bytes_words(machine.memory.total - machine.memory.available),
            samples: machine.memory.history,
        )
        ui.StatTile(
            "Network",
            rate_words(machine.network.rx_per_sec + machine.network.tx_per_sec),
            samples: machine.network.rx_history,
        )
        ui.StatTile(
            "Disk",
            rate_words(machine.disk_read_history.last() ?? 0.0),
            samples: machine.disk_read_history,
        )
    }
    split(side: .trailing, collapse: .stack) {
        primary(min: 24rem) {
            row(gap: .tight, align: .center) {
                ui.TextInput(bind: filter, placeholder: "Filter processes", fill)
                ui.Text("{now.processes.len()} processes", role: .caption, secondary: true)
            }
            let rows = if tree {
                tree_rows(now.processes, 0, 0, sort, descending, kernel, filter)
            } else {
                flat_rows(now.processes, sort, descending, kernel, filter)
            }
            if rows.is_empty() {
                ui.EmptyState("No processes match the search", icon: .activity)
            } else {
                scroll {
                    stack(gap: .none) {
                        for line in rows {
                            process_row(line, chosen)
                        }
                    }
                }
            }
        }
        secondary(min: 18rem) {
            if let process = now.processes.find(item -> item.pid == chosen) {
                process_detail(process, now)
            } else {
                ui.EmptyState("No process selected", icon: .activity)
            }
        }
    }
}

/// One process in the list: its name and user, and what it uses.
view process_row(line: Row, chosen: State<Int>) {
    let process = line.process
    ui.ListRow(
        indent(line.depth) + if line.has_children { "▾ " } else { "" } + process.name,
        secondary: process.user,
        value: percent_words(process.cpu),
        meta: bytes_words(process.memory_bytes),
        dense: true,
        selected: chosen == process.pid,
    ) {
        trailing: {
            row(gap: .related) {
                ui.Text("{process.threads}", role: .caption, numeric: true, secondary: true)
                ui.Text("{process.pid}", role: .caption, numeric: true, secondary: true)
            }
        }
        on activate: set(chosen, process.pid)
    }
}

/// The chosen process: what it uses, what it is, and the verbs that end it.
view process_detail(process: device.Process, now: device.Processes) {
    ui.ObjectHeader(process.name, caption: process.user, icon: .activity)
    ui.StatGroup {
        ui.StatTile("CPU", "{process.cpu.to_int()}", unit: "%", samples: process.cpu_history)
        ui.StatTile("Memory (RSS)", bytes_words(process.memory_bytes))
    }
    ui.PropertyList {
        ui.Property("PID", "{process.pid}", copyable: true)
        ui.Property("User", process.user)
        ui.Property("Status", state_words(process.state))
        if let app_id = process.app_id {
            ui.Property("App", app_id)
        }
        if let parent = now.processes.find(item -> item.pid == (process.ppid ?? 0)) {
            ui.Property("Parent", parent.name)
        }
        ui.Property("Threads", "{process.threads}")
        ui.Property("Virtual memory", bytes_words(process.virtual_bytes))
        ui.Property("Running for", uptime_words(process.run_secs))
    }
    row(gap: .tight, wrap: true) {
        if process.endable {
            ui.Button("End process", icon: .square) {
                on press: device.end(
                    device.processes,
                    process: "{process.pid}:{process.started_at}",
                )
            }
            ui.Button("Kill process", icon: .x, role: .destructive) {
                on press: device.kill(
                    device.processes,
                    process: "{process.pid}:{process.started_at}",
                )
            }
        }
        if let app_id = process.app_id {
            ui.Button("End app", icon: .square, role: .destructive) {
                on press: end_app(app_id)
            }
        }
    }
}

/// The machine's readings: processor, memory, network, disk, and the facts of this computer.
view resources_pane(now: device.Processes) {
    let machine = now.machine
    ui.Section("Processor") {
        ui.Chart(
            [.series(values: machine.cpu.history, label: "Processor")],
            title: .some("Processor use"),
            max: .some(100.0),
            top_label: .some("100%"),
            bottom_label: .some("0%"),
        )
        grid(min: 8rem) {
            for core in machine.cpu.per_core {
                ui.BarGauge(core / 100.0, label: "Core", readout: percent_words(core))
            }
        }
    }
    ui.Section("Memory") {
        ui.BarGauge(
            if machine.memory.total > 0 {
                (machine.memory.total - machine.memory.available).to_float() / machine.memory.total.to_float()
            } else { 0.0 },
            label: "RAM",
            readout: "{bytes_words(machine.memory.total - machine.memory.available)} of {bytes_words(machine.memory.total)}",
            warning: 0.75,
            danger: 0.9,
        )
        ui.BarGauge(
            if machine.memory.swap_total > 0 {
                (machine.memory.swap_total - machine.memory.swap_free).to_float() / machine.memory.swap_total.to_float()
            } else { 0.0 },
            label: "Swap",
            readout: "{bytes_words(machine.memory.swap_total - machine.memory.swap_free)} of {bytes_words(machine.memory.swap_total)}",
            warning: 0.75,
            danger: 0.9,
        )
        ui.Chart(
            [.series(values: machine.memory.history, label: "In use")],
            title: .some("Memory in use"),
            max: .some(100.0),
            top_label: .some("100%"),
            bottom_label: .some("0%"),
        )
    }
    ui.Section("Network") {
        ui.Chart(
            [
                .series(values: machine.network.rx_history, label: "Received"),
                .series(values: machine.network.tx_history, label: "Sent"),
            ],
            title: .some("Network throughput"),
        )
        ui.PropertyList {
            ui.Property("Received", rate_words(machine.network.rx_per_sec))
            ui.Property("Sent", rate_words(machine.network.tx_per_sec))
            for interface in machine.network.interfaces {
                ui.Property(
                    interface.name,
                    "{rate_words(interface.rx_per_sec)} in, {rate_words(interface.tx_per_sec)} out",
                )
            }
        }
    }
    ui.Section("Disk") {
        ui.Chart(
            [
                .series(values: machine.disk_read_history, label: "Read"),
                .series(values: machine.disk_write_history, label: "Written"),
            ],
            title: .some("Disk throughput"),
        )
        ui.PropertyList {
            for disk in machine.disks {
                if let mount = disk.mount {
                    ui.Property(
                        disk.device,
                        "{bytes_words(disk.used_bytes)} of {bytes_words(disk.total_bytes)} at {mount}",
                    )
                } else {
                    ui.Property(
                        disk.device,
                        "{bytes_words(disk.used_bytes)} of {bytes_words(disk.total_bytes)}",
                    )
                }
            }
        }
    }
    if not machine.gpus.is_empty() {
        ui.Section("Graphics") {
            for gpu in machine.gpus {
                if let busy = gpu.busy_percent {
                    ui.PropertyList {
                        ui.Property(gpu.driver, percent_words(busy))
                    }
                } else {
                    ui.Text(gpu.driver, role: .caption, secondary: true)
                }
            }
        }
    }
    if not machine.batteries.is_empty() {
        ui.Section("Battery") {
            for battery in machine.batteries {
                ui.PropertyList {
                    if let percent = battery.percent {
                        ui.Property(
                            battery.name,
                            "{percent_words(percent)}, {battery.status.lower()}",
                        )
                    } else {
                        ui.Property(battery.name, battery.status.lower())
                    }
                }
            }
        }
    }
    if not machine.thermal.is_empty() {
        ui.Section("Temperature") {
            ui.PropertyList {
                for zone in machine.thermal {
                    ui.Property(zone.name, "{zone.celsius.to_int()} °C")
                }
            }
        }
    }
    ui.Section("This computer") {
        ui.PropertyList {
            ui.Property("Host name", machine.hostname)
            ui.Property("Kernel", machine.kernel)
            if let model = machine.cpu_model {
                ui.Property("Processor", model)
            }
            ui.Property("Cores", "{machine.logical_cpus}")
            ui.Property(
                "Load average",
                machine.load_average.map(load -> "{load.to_int()}").join(", "),
            )
            ui.Property("Processes", "{now.processes.len()}")
            ui.Property("Threads", "{thread_total(now.processes, 0)}")
            ui.Property("Uptime", uptime_words(machine.uptime_secs))
        }
    }
    if let pressure = machine.pressure {
        ui.Section("Pressure") {
            ui.PropertyList {
                ui.Property("Processor", "{percent_words(pressure.cpu.some_avg10)} some")
                ui.Property("Memory", "{percent_words(pressure.memory.some_avg10)} some")
                ui.Property("Disk", "{percent_words(pressure.io.some_avg10)} some")
            }
        }
    }
}
