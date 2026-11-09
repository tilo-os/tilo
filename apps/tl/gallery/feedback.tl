/// How things are going: badges and banners, progress and gauges, stats, the facts of a thing,
/// a path, a toolbar, rows that open in place, and what is still loading.
type Feedback {
    title: Text
    /// Who is syncing the files.
    owner: Option<Principal>
}

default view feedback for Feedback

/// The feedback window: each kind in a section of its own page.
view feedback(f: Feedback) {
    state last: Text = ""
    std.page(f) {
        ui.Toolbar {
            ui.ToolbarButton("Bold", icon: .pencil, toggled: true) {
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
        ui.ObjectHeader("Release 4.2", caption: "Ships Friday", who: f.owner)
        ui.Banner(
            "Sync paused",
            message: "Files sync again when you are online.",
            kind: .warning,
            action: "Retry",
        ) {
            on act: set(last, "Retry")
        }
        ui.Section("Status") {
            row(gap: .related, wrap: true) {
                ui.Badge("Beta")
                ui.Badge(count: 12)
                ui.Badge(dot: true)
                ui.Badge("Done", tone: .success)
                ui.Icon(.circle_check, tone: .success)
                ui.SignalMeter(.good)
                ui.Spinner("Syncing")
            }
            ui.StatusLine("Syncing 3 files", status: .active, who: f.owner)
            ui.TextStack(
                "Quarterly report",
                secondary: "Shared with the team",
                tertiary: "Edited an hour ago",
            )
            ui.Facts {
                ui.Fact("12 pages", icon: .folder)
                ui.Fact("Due Friday", icon: .calendar)
                ui.Fact("3 comments")
            }
        }
        ui.Section("Progress") {
            ui.ProgressBar(0.62, label: "Uploading")
            ui.ProgressBar(label: "Preparing")
            progress(0.25)
            row(gap: .related, wrap: true) {
                ui.ProgressRing(0.4, readout: "40%")
                ui.ProgressRing(0.9, tone: .warning, size: .small)
            }
            ui.BarGauge(0.81, label: "Disk", readout: "81 of 100 GB", warning: 0.75, danger: 0.9)
        }
        ui.Section("Numbers") {
            ui.StatGroup {
                ui.StatTile("Open", "24", change: "+3", trend: .up)
                ui.StatTile("Closed", "112", unit: "this month", samples: [3.0, 5.0, 4.0, 8.0, 6.0])
                ui.StatTile("Late", "2", tone: .danger)
            }
            row(gap: .related) {
                ui.Sparkline(
                    [3.0, 5.0, 4.0, 8.0, 6.0, 9.0],
                    label: "Requests per second",
                    tone: .success,
                )
                ui.Sparkline(
                    [9.0, 6.0, 8.0, 4.0, 5.0, 3.0],
                    label: "Errors per minute",
                    smooth: true,
                    filled: false,
                )
            }
        }
        ui.Section("Drawing") {
            ui.Drawing(
                [
                    .rect(x: 0.0, y: 0.0, w: 1.0, h: 1.0, round: 0.1, fill: .track),
                    .path(
                        points: [0.05, 0.8, 0.25, 0.55, 0.45, 0.65, 0.65, 0.3, 0.95, 0.2],
                        smooth: true,
                        fill: .accent,
                        wash: true,
                    ),
                    .path(
                        points: [0.05, 0.8, 0.25, 0.55, 0.45, 0.65, 0.65, 0.3, 0.95, 0.2],
                        smooth: true,
                        stroke: .accent,
                    ),
                    .line(x1: 0.05, y1: 0.5, x2: 0.95, y2: 0.5, stroke: .muted, weight: .hairline),
                    .ellipse(cx: 0.65, cy: 0.3, rx: 0.03, ry: 0.06, fill: .accent),
                ],
                meaning: .chart,
                label: "Visitors this week, above the target since Thursday",
                width: 240.0,
                height: 120.0,
            )
        }
        ui.Section("Details") {
            ui.DisclosureRow("Build log", secondary: "Finished in 4 minutes", icon: .list) {
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
            ui.Skeleton(.text, lines: 3)
        }
    }
}

/// The feedback group, the person looking syncing the files.
scenario "feedback" {
    let g = root Gallery
    g.feedback.insert(Feedback(title: "Feedback", owner: invoker))
}
