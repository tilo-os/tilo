/// The series page: readings on numeric axes, distributions, prices over time, spans and a
/// waterfall. Each is placed with the values an app would give it, and each is also shown empty,
/// as it stands before its first reading.
view series_charts(g: Gallery) {
    ui.Text(
        "Readings on numeric axes, distributions of raw samples, prices per day, spans in time and a total built up step by step.",
        role: .caption,
        secondary: true,
    )
    stack(max: 64rem, gap: .section) {
        part(
            "Cartesian plot",
            "Readings on numeric axes: a line, an area, steps, dots, bubbles, bars or stems, each series in its own colour.",
        ) {
            ui.CartesianPlot(
                [
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
                        y: [120.0, 138.0, 129.0, 96.0, 84.0, 101.0, 92.0, 78.0],
                        label: "Requests per second",
                    ),
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
                        y: [6.0, 8.0, 7.0, 12.0, 9.0, 7.0, 5.0, 4.0],
                        label: "Errors per second",
                        kind: .smooth,
                    ),
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
                        y: [42.0, 46.0, 44.0, 39.0, 33.0, 36.0, 34.0, 30.0],
                        label: "Bandwidth used, in megabytes per second",
                        kind: .smooth_area,
                        marker: .diamond,
                    ),
                ],
                title: .some("The hour after the build finished"),
                x_title: .some("Minutes after the build finished"),
                y_title: .some("Rate per second"),
            )
            ui.CartesianPlot(
                [
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                        y: [14.0, 16.0, 16.0, 21.0, 19.0, 24.0],
                        label: "Cached",
                        kind: .step,
                    ),
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                        y: [8.0, 9.0, 11.0, 10.0, 13.0, 12.0],
                        label: "Fetched from the origin",
                        kind: .step_area,
                    ),
                ],
                title: .some("Hits each hour"),
                x_title: .some("Hour of the night"),
                y_title: .some("Hits"),
            )
            ui.CartesianPlot(
                [
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0],
                        y: [31.0, 44.0, 27.0, 52.0, 39.0, 61.0, 48.0],
                        label: "Samples",
                        kind: .scatter,
                        marker: .square,
                    ),
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0],
                        y: [12.0, 18.0, 15.0, 26.0, 22.0, 34.0, 29.0],
                        label: "Traffic bursts",
                        kind: .bubble,
                    ),
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0],
                        y: [4.0, 0.0, 6.0, 2.0, 0.0, 8.0, 3.0],
                        label: "Deploys",
                        kind: .bar,
                    ),
                    .points(
                        x: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0],
                        y: [2.0, 5.0, 3.0, 7.0, 4.0, 9.0, 6.0],
                        label: "Incidents",
                        kind: .lollipop,
                        marker: .diamond,
                    ),
                ],
                title: .some("A night of samples"),
                x_title: .some("Hour of the night"),
                y_title: .some("Count"),
            )
            ui.CartesianPlot(
                [],
                title: .some("Latency, before the first request"),
                x_title: .some("Minutes after midnight"),
                y_title: .some("Milliseconds"),
            )
        }
        part(
            "Histogram",
            "Raw observations binned into bars: how often each range of a measure came up.",
        ) {
            let nothing_measured: List<Float> = []
            ui.Histogram(
                [
                    88.0,
                    91.0,
                    84.0,
                    97.0,
                    103.0,
                    88.0,
                    79.0,
                    94.0,
                    112.0,
                    86.0,
                    90.0,
                    99.0,
                    105.0,
                    87.0,
                    93.0,
                    121.0,
                    82.0,
                    96.0,
                    101.0,
                    89.0,
                    94.0,
                    108.0,
                    85.0,
                    92.0,
                ],
                bins: .some(10),
                title: .some("Time to the last byte, across a busy hour"),
            )
            ui.Histogram(
                [
                    88.0,
                    91.0,
                    84.0,
                    97.0,
                    103.0,
                    88.0,
                    79.0,
                    94.0,
                    112.0,
                    86.0,
                    90.0,
                    99.0,
                    105.0,
                    87.0,
                    93.0,
                    121.0,
                    82.0,
                    96.0,
                    101.0,
                    89.0,
                    94.0,
                    108.0,
                    85.0,
                    92.0,
                ],
                bins: .some(8),
                scale: .probability,
                title: .some("The same samples, as the share of requests"),
            )
            ui.Histogram(
                [
                    88.0,
                    91.0,
                    84.0,
                    97.0,
                    103.0,
                    88.0,
                    79.0,
                    94.0,
                    112.0,
                    86.0,
                    90.0,
                    99.0,
                    105.0,
                    87.0,
                    93.0,
                    121.0,
                    82.0,
                    96.0,
                    101.0,
                    89.0,
                    94.0,
                    108.0,
                    85.0,
                    92.0,
                ],
                bins: .some(16),
                scale: .density,
                title: .some("The same samples, per millisecond"),
            )
            ui.Histogram(nothing_measured, title: .some("Latency, once the fleet has reported"))
        }
        part(
            "Candlestick chart",
            "A price for each period: its open and close as the body, its high and low as the wick, and the volume below.",
        ) {
            ui.CandlestickChart(
                [
                    .candle(
                        label: "Mon",
                        open: 82.4,
                        high: 88.1,
                        low: 80.2,
                        close: 86.9,
                        volume: 1240.0,
                    ),
                    .candle(
                        label: "Tue",
                        open: 86.9,
                        high: 91.4,
                        low: 85.0,
                        close: 90.2,
                        volume: 1580.0,
                    ),
                    .candle(
                        label: "Wed",
                        open: 90.2,
                        high: 90.8,
                        low: 84.6,
                        close: 85.3,
                        volume: 1360.0,
                    ),
                    .candle(
                        label: "Thu",
                        open: 85.3,
                        high: 89.7,
                        low: 83.9,
                        close: 89.1,
                        volume: 1120.0,
                    ),
                    .candle(
                        label: "Fri",
                        open: 89.1,
                        high: 93.5,
                        low: 88.4,
                        close: 92.8,
                        volume: 1710.0,
                    ),
                    .candle(
                        label: "Week of 12 October 2026",
                        open: 92.8,
                        high: 96.2,
                        low: 90.1,
                        close: 91.4,
                        volume: 6240.0,
                    ),
                ],
                title: .some("Closing price, last two weeks"),
                volume: true,
            )
            ui.CandlestickChart(
                [
                    .candle(label: "Q1", open: 74.0, high: 81.0, low: 71.5, close: 79.2),
                    .candle(label: "Q2", open: 79.2, high: 84.6, low: 76.8, close: 78.1),
                    .candle(label: "Q3", open: 78.1, high: 89.9, low: 77.4, close: 88.0),
                ],
                title: .some("Share price each quarter"),
            )
            ui.CandlestickChart([], title: .some("Prices, before the market opens"))
        }
        part(
            "Range chart",
            "What takes a span of something, one row each: a timeline, a plan filling to its progress, hours booked, or the moments something happened.",
        ) {
            ui.RangeChart(
                [
                    .range(label: "Design review", start: 0.0, end: 3.0),
                    .range(label: "Documentation and release notes", start: 2.0, end: 6.0),
                    .range(label: "Build and sign the release image", start: 6.0, end: 10.0),
                    .range(label: "Ship", start: 10.0, end: 12.0),
                ],
                title: .some("Release week"),
                axis: .some("Day of the sprint"),
                unit: .some("d"),
            )
            ui.RangeChart(
                [
                    .range(label: "Design review", start: 0.0, end: 3.0, progress: .some(1.0)),
                    .range(
                        label: "Documentation and release notes",
                        start: 2.0,
                        end: 6.0,
                        progress: .some(0.6),
                    ),
                    .range(
                        label: "Build and sign the release image",
                        start: 6.0,
                        end: 10.0,
                        progress: .some(0.2),
                    ),
                    .range(label: "Ship", start: 10.0, end: 12.0, progress: .some(0.0)),
                ],
                title: .some("Where the release stands"),
                kind: .plan,
                axis: .some("Day of the sprint"),
            )
            ui.RangeChart(
                [
                    .range(label: "Large studio, morning", start: 8.0, end: 12.0),
                    .range(label: "Large studio, afternoon", start: 13.0, end: 17.0),
                    .range(label: "Small studio, all day", start: 9.0, end: 18.0),
                ],
                title: .some("The rooms today"),
                kind: .intervals,
                axis: .some("Hour"),
                unit: .some("h"),
            )
            ui.RangeChart(
                [
                    .range(label: "Kickoff", start: 1.0, end: 1.0),
                    .range(label: "Design review", start: 4.0, end: 4.0),
                    .range(label: "The release goes out", start: 12.0, end: 12.0),
                ],
                title: .some("The moments that matter"),
                kind: .milestones,
                axis: .some("Day of the sprint"),
            )
            ui.RangeChart([], title: .some("What takes the week, once it is planned"))
        }
        part(
            "Waterfall bars",
            "Bars over categories, each standing on the one before it: how a total was reached, step by step.",
        ) {
            ui.BarChart(
                [.bars(values: [48.0, 21.0, -13.0, -6.0, 9.0], label: "Change")],
                categories: [
                    "Photos, videos and exports",
                    "Documents and shared notebooks",
                    "Old backups removed",
                    "Duplicates merged",
                    "New recordings",
                ],
                title: .some("How this quarter's storage grew"),
                layout: .waterfall,
                readings: true,
                unit: .some("GB"),
            )
            let no_categories: List<Text> = []
            ui.BarChart(
                [],
                categories: no_categories,
                title: .some("Storage, before the first import"),
                layout: .waterfall,
                unit: .some("GB"),
            )
        }
    }
}
