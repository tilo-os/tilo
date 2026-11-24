/// The charts that read a shape across values: how readings spread, a cycle's magnitudes, several
/// measures at once, the steps of a sequence, and a value across two axes. Each in its variants,
/// with long labels, and with nothing to show.
view shape_charts(g: Gallery) {
    ui.Text(
        "Spreads, cycles, measures around a circle, the steps of a sequence and a matrix of values.",
        role: .caption,
        secondary: true,
    )
    stack(max: 64rem, gap: .section) {
        part(
            "Distribution chart",
            "Where readings centre and how far they run, calculated from the samples themselves: a box, a violin, or both.",
        ) {
            ui.DistributionChart(
                [
                    .samples(
                        values: [
                            42.0,
                            45.0,
                            39.0,
                            51.0,
                            48.0,
                            44.0,
                            47.0,
                            53.0,
                            46.0,
                            41.0,
                            49.0,
                            44.0,
                        ],
                        label: "Feature build",
                    ),
                    .samples(
                        values: [18.0, 21.0, 19.0, 24.0, 20.0, 22.0, 17.0, 26.0, 23.0, 19.0],
                        label: "Docs build",
                    ),
                ],
                title: .some("Build times this week, in minutes"),
                kind: .box,
            )
            ui.DistributionChart(
                [
                    .samples(
                        values: [
                            12.0,
                            14.0,
                            15.0,
                            13.0,
                            18.0,
                            16.0,
                            15.0,
                            14.0,
                            19.0,
                            22.0,
                            15.0,
                            13.0,
                        ],
                        label: "Cached requests",
                    ),
                    .samples(
                        values: [
                            48.0,
                            52.0,
                            61.0,
                            57.0,
                            66.0,
                            54.0,
                            58.0,
                            72.0,
                            60.0,
                            55.0,
                            63.0,
                        ],
                        label: "Uncached requests",
                    ),
                ],
                title: .some("Request duration, in milliseconds"),
                kind: .violin,
            )
            ui.DistributionChart(
                [
                    .samples(
                        values: [
                            64.0,
                            71.0,
                            58.0,
                            82.0,
                            69.0,
                            75.0,
                            90.0,
                            66.0,
                            73.0,
                            61.0,
                            78.0,
                        ],
                        label: "Full image build after a clean checkout",
                    ),
                    .samples(
                        values: [22.0, 25.0, 19.0, 28.0, 24.0, 21.0, 31.0, 26.0, 23.0],
                        label: "Incremental docs build on the build server",
                    ),
                ],
                title: .some("The same week, from a cold cache"),
                kind: .both,
            )
            ui.DistributionChart([], title: .some("No builds were recorded"))
        }
        part(
            "Polar area chart",
            "A cycle's magnitudes around a circle: every direction takes the same angle and its length carries the value.",
        ) {
            ui.PolarAreaChart(
                [
                    .area(label: "Jan", value: 120.0),
                    .area(label: "Feb", value: 98.0),
                    .area(label: "Mar", value: 145.0),
                    .area(label: "Apr", value: 162.0),
                    .area(label: "May", value: 138.0),
                    .area(label: "Jun", value: 174.0),
                    .area(label: "Jul", value: 151.0),
                    .area(label: "Aug", value: 119.0),
                    .area(label: "Sep", value: 203.0),
                    .area(label: "Oct", value: 221.0),
                    .area(label: "Nov", value: 188.0),
                    .area(label: "Dec", value: 146.0),
                ],
                title: .some("New sign-ups by month"),
            )
            ui.PolarAreaChart(
                [
                    .area(label: "The week after the launch", value: 214.0),
                    .area(label: "The week of the conference", value: 176.0),
                    .area(label: "The week the pricing changed for every plan", value: 132.0),
                    .area(label: "The week of the outage", value: 61.0),
                    .area(label: "The week before the holidays", value: 149.0),
                    .area(label: "The week after the newsletter", value: 195.0),
                ],
                title: .some("A quarter, week by week"),
            )
            ui.PolarAreaChart([], title: .some("No sign-ups in this period"))
        }
        part(
            "Radar chart",
            "Several measures of the same things on axes around a circle, each axis with its own scale.",
        ) {
            ui.RadarChart(
                [
                    .axis(label: "Speed", max: 100.0),
                    .axis(label: "Reliability", max: 100.0),
                    .axis(label: "Support", max: 100.0),
                    .axis(label: "Coverage", max: 100.0),
                    .axis(label: "Cost", max: 30.0),
                ],
                series: [
                    .series(values: [72.0, 94.0, 81.0, 63.0, 12.0], label: "This week"),
                    .series(
                        values: [65.0, 88.0, 74.0, 58.0, 16.0],
                        label: "The week before the release",
                    ),
                ],
                title: .some("Service health"),
            )
            ui.RadarChart(
                [
                    .axis(label: "Time to the first useful answer", max: 100.0),
                    .axis(label: "P95 request latency at peak load", max: 100.0),
                    .axis(label: "Support tickets closed per day", max: 100.0),
                    .axis(label: "Documentation pages updated", max: 100.0),
                ],
                series: [
                    .series(values: [68.0, 74.0, 52.0, 41.0], label: "After the caching change"),
                ],
                title: .some("The support team's measures"),
            )
            ui.RadarChart([], series: [], title: .some("No measures recorded"))
        }
        part(
            "Funnel chart",
            "How many reach each stage of a sequence, narrowing as fewer arrive; or widening as a pyramid.",
        ) {
            ui.FunnelChart(
                [
                    .stage(label: "Visited the page", value: 1200.0),
                    .stage(label: "Signed up", value: 340.0),
                    .stage(label: "Invited a teammate", value: 96.0),
                    .stage(label: "Created a workspace", value: 61.0),
                ],
                title: .some("Sign-up funnel"),
            )
            ui.FunnelChart(
                [
                    .stage(label: "Visited the page", value: 1200.0),
                    .stage(label: "Signed up", value: 340.0),
                    .stage(label: "Invited a teammate", value: 96.0),
                    .stage(label: "Created a workspace", value: 61.0),
                ],
                title: .some("The same stages as a pyramid"),
                direction: .pyramid,
            )
            ui.FunnelChart(
                [
                    .stage(label: "Opened the product page from a shared link", value: 860.0),
                    .stage(label: "Read past the first screen of the tour", value: 412.0),
                    .stage(label: "Started an import of an existing workspace", value: 148.0),
                    .stage(label: "Finished the import and invited the team", value: 73.0),
                ],
                title: .some("The self-serve path, in detail"),
            )
            ui.FunnelChart([], title: .some("No visitors yet"))
        }
        part(
            "Heatmap",
            "How a value varies across two axes at once, each cell coloured between the lowest and the highest.",
        ) {
            ui.Heatmap(
                [
                    .cell(row: "Monday", column: "6h", value: 4.0),
                    .cell(row: "Monday", column: "9h", value: 18.0),
                    .cell(row: "Monday", column: "12h", value: 26.0),
                    .cell(row: "Monday", column: "15h", value: 31.0),
                    .cell(row: "Monday", column: "18h", value: 12.0),
                    .cell(row: "Tuesday", column: "6h", value: 3.0),
                    .cell(row: "Tuesday", column: "9h", value: 22.0),
                    .cell(row: "Tuesday", column: "12h", value: 34.0),
                    .cell(row: "Tuesday", column: "15h", value: 38.0),
                    .cell(row: "Tuesday", column: "18h", value: 15.0),
                    .cell(row: "Wednesday", column: "6h", value: 5.0),
                    .cell(row: "Wednesday", column: "9h", value: 25.0),
                    .cell(row: "Wednesday", column: "12h", value: 41.0),
                    .cell(row: "Wednesday", column: "15h", value: 36.0),
                    .cell(row: "Wednesday", column: "18h", value: 19.0),
                    .cell(row: "Thursday", column: "6h", value: 2.0),
                    .cell(row: "Thursday", column: "9h", value: 19.0),
                    .cell(row: "Thursday", column: "12h", value: 29.0),
                    .cell(row: "Thursday", column: "15h", value: 44.0),
                    .cell(row: "Thursday", column: "18h", value: 23.0),
                    .cell(row: "Friday", column: "6h", value: 6.0),
                    .cell(row: "Friday", column: "9h", value: 17.0),
                    .cell(row: "Friday", column: "12h", value: 24.0),
                    .cell(row: "Friday", column: "15h", value: 28.0),
                    .cell(row: "Friday", column: "18h", value: 9.0),
                ],
                rows: ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"],
                columns: ["6h", "9h", "12h", "15h", "18h"],
                title: .some("Messages by hour"),
            )
            ui.Heatmap(
                [
                    .cell(
                        row: "Saturday, the long debugging session",
                        column: "Just after lunch",
                        value: 47.0,
                    ),
                    .cell(
                        row: "Saturday, the long debugging session",
                        column: "Late in the evening",
                        value: 12.0,
                    ),
                    .cell(
                        row: "Sunday, the quiet release morning",
                        column: "Just after lunch",
                        value: 8.0,
                    ),
                    .cell(
                        row: "Sunday, the quiet release morning",
                        column: "Late in the evening",
                        value: 21.0,
                    ),
                ],
                rows: ["Saturday, the long debugging session", "Sunday, the quiet release morning"],
                columns: ["Just after lunch", "Late in the evening"],
                title: .some("One weekend at a glance"),
            )
            ui.Heatmap(
                [],
                rows: ["Monday", "Tuesday"],
                columns: ["9h", "12h"],
                title: .some("No activity recorded"),
            )
        }
    }
}
