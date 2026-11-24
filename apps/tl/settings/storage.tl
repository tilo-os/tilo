/// What this computer's disk holds: how full it is, what each kind takes, the largest items and
/// the cache, read from the storage resource and changed by its own actions.
view storage_pane() {
    let storage = device.storage
    ui.Section("This device") {
        if let disk = storage.disk {
            let used = disk.total_bytes - disk.free_bytes
            ui.Drawing(
                disk_shapes(disk),
                meaning: .progress,
                value: .some(used.to_float() / disk.total_bytes.max(1).to_float()),
                label: .some("{size_words(used)} of {size_words(disk.total_bytes)} used"),
                height: .some(28.0),
            )
            ui.PropertyList {
                ui.Property("Used", size_words(used))
                ui.Property("Free", size_words(disk.free_bytes))
                ui.Property("Total", size_words(disk.total_bytes))
            }
        } else {
            ui.Text(
                "The disk could not be measured on this computer.",
                role: .caption,
                secondary: true,
            )
        }
        ui.Button("Read it again", icon: .rotate_cw) {
            on press: device.scan(device.storage)
        }
    }
    ui.Section("Where workspace files are kept") {
        ui.Text(
            "What this device holds of each workspace it mounts: the files only here are lost if this device is wiped, the others are on another device or download again.",
            role: .caption,
            secondary: true,
        )
        if let error = storage.residency_error {
            ui.StatusLine(error, status: .warning)
        }
        if storage.residency.is_empty() {
            ui.Text("This device holds no workspace's files.", role: .caption, secondary: true)
        }
        for held in storage.residency {
            ui.ListRow(held.workspace, secondary: held_words(held), icon: .layers) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        if held.only_here_bytes > 0 {
                            ui.Badge("{size_words(held.only_here_bytes)} only here", tone: .warning)
                        }
                        if held.kept_here {
                            ui.Badge("Kept here")
                        }
                        ui.MenuButton(
                            "More for {held.workspace}",
                            icon: .more_horizontal,
                            icon_only: true,
                        ) {
                            if held.cache_bytes > 0 {
                                ui.MenuItem(
                                    "Free {size_words(held.cache_bytes)}",
                                    icon: .trash,
                                    destructive: true,
                                ) {
                                    on choose: device.free_workspace_cache(
                                        device.storage,
                                        held.workspace,
                                    )
                                }
                            }
                            ui.MenuItem(
                                if held.kept_here { "Stop keeping here" } else { "Keep here" },
                            ) {
                                on choose: device.keep_here(
                                    device.storage,
                                    held.workspace,
                                    not held.kept_here,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
    ui.Section("Fetched workspace files") {
        ui.Text(
            "Files fetched from another device, kept here so they open at once.",
            role: .caption,
            secondary: true,
        )
        if let policy = storage.cache_policy {
            setting(
                "Prefetch files",
                "Download the files you are likely to open next before you open them",
            ) {
                ui.Toggle(shown: policy.prefetch, label: "Prefetch files") {
                    on check(on): device.set_prefetch(device.storage, on)
                }
            }
            setting(
                "Space for fetched files",
                "How much of this disk fetched files may take",
            ) {
                ui.Dropdown(
                    value: cache_size_label(policy.budget_bytes),
                    options: cache_sizes(),
                    caption: "Space for fetched files",
                ) {
                    on change(size): device.set_cache_size(device.storage, size: size)
                }
            }
            ui.Text(
                "Prefetching pauses on metered networks, low battery and a full disk. Files kept offline and files only on this device never count against this space.",
                role: .caption,
                secondary: true,
            )
        } else if let error = storage.sync_error {
            ui.StatusLine(error, status: .warning)
        }
        if not storage.reclaimable.is_empty() {
            ui.ListRow(
                "Free fetched files",
                secondary: "{size_words(storage.reclaimable_bytes)} in {storage.reclaimable.len()} files can be freed; they download again when opened",
                icon: .trash,
            ) {
                trailing: {
                    ui.Button(
                        "Free {size_words(storage.reclaimable_bytes)}",
                        role: .destructive,
                        size: .small,
                    ) {
                        on press: device.free_synced_files(device.storage)
                    }
                }
            }
            for file in storage.reclaimable {
                ui.ListRow(
                    file.path,
                    secondary: "{file.workspace} - {size_words(file.bytes)}",
                    icon: .file,
                ) {
                    trailing: {
                        ui.Button("Free", size: .small) {
                            on press: device.free_file(
                                device.storage,
                                workspace: file.workspace,
                                path: file.path,
                            )
                        }
                    }
                }
            }
        }
    }
    ui.Section("Your other devices") {
        let others = device.devices.devices.filter(d -> not d.local)
        if others.is_empty() {
            ui.Text("None of your other devices is here yet.", role: .caption, secondary: true)
        }
        for other in others {
            ui.ListRow(
                other.label,
                secondary: "{other_holds(other)} - {seen_words(other.seen_ms)}",
                icon: .monitor,
            )
        }
    }
    ui.Section("Kinds") {
        if storage.categories.is_empty() {
            ui.Text("No scan has answered yet.", role: .caption, secondary: true)
        } else {
            ui.Drawing(
                kind_shapes(storage.categories),
                meaning: .chart,
                label: .some(kinds_label(storage.categories)),
                height: .some(30.0 + 26.0 * storage.categories.take(6).len().to_float()),
            )
        }
        for kind in storage.categories {
            ui.ListRow(
                kind.category,
                secondary: "{size_words(kind.bytes)} in {kind.files} files",
                icon: .database,
            )
        }
    }
    ui.Section("Cache") {
        ui.ListRow(
            "Apps' cache",
            secondary: if storage.cache_bytes > 0 {
                "{size_words(storage.cache_bytes)} can be freed; apps download it again when needed"
            } else { "Nothing to free" },
            icon: .trash,
        ) {
            trailing: {
                ui.Button("Clear the cache", role: .destructive, size: .small) {
                    on press: device.clear_cache(device.storage)
                }
            }
        }
    }
    ui.Section("Largest items") {
        if storage.largest.is_empty() {
            ui.Text("Nothing large was found.", role: .caption, secondary: true)
        }
        for item in storage.largest {
            ui.ListRow(
                item.owner,
                secondary: "{item.category} - {size_words(item.bytes)}",
                icon: .file,
            )
        }
    }
    if let error = storage.read_error {
        ui.StatusLine(error, status: .warning)
    }
    if let problem = storage.problem {
        ui.StatusLine(problem, status: .error)
    }
}

/// What this device holds of a workspace, in words: which devices hold it, then each part of it
/// that is here, by what losing this device would cost.
fn held_words(held: device.WorkspaceHold) -> Text {
    let parts: List<Text> = []
    if held.pinned_bytes > 0 {
        parts = parts
            + [std.say("Kept here, also on another device: {size_words(held.pinned_bytes)}")]
    }
    if held.replicated_bytes > 0 {
        parts = parts + [std.say("Also on another device: {size_words(held.replicated_bytes)}")]
    }
    if held.only_here_bytes > 0 {
        parts = parts
            + [
                std.say(
                    "On no other device: {size_words(held.only_here_bytes)} in {held.only_here_files} files",
                ),
            ]
    }
    if held.cache_bytes > 0 {
        parts = parts + [std.say("Fetched files, safe to free: {size_words(held.cache_bytes)}")]
    }
    if parts.is_empty() {
        std.say("Kept on {keepers(held)} - nothing of it is stored here.")
    } else {
        std.say("Kept on {keepers(held)} - {parts.join(" - ")}")
    }
}

/// Which of the person's devices hold a workspace: this one when it holds any of it, and every
/// other device that last said it does.
fn keepers(held: device.WorkspaceHold) -> Text {
    let out: List<Text> = []
    if holds_any(held) {
        out = out + [std.say("this device")]
    }
    for other in device.devices.devices {
        if not other.local
            and other.storage.any(h -> h.workspace == held.workspace and holds_any(h)) {
            out = out + [other.label]
        }
    }
    if out.is_empty() { std.say("no device this one knows of") } else { out.join(", ") }
}

/// Whether any byte of a workspace is on the device that said so.
fn holds_any(held: device.WorkspaceHold) -> Bool {
    held.pinned_bytes + held.replicated_bytes + held.only_here_bytes + held.cache_bytes > 0
}

/// What one of the person's other devices last said it holds, workspace by workspace.
fn other_holds(other: device.PersonalDeviceView) -> Text {
    let parts: List<Text> = []
    for held in other.storage {
        let bytes = held.pinned_bytes
            + held.replicated_bytes
            + held.only_here_bytes
            + held.cache_bytes
        if bytes > 0 {
            parts = parts + [std.say("{held.workspace}: {size_words(bytes)}")]
        }
    }
    if parts.is_empty() {
        std.say("It says it holds nothing of your workspaces")
    } else { std.say("It holds {parts.join(", ")}") }
}

/// When a device was last with a host, as a person reads it.
fn seen_words(seen_ms: Int) -> Text {
    if seen_ms <= 0 {
        std.say("never heard from")
    } else {
        let at = Time("1970-01-01T00:00:00Z") + Duration(seen_ms / 1000)
        std.say("last seen {at.day()}, {at.clock()}")
    }
}

/// The spaces fetched files may take, as the storage resource's action takes them.
fn cache_sizes() -> List<Text> {
    ["Automatic", "5 GB", "10 GB", "25 GB", "50 GB", "100 GB"]
}

/// The space fetched files may take, as the dropdown shows it: what the person chose, "Automatic"
/// for the daemon's own share of the disk, or the size in words when it is none of those.
fn cache_size_label(budget: Option<Int>) -> Text {
    if let bytes = budget {
        cache_sizes().find(s -> s != "Automatic" and size_bytes(s) == bytes) ?? size_words(bytes)
    } else {
        "Automatic"
    }
}

/// The bytes a space of the dropdown names: `10 GB` is ten thousand million.
fn size_bytes(size: Text) -> Int {
    (size.replace(" GB", "").to_int() ?? 0) * 1000000000
}

/// How many bytes read to a person: `4 GB`, `812 MB`.
fn size_words(n: Int) -> Text {
    if n < 1000 {
        "{n} B"
    } else if n < 1000000 {
        "{n / 1000} KB"
    } else if n < 1000000000 { "{n / 1000000} MB" } else { "{n / 1000000000} GB" }
}

/// The disk as one bar: what it holds filled, what is free left open.
fn disk_shapes(disk: device.DiskUsage) -> List<ui.Shape> {
    let total = disk.total_bytes.max(1)
    let used = (total - disk.free_bytes).clamp(0, total)
    [
        ui.Shape.rect(x: 0.0, y: 0.3, w: 1.0, h: 0.4, round: 0.5, fill: .some(.track)),
        ui.Shape.rect(
            x: 0.0,
            y: 0.3,
            w: used.to_float() / total.to_float(),
            h: 0.4,
            round: 0.5,
            fill: .some(.accent),
        ),
    ]
}

/// What each kind of the disk's contents takes, each as a bar against the largest kind.
fn kind_shapes(categories: List<device.CategoryUsage>) -> List<ui.Shape> {
    let shown = categories.take(6)
    let most = categories.map(c -> c.bytes).sort_by(x -> x).last() ?? 1
    let ys = shown.map((c, i) -> 0.1 + i.to_float() * 0.15)
    let labels = shown.map(
        (c, i) -> ui.Shape.text(
            x: 0.0,
            y: (ys.at(i) ?? 0.0) + 0.03,
            text: c.category,
            role: .caption,
            ink: .ink,
            anchor: .start,
        ),
    )
    let tracks = shown.map(
        (c, i) -> ui.Shape.rect(
            x: 0.42,
            y: ys.at(i) ?? 0.0,
            w: 0.52,
            h: 0.06,
            round: 0.5,
            fill: .some(.track),
        ),
    )
    let bars = shown.map(
        (c, i) -> ui.Shape.rect(
            x: 0.42,
            y: ys.at(i) ?? 0.0,
            w: 0.52 * c.bytes.to_float() / most.to_float(),
            h: 0.06,
            round: 0.5,
            fill: .some(.accent),
        ),
    )
    let sizes = shown.map(
        (c, i) -> ui.Shape.text(
            x: 0.96,
            y: (ys.at(i) ?? 0.0) + 0.03,
            text: size_words(c.bytes),
            role: .caption,
            ink: .muted,
            anchor: .end,
        ),
    )
    [labels, tracks, bars, sizes].flat_map(s -> s)
}

/// What the bars show, as assistive technology hears it.
fn kinds_label(categories: List<device.CategoryUsage>) -> Text {
    categories.take(6).map(c -> "{c.category}: {size_words(c.bytes)}").join("; ")
}
