/// The feedback page: each kind in a section of its own.
view feedback(g: Gallery) {
    state last: Text = ""
    ui.Text(
        "Banners, badges, progress and numbers, the facts of a thing, and what is still loading.",
        role: .caption,
        secondary: true,
    )
    stack(max: 48rem, gap: .section) {
        part(
            "Toolbar",
            "A run of tools: one on, one with news, a separator and a share at the end.",
        ) {
            ui.Toolbar {
                ui.ToolbarButton("Bold", icon: .bold, toggled: true) {
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
        }
        part(
            "Breadcrumb and header",
            "Where a thing stands, then the thing: its name, a line and who has it.",
        ) {
            ui.Breadcrumb(root_icon: .folder) {
                ui.BreadcrumbSegment("Projects") {
                    on activate: set(last, "Projects")
                }
                ui.BreadcrumbSegment("Release") {
                    on activate: set(last, "Release")
                }
            }
            ui.ObjectHeader("Release 4.2", caption: "Ships Friday", who: invoker)
        }
        part(
            "Banner",
            "Something the person should know about the whole page, by its kind, with what to do.",
        ) {
            ui.Banner(
                "Sync paused",
                message: "Files sync again when you are online.",
                kind: .warning,
                action: "Retry",
            ) {
                on act: set(last, "Retry")
            }
            ui.Banner(
                "A new version is ready",
                message: "Restart to finish updating.",
                kind: .info,
            ) {
                on dismiss: set(last, "Dismissed")
            }
            ui.Banner("This report is read-only", kind: .neutral)
            ui.Banner(
                "Release 4.2 is out",
                message: "Everyone on the team has the new board.",
                kind: .success,
            )
            ui.Banner(
                "The export failed",
                message: "The disk is full. Free 2 GB and try again.",
                kind: .error,
                action: "Try again",
                secondary_action: "Details",
            ) {
                on act: set(last, "Try again")
                on secondary_act: set(last, "Details")
            }
        }
        part(
            "Badge and status",
            "Small marks that say how something is: a label, a count, a dot, a tone.",
        ) {
            row(gap: .related, wrap: true, align: .center) {
                ui.Badge("Beta")
                ui.Badge(count: 12)
                ui.Badge(count: 128)
                ui.Badge("New", tone: .info)
                ui.Badge("Due soon", tone: .warning)
                ui.Badge(dot: true)
                ui.Badge("Done", tone: .success)
                ui.Badge("Late", tone: .danger)
                ui.Icon(.circle_check, tone: .success)
                ui.SignalMeter(.good)
                ui.Spinner("Syncing")
            }
            ui.StatusLine("Syncing 3 files", status: .active, who: invoker)
        }
        part(
            "Text stack and facts",
            "A thing's name over what it is and when, and its facts in a line.",
        ) {
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
        part("Progress", "How far a task is, or that it runs when nobody knows how long.") {
            ui.ProgressBar(0.62, label: "Uploading")
            ui.ProgressBar(label: "Preparing")
            progress(0.25)
            row(gap: .group, wrap: true, align: .center) {
                ui.ProgressRing(0.4, readout: "40%")
                ui.ProgressRing(0.9, tone: .warning, size: .small)
                ui.ProgressRing(1.0, tone: .success, size: .small)
            }
        }
        part(
            "Clock and countdown",
            "The time where someone else is, and how long is left until a moment or has gone since one — both read live.",
        ) {
            row(gap: .group, wrap: true, align: .center) {
                ui.Clock(zone: .some("Asia/Tokyo"), facts: true, role: .title)
                ui.Clock()
                ui.Countdown(2027-01-01T00:00Z, total: 5184000.0, label: "Until the new year")
                ui.Countdown(2026-01-01T00:00Z, label: "Since the year began", size: .small)
            }
        }
        part("Bar gauge", "A level against its limit, turning to a warning and a danger near it.") {
            ui.BarGauge(0.81, label: "Disk", readout: "81 of 100 GB", warning: 0.75, danger: 0.9)
            ui.BarGauge(0.32, label: "Memory", readout: "5 of 16 GB", warning: 0.75, danger: 0.9)
        }
        part("Stat tile and sparkline", "A number and how it moves, at a glance.") {
            ui.StatGroup {
                ui.StatTile("Open", "24", change: "+3", trend: .up)
                ui.StatTile("Closed", "112", unit: "this month", samples: [3.0, 5.0, 4.0, 8.0, 6.0])
                ui.StatTile("Late", "2", tone: .danger)
                ui.StatTile("Failed", "3", change: "2 fewer", trend: .down, tone: .success)
                ui.StatTile("Latency", "48", unit: "ms", change: "No change", trend: .flat)
            }
            row(gap: .related) {
                ui.Sparkline(
                    [3.0, 5.0, 4.0, 8.0, 6.0, 9.0],
                    label: "Requests per second",
                    tone: .success,
                    unit: .some("/s"),
                )
                ui.Sparkline(
                    [9.0, 6.0, 8.0, 4.0, 5.0, 3.0],
                    label: "Errors per minute",
                    smooth: true,
                    filled: false,
                    unit: .some("/min"),
                )
            }
        }
        part("Drawing", "Shapes and lines drawn from values, in the theme's inks.") {
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
        part("Disclosure", "Rows that open in place to show what is under them.") {
            ui.DisclosureRow("Build log", secondary: "Finished in 4 minutes", icon: .list) {
                ui.Text("Compiled 312 crates.")
                ui.Text("All tests passed.", role: .caption, secondary: true)
            }
            ui.DisclosureBand {
                ui.Text("Older activity is kept for 30 days.", role: .caption, secondary: true)
            }
        }
        part("Keys", "The keys that do something here, as a person presses them.") {
            ui.InputHint(["Ctrl", "K"])
            ui.KeyHints {
                ui.KeyHint(["Enter"], "Open")
                ui.KeyHint(["Esc"], "Close")
            }
        }
        part("Skeleton", "The shape of what is still loading, so nothing jumps when it arrives.") {
            ui.Skeleton(.row, leading: .avatar)
            ui.Skeleton(.text, lines: 3)
            row(gap: .related) {
                ui.Skeleton(.media, aspect: .wide)
                ui.Skeleton(.chip)
            }
        }
    }
}

/// The feedback group, the person looking syncing the files.
scenario "feedback" {
    root Gallery
}
