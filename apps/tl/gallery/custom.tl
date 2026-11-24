/// The custom page: a component built at each step, each live.
view custom(g: Gallery) {
    state used: Float = 0.62
    state requests: List<Float> = [12.0, 18.0, 15.0, 22.0, 19.0, 27.0, 24.0, 31.0, 28.0, 36.0]
    state hovered: Option<Int> = .none
    state last: Text = "Nobody messaged yet"
    ui.Text(
        "Views an app fills like components, shapes drawn with the theme's inks, and live charts.",
        role: .caption,
        secondary: true,
    )
    stack(max: 64rem, gap: .section) {
        ui.Section("A view of its own") {
            ui.Text(
                "A view the caller fills like a component: named options with defaults, a slot of its own content, and an event it answers.",
                role: .caption,
                secondary: true,
            )
            row(gap: .section, wrap: true, align: .stretch) {
                person_card("Ada Lovelace", "Leads the release", online: true) {
                    ui.Chip("Design", icon: .tag)
                    on message: set(last, "You messaged Ada")
                }
                person_card("Grace Hopper", "Reviews the compiler") {
                    ui.Chip("Compiler", icon: .code)
                    on message: set(last, "You messaged Grace")
                }
            }
            ui.Text(last, role: .caption, secondary: true)
        }
        ui.Section("How a change plays") {
            ui.Text(
                "A new reading glides to its place at the pace the view names, and lands at once for someone who asked for less motion.",
                role: .caption,
                secondary: true,
            )
            trend_card("Requests a second", requests) {
                ui.Badge("Live", tone: .success)
            }
            ui.Button("New reading", icon: .refresh) {
                on press: set(requests, next_reading(requests))
            }
        }
        ui.Section("Drawn with the theme") {
            ui.Text(
                "Shapes and text a view draws itself, in the theme's inks and type roles, saying what they mean to assistive technology.",
                role: .caption,
                secondary: true,
            )
            row(gap: .section, wrap: true, align: .center) {
                usage_ring(used, "Disk", "of 256 GB")
                stack(gap: .related) {
                    ui.Button("Fill", icon: .plus) {
                        on press: set(used, (used + 0.17).min(1.0))
                    }
                    ui.Button("Free", icon: .minus) {
                        on press: set(used, (used - 0.23).max(0.05))
                    }
                }
                ui.Drawing(
                    week_bars([3.0, 5.0, 4.0, 7.0, 6.0, 2.0, 1.5], hovered),
                    meaning: .chart,
                    label: .some("Hours worked each day this week"),
                    width: .some(224.0),
                    height: .some(132.0),
                    motion: .brisk,
                ) {
                    on hover(at): set(hovered, at)
                }
            }
        }
        ui.Section("A page you arrange") {
            ui.Text(
                "A page of a fixed size, the places on it standing in the page's own coordinates: each a view the app puts there, moved and sized by hand — and a second page standing empty.",
                role: .caption,
                secondary: true,
            )
            layers(aspect: 16.0 / 9.0) {
                ui.Stage(name: "A slide's page", canvas: [1280, 720], still: true) {
                    ui.StagePin(
                        name: "The title",
                        x: 96,
                        y: 88,
                        width: 640,
                        height: 180,
                        moves: true,
                    ) {
                        ui.Text("Release planning", role: .display)
                    }
                    ui.StagePin(
                        name: "A note",
                        x: 96,
                        y: 320,
                        width: 520,
                        height: 120,
                        moves: true,
                    ) {
                        ui.Text(
                            "A place a person drags to another point of the page.",
                            role: .caption,
                        )
                    }
                }
            }
            layers(aspect: 16.0 / 9.0) {
                ui.Stage(name: "A page with nothing on it", canvas: [1280, 720], still: true)
            }
        }
        ui.Section("Charts and graphs") {
            ui.Chart(
                [
                    .series(values: requests, label: "Requests"),
                    .series(
                        values: [6.0, 8.0, 7.0, 9.0, 12.0, 10.0, 13.0, 15.0, 14.0, 17.0],
                        label: "Errors",
                        ink: .warning,
                    ),
                ],
                title: .some("The last ten minutes"),
                labels: [
                    "9 minutes ago",
                    "8 minutes ago",
                    "7 minutes ago",
                    "6 minutes ago",
                    "5 minutes ago",
                    "4 minutes ago",
                    "3 minutes ago",
                    "2 minutes ago",
                    "A minute ago",
                    "Now",
                ],
                unit: .some("/s"),
                area: true,
                max: .some(48.0),
                top_label: .some("48"),
                bottom_label: .some("0"),
            )
            ui.BarChart(
                [
                    .bars(values: [4.0, 6.0, 5.0, 8.0], label: "Closed"),
                    .bars(values: [2.0, 1.0, 3.0, 1.0], label: "Opened"),
                ],
                categories: ["Week 1", "Week 2", "Week 3", "Week 4"],
                title: .some("Issues each week"),
                layout: .stacked,
            )
            ui.PieChart(
                [
                    .slice(label: "Design", value: 38.0),
                    .slice(label: "Engineering", value: 31.0),
                    .slice(label: "Research", value: 19.0),
                ],
                title: .some("Hours by team"),
                ring: true,
                unit: .some("h"),
                total: .some("88 hours"),
            )
            ui.FunnelChart(
                [
                    .stage(label: "Visited", value: 1200.0),
                    .stage(label: "Signed up", value: 340.0),
                    .stage(label: "Invited a team", value: 96.0),
                    .stage(label: "Paid", value: 41.0),
                ],
                title: .some("Sign-ups"),
            )
            ui.PolarAreaChart(
                [
                    .area(label: "Jan", value: 12.0),
                    .area(label: "Feb", value: 9.0),
                    .area(label: "Mar", value: 15.0),
                    .area(label: "Apr", value: 7.0),
                    .area(label: "May", value: 11.0),
                    .area(label: "Jun", value: 14.0),
                ],
                title: .some("Sign-ups by month"),
            )
            ui.RadarChart(
                [
                    .axis(label: "Speed", max: 100.0),
                    .axis(label: "Cost", max: 40.0),
                    .axis(label: "Quality", max: 100.0),
                    .axis(label: "Reach", max: 100.0),
                    .axis(label: "Care", max: 100.0),
                ],
                series: [
                    .series(values: [72.0, 18.0, 88.0, 64.0, 79.0], label: "This week"),
                    .series(values: [58.0, 27.0, 74.0, 51.0, 66.0], label: "Last week"),
                ],
                title: .some("Service health"),
            )
            ui.DistributionChart(
                [
                    .samples(values: [41.0, 44.0, 42.0, 47.0, 45.0, 43.0], label: "Before"),
                    .samples(values: [33.0, 36.0, 31.0, 38.0, 35.0, 34.0], label: "After"),
                ],
                title: .some("Build times"),
                kind: .violin,
            )
            ui.GraphCanvas(
                title: .some("Release pipeline"),
                connections: [
                    .connection(from: "fetch", to: "build"),
                    .connection(from: "build", to: "test"),
                    .connection(from: "test", to: "ship"),
                ],
            ) {
                ui.GraphNode("fetch", x: .some(0.0), y: .some(0.0), width: .some(130.0)) {
                    stage("Fetch", "Fetched", done: true)
                }
                ui.GraphNode("build", x: .some(150.0), y: .some(36.0), width: .some(130.0)) {
                    stage("Build", "Built", done: true)
                }
                ui.GraphNode("test", x: .some(0.0), y: .some(124.0), width: .some(130.0)) {
                    stage("Test", "Passed", done: true)
                }
                ui.GraphNode("ship", x: .some(150.0), y: .some(160.0), width: .some(130.0)) {
                    stage("Ship", "Waiting")
                }
            }
        }
    }
}

/// A person as a card: their face, name and what they do, what the caller adds under them, and a
/// button to message them, which the caller answers.
view person_card(
    name: Text,
    role: Text,
    online: Bool = false,
    extra: View,
    on_message: Option<Action> = .none,
) {
    ui.Card(title: name) {
        row(gap: .related, align: .center) {
            ui.Avatar(name: name)
            stack(gap: .tight, fill) {
                ui.Text(role, role: .caption, secondary: true)
                if online {
                    ui.Badge("Online", tone: .success)
                }
            }
        }
        extra
        if let message = on_message {
            ui.Button("Message", icon: .send) {
                on press: message
            }
        }
    }
}

/// A card with the newest reading as its figure and the readings as a line under it, which moves
/// to each new reading.
view trend_card(title: Text, readings: List<Float>, badge: View) {
    ui.Card(title: title) {
        row(gap: .section, wrap: true, align: .center) {
            stack(gap: .tight) {
                ui.Text(format_reading(readings), role: .large_title, numeric: true)
                badge
            }
            ui.Drawing(
                trend(readings),
                meaning: .chart,
                label: .some(title),
                width: .some(256.0),
                height: .some(72.0),
                motion: .calm,
            )
        }
    }
}

/// How full something is, as a ring with its figure inside.
view usage_ring(used: Float, label: Text, caption: Text) {
    ui.Drawing(
        [
            .arc(
                cx: 0.5,
                cy: 0.5,
                radius: 0.42,
                from: 0.0,
                to: 1.0,
                stroke: .track,
                weight: .strong,
            ),
            .arc(
                cx: 0.5,
                cy: 0.5,
                radius: 0.42,
                from: 0.0,
                to: used,
                stroke: if used > 0.85 { .danger } else { .accent },
                weight: .strong,
            ),
            .text(x: 0.5, y: 0.45, text: percent(used), role: .title, strong: true),
            .text(x: 0.5, y: 0.62, text: caption, role: .caption, ink: .muted),
        ],
        meaning: .progress,
        value: .some(used),
        label: .some(label),
        height: .some(132.0),
        motion: .calm,
    )
}

/// One stage of a pipeline: its name and how it stands.
view stage(name: Text, detail: Text, done: Bool = false) {
    ui.Card(title: name) {
        ui.Badge(detail, tone: if done { .success } else { .neutral })
    }
}

/// The fraction as a whole percent: `0.62` is "62%".
fn percent(x: Float) -> Text {
    let whole = (x * 100.0).round()
    "{whole}%"
}

/// The newest reading, as a card's figure says it.
fn format_reading(xs: List<Float>) -> Text {
    let last = (xs.last() ?? 0.0).round()
    "{last}"
}

/// The readings a minute on: the oldest gone, a new one at the end.
fn next_reading(xs: List<Float>) -> List<Float> {
    let last = xs.last() ?? 20.0
    let step = if last > 30.0 { -11.0 } else { 7.0 }
    let rest = xs.flat_map((x, i) -> if i == 0 { xs.take(0) } else { [x] })
    [rest, [(last + step).clamp(4.0, 44.0)]].flat_map(p -> p)
}

/// The highest of the readings, a little over so a line never touches the top.
fn top_of(xs: List<Float>) -> Float {
    ((xs.sort_by(x -> x).last() ?? 1.0) * 1.15).max(1.0)
}

/// The readings as a line, x and y in turn, inset so the line's ends stay whole.
fn line_points(xs: List<Float>) -> List<Float> {
    let top = top_of(xs)
    let gaps = (xs.len() - 1).max(1).to_float()
    xs.flat_map((x, i) -> [0.03 + 0.94 * i.to_float() / gaps, 0.92 - 0.8 * x / top])
}

/// A trend as the theme draws one: the line with the area under it washed out downward, and a dot
/// on the newest reading.
fn trend(xs: List<Float>) -> List<ui.Shape> {
    let points = line_points(xs)
    let newest = 0.92 - 0.8 * (xs.last() ?? 0.0) / top_of(xs)
    [
        ui.Shape.path(
            points: points,
            smooth: true,
            fill: .some(.accent),
            stroke: .some(.accent),
            wash: true,
        ),
        ui.Shape.ellipse(cx: 0.97, cy: newest, rx: 0.013, ry: 0.046, fill: .some(.accent)),
    ]
}

/// A week as bars: each day's hours, the day under the pointer (or the latest) in the accent with
/// its figure over it, and each day's initial under its bar.
fn week_bars(xs: List<Float>, hovered: Option<Int>) -> List<ui.Shape> {
    let top = top_of(xs)
    let width = 1.0 / xs.len().max(1).to_float()
    let picked = hovered ?? xs.len() - 1
    let bars = xs.map(
        (x, i) -> ui.Shape.rect(
            x: i.to_float() * width + width * 0.2,
            y: 0.8 - 0.6 * x / top,
            w: width * 0.6,
            h: 0.6 * x / top,
            round: 0.35,
            fill: .some(if i == picked { ui.Ink.accent } else { ui.Ink.track }),
        ),
    )
    let days = ["M", "T", "W", "T", "F", "S", "S"]
        .map(
            (d, i) -> ui.Shape.text(
                x: (i.to_float() + 0.5) * width,
                y: 0.92,
                text: d,
                role: .caption,
                ink: if i == picked { ui.Ink.ink } else { ui.Ink.muted },
            ),
        )
    let figure = xs.flat_map(
        (x, i) -> [
            ui.Shape.text(
                x: (i.to_float() + 0.5) * width,
                y: 0.7 - 0.6 * x / top,
                text: "{x} h",
                role: .caption,
                strong: true,
            ),
        ]
            .take(if i == picked { 1 } else { 0 }),
    )
    [bars, days, figure].flat_map(s -> s)
}

/// The custom components, each at a reading of its own.
scenario "custom" {
    root Gallery
}
