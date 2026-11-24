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
