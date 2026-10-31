/// How things are going: badges and banners, progress and gauges, stats, the facts of a thing,
/// a path, a toolbar, rows that open in place, and what is still loading.
type Feedback {
    title: Text (title)
    /// Who is syncing the files.
    owner: Option<Principal>
}

default view feedback for Feedback

/// The feedback window: each kind in a section of its own page.
view feedback(f: Feedback) {
    lens last: Text = ""
    std.page(f) {
        ui.Toolbar {
            ui.ToolbarButton("Bold", icon: .pencil, toggled: .some(true)) {
                on press: set(last, "Bold")
            }
            ui.ToolbarButton("Flag", icon: .flag, dot: true) {
                on press: set(last, "Flag")
            }
            ui.ToolbarSeparator()
            ui.ToolbarButton("Pin", icon: .pin) {
                on press: set(last, "Pin")
            }
            ui.ToolbarSpacer()
            ui.ToolbarButton("Share", icon: .upload) {
                on press: set(last, "Share")
            }
        }
        ui.Breadcrumb(root_icon: .folder) {
            ui.BreadcrumbSegment("Projects") {
                on activate: set(last, "Projects")
            }
            ui.BreadcrumbSegment("Release") {
                on activate: set(last, "Release")
            }
        }
        ui.ObjectHeader("Release 4.2", caption: .some("Ships Friday"), who: f.owner)
        ui.Banner("Sync paused", message: .some("Files sync again when you are online."), kind: .warning, action: .some("Retry")) {
            on act: set(last, "Retry")
        }
        ui.Section("Status") {
            row(gap: .related, wrap: true) {
                ui.Badge(.some("Beta"))
                ui.Badge(count: .some(12))
                ui.Badge(dot: true)
                ui.Badge(.some("Done"), tone: .success)
                ui.Icon(.circle_check, tone: .success)
                ui.SignalMeter(.good)
                ui.Spinner(.some("Syncing"))
            }
            ui.StatusLine("Syncing 3 files", status: .active, who: f.owner)
            ui.TextStack("Quarterly report", secondary: .some("Shared with the team"), tertiary: .some("Edited an hour ago"))
            ui.Facts {
                ui.Fact("12 pages", icon: .folder)
                ui.Fact("Due Friday", icon: .calendar)
                ui.Fact("3 comments")
            }
        }
        ui.Section("Progress") {
            ui.ProgressBar(.some(0.62), label: .some("Uploading"))
            ui.ProgressBar(label: .some("Preparing"))
            row(gap: .related, wrap: true) {
                ui.ProgressRing(0.4, readout: .some("40%"))
                ui.ProgressRing(0.9, tone: .warning, size: .small)
            }
            ui.BarGauge(0.81, label: .some("Disk"), readout: .some("81 of 100 GB"), warning: .some(0.75), danger: .some(0.9))
        }
        ui.Section("Numbers") {
            ui.StatGroup {
                ui.StatTile("Open", "24", change: .some("+3"), trend: .up)
                ui.StatTile("Closed", "112", unit: .some("this month"), samples: [3.0, 5.0, 4.0, 8.0, 6.0])
                ui.StatTile("Late", "2", tone: .danger)
            }
        }
        ui.Section("Details") {
            ui.DisclosureRow("Build log", secondary: .some("Finished in 4 minutes"), icon: .list) {
                ui.Text("Compiled 312 crates.")
                ui.Text("All tests passed.", role: .caption, secondary: true)
            }
            ui.DisclosureBand {
                ui.Text("Older activity is kept for 30 days.", role: .caption, secondary: true)
            }
        }
        ui.Section("Keys and loading") {
            ui.InputHint(["Ctrl", "K"])
            ui.KeyHints {
                ui.KeyHint(["Enter"], "Open")
                ui.KeyHint(["Esc"], "Close")
            }
            ui.Skeleton(.row, leading: .avatar)
            ui.Skeleton(.text, lines: .some(3))
        }
    }
}

/// The feedback group, the person looking syncing the files.
scenario "feedback" {
    let g = root Gallery
    g.feedback.insert(Feedback(title: "Feedback", owner: .some(invoker)))
}
