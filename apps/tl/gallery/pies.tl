/// A pie in each of its looks: whole, as a ring with its total, with long names, and with nothing
/// to divide yet.
view pies() {
    stack(max: 64rem, gap: .section) {
        part("Pie chart", "The parts of one whole: how a total divides.") {
            row(gap: .section, wrap: true, align: .start) {
                ui.PieChart(
                    [
                        .slice(label: "Chrome", value: 61.0),
                        .slice(label: "Safari", value: 21.0),
                        .slice(label: "Firefox", value: 11.0),
                        .slice(label: "Other", value: 7.0),
                    ],
                    title: .some("Visits by browser"),
                    unit: .some("%"),
                )
                ui.PieChart(
                    [
                        .slice(label: "Photos", value: 112.0),
                        .slice(label: "Videos", value: 64.0),
                        .slice(label: "Documents", value: 21.0),
                        .slice(label: "Apps", value: 18.0),
                    ],
                    title: .some("Disk use"),
                    ring: true,
                    unit: .some("GB"),
                    total: .some("215 GB"),
                )
            }
        }
        part(
            "Pie chart, long names",
            "Names longer than the legend's column wrap or end in an ellipsis.",
        ) {
            ui.PieChart(
                [
                    .slice(label: "Customer support and onboarding calls", value: 34.0),
                    .slice(label: "Engineering, including the release train", value: 41.0),
                    .slice(label: "Research interviews with design partners", value: 25.0),
                ],
                title: .some("Hours this month by the kind of work they went to"),
                unit: .some("h"),
            )
        }
        part("Pie chart, empty", "Nothing to divide yet: the chart says so.") {
            ui.PieChart([], title: .some("Hours by team"), ring: true)
        }
    }
}
