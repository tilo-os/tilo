/// What this computer's disk holds: how full it is, what each kind takes, the largest items and
/// the cache, read from the storage resource and changed by its own actions.
view storage_pane() {
    let storage = device.storage
    ui.Section("This device") {
        if let disk = storage.disk {
            let used = disk.total_bytes - disk.free_bytes
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
