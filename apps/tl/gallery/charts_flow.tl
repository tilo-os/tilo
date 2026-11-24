/// The flow charts' page: a whole and its parts as rings and as rectangles, and how much moves
/// between stages or between the parts of a team.
view flow_charts(g: Gallery) {
    ui.Text(
        "A whole and its parts as rings or rectangles, and how much moves between stages or between the parts of a team.",
        role: .caption,
        secondary: true,
    )
    stack(max: 64rem, gap: .section) {
        part(
            "Sunburst",
            "A whole and its parts, each level a ring around the level it belongs to.",
        ) {
            ui.SunburstChart(
                [
                    .part(id: "build", parent: "pipeline", label: "Build", value: 18.0),
                    .part(id: "test", parent: "pipeline", label: "Test", value: 26.0),
                    .part(id: "pipeline", parent: "", label: "Release pipeline", value: 0.0),
                    .part(id: "reviews", parent: "review", label: "Code review", value: 14.0),
                    .part(id: "design", parent: "review", label: "Design review", value: 6.0),
                    .part(id: "review", parent: "", label: "Review", value: 0.0),
                    .part(id: "notes", parent: "docs", label: "Release notes", value: 9.0),
                    .part(id: "docs", parent: "", label: "Docs", value: 0.0),
                ],
                title: .some("Where the week went"),
                total: .some("73 hours"),
            )
            ui.SunburstChart(
                [
                    .part(
                        id: "integration",
                        parent: "platform",
                        label: "Integration tests for the desktop clients",
                        value: 22.0,
                    ),
                    .part(
                        id: "migration",
                        parent: "platform",
                        label: "Migration of the old calendar records",
                        value: 14.0,
                    ),
                    .part(
                        id: "platform",
                        parent: "",
                        label: "Platform work that ran long",
                        value: 0.0,
                    ),
                    .part(
                        id: "interviews",
                        parent: "people",
                        label: "Interviews with the support team",
                        value: 7.0,
                    ),
                    .part(
                        id: "people",
                        parent: "",
                        label: "Talking to people about the release",
                        value: 0.0,
                    ),
                ],
                title: .some("Where the long names went"),
            )
            ui.SunburstChart([], title: .some("No hours recorded yet"))
        }
        part(
            "Treemap",
            "A whole and its parts as rectangles, each as large as its share, where labels have to be read.",
        ) {
            ui.Treemap(
                [
                    .part(id: "images", parent: "", label: "Images", value: 184.0),
                    .part(id: "packages", parent: "", label: "Packages", value: 97.0),
                    .part(id: "kernel", parent: "", label: "Kernel", value: 62.0),
                    .part(id: "logs", parent: "", label: "Logs", value: 21.0),
                    .part(id: "caches", parent: "", label: "Build caches", value: 13.0),
                ],
                title: .some("Disk used by the build server"),
            )
            ui.Treemap(
                [
                    .part(
                        id: "notifications",
                        parent: "",
                        label: "Notification service for the design team",
                        value: 46.0,
                    ),
                    .part(
                        id: "calendar",
                        parent: "",
                        label: "Calendar sync across a person's devices",
                        value: 31.0,
                    ),
                    .part(
                        id: "search",
                        parent: "",
                        label: "Search over everything the workspace holds",
                        value: 18.0,
                    ),
                ],
                title: .some("Where the quarter went"),
            )
            ui.Treemap([], title: .some("Nothing stored yet"))
        }
        part(
            "Sankey",
            "How much moves between stages: each node as tall as what passes through it, each link as thick as its share.",
        ) {
            ui.SankeyChart(
                [
                    .node(id: "visits", label: "Visits"),
                    .node(id: "signup", label: "Sign-ups"),
                    .node(id: "trial", label: "Trials"),
                    .node(id: "paid", label: "Paid"),
                    .node(id: "churn", label: "Churned"),
                ],
                links: [
                    .link(from: "visits", to: "signup", value: 72.0),
                    .link(from: "signup", to: "trial", value: 54.0),
                    .link(from: "trial", to: "paid", value: 31.0),
                    .link(from: "trial", to: "churn", value: 23.0),
                ],
                title: .some("Sign-ups last week"),
            )
            ui.SankeyChart(
                [
                    .node(id: "docs", label: "Visited the documentation from the release notes"),
                    .node(id: "pricing", label: "Read the pricing page for the hosted workspace"),
                    .node(id: "trial", label: "Started a trial on their own computer"),
                    .node(id: "team", label: "Invited the rest of the team to the workspace"),
                ],
                links: [
                    .link(from: "docs", to: "pricing", value: 48.0),
                    .link(from: "pricing", to: "trial", value: 26.0),
                    .link(from: "trial", to: "team", value: 11.0),
                ],
                title: .some("From the docs to a team"),
            )
            ui.SankeyChart([], links: [], title: .some("No visits yet"))
        }
        part(
            "Chord",
            "How much moves between the parts of a team or a system, as ribbons between arcs around a ring.",
        ) {
            ui.ChordChart(
                [
                    .node(id: "design", label: "Design"),
                    .node(id: "engineering", label: "Engineering"),
                    .node(id: "research", label: "Research"),
                    .node(id: "support", label: "Support"),
                    .node(id: "sales", label: "Sales"),
                ],
                links: [
                    .link(from: "design", to: "engineering", value: 32.0),
                    .link(from: "design", to: "research", value: 14.0),
                    .link(from: "engineering", to: "research", value: 25.0),
                    .link(from: "engineering", to: "support", value: 18.0),
                    .link(from: "support", to: "sales", value: 27.0),
                    .link(from: "sales", to: "design", value: 21.0),
                ],
                title: .some("Hand-overs between teams"),
            )
            ui.ChordChart(
                [
                    .node(id: "ingest", label: "Ingest the raw readings from the field"),
                    .node(id: "clean", label: "Clean and join them with the customer list"),
                    .node(id: "model", label: "Model the weekly totals for the report"),
                    .node(id: "review", label: "Review the numbers before they go out"),
                ],
                links: [
                    .link(from: "ingest", to: "clean", value: 44.0),
                    .link(from: "clean", to: "model", value: 37.0),
                    .link(from: "model", to: "review", value: 29.0),
                    .link(from: "review", to: "ingest", value: 6.0),
                ],
                title: .some("The weekly report's path"),
            )
            ui.ChordChart([], links: [], title: .some("Nobody hands over work yet"))
        }
    }
}

/// The flow charts, as the gallery opens them.
scenario "flow" {
    root Gallery
}
